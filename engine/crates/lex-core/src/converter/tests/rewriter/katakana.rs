use crate::converter::rewriter::{run_rewriters, KatakanaRewriter, Rewriter};
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
