//! Path feature extraction for reranking.
//!
//! Each feature is a raw numeric value computed from a ScoredPath.
//! The reranker applies weights to produce the final cost adjustment.

use crate::dict::connection::ConnectionMatrix;
use crate::dict::Dictionary;
use crate::unicode::is_kanji;

use super::cost::segment_script;
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
    /// Number of Latin units priced as plain Latin because they do not
    /// follow a typed digit (`cost::segment_script`'s fail-safe). Not a
    /// price term: a count that is 0 unless a path maker skipped the
    /// formation rule.
    pub latin_unit_unformed: i64,
}

impl PathFeatures {
    /// Apply weights to produce a single cost adjustment.
    pub fn weighted_cost(&self, w: &FeatureWeights) -> i64 {
        let variance = variance_cost(self.length_variance_raw, self.length_variance_n, w);
        let te_kanji = self.te_kanji_count * w.te_kanji;
        let single_kanji = self.single_kanji_count * w.single_kanji;

        variance + self.script_cost + te_kanji + single_kanji
    }
}

/// The path-level length-variance term for `(raw, n)` as in
/// [`PathFeatures`]: `raw * weight / n²`, 0 below two content words.
pub fn variance_cost(raw: i64, n: i64, w: &FeatureWeights) -> i64 {
    if n >= 2 {
        raw * w.length_variance / (n * n)
    } else {
        0
    }
}

/// The per-segment part of a path's features: what one segment adds to
/// [`PathFeatures`]'s script sum and penalty counts. The path's sums are
/// built from these, so a per-segment display and the path's price share one
/// computation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentFeatures {
    /// Raw script cost of the segment.
    pub script_cost: i64,
    /// The segment triggers the te-form kanji penalty (it follows て/で).
    pub te_kanji: bool,
    /// The segment triggers the single-char kanji penalty.
    pub single_kanji: bool,
    /// The segment is a Latin unit priced by the fail-safe (see
    /// [`PathFeatures::latin_unit_unformed`]).
    pub latin_unit_unformed: bool,
}

/// A segment's features with the weights applied: the amounts rerank adds
/// to the path for this segment.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SegmentRerank {
    pub script_cost: i64,
    pub te_form_kanji_penalty: i64,
    pub single_char_kanji_penalty: i64,
}

impl SegmentRerank {
    /// The segment's amounts summed.
    pub fn total(&self) -> i64 {
        self.script_cost + self.te_form_kanji_penalty + self.single_char_kanji_penalty
    }
}

impl std::ops::AddAssign for SegmentRerank {
    fn add_assign(&mut self, o: Self) {
        self.script_cost += o.script_cost;
        self.te_form_kanji_penalty += o.te_form_kanji_penalty;
        self.single_char_kanji_penalty += o.single_char_kanji_penalty;
    }
}

/// Weights for combining path features into a cost adjustment.
/// Each weight scales its corresponding feature (script cost is unscaled).
#[derive(Debug, Clone)]
pub struct FeatureWeights {
    /// Length variance weight (applied as variance_raw * weight / N²).
    pub length_variance: i64,
    /// Per-occurrence te-form kanji penalty.
    pub te_kanji: i64,
    /// Per-occurrence single-char kanji penalty.
    pub single_kanji: i64,
}

impl Default for FeatureWeights {
    fn default() -> Self {
        use crate::settings::{
            DEFAULT_LENGTH_VARIANCE_WEIGHT, DEFAULT_SINGLE_CHAR_KANJI_PENALTY,
            DEFAULT_TE_FORM_KANJI_PENALTY,
        };
        Self {
            length_variance: DEFAULT_LENGTH_VARIANCE_WEIGHT,
            te_kanji: DEFAULT_TE_FORM_KANJI_PENALTY,
            single_kanji: DEFAULT_SINGLE_CHAR_KANJI_PENALTY,
        }
    }
}

