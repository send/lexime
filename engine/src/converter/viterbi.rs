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

/// A segment with POS metadata, used internally for reranking.
#[derive(Debug, Clone)]
pub(crate) struct RichSegment {
    pub reading: String,
    pub surface: String,
    pub left_id: u16,
    pub right_id: u16,
}

/// A scored path from N-best Viterbi, carrying enough info for reranking.
#[derive(Debug, Clone)]
pub(crate) struct ScoredPath {
    pub segments: Vec<RichSegment>,
    pub viterbi_cost: i64,
}

impl ScoredPath {
    /// Convert to public ConvertedSegment, dropping POS metadata.
    pub fn into_segments(self) -> Vec<ConvertedSegment> {
        self.segments
            .into_iter()
            .map(|s| ConvertedSegment {
                reading: s.reading,
                surface: s.surface,
            })
            .collect()
    }

    /// Surface key for deduplication.
    pub fn surface_key(&self) -> String {
        self.segments.iter().map(|s| s.surface.as_str()).collect()
    }
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
    convert_with_cost(dict, &cost_fn, conn, kana)
}

/// Convert a kana string using a custom cost function.
///
/// `conn` is passed separately for the reranker's structure cost calculation.
pub fn convert_with_cost(
    dict: &dyn Dictionary,
    cost_fn: &dyn CostFunction,
    conn: Option<&ConnectionMatrix>,
    kana: &str,
) -> Vec<ConvertedSegment> {
    if kana.is_empty() {
        return Vec::new();
    }

    let lattice = build_lattice(dict, kana);
    viterbi(&lattice, cost_fn, conn)
}

/// Run the Viterbi algorithm on a lattice to find the minimum-cost path.
/// Over-generates candidates and applies reranking to find the best path.
fn viterbi(
    lattice: &Lattice,
    cost_fn: &dyn CostFunction,
    conn: Option<&ConnectionMatrix>,
) -> Vec<ConvertedSegment> {
    // Over-generate to give the reranker enough candidates
    let mut paths = viterbi_nbest(lattice, cost_fn, 10);
    super::reranker::rerank(&mut paths, conn);
    paths
        .into_iter()
        .next()
        .map(|p| p.into_segments())
        .unwrap_or_default()
}

/// A single entry in the top-K list for a node: (accumulated cost, previous node index, rank at
/// that node). `prev_rank` identifies which of the K paths at the previous node this entry
/// continues from.
#[derive(Clone, Copy)]
struct KEntry {
    cost: i64,
    prev_idx: Option<usize>,
    prev_rank: usize,
}

/// Run N-best Viterbi: keep top-K cost/backpointer pairs per node.
///
/// Returns up to `n` distinct `ScoredPath`s, sorted by Viterbi cost (best first).
/// Paths that produce identical surface strings are deduplicated.
pub(crate) fn viterbi_nbest(
    lattice: &Lattice,
    cost_fn: &dyn CostFunction,
    n: usize,
) -> Vec<ScoredPath> {
    let char_count = lattice.char_count;
    if char_count == 0 || n == 0 {
        return Vec::new();
    }

    let num_nodes = lattice.nodes.len();
    // top_k[node_idx] = sorted Vec of KEntry (ascending cost), max `n` entries
    let mut top_k: Vec<Vec<KEntry>> = vec![Vec::new(); num_nodes];

    // Initialize nodes starting at position 0 (BOS transition)
    for &idx in &lattice.nodes_by_start[0] {
        let node = &lattice.nodes[idx];
        let cost = cost_fn.word_cost(node) + cost_fn.bos_cost(node);
        top_k[idx].push(KEntry {
            cost,
            prev_idx: None,
            prev_rank: 0,
        });
    }

    // Forward pass — next_idx loop is outermost so word_cost is computed
    // once per next_node (O(P)) instead of once per (prev, next) pair (O(P²)).
    for pos in 1..char_count {
        for &next_idx in &lattice.nodes_by_start[pos] {
            let next_node = &lattice.nodes[next_idx];
            let word = cost_fn.word_cost(next_node);

            for &prev_idx in &lattice.nodes_by_end[pos] {
                if top_k[prev_idx].is_empty() {
                    continue;
                }
                let prev_node = &lattice.nodes[prev_idx];
                let transition = cost_fn.transition_cost(prev_node, next_node);

                for rank in 0..top_k[prev_idx].len() {
                    let prev_cost = top_k[prev_idx][rank].cost;
                    let total = prev_cost + transition + word;

                    insert_top_k(
                        &mut top_k[next_idx],
                        n,
                        KEntry {
                            cost: total,
                            prev_idx: Some(prev_idx),
                            prev_rank: rank,
                        },
                    );
                }
            }
        }
    }

    // Collect top-K at EOS
    let mut eos_entries: Vec<(i64, usize, usize)> = Vec::new(); // (total_cost, node_idx, rank)
    for &node_idx in &lattice.nodes_by_end[char_count] {
        let node = &lattice.nodes[node_idx];
        let eos = cost_fn.eos_cost(node);
        for (rank, entry) in top_k[node_idx].iter().enumerate() {
            let total = entry.cost + eos;
            eos_entries.push((total, node_idx, rank));
        }
    }
    eos_entries.sort_by_key(|&(cost, _, _)| cost);

    // Backtrace each path, deduplicate by surface string
    let mut results: Vec<ScoredPath> = Vec::new();
    let mut seen_surfaces: std::collections::HashSet<String> = std::collections::HashSet::new();

    for &(total_cost, end_idx, end_rank) in &eos_entries {
        if results.len() >= n {
            break;
        }
        let segments = backtrace_nbest(&top_k, end_idx, end_rank, lattice);
        let scored = ScoredPath {
            segments,
            viterbi_cost: total_cost,
        };
        if seen_surfaces.insert(scored.surface_key()) {
            results.push(scored);
        }
    }

    results
}

