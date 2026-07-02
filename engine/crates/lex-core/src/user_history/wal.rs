//! Write-Ahead Log for UserHistory persistence (v2).
//!
//! Each confirmed conversion appends a small frame (~50-90 bytes) instead of
//! serializing the entire history. A periodic checkpoint writes the full
//! state and truncates the WAL back to its 8-byte file header.
//!
//! File header (8 bytes) — v1 WALs had no header, so its presence doubles as
//! the format-generation marker:
//!
//! | offset | size | field    | content                     |
//! |--------|------|----------|-----------------------------|
//! | 0      | 4    | magic    | `LXWL`                      |
//! | 4      | 1    | version  | `2`                         |
//! | 5      | 3    | reserved | 0 on write, ignored on read |
//!
//! Frame (16-byte fixed part + payload):
//!
//! | offset | size | field       | content                                  |
//! |--------|------|-------------|------------------------------------------|
//! | 0      | 4    | payload_len | u32 LE, payload only                     |
//! | 4      | 4    | crc32       | u32 LE over offset 8..end (seq + payload)|
//! | 8      | 8    | seq         | u64 LE, strictly increasing within a file|
//! | 16     | —    | payload     | bincode(`WalRecord`)                     |
//!
//! `seq` sits at a fixed offset so frames with `seq <= applied_seq` can be
//! skipped without decoding the payload (a crash between checkpoint write
//! and WAL truncation leaves the whole WAL skippable — replay then costs
//! O(frames) instead of O(decode)).

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use bincode::Options as _;
use serde::{Deserialize, Serialize};
use tracing::warn;

use super::persistence::bincode_reader;
use super::UserHistory;

pub(super) const WAL_MAGIC: &[u8; 4] = b"LXWL";
pub(super) const WAL_VERSION: u8 = 2;
pub(super) const WAL_HEADER: [u8; WAL_HEADER_LEN] = [b'L', b'X', b'W', b'L', WAL_VERSION, 0, 0, 0];
pub(super) const WAL_HEADER_LEN: usize = 8;
pub(super) const FRAME_HEADER_LEN: usize = 16;

const COMPACT_THRESHOLD: usize = 1000;
/// Byte-size backstop: rare truncate skips (see `truncate_covered`) and
/// oversized frames must not let the file grow without bound.
const COMPACT_MAX_BYTES: u64 = 1024 * 1024;
/// Committed frames ride a write barrier every N frames (§6): power loss
/// costs at most N confirmed conversions, which are re-learnable.
const BARRIER_EVERY_FRAMES: usize = 50;

/// A single WAL record. bincode 1.x encodes the variant index as u32 LE,
/// followed by the fields.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum WalRecord {
    /// A confirmed conversion — replayed via `UserHistory::record`-equivalent.
    Committed {
        segments: Vec<(String, String)>,
        timestamp: u64,
    },
    /// Durable deletion marker — replayed via `UserHistory::remove_entries`.
    /// `timestamp` is diagnostics only; deletion semantics do not use it.
    Tombstone {
        segments: Vec<(String, String)>,
        timestamp: u64,
    },
}

/// v1 WAL entry (headerless file of `length + crc32(payload) + payload`
/// frames). Retained for the migration reader and test fixtures.
#[derive(Serialize, Deserialize)]
pub(super) struct LegacyWalEntry {
    pub(super) segments: Vec<(String, String)>,
    pub(super) timestamp: u64,
}

/// How the checkpoint that pairs with this WAL was loaded. Governs legacy
/// (headerless) WAL handling: a v2 checkpoint already contains everything a
/// v1 WAL could hold (migration writes the checkpoint before touching the
/// WAL), so replaying it would double-apply (§2.3).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CheckpointOrigin {
    V1,
    V2,
    Missing,
}

#[derive(Default, Debug)]
pub struct ReplaySummary {
    pub frames_replayed: u64,
    pub frames_skipped: u64,
}

