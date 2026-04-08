//! Grid search over FeatureWeights to optimise conversion accuracy.
//!
//! The expensive work (Viterbi + resegment + feature extraction) runs once per
//! reading.  Grid search then re-scores candidates with different weights using
//! pure arithmetic — fast enough for thousands of combinations.

use crate::dict::connection::ConnectionMatrix;
use crate::dict::Dictionary;
use crate::settings::settings;

use super::cost::DefaultCostFunction;
use super::features::FeatureConfig;
pub use super::features::{FeatureWeights, PathFeatures};
use super::lattice::build_lattice;
use super::resegment;
use super::viterbi::{viterbi_nbest, ScoredPath};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// A pre-computed candidate: surface + base cost + raw features.
#[derive(Debug, Clone)]
pub struct TuneCandidate {
    pub surface: String,
    pub base_cost: i64,
    pub features: PathFeatures,
}

/// Pre-computed data for one corpus reading.
#[derive(Debug, Clone)]
pub struct TuneCase {
    pub reading: String,
    pub expected: String,
    pub candidates: Vec<TuneCandidate>,
}

/// Ranges for grid search.  Each `Vec<i64>` is the list of values to try.
///
/// `structure` and `script` are fixed in `FeatureWeights::from_settings()`
/// (compile-time constants), so the default grid only varies the three
/// settings-backed weights.
#[derive(Debug, Clone)]
pub struct WeightGrid {
    pub length_variance: Vec<i64>,
    pub te_kanji: Vec<i64>,
    pub single_kanji: Vec<i64>,
}

impl Default for WeightGrid {
    fn default() -> Self {
        Self {
            length_variance: vec![0, 500, 1000, 2000, 3000, 5000],
            te_kanji: vec![0, 1000, 2000, 3500, 5000, 7000],
            single_kanji: vec![0, 1000, 2000, 4000, 6000, 8000],
        }
    }
}

impl WeightGrid {
    /// Total number of weight combinations.
    pub fn total_combinations(&self) -> usize {
        self.length_variance.len() * self.te_kanji.len() * self.single_kanji.len()
    }
}

/// Result of evaluating a single weight combination.
#[derive(Debug, Clone)]
pub struct TuneEval {
    pub weights: FeatureWeights,
    pub pass_count: usize,
    pub total: usize,
}

/// A case whose top-1 surface differs between default and best weights.
#[derive(Debug, Clone)]
pub struct TuneCaseDiff {
    pub reading: String,
    pub expected: String,
    pub default_top1: String,
    pub best_top1: String,
    pub default_pass: bool,
    pub best_pass: bool,
}

/// A case that failed with the best weights.
#[derive(Debug, Clone)]
pub struct TuneFailure {
    pub reading: String,
    pub expected: String,
    pub actual: String,
}

/// Full grid search result.
#[derive(Debug, Clone)]
pub struct TuneResult {
    pub best: TuneEval,
    pub default_eval: TuneEval,
    pub top_n: Vec<TuneEval>,
    pub diffs: Vec<TuneCaseDiff>,
    pub best_failures: Vec<TuneFailure>,
}

// ---------------------------------------------------------------------------
// Pre-computation
// ---------------------------------------------------------------------------

/// Run Viterbi + resegment + hard filter + feature extraction for each case.
///
/// `cases` is a slice of `(reading, expected)` pairs.
pub fn precompute_cases(
    dict: &dyn Dictionary,
    conn: &ConnectionMatrix,
    cases: &[(String, String)],
) -> Vec<TuneCase> {
    let s = settings();
    let cap = s.reranker.structure_cost_transition_cap;
    let prefix_floor = (s.reranker.structure_cost_filter / 2).min(cap);
    let filter = s.reranker.structure_cost_filter;
    let cost_fn = DefaultCostFunction::new(Some(conn));

    cases
        .iter()
        .map(|(reading, expected)| {
            let lattice = build_lattice(dict, reading);
            let mut paths = viterbi_nbest(&lattice, &cost_fn, 30);

            // Resegment
            let reseg = resegment::resegment(&paths, &lattice, Some(conn));
            paths.extend(reseg);

            // Extract features and pair with paths
            let mut paired: Vec<(ScoredPath, PathFeatures)> = paths
                .into_iter()
                .map(|p| {
                    let fcfg = FeatureConfig {
                        conn: Some(conn),
                        dict: Some(dict),
                        structure_cap: cap,
                        prefix_floor,
                    };
                    let f = fcfg.extract(&p, None);
                    (p, f)
                })
                .collect();

            // Hard filter using structure_cost from features
            hard_filter(&mut paired, prefix_floor, filter);

            // Build TuneCandidates from surviving paths
            let candidates = paired
                .iter()
                .map(|(p, f)| TuneCandidate {
                    surface: p.surface_key(),
                    base_cost: p.viterbi_cost,
                    features: f.clone(),
                })
                .collect();

            TuneCase {
                reading: reading.clone(),
                expected: expected.clone(),
                candidates,
            }
        })
        .collect()
}

