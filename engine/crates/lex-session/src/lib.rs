//! Stateful IME session managing composition, candidate selection, and key handling.
//!
//! `InputSession` owns the current editing state and processes each keystroke,
//! returning responses that the Swift frontend translates into IMKit calls.

pub(crate) mod types;

mod auto_commit;
mod candidate_gen;
mod commit;
mod composing;
mod key_handlers;
mod response;

#[cfg(test)]
mod tests;

use std::sync::{Arc, RwLock};

use lex_core::dict::connection::ConnectionMatrix;
use lex_core::dict::Dictionary;
use lex_core::user_history::UserHistory;

pub use types::{
    AsyncCandidateRequest, CandidateAction, ConversionMode, KeyResponse, LearningRecord,
    MarkedText, SideEffects,
};

use types::{Composition, SessionConfig, SessionState};

/// Stateful IME session encapsulating all input processing logic.
pub struct InputSession {
    dict: Arc<dyn Dictionary>,
    conn: Option<Arc<ConnectionMatrix>>,
    history: Option<Arc<RwLock<UserHistory>>>,

    state: SessionState,

    config: SessionConfig,

    // History recording buffer
    history_records: Vec<LearningRecord>,

    /// ABC passthrough mode: all keys pass through to app, except Kana.
    abc_passthrough: bool,

    // Accumulated committed text (conversion context)
    committed_context: String,
}

impl InputSession {
    pub fn new(
        dict: Arc<dyn Dictionary>,
        conn: Option<Arc<ConnectionMatrix>>,
        history: Option<Arc<RwLock<UserHistory>>>,
    ) -> Self {
        Self {
            dict,
            conn,
            history,
            state: SessionState::Idle,
            config: SessionConfig {
                defer_candidates: false,
                conversion_mode: ConversionMode::Standard,
            },
            history_records: Vec::new(),
            abc_passthrough: false,
            committed_context: String::new(),
        }
    }

    pub fn set_defer_candidates(&mut self, enabled: bool) {
        self.config.defer_candidates = enabled;
    }

    pub fn set_conversion_mode(&mut self, mode: ConversionMode) {
        self.config.conversion_mode = mode;
    }

    pub fn is_composing(&self) -> bool {
        matches!(self.state, SessionState::Composing(_))
    }

    pub fn is_abc_passthrough(&self) -> bool {
        self.abc_passthrough
    }

    pub fn set_abc_passthrough(&mut self, enabled: bool) {
        self.abc_passthrough = enabled;
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
            SessionState::Composing(c) => c.display_kana(),
            SessionState::Idle => String::new(),
        }
    }

    /// Commit the current composition (called by commitComposition).
    pub fn commit(&mut self) -> KeyResponse {
        self.commit_current_state()
    }

    /// Take recorded history entries, clearing the internal buffer.
    /// The caller should feed these to `UserHistory::record()`.
    pub fn take_history_records(&mut self) -> Vec<LearningRecord> {
        std::mem::take(&mut self.history_records)
    }

    /// Get the accumulated committed text (conversion context).
    pub fn committed_context(&self) -> String {
        self.committed_context.clone()
    }
}
