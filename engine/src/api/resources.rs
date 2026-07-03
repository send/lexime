use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

use tracing::warn;

use crate::dict::connection::ConnectionMatrix;
use crate::dict::{CompositeDictionary, Dictionary, TrieDictionary};
use crate::session::LearningRecord;
use crate::user_history::recovery::{CheckpointState, OpenReport, WalState};
use crate::user_history::wal::{AppendError, HistoryWal, WalRecord};
use crate::user_history::UserHistory;

use super::{LexError, LexUserDictionary};

// ---------------------------------------------------------------------------
// Poison recovery
//
// The history locks recover through poisoning rather than panicking or
// bailing (apply_records is reached synchronously from every handle_key /
// commit, so an unwrap here would surface a panic at the FFI boundary on
// every subsequent keystroke). Recovery is sound for these locks
// specifically:
// - `wal`: seq assignment is monotonic and the file is append-only; a
//   panicking holder can at worst leave a torn frame, which replay's tail
//   repair already handles.
// - `inner`: mutations are WAL-ahead (§5.2) — whatever half-state a panic
//   left behind, a restart replays the durable frames and self-heals.
//   `advance_applied_seq` is a monotonic max and cannot corrupt further.
// ---------------------------------------------------------------------------

fn lock_recover<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn read_recover<T>(l: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    l.read().unwrap_or_else(PoisonError::into_inner)
}

fn write_recover<T>(l: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    l.write().unwrap_or_else(PoisonError::into_inner)
}

#[derive(uniffi::Object)]
pub struct LexDictionary {
    pub(crate) inner: Arc<dyn Dictionary>,
}

#[uniffi::export]
impl LexDictionary {
    #[uniffi::constructor]
    fn open(path: String) -> Result<Arc<Self>, LexError> {
        let dict = TrieDictionary::open(Path::new(&path))?;
        Ok(Arc::new(Self {
            inner: Arc::new(dict),
        }))
    }

    #[uniffi::constructor]
    fn open_with_user_dict(
        path: String,
        user_dict: Option<Arc<LexUserDictionary>>,
    ) -> Result<Arc<Self>, LexError> {
        let trie = TrieDictionary::open(Path::new(&path))?;

        let inner: Arc<dyn Dictionary> = match user_dict {
            Some(ud) => {
                let trie_layer: Arc<dyn Dictionary> = Arc::new(trie);
                let user_layer: Arc<dyn Dictionary> = Arc::clone(&ud.inner) as _;
                let composite = CompositeDictionary::new(vec![trie_layer, user_layer]);
                Arc::new(composite)
            }
            None => Arc::new(trie),
        };

        Ok(Arc::new(Self { inner }))
    }

    fn lookup(&self, reading: String) -> Vec<super::LexDictEntry> {
        self.inner
            .lookup(&reading)
            .iter()
            .map(|e| super::LexDictEntry {
                reading: reading.clone(),
                surface: e.surface.clone(),
                cost: e.cost,
            })
            .collect()
    }
}

#[derive(uniffi::Object)]
pub struct LexConnection {
    pub(crate) inner: Arc<ConnectionMatrix>,
}

#[uniffi::export]
impl LexConnection {
    #[uniffi::constructor]
    fn open(path: String) -> Result<Arc<Self>, LexError> {
        let conn = ConnectionMatrix::open(Path::new(&path))?;
        Ok(Arc::new(Self {
            inner: Arc::new(conn),
        }))
    }
}

#[derive(uniffi::Object)]
pub struct LexUserHistory {
    pub(crate) inner: Arc<RwLock<UserHistory>>,
    wal: Mutex<HistoryWal>,
    /// Compaction exclusivity (design decision #11): threshold compactions
    /// skip when the gate is held (`try_lock`); clear, the deletion path,
    /// and heal requests park on it. The Mutex guards no data — it only
    /// serializes "one compaction at a time" — so recovering it through
    /// poisoning is trivially safe, and the guard's Drop releases it on
    /// panic (UniFFI catches panics at the FFI boundary, and a panicking
    /// background thread does not kill the process).
    compact_gate: Mutex<()>,
    /// §5.3 step 5: a posted compaction request whose snapshot requirement
    /// may postdate an in-flight run. Posted before parking on the gate;
    /// whoever runs a compaction consumes it before snapshotting, so a
    /// parked requester that wakes to find it consumed skips its redundant
    /// run.
    scrub_pending: AtomicBool,
    /// Append-only JSONL log of commit events (rank, top-1 acceptance),
    /// stored next to the checkpoint. Local diagnostics only; mined offline
    /// by lextool to track the real-world top-1 acceptance rate. The mutex
    /// serializes appends across all sessions sharing this history so
    /// concurrent commits cannot interleave partial lines.
    commit_log: Mutex<CommitLog>,
    /// What recovery found and did at open time (§10). Served to Swift via
    /// `open_report()` for the degraded-state menu (data loss) and NSLog
    /// (benign events).
    report: OpenReport,
}

/// UniFFI mirror of [`CheckpointState`].
#[derive(uniffi::Enum, Clone, Copy, Debug)]
pub enum LexHistoryCheckpointState {
    Loaded,
    Migrated,
    Missing,
    Quarantined,
}

/// UniFFI mirror of [`WalState`].
#[derive(uniffi::Enum, Clone, Copy, Debug)]
pub enum LexHistoryWalState {
    Clean,
    Missing,
    TailRepaired,
    Quarantined,
    LegacyDiscarded,
    Reinitialized,
    RepairFailed,
}

/// What `open` found and did (§10). The policy bits are computed on the
/// Rust side so Swift stays purely presentational: `data_loss_suspected`
/// carries the §8 visibility rule (only whole-file quarantine is
/// user-visible), `clean` the "nothing noteworthy happened" rule.
#[derive(uniffi::Record)]
pub struct LexHistoryOpenReport {
    pub checkpoint_state: LexHistoryCheckpointState,
    pub wal_state: LexHistoryWalState,
    pub migrated_from_v1: bool,
    pub frames_replayed: u64,
    pub frames_skipped: u64,
    pub quarantined_paths: Vec<String>,
    pub data_loss_suspected: bool,
    pub clean: bool,
}

