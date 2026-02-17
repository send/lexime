use super::*;
use crate::ConversionMode;

#[test]
fn test_ghosttext_tab_accepts_ghost() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);
    session.set_conversion_mode(ConversionMode::GhostText);

    // Simulate ghost text being received
    session.ghost.text = Some("ですね".to_string());
    session.ghost.generation = 1;

    // Tab should accept ghost text
    let resp = session.handle_key(key::TAB, "", 0);
    assert!(resp.consumed);
    assert_eq!(resp.commit.as_deref(), Some("ですね"));
    assert!(session.ghost.text.is_none());
}

#[test]
fn test_ghosttext_tab_no_ghost_composing_commits() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);
    session.set_conversion_mode(ConversionMode::GhostText);

    // Type something (no ghost text)
    type_string(&mut session, "kyou");
    assert!(session.is_composing());

    // Tab commits in GhostText mode (like Predictive)
    let resp = session.handle_key(key::TAB, "", 0);
    assert!(resp.commit.is_some());
    assert!(!session.is_composing());
}

#[test]
fn test_ghosttext_input_clears_ghost() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);
    session.set_conversion_mode(ConversionMode::GhostText);

    // Simulate ghost text
    session.ghost.text = Some("ですね".to_string());

    // Type a character → should clear ghost
    let resp = session.handle_key(0, "k", 0);
    assert!(resp.consumed);
    assert!(session.ghost.text.is_none());
    // Ghost clear signaled in response
    assert_eq!(resp.ghost_text.as_deref(), Some(""));
}

#[test]
fn test_ghosttext_stale_generation_rejected() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);
    session.set_conversion_mode(ConversionMode::GhostText);
    session.ghost.generation = 5;

    // Stale generation
    let result = session.receive_ghost_text(3, "stale text".to_string());
    assert!(result.is_none());
    assert!(session.ghost.text.is_none());

    // Correct generation
    let result = session.receive_ghost_text(5, "correct text".to_string());
    assert!(result.is_some());
    assert_eq!(session.ghost.text.as_deref(), Some("correct text"));
}

#[test]
fn test_ghosttext_rejected_while_composing() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);
    session.set_conversion_mode(ConversionMode::GhostText);
    session.ghost.generation = 1;

    type_string(&mut session, "kyou");
    assert!(session.is_composing());

    // Should reject ghost text while composing
    let result = session.receive_ghost_text(1, "text".to_string());
    assert!(result.is_none());
}

#[test]
fn test_standard_mode_no_ghost() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);
    session.set_conversion_mode(ConversionMode::Standard);
    session.ghost.generation = 1;

    // Standard mode rejects ghost text
    let result = session.receive_ghost_text(1, "text".to_string());
    assert!(result.is_none());
}

#[test]
fn test_ghosttext_commit_requests_ghost() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);
    session.set_conversion_mode(ConversionMode::GhostText);

    type_string(&mut session, "kyou");
    let resp = session.handle_key(key::ENTER, "", 0);
    assert!(resp.commit.is_some());
    // Should request ghost generation
    assert!(resp.ghost_request.is_some());
    let req = resp.ghost_request.unwrap();
    assert!(!req.context.is_empty());
    assert_eq!(req.generation, 1);
}

#[test]
fn test_ghosttext_accept_then_requests_more() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);
    session.set_conversion_mode(ConversionMode::GhostText);

    // Simulate ghost text
    session.ghost.text = Some("ですね".to_string());
    session.ghost.generation = 1;

    // Accept ghost
    let resp = session.handle_key(key::TAB, "", 0);
    assert_eq!(resp.commit.as_deref(), Some("ですね"));
    // Should request another ghost generation
    assert!(resp.ghost_request.is_some());
    assert_eq!(resp.ghost_request.unwrap().generation, 2);
}
