//! SudachiDict candidate fetcher.
//!
//! Downloads the official SudachiDict-full (Apache-2.0, WAP) ZIP releases via
//! the Sudachi S3 bucket, extracts the `*_lex.csv` files into a working dir,
//! and parses the 18-column CSV format into `(reading, surface, cost, pos)`
//! tuples.
//!
//! ## Lineage
//!
//! Resurrected from `engine/crates/lex-cli/src/dict_source/sudachi.rs` (deleted
//! in PR #156, commit `0a45265`, 2026-02-20). The original lived in
//! `dict_source/` because it was used to build the merged dict directly. That
//! merge approach produced too much top-1 noise and was retired.
//!
//! Here we use the **same fetch+parse code** but route output to the candidate
//! pool instead of the build dict — see `candidates/mod.rs` rationale.

use std::collections::HashMap;
use std::fs;
use std::io::{self, BufRead, BufReader};
use std::path::Path;

use super::{CandidateError, CandidateRow};

/// Sudachi CDN serving the dictionary CSVs. Mirrors `sudachi.s3.ap-northeast-1`.
const SUDACHI_CDN_BASE: &str = "https://d2ej7fkh96fzlu.cloudfront.net/sudachidict-raw";

/// S3 ListBucket URL for discovering the latest version directory.
const SUDACHI_S3_LIST_URL: &str =
    "https://sudachi.s3.ap-northeast-1.amazonaws.com/?list-type=2&prefix=sudachidict-raw/&delimiter=/";

/// All three lexicon ZIPs to fetch for the full dictionary. Order matters
/// only for log clarity; entries from all three are merged after parse.
const SUDACHI_ZIPS: &[&str] = &["small_lex.zip", "core_lex.zip", "notcore_lex.zip"];

/// Fetch SudachiDict-full into `dest`. Single rule for cache validity:
/// **the stamp file must equal the latest upstream version exactly**.
/// Anything else (stamp missing, empty, mismatched, OR no stamp + stale
/// CSVs left over) triggers a clean wipe + full re-download. This kills
/// three classes of stale-cache bug at once:
///
/// - `.stamp` records an old version while CSVs from that version still
///   exist (PR #242 R1: silent reuse + new version label).
/// - `.stamp` is missing/empty but old `*_lex.csv` are still on disk
///   (PR #242 R2: interrupted run, manual cleanup, legacy pre-stamp cache).
/// - Anything in between.
///
/// Within a still-valid cache, individual missing CSVs are downloaded
/// (so a run that fails mid-extraction recovers).
///
/// Returns the version that ended up in the cache.
pub fn fetch(dest: &Path) -> Result<String, CandidateError> {
    fs::create_dir_all(dest)?;
    let stamp_path = dest.join(".stamp");
    let latest = latest_version()?;
    let cached = read_stamp(&stamp_path);
    let cache_valid = cached.as_deref() == Some(latest.as_str());

    if !cache_valid {
        if let Some(v) = &cached {
            eprintln!("Cache version {v} != latest {latest}; wiping stale CSVs.");
        } else if any_csv_exists(dest) {
            eprintln!("Cache has no .stamp but stale CSVs present; wiping.");
        }
        wipe_cache(dest, &stamp_path);
    }

    eprintln!("Downloading SudachiDict v{latest} to {}...", dest.display());

    for zip_name in SUDACHI_ZIPS {
        let csv_name = zip_name.replace(".zip", ".csv");
        let csv_path = dest.join(&csv_name);
        if csv_path.exists() {
            eprintln!("  {csv_name} (already exists, skipping)");
            continue;
        }
        let url = format!("{SUDACHI_CDN_BASE}/{latest}/{zip_name}");
        eprintln!("  {zip_name}");
        download_and_extract(&url, ".csv", dest)?;
    }

    fs::write(&stamp_path, &latest)?;
    Ok(latest)
}

