//! Property-based tests for InputSession state machine.
//!
//! Generates random key-input sequences via proptest and verifies
//! that structural invariants hold after every action.

use proptest::prelude::*;

use lex_core::dict::Dictionary;

use super::make_test_dict;
use crate::types::KeyEvent;
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
    /// Simulate receiving async candidates for current reading.
    ReceiveCandidates,
    /// Switch to Predictive conversion mode.
    SetPredictiveMode,
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
        5 => Just(Action::ReceiveCandidates),
        2 => Just(Action::SetPredictiveMode),
    ]
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
        Action::ReceiveCandidates => {
            if !session.is_composing() {
                return None;
            }
            let reading = session.comp().kana.clone();
            if reading.is_empty() {
                return None;
            }
            let mode = session.config.conversion_mode;
            let cand = mode.generate_candidates(dict, None, None, &reading, 20);
            session.receive_candidates(&reading, cand.surfaces, cand.paths)
        }
        Action::SetPredictiveMode => {
            session.set_conversion_mode(ConversionMode::Predictive);
            None
        }
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

    // 3. Escape from composing → stays Composing (candidates cleared)
    //    IMKit externally calls commitComposition to finalize.
    if was_composing && matches!(action, Action::Escape) {
        assert!(
            session.is_composing(),
            "Escape must keep session in Composing (for IMKit commitComposition), after {:?}",
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
        for action in &actions {
            let was_composing = session.is_composing();
            if let Some(resp) = execute_action(&mut session, action, &*dict) {
                assert_invariants(&session, &resp, action, was_composing);
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
        for action in &actions {
            let was_composing = session.is_composing();
            if let Some(resp) = execute_action(&mut session, action, &*dict) {
                assert_invariants(&session, &resp, action, was_composing);
            }
        }
    }
}
