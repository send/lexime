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
    Arc::new(SnippetStore::new(entries, resolver).unwrap())
}

fn make_session_with_snippets() -> InputSession {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict, None, None);
    session.set_snippet_store(Some(make_snippet_store()));
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

#[test]
fn test_snippet_trigger_marks_selected_key() {
    // Entering snippet mode must emit non-empty marked text so Chromium/
    // Electron hosts stay in composition — otherwise the confirming Enter is
    // dispatched to the web page (e.g. submits a chat message). Before any
    // filter is typed, that text is the selected snippet's key.
    let mut session = make_session_with_snippets();
    let resp = session.handle_key(KeyEvent::SnippetTrigger);
    // Keys sort as: email, gh, gmail, sig → selected 0 = "email".
    assert_eq!(resp.marked.expect("marked text").text, "email");
}

#[test]
fn test_snippet_navigate_updates_marked_preview() {
    let mut session = make_session_with_snippets();
    session.handle_key(KeyEvent::SnippetTrigger);
    let resp = session.handle_key(KeyEvent::ArrowDown); // selected 1 = "gh"
    assert_eq!(resp.marked.expect("marked text").text, "gh");
}

#[test]
fn test_snippet_marked_text_stays_nonempty() {
    // Core invariant behind the Chromium Enter-leak fix: while in snippet mode
    // the emitted marked text is never empty, across trigger / navigate /
    // filter / backspace-to-empty.
    let mut session = make_session_with_snippets();

    let steps = [
        KeyEvent::SnippetTrigger,
        KeyEvent::ArrowDown,
        KeyEvent::text("g"), // filter → "g"
        KeyEvent::Backspace, // filter → "" (key preview again)
    ];
    for step in steps {
        let resp = session.handle_key(step);
        assert!(session.is_composing());
        let marked = resp.marked.expect("snippet mode must emit marked text");
        assert!(
            !marked.text.is_empty(),
            "marked text must stay non-empty in snippet mode"
        );
    }
}

#[test]
fn test_snippet_trigger_empty_store_does_not_enter_mode() {
    // With zero snippets there is nothing to preview, so we must not enter
    // snippet mode with an empty composition (which would leak the confirming
    // key to the host). The trigger is consumed but the session stays idle.
    let dict = make_test_dict();
    let mut session = InputSession::new(dict, None, None);
    let empty_store =
        SnippetStore::new(HashMap::new(), VariableResolver::new(HashMap::new())).unwrap();
    session.set_snippet_store(Some(Arc::new(empty_store)));

    let resp = session.handle_key(KeyEvent::SnippetTrigger);
    assert!(resp.consumed);
    assert!(!session.is_composing());
    assert!(resp.marked.expect("marked text").text.is_empty());
    assert!(matches!(resp.candidates, CandidateAction::Hide));
}

#[test]
fn test_snippet_browse_survives_store_swap() {
    // A snippet browse is a transaction against the store snapshot it began
    // with. Swapping the live store mid-browse (e.g. snippetsDidReload firing
    // while a picker is up) must not disturb the active picker or emit empty
    // marked text — otherwise the confirming key could leak to the host.
    let mut session = make_session_with_snippets();
    session.handle_key(KeyEvent::SnippetTrigger);
    assert!(session.is_composing());

    // Swap in an empty store mid-browse (worst case).
    let empty = SnippetStore::new(HashMap::new(), VariableResolver::new(HashMap::new())).unwrap();
    session.set_snippet_store(Some(Arc::new(empty)));

    // Still browsing the original snapshot; marked text stays non-empty.
    assert!(session.is_composing());
    let resp = session.handle_key(KeyEvent::text("g")); // filters the snapshot, not the empty store
    assert!(!resp.marked.expect("marked text").text.is_empty());
    match resp.candidates {
        CandidateAction::Show { surfaces, .. } => assert_eq!(surfaces.len(), 2), // gh, gmail
        _ => panic!("expected Show candidates from the snapshot"),
    }

    // Backspace to empty filter — snapshot repopulates, still non-empty.
    let resp = session.handle_key(KeyEvent::Backspace);
    assert!(session.is_composing());
    assert!(!resp.marked.expect("marked text").text.is_empty());

    // Confirm inserts from the snapshot, not the swapped-in empty store.
    session.handle_key(KeyEvent::text("g"));
    session.handle_key(KeyEvent::text("h"));
    let resp = session.handle_key(KeyEvent::Enter);
    assert_eq!(resp.commit, Some("https://github.com/".to_string()));
}
