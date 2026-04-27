use crate::converter::lattice::Lattice;
use crate::converter::rewriter::{KanjiVariantRewriter, Rewriter};
use crate::converter::viterbi::{RichSegment, ScoredPath};

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
