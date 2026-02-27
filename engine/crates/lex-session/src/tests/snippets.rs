use std::collections::HashMap;
use std::sync::Arc;

use lex_core::snippets::{SnippetStore, SnippetVariable, VariableResolver};

use super::*;
use crate::types::{CandidateAction, KeyEvent};

fn make_snippet_store() -> Arc<SnippetStore> {
    let mut entries = HashMap::new();
    entries.insert("gh".to_string(), "https://github.com/".to_string());
    entries.insert("gmail".to_string(), "https://mail.google.com/".to_string());
    entries.insert("email".to_string(), "user@example.com".to_string());
    entries.insert("sig".to_string(), "Best regards, $name".to_string());

    let mut user_vars = HashMap::new();
    user_vars.insert(
        "name".to_string(),
        SnippetVariable::Static {
            value: "Taro".to_string(),
        },
    );
    let resolver = VariableResolver::new(user_vars);
    Arc::new(SnippetStore::new(entries, resolver))
}

fn make_session_with_snippets() -> InputSession {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict, None, None);
    session.set_snippet_store(make_snippet_store());
    session
}

#[test]
fn test_snippet_trigger_enters_snippet_mode() {
    let mut session = make_session_with_snippets();
    let resp = session.handle_key(KeyEvent::SnippetTrigger);
    assert!(resp.consumed);
    assert!(session.is_composing());
    // Should show all candidates
    assert!(matches!(resp.candidates, CandidateAction::Show { .. }));
}

#[test]
fn test_snippet_trigger_without_store_not_consumed() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict, None, None);
    let resp = session.handle_key(KeyEvent::SnippetTrigger);
    assert!(!resp.consumed);
    assert!(!session.is_composing());
}

#[test]
fn test_snippet_filter_narrows_candidates() {
    let mut session = make_session_with_snippets();
    session.handle_key(KeyEvent::SnippetTrigger);

    // Type "g" to filter
    let resp = session.handle_key(KeyEvent::text("g"));
    assert!(resp.consumed);
    match resp.candidates {
        CandidateAction::Show { surfaces, .. } => {
            assert_eq!(surfaces.len(), 2); // gh, gmail
            assert!(surfaces[0].starts_with("gh\t"));
            assert!(surfaces[1].starts_with("gmail\t"));
        }
        _ => panic!("expected Show candidates"),
    }
}

#[test]
fn test_snippet_confirm_inserts_text() {
    let mut session = make_session_with_snippets();
    session.handle_key(KeyEvent::SnippetTrigger);

    // Type "gh" to filter to single match
    session.handle_key(KeyEvent::text("g"));
    session.handle_key(KeyEvent::text("h"));

    let resp = session.handle_key(KeyEvent::Enter);
    assert!(resp.consumed);
    assert_eq!(resp.commit, Some("https://github.com/".to_string()));
    assert!(!session.is_composing());
}

#[test]
fn test_snippet_confirm_with_space() {
    let mut session = make_session_with_snippets();
    session.handle_key(KeyEvent::SnippetTrigger);
    session.handle_key(KeyEvent::text("g"));
    session.handle_key(KeyEvent::text("h"));

    let resp = session.handle_key(KeyEvent::Space);
    assert!(resp.consumed);
    assert_eq!(resp.commit, Some("https://github.com/".to_string()));
}

#[test]
fn test_snippet_escape_cancels() {
    let mut session = make_session_with_snippets();
    session.handle_key(KeyEvent::SnippetTrigger);
    session.handle_key(KeyEvent::text("g"));

    let resp = session.handle_key(KeyEvent::Escape);
    assert!(resp.consumed);
    assert!(!session.is_composing());
    assert!(resp.commit.is_none());
    assert!(matches!(resp.candidates, CandidateAction::Hide));
}

#[test]
fn test_snippet_backspace_empty_cancels() {
    let mut session = make_session_with_snippets();
    session.handle_key(KeyEvent::SnippetTrigger);

    let resp = session.handle_key(KeyEvent::Backspace);
    assert!(resp.consumed);
    assert!(!session.is_composing());
}

