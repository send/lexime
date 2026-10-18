//! The two widths the user sees, side by side: the synchronous 1-best and the
//! candidate list's #1 (`widths`, behind `lextool widths`).
//!
//! Both are computed through the production pipelines (the 1-best is
//! `convert_from_lattice`'s population, the list is `generate_candidates`'s),
//! with `now` passed in per width so a census can pin it and a sensitivity
//! run can move one of them by a second. Nothing here reads the wall clock.
//! The output carries what telling a split's cause apart takes: each width's
//! population and top path (origin, price owner, history boost, anchor,
//! Viterbi rank), the cost model's path set of each, and where the list's #1
//! came from (`standard_top`).

use serde::Serialize;

use crate::converter::explain::{explain, ExplainPath, ExplainResult, ModelPath};
use crate::converter::{population, ConversionContext, PathOrigin};
use crate::dict::connection::ConnectionMatrix;
use crate::dict::Dictionary;
use crate::settings::settings;
use crate::user_history::UserHistory;

use super::{generate_candidates_priced, TopSource};

/// The top path of one width's postprocess output.
#[derive(Debug, Clone, Serialize)]
pub struct TopPath {
    pub surface: String,
    pub origin: PathOrigin,
    pub priced_by: PathOrigin,
    /// Whole-path part of the history boost (> 0: a learned surface).
    pub whole_path_boost: i64,
    /// History boost subtracted from the price.
    pub history_boost: i64,
    /// Position in `viterbi_nbest`'s output, for a path Viterbi made and the
    /// model priced.
    pub viterbi_rank: Option<usize>,
}

impl TopPath {
    fn of(p: &ExplainPath) -> Self {
        Self {
            surface: p.surface(),
            origin: p.origin,
            priced_by: p.priced_by,
            whole_path_boost: p.history_breakdown.whole_path_boost,
            history_boost: p.history_boost,
            viterbi_rank: p.viterbi_rank,
        }
    }
}

/// One width: the population it drew and what postprocess put first.
#[derive(Debug, Serialize)]
pub struct Width {
    /// Viterbi paths drawn (`population(n, h)`).
    pub population: usize,
    /// The pre-history #1's cost admission measured from.
    pub anchor: i64,
    /// Postprocess index 0 (`None` for an empty result).
    pub top: Option<TopPath>,
    /// The cost model's paths (Viterbi and resegment) after rerank, before
    /// admission: the set the top had to be in.
    pub model_paths: Vec<ModelPath>,
}

impl Width {
    fn of(r: &ExplainResult, population: usize) -> Self {
        Self {
            population,
            anchor: r.cost_gap_anchor,
            top: r.paths.first().map(TopPath::of),
            model_paths: r.model_paths.clone(),
        }
    }

    fn top_surface(&self) -> &str {
        self.top.as_ref().map_or("", |t| t.surface.as_str())
    }

    /// Whether the cost model's set of this width holds `surface`.
    pub fn has_model_path(&self, surface: &str) -> bool {
        self.model_paths.iter().any(|m| m.surface == surface)
    }
}

/// The candidate list's #1 (`surfaces[0]`) and where it came from.
#[derive(Debug, Serialize)]
pub struct StandardTop {
    pub surface: String,
    pub source: Option<TopSource>,
    /// The N-best path it is, only for `source = Path(i)` — the one case the
    /// price, origin and boost describe it.
    pub path: Option<TopPath>,
    /// For `KanaPromoted` when the kana was also an N-best path: that path,
    /// for the record. The promotion was not decided by its price.
    pub promoted_from: Option<TopPath>,
}

/// Both widths for one reading.
#[derive(Debug, Serialize)]
pub struct Widths {
    pub reading: String,
    pub now_1best: u64,
    pub now_list: u64,
    pub has_history: bool,
    /// The 1-best's width (`n = 1`).
    pub one_best: Width,
    /// The candidate list's N-best width (`n = candidates.nbest`).
    pub list: Width,
    pub standard_top: StandardTop,
    /// The 1-best is not the list's #1.
    pub split: bool,
    /// The list's postprocess #0 is in the 1-best's cost-model set (set
    /// inclusion, not Viterbi rank: the rank is not prefix-closed past EOS
    /// dedup).
    pub list_top_in_one_best_set: bool,
}

