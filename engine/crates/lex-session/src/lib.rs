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
mod lattice_cache;
mod response;
mod snippet_handler;

#[cfg(test)]
mod tests;

use std::sync::{Arc, RwLock};

use lex_core::dict::connection::ConnectionMatrix;
use lex_core::dict::Dictionary;
use lex_core::snippets::SnippetStore;
use lex_core::user_history::UserHistory;

pub use types::{
    AsyncCandidateRequest, CandidateAction, CandidateDispatch, ConversionMode, KeyEvent,
    KeyResponse, LearningRecord, MarkedText, SideEffects,
};

use lattice_cache::LatticeCache;
use types::{Composition, SessionConfig, SessionState};

/// Stateful IME session encapsulating all input processing logic.
pub struct InputSession {
    dict: Arc<dyn Dictionary>,
    conn: Option<Arc<ConnectionMatrix>>,
    history: Option<Arc<RwLock<UserHistory>>>,

    state: SessionState,

    config: SessionConfig,

    /// Monotonic counter identifying the current composition state.
    ///
    /// Bumped (under the caller-held session lock) at the start of every
    /// entry point that may mutate composition/candidate state, and when an
    /// async candidate response is accepted. Async candidate requests
    /// snapshot this value and responses carry it back; `receive_candidates`
    /// rejects any response whose epoch does not match. This closes the race
    /// where a stale async response arrives after a kana-preserving key
    /// (Space selection move / Escape / ForwardDelete) already changed the
    /// selection or panel visibility — a reading-match check alone cannot
    /// detect those.
    epoch: u64,

    /// Incremental Viterbi-input cache, independent of the UI `Composition`.
    pub(crate) lattice_cache: LatticeCache,

    // History recording buffer
    history_records: Vec<LearningRecord>,

    /// ABC passthrough mode: all keys pass through to app, except Kana.
    abc_passthrough: bool,

    snippet_store: Option<Arc<SnippetStore>>,
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
            epoch: 0,
            config: SessionConfig {
                defer_candidates: false,
                conversion_mode: ConversionMode::Standard,
            },
            lattice_cache: LatticeCache::new(),
            history_records: Vec::new(),
            abc_passthrough: false,
            snippet_store: None,
        }
    }

    pub fn set_defer_candidates(&mut self, enabled: bool) {
        self.config.defer_candidates = enabled;
    }

    pub fn set_conversion_mode(&mut self, mode: ConversionMode) {
        // In-flight async responses were generated under the old mode's
        // dispatch; invalidate them.
        self.bump_epoch();
        self.config.conversion_mode = mode;
    }

    /// Invalidate all in-flight async candidate responses.
    /// Called at the start of every entry point that mutates composition or
    /// candidate state. See the `epoch` field docs.
    fn bump_epoch(&mut self) {
        self.epoch += 1;
    }

    /// Current session epoch (see the `epoch` field docs).
    ///
    /// The FFI layer stamps every outgoing response with this value (read
    /// under the same session lock as the state change it reflects) so the
    /// platform frontend can drop async responses that get re-ordered behind
    /// newer synchronous key responses on its UI thread.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    pub fn is_composing(&self) -> bool {
        matches!(
            self.state,
            SessionState::Composing(_) | SessionState::Snippet(_)
        )
    }

    pub fn set_snippet_store(&mut self, store: Option<Arc<SnippetStore>>) {
        self.snippet_store = store;
    }

    pub fn is_abc_passthrough(&self) -> bool {
        self.abc_passthrough
    }

    pub fn set_abc_passthrough(&mut self, enabled: bool) {
        self.abc_passthrough = enabled;
    }

    /// Mutable reference to the composing state. Panics if not Composing.
    fn comp(&mut self) -> &mut Composition {
        match &mut self.state {
            SessionState::Composing(ref mut c) => c,
            _ => unreachable!("comp() called in non-Composing state"),
        }
    }

    pub fn composed_string(&self) -> String {
        match &self.state {
            SessionState::Composing(c) => c.display_kana(),
            SessionState::Snippet(s) => s.display_text(),
            SessionState::Idle => String::new(),
        }
    }

    /// Commit the current composition (called by commitComposition).
    pub fn commit(&mut self) -> KeyResponse {
        // Same contract as `handle_key`: every state-mutating entry point
        // invalidates in-flight async candidate responses.
        self.bump_epoch();
        if matches!(self.state, SessionState::Snippet(_)) {
            // Snippet mode: cancel and go back to idle
            self.reset_state();
            return KeyResponse::consumed()
                .with_marked(String::new())
                .with_hide_candidates();
        }
        self.commit_current_state()
    }

    /// Take recorded history entries, clearing the internal buffer.
    /// The caller should feed these to `UserHistory::record()`.
    pub fn take_history_records(&mut self) -> Vec<LearningRecord> {
        std::mem::take(&mut self.history_records)
    }
}