#[test]
fn test_snippet_backspace_removes_char() {
    let mut session = make_session_with_snippets();
    session.handle_key(KeyEvent::SnippetTrigger);
    session.handle_key(KeyEvent::text("g"));
    session.handle_key(KeyEvent::text("h"));

    // After backspace, filter should be "g" and show gh + gmail
    let resp = session.handle_key(KeyEvent::Backspace);
    assert!(resp.consumed);
    assert!(session.is_composing());
    match resp.candidates {
        CandidateAction::Show { surfaces, .. } => {
            assert_eq!(surfaces.len(), 2);
        }
        _ => panic!("expected Show candidates"),
    }
}

#[test]
fn test_snippet_navigate() {
    let mut session = make_session_with_snippets();
    session.handle_key(KeyEvent::SnippetTrigger);
    session.handle_key(KeyEvent::text("g"));

    // Navigate down to second candidate
    let resp = session.handle_key(KeyEvent::ArrowDown);
    match resp.candidates {
        CandidateAction::Show { selected, .. } => {
            assert_eq!(selected, 1);
        }
        _ => panic!("expected Show candidates"),
    }

    // Navigate up wraps to last
    let resp = session.handle_key(KeyEvent::ArrowUp);
    match resp.candidates {
        CandidateAction::Show { selected, .. } => {
            assert_eq!(selected, 0);
        }
        _ => panic!("expected Show candidates"),
    }
}

#[test]
fn test_snippet_variable_expansion() {
    let mut session = make_session_with_snippets();
    session.handle_key(KeyEvent::SnippetTrigger);
    session.handle_key(KeyEvent::text("s"));
    session.handle_key(KeyEvent::text("i"));
    session.handle_key(KeyEvent::text("g"));

    let resp = session.handle_key(KeyEvent::Enter);
    assert_eq!(resp.commit, Some("Best regards, Taro".to_string()));
}

#[test]
fn test_snippet_trigger_commits_composing_first() {
    let mut session = make_session_with_snippets();

    // Start composing "ka" → "か"
    type_string(&mut session, "ka");
    assert!(session.is_composing());

    // Trigger snippet — should commit composing first
    let resp = session.handle_key(KeyEvent::SnippetTrigger);
    assert!(resp.consumed);
    // The commit should contain the composed text
    assert!(resp.commit.is_some());
    // And now we're in snippet mode
    assert!(session.is_composing());
}

#[test]
fn test_snippet_navigate_then_filter_resets_selected() {
    let mut session = make_session_with_snippets();
    session.handle_key(KeyEvent::SnippetTrigger);

    // All 4 candidates visible. Navigate to index 3.
    session.handle_key(KeyEvent::ArrowDown); // 1
    session.handle_key(KeyEvent::ArrowDown); // 2
    session.handle_key(KeyEvent::ArrowDown); // 3

    // Type "g" → narrows to 2 candidates (gh, gmail).
    // Selected should reset to 0, not stay at 3 (out of bounds).
    let resp = session.handle_key(KeyEvent::text("g"));
    match resp.candidates {
        CandidateAction::Show { surfaces, selected } => {
            assert_eq!(surfaces.len(), 2);
            assert_eq!(selected, 0);
        }
        _ => panic!("expected Show candidates"),
    }
}

#[test]
fn test_snippet_no_match_shows_no_candidates() {
    let mut session = make_session_with_snippets();
    session.handle_key(KeyEvent::SnippetTrigger);
    session.handle_key(KeyEvent::text("z"));
    session.handle_key(KeyEvent::text("z"));
    session.handle_key(KeyEvent::text("z"));

    // Confirm with no matches should cancel
    let resp = session.handle_key(KeyEvent::Enter);
    assert!(resp.consumed);
    assert!(!session.is_composing());
    assert!(resp.commit.is_none());
}
