use tracing::debug_span;

use lex_core::romaji::{RomajiTrie, TrieLookupResult};
use lex_core::settings::settings;

use super::response::{build_candidate_selection, build_marked_text_and_candidates};
use super::types::{
    is_romaji_input, key, CandidateAction, Composition, ConversionMode, KeyResponse, SessionState,
    FLAG_HAS_MODIFIER, FLAG_SHIFT,
};
use super::InputSession;

impl InputSession {
    /// Process a key event. Returns a KeyResponse describing what the caller should do.
    ///
    /// `flags`: bit 0 = shift, bit 1 = has_modifier (Cmd/Ctrl/Opt)
    pub fn handle_key(&mut self, key_code: u16, text: &str, flags: u8) -> KeyResponse {
        let _span = debug_span!("handle_key", key_code, text, flags).entered();
        let has_modifier = flags & FLAG_HAS_MODIFIER != 0;
        let has_shift = flags & FLAG_SHIFT != 0;

        // Clear ghost text on any key except Tab (ghost accept is handled in handle_idle)
        let had_ghost = self.ghost.text.is_some();
        if had_ghost && key_code != key::TAB {
            self.ghost.text = None;
        }

        // Eisu key → commit if composing, enter ABC passthrough
        let mut resp = if key_code == key::EISU {
            let r = if self.is_composing() {
                self.commit_current_state()
            } else {
                KeyResponse::consumed()
            };
            self.abc_passthrough = true;
            r

        // Kana key → exit ABC passthrough
        } else if key_code == key::KANA {
            self.abc_passthrough = false;
            KeyResponse::consumed()

        // Keymap remap: feed remapped text through normal input path (trie, candidates, etc.)
        // Falls back to direct commit if the text isn't handled by trie/romaji (e.g. \ in idle).
        } else if let Some(remapped) = settings().keymap_get(key_code, has_shift) {
            if self.abc_passthrough {
                let mut r = KeyResponse::consumed();
                r.commit = Some(remapped.to_string());
                r
            } else if self.is_composing() {
                self.handle_composing_text(remapped)
            } else {
                let r = self.handle_idle(key_code, remapped);
                if r.consumed {
                    r
                } else {
                    // Not handled by trie/romaji (e.g. \) — commit directly
                    let mut r = KeyResponse::consumed();
                    r.commit = Some(remapped.to_string());
                    r
                }
            }

        // ABC passthrough: commit printable chars directly, pass through the rest.
        // Consuming printable chars avoids macOS keyboard layout re-interpretation
        // which can produce wrong characters on JIS keyboards.
        } else if self.abc_passthrough {
            match text.chars().next() {
                Some(c) if (' '..='~').contains(&c) => {
                    let mut r = KeyResponse::consumed();
                    r.commit = Some(text.to_string());
                    r
                }
                _ => KeyResponse::not_consumed(),
            }

        // Modifier keys (Cmd, Ctrl, etc.) — commit first, then pass through
        } else if has_modifier {
            if self.is_composing() {
                let mut r = self.commit_current_state();
                r.consumed = false;
                r
            } else {
                KeyResponse::not_consumed()
            }
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

    pub(super) fn handle_idle(&mut self, key_code: u16, text: &str) -> KeyResponse {
        // Ghost text: Tab accepts ghost (GhostText mode only)
        if key_code == key::TAB
            && self.ghost.text.is_some()
            && self.config.conversion_mode == ConversionMode::GhostText
        {
            return self.accept_ghost_text();
        }

        // Tab in idle: passthrough (no more submode toggle)
        if key_code == key::TAB {
            return KeyResponse::not_consumed();
        }

        // Romaji input
        if is_romaji_input(text) {
            self.state = SessionState::Composing(Composition::new());
            return self.append_and_convert(&text.to_lowercase());
        }

        // Direct trie match for non-romaji chars (punctuation)
        let trie = RomajiTrie::global();
        match trie.lookup(text) {
            TrieLookupResult::Exact(_) | TrieLookupResult::ExactAndPrefix(_) => {
                self.state = SessionState::Composing(Composition::new());
                self.append_and_convert(text)
            }
            _ => KeyResponse::not_consumed(),
        }
    }

    pub(super) fn handle_composing(&mut self, key_code: u16, text: &str) -> KeyResponse {
        match key_code {
            key::ENTER => {
                // Lazy generate: ensure candidates are available for commit
                if self.comp().candidates.is_empty() && !self.comp().kana.is_empty() {
                    self.update_candidates();
                }
                self.commit_current_state()
            }

            key::SPACE => {
                // Lazy generate: ensure candidates for Space cycling
                if self.comp().candidates.is_empty() && !self.comp().kana.is_empty() {
                    self.update_candidates();
                }
                let c = self.comp();
                if !c.candidates.is_empty() {
                    if c.candidates.selected == 0 && c.candidates.surfaces.len() > 1 {
                        c.candidates.selected = 1;
                    } else {
                        c.candidates.selected = super::types::cyclic_index(
                            c.candidates.selected,
                            1,
                            c.candidates.surfaces.len(),
                        );
                    }
                    build_candidate_selection(self.comp())
                } else {
                    KeyResponse::consumed()
                }
            }

            key::DOWN => {
                // Lazy generate: ensure candidates for arrow cycling
                if self.comp().candidates.is_empty() && !self.comp().kana.is_empty() {
                    self.update_candidates();
                }
                let c = self.comp();
                if !c.candidates.is_empty() {
                    c.candidates.selected = super::types::cyclic_index(
                        c.candidates.selected,
                        1,
                        c.candidates.surfaces.len(),
                    );
                    build_candidate_selection(self.comp())
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
                    c.candidates.selected = super::types::cyclic_index(
                        c.candidates.selected,
                        -1,
                        c.candidates.surfaces.len(),
                    );
                    build_candidate_selection(self.comp())
                } else {
                    KeyResponse::consumed()
                }
            }

            key::TAB => {
                // Tab in composing: always commit
                if self.comp().candidates.is_empty() && !self.comp().kana.is_empty() {
                    self.update_candidates();
                }
                self.commit_current_state()
            }

            key::BACKSPACE => self.handle_backspace(),

            key::ESCAPE => {
                self.comp().flush();
                {
                    let c = self.comp();
                    if !c.kana.is_empty() {
                        let kana = c.kana.clone();
                        self.record_history(kana.clone(), kana);
                    }
                }
                self.comp().candidates.clear();
                let mut resp = KeyResponse::consumed();
                resp.candidates = CandidateAction::Hide;
                // Escape: IMKit will call commitComposition after.
                // composedString() uses display() which computes from current state.
                resp
            }

            _ => self.handle_composing_text(text),
        }
    }

    pub(super) fn handle_backspace(&mut self) -> KeyResponse {
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
            resp.marked = Some(super::MarkedText {
                text: String::new(),
            });
            self.reset_state();
            resp
        } else if self.comp().kana.is_empty() && self.comp().pending.is_empty() {
            // Current segment is empty but prefix has content
            let c = self.comp();
            c.candidates.clear();
            let display = c.display();
            let mut resp = KeyResponse::consumed();
            resp.marked = Some(super::MarkedText { text: display });
            resp.candidates = CandidateAction::Hide;
            resp
        } else if self.config.defer_candidates {
            self.make_deferred_candidates_response()
        } else {
            self.update_candidates();
            let resp = build_marked_text_and_candidates(self.comp());
            self.maybe_auto_commit(resp)
        }
    }
}
