//! Write-Ahead Log for UserHistory persistence.
//!
//! Each confirmed conversion appends a small frame (~40-80 bytes) instead
//! of serializing the entire history. A periodic checkpoint writes the full
//! state and truncates the WAL.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::UserHistory;

const COMPACT_THRESHOLD: usize = 1000;

/// A single WAL entry — mirrors the arguments to `UserHistory::record_at`.
#[derive(Serialize, Deserialize)]
struct WalEntry {
    segments: Vec<(String, String)>,
    timestamp: u64,
}

/// WAL state that lives alongside a checkpoint file.
pub struct HistoryWal {
    /// Path to the checkpoint file (`user_history.lxud`).
    checkpoint_path: PathBuf,
    /// Path to the WAL file (`user_history.lxud.wal`).
    wal_path: PathBuf,
    /// Kept open in append mode to avoid repeated open/close per entry.
    file: Option<File>,
    /// Number of entries in the current WAL (since last compaction).
    entry_count: usize,
}

impl HistoryWal {
    /// Create a new WAL handle for the given checkpoint path.
    pub fn new(checkpoint_path: &Path) -> Self {
        let wal_path = checkpoint_path.with_extension("lxud.wal");
        Self {
            checkpoint_path: checkpoint_path.to_path_buf(),
            wal_path,
            file: None,
            entry_count: 0,
        }
    }

    /// Replay the WAL into the given UserHistory.
    /// Returns the number of entries replayed.
    pub fn replay(&mut self, history: &mut UserHistory) -> io::Result<usize> {
        let data = match fs::read(&self.wal_path) {
            Ok(d) => d,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                self.entry_count = 0;
                return Ok(0);
            }
            Err(e) => return Err(e),
        };

        let mut count = 0;
        let mut pos = 0;
        while pos + 8 <= data.len() {
            let length = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
            let expected_crc = u32::from_le_bytes(data[pos + 4..pos + 8].try_into().unwrap());

            if length == 0 || pos + 8 + length > data.len() {
                break; // truncated frame
            }

            let payload = &data[pos + 8..pos + 8 + length];
            let actual_crc = crc32fast::hash(payload);
            if actual_crc != expected_crc {
                break; // corrupt frame
            }

            match bincode::deserialize::<WalEntry>(payload) {
                Ok(entry) => {
                    history.record_at(&entry.segments, entry.timestamp);
                    count += 1;
                }
                Err(_) => break, // corrupt payload
            }

            pos += 8 + length;
        }

        self.entry_count = count;
        Ok(count)
    }

    /// Append an entry to the WAL file.
    pub fn append(&mut self, segments: &[(String, String)], timestamp: u64) -> io::Result<()> {
        let entry = WalEntry {
            segments: segments.to_vec(),
            timestamp,
        };
        let payload = bincode::serialize(&entry).map_err(io::Error::other)?;
        let length = payload.len() as u32;
        let crc = crc32fast::hash(&payload);

        let file = self.open_file()?;
        file.write_all(&length.to_le_bytes())?;
        file.write_all(&crc.to_le_bytes())?;
        file.write_all(&payload)?;

        self.entry_count += 1;
        Ok(())
    }

    /// Get or lazily open the WAL file handle.
    fn open_file(&mut self) -> io::Result<&mut File> {
        if self.file.is_none() {
            if let Some(parent) = self.wal_path.parent() {
                fs::create_dir_all(parent)?;
            }
            let f = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.wal_path)?;
            self.file = Some(f);
        }
        Ok(self.file.as_mut().unwrap())
    }

    /// Whether the WAL has reached the compaction threshold.
    pub fn needs_compact(&self) -> bool {
        self.entry_count >= COMPACT_THRESHOLD
    }

    /// Truncate the WAL file and reset entry count.
    /// Call after a checkpoint has been written.
    pub fn truncate_wal(&mut self) -> io::Result<()> {
        self.file = None;
        File::create(&self.wal_path)?;
        self.entry_count = 0;
        Ok(())
    }

    /// Current WAL entry count.
    pub fn entry_count(&self) -> usize {
        self.entry_count
    }

    /// Path to the checkpoint file.
    pub fn checkpoint_path(&self) -> &Path {
        &self.checkpoint_path
    }

    /// Path to the WAL file (for testing).
    pub fn wal_path(&self) -> &Path {
        &self.wal_path
    }
}

/// Convenience: open checkpoint + replay WAL in one call.
pub fn open_with_wal(checkpoint_path: &Path) -> io::Result<(UserHistory, HistoryWal)> {
    let mut history = UserHistory::open(checkpoint_path)?;
    let mut wal = HistoryWal::new(checkpoint_path);
    wal.replay(&mut history)?;
    Ok((history, wal))
}