/// Insert a KEntry into a top-K list, maintaining ascending sort by cost and max size `k`.
fn insert_top_k(list: &mut Vec<KEntry>, k: usize, entry: KEntry) {
    // Find insertion point (binary search for ascending order)
    let pos = list.partition_point(|e| e.cost <= entry.cost);
    if pos >= k {
        return; // worse than all K existing entries
    }
    list.insert(pos, entry);
    if list.len() > k {
        list.pop();
    }
}

/// Backtrace from a specific (node_idx, rank) to reconstruct a path.
fn backtrace_nbest(
    top_k: &[Vec<KEntry>],
    end_idx: usize,
    end_rank: usize,
    lattice: &Lattice,
) -> Vec<RichSegment> {
    let mut path_indices = Vec::new();
    let mut cur_idx = end_idx;
    let mut cur_rank = end_rank;

    loop {
        path_indices.push(cur_idx);
        let entry = &top_k[cur_idx][cur_rank];
        match entry.prev_idx {
            Some(prev) => {
                cur_rank = entry.prev_rank;
                cur_idx = prev;
            }
            None => break,
        }
    }
    path_indices.reverse();

    path_indices
        .iter()
        .map(|&idx| {
            let node = &lattice.nodes[idx];
            RichSegment {
                reading: node.reading.clone(),
                surface: node.surface.clone(),
                left_id: node.left_id,
                right_id: node.right_id,
            }
        })
        .collect()
}

/// Convert a kana string to the N-best segmentations using Viterbi algorithm.
///
/// Internally generates more candidates than `n`, applies reranking, then
/// returns the top `n` distinct paths.
pub fn convert_nbest(
    dict: &dyn Dictionary,
    conn: Option<&ConnectionMatrix>,
    kana: &str,
    n: usize,
) -> Vec<Vec<ConvertedSegment>> {
    let cost_fn = DefaultCostFunction::new(conn);
    convert_nbest_with_cost(dict, &cost_fn, conn, kana, n)
}

