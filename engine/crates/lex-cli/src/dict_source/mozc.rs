use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;

use super::{
    is_hiragana, parse_dict_files, parse_id_cost, DictSource, DictSourceError, ParsedLine,
};
use lex_core::dict::DictEntry;

const MOZC_API_BASE: &str = "https://api.github.com/repos/google/mozc";
const MOZC_RAW_BASE: &str = "https://raw.githubusercontent.com/google/mozc";

/// Mozc TSV dictionary source.
///
/// File format: `reading\tleft_id\tright_id\tcost\tsurface`
/// Files matched: `dictionary*.txt` in the input directory.
pub struct MozcSource;

impl MozcSource {
    /// Download `url` into `dest` atomically: the body is staged in a
    /// dot-prefixed temp file next to `dest` and renamed into place. An
    /// interrupted run therefore never leaves a partial download under a
    /// final artifact name — which the repair path (`cache_complete` /
    /// skip-existing) would otherwise trust as a complete file (Codex
    /// review R3 on PR #266). The temp file is removed on every error path.
    fn download_file(url: &str, dest: &Path) -> Result<(), DictSourceError> {
        let body = ureq::get(url)
            .call()
            .map_err(|e| DictSourceError::Http(format!("{url}: {e}")))?
            .into_body()
            .read_to_vec()
            .map_err(|e| DictSourceError::Http(format!("{url}: {e}")))?;
        let mut tmp_name = std::ffi::OsString::from(".");
        tmp_name.push(dest.file_name().ok_or_else(|| {
            DictSourceError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid download path {}", dest.display()),
            ))
        })?);
        tmp_name.push(format!(".tmp.{}", std::process::id()));
        let tmp = dest.with_file_name(tmp_name);
        let _guard = TmpFileGuard(tmp.clone());
        fs::write(&tmp, &body).map_err(DictSourceError::Io)?;
        fs::rename(&tmp, dest).map_err(DictSourceError::Io)?;
        Ok(())
    }

    /// Download one snapshot's files — the listed shards + `id.def`, plus the
    /// fixed-URL connection matrix and LICENSE — pinned to `sha`, into
    /// `target`. With `skip_existing` (repair within a valid cache), files
    /// already present are left alone: they are guaranteed to come from the
    /// same pinned SHA, and `download_file`'s tmp+rename guarantees none of
    /// them is a torn write.
    fn download_snapshot(
        sha: &str,
        remote_files: &[(String, String)],
        target: &Path,
        skip_existing: bool,
    ) -> Result<(), DictSourceError> {
        for (name, url) in remote_files {
            let path = target.join(name);
            if skip_existing && path.is_file() {
                eprintln!("  {name} (already exists, skipping)");
                continue;
            }
            eprintln!("  {name}");
            Self::download_file(url, &path)?;
        }

        let connection = target.join("connection_single_column.txt");
        if skip_existing && connection.is_file() {
            eprintln!("  connection_single_column.txt (already exists, skipping)");
        } else {
            eprintln!("  connection_single_column.txt");
            let url = format!(
                "{MOZC_RAW_BASE}/{sha}/src/data/dictionary_oss/connection_single_column.txt"
            );
            Self::download_file(&url, &connection)?;
        }

        let license = target.join("LICENSE");
        if skip_existing && license.is_file() {
            eprintln!("  LICENSE (already exists, skipping)");
        } else {
            eprintln!("  LICENSE");
            let url = format!("{MOZC_RAW_BASE}/{sha}/LICENSE");
            Self::download_file(&url, &license)?;
        }
        Ok(())
    }

    /// Resolve the latest commit SHA of `master` via the GitHub Commits API.
    /// All raw-file downloads in a fetch run are pinned to this single SHA so
    /// the cached snapshot can never mix files from two upstream states.
    fn latest_commit_sha() -> Result<String, DictSourceError> {
        let url = format!("{MOZC_API_BASE}/commits/master");
        let body = ureq::get(&url)
            .call()
            .map_err(|e| DictSourceError::Http(format!("{url}: {e}")))?
            .into_body()
            .read_to_string()
            .map_err(|e| DictSourceError::Http(format!("{url}: {e}")))?;
        parse_commit_sha(&body)
    }

    /// List dictionary files via GitHub Contents API **pinned to `sha`** and
    /// return (name, download_url) pairs for the upstream-numbered shards
    /// (`dictionary<digits>.txt`) and `id.def`.
    /// The returned `download_url`s point at the same pinned SHA.
    fn list_remote_files(sha: &str) -> Result<Vec<(String, String)>, DictSourceError> {
        let url = format!("{MOZC_API_BASE}/contents/src/data/dictionary_oss?ref={sha}");
        let body = ureq::get(&url)
            .call()
            .map_err(|e| DictSourceError::Http(format!("{url}: {e}")))?
            .into_body()
            .read_to_string()
            .map_err(|e| DictSourceError::Http(format!("{url}: {e}")))?;
        parse_remote_files(&body)
    }
}

