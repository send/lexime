use crate::converter::reranker::{history_rerank_at, rerank};
use crate::converter::viterbi::{PathOrigin, RichSegment, ScoredPath};
use crate::dict::connection::ConnectionMatrix;
use crate::user_history::{now_epoch, UserHistory};

#[test]
fn test_rerank_penalizes_fragmented_path() {
    // Build a connection matrix where transitions cost 100 each
    let num_ids = 3;
    let mut text = format!("{num_ids} {num_ids}\n");
    for _ in 0..(num_ids * num_ids) {
        text.push_str("100\n");
    }
    let conn = ConnectionMatrix::from_text(&text).unwrap();

    let mut paths = vec![
        // Fragmented path: 3 segments → 2 transitions × 100 = 200 structure cost
        // Penalty: 200 / 4 = 50
        ScoredPath::new(
            vec![
                RichSegment {
                    reading: "き".into(),
                    surface: "木".into(),
                    left_id: 1,
                    right_id: 1,
                    word_cost: 0,
                },
                RichSegment {
                    reading: "の".into(),
                    surface: "の".into(),
                    left_id: 1,
                    right_id: 1,
                    word_cost: 0,
                },
                RichSegment {
                    reading: "は".into(),
                    surface: "葉".into(),
                    left_id: 1,
                    right_id: 1,
                    word_cost: 0,
                },
            ],
            1000,
            PathOrigin::Viterbi,
        ),
        // Single segment path: 0 transitions → 0 structure cost
        ScoredPath::new(
            vec![RichSegment {
                reading: "きのは".into(),
                surface: "木の葉".into(),
                left_id: 1,
                right_id: 1,
                word_cost: 0,
            }],
            1040,
            PathOrigin::Viterbi,
        ),
    ];

    rerank(&mut paths, Some(&conn), None, |_, _, _| {});

    // Fragmented: 1000 + 50 = 1050 > Single: 1040 + 0 = 1040
    assert_eq!(paths[0].segments[0].surface, "木の葉");
}

#[test]
fn test_rerank_no_conn_no_structure_penalty() {
    let mut paths = vec![
        ScoredPath::new(
            vec![
                RichSegment {
                    reading: "き".into(),
                    surface: "木".into(),
                    left_id: 1,
                    right_id: 1,
                    word_cost: 0,
                },
                RichSegment {
                    reading: "の".into(),
                    surface: "の".into(),
                    left_id: 1,
                    right_id: 1,
                    word_cost: 0,
                },
            ],
            1000,
            PathOrigin::Viterbi,
        ),
        ScoredPath::new(
            vec![RichSegment {
                reading: "きの".into(),
                surface: "木の".into(),
                left_id: 1,
                right_id: 1,
                word_cost: 0,
            }],
            2000,
            PathOrigin::Viterbi,
        ),
    ];

    // Without conn, structure cost is 0; "木の" (reading "きの" = 2 chars)
    // gets script_cost -3000 * 2/3 = -2000 (mixed kanji+kana bonus scaled).
    rerank(&mut paths, None, None, |_, _, _| {});
    assert_eq!(paths[0].segments[0].surface, "木の");
    assert_eq!(paths[0].viterbi_cost, 2000 - 2000);
}

#[test]
fn test_rerank_single_path_noop() {
    let mut paths = vec![ScoredPath::new(
        vec![RichSegment {
            reading: "あ".into(),
            surface: "亜".into(),
            left_id: 0,
            right_id: 0,
            word_cost: 0,
        }],
        1000,
        PathOrigin::Viterbi,
    )];

    rerank(&mut paths, None, None, |_, _, _| {});
    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0].segments[0].surface, "亜");
}

#[test]
fn test_rerank_empty_noop() {
    let mut paths: Vec<ScoredPath> = Vec::new();
    rerank(&mut paths, None, None, |_, _, _| {});
    assert!(paths.is_empty());
}

