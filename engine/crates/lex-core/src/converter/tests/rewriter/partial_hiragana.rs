use crate::converter::rewriter::{run_rewriters, PartialHiraganaRewriter, Rewriter};
use crate::converter::viterbi::{RichSegment, ScoredPath};

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
