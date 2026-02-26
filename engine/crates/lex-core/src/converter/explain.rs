use serde::Serialize;

use crate::dict::connection::ConnectionMatrix;
use crate::dict::Dictionary;
use crate::user_history::UserHistory;

use crate::settings::settings;

use super::cost::{conn_cost, script_cost, DefaultCostFunction};
use super::lattice::{build_lattice, LatticeNode};
use super::reranker;
use super::viterbi::{viterbi_nbest, ScoredPath};

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

impl From<&LatticeNode> for ExplainNode {
    fn from(n: &LatticeNode) -> Self {
        Self {
            start: n.start,
            end: n.end,
            reading: n.reading.clone(),
            surface: n.surface.clone(),
            cost: n.cost,
            left_id: n.left_id,
            right_id: n.right_id,
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
    /// Total history boost applied (negative = better).
    pub history_boost: i64,
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
    pub left_id: u16,
    pub right_id: u16,
}

/// Build a key string from a ScoredPath for cost tracking across reranker passes.
/// Uses ASCII control characters (US=\x1f, RS=\x1e) as delimiters to avoid
/// collisions with any reading/surface content.
fn path_key(path: &ScoredPath) -> String {
    path.segments
        .iter()
        .map(|s| format!("{}\x1f{}", s.reading, s.surface))
        .collect::<Vec<_>>()
        .join("\x1e")
}

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
            ExplainSegment {
                reading: seg.reading.clone(),
                surface: seg.surface.clone(),
                word_cost: seg.word_cost as i64,
                segment_penalty: settings().cost.segment_penalty,
                script_cost: script_cost(&seg.surface, seg.reading.chars().count()),
                connection_cost: connection,
                left_id: seg.left_id,
                right_id: seg.right_id,
            }
        })
        .collect()
}

/// Run the full conversion pipeline and capture detailed cost breakdown.
pub fn explain(
    dict: &dyn Dictionary,
    conn: Option<&ConnectionMatrix>,
    history: Option<&UserHistory>,
    kana: &str,
    n: usize,
) -> ExplainResult {
    use std::collections::HashMap;

    let lattice = build_lattice(dict, kana);
    let lattice_nodes: Vec<ExplainNode> = lattice.nodes.iter().map(ExplainNode::from).collect();

    let cost_fn = DefaultCostFunction::new(conn);
    let oversample = (n * 3).max(50);
    let mut raw_paths = viterbi_nbest(&lattice, &cost_fn, oversample);

    // 1. Record original viterbi costs
    let original_costs: HashMap<String, i64> = raw_paths
        .iter()
        .map(|p| (path_key(p), p.viterbi_cost))
        .collect();

    // 2. Apply real reranker (structure cost + length variance + script cost)
    reranker::rerank(&mut raw_paths, conn);

    // 3. Record post-rerank costs
    let post_rerank_costs: HashMap<String, i64> = raw_paths
        .iter()
        .map(|p| (path_key(p), p.viterbi_cost))
        .collect();

    // 4. Apply history reranker
    if let Some(h) = history {
        reranker::history_rerank(&mut raw_paths, h);
    }

    // 5. Truncate to requested count
    raw_paths.truncate(n);

    // 6. Build explained paths with cost deltas
    let paths: Vec<ExplainPath> = raw_paths
        .iter()
        .map(|scored| {
            let key = path_key(scored);
            let original = original_costs
                .get(&key)
                .copied()
                .unwrap_or(scored.viterbi_cost);
            let post_rerank = post_rerank_costs.get(&key).copied().unwrap_or(original);
            let rerank_delta = post_rerank - original;
            let history_boost = post_rerank - scored.viterbi_cost;
            ExplainPath {
                segments: explain_segments(scored, conn),
                viterbi_cost: original,
                rerank_delta,
                history_boost,
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
            out.push_str(&format!(
                "    seg[{}]: {} word={:<6} penalty={:<5} script={:<6} {}{}\n",
                j,
                padded,
                seg.word_cost,
                seg.segment_penalty,
                seg.script_cost,
                conn_label,
                seg.connection_cost,
            ));
        }

        out.push_str(&format!(
            "    viterbi={:<8} rerank={:<+8} history={:<+8} -> final={}\n",
            path.viterbi_cost, path.rerank_delta, -path.history_boost, path.final_cost,
        ));
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