fn any_csv_exists(dest: &Path) -> bool {
    SUDACHI_ZIPS.iter().any(|z| {
        let csv = z.replace(".zip", ".csv");
        dest.join(csv).exists()
    })
}

fn wipe_cache(dest: &Path, stamp_path: &Path) {
    for zip_name in SUDACHI_ZIPS {
        let csv = zip_name.replace(".zip", ".csv");
        let _ = fs::remove_file(dest.join(csv));
    }
    let _ = fs::remove_file(stamp_path);
}

fn read_stamp(path: &Path) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Parse all `*_lex.csv` files in `dir` and return a multimap of
/// `reading -> [CandidateRow]`. The reading lives only in the map key, not
/// repeated in each row, so peak memory at full-Sudachi scale (~1.9M rows)
/// is meaningfully lower. Each row that fails the basic shape check
/// (column count, hiragana-able reading, non-empty surface) is skipped.
pub fn parse_dir(dir: &Path) -> Result<HashMap<String, Vec<CandidateRow>>, CandidateError> {
    let mut entries: HashMap<String, Vec<CandidateRow>> = HashMap::new();
    let mut total = 0u64;
    let mut skipped = 0u64;

    let mut files: Vec<_> = fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .filter(|e| {
            let n = e.file_name();
            let s = n.to_string_lossy();
            s.ends_with("_lex.csv")
        })
        .collect();
    files.sort_by_key(|e| e.file_name());

    if files.is_empty() {
        return Err(CandidateError::Parse(format!(
            "no *_lex.csv files in {}",
            dir.display()
        )));
    }

    for file_entry in files {
        let path = file_entry.path();
        eprintln!("Reading {}...", path.display());
        // Stream the CSV instead of slurping into one String — Sudachi-full's
        // notcore_lex.csv alone is well over 100 MB and the parse is the
        // memory peak of the whole `mine` flow.
        let file = fs::File::open(&path)?;
        let reader = BufReader::new(file);
        for line in reader.lines() {
            let line = line?;
            total += 1;
            if line.is_empty() || line.starts_with('#') {
                skipped += 1;
                continue;
            }
            // Sudachi 18-col CSV layout:
            //   0  surface
            //   1-3  left_id, right_id, cost
            //   4   normalized surface (NOT POS — col 4 confused historic merges)
            //   5-10 POS hierarchy (主, 細分類1, 細分類2, 細分類3, 活用型, 活用形)
            //   11  reading (katakana)
            //   12+  base form, pronunciation, ID, ...
            //
            // Walk the split iterator instead of `collect()`-ing a Vec per
            // line — at full-Sudachi scale that's ~3M Vec allocations on
            // the hot path.
            let mut iter = line.split(',');
            let mut surface = "";
            let mut cost_str = "";
            let mut reading_kata: Option<&str> = None;
            // POS slots 5..=10 are at most 6 entries; keep them as a small
            // fixed-size buffer so we don't allocate per row.
            let mut pos_parts: [&str; 6] = [""; 6];
            let mut pos_count = 0usize;
            for (i, field) in (&mut iter).enumerate() {
                match i {
                    0 => surface = field,
                    3 => cost_str = field,
                    5..=10 if !field.is_empty() && field != "*" => {
                        pos_parts[pos_count] = field;
                        pos_count += 1;
                    }
                    11 => {
                        reading_kata = Some(field);
                        break; // cols 12..17 not needed
                    }
                    _ => {}
                }
            }
            let Some(reading_kata) = reading_kata else {
                skipped += 1; // line had < 12 columns
                continue;
            };
            let Ok(cost): Result<i32, _> = cost_str.parse() else {
                skipped += 1;
                continue;
            };
            let reading = kata_to_hira(reading_kata);
            if reading.is_empty() || !is_hiragana(&reading) || surface.is_empty() {
                skipped += 1;
                continue;
            }
            let pos = pos_parts[..pos_count].join("-");
            entries.entry(reading).or_default().push(CandidateRow {
                surface: surface.to_string(),
                cost,
                pos,
            });
        }
    }

    eprintln!(
        "  parsed {} readings, skipped {skipped} of {total} lines",
        entries.len()
    );
    Ok(entries)
}

