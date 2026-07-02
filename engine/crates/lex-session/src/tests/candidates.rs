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
        // Simulate a fresh (non-stale) response: snapshot the current epoch.
        let epoch = session.epoch;
        session.receive_candidates(epoch, &reading, cand.surfaces, cand.paths)
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

// --- Session-owned epoch: stale async response rejection ---

/// Regression: a stale async response must not rewind the candidate
/// selection. Space moves the selection without changing the kana, so the
/// reading-match check alone would accept the stale response and reset
/// `selected` to 0; the session-owned epoch rejects it.
#[test]
fn test_stale_epoch_response_does_not_rewind_selection() {
    use lex_core::candidates::generate_candidates;

    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);
    session.set_defer_candidates(true);

    // Type "kyou" — the final key issues an async request with an epoch snapshot.
    let responses = type_string(&mut session, "kyou");
    let stale_epoch = responses
        .last()
        .unwrap()
        .async_request
        .as_ref()
        .expect("deferred mode should request async candidates")
        .epoch;

    // The response arrives while the epoch is unchanged: accepted.
    let cand = generate_candidates(&*dict, None, None, "きょう", 20);
    let r = session.receive_candidates(
        stale_epoch,
        "きょう",
        cand.surfaces.clone(),
        cand.paths.clone(),
    );
    assert!(r.is_some(), "fresh response must be accepted");
    assert!(session.comp().candidates.surfaces.len() > 1);

    // Space moves the selection. Kana is unchanged, so a reading check
    // cannot detect that the following response is stale.
    session.handle_key(KeyEvent::Space);
    assert_eq!(session.comp().candidates.selected, 1);

    // A stale response from before the Space must be silently discarded.
    let r = session.receive_candidates(stale_epoch, "きょう", cand.surfaces, cand.paths);
    assert!(r.is_none(), "stale-epoch response must be rejected");
    assert_eq!(
        session.comp().candidates.selected,
        1,
        "selection must not rewind to 0"
    );
}

/// Regression: a stale async response arriving after Escape (which hides the
/// candidate panel but keeps the composition, kana unchanged) must not
/// re-show the panel.
#[test]
fn test_stale_epoch_response_does_not_reshow_hidden_panel() {
    use lex_core::candidates::generate_candidates;

    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);
    session.set_defer_candidates(true);

    let responses = type_string(&mut session, "kyou");
    let stale_epoch = responses
        .last()
        .unwrap()
        .async_request
        .as_ref()
        .expect("deferred mode should request async candidates")
        .epoch;

    // Escape hides the panel and clears candidates; state stays Composing
    // with the same kana, so the reading still matches.
    let resp = session.handle_key(KeyEvent::Escape);
    assert!(matches!(resp.candidates, CandidateAction::Hide));
    assert!(session.comp().candidates.is_empty());

    // The in-flight response now arrives: epoch mismatch → discarded.
    let cand = generate_candidates(&*dict, None, None, "きょう", 20);
    let r = session.receive_candidates(stale_epoch, "きょう", cand.surfaces, cand.paths);
    assert!(
        r.is_none(),
        "stale response after Escape must not re-show candidates"
    );
    assert!(session.comp().candidates.is_empty());
}

/// `commit()` must invalidate in-flight async responses like `handle_key`
/// does (it previously skipped invalidation entirely).
#[test]
fn test_commit_bumps_epoch() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);
    session.set_defer_candidates(true);

    type_string(&mut session, "kyou");
    let before = session.epoch;
    session.commit();
    assert!(
        session.epoch > before,
        "commit must start a new epoch to invalidate in-flight responses"
    );
}

// --- Provisional candidates: navigation fallback ---

