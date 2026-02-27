use lex_core::candidates::CandidateResponse;
use lex_core::converter::{convert, convert_with_history, ConvertedSegment};

use super::response::{build_marked_text, build_marked_text_and_candidates};
use super::types::{AsyncCandidateRequest, KeyResponse, SessionState, MAX_CANDIDATES};
use super::InputSession;

impl InputSession {
    pub(super) fn update_candidates(&mut self) {
        self.comp().candidates.selected = 0;

        if self.comp().kana.is_empty() {
            let c = self.comp();
            c.candidates.clear();
            c.stability.reset();
            return;
        }

        let mode = self.config.conversion_mode;
        let reading = self.comp().kana.clone();
        let CandidateResponse { surfaces, paths } = {
            // read().ok() intentionally ignores RwLock poison — if another thread
            // panicked, we degrade gracefully to history-less conversion rather
            // than cascading the panic. macOS will restart the IME if needed.
            let h_guard = self.history.as_ref().and_then(|h| h.read().ok());
            let history_ref = h_guard.as_deref();
            mode.generate_candidates(
                &*self.dict,
                self.conn.as_deref(),
                history_ref,
                &reading,
                MAX_CANDIDATES,
            )
        };
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
            let segments = {
                // See update_candidates for rationale on read().ok()
                let h_guard = self.history.as_ref().and_then(|h| h.read().ok());
                match h_guard.as_deref() {
                    Some(h) => convert_with_history(&*self.dict, self.conn.as_deref(), h, &reading),
                    None => convert(&*self.dict, self.conn.as_deref(), &reading),
                }
            };
            let surface: String = segments.iter().map(|s| s.surface.as_str()).collect();
            let c = self.comp();
            c.candidates.surfaces = vec![surface];
            c.candidates.paths = vec![segments];
            c.candidates.selected = 0;
        } else {
            self.comp().candidates.clear();
        }
        let mut resp = build_marked_text(self.comp());
        if !reading.is_empty() {
            resp.async_request = Some(AsyncCandidateRequest {
                reading,
                candidate_dispatch: self.config.conversion_mode.candidate_dispatch(),
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
        // Stale check: reading must match current composing state
        match &self.state {
            SessionState::Composing(c) if c.kana == reading => {}
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
        Some(build_marked_text_and_candidates(self.comp()))
    }
}
