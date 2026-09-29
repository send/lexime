use crate::converter::lattice::Lattice;
use crate::converter::rewriter::{
    run_rewriters, HiraganaVariantRewriter, KatakanaRewriter, NumericRewriter,
    PartialHiraganaRewriter, RewriteStage, Rewriter,
};
use crate::converter::viterbi::{PathOrigin, RichSegment, ScoredPath};

#[test]
fn test_run_rewriters_applies_all() {
    let rw = KatakanaRewriter;
    let mut paths = vec![ScoredPath::new(
        vec![RichSegment {
            reading: "あ".into(),
            surface: "亜".into(),
            left_id: 0,
            right_id: 0,
            word_cost: 0,
        }],
        1000,
        PathOrigin::Viterbi,
    )];

    run_rewriters(&[&rw], &mut paths, "あ", RewriteStage::Override, None);

    assert_eq!(paths.len(), 2);
    // Katakana has higher cost, so inserted after 亜
    assert_eq!(paths[0].surface_key(), "亜");
    assert_eq!(paths[1].surface_key(), "ア");
}

/// `去れ|ます` with a kana node され, so the rescue (best+4000) and the
/// Partial offer (≤ src+2000) both produce されます.
fn saremasu() -> (Lattice, ScoredPath) {
    let lattice = Lattice::from_test_nodes("されます", &[(0, 2, "され", "され", 0, 1, 1)]);
    let best = ScoredPath::new(
        vec![
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
        1000,
        PathOrigin::Viterbi,
    );
    (lattice, best)
}

#[test]
fn test_run_rewriters_dedup_across_rewriters() {
    // HiraganaVariant and PartialHiragana both produce されます. One copy
    // remains, whichever runs first: the offer's price (≤ source + 2000)
    // beats the rescue's (best+4000) and the Partial's two segments replace
    // the rescue's single synthetic one. It stays marked as the rescue
    // (#263: what keeps the surface on the list).
    let (lattice, best) = saremasu();
    let pricer = super::pricer();
    let hiragana_rw = HiraganaVariantRewriter;
    let partial_rw = PartialHiraganaRewriter {
        lattice: &lattice,
        conn: None,
        pricer: &pricer,
    };
    let orders: [[&dyn Rewriter; 2]; 2] =
        [[&hiragana_rw, &partial_rw], [&partial_rw, &hiragana_rw]];
    for order in orders {
        let mut paths = vec![best.clone()];
        run_rewriters(&order, &mut paths, "されます", RewriteStage::Model, None);
        let rescue: Vec<_> = paths
            .iter()
            .filter(|p| p.surface_key() == "されます")
            .collect();
        assert_eq!(
            rescue.len(),
            1,
            "dedup should prevent duplicate across rewriters"
        );
        assert_eq!(
            rescue[0].viterbi_cost, 3000,
            "the offer, capped at source + 2000"
        );
        assert_eq!(rescue[0].priced_by, PathOrigin::HiraganaVariant);
        assert_eq!(rescue[0].origin, PathOrigin::PartialHiragana);
        assert_eq!(rescue[0].segments.len(), 2, "per-segment history (#271)");
    }
}

#[test]
fn test_run_rewriters_cost_ordered_insertion() {
    // Compound kanji (best_cost) should be inserted at position 0
    let rw = NumericRewriter {
        lattice: None,
        connection: None,
        anchor: 3000,
    };
    let mut paths = vec![ScoredPath::new(
        vec![RichSegment {
            reading: "にじゅうさん".into(),
            surface: "に十三".into(),
            left_id: 10,
            right_id: 10,
            word_cost: 0,
        }],
        3000,
        PathOrigin::Viterbi,
    )];

    run_rewriters(
        &[&rw],
        &mut paths,
        "にじゅうさん",
        RewriteStage::Override,
        None,
    );

    assert_eq!(paths[0].surface_key(), "二十三");
    assert_eq!(paths[0].viterbi_cost, 3000); // best_cost = 3000
    assert_eq!(paths[1].surface_key(), "に十三");
}

// ---------------------------------------------------------------------------
// Duplicate resolution and insertion (resolve_duplicate / insert_by_cost)
// ---------------------------------------------------------------------------

fn path(surface: &str, cost: i64, origin: PathOrigin) -> ScoredPath {
    ScoredPath::new(
        vec![RichSegment {
            reading: "よみ".into(),
            surface: surface.into(),
            left_id: 0,
            right_id: 0,
            word_cost: 0,
        }],
        cost,
        origin,
    )
}

fn seg(reading: &str, surface: &str) -> RichSegment {
    RichSegment {
        reading: reading.into(),
        surface: surface.into(),
        left_id: 0,
        right_id: 0,
        word_cost: 0,
    }
}

/// Emits fixed candidates, ignoring the list it is given.
struct Fixed(Vec<ScoredPath>);

impl Rewriter for Fixed {
    fn generate(&self, _paths: &[ScoredPath], _reading: &str) -> Vec<ScoredPath> {
        self.0.clone()
    }
}

fn surfaces(paths: &[ScoredPath]) -> Vec<String> {
    paths.iter().map(|p| p.surface_key()).collect()
}

#[test]
fn model_stage_never_touches_index_zero() {
    let mut paths = vec![
        path("一", 1000, PathOrigin::Viterbi),
        path("二", 2000, PathOrigin::Viterbi),
    ];
    let rw = Fixed(vec![
        path("安", 10, PathOrigin::KanjiVariant),
        path("一", 5, PathOrigin::KanjiVariant),
    ]);
    run_rewriters(&[&rw], &mut paths, "よみ", RewriteStage::Model, None);
    assert_eq!(surfaces(&paths), ["一", "安", "二"]);
    assert_eq!(paths[0].viterbi_cost, 1000, "index 0 is not repriced");
}

#[test]
fn override_stage_may_take_index_zero() {
    let mut paths = vec![path("一", 1000, PathOrigin::Viterbi)];
    let rw = Fixed(vec![path("数", 500, PathOrigin::Numeric)]);
    run_rewriters(&[&rw], &mut paths, "よみ", RewriteStage::Override, None);
    assert_eq!(surfaces(&paths), ["数", "一"]);
}

#[test]
fn cheaper_offer_reprices_a_listed_path_and_keeps_its_segments() {
    let mut paths = vec![
        path("一", 1000, PathOrigin::Viterbi),
        path("同", 5000, PathOrigin::Viterbi),
    ];
    let rw = Fixed(vec![path("同", 3000, PathOrigin::KanjiVariant)]);
    run_rewriters(&[&rw], &mut paths, "よみ", RewriteStage::Model, None);
    let same = &paths[1];
    assert_eq!(same.viterbi_cost, 3000);
    assert_eq!(same.priced_by, PathOrigin::KanjiVariant);
    assert_eq!(
        same.origin,
        PathOrigin::Viterbi,
        "both lattice: the listed segments stay"
    );
}

#[test]
fn dearer_candidate_leaves_a_lattice_path_alone() {
    let mut paths = vec![
        path("一", 1000, PathOrigin::Viterbi),
        path("同", 5000, PathOrigin::Viterbi),
    ];
    let rw = Fixed(vec![path("同", 7000, PathOrigin::PartialHiragana)]);
    run_rewriters(&[&rw], &mut paths, "よみ", RewriteStage::Model, None);
    assert_eq!(paths[1].priced_by, PathOrigin::Viterbi);
    assert_eq!(paths[1].viterbi_cost, 5000);
}

#[test]
fn lattice_segments_replace_a_synthetic_path_even_when_dearer() {
    // One segment each, so the lattice side wins by being lattice nodes,
    // not by being finer.
    let mut paths = vec![
        path("一", 1000, PathOrigin::Viterbi),
        path("同", 5000, PathOrigin::HiraganaVariant),
    ];
    let lattice = ScoredPath::new(
        vec![RichSegment {
            reading: "よみ".into(),
            surface: "同".into(),
            left_id: 7,
            right_id: 7,
            word_cost: 0,
        }],
        6000,
        PathOrigin::KanjiVariant,
    );
    run_rewriters(
        &[&Fixed(vec![lattice])],
        &mut paths,
        "よみ",
        RewriteStage::Model,
        None,
    );
    let same = &paths[1];
    assert_eq!(same.viterbi_cost, 5000, "the cheaper price stays");
    assert_eq!(same.priced_by, PathOrigin::HiraganaVariant);
    assert_eq!(same.origin, PathOrigin::KanjiVariant);
    assert_eq!(same.segments[0].left_id, 7, "the lattice node's segment");
}

#[test]
fn kana_rescue_adopts_its_price_on_a_model_path() {
    let multi = ScoredPath::new(
        vec![
            RichSegment {
                reading: "りだいれくと".into(),
                surface: "リダイレクト".into(),
                left_id: 0,
                right_id: 0,
                word_cost: 0,
            },
            RichSegment {
                reading: "される".into(),
                surface: "される".into(),
                left_id: 0,
                right_id: 0,
                word_cost: 0,
            },
        ],
        9000,
        PathOrigin::Viterbi,
    );
    let mut paths = vec![path("一", 1000, PathOrigin::Viterbi), multi];
    let rescue = ScoredPath::single(
        "りだいれくとされる".into(),
        "リダイレクトされる".into(),
        5000,
        PathOrigin::HiraganaVariant,
    );
    run_rewriters(
        &[&Fixed(vec![rescue])],
        &mut paths,
        "よみ",
        RewriteStage::Model,
        None,
    );
    let hit = &paths[1];
    assert_eq!(hit.viterbi_cost, 5000);
    assert_eq!(hit.priced_by, PathOrigin::HiraganaVariant);
    assert_eq!(hit.segments.len(), 2, "lattice segments kept (#271)");
}

/// An Override price the stage adopts is decided before history (policy vs
/// model price) and the path is boosted again on its own segments.
#[test]
fn override_price_adoption_is_boosted_again() {
    let boost = |p: &mut ScoredPath| {
        if p.surface_key() == "十円" {
            p.viterbi_cost -= 100;
            p.history_boost = 100;
            p.whole_path_boost = 60;
        }
    };
    let mut learned = path("十円", 3000, PathOrigin::Viterbi);
    boost(&mut learned);
    let mut paths = vec![path("一", 1000, PathOrigin::Viterbi), learned];
    let rw = Fixed(vec![path("十円", 500, PathOrigin::Numeric)]);
    run_rewriters(
        &[&rw],
        &mut paths,
        "よみ",
        RewriteStage::Override,
        Some(&boost),
    );
    let n = &paths[0];
    assert_eq!(n.surface_key(), "十円", "the learned path takes index 0");
    assert_eq!(n.priced_by, PathOrigin::Numeric);
    assert_eq!(
        (n.viterbi_cost, n.history_boost, n.whole_path_boost),
        (400, 100, 60)
    );
}

/// Who sets the price is decided before history: a candidate whose boost is
/// larger only because it has fewer segments does not raise the path.
#[test]
fn override_price_is_chosen_before_history() {
    // Per-segment boost only (not learned, so nothing is frozen): 600 over
    // the path's segment count, so one segment earns 600 and two earn 300.
    let boost = |p: &mut ScoredPath| {
        let b = 600 / p.segments.len() as i64;
        p.viterbi_cost -= b;
        p.history_boost = b;
    };
    let mut existing = ScoredPath::new(
        vec![seg("てれび", "テレビ"), seg("げーむ", "ゲーム")],
        2000,
        PathOrigin::Viterbi,
    );
    boost(&mut existing);
    let mut paths = vec![existing];
    // Pre-history 2100 > 2000, but boosted 1500 < 1700.
    let rw = Fixed(vec![ScoredPath::single(
        "てれびげーむ".into(),
        "テレビゲーム".into(),
        2100,
        PathOrigin::Katakana,
    )]);
    run_rewriters(
        &[&rw],
        &mut paths,
        "てれびげーむ",
        RewriteStage::Override,
        Some(&boost),
    );
    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0].priced_by, PathOrigin::Viterbi);
    assert_eq!(paths[0].viterbi_cost, 1700);
}

