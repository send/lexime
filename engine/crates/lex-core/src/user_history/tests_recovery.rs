//! Fault-injection tests for the v2 persistence protocol (design §11).
//!
//! Crash points are enumerated in the design as on-disk file states, so
//! nearly every scenario is "construct that exact byte state, then open":
//! - T1: recovery matrix, all 16 checkpoint × WAL combinations (§8)
//! - T2: structural exclusion of double-apply (item 4 / compaction races)
//! - T3: truncation sweep over every byte length of a WAL
//! - T4: bit-flip fuzz over WAL and checkpoint bytes
//! - T5: v1 → v2 migration incl. every §7 crash residue
//! - T9: seq monotonicity violations and gaps
//!
//! Power loss itself (page-cache loss) cannot be simulated in-process; the
//! syscall-ordering side is covered by the WalIo seam (T6, PR2).

use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

use super::deletion_marker::{self, DeletionBreach};
use super::recovery::{
    open_recovering, remove_recovery_artifacts, v1_backup_path, CheckpointState, WalState,
};
use super::wal::{
    HistoryWal, LegacyWalEntry, WalRecord, FRAME_HEADER_LEN, WAL_HEADER, WAL_HEADER_LEN,
};
use super::UserHistory;

const T0: u64 = 1_700_000_000;

const A: (&str, &str) = ("あ", "亜");
const B: (&str, &str) = ("い", "以");
const C: (&str, &str) = ("う", "宇");
const D: (&str, &str) = ("え", "江");
const E: (&str, &str) = ("お", "於");

struct Fx {
    _dir: TempDir,
    cp: PathBuf,
    wal: PathBuf,
}

fn fx() -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let cp = dir.path().join("user_history.lxud");
    let wal = cp.with_extension("lxud.wal");
    Fx { _dir: dir, cp, wal }
}

fn seg(pair: (&str, &str)) -> Vec<(String, String)> {
    vec![(pair.0.to_string(), pair.1.to_string())]
}

fn present(h: &UserHistory, pair: (&str, &str)) -> bool {
    h.unigram_boost(pair.0, pair.1, T0 + 10) > 0
}

fn frequency(h: &UserHistory, pair: (&str, &str)) -> u32 {
    h.unigrams()
        .find(|(r, s, _)| *r == pair.0 && *s == pair.1)
        .map(|(_, _, e)| e.frequency)
        .unwrap_or(0)
}

fn assert_contents(h: &UserHistory, expected: &[(&str, &str)], absent: &[(&str, &str)]) {
    for &p in expected {
        assert!(present(h, p), "{}→{} should be present", p.0, p.1);
    }
    for &p in absent {
        assert!(!present(h, p), "{}→{} should be absent", p.0, p.1);
    }
}

/// Baseline v2 state. cp: {A, B} with applied_seq = 2. WAL: frames C, D, E
/// as seq 3..=5. With `overlap`, frames 1..=5 stay in the WAL — the exact
/// on-disk state of a crash between checkpoint write and WAL truncation
/// (item 4), i.e. the "stale checkpoint" row of the matrix.
fn build_v2_state(f: &Fx, overlap: bool) {
    let mut h = UserHistory::new();
    let mut wal = HistoryWal::new(&f.cp);
    for pair in [A, B] {
        let seq = wal.append(&seg(pair), T0).unwrap();
        h.record_at(&seg(pair), T0);
        h.advance_applied_seq(seq);
    }
    h.save(&f.cp).unwrap();
    if !overlap {
        wal.truncate_wal().unwrap();
    }
    for pair in [C, D, E] {
        wal.append(&seg(pair), T0).unwrap();
    }
}

/// Cut the last `n` bytes off (mid-frame tail corruption, as power loss
/// leaves it).
fn cut_tail(path: &Path, n: usize) {
    let data = fs::read(path).unwrap();
    fs::write(path, &data[..data.len() - n]).unwrap();
}

/// Flip a payload byte of the first frame in the file (mid-WAL corruption:
/// everything after is unreachable because framing cannot re-synchronize).
fn corrupt_first_frame(path: &Path) {
    let mut data = fs::read(path).unwrap();
    let idx = WAL_HEADER_LEN + FRAME_HEADER_LEN + 2;
    data[idx] ^= 0xFF;
    fs::write(path, &data).unwrap();
}

/// Flip a checkpoint body byte (CRC mismatch → corrupt).
fn corrupt_cp(path: &Path) {
    let mut data = fs::read(path).unwrap();
    let idx = 32 + 2;
    data[idx] ^= 0xFF;
    fs::write(path, &data).unwrap();
}

fn quarantine_count(f: &Fx) -> usize {
    fs::read_dir(f.cp.parent().unwrap())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| n.contains(".corrupt-"))
        })
        .count()
}

// ---------------------------------------------------------------------------
// T1: recovery matrix (§8). Row = checkpoint, column = WAL. Every cell must
// open successfully; assertions follow the matrix's action/loss/visibility.
// ---------------------------------------------------------------------------

#[test]
fn t1_cp_normal_wal_normal() {
    let f = fx();
    build_v2_state(&f, false);
    let (h, _, report) = open_recovering(&f.cp).unwrap();
    assert_contents(&h, &[A, B, C, D, E], &[]);
    assert_eq!(report.checkpoint_state, CheckpointState::Loaded);
    assert_eq!(report.wal_state, WalState::Clean);
    assert_eq!(report.frames_replayed, 3);
    assert_eq!(report.frames_skipped, 0);
    assert!(report.is_clean());
    assert!(!report.data_loss_suspected());
    // Idempotent: a second open sees identical state
    let (h2, _, r2) = open_recovering(&f.cp).unwrap();
    assert_contents(&h2, &[A, B, C, D, E], &[]);
    assert!(r2.is_clean());
}

#[test]
fn t1_cp_normal_wal_missing() {
    let f = fx();
    build_v2_state(&f, false);
    fs::remove_file(&f.wal).unwrap();
    let (h, _, report) = open_recovering(&f.cp).unwrap();
    // Same shape as a clean shutdown; external deletion is undetectable
    assert_contents(&h, &[A, B], &[C, D, E]);
    assert_eq!(report.wal_state, WalState::Missing);
    assert!(!report.data_loss_suspected());
}

#[test]
fn t1_cp_normal_wal_tail_corrupt() {
    let f = fx();
    build_v2_state(&f, false);
    let full_len = fs::read(&f.wal).unwrap().len();
    cut_tail(&f.wal, 5);
    let (h, _, report) = open_recovering(&f.cp).unwrap();
    // Good prefix replayed; only the torn last frame (E) is lost
    assert_contents(&h, &[A, B, C, D], &[E]);
    assert_eq!(report.wal_state, WalState::TailRepaired);
    assert_eq!(report.frames_replayed, 2);
    // Physical repair: file truncated at the last good frame boundary
    let repaired_len = fs::read(&f.wal).unwrap().len();
    assert!(repaired_len < full_len - 5);
    // Not user-visible data loss (§8 ※1: expected power-loss residue)
    assert!(!report.data_loss_suspected());
    // Next open is clean and content is preserved
    let (h2, _, r2) = open_recovering(&f.cp).unwrap();
    assert_contents(&h2, &[A, B, C, D], &[E]);
    assert_eq!(r2.wal_state, WalState::Clean);
}

#[test]
fn t1_cp_normal_wal_mid_corrupt() {
    let f = fx();
    build_v2_state(&f, false);
    corrupt_first_frame(&f.wal);
    let (h, _, report) = open_recovering(&f.cp).unwrap();
    // Framing cannot re-synchronize past the corrupt frame: C, D, E all lost
    assert_contents(&h, &[A, B], &[C, D, E]);
    assert_eq!(report.wal_state, WalState::TailRepaired);
    assert_eq!(
        fs::read(&f.wal).unwrap().len(),
        WAL_HEADER_LEN,
        "repair truncates to the header when frame 1 is corrupt"
    );
}

#[test]
fn t1_cp_missing_wal_normal() {
    let f = fx();
    build_v2_state(&f, false);
    fs::remove_file(&f.cp).unwrap();
    let (h, _, report) = open_recovering(&f.cp).unwrap();
    // applied_seq = 0 → full WAL replay recovers the un-checkpointed tail
    assert_contents(&h, &[C, D, E], &[A, B]);
    assert_eq!(report.checkpoint_state, CheckpointState::Missing);
    assert_eq!(report.frames_replayed, 3);
    // Abnormal state (checkpoint should not vanish alone): re-checkpoint early
    assert!(report.compaction_recommended);
}

#[test]
fn t1_cp_missing_wal_missing() {
    let f = fx();
    let (h, _, report) = open_recovering(&f.cp).unwrap();
    // Fresh-install shape
    assert_contents(&h, &[], &[A, B, C, D, E]);
    assert_eq!(report.checkpoint_state, CheckpointState::Missing);
    assert_eq!(report.wal_state, WalState::Missing);
    assert!(report.is_clean());
    assert!(!report.compaction_recommended);
}

#[test]
fn t1_cp_missing_wal_tail_corrupt() {
    let f = fx();
    build_v2_state(&f, false);
    fs::remove_file(&f.cp).unwrap();
    cut_tail(&f.wal, 5);
    let (h, _, report) = open_recovering(&f.cp).unwrap();
    assert_contents(&h, &[C, D], &[A, B, E]);
    assert_eq!(report.wal_state, WalState::TailRepaired);
}

#[test]
fn t1_cp_missing_wal_mid_corrupt() {
    let f = fx();
    build_v2_state(&f, false);
    fs::remove_file(&f.cp).unwrap();
    corrupt_first_frame(&f.wal);
    let (h, _, report) = open_recovering(&f.cp).unwrap();
    assert_contents(&h, &[], &[A, B, C, D, E]);
    assert_eq!(report.wal_state, WalState::TailRepaired);
}

#[test]
fn t1_cp_corrupt_wal_normal() {
    let f = fx();
    build_v2_state(&f, false);
    corrupt_cp(&f.cp);
    let (h, _, report) = open_recovering(&f.cp).unwrap();
    // Checkpoint-only learning is lost; WAL replay recovers the recent tail
    assert_contents(&h, &[C, D, E], &[A, B]);
    assert_eq!(report.checkpoint_state, CheckpointState::Quarantined);
    assert_eq!(report.quarantined_paths.len(), 1);
    assert!(!f.cp.exists(), "corrupt checkpoint must be renamed away");
    assert!(
        report.quarantined_paths[0].exists(),
        "quarantined bytes must be preserved for rescue"
    );
    // User-visible (§8 ※2) + re-checkpoint early
    assert!(report.data_loss_suspected());
    assert!(report.compaction_recommended);
    // The quarantine unblocks the next startup: reopen is a normal
    // missing-checkpoint open, not a repeated failure
    let (h2, _, r2) = open_recovering(&f.cp).unwrap();
    assert_contents(&h2, &[C, D, E], &[A, B]);
    assert_eq!(r2.checkpoint_state, CheckpointState::Missing);
}