#[test]
fn test_rerank_penalizes_uneven_segments() {
    // 2-segment paths are exempt from length variance penalty (n >= 3 threshold).
    // Only script cost differentiates them.
    let mut paths = vec![
        // Uneven: readings 1 + 3 chars — no variance penalty (2-segment exempt)
        ScoredPath::new(
            vec![
                RichSegment {
                    reading: "で".into(),
                    surface: "で".into(),
                    left_id: 0,
                    right_id: 0,
                    word_cost: 0,
                },
                RichSegment {
                    reading: "きたり".into(),
                    surface: "来たり".into(),
                    left_id: 0,
                    right_id: 0,
                    word_cost: 0,
                },
            ],
            5000,
            PathOrigin::Viterbi,
        ),
        // Even: readings 2 + 2 chars → sum_sq_dev=0, penalty=0
        ScoredPath::new(
            vec![
                RichSegment {
                    reading: "でき".into(),
                    surface: "出来".into(),
                    left_id: 0,
                    right_id: 0,
                    word_cost: 0,
                },
                RichSegment {
                    reading: "たり".into(),
                    surface: "たり".into(),
                    left_id: 0,
                    right_id: 0,
                    word_cost: 0,
                },
            ],
            6500,
            PathOrigin::Viterbi,
        ),
    ];

    rerank(&mut paths, None, None, |_, _, _| {});

    // script_cost (scaled by reading length, capped at 2):
    //   "来たり" (reading "きたり" = 3 chars, cap 2) → mixed bonus -3000 * 2/3 = -2000
    //   "出来" (reading "でき" = 2 chars) → pure_kanji bonus -1000 * 2/3 = -666
    // Uneven: 5000 + script("で"=0 + "来たり"=-2000) = 3000
    // Even:   6500 + script("出来"=-666 + "たり"=0) = 5834
    // Uneven path wins due to mixed-script bonus on "来たり"
    assert_eq!(paths[0].segments[0].surface, "で");
    assert_eq!(paths[0].viterbi_cost, 3000);
    assert_eq!(paths[1].segments[0].surface, "出来");
    assert_eq!(paths[1].viterbi_cost, 5834);
}

#[test]
fn test_rerank_applies_script_cost() {
    // All-katakana surfaces receive the katakana_penalty (default 150) from
    // script_cost. Since #261 it is a tie-breaker: it decides near-ties in
    // favor of hiragana but must not override a clear dictionary-cost lead.
    let mut paths = vec![
        // Katakana path: タラ (katakana) → +150 script penalty
        ScoredPath::new(
            vec![RichSegment {
                reading: "たら".into(),
                surface: "タラ".into(),
                left_id: 0,
                right_id: 0,
                word_cost: 0,
            }],
            3000,
            PathOrigin::Viterbi,
        ),
        // Hiragana path: たら (no script penalty), raw cost 100 higher
        ScoredPath::new(
            vec![RichSegment {
                reading: "たら".into(),
                surface: "たら".into(),
                left_id: 0,
                right_id: 0,
                word_cost: 0,
            }],
            3100,
            PathOrigin::Viterbi,
        ),
    ];

    rerank(&mut paths, None, None, |_, _, _| {});

    // Katakana: 3000 + 150 = 3150
    // Hiragana: 3100 + 0   = 3100
    // Hiragana wins the near-tie
    assert_eq!(paths[0].segments[0].surface, "たら");
    assert_eq!(paths[0].viterbi_cost, 3100);
    assert_eq!(paths[1].segments[0].surface, "タラ");
    assert_eq!(paths[1].viterbi_cost, 3150);
}

