use lex_core::candidates::{
    generate_candidates, generate_prediction_candidates, CandidateResponse,
};
use lex_core::converter::ConvertedSegment;
use lex_core::dict::connection::ConnectionMatrix;
use lex_core::dict::TrieDictionary;
use lex_core::user_history::UserHistory;

// macOS virtual key codes
pub(super) mod key {
    pub const ENTER: u16 = 36;
    pub const TAB: u16 = 48;
    pub const SPACE: u16 = 49;
    pub const BACKSPACE: u16 = 51;
    pub const ESCAPE: u16 = 53;
    pub const YEN: u16 = 93;
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

/// Pluggable conversion mode: determines how candidates are generated,
/// what Tab does, and whether auto-commit fires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConversionMode {
    /// Standard IME: Viterbi N-best + predictions + lookup, Tab toggles submode.
    Standard,
    /// Predictive: Viterbi base + bigram chaining for Copilot-like completions, Tab commits.
    Predictive,
    /// GhostText: Speculative decode (composing) + GPT-2 ghost text (idle after commit).
    GhostText,
}

pub(super) enum TabAction {
    ToggleSubmode,
    Commit,
}

impl ConversionMode {
    pub(super) fn generate_candidates(
        &self,
        dict: &TrieDictionary,
        conn: Option<&ConnectionMatrix>,
        history: Option<&UserHistory>,
        reading: &str,
        max_results: usize,
    ) -> CandidateResponse {
        match self {
            Self::Standard | Self::GhostText => {
                generate_candidates(dict, conn, history, reading, max_results)
            }
            Self::Predictive => {
                generate_prediction_candidates(dict, conn, history, reading, max_results)
            }
        }
    }

    pub(super) fn tab_action(&self) -> TabAction {
        match self {
            Self::Standard => TabAction::ToggleSubmode,
            Self::Predictive | Self::GhostText => TabAction::Commit,
        }
    }

    pub(super) fn auto_commit_enabled(&self) -> bool {
        matches!(self, Self::Standard)
    }

