use std::sync::{Arc, RwLock};

use super::*;
use crate::types::{cyclic_index, is_romaji_input, CandidateAction, KeyEvent, LearningRecord};
use lex_core::user_history::UserHistory;

// --- Basic romaji input ---

#[test]
fn test_romaji_input_ka() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);

    let resp = session.handle_key(KeyEvent::text("k"));
    assert!(resp.consumed);
    assert!(session.is_composing());

    let resp = session.handle_key(KeyEvent::text("a"));
    assert!(resp.consumed);
    // After "ka" → "か", marked text should be set
    assert!(resp.marked.is_some());
}

#[test]
fn test_romaji_kyou() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);

    type_string(&mut session, "kyou");
    assert!(session.is_composing());
    assert_eq!(session.comp().kana, "きょう");
    assert!(session.comp().pending.is_empty());
}

#[test]
fn test_romaji_sokuon() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);

    type_string(&mut session, "kka");
    assert_eq!(session.comp().kana, "っか");
}

// --- Backspace ---

#[test]
fn test_backspace_removes_pending() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);

    type_string(&mut session, "k"); // pending_romaji = "k"
    assert_eq!(session.comp().pending, "k");

    let resp = session.handle_key(KeyEvent::Backspace);
    assert!(resp.consumed);
    assert!(!session.is_composing()); // back to idle (composition dropped)
}

#[test]
fn test_backspace_removes_kana() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);

    type_string(&mut session, "ka"); // composedKana = "か"
    assert_eq!(session.comp().kana, "か");

    let resp = session.handle_key(KeyEvent::Backspace);
    assert!(resp.consumed);
    assert!(!session.is_composing()); // back to idle (composition dropped)
}

#[test]
fn test_backspace_partial() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);

    type_string(&mut session, "kak"); // "か" + pending "k"
    assert_eq!(session.comp().kana, "か");
    assert_eq!(session.comp().pending, "k");

    session.handle_key(KeyEvent::Backspace);
    assert_eq!(session.comp().kana, "か");
    assert!(session.comp().pending.is_empty());
    assert!(session.is_composing());
}

// --- Escape ---

#[test]
fn test_escape_flushes() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);

    type_string(&mut session, "kyoun"); // "きょう" + pending "n"

    let resp = session.handle_key(KeyEvent::Escape);
    assert!(resp.consumed);
    assert!(matches!(resp.candidates, CandidateAction::Hide));
    // After escape, kana is flushed (n → ん)
    assert_eq!(session.comp().kana, "きょうん");
    assert!(session.comp().pending.is_empty());
}

// --- Enter (commit) ---

#[test]
fn test_enter_commits_selected() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);

    type_string(&mut session, "kyou");
    assert!(!session.comp().candidates.is_empty());

    let resp = session.handle_key(KeyEvent::Enter);
    assert!(resp.consumed);
    assert!(resp.commit.is_some());
    assert!(matches!(resp.candidates, CandidateAction::Hide));
    assert!(!session.is_composing());
}

// --- Space (candidate cycling) ---

#[test]
fn test_space_cycles_candidates() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);

    type_string(&mut session, "kyou");
    let initial_count = session.comp().candidates.surfaces.len();
    assert!(initial_count > 1);
    assert_eq!(session.comp().candidates.selected, 0);

    // First space jumps to index 1
    let resp = session.handle_key(KeyEvent::Space);
    assert!(resp.consumed);
    assert_eq!(session.comp().candidates.selected, 1);
    assert!(matches!(resp.candidates, CandidateAction::Show { .. }));

    // Second space goes to index 2
    let resp = session.handle_key(KeyEvent::Space);
    assert!(resp.consumed);
    assert_eq!(session.comp().candidates.selected, 2);
}

// --- Arrow keys ---