/// The boost's magnitude decides the order where nothing is learned as a
/// whole: per-segment unigrams only, so the PR-G rotate never fires and
/// two records lift a path past a rival that one record leaves behind.
#[test]
fn test_history_rerank_unigram_boost_reorders() {
    let seg = |r: &str, s: &str| RichSegment {
        reading: r.into(),
        surface: s.into(),
        left_id: 0,
        right_id: 0,
        word_cost: 0,
    };
    let fixture = || {
        vec![
            ScoredPath::new(
                vec![seg("がっこう", "楽考"), seg("へ", "辺")],
                3000,
                PathOrigin::Viterbi,
            ),
            ScoredPath::new(
                vec![seg("がっこう", "学校"), seg("へ", "辺")],
                5000,
                PathOrigin::Viterbi,
            ),
        ]
    };
    let now = now_epoch();
    let rerank_after = |records: usize| {
        let mut h = UserHistory::new();
        for _ in 0..records {
            h.record_at(&[("がっこう".into(), "学校".into())], now);
        }
        let mut paths = fixture();
        history_rerank_at(&mut paths, &h, None, now);
        paths
    };

    // One record: BOOST_PER_USE (3000) over two segments = 1500, which
    // leaves 学校 at 3500, above 3000.
    let once = rerank_after(1);
    assert!(once.iter().all(|p| p.whole_path_boost == 0));
    assert_eq!(once[0].segments[0].surface, "楽考");
    let cost_of = |paths: &[ScoredPath], s: &str| {
        paths
            .iter()
            .find(|p| p.segments[0].surface == s)
            .unwrap()
            .viterbi_cost
    };
    assert_eq!(cost_of(&once, "学校"), 5000 - 1500);

    // Two records: 6000 over two segments = 3000, so 学校 (2000) passes.
    let twice = rerank_after(2);
    assert!(twice.iter().all(|p| p.whole_path_boost == 0));
    assert_eq!(cost_of(&twice, "学校"), 5000 - 3000);
    assert_eq!(cost_of(&twice, "楽考"), 3000, "unrecorded path untouched");
    assert_eq!(twice[0].segments[0].surface, "学校");
}

#[test]
fn test_history_rerank_bigram_boost() {
    let mut h = UserHistory::new();
    h.record(&[("きょう".into(), "今日".into()), ("は".into(), "は".into())]);

    let mut paths = vec![
        // Path without bigram match
        ScoredPath::new(
            vec![
                RichSegment {
                    reading: "きょう".into(),
                    surface: "京".into(),
                    left_id: 0,
                    right_id: 0,
                    word_cost: 0,
                },
                RichSegment {
                    reading: "は".into(),
                    surface: "は".into(),
                    left_id: 0,
                    right_id: 0,
                    word_cost: 0,
                },
            ],
            5000,
            PathOrigin::Viterbi,
        ),
        // Path with bigram match: "今日" → "は"
        ScoredPath::new(
            vec![
                RichSegment {
                    reading: "きょう".into(),
                    surface: "今日".into(),
                    left_id: 0,
                    right_id: 0,
                    word_cost: 0,
                },
                RichSegment {
                    reading: "は".into(),
                    surface: "は".into(),
                    left_id: 0,
                    right_id: 0,
                    word_cost: 0,
                },
            ],
            7000,
            PathOrigin::Viterbi,
        ),
    ];

    history_rerank_at(&mut paths, &h, None, now_epoch());

    // "今日は" path should be boosted (both unigram + bigram) to first
    assert_eq!(paths[0].segments[0].surface, "今日");
}

#[test]
fn test_history_rerank_empty_history_preserves_order() {
    let h = UserHistory::new();

    let mut paths = vec![
        ScoredPath::new(
            vec![RichSegment {
                reading: "あ".into(),
                surface: "亜".into(),
                left_id: 0,
                right_id: 0,
                word_cost: 0,
            }],
            1000,
            PathOrigin::Viterbi,
        ),
        ScoredPath::new(
            vec![RichSegment {
                reading: "あ".into(),
                surface: "阿".into(),
                left_id: 0,
                right_id: 0,
                word_cost: 0,
            }],
            2000,
            PathOrigin::Viterbi,
        ),
    ];

    history_rerank_at(&mut paths, &h, None, now_epoch());

    assert_eq!(paths[0].segments[0].surface, "亜");
    assert_eq!(paths[0].viterbi_cost, 1000);
    assert_eq!(paths[1].segments[0].surface, "阿");
    assert_eq!(paths[1].viterbi_cost, 2000);
}

#[test]
fn test_history_rerank_empty_paths() {
    let h = UserHistory::new();
    let mut paths: Vec<ScoredPath> = Vec::new();
    history_rerank_at(&mut paths, &h, None, now_epoch());
    assert!(paths.is_empty());
}

