use crate::converter::lattice::Lattice;
use crate::converter::postprocess::NoopObserver;
use crate::converter::rewriter::{
    run_policy_rewriters, NumericRewriter, PolicyPrice, PolicyRewriter, PolicyRule,
    NUMERIC_POLICY_OFFSET,
};
use crate::converter::testutil::counter_test_conn;
use crate::converter::viterbi::{PathOrigin, RichSegment, ScoredPath};
use crate::dict::connection::ConnectionMatrix;

/// The rewriter's candidates without their prices.
fn generated(rw: &dyn PolicyRewriter, reading: &str) -> Vec<ScoredPath> {
    rw.policy_candidates(reading)
        .into_iter()
        .map(|(p, _)| p)
        .collect()
}

// ───────────────────────────────────────────────────────────────────────
// NumericRewriter: pure-number mode (no lattice / no counter detection)
// ───────────────────────────────────────────────────────────────────────

#[test]
fn test_numeric_rewriter_generates_candidates() {
    let rw = NumericRewriter {
        lattice: None,
        connection: None,
        anchor: 3000,
    };

    let result = generated(&rw, "にじゅうさん");

    assert_eq!(result.len(), 3);
    assert_eq!(result[0].surface_key(), "二十三");
    assert_eq!(result[0].viterbi_cost, 3000); // compound → the anchor
    assert_eq!(result[1].surface_key(), "23");
    assert_eq!(result[1].viterbi_cost, 3000 + NUMERIC_POLICY_OFFSET);
    assert_eq!(result[2].surface_key(), "２３");
    assert_eq!(result[2].viterbi_cost, 3000 + NUMERIC_POLICY_OFFSET + 1);
}

#[test]
fn test_numeric_rewriter_kanji_duplicate_skip() {
    let rw = NumericRewriter {
        lattice: None,
        connection: None,
        anchor: 3000,
    };
    let mut paths = vec![ScoredPath::new(
        vec![RichSegment {
            reading: "にじゅうさん".into(),
            surface: "二十三".into(),
            left_id: 10,
            right_id: 10,
            word_cost: 0,
        }],
        3000,
        PathOrigin::Viterbi,
    )];

    run_policy_rewriters(&[&rw], &mut paths, "にじゅうさん", None, &mut NoopObserver);

    // Kanji already exists, only halfwidth + fullwidth added
    assert_eq!(paths.len(), 3);
    assert_eq!(paths[0].surface_key(), "二十三");
    assert_eq!(paths[1].surface_key(), "23");
    assert_eq!(paths[2].surface_key(), "２３");
}

#[test]
fn test_numeric_rewriter_single_char_kanji_low_priority() {
    let rw = NumericRewriter {
        lattice: None,
        connection: None,
        anchor: 3000,
    };
    let mut paths = vec![ScoredPath::new(
        vec![RichSegment {
            reading: "じゅう".into(),
            surface: "中".into(),
            left_id: 10,
            right_id: 10,
            word_cost: 0,
        }],
        3000,
        PathOrigin::Viterbi,
    )];

    run_policy_rewriters(&[&rw], &mut paths, "じゅう", None, &mut NoopObserver);

    // 十 is single-char → a fallback (anchor + NUMERIC_POLICY_OFFSET), all after 中
    assert_eq!(paths[0].surface_key(), "中");
    let kanji = paths.iter().find(|p| p.surface_key() == "十").unwrap();
    assert_eq!(kanji.viterbi_cost, 3000 + NUMERIC_POLICY_OFFSET);
}

#[test]
fn test_numeric_rewriter_skips_non_numeric() {
    let rw = NumericRewriter {
        lattice: None,
        connection: None,
        anchor: 1000,
    };

    let result = generated(&rw, "きょう");

    assert!(
        result.is_empty(),
        "should not generate numeric candidates for non-numeric input"
    );
}

