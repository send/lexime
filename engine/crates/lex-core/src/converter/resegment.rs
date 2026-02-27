//! Re-segmentation: generate alternative segment boundaries from the lattice.
//!
//! Viterbi N-best tends to converge on similar segmentations. This module
//! explores 2-way splits of each segment in the best path, using lattice
//! nodes that naturally bridge the split point. Only splits where at least
//! one part is a function word are considered, as these are the most
//! common source of missed boundaries (e.g. 教派 → 今日+は).

use std::collections::HashSet;

use crate::dict::connection::ConnectionMatrix;
use crate::settings::settings;

use super::cost::conn_cost;
use super::lattice::Lattice;
use super::viterbi::{RichSegment, ScoredPath};

/// Maximum number of resegmented paths to add.
const MAX_RESEG_PATHS: usize = 10;

/// Generate alternative paths by re-segmenting the best path's segments.
///
/// For each segment in `paths[0]`, tries splitting it at every internal
/// character boundary using nodes present in the lattice. Only splits
/// where at least one half is a function word are kept.
pub(super) fn resegment(
    paths: &[ScoredPath],
    lattice: &Lattice,
    conn: Option<&ConnectionMatrix>,
) -> Vec<ScoredPath> {
    let best = match paths.first() {
        Some(p) if !p.segments.is_empty() => p,
        _ => return Vec::new(),
    };

    // Collect existing surface keys for dedup
    let existing_keys: HashSet<String> = paths.iter().map(|p| p.surface_key()).collect();

    // Build char-position boundaries of the best path's segments
    let mut seg_boundaries: Vec<(usize, usize)> = Vec::new();
    let mut pos = 0;
    for seg in &best.segments {
        let len = seg.reading.chars().count();
        seg_boundaries.push((pos, pos + len));
        pos += len;
    }

    let mut new_paths: Vec<ScoredPath> = Vec::new();
    let mut new_keys: HashSet<String> = HashSet::new();

    for (seg_idx, &(seg_start, seg_end)) in seg_boundaries.iter().enumerate() {
        if seg_end - seg_start < 2 {
            continue; // single-char segment, no internal split possible
        }

        // Try every internal split point
        for mid in (seg_start + 1)..seg_end {
            // Find lattice nodes for left part: start=seg_start, end=mid
            let left_nodes: Vec<usize> = lattice
                .nodes_by_start
                .get(seg_start)
                .map(|indices| {
                    indices
                        .iter()
                        .copied()
                        .filter(|&idx| lattice.nodes[idx].end == mid)
                        .collect()
                })
                .unwrap_or_default();

            // Find lattice nodes for right part: start=mid, end=seg_end
            let right_nodes: Vec<usize> = lattice
                .nodes_by_start
                .get(mid)
                .map(|indices| {
                    indices
                        .iter()
                        .copied()
                        .filter(|&idx| lattice.nodes[idx].end == seg_end)
                        .collect()
                })
                .unwrap_or_default();

            for &left_idx in &left_nodes {
                for &right_idx in &right_nodes {
                    let left_node = &lattice.nodes[left_idx];
                    let right_node = &lattice.nodes[right_idx];

                    // At least one part must be a function word
                    let left_is_fw = conn
                        .map(|c| c.is_function_word(left_node.left_id))
                        .unwrap_or(false);
                    let right_is_fw = conn
                        .map(|c| c.is_function_word(right_node.left_id))
                        .unwrap_or(false);
                    if !left_is_fw && !right_is_fw {
                        continue;
                    }

                    // Build alternative path: replace segment at seg_idx with left+right
                    let mut new_segs: Vec<RichSegment> =
                        Vec::with_capacity(best.segments.len() + 1);
                    new_segs.extend_from_slice(&best.segments[..seg_idx]);
                    new_segs.push(RichSegment::from(left_node));
                    new_segs.push(RichSegment::from(right_node));
                    new_segs.extend_from_slice(&best.segments[(seg_idx + 1)..]);

                    let cost = score_path(&new_segs, conn);

                    let candidate = ScoredPath {
                        segments: new_segs,
                        viterbi_cost: cost,
                    };

                    // Dedup against existing paths and already-generated candidates
                    let key = candidate.surface_key();
                    if existing_keys.contains(&key) || !new_keys.insert(key) {
                        continue;
                    }

                    new_paths.push(candidate);
                    if new_paths.len() >= MAX_RESEG_PATHS {
                        return new_paths;
                    }
                }
            }
        }
    }

    new_paths
}

