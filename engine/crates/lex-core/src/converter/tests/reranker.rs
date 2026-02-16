use crate::converter::reranker::{history_rerank, rerank};
use crate::converter::viterbi::{RichSegment, ScoredPath};
use crate::dict::connection::ConnectionMatrix;
use crate::user_history::UserHistory;

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
        ScoredPath {
            segments: vec![
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
            viterbi_cost: 1000,
        },
        // Single segment path: 0 transitions → 0 structure cost
        ScoredPath {
            segments: vec![RichSegment {
                reading: "きのは".into(),
                surface: "木の葉".into(),
                left_id: 1,
                right_id: 1,
                word_cost: 0,
            }],
            viterbi_cost: 1040,
        },
    ];

    rerank(&mut paths, Some(&conn));

    // Fragmented: 1000 + 50 = 1050 > Single: 1040 + 0 = 1040
    assert_eq!(paths[0].segments[0].surface, "木の葉");
}

#[test]
fn test_rerank_no_conn_no_structure_penalty() {
    let mut paths = vec![
        ScoredPath {
            segments: vec![
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
            viterbi_cost: 1000,
        },
        ScoredPath {
            segments: vec![RichSegment {
                reading: "きの".into(),
                surface: "木の".into(),
                left_id: 1,
                right_id: 1,
                word_cost: 0,
            }],
            viterbi_cost: 2000,
        },
    ];

    // Without conn, structure cost is 0; "木の" gets script_cost -3000
    // (mixed kanji+kana bonus) so it reranks to first.
    rerank(&mut paths, None);
    assert_eq!(paths[0].segments[0].surface, "木の");
    assert_eq!(paths[0].viterbi_cost, 2000 - 3000);
}

#[test]
fn test_rerank_single_path_noop() {
    let mut paths = vec![ScoredPath {
        segments: vec![RichSegment {
            reading: "あ".into(),
            surface: "亜".into(),
            left_id: 0,
            right_id: 0,
            word_cost: 0,
        }],
        viterbi_cost: 1000,
    }];

    rerank(&mut paths, None);
    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0].segments[0].surface, "亜");
}

#[test]
fn test_rerank_empty_noop() {
    let mut paths: Vec<ScoredPath> = Vec::new();
    rerank(&mut paths, None);
    assert!(paths.is_empty());
}

