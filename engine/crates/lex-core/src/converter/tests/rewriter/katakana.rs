use crate::converter::postprocess::NoopObserver;
use crate::converter::rewriter::{
    run_rewriters, KatakanaRewriter, RewriteStage, Rewriter, KATAKANA_POLICY_OFFSET,
};
use crate::converter::viterbi::{PathOrigin, RichSegment, ScoredPath};

#[test]
fn test_katakana_rewriter_generates_candidate() {
    let rw = KatakanaRewriter { anchor: 3000 };
    let paths = vec![ScoredPath::new(
        vec![RichSegment {
            reading: "きょう".into(),
            surface: "今日".into(),
            left_id: 10,
            right_id: 10,
            word_cost: 0,
        }],
        3000,
        PathOrigin::Viterbi,
    )];

    let result = rw.generate(&paths, "きょう");

    assert_eq!(result.len(), 1);
    assert_eq!(result[0].surface_key(), "キョウ");
    assert_eq!(result[0].viterbi_cost, 3000 + KATAKANA_POLICY_OFFSET);
}

#[test]
fn test_katakana_dedup_via_run_rewriters() {
    let rw = KatakanaRewriter { anchor: 5000 };
    let mut paths = vec![ScoredPath::new(
        vec![RichSegment {
            reading: "きょう".into(),
            surface: "キョウ".into(),
            left_id: 0,
            right_id: 0,
            word_cost: 0,
        }],
        5000,
        PathOrigin::Viterbi,
    )];

    run_rewriters(
        &[&rw],
        &mut paths,
        "きょう",
        RewriteStage::Override,
        None,
        &mut NoopObserver,
    );

    assert_eq!(
        paths.len(),
        1,
        "should not add duplicate katakana candidate"
    );
}

#[test]
fn test_katakana_rewriter_empty_paths() {
    let rw = KatakanaRewriter { anchor: 0 };
    let paths: Vec<ScoredPath> = Vec::new();

    let result = rw.generate(&paths, "てすと");

    assert_eq!(result.len(), 1);
    assert_eq!(result[0].surface_key(), "テスト");
    assert_eq!(result[0].viterbi_cost, KATAKANA_POLICY_OFFSET);
}

/// §27-8 B6 (T-B4): a fallback priced under a path with its surface — a
/// learned path admission kept far above the limit — leaves that path's
/// price and `priced_by` alone, held head or not.
#[test]
fn test_katakana_fallback_does_not_reprice_its_surface() {
    let learned = || {
        let mut p = ScoredPath::single(
            "てすと".into(),
            "テスト".into(),
            30_000,
            PathOrigin::Viterbi,
        );
        p.whole_path_boost = 1;
        p
    };
    let other = ScoredPath::single("てすと".into(), "手酢と".into(), 1000, PathOrigin::Viterbi);
    let rw = KatakanaRewriter { anchor: 1000 };
    for mut paths in [vec![learned()], vec![other, learned()]] {
        run_rewriters(
            &[&rw],
            &mut paths,
            "てすと",
            RewriteStage::Override,
            None,
            &mut NoopObserver,
        );
        let listed: Vec<&ScoredPath> = paths
            .iter()
            .filter(|p| p.surface_key() == "テスト")
            .collect();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].viterbi_cost, 30_000);
        assert_eq!(listed[0].priced_by, PathOrigin::Viterbi);
    }
}
