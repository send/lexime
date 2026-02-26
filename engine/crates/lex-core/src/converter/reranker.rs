use tracing::{debug, debug_span};

use crate::dict::connection::ConnectionMatrix;
use crate::dict::Dictionary;
use crate::settings::settings;
use crate::unicode::is_kanji;
use crate::user_history::UserHistory;

use super::cost::{conn_cost, script_cost};
use super::viterbi::{RichSegment, ScoredPath};

/// Non-independent kanji penalty for a segment.
/// Returns penalty (> 0) if the segment is non-independent (形式名詞/補助動詞) with kanji surface.
pub(super) fn non_independent_kanji_penalty(seg: &RichSegment, conn: &ConnectionMatrix) -> i64 {
    if conn.is_non_independent(seg.left_id) && seg.surface.chars().any(is_kanji) {
        settings().reranker.non_independent_kanji_penalty
    } else {
        0
    }
}

/// Pronoun cost bonus for a segment (positive value = cost reduction).
pub(super) fn pronoun_bonus(seg: &RichSegment, conn: &ConnectionMatrix) -> i64 {
    if conn.is_pronoun(seg.left_id) {
        settings().reranker.pronoun_cost_bonus
    } else {
        0
    }
}

/// Te-form kanji penalty for a segment that follows て/で.
/// `prev` is the preceding segment (None for the first segment).
pub(super) fn te_form_kanji_penalty(
    prev: Option<&RichSegment>,
    curr: &RichSegment,
    conn: &ConnectionMatrix,
) -> i64 {
    if let Some(prev) = prev {
        if conn.is_function_word(prev.left_id)
            && (prev.surface == "て" || prev.surface == "で")
            && curr.surface.chars().any(is_kanji)
        {
            return settings().reranker.te_form_kanji_penalty;
        }
    }
    0
}

