use super::*;
use crate::converter::testutil::test_dict;
use crate::user_history::UserHistory;

#[test]
fn test_convert_with_history_promotes_learned() {
    let dict = test_dict();
    // "きょう" has 今日(3000) and 京(5000). Without history, 今日 wins.
    let baseline = convert(&dict, None, "きょう");
    assert_eq!(baseline[0].surface, "今日");

    // After learning "京", it should be promoted
    let mut h = UserHistory::new();
    h.record(&[("きょう".into(), "京".into())]);
    h.record(&[("きょう".into(), "京".into())]);

    let result = convert_with_history(&dict, None, &h, "きょう");
    assert_eq!(result[0].surface, "京");
}

#[test]
fn test_convert_with_history_empty_history_matches_baseline() {
    let dict = test_dict();
    let h = UserHistory::new();

    let baseline = convert(&dict, None, "きょうはいいてんき");
    let with_history = convert_with_history(&dict, None, &h, "きょうはいいてんき");

    let baseline_surfaces: Vec<&str> = baseline.iter().map(|s| s.surface.as_str()).collect();
    let history_surfaces: Vec<&str> = with_history.iter().map(|s| s.surface.as_str()).collect();
    assert_eq!(baseline_surfaces, history_surfaces);
}

#[test]
fn test_convert_with_history_empty_input() {
    let dict = test_dict();
    let h = UserHistory::new();
    let result = convert_with_history(&dict, None, &h, "");
    assert!(result.is_empty());
}

#[test]
fn test_convert_nbest_with_history_promotes_learned() {
    let dict = test_dict();
    let mut h = UserHistory::new();
    h.record(&[("きょう".into(), "京".into())]);
    h.record(&[("きょう".into(), "京".into())]);

    let results = convert_nbest_with_history(&dict, None, &h, "きょう", 5);
    assert!(!results.is_empty());
    assert_eq!(results[0][0].surface, "京");
}

#[test]
fn test_convert_nbest_with_history_empty_input() {
    let dict = test_dict();
    let h = UserHistory::new();
    assert!(convert_nbest_with_history(&dict, None, &h, "", 5).is_empty());
    assert!(convert_nbest_with_history(&dict, None, &h, "きょう", 0).is_empty());
}

/// When history heavily boosts single-char alternatives, the Viterbi #1
/// (compound entry) must still appear in the n-best results.
#[test]
fn test_viterbi_best_preserved_despite_history_boost() {
    use crate::dict::DictEntry;

    // Dict with a compound entry and multiple single-char alternatives.
    // Viterbi #1 without history: "気がし" + "ます" (compound, lowest cost).
    let entries = vec![
        (
            "きがし".to_string(),
            vec![DictEntry {
                surface: "気がし".to_string(),
                cost: 3000,
                left_id: 0,
                right_id: 0,
            }],
        ),
        (
            "き".to_string(),
            vec![
                DictEntry {
                    surface: "機".into(),
                    cost: 5000,
                    left_id: 0,
                    right_id: 0,
                },
                DictEntry {
                    surface: "木".into(),
                    cost: 5500,
                    left_id: 0,
                    right_id: 0,
                },
                DictEntry {
                    surface: "黄".into(),
                    cost: 6000,
                    left_id: 0,
                    right_id: 0,
                },
                DictEntry {
                    surface: "基".into(),
                    cost: 6500,
                    left_id: 0,
                    right_id: 0,
                },
                DictEntry {
                    surface: "樹".into(),
                    cost: 7000,
                    left_id: 0,
                    right_id: 0,
                },
                DictEntry {
                    surface: "記".into(),
                    cost: 7500,
                    left_id: 0,
                    right_id: 0,
                },
            ],
        ),
        (
            "がし".to_string(),
            vec![DictEntry {
                surface: "がし".to_string(),
                cost: 2000,
                left_id: 0,
                right_id: 0,
            }],
        ),
        (
            "ます".to_string(),
            vec![DictEntry {
                surface: "ます".to_string(),
                cost: 2000,
                left_id: 0,
                right_id: 0,
            }],
        ),
    ];
    let dict = crate::dict::TrieDictionary::from_entries(entries);

    // Without history, Viterbi #1 should contain "気がし"
    let baseline = convert_nbest(&dict, None, "きがします", 5);
    let baseline_surfaces: Vec<String> = baseline
        .iter()
        .map(|path| path.iter().map(|s| s.surface.as_str()).collect())
        .collect();
    assert!(
        baseline_surfaces.contains(&"気がします".to_string()),
        "baseline should contain '気がします', got: {:?}",
        baseline_surfaces,
    );

    // Heavily boost all single-char "き→X" alternatives to push them above compound
    let mut h = UserHistory::new();
    for _ in 0..5 {
        h.record(&[("き".into(), "機".into())]);
        h.record(&[("き".into(), "木".into())]);
        h.record(&[("き".into(), "黄".into())]);
        h.record(&[("き".into(), "基".into())]);
        h.record(&[("き".into(), "樹".into())]);
        h.record(&[("き".into(), "記".into())]);
    }

    let with_history = convert_nbest_with_history(&dict, None, &h, "きがします", 5);
    let history_surfaces: Vec<String> = with_history
        .iter()
        .map(|path| path.iter().map(|s| s.surface.as_str()).collect())
        .collect();

    // The compound "気がします" should still be present despite history boosts
    assert!(
        history_surfaces.contains(&"気がします".to_string()),
        "Viterbi #1 '気がします' should be preserved after history reranking, got: {:?}",
        history_surfaces,
    );
}
