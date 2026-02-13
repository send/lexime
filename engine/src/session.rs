use crate::candidates::{generate_candidates, CandidateResponse};
use crate::converter::ConvertedSegment;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Idle,
    Composing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Submode {
    Japanese,
    English,
}

/// Response from handle_key / commit, returned to the caller (Swift via FFI).
pub struct KeyResponse {
    pub consumed: bool,
    pub commit_text: Option<String>,
    pub marked_text: Option<String>,
    pub is_dashed_underline: bool,
    pub candidates: Vec<String>,
    pub selected_index: u32,
    pub show_candidates: bool,
    pub hide_candidates: bool,
    pub switch_to_abc: bool,
    pub save_history: bool,
    /// When true, the caller should generate candidates asynchronously
    /// using `candidate_reading` and feed them back via `receive_candidates`.
    pub needs_candidates: bool,
    /// The reading (composed_kana) to use for async candidate generation.
    pub candidate_reading: Option<String>,
}

impl KeyResponse {
    fn not_consumed() -> Self {
        Self {
            consumed: false,
            commit_text: None,
            marked_text: None,
            is_dashed_underline: false,
            candidates: Vec::new(),
            selected_index: 0,
            show_candidates: false,
            hide_candidates: false,
            switch_to_abc: false,
            save_history: false,
            needs_candidates: false,
            candidate_reading: None,
        }
    }

    fn consumed() -> Self {
        Self {
            consumed: true,
            ..Self::not_consumed()
        }
    }
}

