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
//! | 8      | 8    | witness_seq | u64 LE, set with bit0; 0 is invalid      |
//!
//! **Fail-safe by construction: only `NotFound` means clean.** A read error, a
//! bad magic, an unknown version, any length other than [`LEN`], a witness of
//! 0 — every outcome other than "there is no file" resolves to the strongest
//! claim ([`DeletionBreach::Lost`], reported unconditionally). The last two are
//! the load-bearing ones, because they are the shapes that would otherwise
//! resolve toward *suppression*: a longer file read as a well-formed prefix,
//! and a zero seq that no applied_seq can fail to cover. Suppressing a report is the only direction that
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

    /// Total decode: **only what [`Self::encode`] can produce is accepted**;
    /// everything else is `Lost`, and nothing panics.
    ///
    /// Written as a round-trip against the writer rather than as a series of
    /// field checks. Field checks are what this was, and they were wrong four
    /// times in a row — a length other than [`LEN`], an unknown version, a
    /// witness of 0, an unrecognized flags byte — each found separately, each
    /// the same bug: a byte the writer never emits, read as if it meant
    /// something. Comparing against the encoding leaves no unchecked byte, so
    /// there is no fifth one to forget. It also rejects a non-zero reserved
    /// field, a witness flag with no seq, and a seq with no witness flag,
    /// none of which anyone had thought to name.
    ///
    /// Reached from `#[uniffi::constructor]`, where a slice panic would cross
    /// the FFI boundary.
    fn decode(bytes: &[u8]) -> Self {
        let Ok(exact) = <[u8; LEN]>::try_from(bytes) else {
            return Self::Lost;
        };
        if exact == Self::Lost.encode() {
            return Self::Lost;
        }
        // Seq 0 cannot round-trip: `encode` writes it only for `Lost`, which
        // the comparison above already claimed.
        let seq = u64::from_le_bytes(exact[8..16].try_into().expect("8-byte field"));
        let witnessed = Self::Unflushed { seq };
        if seq != 0 && exact == witnessed.encode() {
            return witnessed;
        }
        Self::Lost
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
/// [`LEN`] bytes, and a file that has more of them is not this format.
pub fn read(checkpoint_path: &Path) -> Option<DeletionBreach> {
    read_raw(checkpoint_path).map(|bytes| DeletionBreach::decode(&bytes))
}

/// The marker's bytes, under the same rules as [`read`]: `None` means — and
/// only means — there is no file, and anything unreadable comes back as a
/// buffer that [`DeletionBreach::decode`] resolves to `Lost`.
fn read_raw(checkpoint_path: &Path) -> Option<Vec<u8>> {
    let path = marker_path(checkpoint_path);
    // Ask what is there before opening it. A FIFO left at this path by a
    // restore or a sync tool would make a read-only `File::open` block until
    // someone opens the other end — and this runs synchronously inside
    // `LexUserHistory::open`, on the thread the IME starts up on, so the
    // input method would simply never become available. `symlink_metadata`
    // rather than `metadata`: a symlink pointing at a FIFO is the same trap.
    match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return None,
        Err(e) => {
            warn!("unpersisted-deletion marker unreadable ({e}); reporting conservatively");
            return Some(Vec::new());
        }
        Ok(meta) if !meta.file_type().is_file() => {
            warn!(
                "unpersisted-deletion marker at {} is not a regular file; reporting conservatively",
                path.display()
            );
            return Some(Vec::new());
        }
        Ok(_) => {}
    }
    let mut file = match fs::File::open(&path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return None,
        Err(e) => {
            warn!("unpersisted-deletion marker unreadable ({e}); reporting conservatively");
            return Some(Vec::new());
        }
    };
    // LEN + 1, so a longer file is *seen* to be longer rather than read as a
    // well-formed prefix — the round-trip in `decode` then rejects it.
    let mut buf = Vec::with_capacity(LEN + 1);
    match io::Read::read_to_end(&mut io::Read::take(&mut file, LEN as u64 + 1), &mut buf) {
        Ok(_) => Some(buf),
        Err(e) => {
            warn!("unpersisted-deletion marker unreadable ({e}); reporting conservatively");
            Some(Vec::new())
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
/// Callers must hold the wal mutex, which is what serializes the read against
/// a concurrent write. The one exception is recovery's promotion of an
/// unsatisfied witness, which runs before the `HistoryWal` enters its mutex and
/// is therefore exclusive by ownership rather than by locking. That mutex is
/// held by the key-processing thread, so
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
    let existing = read_raw(checkpoint_path);
    let merged = existing
        .as_deref()
        .map_or(breach, |bytes| DeletionBreach::decode(bytes).merge(breach));
    let encoded = merged.encode();
    // Deliberately no "the bytes already match, skip" short-circuit here.
    // Matching bytes prove the content reached the page cache, not that it was
    // flushed — so a failed `sync_all` would read back as up-to-date and never
    // be retried, reopening in silence the power-loss window this function
    // refuses to open for a 0.3ms saving. The caller skips redundant writes
    // instead, keyed on having *successfully flushed* the value.
    persist::ensure_parent_dir(checkpoint_path)?;
    let path = marker_path(checkpoint_path);

    // The writer owns this path: whatever else is there gets *replaced*, never
    // written through. `File::create` follows symlinks, so without this a link
    // left by a restore would have the engine truncate and overwrite a file it
    // has no business touching — or block forever on a link to a FIFO, inside
    // the synchronous deletion path. Unlinking removes the link itself.
    //
    // It also unlinks a marker the engine cannot rewrite. That is the
    // difference between a read-only *file* and a read-only *directory*:
    // removing an entry needs write permission on the parent, not on the file,
    // so a marker that refuses `create` can still be replaced with a fresh
    // one. Without it, a promotion to `Lost` that failed left a witness on
    // disk that later, re-based sequence numbers could satisfy — silencing a
    // report that was still owed. What remains is the directory-level failure
    // this design already documents as unclosable.
    let replace_first = match fs::symlink_metadata(&path) {
        Ok(meta) => !meta.file_type().is_file(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => false,
        Err(_) => true,
    };
    if replace_first {
        let _ = fs::remove_file(&path);
        let _ = fs::remove_dir(&path);
    }
    let mut f = match fs::File::create(&path) {
        Ok(f) => f,
        Err(create_err) => {
            // Present but not writable: take the entry out and start over.
            if fs::remove_file(&path).is_err() {
                return Err(create_err);
            }
            fs::File::create(&path)?
        }
    };
    io::Write::write_all(&mut f, &encoded)?;
    f.sync_all()
}

/// Remove the marker, reporting whether the path is now clear.
///
/// `true` means the record is gone — removed, or never there. `false` means it
/// stands, and the caller must keep telling the user so: an acknowledgement
/// that says it succeeded while the marker survives drops the row, takes away
/// the retry, and lets the warning come back on the next launch anyway.
///
/// Best-effort in the sense that it does not retry — the disk this runs
/// against is the one that just failed — but never in the sense of hiding the
/// outcome.
///
/// A **directory** at the path is removed only when empty. It is not ours:
/// something external put it there, and `remove_dir_all` would both walk an
/// unbounded tree on the thread the menu runs on and delete whatever a restore
/// had placed inside. An empty one is a placeholder and safe to clear; a full
/// one stays, and the `false` return makes that visible instead of silent —
/// which is the honest form of the "unclearable latch" this fallback was added
/// to prevent, since the user is now told the acknowledgement did not take.
pub fn remove(checkpoint_path: &Path) -> bool {
    let path = marker_path(checkpoint_path);
    let Err(e) = fs::remove_file(&path) else {
        return true;
    };
    if e.kind() == io::ErrorKind::NotFound {
        return true;
    }
    if fs::remove_dir(&path).is_ok() {
        warn!(
            "removed an empty directory left at the marker path {}",
            path.display()
        );
        return true;
    }
    warn!(
        "failed to remove unpersisted-deletion marker {}: {e}",
        path.display()
    );
    false
}