#[test]
fn test_history_rerank_at_matches_compute_history_boost() {
    // Contract: `history_rerank_at` must subtract exactly the value reported
    // by `compute_history_boost(...).applied(seg_count)` when given the same
    // `now`. The `explain` observer relies on this so its precomputed
    // breakdown matches what the pipeline actually applied. Regression for
    // PR #247 R3.
    use crate::converter::reranker::compute_history_boost;

    let mut h = UserHistory::new();
    h.record(&[("きょう".into(), "京".into())]);
    let now = 1_700_000_000;

    let path_before = ScoredPath::new(
        vec![RichSegment {
            reading: "きょう".into(),
            surface: "京".into(),
            left_id: 0,
            right_id: 0,
            word_cost: 0,
        }],
        10_000,
        PathOrigin::Viterbi,
    );
    let expected_applied =
        compute_history_boost(&path_before, &h, None, now).applied(path_before.segments.len());

    let initial_cost = path_before.viterbi_cost;
    let mut paths = vec![path_before];
    history_rerank_at(&mut paths, &h, None, now);
    let actual_applied = initial_cost - paths[0].viterbi_cost;

    assert_eq!(actual_applied, expected_applied);
    // The applied boost must also be stored on the path so that candidate
    // generators running after history_rerank can recover the pre-boost cost
    // via `pre_history_cost()`. Locks the `history_boost` field contract.
    assert_eq!(paths[0].history_boost, expected_applied);
}

#[test]
fn test_compute_history_boost_skips_function_word_unigram() {
    // Per-segment unigram boost must NOT count function-word segments
    // (particles). A particle like に is confirmed in nearly every sentence, so
    // its unigram boost saturates and would inflate any fragmented
    // mis-segmentation that isolates it (e.g. 代/に/段 for だいにだん), burying
    // the correct compound. Regression for だいにだん → 第二弾.
    use crate::converter::reranker::compute_history_boost;

    // POS IDs: 1 = content word, 2 = function word (fw_min=fw_max=2).
    let text = format!("3 3\n{}", "0\n".repeat(9));
    let conn = ConnectionMatrix::from_text_with_roles(&text, 2, 2, vec![0, 0, 0]).unwrap();
    assert!(conn.is_function_word(2));
    assert!(!conn.is_function_word(1));

    let mut h = UserHistory::new();
    h.record(&[("だい".into(), "代".into())]);
    for _ in 0..5 {
        h.record(&[("に".into(), "に".into())]);
    }
    let now = now_epoch();

    let path = ScoredPath::new(
        vec![
            RichSegment {
                reading: "だい".into(),
                surface: "代".into(),
                left_id: 1,
                right_id: 1,
                word_cost: 0,
            },
            RichSegment {
                reading: "に".into(),
                surface: "に".into(),
                left_id: 2,
                right_id: 2,
                word_cost: 0,
            },
        ],
        0,
        PathOrigin::Viterbi,
    );

    let content_boost = h.unigram_boost("だい", "代", now);
    let particle_boost = h.unigram_boost("に", "に", now);
    assert!(particle_boost > 0, "precondition: particle is boosted");

    // Without conn: both content word and particle contribute.
    let without = compute_history_boost(&path, &h, None, now);
    assert_eq!(without.unigram_sum, content_boost + particle_boost);

    // With conn: the function-word particle is excluded from per-segment boost.
    let with = compute_history_boost(&path, &h, Some(&conn), now);
    assert_eq!(with.unigram_sum, content_boost);
}

/// Build a connection matrix where all transitions cost the given value.
fn uniform_conn(cost: i16) -> ConnectionMatrix {
    let num_ids = 4;
    let mut text = format!("{num_ids} {num_ids}\n");
    for _ in 0..(num_ids * num_ids) {
        text.push_str(&format!("{cost}\n"));
    }
    ConnectionMatrix::from_text(&text).unwrap()
}

