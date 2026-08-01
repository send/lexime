use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

use tracing::warn;

use crate::dict::connection::ConnectionMatrix;
use crate::dict::{CompositeDictionary, Dictionary, TrieDictionary};
use crate::session::LearningRecord;
use crate::user_history::recovery::{CheckpointState, OpenReport, WalState};
use crate::user_history::wal::{AppendError, FreezeFlag, HistoryWal, WalRecord};
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
    /// Runtime durability ledger for #295: raised whenever a deletion's WAL
    /// durability fails, covered when a checkpoint that excludes it becomes
    /// durable. Outstanding iff `gen > covered`.
    ///
    /// A generation pair rather than a flag because raise and cover race:
    /// a compactor reads the generation *before* snapshotting, so a deletion
    /// raised after that snapshot cannot be cleared by it. A flag would be
    /// cleared by a checkpoint that never contained the deletion.
    ///
    /// There is deliberately no matching ledger for commits. `append_record`
    /// only returns `Io` from its frozen guard or from a failing `append`
    /// that freezes (wal.rs), so "a commit is memory-only" is exactly "the
    /// WAL is frozen" — `appends_frozen` below already reports it, and a
    /// second ledger would be a duplicate book, free to disagree.
    unpersisted_deletion_gen: AtomicU64,
    unpersisted_deletion_covered: AtomicU64,
    /// Read-only view of the WAL's own freeze flag, observable without the
    /// wal mutex (#288). The key-processing thread holds that mutex for every
    /// append; a UI poll for the menu must not queue behind one.
    appends_frozen: FreezeFlag,
}

/// A durability problem that is true *right now*, as opposed to
/// [`LexHistoryOpenReport`]'s record of what startup recovery found.
///
/// Kept as a list rather than collapsed into one enum or a bool: on a
/// failing volume both conditions hold at once — that is the steady state,
/// not a corner — and collapsing would hide the memory-only learning (#288)
/// behind the unpersisted deletion (#295), or the reverse.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum LexHistoryDurabilityIssue {
    /// A deletion the user asked for could not be made durable: its WAL
    /// append or flush failed *and* the synchronous checkpoint fallback
    /// (§5.4) failed too.
    ///
    /// Two halves with different remedies, which is why the user-facing
    /// wording names neither. When the frame never reached the WAL the
    /// deletion is memory-only and has no startup heal — the old checkpoint
    /// still holds the entry and wins on the next start. When the frame is on
    /// disk but its flush failed, replay re-applies the deletion, so only
    /// power loss (not a restart) undoes it.
    DeletionNotPersisted,
    /// Appends are frozen, so this session's learning lives only in memory
    /// until a compaction restores appendable form. A restart recovers what
    /// the last checkpoint holds; anything since is lost.
    LearningMemoryOnly,
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
    /// A v1->v2 migration was needed and its commit failed; the v1 files
    /// are kept, and a startup compaction retries the write in-session.
    /// Surfaced because `clean` alone only says "something happened" —
    /// without it the log line reads `checkpoint: Loaded, wal: Clean`, i.e.
    /// exactly like a healthy start.
    pub migration_failed: bool,
    /// Appends were frozen at open: this session's learning stays in memory
    /// until a compaction restores appendable form.
    pub appends_frozen: bool,
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
            migration_failed: r.migration_failed,
            appends_frozen: r.appends_frozen,
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
        // Take a handle to the WAL's freeze state (#288) before it goes
        // behind the mutex. A freeze recovery already applied — a failed tail
        // repair or migration commit — is in the flag from the start, so it
        // reports from the first menu open rather than from the first append.
        let appends_frozen = wal.frozen_flag();
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
            unpersisted_deletion_gen: AtomicU64::new(0),
            unpersisted_deletion_covered: AtomicU64::new(0),
            appends_frozen,
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

    /// Durability problems that hold right now, most severe first.
    ///
    /// Polled by the UI (the status menu re-reads on every open), so it must
    /// not block: both answers come from atomics, never from the wal mutex
    /// the key-processing thread holds across appends.
    fn durability_issues(&self) -> Vec<LexHistoryDurabilityIssue> {
        let mut issues = Vec::new();
        // First: a deletion that did not persist has no startup heal, and it
        // is the one the user explicitly asked for.
        if self.has_unpersisted_deletion() {
            issues.push(LexHistoryDurabilityIssue::DeletionNotPersisted);
        }
        if self.appends_frozen.is_frozen() {
            issues.push(LexHistoryDurabilityIssue::LearningMemoryOnly);
        }
        issues
    }
}

impl LexUserHistory {
    /// Record that a deletion is not on disk anywhere (#295).
    ///
    /// Called under the wal mutex, after the in-memory apply. Both placements
    /// are load-bearing:
    /// - after the apply, so a compactor that observes this raise is
    ///   guaranteed to snapshot a history that already excludes the entry
    ///   (the apply released `inner` before the store, and the compactor
    ///   acquires it after the load);
    /// - under the wal mutex, so it cannot land inside `clear_impl`'s
    ///   read-then-cover window. A raise slipping in there would outlive a
    ///   wipe that made it vacuously true, leaving a privacy warning on a
    ///   history that is provably empty.
    fn raise_unpersisted_deletion(&self) {
        self.unpersisted_deletion_gen.fetch_add(1, Ordering::SeqCst);
    }

