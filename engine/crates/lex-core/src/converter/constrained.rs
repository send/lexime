//! Prefix-constrained Viterbi search for speculative decoding.
//!
//! When speculative decoding confirms the first K segments of a conversion,
//! this module constrains the lattice so that those segments are fixed,
//! allowing re-exploration only of the suffix.

use std::ops::Range;

use super::cost::{CostFunction, DefaultCostFunction};
use super::lattice::Lattice;
use super::viterbi::ConvertedSegment;

/// Confirmed prefix constraint for constrained Viterbi.
///
/// Segments within the prefix are matched by (char_range, reading, surface).
/// Nodes that contradict the fixed prefix receive a prohibitive cost.
pub(crate) struct PrefixConstraint {
    /// Fixed segments: (char_range, reading, surface)
    segments: Vec<(Range<usize>, String, String)>,
    /// Total character length of the prefix
    prefix_char_end: usize,
}

impl PrefixConstraint {
    /// Build a constraint from the first `n` confirmed segments.
    pub fn from_confirmed(confirmed: &[ConvertedSegment]) -> Self {
        let mut segments = Vec::with_capacity(confirmed.len());
        let mut pos = 0;
        for seg in confirmed {
            let char_len = seg.reading.chars().count();
            let end = pos + char_len;
            segments.push((pos..end, seg.reading.clone(), seg.surface.clone()));
            pos = end;
        }
        Self {
            prefix_char_end: pos,
            segments,
        }
    }

    /// Check if a lattice node is within the prefix region.
    fn is_in_prefix(&self, lattice: &Lattice, idx: usize) -> bool {
        lattice.start(idx) < self.prefix_char_end
    }

    /// Check if a lattice node spans the prefix boundary.
    fn spans_boundary(&self, lattice: &Lattice, idx: usize) -> bool {
        lattice.start(idx) < self.prefix_char_end && lattice.end(idx) > self.prefix_char_end
    }

    /// Check if a lattice node matches a fixed segment exactly.
    fn matches_fixed_segment(&self, lattice: &Lattice, idx: usize) -> bool {
        self.segments.iter().any(|(pos, reading, surface)| {
            lattice.start(idx) == pos.start
                && lattice.end(idx) == pos.end
                && lattice.reading(idx) == *reading
                && lattice.surface(idx) == *surface
        })
    }
}

/// Cost that prohibits nodes contradicting a prefix constraint.
const CONSTRAINT_VIOLATION_COST: i64 = i64::MAX / 4;

/// Cost function wrapper that enforces prefix constraints.
///
/// - Nodes within the prefix that match a fixed segment: normal cost
/// - Nodes within the prefix that don't match: CONSTRAINT_VIOLATION_COST
/// - Nodes that span the prefix boundary: CONSTRAINT_VIOLATION_COST
/// - Nodes after the prefix: normal cost
pub(crate) struct PrefixConstrainedCost<'a> {
    inner: DefaultCostFunction<'a>,
    constraint: &'a PrefixConstraint,
}

impl<'a> PrefixConstrainedCost<'a> {
    pub fn new(
        conn: Option<&'a crate::dict::connection::ConnectionMatrix>,
        constraint: &'a PrefixConstraint,
    ) -> Self {
        Self {
            inner: DefaultCostFunction::new(conn),
            constraint,
        }
    }
}

