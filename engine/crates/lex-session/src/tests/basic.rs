use std::sync::{Arc, RwLock};

use super::*;
use crate::types::{
    cyclic_index, is_romaji_input, CandidateAction, LearningRecord, FLAG_HAS_MODIFIER, FLAG_SHIFT,
};
use lex_core::user_history::UserHistory;

// --- Basic romaji input ---

#[test]
fn test_romaji_input_ka() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);

    let resp = session.handle_key(0, "k", 0);
    assert!(resp.consumed);
    assert!(session.is_composing());

    let resp = session.handle_key(0, "a", 0);
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

    let resp = session.handle_key(key::BACKSPACE, "", 0);
    assert!(resp.consumed);
    assert!(!session.is_composing()); // back to idle (composition dropped)
}

#[test]
fn test_backspace_removes_kana() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);

    type_string(&mut session, "ka"); // composedKana = "か"
    assert_eq!(session.comp().kana, "か");

    let resp = session.handle_key(key::BACKSPACE, "", 0);
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

    session.handle_key(key::BACKSPACE, "", 0);
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

    let resp = session.handle_key(key::ESCAPE, "", 0);
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

    let resp = session.handle_key(key::ENTER, "", 0);
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
    let resp = session.handle_key(key::SPACE, "", 0);
    assert!(resp.consumed);
    assert_eq!(session.comp().candidates.selected, 1);
    assert!(matches!(resp.candidates, CandidateAction::Show { .. }));

    // Second space goes to index 2
    let resp = session.handle_key(key::SPACE, "", 0);
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

    session.handle_key(key::DOWN, "", 0);
    assert_eq!(session.comp().candidates.selected, 1);

    session.handle_key(key::UP, "", 0);
    assert_eq!(session.comp().candidates.selected, 0);

    // Up from 0 wraps to last
    session.handle_key(key::UP, "", 0);
    assert_eq!(session.comp().candidates.selected, count - 1);
}

// --- Modifier pass-through ---

#[test]
fn test_modifier_passthrough_idle() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);

    let resp = session.handle_key(0, "c", FLAG_HAS_MODIFIER);
    assert!(!resp.consumed);
}

#[test]
fn test_modifier_passthrough_composing() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);

    type_string(&mut session, "kyou");
    assert!(session.is_composing());

    let resp = session.handle_key(0, "c", FLAG_HAS_MODIFIER);
    assert!(!resp.consumed);
    assert!(resp.commit.is_some()); // commits before passing through
    assert!(!session.is_composing());
}

// --- Eisu key ---

#[test]
fn test_eisu_switches_to_abc() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);

    let resp = session.handle_key(key::EISU, "", 0);
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

    let resp = session.handle_key(key::EISU, "", 0);
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

    let resp = session.handle_key(key::KANA, "", 0);
    assert!(resp.consumed);
}

// --- Keymap remap (replaces programmer_mode ¥ tests) ---

#[test]
fn test_keymap_yen_idle() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);

    // keyCode 93 (¥) is remapped to \ by default settings
    let resp = session.handle_key(93, "¥", 0);
    assert!(resp.consumed);
    assert_eq!(resp.commit.as_deref(), Some("\\"));
}

#[test]
fn test_keymap_yen_shifted() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);

    // keyCode 93 + shift → |
    let resp = session.handle_key(93, "¥", FLAG_SHIFT);
    assert!(resp.consumed);
    assert_eq!(resp.commit.as_deref(), Some("|"));
}

#[test]
fn test_keymap_yen_composing() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);

    type_string(&mut session, "kyou");
    let resp = session.handle_key(93, "¥", 0);
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
    let resp = session.handle_key(10, "§", 0);
    assert!(resp.consumed);
    assert!(session.is_composing());
    assert!(session.comp().kana.contains('」'));

    // Commit to reset state
    session.handle_key(key::ENTER, "", 0);

    // shifted → } (not in trie, so direct commit)
    let resp = session.handle_key(10, "§", FLAG_SHIFT);
    assert!(resp.consumed);
    assert_eq!(resp.commit.as_deref(), Some("}"));
}

// --- Tab behavior ---

#[test]
fn test_tab_idle_passthrough() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);

    // Tab in idle is not consumed (passthrough)
    let resp = session.handle_key(key::TAB, "", 0);
    assert!(!resp.consumed);
}

#[test]
fn test_tab_composing_commits() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);

    type_string(&mut session, "kyou");
    assert!(session.is_composing());

    let resp = session.handle_key(key::TAB, "", 0);
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
    let resp = session.handle_key(0, ".", 0);
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
    session.handle_key(key::ENTER, "", 0);

    let records = session.take_history_records();
    assert!(!records.is_empty());
}

#[test]
fn test_history_recorded_on_escape() {
    let dict = make_test_dict();
    let history = UserHistory::new();
    let mut session = InputSession::new(dict.clone(), None, Some(Arc::new(RwLock::new(history))));

    type_string(&mut session, "kyou");
    session.handle_key(key::ESCAPE, "", 0);

    let records = session.take_history_records();
    assert!(!records.is_empty());
    // Should record kana → kana
    match &records[0] {
        LearningRecord::Committed {
            reading, surface, ..
        } => {
            assert_eq!(reading, "きょう");
            assert_eq!(surface, "きょう");
        }
    }
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
    session.handle_key(0, "1", 0); // unrecognized
    assert!(session.comp().kana.ends_with('1'));
}

// --- Shift+letter (uppercase passthrough) ---

#[test]
fn test_uppercase_idle() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);

    // Shift+A in idle: starts composing with "A" (not romaji-converted)
    let resp = session.handle_key(0, "A", FLAG_SHIFT);
    assert!(resp.consumed);
    assert!(session.is_composing());
    assert_eq!(session.comp().kana, "A");
}

#[test]
fn test_uppercase_composing() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);

    type_string(&mut session, "ka"); // "か"
    let resp = session.handle_key(0, "B", FLAG_SHIFT);
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

    let resp = session.handle_key(0, "A", FLAG_SHIFT);
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