/// Single-char kanji noun penalty with dictionary compound exemption.
pub(super) fn single_char_kanji_penalty(
    seg: &RichSegment,
    idx: usize,
    segments: &[RichSegment],
    conn: &ConnectionMatrix,
    dict: Option<&dyn Dictionary>,
) -> i64 {
    if seg.reading.chars().count() != 1
        || !seg.surface.chars().any(is_kanji)
        || conn.role(seg.left_id) != 0
    {
        return 0;
    }
    let exempt = dict.is_some_and(|d| {
        if idx > 0 {
            let prev = &segments[idx - 1];
            let combined = format!("{}{}", prev.reading, seg.reading);
            if !d.lookup(&combined).is_empty() {
                return true;
            }
        }
        if idx + 1 < segments.len() {
            let next = &segments[idx + 1];
            let combined = format!("{}{}", seg.reading, next.reading);
            if !d.lookup(&combined).is_empty() {
                return true;
            }
        }
        false
    });
    if exempt {
        0
    } else {
        settings().reranker.single_char_kanji_penalty
    }
}

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

    // Step 1: Compute structure_cost for each path
    let mut structure_costs: Vec<i64> = paths
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
    let threshold = min_sc + settings().reranker.structure_cost_filter;
    if structure_costs.iter().any(|&sc| sc <= threshold) {
        let mut i = 0;
        let mut kept_costs = Vec::new();
        paths.retain(|_| {
            let keep = structure_costs[i] <= threshold;
            if keep {
                kept_costs.push(structure_costs[i]);
            }
            i += 1;
            keep
        });
        structure_costs = kept_costs;
    }
    // else: all paths exceed threshold → keep all (don't drop everything)

    // Step 3: Soft penalty + length variance + script cost
    // Reuse pre-computed structure costs (aligned with paths after filter).
    for (path, &structure_cost) in paths.iter_mut().zip(structure_costs.iter()) {
        // Add 25% of structure cost as penalty — enough to differentiate
        // fragmented paths without dominating the Viterbi cost.
        path.viterbi_cost += structure_cost / 4;

        // Length variance penalty: for paths with 3+ segments, penalise
        // uneven reading lengths. 2-segment paths are exempt because
        // "long content word + short particle" is natural Japanese.
        // Computed as sum-of-squared-deviations from the mean, scaled
        // by LENGTH_VARIANCE_WEIGHT / N.
        //
        // Function-word segments (particles like は, が) and single-char
        // reading segments are excluded from the variance calculation —
        // they are naturally short and should not penalise an otherwise
        // uniform segmentation.  Single-char readings cover verb inflection
        // pieces (し, き, け …) whose POS IDs may fall outside the FW range.
        let n = path.segments.len();
        if n >= 3 {
            let lengths: Vec<i64> = path
                .segments
                .iter()
                .filter_map(|s| {
                    let len = s.reading.chars().count() as i64;
                    if len > 1 && !conn.is_some_and(|c| c.is_function_word(s.left_id)) {
                        Some(len)
                    } else {
                        None
                    }
                })
                .collect();
            let n_var = lengths.len();
            if n_var >= 2 {
                let sum: i64 = lengths.iter().sum();
                // sum_sq_dev = Σ (len_i - mean)² × N  (multiplied through to stay in integers)
                //            = N × Σ len_i² - (Σ len_i)²
                let sum_sq: i64 = lengths.iter().map(|l| l * l).sum();
                let n_i64 = n_var as i64;
                let sum_sq_dev = n_i64 * sum_sq - sum * sum;
                // Divide by N² to get the true variance-based penalty:
                // penalty = (sum_sq_dev / N) * WEIGHT / N = sum_sq_dev * WEIGHT / N²
                path.viterbi_cost +=
                    sum_sq_dev * settings().reranker.length_variance_weight / (n_i64 * n_i64);
            }
        }

        // Script cost: penalise katakana / Latin surfaces, reward kanji+kana.
        let total_script: i64 = path
            .segments
            .iter()
            .map(|s| script_cost(&s.surface, s.reading.chars().count()))
            .sum();
        path.viterbi_cost += total_script;

        // Per-segment penalties: non-independent kanji, pronoun bonus,
        // te-form kanji, single-char kanji noun.
        if let Some(conn) = conn {
            for (i, seg) in path.segments.iter().enumerate() {
                let prev = if i > 0 {
                    Some(&path.segments[i - 1])
                } else {
                    None
                };
                path.viterbi_cost += non_independent_kanji_penalty(seg, conn);
                path.viterbi_cost -= pronoun_bonus(seg, conn);
                path.viterbi_cost += te_form_kanji_penalty(prev, seg, conn);
                path.viterbi_cost += single_char_kanji_penalty(seg, i, &path.segments, conn, dict);
            }
        }
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
        let full_reading: String = path.segments.iter().map(|s| s.reading.as_str()).collect();
        let full_surface: String = path.segments.iter().map(|s| s.surface.as_str()).collect();
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

    #[test]
    fn non_independent_kanji_penalty_applied() {
        // ID 2 = non-independent (role 4), ID 1 = content word (role 0)
        let roles = vec![0u8, 0, 4];
        let conn = conn_with_roles(roles);

        // Path A: こと (hiragana, non-independent) — no penalty
        // Path B: 事 (kanji, non-independent) — penalty applied
        let mut paths = vec![
            path(vec![seg("こと", "事", 2)], 100),
            path(vec![seg("こと", "こと", 2)], 100),
        ];

        rerank(&mut paths, Some(&conn), None);

        // The hiragana path should rank higher (lower cost)
        assert_eq!(paths[0].segments[0].surface, "こと");
        assert_eq!(paths[1].segments[0].surface, "事");
        assert!(paths[0].viterbi_cost < paths[1].viterbi_cost);
    }

    /// Build a minimal ConnectionMatrix with the given roles vector and
    /// function-word ID range.
    fn conn_with_roles_and_fw(roles: Vec<u8>, fw_min: u16, fw_max: u16) -> ConnectionMatrix {
        let num_ids = roles.len() as u16;
        let costs = vec![0i16; num_ids as usize * num_ids as usize];
        ConnectionMatrix::new_owned(num_ids, fw_min, fw_max, roles, costs)
    }

    #[test]
    fn non_independent_kanji_penalty_not_applied_to_content_words() {
        // ID 1 = content word (role 0)
        let roles = vec![0u8, 0];
        let conn = conn_with_roles(roles);

        // Both paths use content word IDs — no non-independent penalty
        let mut paths = vec![
            path(vec![seg("こと", "事", 1)], 100),
            path(vec![seg("こと", "こと", 1)], 100),
        ];

        rerank(&mut paths, Some(&conn), None);

        // Costs should differ only by script cost, not by non-independent penalty
        let penalty = settings().reranker.non_independent_kanji_penalty;
        let cost_diff = (paths[1].viterbi_cost - paths[0].viterbi_cost).abs();
        assert!(
            cost_diff < penalty,
            "no non-independent penalty should be applied: diff = {}",
            cost_diff
        );
    }

    #[test]
    fn te_form_kanji_penalty_applied() {
        // ID 2 = function word (fw_min=2, fw_max=2), ID 1 = content word
        let conn = conn_with_roles_and_fw(vec![0u8, 0, 0], 2, 2);

        // Path A: で + 見る (kanji after て/で FW) — penalty applied
        // Path B: で + みる (hiragana after て/で FW) — no penalty
        let mut paths = vec![
            path(vec![seg("で", "で", 2), seg("みる", "見る", 1)], 100),
            path(vec![seg("で", "で", 2), seg("みる", "みる", 1)], 100),
        ];

        rerank(&mut paths, Some(&conn), None);

        assert_eq!(paths[0].segments[1].surface, "みる");
        assert_eq!(paths[1].segments[1].surface, "見る");
        assert!(paths[0].viterbi_cost < paths[1].viterbi_cost);
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

    #[test]
    fn pronoun_bonus_applied() {
        // ID 2 = pronoun (role 5), ID 1 = content word (role 0)
        let roles = vec![0u8, 0, 5];
        let conn = conn_with_roles(roles);

        // Both paths have the same surface (hiragana) to isolate pronoun bonus.
        // Path A: pronoun POS (id=2) — bonus applied
        // Path B: content word POS (id=1) — no bonus
        let mut paths = vec![
            path(vec![seg("どれ", "どれ", 2)], 1000),
            path(vec![seg("どれ", "どれ", 1)], 1000),
        ];

        rerank(&mut paths, Some(&conn), None);

        // The pronoun path should rank higher (lower cost) after bonus
        assert_eq!(
            paths[0].segments[0].left_id, 2,
            "pronoun path should rank first"
        );
        let bonus = settings().reranker.pronoun_cost_bonus;
        let diff = paths[1].viterbi_cost - paths[0].viterbi_cost;
        assert_eq!(
            diff, bonus,
            "cost difference should equal pronoun_cost_bonus"
        );
    }

    /// A minimal dictionary for testing compound exemption.
    struct MockDict {
        entries: Vec<(String, Vec<DictEntry>)>,
    }

    impl MockDict {
        fn new(readings: &[&str]) -> Self {
            Self {
                entries: readings
                    .iter()
                    .map(|&r| {
                        (
                            r.to_string(),
                            vec![DictEntry {
                                surface: r.to_string(),
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
            "single-char kanji noun should be penalized: root={}, other={}",
            root_path.viterbi_cost,
            other_path.viterbi_cost
        );
    }

    #[test]
    fn single_char_kanji_penalty_exempt_with_dict_compound() {
        // ID 1 = content word (role 0)
        let roles = vec![0u8, 0];
        let conn = conn_with_roles(roles);

        // Dictionary has "きょうと" compound reading
        let dict = MockDict::new(&["きょうと"]);

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
    fn single_char_kanji_penalty_not_applied_to_function_word() {
        // ID 1 = content word (role 0), ID 2 = function word
        let conn = conn_with_roles_and_fw(vec![0u8, 0, 0], 2, 2);

        // "は" (hiragana) with FW POS — no penalty even if it were kanji
        // "ね" (hiragana) with CW POS — would get penalty if kanji
        let mut paths = vec![
            path(vec![seg("ね", "根", 1)], 100), // CW, kanji → penalty
            path(vec![seg("ね", "ね", 2)], 100), // FW → no penalty
        ];

        rerank(&mut paths, Some(&conn), None);

        let fw_path = paths
            .iter()
            .find(|p| p.segments[0].surface == "ね")
            .unwrap();
        let cw_path = paths
            .iter()
            .find(|p| p.segments[0].surface == "根")
            .unwrap();
        assert!(
            cw_path.viterbi_cost > fw_path.viterbi_cost,
            "function word should not get single-char kanji penalty"
        );
    }

    #[test]
    fn single_char_kanji_penalty_not_applied_to_multi_char_reading() {
        // ID 1 = content word (role 0)
        let roles = vec![0u8, 0];
        let conn = conn_with_roles(roles);

        // "ねこ" → "猫" has 2-char reading, should not get penalty
        let mut paths = vec![
            path(vec![seg("ねこ", "猫", 1)], 100),
            path(vec![seg("ねこ", "猫", 1)], 100),
        ];

        let cost_before = paths[0].viterbi_cost;
        rerank(&mut paths, Some(&conn), None);

        // Cost should not include single_char_kanji_penalty
        let penalty = settings().reranker.single_char_kanji_penalty;
        assert!(
            paths[0].viterbi_cost - cost_before < penalty,
            "multi-char reading should not get single-char kanji penalty"
        );
    }

    #[test]
    fn te_form_kanji_penalty_not_applied_to_non_te_function_word() {
        // ID 2 = function word (fw_min=2, fw_max=2), ID 1 = content word
        let conn = conn_with_roles_and_fw(vec![0u8, 0, 0], 2, 2);

        // "は" is a function word but not て/で — no te-form penalty
        let mut paths = vec![
            path(vec![seg("は", "は", 2), seg("みる", "見る", 1)], 100),
            path(vec![seg("は", "は", 2), seg("みる", "みる", 1)], 100),
        ];

        rerank(&mut paths, Some(&conn), None);

        let te_penalty = settings().reranker.te_form_kanji_penalty;
        let cost_diff = (paths[1].viterbi_cost - paths[0].viterbi_cost).abs();
        assert!(
            cost_diff < te_penalty,
            "no te-form penalty should be applied for non-te FW: diff = {}",
            cost_diff
        );
    }
}
