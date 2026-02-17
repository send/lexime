//! UniFFI export layer — type-safe Swift bindings for the Lexime engine.
//!
//! Each public type here maps to a generated Swift class, struct, or enum.

mod engine;
pub use engine::LexEngine;

use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};

use crate::async_worker::AsyncWorker;
use crate::converter::ConvertedSegment;
use crate::dict::connection::ConnectionMatrix;
use crate::dict::{Dictionary, TrieDictionary};
use crate::romaji::{convert_romaji, RomajiTrie, TrieLookupResult};
use crate::session::{CandidateAction, InputSession, KeyResponse};
use crate::user_history::UserHistory;

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum LexError {
    #[error("IO error: {msg}")]
    Io { msg: String },
    #[error("invalid data: {msg}")]
    InvalidData { msg: String },
    #[error("internal error: {msg}")]
    Internal { msg: String },
}

// ---------------------------------------------------------------------------
// Records (value types, copied across FFI boundary)
// ---------------------------------------------------------------------------

#[derive(Clone, uniffi::Record)]
pub struct LexSegment {
    pub reading: String,
    pub surface: String,
}

#[derive(uniffi::Record)]
pub struct LexDictEntry {
    pub reading: String,
    pub surface: String,
    pub cost: i16,
}

#[derive(Clone, uniffi::Record)]
pub struct LexCandidateResult {
    pub surfaces: Vec<String>,
    pub paths: Vec<Vec<LexSegment>>,
}

#[derive(uniffi::Record)]
pub struct LexRomajiConvert {
    pub composed_kana: String,
    pub pending_romaji: String,
}

/// Event-driven response from handle_key / commit / poll.
#[derive(uniffi::Record)]
pub struct LexKeyResponse {
    pub consumed: bool,
    pub events: Vec<LexEvent>,
}

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, uniffi::Enum)]
pub enum LexEvent {
    Commit {
        text: String,
    },
    SetMarkedText {
        text: String,
        dashed: bool,
    },
    ClearMarkedText,
    ShowCandidates {
        surfaces: Vec<String>,
        selected: u32,
    },
    HideCandidates,
    SwitchToAbc,
    SaveHistory,
    SetGhostText {
        text: String,
    },
    ClearGhostText {
        update_display: bool,
    },
    SchedulePoll,
}

#[derive(uniffi::Enum)]
pub enum LexRomajiLookup {
    None,
    Prefix,
    Exact { kana: String },
    ExactAndPrefix { kana: String },
}

// ---------------------------------------------------------------------------
// Objects (Arc-wrapped, passed by reference across FFI)
// ---------------------------------------------------------------------------

#[derive(uniffi::Object)]
pub struct LexDictionary {
    pub(crate) inner: Arc<TrieDictionary>,
}

#[uniffi::export]
impl LexDictionary {
    #[uniffi::constructor]
    fn open(path: String) -> Result<Arc<Self>, LexError> {
        let dict = TrieDictionary::open(Path::new(&path))
            .map_err(|e: crate::dict::DictError| LexError::Io { msg: e.to_string() })?;
        Ok(Arc::new(Self {
            inner: Arc::new(dict),
        }))
    }

    fn lookup(&self, reading: String) -> Vec<LexDictEntry> {
        let Some(entries) = self.inner.lookup(&reading) else {
            return Vec::new();
        };
        entries
            .iter()
            .map(|e| LexDictEntry {
                reading: reading.clone(),
                surface: e.surface.clone(),
                cost: e.cost,
            })
            .collect()
    }
}

#[derive(uniffi::Object)]
pub struct LexConnection {
    pub(crate) inner: Arc<ConnectionMatrix>,
}

#[uniffi::export]
impl LexConnection {
    #[uniffi::constructor]
    fn open(path: String) -> Result<Arc<Self>, LexError> {
        let conn = ConnectionMatrix::open(Path::new(&path))
            .map_err(|e: crate::dict::DictError| LexError::Io { msg: e.to_string() })?;
        Ok(Arc::new(Self {
            inner: Arc::new(conn),
        }))
    }
}

#[derive(uniffi::Object)]
pub struct LexUserHistory {
    pub(crate) inner: Arc<RwLock<UserHistory>>,
}

#[uniffi::export]
impl LexUserHistory {
    #[uniffi::constructor]
    fn open(path: String) -> Result<Arc<Self>, LexError> {
        let history = UserHistory::open(Path::new(&path))
            .map_err(|e: std::io::Error| LexError::Io { msg: e.to_string() })?;
        Ok(Arc::new(Self {
            inner: Arc::new(RwLock::new(history)),
        }))
    }

    fn save(&self, path: String) -> Result<(), LexError> {
        let h = self
            .inner
            .read()
            .map_err(|e| LexError::Internal { msg: e.to_string() })?;
        h.save(Path::new(&path))
            .map_err(|e| LexError::Io { msg: e.to_string() })
    }
}

