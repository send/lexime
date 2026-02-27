use tracing::debug_span;

use lex_core::romaji::{RomajiTrie, TrieLookupResult};

use super::response::{build_candidate_selection, build_marked_text_and_candidates};
use super::types::{
    is_romaji_input, CandidateAction, Composition, KeyEvent, KeyResponse, LearningRecord,
    SessionState,
};
use super::InputSession;

impl InputSession {
    /// Ensure candidates are generated (lazy generate on first demand).
    fn ensure_candidates(&mut self) {
        if self.comp().candidates.is_empty() && !self.comp().kana.is_empty() {
            self.update_candidates();
        }
    }

    /// Move candidate selection by `delta` (1=next, -1=prev).
    /// If `skip_current` is true and selected==0, jump directly to 1.
    fn navigate_candidates(&mut self, delta: i32, skip_current: bool) -> KeyResponse {
        self.ensure_candidates();
        let c = self.comp();
        if !c.candidates.is_empty() {
            if skip_current && c.candidates.selected == 0 && c.candidates.surfaces.len() > 1 {
                c.candidates.selected = 1;
            } else {
                c.candidates.selected = super::types::cyclic_index(
                    c.candidates.selected,
                    delta,
                    c.candidates.surfaces.len(),
                );
            }
            build_candidate_selection(self.comp())
        } else {
            KeyResponse::consumed()
        }
    }

    /// Process a key event. Returns a KeyResponse describing what the caller should do.
    pub fn handle_key(&mut self, event: KeyEvent) -> KeyResponse {
        let _span = debug_span!("handle_key", ?event).entered();

        match event {
            // Eisu key → commit if composing, enter ABC passthrough
            KeyEvent::SwitchToDirectInput => {
                let r = if self.is_composing() {
                    self.commit_current_state()
                } else {
                    KeyResponse::consumed()
                };
                self.abc_passthrough = true;
                r
            }

            // Kana key → exit ABC passthrough
            KeyEvent::SwitchToJapanese => {
                self.abc_passthrough = false;
                KeyResponse::consumed()
            }

            // Keymap remap: feed remapped text through normal input path (trie, candidates, etc.)
            // Falls back to direct commit if the text isn't handled by trie/romaji (e.g. \ in idle).
            KeyEvent::Remapped { text, .. } => {
                if self.abc_passthrough {
                    self.committed_context.push_str(&text);
                    let mut r = KeyResponse::consumed();
                    r.commit = Some(text);
                    r
                } else if self.is_composing() {
                    self.handle_composing_text(&text)
                } else {
                    let r = self.handle_idle(&text);
                    if r.consumed {
                        r
                    } else {
                        // Not handled by trie/romaji (e.g. \) — commit directly
                        self.committed_context.push_str(&text);
                        let mut r = KeyResponse::consumed();
                        r.commit = Some(text);
                        r
                    }
                }
            }

            // ABC passthrough: Space is printable but comes as KeyEvent::Space, not Text.
            KeyEvent::Space if self.abc_passthrough => {
                self.committed_context.push(' ');
                let mut r = KeyResponse::consumed();
                r.commit = Some(" ".to_string());
                r
            }

            // ABC passthrough: commit printable chars directly, pass through the rest.
            // Text and special keys in ABC mode.
            KeyEvent::Text { ref text, .. } if self.abc_passthrough => match text.chars().next() {
                Some(c) if (' '..='~').contains(&c) => {
                    let text = text.clone();
                    self.committed_context.push_str(&text);
                    let mut r = KeyResponse::consumed();
                    r.commit = Some(text);
                    r
                }
                _ => KeyResponse::not_consumed(),
            },
            _ if self.abc_passthrough => KeyResponse::not_consumed(),

            // Modifier keys (Cmd, Ctrl, etc.) — commit first, then pass through
            KeyEvent::ModifiedKey => {
                if self.is_composing() {
                    let mut r = self.commit_current_state();
                    r.consumed = false;
                    r
                } else {
                    KeyResponse::not_consumed()
                }
            }

            // Composing state dispatch
            KeyEvent::Enter if self.is_composing() => {
                self.ensure_candidates();
                self.commit_current_state()
            }

            KeyEvent::Space if self.is_composing() => self.navigate_candidates(1, true),

            KeyEvent::ArrowDown if self.is_composing() => self.navigate_candidates(1, false),

            KeyEvent::ArrowUp if self.is_composing() => self.navigate_candidates(-1, false),

            KeyEvent::Tab if self.is_composing() => {
                self.ensure_candidates();
                self.commit_current_state()
            }

            KeyEvent::ForwardDelete if self.is_composing() => self.handle_forward_delete(),

            KeyEvent::Backspace if self.is_composing() => self.handle_backspace(),

            KeyEvent::Escape if self.is_composing() => {
                self.comp().flush();
                // Escape cancels composition — do not record history for unconfirmed input.
                self.comp().candidates.clear();
                let mut resp = KeyResponse::consumed();
                resp.candidates = CandidateAction::Hide;
                // Escape: IMKit will call commitComposition after.
                // composedString() uses display() which computes from current state.
                resp
            }

            KeyEvent::Text { ref text, .. } if self.is_composing() => {
                self.handle_composing_text(text)
            }

            // Idle state dispatch
            KeyEvent::Tab => KeyResponse::not_consumed(),

            KeyEvent::Text { ref text, .. } => self.handle_idle(text),

            // Other special keys in idle — not consumed
            _ => KeyResponse::not_consumed(),
        }
    }

    pub(super) fn handle_idle(&mut self, text: &str) -> KeyResponse {
        // Uppercase letter: add to composition as-is (no romaji conversion)
        if text.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
            self.state = SessionState::Composing(Composition::new());
            self.comp().kana.push_str(text);
            self.comp().stability.reset();
            return if self.config.defer_candidates {
                self.make_deferred_candidates_response()
            } else {
                self.update_candidates();
                build_marked_text_and_candidates(self.comp())
            };
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

    fn handle_forward_delete(&mut self) -> KeyResponse {
        self.ensure_candidates();
        let c = self.comp();
        if c.candidates.is_empty() {
            return KeyResponse::consumed();
        }

        let selected = c.candidates.selected;
        let Some(surface) = c.candidates.surfaces.get(selected).cloned() else {
            return KeyResponse::consumed();
        };
        let reading = c.kana.clone();

        // Collect segments for the selected path (for bigram deletion)
        let segments = c.find_matching_path(&surface);

        // Build deletion segments: whole-reading→surface + sub-segments if multi-segment
        let mut all_segments = vec![(reading, surface)];
        if let Some(sub) = segments {
            all_segments.extend(sub);
        }

        // Buffer deletion record
        if self.history.is_some() {
            self.history_records.push(LearningRecord::Deletion {
                segments: all_segments,
            });
        }

        // Remove from candidate list.
        // surfaces can have more entries than paths (predictions, history,
        // lookup entries don't always have corresponding path data).
        let c = self.comp();
        c.candidates.surfaces.remove(selected);
        if selected < c.candidates.paths.len() {
            c.candidates.paths.remove(selected);
        }

        if c.candidates.surfaces.is_empty() {
            c.candidates.selected = 0;
            let mut resp = KeyResponse::consumed();
            resp.candidates = CandidateAction::Hide;
            return resp;
        }

        // Adjust selection
        if selected >= c.candidates.surfaces.len() {
            c.candidates.selected = c.candidates.surfaces.len() - 1;
        }

        build_candidate_selection(self.comp())
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
            let display = c.display_kana();
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
