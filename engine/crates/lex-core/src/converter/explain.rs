use std::collections::HashMap;

use serde::Serialize;

use crate::dict::connection::ConnectionMatrix;
use crate::dict::Dictionary;

use crate::settings::settings;

use super::cost::{conn_cost, score_path, script_cost, DefaultCostFunction};
use super::features::{
    is_single_char_kanji, is_single_char_kanji_penalised, is_te_form_kanji_penalised,
};
use super::lattice::{build_lattice, Lattice};
use super::postprocess::{
    admission_bound, postprocess_observed, PostprocessContext, PostprocessObserver,
};
use super::reranker::{compute_history_boost, FeaturePricer};
use super::viterbi::{viterbi_nbest, PathOrigin, ScoredPath};
use super::{ConversionContext, HistoryAt};

// Re-export so downstream crates (e.g. lex-cli) can name the type behind
// `ExplainPath::history_breakdown` — the definition lives in the crate-private
// `reranker` module.
pub use super::reranker::HistoryBoostBreakdown;
pub use super::rewriter::{PolicyPrice, PolicyRule};

/// Full diagnostic result for a single reading.
#[derive(Debug, Serialize)]
pub struct ExplainResult {
    pub reading: String,
    pub lattice_char_count: usize,
    pub lattice_nodes: Vec<ExplainNode>,
    pub paths: Vec<ExplainPath>,
    /// The `[candidates] max_cost_gap` the pipeline ran with.
    pub max_cost_gap: i64,
    /// The bound admission applied above the anchor: `max_cost_gap`, never
    /// below the kana-rescue band.
    pub cost_gap_bound: i64,
    /// The pre-history #1's cost the gaps are measured from.
    pub cost_gap_anchor: i64,
    /// Paths cost-gap admission dropped, by gap then surface.
    pub dropped_by_cost_gap: Vec<DroppedPath>,
    /// Surfaces whose policy record disagrees with the final path: priced by
    /// Numeric / Katakana with no record or with a record of another price,
    /// or recorded while something else priced the path. A pipeline bug;
    /// empty when consistent.
    pub policy_price_inconsistent: Vec<String>,
    /// Surfaces a policy fallback found already listed and left alone (the
    /// listed path keeps its price, plan §27-8 B6).
    pub policy_skipped: Vec<String>,
    /// The paths of the cost model's population (origin Viterbi or
    /// Resegment) after rerank, before admission and the other stages: the
    /// set a surface must be in for this run to have been able to choose it.
    /// For callers comparing two populations (`candidates::widths`); not part
    /// of the JSON output.
    #[serde(skip)]
    pub model_paths: Vec<ModelPath>,
}

/// A path of the cost model's population (see [`ExplainResult::model_paths`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ModelPath {
    pub surface: String,
    pub origin: PathOrigin,
}

/// A path cost-gap admission dropped.
#[derive(Debug, Serialize)]
pub struct DroppedPath {
    pub surface: String,
    /// Its pre-history cost above the pre-history #1.
    pub gap: i64,
    pub origin: PathOrigin,
    pub priced_by: PathOrigin,
}

/// A lattice node for diagnostic display.
#[derive(Debug, Serialize)]
pub struct ExplainNode {
    pub start: usize,
    pub end: usize,
    pub reading: String,
    pub surface: String,
    pub cost: i16,
    pub left_id: u16,
    pub right_id: u16,
}

impl ExplainNode {
    /// Build from a lattice + node index.
    fn from_lattice(lattice: &Lattice, idx: usize) -> Self {
        Self {
            start: lattice.start(idx),
            end: lattice.end(idx),
            reading: lattice.reading(idx).to_string(),
            surface: lattice.surface(idx).to_string(),
            cost: lattice.cost(idx),
            left_id: lattice.left_id(idx),
            right_id: lattice.right_id(idx),
        }
    }
}

/// A complete path with full cost breakdown.
#[derive(Debug, Serialize)]
pub struct ExplainPath {
    pub segments: Vec<ExplainSegment>,
    pub viterbi_cost: i64,
    /// Feature adjustment rerank added to the Viterbi cost.
    pub rerank_delta: i64,
    /// Per-component history boost (raw sums + whole-path × 5).
    pub history_breakdown: HistoryBoostBreakdown,
    /// History boost actually subtracted from the cost (post-normalization).
    pub history_boost: i64,
    /// Segment count `history_rerank` used as the normalization denominator.
    /// May differ from `segments.len()` when `group_segments` later merged
    /// adjacent segments — keep this value when reporting `/N segs`.
    pub history_segment_count: usize,
    /// Final cost after all adjustments.
    pub final_cost: i64,
    /// Who produced the segments.
    pub origin: PathOrigin,
    /// Who set the price (differs from `origin` when a duplicate's price
    /// was adopted).
    pub priced_by: PathOrigin,
    /// What Viterbi charges for these segments' ids and costs, before the
    /// rerank features (over the ungrouped nodes; a PartialHiragana fallback
    /// carries the kanji node's under its reading); `None` for a synthetic
    /// single segment (kana rescue, Numeric, Katakana).
    pub model_cost: Option<i64>,
    /// For a path not priced by the model (an offer, the kana rescue, a
    /// policy price, or a path one of them repriced): its price before
    /// history minus `model_cost` and the rerank features — what the offer
    /// clamp or the policy moved it by. `None` for model-priced paths and
    /// where there is no `model_cost`.
    pub price_clamp: Option<i64>,
    /// The price was set by another stage than the one that made the
    /// segments (an offer or the kana rescue adopted over them).
    pub repriced: bool,
    /// The path's one-character-reading segments with kanji in the surface,
    /// taken before `group_segments` merges them into a neighbour (the
    /// `segments` above are grouped, which is why the single-kanji penalty
    /// is shown here and not per segment). Empty without a connection
    /// matrix.
    pub single_kanji_segments: Vec<SingleKanjiSegment>,
    /// Position in `viterbi_nbest`'s output (after its surface dedup, before
    /// any stage reorders). Set only for a path Viterbi made and the model
    /// priced (`origin == Viterbi`, `priced_by.is_model()`); `None` for a
    /// resegment, an offer, a policy path or a path another stage repriced.
    /// Tells a path the population cut from one it kept (#361).
    pub viterbi_rank: Option<usize>,
    /// The policy that priced a Numeric / Katakana path, as the Override
    /// stage wrote it: `final_cost + history_boost == policy_price.cost()`.
    /// `None` for any other path.
    pub policy_price: Option<PolicyPrice>,
    /// Cost-gap admission's verdict on the path: its pre-history gap to the
    /// anchor when admission kept it by that price; `None` for the head, an
    /// exempt path (learned, the typed kana) or a policy price.
    pub admission_gap: Option<i64>,
}

