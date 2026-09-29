//! Cost-gap admission: which paths stay a candidate, at which T.

use super::*;
use crate::converter::postprocess::{
    admit_by_cost_gap, postprocess_observed, NoopObserver, PostprocessContext,
};
use crate::converter::rewriter::RESCUE_OFFSET;
use crate::converter::testutil::{entry as e, taberu_dict as taberu};
use crate::converter::viterbi::PathOrigin;
use crate::dict::TrieDictionary;
use crate::settings::DEFAULT_CANDIDATE_MAX_COST_GAP;
use crate::user_history::UserHistory;

fn path(reading: &str, surface: &str, cost: i64) -> ScoredPath {
    ScoredPath::single(reading.into(), surface.into(), cost, PathOrigin::Viterbi)
}

fn keys(paths: &[ScoredPath]) -> Vec<String> {
    paths.iter().map(ScoredPath::surface_key).collect()
}

/// Admission, returning the surfaces it dropped.
fn admit(paths: &mut Vec<ScoredPath>, anchor: i64, max_gap: i64) -> Vec<String> {
    let mut dropped = Vec::new();
    admit_by_cost_gap(paths, anchor, max_gap, |p| dropped.push(p.surface_key()));
    dropped
}

/// The pipeline at a chosen T (production reads T from settings, which a
/// test cannot vary): Viterbi at `oversample`, then postprocess.
fn run(
    dict: &TrieDictionary,
    history: Option<&UserHistory>,
    kana: &str,
    n: usize,
    oversample: usize,
    max_cost_gap: i64,
) -> Vec<ScoredPath> {
    let lattice = build_lattice(dict, kana);
    let mut raw = viterbi_nbest(&lattice, &DefaultCostFunction::new(None), oversample);
    let ctx = PostprocessContext {
        lattice: &lattice,
        conn: None,
        dict: Some(dict),
        history,
        kana,
        n,
        max_cost_gap,
        now: crate::user_history::now_epoch(),
    };
    postprocess_observed(&mut raw, &ctx, &mut NoopObserver)
}

// ---------------------------------------------------------------------------
// admit_by_cost_gap
// ---------------------------------------------------------------------------

#[test]
fn keeps_index_zero_and_paths_within_the_gap() {
    let mut paths = vec![
        path("よみ", "一", 1000),
        path("よみ", "内", 9000),
        path("よみ", "外", 9001),
    ];
    let dropped = admit(&mut paths, 1000, 8000);
    assert_eq!(keys(&paths), ["一", "内"], "gap == T stays, T + 1 goes");
    assert_eq!(dropped, ["外"], "the dropped path is reported");

    // Index 0 stays whatever its cost (a learned #1 can sit above the anchor).
    let mut paths = vec![path("よみ", "学", 50_000), path("よみ", "外", 20_000)];
    admit(&mut paths, 1000, 8000);
    assert_eq!(keys(&paths), ["学"]);
}

#[test]
fn committed_surfaces_and_the_typed_kana_stay_whatever_their_gap() {
    let mut learned = path("よみ", "習", 90_000);
    learned.whole_path_boost = 1;
    let mut paths = vec![
        path("よみ", "一", 0),
        learned,
        path("よみ", "よみ", 90_000),
        path("よみ", "外", 90_000),
    ];
    admit(&mut paths, 0, 8000);
    assert_eq!(keys(&paths), ["一", "習", "よみ"]);
}

#[test]
fn the_gap_never_cuts_below_the_kana_rescue_band() {
    for t in [0, 3999, RESCUE_OFFSET] {
        let mut paths = vec![
            path("よみ", "一", 1000),
            path("よみ", "救", 1000 + RESCUE_OFFSET),
            path("よみ", "外", 1000 + RESCUE_OFFSET + 1),
        ];
        admit(&mut paths, 1000, t);
        assert_eq!(keys(&paths), ["一", "救"], "T = {t}");
    }
}

#[test]
fn admission_is_a_no_op_on_empty_single_and_unbounded_lists() {
    let mut empty: Vec<ScoredPath> = Vec::new();
    admit(&mut empty, 0, 0);
    assert!(empty.is_empty());
    let mut one = vec![path("よみ", "一", 0)];
    admit(&mut one, 0, 0);
    assert_eq!(keys(&one), ["一"]);
    let mut far = vec![path("よみ", "一", 0), path("よみ", "遠", i64::MAX - 1)];
    admit(&mut far, i64::MAX - 10, i64::MAX);
    assert_eq!(keys(&far), ["一", "遠"], "the limit saturates");
}

