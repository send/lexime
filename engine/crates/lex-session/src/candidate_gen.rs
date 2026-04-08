use lex_core::candidates::CandidateResponse;
use lex_core::converter::{build_lattice, convert_from_lattice, ConvertedSegment};

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
            // Reuse cached lattice: extend if kana grew, or keep as-is
            // if unchanged. Fall back to full rebuild otherwise.
            let lattice = match self.comp().cached_lattice.take() {
                Some(mut cached) if reading.starts_with(&cached.input) => {
                    cached.extend(&*self.dict, &reading); // no-op if reading == input
                    cached
                }
                _ => build_lattice(&*self.dict, &reading),
            };

            let segments = {
                let h_guard = self.history.as_ref().and_then(|h| h.read().ok());
                convert_from_lattice(
                    &lattice,
                    &*self.dict,
                    self.conn.as_deref(),
                    h_guard.as_deref(),
                )
            };
            let surface: String = segments.iter().map(|s| s.surface.as_str()).collect();
            let c = self.comp();
            c.candidates.surfaces = vec![surface];
            c.candidates.paths = vec![segments];
            c.candidates.selected = 0;
            // Cache for next keystroke's incremental extension
            c.cached_lattice = Some(lattice.clone());

            let mut resp = build_marked_text(self.comp());
            resp.async_request = Some(AsyncCandidateRequest {
                reading,
                candidate_dispatch: self.config.conversion_mode.candidate_dispatch(),
                lattice: Some(lattice),
            });
            return resp;
        } else {
            self.comp().candidates.clear();
        }
        let resp = build_marked_text(self.comp());
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
