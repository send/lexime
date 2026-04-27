use crate::converter::rewriter::{
    run_rewriters, HiraganaVariantRewriter, KatakanaRewriter, NumericRewriter,
    PartialHiraganaRewriter,
};
use crate::converter::viterbi::{RichSegment, ScoredPath};

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
    let rw = NumericRewriter {
        lattice: None,
        connection: None,
    };
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
