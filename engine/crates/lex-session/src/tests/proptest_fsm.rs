//! Property-based tests for InputSession state machine.
//!
//! Generates random key-input sequences via proptest and verifies
//! that structural invariants hold after every action.

use std::collections::HashMap;
use std::sync::Arc;

use proptest::prelude::*;

use lex_core::dict::Dictionary;
use lex_core::snippets::{SnippetStore, SnippetVariable, VariableResolver};

use super::make_test_dict;
use crate::types::{KeyEvent, SessionState};
use crate::{CandidateAction, ConversionMode, InputSession};

// ---------------------------------------------------------------------------
// Action enum — models every user-facing operation
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Action {
    TypeRomaji(char),
    Enter,
    Space,
    Backspace,
    Escape,
    Tab,
    ArrowDown,
    ArrowUp,
    Eisu,
    Kana,
    TypeDigit(char),
    TypePunctuation(char),
    ForwardDelete,
    /// Simulate receiving async candidates for current reading.
    ReceiveCandidates,
    /// Switch to Predictive conversion mode.
    SetPredictiveMode,
    /// Trigger snippet mode (opens the picker; commits any composing first).
    SnippetTrigger,
}

// ---------------------------------------------------------------------------
// Strategy: weighted random Action generation
// ---------------------------------------------------------------------------

fn arb_romaji_char() -> impl Strategy<Value = char> {
    // Vowels at higher weight for more realistic romaji
    prop_oneof![
        3 => Just('a'),
        3 => Just('i'),
        3 => Just('u'),
        3 => Just('e'),
        3 => Just('o'),
        1 => prop::sample::select(vec![
            'k', 's', 't', 'n', 'h', 'm', 'y', 'r', 'w',
            'g', 'z', 'd', 'b', 'p', 'c', 'f', 'j', 'l', 'v', 'x', 'q',
        ]),
    ]
}

fn arb_action() -> impl Strategy<Value = Action> {
    prop_oneof![
        50 => arb_romaji_char().prop_map(Action::TypeRomaji),
        8 => Just(Action::Enter),
        8 => Just(Action::Space),
        8 => Just(Action::Backspace),
        5 => Just(Action::Escape),
        5 => Just(Action::Tab),
        3 => Just(Action::ArrowDown),
        3 => Just(Action::ArrowUp),
        2 => Just(Action::Eisu),
        2 => Just(Action::Kana),
        3 => prop::sample::select(vec!['0', '1', '2', '3', '4', '5', '6', '7', '8', '9'])
            .prop_map(Action::TypeDigit),
        3 => prop::sample::select(vec!['.', ',', '/', '-'])
            .prop_map(Action::TypePunctuation),
        3 => Just(Action::ForwardDelete),
        5 => Just(Action::ReceiveCandidates),
        2 => Just(Action::SetPredictiveMode),
        4 => Just(Action::SnippetTrigger),
    ]
}

// ---------------------------------------------------------------------------
// Snippet store fixture
// ---------------------------------------------------------------------------

