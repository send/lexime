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
//!
//! It also runs **before the history is shared**: the `HistoryWal` it returns
//! has not entered its mutex yet and no session can reach it. That is the
//! standing exemption for the one write it makes to the unpersisted-deletion
//! marker, which everywhere else in the engine happens only under the wal mutex
//! (the mutex is what makes a raise and a cover unable to interleave). A future
//! path that re-opens a *live* history would break the exemption, not merely
//! bend it. This function removes the marker on two grounds and no others: a
//! *durable checkpoint* on disk already covers the witness — including the one
//! the migration path writes itself — or the `Lost` claim is vacuous, both the
//! durable set and the loaded state being empty, so no entry exists for the
//! deletion to have failed against. Replay reaching the witness is neither: it
//! proves the frame was readable, not flushed.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use tracing::{info, warn};

use crate::persist;

use super::deletion_marker;
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
    /// A v1→v2 migration was needed but its commit (the v2 checkpoint
    /// write) failed, so the v1 files are kept intact.
    ///
    /// This flag itself schedules nothing — a compaction is not the
    /// migration, so using one as the retry would overwrite the v1
    /// checkpoint on exactly the path where the commit is already failing.
    /// When a legacy WAL was consumed the conversion does still complete
    /// in-session, but for an unrelated reason: that path freezes the WAL,
    /// and the compaction `appends_frozen` schedules to thaw it writes the
    /// v2 checkpoint as a side effect. Otherwise the next launch retries.
    /// So the timing is not a property of this flag, which is why the
    /// shipped log line states the fact and not a schedule.
    ///
    /// Derived from the migration's own outcome rather than from a
    /// side effect of it: the `Err` branch only freezes the WAL when a
    /// legacy WAL was consumed, so `is_frozen()` misses the v1-checkpoint-
    /// with-fresh-WAL case entirely — and that case reported a clean
    /// startup while learning quietly failed to migrate.
    pub migration_failed: bool,
    /// Appends were frozen when this WAL was opened: everything learned
    /// this session stays in memory until a compaction restores appendable
    /// form. Derived once from `wal.is_frozen()` rather than assigned in
    /// each branch that freezes — a branch forgetting to set its state is
    /// precisely how a failed migration came to look clean.
    pub appends_frozen: bool,
    /// A replayed Tombstone left deleted strings in the old checkpoint /
    /// earlier frames unscrubbed — feeds `compaction_recommended` (§5.4).
    /// Not surfaced over UniFFI; an internal startup-scrub signal.
    pub replayed_deletion: bool,
    /// Startup-compaction hint (§5.1-6): recovery results should be
    /// checkpointed early so the next startup is clean. Consumed in PR2.
    pub compaction_recommended: bool,
    /// A previous session could not persist a deletion the user asked for,
    /// and nothing since has covered it (#312) — so the state just loaded may
    /// still hold the entry that deletion was meant to remove.
    ///
    /// Its own field for the same reason `migration_failed` is: on this path
    /// `checkpoint_state` is *truthfully* `Loaded` — the checkpoint read
    /// perfectly, it just contains something that should be gone — so no
    /// existing enum can carry the fact without a state per combination. It
    /// deliberately stays out of `data_loss_suspected()`, whose user-facing
    /// wording is "some past learning was lost": this is the opposite loss,
    /// data that survived when it should not have.
    ///
    /// Feeds `compaction_recommended` **when the disk does not already say
    /// `Lost`** — a reversal of the note that used to sit here, kept as a
    /// record of why.
    ///
    /// That note said a compaction "would checkpoint the resurrected entry and
    /// cover the ledger — i.e. tell the user everything is fine". All three
    /// premises are false now, and the last was false when it was written:
    ///
    /// - it cannot cover the ledger — nothing raised it this session, so the
    ///   cover early-returns, which the note's own parenthetical conceded;
    /// - it cannot tell the user anything is fine — the row is driven by
    ///   `inherited_owed` and the latched `EngineInitFailure`, and a
    ///   compaction touches neither;
    /// - re-checkpointing the resurrected entry changes nothing. The entry is
    ///   already in the durable checkpoint. That is *why* it resurrected.
    ///
    /// What a compaction does do is project, and with the inherited claim
    /// standing the projection writes `Lost`. That is the point: recovery's
    /// promotion is best-effort, and when it fails the disk keeps the
    /// *suppressible* `Unflushed{seq}` under a memory claim of `Lost`. A
    /// healthy session raises nothing, so nothing re-projects until a
    /// threshold compaction a thousand frames away — and ordinary commits
    /// advance the WAL past the witness in the meantime, so a restart before
    /// then reads the witness as replayed and suppresses a report that is
    /// still owed. Scheduling the compaction is what makes the promotion
    /// actually happen.
    pub deletion_lost: bool,
    /// What the marker file holds once this function is done with it, as
    /// observed rather than assumed.
    ///
    /// The runtime seeds `MarkerClaims::flushed` from this. That field means
    /// "what the disk holds, as far as we know", and starting it at `None` was
    /// a claim recovery is in a position to contradict: a retraction whose
    /// unlink failed, or a promotion whose write failed, both leave bytes
    /// behind that the process would then believe were gone. The promotion
    /// case is the sharp one — the disk keeps the *suppressible*
    /// `Unflushed{seq}` while memory holds `Lost`, so a later checkpoint can
    /// satisfy the stale witness and silently retract the very report the
    /// promotion exists to make unconditional.
    ///
    /// Internal; not surfaced over UniFFI.
    pub marker_on_disk: deletion_marker::MarkerState,
    /// A previous session's deletion *was* applied by this startup's replay,
    /// but out of the page cache — the flush that failed never happened, so
    /// power loss still undoes it. Not a report: a live durability problem the
    /// engine seeds its runtime ledger from, so the first durable checkpoint
    /// retracts it. Internal; not surfaced over UniFFI.
    pub deletion_pending_checkpoint: bool,
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
            && !self.migration_failed
            && !self.appends_frozen
            && !self.deletion_lost
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
        migration_failed: false,
        appends_frozen: false,
        replayed_deletion: false,
        compaction_recommended: false,
        marker_on_disk: deletion_marker::MarkerState::Absent,
        deletion_lost: false,
        deletion_pending_checkpoint: false,
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
    // The checkpoint's own coverage, before replay moves `applied_seq`. The
    // marker below needs the two apart: a witness the *checkpoint* covers is
    // durably persisted, while one only *replay* reaches is still riding the
    // page cache.
    let checkpoint_applied_seq = history.applied_seq();
    // …and whether it holds anything, before replay can change the answer.
    let checkpoint_empty = history.is_empty();
    let mut wal = HistoryWal::new(checkpoint_path);
    // Read once, here; §3b reuses the value, and nothing between writes the
    // file.
    //
    // A sequence *floor* used to be installed from an outstanding
    // `Unflushed{seq}` here, on the theory that a rebase must not re-issue the
    // number the claim names. That was a mis-model and is gone: the witness
    // test is an **inequality** (`seq > applied_seq`), so any later frame above
    // the witness satisfies it — gaps are legal and `applied_seq` is a high
    // water mark. Skipping one number changes nothing. The seq is a *position
    // within one WAL file*, not an epoch, and it stops meaning anything the
    // moment that file is replaced; only promotion to `Lost` survives a
    // lineage change, which is why promotion is mandatory rather than an
    // optimization, and why a report that is owed with the promotion unlanded
    // freezes appends (see `apply_records`).
    let marker = deletion_marker::read(checkpoint_path);
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
                // This checkpoint was written from `history` itself, so it
                // contains everything memory contains: nothing can be left on
                // disk that memory lacks, and the residue the post-replay
                // `evict()` raised is settled. (The non-migrating startup
                // path writes no checkpoint, so its residue correctly stands
                // until the first compaction.)
                history.reset_durable_residue();
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
        // Derived from the outcome, not from the freeze above: that freeze
        // is conditional on a legacy WAL, so a v1 checkpoint next to a fresh
        // v2 WAL fails the commit without freezing anything.
        report.migration_failed = !report.migrated_from_v1;
    }

    // --- 3b. unpersisted-deletion marker (#312) ---
    // Evaluated here, after the migration commit, because the question it asks
    // is "does the checkpoint that is durable **when this function returns**
    // cover the witness" — and on the migration path this function writes that
    // checkpoint itself. Asking before the commit answered against a v1 file
    // that predates the deletion, so a migration whose save had just persisted
    // the deletion still handed the engine a live durability warning and left
    // the marker for an asynchronous compaction to clean up. One evaluation
    // point against one durable state, rather than a settle-again special case
    // in the migration branch.
    //
    // `durable_applied_seq` is what is on disk now: the checkpoint this startup
    // wrote if the migration committed (it was serialized from `history`, so it
    // covers everything memory holds), otherwise the one that was loaded.
    let (durable_applied_seq, durable_empty) = if report.migrated_from_v1 {
        (history.applied_seq(), history.is_empty())
    } else {
        (checkpoint_applied_seq, checkpoint_empty)
    };
    // A retraction this startup decided but could not carry out. The verdict
    // is permanent — the entries the claim spoke of are gone, and later
    // learning is not them — but the only place to record it is the file we
    // just failed to unlink, so the debt has to be handed to the runtime
    // instead. Recovery is the last marker site outside the single projection
    // writer (`apply_marker`), and this is the hole that left: the runtime's
    // projection does retry a stale file, but only when a compaction runs, and
    // the next one is a thousand frames away. A restart before then reloads
    // the marker against a history that replay has made non-empty, and reports
    // a loss this startup already refuted. Scheduling the compaction *now* is
    // what makes the retry prompt, through the channel other recovery results
    // already use rather than a mechanism of its own.
    let mut marker_retraction_stuck = false;
    if let Some(observed) = marker {
        let breach = observed.breach;
        if breach == deletion_marker::DeletionBreach::Lost && durable_empty && history.is_empty() {
            // `Lost` says an entry survived the deletion. Refuting that takes
            // **both** halves, and each alone was wrong once:
            //
            // - the durable set alone — a checkpoint emptied by `clear` — says
            //   nothing when replay brings entries back from the WAL;
            // - the loaded state alone lets a replayed tombstone empty memory
            //   while the checkpoint still holds the entry, and `decode` maps
            //   *malformed* input to `Lost` too, so a garbled `Unflushed`
            //   would be retracted as a refuted presence claim and a power
            //   loss would restore the entry with nothing reported.
            //
            // Together they say what the claim actually needs: no entry on
            // disk, and none replayed back. That also removes any need to tell
            // a decoded `Lost` from a fallback one — neither is refutable
            // while the checkpoint still holds something.
            info!("an unpersisted-deletion marker outlived the entries it referred to");
            marker_retraction_stuck = !deletion_marker::remove(checkpoint_path);
            if marker_retraction_stuck {
                // `state()`, not the claim: an unlink that failed on a file
                // nobody could read leaves the disk *unknown*, and recording
                // that as absence let the projection skip the retry.
                report.marker_on_disk = observed.state();
            }
        } else if !breach.outstanding(durable_applied_seq) {
            // A durable checkpoint contains the deletion's effect, so it is
            // persisted. Either a crash landed between a successful `save()`
            // and the unlink that follows it, or the migration above just wrote
            // the covering checkpoint. Retracting is sound because the evidence
            // is a checkpoint on disk.
            info!("an unpersisted-deletion marker is covered by a durable checkpoint");
            marker_retraction_stuck = !deletion_marker::remove(checkpoint_path);
            if marker_retraction_stuck {
                report.marker_on_disk = observed.state();
            }
        } else if breach.outstanding(history.applied_seq()) {
            // The frame is provably not in the state we just loaded, so the
            // deletion did not take and nothing will make it take. Promote the
            // claim to unconditional: seq numbering is *re-based* whenever a
            // WAL is quarantined or reinitialized (`adopt_empty` restarts at
            // the checkpoint's applied_seq + 1), so an unrelated later frame
            // could otherwise satisfy this witness and settle a report that is
            // still owed. Having answered the question once, the answer stops
            // depending on a comparison a reset can invalidate.
            warn!("a deletion from a previous session was never persisted ({breach:?})");
            report.deletion_lost = true;
            // Best-effort, and the runtime is told which way it went. A
            // promotion that fails leaves the *suppressible* witness on disk
            // under a memory claim of `Lost`, so the next checkpoint to reach
            // that seq would retract a report that is still owed. Handing the
            // observed value out means the runtime's first projection sees
            // disk != desired and re-asserts, rather than believing a write
            // that never landed.
            // `confirmed` gates the whole thing: a marker that could not be
            // read comes back as `Lost` by the fail-safe rule, and recording
            // that fallback as an observation would have the runtime believe
            // the disk holds `Lost` when it may hold a live `Unflushed`
            // witness. `flushed` would then match, every reconcile would skip,
            // and the witness would sit there until something satisfied it.
            report.marker_on_disk = if !observed.confirmed {
                deletion_marker::MarkerState::Unknown
            } else if breach == deletion_marker::DeletionBreach::Lost {
                deletion_marker::MarkerState::Holds(breach)
            } else {
                match deletion_marker::merge_write(
                    checkpoint_path,
                    deletion_marker::DeletionBreach::Lost,
                ) {
                    Ok(persisted) => deletion_marker::MarkerState::Holds(persisted),
                    Err(e) => {
                        warn!("failed to promote the unpersisted-deletion claim: {e}");
                        deletion_marker::MarkerState::Holds(breach)
                    }
                }
            };
        } else {
            // Replay applied the deletion and no durable checkpoint covers it,
            // so nothing is owed to the user — but replay read that frame out
            // of the page cache, which is not the flush that failed: until a
            // checkpoint covers it, power loss still undoes the deletion.
            // Retracting here would be retract-then-persist, the inverse of the
            // discipline every other write on this path follows. Hand the claim
            // to the runtime ledger instead — it is a live durability problem
            // now, and the first durable checkpoint both settles it and unlinks
            // the file.
            info!("an unflushed deletion replayed; it stands until a checkpoint covers it");
            report.deletion_pending_checkpoint = true;
            report.marker_on_disk = deletion_marker::MarkerState::Holds(breach);
            // Reachable only with a decoded `Unflushed` — an unreadable marker
            // resolves to `Lost`, which is always outstanding and never lands
            // here — so this observation is confirmed by construction.
        }
    }

    // Whatever branch froze the WAL — or left it frozen — this session's
    // appends are memory-only until a compaction heals the file. Derived
    // once, here, so a future freeze site cannot report a clean startup by
    // omission.
    report.appends_frozen = wal.is_frozen();

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
    // A failed migration deliberately schedules nothing on its own. A
    // compaction is not the migration — it writes a v2 checkpoint over the
    // v1 file with none of the commit's steps (no `.v1.bak`, no `Migrated`
    // state) — so using one as the retry would destroy the v1 bytes on
    // exactly the path where the commit is already failing. The next launch
    // re-attempts properly. The legacy-WAL variant still heals, via
    // `appends_frozen` below: there the WAL is frozen, which is a real
    // degradation the compaction genuinely fixes.
    // The two marker feeders below are **promptness, not correctness**. What
    // guarantees the disk eventually agrees with the projection is that every
    // commit reconciles it (`apply_records`, under the wal mutex, before the
    // appends) — a free memory comparison when they already agree. These only
    // spare a user whose startup was degraded from waiting until their next
    // keystroke, and losing one costs nothing but latency. Keeping that
    // division explicit matters: six review rounds were spent adding retry
    // triggers one event at a time, and the answer was never another trigger.
    report.compaction_recommended = marker_retraction_stuck
        // A report is owed but the disk does not yet say so unconditionally:
        // the promotion above failed. Scheduled so the runtime's projection
        // re-asserts `Lost` promptly, because nothing else will — see
        // `deletion_lost`'s doc for why the old "never schedule here" rule was
        // wrong. Skipped when the disk already holds `Lost`, since then the
        // projection and the file agree and a compaction buys nothing.
        || (report.deletion_lost
            && report.marker_on_disk
                != deletion_marker::MarkerState::Holds(deletion_marker::DeletionBreach::Lost))
        || report.migrated_from_v1
        || report.data_loss_suspected()
        || (report.checkpoint_state == CheckpointState::Missing && report.frames_replayed > 0)
        || report.frames_skipped > 0
        || report.replayed_deletion
        || wal.needs_compact()
        || report.appends_frozen;

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
///
/// Deliberately **not** the sweep for the unpersisted-deletion marker, even
/// though that is a family member too: this returns on its first non-NotFound
/// error and only reaches the `.corrupt-*` files afterwards, so letting a
/// stubborn 16-byte sidecar that holds no user text stand in front of files
/// that hold plenty would be the wrong trade. `clear_impl` removes the marker
/// itself. (The same fail-fast shape means a stubborn `.v1.bak` can already
/// block the quarantine sweep, which does hold user text — a pre-existing
/// weakness of this helper, not one the marker introduces.)
pub fn remove_recovery_artifacts(checkpoint_path: &Path) -> io::Result<()> {
    for path in [
        v1_backup_path(checkpoint_path),
        persist::tmp_path(checkpoint_path),
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
///
/// Best-effort by design (LXUD v2 decision #13): the backup is a manual
/// rescue hatch for downgrades and migration bugs, **not** a correctness
/// dependency, and migration proceeds whether or not it lands. Making it a
/// precondition was tried and reverted — see the AGENTS.md settled note.
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
