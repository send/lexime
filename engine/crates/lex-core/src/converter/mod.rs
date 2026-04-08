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

pub use lattice::{build_lattice, Lattice};
pub use viterbi::ConvertedSegment;
#[allow(unused_imports)]
pub(crate) use viterbi::{viterbi_nbest, RichSegment, ScoredPath};

/// Shared conversion resources: dictionary, connection matrix, and user history.
///
/// Groups the `(dict, conn, history)` triple that appears across conversion
/// and candidate generation APIs.
pub struct ConversionContext<'a> {
    pub dict: &'a dyn Dictionary,
    pub conn: Option<&'a ConnectionMatrix>,
    pub history: Option<&'a UserHistory>,
}

impl ConversionContext<'_> {
    /// Build a lattice from a kana string.
    pub fn build_lattice(&self, kana: &str) -> Lattice {
        build_lattice(self.dict, kana)
    }

    /// 1-best conversion from a pre-built lattice.
    pub fn convert_from_lattice(&self, lattice: &Lattice) -> Vec<ConvertedSegment> {
        if lattice.input.is_empty() {
            return Vec::new();
        }
        let cost_fn = DefaultCostFunction::new(self.conn);
        let oversample = if self.history.is_some() { 30 } else { 10 };
        let mut paths = viterbi_nbest(lattice, &cost_fn, oversample);
        postprocess(
            &mut paths,
            lattice,
            self.conn,
            Some(self.dict),
            self.history,
            &lattice.input,
            1,
        )
        .into_iter()
        .next()
        .unwrap_or_default()
    }

    /// N-best conversion from a pre-built lattice.
    pub fn convert_nbest_from_lattice(
        &self,
        lattice: &Lattice,
        n: usize,
    ) -> Vec<Vec<ConvertedSegment>> {
        if lattice.input.is_empty() || n == 0 {
            return Vec::new();
        }
        let cost_fn = DefaultCostFunction::new(self.conn);
        let oversample = if self.history.is_some() {
            (n * 3).max(50)
        } else {
            n * 3
        };
        let mut paths = viterbi_nbest(lattice, &cost_fn, oversample);
        postprocess(
            &mut paths,
            lattice,
            self.conn,
            Some(self.dict),
            self.history,
            &lattice.input,
            n,
        )
    }
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
    if kana.is_empty() {
        return Vec::new();
    }
    let ctx = ConversionContext {
        dict,
        conn,
        history: None,
    };
    let lattice = ctx.build_lattice(kana);
    ctx.convert_from_lattice(&lattice)
}

/// 1-best conversion with history-aware reranking.
pub fn convert_with_history(
    dict: &dyn Dictionary,
    conn: Option<&ConnectionMatrix>,
    history: &UserHistory,
    kana: &str,
) -> Vec<ConvertedSegment> {
    if kana.is_empty() {
        return Vec::new();
    }
    let ctx = ConversionContext {
        dict,
        conn,
        history: Some(history),
    };
    let lattice = ctx.build_lattice(kana);
    ctx.convert_from_lattice(&lattice)
}

/// Convert a kana string to the N-best segmentations.
pub fn convert_nbest(
    dict: &dyn Dictionary,
    conn: Option<&ConnectionMatrix>,
    kana: &str,
    n: usize,
) -> Vec<Vec<ConvertedSegment>> {
    if kana.is_empty() || n == 0 {
        return Vec::new();
    }
    let ctx = ConversionContext {
        dict,
        conn,
        history: None,
    };
    let lattice = ctx.build_lattice(kana);
    ctx.convert_nbest_from_lattice(&lattice, n)
}

/// N-best conversion with history-aware reranking.
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
    let ctx = ConversionContext {
        dict,
        conn,
        history: Some(history),
    };
    let lattice = ctx.build_lattice(kana);
    ctx.convert_nbest_from_lattice(&lattice, n)
}

// ---------------------------------------------------------------------------
// Internal — constrained decoding (neural feature)
// ---------------------------------------------------------------------------

/// N-best Viterbi with a prefix constraint (for speculative decoding).
#[cfg(feature = "neural")]
pub(crate) fn convert_nbest_constrained(
    ctx: &ConversionContext<'_>,
    kana: &str,
    constraint: &constrained::PrefixConstraint,
    n: usize,
) -> Vec<ScoredPath> {
    if kana.is_empty() || n == 0 {
        return Vec::new();
    }
    let cost_fn = constrained::PrefixConstrainedCost::new(ctx.conn, constraint);
    let lattice = build_lattice(ctx.dict, kana);
    let oversample = n * 3;
    let mut paths = viterbi_nbest(&lattice, &cost_fn, oversample);
    reranker::rerank(&mut paths, ctx.conn, Some(ctx.dict));
    paths.truncate(n);
    paths
}