/// Compute both widths. The 1-best decays history at `now_1best`, the list at
/// `now_list`; a caller wanting the production pairing passes one value.
pub fn widths(
    dict: &dyn Dictionary,
    conn: Option<&ConnectionMatrix>,
    history: Option<&UserHistory>,
    reading: &str,
    now_1best: u64,
    now_list: u64,
) -> Widths {
    let has_history = history.is_some();
    let ctx_1best = ConversionContext::new(dict, conn).with_history(history, now_1best);
    let ctx_list = ConversionContext::new(dict, conn).with_history(history, now_list);

    let nbest = settings().candidates.nbest;
    let one_best = explain(&ctx_1best, reading, 1);
    let list = explain(&ctx_list, reading, nbest);
    let priced = generate_candidates_priced(&ctx_list, reading, settings().candidates.max_results);

    let surface = priced
        .response
        .surfaces
        .first()
        .cloned()
        .unwrap_or_default();
    let source = priced.diagnostics.top_source;
    let path_of = |i: usize| list.paths.get(i).map(TopPath::of);
    let standard_top = StandardTop {
        path: match source {
            Some(TopSource::Path(i)) => path_of(i),
            _ => None,
        },
        promoted_from: match source {
            Some(TopSource::KanaPromoted { .. }) => list
                .paths
                .iter()
                .find(|p| p.surface() == reading)
                .map(TopPath::of),
            _ => None,
        },
        surface,
        source,
    };

    let one_best = Width::of(&one_best, population(1, has_history));
    let list = Width::of(&list, population(nbest, has_history));
    Widths {
        reading: reading.to_string(),
        now_1best,
        now_list,
        has_history,
        split: one_best.top_surface() != standard_top.surface,
        list_top_in_one_best_set: !list.top_surface().is_empty()
            && one_best.has_model_path(list.top_surface()),
        one_best,
        list,
        standard_top,
    }
}

fn top_line(t: &Option<TopPath>) -> String {
    match t {
        None => "(none)".into(),
        Some(t) => format!(
            "{} [{:?}, priced by {:?}, whole {}, boost {}, viterbi rank {}]",
            t.surface,
            t.origin,
            t.priced_by,
            t.whole_path_boost,
            t.history_boost,
            t.viterbi_rank.map_or("-".into(), |r| r.to_string()),
        ),
    }
}