#[test]
fn test_filter_drops_fragmented_paths() {
    // Transition cost = 5000 each.
    // Path A: 1 segment → sc = 0
    // Path B: 2 segments → sc = 5000, the cheapest: the anchor
    // Path C: 5 segments → sc = 20000
    // threshold = 5000 + 6000 = 11000.
    // Path C (20000 > 11000) should be dropped; A and B survive.
    let conn = uniform_conn(5000);

    let mut paths = vec![
        ScoredPath::new(
            vec![RichSegment {
                reading: "あいうえお".into(),
                surface: "合言葉".into(),
                left_id: 1,
                right_id: 1,
                word_cost: 0,
            }],
            5000,
            PathOrigin::Viterbi,
        ),
        ScoredPath::new(
            vec![
                RichSegment {
                    reading: "あい".into(),
                    surface: "愛".into(),
                    left_id: 1,
                    right_id: 1,
                    word_cost: 0,
                },
                RichSegment {
                    reading: "うえお".into(),
                    surface: "上尾".into(),
                    left_id: 1,
                    right_id: 1,
                    word_cost: 0,
                },
            ],
            4000,
            PathOrigin::Viterbi,
        ),
        ScoredPath::new(
            vec![
                RichSegment {
                    reading: "あ".into(),
                    surface: "亜".into(),
                    left_id: 1,
                    right_id: 1,
                    word_cost: 0,
                },
                RichSegment {
                    reading: "い".into(),
                    surface: "位".into(),
                    left_id: 1,
                    right_id: 1,
                    word_cost: 0,
                },
                RichSegment {
                    reading: "う".into(),
                    surface: "鵜".into(),
                    left_id: 1,
                    right_id: 1,
                    word_cost: 0,
                },
                RichSegment {
                    reading: "え".into(),
                    surface: "絵".into(),
                    left_id: 1,
                    right_id: 1,
                    word_cost: 0,
                },
                RichSegment {
                    reading: "お".into(),
                    surface: "尾".into(),
                    left_id: 1,
                    right_id: 1,
                    word_cost: 0,
                },
            ],
            9000,
            PathOrigin::Viterbi,
        ),
    ];

    rerank(&mut paths, Some(&conn), None, |_, _, _| {});

    // Path C should have been filtered out (sc=20000 > threshold=11000);
    // paths A and B survive.
    assert_eq!(paths.len(), 2);
    assert!(paths.iter().all(|p| p.segments.len() <= 2));
}

#[test]
fn test_filter_keeps_equally_fragmented_paths() {
    // A high structure cost alone drops nothing: the threshold is measured
    // from the anchor. Transition cost = 2000, 4 segments → sc = 6000 for
    // every path; anchor sc 6000 → threshold 12000, so all pass.
    let conn = uniform_conn(2000);

    let seg = |r: &str, s: &str| RichSegment {
        reading: r.into(),
        surface: s.into(),
        left_id: 1,
        right_id: 1,
        word_cost: 0,
    };

    let mut paths = vec![
        ScoredPath::new(
            vec![
                seg("あ", "亜"),
                seg("い", "位"),
                seg("う", "鵜"),
                seg("え", "絵"),
            ],
            3000,
            PathOrigin::Viterbi,
        ),
        ScoredPath::new(
            vec![
                seg("あ", "阿"),
                seg("い", "胃"),
                seg("う", "卯"),
                seg("え", "江"),
            ],
            4000,
            PathOrigin::Viterbi,
        ),
    ];

    rerank(&mut paths, Some(&conn), None, |_, _, _| {});

    // Both have identical structure_cost, so neither is filtered
    assert_eq!(paths.len(), 2);
}

#[test]
fn test_filter_preserves_the_best() {
    // The cheapest path is the anchor and always survives, however
    // fragmented (#353: a population-min threshold dropped it).
    // Path A: 4 segments → sc = 15000, the cheapest: the anchor
    // Path B: 1 segment → sc = 0 (imputed to 3000)
    // threshold = 15000 + 6000 = 21000: both survive, A first. (Measured
    // from B, the threshold would be 9000 and drop A.)
    let conn = uniform_conn(5000);

    let mut paths = vec![
        ScoredPath::new(
            vec![
                RichSegment {
                    reading: "あ".into(),
                    surface: "亜".into(),
                    left_id: 1,
                    right_id: 1,
                    word_cost: 0,
                },
                RichSegment {
                    reading: "い".into(),
                    surface: "位".into(),
                    left_id: 1,
                    right_id: 1,
                    word_cost: 0,
                },
                RichSegment {
                    reading: "う".into(),
                    surface: "鵜".into(),
                    left_id: 1,
                    right_id: 1,
                    word_cost: 0,
                },
                RichSegment {
                    reading: "え".into(),
                    surface: "絵".into(),
                    left_id: 1,
                    right_id: 1,
                    word_cost: 0,
                },
            ],
            1000,
            PathOrigin::Viterbi,
        ),
        ScoredPath::new(
            vec![RichSegment {
                reading: "あいうえ".into(),
                surface: "合言葉".into(),
                left_id: 1,
                right_id: 1,
                word_cost: 0,
            }],
            5000,
            PathOrigin::Viterbi,
        ),
    ];

    rerank(&mut paths, Some(&conn), None, |_, _, _| {});

    assert_eq!(paths.len(), 2);
    assert_eq!(paths[0].segments.len(), 4, "the best stays at index 0");
}