// ---------------------------------------------------------------------------
// Through the pipeline
// ---------------------------------------------------------------------------

#[test]
fn fragments_far_above_the_best_are_dropped_and_come_back_with_no_bound() {
    let dict = taberu();
    let bounded = keys(&run(&dict, None, "たべる", 20, 60, 0));
    let unbounded = keys(&run(&dict, None, "たべる", 20, 60, i64::MAX));
    assert!(
        unbounded.contains(&"田辺留".to_string()),
        "fixture: {unbounded:?}"
    );
    assert!(!bounded.contains(&"田辺留".to_string()), "{bounded:?}");
    assert_eq!(bounded[0], "食べる");
    assert!(
        bounded.contains(&"たべる".to_string()),
        "the typed kana stays"
    );
}

#[test]
fn a_rescue_surface_the_model_already_prices_stays_at_the_floor() {
    // The best keeps a katakana segment (テスト|去れ|ます), so the kana
    // rescue's surface テストされます is not identity, and a Viterbi path
    // already spells it 1000 above the best — cheaper than the rescue
    // (best + 4000) and no dearer than the Partial offer of the same
    // spelling: neither reprices it, its price stays the model's, and with
    // no bound but the floor (T = 0) the floor is what keeps it.
    let dict = TrieDictionary::from_entries(vec![
        ("てすと".into(), vec![e("テスト", 0)]),
        ("され".into(), vec![e("去れ", 0), e("され", -2000)]),
        ("ます".into(), vec![e("ます", 0)]),
    ]);
    let paths = run(&dict, None, "てすとされます", 20, 60, 0);
    let rescue = paths
        .iter()
        .find(|p| p.surface_key() == "テストされます")
        .expect("the rescue's surface stays");
    assert!(
        rescue.priced_by.is_model(),
        "fixture: a model-priced rescue surface"
    );
    assert!(!rescue.is_identity(), "fixture: not saved by identity");
}

#[test]
fn a_whole_pair_commit_keeps_a_far_surface_and_segments_alone_do_not() {
    let dict = taberu();
    let far = |h: &UserHistory| keys(&run(&dict, Some(h), "たべる", 20, 60, 0));

    // Recorded as the live engine does: whole pairs and the committed
    // segments as separate records, 食べる committed more often so 田辺留
    // is kept by the exemption, not by being #1.
    let mut whole = UserHistory::new();
    for _ in 0..3 {
        whole.record(&[("たべる".into(), "食べる".into())]);
    }
    whole.record(&[("たべる".into(), "田辺留".into())]);
    whole.record(&[("た".into(), "田".into()), ("べる".into(), "辺留".into())]);
    let listed = far(&whole);
    assert_eq!(listed[0], "食べる", "fixture: the exemption, not index 0");
    assert!(listed.contains(&"田辺留".to_string()), "{listed:?}");

    // Per-segment learning of a multi-segment path is not "committed for
    // this reading": the path still has to be within the gap.
    let mut segments = UserHistory::new();
    segments.record(&[("た".into(), "田".into()), ("べる".into(), "辺留".into())]);
    let listed = far(&segments);
    assert!(
        listed[0] != "田辺留",
        "fixture: not lifted to #1 {listed:?}"
    );
    assert!(!listed.contains(&"田辺留".to_string()), "{listed:?}");
}

#[test]
fn a_learned_top_1_stays_however_far_it_was_priced() {
    // Enough per-segment learning lifts 田|辺留 to #1: index 0 stays.
    let dict = taberu();
    let mut h = UserHistory::new();
    for _ in 0..10 {
        h.record(&[("た".into(), "田".into()), ("べる".into(), "辺留".into())]);
    }
    let paths = run(&dict, Some(&h), "たべる", 20, 60, 0);
    assert_eq!(paths[0].surface_key(), "田辺留");
    assert_eq!(paths[0].whole_path_boost, 0, "fixture: per-segment only");
}

#[test]
fn the_one_best_is_the_same_at_every_bound() {
    let dict = taberu();
    let empty = UserHistory::new();
    let mut learned = UserHistory::new();
    learned.record(&[("たべる".into(), "田辺留".into())]);
    for history in [None, Some(&empty), Some(&learned)] {
        let oversample = if history.is_some() { 30 } else { 10 };
        let one_best = |t| {
            let p = run(&dict, history, "たべる", 1, oversample, t);
            (p[0].surface_key(), p[0].viterbi_cost)
        };
        let reference = one_best(DEFAULT_CANDIDATE_MAX_COST_GAP);
        assert_eq!(one_best(0), reference);
        assert_eq!(one_best(i64::MAX), reference);
        let nbest = run(
            &dict,
            history,
            "たべる",
            20,
            nbest_oversample(20, history.is_some()),
            0,
        );
        assert_eq!(nbest[0].surface_key(), reference.0, "1-best == N-best[0]");
    }
}

