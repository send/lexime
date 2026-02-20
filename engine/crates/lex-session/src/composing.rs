use lex_core::romaji::{convert_romaji, RomajiTrie, TrieLookupResult};

use super::response::{build_marked_text, build_marked_text_and_candidates};
use super::types::{
    is_romaji_input, Composition, KeyResponse, SessionState, MAX_COMPOSED_KANA_LENGTH,
};
use super::InputSession;

impl InputSession {
    pub(super) fn handle_composing_text(&mut self, text: &str) -> KeyResponse {
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

        // Uppercase letter: flush pending romaji and add to kana as-is (no romaji conversion).
        // Skip auto-commit so consecutive uppercase letters stay grouped as one word.
        if text.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
            self.comp().drain_pending(true);
            self.comp().kana.push_str(text);
            self.comp().stability.reset();
            return if self.config.defer_candidates {
                self.make_deferred_candidates_response()
            } else {
                self.update_candidates();
                build_marked_text_and_candidates(self.comp())
            };
        }

        if is_romaji_input(text) {
            // If user has selected a non-default candidate, commit it first
            let c = self.comp();
            if c.candidates.selected > 0 && c.candidates.selected < c.candidates.surfaces.len() {
                let commit_resp = self.commit_current_state();
                self.state = SessionState::Composing(Composition::new());
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
        if self.config.defer_candidates {
            self.make_deferred_candidates_response()
        } else {
            self.update_candidates();
            let resp = build_marked_text_and_candidates(self.comp());
            self.maybe_auto_commit(resp)
        }
    }

    pub(super) fn append_and_convert(&mut self, input: &str) -> KeyResponse {
        // Overflow: flush + commit if kana too long
        if self.comp().kana.len() >= MAX_COMPOSED_KANA_LENGTH {
            let resp = self.commit_composed();
            self.state = SessionState::Composing(Composition::new());
            self.comp().pending.push_str(input);
            self.comp().drain_pending(false);
            let sub_resp = if self.config.defer_candidates {
                self.make_deferred_candidates_response()
            } else {
                if self.comp().pending.is_empty() {
                    self.update_candidates();
                }
                let resp = build_marked_text_and_candidates(self.comp());
                self.maybe_auto_commit(resp)
            };
            return resp.with_display_from(sub_resp);
        }

        self.comp().pending.push_str(input);
        self.comp().drain_pending(false);

        if self.config.defer_candidates {
            if self.comp().pending.is_empty() {
                // Kana resolved — defer candidate generation to caller
                self.make_deferred_candidates_response()
            } else {
                // Pending romaji: show kana + pending, no candidates needed yet
                build_marked_text(self.comp())
            }
        } else {
            // Sync mode: generate candidates immediately when romaji resolves
            if self.comp().pending.is_empty() {
                self.update_candidates();
            }
            let resp = build_marked_text_and_candidates(self.comp());
            self.maybe_auto_commit(resp)
        }
    }

    /// Try auto-commit in sync mode. If auto-commit fires, return its response;
    /// otherwise return the provided display response.
    pub(super) fn maybe_auto_commit(&mut self, display_resp: KeyResponse) -> KeyResponse {
        if !self.config.defer_candidates {
            if let Some(auto_resp) = self.try_auto_commit() {
                return auto_resp;
            }
        }
        display_resp
    }
}
