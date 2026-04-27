use crate::converter::rewriter::{run_rewriters, HiraganaVariantRewriter, Rewriter};
use crate::converter::viterbi::{RichSegment, ScoredPath};

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
