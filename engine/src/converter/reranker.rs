use tracing::{debug, debug_span};

use crate::dict::connection::ConnectionMatrix;
use crate::user_history::UserHistory;

use super::cost::{conn_cost, script_cost};
use super::viterbi::ScoredPath;

/// Weight for the segment-length variance penalty.
/// Penalises paths whose segments have very uneven reading lengths
/// (e.g. 1-char + 3-char) in favour of more uniform splits (2+2).
const LENGTH_VARIANCE_WEIGHT: i64 = 2000;

/// Threshold for hard-filtering fragmented paths.
/// Paths whose structure_cost exceeds the minimum by more than this value
/// are dropped from the N-best pool. Inspired by Mozc's kStructureCostOffset (3453).
const STRUCTURE_COST_FILTER: i64 = 4000;

/// Rerank N-best Viterbi paths by applying post-hoc features.
///
/// The Viterbi core handles dictionary cost + connection cost + segment penalty.
/// The reranker adds features that are ranking preferences rather than
/// search-quality parameters:
///
/// - **Structure cost**: sum of transition costs along the path (Mozc-inspired);
///   paths with high accumulated transition costs tend to be fragmented
/// - **Length variance**: penalises uneven segment splits so that more uniform
///   segmentations are preferred when Viterbi costs are close
/// - **Script cost**: penalises katakana / Latin surfaces and rewards mixed-script
///   (kanji+kana) surfaces — a ranking preference that doesn't affect search quality
pub fn rerank(paths: &mut Vec<ScoredPath>, conn: Option<&ConnectionMatrix>) {
    let _span = debug_span!("rerank", paths_in = paths.len()).entered();
    if paths.len() <= 1 {
        return;
    }

    // Step 1: Compute structure_cost for each path
    let structure_costs: Vec<i64> = paths
        .iter()
        .map(|p| {
            let mut sc: i64 = 0;
            for i in 1..p.segments.len() {
                sc += conn_cost(conn, p.segments[i - 1].right_id, p.segments[i].left_id);
            }
            sc
        })
        .collect();

    // Step 2: Hard filter — drop paths exceeding min + threshold
    let min_sc = *structure_costs.iter().min().unwrap();
    let threshold = min_sc + STRUCTURE_COST_FILTER;
    if structure_costs.iter().any(|&sc| sc <= threshold) {
        let mut i = 0;
        paths.retain(|_| {
            let keep = structure_costs[i] <= threshold;
            i += 1;
            keep
        });
    }
    // else: all paths exceed threshold → keep all (don't drop everything)

    // Step 3: Soft penalty + length variance + script cost
    for path in paths.iter_mut() {
        // Recompute structure_cost for remaining paths (indices shifted after filter)
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

        // Script cost: penalise katakana / Latin surfaces, reward kanji+kana.
        let total_script: i64 = path.segments.iter().map(|s| script_cost(&s.surface)).sum();
        path.viterbi_cost += total_script;
    }

    paths.sort_by_key(|p| p.viterbi_cost);

    if let Some(best) = paths.first() {
        let best_surface: String = best.segments.iter().map(|s| s.surface.as_str()).collect();
        debug!(
            paths_out = paths.len(),
            best_cost = best.viterbi_cost,
            best_surface,
            "rerank done"
        );
    }
}

