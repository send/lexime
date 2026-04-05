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
    /// Default: 25 (= 25% of raw structure cost).
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

impl Default for FeatureWeights {
    fn default() -> Self {
        Self {
            structure: 0,
            length_variance: 1000,
            script: 100,
            te_kanji: 2000,
            single_kanji: 0,
        }
    }
}

impl FeatureWeights {
    /// Load weights from the current settings.
    pub fn from_settings() -> Self {
        let s = crate::settings::settings();
        Self {
            structure: 0,
            length_variance: s.reranker.length_variance_weight,
            script: 100,
            te_kanji: s.reranker.te_form_kanji_penalty,
            single_kanji: s.reranker.single_char_kanji_penalty,
        }
    }
}

/// Extract features from a path.
pub fn extract_features(
    path: &ScoredPath,
    conn: Option<&ConnectionMatrix>,
    dict: Option<&dyn Dictionary>,
    structure_cap: i64,
    prefix_floor: i64,
) -> PathFeatures {
    let mut f = PathFeatures::default();

    // Structure cost
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
        f.structure_cost += tc.min(structure_cap);
    }

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
            if i > 0 {
                let prev = &path.segments[i - 1];
                if c.is_function_word(prev.left_id)
                    && (prev.surface == "て" || prev.surface == "で")
                    && seg.surface.chars().any(is_kanji)
                {
                    f.te_kanji_count += 1;
                }
            }
            // Single-char kanji content word
            if seg.reading.chars().count() == 1
                && seg.surface.chars().any(is_kanji)
                && c.role(seg.left_id) == 0
            {
                let exempt = dict.is_some_and(|d| {
                    let has_compound = |a: &RichSegment, b: &RichSegment| -> bool {
                        let reading = format!("{}{}", a.reading, b.reading);
                        let surface = format!("{}{}", a.surface, b.surface);
                        d.lookup(&reading).iter().any(|e| e.surface == surface)
                    };
                    (i > 0 && has_compound(&path.segments[i - 1], seg))
                        || (i + 1 < path.segments.len() && has_compound(seg, &path.segments[i + 1]))
                });
                if !exempt {
                    f.single_kanji_count += 1;
                }
            }
        }
    }

    f
}
