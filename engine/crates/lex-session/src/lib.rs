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
use lex_core::dict::Dictionary;
use lex_core::user_history::UserHistory;

pub use types::{
    AsyncCandidateRequest, AsyncGhostRequest, CandidateAction, ConversionMode, KeyResponse,
    LearningRecord, MarkedText, SideEffects,
};

use types::{Composition, GhostState, SessionConfig, SessionState, Submode};

/// Stateful IME session encapsulating all input processing logic.
pub struct InputSession {
    dict: Arc<dyn Dictionary>,
    conn: Option<Arc<ConnectionMatrix>>,
    history: Option<Arc<RwLock<UserHistory>>>,

    state: SessionState,
    /// Submode to use when starting a new composition from Idle.
    /// Reset to Japanese on commit/reset, toggled by Tab in Idle.
    idle_submode: Submode,

    config: SessionConfig,

    // History recording buffer
    history_records: Vec<LearningRecord>,

    ghost: GhostState,

    /// ABC passthrough mode: all keys pass through to app, except ¥→`\` and Kana.
    abc_passthrough: bool,

    // Accumulated committed text for neural context
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
            idle_submode: Submode::Japanese,
            config: SessionConfig {
                programmer_mode: false,
                defer_candidates: false,
                conversion_mode: ConversionMode::Standard,
            },
            history_records: Vec::new(),
            ghost: GhostState {
                text: None,
                generation: 0,
            },
            abc_passthrough: false,
            committed_context: String::new(),
        }
    }

    pub fn set_programmer_mode(&mut self, enabled: bool) {
        self.config.programmer_mode = enabled;
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
    pub fn take_history_records(&mut self) -> Vec<LearningRecord> {
        std::mem::take(&mut self.history_records)
    }

    /// Get the accumulated committed text for use as neural context.
    pub fn committed_context(&self) -> String {
        self.committed_context.clone()
    }

    /// Whether ghost text is currently being displayed.
    pub fn has_ghost_text(&self) -> bool {
        self.ghost.text.is_some()
    }
}
