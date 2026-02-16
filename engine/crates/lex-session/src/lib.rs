//! Stateful IME session managing composition, candidate selection, and key handling.
//!
//! `InputSession` owns the current editing state and processes each keystroke,
//! returning responses that the Swift frontend translates into IMKit calls.

pub(crate) mod types;

mod auto_commit;
mod candidate_gen;
mod commit;
mod composing;
mod ghost;
mod key_handlers;
mod response;
mod submode;

#[cfg(test)]
mod tests;

use std::sync::{Arc, RwLock};

use lex_core::dict::connection::ConnectionMatrix;
use lex_core::dict::TrieDictionary;
use lex_core::user_history::UserHistory;

pub use types::{
    AsyncCandidateRequest, AsyncGhostRequest, CandidateAction, ConversionMode, KeyResponse,
    MarkedText, SideEffects,
};

use types::{Composition, SessionState, Submode};

/// Stateful IME session encapsulating all input processing logic.
pub struct InputSession {
    dict: Arc<TrieDictionary>,
    conn: Option<Arc<ConnectionMatrix>>,
    history: Option<Arc<RwLock<UserHistory>>>,

    state: SessionState,
    /// Submode to use when starting a new composition from Idle.
    /// Reset to Japanese on commit/reset, toggled by Tab in Idle.
    idle_submode: Submode,

    // Settings
    programmer_mode: bool,
    /// When true, handle_key skips synchronous candidate generation and
    /// sets `needs_candidates` in the response for async generation by the caller.
    defer_candidates: bool,
    conversion_mode: ConversionMode,

    // History recording buffer
    history_records: Vec<Vec<(String, String)>>,

    // Ghost text state (GhostText mode)
    ghost_text: Option<String>,
    ghost_generation: u64,

    // Accumulated committed text for neural context
    committed_context: String,
}

impl InputSession {
    pub fn new(
        dict: Arc<TrieDictionary>,
        conn: Option<Arc<ConnectionMatrix>>,
        history: Option<Arc<RwLock<UserHistory>>>,
    ) -> Self {
        Self {
            dict,
            conn,
            history,
            state: SessionState::Idle,
            idle_submode: Submode::Japanese,
            programmer_mode: false,
            defer_candidates: false,
            conversion_mode: ConversionMode::Standard,
            history_records: Vec::new(),
            ghost_text: None,
            ghost_generation: 0,
            committed_context: String::new(),
        }
    }

    pub fn set_programmer_mode(&mut self, enabled: bool) {
        self.programmer_mode = enabled;
    }

    pub fn set_defer_candidates(&mut self, enabled: bool) {
        self.defer_candidates = enabled;
    }

    pub fn set_conversion_mode(&mut self, mode: ConversionMode) {
        self.conversion_mode = mode;
    }

    pub fn is_composing(&self) -> bool {
        matches!(self.state, SessionState::Composing(_))
    }

    /// Current submode, whether composing or idle.
    fn submode(&self) -> Submode {
        match &self.state {
            SessionState::Composing(c) => c.submode,
            SessionState::Idle => self.idle_submode,
        }
    }

    /// Mutable reference to the composing state. Panics if Idle.
    fn comp(&mut self) -> &mut Composition {
        match &mut self.state {
            SessionState::Composing(ref mut c) => c,
            SessionState::Idle => unreachable!("comp() called in Idle state"),
        }
    }

    pub fn composed_string(&self) -> String {
        match &self.state {
            SessionState::Composing(c) => c.display(),
            SessionState::Idle => String::new(),
        }
    }

    /// Commit the current composition (called by commitComposition).
    pub fn commit(&mut self) -> KeyResponse {
        self.commit_current_state()
    }

    /// Take recorded history entries, clearing the internal buffer.
    /// The caller should feed these to `UserHistory::record()`.
    pub fn take_history_records(&mut self) -> Vec<Vec<(String, String)>> {
        std::mem::take(&mut self.history_records)
    }

    /// Get the accumulated committed text for use as neural context.
    pub fn committed_context(&self) -> String {
        self.committed_context.clone()
    }
}
