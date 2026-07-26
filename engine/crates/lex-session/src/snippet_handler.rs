use super::types::{
    cyclic_index, CandidateAction, KeyEvent, KeyResponse, MarkedText, SessionState, SnippetState,
};
use super::InputSession;

impl InputSession {
    /// Enter snippet mode. If composing, commit first.
    pub(super) fn enter_snippet_mode(&mut self) -> KeyResponse {
        // "No store" and "a store with nothing usable in it" are one situation:
        // there is nothing to pick, so the trigger key is not ours. They must
        // take the same exit — this used to be two branches, and the second one
        // returned empty marked text while leaving the session in `Snippet`,
        // which drops the host out of composition with the session still live
        // (see `SnippetState::display_text`). Deciding before any state is
        // touched is what keeps the two from drifting apart again.
        let browse = self.snippet_store.as_ref().and_then(|store| {
            let matches = store.all_entries();
            (!matches.is_empty()).then(|| (store.clone(), matches))
        });
        let Some((store, matches)) = browse else {
            return self.snippet_trigger_unavailable();
        };

        // If composing, commit first
        let base_resp = if matches!(self.state, SessionState::Composing(_)) {
            self.commit_current_state()
        } else {
            KeyResponse::consumed()
        };

        // Render through the same builder as every later keystroke so the
        // "snippet mode ⇒ non-empty marked text" invariant holds by
        // construction: build_snippet_response is the single site that derives
        // marked (from display_text) and candidates for a browsing state.
        // with_display_from keeps any commit from the composing state above
        // while taking the display fields from the render.
        let state = SnippetState {
            filter: String::new(),
            matches,
            selected: 0,
            store,
        };
        let resp = base_resp.with_display_from(build_snippet_response(&state));
        self.state = SessionState::Snippet(state);
        resp
    }

    /// The trigger key when there is nothing to offer. Tears down any stale
    /// browse, commits any composition, and lets the key through to the client
    /// — the same treatment `ModifiedKey` gets, since as far as the user is
    /// concerned the IME has no binding for it.
    fn snippet_trigger_unavailable(&mut self) -> KeyResponse {
        if matches!(self.state, SessionState::Snippet(_)) {
            return self.snippet_cancel_passthrough();
        }
        if matches!(self.state, SessionState::Composing(_)) {
            let mut resp = self.commit_current_state();
            resp.consumed = false;
            return resp;
        }
        KeyResponse::not_consumed()
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

            // Mode-switch keys: cancel snippet mode and apply mode switch
            KeyEvent::SwitchToDirectInput => {
                let resp = self.snippet_cancel();
                self.abc_passthrough = true;
                resp
            }
            KeyEvent::SwitchToJapanese => {
                let resp = self.snippet_cancel();
                self.abc_passthrough = false;
                resp
            }

            // Any other key: cancel snippet mode, don't consume
            _ => self.snippet_cancel_passthrough(),
        }
    }

    fn snippet_filter_append(&mut self, text: &str) -> KeyResponse {
        let SessionState::Snippet(ref mut s) = self.state else {
            unreachable!();
        };
        s.filter.push_str(text);
        // Filter against the browse's own store snapshot, not the live store —
        // a mid-browse swap must not change what this picker shows.
        s.matches = s.store.prefix_search(&s.filter);
        s.selected = 0;

        build_snippet_response(s)
    }

    fn snippet_filter_pop(&mut self) -> KeyResponse {
        let SessionState::Snippet(ref mut s) = self.state else {
            unreachable!();
        };

        if s.filter.is_empty() {
            // Empty filter + backspace → cancel
            return self.snippet_cancel();
        }

        s.filter.pop();
        s.matches = s.store.prefix_search(&s.filter);
        s.selected = 0;

        build_snippet_response(s)
    }

    fn snippet_confirm(&mut self) -> KeyResponse {
        let SessionState::Snippet(ref s) = self.state else {
            unreachable!();
        };

        if s.matches.is_empty() {
            return self.snippet_cancel();
        }

        // Non-empty by construction: `prefix_search` drops entries whose body
        // expands to nothing, so an unusable snippet never reaches `matches`
        // (and a store of only such entries never enters snippet mode at all).
        // That is what keeps this commit non-empty without a check here.
        let (_key, body) = s.matches[s.selected].clone();

        self.reset_state();

        let mut resp = KeyResponse::consumed().with_hide_candidates();
        resp.marked = Some(MarkedText {
            text: String::new(),
        });
        resp.commit = Some(body);
        resp
    }

    fn snippet_navigate(&mut self, delta: i32) -> KeyResponse {
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
    let mut resp = KeyResponse::consumed().with_marked(s.display_text());
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
