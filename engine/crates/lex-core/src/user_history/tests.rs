use std::fs;
use std::path::Path;

use crate::settings::settings;

use super::wal::{CheckpointOrigin, HistoryWal};
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
fn test_unigrams_iterator() {
    let mut h = UserHistory::new();
    h.record(&[("きょう".into(), "今日".into())]);
    h.record(&[("きょう".into(), "今日".into())]);
    h.record(&[("きょう".into(), "京".into())]);

    let mut records: Vec<(String, String, u32)> = h
        .unigrams()
        .map(|(r, s, e)| (r.to_string(), s.to_string(), e.frequency))
        .collect();
    records.sort();
    assert_eq!(
        records,
        vec![
            ("きょう".to_string(), "京".to_string(), 1),
            ("きょう".to_string(), "今日".to_string(), 2),
        ]
    );
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
    let max_unigrams = settings().history.max_unigrams;
    let mut h = UserHistory::new();
    // Insert max_unigrams + 1 entries
    for i in 0..=max_unigrams {
        h.record(&[(format!("r{i}"), format!("s{i}"))]);
    }
    let count: usize = h.unigrams.values().map(|inner| inner.len()).sum();
    assert!(count <= max_unigrams);
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
fn test_learned_surfaces() {
    let mut h = UserHistory::new();
    let now = 1_700_000_000;
    h.record_at(&[("ゆうかい".into(), "夕会".into())], now);
    h.record_at(&[("ゆうかい".into(), "夕会".into())], now + 1);
    h.record_at(&[("ゆうかい".into(), "誘拐".into())], now + 2);

    let surfaces = h.learned_surfaces("ゆうかい", now + 3);
    assert_eq!(surfaces.len(), 2);
    // "夕会" has frequency=2 → higher boost → first
    assert_eq!(surfaces[0].0, "夕会");
    assert_eq!(surfaces[1].0, "誘拐");
    assert!(surfaces[0].1 > surfaces[1].1);
}

#[test]
fn test_learned_surfaces_empty() {
    let h = UserHistory::new();
    assert!(h.learned_surfaces("ゆうかい", now_epoch()).is_empty());
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
    // Use a regular file as a path component: create_dir_all fails with
    // ENOTDIR deterministically, even for root / CAP_DAC_OVERRIDE
    // environments where permission-based invalid paths would succeed.
    let dir = tempfile::tempdir().unwrap();
    let blocker = dir.path().join("not_a_dir");
    fs::write(&blocker, b"x").unwrap();
    let h = UserHistory::new();
    let result = h.save(&blocker.join("nested").join("history.lxud"));
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// WAL tests
// ---------------------------------------------------------------------------

#[test]
fn test_wal_append_and_replay() {
    let dir = tempfile::tempdir().unwrap();
    let cp = dir.path().join("history.lxud");

    let mut wal = HistoryWal::new(&cp);
    let now = 1_700_000_000;
    wal.append(&[("きょう".into(), "今日".into())], now)
        .unwrap();
    wal.append(
        &[("きょう".into(), "今日".into()), ("は".into(), "は".into())],
        now + 1,
    )
    .unwrap();
    assert_eq!(wal.entry_count(), 2);

    // Replay into fresh history
    let mut h = UserHistory::new();
    let mut wal2 = HistoryWal::new(&cp);
    let summary = wal2.replay(&mut h, CheckpointOrigin::Missing).unwrap();
    assert_eq!(summary.frames_replayed, 2);
    assert_eq!(summary.frames_skipped, 0);
    assert_eq!(wal2.entry_count(), 2);
    assert!(h.unigram_boost("きょう", "今日", now + 2) > 0);
    assert!(h.bigram_boost("今日", "は", "は", now + 2) > 0);
    // Sequence numbers 1 and 2 were assigned and applied
    assert_eq!(h.applied_seq(), 2);
}

#[test]
fn test_wal_compaction() {
    let dir = tempfile::tempdir().unwrap();
    let cp = dir.path().join("history.lxud");

    let mut h = UserHistory::new();
    let mut wal = HistoryWal::new(&cp);
    let now = 1_700_000_000;

    wal.append(&[("きょう".into(), "今日".into())], now)
        .unwrap();
    h.record_at(&[("きょう".into(), "今日".into())], now);

    // Force compact
    h.save(&cp).unwrap();
    wal.truncate_wal().unwrap();
    assert_eq!(wal.entry_count(), 0);
    assert!(cp.exists(), "checkpoint should exist");
    assert_eq!(
        fs::read(wal.wal_path()).unwrap().len(),
        8,
        "WAL should be header-only after truncation"
    );

    // Reopen: checkpoint has the data, WAL is empty
    let mut h2 = UserHistory::open(&cp).unwrap();
    let mut wal2 = HistoryWal::new(&cp);
    let replayed = wal2.replay(&mut h2, CheckpointOrigin::V2).unwrap();
    assert_eq!(replayed.frames_replayed, 0);
    assert!(h2.unigram_boost("きょう", "今日", now + 1) > 0);
}

#[test]
fn test_wal_truncated_frame() {
    let dir = tempfile::tempdir().unwrap();
    let cp = dir.path().join("history.lxud");

    let mut wal = HistoryWal::new(&cp);
    let now = 1_700_000_000;
    wal.append(&[("きょう".into(), "今日".into())], now)
        .unwrap();
    wal.append(&[("あした".into(), "明日".into())], now)
        .unwrap();

    // Truncate the WAL mid-frame (remove last 5 bytes)
    let data = fs::read(wal.wal_path()).unwrap();
    fs::write(wal.wal_path(), &data[..data.len() - 5]).unwrap();

    // Replay should recover the first entry only
    let mut h = UserHistory::new();
    let mut wal2 = HistoryWal::new(&cp);
    let summary = wal2.replay(&mut h, CheckpointOrigin::Missing).unwrap();
    assert_eq!(summary.frames_replayed, 1);
    assert!(h.unigram_boost("きょう", "今日", now + 1) > 0);
    assert_eq!(h.unigram_boost("あした", "明日", now + 1), 0);
    // Strict replay never mutates the file (offline tooling reads live
    // files); the corrupt tail is still there.
    assert_eq!(
        fs::read(wal2.wal_path()).unwrap().len(),
        data.len() - 5,
        "strict replay must not repair the file"
    );
}

#[test]
fn test_wal_corrupt_crc() {
    let dir = tempfile::tempdir().unwrap();
    let cp = dir.path().join("history.lxud");

    let mut wal = HistoryWal::new(&cp);
    let now = 1_700_000_000;
    wal.append(&[("きょう".into(), "今日".into())], now)
        .unwrap();
    wal.append(&[("あした".into(), "明日".into())], now)
        .unwrap();

    // Corrupt the CRC of the first frame (file header 8B + frame offset 4)
    let mut data = fs::read(wal.wal_path()).unwrap();
    data[12] ^= 0xFF;
    fs::write(wal.wal_path(), &data).unwrap();

    // Strict replay should stop at the corrupt frame without repairing
    let mut h = UserHistory::new();
    let mut wal2 = HistoryWal::new(&cp);
    let summary = wal2.replay(&mut h, CheckpointOrigin::Missing).unwrap();
    assert_eq!(summary.frames_replayed, 0);

    // Recovery-mode open repairs: truncate back to the last good frame
    // (here: the file header) so later appends stay visible to replay
    let (h2, wal3, report) = super::recovery::open_recovering(&cp).unwrap();
    assert_eq!(h2.unigram_boost("きょう", "今日", now + 1), 0);
    assert_eq!(report.wal_state, super::recovery::WalState::TailRepaired);
    assert_eq!(
        fs::read(wal3.wal_path()).unwrap().len(),
        8,
        "recovery should truncate the corrupt tail"
    );
}

#[test]
fn test_wal_empty_file() {
    let dir = tempfile::tempdir().unwrap();
    let cp = dir.path().join("history.lxud");

    // Create empty WAL file
    fs::write(cp.with_extension("lxud.wal"), b"").unwrap();

    let mut h = UserHistory::new();
    let mut wal = HistoryWal::new(&cp);
    let summary = wal.replay(&mut h, CheckpointOrigin::Missing).unwrap();
    assert_eq!(summary.frames_replayed, 0);
}

#[test]
fn test_open_with_wal_checkpoint_only() {
    let dir = tempfile::tempdir().unwrap();
    let cp = dir.path().join("history.lxud");

    // Create a v2 checkpoint (no WAL file)
    let mut h = UserHistory::new();
    h.record(&[("きょう".into(), "今日".into())]);
    h.save(&cp).unwrap();
    assert!(!cp.with_extension("lxud.wal").exists());

    // open_with_wal should load checkpoint and create WAL handle with 0 entries
    let (h2, wal) = super::wal::open_with_wal(&cp).unwrap();
    assert!(h2.unigram_boost("きょう", "今日", now_epoch()) > 0);
    assert_eq!(wal.entry_count(), 0);
}

#[test]
fn test_checkpoint_applied_seq_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let cp = dir.path().join("history.lxud");

    let mut h = UserHistory::new();
    h.record(&[("きょう".into(), "今日".into())]);
    h.advance_applied_seq(42);
    // Monotonic: lower values never roll it back
    h.advance_applied_seq(7);
    assert_eq!(h.applied_seq(), 42);
    h.save(&cp).unwrap();

    let h2 = UserHistory::open(&cp).unwrap();
    assert_eq!(h2.applied_seq(), 42);
    assert!(h2.unigram_boost("きょう", "今日", now_epoch()) > 0);
}

#[test]
fn test_record_at_preserves_timestamp() {
    let mut h = UserHistory::new();
    let ts = 1_600_000_000;
    h.record_at(&[("きょう".into(), "今日".into())], ts);
    let entry = &h.unigrams["きょう"]["今日"];
    assert_eq!(entry.last_used, ts);
}

#[test]
fn test_wal_roundtrip_with_bigrams() {
    let dir = tempfile::tempdir().unwrap();
    let cp = dir.path().join("history.lxud");
    let now = 1_700_000_000;

    let segments: Vec<(String, String)> = vec![
        ("きょう".into(), "今日".into()),
        ("は".into(), "は".into()),
        ("いい".into(), "良い".into()),
    ];

    let mut wal = HistoryWal::new(&cp);
    wal.append(&segments, now).unwrap();

    let mut h = UserHistory::new();
    let mut wal2 = HistoryWal::new(&cp);
    wal2.replay(&mut h, CheckpointOrigin::Missing).unwrap();

    assert!(h.unigram_boost("きょう", "今日", now + 1) > 0);
    assert!(h.bigram_boost("今日", "は", "は", now + 1) > 0);
    assert!(h.bigram_boost("は", "いい", "良い", now + 1) > 0);
}

// ---------------------------------------------------------------------------
// remove_entries tests
// ---------------------------------------------------------------------------

#[test]
fn test_remove_entries_unigram() {
    let mut h = UserHistory::new();
    h.record(&[("きょう".into(), "今日".into())]);
    assert!(h.unigram_boost("きょう", "今日", now_epoch()) > 0);

    let removed = h.remove_entries(&[("きょう".into(), "今日".into())]);
    assert!(removed);
    assert_eq!(h.unigram_boost("きょう", "今日", now_epoch()), 0);
    // The outer key should also be removed when inner map is empty
    assert!(!h.unigrams.contains_key("きょう"));
}

#[test]
fn test_remove_entries_bigram() {
    let mut h = UserHistory::new();
    h.record(&[("きょう".into(), "今日".into()), ("は".into(), "は".into())]);
    assert!(h.bigram_boost("今日", "は", "は", now_epoch()) > 0);

    let removed = h.remove_entries(&[("きょう".into(), "今日".into()), ("は".into(), "は".into())]);
    assert!(removed);
    assert_eq!(h.bigram_boost("今日", "は", "は", now_epoch()), 0);
}

#[test]
fn test_remove_entries_nonexistent() {
    let mut h = UserHistory::new();
    let removed = h.remove_entries(&[("きょう".into(), "今日".into())]);
    assert!(!removed);
}

#[test]
fn test_remove_entries_preserves_other_entries() {
    let mut h = UserHistory::new();
    h.record(&[("きょう".into(), "今日".into())]);
    h.record(&[("きょう".into(), "京".into())]);

    let removed = h.remove_entries(&[("きょう".into(), "今日".into())]);
    assert!(removed);
    assert_eq!(h.unigram_boost("きょう", "今日", now_epoch()), 0);
    // "京" should still exist
    assert!(h.unigram_boost("きょう", "京", now_epoch()) > 0);
}

#[test]
fn test_contains_entries_matches_remove_entries() {
    // The engine's no-op Tombstone skip relies on contains_entries being
    // true exactly when remove_entries would remove something — this test
    // pins the equivalence the doc comment claims.
    let probes: Vec<Vec<(String, String)>> = vec![
        vec![("きょう".into(), "今日".into())],
        vec![("きょう".into(), "京".into())], // learned reading, other surface
        vec![("なし".into(), "無し".into())], // never learned
        vec![
            ("あす".into(), "明日".into()),
            ("いく".into(), "行く".into()),
        ],
        vec![
            ("あす".into(), "明日".into()),
            ("かう".into(), "買う".into()),
        ], // bigram absent
    ];
    let build = || {
        let mut h = UserHistory::new();
        h.record(&[("きょう".into(), "今日".into())]);
        h.record(&[
            ("あす".into(), "明日".into()),
            ("いく".into(), "行く".into()),
        ]);
        h
    };
    for probe in &probes {
        let h = build();
        let mut remover = h.clone();
        assert_eq!(
            h.contains_entries(probe),
            remover.remove_entries(probe),
            "contains/remove diverged for {probe:?}"
        );
    }
    // Bigram-only presence (unigrams gone, bigram remains) must count.
    let mut h = build();
    h.unigrams.clear();
    let probe = vec![
        ("あす".to_string(), "明日".to_string()),
        ("いく".to_string(), "行く".to_string()),
    ];
    assert!(h.contains_entries(&probe));
    assert!(h.remove_entries(&probe));
}

// ---------------------------------------------------------------------------
// Durable residue (#286): memory-absent must stop implying disk-absent
// ---------------------------------------------------------------------------

fn pair(reading: &str, surface: &str) -> Vec<(String, String)> {
    vec![(reading.to_string(), surface.to_string())]
}

/// Raise residue the way production does: a tombstone whose frame never
/// reached the WAL (`seq: None`) removes the entry from memory while the
/// durable set keeps it.
fn raise_residue(h: &mut UserHistory, segments: &[(String, String)]) {
    h.apply_batch(&[(
        wal::WalRecord::Tombstone {
            segments: segments.to_vec(),
            timestamp: 0,
        },
        None,
    )]);
}

/// Fill both maps past capacity without paying an `evict()` per insert
/// (the batch/replay shape: apply many, settle capacity once).
fn over_capacity(unigrams: usize, bigrams: usize) -> UserHistory {
    let mut h = UserHistory::new();
    for i in 0..unigrams {
        h.unigrams.entry(format!("r{i}")).or_default().insert(
            format!("s{i}"),
            HistoryEntry {
                frequency: 1,
                last_used: 1000 + i as u64,
            },
        );
    }
    for i in 0..bigrams {
        h.bigrams.entry(format!("p{i}")).or_default().insert(
            (format!("n{i}"), format!("t{i}")),
            HistoryEntry {
                frequency: 1,
                last_used: 1000 + i as u64,
            },
        );
    }
    h
}

#[test]
fn test_evict_enforces_both_caps_and_reports_bigram_residue() {
    // Both maps must be processed, and BOTH halves must record what they
    // dropped. The bigram half is the easy one to lose: it is the only
    // residue path with no unigram counterpart to notice, so an evicted
    // bigram that stays in the checkpoint would make a later deletion of
    // that pair a silent no-op — #286 on the bigram axis.
    let (max_unigrams, max_bigrams) = {
        let s = settings();
        (s.history.max_unigrams, s.history.max_bigrams)
    };
    let mut h = over_capacity(max_unigrams + 1, max_bigrams + 3);
    h.evict();

    let unigrams: usize = h.unigrams.values().map(|inner| inner.len()).sum();
    let bigrams: usize = h.bigrams.values().map(|inner| inner.len()).sum();
    assert!(unigrams <= max_unigrams, "unigram cap enforced: {unigrams}");
    assert!(bigrams <= max_bigrams, "bigram cap enforced: {bigrams}");

    // p0..p2 are the oldest bigrams, so they are the ones evicted. Probe
    // them as a two-segment list, which is how a deletion arrives: the
    // residue must derive the same (prev_surface, next_reading,
    // next_surface) triple the history is keyed by. A transposed triple
    // answers false here.
    for i in 0..3 {
        let segs = vec![
            ("_".to_string(), format!("p{i}")),
            (format!("n{i}"), format!("t{i}")),
        ];
        assert!(
            h.deletion_has_durable_target(&segs),
            "evicted bigram p{i} must stay answerable as possibly-durable"
        );
    }
    // A bigram still in memory is not residue, and neither is one that never
    // existed — the triple must be matched in full, not by prev-surface.
    let retained = vec![
        ("_".to_string(), format!("p{}", max_bigrams + 2)),
        (
            format!("n{}", max_bigrams + 2),
            format!("t{}", max_bigrams + 2),
        ),
    ];
    assert!(!h.durable_residue.contains_entries(&retained));
    let wrong_surface = vec![
        ("_".to_string(), "p0".to_string()),
        ("n0".to_string(), "no-such-surface".to_string()),
    ];
    assert!(
        !h.durable_residue.contains_entries(&wrong_surface),
        "the residue is keyed by the whole triple, not by prev+reading"
    );
}

#[test]
fn test_evict_selection_is_unchanged_and_reports_residue() {
    // Two claims that must hold together: reporting the evicted keys must
    // not disturb *which* keys are chosen (lowest `frequency ×
    // decay(last_used)` first), and every key it drops must become residue.
    // The accuracy corpora cannot catch a selection regression here — they
    // seed ~30 entries against a cap of 10000, so eviction never runs.
    let max_unigrams = settings().history.max_unigrams;
    let mut h = over_capacity(max_unigrams + 3, 0);
    // r0..r2 are the oldest (lowest last_used ⇒ lowest score at equal
    // frequency), so they are exactly the three that must go.
    h.evict();
    for i in 0..3 {
        let p = pair(&format!("r{i}"), &format!("s{i}"));
        assert!(!h.contains_entries(&p), "oldest entries evicted first");
        assert!(
            h.deletion_has_durable_target(&p),
            "an evicted key must stay answerable as possibly-durable"
        );
    }
    for i in 3..6 {
        assert!(
            h.contains_entries(&pair(&format!("r{i}"), &format!("s{i}"))),
            "newer entries retained"
        );
    }
    // Entries still in memory are not residue: the fast path for a deletion
    // that is genuinely a no-op has to survive being at capacity.
    assert!(!h.deletion_has_durable_target(&pair("r9", "no-such-surface")));
    // And the residue is keyed by (reading, surface), not by reading alone:
    // r0 IS an evicted reading, so a probe for a different surface under it
    // must still answer false. Without this, deleting any never-learned
    // candidate that merely shares a reading with an evicted entry would pay
    // a key-thread full flush plus a checkpoint.
    assert!(
        !h.durable_residue
            .contains_entries(&pair("r0", "no-such-surface")),
        "residue outer key present, inner surface absent"
    );
}

#[test]
fn test_residue_cleared_by_the_checkpoint_that_covers_it() {
    let mut h = UserHistory::new();
    h.record(&pair("きのう", "昨日"));
    raise_residue(&mut h, &pair("あす", "明日"));
    assert!(h.deletion_has_durable_target(&pair("あす", "明日")));

    // A checkpoint written from this very state settles what it saw.
    let snapshot = h.clone();
    h.cover_durable_residue(&snapshot);
    assert!(!h.deletion_has_durable_target(&pair("あす", "明日")));
}

#[test]
fn test_residue_reraised_during_a_checkpoint_write_survives_it() {
    // The case a plain set cannot express. A key is raised, re-learned, and
    // raised again while the checkpoint is being written. That checkpoint
    // was cloned while the entry was in memory, so the file DOES contain it
    // — retiring the key would let a later deletion be skipped as a no-op
    // and resurrect on restart.
    let mut h = UserHistory::new();
    raise_residue(&mut h, &pair("あす", "明日"));

    // Re-learned, then cloned for the checkpoint: the snapshot holds both
    // the entry and the stale residue key.
    h.record(&pair("あす", "明日"));
    let snapshot = h.clone();
    assert!(snapshot.contains_entries(&pair("あす", "明日")));

    // Evicted again while the slow write is in flight.
    h.remove_entries(&pair("あす", "明日"));
    raise_residue(&mut h, &pair("あす", "明日"));

    h.cover_durable_residue(&snapshot);
    assert!(
        h.deletion_has_durable_target(&pair("あす", "明日")),
        "a key re-raised after the snapshot is still in the written checkpoint"
    );
}

#[test]
fn test_residue_duplicate_raise_at_the_cap_does_not_saturate() {
    // Saturation must key off the set's SIZE, not the number of raises. A
    // re-raise of an already-tracked key cannot grow the set, so throwing
    // away every tracked key for it would drop a precise residue to the
    // blanket "everything might be on disk" answer for no gain. Reachable
    // without any timing window: a segment list containing the same
    // (reading, surface) twice raises the same key twice in one deletion.
    let mut h = UserHistory::new();
    for i in 0..MAX_RESIDUE {
        raise_residue(&mut h, &pair(&format!("r{i}"), &format!("s{i}")));
    }
    assert_eq!(h.durable_residue.tracked, MAX_RESIDUE);
    assert!(!h.durable_residue.saturated);

    raise_residue(&mut h, &pair("r0", "s0"));
    assert!(
        !h.durable_residue.saturated,
        "a re-raise of a tracked key adds nothing and must not discard the set"
    );
    assert!(h.deletion_has_durable_target(&pair("r0", "s0")));

    // A genuinely new key past the cap does saturate.
    raise_residue(&mut h, &pair("brand", "new"));
    assert!(h.durable_residue.saturated);
}

#[test]
fn test_residue_saturation_is_conservative_and_lifts_only_when_quiescent() {
    let mut h = UserHistory::new();
    for i in 0..(MAX_RESIDUE + 1) {
        raise_residue(&mut h, &pair(&format!("r{i}"), &format!("s{i}")));
    }
    // Past the cap the set is dropped and every query answers "possible".
    assert!(h.deletion_has_durable_target(&pair("never", "raised")));

    // A checkpoint taken while raises are still arriving cannot lift it:
    // keys discarded during the write were never tracked.
    let snapshot = h.clone();
    raise_residue(&mut h, &pair("during", "write"));
    h.cover_durable_residue(&snapshot);
    assert!(h.deletion_has_durable_target(&pair("never", "raised")));

    // A checkpoint over a quiescent state does.
    let snapshot = h.clone();
    h.cover_durable_residue(&snapshot);
    assert!(!h.deletion_has_durable_target(&pair("never", "raised")));
}

#[test]
fn test_residue_matches_contains_entries_shape() {
    // The residue answers the same question over the same key space as
    // `contains_entries`, bigram pairs included — otherwise the two halves
    // of the deletion check would disagree about what a segment list means.
    let segs = vec![
        ("あす".to_string(), "明日".to_string()),
        ("いく".to_string(), "行く".to_string()),
    ];
    let mut h = UserHistory::new();
    raise_residue(&mut h, &segs);
    // Both halves answer over the same key space: what the residue records
    // for a segment list is exactly what `contains_entries` would find if
    // the entries were in memory.
    let mut in_memory = UserHistory::new();
    in_memory.record(&segs);
    assert!(in_memory.contains_entries(&segs));
    assert!(h.durable_residue.contains_entries(&segs));

    // Bigram-only residue must count, as it does for `contains_entries`.
    h.durable_residue.unigrams.clear();
    assert!(h.deletion_has_durable_target(&segs));
}

#[test]
fn test_residue_is_not_part_of_the_checkpoint_body() {
    // The residue is process-local state. `to_data`/`from_data` enumerate
    // their fields explicitly, so it cannot reach the file — pinned here
    // because the format is on-disk user data.
    let mut h = UserHistory::new();
    h.record(&pair("きょう", "今日"));
    let without = bincode::serialize(&h.to_data()).unwrap();
    raise_residue(&mut h, &pair("あす", "明日"));
    let with = bincode::serialize(&h.to_data()).unwrap();
    assert_eq!(without, with, "residue must not change the checkpoint body");
}