#[test]
fn t1_cp_corrupt_wal_missing() {
    let f = fx();
    build_v2_state(&f, false);
    corrupt_cp(&f.cp);
    fs::remove_file(&f.wal).unwrap();
    let (h, _, report) = open_recovering(&f.cp).unwrap();
    assert_contents(&h, &[], &[A, B, C, D, E]);
    assert_eq!(report.checkpoint_state, CheckpointState::Quarantined);
    assert!(report.data_loss_suspected());
}

#[test]
fn t1_cp_corrupt_wal_tail_corrupt() {
    let f = fx();
    build_v2_state(&f, false);
    corrupt_cp(&f.cp);
    cut_tail(&f.wal, 5);
    let (h, _, report) = open_recovering(&f.cp).unwrap();
    assert_contents(&h, &[C, D], &[A, B, E]);
    assert_eq!(report.checkpoint_state, CheckpointState::Quarantined);
    assert_eq!(report.wal_state, WalState::TailRepaired);
}

#[test]
fn t1_cp_corrupt_wal_mid_corrupt() {
    let f = fx();
    build_v2_state(&f, false);
    corrupt_cp(&f.cp);
    corrupt_first_frame(&f.wal);
    let (h, _, report) = open_recovering(&f.cp).unwrap();
    assert_contents(&h, &[], &[A, B, C, D, E]);
    assert_eq!(report.checkpoint_state, CheckpointState::Quarantined);
    assert_eq!(report.wal_state, WalState::TailRepaired);
    assert!(report.data_loss_suspected());
}

#[test]
fn t1_cp_stale_wal_normal() {
    // Stale = crash between checkpoint write and WAL truncation: cp covers
    // seq 1..=2 but the WAL still holds frames 1..=5.
    let f = fx();
    build_v2_state(&f, true);
    let (h, _, report) = open_recovering(&f.cp).unwrap();
    assert_contents(&h, &[A, B, C, D, E], &[]);
    // The two covered frames are skipped, not re-applied (T2 asserts freq)
    assert_eq!(report.frames_skipped, 2);
    assert_eq!(report.frames_replayed, 3);
    assert!(!report.data_loss_suspected());
}

#[test]
fn t1_cp_stale_wal_missing() {
    let f = fx();
    build_v2_state(&f, true);
    fs::remove_file(&f.wal).unwrap();
    let (h, _, report) = open_recovering(&f.cp).unwrap();
    // Indistinguishable from normal × missing — by design
    assert_contents(&h, &[A, B], &[C, D, E]);
    assert_eq!(report.wal_state, WalState::Missing);
}

#[test]
fn t1_cp_stale_wal_tail_corrupt() {
    let f = fx();
    build_v2_state(&f, true);
    cut_tail(&f.wal, 5);
    let (h, _, report) = open_recovering(&f.cp).unwrap();
    assert_contents(&h, &[A, B, C, D], &[E]);
    assert_eq!(report.frames_skipped, 2);
    assert_eq!(report.wal_state, WalState::TailRepaired);
}

#[test]
fn t1_cp_stale_wal_mid_corrupt() {
    let f = fx();
    build_v2_state(&f, true);
    // Frame 1 (a *skipped* frame) is corrupted: CRC is still verified on
    // skip, so scanning stops immediately — checkpoint content only.
    corrupt_first_frame(&f.wal);
    let (h, _, report) = open_recovering(&f.cp).unwrap();
    assert_contents(&h, &[A, B], &[C, D, E]);
    assert_eq!(report.frames_skipped, 0);
    assert_eq!(report.wal_state, WalState::TailRepaired);
    assert_eq!(frequency(&h, A), 1, "no double apply even on this path");
}

// ---------------------------------------------------------------------------
// T2: structural exclusion of double-apply (item 4)
// ---------------------------------------------------------------------------

#[test]
fn t2_no_double_apply_after_compaction_crash() {
    // Exact item-4 state: checkpoint durable, truncation never happened.
    let f = fx();
    build_v2_state(&f, true);
    let (h, _, report) = open_recovering(&f.cp).unwrap();
    assert_eq!(frequency(&h, A), 1, "v1 replay would double this to 2");
    assert_eq!(frequency(&h, B), 1);
    assert_eq!(report.frames_skipped, 2);

    // The recovered state re-checkpoints and survives another such crash:
    // save a new checkpoint (covering everything) while leaving the WAL.
    let mut h2 = h.clone();
    h2.advance_applied_seq(5);
    h2.save(&f.cp).unwrap();
    let (h3, _, r3) = open_recovering(&f.cp).unwrap();
    assert_eq!(frequency(&h3, A), 1);
    assert_eq!(frequency(&h3, C), 1);
    assert_eq!(r3.frames_skipped, 5);
    assert_eq!(r3.frames_replayed, 0);
}

#[test]
fn t2_strict_replay_also_skips_covered_frames() {
    // The strict (lextool) path must apply the same seq filter, or audits
    // double-count during the item-4 window.
    let f = fx();
    build_v2_state(&f, true);
    let (h, _) = super::wal::open_with_wal(&f.cp).unwrap();
    assert_eq!(frequency(&h, A), 1);
    assert_eq!(frequency(&h, C), 1);
}

// ---------------------------------------------------------------------------
// T3: truncation sweep — every prefix of a WAL opens safely, recovers a
// monotone prefix, and the repaired file accepts appends (v1 problem A)
// ---------------------------------------------------------------------------

#[test]
fn t3_truncation_sweep() {
    let master = {
        let f = fx();
        let mut wal = HistoryWal::new(&f.cp);
        for i in 0..20 {
            wal.append(&[(format!("よみ{i}"), format!("面{i}"))], T0 + i)
                .unwrap();
        }
        fs::read(&f.wal).unwrap()
    };

    let mut prev_replayed = 0u64;
    for cut in 0..=master.len() {
        let f = fx();
        fs::write(&f.wal, &master[..cut]).unwrap();
        let (_h, mut wal, report) = open_recovering(&f.cp).unwrap();
        assert!(
            report.frames_replayed >= prev_replayed,
            "recovered frame count must be monotone in prefix length \
             (cut={cut}: {} < {prev_replayed})",
            report.frames_replayed
        );
        prev_replayed = report.frames_replayed;

        // The repaired file must accept appends that the next replay sees —
        // v1 left the corrupt tail in place and appended after it, making
        // every later frame permanently invisible (problem A).
        assert!(!wal.is_frozen(), "cut={cut}");
        wal.append(&seg(A), T0 + 100).unwrap();
        let (h2, _, r2) = open_recovering(&f.cp).unwrap();
        assert!(
            present(&h2, A),
            "appended frame lost after repair (cut={cut})"
        );
        assert_eq!(r2.frames_replayed, report.frames_replayed + 1, "cut={cut}");
        assert_eq!(r2.wal_state, WalState::Clean, "cut={cut}");
    }
    assert_eq!(
        prev_replayed, 20,
        "full-length prefix must recover all frames"
    );
}

// ---------------------------------------------------------------------------
// T4: bit-flip fuzz — no panic, no Err, no giant alloc, sane classification
// ---------------------------------------------------------------------------

#[test]
fn t4_bitflip_fuzz_wal() {
    let f0 = fx();
    build_v2_state(&f0, false);
    let cp_master = fs::read(&f0.cp).unwrap();
    let wal_master = fs::read(&f0.wal).unwrap();

    for i in 0..wal_master.len() {
        let f = fx();
        fs::write(&f.cp, &cp_master).unwrap();
        let mut data = wal_master.clone();
        data[i] ^= 0xFF;
        fs::write(&f.wal, &data).unwrap();
        let (h, _, report) = open_recovering(&f.cp)
            .unwrap_or_else(|e| panic!("open must not fail on bit flip at {i}: {e}"));
        // Checkpoint content survives any WAL damage
        assert!(present(&h, A), "flip at byte {i}");
        // Whatever happened is classified, not silently ignored: replay
        // count can only shrink vs. the pristine 3 frames
        assert!(report.frames_replayed <= 3, "flip at byte {i}");
    }
}

#[test]
fn t4_bitflip_fuzz_checkpoint() {
    let f0 = fx();
    build_v2_state(&f0, false);
    let cp_master = fs::read(&f0.cp).unwrap();
    let wal_master = fs::read(&f0.wal).unwrap();

    for i in 0..cp_master.len() {
        let f = fx();
        let mut data = cp_master.clone();
        data[i] ^= 0xFF;
        fs::write(&f.cp, &data).unwrap();
        fs::write(&f.wal, &wal_master).unwrap();
        let (h, _, report) = open_recovering(&f.cp)
            .unwrap_or_else(|e| panic!("open must not fail on bit flip at {i}: {e}"));
        // The CRC covers the header prefix (magic/version flips fail their
        // own checks) and the body: every single-byte flip must quarantine,
        // and the WAL tail must still be recovered.
        assert_eq!(
            report.checkpoint_state,
            CheckpointState::Quarantined,
            "flip at byte {i} must not pass validation"
        );
        assert!(present(&h, C), "WAL recovery after quarantine, flip at {i}");
    }
}

#[test]
fn t4_applied_seq_bitflip_is_detected() {
    // applied_seq drives the replay filter: an unprotected flip raising it
    // would silently skip live WAL frames. The header CRC must catch it.
    let f = fx();
    build_v2_state(&f, false);
    let mut data = fs::read(&f.cp).unwrap();
    data[14] ^= 0xFF; // high-ish byte of applied_seq → huge value
    fs::write(&f.cp, &data).unwrap();

    let (h, _, report) = open_recovering(&f.cp).unwrap();
    assert_eq!(report.checkpoint_state, CheckpointState::Quarantined);
    assert!(report.data_loss_suspected());
    // WAL frames are replayed, not silently skipped by the corrupt seq
    assert_contents(&h, &[C, D, E], &[A, B]);
    assert_eq!(report.frames_replayed, 3);
}

// ---------------------------------------------------------------------------
// T5: v1 → v2 migration (§7), including every crash residue
// ---------------------------------------------------------------------------

