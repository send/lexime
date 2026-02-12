use crate::dict::connection::ConnectionMatrix;

use super::cost::conn_cost;
use super::viterbi::ScoredPath;

/// Weight for the segment-length variance penalty.
/// Penalises paths whose segments have very uneven reading lengths
/// (e.g. 1-char + 3-char) in favour of more uniform splits (2+2).
const LENGTH_VARIANCE_WEIGHT: i64 = 2000;

/// Rerank N-best Viterbi paths by applying post-hoc features.
///
/// The Viterbi core already handles dictionary cost + connection cost +
/// segment penalty + script cost. The reranker adds features that are better
/// evaluated on complete paths rather than locally during the forward pass:
///
/// - **Structure cost**: sum of transition costs along the path (Mozc-inspired);
///   paths with high accumulated transition costs tend to be fragmented
/// - **Length variance**: penalises uneven segment splits so that more uniform
///   segmentations are preferred when Viterbi costs are close
pub fn rerank(paths: &mut [ScoredPath], conn: Option<&ConnectionMatrix>) {
    if paths.len() <= 1 {
        return;
    }

    for path in paths.iter_mut() {
        // Structure cost: accumulated transition costs along the path.
        // High structure cost indicates many transitions through morpheme
        // boundaries — a sign of over-fragmentation. We add a fraction of
        // the structure cost as a penalty to prefer naturally connected paths.
        let mut structure_cost: i64 = 0;
        for i in 1..path.segments.len() {
            let prev = &path.segments[i - 1];
            let next = &path.segments[i];
            structure_cost += conn_cost(conn, prev.right_id, next.left_id);
        }
        // Add 25% of structure cost as penalty — enough to differentiate
        // fragmented paths without dominating the Viterbi cost.
        path.viterbi_cost += structure_cost / 4;

        // Length variance penalty: for paths with 2+ segments, penalise
        // uneven reading lengths. Computed as sum-of-squared-deviations
        // from the mean, scaled by LENGTH_VARIANCE_WEIGHT / N.
        let n = path.segments.len();
        if n >= 2 {
            let lengths: Vec<i64> = path
                .segments
                .iter()
                .map(|s| s.reading.chars().count() as i64)
                .collect();
            let sum: i64 = lengths.iter().sum();
            // sum_sq_dev = Σ (len_i - mean)² × N  (multiplied through to stay in integers)
            //            = N × Σ len_i² - (Σ len_i)²
            let sum_sq: i64 = lengths.iter().map(|l| l * l).sum();
            let n_i64 = n as i64;
            let sum_sq_dev = n_i64 * sum_sq - sum * sum;
            // Divide by N² to get the true variance-based penalty:
            // penalty = (sum_sq_dev / N) * WEIGHT / N = sum_sq_dev * WEIGHT / N²
            path.viterbi_cost += sum_sq_dev * LENGTH_VARIANCE_WEIGHT / (n_i64 * n_i64);
        }
    }

    paths.sort_by_key(|p| p.viterbi_cost);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::converter::viterbi::{RichSegment, ScoredPath};
    use crate::dict::connection::ConnectionMatrix;

    #[test]
    fn test_rerank_penalizes_fragmented_path() {
        // Build a connection matrix where transitions cost 100 each
        let num_ids = 3;
        let mut text = format!("{num_ids} {num_ids}\n");
        for _ in 0..(num_ids * num_ids) {
            text.push_str("100\n");
        }
        let conn = ConnectionMatrix::from_text(&text).unwrap();

        let mut paths = vec![
            // Fragmented path: 3 segments → 2 transitions × 100 = 200 structure cost
            // Penalty: 200 / 4 = 50
            ScoredPath {
                segments: vec![
                    RichSegment {
                        reading: "き".into(),
                        surface: "木".into(),
                        left_id: 1,
                        right_id: 1,
                    },
                    RichSegment {
                        reading: "の".into(),
                        surface: "の".into(),
                        left_id: 1,
                        right_id: 1,
                    },
                    RichSegment {
                        reading: "は".into(),
                        surface: "葉".into(),
                        left_id: 1,
                        right_id: 1,
                    },
                ],
                viterbi_cost: 1000,
            },
            // Single segment path: 0 transitions → 0 structure cost
            ScoredPath {
                segments: vec![RichSegment {
                    reading: "きのは".into(),
                    surface: "木の葉".into(),
                    left_id: 1,
                    right_id: 1,
                }],
                viterbi_cost: 1040,
            },
        ];

        rerank(&mut paths, Some(&conn));

        // Fragmented: 1000 + 50 = 1050 > Single: 1040 + 0 = 1040
        assert_eq!(paths[0].segments[0].surface, "木の葉");
    }

    #[test]
    fn test_rerank_no_conn_no_structure_penalty() {
        let mut paths = vec![
            ScoredPath {
                segments: vec![
                    RichSegment {
                        reading: "き".into(),
                        surface: "木".into(),
                        left_id: 1,
                        right_id: 1,
                    },
                    RichSegment {
                        reading: "の".into(),
                        surface: "の".into(),
                        left_id: 1,
                        right_id: 1,
                    },
                ],
                viterbi_cost: 1000,
            },
            ScoredPath {
                segments: vec![RichSegment {
                    reading: "きの".into(),
                    surface: "木の".into(),
                    left_id: 1,
                    right_id: 1,
                }],
                viterbi_cost: 2000,
            },
        ];

        // Without conn, structure cost is 0 → order preserved
        rerank(&mut paths, None);
        assert_eq!(paths[0].viterbi_cost, 1000);
    }

    #[test]
    fn test_rerank_single_path_noop() {
        let mut paths = vec![ScoredPath {
            segments: vec![RichSegment {
                reading: "あ".into(),
                surface: "亜".into(),
                left_id: 0,
                right_id: 0,
            }],
            viterbi_cost: 1000,
        }];

        rerank(&mut paths, None);
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].segments[0].surface, "亜");
    }

    #[test]
    fn test_rerank_empty_noop() {
        let mut paths: Vec<ScoredPath> = Vec::new();
        rerank(&mut paths, None);
        assert!(paths.is_empty());
    }

    #[test]
    fn test_rerank_penalizes_uneven_segments() {
        // Uneven split: で(1) | 来たり(3) — variance penalty should apply
        // Even split:   出来(2) | たり(2) — no variance penalty
        let mut paths = vec![
            // Uneven: readings 1 + 3 chars → mean=2, sum_sq_dev=2×(1+1)=4 (via formula: N*Σl²-S² = 2*10-16=4)
            // penalty = 4 * 2000 / 4 = 2000
            ScoredPath {
                segments: vec![
                    RichSegment {
                        reading: "で".into(),
                        surface: "で".into(),
                        left_id: 0,
                        right_id: 0,
                    },
                    RichSegment {
                        reading: "きたり".into(),
                        surface: "来たり".into(),
                        left_id: 0,
                        right_id: 0,
                    },
                ],
                viterbi_cost: 5000,
            },
            // Even: readings 2 + 2 chars → sum_sq_dev=0, penalty=0
            ScoredPath {
                segments: vec![
                    RichSegment {
                        reading: "でき".into(),
                        surface: "出来".into(),
                        left_id: 0,
                        right_id: 0,
                    },
                    RichSegment {
                        reading: "たり".into(),
                        surface: "たり".into(),
                        left_id: 0,
                        right_id: 0,
                    },
                ],
                viterbi_cost: 6500,
            },
        ];

        rerank(&mut paths, None);

        // Uneven: 5000 + 2000 = 7000
        // Even:   6500 + 0    = 6500
        // Even split should be ranked first
        assert_eq!(paths[0].segments[0].surface, "出来");
        assert_eq!(paths[0].viterbi_cost, 6500);
        assert_eq!(paths[1].segments[0].surface, "で");
        assert_eq!(paths[1].viterbi_cost, 7000);
    }
}
