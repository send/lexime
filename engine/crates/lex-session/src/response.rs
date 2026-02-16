use super::types::{CandidateAction, KeyResponse, MarkedText, Submode};
use super::InputSession;

impl InputSession {
    pub(super) fn make_marked_text_response(&mut self) -> KeyResponse {
        let c = self.comp();
        let display = c.display();
        let mut resp = KeyResponse::consumed();
        resp.marked = Some(MarkedText {
            text: display,
            dashed: c.submode == Submode::English,
        });
        resp
    }

    pub(super) fn make_marked_text_and_candidates_response(&mut self) -> KeyResponse {
        let mut resp = KeyResponse::consumed();

        let c = self.comp();
        let display = c.display();
        resp.marked = Some(MarkedText {
            text: display,
            dashed: c.submode == Submode::English,
        });

        // Candidates
        if !c.candidates.is_empty() {
            resp.candidates = CandidateAction::Show {
                surfaces: c.candidates.surfaces.clone(),
                selected: c.candidates.selected as u32,
            };
        }

        // Try auto-commit (only in sync mode; async mode handles it in receive_candidates)
        if !self.defer_candidates {
            if let Some(auto_resp) = self.try_auto_commit() {
                resp = auto_resp;
            }
        }

        resp
    }

    pub(super) fn make_candidate_selection_response(&mut self) -> KeyResponse {
        let mut resp = KeyResponse::consumed();

        let c = self.comp();
        resp.marked = Some(MarkedText {
            text: c.display(),
            dashed: false,
        });
        resp.candidates = CandidateAction::Show {
            surfaces: c.candidates.surfaces.clone(),
            selected: c.candidates.selected as u32,
        };
        resp
    }
}
