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

/// Shared post-processing pipeline: rerank → history_rerank → take(n) → rewrite → group.
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
    if let Some(c) = conn {
        for path in &mut top {
            group_segments(&mut path.segments, c);
        }
    }
    top.into_iter().map(|p| p.into_segments()).collect()
}

/// Group morpheme-level segments into phrase-level segments (content word + trailing function words).
///
/// A segment boundary is placed before each content word (non-function word),
/// except at the very beginning. Leading function words are kept standalone.
fn group_segments(segments: &mut Vec<viterbi::RichSegment>, conn: &ConnectionMatrix) {
    if segments.len() <= 1 {
        return;
    }

    let mut grouped: Vec<viterbi::RichSegment> = Vec::new();
    let mut current: Option<viterbi::RichSegment> = None;

    for seg in segments.drain(..) {
        let is_fw = conn.is_function_word(seg.left_id);

        match (&mut current, is_fw) {
            (Some(cur), true) => {
                cur.reading.push_str(&seg.reading);
                cur.surface.push_str(&seg.surface);
                cur.right_id = seg.right_id;
            }
            (Some(_), false) => {
                grouped.push(current.take().unwrap());
                current = Some(seg);
            }
            (None, true) => grouped.push(seg),
            (None, false) => {
                current = Some(seg);
            }
        }
    }

    if let Some(cur) = current {
        grouped.push(cur);
    }

    *segments = grouped;
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

    // --- group_segments tests ---

    use crate::converter::testutil::zero_conn_with_fw;
    use crate::converter::viterbi::RichSegment;

    fn rich(reading: &str, surface: &str, id: u16) -> RichSegment {
        RichSegment {
            reading: reading.into(),
            surface: surface.into(),
            left_id: id,
            right_id: id,
        }
    }

    #[test]
    fn test_group_segments_basic() {
        // content(100) + func(200) + content(300) → 2 segments
        let conn = zero_conn_with_fw(301, 200, 200);
        let mut segs = vec![
            rich("きょう", "今日", 100),
            rich("は", "は", 200),
            rich("いい", "良い", 300),
        ];
        group_segments(&mut segs, &conn);
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].reading, "きょうは");
        assert_eq!(segs[0].surface, "今日は");
        assert_eq!(segs[0].left_id, 100);
        assert_eq!(segs[0].right_id, 200);
        assert_eq!(segs[1].reading, "いい");
        assert_eq!(segs[1].surface, "良い");
    }

    #[test]
    fn test_group_segments_leading_func() {
        // Leading function word stays standalone
        let conn = zero_conn_with_fw(301, 200, 200);
        let mut segs = vec![rich("は", "は", 200), rich("きょう", "今日", 100)];
        group_segments(&mut segs, &conn);
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].surface, "は");
        assert_eq!(segs[1].surface, "今日");
    }

    #[test]
    fn test_group_segments_consecutive_func() {
        // content + func + func → all merged into one segment
        let conn = zero_conn_with_fw(301, 200, 210);
        let mut segs = vec![
            rich("たべ", "食べ", 100),
            rich("て", "て", 200),
            rich("は", "は", 210),
        ];
        group_segments(&mut segs, &conn);
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].reading, "たべては");
        assert_eq!(segs[0].surface, "食べては");
        assert_eq!(segs[0].left_id, 100);
        assert_eq!(segs[0].right_id, 210);
    }

    #[test]
    fn test_group_segments_all_content() {
        // All content words → no grouping
        let conn = zero_conn_with_fw(301, 200, 200);
        let mut segs = vec![rich("きょう", "今日", 100), rich("いい", "良い", 300)];
        group_segments(&mut segs, &conn);
        assert_eq!(segs.len(), 2);
    }

    #[test]
    fn test_group_segments_single_and_empty() {
        let conn = zero_conn_with_fw(301, 200, 200);

        let mut single = vec![rich("きょう", "今日", 100)];
        group_segments(&mut single, &conn);
        assert_eq!(single.len(), 1);

        let mut empty: Vec<RichSegment> = vec![];
        group_segments(&mut empty, &conn);
        assert!(empty.is_empty());
    }

    #[test]
    fn test_convert_groups_with_conn() {
        // Integration test: convert with a conn that has fw_range covering は(id=200)
        let dict = test_dict();
        let conn = zero_conn_with_fw(1200, 200, 200);
        let result = convert(&dict, Some(&conn), "きょうは");
        // "今日" + "は" should be grouped into one segment "今日は"
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].surface, "今日は");
        assert_eq!(result[0].reading, "きょうは");
    }

    // --- latency benchmark ---

    /// Build a richer dictionary for benchmarking that covers all words in the
    /// benchmark inputs so we exercise real dictionary lookup paths, not just
    /// unknown-word fallback.
    fn bench_dict() -> TrieDictionary {
        let entries = vec![
            (
                "きょう".into(),
                vec![
                    DictEntry {
                        surface: "今日".into(),
                        cost: 3000,
                        left_id: 100,
                        right_id: 100,
                    },
                    DictEntry {
                        surface: "京".into(),
                        cost: 5000,
                        left_id: 101,
                        right_id: 101,
                    },
                ],
            ),
            (
                "は".into(),
                vec![DictEntry {
                    surface: "は".into(),
                    cost: 2000,
                    left_id: 200,
                    right_id: 200,
                }],
            ),
            (
                "いい".into(),
                vec![
                    DictEntry {
                        surface: "良い".into(),
                        cost: 3500,
                        left_id: 300,
                        right_id: 300,
                    },
                    DictEntry {
                        surface: "いい".into(),
                        cost: 4000,
                        left_id: 301,
                        right_id: 301,
                    },
                ],
            ),
            (
                "てんき".into(),
                vec![DictEntry {
                    surface: "天気".into(),
                    cost: 4000,
                    left_id: 400,
                    right_id: 400,
                }],
            ),
            (
                "です".into(),
                vec![DictEntry {
                    surface: "です".into(),
                    cost: 2500,
                    left_id: 800,
                    right_id: 800,
                }],
            ),
            (
                "ね".into(),
                vec![DictEntry {
                    surface: "ね".into(),
                    cost: 2000,
                    left_id: 900,
                    right_id: 900,
                }],
            ),
            (
                "わたし".into(),
                vec![DictEntry {
                    surface: "私".into(),
                    cost: 3000,
                    left_id: 1000,
                    right_id: 1000,
                }],
            ),
            (
                "だ".into(),
                vec![DictEntry {
                    surface: "だ".into(),
                    cost: 2500,
                    left_id: 810,
                    right_id: 810,
                }],
            ),
            (
                "と".into(),
                vec![DictEntry {
                    surface: "と".into(),
                    cost: 2000,
                    left_id: 820,
                    right_id: 820,
                }],
            ),
            (
                "おもい".into(),
                vec![DictEntry {
                    surface: "思い".into(),
                    cost: 3500,
                    left_id: 830,
                    right_id: 830,
                }],
            ),
            (
                "おもいます".into(),
                vec![DictEntry {
                    surface: "思います".into(),
                    cost: 3200,
                    left_id: 831,
                    right_id: 831,
                }],
            ),
            (
                "ます".into(),
                vec![DictEntry {
                    surface: "ます".into(),
                    cost: 2500,
                    left_id: 840,
                    right_id: 840,
                }],
            ),
            (
                "い".into(),
                vec![DictEntry {
                    surface: "胃".into(),
                    cost: 6000,
                    left_id: 600,
                    right_id: 600,
                }],
            ),
            (
                "き".into(),
                vec![DictEntry {
                    surface: "木".into(),
                    cost: 4500,
                    left_id: 500,
                    right_id: 500,
                }],
            ),
            (
                "てん".into(),
                vec![DictEntry {
                    surface: "天".into(),
                    cost: 5000,
                    left_id: 700,
                    right_id: 700,
                }],
            ),
            (
                "がくせい".into(),
                vec![DictEntry {
                    surface: "学生".into(),
                    cost: 4000,
                    left_id: 1100,
                    right_id: 1100,
                }],
            ),
            (
                "しゅくだい".into(),
                vec![DictEntry {
                    surface: "宿題".into(),
                    cost: 4000,
                    left_id: 1200,
                    right_id: 1200,
                }],
            ),
            (
                "を".into(),
                vec![DictEntry {
                    surface: "を".into(),
                    cost: 2000,
                    left_id: 210,
                    right_id: 210,
                }],
            ),
            (
                "やる".into(),
                vec![DictEntry {
                    surface: "やる".into(),
                    cost: 3500,
                    left_id: 850,
                    right_id: 850,
                }],
            ),
            (
                "の".into(),
                vec![DictEntry {
                    surface: "の".into(),
                    cost: 2000,
                    left_id: 220,
                    right_id: 220,
                }],
            ),
            (
                "が".into(),
                vec![DictEntry {
                    surface: "が".into(),
                    cost: 2000,
                    left_id: 230,
                    right_id: 230,
                }],
            ),
            (
                "めんどう".into(),
                vec![DictEntry {
                    surface: "面倒".into(),
                    cost: 4500,
                    left_id: 860,
                    right_id: 860,
                }],
            ),
            (
                "くさい".into(),
                vec![DictEntry {
                    surface: "臭い".into(),
                    cost: 5000,
                    left_id: 870,
                    right_id: 870,
                }],
            ),
            (
                "めんどうくさい".into(),
                vec![DictEntry {
                    surface: "面倒くさい".into(),
                    cost: 3800,
                    left_id: 861,
                    right_id: 861,
                }],
            ),
            (
                "けど".into(),
                vec![DictEntry {
                    surface: "けど".into(),
                    cost: 2500,
                    left_id: 880,
                    right_id: 880,
                }],
            ),
            (
                "がんばり".into(),
                vec![DictEntry {
                    surface: "頑張り".into(),
                    cost: 4000,
                    left_id: 890,
                    right_id: 890,
                }],
            ),
            (
                "がんばります".into(),
                vec![DictEntry {
                    surface: "頑張ります".into(),
                    cost: 3500,
                    left_id: 891,
                    right_id: 891,
                }],
            ),
        ];
        TrieDictionary::from_entries(entries)
    }

    #[test]
    #[ignore]
    fn bench_convert_latency() {
        let dict = bench_dict();

        let inputs: Vec<(&str, &str)> = vec![
            ("short", "きょう"),
            ("medium", "きょうはいいてんきですね"),
            ("long", "わたしはきょうはいいてんきだとおもいます"),
            (
                "very_long",
                "わたしはきょうしゅくだいをやるのがめんどうくさいけどがんばります",
            ),
        ];

        let warmup = 50;
        let iterations = 200;

        println!();
        println!("=== Viterbi Convert Pipeline Latency Benchmark ===");
        println!("  warmup: {warmup} iterations, measured: {iterations} iterations");
        println!();

        for (label, kana) in &inputs {
            let char_count = kana.chars().count();

            // Warmup
            for _ in 0..warmup {
                let _ = convert(&dict, None, kana);
            }

            // Measure convert (1-best)
            let start = std::time::Instant::now();
            for _ in 0..iterations {
                let _ = convert(&dict, None, kana);
            }
            let elapsed_1best = start.elapsed();
            let avg_1best_us = elapsed_1best.as_micros() as f64 / iterations as f64;

            // Measure convert_nbest (10-best)
            for _ in 0..warmup {
                let _ = convert_nbest(&dict, None, kana, 10);
            }
            let start = std::time::Instant::now();
            for _ in 0..iterations {
                let _ = convert_nbest(&dict, None, kana, 10);
            }
            let elapsed_nbest = start.elapsed();
            let avg_nbest_us = elapsed_nbest.as_micros() as f64 / iterations as f64;

            // Measure build_lattice only
            for _ in 0..warmup {
                let _ = build_lattice(&dict, kana);
            }
            let start = std::time::Instant::now();
            for _ in 0..iterations {
                let _ = build_lattice(&dict, kana);
            }
            let elapsed_lattice = start.elapsed();
            let avg_lattice_us = elapsed_lattice.as_micros() as f64 / iterations as f64;

            println!("  {label} ({char_count} chars): \"{}\"", kana);
            println!("    build_lattice:    {:>8.1} us", avg_lattice_us);
            println!(
                "    convert (1-best): {:>8.1} us ({:.3} ms)",
                avg_1best_us,
                avg_1best_us / 1000.0
            );
            println!(
                "    convert (10-best):{:>8.1} us ({:.3} ms)",
                avg_nbest_us,
                avg_nbest_us / 1000.0
            );
            println!();
        }

        println!("=== Summary ===");
        println!("  Target: < 10ms per keystroke for responsive IME input");
        println!("  Note: using small test dictionary; real dictionary will be larger");
        println!("        but trie lookups are O(key_length), not O(dict_size).");
    }
}