/// A one-character-reading segment with kanji in its surface, whatever its
/// role.
#[derive(Debug, Clone, Serialize)]
pub struct SingleKanjiSegment {
    pub reading: String,
    pub surface: String,
    pub left_id: u16,
    /// Connection-matrix role of `left_id` (0 = content word).
    pub role: u8,
    /// The single-char kanji penalty's predicate holds for it (rerank's
    /// predicate, with the dictionary for the compound exemption), whatever
    /// the weight. Whether this path's price includes it is another matter:
    /// a lone path, an Override price and the kana rescue are not priced by
    /// the features.
    pub penalised: bool,
}

impl ExplainPath {
    pub fn surface(&self) -> String {
        self.segments.iter().map(|s| s.surface.as_str()).collect()
    }
}

impl ExplainResult {
    /// The first path carrying `surface`, `None` when no path does.
    pub fn path(&self, surface: &str) -> Option<&ExplainPath> {
        self.paths.iter().find(|p| p.surface() == surface)
    }
}

/// A segment within a conversion path, with full cost breakdown.
#[derive(Debug, Serialize)]
pub struct ExplainSegment {
    pub reading: String,
    pub surface: String,
    pub word_cost: i64,
    pub segment_penalty: i64,
    pub script_cost: i64,
    /// Connection cost from BOS or previous segment.
    pub connection_cost: i64,
    /// Te-form kanji penalty applied.
    pub te_form_kanji_penalty: i64,
    pub left_id: u16,
    pub right_id: u16,
}

// ---------------------------------------------------------------------------
// Observer that captures cost snapshots for explain diagnostics
// ---------------------------------------------------------------------------

/// Snapshot of one path's state at the post-rerank / pre-history-rerank stage.
///
/// Captured here (and not after history_rerank) so the recorded breakdown is
/// computed on the same segments that history_rerank actually scored — the
/// pipeline later runs rewriters that add new candidates and `group_segments`
/// that merges adjacent segments, both of which would invalidate a recompute
/// against the final path.
#[derive(Default, Clone, Copy)]
struct PreHistorySnapshot {
    /// Cost after resegment, rerank and the Model-stage rewriters, before any
    /// history adjustment.
    cost: i64,
    /// Per-component history boost (raw sums + whole-path × 5).
    breakdown: HistoryBoostBreakdown,
    /// Segment count at the moment `history_rerank` saw the path. May differ
    /// from the final `segments.len()` after `group_segments` merges adjacent
    /// segments — kept here so the displayed `/N segs` matches the denominator
    /// actually used during normalization.
    segment_count: usize,
}

/// Each surface of the Viterbi output with its `viterbi_cost` and position
/// (`viterbi_nbest` has already deduped by surface, so positions are
/// consecutive).
type ViterbiOutput = HashMap<String, (i64, usize)>;

/// Diagnostic observer.
///
/// Keys are `ScoredPath::surface_key()` — i.e. the concatenated surface — so
/// that lookups survive `group_segments` (which merges adjacent segments but
/// preserves the overall surface). Paths priced by the Override stage
/// (numeric, katakana) take their breakdown from `override_boosts` instead:
/// that stage boosts them itself, after history_rerank.
struct ExplainObserver<'a> {
    history: Option<HistoryAt<'a>>,
    /// Needed so the displayed breakdown matches `history_rerank`'s
    /// function-word exclusion for per-segment unigram boosts.
    conn: Option<&'a ConnectionMatrix>,
    /// The raw Viterbi output, before resegment / rerank.
    viterbi: ViterbiOutput,
    /// State at the post-rerank / pre-history-rerank boundary.
    pre_history: HashMap<String, PreHistorySnapshot>,
    /// Breakdown and segment count of each final Override-priced path, as
    /// that stage boosted it.
    override_boosts: HashMap<String, (HistoryBoostBreakdown, usize)>,
    /// `(Viterbi price, rerank features)` of each final path whose segments
    /// are lattice nodes, taken before `group_segments` merges morphemes (a
    /// grouped segment keeps only the first node's cost).
    model_costs: HashMap<String, (i64, i64)>,
    /// Single-kanji segments of each final path, before `group_segments`.
    single_kanji: HashMap<String, Vec<SingleKanjiSegment>>,
    dict: &'a dyn Dictionary,
    pricer: FeaturePricer<'a>,
    /// Paths cost-gap admission dropped, and the anchor it measured from.
    dropped: Vec<DroppedPath>,
    anchor: i64,
    model_paths: Vec<ModelPath>,
    /// The last policy price the Override stage wrote onto each surface.
    policy: HashMap<String, PolicyPrice>,
    policy_skipped: Vec<String>,
}

impl<'a> ExplainObserver<'a> {
    fn new(conv: &ConversionContext<'a>) -> Self {
        let ConversionContext {
            dict,
            conn,
            history,
        } = *conv;
        Self {
            history,
            conn,
            viterbi: HashMap::new(),
            pre_history: HashMap::new(),
            override_boosts: HashMap::new(),
            model_costs: HashMap::new(),
            single_kanji: HashMap::new(),
            dict,
            pricer: FeaturePricer::new(conn, Some(dict)),
            dropped: Vec::new(),
            anchor: 0,
            model_paths: Vec::new(),
            policy: HashMap::new(),
            policy_skipped: Vec::new(),
        }
    }
}

impl PostprocessObserver for ExplainObserver<'_> {
    fn after_viterbi(&mut self, paths: &[ScoredPath]) {
        self.viterbi = paths
            .iter()
            .enumerate()
            .map(|(rank, p)| (p.surface_key(), (p.viterbi_cost, rank)))
            .collect();
    }

    fn after_rerank(&mut self, paths: &[ScoredPath]) {
        self.pre_history.clear();
        self.anchor = paths.first().map_or(0, |p| p.viterbi_cost);
        self.model_paths = paths
            .iter()
            .filter(|p| p.origin.is_model())
            .map(|p| ModelPath {
                surface: p.surface_key(),
                origin: p.origin,
            })
            .collect();
        for p in paths {
            let breakdown = match self.history {
                Some(h) => compute_history_boost(p, h.history, self.conn, h.now),
                None => HistoryBoostBreakdown::default(),
            };
            self.pre_history.insert(
                p.surface_key(),
                PreHistorySnapshot {
                    cost: p.viterbi_cost,
                    breakdown,
                    segment_count: p.segments.len(),
                },
            );
        }
    }

    fn dropped_by_cost_gap(&mut self, path: &ScoredPath, anchor: i64) {
        // An Override candidate can bring the surface back; its breakdown
        // must not borrow the dropped path's snapshot.
        self.pre_history.remove(&path.surface_key());
        self.dropped.push(DroppedPath {
            surface: path.surface_key(),
            gap: path.pre_history_cost() - anchor,
            origin: path.origin,
            priced_by: path.priced_by,
        });
    }

    fn policy_priced(&mut self, key: &str, price: PolicyPrice) {
        self.policy.insert(key.to_string(), price);
    }

    fn policy_skipped(&mut self, key: &str, _price: PolicyPrice) {
        self.policy_skipped.push(key.to_string());
    }

    fn before_group(&mut self, paths: &[ScoredPath]) {
        if let Some(h) = self.history {
            self.override_boosts = paths
                .iter()
                .filter(|p| is_override_priced(p))
                .map(|p| {
                    let b = compute_history_boost(p, h.history, self.conn, h.now);
                    (p.surface_key(), (b, p.segments.len()))
                })
                .collect();
        }
        // Offers carry node ids and costs (a Partial fallback, the kanji
        // node's under its reading); the kana rescue, Numeric and Katakana
        // are one synthetic segment.
        self.model_costs = paths
            .iter()
            .filter(|p| p.origin.is_lattice_path() || p.origin == PathOrigin::PartialHiragana)
            .map(|p| {
                let costs = (
                    score_path(&p.segments, self.conn),
                    self.pricer.adjustment(p),
                );
                (p.surface_key(), costs)
            })
            .collect();
        if let Some(conn) = self.conn {
            self.single_kanji = paths
                .iter()
                .filter_map(|p| {
                    let listed = single_kanji_segments(p, conn, self.dict);
                    (!listed.is_empty()).then(|| (p.surface_key(), listed))
                })
                .collect();
        }
    }
}

