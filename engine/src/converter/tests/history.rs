use super::*;
use crate::converter::testutil::test_dict;
use crate::user_history::UserHistory;

#[test]
fn test_convert_with_history_promotes_learned() {
    let dict = test_dict();
    // "きょう" has 今日(3000) and 京(5000). Without history, 今日 wins.
    let baseline = convert(&dict, None, "きょう");
    assert_eq!(baseline[0].surface, "今日");

    // After learning "京", it should be promoted
    let mut h = UserHistory::new();
    h.record(&[("きょう".into(), "京".into())]);
    h.record(&[("きょう".into(), "京".into())]);

    let result = convert_with_history(&dict, None, &h, "きょう");
    assert_eq!(result[0].surface, "京");
}

#[test]
fn test_convert_with_history_empty_history_matches_baseline() {
    let dict = test_dict();
    let h = UserHistory::new();

    let baseline = convert(&dict, None, "きょうはいいてんき");
    let with_history = convert_with_history(&dict, None, &h, "きょうはいいてんき");

    let baseline_surfaces: Vec<&str> = baseline.iter().map(|s| s.surface.as_str()).collect();
    let history_surfaces: Vec<&str> = with_history.iter().map(|s| s.surface.as_str()).collect();
    assert_eq!(baseline_surfaces, history_surfaces);
}

#[test]
fn test_convert_with_history_empty_input() {
    let dict = test_dict();
    let h = UserHistory::new();
    let result = convert_with_history(&dict, None, &h, "");
    assert!(result.is_empty());
}

#[test]
fn test_convert_nbest_with_history_promotes_learned() {
    let dict = test_dict();
    let mut h = UserHistory::new();
    h.record(&[("きょう".into(), "京".into())]);
    h.record(&[("きょう".into(), "京".into())]);

    let results = convert_nbest_with_history(&dict, None, &h, "きょう", 5);
    assert!(!results.is_empty());
    assert_eq!(results[0][0].surface, "京");
}

#[test]
fn test_convert_nbest_with_history_empty_input() {
    let dict = test_dict();
    let h = UserHistory::new();
    assert!(convert_nbest_with_history(&dict, None, &h, "", 5).is_empty());
    assert!(convert_nbest_with_history(&dict, None, &h, "きょう", 0).is_empty());
}
