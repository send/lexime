use super::types::{CandidateAction, Composition, KeyResponse};

/// Build a response showing only marked text (no candidates).
pub(super) fn build_marked_text(comp: &Composition) -> KeyResponse {
    KeyResponse::consumed().with_marked(comp.display_kana())
}

/// Build a response showing marked text and candidate panel.
pub(super) fn build_marked_text_and_candidates(comp: &Composition) -> KeyResponse {
    let mut resp = KeyResponse::consumed().with_marked(comp.display_kana());

    if !comp.candidates.is_empty() {
        resp.candidates = CandidateAction::Show {
            surfaces: comp.candidates.surfaces.clone(),
            selected: comp.candidates.selected as u32,
        };
    }

    resp
}

/// Build a response for candidate selection.
pub(super) fn build_candidate_selection(comp: &Composition) -> KeyResponse {
    let mut resp = KeyResponse::consumed().with_marked(comp.display());
    resp.candidates = CandidateAction::Show {
        surfaces: comp.candidates.surfaces.clone(),
        selected: comp.candidates.selected as u32,
    };
    resp
}
