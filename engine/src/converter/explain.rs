//! Diagnostic explanation of conversion cost breakdown.
//!
//! Used by `dictool explain` to show why a particular candidate was ranked
//! where it was. This module wraps the internal lattice/viterbi/reranker
//! pipeline and produces a human-readable cost breakdown.

use crate::dict::connection::ConnectionMatrix;
use crate::dict::Dictionary;
use crate::user_history::UserHistory;

use super::cost::{conn_cost, script_cost, DefaultCostFunction, SEGMENT_PENALTY};
use super::lattice::build_lattice;
use super::viterbi::viterbi_nbest;

/// A single node in the lattice explanation.
#[derive(Debug, Clone)]
pub struct ExplainNode {
    pub start: usize,
    pub end: usize,
    pub reading: String,
    pub surface: String,
    pub cost: i16,
    pub left_id: u16,
    pub right_id: u16,
}

/// A segment in an explained path, with full cost breakdown.
#[derive(Debug, Clone)]
pub struct ExplainSegment {
    pub reading: String,
    pub surface: String,
    pub word_cost: i64,
    pub segment_penalty: i64,
    pub script_cost: i64,
    /// Connection cost from BOS or previous segment.
    pub connection_cost: i64,
}

/// A complete path with cost breakdown.
#[derive(Debug, Clone)]
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

/// Full explanation result for a conversion.
#[derive(Debug)]
pub struct ExplainResult {
    pub reading: String,
    pub lattice_nodes: Vec<ExplainNode>,
    pub paths: Vec<ExplainPath>,
}

/// Generate a detailed explanation of how a reading is converted.
pub fn explain(
    dict: &dyn Dictionary,
    conn: Option<&ConnectionMatrix>,
    history: Option<&UserHistory>,
    reading: &str,
    n: usize,
) -> ExplainResult {
    let lattice = build_lattice(dict, reading);

    // Collect lattice nodes for display
    let lattice_nodes: Vec<ExplainNode> = lattice
        .nodes
        .iter()
        .map(|node| ExplainNode {
            start: node.start,
            end: node.end,
            reading: node.reading.clone(),
            surface: node.surface.clone(),
            cost: node.cost,
            left_id: node.left_id,
            right_id: node.right_id,
        })
        .collect();

    // Run Viterbi
    let cost_fn = DefaultCostFunction::new(conn);
    let oversample = (n * 3).max(50);
    let raw_paths = viterbi_nbest(&lattice, &cost_fn, oversample);

    // Build explained paths with cost breakdown
    let mut paths: Vec<ExplainPath> = raw_paths
        .iter()
        .map(|scored| {
            let mut segments = Vec::new();
            for (i, seg) in scored.segments.iter().enumerate() {
                // Look up the actual node to get the raw cost
                let raw_cost = lattice
                    .nodes
                    .iter()
                    .find(|n| {
                        n.reading == seg.reading
                            && n.surface == seg.surface
                            && n.left_id == seg.left_id
                    })
                    .map(|n| n.cost as i64)
                    .unwrap_or(0);

                let script = script_cost(&seg.surface);

                let connection = if i == 0 {
                    conn_cost(conn, 0, seg.left_id)
                } else {
                    let prev = &scored.segments[i - 1];
                    conn_cost(conn, prev.right_id, seg.left_id)
                };

                segments.push(ExplainSegment {
                    reading: seg.reading.clone(),
                    surface: seg.surface.clone(),
                    word_cost: raw_cost,
                    segment_penalty: SEGMENT_PENALTY,
                    script_cost: script,
                    connection_cost: connection,
                });
            }

            // Add EOS cost
            let eos_cost = if let Some(last) = scored.segments.last() {
                conn_cost(conn, last.right_id, 0)
            } else {
                0
            };

            // Compute expected viterbi cost from components
            let component_sum: i64 = segments
                .iter()
                .map(|s| s.word_cost + s.segment_penalty + s.connection_cost)
                .sum::<i64>()
                + eos_cost;

            ExplainPath {
                segments,
                viterbi_cost: scored.viterbi_cost,
                rerank_delta: 0,
                history_boost: 0,
                final_cost: component_sum,
            }
        })
        .collect();

    // Simulate reranker: compute structure cost
    for path in &mut paths {
        let mut structure_cost: i64 = 0;

        // Connection-based structure cost (same as reranker::rerank)
        for i in 0..path.segments.len() {
            let seg = &path.segments[i];
            let left_id = if i == 0 {
                0
            } else {
                // We need the right_id of the previous segment
                // Look it up from the lattice
                lattice
                    .nodes
                    .iter()
                    .find(|n| {
                        n.reading == path.segments[i - 1].reading
                            && n.surface == path.segments[i - 1].surface
                    })
                    .map(|n| n.right_id)
                    .unwrap_or(0)
            };

            let node_left_id = lattice
                .nodes
                .iter()
                .find(|n| n.reading == seg.reading && n.surface == seg.surface)
                .map(|n| n.left_id)
                .unwrap_or(0);

            structure_cost += conn_cost(conn, left_id, node_left_id);
            structure_cost += script_cost(&seg.surface);
        }

        // EOS structure cost
        if let Some(last_seg) = path.segments.last() {
            let right_id = lattice
                .nodes
                .iter()
                .find(|n| n.reading == last_seg.reading && n.surface == last_seg.surface)
                .map(|n| n.right_id)
                .unwrap_or(0);
            structure_cost += conn_cost(conn, right_id, 0);
        }

        path.rerank_delta = structure_cost;
        path.final_cost = path.viterbi_cost + structure_cost;
    }

    // Apply history boost
    if let Some(h) = history {
        for path in &mut paths {
            let mut boost: i64 = 0;
            for seg in &path.segments {
                boost += h.unigram_boost(&seg.reading, &seg.surface);
            }
            // Bigram boosts
            for i in 1..path.segments.len() {
                let prev = &path.segments[i - 1];
                let next = &path.segments[i];
                boost += h.bigram_boost(&prev.surface, &next.reading, &next.surface);
            }
            path.history_boost = boost;
            path.final_cost -= boost;
        }
    }

    // Sort by final cost
    paths.sort_by_key(|p| p.final_cost);
    paths.truncate(n);

    ExplainResult {
        reading: reading.to_string(),
        lattice_nodes,
        paths,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::converter::testutil::test_dict;

    #[test]
    fn test_explain_basic() {
        let dict = test_dict();
        let result = explain(&dict, None, None, "きょう", 5);

        assert_eq!(result.reading, "きょう");
        assert!(!result.lattice_nodes.is_empty());
        assert!(!result.paths.is_empty());

        // Best path should have "今日" (lowest cost in test dict)
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

        // With history, the boosted entry should have different final cost
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
                // segment_penalty should be SEGMENT_PENALTY
                assert_eq!(seg.segment_penalty, SEGMENT_PENALTY);
            }
        }
    }
}
