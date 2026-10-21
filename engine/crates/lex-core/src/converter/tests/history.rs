use super::*;
use crate::converter::testutil::{joined, test_dict};
use crate::user_history::{now_epoch, UserHistory};

#[test]
fn test_convert_with_history_promotes_learned() {
    let now = now_epoch();
    let dict = test_dict();
    // "きょう" has 今日(3000) and 京(5000). Without history, 今日 wins.
    let baseline = convert(&dict, None, "きょう");
    assert_eq!(baseline[0].surface, "今日");

    // After learning "京", it should be promoted
    let mut h = UserHistory::new();
    h.record(&[("きょう".into(), "京".into())]);
    h.record(&[("きょう".into(), "京".into())]);

    let result = crate::converter::ConversionContext::new(&dict, None)
        .with_history(Some(&h), now)
        .convert("きょう");
    assert_eq!(result[0].surface, "京");
}

#[test]
fn test_convert_with_history_empty_history_matches_baseline() {
    let now = now_epoch();
    let dict = test_dict();
    let h = UserHistory::new();

    let baseline = convert(&dict, None, "きょうはいいてんき");
    let with_history = crate::converter::ConversionContext::new(&dict, None)
        .with_history(Some(&h), now)
        .convert("きょうはいいてんき");

    let baseline_surfaces: Vec<&str> = baseline.iter().map(|s| s.surface.as_str()).collect();
    let history_surfaces: Vec<&str> = with_history.iter().map(|s| s.surface.as_str()).collect();
    assert_eq!(baseline_surfaces, history_surfaces);
}

#[test]
fn test_convert_with_history_empty_input() {
    let now = now_epoch();
    let dict = test_dict();
    let h = UserHistory::new();
    let result = crate::converter::ConversionContext::new(&dict, None)
        .with_history(Some(&h), now)
        .convert("");
    assert!(result.is_empty());
}

#[test]
fn test_convert_nbest_with_history_promotes_learned() {
    let now = now_epoch();
    let dict = test_dict();
    let mut h = UserHistory::new();
    h.record(&[("きょう".into(), "京".into())]);
    h.record(&[("きょう".into(), "京".into())]);

    let results = crate::converter::ConversionContext::new(&dict, None)
        .with_history(Some(&h), now)
        .convert_nbest("きょう", 5);
    assert!(!results.is_empty());
    assert_eq!(results[0][0].surface, "京");
}

#[test]
fn test_convert_nbest_with_history_empty_input() {
    let now = now_epoch();
    let dict = test_dict();
    let h = UserHistory::new();
    assert!(crate::converter::ConversionContext::new(&dict, None)
        .with_history(Some(&h), now)
        .convert_nbest("", 5)
        .is_empty());
    assert!(crate::converter::ConversionContext::new(&dict, None)
        .with_history(Some(&h), now)
        .convert_nbest("きょう", 0)
        .is_empty());
}

