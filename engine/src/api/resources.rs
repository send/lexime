use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

use tracing::warn;

use crate::dict::connection::ConnectionMatrix;
use crate::dict::{CompositeDictionary, Dictionary, TrieDictionary};
use crate::session::LearningRecord;
use crate::user_history::deletion_marker::{self, DeletionBreach, MarkerState};
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
    /// Runtime durability ledger (#295 / #288): what memory holds that no
    /// durable checkpoint covers.
    ///
    /// Three generations packed into one atomic — `raised_memory_only`,
    /// `raised_deletion`, `covered` — so the whole report comes from a single
    /// load. Two of them are separate facts, not one: an `Io` append leaves
    /// the effect in memory alone, while a `SyncFailed` tombstone *is* on
    /// disk yet still breached the flush the deletion contract promises.
    ///
    /// This replaced deriving "learning is memory-only" from
    /// `HistoryWal::is_frozen()`. That derivation rested on "a commit is
    /// memory-only ⟺ the WAL is frozen", which is false on a reachable path:
    /// a commit refused by the frozen guard never reaches seq assignment, so
    /// `last_appended_seq` does not move, and an in-flight compaction whose
    /// snapshot predates that commit then satisfies `truncate_covered` and
    /// clears the freeze — leaving the commit in neither the checkpoint nor
    /// the WAL while the report reads clean (Codex R2). Tracking the fact
    /// directly removes the proxy rather than defending the equivalence
    /// again; the freeze stays what its name says, a property of the file.
    ///
    /// 21 bits per generation: a wrap needs ~2M failed appends inside one
    /// session, and the values never leave the process.
    durability_ledger: AtomicU64,
    /// What the unpersisted-deletion marker should say right now.
    ///
    /// The marker is documented — in SPEC and in AGENTS — as *the ledger's
    /// on-disk projection*, and this is the value it projects. Four call sites
    /// used to maintain the file incrementally, each with its own flags and
    /// its own decision about when to touch it, and three consecutive review
    /// rounds each found one of them forgetting a case: an acknowledgement
    /// that ignored a failed unlink, a wipe that did the same, a raise that
    /// skipped the write when the breach it carried was `None`. Incremental
    /// maintenance is not a projection. Sites now update this value and project it with
    /// [`Self::project_marker`]; [`Self::apply_marker`] is the only thing that
    /// touches the file.
    ///
    /// Two claims, kept apart because they retire on different events:
    /// - `session` — what this process has failed to persist. Retired by a
    ///   durable checkpoint covering it. Lives here.
    /// - `inherited` — what a previous session left. The ledger's `covered`
    ///   has no authority over it: a checkpoint written now persists the
    ///   *resurrected* entry rather than removing it. Retired by delivery
    ///   (`ack_open_report`) or by a wipe. Lives in
    ///   [`Self::inherited_owed`], outside this mutex.
    ///
    /// Every mutation happens under the wal mutex, so the value and the file
    /// cannot be updated out of order. This mutex itself is **never held
    /// across I/O** — the status menu reads it through
    /// [`Self::deletion_report_owed`], and AGENTS' ledger entry makes
    /// "the read must take no lock" a hard blocker precisely because a UI poll
    /// must not queue behind history I/O. Holders are instruction-length; the
    /// wal mutex is what actually serializes writers.
    claims: Mutex<MarkerClaims>,
    /// Whether a previous session's report is still owed.
    ///
    /// A bool rather than a claim, and outside the mutex, for one reason each.
    /// It is faithful because recovery reports `deletion_lost` only from the
    /// branch that promotes the claim to unconditional, so an inherited claim
    /// is *always* `Lost` — this is the claim, not a cached derivation of it.
    /// And it is lock-free because the status menu reads it on the main
    /// thread: AGENTS' ledger entry makes "the read must take no lock" a hard
    /// blocker, and a mutex there can be waited on whenever a history worker
    /// is preempted mid-update, however briefly it means to hold it.
    inherited_owed: AtomicBool,
}

/// The two outstanding deletion claims, and what they project onto disk.
#[derive(Clone, Copy, Default)]
struct MarkerClaims {
    session: Option<DeletionBreach>,
    /// What the marker file holds, as far as this process knows.
    ///
    /// Three states, because reality has three and an `Option` has two. The
    /// missing one is [`MarkerState::Unknown`] — a file is there and nobody
    /// has managed to read it — and collapsing that into "absent" is what let
    /// a failed unlink of an unreadable marker look settled: the projection
    /// found `None == None`, skipped, and the surviving file reported a lost
    /// deletion on the next start. A `confirmed` bit bolted onto the *read*
    /// was the half-measure; the belief itself has to carry it.
    ///
    /// Set from a confirmed write, or seeded from what recovery observed.
    /// Re-reading the file to decide instead would be wrong: matching bytes
    /// prove the content reached the page cache, not that `sync_all`
    /// returned — so a failed flush would read back as up to date and never be
    /// retried, which is the power-loss window the marker exists to close.
    flushed: MarkerState,
}

impl MarkerClaims {
    /// What the marker should hold — the stronger of the two claims, or
    /// nothing. `inherited` is passed in because it is kept outside this
    /// mutex; see [`LexUserHistory::inherited_owed`].
    fn projected(&self, inherited: bool) -> Option<DeletionBreach> {
        match (self.session, inherited) {
            (Some(s), true) => Some(s.merge(DeletionBreach::Lost)),
            (Some(s), false) => Some(s),
            (None, true) => Some(DeletionBreach::Lost),
            (None, false) => None,
        }
    }
}

/// Layout of `LexUserHistory::durability_ledger`, low bits first:
/// `covered | raised_deletion | raised_memory_only`, 21 bits each.
const GEN_BITS: u32 = 21;
const GEN_MASK: u64 = (1 << GEN_BITS) - 1;

/// Fold one record's breach into the batch's claim, so the merge rule that
/// makes `Lost` absorbing is stated once rather than at each arm that raises.
fn note_breach(slot: &mut Option<DeletionBreach>, breach: DeletionBreach) {
    *slot = Some(slot.map_or(breach, |prev| prev.merge(breach)));
}

fn covered_of(ledger: u64) -> u64 {
    ledger & GEN_MASK
}

fn raised_deletion_of(ledger: u64) -> u64 {
    (ledger >> GEN_BITS) & GEN_MASK
}

fn raised_memory_only_of(ledger: u64) -> u64 {
    (ledger >> (2 * GEN_BITS)) & GEN_MASK
}

fn highest_raised(ledger: u64) -> u64 {
    raised_memory_only_of(ledger).max(raised_deletion_of(ledger))
}

