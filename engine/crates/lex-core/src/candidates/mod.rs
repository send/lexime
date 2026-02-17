//! Candidate generation for IME input.
//!
//! Provides pluggable strategies (standard, predictive, neural) for
//! generating conversion candidates from a kana reading.

use std::collections::HashSet;

use crate::converter::ConvertedSegment;
use crate::dict::Dictionary;
use crate::user_history::UserHistory;

pub mod predictive;
pub mod standard;
pub mod strategy;

#[cfg(feature = "neural")]
pub mod neural;

#[cfg(test)]
mod tests;

pub use strategy::CandidateStrategy;

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

// --- Backward-compatible public API wrappers ---

/// Unified candidate generation: handles both punctuation and normal input.
pub fn generate_candidates(
    dict: &dyn Dictionary,
    conn: Option<&crate::dict::connection::ConnectionMatrix>,
    history: Option<&UserHistory>,
    reading: &str,
    max_results: usize,
) -> CandidateResponse {
    standard::generate(dict, conn, history, reading, max_results)
}

/// Generate prediction candidates with bigram chaining.
pub fn generate_prediction_candidates(
    dict: &dyn Dictionary,
    conn: Option<&crate::dict::connection::ConnectionMatrix>,
    history: Option<&UserHistory>,
    reading: &str,
    max_results: usize,
) -> CandidateResponse {
    predictive::generate(dict, conn, history, reading, max_results)
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
