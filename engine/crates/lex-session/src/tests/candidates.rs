use super::*;
use crate::types::{CandidateAction, CandidateDispatch, KeyEvent};
use crate::ConversionMode;

#[test]
fn test_candidates_generated() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);

    type_string(&mut session, "kyou");
    assert!(!session.comp().candidates.is_empty());
    assert!(!session.comp().candidates.paths.is_empty());
}

// --- Predictive conversion mode ---

#[test]
fn test_predictive_mode_generates_candidates() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);
    session.set_conversion_mode(ConversionMode::Predictive);

    type_string(&mut session, "kyou");
    // Predictive mode uses Viterbi base + bigram chaining
    assert!(!session.comp().candidates.is_empty());
    // Without history, behaves like standard (Viterbi-based)
    assert!(!session.comp().candidates.paths.is_empty());
    // Kana should be present as fallback
    assert!(session
        .comp()
        .candidates
        .surfaces
        .contains(&"きょう".to_string()));
}

#[test]
fn test_predictive_mode_tab_commits() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);
    session.set_conversion_mode(ConversionMode::Predictive);

    type_string(&mut session, "kyou");
    assert!(session.is_composing());

    let resp = session.handle_key(KeyEvent::Tab);
    assert!(resp.consumed);
    // Tab in Predictive mode commits (not toggles submode)
    assert!(resp.commit.is_some());
    assert!(!session.is_composing());
}

#[test]
fn test_predictive_mode_space_cycles() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);
    session.set_conversion_mode(ConversionMode::Predictive);

    type_string(&mut session, "kyou");
    let count = session.comp().candidates.surfaces.len();
    assert!(count > 1);
    assert_eq!(session.comp().candidates.selected, 0);

    // Space cycles candidates in Predictive mode too
    session.handle_key(KeyEvent::Space);
    assert_eq!(session.comp().candidates.selected, 1);
}

#[test]
fn test_predictive_mode_deferred_dispatch() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);
    session.set_conversion_mode(ConversionMode::Predictive);
    session.set_defer_candidates(true);

    // Type "ka" to trigger deferred candidate generation
    session.handle_key(KeyEvent::text("k"));
    let resp = session.handle_key(KeyEvent::text("a"));
    // Predictive mode uses prediction-specific generation
    if let Some(req) = resp.async_request {
        assert_eq!(
            req.candidate_dispatch,
            CandidateDispatch::Predictive,
            "predictive uses prediction_only generation"
        );
    }
}

#[test]
fn test_standard_mode_deferred_dispatch() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);
    session.set_conversion_mode(ConversionMode::Standard);
    session.set_defer_candidates(true);

    // Type "ka" one char at a time to capture deferred response
    session.handle_key(KeyEvent::text("k"));
    let resp = session.handle_key(KeyEvent::text("a"));
    if let Some(req) = resp.async_request {
        assert_eq!(
            req.candidate_dispatch,
            CandidateDispatch::Standard,
            "standard dispatch should be Standard"
        );
    }
}

#[test]
fn test_conversion_mode_switch() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);

    // Default is Standard
    assert_eq!(session.config.conversion_mode, ConversionMode::Standard);

    // Switch to Predictive
    session.set_conversion_mode(ConversionMode::Predictive);
    assert_eq!(session.config.conversion_mode, ConversionMode::Predictive);

    type_string(&mut session, "kyou");
    // Tab should commit (Predictive behavior)
    let resp = session.handle_key(KeyEvent::Tab);
    assert!(resp.commit.is_some());
    assert!(!session.is_composing());

    // Switch back to Standard
    session.set_conversion_mode(ConversionMode::Standard);
    assert_eq!(session.config.conversion_mode, ConversionMode::Standard);

    type_string(&mut session, "kyou");
    // Tab in Standard mode now commits (no more submode toggle)
    let resp = session.handle_key(KeyEvent::Tab);
    assert!(resp.commit.is_some());
    assert!(!session.is_composing());
}

#[test]
fn test_deferred_auto_commit_shows_provisional_candidates() {
    use lex_core::candidates::generate_candidates;

    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);
    session.set_defer_candidates(true);

    // Helper: complete one async candidate cycle.
    // Returns the response from receive_candidates (None if stale).
    fn complete_cycle(session: &mut InputSession, dict: &dyn Dictionary) -> Option<KeyResponse> {
        let reading = session.comp().kana.clone();
        if reading.is_empty() {
            return None;
        }
        let cand = generate_candidates(dict, None, None, &reading, 20);
        session.receive_candidates(&reading, cand.surfaces, cand.paths)
    }

    // Build up "きょうはいいてんき" with async cycles after each romaji group.
    // Each cycle increments the stability counter (first segment = "きょう").
    type_string(&mut session, "kyou"); // "きょう"
    let r = complete_cycle(&mut session, &*dict);
    assert!(r.is_some());
    assert!(r.unwrap().commit.is_none(), "no auto-commit yet");

    type_string(&mut session, "ha"); // "きょうは"
    let r = complete_cycle(&mut session, &*dict);
    assert!(r.is_some());
    assert!(r.unwrap().commit.is_none(), "no auto-commit yet");

    type_string(&mut session, "ii"); // "きょうはいい"
    let r = complete_cycle(&mut session, &*dict);
    assert!(r.is_some());
    assert!(
        r.unwrap().commit.is_none(),
        "no auto-commit yet (< 4 segments)"
    );

    type_string(&mut session, "tenki"); // "きょうはいいてんき"
    let r = complete_cycle(&mut session, &*dict);
    let resp = r.expect("receive_candidates should return a response");

    // Auto-commit should fire: first segment committed, remaining shown
    assert!(
        resp.commit.is_some(),
        "auto-commit should produce commit_text"
    );
    assert!(
        matches!(resp.candidates, CandidateAction::Show { .. }),
        "deferred auto-commit should show provisional candidates (not hide)"
    );
    if let CandidateAction::Show { ref surfaces, .. } = resp.candidates {
        assert!(
            !surfaces.is_empty(),
            "deferred auto-commit should provide provisional candidates"
        );
    }
    // Async generation should still be requested for proper results
    assert!(
        resp.async_request.is_some(),
        "deferred auto-commit should request async candidate generation"
    );
    // Session state should also hold provisional candidates
    // so that candidate navigation works during the async phase.
    assert!(
        !session.comp().candidates.surfaces.is_empty(),
        "session should retain provisional candidates for navigation"
    );
}

#[test]
fn test_predictive_mode_no_auto_commit() {
    use lex_core::candidates::generate_candidates;

    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);
    session.set_conversion_mode(ConversionMode::Predictive);
    session.set_defer_candidates(true);

    fn complete_cycle(session: &mut InputSession, dict: &dyn Dictionary) -> Option<KeyResponse> {
        let reading = session.comp().kana.clone();
        if reading.is_empty() {
            return None;
        }
        let cand = generate_candidates(dict, None, None, &reading, 20);
        session.receive_candidates(&reading, cand.surfaces, cand.paths)
    }

    // Build up enough input that would trigger auto-commit in Standard mode
    type_string(&mut session, "kyou");
    complete_cycle(&mut session, &*dict);
    type_string(&mut session, "ha");
    complete_cycle(&mut session, &*dict);
    type_string(&mut session, "ii");
    complete_cycle(&mut session, &*dict);
    type_string(&mut session, "tenki");
    let r = complete_cycle(&mut session, &*dict);

    // In Predictive mode, auto-commit should NOT fire
    if let Some(resp) = r {
        assert!(
            resp.commit.is_none(),
            "predictive mode should not auto-commit"
        );
    }
}
