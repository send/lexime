//! Kana-to-kanji conversion via lattice construction and Viterbi search.
//!
//! The conversion pipeline has two explicit steps:
//!
//! 1. **Lattice construction** (`build_lattice`) — builds a character-level
//!    lattice from dictionary lookups.
//! 2. **Conversion** (`convert_from_lattice` / `convert_nbest_from_lattice`) —
//!    runs N-best Viterbi with post-processing (reranking, segment grouping,
//!    history boosting).
//!
//! Separating these steps allows callers to reuse a lattice across multiple
//! conversions (e.g. sync 1-best + async N-best in deferred candidate mode).

#[cfg(feature = "neural")]
pub(crate) mod constrained;
pub(crate) mod cost;
pub mod explain;
pub(crate) mod features;
mod lattice;
mod postprocess;
pub(crate) mod reranker;
mod resegment;
pub(crate) mod rewriter;
pub(crate) mod testutil;
pub mod tune;
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

// ---------------------------------------------------------------------------
// Primary API — lattice is an explicit parameter
// ---------------------------------------------------------------------------

/// 1-best conversion from a pre-built lattice.
///
/// Pass `history` to enable user-history reranking (boosts previously
/// selected candidates). The oversample factor is higher with history
/// to ensure enough diversity for the reranker.
pub fn convert_from_lattice(
    lattice: &Lattice,
    dict: &dyn Dictionary,
    conn: Option<&ConnectionMatrix>,
    history: Option<&UserHistory>,
    kana: &str,
) -> Vec<ConvertedSegment> {
    if kana.is_empty() {
        return Vec::new();
    }
    let cost_fn = DefaultCostFunction::new(conn);
    let oversample = if history.is_some() { 30 } else { 10 };
    let mut paths = viterbi_nbest(lattice, &cost_fn, oversample);
    postprocess(&mut paths, lattice, conn, Some(dict), history, kana, 1)
        .into_iter()
        .next()
        .unwrap_or_default()
}

/// N-best conversion from a pre-built lattice.
///
/// Internally generates more candidates than `n`, applies reranking,
/// then returns the top `n` distinct paths. With history the oversample
/// is set to `max(n*3, 50)` for diversity.
pub fn convert_nbest_from_lattice(
    lattice: &Lattice,
    dict: &dyn Dictionary,
    conn: Option<&ConnectionMatrix>,
    history: Option<&UserHistory>,
    kana: &str,
    n: usize,
) -> Vec<Vec<ConvertedSegment>> {
    if kana.is_empty() || n == 0 {
        return Vec::new();
    }
    let cost_fn = DefaultCostFunction::new(conn);
    let oversample = if history.is_some() {
        (n * 3).max(50)
    } else {
        n * 3
    };
    let mut paths = viterbi_nbest(lattice, &cost_fn, oversample);
    postprocess(&mut paths, lattice, conn, Some(dict), history, kana, n)
}

// ---------------------------------------------------------------------------
// Convenience wrappers — build lattice internally
// ---------------------------------------------------------------------------

/// Convert a kana string to the best segmentation using Viterbi algorithm.
pub fn convert(
    dict: &dyn Dictionary,
    conn: Option<&ConnectionMatrix>,
    kana: &str,
) -> Vec<ConvertedSegment> {
    let lattice = build_lattice(dict, kana);
    convert_from_lattice(&lattice, dict, conn, None, kana)
}

/// 1-best conversion with history-aware reranking.
pub fn convert_with_history(
    dict: &dyn Dictionary,
    conn: Option<&ConnectionMatrix>,
    history: &UserHistory,
    kana: &str,
) -> Vec<ConvertedSegment> {
    let lattice = build_lattice(dict, kana);
    convert_from_lattice(&lattice, dict, conn, Some(history), kana)
}

/// Convert a kana string to the N-best segmentations.
pub fn convert_nbest(
    dict: &dyn Dictionary,
    conn: Option<&ConnectionMatrix>,
    kana: &str,
    n: usize,
) -> Vec<Vec<ConvertedSegment>> {
    let lattice = build_lattice(dict, kana);
    convert_nbest_from_lattice(&lattice, dict, conn, None, kana, n)
}

/// N-best conversion with history-aware reranking.
pub fn convert_nbest_with_history(
    dict: &dyn Dictionary,
    conn: Option<&ConnectionMatrix>,
    history: &UserHistory,
    kana: &str,
    n: usize,
) -> Vec<Vec<ConvertedSegment>> {
    let lattice = build_lattice(dict, kana);
    convert_nbest_from_lattice(&lattice, dict, conn, Some(history), kana, n)
}

// ---------------------------------------------------------------------------
// Internal — constrained decoding (neural feature)
// ---------------------------------------------------------------------------

/// N-best Viterbi with a prefix constraint (for speculative decoding).
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
    reranker::rerank(&mut paths, conn, Some(dict));
    paths.truncate(n);
    paths
}