impl From<&OpenReport> for LexHistoryOpenReport {
    fn from(r: &OpenReport) -> Self {
        Self {
            checkpoint_state: match r.checkpoint_state {
                CheckpointState::Loaded => LexHistoryCheckpointState::Loaded,
                CheckpointState::Migrated => LexHistoryCheckpointState::Migrated,
                CheckpointState::Missing => LexHistoryCheckpointState::Missing,
                CheckpointState::Quarantined => LexHistoryCheckpointState::Quarantined,
            },
            wal_state: match r.wal_state {
                WalState::Clean => LexHistoryWalState::Clean,
                WalState::Missing => LexHistoryWalState::Missing,
                WalState::TailRepaired => LexHistoryWalState::TailRepaired,
                WalState::Quarantined => LexHistoryWalState::Quarantined,
                WalState::LegacyDiscarded => LexHistoryWalState::LegacyDiscarded,
                WalState::Reinitialized => LexHistoryWalState::Reinitialized,
                WalState::RepairFailed => LexHistoryWalState::RepairFailed,
            },
            migrated_from_v1: r.migrated_from_v1,
            frames_replayed: r.frames_replayed,
            frames_skipped: r.frames_skipped,
            // Lossy on purpose: display-only, and a non-UTF-8 path must not
            // panic at the FFI boundary.
            quarantined_paths: r
                .quarantined_paths
                .iter()
                .map(|p| p.display().to_string())
                .collect(),
            data_loss_suspected: r.data_loss_suspected(),
            clean: r.is_clean(),
        }
    }
}

/// Lazily-opened append handle for the commit log, mirroring HistoryWal.
struct CommitLog {
    path: PathBuf,
    file: Option<std::fs::File>,
}

impl CommitLog {
    fn append(&mut self, line: &str) -> std::io::Result<()> {
        if self.file.is_none() {
            // Mirror HistoryWal::open_file: on a fresh install the first
            // logged event can precede any WAL append, so the parent
            // directory may not exist yet.
            if let Some(parent) = self.path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            self.file = Some(
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&self.path)?,
            );
        }
        let f = self.file.as_mut().expect("file set by preceding lines");
        // Single write_all per line (payload + newline in one buffer) so a
        // line can never be split even if the file handle is shared.
        let mut buf = Vec::with_capacity(line.len() + 1);
        buf.extend_from_slice(line.as_bytes());
        buf.push(b'\n');
        f.write_all(&buf)
    }
}

#[uniffi::export]
impl LexUserHistory {
    #[uniffi::constructor]
    fn open(path: String) -> Result<Arc<Self>, LexError> {
        let cp = Path::new(&path);
        // Recovery-mode open: corruption is quarantined instead of failing
        // startup, so learning never silently stops. Err = environmental
        // read failure only (EACCES etc.). Swift consumes the report via
        // `open_report()`; the logs here cover headless/CLI contexts.
        let (history, wal, report) = crate::user_history::recovery::open_recovering(cp)?;
        if report.data_loss_suspected() {
            warn!("user history recovered with suspected data loss: {report:?}");
        } else if !report.is_clean() {
            tracing::info!("user history recovery events: {report:?}");
        }
        let commit_log = CommitLog {
            path: cp.with_file_name("commit-log.jsonl"),
            file: None,
        };
        let this = Arc::new(Self {
            inner: Arc::new(RwLock::new(history)),
            wal: Mutex::new(wal),
            compact_gate: Mutex::new(()),
            scrub_pending: AtomicBool::new(false),
            commit_log: Mutex::new(commit_log),
            report,
        });
        // Startup compaction (§5.1-6): checkpoint recovery results early so
        // the next startup is clean. This is also the heal path for a
        // frozen WAL (failed tail repair / failed migration commit): the
        // checkpoint covers memory, so the truncation that follows it
        // restores appendable v2 form — without this, appends would keep
        // failing and threshold-based compaction would never trigger.
        if this.report.compaction_recommended {
            this.spawn_compact();
        }
        Ok(this)
    }

    /// What recovery found and did at open time (§10).
    fn open_report(&self) -> LexHistoryOpenReport {
        (&self.report).into()
    }
}