    /// FFI dispatch tag for async candidate generation.
    /// Swift uses this to call the correct FFI generator.
    pub fn candidate_dispatch(&self) -> u8 {
        match self {
            Self::Standard => 0,
            Self::Predictive => 1,
            Self::GhostText => 2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Submode {
    Japanese,
    English,
}

pub(super) enum SessionState {
    Idle,
    Composing(Composition),
}

pub(super) struct Composition {
    pub(super) submode: Submode,
    pub(super) kana: String,
    pub(super) pending: String,
    pub(super) prefix: FrozenPrefix,
    pub(super) candidates: CandidateState,
    pub(super) stability: StabilityTracker,
}

impl Composition {
    pub(super) fn new(submode: Submode) -> Self {
        Self {
            submode,
            kana: String::new(),
            pending: String::new(),
            prefix: FrozenPrefix::new(),
            candidates: CandidateState::new(),
            stability: StabilityTracker::new(),
        }
    }

    /// Compute the display string (replaces `current_display` field).
    /// Uses the selected candidate surface in Japanese mode, falls back to kana + pending.
    /// When pending romaji is present, appends it to the candidate surface so the user
    /// sees e.g. "違和感なk" rather than reverting to raw kana "いわかんなk".
    pub(super) fn display(&self) -> String {
        let segment = if self.submode == Submode::Japanese {
            if let Some(surface) = self.candidates.surfaces.get(self.candidates.selected) {
                if self.pending.is_empty() {
                    surface.clone()
                } else {
                    format!("{}{}", surface, self.pending)
                }
            } else {
                format!("{}{}", self.kana, self.pending)
            }
        } else {
            format!("{}{}", self.kana, self.pending)
        };
        format!("{}{}", self.prefix.text, segment)
    }

    /// Display string without candidates (always kana + pending).
    pub(super) fn display_kana(&self) -> String {
        format!("{}{}{}", self.prefix.text, self.kana, self.pending)
    }

    /// Convert pending romaji to kana. If `force`, flush incomplete sequences.
    pub(super) fn drain_pending(&mut self, force: bool) {
        let result = lex_core::romaji::convert_romaji(&self.kana, &self.pending, force);
        self.kana = result.composed_kana;
        self.pending = result.pending_romaji;
    }

    /// Flush all pending romaji (force incomplete sequences).
    pub(super) fn flush(&mut self) {
        self.drain_pending(true);
    }

    /// Find the N-best path whose concatenated surfaces match `surface`.
    /// Returns segment pairs (reading, surface) for sub-phrase history recording.
    pub(super) fn find_matching_path(&self, surface: &str) -> Option<Vec<(String, String)>> {
        let path =
            self.candidates.paths.iter().find(|path| {
                path.iter().map(|s| s.surface.as_str()).collect::<String>() == surface
            })?;
        if path.len() <= 1 {
            return None;
        }
        Some(
            path.iter()
                .map(|s| (s.reading.clone(), s.surface.clone()))
                .collect(),
        )
    }
}

// --- Session-level groupings ---

pub(super) struct SessionConfig {
    pub(super) programmer_mode: bool,
    pub(super) defer_candidates: bool,
    pub(super) conversion_mode: ConversionMode,
}

pub(super) struct GhostState {
    pub(super) text: Option<String>,
    pub(super) generation: u64,
}

// --- Sub-structures for grouping related state ---

pub(super) struct CandidateState {
    pub(super) surfaces: Vec<String>,
    pub(super) paths: Vec<Vec<ConvertedSegment>>,
    pub(super) selected: usize,
}

impl CandidateState {
    pub(super) fn new() -> Self {
        Self {
            surfaces: Vec::new(),
            paths: Vec::new(),
            selected: 0,
        }
    }

    pub(super) fn clear(&mut self) {
        self.surfaces.clear();
        self.paths.clear();
        self.selected = 0;
    }

    pub(super) fn is_empty(&self) -> bool {
        self.surfaces.is_empty()
    }
}

pub(super) struct StabilityTracker {
    pub(super) prev_first_seg_reading: Option<String>,
    pub(super) count: usize,
}

impl StabilityTracker {
    pub(super) fn new() -> Self {
        Self {
            prev_first_seg_reading: None,
            count: 0,
        }
    }

    pub(super) fn reset(&mut self) {
        self.prev_first_seg_reading = None;
        self.count = 0;
    }

    pub(super) fn track(&mut self, paths: &[Vec<ConvertedSegment>]) {
        let best_path = match paths.first() {
            Some(path) if path.len() >= 2 => path,
            _ => {
                self.reset();
                return;
            }
        };

        let first_reading = &best_path[0].reading;
        if Some(first_reading) == self.prev_first_seg_reading.as_ref() {
            self.count += 1;
        } else {
            self.prev_first_seg_reading = Some(first_reading.clone());
            self.count = 1;
        }
    }
}

pub(super) struct FrozenPrefix {
    pub(super) text: String,
    pub(super) has_boundary_space: bool,
}

impl FrozenPrefix {
    pub(super) fn new() -> Self {
        Self {
            text: String::new(),
            has_boundary_space: false,
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub(super) fn push_str(&mut self, s: &str) {
        self.text.push_str(s);
    }

    pub(super) fn pop(&mut self) -> Option<char> {
        self.text.pop()
    }

    pub(super) fn undo_boundary_space(&mut self) -> bool {
        if self.has_boundary_space && self.text.ends_with(' ') {
            self.text.pop();
            self.has_boundary_space = false;
            true
        } else {
            false
        }
    }
}

/// Marked (composing) text with underline style.
pub struct MarkedText {
    pub text: String,
    pub dashed: bool,
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
    pub save_history: bool,
}

/// Request for asynchronous ghost text generation (GhostText mode).
pub struct AsyncGhostRequest {
    pub context: String,
    pub generation: u64,
}

/// Response from handle_key / commit, returned to the caller (Swift via FFI).
pub struct KeyResponse {
    pub consumed: bool,
    pub commit: Option<String>,
    pub marked: Option<MarkedText>,
    pub candidates: CandidateAction,
    pub async_request: Option<AsyncCandidateRequest>,
    pub side_effects: SideEffects,
    /// Ghost text: `Some("")` = clear, `Some(text)` = show, `None` = no change.
    pub ghost_text: Option<String>,
    /// Request for async ghost text generation.
    pub ghost_request: Option<AsyncGhostRequest>,
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
            ghost_text: None,
            ghost_request: None,
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

pub(super) fn cyclic_index(current: usize, delta: i32, count: usize) -> usize {
    if count == 0 {
        return 0;
    }
    let c = current as i32;
    let n = count as i32;
    ((c + delta + n) % n) as usize
}
