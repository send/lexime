use tracing::debug_span;

use crate::dict::connection::ConnectionMatrix;
use crate::dict::Dictionary;
use crate::unicode::symbol_skip;

use super::lattice::Lattice;
use super::reranker;
use super::resegment;
use super::rewriter;
use super::viterbi::{PathOrigin, RichSegment, ScoredPath};
use super::ConversionContext;

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
    /// Called right after rerank, with whether it priced the paths (it
    /// leaves a lone path alone).
    fn reranked(&mut self, _priced: bool) {}
    /// Called after the symbol placement with the symbols it inserted (none
    /// when the reading has no unlisted symbol).
    fn symbols_placed(&mut self, _placed: &[SymbolCandidate]) {}
    /// Called after resegment + rerank + variant rewriters + symbol
    /// placement, before history_rerank.
    fn after_rerank(&mut self, _paths: &[ScoredPath]) {}
    /// Called for each path cost-gap admission drops, with the gap's base
    /// (the pre-history #1's cost).
    fn dropped_by_cost_gap(&mut self, _path: &ScoredPath, _anchor: i64) {}
    /// Called when the Override stage writes a policy price onto the path
    /// with surface `key` (a new path, or one replacing a listed path with
    /// its surface).
    fn policy_priced(&mut self, _key: &str, _price: rewriter::PolicyPrice) {}
    /// Called when a policy fallback finds its surface `key` already listed
    /// and leaves the list as it is.
    fn policy_skipped(&mut self, _key: &str, _price: rewriter::PolicyPrice) {}
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

/// Shared context for the postprocess pipeline. The reading is the
/// lattice's input; `conv` carries dictionary, connection matrix and history
/// (with the `now` the pipeline and its diagnostic observers both use).
pub(crate) struct PostprocessContext<'a> {
    pub lattice: &'a Lattice,
    pub conv: &'a ConversionContext<'a>,
    pub n: usize,
    /// `[candidates] max_cost_gap`: how far above the pre-history #1 a path
    /// may be priced and stay a candidate (see [`admit_by_cost_gap`]).
    pub max_cost_gap: i64,
}

// ---------------------------------------------------------------------------
// Pipeline
// ---------------------------------------------------------------------------

