use std::path::Path;
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
        let dict = TrieDictionary::open(Path::new(&path))
            .map_err(|e: crate::dict::DictError| LexError::Io { msg: e.to_string() })?;
        Ok(Arc::new(Self {
            inner: Arc::new(dict),
        }))
    }

    #[uniffi::constructor]
    fn open_with_user_dict(
        path: String,
        user_dict: Option<Arc<LexUserDictionary>>,
    ) -> Result<Arc<Self>, LexError> {
        let trie = TrieDictionary::open(Path::new(&path))
            .map_err(|e: crate::dict::DictError| LexError::Io { msg: e.to_string() })?;

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
        let conn = ConnectionMatrix::open(Path::new(&path))
            .map_err(|e: crate::dict::DictError| LexError::Io { msg: e.to_string() })?;
        Ok(Arc::new(Self {
            inner: Arc::new(conn),
        }))
    }
}

#[derive(uniffi::Object)]
pub struct LexUserHistory {
    pub(crate) inner: Arc<RwLock<UserHistory>>,
    wal: Mutex<HistoryWal>,
    compacting: AtomicBool,
}

#[uniffi::export]
impl LexUserHistory {
    #[uniffi::constructor]
    fn open(path: String) -> Result<Arc<Self>, LexError> {
        let cp = Path::new(&path);
        let (history, wal) = crate::user_history::wal::open_with_wal(cp)
            .map_err(|e: std::io::Error| LexError::Io { msg: e.to_string() })?;
        Ok(Arc::new(Self {
            inner: Arc::new(RwLock::new(history)),
            wal: Mutex::new(wal),
            compacting: AtomicBool::new(false),
        }))
    }

    /// Clear all learning history (in-memory + WAL + checkpoint files).
    fn clear(&self) -> Result<(), LexError> {
        self.clear_impl()
    }
}

impl LexUserHistory {
    pub(super) fn clear_impl(&self) -> Result<(), LexError> {
        {
            let mut h = self.inner.write().map_err(|e| LexError::Io {
                msg: format!("history write lock poisoned: {e}"),
            })?;
            *h = UserHistory::new();
        }
        let mut wal = self.wal.lock().map_err(|e| LexError::Io {
            msg: format!("WAL lock poisoned: {e}"),
        })?;
        wal.truncate_wal()
            .map_err(|e| LexError::Io { msg: e.to_string() })?;
        for path in [wal.wal_path(), wal.checkpoint_path()] {
            match std::fs::remove_file(path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(LexError::Io { msg: e.to_string() }),
            }
        }
        Ok(())
    }

    /// Append a WAL entry. Does not block on compaction.
    pub(super) fn append_wal(&self, segments: &[(String, String)], timestamp: u64) {
        let mut wal = match self.wal.lock() {
            Ok(w) => w,
            Err(e) => {
                warn!("WAL lock poisoned: {e}");
                return;
            }
        };
        if let Err(e) = wal.append(segments, timestamp) {
            warn!("WAL append failed: {e}");
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
        // Prevent concurrent compactions
        if self.compacting.swap(true, Ordering::Relaxed) {
            return;
        }
        let this = Arc::clone(self);
        std::thread::spawn(move || {
            this.run_compact();
            this.compacting.store(false, Ordering::Relaxed);
        });
    }

    /// Force an immediate compaction (used after history deletion to persist changes).
    /// Waits for any in-flight background compaction to finish, then runs another
    /// compaction to ensure the post-deletion snapshot is persisted.
    pub(super) fn force_compact(&self) {
        // Spin-wait for any in-flight background compaction to finish.
        // Compaction is fast (snapshot clone + single file write), so this is brief.
        while self.compacting.load(Ordering::Relaxed) {
            std::thread::yield_now();
        }
        self.run_compact();
    }

    fn run_compact(&self) {
        // 1. Clone history under read lock (brief)
        let snapshot = match self.inner.read() {
            Ok(h) => h.clone(),
            Err(e) => {
                warn!("history read lock failed during compaction: {e}");
                return;
            }
        };
        let cp_path = match self.wal.lock() {
            Ok(wal) => wal.checkpoint_path().to_path_buf(),
            Err(_) => return,
        };

        // 2. Write checkpoint (no locks held, slow I/O)
        if let Err(e) = snapshot.save(&cp_path) {
            warn!("checkpoint write failed: {e}");
            return;
        }

        // 3. Truncate WAL (brief lock)
        if let Ok(mut wal) = self.wal.lock() {
            if let Err(e) = wal.truncate_wal() {
                warn!("WAL truncate failed: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