/// Store used to make snippet states reachable in generated sequences.
///
/// Keys start with characters the romaji strategy emits frequently (vowels /
/// common consonants) so generated `Text` actions actually narrow, hit, and
/// miss the filter. `email`/`eta` share the `e` prefix so filtering exercises
/// multi-match narrowing down to a single match; `sig` carries a variable so
/// confirmation exercises expansion. All keys and bodies are non-empty (the
/// constructor rejects empty keys), so `unwrap` cannot fire.
fn make_test_snippet_store() -> Arc<SnippetStore> {
    let mut entries = HashMap::new();
    entries.insert("addr".to_string(), "123 Example St".to_string());
    entries.insert("eta".to_string(), "on my way".to_string());
    entries.insert("email".to_string(), "user@example.com".to_string());
    entries.insert("sig".to_string(), "Best regards, $name".to_string());
    entries.insert("ok".to_string(), "sounds good".to_string());

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

// ---------------------------------------------------------------------------
// Execute an Action against the session
// ---------------------------------------------------------------------------

fn execute_action(
    session: &mut InputSession,
    action: &Action,
    dict: &dyn Dictionary,
) -> Option<crate::KeyResponse> {
    match action {
        Action::TypeRomaji(ch) => Some(session.handle_key(KeyEvent::text(&ch.to_string()))),
        Action::Enter => Some(session.handle_key(KeyEvent::Enter)),
        Action::Space => Some(session.handle_key(KeyEvent::Space)),
        Action::Backspace => Some(session.handle_key(KeyEvent::Backspace)),
        Action::Escape => Some(session.handle_key(KeyEvent::Escape)),
        Action::Tab => Some(session.handle_key(KeyEvent::Tab)),
        Action::ArrowDown => Some(session.handle_key(KeyEvent::ArrowDown)),
        Action::ArrowUp => Some(session.handle_key(KeyEvent::ArrowUp)),
        Action::Eisu => Some(session.handle_key(KeyEvent::SwitchToDirectInput)),
        Action::Kana => Some(session.handle_key(KeyEvent::SwitchToJapanese)),
        Action::TypeDigit(ch) => Some(session.handle_key(KeyEvent::text(&ch.to_string()))),
        Action::TypePunctuation(ch) => Some(session.handle_key(KeyEvent::text(&ch.to_string()))),
        Action::ForwardDelete => Some(session.handle_key(KeyEvent::ForwardDelete)),
        Action::ReceiveCandidates => {
            // Only the plain Composing state has a `comp()`; snippet mode also
            // reports `is_composing()` but `comp()` panics there.
            if !matches!(session.state, SessionState::Composing(_)) {
                return None;
            }
            let reading = session.comp().kana.clone();
            if reading.is_empty() {
                return None;
            }
            let mode = session.config.conversion_mode;
            let cand = mode.generate_candidates(dict, None, None, &reading, 20);
            // Simulate a fresh (non-stale) response: snapshot the current epoch.
            let epoch = session.epoch;
            session.receive_candidates(epoch, &reading, cand.surfaces, cand.paths)
        }
        Action::SetPredictiveMode => {
            session.set_conversion_mode(ConversionMode::Predictive);
            None
        }
        Action::SnippetTrigger => Some(session.handle_key(KeyEvent::SnippetTrigger)),
    }
}

// ---------------------------------------------------------------------------
// Invariant checks — run after every action
// ---------------------------------------------------------------------------

fn assert_invariants(
    session: &InputSession,
    resp: &crate::KeyResponse,
    action: &Action,
    was_composing: bool,
    was_snippet: bool,
) {
    // 1. Idle → composed_string is empty
    if !session.is_composing() {
        assert!(
            session.composed_string().is_empty(),
            "Idle session must have empty composed_string, got {:?} after {:?}",
            session.composed_string(),
            action,
        );
    }

    // 1b. Composing → composed_string is non-empty (the composing-side dual of
    //     #1). While is_composing() holds — plain Composing OR Snippet mode —
    //     the inline marked text (composed_string) must never be empty: an empty
    //     marked string drops Chromium/Electron hosts out of IME composition, so
    //     the confirming Enter leaks to the web page (e.g. submits a chat
    //     message). This is the invariant behind fix/snippet-enter-leak. It
    //     would catch e.g. prefix_search("") no longer returning all_entries
    //     (the backspace-to-empty-filter path repopulates matches through it).
    if session.is_composing() {
        assert!(
            !session.composed_string().is_empty(),
            "Composing session must have non-empty composed_string after {:?}",
            action,
        );
    }

    // 2. Enter from composing → Idle
    //    (Enter calls commit_current_state → reset_state → Idle.)
    //    Escape does NOT transition: it stays Composing for IMKit commitComposition.
    if was_composing && matches!(action, Action::Enter) {
        assert!(
            !session.is_composing(),
            "Enter must transition from Composing to Idle, after {:?}",
            action,
        );
    }

    // 3. Escape from *plain* composing → stays Composing (candidates cleared).
    //    IMKit externally calls commitComposition to finalize. Snippet mode is
    //    excluded — there Escape cancels the picker (see 3b).
    if was_composing && !was_snippet && matches!(action, Action::Escape) {
        assert!(
            session.is_composing(),
            "Escape must keep plain composing in Composing (for IMKit commitComposition), after {:?}",
            action,
        );
    }

    // 3b. Escape from snippet mode → cancels back to Idle (the picker is torn
    //     down; no IMKit commitComposition is expected for a snippet browse).
    if was_snippet && matches!(action, Action::Escape) {
        assert!(
            !session.is_composing(),
            "Escape must cancel snippet mode back to Idle, after {:?}",
            action,
        );
    }

    // 4. Candidate index bounds
    if let CandidateAction::Show { surfaces, selected } = &resp.candidates {
        assert!(
            !surfaces.is_empty(),
            "CandidateAction::Show must have non-empty surfaces after {:?}",
            action,
        );
        assert!(
            (*selected as usize) < surfaces.len(),
            "selected ({}) out of bounds for {} candidates after {:?}",
            selected,
            surfaces.len(),
            action,
        );
    }

    // 5. Eisu → enters ABC passthrough (no longer sets switch_to_abc)
    if matches!(action, Action::Eisu) {
        assert!(
            session.is_abc_passthrough(),
            "Eisu key must activate ABC passthrough, after {:?}",
            action,
        );
        assert!(
            !resp.side_effects.switch_to_abc,
            "Eisu key must not set switch_to_abc, after {:?}",
            action,
        );
    }

    // 5b. Kana → exits ABC passthrough
    if matches!(action, Action::Kana) {
        assert!(
            !session.is_abc_passthrough(),
            "Kana key must deactivate ABC passthrough, after {:?}",
            action,
        );
    }

    // 6. Escape → never shows candidates
    if matches!(action, Action::Escape) {
        if let CandidateAction::Show { .. } = &resp.candidates {
            panic!(
                "Escape must not show candidates, got Show after {:?}",
                action,
            );
        }
    }

    // 7. Committed text is non-empty when present
    if let Some(text) = &resp.commit {
        assert!(
            !text.is_empty(),
            "Committed text must be non-empty after {:?}",
            action,
        );
    }

    // 8. Async candidate request implies composing state
    if resp.async_request.is_some() {
        assert!(
            session.is_composing(),
            "Async candidate request must imply composing state, after {:?}",
            action,
        );
    }
}

// ---------------------------------------------------------------------------
// proptest entry point
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn session_invariants_hold(actions in prop::collection::vec(arb_action(), 1..100)) {
        let dict = make_test_dict();
        let mut session = InputSession::new(dict.clone(), None, None);
        session.set_snippet_store(Some(make_test_snippet_store()));
        for action in &actions {
            let was_composing = session.is_composing();
            let was_snippet = matches!(session.state, SessionState::Snippet(_));
            if let Some(resp) = execute_action(&mut session, action, &*dict) {
                assert_invariants(&session, &resp, action, was_composing, was_snippet);
            }
        }
    }

    #[test]
    fn session_invariants_with_deferred_candidates(
        actions in prop::collection::vec(arb_action(), 1..100)
    ) {
        let dict = make_test_dict();
        let mut session = InputSession::new(dict.clone(), None, None);
        session.set_defer_candidates(true);
        session.set_snippet_store(Some(make_test_snippet_store()));
        for action in &actions {
            let was_composing = session.is_composing();
            let was_snippet = matches!(session.state, SessionState::Snippet(_));
            if let Some(resp) = execute_action(&mut session, action, &*dict) {
                assert_invariants(&session, &resp, action, was_composing, was_snippet);
            }
        }
    }
}