/// Shared post-processing pipeline: resegment → rerank → hiragana / partial
/// / kanji-variant rewriters → symbol placement → history_rerank → cost-gap
/// admission → n model-priced paths (offers and symbols ride along) →
/// numeric/katakana → group.
pub(super) fn postprocess(
    paths: &mut Vec<ScoredPath>,
    lattice: &Lattice,
    conv: &ConversionContext<'_>,
    n: usize,
) -> Vec<ScoredPath> {
    let ctx = PostprocessContext {
        lattice,
        conv,
        n,
        max_cost_gap: crate::settings::settings().candidates.max_cost_gap,
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
    let (conn, dict, history, kana) = (
        ctx.conv.conn,
        Some(ctx.conv.dict),
        ctx.conv.history,
        ctx.lattice.input.as_str(),
    );

    observer.after_viterbi(paths);

    // Generate alternative segmentations from the lattice before reranking,
    // so the reranker can compare them on equal footing with Viterbi paths.
    let reseg_paths = resegment::resegment(paths, ctx.lattice, conn);
    paths.extend(reseg_paths);

    let pricer = reranker::FeaturePricer::new(conn, dict);
    let priced = reranker::rerank(paths, &pricer);
    observer.reranked(priced);

    // Variants run BEFORE history_rerank, so history boosts a variant the
    // user picked like any other path (whole-path boosts ×5 can promote a
    // learned hiragana or kanji spelling), and the 1-best sees the variants
    // the N-best sees. The one exception: a 1-best without history keeps
    // index 0 by construction (the Model stage never touches it and nothing
    // re-sorts), so the offers could only be dropped at the cut.
    let hiragana_rw = rewriter::HiraganaVariantRewriter;
    let partial_rw = rewriter::PartialHiraganaRewriter {
        lattice: ctx.lattice,
        conn,
        pricer: &pricer,
    };
    let kanji_rw = rewriter::KanjiVariantRewriter {
        lattice: ctx.lattice,
        conn,
        pricer: &pricer,
    };
    let offers_can_surface = ctx.n > 1 || history.is_some_and(|h| !h.history.is_empty());
    let variants: &[&dyn rewriter::Rewriter] = if offers_can_surface {
        &[&hiragana_rw, &partial_rw, &kanji_rw]
    } else {
        &[]
    };
    rewriter::run_rewriters(variants, paths, kana);

    // Symbols of the whole reading, by position (always, at every n and
    // with or without history): after the offers, whose kana and single
    // kanji the position rule steps over, and before the anchor is taken.
    let placed = place_symbols(paths, ctx.conv.dict, kana, ctx.max_cost_gap);
    observer.symbols_placed(&placed);

    observer.after_rerank(paths);

    // The rerank best before history. Its cost is what number compounds are
    // priced from and what cost-gap admission measures gaps from, whatever
    // history or later rewriters do to the list (the anchor). Its surface is kept on the list: history boosts per-segment
    // unigrams (e.g. き→機 from past "機械"), which can push fragmented
    // single-char paths above the statistically correct compound path
    // (e.g. きがし→気がし).
    let best = paths.first();
    let anchor = best.map_or(0, |p| p.viterbi_cost);
    let viterbi_best_key = best.filter(|_| history.is_some()).map(|p| p.surface_key());

    if let Some(h) = history {
        reranker::history_rerank_at(paths, h.history, conn, h.now);
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
        connection: conn,
        anchor,
    };
    let katakana_rw = rewriter::KatakanaRewriter { anchor };
    // The paths this stage creates are learned on the same terms as the
    // history-reranked ones (G7).
    let boost = history.map(|h| {
        move |p: &mut ScoredPath| reranker::apply_history_boost(p, h.history, conn, h.now)
    });
    rewriter::run_policy_rewriters(
        &[&numeric_rw, &katakana_rw],
        &mut top,
        kana,
        boost.as_ref().map(|b| b as &dyn Fn(&mut ScoredPath)),
        observer,
    );
    observer.before_group(&top);
    if let Some(c) = conn {
        for path in &mut top {
            group_segments(&mut path.segments, c);
        }
    }
    top
}

/// Where the symbol placement puts a reading's symbols when nothing is
/// stepped over: Mozc's `symbol_rewriter_candidate_position` default. A
/// position, not a price, so not a setting.
pub(crate) const SYMBOL_INSERT_OFFSET: usize = 3;

/// One symbol [`place_symbols`] inserted (explain's `symbol_candidates`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SymbolCandidate {
    pub surface: String,
    /// The position rule's `p` in the population (the paths admission keeps
    /// by price): the reading's symbols take `p`, `p + 1`, … in row order.
    pub position: usize,
    /// Population paths the rule stepped over (`symbol_skip`) past the
    /// default position.
    pub skipped: usize,
    /// Size of the population the position was counted in.
    pub population: usize,
    /// The price copied from the population's `p − 1`-th path.
    pub price: i64,
    /// That path is the typed kana (`is_identity`).
    pub copied_from_identity: bool,
}

/// Insert the whole reading's symbol entries (`DictEntry::symbol`, row
/// order) whose surface is not listed yet — Mozc's `SymbolRewriter`
/// position rule over the population admission keeps by price (index 0 and
/// [`admission_keeps`] at the anchor `paths[0]`'s cost): position
/// `min(SYMBOL_INSERT_OFFSET, population)`, then past every population path
/// [`symbol_skip`] names; the symbols go right after the population's
/// `p − 1`-th path and copy its price, so the cost order (and with it the
/// position, through history's stable sort) holds. Nothing for an empty list.
/// The price is a copy, never judged: admission exempts symbol paths. Later
/// stages only move an unlearned symbol down (history makes other paths
/// cheaper; the Override stage inserts by price) or a learned one to #1.
pub(super) fn place_symbols(
    paths: &mut Vec<ScoredPath>,
    dict: &dyn Dictionary,
    reading: &str,
    max_cost_gap: i64,
) -> Vec<SymbolCandidate> {
    let Some(anchor) = paths.first().map(|p| p.viterbi_cost) else {
        return Vec::new();
    };
    let symbols: Vec<_> = dict
        .lookup(reading)
        .into_iter()
        .filter(|e| e.symbol && !paths.iter().any(|p| p.surface_key_eq(&e.surface)))
        .collect();
    if symbols.is_empty() {
        return Vec::new();
    }
    let limit = anchor.saturating_add(admission_bound(max_cost_gap));
    let population: Vec<usize> = (0..paths.len())
        .filter(|&i| i == 0 || admission_keeps(&paths[i], limit))
        .collect();
    let start = SYMBOL_INSERT_OFFSET.min(population.len());
    let mut p = start;
    while p < population.len() && symbol_skip(&paths[population[p]].surface_key()) {
        p += 1;
    }
    let source = population[p - 1];
    let price = paths[source].viterbi_cost;
    let copied_from_identity = paths[source].is_identity();
    let mut placed = Vec::with_capacity(symbols.len());
    for (i, e) in symbols.into_iter().enumerate() {
        let mut path = ScoredPath::single(
            reading.to_string(),
            e.surface.clone(),
            price,
            PathOrigin::Symbol,
        );
        path.segments[0].left_id = e.left_id;
        path.segments[0].right_id = e.right_id;
        paths.insert(source + 1 + i, path);
        placed.push(SymbolCandidate {
            surface: e.surface,
            position: p,
            skipped: p - start,
            population: population.len(),
            price,
            copied_from_identity,
        });
    }
    placed
}

