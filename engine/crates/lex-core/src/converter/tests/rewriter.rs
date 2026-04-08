use crate::converter::lattice::Lattice;
use crate::converter::rewriter::{
    run_rewriters, HiraganaVariantRewriter, KanjiVariantRewriter, KatakanaRewriter,
    NumericRewriter, PartialHiraganaRewriter, Rewriter,
};
use crate::converter::viterbi::{RichSegment, ScoredPath};

#[test]
fn test_katakana_rewriter_generates_candidate() {
    let rw = KatakanaRewriter;
    let paths = vec![ScoredPath {
        segments: vec![RichSegment {
            reading: "きょう".into(),
            surface: "今日".into(),
            left_id: 10,
            right_id: 10,
            word_cost: 0,
        }],
        viterbi_cost: 3000,
    }];

    let result = rw.generate(&paths, "きょう");

    assert_eq!(result.len(), 1);
    assert_eq!(result[0].surface_key(), "キョウ");
    assert_eq!(result[0].viterbi_cost, 3000 + 10000);
}

#[test]
fn test_katakana_dedup_via_run_rewriters() {
    let rw = KatakanaRewriter;
    let mut paths = vec![ScoredPath {
        segments: vec![RichSegment {
            reading: "きょう".into(),
            surface: "キョウ".into(),
            left_id: 0,
            right_id: 0,
            word_cost: 0,
        }],
        viterbi_cost: 5000,
    }];

    run_rewriters(&[&rw], &mut paths, "きょう");

    assert_eq!(
        paths.len(),
        1,
        "should not add duplicate katakana candidate"
    );
}

#[test]
fn test_katakana_rewriter_empty_paths() {
    let rw = KatakanaRewriter;
    let paths: Vec<ScoredPath> = Vec::new();

    let result = rw.generate(&paths, "てすと");

    assert_eq!(result.len(), 1);
    assert_eq!(result[0].surface_key(), "テスト");
    assert_eq!(result[0].viterbi_cost, 10000);
}

#[test]
fn test_run_rewriters_applies_all() {
    let rw = KatakanaRewriter;
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

    run_rewriters(&[&rw], &mut paths, "あ");

    assert_eq!(paths.len(), 2);
    // Katakana has higher cost, so inserted after 亜
    assert_eq!(paths[0].surface_key(), "亜");
    assert_eq!(paths[1].surface_key(), "ア");
}

#[test]
fn test_run_rewriters_dedup_across_rewriters() {
    // HiraganaVariant and PartialHiragana could produce the same surface;
    // run_rewriters should keep only the first one.
    let hiragana_rw = HiraganaVariantRewriter;
    let partial_rw = PartialHiraganaRewriter;
    let mut paths = vec![ScoredPath {
        segments: vec![
            RichSegment {
                reading: "され".into(),
                surface: "去れ".into(),
                left_id: 10,
                right_id: 10,
                word_cost: 0,
            },
            RichSegment {
                reading: "ます".into(),
                surface: "ます".into(),
                left_id: 0,
                right_id: 0,
                word_cost: 0,
            },
        ],
        viterbi_cost: 1000,
    }];

    run_rewriters(&[&hiragana_rw, &partial_rw], &mut paths, "されます");

    // Both would generate "されます", but only one copy should exist
    let count = paths
        .iter()
        .filter(|p| p.surface_key() == "されます")
        .count();
    assert_eq!(count, 1, "dedup should prevent duplicate across rewriters");
}

#[test]
fn test_run_rewriters_cost_ordered_insertion() {
    // Compound kanji (best_cost) should be inserted at position 0
    let rw = NumericRewriter;
    let mut paths = vec![ScoredPath {
        segments: vec![RichSegment {
            reading: "にじゅうさん".into(),
            surface: "に十三".into(),
            left_id: 10,
            right_id: 10,
            word_cost: 0,
        }],
        viterbi_cost: 3000,
    }];

    run_rewriters(&[&rw], &mut paths, "にじゅうさん");

    assert_eq!(paths[0].surface_key(), "二十三");
    assert_eq!(paths[0].viterbi_cost, 3000); // best_cost = 3000
    assert_eq!(paths[1].surface_key(), "に十三");
}

