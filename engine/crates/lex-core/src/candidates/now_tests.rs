//! T-N1: the read path decays history against the `now` it is handed, never
//! against the wall clock.
//!
//! Every history here was last used at `T0`, a moment in the future of any
//! wall clock a test runs under (`decay` saturates a negative age to 0, so
//! the wall clock would see every boost undecayed). Two injected moments pull
//! apart from it:
//! - `STALE`: ~1100 years on, every boost is decayed to 0. Any read that fell
//!   back to the wall clock would still see the full boost and lift the
//!   learned surface.
//! - `HALF`: one half-life (168 h) on. A boost is exactly half its undecayed
//!   value (`frequency * 3000`, capped at 15000); the wall clock would see it
//!   whole.

use crate::converter::explain::explain;
use crate::converter::testutil::test_dict;
use crate::converter::{convert_nbest_with_history, convert_with_history, ConvertedSegment};
use crate::dict::{DictEntry, TrieDictionary};
use crate::user_history::UserHistory;

use super::{generate_candidates, generate_prediction_candidates};

const T0: u64 = 4_000_000_000;
const HALF: u64 = T0 + 168 * 3600;
const STALE: u64 = T0 + 10_000_000 * 3600;

fn learned(sentences: &[&[(&str, &str)]]) -> UserHistory {
    let mut h = UserHistory::new();
    for pairs in sentences {
        let segs: Vec<(String, String)> = pairs
            .iter()
            .map(|&(r, s)| (r.to_string(), s.to_string()))
            .collect();
        h.record_at(&segs, T0);
    }
    h
}

fn joined(paths: &[Vec<ConvertedSegment>]) -> Vec<String> {
    paths
        .iter()
        .map(|p| p.iter().map(|s| s.surface.as_str()).collect())
        .collect()
}

#[test]
fn the_fixture_separates_the_injected_now_from_the_wall_clock() {
    let h = learned(&[&[("きょう", "京")]]);
    assert_eq!(h.unigram_boost("きょう", "京", HALF), 1500);
    assert_eq!(h.unigram_boost("きょう", "京", STALE), 0);
    // What a read gone back to the wall clock would see.
    let wall = crate::user_history::now_epoch();
    assert!(wall < T0, "fixture: T0 must lie ahead of the wall clock");
    assert_eq!(h.unigram_boost("きょう", "京", wall), 3000);
}

#[test]
fn one_best_and_nbest_decay_against_the_injected_now() {
    let dict = test_dict();
    let h = learned(&[&[("きょう", "京")]]);
    // 今日 (3000) is the unlearned #1; a boost lifts 京 (5000) past it.
    for (now, top) in [(STALE, "今日"), (HALF, "京")] {
        assert_eq!(
            convert_with_history(&dict, None, &h, "きょう", now)[0].surface,
            top,
            "1-best at {now}"
        );
        assert_eq!(
            joined(&convert_nbest_with_history(
                &dict,
                None,
                &h,
                "きょう",
                5,
                now
            ))[0],
            top,
            "n-best at {now}"
        );
    }
}

#[test]
fn explain_prices_history_at_the_injected_now() {
    let dict = test_dict();
    let h = learned(&[&[("きょう", "京")]]);

    // Half-life: boost = 3000 * 1/2 = 1500; a whole-path boost is 5x.
    let half = explain(&dict, None, Some(&h), "きょう", 5, HALF);
    let kyo = &half.paths[0];
    assert_eq!(kyo.surface(), "京");
    assert_eq!(kyo.history_breakdown.unigram_sum, 1500);
    assert_eq!(kyo.history_breakdown.whole_path_boost, 5 * 1500);

    // Decayed away: nothing is priced, and 京 does not lead.
    let stale = explain(&dict, None, Some(&h), "きょう", 5, STALE);
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

    let stale = generate_candidates(&dict, None, Some(&h), "きょう", 20, STALE);
    assert_eq!(stale.surfaces[0], "今日", "{:?}", stale.surfaces);
    assert!(
        !stale.surfaces.contains(&"強".to_string()),
        "a decayed surface is not injected: {:?}",
        stale.surfaces
    );

    let half = generate_candidates(&dict, None, Some(&h), "きょう", 20, HALF);
    // Both are learned at 1500, so the tie falls to the surface order
    // (`by_boost_then_recency`): the kana is promoted to index 0, the other
    // learned surface is injected where the N-best ends.
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

    let stale = generate_prediction_candidates(&dict, None, Some(&h), "きょう", 20, STALE);
    assert!(
        !stale.surfaces.contains(&"今日は".to_string()),
        "a decayed bigram does not chain: {:?}",
        stale.surfaces
    );

    let half = generate_prediction_candidates(&dict, None, Some(&h), "きょう", 20, HALF);
    assert!(
        half.surfaces.contains(&"今日は".to_string()),
        "{:?}",
        half.surfaces
    );
    assert_eq!(h.bigram_successors("今日", HALF)[0].2, 1500);
}