/// Removes the staged temp file on drop so error paths don't leak it. After
/// a successful rename the path no longer exists and the removal is a no-op.
struct TmpFileGuard(std::path::PathBuf);
impl Drop for TmpFileGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// True if `s` is a full commit SHA: exactly 40 hex chars. Anything else is
/// rejected so a malformed value can't end up embedded in raw URLs or trusted
/// as a cache version.
fn is_valid_sha(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Extract and validate the commit SHA from a GitHub Commits API response.
fn parse_commit_sha(json: &str) -> Result<String, DictSourceError> {
    let v: serde_json::Value = serde_json::from_str(json)
        .map_err(|e| DictSourceError::Parse(format!("GitHub commit JSON: {e}")))?;
    let sha = v["sha"]
        .as_str()
        .ok_or_else(|| DictSourceError::Parse("GitHub commit JSON: missing sha".to_string()))?;
    if !is_valid_sha(sha) {
        return Err(DictSourceError::Parse(format!(
            "GitHub commit JSON: invalid sha {sha:?}"
        )));
    }
    Ok(sha.to_string())
}

/// A parsed version stamp: line 1 is the pinned commit SHA, lines 2+ are the
/// manifest — the names of the files that fetch run downloaded. The manifest
/// is what `wipe_cache` deletes, so user-placed files (e.g. a hand-added
/// `dictionary-custom.txt`, which `parse_dir`'s `dictionary*.txt` glob happily
/// reads) are never deleted just because upstream moved. A legacy SHA-only
/// stamp yields an empty manifest.
struct Stamp {
    sha: String,
    manifest: Vec<String>,
}

/// Read the version stamp. Returns `None` when the first line is not a
/// well-formed commit SHA (missing file, empty / legacy pre-versioning
/// `.stamp`, corrupted or hand-edited SHA line) — the caller treats that as
/// "no version" (wipe + full re-download). Manifest entries on lines 2+ are
/// filtered, not fatal: anything that fails `is_upstream_file_name` (path
/// traversal attempts, or a hand-edited entry naming a user file like
/// `dictionary-custom.txt`) is silently dropped so a tampered stamp can
/// neither direct the wipe outside the cache dir nor at files this source
/// never fetched.
fn read_stamp(path: &Path) -> Option<Stamp> {
    let content = fs::read_to_string(path).ok()?;
    let mut lines = content.lines();
    let sha = lines.next()?.trim();
    if !is_valid_sha(sha) {
        return None;
    }
    let manifest = lines
        .map(str::trim)
        .filter(|l| is_upstream_file_name(l))
        .map(str::to_string)
        .collect();
    Some(Stamp {
        sha: sha.to_string(),
        manifest,
    })
}

/// True if `name` is an upstream dictionary shard: `dictionary<digits>.txt`.
/// Deliberately narrower than the `dictionary*.txt` glob that `parse_dir`
/// accepts, so a user-placed `dictionary-custom.txt` is never mistaken for a
/// fetched artifact (Codex review on PR #266).
fn is_numbered_dictionary(name: &str) -> bool {
    name.strip_prefix("dictionary")
        .and_then(|rest| rest.strip_suffix(".txt"))
        .is_some_and(|mid| !mid.is_empty() && mid.bytes().all(|b| b.is_ascii_digit()))
}

/// True if `name` matches the exact upstream naming of the files this source
/// fetches: `dictionary<digits>.txt`, `id.def`, `connection_single_column.txt`
/// or `LICENSE`.
fn is_upstream_file_name(name: &str) -> bool {
    is_numbered_dictionary(name)
        || name == "id.def"
        || name == "connection_single_column.txt"
        || name == "LICENSE"
}

/// True if the cached snapshot vouched for by `stamp` is fully on disk.
/// Only a manifest-carrying stamp can prove completeness (every recorded
/// file must exist); a legacy SHA-only stamp cannot, so it returns false
/// and the caller falls back to weaker heuristics or a re-download.
fn cache_complete(stamp: &Stamp, dest: &Path) -> bool {
    // is_file(), not exists(): a directory squatting on an artifact name must
    // not count as a cached file (it would poison the fast path).
    !stamp.manifest.is_empty() && stamp.manifest.iter().all(|f| dest.join(f).is_file())
}

/// True if the files required by downstream consumers are all present:
/// `id.def`, `connection_single_column.txt`, and at least one upstream
/// dictionary shard (`dictionary<digits>.txt` — a user-placed
/// `dictionary-custom.txt` does not count). Offline-fallback heuristic for
/// legacy SHA-only stamps, which record no manifest: it cannot detect a
/// partially deleted shard set (e.g. `dictionary01.txt` missing while
/// `dictionary00.txt` survives). This weakness is transitional — the first
/// online fetch rewrites the stamp with a manifest, after which
/// `cache_complete` gives an exact answer.
fn required_files_present(dest: &Path) -> bool {
    let any_dictionary = fs::read_dir(dest)
        .map(|rd| {
            rd.filter_map(|e| e.ok()).any(|e| {
                is_numbered_dictionary(&e.file_name().to_string_lossy()) && e.path().is_file()
            })
        })
        .unwrap_or(false);
    any_dictionary
        && dest.join("id.def").is_file()
        && dest.join("connection_single_column.txt").is_file()
}

/// True if any upstream-named fetched file exists in `dest`.
fn any_fetched_file_exists(dest: &Path) -> bool {
    fs::read_dir(dest)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .any(|e| is_upstream_file_name(&e.file_name().to_string_lossy()))
        })
        .unwrap_or(false)
}

/// Remove `path` treating "already gone" as success. Any other failure is
/// propagated — see `wipe_cache`.
fn remove_if_exists(path: &Path) -> Result<(), DictSourceError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(DictSourceError::Io(io::Error::new(
            e.kind(),
            format!("failed to remove stale {}: {e}", path.display()),
        ))),
    }
}

/// Remove fetched files plus the stamp so a re-download starts clean.
/// Deletes exactly the files recorded in the stamp's manifest when one is
/// available; without a manifest (legacy SHA-only stamp, no stamp at all)
/// it falls back to the exact upstream naming pattern. Either way,
/// user-placed files like `dictionary-custom.txt` survive.
///
/// Removal failures are errors, not warnings: a stale file that survives the
/// wipe would sit next to freshly downloaded files from a different upstream
/// snapshot — exactly the id.def / dictionary*.txt mix this module exists to
/// prevent. The caller must abort the fetch (Codex review on PR #266).
fn wipe_cache(dest: &Path, stamp_path: &Path, manifest: &[String]) -> Result<(), DictSourceError> {
    if manifest.is_empty() {
        if let Ok(rd) = fs::read_dir(dest) {
            for entry in rd.filter_map(|e| e.ok()) {
                if is_upstream_file_name(&entry.file_name().to_string_lossy()) {
                    remove_if_exists(&entry.path())?;
                }
            }
        }
    } else {
        for name in manifest {
            remove_if_exists(&dest.join(name))?;
        }
    }
    remove_if_exists(stamp_path)
}