#[test]
fn test_numeric_rewriter_generates_candidates() {
    let rw = NumericRewriter;
    let paths = vec![ScoredPath {
        segments: vec![RichSegment {
            reading: "にじゅうさん".into(),
            surface: "に十三".into(),
            left_id: 10,
            right_id: 10,
            word_cost: 0,
        }],
        viterbi_cost: 3000,
    }];

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
    let rw = NumericRewriter;
    let mut paths = vec![ScoredPath {
        segments: vec![RichSegment {
            reading: "にじゅうさん".into(),
            surface: "二十三".into(),
            left_id: 10,
            right_id: 10,
            word_cost: 0,
        }],
        viterbi_cost: 3000,
    }];

    run_rewriters(&[&rw], &mut paths, "にじゅうさん");

    // Kanji already exists, only halfwidth + fullwidth added
    assert_eq!(paths.len(), 3);
    assert_eq!(paths[0].surface_key(), "二十三");
    assert_eq!(paths[1].surface_key(), "23");
    assert_eq!(paths[2].surface_key(), "２３");
}

#[test]
fn test_numeric_rewriter_single_char_kanji_low_priority() {
    let rw = NumericRewriter;
    let mut paths = vec![ScoredPath {
        segments: vec![RichSegment {
            reading: "じゅう".into(),
            surface: "中".into(),
            left_id: 10,
            right_id: 10,
            word_cost: 0,
        }],
        viterbi_cost: 3000,
    }];

    run_rewriters(&[&rw], &mut paths, "じゅう");

    // 十 is single-char → base_cost (not best_cost), all after 中
    assert_eq!(paths[0].surface_key(), "中");
    let kanji = paths.iter().find(|p| p.surface_key() == "十").unwrap();
    assert_eq!(kanji.viterbi_cost, 3000 + 5000);
}

#[test]
fn test_numeric_rewriter_skips_non_numeric() {
    let rw = NumericRewriter;
    let paths = vec![ScoredPath {
        segments: vec![RichSegment {
            reading: "きょう".into(),
            surface: "今日".into(),
            left_id: 0,
            right_id: 0,
            word_cost: 0,
        }],
        viterbi_cost: 1000,
    }];

    let result = rw.generate(&paths, "きょう");

    assert!(
        result.is_empty(),
        "should not generate numeric candidates for non-numeric input"
    );
}

#[test]
fn test_numeric_rewriter_skips_duplicate() {
    let rw = NumericRewriter;
    let mut paths = vec![ScoredPath {
        segments: vec![RichSegment {
            reading: "いち".into(),
            surface: "1".into(),
            left_id: 0,
            right_id: 0,
            word_cost: 0,
        }],
        viterbi_cost: 1000,
    }];

    run_rewriters(&[&rw], &mut paths, "いち");

    // Half-width "1" already exists; kanji "一" (single-char) + full-width "１" added
    assert_eq!(paths.len(), 3);
    // All have high cost, so they come after "1"
    assert_eq!(paths[0].surface_key(), "1");
    assert!(paths.iter().any(|p| p.surface_key() == "一"));
    assert!(paths.iter().any(|p| p.surface_key() == "１"));
}

#[test]
fn test_hiragana_variant_replaces_kanji() {
    let rw = HiraganaVariantRewriter;
    let paths = vec![ScoredPath {
        segments: vec![
            RichSegment {
                reading: "りだいれくと".into(),
                surface: "リダイレクト".into(),
                left_id: 10,
                right_id: 10,
                word_cost: 0,
            },
            RichSegment {
                reading: "され".into(),
                surface: "去れ".into(),
                left_id: 20,
                right_id: 20,
                word_cost: 0,
            },
            RichSegment {
                reading: "ます".into(),
                surface: "ます".into(),
                left_id: 30,
                right_id: 30,
                word_cost: 0,
            },
            RichSegment {
                reading: "か".into(),
                surface: "化".into(),
                left_id: 40,
                right_id: 40,
                word_cost: 0,
            },
        ],
        viterbi_cost: 3000,
    }];

    let result = rw.generate(&paths, "りだいれくとされますか");

    assert_eq!(result.len(), 1);
    assert_eq!(result[0].surface_key(), "リダイレクトされますか");
    assert_eq!(result[0].viterbi_cost, 3000 + 5000);
}

#[test]
fn test_hiragana_variant_skips_all_hiragana() {
    let rw = HiraganaVariantRewriter;
    let paths = vec![ScoredPath {
        segments: vec![
            RichSegment {
                reading: "され".into(),
                surface: "され".into(),
                left_id: 0,
                right_id: 0,
                word_cost: 0,
            },
            RichSegment {
                reading: "ます".into(),
                surface: "ます".into(),
                left_id: 0,
                right_id: 0,
                word_cost: 0,
            },
        ],
        viterbi_cost: 1000,
    }];

    let result = rw.generate(&paths, "されます");

    assert!(
        result.is_empty(),
        "should not add variant when all segments are already hiragana"
    );
}