#[test]
fn punctuation_ranking_reads_the_injected_now() {
    let e = |surface: &str, cost: i16| DictEntry {
        surface: surface.into(),
        cost,
        left_id: 0,
        right_id: 0,
    };
    let dict =
        TrieDictionary::from_entries(vec![("、".into(), vec![e("、", 1000), e("，", 2000)])]);
    let h = learned(&[&[("、", "，")]]);

    // Learned predictions come first; decayed away, the dictionary's cost
    // order stands.
    let half = generate_candidates(&dict, None, Some(&h), "、", 9, HALF);
    assert_eq!(half.surfaces[..2], ["，", "、"], "{:?}", half.surfaces);
    let stale = generate_candidates(&dict, None, Some(&h), "、", 9, STALE);
    assert_eq!(stale.surfaces[..2], ["、", "，"], "{:?}", stale.surfaces);
}

/// Longer readings only the prediction step lists: 京都 is cheaper than 教室,
/// so only a learned 教室 can lead.
fn predicting_dict() -> TrieDictionary {
    let e = |surface: &str, cost: i16| DictEntry {
        surface: surface.into(),
        cost,
        left_id: 0,
        right_id: 0,
    };
    TrieDictionary::from_entries(vec![
        ("きょう".into(), vec![e("今日", 3000), e("京", 5000)]),
        ("きょうと".into(), vec![e("京都", 4000)]),
        ("きょうしつ".into(), vec![e("教室", 5000)]),
        ("は".into(), vec![e("は", 2000)]),
    ])
}

fn position(surfaces: &[String], s: &str) -> usize {
    surfaces
        .iter()
        .position(|x| x == s)
        .unwrap_or_else(|| panic!("{s} missing from {surfaces:?}"))
}

#[test]
fn predictions_are_ranked_by_the_injected_now() {
    let dict = predicting_dict();
    let h = learned(&[&[("きょうしつ", "教室")]]);

    let stale = generate_candidates(&dict, None, Some(&h), "きょう", 20, STALE);
    assert!(position(&stale.surfaces, "京都") < position(&stale.surfaces, "教室"));
    let half = generate_candidates(&dict, None, Some(&h), "きょう", 20, HALF);
    assert!(position(&half.surfaces, "教室") < position(&half.surfaces, "京都"));
}

#[test]
fn predictive_chains_from_listed_predictions_by_the_injected_now() {
    let dict = predicting_dict();
    // 教室 is listed by the prediction step, not by an N-best path.
    let h = learned(&[&[("きょうしつ", "教室"), ("は", "は")]]);

    let stale = generate_prediction_candidates(&dict, None, Some(&h), "きょう", 20, STALE);
    assert!(
        !stale.surfaces.contains(&"教室は".to_string()),
        "{:?}",
        stale.surfaces
    );
    let half = generate_prediction_candidates(&dict, None, Some(&h), "きょう", 20, HALF);
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
    h.record_at(&[("きょう".into(), "きょう".into())], STALE - 168 * 3600);

    // The #1's learning is gone, so the learned kana takes index 0. Had the
    // #1 still counted as learned, the kana would sit at index 1 below it.
    let list = generate_candidates(&dict, None, Some(&h), "きょう", 20, STALE);
    assert_eq!(
        list.surfaces[..2],
        ["きょう", "今日"],
        "{:?}",
        list.surfaces
    );
}