/// A learned candidate cheaper than the learned index 0 takes index 0; the
/// one it replaces goes back to its price position, not to index 1.
#[test]
fn learned_candidate_undercutting_learned_top_demotes_it_to_its_price() {
    let boost = |p: &mut ScoredPath| {
        if p.surface_key() == "二十" {
            p.whole_path_boost = 1;
        }
    };
    let mut top = path("荷重", 9000, PathOrigin::Viterbi);
    top.whole_path_boost = 1;
    let mut paths = vec![top, path("二重", 3000, PathOrigin::Viterbi)];
    let rw = Fixed(vec![path("二十", 2500, PathOrigin::Numeric)]);
    run_rewriters(
        &[&rw],
        &mut paths,
        "にじゅう",
        RewriteStage::Override,
        Some(&boost),
    );
    assert_eq!(surfaces(&paths), ["二十", "二重", "荷重"]);
}

#[test]
fn insertion_into_empty_or_unsorted_lists() {
    let mut paths = Vec::new();
    run_rewriters(
        &[&Fixed(vec![path("甲", 5, PathOrigin::KanjiVariant)])],
        &mut paths,
        "よみ",
        RewriteStage::Model,
        None,
    );
    assert_eq!(surfaces(&paths), ["甲"]);

    // Index 1 holds a re-inserted, dearer Viterbi best.
    let mut paths = vec![
        path("学", 100, PathOrigin::Viterbi),
        path("最", 9000, PathOrigin::Viterbi),
        path("三", 3000, PathOrigin::Viterbi),
    ];
    run_rewriters(
        &[&Fixed(vec![path("新", 2000, PathOrigin::KanjiVariant)])],
        &mut paths,
        "よみ",
        RewriteStage::Model,
        None,
    );
    assert_eq!(surfaces(&paths), ["学", "新", "最", "三"]);
}

