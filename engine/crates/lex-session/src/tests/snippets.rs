use super::*;
use crate::types::{CandidateAction, KeyEvent};

/// Entries these example tests assert against by exact match count and sort
/// position (e.g. `"g"` → exactly `gh`, `gmail`; index 0 of all four →
/// `email`). Changing this table breaks those expectations by design.
const SNIPPET_ENTRIES: &[(&str, &str)] = &[
    ("gh", "https://github.com/"),
    ("gmail", "https://mail.google.com/"),
    ("email", "user@example.com"),
    ("sig", "Best regards, $name"),
];

fn make_session_with_snippets() -> InputSession {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict, None, None);
    session.set_snippet_store(Some(make_snippet_store(SNIPPET_ENTRIES)));
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
fn test_snippet_trigger_without_store_commits_composing() {
    // No store configured, but a composition is in progress: the trigger key
    // has to reach the host, so it commits (like ModifiedKey) and does not
    // consume. Committing is what keeps the host out of an empty composition —
    // the shape that leaks the next key on Chromium/Electron hosts.
    let dict = make_test_dict();
    let mut session = InputSession::new(dict, None, None);
    type_string(&mut session, "ka");
    assert!(session.is_composing());

    let resp = session.handle_key(KeyEvent::SnippetTrigger);
    assert!(!resp.consumed);
    assert!(!session.is_composing());
    assert_eq!(resp.commit, Some("か".to_string()));
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
fn test_snippet_with_empty_body_is_never_offered() {
    // A snippet whose body expands to nothing has nothing to insert, so
    // `prefix_search` drops it before the picker ever sees it — confirming one
    // would commit "". Here it is the only entry, so there is nothing to browse
    // and the trigger behaves as it does with no store at all.
    let dict = make_test_dict();
    let mut session = InputSession::new(dict, None, None);
    session.set_snippet_store(Some(make_snippet_store(&[("blank", "")])));

    let resp = session.handle_key(KeyEvent::SnippetTrigger);
    assert!(!resp.consumed);
    assert!(!session.is_composing());
}

#[test]
fn test_snippet_with_empty_body_is_filtered_out_of_a_live_store() {
    // Same rule when usable entries exist alongside it: the picker shows only
    // the usable ones, so `selected` can never land on an unusable body.
    let dict = make_test_dict();
    let mut session = InputSession::new(dict, None, None);
    session.set_snippet_store(Some(make_snippet_store(&[
        ("blank", ""),
        ("real", "inserted"),
    ])));

    let resp = session.handle_key(KeyEvent::SnippetTrigger);
    match resp.candidates {
        CandidateAction::Show { surfaces, .. } => {
            assert_eq!(surfaces.len(), 1);
            assert!(surfaces[0].starts_with("real\t"));
        }
        _ => panic!("expected Show candidates"),
    }
    assert_eq!(resp.marked.expect("marked text").text, "real");

    let resp = session.handle_key(KeyEvent::Enter);
    assert_eq!(resp.commit, Some("inserted".to_string()));
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
fn test_snippet_trigger_empty_store_behaves_like_no_store() {
    // A store with zero entries is the same situation as no store: nothing to
    // preview, so we must not enter snippet mode with an empty composition
    // (which would leak the confirming key to the host), and the trigger key is
    // not ours — it passes through, exactly as `test_snippet_trigger_without_
    // store_not_consumed` asserts for a `None` store. Emptying snippets.toml
    // must not silently swallow the keybinding.
    let dict = make_test_dict();
    let mut session = InputSession::new(dict, None, None);
    session.set_snippet_store(Some(make_snippet_store(&[])));

    let resp = session.handle_key(KeyEvent::SnippetTrigger);
    assert!(!resp.consumed);
    assert!(!session.is_composing());
}

#[test]
fn test_snippet_retrigger_after_store_emptied_cancels_the_browse() {
    // Regression: pressing the trigger again while browsing, after the live
    // store was emptied (the user deleted their last snippet — snippetsDidReload
    // delivers a present-but-empty store, not `None`), used to return empty
    // marked text while leaving the session in Snippet. The host left
    // composition with the session still live, so the next Enter was consumed
    // here *and* delivered to the web page, committing a body the user never
    // picked. The browse must be torn down instead.
    let mut session = make_session_with_snippets();
    session.handle_key(KeyEvent::SnippetTrigger);
    assert!(session.is_composing());

    session.set_snippet_store(Some(make_snippet_store(&[])));
    let resp = session.handle_key(KeyEvent::SnippetTrigger);

    assert!(
        !session.is_composing(),
        "an empty live store must cancel the browse, not leave it live behind cleared marked text"
    );
    assert!(resp.marked.expect("marked text").text.is_empty());
    assert!(matches!(resp.candidates, CandidateAction::Hide));

    // And the next Enter must have nothing left to commit.
    let resp = session.handle_key(KeyEvent::Enter);
    assert!(resp.commit.is_none());
}

#[test]
fn test_commit_composition_cancels_a_snippet_browse() {
    // IMKit calls commitComposition on focus loss, which can land mid-browse.
    // There is nothing to confirm — the user never picked an entry — so the
    // browse is cancelled, and the empty marked text must be paired with leaving
    // snippet mode or the host would keep inline text with no session behind it.
    let mut session = make_session_with_snippets();
    session.handle_key(KeyEvent::SnippetTrigger);
    session.handle_key(KeyEvent::text("g"));
    assert!(session.is_composing());

    let resp = session.commit();
    assert!(!session.is_composing());
    assert!(resp.commit.is_none(), "a browse has nothing to commit");
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
    session.set_snippet_store(Some(make_snippet_store(&[])));

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
