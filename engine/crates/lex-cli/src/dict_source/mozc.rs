use std::collections::HashMap;
use std::fs;
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
    fn download_file(url: &str, dest: &Path) -> Result<(), DictSourceError> {
        let body = ureq::get(url)
            .call()
            .map_err(|e| DictSourceError::Http(format!("{url}: {e}")))?
            .into_body()
            .read_to_vec()
            .map_err(|e| DictSourceError::Http(format!("{url}: {e}")))?;
        fs::write(dest, &body).map_err(DictSourceError::Io)?;
        Ok(())
    }

    /// Resolve the latest commit SHA of `master` via the GitHub Commits API.
    /// All raw-file downloads in a fetch run are pinned to this single SHA so
    /// the cached snapshot can never mix files from two upstream states.
    fn latest_commit_sha() -> Result<String, DictSourceError> {
        let url = format!("{MOZC_API_BASE}/commits/master");
        let body = ureq::get(&url)
            .call()
            .map_err(|e| DictSourceError::Http(format!("GitHub API: {e}")))?
            .into_body()
            .read_to_string()
            .map_err(|e| DictSourceError::Http(format!("GitHub API: {e}")))?;
        parse_commit_sha(&body)
    }

    /// List dictionary files via GitHub Contents API **pinned to `sha`** and
    /// return (name, download_url) pairs for `dictionary*.txt` and `id.def`.
    /// The returned `download_url`s point at the same pinned SHA.
    fn list_remote_files(sha: &str) -> Result<Vec<(String, String)>, DictSourceError> {
        let url = format!("{MOZC_API_BASE}/contents/src/data/dictionary_oss?ref={sha}");
        let body = ureq::get(&url)
            .call()
            .map_err(|e| DictSourceError::Http(format!("GitHub API: {e}")))?
            .into_body()
            .read_to_string()
            .map_err(|e| DictSourceError::Http(format!("GitHub API: {e}")))?;
        parse_remote_files(&body)
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

/// Read the version stamp. Only a well-formed commit SHA counts as a version:
/// missing / empty / legacy pre-versioning empty `.stamp` / corrupted or
/// hand-edited contents all return `None`, which the caller treats as
/// "no version" (wipe + full re-download).
fn read_stamp(path: &Path) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| is_valid_sha(s))
}

/// True if `name` is one of the files this source fetches into the cache dir.
fn is_fetched_file(name: &str) -> bool {
    (name.starts_with("dictionary") && name.ends_with(".txt"))
        || name == "id.def"
        || name == "connection_single_column.txt"
        || name == "LICENSE"
}

/// True if the files required by downstream consumers are all present:
/// `id.def`, `connection_single_column.txt`, and at least one
/// `dictionary*.txt`. Used by the offline fallback — a stamp alone is not
/// enough evidence if the artifacts themselves have since been deleted.
fn required_files_present(dest: &Path) -> bool {
    let any_dictionary = fs::read_dir(dest)
        .map(|rd| {
            rd.filter_map(|e| e.ok()).any(|e| {
                let n = e.file_name();
                let s = n.to_string_lossy();
                s.starts_with("dictionary") && s.ends_with(".txt")
            })
        })
        .unwrap_or(false);
    any_dictionary
        && dest.join("id.def").exists()
        && dest.join("connection_single_column.txt").exists()
}

/// True if any previously fetched file exists in `dest`.
fn any_fetched_file_exists(dest: &Path) -> bool {
    fs::read_dir(dest)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .any(|e| is_fetched_file(&e.file_name().to_string_lossy()))
        })
        .unwrap_or(false)
}

