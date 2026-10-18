//! T-N1: the read path decays history against the `now` it is handed, never
//! against the wall clock.
//!
//! Every history here was last used at `T0`, a moment ahead of any wall clock
//! a test runs under (see `testutil`), so a read that fell back to the clock
//! would see every boost whole. Two injected moments pull apart from it:
//! - `STALE`: every boost is decayed to 0, so nothing learned may lead, be
//!   injected or chain. The wall clock would still see the full boost.
//! - `half()`: one half-life on. A boost is exactly half its undecayed value
//!   (`frequency * 3000`, capped at 15000); the wall clock would see it whole.

use crate::converter::explain::explain;
use crate::converter::testutil::{entry, half, joined, learned, test_dict, STALE, T0};
use crate::converter::{ConversionContext, ConvertedSegment};
use crate::dict::TrieDictionary;
use crate::user_history::now_epoch;

use super::{generate_candidates, generate_prediction_candidates};

fn nbest_surfaces(paths: &[Vec<ConvertedSegment>]) -> Vec<String> {
    paths.iter().map(|p| joined(p)).collect()
}

fn position(surfaces: &[String], s: &str) -> usize {
    surfaces
        .iter()
        .position(|x| x == s)
        .unwrap_or_else(|| panic!("{s} missing from {surfaces:?}"))
}

#[test]
fn the_fixture_separates_the_injected_now_from_the_wall_clock() {
    let h = learned(&[&[("きょう", "京")]]);
    assert_eq!(h.unigram_boost("きょう", "京", half()), 1500);
    assert_eq!(h.unigram_boost("きょう", "京", STALE), 0);
    // What a read gone back to the wall clock would see.
    let wall = now_epoch();
    assert!(wall < T0, "fixture: T0 must lie ahead of the wall clock");
    assert_eq!(h.unigram_boost("きょう", "京", wall), 3000);
}

#[test]
fn one_best_and_nbest_decay_against_the_injected_now() {
    let dict = test_dict();
    let h = learned(&[&[("きょう", "京")]]);
    // 今日 (3000) is the unlearned #1; a boost lifts 京 (5000) past it.
    for (now, top) in [(STALE, "今日"), (half(), "京")] {
        let ctx = ConversionContext::new(&dict, None).with_history(Some(&h), now);
        assert_eq!(ctx.convert("きょう")[0].surface, top, "1-best at {now}");
        assert_eq!(
            nbest_surfaces(&ctx.convert_nbest("きょう", 5))[0],
            top,
            "n-best at {now}"
        );
    }
}

#[test]
fn explain_prices_history_at_the_injected_now() {
    let dict = test_dict();
    let h = learned(&[&[("きょう", "京")]]);
    let at = |now| ConversionContext::new(&dict, None).with_history(Some(&h), now);

    // Half-life: boost = 3000 * 1/2 = 1500; a whole-path boost is 5x.
    let half = explain(&at(half()), "きょう", 5);
    let kyo = &half.paths[0];
    assert_eq!(kyo.surface(), "京");
    assert_eq!(kyo.history_breakdown.unigram_sum, 1500);
    assert_eq!(kyo.history_breakdown.whole_path_boost, 5 * 1500);

    // Decayed away: nothing is priced, and 京 does not lead.
    let stale = explain(&at(STALE), "きょう", 5);
    assert_eq!(stale.paths[0].surface(), "今日");
    for p in &stale.paths {
        assert_eq!(p.history_breakdown.unigram_sum, 0, "{}", p.surface());
        assert_eq!(p.history_breakdown.whole_path_boost, 0, "{}", p.surface());
    }
}