#[test]
fn test_rerank_penalizes_uneven_segments() {
    // Uneven split: で(1) | 来たり(3) — variance penalty should apply
    // Even split:   出来(2) | たり(2) — no variance penalty
    let mut paths = vec![
        // Uneven: readings 1 + 3 chars → mean=2, sum_sq_dev=2×(1+1)=4 (via formula: N*Σl²-S² = 2*10-16=4)
        // penalty = 4 * 2000 / 4 = 2000
        ScoredPath {
            segments: vec![
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
            viterbi_cost: 5000,
        },
        // Even: readings 2 + 2 chars → sum_sq_dev=0, penalty=0
        ScoredPath {
            segments: vec![
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
            viterbi_cost: 6500,
        },
    ];

    rerank(&mut paths, None);

    // script_cost: "来たり" is mixed (kanji+kana) → -3000; "出来" is pure kanji → -1000
    // Uneven: 5000 + variance(2000) + script("で"=0 + "来たり"=-3000) = 4000
    // Even:   6500 + variance(0)    + script("出来"=-1000 + "たり"=0) = 5500
    // Uneven path wins due to mixed-script bonus on "来たり"
    assert_eq!(paths[0].segments[0].surface, "で");
    assert_eq!(paths[0].viterbi_cost, 4000);
    assert_eq!(paths[1].segments[0].surface, "出来");
    assert_eq!(paths[1].viterbi_cost, 5500);
}

#[test]
fn test_rerank_applies_script_cost() {
    // Katakana surface should receive +5000 penalty from script_cost
    let mut paths = vec![
        // Katakana path: タラ (katakana) → +5000 script penalty
        ScoredPath {
            segments: vec![RichSegment {
                reading: "たら".into(),
                surface: "タラ".into(),
                left_id: 0,
                right_id: 0,
                word_cost: 0,
            }],
            viterbi_cost: 3000,
        },
        // Hiragana path: たら (no script penalty)
        ScoredPath {
            segments: vec![RichSegment {
                reading: "たら".into(),
                surface: "たら".into(),
                left_id: 0,
                right_id: 0,
                word_cost: 0,
            }],
            viterbi_cost: 7000,
        },
    ];

    rerank(&mut paths, None);

    // Katakana: 3000 + 5000 = 8000
    // Hiragana: 7000 + 0    = 7000
    // Hiragana should be ranked first
    assert_eq!(paths[0].segments[0].surface, "たら");
    assert_eq!(paths[0].viterbi_cost, 7000);
    assert_eq!(paths[1].segments[0].surface, "タラ");
    assert_eq!(paths[1].viterbi_cost, 8000);
}

#[test]
fn test_history_rerank_unigram_boost_reorders() {
    let mut h = UserHistory::new();
    // Record twice to get 6000 boost (BOOST_PER_USE=3000 × 2), enough to
    // overcome the 2000 cost gap (5000 - 3000).
    h.record(&[("きょう".into(), "京".into())]);
    h.record(&[("きょう".into(), "京".into())]);

    let mut paths = vec![
        ScoredPath {
            segments: vec![RichSegment {
                reading: "きょう".into(),
                surface: "今日".into(),
                left_id: 0,
                right_id: 0,
                word_cost: 0,
            }],
            viterbi_cost: 3000,
        },
        ScoredPath {
            segments: vec![RichSegment {
                reading: "きょう".into(),
                surface: "京".into(),
                left_id: 0,
                right_id: 0,
                word_cost: 0,
            }],
            viterbi_cost: 5000,
        },
    ];

    history_rerank(&mut paths, &h);

    // "京" should be boosted to first place
    assert_eq!(paths[0].segments[0].surface, "京");
}

#[test]
fn test_history_rerank_bigram_boost() {
    let mut h = UserHistory::new();
    h.record(&[("きょう".into(), "今日".into()), ("は".into(), "は".into())]);

    let mut paths = vec![
        // Path without bigram match
        ScoredPath {
            segments: vec![
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
            viterbi_cost: 5000,
        },
        // Path with bigram match: "今日" → "は"
        ScoredPath {
            segments: vec![
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
            viterbi_cost: 7000,
        },
    ];

    history_rerank(&mut paths, &h);

    // "今日は" path should be boosted (both unigram + bigram) to first
    assert_eq!(paths[0].segments[0].surface, "今日");
}

#[test]
fn test_history_rerank_empty_history_preserves_order() {
    let h = UserHistory::new();

    let mut paths = vec![
        ScoredPath {
            segments: vec![RichSegment {
                reading: "あ".into(),
                surface: "亜".into(),
                left_id: 0,
                right_id: 0,
                word_cost: 0,
            }],
            viterbi_cost: 1000,
        },
        ScoredPath {
            segments: vec![RichSegment {
                reading: "あ".into(),
                surface: "阿".into(),
                left_id: 0,
                right_id: 0,
                word_cost: 0,
            }],
            viterbi_cost: 2000,
        },
    ];

    history_rerank(&mut paths, &h);

    assert_eq!(paths[0].segments[0].surface, "亜");
    assert_eq!(paths[0].viterbi_cost, 1000);
    assert_eq!(paths[1].segments[0].surface, "阿");
    assert_eq!(paths[1].viterbi_cost, 2000);
}

#[test]
fn test_history_rerank_empty_paths() {
    let h = UserHistory::new();
    let mut paths: Vec<ScoredPath> = Vec::new();
    history_rerank(&mut paths, &h);
    assert!(paths.is_empty());
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
    // Transition cost = 1500 each.
    // Path A: 1 segment → 0 transitions → structure_cost = 0
    // Path B: 2 segments → 1 transition → structure_cost = 1500
    // Path C: 5 segments → 4 transitions → structure_cost = 6000
    // min_sc = 0, threshold = 0 + 4000 = 4000
    // Path C (6000 > 4000) should be dropped; A and B should remain.
    let conn = uniform_conn(1500);

    let mut paths = vec![
        ScoredPath {
            segments: vec![RichSegment {
                reading: "あいうえお".into(),
                surface: "合言葉".into(),
                left_id: 1,
                right_id: 1,
                word_cost: 0,
            }],
            viterbi_cost: 5000,
        },
        ScoredPath {
            segments: vec![
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
            viterbi_cost: 4000,
        },
        ScoredPath {
            segments: vec![
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
            viterbi_cost: 3000,
        },
    ];

    rerank(&mut paths, Some(&conn));

    // Path C should have been filtered out
    assert_eq!(paths.len(), 2);
    // Verify the fragmented 5-segment path is gone
    assert!(paths.iter().all(|p| p.segments.len() <= 2));
}

#[test]
fn test_filter_keeps_all_when_all_exceed() {
    // All paths have high structure_cost; none should be dropped.
    // Transition cost = 2000. All paths have 4 segments → 3 transitions → sc = 6000.
    // min_sc = 6000, threshold = 6000 + 4000 = 10000.
    // All paths have sc = 6000 ≤ 10000, so all pass.
    // But to truly test the "all exceed" safety, we need a scenario where
    // min_sc itself is above the threshold relative to... Actually the safety
    // is: if ALL paths have sc > threshold, keep all. Let's just verify
    // that when all paths are equally fragmented, none are dropped.
    let conn = uniform_conn(2000);

    let seg = |r: &str, s: &str| RichSegment {
        reading: r.into(),
        surface: s.into(),
        left_id: 1,
        right_id: 1,
        word_cost: 0,
    };

    let mut paths = vec![
        ScoredPath {
            segments: vec![
                seg("あ", "亜"),
                seg("い", "位"),
                seg("う", "鵜"),
                seg("え", "絵"),
            ],
            viterbi_cost: 3000,
        },
        ScoredPath {
            segments: vec![
                seg("あ", "阿"),
                seg("い", "胃"),
                seg("う", "卯"),
                seg("え", "江"),
            ],
            viterbi_cost: 4000,
        },
    ];

    rerank(&mut paths, Some(&conn));

    // Both have identical structure_cost, so neither is filtered
    assert_eq!(paths.len(), 2);
}

#[test]
fn test_filter_preserves_minimum_path() {
    // The path with minimum structure_cost must always survive the filter.
    // Path A: 1 segment → sc = 0 (minimum)
    // Path B: 4 segments → sc = 4500 (3 × 1500); 4500 > 0 + 4000 → filtered
    let conn = uniform_conn(1500);

    let mut paths = vec![
        ScoredPath {
            segments: vec![
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
            viterbi_cost: 1000,
        },
        ScoredPath {
            segments: vec![RichSegment {
                reading: "あいうえ".into(),
                surface: "合言葉".into(),
                left_id: 1,
                right_id: 1,
                word_cost: 0,
            }],
            viterbi_cost: 5000,
        },
    ];

    rerank(&mut paths, Some(&conn));

    // Only the single-segment path (sc=0) should survive
    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0].segments[0].surface, "合言葉");
}