/// Abstraction over the WAL file's write path. Exists as a testing seam:
/// fault-injection tests substitute a recording/failing implementation to
/// verify syscall ordering (T6) without a real disk.
pub trait WalIo: Send {
    /// Append bytes at the end of the log, creating the file (including the
    /// v2 file header) if needed.
    fn append(&mut self, buf: &[u8]) -> io::Result<()>;
    /// Write barrier: on Apple targets `fcntl(F_BARRIERFSYNC)` (~0.3ms on
    /// M-series). NOTE: std's `File::sync_data` maps to F_FULLFSYNC on Apple
    /// (verified against the 1.95.0 std source), which is why this goes
    /// through libc directly — a full flush (~4ms) on the key-processing
    /// thread every [`BARRIER_EVERY_FRAMES`] commits is the thing §6 avoids.
    fn sync_barrier(&mut self) -> io::Result<()>;
    /// Full durability: `File::sync_all` = `fcntl(F_FULLFSYNC)` on Apple.
    fn sync_full(&mut self) -> io::Result<()>;
    /// Recreate the log as a header-only file and sync it.
    fn truncate_to_header(&mut self) -> io::Result<()>;
}

struct FileWalIo {
    path: PathBuf,
    /// Kept open in append mode to avoid repeated open/close per entry.
    file: Option<File>,
}

use super::persistence::ensure_parent_dir;

impl FileWalIo {
    fn open_file(&mut self) -> io::Result<&mut File> {
        if self.file.is_none() {
            ensure_parent_dir(&self.path)?;
            let mut f = OpenOptions::new()
                .read(true)
                .create(true)
                .append(true)
                .open(&self.path)?;
            let len = f.metadata()?.len();
            if len < WAL_HEADER_LEN as u64 {
                if len != 0 {
                    // 1-7 byte stub (torn truncation): recreate as
                    // header-only so frames never land after foreign bytes
                    // and get misclassified at the next startup.
                    f.set_len(0)?;
                }
                f.write_all(&WAL_HEADER)?;
                // Order the header ahead of any frame bytes: without this,
                // power loss could persist later frames while the header
                // page is lost, and recovery would misclassify the file as
                // headerless and quarantine otherwise-recoverable frames.
                barrier_fsync(&f)?;
                // A freshly created file's directory entry needs a parent
                // fsync to be durable — otherwise a power loss could drop
                // the whole file, frames included, past the barrier bound.
                if !super::persistence::sync_parent_dir(&self.path) {
                    warn!("WAL parent dir sync failed; new file's durability unconfirmed");
                }
            } else {
                // Validate the existing header: appending after unreadable
                // header bytes (external edit, on-disk rot) would get the
                // whole file — new frames included — quarantined at the
                // next startup. The Err freezes the WAL via append_record's
                // failure path; compaction heals it by re-creating the file.
                use std::io::{Read, Seek, SeekFrom};
                let mut header = [0u8; WAL_HEADER_LEN];
                f.seek(SeekFrom::Start(0))?;
                f.read_exact(&mut header)?;
                // O_APPEND writes always go to EOF; no seek-back needed.
                // Match classify_wal: magic + version only, reserved bytes
                // are "ignored on read" by the format spec.
                if &header[0..4] != WAL_MAGIC || header[4] != WAL_VERSION {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "WAL header mismatch; refusing to append",
                    ));
                }
            }
            self.file = Some(f);
        }
        Ok(self.file.as_mut().expect("file set by preceding lines"))
    }
}

#[cfg(target_vendor = "apple")]
fn barrier_fsync(f: &File) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    let rc = unsafe { libc::fcntl(f.as_raw_fd(), libc::F_BARRIERFSYNC) };
    if rc == 0 {
        Ok(())
    } else {
        // e.g. filesystems without barrier support — fall back to the full
        // flush rather than silently skipping durability.
        f.sync_data()
    }
}

#[cfg(not(target_vendor = "apple"))]
fn barrier_fsync(f: &File) -> io::Result<()> {
    f.sync_data()
}

impl WalIo for FileWalIo {
    fn append(&mut self, buf: &[u8]) -> io::Result<()> {
        self.open_file()?.write_all(buf)
    }

    fn sync_barrier(&mut self) -> io::Result<()> {
        match &self.file {
            Some(f) => barrier_fsync(f),
            None => Ok(()),
        }
    }

    fn sync_full(&mut self) -> io::Result<()> {
        match &self.file {
            Some(f) => f.sync_all(),
            None => Ok(()),
        }
    }