/// Stateful IME session encapsulating all input processing logic.
pub struct InputSession<'a> {
    dict: &'a TrieDictionary,
    conn: Option<&'a ConnectionMatrix>,
    history: Option<&'a UserHistory>,

    // Input state
    state: State,
    submode: Submode,
    composed_kana: String,
    pending_romaji: String,

    // Candidate state
    candidates: Vec<String>,
    nbest_paths: Vec<Vec<ConvertedSegment>>,
    selected_index: usize,

    // Auto-commit state
    prev_first_seg_reading: Option<String>,
    first_seg_stable_count: usize,

    // Display state
    current_display: Option<String>,

    /// Frozen text from previous submode segments (e.g., Viterbi result before
    /// switching to English). Prepended to all display and commit output.
    display_prefix: String,

    // Settings
    programmer_mode: bool,
    did_insert_boundary_space: bool,
    /// When true, handle_key skips synchronous candidate generation and
    /// sets `needs_candidates` in the response for async generation by the caller.
    defer_candidates: bool,

    // History recording buffer
    history_records: Vec<Vec<(String, String)>>,
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
            state: State::Idle,
            submode: Submode::Japanese,
            composed_kana: String::new(),
            pending_romaji: String::new(),
            candidates: Vec::new(),
            nbest_paths: Vec::new(),
            selected_index: 0,
            prev_first_seg_reading: None,
            first_seg_stable_count: 0,
            current_display: None,
            display_prefix: String::new(),
            programmer_mode: false,
            did_insert_boundary_space: false,
            defer_candidates: false,
            history_records: Vec::new(),
        }
    }

    pub fn set_programmer_mode(&mut self, enabled: bool) {
        self.programmer_mode = enabled;
    }

    pub fn set_defer_candidates(&mut self, enabled: bool) {
        self.defer_candidates = enabled;
    }

    /// Temporarily set the history reference. Used by FFI to pass a lock guard.
    pub fn set_history(&mut self, history: Option<&'a UserHistory>) {
        self.history = history;
    }

    pub fn is_composing(&self) -> bool {
        self.state != State::Idle
    }

    pub fn composed_string(&self) -> &str {
        if let Some(ref display) = self.current_display {
            display
        } else {
            // Fallback: composedKana + pendingRomaji
            // Since we can't return a concatenation by reference,
            // we rely on current_display always being set when composing.
            // In idle state, return empty.
            ""
        }
    }

    /// Process a key event. Returns a KeyResponse describing what the caller should do.
    ///
    /// `flags`: bit 0 = shift, bit 1 = has_modifier (Cmd/Ctrl/Opt)
    pub fn handle_key(&mut self, key_code: u16, text: &str, flags: u8) -> KeyResponse {
        let has_modifier = flags & FLAG_HAS_MODIFIER != 0;
        let has_shift = flags & FLAG_SHIFT != 0;

        // Eisu key → commit if composing, switch to ABC
        if key_code == key::EISU {
            let mut resp = if self.is_composing() {
                self.commit_current_state()
            } else {
                KeyResponse::consumed()
            };
            resp.switch_to_abc = true;
            return resp;
        }

        // Kana key → already in Japanese mode, consume
        if key_code == key::KANA {
            return KeyResponse::consumed();
        }

        // Modifier keys (Cmd, Ctrl, etc.) — commit first, then pass through
        if has_modifier {
            if self.is_composing() {
                let mut resp = self.commit_current_state();
                resp.consumed = false;
                return resp;
            }
            return KeyResponse::not_consumed();
        }

        // Programmer mode: ¥ key → insert backslash
        if key_code == key::YEN && self.programmer_mode && !has_shift {
            let mut resp = if self.is_composing() {
                self.commit_current_state()
            } else {
                KeyResponse::consumed()
            };
            // Append backslash to commit_text
            match resp.commit_text {
                Some(ref mut t) => t.push('\\'),
                None => resp.commit_text = Some("\\".to_string()),
            }
            return resp;
        }

        match self.state {
            State::Idle => self.handle_idle(key_code, text),
            State::Composing => self.handle_composing(key_code, text),
        }
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
        // Tab — toggle submode
        if key_code == key::TAB {
            return self.toggle_submode();
        }

        // English submode: add characters directly
        if self.submode == Submode::English {
            if let Some(scalar) = text.chars().next() {
                let val = scalar as u32;
                if (0x20..0x7F).contains(&val) {
                    self.state = State::Composing;
                    self.did_insert_boundary_space = false;
                    self.composed_kana.push_str(text);
                    return self.make_marked_text_response();
                }
            }
            return KeyResponse::not_consumed();
        }

        // Romaji input
        if is_romaji_input(text) {
            self.state = State::Composing;
            return self.append_and_convert(&text.to_lowercase());
        }

        // Direct trie match for non-romaji chars (punctuation)
        let trie = RomajiTrie::global();
        match trie.lookup(text) {
            TrieLookupResult::Exact(_) | TrieLookupResult::ExactAndPrefix(_) => {
                self.state = State::Composing;
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
                if self.submode == Submode::English {
                    let mut resp = self.commit_composed();
                    resp.hide_candidates = true;
                    resp
                } else {
                    // Lazy generate: ensure candidates are available for commit
                    if self.candidates.is_empty() && !self.composed_kana.is_empty() {
                        self.update_candidates();
                    }
                    self.commit_current_state()
                }
            }

            key::SPACE => {
                if self.submode == Submode::English {
                    self.composed_kana.push(' ');
                    self.make_marked_text_response()
                } else {
                    // Lazy generate: ensure candidates for Space cycling
                    if self.candidates.is_empty() && !self.composed_kana.is_empty() {
                        self.update_candidates();
                    }
                    if !self.candidates.is_empty() {
                        if self.selected_index == 0 && self.candidates.len() > 1 {
                            self.selected_index = 1;
                        } else {
                            self.selected_index =
                                cyclic_index(self.selected_index, 1, self.candidates.len());
                        }
                        self.make_candidate_selection_response()
                    } else {
                        KeyResponse::consumed()
                    }
                }
            }

            key::DOWN => {
                // Lazy generate: ensure candidates for arrow cycling
                if self.candidates.is_empty() && !self.composed_kana.is_empty() {
                    self.update_candidates();
                }
                if !self.candidates.is_empty() {
                    self.selected_index =
                        cyclic_index(self.selected_index, 1, self.candidates.len());
                    self.make_candidate_selection_response()
                } else {
                    KeyResponse::consumed()
                }
            }

            key::UP => {
                // Lazy generate: ensure candidates for arrow cycling
                if self.candidates.is_empty() && !self.composed_kana.is_empty() {
                    self.update_candidates();
                }
                if !self.candidates.is_empty() {
                    self.selected_index =
                        cyclic_index(self.selected_index, -1, self.candidates.len());
                    self.make_candidate_selection_response()
                } else {
                    KeyResponse::consumed()
                }
            }

            key::TAB => self.toggle_submode(),

            key::BACKSPACE => self.handle_backspace(),

            key::ESCAPE => {
                self.flush();
                if self.submode == Submode::Japanese && !self.composed_kana.is_empty() {
                    self.record_history(self.composed_kana.clone(), self.composed_kana.clone());
                }
                self.candidates.clear();
                self.selected_index = 0;
                let mut resp = KeyResponse::consumed();
                resp.hide_candidates = true;
                if !self.history_records.is_empty() {
                    resp.save_history = true;
                }
                // Escape: IMKit will call commitComposition after, so we just signal flush.
                // Set marked text so composedString is correct for the auto-commit.
                let display = format!(
                    "{}{}{}",
                    self.display_prefix, self.composed_kana, self.pending_romaji
                );
                self.current_display = Some(display);
                resp
            }

            _ => self.handle_composing_text(text),
        }
    }

    fn handle_composing_text(&mut self, text: &str) -> KeyResponse {
        // English submode: add characters directly
        if self.submode == Submode::English {
            if let Some(scalar) = text.chars().next() {
                let val = scalar as u32;
                if (0x20..0x7F).contains(&val) {
                    self.did_insert_boundary_space = false;
                    self.composed_kana.push_str(text);
                    return self.make_marked_text_response();
                }
            }
            return KeyResponse::consumed();
        }

        // z-sequences: composing 中、pendingRomaji + text が trie にマッチする場合
        if !self.pending_romaji.is_empty() {
            let candidate = format!("{}{}", self.pending_romaji, text);
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
            if self.selected_index > 0 && self.selected_index < self.candidates.len() {
                let mut commit_resp = self.commit_current_state();
                self.state = State::Composing;
                let append_resp = self.append_and_convert(&text.to_lowercase());
                // Merge: commit from first, marked+candidates from second
                commit_resp.marked_text = append_resp.marked_text;
                commit_resp.candidates = append_resp.candidates;
                commit_resp.selected_index = append_resp.selected_index;
                commit_resp.show_candidates = append_resp.show_candidates;
                commit_resp.hide_candidates = append_resp.hide_candidates;
                commit_resp.is_dashed_underline = append_resp.is_dashed_underline;
                return commit_resp;
            }
            return self.append_and_convert(&text.to_lowercase());
        }

        // Direct trie match for non-romaji chars (punctuation auto-commit)
        if !is_romaji_input(text) {
            let trie = RomajiTrie::global();
            match trie.lookup(text) {
                TrieLookupResult::Exact(_) | TrieLookupResult::ExactAndPrefix(_) => {
                    let mut resp = self.commit_current_state();
                    // Convert punctuation
                    let result = convert_romaji("", text, true);
                    if !result.composed_kana.is_empty() {
                        match resp.commit_text {
                            Some(ref mut t) => t.push_str(&result.composed_kana),
                            None => resp.commit_text = Some(result.composed_kana),
                        }
                    }
                    return resp;
                }
                _ => {}
            }
        }

        // Unrecognized non-romaji character — add to composedKana
        self.composed_kana.push_str(text);
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
        if self.composed_kana.len() >= MAX_COMPOSED_KANA_LENGTH {
            let mut resp = self.commit_composed();
            self.state = State::Composing;
            self.pending_romaji.push_str(input);
            self.drain_pending(false);
            let sub_resp = if self.defer_candidates {
                self.make_deferred_candidates_response()
            } else {
                if self.pending_romaji.is_empty() {
                    self.update_candidates();
                }
                self.make_marked_text_and_candidates_response()
            };
            resp.marked_text = sub_resp.marked_text;
            resp.candidates = sub_resp.candidates;
            resp.selected_index = sub_resp.selected_index;
            resp.show_candidates = sub_resp.show_candidates;
            resp.hide_candidates = sub_resp.hide_candidates;
            resp.is_dashed_underline = sub_resp.is_dashed_underline;
            resp.needs_candidates = sub_resp.needs_candidates;
            resp.candidate_reading = sub_resp.candidate_reading;
            return resp;
        }

        self.did_insert_boundary_space = false;
        self.pending_romaji.push_str(input);
        self.drain_pending(false);

        if self.defer_candidates {
            if self.pending_romaji.is_empty() {
                // Kana resolved — defer candidate generation to caller
                self.make_deferred_candidates_response()
            } else {
                // Pending romaji: show kana + pending, no candidates needed yet
                self.make_marked_text_response()
            }
        } else {
            // Sync mode: generate candidates immediately when romaji resolves
            if self.pending_romaji.is_empty() {
                self.update_candidates();
            }
            self.make_marked_text_and_candidates_response()
        }
    }

    fn drain_pending(&mut self, force: bool) {
        let result = convert_romaji(&self.composed_kana, &self.pending_romaji, force);
        self.composed_kana = result.composed_kana;
        self.pending_romaji = result.pending_romaji;
    }

    fn flush(&mut self) {
        self.drain_pending(true);
    }

    // -----------------------------------------------------------------------
    // Candidate generation
    // -----------------------------------------------------------------------

    fn update_candidates(&mut self) {
        self.selected_index = 0;

        if self.composed_kana.is_empty() {
            self.candidates.clear();
            self.nbest_paths.clear();
            self.prev_first_seg_reading = None;
            self.first_seg_stable_count = 0;
            return;
        }

        let CandidateResponse { surfaces, paths } = generate_candidates(
            self.dict,
            self.conn,
            self.history,
            &self.composed_kana,
            MAX_CANDIDATES,
        );
        self.candidates = surfaces;
        self.nbest_paths = paths;

        self.track_segment_stability();
    }

    /// Build a response that defers candidate generation to the caller.
    /// Clears stale internal candidates (for correct lazy generation on Space/Enter),
    /// shows kana as marked text, and sets `needs_candidates` for async generation.
    /// Does NOT hide the candidate panel — old candidates stay visible until
    /// the async result replaces them, avoiding a visible hide→show flash.
    fn make_deferred_candidates_response(&mut self) -> KeyResponse {
        self.candidates.clear();
        self.nbest_paths.clear();
        self.selected_index = 0;
        // Do NOT reset prev_first_seg_reading / first_seg_stable_count here.
        // Stability tracking is updated only when fresh candidates arrive
        // (in receive_candidates), allowing the count to accumulate across keystrokes.
        let mut resp = self.make_marked_text_response();
        if !self.composed_kana.is_empty() {
            resp.needs_candidates = true;
            resp.candidate_reading = Some(self.composed_kana.clone());
        }
        resp
    }

    /// Receive asynchronously generated candidates and update session state.
    /// Returns `None` if the reading is stale (composed_kana has changed).
    pub fn receive_candidates(
        &mut self,
        reading: &str,
        surfaces: Vec<String>,
        paths: Vec<Vec<ConvertedSegment>>,
    ) -> Option<KeyResponse> {
        // Stale check: reading must match current state
        if reading != self.composed_kana
            || self.state != State::Composing
            || self.submode != Submode::Japanese
        {
            return None;
        }

        self.candidates = surfaces;
        self.nbest_paths = paths;
        self.selected_index = 0;
        self.track_segment_stability();

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

    fn track_segment_stability(&mut self) {
        let best_path = match self.nbest_paths.first() {
            Some(path) if path.len() >= 2 => path,
            _ => {
                self.prev_first_seg_reading = None;
                self.first_seg_stable_count = 0;
                return;
            }
        };

        let first_reading = &best_path[0].reading;
        if Some(first_reading) == self.prev_first_seg_reading.as_ref() {
            self.first_seg_stable_count += 1;
        } else {
            self.prev_first_seg_reading = Some(first_reading.clone());
            self.first_seg_stable_count = 1;
        }
    }

    fn try_auto_commit(&mut self) -> Option<KeyResponse> {
        if self.first_seg_stable_count < 3 {
            return None;
        }
        let best_path = self.nbest_paths.first()?;
        if best_path.len() < 4 {
            return None;
        }
        if self.selected_index != 0 {
            return None;
        }
        if !self.pending_romaji.is_empty() {
            return None;
        }

        // Count how many segments to commit (group consecutive ASCII)
        let mut commit_count = 1;
        if best_path[0].surface.is_ascii() {
            while commit_count < best_path.len() - 1 && best_path[commit_count].surface.is_ascii() {
                commit_count += 1;
            }
        }

        let segments: Vec<&ConvertedSegment> = best_path[0..commit_count].iter().collect();
        let committed_reading: String = segments.iter().map(|s| s.reading.as_str()).collect();
        let committed_surface: String = segments.iter().map(|s| s.surface.as_str()).collect();

        if !self.composed_kana.starts_with(&committed_reading) {
            return None;
        }

        // Record to history
        if committed_surface != committed_reading {
            let pairs: Vec<(String, String)> =
                vec![(committed_reading.clone(), committed_surface.clone())];
            self.history_records.push(pairs);
        }
        if commit_count > 1 {
            let seg_pairs: Vec<(String, String)> = segments
                .iter()
                .map(|s| (s.reading.clone(), s.surface.clone()))
                .collect();
            self.history_records.push(seg_pairs);
        }

        // Remove committed reading from composed_kana
        self.composed_kana = self.composed_kana[committed_reading.len()..].to_string();
        self.prev_first_seg_reading = None;
        self.first_seg_stable_count = 0;

        // Include display_prefix in the committed text, then clear it
        let prefix = std::mem::take(&mut self.display_prefix);
        let mut resp = KeyResponse::consumed();
        resp.commit_text = Some(format!("{}{}", prefix, committed_surface));
        resp.save_history = true;

        if self.composed_kana.is_empty() {
            self.candidates.clear();
            self.nbest_paths.clear();
            resp.hide_candidates = true;
            resp.marked_text = Some(String::new());
            self.current_display = Some(String::new());
        } else if self.defer_candidates {
            // Async mode: show kana, request async candidate generation
            self.candidates.clear();
            self.nbest_paths.clear();
            self.selected_index = 0;
            let display = format!("{}{}", self.composed_kana, self.pending_romaji);
            self.current_display = Some(display.clone());
            resp.marked_text = Some(display);
            resp.hide_candidates = true;
            resp.needs_candidates = true;
            resp.candidate_reading = Some(self.composed_kana.clone());
        } else {
            // Sync mode: re-generate candidates for remaining input
            let display = format!("{}{}", self.composed_kana, self.pending_romaji);
            self.current_display = Some(display.clone());
            resp.marked_text = Some(display);
            resp.is_dashed_underline = self.submode == Submode::English;
            self.update_candidates();
            if let Some(best) = self.candidates.first() {
                let best_display = best.clone();
                self.current_display = Some(best_display.clone());
                resp.marked_text = Some(best_display);
            }
            resp.candidates.clone_from(&self.candidates);
            resp.selected_index = self.selected_index as u32;
            resp.show_candidates = !self.candidates.is_empty();
        }

        Some(resp)
    }

    // -----------------------------------------------------------------------
    // Commit helpers
    // -----------------------------------------------------------------------

    fn commit_composed(&mut self) -> KeyResponse {
        let mut resp = KeyResponse::consumed();
        let text = format!("{}{}", self.display_prefix, self.composed_kana);
        if !text.is_empty() {
            resp.commit_text = Some(text);
        } else {
            resp.marked_text = Some(String::new());
        }
        self.reset_state();
        resp
    }

    fn commit_current_state(&mut self) -> KeyResponse {
        match self.state {
            State::Idle => KeyResponse::consumed(),
            State::Composing => {
                let mut resp = KeyResponse::consumed();
                resp.hide_candidates = true;
                self.flush();

                let prefix = std::mem::take(&mut self.display_prefix);

                if self.selected_index < self.candidates.len() {
                    let reading = self.composed_kana.clone();
                    let surface = self.candidates[self.selected_index].clone();

                    self.record_history(reading, surface.clone());
                    resp.save_history = true;
                    resp.commit_text = Some(format!("{}{}", prefix, surface));
                } else if !self.composed_kana.is_empty() || !prefix.is_empty() {
                    resp.commit_text = Some(format!("{}{}", prefix, self.composed_kana));
                } else {
                    resp.marked_text = Some(String::new());
                }

                self.reset_state();
                resp
            }
        }
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
            .nbest_paths
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
        self.composed_kana.clear();
        self.pending_romaji.clear();
        self.nbest_paths.clear();
        self.candidates.clear();
        self.selected_index = 0;
        self.current_display = None;
        self.display_prefix.clear();
        self.prev_first_seg_reading = None;
        self.first_seg_stable_count = 0;
        self.submode = Submode::Japanese;
        self.did_insert_boundary_space = false;
        self.state = State::Idle;
    }

    // -----------------------------------------------------------------------
    // Backspace
    // -----------------------------------------------------------------------

    fn handle_backspace(&mut self) -> KeyResponse {
        if !self.pending_romaji.is_empty() {
            self.pending_romaji.pop();
        } else if !self.composed_kana.is_empty() {
            self.composed_kana.pop();
        } else if !self.display_prefix.is_empty() {
            // Delete from the frozen prefix
            self.display_prefix.pop();
        }

        let all_empty = self.composed_kana.is_empty()
            && self.pending_romaji.is_empty()
            && self.display_prefix.is_empty();

        if all_empty {
            let mut resp = KeyResponse::consumed();
            resp.hide_candidates = true;
            resp.marked_text = Some(String::new());
            self.current_display = Some(String::new());
            self.state = State::Idle;
            self.candidates.clear();
            self.nbest_paths.clear();
            self.selected_index = 0;
            self.submode = Submode::Japanese;
            resp
        } else if self.composed_kana.is_empty() && self.pending_romaji.is_empty() {
            // Current segment is empty but display_prefix has content
            self.candidates.clear();
            self.nbest_paths.clear();
            self.selected_index = 0;
            let display = self.display_prefix.clone();
            self.current_display = Some(display.clone());
            let mut resp = KeyResponse::consumed();
            resp.marked_text = Some(display);
            resp.hide_candidates = true;
            resp.is_dashed_underline = self.submode == Submode::English;
            resp
        } else if self.defer_candidates && self.submode == Submode::Japanese {
            self.make_deferred_candidates_response()
        } else {
            if self.submode == Submode::Japanese {
                self.update_candidates();
            }
            self.make_marked_text_and_candidates_response()
        }
    }

    // -----------------------------------------------------------------------
    // Submode toggle
    // -----------------------------------------------------------------------

    fn toggle_submode(&mut self) -> KeyResponse {
        let new_submode = match self.submode {
            Submode::Japanese => Submode::English,
            Submode::English => Submode::Japanese,
        };

        // Flush pending romaji before switching
        if !self.pending_romaji.is_empty() {
            self.flush();
        }

        // Undo boundary space if nothing was typed since the last toggle
        let undid_boundary_space =
            self.did_insert_boundary_space && self.display_prefix.ends_with(' ');
        if undid_boundary_space {
            self.display_prefix.pop();
            self.did_insert_boundary_space = false;
        }

        // Crystallize the current segment into display_prefix.
        if self.is_composing() {
            match self.submode {
                Submode::Japanese => {
                    // Freeze the Viterbi result (or kana if no candidates)
                    let frozen = if self.selected_index < self.candidates.len() {
                        let reading = self.composed_kana.clone();
                        let surface = self.candidates[self.selected_index].clone();
                        self.record_history(reading, surface.clone());
                        surface
                    } else {
                        self.composed_kana.clone()
                    };
                    self.display_prefix.push_str(&frozen);
                }
                Submode::English => {
                    // English text goes directly into prefix
                    self.display_prefix.push_str(&self.composed_kana);
                }
            }
            // Clear the current segment for the new submode
            self.composed_kana.clear();
            self.pending_romaji.clear();
            self.candidates.clear();
            self.nbest_paths.clear();
            self.selected_index = 0;
            self.prev_first_seg_reading = None;
            self.first_seg_stable_count = 0;
        }

        // Programmer mode: insert space at submode boundary
        self.did_insert_boundary_space = false;
        if self.programmer_mode && !self.display_prefix.is_empty() {
            if let Some(last) = self.display_prefix.chars().last() {
                let last_is_ascii = last.is_ascii();
                let should_insert = (self.submode == Submode::Japanese
                    && new_submode == Submode::English
                    && !last_is_ascii)
                    || (self.submode == Submode::English
                        && new_submode == Submode::Japanese
                        && last_is_ascii
                        && last != ' ');
                if should_insert {
                    self.display_prefix.push(' ');
                    self.did_insert_boundary_space = true;
                }
            }
        }

        self.submode = new_submode;

        // If we have a display_prefix, we're still composing
        if !self.display_prefix.is_empty() {
            self.state = State::Composing;
        }

        let display = format!("{}{}", self.display_prefix, self.composed_kana);
        self.current_display = Some(display.clone());
        let mut resp = KeyResponse::consumed();
        if !display.is_empty() {
            resp.marked_text = Some(display);
        }
        resp.is_dashed_underline = self.submode == Submode::English;
        resp.hide_candidates = true;
        if !self.history_records.is_empty() {
            resp.save_history = true;
        }
        resp
    }

    // -----------------------------------------------------------------------
    // Response builders
    // -----------------------------------------------------------------------

    fn make_marked_text_response(&mut self) -> KeyResponse {
        let display = format!(
            "{}{}{}",
            self.display_prefix, self.composed_kana, self.pending_romaji
        );
        self.current_display = Some(display.clone());

        let mut resp = KeyResponse::consumed();
        resp.marked_text = Some(display);
        resp.is_dashed_underline = self.submode == Submode::English;
        resp
    }

    fn make_marked_text_and_candidates_response(&mut self) -> KeyResponse {
        let mut resp = KeyResponse::consumed();

        // Set marked text: use best candidate if available, else kana + pending
        let segment_display = if self.submode == Submode::Japanese {
            if let Some(best) = self.candidates.first() {
                best.clone()
            } else {
                format!("{}{}", self.composed_kana, self.pending_romaji)
            }
        } else {
            format!("{}{}", self.composed_kana, self.pending_romaji)
        };
        let display = format!("{}{}", self.display_prefix, segment_display);
        self.current_display = Some(display.clone());
        resp.marked_text = Some(display);
        resp.is_dashed_underline = self.submode == Submode::English;

        // Candidates
        resp.candidates.clone_from(&self.candidates);
        resp.selected_index = self.selected_index as u32;
        resp.show_candidates = !self.candidates.is_empty();

        // Try auto-commit (only in sync mode; async mode handles it in receive_candidates)
        if !self.defer_candidates {
            if let Some(auto_resp) = self.try_auto_commit() {
                resp.commit_text = auto_resp.commit_text;
                resp.marked_text = auto_resp.marked_text;
                resp.candidates = auto_resp.candidates;
                resp.selected_index = auto_resp.selected_index;
                resp.show_candidates = auto_resp.show_candidates;
                resp.hide_candidates = auto_resp.hide_candidates;
                resp.save_history = auto_resp.save_history;
            }
        }

        resp
    }

    fn make_candidate_selection_response(&mut self) -> KeyResponse {
        let mut resp = KeyResponse::consumed();

        // Update marked text to selected candidate
        if self.selected_index < self.candidates.len() {
            let display = format!(
                "{}{}",
                self.display_prefix, self.candidates[self.selected_index]
            );
            self.current_display = Some(display.clone());
            resp.marked_text = Some(display);
        }

        resp.candidates.clone_from(&self.candidates);
        resp.selected_index = self.selected_index as u32;
        resp.show_candidates = true;
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
        assert!(resp.marked_text.is_some());
    }

    #[test]
    fn test_romaji_kyou() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);

        type_string(&mut session, "kyou");
        assert!(session.is_composing());
        assert_eq!(session.composed_kana, "きょう");
        assert!(session.pending_romaji.is_empty());
    }

    #[test]
    fn test_romaji_sokuon() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);

        type_string(&mut session, "kka");
        assert_eq!(session.composed_kana, "っか");
    }

    // --- Backspace ---

    #[test]
    fn test_backspace_removes_pending() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);

        type_string(&mut session, "k"); // pending_romaji = "k"
        assert_eq!(session.pending_romaji, "k");

        let resp = session.handle_key(key::BACKSPACE, "", 0);
        assert!(resp.consumed);
        assert!(session.pending_romaji.is_empty());
        assert!(!session.is_composing()); // back to idle
    }

    #[test]
    fn test_backspace_removes_kana() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);

        type_string(&mut session, "ka"); // composedKana = "か"
        assert_eq!(session.composed_kana, "か");

        let resp = session.handle_key(key::BACKSPACE, "", 0);
        assert!(resp.consumed);
        assert!(session.composed_kana.is_empty());
        assert!(!session.is_composing()); // back to idle
    }

    #[test]
    fn test_backspace_partial() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);

        type_string(&mut session, "kak"); // "か" + pending "k"
        assert_eq!(session.composed_kana, "か");
        assert_eq!(session.pending_romaji, "k");

        session.handle_key(key::BACKSPACE, "", 0);
        assert_eq!(session.composed_kana, "か");
        assert!(session.pending_romaji.is_empty());
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
        assert!(resp.hide_candidates);
        // After escape, kana is flushed (n → ん)
        assert_eq!(session.composed_kana, "きょうん");
        assert!(session.pending_romaji.is_empty());
    }

    // --- Enter (commit) ---

    #[test]
    fn test_enter_commits_selected() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);

        type_string(&mut session, "kyou");
        assert!(!session.candidates.is_empty());

        let resp = session.handle_key(key::ENTER, "", 0);
        assert!(resp.consumed);
        assert!(resp.commit_text.is_some());
        assert!(resp.hide_candidates);
        assert!(!session.is_composing());
    }

    // --- Space (candidate cycling) ---

    #[test]
    fn test_space_cycles_candidates() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);

        type_string(&mut session, "kyou");
        let initial_count = session.candidates.len();
        assert!(initial_count > 1);
        assert_eq!(session.selected_index, 0);

        // First space jumps to index 1
        let resp = session.handle_key(key::SPACE, "", 0);
        assert!(resp.consumed);
        assert_eq!(session.selected_index, 1);
        assert!(resp.show_candidates);

        // Second space goes to index 2
        let resp = session.handle_key(key::SPACE, "", 0);
        assert!(resp.consumed);
        assert_eq!(session.selected_index, 2);
    }

    // --- Arrow keys ---

    #[test]
    fn test_arrow_keys_cycle() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);

        type_string(&mut session, "kyou");
        let count = session.candidates.len();
        assert!(count > 1);

        session.handle_key(key::DOWN, "", 0);
        assert_eq!(session.selected_index, 1);

        session.handle_key(key::UP, "", 0);
        assert_eq!(session.selected_index, 0);

        // Up from 0 wraps to last
        session.handle_key(key::UP, "", 0);
        assert_eq!(session.selected_index, count - 1);
    }

    // --- Tab (submode toggle) ---

    #[test]
    fn test_tab_toggles_submode() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);

        assert_eq!(session.submode, Submode::Japanese);
        session.handle_key(key::TAB, "", 0);
        assert_eq!(session.submode, Submode::English);
        session.handle_key(key::TAB, "", 0);
        assert_eq!(session.submode, Submode::Japanese);
    }

    #[test]
    fn test_english_submode_direct_input() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);

        session.handle_key(key::TAB, "", 0); // switch to English
        let resp = session.handle_key(0, "h", 0);
        assert!(resp.consumed);
        assert!(session.is_composing());
        assert_eq!(session.composed_kana, "h");
        assert!(resp.is_dashed_underline);

        let resp = session.handle_key(0, "i", 0);
        assert!(resp.consumed);
        assert_eq!(session.composed_kana, "hi");
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
        assert!(resp.commit_text.is_some()); // commits before passing through
        assert!(!session.is_composing());
    }

    // --- Eisu key ---

    #[test]
    fn test_eisu_switches_to_abc() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);

        let resp = session.handle_key(key::EISU, "", 0);
        assert!(resp.consumed);
        assert!(resp.switch_to_abc);
    }

    #[test]
    fn test_eisu_commits_and_switches() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);

        type_string(&mut session, "kyou");
        assert!(session.is_composing());

        let resp = session.handle_key(key::EISU, "", 0);
        assert!(resp.consumed);
        assert!(resp.switch_to_abc);
        assert!(resp.commit_text.is_some());
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
        assert_eq!(resp.commit_text.as_deref(), Some("\\"));
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
        assert!(resp.commit_text.is_some());
        let text = resp.commit_text.unwrap();
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
        let text = resp.commit_text.unwrap();
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
        assert!(resp.commit_text.is_some());
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
        let best = session.candidates[0].clone();
        session.handle_key(key::TAB, "", 0); // → English
                                             // Boundary space should be in display_prefix after crystallization
        assert!(session.display_prefix.ends_with(' '));
        assert!(session.did_insert_boundary_space);
        // composed_kana should be cleared (crystallized into prefix)
        assert!(session.composed_kana.is_empty());

        // Toggle back without typing → space should be removed
        session.handle_key(key::TAB, "", 0); // → Japanese
        assert!(!session.display_prefix.ends_with(' '));
        assert!(!session.did_insert_boundary_space);
        // Prefix should still contain the crystallized conversion (without space)
        assert_eq!(session.display_prefix, best);
    }

    #[test]
    fn test_toggle_submode_preserves_conversion() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);

        // Type "kyou" → candidates include "今日" (Viterbi best)
        type_string(&mut session, "kyou");
        assert!(!session.candidates.is_empty());
        let best = session.candidates[0].clone();

        // current_display should be the Viterbi best
        assert_eq!(session.current_display.as_deref(), Some(best.as_str()));

        // Toggle to English — display must preserve the conversion, not revert to kana
        let resp = session.handle_key(key::TAB, "", 0);
        assert!(resp.consumed);
        assert!(resp.is_dashed_underline);
        let marked = resp.marked_text.unwrap();
        assert_eq!(
            marked, best,
            "toggle should preserve conversion, not revert to kana"
        );
        // Candidates are cleared after crystallization
        assert!(resp.hide_candidates);
        // Conversion should be crystallized into display_prefix
        assert_eq!(session.display_prefix, best);
        assert!(session.composed_kana.is_empty());
    }

    // --- Mixed mode (Japanese + English) ---

    #[test]
    fn test_mixed_mode_commit() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);

        // Type "kyou" → "今日", then Tab to English, type "test", then Enter
        type_string(&mut session, "kyou");
        let best = session.candidates[0].clone();
        session.handle_key(key::TAB, "", 0); // → English
        type_string(&mut session, "test");

        // Marked text should show "今日test"
        let display = session.current_display.as_deref().unwrap();
        assert_eq!(display, format!("{}test", best));

        // Commit should produce "今日test"
        let resp = session.handle_key(key::ENTER, "", 0);
        assert_eq!(
            resp.commit_text.as_deref(),
            Some(&format!("{}test", best)[..])
        );
        assert!(!session.is_composing());
    }

    #[test]
    fn test_mixed_mode_display() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);

        // Type Japanese → English → Japanese
        type_string(&mut session, "kyou");
        let best = session.candidates[0].clone();
        session.handle_key(key::TAB, "", 0); // → English
        type_string(&mut session, "hello");
        session.handle_key(key::TAB, "", 0); // → Japanese
        type_string(&mut session, "kyou");

        // Display should be "<best>hello<new_best>"
        let display = session.current_display.as_deref().unwrap();
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
        let best = session.candidates[0].clone();
        session.handle_key(key::TAB, "", 0); // → English
        type_string(&mut session, "ab");

        // Backspace twice to empty English segment
        session.handle_key(key::BACKSPACE, "", 0);
        session.handle_key(key::BACKSPACE, "", 0);
        assert!(session.composed_kana.is_empty());
        // display_prefix still has the frozen conversion
        assert_eq!(session.display_prefix, best);

        // One more backspace deletes from prefix
        session.handle_key(key::BACKSPACE, "", 0);
        assert!(session.display_prefix.len() < best.len());
    }

    // --- Space in English mode ---

    #[test]
    fn test_english_mode_space_literal() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);

        session.handle_key(key::TAB, "", 0); // → English
        type_string(&mut session, "hi");
        session.handle_key(key::SPACE, "", 0);
        assert_eq!(session.composed_kana, "hi ");
    }

    // --- Candidates are generated ---

    #[test]
    fn test_candidates_generated() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);

        type_string(&mut session, "kyou");
        assert!(!session.candidates.is_empty());
        assert!(!session.nbest_paths.is_empty());
    }

    // --- Non-romaji char in composing ---

    #[test]
    fn test_unrecognized_char_added_to_kana() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);

        type_string(&mut session, "ka"); // "か"
        session.handle_key(0, "1", 0); // unrecognized
        assert!(session.composed_kana.ends_with('1'));
    }

    // --- z-sequence ---

    #[test]
    fn test_z_sequence() {
        let dict = make_test_dict();
        let mut session = InputSession::new(&dict, None, None);

        // "z" is a prefix in the romaji trie, "zh" → "←"
        type_string(&mut session, "zh");
        assert_eq!(session.composed_kana, "←");
    }
}