impl CostFunction for PrefixConstrainedCost<'_> {
    fn word_cost(&self, lattice: &Lattice, idx: usize) -> i64 {
        if self.constraint.spans_boundary(lattice, idx) {
            return CONSTRAINT_VIOLATION_COST;
        }
        if self.constraint.is_in_prefix(lattice, idx) {
            if self.constraint.matches_fixed_segment(lattice, idx) {
                self.inner.word_cost(lattice, idx)
            } else {
                CONSTRAINT_VIOLATION_COST
            }
        } else {
            self.inner.word_cost(lattice, idx)
        }
    }

    fn transition_cost(&self, prev_right_id: u16, next_left_id: u16) -> i64 {
        self.inner.transition_cost(prev_right_id, next_left_id)
    }

    fn bos_cost(&self, left_id: u16) -> i64 {
        self.inner.bos_cost(left_id)
    }

    fn eos_cost(&self, right_id: u16) -> i64 {
        self.inner.eos_cost(right_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::converter::testutil::test_dict;
    use crate::converter::viterbi::{viterbi_nbest, ScoredPath};
    use crate::converter::{build_lattice, convert_nbest};

    fn to_segments(path: &ScoredPath) -> Vec<ConvertedSegment> {
        path.segments
            .iter()
            .map(|s| ConvertedSegment {
                reading: s.reading.clone(),
                surface: s.surface.clone(),
            })
            .collect()
    }

    #[test]
    fn test_empty_constraint_matches_unconstrained() {
        let dict = test_dict();
        let kana = "きょうは";

        // Unconstrained
        let unconstrained = convert_nbest(&dict, None, kana, 5);

        // Empty constraint (no confirmed segments)
        let constraint = PrefixConstraint::from_confirmed(&[]);
        let cost_fn = PrefixConstrainedCost::new(None, &constraint);
        let lattice = build_lattice(&dict, kana);
        let constrained = viterbi_nbest(&lattice, &cost_fn, 15);

        // First result should match
        assert_eq!(
            unconstrained[0]
                .iter()
                .map(|s| &s.surface)
                .collect::<Vec<_>>(),
            constrained[0]
                .segments
                .iter()
                .map(|s| &s.surface)
                .collect::<Vec<_>>(),
        );
    }

    #[test]
    fn test_full_constraint_returns_original() {
        let dict = test_dict();
        let kana = "きょうは";

        // Get raw 1-best (no grouping)
        let cost_fn = crate::converter::cost::DefaultCostFunction::new(None);
        let lattice = build_lattice(&dict, kana);
        let raw_paths = viterbi_nbest(&lattice, &cost_fn, 1);
        assert!(!raw_paths.is_empty());
        let first_raw = to_segments(&raw_paths[0]);

        // Constrain all segments
        let constraint = PrefixConstraint::from_confirmed(&first_raw);
        let constrained_cost = PrefixConstrainedCost::new(None, &constraint);
        let lattice2 = build_lattice(&dict, kana);
        let constrained = viterbi_nbest(&lattice2, &constrained_cost, 5);

        // First result should have the same segments as the constrained prefix
        assert!(!constrained.is_empty());
        let result_surfaces: Vec<&str> = constrained[0]
            .segments
            .iter()
            .map(|s| s.surface.as_str())
            .collect();
        let expected_surfaces: Vec<&str> = first_raw.iter().map(|s| s.surface.as_str()).collect();
        assert_eq!(result_surfaces, expected_surfaces);
    }

    #[test]
    fn test_partial_constraint_fixes_prefix() {
        let dict = test_dict();
        let kana = "きょうはいいてんき";

        // Get raw 1-best (no grouping) to use as constraint source
        let cost_fn = crate::converter::cost::DefaultCostFunction::new(None);
        let lattice = build_lattice(&dict, kana);
        let raw_paths = viterbi_nbest(&lattice, &cost_fn, 5);
        assert!(!raw_paths.is_empty());
        let first_raw = &raw_paths[0];

        // Constrain first 2 segments
        let confirmed: Vec<ConvertedSegment> = first_raw
            .segments
            .iter()
            .take(2)
            .map(|s| ConvertedSegment {
                reading: s.reading.clone(),
                surface: s.surface.clone(),
            })
            .collect();
        let constraint = PrefixConstraint::from_confirmed(&confirmed);
        let cost_fn = PrefixConstrainedCost::new(None, &constraint);
        let lattice2 = build_lattice(&dict, kana);
        let constrained = viterbi_nbest(&lattice2, &cost_fn, 10);

        // Valid results (non-violated paths) should have the prefix matching
        let prefix_char_len: usize = confirmed.iter().map(|s| s.reading.chars().count()).sum();

        // Only check paths with reasonable cost (not constraint-violated)
        let valid_paths: Vec<_> = constrained
            .iter()
            .filter(|p| p.viterbi_cost < CONSTRAINT_VIOLATION_COST / 2)
            .collect();
        assert!(
            !valid_paths.is_empty(),
            "should have at least one valid path"
        );

        for path in &valid_paths {
            let segs = to_segments(path);
            let mut chars = 0;
            let mut prefix_surfaces = Vec::new();
            for seg in &segs {
                if chars >= prefix_char_len {
                    break;
                }
                prefix_surfaces.push(seg.surface.as_str());
                chars += seg.reading.chars().count();
            }
            let result_prefix: String = prefix_surfaces.join("");
            let expected_prefix: String = confirmed.iter().map(|s| s.surface.as_str()).collect();
            assert_eq!(
                result_prefix, expected_prefix,
                "prefix segments should match constraint"
            );
        }
    }

    #[test]
    fn test_boundary_spanning_node_rejected() {
        // A node that starts in prefix and ends after should be rejected
        let constraint = PrefixConstraint {
            segments: vec![(0..2, "きょ".to_string(), "虚".to_string())],
            prefix_char_end: 2,
        };

        // Build a mini lattice with one boundary-spanning node
        let lattice = Lattice::from_test_nodes("きょう", &[(1, 3, "ょう", "陽", 1000, 0, 0)]);

        let cost_fn = PrefixConstrainedCost::new(None, &constraint);
        assert_eq!(cost_fn.word_cost(&lattice, 0), CONSTRAINT_VIOLATION_COST);
    }

    #[test]
    fn test_prefix_constraint_from_confirmed() {
        let confirmed = vec![
            ConvertedSegment {
                reading: "きょう".to_string(),
                surface: "今日".to_string(),
            },
            ConvertedSegment {
                reading: "は".to_string(),
                surface: "は".to_string(),
            },
        ];
        let constraint = PrefixConstraint::from_confirmed(&confirmed);

        assert_eq!(constraint.prefix_char_end, 4); // きょう(3) + は(1)
        assert_eq!(constraint.segments.len(), 2);
        assert_eq!(
            constraint.segments[0],
            (0..3, "きょう".to_string(), "今日".to_string())
        );
        assert_eq!(
            constraint.segments[1],
            (3..4, "は".to_string(), "は".to_string())
        );
    }
}
