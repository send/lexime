use crate::dict::connection::ConnectionMatrix;
use crate::dict::Dictionary;

use super::cost::{CostFunction, DefaultCostFunction};
use super::lattice::{build_lattice, Lattice};

/// A segment in the conversion result.
#[derive(Debug, Clone)]
pub struct ConvertedSegment {
    /// The kana reading of this segment
    pub reading: String,
    /// The converted surface form (kanji, etc.)
    pub surface: String,
}

/// Convert a kana string to the best segmentation using Viterbi algorithm.
///
/// If `conn` is provided, uses connection costs for scoring transitions.
/// Otherwise, falls back to unigram-only scoring (sum of word costs).
pub fn convert(
    dict: &dyn Dictionary,
    conn: Option<&ConnectionMatrix>,
    kana: &str,
) -> Vec<ConvertedSegment> {
    let cost_fn = DefaultCostFunction::new(conn);
    convert_with_cost(dict, &cost_fn, kana)
}

/// Convert a kana string using a custom cost function.
pub fn convert_with_cost(
    dict: &dyn Dictionary,
    cost_fn: &dyn CostFunction,
    kana: &str,
) -> Vec<ConvertedSegment> {
    if kana.is_empty() {
        return Vec::new();
    }

    let lattice = build_lattice(dict, kana);
    viterbi(&lattice, cost_fn)
}

