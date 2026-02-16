use super::types::{
    AsyncGhostRequest, CandidateAction, ConversionMode, KeyResponse, MarkedText, SessionState,
    Submode,
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
                dashed: false,
            });
        }
        self.reset_state();
        resp
    }

    pub(super) fn commit_current_state(&mut self) -> KeyResponse {
        if !self.is_composing() {
            return KeyResponse::consumed();
        }

        let mut resp = KeyResponse::consumed();
        resp.candidates = CandidateAction::Hide;
        self.flush();

        let c = self.comp();
        let prefix_text = std::mem::take(&mut c.prefix.text);

        if c.candidates.selected < c.candidates.surfaces.len() {
            let reading = c.kana.clone();
            let surface = c.candidates.surfaces[c.candidates.selected].clone();

            self.record_history(reading, surface.clone());
            resp.side_effects.save_history = true;
            resp.commit = Some(format!("{}{}", prefix_text, surface));
        } else {
            let c = self.comp();
            if !c.kana.is_empty() || !prefix_text.is_empty() {
                resp.commit = Some(format!("{}{}", prefix_text, c.kana));
            } else {
                resp.marked = Some(MarkedText {
                    text: String::new(),
                    dashed: false,
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
        if self.conversion_mode == ConversionMode::GhostText && resp.commit.is_some() {
            self.ghost_generation += 1;
            resp.ghost_request = Some(AsyncGhostRequest {
                context: self.committed_context.clone(),
                generation: self.ghost_generation,
            });
        }

        self.reset_state();
        resp
    }

    pub(super) fn record_history(&mut self, reading: String, surface: String) {
        if self.history.is_none() {
            return;
        }
        // Record whole pair
        self.history_records
            .push(vec![(reading.clone(), surface.clone())]);

        // Sub-phrase learning: if a matching N-best path exists
        if let Some(matching_path) = self
            .comp()
            .candidates
            .paths
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

    pub(super) fn reset_state(&mut self) {
        self.state = SessionState::Idle;
        self.idle_submode = Submode::Japanese;
    }
}
