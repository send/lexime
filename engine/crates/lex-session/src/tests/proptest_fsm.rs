//! Property-based tests for InputSession state machine.
//!
//! Generates random key-input sequences via proptest and verifies
//! that structural invariants hold after every action.

use proptest::prelude::*;

use lex_core::dict::Dictionary;

use super::{composing_kana, make_snippet_store, make_test_dict};
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
    ]
}

/// `arb_action` plus the snippet trigger, for the snippet-specific test.
///
/// `SnippetTrigger` is deliberately kept out of `arb_action`: it commits any
/// in-progress composition, so mixing it in would truncate the long composing
/// runs the other tests exist to sample (measured over 4096 cases: −31% of runs
/// reaching 10 composing actions, −40% at 20). Keeping the trigger in its own
/// generator leaves those distributions untouched by construction rather than
/// by a case count tuned to compensate.
fn arb_action_with_snippets() -> impl Strategy<Value = Action> {
    // ~3.5% of actions, matching the weight the trigger had when it shared the
    // generator. The ratio is approximate on purpose — nothing here needs to
    // track the exact sum of `arb_action`'s weights.
    prop_oneof![
        27 => arb_action(),
        1 => Just(Action::SnippetTrigger),
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
/// realistically hit. With these entries a *single* generated keystroke reaches
/// every match-count class the invariants distinguish: `a` → 1 match (`addr`),
/// `e` → 2 (`email`, `eta`), `i`/`u` → 0. Narrowing 2 → 1 needs a following
/// `m`/`t` and is correspondingly rare, so it is not what carries the coverage.
/// Bodies are non-empty so that confirming actually reaches the commit path: an
/// empty body cancels instead (see `test_snippet_confirm_empty_body_cancels`).
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
    ///
    /// Phrased as "not idle" rather than as a list of the composing variants so
    /// that a future `SessionState` variant is covered by the transition
    /// invariants by default instead of silently dropping out of them.
    fn is_composing(self) -> bool {
        !matches!(self, Self::Idle)
    }
}

// ---------------------------------------------------------------------------
// Host-side marked text model
// ---------------------------------------------------------------------------

/// Mirror of `SessionCoordinator.currentDisplay` — what the *host* still has as
/// marked text after a response has been applied.
///
/// The contract behind fix/snippet-enter-leak is cumulative, not per-response:
/// `marked: None` means "leave the host's marked text alone", so whether the
/// host is still in composition depends on every response so far, and it has
/// **two** writers. `convert_to_events` emits `Commit` before `SetMarkedText`;
/// on the Swift side (`SessionCoordinator.applyEvents`) `Commit` calls
/// `insertText`, which ends the marked session, and clears `currentDisplay`,
/// while `SetMarkedText` sets it — or clears it when the text is empty. So a
/// response carrying a commit and no marked text takes the host out of
/// composition even though it emitted no empty marked string.
#[derive(Debug, Default)]
struct HostMarked(Option<String>);

impl HostMarked {
    fn apply(&mut self, resp: &crate::KeyResponse) {
        if resp.commit.is_some() {
            self.0 = None;
        }
        if let Some(marked) = &resp.marked {
            self.0 = (!marked.text.is_empty()).then(|| marked.text.clone());
        }
    }

    /// Whether the host would still report itself as composing — i.e. whether
    /// `KeyboardEvent.isComposing` is true in a Chromium/Electron host.
    fn is_composing(&self) -> bool {
        self.0.is_some()
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
            // Async candidates only exist for a plain composition; snippet mode
            // reports `is_composing()` but has none.
            let reading = match composing_kana(session) {
                Some(kana) if !kana.is_empty() => kana.to_string(),
                _ => return None,
            };
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
    host: &HostMarked,
) {
    // 1. Idle → composed_string is empty, and the host has been taken out of
    //    composition too (a live marked string with no session behind it would
    //    leave stale inline text on screen and a stale IMKit composedString).
    if !session.is_composing() {
        assert!(
            session.composed_string().is_empty(),
            "Idle session must have empty composed_string, got {:?} after {:?}",
            session.composed_string(),
            action,
        );
        assert!(
            !host.is_composing(),
            "Idle session must leave the host with no marked text, got {:?} after {:?}",
            host.0,
            action,
        );
    }

    // 1b. Composing → the composition is non-empty (the composing-side dual of
    //     #1), checked against both the session state and the host model.
    //
    //     Why it matters: Chromium/Electron hosts derive
    //     `KeyboardEvent.isComposing` from whether marked text is present, so
    //     losing the marked text drops them out of composition and the
    //     confirming Enter also reaches the web page (e.g. sends the chat
    //     message). This is the invariant behind fix/snippet-enter-leak.
    //
    //     The two checks are independent, and neither implies the other:
    //
    //     - `composed_string()` re-derives the text from session state. It is
    //       not a host surface (Swift's `composedString` reads the coordinator's
    //       own shadow, and the two diverge by design on Escape, where `flush()`
    //       turns a pending `n` into `ん` with no marked text emitted). It
    //       catches a state-side regression such as `SnippetState::display_text`
    //       going empty for a live selection.
    //     - `HostMarked` is the host's side, accumulated over the whole
    //       sequence, so it also covers the writer that emits no marked text at
    //       all: a response whose commit ends the marked session while the
    //       session stays composing. That shape reproduces the
    //       fix/snippet-enter-leak symptom without ever emitting an empty
    //       string, so a per-response check on `resp.marked` cannot see it.
    if session.is_composing() {
        assert!(
            !session.composed_string().is_empty(),
            "Composing session must have non-empty composed_string after {:?}",
            action,
        );
        assert!(
            host.is_composing(),
            "Composing session must leave the host in composition after {:?}",
            action,
        );
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

    // 9. SnippetTrigger always lands in snippet mode. The fixture store is
    //    non-empty and the trigger is dispatched ahead of every state and mode
    //    check in `handle_key` (ABC passthrough included), so nothing here can
    //    legitimately refuse it.
    //
    //    This is also what keeps the snippet coverage honest. Snippet mode is
    //    reachable only if `enter_snippet_mode` finds entries, so a regression
    //    in the store lookup (e.g. `prefix_search("")` no longer returning
    //    every entry, the path a backspace-to-empty-filter repopulates through)
    //    would make every generated sequence skip snippet mode entirely and
    //    leave every proptest green. Asserting the transition turns that silent
    //    loss of coverage into a failure.
    if matches!(action, Action::SnippetTrigger) {
        assert!(
            matches!(session.state, SessionState::Snippet(_)),
            "SnippetTrigger must enter snippet mode, after {:?}",
            action,
        );
    }
}

// ---------------------------------------------------------------------------
// Sequence driver
// ---------------------------------------------------------------------------

/// Run one generated sequence against a fresh session, checking every
/// invariant after each action. Shared by all three proptests so they differ
/// only by the generator and the one setting under test.
///
/// A snippet store is always installed, so snippet mode is reachable whenever
/// the generator can produce `SnippetTrigger`. The store-less variant of the
/// trigger is a single fixed transition, covered by unit test
/// `test_snippet_trigger_without_store_commits_composing`.
fn run_sequence(actions: &[Action], defer_candidates: bool) {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);
    session.set_defer_candidates(defer_candidates);
    session.set_snippet_store(Some(make_snippet_store(SNIPPET_ENTRIES)));
    let mut host = HostMarked::default();
    for action in actions {
        let prev = PrevState::of(&session);
        if let Some(resp) = execute_action(&mut session, action, &*dict) {
            host.apply(&resp);
            assert_invariants(&session, &resp, action, prev, &host);
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

    /// Snippet mode on its own sampling budget (see `arb_action_with_snippets`).
    /// `defer_candidates` is generated rather than fixed so both candidate paths
    /// are crossed with snippet mode without splitting this into two tests.
    #[test]
    fn session_invariants_with_snippets(
        actions in prop::collection::vec(arb_action_with_snippets(), 1..100),
        defer_candidates in any::<bool>(),
    ) {
        run_sequence(&actions, defer_candidates);
    }
}