#[test]
fn ties_follow_in_the_model_stage_and_lead_in_the_override_stage() {
    let base = || {
        vec![
            path("一", 1000, PathOrigin::Viterbi),
            path("源", 3000, PathOrigin::Viterbi),
        ]
    };
    // An offer at its source's price stays below the source.
    let mut paths = base();
    let rw = Fixed(vec![path("変", 3000, PathOrigin::KanjiVariant)]);
    run_rewriters(&[&rw], &mut paths, "よみ", RewriteStage::Model, None);
    assert_eq!(surfaces(&paths), ["一", "源", "変"]);
    // A policy candidate leads its tie (a number compound at the anchor).
    let mut paths = base();
    let rw = Fixed(vec![path("数", 3000, PathOrigin::Numeric)]);
    run_rewriters(&[&rw], &mut paths, "よみ", RewriteStage::Override, None);
    assert_eq!(surfaces(&paths), ["一", "数", "源"]);
}

/// Emits one candidate per path it is given, so its output counts its input.
struct Echo;

impl Rewriter for Echo {
    fn generate(&self, paths: &[ScoredPath], _reading: &str) -> Vec<ScoredPath> {
        paths
            .iter()
            .map(|p| {
                path(
                    &format!("{}'", p.surface_key()),
                    p.viterbi_cost + 1,
                    PathOrigin::KanjiVariant,
                )
            })
            .collect()
    }
}

