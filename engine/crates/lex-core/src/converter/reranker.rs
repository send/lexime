use tracing::{debug, debug_span};

use crate::dict::connection::ConnectionMatrix;
use crate::dict::Dictionary;
use crate::settings::settings;
use crate::user_history::UserHistory;

use super::features::{compute_structure_cost, extract_features, FeatureWeights};
use super::viterbi::ScoredPath;

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
pub fn rerank(
    paths: &mut Vec<ScoredPath>,
    conn: Option<&ConnectionMatrix>,
    dict: Option<&dyn Dictionary>,
) {
    let _span = debug_span!("rerank", paths_in = paths.len()).entered();
    if paths.len() <= 1 {
        return;
    }

    // Step 1: Compute structure_cost for each path.
    //
    // Transitions FROM a prefix POS (role == 3) get a floor of half the
    // filter threshold. Without this, a prefix→content-word transition
    // (e.g. 今[prefix]→デスネ with conn=256) can drag min_sc so low that
    // the hard filter drops correct multi-segment paths like 今|です|ね.
    let cap = settings().reranker.structure_cost_transition_cap;
    let prefix_floor = (settings().reranker.structure_cost_filter / 2).min(cap);
    let structure_costs: Vec<i64> = paths
        .iter()
        .map(|p| compute_structure_cost(p, conn, cap, prefix_floor))
        .collect();

    // Step 2: Hard filter — drop paths exceeding min + threshold.
    //
    // For min_sc computation, single-segment paths (0 transitions, sc=0) are
    // imputed with prefix_floor so they don't set an artificially low baseline.
    // Combined with the prefix-transition floor in step 1, this ensures the
    // threshold is high enough to keep correct multi-segment paths.
    let filter = settings().reranker.structure_cost_filter;
    let min_sc = structure_costs
        .iter()
        .zip(paths.iter())
        .map(|(&sc, p)| {
            if p.segments.len() <= 1 {
                prefix_floor
            } else {
                sc
            }
        })
        .min()
        .expect("paths guaranteed non-empty after early return");
    let threshold = min_sc + filter;
    let mut kept_sc: Vec<i64> = Vec::new();
    {
        let mut i = 0;
        paths.retain(|_| {
            let keep = structure_costs[i] <= threshold;
            if keep {
                kept_sc.push(structure_costs[i]);
            }
            i += 1;
            keep
        });
    }

    // Step 3: Feature extraction + weighted cost adjustment.
    // Pass pre-computed structure_cost to avoid recomputing transition costs.
    // Skip dictionary lookups when per-segment penalties are both zero.
    let weights = FeatureWeights::from_settings();
    let need_dict = weights.te_kanji != 0 || weights.single_kanji != 0;
    let dict_for_features = if need_dict { dict } else { None };
    for (path, &sc) in paths.iter_mut().zip(kept_sc.iter()) {
        let features = extract_features(path, conn, dict_for_features, cap, prefix_floor, Some(sc));
        path.viterbi_cost += features.weighted_cost(&weights);
    }

    paths.sort_by_key(|p| p.viterbi_cost);
    debug!(paths_out = paths.len());
}