    fn truncate_to_header(&mut self) -> io::Result<()> {
        // Drop the handle first and do not retain the new one: clear() may
        // remove the file right after, and a retained handle would make a
        // later append write to an unlinked inode.
        self.file = None;
        ensure_parent_dir(&self.path)?;
        let mut f = File::create(&self.path)?;
        f.write_all(&WAL_HEADER)?;
        f.sync_data()?;
        // If create() made a new directory entry (file was removed
        // externally), it needs a parent fsync to be durable.
        if !super::persistence::sync_parent_dir(&self.path) {
            warn!("WAL parent dir sync failed after truncation; durability unconfirmed");
        }
        Ok(())
    }
}

pub(super) fn wal_path_for(checkpoint_path: &Path) -> PathBuf {
    checkpoint_path.with_extension("lxud.wal")
}

/// WAL state that lives alongside a checkpoint file.
///
/// # Single-writer invariant
///
/// Exactly one writing handle may exist per history path (§3: `next_seq` is
/// owned by this handle; §4's concurrency model is intra-process). The
/// engine upholds this by opening one `LexUserHistory` per file for the
/// process's lifetime; offline tooling uses the strict read-only paths.
/// Concurrent writers were never supported: in v1 a second handle's
/// compaction would checkpoint its own memory (lacking the other handle's
/// commits entirely) and truncate the shared WAL, silently discarding the
/// other handle's learning wholesale. Under v2 the failure is narrower
/// (duplicate seqs read as a corrupt tail) but still a failure — do not
/// open two writing handles on one path.
pub struct HistoryWal {
    /// Path to the checkpoint file (`user_history.lxud`).
    checkpoint_path: PathBuf,
    /// Path to the WAL file (`user_history.lxud.wal`).
    wal_path: PathBuf,
    io: Box<dyn WalIo>,
    /// Number of valid frames in the file (replayed + skipped).
    entry_count: usize,
    /// Approximate on-disk size, for the compaction byte backstop.
    wal_bytes: u64,
    /// Next sequence number to assign. Never reset — not even by truncation
    /// or clear — so a frame seq can never collide with a checkpoint's
    /// applied_seq from a previous generation.
    next_seq: u64,
    /// Highest seq physically appended to (or found in) the file. Guards
    /// `truncate_covered`.
    last_appended_seq: u64,
    frames_since_barrier: usize,
    /// Set when the on-disk file is known not to be in appendable v2 form
    /// (legacy format pending a failed migration, unrepaired tail, ...).
    /// Appending v2 frames after foreign bytes would make them unreadable,
    /// so appends fail fast instead; a successful truncate (e.g. by the next
    /// compaction, whose checkpoint contains the full state) re-establishes
    /// v2 form and lifts the freeze.
    frozen: bool,
}

impl HistoryWal {
    /// Create a new WAL handle for the given checkpoint path.
    pub fn new(checkpoint_path: &Path) -> Self {
        let wal_path = wal_path_for(checkpoint_path);
        Self {
            checkpoint_path: checkpoint_path.to_path_buf(),
            wal_path: wal_path.clone(),
            io: Box::new(FileWalIo {
                path: wal_path,
                file: None,
            }),
            entry_count: 0,
            wal_bytes: WAL_HEADER_LEN as u64,
            next_seq: 1,
            last_appended_seq: 0,
            frames_since_barrier: 0,
            frozen: false,
        }
    }

    /// Testing seam: construct with a custom I/O backend (fault injection).
    pub fn with_io(checkpoint_path: &Path, io: Box<dyn WalIo>) -> Self {
        let mut wal = Self::new(checkpoint_path);
        wal.io = io;
        wal
    }

    /// Replay the WAL into the given UserHistory (strict: no repairs, no
    /// file mutation — offline tooling reads live user files). Frames with
    /// `seq <= history.applied_seq()` are skipped; scanning stops silently
    /// at the first corrupt/truncated frame, mirroring v1 semantics.
    pub fn replay(
        &mut self,
        history: &mut UserHistory,
        origin: CheckpointOrigin,
    ) -> io::Result<ReplaySummary> {
        let data = match fs::read(&self.wal_path) {
            Ok(d) => d,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                self.adopt_empty(history.applied_seq());
                return Ok(ReplaySummary::default());
            }
            Err(e) => return Err(e),
        };

