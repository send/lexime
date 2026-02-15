use crate::candidates::CandidateResponse;
use crate::converter::{convert, ConvertedSegment};

use super::types::{AsyncCandidateRequest, KeyResponse, SessionState, Submode, MAX_CANDIDATES};
use super::InputSession;

impl InputSession<'_> {
    pub(super) fn update_candidates(&mut self) {
        self.comp().candidates.selected = 0;

        if self.comp().kana.is_empty() {
            let c = self.comp();
            c.candidates.clear();
            c.stability.reset();
            return;
        }

        let mode = self.conversion_mode;
        let reading = self.comp().kana.clone();
        let CandidateResponse { surfaces, paths } =
            mode.generate_candidates(self.dict, self.conn, self.history, &reading, MAX_CANDIDATES);
        let c = self.comp();
        c.candidates.surfaces = surfaces;
        c.candidates.paths = paths;
        c.stability.track(&c.candidates.paths);
    }

    /// Build a response that defers candidate generation to the caller.
    /// Computes a synchronous 1-best conversion for interim display so the
    /// marked text shows a converted result immediately (e.g. "違和感無く")
    /// rather than raw kana while the full N-best candidates are generated async.
    pub(super) fn make_deferred_candidates_response(&mut self) -> KeyResponse {
        // Do NOT reset stability here. It accumulates across keystrokes.
        let reading = self.comp().kana.clone();
        if !reading.is_empty() {
            // Quick sync 1-best for interim display (~1-2ms)
            let segments = convert(self.dict, self.conn, &reading);
            let surface: String = segments.iter().map(|s| s.surface.as_str()).collect();
            let c = self.comp();
            c.candidates.surfaces = vec![surface];
            c.candidates.paths = vec![segments];
            c.candidates.selected = 0;
        } else {
            self.comp().candidates.clear();
        }
        let mut resp = self.make_marked_text_response();
        if !reading.is_empty() {
            resp.async_request = Some(AsyncCandidateRequest {
                reading,
                candidate_dispatch: self.conversion_mode.candidate_dispatch(),
            });
        }
        resp
    }

    /// Receive asynchronously generated candidates and update session state.
    /// Returns `None` if the reading is stale (kana has changed).
    pub fn receive_candidates(
        &mut self,
        reading: &str,
        surfaces: Vec<String>,
        paths: Vec<Vec<ConvertedSegment>>,
    ) -> Option<KeyResponse> {
        // Stale check: reading must match current state
        match &self.state {
            SessionState::Composing(c) if c.kana == reading && c.submode == Submode::Japanese => {}
            _ => return None,
        }

        let c = self.comp();
        c.candidates.surfaces = surfaces;
        c.candidates.paths = paths;
        c.candidates.selected = 0;
        c.stability.track(&c.candidates.paths);

        // Try auto-commit with fresh candidates
        if let Some(auto_resp) = self.try_auto_commit() {
            return Some(auto_resp);
        }

        // No auto-commit: update marked text to Viterbi #1 and show candidates
        Some(self.make_marked_text_and_candidates_response())
    }
}
