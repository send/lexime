use tracing::{debug, debug_span};

use crate::dict::connection::ConnectionMatrix;
use crate::settings::settings;
use crate::unicode::is_kanji;
use crate::user_history::UserHistory;

use super::cost::{conn_cost, script_cost};
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
pub fn rerank(paths: &mut Vec<ScoredPath>, conn: Option<&ConnectionMatrix>) {
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
                .filter(|s| {
                    s.reading.chars().count() > 1
                        && !conn.is_some_and(|c| c.is_function_word(s.left_id))
                })
                .map(|s| s.reading.chars().count() as i64)
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
        let total_script: i64 = path.segments.iter().map(|s| script_cost(&s.surface)).sum();
        path.viterbi_cost += total_script;

        // Non-independent kanji penalty: penalise kanji surfaces for 非自立
        // morphemes (形式名詞 like 事/物/所, 補助動詞 like 下さい/頂く).
        if let Some(conn) = conn {
            let penalty = settings().reranker.non_independent_kanji_penalty;
            for seg in &path.segments {
                if conn.is_non_independent(seg.left_id) && seg.surface.chars().any(is_kanji) {
                    path.viterbi_cost += penalty;
                }
            }
        }

        // Te-form kanji penalty: penalise kanji surfaces following て/で
        // conjunctive particles to prefer hiragana auxiliary verbs
        // (e.g., 読んでみる over 読んで見る).
        if let Some(conn) = conn {
            let te_penalty = settings().reranker.te_form_kanji_penalty;
            for pair in path.segments.windows(2) {
                let prev = &pair[0];
                let curr = &pair[1];
                if conn.is_function_word(prev.left_id)
                    && (prev.surface == "て" || prev.surface == "で")
                    && curr.surface.chars().any(is_kanji)
                {
                    path.viterbi_cost += te_penalty;
                }
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

        rerank(&mut paths, Some(&conn));

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

        rerank(&mut paths, Some(&conn));

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

        rerank(&mut paths, Some(&conn));

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

        rerank(&mut paths, Some(&conn));

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
        //   Path B: [4, 1(CW), 3]      → exclude 1-char CW  → [4, 3] → small variance
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

        rerank(&mut paths, Some(&conn));

        // Path A should rank better: its 1-char segments are all excluded,
        // leaving no variance. Path B has [4, 2] with nonzero variance.
        assert_eq!(
            paths[0].segments[2].surface, "て",
            "path with only 1-char non-FW should rank first (no variance penalty)"
        );
        assert!(paths[0].viterbi_cost < paths[1].viterbi_cost);
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

        rerank(&mut paths, Some(&conn));

        let te_penalty = settings().reranker.te_form_kanji_penalty;
        let cost_diff = (paths[1].viterbi_cost - paths[0].viterbi_cost).abs();
        assert!(
            cost_diff < te_penalty,
            "no te-form penalty should be applied for non-te FW: diff = {}",
            cost_diff
        );
    }
}