#[test]
fn candidate_list_promotes_and_injects_by_the_injected_now() {
    let dict = test_dict();
    // A learned kana (promotion) and a learned surface the dictionary lacks
    // (injection after the N-best).
    let h = learned(&[&[("きょう", "きょう")], &[("きょう", "強")]]);
    let at = |now| ConversionContext::new(&dict, None).with_history(Some(&h), now);

    let stale = generate_candidates(&at(STALE), "きょう", 20);
    assert_eq!(stale.surfaces[0], "今日", "{:?}", stale.surfaces);
    assert!(
        !stale.surfaces.contains(&"強".to_string()),
        "a decayed surface is not injected: {:?}",
        stale.surfaces
    );

    // Both are learned at 1500: the kana is promoted to index 0, the other
    // learned surface is injected where the N-best ends.
    let half = generate_candidates(&at(half()), "きょう", 20);
    assert_eq!(half.surfaces[0], "きょう", "{:?}", half.surfaces);
    assert!(
        half.surfaces.contains(&"強".to_string()),
        "{:?}",
        half.surfaces
    );
}

#[test]
fn predictive_chains_bigrams_by_the_injected_now() {
    let dict = test_dict();
    let h = learned(&[&[("きょう", "今日"), ("は", "は")]]);
    let at = |now| ConversionContext::new(&dict, None).with_history(Some(&h), now);

    let stale = generate_prediction_candidates(&at(STALE), "きょう", 20);
    assert!(
        !stale.surfaces.contains(&"今日は".to_string()),
        "a decayed bigram does not chain: {:?}",
        stale.surfaces
    );

    let half = generate_prediction_candidates(&at(half()), "きょう", 20);
    assert!(
        half.surfaces.contains(&"今日は".to_string()),
        "{:?}",
        half.surfaces
    );
    assert_eq!(h.bigram_successors("今日", self::half())[0].2, 1500);
}

#[test]
fn punctuation_ranking_reads_the_injected_now() {
    let dict = TrieDictionary::from_entries(vec![(
        "、".into(),
        vec![entry("、", 1000), entry("，", 2000)],
    )]);
    let h = learned(&[&[("、", "，")]]);
    let at = |now| ConversionContext::new(&dict, None).with_history(Some(&h), now);

    // Learned predictions come first; decayed away, the dictionary's cost
    // order stands.
    let half = generate_candidates(&at(half()), "、", 9);
    assert_eq!(half.surfaces[..2], ["，", "、"], "{:?}", half.surfaces);
    let stale = generate_candidates(&at(STALE), "、", 9);
    assert_eq!(stale.surfaces[..2], ["、", "，"], "{:?}", stale.surfaces);
}

/// Longer readings only the prediction step lists: 京都 is cheaper than 教室,
/// so only a learned 教室 can lead.
fn predicting_dict() -> TrieDictionary {
    TrieDictionary::from_entries(vec![
        (
            "きょう".into(),
            vec![entry("今日", 3000), entry("京", 5000)],
        ),
        ("きょうと".into(), vec![entry("京都", 4000)]),
        ("きょうしつ".into(), vec![entry("教室", 5000)]),
        ("は".into(), vec![entry("は", 2000)]),
    ])
}

#[test]
fn predictions_are_ranked_by_the_injected_now() {
    let dict = predicting_dict();
    let h = learned(&[&[("きょうしつ", "教室")]]);
    let at = |now| ConversionContext::new(&dict, None).with_history(Some(&h), now);

    let stale = generate_candidates(&at(STALE), "きょう", 20);
    assert!(position(&stale.surfaces, "京都") < position(&stale.surfaces, "教室"));
    let half = generate_candidates(&at(half()), "きょう", 20);
    assert!(position(&half.surfaces, "教室") < position(&half.surfaces, "京都"));
}

#[test]
fn predictive_chains_from_listed_predictions_by_the_injected_now() {
    let dict = predicting_dict();
    // 教室 is listed by the prediction step, not by an N-best path.
    let h = learned(&[&[("きょうしつ", "教室"), ("は", "は")]]);
    let at = |now| ConversionContext::new(&dict, None).with_history(Some(&h), now);

    let stale = generate_prediction_candidates(&at(STALE), "きょう", 20);
    assert!(
        !stale.surfaces.contains(&"教室は".to_string()),
        "{:?}",
        stale.surfaces
    );
    let half = generate_prediction_candidates(&at(half()), "きょう", 20);
    assert!(
        half.surfaces.contains(&"教室は".to_string()),
        "{:?}",
        half.surfaces
    );
}