/// Write the stamp: line 1 = pinned commit SHA, lines 2+ = manifest.
fn write_stamp(path: &Path, sha: &str, manifest: &[String]) -> Result<(), DictSourceError> {
    let mut contents = sha.to_string();
    for name in manifest {
        contents.push('\n');
        contents.push_str(name);
    }
    // tmp + rename: an interrupted write must not leave a truncated stamp
    // whose valid SHA line + partial manifest would satisfy cache_complete
    // and mask an incomplete snapshot.
    // Not with_extension(): for a dotfile like `.stamp` that has no extension,
    // with_extension would yield `.stamp.stamp.tmp`. Append to the name instead.
    let mut tmp_name = path.file_name().unwrap_or_default().to_os_string();
    tmp_name.push(".tmp");
    let tmp = path.with_file_name(tmp_name);
    fs::write(&tmp, contents).map_err(DictSourceError::Io)?;
    fs::rename(&tmp, path).map_err(DictSourceError::Io)
}

/// The full manifest of a snapshot: the listed shard / `id.def` names plus
/// the two fixed-URL files.
fn snapshot_manifest(remote_files: &[(String, String)]) -> Vec<String> {
    let mut manifest: Vec<String> = remote_files.iter().map(|(n, _)| n.clone()).collect();
    manifest.push("connection_single_column.txt".to_string());
    manifest.push("LICENSE".to_string());
    manifest
}

/// Swap a fully staged snapshot into `dest`. Pure file operations, ordered so
/// that no crash point leaves a state the next run would *trust*:
///
/// 1. Preflight — every `new_manifest` file must exist in `staging`; bail
///    before touching `dest` otherwise, leaving the current cache intact.
/// 2. Delete the stamp. From here on the cache is officially "no version":
///    a crash mid-swap leaves a mixed dir at worst, and the next run wipes
///    and re-downloads it rather than mistaking it for a complete snapshot.
/// 3. Wipe the old snapshot files (old manifest, or the upstream naming
///    pattern for legacy stamps). User-placed files survive as usual.
/// 4. Rename the staged files into place (same filesystem — `staging` lives
///    inside `dest` — so these are atomic per file).
/// 5. Remove the now-empty staging dir and write the new stamp, which
///    re-validates the cache.
fn commit_staged(
    dest: &Path,
    stamp_path: &Path,
    staging: &Path,
    old_manifest: &[String],
    sha: &str,
    new_manifest: &[String],
) -> Result<(), DictSourceError> {
    for name in new_manifest {
        if !staging.join(name).is_file() {
            return Err(DictSourceError::Io(io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "staged snapshot in {} is missing {name}; leaving current cache untouched",
                    staging.display()
                ),
            )));
        }
    }
    remove_if_exists(stamp_path)?;
    wipe_cache(dest, stamp_path, old_manifest)?;
    for name in new_manifest {
        fs::rename(staging.join(name), dest.join(name)).map_err(|e| {
            DictSourceError::Io(io::Error::new(
                e.kind(),
                format!("failed to move staged {name} into place: {e}"),
            ))
        })?;
    }
    let _ = fs::remove_dir_all(staging);
    write_stamp(stamp_path, sha, new_manifest)
}

/// Parse GitHub Contents API JSON and return (name, download_url) pairs
/// for the upstream-numbered shards (`dictionary<digits>.txt`) and `id.def`.
fn parse_remote_files(json: &str) -> Result<Vec<(String, String)>, DictSourceError> {
    let entries: Vec<serde_json::Value> = serde_json::from_str(json)
        .map_err(|e| DictSourceError::Parse(format!("GitHub API JSON: {e}")))?;

    let mut files: Vec<(String, String)> = Vec::new();
    for entry in &entries {
        let (Some(raw_name), Some(url)) = (entry["name"].as_str(), entry["download_url"].as_str())
        else {
            continue; // skip entries with missing name or download_url
        };
        if url.is_empty() {
            continue;
        }
        // Sanitize: use only the file basename to prevent path traversal
        let name = Path::new(raw_name)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        // Invariant: everything fetch downloads must pass is_upstream_file_name,
        // because the manifest read-filter (read_stamp) and wipe_cache only
        // recognize those names. A broader match here (e.g. plain
        // `dictionary*.txt`) would download files the wipe never removes,
        // re-introducing cross-snapshot mixing on the next version bump.
        let wanted = is_numbered_dictionary(&name) || name == "id.def";
        if wanted {
            files.push((name.into_owned(), url.to_string()));
        }
    }
    files.sort();
    Ok(files)
}

impl DictSource for MozcSource {
    fn parse_dir(&self, dir: &Path) -> Result<HashMap<String, Vec<DictEntry>>, DictSourceError> {
        parse_dict_files(
            dir,
            "dictionary*.txt",
            |name| name.starts_with("dictionary") && name.ends_with(".txt"),
            '\t',
            |fields| {
                if fields.len() < 5 {
                    return None;
                }
                let reading = fields[0];
                let (left_id, right_id, cost) = parse_id_cost(fields)?;
                let surface = fields[4];
                if !is_hiragana(reading) {
                    return None;
                }
                Some(ParsedLine {
                    reading: reading.to_string(),
                    surface: surface.to_string(),
                    left_id,
                    right_id,
                    cost,
                })
            },
        )
    }

