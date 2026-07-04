//! Recovery-mode open for the engine: quarantine, migration, tail repair.
//!
//! The core guarantee is "no path silently stops learning forever": whatever
//! the on-disk state, `open_recovering` succeeds and returns a usable
//! history, with corruption quarantined (renamed aside so the bytes stay
//! rescuable; see [`quarantine`] for the last-resort fallbacks when even the
//! rename fails) and a report of what happened. The only `Err` is an
//! environmental read failure (EACCES etc.), which is visible degradation,
//! not a silent stop.
//!
//! This path owns the files and performs side effects (rename / truncate /
//! migration writes). Offline tooling must keep using the strict, side-
//! effect-free [`UserHistory::open`] / [`super::wal::open_with_wal`]: an
//! audit tool renaming a live IME's files would be an incident.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use tracing::{info, warn};

use crate::persist;

use super::persistence::{load_checkpoint, CheckpointLoaded};
use super::wal::{
    classify_wal, legacy_valid_prefix, scan_legacy, scan_v2, wal_path_for, HistoryWal, WalFormat,
    WAL_HEADER_LEN,
};
use super::UserHistory;

/// How many quarantined (`.corrupt-*`) files to retain per history family.
const QUARANTINE_KEEP: usize = 3;

/// GC horizon for the v1 migration backup (`.v1.bak`), ~90 days. Its value
/// as a manual rescue hatch (downgrade / migration bug) concentrates right
/// after migration; a time-based GC keeps cleanup independent of release
/// cadence (no "remove in version N+2" bookkeeping). `clear()` removes it
/// immediately regardless (privacy wipe).
const V1_BACKUP_TTL_SECS: u64 = 90 * 24 * 60 * 60;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CheckpointState {
    /// Read successfully (v2).
    Loaded,
    /// v1 data converted to a v2 checkpoint this startup.
    Migrated,
    /// No checkpoint file (fresh install, or post-clear).
    Missing,
    /// Corrupt; renamed to `.corrupt-<ts>` and started empty.
    Quarantined,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WalState {
    /// Every frame scanned cleanly to EOF.
    Clean,
    /// No WAL file.
    Missing,
    /// A corrupt/truncated tail was cut back to the last good frame. The
    /// expected residue of power loss — log-only, not user-visible (§8).
    TailRepaired,
    /// Unreadable header; whole file renamed to `.corrupt-<ts>`.
    Quarantined,
    /// v1-format WAL alongside a v2 checkpoint (migration-crash residue,
    /// already covered by the checkpoint) — discarded without replay.
    LegacyDiscarded,
    /// File re-created as header-only (sub-header stub, or v1 WAL consumed
    /// by migration). Log-only.
    Reinitialized,
    /// A repair (tail truncation or reinitialization) was needed but the
    /// file operation failed; appends are frozen until a compaction heals
    /// the file. Nothing is lost — memory keeps the state and checkpoints
    /// keep persisting it.
    RepairFailed,
}

/// What `open_recovering` found and did. PR2 surfaces this over UniFFI;
/// until then the engine logs it.
#[derive(Clone, Debug)]
pub struct OpenReport {
    pub checkpoint_state: CheckpointState,
    pub wal_state: WalState,
    pub migrated_from_v1: bool,
    pub frames_replayed: u64,
    pub frames_skipped: u64,
    pub quarantined_paths: Vec<PathBuf>,
    /// A replayed Tombstone left deleted strings in the old checkpoint /
    /// earlier frames unscrubbed — feeds `compaction_recommended` (§5.4).
    /// Not surfaced over UniFFI; an internal startup-scrub signal.
    pub replayed_deletion: bool,
    /// Startup-compaction hint (§5.1-6): recovery results should be
    /// checkpointed early so the next startup is clean. Consumed in PR2.
    pub compaction_recommended: bool,
}

impl OpenReport {
    /// Whether learning data may have been lost in a way the user should
    /// hear about. Per §8 only whole-file quarantine qualifies; tail repair
    /// and migration are expected/benign and stay log-only.
    pub fn data_loss_suspected(&self) -> bool {
        !self.quarantined_paths.is_empty()
            || self.checkpoint_state == CheckpointState::Quarantined
            || self.wal_state == WalState::Quarantined
    }

    /// Nothing worth logging happened.
    pub fn is_clean(&self) -> bool {
        matches!(
            self.checkpoint_state,
            CheckpointState::Loaded | CheckpointState::Missing
        ) && matches!(self.wal_state, WalState::Clean | WalState::Missing)
            && !self.migrated_from_v1
    }
}

/// Open checkpoint + WAL with full recovery semantics. See module docs.
pub fn open_recovering(
    checkpoint_path: &Path,
) -> io::Result<(UserHistory, HistoryWal, OpenReport)> {
    let wal_path = wal_path_for(checkpoint_path);
    let mut report = OpenReport {
        checkpoint_state: CheckpointState::Loaded,
        wal_state: WalState::Clean,
        migrated_from_v1: false,
        frames_replayed: 0,
        frames_skipped: 0,
        quarantined_paths: Vec::new(),
        replayed_deletion: false,
        compaction_recommended: false,
    };

    // --- 1. checkpoint ---
    // `migrate` = v1-format data seen (v1 checkpoint and/or headerless WAL);
    // committed below by writing a v2 checkpoint, then reinitializing the WAL.
    let (mut history, cp_is_v2, mut migrate) = match load_checkpoint(checkpoint_path)? {
        CheckpointLoaded::V2(h) => (h, true, false),
        CheckpointLoaded::V1(h) => (h, false, true),
        CheckpointLoaded::Missing => {
            report.checkpoint_state = CheckpointState::Missing;
            (UserHistory::new(), false, false)
        }
        CheckpointLoaded::Corrupt(e) => {
            warn!("user history checkpoint corrupt ({e}); quarantining");
            quarantine(checkpoint_path, &mut report.quarantined_paths);
            report.checkpoint_state = CheckpointState::Quarantined;
            (UserHistory::new(), false, false)
        }
    };
    let v1_checkpoint_present = migrate;

    // --- 2. WAL ---
    let mut wal = HistoryWal::new(checkpoint_path);
    let mut legacy_wal_consumed = false;
    match fs::read(&wal_path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            report.wal_state = WalState::Missing;
            wal.adopt_empty(history.applied_seq());
        }
        Err(e) => return Err(e),
        Ok(data) => {
            match classify_wal(&data) {
                WalFormat::Stub => {
                    // 0-7 bytes: normal residue of a crash during WAL truncation.
                    report.wal_state = if reinitialize(&mut wal, &history) {
                        WalState::Reinitialized
                    } else {
                        WalState::RepairFailed
                    };
                }
                WalFormat::BadHeader => {
                    warn!("user history WAL header unreadable; quarantining whole file");
                    report.wal_state = WalState::Quarantined;
                    if quarantine(&wal_path, &mut report.quarantined_paths) {
                        wal.adopt_empty(history.applied_seq());
                    } else {
                        // The unreadable file is still in place; do not append
                        // v2 frames after foreign bytes.
                        wal.adopt_empty(history.applied_seq());
                        wal.freeze();
                    }
                }
                WalFormat::Legacy => {
                    if legacy_valid_prefix(&data) == 0 {
                        // No readable v1 frame at all — most likely a v2 WAL
                        // whose magic bytes got corrupted (a real v1 file
                        // starts with a small length field and CRC-valid
                        // frames). Quarantine like an unreadable header
                        // rather than silently discarding what may be the
                        // post-checkpoint tail.
                        warn!("headerless WAL with no readable v1 frame; quarantining");
                        report.wal_state = WalState::Quarantined;
                        wal.adopt_empty(history.applied_seq());
                        if !quarantine(&wal_path, &mut report.quarantined_paths) {
                            wal.freeze();
                        }
                    } else if cp_is_v2 {
                        // Migration-crash residue: the v2 checkpoint already
                        // contains everything the v1 WAL held (§2.3/§7) —
                        // discard without replay so migration stays
                        // idempotent.
                        report.wal_state = if reinitialize(&mut wal, &history) {
                            WalState::LegacyDiscarded
                        } else {
                            WalState::RepairFailed
                        };
                    } else {
                        // v1-format data (v1, missing, or quarantined
                        // checkpoint): migration input. Replaying under a
                        // missing/quarantined checkpoint cannot double-apply —
                        // nothing was loaded.
                        let scan = scan_legacy(&data, &mut history);
                        report.frames_replayed = scan.frames_applied;
                        if scan.truncated_tail {
                            info!("v1 WAL tail unreadable; migrated the good prefix");
                        }
                        legacy_wal_consumed = true;
                        migrate = true;
                    }
                }
                WalFormat::V2 => {
                    let scan = scan_v2(&data, &mut history);
                    report.frames_replayed = scan.frames_applied;
                    report.frames_skipped = scan.frames_skipped;
                    report.replayed_deletion = scan.replayed_deletion;
                    // wal_bytes reflects the on-disk size, so start from the
                    // valid-prefix length only when the repair below actually
                    // shrinks the file to it.
                    let mut file_bytes = scan.last_good_end as u64;
                    if scan.truncated_tail {
                        // Physical repair. v1 never truncated, so appends landed
                        // after the corrupt point and became permanently
                        // invisible to replay (problem A).
                        match repair_tail(&wal_path, scan.last_good_end) {
                            Ok(()) => {
                                report.wal_state = WalState::TailRepaired;
                                info!(
                                    "user history WAL tail repaired at byte {}",
                                    scan.last_good_end
                                );
                            }
                            Err(e) => {
                                warn!("WAL tail repair failed ({e}); freezing appends until compaction");
                                report.wal_state = WalState::RepairFailed;
                                // The corrupt tail is still on disk; under-
                                // reporting the size would keep byte-based
                                // compaction backstops from ever firing.
                                file_bytes = data.len() as u64;
                                wal.freeze();
                            }
                        }
                    }
                    wal.adopt_scan(
                        scan.valid_frames,
                        file_bytes,
                        scan.max_seq,
                        history.applied_seq(),
                    );
                }
            }
        }
    }

    // Replay applied frames without evicting; settle capacity once (§5.1-4).
    history.evict();

    // --- 3. migration commit (§7) ---
    if migrate {
        if v1_checkpoint_present {
            backup_v1_checkpoint(checkpoint_path);
        }
        // applied_seq is usually 0 here (v1 data carries no seqs), but not
        // always: if a previous startup's migration commit failed with no
        // legacy WAL to freeze, later commits appended v2 frames next to
        // the still-v1 checkpoint. Those frames were replayed above, so the
        // checkpoint written here covers them (its applied_seq reflects the
        // replay) and they are correctly skipped from now on.
        match history.save(checkpoint_path) {
            Ok(()) => {
                // Commit point passed: from here any crash leaves a v2
                // checkpoint, and a leftover v1 WAL is discarded on the next
                // startup (idempotent).
                report.migrated_from_v1 = true;
                if v1_checkpoint_present {
                    report.checkpoint_state = CheckpointState::Migrated;
                }
                if legacy_wal_consumed {
                    report.wal_state = if reinitialize(&mut wal, &history) {
                        WalState::Reinitialized
                    } else {
                        WalState::RepairFailed
                    };
                }
                info!(
                    "user history migrated from v1 (frames: {})",
                    report.frames_replayed
                );
            }
            Err(e) => {
                // Leave the v1 files intact so the next startup retries.
                // Learning continues in memory; appends are frozen because
                // the on-disk WAL is still v1-format.
                warn!("v1->v2 migration checkpoint write failed ({e}); keeping v1 files intact");
                if legacy_wal_consumed {
                    wal.freeze();
                }
            }
        }
    }

    // Anomaly (§8 missing x non-empty): a checkpoint should only be absent
    // when the WAL is too. Recoverable via replay, but checkpoint early.
    if report.checkpoint_state == CheckpointState::Missing && report.frames_replayed > 0 {
        warn!(
            "user history checkpoint missing but WAL has {} frames; will re-checkpoint",
            report.frames_replayed
        );
    }

    // --- 4. startup-compaction hint (§5.1-6) ---
    // data_loss_suspected() keys off the quarantine *states*, not the path
    // list: quarantine() can succeed via the remove-file fallback without
    // recording a path, and that case still needs an early re-checkpoint.
    // frames_skipped: every skipped frame is already covered by the
    // checkpoint, so the whole file is truncation-eligible — this is the
    // residue of a crash between checkpoint write and truncation, or of a
    // clear whose truncation failed. The latter makes this a privacy
    // backstop: a clear's leftover input strings are scrubbed on the next
    // startup even if the post-clear heal never ran (e.g. the reset flow
    // restarts the process immediately).
    // replayed_deletion: the delete-residue counterpart of frames_skipped.
    // A crash between a Tombstone append and its async scrub leaves the
    // deleted strings in the old checkpoint (and earlier frames); on restart
    // the Tombstone replays (deletion correct) but nothing is skipped, so
    // without this signal no scrub would run until an unrelated compaction,
    // leaving the input on disk indefinitely.
    report.compaction_recommended = report.migrated_from_v1
        || report.data_loss_suspected()
        || (report.checkpoint_state == CheckpointState::Missing && report.frames_replayed > 0)
        || report.frames_skipped > 0
        || report.replayed_deletion
        || wal.needs_compact()
        || wal.is_frozen();

    // --- 5. quarantine rotation + v1-backup GC ---
    persist::rotate_quarantined(checkpoint_path, QUARANTINE_KEEP);
    gc_v1_backup(checkpoint_path);

    Ok((history, wal, report))
}

