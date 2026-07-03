use std::sync::{Arc, Mutex};

use crate::async_worker::{AsyncWorker, CandidateResult, CandidateSink};
use crate::converter::ConvertedSegment;
use crate::session::{InputSession, LearningRecord};

use super::mapping::convert_to_events;
use super::resources::{LexConnection, LexDictionary, LexUserHistory};
use super::snippet_store::LexSnippetStore;
use super::types::LexConversionMode;
use super::{LexKeyEvent, LexKeyResponse};

/// Listener for async session events delivered from the Rust worker thread.
///
/// Implementations must be `Send + Sync` (UniFFI requirement). The callback is
/// invoked on the AsyncWorker thread — foreign implementations are responsible
/// for dispatching onto their UI thread if needed.
#[uniffi::export(with_foreign)]
pub trait LexSessionEvents: Send + Sync {
    fn on_async_response(&self, response: LexKeyResponse);
}

/// Bridge from the internal `CandidateSink` trait to the foreign `LexSessionEvents`.
/// Also holds a weak reference back to the `LexSession` so results can be merged
/// into session state without creating a retain cycle with the worker thread.
struct ListenerSink {
    session: std::sync::Weak<LexSession>,
    listener: Arc<dyn LexSessionEvents>,
}

impl CandidateSink for ListenerSink {
    fn deliver(&self, result: CandidateResult) {
        let Some(session) = self.session.upgrade() else {
            return;
        };
        let Some(resp) = session.integrate_candidate_result(result) else {
            return;
        };
        let listener = Arc::clone(&self.listener);
        // Isolate foreign-code panics so the worker thread keeps running.
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            listener.on_async_response(resp);
        }))
        .is_err()
        {
            tracing::error!("foreign LexSessionEvents.on_async_response panicked");
        }
    }
}

/// IME session exposed to the Swift frontend via UniFFI.
///
/// `self.session.lock().unwrap()` is used intentionally throughout this struct.
/// If the Mutex is poisoned (a panic occurred in a prior lock holder), the
/// session state is unrecoverable. For an IME, panicking is the correct
/// response — macOS automatically restarts the input method process, so
/// the user experiences only a momentary input interruption rather than
/// a silently corrupted session.
#[derive(uniffi::Object)]
pub struct LexSession {
    history: Option<Arc<LexUserHistory>>,
    session: Mutex<InputSession>,
    worker: Mutex<Option<AsyncWorker>>,
}

#[uniffi::export]
impl LexSession {
    #[uniffi::constructor]
    pub(super) fn new(
        dict: Arc<LexDictionary>,
        conn: Option<Arc<LexConnection>>,
        history: Option<Arc<LexUserHistory>>,
        listener: Arc<dyn LexSessionEvents>,
    ) -> Arc<Self> {
        let session = InputSession::new(
            Arc::clone(&dict.inner),
            conn.as_ref().map(|c| Arc::clone(&c.inner)),
            history.as_ref().map(|h| Arc::clone(&h.inner)),
        );

        let arc = Arc::new_cyclic(|weak: &std::sync::Weak<LexSession>| {
            let sink: Arc<dyn CandidateSink> = Arc::new(ListenerSink {
                session: weak.clone(),
                listener,
            });
            let worker = AsyncWorker::new(
                Arc::clone(&dict.inner),
                conn.as_ref().map(|c| Arc::clone(&c.inner)),
                history.as_ref().map(|h| Arc::clone(&h.inner)),
                sink,
            );
            Self {
                history,
                session: Mutex::new(session),
                worker: Mutex::new(Some(worker)),
            }
        });
        arc
    }

