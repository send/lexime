//! Shared on-disk persistence primitives for the durable stores in this crate
//! (`user_history` LXUD, `user_dict` LXUW).
//!
//! These are the canonical, single-source implementations of three concerns
//! that any durable store here needs, so the stores cannot drift apart on the
//! privacy/durability details (the kind of drift that once left `user_dict`
//! with an unsynced write and a stray-`.tmp` bug that LXUD had already fixed):
//!
//! - **Atomic durable write** ([`write_atomic`]): tmp → fsync → rename → dir
//!   fsync, so a rename can never become durable before the file contents.
//! - **Quarantine** ([`quarantine`] / [`rotate_quarantined`]): rename a corrupt
//!   file aside as `<name>.corrupt-<ts>` instead of deleting it, so the bytes
//!   stay rescuable, and cap how many accumulate.
//! - **bincode config** ([`bincode_reader`] / [`bincode_reader_v1`]): a fixint
//!   reader with an explicit allocation cap; trailing-byte tolerance is the one
//!   knob that differs, and the choice is format-dependent (see below).
//!
//! The module has no dependency on any store's domain types or clock: every
//! function takes only paths, bytes, and (for quarantine) an injected `ts`, so
//! it is a leaf both stores depend on rather than a peer of either.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use bincode::Options as _;
use tracing::warn;

/// bincode config matching the wire format of `bincode::serialize` (fixint
/// encoding) plus an explicit allocation cap. The plain `bincode::deserialize`
/// is unlimited: a corrupt length prefix inside the body could trigger a giant
/// allocation before any data is read.
///
/// Trailing bytes are **rejected**. Use this for length- and CRC-framed bodies
/// (LXUD v2 checkpoint / WAL payloads), which are exact-length by construction,
/// so leftovers can only mean a writer bug or corruption that happened to keep
/// the CRC — surface it instead of silently succeeding. For a no-CRC / v1-class
/// format use [`bincode_reader_v1`] instead.
pub(crate) fn bincode_reader(limit: usize) -> impl bincode::Options {
    bincode::options()
        .with_fixint_encoding()
        .with_limit(limit as u64)
}

/// Like [`bincode_reader`] but **tolerates trailing bytes**. Use this for
/// no-CRC / v1-class formats (LXUD v1 checkpoint bodies, LXUW user_dict): the
/// original readers were the plain `bincode::deserialize`, which tolerates
/// trailing bytes, and without a CRC there is no way to tell benign residue
/// from corruption — staying lenient avoids quarantining data the old code
/// recovered. The `with_limit` cap is still applied.
pub(crate) fn bincode_reader_v1(limit: usize) -> impl bincode::Options {
    bincode::options()
        .with_fixint_encoding()
        .allow_trailing_bytes()
        .with_limit(limit as u64)
}

/// `create_dir_all` for the parent, tolerating bare relative paths whose
/// parent is "" (the current directory — nothing to create).
pub(crate) fn ensure_parent_dir(path: &Path) -> io::Result<()> {
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => fs::create_dir_all(p),
        _ => Ok(()),
    }
}

/// Atomic write with content durability: tmp → sync_all (F_FULLFSYNC on
/// macOS) → rename → best-effort parent-dir fsync. Without the tmp sync, the
/// rename can become durable before the file contents, manufacturing a corrupt
/// file on power loss. The parent-dir fsync only makes the rename itself
/// durable; per the LXUD design (§6) it is deliberately best-effort and
/// log-only **for this entry point** — APFS journals renames, and the worst
/// case of an unsynced rename is rolling back to the previous file, never
/// corruption. That is the right trade for a store whose file holds a complete
/// older state (the checkpoint, the user dictionary): a rollback costs recency,
/// which the next write re-establishes. It is the wrong trade for a store whose
/// previous content is a *weaker claim* — the deletion marker, where rolling
/// back turns `Lost` into a suppressible `Unflushed`, or into no file at all.
/// Those callers take [`write_atomic_staged`], which reports whether the name
/// became durable instead of collapsing it into `Ok`. The tmp name
/// appends `.tmp` to the full file name ([`suffixed`]); `with_extension` would
/// strip the store's extension and leave a stray sibling `<stem>.tmp`.
///
/// `rename` protects the *destination* — it replaces a symlink or a FIFO
/// rather than writing through it — but that says nothing about the tmp, which
/// is opened by name like any other file. [`create_regular`] closes that end,
/// so neither half of the write can be diverted by whatever a restore or sync
/// tool left lying at either name.
///
/// What this still cannot promise: `rename` promotes a *name*, not the
/// descriptor whose bytes were flushed. A writer that replaced the tmp between
/// the open and the rename would have its file promoted instead. POSIX offers
/// no fd-based rename, so the window cannot be closed by construction, and the
/// two ways to narrow it are both worse than stating it: a uniquely-named tmp
/// breaks the LXUD rule that the logical marker is the canonical file *and*
/// its orphan tmp (a crash must leave the stronger claim findable at a known
/// name), and a stat-before-rename only shrinks the window while reading as
/// though it closed it. It also buys nothing: anything that can write into
/// this directory can overwrite the destination outright at any moment, with
/// no window to hit.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    // Both outcomes are `Ok` here on purpose — see the trade above. Written as
    // an explicit discard rather than `.map(|_| ())` so that adding a third
    // outcome has to be answered for this caller too.
    match write_atomic_staged(path, bytes) {
        Ok(AtomicWrite::Durable | AtomicWrite::NameUnconfirmed) => Ok(()),
        Err(e) => Err(e.into_io()),
    }
}