/// Best-effort removal of an expired `.v1.bak` (see [`V1_BACKUP_TTL_SECS`]).
/// A backup written by this very startup has a fresh mtime and survives.
fn gc_v1_backup(checkpoint_path: &Path) {
    let path = v1_backup_path(checkpoint_path);
    let Ok(meta) = fs::metadata(&path) else {
        return;
    };
    let Ok(modified) = meta.modified() else {
        return;
    };
    let age = std::time::SystemTime::now()
        .duration_since(modified)
        .unwrap_or_default();
    if age.as_secs() <= V1_BACKUP_TTL_SECS {
        return;
    }
    match fs::remove_file(&path) {
        Ok(()) => info!("removed expired v1 checkpoint backup {}", path.display()),
        Err(e) => warn!("failed to remove expired v1 backup {}: {e}", path.display()),
    }
}

/// Path of the best-effort v1 checkpoint backup (`<name>.v1.bak`).
pub fn v1_backup_path(checkpoint_path: &Path) -> PathBuf {
    persist::suffixed(checkpoint_path, ".v1.bak")
}

/// Remove recovery artifacts (`.v1.bak`, `.corrupt-*`, stray `.tmp`) for
/// this history family. `clear()` calls this: a privacy wipe must not leave
/// rescued bytes behind.
pub fn remove_recovery_artifacts(checkpoint_path: &Path) -> io::Result<()> {
    for path in [
        v1_backup_path(checkpoint_path),
        persist::suffixed(checkpoint_path, ".tmp"),
        // The v1 writer used `with_extension("tmp")` (`user_history.tmp`):
        // a pre-upgrade crash before rename can leave a full serialized
        // history copy under that name.
        checkpoint_path.with_extension("tmp"),
    ] {
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    for path in persist::quarantined_files(checkpoint_path) {
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Adapter over [`persist::quarantine`] for the checkpoint/WAL recovery
/// path: injects the recovery clock, records the rescued path in the report,
/// and reports whether `path` is now clear (both rename and remove failing
/// leaves it in place, which the WAL branch turns into a `freeze()`).
fn quarantine(path: &Path, quarantined: &mut Vec<PathBuf>) -> bool {
    match persist::quarantine(path, super::now_epoch()) {
        persist::Quarantine::Renamed(dest) => {
            quarantined.push(dest);
            true
        }
        persist::Quarantine::Removed => true,
        persist::Quarantine::Failed => false,
    }
}

/// Copy the v1 checkpoint bytes aside before migration overwrites the path.
/// Best-effort: the backup is a manual rescue hatch for downgrades and
/// migration bugs, not a correctness dependency.
fn backup_v1_checkpoint(checkpoint_path: &Path) {
    let dest = v1_backup_path(checkpoint_path);
    if let Err(e) = fs::copy(checkpoint_path, &dest) {
        warn!("v1 checkpoint backup failed ({e}); continuing migration");
    }
}

/// Physically truncate the WAL at the last good frame boundary so appends
/// land where replay can see them.
fn repair_tail(wal_path: &Path, keep: usize) -> io::Result<()> {
    debug_assert!(keep >= WAL_HEADER_LEN);
    let f = fs::OpenOptions::new().write(true).open(wal_path)?;
    f.set_len(keep as u64)?;
    f.sync_data()?;
    Ok(())
}

/// Re-create the WAL as header-only; on failure freeze appends (the file is
/// not in appendable v2 form) and let the next compaction heal it. Returns
/// whether the reinitialization actually happened (callers report
/// `RepairFailed` on `false`).
fn reinitialize(wal: &mut HistoryWal, history: &UserHistory) -> bool {
    match wal.truncate_wal() {
        Ok(()) => {
            wal.adopt_empty(history.applied_seq());
            true
        }
        Err(e) => {
            warn!("WAL reinitialization failed ({e}); freezing appends until compaction");
            wal.adopt_empty(history.applied_seq());
            wal.freeze();
            false
        }
    }
}
