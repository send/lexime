use super::types::{
    cyclic_index, CandidateAction, KeyEvent, KeyResponse, MarkedText, SessionState, SnippetState,
};
use super::InputSession;

impl InputSession {
    /// Enter snippet mode. If composing, commit first.
    pub(super) fn enter_snippet_mode(&mut self) -> KeyResponse {
        let store = match &self.snippet_store {
            Some(s) => s.clone(),
            None => {
                // Cancel stale snippet mode if active
                if matches!(self.state, SessionState::Snippet(_)) {
                    return self.snippet_cancel_passthrough();
                }
                // Mirror ModifiedKey: commit composing state but don't consume
                if matches!(self.state, SessionState::Composing(_)) {
                    let mut resp = self.commit_current_state();
                    resp.consumed = false;
                    return resp;
                }
                return KeyResponse::not_consumed();
            }
        };

        // If composing, commit first
        let mut base_resp = if matches!(self.state, SessionState::Composing(_)) {
            self.commit_current_state()
        } else {
            KeyResponse::consumed()
        };

        let matches = store.all_entries();
        let surfaces = snippet_surfaces(&matches);

        self.state = SessionState::Snippet(SnippetState {
            filter: String::new(),
            matches,
            selected: 0,
        });

        base_resp.marked = Some(MarkedText {
            text: String::new(),
        });
        if surfaces.is_empty() {
            base_resp.candidates = CandidateAction::Hide;
        } else {
            base_resp.candidates = CandidateAction::Show {
                surfaces,
                selected: 0,
            };
        }
        base_resp
    }

    /// Handle a key event while in snippet mode.
    pub(super) fn handle_snippet_key(&mut self, event: KeyEvent) -> KeyResponse {
        match event {
            KeyEvent::Text { ref text, .. } | KeyEvent::Remapped { ref text, .. } => {
                self.snippet_filter_append(text)
            }

            KeyEvent::Backspace => self.snippet_filter_pop(),

            KeyEvent::Enter | KeyEvent::Space => self.snippet_confirm(),

            KeyEvent::ArrowDown => self.snippet_navigate(1),
            KeyEvent::ArrowUp => self.snippet_navigate(-1),

            KeyEvent::Escape => self.snippet_cancel(),

            // Any other key: cancel snippet mode, don't consume
            _ => self.snippet_cancel_passthrough(),
        }
    }

    fn snippet_filter_append(&mut self, text: &str) -> KeyResponse {
        let store = match &self.snippet_store {
            Some(s) => s.clone(),
            None => return self.snippet_cancel_passthrough(),
        };

        let SessionState::Snippet(ref mut s) = self.state else {
            unreachable!();
        };
        s.filter.push_str(text);
        s.matches = store.prefix_search(&s.filter);
        s.selected = 0;

        build_snippet_response(s)
    }

    fn snippet_filter_pop(&mut self) -> KeyResponse {
        let store = match &self.snippet_store {
            Some(s) => s.clone(),
            None => return self.snippet_cancel_passthrough(),
        };

        let SessionState::Snippet(ref mut s) = self.state else {
            unreachable!();
        };

        if s.filter.is_empty() {
            // Empty filter + backspace → cancel
            return self.snippet_cancel();
        }

        s.filter.pop();
        s.matches = store.prefix_search(&s.filter);
        s.selected = 0;

        build_snippet_response(s)
    }

    fn snippet_confirm(&mut self) -> KeyResponse {
        if self.snippet_store.is_none() {
            return self.snippet_cancel_passthrough();
        }

        let SessionState::Snippet(ref s) = self.state else {
            unreachable!();
        };

        if s.matches.is_empty() {
            return self.snippet_cancel();
        }

        let (_key, body) = s.matches[s.selected].clone();

        self.committed_context.push_str(&body);
        self.reset_state();

        let mut resp = KeyResponse::consumed().with_hide_candidates();
        resp.marked = Some(MarkedText {
            text: String::new(),
        });
        resp.commit = Some(body);
        resp
    }

    fn snippet_navigate(&mut self, delta: i32) -> KeyResponse {
        if self.snippet_store.is_none() {
            return self.snippet_cancel_passthrough();
        }

        let SessionState::Snippet(ref mut s) = self.state else {
            unreachable!();
        };

        if s.matches.is_empty() {
            return KeyResponse::consumed();
        }

        s.selected = cyclic_index(s.selected, delta, s.matches.len());

        build_snippet_response(s)
    }

    fn snippet_cancel(&mut self) -> KeyResponse {
        self.reset_state();
        KeyResponse::consumed()
            .with_marked(String::new())
            .with_hide_candidates()
    }

    /// Cancel snippet mode and pass through the key to the client.
    fn snippet_cancel_passthrough(&mut self) -> KeyResponse {
        self.reset_state();
        let mut r = KeyResponse::not_consumed();
        r.marked = Some(MarkedText {
            text: String::new(),
        });
        r.candidates = CandidateAction::Hide;
        r
    }
}

fn build_snippet_response(s: &SnippetState) -> KeyResponse {
    let mut resp = KeyResponse::consumed().with_marked(s.filter.clone());
    let surfaces = snippet_surfaces(&s.matches);

    if surfaces.is_empty() {
        resp.candidates = CandidateAction::Hide;
    } else {
        resp.candidates = CandidateAction::Show {
            surfaces,
            selected: u32::try_from(s.selected).unwrap_or(0),
        };
    }
    resp
}

/// Format snippet matches as "key\tbody" for the candidate panel.
fn snippet_surfaces(matches: &[(String, String)]) -> Vec<String> {
    matches
        .iter()
        .map(|(key, body)| format!("{}\t{}", key, body))
        .collect()
}
