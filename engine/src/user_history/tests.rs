use std::fs;
use std::path::Path;

use super::*;

#[test]
fn test_record_unigram() {
    let mut h = UserHistory::new();
    h.record(&[("きょう".into(), "今日".into())]);
    assert!(h.unigram_boost("きょう", "今日", now_epoch()) > 0);
}

#[test]
fn test_record_bigram() {
    let mut h = UserHistory::new();
    h.record(&[("きょう".into(), "今日".into()), ("は".into(), "は".into())]);
    assert!(h.bigram_boost("今日", "は", "は", now_epoch()) > 0);
}

#[test]
fn test_frequency_increment() {
    let mut h = UserHistory::new();
    h.record(&[("きょう".into(), "今日".into())]);
    h.record(&[("きょう".into(), "今日".into())]);
    let entry = &h.unigrams["きょう"]["今日"];
    assert_eq!(entry.frequency, 2);
}

#[test]
fn test_serialize_roundtrip() {
    let mut h = UserHistory::new();
    h.record(&[("きょう".into(), "今日".into()), ("は".into(), "は".into())]);
    let bytes = h.to_bytes().unwrap();
    let h2 = UserHistory::from_bytes(&bytes).unwrap();
    let now = now_epoch();
    assert!(h2.unigram_boost("きょう", "今日", now) > 0);
    assert!(h2.bigram_boost("今日", "は", "は", now) > 0);
}

#[test]
fn test_file_roundtrip() {
    let dir = std::env::temp_dir().join("lexime_test_history");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("test.lxud");

    let mut h = UserHistory::new();
    h.record(&[("きょう".into(), "今日".into())]);
    h.save(&path).unwrap();

    let h2 = UserHistory::open(&path).unwrap();
    assert!(h2.unigram_boost("きょう", "今日", now_epoch()) > 0);

    fs::remove_dir_all(&dir).ok();
}

#[test]
fn test_open_nonexistent() {
    let h = UserHistory::open(Path::new("/nonexistent/path/history.lxud")).unwrap();
    assert_eq!(h.unigram_boost("きょう", "今日", now_epoch()), 0);
}

#[test]
fn test_evict() {
    let mut h = UserHistory::new();
    // Insert MAX_UNIGRAMS + 1 entries
    for i in 0..=MAX_UNIGRAMS {
        h.record(&[(format!("r{i}"), format!("s{i}"))]);
    }
    let count: usize = h.unigrams.values().map(|inner| inner.len()).sum();
    assert!(count <= MAX_UNIGRAMS);
}

#[test]
fn test_reorder_candidates() {
    let mut h = UserHistory::new();
    h.record(&[("きょう".into(), "京".into())]);

    let entries = vec![
        DictEntry {
            surface: "今日".into(),
            cost: 3000,
            left_id: 0,
            right_id: 0,
        },
        DictEntry {
            surface: "京".into(),
            cost: 5000,
            left_id: 0,
            right_id: 0,
        },
    ];
    let reordered = h.reorder_candidates("きょう", &entries);
    assert_eq!(reordered[0].surface, "京");
}

#[test]
fn test_no_boost_for_unrecorded() {
    let h = UserHistory::new();
    let now = now_epoch();
    assert_eq!(h.unigram_boost("きょう", "今日", now), 0);
    assert_eq!(h.bigram_boost("今日", "は", "は", now), 0);
}

#[test]
fn test_decay_recent() {
    // Just recorded → decay ≈ 1.0
    let now = now_epoch();
    let d = decay(now, now);
    assert!(
        (d - 1.0).abs() < 0.01,
        "recent decay should be ~1.0, got {d}"
    );
}

#[test]
fn test_decay_one_week_old() {
    // 1 week (168 hours) ago → decay = 1/(1+1) = 0.5
    let now = now_epoch();
    let one_week_ago = now - 168 * 3600;
    let d = decay(one_week_ago, now);
    assert!(
        (d - 0.5).abs() < 0.01,
        "1-week decay should be ~0.5, got {d}"
    );
}

