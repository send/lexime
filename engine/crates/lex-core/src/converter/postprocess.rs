use tracing::debug_span;

use crate::dict::connection::ConnectionMatrix;
use crate::dict::Dictionary;
use crate::user_history::UserHistory;

use super::lattice::Lattice;
use super::reranker;
use super::resegment;
use super::rewriter;
use super::viterbi::{RichSegment, ScoredPath};

// ---------------------------------------------------------------------------
// Observer trait — allows explain to capture intermediate cost snapshots
// without duplicating the pipeline.
// ---------------------------------------------------------------------------

/// Observer for the postprocess pipeline.
///
/// Default methods are no-ops. The generic parameter is monomorphized,
/// so `NoopObserver` compiles to zero overhead in production.
pub(crate) trait PostprocessObserver {
    /// Called after viterbi paths are collected, before resegment/rerank.
    fn after_viterbi(&mut self, _paths: &[ScoredPath]) {}
    /// Called after resegment + rerank + variant rewriters, before history_rerank.
    fn after_rerank(&mut self, _paths: &[ScoredPath]) {}
    /// Called for each path the structure filter drops, with its structure
    /// cost and its price's gap to the pre-history #1.
    fn dropped_by_structure(&mut self, _path: &ScoredPath, _sc: i64, _gap: i64) {}
    /// Called for each path cost-gap admission drops, with the gap's base
    /// (the pre-history #1's cost).
    fn dropped_by_cost_gap(&mut self, _path: &ScoredPath, _anchor: i64) {}
    /// Called on the final paths before `group_segments` merges morphemes
    /// into phrases, while segments are still the priced lattice nodes.
    fn before_group(&mut self, _paths: &[ScoredPath]) {}
}

/// No-op observer for production use.
pub(crate) struct NoopObserver;
impl PostprocessObserver for NoopObserver {}

// ---------------------------------------------------------------------------
// Pipeline context — groups the shared parameters
// ---------------------------------------------------------------------------

/// Shared context for the postprocess pipeline.
pub(crate) struct PostprocessContext<'a> {
    pub lattice: &'a Lattice,
    pub conn: Option<&'a ConnectionMatrix>,
    pub dict: Option<&'a dyn Dictionary>,
    pub history: Option<&'a UserHistory>,
    pub kana: &'a str,
    pub n: usize,
    /// `[candidates] max_cost_gap`: how far above the pre-history #1 a path
    /// may be priced and stay a candidate (see [`admit_by_cost_gap`]).
    pub max_cost_gap: i64,
    /// Timestamp passed to `history_rerank_at`. Pinning it here lets diagnostic
    /// observers compute breakdowns against the exact value the pipeline will
    /// use, avoiding sub-second drift across the second boundary.
    pub now: u64,
}

// ---------------------------------------------------------------------------
// Pipeline
// ---------------------------------------------------------------------------

/// Shared post-processing pipeline: resegment → rerank → hiragana / partial
/// / kanji-variant rewriters → history_rerank → cost-gap admission → n
/// model-priced paths (offers ride along) → numeric/katakana → group.
pub(super) fn postprocess(
    paths: &mut Vec<ScoredPath>,
    lattice: &Lattice,
    conn: Option<&ConnectionMatrix>,
    dict: Option<&dyn Dictionary>,
    history: Option<&UserHistory>,
    kana: &str,
    n: usize,
) -> Vec<ScoredPath> {
    let ctx = PostprocessContext {
        lattice,
        conn,
        dict,
        history,
        kana,
        n,
        max_cost_gap: crate::settings::settings().candidates.max_cost_gap,
        now: crate::user_history::now_epoch(),
    };
    postprocess_observed(paths, &ctx, &mut NoopObserver)
}

