//! Path feature extraction for reranking.
//!
//! Each feature is a raw numeric value computed from a ScoredPath.
//! The reranker applies weights to produce the final cost adjustment.

use crate::dict::connection::ConnectionMatrix;
use crate::dict::Dictionary;
use crate::unicode::is_kanji;

use super::cost::{conn_cost, script_cost};
use super::viterbi::{RichSegment, ScoredPath};

/// Raw features extracted from a single conversion path.
/// All values are in "raw" units — weights convert them to cost adjustments.
#[derive(Debug, Clone, Default)]
pub struct PathFeatures {
    /// Sum of capped transition costs along the path.
    /// Higher = more fragmented / unnatural transitions.
    pub structure_cost: i64,
    /// Length variance of content-word segments (integer-scaled).
    /// Computed as N × Σlen² - (Σlen)², where N and len exclude FW and
    /// single-char segments. 0 means uniform. Divide by N² for true variance.
    pub length_variance_raw: i64,
    /// Number of content-word segments used in variance calculation.
    pub length_variance_n: i64,
    /// Total script cost (sum of per-segment script preferences).
    /// Negative = good (kanji+kana), positive = bad (katakana/latin).
    pub script_cost: i64,
    /// Number of te-form kanji penalties triggered.
    pub te_kanji_count: i64,
    /// Number of single-char kanji content-word penalties triggered.
    pub single_kanji_count: i64,
}

impl PathFeatures {
    /// Apply weights to produce a single cost adjustment.
    pub fn weighted_cost(&self, w: &FeatureWeights) -> i64 {
        let structure = self.structure_cost * w.structure / 100;
        let variance = if self.length_variance_n >= 2 {
            self.length_variance_raw * w.length_variance
                / (self.length_variance_n * self.length_variance_n)
        } else {
            0
        };
        let script = self.script_cost * w.script / 100;
        let te_kanji = self.te_kanji_count * w.te_kanji;
        let single_kanji = self.single_kanji_count * w.single_kanji;

        structure + variance + script + te_kanji + single_kanji
    }
}

/// Weights for combining path features into a cost adjustment.
/// Each weight scales its corresponding feature.
#[derive(Debug, Clone)]
pub struct FeatureWeights {
    /// Structure cost weight (applied as structure_cost * weight / 100).
    pub structure: i64,
    /// Length variance weight (applied as variance_raw * weight / N²).
    pub length_variance: i64,
    /// Script cost weight (applied as script_cost * weight / 100).
    /// Default: 100 (= 100% of raw script cost, i.e. no change).
    pub script: i64,
    /// Per-occurrence te-form kanji penalty.
    pub te_kanji: i64,
    /// Per-occurrence single-char kanji penalty.
    pub single_kanji: i64,
}

/// Structure weight is fixed at 0 (grid search found structure_cost
/// adds noise rather than signal at the reranker stage).
const STRUCTURE_WEIGHT: i64 = 0;
/// Script weight is fixed at 100 (= 100% of raw script cost, no scaling).
const SCRIPT_WEIGHT: i64 = 100;

impl Default for FeatureWeights {
    fn default() -> Self {
        Self {
            structure: STRUCTURE_WEIGHT,
            length_variance: 1000,
            script: SCRIPT_WEIGHT,
            te_kanji: 2000,
            single_kanji: 0,
        }
    }
}

impl FeatureWeights {
    /// Build weights from settings + fixed constants.
    ///
    /// `structure` and `script` are compile-time constants (not in settings)
    /// because grid search showed no benefit from varying them.
    pub fn from_settings() -> Self {
        let s = crate::settings::settings();
        Self {
            structure: STRUCTURE_WEIGHT,
            length_variance: s.reranker.length_variance_weight,
            script: SCRIPT_WEIGHT,
            te_kanji: s.reranker.te_form_kanji_penalty,
            single_kanji: s.reranker.single_char_kanji_penalty,
        }
    }
}

