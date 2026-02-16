use lex_core::romaji::{convert_romaji, RomajiTrie, TrieLookupResult};

use super::types::{
    is_romaji_input, Composition, KeyResponse, SessionState, Submode, MAX_COMPOSED_KANA_LENGTH,
};
use super::InputSession;

impl InputSession {
    pub(super) fn handle_composing_text(&mut self, text: &str) -> KeyResponse {
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

    pub(super) fn append_and_convert(&mut self, input: &str) -> KeyResponse {
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

    pub(super) fn drain_pending(&mut self, force: bool) {
        let c = self.comp();
        let result = convert_romaji(&c.kana, &c.pending, force);
        c.kana = result.composed_kana;
        c.pending = result.pending_romaji;
    }

    pub(super) fn flush(&mut self) {
        self.drain_pending(true);
    }
}