impl LexUserHistory {
    /// Apply a batch of learning records (§5.2): WAL append first
    /// (write-ahead), then the in-memory apply — both in one critical
    /// section under the wal mutex (§4).
    ///
    /// The coupling is what makes `applied_seq = S` mean "every frame ≤ S
    /// is applied": uncoupled, two sessions could append seq 5 then 6,
    /// apply 6 first, and a snapshot taken at that instant would claim
    /// coverage of 5 while lacking its effect — the conditional truncation
    /// (§5.3) would then destroy frame 5. Crash consequences: "on the WAL
    /// but not in memory" self-heals via restart replay; the v1 direction
    /// (in memory but not on the WAL = loss) is structurally gone.
    pub(super) fn apply_records(self: &Arc<Self>, records: &[LearningRecord]) {
        let now = crate::user_history::now_epoch();
        let mut wal_records: Vec<WalRecord> = Vec::new();
        let mut log_lines: Vec<String> = Vec::new();
        for r in records {
            match r {
                LearningRecord::Committed {
                    reading,
                    surface,
                    segments,
                    rank,
                    top1,
                    auto,
                    learn,
                } => {
                    if *learn {
                        wal_records.push(WalRecord::Committed {
                            segments: vec![(reading.clone(), surface.clone())],
                            timestamp: now,
                        });
                        if let Some(sub_segs) = segments {
                            wal_records.push(WalRecord::Committed {
                                segments: sub_segs.clone(),
                                timestamp: now,
                            });
                        }
                    }
                    log_lines.push(commit_log_line(
                        now,
                        reading,
                        surface,
                        *rank,
                        top1.as_deref(),
                        *auto,
                    ));
                }
                LearningRecord::Deletion { segments } => {
                    wal_records.push(WalRecord::Tombstone {
                        segments: segments.clone(),
                        timestamp: now,
                    });
                }
            }
        }

        // One flag decides the compaction mode below; both causes need the
        // same immediate gated run:
        // - Tombstone written: physical scrub (§5.4) — the deleted strings
        //   still sit in the old checkpoint and past Committed frames until
        //   a compaction rewrites the checkpoint and truncates the WAL.
        // - Append failure: the WAL may be frozen; only a post-checkpoint
        //   truncation restores appendable form (heal), and the entry-count
        //   threshold can no longer reach it.
        let mut scrub = false;
        // A Tombstone whose WAL durability failed (SyncFailed or Io): the
        // deletion must be checkpointed synchronously before returning (§5.4),
        // not left to an async scrub that a crash could preempt.
        let mut durability_failed = false;
        let mut needs_threshold_compact = false;
        if !wal_records.is_empty() {
            let mut wal = lock_recover(&self.wal);
            let mut sequenced: Vec<(WalRecord, Option<u64>)> =
                Vec::with_capacity(wal_records.len());
            for record in wal_records {
                if let WalRecord::Tombstone { segments, .. } = &record {
                    // No-op deletion: pruning a candidate that was never
                    // learned (the common ForwardDelete case) must not cost
                    // a key-thread F_FULLFSYNC. Check-then-append is
                    // race-free because every history mutation runs under
                    // the wal mutex we hold (wal -> inner is the §4 order).
                    if !read_recover(&self.inner).contains_entries(segments) {
                        continue;
                    }
                    scrub = true;
                }
                match wal.append_record(&record) {
                    Ok(seq) => sequenced.push((record, Some(seq))),
                    // Frame on disk, durability unconfirmed: apply with the
                    // real seq — leaving an on-disk frame uncovered would
                    // stall conditional truncation indefinitely (AppendError
                    // docs) — and fall back to the checkpoint for the
                    // deletion's durability (§5.4): the scrub compaction
                    // scheduled below persists it in full.
                    Err(AppendError::SyncFailed { seq, source }) => {
                        warn!(
                            "tombstone durability sync failed (synchronous checkpoint fallback): {source}"
                        );
                        // SyncFailed only arises from a Tombstone append, and a
                        // real (non-no-op) Tombstone already set `scrub` above.
                        debug_assert!(scrub, "SyncFailed implies a scrubbing Tombstone");
                        // The frame is on disk (process-crash safe via replay)
                        // but its F_FULLFSYNC failed, reopening the power-loss
                        // window the Tombstone contract closes. Persist the
                        // deletion synchronously below rather than resurrect it
                        // if the async scrub is preempted.
                        durability_failed = true;
                        sequenced.push((record, Some(seq)));
                    }
                    // Frame not on disk: memory still applies — a confirmed
                    // conversion that stops boosting is an immediate quality
                    // regression (§5.2) — but applied_seq must not advance
                    // (None), or a checkpoint would claim coverage of a
                    // frame that cannot replay. The next checkpoint is a
                    // full snapshot, so a recovered disk picks the effect
                    // up; the frame's absence makes double-apply impossible.
                    Err(e @ AppendError::Io(_)) => {
                        warn!("{e}");
                        scrub = true;
                        // A Tombstone whose frame never reached disk has no
                        // durable representation at all (memory-only): persist
                        // the deletion synchronously below (privacy), not just
                        // via the async heal a re-learnable Committed loss can
                        // wait for.
                        if matches!(record, WalRecord::Tombstone { .. }) {
                            durability_failed = true;
                        }
                        sequenced.push((record, None));
                    }
                }
            }
            needs_threshold_compact = wal.needs_compact();
            if !sequenced.is_empty() {
                write_recover(&self.inner).apply_batch(&sequenced);
            }
        }

        for line in &log_lines {
            self.append_commit_log(line);
        }

        if durability_failed {
            // A Tombstone could not be made durable through the WAL. Write
            // the checkpoint synchronously before returning so the deletion
            // survives a crash instead of resurrecting if an async scrub is
            // preempted — v1 force_compact's synchronous intent, kept as a
            // failure-only fallback (§5.4). Only the rare failing-disk path
            // pays this key-thread cost; every healthy delete stays async.
            self.scrub_pending.store(true, Ordering::SeqCst);
            self.run_gated_compact();
        } else if scrub {
            self.spawn_compact();
        } else if needs_threshold_compact {
            self.spawn_threshold_compact();
        }
    }

    /// §5.5 clear: empty-checkpoint-first. The empty checkpoint (stamped
    /// with the highest on-disk seq) is the commit point — once its rename
    /// lands, every leftover frame replays as a skip and any crash
    /// converges to an empty history. The v1 file-deletion protocol passed
    /// through "checkpoint missing + WAL non-empty", a state replay can
    /// resurrect.
    pub(super) fn clear_impl(self: &Arc<Self>) -> Result<(), LexError> {
        // Park until any in-flight compaction finishes (§5.5 step 1): a
        // compactor that cloned a pre-clear snapshot must not save it after
        // the wipe, or the wipe would be undone. Holding the gate also
        // blocks new compactions for the duration.
        let _gate = self.lock_gate();
        // The wal mutex blocks concurrent apply_records for the whole
        // protocol. inner is taken only for the memory reset below — the
        // checkpoint write needs no lock on it (it serializes an empty
        // history), so conversion reads stay unblocked during the I/O.
        let mut wal = lock_recover(&self.wal);

        // Commit point. On Err nothing has changed — including
        // scrub_pending, so a pre-clear Tombstone's scrub request stays
        // posted for the next compaction.
        let mut empty = UserHistory::new();
        empty.advance_applied_seq(wal.last_appended_seq());
        empty.save(wal.checkpoint_path())?;

        // Consumed only after the commit point: the wipe supersedes every
        // scrub request posted so far.
        self.scrub_pending.store(false, Ordering::SeqCst);

        // Physical deletions below are deferred-error: the logical clear is
        // committed, so every step runs (the memory reset especially —
        // bailing before it would let the next compaction re-persist the
        // pre-clear state from memory), and the first failure is surfaced
        // at the end.
        let mut deferred: Option<LexError> = None;

        if let Err(e) = wal.truncate_wal() {
            // The old frames remain on disk. They replay as skips (covered
            // by the empty checkpoint), but a privacy wipe must not
            // silently leave input strings behind — freeze appends (file
            // state unknown) and surface the failure.
            warn!("clear: WAL truncation failed: {e}");
            wal.freeze();
            deferred.get_or_insert(e.into());
        }

        {
            let mut h = write_recover(&self.inner);
            // applied_seq carries over (§5.5 step 5); next_seq in the wal
            // continues its numbering (§3).
            *h = empty;
        }

        // Drop the commit-log handle before removing the file so a later
        // append reopens (re-creates) it instead of writing to an unlinked
        // inode.
        let commit_log_path = {
            let mut log = lock_recover(&self.commit_log);
            log.file = None;
            log.path.clone()
        };
        match std::fs::remove_file(&commit_log_path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                // A privacy wipe must not leave input strings on disk, and
                // the commit-log — unlike the WAL — has no startup scrub
                // backstop (recovery never touches it). If it can't be
                // unlinked (an external reader holds the inode, or the
                // parent dir isn't writable), truncate it to zero instead:
                // unlink needs parent-dir write while truncation needs only
                // file write, so it can succeed where unlink can't, and
                // zero-length scrubbing matches the WAL's own clear (§0
                // excludes physical byte erasure). Only a failure of *both*
                // is deferred and surfaced.
                warn!("clear: commit-log removal failed ({e}); truncating to scrub");
                if let Err(trunc) = std::fs::File::create(&commit_log_path) {
                    warn!("clear: commit-log truncation also failed: {trunc}");
                    deferred.get_or_insert(e.into());
                }
            }
        }
        // Privacy wipe: quarantined bytes and the v1 migration backup must
        // not survive a clear.
        if let Err(e) =
            crate::user_history::recovery::remove_recovery_artifacts(wal.checkpoint_path())
        {
            warn!("clear: recovery-artifact removal failed: {e}");
            deferred.get_or_insert(e.into());
        }
        drop(wal);