fn single_kanji_segments(
    p: &ScoredPath,
    conn: &ConnectionMatrix,
    dict: &dyn Dictionary,
) -> Vec<SingleKanjiSegment> {
    p.segments
        .iter()
        .enumerate()
        .filter(|(_, s)| is_single_char_kanji(s))
        .map(|(i, s)| SingleKanjiSegment {
            reading: s.reading.clone(),
            surface: s.surface.clone(),
            left_id: s.left_id,
            role: conn.role(s.left_id),
            penalised: is_single_char_kanji_penalised(s, i, &p.segments, conn, Some(dict)),
        })
        .collect()
}

/// Priced by the Override stage (Numeric / Katakana), which applies the
/// history boost itself.
fn is_override_priced(p: &ScoredPath) -> bool {
    matches!(p.priced_by, PathOrigin::Numeric | PathOrigin::Katakana)
}

// ---------------------------------------------------------------------------
// Per-segment cost breakdown
// ---------------------------------------------------------------------------

/// Build per-segment cost breakdown from a ScoredPath.
fn explain_segments(scored: &ScoredPath, conn: Option<&ConnectionMatrix>) -> Vec<ExplainSegment> {
    scored
        .segments
        .iter()
        .enumerate()
        .map(|(i, seg)| {
            let connection = if i == 0 {
                conn_cost(conn, 0, seg.left_id)
            } else {
                let prev = &scored.segments[i - 1];
                conn_cost(conn, prev.right_id, seg.left_id)
            };
            let prev_seg = if i > 0 {
                Some(&scored.segments[i - 1])
            } else {
                None
            };
            let te_penalty = if let Some(c) = conn {
                if is_te_form_kanji_penalised(seg, prev_seg, c) {
                    settings().reranker.te_form_kanji_penalty
                } else {
                    0
                }
            } else {
                0
            };
            ExplainSegment {
                reading: seg.reading.clone(),
                surface: seg.surface.clone(),
                word_cost: seg.word_cost as i64,
                segment_penalty: settings().cost.segment_penalty,
                script_cost: script_cost(&seg.surface, seg.reading.chars().count()),
                connection_cost: connection,
                te_form_kanji_penalty: te_penalty,
                left_id: seg.left_id,
                right_id: seg.right_id,
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// The `viterbi_rank` of a final path: its position in `viterbi_nbest`'s
/// output, only for a path Viterbi made whose price the model still owns. A
/// Viterbi path a Model-stage candidate repriced (`compose_duplicate` keeps
/// the existing path and writes the cheaper candidate's `priced_by` onto it)
/// has no rank: its price is not the Viterbi one the rank describes.
fn viterbi_rank_of(scored: &ScoredPath, viterbi: &ViterbiOutput) -> Option<usize> {
    if scored.origin == PathOrigin::Viterbi && scored.priced_by.is_model() {
        viterbi.get(&scored.surface_key()).map(|&(_, rank)| rank)
    } else {
        None
    }
}

/// Run the full conversion pipeline and capture detailed cost breakdown.
///
/// Uses `postprocess_observed` over the same oversample as production N-best
/// (`population`) to follow the exact same pipeline as production
/// conversion, with an observer that records cost snapshots at each stage
/// for diagnostic output. History decays against the caller's `now`.
pub fn explain(conv: &ConversionContext<'_>, kana: &str, n: usize) -> ExplainResult {
    let ConversionContext {
        dict,
        conn,
        history,
    } = *conv;
    let max_cost_gap = settings().candidates.max_cost_gap;
    if kana.is_empty() || n == 0 {
        return ExplainResult {
            reading: kana.to_string(),
            lattice_char_count: 0,
            lattice_nodes: Vec::new(),
            paths: Vec::new(),
            max_cost_gap,
            cost_gap_bound: admission_bound(max_cost_gap),
            cost_gap_anchor: 0,
            dropped_by_cost_gap: Vec::new(),
            policy_price_inconsistent: Vec::new(),
            policy_skipped: Vec::new(),
            model_paths: Vec::new(),
        };
    }

    let lattice = build_lattice(dict, kana);
    let lattice_nodes: Vec<ExplainNode> = (0..lattice.node_count())
        .map(|idx| ExplainNode::from_lattice(&lattice, idx))
        .collect();

    let cost_fn = DefaultCostFunction::new(conn);
    let oversample = super::population(n, history.is_some());
    let mut raw_paths = viterbi_nbest(&lattice, &cost_fn, oversample);

    let mut observer = ExplainObserver::new(conv);
    let ctx = PostprocessContext {
        lattice: &lattice,
        conv,
        n,
        max_cost_gap,
    };
    let final_paths = postprocess_observed(&mut raw_paths, &ctx, &mut observer);
    let mut dropped_by_cost_gap = std::mem::take(&mut observer.dropped);
    dropped_by_cost_gap.sort_by(|a, b| a.gap.cmp(&b.gap).then_with(|| a.surface.cmp(&b.surface)));
    let mut policy_price_inconsistent = Vec::new();

    let paths: Vec<ExplainPath> = final_paths
        .iter()
        .map(|scored| {
            let key = scored.surface_key();
            let (policy_price, consistent) =
                match (is_override_priced(scored), observer.policy.remove(&key)) {
                    (true, Some(p)) => (Some(p), p.cost() == scored.pre_history_cost()),
                    (false, None) => (None, true),
                    (_, recorded) => (recorded.filter(|_| is_override_priced(scored)), false),
                };
            if !consistent {
                policy_price_inconsistent.push(key.clone());
            }
            // Look up snapshots by surface_key — preserved through group_segments.
            // A model-priced path shows its raw Viterbi cost (or, missed by
            // the observer, its model cost) and the rerank delta. Any other
            // price is split into model cost + features + clamp/policy.
            // Numeric / Katakana are added after history_rerank, so they
            // have no snapshot; their boost is the Override stage's.
            let model = observer.model_costs.get(&key).copied();
            let model_cost = model.map(|(m, _)| m);
            let pre = scored.pre_history_cost();
            let original = if scored.priced_by.is_model() {
                observer
                    .viterbi
                    .get(&key)
                    .map(|&(cost, _)| cost)
                    .or(model_cost)
                    .unwrap_or(pre)
            } else {
                model_cost.unwrap_or(pre)
            };
            let price_clamp = model
                .filter(|_| !scored.priced_by.is_model())
                .map(|(m, adj)| pre - (m + adj));
            let snapshot = observer
                .pre_history
                .get(&key)
                .copied()
                .unwrap_or(PreHistorySnapshot {
                    cost: original,
                    breakdown: HistoryBoostBreakdown::default(),
                    segment_count: scored.segments.len(),
                });
            // An Override price was boosted by that stage, on its own
            // segments; the pre-history snapshot does not describe it.
            let (history_breakdown, history_segment_count) = if is_override_priced(scored) {
                observer
                    .override_boosts
                    .get(&key)
                    .copied()
                    .unwrap_or((HistoryBoostBreakdown::default(), scored.segments.len()))
            } else {
                (snapshot.breakdown, snapshot.segment_count)
            };
            let rerank_delta = match (price_clamp, model) {
                (Some(_), Some((_, adj))) => adj,
                _ => snapshot.cost - original,
            };
            ExplainPath {
                segments: explain_segments(scored, conn),
                viterbi_cost: original,
                rerank_delta,
                history_breakdown,
                history_boost: scored.history_boost,
                history_segment_count,
                final_cost: scored.viterbi_cost,
                origin: scored.origin,
                priced_by: scored.priced_by,
                model_cost,
                price_clamp,
                repriced: scored.priced_by != scored.origin,
                viterbi_rank: viterbi_rank_of(scored, &observer.viterbi),
                policy_price,
                admission_gap: scored.admission_gap,
                single_kanji_segments: observer.single_kanji.remove(&key).unwrap_or_default(),
            }
        })
        .collect();

    ExplainResult {
        reading: kana.to_string(),
        lattice_char_count: lattice.char_count,
        lattice_nodes,
        paths,
        max_cost_gap: ctx.max_cost_gap,
        cost_gap_bound: admission_bound(ctx.max_cost_gap),
        cost_gap_anchor: observer.anchor,
        dropped_by_cost_gap,
        policy_price_inconsistent,
        policy_skipped: std::mem::take(&mut observer.policy_skipped),
        model_paths: std::mem::take(&mut observer.model_paths),
    }
}

/// Format an ExplainResult as human-readable text.
pub fn format_text(result: &ExplainResult) -> String {
    use unicode_width::UnicodeWidthStr;
    let mut out = String::new();

    out.push_str(&format!(
        "=== Lattice for \"{}\" ({} chars, {} nodes) ===\n",
        result.reading,
        result.lattice_char_count,
        result.lattice_nodes.len(),
    ));

    // Group nodes by start position
    let max_pos = result
        .lattice_nodes
        .iter()
        .map(|n| n.start)
        .max()
        .unwrap_or(0);
    for pos in 0..=max_pos {
        let nodes_at_pos: Vec<&ExplainNode> = result
            .lattice_nodes
            .iter()
            .filter(|n| n.start == pos)
            .collect();
        if nodes_at_pos.is_empty() {
            continue;
        }
        out.push_str(&format!("  Position {}:\n", pos));
        for n in &nodes_at_pos {
            let surface_display = if n.surface != n.reading {
                format!(" -> {}", n.surface)
            } else {
                String::new()
            };
            out.push_str(&format!(
                "    [{},{}] {}  cost={:<6} L={:<4} R={:<4}{}\n",
                n.start, n.end, n.reading, n.cost, n.left_id, n.right_id, surface_display,
            ));
        }
    }

    if result.paths.is_empty() {
        out.push_str("\nNo paths found.\n");
        // What was dropped is the answer to "why is X not here".
        format_dropped(result, &mut out);
        return out;
    }

    out.push_str(&format!("\n=== Paths ({}) ===\n", result.paths.len()));
    for (i, path) in result.paths.iter().enumerate() {
        let surface = path.surface();
        let mut priced = if path.priced_by == path.origin {
            format!("{:?}", path.origin)
        } else {
            format!("{:?}, priced by {:?}", path.origin, path.priced_by)
        };
        if let Some(p) = path.policy_price {
            priced.push_str(&format!(
                ", {:?} = anchor {:+} {:+}",
                p.rule,
                p.rule.offset(),
                p.cost_offset
            ));
        }
        out.push_str(&format!(
            "\n  #{:<2} {}  (final_cost={}, {})\n",
            i + 1,
            surface,
            path.final_cost,
            priced,
        ));

        for (j, seg) in path.segments.iter().enumerate() {
            let seg_label = if seg.surface != seg.reading {
                format!("{}({})", seg.surface, seg.reading)
            } else {
                seg.surface.clone()
            };
            let pad_width = 16;
            let display_width = UnicodeWidthStr::width(seg_label.as_str());
            let padded = if display_width < pad_width {
                format!("{}{}", seg_label, " ".repeat(pad_width - display_width))
            } else {
                seg_label
            };
            let conn_label = if j == 0 { "BOS->" } else { "conn=" };
            let te_str = if seg.te_form_kanji_penalty > 0 {
                format!(" teK={:<+6}", seg.te_form_kanji_penalty)
            } else {
                String::new()
            };
            out.push_str(&format!(
                "    seg[{}]: {} word={:<6} penalty={:<5} script={:<6} {}{}{}\n",
                j,
                padded,
                seg.word_cost,
                seg.segment_penalty,
                seg.script_cost,
                conn_label,
                seg.connection_cost,
                te_str,
            ));
        }

        let clamp = path
            .price_clamp
            .map_or(String::new(), |c| format!(" clamp={c:<+8}"));
        out.push_str(&format!(
            "    viterbi={:<8} rerank={:<+8}{} history={:<+8} -> final={}\n",
            path.viterbi_cost, path.rerank_delta, clamp, -path.history_boost, path.final_cost,
        ));
        let hb = &path.history_breakdown;
        if hb.unigram_sum != 0 || hb.bigram_sum != 0 || hb.whole_path_boost != 0 {
            out.push_str(&format!(
                "      history: uni_sum={:<+7} bi_sum={:<+7} whole×5={:<+7} (/{} segs)\n",
                -hb.unigram_sum, -hb.bigram_sum, -hb.whole_path_boost, path.history_segment_count,
            ));
        }
        if !path.single_kanji_segments.is_empty() {
            let listed: Vec<String> = path
                .single_kanji_segments
                .iter()
                .map(|s| {
                    let mark = if s.penalised { " penalised" } else { "" };
                    format!("{}({}) role={}{mark}", s.surface, s.reading, s.role)
                })
                .collect();
            out.push_str(&format!(
                "      1-char kanji (ungrouped): {}\n",
                listed.join(", ")
            ));
        }
    }

    format_dropped(result, &mut out);
    if !result.policy_price_inconsistent.is_empty() {
        out.push_str(&format!(
            "\n=== Policy price inconsistent (pipeline bug): {} ===\n",
            result.policy_price_inconsistent.join(", ")
        ));
    }
    out
}

/// The paths cost-gap admission dropped.
fn format_dropped(result: &ExplainResult, out: &mut String) {
    if !result.dropped_by_cost_gap.is_empty() {
        out.push_str(&format!(
            "\n=== Dropped by cost gap ({}; bound={} above anchor={}, max_cost_gap={}) ===\n",
            result.dropped_by_cost_gap.len(),
            result.cost_gap_bound,
            result.cost_gap_anchor,
            result.max_cost_gap,
        ));
        for d in &result.dropped_by_cost_gap {
            let priced = if d.priced_by == d.origin {
                format!("{:?}", d.origin)
            } else {
                format!("{:?}, priced by {:?}", d.origin, d.priced_by)
            };
            out.push_str(&format!("  {}  (gap={}, {})\n", d.surface, d.gap, priced));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::converter::testutil::{joined, test_dict};
    use crate::dict::connection::ConnectionMatrix;
    use crate::dict::{DictEntry, TrieDictionary};
    use crate::user_history::{now_epoch, UserHistory};

    /// 確認 (content word), 史 (名詞,接尾: role 2, grouped into the word
    /// before it) and 値 (content word), the shapes of かくにんしますね.
    fn single_kanji_fixture() -> (TrieDictionary, ConnectionMatrix) {
        let e = crate::converter::testutil::entry_with_id;
        let dict = TrieDictionary::from_entries(vec![
            ("かくにん".into(), vec![e("確認", 0, 1)]),
            ("し".into(), vec![e("史", 0, 2)]),
            ("ね".into(), vec![e("値", 0, 1)]),
        ]);
        (
            dict,
            crate::converter::testutil::zero_conn_with_roles(3, vec![0, 0, 2]),
        )
    }

    /// §27-8 T-B6 / T-B7: every policy path carries the rule that priced it,
    /// with `final_cost + history_boost == policy_price.cost()`, including
    /// 二十三, whose Viterbi path the number compound replaced: one segment,
    /// made and priced by Numeric (#393).
    #[test]
    fn policy_prices_are_recorded_where_the_override_stage_writes_them() {
        let e = crate::converter::testutil::entry_with_id;
        let dict = TrieDictionary::from_entries(vec![(
            "にじゅうさん".into(),
            vec![e("に十三", 0, 1), e("二十三", 2000, 1)],
        )]);
        let result = explain(
            &crate::converter::ConversionContext::new(&dict, None),
            "にじゅうさん",
            20,
        );
        assert!(result.policy_price_inconsistent.is_empty());
        let anchor = result.cost_gap_anchor;
        for (surface, rule, cost_offset) in [
            ("二十三", PolicyRule::NumericCompound, 0),
            ("23", PolicyRule::NumericFallback, 0),
            ("２３", PolicyRule::NumericFallback, 1),
            ("ニジュウサン", PolicyRule::KatakanaFallback, 0),
        ] {
            let p = path_by_surface(&result, surface);
            let price = p.policy_price.expect(surface);
            assert_eq!(
                (price.rule, price.base, price.cost_offset),
                (rule, anchor, cost_offset)
            );
            assert_eq!(p.final_cost + p.history_boost, price.cost(), "{surface}");
        }
        let compound = path_by_surface(&result, "二十三");
        assert_eq!(
            (compound.origin, compound.priced_by, compound.repriced),
            (PathOrigin::Numeric, PathOrigin::Numeric, false),
            "the compound's own path"
        );
        assert_eq!(compound.segments.len(), 1);
        assert_eq!(compound.model_cost, None);
        assert_eq!(path_by_surface(&result, "に十三").policy_price, None);
    }

    /// B6 in explain: the katakana fallback finds テスト listed (a
    /// dictionary path) and is reported as skipped; the path keeps its model
    /// price and admission's verdict (the head has none, a path kept by
    /// price has its gap).
    #[test]
    fn a_skipped_fallback_and_admission_verdicts_are_reported() {
        let e = crate::converter::testutil::entry_with_id;
        let dict = TrieDictionary::from_entries(vec![(
            "てすと".into(),
            vec![e("手酢と", 0, 1), e("テスト", 1000, 1)],
        )]);
        let result = explain(
            &crate::converter::ConversionContext::new(&dict, None),
            "てすと",
            20,
        );
        assert_eq!(result.policy_skipped, ["テスト"]);
        let listed = path_by_surface(&result, "テスト");
        assert_eq!(listed.priced_by, PathOrigin::Viterbi);
        assert_eq!(listed.policy_price, None);
        let head = &result.paths[0];
        assert_eq!(
            head.admission_gap, None,
            "the head is kept whatever its price"
        );
        assert_eq!(
            listed.admission_gap,
            Some(listed.final_cost - result.cost_gap_anchor)
        );
    }

    fn path_by_surface<'r>(result: &'r ExplainResult, surface: &str) -> &'r ExplainPath {
        result
            .path(surface)
            .unwrap_or_else(|| panic!("no path {surface}"))
    }

    fn listed(p: &ExplainPath) -> Vec<(&str, u8, bool)> {
        p.single_kanji_segments
            .iter()
            .map(|s| (s.surface.as_str(), s.role, s.penalised))
            .collect()
    }

    #[test]
    fn single_kanji_segments_are_listed_before_grouping() {
        let (dict, conn) = single_kanji_fixture();
        let result = explain(
            &crate::converter::ConversionContext::new(&dict, Some(&conn)),
            "かくにんし",
            5,
        );
        let p = path_by_surface(&result, "確認史");
        assert_eq!(p.segments.len(), 1, "the suffix is grouped into 確認");
        assert_eq!(listed(p), [("史", 2, false)]);
    }

    #[test]
    fn single_kanji_segments_apply_the_compound_exemption() {
        // 京|都 with 京都 in the dictionary: 都 is a content word, exempt.
        let e = crate::converter::testutil::entry_with_id;
        let dict = TrieDictionary::from_entries(vec![
            ("きょう".into(), vec![e("京", 0, 1)]),
            ("と".into(), vec![e("都", 0, 1)]),
            ("きょうと".into(), vec![e("京都", 20000, 1)]),
        ]);
        let conn = crate::converter::testutil::zero_conn_with_roles(2, vec![0, 0]);
        let result = explain(
            &crate::converter::ConversionContext::new(&dict, Some(&conn)),
            "きょうと",
            5,
        );
        let p = result
            .paths
            .iter()
            .find(|p| p.segments.len() == 2)
            .expect("京|都 is a path");
        assert_eq!(listed(p), [("都", 0, false)]);
    }

    #[test]
    fn single_kanji_segments_mark_the_penalised_content_word_at_any_weight() {
        // The shipped weight is not what decides `penalised`.
        let (dict, conn) = single_kanji_fixture();
        let result = explain(
            &crate::converter::ConversionContext::new(&dict, Some(&conn)),
            "かくにんね",
            5,
        );
        assert_eq!(
            listed(path_by_surface(&result, "確認値")),
            [("値", 0, true)]
        );
    }

    #[test]
    fn test_explain_basic() {
        let dict = test_dict();
        let result = explain(
            &crate::converter::ConversionContext::new(&dict, None),
            "きょう",
            5,
        );

        assert_eq!(result.reading, "きょう");
        assert!(!result.lattice_nodes.is_empty());
        assert!(!result.paths.is_empty());

        // Best path should have segments
        let best = &result.paths[0];
        assert!(!best.segments.is_empty());
    }

    /// The #353 record: the two cheapest paths (可|な|や, 課|な|や) have two
    /// 4000 transitions; カナ|や connects at 0 but is the 4th-cheapest path.
    /// A stage that measured from the population's minimum rather than the
    /// #1 would change what survives once the population reached カナ|や.
    fn multi_segment_kanaya() -> (TrieDictionary, ConnectionMatrix) {
        let e = crate::converter::testutil::entry_with_id;
        let dict = TrieDictionary::from_entries(vec![
            ("か".into(), vec![e("可", 0, 1), e("課", 100, 1)]),
            ("な".into(), vec![e("な", 0, 2)]),
            ("や".into(), vec![e("や", 0, 4)]),
            ("かな".into(), vec![e("カナ", 16000, 3)]),
            ("かなや".into(), vec![e("仮名屋", 20000, 5)]),
        ]);
        let mut costs = vec![0i16; 36];
        costs[6 + 2] = 4000; // 1 → 2
        costs[2 * 6 + 4] = 4000; // 2 → 4
        (
            dict,
            ConnectionMatrix::new_owned(6, 0, 0, Vec::new(), costs),
        )
    }

    /// explain describes the production N-best: same paths, same order, at
    /// every n. The population decides which paths rerank sees, so a
    /// different population can swap even the top-1: the two deep fixtures
    /// put rerank's argmin at Viterbi's 13th path (no history) and a learned
    /// path at its 41st, so the populations 10 / 12 / 15 (no history) and
    /// 30 / 50 (history) of n = 1..=6 each see a different set (#361). The
    /// other learned arms run on the multi-segment fixture, whose 仮名屋 /
    /// カナや are not the #1.
    #[test]
    fn test_explain_paths_match_production_nbest() {
        let now = now_epoch();
        let deep_argmin = crate::converter::testutil::deep_argmin_kanaya(12);
        let deep_learned = crate::converter::testutil::deep_learned_kanaya(40);
        let mut deep_h = UserHistory::new();
        deep_h.record(&[(
            "かなや".into(),
            crate::converter::testutil::DEEP_LEARNED.into(),
        )]);
        let multi_segment = multi_segment_kanaya();
        // Admission drops 可|な|や here: explain must observe the same drops
        // production makes.
        let far = crate::converter::testutil::far_multi_segment_kanaya();
        let mut h = UserHistory::new();
        h.record(&[("な".into(), "な".into())]);
        // A whole-pair learning of a non-#1 surface moves it to index 0.
        let mut learned = UserHistory::new();
        learned.record(&[("かなや".into(), "仮名屋".into())]);
        // カナや learned a year ago: its boost has decayed far short of its
        // gap, so only the rotate can make it #1.
        let mut stale = UserHistory::new();
        stale.record_at(
            &[("かなや".into(), "カナや".into())],
            now_epoch() - 3600 * 24 * 365,
        );
        let mut rotated_at = 0;
        for ((dict, conn), history, is_stale) in [
            (&deep_argmin, None, false),
            (&deep_argmin, Some(&h), false),
            (&deep_learned, Some(&deep_h), false),
            (&multi_segment, None, false),
            (&multi_segment, Some(&h), false),
            (&multi_segment, Some(&learned), false),
            (&multi_segment, Some(&stale), true),
            (&far, None, false),
            (&far, Some(&h), false),
        ] {
            for n in 1..=6 {
                let explanation = explain(
                    &crate::converter::ConversionContext::new(dict, Some(conn))
                        .with_history(history, now),
                    "かなや",
                    n,
                );
                let explained: Vec<String> =
                    explanation.paths.iter().map(|p| p.surface()).collect();
                if let [first, second, ..] = explanation.paths.as_slice() {
                    if is_stale && n >= 2 {
                        assert!(first.history_breakdown.whole_path_boost > 0, "n={n}");
                        assert!(
                            first.final_cost > second.final_cost,
                            "n={n}: the learned #1 was moved, not priced, to the front"
                        );
                        rotated_at += 1;
                    }
                }
                let production: Vec<String> = match history {
                    Some(h) => crate::converter::ConversionContext::new(dict, Some(conn))
                        .with_history(Some(h), now)
                        .convert_nbest("かなや", n),
                    None => crate::converter::convert_nbest(dict, Some(conn), "かなや", n),
                }
                .iter()
                .map(|p| p.iter().map(|s| s.surface.as_str()).collect())
                .collect();
                assert_eq!(explained, production, "n={n} history={}", history.is_some());
            }
        }
        assert_eq!(rotated_at, 5, "the stale arm rotates at every n >= 2");
        // The deep fixtures are not hollow: the top-1 does depend on n, on the
        // side of the cut the populations above put it.
        let top = |(dict, conn): &(TrieDictionary, ConnectionMatrix), h, n| {
            explain(
                &crate::converter::ConversionContext::new(dict, Some(conn)).with_history(h, now),
                "かなや",
                n,
            )
            .paths[0]
                .surface()
        };
        assert_ne!(top(&deep_argmin, None, 1), top(&deep_argmin, None, 5));
        assert_eq!(top(&deep_argmin, None, 5), "金谷");
        assert_ne!(
            top(&deep_learned, Some(&deep_h), 1),
            top(&deep_learned, Some(&deep_h), 2)
        );
        assert_eq!(
            top(&deep_learned, Some(&deep_h), 2),
            crate::converter::testutil::DEEP_LEARNED
        );
        let (dict, conn) = &far;
        let result = explain(
            &crate::converter::ConversionContext::new(dict, Some(conn)),
            "かなや",
            6,
        );
        assert!(
            result
                .dropped_by_cost_gap
                .iter()
                .any(|d| d.surface == "可なや"),
            "the far arm has an admission drop, so explain == production is checked across one"
        );
    }

    /// A3/T-A3: `viterbi_rank` is the position in `viterbi_nbest`'s output
    /// (after its surface dedup), on a path Viterbi made and the model priced.
    /// 可奈屋 is reached by 可奈|屋 (cheaper) and by one entry; the dup takes
    /// no position, so 加南屋 is at 3, not 4. Rerank's katakana penalty moves
    /// カナヤ to the end of the final order, but not its rank.
    #[test]
    fn viterbi_rank_is_the_position_in_the_deduped_viterbi_output() {
        let e = crate::converter::testutil::entry_with_id;
        let dict = TrieDictionary::from_entries(vec![
            (
                "かなや".into(),
                vec![
                    e("金谷", 0, 1),
                    e("可奈屋", 20, 1),
                    e("加南屋", 40, 1),
                    e("カナヤ", 5, 1),
                ],
            ),
            ("かな".into(), vec![e("可奈", 5, 1)]),
            ("や".into(), vec![e("屋", 5, 1)]),
        ]);
        let conn = ConnectionMatrix::new_owned(2, 0, 0, Vec::new(), vec![0i16; 4]);
        let ctx = crate::converter::ConversionContext::new(&dict, Some(&conn));
        let result = explain(&ctx, "かなや", 20);
        let rank = |surface: &str| path_by_surface(&result, surface).viterbi_rank;
        assert_eq!(rank("金谷"), Some(0));
        assert_eq!(rank("カナヤ"), Some(1));
        assert_eq!(rank("可奈屋"), Some(2));
        assert_eq!(rank("加南屋"), Some(3));
        // Not a Viterbi path priced by the model: a variant offer.
        let variant = path_by_surface(&result, "かなや");
        assert_ne!(variant.origin, PathOrigin::Viterbi);
        assert_eq!(variant.viterbi_rank, None);
        for p in &result.paths {
            assert_eq!(
                p.viterbi_rank.is_some(),
                p.origin == PathOrigin::Viterbi && p.priced_by.is_model(),
                "{}",
                p.surface()
            );
        }
    }

    /// A3/T-A3: a Viterbi path repriced by another stage (the clone path of
    /// `compose_duplicate`) keeps `origin == Viterbi` but has no rank.
    #[test]
    fn viterbi_rank_is_none_for_a_repriced_viterbi_path() {
        let ranks = ViterbiOutput::from([(String::new(), (0, 3usize))]);
        let mut p = ScoredPath::new(Vec::new(), 0, PathOrigin::Viterbi);
        assert_eq!(viterbi_rank_of(&p, &ranks), Some(3));
        for other in [
            PathOrigin::KanjiVariant,
            PathOrigin::PartialHiragana,
            PathOrigin::HiraganaVariant,
            PathOrigin::Katakana,
            PathOrigin::Numeric,
        ] {
            p.priced_by = other;
            assert_eq!(viterbi_rank_of(&p, &ranks), None, "{other:?}");
        }
        let mut reseg = ScoredPath::new(Vec::new(), 0, PathOrigin::Resegment);
        assert_eq!(viterbi_rank_of(&reseg, &ranks), None);
        reseg.priced_by = PathOrigin::Resegment;
        assert_eq!(viterbi_rank_of(&reseg, &ranks), None);
    }

    /// A path that never wins (カナ|や) cannot change which paths survive:
    /// every width and the 1-best agree (#353; before, n=1 saw 可なや and
    /// n=20 仮名屋).
    #[test]
    fn widths_agree_on_the_multi_segment_fixture() {
        let (dict, conn) = multi_segment_kanaya();
        let head = |n| joined(&crate::converter::convert_nbest(&dict, Some(&conn), "かなや", n)[0]);
        let one_best = joined(&crate::converter::convert(&dict, Some(&conn), "かなや"));
        assert_eq!(head(1), head(20));
        assert_eq!(one_best, head(20));
    }

    #[test]
    fn test_explain_empty_reading() {
        let dict = test_dict();
        let result = explain(
            &crate::converter::ConversionContext::new(&dict, None),
            "",
            5,
        );

        assert!(result.lattice_nodes.is_empty());
        assert!(result.paths.is_empty());
    }

    #[test]
    fn test_explain_with_history() {
        let now = now_epoch();
        let dict = test_dict();
        let mut h = UserHistory::new();
        h.record(&[("きょう".into(), "京".into())]);

        let without = explain(
            &crate::converter::ConversionContext::new(&dict, None),
            "きょう",
            5,
        );
        let with = explain(
            &crate::converter::ConversionContext::new(&dict, None).with_history(Some(&h), now),
            "きょう",
            5,
        );

        let without_kyou = without.paths.iter().find(|p| p.surface() == "京");
        let with_kyou = with.paths.iter().find(|p| p.surface() == "京");

        if let (Some(w), Some(wh)) = (without_kyou, with_kyou) {
            assert!(
                wh.history_boost > 0,
                "history boost should be positive for learned entry"
            );
            assert!(
                wh.final_cost < w.final_cost,
                "final cost should be lower with history boost"
            );
            // Single-segment きょう→京: whole-path is the only contributor.
            // Per-segment unigram is also recorded (same reading+surface), so
            // unigram_sum is also nonzero, but bigram_sum should be 0.
            assert!(
                wh.history_breakdown.whole_path_boost > 0,
                "whole-path boost should fire for explicit full-input selection"
            );
            assert_eq!(
                wh.history_breakdown.bigram_sum, 0,
                "single-segment path has no bigram pairs"
            );
        }
    }

    #[test]
    fn test_explain_history_breakdown_empty_without_history() {
        let dict = test_dict();
        let result = explain(
            &crate::converter::ConversionContext::new(&dict, None),
            "きょう",
            5,
        );
        for path in &result.paths {
            assert_eq!(path.history_boost, 0);
            assert_eq!(path.history_breakdown.unigram_sum, 0);
            assert_eq!(path.history_breakdown.bigram_sum, 0);
            assert_eq!(path.history_breakdown.whole_path_boost, 0);
        }
    }

    #[test]
    fn test_explain_history_segment_count_consistent_with_boost() {
        let now = now_epoch();
        // The reported `history_segment_count` is the denominator that
        // `history_rerank` used at normalization time. Without `group_segments`
        // (no conn passed here) the pre-history and final segmentation match,
        // so the field must equal `segments.len()` AND
        // `history_breakdown.applied(history_segment_count)` must reproduce
        // the displayed `history_boost`. Regression for PR #247 R2.
        let dict = test_dict();
        let mut h = UserHistory::new();
        h.record(&[("きょう".into(), "京".into())]);

        let result = explain(
            &crate::converter::ConversionContext::new(&dict, None).with_history(Some(&h), now),
            "きょう",
            5,
        );
        for path in &result.paths {
            assert_eq!(
                path.history_segment_count,
                path.segments.len(),
                "without grouping, history_segment_count should equal segments.len()",
            );
            assert_eq!(
                path.history_boost,
                path.history_breakdown.applied(path.history_segment_count),
                "history_boost must equal applied(history_segment_count)",
            );
        }
    }

    #[test]
    fn test_explain_unrelated_paths_have_zero_history_boost() {
        let now = now_epoch();
        // Paths whose surface does NOT match the recorded history must show a
        // zero breakdown regardless of how they entered the final candidate set:
        //   - Real Viterbi paths that simply don't match (lookup hit, zero score).
        //   - Rewriter-added paths (katakana / numeric) that were synthesised
        //     after history_rerank, so the observer never saw them.
        //
        // Regression for the PR #247 R1 review: previously the breakdown was
        // recomputed against the final (post-grouping / post-rewriter) path,
        // which could produce non-zero values for paths that never received
        // an actual boost in `history_rerank`.
        let dict = test_dict();
        let mut h = UserHistory::new();
        h.record(&[("きょう".into(), "京".into())]);

        let result = explain(
            &crate::converter::ConversionContext::new(&dict, None).with_history(Some(&h), now),
            "きょう",
            10,
        );
        for path in result.paths.iter().filter(|p| p.surface() != "京") {
            assert_eq!(
                path.history_boost,
                0,
                "non-matching surface {:?} must not receive a history boost",
                path.surface(),
            );
            assert_eq!(path.history_breakdown.unigram_sum, 0);
            assert_eq!(path.history_breakdown.bigram_sum, 0);
            assert_eq!(path.history_breakdown.whole_path_boost, 0);
        }
    }

    #[test]
    fn test_explain_paths_sorted_by_final_cost() {
        let dict = test_dict();
        let result = explain(
            &crate::converter::ConversionContext::new(&dict, None),
            "きょう",
            10,
        );

        for window in result.paths.windows(2) {
            assert!(
                window[0].final_cost <= window[1].final_cost,
                "paths should be sorted by final_cost"
            );
        }
    }

    #[test]
    fn test_explain_segment_costs_are_populated() {
        let dict = test_dict();
        let result = explain(
            &crate::converter::ConversionContext::new(&dict, None),
            "きょう",
            5,
        );

        for path in &result.paths {
            for seg in &path.segments {
                assert_eq!(seg.segment_penalty, settings().cost.segment_penalty);
            }
        }
    }

    #[test]
    fn breakdown_adds_up_for_every_price_owner() {
        let now = now_epoch();
        // viterbi + rerank (+ clamp) − history = final, for model paths,
        // offers, repriced paths and the kana rescue alike — with and
        // without history.
        let e = |surface: &str, cost: i16| DictEntry {
            surface: surface.into(),
            cost,
            left_id: 0,
            right_id: 0,
        };
        // 方 is cheap and ほう dear, so the rescue and a Partial offer both
        // price あったほうが; 会った|ほう is a real path a KanjiVariant can
        // reprice.
        let dict = TrieDictionary::from_entries(vec![
            ("あった".into(), vec![e("あった", 0), e("会った", 50)]),
            ("ほう".into(), vec![e("方", 0), e("ほう", 3000)]),
            ("が".into(), vec![e("が", 0), e("蛾", 100)]),
        ]);
        let mut h = UserHistory::new();
        for _ in 0..3 {
            h.record(&[("あったほうが".into(), "あったほうが".into())]);
        }
        let mut owners = std::collections::HashSet::new();
        for history in [None, Some(&h)] {
            let result = explain(
                &crate::converter::ConversionContext::new(&dict, None).with_history(history, now),
                "あったほうが",
                20,
            );
            for p in &result.paths {
                owners.insert(format!("{:?}", p.priced_by));
                assert_eq!(
                    p.viterbi_cost + p.rerank_delta + p.price_clamp.unwrap_or(0) - p.history_boost,
                    p.final_cost,
                    "{} ({:?} priced by {:?})",
                    p.surface(),
                    p.origin,
                    p.priced_by
                );
                assert_eq!(
                    p.price_clamp.is_some(),
                    !p.priced_by.is_model() && p.model_cost.is_some()
                );
            }
        }
        assert!(
            owners.len() >= 3,
            "fixture: several price owners {owners:?}"
        );
    }

    #[test]
    fn dropped_by_cost_gap_lists_what_admission_cut() {
        let dict = crate::converter::testutil::taberu_dict();
        let result = explain(
            &crate::converter::ConversionContext::new(&dict, None),
            "たべる",
            20,
        );
        assert_eq!(
            result.max_cost_gap,
            crate::settings::settings().candidates.max_cost_gap
        );
        let dropped: Vec<&str> = result
            .dropped_by_cost_gap
            .iter()
            .map(|d| d.surface.as_str())
            .collect();
        assert!(dropped.contains(&"田辺留"), "{dropped:?}");
        let listed: Vec<String> = result.paths.iter().map(ExplainPath::surface).collect();
        for d in &result.dropped_by_cost_gap {
            assert!(d.gap > result.cost_gap_bound, "{} gap {}", d.surface, d.gap);
            assert!(
                !listed.contains(&d.surface),
                "{} is both listed and dropped",
                d.surface
            );
        }
        let gaps: Vec<i64> = result.dropped_by_cost_gap.iter().map(|d| d.gap).collect();
        assert!(gaps.windows(2).all(|w| w[0] <= w[1]), "by gap: {gaps:?}");
        assert!(format_text(&result).contains("Dropped by cost gap"));
    }

    /// 可|な|や (two 5000 transitions, in the Viterbi price) is not a
    /// candidate behind 仮名屋: rerank keeps it and admission drops it, so
    /// it is listed under the cost gap, not among the paths.
    #[test]
    fn multi_segment_path_is_dropped_by_admission_not_rerank() {
        let (dict, conn) = crate::converter::testutil::far_multi_segment_kanaya();
        let result = explain(
            &crate::converter::ConversionContext::new(&dict, Some(&conn)),
            "かなや",
            20,
        );
        let dropped = result
            .dropped_by_cost_gap
            .iter()
            .find(|d| d.surface == "可なや")
            .expect("admission dropped 可なや");
        assert!(dropped.gap > result.cost_gap_bound, "{}", dropped.gap);
        let listed: Vec<String> = result.paths.iter().map(ExplainPath::surface).collect();
        assert!(!listed.contains(&"可なや".to_string()), "{listed:?}");
        // Asking why a surface is missing can leave no paths; the dropped
        // section still answers it.
        let mut none_listed = result;
        none_listed.paths.clear();
        let text = format_text(&none_listed);
        assert!(text.contains("No paths found") && text.contains("Dropped by cost gap"));
    }
}