/// Check whether a segment triggers the te-form kanji penalty.
///
/// Returns `true` when the segment contains kanji and follows a て/で
/// function word.
pub fn is_te_form_kanji_penalised(
    seg: &RichSegment,
    prev: Option<&RichSegment>,
    conn: &ConnectionMatrix,
) -> bool {
    if let Some(prev) = prev {
        conn.is_function_word(prev.left_id)
            && (prev.surface == "て" || prev.surface == "で")
            && seg.surface.chars().any(is_kanji)
    } else {
        false
    }
}

/// Check whether a segment triggers the single-char kanji content-word penalty.
///
/// Returns `true` when the segment is a single-character kanji content word
/// that is NOT part of a dictionary compound with its neighbours.
pub fn is_single_char_kanji_penalised(
    seg: &RichSegment,
    idx: usize,
    segments: &[RichSegment],
    conn: &ConnectionMatrix,
    dict: Option<&dyn Dictionary>,
) -> bool {
    if seg.reading.chars().count() != 1
        || !seg.surface.chars().any(is_kanji)
        || conn.role(seg.left_id) != 0
    {
        return false;
    }
    let exempt = dict.is_some_and(|d| {
        let has_compound = |a: &RichSegment, b: &RichSegment| -> bool {
            let reading = format!("{}{}", a.reading, b.reading);
            let surface = format!("{}{}", a.surface, b.surface);
            d.lookup(&reading).iter().any(|e| e.surface == surface)
        };
        (idx > 0 && has_compound(&segments[idx - 1], seg))
            || (idx + 1 < segments.len() && has_compound(seg, &segments[idx + 1]))
    });
    !exempt
}

/// Compute capped structure cost for a path (sum of capped transition costs).
///
/// This is used by the reranker hard filter and can be passed into
/// `extract_features` via `precomputed_structure_cost` to avoid recomputation.
pub fn compute_structure_cost(
    path: &ScoredPath,
    conn: Option<&ConnectionMatrix>,
    cap: i64,
    prefix_floor: i64,
) -> i64 {
    let mut sc: i64 = 0;
    for i in 1..path.segments.len() {
        let mut tc = conn_cost(
            conn,
            path.segments[i - 1].right_id,
            path.segments[i].left_id,
        );
        if let Some(c) = conn {
            if c.is_prefix(path.segments[i - 1].right_id) {
                tc = tc.max(prefix_floor);
            }
        }
        sc += tc.min(cap);
    }
    sc
}

/// Extract features from a path.
///
/// If `precomputed_structure_cost` is `Some`, reuses the value instead of
/// recomputing the transition-cost sum (an optimisation for the reranker
/// which already computes it for the hard filter).
pub fn extract_features(
    path: &ScoredPath,
    conn: Option<&ConnectionMatrix>,
    dict: Option<&dyn Dictionary>,
    structure_cap: i64,
    prefix_floor: i64,
    precomputed_structure_cost: Option<i64>,
) -> PathFeatures {
    let mut f = PathFeatures {
        structure_cost: precomputed_structure_cost
            .unwrap_or_else(|| compute_structure_cost(path, conn, structure_cap, prefix_floor)),
        ..Default::default()
    };

    // Length variance (excluding FW and single-char segments)
    if path.segments.len() >= 3 {
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
        let n = lengths.len() as i64;
        if n >= 2 {
            let sum: i64 = lengths.iter().sum();
            let sum_sq: i64 = lengths.iter().map(|l| l * l).sum();
            f.length_variance_raw = n * sum_sq - sum * sum;
            f.length_variance_n = n;
        }
    }

    // Script cost
    f.script_cost = path
        .segments
        .iter()
        .map(|s| script_cost(&s.surface, s.reading.chars().count()))
        .sum();

    // Per-segment features
    if let Some(c) = conn {
        for (i, seg) in path.segments.iter().enumerate() {
            // Te-form kanji
            let prev = if i > 0 {
                Some(&path.segments[i - 1])
            } else {
                None
            };
            if is_te_form_kanji_penalised(seg, prev, c) {
                f.te_kanji_count += 1;
            }
            // Single-char kanji content word
            if is_single_char_kanji_penalised(seg, i, &path.segments, c, dict) {
                f.single_kanji_count += 1;
            }
        }
    }

    f
}
