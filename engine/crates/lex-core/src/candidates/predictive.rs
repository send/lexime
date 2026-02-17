use std::collections::HashSet;

use tracing::{debug, debug_span};

use crate::dict::connection::ConnectionMatrix;
use crate::dict::Dictionary;
use crate::user_history::UserHistory;

use super::{generate_punctuation_candidates, punctuation_alternatives, CandidateResponse};

/// Chain bigram successors from a starting surface to build multi-word phrases.
/// Returns the chained phrase (start_surface + successors) or None if no successors found.
/// Detects cycles (repeated surfaces) and stops chaining to avoid garbage output.
fn chain_bigram_phrase(
    history: &UserHistory,
    start_surface: &str,
    max_chain: usize,
) -> Option<String> {
    let mut result = start_surface.to_string();
    let mut current_surface = start_surface.to_string();
    let mut visited = HashSet::new();
    visited.insert(current_surface.clone());
    let mut extended = false;

    for _ in 0..max_chain {
        let successors = history.bigram_successors(&current_surface);
        if let Some((_, next_surface, _)) = successors.first() {
            if !visited.insert(next_surface.clone()) {
                break; // cycle detected
            }
            result.push_str(next_surface);
            current_surface.clone_from(next_surface);
            extended = true;
        } else {
            break;
        }
    }

    if extended {
        Some(result)
    } else {
        None
    }
}

/// Generate prediction candidates with bigram chaining (Copilot-like completions).
/// Uses Viterbi N-best as the base, then chains bigram successors from history
/// to produce progressively longer multi-word phrases.
pub fn generate(
    dict: &dyn Dictionary,
    conn: Option<&ConnectionMatrix>,
    history: Option<&UserHistory>,
    reading: &str,
    max_results: usize,
) -> CandidateResponse {
    let _span = debug_span!("generate_prediction_candidates", reading, max_results).entered();
    if reading.is_empty() {
        return CandidateResponse {
            surfaces: Vec::new(),
            paths: Vec::new(),
        };
    }

    // Punctuation falls back to standard punctuation candidates
    if punctuation_alternatives(reading).is_some() {
        return generate_punctuation_candidates(dict, history, reading, max_results);
    }

    // Get base candidates (same Viterbi N-best + predictions as Standard)
    let base =
        super::standard::generate_normal_candidates(dict, conn, history, reading, max_results);

    let Some(h) = history else {
        // No history → can't chain, return standard candidates
        debug!(surface_count = base.surfaces.len(), "no history, fallback");
        return base;
    };

    let mut surfaces = Vec::new();
    let mut seen = HashSet::new();
    let max_chain = 5;

    // Build chained phrases from N-best paths (use last segment surface for chaining)
    let mut chained_phrases: Vec<(String, usize)> = Vec::new(); // (phrase, length)
    let mut chained_starts: HashSet<String> = HashSet::new(); // track chain start surfaces

    for path in &base.paths {
        if path.is_empty() {
            continue;
        }
        let last_surface = &path.last().unwrap().surface;
        let joined: String = path.iter().map(|s| s.surface.as_str()).collect();

        chained_starts.insert(joined.clone());
        if let Some(chained) = chain_bigram_phrase(h, last_surface, max_chain) {
            let full = format!("{}{}", joined, &chained[last_surface.len()..]);
            let chain_len = full.chars().count();
            if full != joined {
                chained_phrases.push((full, chain_len));
            }
        }
    }

    // Also try chaining from base candidate surfaces not already covered by paths
    for surface in &base.surfaces {
        if chained_starts.contains(surface) {
            continue;
        }
        if let Some(chained) = chain_bigram_phrase(h, surface, max_chain) {
            let chain_len = chained.chars().count();
            chained_phrases.push((chained, chain_len));
        }
    }

    // Sort chained phrases by length descending (longest first = most Copilot-like)
    chained_phrases.sort_by(|a, b| b.1.cmp(&a.1));

    // Add chained phrases first (longest completions)
    for (phrase, _) in &chained_phrases {
        if seen.insert(phrase.clone()) {
            surfaces.push(phrase.clone());
        }
    }

    // Then add base candidates
    for s in &base.surfaces {
        if seen.insert(s.clone()) {
            surfaces.push(s.clone());
        }
    }

    surfaces.truncate(max_results);

    debug!(
        surface_count = surfaces.len(),
        chained_count = chained_phrases.len(),
    );
    CandidateResponse {
        surfaces,
        paths: base.paths,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_history::UserHistory;

    #[test]
    fn test_chain_bigram_phrase_basic() {
        let mut h = UserHistory::new();
        h.record(&[
            ("きょう".into(), "今日".into()),
            ("は".into(), "は".into()),
            ("いい".into(), "良い".into()),
        ]);
        let result = chain_bigram_phrase(&h, "今日", 5);
        assert_eq!(result.as_deref(), Some("今日は良い"));
    }

    #[test]
    fn test_chain_bigram_phrase_no_successors() {
        let h = UserHistory::new();
        assert!(chain_bigram_phrase(&h, "今日", 5).is_none());
    }

    #[test]
    fn test_chain_bigram_phrase_cycle_detection() {
        let mut h = UserHistory::new();
        // Create a cycle: A→B→A
        h.record(&[("あ".into(), "A".into()), ("び".into(), "B".into())]);
        h.record(&[("び".into(), "B".into()), ("あ".into(), "A".into())]);

        let result = chain_bigram_phrase(&h, "A", 10);
        // Should chain A→B then stop (A already visited)
        assert_eq!(result.as_deref(), Some("AB"));
    }

    #[test]
    fn test_chain_bigram_phrase_self_loop() {
        let mut h = UserHistory::new();
        // Self-loop: は→は
        h.record(&[("は".into(), "は".into()), ("は".into(), "は".into())]);

        let result = chain_bigram_phrase(&h, "は", 10);
        // Should not chain at all (first successor is "は" which is already visited)
        assert!(result.is_none());
    }
}