/// Post-processing pipeline with an observer for diagnostic hooks.
///
/// Returns `Vec<ScoredPath>` (before `into_segments()`) so callers like
/// `explain` can inspect the final segment-level detail.
pub(crate) fn postprocess_observed<O: PostprocessObserver>(
    paths: &mut Vec<ScoredPath>,
    ctx: &PostprocessContext<'_>,
    observer: &mut O,
) -> Vec<ScoredPath> {
    let _span = debug_span!("postprocess", n = ctx.n, paths_in = paths.len()).entered();

    observer.after_viterbi(paths);

    // Generate alternative segmentations from the lattice before reranking,
    // so the reranker can compare them on equal footing with Viterbi paths.
    let reseg_paths = resegment::resegment(paths, ctx.lattice, ctx.conn);
    paths.extend(reseg_paths);

    reranker::rerank(paths, ctx.conn, ctx.dict, |p, sc, gap| {
        observer.dropped_by_structure(p, sc, gap)
    });

    // Variants run BEFORE history_rerank, so history boosts a variant the
    // user picked like any other path (whole-path boosts ×5 can promote a
    // learned hiragana or kanji spelling), and the 1-best sees the variants
    // the N-best sees. The one exception: a 1-best without history keeps
    // index 0 by construction (the Model stage never touches it and nothing
    // re-sorts), so the offers could only be dropped at the cut.
    let hiragana_rw = rewriter::HiraganaVariantRewriter;
    let pricer = reranker::FeaturePricer::new(ctx.conn, ctx.dict);
    let partial_rw = rewriter::PartialHiraganaRewriter {
        lattice: ctx.lattice,
        conn: ctx.conn,
        pricer: &pricer,
    };
    let kanji_rw = rewriter::KanjiVariantRewriter {
        lattice: ctx.lattice,
        conn: ctx.conn,
        pricer: &pricer,
    };
    let offers_can_surface = ctx.n > 1 || ctx.history.is_some_and(|h| !h.is_empty());
    let variants: &[&dyn rewriter::Rewriter] = if offers_can_surface {
        &[&hiragana_rw, &partial_rw, &kanji_rw]
    } else {
        &[]
    };
    rewriter::run_rewriters(
        variants,
        paths,
        ctx.kana,
        rewriter::RewriteStage::Model,
        None,
    );

    observer.after_rerank(paths);

    // The rerank best before history. Its cost is what number compounds are
    // priced from and what cost-gap admission measures gaps from, whatever
    // history or later rewriters do to the list (the anchor). Its surface is kept on the list: history boosts per-segment
    // unigrams (e.g. き→機 from past "機械"), which can push fragmented
    // single-char paths above the statistically correct compound path
    // (e.g. きがし→気がし).
    let best = paths.first();
    let anchor = best.map_or(0, |p| p.viterbi_cost);
    let viterbi_best_key = best
        .filter(|_| ctx.history.is_some())
        .map(|p| p.surface_key());

    if let Some(h) = ctx.history {
        reranker::history_rerank_at(paths, h, ctx.conn, ctx.now);
    }

    admit_by_cost_gap(paths, anchor, ctx.max_cost_gap, |p| {
        observer.dropped_by_cost_gap(p, anchor)
    });

    let mut top: Vec<ScoredPath> = paths.drain(..model_budget_end(paths, ctx.n)).collect();

    // If the Viterbi #1 was pushed out of the top-n by history boosts, pull it
    // back in after the history-preferred #1, in place of the n-th model
    // path (top's last entry when the budget is full), so the offers sorted
    // before that path stay. At n = 1 there is no room: the one model path is
    // the history #1.
    if let Some(ref best_key) = viterbi_best_key {
        if ctx.n >= 2 && !top.iter().any(|p| p.surface_key_eq(best_key)) {
            if let Some(pos) = paths.iter().position(|p| p.surface_key_eq(best_key)) {
                let best = paths.remove(pos);
                if top.iter().filter(|p| p.priced_by.is_model()).count() >= ctx.n {
                    top.pop();
                }
                let insert_at = 1.min(top.len());
                top.insert(insert_at, best);
            }
        }
    }
    let numeric_rw = rewriter::NumericRewriter {
        lattice: Some(ctx.lattice),
        connection: ctx.conn,
        anchor,
    };
    let katakana_rw = rewriter::KatakanaRewriter;
    // The paths this stage creates are learned on the same terms as the
    // history-reranked ones (G7).
    let boost = ctx
        .history
        .map(|h| move |p: &mut ScoredPath| reranker::apply_history_boost(p, h, ctx.conn, ctx.now));
    rewriter::run_rewriters(
        &[&numeric_rw, &katakana_rw],
        &mut top,
        ctx.kana,
        rewriter::RewriteStage::Override,
        boost.as_ref().map(|b| b as &dyn Fn(&mut ScoredPath)),
    );
    observer.before_group(&top);
    if let Some(c) = ctx.conn {
        for path in &mut top {
            group_segments(&mut path.segments, c);
        }
    }
    top
}