        match classify_wal(&data) {
            WalFormat::Stub => {
                self.adopt_empty(history.applied_seq());
                self.frozen = !data.is_empty();
                Ok(ReplaySummary::default())
            }
            // A whole-file format mismatch must fail loudly in the strict
            // path: silently reporting an empty WAL would let offline audits
            // produce plausible-but-incomplete results. (Frame-level tail
            // corruption below keeps the v1 stop-silently semantics — a torn
            // tail is the expected power-loss residue.)
            WalFormat::BadHeader => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "WAL header unreadable (unknown version); use the engine's \
                 recovery path or inspect the file",
            )),
            WalFormat::Legacy => {
                if origin == CheckpointOrigin::V2 {
                    if legacy_valid_prefix(&data) == 0 {
                        // Not readable as v1 either — most likely a v2 WAL
                        // with corrupted magic, not migration residue.
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "headerless WAL with no readable v1 frame",
                        ));
                    }
                    // Migration-crash residue: already covered by the v2
                    // checkpoint (§2.3) — do not replay.
                    self.adopt_empty(history.applied_seq());
                    self.frozen = true;
                    return Ok(ReplaySummary::default());
                }
                let scan = scan_legacy(&data, history);
                if scan.frames_applied > 0 {
                    history.evict();
                }
                self.adopt_scan(
                    scan.frames_applied as usize,
                    data.len() as u64,
                    0,
                    history.applied_seq(),
                );
                self.frozen = true; // v2 frames must not follow v1 bytes
                Ok(ReplaySummary {
                    frames_replayed: scan.frames_applied,
                    frames_skipped: 0,
                })
            }
            WalFormat::V2 => {
                let scan = scan_v2(&data, history);
                if scan.frames_applied > 0 {
                    history.evict();
                }
                self.adopt_scan(
                    scan.valid_frames,
                    data.len() as u64,
                    scan.max_seq,
                    history.applied_seq(),
                );
                // An unrepaired corrupt tail would swallow anything appended
                // after it (strict mode never truncates the file).
                self.frozen = scan.truncated_tail;
                Ok(ReplaySummary {
                    frames_replayed: scan.frames_applied,
                    frames_skipped: scan.frames_skipped,
                })
            }
        }
    }

    /// Append a Committed record. Returns the assigned sequence number.
    pub fn append(&mut self, segments: &[(String, String)], timestamp: u64) -> io::Result<u64> {
        self.append_record(&WalRecord::Committed {
            segments: segments.to_vec(),
            timestamp,
        })
    }

    /// Append a record frame. Returns the assigned sequence number.
    ///
    /// Durability (§6): Tombstones are privacy-critical — full flush
    /// (F_FULLFSYNC, ~4ms on M-series) before returning, so a deletion never
    /// resurrects even across power loss. Committed frames take a write
    /// barrier every [`BARRIER_EVERY_FRAMES`] frames instead.
    ///
    /// Failure semantics:
    /// - `Err` before the frame write: nothing on disk; the assigned seq is
    ///   consumed (gaps are legal, §3, replay only warns).
    /// - Frame write failure: a partial frame may sit at the tail, and any
    ///   later append would land after unreadable bytes and be lost to
    ///   replay (problem A at runtime) — so the WAL freezes until a
    ///   truncation (post-checkpoint) re-establishes appendable form.
    /// - Committed barrier failure: logged, still `Ok` — the frame is in
    ///   the file (callers must count it as covered or a crash would
    ///   double-apply it); only the power-loss window widens.
    /// - Tombstone `sync_full` failure: `Err`, because the immediate
    ///   durability is the operation's contract. The frame remains appended
    ///   and uncovered, which is safe: re-applying a Tombstone on replay is
    ///   idempotent.
    pub fn append_record(&mut self, record: &WalRecord) -> io::Result<u64> {
        if self.frozen {
            return Err(io::Error::other(
                "WAL file is not in appendable v2 form (pending compaction)",
            ));
        }
        let payload = bincode::serialize(record).map_err(io::Error::other)?;
        let payload_len = u32::try_from(payload.len())
            .map_err(|_| io::Error::other("WAL entry too large (>4 GiB)"))?;
        let seq = self.next_seq;
        self.next_seq += 1;

        let mut hasher = crc32fast::Hasher::new();
        hasher.update(&seq.to_le_bytes());
        hasher.update(&payload);
        let crc = hasher.finalize();

        // Build the frame in one buffer for a single write_all call, so a
        // partial write is the only tearing mode (write_all may still issue
        // multiple write(2) calls — torn tails are repaired by replay; the
        // point is not to interleave header and payload as separate appends
        // the way v1 did).
        let mut frame = Vec::with_capacity(FRAME_HEADER_LEN + payload.len());
        frame.extend_from_slice(&payload_len.to_le_bytes());
        frame.extend_from_slice(&crc.to_le_bytes());
        frame.extend_from_slice(&seq.to_le_bytes());
        frame.extend_from_slice(&payload);
        if let Err(e) = self.io.append(&frame) {
            self.frozen = true;
            return Err(e);
        }

        self.last_appended_seq = seq;
        self.entry_count += 1;
        self.wal_bytes += frame.len() as u64;

        match record {
            WalRecord::Tombstone { .. } => {
                // Reset only after a successful flush: on Err the counter
                // stays saturated so the next append retries a sync instead
                // of deferring it a full interval.
                self.io.sync_full()?;
                self.frames_since_barrier = 0;
            }
            WalRecord::Committed { .. } => {
                self.frames_since_barrier += 1;
                if self.frames_since_barrier >= BARRIER_EVERY_FRAMES {
                    match self.io.sync_barrier() {
                        // Reset only on success: a transient failure must
                        // not defer the retry by another full interval, or
                        // the documented power-loss bound would double.
                        Ok(()) => self.frames_since_barrier = 0,
                        Err(e) => warn!(
                            "WAL barrier sync failed (frame is written, durability window widens): {e}"
                        ),
                    }
                }
            }
        }
        Ok(seq)
    }

    /// Whether the WAL has reached the compaction threshold.
    pub fn needs_compact(&self) -> bool {
        self.entry_count >= COMPACT_THRESHOLD || self.wal_bytes >= COMPACT_MAX_BYTES
    }

    /// Truncate the WAL back to a header-only file and reset the entry
    /// count. Call only after a checkpoint covering its frames is durable.
    pub fn truncate_wal(&mut self) -> io::Result<()> {
        self.io.truncate_to_header()?;
        self.entry_count = 0;
        self.wal_bytes = WAL_HEADER_LEN as u64;
        self.frames_since_barrier = 0;
        self.last_appended_seq = 0;
        self.frozen = false;
        Ok(())
    }

    /// Conditional truncation (§5.3): frames may be destroyed only when the
    /// durable checkpoint provably covers them (`last_appended_seq <=
    /// applied_seq`). If new frames were appended since the checkpoint
    /// snapshot, skip — replay is protected by the seq filter either way, so
    /// a skipped truncate only costs file size until the next compaction.
    /// Returns whether truncation ran.
    pub fn truncate_covered(&mut self, applied_seq: u64) -> io::Result<bool> {
        if self.last_appended_seq > applied_seq {
            return Ok(false);
        }
        self.truncate_wal()?;
        Ok(true)
    }

    /// Current WAL entry count.
    pub fn entry_count(&self) -> usize {
        self.entry_count
    }

    /// Approximate on-disk size (header + appended frames).
    pub fn file_bytes(&self) -> u64 {
        self.wal_bytes
    }

    /// Whether appends are currently rejected (see `frozen` field docs).
    pub fn is_frozen(&self) -> bool {
        self.frozen
    }

    /// Path to the checkpoint file.
    pub fn checkpoint_path(&self) -> &Path {
        &self.checkpoint_path
    }

    /// Path to the WAL file (for testing).
    pub fn wal_path(&self) -> &Path {
        &self.wal_path
    }

    pub(super) fn adopt_scan(
        &mut self,
        valid_frames: usize,
        file_bytes: u64,
        max_seq: u64,
        applied_seq: u64,
    ) {
        self.entry_count = valid_frames;
        self.wal_bytes = file_bytes.max(WAL_HEADER_LEN as u64);
        self.last_appended_seq = max_seq;
        self.next_seq = max_seq.max(applied_seq) + 1;
        // Existing frames may include an unbarriered tail from before the
        // restart (their power-loss durability is unknown even though they
        // were readable). Seed the counter so the first new append issues a
        // barrier, flushing them along with it — otherwise the documented
        // <= BARRIER_EVERY_FRAMES loss bound could double across a restart.
        self.frames_since_barrier = if valid_frames > 0 {
            BARRIER_EVERY_FRAMES - 1
        } else {
            0
        };
    }

    pub(super) fn adopt_empty(&mut self, applied_seq: u64) {
        self.adopt_scan(0, WAL_HEADER_LEN as u64, 0, applied_seq);
    }

    pub(super) fn freeze(&mut self) {
        self.frozen = true;
    }
}

