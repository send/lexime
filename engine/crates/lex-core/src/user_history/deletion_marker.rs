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
//! | 6      | 2    | reserved    | **must be 0** in version 1               |
//! | 8      | 8    | witness_seq | u64 LE, set with bit0; 0 is invalid      |
//!
//! "Ignored on read" would be the usual convention for a reserved field, and
//! it is wrong here: `decode` accepts only what `encode` emits, so a non-zero
//! reserved byte resolves to `Lost` like any other unrecognised shape. A later
//! writer that read the field as ignorable and used it would turn every
//! witnessed `Unflushed` into an unconditional lost-deletion warning. Spending
//! it needs a version bump, which is what the version byte is for.
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
    /// seq — the suppression test asks whether a given state has reached this
    /// seq, and the lower of two seqs can be covered while the higher is still
    /// missing.
    ///
    /// Both rules are one-directional: no merge can weaken an outstanding
    /// claim. That is what makes a read-modify-write safe against a concurrent
    /// reader — and, with the write being tmp+rename through
    /// [`crate::persist::write_atomic`], what makes the orphan tmp a crash can
    /// leave harmless, since it can only carry a claim at least as strong.
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
    /// `Lost` always stands. `Unflushed` is settled once the state in question
    /// includes its frame. *Which* state is the caller's choice, and recovery
    /// deliberately asks twice with different ones rather than folding them
    /// into a single comparison: against the **durable checkpoint's**
    /// `applied_seq` to decide retraction, because only a checkpoint on disk
    /// can settle a durability claim, and against the **loaded** `applied_seq`
    /// only to choose between promoting the claim to `Lost` and handing it to
    /// the runtime ledger. A witness that replay satisfied is explicitly not
    /// retracted — replay proves the frame was readable, not that the flush
    /// happened. A frame beyond a repaired tail leaves `applied_seq` short of
    /// the witness, which is the power-loss case and correctly still stands.
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
/// What the marker path holds, and whether the bytes behind it were actually
/// read.
///
/// The two are different questions and conflating them cost a review round.
/// `breach` answers *what is claimed*, under the fail-safe rule that anything
/// unreadable claims `Lost`. `confirmed` answers *do we know that from the
/// file*, and only a successful read and decode sets it. A caller recording
/// what the disk holds — `OpenReport::marker_on_disk`, which seeds the
/// runtime's `flushed` — must use the second: believing a synthesized `Lost`
/// makes every later reconcile skip, leaving a live `Unflushed` witness on
/// disk that nothing will ever promote.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MarkerObservation {
    pub breach: DeletionBreach,
    pub confirmed: bool,
}

impl MarkerObservation {
    /// What the disk holds, as a belief: only a confirmed read can name bytes.
    pub fn state(self) -> MarkerState {
        if self.confirmed {
            MarkerState::Holds(self.breach)
        } else {
            MarkerState::Unknown
        }
    }
}

/// What a process believes the marker path holds.
///
/// Three states, because reality has three and an `Option` has two. The
/// missing one is `Unknown` — a file is there and nobody has managed to read
/// it — and collapsing that into "absent" is what let a failed unlink of an
/// unreadable marker look settled: the projection found the disk already in
/// the desired state, skipped, and the surviving file reported a lost deletion
/// on the next start. A `confirmed` bit bolted onto the *read* was the
/// half-measure; the belief itself has to carry it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum MarkerState {
    /// Nothing there — an observed absence, or a removal that succeeded.
    #[default]
    Absent,
    /// These exact bytes are there.
    Holds(DeletionBreach),
    /// A file is there and nobody has read it. Never equal to any desired
    /// state, so a projection can never conclude it has nothing to do — which
    /// is the entire point of the variant.
    Unknown,
}