#[test]
fn kana_goes_above_a_top_1_whose_learning_has_decayed() {
    let dict = test_dict();
    // 今日 (the #1) was learned at T0, the kana half a life before `now`.
    let mut h = learned(&[&[("きょう", "今日")]]);
    h.record_at(&[("きょう".into(), "きょう".into())], STALE - (half() - T0));
    let ctx = ConversionContext::new(&dict, None).with_history(Some(&h), STALE);

    // The #1's learning is gone, so the learned kana takes index 0. Had the
    // #1 still counted as learned, the kana would sit at index 1 below it.
    let list = generate_candidates(&ctx, "きょう", 20);
    assert_eq!(
        list.surfaces[..2],
        ["きょう", "今日"],
        "{:?}",
        list.surfaces
    );
}

/// A number compound (二十) is created after history reranking and learned on
/// the same terms: its boost, too, decays against the injected `now`.
#[test]
fn number_compound_is_boosted_at_the_injected_now() {
    let dict = TrieDictionary::from_entries(vec![(
        "にじゅう".into(),
        vec![entry("二重", 3000), entry("荷重", 9000)],
    )]);
    // 荷重 was learned at T0; 二十 ten times, a day before STALE.
    let mut h = learned(&[&[("にじゅう", "荷重")]]);
    for _ in 0..10 {
        h.record_at(&[("にじゅう".into(), "二十".into())], STALE - 24 * 3600);
    }

    // At STALE the 荷重 learning is gone and 二十's is 15000 * 1/(1+24/168).
    let ctx = ConversionContext::new(&dict, None).with_history(Some(&h), STALE);
    assert_eq!(h.unigram_boost("にじゅう", "荷重", STALE), 0);
    assert_eq!(h.unigram_boost("にじゅう", "二十", STALE), 13125);
    let nbest = ctx.convert_nbest_from_lattice(&ctx.build_lattice("にじゅう"), 5);
    assert_eq!(joined(&nbest[0]), "二十");
    let ex = explain(&ctx, "にじゅう", 5);
    assert_eq!(ex.paths[0].surface(), "二十");
    assert_eq!(ex.paths[0].history_breakdown.whole_path_boost, 5 * 13125);
    // The price the pipeline set (not the observer's own breakdown) took the
    // same boost off: the whole-path 5x plus the one segment's unigram.
    assert_eq!(ex.paths[0].history_boost, 6 * 13125);
}

/// More same-reading entries than the N-best holds, so the dearest few are
/// listed only by the lookup step (the predictions are cut at `max_results`).
fn crowded_dict(entries: usize) -> TrieDictionary {
    let crowd: Vec<_> = (0..entries)
        .map(|i| entry(&format!("語{i:02}"), 3000 + 100 * i as i16))
        .collect();
    TrieDictionary::from_entries(vec![("ごご".into(), crowd)])
}

#[test]
fn lookup_remainder_is_reordered_by_the_injected_now() {
    let nbest = crate::settings::settings().candidates.nbest;
    let entries = nbest + 4;
    let dict = crowded_dict(entries);
    // Only the dearest entry is learned: too dear to be lifted into the
    // N-best, and (at STALE) not injected either.
    let last = format!("語{:02}", entries - 1);
    let h = learned(&[&[("ごご", &last)]]);
    let at = |now| ConversionContext::new(&dict, None).with_history(Some(&h), now);

    // The lookup step appends what the N-best and the predictions did not
    // list. Decayed away, it keeps the dictionary's cost order, so the
    // learned entry stays last; had the reorder read the wall clock it would
    // see the entry whole and move it first.
    let stale = generate_candidates(&at(STALE), "ごご", 2);
    let tail: Vec<&String> = stale.surfaces.iter().rev().take(4).rev().collect();
    let want: Vec<String> = (entries - 4..entries)
        .map(|i| format!("語{i:02}"))
        .collect();
    assert_eq!(
        tail,
        want.iter().collect::<Vec<_>>(),
        "{:?}",
        stale.surfaces
    );
}