#[test]
fn katakana_is_priced_from_the_admitted_worst() {
    // Declared change: worst-priced Override candidates follow the list
    // admission kept, so they move with T.
    let dict = taberu();
    let katakana = |t| {
        let paths = run(&dict, None, "たべる", 20, 60, t);
        let worst = paths
            .iter()
            .filter(|p| !matches!(p.origin, PathOrigin::Katakana | PathOrigin::Numeric))
            .map(ScoredPath::pre_history_cost)
            .max()
            .unwrap();
        let k = paths
            .iter()
            .find(|p| p.origin == PathOrigin::Katakana)
            .unwrap();
        (k.viterbi_cost, worst)
    };
    let (bounded, bounded_worst) = katakana(0);
    let (unbounded, unbounded_worst) = katakana(i64::MAX);
    assert_eq!(bounded, bounded_worst + 10000);
    assert_eq!(unbounded, unbounded_worst + 10000);
    assert!(bounded < unbounded, "fixture: admission lowered the worst");
}

#[test]
fn number_candidates_are_added_after_admission() {
    let dict = TrieDictionary::from_entries(vec![("さん".into(), vec![e("三", 0), e("産", 2000)])]);
    let paths = keys(&run(&dict, None, "にじゅうさん", 20, 60, 0));
    assert!(paths.contains(&"二十三".to_string()), "{paths:?}");
    assert!(paths.contains(&"ニジュウサン".to_string()), "{paths:?}");
}

#[test]
fn whole_path_boost_is_the_whole_part_of_the_history_boost() {
    let dict = taberu();
    let mut h = UserHistory::new();
    h.record(&[("たべる".into(), "田辺留".into())]);
    h.record(&[("た".into(), "田".into()), ("べる".into(), "辺留".into())]);
    let paths = run(&dict, Some(&h), "たべる", 20, 60, i64::MAX);
    for p in &paths {
        assert!(
            0 <= p.whole_path_boost && p.whole_path_boost <= p.history_boost,
            "{p:?}"
        );
    }
    let far = paths.iter().find(|p| p.surface_key() == "田辺留").unwrap();
    assert!(far.whole_path_boost > 0);
    let best = paths.iter().find(|p| p.surface_key() == "食べる").unwrap();
    assert_eq!(
        best.whole_path_boost, 0,
        "no whole pair, no whole-path boost"
    );
}

/// taberu plus kana nodes, so Partial offers kana spellings of the far
/// fragments (た|辺留): offers priced at or above their source.
fn taberu_with_kana() -> TrieDictionary {
    TrieDictionary::from_entries(vec![
        ("たべる".into(), vec![e("食べる", 0)]),
        ("た".into(), vec![e("田", 3000), e("た", 4000)]),
        ("べる".into(), vec![e("辺留", 3000), e("べる", 4000)]),
    ])
}

#[test]
fn a_dropped_source_takes_its_offers_and_a_learned_offer_stays() {
    let dict = taberu_with_kana();
    let offers = |paths: &[ScoredPath]| -> Vec<String> {
        paths
            .iter()
            .filter(|p| p.origin == PathOrigin::PartialHiragana)
            .map(ScoredPath::surface_key)
            .collect()
    };
    // Oversample 2: the population is 食べる and 田|辺留, so た|辺留 and
    // 田|べる exist only as offers of 田|辺留.
    let unbounded = run(&dict, None, "たべる", 20, 2, i64::MAX);
    let offered = offers(&unbounded);
    assert!(
        offered.contains(&"た辺留".to_string()),
        "fixture: {offered:?}"
    );
    // At the floor the far fragments go, and their offers with them.
    let bounded = run(&dict, None, "たべる", 20, 2, 0);
    assert!(offers(&bounded).is_empty(), "{:?}", keys(&bounded));
    // The spelling the user committed stays though its source goes.
    let mut h = UserHistory::new();
    for _ in 0..3 {
        h.record(&[("たべる".into(), "食べる".into())]);
    }
    h.record(&[("たべる".into(), "た辺留".into())]);
    let learned = keys(&run(&dict, Some(&h), "たべる", 20, 2, 0));
    assert_eq!(learned[0], "食べる");
    assert!(learned.contains(&"た辺留".to_string()), "{learned:?}");
}