#[test]
fn test_numeric_rewriter_skips_duplicate() {
    let rw = NumericRewriter {
        lattice: None,
        connection: None,
        anchor: 1000,
    };
    let mut paths = vec![ScoredPath::new(
        vec![RichSegment {
            reading: "いち".into(),
            surface: "1".into(),
            left_id: 0,
            right_id: 0,
            word_cost: 0,
        }],
        1000,
        PathOrigin::Viterbi,
    )];

    run_policy_rewriters(&[&rw], &mut paths, "いち", None, &mut NoopObserver);

    // Half-width "1" already exists; kanji "一" (single-char) + full-width "１" added
    assert_eq!(paths.len(), 3);
    // All have high cost, so they come after "1"
    assert_eq!(paths[0].surface_key(), "1");
    assert!(paths.iter().any(|p| p.surface_key() == "一"));
    assert!(paths.iter().any(|p| p.surface_key() == "１"));
}

// ───────────────────────────────────────────────────────────────────────
// NumericRewriter: counter (助数詞) mode
// ───────────────────────────────────────────────────────────────────────

#[test]
fn test_numeric_counter_generates_kanji_compound() {
    // Reading: さんぜんえん (3000円). Lattice has the counter node 円(えん)
    // ending at the tail. Expect a kanji compound 三千円 to be generated.
    let counter_id: u16 = 7;
    let conn = counter_test_conn(8, counter_id);
    let lattice = Lattice::from_test_nodes(
        "さんぜんえん",
        &[
            // (start, end, reading, surface, cost, left_id, right_id)
            (4, 6, "えん", "円", 1000, counter_id, counter_id),
            (4, 6, "えん", "園", 1500, 1, 1), // non-counter homophone
        ],
    );
    let rw = NumericRewriter {
        lattice: Some(&lattice),
        connection: Some(&conn),
        anchor: 5000,
    };

    let result = generated(&rw, "さんぜんえん");

    let kanji = result
        .iter()
        .find(|p| p.surface_key() == "三千円")
        .expect("should generate 三千円");
    // Cheapest counter at the position is anchored at anchor - COUNTER_COMPOUND_DISCOUNT
    // (lifts the kanji compound above the existing top-1).
    assert_eq!(kanji.viterbi_cost, 4500);
    assert!(result.iter().any(|p| p.surface_key() == "3000円"));
    assert!(result.iter().any(|p| p.surface_key() == "３０００円"));
    // Non-counter homophone 園 should NOT spawn a number candidate.
    assert!(!result.iter().any(|p| p.surface_key() == "三千園"));
}

#[test]
fn test_numeric_counter_dedupes_multi_pos_counter() {
    // Same surface 円 with two counter POS variants — only one set of
    // candidates should be emitted.
    let counter_id_a: u16 = 7;
    let counter_id_b: u16 = 5;
    let n: usize = 8;
    let header = format!("{n} {n}\n");
    let costs: String = (0..(n * n)).map(|_| "0\n".to_string()).collect();
    let text = format!("{header}{costs}");
    let mut roles = vec![0u8; n];
    roles[counter_id_a as usize] = 7;
    roles[counter_id_b as usize] = 7;
    let conn = ConnectionMatrix::from_text_with_roles(&text, 0, 0, roles).unwrap();
    let lattice = Lattice::from_test_nodes(
        "ごえん",
        &[
            (1, 3, "えん", "円", 1000, counter_id_a, counter_id_a),
            (1, 3, "えん", "円", 2000, counter_id_b, counter_id_b),
        ],
    );
    let rw = NumericRewriter {
        lattice: Some(&lattice),
        connection: Some(&conn),
        anchor: 4000,
    };

    let result = generated(&rw, "ごえん");

    let kanji_count = result.iter().filter(|p| p.surface_key() == "五円").count();
    assert_eq!(
        kanji_count, 1,
        "duplicate counter POS variants should dedupe"
    );
}

#[test]
fn test_numeric_counter_skips_when_prefix_not_a_number() {
    // Reading: あいえん — counter 円 at tail, but prefix "あい" doesn't parse
    // as a number, so no counter candidate should be generated.
    let counter_id: u16 = 7;
    let conn = counter_test_conn(8, counter_id);
    let lattice = Lattice::from_test_nodes(
        "あいえん",
        &[(2, 4, "えん", "円", 1000, counter_id, counter_id)],
    );
    let rw = NumericRewriter {
        lattice: Some(&lattice),
        connection: Some(&conn),
        anchor: 4000,
    };

    let result = generated(&rw, "あいえん");

    assert!(result.iter().all(|p| !p.surface_key().contains('円')));
}

