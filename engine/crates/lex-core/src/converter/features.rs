//! Path feature extraction for reranking.
//!
//! Each feature is a raw numeric value computed from a ScoredPath.
//! The reranker applies weights to produce the final cost adjustment.

use crate::dict::connection::ConnectionMatrix;
use crate::dict::Dictionary;
use crate::unicode::is_kanji;

use super::cost::script_cost;
use super::viterbi::{RichSegment, ScoredPath};

/// Raw features extracted from a single conversion path.
/// All values are in "raw" units — weights convert them to cost adjustments.
#[derive(Debug, Clone, Default)]
pub struct PathFeatures {
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
        let variance = if self.length_variance_n >= 2 {
            self.length_variance_raw * w.length_variance
                / (self.length_variance_n * self.length_variance_n)
        } else {
            0
        };
        let script = self.script_cost * w.script / 100;
        let te_kanji = self.te_kanji_count * w.te_kanji;
        let single_kanji = self.single_kanji_count * w.single_kanji;

        variance + script + te_kanji + single_kanji
    }
}

/// Weights for combining path features into a cost adjustment.
/// Each weight scales its corresponding feature.
#[derive(Debug, Clone)]
pub struct FeatureWeights {
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

/// Script weight is fixed at 100 (= 100% of raw script cost, no scaling).
const SCRIPT_WEIGHT: i64 = 100;

impl Default for FeatureWeights {
    fn default() -> Self {
        use crate::settings::{
            DEFAULT_LENGTH_VARIANCE_WEIGHT, DEFAULT_SINGLE_CHAR_KANJI_PENALTY,
            DEFAULT_TE_FORM_KANJI_PENALTY,
        };
        Self {
            length_variance: DEFAULT_LENGTH_VARIANCE_WEIGHT,
            script: SCRIPT_WEIGHT,
            te_kanji: DEFAULT_TE_FORM_KANJI_PENALTY,
            single_kanji: DEFAULT_SINGLE_CHAR_KANJI_PENALTY,
        }
    }
}

impl FeatureWeights {
    /// Build weights from settings + fixed constants.
    ///
    /// `script` is a compile-time constant (not in settings) because grid
    /// search showed no benefit from varying it.
    pub fn from_settings() -> Self {
        let s = crate::settings::settings();
        Self {
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

/// Configuration for feature extraction shared across paths.
pub struct FeatureConfig<'a> {
    pub conn: Option<&'a ConnectionMatrix>,
    pub dict: Option<&'a dyn Dictionary>,
}

impl FeatureConfig<'_> {
    /// Extract features from a path.
    pub fn extract(&self, path: &ScoredPath) -> PathFeatures {
        let mut f = PathFeatures::default();

        // Length variance (excluding FW and single-char segments)
        if path.segments.len() >= 3 {
            let lengths: Vec<i64> = path
                .segments
                .iter()
                .filter_map(|s| {
                    let len = s.reading.chars().count() as i64;
                    if len > 1 && !self.conn.is_some_and(|c| c.is_function_word(s.left_id)) {
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
        if let Some(c) = self.conn {
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
                if is_single_char_kanji_penalised(seg, i, &path.segments, c, self.dict) {
                    f.single_kanji_count += 1;
                }
            }
        }

        f
    }
}