/// Remove all fetched files plus the stamp so a re-download starts clean.
fn wipe_cache(dest: &Path, stamp_path: &Path) {
    if let Ok(rd) = fs::read_dir(dest) {
        for entry in rd.filter_map(|e| e.ok()) {
            if is_fetched_file(&entry.file_name().to_string_lossy()) {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
    let _ = fs::remove_file(stamp_path);
}

/// Parse GitHub Contents API JSON and return (name, download_url) pairs
/// for `dictionary*.txt` and `id.def`.
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
        let wanted = (name.starts_with("dictionary") && name.ends_with(".txt")) || name == "id.def";
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
    /// files) triggers a clean wipe + full re-download.
    ///
    /// Crucially, every download in a run is pinned to that one SHA via
    /// `raw.githubusercontent.com/google/mozc/<sha>/...`, so `id.def` and
    /// `dictionary*.txt` can never come from two different upstream states —
    /// neither across runs (stale per-file cache) nor within a run (upstream
    /// moving while we fetch). A skew there silently shifts POS ids and
    /// corrupts conversion quality without any visible error.
    ///
    /// Within a still-valid cache (stamp == latest SHA), individual missing
    /// files (e.g. deleted by hand after a successful fetch) are re-downloaded
    /// from that same pinned SHA. An interrupted run never writes a stamp, so
    /// it takes the wipe + full re-download path on the next attempt.
    ///
    /// Offline behavior: if the SHA can't be resolved but a cached snapshot
    /// exists (valid stamp — only written after a fully successful fetch) and
    /// the required artifacts are still on disk, warn and keep using it;
    /// otherwise fail.
    fn fetch(&self, dest: &Path) -> Result<(), DictSourceError> {
        fs::create_dir_all(dest).map_err(DictSourceError::Io)?;
        let stamp_path = dest.join(".stamp");
        let cached = read_stamp(&stamp_path);

        let sha = match Self::latest_commit_sha() {
            Ok(sha) => sha,
            Err(e) => {
                if let Some(v) = &cached {
                    if required_files_present(dest) {
                        eprintln!(
                            "Warning: could not resolve latest mozc commit ({e}); \
                             using cached snapshot {v}."
                        );
                        return Ok(());
                    }
                    return Err(DictSourceError::Http(format!(
                        "could not resolve latest mozc commit ({e}) and cached snapshot {v} \
                         in {} is missing required files",
                        dest.display()
                    )));
                }
                return Err(e);
            }
        };

        let cache_valid = cached.as_deref() == Some(sha.as_str());
        if !cache_valid {
            if let Some(v) = &cached {
                eprintln!("Cache commit {v} != latest {sha}; wiping stale files.");
            } else if any_fetched_file_exists(dest) {
                eprintln!("Cache has no version stamp but files present; wiping.");
            }
            wipe_cache(dest, &stamp_path);
        }

        eprintln!(
            "Downloading Mozc dictionary files (commit {sha}) to {}...",
            dest.display()
        );

        let remote_files = Self::list_remote_files(&sha)?;
        for (name, url) in &remote_files {
            let file_path = dest.join(name);
            if file_path.exists() {
                eprintln!("  {name} (already exists, skipping)");
                continue;
            }
            eprintln!("  {name}");
            Self::download_file(url, &file_path)?;
        }

        // Download connection matrix
        let connection = dest.join("connection_single_column.txt");
        if connection.exists() {
            eprintln!("  connection_single_column.txt (already exists, skipping)");
        } else {
            eprintln!("  connection_single_column.txt");
            let url = format!(
                "{MOZC_RAW_BASE}/{sha}/src/data/dictionary_oss/connection_single_column.txt"
            );
            Self::download_file(&url, &connection)?;
        }

        // Download LICENSE
        let license = dest.join("LICENSE");
        if license.exists() {
            eprintln!("  LICENSE (already exists, skipping)");
        } else {
            eprintln!("  LICENSE");
            let url = format!("{MOZC_RAW_BASE}/{sha}/LICENSE");
            Self::download_file(&url, &license)?;
        }

        // Record the snapshot version only after every file landed.
        fs::write(&stamp_path, &sha).map_err(DictSourceError::Io)?;
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

        // Trim trailing newline
        fs::write(&stamp, "0123456789abcdef0123456789abcdef01234567\n").unwrap();
        assert_eq!(
            read_stamp(&stamp).as_deref(),
            Some("0123456789abcdef0123456789abcdef01234567")
        );

        fs::remove_dir_all(&dir).ok();
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
            cached.as_deref() != Some(latest)
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
        fs::write(dir.join("dictionary00.txt"), "x").unwrap();
        assert!(required_files_present(&dir));

        // Losing any one required artifact flips it back to incomplete.
        fs::remove_file(dir.join("id.def")).unwrap();
        assert!(!required_files_present(&dir));

        fs::remove_dir_all(&dir).ok();
    }

    /// A SHA mismatch must remove every fetched artifact — leaving id.def from
    /// snapshot A next to dictionary*.txt from snapshot B silently shifts POS
    /// ids and corrupts conversion quality.
    #[test]
    fn test_wipe_cache_removes_all_fetched_files() {
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
        fs::write(
            dir.join(".stamp"),
            "fedcba9876543210fedcba9876543210fedcba98",
        )
        .unwrap();
        // A user-placed file must survive the wipe.
        fs::write(dir.join("notes.md"), "keep me").unwrap();

        assert!(any_fetched_file_exists(&dir));
        wipe_cache(&dir, &dir.join(".stamp"));

        for name in fetched {
            assert!(!dir.join(name).exists(), "{name} should have been wiped");
        }
        assert!(!dir.join(".stamp").exists());
        assert!(dir.join("notes.md").exists());
        assert!(!any_fetched_file_exists(&dir));

        fs::remove_dir_all(&dir).ok();
    }
}
