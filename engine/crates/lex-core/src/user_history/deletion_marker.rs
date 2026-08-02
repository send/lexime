//! Sidecar marker: a deletion the user asked for did not reach disk (#312).
//!
//! The runtime durability ledger lives in an atomic on `LexUserHistory`, so it
//! dies with the process — and the `Io` half of `DeletionNotPersisted` has no
//! startup heal: the old checkpoint still holds the entry and wins on the next
//! start. The report was therefore gone on the very restart where the deletion
//! resurrects. This file is the ledger's on-disk projection, so the fact
//! survives to the launch that materialises it.
//!
//! Layout — 16 fixed bytes, no CRC (see "fail-safe" below):
//!
//! | offset | size | field       | content                                  |
//! |--------|------|-------------|------------------------------------------|
//! | 0      | 4    | magic       | `LXDM`                                   |
//! | 4      | 1    | version     | `1`                                      |
//! | 5      | 1    | flags       | bit0 = a witness seq follows             |
//! | 6      | 2    | reserved    | 0 on write, ignored on read              |
//! | 8      | 8    | witness_seq | u64 LE, meaningful only when bit0 is set |
//!
//! **Fail-safe by construction: only `NotFound` means clean.** A read error, a
//! short file, a bad magic, an unknown version — every outcome other than
//! "there is no file" resolves to the strongest claim ([`DeletionBreach::Lost`],
//! reported unconditionally). Suppressing a report is the only direction that
//! demands a well-formed witness, which is why no CRC is needed: corruption can
//! only push the marker toward reporting. It is also why reading never returns
//! an error to the caller — surfacing one would let a sidecar nobody can read
//! fail the whole history open, i.e. stop learning outright.
//!
//! Deliberately holds **no** input strings: which entry was deleted is exactly
//! the text the deletion was meant to erase. The witness is a WAL seq.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use tracing::warn;

use crate::persist;

const MAGIC: &[u8; 4] = b"LXDM";
const VERSION: u8 = 1;
const LEN: usize = 16;
const FLAG_WITNESS: u8 = 0b0000_0001;

/// A deletion whose durability failed, in the form the next startup needs.
///
/// The two halves differ in whether a restart heals them, which is the whole
/// reason the witness exists: reporting the healed half would be a latching
/// privacy alarm about data that is, in fact, gone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeletionBreach {
    /// No durable representation at all: the WAL append failed, so no frame
    /// exists, and the synchronous checkpoint fallback failed too. The old
    /// checkpoint still holds the entry and no restart heals it.
    Lost,
    /// The frame reached the WAL at `seq` but its flush was not confirmed. A
    /// plain restart replays it (the deletion takes); only power loss can
    /// still undo it — which is what makes the seq worth recording.
    Unflushed { seq: u64 },
}

impl DeletionBreach {
    /// Combine two breaches into the claim that covers both.
    ///
    /// `Lost` absorbs: one deletion with no durable representation is not made
    /// healable by another that has a frame. Two `Unflushed` keep the **max**
    /// seq — the suppression test asks "did the loaded state reach this seq",
    /// and the lower of two seqs can be covered while the higher is still
    /// missing.
    ///
    /// Both rules are one-directional, which is what lets the marker be
    /// rewritten in place: no merge can weaken an outstanding claim.
    pub fn merge(self, other: Self) -> Self {
        match (self, other) {
            (Self::Lost, _) | (_, Self::Lost) => Self::Lost,
            (Self::Unflushed { seq: a }, Self::Unflushed { seq: b }) => {
                Self::Unflushed { seq: a.max(b) }
            }
        }
    }

    /// Whether this breach still stands against a state that has replayed up
    /// to `applied_seq`.
    ///
    /// `Lost` always stands. `Unflushed` is settled once the loaded state
    /// includes its frame — `applied_seq` after replay is
    /// `max(checkpoint.applied_seq, last replayed seq)`, so one comparison
    /// answers both "the checkpoint already covered it" and "replay applied
    /// it". A frame beyond a repaired tail leaves `applied_seq` short of the
    /// witness, which is the power-loss case and correctly still stands.
    pub fn outstanding(self, applied_seq: u64) -> bool {
        match self {
            Self::Lost => true,
            Self::Unflushed { seq } => seq > applied_seq,
        }
    }

    fn encode(self) -> [u8; LEN] {
        let mut buf = [0u8; LEN];
        buf[0..4].copy_from_slice(MAGIC);
        buf[4] = VERSION;
        if let Self::Unflushed { seq } = self {
            buf[5] = FLAG_WITNESS;
            buf[8..16].copy_from_slice(&seq.to_le_bytes());
        }
        buf
    }

    /// Total decode: every malformed input resolves to `Lost`, never a panic.
    /// Reached from `#[uniffi::constructor]`, where a slice panic would cross
    /// the FFI boundary.
    fn decode(bytes: &[u8]) -> Self {
        if bytes.len() < LEN || &bytes[0..4] != MAGIC || bytes[4] != VERSION {
            return Self::Lost;
        }
        if bytes[5] & FLAG_WITNESS == 0 {
            return Self::Lost;
        }
        Self::Unflushed {
            seq: u64::from_le_bytes(bytes[8..16].try_into().expect("8-byte field")),
        }
    }
}

/// Path of the marker for a history family (`<checkpoint>.deletion-pending`).
///
/// Suffixed, not `with_extension`: the family shares the checkpoint's full
/// file name so quarantine rotation and the clear sweep keep matching it.
/// It does **not** contain `.corrupt-`, so [`persist::quarantined_files`]
/// never picks it up (pinned by a test that calls the real predicate).
///
/// `pub` for fault injection: a test in a dependent crate needs to name the
/// path to plant an obstacle at it. Nothing in production derives it outside
/// this module.
pub fn marker_path(checkpoint_path: &Path) -> PathBuf {
    persist::suffixed(checkpoint_path, ".deletion-pending")
}

