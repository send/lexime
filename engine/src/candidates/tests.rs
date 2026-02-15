use std::collections::HashSet;

use crate::dict::{DictEntry, TrieDictionary};
use crate::user_history::UserHistory;

use super::{generate_candidates, generate_prediction_candidates, punctuation_alternatives};

fn make_dict() -> TrieDictionary {
    let entries = vec![
        (
            "きょう".to_string(),
            vec![
                DictEntry {
                    surface: "今日".to_string(),
                    cost: 3000,
                    left_id: 0,
                    right_id: 0,
                },
                DictEntry {
                    surface: "京".to_string(),
                    cost: 5000,
                    left_id: 0,
                    right_id: 0,
                },
            ],
        ),
        (
            "は".to_string(),
            vec![DictEntry {
                surface: "は".to_string(),
                cost: 2000,
                left_id: 0,
                right_id: 0,
            }],
        ),
        (
            "。".to_string(),
            vec![DictEntry {
                surface: "。".to_string(),
                cost: 1000,
                left_id: 0,
                right_id: 0,
            }],
        ),
    ];
    TrieDictionary::from_entries(entries)
}

#[test]
fn test_punctuation_candidates() {
    let dict = make_dict();
    let resp = generate_candidates(&dict, None, None, "。", 9);
    assert!(resp.surfaces.contains(&"。".to_string()));
    assert!(resp.surfaces.contains(&"．".to_string()));
    assert!(resp.surfaces.contains(&".".to_string()));
    assert!(resp.paths.is_empty());
}

#[test]
fn test_normal_candidates() {
    let dict = make_dict();
    let resp = generate_candidates(&dict, None, None, "きょう", 9);
    // Viterbi #1 should be first (conversion result, not kana)
    // Kana should still be present
    assert!(resp.surfaces.contains(&"きょう".to_string()));
    assert!(resp.surfaces.contains(&"今日".to_string()));
    assert!(resp.surfaces.contains(&"京".to_string()));
    // N-best paths should be non-empty
    assert!(!resp.paths.is_empty());
}

#[test]
fn test_empty_reading() {
    let dict = make_dict();
    let resp = generate_candidates(&dict, None, None, "", 9);
    assert!(resp.surfaces.is_empty());
    assert!(resp.paths.is_empty());
}

#[test]
fn test_no_duplicates() {
    let dict = make_dict();
    let resp = generate_candidates(&dict, None, None, "きょう", 20);
    let unique: HashSet<&String> = resp.surfaces.iter().collect();
    assert_eq!(
        unique.len(),
        resp.surfaces.len(),
        "candidates should be deduplicated"
    );
}

#[test]
fn test_punctuation_mode_detected() {
    assert!(punctuation_alternatives("。").is_some());
    assert!(punctuation_alternatives("、").is_some());
    assert!(punctuation_alternatives("？").is_some());
    assert!(punctuation_alternatives("きょう").is_none());
}

#[test]
fn test_kana_promoted_by_history() {
    let dict = make_dict();
    let mut h = UserHistory::new();
    // Record hiragana selection: user chose "きょう" (kana) for reading "きょう"
    h.record(&[("きょう".into(), "きょう".into())]);

    let resp = generate_candidates(&dict, None, Some(&h), "きょう", 9);
    // Kana "きょう" should appear at position 1 (after Viterbi #1, before other N-best)
    assert_eq!(
        resp.surfaces[0], "きょう",
        "kana should be promoted to position 0 (inline preview)"
    );
}

#[test]
fn test_prediction_bigram_chaining() {
    let dict = make_dict();
    let mut h = UserHistory::new();
    // Record a sentence: 今日は → bigrams: 今日→は
    h.record(&[("きょう".into(), "今日".into()), ("は".into(), "は".into())]);

    let resp = generate_prediction_candidates(&dict, None, Some(&h), "きょう", 20);
    // Should contain chained phrase "今日は"
    assert!(
        resp.surfaces.contains(&"今日は".to_string()),
        "should contain chained phrase '今日は', got: {:?}",
        resp.surfaces,
    );
    // Chained phrase should appear before unchained base candidates
    let chained_pos = resp.surfaces.iter().position(|s| s == "今日は").unwrap();
    let base_pos = resp.surfaces.iter().position(|s| s == "今日");
    if let Some(bp) = base_pos {
        assert!(
            chained_pos < bp,
            "chained phrase should appear before base candidate"
        );
    }
}

#[test]
fn test_prediction_no_chaining_without_history() {
    let dict = make_dict();
    let resp = generate_prediction_candidates(&dict, None, None, "きょう", 20);
    // Without history, should behave like standard candidates
    assert!(resp.surfaces.contains(&"今日".to_string()));
    assert!(resp.surfaces.contains(&"きょう".to_string()));
}

#[test]
fn test_prediction_multi_word_chain() {
    let entries = vec![
        (
            "きょう".to_string(),
            vec![DictEntry {
                surface: "今日".to_string(),
                cost: 3000,
                left_id: 0,
                right_id: 0,
            }],
        ),
        (
            "は".to_string(),
            vec![DictEntry {
                surface: "は".to_string(),
                cost: 2000,
                left_id: 0,
                right_id: 0,
            }],
        ),
        (
            "いい".to_string(),
            vec![DictEntry {
                surface: "良い".to_string(),
                cost: 3500,
                left_id: 0,
                right_id: 0,
            }],
        ),
        (
            "てんき".to_string(),
            vec![DictEntry {
                surface: "天気".to_string(),
                cost: 4000,
                left_id: 0,
                right_id: 0,
            }],
        ),
    ];
    let dict = TrieDictionary::from_entries(entries);
    let mut h = UserHistory::new();
    // Record a full sentence: 今日は良い天気
    h.record(&[
        ("きょう".into(), "今日".into()),
        ("は".into(), "は".into()),
        ("いい".into(), "良い".into()),
        ("てんき".into(), "天気".into()),
    ]);

    let resp = generate_prediction_candidates(&dict, None, Some(&h), "きょう", 20);
    // Should contain the full chained phrase
    assert!(
        resp.surfaces.contains(&"今日は良い天気".to_string()),
        "should contain multi-word chain '今日は良い天気', got: {:?}",
        resp.surfaces,
    );
}

#[test]
fn test_prediction_cycle_produces_no_garbage() {
    let dict = make_dict();
    let mut h = UserHistory::new();
    // Create a cycle: は→は (self-loop)
    h.record(&[("は".into(), "は".into()), ("は".into(), "は".into())]);

    let resp = generate_prediction_candidates(&dict, None, Some(&h), "は", 20);
    // No candidate should contain repeated "ははは..." garbage
    for surface in &resp.surfaces {
        assert!(
            !surface.contains("はは"),
            "should not contain repeated garbage: {}",
            surface,
        );
    }
}

#[test]
fn test_kana_not_promoted_without_history() {
    let dict = make_dict();
    let resp = generate_candidates(&dict, None, None, "きょう", 9);
    // Without history, kana should NOT be at position 1
    // (it should be after all N-best paths)
    if resp.surfaces.len() >= 2 {
        // Position 0 is Viterbi #1 (likely kanji), kana comes after N-best
        let kana_pos = resp.surfaces.iter().position(|s| s == "きょう").unwrap();
        assert!(
            kana_pos > 0,
            "kana should not be at position 0 without history"
        );
    }
}
