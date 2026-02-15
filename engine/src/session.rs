use tracing::debug_span;

use crate::candidates::{generate_candidates, generate_prediction_candidates, CandidateResponse};
use crate::converter::{convert, ConvertedSegment};
use crate::dict::connection::ConnectionMatrix;
use crate::dict::TrieDictionary;
use crate::romaji::{convert_romaji, RomajiTrie, TrieLookupResult};
use crate::user_history::UserHistory;

// macOS virtual key codes
mod key {
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
const FLAG_SHIFT: u8 = 1;
const FLAG_HAS_MODIFIER: u8 = 2;

const MAX_COMPOSED_KANA_LENGTH: usize = 100;
const MAX_CANDIDATES: usize = 20;

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

enum TabAction {
    ToggleSubmode,
    Commit,
}

impl ConversionMode {
    fn generate_candidates(
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

    fn tab_action(&self) -> TabAction {
        match self {
            Self::Standard => TabAction::ToggleSubmode,
            Self::Predictive | Self::GhostText => TabAction::Commit,
        }
    }

    fn auto_commit_enabled(&self) -> bool {
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
enum Submode {
    Japanese,
    English,
}

enum SessionState {
    Idle,
    Composing(Composition),
}

struct Composition {
    submode: Submode,
    kana: String,
    pending: String,
    prefix: FrozenPrefix,
    candidates: CandidateState,
    stability: StabilityTracker,
}

impl Composition {
    fn new(submode: Submode) -> Self {
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
    fn display(&self) -> String {
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
    fn display_kana(&self) -> String {
        format!("{}{}{}", self.prefix.text, self.kana, self.pending)
    }
}

// --- Sub-structures for grouping related state ---

struct CandidateState {
    surfaces: Vec<String>,
    paths: Vec<Vec<ConvertedSegment>>,
    selected: usize,
}

impl CandidateState {
    fn new() -> Self {
        Self {
            surfaces: Vec::new(),
            paths: Vec::new(),
            selected: 0,
        }
    }

    fn clear(&mut self) {
        self.surfaces.clear();
        self.paths.clear();
        self.selected = 0;
    }

    fn is_empty(&self) -> bool {
        self.surfaces.is_empty()
    }
}

struct StabilityTracker {
    prev_first_seg_reading: Option<String>,
    count: usize,
}

impl StabilityTracker {
    fn new() -> Self {
        Self {
            prev_first_seg_reading: None,
            count: 0,
        }
    }

    fn reset(&mut self) {
        self.prev_first_seg_reading = None;
        self.count = 0;
    }

    fn track(&mut self, paths: &[Vec<ConvertedSegment>]) {
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

struct FrozenPrefix {
    text: String,
    has_boundary_space: bool,
}

impl FrozenPrefix {
    fn new() -> Self {
        Self {
            text: String::new(),
            has_boundary_space: false,
        }
    }

    fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    fn push_str(&mut self, s: &str) {
        self.text.push_str(s);
    }

    fn pop(&mut self) -> Option<char> {
        self.text.pop()
    }

    fn undo_boundary_space(&mut self) -> bool {
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
    fn not_consumed() -> Self {
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

    fn consumed() -> Self {
        Self {
            consumed: true,
            ..Self::not_consumed()
        }
    }

    /// Merge: keep commit/side_effects from self, take display-related fields from `other`.
    fn with_display_from(mut self, other: KeyResponse) -> KeyResponse {
        self.marked = other.marked;
        self.candidates = other.candidates;
        self.async_request = other.async_request;
        self
    }
}

/// Stateful IME session encapsulating all input processing logic.
pub struct InputSession<'a> {
    dict: &'a TrieDictionary,
    conn: Option<&'a ConnectionMatrix>,
    history: Option<&'a UserHistory>,

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
}

impl<'a> InputSession<'a> {
    pub fn new(
        dict: &'a TrieDictionary,
        conn: Option<&'a ConnectionMatrix>,
        history: Option<&'a UserHistory>,
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

    /// Temporarily set the history reference. Used by FFI to pass a lock guard.
    pub fn set_history(&mut self, history: Option<&'a UserHistory>) {
        self.history = history;
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

    /// Process a key event. Returns a KeyResponse describing what the caller should do.
    ///
    /// `flags`: bit 0 = shift, bit 1 = has_modifier (Cmd/Ctrl/Opt)
    pub fn handle_key(&mut self, key_code: u16, text: &str, flags: u8) -> KeyResponse {
        let _span = debug_span!("handle_key", key_code, text, flags).entered();
        let has_modifier = flags & FLAG_HAS_MODIFIER != 0;
        let has_shift = flags & FLAG_SHIFT != 0;

        // Clear ghost text on any key except Tab (ghost accept is handled in handle_idle)
        let had_ghost = self.ghost_text.is_some();
        if had_ghost && key_code != key::TAB {
            self.ghost_text = None;
        }

        // Eisu key → commit if composing, switch to ABC
        let mut resp = if key_code == key::EISU {
            let mut r = if self.is_composing() {
                self.commit_current_state()
            } else {
                KeyResponse::consumed()
            };
            r.side_effects.switch_to_abc = true;
            r
        } else if key_code == key::KANA {
            // Kana key → already in Japanese mode, consume
            KeyResponse::consumed()
        } else if has_modifier {
            // Modifier keys (Cmd, Ctrl, etc.) — commit first, then pass through
            if self.is_composing() {
                let mut r = self.commit_current_state();
                r.consumed = false;
                r
            } else {
                KeyResponse::not_consumed()
            }
        } else if key_code == key::YEN && self.programmer_mode && !has_shift {
            // Programmer mode: ¥ key → insert backslash
            let mut r = if self.is_composing() {
                self.commit_current_state()
            } else {
                KeyResponse::consumed()
            };
            match r.commit {
                Some(ref mut t) => t.push('\\'),
                None => r.commit = Some("\\".to_string()),
            }
            r
        } else {
            match &self.state {
                SessionState::Idle => self.handle_idle(key_code, text),
                SessionState::Composing(_) => self.handle_composing(key_code, text),
            }
        };

        // Signal ghost clear if ghost was present and key wasn't Tab
        if had_ghost && key_code != key::TAB {
            resp.ghost_text = Some(String::new());
        }

        resp
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

    // -----------------------------------------------------------------------
    // Idle state
    // -----------------------------------------------------------------------

    fn handle_idle(&mut self, key_code: u16, text: &str) -> KeyResponse {
        // Ghost text: Tab accepts ghost (GhostText mode only)
        if key_code == key::TAB
            && self.ghost_text.is_some()
            && self.conversion_mode == ConversionMode::GhostText
        {
            return self.accept_ghost_text();
        }

        // Tab — toggle submode
        if key_code == key::TAB {
            return self.toggle_submode();
        }

        // English submode: add characters directly
        if self.idle_submode == Submode::English {
            if let Some(scalar) = text.chars().next() {
                let val = scalar as u32;
                if (0x20..0x7F).contains(&val) {
                    self.state = SessionState::Composing(Composition::new(Submode::English));
                    self.comp().prefix.has_boundary_space = false;
                    self.comp().kana.push_str(text);
                    return self.make_marked_text_response();
                }
            }
            return KeyResponse::not_consumed();
        }

        // Romaji input
        if is_romaji_input(text) {
            self.state = SessionState::Composing(Composition::new(Submode::Japanese));
            return self.append_and_convert(&text.to_lowercase());
        }

        // Direct trie match for non-romaji chars (punctuation)
        let trie = RomajiTrie::global();
        match trie.lookup(text) {
            TrieLookupResult::Exact(_) | TrieLookupResult::ExactAndPrefix(_) => {
                self.state = SessionState::Composing(Composition::new(Submode::Japanese));
                self.append_and_convert(text)
            }
            _ => KeyResponse::not_consumed(),
        }
    }

    // -----------------------------------------------------------------------
    // Composing state
    // -----------------------------------------------------------------------

    fn handle_composing(&mut self, key_code: u16, text: &str) -> KeyResponse {
        match key_code {
            key::ENTER => {
                if self.comp().submode == Submode::English {
                    let mut resp = self.commit_composed();
                    resp.candidates = CandidateAction::Hide;
                    resp
                } else {
                    // Lazy generate: ensure candidates are available for commit
                    if self.comp().candidates.is_empty() && !self.comp().kana.is_empty() {
                        self.update_candidates();
                    }
                    self.commit_current_state()
                }
            }

            key::SPACE => {
                if self.comp().submode == Submode::English {
                    self.comp().kana.push(' ');
                    self.make_marked_text_response()
                } else {
                    // Lazy generate: ensure candidates for Space cycling
                    if self.comp().candidates.is_empty() && !self.comp().kana.is_empty() {
                        self.update_candidates();
                    }
                    let c = self.comp();
                    if !c.candidates.is_empty() {
                        if c.candidates.selected == 0 && c.candidates.surfaces.len() > 1 {
                            c.candidates.selected = 1;
                        } else {
                            c.candidates.selected =
                                cyclic_index(c.candidates.selected, 1, c.candidates.surfaces.len());
                        }
                        self.make_candidate_selection_response()
                    } else {
                        KeyResponse::consumed()
                    }
                }
            }

            key::DOWN => {
                // Lazy generate: ensure candidates for arrow cycling
                if self.comp().candidates.is_empty() && !self.comp().kana.is_empty() {
                    self.update_candidates();
                }
                let c = self.comp();
                if !c.candidates.is_empty() {
                    c.candidates.selected =
                        cyclic_index(c.candidates.selected, 1, c.candidates.surfaces.len());
                    self.make_candidate_selection_response()
                } else {
                    KeyResponse::consumed()
                }
            }

            key::UP => {
                // Lazy generate: ensure candidates for arrow cycling
                if self.comp().candidates.is_empty() && !self.comp().kana.is_empty() {
                    self.update_candidates();
                }
                let c = self.comp();
                if !c.candidates.is_empty() {
                    c.candidates.selected =
                        cyclic_index(c.candidates.selected, -1, c.candidates.surfaces.len());
                    self.make_candidate_selection_response()
                } else {
                    KeyResponse::consumed()
                }
            }

            key::TAB => match self.conversion_mode.tab_action() {
                TabAction::ToggleSubmode => self.toggle_submode(),
                TabAction::Commit => {
                    // Lazy generate: ensure candidates for commit
                    if self.comp().candidates.is_empty() && !self.comp().kana.is_empty() {
                        self.update_candidates();
                    }
                    self.commit_current_state()
                }
            },

            key::BACKSPACE => self.handle_backspace(),

            key::ESCAPE => {
                self.flush();
                {
                    let c = self.comp();
                    if c.submode == Submode::Japanese && !c.kana.is_empty() {
                        let kana = c.kana.clone();
                        self.record_history(kana.clone(), kana);
                    }
                }
                self.comp().candidates.clear();
                let mut resp = KeyResponse::consumed();
                resp.candidates = CandidateAction::Hide;
                if !self.history_records.is_empty() {
                    resp.side_effects.save_history = true;
                }
                // Escape: IMKit will call commitComposition after.
                // composedString() uses display() which computes from current state.
                resp
            }

            _ => self.handle_composing_text(text),
        }
    }

    fn handle_composing_text(&mut self, text: &str) -> KeyResponse {
        // English submode: add characters directly
        if self.comp().submode == Submode::English {
            if let Some(scalar) = text.chars().next() {
                let val = scalar as u32;
                if (0x20..0x7F).contains(&val) {
                    self.comp().prefix.has_boundary_space = false;
                    self.comp().kana.push_str(text);
                    return self.make_marked_text_response();
                }
            }
            return KeyResponse::consumed();
        }

        // z-sequences: composing 中、pending + text が trie にマッチする場合
        if !self.comp().pending.is_empty() {
            let candidate = format!("{}{}", self.comp().pending, text);
            let trie = RomajiTrie::global();
            match trie.lookup(&candidate) {
                TrieLookupResult::Exact(_)
                | TrieLookupResult::ExactAndPrefix(_)
                | TrieLookupResult::Prefix => {
                    return self.append_and_convert(text);
                }
                TrieLookupResult::None => {}
            }
        }

        if is_romaji_input(text) {
            // If user has selected a non-default candidate, commit it first
            let c = self.comp();
            if c.candidates.selected > 0 && c.candidates.selected < c.candidates.surfaces.len() {
                let commit_resp = self.commit_current_state();
                self.state = SessionState::Composing(Composition::new(Submode::Japanese));
                let append_resp = self.append_and_convert(&text.to_lowercase());
                return commit_resp.with_display_from(append_resp);
            }
            return self.append_and_convert(&text.to_lowercase());
        }

        // Direct trie match for non-romaji chars (punctuation auto-commit)
        {
            let trie = RomajiTrie::global();
            match trie.lookup(text) {
                TrieLookupResult::Exact(_) | TrieLookupResult::ExactAndPrefix(_) => {
                    let mut resp = self.commit_current_state();
                    // Convert punctuation
                    let result = convert_romaji("", text, true);
                    if !result.composed_kana.is_empty() {
                        match resp.commit {
                            Some(ref mut t) => t.push_str(&result.composed_kana),
                            None => resp.commit = Some(result.composed_kana),
                        }
                    }
                    return resp;
                }
                _ => {}
            }
        }

        // Unrecognized non-romaji character — add to kana
        self.comp().kana.push_str(text);
        if self.defer_candidates {
            self.make_deferred_candidates_response()
        } else {
            self.update_candidates();
            self.make_marked_text_and_candidates_response()
        }
    }

    // -----------------------------------------------------------------------
    // Romaji conversion
    // -----------------------------------------------------------------------

    fn append_and_convert(&mut self, input: &str) -> KeyResponse {
        // Overflow: flush + commit if kana too long
        if self.comp().kana.len() >= MAX_COMPOSED_KANA_LENGTH {
            let resp = self.commit_composed();
            self.state = SessionState::Composing(Composition::new(Submode::Japanese));
            self.comp().pending.push_str(input);
            self.drain_pending(false);
            let sub_resp = if self.defer_candidates {
                self.make_deferred_candidates_response()
            } else {
                if self.comp().pending.is_empty() {
                    self.update_candidates();
                }
                self.make_marked_text_and_candidates_response()
            };
            return resp.with_display_from(sub_resp);
        }

        self.comp().prefix.has_boundary_space = false;
        self.comp().pending.push_str(input);
        self.drain_pending(false);

        if self.defer_candidates {
            if self.comp().pending.is_empty() {
                // Kana resolved — defer candidate generation to caller
                self.make_deferred_candidates_response()
            } else {
                // Pending romaji: show kana + pending, no candidates needed yet
                self.make_marked_text_response()
            }
        } else {
            // Sync mode: generate candidates immediately when romaji resolves
            if self.comp().pending.is_empty() {
                self.update_candidates();
            }
            self.make_marked_text_and_candidates_response()
        }
    }

    fn drain_pending(&mut self, force: bool) {
        let c = self.comp();
        let result = convert_romaji(&c.kana, &c.pending, force);
        c.kana = result.composed_kana;
        c.pending = result.pending_romaji;
    }

    fn flush(&mut self) {
        self.drain_pending(true);
    }

    // -----------------------------------------------------------------------
    // Candidate generation
    // -----------------------------------------------------------------------

    fn update_candidates(&mut self) {
        self.comp().candidates.selected = 0;

        if self.comp().kana.is_empty() {
            let c = self.comp();
            c.candidates.clear();
            c.stability.reset();
            return;
        }

        let mode = self.conversion_mode;
        let reading = self.comp().kana.clone();
        let CandidateResponse { surfaces, paths } =
            mode.generate_candidates(self.dict, self.conn, self.history, &reading, MAX_CANDIDATES);
        let c = self.comp();
        c.candidates.surfaces = surfaces;
        c.candidates.paths = paths;
        c.stability.track(&c.candidates.paths);
    }

    /// Build a response that defers candidate generation to the caller.
    /// Computes a synchronous 1-best conversion for interim display so the
    /// marked text shows a converted result immediately (e.g. "違和感無く")
    /// rather than raw kana while the full N-best candidates are generated async.
    fn make_deferred_candidates_response(&mut self) -> KeyResponse {
        // Do NOT reset stability here. It accumulates across keystrokes.
        let reading = self.comp().kana.clone();
        if !reading.is_empty() {
            // Quick sync 1-best for interim display (~1-2ms)
            let segments = convert(self.dict, self.conn, &reading);
            let surface: String = segments.iter().map(|s| s.surface.as_str()).collect();
            let c = self.comp();
            c.candidates.surfaces = vec![surface];
            c.candidates.paths = vec![segments];
            c.candidates.selected = 0;
        } else {
            self.comp().candidates.clear();
        }
        let mut resp = self.make_marked_text_response();
        if !reading.is_empty() {
            resp.async_request = Some(AsyncCandidateRequest {
                reading,
                candidate_dispatch: self.conversion_mode.candidate_dispatch(),
            });
        }
        resp
    }

    /// Receive asynchronously generated candidates and update session state.
    /// Returns `None` if the reading is stale (kana has changed).
    pub fn receive_candidates(
        &mut self,
        reading: &str,
        surfaces: Vec<String>,
        paths: Vec<Vec<ConvertedSegment>>,
    ) -> Option<KeyResponse> {
        // Stale check: reading must match current state
        match &self.state {
            SessionState::Composing(c) if c.kana == reading && c.submode == Submode::Japanese => {}
            _ => return None,
        }

        let c = self.comp();
        c.candidates.surfaces = surfaces;
        c.candidates.paths = paths;
        c.candidates.selected = 0;
        c.stability.track(&c.candidates.paths);

        // Try auto-commit with fresh candidates
        if let Some(auto_resp) = self.try_auto_commit() {
            return Some(auto_resp);
        }

        // No auto-commit: update marked text to Viterbi #1 and show candidates
        Some(self.make_marked_text_and_candidates_response())
    }

    // -----------------------------------------------------------------------
    // Segment stability auto-commit
    // -----------------------------------------------------------------------

    fn try_auto_commit(&mut self) -> Option<KeyResponse> {
        if !self.conversion_mode.auto_commit_enabled() {
            return None;
        }
        // Extract data from comp() in a block so the borrow is dropped before
        // we access self.history_records.
        let (committed_reading, committed_surface, seg_pairs, commit_count) = {
            let c = self.comp();
            if c.stability.count < 3 {
                return None;
            }
            let best_path = c.candidates.paths.first()?;
            if best_path.len() < 4 {
                return None;
            }
            if c.candidates.selected != 0 {
                return None;
            }
            if !c.pending.is_empty() {
                return None;
            }

            // Count how many segments to commit (group consecutive ASCII)
            let mut commit_count = 1;
            if best_path[0].surface.is_ascii() {
                while commit_count < best_path.len() - 1
                    && best_path[commit_count].surface.is_ascii()
                {
                    commit_count += 1;
                }
            }

            let segments: Vec<&ConvertedSegment> = best_path[0..commit_count].iter().collect();
            let committed_reading: String = segments.iter().map(|s| s.reading.as_str()).collect();
            let committed_surface: String = segments.iter().map(|s| s.surface.as_str()).collect();

            if !c.kana.starts_with(&committed_reading) {
                return None;
            }

            let seg_pairs: Option<Vec<(String, String)>> = if commit_count > 1 {
                Some(
                    segments
                        .iter()
                        .map(|s| (s.reading.clone(), s.surface.clone()))
                        .collect(),
                )
            } else {
                None
            };

            (
                committed_reading,
                committed_surface,
                seg_pairs,
                commit_count,
            )
        };

        // Record to history (comp() borrow is dropped)
        if committed_surface != committed_reading {
            let pairs = vec![(committed_reading.clone(), committed_surface.clone())];
            self.history_records.push(pairs);
        }
        if let Some(seg_pairs) = seg_pairs {
            self.history_records.push(seg_pairs);
        }

        // Remove committed reading from kana
        let c = self.comp();
        c.kana = c.kana[committed_reading.len()..].to_string();
        c.stability.reset();

        // Include prefix in the committed text, then clear it
        let prefix_text = std::mem::take(&mut c.prefix.text);
        c.prefix.has_boundary_space = false;
        let mut resp = KeyResponse::consumed();
        resp.commit = Some(format!("{}{}", prefix_text, committed_surface));
        resp.side_effects.save_history = true;

        if self.comp().kana.is_empty() {
            self.comp().candidates.clear();
            resp.candidates = CandidateAction::Hide;
            resp.marked = Some(MarkedText {
                text: String::new(),
                dashed: false,
            });
        } else if self.defer_candidates {
            // Async mode: extract provisional candidates from remaining N-best
            // segments so the candidate panel stays visible (no flicker).
            let c = self.comp();
            let mut provisional: Vec<String> = Vec::new();
            let mut seen = std::collections::HashSet::new();
            for path in &c.candidates.paths {
                if path.len() > commit_count {
                    let remaining: String = path[commit_count..]
                        .iter()
                        .map(|s| s.surface.as_str())
                        .collect();
                    if !remaining.is_empty() && seen.insert(remaining.clone()) {
                        provisional.push(remaining);
                    }
                }
            }
            // Always include kana as a fallback candidate
            if seen.insert(c.kana.clone()) {
                provisional.push(c.kana.clone());
            }

            // kana is guaranteed non-empty here (empty case handled above),
            // so provisional always has at least the kana entry.
            debug_assert!(!provisional.is_empty());

            c.candidates.clear();

            // Store provisional candidates in session state so that candidate
            // navigation (Space / Arrow) works during the async phase.
            c.candidates.surfaces.clone_from(&provisional);

            // prefix.text was already consumed into commit via std::mem::take
            // above, so it is empty here — no need to prepend it.
            resp.marked = Some(MarkedText {
                text: provisional[0].clone(),
                dashed: false,
            });
            resp.async_request = Some(AsyncCandidateRequest {
                reading: c.kana.clone(),
                candidate_dispatch: self.conversion_mode.candidate_dispatch(),
            });
            resp.candidates = CandidateAction::Show {
                surfaces: provisional,
                selected: 0,
            };
        } else {
            // Sync mode: re-generate candidates for remaining input
            let c = self.comp();
            let dashed = c.submode == Submode::English;
            let display = c.display_kana();
            resp.marked = Some(MarkedText {
                text: display,
                dashed,
            });
            self.update_candidates();
            let c = self.comp();
            if let Some(best) = c.candidates.surfaces.first() {
                resp.marked = Some(MarkedText {
                    text: format!("{}{}", c.prefix.text, best),
                    dashed,
                });
            }
            if !c.candidates.is_empty() {
                resp.candidates = CandidateAction::Show {
                    surfaces: c.candidates.surfaces.clone(),
                    selected: c.candidates.selected as u32,
                };
            }
        }

        Some(resp)
    }

    // -----------------------------------------------------------------------
    // Commit helpers
    // -----------------------------------------------------------------------

    fn commit_composed(&mut self) -> KeyResponse {
        let mut resp = KeyResponse::consumed();
        let c = self.comp();
        let text = format!("{}{}", c.prefix.text, c.kana);
        if !text.is_empty() {
            resp.commit = Some(text);
        } else {
            resp.marked = Some(MarkedText {
                text: String::new(),
                dashed: false,
            });
        }
        self.reset_state();
        resp
    }

    fn commit_current_state(&mut self) -> KeyResponse {
        if !self.is_composing() {
            return KeyResponse::consumed();
        }

        let mut resp = KeyResponse::consumed();
        resp.candidates = CandidateAction::Hide;
        self.flush();

        let c = self.comp();
        let prefix_text = std::mem::take(&mut c.prefix.text);

        if c.candidates.selected < c.candidates.surfaces.len() {
            let reading = c.kana.clone();
            let surface = c.candidates.surfaces[c.candidates.selected].clone();

            self.record_history(reading, surface.clone());
            resp.side_effects.save_history = true;
            resp.commit = Some(format!("{}{}", prefix_text, surface));
        } else {
            let c = self.comp();
            if !c.kana.is_empty() || !prefix_text.is_empty() {
                resp.commit = Some(format!("{}{}", prefix_text, c.kana));
            } else {
                resp.marked = Some(MarkedText {
                    text: String::new(),
                    dashed: false,
                });
            }
        }

        // GhostText mode: request ghost text generation after commit
        if self.conversion_mode == ConversionMode::GhostText {
            if let Some(ref committed) = resp.commit {
                self.ghost_generation += 1;
                resp.ghost_request = Some(AsyncGhostRequest {
                    context: committed.clone(),
                    generation: self.ghost_generation,
                });
            }
        }

        self.reset_state();
        resp
    }

    fn record_history(&mut self, reading: String, surface: String) {
        if self.history.is_none() {
            return;
        }
        // Record whole pair
        self.history_records
            .push(vec![(reading.clone(), surface.clone())]);

        // Sub-phrase learning: if a matching N-best path exists
        if let Some(matching_path) = self
            .comp()
            .candidates
            .paths
            .iter()
            .find(|path| path.iter().map(|s| s.surface.as_str()).collect::<String>() == surface)
        {
            if matching_path.len() > 1 {
                let seg_pairs: Vec<(String, String)> = matching_path
                    .iter()
                    .map(|s| (s.reading.clone(), s.surface.clone()))
                    .collect();
                self.history_records.push(seg_pairs);
            }
        }
    }

    fn reset_state(&mut self) {
        self.state = SessionState::Idle;
        self.idle_submode = Submode::Japanese;
    }

    // -----------------------------------------------------------------------
    // Ghost text (GhostText mode)
    // -----------------------------------------------------------------------

    /// Accept the current ghost text (Tab in idle with ghost visible).
    fn accept_ghost_text(&mut self) -> KeyResponse {
        let text = self.ghost_text.take().unwrap();
        let mut resp = KeyResponse::consumed();
        resp.commit = Some(text);
        // After accepting ghost, request another generation
        if self.conversion_mode == ConversionMode::GhostText {
            if let Some(ref committed) = resp.commit {
                self.ghost_generation += 1;
                resp.ghost_request = Some(AsyncGhostRequest {
                    context: committed.clone(),
                    generation: self.ghost_generation,
                });
            }
        }
        resp
    }

    /// Receive ghost text from async generation. Returns a response if valid.
    /// Returns `None` if the generation is stale or session is in wrong state.
    pub fn receive_ghost_text(&mut self, generation: u64, text: String) -> Option<KeyResponse> {
        if generation != self.ghost_generation {
            return None;
        }
        if self.is_composing() {
            return None;
        }
        if self.conversion_mode != ConversionMode::GhostText {
            return None;
        }
        self.ghost_text = Some(text.clone());
        let mut resp = KeyResponse::consumed();
        resp.ghost_text = Some(text);
        Some(resp)
    }

    /// Get current ghost generation counter (for staleness checks).
    pub fn ghost_generation(&self) -> u64 {
        self.ghost_generation
    }

    // -----------------------------------------------------------------------
    // Backspace
    // -----------------------------------------------------------------------

    fn handle_backspace(&mut self) -> KeyResponse {
        {
            let c = self.comp();
            if !c.pending.is_empty() {
                c.pending.pop();
            } else if !c.kana.is_empty() {
                c.kana.pop();
            } else if !c.prefix.is_empty() {
                c.prefix.pop();
            }
        }

        let c = self.comp();
        let all_empty = c.kana.is_empty() && c.pending.is_empty() && c.prefix.is_empty();

        if all_empty {
            let mut resp = KeyResponse::consumed();
            resp.candidates = CandidateAction::Hide;
            resp.marked = Some(MarkedText {
                text: String::new(),
                dashed: false,
            });
            self.reset_state();
            resp
        } else if self.comp().kana.is_empty() && self.comp().pending.is_empty() {
            // Current segment is empty but prefix has content
            let c = self.comp();
            c.candidates.clear();
            let display = c.display();
            let mut resp = KeyResponse::consumed();
            resp.marked = Some(MarkedText {
                text: display,
                dashed: c.submode == Submode::English,
            });
            resp.candidates = CandidateAction::Hide;
            resp
        } else if self.defer_candidates && self.comp().submode == Submode::Japanese {
            self.make_deferred_candidates_response()
        } else {
            if self.comp().submode == Submode::Japanese {
                self.update_candidates();
            }
            self.make_marked_text_and_candidates_response()
        }
    }

    // -----------------------------------------------------------------------
    // Submode toggle
    // -----------------------------------------------------------------------

    fn toggle_submode(&mut self) -> KeyResponse {
        let current_submode = self.submode();
        let new_submode = match current_submode {
            Submode::Japanese => Submode::English,
            Submode::English => Submode::Japanese,
        };

        if self.is_composing() {
            // Flush pending romaji before switching
            if !self.comp().pending.is_empty() {
                self.flush();
            }

            // Undo boundary space if nothing was typed since the last toggle
            self.comp().prefix.undo_boundary_space();

            // Crystallize the current segment into prefix.
            match current_submode {
                Submode::Japanese => {
                    let c = self.comp();
                    let frozen = if c.candidates.selected < c.candidates.surfaces.len() {
                        let reading = c.kana.clone();
                        let surface = c.candidates.surfaces[c.candidates.selected].clone();
                        self.record_history(reading, surface.clone());
                        surface
                    } else {
                        self.comp().kana.clone()
                    };
                    self.comp().prefix.push_str(&frozen);
                }
                Submode::English => {
                    let kana = self.comp().kana.clone();
                    self.comp().prefix.push_str(&kana);
                }
            }
            // Clear the current segment for the new submode
            let c = self.comp();
            c.kana.clear();
            c.pending.clear();
            c.candidates.clear();
            c.stability.reset();

            // Programmer mode: insert space at submode boundary
            c.prefix.has_boundary_space = false;
            if self.programmer_mode && !self.comp().prefix.is_empty() {
                if let Some(last) = self.comp().prefix.text.chars().last() {
                    let last_is_ascii = last.is_ascii();
                    let should_insert = (current_submode == Submode::Japanese
                        && new_submode == Submode::English
                        && !last_is_ascii)
                        || (current_submode == Submode::English
                            && new_submode == Submode::Japanese
                            && last_is_ascii
                            && last != ' ');
                    if should_insert {
                        self.comp().prefix.text.push(' ');
                        self.comp().prefix.has_boundary_space = true;
                    }
                }
            }

            self.comp().submode = new_submode;

            let display = self.comp().display();
            let mut resp = KeyResponse::consumed();
            if !display.is_empty() {
                resp.marked = Some(MarkedText {
                    text: display,
                    dashed: new_submode == Submode::English,
                });
            }
            resp.candidates = CandidateAction::Hide;
            if !self.history_records.is_empty() {
                resp.side_effects.save_history = true;
            }
            resp
        } else {
            // Idle: just toggle the idle_submode
            self.idle_submode = new_submode;
            KeyResponse::consumed()
        }
    }

    // -----------------------------------------------------------------------
    // Response builders
    // -----------------------------------------------------------------------

    fn make_marked_text_response(&mut self) -> KeyResponse {
        let c = self.comp();
        let display = c.display();
        let mut resp = KeyResponse::consumed();
        resp.marked = Some(MarkedText {
            text: display,
            dashed: c.submode == Submode::English,
        });
        resp
    }

    fn make_marked_text_and_candidates_response(&mut self) -> KeyResponse {
        let mut resp = KeyResponse::consumed();

        let c = self.comp();
        let display = c.display();
        resp.marked = Some(MarkedText {
            text: display,
            dashed: c.submode == Submode::English,
        });

        // Candidates
        if !c.candidates.is_empty() {
            resp.candidates = CandidateAction::Show {
                surfaces: c.candidates.surfaces.clone(),
                selected: c.candidates.selected as u32,
            };
        }

        // Try auto-commit (only in sync mode; async mode handles it in receive_candidates)
        if !self.defer_candidates {
            if let Some(auto_resp) = self.try_auto_commit() {
                resp = auto_resp;
            }
        }

        resp
    }

    fn make_candidate_selection_response(&mut self) -> KeyResponse {
        let mut resp = KeyResponse::consumed();

        let c = self.comp();
        resp.marked = Some(MarkedText {
            text: c.display(),
            dashed: false,
        });
        resp.candidates = CandidateAction::Show {
            surfaces: c.candidates.surfaces.clone(),
            selected: c.candidates.selected as u32,
        };
        resp
    }
}

fn is_romaji_input(text: &str) -> bool {
    if text == "-" {
        return true;
    }
    match text.chars().next() {
        Some(c) => c.is_ascii_lowercase() || c.is_ascii_uppercase(),
        None => false,
    }
}

fn cyclic_index(current: usize, delta: i32, count: usize) -> usize {
    if count == 0 {
        return 0;
    }
    let c = current as i32;
    let n = count as i32;
    ((c + delta + n) % n) as usize
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dict::DictEntry;

    fn make_test_dict() -> TrieDictionary {
        let entries = vec![
            (
                "きょう".to_string(),
                vec![
                    DictEntry {
                        surface: "今日".to_string(),
                        cost: 3000,
                        left_id: 0,
                        right_id: 0,
                    },
                    DictEntry {
                        surface: "京".to_string(),
                        cost: 5000,
                        left_id: 0,
                        right_id: 0,
                    },
                ],
            ),
            (
                "は".to_string(),
                vec![DictEntry {
                    surface: "は".to_string(),
                    cost: 2000,
                    left_id: 0,
                    right_id: 0,
                }],
            ),
            (
                "いい".to_string(),
                vec![
                    DictEntry {
                        surface: "良い".to_string(),
                        cost: 3500,
                        left_id: 0,
                        right_id: 0,
                    },
                    DictEntry {
                        surface: "いい".to_string(),
                        cost: 4000,
                        left_id: 0,
                        right_id: 0,
                    },
                ],
            ),
            (
                "てんき".to_string(),
                vec![DictEntry {
                    surface: "天気".to_string(),
                    cost: 4000,
                    left_id: 0,
                    right_id: 0,
                }],
            ),
            (
                "い".to_string(),
                vec![DictEntry {
                    surface: "胃".to_string(),
                    cost: 6000,
                    left_id: 0,
                    right_id: 0,
                }],
            ),
            (
                "き".to_string(),
                vec![DictEntry {
                    surface: "木".to_string(),
                    cost: 4500,
                    left_id: 0,
                    right_id: 0,
                }],
            ),
            (
                "てん".to_string(),
                vec![DictEntry {
                    surface: "天".to_string(),
                    cost: 5000,
                    left_id: 0,
                    right_id: 0,
                }],
            ),
            (
                "わたし".to_string(),
                vec![DictEntry {
                    surface: "私".to_string(),
                    cost: 3000,
                    left_id: 0,
                    right_id: 0,
                }],
            ),
            (
                "です".to_string(),
                vec![DictEntry {
                    surface: "です".to_string(),
                    cost: 2500,
                    left_id: 0,
                    right_id: 0,
                }],
            ),
            (
                "ね".to_string(),
                vec![DictEntry {
                    surface: "ね".to_string(),
                    cost: 2000,
                    left_id: 0,
                    right_id: 0,
                }],
            ),
            (
                "。".to_string(),
                vec![DictEntry {
                    surface: "。".to_string(),
                    cost: 1000,
                    left_id: 0,
                    right_id: 0,
                }],
            ),
        ];
        TrieDictionary::from_entries(entries)
    }

    // Helper: simulate typing a string one character at a time
    fn type_string(session: &mut InputSession, s: &str) -> Vec<KeyResponse> {
        let mut responses = Vec::new();
        for ch in s.chars() {
            let text = ch.to_string();
            let resp = session.handle_key(0, &text, 0);
            responses.push(resp);
        }
        responses
    }

    // --- Basic romaji input ---

    #[test]
    fn test_romaji_input_ka() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);

        let resp = session.handle_key(0, "k", 0);
        assert!(resp.consumed);
        assert!(session.is_composing());

        let resp = session.handle_key(0, "a", 0);
        assert!(resp.consumed);
        // After "ka" → "か", marked text should be set
        assert!(resp.marked.is_some());
    }

    #[test]
    fn test_romaji_kyou() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);

        type_string(&mut session, "kyou");
        assert!(session.is_composing());
        assert_eq!(session.comp().kana, "きょう");
        assert!(session.comp().pending.is_empty());
    }

    #[test]
    fn test_romaji_sokuon() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);

        type_string(&mut session, "kka");
        assert_eq!(session.comp().kana, "っか");
    }

    // --- Backspace ---

    #[test]
    fn test_backspace_removes_pending() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);

        type_string(&mut session, "k"); // pending_romaji = "k"
        assert_eq!(session.comp().pending, "k");

        let resp = session.handle_key(key::BACKSPACE, "", 0);
        assert!(resp.consumed);
        assert!(!session.is_composing()); // back to idle (composition dropped)
    }

    #[test]
    fn test_backspace_removes_kana() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);

        type_string(&mut session, "ka"); // composedKana = "か"
        assert_eq!(session.comp().kana, "か");

        let resp = session.handle_key(key::BACKSPACE, "", 0);
        assert!(resp.consumed);
        assert!(!session.is_composing()); // back to idle (composition dropped)
    }

    #[test]
    fn test_backspace_partial() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);

