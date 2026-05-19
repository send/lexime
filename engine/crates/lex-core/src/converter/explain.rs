use std::collections::HashMap;

use serde::Serialize;

use crate::dict::connection::ConnectionMatrix;
use crate::dict::Dictionary;
use crate::user_history::UserHistory;

use crate::settings::settings;

use super::cost::{conn_cost, script_cost, DefaultCostFunction};
use super::features::{is_single_char_kanji_penalised, is_te_form_kanji_penalised};
use super::lattice::{build_lattice, Lattice};
use super::postprocess::{postprocess_observed, PostprocessContext, PostprocessObserver};
use super::reranker::compute_history_boost;
use super::viterbi::{viterbi_nbest, ScoredPath};

// Re-export so downstream crates (e.g. lex-cli) can name the type behind
// `ExplainPath::history_breakdown` — the definition lives in the crate-private
// `reranker` module.
pub use super::reranker::HistoryBoostBreakdown;

/// Full diagnostic result for a single reading.
#[derive(Debug, Serialize)]
pub struct ExplainResult {
    pub reading: String,
    pub lattice_char_count: usize,
    pub lattice_nodes: Vec<ExplainNode>,
    pub paths: Vec<ExplainPath>,
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
    /// Cost delta from structure reranking.
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
}

impl ExplainPath {
    pub fn surface(&self) -> String {
        self.segments.iter().map(|s| s.surface.as_str()).collect()
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
    /// Single-char kanji content-word penalty applied.
    pub single_char_kanji_penalty: i64,
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
    /// Cost after resegment + rerank, before any history adjustment.
    cost: i64,
    /// Per-component history boost (raw sums + whole-path × 5).
    breakdown: HistoryBoostBreakdown,
    /// Boost actually subtracted from `cost` by `history_rerank`.
    applied_boost: i64,
    /// Segment count at the moment `history_rerank` saw the path. May differ
    /// from the final `segments.len()` after `group_segments` merges adjacent
    /// segments — kept here so the displayed `/N segs` matches the denominator
    /// actually used during normalization.
    segment_count: usize,
}

/// Diagnostic observer.
///
/// Keys are `ScoredPath::surface_key()` — i.e. the concatenated surface — so
/// that lookups survive `group_segments` (which merges adjacent segments but
/// preserves the overall surface). Paths that only appear after history_rerank
/// (rewriter-added candidates: numeric, katakana, kanji variants) are absent
/// from these maps and fall back to zero in the caller.
struct ExplainObserver<'a> {
    history: Option<&'a UserHistory>,
    now: u64,
    /// viterbi_cost before resegment/rerank — the raw Viterbi output.
    original_costs: HashMap<String, i64>,
    /// State at the post-rerank / pre-history-rerank boundary.
    pre_history: HashMap<String, PreHistorySnapshot>,
}

impl<'a> ExplainObserver<'a> {
    fn new(history: Option<&'a UserHistory>, now: u64) -> Self {
        Self {
            history,
            now,
            original_costs: HashMap::new(),
            pre_history: HashMap::new(),
        }
    }
}