#[test]
fn test_prefix_floor_prevents_low_baseline() {
    // Verifies that the prefix floor raises the anchor's sc (Path A, the
    // cheapest) enough to keep a path that would be dropped without it.
    //
    // Setup: 4 POS IDs, ID 0 is prefix (role=3).
    // Connection costs: all 4000, except (0→any) = 100.
    // prefix_floor = 6000 / 2 = 3000.
    //
    // Path A: [prefix(id=0)] → [content(id=1)]  (1 transition)
    //   Without floor: sc = 100
    //   With floor:    sc = 3000
    //
    // Path B: [content(id=1)] → [content(id=1)] → [content(id=1)]  (2 transitions)
    //   sc = 4000 + 4000 = 8000
    //
    // Without floor: anchor sc = 100,  threshold = 100 + 6000 = 6100.
    //   Path B (8000 > 6100) → DROPPED.
    //
    // With floor: anchor sc = 3000, threshold = 3000 + 6000 = 9000.
    //   Path B (8000 ≤ 9000) → KEPT.
    let num_ids = 4u16;
    let mut costs = Vec::new();
    for left in 0..num_ids {
        for _right in 0..num_ids {
            costs.push(if left == 0 { 100i16 } else { 4000 });
        }
    }
    let mut text = format!("{num_ids} {num_ids}\n");
    for c in &costs {
        text.push_str(&format!("{c}\n"));
    }
    // ID 0 = prefix (role 3), IDs 1-3 = content (role 0)
    let roles = vec![3u8, 0, 0, 0];
    let conn = ConnectionMatrix::from_text_with_roles(&text, 0, num_ids - 1, roles).unwrap();

    assert!(conn.is_prefix(0));
    assert!(!conn.is_prefix(1));

    let mut paths = vec![
        // Path A: prefix → content (low prefix transition, floored to 3000)
        ScoredPath::new(
            vec![
                RichSegment {
                    reading: "お".into(),
                    surface: "御".into(),
                    left_id: 0,
                    right_id: 0,
                    word_cost: 0,
                },
                RichSegment {
                    reading: "くるま".into(),
                    surface: "車".into(),
                    left_id: 1,
                    right_id: 1,
                    word_cost: 0,
                },
            ],
            3000,
            PathOrigin::Viterbi,
        ),
        // Path B: content → content → content (sc = 8000)
        // Without floor this would be dropped (8000 > 6100).
        // With floor it survives (8000 ≤ 9000).
        ScoredPath::new(
            vec![
                RichSegment {
                    reading: "おくる".into(),
                    surface: "送る".into(),
                    left_id: 1,
                    right_id: 1,
                    word_cost: 0,
                },
                RichSegment {
                    reading: "ま".into(),
                    surface: "間".into(),
                    left_id: 1,
                    right_id: 1,
                    word_cost: 0,
                },
                RichSegment {
                    reading: "で".into(),
                    surface: "で".into(),
                    left_id: 1,
                    right_id: 1,
                    word_cost: 0,
                },
            ],
            4000,
            PathOrigin::Viterbi,
        ),
    ];

    rerank(&mut paths, Some(&conn), None, |_, _, _| {});

    // Both paths survive thanks to the prefix floor raising the threshold.
    assert_eq!(paths.len(), 2);
}

/// A one-segment path `surface` over the reading かな, priced `cost`.
fn kana_path(surface: &str, cost: i64) -> ScoredPath {
    ScoredPath::single("かな".into(), surface.into(), cost, PathOrigin::Viterbi)
}

fn learned(surfaces: &[&str]) -> UserHistory {
    let mut h = UserHistory::new();
    for s in surfaces {
        h.record(&[("かな".into(), (*s).into())]);
    }
    h
}