#[test]
fn rewriters_generate_from_the_stage_input_only() {
    let mut paths = vec![
        path("一", 1000, PathOrigin::Viterbi),
        path("二", 2000, PathOrigin::Viterbi),
    ];
    let first = Fixed(vec![path("新", 1500, PathOrigin::PartialHiragana)]);
    run_rewriters(
        &[&first, &Echo],
        &mut paths,
        "よみ",
        RewriteStage::Model,
        None,
    );
    // Echo saw 一 and 二, not 新.
    assert_eq!(surfaces(&paths), ["一", "一'", "新", "二", "二'"]);
}

#[test]
fn a_kanji_variant_is_not_sourced_from_a_path_partial_repriced() {
    // 下|方|くる (1000) and した|方|くる (9000). Partial offers した|方|くる
    // from the best at ≤ 3000, which reprices the listed path; sourcing 来る
    // from that price would offer した|方|来る at ≤ 5000, far under its own
    // source's 9000.
    let lattice = Lattice::from_test_nodes(
        "したほうくる",
        &[
            (0, 2, "した", "した", 0, 0, 0),
            (4, 6, "くる", "来る", 0, 0, 0),
        ],
    );
    let s = |reading: &str, surface: &str, word_cost: i16| RichSegment {
        reading: reading.into(),
        surface: surface.into(),
        left_id: 0,
        right_id: 0,
        word_cost,
    };
    let mut paths = vec![
        ScoredPath::new(
            vec![s("した", "下", 0), s("ほう", "方", 0), s("くる", "くる", 0)],
            1000,
            PathOrigin::Viterbi,
        ),
        ScoredPath::new(
            vec![
                s("した", "した", 8000),
                s("ほう", "方", 0),
                s("くる", "くる", 0),
            ],
            9000,
            PathOrigin::Viterbi,
        ),
    ];
    let pricer = crate::converter::reranker::FeaturePricer::new(None, None);
    let partial = PartialHiraganaRewriter {
        lattice: &lattice,
        conn: None,
        pricer: &pricer,
    };
    let kanji = crate::converter::rewriter::KanjiVariantRewriter {
        lattice: &lattice,
        conn: None,
        pricer: &pricer,
    };
    run_rewriters(
        &[&partial, &kanji],
        &mut paths,
        "したほうくる",
        RewriteStage::Model,
        None,
    );
    let repriced = paths
        .iter()
        .find(|p| p.surface_key() == "したほうくる")
        .unwrap();
    assert_eq!(
        repriced.priced_by,
        PathOrigin::PartialHiragana,
        "fixture: Partial reprices it"
    );
    let chained = paths
        .iter()
        .find(|p| p.surface_key() == "した方来る")
        .unwrap();
    assert!(
        chained.viterbi_cost >= 9000,
        "priced from its own source: {}",
        chained.viterbi_cost
    );
}