impl MarkerState {
    /// Whether the disk already says at least what is wanted. `Unknown` never
    /// does.
    ///
    /// "At least", not "exactly", and the merge lattice decides it: claims are
    /// one-directional, so a `Lost` on disk covers a desired `Unflushed` — it
    /// reports more, never less, which is the only direction this format is
    /// allowed to fail in. Exact equality deadlocked instead: with a stale
    /// `Lost` surviving and a later `SyncFailed` wanting `Unflushed`,
    /// `merge_write` absorbs the request back into `Lost` every time, so the
    /// desired state was unreachable and every commit paid another key-thread
    /// full sync without converging.
    pub fn satisfies(self, desired: Option<DeletionBreach>) -> bool {
        match (self, desired) {
            (Self::Absent, None) => true,
            (Self::Holds(held), Some(want)) => held.merge(want) == held,
            _ => false,
        }
    }
}

pub fn read(checkpoint_path: &Path) -> Option<MarkerObservation> {
    let path = marker_path(checkpoint_path);
    // The marker *and* any orphan tmp beside it. A crash between the tmp's
    // flush and the rename leaves the stronger claim in the sibling, and
    // reading only the marker would hand the next startup a witness it can
    // suppress. Merging makes the orphan able to strengthen the claim and
    // never to weaken it, which is what lets this go back through the shared
    // atomic write instead of a hand-rolled in-place one.
    let claims = [read_at(&path), read_at(&persist::tmp_path(&path))];
    claims
        .into_iter()
        .flatten()
        .map(|(bytes, readable)| MarkerObservation {
            breach: DeletionBreach::decode(&bytes),
            confirmed: readable,
        })
        .reduce(|a, b| MarkerObservation {
            breach: a.breach.merge(b.breach),
            // Both halves, since either one being a guess makes the pair a
            // guess about what the path as a whole holds.
            confirmed: a.confirmed && b.confirmed,
        })
}

