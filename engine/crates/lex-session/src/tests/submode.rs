use super::*;
use crate::types::{CandidateAction, Submode};

#[test]
fn test_tab_toggles_submode() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);

    assert_eq!(session.submode(), Submode::Japanese);
    session.handle_key(key::TAB, "", 0);
    assert_eq!(session.submode(), Submode::English);
    session.handle_key(key::TAB, "", 0);
    assert_eq!(session.submode(), Submode::Japanese);
}

#[test]
fn test_english_submode_direct_input() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);

    session.handle_key(key::TAB, "", 0); // switch to English
    let resp = session.handle_key(0, "h", 0);
    assert!(resp.consumed);
    assert!(session.is_composing());
    assert_eq!(session.comp().kana, "h");
    assert!(resp.marked.as_ref().is_some_and(|m| m.dashed));

    let resp = session.handle_key(0, "i", 0);
    assert!(resp.consumed);
    assert_eq!(session.comp().kana, "hi");
}

#[test]
fn test_english_mode_space_literal() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);

    session.handle_key(key::TAB, "", 0); // → English
    type_string(&mut session, "hi");
    session.handle_key(key::SPACE, "", 0);
    assert_eq!(session.comp().kana, "hi ");
}

// --- Boundary space (programmer mode) ---

#[test]
fn test_programmer_mode_boundary_space() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);
    session.set_programmer_mode(true);

    // Type Japanese, toggle to English
    type_string(&mut session, "kyou");
    let best = session.comp().candidates.surfaces[0].clone();
    session.handle_key(key::TAB, "", 0); // → English
                                         // Boundary space should be in display_prefix after crystallization
    assert!(session.comp().prefix.text.ends_with(' '));
    assert!(session.comp().prefix.has_boundary_space);
    // composed_kana should be cleared (crystallized into prefix)
    assert!(session.comp().kana.is_empty());

    // Toggle back without typing → space should be removed
    session.handle_key(key::TAB, "", 0); // → Japanese
    assert!(!session.comp().prefix.text.ends_with(' '));
    assert!(!session.comp().prefix.has_boundary_space);
    // Prefix should still contain the crystallized conversion (without space)
    assert_eq!(session.comp().prefix.text, best);
}

#[test]
fn test_toggle_submode_preserves_conversion() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);

    // Type "kyou" → candidates include "今日" (Viterbi best)
    type_string(&mut session, "kyou");
    assert!(!session.comp().candidates.is_empty());
    let best = session.comp().candidates.surfaces[0].clone();

    // display() should return the Viterbi best
    assert_eq!(session.comp().display(), best);

    // Toggle to English — display must preserve the conversion, not revert to kana
    let resp = session.handle_key(key::TAB, "", 0);
    assert!(resp.consumed);
    assert!(resp.marked.as_ref().is_some_and(|m| m.dashed));
    let marked = resp.marked.unwrap().text;
    assert_eq!(
        marked, best,
        "toggle should preserve conversion, not revert to kana"
    );
    // Candidates are cleared after crystallization
    assert!(matches!(resp.candidates, CandidateAction::Hide));
    // Conversion should be crystallized into display_prefix
    assert_eq!(session.comp().prefix.text, best);
    assert!(session.comp().kana.is_empty());
}

// --- Mixed mode (Japanese + English) ---

#[test]
fn test_mixed_mode_commit() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);

    // Type "kyou" → "今日", then Tab to English, type "test", then Enter
    type_string(&mut session, "kyou");
    let best = session.comp().candidates.surfaces[0].clone();
    session.handle_key(key::TAB, "", 0); // → English
    type_string(&mut session, "test");

    // Marked text should show "今日test"
    let display = session.comp().display();
    assert_eq!(display, format!("{}test", best));

    // Commit should produce "今日test"
    let resp = session.handle_key(key::ENTER, "", 0);
    assert_eq!(resp.commit.as_deref(), Some(&format!("{}test", best)[..]));
    assert!(!session.is_composing());
}

#[test]
fn test_mixed_mode_display() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);

    // Type Japanese → English → Japanese
    type_string(&mut session, "kyou");
    let best = session.comp().candidates.surfaces[0].clone();
    session.handle_key(key::TAB, "", 0); // → English
    type_string(&mut session, "hello");
    session.handle_key(key::TAB, "", 0); // → Japanese
    type_string(&mut session, "kyou");

    // Display should be "<best>hello<new_best>"
    let display = session.comp().display();
    assert!(
        display.starts_with(&best),
        "display should start with first conversion: got {}",
        display,
    );
    assert!(
        display.contains("hello"),
        "display should contain English segment: got {}",
        display,
    );
}

#[test]
fn test_mixed_mode_backspace_into_prefix() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);

    // Type Japanese, toggle to English
    type_string(&mut session, "kyou");
    let best = session.comp().candidates.surfaces[0].clone();
    session.handle_key(key::TAB, "", 0); // → English
    type_string(&mut session, "ab");

    // Backspace twice to empty English segment
    session.handle_key(key::BACKSPACE, "", 0);
    session.handle_key(key::BACKSPACE, "", 0);
    assert!(session.comp().kana.is_empty());
    // display_prefix still has the frozen conversion
    assert_eq!(session.comp().prefix.text, best);

    // One more backspace deletes from prefix
    session.handle_key(key::BACKSPACE, "", 0);
    assert!(session.comp().prefix.text.len() < best.len());
}
