use lex_core::candidates::CandidateResponse;
use lex_core::converter::{ConversionContext, ConvertedSegment};

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
        c.candidates.provisional = false;
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
            let lattice = self.lattice_cache.get_or_build(&reading, &*self.dict);

            let h_guard = self.history.as_ref().and_then(|h| h.read().ok());
            let ctx = ConversionContext {
                dict: &*self.dict,
                conn: self.conn.as_deref(),
                history: h_guard.as_deref(),
            };
            let segments = ctx.convert_from_lattice(&lattice);
            drop(h_guard);

            let surface: String = segments.iter().map(|s| s.surface.as_str()).collect();
            let c = self.comp();
            c.candidates.surfaces = vec![surface];
            c.candidates.paths = vec![segments];
            c.candidates.selected = 0;
            // Interim 1-best only; the full N-best arrives asynchronously
            // (or never, if a later key invalidates the request).
            c.candidates.provisional = true;

            let mut resp = build_marked_text(self.comp());
            resp.async_request = Some(AsyncCandidateRequest {
                reading,
                candidate_dispatch: self.config.conversion_mode.candidate_dispatch(),
                lattice: Some(std::sync::Arc::clone(&lattice)),
                epoch: self.epoch,
            });
            return resp;
        } else {
            self.comp().candidates.clear();
        }
        let resp = build_marked_text(self.comp());
        resp
    }

    /// Receive asynchronously generated candidates and update session state.
    ///
    /// `epoch` is the session epoch snapshot carried over from the
    /// originating [`AsyncCandidateRequest`]. Returns `None` (silently
    /// discarding the response) if the response is stale.
    pub fn receive_candidates(
        &mut self,
        epoch: u64,
        reading: &str,
        surfaces: Vec<String>,
        paths: Vec<Vec<ConvertedSegment>>,
    ) -> Option<KeyResponse> {
        let resp = self.receive_candidates_inner(epoch, reading, surfaces, paths);
        if let Some(ref resp) = resp {
            self.note_response(resp);
        }
        resp
    }

    fn receive_candidates_inner(
        &mut self,
        epoch: u64,
        reading: &str,
        surfaces: Vec<String>,
        paths: Vec<Vec<ConvertedSegment>>,
    ) -> Option<KeyResponse> {
        // Correctness gate: the epoch must match. Every state-mutating entry
        // point bumps the epoch under the session lock, so a mismatch means
        // the user did something after this request was issued — even a
        // kana-preserving key (Space/Escape/ForwardDelete) that the reading
        // check below cannot detect.
        if epoch != self.epoch {
            return None;
        }
        // Defense in depth: reading must match current composing state.
        // With session-owned epochs this should never fire when the epoch
        // matches, but it is a cheap second line of defense.
        match &self.state {
            SessionState::Composing(c) if c.kana == reading => {}
            _ => return None,
        }

        // Accepting a response mutates candidate state, so it starts a new
        // epoch like any other state-mutating entry (chained auto-commit
        // requests below snapshot the new value).
        self.bump_epoch();

        let c = self.comp();
        c.candidates.surfaces = surfaces;
        c.candidates.paths = paths;
        c.candidates.selected = 0;
        c.candidates.provisional = false;
        c.stability.track(&c.candidates.paths);

        // Try auto-commit with fresh candidates
        if let Some(auto_resp) = self.try_auto_commit() {
            return Some(auto_resp);
        }

        // No auto-commit: update marked text to Viterbi #1 and show candidates
        Some(build_marked_text_and_candidates(self.comp()))
    }
}
