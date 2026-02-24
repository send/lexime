//! Kana-to-kanji conversion via lattice construction and Viterbi search.
//!
//! Builds a character-level lattice from dictionary lookups, then runs
//! N-best Viterbi with connection costs and post-processing (reranking,
//! segment grouping, history boosting).

#[cfg(feature = "neural")]
pub(crate) mod constrained;
pub(crate) mod cost;
pub mod explain;
mod lattice;
mod postprocess;
pub(crate) mod reranker;
mod resegment;
pub(crate) mod rewriter;
pub(crate) mod testutil;
mod viterbi;

#[cfg(test)]
mod tests;

use crate::dict::connection::ConnectionMatrix;
use crate::dict::Dictionary;
use crate::user_history::UserHistory;

use cost::DefaultCostFunction;
use postprocess::postprocess;

pub use lattice::{build_lattice, Lattice, LatticeNode};
pub use viterbi::ConvertedSegment;
#[allow(unused_imports)]
pub(crate) use viterbi::{viterbi_nbest, RichSegment, ScoredPath};

/// N-best Viterbi with a prefix constraint (for speculative decoding).
///
/// Fixed segments in the prefix are enforced via a prohibitive cost for
/// non-matching nodes. The suffix is explored freely.
#[cfg(feature = "neural")]
pub(crate) fn convert_nbest_constrained(
    dict: &dyn Dictionary,
    conn: Option<&ConnectionMatrix>,
    kana: &str,
    constraint: &constrained::PrefixConstraint,
    n: usize,
) -> Vec<ScoredPath> {
    if kana.is_empty() || n == 0 {
        return Vec::new();
    }
    let cost_fn = constrained::PrefixConstrainedCost::new(conn, constraint);
    let lattice = build_lattice(dict, kana);
    let oversample = n * 3;
    let mut paths = viterbi_nbest(&lattice, &cost_fn, oversample);
    // Apply reranking but not grouping (speculative decode needs raw segments)
    reranker::rerank(&mut paths, conn);
    paths.truncate(n);
    paths
}

/// Convert a kana string to the best segmentation using Viterbi algorithm.
///
/// If `conn` is provided, uses connection costs for scoring transitions.
/// Otherwise, falls back to unigram-only scoring (sum of word costs).
pub fn convert(
    dict: &dyn Dictionary,
    conn: Option<&ConnectionMatrix>,
    kana: &str,
) -> Vec<ConvertedSegment> {
    if kana.is_empty() {
        return Vec::new();
    }
    let cost_fn = DefaultCostFunction::new(conn);
    let lattice = build_lattice(dict, kana);
    let mut paths = viterbi_nbest(&lattice, &cost_fn, 10);
    postprocess(&mut paths, &lattice, conn, None, kana, 1)
        .into_iter()
        .next()
        .unwrap_or_default()
}

/// Convert a kana string to the N-best segmentations using Viterbi algorithm.
///
/// Internally generates more candidates than `n`, applies reranking, then
/// returns the top `n` distinct paths.
pub fn convert_nbest(
    dict: &dyn Dictionary,
    conn: Option<&ConnectionMatrix>,
    kana: &str,
    n: usize,
) -> Vec<Vec<ConvertedSegment>> {
    if kana.is_empty() || n == 0 {
        return Vec::new();
    }
    let cost_fn = DefaultCostFunction::new(conn);
    let lattice = build_lattice(dict, kana);
    let oversample = n * 3;
    let mut paths = viterbi_nbest(&lattice, &cost_fn, oversample);
    postprocess(&mut paths, &lattice, conn, None, kana, n)
}

/// 1-best conversion with history-aware reranking.
///
/// Viterbi runs with `DefaultCostFunction` (no learned boosts), then
/// `rerank` + `history_rerank` are applied on the N-best list. This avoids
/// boost-induced lattice fragmentation while still surfacing learned
/// candidates.
pub fn convert_with_history(
    dict: &dyn Dictionary,
    conn: Option<&ConnectionMatrix>,
    history: &UserHistory,
    kana: &str,
) -> Vec<ConvertedSegment> {
    if kana.is_empty() {
        return Vec::new();
    }
    let cost_fn = DefaultCostFunction::new(conn);
    let lattice = build_lattice(dict, kana);
    let mut paths = viterbi_nbest(&lattice, &cost_fn, 30);
    postprocess(&mut paths, &lattice, conn, Some(history), kana, 1)
        .into_iter()
        .next()
        .unwrap_or_default()
}

/// N-best conversion with history-aware reranking.
///
/// Viterbi runs with `DefaultCostFunction`, then `rerank` +
/// `history_rerank` are applied. The oversample is set to
/// `max(n*3, 50)` to ensure enough diversity for the reranker to find
/// learned candidates.
pub fn convert_nbest_with_history(
    dict: &dyn Dictionary,
    conn: Option<&ConnectionMatrix>,
    history: &UserHistory,
    kana: &str,
    n: usize,
) -> Vec<Vec<ConvertedSegment>> {
    if kana.is_empty() || n == 0 {
        return Vec::new();
    }
    let cost_fn = DefaultCostFunction::new(conn);
    let lattice = build_lattice(dict, kana);
    let oversample = (n * 3).max(50);
    let mut paths = viterbi_nbest(&lattice, &cost_fn, oversample);
    postprocess(&mut paths, &lattice, conn, Some(history), kana, n)
}
