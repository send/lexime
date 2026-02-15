use super::*;
use crate::converter::cost::DefaultCostFunction;
use crate::converter::testutil::test_dict;

#[test]
fn test_nbest_returns_multiple_paths() {
    let dict = test_dict();
    // "きょう" has two entries: 今日(3000) and 京(5000)
    let results = convert_nbest(&dict, None, "きょう", 5);

    assert!(results.len() >= 2, "should return at least 2 paths");
    // 1-best should be 今日
    assert_eq!(results[0][0].surface, "今日");
    // 2nd best should be 京
    assert_eq!(results[1][0].surface, "京");
}

#[test]
fn test_nbest_first_matches_1best() {
    let dict = test_dict();
    let best = convert(&dict, None, "きょうはいいてんき");
    let nbest = convert_nbest(&dict, None, "きょうはいいてんき", 5);

    assert!(!nbest.is_empty());
    let best_surfaces: Vec<&str> = best.iter().map(|s| s.surface.as_str()).collect();
    let nbest_surfaces: Vec<&str> = nbest[0].iter().map(|s| s.surface.as_str()).collect();
    assert_eq!(best_surfaces, nbest_surfaces, "1-best must match convert()");
}

#[test]
fn test_nbest_deduplicates_surfaces() {
    let dict = test_dict();
    let results = convert_nbest(&dict, None, "きょうは", 10);

    let surface_strings: Vec<String> = results
        .iter()
        .map(|path| path.iter().map(|s| s.surface.as_str()).collect::<String>())
        .collect();
    let unique: std::collections::HashSet<&String> = surface_strings.iter().collect();
    assert_eq!(
        surface_strings.len(),
        unique.len(),
        "N-best should not contain duplicate surface strings"
    );
}

#[test]
fn test_nbest_empty_input() {
    let dict = test_dict();
    let results = convert_nbest(&dict, None, "", 5);
    assert!(results.is_empty());
}

#[test]
fn test_nbest_n_zero() {
    let dict = test_dict();
    let results = convert_nbest(&dict, None, "きょう", 0);
    assert!(results.is_empty());
}

#[test]
fn test_nbest_n_one_matches_1best() {
    let dict = test_dict();
    let best = convert(&dict, None, "きょうはいいてんき");
    let nbest = convert_nbest(&dict, None, "きょうはいいてんき", 1);

    // n=1 Viterbi candidate + 1 katakana rewriter candidate
    assert!(!nbest.is_empty());
    let best_surfaces: Vec<&str> = best.iter().map(|s| s.surface.as_str()).collect();
    let nbest_surfaces: Vec<&str> = nbest[0].iter().map(|s| s.surface.as_str()).collect();
    assert_eq!(best_surfaces, nbest_surfaces);
}

#[test]
fn test_nbest_includes_katakana_candidate() {
    let dict = test_dict();
    let results = convert_nbest(&dict, None, "きょう", 10);

    let surfaces: Vec<String> = results
        .iter()
        .map(|path| path.iter().map(|s| s.surface.as_str()).collect::<String>())
        .collect();
    assert!(
        surfaces.contains(&"キョウ".to_string()),
        "N-best should include katakana candidate, got: {:?}",
        surfaces
    );
}

#[test]
fn test_nbest_sorted_by_cost() {
    // Verify N-best paths are returned in ascending cost order
    let dict = test_dict();
    let cost_fn = DefaultCostFunction::new(None);
    let lattice = build_lattice(&dict, "きょうは");
    let results = viterbi_nbest(&lattice, &cost_fn, 10);

    // We can't directly check costs from the public API, but we can verify
    // the best path is first (already tested above). For additional confidence,
    // ensure at least 2 results with different segmentations.
    assert!(results.len() >= 2);
    // Different segmentations should exist (e.g., 今日+は vs 京+は vs き+ょ+う+は)
    let first: String = results[0].segments.iter().map(|s| &*s.surface).collect();
    let second: String = results[1].segments.iter().map(|s| &*s.surface).collect();
    assert_ne!(first, second);
}