#[test]
fn test_arrow_keys_cycle() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);

    type_string(&mut session, "kyou");
    let count = session.comp().candidates.surfaces.len();
    assert!(count > 1);

    session.handle_key(KeyEvent::ArrowDown);
    assert_eq!(session.comp().candidates.selected, 1);

    session.handle_key(KeyEvent::ArrowUp);
    assert_eq!(session.comp().candidates.selected, 0);

    // Up from 0 wraps to last
    session.handle_key(KeyEvent::ArrowUp);
    assert_eq!(session.comp().candidates.selected, count - 1);
}

// --- Modifier pass-through ---

#[test]
fn test_modifier_passthrough_idle() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);

    let resp = session.handle_key(KeyEvent::ModifiedKey);
    assert!(!resp.consumed);
}

#[test]
fn test_modifier_passthrough_composing() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);

    type_string(&mut session, "kyou");
    assert!(session.is_composing());

    let resp = session.handle_key(KeyEvent::ModifiedKey);
    assert!(!resp.consumed);
    assert!(resp.commit.is_some()); // commits before passing through
    assert!(!session.is_composing());
}

// --- Eisu key ---

#[test]
fn test_eisu_switches_to_abc() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);

    let resp = session.handle_key(KeyEvent::SwitchToDirectInput);
    assert!(resp.consumed);
    assert!(!resp.side_effects.switch_to_abc);
    assert!(session.is_abc_passthrough());
}

#[test]
fn test_eisu_commits_and_switches() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);

    type_string(&mut session, "kyou");
    assert!(session.is_composing());

    let resp = session.handle_key(KeyEvent::SwitchToDirectInput);
    assert!(resp.consumed);
    assert!(!resp.side_effects.switch_to_abc);
    assert!(resp.commit.is_some());
    assert!(!session.is_composing());
    assert!(session.is_abc_passthrough());
}

// --- Kana key ---

#[test]
fn test_kana_consumed() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);

    let resp = session.handle_key(KeyEvent::SwitchToJapanese);
    assert!(resp.consumed);
}

// --- ABC passthrough: Space commits as " " ---

#[test]
fn test_abc_passthrough_space() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);

    session.handle_key(KeyEvent::SwitchToDirectInput);
    assert!(session.is_abc_passthrough());

    let resp = session.handle_key(KeyEvent::Space);
    assert!(resp.consumed);
    assert_eq!(resp.commit.as_deref(), Some(" "));
}

// --- Keymap remap (replaces programmer_mode ¥ tests) ---

#[test]
fn test_keymap_yen_idle() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);

    // keyCode 93 (¥) is remapped to \ by default settings — caller now sends Remapped
    let resp = session.handle_key(KeyEvent::remapped("\\"));
    assert!(resp.consumed);
    assert_eq!(resp.commit.as_deref(), Some("\\"));
}

#[test]
fn test_keymap_empty_remap_is_not_consumed() {
    // `keymap_get` reports an empty mapping as unmapped, but `Remapped` crosses
    // the FFI and any frontend can build one. Empty text has nothing to insert, so it must
    // not become `commit: Some("")` — that inserts nothing yet ends the host's
    // marked-text session, the shape that leaks the next key (#293). Checked in
    // all three states the remap path branches on.
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);

    let resp = session.handle_key(KeyEvent::remapped(""));
    assert!(!resp.consumed);
    assert!(resp.commit.is_none());

    session.set_abc_passthrough(true);
    let resp = session.handle_key(KeyEvent::remapped(""));
    assert!(!resp.consumed);
    assert!(resp.commit.is_none());
    session.set_abc_passthrough(false);

    type_string(&mut session, "ka");
    assert!(session.is_composing());
    let resp = session.handle_key(KeyEvent::remapped(""));
    assert!(!resp.consumed);
    assert!(resp.commit.is_none());
    assert!(session.is_composing(), "composition must be left alone");
}

#[test]
fn test_keymap_yen_shifted() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);

    // keyCode 93 + shift → |
    let resp = session.handle_key(KeyEvent::remapped_shift("|"));
    assert!(resp.consumed);
    assert_eq!(resp.commit.as_deref(), Some("|"));
}