/// How far a *successful* [`write_atomic_staged`] got — the success-side twin
/// of [`AtomicWriteFailure`], and it exists for the same reason.
///
/// That type's doc already states the rule: the stage a write reached "is not
/// something a caller can infer … so the writer says which it was". The
/// success path used to break it, collapsing both of these into `Ok(())` with
/// a log line. The two are not interchangeable for every store: the bytes are
/// durable in both, but in the second the *name* pointing at them is not, so a
/// power loss can restore whatever the path held before.
pub(crate) enum AtomicWrite {
    /// Bytes flushed, and the directory entry naming them is flushed too.
    Durable,
    /// Bytes flushed and renamed, but the parent directory entry was not
    /// synced. A power loss can roll the *name* back to its previous target —
    /// including to no entry at all, when this write created it.
    ///
    /// Harmless where the previous target is a complete older state. Not
    /// harmless where it is a weaker claim: a caller that treats this as landed
    /// may stop guarding the very thing the write was recording.
    NameUnconfirmed,
}

/// How far [`write_atomic`] got before failing.
///
/// The stage is not something a caller can infer, and one tried: reading the
/// tmp back and comparing bytes cannot tell a flushed image from one that
/// merely reached the page cache before `sync_all` failed. Both compare equal,
/// and treating the second as durable is precisely the power-loss window this
/// crate exists to close. So the writer says which it was.
pub(crate) enum AtomicWriteFailure {
    /// The bytes never became durable — create, write or sync failed.
    NotDurable(io::Error),
    /// The bytes are flushed at the tmp path; only the rename failed. For a
    /// store whose logical record includes its orphan tmp, the claim has
    /// landed even though the name did not move.
    FlushedNotRenamed(io::Error),
}

impl AtomicWriteFailure {
    pub(crate) fn into_io(self) -> io::Error {
        match self {
            Self::NotDurable(e) | Self::FlushedNotRenamed(e) => e,
        }
    }
}

pub(crate) fn write_atomic_staged(
    path: &Path,
    bytes: &[u8],
) -> Result<AtomicWrite, AtomicWriteFailure> {
    use AtomicWriteFailure::{FlushedNotRenamed, NotDurable};
    let tmp = tmp_path(path);
    ensure_parent_dir(path).map_err(NotDurable)?;
    let mut f = create_regular(&tmp).map_err(NotDurable)?;
    f.write_all(bytes).map_err(NotDurable)?;
    f.sync_all().map_err(NotDurable)?;
    drop(f);
    if let Err(e) = fs::rename(&tmp, path) {
        // The tmp's contents are durable — `sync_all` above — but its *name*
        // may not be: `create_regular` added a directory entry that nothing has
        // flushed yet. A caller that treats the orphan as a landed claim is
        // relying on the next `read` finding it, so the entry has to survive a
        // power loss too. Here the parent-dir fsync decides the outcome outright
        // rather than grading it as the success path below does: with no rename
        // to fall back on, a directory that was not synced leaves nothing about
        // this write dependable, and the caller must not build on it.
        //
        // Confirmed undetectable by measurement: no deterministic test can
        // tell a synced directory entry from an unsynced one without
        // simulating power loss, so removing this survives the suite. It is
        // carried by the argument, not by a test.
        return Err(if sync_parent_dir(path) {
            FlushedNotRenamed(e)
        } else {
            NotDurable(e)
        });
    }
    if !sync_parent_dir(path) {
        warn!("parent dir sync failed; rename durability unconfirmed");
        return Ok(AtomicWrite::NameUnconfirmed);
    }
    Ok(AtomicWrite::Durable)
}

/// The temporary path [`write_atomic`] writes through.
///
/// Called by `write_atomic` itself, so a site that needs to name, sweep, or
/// block that file derives it from the same definition rather than re-spelling
/// the convention. The one that made this matter is a fault injector: a test
/// planting an obstacle at a separately-spelled path stops obstructing
/// anything the moment the writer moves, and passes vacuously instead of
/// failing. (Fixture code that asserts on a literal name is left alone — it
/// fails loudly rather than vacuously if the convention moves.)
pub(crate) fn tmp_path(path: &Path) -> PathBuf {
    suffixed(path, ".tmp")
}

