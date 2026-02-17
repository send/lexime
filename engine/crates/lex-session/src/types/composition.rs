use lex_core::candidates::{
    generate_candidates, generate_prediction_candidates, CandidateResponse,
};
use lex_core::converter::ConvertedSegment;
use lex_core::dict::connection::ConnectionMatrix;
use lex_core::dict::TrieDictionary;
use lex_core::user_history::UserHistory;

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

pub(crate) enum TabAction {
    ToggleSubmode,
    Commit,
}

impl ConversionMode {
    pub(crate) fn generate_candidates(
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

    pub(crate) fn tab_action(&self) -> TabAction {
        match self {
            Self::Standard => TabAction::ToggleSubmode,
            Self::Predictive | Self::GhostText => TabAction::Commit,
        }
    }

    pub(crate) fn auto_commit_enabled(&self) -> bool {
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
pub(crate) enum Submode {
    Japanese,
    English,
}

pub(crate) enum SessionState {
    Idle,
    Composing(Composition),
}

pub(crate) struct Composition {
    pub(crate) submode: Submode,
    pub(crate) kana: String,
    pub(crate) pending: String,
    pub(crate) prefix: FrozenPrefix,
    pub(crate) candidates: CandidateState,
    pub(crate) stability: StabilityTracker,
}

impl Composition {
    pub(crate) fn new(submode: Submode) -> Self {
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
    pub(crate) fn display(&self) -> String {
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
    pub(crate) fn display_kana(&self) -> String {
        format!("{}{}{}", self.prefix.text, self.kana, self.pending)
    }

    /// Convert pending romaji to kana. If `force`, flush incomplete sequences.
    pub(crate) fn drain_pending(&mut self, force: bool) {
        let result = lex_core::romaji::convert_romaji(&self.kana, &self.pending, force);
        self.kana = result.composed_kana;
        self.pending = result.pending_romaji;
    }

    /// Flush all pending romaji (force incomplete sequences).
    pub(crate) fn flush(&mut self) {
        self.drain_pending(true);
    }

    /// Find the N-best path whose concatenated surfaces match `surface`.
    /// Returns segment pairs (reading, surface) for sub-phrase history recording.
    pub(crate) fn find_matching_path(&self, surface: &str) -> Option<Vec<(String, String)>> {
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

pub(crate) struct SessionConfig {
    pub(crate) programmer_mode: bool,
    pub(crate) defer_candidates: bool,
    pub(crate) conversion_mode: ConversionMode,
}

pub(crate) struct GhostState {
    pub(crate) text: Option<String>,
    pub(crate) generation: u64,
}

// --- Sub-structures for grouping related state ---

pub(crate) struct CandidateState {
    pub(crate) surfaces: Vec<String>,
    pub(crate) paths: Vec<Vec<ConvertedSegment>>,
    pub(crate) selected: usize,
}

impl CandidateState {
    pub(crate) fn new() -> Self {
        Self {
            surfaces: Vec::new(),
            paths: Vec::new(),
            selected: 0,
        }
    }

    pub(crate) fn clear(&mut self) {
        self.surfaces.clear();
        self.paths.clear();
        self.selected = 0;
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.surfaces.is_empty()
    }
}

pub(crate) struct StabilityTracker {
    pub(crate) prev_first_seg_reading: Option<String>,
    pub(crate) count: usize,
}

impl StabilityTracker {
    pub(crate) fn new() -> Self {
        Self {
            prev_first_seg_reading: None,
            count: 0,
        }
    }

    pub(crate) fn reset(&mut self) {
        self.prev_first_seg_reading = None;
        self.count = 0;
    }

    pub(crate) fn track(&mut self, paths: &[Vec<ConvertedSegment>]) {
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

pub(crate) struct FrozenPrefix {
    pub(crate) text: String,
    pub(crate) has_boundary_space: bool,
}

impl FrozenPrefix {
    pub(crate) fn new() -> Self {
        Self {
            text: String::new(),
            has_boundary_space: false,
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub(crate) fn push_str(&mut self, s: &str) {
        self.text.push_str(s);
    }

    pub(crate) fn pop(&mut self) -> Option<char> {
        self.text.pop()
    }

    pub(crate) fn undo_boundary_space(&mut self) -> bool {
        if self.has_boundary_space && self.text.ends_with(' ') {
            self.text.pop();
            self.has_boundary_space = false;
            true
        } else {
            false
        }
    }
}