#[test]
fn test_keymap_yen_composing() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);

    type_string(&mut session, "kyou");
    let resp = session.handle_key(KeyEvent::remapped("\\"));
    assert!(resp.consumed);
    // In composing, remapped text is fed as input (not commit-and-insert)
    assert!(resp.commit.is_none());
    assert!(session.is_composing());
    // The backslash should be added to the composition
    assert!(session.comp().kana.contains('\\'));
}

#[test]
fn test_keymap_jis_bracket() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);

    // keyCode 10 is remapped to ] by default settings.
    // ] is in the romaji trie (] → 」), so it enters composing via trie match.
    let resp = session.handle_key(KeyEvent::remapped("]"));
    assert!(resp.consumed);
    assert!(session.is_composing());
    assert!(session.comp().kana.contains('」'));

    // Commit to reset state
    session.handle_key(KeyEvent::Enter);

    // shifted → } (not in trie, so direct commit)
    let resp = session.handle_key(KeyEvent::remapped_shift("}"));
    assert!(resp.consumed);
    assert_eq!(resp.commit.as_deref(), Some("}"));
}

// --- Tab behavior ---

#[test]
fn test_tab_idle_passthrough() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);

    // Tab in idle is not consumed (passthrough)
    let resp = session.handle_key(KeyEvent::Tab);
    assert!(!resp.consumed);
}

#[test]
fn test_tab_composing_commits() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);

    type_string(&mut session, "kyou");
    assert!(session.is_composing());

    let resp = session.handle_key(KeyEvent::Tab);
    assert!(resp.consumed);
    assert!(resp.commit.is_some());
    assert!(!session.is_composing());
}

// --- Punctuation auto-commit ---

#[test]
fn test_punctuation_auto_commit() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);

    type_string(&mut session, "kyou");
    assert!(session.is_composing());

    // Type "." which is a romaji trie match for "。"
    let resp = session.handle_key(KeyEvent::text("."));
    assert!(resp.consumed);
    // Should commit current state + append punctuation
    let text = resp.commit.unwrap();
    assert!(
        text.ends_with('。'),
        "commit should end with 。, got: {}",
        text
    );
}

// --- Commit (composedString for IMKit) ---

#[test]
fn test_commit_method() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);

    type_string(&mut session, "kyou");
    assert!(session.is_composing());

    let resp = session.commit();
    assert!(resp.commit.is_some());
    assert!(!session.is_composing());
}

// --- composed_string ---

#[test]
fn test_composed_string_idle() {
    let dict = make_test_dict();
    let session = InputSession::new(dict.clone(), None, None);
    assert_eq!(session.composed_string(), "");
}

#[test]
fn test_composed_string_composing() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);

    type_string(&mut session, "kyou");
    // composed_string should return the current display (best candidate)
    let cs = session.composed_string();
    assert!(!cs.is_empty());
}

// --- History recording ---

#[test]
fn test_history_recorded_on_commit() {
    let dict = make_test_dict();
    let history = UserHistory::new();
    let mut session = InputSession::new(dict.clone(), None, Some(Arc::new(RwLock::new(history))));

    type_string(&mut session, "kyou");
    session.handle_key(KeyEvent::Enter);

    let records = session.take_history_records();
    assert!(!records.is_empty());
}

#[test]
fn test_history_not_recorded_on_escape() {
    let dict = make_test_dict();
    let history = UserHistory::new();
    let mut session = InputSession::new(dict.clone(), None, Some(Arc::new(RwLock::new(history))));

    type_string(&mut session, "kyou");
    session.handle_key(KeyEvent::Escape);

    // Escape cancels conversion/candidate selection — unconfirmed input should not be learned
    let records = session.take_history_records();
    assert!(records.is_empty());
}