/// Create-or-truncate a path, refusing anything that is not a regular file.
///
/// `File::create` resolves a symlink at the final component and truncates
/// whatever it points at, and blocks indefinitely opening a FIFO that has no
/// reader. Both are reachable without an adversary — a restore or a sync tool
/// leaving an entry at a `.tmp` name is enough — and both are worse here than
/// a failed write: the first destroys a file this crate does not own, the
/// second hangs the thread that called it, which for the deletion marker is
/// the key-processing thread.
///
/// Closed by construction rather than by a preceding `symlink_metadata` check,
/// which would leave the check and the open racing over the same name:
/// `O_NOFOLLOW` makes the symlink case fail in the open itself, `O_NONBLOCK`
/// turns the readerless-FIFO case into `ENXIO` instead of a wait, and the
/// `fstat` afterwards runs on the descriptor already obtained — so what it
/// reports is what was opened, not what the name resolves to now. That last
/// one is what catches a FIFO whose reader happens to be attached, plus
/// sockets and device nodes.
///
/// Unix-only on purpose. A port would have to answer these for its own
/// namespace, and a portable fallback would answer "no protection" silently.
#[cfg(unix)]
fn create_regular(path: &Path) -> io::Result<File> {
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    regular_only(&mut opts, path)
}

/// Open an existing path for reading, refusing anything that is not a regular
/// file — the read counterpart of [`create_regular`], and the same single
/// resolution.
///
/// A `symlink_metadata` check followed by an open is two resolutions of the
/// same name, so substituting a FIFO between them leaves the blocking open
/// intact; that is the shape this replaced. Callers distinguish `NotFound`
/// from every other error themselves — for the deletion marker only the former
/// is clean.
#[cfg(unix)]
pub(crate) fn open_regular(path: &Path) -> io::Result<File> {
    let mut opts = fs::OpenOptions::new();
    opts.read(true);
    regular_only(&mut opts, path)
}

/// Open through `opts` with the final component resolved exactly once, and
/// reject anything that is not a regular file.
///
/// `O_NOFOLLOW` fails a symlink in the open itself rather than acting on its
/// target. `O_NONBLOCK` keeps a FIFO from turning the call into a wait — for
/// writing it becomes `ENXIO` with no reader, for reading it returns at once —
/// and has no effect on a regular file. The `fstat` then runs on the
/// descriptor already obtained, so what it reports is what was opened rather
/// than what the name resolves to now; that is what catches a FIFO whose other
/// end happens to be attached, plus sockets and device nodes.
///
/// Unix-only on purpose. A port would have to answer these for its own
/// namespace, and a portable fallback would answer "no protection" silently.
#[cfg(unix)]
fn regular_only(opts: &mut fs::OpenOptions, path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt as _;

    let f = opts
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    if !f.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a regular file", path.display()),
        ));
    }
    Ok(f)
}

/// Best-effort fsync of the parent directory so the rename itself is durable.
/// APFS likely journals renames already; this is POSIX practice on a background
/// path, so it costs nothing and failures are non-fatal. Returns whether the
/// sync succeeded (callers only log on failure).
pub(crate) fn sync_parent_dir(path: &Path) -> bool {
    let parent = match path.parent() {
        // A bare relative path ("history.lxud") has parent "" — that means the
        // current directory, and File::open("") would fail.
        Some(p) if p.as_os_str().is_empty() => Path::new("."),
        Some(p) => p,
        None => return false,
    };
    match File::open(parent) {
        Ok(dir) => dir.sync_all().is_ok(),
        Err(_) => false,
    }
}

/// Append `suffix` to the full file name (`user_history.lxud` + `.corrupt-1` →
/// `user_history.lxud.corrupt-1`). Unlike `Path::with_extension`, this keeps
/// the store's own extension so the artifact stays within the file family.
pub(crate) fn suffixed(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}

/// Outcome of [`quarantine`]. `Renamed`/`Removed` both leave the path clear so
/// a later save can recreate it; `Failed` means the corrupt file is still in
/// place (a later save will try to overwrite it).
pub(crate) enum Quarantine {
    /// Renamed aside as `<name>.corrupt-<ts>[-n]`; bytes preserved for rescue.
    Renamed(PathBuf),
    /// Rename failed; the corrupt file was removed as a last resort.
    Removed,
    /// Both rename and removal failed; the file remains in place.
    Failed,
}