    /// Fetch the Mozc dictionary snapshot into `dest`. Same cache discipline
    /// as the SudachiDict fetcher (`candidates/sudachi.rs`, PR #242): **the
    /// stamp file must equal the resolved upstream version exactly**, here
    /// the latest commit SHA of `google/mozc` master. Anything else (stamp
    /// missing, legacy empty stamp, mismatched SHA — with or without leftover
    /// files) triggers a full re-download of the new snapshot into a
    /// `.staging` subdirectory, followed by a transactional swap
    /// (`commit_staged`): the old snapshot keeps working — and remains
    /// available to the offline fallback — until the entire new snapshot has
    /// landed. A transient listing/download failure therefore never costs a
    /// previously working cache.
    ///
    /// Crucially, every download in a run is pinned to that one SHA via
    /// `raw.githubusercontent.com/google/mozc/<sha>/...`, so `id.def` and
    /// `dictionary*.txt` can never come from two different upstream states —
    /// neither across runs (stale per-file cache) nor within a run (upstream
    /// moving while we fetch). A skew there silently shifts POS ids and
    /// corrupts conversion quality without any visible error.
    ///
    /// When the cached snapshot is already at the latest SHA and its manifest
    /// is fully on disk, the fetch returns early without touching the
    /// Contents API at all — a valid cache must not fail on rate limits or
    /// transient listing errors. Within a valid-but-incomplete cache (files
    /// deleted by hand after a successful fetch), only the missing files are
    /// re-downloaded, from that same pinned SHA, atomically per file
    /// (tmp+rename), so an interrupted repair never leaves a partial file
    /// under a final name. An interrupted swap never leaves a stamp, so the
    /// next run wipes and re-downloads instead of trusting a mixed dir; if
    /// that wipe cannot remove a stale file, the fetch aborts rather than
    /// risk mixing snapshots.
    ///
    /// Offline behavior: if the SHA can't be resolved but a cached snapshot
    /// exists (valid stamp — only written after a fully successful fetch) and
    /// its artifacts are still on disk, warn and keep using it; otherwise
    /// fail.
    ///
    /// The stamp records, after the SHA, the manifest of files this fetch
    /// downloaded; a later wipe deletes exactly those, so user-placed files
    /// in the cache dir survive version bumps.
    fn fetch(&self, dest: &Path) -> Result<(), DictSourceError> {
        fs::create_dir_all(dest).map_err(DictSourceError::Io)?;
        let stamp_path = dest.join(".stamp");
        // Clear staging leftovers from an interrupted previous run up front.
        // `.staging` is dot-prefixed, so parse_dir's dictionary*.txt glob and
        // the wipe/presence scans never see it or its contents.
        let staging = dest.join(".staging");
        if staging.exists() {
            fs::remove_dir_all(&staging).map_err(DictSourceError::Io)?;
        }
        let cached = read_stamp(&stamp_path);

        let sha = match Self::latest_commit_sha() {
            Ok(sha) => sha,
            Err(e) => {
                if let Some(st) = &cached {
                    // A stamp alone is not enough — verify the artifacts it
                    // vouches for are still on disk (manifest when recorded,
                    // required-file heuristic for legacy SHA-only stamps).
                    let complete = if st.manifest.is_empty() {
                        required_files_present(dest)
                    } else {
                        cache_complete(st, dest)
                    };
                    if complete {
                        eprintln!(
                            "Warning: could not resolve latest mozc commit ({e}); \
                             using cached snapshot {}.",
                            st.sha
                        );
                        // Same-content rewrite, like the online fast path:
                        // a successful (offline-verified) fetch refreshes the
                        // stamp mtime so mise's sources-vs-outputs staleness
                        // check doesn't re-run this task on every build.
                        write_stamp(&stamp_path, &st.sha, &st.manifest)?;
                        return Ok(());
                    }
                    return Err(DictSourceError::Http(format!(
                        "could not resolve latest mozc commit ({e}) and cached snapshot {} \
                         in {} is missing required files",
                        st.sha,
                        dest.display()
                    )));
                }
                return Err(e);
            }
        };

        let cache_valid = cached.as_ref().map(|s| s.sha.as_str()) == Some(sha.as_str());

        if cache_valid {
            let st = cached.as_ref().expect("cache_valid implies stamp");

            // Fast path: latest SHA with every manifest file on disk — done,
            // without spending a Contents API call (or risking its failure).
            if cache_complete(st, dest) {
                eprintln!("Mozc dictionary cache is up to date (commit {sha}).");
                // Rewrite the (identical) stamp so its mtime satisfies
                // mise's outputs-based staleness check.
                write_stamp(&stamp_path, &st.sha, &st.manifest)?;
                return Ok(());
            }

            // Repair path: same SHA but files missing (deleted by hand, or a
            // legacy manifest-less stamp). Fill in only the gaps, straight
            // into dest — sound because everything already present is from
            // the same pinned SHA and download_file lands each file
            // atomically, so nothing partial can be mistaken for complete.
            eprintln!(
                "Repairing Mozc dictionary cache (commit {sha}) in {}...",
                dest.display()
            );
            let remote_files = Self::list_remote_files(&sha)?;
            let manifest = snapshot_manifest(&remote_files);
            Self::download_snapshot(&sha, &remote_files, dest, true)?;
            write_stamp(&stamp_path, &sha, &manifest)?;
            eprintln!("Done. Files saved to {}", dest.display());
            return Ok(());
        }

        // Stale (or unversioned) cache: transactional snapshot swap. Nothing
        // in dest is touched until the ENTIRE new snapshot sits in staging —
        // a transient listing/download failure leaves the old snapshot
        // working and offline-usable (Codex review R3 on PR #266: wiping
        // up front traded a working cache for any network hiccup).
        if let Some(st) = &cached {
            eprintln!(
                "Cache commit {} != latest {sha}; downloading new snapshot before replacing.",
                st.sha
            );
        } else if any_fetched_file_exists(dest) {
            eprintln!("Cache has no version stamp; will replace files after download.");
        }

        eprintln!("Downloading Mozc dictionary files (commit {sha}) to staging...");
        let remote_files = Self::list_remote_files(&sha)?;
        let manifest = snapshot_manifest(&remote_files);
        fs::create_dir_all(&staging).map_err(DictSourceError::Io)?;
        Self::download_snapshot(&sha, &remote_files, &staging, false)?;

        let old_manifest = cached
            .as_ref()
            .map(|s| s.manifest.as_slice())
            .unwrap_or(&[]);
        commit_staged(dest, &stamp_path, &staging, old_manifest, &sha, &manifest)?;
        eprintln!("Done. Files saved to {}", dest.display());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_parse_mozc_tsv() {
        let dir = std::env::temp_dir().join("lexime_test_mozc");
        fs::create_dir_all(&dir).unwrap();
        let dict_file = dir.join("dictionary00.txt");
        fs::write(
            &dict_file,
            "# comment line\n\
             かんじ\t1847\t1847\t5100\t漢字\n\
             かんじ\t1847\t1847\t5150\t感じ\n\
             テスト\t100\t100\t3000\ttest\n\
             にほん\t1847\t1847\t4500\t日本\n",
        )
        .unwrap();

        let source = MozcSource;
        let entries = source.parse_dir(&dir).unwrap();

        // テスト (katakana reading) should be skipped
        assert!(!entries.contains_key("テスト"));

        // かんじ should have 2 entries
        let kanji = entries.get("かんじ").unwrap();
        assert_eq!(kanji.len(), 2);
        assert_eq!(kanji[0].surface, "漢字");
        assert_eq!(kanji[0].cost, 5100);
        assert_eq!(kanji[0].left_id, 1847);
        assert_eq!(kanji[1].surface, "感じ");

        // にほん should have 1 entry
        let nihon = entries.get("にほん").unwrap();
        assert_eq!(nihon.len(), 1);
        assert_eq!(nihon[0].surface, "日本");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_parse_mozc_empty_dir() {
        let dir = std::env::temp_dir().join("lexime_test_mozc_empty");
        fs::create_dir_all(&dir).unwrap();

        let source = MozcSource;
        let result = source.parse_dir(&dir);
        assert!(result.is_err());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_is_hiragana() {
        assert!(is_hiragana("かんじ"));
        assert!(is_hiragana("あ"));
        assert!(is_hiragana("らーめん")); // prolonged sound mark allowed
        assert!(!is_hiragana("カタカナ"));
        assert!(!is_hiragana("abc"));
        assert!(!is_hiragana(""));
    }

    #[test]
    fn test_parse_remote_files() {
        let json = r#"[
            {"name": "dictionary00.txt", "download_url": "https://example.com/dictionary00.txt"},
            {"name": "dictionary01.txt", "download_url": "https://example.com/dictionary01.txt"},
            {"name": "id.def", "download_url": "https://example.com/id.def"},
            {"name": "README.md", "download_url": "https://example.com/README.md"},
            {"name": "reading_correction.tsv", "download_url": "https://example.com/rc.tsv"},
            {"name": "subdir", "download_url": null}
        ]"#;
        let files = parse_remote_files(json).unwrap();
        assert_eq!(files.len(), 3);
        assert_eq!(files[0].0, "dictionary00.txt");
        assert_eq!(files[1].0, "dictionary01.txt");
        assert_eq!(files[2].0, "id.def");
    }

    #[test]
    fn test_parse_remote_files_only_recognized_names() {
        // Invariant: fetched ⊆ recognized (is_upstream_file_name). A file the
        // wipe/manifest layer doesn't recognize must not be downloaded, or it
        // would survive version bumps and mix into the next snapshot.
        let json = r#"[
            {"name": "dictionary_extra.txt", "download_url": "https://example.com/a"},
            {"name": "dictionary-custom.txt", "download_url": "https://example.com/b"},
            {"name": "dictionary00.txt", "download_url": "https://example.com/c"}
        ]"#;
        let files = parse_remote_files(json).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].0, "dictionary00.txt");
        for (name, _) in &files {
            assert!(is_upstream_file_name(name));
        }
    }

    #[test]
    fn test_parse_remote_files_sanitizes_path() {
        let json = r#"[
            {"name": "../../../etc/dictionary00.txt", "download_url": "https://example.com/x"}
        ]"#;
        let files = parse_remote_files(json).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].0, "dictionary00.txt");
    }

    #[test]
    fn test_is_valid_sha() {
        assert!(is_valid_sha("0123456789abcdef0123456789abcdef01234567"));
        assert!(is_valid_sha("0123456789ABCDEF0123456789ABCDEF01234567"));
        // Wrong length
        assert!(!is_valid_sha(""));
        assert!(!is_valid_sha("abc123"));
        assert!(!is_valid_sha("0123456789abcdef0123456789abcdef012345678"));
        // Right length, non-hex
        assert!(!is_valid_sha("0123456789abcdef0123456789abcdef0123456z"));
        assert!(!is_valid_sha("../../../../../../../../../../../../etc/x"));
    }

    #[test]
    fn test_parse_commit_sha() {
        let sha = "0123456789abcdef0123456789abcdef01234567";
        let json = format!(r#"{{"sha": "{sha}", "commit": {{}}}}"#);
        assert_eq!(parse_commit_sha(&json).unwrap(), sha);
    }

    #[test]
    fn test_parse_commit_sha_rejects_malformed() {
        // Missing sha field
        assert!(parse_commit_sha(r#"{"commit": {}}"#).is_err());
        // Not JSON
        assert!(parse_commit_sha("not json").is_err());
        // Wrong length
        assert!(parse_commit_sha(r#"{"sha": "abc123"}"#).is_err());
        // Right length but non-hex (could smuggle path segments into raw URLs)
        assert!(
            parse_commit_sha(r#"{"sha": "0123456789abcdef0123456789abcdef0123456z"}"#).is_err()
        );
    }

    #[test]
    fn test_read_stamp_roundtrip() {
        let dir = std::env::temp_dir().join("lexime_test_mozc_stamp");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let stamp = dir.join(".stamp");

        // Missing → None
        assert!(read_stamp(&stamp).is_none());

        // Legacy empty stamp (pre-versioning marker) → None, so it triggers
        // a wipe + re-download instead of being trusted as current.
        fs::write(&stamp, "").unwrap();
        assert!(read_stamp(&stamp).is_none());

        // Whitespace-only → None
        fs::write(&stamp, "  \n").unwrap();
        assert!(read_stamp(&stamp).is_none());

        // Corrupted / hand-edited (not a 40-hex SHA) → None, so it is treated
        // as "no version" and triggers a wipe + re-download instead of being
        // trusted (Copilot review on PR #266).
        fs::write(&stamp, "master\n").unwrap();
        assert!(read_stamp(&stamp).is_none());
        fs::write(&stamp, "0123456789abcdef0123456789abcdef0123456z\n").unwrap();
        assert!(read_stamp(&stamp).is_none());

        // Legacy SHA-only stamp (no manifest) → sha parsed, empty manifest.
        fs::write(&stamp, "0123456789abcdef0123456789abcdef01234567\n").unwrap();
        let st = read_stamp(&stamp).unwrap();
        assert_eq!(st.sha, "0123456789abcdef0123456789abcdef01234567");
        assert!(st.manifest.is_empty());

        // SHA + manifest roundtrip.
        fs::write(
            &stamp,
            "0123456789abcdef0123456789abcdef01234567\ndictionary00.txt\nid.def\n",
        )
        .unwrap();
        let st = read_stamp(&stamp).unwrap();
        assert_eq!(st.sha, "0123456789abcdef0123456789abcdef01234567");
        assert_eq!(st.manifest, vec!["dictionary00.txt", "id.def"]);

        // Tampered manifest entries with path separators / `..` are dropped
        // so a wipe can never escape the cache dir.
        fs::write(
            &stamp,
            "0123456789abcdef0123456789abcdef01234567\n../evil\nsub/dir.txt\n..\nid.def\n",
        )
        .unwrap();
        let st = read_stamp(&stamp).unwrap();
        assert_eq!(st.manifest, vec!["id.def"]);

        // Hand-edited manifest entries naming files this source never
        // fetches are dropped too, so a tampered stamp can't aim the wipe
        // at user files (Copilot review on PR #266).
        fs::write(
            &stamp,
            "0123456789abcdef0123456789abcdef01234567\n\
             dictionary-custom.txt\nnotes.md\ndictionary00.txt\n",
        )
        .unwrap();
        let st = read_stamp(&stamp).unwrap();
        assert_eq!(st.manifest, vec!["dictionary00.txt"]);

        // write_stamp uses tmp+rename with a proper `.stamp.tmp` sibling —
        // for a dotfile, with_extension would have produced `.stamp.stamp.tmp`
        // (Copilot review on PR #266). No tmp residue after a write.
        write_stamp(
            &stamp,
            "0123456789abcdef0123456789abcdef01234567",
            &["dictionary00.txt".to_string()],
        )
        .unwrap();
        let residue: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp"))
            .collect();
        assert!(residue.is_empty(), "tmp residue left behind: {residue:?}");
        let st = read_stamp(&stamp).unwrap();
        assert_eq!(st.manifest, vec!["dictionary00.txt"]);

        fs::remove_dir_all(&dir).ok();
    }

    /// Pins the fast-path predicate: a manifest stamp with every file on disk
    /// is complete (fetch early-returns without any Contents API call); a
    /// missing file or a legacy manifest-less stamp is not.
    #[test]
    fn test_cache_complete() {
        let dir = std::env::temp_dir().join("lexime_test_mozc_complete");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let sha = "0123456789abcdef0123456789abcdef01234567".to_string();

        // Legacy SHA-only stamp: no manifest → cannot prove completeness.
        let legacy = Stamp {
            sha: sha.clone(),
            manifest: vec![],
        };
        assert!(!cache_complete(&legacy, &dir));

        let st = Stamp {
            sha,
            manifest: vec!["dictionary00.txt".to_string(), "id.def".to_string()],
        };
        // Manifest recorded but files absent → incomplete.
        assert!(!cache_complete(&st, &dir));

        fs::write(dir.join("dictionary00.txt"), "x").unwrap();
        assert!(!cache_complete(&st, &dir), "one of two files → incomplete");

        fs::write(dir.join("id.def"), "x").unwrap();
        assert!(cache_complete(&st, &dir));

        // A directory squatting on an artifact name is not a cached file.
        fs::remove_file(dir.join("id.def")).unwrap();
        fs::create_dir(dir.join("id.def")).unwrap();
        assert!(
            !cache_complete(&st, &dir),
            "directory in place of a file → incomplete"
        );

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_is_upstream_file_name() {
        // Exact upstream names are matched.
        assert!(is_upstream_file_name("dictionary00.txt"));
        assert!(is_upstream_file_name("dictionary09.txt"));
        assert!(is_upstream_file_name("id.def"));
        assert!(is_upstream_file_name("connection_single_column.txt"));
        assert!(is_upstream_file_name("LICENSE"));
        // User-placed dictionaries that parse_dir's glob would read must NOT
        // be treated as fetched artifacts (Codex review on PR #266).
        assert!(!is_upstream_file_name("dictionary-custom.txt"));
        assert!(!is_upstream_file_name("dictionary.txt"));
        assert!(!is_upstream_file_name("dictionary_backup01.txt"));
        assert!(!is_upstream_file_name("notes.md"));
        assert!(!is_upstream_file_name(".stamp"));
    }

    /// Pins the cache_valid predicate that drives wipe decisions — the mozc
    /// counterpart of `candidates/sudachi.rs::test_stale_cache_invariant`
    /// (PR #242). Covers SHA mismatch, missing stamp, and the legacy empty
    /// stamp left behind by the pre-versioning fetch. Network path is
    /// exercised by manual `dictool fetch --source mozc` runs.
    #[test]
    fn test_stale_cache_invariant() {
        fn should_wipe(dest: &Path, latest: &str) -> bool {
            let cached = read_stamp(&dest.join(".stamp"));
            cached.as_ref().map(|s| s.sha.as_str()) != Some(latest)
        }

        let dir = std::env::temp_dir().join("lexime_test_mozc_invariant");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let latest = "0123456789abcdef0123456789abcdef01234567";
        let older = "fedcba9876543210fedcba9876543210fedcba98";

        // Case 1: stamp matches latest SHA → no wipe.
        fs::write(dir.join(".stamp"), latest).unwrap();
        assert!(!should_wipe(&dir, latest));

        // Case 2: stamp records an older SHA → wipe.
        fs::write(dir.join(".stamp"), older).unwrap();
        assert!(should_wipe(&dir, latest));

        // Case 3: stamp missing entirely → wipe.
        fs::remove_file(dir.join(".stamp")).unwrap();
        assert!(should_wipe(&dir, latest));

        // Case 4: legacy empty stamp → wipe.
        fs::write(dir.join(".stamp"), "").unwrap();
        assert!(should_wipe(&dir, latest));

        // Case 5: corrupted / hand-edited stamp (not a 40-hex SHA) → wipe.
        fs::write(dir.join(".stamp"), "master").unwrap();
        assert!(should_wipe(&dir, latest));

        fs::remove_dir_all(&dir).ok();
    }

    /// Pins the offline-fallback guard (Copilot review on PR #266): a stamp
    /// alone must not be enough to skip fetching — the required artifacts
    /// (id.def, connection matrix, ≥1 dictionary*.txt) must still be on disk.
    #[test]
    fn test_required_files_present() {
        let dir = std::env::temp_dir().join("lexime_test_mozc_required");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        // Empty dir → incomplete.
        assert!(!required_files_present(&dir));

        // Build up piece by piece; only the full set passes.
        fs::write(dir.join("id.def"), "x").unwrap();
        assert!(!required_files_present(&dir));
        fs::write(dir.join("connection_single_column.txt"), "x").unwrap();
        assert!(!required_files_present(&dir));
        // A user-placed dictionary must NOT satisfy the shard requirement
        // (Codex + Copilot review on PR #266) — only dictionary<digits>.txt.
        fs::write(dir.join("dictionary-custom.txt"), "x").unwrap();
        assert!(!required_files_present(&dir));
        fs::write(dir.join("dictionary00.txt"), "x").unwrap();
        assert!(required_files_present(&dir));

        // Losing any one required artifact flips it back to incomplete.
        fs::remove_file(dir.join("id.def")).unwrap();
        assert!(!required_files_present(&dir));

        fs::remove_dir_all(&dir).ok();
    }

    /// A SHA mismatch must remove every fetched artifact — leaving id.def from
    /// snapshot A next to dictionary*.txt from snapshot B silently shifts POS
    /// ids and corrupts conversion quality. With a manifest in the stamp, the
    /// wipe deletes exactly the recorded files; user-placed files survive
    /// (Codex review on PR #266).
    #[test]
    fn test_wipe_cache_manifest_mode_spares_user_files() {
        let dir = std::env::temp_dir().join("lexime_test_mozc_wipe");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let fetched = [
            "dictionary00.txt",
            "dictionary09.txt",
            "id.def",
            "connection_single_column.txt",
            "LICENSE",
        ];
        for name in fetched {
            fs::write(dir.join(name), "stale").unwrap();
        }
        let stamp_contents = format!(
            "fedcba9876543210fedcba9876543210fedcba98\n{}",
            fetched.join("\n")
        );
        fs::write(dir.join(".stamp"), &stamp_contents).unwrap();
        // User-placed files must survive the wipe — including one that
        // parse_dir's dictionary*.txt glob matches.
        fs::write(dir.join("notes.md"), "keep me").unwrap();
        fs::write(dir.join("dictionary-custom.txt"), "user entries").unwrap();

        assert!(any_fetched_file_exists(&dir));
        let st = read_stamp(&dir.join(".stamp")).unwrap();
        wipe_cache(&dir, &dir.join(".stamp"), &st.manifest).unwrap();

        for name in fetched {
            assert!(!dir.join(name).exists(), "{name} should have been wiped");
        }
        assert!(!dir.join(".stamp").exists());
        assert!(dir.join("notes.md").exists());
        assert!(dir.join("dictionary-custom.txt").exists());
        assert!(!any_fetched_file_exists(&dir));

        fs::remove_dir_all(&dir).ok();
    }

    /// Without a manifest (legacy SHA-only stamp, or no stamp at all) the
    /// wipe falls back to the exact upstream naming pattern — which still
    /// spares user-placed files like dictionary-custom.txt.
    #[test]
    fn test_wipe_cache_legacy_mode_spares_user_files() {
        let dir = std::env::temp_dir().join("lexime_test_mozc_wipe_legacy");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let fetched = [
            "dictionary00.txt",
            "dictionary09.txt",
            "id.def",
            "connection_single_column.txt",
            "LICENSE",
        ];
        for name in fetched {
            fs::write(dir.join(name), "stale").unwrap();
        }
        // Legacy stamp: SHA only, no manifest.
        fs::write(
            dir.join(".stamp"),
            "fedcba9876543210fedcba9876543210fedcba98",
        )
        .unwrap();
        fs::write(dir.join("notes.md"), "keep me").unwrap();
        fs::write(dir.join("dictionary-custom.txt"), "user entries").unwrap();

        wipe_cache(&dir, &dir.join(".stamp"), &[]).unwrap();

        for name in fetched {
            assert!(!dir.join(name).exists(), "{name} should have been wiped");
        }
        assert!(!dir.join(".stamp").exists());
        assert!(dir.join("notes.md").exists());
        assert!(dir.join("dictionary-custom.txt").exists());

        fs::remove_dir_all(&dir).ok();
    }

    /// A wipe that cannot actually remove a stale file must abort the fetch:
    /// carrying on would put files from two upstream snapshots side by side —
    /// the exact mix this module exists to prevent (Codex review on PR #266).
    #[test]
    fn test_wipe_cache_propagates_removal_failure() {
        let dir = std::env::temp_dir().join("lexime_test_mozc_wipe_fail");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        // Manifest entries that are simply absent are fine (already gone).
        let manifest = vec!["dictionary00.txt".to_string()];
        assert!(wipe_cache(&dir, &dir.join(".stamp"), &manifest).is_ok());

        // A manifest entry that is a non-empty directory makes remove_file
        // fail with something other than NotFound → the wipe must error.
        fs::create_dir_all(dir.join("dictionary00.txt")).unwrap();
        fs::write(dir.join("dictionary00.txt").join("blocker"), "x").unwrap();
        assert!(wipe_cache(&dir, &dir.join(".stamp"), &manifest).is_err());

        // Same failure through the legacy (pattern-scan) path.
        assert!(wipe_cache(&dir, &dir.join(".stamp"), &[]).is_err());

        fs::remove_dir_all(&dir).ok();
    }

    const OLD_SHA: &str = "fedcba9876543210fedcba9876543210fedcba98";
    const NEW_SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    /// Build a cache dir holding an "old" snapshot (files + manifest stamp)
    /// plus user files, and a staging dir holding a "new" snapshot. Returns
    /// (dir, staging, old_manifest, new_manifest).
    fn setup_swap_fixture(
        tag: &str,
    ) -> (
        std::path::PathBuf,
        std::path::PathBuf,
        Vec<String>,
        Vec<String>,
    ) {
        let dir = std::env::temp_dir().join(format!("lexime_test_mozc_swap_{tag}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        // Old snapshot: has a shard (dictionary09.txt) the new one drops.
        let old_manifest: Vec<String> = ["dictionary00.txt", "dictionary09.txt", "id.def"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        for name in &old_manifest {
            fs::write(dir.join(name), "old").unwrap();
        }
        write_stamp(&dir.join(".stamp"), OLD_SHA, &old_manifest).unwrap();

        // User files that must survive any swap.
        fs::write(dir.join("notes.md"), "keep me").unwrap();
        fs::write(dir.join("dictionary-custom.txt"), "user entries").unwrap();

        // New snapshot fully staged.
        let new_manifest: Vec<String> = ["dictionary00.txt", "id.def", "LICENSE"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let staging = dir.join(".staging");
        fs::create_dir_all(&staging).unwrap();
        for name in &new_manifest {
            fs::write(staging.join(name), "new").unwrap();
        }

        (dir, staging, old_manifest, new_manifest)
    }

    /// Happy path of the transactional swap (Codex review R3 on PR #266):
    /// old snapshot files go away — including a shard the new snapshot no
    /// longer has — staged files land under their final names, the stamp
    /// records the new SHA + manifest, staging is gone, user files survive.
    #[test]
    fn test_commit_staged_swaps_snapshots() {
        let (dir, staging, old_manifest, new_manifest) = setup_swap_fixture("ok");
        let stamp = dir.join(".stamp");

        commit_staged(
            &dir,
            &stamp,
            &staging,
            &old_manifest,
            NEW_SHA,
            &new_manifest,
        )
        .unwrap();

        for name in &new_manifest {
            assert_eq!(fs::read_to_string(dir.join(name)).unwrap(), "new");
        }
        assert!(
            !dir.join("dictionary09.txt").exists(),
            "shard dropped upstream must not survive the swap"
        );
        assert!(!staging.exists(), "staging dir should be removed");
        let st = read_stamp(&stamp).unwrap();
        assert_eq!(st.sha, NEW_SHA);
        assert_eq!(st.manifest, new_manifest);
        assert!(dir.join("notes.md").exists());
        assert!(dir.join("dictionary-custom.txt").exists());

        fs::remove_dir_all(&dir).ok();
    }

    /// If staging is missing any manifest file, the commit must fail BEFORE
    /// touching dest: old snapshot files and the old stamp stay intact, so
    /// the working cache (and the offline fallback) is never sacrificed to
    /// an incomplete download.
    #[test]
    fn test_commit_staged_incomplete_staging_leaves_dest_untouched() {
        let (dir, staging, old_manifest, new_manifest) = setup_swap_fixture("incomplete");
        let stamp = dir.join(".stamp");
        fs::remove_file(staging.join("id.def")).unwrap();

        let result = commit_staged(
            &dir,
            &stamp,
            &staging,
            &old_manifest,
            NEW_SHA,
            &new_manifest,
        );
        assert!(result.is_err());

        // Old snapshot untouched and still trusted.
        for name in &old_manifest {
            assert_eq!(fs::read_to_string(dir.join(name)).unwrap(), "old");
        }
        let st = read_stamp(&stamp).unwrap();
        assert_eq!(st.sha, OLD_SHA);
        assert_eq!(st.manifest, old_manifest);

        fs::remove_dir_all(&dir).ok();
    }

    /// Legacy path: no old manifest (SHA-only stamp) → the wipe inside the
    /// commit falls back to the upstream naming pattern. Old upstream-named
    /// files are still replaced/removed and user files still survive.
    #[test]
    fn test_commit_staged_legacy_old_manifest() {
        let (dir, staging, _old_manifest, new_manifest) = setup_swap_fixture("legacy");
        let stamp = dir.join(".stamp");
        // Downgrade to a legacy SHA-only stamp.
        fs::write(&stamp, OLD_SHA).unwrap();

        commit_staged(&dir, &stamp, &staging, &[], NEW_SHA, &new_manifest).unwrap();

        for name in &new_manifest {
            assert_eq!(fs::read_to_string(dir.join(name)).unwrap(), "new");
        }
        assert!(!dir.join("dictionary09.txt").exists());
        assert!(dir.join("notes.md").exists());
        assert!(dir.join("dictionary-custom.txt").exists());
        let st = read_stamp(&stamp).unwrap();
        assert_eq!(st.sha, NEW_SHA);

        fs::remove_dir_all(&dir).ok();
    }
}
