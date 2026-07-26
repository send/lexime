//! Property-based tests for InputSession state machine.
//!
//! Generates random key-input sequences via proptest and verifies
//! that structural invariants hold after every action.

use proptest::prelude::*;

use lex_core::dict::Dictionary;

use super::{make_snippet_store, make_test_dict};
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

/// Entries that make snippet states reachable in generated sequences.
///
/// The snippet filter takes typed characters verbatim (no romaji conversion),
/// and `arb_romaji_char` draws each vowel far more often than any individual
/// consonant — so vowel-initial keys are the ones generated `Text` actions can
/// realistically hit, narrow, and miss. `email`/`eta` share the `e` prefix so
/// filtering exercises narrowing from several matches down to one. Bodies are
/// non-empty because confirming a snippet commits its body, which invariant #7
/// requires to be non-empty.
const SNIPPET_ENTRIES: &[(&str, &str)] = &[
    ("addr", "123 Example St"),
    ("email", "user@example.com"),
    ("eta", "on my way"),
    ("ok", "sounds good"),
    ("sig", "Best regards, $name"),
];

// ---------------------------------------------------------------------------
// Pre-action state, for the transition invariants
// ---------------------------------------------------------------------------

/// The state kind observed *before* an action ran.
///
/// A single discriminant rather than a pair of booleans: `is_composing()` is
/// the union of `Composing` and `Snippet`, so booleans would admit the illegal
/// "not composing but snippet" combination and force the plain-composing case
/// to be written as a subtraction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PrevState {
    Idle,
    Composing,
    Snippet,
}

impl PrevState {
    fn of(session: &InputSession) -> Self {
        match session.state {
            SessionState::Idle => Self::Idle,
            SessionState::Composing(_) => Self::Composing,
            SessionState::Snippet(_) => Self::Snippet,
        }
    }

    /// Both composing kinds — the union `InputSession::is_composing()` reports.
    fn is_composing(self) -> bool {
        matches!(self, Self::Composing | Self::Snippet)
    }
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
    prev: PrevState,
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

    // 1b. Composing → the composition is non-empty (the composing-side dual of
    //     #1), asserted on both surfaces it is observable through.
    //
    //     Why it matters: Chromium/Electron hosts derive
    //     `KeyboardEvent.isComposing` from whether marked text is present, so
    //     an empty marked string drops them out of composition and the
    //     confirming Enter also reaches the web page (e.g. sends the chat
    //     message). This is the invariant behind fix/snippet-enter-leak.
    //
    //     Both surfaces are checked because they regress independently:
    //     `composed_string()` re-derives the text from session state, so it
    //     catches e.g. `prefix_search("")` no longer returning all_entries
    //     (which the backspace-to-empty-filter path repopulates through),
    //     while `resp.marked` is what actually reaches the host as
    //     `LexEvent::SetMarkedText`, so it catches an emit path that goes
    //     empty while the state behind it is still intact.
    if session.is_composing() {
        assert!(
            !session.composed_string().is_empty(),
            "Composing session must have non-empty composed_string after {:?}",
            action,
        );
        // `marked: None` means "leave the host's marked text as-is", which is
        // legitimate; only an explicitly emitted empty string is the bug.
        if let Some(marked) = &resp.marked {
            assert!(
                !marked.text.is_empty(),
                "Composing session must not emit empty marked text after {:?}",
                action,
            );
        }
    }

    // 2. Enter from composing → Idle
    //    (Enter calls commit_current_state → reset_state → Idle.)
    //    Escape does NOT transition: it stays Composing for IMKit commitComposition.
    if prev.is_composing() && matches!(action, Action::Enter) {
        assert!(
            !session.is_composing(),
            "Enter must transition from Composing to Idle, after {:?}",
            action,
        );
    }

    // 3. Escape from *plain* composing → stays Composing (candidates cleared).
    //    IMKit externally calls commitComposition to finalize. Snippet mode is
    //    excluded — there Escape cancels the picker (see 3b).
    if prev == PrevState::Composing && matches!(action, Action::Escape) {
        assert!(
            session.is_composing(),
            "Escape must keep plain composing in Composing (for IMKit commitComposition), after {:?}",
            action,
        );
    }

    // 3b. Escape from snippet mode → cancels back to Idle (the picker is torn
    //     down; no IMKit commitComposition is expected for a snippet browse).
    if prev == PrevState::Snippet && matches!(action, Action::Escape) {
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
// Sequence driver
// ---------------------------------------------------------------------------

/// Run one generated sequence against a fresh session, checking every
/// invariant after each action. Shared by both proptests so they differ only
/// by the one setting under test.
fn run_sequence(actions: &[Action], defer_candidates: bool) {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);
    session.set_defer_candidates(defer_candidates);
    session.set_snippet_store(Some(make_snippet_store(SNIPPET_ENTRIES)));
    for action in actions {
        let prev = PrevState::of(&session);
        if let Some(resp) = execute_action(&mut session, action, &*dict) {
            assert_invariants(&session, &resp, action, prev);
        }
    }
}

// ---------------------------------------------------------------------------
// proptest entry point
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn session_invariants_hold(actions in prop::collection::vec(arb_action(), 1..100)) {
        run_sequence(&actions, false);
    }

    #[test]
    fn session_invariants_with_deferred_candidates(
        actions in prop::collection::vec(arb_action(), 1..100)
    ) {
        run_sequence(&actions, true);
    }
}