/// A short human-readable block for one reading.
pub fn format_text(w: &Widths) -> String {
    let set = |x: &Width| {
        x.model_paths
            .iter()
            .map(|m| m.surface.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    };
    let mut out = format!(
        "=== {} (now_1best {}, now_list {}, history {}) {} ===\n",
        w.reading,
        w.now_1best,
        w.now_list,
        w.has_history,
        if w.split { "SPLIT" } else { "same" },
    );
    out.push_str(&format!(
        "  1-best  (population {}, anchor {}): {}\n",
        w.one_best.population,
        w.one_best.anchor,
        top_line(&w.one_best.top),
    ));
    out.push_str(&format!(
        "  list P  (population {}, anchor {}): {}\n",
        w.list.population,
        w.list.anchor,
        top_line(&w.list.top),
    ));
    out.push_str(&format!(
        "  list #1 {} from {:?}\n",
        w.standard_top.surface, w.standard_top.source
    ));
    if let Some(p) = &w.standard_top.path {
        out.push_str(&format!("    path: {}\n", top_line(&Some(p.clone()))));
    }
    if let Some(p) = &w.standard_top.promoted_from {
        out.push_str(&format!(
            "    promoted_from: {}\n",
            top_line(&Some(p.clone()))
        ));
    }
    out.push_str(&format!(
        "  list top in 1-best set: {}\n  1-best set: {}\n  list set:   {}\n",
        w.list_top_in_one_best_set,
        set(&w.one_best),
        set(&w.list),
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::converter::testutil::{
        deep_argmin_kanaya, entry_with_id, half, test_dict, STALE, T0,
    };
    use crate::dict::TrieDictionary;
    use crate::user_history::{now_epoch, UserHistory};

    fn learned(pairs: &[(&str, &str)]) -> UserHistory {
        let mut h = UserHistory::new();
        let segs: Vec<(String, String)> = pairs
            .iter()
            .map(|&(r, s)| (r.to_string(), s.to_string()))
            .collect();
        h.record_at(&segs, T0);
        h
    }

    /// T-A5 (1)(2): each width decays at the `now` it is handed, and the
    /// report carries both. The history was last used at `T0`, ahead of the
    /// wall clock, so a width that read the clock would see the boost whole
    /// (3000 x 5) and match neither value below.
    #[test]
    fn each_width_decays_at_its_own_injected_now() {
        let dict = test_dict();
        let h = learned(&[("きょう", "京")]);
        let w = widths(&dict, None, Some(&h), "きょう", half(), STALE);
        assert_eq!((w.now_1best, w.now_list), (half(), STALE));
        let wall = now_epoch();
        assert!(wall < T0, "fixture: T0 lies ahead of the clock");

        // 1-best at one half-life: boost 3000 / 2, a whole path x 5.
        let one = w.one_best.top.as_ref().unwrap();
        assert_eq!(one.surface, "京");
        assert_eq!(one.whole_path_boost, 5 * 1500);
        // List at STALE: decayed to 0, the unlearned 今日 leads.
        let list = w.list.top.as_ref().unwrap();
        assert_eq!(list.surface, "今日");
        assert_eq!((list.whole_path_boost, list.history_boost), (0, 0));
        assert_eq!(w.standard_top.surface, "今日");
        assert!(w.split, "the two nows split this reading");

        // The same pair swapped is the mirror image.
        let w = widths(&dict, None, Some(&h), "きょう", STALE, half());
        assert_eq!(w.one_best.top.as_ref().unwrap().surface, "今日");
        assert_eq!(w.standard_top.surface, "京");
        assert_eq!(
            w.standard_top.path.as_ref().unwrap().whole_path_boost,
            5 * 1500
        );
        // One value for both is no split.
        assert!(!widths(&dict, None, Some(&h), "きょう", half(), half()).split);
    }

    /// T-A5 (3): a second past the boundary where the rounded boost reaches 0
    /// shows up only when the list is read at `now + 1`. The boundary is
    /// located on the history itself, so the fixture needs no constant for
    /// the half-life.
    #[test]
    fn a_decay_rounding_boundary_splits_only_at_now_plus_one() {
        let dict = test_dict();
        let h = learned(&[("きょう", "京")]);
        let boost = |t| h.unigram_boost("きょう", "京", t);
        let (mut lo, mut hi) = (T0, STALE);
        assert!(boost(lo) > 0 && boost(hi) == 0);
        while hi - lo > 1 {
            let mid = lo + (hi - lo) / 2;
            if boost(mid) > 0 {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        // `lo` still holds a boost of 1, `lo + 1` rounds to 0.
        assert_eq!((boost(lo), boost(lo + 1)), (1, 0));

        let same = widths(&dict, None, Some(&h), "きょう", lo, lo);
        assert!(!same.split);
        assert_eq!(same.standard_top.surface, "京");
        let shifted = widths(&dict, None, Some(&h), "きょう", lo, lo + 1);
        assert!(shifted.split);
        assert_eq!(shifted.one_best.top.as_ref().unwrap().surface, "京");
        assert_eq!(shifted.standard_top.surface, "今日");
    }

    /// T-A5 (3): the inclusion test is on sets. Every path ends at the one
    /// `や` node, whose K-best list holds the same prefix surface twice (one
    /// entry for かな, or か + な), so the 1-best's ten paths are five distinct
    /// surfaces while the list's sixty reach 金谷や, 8th in Viterbi order and
    /// rerank's argmin over the penalised katakana spellings. A rank < 10
    /// test would call it inside the 1-best's population; the set says it is
    /// not.
    #[test]
    fn set_inclusion_survives_the_eos_surface_dedup() {
        let e = entry_with_id;
        let mut whole = Vec::new();
        for i in 0..7 {
            // The same surface from two nodes (POS ids 1 and 2), one cost
            // apart: both reach the one `や` node, whose K-best list then
            // holds each surface twice.
            let surface = format!("カナ{}", "ア".repeat(i));
            whole.push(e(&surface, (i as i16) * 10, 1));
            whole.push(e(&surface, (i as i16) * 10 + 1, 2));
        }
        whole.push(e("金谷", 700, 1));
        let dict = TrieDictionary::from_entries(vec![
            ("かな".into(), whole),
            ("や".into(), vec![e("や", 0, 1)]),
        ]);
        let conn =
            crate::dict::connection::ConnectionMatrix::new_owned(3, 0, 0, Vec::new(), vec![0; 9]);
        let w = widths(&dict, Some(&conn), None, "かなや", T0, T0);
        let top = w.list.top.as_ref().unwrap();
        assert_eq!(top.surface, "金谷や");
        let rank = top.viterbi_rank.unwrap();
        assert!(rank < w.one_best.population, "rank {rank} is inside K1");
        assert!(!w.list_top_in_one_best_set, "but the 1-best's set lacks it");
        assert!(w.split);
        assert!(w.list.has_model_path("金谷や") && !w.one_best.has_model_path("金谷や"));
    }

    /// T-A5 (3): the same reading, shallow enough for the 1-best's
    /// population: the set holds the list's top and nothing splits.
    #[test]
    fn a_list_top_inside_the_one_best_population_is_in_its_set() {
        let (dict, conn) = deep_argmin_kanaya(5);
        let w = widths(&dict, Some(&conn), None, "かなや", T0, T0);
        assert!(w.list_top_in_one_best_set && !w.split);
        let (dict, conn) = deep_argmin_kanaya(12);
        let w = widths(&dict, Some(&conn), None, "かなや", T0, T0);
        assert!(!w.list_top_in_one_best_set && w.split);
        assert_eq!(w.one_best.population, 10);
        assert_eq!(w.list.population, 60);
    }

    /// T-A5 (3): a learned kana is promoted over the list's #1. The record
    /// names the stage; origin, price owner and boost belong to a path, so
    /// they are null.
    #[test]
    fn a_promoted_kana_carries_no_path_fields() {
        let dict = test_dict();
        let h = learned(&[("きょう", "きょう")]);
        let w = widths(&dict, None, Some(&h), "きょう", T0, T0);
        assert_eq!(w.standard_top.surface, "きょう");
        assert!(matches!(
            w.standard_top.source,
            Some(TopSource::KanaPromoted {
                top_has_boost: false,
                ..
            })
        ));
        assert!(w.standard_top.path.is_none());

        // A #1 with a history of its own keeps index 0: no promotion.
        let h = learned(&[("きょう", "京")]);
        let w = widths(&dict, None, Some(&h), "きょう", T0, T0);
        assert_eq!(w.standard_top.source, Some(TopSource::Path(0)));
        assert_eq!(w.standard_top.path.as_ref().unwrap().surface, "京");
    }

    /// The 1-best and the list's #1 are the production ones: the same
    /// strings `convert` and `generate_candidates` return.
    #[test]
    fn the_widths_are_the_production_outputs() {
        let dict = test_dict();
        let h = learned(&[("きょう", "京")]);
        for history in [None, Some(&h)] {
            let ctx = ConversionContext::new(&dict, None).with_history(history, half());
            let w = widths(&dict, None, history, "きょう", half(), half());
            assert_eq!(
                w.one_best.top.as_ref().unwrap().surface,
                ctx.convert("きょう")
                    .iter()
                    .map(|s| s.surface.as_str())
                    .collect::<String>()
            );
            assert_eq!(
                w.standard_top.surface,
                super::super::generate_candidates(&ctx, "きょう", 20).surfaces[0]
            );
        }
    }
}