impl FeatureWeights {
    /// One segment's features, weighted.
    pub fn segment(&self, f: &SegmentFeatures) -> SegmentRerank {
        SegmentRerank {
            script_cost: f.script_cost,
            te_form_kanji_penalty: if f.te_kanji { self.te_kanji } else { 0 },
            single_char_kanji_penalty: if f.single_kanji { self.single_kanji } else { 0 },
        }
    }

    /// Build weights from settings.
    ///
    /// The script feature has no weight: its raw cost is the price (grid
    /// search showed no benefit from scaling it).
    pub fn from_settings() -> Self {
        let s = crate::settings::settings();
        Self {
            length_variance: s.reranker.length_variance_weight,
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

/// A one-character reading with kanji in the surface: the segments the
/// single-char kanji penalty looks at, whatever their role.
pub fn is_single_char_kanji(seg: &RichSegment) -> bool {
    seg.reading.chars().count() == 1 && seg.surface.chars().any(is_kanji)
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
    if !is_single_char_kanji(seg) || conn.role(seg.left_id) != 0 {
        return false;
    }
    let exempt = dict.is_some_and(|d| {
        let has_compound = |a: &RichSegment, b: &RichSegment| -> bool {
            let reading = format!("{}{}", a.reading, b.reading);
            let surface = format!("{}{}", a.surface, b.surface);
            // Symbols are outside the conversion model: no compound.
            d.lookup(&reading)
                .iter()
                .any(|e| !e.symbol && e.surface == surface)
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
    /// Each segment's features, in path order: the one place a segment's
    /// script cost and penalty predicates are evaluated.
    pub fn segment_features<'s>(
        &'s self,
        path: &'s ScoredPath,
    ) -> impl Iterator<Item = SegmentFeatures> + 's {
        path.segments.iter().enumerate().map(move |(i, seg)| {
            let prev = i.checked_sub(1).map(|j| &path.segments[j]);
            let (te_kanji, single_kanji) = match self.conn {
                Some(c) => (
                    is_te_form_kanji_penalised(seg, prev, c),
                    is_single_char_kanji_penalised(seg, i, &path.segments, c, self.dict),
                ),
                None => (false, false),
            };
            let script = segment_script(seg, prev, self.conn);
            SegmentFeatures {
                script_cost: script.cost,
                te_kanji,
                single_kanji,
                latin_unit_unformed: script.latin_unit_unformed,
            }
        })
    }

    /// Length variance of the path's content-word segments, excluding FW and
    /// single-char segments: `(raw, n)` as in [`PathFeatures`], `(0, 0)` when
    /// the path has no variance term.
    fn length_variance(&self, path: &ScoredPath) -> (i64, i64) {
        if path.segments.len() < 3 {
            return (0, 0);
        }
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
        if n < 2 {
            return (0, 0);
        }
        let sum: i64 = lengths.iter().sum();
        let sum_sq: i64 = lengths.iter().map(|l| l * l).sum();
        (n * sum_sq - sum * sum, n)
    }

    /// The path-level length-variance feature, weighted: the one rerank term
    /// that belongs to no segment.
    pub fn length_variance_cost(&self, path: &ScoredPath, w: &FeatureWeights) -> i64 {
        let (raw, n) = self.length_variance(path);
        variance_cost(raw, n, w)
    }

    /// Extract features from a path (the per-segment features summed, plus
    /// the path-level length variance).
    pub fn extract(&self, path: &ScoredPath) -> PathFeatures {
        let (length_variance_raw, length_variance_n) = self.length_variance(path);
        let mut f = PathFeatures {
            length_variance_raw,
            length_variance_n,
            ..PathFeatures::default()
        };
        for s in self.segment_features(path) {
            f.script_cost += s.script_cost;
            f.te_kanji_count += i64::from(s.te_kanji);
            f.single_kanji_count += i64::from(s.single_kanji);
            f.latin_unit_unformed += i64::from(s.latin_unit_unformed);
        }
        f
    }
}
