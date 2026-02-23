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
        let n = path.segments.len();
        if n >= 3 {
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
            path.viterbi_cost +=
                sum_sq_dev * settings().reranker.length_variance_weight / (n_i64 * n_i64);
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
}