#[test]
fn test_history_record_includes_rank_and_top1() {
    let dict = make_test_dict();
    let history = UserHistory::new();
    let mut session = InputSession::new(dict.clone(), None, Some(Arc::new(RwLock::new(history))));

    // Accepting top-1: rank 0, no top1, not auto
    type_string(&mut session, "kyou");
    session.handle_key(KeyEvent::Enter);
    let records = session.take_history_records();
    let LearningRecord::Committed {
        rank, top1, auto, ..
    } = &records[0]
    else {
        panic!("expected Committed record");
    };
    assert_eq!(*rank, 0);
    assert!(top1.is_none());
    assert!(!auto);

    // Manual selection: rank reflects the picked index, top1 is recorded
    type_string(&mut session, "kyou");
    let expected_top1 = session.comp().candidates.surfaces[0].clone();
    session.handle_key(KeyEvent::Space); // move selection to index 1
    let expected_surface = session.comp().candidates.surfaces[1].clone();
    session.handle_key(KeyEvent::Enter);
    let records = session.take_history_records();
    let LearningRecord::Committed {
        rank,
        top1,
        surface,
        auto,
        ..
    } = &records[0]
    else {
        panic!("expected Committed record");
    };
    assert_eq!(*rank, 1);
    assert_eq!(top1.as_deref(), Some(expected_top1.as_str()));
    assert_eq!(surface, &expected_surface);
    assert!(!auto);
}

// --- Cyclic index ---

#[test]
fn test_cyclic_index() {
    assert_eq!(cyclic_index(0, 1, 3), 1);
    assert_eq!(cyclic_index(2, 1, 3), 0); // wrap
    assert_eq!(cyclic_index(0, -1, 3), 2); // wrap backwards
    assert_eq!(cyclic_index(0, 0, 0), 0); // empty
}

// --- is_romaji_input ---

#[test]
fn test_is_romaji_input() {
    assert!(is_romaji_input("a"));
    assert!(is_romaji_input("Z"));
    assert!(is_romaji_input("-"));
    assert!(!is_romaji_input("1"));
    assert!(!is_romaji_input("。"));
    assert!(!is_romaji_input(""));
}

// --- Non-romaji char in composing ---

#[test]
fn test_unrecognized_char_added_to_kana() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);

    type_string(&mut session, "ka"); // "か"
    session.handle_key(KeyEvent::text("1")); // unrecognized
    assert!(session.comp().kana.ends_with('1'));
}

// --- Shift+letter (uppercase passthrough) ---

#[test]
fn test_uppercase_idle() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);

    // Shift+A in idle: starts composing with "A" (not romaji-converted)
    let resp = session.handle_key(KeyEvent::text_shift("A"));
    assert!(resp.consumed);
    assert!(session.is_composing());
    assert_eq!(session.comp().kana, "A");
}

#[test]
fn test_uppercase_composing() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);

    type_string(&mut session, "ka"); // "か"
    let resp = session.handle_key(KeyEvent::text_shift("B"));
    assert!(resp.consumed);
    assert!(session.is_composing());
    assert_eq!(session.comp().kana, "かB");
}

#[test]
fn test_uppercase_with_pending() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);

    type_string(&mut session, "kan"); // "か" + pending "n"
    assert_eq!(session.comp().pending, "n");

    let resp = session.handle_key(KeyEvent::text_shift("A"));
    assert!(resp.consumed);
    // Pending "n" should be flushed to "ん", then "A" added
    assert_eq!(session.comp().kana, "かんA");
    assert!(session.comp().pending.is_empty());
}

// --- z-sequence ---

#[test]
fn test_z_sequence() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);

    // "z" is a prefix in the romaji trie, "zh" → "←"
    type_string(&mut session, "zh");
    assert_eq!(session.comp().kana, "←");
}

// --- ForwardDelete ---

