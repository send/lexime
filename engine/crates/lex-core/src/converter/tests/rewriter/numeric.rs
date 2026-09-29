use crate::converter::lattice::Lattice;
use crate::converter::rewriter::{run_rewriters, NumericRewriter, RewriteStage, Rewriter};
use crate::converter::viterbi::{PathOrigin, RichSegment, ScoredPath};
use crate::dict::connection::ConnectionMatrix;

/// Build a tiny ConnectionMatrix where POS id `counter_id` is tagged as
/// counter (role 7). Other ids default to ContentWord (role 0).
fn counter_test_conn(num_ids: u16, counter_id: u16) -> ConnectionMatrix {
    let n = num_ids as usize;
    let header = format!("{n} {n}\n");
    let costs: String = (0..(n * n)).map(|_| "0\n".to_string()).collect();
    let text = format!("{header}{costs}");
    let mut roles = vec![0u8; n];
    roles[counter_id as usize] = 7; // ROLE_COUNTER
    ConnectionMatrix::from_text_with_roles(&text, 0, 0, roles).unwrap()
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
    let paths = vec![ScoredPath::new(
        vec![RichSegment {
            reading: "にじゅうさん".into(),
            surface: "に十三".into(),
            left_id: 10,
            right_id: 10,
            word_cost: 0,
        }],
        3000,
        PathOrigin::Viterbi,
    )];

    let result = rw.generate(&paths, "にじゅうさん");

    assert_eq!(result.len(), 3);
    assert_eq!(result[0].surface_key(), "二十三");
    assert_eq!(result[0].viterbi_cost, 3000); // compound → best_cost
    assert_eq!(result[1].surface_key(), "23");
    assert_eq!(result[1].viterbi_cost, 3000 + 5000);
    assert_eq!(result[2].surface_key(), "２３");
    assert_eq!(result[2].viterbi_cost, 3000 + 5001);
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

    run_rewriters(
        &[&rw],
        &mut paths,
        "にじゅうさん",
        RewriteStage::Override,
        None,
    );

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

    run_rewriters(&[&rw], &mut paths, "じゅう", RewriteStage::Override, None);

    // 十 is single-char → base_cost (not best_cost), all after 中
    assert_eq!(paths[0].surface_key(), "中");
    let kanji = paths.iter().find(|p| p.surface_key() == "十").unwrap();
    assert_eq!(kanji.viterbi_cost, 3000 + 5000);
}

#[test]
fn test_numeric_rewriter_skips_non_numeric() {
    let rw = NumericRewriter {
        lattice: None,
        connection: None,
        anchor: 1000,
    };
    let paths = vec![ScoredPath::new(
        vec![RichSegment {
            reading: "きょう".into(),
            surface: "今日".into(),
            left_id: 0,
            right_id: 0,
            word_cost: 0,
        }],
        1000,
        PathOrigin::Viterbi,
    )];

    let result = rw.generate(&paths, "きょう");

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

    run_rewriters(&[&rw], &mut paths, "いち", RewriteStage::Override, None);

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
    let paths = vec![ScoredPath::new(
        vec![RichSegment {
            reading: "さんぜんえん".into(),
            surface: "産前園".into(),
            left_id: 1,
            right_id: 1,
            word_cost: 0,
        }],
        5000,
        PathOrigin::Viterbi,
    )];

    let result = rw.generate(&paths, "さんぜんえん");

    let kanji = result
        .iter()
        .find(|p| p.surface_key() == "三千円")
        .expect("should generate 三千円");
    // Cheapest counter at the position is anchored at best_cost - 500
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
    let paths = vec![ScoredPath::new(
        vec![RichSegment {
            reading: "ごえん".into(),
            surface: "ご縁".into(),
            left_id: 1,
            right_id: 1,
            word_cost: 0,
        }],
        4000,
        PathOrigin::Viterbi,
    )];

    let result = rw.generate(&paths, "ごえん");

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
    let paths = vec![ScoredPath::new(
        vec![RichSegment {
            reading: "あいえん".into(),
            surface: "愛縁".into(),
            left_id: 1,
            right_id: 1,
            word_cost: 0,
        }],
        4000,
        PathOrigin::Viterbi,
    )];

    let result = rw.generate(&paths, "あいえん");

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
    let paths = vec![ScoredPath::new(
        vec![RichSegment {
            reading: "さんぜんえん".into(),
            surface: "産前園".into(),
            left_id: 0,
            right_id: 0,
            word_cost: 0,
        }],
        5000,
        PathOrigin::Viterbi,
    )];

    let result = rw.generate(&paths, "さんぜんえん");

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
    let paths = vec![ScoredPath::new(
        vec![RichSegment {
            reading: "ごねん".into(),
            surface: "ご年".into(),
            left_id: 1,
            right_id: 1,
            word_cost: 0,
        }],
        4000,
        PathOrigin::Viterbi,
    )];

    let mut emit_orders: Vec<Vec<String>> = Vec::new();
    for _ in 0..5 {
        let result = rw.generate(&paths, "ごねん");
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
    let paths = vec![ScoredPath::new(
        vec![RichSegment {
            reading: "ごえん".into(),
            surface: "ご縁".into(),
            left_id: 1,
            right_id: 1,
            word_cost: 0,
        }],
        4000,
        PathOrigin::Viterbi,
    )];

    // Should not panic; should still emit candidates for both counters.
    let result = rw.generate(&paths, "ごえん");
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
        run_rewriters(
            &[&rw as &dyn Rewriter],
            paths,
            "にじゅうさん",
            RewriteStage::Override,
            Some(&boost),
        )
    };

    // Learned に十三 costs 5000 (> anchor 3000), the compound is not learned:
    // に十三 keeps index 0.
    let mut paths = vec![path("に十三", 5000, 1000)];
    run(&mut paths, false);
    assert_eq!(paths[0].surface_key(), "に十三");
    assert_eq!(paths[1].surface_key(), "二十三");

    // The learned surface is the compound the rewriter generates: index 0 is
    // not repriced and keeps its boost.
    let mut paths = vec![path("二十三", 5000, 1000)];
    run(&mut paths, true);
    assert_eq!(paths[0].surface_key(), "二十三");
    assert_eq!(
        (paths[0].viterbi_cost, paths[0].whole_path_boost),
        (5000, 1000)
    );

    // Nothing learned: the compound takes index 0.
    let mut paths = vec![path("に十三", 5000, 0)];
    run(&mut paths, false);
    assert_eq!(paths[0].surface_key(), "二十三");
}