impl PostprocessObserver for ExplainObserver<'_> {
    fn after_viterbi(&mut self, paths: &[ScoredPath]) {
        self.original_costs = paths
            .iter()
            .map(|p| (p.surface_key(), p.viterbi_cost))
            .collect();
    }

    fn after_rerank(&mut self, paths: &[ScoredPath]) {
        self.pre_history.clear();
        for p in paths {
            let (breakdown, applied) = match self.history {
                Some(h) => {
                    let b = compute_history_boost(p, h, self.now);
                    let a = b.applied(p.segments.len());
                    (b, a)
                }
                None => (HistoryBoostBreakdown::default(), 0),
            };
            self.pre_history.insert(
                p.surface_key(),
                PreHistorySnapshot {
                    cost: p.viterbi_cost,
                    breakdown,
                    applied_boost: applied,
                    segment_count: p.segments.len(),
                },
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Per-segment cost breakdown
// ---------------------------------------------------------------------------

/// Build per-segment cost breakdown from a ScoredPath.
fn explain_segments(
    scored: &ScoredPath,
    conn: Option<&ConnectionMatrix>,
    dict: &dyn Dictionary,
) -> Vec<ExplainSegment> {
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
            let sc_penalty = if let Some(c) = conn {
                if is_single_char_kanji_penalised(seg, i, &scored.segments, c, Some(dict)) {
                    settings().reranker.single_char_kanji_penalty
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
                single_char_kanji_penalty: sc_penalty,
                left_id: seg.left_id,
                right_id: seg.right_id,
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Run the full conversion pipeline and capture detailed cost breakdown.
///
/// Uses `postprocess_observed` to follow the exact same pipeline as
/// production conversion, with an observer that records cost snapshots
/// at each stage for diagnostic output.
pub fn explain(
    dict: &dyn Dictionary,
    conn: Option<&ConnectionMatrix>,
    history: Option<&UserHistory>,
    kana: &str,
    n: usize,
) -> ExplainResult {
    if kana.is_empty() || n == 0 {
        return ExplainResult {
            reading: kana.to_string(),
            lattice_char_count: 0,
            lattice_nodes: Vec::new(),
            paths: Vec::new(),
        };
    }

    let lattice = build_lattice(dict, kana);
    let lattice_nodes: Vec<ExplainNode> = (0..lattice.node_count())
        .map(|idx| ExplainNode::from_lattice(&lattice, idx))
        .collect();

    let cost_fn = DefaultCostFunction::new(conn);
    let oversample = (n * 3).max(50);
    let mut raw_paths = viterbi_nbest(&lattice, &cost_fn, oversample);

    let now = crate::user_history::now_epoch();
    let mut observer = ExplainObserver::new(history, now);
    let ctx = PostprocessContext {
        lattice: &lattice,
        conn,
        dict: Some(dict),
        history,
        kana,
        n,
    };
    let final_paths = postprocess_observed(&mut raw_paths, &ctx, &mut observer);

    let paths: Vec<ExplainPath> = final_paths
        .iter()
        .map(|scored| {
            let key = scored.surface_key();
            // Look up snapshots by surface_key — preserved through group_segments.
            // Rewriter-added candidates (numeric / katakana / kanji variants) are
            // synthesised after history_rerank and have no snapshot, so they fall
            // back to zero history boost and use the final cost for `viterbi_cost`.
            let original = observer
                .original_costs
                .get(&key)
                .copied()
                .unwrap_or(scored.viterbi_cost);
            let snapshot = observer
                .pre_history
                .get(&key)
                .copied()
                .unwrap_or(PreHistorySnapshot {
                    cost: original,
                    breakdown: HistoryBoostBreakdown::default(),
                    applied_boost: 0,
                    segment_count: scored.segments.len(),
                });
            ExplainPath {
                segments: explain_segments(scored, conn, dict),
                viterbi_cost: original,
                rerank_delta: snapshot.cost - original,
                history_breakdown: snapshot.breakdown,
                history_boost: snapshot.applied_boost,
                history_segment_count: snapshot.segment_count,
                final_cost: scored.viterbi_cost,
            }
        })
        .collect();

    ExplainResult {
        reading: kana.to_string(),
        lattice_char_count: lattice.char_count,
        lattice_nodes,
        paths,
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
        return out;
    }

    out.push_str(&format!("\n=== Paths ({}) ===\n", result.paths.len()));
    for (i, path) in result.paths.iter().enumerate() {
        let surface = path.surface();
        out.push_str(&format!(
            "\n  #{:<2} {}  (final_cost={})\n",
            i + 1,
            surface,
            path.final_cost,
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
            let single_char_str = if seg.single_char_kanji_penalty > 0 {
                format!(" 1charK={:<+6}", seg.single_char_kanji_penalty)
            } else {
                String::new()
            };
            out.push_str(&format!(
                "    seg[{}]: {} word={:<6} penalty={:<5} script={:<6} {}{}{}{}\n",
                j,
                padded,
                seg.word_cost,
                seg.segment_penalty,
                seg.script_cost,
                conn_label,
                seg.connection_cost,
                te_str,
                single_char_str,
            ));
        }

        out.push_str(&format!(
            "    viterbi={:<8} rerank={:<+8} history={:<+8} -> final={}\n",
            path.viterbi_cost, path.rerank_delta, -path.history_boost, path.final_cost,
        ));
        let hb = &path.history_breakdown;
        if hb.unigram_sum != 0 || hb.bigram_sum != 0 || hb.whole_path_boost != 0 {
            out.push_str(&format!(
                "      history: uni_sum={:<+7} bi_sum={:<+7} whole×5={:<+7} (/{} segs)\n",
                -hb.unigram_sum, -hb.bigram_sum, -hb.whole_path_boost, path.history_segment_count,
            ));
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::converter::testutil::test_dict;
    use crate::user_history::UserHistory;

    #[test]
    fn test_explain_basic() {
        let dict = test_dict();
        let result = explain(&dict, None, None, "きょう", 5);

        assert_eq!(result.reading, "きょう");
        assert!(!result.lattice_nodes.is_empty());
        assert!(!result.paths.is_empty());

        // Best path should have segments
        let best = &result.paths[0];
        assert!(!best.segments.is_empty());
    }

    #[test]
    fn test_explain_empty_reading() {
        let dict = test_dict();
        let result = explain(&dict, None, None, "", 5);

        assert!(result.lattice_nodes.is_empty());
        assert!(result.paths.is_empty());
    }

    #[test]
    fn test_explain_with_history() {
        let dict = test_dict();
        let mut h = UserHistory::new();
        h.record(&[("きょう".into(), "京".into())]);

        let without = explain(&dict, None, None, "きょう", 5);
        let with = explain(&dict, None, Some(&h), "きょう", 5);

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
        let result = explain(&dict, None, None, "きょう", 5);
        for path in &result.paths {
            assert_eq!(path.history_boost, 0);
            assert_eq!(path.history_breakdown.unigram_sum, 0);
            assert_eq!(path.history_breakdown.bigram_sum, 0);
            assert_eq!(path.history_breakdown.whole_path_boost, 0);
        }
    }

    #[test]
    fn test_explain_history_segment_count_consistent_with_boost() {
        // The reported `history_segment_count` is the denominator that
        // `history_rerank` used at normalization time. Without `group_segments`
        // (no conn passed here) the pre-history and final segmentation match,
        // so the field must equal `segments.len()` AND
        // `history_breakdown.applied(history_segment_count)` must reproduce
        // the displayed `history_boost`. Regression for PR #247 R2.
        let dict = test_dict();
        let mut h = UserHistory::new();
        h.record(&[("きょう".into(), "京".into())]);

        let result = explain(&dict, None, Some(&h), "きょう", 5);
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
        // Paths whose surface does NOT match the recorded history must show a
        // zero breakdown regardless of how they entered the final candidate set:
        //   - Real Viterbi paths that simply don't match (lookup hit, zero score).
        //   - Rewriter-added paths (katakana / kanji variants) that were
        //     synthesised after history_rerank, so the observer never saw them.
        //
        // Regression for the PR #247 R1 review: previously the breakdown was
        // recomputed against the final (post-grouping / post-rewriter) path,
        // which could produce non-zero values for paths that never received
        // an actual boost in `history_rerank`.
        let dict = test_dict();
        let mut h = UserHistory::new();
        h.record(&[("きょう".into(), "京".into())]);

        let result = explain(&dict, None, Some(&h), "きょう", 10);
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
        let result = explain(&dict, None, None, "きょう", 10);

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
        let result = explain(&dict, None, None, "きょう", 5);

        for path in &result.paths {
            for seg in &path.segments {
                assert_eq!(seg.segment_penalty, settings().cost.segment_penalty);
            }
        }
    }
}
