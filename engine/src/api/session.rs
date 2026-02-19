use std::sync::{Arc, Mutex};

use crate::async_worker::AsyncWorker;
use crate::converter::ConvertedSegment;
use crate::session::{InputSession, LearningRecord};

use super::resources::{LexConnection, LexDictionary, LexUserHistory};
use super::types::convert_to_events;
use super::LexKeyResponse;
use super::LexNeuralScorer;

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
        #[allow(unused_variables)] neural: Option<Arc<LexNeuralScorer>>,
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
            #[cfg(feature = "neural")]
            neural.as_ref().map(|n| Arc::clone(&n.inner)),
        );
        Arc::new(Self {
            history,
            session: Mutex::new(session),
            worker,
        })
    }

    fn handle_key(&self, key_code: u16, text: String, flags: u8) -> LexKeyResponse {
        // Invalidate stale candidates from previous key events
        self.worker.invalidate_candidates();

        let mut session = self.session.lock().unwrap();
        let had_ghost = session.has_ghost_text();
        let resp = session.handle_key(key_code, &text, flags);

        // Ghost was cleared by the key handler — invalidate pending ghost work
        if had_ghost && !session.has_ghost_text() {
            self.worker.invalidate_ghost();
        }

        // Submit async candidate work internally
        let has_async = resp.async_request.is_some();
        if let Some(ref req) = resp.async_request {
            let context = if req.candidate_dispatch == 2 {
                session.committed_context()
            } else {
                String::new()
            };
            self.worker
                .submit_candidates(req.reading.clone(), req.candidate_dispatch, context);
        }

        // Submit ghost text work internally
        let has_ghost_req = resp.ghost_request.is_some();
        if let Some(ref req) = resp.ghost_request {
            self.worker
                .submit_ghost(req.context.clone(), req.generation);
        }

        let records = session.take_history_records();
        drop(session);
        self.record_history(&records);
        convert_to_events(resp, has_async || has_ghost_req)
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
        // 1. Check for candidate results
        if let Some(result) = self.worker.try_recv_candidate() {
            let surfaces = result.response.surfaces;
            let paths: Vec<Vec<ConvertedSegment>> = result.response.paths;

            let mut session = self.session.lock().unwrap();
            if let Some(resp) = session.receive_candidates(&result.reading, surfaces, paths) {
                // Chain: submit any new async requests from the response
                let has_async = resp.async_request.is_some();
                if let Some(ref req) = resp.async_request {
                    let context = if req.candidate_dispatch == 2 {
                        session.committed_context()
                    } else {
                        String::new()
                    };
                    self.worker.submit_candidates(
                        req.reading.clone(),
                        req.candidate_dispatch,
                        context,
                    );
                }
                let has_ghost_req = resp.ghost_request.is_some();
                if let Some(ref req) = resp.ghost_request {
                    self.worker
                        .submit_ghost(req.context.clone(), req.generation);
                }
                let records = session.take_history_records();
                drop(session);
                self.record_history(&records);
                return Some(convert_to_events(resp, has_async || has_ghost_req));
            }
        }

        // 2. Check for ghost results
        if let Some(result) = self.worker.try_recv_ghost() {
            let mut session = self.session.lock().unwrap();
            if let Some(resp) = session.receive_ghost_text(result.generation, result.text) {
                return Some(convert_to_events(resp, false));
            }
        }

        None
    }

    fn is_composing(&self) -> bool {
        self.session.lock().unwrap().is_composing()
    }

    fn set_programmer_mode(&self, enabled: bool) {
        self.session.lock().unwrap().set_programmer_mode(enabled);
    }

    fn set_defer_candidates(&self, enabled: bool) {
        self.session.lock().unwrap().set_defer_candidates(enabled);
    }

    fn set_conversion_mode(&self, mode: u8) {
        let conversion_mode = match mode {
            1 => crate::session::ConversionMode::Predictive,
            2 => crate::session::ConversionMode::GhostText,
            _ => crate::session::ConversionMode::Standard,
        };
        self.session
            .lock()
            .unwrap()
            .set_conversion_mode(conversion_mode);
    }

    fn set_abc_passthrough(&self, enabled: bool) {
        self.session.lock().unwrap().set_abc_passthrough(enabled);
    }

    fn committed_context(&self) -> String {
        self.session.lock().unwrap().committed_context()
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
                }
            }
        }

        // Phase 2: WAL append (write lock released, wal mutex only)
        for segments in &wal_entries {
            h.append_wal(segments, now);
        }

        // Phase 3: background compaction if threshold reached
        h.maybe_compact();
    }
}