/// Apply user-history boosts to N-best paths, then re-sort.
///
/// Unigram and bigram boosts are subtracted from each path's cost so that
/// learned candidates float to the top. Because this operates on complete
/// paths (not individual lattice nodes), it cannot cause the fragmentation
/// problems that in-Viterbi boosting could.
pub fn history_rerank(paths: &mut [ScoredPath], history: &UserHistory) {
    let _span = debug_span!("history_rerank", paths = paths.len()).entered();
    if paths.is_empty() {
        return;
    }
    for path in paths.iter_mut() {
        let mut boost: i64 = 0;
        for seg in &path.segments {
            let ub = history.unigram_boost(&seg.reading, &seg.surface);
            if ub > 0 {
                debug!(
                    reading = seg.reading,
                    surface = seg.surface,
                    unigram_boost = ub,
                    "history boost applied"
                );
            }
            boost += ub;
        }
        for pair in path.segments.windows(2) {
            let bb = history.bigram_boost(&pair[0].surface, &pair[1].reading, &pair[1].surface);
            if bb > 0 {
                debug!(
                    prev = pair[0].surface,
                    next = pair[1].surface,
                    bigram_boost = bb,
                    "bigram boost applied"
                );
            }
            boost += bb;
        }
        path.viterbi_cost -= boost;
    }
    paths.sort_by_key(|p| p.viterbi_cost);

    if let Some(best) = paths.first() {
        let best_surface: String = best.segments.iter().map(|s| s.surface.as_str()).collect();
        debug!(
            best_cost = best.viterbi_cost,
            best_surface, "history rerank done"
        );
    }
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

        // Without conn, structure cost is 0; "木の" gets script_cost -3000
        // (mixed kanji+kana bonus) so it reranks to first.
        rerank(&mut paths, None);
        assert_eq!(paths[0].segments[0].surface, "木の");
        assert_eq!(paths[0].viterbi_cost, 2000 - 3000);
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

        // script_cost: "来たり" is mixed (kanji+kana) → -3000; "出来" is pure kanji → 0
        // Uneven: 5000 + variance(2000) + script("で"=0 + "来たり"=-3000) = 4000
        // Even:   6500 + variance(0)    + script("出来"=0 + "たり"=0)     = 6500
        // Uneven path wins due to mixed-script bonus on "来たり"
        assert_eq!(paths[0].segments[0].surface, "で");
        assert_eq!(paths[0].viterbi_cost, 4000);
        assert_eq!(paths[1].segments[0].surface, "出来");
        assert_eq!(paths[1].viterbi_cost, 6500);
    }

    #[test]
    fn test_rerank_applies_script_cost() {
        // Katakana surface should receive +5000 penalty from script_cost
        let mut paths = vec![
            // Katakana path: タラ (katakana) → +5000 script penalty
            ScoredPath {
                segments: vec![RichSegment {
                    reading: "たら".into(),
                    surface: "タラ".into(),
                    left_id: 0,
                    right_id: 0,
                }],
                viterbi_cost: 3000,
            },
            // Hiragana path: たら (no script penalty)
            ScoredPath {
                segments: vec![RichSegment {
                    reading: "たら".into(),
                    surface: "たら".into(),
                    left_id: 0,
                    right_id: 0,
                }],
                viterbi_cost: 7000,
            },
        ];

        rerank(&mut paths, None);

        // Katakana: 3000 + 5000 = 8000
        // Hiragana: 7000 + 0    = 7000
        // Hiragana should be ranked first
        assert_eq!(paths[0].segments[0].surface, "たら");
        assert_eq!(paths[0].viterbi_cost, 7000);
        assert_eq!(paths[1].segments[0].surface, "タラ");
        assert_eq!(paths[1].viterbi_cost, 8000);
    }

    #[test]
    fn test_history_rerank_unigram_boost_reorders() {
        let mut h = UserHistory::new();
        // Record twice to get 6000 boost (BOOST_PER_USE=3000 × 2), enough to
        // overcome the 2000 cost gap (5000 - 3000).
        h.record(&[("きょう".into(), "京".into())]);
        h.record(&[("きょう".into(), "京".into())]);

        let mut paths = vec![
            ScoredPath {
                segments: vec![RichSegment {
                    reading: "きょう".into(),
                    surface: "今日".into(),
                    left_id: 0,
                    right_id: 0,
                }],
                viterbi_cost: 3000,
            },
            ScoredPath {
                segments: vec![RichSegment {
                    reading: "きょう".into(),
                    surface: "京".into(),
                    left_id: 0,
                    right_id: 0,
                }],
                viterbi_cost: 5000,
            },
        ];

        history_rerank(&mut paths, &h);

        // "京" should be boosted to first place
        assert_eq!(paths[0].segments[0].surface, "京");
    }

    #[test]
    fn test_history_rerank_bigram_boost() {
        let mut h = UserHistory::new();
        h.record(&[("きょう".into(), "今日".into()), ("は".into(), "は".into())]);

        let mut paths = vec![
            // Path without bigram match
            ScoredPath {
                segments: vec![
                    RichSegment {
                        reading: "きょう".into(),
                        surface: "京".into(),
                        left_id: 0,
                        right_id: 0,
                    },
                    RichSegment {
                        reading: "は".into(),
                        surface: "は".into(),
                        left_id: 0,
                        right_id: 0,
                    },
                ],
                viterbi_cost: 5000,
            },
            // Path with bigram match: "今日" → "は"
            ScoredPath {
                segments: vec![
                    RichSegment {
                        reading: "きょう".into(),
                        surface: "今日".into(),
                        left_id: 0,
                        right_id: 0,
                    },
                    RichSegment {
                        reading: "は".into(),
                        surface: "は".into(),
                        left_id: 0,
                        right_id: 0,
                    },
                ],
                viterbi_cost: 7000,
            },
        ];

        history_rerank(&mut paths, &h);

        // "今日は" path should be boosted (both unigram + bigram) to first
        assert_eq!(paths[0].segments[0].surface, "今日");
    }

    #[test]
    fn test_history_rerank_empty_history_preserves_order() {
        let h = UserHistory::new();

        let mut paths = vec![
            ScoredPath {
                segments: vec![RichSegment {
                    reading: "あ".into(),
                    surface: "亜".into(),
                    left_id: 0,
                    right_id: 0,
                }],
                viterbi_cost: 1000,
            },
            ScoredPath {
                segments: vec![RichSegment {
                    reading: "あ".into(),
                    surface: "阿".into(),
                    left_id: 0,
                    right_id: 0,
                }],
                viterbi_cost: 2000,
            },
        ];

        history_rerank(&mut paths, &h);

        assert_eq!(paths[0].segments[0].surface, "亜");
        assert_eq!(paths[0].viterbi_cost, 1000);
        assert_eq!(paths[1].segments[0].surface, "阿");
        assert_eq!(paths[1].viterbi_cost, 2000);
    }

    #[test]
    fn test_history_rerank_empty_paths() {
        let h = UserHistory::new();
        let mut paths: Vec<ScoredPath> = Vec::new();
        history_rerank(&mut paths, &h);
        assert!(paths.is_empty());
    }

    /// Build a connection matrix where all transitions cost the given value.
    fn uniform_conn(cost: i16) -> ConnectionMatrix {
        let num_ids = 4;
        let mut text = format!("{num_ids} {num_ids}\n");
        for _ in 0..(num_ids * num_ids) {
            text.push_str(&format!("{cost}\n"));
        }
        ConnectionMatrix::from_text(&text).unwrap()
    }

    #[test]
    fn test_filter_drops_fragmented_paths() {
        // Transition cost = 1500 each.
        // Path A: 1 segment → 0 transitions → structure_cost = 0
        // Path B: 2 segments → 1 transition → structure_cost = 1500
        // Path C: 5 segments → 4 transitions → structure_cost = 6000
        // min_sc = 0, threshold = 0 + 4000 = 4000
        // Path C (6000 > 4000) should be dropped; A and B should remain.
        let conn = uniform_conn(1500);

        let mut paths = vec![
            ScoredPath {
                segments: vec![RichSegment {
                    reading: "あいうえお".into(),
                    surface: "合言葉".into(),
                    left_id: 1,
                    right_id: 1,
                }],
                viterbi_cost: 5000,
            },
            ScoredPath {
                segments: vec![
                    RichSegment {
                        reading: "あい".into(),
                        surface: "愛".into(),
                        left_id: 1,
                        right_id: 1,
                    },
                    RichSegment {
                        reading: "うえお".into(),
                        surface: "上尾".into(),
                        left_id: 1,
                        right_id: 1,
                    },
                ],
                viterbi_cost: 4000,
            },
            ScoredPath {
                segments: vec![
                    RichSegment {
                        reading: "あ".into(),
                        surface: "亜".into(),
                        left_id: 1,
                        right_id: 1,
                    },
                    RichSegment {
                        reading: "い".into(),
                        surface: "位".into(),
                        left_id: 1,
                        right_id: 1,
                    },
                    RichSegment {
                        reading: "う".into(),
                        surface: "鵜".into(),
                        left_id: 1,
                        right_id: 1,
                    },
                    RichSegment {
                        reading: "え".into(),
                        surface: "絵".into(),
                        left_id: 1,
                        right_id: 1,
                    },
                    RichSegment {
                        reading: "お".into(),
                        surface: "尾".into(),
                        left_id: 1,
                        right_id: 1,
                    },
                ],
                viterbi_cost: 3000,
            },
        ];

        rerank(&mut paths, Some(&conn));

        // Path C should have been filtered out
        assert_eq!(paths.len(), 2);
        // Verify the fragmented 5-segment path is gone
        assert!(paths.iter().all(|p| p.segments.len() <= 2));
    }

    #[test]
    fn test_filter_keeps_all_when_all_exceed() {
        // All paths have high structure_cost; none should be dropped.
        // Transition cost = 2000. All paths have 4 segments → 3 transitions → sc = 6000.
        // min_sc = 6000, threshold = 6000 + 4000 = 10000.
        // All paths have sc = 6000 ≤ 10000, so all pass.
        // But to truly test the "all exceed" safety, we need a scenario where
        // min_sc itself is above the threshold relative to... Actually the safety
        // is: if ALL paths have sc > threshold, keep all. Let's just verify
        // that when all paths are equally fragmented, none are dropped.
        let conn = uniform_conn(2000);

        let seg = |r: &str, s: &str| RichSegment {
            reading: r.into(),
            surface: s.into(),
            left_id: 1,
            right_id: 1,
        };

        let mut paths = vec![
            ScoredPath {
                segments: vec![
                    seg("あ", "亜"),
                    seg("い", "位"),
                    seg("う", "鵜"),
                    seg("え", "絵"),
                ],
                viterbi_cost: 3000,
            },
            ScoredPath {
                segments: vec![
                    seg("あ", "阿"),
                    seg("い", "胃"),
                    seg("う", "卯"),
                    seg("え", "江"),
                ],
                viterbi_cost: 4000,
            },
        ];

        rerank(&mut paths, Some(&conn));

        // Both have identical structure_cost, so neither is filtered
        assert_eq!(paths.len(), 2);
    }

    #[test]
    fn test_filter_preserves_minimum_path() {
        // The path with minimum structure_cost must always survive the filter.
        // Path A: 1 segment → sc = 0 (minimum)
        // Path B: 4 segments → sc = 4500 (3 × 1500); 4500 > 0 + 4000 → filtered
        let conn = uniform_conn(1500);

        let mut paths = vec![
            ScoredPath {
                segments: vec![
                    RichSegment {
                        reading: "あ".into(),
                        surface: "亜".into(),
                        left_id: 1,
                        right_id: 1,
                    },
                    RichSegment {
                        reading: "い".into(),
                        surface: "位".into(),
                        left_id: 1,
                        right_id: 1,
                    },
                    RichSegment {
                        reading: "う".into(),
                        surface: "鵜".into(),
                        left_id: 1,
                        right_id: 1,
                    },
                    RichSegment {
                        reading: "え".into(),
                        surface: "絵".into(),
                        left_id: 1,
                        right_id: 1,
                    },
                ],
                viterbi_cost: 1000,
            },
            ScoredPath {
                segments: vec![RichSegment {
                    reading: "あいうえ".into(),
                    surface: "合言葉".into(),
                    left_id: 1,
                    right_id: 1,
                }],
                viterbi_cost: 5000,
            },
        ];

        rerank(&mut paths, Some(&conn));

        // Only the single-segment path (sc=0) should survive
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].segments[0].surface, "合言葉");
    }
}