#[test]
fn the_rescue_mark_does_not_depend_on_rewriter_order() {
    // A model path already spells the rescue's surface (3000). The rescue
    // (4000) does not undercut it; a Partial offer (2000) does. Whichever
    // runs first, the surface ends priced by the offer and marked as the
    // rescue (#263), in the same place.
    let base = || {
        vec![
            path("最", 0, PathOrigin::Viterbi),
            path("かな", 3000, PathOrigin::Viterbi),
            path("他", 2000, PathOrigin::Viterbi),
        ]
    };
    let rescue = Fixed(vec![path("かな", 4000, PathOrigin::HiraganaVariant)]);
    let partial = Fixed(vec![path("かな", 2000, PathOrigin::PartialHiragana)]);
    let orders: [[&dyn Rewriter; 2]; 2] = [[&rescue, &partial], [&partial, &rescue]];
    let mut results = Vec::new();
    for order in orders {
        let mut paths = base();
        run_rewriters(&order, &mut paths, "よみ", RewriteStage::Model, None);
        let kana = paths.iter().find(|p| p.surface_key() == "かな").unwrap();
        assert_eq!(kana.priced_by, PathOrigin::HiraganaVariant);
        assert_eq!(kana.viterbi_cost, 2000);
        results.push(surfaces(&paths));
    }
    assert_eq!(results[0], results[1]);
}

#[test]
fn a_model_price_is_not_marked_as_the_rescue() {
    let mut paths = vec![
        path("最", 0, PathOrigin::Viterbi),
        path("かな", 3000, PathOrigin::Viterbi),
    ];
    let rescue = Fixed(vec![path("かな", 4000, PathOrigin::HiraganaVariant)]);
    run_rewriters(&[&rescue], &mut paths, "よみ", RewriteStage::Model, None);
    assert_eq!(paths[1].priced_by, PathOrigin::Viterbi);
}