#[test]
fn test_numeric_counter_disabled_without_lattice_or_conn() {
    // No lattice/connection → counter mode must not fire (only pure-number
    // path runs, and "さんぜんえん" doesn't parse as a pure number).
    let rw = NumericRewriter {
        lattice: None,
        connection: None,
        anchor: 5000,
    };

    let result = generated(&rw, "さんぜんえん");

    assert!(result.is_empty());
}

#[test]
fn test_numeric_counter_deterministic_order_on_cost_tie() {
    // Two counter surfaces with identical word_cost. Sorting by
    // (cost, surface) must produce a deterministic emit order — without it,
    // HashMap iteration could swap the top candidate run-to-run.
    let counter_id: u16 = 7;
    let conn = counter_test_conn(8, counter_id);
    let lattice = Lattice::from_test_nodes(
        "ごねん",
        &[
            (1, 3, "ねん", "年", 100, counter_id, counter_id),
            (1, 3, "ねん", "念", 100, counter_id, counter_id),
        ],
    );
    let rw = NumericRewriter {
        lattice: Some(&lattice),
        connection: Some(&conn),
        anchor: 4000,
    };

    let mut emit_orders: Vec<Vec<String>> = Vec::new();
    for _ in 0..5 {
        let result = generated(&rw, "ごねん");
        let kanji_order: Vec<String> = result
            .iter()
            .filter(|p| p.surface_key().starts_with("五"))
            .map(|p| p.surface_key())
            .collect();
        emit_orders.push(kanji_order);
    }
    let first = &emit_orders[0];
    assert!(
        emit_orders.iter().all(|o| o == first),
        "emit order must be stable across runs, got: {emit_orders:?}"
    );
    // Sort key is surface (lexicographic). 念 (U+5FF5) > 年 (U+5E74), so
    // 五年 emits before 五念.
    assert_eq!(first, &vec!["五年".to_string(), "五念".to_string()]);
}

#[test]
fn test_numeric_counter_extreme_cost_no_overflow() {
    // i16::MAX counter and i16::MIN+ counter — the (cand.cost - cheapest)
    // diff would overflow as plain i16 subtraction. Widening to i64 first
    // keeps the rewriter safe on extreme dictionary costs.
    let counter_id: u16 = 7;
    let conn = counter_test_conn(8, counter_id);
    let lattice = Lattice::from_test_nodes(
        "ごえん",
        &[
            (1, 3, "えん", "円", i16::MIN + 1, counter_id, counter_id),
            (1, 3, "えん", "園", i16::MAX, counter_id, counter_id),
        ],
    );
    let rw = NumericRewriter {
        lattice: Some(&lattice),
        connection: Some(&conn),
        anchor: 4000,
    };

    // Should not panic; should still emit candidates for both counters.
    let result = generated(&rw, "ごえん");
    assert!(result.iter().any(|p| p.surface_key() == "五円"));
    assert!(result.iter().any(|p| p.surface_key() == "五園"));
}

/// PR-G (G1′): a learned index 0 outranks the number compound, even when it
/// costs more than the anchor the compound is priced at; with nothing
/// learned the compound still takes index 0 (#239).
#[test]
fn learned_surface_outranks_number_compound() {
    let rw = NumericRewriter {
        lattice: None,
        connection: None,
        anchor: 3000,
    };
    // Production boosts what the Override stage creates as history reranking
    // boosted the list: a surface learned for the reading is learned wherever
    // it comes from. Here に十三 and a learned 二十三 carry whole 1000.
    let learned = |surface: &str| surface == "に十三" || surface == "二十三";
    let path = |surface: &str, cost: i64, whole: i64| {
        let mut p = ScoredPath::single(
            "にじゅうさん".into(),
            surface.into(),
            cost,
            PathOrigin::Viterbi,
        );
        p.whole_path_boost = whole;
        p.history_boost = whole;
        p
    };
    let run = |paths: &mut Vec<ScoredPath>, compound_learned: bool| {
        let boost = |p: &mut ScoredPath| {
            if compound_learned && learned(&p.surface_key()) {
                p.viterbi_cost -= 1000;
                p.history_boost = 1000;
                p.whole_path_boost = 1000;
            }
        };
        run_policy_rewriters(
            &[&rw as &dyn PolicyRewriter],
            paths,
            "にじゅうさん",
            Some(&boost),
            &mut NoopObserver,
        )
    };

    // Learned に十三 costs 5000 (> anchor 3000), the compound is not learned:
    // に十三 keeps index 0.
    let mut paths = vec![path("に十三", 5000, 1000)];
    run(&mut paths, false);
    assert_eq!(paths[0].surface_key(), "に十三");
    assert_eq!(paths[1].surface_key(), "二十三");

    // The learned surface is the compound the rewriter generates: learned on
    // both sides, so the compound replaces it with its own path, priced
    // anchor 3000 − its own boost.
    let mut paths = vec![path("二十三", 5000, 1000)];
    run(&mut paths, true);
    assert_eq!(paths[0].surface_key(), "二十三");
    assert_eq!(
        (paths[0].origin, paths[0].priced_by),
        (PathOrigin::Numeric, PathOrigin::Numeric)
    );
    assert_eq!(
        (paths[0].viterbi_cost, paths[0].whole_path_boost),
        (2000, 1000)
    );

    // Nothing learned: the compound takes index 0.
    let mut paths = vec![path("に十三", 5000, 0)];
    run(&mut paths, false);
    assert_eq!(paths[0].surface_key(), "二十三");
}