        match deferred {
            None => Ok(()),
            Some(e) => {
                // Partial physical failure: the logical clear is done
                // (memory and checkpoint are empty) but some bytes remain.
                // Post a heal — the compaction re-truncates the frozen WAL
                // against the empty state — and surface the error so the
                // UI can say the wipe is incomplete (never silent).
                self.spawn_compact();
                Err(e)
            }
        }
    }

    /// Acquire the compaction gate. Poison recovery is trivially safe here:
    /// the gate protects no data (see the field docs).
    fn lock_gate(&self) -> MutexGuard<'_, ()> {
        lock_recover(&self.compact_gate)
    }

    /// Append one JSONL line to the commit log. Failures are logged and
    /// swallowed — diagnostics must never break the commit path.
    pub(super) fn append_commit_log(&self, line: &str) {
        let mut log = match self.commit_log.lock() {
            Ok(l) => l,
            Err(e) => {
                warn!("commit-log lock poisoned: {e}");
                return;
            }
        };
        if let Err(e) = log.append(line) {
            warn!("commit-log append failed: {e}");
        }
    }

    /// Spawn a threshold compaction (the caller has already observed
    /// `needs_compact()` under the wal lock).
    fn spawn_threshold_compact(self: &Arc<Self>) {
        // §4: threshold compactions skip when one is in flight — the
        // threshold stays exceeded and the next commit retries. Advisory
        // pre-check to avoid spawning a thread per commit while a
        // compaction runs; the spawned thread re-checks with its own
        // try_lock.
        match self.compact_gate.try_lock() {
            Ok(gate) => drop(gate),
            Err(std::sync::TryLockError::WouldBlock) => return,
            Err(std::sync::TryLockError::Poisoned(_)) => {}
        }
        let this = Arc::clone(self);
        if let Err(e) = std::thread::Builder::new()
            .name("lexime-history-compact".into())
            .spawn(move || {
                let _gate = match this.compact_gate.try_lock() {
                    Ok(g) => g,
                    Err(std::sync::TryLockError::WouldBlock) => return,
                    Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner(),
                };
                // This run's snapshot covers heal requests posted so far.
                this.scrub_pending.store(false, Ordering::SeqCst);
                let outcome = this.run_compact_impl();
                this.after_compact(outcome);
            })
        {
            warn!("failed to spawn compaction thread: {e}");
        }
    }

    /// React to a [`run_compact_impl`](Self::run_compact_impl) outcome: chain
    /// a follow-up for reclaimable frames, or re-post the scrub if the
    /// checkpoint did not become durable so a later trigger retries it —
    /// never silently dropping a delete that is only in memory (or an
    /// unconfirmed WAL frame).
    fn after_compact(self: &Arc<Self>, outcome: CompactOutcome) {
        match outcome {
            CompactOutcome::Done => {}
            CompactOutcome::FollowUp => self.spawn_compact(),
            CompactOutcome::Failed => self.scrub_pending.store(true, Ordering::SeqCst),
        }
    }

    /// Acquire the gate and run one compaction if a scrub is still pending
    /// (see [`Self::after_compact`] for outcome handling). Shared by
    /// [`Self::spawn_compact`] (async) and the synchronous durability-failure
    /// fallback in [`Self::apply_records`] (§5.4). The caller posts
    /// `scrub_pending` first so a concurrent gated run can absorb the request.
    fn run_gated_compact(self: &Arc<Self>) {
        let _gate = self.lock_gate();
        if self.scrub_pending.swap(false, Ordering::SeqCst) {
            let outcome = self.run_compact_impl();
            self.after_compact(outcome);
        }
    }

    /// Post a compaction request that must run with a snapshot no older
    /// than now (startup recovery, WAL-append-failure healing) and park a
    /// worker on the gate until it can run. If an intervening gated run —
    /// which consumes `scrub_pending` before snapshotting — already covered
    /// the request, the worker wakes to a consumed flag and skips its
    /// redundant run.
    fn spawn_compact(self: &Arc<Self>) {
        self.scrub_pending.store(true, Ordering::SeqCst);
        let this = Arc::clone(self);
        if let Err(e) = std::thread::Builder::new()
            .name("lexime-history-compact".into())
            .spawn(move || this.run_gated_compact())
        {
            // scrub_pending stays posted; the next request (or the next
            // gated run's consume) picks it up.
            warn!("failed to spawn compaction thread: {e}");
        }
    }

    #[cfg(test)]
    fn run_compact(&self) -> CompactOutcome {
        self.run_compact_impl()
    }

    /// Write a checkpoint and conditionally truncate the WAL, reporting
    /// whether the checkpoint became durable and whether a follow-up is
    /// warranted (see [`CompactOutcome`]).
    fn run_compact_impl(&self) -> CompactOutcome {
        // 1. Clone history under read lock (brief)
        let snapshot = read_recover(&self.inner).clone();
        let cp_path = lock_recover(&self.wal).checkpoint_path().to_path_buf();

        // 2. Write checkpoint (no locks held, slow I/O)
        if let Err(e) = snapshot.save(&cp_path) {
            warn!("checkpoint write failed: {e}");
            return CompactOutcome::Failed;
        }

        // 3. Truncate WAL (brief lock) — conditionally (§5.3): only frames
        // provably covered by the durable checkpoint (seq <= applied_seq at
        // snapshot time) may be destroyed.
        let mut wal = lock_recover(&self.wal);
        match wal.truncate_covered(snapshot.applied_seq()) {
            Ok(true) => CompactOutcome::Done,
            Ok(false) => {
                // Frames landed after our snapshot: request a follow-up
                // pass (§5.3 step 5), which keeps the §5.4 scrub prompt. A
                // Tombstone racing this run's checkpoint write would
                // otherwise leave the deleted strings — its own frame plus
                // the old Committed frames — on disk until the next
                // threshold compaction. Correctness is unaffected either
                // way (WAL-ahead means a snapshot can never contain
                // unsequenced effects, so a skipped truncation only costs
                // file bytes); convergence holds because every skip means
                // "frames landed after the snapshot" and the follow-up's
                // snapshot covers them (SyncFailed tombstones carry their
                // real seq, so no on-disk frame stays uncovered forever).
                CompactOutcome::FollowUp
            }
            Err(e) => {
                // The checkpoint IS durable (save succeeded), so the deletion
                // is persisted; only the physical WAL scrub is deferred. The
                // leftover frames are covered (replay skips them) and the
                // next threshold/startup compaction retries the truncation —
                // re-posting here would risk a tight loop on a stuck disk.
                warn!("WAL truncate failed: {e}");
                CompactOutcome::Done
            }
        }
    }
}