#[test]
fn the_pre_history_best_is_restored_at_the_floor() {
    // Per-segment learning lifts 田|辺留 to #1; the pre-history best
    // (gap 0) survives admission and K4 puts it back at index 1.
    let dict = taberu();
    let mut h = UserHistory::new();
    for _ in 0..10 {
        h.record(&[("た".into(), "田".into()), ("べる".into(), "辺留".into())]);
    }
    let paths = keys(&run(&dict, Some(&h), "たべる", 2, 50, 0));
    assert_eq!(&paths[..2], ["田辺留", "食べる"], "{paths:?}");
}

/// PR-G: a fragment that per-segment boosts had put first, displaced by a
/// learned whole-pair path, is judged by its pre-history gap at index 1 and
/// dropped when it is over the bound (§24 (r)). Through the pipeline:
/// without the learned path the fragment keeps index 0 unconditionally.
#[test]
fn displaced_fragment_top_is_dropped_by_admission() {
    let entries = vec![
        ("たべる".to_string(), vec![e("食べる", 0), e("旅留", 30000)]),
        ("た".to_string(), vec![e("田", 3000)]),
        ("べる".to_string(), vec![e("辺留", 3000)]),
    ];
    let dict = TrieDictionary::from_entries(entries);
    let mut h = UserHistory::new();
    for _ in 0..10 {
        h.record(&[("た".into(), "田".into()), ("べる".into(), "辺留".into())]);
    }
    // Fixture: per-segment learning alone puts the fragment first, unlearned
    // and 6000 above the pre-history best (bound 4000, the rescue floor).
    let alone = run(&dict, Some(&h), "たべる", 20, 20, 0);
    assert_eq!(alone[0].surface_key(), "田辺留");
    assert_eq!(alone[0].whole_path_boost, 0);
    assert!(alone[0].pre_history_cost() > alone[1].pre_history_cost() + 4000);

    // A whole-pair learned surface far above both takes index 0; the
    // displaced fragment is no longer exempt and goes, the pre-history
    // #1 (gap 0) stays.
    h.record(&[("たべる".into(), "旅留".into())]);
    let paths = keys(&run(&dict, Some(&h), "たべる", 20, 20, 0));
    assert_eq!(paths[0], "旅留", "{paths:?}");
    assert!(paths.contains(&"食べる".to_string()), "{paths:?}");
    assert!(!paths.contains(&"田辺留".to_string()), "{paths:?}");
}

/// PR-G: with a learned whole-pair path at index 0 and the pre-history #1
/// pushed out of the n by per-segment boosts, the pre-history #1 comes back
/// at index 1 (K4), below the learned path.
#[test]
fn viterbi_best_reinserted_below_learned_top() {
    let entries = vec![
        ("きがし".to_string(), vec![e("気がし", 8000)]),
        (
            "き".to_string(),
            vec![
                e("機", 0),
                e("木", 500),
                e("黄", 1000),
                e("基", 1500),
                e("樹", 2000),
                e("記", 2500),
                e("鬼", 30000),
            ],
        ),
        ("がし".to_string(), vec![e("がし", 2000)]),
        ("ます".to_string(), vec![e("ます", 2000)]),
    ];
    let dict = TrieDictionary::from_entries(entries);
    let mut h = UserHistory::new();
    for _ in 0..5 {
        for s in ["機", "木", "黄", "基", "樹", "記"] {
            h.record(&[("き".into(), s.into())]);
        }
    }
    // Fixture: the boosted fragments fill the n and push the pre-history #1
    // (気がします) out; with nothing learned as a whole it is re-inserted at 1.
    let before = keys(&run(&dict, Some(&h), "きがします", 3, 50, i64::MAX));
    assert_eq!(before[1], "気がします", "{before:?}");
    assert_ne!(before[0], "気がします", "{before:?}");

    // 鬼がします, far above everything, is learned as a whole: it takes
    // index 0 and the pre-history #1 is still put back right below it.
    h.record(&[("きがします".into(), "鬼がします".into())]);
    let after = run(&dict, Some(&h), "きがします", 3, 50, i64::MAX);
    let after = keys(&after);
    assert_eq!(after[0], "鬼がします", "{after:?}");
    assert_eq!(after[1], "気がします", "{after:?}");
}
