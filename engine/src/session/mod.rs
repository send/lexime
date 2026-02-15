pub(crate) mod types;

#[cfg(test)]
mod tests;

use tracing::debug_span;

use crate::candidates::CandidateResponse;
use crate::converter::{convert, ConvertedSegment};
use crate::dict::connection::ConnectionMatrix;
use crate::dict::TrieDictionary;
use crate::romaji::{convert_romaji, RomajiTrie, TrieLookupResult};
use crate::user_history::UserHistory;

pub use types::{
    AsyncCandidateRequest, AsyncGhostRequest, CandidateAction, ConversionMode, KeyResponse,
    MarkedText, SideEffects,
};

use types::{
    cyclic_index, is_romaji_input, key, Composition, SessionState, Submode, TabAction,
    FLAG_HAS_MODIFIER, FLAG_SHIFT, MAX_CANDIDATES, MAX_COMPOSED_KANA_LENGTH,
};

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

    // Accumulated committed text for neural context
    committed_context: String,
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

    /// Get the accumulated committed text for use as neural context.
    pub fn committed_context(&self) -> String {
        self.committed_context.clone()
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

        // Remove committed reading from kana.
        // Safety: starts_with check above guarantees the byte offset is a valid
        // UTF-8 boundary, but we use char-based slicing for extra safety.
        let c = self.comp();
        let skip_chars = committed_reading.chars().count();
        c.kana = c.kana.chars().skip(skip_chars).collect();
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

        // Accumulate committed text for neural context
        if let Some(ref committed) = resp.commit {
            self.committed_context.push_str(committed);
        }

        // GhostText mode: request ghost text generation after commit.
        // Use full committed_context (not just the latest commit) so the
        // neural model sees the complete preceding text.
        if self.conversion_mode == ConversionMode::GhostText && resp.commit.is_some() {
            self.ghost_generation += 1;
            resp.ghost_request = Some(AsyncGhostRequest {
                context: self.committed_context.clone(),
                generation: self.ghost_generation,
            });
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
        // Accumulate accepted ghost text into committed_context
        self.committed_context.push_str(&text);
        let mut resp = KeyResponse::consumed();
        resp.commit = Some(text);
        // After accepting ghost, request another generation with full context
        if self.conversion_mode == ConversionMode::GhostText {
            self.ghost_generation += 1;
            resp.ghost_request = Some(AsyncGhostRequest {
                context: self.committed_context.clone(),
                generation: self.ghost_generation,
            });
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