    fn handle_key(&self, event: LexKeyEvent) -> LexKeyResponse {
        // Cancel in-flight candidate work early (computation-skip
        // optimization; correctness is owned by the session epoch, which
        // `session.handle_key` bumps under the session lock below).
        if let Some(worker) = self.worker.lock().unwrap().as_ref() {
            worker.invalidate_candidates();
        }

        let mut session = self.session.lock().unwrap();
        let mut resp = session.handle_key(event.into());

        // Submit async candidate work internally
        if let Some(req) = resp.async_request.take() {
            if let Some(worker) = self.worker.lock().unwrap().as_ref() {
                worker.submit_candidates(
                    req.reading,
                    req.candidate_dispatch,
                    req.lattice,
                    req.epoch,
                );
            }
        }

        // Stamp the response with the epoch it reflects (still under the
        // session lock) so the Swift side can drop async responses that are
        // re-ordered behind this one on the main queue.
        let epoch = session.epoch();
        let records = session.take_history_records();
        drop(session);
        self.record_history(&records);
        convert_to_events(resp, epoch)
    }

    fn commit(&self) -> LexKeyResponse {
        // Cancel in-flight candidate work, mirroring `handle_key`
        // (computation-skip optimization; `session.commit` bumps the session
        // epoch, which is what actually rejects stale responses).
        if let Some(worker) = self.worker.lock().unwrap().as_ref() {
            worker.invalidate_candidates();
        }

        let mut session = self.session.lock().unwrap();
        let resp = session.commit();
        let epoch = session.epoch();
        let records = session.take_history_records();
        drop(session);
        self.record_history(&records);
        convert_to_events(resp, epoch)
    }

    fn is_composing(&self) -> bool {
        self.session.lock().unwrap().is_composing()
    }

    fn set_defer_candidates(&self, enabled: bool) {
        self.session.lock().unwrap().set_defer_candidates(enabled);
    }

    fn set_conversion_mode(&self, mode: LexConversionMode) {
        let conversion_mode = match mode {
            LexConversionMode::Predictive => crate::session::ConversionMode::Predictive,
            LexConversionMode::Standard => crate::session::ConversionMode::Standard,
        };
        self.session
            .lock()
            .unwrap()
            .set_conversion_mode(conversion_mode);
    }

    fn set_abc_passthrough(&self, enabled: bool) {
        self.session.lock().unwrap().set_abc_passthrough(enabled);
    }

    fn set_snippet_store(&self, store: Option<Arc<LexSnippetStore>>) {
        self.session
            .lock()
            .unwrap()
            .set_snippet_store(store.map(|s| Arc::clone(&s.inner)));
    }

    /// Stop the async worker thread eagerly. Called by the Swift side on
    /// IMKInputController teardown to guarantee the worker is joined before
    /// the last Arc to `LexSession` is dropped.
    fn shutdown(&self) {
        let worker = {
            let mut slot = self.worker.lock().unwrap();
            slot.take()
        };
        drop(worker);
    }
}

impl LexSession {
    /// Merge a candidate result returned by the worker into session state and
    /// build the response that should be forwarded to the foreign listener.
    /// Returns `None` if the result was stale.
    fn integrate_candidate_result(&self, result: CandidateResult) -> Option<LexKeyResponse> {
        let surfaces = result.response.surfaces;
        let paths: Vec<Vec<ConvertedSegment>> = result.response.paths;

        let mut session = self.session.lock().unwrap();
        let mut resp =
            session.receive_candidates(result.epoch, &result.reading, surfaces, paths)?;

        // Chain: submit any new async requests from the response
        if let Some(req) = resp.async_request.take() {
            if let Some(worker) = self.worker.lock().unwrap().as_ref() {
                worker.submit_candidates(
                    req.reading,
                    req.candidate_dispatch,
                    req.lattice,
                    req.epoch,
                );
            }
        }
        // Stamp the accepted response with the post-acceptance epoch (read
        // under the same lock). A later key handler that beats this response
        // to the main queue will have applied a strictly higher epoch, so
        // the Swift side can detect and drop the re-ordered delivery.
        let epoch = session.epoch();
        let records = session.take_history_records();
        drop(session);
        self.record_history(&records);
        Some(convert_to_events(resp, epoch))
    }

    /// Persist a batch of learning records. The whole protocol — WAL-ahead
    /// append, in-memory apply, commit log, compaction scheduling — lives
    /// in [`LexUserHistory::apply_records`] (§5.2).
    fn record_history(&self, records: &[LearningRecord]) {
        if records.is_empty() {
            return;
        }
        if let Some(ref h) = self.history {
            h.apply_records(records);
        }
    }
}