/// Outcome of a single [`LexUserHistory::run_compact_impl`] pass, so callers
/// can tell a durable checkpoint from a failed write. The boolean this
/// replaced conflated "done" with "save failed", letting the durability-
/// failure path (§5.4) consume the pending scrub and return without any
/// durable checkpoint or guaranteed retry.
enum CompactOutcome {
    /// Durable checkpoint written and the WAL truncated to cover it. A failed
    /// *truncation* also lands here: the checkpoint is durable (the deletion
    /// is persisted); the leftover covered frames are reclaimed by the next
    /// threshold/startup compaction (the `frames_skipped` backstop).
    Done,
    /// Durable checkpoint written, but frames landed after the snapshot so the
    /// covered-only truncation was skipped — a follow-up pass reclaims them
    /// once writes settle.
    FollowUp,
    /// The checkpoint write failed, so nothing became durable. The scrub is
    /// unmet and must stay pending for a later retry.
    Failed,
}

/// Serialize one commit event as a JSONL line for the commit log.
/// `top1` and `auto` are omitted at their defaults to keep lines lean.
fn commit_log_line(
    now: u64,
    reading: &str,
    surface: &str,
    rank: usize,
    top1: Option<&str>,
    auto: bool,
) -> String {
    let mut obj = serde_json::json!({
        "t": now,
        "reading": reading,
        "surface": surface,
        "rank": rank,
    });
    if let Some(t1) = top1 {
        obj["top1"] = serde_json::Value::String(t1.to_string());
    }
    if auto {
        obj["auto"] = serde_json::Value::Bool(true);
    }
    obj.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_hist(cp: &Path) -> Arc<LexUserHistory> {
        LexUserHistory::open(cp.display().to_string()).unwrap()
    }

    fn committed(reading: &str, surface: &str) -> LearningRecord {
        LearningRecord::Committed {
            reading: reading.to_string(),
            surface: surface.to_string(),
            segments: None,
            rank: 0,
            top1: None,
            auto: false,
            learn: true,
        }
    }

    fn deletion(reading: &str, surface: &str) -> LearningRecord {
        LearningRecord::Deletion {
            segments: vec![(reading.to_string(), surface.to_string())],
        }
    }

    fn learned(hist: &LexUserHistory, reading: &str) -> Vec<String> {
        let now = crate::user_history::now_epoch();
        read_recover(&hist.inner)
            .learned_surfaces(reading, now)
            .into_iter()
            .map(|(s, _)| s)
            .collect()
    }

    /// Wait for an async (compaction-thread) condition, with a deadline.
    fn wait_until(mut cond: impl FnMut() -> bool, what: &str) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !cond() {
            assert!(
                std::time::Instant::now() < deadline,
                "timeout waiting for {what}"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// Construct a LexUserHistory over a mock WalIo (fault injection).
    fn hist_with_io(
        cp: &Path,
        io: Box<dyn crate::user_history::wal::WalIo>,
    ) -> Arc<LexUserHistory> {
        Arc::new(LexUserHistory {
            inner: Arc::new(RwLock::new(UserHistory::new())),
            wal: Mutex::new(HistoryWal::with_io(cp, io)),
            compact_gate: Mutex::new(()),
            scrub_pending: AtomicBool::new(false),
            commit_log: Mutex::new(CommitLog {
                path: cp.with_file_name("commit-log.jsonl"),
                file: None,
            }),
            report: OpenReport {
                checkpoint_state: CheckpointState::Missing,
                wal_state: WalState::Missing,
                migrated_from_v1: false,
                frames_replayed: 0,
                frames_skipped: 0,
                quarantined_paths: Vec::new(),
                compaction_recommended: false,
            },
        })
    }

    #[test]
    fn test_commit_log_line_shape() {
        // Manual pick: top1 present, auto omitted
        let line = commit_log_line(42, "きょう", "京", 2, Some("今日"), false);
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["t"], 42);
        assert_eq!(v["reading"], "きょう");
        assert_eq!(v["surface"], "京");
        assert_eq!(v["rank"], 2);
        assert_eq!(v["top1"], "今日");
        assert!(v.get("auto").is_none());

        // Auto-commit acceptance: top1 omitted, auto present
        let line = commit_log_line(42, "きょう", "今日", 0, None, true);
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert!(v.get("top1").is_none());
        assert_eq!(v["auto"], true);
    }

    #[test]
    fn test_commit_log_append_and_clear() {
        let dir = tempfile::tempdir().unwrap();
        // Nested path that does not exist yet: the first append must create
        // the parent directory (fresh-install case).
        let cp = dir.path().join("nested").join("history.lxud");
        let hist = open_hist(&cp);

        hist.append_commit_log(r#"{"t":1,"reading":"きょう","surface":"今日","rank":0}"#);
        hist.append_commit_log(
            r#"{"t":2,"reading":"きょう","surface":"京","rank":2,"top1":"今日"}"#,
        );

        let log_path = dir.path().join("nested").join("commit-log.jsonl");
        let content = std::fs::read_to_string(&log_path).unwrap();
        assert_eq!(content.lines().count(), 2);

        // clear() removes the commit log along with history data
        hist.clear_impl().unwrap();
        assert!(!log_path.exists(), "commit log should be removed by clear");
    }

    #[test]
    fn test_apply_records_wal_ahead_and_durable() {
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("history.lxud");
        let hist = open_hist(&cp);

        hist.apply_records(&[committed("きょう", "今日")]);
        assert_eq!(learned(&hist, "きょう"), vec!["今日".to_string()]);
        {
            let wal = lock_recover(&hist.wal);
            assert_eq!(wal.entry_count(), 1);
            assert_eq!(
                read_recover(&hist.inner).applied_seq(),
                wal.last_appended_seq(),
                "coupled append+apply must leave applied_seq covering the file"
            );
        }

        // The frame alone (no checkpoint yet) reconstructs the state.
        drop(hist);
        let hist2 = open_hist(&cp);
        assert_eq!(learned(&hist2, "きょう"), vec!["今日".to_string()]);
    }

    #[test]
    fn test_noop_deletion_writes_no_tombstone() {
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("history.lxud");
        let hist = open_hist(&cp);

        // Deleting a never-learned pair (dictionary candidate pruning) must
        // not write a frame — that would cost a key-thread F_FULLFSYNC per
        // ForwardDelete.
        hist.apply_records(&[deletion("きょう", "今日")]);
        assert_eq!(lock_recover(&hist.wal).entry_count(), 0);
        assert!(
            !hist.scrub_pending.load(Ordering::SeqCst),
            "no scrub needed for a no-op deletion"
        );
    }

    #[test]
    fn test_deletion_scrubs_checkpoint_and_wal() {
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("history.lxud");
        let hist = open_hist(&cp);

        hist.apply_records(&[committed("きょう", "今日")]);
        // Persist the entry into a checkpoint so the scrub has something to
        // rewrite. A quiescent compaction writes a durable checkpoint and
        // truncates the covered frames (CompactOutcome::Done).
        assert!(
            matches!(hist.run_compact(), CompactOutcome::Done),
            "quiescent compaction should report a durable, truncated checkpoint"
        );
        assert!(cp.exists());

        hist.apply_records(&[deletion("きょう", "今日")]);
        assert!(
            learned(&hist, "きょう").is_empty(),
            "memory delete is immediate"
        );

        // The posted scrub compaction rewrites the checkpoint and truncates
        // the WAL (tombstone included) in the background.
        wait_until(
            || lock_recover(&hist.wal).entry_count() == 0,
            "scrub compaction to truncate the WAL",
        );

        // Nothing on disk resurrects the deletion.
        drop(hist);
        let hist2 = open_hist(&cp);
        assert!(
            learned(&hist2, "きょう").is_empty(),
            "deletion survives reopen"
        );
    }

    #[test]
    fn test_tombstone_deletes_before_scrub_after_crash() {
        // Crash between the tombstone append and the scrub compaction: the
        // checkpoint still contains the entry, the WAL carries the
        // tombstone. Replay must apply the deletion.
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("history.lxud");
        {
            // Build the crash state directly (no background threads): a
            // checkpoint containing the entry + a WAL holding only the
            // tombstone frame.
            let mut h = UserHistory::new();
            h.record_at(&[("きょう".to_string(), "今日".to_string())], 1000);
            let mut wal = HistoryWal::new(&cp);
            let seq = wal
                .append_record(&crate::user_history::wal::WalRecord::Tombstone {
                    segments: vec![("きょう".to_string(), "今日".to_string())],
                    timestamp: 1001,
                })
                .unwrap();
            assert_eq!(seq, 1);
            h.save(&cp).unwrap(); // applied_seq = 0 < 1: tombstone uncovered
        }
        let hist = open_hist(&cp);
        assert!(
            learned(&hist, "きょう").is_empty(),
            "uncovered tombstone must replay as a deletion"
        );
    }

    #[test]
    fn test_clear_leaves_empty_checkpoint_and_header_wal() {
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("history.lxud");
        let hist = open_hist(&cp);

        hist.apply_records(&[committed("きょう", "今日")]);
        hist.run_compact();
        let wal_path = lock_recover(&hist.wal).wal_path().to_path_buf();
        assert!(cp.exists(), "checkpoint should exist before clear");

        hist.clear_impl().unwrap();

        assert!(
            learned(&hist, "きょう").is_empty(),
            "in-memory history should be empty after clear"
        );
        // §5.5: clear leaves an empty checkpoint + header-only WAL — never
        // the resurrectable "checkpoint missing + WAL non-empty" state.
        assert!(cp.exists(), "empty checkpoint remains (commit point)");
        assert!(wal_path.exists(), "header-only WAL remains");
        assert_eq!(lock_recover(&hist.wal).entry_count(), 0);

        // Post-clear learning still works and survives reopen.
        hist.apply_records(&[committed("あす", "明日")]);
        drop(hist);
        let hist2 = open_hist(&cp);
        assert!(
            learned(&hist2, "きょう").is_empty(),
            "cleared data stays gone"
        );
        assert_eq!(learned(&hist2, "あす"), vec!["明日".to_string()]);
    }

    #[test]
    fn test_clear_supersedes_in_flight_compaction() {
        // T7: a compaction racing clear must not save its pre-clear
        // snapshot after the wipe. The gate serializes them: whichever
        // order the OS picks, the final on-disk state is empty.
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("history.lxud");
        let hist = open_hist(&cp);

        for i in 0..50 {
            hist.apply_records(&[committed(&format!("よみ{i}"), &format!("面{i}"))]);
        }
        // Post gated compaction requests that will race the clear.
        for _ in 0..4 {
            hist.spawn_compact();
        }
        hist.clear_impl().unwrap();

        // Any parked compactor that runs after clear snapshots the empty
        // state; give stragglers time to finish, then verify emptiness
        // holds on disk.
        wait_until(
            || {
                let gate = hist.lock_gate();
                drop(gate);
                let (h, _wal) =
                    crate::user_history::wal::open_with_wal(&cp).expect("reopen after clear");
                h.learned_surfaces("よみ0", u64::MAX).is_empty() && h.unigrams().next().is_none()
            },
            "post-clear disk state to settle empty",
        );
        assert!(
            learned(&hist, "よみ0").is_empty(),
            "no resurrection in memory"
        );
    }

    // -----------------------------------------------------------------------
    // T6 (engine side): append-failure fallbacks over a mock WalIo
    // -----------------------------------------------------------------------

    /// WalIo whose appends fail while `fail_appends` is set; truncation
    /// heals it (mirroring a disk that recovered).
    struct FlakyIo {
        fail_appends: Arc<AtomicBool>,
        truncates: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl crate::user_history::wal::WalIo for FlakyIo {
        fn append(&mut self, _buf: &[u8]) -> std::io::Result<()> {
            if self.fail_appends.load(Ordering::SeqCst) {
                return Err(std::io::Error::other("injected append failure"));
            }
            Ok(())
        }
        fn sync_barrier(&mut self) -> std::io::Result<()> {
            Ok(())
        }
        fn sync_full(&mut self) -> std::io::Result<()> {
            Ok(())
        }
        fn truncate_to_header(&mut self) -> std::io::Result<()> {
            self.truncates
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
    }

    #[test]
    fn test_append_failure_keeps_memory_and_heals() {
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("history.lxud");
        let fail_appends = Arc::new(AtomicBool::new(true));
        let truncates = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let hist = hist_with_io(
            &cp,
            Box::new(FlakyIo {
                fail_appends: Arc::clone(&fail_appends),
                truncates: Arc::clone(&truncates),
            }),
        );

        hist.apply_records(&[committed("きょう", "今日")]);

        // §5.2: memory keeps the record (immediate quality), applied_seq
        // does not advance (the frame cannot replay), the WAL freezes, and
        // a heal is posted.
        assert_eq!(learned(&hist, "きょう"), vec!["今日".to_string()]);
        assert_eq!(read_recover(&hist.inner).applied_seq(), 0);
        assert!(lock_recover(&hist.wal).is_frozen());

        // The healing compaction checkpoints memory (covering the effect)
        // and re-truncates the WAL back to appendable form.
        fail_appends.store(false, Ordering::SeqCst);
        wait_until(
            || !lock_recover(&hist.wal).is_frozen(),
            "heal compaction to unfreeze the WAL",
        );
        assert!(cp.exists(), "healed checkpoint persists the effect");
        assert!(truncates.load(std::sync::atomic::Ordering::SeqCst) >= 1);

        // Nothing was lost: reopen from disk sees the entry (via checkpoint).
        drop(hist);
        let hist2 = open_hist(&cp);
        assert_eq!(learned(&hist2, "きょう"), vec!["今日".to_string()]);
    }

    /// WalIo whose sync_full fails once (tombstone durability failure).
    struct SyncFailOnceIo {
        failed: bool,
    }

    impl crate::user_history::wal::WalIo for SyncFailOnceIo {
        fn append(&mut self, _buf: &[u8]) -> std::io::Result<()> {
            Ok(())
        }
        fn sync_barrier(&mut self) -> std::io::Result<()> {
            Ok(())
        }
        fn sync_full(&mut self) -> std::io::Result<()> {
            if !self.failed {
                self.failed = true;
                return Err(std::io::Error::other("injected sync failure"));
            }
            Ok(())
        }
        fn truncate_to_header(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn test_tombstone_sync_failure_applies_with_real_seq() {
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("history.lxud");
        let hist = hist_with_io(&cp, Box::new(SyncFailOnceIo { failed: false }));

        hist.apply_records(&[committed("きょう", "今日")]);
        let commit_seq = lock_recover(&hist.wal).last_appended_seq();
        hist.apply_records(&[deletion("きょう", "今日")]);

        // The frame is on disk (mock append succeeded); the failed flush
        // must not leave it uncovered, or conditional truncation would
        // stall until an unrelated append.
        assert!(
            learned(&hist, "きょう").is_empty(),
            "memory delete still runs"
        );
        assert!(
            read_recover(&hist.inner).applied_seq() > commit_seq,
            "tombstone must be covered by its real seq despite the sync failure"
        );
        assert!(!lock_recover(&hist.wal).is_frozen());
        // §5.4: because the tombstone's WAL durability failed, the checkpoint
        // is written SYNCHRONOUSLY before apply_records returns (no wait for a
        // background thread) — the deletion cannot resurrect if a crash
        // preempts an async scrub.
        assert_eq!(
            lock_recover(&hist.wal).entry_count(),
            0,
            "durability-failed tombstone must checkpoint synchronously, not via async scrub"
        );
        drop(hist);
        let hist2 = open_hist(&cp);
        assert!(
            learned(&hist2, "きょう").is_empty(),
            "synchronously-checkpointed deletion survives reopen"
        );
    }

    #[test]
    fn test_tombstone_append_failure_checkpoints_synchronously() {
        // Io on a tombstone append: the frame never reaches disk, so memory
        // is the only copy of the deletion. It must be checkpointed
        // synchronously before returning — a crash before an async scrub
        // would otherwise resurrect the deleted entry from the old checkpoint.
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("history.lxud");
        let fail_appends = Arc::new(AtomicBool::new(false));
        let hist = hist_with_io(
            &cp,
            Box::new(FlakyIo {
                fail_appends: Arc::clone(&fail_appends),
                truncates: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            }),
        );

        hist.apply_records(&[committed("きょう", "今日")]);
        hist.run_compact(); // the entry now lives in the checkpoint
        assert!(cp.exists());

        // The tombstone's frame fails to append (frozen WAL, memory-only delete).
        fail_appends.store(true, Ordering::SeqCst);
        hist.apply_records(&[deletion("きょう", "今日")]);
        assert!(learned(&hist, "きょう").is_empty(), "memory delete runs");

        // Synchronous durability: reopening immediately (no wait for a
        // background scrub) must not resurrect the deletion.
        drop(hist);
        let hist2 = open_hist(&cp);
        assert!(
            learned(&hist2, "きょう").is_empty(),
            "durability-failed tombstone is checkpointed before return, not resurrected"
        );
    }

    #[test]
    fn test_durability_failure_with_failed_checkpoint_keeps_scrub_pending() {
        // R2: if the synchronous durability checkpoint ALSO fails (stuck
        // disk), the scrub must stay pending so a later compaction / startup
        // retries it — the deletion must not be silently dropped with the
        // pending flag consumed.
        let dir = tempfile::tempdir().unwrap();
        // Parent of the checkpoint is a *file*: save() can never create it.
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, b"x").unwrap();
        let cp = blocker.join("history.lxud");
        let hist = hist_with_io(&cp, Box::new(SyncFailOnceIo { failed: false }));

        // Learn, then delete: the tombstone's sync_full fails (SyncFailed)
        // AND the synchronous checkpoint save fails (blocked parent).
        hist.apply_records(&[committed("きょう", "今日")]);
        hist.apply_records(&[deletion("きょう", "今日")]);

        assert!(
            learned(&hist, "きょう").is_empty(),
            "memory delete still runs"
        );
        assert!(
            hist.scrub_pending.load(Ordering::SeqCst),
            "a failed durability checkpoint must keep the scrub pending for retry"
        );
    }

    #[test]
    fn test_checkpoint_failure_skips_truncation() {
        // T6 ordering: the WAL may only be truncated after a covering
        // checkpoint is durable. A failed checkpoint write must leave the
        // frames in place.
        let dir = tempfile::tempdir().unwrap();
        // Parent of the checkpoint path is a *file*: save() cannot create it.
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, b"x").unwrap();
        let cp = blocker.join("history.lxud");
        let truncates = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let hist = hist_with_io(
            &cp,
            Box::new(FlakyIo {
                fail_appends: Arc::new(AtomicBool::new(false)),
                truncates: Arc::clone(&truncates),
            }),
        );

        hist.apply_records(&[committed("きょう", "今日")]);
        assert_eq!(lock_recover(&hist.wal).entry_count(), 1);
        hist.run_compact();
        assert_eq!(
            truncates.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "no truncation without a durable checkpoint"
        );
        assert_eq!(lock_recover(&hist.wal).entry_count(), 1);
    }

    // -----------------------------------------------------------------------
    // §14-2: manual latency profile of the coupled critical section.
    // Run with:
    //   cargo test --release -p lex_engine profile_apply_records \
    //     -- --ignored --nocapture
    // -----------------------------------------------------------------------

    #[test]
    #[ignore = "manual profiling (§14-2): run with --release --ignored --nocapture"]
    fn profile_apply_records_lock_hold() {
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("history.lxud");
        let hist = open_hist(&cp);

        // Realistic capacity: ~20k unigram entries.
        {
            let mut h = write_recover(&hist.inner);
            for i in 0..20_000 {
                h.record_at(&[(format!("よみ{i}"), format!("面{i}"))], 1000 + i as u64);
            }
        }

        // Contending conversion reads (the §14-2 concern: inner.read vs the
        // wal->inner write section).
        let stop = Arc::new(AtomicBool::new(false));
        let mut readers = Vec::new();
        for _ in 0..2 {
            let hist = Arc::clone(&hist);
            let stop = Arc::clone(&stop);
            readers.push(std::thread::spawn(move || {
                let mut n = 0u64;
                while !stop.load(Ordering::Relaxed) {
                    let h = read_recover(&hist.inner);
                    n += h.learned_surfaces("よみ100", 2000).len() as u64;
                    drop(h);
                }
                n
            }));
        }

        let percentile = |sorted: &[std::time::Duration], p: f64| {
            sorted[((sorted.len() as f64 - 1.0) * p) as usize]
        };

        // Committed path (includes the every-50 F_BARRIERFSYNC).
        let mut durs = Vec::with_capacity(1000);
        for i in 0..1000 {
            let rec = committed(&format!("けいそく{i}"), &format!("計測{i}"));
            let t = std::time::Instant::now();
            hist.apply_records(&[rec]);
            durs.push(t.elapsed());
        }
        durs.sort();
        println!(
            "apply_records Committed (20k entries, 2 readers): p50={:?} p95={:?} p99={:?} max={:?}",
            percentile(&durs, 0.50),
            percentile(&durs, 0.95),
            percentile(&durs, 0.99),
            durs.last().unwrap(),
        );

        // Tombstone path (per-gesture F_FULLFSYNC).
        let mut tdurs = Vec::with_capacity(20);
        for i in 0..20 {
            let rec = deletion(&format!("けいそく{i}"), &format!("計測{i}"));
            let t = std::time::Instant::now();
            hist.apply_records(&[rec]);
            tdurs.push(t.elapsed());
        }
        tdurs.sort();
        println!(
            "apply_records Tombstone (F_FULLFSYNC): p50={:?} max={:?}",
            percentile(&tdurs, 0.50),
            tdurs.last().unwrap(),
        );

        stop.store(true, Ordering::Relaxed);
        for r in readers {
            r.join().unwrap();
        }
    }

    // -----------------------------------------------------------------------
    // T8: concurrency smoke — records × compactions × tombstones, then
    // (checkpoint + WAL replay) == memory. Non-deterministic by nature; the
    // deterministic guarantees live in the T2/T6/T7 tests.
    // -----------------------------------------------------------------------

    #[test]
    fn test_concurrent_smoke_disk_matches_memory() {
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("history.lxud");
        let hist = open_hist(&cp);

        const WRITERS: usize = 4;
        const BATCHES: usize = 100;
        let mut handles = Vec::new();
        for w in 0..WRITERS {
            let hist = Arc::clone(&hist);
            handles.push(std::thread::spawn(move || {
                for i in 0..BATCHES {
                    let reading = format!("よみ{w}-{i}");
                    let surface = format!("面{w}-{i}");
                    hist.apply_records(&[committed(&reading, &surface)]);
                    if i % 10 == 3 {
                        // Delete an entry another iteration just learned
                        // (sometimes a no-op — both paths exercised).
                        let target = format!("よみ{w}-{}", i - 1);
                        let target_surface = format!("面{w}-{}", i - 1);
                        hist.apply_records(&[deletion(&target, &target_surface)]);
                    }
                    if i % 25 == 7 {
                        hist.spawn_compact();
                    }
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        // Quiesce: the gate is free only when no compaction is running;
        // parked ones finish before our acquisition returns.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            assert!(
                std::time::Instant::now() < deadline,
                "timeout waiting for compactions to quiesce"
            );
            let gate = hist.lock_gate();
            if !hist.scrub_pending.load(Ordering::SeqCst) {
                // Holding the gate: no compaction can start; writers are
                // done, so the disk state is stable. Reconstruct and diff.
                let (disk, _wal) =
                    crate::user_history::wal::open_with_wal(&cp).expect("strict reopen");
                let mem = read_recover(&hist.inner);
                let collect = |h: &UserHistory| {
                    let mut v: Vec<(String, String, u32)> = h
                        .unigrams()
                        .map(|(r, s, e)| (r.to_string(), s.to_string(), e.frequency))
                        .collect();
                    v.sort();
                    v
                };
                assert_eq!(
                    collect(&disk),
                    collect(&mem),
                    "checkpoint + WAL replay must equal the in-memory state"
                );
                drop(gate);
                break;
            }
            drop(gate);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}
