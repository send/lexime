use std::collections::HashSet;

use tracing::{debug, debug_span};

use crate::dict::connection::ConnectionMatrix;
use crate::dict::Dictionary;
use crate::neural::NeuralScorer;
use crate::user_history::UserHistory;

use super::{generate_punctuation_candidates, punctuation_alternatives, CandidateResponse};

/// Generate candidates using neural speculative decoding (GhostText mode).
///
/// Uses speculative decode (Viterbi draft + GPT-2 verify) as the #1 candidate,
/// followed by standard Viterbi N-best candidates. Falls back to standard
/// candidate generation on neural failure.
pub fn generate(
    scorer: &mut NeuralScorer,
    dict: &dyn Dictionary,
    conn: Option<&ConnectionMatrix>,
    history: Option<&UserHistory>,
    context: &str,
    reading: &str,
    max_results: usize,
) -> CandidateResponse {
    let _span = debug_span!("generate_neural_candidates", reading, max_results).entered();

    if reading.is_empty() {
        return CandidateResponse {
            surfaces: Vec::new(),
            paths: Vec::new(),
        };
    }

    // Punctuation → standard punctuation candidates
    if punctuation_alternatives(reading).is_some() {
        return generate_punctuation_candidates(dict, history, reading, max_results);
    }

    // Try speculative decoding
    use crate::neural::speculative::{speculative_decode, SpeculativeConfig};

    let config = SpeculativeConfig::default();
    match speculative_decode(scorer, dict, conn, context, reading, &config) {
        Ok(result) => {
            let spec_surface: String = result.segments.iter().map(|s| s.surface.as_str()).collect();
            let spec_segments = result.segments;

            // Get standard candidates as the base
            let base = super::standard::generate_normal_candidates(
                dict,
                conn,
                history,
                reading,
                max_results,
            );

            let mut surfaces = Vec::new();
            let mut seen = HashSet::new();
            let mut paths = Vec::new();

            // Speculative result as #1
            if !spec_surface.is_empty() && seen.insert(spec_surface.clone()) {
                surfaces.push(spec_surface);
                paths.push(spec_segments);
            }

            // Then add base candidates
            for (i, s) in base.surfaces.iter().enumerate() {
                if seen.insert(s.clone()) {
                    surfaces.push(s.clone());
                }
                // Include base paths
                if i < base.paths.len() {
                    paths.push(base.paths[i].clone());
                }
            }

            surfaces.truncate(max_results);

            debug!(
                surface_count = surfaces.len(),
                path_count = paths.len(),
                "neural candidates"
            );
            CandidateResponse { surfaces, paths }
        }
        Err(e) => {
            debug!("speculative decode failed, falling back to standard: {e}");
            super::standard::generate_normal_candidates(dict, conn, history, reading, max_results)
        }
    }
}
