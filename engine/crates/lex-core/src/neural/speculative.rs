//! Speculative decoding: Viterbi draft + GPT-2 verify.
//!
//! Uses Viterbi 1-best as a draft, then GPT-2 scores each segment.
//! Low-confidence segments trigger constrained Viterbi re-exploration
//! with the confirmed prefix fixed. Converges in 2-3 iterations.

use std::time::{Duration, Instant};

use crate::converter::constrained::PrefixConstraint;
use crate::converter::{build_lattice, convert_nbest_constrained, ConvertedSegment, ScoredPath};
use crate::dict::connection::ConnectionMatrix;
use crate::dict::Dictionary;

use super::NeuralScorer;

/// Configuration for speculative decoding.
pub struct SpeculativeConfig {
    /// Per-char log-prob threshold below which a segment is considered low confidence.
    pub threshold: f64,
    /// Maximum number of verify-refine iterations.
    pub max_iterations: usize,
    /// Minimum number of segments to trigger speculative decoding.
    /// Shorter inputs fall back to N-best reranking.
    pub min_segments: usize,
    /// Number of N-best candidates per constrained Viterbi pass.
    pub nbest_per_pass: usize,
}

impl Default for SpeculativeConfig {
    fn default() -> Self {
        Self {
            threshold: -2.0,
            max_iterations: 3,
            min_segments: 3,
            nbest_per_pass: 5,
        }
    }
}

/// Result of speculative decoding.
pub struct SpeculativeResult {
    pub segments: Vec<ConvertedSegment>,
    pub segment_scores: Vec<f64>,
    pub metadata: SpeculativeMetadata,
}

/// Metadata about the speculative decoding process.
pub struct SpeculativeMetadata {
    /// Number of verify-refine iterations performed.
    pub iterations: usize,
    /// Number of confirmed segments after each iteration.
    pub confirmed_counts: Vec<usize>,
    /// Whether the result converged (all segments above threshold).
    pub converged: bool,
    /// Whether the input fell back to N-best reranking (too few segments).
    pub fell_back: bool,
    /// Total wall-clock time.
    pub total_latency: Duration,
    /// Time spent in Viterbi search.
    pub viterbi_latency: Duration,
    /// Time spent in neural scoring.
    pub neural_latency: Duration,
}