// ---------------------------------------------------------------------------
// Format classification + frame scanning (shared by strict replay and the
// recovery path in `super::recovery`)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum WalFormat {
    /// 8-byte `LXWL` v2 header present.
    V2,
    /// No `LXWL` magic: v1 headerless frame stream.
    Legacy,
    /// Shorter than a file header (0-7 bytes) — the normal residue of a
    /// crash during WAL truncation.
    Stub,
    /// `LXWL` magic with an unknown version byte.
    BadHeader,
}

pub(super) fn classify_wal(data: &[u8]) -> WalFormat {
    if data.len() < WAL_HEADER_LEN {
        return WalFormat::Stub;
    }
    if &data[0..4] == WAL_MAGIC {
        if data[4] == WAL_VERSION {
            WalFormat::V2
        } else {
            WalFormat::BadHeader
        }
    } else {
        // A v1 file cannot start with `LXWL`: those bytes as a v1 length
        // field would claim a ~1.28 GB frame.
        WalFormat::Legacy
    }
}

pub(super) struct V2Scan {
    pub(super) frames_applied: u64,
    pub(super) frames_skipped: u64,
    /// applied + skipped.
    pub(super) valid_frames: usize,
    /// Byte offset just past the last valid frame (>= WAL_HEADER_LEN).
    /// Recovery physically truncates here when `truncated_tail` is set.
    pub(super) last_good_end: usize,
    pub(super) max_seq: u64,
    /// Scan stopped before EOF: corrupt CRC, truncated frame, undecodable
    /// payload, or non-monotonic seq.
    pub(super) truncated_tail: bool,
}

