//! Candidate generation for IME input.
//!
//! Provides standard and predictive strategies for generating conversion
//! candidates from a kana reading. Neural scoring is feature-gated for
//! research use only.

use std::collections::HashSet;

use crate::converter::{build_lattice, ConvertedSegment, Lattice, PathOrigin};
use crate::dict::Dictionary;
use crate::user_history::UserHistory;

pub mod predictive;
pub mod standard;

#[cfg(feature = "neural")]
pub mod neural;

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

/// What the Standard-mode generator records about the list it built. Empty
/// for punctuation input, whose paths are not N-best paths.
#[derive(Debug, Default)]
pub struct CandidateDiagnostics {
    /// `prices[i]` is the price of `response.paths[i]`.
    pub prices: Vec<PathPrice>,
    /// Learned surfaces the history-injection stage added to the list (those
    /// no N-best path had already placed), in list order.
    pub injected: Vec<String>,
}

/// The final price of an N-best path and the stage that set it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PathPrice {
    /// Viterbi + rerank − history, as the list was ordered.
    pub cost: i64,
    pub priced_by: PathOrigin,
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
    dict: &dyn Dictionary,
    history: Option<&UserHistory>,
    reading: &str,
    max_results: usize,
) -> CandidateResponse {
    let mut surfaces = Vec::new();
    let mut seen = HashSet::new();

    // Learned predictions first
    if let Some(h) = history {
        let now = crate::user_history::now_epoch();
        let fetch_limit = max_results.max(200);
        let mut ranked = dict.predict_ranked(reading, fetch_limit, 1000);
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
    dict: &dyn Dictionary,
    conn: Option<&crate::dict::connection::ConnectionMatrix>,
    history: Option<&UserHistory>,
    reading: &str,
    max_results: usize,
) -> CandidateResponse {
    if reading.is_empty() || punctuation_alternatives(reading).is_some() {
        return standard::generate(dict, conn, history, reading, max_results, &Lattice::empty());
    }
    let lattice = build_lattice(dict, reading);
    standard::generate(dict, conn, history, reading, max_results, &lattice)
}

/// [`generate_candidates`] with each N-best path's final cost from the same
/// run. Diagnostic entry point; the IME uses [`generate_candidates`].
pub fn generate_candidates_priced(
    dict: &dyn Dictionary,
    conn: Option<&crate::dict::connection::ConnectionMatrix>,
    history: Option<&UserHistory>,
    reading: &str,
    max_results: usize,
) -> PricedCandidates {
    if reading.is_empty() || punctuation_alternatives(reading).is_some() {
        return PricedCandidates {
            response: generate_candidates(dict, conn, history, reading, max_results),
            diagnostics: CandidateDiagnostics::default(),
        };
    }
    let lattice = build_lattice(dict, reading);
    standard::generate_normal_priced(dict, conn, history, reading, max_results, &lattice)
}

/// Unified candidate generation from a pre-built lattice.
///
/// Uses `lattice.input` as the reading to prevent mismatch.
pub fn generate_candidates_from_lattice(
    lattice: &Lattice,
    dict: &dyn Dictionary,
    conn: Option<&crate::dict::connection::ConnectionMatrix>,
    history: Option<&UserHistory>,
    max_results: usize,
) -> CandidateResponse {
    standard::generate(dict, conn, history, &lattice.input, max_results, lattice)
}

/// Generate prediction candidates with bigram chaining.
pub fn generate_prediction_candidates(
    dict: &dyn Dictionary,
    conn: Option<&crate::dict::connection::ConnectionMatrix>,
    history: Option<&UserHistory>,
    reading: &str,
    max_results: usize,
) -> CandidateResponse {
    if reading.is_empty() || punctuation_alternatives(reading).is_some() {
        return generate_candidates(dict, conn, history, reading, max_results);
    }
    let lattice = build_lattice(dict, reading);
    predictive::generate(dict, conn, history, reading, max_results, &lattice)
}

/// Generate prediction candidates from a pre-built lattice.
///
/// Uses `lattice.input` as the reading to prevent mismatch.
pub fn generate_prediction_candidates_from_lattice(
    lattice: &Lattice,
    dict: &dyn Dictionary,
    conn: Option<&crate::dict::connection::ConnectionMatrix>,
    history: Option<&UserHistory>,
    max_results: usize,
) -> CandidateResponse {
    predictive::generate(dict, conn, history, &lattice.input, max_results, lattice)
}

/// Generate candidates using neural speculative decoding.
#[cfg(feature = "neural")]
pub fn generate_neural_candidates(
    scorer: &mut crate::neural::NeuralScorer,
    dict: &dyn Dictionary,
    conn: Option<&crate::dict::connection::ConnectionMatrix>,
    history: Option<&UserHistory>,
    context: &str,
    reading: &str,
    max_results: usize,
) -> CandidateResponse {
    neural::generate(scorer, dict, conn, history, context, reading, max_results)
}