/// Admission keeps `p` by price: the typed kana (`is_identity`, #263/#271)
/// or a pre-history cost at most `limit` (the anchor plus
/// [`admission_bound`]). The one definition admission and the symbol
/// placement's population share.
pub(crate) fn admission_keeps(p: &ScoredPath, limit: i64) -> bool {
    p.is_identity() || p.pre_history_cost() <= limit
}

/// Drop the candidates priced too far above the pre-history #1 (`anchor`):
/// every path after index 0 must cost at most `anchor + max(max_gap,
/// RESCUE_OFFSET)` before history ([`admission_keeps`], which also keeps the
/// typed kana, `is_identity`, #263/#271). Kept whatever their gap besides:
/// index 0 (the top-1, learned or not), a surface the user has committed for
/// this reading (`whole_path_boost > 0`), and a symbol (`PathOrigin::Symbol`:
/// its price is a copy that holds its position, not a price to judge).
/// The floor keeps the kana rescue's band (best + 4000) however the rescue's
/// surface is priced. An offer is judged by its own price, which is never
/// below its source's, so a dropped source takes its offers with it.
/// `on_drop` sees each dropped path (explain lists them); each path other
/// than the typed kana kept by its price records its gap
/// ([`ScoredPath::admission_gap`]).
pub(super) fn admit_by_cost_gap(
    paths: &mut Vec<ScoredPath>,
    anchor: i64,
    max_gap: i64,
    mut on_drop: impl FnMut(&ScoredPath),
) {
    let limit = anchor.saturating_add(admission_bound(max_gap));
    let mut first = true;
    paths.retain_mut(|p| {
        let exempt = std::mem::take(&mut first) || p.is_learned() || p.origin == PathOrigin::Symbol;
        p.admission_gap = None;
        if exempt {
            return true;
        }
        if !admission_keeps(p, limit) {
            on_drop(p);
            return false;
        }
        if !p.is_identity() {
            p.admission_gap = Some(p.pre_history_cost().saturating_sub(anchor));
        }
        true
    });
}

/// How far above the anchor admission keeps a path by price: `max_gap`,
/// never below the kana rescue's band.
pub(crate) fn admission_bound(max_gap: i64) -> i64 {
    max_gap.max(rewriter::RESCUE_OFFSET)
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
    let spans = group_spans(segments, conn);
    let mut morphemes = std::mem::take(segments).into_iter();
    *segments = spans
        .into_iter()
        .filter_map(|span| {
            // A group is its first morpheme with the rest's reading and
            // surface appended and the last one's right id. The spans
            // partition the segments, so each takes at least one.
            let mut members = morphemes.by_ref().take(span.len());
            let mut group = members.next()?;
            for seg in members {
                group.reading.push_str(&seg.reading);
                group.surface.push_str(&seg.surface);
                group.right_id = seg.right_id;
            }
            Some(group)
        })
        .collect();
}

/// The groups [`group_segments`] forms, as ranges of `segments` (the one
/// definition of the grouping; explain sums per-morpheme costs over them).
pub(super) fn group_spans(
    segments: &[RichSegment],
    conn: &ConnectionMatrix,
) -> Vec<std::ops::Range<usize>> {
    let mut spans: Vec<std::ops::Range<usize>> = Vec::new();
    let mut open = false; // the last span is a group that can still grow
    let mut pending_prefix = false;

    for (i, seg) in segments.iter().enumerate() {
        let is_fw = conn.is_function_word(seg.left_id);
        let attach_to_prev = is_fw || conn.is_suffix(seg.left_id); // FunctionWord, Suffix, or Counter

        if attach_to_prev {
            // Joins the open group; with none, stands alone.
            match spans.last_mut().filter(|_| open) {
                Some(last) => last.end = i + 1,
                None => spans.push(i..i + 1),
            }
        } else if conn.is_prefix(seg.left_id) {
            // Prefix: close the open group, start one that will absorb the next CW
            spans.push(i..i + 1);
            open = true;
            pending_prefix = true;
        } else if pending_prefix {
            // ContentWord joins the pending prefix group
            if let Some(last) = spans.last_mut() {
                last.end = i + 1;
            }
            pending_prefix = false;
        } else {
            spans.push(i..i + 1);
            open = true;
        }
    }
    spans
}
