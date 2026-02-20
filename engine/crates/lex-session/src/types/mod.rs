mod composition;
pub use composition::*;

// macOS virtual key codes
pub(super) mod key {
    pub const ENTER: u16 = 36;
    pub const TAB: u16 = 48;
    pub const SPACE: u16 = 49;
    pub const BACKSPACE: u16 = 51;
    pub const ESCAPE: u16 = 53;
    pub const EISU: u16 = 102;
    pub const KANA: u16 = 104;
    pub const DOWN: u16 = 125;
    pub const UP: u16 = 126;
}

// Flag bits for handle_key
pub(super) const FLAG_SHIFT: u8 = 1;
pub(super) const FLAG_HAS_MODIFIER: u8 = 2;

pub(super) const MAX_COMPOSED_KANA_LENGTH: usize = 100;
pub(super) const MAX_CANDIDATES: usize = 20;

/// Marked (composing) text.
pub struct MarkedText {
    pub text: String,
}

/// Candidate panel action — exactly one of three states.
/// Replaces the old `show_candidates` / `hide_candidates` bool pair,
/// making the invalid combination (both true) unrepresentable.
pub enum CandidateAction {
    /// Leave the panel as-is (e.g. deferred mode keeping stale candidates visible).
    Keep,
    /// Show or update the candidate panel with these surfaces.
    Show {
        surfaces: Vec<String>,
        selected: u32,
    },
    /// Hide the candidate panel.
    Hide,
}

/// Request for asynchronous candidate generation.
/// Bundles `needs_candidates` and `candidate_reading` so that
/// a request without a reading is structurally impossible.
pub struct AsyncCandidateRequest {
    pub reading: String,
    /// 0 = standard, 1 = prediction_only
    pub candidate_dispatch: u8,
}

/// Orthogonal side-effects that accompany a response.
#[derive(Default)]
pub struct SideEffects {
    pub switch_to_abc: bool,
}

/// Response from handle_key / commit, returned to the caller (Swift via FFI).
pub struct KeyResponse {
    pub consumed: bool,
    pub commit: Option<String>,
    pub marked: Option<MarkedText>,
    pub candidates: CandidateAction,
    pub async_request: Option<AsyncCandidateRequest>,
    pub side_effects: SideEffects,
}

impl KeyResponse {
    pub(super) fn not_consumed() -> Self {
        Self {
            consumed: false,
            commit: None,
            marked: None,
            candidates: CandidateAction::Keep,
            async_request: None,
            side_effects: SideEffects::default(),
        }
    }

    pub(super) fn consumed() -> Self {
        Self {
            consumed: true,
            ..Self::not_consumed()
        }
    }

    /// Merge: keep commit/side_effects from self, take display-related fields from `other`.
    pub(super) fn with_display_from(mut self, other: KeyResponse) -> KeyResponse {
        self.marked = other.marked;
        self.candidates = other.candidates;
        self.async_request = other.async_request;
        self
    }
}

pub(super) fn is_romaji_input(text: &str) -> bool {
    if text == "-" {
        return true;
    }
    match text.chars().next() {
        Some(c) => c.is_ascii_lowercase() || c.is_ascii_uppercase(),
        None => false,
    }
}

/// A typed record of what the user confirmed, for history learning.
#[derive(Debug, Clone)]
pub enum LearningRecord {
    /// User confirmed a whole reading→surface mapping.
    /// Generates unigram entry + optional sub-phrase bigrams.
    Committed {
        reading: String,
        surface: String,
        /// Pre-segmented N-best path (if available and multi-segment).
        /// Used for sub-phrase unigram + bigram learning.
        segments: Option<Vec<(String, String)>>,
    },
}

pub(super) fn cyclic_index(current: usize, delta: i32, count: usize) -> usize {
    if count == 0 {
        return 0;
    }
    let c = current as i32;
    let n = count as i32;
    ((c + delta + n) % n) as usize
}