#[test]
fn test_decay_very_old() {
    // Very old entry → decay approaches 0
    let now = now_epoch();
    let very_old = now.saturating_sub(365 * 24 * 3600);
    let d = decay(very_old, now);
    assert!(d < 0.02, "very old decay should be near 0, got {d}");
}

#[test]
fn test_decay_future_timestamp() {
    // Future timestamp → saturating_sub yields 0 hours → decay = 1.0
    let now = now_epoch();
    let future = now + 3600;
    let d = decay(future, now);
    assert!(
        (d - 1.0).abs() < 0.001,
        "future decay should be 1.0, got {d}"
    );
}

#[test]
fn test_decay_known_timestamps() {
    // Use fixed timestamps so the test is fully deterministic (no system clock).
    let now: u64 = 1_700_000_000; // arbitrary epoch value

    // 0 hours elapsed → decay = 1/(1+0/168) = 1.0
    assert!(
        (decay(now, now) - 1.0).abs() < 1e-9,
        "zero elapsed: expected 1.0"
    );

    // Exactly 1 half-life (168 h) elapsed → decay = 1/(1+1) = 0.5
    let one_hl = now - 168 * 3600;
    assert!(
        (decay(one_hl, now) - 0.5).abs() < 1e-9,
        "one half-life: expected 0.5"
    );

    // Exactly 2 half-lives (336 h) elapsed → decay = 1/(1+2) ≈ 0.333…
    let two_hl = now - 336 * 3600;
    let expected = 1.0 / 3.0;
    assert!(
        (decay(two_hl, now) - expected).abs() < 1e-9,
        "two half-lives: expected {expected}"
    );

    // 24 hours elapsed → decay = 1/(1+24/168) = 168/192 = 0.875
    let day_ago = now - 24 * 3600;
    let expected_day = 168.0 / 192.0;
    assert!(
        (decay(day_ago, now) - expected_day).abs() < 1e-9,
        "24h elapsed: expected {expected_day}"
    );

    // Future timestamp (last_used > now) → saturating_sub gives 0 → decay = 1.0
    let future = now + 9999;
    assert!(
        (decay(future, now) - 1.0).abs() < 1e-9,
        "future timestamp: expected 1.0"
    );
}

#[test]
fn test_reorder_candidates_no_boost_preserves_order() {
    let h = UserHistory::new();
    let entries = vec![
        DictEntry {
            surface: "今日".into(),
            cost: 3000,
            left_id: 0,
            right_id: 0,
        },
        DictEntry {
            surface: "京".into(),
            cost: 5000,
            left_id: 0,
            right_id: 0,
        },
        DictEntry {
            surface: "教".into(),
            cost: 6000,
            left_id: 0,
            right_id: 0,
        },
    ];
    let reordered = h.reorder_candidates("きょう", &entries);
    // All boosts are 0 → original order preserved
    assert_eq!(reordered[0].surface, "今日");
    assert_eq!(reordered[1].surface, "京");
    assert_eq!(reordered[2].surface, "教");
}

#[test]
fn test_bigram_successors() {
    let mut h = UserHistory::new();
    h.record(&[
        ("きょう".into(), "今日".into()),
        ("は".into(), "は".into()),
        ("いい".into(), "良い".into()),
    ]);
    let succs = h.bigram_successors("今日");
    assert_eq!(succs.len(), 1);
    assert_eq!(succs[0].0, "は"); // reading
    assert_eq!(succs[0].1, "は"); // surface
    assert!(succs[0].2 > 0); // boost

    let succs2 = h.bigram_successors("は");
    assert_eq!(succs2.len(), 1);
    assert_eq!(succs2[0].0, "いい");
    assert_eq!(succs2[0].1, "良い");

    // No successors for "良い" (end of chain)
    let succs3 = h.bigram_successors("良い");
    assert!(succs3.is_empty());
}

#[test]
fn test_bigram_successors_empty_history() {
    let h = UserHistory::new();
    assert!(h.bigram_successors("今日").is_empty());
}

#[test]
fn test_save_to_invalid_path() {
    let h = UserHistory::new();
    let result = h.save(Path::new("/nonexistent/deeply/nested/dir/history.lxud"));
    // create_dir_all on a path under /nonexistent should fail on macOS
    assert!(result.is_err());
}