    /// Take the generation to cover *together with* the snapshot that will
    /// cover it, under one `inner` read guard.
    ///
    /// The guard is what makes the pairing sound — not the two statements
    /// happening to sit in the right order. Every raise is preceded by
    /// `apply_batch`, which needs `inner` for writing, so while this guard is
    /// held no raise can slip its effect past the clone: a concurrent raise
    /// either finished before the guard (its deletion is in the snapshot) or
    /// is still blocked on the write lock (its generation lands after this
    /// load, so it stays outstanding, which is the safe direction).
    ///
    /// Reading the generation after the clone would clear deletions the
    /// snapshot may not contain — a silent false negative, the one direction
    /// the design forbids. That used to rest on statement order inside
    /// `run_compact_impl` with only a comment to hold it.
    fn snapshot_to_cover(&self) -> (u64, UserHistory) {
        let history = read_recover(&self.inner);
        let generation = self.unpersisted_deletion_gen.load(Ordering::SeqCst);
        (generation, history.clone())
    }

    /// The generation to cover for a wipe. A bare load is sound here because
    /// `clear_impl` holds the wal mutex across both this read and the cover,
    /// and every raise happens under that mutex — so unlike the compaction
    /// path there is no window to close.
    fn deletion_gen_under_wal_lock(&self) -> u64 {
        self.unpersisted_deletion_gen.load(Ordering::SeqCst)
    }

    /// Mark every deletion raised up to `generation` as persisted. Called
    /// only once a checkpoint that excludes them is durable on disk.
    ///
    /// `fetch_max` rather than a store: covers cannot currently race (every
    /// `run_compact_impl` caller holds `compact_gate`, and `clear_impl` takes
    /// it too), so a store would also be correct today. Keeping the counter
    /// monotone means the ledger cannot walk backwards — and report a
    /// deletion as unpersisted after it was covered — if that exclusivity is
    /// ever relaxed.
    fn cover_unpersisted_deletions(&self, generation: u64) {
        self.unpersisted_deletion_covered
            .fetch_max(generation, Ordering::SeqCst);
    }