/// Run the Viterbi algorithm on a lattice to find the minimum-cost path.
fn viterbi(lattice: &Lattice, cost_fn: &dyn CostFunction) -> Vec<ConvertedSegment> {
    let n = lattice.char_count;
    if n == 0 {
        return Vec::new();
    }

    // best_cost[node_idx] = minimum total cost to reach this node
    // backpointer[node_idx] = previous node index on the best path (None for start nodes)
    let num_nodes = lattice.nodes.len();
    let mut best_cost: Vec<i64> = vec![i64::MAX; num_nodes];
    let mut backpointer: Vec<Option<usize>> = vec![None; num_nodes];

    // Initialize nodes starting at position 0 (BOS transition)
    for &idx in &lattice.nodes_by_start[0] {
        let node = &lattice.nodes[idx];
        best_cost[idx] = cost_fn.word_cost(node) + cost_fn.bos_cost(node);
        backpointer[idx] = None;
    }

    // Forward pass: for each position, update costs of nodes starting there
    for pos in 1..n {
        // For each node ending at `pos`
        for &prev_idx in &lattice.nodes_by_end[pos] {
            if best_cost[prev_idx] == i64::MAX {
                continue; // unreachable node
            }
            let prev_node = &lattice.nodes[prev_idx];

            // For each node starting at `pos` (O(E) via index)
            for &next_idx in &lattice.nodes_by_start[pos] {
                let next_node = &lattice.nodes[next_idx];

                let total = best_cost[prev_idx]
                    + cost_fn.transition_cost(prev_node, next_node)
                    + cost_fn.word_cost(next_node);

                if total < best_cost[next_idx] {
                    best_cost[next_idx] = total;
                    backpointer[next_idx] = Some(prev_idx);
                }
            }
        }
    }

    // Find the best node ending at position n (EOS)
    let mut best_end_idx: Option<usize> = None;
    let mut best_end_cost = i64::MAX;

    for &node_idx in &lattice.nodes_by_end[n] {
        if best_cost[node_idx] == i64::MAX {
            continue;
        }
        let node = &lattice.nodes[node_idx];
        let total = best_cost[node_idx] + cost_fn.eos_cost(node);

        if total < best_end_cost {
            best_end_cost = total;
            best_end_idx = Some(node_idx);
        }
    }

    // Backtrace
    let mut path = Vec::new();
    let mut current = best_end_idx;
    while let Some(idx) = current {
        path.push(idx);
        current = backpointer[idx];
    }
    path.reverse();

    path.iter()
        .map(|&idx| {
            let node = &lattice.nodes[idx];
            ConvertedSegment {
                reading: node.reading.clone(),
                surface: node.surface.clone(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dict::connection::ConnectionMatrix;
    use crate::dict::{DictEntry, TrieDictionary};

    fn test_dict() -> TrieDictionary {
        let entries = vec![
            (
                "きょう".to_string(),
                vec![
                    DictEntry {
                        surface: "今日".to_string(),
                        cost: 3000,
                        left_id: 100,
                        right_id: 100,
                    },
                    DictEntry {
                        surface: "京".to_string(),
                        cost: 5000,
                        left_id: 101,
                        right_id: 101,
                    },
                ],
            ),
            (
                "は".to_string(),
                vec![DictEntry {
                    surface: "は".to_string(),
                    cost: 2000,
                    left_id: 200,
                    right_id: 200,
                }],
            ),
            (
                "いい".to_string(),
                vec![DictEntry {
                    surface: "良い".to_string(),
                    cost: 3500,
                    left_id: 300,
                    right_id: 300,
                }],
            ),
            (
                "てんき".to_string(),
                vec![DictEntry {
                    surface: "天気".to_string(),
                    cost: 4000,
                    left_id: 400,
                    right_id: 400,
                }],
            ),
            (
                "き".to_string(),
                vec![DictEntry {
                    surface: "木".to_string(),
                    cost: 4500,
                    left_id: 500,
                    right_id: 500,
                }],
            ),
            (
                "い".to_string(),
                vec![DictEntry {
                    surface: "胃".to_string(),
                    cost: 6000,
                    left_id: 600,
                    right_id: 600,
                }],
            ),
            (
                "てん".to_string(),
                vec![DictEntry {
                    surface: "天".to_string(),
                    cost: 5000,
                    left_id: 700,
                    right_id: 700,
                }],
            ),
            (
                "です".to_string(),
                vec![DictEntry {
                    surface: "です".to_string(),
                    cost: 2500,
                    left_id: 800,
                    right_id: 800,
                }],
            ),
            (
                "ね".to_string(),
                vec![DictEntry {
                    surface: "ね".to_string(),
                    cost: 2000,
                    left_id: 900,
                    right_id: 900,
                }],
            ),
            (
                "わたし".to_string(),
                vec![DictEntry {
                    surface: "私".to_string(),
                    cost: 3000,
                    left_id: 1000,
                    right_id: 1000,
                }],
            ),
            (
                "がくせい".to_string(),
                vec![DictEntry {
                    surface: "学生".to_string(),
                    cost: 4000,
                    left_id: 1100,
                    right_id: 1100,
                }],
            ),
        ];
        TrieDictionary::from_entries(entries)
    }

    #[test]
    fn test_convert_unigram() {
        let dict = test_dict();
        let result = convert(&dict, None, "きょうはいいてんき");

        let surfaces: Vec<&str> = result.iter().map(|s| s.surface.as_str()).collect();
        // Without connection costs, should pick lowest-cost words
        // Expected: 今日(3000) + は(2000) + 良い(3500) + 天気(4000) = 12500
        assert_eq!(surfaces, vec!["今日", "は", "良い", "天気"]);
    }

    #[test]
    fn test_convert_empty() {
        let dict = test_dict();
        let result = convert(&dict, None, "");
        assert!(result.is_empty());
    }

    #[test]
    fn test_convert_single_word() {
        let dict = test_dict();
        let result = convert(&dict, None, "きょう");

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].surface, "今日");
        assert_eq!(result[0].reading, "きょう");
    }

    #[test]
    fn test_convert_unknown_chars() {
        let dict = test_dict();
        let result = convert(&dict, None, "ぬ");

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].surface, "ぬ");
    }

    #[test]
    fn test_convert_watashi() {
        let dict = test_dict();
        let result = convert(&dict, None, "わたしはがくせいです");

        let surfaces: Vec<&str> = result.iter().map(|s| s.surface.as_str()).collect();
        assert_eq!(surfaces, vec!["私", "は", "学生", "です"]);
    }

    #[test]
    fn test_convert_with_connection_costs() {
        // Build a dict where "きょう" has two entries with similar word costs:
        //   "今日" (cost=5000, id=10) and "京" (cost=4900, id=20)
        // Without connection costs, "京" wins (lower cost).
        // With connection costs that penalize id=20→id=30 ("は") heavily,
        // "今日" should win instead.
        let entries = vec![
            (
                "きょう".to_string(),
                vec![
                    DictEntry {
                        surface: "今日".to_string(),
                        cost: 5000,
                        left_id: 10,
                        right_id: 10,
                    },
                    DictEntry {
                        surface: "京".to_string(),
                        cost: 4900,
                        left_id: 20,
                        right_id: 20,
                    },
                ],
            ),
            (
                "は".to_string(),
                vec![DictEntry {
                    surface: "は".to_string(),
                    cost: 2000,
                    left_id: 30,
                    right_id: 30,
                }],
            ),
        ];
        let dict = TrieDictionary::from_entries(entries);

        // Without connection costs, "京" (4900) wins over "今日" (5000)
        let result_unigram = convert(&dict, None, "きょうは");
        assert_eq!(result_unigram[0].surface, "京");

        // Build a connection matrix (31×31 to cover ids 0..30)
        // Set high penalty for id 20→30 transition, low for id 10→30
        let num_ids = 31;
        let mut text = format!("{num_ids} {num_ids}\n");
        for left in 0..num_ids {
            for right in 0..num_ids {
                let cost = if left == 20 && right == 30 {
                    500 // heavy penalty: 京→は
                } else {
                    0
                };
                text.push_str(&format!("{cost}\n"));
            }
        }
        let conn = ConnectionMatrix::from_text(&text).unwrap();

        // With connection costs: "京"(4900) + conn(20→30=500) = 5400
        //                        "今日"(5000) + conn(10→30=0) = 5000
        // "今日" should now win
        let result_bigram = convert(&dict, Some(&conn), "きょうは");
        assert_eq!(result_bigram[0].surface, "今日");
        assert_eq!(result_bigram[1].surface, "は");
    }

    #[test]
    fn test_viterbi_tiebreak_deterministic() {
        let entries = vec![(
            "あ".to_string(),
            vec![
                DictEntry {
                    surface: "亜".to_string(),
                    cost: 5000,
                    left_id: 0,
                    right_id: 0,
                },
                DictEntry {
                    surface: "阿".to_string(),
                    cost: 5000,
                    left_id: 0,
                    right_id: 0,
                },
            ],
        )];
        let dict = TrieDictionary::from_entries(entries);
        let first = convert(&dict, None, "あ");
        assert_eq!(first.len(), 1);
        for _ in 0..10 {
            let result = convert(&dict, None, "あ");
            assert_eq!(
                result[0].surface, first[0].surface,
                "Viterbi tie-breaking must be deterministic"
            );
        }
    }
}
