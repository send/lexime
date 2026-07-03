use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use tracing::warn;

use crate::dict::connection::ConnectionMatrix;
use crate::dict::{CompositeDictionary, Dictionary, TrieDictionary};
use crate::user_history::wal::HistoryWal;
use crate::user_history::UserHistory;

use super::{LexError, LexUserDictionary};

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
        // read failure only (EACCES etc.). PR2 surfaces the report to Swift;
        // until then it is logged here.
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
        });
        // Startup compaction (§5.1-6): checkpoint recovery results early so
        // the next startup is clean. This is also the heal path for a
        // frozen WAL (failed tail repair / failed migration commit): the
        // checkpoint covers memory, so the truncation that follows it
        // restores appendable v2 form — without this, appends would keep
        // failing and threshold-based compaction would never trigger.
        if report.compaction_recommended {
            this.spawn_compact();
        }
        Ok(this)
    }

    /// Clear all learning history (in-memory + WAL + checkpoint files).
    fn clear(&self) -> Result<(), LexError> {
        self.clear_impl()
    }
}

impl LexUserHistory {
    pub(super) fn clear_impl(&self) -> Result<(), LexError> {
        // Park until any in-flight compaction finishes (§5.5 step 1): a
        // compactor that cloned a pre-clear snapshot must not save it after
        // the files are removed, or the privacy wipe would be undone.
        // Holding the gate also blocks new compactions for the duration.
        let _gate = self.lock_gate();
        // Clearing supersedes requests posted so far: it truncates the WAL
        // (unfreezing it) and empties memory. A heal posted mid-clear parks
        // on the gate and runs after release, checkpointing the (empty)
        // post-clear state — harmless.
        self.scrub_pending.store(false, Ordering::SeqCst);
        self.clear_locked()
    }