#[derive(uniffi::Object)]
pub struct LexSession {
    #[allow(dead_code)]
    dict: Arc<LexDictionary>,
    #[allow(dead_code)]
    conn: Option<Arc<LexConnection>>,
    history: Option<Arc<LexUserHistory>>,
    session: Mutex<InputSession>,
    worker: AsyncWorker,
}

#[uniffi::export]
impl LexSession {
    #[uniffi::constructor]
    fn new(
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
            dict,
            conn,
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

    fn committed_context(&self) -> String {
        self.session.lock().unwrap().committed_context()
    }
}

impl LexSession {
    fn record_history(&self, records: &[Vec<(String, String)>]) {
        if records.is_empty() {
            return;
        }
        if let Some(ref h) = self.history {
            if let Ok(mut hist) = h.inner.write() {
                for r in records {
                    hist.record(r);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Neural scorer (behind feature flag)
// ---------------------------------------------------------------------------

#[cfg(feature = "neural")]
mod neural_api {
    use super::*;

    #[derive(uniffi::Object)]
    pub struct LexNeuralScorer {
        pub(crate) inner: Arc<Mutex<crate::neural::NeuralScorer>>,
    }

    #[uniffi::export]
    impl LexNeuralScorer {
        #[uniffi::constructor]
        fn open(model_path: String) -> Result<Arc<Self>, LexError> {
            let scorer = crate::neural::NeuralScorer::open(Path::new(&model_path))
                .map_err(|e| LexError::Io { msg: e.to_string() })?;
            Ok(Arc::new(Self {
                inner: Arc::new(Mutex::new(scorer)),
            }))
        }
    }
}

#[cfg(feature = "neural")]
pub use neural_api::*;

#[cfg(not(feature = "neural"))]
mod neural_stub {
    use super::*;

    #[derive(uniffi::Object)]
    pub struct LexNeuralScorer;

    #[uniffi::export]
    impl LexNeuralScorer {
        #[uniffi::constructor]
        fn open(model_path: String) -> Result<Arc<Self>, LexError> {
            Err(LexError::Internal {
                msg: format!(
                    "neural feature not enabled, cannot load model: {}",
                    model_path
                ),
            })
        }
    }
}

#[cfg(not(feature = "neural"))]
pub use neural_stub::*;

// ---------------------------------------------------------------------------
// Top-level functions
// ---------------------------------------------------------------------------

#[uniffi::export]
fn engine_version() -> String {
    "0.1.0".to_string()
}

#[uniffi::export]
fn romaji_lookup(romaji: String) -> LexRomajiLookup {
    let trie = RomajiTrie::global();
    match trie.lookup(&romaji) {
        TrieLookupResult::None => LexRomajiLookup::None,
        TrieLookupResult::Prefix => LexRomajiLookup::Prefix,
        TrieLookupResult::Exact(kana) => LexRomajiLookup::Exact { kana },
        TrieLookupResult::ExactAndPrefix(kana) => LexRomajiLookup::ExactAndPrefix { kana },
    }
}

#[uniffi::export]
fn romaji_convert(kana: String, pending: String, force: bool) -> LexRomajiConvert {
    let result = convert_romaji(&kana, &pending, force);
    LexRomajiConvert {
        composed_kana: result.composed_kana,
        pending_romaji: result.pending_romaji,
    }
}

#[uniffi::export]
fn trace_init(log_dir: String) {
    crate::trace_init::init_tracing(Path::new(&log_dir));
}

// ---------------------------------------------------------------------------
// Conversion helpers
// ---------------------------------------------------------------------------

fn convert_to_events(resp: KeyResponse, has_pending_work: bool) -> LexKeyResponse {
    let has_marked = resp.marked.is_some();
    let mut events = Vec::new();

    // 1. Commit
    if let Some(text) = resp.commit {
        events.push(LexEvent::Commit { text });
    }

    // 2. Marked text
    if let Some(m) = resp.marked {
        events.push(LexEvent::SetMarkedText {
            text: m.text,
            dashed: m.dashed,
        });
    }

    // 3. Candidates
    match resp.candidates {
        CandidateAction::Show { surfaces, selected } => {
            events.push(LexEvent::ShowCandidates { surfaces, selected });
        }
        CandidateAction::Hide => events.push(LexEvent::HideCandidates),
        CandidateAction::Keep => {}
    }

    // 4. Side effects
    if resp.side_effects.switch_to_abc {
        events.push(LexEvent::SwitchToAbc);
    }
    if resp.side_effects.save_history {
        events.push(LexEvent::SaveHistory);
    }

    // 5. Ghost text
    if let Some(ghost) = resp.ghost_text {
        if ghost.is_empty() {
            // Clear ghost. update_display = true when no marked text was set in this response
            events.push(LexEvent::ClearGhostText {
                update_display: !has_marked,
            });
        } else {
            events.push(LexEvent::SetGhostText { text: ghost });
        }
    }

    // 6. Schedule poll
    if has_pending_work {
        events.push(LexEvent::SchedulePoll);
    }

    LexKeyResponse {
        consumed: resp.consumed,
        events,
    }
}