fn pack_ledger(memory_only: u64, deletion: u64, covered: u64) -> u64 {
    // Mask every field: an unmasked value would not merely wrap, it would
    // carry into the neighbouring generation and corrupt it.
    ((memory_only & GEN_MASK) << (2 * GEN_BITS))
        | ((deletion & GEN_MASK) << GEN_BITS)
        | (covered & GEN_MASK)
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
    /// A confirmed conversion was applied with no WAL frame behind it, and
    /// no durable checkpoint covers it yet — so only memory holds it. A
    /// restart recovers what the last checkpoint holds; that conversion is
    /// lost.
    ///
    /// Not derived from the WAL's freeze state: a frozen WAL with nothing
    /// learned since puts nothing at risk, and a freeze can be cleared by a
    /// compaction whose snapshot predates the commit it refused. Retraction
    /// is a durable checkpoint covering the commit, not the thaw.
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
    /// A deletion from a previous session never reached disk, and the state
    /// just loaded may still hold the entry it was meant to remove (#312).
    ///
    /// Surfaced here rather than through `durability_issues()` on lifetime:
    /// the runtime list reports what holds *now* and retracts when a
    /// checkpoint covers it, whereas no disk recovery retracts this — the
    /// deletion is already lost. What does retire it is a user action, and
    /// consuming it is an explicit `ack_open_report()`, so the on-disk record
    /// outlives the gap between this report being built and something acting
    /// on it.
    pub deletion_lost: bool,
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
            deletion_lost: r.deletion_lost,
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
        // A previous session's deletion that this startup's replay applied is
        // not settled: replay read the frame from the page cache, and the
        // flush that failed never happened. Seeding the ledger makes it what
        // it actually is — a live durability problem — so the first durable
        // checkpoint both clears the row and unlinks the marker, instead of
        // recovery retracting on evidence it does not have.
        // The claim a previous session left, if the report says one is owed.
        // Always `Lost`, never re-read: recovery sets `deletion_lost` only in
        // the branch that promotes the claim to unconditional, and the one
        // case where the file still says `Unflushed` there is a promotion
        // whose write failed. Re-reading would carry that suppressible witness
        // back into memory and project it again — undoing the promotion the
        // report is predicated on. It also costs no syscall on the startup
        // thread.
        let inherited_owed = report.deletion_lost;
        let marker_on_disk = report.marker_on_disk;
        let pending_claim = match (report.deletion_pending_checkpoint, marker_on_disk) {
            (true, MarkerState::Holds(breach)) => Some(breach),
            _ => None,
        };
        let ledger = if report.deletion_pending_checkpoint {
            pack_ledger(0, 1, 0)
        } else {
            0
        };
        let this = Arc::new(Self {
            inner: Arc::new(RwLock::new(history)),
            wal: Mutex::new(wal),
            compact_gate: Mutex::new(()),
            scrub_pending: AtomicBool::new(false),
            commit_log: Mutex::new(commit_log),
            report,
            durability_ledger: AtomicU64::new(ledger),
            claims: Mutex::new(MarkerClaims {
                // The replayed witness, when there is one.
                // `deletion_pending_checkpoint` raises the *ledger* for a
                // deletion that replay applied but no checkpoint covers;
                // leaving `session` empty made the projection compute a
                // desired state of `None` and — once every commit reconciles —
                // unlink that witness before any checkpoint had persisted the
                // deletion, so a power loss would restore the entry with
                // nothing left to report it. The ledger and the claims have to
                // agree about what is outstanding.
                //
                // Confirmed by construction: an unreadable marker resolves to
                // `Lost`, which is always outstanding and never reaches the
                // branch that sets this flag, so this is a decoded `Unflushed`.
                session: pending_claim,
                // Observed, not assumed. When recovery promoted the claim
                // successfully this equals what the projection wants, so the
                // first compaction skips the write entirely; when it did not,
                // the mismatch is what drives the re-assertion.
                flushed: marker_on_disk,
            }),
            inherited_owed: AtomicBool::new(inherited_owed),
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
    ///
    /// Pure — a getter that mutated the disk would be a trap. Clearing the
    /// unpersisted-deletion record is [`Self::ack_open_report`], called once
    /// the caller has actually taken the report.
    fn open_report(&self) -> LexHistoryOpenReport {
        (&self.report).into()
    }

    /// Acknowledge [`Self::open_report`]: the report has been shown to the
    /// user, so the on-disk record behind `deletion_lost` can go.
    ///
    /// Acknowledges the report as a whole, not one field of it: today
    /// `deletion_lost` is the only fact with durable state behind it, and a
    /// later deliver-once fact joins here rather than growing a second ack.
    ///
    /// Separate from `open_report` because it is *delivery* that retires the
    /// record, not the read. `open_recovering` deliberately leaves the marker
    /// alone: it returns long before anything consumes the report, and the
    /// launches this exists for are the ones where a failing disk may take the
    /// process down in between. For the same reason the caller must not ack at
    /// load — a short-lived IMKit probe launch opens the history, never shows a
    /// menu, and would consume the report on the user's behalf. Nor at
    /// menu-build time: IMKit constructs the menu without displaying it, so
    /// construction is not delivery either. Ack when a person **clicks** the
    /// row — that is the only evidence anyone saw it.
    ///
    /// Two guards, both load-bearing:
    /// - **the ledger, not the startup flag.** `report.deletion_lost` is frozen
    ///   at open, so acting on it alone would delete a marker written by a
    ///   raise that landed since — the session's own breach, silently dropped.
    ///   Asking the ledger is the question `cover_unpersisted` asks, of the
    ///   same authority.
    /// - **`try_lock`.** This runs on the main thread when the menu opens, and
    ///   only when the disk is degraded — exactly when a compaction may hold
    ///   the wal mutex across `cover_durable_residue` and file I/O. A skipped
    ///   ack costs one more report next launch, the safe direction; a blocked
    ///   main thread costs the UI.
    ///
    /// Whether the row should still be shown afterwards is
    /// [`Self::deletion_report_owed`], not this call's outcome — the same
    /// predicate answers for a wipe, which retires the report without anyone
    /// acknowledging it. Both early exits below leave the marker on disk and
    /// the report owed, so a caller that drops the row on a failed
    /// acknowledgement takes away the only affordance for retrying it while
    /// the warning returns on every launch.
    ///
    /// Idempotent; safe to call when nothing was reported.
    fn ack_open_report(&self) {
        if !self.report.deletion_lost {
            return;
        }
        // Under the wal mutex like every other marker operation, so an
        // acknowledgement cannot land between a raise and its projection.
        let wal = match self.wal.try_lock() {
            Ok(w) => w,
            Err(std::sync::TryLockError::WouldBlock) => return,
            Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner(),
        };
        let ledger = self.durability_ledger.load(Ordering::SeqCst);
        if raised_deletion_of(ledger) > covered_of(ledger) {
            // This session raised a breach of its own after the report was
            // built. The marker still has to carry that, so there is nothing
            // to deliver away yet.
            return;
        }
        let session_only = *lock_recover(&self.claims);
        // `session` is provably `None` here — a session claim implies
        // `raised_deletion > covered`, which the guard above returned on — so
        // the projection without the inherited claim is an unlink. Requiring
        // that explicitly keeps a future change to that guard from turning
        // this into "wrote the session's claim, then retired the inherited
        // one", which is the shape R3 found.
        //
        // Confirmed by mutation that no test can detect this clause today —
        // the ledger guard makes it always true — so it is stated here rather
        // than left to a reader to re-derive.
        let desired = session_only.projected(false);
        if desired.is_none() && self.apply_marker(&wal, None) {
            // The only site that commits its change *after* the disk agrees.
            // Delivery is not done until the record is gone: dropping the
            // claim on a failed unlink would take the row away while the
            // marker stood, and the warning would come back on the next
            // launch with the retry.
            self.inherited_owed.store(false, Ordering::SeqCst);
        }
    }

    /// Whether a lost-deletion report from a previous session is still owed.
    ///
    /// The single authority for whether the status row belongs on screen. Both
    /// an acknowledgement the engine could not complete and a wipe that failed
    /// before its commit point leave it owed, so one question answers for both
    /// call sites.
    fn deletion_report_owed(&self) -> bool {
        self.inherited_owed.load(Ordering::SeqCst)
    }

    /// Durability problems that hold right now, most severe first.
    ///
    /// Polled by the UI (the status menu re-reads on every open), so it must
    /// not block: one atomic load, never the wal mutex the key-processing
    /// thread holds across appends.
    ///
    /// One load is also what makes the pair consistent. Read as two
    /// independent atomics — which a separate freeze flag forced — a raise
    /// landing between them yields a pair that never simultaneously held,
    /// dropping the more severe row while keeping the lesser one (Codex R1).
    fn durability_issues(&self) -> Vec<LexHistoryDurabilityIssue> {
        let ledger = self.durability_ledger.load(Ordering::SeqCst);
        let covered = covered_of(ledger);

        let mut issues = Vec::new();
        // First: a deletion that did not persist has no startup heal, and it
        // is the one the user explicitly asked for.
        if raised_deletion_of(ledger) > covered {
            issues.push(LexHistoryDurabilityIssue::DeletionNotPersisted);
        }
        if raised_memory_only_of(ledger) > covered {
            issues.push(LexHistoryDurabilityIssue::LearningMemoryOnly);
        }
        issues
    }
}

impl LexUserHistory {
    /// Make the marker say `desired`, and report whether the file now agrees.
    ///
    /// The only place the marker is written or removed. Sites decide what the
    /// claims are; this projects them. On failure the caller's claim stays in
    /// memory, so the next site to project re-asserts it — which is what makes
    /// a transient write failure recover without anyone remembering to retry.
    fn apply_marker(
        &self,
        wal: &MutexGuard<'_, HistoryWal>,
        desired: Option<DeletionBreach>,
    ) -> bool {
        if lock_recover(&self.claims).flushed.satisfies(desired) {
            // The disk already says what it should. Symmetric — `None == None`
            // skips too — which is sound only because `flushed` is seeded from
            // what recovery observed rather than assumed: an asymmetric skip
            // was how a healthy compaction still paid two `unlink` syscalls
            // inside the wal critical section for a file that was never there.
            //
            // The `Some` side matters more: the claim is re-projected on every
            // raise while it is outstanding, and each write is an F_FULLFSYNC
            // on the key-processing thread.
            //
            // A cost, not a behaviour: removing this skip is invisible to the
            // tests by construction, which is why the *recording* of what the
            // disk holds is what they pin instead.
            return true;
        }
        match desired {
            Some(claim) => match deletion_marker::merge_write(wal.checkpoint_path(), claim) {
                Ok(persisted) => {
                    // What landed, not what was asked for: the write merges, so
                    // a request of `Unflushed` over a surviving `Lost` leaves
                    // `Lost` on disk. Recording the request would be a belief
                    // the disk never held, and the next reconcile would find it
                    // already satisfied and skip.
                    lock_recover(&self.claims).flushed = MarkerState::Holds(persisted);
                    true
                }
                Err(e) => {
                    // The belief is now *unknown*, not simply stale. A write
                    // that failed may still have left a truncated orphan — a
                    // short write on ENOSPC — and the previous value does not
                    // describe that. Keeping it let a later `Absent` satisfy a
                    // desired `None`, skip the removal, and leave the malformed
                    // orphan for the next startup to decode as `Lost` and
                    // report against a deletion the checkpoint had persisted.
                    warn!("failed to record the unpersisted deletion for the next start: {e}");
                    lock_recover(&self.claims).flushed = MarkerState::Unknown;
                    false
                }
            },
            None => {
                let cleared = deletion_marker::remove(wal.checkpoint_path());
                lock_recover(&self.claims).flushed = if cleared {
                    MarkerState::Absent
                } else {
                    // Same rule: a removal that did not complete leaves the
                    // path in a state this process has not observed, and only
                    // `Unknown` says so. Holding the old value would let a
                    // later projection decide it matched.
                    MarkerState::Unknown
                };
                cleared
            }
        }
    }

    /// Project the current claims onto disk. Sites that settle a claim
    /// unconditionally — a cover, a wipe — use this; only the acknowledgement
    /// needs to know whether the disk agreed.
    /// Returns whether the disk now says what the projection wants.
    fn project_marker(&self, wal: &MutexGuard<'_, HistoryWal>) -> bool {
        // The guard is released before the I/O — see the field docs: every
        // holder of `claims` must be instruction-length, because the status
        // menu reads it.
        let desired =
            lock_recover(&self.claims).projected(self.inherited_owed.load(Ordering::SeqCst));
        self.apply_marker(wal, desired)
    }

    /// Record what this batch failed to make durable (#295 / #288).
    ///
    /// `memory_only` — at least one effect was applied with no WAL frame, so
    /// only memory holds it. `deletion_breach` — a deletion's durability
    /// failed, which includes `SyncFailed`, where the frame *is* on disk but
    /// the flush the Tombstone contract promises did not happen. They are
    /// tracked apart because neither implies the other.
    ///
    /// Called under the wal mutex, after the in-memory apply. Both placements
    /// are load-bearing:
    /// - after the apply, so a compactor that observes this raise is
    ///   guaranteed to snapshot a history that already excludes the entry.
    ///   [`Self::snapshot_to_cover`] holds `inner` for reading while it takes
    ///   both, and the apply needs `inner` for writing, so the two cannot
    ///   interleave the wrong way round;
    /// - under the wal mutex, so it cannot land inside `clear_impl`'s
    ///   read-then-cover window. A raise slipping in there would outlive a
    ///   wipe that made it vacuously true, leaving a privacy warning on a
    ///   history that is provably empty. The guard is a parameter so that half
    ///   is checked by the compiler rather than merely documented — it is also
    ///   what names the checkpoint the marker sits beside, so the requirement
    ///   and the use are the same object.
    fn raise_unpersisted(
        &self,
        wal: &MutexGuard<'_, HistoryWal>,
        memory_only: bool,
        deletion_breach: Option<DeletionBreach>,
    ) {
        if !memory_only && deletion_breach.is_none() {
            return;
        }
        // On disk before the ledger, and before the synchronous checkpoint
        // fallback the caller runs next (§5.4) — the same write-ahead
        // discipline the WAL itself follows. Writing after the fallback would
        // open a crash window whose only outcome is the silent one; writing
        // first can only over-report, since a fallback that succeeds unlinks
        // the marker through the cover below.
        // Projected on every raise, not only when this one carries a breach.
        // A write that failed leaves the claim in memory, and the next raise
        // re-asserts it — including a memory-only one, which is the shape that
        // used to skip the retry entirely and let a recovered disk go
        // unrecorded.
        {
            let mut claims = lock_recover(&self.claims);
            if let Some(breach) = deletion_breach {
                // Through `note_breach`, which exists so the rule that makes
                // `Lost` absorbing is written once rather than at each arm
                // that raises.
                note_breach(&mut claims.session, breach);
            }
            let desired = claims.projected(self.inherited_owed.load(Ordering::SeqCst));
            drop(claims);
            if desired.is_some() {
                self.apply_marker(wal, desired);
            }
        }
        let mut current = self.durability_ledger.load(Ordering::SeqCst);
        loop {
            let mem = raised_memory_only_of(current);
            let del = raised_deletion_of(current);
            // One shared sequence, so a single `covered` settles both.
            //
            // Saturating, not wrapping. A wrap reads back as 0, which puts
            // the condition *below* `covered` and stops reporting it — the
            // one direction this design forbids. Pinned at GEN_MASK the
            // generation can never be reached by `covered` (capped one
            // lower), so a saturated ledger reports forever: over-reporting,
            // which is the safe direction. Needs ~2M failed appends in one
            // session to reach.
            let next = (mem.max(del).max(covered_of(current)) + 1).min(GEN_MASK);
            let updated = pack_ledger(
                if memory_only { next } else { mem },
                if deletion_breach.is_some() { next } else { del },
                covered_of(current),
            );
            match self.durability_ledger.compare_exchange_weak(
                current,
                updated,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return,
                Err(observed) => current = observed,
            }
        }
    }

    /// Take the generation to cover *together with* the snapshot that will
    /// cover it, under one `inner` read guard.
    ///
    /// The guard is what makes the pairing sound — not the two statements
    /// happening to sit in the right order. Every raise is preceded by
    /// `apply_batch`, which needs `inner` for writing, so while this guard is
    /// held no raise can slip its effect past the clone: a concurrent raise
    /// either finished before the guard (its effect is in the snapshot) or is
    /// still blocked on the write lock (its generation lands after this load,
    /// so it stays outstanding, which is the safe direction).
    ///
    /// A third interleaving exists and is deliberately allowed: `apply_batch`
    /// has released `inner` but the raise two statements later has not run,
    /// so the snapshot excludes the effect yet the cover carries the older
    /// generation. That is a transient false positive — the same
    /// `apply_records` call then posts a compaction that covers it — and it
    /// errs toward reporting, which is the direction this design accepts.
    fn snapshot_to_cover(&self) -> (u64, UserHistory) {
        let history = read_recover(&self.inner);
        let generation = highest_raised(self.durability_ledger.load(Ordering::SeqCst));
        (generation, history.clone())
    }

    /// The generation to cover for a wipe. A bare load is sound here because
    /// the caller holds the wal mutex across both this read and the cover
    /// (hence the guard witness), and every raise happens under that mutex —
    /// so unlike the compaction path there is no window to close.
    fn deletion_gen_under_wal_lock(&self, _wal: &MutexGuard<'_, HistoryWal>) -> u64 {
        highest_raised(self.durability_ledger.load(Ordering::SeqCst))
    }

    /// Mark everything raised up to `generation` as persisted. Called only
    /// once a checkpoint that covers it is durable on disk — a checkpoint is
    /// a full snapshot, so one `covered` settles both kinds.
    ///
    /// A CAS loop rather than a store: it must not clobber a raise that
    /// landed since the load (the retry picks up the new generations), and it
    /// must never walk `covered` backwards.
    ///
    /// The on-disk marker is re-projected here, in the same call and under the
    /// same wal guard as the CAS that settles the ledger — not as a follow-up
    /// statement in the caller. Re-projected rather than unlinked: an
    /// inherited claim nobody has delivered yet still has to be on disk, and
    /// this checkpoint is no authority over it. Between a successful CAS and a
    /// separate unlink there is a window of a few instructions in which a new
    /// raise can write a marker that the unlink then destroys, dropping the
    /// report for a deletion that is still outstanding. That window is not
    /// something a deterministic test can pin (#317 proved twice that tests
    /// over such windows pass under mutation), so it is removed by
    /// construction: the guard witness makes "cover without the wal mutex" not
    /// compile, and every raise takes the same mutex.
    ///
    /// **Unconditionally, including on the early-return path**, and that is
    /// not a cost the steady state pays: a projection whose write or unlink
    /// failed has nothing else to revisit it — the ledger is covered, so later
    /// covers return early, and a healthy session raises nothing — so the
    /// durable checkpoint is the retry. `apply_marker` skips when the disk
    /// already agrees, which on a healthy history is every time, so no syscall
    /// is issued inside the critical section the key thread waits on. Having
    /// the caller do it instead left "every cover is followed by a projection
    /// under the same guard" as an unenforced convention.
    fn cover_unpersisted(&self, wal: &MutexGuard<'_, HistoryWal>, generation: u64) {
        self.settle_ledger(generation);
        // Paired here, so it cannot be forgotten at a call site: the two
        // together are what "a durable checkpoint reconciles the record" means.
        self.project_marker(wal);
    }

    /// Move `covered` to `generation`, settling the session's claim if this
    /// checkpoint is what settled it. Split out only so the projection above
    /// runs on every path, including the already-covered early return — the
    /// retry has to happen whether or not this particular call moved anything.
    fn settle_ledger(&self, generation: u64) {
        let mut current = self.durability_ledger.load(Ordering::SeqCst);
        loop {
            if covered_of(current) >= generation {
                return;
            }
            let covered = generation.min(GEN_MASK - 1);
            let updated = pack_ledger(
                raised_memory_only_of(current),
                raised_deletion_of(current),
                // One below the raise ceiling, so a saturated generation
                // stays outstanding rather than being covered by accident.
                covered,
            );
            match self.durability_ledger.compare_exchange_weak(
                current,
                updated,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => {
                    // Decided from the values this CAS itself exchanged, not
                    // from a fresh load: a re-read could see a raise that
                    // landed after the swap and mistake it for one this
                    // checkpoint covered.
                    let raised = raised_deletion_of(current);
                    if raised > covered_of(current) && raised <= covered {
                        // The session's own claim is settled by this durable
                        // checkpoint. The inherited one is not — a checkpoint
                        // written now persists the *resurrected* entry — and
                        // the projection keeps the file if that is still owed.
                        lock_recover(&self.claims).session = None;
                    }
                    return;
                }
                Err(observed) => current = observed,
            }
        }
    }

    #[cfg(test)]
    fn has_unpersisted_deletion(&self) -> bool {
        let ledger = self.durability_ledger.load(Ordering::SeqCst);
        raised_deletion_of(ledger) > covered_of(ledger)
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
        //
        // Accumulated across the batch as a `DeletionBreach` rather than a
        // bool, because the two halves are not interchangeable on the next
        // start: `Io` has no frame and no heal, while `Unflushed` is settled
        // by replay. `merge` keeps the claim that covers the whole batch —
        // `Lost` absorbs, and two unflushed frames keep the higher seq.
        let mut durability_failed: Option<DeletionBreach> = None;
        let mut needs_threshold_compact = false;
        if !wal_records.is_empty() {
            let mut wal = lock_recover(&self.wal);
            // Reconcile the marker with what the projection wants, before this
            // batch touches the WAL. **This is where the projection's
            // correctness lives**; every other projection point is either
            // write-ahead ordering or promptness.
            //
            // Six review rounds arrived here one event at a time — the ack, the
            // wipe, the compaction's cover, recovery's removal, recovery's
            // promotion, and then the compaction that recovery schedules, each
            // one a place where a failed write had nothing to revisit it. The
            // answer was never another trigger. A record that is only
            // reconciled at *some* events is not a projection, it is a cache
            // with invalidation, and the design calls this the ledger's on-disk
            // projection. So it reconciles wherever the process is running and
            // able to act, which is here.
            //
            // Free in the steady state, and only because `flushed` is seeded
            // from what recovery observed: `apply_marker` compares two
            // `Option<DeletionBreach>` under a mutex this thread already holds
            // and returns. No syscall on a healthy history, which is what makes
            // a key-path reconcile affordable at all.
            //
            // Before the appends, not after: the harm this closes is a WAL that
            // advances past an un-promoted witness, so promoting after the
            // append would leave the same window one batch wide.
            // Unconditionally, and its result decides the freeze below —
            // `&&` would short-circuit the reconcile away on the healthy path,
            // which is the one place it has to happen.
            let projected = self.project_marker(&wal);
            if !projected
                && self.inherited_owed.load(Ordering::SeqCst)
                && lock_recover(&self.claims).flushed != MarkerState::Holds(DeletionBreach::Lost)
            {
                // The one case where appending is genuinely unsafe: a report is
                // owed and the disk does not yet say so *unconditionally* —
                // either nobody could read it, or it still holds the
                // `Unflushed{seq}` the promotion failed to replace.
                //
                // A witness is a claim about one frame in one WAL file, and it
                // is answered by `seq > applied_seq` — an **inequality** over a
                // high water mark. Gaps are legal, so once that file has been
                // replaced *any* later frame above the witness answers it
                // "applied", whether or not the tombstone ever existed in the
                // new lineage. Numbering cannot fix this — a floor was tried,
                // and skipping one number changes nothing — so promotion to the
                // lineage-independent `Lost` is mandatory, and until it lands
                // the state that would answer the witness must not advance.
                //
                // Freezing is not a new mechanism: `frozen` already means
                // "this file is not in a state where appending is safe", and
                // numbering that may alias a live claim is exactly that. The
                // batch becomes memory-only — reported as `LearningMemoryOnly`
                // and healed by the compaction that rewrites both files, the
                // same path an unrepairable tail takes.
                //
                // Deliberately *not* the broader "freeze whenever the marker
                // write fails": a sidecar must not stop learning, the same rule
                // that keeps a read failure from failing the open. What is
                // exempt is a disk that already says `Lost` — unsatisfiable by
                // any sequence, so nothing it could answer is at risk — and a
                // claim this session raised about the file it is still
                // appending to. An *inherited* witness is the dangerous one,
                // which is why the condition names it.
                wal.freeze();
            }
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
                            note_breach(&mut durability_failed, DeletionBreach::Unflushed { seq });
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
                            note_breach(&mut durability_failed, DeletionBreach::Lost);
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
            // Still holding the wal mutex, and after the apply above — see
            // `raise_unpersisted` for why both matter. A `None` seq means the
            // frame never landed, so only memory holds that effect; this is
            // what makes "learning is memory-only" a tracked fact instead of
            // something inferred from the WAL's freeze state.
            // Committed only. A Tombstone that never reached the WAL is a
            // *lost deletion*, which `durability_failed` already carries —
            // telling the user their learning is unsaved when all they did
            // was delete would be the same error as reporting a freeze that
            // has put nothing at risk yet.
            let memory_only = sequenced.iter().any(|(record, seq)| {
                seq.is_none() && matches!(record, WalRecord::Committed { .. })
            });
            self.raise_unpersisted(&wal, memory_only, durability_failed);
        }

        for line in &log_lines {
            self.append_commit_log(line);
        }

        if durability_failed.is_some() {
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
        let covered_gen = self.deletion_gen_under_wal_lock(&wal);

        // Commit point. On Err nothing has changed — including
        // scrub_pending, so a pre-clear Tombstone's scrub request stays
        // posted for the next compaction.
        let mut empty = UserHistory::new();
        empty.advance_applied_seq(wal.last_appended_seq());
        empty.save(wal.checkpoint_path())?;

        // Consumed only after the commit point: the wipe supersedes every
        // scrub request posted so far.
        self.scrub_pending.store(false, Ordering::SeqCst);
        // A wipe settles every claim before the cover, not after it: they said
        // an entry might be back, and now nothing is. Ordering matters — the
        // cover projects, so resetting afterwards would have it write the
        // pre-wipe claim to disk and then need a second projection to take it
        // straight back off.
        //
        // The reset holds whether or not the file can be unlinked: an
        // unremovable marker is stale, not owed. What does *not* follow, and
        // was claimed here until the design re-gate, is that the next startup
        // reaches the same verdict on its own — it only does so while the
        // history is still empty, and the user typing one thing before the
        // restart makes replay non-empty and the stale `Lost` report again.
        // `flushed` carries the disagreement instead, so the projection keeps
        // retrying; the heal below is what makes the retry prompt.
        // `session` only. `flushed` is not a claim to be settled, it is what
        // this process knows about the disk, and a wipe does not make the file
        // disappear — clearing it here would have the projection conclude the
        // disk already agrees and skip the very removal this is for.
        lock_recover(&self.claims).session = None;
        self.inherited_owed.store(false, Ordering::SeqCst);
        // Likewise for the durability ledger (#295). An empty durable set
        // contains no un-deleted entry, so every raised deletion is now
        // vacuously persisted. Without this second cover point, wiping
        // everything would leave a standing "a deletion did not persist"
        // warning on a history that provably holds nothing.
        //
        // Not routed through `remove_recovery_artifacts`: that helper returns
        // on its first error and only reaches the `.corrupt-*` files
        // afterwards, so a marker that refuses to unlink would skip the
        // deletion of files that do hold the user's input text. This one holds
        // none (magic, version, flags, a seq), which is also why its failure
        // stays a log line rather than joining `deferred`.
        self.cover_unpersisted(&wal, covered_gen);
        let marker_stuck = lock_recover(&self.claims).flushed != MarkerState::Absent;

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
            None => {
                if marker_stuck {
                    // Promptness only. Correctness is the commit-path
                    // reconcile: the next commit retires this record whether or
                    // not the compaction below ever runs. Without it a user who
                    // wipes and then stops typing would keep a stale file until
                    // they resumed — harmless, but a wipe should finish.
                    self.spawn_compact();
                }
                Ok(())
            }
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

        // 3. Truncate WAL (brief lock) — conditionally (§5.3): only frames
        // provably covered by the durable checkpoint (seq <= applied_seq at
        // snapshot time) may be destroyed.
        let mut wal = lock_recover(&self.wal);

        // The deletion is persisted the moment the full-snapshot checkpoint
        // above became durable — before the truncation below, and regardless
        // of whether it runs. Truncation is the physical scrub of superseded
        // frames, not what makes the deletion survive a restart, so tying the
        // cover to it would leave a permanent warning whenever frames land
        // mid-run (FollowUp) or the truncate fails on an otherwise durable
        // write. Only the *placement* moved under this guard, and only so the
        // ledger update and the marker unlink cannot be split by a concurrent
        // raise; the condition being covered is unchanged.
        self.cover_unpersisted(&wal, covered_gen);

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

    /// Raise through the real precondition: tests must hold the wal mutex
    /// too, or the witness parameter would be documenting nothing.
    fn raise_under_wal(hist: &LexUserHistory) {
        let wal = lock_recover(&hist.wal);
        hist.raise_unpersisted(&wal, true, Some(DeletionBreach::Lost));
    }

    /// Both halves, as every durable checkpoint does them: the cover settles
    /// the ledger, the projection is what reaches the disk. Splitting them
    /// here would let a test pass against a pairing production does not have.
    fn cover_under_wal(hist: &LexUserHistory, generation: u64) {
        let wal = lock_recover(&hist.wal);
        hist.cover_unpersisted(&wal, generation);
        hist.project_marker(&wal);
    }

    fn gen_under_wal(hist: &LexUserHistory) -> u64 {
        let wal = lock_recover(&hist.wal);
        hist.deletion_gen_under_wal_lock(&wal)
    }

    /// Plant a marker the way a startup hands one over: the file **and** the
    /// runtime's record of what the file holds.
    ///
    /// Writing only the file is a state production cannot reach. Recovery
    /// reports what it read (`OpenReport::marker_on_disk`) and `open` seeds
    /// `flushed` from it, so the process never believes the disk is clear
    /// while bytes sit there. A fixture that skips the second half is testing
    /// the projection against a lie — and since the skip is symmetric, it
    /// would simply decline to project at all.
    fn plant_marker(hist: &LexUserHistory, cp: &Path, breach: DeletionBreach) {
        deletion_marker::merge_write(cp, breach).unwrap();
        lock_recover(&hist.claims).flushed = MarkerState::Holds(breach);
    }

    fn marker(cp: &Path) -> Option<DeletionBreach> {
        deletion_marker::read(cp).map(|o| o.breach)
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
        hist_with_io_reporting(cp, io, false)
    }

    /// As `hist_with_io`, but with a startup report that carries an inherited
    /// lost-deletion claim — the state an acknowledgement acts on.
    fn hist_with_io_reporting(
        cp: &Path,
        io: Box<dyn crate::user_history::wal::WalIo>,
        deletion_lost: bool,
    ) -> Arc<LexUserHistory> {
        // The whole startup state, not two thirds of it: a report is owed
        // because recovery read a marker and left it in place, so the file has
        // to exist alongside `inherited_owed` and `flushed`. Setting only the
        // in-memory halves describes a disk that never matched them, and the
        // projection — which skips when it believes the disk already agrees —
        // would then decline to write the marker a raise is meant to record.
        if deletion_lost {
            deletion_marker::merge_write(cp, DeletionBreach::Lost).unwrap();
        }
        let wal = HistoryWal::with_io(cp, io);
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
                // Paired with `deletion_lost`, because recovery cannot produce
                // one without the other: the report is owed *because* a marker
                // was read and deliberately left in place. A fixture that
                // reported the loss while claiming a clear disk would have the
                // projection skip the removal it exists to perform, and the ack
                // would settle against nothing.
                marker_on_disk: if deletion_lost {
                    MarkerState::Holds(DeletionBreach::Lost)
                } else {
                    MarkerState::Absent
                },
                deletion_lost,
                deletion_pending_checkpoint: false,
            },
            durability_ledger: AtomicU64::new(0),
            claims: Mutex::new(MarkerClaims {
                session: None,
                flushed: if deletion_lost {
                    MarkerState::Holds(DeletionBreach::Lost)
                } else {
                    MarkerState::Absent
                },
            }),
            inherited_owed: AtomicBool::new(deletion_lost),
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

    /// Fail the *checkpoint write only*, leaving every sibling in the family
    /// writable — including the marker, and including the checkpoint file
    /// itself, which a restart still has to read.
    ///
    /// `blocked_checkpoint` cannot serve here: it makes the whole parent
    /// directory a file, so the marker (same directory) cannot be written
    /// either, and a test built on it would observe "no marker" and pass for
    /// the wrong reason — the #312 case would have no test at all. This
    /// instead puts a *directory* at the tmp path `write_atomic` needs, so
    /// `File::create` fails with EISDIR and nothing else is disturbed.
    ///
    /// Idempotent: an `unwrap` here raced a startup compaction that a test's
    /// own `open` had scheduled, which creates and renames that same tmp path.
    /// It passed locally and failed in CI. What the caller needs is the
    /// obstacle to be present, not to have been the one who placed it.
    fn block_checkpoint_write(cp: &Path) {
        match std::fs::create_dir(crate::user_history::checkpoint_tmp_path(cp)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => panic!("could not block the checkpoint write: {e}"),
        }
    }

    /// Make every marker write fail, without disturbing a read: a directory
    /// at the tmp `write_atomic` writes through is refused by `create_regular`
    /// (EISDIR) while the canonical path is untouched.
    fn block_marker_write(cp: &Path) {
        std::fs::create_dir(crate::user_history::checkpoint_tmp_path(
            &deletion_marker::marker_path(cp),
        ))
        .unwrap();
    }

    fn unblock_checkpoint_write(cp: &Path) {
        std::fs::remove_dir(crate::user_history::checkpoint_tmp_path(cp)).unwrap();
    }

    // -----------------------------------------------------------------------
    // The unpersisted-deletion marker (#312): the runtime ledger dies with the
    // process, so before this the `Io` half went unreported on exactly the
    // restart where the entry comes back.
    // -----------------------------------------------------------------------

    /// Learn something, then delete it with the tombstone's append failing —
    /// the `Io` half, where no frame exists at all.
    fn lose_a_deletion(hist: &Arc<LexUserHistory>, io: &FaultyIo) {
        hist.apply_records(&[committed("きょう", "今日")]);
        io.fail_appends.store(true, Ordering::SeqCst);
        hist.apply_records(&[deletion("きょう", "今日")]);
    }

    #[test]
    fn test_lost_deletion_is_recorded_for_the_next_start() {
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("history.lxud");
        let io = FaultyIo::default();
        let hist = hist_with_io(&cp, io.boxed());
        block_checkpoint_write(&cp);

        lose_a_deletion(&hist, &io);

        assert!(hist.has_unpersisted_deletion(), "runtime row still holds");
        assert_eq!(
            marker(&cp),
            Some(DeletionBreach::Lost),
            "a deletion with no frame and no checkpoint must reach the disk as Lost"
        );
    }

    #[test]
    fn test_a_covering_checkpoint_retracts_the_marker() {
        // The fallback checkpoint (§5.4) succeeding *is* the deletion being
        // persisted, so nothing should be reported next start.
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("history.lxud");
        let io = FaultyIo::default();
        let hist = hist_with_io(&cp, io.boxed());

        lose_a_deletion(&hist, &io);

        assert!(cp.exists(), "the fallback checkpoint must have landed");
        assert_eq!(marker(&cp), None, "a covered deletion leaves no record");
        assert!(!hist.has_unpersisted_deletion());
    }

    #[test]
    fn test_a_failed_compaction_leaves_the_marker_standing() {
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("history.lxud");
        let io = FaultyIo::default();
        let hist = hist_with_io(&cp, io.boxed());
        block_checkpoint_write(&cp);

        lose_a_deletion(&hist, &io);
        assert_eq!(marker(&cp), Some(DeletionBreach::Lost));

        // Disk heals: the next compaction writes a checkpoint that no longer
        // contains the entry, which is what makes the deletion durable.
        io.fail_appends.store(false, Ordering::SeqCst);
        unblock_checkpoint_write(&cp);
        hist.scrub_pending.store(true, Ordering::SeqCst);
        hist.run_gated_compact();

        assert_eq!(
            marker(&cp),
            None,
            "a durable checkpoint retracts the record along with the ledger"
        );
    }

    #[test]
    fn test_a_durable_checkpoint_retracts_even_if_the_truncate_fails() {
        // Retraction is owed to the checkpoint, not to the WAL truncation that
        // follows it (AGENTS (d)). Gating on the truncation would leave a
        // standing warning whenever the truncate fails on an otherwise durable
        // write, or frames land mid-run.
        //
        // Uses the truncate-failure outcome rather than FollowUp: both reach
        // the same question, and this one is deterministic — FollowUp needs a
        // frame to land between a compaction's own snapshot and its truncate,
        // which no fixture can schedule.
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("history.lxud");
        let io = FaultyIo::default();
        let hist = hist_with_io(&cp, io.boxed());
        block_checkpoint_write(&cp);

        hist.apply_records(&[committed("きょう", "今日")]);
        io.fail_full_sync.store(true, Ordering::SeqCst);
        hist.apply_records(&[deletion("きょう", "今日")]);
        io.fail_full_sync.store(false, Ordering::SeqCst);
        assert!(matches!(
            marker(&cp),
            Some(DeletionBreach::Unflushed { .. })
        ));

        unblock_checkpoint_write(&cp);
        io.fail_truncates.store(true, Ordering::SeqCst);
        assert!(matches!(hist.run_compact(), CompactOutcome::Done));
        assert_eq!(
            marker(&cp),
            None,
            "a durable checkpoint persists the deletion; the truncation is only the scrub"
        );
    }

    #[test]
    fn test_lost_survives_a_later_unflushed_raise() {
        // The reachable route to an `Unflushed` landing on an outstanding
        // `Lost`, which is what the read-modify-write merge exists for. It is
        // not the obvious one: an `Io` append freezes the WAL, and the frozen
        // guard turns every later append into `Io` too. What lifts the freeze
        // is a compaction whose cover generation predates the raise — it
        // leaves the marker standing — after which the next tombstone can
        // append and fail its flush.
        //
        // An earlier version drove "both orders" through fail_appends /
        // fail_full_sync and claimed to cover this one; the freeze made that
        // iteration produce `Lost` twice, and a merge rewritten to let
        // `Unflushed` win stayed green.
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("history.lxud");
        let io = FaultyIo::default();
        let hist = hist_with_io(&cp, io.boxed());
        block_checkpoint_write(&cp);

        hist.apply_records(&[committed("きょう", "今日"), committed("あす", "明日")]);
        io.fail_appends.store(true, Ordering::SeqCst);
        hist.apply_records(&[deletion("きょう", "今日")]);
        assert_eq!(marker(&cp), Some(DeletionBreach::Lost));
        assert!(lock_recover(&hist.wal).is_frozen());

        // A cover carrying a generation older than the raise settles nothing,
        // and the truncation that follows it is what thaws the file.
        io.fail_appends.store(false, Ordering::SeqCst);
        {
            let wal = lock_recover(&hist.wal);
            hist.cover_unpersisted(&wal, 0);
        }
        lock_recover(&hist.wal).truncate_wal().unwrap();
        assert!(!lock_recover(&hist.wal).is_frozen());
        assert_eq!(
            marker(&cp),
            Some(DeletionBreach::Lost),
            "a stale cover must not settle the claim"
        );

        io.fail_full_sync.store(true, Ordering::SeqCst);
        hist.apply_records(&[deletion("あす", "明日")]);
        assert_eq!(
            marker(&cp),
            Some(DeletionBreach::Lost),
            "the unhealable claim must not be downgraded to a suppressible witness"
        );
    }

    #[test]
    fn test_a_failed_marker_write_does_not_drop_the_claim() {
        // The merge runs against the file, so a write that failed leaves
        // nothing to merge with, and the next weaker raise would start from an
        // empty read — on the failing disk where the write fails, which is the
        // only disk this path runs on. The session keeps its own claim.
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("history.lxud");
        let io = FaultyIo::default();
        let hist = hist_with_io(&cp, io.boxed());
        block_checkpoint_write(&cp);
        hist.apply_records(&[committed("きょう", "今日"), committed("あす", "明日")]);

        // A *non-empty* directory at the marker path fails that write and
        // nothing else. An empty one would not: the writer owns this path and
        // clears a placeholder out of its way. What it will not do is delete
        // someone else's contents.
        // At the *tmp*, not the canonical name: a directory at the canonical
        // name no longer fails the write, because `write_atomic` flushes the
        // tmp before renaming and `read` merges that orphan — the claim lands.
        // Blocking the tmp stops `create_regular` outright, which is what
        // "the write failed" now means.
        let marker_dir =
            crate::user_history::checkpoint_tmp_path(&deletion_marker::marker_path(&cp));
        std::fs::create_dir(&marker_dir).unwrap();
        std::fs::write(marker_dir.join("restored"), b"not ours").unwrap();
        io.fail_appends.store(true, Ordering::SeqCst);
        hist.apply_records(&[deletion("きょう", "今日")]);
        assert_eq!(
            marker(&cp),
            Some(DeletionBreach::Lost),
            "unreadable reads as Lost"
        );
        std::fs::remove_dir_all(&marker_dir).unwrap();
        // The rename failed, so the atomic write's tmp is still beside the
        // absent marker holding the claim — and `read` merges it, which is
        // what makes an orphan strengthen rather than hide. Clearing it here
        // isolates what this test is about: the claim surviving in *memory*.
        assert_eq!(
            marker(&cp),
            None,
            "and now there is genuinely nothing on disk"
        );

        // The disk accepts the marker again, and a weaker breach arrives.
        io.fail_appends.store(false, Ordering::SeqCst);
        lock_recover(&hist.wal).truncate_wal().unwrap();
        io.fail_full_sync.store(true, Ordering::SeqCst);
        hist.apply_records(&[deletion("あす", "明日")]);

        assert_eq!(
            marker(&cp),
            Some(DeletionBreach::Lost),
            "the claim whose write failed must be re-asserted, not forgotten"
        );
    }

    #[test]
    fn test_unflushed_raises_keep_the_higher_seq() {
        // The suppression test asks whether the loaded state reached the
        // witness, so the lower of two seqs can be covered while the higher is
        // still missing. Keeping the minimum would suppress a real loss.
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("history.lxud");
        let io = FaultyIo::default();
        let hist = hist_with_io(&cp, io.boxed());
        block_checkpoint_write(&cp);

        hist.apply_records(&[committed("きょう", "今日"), committed("あす", "明日")]);
        io.fail_full_sync.store(true, Ordering::SeqCst);
        hist.apply_records(&[deletion("きょう", "今日")]);
        let first = match marker(&cp) {
            Some(DeletionBreach::Unflushed { seq }) => seq,
            other => panic!("expected an unflushed witness, got {other:?}"),
        };
        hist.apply_records(&[deletion("あす", "明日")]);

        match marker(&cp) {
            Some(DeletionBreach::Unflushed { seq }) => {
                assert!(
                    seq > first,
                    "the later frame's seq must win ({seq} > {first})"
                );
                // Joined to the seq the WAL actually assigned, not just to the
                // other witness. Without this, recording `seq + 1` or `seq - 1`
                // passes every test: one latches a false privacy alarm, the
                // other silently suppresses a genuine loss, and a comparison
                // between two witnesses sees neither.
                assert_eq!(
                    seq,
                    lock_recover(&hist.wal).last_appended_seq(),
                    "the witness must be the frame's own seq"
                );
            }
            other => panic!("expected an unflushed witness, got {other:?}"),
        }
    }

    #[test]
    fn test_a_replayed_unflushed_deletion_is_a_live_problem_until_checkpointed() {
        // The cross-crate half of "startup never retracts". Recovery hands the
        // claim over instead of unlinking, and this is where it becomes a live
        // durability problem: replay applied the deletion out of the page
        // cache, so until a checkpoint covers it, power loss still undoes it.
        // Without the hand-off the row is silent and the marker gets settled by
        // whatever the next compaction happens to do.
        //
        // Built against real files rather than `hist_with_io`: that fixture
        // mocks every WAL write, so a tombstone frame never reaches the disk
        // and no reopen can replay one.
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("history.lxud");

        let mut history = UserHistory::new();
        history.record_at(
            &[("きょう".to_string(), "今日".to_string())],
            crate::user_history::now_epoch(),
        );
        history.save(&cp).unwrap();

        // A real tombstone frame, as a SyncFailed append leaves it: on disk,
        // its flush unconfirmed.
        let seq = {
            let mut wal = HistoryWal::new(&cp);
            wal.append_record(&WalRecord::Tombstone {
                segments: vec![("きょう".to_string(), "今日".to_string())],
                timestamp: crate::user_history::now_epoch(),
            })
            .unwrap()
        };
        // Written before the open, so it reaches the runtime the production
        // way — through recovery, which reports what it read.
        deletion_marker::merge_write(&cp, DeletionBreach::Unflushed { seq }).unwrap();

        let reopened = open_hist(&cp);
        assert!(
            learned(&reopened, "きょう").is_empty(),
            "replay must have applied the deletion"
        );
        assert!(
            !reopened.open_report().deletion_lost,
            "so nothing is owed to the user as a past loss"
        );
        assert!(
            reopened
                .durability_issues()
                .contains(&LexHistoryDurabilityIssue::DeletionNotPersisted),
            "but it is not durable yet, and the runtime row is what says so"
        );
        assert!(
            marker(&cp).is_some(),
            "the record stands until a checkpoint covers it"
        );

        reopened.scrub_pending.store(true, Ordering::SeqCst);
        reopened.run_gated_compact();
        assert!(reopened.durability_issues().is_empty());
        assert_eq!(
            marker(&cp),
            None,
            "a durable checkpoint is what retracts it — the only thing that can"
        );
    }

    #[test]
    fn test_a_projection_that_did_not_flush_is_retried() {
        // The skip is keyed on having *flushed* the value, not on the file's
        // bytes matching. Matching bytes prove the content reached the page
        // cache, so keying on them would read a failed `sync_all` back as
        // up-to-date and never retry it — silently reopening the power-loss
        // window `merge_write` refuses to open for a 0.3ms saving.
        //
        // Modelled by a write that fails outright: the bytes are absent, the
        // claim is retained, and the next projection must try again rather
        // than conclude anything from the previous attempt.
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("history.lxud");
        let io = FaultyIo::default();
        let hist = hist_with_io(&cp, io.boxed());
        block_checkpoint_write(&cp);
        hist.apply_records(&[committed("きょう", "今日")]);

        // At the *tmp*, not the canonical name: a directory at the canonical
        // name no longer fails the write, because `write_atomic` flushes the
        // tmp before renaming and `read` merges that orphan — the claim lands.
        // Blocking the tmp stops `create_regular` outright, which is what
        // "the write failed" now means.
        let marker_dir =
            crate::user_history::checkpoint_tmp_path(&deletion_marker::marker_path(&cp));
        std::fs::create_dir(&marker_dir).unwrap();
        std::fs::write(marker_dir.join("restored"), b"not ours").unwrap();
        io.fail_appends.store(true, Ordering::SeqCst);
        hist.apply_records(&[deletion("きょう", "今日")]);
        // `Unknown`, not `Absent`: nothing may be remembered as flushed, and
        // "the path is clear" is itself a claim this process cannot make after
        // a write that failed — the same write could have left a truncated
        // orphan behind, and only `Unknown` refuses to satisfy anything.
        assert_eq!(lock_recover(&hist.claims).flushed, MarkerState::Unknown);

        std::fs::remove_dir_all(&marker_dir).unwrap();
        hist.apply_records(&[committed("あした", "明日")]);
        assert_eq!(
            marker(&cp),
            Some(DeletionBreach::Lost),
            "the retry must happen because the value was never flushed"
        );
        assert_eq!(
            lock_recover(&hist.claims).flushed,
            MarkerState::Holds(DeletionBreach::Lost),
            "and only now is it remembered as flushed"
        );
    }

    #[test]
    fn test_a_later_memory_only_raise_re_asserts_a_failed_claim() {
        // A `Lost` write that failed leaves the claim in memory, and the next
        // raise re-asserts it — including a memory-only one, which carries no
        // breach of its own. The projection is of the *claims*, not of the
        // breach this particular raise happened to bring, so there is no site
        // that can forget to retry.
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("history.lxud");
        let io = FaultyIo::default();
        let hist = hist_with_io(&cp, io.boxed());
        block_checkpoint_write(&cp);
        hist.apply_records(&[committed("きょう", "今日")]);

        // The marker write fails: a non-empty directory is the one shape the
        // writer will not clear out of its way.
        // At the *tmp*, not the canonical name: a directory at the canonical
        // name no longer fails the write, because `write_atomic` flushes the
        // tmp before renaming and `read` merges that orphan — the claim lands.
        // Blocking the tmp stops `create_regular` outright, which is what
        // "the write failed" now means.
        let marker_dir =
            crate::user_history::checkpoint_tmp_path(&deletion_marker::marker_path(&cp));
        std::fs::create_dir(&marker_dir).unwrap();
        std::fs::write(marker_dir.join("restored"), b"not ours").unwrap();
        io.fail_appends.store(true, Ordering::SeqCst);
        hist.apply_records(&[deletion("きょう", "今日")]);
        std::fs::remove_dir_all(&marker_dir).unwrap();
        // Same as above: the failed rename leaves the atomic write's tmp, and
        // `read` merges it. Clear it so the assertion is about memory.
        assert_eq!(marker(&cp), None, "nothing reached the disk");

        // A later *commit* against the frozen WAL — no deletion, so no breach.
        hist.apply_records(&[committed("あした", "明日")]);

        assert_eq!(
            marker(&cp),
            Some(DeletionBreach::Lost),
            "the retained claim must be re-asserted on any raise, not only on another deletion"
        );
    }

    #[test]
    fn test_an_ack_whose_removal_fails_keeps_the_report_owed() {
        // The acknowledgement clears the flag only when the record is really
        // gone. A path the engine cannot clear — here a directory something
        // else filled — used to be settled anyway, so the row disappeared
        // while the marker stood and the warning came back next launch with
        // the retry gone.
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("history.lxud");
        let io = FaultyIo::default();
        let hist = hist_with_io_reporting(&cp, io.boxed(), true);
        // Replace the marker the helper left with something the engine cannot
        // clear. Reachable: recovery reads a non-regular path conservatively as
        // `Lost` and leaves it, so the report is owed against a record no
        // unlink will retire.
        let marker_path = deletion_marker::marker_path(&cp);
        std::fs::remove_file(&marker_path).unwrap();
        std::fs::create_dir(&marker_path).unwrap();
        std::fs::write(marker_path.join("restored"), b"not ours").unwrap();

        hist.ack_open_report();

        assert!(
            hist.deletion_report_owed(),
            "an acknowledgement that could not clear the record still owes it"
        );
        assert!(
            marker_path.join("restored").exists(),
            "and it did not delete what it does not own"
        );
    }

    #[test]
    fn test_a_cover_leaves_an_inherited_report_that_was_never_delivered() {
        // The third authority error, and the narrowest reopening of #312. A
        // session that inherits a report, raises a breach of its own, and then
        // heals gets a cover — but a checkpoint written here persists the
        // *resurrected* entry rather than removing it, so it settles nothing
        // about the inherited claim. The two share one file, so unlinking on
        // the ledger alone destroys a report the user never saw.
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("history.lxud");
        let io = FaultyIo::default();
        let hist = hist_with_io_reporting(&cp, io.boxed(), true);
        plant_marker(&hist, &cp, DeletionBreach::Lost);
        block_checkpoint_write(&cp);

        // This session loses a deletion of its own, then the disk recovers.
        hist.apply_records(&[committed("きょう", "今日")]);
        io.fail_appends.store(true, Ordering::SeqCst);
        hist.apply_records(&[deletion("きょう", "今日")]);
        assert!(hist.has_unpersisted_deletion());
        io.fail_appends.store(false, Ordering::SeqCst);
        unblock_checkpoint_write(&cp);
        hist.scrub_pending.store(true, Ordering::SeqCst);
        hist.run_gated_compact();

        assert!(
            !hist.has_unpersisted_deletion(),
            "this session's breach is genuinely covered"
        );
        assert_eq!(
            marker(&cp),
            Some(DeletionBreach::Lost),
            "but the undelivered report from the previous session is not"
        );

        // Delivery is what settles it, and then a later cover may reclaim it.
        hist.ack_open_report();
        assert!(
            !hist.deletion_report_owed(),
            "a clean ack retires the report"
        );
        assert_eq!(marker(&cp), None);
    }

    #[test]
    fn test_ack_leaves_a_marker_this_session_raised() {
        // `report.deletion_lost` is frozen at open, so acknowledging on it
        // alone deletes whatever marker is on disk *now* — including one a
        // raise wrote minutes later, whose breach is still outstanding. That
        // session's own report would then be the thing that goes missing.
        //
        // Reachable as soon as the ack moves to the row's click handler, which
        // is exactly where it had to move so probe launches — and menu builds
        // nobody sees — stop consuming reports.
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("history.lxud");
        let io = FaultyIo::default();
        let hist = hist_with_io_reporting(&cp, io.boxed(), true);
        block_checkpoint_write(&cp);

        hist.apply_records(&[committed("きょう", "今日")]);
        io.fail_appends.store(true, Ordering::SeqCst);
        hist.apply_records(&[deletion("きょう", "今日")]);
        assert_eq!(marker(&cp), Some(DeletionBreach::Lost));
        assert!(hist.has_unpersisted_deletion());

        hist.ack_open_report();
        assert!(
            hist.deletion_report_owed(),
            "an ack that retires nothing leaves the report owed, or the caller drops the row that is the only way to retry it"
        );
        assert_eq!(
            marker(&cp),
            Some(DeletionBreach::Lost),
            "an outstanding breach of this session's own must survive the ack"
        );
    }

    #[test]
    fn test_memory_only_learning_writes_no_marker() {
        // The marker is a claim about a *deletion*. A Committed append that
        // never reached the WAL is memory-only learning — reportable while it
        // lasts, but not something the user asked to erase. Writing a marker
        // for it would latch 「前回のセッションの削除が保存されていません」 at the
        // next launch for a deletion nobody requested, and the ledger
        // assertions alone cannot see that.
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("history.lxud");
        let io = FaultyIo::default();
        io.fail_appends.store(true, Ordering::SeqCst);
        let hist = hist_with_io(&cp, io.boxed());

        hist.apply_records(&[committed("きょう", "今日")]);

        assert_eq!(
            hist.durability_issues(),
            vec![LexHistoryDurabilityIssue::LearningMemoryOnly]
        );
        assert_eq!(marker(&cp), None, "learning is not a deletion");
    }

    #[test]
    fn test_a_replayed_witness_is_not_unlinked_before_a_checkpoint_covers_it() {
        // The ledger and the claims have to agree about what is outstanding.
        // `deletion_pending_checkpoint` raises the ledger for a deletion replay
        // applied but no checkpoint covers; with `session` left empty the
        // projection computed a desired state of `None`, and once every commit
        // reconciles, the first ordinary commit unlinked the witness before
        // anything had persisted the deletion — a power loss would then restore
        // the entry with no record left to report it.
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("history.lxud");
        let seq = {
            let mut wal = HistoryWal::new(&cp);
            wal.append_record(&WalRecord::Tombstone {
                segments: vec![("きょう".to_string(), "今日".to_string())],
                timestamp: crate::user_history::now_epoch(),
            })
            .unwrap()
        };
        deletion_marker::merge_write(&cp, DeletionBreach::Unflushed { seq }).unwrap();

        // Before the open, not after: `open` schedules a startup compaction for
        // the replayed deletion, and that compaction *is* a covering
        // checkpoint. Blocking afterwards left the two racing — the test
        // passed locally and failed in CI.
        block_checkpoint_write(&cp);
        let hist = open_hist(&cp);
        assert!(
            hist.has_unpersisted_deletion(),
            "the replayed witness is a live durability problem"
        );
        hist.apply_records(&[committed("あした", "明日")]);

        assert_eq!(
            marker(&cp),
            Some(DeletionBreach::Unflushed { seq }),
            "only a covering checkpoint may retire it, not an unrelated commit"
        );
    }

    #[test]
    fn test_a_stronger_claim_on_disk_satisfies_a_weaker_one() {
        // Without this the projection livelocks. A stale `Lost` survives a
        // failed cleanup, a later `SyncFailed` deletion makes the desired
        // state `Unflushed`, and `merge_write` absorbs that request straight
        // back into `Lost` — so under exact equality the desired state is
        // unreachable and *every* commit pays another key-thread full sync,
        // forever, while the restart still reports the loss.
        //
        // Claims are one-directional, so a stronger record covers a weaker
        // want: it reports more, never less, which is the only direction this
        // format may fail in.
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("history.lxud");
        let hist = hist_with_io(&cp, FaultyIo::default().boxed());
        plant_marker(&hist, &cp, DeletionBreach::Lost);

        // Blocked, which is what makes the difference observable: under exact
        // equality the projection would try to write and fail, so a `true`
        // here means it recognised the disk as already sufficient and issued
        // no syscall at all. Asserting on the resulting state cannot see this
        // — both paths end at `Holds(Lost)`, since the write merges back.
        block_marker_write(&cp);
        {
            let wal = lock_recover(&hist.wal);
            assert!(
                hist.apply_marker(&wal, Some(DeletionBreach::Unflushed { seq: 7 })),
                "the disk already says more than this asks for, so nothing is written"
            );
        }
        assert_eq!(
            lock_recover(&hist.claims).flushed,
            MarkerState::Holds(DeletionBreach::Lost),
            "and the belief is unchanged — nothing needed writing"
        );

        // The converse must not hold: a weaker record does not cover a stronger
        // want, which is what keeps a failed promotion retrying rather than
        // deciding a witness is good enough for an unconditional claim.
        assert!(
            !MarkerState::Holds(DeletionBreach::Unflushed { seq: 7 })
                .satisfies(Some(DeletionBreach::Lost)),
            "a witness must never be taken to cover an unconditional claim"
        );
    }

    #[test]
    fn test_the_belief_records_what_the_write_actually_persisted() {
        // `merge_write` merges, so a request is not what lands. With a
        // surviving `Lost` on disk and a later `SyncFailed` deletion asking for
        // `Unflushed`, the write correctly keeps the stronger `Lost` — and a
        // caller that recorded its own request would hold a belief the disk
        // never had. The next reconcile would then find the desired witness
        // "already satisfied", skip, and leave the stronger claim standing for
        // the next startup to report against a deletion replay had applied.
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("history.lxud");
        let hist = hist_with_io(&cp, FaultyIo::default().boxed());
        plant_marker(&hist, &cp, DeletionBreach::Lost);

        {
            let wal = lock_recover(&hist.wal);
            hist.apply_marker(&wal, Some(DeletionBreach::Unflushed { seq: 7 }));
        }

        assert_eq!(
            marker(&cp),
            Some(DeletionBreach::Lost),
            "the merge keeps the stronger claim on disk"
        );
        assert_eq!(
            lock_recover(&hist.claims).flushed,
            MarkerState::Holds(DeletionBreach::Lost),
            "and the belief must say so, not repeat the request"
        );
    }

    #[test]
    fn test_an_unknown_marker_is_not_mistaken_for_an_absent_one() {
        // The residue of a startup that refuted an *unreadable* marker and
        // could not unlink it. Recording that as absence made the projection
        // find `Absent == None`, skip, and leave the file standing — so once
        // the user learned anything, the survivor produced a false
        // lost-deletion report on the next start. `Unknown` never satisfies
        // any desired state, so the removal is retried instead.
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("history.lxud");
        let hist = hist_with_io(&cp, FaultyIo::default().boxed());
        deletion_marker::merge_write(&cp, DeletionBreach::Lost).unwrap();
        lock_recover(&hist.claims).flushed = MarkerState::Unknown;
        assert!(!hist.deletion_report_owed(), "nothing is owed");

        hist.apply_records(&[committed("きょう", "今日")]);

        assert_eq!(
            marker(&cp),
            None,
            "a file nobody could read is not a clear path; the removal must be retried"
        );
        assert_eq!(
            lock_recover(&hist.claims).flushed,
            MarkerState::Absent,
            "and only a removal that succeeded may record absence"
        );
    }

    #[test]
    fn test_an_unknown_marker_that_will_not_yield_freezes_appends() {
        // A marker file nobody can read may hold an `Unflushed{seq}` naming a
        // number no floor could protect — recovery never saw the seq. While
        // the write that would replace it with an unconditional `Lost` keeps
        // failing, issuing more sequence numbers risks handing out that one,
        // and a later startup would read it as evidence the deletion replayed.
        //
        // So this batch goes memory-only instead: reported, and healed by the
        // compaction that rewrites both files. Not the broader "freeze on any
        // failed marker write" — a disk already saying `Lost` cannot be
        // satisfied by any sequence, and a claim this session raised is about
        // the file it is still appending to.
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("history.lxud");
        let hist = hist_with_io(&cp, FaultyIo::default().boxed());
        lock_recover(&hist.claims).flushed = MarkerState::Unknown;
        // Inherited, which is the whole point: a witness from a *previous* WAL
        // lineage is the one nothing can answer. This session's own claim is
        // about the file it is still appending to.
        hist.inherited_owed.store(true, Ordering::SeqCst);
        // Nothing can be written into a directory that will not take one.
        block_marker_write(&cp);

        hist.apply_records(&[committed("きょう", "今日")]);

        assert!(
            hist.durability_issues()
                .contains(&LexHistoryDurabilityIssue::LearningMemoryOnly),
            "the batch must be memory-only rather than consuming sequence numbers"
        );
    }

    #[test]
    fn test_an_ordinary_commit_reconciles_the_marker_both_ways() {
        // Where the projection's correctness actually lives, after six review
        // rounds spent adding one retry trigger at a time (the ack, the wipe,
        // the compaction's cover, recovery's removal, recovery's promotion,
        // and the compaction recovery schedules). None of those is needed for
        // *correctness* any more: whatever left the disk disagreeing, the next
        // ordinary commit fixes it, because a projection that only reconciles
        // at chosen events is a cache with invalidation rather than a
        // projection.
        //
        // No compaction anywhere in this test — one record is nowhere near the
        // threshold — which is the whole point.
        //
        // What this cannot pin is the *placement*: reconciling after the
        // appends instead of before passes every test here, because the only
        // difference is a crash landing between the append and the reconcile,
        // one batch wide. Confirmed undetectable by measurement, so the
        // ordering is carried by the comment at the call site and by the
        // argument for it — a WAL that advances past an un-promoted witness is
        // exactly the harm being closed.
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("history.lxud");
        let hist = hist_with_io(&cp, FaultyIo::default().boxed());

        // Direction 1: the disk holds a record nothing claims — an unlink that
        // failed, wherever it failed.
        plant_marker(&hist, &cp, DeletionBreach::Lost);
        assert!(!hist.deletion_report_owed(), "nothing is owed");
        hist.apply_records(&[committed("きょう", "今日")]);
        assert_eq!(
            marker(&cp),
            None,
            "a commit must retire a record no claim stands behind"
        );

        // Direction 2: a claim is owed and the disk does not say so — the
        // startup promotion that could not write. Left unreconciled, ordinary
        // commits advance the WAL past the witness and the next start reads it
        // as replayed.
        let owed = hist_with_io_reporting(&cp, FaultyIo::default().boxed(), true);
        std::fs::remove_file(deletion_marker::marker_path(&cp)).unwrap();
        lock_recover(&owed.claims).flushed = MarkerState::Absent;
        owed.apply_records(&[committed("あした", "明日")]);
        assert_eq!(
            marker(&cp),
            Some(DeletionBreach::Lost),
            "a commit must re-assert a claim the disk is missing"
        );
    }

    #[test]
    fn test_a_checkpoint_retries_a_marker_removal_that_failed_earlier() {
        // The residue of an unlink that was refused between a successful save
        // and the removal that should have followed: the claim is settled, so
        // nothing raises it again, and every later cover early-returns off the
        // covered ledger. Without an unconditional projection here, the stale
        // file has nothing left to revisit it, and the next launch reports a
        // previous-session loss against a deletion this very checkpoint made
        // durable — a warning telling the user to go delete an entry that is
        // already gone.
        //
        // The fixture is that residue exactly, and it is the state that tells
        // it apart from an inherited claim (the test above): a file on disk
        // with no claim behind it and nothing owed.
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("history.lxud");
        let hist = hist_with_io(&cp, FaultyIo::default().boxed());
        plant_marker(&hist, &cp, DeletionBreach::Lost);
        assert!(!hist.deletion_report_owed(), "nothing is owed to the user");
        assert!(!hist.has_unpersisted_deletion(), "and no claim is standing");

        hist.apply_records(&[committed("きょう", "今日")]);
        assert!(
            matches!(hist.run_compact(), CompactOutcome::Done),
            "fixture needs a durable save"
        );

        assert_eq!(
            marker(&cp),
            None,
            "a checkpoint must retry a removal nothing else would"
        );
    }

    #[test]
    fn test_a_cover_for_memory_only_learning_leaves_an_inherited_marker() {
        // The ledger shares one generation sequence, so a memory-only raise
        // moves it without raising `raised_deletion`. Covering that must not
        // unlink a marker this session never claimed: an inherited report is
        // owed to the user until it is shown, and nothing here settles the
        // deletion it describes.
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("history.lxud");
        let io = FaultyIo::default();
        io.fail_appends.store(true, Ordering::SeqCst);
        let hist = hist_with_io(&cp, io.boxed());
        // Both halves, because recovery only ever produces them together: the
        // file, and the in-memory record that it is owed to the user. Writing
        // the file alone would be a state production cannot reach — the
        // session would hold no claim at all — and the projection would then
        // be right to unlink it.
        plant_marker(&hist, &cp, DeletionBreach::Lost);
        hist.inherited_owed.store(true, Ordering::SeqCst);

        hist.apply_records(&[committed("きょう", "今日")]);
        let generation = gen_under_wal(&hist);
        assert!(generation > 0, "the memory-only raise must move the ledger");
        cover_under_wal(&hist, generation);

        assert_eq!(
            marker(&cp),
            Some(DeletionBreach::Lost),
            "covering learning must not retract a deletion report"
        );
    }

    #[test]
    fn test_clear_removes_a_marker_it_did_not_raise() {
        // A marker from a *previous* session raises nothing in this one, so
        // the ledger is zero and the cover early-returns without touching it.
        // Only the unconditional wipe in clear_impl removes it — and it must,
        // or a full wipe would keep reporting a lost deletion against a
        // history that provably holds nothing.
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("history.lxud");
        let hist = open_hist(&cp);
        plant_marker(&hist, &cp, DeletionBreach::Lost);

        hist.clear_impl().unwrap();

        assert_eq!(marker(&cp), None, "a wipe must retire a stale marker too");
    }

    #[test]
    fn test_clear_removes_a_marker_it_did_raise() {
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("history.lxud");
        let io = FaultyIo::default();
        let hist = hist_with_io(&cp, io.boxed());
        block_checkpoint_write(&cp);
        lose_a_deletion(&hist, &io);
        assert_eq!(marker(&cp), Some(DeletionBreach::Lost));

        unblock_checkpoint_write(&cp);
        io.fail_appends.store(false, Ordering::SeqCst);
        hist.clear_impl().unwrap();

        assert_eq!(marker(&cp), None);
    }

    #[test]
    fn test_a_lost_deletion_survives_the_restart_that_resurrects_it() {
        // The end-to-end shape of #312, and the only test that exercises the
        // writer and the reader against one real file: delete, lose it, close
        // the process, reopen. The entry comes back — and this time the
        // report comes back with it.
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("history.lxud");

        {
            let io = FaultyIo::default();
            let hist = hist_with_io(&cp, io.boxed());
            // A real checkpoint first, so the reopen below has something to
            // load that still holds the entry.
            hist.apply_records(&[committed("きょう", "今日")]);
            hist.scrub_pending.store(true, Ordering::SeqCst);
            hist.run_gated_compact();
            assert!(cp.exists());

            block_checkpoint_write(&cp);
            io.fail_appends.store(true, Ordering::SeqCst);
            hist.apply_records(&[deletion("きょう", "今日")]);
            assert!(learned(&hist, "きょう").is_empty(), "memory delete runs");
            assert!(hist.has_unpersisted_deletion());
        }
        unblock_checkpoint_write(&cp);

        let reopened = open_hist(&cp);
        assert_eq!(
            learned(&reopened, "きょう"),
            vec!["今日".to_string()],
            "the deletion did not persist, so the entry is back"
        );
        assert!(
            reopened.open_report().deletion_lost,
            "and the report is back with it — this is the whole of #312"
        );
        // No disk recovery retracts it — only a user action does — so it must
        // not be on the channel that retracts itself.
        assert!(
            !reopened
                .durability_issues()
                .contains(&LexHistoryDurabilityIssue::DeletionNotPersisted),
            "a past loss is not a live durability problem"
        );

        // "It appears" is half the property; "it goes away once delivered" is
        // the other half. Without the second, a permanently latched row would
        // pass the first.
        assert_eq!(marker(&cp), Some(DeletionBreach::Lost));
        reopened.ack_open_report();
        assert!(
            !reopened.deletion_report_owed(),
            "a clean ack retires the report"
        );
        assert_eq!(marker(&cp), None);
        assert!(!open_hist(&cp).open_report().deletion_lost);
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
    // the user. These pin the raise/cover ledger.
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
        // A refused commit is what makes learning memory-only; a refused
        // deletion is the other row. Both are needed to see both rows — a
        // deletion alone must NOT claim learning was lost.
        hist.apply_records(&[deletion("きょう", "今日")]);
        assert_eq!(
            hist.durability_issues(),
            vec![LexHistoryDurabilityIssue::DeletionNotPersisted],
            "a refused deletion is not a claim that learning was lost"
        );
        hist.apply_records(&[committed("あす", "明日")]);

        assert_eq!(
            hist.durability_issues(),
            vec![
                // The deletion first: it is what the user explicitly asked
                // for, and unlike memory-only learning nothing heals it on
                // restart.
                LexHistoryDurabilityIssue::DeletionNotPersisted,
                LexHistoryDurabilityIssue::LearningMemoryOnly,
            ]
        );
    }

    #[test]
    fn test_polling_under_concurrent_raises_never_drops_the_deletion_row() {
        // The same defect under real interleaving. The detector has to avoid
        // the very race it is testing: comparing the poll's result against a
        // predicate read *after* the poll returned would flag a raise that
        // landed in between, which is legitimate.
        //
        // So the test first drives one failing deletion to completion and
        // only then arms the assertion. Past that point both conditions hold
        // permanently — the checkpoint is blocked so nothing can cover, and
        // appends stay failing so nothing can thaw — and any poll returning
        // anything other than both rows has torn the pair.
        //
        // Disclosure, measured: this does NOT reliably catch the defect it
        // describes. Reverting `durability_issues` to two independent loads
        // leaves it green across repeated runs, because the window is three
        // atomic loads wide and a raise has to land inside it. What it does
        // pin is that the retry loop terminates and stays correct under
        // concurrent raises. The tear itself is closed by construction — the
        // same standing as the ledger's own packed counters.
        let dir = tempfile::tempdir().unwrap();
        let cp = blocked_checkpoint(dir.path());
        let io = FaultyIo::default();
        let hist = hist_with_io(&cp, io.boxed());
        hist.apply_records(&[committed("きょう", "今日")]);

        io.fail_appends.store(true, Ordering::SeqCst);
        // Both rows: the refused deletion raises one, the refused commit the
        // other. Neither can be covered (the checkpoint is blocked) nor
        // thawed (appends keep failing), so past here both hold permanently.
        hist.apply_records(&[deletion("きょう", "今日")]);
        hist.apply_records(&[committed("あす", "明日")]);
        let expected = vec![
            LexHistoryDurabilityIssue::DeletionNotPersisted,
            LexHistoryDurabilityIssue::LearningMemoryOnly,
        ];
        assert_eq!(hist.durability_issues(), expected, "armed state");

        let stop = Arc::new(AtomicBool::new(false));
        let torn = Arc::new(AtomicBool::new(false));
        let pollers: Vec<_> = (0..3)
            .map(|_| {
                let hist = Arc::clone(&hist);
                let stop = Arc::clone(&stop);
                let torn = Arc::clone(&torn);
                let expected = expected.clone();
                std::thread::spawn(move || {
                    while !stop.load(Ordering::SeqCst) {
                        if hist.durability_issues() != expected {
                            torn.store(true, Ordering::SeqCst);
                        }
                    }
                })
            })
            .collect();

        // Keep raising: the residue holds the key, so each deletion still
        // writes a tombstone, fails to append, and bumps the generation.
        for _ in 0..500 {
            hist.apply_records(&[deletion("きょう", "今日")]);
        }
        stop.store(true, Ordering::SeqCst);
        for p in pollers {
            p.join().unwrap();
        }

        assert!(
            !torn.load(Ordering::SeqCst),
            "a poll dropped a row while both conditions permanently held"
        );
    }

    #[test]
    fn test_a_stale_compaction_must_not_unfreeze_over_memory_only_commits() {
        // Codex R2 (P1). A commit that hits the frozen guard never reaches
        // `append_record`'s seq assignment, so `last_appended_seq` does not
        // move — and `truncate_covered` unfreezes on
        // `last_appended_seq <= applied_seq`. An in-flight compaction whose
        // snapshot predates that commit therefore clears the freeze while the
        // commit is in neither the checkpoint nor the WAL.
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("history.lxud");
        let io = FaultyIo::default();
        let hist = hist_with_io(&cp, io.boxed());

        // Hold the gate so the compactions apply_records posts cannot run and
        // cover things behind the assertions; this stands in for the single
        // in-flight compaction the scenario describes.
        let gate = hist.lock_gate();

        // A first failure freezes the WAL, its effect memory-only.
        io.fail_appends.store(true, Ordering::SeqCst);
        hist.apply_records(&[committed("きょう", "今日")]);
        assert!(lock_recover(&hist.wal).is_frozen());

        // The heal compaction snapshots here...
        let (_gen, snapshot) = hist.snapshot_to_cover();

        // ...and while its I/O is in flight, another commit hits the frozen
        // guard and lands in memory only.
        hist.apply_records(&[committed("あす", "明日")]);

        // The in-flight compaction completes against its older snapshot.
        snapshot.save(&cp).unwrap();
        let truncated = lock_recover(&hist.wal)
            .truncate_covered(snapshot.applied_seq())
            .unwrap();
        assert!(
            truncated,
            "the covered-only guard does not see the memory-only commit"
        );

        assert!(
            !lock_recover(&hist.wal).is_frozen(),
            "the stale compaction cleared the freeze"
        );
        assert!(
            hist.durability_issues()
                .contains(&LexHistoryDurabilityIssue::LearningMemoryOnly),
            "あす is in neither the checkpoint nor the WAL, so learning IS memory-only"
        );
        drop(gate);
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
        raise_under_wal(&hist); // lands after the pair was taken
        cover_under_wal(&hist, generation);

        assert!(
            hist.has_unpersisted_deletion(),
            "a checkpoint cannot vouch for a deletion taken after its snapshot"
        );
        // The next compaction, pairing a fresh generation, does clear it.
        let (generation, _snapshot) = hist.snapshot_to_cover();
        cover_under_wal(&hist, generation);
        assert!(!hist.has_unpersisted_deletion());
    }

    #[test]
    fn test_a_cover_cannot_swallow_a_raise_that_races_it() {
        // The packed ledger's reason for existing: raised and covered move
        // independently, and the outstanding test must see both from one
        // instant. As two atomics that needed two loads in exactly one order,
        // held there by a comment — a cover plus a fresh raise landing
        // between them reported clean while a deletion was memory-only.
        //
        // Packed, the read is a single load and that skew is unrepresentable,
        // so what is left to pin is the write side: a cover must preserve a
        // raise that lands while it is in flight, and must never walk back.
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("history.lxud");
        let hist = open_hist(&cp);

        raise_under_wal(&hist);
        let observed = gen_under_wal(&hist);
        // A second deletion fails while the first cover is being computed.
        raise_under_wal(&hist);
        cover_under_wal(&hist, observed);
        assert!(
            hist.has_unpersisted_deletion(),
            "covering generation 1 must not settle the deletion raised after it"
        );
        // And the marker must survive with it. This is the only place the
        // `raised <= covered` half of the unlink condition is exercised
        // against a genuinely stale cover: elsewhere the early return fires
        // first, so the condition is never evaluated at all.
        assert_eq!(
            marker(&cp),
            Some(DeletionBreach::Lost),
            "a stale cover must not retract a still-outstanding claim"
        );

        // A stale cover must not un-settle newer work.
        cover_under_wal(&hist, gen_under_wal(&hist));
        assert!(!hist.has_unpersisted_deletion());
        cover_under_wal(&hist, observed);
        assert!(
            !hist.has_unpersisted_deletion(),
            "a late cover carrying an older generation must not reopen it"
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
    fn test_clear_truncate_failure_defers_the_row_until_learning_is_at_risk() {
        // F13. clear freezes the WAL when it cannot truncate it. That does
        // NOT by itself mean learning is memory-only — the wipe emptied both
        // memory and the checkpoint, so at that instant nothing is at risk;
        // what the freeze means is that the *next* commit will fail. The row
        // now tracks the fact rather than the freeze, so it appears then.
        //
        // The incomplete wipe is a different fact with a different sink: the
        // Err that clear_impl returns.
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("history.lxud");
        let io = FaultyIo::default();
        let hist = hist_with_io(&cp, io.boxed());

        hist.apply_records(&[committed("きょう", "今日")]);
        io.fail_truncates.store(true, Ordering::SeqCst);
        assert!(hist.clear_impl().is_err(), "the partial wipe is surfaced");
        assert!(lock_recover(&hist.wal).is_frozen());
        assert!(
            hist.durability_issues().is_empty(),
            "an empty history has nothing memory-only, however frozen the WAL"
        );

        // The first commit after the freeze is the one that is memory-only.
        // Hold the gate so the heal compaction cannot cover it before the
        // assertion runs.
        let gate = hist.lock_gate();
        hist.apply_records(&[committed("あす", "明日")]);
        assert_eq!(
            hist.durability_issues(),
            vec![LexHistoryDurabilityIssue::LearningMemoryOnly],
            "the commit the freeze refused is memory-only, and is reported"
        );
        drop(gate);
    }

    #[test]
    fn test_a_freeze_inherited_at_open_reports_once_a_commit_is_at_risk() {
        // F12. Recovery can hand back an already-frozen WAL (here: a failed
        // WAL repair). Nothing has been learned yet, so nothing is
        // memory-only — reporting 「新しい学習内容を保存できていません」 there
        // would be false. The startup compaction may well heal the freeze
        // before the user types at all.
        //
        // What must be reported is the first commit the freeze refuses, and
        // it is. `OpenReport::appends_frozen` separately records the freeze
        // itself for the startup log, which is the fact it names.
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("history.lxud");
        let wal_path = cp.with_extension("lxud.wal");
        std::fs::write(&wal_path, b"\0\0\0").unwrap();
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
        assert!(
            hist.durability_issues().is_empty(),
            "a freeze with nothing learned yet puts no learning at risk"
        );

        let gate = hist.lock_gate();
        hist.apply_records(&[committed("きょう", "今日")]);
        assert_eq!(
            hist.durability_issues(),
            vec![LexHistoryDurabilityIssue::LearningMemoryOnly],
            "the first commit the freeze refuses is memory-only, and is reported"
        );
        drop(gate);
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