#[test]
fn test_hiragana_variant_dedup_via_run_rewriters() {
    let rw = HiraganaVariantRewriter;
    let mut paths = vec![
        ScoredPath {
            segments: vec![RichSegment {
                reading: "され".into(),
                surface: "去れ".into(),
                left_id: 10,
                right_id: 10,
                word_cost: 0,
            }],
            viterbi_cost: 3000,
        },
        ScoredPath {
            segments: vec![RichSegment {
                reading: "され".into(),
                surface: "され".into(),
                left_id: 0,
                right_id: 0,
                word_cost: 0,
            }],
            viterbi_cost: 4000,
        },
    ];

    run_rewriters(&[&rw], &mut paths, "され");

    assert_eq!(paths.len(), 2, "should not add duplicate hiragana variant");
}

#[test]
fn test_hiragana_variant_keeps_katakana() {
    let rw = HiraganaVariantRewriter;
    let paths = vec![ScoredPath {
        segments: vec![
            RichSegment {
                reading: "てすと".into(),
                surface: "テスト".into(),
                left_id: 10,
                right_id: 10,
                word_cost: 0,
            },
            RichSegment {
                reading: "ちゅう".into(),
                surface: "中".into(),
                left_id: 20,
                right_id: 20,
                word_cost: 0,
            },
        ],
        viterbi_cost: 2000,
    }];

    let result = rw.generate(&paths, "てすとちゅう");

    assert_eq!(result.len(), 1);
    assert_eq!(result[0].surface_key(), "テストちゅう");
}

// ── PartialHiraganaRewriter tests ──────────────────────────────

#[test]
fn test_partial_hiragana_basic() {
    let rw = PartialHiraganaRewriter;
    let paths = vec![ScoredPath {
        segments: vec![
            RichSegment {
                reading: "した".into(),
                surface: "下".into(),
                left_id: 10,
                right_id: 10,
                word_cost: 0,
            },
            RichSegment {
                reading: "ほう".into(),
                surface: "方".into(),
                left_id: 20,
                right_id: 20,
                word_cost: 0,
            },
        ],
        viterbi_cost: 3000,
    }];

    let result = rw.generate(&paths, "したほう");

    // Should produce 2 variants: した|方 and 下|ほう
    assert_eq!(result.len(), 2);
    assert!(result.iter().any(|p| p.surface_key() == "した方"));
    assert!(result.iter().any(|p| p.surface_key() == "下ほう"));
    assert!(result.iter().all(|p| p.viterbi_cost == 5000));
}

#[test]
fn test_partial_hiragana_multiple_kanji() {
    let rw = PartialHiraganaRewriter;
    let paths = vec![ScoredPath {
        segments: vec![
            RichSegment {
                reading: "した".into(),
                surface: "舌".into(),
                left_id: 10,
                right_id: 10,
                word_cost: 0,
            },
            RichSegment {
                reading: "ほう".into(),
                surface: "法".into(),
                left_id: 20,
                right_id: 20,
                word_cost: 0,
            },
            RichSegment {
                reading: "が".into(),
                surface: "が".into(),
                left_id: 30,
                right_id: 30,
                word_cost: 0,
            },
        ],
        viterbi_cost: 1000,
    }];

    let result = rw.generate(&paths, "したほうが");

    // Two kanji segments → 2 variants: した|法|が and 舌|ほう|が
    assert_eq!(result.len(), 2);
    assert!(result.iter().any(|p| p.surface_key() == "した法が"));
    assert!(result.iter().any(|p| p.surface_key() == "舌ほうが"));
}

