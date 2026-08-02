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
/// log-only — APFS journals renames, and the worst case of an unsynced rename
/// is rolling back to the previous file, never corruption. The tmp name
/// appends `.tmp` to the full file name ([`suffixed`]); `with_extension` would
/// strip the store's extension and leave a stray sibling `<stem>.tmp`.
///
/// `rename` protects the *destination* — it replaces a symlink or a FIFO
/// rather than writing through it — but that says nothing about the tmp, which
/// is opened by name like any other file. [`create_regular`] closes that end,
/// so neither half of the write can be diverted by whatever a restore or sync
/// tool left lying at either name.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = tmp_path(path);
    ensure_parent_dir(path)?;
    let mut f = create_regular(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    drop(f);
    fs::rename(&tmp, path)?;
    if !sync_parent_dir(path) {
        warn!("parent dir sync failed; rename durability unconfirmed (best-effort, by design)");
    }
    Ok(())
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
    use std::os::unix::fs::OpenOptionsExt as _;

    let f = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
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