/// A learned index 0 takes the Override price of its own surface like any
/// other position: adding another learned path must not change which
/// learned surface is #1 (it did while index 0 was frozen for learned
/// candidates too).
#[test]
fn learned_index0_takes_its_compound_price() {
    let rw = NumericRewriter {
        lattice: None,
        connection: None,
        anchor: 3000,
    };
    let boost = |p: &mut ScoredPath| {
        let b = match p.surface_key().as_str() {
            "二十" | "荷重" => 1000,
            "20" => 7000,
            _ => return,
        };
        p.viterbi_cost -= b;
        p.history_boost = b;
        p.whole_path_boost = b;
    };
    let learned = |surface: &str, pre: i64| {
        let mut p = ScoredPath::single("にじゅう".into(), surface.into(), pre, PathOrigin::Viterbi);
        boost(&mut p);
        p
    };
    let run = |mut paths: Vec<ScoredPath>| {
        run_policy_rewriters(
            &[&rw as &dyn PolicyRewriter],
            &mut paths,
            "にじゅう",
            Some(&boost),
            &mut NoopObserver,
        );
        paths.iter().map(|p| p.surface_key()).collect::<Vec<_>>()
    };
    // 二十 at index 0, alone or below another learned path: #1 either way.
    assert_eq!(run(vec![learned("二十", 9000)])[0], "二十");
    assert_eq!(
        run(vec![learned("荷重", 5000), learned("二十", 9000)])[0],
        "二十"
    );
}

// ───────────────────────────────────────────────────────────────────────
// R-b: a Latin unit (pure-Latin counter) pairs with half-width digits only
// and is not the price anchor
// ───────────────────────────────────────────────────────────────────────

/// The counter candidates for `さんびょう` from `counters` (surface, cost),
/// all on the counter POS and ending at the tail.
fn counter_candidates(counters: &[(&str, i16)]) -> Vec<(ScoredPath, PolicyPrice)> {
    let counter_id: u16 = 7;
    let conn = counter_test_conn(8, counter_id);
    let nodes: Vec<(usize, usize, &str, &str, i16, u16, u16)> = counters
        .iter()
        .map(|&(surface, cost)| (2, 5, "びょう", surface, cost, counter_id, counter_id))
        .collect();
    let lattice = Lattice::from_test_nodes("さんびょう", &nodes);
    let rw = NumericRewriter {
        lattice: Some(&lattice),
        connection: Some(&conn),
        anchor: 5000,
    };
    rw.policy_candidates("さんびょう")
}

fn price_of(cands: &[(ScoredPath, PolicyPrice)], surface: &str) -> Option<PolicyPrice> {
    cands
        .iter()
        .find(|(p, _)| p.surface_key() == surface)
        .map(|&(_, price)| price)
}

#[test]
fn latin_unit_counter_is_half_width_only() {
    let cands = counter_candidates(&[("秒", 3000), ("s", 3001)]);
    assert!(price_of(&cands, "3s").is_some());
    assert!(price_of(&cands, "三s").is_none());
    assert!(price_of(&cands, "３s").is_none());
    // A non-Latin counter keeps all three forms.
    for s in ["三秒", "3秒", "３秒"] {
        assert!(price_of(&cands, s).is_some(), "{s}");
    }
}

