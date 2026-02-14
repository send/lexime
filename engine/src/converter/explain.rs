use serde::Serialize;

use crate::dict::connection::ConnectionMatrix;
use crate::dict::Dictionary;
use crate::user_history::UserHistory;

use super::cost::DefaultCostFunction;
use super::lattice::{build_lattice, LatticeNode};
use super::reranker;
use super::viterbi::viterbi_nbest;

/// Full diagnostic result for a single reading.
#[derive(Debug, Serialize)]
pub struct ExplainResult {
    pub reading: String,
    pub lattice_char_count: usize,
    pub lattice_nodes: Vec<ExplainNode>,
    pub paths_before_rerank: Vec<ExplainPath>,
    pub paths_after_rerank: Vec<ExplainPath>,
    pub paths_final: Vec<ExplainPath>,
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

/// A conversion path for diagnostic display.
#[derive(Debug, Serialize)]
pub struct ExplainPath {
    pub segments: Vec<ExplainSegment>,
    pub viterbi_cost: i64,
}

/// A segment within a conversion path.
#[derive(Debug, Serialize)]
pub struct ExplainSegment {
    pub reading: String,
    pub surface: String,
    pub word_cost: i16,
    pub left_id: u16,
    pub right_id: u16,
}

/// Run the full conversion pipeline and capture intermediate results at each stage.
pub fn explain(
    dict: &dyn Dictionary,
    conn: Option<&ConnectionMatrix>,
    history: Option<&UserHistory>,
    kana: &str,
    n: usize,
) -> ExplainResult {
    let lattice = build_lattice(dict, kana);
    let lattice_nodes: Vec<ExplainNode> = lattice.nodes.iter().map(ExplainNode::from).collect();

    let cost_fn = DefaultCostFunction::new(conn);
    let oversample = (n * 3).max(50);
    let mut paths = viterbi_nbest(&lattice, &cost_fn, oversample);

    let paths_before_rerank: Vec<ExplainPath> = paths
        .iter()
        .take(n)
        .map(|p| ExplainPath {
            segments: p
                .segments
                .iter()
                .map(|s| ExplainSegment {
                    reading: s.reading.clone(),
                    surface: s.surface.clone(),
                    word_cost: s.word_cost,
                    left_id: s.left_id,
                    right_id: s.right_id,
                })
                .collect(),
            viterbi_cost: p.viterbi_cost,
        })
        .collect();

    reranker::rerank(&mut paths, conn);

    let paths_after_rerank: Vec<ExplainPath> = paths
        .iter()
        .take(n)
        .map(|p| ExplainPath {
            segments: p
                .segments
                .iter()
                .map(|s| ExplainSegment {
                    reading: s.reading.clone(),
                    surface: s.surface.clone(),
                    word_cost: s.word_cost,
                    left_id: s.left_id,
                    right_id: s.right_id,
                })
                .collect(),
            viterbi_cost: p.viterbi_cost,
        })
        .collect();

    if let Some(h) = history {
        reranker::history_rerank(&mut paths, h);
    }

    let paths_final: Vec<ExplainPath> = paths
        .into_iter()
        .take(n)
        .map(|p| ExplainPath {
            segments: p
                .segments
                .into_iter()
                .map(|s| ExplainSegment {
                    reading: s.reading,
                    surface: s.surface,
                    word_cost: s.word_cost,
                    left_id: s.left_id,
                    right_id: s.right_id,
                })
                .collect(),
            viterbi_cost: p.viterbi_cost,
        })
        .collect();

    ExplainResult {
        reading: kana.to_string(),
        lattice_char_count: lattice.char_count,
        lattice_nodes,
        paths_before_rerank,
        paths_after_rerank,
        paths_final,
    }
}

/// Format an ExplainResult as human-readable text.
pub fn format_text(result: &ExplainResult) -> String {
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
        out.push_str(&format!("Position {}:\n", pos));
        for n in nodes_at_pos {
            out.push_str(&format!(
                "  [{}-{}] {} → {} (cost={}, L={}, R={})\n",
                n.start, n.end, n.reading, n.surface, n.cost, n.left_id, n.right_id,
            ));
        }
    }

    fn format_paths(out: &mut String, header: &str, paths: &[ExplainPath]) {
        out.push_str(&format!("\n=== {} ===\n", header));
        for (i, path) in paths.iter().enumerate() {
            let surface: String = path.segments.iter().map(|s| s.surface.as_str()).collect();
            out.push_str(&format!(
                "#{} [cost={}] {}\n",
                i + 1,
                path.viterbi_cost,
                surface
            ));
            for seg in &path.segments {
                out.push_str(&format!(
                    "    {} → {} (word={}, L={}, R={})\n",
                    seg.reading, seg.surface, seg.word_cost, seg.left_id, seg.right_id,
                ));
            }
        }
    }

    format_paths(
        &mut out,
        "Viterbi N-best (before rerank)",
        &result.paths_before_rerank,
    );
    format_paths(&mut out, "After rerank", &result.paths_after_rerank);
    format_paths(
        &mut out,
        "Final (after history rerank)",
        &result.paths_final,
    );

    out
}
