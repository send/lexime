mod composition;
pub use composition::*;

/// Platform-independent key event.
#[derive(Debug, Clone)]
pub enum KeyEvent {
    /// Text input (romaji, punctuation, etc.)
    Text {
        text: String,
        /// Currently unused by lex-session (uppercase is detected from text content).
        /// Reserved for platform frontends that need shift state (e.g. Fcitx5).
        shift: bool,
    },
    /// Remapped text from platform keymap (e.g. JIS ¥→\).
    /// Like Text but falls back to direct commit if trie doesn't match.
    Remapped {
        text: String,
        /// Currently unused (remapped text already reflects shift state).
        /// Reserved for platform frontends.
        shift: bool,
    },
    Enter,
    Space,
    Backspace,
    Escape,
    Tab,
    ArrowDown,
    ArrowUp,
    /// 英数キー (macOS) / Fcitx5 deactivate
    SwitchToDirectInput,
    /// かなキー (macOS) / Fcitx5 activate
    SwitchToJapanese,
    /// Forward Delete (Fn+Delete on macOS) — delete history for selected candidate
    ForwardDelete,
    /// Cmd/Ctrl/Alt 付きキー — composing 中なら確定してパススルー
    ModifiedKey,
}

impl KeyEvent {
    pub fn text(s: &str) -> Self {
        KeyEvent::Text {
            text: s.to_string(),
            shift: false,
        }
    }

    pub fn text_shift(s: &str) -> Self {
        KeyEvent::Text {
            text: s.to_string(),
            shift: true,
        }
    }

    pub fn remapped(s: &str) -> Self {
        KeyEvent::Remapped {
            text: s.to_string(),
            shift: false,
        }
    }

    pub fn remapped_shift(s: &str) -> Self {
        KeyEvent::Remapped {
            text: s.to_string(),
            shift: true,
        }
    }
}

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

/// Dispatch mode for candidate generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateDispatch {
    Standard,
    Predictive,
}

/// Request for asynchronous candidate generation.
/// Bundles `needs_candidates` and `candidate_reading` so that
/// a request without a reading is structurally impossible.
pub struct AsyncCandidateRequest {
    pub reading: String,
    pub candidate_dispatch: CandidateDispatch,
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

    pub(super) fn with_marked(mut self, text: String) -> Self {
        self.marked = Some(MarkedText { text });
        self
    }

    pub(super) fn with_hide_candidates(mut self) -> Self {
        self.candidates = CandidateAction::Hide;
        self
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
    /// User requested deletion of history for this reading→surface.
    Deletion { segments: Vec<(String, String)> },
}

pub(super) fn cyclic_index(current: usize, delta: i32, count: usize) -> usize {
    if count == 0 {
        return 0;
    }
    let c = current as i32;
    let n = count as i32;
    ((c + delta + n) % n) as usize
}