/// Regression: in deferred mode, a fast Space right after typing races with
/// the async N-best — the key press invalidates the in-flight work, and
/// `ensure_candidates` used to skip regeneration because the provisional
/// 1-best made the list non-empty, leaving navigation stuck on a single
/// candidate with no recovery path. Navigation must fall back to synchronous
/// generation when the candidates are provisional.
#[test]
fn test_provisional_navigation_falls_back_to_sync_generation() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);
    session.set_defer_candidates(true);

    // Type "kyou": deferred mode leaves a provisional 1-best while the
    // async N-best is (still) in flight.
    type_string(&mut session, "kyou");
    assert!(session.comp().candidates.provisional);
    assert_eq!(session.comp().candidates.surfaces.len(), 1);

    // Space before the async response arrives. In production this key press
    // also invalidates the in-flight work, so no async refresh would come.
    let resp = session.handle_key(KeyEvent::Space);

    assert!(
        !session.comp().candidates.provisional,
        "navigation must resolve provisional candidates"
    );
    assert!(
        session.comp().candidates.surfaces.len() > 1,
        "sync fallback must produce the full candidate list"
    );
    assert_eq!(session.comp().candidates.selected, 1);
    match resp.candidates {
        CandidateAction::Show { surfaces, selected } => {
            assert!(surfaces.len() > 1);
            assert_eq!(selected, 1);
        }
        _ => panic!("Space over provisional candidates must show the candidate panel"),
    }
}

/// A fresh async response still lands normally on provisional candidates
/// (the fallback only kicks in on navigation, not on Enter/Tab commits).
#[test]
fn test_provisional_cleared_by_async_response() {
    use lex_core::candidates::generate_candidates;

    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);
    session.set_defer_candidates(true);

    type_string(&mut session, "kyou");
    assert!(session.comp().candidates.provisional);

    let cand = generate_candidates(&*dict, None, None, "きょう", 20);
    let epoch = session.epoch;
    let r = session.receive_candidates(epoch, "きょう", cand.surfaces, cand.paths);
    assert!(r.is_some());
    assert!(
        !session.comp().candidates.provisional,
        "accepted async response must mark candidates as ready"
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
        // Simulate a fresh (non-stale) response: snapshot the current epoch.
        let epoch = session.epoch;
        session.receive_candidates(epoch, &reading, cand.surfaces, cand.paths)
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

#[test]
fn test_auto_commit_skips_single_kana_first_segment() {
    use lex_core::candidates::generate_candidates;

    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);
    session.set_defer_candidates(true);

    fn complete_cycle(session: &mut InputSession, dict: &dyn Dictionary) -> Option<KeyResponse> {
        let reading = session.comp().kana.clone();
        if reading.is_empty() {
            return None;
        }
        let cand = generate_candidates(dict, None, None, &reading, 20);
        // Simulate a fresh (non-stale) response: snapshot the current epoch.
        let epoch = session.epoch;
        session.receive_candidates(epoch, &reading, cand.surfaces, cand.paths)
    }

    // Type a reading where Viterbi may produce a single-kana first segment.
    // Even if stability threshold is met, auto-commit must NOT fire when the
    // committed reading is shorter than 2 kana.
    type_string(&mut session, "ji");
    complete_cycle(&mut session, &*dict);
    type_string(&mut session, "ji");
    complete_cycle(&mut session, &*dict);
    type_string(&mut session, "ji");
    complete_cycle(&mut session, &*dict);
    type_string(&mut session, "ji");
    let r = complete_cycle(&mut session, &*dict);

    // Verify auto-commit preconditions are met (stability >= 3),
    // so this test is actually exercising the min-length guard.
    assert!(
        session.comp().stability.count >= 3,
        "stability count should reach threshold; got {}",
        session.comp().stability.count
    );

    // Even after many cycles, a single-kana first segment must not auto-commit.
    if let Some(ref resp) = r {
        assert!(
            resp.commit.is_none(),
            "auto-commit should not commit when the first segment would be a single kana"
        );
    }
    assert_eq!(
        session.comp().kana,
        "じじじじ",
        "composing kana should remain unchanged when auto-commit is skipped"
    );
}