/// Score a path using the same formula as `DefaultCostFunction`.
///
/// Reproduces: word_cost(node) + BOS + transitions + EOS.
fn score_path(segments: &[RichSegment], conn: Option<&ConnectionMatrix>) -> i64 {
    if segments.is_empty() {
        return 0;
    }

    let seg_penalty = settings().cost.segment_penalty;
    let mut cost: i64 = 0;

    for (i, seg) in segments.iter().enumerate() {
        let is_fw = conn
            .map(|c| c.is_function_word(seg.left_id))
            .unwrap_or(false);
        let penalty = if is_fw { seg_penalty / 2 } else { seg_penalty };
        cost += seg.word_cost as i64 + penalty;

        if i == 0 {
            // BOS transition
            cost += conn_cost(conn, 0, seg.left_id);
        } else {
            // Transition from previous segment
            cost += conn_cost(conn, segments[i - 1].right_id, seg.left_id);
        }
    }

    // EOS transition
    cost += conn_cost(conn, segments.last().unwrap().right_id, 0);

    cost
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::converter::cost::DefaultCostFunction;
    use crate::converter::lattice::build_lattice;
    use crate::converter::testutil::{test_dict, zero_conn_with_fw};
    use crate::converter::viterbi::viterbi_nbest;
    use crate::dict::{DictEntry, TrieDictionary};

    /// Helper: build lattice + viterbi paths for a kana string.
    fn build_paths(
        dict: &dyn crate::dict::Dictionary,
        kana: &str,
        conn: Option<&ConnectionMatrix>,
        n: usize,
    ) -> (Lattice, Vec<ScoredPath>) {
        let lattice = build_lattice(dict, kana);
        let cost_fn = DefaultCostFunction::new(conn);
        let paths = viterbi_nbest(&lattice, &cost_fn, n);
        (lattice, paths)
    }

    /// Test dictionary that includes "きょうは"→教派 as a compound entry.
    ///
    /// With conn (FW half-penalty), 教派 (cost 4000 + seg_penalty) is cheaper
    /// than 今日+は (3000+seg + 2000+seg/2), so Viterbi picks 教派 as a single
    /// segment. Resegment should then split it into 今日+は.
    fn dict_with_compound() -> TrieDictionary {
        let entries = vec![
            (
                "きょう".to_string(),
                vec![DictEntry {
                    surface: "今日".to_string(),
                    cost: 3000,
                    left_id: 100,
                    right_id: 100,
                }],
            ),
            (
                "きょうは".to_string(),
                vec![DictEntry {
                    surface: "教派".to_string(),
                    cost: 4000,
                    left_id: 102,
                    right_id: 102,
                }],
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
        ];
        TrieDictionary::from_entries(entries)
    }

    #[test]
    fn test_resegment_splits_compound_with_fw() {
        // dict_with_compound has "きょうは"→教派 (compound) plus "きょう"→今日 + "は"→は(FW).
        // With FW half-penalty, Viterbi picks 教派 as one segment; resegment
        // should split it into 今日+は.
        let conn = zero_conn_with_fw(1200, 200, 200);
        let dict = dict_with_compound();
        // n=1: only the best Viterbi path (教派) so the 今日+は split is novel
        let (lattice, paths) = build_paths(&dict, "きょうはいいてんき", Some(&conn), 1);

        // Verify best path actually contains 教派 as a single segment
        assert!(
            paths[0].segments.iter().any(|s| s.surface == "教派"),
            "Viterbi best should contain 教派 compound for this test to be meaningful"
        );

        let new_paths = resegment(&paths, &lattice, Some(&conn));

        // Must produce at least one resegmented candidate
        assert!(
            !new_paths.is_empty(),
            "resegment should produce at least one alternative path"
        );

        // No duplicates with existing Viterbi paths
        let existing_keys: HashSet<String> = paths.iter().map(|p| p.surface_key()).collect();
        for p in &new_paths {
            assert!(
                !existing_keys.contains(&p.surface_key()),
                "resegmented path should not duplicate existing: {}",
                p.surface_key()
            );
        }

        // At least one resegmented path should contain 今日+は split
        let has_kyou_ha = new_paths.iter().any(|p| {
            p.segments
                .windows(2)
                .any(|w| w[0].surface == "今日" && w[1].surface == "は")
        });
        assert!(
            has_kyou_ha,
            "resegment should produce a path with 今日+は split"
        );
    }

    #[test]
    fn test_resegment_no_split_without_fw() {
        // With no function words defined (fw_min=0, fw_max=0), no splits should occur
        let conn = zero_conn_with_fw(1200, 0, 0);
        let dict = dict_with_compound();
        let (lattice, paths) = build_paths(&dict, "きょうはいいてんき", Some(&conn), 5);

        let new_paths = resegment(&paths, &lattice, Some(&conn));
        assert!(
            new_paths.is_empty(),
            "no splits should be generated without FW: got {} paths",
            new_paths.len()
        );
    }

    #[test]
    fn test_resegment_dedup_existing() {
        let conn = zero_conn_with_fw(1200, 200, 200);
        let dict = dict_with_compound();
        let (lattice, paths) = build_paths(&dict, "きょうはいいてんき", Some(&conn), 20);

        let new_paths = resegment(&paths, &lattice, Some(&conn));

        let existing_keys: HashSet<String> = paths.iter().map(|p| p.surface_key()).collect();
        for p in &new_paths {
            assert!(
                !existing_keys.contains(&p.surface_key()),
                "should not duplicate existing path: {}",
                p.surface_key()
            );
        }
    }

    #[test]
    fn test_resegment_empty_paths() {
        let conn = zero_conn_with_fw(1200, 200, 200);
        let dict = test_dict();
        let lattice = build_lattice(&dict, "きょう");
        let paths: Vec<ScoredPath> = Vec::new();

        let new_paths = resegment(&paths, &lattice, Some(&conn));
        assert!(new_paths.is_empty());
    }

    #[test]
    fn test_score_path_matches_viterbi() {
        // For a path that Viterbi also produces, score_path should match.
        let conn = zero_conn_with_fw(1200, 200, 200);
        let dict = test_dict();
        let (_, paths) = build_paths(&dict, "きょうはいいてんき", Some(&conn), 5);

        if let Some(best) = paths.first() {
            let rescored = score_path(&best.segments, Some(&conn));
            assert_eq!(
                rescored, best.viterbi_cost,
                "score_path ({}) should match viterbi_cost ({})",
                rescored, best.viterbi_cost
            );
        }
    }
}