/// Apply user-history boosts to N-best paths, then re-sort.
///
/// Unigram and bigram boosts are subtracted from each path's cost so that
/// learned candidates float to the top. Because this operates on complete
/// paths (not individual lattice nodes), it cannot cause the fragmentation
/// problems that in-Viterbi boosting could.
pub fn history_rerank(paths: &mut [ScoredPath], history: &UserHistory) {
    let _span = debug_span!("history_rerank", paths_count = paths.len()).entered();
    if paths.is_empty() {
        return;
    }
    let now = crate::user_history::now_epoch();
    for path in paths.iter_mut() {
        // Per-segment boosts normalized by segment count. Fragmented paths
        // (e.g. き→機 + が + し + ます) accumulate boosts from common particles
        // (が, し, は, etc.) across ALL prior conversions, giving them a structural
        // advantage over compound paths. Dividing by segment count neutralizes this.
        let seg_count = path.segments.len().max(1) as i64;
        let mut seg_boost: i64 = 0;
        for seg in &path.segments {
            seg_boost += history.unigram_boost(&seg.reading, &seg.surface, now);
        }
        for pair in path.segments.windows(2) {
            seg_boost +=
                history.bigram_boost(&pair[0].surface, &pair[1].reading, &pair[1].surface, now);
        }
        let mut boost = seg_boost / seg_count;

        // Whole-path boost (not normalized): reward paths whose full reading→surface
        // has been explicitly selected. This is the strongest learning signal and is
        // not subject to cross-reading contamination.
        let full_reading = path.full_reading();
        let full_surface = path.surface_key();
        boost += history.unigram_boost(&full_reading, &full_surface, now) * 5;
        path.viterbi_cost -= boost;
    }
    paths.sort_by_key(|p| p.viterbi_cost);
    debug!(best_cost = paths.first().map(|p| p.viterbi_cost));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::converter::viterbi::RichSegment;
    use crate::dict::connection::ConnectionMatrix;
    use crate::dict::{DictEntry, Dictionary, SearchResult};

    /// Build a minimal ConnectionMatrix with the given roles vector.
    fn conn_with_roles(roles: Vec<u8>) -> ConnectionMatrix {
        let num_ids = roles.len() as u16;
        let costs = vec![0i16; num_ids as usize * num_ids as usize];
        ConnectionMatrix::new_owned(num_ids, 0, 0, roles, costs)
    }

    fn seg(reading: &str, surface: &str, left_id: u16) -> RichSegment {
        RichSegment {
            reading: reading.to_string(),
            surface: surface.to_string(),
            left_id,
            right_id: left_id,
            word_cost: 0,
        }
    }

    fn path(segments: Vec<RichSegment>, cost: i64) -> ScoredPath {
        ScoredPath {
            segments,
            viterbi_cost: cost,
        }
    }

    /// Build a minimal ConnectionMatrix with the given roles vector and
    /// function-word ID range.
    fn conn_with_roles_and_fw(roles: Vec<u8>, fw_min: u16, fw_max: u16) -> ConnectionMatrix {
        let num_ids = roles.len() as u16;
        let costs = vec![0i16; num_ids as usize * num_ids as usize];
        ConnectionMatrix::new_owned(num_ids, fw_min, fw_max, roles, costs)
    }

    #[test]
    fn te_form_kanji_penalty_applied() {
        // ID 2 = function word (fw_min=2, fw_max=2), ID 1 = content word
        let conn = conn_with_roles_and_fw(vec![0u8, 0, 0], 2, 2);

        // Path A: で + 見る (kanji after て/で FW) — te_kanji penalty applied
        // Path B: で + みる (hiragana after て/で FW) — no te_kanji penalty
        // Both start at cost=100.  After rerank the kanji path gets a
        // te_kanji penalty.  The script bonus for kanji may offset it, so we
        // only assert the cost *delta* equals the configured penalty.
        let mut with_kanji = vec![
            path(vec![seg("で", "で", 2), seg("みる", "見る", 1)], 100),
            path(vec![seg("で", "で", 2), seg("みる", "みる", 1)], 99999), // dummy
        ];
        let mut without_kanji = vec![
            path(vec![seg("で", "で", 2), seg("みる", "みる", 1)], 100),
            path(vec![seg("で", "で", 2), seg("みる", "見る", 1)], 99999), // dummy
        ];

        rerank(&mut with_kanji, Some(&conn), None);
        rerank(&mut without_kanji, Some(&conn), None);

        let kanji_cost = with_kanji
            .iter()
            .find(|p| p.segments[1].surface == "見る")
            .unwrap()
            .viterbi_cost;
        // Compare against a non-te baseline (は instead of で) to isolate
        // the te_penalty amount from the script cost difference.
        let te_penalty = settings().reranker.te_form_kanji_penalty;
        let mut baseline_kanji = vec![
            path(vec![seg("は", "は", 2), seg("みる", "見る", 1)], 100),
            path(vec![seg("は", "は", 2), seg("みる", "みる", 1)], 99999),
        ];
        rerank(&mut baseline_kanji, Some(&conn), None);
        let baseline_kanji_cost = baseline_kanji
            .iter()
            .find(|p| p.segments[1].surface == "見る")
            .unwrap()
            .viterbi_cost;

        // te_penalty = (cost with te-form context) - (cost without te-form context)
        let actual_te_delta = kanji_cost - baseline_kanji_cost;
        assert_eq!(
            actual_te_delta, te_penalty,
            "te-form kanji penalty should add exactly {} to cost",
            te_penalty
        );
    }

    #[test]
    fn length_variance_excludes_fw_segments() {
        // Two paths with identical starting costs. Path A has a 2-char FW
        // segment that should be excluded; Path B has the same segment as CW.
        //   Path A (FW): [3, 2(FW), 3, 3] → FW excluded → variance of [3,3,3] = 0
        //   Path B (CW): [3, 2(CW), 3, 3] → all included → variance of [3,2,3,3] > 0
        let conn = conn_with_roles_and_fw(vec![0u8, 0, 0], 2, 2);

        let mut paths = vec![
            // Path A: "から" is FW (id=2)
            path(
                vec![
                    seg("きょう", "今日", 1),
                    seg("から", "から", 2), // FW
                    seg("いいい", "良い", 1),
                    seg("てんき", "天気", 1),
                ],
                100,
            ),
            // Path B: "から" is CW (id=1)
            path(
                vec![
                    seg("きょう", "今日", 1),
                    seg("から", "から", 1), // content word
                    seg("いいい", "良い", 1),
                    seg("てんき", "天気", 1),
                ],
                100,
            ),
        ];

        rerank(&mut paths, Some(&conn), None);

        // The FW path should rank first (lower cost) because its 2-char
        // particle is excluded from the variance calculation.
        assert_eq!(paths[0].segments[1].left_id, 2, "FW path should rank first");
        assert!(
            paths[0].viterbi_cost < paths[1].viterbi_cost,
            "FW path cost ({}) should be less than CW path cost ({}) due to excluded FW variance",
            paths[0].viterbi_cost,
            paths[1].viterbi_cost
        );
    }

    #[test]
    fn length_variance_excludes_single_char_segments() {
        // Single-char reading segments (verb inflections like し, き) should
        // be excluded from variance even when their POS is not FW.
        //   Path A: [4, 1(CW), 1(FW)] → exclude both 1-char → only [4] → no variance
        //   Path B: [4, 1(CW), 2]      → exclude 1-char CW  → [4, 2] → small variance
        // Without single-char exclusion, Path A would get a large penalty from [4,1].
        let conn = conn_with_roles_and_fw(vec![0u8, 0, 0], 2, 2);

        let mut paths = vec![
            // Path A: "し" is 1-char CW — should be excluded from variance
            path(
                vec![
                    seg("せつめい", "説明", 1),
                    seg("し", "し", 1), // 1-char CW (する連用形)
                    seg("て", "て", 2), // FW
                ],
                100,
            ),
            // Path B: "ある" is 2-char CW — included in variance
            path(
                vec![
                    seg("せつめい", "説明", 1),
                    seg("し", "し", 1), // 1-char CW
                    seg("ある", "ある", 1),
                ],
                100,
            ),
        ];

        rerank(&mut paths, Some(&conn), None);

        // Path A should rank better: its 1-char segments are all excluded,
        // leaving no variance. Path B has [4, 2] with nonzero variance.
        assert_eq!(
            paths[0].segments[2].surface, "て",
            "path where single-char reading segments are excluded from length variance should rank first (no variance penalty)"
        );
        assert!(paths[0].viterbi_cost < paths[1].viterbi_cost);
    }

    /// A minimal dictionary for testing compound exemption.
    struct MockDict {
        entries: Vec<(String, Vec<DictEntry>)>,
    }

    impl MockDict {
        fn new(pairs: &[(&str, &str)]) -> Self {
            Self {
                entries: pairs
                    .iter()
                    .map(|&(reading, surface)| {
                        (
                            reading.to_string(),
                            vec![DictEntry {
                                surface: surface.to_string(),
                                cost: 5000,
                                left_id: 1,
                                right_id: 1,
                            }],
                        )
                    })
                    .collect(),
            }
        }
    }

    impl Dictionary for MockDict {
        fn lookup(&self, reading: &str) -> Vec<DictEntry> {
            self.entries
                .iter()
                .find(|(r, _)| r == reading)
                .map(|(_, e)| e.clone())
                .unwrap_or_default()
        }
        fn predict(&self, _prefix: &str, _max_results: usize) -> Vec<SearchResult> {
            Vec::new()
        }
        fn common_prefix_search(&self, _query: &str) -> Vec<SearchResult> {
            Vec::new()
        }
    }

    #[test]
    fn single_char_kanji_penalty_applied() {
        // ID 1 = content word (role 0)
        let roles = vec![0u8, 0];
        let conn = conn_with_roles(roles);

        // Path A: single-char kanji content word "ね" → "根" — penalty applied
        // Path B: multi-char content word "ねこ" → "猫" — no penalty
        let mut paths = vec![
            path(vec![seg("かくにん", "確認", 1), seg("ね", "根", 1)], 100),
            path(vec![seg("かくにんね", "確認ね", 1)], 100),
        ];

        rerank(&mut paths, Some(&conn), None);

        // The path with 根 should have penalty applied
        let root_path = paths
            .iter()
            .find(|p| p.segments.len() == 2 && p.segments[1].surface == "根")
            .unwrap();
        let other_path = paths.iter().find(|p| p.segments.len() == 1).unwrap();
        assert!(
            root_path.viterbi_cost > other_path.viterbi_cost,
            "single-char kanji content-word should be penalized: root={}, other={}",
            root_path.viterbi_cost,
            other_path.viterbi_cost
        );
    }

    #[test]
    fn single_char_kanji_penalty_exempt_with_dict_compound() {
        // ID 1 = content word (role 0)
        let roles = vec![0u8, 0];
        let conn = conn_with_roles(roles);

        // Dictionary has "きょうと" → "京都" compound
        let dict = MockDict::new(&[("きょうと", "京都")]);

        // "と" → "都" is single-char kanji CW, but "きょう" + "と" = "きょうと"
        // exists in dictionary, so it should be exempt.
        // Need 2+ paths so rerank doesn't short-circuit.
        let dummy = path(vec![seg("きょうと", "京都", 1)], 99999);

        let cost_with_dict = {
            let mut p = vec![
                path(vec![seg("きょう", "京", 1), seg("と", "都", 1)], 100),
                dummy.clone(),
            ];
            rerank(&mut p, Some(&conn), Some(&dict));
            p.iter()
                .find(|pp| pp.segments.len() == 2)
                .unwrap()
                .viterbi_cost
        };

        // Without dict: penalty applied
        let cost_without_dict = {
            let mut p = vec![
                path(vec![seg("きょう", "京", 1), seg("と", "都", 1)], 100),
                dummy.clone(),
            ];
            rerank(&mut p, Some(&conn), None);
            p.iter()
                .find(|pp| pp.segments.len() == 2)
                .unwrap()
                .viterbi_cost
        };

        let penalty = settings().reranker.single_char_kanji_penalty;
        assert_eq!(
            cost_without_dict - cost_with_dict,
            penalty,
            "compound exemption should save exactly the penalty amount"
        );
    }

    #[test]
    fn single_char_kanji_penalty_not_exempt_when_surface_mismatches() {
        // ID 1 = content word (role 0)
        let roles = vec![0u8, 0];
        let conn = conn_with_roles(roles);

        // Dictionary has "ますね" → "増根" but segments are ます + 根,
        // so combined_surface = "ます根" ≠ "増根" — should NOT be exempt.
        let dict = MockDict::new(&[("ますね", "増根")]);

        let dummy = path(vec![seg("ますね", "ますね", 1)], 99999);

        let cost_with_dict = {
            let mut p = vec![
                path(vec![seg("ます", "ます", 1), seg("ね", "根", 1)], 100),
                dummy.clone(),
            ];
            rerank(&mut p, Some(&conn), Some(&dict));
            p.iter()
                .find(|pp| pp.segments.len() == 2)
                .unwrap()
                .viterbi_cost
        };

        let cost_without_dict = {
            let mut p = vec![
                path(vec![seg("ます", "ます", 1), seg("ね", "根", 1)], 100),
                dummy.clone(),
            ];
            rerank(&mut p, Some(&conn), None);
            p.iter()
                .find(|pp| pp.segments.len() == 2)
                .unwrap()
                .viterbi_cost
        };

        assert_eq!(
            cost_with_dict, cost_without_dict,
            "surface mismatch should not grant exemption"
        );
    }

    #[test]
    fn single_char_kanji_feature_not_counted_for_function_word() {
        // Verify that single-char kanji feature count is 0 for FW segments
        // regardless of the configured penalty weight.
        // ID 2 has role=1 (FW) so extract_features skips it (role != 0).
        let conn = conn_with_roles_and_fw(vec![0u8, 0, 1], 2, 2);
        let cap = settings().reranker.structure_cost_transition_cap;
        let prefix_floor = (settings().reranker.structure_cost_filter / 2).min(cap);

        // CW single-char kanji — should count
        let cw_path = path(vec![seg("ね", "根", 1)], 100);
        let cw_features = extract_features(&cw_path, Some(&conn), None, cap, prefix_floor, None);
        assert_eq!(
            cw_features.single_kanji_count, 1,
            "CW single-char kanji should be counted"
        );

        // FW single-char — should NOT count (role checked)
        let fw_path = path(vec![seg("ね", "根", 2)], 100); // FW POS
        let fw_features = extract_features(&fw_path, Some(&conn), None, cap, prefix_floor, None);
        assert_eq!(
            fw_features.single_kanji_count, 0,
            "FW should not trigger single-char kanji feature"
        );
    }

    #[test]
    fn single_char_kanji_penalty_not_applied_to_multi_char_reading() {
        // ID 1 = content word (role 0)
        let roles = vec![0u8, 0];
        let conn = conn_with_roles(roles);

        // Compare multi-char reading (no penalty) vs single-char reading (penalty)
        let mut paths = vec![
            path(vec![seg("ねこ", "猫", 1)], 100), // 2-char reading
            path(vec![seg("ね", "根", 1)], 100),   // 1-char reading
        ];

        rerank(&mut paths, Some(&conn), None);

        let multi = paths
            .iter()
            .find(|p| p.segments[0].reading == "ねこ")
            .unwrap();
        let single = paths
            .iter()
            .find(|p| p.segments[0].reading == "ね")
            .unwrap();
        let penalty = settings().reranker.single_char_kanji_penalty;
        assert!(
            single.viterbi_cost - multi.viterbi_cost >= penalty,
            "only single-char reading should get penalty: multi={}, single={}",
            multi.viterbi_cost,
            single.viterbi_cost,
        );
    }

    #[test]
    fn te_form_kanji_feature_not_counted_for_non_te_function_word() {
        // Verify that te_kanji_count is 0 when the preceding FW is not て/で.
        let conn = conn_with_roles_and_fw(vec![0u8, 0, 0], 2, 2);
        let cap = settings().reranker.structure_cost_transition_cap;
        let prefix_floor = (settings().reranker.structure_cost_filter / 2).min(cap);

        // "は" (FW, not て/で) + "見る" (kanji) — should NOT trigger te-form
        let ha_path = path(vec![seg("は", "は", 2), seg("みる", "見る", 1)], 100);
        let ha_features = extract_features(&ha_path, Some(&conn), None, cap, prefix_floor, None);
        assert_eq!(
            ha_features.te_kanji_count, 0,
            "は is not て/で — no te-form kanji"
        );

        // "で" (FW, て/で) + "見る" (kanji) — should trigger te-form
        let de_path = path(vec![seg("で", "で", 2), seg("みる", "見る", 1)], 100);
        let de_features = extract_features(&de_path, Some(&conn), None, cap, prefix_floor, None);
        assert_eq!(
            de_features.te_kanji_count, 1,
            "で + kanji should trigger te-form"
        );
    }
}