        type_string(&mut session, "kak"); // "か" + pending "k"
        assert_eq!(session.comp().kana, "か");
        assert_eq!(session.comp().pending, "k");

        session.handle_key(key::BACKSPACE, "", 0);
        assert_eq!(session.comp().kana, "か");
        assert!(session.comp().pending.is_empty());
        assert!(session.is_composing());
    }

    // --- Escape ---

    #[test]
    fn test_escape_flushes() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);

        type_string(&mut session, "kyoun"); // "きょう" + pending "n"

        let resp = session.handle_key(key::ESCAPE, "", 0);
        assert!(resp.consumed);
        assert!(matches!(resp.candidates, CandidateAction::Hide));
        // After escape, kana is flushed (n → ん)
        assert_eq!(session.comp().kana, "きょうん");
        assert!(session.comp().pending.is_empty());
    }

    // --- Enter (commit) ---

    #[test]
    fn test_enter_commits_selected() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);

        type_string(&mut session, "kyou");
        assert!(!session.comp().candidates.is_empty());

        let resp = session.handle_key(key::ENTER, "", 0);
        assert!(resp.consumed);
        assert!(resp.commit.is_some());
        assert!(matches!(resp.candidates, CandidateAction::Hide));
        assert!(!session.is_composing());
    }

    // --- Space (candidate cycling) ---

    #[test]
    fn test_space_cycles_candidates() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);

        type_string(&mut session, "kyou");
        let initial_count = session.comp().candidates.surfaces.len();
        assert!(initial_count > 1);
        assert_eq!(session.comp().candidates.selected, 0);

        // First space jumps to index 1
        let resp = session.handle_key(key::SPACE, "", 0);
        assert!(resp.consumed);
        assert_eq!(session.comp().candidates.selected, 1);
        assert!(matches!(resp.candidates, CandidateAction::Show { .. }));

        // Second space goes to index 2
        let resp = session.handle_key(key::SPACE, "", 0);
        assert!(resp.consumed);
        assert_eq!(session.comp().candidates.selected, 2);
    }

    // --- Arrow keys ---

    #[test]
    fn test_arrow_keys_cycle() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);

        type_string(&mut session, "kyou");
        let count = session.comp().candidates.surfaces.len();
        assert!(count > 1);

        session.handle_key(key::DOWN, "", 0);
        assert_eq!(session.comp().candidates.selected, 1);

        session.handle_key(key::UP, "", 0);
        assert_eq!(session.comp().candidates.selected, 0);

        // Up from 0 wraps to last
        session.handle_key(key::UP, "", 0);
        assert_eq!(session.comp().candidates.selected, count - 1);
    }

    // --- Tab (submode toggle) ---

    #[test]
    fn test_tab_toggles_submode() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);

        assert_eq!(session.submode(), Submode::Japanese);
        session.handle_key(key::TAB, "", 0);
        assert_eq!(session.submode(), Submode::English);
        session.handle_key(key::TAB, "", 0);
        assert_eq!(session.submode(), Submode::Japanese);
    }

    #[test]
    fn test_english_submode_direct_input() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);

        session.handle_key(key::TAB, "", 0); // switch to English
        let resp = session.handle_key(0, "h", 0);
        assert!(resp.consumed);
        assert!(session.is_composing());
        assert_eq!(session.comp().kana, "h");
        assert!(resp.marked.as_ref().is_some_and(|m| m.dashed));

        let resp = session.handle_key(0, "i", 0);
        assert!(resp.consumed);
        assert_eq!(session.comp().kana, "hi");
    }

    // --- Modifier pass-through ---

    #[test]
    fn test_modifier_passthrough_idle() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);

        let resp = session.handle_key(0, "c", FLAG_HAS_MODIFIER);
        assert!(!resp.consumed);
    }

    #[test]
    fn test_modifier_passthrough_composing() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);

        type_string(&mut session, "kyou");
        assert!(session.is_composing());

        let resp = session.handle_key(0, "c", FLAG_HAS_MODIFIER);
        assert!(!resp.consumed);
        assert!(resp.commit.is_some()); // commits before passing through
        assert!(!session.is_composing());
    }

    // --- Eisu key ---

    #[test]
    fn test_eisu_switches_to_abc() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);

        let resp = session.handle_key(key::EISU, "", 0);
        assert!(resp.consumed);
        assert!(resp.side_effects.switch_to_abc);
    }

    #[test]
    fn test_eisu_commits_and_switches() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);

        type_string(&mut session, "kyou");
        assert!(session.is_composing());

        let resp = session.handle_key(key::EISU, "", 0);
        assert!(resp.consumed);
        assert!(resp.side_effects.switch_to_abc);
        assert!(resp.commit.is_some());
        assert!(!session.is_composing());
    }

    // --- Kana key ---

    #[test]
    fn test_kana_consumed() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);

        let resp = session.handle_key(key::KANA, "", 0);
        assert!(resp.consumed);
    }

    // --- Yen key (programmer mode) ---

    #[test]
    fn test_yen_programmer_mode() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);
        session.set_programmer_mode(true);

        let resp = session.handle_key(key::YEN, "¥", 0);
        assert!(resp.consumed);
        assert_eq!(resp.commit.as_deref(), Some("\\"));
    }

    #[test]
    fn test_yen_programmer_mode_composing() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);
        session.set_programmer_mode(true);

        type_string(&mut session, "kyou");
        let resp = session.handle_key(key::YEN, "¥", 0);
        assert!(resp.consumed);
        // Should commit current + add backslash
        assert!(resp.commit.is_some());
        let text = resp.commit.unwrap();
        assert!(text.ends_with('\\'));
    }

    #[test]
    fn test_yen_non_programmer_mode_passthrough() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);

        // Without programmer mode, ¥ in idle state should not be consumed (romaji check)
        let resp = session.handle_key(key::YEN, "¥", 0);
        assert!(!resp.consumed);
    }

    // --- Punctuation auto-commit ---

    #[test]
    fn test_punctuation_auto_commit() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);

        type_string(&mut session, "kyou");
        assert!(session.is_composing());

        // Type "." which is a romaji trie match for "。"
        let resp = session.handle_key(0, ".", 0);
        assert!(resp.consumed);
        // Should commit current state + append punctuation
        let text = resp.commit.unwrap();
        assert!(
            text.ends_with('。'),
            "commit should end with 。, got: {}",
            text
        );
    }

    // --- Commit (composedString for IMKit) ---

    #[test]
    fn test_commit_method() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);

        type_string(&mut session, "kyou");
        assert!(session.is_composing());

        let resp = session.commit();
        assert!(resp.commit.is_some());
        assert!(!session.is_composing());
    }

    // --- composed_string ---

    #[test]
    fn test_composed_string_idle() {
        let dict = make_test_dict();
        let session = InputSession::new(&dict, None, None);
        assert_eq!(session.composed_string(), "");
    }

    #[test]
    fn test_composed_string_composing() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);

        type_string(&mut session, "kyou");
        // composed_string should return the current display (best candidate)
        let cs = session.composed_string();
        assert!(!cs.is_empty());
    }

    // --- History recording ---

    #[test]
    fn test_history_recorded_on_commit() {
        let dict = make_test_dict();
        let history = UserHistory::new();
        let mut session = InputSession::new(&dict, None, Some(&history));

        type_string(&mut session, "kyou");
        session.handle_key(key::ENTER, "", 0);

        let records = session.take_history_records();
        assert!(!records.is_empty());
    }

    #[test]
    fn test_history_recorded_on_escape() {
        let dict = make_test_dict();
        let history = UserHistory::new();
        let mut session = InputSession::new(&dict, None, Some(&history));

        type_string(&mut session, "kyou");
        session.handle_key(key::ESCAPE, "", 0);

        let records = session.take_history_records();
        assert!(!records.is_empty());
        // Should record kana → kana
        assert_eq!(records[0][0].0, "きょう");
        assert_eq!(records[0][0].1, "きょう");
    }

    // --- Cyclic index ---

    #[test]
    fn test_cyclic_index() {
        assert_eq!(cyclic_index(0, 1, 3), 1);
        assert_eq!(cyclic_index(2, 1, 3), 0); // wrap
        assert_eq!(cyclic_index(0, -1, 3), 2); // wrap backwards
        assert_eq!(cyclic_index(0, 0, 0), 0); // empty
    }

    // --- is_romaji_input ---

    #[test]
    fn test_is_romaji_input() {
        assert!(is_romaji_input("a"));
        assert!(is_romaji_input("Z"));
        assert!(is_romaji_input("-"));
        assert!(!is_romaji_input("1"));
        assert!(!is_romaji_input("。"));
        assert!(!is_romaji_input(""));
    }

    // --- Boundary space (programmer mode) ---

    #[test]
    fn test_programmer_mode_boundary_space() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);
        session.set_programmer_mode(true);

        // Type Japanese, toggle to English
        type_string(&mut session, "kyou");
        let best = session.comp().candidates.surfaces[0].clone();
        session.handle_key(key::TAB, "", 0); // → English
                                             // Boundary space should be in display_prefix after crystallization
        assert!(session.comp().prefix.text.ends_with(' '));
        assert!(session.comp().prefix.has_boundary_space);
        // composed_kana should be cleared (crystallized into prefix)
        assert!(session.comp().kana.is_empty());

        // Toggle back without typing → space should be removed
        session.handle_key(key::TAB, "", 0); // → Japanese
        assert!(!session.comp().prefix.text.ends_with(' '));
        assert!(!session.comp().prefix.has_boundary_space);
        // Prefix should still contain the crystallized conversion (without space)
        assert_eq!(session.comp().prefix.text, best);
    }

    #[test]
    fn test_toggle_submode_preserves_conversion() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);

        // Type "kyou" → candidates include "今日" (Viterbi best)
        type_string(&mut session, "kyou");
        assert!(!session.comp().candidates.is_empty());
        let best = session.comp().candidates.surfaces[0].clone();

        // display() should return the Viterbi best
        assert_eq!(session.comp().display(), best);

        // Toggle to English — display must preserve the conversion, not revert to kana
        let resp = session.handle_key(key::TAB, "", 0);
        assert!(resp.consumed);
        assert!(resp.marked.as_ref().is_some_and(|m| m.dashed));
        let marked = resp.marked.unwrap().text;
        assert_eq!(
            marked, best,
            "toggle should preserve conversion, not revert to kana"
        );
        // Candidates are cleared after crystallization
        assert!(matches!(resp.candidates, CandidateAction::Hide));
        // Conversion should be crystallized into display_prefix
        assert_eq!(session.comp().prefix.text, best);
        assert!(session.comp().kana.is_empty());
    }

    // --- Mixed mode (Japanese + English) ---

    #[test]
    fn test_mixed_mode_commit() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);

        // Type "kyou" → "今日", then Tab to English, type "test", then Enter
        type_string(&mut session, "kyou");
        let best = session.comp().candidates.surfaces[0].clone();
        session.handle_key(key::TAB, "", 0); // → English
        type_string(&mut session, "test");

        // Marked text should show "今日test"
        let display = session.comp().display();
        assert_eq!(display, format!("{}test", best));

        // Commit should produce "今日test"
        let resp = session.handle_key(key::ENTER, "", 0);
        assert_eq!(resp.commit.as_deref(), Some(&format!("{}test", best)[..]));
        assert!(!session.is_composing());
    }

    #[test]
    fn test_mixed_mode_display() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);

        // Type Japanese → English → Japanese
        type_string(&mut session, "kyou");
        let best = session.comp().candidates.surfaces[0].clone();
        session.handle_key(key::TAB, "", 0); // → English
        type_string(&mut session, "hello");
        session.handle_key(key::TAB, "", 0); // → Japanese
        type_string(&mut session, "kyou");

        // Display should be "<best>hello<new_best>"
        let display = session.comp().display();
        assert!(
            display.starts_with(&best),
            "display should start with first conversion: got {}",
            display,
        );
        assert!(
            display.contains("hello"),
            "display should contain English segment: got {}",
            display,
        );
    }

    #[test]
    fn test_mixed_mode_backspace_into_prefix() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);

        // Type Japanese, toggle to English
        type_string(&mut session, "kyou");
        let best = session.comp().candidates.surfaces[0].clone();
        session.handle_key(key::TAB, "", 0); // → English
        type_string(&mut session, "ab");

        // Backspace twice to empty English segment
        session.handle_key(key::BACKSPACE, "", 0);
        session.handle_key(key::BACKSPACE, "", 0);
        assert!(session.comp().kana.is_empty());
        // display_prefix still has the frozen conversion
        assert_eq!(session.comp().prefix.text, best);

        // One more backspace deletes from prefix
        session.handle_key(key::BACKSPACE, "", 0);
        assert!(session.comp().prefix.text.len() < best.len());
    }

    // --- Space in English mode ---

    #[test]
    fn test_english_mode_space_literal() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);

        session.handle_key(key::TAB, "", 0); // → English
        type_string(&mut session, "hi");
        session.handle_key(key::SPACE, "", 0);
        assert_eq!(session.comp().kana, "hi ");
    }

    // --- Candidates are generated ---

    #[test]
    fn test_candidates_generated() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);

        type_string(&mut session, "kyou");
        assert!(!session.comp().candidates.is_empty());
        assert!(!session.comp().candidates.paths.is_empty());
    }

    // --- Non-romaji char in composing ---

    #[test]
    fn test_unrecognized_char_added_to_kana() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);

        type_string(&mut session, "ka"); // "か"
        session.handle_key(0, "1", 0); // unrecognized
        assert!(session.comp().kana.ends_with('1'));
    }

    // --- z-sequence ---

    #[test]
    fn test_z_sequence() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);

        // "z" is a prefix in the romaji trie, "zh" → "←"
        type_string(&mut session, "zh");
        assert_eq!(session.comp().kana, "←");
    }

    // --- Deferred auto-commit provisional candidates ---

    #[test]
    fn test_deferred_auto_commit_shows_provisional_candidates() {
        use crate::candidates::generate_candidates;

        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);
        session.set_defer_candidates(true);

        // Helper: complete one async candidate cycle.
        // Returns the response from receive_candidates (None if stale).
        fn complete_cycle(
            session: &mut InputSession,
            dict: &TrieDictionary,
        ) -> Option<KeyResponse> {
            let reading = session.comp().kana.clone();
            if reading.is_empty() {
                return None;
            }
            let cand = generate_candidates(dict, None, None, &reading, 20);
            session.receive_candidates(&reading, cand.surfaces, cand.paths)
        }

        // Build up "きょうはいいてんき" with async cycles after each romaji group.
        // Each cycle increments the stability counter (first segment = "きょう").
        type_string(&mut session, "kyou"); // "きょう"
        let r = complete_cycle(&mut session, &dict);
        assert!(r.is_some());
        assert!(r.unwrap().commit.is_none(), "no auto-commit yet");

        type_string(&mut session, "ha"); // "きょうは"
        let r = complete_cycle(&mut session, &dict);
        assert!(r.is_some());
        assert!(r.unwrap().commit.is_none(), "no auto-commit yet");

        type_string(&mut session, "ii"); // "きょうはいい"
        let r = complete_cycle(&mut session, &dict);
        assert!(r.is_some());
        assert!(
            r.unwrap().commit.is_none(),
            "no auto-commit yet (< 4 segments)"
        );

        type_string(&mut session, "tenki"); // "きょうはいいてんき"
        let r = complete_cycle(&mut session, &dict);
        let resp = r.expect("receive_candidates should return a response");

        // Auto-commit should fire: first segment committed, remaining shown
        assert!(
            resp.commit.is_some(),
            "auto-commit should produce commit_text"
        );
        assert!(
            matches!(resp.candidates, CandidateAction::Show { .. }),
            "deferred auto-commit should show provisional candidates (not hide)"
        );
        if let CandidateAction::Show { ref surfaces, .. } = resp.candidates {
            assert!(
                !surfaces.is_empty(),
                "deferred auto-commit should provide provisional candidates"
            );
        }
        // Async generation should still be requested for proper results
        assert!(
            resp.async_request.is_some(),
            "deferred auto-commit should request async candidate generation"
        );
        // Session state should also hold provisional candidates
        // so that candidate navigation works during the async phase.
        assert!(
            !session.comp().candidates.surfaces.is_empty(),
            "session should retain provisional candidates for navigation"
        );
    }

    // --- Predictive conversion mode ---

    #[test]
    fn test_predictive_mode_generates_candidates() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);
        session.set_conversion_mode(ConversionMode::Predictive);

        type_string(&mut session, "kyou");
        // Predictive mode uses Viterbi base + bigram chaining
        assert!(!session.comp().candidates.is_empty());
        // Without history, behaves like standard (Viterbi-based)
        assert!(!session.comp().candidates.paths.is_empty());
        // Kana should be present as fallback
        assert!(session
            .comp()
            .candidates
            .surfaces
            .contains(&"きょう".to_string()));
    }

    #[test]
    fn test_predictive_mode_tab_commits() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);
        session.set_conversion_mode(ConversionMode::Predictive);

        type_string(&mut session, "kyou");
        assert!(session.is_composing());

        let resp = session.handle_key(key::TAB, "", 0);
        assert!(resp.consumed);
        // Tab in Predictive mode commits (not toggles submode)
        assert!(resp.commit.is_some());
        assert!(!session.is_composing());
    }

    #[test]
    fn test_predictive_mode_space_cycles() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);
        session.set_conversion_mode(ConversionMode::Predictive);

        type_string(&mut session, "kyou");
        let count = session.comp().candidates.surfaces.len();
        assert!(count > 1);
        assert_eq!(session.comp().candidates.selected, 0);

        // Space cycles candidates in Predictive mode too
        session.handle_key(key::SPACE, "", 0);
        assert_eq!(session.comp().candidates.selected, 1);
    }

    #[test]
    fn test_predictive_mode_no_auto_commit() {
        use crate::candidates::generate_candidates;

        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);
        session.set_conversion_mode(ConversionMode::Predictive);
        session.set_defer_candidates(true);

        fn complete_cycle(
            session: &mut InputSession,
            dict: &TrieDictionary,
        ) -> Option<KeyResponse> {
            let reading = session.comp().kana.clone();
            if reading.is_empty() {
                return None;
            }
            let cand = generate_candidates(dict, None, None, &reading, 20);
            session.receive_candidates(&reading, cand.surfaces, cand.paths)
        }

        // Build up enough input that would trigger auto-commit in Standard mode
        type_string(&mut session, "kyou");
        complete_cycle(&mut session, &dict);
        type_string(&mut session, "ha");
        complete_cycle(&mut session, &dict);
        type_string(&mut session, "ii");
        complete_cycle(&mut session, &dict);
        type_string(&mut session, "tenki");
        let r = complete_cycle(&mut session, &dict);

        // In Predictive mode, auto-commit should NOT fire
        if let Some(resp) = r {
            assert!(
                resp.commit.is_none(),
                "predictive mode should not auto-commit"
            );
        }
    }

    #[test]
    fn test_predictive_mode_deferred_dispatch() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);
        session.set_conversion_mode(ConversionMode::Predictive);
        session.set_defer_candidates(true);

        // Type "ka" to trigger deferred candidate generation
        session.handle_key(0, "k", 0);
        let resp = session.handle_key(0, "a", 0);
        // Predictive mode uses prediction-specific generation (dispatch=1)
        if let Some(req) = resp.async_request {
            assert_eq!(
                req.candidate_dispatch, 1,
                "predictive uses prediction_only generation"
            );
        }
    }

    #[test]
    fn test_standard_mode_deferred_dispatch() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);
        session.set_conversion_mode(ConversionMode::Standard);
        session.set_defer_candidates(true);

        // Type "ka" one char at a time to capture deferred response
        session.handle_key(0, "k", 0);
        let resp = session.handle_key(0, "a", 0);
        if let Some(req) = resp.async_request {
            assert_eq!(req.candidate_dispatch, 0, "standard dispatch should be 0");
        }
    }

    #[test]
    fn test_conversion_mode_switch() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);

        // Default is Standard
        assert_eq!(session.conversion_mode, ConversionMode::Standard);

        // Switch to Predictive
        session.set_conversion_mode(ConversionMode::Predictive);
        assert_eq!(session.conversion_mode, ConversionMode::Predictive);

        type_string(&mut session, "kyou");
        // Tab should commit (Predictive behavior)
        let resp = session.handle_key(key::TAB, "", 0);
        assert!(resp.commit.is_some());
        assert!(!session.is_composing());

        // Switch back to Standard
        session.set_conversion_mode(ConversionMode::Standard);
        assert_eq!(session.conversion_mode, ConversionMode::Standard);

        type_string(&mut session, "kyou");
        // Tab should toggle submode (Standard behavior)
        let resp = session.handle_key(key::TAB, "", 0);
        assert!(resp.commit.is_none());
        assert!(session.is_composing());
        assert_eq!(session.comp().submode, Submode::English);
    }

    // --- GhostText mode ---

    #[test]
    fn test_ghosttext_tab_accepts_ghost() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);
        session.set_conversion_mode(ConversionMode::GhostText);

        // Simulate ghost text being received
        session.ghost_text = Some("ですね".to_string());
        session.ghost_generation = 1;

        // Tab should accept ghost text
        let resp = session.handle_key(key::TAB, "", 0);
        assert!(resp.consumed);
        assert_eq!(resp.commit.as_deref(), Some("ですね"));
        assert!(session.ghost_text.is_none());
    }

    #[test]
    fn test_ghosttext_tab_no_ghost_composing_commits() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);
        session.set_conversion_mode(ConversionMode::GhostText);

        // Type something (no ghost text)
        type_string(&mut session, "kyou");
        assert!(session.is_composing());

        // Tab commits in GhostText mode (like Predictive)
        let resp = session.handle_key(key::TAB, "", 0);
        assert!(resp.commit.is_some());
        assert!(!session.is_composing());
    }

    #[test]
    fn test_ghosttext_input_clears_ghost() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);
        session.set_conversion_mode(ConversionMode::GhostText);

        // Simulate ghost text
        session.ghost_text = Some("ですね".to_string());

        // Type a character → should clear ghost
        let resp = session.handle_key(0, "k", 0);
        assert!(resp.consumed);
        assert!(session.ghost_text.is_none());
        // Ghost clear signaled in response
        assert_eq!(resp.ghost_text.as_deref(), Some(""));
    }

    #[test]
    fn test_ghosttext_stale_generation_rejected() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);
        session.set_conversion_mode(ConversionMode::GhostText);
        session.ghost_generation = 5;

        // Stale generation
        let result = session.receive_ghost_text(3, "stale text".to_string());
        assert!(result.is_none());
        assert!(session.ghost_text.is_none());

        // Correct generation
        let result = session.receive_ghost_text(5, "correct text".to_string());
        assert!(result.is_some());
        assert_eq!(session.ghost_text.as_deref(), Some("correct text"));
    }

    #[test]
    fn test_ghosttext_rejected_while_composing() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);
        session.set_conversion_mode(ConversionMode::GhostText);
        session.ghost_generation = 1;

        type_string(&mut session, "kyou");
        assert!(session.is_composing());

        // Should reject ghost text while composing
        let result = session.receive_ghost_text(1, "text".to_string());
        assert!(result.is_none());
    }

    #[test]
    fn test_standard_mode_no_ghost() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);
        session.set_conversion_mode(ConversionMode::Standard);
        session.ghost_generation = 1;

        // Standard mode rejects ghost text
        let result = session.receive_ghost_text(1, "text".to_string());
        assert!(result.is_none());
    }

    #[test]
    fn test_ghosttext_commit_requests_ghost() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);
        session.set_conversion_mode(ConversionMode::GhostText);

        type_string(&mut session, "kyou");
        let resp = session.handle_key(key::ENTER, "", 0);
        assert!(resp.commit.is_some());
        // Should request ghost generation
        assert!(resp.ghost_request.is_some());
        let req = resp.ghost_request.unwrap();
        assert!(!req.context.is_empty());
        assert_eq!(req.generation, 1);
    }

    #[test]
    fn test_set_conversion_mode_ghosttext() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);

        session.set_conversion_mode(ConversionMode::GhostText);
        assert_eq!(session.conversion_mode, ConversionMode::GhostText);
        assert_eq!(session.conversion_mode.candidate_dispatch(), 2);
    }

    #[test]
    fn test_ghosttext_accept_then_requests_more() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);
        session.set_conversion_mode(ConversionMode::GhostText);

        // Simulate ghost text
        session.ghost_text = Some("ですね".to_string());
        session.ghost_generation = 1;

        // Accept ghost
        let resp = session.handle_key(key::TAB, "", 0);
        assert_eq!(resp.commit.as_deref(), Some("ですね"));
        // Should request another ghost generation
        assert!(resp.ghost_request.is_some());
        assert_eq!(resp.ghost_request.unwrap().generation, 2);
    }
}