/// PR-G: the cheapest learned path takes index 0 even when its boost does
/// not cover its price gap; the rest keep their price order.
#[test]
fn history_puts_cheapest_learned_first() {
    let h = learned(&["可奈"]);
    // 可奈 is 40000 above 仮名: far more than a fresh whole-path boost.
    let mut paths = vec![
        kana_path("仮名", 1000),
        kana_path("加奈", 2000),
        kana_path("可奈", 41000),
    ];
    history_rerank_at(&mut paths, &h, None, now_epoch());
    let order: Vec<String> = paths.iter().map(|p| p.surface_key()).collect();
    assert_eq!(order, ["可奈", "仮名", "加奈"]);
    assert!(
        paths[0].viterbi_cost > paths[1].viterbi_cost,
        "moved, not priced below"
    );
}

/// Among learned paths price decides, decay included.
#[test]
fn learned_paths_order_by_price() {
    let now = now_epoch();
    let mut h = UserHistory::new();
    h.record_at(&[("かな".into(), "加奈".into())], now);
    // 可奈 learned long ago: its boost has decayed.
    h.record_at(&[("かな".into(), "可奈".into())], now - 3600 * 24 * 365);
    let mut paths = vec![
        kana_path("仮名", 1000),
        kana_path("加奈", 30000),
        kana_path("可奈", 30000),
    ];
    history_rerank_at(&mut paths, &h, None, now);
    assert_eq!(paths[0].surface_key(), "加奈");
}

/// Among learned paths the price decides, not the boost: A has the larger
/// boost (two records) but B is cheaper after its own (one record).
#[test]
fn learned_paths_order_by_price_not_by_boost() {
    let now = now_epoch();
    let mut h = UserHistory::new();
    h.record_at(&[("かな".into(), "可奈".into())], now);
    h.record_at(&[("かな".into(), "可奈".into())], now);
    h.record_at(&[("かな".into(), "加奈".into())], now);
    let mut paths = vec![
        kana_path("仮名", 1000),
        kana_path("可奈", 60000),
        kana_path("加奈", 30000),
    ];
    history_rerank_at(&mut paths, &h, None, now);
    let (a, b) = (
        paths.iter().find(|p| p.surface_key() == "可奈").unwrap(),
        paths.iter().find(|p| p.surface_key() == "加奈").unwrap(),
    );
    assert!(
        a.whole_path_boost > b.whole_path_boost,
        "fixture: A boosts more"
    );
    assert!(b.viterbi_cost < a.viterbi_cost, "fixture: B ends cheaper");
    assert_eq!(paths[0].surface_key(), "加奈");
    assert_eq!(paths[1].surface_key(), "仮名");
    assert_eq!(paths[2].surface_key(), "可奈");
}

/// Without a whole-path boost (per-segment learning only), the order is
/// the price order.
#[test]
fn no_learned_keeps_price_order() {
    let mut h = UserHistory::new();
    // A segment of a two-segment path, never the whole reading.
    h.record(&[("か".into(), "可".into())]);
    let seg = |r: &str, s: &str| RichSegment {
        reading: r.into(),
        surface: s.into(),
        left_id: 0,
        right_id: 0,
        word_cost: 0,
    };
    let mut paths = vec![
        kana_path("仮名", 1000),
        ScoredPath::new(
            vec![seg("か", "可"), seg("な", "名")],
            50000,
            PathOrigin::Viterbi,
        ),
    ];
    history_rerank_at(&mut paths, &h, None, now_epoch());
    assert!(paths.iter().all(|p| p.whole_path_boost == 0));
    assert!(paths
        .windows(2)
        .all(|w| w[0].viterbi_cost <= w[1].viterbi_cost));
    assert_eq!(paths[0].surface_key(), "仮名");
}

/// Paths over ids 1..=4: 1→2 and 2→2 cost 4000, 3→4 costs 0 (a
/// `カナ|や`-shaped path: cheap transitions, expensive words).
fn filter_conn() -> ConnectionMatrix {
    let mut costs = vec![0i16; 25];
    costs[5 + 2] = 4000; // 1 → 2
    costs[2 * 5 + 2] = 4000; // 2 → 2
    ConnectionMatrix::new_owned(5, 0, 0, Vec::new(), costs)
}

