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
use viterbi::{viterbi_nbest, ScoredPath};
pub use viterbi::{ConvertedSegment, PathOrigin};

/// A user history together with the moment its decay is measured at.
///
/// The read path never reads the wall clock: the entry point (IME session,
/// candidate worker, `lextool`, `dictool`) reads it once and pairs it with the
/// history here. A `now` exists only beside a history, so a conversion without
/// one has no timestamp to pass or to get wrong.
#[derive(Clone, Copy)]
pub struct HistoryAt<'a> {
    pub history: &'a UserHistory,
    pub now: u64,
}

/// Shared conversion resources: dictionary, connection matrix, and user
/// history (with the `now` it decays against).
///
/// Groups what appears across conversion and candidate generation APIs; every
/// read-path entry point takes one.
#[derive(Clone, Copy)]
pub struct ConversionContext<'a> {
    pub dict: &'a dyn Dictionary,
    pub conn: Option<&'a ConnectionMatrix>,
    pub history: Option<HistoryAt<'a>>,
}

impl<'a> ConversionContext<'a> {
    /// A context without history.
    pub fn new(dict: &'a dyn Dictionary, conn: Option<&'a ConnectionMatrix>) -> Self {
        Self {
            dict,
            conn,
            history: None,
        }
    }

    /// The same context with `history` (decayed at `now`) when there is one.
    pub fn with_history(self, history: Option<&'a UserHistory>, now: u64) -> Self {
        Self {
            history: history.map(|history| HistoryAt { history, now }),
            ..self
        }
    }

    /// Build a lattice from a kana string.
    pub fn build_lattice(&self, kana: &str) -> Lattice {
        build_lattice(self.dict, kana)
    }

    /// 1-best conversion of a kana string (builds its lattice).
    pub fn convert(&self, kana: &str) -> Vec<ConvertedSegment> {
        if kana.is_empty() {
            return Vec::new();
        }
        self.convert_from_lattice(&self.build_lattice(kana))
    }

    /// N-best conversion of a kana string (builds its lattice).
    pub fn convert_nbest(&self, kana: &str, n: usize) -> Vec<Vec<ConvertedSegment>> {
        if kana.is_empty() || n == 0 {
            return Vec::new();
        }
        self.convert_nbest_from_lattice(&self.build_lattice(kana), n)
    }

    /// 1-best conversion from a pre-built lattice: the N-best path with
    /// `n = 1`, so the 1-best and `convert_nbest(.., 1)[0]` are one code path
    /// and one population (`population(1, h)`).
    pub fn convert_from_lattice(&self, lattice: &Lattice) -> Vec<ConvertedSegment> {
        self.convert_nbest_scored_from_lattice(lattice, 1)
            .into_iter()
            .next()
            .map(ScoredPath::into_segments)
            .unwrap_or_default()
    }

    /// N-best conversion from a pre-built lattice.
    pub fn convert_nbest_from_lattice(
        &self,
        lattice: &Lattice,
        n: usize,
    ) -> Vec<Vec<ConvertedSegment>> {
        self.convert_nbest_scored_from_lattice(lattice, n)
            .into_iter()
            .map(ScoredPath::into_segments)
            .collect()
    }

    /// N-best paths with their final cost (after rerank, history and the
    /// rewriters), for callers that need the price the list was ordered by
    /// (the cheapest learned path first, then by price).
    pub(crate) fn convert_nbest_scored_from_lattice(
        &self,
        lattice: &Lattice,
        n: usize,
    ) -> Vec<ScoredPath> {
        self.convert_lattice_impl(lattice, n, population(n, self.history.is_some()))
    }

    /// Viterbi at `oversample` paths + postprocess for `n`. The N-best
    /// wrappers call it with `population(n, h)` (the 1-best is `n = 1`);
    /// tests call it with a chosen population.
    fn convert_lattice_impl(
        &self,
        lattice: &Lattice,
        n: usize,
        oversample: usize,
    ) -> Vec<ScoredPath> {
        if lattice.input.is_empty() || n == 0 {
            return Vec::new();
        }
        let cost_fn = DefaultCostFunction::new(self.conn);
        let mut paths = viterbi_nbest(lattice, &cost_fn, oversample);
        postprocess(&mut paths, lattice, self, n)
    }
}

/// Per-keystroke budget: the Viterbi paths the 1-best (`n = 1`) works on
/// without a history record to learn from. The 1-best runs synchronously on
/// every keystroke, so its population is kept small (drawing the candidate
/// list's 60 here made it about 2.2x slower, #361).
pub(crate) const KEYSTROKE_POPULATION: usize = 10;
/// Per-keystroke budget with a history: gives the reranker and the history
/// boost paths to work on. A learned path past 30 reaches the candidate list's
/// #1 but not the 1-best's (#361).
pub(crate) const KEYSTROKE_POPULATION_WITH_HISTORY: usize = 30;
/// Candidate-list budget with a history, for `n >= 2` (it may also run
/// synchronously on a key thread, so it is a budget too).
pub(crate) const LIST_POPULATION_WITH_HISTORY: usize = 50;

/// Viterbi paths collected for an N-best of `n`: the population rerank,
/// history and the rewriters work on, `max(3n, floor(n, h))`. `n = 1` is the
/// 1-best and takes the per-keystroke floor. Every reader (the 1-best, the
/// candidate list, explain) uses this one rule.
pub(crate) fn population(n: usize, has_history: bool) -> usize {
    let floor = match (has_history, n) {
        (false, _) => KEYSTROKE_POPULATION,
        (true, 1) => KEYSTROKE_POPULATION_WITH_HISTORY,
        (true, _) => LIST_POPULATION_WITH_HISTORY,
    };
    (n * 3).max(floor)
}

/// The surface a conversion path spells.
pub fn joined_surface(segments: &[ConvertedSegment]) -> String {
    segments.iter().map(|s| s.surface.as_str()).collect()
}

// ---------------------------------------------------------------------------
// Convenience wrappers without history
// ---------------------------------------------------------------------------

/// Convert a kana string to the best segmentation using Viterbi algorithm.
pub fn convert(
    dict: &dyn Dictionary,
    conn: Option<&ConnectionMatrix>,
    kana: &str,
) -> Vec<ConvertedSegment> {
    ConversionContext::new(dict, conn).convert(kana)
}

/// Convert a kana string to the N-best segmentations.
pub fn convert_nbest(
    dict: &dyn Dictionary,
    conn: Option<&ConnectionMatrix>,
    kana: &str,
    n: usize,
) -> Vec<Vec<ConvertedSegment>> {
    ConversionContext::new(dict, conn).convert_nbest(kana, n)
}