    /// Acquire the compaction gate, recovering through poisoning: the gate
    /// protects no data (see the field docs).
    fn lock_gate(&self) -> std::sync::MutexGuard<'_, ()> {
        self.compact_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn clear_locked(&self) -> Result<(), LexError> {
        {
            let mut h = self.inner.write().map_err(|e| LexError::Io {
                msg: format!("history write lock poisoned: {e}"),
            })?;
            *h = UserHistory::new();
        }
        // Drop the commit-log handle before removing the file so a later
        // append reopens (re-creates) it instead of writing to an unlinked
        // inode.
        let commit_log_path = {
            let mut log = self.commit_log.lock().map_err(|e| LexError::Io {
                msg: format!("commit-log lock poisoned: {e}"),
            })?;
            log.file = None;
            log.path.clone()
        };
        let mut wal = self.wal.lock().map_err(|e| LexError::Io {
            msg: format!("WAL lock poisoned: {e}"),
        })?;
        wal.truncate_wal()?;
        for path in [
            wal.wal_path(),
            wal.checkpoint_path(),
            commit_log_path.as_path(),
        ] {
            match std::fs::remove_file(path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        // Privacy wipe: quarantined bytes and the v1 migration backup must
        // not survive a clear.
        crate::user_history::recovery::remove_recovery_artifacts(wal.checkpoint_path())?;
        Ok(())
    }

    /// Append a WAL entry. Does not block on compaction.
    pub(super) fn append_wal(self: &Arc<Self>, segments: &[(String, String)], timestamp: u64) {
        let failed = {
            let mut wal = match self.wal.lock() {
                Ok(w) => w,
                Err(e) => {
                    warn!("WAL lock poisoned: {e}");
                    return;
                }
            };
            match wal.append(segments, timestamp) {
                Ok(seq) => {
                    // Advance applied_seq while still holding the wal mutex
                    // (lock order: wal -> inner). The in-memory effect of this
                    // frame was applied in record_history phase 1, so once the
                    // frame is on disk a checkpoint snapshot may claim coverage
                    // of `seq`. A snapshot racing between phase 1 and here only
                    // under-reports applied_seq — the safe direction (bounded
                    // re-apply after a crash, never loss).
                    match self.inner.write() {
                        Ok(mut h) => h.advance_applied_seq(seq),
                        // Recover through the poison: advance_applied_seq is
                        // a monotonic u64 max, safe regardless of the state
                        // the panicking holder left behind — and a stale
                        // applied_seq would let a later checkpoint under-
                        // report coverage (restart replay then re-applies
                        // its frames).
                        Err(poisoned) => {
                            warn!("history write lock poisoned; advancing applied_seq anyway");
                            poisoned.into_inner().advance_applied_seq(seq);
                        }
                    }
                    false
                }
                // Memory keeps the record (§5.2): the next checkpoint is a full
                // snapshot, so a recovered disk picks it up; the frame's absence
                // makes double-apply impossible.
                Err(e) => {
                    warn!("WAL append failed: {e}");
                    true
                }
            }
        };
        // An append failure freezes the WAL (partial frame at the tail);
        // only a post-checkpoint truncation restores appendable form, and
        // the entry-count threshold can no longer reach it. Post a heal
        // request (checkpoint writes may still fail under a persistent disk
        // problem — the next record's failure re-posts).
        if failed {
            self.spawn_compact();
        }
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

    /// Spawn background compaction if threshold is reached.
    pub(super) fn maybe_compact(self: &Arc<Self>) {
        let needs = match self.wal.lock() {
            Ok(wal) => wal.needs_compact(),
            Err(_) => false,
        };
        if !needs {
            return;
        }
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
                if this.run_compact_impl(true) {
                    this.spawn_compact();
                }
            })
        {
            warn!("failed to spawn compaction thread: {e}");
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
            .spawn(move || {
                let _gate = this.lock_gate();
                if this.scrub_pending.swap(false, Ordering::SeqCst) && this.run_compact_impl(true) {
                    this.spawn_compact();
                }
            })
        {
            // scrub_pending stays posted; the next request (or the next
            // gated run's consume) picks it up.
            warn!("failed to spawn compaction thread: {e}");
        }
    }

    /// Force an immediate compaction (used after history deletion to
    /// persist changes). Parks until any in-flight compaction finishes,
    /// then truncates unconditionally: leaving a racing Committed frame for
    /// the just-deleted entry uncovered in the WAL would let the next
    /// replay resurrect the deleted learning. Destroying an uncovered frame
    /// instead loses at most that racing commit (v1 semantics; the
    /// structural fix is PR2's Tombstone frames, which make the
    /// delete-after-commit ordering durable in the WAL itself).
    pub(super) fn force_compact(&self) {
        let _gate = self.lock_gate();
        // This full-snapshot run covers heal requests posted so far.
        self.scrub_pending.store(false, Ordering::SeqCst);
        self.run_compact_impl(false);
    }

    #[cfg(test)]
    fn run_compact(&self) {
        self.run_compact_impl(true);
    }

    /// Returns whether a follow-up pass is warranted: frames were appended
    /// after this run's snapshot, so the covered-only truncation was
    /// skipped.
    fn run_compact_impl(&self, covered_only: bool) -> bool {
        // 1. Clone history under read lock (brief)
        let snapshot = match self.inner.read() {
            Ok(h) => h.clone(),
            Err(e) => {
                warn!("history read lock failed during compaction: {e}");
                return false;
            }
        };
        let cp_path = match self.wal.lock() {
            Ok(wal) => wal.checkpoint_path().to_path_buf(),
            Err(e) => {
                warn!("WAL lock poisoned during compaction: {e}");
                return false;
            }
        };

        // 2. Write checkpoint (no locks held, slow I/O)
        if let Err(e) = snapshot.save(&cp_path) {
            warn!("checkpoint write failed: {e}");
            return false;
        }

        // 3. Truncate WAL (brief lock) — conditionally (§5.3): only frames
        // provably covered by the durable checkpoint (seq <= applied_seq at
        // snapshot time) may be destroyed. The deletion path truncates
        // unconditionally (see force_compact).
        let mut wal = match self.wal.lock() {
            Ok(w) => w,
            Err(e) => {
                warn!("WAL lock poisoned; skipping truncation after checkpoint: {e}");
                return false;
            }
        };
        if covered_only {
            match wal.truncate_covered(snapshot.applied_seq()) {
                Ok(true) => false,
                Ok(false) => {
                    // Frames landed after our snapshot: request a follow-up
                    // pass (§5.3 step 5). A commit whose in-memory effect
                    // raced into this checkpoint before its frame was
                    // sequenced would otherwise stay replayable until the
                    // next threshold compaction; the follow-up's snapshot
                    // carries the advanced applied_seq and covers it. This
                    // narrows the residual double-apply to a crash between
                    // the two passes — the structural fix (WAL-ahead
                    // ordering under the wal mutex, so snapshots can never
                    // contain unsequenced effects) lands in PR2.
                    true
                }
                Err(e) => {
                    warn!("WAL truncate failed: {e}");
                    false
                }
            }
        } else {
            if let Err(e) = wal.truncate_wal() {
                warn!("WAL truncate failed: {e}");
            }
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_commit_log_append_and_clear() {
        let dir = tempfile::tempdir().unwrap();
        // Nested path that does not exist yet: the first append must create
        // the parent directory (fresh-install case).
        let cp = dir.path().join("nested").join("history.lxud");
        let hist = LexUserHistory::open(cp.display().to_string()).unwrap();

        hist.append_commit_log(r#"{"t":1,"reading":"きょう","surface":"今日","rank":0}"#);
        hist.append_commit_log(
            r#"{"t":2,"reading":"きょう","surface":"京","rank":2,"top1":"今日"}"#,
        );

        let log_path = dir.path().join("nested").join("commit-log.jsonl");
        let content = std::fs::read_to_string(&log_path).unwrap();
        assert_eq!(content.lines().count(), 2);

        // clear() removes the commit log along with history files
        hist.clear_impl().unwrap();
        assert!(!log_path.exists(), "commit log should be removed by clear");
    }

    #[test]
    fn test_force_compact_truncates_uncovered_frames() {
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("history.lxud");
        let hist = LexUserHistory::open(cp.display().to_string()).unwrap();

        // Covered frame: the normal path advances applied_seq
        hist.append_wal(&[("きょう".into(), "今日".into())], 1000);
        // Uncovered frame: append directly without advancing applied_seq,
        // modeling a commit racing the compaction snapshot
        hist.wal
            .lock()
            .unwrap()
            .append(&[("あす".into(), "明日".into())], 1001)
            .unwrap();

        // Background-style compaction must skip truncation (uncovered frame
        // would be destroyed while absent from the checkpoint)
        hist.run_compact();
        assert_eq!(
            hist.wal.lock().unwrap().entry_count(),
            2,
            "covered-only truncation must be skipped"
        );

        // Deletion-path compaction truncates unconditionally so a deleted
        // entry can never be resurrected by an uncovered replayable frame
        hist.force_compact();
        assert_eq!(hist.wal.lock().unwrap().entry_count(), 0);
    }

    #[test]
    fn test_clear_resets_history_and_removes_files() {
        let dir = tempfile::tempdir().unwrap();
        let cp = dir.path().join("history.lxud");

        // Open, record some entries, and trigger a checkpoint write
        let hist = LexUserHistory::open(cp.display().to_string()).unwrap();
        hist.append_wal(&[("きょう".into(), "今日".into())], 1000);
        hist.inner
            .write()
            .unwrap()
            .record_at(&[("きょう".to_string(), "今日".to_string())], 1000);
        // Force checkpoint so files exist on disk
        hist.run_compact();

        let wal_path = hist.wal.lock().unwrap().wal_path().to_path_buf();
        let cp_path = hist.wal.lock().unwrap().checkpoint_path().to_path_buf();
        assert!(cp_path.exists(), "checkpoint should exist before clear");

        // Clear and verify
        hist.clear_impl().unwrap();

        // In-memory history should be empty
        let h = hist.inner.read().unwrap();
        assert!(
            h.learned_surfaces("きょう", 2000).is_empty(),
            "in-memory history should be empty after clear"
        );
        drop(h);

        // Files should be removed
        assert!(!cp_path.exists(), "checkpoint file should be deleted");
        assert!(!wal_path.exists(), "WAL file should be deleted");

        // Re-opening from the same path should yield empty history
        drop(hist);
        let hist2 = LexUserHistory::open(cp.display().to_string()).unwrap();
        let h2 = hist2.inner.read().unwrap();
        assert!(
            h2.learned_surfaces("きょう", 2000).is_empty(),
            "reopened history should be empty"
        );
    }
}
