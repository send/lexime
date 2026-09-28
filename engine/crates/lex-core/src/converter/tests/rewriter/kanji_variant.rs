use super::{offer_price, pricer};
use crate::converter::lattice::Lattice;
use crate::converter::rewriter::{KanjiVariantRewriter, Rewriter};
use crate::converter::viterbi::{PathOrigin, RichSegment, ScoredPath};

fn kana(reading: &str) -> RichSegment {
    RichSegment {
        reading: reading.into(),
        surface: reading.into(),
        left_id: 0,
        right_id: 0,
        word_cost: 0,
    }
}

fn source(segments: Vec<RichSegment>, cost: i64, origin: PathOrigin) -> ScoredPath {
    ScoredPath::new(segments, cost, origin)
}

/// `あっ|た|ほう|が`, all kana at word cost 0.
fn attahouga(cost: i64) -> ScoredPath {
    source(
        vec![kana("あっ"), kana("た"), kana("ほう"), kana("が")],
        cost,
        PathOrigin::Viterbi,
    )
}

fn generate(lattice: &Lattice, paths: &[ScoredPath], reading: &str) -> Vec<ScoredPath> {
    KanjiVariantRewriter {
        lattice,
        conn: None,
        pricer: &pricer(),
    }
    .generate(paths, reading)
}

#[test]
fn test_kanji_variant_offers_the_favourite_kanji_only() {
    // 方 is the model's favourite for ほう; 法 and 砲 would be homophone
    // offers at the same capped price, so they are not generated.
    let lattice = Lattice::from_test_nodes(
        "あったほうが",
        &[
            (3, 5, "ほう", "ほう", 0, 0, 0),
            (3, 5, "ほう", "方", 733, 0, 0),
            (3, 5, "ほう", "法", 2181, 0, 0),
            (3, 5, "ほう", "砲", 4000, 0, 0),
        ],
    );
    let src = attahouga(20000);
    let result = generate(&lattice, std::slice::from_ref(&src), "あったほうが");

    assert_eq!(result.len(), 1);
    let v = &result[0];
    assert_eq!(v.surface_key(), "あった方が");
    assert_eq!(v.viterbi_cost, offer_price(v, &src));
    assert_eq!(
        (v.origin, v.priced_by),
        (PathOrigin::KanjiVariant, PathOrigin::KanjiVariant)
    );
    assert_eq!(v.history_boost, 0);
}

#[test]
fn test_kanji_variant_cap_and_gap_bound() {
    let src = attahouga(20000);
    let offer_at = |cost: i16| {
        let lattice = Lattice::from_test_nodes("あったほうが", &[(3, 5, "ほう", "方", cost, 0, 0)]);
        generate(&lattice, std::slice::from_ref(&src), "あったほうが")
            .into_iter()
            .next()
    };
    // The model's gap beyond the cap: offered at source + 2000 at most.
    let far = offer_at(3380).expect("offered");
    assert_eq!(far.viterbi_cost, offer_price(&far, &src));
    assert!(far.viterbi_cost <= 22000);
    // At the Viterbi gap bound: still offered. Past it: not offered.
    assert!(offer_at(6000).is_some());
    assert!(offer_at(6001).is_none());
}

#[test]
fn test_kanji_variant_exact_span_of_any_length() {
    // ください → 下さい is one 4-char node; there is no 下(くだ) node, so a
    // split at +2 (くだ → 管 + さい) is never made.
    let lattice = Lattice::from_test_nodes(
        "してください",
        &[
            (2, 6, "ください", "下さい", 3380, 0, 0),
            (2, 4, "くだ", "管", 500, 0, 0),
        ],
    );
    let src = source(
        vec![kana("し"), kana("て"), kana("ください")],
        20000,
        PathOrigin::Viterbi,
    );
    let result = generate(&lattice, &[src], "してください");
    let surfaces: Vec<String> = result.iter().map(|p| p.surface_key()).collect();
    assert_eq!(surfaces, ["して下さい"]);
}

#[test]
fn test_kanji_variant_does_not_inherit_history_boost() {
    // #248: the cap is the source's pre-history cost, never its boosted one.
    // (In the pipeline variants run before history; the rewriter holds the
    // contract on its own too.)
    let lattice = Lattice::from_test_nodes("あったほうが", &[(3, 5, "ほう", "方", 3000, 0, 0)]);
    let mut src = attahouga(-30000);
    src.history_boost = 50000;
    let result = generate(&lattice, std::slice::from_ref(&src), "あったほうが");
    assert_eq!(result[0].viterbi_cost, offer_price(&result[0], &src));
    assert!((20000..=22000).contains(&result[0].viterbi_cost));
}

#[test]
fn test_kanji_variant_skips_single_char() {
    // Single-char hiragana (し) should NOT be replaced
    let lattice = Lattice::from_test_nodes("した", &[(0, 1, "し", "死", 500, 0, 0)]);
    let src = source(vec![kana("し"), kana("た")], 1000, PathOrigin::Viterbi);
    assert!(generate(&lattice, &[src], "した").is_empty());
}

#[test]
fn test_kanji_variant_skips_single_segment() {
    // A single-segment kana path has no internal boundaries, so no kanji may
    // be inlined into it — even where the lattice has a kanji node for a
    // sub-span. Inlining at arbitrary offsets cut through words (お|圧|さか).
    let lattice = Lattice::from_test_nodes(
        "しておいたほうが",
        &[
            (5, 7, "ほう", "方", 733, 0, 0),
            (0, 8, "しておいたほうが", "為て置いた方が", 0, 0, 0),
        ],
    );
    let src = source(vec![kana("しておいたほうが")], 30000, PathOrigin::Viterbi);
    assert!(generate(&lattice, &[src], "しておいたほうが").is_empty());
}

#[test]
fn test_kanji_variant_skips_kanji_segments() {
    let lattice = Lattice::from_test_nodes("したほう", &[(2, 4, "ほう", "方", 733, 0, 0)]);
    let src = source(
        vec![
            RichSegment {
                surface: "下".into(),
                ..kana("した")
            },
            RichSegment {
                surface: "方".into(),
                ..kana("ほう")
            },
        ],
        3000,
        PathOrigin::Viterbi,
    );
    assert!(generate(&lattice, &[src], "したほう").is_empty());
}

#[test]
fn test_kanji_variant_sources_are_model_paths_only() {
    // Offers and repriced paths are never sources, so variants do not chain; synthetic
    // single-segment paths are not sources either.
    let lattice = Lattice::from_test_nodes("あったほうが", &[(3, 5, "ほう", "方", 733, 0, 0)]);
    let from = |origin, priced_by| {
        let mut p = attahouga(20000);
        p.origin = origin;
        p.priced_by = priced_by;
        generate(&lattice, &[p], "あったほうが").len()
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
        assert_eq!(from(origin, origin), 1, "{origin:?} is a source");
    }
}
