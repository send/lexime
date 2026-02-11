use crate::dict::connection::ConnectionMatrix;

use super::cost::conn_cost;
use super::viterbi::ScoredPath;

/// Rerank N-best Viterbi paths by applying post-hoc features.
///
/// The Viterbi core already handles dictionary cost + connection cost +
/// segment penalty + script cost. The reranker adds features that are better
/// evaluated on complete paths rather than locally during the forward pass:
///
/// - **Structure cost**: sum of transition costs along the path (Mozc-inspired);
///   paths with high accumulated transition costs tend to be fragmented
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
}