#[test]
fn mixed_and_letterless_counters_keep_all_three_forms() {
    for unit in ["kバイト", "%"] {
        let cands = counter_candidates(&[(unit, 3000)]);
        for form in [format!("三{unit}"), format!("3{unit}"), format!("３{unit}")] {
            assert!(price_of(&cands, &form).is_some(), "{form}");
        }
    }
}

/// M3: the kanji compound's price does not depend on whether a Latin unit
/// shares the tail.
#[test]
fn a_latin_unit_does_not_move_the_kanji_compound_price() {
    let without = counter_candidates(&[("秒", 3000)]);
    for latin_cost in [2000, 3001, 9000] {
        let with = counter_candidates(&[("秒", 3000), ("s", latin_cost)]);
        assert_eq!(
            price_of(&with, "三秒"),
            price_of(&without, "三秒"),
            "{latin_cost}"
        );
        assert_eq!(
            price_of(&with, "3秒"),
            price_of(&without, "3秒"),
            "{latin_cost}"
        );
    }
}

/// N1: a Latin unit cheaper than the anchor gets offset 0, not a negative
/// one; with no non-Latin counter at the tail, the cheapest Latin unit is the
/// anchor.
#[test]
fn a_latin_unit_offset_is_never_negative() {
    let cands = counter_candidates(&[("秒", 3000), ("s", 1000)]);
    let p = price_of(&cands, "3s").unwrap();
    assert_eq!((p.rule, p.cost_offset), (PolicyRule::CounterFallback, 0));
    let cands = counter_candidates(&[("s", 1000), ("sec", 1500)]);
    assert_eq!(price_of(&cands, "3s").unwrap().cost_offset, 0);
    assert_eq!(price_of(&cands, "3sec").unwrap().cost_offset, 500);
}

/// On an equal price (the Latin unit cheaper than the anchor, offset clamped
/// to 0) the Japanese-name counter's form is emitted before the Latin unit's.
#[test]
fn equal_priced_japanese_counter_precedes_the_latin_unit() {
    let cands = counter_candidates(&[("秒", 3000), ("s", 1000)]);
    let pos = |surface: &str| cands.iter().position(|(p, _)| p.surface_key() == surface);
    assert!(pos("3秒").unwrap() < pos("3s").unwrap());
    let (jp, latin) = (
        price_of(&cands, "3秒").unwrap(),
        price_of(&cands, "3s").unwrap(),
    );
    assert_eq!((jp.rule, jp.cost_offset), (PolicyRule::CounterFallback, 0));
    assert_eq!(jp, latin);
}

/// The final list, not only the emission, puts the Japanese-name counter's
/// form before the Latin unit's on an equal price: the Override stage keeps
/// emission order among its own candidates.
#[test]
fn equal_priced_japanese_counter_precedes_the_latin_unit_in_the_final_list() {
    let counter_id: u16 = 7;
    let conn = counter_test_conn(8, counter_id);
    let nodes = [
        (2, 5, "びょう", "秒", 3000, counter_id, counter_id),
        (2, 5, "びょう", "s", 1000, counter_id, counter_id),
    ];
    let lattice = Lattice::from_test_nodes("さんびょう", &nodes);
    let rw = NumericRewriter {
        lattice: Some(&lattice),
        connection: Some(&conn),
        anchor: 5000,
    };
    let mut paths = vec![ScoredPath::new(
        vec![RichSegment {
            reading: "さんびょう".into(),
            surface: "さん秒".into(),
            left_id: 0,
            right_id: 0,
            word_cost: 0,
        }],
        100,
        PathOrigin::Viterbi,
    )];
    run_policy_rewriters(&[&rw], &mut paths, "さんびょう", None, &mut NoopObserver);
    let pos = |s: &str| paths.iter().position(|p| p.surface_key() == s);
    let (jp, latin) = (pos("3秒").unwrap(), pos("3s").unwrap());
    assert_eq!(paths[jp].viterbi_cost, paths[latin].viterbi_cost);
    assert!(
        jp < latin,
        "{:?}",
        paths.iter().map(|p| p.surface_key()).collect::<Vec<_>>()
    );
}