/// When history heavily boosts single-char alternatives, the Viterbi #1
/// (compound entry) must still appear in the n-best results.
#[test]
fn test_viterbi_best_preserved_despite_history_boost() {
    let now = now_epoch();
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

    let with_history = crate::converter::ConversionContext::new(&dict, None)
        .with_history(Some(&h), now)
        .convert_nbest("きがします", 5);
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

    // After a single explicit selection the compound is a learned whole
    // path, so it takes #1 (PR-G) whatever the size of its boost. The boost's
    // magnitude is tested in `test_history_rerank_unigram_boost_reorders`.
    let after_learn = crate::converter::ConversionContext::new(&dict, None)
        .with_history(Some(&h), now)
        .convert_nbest("きがします", 5);
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
    let now = now_epoch();
    use crate::converter::testutil::entry;
    use crate::dict::TrieDictionary;
    let dict = TrieDictionary::from_entries(vec![(
        "かな".into(),
        vec![entry("仮名", 0), entry("可奈", 25000)],
    )]);
    let mut h = UserHistory::new();
    h.record(&[("かな".into(), "可奈".into())]);
    let one_best = joined(
        &crate::converter::ConversionContext::new(&dict, None)
            .with_history(Some(&h), now)
            .convert("かな"),
    );
    let nbest = crate::converter::ConversionContext::new(&dict, None)
        .with_history(Some(&h), now)
        .convert_nbest("かな", 20);
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

    let nbest = crate::converter::ConversionContext::new(&dict, None)
        .with_history(Some(&h), now)
        .convert_nbest("にじゅう", 5);
    assert_eq!(joined(&nbest[0]), "二十", "N-best #1");
    assert_eq!(
        joined(
            &crate::converter::ConversionContext::new(&dict, None)
                .with_history(Some(&h), now)
                .convert("にじゅう")
        ),
        "二十",
        "1-best"
    );
    let list = crate::candidates::generate_candidates(
        &crate::converter::ConversionContext::new(&dict, None).with_history(Some(&h), now),
        "にじゅう",
        20,
    );
    assert_eq!(list.surfaces[0], "二十", "candidate list #1");

    let ex = explain(
        &crate::converter::ConversionContext::new(&dict, None).with_history(Some(&h), now),
        "にじゅう",
        5,
    );
    let top = &ex.paths[0];
    let surface: String = top.segments.iter().map(|s| s.surface.as_str()).collect();
    assert_eq!(surface, "二十");
    assert!(
        top.history_breakdown.whole_path_boost > 0,
        "explain reports the compound's own boost"
    );
}

/// A1/T-A1: the 1-best is the N-best of `n = 1`, one path and one population.
/// Before #361's follow-up the n = 1 head ran at 3 (no history) / 50 (history)
/// against the 1-best's 10 / 30, so a path inside only one of them split the
/// two. 金谷 is Viterbi's 6th and rerank's argmin without a history; 学習 is
/// Viterbi's 41st, learned once (inside 50, outside 30).
#[test]
fn one_best_is_the_nbest_of_one() {
    use crate::converter::testutil::{deep_argmin_kanaya, deep_learned_kanaya, DEEP_LEARNED};
    let now = now_epoch();
    let (shallow, conn_s) = deep_argmin_kanaya(5);
    let (deep, conn_d) = deep_learned_kanaya(40);
    let mut learned = UserHistory::new();
    learned.record(&[("かなや".into(), DEEP_LEARNED.into())]);
    for (dict, conn, history) in [
        (&shallow, &conn_s, None),
        (&deep, &conn_d, None),
        (&deep, &conn_d, Some(&learned)),
    ] {
        let ctx =
            crate::converter::ConversionContext::new(dict, Some(conn)).with_history(history, now);
        let lattice = ctx.build_lattice("かなや");
        let one_best = joined(&ctx.convert_from_lattice(&lattice));
        let head = joined(&ctx.convert_nbest_from_lattice(&lattice, 1)[0]);
        assert_eq!(one_best, head, "history={}", history.is_some());
    }
    // The fixtures reach what they claim: a population of 10 sees 金谷
    // (6th), the learned 学習 is the list's #1 and outside the 1-best's 30.
    let ctx = crate::converter::ConversionContext::new(&shallow, Some(&conn_s));
    assert_eq!(joined(&ctx.convert("かなや")), "金谷");
    let ctx = crate::converter::ConversionContext::new(&deep, Some(&conn_d))
        .with_history(Some(&learned), now);
    assert_eq!(joined(&ctx.convert_nbest("かなや", 20)[0]), DEEP_LEARNED);
    assert_ne!(joined(&ctx.convert("かなや")), DEEP_LEARNED);
}

/// A1/T-A2: the population rule's values. T-A1 compares the 1-best with the
/// n = 1 head, so it cannot see both moving together (both 50); the 1-best's
/// own budget (it runs on every keystroke) and the default list's 60 are
/// pinned here by value.
#[test]
fn population_values() {
    for ((n, history), want) in [
        ((1, false), 10),
        ((1, true), 30),
        ((2, false), 10),
        ((3, false), 10),
        ((4, false), 12),
        ((2, true), 50),
        ((5, true), 50),
        ((20, false), 60),
        ((20, true), 60),
    ] {
        assert_eq!(population(n, history), want, "n={n} history={history}");
    }
}