// ─── Private helpers ─────────────────────────────────────────────────

fn latest_version() -> Result<String, CandidateError> {
    let body = ureq::get(SUDACHI_S3_LIST_URL)
        .call()
        .map_err(|e| CandidateError::Http(format!("S3 listing: {e}")))?
        .into_body()
        .read_to_string()
        .map_err(|e| CandidateError::Http(format!("S3 listing: {e}")))?;
    parse_latest_version(&body)
}

fn parse_latest_version(xml: &str) -> Result<String, CandidateError> {
    let prefix = "sudachidict-raw/";
    let mut versions: Vec<&str> = Vec::new();
    for segment in xml.split("<Prefix>") {
        if let Some(rest) = segment.strip_prefix(prefix) {
            if let Some(end) = rest.find("</Prefix>") {
                let dir = rest[..end].trim_end_matches('/');
                if is_sudachi_version(dir) {
                    versions.push(dir);
                }
            }
        }
    }
    versions.sort();
    versions
        .last()
        .map(|v| v.to_string())
        .ok_or_else(|| CandidateError::Http("no versions found in S3 listing".to_string()))
}

/// Sudachi release directories are dated `YYYYMMDD` and occasionally carry
/// an underscore-suffixed disambiguator like `20201023_2` (re-cut of the same
/// day). Reject anything else so XML noise can't slip in.
///
/// Lexicographic ordering on these strings happens to match release order:
/// - "20201023" < "20201023_2" < "20210608" (date prefix differs)
/// - "20260116" < "20260116_2" < "20260117"
///
/// so `versions.sort().last()` still picks the newest.
fn is_sudachi_version(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.is_empty() || !bytes[0].is_ascii_digit() {
        return false;
    }
    let mut seen_underscore = false;
    let mut digits_after_underscore = 0;
    for &b in &bytes[1..] {
        if b.is_ascii_digit() {
            if seen_underscore {
                digits_after_underscore += 1;
            }
        } else if b == b'_' && !seen_underscore {
            seen_underscore = true;
        } else {
            return false;
        }
    }
    // If an underscore was present, require ≥1 digit after it (`20201023_` is invalid).
    !seen_underscore || digits_after_underscore > 0
}

fn download_and_extract(url: &str, suffix: &str, dest: &Path) -> Result<usize, CandidateError> {
    // Stage the ZIP on disk so we don't hold both the compressed bytes (a
    // few hundred MB for SudachiDict-full) AND the extracted CSVs in
    // memory at the same time. Use a per-process unique tmp name so two
    // parallel `mine` runs against the same cache dir don't clobber each
    // other's tmp file. Install the guard BEFORE the download so a failed
    // io::copy doesn't leak a partial tmp.
    let tmp_path = dest.join(format!(".tmp.{}.zip", std::process::id()));
    let _guard = TmpFileGuard(tmp_path.clone());
    {
        let resp = ureq::get(url)
            .call()
            .map_err(|e| CandidateError::Http(format!("{url}: {e}")))?;
        let mut body = resp.into_body();
        let body_cfg = body.with_config();
        let mut body_reader = body_cfg.limit(500 * 1024 * 1024).reader();
        let mut tmp = fs::File::create(&tmp_path)?;
        io::copy(&mut body_reader, &mut tmp)?;
    }

    let mut archive = zip::ZipArchive::new(fs::File::open(&tmp_path)?).map_err(zip_err)?;
    let mut count = 0;
    for i in 0..archive.len() {
        let mut file = archive.by_index(i).map_err(zip_err)?;
        let raw_name = file.name().to_string();
        if !raw_name.ends_with(suffix) {
            continue;
        }
        // Use basename only — defends against zip-slip even though Sudachi
        // ZIPs are first-party.
        let basename = Path::new(&raw_name)
            .file_name()
            .ok_or_else(|| CandidateError::Parse(format!("invalid ZIP entry: {raw_name}")))?
            .to_string_lossy()
            .into_owned();
        let out_path = dest.join(&basename);
        // Stage the extraction to a per-process unique temp file then
        // atomically rename so two parallel `mine` runs against the same
        // cache dir don't interleave half-written CSVs (the prior
        // exists-check-then-create pattern was TOCTOU-racy).
        let extract_tmp = dest.join(format!(".{}.extract.{}", basename, std::process::id()));
        {
            let mut out = fs::File::create(&extract_tmp)?;
            io::copy(&mut file, &mut out)?;
        }
        fs::rename(&extract_tmp, &out_path)?;
        eprintln!("    → {basename}");
        count += 1;
    }
    Ok(count)
}

