use super::types::{KeyResponse, LearningRecord, MarkedText, SessionState};
use super::InputSession;

impl InputSession {
    pub(super) fn commit_composed(&mut self) -> KeyResponse {
        let mut resp = KeyResponse::consumed();
        let c = self.comp();
        let text = format!("{}{}", c.prefix.text, c.kana);
        if !text.is_empty() {
            resp.commit = Some(text);
        } else {
            resp.marked = Some(MarkedText {
                text: String::new(),
            });
        }

        self.reset_state();
        resp
    }

    pub(super) fn commit_current_state(&mut self) -> KeyResponse {
        if !matches!(self.state, SessionState::Composing(_)) {
            return KeyResponse::consumed();
        }
        let SessionState::Composing(ref mut c) = self.state else {
            unreachable!();
        };

        let mut resp = KeyResponse::consumed().with_hide_candidates();
        c.flush();

        let prefix_text = std::mem::take(&mut c.prefix.text);

        if c.candidates.selected < c.candidates.surfaces.len() {
            let reading = c.kana.clone();
            let surface = c.candidates.surfaces[c.candidates.selected].clone();

            self.record_history(reading, surface.clone());
            resp.commit = Some(format!("{}{}", prefix_text, surface));
        } else {
            let SessionState::Composing(ref c) = self.state else {
                unreachable!();
            };
            if !c.kana.is_empty() || !prefix_text.is_empty() {
                resp.commit = Some(format!("{}{}", prefix_text, c.kana));
            } else {
                resp.marked = Some(MarkedText {
                    text: String::new(),
                });
            }
        }

        self.reset_state();
        resp
    }

    /// Settle a composition the user did **not** choose to end — the host is
    /// tearing down its marked-text session because focus left (#298).
    ///
    /// Differs from `commit_current_state` in exactly the two ways an
    /// involuntary end differs from a voluntary one:
    ///
    /// - **Commits what the host was showing** — `last_marked`, recorded by
    ///   `note_response` when the response was emitted. A voluntary commit
    ///   resolves to the selected surface because the user asked to convert.
    ///   Here nobody asked, so putting `surfaces[0]` into the document for
    ///   merely switching apps would insert a conversion never seen.
    /// - **Records no history.** An app switch is not acceptance. Treating it as
    ///   one trains top-1 on a non-signal and, over time, degrades the 1発目精度
    ///   that CLAUDE.md makes the metric — while quietly writing to what it
    ///   calls ユーザーの資産.
    ///
    /// Both are the same principle: an involuntary end must not manufacture
    /// intent the user never expressed.
    pub(super) fn settle_unconfirmed(&mut self) -> KeyResponse {
        if !matches!(self.state, SessionState::Composing(_)) {
            return KeyResponse::consumed();
        }

        // Commit exactly the marked text the host is showing — recorded by
        // `note_response`, not re-derived here. Deriving it is what R2 broke:
        // any rule reconstructing "reading or surface?" from selection state
        // has to be re-applied at every site that re-renders (Backspace after
        // navigating, ForwardDelete, auto-commit), and goes stale at the first
        // one missed. Reading it also means **no `flush()`**: forcing pending
        // romaji would convert a trailing `n` to `ん` and commit text that was
        // never on screen.
        let shown = std::mem::take(&mut self.last_marked);

        let mut resp = KeyResponse::consumed().with_hide_candidates();
        if shown.is_empty() {
            resp.marked = Some(MarkedText {
                text: String::new(),
            });
        } else {
            resp.commit = Some(shown);
        }

        self.reset_state();
        resp
    }

    pub(super) fn record_history(&mut self, reading: String, surface: String) {
        if self.history.is_none() {
            return;
        }
        let (segments, rank, top1) = {
            let c = self.comp();
            let rank = c.candidates.selected;
            let top1 = if rank > 0 {
                c.candidates.surfaces.first().cloned()
            } else {
                None
            };
            (c.find_matching_path(&surface), rank, top1)
        };
        self.history_records.push(LearningRecord::Committed {
            reading,
            surface,
            segments,
            rank,
            top1,
            auto: false,
            learn: true,
        });
    }

    pub(super) fn reset_state(&mut self) {
        self.state = SessionState::Idle;
        self.lattice_cache.invalidate();
    }
}