/// Apply the structure-cost hard filter (same logic as reranker step 1-2).
///
/// Removes pairs whose structure_cost exceeds `min_sc + filter`.
fn hard_filter(paired: &mut Vec<(ScoredPath, PathFeatures)>, prefix_floor: i64, filter: i64) {
    if paired.len() <= 1 {
        return;
    }

    let min_sc = paired
        .iter()
        .map(|(p, f)| {
            if p.segments.len() <= 1 {
                prefix_floor
            } else {
                f.structure_cost
            }
        })
        .min()
        .unwrap_or(0);
    let threshold = min_sc + filter;

    paired.retain(|(_, f)| f.structure_cost <= threshold);
}

// ---------------------------------------------------------------------------
// Grid search
// ---------------------------------------------------------------------------

/// Evaluate all weight combinations and return the best result.
pub fn grid_search(cases: &[TuneCase], grid: &WeightGrid, top_n: usize) -> TuneResult {
    // Evaluate current production weights as the baseline
    let default_weights = FeatureWeights::from_settings();
    let default_pass = count_passes(cases, &default_weights);
    let default_eval = TuneEval {
        weights: default_weights.clone(),
        pass_count: default_pass,
        total: cases.len(),
    };

    // Grid search — only track pass counts (cheap).
    // structure and script are fixed (compile-time constants in from_settings).
    let mut evals: Vec<TuneEval> = Vec::with_capacity(grid.total_combinations());

    for &lv in &grid.length_variance {
        for &te in &grid.te_kanji {
            for &sk in &grid.single_kanji {
                let w = FeatureWeights {
                    structure: default_weights.structure,
                    length_variance: lv,
                    script: default_weights.script,
                    te_kanji: te,
                    single_kanji: sk,
                };
                let pass_count = count_passes(cases, &w);
                evals.push(TuneEval {
                    weights: w,
                    pass_count,
                    total: cases.len(),
                });
            }
        }
    }

    // Sort by pass_count descending, tie-break by distance from production
    // weights (prefer weights closer to current settings for stability).
    let defaults = &default_weights;
    evals.sort_by(|a, b| {
        b.pass_count.cmp(&a.pass_count).then_with(|| {
            weight_distance(&a.weights, defaults).cmp(&weight_distance(&b.weights, defaults))
        })
    });

    let best = evals.first().cloned().unwrap_or(default_eval.clone());

    // Collect surfaces only for default and best (for diffs + failures)
    let default_surfaces = collect_surfaces(cases, &default_weights);
    let best_surfaces = collect_surfaces(cases, &best.weights);
    let diffs = compute_diffs(cases, &default_surfaces, &best_surfaces);

    let best_failures: Vec<TuneFailure> = cases
        .iter()
        .zip(best_surfaces.iter())
        .filter(|(c, s)| s.as_str() != c.expected)
        .map(|(c, s)| TuneFailure {
            reading: c.reading.clone(),
            expected: c.expected.clone(),
            actual: s.clone(),
        })
        .collect();

    let top_evals = evals.into_iter().take(top_n).collect();

    TuneResult {
        best,
        default_eval,
        top_n: top_evals,
        diffs,
        best_failures,
    }
}

