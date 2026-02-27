use std::sync::{Arc, Mutex};

use crate::async_worker::AsyncWorker;
use crate::converter::ConvertedSegment;
use crate::session::{InputSession, LearningRecord};

use super::resources::{LexConnection, LexDictionary, LexUserHistory};
use super::snippet_store::LexSnippetStore;
use super::types::{convert_to_events, LexConversionMode};
use super::{LexKeyEvent, LexKeyResponse};

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
    worker: AsyncWorker,
}

#[uniffi::export]
impl LexSession {
    #[uniffi::constructor]
    pub(super) fn new(
        dict: Arc<LexDictionary>,
        conn: Option<Arc<LexConnection>>,
        history: Option<Arc<LexUserHistory>>,
    ) -> Arc<Self> {
        let session = InputSession::new(
            Arc::clone(&dict.inner),
            conn.as_ref().map(|c| Arc::clone(&c.inner)),
            history.as_ref().map(|h| Arc::clone(&h.inner)),
        );
        let worker = AsyncWorker::new(
            Arc::clone(&dict.inner),
            conn.as_ref().map(|c| Arc::clone(&c.inner)),
            history.as_ref().map(|h| Arc::clone(&h.inner)),
        );
        Arc::new(Self {
            history,
            session: Mutex::new(session),
            worker,
        })
    }

    fn handle_key(&self, event: LexKeyEvent) -> LexKeyResponse {
        // Invalidate stale candidates from previous key events
        self.worker.invalidate_candidates();

        let mut session = self.session.lock().unwrap();
        let mut resp = session.handle_key(event.into());

        // Submit async candidate work internally
        let has_pending_work = resp.async_request.is_some();
        if let Some(req) = resp.async_request.take() {
            self.worker
                .submit_candidates(req.reading, req.candidate_dispatch);
        }

        let records = session.take_history_records();
        drop(session);
        self.record_history(&records);
        convert_to_events(resp, has_pending_work)
    }

    fn commit(&self) -> LexKeyResponse {
        let mut session = self.session.lock().unwrap();
        let resp = session.commit();
        let records = session.take_history_records();
        drop(session);
        self.record_history(&records);
        convert_to_events(resp, false)
    }

    fn poll(&self) -> Option<LexKeyResponse> {
        if let Some(result) = self.worker.try_recv_candidate() {
            let surfaces = result.response.surfaces;
            let paths: Vec<Vec<ConvertedSegment>> = result.response.paths;

            let mut session = self.session.lock().unwrap();
            if let Some(mut resp) = session.receive_candidates(&result.reading, surfaces, paths) {
                // Chain: submit any new async requests from the response
                let has_pending_work = resp.async_request.is_some();
                if let Some(req) = resp.async_request.take() {
                    self.worker
                        .submit_candidates(req.reading, req.candidate_dispatch);
                }
                let records = session.take_history_records();
                drop(session);
                self.record_history(&records);
                return Some(convert_to_events(resp, has_pending_work));
            }
        }

        None
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

    fn set_snippet_store(&self, store: Arc<LexSnippetStore>) {
        self.session
            .lock()
            .unwrap()
            .set_snippet_store(Arc::clone(&store.inner));
    }
}

impl LexSession {
    fn record_history(&self, records: &[LearningRecord]) {
        if records.is_empty() {
            return;
        }
        let Some(ref h) = self.history else {
            return;
        };

        // Phase 1: in-memory update (write lock)
        let now = crate::user_history::now_epoch();
        let mut wal_entries: Vec<Vec<(String, String)>> = Vec::new();
        let mut needs_compact = false;
        if let Ok(mut hist) = h.inner.write() {
            for r in records {
                match r {
                    LearningRecord::Committed {
                        reading,
                        surface,
                        segments,
                    } => {
                        let segs = vec![(reading.clone(), surface.clone())];
                        hist.record_at(&segs, now);
                        wal_entries.push(segs);
                        if let Some(sub_segs) = segments {
                            hist.record_at(sub_segs, now);
                            wal_entries.push(sub_segs.clone());
                        }
                    }
                    LearningRecord::Deletion { segments } => {
                        if hist.remove_entries(segments) {
                            needs_compact = true;
                        }
                    }
                }
            }
        }

        // Phase 2: WAL append (write lock released, wal mutex only)
        for segments in &wal_entries {
            h.append_wal(segments, now);
        }

        // Phase 3: persist deletion immediately, or background compaction
        if needs_compact {
            h.force_compact();
        } else {
            h.maybe_compact();
        }
    }
}