#[test]
fn test_partial_hiragana_dedup_via_run_rewriters() {
    let rw = PartialHiraganaRewriter;
    let mut paths = vec![
        ScoredPath {
            segments: vec![
                RichSegment {
                    reading: "した".into(),
                    surface: "下".into(),
                    left_id: 10,
                    right_id: 10,
                    word_cost: 0,
                },
                RichSegment {
                    reading: "ほう".into(),
                    surface: "方".into(),
                    left_id: 20,
                    right_id: 20,
                    word_cost: 0,
                },
            ],
            viterbi_cost: 3000,
        },
        // This path already has the surface "した方"
        ScoredPath {
            segments: vec![
                RichSegment {
                    reading: "した".into(),
                    surface: "した".into(),
                    left_id: 0,
                    right_id: 0,
                    word_cost: 0,
                },
                RichSegment {
                    reading: "ほう".into(),
                    surface: "方".into(),
                    left_id: 20,
                    right_id: 20,
                    word_cost: 0,
                },
            ],
            viterbi_cost: 5000,
        },
    ];

    run_rewriters(&[&rw], &mut paths, "したほう");

    // "した方" already exists in paths, should not be duplicated
    let count = paths.iter().filter(|p| p.surface_key() == "した方").count();
    assert_eq!(count, 1, "should not add duplicate した方");
}

#[test]
fn test_partial_hiragana_all_hiragana_no_variants() {
    let rw = PartialHiraganaRewriter;
    let paths = vec![ScoredPath {
        segments: vec![
            RichSegment {
                reading: "した".into(),
                surface: "した".into(),
                left_id: 0,
                right_id: 0,
                word_cost: 0,
            },
            RichSegment {
                reading: "ほう".into(),
                surface: "ほう".into(),
                left_id: 0,
                right_id: 0,
                word_cost: 0,
            },
        ],
        viterbi_cost: 1000,
    }];

    let result = rw.generate(&paths, "したほう");

    assert!(
        result.is_empty(),
        "all-hiragana path should produce no variants"
    );
}

#[test]
fn test_partial_hiragana_keeps_katakana() {
    let rw = PartialHiraganaRewriter;
    let paths = vec![ScoredPath {
        segments: vec![
            RichSegment {
                reading: "てすと".into(),
                surface: "テスト".into(),
                left_id: 10,
                right_id: 10,
                word_cost: 0,
            },
            RichSegment {
                reading: "ちゅう".into(),
                surface: "中".into(),
                left_id: 20,
                right_id: 20,
                word_cost: 0,
            },
        ],
        viterbi_cost: 2000,
    }];

    let result = rw.generate(&paths, "てすとちゅう");

    // Only 中→ちゅう variant, katakana テスト should NOT be replaced
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].surface_key(), "テストちゅう");
}

#[test]
fn test_partial_hiragana_single_segment_skip() {
    let rw = PartialHiraganaRewriter;
    let paths = vec![ScoredPath {
        segments: vec![RichSegment {
            reading: "した".into(),
            surface: "下".into(),
            left_id: 10,
            right_id: 10,
            word_cost: 0,
        }],
        viterbi_cost: 1000,
    }];

    let result = rw.generate(&paths, "した");

    assert!(result.is_empty(), "single-segment path should be skipped");
}

// ── KanjiVariantRewriter tests ────────────────────────────────────

#[test]
fn test_kanji_variant_replaces_2char_hiragana() {
    // Lattice has ほう → 方 (cost=733) at position [3,5)
    let lattice = Lattice::from_test_nodes(
        "あったほうが",
        &[
            (3, 5, "ほう", "ほう", 0, 0, 0),
            (3, 5, "ほう", "方", 733, 0, 0),
            (3, 5, "ほう", "法", 2181, 0, 0),
        ],
    );
    let rw = KanjiVariantRewriter { lattice: &lattice };

    let paths = vec![ScoredPath {
        segments: vec![
            RichSegment {
                reading: "あっ".into(),
                surface: "あっ".into(),
                left_id: 0,
                right_id: 0,
                word_cost: 0,
            },
            RichSegment {
                reading: "た".into(),
                surface: "た".into(),
                left_id: 0,
                right_id: 0,
                word_cost: 0,
            },
            RichSegment {
                reading: "ほう".into(),
                surface: "ほう".into(),
                left_id: 0,
                right_id: 0,
                word_cost: 0,
            },
            RichSegment {
                reading: "が".into(),
                surface: "が".into(),
                left_id: 0,
                right_id: 0,
                word_cost: 0,
            },
        ],
        viterbi_cost: 20000,
    }];

    let result = rw.generate(&paths, "あったほうが");

    // Should produce variants for 方 and 法 (top 3, but only 2 kanji available)
    assert_eq!(result.len(), 2);
    assert!(result.iter().any(|p| p.surface_key() == "あった方が"));
    assert!(result.iter().any(|p| p.surface_key() == "あった法が"));
    // All variants should have +2000 penalty
    assert!(result.iter().all(|p| p.viterbi_cost == 22000));
}