/// Count how many cases pass with the given weights (no allocation).
fn count_passes(cases: &[TuneCase], weights: &FeatureWeights) -> usize {
    cases
        .iter()
        .filter(|case| top1_surface(&case.candidates, weights) == case.expected)
        .count()
}

/// Collect top-1 surfaces for all cases (only used for diff/failure reporting).
fn collect_surfaces(cases: &[TuneCase], weights: &FeatureWeights) -> Vec<String> {
    cases
        .iter()
        .map(|case| top1_surface(&case.candidates, weights).to_owned())
        .collect()
}

/// Find the top-1 surface for a set of candidates under the given weights.
fn top1_surface<'a>(candidates: &'a [TuneCandidate], weights: &FeatureWeights) -> &'a str {
    candidates
        .iter()
        .min_by_key(|c| c.base_cost + c.features.weighted_cost(weights))
        .map(|c| c.surface.as_str())
        .unwrap_or("")
}

/// L1 distance over the tunable weight dimensions (excludes fixed structure/script).
fn weight_distance(a: &FeatureWeights, b: &FeatureWeights) -> i64 {
    (a.length_variance - b.length_variance).abs()
        + (a.te_kanji - b.te_kanji).abs()
        + (a.single_kanji - b.single_kanji).abs()
}

/// Compute per-case diffs between two sets of top-1 surfaces.
fn compute_diffs(
    cases: &[TuneCase],
    default_surfaces: &[String],
    best_surfaces: &[String],
) -> Vec<TuneCaseDiff> {
    cases
        .iter()
        .zip(default_surfaces.iter().zip(best_surfaces.iter()))
        .filter_map(|(case, (def, best))| {
            if def == best {
                return None;
            }
            Some(TuneCaseDiff {
                reading: case.reading.clone(),
                expected: case.expected.clone(),
                default_top1: def.clone(),
                best_top1: best.clone(),
                default_pass: *def == case.expected,
                best_pass: *best == case.expected,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::converter::testutil::{test_dict, zero_conn_with_fw};

    #[test]
    fn precompute_returns_candidates() {
        let dict = test_dict();
        let conn = zero_conn_with_fw(1200, 200, 200);
        let cases = vec![("きょう".to_string(), "今日".to_string())];
        let result = precompute_cases(&dict, &conn, &cases);
        assert_eq!(result.len(), 1);
        assert!(!result[0].candidates.is_empty(), "should have candidates");
        assert!(
            result[0].candidates.iter().any(|c| c.surface == "今日"),
            "expected surface should be among candidates"
        );
    }

    #[test]
    fn grid_search_best_ge_default() {
        let dict = test_dict();
        let conn = zero_conn_with_fw(1200, 200, 200);
        let cases = vec![
            ("きょう".to_string(), "今日".to_string()),
            ("てんき".to_string(), "天気".to_string()),
        ];
        let tune_cases = precompute_cases(&dict, &conn, &cases);
        let result = grid_search(&tune_cases, &WeightGrid::default(), 5);
        assert!(
            result.best.pass_count >= result.default_eval.pass_count,
            "best should be at least as good as default"
        );
    }

    #[test]
    fn grid_search_empty_grid() {
        let cases = vec![TuneCase {
            reading: "あ".to_string(),
            expected: "あ".to_string(),
            candidates: vec![],
        }];
        let grid = WeightGrid {
            length_variance: vec![],
            te_kanji: vec![],
            single_kanji: vec![],
        };
        let result = grid_search(&cases, &grid, 5);
        // Should not panic; falls back to default
        assert_eq!(result.best.total, 1);
    }

    #[test]
    fn top1_surface_selects_lowest_cost() {
        let candidates = vec![
            TuneCandidate {
                surface: "A".to_string(),
                base_cost: 100,
                features: PathFeatures::default(),
            },
            TuneCandidate {
                surface: "B".to_string(),
                base_cost: 50,
                features: PathFeatures::default(),
            },
        ];
        let w = FeatureWeights::default();
        assert_eq!(top1_surface(&candidates, &w), "B");
    }

    #[test]
    fn weight_distance_is_zero_for_same() {
        let w = FeatureWeights::from_settings();
        assert_eq!(weight_distance(&w, &w), 0);
    }
}