/// Scan v2 frames, applying `seq > applied_seq` frames into `history`
/// without evicting (callers evict once afterwards).
pub(super) fn scan_v2(data: &[u8], history: &mut UserHistory) -> V2Scan {
    let mut scan = V2Scan {
        frames_applied: 0,
        frames_skipped: 0,
        valid_frames: 0,
        last_good_end: WAL_HEADER_LEN,
        max_seq: 0,
        truncated_tail: false,
    };
    let applied_seq = history.applied_seq();
    let mut pos = WAL_HEADER_LEN;
    let mut prev_seq: u64 = 0;
    let mut warned_gap = false;

    while pos < data.len() {
        if pos + FRAME_HEADER_LEN > data.len() {
            scan.truncated_tail = true;
            break;
        }
        let payload_len =
            u32::from_le_bytes(data[pos..pos + 4].try_into().expect("4-byte field")) as usize;
        let expected_crc =
            u32::from_le_bytes(data[pos + 4..pos + 8].try_into().expect("4-byte field"));
        let seq = u64::from_le_bytes(data[pos + 8..pos + 16].try_into().expect("8-byte field"));
        // Checked arithmetic: on 32-bit targets a corrupt payload_len near
        // u32::MAX could wrap `usize` and slip past the bounds check.
        let end = pos
            .checked_add(FRAME_HEADER_LEN)
            .and_then(|v| v.checked_add(payload_len));
        let end = match end {
            Some(end) if payload_len != 0 && end <= data.len() => end,
            _ => {
                scan.truncated_tail = true;
                break;
            }
        };
        if crc32fast::hash(&data[pos + 8..end]) != expected_crc {
            scan.truncated_tail = true;
            break;
        }
        // seq must be strictly increasing within the file; a violation means
        // the frame stream is not trustworthy past this point (§3). Gaps are
        // legal (append failures consume seqs) and only worth a log line.
        if seq <= prev_seq {
            scan.truncated_tail = true;
            break;
        }
        if prev_seq != 0 && seq != prev_seq + 1 && !warned_gap {
            warn!("WAL seq gap: {prev_seq} -> {seq} (benign after append failures)");
            warned_gap = true;
        }

        if seq <= applied_seq {
            // Covered by the checkpoint — skip without decoding.
            scan.frames_skipped += 1;
        } else {
            match bincode_reader(payload_len)
                .deserialize::<WalRecord>(&data[pos + FRAME_HEADER_LEN..end])
            {
                Ok(record) => {
                    history.apply(&record, seq);
                    scan.frames_applied += 1;
                }
                Err(_) => {
                    scan.truncated_tail = true;
                    break;
                }
            }
        }

        prev_seq = seq;
        scan.max_seq = seq;
        scan.valid_frames += 1;
        scan.last_good_end = end;
        pos = end;
    }
    scan
}