#[test]
fn test_kanji_variant_skips_single_char() {
    // Single-char hiragana (し) should NOT be replaced
    let lattice = Lattice::from_test_nodes("した", &[(0, 1, "し", "死", 500, 0, 0)]);
    let rw = KanjiVariantRewriter { lattice: &lattice };

    let paths = vec![ScoredPath {
        segments: vec![
            RichSegment {
                reading: "し".into(),
                surface: "し".into(),
                left_id: 0,
                right_id: 0,
                word_cost: 0,
            },
            RichSegment {
                reading: "た".into(),
                surface: "た".into(),
                left_id: 0,
                right_id: 0,
                word_cost: 0,
            },
        ],
        viterbi_cost: 1000,
    }];

    let result = rw.generate(&paths, "した");

    assert!(result.is_empty(), "single-char hiragana should be skipped");
}

#[test]
fn test_kanji_variant_skips_single_segment() {
    let lattice = Lattice::from_test_nodes("ほう", &[(0, 2, "ほう", "方", 733, 0, 0)]);
    let rw = KanjiVariantRewriter { lattice: &lattice };

    let paths = vec![ScoredPath {
        segments: vec![RichSegment {
            reading: "ほう".into(),
            surface: "ほう".into(),
            left_id: 0,
            right_id: 0,
            word_cost: 0,
        }],
        viterbi_cost: 1000,
    }];

    let result = rw.generate(&paths, "ほう");

    assert!(result.is_empty(), "single-segment path should be skipped");
}

#[test]
fn test_kanji_variant_skips_kanji_segments() {
    // Segments already containing kanji should not be processed
    let lattice = Lattice::from_test_nodes("したほう", &[(2, 4, "ほう", "方", 733, 0, 0)]);
    let rw = KanjiVariantRewriter { lattice: &lattice };

    let paths = vec![ScoredPath {
        segments: vec![
            RichSegment {
                reading: "した".into(),
                surface: "下".into(), // kanji — should skip
                left_id: 10,
                right_id: 10,
                word_cost: 0,
            },
            RichSegment {
                reading: "ほう".into(),
                surface: "方".into(), // kanji — should skip
                left_id: 20,
                right_id: 20,
                word_cost: 0,
            },
        ],
        viterbi_cost: 3000,
    }];

    let result = rw.generate(&paths, "したほう");

    assert!(
        result.is_empty(),
        "kanji segments should not produce variants"
    );
}

#[test]
fn test_kanji_variant_skips_3char_segments_no_2char_kanji() {
    // 3-char hiragana segment "たほう" — lattice has 他方 at [0,3) but
    // no 2-char kanji split, so subsplit produces nothing.
    let lattice = Lattice::from_test_nodes(
        "たほうが",
        &[
            (0, 3, "たほう", "他方", 5290, 0, 0),
            // No 2-char kanji at [0,2) ("たほ" has no kanji)
        ],
    );
    let rw = KanjiVariantRewriter { lattice: &lattice };

    let paths = vec![ScoredPath {
        segments: vec![
            RichSegment {
                reading: "たほう".into(),
                surface: "たほう".into(),
                left_id: 0,
                right_id: 0,
                word_cost: 0,
            },
            RichSegment {
                reading: "が".into(),
                surface: "が".into(),
                left_id: 0,
                right_id: 0,
                word_cost: 0,
            },
        ],
        viterbi_cost: 5000,
    }];

    let result = rw.generate(&paths, "たほうが");

    assert!(
        result.is_empty(),
        "3-char segment without 2-char kanji split should produce nothing"
    );
}

#[test]
fn test_kanji_variant_subsplit_3char_segment() {
    // 3-char hiragana segment "ほうが" [3,6) — lattice has 方 at [3,5)
    // and が at [5,6). Subsplit should produce "方が".
    let lattice = Lattice::from_test_nodes(
        "あったほうが",
        &[
            (3, 5, "ほう", "方", 733, 0, 0),
            (3, 5, "ほう", "法", 2181, 0, 0),
            (5, 6, "が", "が", 0, 0, 0),
        ],
    );
    let rw = KanjiVariantRewriter { lattice: &lattice };

    let paths = vec![ScoredPath {
        segments: vec![
            RichSegment {
                reading: "あっ".into(),
                surface: "あっ".into(),
                left_id: 0,
                right_id: 0,
                word_cost: 0,
            },
            RichSegment {
                reading: "た".into(),
                surface: "た".into(),
                left_id: 0,
                right_id: 0,
                word_cost: 0,
            },
            RichSegment {
                reading: "ほうが".into(),
                surface: "ほうが".into(),
                left_id: 0,
                right_id: 0,
                word_cost: 0,
            },
        ],
        viterbi_cost: 20000,
    }];

    let result = rw.generate(&paths, "あったほうが");

    // Should produce variants for 方+が and 法+が
    assert_eq!(result.len(), 2, "should produce 2 subsplit variants");
    assert!(result.iter().any(|p| p.surface_key() == "あった方が"));
    assert!(result.iter().any(|p| p.surface_key() == "あった法が"));
    // Check segment count increased by 1 (split added a segment)
    assert!(result.iter().all(|p| p.segments.len() == 4));
    assert!(result.iter().all(|p| p.viterbi_cost == 22000));
}