/// A path of one-char kanji segments over `ids`, priced `cost`.
fn kanji_path(surfaces: &str, ids: &[u16], cost: i64) -> ScoredPath {
    let readings = ["あ", "い", "う", "え"];
    let segs = surfaces
        .chars()
        .zip(ids)
        .enumerate()
        .map(|(i, (c, &id))| RichSegment {
            reading: readings[i].into(),
            surface: c.to_string(),
            left_id: id,
            right_id: id,
            word_cost: 0,
        })
        .collect();
    ScoredPath::new(segs, cost, PathOrigin::Viterbi)
}

fn surfaces(paths: &[ScoredPath]) -> Vec<String> {
    paths.iter().map(|p| p.surface_key()).collect()
}

/// #353: the threshold depends on the population only through its #1. A
/// cheap-transition path G changes nothing while it does not win; once it
/// is the #1, the threshold is measured from it.
#[test]
fn threshold_depends_on_the_population_only_through_the_best() {
    let conn = filter_conn();
    // sc: 亜位宇 8000 (the #1), 阿伊 4000, 吾以卯江 12000 → threshold 14000.
    let p = || {
        vec![
            kanji_path("亜位宇", &[1, 2, 2], 1000),
            kanji_path("阿伊", &[1, 2], 2000),
            kanji_path("吾以卯江", &[1, 2, 2, 2], 3000),
        ]
    };
    let mut alone = p();
    rerank(&mut alone, Some(&conn), None, |_, _, _| {});
    assert_eq!(alone.len(), 3);

    // (a) G (sc 0) never wins: P's survivors, prices and order are
    // unchanged. (A population minimum would put the threshold at 6000 and
    // drop 亜位宇 and 吾以卯江.)
    let mut with_g = p();
    with_g.push(kanji_path("蚊名", &[3, 4], 5000));
    rerank(&mut with_g, Some(&conn), None, |_, _, _| {});
    let p_part: Vec<_> = with_g
        .iter()
        .filter(|q| q.surface_key() != "蚊名")
        .collect();
    assert_eq!(
        p_part
            .iter()
            .map(|q| (q.surface_key(), q.viterbi_cost))
            .collect::<Vec<_>>(),
        alone
            .iter()
            .map(|q| (q.surface_key(), q.viterbi_cost))
            .collect::<Vec<_>>(),
    );

    // (b) G wins: the threshold is 0 + 6000, so 亜位宇 and 吾以卯江 go.
    let mut g_best = p();
    g_best.push(kanji_path("蚊名", &[3, 4], 500));
    let mut dropped = Vec::new();
    rerank(&mut g_best, Some(&conn), None, |q, sc, _| {
        dropped.push((q.surface_key(), sc))
    });
    assert_eq!(surfaces(&g_best), ["蚊名", "阿伊"]);
    assert_eq!(
        dropped,
        [
            ("亜位宇".to_string(), 8000),
            ("吾以卯江".to_string(), 12000)
        ]
    );
}

/// The #1 survives however fragmented, identity or not; everything is
/// measured from it.
#[test]
fn filter_never_drops_the_best() {
    let conn = filter_conn();
    // The #1 (sc 12000) is far above the population minimum (蚊名, 0).
    let mut paths = vec![
        kanji_path("吾以卯江", &[1, 2, 2, 2], 1000),
        kanji_path("蚊名", &[3, 4], 6000),
    ];
    rerank(&mut paths, Some(&conn), None, |_, _, _| {});
    assert_eq!(surfaces(&paths), ["吾以卯江", "蚊名"]);

    // A multi-segment identity #1 anchors too: threshold = its sc + 6000.
    let identity = |ids: &[u16], cost: i64| {
        let mut p = kanji_path("あいうえ", ids, cost);
        for s in &mut p.segments {
            s.surface = s.reading.clone();
        }
        p
    };
    let mut paths = vec![
        identity(&[1, 2, 2, 2], 1000),          // sc 12000 → threshold 18000
        kanji_path("阿伊宇", &[1, 2, 2], 2000), // sc 8000, kept
        kanji_path("蚊名", &[3, 4], 3000),      // sc 0, kept
    ];
    let mut dropped = 0;
    rerank(&mut paths, Some(&conn), None, |_, _, _| dropped += 1);
    assert_eq!(paths[0].surface_key(), "あいうえ");
    assert_eq!((paths.len(), dropped), (3, 0));
}