/// Rename `path` aside as `<name>.corrupt-<ts>[-n]`, preserving the bytes for
/// manual rescue. If the rename fails, removal is attempted as a last resort —
/// clearing the path so subsequent saves can recreate it is prioritized over
/// byte preservation at that point (in practice rename and removal need the
/// same directory permission, so a byte-destroying fallback is nearly
/// unreachable); if even that fails, the file stays in place and later saves
/// will try to overwrite it. `ts` (epoch secs) is injected by the caller so
/// this primitive stays clock-free and independent of any store's clock.
pub(crate) fn quarantine(path: &Path, ts: u64) -> Quarantine {
    for n in 0..10 {
        let suffix = if n == 0 {
            format!(".corrupt-{ts}")
        } else {
            format!(".corrupt-{ts}-{n}")
        };
        let dest = suffixed(path, &suffix);
        if dest.exists() {
            continue;
        }
        return match fs::rename(path, &dest) {
            Ok(()) => Quarantine::Renamed(dest),
            Err(rename_err) => match fs::remove_file(path) {
                Ok(()) => {
                    warn!("quarantine rename failed ({rename_err}); removed corrupt file instead");
                    Quarantine::Removed
                }
                Err(remove_err) => {
                    warn!(
                        "quarantine failed (rename: {rename_err}, remove: {remove_err}); continuing"
                    );
                    Quarantine::Failed
                }
            },
        };
    }
    Quarantine::Failed
}

/// All `<name>.corrupt-*` files in the same directory as `path`.
pub(crate) fn quarantined_files(path: &Path) -> Vec<PathBuf> {
    let parent = match path.parent() {
        // Bare relative name ("history.lxud"): parent is "" = the current
        // directory; read_dir("") would fail and hide quarantine files from
        // rotation and the clear() privacy wipe.
        Some(p) if p.as_os_str().is_empty() => Path::new("."),
        Some(p) => p,
        None => return Vec::new(),
    };
    let Some(prefix) = path.file_name().and_then(|n| n.to_str()) else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(parent) else {
        return Vec::new();
    };
    entries
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|name| name.starts_with(prefix) && name.contains(".corrupt-"))
        })
        .map(|e| e.path())
        .collect()
}

/// Keep only the newest `keep` quarantined files (by epoch in the file name) so
/// repeated corruption cannot accumulate junk forever.
pub(crate) fn rotate_quarantined(path: &Path, keep: usize) {
    let mut files = quarantined_files(path);
    if files.len() <= keep {
        return;
    }
    // `.corrupt-<epoch>[-n]`: sort newest first; unparsable names sort oldest
    // and get cleaned up.
    files.sort_by_key(|p| {
        let key = p
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|name| name.rsplit_once(".corrupt-"))
            .and_then(|(_, suffix)| {
                let (epoch, n) = match suffix.split_once('-') {
                    Some((e, n)) => (e, n.parse::<u64>().unwrap_or(0)),
                    None => (suffix, 0),
                };
                epoch.parse::<u64>().ok().map(|e| (e, n))
            })
            .unwrap_or((0, 0));
        std::cmp::Reverse(key)
    });
    for old in &files[keep..] {
        if let Err(e) = fs::remove_file(old) {
            warn!(
                "failed to rotate old quarantine file {}: {e}",
                old.display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_rename_is_distinguished_from_a_failed_flush() {
        // The stage has to come from the writer. A caller that read the tmp
        // back and compared bytes could not tell these apart: a flushed image
        // and one that only reached the page cache before `sync_all` failed
        // compare equal, and calling the second durable is exactly the
        // power-loss window the atomic write exists to close.
        let dir = tempfile::tempdir().unwrap();

        // Rename blocked by a non-empty directory at the destination; create,
        // write and sync all succeed, so the bytes are durable at the tmp.
        let dest = dir.path().join("store.bin");
        fs::create_dir(&dest).unwrap();
        fs::write(dest.join("occupant"), b"x").unwrap();
        match write_atomic_staged(&dest, b"payload") {
            Err(AtomicWriteFailure::FlushedNotRenamed(_)) => {}
            other => panic!("expected a rename-stage failure, got {:?}", other.is_ok()),
        }
        assert_eq!(
            fs::read(tmp_path(&dest)).unwrap(),
            b"payload",
            "and the flushed bytes really are where the caller is told to look"
        );

        // Nothing durable: the tmp path itself cannot be created.
        let other = dir.path().join("other.bin");
        fs::create_dir(tmp_path(&other)).unwrap();
        match write_atomic_staged(&other, b"payload") {
            Err(AtomicWriteFailure::NotDurable(_)) => {}
            v => panic!("expected a pre-durability failure, got {:?}", v.is_ok()),
        }

        // The `sync_all` half is not reachable without fault injection in this
        // module, which it deliberately does not have — it takes only paths
        // and bytes. What is pinned here is that the two *reachable* stages
        // are reported distinctly, which is what the caller branches on.
    }
}