fn v1_checkpoint_bytes(h: &UserHistory) -> Vec<u8> {
    let mut buf = vec![b'L', b'X', b'U', b'D', 1u8];
    buf.extend_from_slice(&bincode::serialize(&h.to_data()).unwrap());
    buf
}

fn v1_wal_bytes(entries: &[(Vec<(String, String)>, u64)]) -> Vec<u8> {
    let mut buf = Vec::new();
    for (segments, ts) in entries {
        let payload = bincode::serialize(&LegacyWalEntry {
            segments: segments.clone(),
            timestamp: *ts,
        })
        .unwrap();
        buf.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        buf.extend_from_slice(&crc32fast::hash(&payload).to_le_bytes());
        buf.extend_from_slice(&payload);
    }
    buf
}

fn write_v1_state(f: &Fx) -> Vec<u8> {
    let mut h = UserHistory::new();
    h.record_at(&seg(A), T0);
    let cp_bytes = v1_checkpoint_bytes(&h);
    fs::write(&f.cp, &cp_bytes).unwrap();
    fs::write(&f.wal, v1_wal_bytes(&[(seg(B), T0 + 1), (seg(C), T0 + 2)])).unwrap();
    cp_bytes
}

#[test]
fn t5_full_migration() {
    let f = fx();
    let v1_bytes = write_v1_state(&f);

    let (h, mut wal, report) = open_recovering(&f.cp).unwrap();
    assert_contents(&h, &[A, B, C], &[D, E]);
    assert_eq!(
        frequency(&h, A),
        1,
        "checkpoint + WAL must not double-apply"
    );
    assert!(report.migrated_from_v1);
    assert_eq!(report.checkpoint_state, CheckpointState::Migrated);
    assert_eq!(report.wal_state, WalState::Reinitialized);
    assert_eq!(report.frames_replayed, 2);
    assert!(report.compaction_recommended);
    assert!(!report.data_loss_suspected(), "migration is not data loss");

    // Commit artifacts: v2 checkpoint, header-only WAL, v1 rescue backup
    let cp_now = fs::read(&f.cp).unwrap();
    assert_eq!(cp_now[4], 2, "checkpoint must be v2 after migration");
    assert_eq!(fs::read(&f.wal).unwrap(), WAL_HEADER.to_vec());
    assert_eq!(
        fs::read(v1_backup_path(&f.cp)).unwrap(),
        v1_bytes,
        ".v1.bak must preserve the original v1 bytes"
    );

    // Migration is one-shot: the next open is clean with identical content
    let (h2, _, r2) = open_recovering(&f.cp).unwrap();
    assert_contents(&h2, &[A, B, C], &[]);
    assert!(r2.is_clean());

    // And the new sequence space starts at 1
    assert_eq!(wal.append(&seg(D), T0 + 3).unwrap(), 1);
}

#[test]
fn t5_residue_v2_checkpoint_with_v1_wal() {
    // §7 crash between step 4 (v2 checkpoint rename) and step 5 (WAL
    // reinit): the v2 checkpoint already contains the v1 WAL's content, so
    // replaying it would double-apply. It must be discarded unread.
    let f = fx();
    write_v1_state(&f);
    let v1_wal = fs::read(&f.wal).unwrap();
    let (_, _, first) = open_recovering(&f.cp).unwrap();
    assert!(first.migrated_from_v1);
    // Reconstruct the residue: put the v1 WAL back next to the v2 checkpoint
    fs::write(&f.wal, &v1_wal).unwrap();

    let (h, _, report) = open_recovering(&f.cp).unwrap();
    assert_eq!(report.wal_state, WalState::LegacyDiscarded);
    assert!(!report.migrated_from_v1);
    assert_contents(&h, &[A, B, C], &[]);
    assert_eq!(frequency(&h, B), 1, "discard, not double-apply");
    assert_eq!(fs::read(&f.wal).unwrap(), WAL_HEADER.to_vec());
}

#[test]
fn t5_migration_idempotent_from_pristine_v1() {
    // §7 crash before step 4 leaves the v1 files untouched (reads + backup
    // copy only) — rerunning from the exact same input must converge.
    let f = fx();
    write_v1_state(&f);
    let (h1, _, _) = open_recovering(&f.cp).unwrap();
    let (h2, _, r2) = open_recovering(&f.cp).unwrap();
    assert!(r2.is_clean());
    assert_eq!(frequency(&h1, A), frequency(&h2, A));
    assert_eq!(frequency(&h1, C), frequency(&h2, C));
}

#[test]
fn t5_missing_checkpoint_with_v1_wal() {
    // A fresh v1 install that never reached its first compaction has a WAL
    // but no checkpoint. Its learnings must survive migration.
    let f = fx();
    fs::write(&f.wal, v1_wal_bytes(&[(seg(B), T0), (seg(C), T0 + 1)])).unwrap();
    let (h, _, report) = open_recovering(&f.cp).unwrap();
    assert_contents(&h, &[B, C], &[A]);
    assert!(report.migrated_from_v1);
    assert_eq!(report.checkpoint_state, CheckpointState::Missing);
    assert!(f.cp.exists(), "v2 checkpoint written by migration");
    assert!(
        !v1_backup_path(&f.cp).exists(),
        "no v1 checkpoint to back up"
    );
    let (h2, _, r2) = open_recovering(&f.cp).unwrap();
    assert!(r2.is_clean());
    assert_contents(&h2, &[B, C], &[]);
}

#[test]
fn t5_corrupt_v1_checkpoint_with_v1_wal() {
    // The v1 checkpoint is quarantined; the v1 WAL is still migration input
    // (nothing was loaded, so replaying it cannot double-apply).
    let f = fx();
    write_v1_state(&f);
    let mut cp = fs::read(&f.cp).unwrap();
    let idx = cp.len() - 3;
    cp[idx] ^= 0xFF;
    fs::write(&f.cp, &cp).unwrap();

    let (h, _, report) = open_recovering(&f.cp).unwrap();
    assert_contents(&h, &[B, C], &[A]);
    assert_eq!(report.checkpoint_state, CheckpointState::Quarantined);
    assert!(report.migrated_from_v1);
    assert!(report.data_loss_suspected());
}

#[test]
fn t5_v1_checkpoint_with_v2_wal_mixed_state() {
    // A failed migration commit with no legacy WAL leaves a v1 checkpoint in
    // place and an appendable (fresh) v2 WAL; later commits append v2 frames
    // beside the v1 checkpoint. Recovery must replay them, migrate, and
    // cover them — not assert or double-apply.
    let f = fx();
    let mut h = UserHistory::new();
    h.record_at(&seg(A), T0);
    fs::write(&f.cp, v1_checkpoint_bytes(&h)).unwrap();
    let mut wal = HistoryWal::new(&f.cp);
    wal.append(&seg(B), T0 + 1).unwrap();
    wal.append(&seg(C), T0 + 2).unwrap();

    let (h2, _, report) = open_recovering(&f.cp).unwrap();
    assert_contents(&h2, &[A, B, C], &[]);
    assert!(report.migrated_from_v1);
    assert_eq!(report.frames_replayed, 2);
    assert_eq!(fs::read(&f.cp).unwrap()[4], 2, "checkpoint migrated to v2");

    // The migrated checkpoint covers the replayed v2 frames: the retained
    // WAL skips them on the next open (no double apply)
    let (h3, _, r3) = open_recovering(&f.cp).unwrap();
    assert_eq!(frequency(&h3, B), 1);
    assert_eq!(r3.frames_skipped, 2);
    assert_eq!(r3.frames_replayed, 0);
}

#[test]
fn wal_corrupted_magic_quarantined_not_discarded() {
    // A bit flip in the WAL magic bytes classifies the file as Legacy. Since
    // it parses as zero valid v1 frames it must be quarantined (user-visible
    // loss), not silently discarded as v1 migration residue.
    let f = fx();
    build_v2_state(&f, false);
    let mut data = fs::read(&f.wal).unwrap();
    data[0] ^= 0xFF;
    fs::write(&f.wal, &data).unwrap();

    let (h, _, report) = open_recovering(&f.cp).unwrap();
    assert_eq!(report.wal_state, WalState::Quarantined);
    assert!(report.data_loss_suspected());
    assert_contents(&h, &[A, B], &[C, D, E]);
    assert!(!f.wal.exists(), "unreadable WAL renamed away");
    assert_eq!(quarantine_count(&f), 1);
}

#[cfg(unix)]
#[test]
fn t5_migration_commit_failure_leaves_v1_intact_and_freezes_appends() {
    use std::os::unix::fs::PermissionsExt;
    let f = fx();
    let v1_cp = write_v1_state(&f);
    let v1_wal = fs::read(&f.wal).unwrap();
    let dir = f.cp.parent().unwrap().to_path_buf();

    // Read-only directory: the migration checkpoint write (tmp create) fails
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).unwrap();
    // Probe: root / CAP_DAC_OVERRIDE environments (containerized CI) ignore
    // directory permissions, so the failure cannot be injected this way —
    // skip rather than assert a failure that will not happen.
    if fs::write(dir.join("probe"), b"x").is_ok() {
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        eprintln!("skipping: directory permissions are not enforced here");
        return;
    }
    let result = open_recovering(&f.cp);
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();

    let (h, mut wal, report) = result.unwrap();
    // Learning continues in memory this session
    assert_contents(&h, &[A, B, C], &[]);
    assert!(!report.migrated_from_v1, "commit did not happen");
    // v1 files are untouched so the next startup retries migration
    assert_eq!(fs::read(&f.cp).unwrap(), v1_cp);
    assert_eq!(fs::read(&f.wal).unwrap(), v1_wal);
    // Appends are frozen: v2 frames after v1 bytes would be unreadable
    assert!(wal.is_frozen());
    assert!(wal.append(&seg(D), T0 + 5).is_err());
    // The report must not describe this as an ordinary startup: every field
    // the pre-existing states carry (checkpoint_state, wal_state,
    // migrated_from_v1) stays at its healthy value here, so without the two
    // derived flags `is_clean()` answers true and no consumer branches.
    assert!(report.migration_failed);
    assert!(report.appends_frozen);
    assert!(!report.is_clean());
    // Not data loss: nothing has been lost, the v1 files are intact.
    assert!(!report.data_loss_suspected());
    assert!(report.compaction_recommended);
    // And the retry succeeds once the environment recovers
    let (h2, _, r2) = open_recovering(&f.cp).unwrap();
    assert!(r2.migrated_from_v1);
    assert!(!r2.migration_failed);
    assert_contents(&h2, &[A, B, C], &[]);
}