/// Read the marker. `None` means — and only means — there is no file.
///
/// See the module docs: every other outcome, including an unreadable file, is
/// [`DeletionBreach::Lost`].
///
/// Reads at most [`LEN`] bytes rather than `fs::read`, which pre-sizes its
/// buffer from the file's length: whatever sits at this path is attacker-free
/// but not size-checked, and this crate already holds the line that a length
/// taken from disk must not size an allocation (see `persist`'s bincode
/// readers). A longer file is malformed anyway — `decode` needs the first
/// [`LEN`] bytes and nothing else.
pub fn read(checkpoint_path: &Path) -> Option<DeletionBreach> {
    let path = marker_path(checkpoint_path);
    let mut file = match fs::File::open(&path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return None,
        Err(e) => {
            warn!("unpersisted-deletion marker unreadable ({e}); reporting conservatively");
            return Some(DeletionBreach::Lost);
        }
    };
    let mut buf = Vec::with_capacity(LEN);
    match io::Read::read_to_end(&mut io::Read::take(&mut file, LEN as u64), &mut buf) {
        Ok(_) => Some(DeletionBreach::decode(&buf)),
        Err(e) => {
            warn!("unpersisted-deletion marker unreadable ({e}); reporting conservatively");
            Some(DeletionBreach::Lost)
        }
    }
}

/// Merge `breach` into whatever the marker already claims and write it back.
///
/// Read-modify-write, not a plain overwrite. A full replacement is
/// last-write-wins, and an `Unflushed` landing on top of an outstanding `Lost`
/// would downgrade the claim to one the next startup can suppress — losing
/// exactly the report this file exists for.
///
/// The reachable route to that ordering is not the obvious one, and stating it
/// wrongly is how the first version of this file grew a test that proved
/// nothing. An `Io` append *freezes* the WAL, and the frozen guard turns every
/// later append in the session into `Io` too — so `Lost` cannot be followed by
/// an `Unflushed` while the freeze holds. What lifts it is a compaction whose
/// `snapshot_to_cover` generation predates the `Io` raise: it early-returns
/// from the cover (leaving the marker outstanding) yet still reaches
/// `truncate_covered`, which thaws the file. The next tombstone can then append
/// and fail its flush, arriving as `Unflushed` on top of a standing `Lost`.
///
/// **Written in place — deliberately not through [`write_atomic`].** The usual
/// reason for tmp+rename is that a torn file is worse than an old one; here it
/// is the opposite, because a torn marker decodes to `Lost`, the strongest
/// claim this file can make. What tmp+rename does buy is a window: a crash
/// between the tmp's flush and the rename leaves the stronger claim in a
/// sibling that `read` does not consult and the next `remove` deletes, so the
/// deletion goes unreported — the exact outcome this file exists to prevent.
/// Writing in place has no such intermediate object, and costs one flush
/// instead of two.
///
/// Callers hold the wal mutex, which is what serializes the read against a
/// concurrent write — and that mutex is held by the key-processing thread, so
/// this lands on the ForwardDelete path. Measured on an M4 (release, APFS):
/// **p50 4.0ms / p95 5.0ms**, against **12.3ms p50** for the synchronous
/// fallback checkpoint (5k entries) the same call runs immediately afterwards.
/// The tmp+rename form this replaced measured 10.1ms p50 — dropping the second
/// flush, the rename and the directory fsync is most of the difference. It only
/// ever runs when a tombstone failed to reach the disk.
///
/// A barrier flush instead of `sync_all` would cost ~0.3ms and would still
/// cover the scenario #312 is named for (a process restart keeps the page
/// cache), but it would reopen a power-loss window in the *report* about a
/// deletion whose own power-loss window §6 sets to zero.
pub fn merge_write(checkpoint_path: &Path, breach: DeletionBreach) -> io::Result<()> {
    let merged = read(checkpoint_path).map_or(breach, |existing| existing.merge(breach));
    persist::ensure_parent_dir(checkpoint_path)?;
    let mut f = fs::File::create(marker_path(checkpoint_path))?;
    io::Write::write_all(&mut f, &merged.encode())?;
    f.sync_all()
}

/// Remove the marker.
///
/// Best-effort: it holds no user text, so a failure to unlink is worth a log
/// line and nothing more, and what remains is re-reported on the next start —
/// the safe direction. Retrying is deliberately not attempted; the disk this
/// runs against is the one that just failed.
///
/// Falls back to removing a *directory* at the path. That is not defensive
/// noise: `read` resolves an unreadable path to `Lost`, so anything that leaves
/// a non-file here — a sync tool, a restore — would otherwise report a lost
/// deletion on every launch with no way to clear it, since every retraction
/// path in the system clears it by unlinking. A latch the user is told to
/// resolve but cannot is worse than the over-report it came from. Scoped to
/// this one derived path, which the engine owns.
pub fn remove(checkpoint_path: &Path) {
    let path = marker_path(checkpoint_path);
    let Err(e) = fs::remove_file(&path) else {
        return;
    };
    if e.kind() == io::ErrorKind::NotFound {
        return;
    }
    if fs::remove_dir_all(&path).is_ok() {
        warn!(
            "removed a directory left at the marker path {}",
            path.display()
        );
        return;
    }
    warn!(
        "failed to remove unpersisted-deletion marker {}: {e}",
        path.display()
    );
}