pub(super) struct LegacyScan {
    pub(super) frames_applied: u64,
    pub(super) truncated_tail: bool,
}

/// Count CRC-valid v1 frames without applying anything. Used to tell a real
/// v1 (headerless) WAL apart from a v2 WAL whose magic bytes got corrupted:
/// the latter reads as a v1 frame with a ~1 GB length field and yields zero
/// valid frames.
pub(super) fn legacy_valid_prefix(data: &[u8]) -> usize {
    let mut count = 0;
    let mut pos = 0;
    while pos < data.len() {
        if pos + 8 > data.len() {
            break;
        }
        let length =
            u32::from_le_bytes(data[pos..pos + 4].try_into().expect("4-byte field")) as usize;
        let expected_crc =
            u32::from_le_bytes(data[pos + 4..pos + 8].try_into().expect("4-byte field"));
        let end = pos.checked_add(8).and_then(|v| v.checked_add(length));
        let end = match end {
            Some(end) if length != 0 && end <= data.len() => end,
            _ => break,
        };
        if crc32fast::hash(&data[pos + 8..end]) != expected_crc {
            break;
        }
        count += 1;
        pos = end;
    }
    count
}

/// Scan v1 (headerless) frames, applying into `history` without evicting.
pub(super) fn scan_legacy(data: &[u8], history: &mut UserHistory) -> LegacyScan {
    let mut scan = LegacyScan {
        frames_applied: 0,
        truncated_tail: false,
    };
    let mut pos = 0;
    while pos < data.len() {
        if pos + 8 > data.len() {
            scan.truncated_tail = true;
            break;
        }
        let length =
            u32::from_le_bytes(data[pos..pos + 4].try_into().expect("4-byte field")) as usize;
        let expected_crc =
            u32::from_le_bytes(data[pos + 4..pos + 8].try_into().expect("4-byte field"));
        // Checked arithmetic: see scan_v2 — a corrupt length near u32::MAX
        // could wrap `usize` on 32-bit targets.
        let end = pos.checked_add(8).and_then(|v| v.checked_add(length));
        let end = match end {
            Some(end) if length != 0 && end <= data.len() => end,
            _ => {
                scan.truncated_tail = true;
                break;
            }
        };
        let payload = &data[pos + 8..end];
        if crc32fast::hash(payload) != expected_crc {
            scan.truncated_tail = true;
            break;
        }
        match bincode_reader(length).deserialize::<LegacyWalEntry>(payload) {
            Ok(entry) => {
                // v1 frames carry no seq; applied_seq stays untouched at
                // whatever the (v1 = 0) checkpoint set.
                history.apply(
                    &WalRecord::Committed {
                        segments: entry.segments,
                        timestamp: entry.timestamp,
                    },
                    0,
                );
                scan.frames_applied += 1;
            }
            Err(_) => {
                scan.truncated_tail = true;
                break;
            }
        }
        pos = end;
    }
    scan
}

/// Convenience: open checkpoint + replay WAL in one call. Strict and free of
/// side effects — used by offline tooling (`lextool`); the engine uses
/// [`super::recovery::open_recovering`].
pub fn open_with_wal(checkpoint_path: &Path) -> io::Result<(UserHistory, HistoryWal)> {
    use super::persistence::{load_checkpoint, CheckpointLoaded};
    let (mut history, origin) = match load_checkpoint(checkpoint_path)? {
        CheckpointLoaded::V2(h) => (h, CheckpointOrigin::V2),
        CheckpointLoaded::V1(h) => (h, CheckpointOrigin::V1),
        CheckpointLoaded::Missing => (UserHistory::new(), CheckpointOrigin::Missing),
        CheckpointLoaded::Corrupt(e) => return Err(e),
    };
    let mut wal = HistoryWal::new(checkpoint_path);
    wal.replay(&mut history, origin)?;
    Ok((history, wal))
}