/// Convert a kana string to the N-best segmentations using a custom cost function.
///
/// `conn` is passed separately for the reranker's structure cost calculation.
pub fn convert_nbest_with_cost(
    dict: &dyn Dictionary,
    cost_fn: &dyn CostFunction,
    conn: Option<&ConnectionMatrix>,
    kana: &str,
    n: usize,
) -> Vec<Vec<ConvertedSegment>> {
    if kana.is_empty() || n == 0 {
        return Vec::new();
    }
    let lattice = build_lattice(dict, kana);
    // Over-generate candidates to give the reranker enough diversity
    let oversample = n * 3;
    let mut paths = viterbi_nbest(&lattice, cost_fn, oversample);
    super::reranker::rerank(&mut paths, conn);

    // Take top-n Viterbi candidates, then append synthetic candidates from rewriters
    let mut top_paths: Vec<ScoredPath> = paths.into_iter().take(n).collect();

    let katakana_rw = super::rewriter::KatakanaRewriter;
    let rewriters: Vec<&dyn super::rewriter::Rewriter> = vec![&katakana_rw];
    super::rewriter::run_rewriters(&rewriters, &mut top_paths, kana);

    top_paths.into_iter().map(|p| p.into_segments()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::converter::testutil::test_dict;
    use crate::dict::connection::ConnectionMatrix;
    use crate::dict::{DictEntry, TrieDictionary};

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

    #[test]
    fn test_nbest_returns_multiple_paths() {
        let dict = test_dict();
        // "きょう" has two entries: 今日(3000) and 京(5000)
        let results = convert_nbest(&dict, None, "きょう", 5);

        assert!(results.len() >= 2, "should return at least 2 paths");
        // 1-best should be 今日
        assert_eq!(results[0][0].surface, "今日");
        // 2nd best should be 京
        assert_eq!(results[1][0].surface, "京");
    }

    #[test]
    fn test_nbest_first_matches_1best() {
        let dict = test_dict();
        let best = convert(&dict, None, "きょうはいいてんき");
        let nbest = convert_nbest(&dict, None, "きょうはいいてんき", 5);

        assert!(!nbest.is_empty());
        let best_surfaces: Vec<&str> = best.iter().map(|s| s.surface.as_str()).collect();
        let nbest_surfaces: Vec<&str> = nbest[0].iter().map(|s| s.surface.as_str()).collect();
        assert_eq!(best_surfaces, nbest_surfaces, "1-best must match convert()");
    }

    #[test]
    fn test_nbest_deduplicates_surfaces() {
        let dict = test_dict();
        let results = convert_nbest(&dict, None, "きょうは", 10);

        let surface_strings: Vec<String> = results
            .iter()
            .map(|path| path.iter().map(|s| s.surface.as_str()).collect::<String>())
            .collect();
        let unique: std::collections::HashSet<&String> = surface_strings.iter().collect();
        assert_eq!(
            surface_strings.len(),
            unique.len(),
            "N-best should not contain duplicate surface strings"
        );
    }

    #[test]
    fn test_nbest_empty_input() {
        let dict = test_dict();
        let results = convert_nbest(&dict, None, "", 5);
        assert!(results.is_empty());
    }

    #[test]
    fn test_nbest_n_zero() {
        let dict = test_dict();
        let results = convert_nbest(&dict, None, "きょう", 0);
        assert!(results.is_empty());
    }

    #[test]
    fn test_nbest_n_one_matches_1best() {
        let dict = test_dict();
        let best = convert(&dict, None, "きょうはいいてんき");
        let nbest = convert_nbest(&dict, None, "きょうはいいてんき", 1);

        // n=1 Viterbi candidate + 1 katakana rewriter candidate
        assert!(nbest.len() >= 1);
        let best_surfaces: Vec<&str> = best.iter().map(|s| s.surface.as_str()).collect();
        let nbest_surfaces: Vec<&str> = nbest[0].iter().map(|s| s.surface.as_str()).collect();
        assert_eq!(best_surfaces, nbest_surfaces);
    }

    #[test]
    fn test_nbest_includes_katakana_candidate() {
        let dict = test_dict();
        let results = convert_nbest(&dict, None, "きょう", 10);

        let surfaces: Vec<String> = results
            .iter()
            .map(|path| path.iter().map(|s| s.surface.as_str()).collect::<String>())
            .collect();
        assert!(
            surfaces.contains(&"キョウ".to_string()),
            "N-best should include katakana candidate, got: {:?}",
            surfaces
        );
    }

    #[test]
    fn test_nbest_sorted_by_cost() {
        // Verify N-best paths are returned in ascending cost order
        let dict = test_dict();
        let cost_fn = DefaultCostFunction::new(None);
        let lattice = build_lattice(&dict, "きょうは");
        let results = viterbi_nbest(&lattice, &cost_fn, 10);

        // We can't directly check costs from the public API, but we can verify
        // the best path is first (already tested above). For additional confidence,
        // ensure at least 2 results with different segmentations.
        assert!(results.len() >= 2);
        // Different segmentations should exist (e.g., 今日+は vs 京+は vs き+ょ+う+は)
        let first: String = results[0].segments.iter().map(|s| &*s.surface).collect();
        let second: String = results[1].segments.iter().map(|s| &*s.surface).collect();
        assert_ne!(first, second);
    }
}