/// The other half of a failed migration commit: a v1 checkpoint with no
/// legacy WAL beside it. `legacy_wal_consumed` is false, so the `Err` branch
/// freezes nothing and the WAL stays perfectly appendable — which means a
/// report derived from `is_frozen()` sees nothing wrong. Deriving from the
/// migration's own outcome is what covers this case, and covering it also
/// schedules the heal: a compaction writes exactly the v2 checkpoint whose
/// write just failed.
#[cfg(unix)]
#[test]
fn t5_migration_commit_failure_without_legacy_wal_is_still_reported() {
    use std::os::unix::fs::PermissionsExt;
    let f = fx();
    let mut seed = UserHistory::new();
    seed.record_at(&seg(A), T0);
    let v1_cp = v1_checkpoint_bytes(&seed);
    fs::write(&f.cp, &v1_cp).unwrap();
    // No WAL at all — the state a v1 install reaches after a clean shutdown
    // whose last compaction truncated the log.
    assert!(!f.wal.exists());
    let dir = f.cp.parent().unwrap().to_path_buf();

    fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).unwrap();
    if fs::write(dir.join("probe"), b"x").is_ok() {
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        eprintln!("skipping: directory permissions are not enforced here");
        return;
    }
    let result = open_recovering(&f.cp);
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();

    let (h, wal, report) = result.unwrap();
    assert_contents(&h, &[A], &[]);
    assert!(!report.migrated_from_v1, "commit did not happen");
    assert_eq!(fs::read(&f.cp).unwrap(), v1_cp, "v1 checkpoint kept");

    assert!(report.migration_failed);
    // The distinguishing feature of this half: appends are fine.
    assert!(!wal.is_frozen());
    assert!(!report.appends_frozen);
    // Still not a clean startup.
    assert!(!report.is_clean());
    assert!(!report.data_loss_suspected());
    // Deliberately no in-session retry: a compaction is not the migration
    // (no `.v1.bak`, no `Migrated` state), so using one as the retry would
    // overwrite the v1 checkpoint on exactly the path where the commit is
    // already failing. The next launch re-attempts properly. Appends are
    // fine here, so nothing else is degraded enough to warrant one either.
    assert!(
        !report.compaction_recommended,
        "a failed migration must not schedule a generic compaction"
    );

    let (h2, _, r2) = open_recovering(&f.cp).unwrap();
    assert!(r2.migrated_from_v1);
    assert!(!r2.migration_failed);
    assert_contents(&h2, &[A], &[]);
}

/// Same failure with a `.v1.bak` already on disk: the backup is best-effort
/// and changes nothing about scheduling. Pinned so a future change cannot
/// quietly reintroduce a compaction-as-migration retry on either variant.
#[cfg(unix)]
#[test]
fn t5_migration_commit_failure_schedules_nothing_even_with_a_backup() {
    use std::os::unix::fs::PermissionsExt;
    let f = fx();
    let mut seed = UserHistory::new();
    seed.record_at(&seg(A), T0);
    let v1_cp = v1_checkpoint_bytes(&seed);
    fs::write(&f.cp, &v1_cp).unwrap();
    // A rescue copy an earlier startup managed to write before the disk went
    // bad.
    fs::write(v1_backup_path(&f.cp), &v1_cp).unwrap();
    let dir = f.cp.parent().unwrap().to_path_buf();

    fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).unwrap();
    if fs::write(dir.join("probe"), b"x").is_ok() {
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        eprintln!("skipping: directory permissions are not enforced here");
        return;
    }
    let result = open_recovering(&f.cp);
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();

    let (_, _, report) = result.unwrap();
    assert!(report.migration_failed);
    assert!(!report.is_clean());
    assert!(!report.compaction_recommended);
}

// ---------------------------------------------------------------------------
// T9: seq validation
// ---------------------------------------------------------------------------

fn raw_v2_frame(seq: u64, record: &WalRecord) -> Vec<u8> {
    let payload = bincode::serialize(record).unwrap();
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(&seq.to_le_bytes());
    hasher.update(&payload);
    let crc = hasher.finalize();
    let mut frame = Vec::new();
    frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    frame.extend_from_slice(&crc.to_le_bytes());
    frame.extend_from_slice(&seq.to_le_bytes());
    frame.extend_from_slice(&payload);
    frame
}

fn committed(pair: (&str, &str), ts: u64) -> WalRecord {
    WalRecord::Committed {
        segments: seg(pair),
        timestamp: ts,
    }
}

#[test]
fn t9_non_monotonic_seq_stops_and_repairs() {
    let f = fx();
    let mut data = WAL_HEADER.to_vec();
    data.extend(raw_v2_frame(1, &committed(A, T0)));
    data.extend(raw_v2_frame(2, &committed(B, T0 + 1)));
    let good_len = data.len();
    // Duplicate seq: the frame stream is not trustworthy past this point
    data.extend(raw_v2_frame(2, &committed(C, T0 + 2)));
    fs::write(&f.wal, &data).unwrap();

    let (h, _, report) = open_recovering(&f.cp).unwrap();
    assert_contents(&h, &[A, B], &[C]);
    assert_eq!(report.wal_state, WalState::TailRepaired);
    assert_eq!(fs::read(&f.wal).unwrap().len(), good_len);
}

#[test]
fn t9_decreasing_seq_stops_and_repairs() {
    let f = fx();
    let mut data = WAL_HEADER.to_vec();
    data.extend(raw_v2_frame(5, &committed(A, T0)));
    data.extend(raw_v2_frame(3, &committed(B, T0 + 1)));
    fs::write(&f.wal, &data).unwrap();

    let (h, _, report) = open_recovering(&f.cp).unwrap();
    assert_contents(&h, &[A], &[B]);
    assert_eq!(report.wal_state, WalState::TailRepaired);
}

#[test]
fn t9_seq_gap_is_tolerated() {
    // Gaps arise legitimately when an append fails after consuming a seq
    // (§5.2) — replay warns and continues.
    let f = fx();
    let mut data = WAL_HEADER.to_vec();
    data.extend(raw_v2_frame(1, &committed(A, T0)));
    data.extend(raw_v2_frame(5, &committed(B, T0 + 1)));
    data.extend(raw_v2_frame(9, &committed(C, T0 + 2)));
    fs::write(&f.wal, &data).unwrap();

    let (h, mut wal, report) = open_recovering(&f.cp).unwrap();
    assert_contents(&h, &[A, B, C], &[]);
    assert_eq!(report.wal_state, WalState::Clean);
    assert_eq!(report.frames_replayed, 3);
    // next_seq resumes past the highest seen seq
    assert_eq!(wal.append(&seg(D), T0 + 3).unwrap(), 10);
}

// ---------------------------------------------------------------------------
// Supporting protocol pieces shipped in PR1
// ---------------------------------------------------------------------------

#[test]
fn tombstone_replay_removes_entries() {
    // PR1 ships the Tombstone reader ahead of the PR2 writer so mixed
    // versions stay forward-compatible.
    let f = fx();
    let mut wal = HistoryWal::new(&f.cp);
    wal.append_record(&committed(A, T0)).unwrap();
    wal.append_record(&committed(B, T0)).unwrap();
    wal.append_record(&WalRecord::Tombstone {
        segments: seg(A),
        timestamp: T0 + 1,
    })
    .unwrap();

    let (h, _, report) = open_recovering(&f.cp).unwrap();
    assert_contents(&h, &[B], &[A]);
    assert_eq!(report.frames_replayed, 3);
}

#[test]
fn replayed_tombstone_recommends_startup_scrub() {
    // A crash between a Tombstone append and its async scrub: the loaded
    // checkpoint still contains the deleted strings and the WAL carries an
    // uncovered Tombstone. Replay applies the deletion but skips nothing
    // (frames_skipped == 0), so the startup scrub must be driven by the
    // replayed-deletion signal — otherwise the deleted input lingers in the
    // old checkpoint until an unrelated compaction (§5.4 privacy).
    let f = fx();
    // Checkpoint (applied_seq = 0) that still contains A.
    let mut h = UserHistory::new();
    h.record_at(&seg(A), T0);
    h.save(&f.cp).unwrap();
    // WAL with an uncovered Tombstone (seq 1 > applied_seq 0).
    let mut wal = HistoryWal::new(&f.cp);
    wal.append_record(&WalRecord::Tombstone {
        segments: seg(A),
        timestamp: T0 + 1,
    })
    .unwrap();
    drop(wal);

    let (h, _, report) = open_recovering(&f.cp).unwrap();
    assert_contents(&h, &[], &[A]); // deletion applied
    assert_eq!(report.checkpoint_state, CheckpointState::Loaded);
    assert_eq!(report.frames_replayed, 1);
    assert_eq!(report.frames_skipped, 0);
    assert!(
        report.replayed_deletion,
        "a replayed tombstone was observed"
    );
    assert!(
        report.compaction_recommended,
        "delete-residue must schedule a startup scrub even with no skipped frames"
    );
}

#[test]
fn truncate_covered_requires_checkpoint_coverage() {
    let f = fx();
    let mut wal = HistoryWal::new(&f.cp);
    for pair in [A, B, C] {
        wal.append(&seg(pair), T0).unwrap();
    }
    let before = fs::read(&f.wal).unwrap();

    // applied_seq = 2 < last appended seq 3: refuse (frame C would be lost)
    assert!(!wal.truncate_covered(2).unwrap());
    assert_eq!(fs::read(&f.wal).unwrap(), before);
    assert_eq!(wal.entry_count(), 3);

    // applied_seq = 3 covers everything: truncate to header
    assert!(wal.truncate_covered(3).unwrap());
    assert_eq!(fs::read(&f.wal).unwrap(), WAL_HEADER.to_vec());
    assert_eq!(wal.entry_count(), 0);
}

#[test]
fn wal_stub_reinitialized() {
    // 0-7 bytes = residue of a crash mid-truncation: reinit, log-only
    for stub in [&b""[..], &b"LXW"[..], &b"LXWL\x02\x00\x00"[..]] {
        let f = fx();
        fs::write(&f.wal, stub).unwrap();
        let (_, mut wal, report) = open_recovering(&f.cp).unwrap();
        assert_eq!(report.wal_state, WalState::Reinitialized);
        assert!(!report.data_loss_suspected());
        assert_eq!(fs::read(&f.wal).unwrap(), WAL_HEADER.to_vec());
        wal.append(&seg(A), T0).unwrap();
    }
}