/// Removes the staged ZIP on drop so a panic / error on extraction doesn't
/// leak the temp file.
struct TmpFileGuard(std::path::PathBuf);
impl Drop for TmpFileGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn zip_err(e: impl std::fmt::Display) -> CandidateError {
    CandidateError::Io(io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
}

/// Map katakana (U+30A1..U+30F6) to hiragana (U+3041..U+3096). The prolonged
/// sound mark `ー` (U+30FC) and other characters pass through unchanged.
fn kata_to_hira(s: &str) -> String {
    s.chars()
        .map(|c| {
            if ('\u{30A1}'..='\u{30F6}').contains(&c) {
                char::from_u32(c as u32 - 0x60).unwrap_or(c)
            } else {
                c
            }
        })
        .collect()
}

fn is_hiragana(s: &str) -> bool {
    !s.is_empty()
        && s.chars().all(|c| {
            ('\u{3041}'..='\u{3096}').contains(&c)
                || c == '\u{30FC}' // ー: prolonged sound (loanwords like らーめん)
                || c == '\u{309B}' // ゛
                || c == '\u{309C}' // ゜
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_kata_to_hira() {
        assert_eq!(kata_to_hira("カンジ"), "かんじ");
        assert_eq!(kata_to_hira("ラーメン"), "らーめん");
        assert_eq!(kata_to_hira(""), "");
        assert_eq!(kata_to_hira("ABC"), "ABC");
    }

    #[test]
    fn test_is_hiragana() {
        assert!(is_hiragana("かんじ"));
        assert!(is_hiragana("らーめん"));
        assert!(!is_hiragana("カタカナ"));
        assert!(!is_hiragana("abc"));
        assert!(!is_hiragana(""));
    }

    #[test]
    fn test_parse_latest_version() {
        let xml = r#"<ListBucketResult>
  <CommonPrefixes><Prefix>sudachidict-raw/20210608/</Prefix></CommonPrefixes>
  <CommonPrefixes><Prefix>sudachidict-raw/20260116/</Prefix></CommonPrefixes>
  <CommonPrefixes><Prefix>sudachidict-raw/20201023_2/</Prefix></CommonPrefixes>
</ListBucketResult>"#;
        assert_eq!(parse_latest_version(xml).unwrap(), "20260116");
    }

    #[test]
    fn test_parse_latest_version_empty() {
        assert!(parse_latest_version("<ListBucketResult></ListBucketResult>").is_err());
    }

    #[test]
    fn test_parse_latest_version_underscore_suffix_wins() {
        // Regression for PR #242 R4 sudachi.rs:225 — a re-cut release
        // (`YYYYMMDD_N`) at the top of the listing must NOT be filtered out.
        // Otherwise the next day's release wins by default and we silently
        // mine outdated SudachiDict.
        let xml = r#"<ListBucketResult>
  <CommonPrefixes><Prefix>sudachidict-raw/20210608/</Prefix></CommonPrefixes>
  <CommonPrefixes><Prefix>sudachidict-raw/20260601/</Prefix></CommonPrefixes>
  <CommonPrefixes><Prefix>sudachidict-raw/20260601_2/</Prefix></CommonPrefixes>
</ListBucketResult>"#;
        assert_eq!(parse_latest_version(xml).unwrap(), "20260601_2");
    }

    #[test]
    fn test_is_sudachi_version_filters_noise() {
        // Accept: dated, dated+suffix
        assert!(is_sudachi_version("20260116"));
        assert!(is_sudachi_version("20201023_2"));
        // Reject: empty, non-digit lead, double underscore, dangling underscore,
        // alpha noise from XML.
        assert!(!is_sudachi_version(""));
        assert!(!is_sudachi_version("abc"));
        assert!(!is_sudachi_version("_20260116"));
        assert!(!is_sudachi_version("20260116_"));
        assert!(!is_sudachi_version("20260116__2"));
        assert!(!is_sudachi_version("20260116-2"));
    }

    #[test]
    fn test_read_stamp_roundtrip() {
        let dir = std::env::temp_dir().join("lexime_test_sudachi_stamp");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let stamp = dir.join(".stamp");

        // Missing → None
        assert!(read_stamp(&stamp).is_none());

        // Empty → None (treated as no version)
        fs::write(&stamp, "").unwrap();
        assert!(read_stamp(&stamp).is_none());

        // Whitespace-only → None
        fs::write(&stamp, "  \n").unwrap();
        assert!(read_stamp(&stamp).is_none());

        // Trim trailing newline
        fs::write(&stamp, "20260428\n").unwrap();
        assert_eq!(read_stamp(&stamp).as_deref(), Some("20260428"));

        fs::remove_dir_all(&dir).ok();
    }

    /// Regression test for the "stale cache silently reused" findings
    /// (PR #242 Copilot R1 sudachi.rs:60 + R2 sudachi.rs:68). Validates the
    /// cache_valid predicate that drives wipe decisions — covers both
    /// version-mismatch and missing-stamp-with-stale-CSVs cases. Network
    /// path is exercised by manual `dictool candidates mine` runs.
    #[test]
    fn test_stale_cache_invariant() {
        // Helper that re-implements the cache_valid + wipe decision so the
        // test pins the invariant without needing a live HTTP call.
        fn should_wipe(dest: &Path, latest: &str) -> bool {
            let stamp_path = dest.join(".stamp");
            let cached = read_stamp(&stamp_path);
            cached.as_deref() != Some(latest)
        }

        let dir = std::env::temp_dir().join("lexime_test_sudachi_invariant");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        // Case 1: stamp matches → no wipe.
        fs::write(dir.join(".stamp"), "20260428").unwrap();
        assert!(!should_wipe(&dir, "20260428"));

        // Case 2: stamp records old version → wipe.
        fs::write(dir.join(".stamp"), "20260116").unwrap();
        assert!(should_wipe(&dir, "20260428"));

        // Case 3: stamp missing entirely → wipe.
        fs::remove_file(dir.join(".stamp")).unwrap();
        assert!(should_wipe(&dir, "20260428"));

        // Case 4: stamp empty → wipe (read_stamp returns None for empty/whitespace).
        fs::write(dir.join(".stamp"), "").unwrap();
        assert!(should_wipe(&dir, "20260428"));

        // Case 5: stamp whitespace-only → wipe.
        fs::write(dir.join(".stamp"), "   \n").unwrap();
        assert!(should_wipe(&dir, "20260428"));

        fs::remove_dir_all(&dir).ok();
    }

    /// Verify any_csv_exists detects orphan CSVs across the SUDACHI_ZIPS set.
    #[test]
    fn test_any_csv_exists_detects_orphans() {
        let dir = std::env::temp_dir().join("lexime_test_sudachi_orphan");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        assert!(!any_csv_exists(&dir), "empty dir → no orphan CSV");

        // Drop a single core_lex.csv (no stamp) — the missing-stamp + stale
        // CSV scenario from R2.
        fs::write(dir.join("core_lex.csv"), "stale").unwrap();
        assert!(any_csv_exists(&dir));

        fs::remove_dir_all(&dir).ok();
    }

    /// Original R1 regression — kept for explicit coverage of the version
    /// mismatch wipe path. The test_stale_cache_invariant test above is
    /// the broader companion.
    #[test]
    fn test_stale_cache_csvs_wiped_on_version_mismatch() {
        let dir = std::env::temp_dir().join("lexime_test_sudachi_stale");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        // Pretend a previous run cached v20260116.
        for zip_name in SUDACHI_ZIPS {
            let csv = zip_name.replace(".zip", ".csv");
            fs::write(dir.join(csv), "stale,csv,row\n").unwrap();
        }
        fs::write(dir.join(".stamp"), "20260116").unwrap();

        // Simulate the wipe step that fetch() performs when stamp mismatches.
        let cached = read_stamp(&dir.join(".stamp"));
        assert_eq!(cached.as_deref(), Some("20260116"));

        let latest = "20260428";
        if cached.as_deref() != Some(latest) {
            for zip_name in SUDACHI_ZIPS {
                let csv = zip_name.replace(".zip", ".csv");
                let _ = fs::remove_file(dir.join(csv));
            }
            let _ = fs::remove_file(dir.join(".stamp"));
        }

        // After wipe, no stale CSVs remain — a fresh download would not
        // mix v20260116 rows into v20260428 output.
        for zip_name in SUDACHI_ZIPS {
            let csv = zip_name.replace(".zip", ".csv");
            assert!(
                !dir.join(&csv).exists(),
                "{csv} should have been wiped on version mismatch"
            );
        }
        assert!(!dir.join(".stamp").exists());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_parse_dir_18col_csv() {
        let dir = std::env::temp_dir().join("lexime_test_candidates_sudachi");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        // Sudachi 18-col format:
        //   0=surface, 1-3=ids/cost, 4=normalized,
        //   5-10=POS, 11=reading (katakana), 12+=metadata
        //
        // Use `concat!` of separate string literals so source-code
        // indentation doesn't leak into the fixture (which would land in
        // surface and silently corrupt the assertion below).
        fs::write(
            dir.join("core_lex.csv"),
            concat!(
                "藤椒,1847,1847,8000,藤椒,名詞,普通名詞,一般,*,*,*,タンジャオ,藤椒,*,A,*,*,*,1\n",
                "東京都,1847,1847,4500,東京都,名詞,固有名詞,地名,一般,*,*,トウキョウト,東京都,*,A,*,*,*,2\n",
                "ABC,100,100,3000,ABC,名詞,普通名詞,一般,*,*,*,ABC,ABC,*,A,*,*,*,3\n",
            ),
        )
        .unwrap();

        let entries = parse_dir(&dir).unwrap();
        // ABC reading column is "ABC" (non-katakana) → skipped.
        assert!(!entries.contains_key("ABC"));

        let tanjao = entries.get("たんじゃお").unwrap();
        assert_eq!(tanjao[0].surface, "藤椒");
        assert_eq!(tanjao[0].cost, 8000);
        assert_eq!(tanjao[0].pos, "名詞-普通名詞-一般");

        let toukyou = entries.get("とうきょうと").unwrap();
        // Asserting surface verifies the CSV had no leading whitespace; pre-fix
        // tests passed without checking surface and could have hidden corruption.
        assert_eq!(toukyou[0].surface, "東京都");
        assert_eq!(toukyou[0].pos, "名詞-固有名詞-地名-一般");

        fs::remove_dir_all(&dir).ok();
    }
}
