//! UniFFI export layer — type-safe Swift bindings for the Lexime engine.
//!
//! Each public type here maps to a generated Swift class, struct, or enum.
//! The old C FFI in `ffi/` remains for now; PR-3 will remove it and switch
//! Swift to use these bindings exclusively.

use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};

use crate::candidates::CandidateResponse;
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

/// Response from handle_key / commit.
#[derive(uniffi::Record)]
pub struct LexKeyResult {
    pub consumed: bool,
    pub commit_text: Option<String>,
    pub marked_text: Option<String>,
    pub is_dashed_underline: bool,
    pub candidate_action: LexCandidateAction,
    pub switch_to_abc: bool,
    pub save_history: bool,
    pub needs_candidates: bool,
    pub candidate_reading: Option<String>,
    pub candidate_dispatch: u8,
    pub ghost_text: Option<String>,
    pub needs_ghost_text: bool,
    pub ghost_context: Option<String>,
    pub ghost_generation: u64,
}

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

#[derive(uniffi::Enum)]
pub enum LexCandidateAction {
    Keep,
    Show {
        surfaces: Vec<String>,
        selected: u32,
    },
    Hide,
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
}

#[uniffi::export]
impl LexSession {
    #[uniffi::constructor]
    fn new(
        dict: Arc<LexDictionary>,
        conn: Option<Arc<LexConnection>>,
        history: Option<Arc<LexUserHistory>>,
    ) -> Arc<Self> {
        let session = InputSession::new(
            Arc::clone(&dict.inner),
            conn.as_ref().map(|c| Arc::clone(&c.inner)),
            history.as_ref().map(|h| Arc::clone(&h.inner)),
        );
        Arc::new(Self {
            dict,
            conn,
            history,
            session: Mutex::new(session),
        })
    }

    fn handle_key(&self, key_code: u16, text: String, flags: u8) -> LexKeyResult {
        let mut session = self.session.lock().unwrap();
        let resp = session.handle_key(key_code, &text, flags);
        let records = session.take_history_records();
        drop(session);
        self.record_history(&records);
        convert_response(resp)
    }