#[test]
fn test_forward_delete_removes_candidate_and_records_deletion() {
    let dict = make_test_dict();
    let history = UserHistory::new();
    let mut session = InputSession::new(dict.clone(), None, Some(Arc::new(RwLock::new(history))));

    type_string(&mut session, "kyou");
    let initial_count = session.comp().candidates.surfaces.len();
    assert!(initial_count >= 2, "need at least 2 candidates");

    let first_surface = session.comp().candidates.surfaces[0].clone();

    let resp = session.handle_key(KeyEvent::ForwardDelete);
    assert!(resp.consumed);
    // Candidate should be removed
    assert_eq!(session.comp().candidates.surfaces.len(), initial_count - 1);
    assert!(!session.comp().candidates.surfaces.contains(&first_surface));
    // Should show updated candidate list
    assert!(matches!(resp.candidates, CandidateAction::Show { .. }));

    // Deletion record should be generated
    let records = session.take_history_records();
    assert_eq!(records.len(), 1);
    assert!(matches!(&records[0], LearningRecord::Deletion { .. }));
}

#[test]
fn test_forward_delete_no_candidates() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);

    // Type something that has no dictionary match
    type_string(&mut session, "zzz");
    assert!(session.is_composing());

    let resp = session.handle_key(KeyEvent::ForwardDelete);
    assert!(resp.consumed);
}

#[test]
fn test_forward_delete_last_candidate_hides_panel() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);

    type_string(&mut session, "ha");
    // Remove all candidates one by one
    let count = session.comp().candidates.surfaces.len();
    assert!(count >= 1);

    for _ in 0..count - 1 {
        let resp = session.handle_key(KeyEvent::ForwardDelete);
        assert!(resp.consumed);
        assert!(matches!(resp.candidates, CandidateAction::Show { .. }));
    }

    // Delete the last candidate → should hide panel
    let resp = session.handle_key(KeyEvent::ForwardDelete);
    assert!(resp.consumed);
    assert!(matches!(resp.candidates, CandidateAction::Hide));
    assert!(session.comp().candidates.surfaces.is_empty());
}

#[test]
fn test_forward_delete_idle_not_consumed() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);

    let resp = session.handle_key(KeyEvent::ForwardDelete);
    assert!(!resp.consumed);
}

#[test]
fn test_forward_delete_no_history_no_record() {
    let dict = make_test_dict();
    // No history passed — deletion record should not be generated
    let mut session = InputSession::new(dict.clone(), None, None);

    type_string(&mut session, "kyou");
    session.handle_key(KeyEvent::ForwardDelete);

    let records = session.take_history_records();
    assert!(records.is_empty());
}

/// Type `input` and return the marked text the final response emitted — i.e.
/// exactly what the host is showing afterwards.
fn type_string_returning_marked(session: &mut InputSession, input: &str) -> String {
    let mut shown = String::new();
    for ch in input.chars() {
        if let Some(m) = session
            .handle_key(KeyEvent::Text {
                text: ch.to_string(),
                shift: false,
            })
            .marked
        {
            shown = m.text;
        }
    }
    shown
}

// --- settle_unconfirmed (#298 / #309 / #310) ---
//
// The involuntary counterpart to `commit`. IMKit delivers `deactivateServer`
// mid-composition without reliably sending `commitComposition` first, so the
// session has to settle — but an app switch is not acceptance, and settling
// through `commit()` would both insert a conversion the user never saw and
// train top-1 on it.

#[test]
fn settle_unconfirmed_commits_the_reading_when_the_user_never_navigated() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);

    // The composing response emitted `display_kana()`, so that is what the
    // host is showing even though candidates exist.
    let shown = type_string_returning_marked(&mut session, "kyou");
    assert!(!session.comp().candidates.is_empty());
    // The test rests on the reading and top-1 differing. If a fixture change
    // ever made them equal, the assertion below would decay into `f(x) == x`
    // while keeping its name.
    assert_ne!(
        shown,
        session.comp().candidates.surfaces[0],
        "fixture must keep the reading distinct from top-1, or this test proves nothing",
    );

    let resp = session.settle_unconfirmed(&shown);

    assert_eq!(
        resp.commit.as_deref(),
        Some(shown.as_str()),
        "settle must commit what the host was showing, not a candidate surface",
    );
    assert!(!session.is_composing(), "settle reaches Idle");
}