#[test]
fn test_kanji_variant_subsplit_only_2char_prefix() {
    // 4-char hiragana segment "ほうがく" — should only try 2-char prefix split.
    // Lattice has 方 at [0,2) and がく at [2,4) (hiragana).
    let lattice = Lattice::from_test_nodes(
        "ほうがくが",
        &[
            (0, 2, "ほう", "方", 733, 0, 0),
            // がく has kanji 学 but that's for the right side — we need hiragana
            (2, 4, "がく", "がく", 0, 0, 0),
        ],
    );
    let rw = KanjiVariantRewriter { lattice: &lattice };

    let paths = vec![ScoredPath {
        segments: vec![
            RichSegment {
                reading: "ほうがく".into(),
                surface: "ほうがく".into(),
                left_id: 0,
                right_id: 0,
                word_cost: 0,
            },
            RichSegment {
                reading: "が".into(),
                surface: "が".into(),
                left_id: 0,
                right_id: 0,
                word_cost: 0,
            },
        ],
        viterbi_cost: 10000,
    }];

    let result = rw.generate(&paths, "ほうがくが");

    // Should produce 方+がく variant
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].surface_key(), "方がくが");
}

#[test]
fn test_kanji_variant_reading_scan_single_segment() {
    // Single-segment hiragana path "しておいたほうが" — reading scan should
    // find 方 at [5,7) and produce a single-segment variant with kanji inlined.
    let lattice = Lattice::from_test_nodes(
        "しておいたほうが",
        &[
            (5, 7, "ほう", "方", 733, 0, 0),
            (5, 7, "ほう", "法", 2181, 0, 0),
        ],
    );
    let rw = KanjiVariantRewriter { lattice: &lattice };

    // Single-segment hiragana path (as produced by HiraganaVariantRewriter)
    let paths = vec![ScoredPath {
        segments: vec![RichSegment {
            reading: "しておいたほうが".into(),
            surface: "しておいたほうが".into(),
            left_id: 0,
            right_id: 0,
            word_cost: 0,
        }],
        viterbi_cost: 30000,
    }];

    let result = rw.generate(&paths, "しておいたほうが");

    // Should produce 方 and 法 variants as single-segment paths
    assert_eq!(result.len(), 2, "should produce 2 reading-scan variants");
    assert!(result.iter().any(|p| p.surface_key() == "しておいた方が"));
    assert!(result.iter().any(|p| p.surface_key() == "しておいた法が"));
    // Single-segment to avoid group_segments POS misclassification
    assert!(result.iter().all(|p| p.segments.len() == 1));
    assert!(result.iter().all(|p| p.viterbi_cost == 32000));
}

#[test]
fn test_kanji_variant_reading_scan_skips_edges() {
    // Reading scan should skip positions at start (pos=0) and end
    // where the remaining prefix/suffix would be empty.
    let lattice = Lattice::from_test_nodes("ほうが", &[(0, 2, "ほう", "方", 733, 0, 0)]);
    let rw = KanjiVariantRewriter { lattice: &lattice };

    let paths = vec![ScoredPath {
        segments: vec![RichSegment {
            reading: "ほうが".into(),
            surface: "ほうが".into(),
            left_id: 0,
            right_id: 0,
            word_cost: 0,
        }],
        viterbi_cost: 10000,
    }];

    let result = rw.generate(&paths, "ほうが");

    // pos=0 is skipped (no prefix), end=3 doesn't happen (2-char only goes to pos=1)
    // pos=1 → [1,3) "うが" — no kanji
    assert!(
        result.is_empty(),
        "should not produce variants at reading edges"
    );
}
