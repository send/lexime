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

    // Simulate the user selecting "気がします" once. In practice, grouping
    // merges segments into one, so record_history only records the whole-reading
    // unigram (sub-phrase learning is skipped for single-segment grouped paths).
    h.record(&[("きがします".into(), "気がします".into())]);

    // After a single explicit selection, the compound should become #1
    // thanks to the ×5 whole-path boost weight.
    let after_learn = convert_nbest_with_history(&dict, None, &h, "きがします", 5);
    let learned_surfaces: Vec<String> = after_learn
        .iter()
        .map(|path| path.iter().map(|s| s.surface.as_str()).collect())
        .collect();
    assert_eq!(
        learned_surfaces[0], "気がします",
        "after 1 selection, compound should be #1, got: {:?}",
        learned_surfaces,
    );
}

/// PR-G: a learned surface whose boost does not cover its price gap is the
/// #1 of the 1-best and of the N-best alike.
#[test]
fn one_best_equals_nbest_head_with_learned_whole_pair() {
    use crate::converter::testutil::entry;
    use crate::dict::TrieDictionary;
    let dict = TrieDictionary::from_entries(vec![(
        "かな".into(),
        vec![entry("仮名", 0), entry("可奈", 25000)],
    )]);
    let mut h = UserHistory::new();
    h.record(&[("かな".into(), "可奈".into())]);
    let joined = |p: &[ConvertedSegment]| p.iter().map(|s| s.surface.as_str()).collect::<String>();
    let one_best = joined(&convert_with_history(&dict, None, &h, "かな"));
    let nbest = convert_nbest_with_history(&dict, None, &h, "かな", 20);
    assert_eq!(one_best, "可奈");
    assert_eq!(joined(&nbest[0]), "可奈");
    // Without the learning 仮名 is #1: the boost alone does not flip it.
    assert_eq!(joined(&convert(&dict, None, "かな")), "仮名");
}

/// PR-G (G7): a number compound the user commits is learned on the same
/// terms as a lattice path, so it wins #1 back from a stale learned surface
/// on price. Before G7 the compound (created after history reranking) was
/// never learned, and 荷重 — learned once, long ago — stayed #1 however
/// often 二十 was committed.
#[test]
fn learned_number_compound_outprices_stale_learned_surface() {
    use crate::converter::explain::explain;
    use crate::dict::{DictEntry, TrieDictionary};
    use crate::user_history::now_epoch;

    let e = |surface: &str, cost: i16| DictEntry {
        surface: surface.into(),
        cost,
        left_id: 0,
        right_id: 0,
    };
    let dict = TrieDictionary::from_entries(vec![(
        "にじゅう".into(),
        vec![e("二重", 3000), e("荷重", 9000)],
    )]);
    let now = now_epoch();
    let mut h = UserHistory::new();
    h.record_at(&[("にじゅう".into(), "荷重".into())], now - 60 * 24 * 3600);
    for _ in 0..10 {
        h.record_at(&[("にじゅう".into(), "二十".into())], now - 24 * 3600);
    }
    let joined = |p: &[ConvertedSegment]| p.iter().map(|s| s.surface.as_str()).collect::<String>();

    let nbest = convert_nbest_with_history(&dict, None, &h, "にじゅう", 5);
    assert_eq!(joined(&nbest[0]), "二十", "N-best #1");
    assert_eq!(
        joined(&convert_with_history(&dict, None, &h, "にじゅう")),
        "二十",
        "1-best"
    );
    let list = crate::candidates::generate_candidates(&dict, None, Some(&h), "にじゅう", 20);
    assert_eq!(list.surfaces[0], "二十", "candidate list #1");

    let ex = explain(&dict, None, Some(&h), "にじゅう", 5);
    let top = &ex.paths[0];
    let surface: String = top.segments.iter().map(|s| s.surface.as_str()).collect();
    assert_eq!(surface, "二十");
    assert!(
        top.history_breakdown.whole_path_boost > 0,
        "explain reports the compound's own boost"
    );
}
