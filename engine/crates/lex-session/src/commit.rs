use super::types::{
    AsyncGhostRequest, CandidateAction, ConversionMode, KeyResponse, LearningRecord, MarkedText,
    SessionState,
};
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
        let SessionState::Composing(ref mut c) = self.state else {
            return KeyResponse::consumed();
        };

        let mut resp = KeyResponse::consumed();
        resp.candidates = CandidateAction::Hide;
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

        // Accumulate committed text for neural context
        if let Some(ref committed) = resp.commit {
            self.committed_context.push_str(committed);
        }

        // GhostText mode: request ghost text generation after commit.
        // Use full committed_context (not just the latest commit) so the
        // neural model sees the complete preceding text.
        if self.config.conversion_mode == ConversionMode::GhostText && resp.commit.is_some() {
            self.ghost.generation += 1;
            resp.ghost_request = Some(AsyncGhostRequest {
                context: self.committed_context.clone(),
                generation: self.ghost.generation,
            });
        }

        self.reset_state();
        resp
    }

    pub(super) fn record_history(&mut self, reading: String, surface: String) {
        if self.history.is_none() {
            return;
        }
        let segments = self.comp().find_matching_path(&surface);
        self.history_records.push(LearningRecord::Committed {
            reading,
            surface,
            segments,
        });
    }

    pub(super) fn reset_state(&mut self) {
        self.state = SessionState::Idle;
    }
}