/// 二段 / 二弾: a number + counter (弾 and 段, counters at 0 and 50) whose
/// lattice path 二|段 costs 2050 over the anchor 二|弾 (the connection
/// 二 → 段 adds 2000; a compound's counter offset is the word cost only, 50).
/// 段 committed on its own once boosts the lattice path's 段 segment by
/// 1500: enough to lift the compound 二段 over 二弾 by its segments, not
/// enough to lift 二|段 into the 1-best's one model path. So only the
/// N-best keeps a lattice path with the compound's surface.
fn nidan() -> (
    crate::dict::TrieDictionary,
    crate::dict::connection::ConnectionMatrix,
) {
    use crate::converter::testutil::entry_with_id;
    let e = entry_with_id;
    let dict = crate::dict::TrieDictionary::from_entries(vec![
        ("に".into(), vec![e("二", 0, 1)]),
        ("だん".into(), vec![e("弾", 0, 2), e("段", 50, 3)]),
    ]);
    let n = 4;
    let costs: String = (0..n * n)
        .map(|i| {
            if i == n + 3 || i == 3 * n + 1 {
                "2000\n"
            } else {
                "0\n"
            }
        })
        .collect();
    let conn = crate::dict::connection::ConnectionMatrix::from_text_with_roles(
        &format!("{n} {n}\n{costs}"),
        0,
        0,
        vec![0, 0, 7, 7],
    )
    .unwrap();
    (dict, conn)
}

/// §29-6 T-393-6 / T-393-9 (#393): a number compound's path does not depend
/// on the population. The 1-best and the N-best price 二段 from the same
/// anchor and write the compound itself, one segment, boosted on its own —
/// whether or not a lattice path with its surface survived the cut — so the
/// two widths agree, and explain recomputes the boost from those segments.
#[test]
fn a_number_compound_is_boosted_the_same_at_every_width() {
    use crate::converter::explain::explain;
    let now = now_epoch();
    let (dict, conn) = nidan();
    let mut h = UserHistory::new();
    h.record_at(&[("だん".into(), "段".into())], now);
    let ctx =
        crate::converter::ConversionContext::new(&dict, Some(&conn)).with_history(Some(&h), now);
    // The fixture reaches its case: the N-best's population holds the
    // lattice path 二|段 (the dictionary has no 二段 node).
    let bare = crate::converter::ConversionContext::new(&dict, Some(&conn));
    assert!(
        explain(&bare, "にだん", 20)
            .model_paths
            .iter()
            .any(|p| p.surface == "二段" && p.origin == crate::converter::PathOrigin::Viterbi),
        "the N-best holds 二|段"
    );

    let one_best = joined(&ctx.convert("にだん"));
    let list = joined(&ctx.convert_nbest("にだん", 20)[0]);
    assert_eq!((one_best.as_str(), list.as_str()), ("二弾", "二弾"));

    let compound = |n: usize| {
        let result = explain(&ctx, "にだん", n);
        assert!(result.policy_price_inconsistent.is_empty());
        let p = result.path("二段").expect("二段 listed");
        assert_eq!(
            p.history_breakdown.applied(p.history_segment_count),
            p.history_boost,
            "n = {n}"
        );
        (
            p.segments.len(),
            p.history_segment_count,
            p.origin,
            p.priced_by,
            p.history_boost,
            p.final_cost,
        )
    };
    let (one, all) = (compound(1), compound(20));
    assert_eq!(one, all);
    assert_eq!(
        all,
        (
            1,
            1,
            crate::converter::PathOrigin::Numeric,
            crate::converter::PathOrigin::Numeric,
            0,
            one.5
        )
    );
}
