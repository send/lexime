use super::{offer_price, pricer};
use crate::converter::lattice::Lattice;
use crate::converter::rewriter::{run_rewriters, PartialHiraganaRewriter, RewriteStage, Rewriter};
use crate::converter::viterbi::{PathOrigin, RichSegment, ScoredPath};

fn seg(reading: &str, surface: &str, id: u16) -> RichSegment {
    RichSegment {
        reading: reading.into(),
        surface: surface.into(),
        left_id: id,
        right_id: id,
        word_cost: 0,
    }
}

fn source(segments: Vec<RichSegment>, cost: i64) -> ScoredPath {
    ScoredPath::new(segments, cost, PathOrigin::Viterbi)
}

/// `下|方` with kana nodes した (cost 100, POS 1) and ほう (cost 50, POS 2).
fn shitahou() -> Lattice {
    Lattice::from_test_nodes(
        "したほう",
        &[
            (0, 2, "した", "した", 100, 1, 1),
            (2, 4, "ほう", "ほう", 50, 2, 2),
        ],
    )
}

fn shita_hou() -> ScoredPath {
    source(vec![seg("した", "下", 10), seg("ほう", "方", 20)], 20000)
}

fn generate(lattice: &Lattice, paths: &[ScoredPath], reading: &str) -> Vec<ScoredPath> {
    PartialHiraganaRewriter {
        lattice,
        conn: None,
        pricer: &pricer(),
    }
    .generate(paths, reading)
}

fn find<'a>(result: &'a [ScoredPath], surface: &str) -> &'a ScoredPath {
    result
        .iter()
        .find(|p| p.surface_key() == surface)
        .unwrap_or_else(|| panic!("no {surface}"))
}

#[test]
fn test_partial_hiragana_substitutes_the_lattice_kana_node() {
    let lattice = shitahou();
    let src = shita_hou();
    let result = generate(&lattice, std::slice::from_ref(&src), "したほう");

    assert_eq!(result.len(), 2);
    let v = find(&result, "した方");
    // The kana segment is the lattice node, not the kanji node relabelled.
    assert_eq!((v.segments[0].left_id, v.segments[0].word_cost), (1, 100));
    assert_eq!(v.viterbi_cost, offer_price(v, &src));
    let w = find(&result, "下ほう");
    assert_eq!(w.viterbi_cost, offer_price(w, &src));
    for p in &result {
        assert_eq!(
            (p.origin, p.priced_by),
            (PathOrigin::PartialHiragana, PathOrigin::PartialHiragana)
        );
        assert_eq!(p.history_boost, 0);
    }
}

#[test]
fn test_partial_hiragana_has_no_gap_bound() {
    // Kana is always a valid spelling (#263): a kana node the model prices
    // far above the kanji is still offered, at the cap.
    let lattice = Lattice::from_test_nodes("したほう", &[(0, 2, "した", "した", 20000, 1, 1)]);
    let result = generate(&lattice, &[shita_hou()], "したほう");
    let v = find(&result, "した方");
    assert_eq!(v.viterbi_cost, offer_price(v, &shita_hou()));
    assert_eq!(v.viterbi_cost, 22000, "capped");
}

#[test]
fn test_partial_hiragana_without_kana_node_keeps_the_kanji_node_at_the_cap() {
    // No kana node for either span: the kanji node carries its reading as
    // its surface (as the kana rescue does for the whole input).
    let lattice = Lattice::from_test_nodes("したほう", &[]);
    let result = generate(&lattice, &[shita_hou()], "したほう");
    assert_eq!(result.len(), 2);
    let v = find(&result, "した方");
    assert_eq!(v.segments[0].left_id, 10, "the source's kanji node");
    assert_eq!(v.segments[0].surface, "した");
    assert_eq!(v.viterbi_cost, 22000);
    assert!(!v.origin.is_lattice_path());
}

#[test]
fn test_partial_hiragana_offers_the_cheapest_kana_node() {
    let lattice = Lattice::from_test_nodes(
        "したほう",
        &[
            (0, 2, "した", "した", 900, 1, 1),
            (0, 2, "した", "した", 100, 3, 3),
        ],
    );
    let result = generate(&lattice, &[shita_hou()], "したほう");
    assert_eq!(find(&result, "した方").segments[0].left_id, 3);
}

#[test]
fn test_partial_hiragana_dedup_via_run_rewriters() {
    let lattice = shitahou();
    let pricer = pricer();
    let rw = PartialHiraganaRewriter {
        lattice: &lattice,
        conn: None,
        pricer: &pricer,
    };
    let mut paths = vec![
        source(vec![seg("した", "下", 10), seg("ほう", "方", 20)], 3000),
        // This path already has the surface "した方"
        source(vec![seg("した", "した", 1), seg("ほう", "方", 20)], 9000),
    ];

    run_rewriters(&[&rw], &mut paths, "したほう", RewriteStage::Model, None);

    let same: Vec<_> = paths
        .iter()
        .filter(|p| p.surface_key() == "した方")
        .collect();
    assert_eq!(same.len(), 1, "should not add duplicate した方");
    // The offer (≤ 3000 + 2000) reprices the listed path; its segments stay.
    assert!(same[0].viterbi_cost <= 5000);
    assert_eq!(same[0].priced_by, PathOrigin::PartialHiragana);
    assert_eq!(same[0].origin, PathOrigin::Viterbi);
}

#[test]
fn test_partial_hiragana_all_hiragana_no_variants() {
    let lattice = shitahou();
    let src = source(vec![seg("した", "した", 1), seg("ほう", "ほう", 2)], 1000);
    assert!(generate(&lattice, &[src], "したほう").is_empty());
}

#[test]
fn test_partial_hiragana_keeps_katakana() {
    let lattice = Lattice::from_test_nodes("てすとちゅう", &[(3, 6, "ちゅう", "ちゅう", 0, 2, 2)]);
    let src = source(
        vec![seg("てすと", "テスト", 10), seg("ちゅう", "中", 20)],
        20000,
    );
    let result = generate(&lattice, &[src], "てすとちゅう");

    // Only 中→ちゅう variant, katakana テスト should NOT be replaced
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].surface_key(), "テストちゅう");
}

#[test]
fn test_partial_hiragana_single_segment_skip() {
    let lattice = Lattice::from_test_nodes("した", &[(0, 2, "した", "した", 0, 1, 1)]);
    let src = source(vec![seg("した", "下", 10)], 1000);
    assert!(generate(&lattice, &[src], "した").is_empty());
}

#[test]
fn test_partial_hiragana_sources_are_model_paths_only() {
    let lattice = shitahou();
    let from = |origin, priced_by| {
        let mut p = shita_hou();
        p.origin = origin;
        p.priced_by = priced_by;
        generate(&lattice, &[p], "したほう").len()
    };
    for other in [
        PathOrigin::KanjiVariant,
        PathOrigin::PartialHiragana,
        PathOrigin::HiraganaVariant,
        PathOrigin::Katakana,
        PathOrigin::Numeric,
    ] {
        assert_eq!(from(other, other), 0, "{other:?} must not be a source");
        // A model path another rewriter repriced carries that price, not the
        // model's: sourcing from it would chain offers.
        assert_eq!(
            from(PathOrigin::Viterbi, other),
            0,
            "a path repriced by {other:?} must not be a source"
        );
    }
    for origin in [PathOrigin::Viterbi, PathOrigin::Resegment] {
        assert_eq!(from(origin, origin), 2, "{origin:?} is a source");
    }
}