#[test]
fn append_onto_stub_recreates_header() {
    // A 1-7 byte stub (torn truncation) must not receive frames after the
    // foreign bytes: the lazy open recreates the file as header-only first.
    let f = fx();
    fs::write(&f.wal, b"LXW").unwrap();
    let mut wal = HistoryWal::new(&f.cp);
    wal.append(&seg(A), T0).unwrap();

    let data = fs::read(&f.wal).unwrap();
    assert_eq!(&data[..WAL_HEADER_LEN], &WAL_HEADER);
    let (h, _, report) = open_recovering(&f.cp).unwrap();
    assert!(present(&h, A), "appended frame must be replayable");
    assert_eq!(report.frames_replayed, 1);
    assert_eq!(report.wal_state, WalState::Clean);
}

#[test]
fn append_refused_on_corrupt_header() {
    // A live handle must not append after unreadable header bytes: the next
    // startup would quarantine the whole file, new frames included. The
    // failed append freezes the WAL; truncation heals it.
    let f = fx();
    fs::write(&f.wal, b"XXXXXXXX-garbage").unwrap();
    let mut wal = HistoryWal::new(&f.cp);
    assert!(wal.append(&seg(A), T0).is_err());
    assert!(wal.is_frozen());
    wal.truncate_wal().unwrap();
    assert!(!wal.is_frozen());
    wal.append(&seg(A), T0).unwrap();
    let (h, _, report) = open_recovering(&f.cp).unwrap();
    assert!(present(&h, A));
    assert_eq!(report.frames_replayed, 1);
}

#[test]
fn wal_unknown_version_quarantined() {
    let f = fx();
    build_v2_state(&f, false);
    let mut data = fs::read(&f.wal).unwrap();
    data[4] = 9; // future version
    fs::write(&f.wal, &data).unwrap();

    let (h, wal, report) = open_recovering(&f.cp).unwrap();
    assert_eq!(report.wal_state, WalState::Quarantined);
    assert!(report.data_loss_suspected());
    assert_contents(&h, &[A, B], &[C, D, E]);
    assert!(!f.wal.exists(), "unreadable WAL renamed away");
    assert_eq!(quarantine_count(&f), 1);
    // A quarantine that succeeded leaves a fresh, appendable WAL: the state
    // is data loss, not degraded persistence. Folding "frozen" into
    // `wal_state` would have conflated the two and reported this healthy
    // file as memory-only.
    assert!(!wal.is_frozen());
    assert!(!report.appends_frozen);
}

#[test]
fn checkpoint_unknown_version_quarantined_not_hard_error() {
    // §7 version-bump policy: unknown versions must fall into quarantine +
    // fallback, never a startup failure (item 1's permanent-stop path).
    let f = fx();
    build_v2_state(&f, false);
    let mut data = fs::read(&f.cp).unwrap();
    data[4] = 9;
    fs::write(&f.cp, &data).unwrap();

    let (h, _, report) = open_recovering(&f.cp).unwrap();
    assert_eq!(report.checkpoint_state, CheckpointState::Quarantined);
    assert_contents(&h, &[C, D, E], &[A, B]);
}

#[test]
fn strict_open_errors_on_corruption_and_never_mutates() {
    // The lextool path: corrupt input is an error, and the live user's
    // files are never renamed/truncated by an audit.
    let f = fx();
    build_v2_state(&f, false);
    corrupt_cp(&f.cp);
    let cp_bytes = fs::read(&f.cp).unwrap();
    let wal_bytes = fs::read(&f.wal).unwrap();

    assert!(UserHistory::open(&f.cp).is_err());
    assert!(super::wal::open_with_wal(&f.cp).is_err());
    assert_eq!(fs::read(&f.cp).unwrap(), cp_bytes);
    assert_eq!(fs::read(&f.wal).unwrap(), wal_bytes);
    assert_eq!(quarantine_count(&f), 0);
}

#[test]
fn strict_replay_errors_on_unreadable_wal_formats() {
    // Whole-file format mismatches must fail loudly for offline tooling:
    // a silent empty replay would make audits plausible-but-incomplete.
    // (Torn tails keep the silent-prefix semantics — expected residue.)

    // Unknown WAL version
    let f = fx();
    build_v2_state(&f, false);
    let mut data = fs::read(&f.wal).unwrap();
    data[4] = 9;
    fs::write(&f.wal, &data).unwrap();
    assert!(super::wal::open_with_wal(&f.cp).is_err());

    // Corrupted magic (classifies Legacy but parses as zero v1 frames)
    let f = fx();
    build_v2_state(&f, false);
    let mut data = fs::read(&f.wal).unwrap();
    data[0] ^= 0xFF;
    fs::write(&f.wal, &data).unwrap();
    assert!(super::wal::open_with_wal(&f.cp).is_err());

    // Genuine migration residue (readable v1 WAL beside a v2 checkpoint)
    // stays a silent skip: the checkpoint already contains its content.
    let f = fx();
    build_v2_state(&f, false);
    fs::write(&f.wal, v1_wal_bytes(&[(seg(B), T0)])).unwrap();
    let (h, _) = super::wal::open_with_wal(&f.cp).unwrap();
    assert_eq!(frequency(&h, B), 1, "from checkpoint only, not re-applied");

    // Corrupted magic with a *missing* checkpoint must also fail loudly:
    // the whole uncheckpointed tail would otherwise be silently ignored.
    let f = fx();
    build_v2_state(&f, false);
    fs::remove_file(&f.cp).unwrap();
    let mut data = fs::read(&f.wal).unwrap();
    data[0] ^= 0xFF;
    fs::write(&f.wal, &data).unwrap();
    assert!(super::wal::open_with_wal(&f.cp).is_err());
}

#[test]
fn quarantine_rotation_keeps_newest_three() {
    let f = fx();
    build_v2_state(&f, false);
    // Older quarantine residue from previous corruption events
    for ts in [100, 101, 102, 103, 104] {
        fs::write(
            f.cp.with_file_name(format!("user_history.lxud.corrupt-{ts}")),
            b"old",
        )
        .unwrap();
    }
    corrupt_cp(&f.cp);
    let (_, _, report) = open_recovering(&f.cp).unwrap();
    assert_eq!(report.quarantined_paths.len(), 1);
    assert!(
        report.quarantined_paths[0].exists(),
        "newest must survive rotation"
    );
    assert_eq!(quarantine_count(&f), 3, "rotation keeps the newest 3");
}

/// Mock WalIo for write/sync failure injection (the full syscall-order
/// matrix is T6, PR2 — these cover the failure semantics PR1 relies on).
struct FailingIo {
    appended: Vec<u8>,
    fail_append: bool,
    fail_barrier: bool,
}

impl super::wal::WalIo for FailingIo {
    fn append(&mut self, buf: &[u8]) -> std::io::Result<()> {
        if self.fail_append {
            return Err(std::io::Error::other("injected append failure"));
        }
        self.appended.extend_from_slice(buf);
        Ok(())
    }
    fn sync_barrier(&mut self) -> std::io::Result<()> {
        if self.fail_barrier {
            return Err(std::io::Error::other("injected barrier failure"));
        }
        Ok(())
    }
    fn sync_full(&mut self) -> std::io::Result<()> {
        Ok(())
    }
    fn truncate_to_header(&mut self) -> std::io::Result<()> {
        self.appended.clear();
        Ok(())
    }
}

