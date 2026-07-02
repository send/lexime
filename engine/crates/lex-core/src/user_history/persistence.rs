//! LXUD checkpoint serialization: v2 writer + v1/v2 readers.
//!
//! v2 layout — fixed 32-byte header followed by a bincode body:
//!
//! | offset | size | field       | content                                    |
//! |--------|------|-------------|--------------------------------------------|
//! | 0      | 4    | magic       | `LXUD`                                     |
//! | 4      | 1    | version     | `2`                                        |
//! | 5      | 3    | reserved    | 0 on write, ignored on read                |
//! | 8      | 8    | applied_seq | u64 LE — last WAL seq covered by the body  |
//! | 16     | 8    | created_at  | u64 LE epoch secs, diagnostics only        |
//! | 24     | 4    | body_len    | u32 LE                                     |
//! | 28     | 4    | crc32       | u32 LE, crc32fast over bytes 0..28 + body  |
//! | 32     | —    | body        | bincode(`UserHistoryData`) — same as v1    |
//!
//! The CRC covers the header prefix as well as the body: `applied_seq`
//! drives the replay filter, and an unprotected bit flip there would
//! silently skip (or re-apply) WAL frames instead of quarantining.
//!
//! v1 (retained reader, ~30 lines): `LXUD` + version byte `1` + bincode body.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use bincode::Options as _;

use super::{BigramRecord, HistoryEntry, UnigramRecord, UserHistory, UserHistoryData};

pub(super) const MAGIC: &[u8; 4] = b"LXUD";
pub(super) const VERSION_V1: u8 = 1;
pub(super) const VERSION: u8 = 2;
pub(super) const HEADER_LEN: usize = 32;

/// bincode config matching the wire format of `bincode::serialize` (fixint
/// encoding, trailing bytes tolerated) plus an explicit allocation cap.
/// The plain `bincode::deserialize` is unlimited: a corrupt length prefix
/// inside the body could trigger a giant allocation before any data is read.
pub(super) fn bincode_reader(limit: usize) -> impl bincode::Options {
    bincode::options()
        .with_fixint_encoding()
        .allow_trailing_bytes()
        .with_limit(limit as u64)
}

fn invalid(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

/// Result of reading a checkpoint file. Content problems are reported as
/// `Corrupt` so the recovery path can quarantine; environmental read failures
/// (EACCES etc.) surface as the outer `Err`.
pub(super) enum CheckpointLoaded {
    V2(UserHistory),
    V1(UserHistory),
    Missing,
    Corrupt(io::Error),
}

pub(super) fn load_checkpoint(path: &Path) -> io::Result<CheckpointLoaded> {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(CheckpointLoaded::Missing),
        Err(e) => return Err(e),
    };
    if bytes.len() < 5 {
        return Ok(CheckpointLoaded::Corrupt(invalid("too short")));
    }
    if &bytes[0..4] != MAGIC {
        return Ok(CheckpointLoaded::Corrupt(invalid("bad magic")));
    }
    match bytes[4] {
        VERSION_V1 => match UserHistory::from_bytes_v1(&bytes) {
            Ok(h) => Ok(CheckpointLoaded::V1(h)),
            Err(e) => Ok(CheckpointLoaded::Corrupt(e)),
        },
        VERSION => match UserHistory::from_bytes_v2(&bytes) {
            Ok(h) => Ok(CheckpointLoaded::V2(h)),
            Err(e) => Ok(CheckpointLoaded::Corrupt(e)),
        },
        // Unknown versions are "corrupt" (quarantine + fallback), never a
        // hard error: the bytes are preserved by the rename, so a downgrade
        // can still rescue them (§7 version-bump policy).
        v => Ok(CheckpointLoaded::Corrupt(invalid(format!(
            "unsupported version {v}"
        )))),
    }
}

/// Atomic write with content durability: tmp → sync_all (F_FULLFSYNC on
/// macOS) → rename → best-effort parent-dir fsync. Without the tmp sync,
/// the rename can become durable before the file contents, manufacturing a
/// corrupt checkpoint on power loss. The parent-dir fsync only makes the
/// rename itself durable and its failure is deliberately ignored (POSIX
/// practice; APFS journals renames anyway) — the worst case is rolling back
/// to the previous checkpoint, never corruption. The tmp name appends
/// `.tmp` to the full file name (`with_extension` would strip `.lxud` and
/// leave a stray `user_history.tmp`).
pub(super) fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = tmp_path(path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut f = File::create(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    drop(f);
    fs::rename(&tmp, path)?;
    sync_parent_dir(path);
    Ok(())
}

fn tmp_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    path.with_file_name(name)
}

/// Best-effort fsync of the parent directory so the rename itself is
/// durable. APFS likely journals renames already; this is POSIX practice on
/// a background path, so it costs nothing and failures are non-fatal.
fn sync_parent_dir(path: &Path) {
    let Some(parent) = path.parent() else { return };
    if let Ok(dir) = File::open(parent) {
        let _ = dir.sync_all();
    }
}