/// One file's bytes, under the fail-safe rule: `None` means — and only means —
/// there is no file, and anything unreadable comes back as a buffer that
/// [`DeletionBreach::decode`] resolves to `Lost`.
fn read_at(path: &Path) -> Option<(Vec<u8>, bool)> {
    // Through one descriptor, not a `symlink_metadata` check followed by an
    // open: those are two pathname resolutions, and a restore or a sync tool
    // replacing the checked regular file with a FIFO in between leaves the
    // blocking open exactly where it was. This runs synchronously inside
    // `LexUserHistory::open`, on the thread the IME starts up on, so that open
    // never returning means the input method never becomes available.
    // `open_regular` resolves the name once and validates what it got.
    let mut file = match persist::open_regular(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return None,
        Err(e) => {
            warn!("unpersisted-deletion marker unreadable ({e}); reporting conservatively");
            return Some((Vec::new(), false));
        }
    };
    // LEN + 1, so a longer file is *seen* to be longer rather than read as a
    // well-formed prefix — the round-trip in `decode` then rejects it.
    let mut buf = Vec::with_capacity(LEN + 1);
    match io::Read::read_to_end(&mut io::Read::take(&mut file, LEN as u64 + 1), &mut buf) {
        Ok(_) => Some((buf, true)),
        Err(e) => {
            warn!("unpersisted-deletion marker unreadable ({e}); reporting conservatively");
            Some((Vec::new(), false))
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
/// **Through [`write_atomic`], the shared primitive.** A revision of this file
/// wrote in place instead, on the argument that a torn marker decodes to
/// `Lost` so atomicity buys nothing, and that tmp+rename's crash window hides
/// a stronger claim in a sibling. The first half was true and the second was
/// the wrong fix: hiding is what [`read`] now prevents by merging the orphan,
/// which can only strengthen a claim. What writing by hand cost was everything
/// `write_atomic` already encodes — three review rounds re-derived it one
/// durability detail at a time (a symlink at the path being followed and its
/// target truncated, an unlink failure left unchecked before `File::create`, a
/// newly created directory entry never fsynced so power loss could drop the
/// filename while its contents were durable). `rename` replaces a symlink or a
/// FIFO at the destination rather than writing through it, and syncs the
/// parent. `persist`'s own module doc calls these the single-source
/// implementations "so the stores cannot drift apart on the durability
/// details"; this file drifted, and is back.
///
/// Callers hold the wal mutex, which serializes the read against a concurrent
/// write. The one exception is recovery's promotion of an unsatisfied witness,
/// which runs before the `HistoryWal` enters its mutex and is exclusive by
/// ownership rather than by locking.
///
/// Measured on an M4 (release, APFS) at **p50 10.1ms**, against **12.3ms p50**
/// for the synchronous fallback checkpoint the same call runs immediately
/// afterwards. The caller skips a projection whose value it has already
/// flushed, so the cost is per *change* of claim, not per raise.
///
/// A barrier flush instead of `sync_all` would cost ~0.3ms and would still
/// cover the scenario #312 is named for (a process restart keeps the page
/// cache), but it would reopen a power-loss window in the *report* about a
/// deletion whose own power-loss window §6 sets to zero.
pub fn merge_write(checkpoint_path: &Path, breach: DeletionBreach) -> io::Result<DeletionBreach> {
    // The claim only — whether the existing bytes were readable does not
    // change what has to be written, and merging is one-directional so an
    // unreadable existing marker (conservatively `Lost`) can only strengthen.
    let merged = read(checkpoint_path).map_or(breach, |existing| existing.breach.merge(breach));
    let image = merged.encode();
    if let Err(e) = persist::write_atomic_staged(&marker_path(checkpoint_path), &image) {
        // The logical marker is the canonical file **and** its orphan tmp —
        // `read` merges them and `remove` clears both — and that rule holds
        // here too: `write_atomic` flushes the tmp before renaming, so a
        // rename that fails (a non-empty directory sitting at the canonical
        // name, say) has still made the claim durable where the next `read`
        // will find it. Reporting failure there made the claim look unlanded
        // forever — appends froze on every commit, each compaction thawed
        // them, and the next keystroke froze again.
        //
        // Which stage failed is taken from the **writer**, never inferred. An
        // earlier version compared the bytes back and accepted a match: that
        // cannot tell a flushed image from one that only reached the page
        // cache before `sync_all` failed, since both compare equal — and
        // calling the second durable is exactly the window this file exists to
        // close.
        match e {
            persist::AtomicWriteFailure::FlushedNotRenamed(e) => {
                warn!("marker rename failed ({e}); the flushed orphan carries the claim");
            }
            persist::AtomicWriteFailure::NotDurable(e) => return Err(e),
        }
    }
    Ok(merged)
}

/// Remove the marker, reporting whether the record is now gone.
///
/// **Both files.** [`read`] merges the atomic write's orphan tmp so that a
/// crash between its flush and the rename can only strengthen a claim; the
/// logical marker is therefore the pair, and removing one of them would leave
/// the claim standing. A `Lost` orphan would then re-report on every launch
/// with no acknowledgement able to clear it — the unclearable latch, rebuilt
/// out of the fix for hiding.
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
/// A **directory** at either path is removed only when empty. It is not ours:
/// something external put it there, and `remove_dir_all` would both walk an
/// unbounded tree on the thread the menu runs on and delete whatever a restore
/// had placed inside. An empty one is a placeholder and safe to clear; a full
/// one stays, and the `false` return makes that visible instead of silent —
/// which is the honest form of the "unclearable latch" this fallback was added
/// to prevent, since the user is now told the acknowledgement did not take.
pub fn remove(checkpoint_path: &Path) -> bool {
    let path = marker_path(checkpoint_path);
    // Both, and `&` not `&&`: the orphan must be attempted even when the
    // canonical marker refuses, or a `false` return would leave a claim the
    // caller was never told about.
    remove_one(&path) & remove_one(&persist::tmp_path(&path))
}

fn remove_one(path: &Path) -> bool {
    let Err(e) = fs::remove_file(path) else {
        return true;
    };
    if e.kind() == io::ErrorKind::NotFound {
        return true;
    }
    if fs::remove_dir(path).is_ok() {
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
