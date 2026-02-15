use super::types::{AsyncGhostRequest, ConversionMode, KeyResponse};
use super::InputSession;

impl InputSession<'_> {
    /// Accept the current ghost text (Tab in idle with ghost visible).
    pub(super) fn accept_ghost_text(&mut self) -> KeyResponse {
        let text = self.ghost_text.take().unwrap();
        // Accumulate accepted ghost text into committed_context
        self.committed_context.push_str(&text);
        let mut resp = KeyResponse::consumed();
        resp.commit = Some(text);
        // After accepting ghost, request another generation with full context
        if self.conversion_mode == ConversionMode::GhostText {
            self.ghost_generation += 1;
            resp.ghost_request = Some(AsyncGhostRequest {
                context: self.committed_context.clone(),
                generation: self.ghost_generation,
            });
        }
        resp
    }

    /// Receive ghost text from async generation. Returns a response if valid.
    /// Returns `None` if the generation is stale or session is in wrong state.
    pub fn receive_ghost_text(&mut self, generation: u64, text: String) -> Option<KeyResponse> {
        if generation != self.ghost_generation {
            return None;
        }
        if self.is_composing() {
            return None;
        }
        if self.conversion_mode != ConversionMode::GhostText {
            return None;
        }
        self.ghost_text = Some(text.clone());
        let mut resp = KeyResponse::consumed();
        resp.ghost_text = Some(text);
        Some(resp)
    }

    /// Get current ghost generation counter (for staleness checks).
    pub fn ghost_generation(&self) -> u64 {
        self.ghost_generation
    }
}