/// Run speculative decoding: draft with Viterbi, verify with GPT-2, refine.
pub fn speculative_decode(
    scorer: &mut NeuralScorer,
    dict: &dyn Dictionary,
    conn: Option<&ConnectionMatrix>,
    context: &str,
    kana: &str,
    config: &SpeculativeConfig,
) -> anyhow::Result<SpeculativeResult> {
    let total_start = Instant::now();
    let mut viterbi_latency = Duration::ZERO;
    let mut neural_latency = Duration::ZERO;

    // 1. Initial Viterbi N-best
    let viterbi_start = Instant::now();
    let cost_fn = crate::converter::cost::DefaultCostFunction::new(conn);
    let lattice = build_lattice(dict, kana);
    let mut initial_paths =
        crate::converter::viterbi_nbest(&lattice, &cost_fn, config.nbest_per_pass * 3);
    crate::converter::reranker::rerank(&mut initial_paths, conn);
    initial_paths.truncate(config.nbest_per_pass);
    viterbi_latency += viterbi_start.elapsed();

    if initial_paths.is_empty() {
        return Ok(SpeculativeResult {
            segments: Vec::new(),
            segment_scores: Vec::new(),
            metadata: SpeculativeMetadata {
                iterations: 0,
                confirmed_counts: Vec::new(),
                converged: true,
                fell_back: false,
                total_latency: total_start.elapsed(),
                viterbi_latency,
                neural_latency,
            },
        });
    }

    // 2. Short-input fallback: if draft has < min_segments, use N-best reranking
    let draft_segments = scored_path_to_segments(&initial_paths[0]);
    if draft_segments.len() < config.min_segments {
        let neural_start = Instant::now();
        let paths_as_segments: Vec<Vec<ConvertedSegment>> =
            initial_paths.iter().map(scored_path_to_segments).collect();
        let scores = scorer.score_paths(context, kana, &paths_as_segments)?;
        neural_latency += neural_start.elapsed();

        let best_idx = scores.first().map(|&(idx, _)| idx).unwrap_or(0);
        let best_segments = paths_as_segments[best_idx].clone();

        let neural_start2 = Instant::now();
        let segment_scores = scorer.score_segments(context, kana, &best_segments)?;
        neural_latency += neural_start2.elapsed();

        return Ok(SpeculativeResult {
            segments: best_segments,
            segment_scores,
            metadata: SpeculativeMetadata {
                iterations: 0,
                confirmed_counts: Vec::new(),
                converged: true,
                fell_back: true,
                total_latency: total_start.elapsed(),
                viterbi_latency,
                neural_latency,
            },
        });
    }

    // 3. Speculative decode loop
    let mut current = draft_segments;
    let mut confirmed_counts = Vec::new();
    let mut converged = false;

    for _iter in 0..config.max_iterations {
        // Score current segments
        let neural_start = Instant::now();
        let scores = scorer.score_segments(context, kana, &current)?;
        neural_latency += neural_start.elapsed();

        // Find the first low-confidence segment
        let first_low = scores.iter().position(|&s| s < config.threshold);

        match first_low {
            None => {
                // All segments above threshold → converged
                converged = true;
                confirmed_counts.push(current.len());
                break;
            }
            Some(low_idx) => {
                // Confirm segments before the low-confidence one
                let confirmed_prefix: Vec<ConvertedSegment> = current[..low_idx].to_vec();
                confirmed_counts.push(low_idx);

                // Build constraint and re-search
                let constraint = PrefixConstraint::from_confirmed(&confirmed_prefix);
                let viterbi_start = Instant::now();
                let new_paths =
                    convert_nbest_constrained(dict, conn, kana, &constraint, config.nbest_per_pass);
                viterbi_latency += viterbi_start.elapsed();

                if new_paths.is_empty() {
                    // No valid paths found, keep current
                    converged = false;
                    break;
                }

                // Score all new candidates with neural model
                let paths_as_segments: Vec<Vec<ConvertedSegment>> =
                    new_paths.iter().map(scored_path_to_segments).collect();
                let neural_start = Instant::now();
                let path_scores = scorer.score_paths(context, kana, &paths_as_segments)?;
                neural_latency += neural_start.elapsed();

                let best_idx = path_scores.first().map(|&(idx, _)| idx).unwrap_or(0);
                let new_best = paths_as_segments[best_idx].clone();

                // Check convergence: same surface as before?
                let prev_surface: String = current.iter().map(|s| s.surface.as_str()).collect();
                let new_surface: String = new_best.iter().map(|s| s.surface.as_str()).collect();

                current = new_best;
                if prev_surface == new_surface {
                    converged = true;
                    break;
                }
            }
        }
    }

    // Final segment scores
    let neural_start = Instant::now();
    let segment_scores = scorer.score_segments(context, kana, &current)?;
    neural_latency += neural_start.elapsed();

    Ok(SpeculativeResult {
        segments: current,
        segment_scores,
        metadata: SpeculativeMetadata {
            iterations: confirmed_counts.len(),
            confirmed_counts,
            converged,
            fell_back: false,
            total_latency: total_start.elapsed(),
            viterbi_latency,
            neural_latency,
        },
    })
}

/// Convert a ScoredPath to Vec<ConvertedSegment>.
fn scored_path_to_segments(path: &ScoredPath) -> Vec<ConvertedSegment> {
    path.segments
        .iter()
        .map(|s| ConvertedSegment {
            reading: s.reading.clone(),
            surface: s.surface.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_speculative_config_defaults() {
        let config = SpeculativeConfig::default();
        assert!((config.threshold - (-2.0)).abs() < f64::EPSILON);
        assert_eq!(config.max_iterations, 3);
        assert_eq!(config.min_segments, 3);
        assert_eq!(config.nbest_per_pass, 5);
    }

    #[test]
    fn test_scored_path_to_segments() {
        use crate::converter::RichSegment;

        let path = ScoredPath {
            segments: vec![
                RichSegment {
                    reading: "きょう".into(),
                    surface: "今日".into(),
                    left_id: 100,
                    right_id: 100,
                    word_cost: 3000,
                },
                RichSegment {
                    reading: "は".into(),
                    surface: "は".into(),
                    left_id: 200,
                    right_id: 200,
                    word_cost: 2000,
                },
            ],
            viterbi_cost: 5000,
        };

        let segments = scored_path_to_segments(&path);
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].reading, "きょう");
        assert_eq!(segments[0].surface, "今日");
        assert_eq!(segments[1].reading, "は");
        assert_eq!(segments[1].surface, "は");
    }
}