    fn has_unpersisted_deletion(&self) -> bool {
        // `covered` first, and the order is load-bearing. Both counters only
        // increase and `covered` never exceeds `gen`, so reading C then G
        // gives G >= C: a raise outstanding when C was read forces G > C, and
        // the worst skew is reporting an issue that a cover has just settled,
        // which the next poll retracts.
        //
        // The other order can report *clean* while a deletion is unpersisted:
        // load gen=5, then a compaction covers 5 and a new failed deletion
        // raises 6, then load covered=5 — and 5 > 5 is false. The sink is a
        // menu the user opens once, so that single miss is the whole
        // notification.
        let covered = self.unpersisted_deletion_covered.load(Ordering::SeqCst);
        self.unpersisted_deletion_gen.load(Ordering::SeqCst) > covered
    }

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
                    //
                    // Memory alone cannot answer this: an entry evicted for
                    // capacity is gone from the maps while the checkpoint
                    // still holds it, and skipping there makes the deletion
                    // a silent no-op that a restart undoes. Scoped so the
                    // read guard is released before the append below, which
                    // for a Tombstone includes a full flush.
                    // The memory probe sees state as of *before* this
                    // batch — `apply_batch` runs once, after the loop — so an
                    // earlier record in the same batch has to be consulted
                    // separately. Without that, a batch of
                    // [Committed(X), Deletion(X)] finds X nowhere, skips the
                    // tombstone, and then learns X: the deletion loses to a
                    // commit it was supposed to undo.
                    let has_target = read_recover(&self.inner)
                        .deletion_has_durable_target(segments)
                        || sequenced.iter().any(|(earlier, _)| match earlier {
                            WalRecord::Committed { segments: c, .. } => {
                                c.iter().any(|pair| segments.contains(pair))
                            }
                            WalRecord::Tombstone { .. } => false,
                        });
                    if !has_target {
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
                        // The frame is on disk (process-crash safe via replay)
                        // but its F_FULLFSYNC failed, reopening the power-loss
                        // window the Tombstone contract closes. Persist the
                        // deletion synchronously below rather than resurrect it
                        // if the async scrub is preempted.
                        //
                        // Gated on the record kind for the same reason the Io
                        // arm is, not left to the cross-crate invariant that
                        // only Tombstones can produce SyncFailed: that lives in
                        // another file, and a debug_assert vouching for it is
                        // gone in the shipped build. Were a Committed barrier
                        // failure ever routed here, an ungated raise would tell
                        // the user a *deletion* did not persist — a privacy
                        // claim about an operation they never requested.
                        if matches!(record, WalRecord::Tombstone { .. }) {
                            durability_failed = true;
                        }
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
                // apply_batch records the residue for any tombstone whose
                // frame never landed: the entry leaves memory but stays in
                // the checkpoint with nothing to neutralise it.
                write_recover(&self.inner).apply_batch(&sequenced);
            }
            if durability_failed {
                // Still holding the wal mutex, and after the apply above —
                // see `raise_unpersisted_deletion` for why both matter. The
                // gated compaction that tries to cover this runs below, once
                // the mutex is released (it needs it itself).
                self.raise_unpersisted_deletion();
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

        // The wal mutex (held since above) is what closes this window: every
        // raise happens under it, so none can land between this read and the
        // cover below.
        let covered_gen = self.deletion_gen_under_wal_lock();

        // Commit point. On Err nothing has changed — including
        // scrub_pending, so a pre-clear Tombstone's scrub request stays
        // posted for the next compaction.
        let mut empty = UserHistory::new();
        empty.advance_applied_seq(wal.last_appended_seq());
        empty.save(wal.checkpoint_path())?;

        // Consumed only after the commit point: the wipe supersedes every
        // scrub request posted so far.
        self.scrub_pending.store(false, Ordering::SeqCst);
        // Likewise for the durability ledger (#295). An empty durable set
        // contains no un-deleted entry, so every raised deletion is now
        // vacuously persisted. Without this second cover point, wiping
        // everything would leave a standing "a deletion did not persist"
        // warning on a history that provably holds nothing.
        self.cover_unpersisted_deletions(covered_gen);

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
            // Assigning the whole state also drops the durable residue,
            // which is correct: the checkpoint written above emptied the
            // durable set, so nothing can be left on disk that memory
            // lacks. It is safe against a concurrent raise not because
            // memory is empty at this instant (it is only becoming so
            // here), but because the wal mutex has been held since before
            // the commit point, and every raise happens under it.
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
        // 1. Clone history under read lock (brief), taking the generation
        // this checkpoint can vouch for under the same guard (#295).
        let (covered_gen, snapshot) = self.snapshot_to_cover();
        let cp_path = lock_recover(&self.wal).checkpoint_path().to_path_buf();

        // 2. Write checkpoint (no locks held, slow I/O)
        if let Err(e) = snapshot.save(&cp_path) {
            warn!("checkpoint write failed: {e}");
            return CompactOutcome::Failed;
        }

        // The deletion is persisted the moment this full-snapshot checkpoint
        // is durable — before the truncation below, and regardless of whether
        // it runs. Truncation is the physical scrub of superseded frames, not
        // what makes the deletion survive a restart, so tying the cover to it
        // would leave a permanent warning whenever frames land mid-run
        // (FollowUp) or the truncate fails on an otherwise durable write.
        self.cover_unpersisted_deletions(covered_gen);

        // 3. Truncate WAL (brief lock) — conditionally (§5.3): only frames
        // provably covered by the durable checkpoint (seq <= applied_seq at
        // snapshot time) may be destroyed.
        let mut wal = lock_recover(&self.wal);

        // The checkpoint is a full snapshot, so everything it contains is
        // now both on disk and in the state it was cloned from: the residue
        // keys that snapshot carried are settled. Keys raised *since* the
        // clone carry a newer epoch and survive — the file does hold those.
        //
        // Under the wal mutex, not beside it: "every history mutation runs
        // under the wal mutex" is the invariant the deletion-skip check
        // cites for being race-free, and it is also what lets §4 promise
        // that swapping the RwLock for an ArcSwap stays a local change.
        //
        // Cost: one pass over the residue, holding the wal mutex and the
        // `inner` write lock — so both the key thread and conversion reads
        // wait on it. ~550 µs at 10k tracked keys and ~3 ms at the 50k cap
        // (M-series, release), against ~0 in the steady state where the
        // residue is empty. The cap is only approached when checkpoints have
        // been failing for many intervals, which is also when this run is
        // the thing about to fix that.
        //
        // Comparing stamps across this call is only meaningful because
        // `compact_gate` keeps a `clear` out of the window above — the one
        // stretch where no lock is held — and a clear resets the residue
        // epoch to 0. See `DurableResidue::epoch`.
        write_recover(&self.inner).cover_durable_residue(&snapshot);
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
        let wal = HistoryWal::with_io(cp, io);
        let appends_frozen = wal.frozen_flag();
        Arc::new(LexUserHistory {
            inner: Arc::new(RwLock::new(UserHistory::new())),
            wal: Mutex::new(wal),
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
                migration_failed: false,
                appends_frozen: false,
                frames_replayed: 0,
                frames_skipped: 0,
                quarantined_paths: Vec::new(),
                replayed_deletion: false,
                compaction_recommended: false,
            },
            unpersisted_deletion_gen: AtomicU64::new(0),
            unpersisted_deletion_covered: AtomicU64::new(0),
            appends_frozen,
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

    /// A history holding "きょう→今日" in a durable checkpoint, with the
    /// entry then dropped from memory the way capacity eviction drops it —
    /// present on disk, absent from the maps.
    ///
    /// The drop goes through a tombstone whose frame never reached the WAL,
    /// which leaves the history in exactly the state `evict()` produces
    /// (pinned in lex-core by
    /// `test_evict_selection_is_unchanged_and_reports_residue`) without
    /// seeding 10,000 entries to cross `max_unigrams`.
    fn hist_with_evicted_entry(cp: &Path) -> Arc<LexUserHistory> {
        let hist = open_hist(cp);
        hist.apply_records(&[committed("きょう", "今日")]);
        assert!(
            matches!(hist.run_compact(), CompactOutcome::Done),
            "the entry must reach a durable checkpoint"
        );
        write_recover(&hist.inner).apply_batch(&[(
            WalRecord::Tombstone {
                segments: vec![("きょう".to_string(), "今日".to_string())],
                timestamp: 0,
            },
            None,
        )]);
        assert!(
            learned(&hist, "きょう").is_empty(),
            "evicted: absent from memory"
        );
        hist
    }

    #[test]
    fn test_deleting_an_evicted_entry_still_persists() {
        // #286: an entry evicted for capacity is absent from memory but
        // still in the checkpoint until the next compaction re-snapshots.
        // Gating the tombstone on memory alone made the deletion a silent
        // no-op that the next startup undid.
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("history.lxud");
        let hist = hist_with_evicted_entry(&cp);

        let before = read_recover(&hist.inner).applied_seq();
        hist.apply_records(&[deletion("きょう", "今日")]);
        // Not `entry_count() > 0`: this call also spawns the scrub
        // compaction, which may truncate the WAL before the assertion runs.
        // `applied_seq` records the same fact (a frame was appended and
        // applied) and truncation does not roll it back.
        assert!(
            read_recover(&hist.inner).applied_seq() > before,
            "the deletion must reach the WAL: memory-absent is not disk-absent"
        );

        wait_until(
            || lock_recover(&hist.wal).entry_count() == 0,
            "scrub compaction to truncate the WAL",
        );
        drop(hist);
        let hist2 = open_hist(&cp);
        assert!(
            learned(&hist2, "きょう").is_empty(),
            "the deleted conversion must not come back"
        );
    }

    #[test]
    fn test_deletion_beats_a_commit_earlier_in_the_same_batch() {
        // The memory probe sees pre-batch state, so a Deletion that follows a
        // Committed for the same pair in one batch would find the pair
        // nowhere, skip the tombstone, and then learn it — the ForwardDelete
        // silently losing to the commit it was meant to undo.
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("history.lxud");
        let hist = open_hist(&cp);

        hist.apply_records(&[committed("きょう", "今日"), deletion("きょう", "今日")]);

        assert!(
            learned(&hist, "きょう").is_empty(),
            "the deletion must win over a commit earlier in the same batch"
        );
        assert!(
            hist.scrub_pending.load(Ordering::SeqCst) || lock_recover(&hist.wal).entry_count() >= 2,
            "a tombstone must have been written, not skipped as a no-op"
        );

        drop(hist);
        let hist2 = open_hist(&cp);
        assert!(
            learned(&hist2, "きょう").is_empty(),
            "and it must not come back on restart"
        );
    }

    #[test]
    fn test_unrelated_residue_does_not_cost_a_flush() {
        // The residue is per-key, not a global "something was evicted" bit:
        // being at capacity must not make every ForwardDelete of a
        // never-learned candidate pay a key-thread F_FULLFSYNC plus a full
        // checkpoint.
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("history.lxud");
        let hist = hist_with_evicted_entry(&cp);

        hist.apply_records(&[deletion("あした", "明日")]);
        assert_eq!(
            lock_recover(&hist.wal).entry_count(),
            0,
            "a pair in neither memory nor the residue is still a no-op"
        );
        assert!(!hist.scrub_pending.load(Ordering::SeqCst));
    }

    #[test]
    fn test_compaction_restores_the_no_op_fast_path() {
        // Once a checkpoint has been written from the post-eviction state,
        // the key really is absent from disk and the skip is safe again.
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("history.lxud");
        let hist = hist_with_evicted_entry(&cp);
        assert!(matches!(hist.run_compact(), CompactOutcome::Done));

        hist.apply_records(&[deletion("きょう", "今日")]);
        assert_eq!(
            lock_recover(&hist.wal).entry_count(),
            0,
            "covered by a checkpoint that no longer contains the entry"
        );
    }

    #[test]
    fn test_clear_drops_the_residue() {
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("history.lxud");
        let hist = hist_with_evicted_entry(&cp);

        hist.clear_impl().unwrap();
        hist.apply_records(&[deletion("きょう", "今日")]);
        assert_eq!(
            lock_recover(&hist.wal).entry_count(),
            0,
            "the wipe emptied the durable set; nothing can be left on disk"
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

    /// WalIo with independently switchable failures. One mock rather than a
    /// family of near-identical ones: the durability matrix needs appends,
    /// full syncs and truncations to fail in several combinations, and on a
    /// real failing volume they fail *together* — the correlated case the
    /// issue list exists for.
    ///
    /// Every switch is live, so a test can also heal the disk mid-run.
    #[derive(Default, Clone)]
    struct FaultyIo {
        fail_appends: Arc<AtomicBool>,
        fail_full_sync: Arc<AtomicBool>,
        fail_truncates: Arc<AtomicBool>,
        truncates: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl FaultyIo {
        fn boxed(&self) -> Box<dyn crate::user_history::wal::WalIo> {
            Box::new(self.clone())
        }
    }

    impl crate::user_history::wal::WalIo for FaultyIo {
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
            if self.fail_full_sync.load(Ordering::SeqCst) {
                return Err(std::io::Error::other("injected sync failure"));
            }
            Ok(())
        }
        fn truncate_to_header(&mut self) -> std::io::Result<()> {
            self.truncates
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self.fail_truncates.load(Ordering::SeqCst) {
                return Err(std::io::Error::other("injected truncate failure"));
            }
            Ok(())
        }
    }

    /// A checkpoint path whose parent is a *file*, so `save()` can never
    /// succeed. `unblock_checkpoint` turns it into a real directory, healing
    /// the disk.
    fn blocked_checkpoint(dir: &Path) -> PathBuf {
        let blocker = dir.join("blocker");
        std::fs::write(&blocker, b"x").unwrap();
        blocker.join("history.lxud")
    }

    fn unblock_checkpoint(cp: &Path) {
        let blocker = cp.parent().unwrap();
        std::fs::remove_file(blocker).unwrap();
        std::fs::create_dir(blocker).unwrap();
    }

    #[test]
    fn test_append_failure_keeps_memory_and_heals() {
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("history.lxud");
        let io = FaultyIo::default();
        io.fail_appends.store(true, Ordering::SeqCst);
        let hist = hist_with_io(&cp, io.boxed());

        hist.apply_records(&[committed("きょう", "今日")]);

        // §5.2: memory keeps the record (immediate quality), applied_seq
        // does not advance (the frame cannot replay), the WAL freezes, and
        // a heal is posted.
        assert_eq!(learned(&hist, "きょう"), vec!["今日".to_string()]);
        assert_eq!(read_recover(&hist.inner).applied_seq(), 0);
        assert!(lock_recover(&hist.wal).is_frozen());
        // F10 (#288): the frozen window is what makes this session's learning
        // memory-only, and it is reportable while it lasts — not only after
        // the fact via the next startup's OpenReport.
        assert_eq!(
            hist.durability_issues(),
            vec![LexHistoryDurabilityIssue::LearningMemoryOnly]
        );

        // The healing compaction checkpoints memory (covering the effect)
        // and re-truncates the WAL back to appendable form.
        io.fail_appends.store(false, Ordering::SeqCst);
        wait_until(
            || !lock_recover(&hist.wal).is_frozen(),
            "heal compaction to unfreeze the WAL",
        );
        assert!(cp.exists(), "healed checkpoint persists the effect");
        assert!(io.truncates.load(std::sync::atomic::Ordering::SeqCst) >= 1);
        // F11: the issue is clearable, not a latch — which is exactly why it
        // is polled rather than folded into Swift's `initFailures`.
        assert!(
            hist.durability_issues().is_empty(),
            "the heal must clear the report"
        );

        // Nothing was lost: reopen from disk sees the entry (via checkpoint).
        drop(hist);
        let hist2 = open_hist(&cp);
        assert_eq!(learned(&hist2, "きょう"), vec!["今日".to_string()]);
    }

    #[test]
    fn test_tombstone_sync_failure_applies_with_real_seq() {
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("history.lxud");
        let io = FaultyIo::default();
        io.fail_full_sync.store(true, Ordering::SeqCst);
        let hist = hist_with_io(&cp, io.boxed());

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
        let io = FaultyIo::default();
        let hist = hist_with_io(&cp, io.boxed());

        hist.apply_records(&[committed("きょう", "今日")]);
        hist.run_compact(); // the entry now lives in the checkpoint
        assert!(cp.exists());

        // The tombstone's frame fails to append (frozen WAL, memory-only delete).
        io.fail_appends.store(true, Ordering::SeqCst);
        hist.apply_records(&[deletion("きょう", "今日")]);
        assert!(learned(&hist, "きょう").is_empty(), "memory delete runs");
        // F1: the deletion *was* raised, and the synchronous fallback
        // checkpoint covered it before returning — so nothing is reported.
        // The WAL is still frozen, which is its own (accurate) issue.
        assert!(
            !hist
                .durability_issues()
                .contains(&LexHistoryDurabilityIssue::DeletionNotPersisted),
            "a deletion the fallback persisted must not be reported as lost"
        );

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
        let cp = blocked_checkpoint(dir.path());
        let io = FaultyIo::default();
        io.fail_full_sync.store(true, Ordering::SeqCst);
        let hist = hist_with_io(&cp, io.boxed());

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
        let cp = blocked_checkpoint(dir.path());
        let io = FaultyIo::default();
        let hist = hist_with_io(&cp, io.boxed());

        hist.apply_records(&[committed("きょう", "今日")]);
        assert_eq!(lock_recover(&hist.wal).entry_count(), 1);
        hist.run_compact();
        assert_eq!(
            io.truncates.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "no truncation without a durable checkpoint"
        );
        assert_eq!(lock_recover(&hist.wal).entry_count(), 1);
    }

    // -----------------------------------------------------------------------
    // Runtime durability channel (#295 / #288). A deletion that reaches
    // neither the WAL nor a checkpoint has no startup heal — the old
    // checkpoint simply wins — so the only thing that can save it is telling
    // the user. These pin the raise/cover ledger and the frozen mirror.
    // -----------------------------------------------------------------------

    /// A history whose tombstone durability has failed with no checkpoint to
    /// fall back on: the #295 double failure, minus the WAL freeze so the
    /// deletion issue can be observed on its own.
    fn hist_with_unpersisted_deletion(dir: &Path) -> (Arc<LexUserHistory>, PathBuf) {
        let cp = blocked_checkpoint(dir);
        let io = FaultyIo::default();
        io.fail_full_sync.store(true, Ordering::SeqCst);
        let hist = hist_with_io(&cp, io.boxed());
        hist.apply_records(&[committed("きょう", "今日")]);
        hist.apply_records(&[deletion("きょう", "今日")]);
        (hist, cp)
    }

    #[test]
    fn test_sync_failed_deletion_with_failed_checkpoint_is_reported() {
        // F3. SyncFailed leaves the frame on disk, so a *process* crash still
        // replays the deletion — but the flush the Tombstone contract (§6)
        // promises did not happen, and the fallback checkpoint failed too.
        // The window the design puts at zero is open, so it is reported: the
        // raise deliberately does not branch on the error variant.
        let dir = tempfile::tempdir().unwrap();
        let (hist, _cp) = hist_with_unpersisted_deletion(dir.path());

        assert!(
            learned(&hist, "きょう").is_empty(),
            "memory delete still runs"
        );
        assert!(
            !lock_recover(&hist.wal).is_frozen(),
            "SyncFailed does not freeze: the frame itself is valid"
        );
        assert_eq!(
            hist.durability_issues(),
            vec![LexHistoryDurabilityIssue::DeletionNotPersisted],
            "the deletion is unpersisted, and only that"
        );
    }

    #[test]
    fn test_deletion_double_failure_reports_both_issues_in_severity_order() {
        // F2 + F14. One failing volume breaks appends and checkpoint writes
        // together, so both conditions hold — the steady state, not a corner.
        // Collapsing the channel into a single enum would hide the
        // memory-only learning (#288) behind the lost deletion (#295).
        let dir = tempfile::tempdir().unwrap();
        let cp = blocked_checkpoint(dir.path());
        let io = FaultyIo::default();
        let hist = hist_with_io(&cp, io.boxed());

        hist.apply_records(&[committed("きょう", "今日")]);
        io.fail_appends.store(true, Ordering::SeqCst);
        hist.apply_records(&[deletion("きょう", "今日")]);

        assert_eq!(
            hist.durability_issues(),
            vec![
                // The deletion first: it is what the user explicitly asked
                // for, and unlike a frozen WAL nothing heals it on restart.
                LexHistoryDurabilityIssue::DeletionNotPersisted,
                LexHistoryDurabilityIssue::LearningMemoryOnly,
            ]
        );
    }

    #[test]
    fn test_later_successful_compaction_clears_the_deletion_issue() {
        // F4. The report tracks the current state, so a disk that recovers
        // must retract it — otherwise the warning is permanent and the user
        // learns to ignore it.
        let dir = tempfile::tempdir().unwrap();
        let (hist, cp) = hist_with_unpersisted_deletion(dir.path());
        assert!(!hist.durability_issues().is_empty());

        unblock_checkpoint(&cp);
        assert!(matches!(hist.run_compact(), CompactOutcome::Done));
        assert!(hist.durability_issues().is_empty());

        // And the deletion really is on disk now, not merely unreported.
        drop(hist);
        assert!(learned(&open_hist(&cp), "きょう").is_empty());
    }

    #[test]
    fn test_clear_covers_the_unpersisted_deletion() {
        // F5. clear writes an empty checkpoint, so every raised deletion
        // becomes vacuously persisted. Without this second cover point,
        // wiping *everything* would leave a standing "a deletion did not
        // persist" warning on a history that provably holds nothing.
        let dir = tempfile::tempdir().unwrap();
        let (hist, cp) = hist_with_unpersisted_deletion(dir.path());
        assert!(!hist.durability_issues().is_empty());

        unblock_checkpoint(&cp);
        hist.clear_impl().unwrap();
        assert!(
            hist.durability_issues().is_empty(),
            "a full wipe persists every deletion"
        );
    }

    #[test]
    fn test_failed_clear_does_not_cover_the_deletion() {
        // F9. clear's empty checkpoint is its commit point; if it never
        // lands, the durable set is unchanged and the deletion is still only
        // in memory. The cover has to be tied to the write, not the attempt.
        let dir = tempfile::tempdir().unwrap();
        let (hist, _cp) = hist_with_unpersisted_deletion(dir.path());

        assert!(
            hist.clear_impl().is_err(),
            "blocked checkpoint fails the clear"
        );
        assert!(
            hist.durability_issues()
                .contains(&LexHistoryDurabilityIssue::DeletionNotPersisted),
            "a clear that did not commit persists nothing"
        );
    }

    #[test]
    fn test_a_deletion_raised_after_the_snapshot_stays_reported() {
        // F6. A deletion raised after a compaction took its snapshot must not
        // be cleared by that compaction's checkpoint.
        //
        // What makes this hold is structural rather than tested:
        // `snapshot_to_cover` reads the generation and clones under one
        // `inner` read guard, and every raise is preceded by an `apply_batch`
        // that needs `inner` for writing — so the pair is atomic with respect
        // to the apply. An earlier revision put the read and the clone in
        // separate statements inside `run_compact_impl`, where only a comment
        // kept them in order; this test could not have caught that, which is
        // why the ordering was moved into the guard instead.
        //
        // Exercised through the real pairing function, so at least the value
        // being covered is the one a compaction would use.
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("history.lxud");
        let hist = open_hist(&cp);

        let (generation, _snapshot) = hist.snapshot_to_cover();
        hist.raise_unpersisted_deletion(); // lands after the pair was taken
        hist.cover_unpersisted_deletions(generation);

        assert!(
            hist.has_unpersisted_deletion(),
            "a checkpoint cannot vouch for a deletion taken after its snapshot"
        );
        // The next compaction, pairing a fresh generation, does clear it.
        let (generation, _snapshot) = hist.snapshot_to_cover();
        hist.cover_unpersisted_deletions(generation);
        assert!(!hist.has_unpersisted_deletion());
    }

    #[test]
    fn test_ledger_read_order_cannot_report_clean_while_outstanding() {
        // The pair of loads in `has_unpersisted_deletion` must read `covered`
        // first. Reproduces the interleaving directly, because the window is
        // two adjacent loads with nothing to interpose on: read gen, let a
        // cover and a fresh raise both land, then read covered. With the
        // wrong order that yields 5 > 5 = false — a report of "no issues"
        // while a deletion is only in memory, which for a once-opened menu is
        // the entire notification.
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("history.lxud");
        let hist = open_hist(&cp);

        // gen=5, covered=4: one deletion outstanding.
        for _ in 0..5 {
            hist.raise_unpersisted_deletion();
        }
        hist.cover_unpersisted_deletions(4);

        // What a poll interleaved with (cover 5, raise 6) would observe.
        let covered_first = hist.unpersisted_deletion_covered.load(Ordering::SeqCst);
        hist.cover_unpersisted_deletions(5);
        hist.raise_unpersisted_deletion();
        let gen_after = hist.unpersisted_deletion_gen.load(Ordering::SeqCst);

        assert_eq!((covered_first, gen_after), (4, 6));
        assert!(
            gen_after > covered_first,
            "reading covered first keeps the skew on the over-reporting side"
        );
        assert!(hist.has_unpersisted_deletion());
    }

    #[test]
    fn test_a_durable_checkpoint_covers_even_without_truncation() {
        // F7 + F8. The deletion is persisted the moment the full-snapshot
        // checkpoint is durable. Truncation is the physical scrub of
        // superseded frames — tying the cover to it would leave a permanent
        // warning whenever frames land mid-run (FollowUp) or the truncate
        // fails on an otherwise durable write.
        let dir = tempfile::tempdir().unwrap();

        // FollowUp: a frame appended after the snapshot's applied_seq.
        let cp = dir.path().join("followup.lxud");
        let hist = open_hist(&cp);
        hist.apply_records(&[committed("きょう", "今日")]);
        hist.raise_unpersisted_deletion();
        lock_recover(&hist.wal)
            .append_record(&WalRecord::Committed {
                segments: vec![("あす".to_string(), "明日".to_string())],
                timestamp: 0,
            })
            .unwrap();
        assert!(matches!(hist.run_compact(), CompactOutcome::FollowUp));
        assert!(
            hist.durability_issues().is_empty(),
            "FollowUp still wrote a durable checkpoint"
        );

        // Failed truncation: reported as Done, and equally durable.
        let cp2 = dir.path().join("truncfail.lxud");
        let io = FaultyIo::default();
        io.fail_truncates.store(true, Ordering::SeqCst);
        let hist2 = hist_with_io(&cp2, io.boxed());
        hist2.apply_records(&[committed("きょう", "今日")]);
        hist2.raise_unpersisted_deletion();
        assert!(matches!(hist2.run_compact(), CompactOutcome::Done));
        assert!(
            hist2.durability_issues().is_empty(),
            "the checkpoint is durable regardless"
        );
    }

    #[test]
    fn test_concurrent_raises_converge_once_the_disk_heals() {
        // Real interleaving of raises against compactions, which the healthy
        // smoke test cannot produce (it never fails, so the ledger stays at
        // 0/0). Writers keep deleting while their tombstone flushes fail, so
        // raises land concurrently with the compactions those same calls
        // trigger.
        //
        // The property asserted is convergence in the safe direction: once
        // the disk recovers and one more compaction lands, every raise must
        // be covered. A ledger that lost a cover would stay lit forever; one
        // that over-covered would have shown clean before the heal, which the
        // mid-run assertion catches.
        //
        // The checkpoint has to fail too, not just the flush: on a writable
        // directory the §5.4 synchronous fallback persists each deletion
        // before `apply_records` returns and covers it immediately, so the
        // ledger legitimately never stays lit. That is the double failure
        // #295 is about.
        let dir = tempfile::tempdir().unwrap();
        let cp = blocked_checkpoint(dir.path());
        let io = FaultyIo::default();
        io.fail_full_sync.store(true, Ordering::SeqCst);
        let hist = hist_with_io(&cp, io.boxed());

        const WRITERS: usize = 4;
        let mut handles = Vec::new();
        for w in 0..WRITERS {
            let hist = Arc::clone(&hist);
            handles.push(std::thread::spawn(move || {
                for i in 0..25 {
                    let reading = format!("よみ{w}-{i}");
                    let surface = format!("面{w}-{i}");
                    hist.apply_records(&[committed(&reading, &surface)]);
                    // Deletes a pair this thread just learned, so the
                    // tombstone is never a no-op and its flush always fails.
                    hist.apply_records(&[deletion(&reading, &surface)]);
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        // Every one of those deletions had its flush fail, so the channel
        // must be reporting — never silently clean.
        assert!(
            hist.durability_issues()
                .contains(&LexHistoryDurabilityIssue::DeletionNotPersisted),
            "failed flushes must not read as clean"
        );

        // The disk heals; one covering compaction settles every raise.
        io.fail_full_sync.store(false, Ordering::SeqCst);
        unblock_checkpoint(&cp);
        assert!(matches!(hist.run_compact(), CompactOutcome::Done));
        assert!(
            hist.durability_issues().is_empty(),
            "a covering checkpoint must settle every concurrently-raised deletion"
        );

        // And a tombstone whose flush now succeeds neither raises nor leaves
        // the ledger behind — the "the fault was transient" path, which a
        // level-triggered fault switch would otherwise never reach.
        hist.apply_records(&[committed("あす", "明日")]);
        hist.apply_records(&[deletion("あす", "明日")]);
        wait_until(
            || hist.durability_issues().is_empty(),
            "a healthy deletion to leave no issue",
        );
    }

    #[test]
    fn test_clear_truncate_failure_reports_memory_only() {
        // F13. clear freezes the WAL when it cannot truncate it, which makes
        // every later commit memory-only. The Err tells the caller the wipe
        // was partial; the issue tells the user learning has stopped
        // persisting — two different facts with two different lifetimes.
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("history.lxud");
        let io = FaultyIo::default();
        let hist = hist_with_io(&cp, io.boxed());

        hist.apply_records(&[committed("きょう", "今日")]);
        io.fail_truncates.store(true, Ordering::SeqCst);
        assert!(hist.clear_impl().is_err());
        assert!(lock_recover(&hist.wal).is_frozen());
        assert_eq!(
            hist.durability_issues(),
            vec![LexHistoryDurabilityIssue::LearningMemoryOnly]
        );
    }

    #[test]
    fn test_frozen_at_open_is_reported_without_any_append() {
        // F12. Recovery can hand back an already-frozen WAL (here: a failed
        // WAL repair). That freeze has no transition left to publish, so the
        // flag has to be seeded when it is adopted — otherwise the history
        // reads as healthy until the first append fails, which for a user who
        // is not typing may be never.
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("history.lxud");
        // 0-7 bytes: the crash-during-truncation residue recovery re-creates.
        let wal_path = cp.with_extension("lxud.wal");
        std::fs::write(&wal_path, b"\0\0\0").unwrap();
        // Read-only, so the re-creation fails and recovery freezes appends.
        std::fs::set_permissions(&wal_path, std::fs::Permissions::from_mode(0o444)).unwrap();
        if std::fs::OpenOptions::new()
            .write(true)
            .open(&wal_path)
            .is_ok()
        {
            eprintln!("skipping: file permissions are not enforced here (root?)");
            return;
        }

        let hist = open_hist(&cp);
        assert!(hist.open_report().appends_frozen, "fixture must freeze");
        assert_eq!(
            hist.durability_issues(),
            vec![LexHistoryDurabilityIssue::LearningMemoryOnly],
            "a freeze inherited from recovery is live from the first poll"
        );
        std::fs::set_permissions(&wal_path, std::fs::Permissions::from_mode(0o644)).unwrap();
    }

    #[test]
    fn test_healthy_history_reports_nothing() {
        // F15. The channel must stay silent through the ordinary lifecycle,
        // or the menu row becomes noise the user learns to ignore.
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("history.lxud");
        let hist = open_hist(&cp);

        hist.apply_records(&[committed("きょう", "今日")]);
        hist.apply_records(&[deletion("あした", "明日")]); // no-op deletion
        assert!(matches!(hist.run_compact(), CompactOutcome::Done));
        hist.apply_records(&[deletion("きょう", "今日")]); // real deletion
        hist.clear_impl().unwrap();

        assert!(hist.durability_issues().is_empty());
    }

    #[test]
    fn test_durability_issues_does_not_take_the_wal_lock() {
        // F15b. The menu polls this on the main thread while the key thread
        // holds the wal mutex across every append (including a Tombstone's
        // F_FULLFSYNC). Reading through that mutex would stall the UI behind
        // disk I/O — the answer comes from atomics instead.
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("history.lxud");
        let hist = open_hist(&cp);

        // Asserts *completion while the mutex is held*, not elapsed time.
        // Timing the poll itself would flake: this binary runs its tests in
        // parallel, so the measuring thread can be descheduled past any
        // threshold with nothing regressed. Here a false red needs the poller
        // thread to get no CPU at all for two seconds.
        let done = Arc::new(AtomicBool::new(false));
        let completed = {
            // Held for this whole block, so the poller cannot finish before
            // the mutex is taken and pass the test vacuously.
            let _wal = lock_recover(&hist.wal);
            let poller = {
                let hist = Arc::clone(&hist);
                let done = Arc::clone(&done);
                std::thread::spawn(move || {
                    let issues = hist.durability_issues();
                    done.store(true, Ordering::SeqCst);
                    issues
                })
            };
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while !done.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            let completed = done.load(Ordering::SeqCst);
            // Guard drops here, so a regressed poll unblocks and the join
            // below returns instead of hanging the suite.
            drop(_wal);
            poller.join().unwrap();
            completed
        };
        assert!(
            completed,
            "the poll did not return while the wal mutex was held"
        );
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
                // Nothing failed on this tempdir, so no raise ever happened
                // and the channel must be silent. This says nothing about the
                // raise/cover interleavings — with no faults injected the
                // ledger sits at 0/0 throughout, so the assertion would hold
                // with the cover stubbed out. Concurrent raises are covered by
                // `test_concurrent_raises_converge_once_the_disk_heals`.
                assert!(
                    hist.durability_issues().is_empty(),
                    "no failures, no issues"
                );
                drop(gate);
                break;
            }
            drop(gate);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}
