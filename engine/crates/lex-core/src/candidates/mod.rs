//! Candidate generation for IME input.
//!
//! Provides standard and predictive strategies for generating conversion
//! candidates from a kana reading.

use std::collections::HashSet;

use crate::converter::{
    build_lattice, ConversionContext, ConvertedSegment, HistoryAt, Lattice, PathOrigin,
};

pub mod predictive;
pub mod standard;
pub mod widths;

#[cfg(test)]
mod now_tests;
#[cfg(test)]
mod tests;

/// Alternative forms for punctuation characters.
/// When the reading is a punctuation kana, we show the original + these alternatives.
static PUNCTUATION_ALTERNATIVES: &[(&str, &[&str])] = &[
    ("。", &["．", "."]),
    ("、", &["，", ","]),
    ("？", &["?"]),
    ("！", &["!"]),
    ("「", &["｢", "["]),
    ("」", &["｣", "]"]),
    ("・", &["／", "/"]),
    ("〜", &["~"]),
];

/// Result of unified candidate generation.
pub struct CandidateResponse {
    /// Candidate surfaces for display (ordered, deduplicated).
    pub surfaces: Vec<String>,
    /// N-best paths for segment-level learning.
    pub paths: Vec<Vec<ConvertedSegment>>,
}

/// A candidate list with what built it, both from one pipeline run. For
/// diagnostics that must describe the list as shipped: a second run to
/// recover them could see a different path population.
pub struct PricedCandidates {
    pub response: CandidateResponse,
    pub diagnostics: CandidateDiagnostics,
}

/// What the Standard-mode generator records about the list it built. For
/// punctuation input, whose paths are not N-best paths, only `top_source`
/// (`Punctuation`) is set.
#[derive(Debug, Default)]
pub struct CandidateDiagnostics {
    /// `prices[i]` is the price of `response.paths[i]`.
    pub prices: Vec<PathPrice>,
    /// Learned surfaces the history-injection stage added to the list (those
    /// no N-best path had already placed), in list order.
    pub injected: Vec<String>,
    /// Which stage put the list's #1 (`response.surfaces[0]`) there. `None`
    /// for an empty reading.
    pub top_source: Option<TopSource>,
}

/// Which stage of the Standard generator *placed* the list's #1 at index 0
/// (a promotion included). Who put the surface in the list or set its price
/// is `replay-commit-log`'s `Owner`; the two differ when a promoted kana is
/// also an N-best path — as intended: M9 is judged on the placing stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum TopSource {
    /// The `i`-th N-best path (postprocess order); `i = 0` unless the
    /// N-best's first paths were empty.
    Path(usize),
    /// The reading itself, placed at index 0 by the kana promotion (a learned
    /// kana over a #1 with no history of its own, `standard.rs`). Recorded
    /// whenever that branch decides index 0, even when the kana was already
    /// there (then `from == Some(0)`).
    KanaPromoted {
        /// Its position in the list before the promotion, if it was there.
        from: Option<usize>,
        kana_boost: i64,
    },
    /// A learned surface the injection stage added (N-best empty).
    Injected,
    /// The reading, appended with no learning (N-best empty).
    Kana,
    /// Punctuation input, which has no N-best.
    Punctuation,
}

/// The final price of an N-best path and the stage that set it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PathPrice {
    /// Viterbi + rerank − history. The list is in this order except that the
    /// cheapest learned path is first (SPEC §ブースト計算), so a later path
    /// can cost less than the first.
    pub cost: i64,
    pub priced_by: PathOrigin,
    /// Cost-gap admission keeps the path whatever its price: a surface the
    /// user committed for this reading, or the typed kana.
    pub cost_gap_exempt: bool,
}

/// Look up punctuation alternatives for a reading.
fn punctuation_alternatives(reading: &str) -> Option<&'static [&'static str]> {
    PUNCTUATION_ALTERNATIVES
        .iter()
        .find(|&&(k, _)| k == reading)
        .map(|&(_, v)| v)
}