/// Drop the candidates priced too far above the pre-history #1 (`anchor`):
/// every path after index 0 must cost at most `anchor + max(max_gap,
/// RESCUE_OFFSET)` before history. Kept whatever their gap: index 0 (the
/// top-1, learned or not), a surface the user has committed for this reading
/// (`whole_path_boost > 0`), and the typed kana (`is_identity`, #263/#271).
/// The floor keeps the kana rescue's band (best + 4000) however the rescue's
/// surface is priced. An offer is judged by its own price, which is never
/// below its source's, so a dropped source takes its offers with it.
/// `on_drop` sees each dropped path (explain lists them).
pub(super) fn admit_by_cost_gap(
    paths: &mut Vec<ScoredPath>,
    anchor: i64,
    max_gap: i64,
    mut on_drop: impl FnMut(&ScoredPath),
) {
    let limit = anchor.saturating_add(max_gap.max(rewriter::RESCUE_OFFSET));
    let mut first = true;
    paths.retain(|p| {
        let keep = std::mem::take(&mut first)
            || p.pre_history_cost() <= limit
            || p.is_learned()
            || p.is_identity();
        if !keep {
            on_drop(p);
        }
        keep
    });
}

/// Length of the prefix of `paths` holding its first `n` model-priced paths
/// and every offer (kana/kanji variant, kana rescue) sorted among them:
/// offers ride along and do not use up the N-best budget, so offering a
/// spelling never pushes a model path off the list.
fn model_budget_end(paths: &[ScoredPath], n: usize) -> usize {
    if n == 0 {
        return 0;
    }
    paths
        .iter()
        .enumerate()
        .filter(|(_, p)| p.priced_by.is_model())
        .nth(n - 1)
        .map_or(paths.len(), |(i, _)| i + 1)
}

/// Group morpheme-level segments into phrase-level segments (bunsetsu).
///
/// Rules:
/// - **FunctionWord / Suffix**: merge into the preceding group (same as trailing particle).
/// - **Prefix**: start a new group that absorbs the next content word.
/// - **ContentWord**: if a pending prefix exists, merge into it; otherwise start a new group.
/// - Leading function words / suffixes with no preceding group stay standalone.
pub(super) fn group_segments(segments: &mut Vec<RichSegment>, conn: &ConnectionMatrix) {
    if segments.len() <= 1 {
        return;
    }

    let mut grouped: Vec<RichSegment> = Vec::new();
    let mut current: Option<RichSegment> = None;
    let mut pending_prefix = false;

    for seg in segments.drain(..) {
        let is_fw = conn.is_function_word(seg.left_id);
        let attach_to_prev = is_fw || conn.is_suffix(seg.left_id); // FunctionWord, Suffix, or Counter

        if attach_to_prev {
            // Merge into current group if one exists
            if let Some(cur) = current.as_mut() {
                cur.reading.push_str(&seg.reading);
                cur.surface.push_str(&seg.surface);
                cur.right_id = seg.right_id;
            } else {
                // No preceding group — standalone
                grouped.push(seg);
            }
        } else if conn.is_prefix(seg.left_id) {
            // Prefix: flush current group, start new one that will absorb next CW
            if let Some(cur) = current.take() {
                grouped.push(cur);
            }
            current = Some(seg);
            pending_prefix = true;
        } else {
            // ContentWord
            if pending_prefix {
                // Merge CW into the pending prefix group
                if let Some(cur) = current.as_mut() {
                    cur.reading.push_str(&seg.reading);
                    cur.surface.push_str(&seg.surface);
                    cur.right_id = seg.right_id;
                }
                pending_prefix = false;
            } else {
                // New group
                if let Some(cur) = current.take() {
                    grouped.push(cur);
                }
                current = Some(seg);
            }
        }
    }

    if let Some(cur) = current {
        grouped.push(cur);
    }

    *segments = grouped;
}