/// Records barrier calls (durability-cadence tests).
#[derive(Default)]
struct CountingIo {
    barriers: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl super::wal::WalIo for CountingIo {
    fn append(&mut self, _buf: &[u8]) -> std::io::Result<()> {
        Ok(())
    }
    fn sync_barrier(&mut self) -> std::io::Result<()> {
        self.barriers
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
    fn sync_full(&mut self) -> std::io::Result<()> {
        Ok(())
    }
    fn truncate_to_header(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn barrier_reissued_on_first_append_after_replay() {
    // Frames replayed from a previous process may include an unbarriered
    // tail; the first new append must issue a barrier so the documented
    // power-loss bound (<= BARRIER_EVERY_FRAMES commits) survives restarts.
    let f = fx();
    {
        let mut wal = HistoryWal::new(&f.cp);
        for _ in 0..3 {
            wal.append(&seg(A), T0).unwrap();
        }
    }
    let barriers = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let io = CountingIo {
        barriers: std::sync::Arc::clone(&barriers),
    };
    let mut h = UserHistory::new();
    let mut wal = HistoryWal::with_io(&f.cp, Box::new(io));
    wal.replay(&mut h, super::wal::CheckpointOrigin::Missing)
        .unwrap();
    wal.append(&seg(B), T0 + 1).unwrap();
    assert_eq!(
        barriers.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "first append after replaying existing frames must barrier"
    );

    // A fresh (empty) WAL keeps the normal cadence: no barrier on the first
    // append.
    let f2 = fx();
    let barriers2 = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let io2 = CountingIo {
        barriers: std::sync::Arc::clone(&barriers2),
    };
    let mut wal2 = HistoryWal::with_io(&f2.cp, Box::new(io2));
    wal2.append(&seg(A), T0).unwrap();
    assert_eq!(barriers2.load(std::sync::atomic::Ordering::SeqCst), 0);
}

/// Barrier that fails the first N attempts (transient-failure cadence test).
struct FlakyBarrierIo {
    attempts: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    fail_first: usize,
}

impl super::wal::WalIo for FlakyBarrierIo {
    fn append(&mut self, _buf: &[u8]) -> std::io::Result<()> {
        Ok(())
    }
    fn sync_barrier(&mut self) -> std::io::Result<()> {
        let n = self
            .attempts
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if n < self.fail_first {
            Err(std::io::Error::other("transient barrier failure"))
        } else {
            Ok(())
        }
    }
    fn sync_full(&mut self) -> std::io::Result<()> {
        Ok(())
    }
    fn truncate_to_header(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn barrier_retries_on_next_append_after_transient_failure() {
    // A failed barrier must not defer the retry by another full interval —
    // the counter stays saturated so the very next append retries.
    let f = fx();
    let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let io = FlakyBarrierIo {
        attempts: std::sync::Arc::clone(&attempts),
        fail_first: 1,
    };
    let mut wal = HistoryWal::with_io(&f.cp, Box::new(io));
    for i in 0..50 {
        wal.append(&seg(A), T0 + i).unwrap();
    }
    // 50th append attempted the barrier and it failed
    assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 1);
    // 51st append retries immediately (and succeeds, resetting the cadence)
    wal.append(&seg(B), T0 + 50).unwrap();
    assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 2);
    // Cadence resumed: no further attempt until 50 more frames
    for i in 0..49 {
        wal.append(&seg(A), T0 + 51 + i).unwrap();
    }
    assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 2);
    wal.append(&seg(B), T0 + 100).unwrap();
    assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 3);
}

#[test]
fn committed_barrier_failure_still_returns_seq() {
    // A failed barrier after a successful frame write must not surface as
    // an append failure: the frame is durable enough to be replayed, so the
    // caller has to count it as covered (advance applied_seq) or a crash
    // would double-apply it.
    let f = fx();
    let io = FailingIo {
        appended: Vec::new(),
        fail_append: false,
        fail_barrier: true,
    };
    let mut wal = HistoryWal::with_io(&f.cp, Box::new(io));
    for i in 1..=60u64 {
        // Frame 50 crosses the barrier interval with a failing barrier
        assert_eq!(wal.append(&seg(A), T0 + i).unwrap(), i, "append {i}");
    }
    assert_eq!(wal.entry_count(), 60);
    assert!(!wal.is_frozen());
}

#[test]
fn append_write_failure_freezes_until_truncate() {
    // A failed frame write can leave a partial frame at the tail; further
    // appends would land after unreadable bytes and be lost to replay.
    let f = fx();
    let io = FailingIo {
        appended: Vec::new(),
        fail_append: true,
        fail_barrier: false,
    };
    let mut wal = HistoryWal::with_io(&f.cp, Box::new(io));
    assert!(wal.append(&seg(A), T0).is_err());
    assert!(wal.is_frozen(), "append failure must freeze the WAL");
    assert!(
        wal.append(&seg(B), T0 + 1).is_err(),
        "appends stay rejected while frozen"
    );
    // The post-checkpoint truncation re-establishes appendable form
    wal.truncate_wal().unwrap();
    assert!(!wal.is_frozen());
}

// ---------------------------------------------------------------------------
// T6: syscall ordering / mid-write failure (WalIo seam, design §11)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum IoOp {
    Append,
    Barrier,
    Full,
    Truncate,
}

/// Records the WalIo call order; optionally fails sync_full.
struct OrderIo {
    ops: std::sync::Arc<std::sync::Mutex<Vec<IoOp>>>,
    fail_full: bool,
}

impl super::wal::WalIo for OrderIo {
    fn append(&mut self, _buf: &[u8]) -> std::io::Result<()> {
        self.ops.lock().unwrap().push(IoOp::Append);
        Ok(())
    }
    fn sync_barrier(&mut self) -> std::io::Result<()> {
        self.ops.lock().unwrap().push(IoOp::Barrier);
        Ok(())
    }
    fn sync_full(&mut self) -> std::io::Result<()> {
        if self.fail_full {
            return Err(std::io::Error::other("injected full-sync failure"));
        }
        self.ops.lock().unwrap().push(IoOp::Full);
        Ok(())
    }
    fn truncate_to_header(&mut self) -> std::io::Result<()> {
        self.ops.lock().unwrap().push(IoOp::Truncate);
        Ok(())
    }
}

fn tombstone(pair: (&str, &str)) -> WalRecord {
    WalRecord::Tombstone {
        segments: seg(pair),
        timestamp: T0,
    }
}

#[test]
fn t6_tombstone_full_sync_follows_write_before_return() {
    // §5.4/§6: a Tombstone's durability contract is a full flush after the
    // frame write, before append_record returns — the deletion must not be
    // resurrectable once the user's gesture completes.
    let f = fx();
    let ops = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let io = OrderIo {
        ops: std::sync::Arc::clone(&ops),
        fail_full: false,
    };
    let mut wal = HistoryWal::with_io(&f.cp, Box::new(io));
    wal.append_record(&tombstone(A)).unwrap();
    assert_eq!(
        *ops.lock().unwrap(),
        vec![IoOp::Append, IoOp::Full],
        "tombstone must be written, then fully flushed, before returning"
    );
}

#[test]
fn t6_tombstone_sync_failure_carries_seq_and_does_not_freeze() {
    // The frame is validly on disk; only its flush failed. The caller must
    // receive the real seq (covering the frame keeps conditional truncation
    // live) and the WAL must stay appendable.
    let f = fx();
    let ops = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let io = OrderIo {
        ops: std::sync::Arc::clone(&ops),
        fail_full: true,
    };
    let mut wal = HistoryWal::with_io(&f.cp, Box::new(io));
    match wal.append_record(&tombstone(A)) {
        Err(super::wal::AppendError::SyncFailed { seq, .. }) => assert_eq!(seq, 1),
        other => panic!("expected SyncFailed, got {other:?}"),
    }
    assert!(!wal.is_frozen(), "a sync failure must not freeze the WAL");
    assert_eq!(wal.entry_count(), 1, "the frame is in the file");
    assert_eq!(wal.last_appended_seq(), 1);
    // The next append still works and gets the next seq.
    assert_eq!(wal.append(&seg(B), T0 + 1).unwrap(), 2);
}

// ---------------------------------------------------------------------------
// T7 (core side): clear-protocol crash residue (§5.5)
// ---------------------------------------------------------------------------

#[test]
fn t7_clear_residue_empty_checkpoint_covers_leftover_wal() {
    // Crash between §5.5 step 3 (empty checkpoint committed) and step 5
    // (WAL truncation): the empty checkpoint's applied_seq equals the
    // highest on-disk seq, so every leftover frame replays as a skip and
    // the history converges to empty — the resurrection window is zero.
    let f = fx();
    let mut wal = HistoryWal::new(&f.cp);
    for (i, p) in [A, B, C].iter().enumerate() {
        wal.append(&seg(*p), T0 + i as u64).unwrap();
    }
    let mut empty = UserHistory::new();
    empty.advance_applied_seq(wal.last_appended_seq());
    empty.save(&f.cp).unwrap();
    drop(wal);

    let (h, mut wal, report) = open_recovering(&f.cp).unwrap();
    assert_contents(&h, &[], &[A, B, C]);
    assert_eq!(report.frames_replayed, 0);
    assert_eq!(
        report.frames_skipped, 3,
        "all frames covered by the empty cp"
    );
    assert_eq!(report.checkpoint_state, CheckpointState::Loaded);
    // Privacy backstop: the leftover (covered) frames still hold input
    // strings; startup compaction truncates them away.
    assert!(report.compaction_recommended);
    // Numbering continues past the cleared generation (§3).
    assert_eq!(wal.append(&seg(D), T0 + 10).unwrap(), 4);
}

// ---------------------------------------------------------------------------
// v1 backup GC (time-based, §9 decision)
// ---------------------------------------------------------------------------

#[test]
fn v1_backup_expires_after_ttl() {
    let f = fx();
    UserHistory::new().save(&f.cp).unwrap();
    let bak = v1_backup_path(&f.cp);

    // Fresh backup survives startup.
    fs::write(&bak, b"v1 bytes").unwrap();
    open_recovering(&f.cp).unwrap();
    assert!(bak.exists(), "fresh backup must survive");

    // Backdate past the 90-day TTL: the next startup removes it.
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(91 * 24 * 60 * 60);
    let file = fs::OpenOptions::new().write(true).open(&bak).unwrap();
    file.set_times(fs::FileTimes::new().set_modified(old))
        .unwrap();
    drop(file);
    open_recovering(&f.cp).unwrap();
    assert!(!bak.exists(), "expired backup must be GCed");
}

#[test]
fn remove_recovery_artifacts_wipes_backup_and_quarantine() {
    let f = fx();
    build_v2_state(&f, false);
    fs::write(v1_backup_path(&f.cp), b"v1 bytes").unwrap();
    fs::write(f.cp.with_file_name("user_history.lxud.corrupt-100"), b"x").unwrap();
    fs::write(
        f.cp.with_file_name("user_history.lxud.wal.corrupt-101"),
        b"y",
    )
    .unwrap();
    // Both temp-name generations: v2 appends ".tmp" to the full name; the
    // v1 writer's with_extension("tmp") produced "user_history.tmp", which
    // can hold a full pre-upgrade history copy.
    fs::write(f.cp.with_file_name("user_history.lxud.tmp"), b"t2").unwrap();
    fs::write(f.cp.with_file_name("user_history.tmp"), b"t1").unwrap();

    remove_recovery_artifacts(&f.cp).unwrap();
    assert!(!v1_backup_path(&f.cp).exists());
    assert_eq!(quarantine_count(&f), 0);
    assert!(!f.cp.with_file_name("user_history.lxud.tmp").exists());
    assert!(!f.cp.with_file_name("user_history.tmp").exists());
}

// ---------------------------------------------------------------------------
// T10: the unpersisted-deletion marker (#312). The runtime ledger is
// process-local, so before this the `Io` half went unreported on exactly the
// restart where the entry comes back.
//
// The axis is independent of the checkpoint × WAL matrix above: the marker
// says something about a *previous* session, not about the files just read.
// ---------------------------------------------------------------------------

fn write_marker(f: &Fx, breach: DeletionBreach) {
    deletion_marker::merge_write(&f.cp, breach).unwrap();
}

/// The on-disk encoding of a breach, obtained through the writer so the test
/// never re-spells the format.
fn encoded_marker(breach: DeletionBreach) -> Vec<u8> {
    let dir = tempfile::tempdir().unwrap();
    let cp = dir.path().join("scratch.lxud");
    deletion_marker::merge_write(&cp, breach).unwrap();
    fs::read(deletion_marker::marker_path(&cp)).unwrap()
}

fn marker_bytes(f: &Fx, bytes: &[u8]) {
    fs::write(deletion_marker::marker_path(&f.cp), bytes).unwrap();
}

fn open_report_of(f: &Fx) -> super::recovery::OpenReport {
    let (_, _, report) = open_recovering(&f.cp).unwrap();
    report
}

fn applied_seq_of(f: &Fx) -> u64 {
    let (h, _, _) = open_recovering(&f.cp).unwrap();
    h.applied_seq()
}

#[test]
fn t10_no_marker_is_clean() {
    let f = fx();
    build_v2_state(&f, false);
    let report = open_report_of(&f);
    assert!(!report.deletion_lost);
    assert!(report.is_clean(), "a plain start must stay clean");
}

#[test]
fn t10_lost_marker_reports_and_survives_the_open() {
    let f = fx();
    build_v2_state(&f, false);
    write_marker(&f, DeletionBreach::Lost);

    let report = open_report_of(&f);
    assert!(report.deletion_lost, "a lost deletion must be reported");
    assert!(!report.is_clean());
    // Not folded into the data-loss channel: that one means past learning was
    // lost, this means data survived a deletion.
    assert!(!report.data_loss_suspected());
    // A compaction here would checkpoint the resurrected entry and cover the
    // ledger — i.e. tell the user it is fine. Reported, not healed.
    assert!(!report.compaction_recommended);
    // Consumption is `ack_open_report`, not the open: the report has to
    // outlive the gap between being built and being delivered.
    assert!(
        deletion_marker::marker_path(&f.cp).exists(),
        "open must not consume the only durable trace"
    );
}

#[test]
fn t10_unflushed_witness_pins_the_boundary() {
    // The suppression rule is `witness <= applied_seq`, and the boundary is the
    // whole meaning: seq == applied_seq is the frame that *did* replay. A test
    // that only probed values far from the boundary would survive turning `<=`
    // into `<`.
    let f = fx();
    build_v2_state(&f, false);
    let applied = applied_seq_of(&f);
    assert!(applied > 0, "fixture must reach a non-zero applied_seq");

    write_marker(&f, DeletionBreach::Unflushed { seq: applied });
    let report = open_report_of(&f);
    assert!(
        !report.deletion_lost,
        "a frame the loaded state already includes was applied — nothing is owed"
    );
    // Applied, but out of the page cache: the flush that failed never happened,
    // so this is a live durability problem, not a settled one. Retracting here
    // would be retract-then-persist.
    assert!(report.deletion_pending_checkpoint);
    assert!(
        deletion_marker::marker_path(&f.cp).exists(),
        "only a durable checkpoint may retract; recovery writes none"
    );

    let f2 = fx();
    build_v2_state(&f2, false);
    write_marker(&f2, DeletionBreach::Unflushed { seq: applied + 1 });
    let report = open_report_of(&f2);
    assert!(
        report.deletion_lost,
        "a frame beyond the loaded state never took effect — report it"
    );
    assert!(!report.deletion_pending_checkpoint);
    // Promoted to unconditional: seq numbering is re-based by a WAL reset, so
    // leaving a witness behind would let an unrelated later frame settle a
    // report that is still owed.
    assert_eq!(
        deletion_marker::read(&f2.cp),
        Some(DeletionBreach::Lost),
        "an answered witness must stop depending on a comparison a reset can invalidate"
    );
}

#[test]
fn t10_a_witness_the_checkpoint_covers_is_retracted_outright() {
    // The residue of a crash between a successful `save()` and the unlink that
    // follows it: the deletion IS durable, so the marker is stale. Telling the
    // two apart takes the checkpoint's own applied_seq — the post-replay value
    // answers "the checkpoint covered it" and "replay reached it" with the same
    // number, and only the first is durable. Conflating them showed a live
    // durability warning for a deletion that was already persisted.
    let f = fx();
    build_v2_state(&f, false);
    let covered = {
        let (h, _, _) = open_recovering(&f.cp).unwrap();
        let settled = h.clone();
        settled.save(&f.cp).unwrap();
        settled.applied_seq()
    };
    assert!(covered > 0);
    write_marker(&f, DeletionBreach::Unflushed { seq: covered });

    let report = open_report_of(&f);
    assert!(!report.deletion_lost);
    assert!(
        !report.deletion_pending_checkpoint,
        "a checkpoint-covered deletion is not a live durability problem"
    );
    assert_eq!(
        deletion_marker::read(&f.cp),
        None,
        "and its marker is stale, not owed"
    );
}

#[test]
fn t10_a_reported_witness_cannot_be_settled_by_a_rebased_seq() {
    // The concrete shape of the promotion above. A WAL quarantine re-bases
    // numbering (`adopt_empty` restarts at the checkpoint's applied_seq + 1),
    // so without the promotion a later, unrelated frame reaching the same seq
    // would satisfy the old witness and silently drop the report.
    let f = fx();
    build_v2_state(&f, false);
    let applied = applied_seq_of(&f);
    write_marker(&f, DeletionBreach::Unflushed { seq: applied + 5 });

    assert!(open_report_of(&f).deletion_lost);
    // Numbering advances past the old witness on wholly unrelated work. The
    // checkpoint must still hold entries: an *empty* durable set settles the
    // claim outright (there is nothing left for a deletion to have failed
    // against), which is a different rule and would mask this one.
    let mut h = UserHistory::new();
    h.record_at(&seg(B), T0);
    h.advance_applied_seq(applied + 99);
    h.save(&f.cp).unwrap();

    assert!(
        open_report_of(&f).deletion_lost,
        "the report must not be settled by seqs the witness never referred to"
    );
}

#[test]
fn t10_an_empty_history_settles_the_marker() {
    // A claim that some entry survived a deletion is *false*, not stale, when
    // no entry was loaded at all — the same reasoning `clear` uses when it
    // covers the ledger from its empty checkpoint. Without this, a wipe that
    // could not unlink the marker warned on the next launch about a history
    // that provably holds nothing.
    let f = fx();
    let empty = UserHistory::new();
    empty.save(&f.cp).unwrap();
    write_marker(&f, DeletionBreach::Lost);

    let report = open_report_of(&f);
    assert!(
        !report.deletion_lost,
        "there is no entry for the deletion to have failed against"
    );
    assert_eq!(
        deletion_marker::read(&f.cp),
        None,
        "and the record goes with the claim"
    );
}

#[test]
fn t10_malformed_markers_all_report() {
    // Fail-safe by construction: only NotFound is clean. Every malformed
    // shape resolves to the strongest claim, which is why the format carries
    // no CRC — corruption can only push it toward reporting.
    //
    // The 12-byte case is the one that matters structurally: a parser that
    // checked the magic but not the length would slice bytes[8..16] on it and
    // panic out through `#[uniffi::constructor]`.
    let mut good = Vec::new();
    good.extend_from_slice(b"LXDM");
    good.push(1);
    good.push(1);
    good.extend_from_slice(&[0, 0]);
    good.extend_from_slice(&7u64.to_le_bytes());

    for (name, bytes) in [
        ("empty", vec![]),
        ("three bytes", vec![b'L', b'X', b'D']),
        ("twelve bytes", good[..12].to_vec()),
        ("bad magic", {
            let mut b = good.clone();
            b[0] = b'X';
            b
        }),
        ("unknown version", {
            let mut b = good.clone();
            b[4] = 9;
            b
        }),
        // The one shape that would resolve toward *suppression*: a
        // well-formed 16-byte prefix followed by anything else. Reading
        // exactly LEN would decode it as a valid witness, and this witness is
        // one the fixture's replay satisfies — so the "malformed always
        // reports" rule, and the absent CRC that rests on it, would both be
        // false. Seq 1 is load-bearing here: a witness beyond `applied_seq`
        // reports either way and would prove nothing.
        // Bytes the writer cannot emit. Each of these was found separately as
        // its own bug before `decode` became a round-trip against `encode`;
        // the last two nobody had named at all.
        ("unknown flags", {
            let mut b = good.clone();
            b[5] = 0x03;
            b[8..16].copy_from_slice(&1u64.to_le_bytes());
            b
        }),
        ("non-zero reserved", {
            let mut b = good.clone();
            b[6] = 1;
            b[8..16].copy_from_slice(&1u64.to_le_bytes());
            b
        }),
        ("seq without the witness flag", {
            let mut b = good.clone();
            b[5] = 0;
            b
        }),
        // The shape that resolved to *silence* rather than to a suppressible
        // witness: seq 0 is below every applied_seq, so recovery read it as
        // "the checkpoint already covers this", removed the marker, and
        // reported nothing. The writer cannot produce it — WAL numbering
        // starts at 1 — so it is malformed like the rest.
        ("witness of zero", {
            let mut b = good.clone();
            b[8..16].copy_from_slice(&0u64.to_le_bytes());
            b
        }),
        ("valid prefix, extra bytes", {
            let mut b = good.clone();
            b[8..16].copy_from_slice(&1u64.to_le_bytes());
            b.extend_from_slice(b"trailing");
            b
        }),
    ] {
        let f = fx();
        build_v2_state(&f, false);
        marker_bytes(&f, &bytes);
        assert!(
            open_report_of(&f).deletion_lost,
            "{name}: an unreadable marker must report, not suppress"
        );
        assert!(
            deletion_marker::marker_path(&f.cp).exists(),
            "{name}: and it must not be silently retracted as checkpoint-covered"
        );
    }
}

#[test]
fn t10_unreadable_marker_reports_without_failing_the_open() {
    // A directory at the marker path makes the read fail. It must not
    // propagate: `open_recovering`'s only Err is an environmental failure, and
    // Swift turns that into "learning disabled" — stopping learning outright
    // because a 16-byte sidecar is unreadable is the worst possible default.
    let f = fx();
    build_v2_state(&f, false);
    fs::create_dir(deletion_marker::marker_path(&f.cp)).unwrap();

    assert!(
        open_report_of(&f).deletion_lost,
        "unreadable resolves to reporting"
    );
}

#[test]
fn t10_removal_reports_whether_the_path_is_actually_clear() {
    // Every retraction clears the marker by unlinking, so a removal that
    // fails must say so — otherwise the caller drops the status row while the
    // record stands, and the warning returns on the next launch with the
    // retry gone.
    //
    // A directory at the path is removed only when empty. A non-empty one is
    // not the engine's to delete (a restore put it there, and walking it would
    // block the thread the menu runs on), so the honest outcome is `false`.
    let f = fx();
    build_v2_state(&f, false);
    let path = deletion_marker::marker_path(&f.cp);

    assert!(deletion_marker::remove(&f.cp), "absent counts as clear");

    fs::create_dir(&path).unwrap();
    assert!(
        deletion_marker::remove(&f.cp),
        "an empty placeholder is ours to clear"
    );
    assert!(!path.exists());

    fs::create_dir(&path).unwrap();
    fs::write(path.join("restored"), b"someone else's bytes").unwrap();
    assert!(
        !deletion_marker::remove(&f.cp),
        "a non-empty directory is not ours to delete, and saying so is the point"
    );
    assert!(
        path.join("restored").exists(),
        "and its contents must survive"
    );
    // The report is then still owed, which the reader agrees with.
    assert_eq!(deletion_marker::read(&f.cp), Some(DeletionBreach::Lost));
}

#[test]
fn t10_removal_covers_the_orphan_tmp_too() {
    // The logical marker is the pair. `read` merges the atomic write's orphan
    // so a crash between its flush and the rename can only strengthen a claim
    // — which means removing only the canonical file leaves the claim alive.
    // A `Lost` orphan would then re-report on every launch with nothing able
    // to clear it: the unclearable latch, rebuilt out of the fix for hiding.
    let f = fx();
    build_v2_state(&f, false);
    let tmp =
        f.cp.with_file_name("user_history.lxud.deletion-pending.tmp");
    write_marker(&f, DeletionBreach::Lost);
    fs::write(&tmp, encoded_marker(DeletionBreach::Lost)).unwrap();

    assert!(deletion_marker::remove(&f.cp));
    assert!(!tmp.exists(), "the orphan is part of what had to go");
    assert_eq!(deletion_marker::read(&f.cp), None);
    assert!(!open_report_of(&f).deletion_lost);
}

#[test]
fn t10_removal_reports_false_when_only_the_orphan_resists() {
    // And the outcome is honest about either half: a claim the caller cannot
    // clear must not be reported as cleared just because the other file went.
    let f = fx();
    build_v2_state(&f, false);
    let tmp =
        f.cp.with_file_name("user_history.lxud.deletion-pending.tmp");
    write_marker(&f, DeletionBreach::Lost);
    fs::create_dir(&tmp).unwrap();
    fs::write(tmp.join("restored"), b"not ours").unwrap();

    assert!(
        !deletion_marker::remove(&f.cp),
        "an orphan that resists leaves the record standing"
    );
    assert!(
        !deletion_marker::marker_path(&f.cp).exists(),
        "the half that could go, went"
    );
    assert_eq!(
        deletion_marker::read(&f.cp),
        Some(DeletionBreach::Lost),
        "and the surviving orphan still carries the claim"
    );
}

#[test]
fn t10_a_fifo_at_the_marker_path_does_not_block_the_open() {
    // A read-only `File::open` on a FIFO blocks until a writer appears, and
    // this runs synchronously inside the IME's startup. Left unguarded, a
    // sidecar nobody can read would stop the input method from ever becoming
    // available — so the file type is checked before anything is opened.
    let f = fx();
    build_v2_state(&f, false);
    let path = deletion_marker::marker_path(&f.cp);
    let status = std::process::Command::new("mkfifo")
        .arg(&path)
        .status()
        .expect("mkfifo");
    assert!(status.success());

    // Note the failure mode: without the guard this does not fail, it HANGS —
    // which is exactly the defect (a startup that never completes). A mutation
    // check on the guard therefore shows up as a timeout, not a red test.
    assert_eq!(deletion_marker::read(&f.cp), Some(DeletionBreach::Lost));
    assert!(open_report_of(&f).deletion_lost);
}

#[test]
fn t10_a_migration_that_persists_the_deletion_settles_the_marker() {
    // The migration commit writes a durable v2 checkpoint serialized from the
    // replayed state — which, when replay applied a tombstone, is a checkpoint
    // that *contains* the deletion. That is coverage, so the marker is settled
    // and nothing is handed to the runtime ledger.
    //
    // Evaluating the marker before the commit answered against the v1 file,
    // which predates the deletion: the engine then seeded a live durability
    // warning for an already-persisted deletion and left the marker for an
    // asynchronous compaction that might never run.
    //
    // The fixture is the state the migration path documents as reachable: a
    // previous startup's commit failed, so later commits appended v2 frames
    // beside the still-v1 checkpoint.
    let f = fx();
    let mut h = UserHistory::new();
    h.record_at(&seg(A), T0);
    fs::write(&f.cp, v1_checkpoint_bytes(&h)).unwrap();
    let seq = {
        let mut wal = HistoryWal::new(&f.cp);
        wal.append_record(&WalRecord::Tombstone {
            segments: seg(A),
            timestamp: T0 + 1,
        })
        .unwrap()
    };
    write_marker(&f, DeletionBreach::Unflushed { seq });

    let report = open_report_of(&f);
    assert!(report.migrated_from_v1, "fixture must actually migrate");
    assert!(
        !report.deletion_lost,
        "the deletion took, so nothing is owed"
    );
    assert!(
        !report.deletion_pending_checkpoint,
        "the migration's own checkpoint is the durable coverage"
    );
    assert_eq!(
        deletion_marker::read(&f.cp),
        None,
        "and it settles the marker rather than leaving it to a compaction"
    );

    // The deletion really is gone from the checkpoint that was just written.
    let (reloaded, _, _) = open_recovering(&f.cp).unwrap();
    assert_contents(&reloaded, &[], &[A]);
}

#[test]
fn t10_a_migration_does_not_settle_an_unconditional_claim() {
    // A `Lost` claim is unconditional, so the checkpoint the migration writes
    // — which does contain the resurrected entry, replay having applied no
    // tombstone here — settles nothing. It has to come out the other side.
    //
    // The sibling test above is the complement: when replay *did* apply the
    // tombstone, that same commit is genuine coverage. What separates them is
    // the claim, not the migration, which is why an earlier version of this
    // test asserting "a migration checkpoint is never coverage" stated a rule
    // its own neighbour disproves.
    let f = fx();
    write_v1_state(&f);
    write_marker(&f, DeletionBreach::Lost);

    let report = open_report_of(&f);
    assert!(report.migrated_from_v1, "fixture must actually migrate");
    assert!(
        report.deletion_lost,
        "an unconditional claim is owed no matter what was written"
    );
    assert!(
        deletion_marker::marker_path(&f.cp).exists(),
        "and its record stays until someone is told"
    );
}

#[test]
fn t10_witness_beyond_a_repaired_tail_still_reports() {
    // E5/E16: the tombstone's frame was on disk but is gone now — cut away by
    // tail repair, or carried off with a quarantined WAL. `applied_seq` never
    // reaches the witness, so the deletion never took effect. This is the
    // power-loss half of `Unflushed`, and the only reason the witness is a
    // seq rather than a bool.
    let f = fx();
    build_v2_state(&f, false);
    let full = applied_seq_of(&f);

    let f2 = fx();
    build_v2_state(&f2, false);
    cut_tail(&f2.wal, 4);
    write_marker(&f2, DeletionBreach::Unflushed { seq: full });

    let report = open_report_of(&f2);
    assert_eq!(report.wal_state, WalState::TailRepaired);
    assert!(
        report.deletion_lost,
        "a witness the repaired file can no longer reach must report"
    );
}

#[test]
fn t10_marker_coexists_with_a_quarantine() {
    // Independent facts about one startup: the checkpoint was corrupt *and* a
    // previous deletion never persisted. Neither may mask the other.
    let f = fx();
    build_v2_state(&f, false);
    corrupt_cp(&f.cp);
    write_marker(&f, DeletionBreach::Lost);

    let report = open_report_of(&f);
    assert_eq!(report.checkpoint_state, CheckpointState::Quarantined);
    assert!(report.data_loss_suspected());
    assert!(report.deletion_lost);
}

#[test]
fn t10_strict_open_ignores_the_marker() {
    // The offline path must stay side-effect-free: an audit tool consuming a
    // live IME's marker would delete the report the user never saw.
    let f = fx();
    build_v2_state(&f, false);
    write_marker(&f, DeletionBreach::Lost);

    UserHistory::open(&f.cp).unwrap();
    assert!(deletion_marker::marker_path(&f.cp).exists());
}

#[test]
fn t10_marker_is_not_a_quarantine_file() {
    // Rotation matches `<name>*` containing ".corrupt-". The marker shares the
    // family prefix, so a widened match would start rotating (and eventually
    // deleting) the report.
    //
    // Calls the production predicate, not a local re-spelling of it. The first
    // version of this test re-implemented the `.corrupt-` filter in the test
    // helper and therefore could not observe the real one changing at all — a
    // mutation widening `quarantined_files` left it green.
    let f = fx();
    build_v2_state(&f, false);
    write_marker(&f, DeletionBreach::Lost);
    // Enough real quarantine files to push rotation past its keep limit, so a
    // predicate that matched the marker would actually delete it.
    for i in 0..5 {
        fs::write(
            f.cp.with_file_name(format!("user_history.lxud.corrupt-{i}")),
            b"x",
        )
        .unwrap();
    }

    let listed = crate::persist::quarantined_files(&f.cp);
    let marker = deletion_marker::marker_path(&f.cp);
    assert!(
        !listed.contains(&marker),
        "the marker must not be listed as a quarantine artifact: {listed:?}"
    );
    crate::persist::rotate_quarantined(&f.cp, 3);
    assert!(marker.exists(), "rotation must not reach the marker");
    assert_eq!(deletion_marker::read(&f.cp), Some(DeletionBreach::Lost));
}

#[test]
fn t10_decode_accepts_only_what_encode_emits() {
    // The property the round-trip form buys, stated directly: whatever the
    // writer can produce reads back as itself, and nothing else is trusted.
    // Field-by-field validation cannot be checked this way, which is why it
    // was wrong four times.
    for breach in [
        DeletionBreach::Lost,
        DeletionBreach::Unflushed { seq: 1 },
        DeletionBreach::Unflushed { seq: u64::MAX },
    ] {
        let f = fx();
        build_v2_state(&f, false);
        write_marker(&f, breach);
        assert_eq!(
            deletion_marker::read(&f.cp),
            Some(breach),
            "{breach:?} must survive a round trip"
        );
    }
}

#[test]
fn t10_merge_never_weakens_an_outstanding_claim() {
    // The rule the whole file depends on: a plain overwrite would let an
    // `Unflushed` land on top of a `Lost` and hand the next startup a witness
    // it can suppress — losing precisely the report #312 is about. Both
    // orders, because the batch loop can produce either.
    let lost = DeletionBreach::Lost;
    let a = DeletionBreach::Unflushed { seq: 5 };
    let b = DeletionBreach::Unflushed { seq: 9 };
    assert_eq!(lost.merge(a), lost);
    assert_eq!(a.merge(lost), lost);
    assert_eq!(a.merge(b), b, "two unflushed frames keep the higher seq");
    assert_eq!(b.merge(a), b);

    // And through the file, which is where the ordering actually matters.
    let f = fx();
    build_v2_state(&f, false);
    write_marker(&f, DeletionBreach::Unflushed { seq: 5 });
    write_marker(&f, DeletionBreach::Lost);
    write_marker(&f, DeletionBreach::Unflushed { seq: 9 });
    assert_eq!(deletion_marker::read(&f.cp), Some(DeletionBreach::Lost));
}