/// Generate punctuation candidates: learned predictions first, then default + alternatives.
fn generate_punctuation_candidates(
    ctx: &ConversionContext<'_>,
    reading: &str,
    max_results: usize,
) -> CandidateResponse {
    let mut surfaces = Vec::new();
    let mut seen = HashSet::new();

    // Learned predictions first
    if let Some(HistoryAt { history: h, now }) = ctx.history {
        let fetch_limit = max_results.max(200);
        let mut ranked = ctx.dict.predict_ranked(reading, fetch_limit, 1000);
        ranked.sort_by(|(r_a, e_a), (r_b, e_b)| {
            let boost_a = h.unigram_boost(r_a, &e_a.surface, now);
            let boost_b = h.unigram_boost(r_b, &e_b.surface, now);
            boost_b.cmp(&boost_a).then(e_a.cost.cmp(&e_b.cost))
        });
        ranked.truncate(max_results);
        for (_, entry) in &ranked {
            if seen.insert(entry.surface.clone()) {
                surfaces.push(entry.surface.clone());
            }
        }
    }

    // Reading itself
    if seen.insert(reading.to_string()) {
        surfaces.push(reading.to_string());
    }

    // Alternatives
    if let Some(alts) = punctuation_alternatives(reading) {
        for &alt in alts {
            if seen.insert(alt.to_string()) {
                surfaces.push(alt.to_string());
            }
        }
    }

    CandidateResponse {
        surfaces,
        paths: Vec::new(),
    }
}

// --- Public API ---

/// Unified candidate generation: handles both punctuation and normal input.
///
/// Builds a lattice internally. Use `generate_candidates_from_lattice` when
/// a pre-built lattice is available (e.g. deferred candidate mode).
pub fn generate_candidates(
    ctx: &ConversionContext<'_>,
    reading: &str,
    max_results: usize,
) -> CandidateResponse {
    if reading.is_empty() || punctuation_alternatives(reading).is_some() {
        return standard::generate(ctx, reading, max_results, &Lattice::empty());
    }
    let lattice = build_lattice(ctx.dict, reading);
    standard::generate(ctx, reading, max_results, &lattice)
}

/// [`generate_candidates`] with the [`CandidateDiagnostics`] of the same run
/// (each N-best path's price and the stage that set it, the injected learned
/// surfaces). Diagnostic entry point; the IME uses [`generate_candidates`].
pub fn generate_candidates_priced(
    ctx: &ConversionContext<'_>,
    reading: &str,
    max_results: usize,
) -> PricedCandidates {
    if reading.is_empty() || punctuation_alternatives(reading).is_some() {
        return PricedCandidates {
            response: generate_candidates(ctx, reading, max_results),
            diagnostics: CandidateDiagnostics {
                top_source: (!reading.is_empty()).then_some(TopSource::Punctuation),
                ..CandidateDiagnostics::default()
            },
        };
    }
    let lattice = build_lattice(ctx.dict, reading);
    standard::generate_normal_priced(ctx, reading, max_results, &lattice)
}

/// [`generate_candidates_priced`] at the production list budget
/// (`candidates.max_results`, the limit the async worker uses): the one place
/// the tools ask for "the candidate list as the IME builds it".
pub fn production_candidates_priced(
    ctx: &ConversionContext<'_>,
    reading: &str,
) -> PricedCandidates {
    generate_candidates_priced(
        ctx,
        reading,
        crate::settings::settings().candidates.max_results,
    )
}

/// Unified candidate generation from a pre-built lattice.
///
/// Uses `lattice.input` as the reading to prevent mismatch.
pub fn generate_candidates_from_lattice(
    lattice: &Lattice,
    ctx: &ConversionContext<'_>,
    max_results: usize,
) -> CandidateResponse {
    standard::generate(ctx, &lattice.input, max_results, lattice)
}

/// Generate prediction candidates with bigram chaining.
pub fn generate_prediction_candidates(
    ctx: &ConversionContext<'_>,
    reading: &str,
    max_results: usize,
) -> CandidateResponse {
    if reading.is_empty() || punctuation_alternatives(reading).is_some() {
        return generate_candidates(ctx, reading, max_results);
    }
    let lattice = build_lattice(ctx.dict, reading);
    predictive::generate(ctx, reading, max_results, &lattice)
}

/// Generate prediction candidates from a pre-built lattice.
///
/// Uses `lattice.input` as the reading to prevent mismatch.
pub fn generate_prediction_candidates_from_lattice(
    lattice: &Lattice,
    ctx: &ConversionContext<'_>,
    max_results: usize,
) -> CandidateResponse {
    predictive::generate(ctx, &lattice.input, max_results, lattice)
}
