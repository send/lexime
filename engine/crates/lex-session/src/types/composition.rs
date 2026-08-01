use std::sync::Arc;

use lex_core::candidates::{
    generate_candidates, generate_prediction_candidates, CandidateResponse,
};
use lex_core::converter::ConvertedSegment;
use lex_core::dict::connection::ConnectionMatrix;
use lex_core::dict::Dictionary;
use lex_core::snippets::SnippetStore;
use lex_core::user_history::UserHistory;

/// Pluggable conversion mode: determines how candidates are generated
/// and whether auto-commit fires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConversionMode {
    /// Standard IME: Viterbi N-best + predictions + lookup.
    Standard,
    /// Predictive: Viterbi base + bigram chaining for Copilot-like completions.
    Predictive,
}

impl ConversionMode {
    pub(crate) fn generate_candidates(
        &self,
        dict: &dyn Dictionary,
        conn: Option<&ConnectionMatrix>,
        history: Option<&UserHistory>,
        reading: &str,
        max_results: usize,
    ) -> CandidateResponse {
        match self {
            Self::Standard => generate_candidates(dict, conn, history, reading, max_results),
            Self::Predictive => {
                generate_prediction_candidates(dict, conn, history, reading, max_results)
            }
        }
    }

    pub(crate) fn auto_commit_enabled(&self) -> bool {
        matches!(self, Self::Standard)
    }

    /// Dispatch tag for async candidate generation.
    pub fn candidate_dispatch(&self) -> super::CandidateDispatch {
        match self {
            Self::Standard => super::CandidateDispatch::Standard,
            Self::Predictive => super::CandidateDispatch::Predictive,
        }
    }
}

pub(crate) enum SessionState {
    Idle,
    Composing(Box<Composition>),
    Snippet(SnippetState),
}

pub(crate) struct SnippetState {
    pub(crate) filter: String,
    pub(crate) matches: Vec<(String, String)>,
    pub(crate) selected: usize,
    /// Snapshot of the store this browse began against. Filtering reads this,
    /// not the live `snippet_store`, so a mid-browse store swap
    /// (`snippetsDidReload`) cannot disturb the active picker or desync the
    /// host — the browse is a transaction against the store it started with.
    pub(crate) store: Arc<SnippetStore>,
}

impl SnippetState {
    /// Inline composition text shown to the host while browsing snippets.
    ///
    /// Must stay non-empty for every reachable snippet-mode state: Chromium/
    /// Electron hosts derive `KeyboardEvent.isComposing` from whether marked
    /// text is present, so a confirming Enter over *empty* marked text is
    /// dispatched to the web page (e.g. sends the message in chat apps) even
    /// though the IME also consumes it. Shows the typed filter, or — before
    /// anything is typed — the key of the currently selected snippet (the one
    /// Enter would insert).
    ///
    /// The `unwrap_or_default` is unreachable: the only state with both an empty
    /// filter and no matches is a browse with nothing to offer, which
    /// `enter_snippet_mode` refuses to start and `snippet_filter_pop` cancels
    /// rather than render.
    pub(crate) fn display_text(&self) -> String {
        if !self.filter.is_empty() {
            return self.filter.clone();
        }
        self.matches
            .get(self.selected)
            .map(|(key, _)| key.clone())
            .unwrap_or_default()
    }
}

pub(crate) struct Composition {
    pub(crate) kana: String,
    pub(crate) pending: String,
    pub(crate) prefix: FrozenPrefix,
    pub(crate) candidates: CandidateState,
    pub(crate) stability: StabilityTracker,
}

impl Composition {
    pub(crate) fn new() -> Self {
        Self {
            kana: String::new(),
            pending: String::new(),
            prefix: FrozenPrefix::new(),
            candidates: CandidateState::new(),
            stability: StabilityTracker::new(),
        }
    }

    /// Compute the display string.
    /// Uses the selected candidate surface, falls back to kana + pending.
    /// When pending romaji is present, appends it to the candidate surface so the user
    /// sees e.g. "違和感なk" rather than reverting to raw kana "いわかんなk".
    pub(crate) fn display(&self) -> String {
        let segment = if let Some(surface) = self.candidates.surfaces.get(self.candidates.selected)
        {
            if self.pending.is_empty() {
                surface.clone()
            } else {
                format!("{}{}", surface, self.pending)
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
    pub(crate) defer_candidates: bool,
    pub(crate) conversion_mode: ConversionMode,
}

// --- Sub-structures for grouping related state ---

pub(crate) struct CandidateState {
    pub(crate) surfaces: Vec<String>,
    pub(crate) paths: Vec<Vec<ConvertedSegment>>,
    pub(crate) selected: usize,
    /// True while `surfaces` holds only the interim result of deferred mode
    /// (the synchronous 1-best, or post-auto-commit remainders) and the full
    /// async N-best has not been received yet. The async refresh may never
    /// arrive (every key press invalidates in-flight work), so candidate
    /// navigation falls back to synchronous generation when this is set
    /// instead of cycling the interim list forever.
    pub(crate) provisional: bool,
    /// True once the user has moved the selection themselves (Space / ↑ / ↓).
    ///
    /// This is what the host is *showing*: the composing response emits
    /// `display_kana()` (the reading) while a selection response emits
    /// `display()` (the surface), so the two diverge exactly when candidates
    /// exist but the user has not navigated. A voluntary commit resolves to
    /// the selected surface regardless — the user asked to convert. An
    /// *involuntary* settle must not, or an app switch silently replaces what
    /// was on screen with a conversion the user never chose (#298 / #309).
    pub(crate) user_selected: bool,
}

impl CandidateState {
    pub(crate) fn new() -> Self {
        Self {
            surfaces: Vec::new(),
            paths: Vec::new(),
            selected: 0,
            provisional: false,
            user_selected: false,
        }
    }

    pub(crate) fn clear(&mut self) {
        self.surfaces.clear();
        self.paths.clear();
        self.selected = 0;
        self.provisional = false;
        self.user_selected = false;
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
}

impl FrozenPrefix {
    pub(crate) fn new() -> Self {
        Self {
            text: String::new(),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub(crate) fn pop(&mut self) -> Option<char> {
        self.text.pop()
    }
}
