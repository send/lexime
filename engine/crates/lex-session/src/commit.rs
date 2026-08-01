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
    /// - **Commits what the host was showing.** A voluntary commit resolves to
    ///   the selected surface because the user asked to convert. Here nobody
    ///   asked: if they never navigated, the marked text was the reading
    ///   (`display_kana()`), and committing `surfaces[0]` instead would put a
    ///   conversion they never saw into their document just for switching apps.
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
        let SessionState::Composing(ref mut c) = self.state else {
            unreachable!();
        };

        let mut resp = KeyResponse::consumed().with_hide_candidates();
        c.flush();

        let prefix_text = std::mem::take(&mut c.prefix.text);
        // `user_selected` is precisely "the host is showing `display()`, not
        // `display_kana()`" — so this commits the text that was on screen.
        let shown = if c.candidates.user_selected {
            c.candidates
                .surfaces
                .get(c.candidates.selected)
                .cloned()
                .unwrap_or_else(|| c.kana.clone())
        } else {
            c.kana.clone()
        };

        if !shown.is_empty() || !prefix_text.is_empty() {
            resp.commit = Some(format!("{}{}", prefix_text, shown));
        } else {
            resp.marked = Some(MarkedText {
                text: String::new(),
            });
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