impl UserHistory {
    /// Serialize to bytes (LXUD v2 format).
    pub fn to_bytes(&self) -> Result<Vec<u8>, io::Error> {
        let data = self.to_data();
        let body = bincode::serialize(&data).map_err(io::Error::other)?;
        let body_len = u32::try_from(body.len())
            .map_err(|_| io::Error::other("history body too large (>4 GiB)"))?;

        let mut buf = Vec::with_capacity(HEADER_LEN + body.len());
        buf.extend_from_slice(MAGIC);
        buf.push(VERSION);
        buf.extend_from_slice(&[0u8; 3]);
        buf.extend_from_slice(&self.applied_seq.to_le_bytes());
        buf.extend_from_slice(&super::now_epoch().to_le_bytes());
        buf.extend_from_slice(&body_len.to_le_bytes());
        // CRC over the header prefix + body (see module docs: applied_seq
        // must not be trusted unprotected).
        let mut hasher = crc32fast::Hasher::new();
        hasher.update(&buf);
        hasher.update(&body);
        buf.extend_from_slice(&hasher.finalize().to_le_bytes());
        buf.extend_from_slice(&body);
        Ok(buf)
    }

    /// Deserialize from bytes (LXUD v1 or v2). Strict: any content problem
    /// is an error — recovery-mode handling lives in [`super::recovery`].
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, io::Error> {
        if bytes.len() < 5 {
            return Err(invalid("too short"));
        }
        if &bytes[0..4] != MAGIC {
            return Err(invalid("bad magic"));
        }
        match bytes[4] {
            VERSION_V1 => Self::from_bytes_v1(bytes),
            VERSION => Self::from_bytes_v2(bytes),
            v => Err(invalid(format!("unsupported version {v}"))),
        }
    }

    pub(super) fn from_bytes_v1(bytes: &[u8]) -> Result<Self, io::Error> {
        let body = &bytes[5..];
        let data: UserHistoryData = bincode_reader(body.len())
            .deserialize(body)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        Ok(Self::from_data(data))
    }

    pub(super) fn from_bytes_v2(bytes: &[u8]) -> Result<Self, io::Error> {
        if bytes.len() < HEADER_LEN {
            return Err(invalid("truncated v2 header"));
        }
        let applied_seq = u64::from_le_bytes(bytes[8..16].try_into().expect("8-byte field"));
        // bytes[16..24]: created_at, diagnostics only.
        let body_len = u32::from_le_bytes(bytes[24..28].try_into().expect("4-byte field")) as usize;
        let expected_crc = u32::from_le_bytes(bytes[28..32].try_into().expect("4-byte field"));
        // Strict equality: tmp+rename writes never legitimately leave
        // trailing garbage, so excess length is a bug worth surfacing.
        if bytes.len() != HEADER_LEN + body_len {
            return Err(invalid(format!(
                "body length mismatch: header says {body_len}, file has {}",
                bytes.len() - HEADER_LEN
            )));
        }
        let body = &bytes[HEADER_LEN..];
        let mut hasher = crc32fast::Hasher::new();
        hasher.update(&bytes[0..28]);
        hasher.update(body);
        if hasher.finalize() != expected_crc {
            return Err(invalid("checkpoint CRC mismatch"));
        }
        let data: UserHistoryData = bincode_reader(body_len)
            .deserialize(body)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let mut history = Self::from_data(data);
        history.applied_seq = applied_seq;
        Ok(history)
    }

    /// Atomic durable write (see [`write_atomic`]).
    pub fn save(&self, path: &Path) -> Result<(), io::Error> {
        let bytes = self.to_bytes()?;
        write_atomic(path, &bytes)
    }

    /// Open from file, returning empty UserHistory if file doesn't exist.
    /// Strict (no side effects, corrupt = `Err`): offline tooling must never
    /// mutate a live user's files. The engine uses
    /// [`super::recovery::open_recovering`] instead.
    pub fn open(path: &Path) -> Result<Self, io::Error> {
        match fs::read(path) {
            Ok(bytes) => Self::from_bytes(&bytes),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::new()),
            Err(e) => Err(e),
        }
    }

    pub(super) fn to_data(&self) -> UserHistoryData {
        let mut unigrams = Vec::new();
        for (reading, inner) in &self.unigrams {
            for (surface, entry) in inner {
                unigrams.push(UnigramRecord {
                    reading: reading.clone(),
                    surface: surface.clone(),
                    frequency: entry.frequency,
                    last_used: entry.last_used,
                });
            }
        }

        let mut bigrams = Vec::new();
        for (prev, inner) in &self.bigrams {
            for ((next_r, next_s), entry) in inner {
                bigrams.push(BigramRecord {
                    prev_surface: prev.clone(),
                    next_reading: next_r.clone(),
                    next_surface: next_s.clone(),
                    frequency: entry.frequency,
                    last_used: entry.last_used,
                });
            }
        }

        UserHistoryData { unigrams, bigrams }
    }

    pub(super) fn from_data(data: UserHistoryData) -> Self {
        let mut unigrams: std::collections::HashMap<
            String,
            std::collections::HashMap<String, HistoryEntry>,
        > = std::collections::HashMap::new();
        for rec in data.unigrams {
            unigrams.entry(rec.reading).or_default().insert(
                rec.surface,
                HistoryEntry {
                    frequency: rec.frequency,
                    last_used: rec.last_used,
                },
            );
        }

        let mut bigrams: std::collections::HashMap<
            String,
            std::collections::HashMap<(String, String), HistoryEntry>,
        > = std::collections::HashMap::new();
        for rec in data.bigrams {
            bigrams.entry(rec.prev_surface).or_default().insert(
                (rec.next_reading, rec.next_surface),
                HistoryEntry {
                    frequency: rec.frequency,
                    last_used: rec.last_used,
                },
            );
        }

        Self {
            unigrams,
            bigrams,
            applied_seq: 0,
        }
    }
}