#[test]
fn settle_unconfirmed_commits_the_surface_once_the_user_has_navigated() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);

    type_string(&mut session, "kyou");
    let shown = session
        .handle_key(KeyEvent::Space)
        .marked
        .expect("navigation re-renders the marked text")
        .text;

    let resp = session.settle_unconfirmed(&shown);

    assert_eq!(
        resp.commit.as_deref(),
        Some(shown.as_str()),
        "after navigating, the surface is what the host shows",
    );
}

#[test]
fn settle_unconfirmed_follows_the_display_back_to_the_reading() {
    // PR315 Codex R2: navigating and then editing re-renders the reading. A
    // rule that inferred "surface" from a flag set at navigation went stale
    // here and committed a candidate the host was no longer showing.
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);

    type_string(&mut session, "kyou");
    session.handle_key(KeyEvent::Space);
    let shown = session
        .handle_key(KeyEvent::Backspace)
        .marked
        .expect("backspace re-renders the marked text")
        .text;

    let resp = session.settle_unconfirmed(&shown);

    assert_eq!(
        resp.commit.as_deref(),
        Some(shown.as_str()),
        "editing after navigating puts the reading back on screen",
    );
}

#[test]
fn settle_unconfirmed_preserves_pending_romaji_exactly() {
    // PR315 Codex R2: a trailing `n` is displayed as `n`. Flushing before
    // settling converted it to `ん` and committed text never shown.
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);

    let shown = type_string_returning_marked(&mut session, "kyoun");
    assert!(
        shown.ends_with('n'),
        "precondition: pending romaji is on screen, got {shown:?}",
    );

    let resp = session.settle_unconfirmed(&shown);

    assert_eq!(
        resp.commit.as_deref(),
        Some(shown.as_str()),
        "settle must not force pending romaji the host never rendered",
    );
}

#[test]
fn settle_unconfirmed_records_no_history() {
    let dict = make_test_dict();
    let history = UserHistory::new();
    let mut session = InputSession::new(dict.clone(), None, Some(Arc::new(RwLock::new(history))));

    let shown = type_string_returning_marked(&mut session, "kyou");
    session.settle_unconfirmed(&shown);

    assert!(
        session.take_history_records().is_empty(),
        "an app switch is not acceptance — it must not feed top-1",
    );
}

#[test]
fn settle_unconfirmed_records_no_history_even_after_navigating() {
    let dict = make_test_dict();
    let history = UserHistory::new();
    let mut session = InputSession::new(dict.clone(), None, Some(Arc::new(RwLock::new(history))));

    type_string(&mut session, "kyou");
    let shown = session
        .handle_key(KeyEvent::Space)
        .marked
        .map(|m| m.text)
        .unwrap_or_default();
    session.settle_unconfirmed(&shown);

    assert!(
        session.take_history_records().is_empty(),
        "navigating is not confirming either — only an explicit commit learns",
    );
}

#[test]
fn commit_still_learns_and_resolves_the_surface() {
    // The voluntary path is deliberately unchanged: Enter converts and learns.
    let dict = make_test_dict();
    let history = UserHistory::new();
    let mut session = InputSession::new(dict.clone(), None, Some(Arc::new(RwLock::new(history))));

    type_string(&mut session, "kyou");
    let surface = session.comp().candidates.surfaces[0].clone();
    let resp = session.commit();

    assert_eq!(resp.commit.as_deref(), Some(surface.as_str()));
    assert!(!session.take_history_records().is_empty());
}

#[test]
fn settle_unconfirmed_on_idle_is_a_no_op() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);

    let resp = session.settle_unconfirmed("");

    assert!(
        resp.commit.is_none(),
        "nothing composing → nothing to commit"
    );
    assert!(!session.is_composing());
}