    fn commit(&self) -> LexKeyResult {
        let mut session = self.session.lock().unwrap();
        let resp = session.commit();
        let records = session.take_history_records();
        drop(session);
        self.record_history(&records);
        convert_response(resp)
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

    fn receive_candidates(
        &self,
        reading: String,
        result: LexCandidateResult,
    ) -> Option<LexKeyResult> {
        let paths: Vec<Vec<ConvertedSegment>> = result
            .paths
            .into_iter()
            .map(|p| {
                p.into_iter()
                    .map(|s| ConvertedSegment {
                        reading: s.reading,
                        surface: s.surface,
                    })
                    .collect()
            })
            .collect();

        let mut session = self.session.lock().unwrap();
        let resp = session.receive_candidates(&reading, result.surfaces, paths)?;
        let records = session.take_history_records();
        drop(session);
        self.record_history(&records);
        Some(convert_response(resp))
    }

    fn receive_ghost_text(&self, generation: u64, text: String) -> Option<LexKeyResult> {
        let mut session = self.session.lock().unwrap();
        let resp = session.receive_ghost_text(generation, text)?;
        Some(convert_response(resp))
    }

    fn ghost_generation(&self) -> u64 {
        self.session.lock().unwrap().ghost_generation()
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
        inner: Mutex<crate::neural::NeuralScorer>,
    }

    #[uniffi::export]
    impl LexNeuralScorer {
        #[uniffi::constructor]
        fn open(model_path: String) -> Result<Arc<Self>, LexError> {
            let scorer = crate::neural::NeuralScorer::open(Path::new(&model_path))
                .map_err(|e| LexError::Io { msg: e.to_string() })?;
            Ok(Arc::new(Self {
                inner: Mutex::new(scorer),
            }))
        }

        fn generate_ghost(&self, context: Option<String>, max_tokens: u32) -> Option<String> {
            let mut guard = self.inner.lock().ok()?;
            let config = crate::neural::GenerateConfig {
                max_tokens: max_tokens as usize,
                ..crate::neural::GenerateConfig::default()
            };
            guard
                .generate_text(context.as_deref().unwrap_or(""), &config)
                .ok()
        }
    }

    #[uniffi::export]
    fn generate_neural_candidates(
        scorer: &LexNeuralScorer,
        dict: &LexDictionary,
        conn: Option<Arc<LexConnection>>,
        history: Option<Arc<LexUserHistory>>,
        context: Option<String>,
        reading: Option<String>,
        max_results: u32,
    ) -> LexCandidateResult {
        let mut guard = match scorer.inner.lock() {
            Ok(g) => g,
            Err(_) => {
                return LexCandidateResult {
                    surfaces: vec![],
                    paths: vec![],
                }
            }
        };
        let h_guard = history.as_ref().and_then(|h| h.inner.read().ok());
        let hist_ref = h_guard.as_deref();
        let conn_ref = conn.as_ref().map(|c| c.inner.as_ref());
        let resp = crate::candidates::generate_neural_candidates(
            &mut guard,
            &dict.inner,
            conn_ref,
            hist_ref,
            context.as_deref().unwrap_or(""),
            reading.as_deref().unwrap_or(""),
            max_results as usize,
        );
        convert_candidate_response(resp)
    }
}

#[cfg(feature = "neural")]
pub use neural_api::*;

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
fn generate_candidates(
    dict: &LexDictionary,
    conn: Option<Arc<LexConnection>>,
    history: Option<Arc<LexUserHistory>>,
    reading: String,
    max_results: u32,
) -> LexCandidateResult {
    let h_guard = history.as_ref().and_then(|h| h.inner.read().ok());
    let hist_ref = h_guard.as_deref();
    let conn_ref = conn.as_ref().map(|c| c.inner.as_ref());
    let resp = crate::candidates::generate_candidates(
        &dict.inner,
        conn_ref,
        hist_ref,
        &reading,
        max_results as usize,
    );
    convert_candidate_response(resp)
}

#[uniffi::export]
fn generate_prediction_candidates(
    dict: &LexDictionary,
    conn: Option<Arc<LexConnection>>,
    history: Option<Arc<LexUserHistory>>,
    reading: String,
    max_results: u32,
) -> LexCandidateResult {
    let h_guard = history.as_ref().and_then(|h| h.inner.read().ok());
    let hist_ref = h_guard.as_deref();
    let conn_ref = conn.as_ref().map(|c| c.inner.as_ref());
    let resp = crate::candidates::generate_prediction_candidates(
        &dict.inner,
        conn_ref,
        hist_ref,
        &reading,
        max_results as usize,
    );
    convert_candidate_response(resp)
}

#[uniffi::export]
fn trace_init(log_dir: String) {
    crate::trace_init::init_tracing(Path::new(&log_dir));
}

// ---------------------------------------------------------------------------
// Conversion helpers
// ---------------------------------------------------------------------------

fn convert_response(resp: KeyResponse) -> LexKeyResult {
    let is_dashed = resp.marked.as_ref().is_some_and(|m| m.dashed);
    let candidate_action = match resp.candidates {
        CandidateAction::Keep => LexCandidateAction::Keep,
        CandidateAction::Show { surfaces, selected } => {
            LexCandidateAction::Show { surfaces, selected }
        }
        CandidateAction::Hide => LexCandidateAction::Hide,
    };
    let (needs_candidates, candidate_reading, candidate_dispatch) = match resp.async_request {
        Some(req) => (true, Some(req.reading), req.candidate_dispatch),
        None => (false, None, 0),
    };
    let (needs_ghost_text, ghost_context, ghost_generation) = match resp.ghost_request {
        Some(req) => (true, Some(req.context), req.generation),
        None => (false, None, 0),
    };
    LexKeyResult {
        consumed: resp.consumed,
        commit_text: resp.commit,
        marked_text: resp.marked.map(|m| m.text),
        is_dashed_underline: is_dashed,
        candidate_action,
        switch_to_abc: resp.side_effects.switch_to_abc,
        save_history: resp.side_effects.save_history,
        needs_candidates,
        candidate_reading,
        candidate_dispatch,
        ghost_text: resp.ghost_text,
        needs_ghost_text,
        ghost_context,
        ghost_generation,
    }
}

fn convert_candidate_response(resp: CandidateResponse) -> LexCandidateResult {
    let paths = resp
        .paths
        .into_iter()
        .map(|p| {
            p.into_iter()
                .map(|s| LexSegment {
                    reading: s.reading,
                    surface: s.surface,
                })
                .collect()
        })
        .collect();
    LexCandidateResult {
        surfaces: resp.surfaces,
        paths,
    }
}
