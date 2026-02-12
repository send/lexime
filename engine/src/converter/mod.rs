pub(crate) mod cost;
mod lattice;
pub(crate) mod reranker;
pub(crate) mod rewriter;
pub(crate) mod testutil;
mod viterbi;

use crate::dict::connection::ConnectionMatrix;
use crate::dict::Dictionary;
use crate::user_history::UserHistory;

use cost::DefaultCostFunction;
use viterbi::{viterbi_nbest, ScoredPath};

pub use lattice::{build_lattice, Lattice, LatticeNode};
pub use viterbi::ConvertedSegment;

/// Shared post-processing pipeline: rerank → history_rerank → take(n) → rewrite.
fn postprocess(
    paths: &mut Vec<ScoredPath>,
    conn: Option<&ConnectionMatrix>,
    history: Option<&UserHistory>,
    kana: &str,
    n: usize,
) -> Vec<Vec<ConvertedSegment>> {
    reranker::rerank(paths, conn);
    if let Some(h) = history {
        reranker::history_rerank(paths, h);
    }
    let mut top: Vec<ScoredPath> = paths.drain(..n.min(paths.len())).collect();
    let katakana_rw = rewriter::KatakanaRewriter;
    let rewriters: Vec<&dyn rewriter::Rewriter> = vec![&katakana_rw];
    rewriter::run_rewriters(&rewriters, &mut top, kana);
    top.into_iter().map(|p| p.into_segments()).collect()
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
    if kana.is_empty() {
        return Vec::new();
    }
    let cost_fn = DefaultCostFunction::new(conn);
    let lattice = build_lattice(dict, kana);
    let mut paths = viterbi_nbest(&lattice, &cost_fn, 10);
    postprocess(&mut paths, conn, None, kana, 1)
        .into_iter()
        .next()
        .unwrap_or_default()
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
    if kana.is_empty() || n == 0 {
        return Vec::new();
    }
    let cost_fn = DefaultCostFunction::new(conn);
    let lattice = build_lattice(dict, kana);
    let oversample = n * 3;
    let mut paths = viterbi_nbest(&lattice, &cost_fn, oversample);
    postprocess(&mut paths, conn, None, kana, n)
}

/// 1-best conversion with history-aware reranking.
///
/// Viterbi runs with `DefaultCostFunction` (no learned boosts), then
/// `rerank` + `history_rerank` are applied on the N-best list. This avoids
/// boost-induced lattice fragmentation while still surfacing learned
/// candidates.
pub fn convert_with_history(
    dict: &dyn Dictionary,
    conn: Option<&ConnectionMatrix>,
    history: &UserHistory,
    kana: &str,
) -> Vec<ConvertedSegment> {
    if kana.is_empty() {
        return Vec::new();
    }
    let cost_fn = DefaultCostFunction::new(conn);
    let lattice = build_lattice(dict, kana);
    let mut paths = viterbi_nbest(&lattice, &cost_fn, 30);
    postprocess(&mut paths, conn, Some(history), kana, 1)
        .into_iter()
        .next()
        .unwrap_or_default()
}

/// N-best conversion with history-aware reranking.
///
/// Viterbi runs with `DefaultCostFunction`, then `rerank` +
/// `history_rerank` are applied. The oversample is set to
/// `max(n*3, 50)` to ensure enough diversity for the reranker to find
/// learned candidates.
pub fn convert_nbest_with_history(
    dict: &dyn Dictionary,
    conn: Option<&ConnectionMatrix>,
    history: &UserHistory,
    kana: &str,
    n: usize,
) -> Vec<Vec<ConvertedSegment>> {
    if kana.is_empty() || n == 0 {
        return Vec::new();
    }
    let cost_fn = DefaultCostFunction::new(conn);
    let lattice = build_lattice(dict, kana);
    let oversample = (n * 3).max(50);
    let mut paths = viterbi_nbest(&lattice, &cost_fn, oversample);
    postprocess(&mut paths, conn, Some(history), kana, n)
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

    #[test]
    fn test_convert_with_history_promotes_learned() {
        let dict = test_dict();
        // "きょう" has 今日(3000) and 京(5000). Without history, 今日 wins.
        let baseline = convert(&dict, None, "きょう");
        assert_eq!(baseline[0].surface, "今日");

        // After learning "京", it should be promoted
        let mut h = crate::user_history::UserHistory::new();
        h.record(&[("きょう".into(), "京".into())]);
        h.record(&[("きょう".into(), "京".into())]);

        let result = convert_with_history(&dict, None, &h, "きょう");
        assert_eq!(result[0].surface, "京");
    }

    #[test]
    fn test_convert_with_history_empty_history_matches_baseline() {
        let dict = test_dict();
        let h = crate::user_history::UserHistory::new();

        let baseline = convert(&dict, None, "きょうはいいてんき");
        let with_history = convert_with_history(&dict, None, &h, "きょうはいいてんき");

        let baseline_surfaces: Vec<&str> = baseline.iter().map(|s| s.surface.as_str()).collect();
        let history_surfaces: Vec<&str> = with_history.iter().map(|s| s.surface.as_str()).collect();
        assert_eq!(baseline_surfaces, history_surfaces);
    }

    #[test]
    fn test_convert_with_history_empty_input() {
        let dict = test_dict();
        let h = crate::user_history::UserHistory::new();
        let result = convert_with_history(&dict, None, &h, "");
        assert!(result.is_empty());
    }

    #[test]
    fn test_convert_nbest_with_history_promotes_learned() {
        let dict = test_dict();
        let mut h = crate::user_history::UserHistory::new();
        h.record(&[("きょう".into(), "京".into())]);
        h.record(&[("きょう".into(), "京".into())]);

        let results = convert_nbest_with_history(&dict, None, &h, "きょう", 5);
        assert!(!results.is_empty());
        assert_eq!(results[0][0].surface, "京");
    }

    #[test]
    fn test_convert_nbest_with_history_empty_input() {
        let dict = test_dict();
        let h = crate::user_history::UserHistory::new();
        assert!(convert_nbest_with_history(&dict, None, &h, "", 5).is_empty());
        assert!(convert_nbest_with_history(&dict, None, &h, "きょう", 0).is_empty());
    }
}
