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
use std::io::{self, Cursor};
use std::path::Path;

use super::{Candidate, CandidateError};

/// Sudachi CDN serving the dictionary CSVs. Mirrors `sudachi.s3.ap-northeast-1`.
const SUDACHI_CDN_BASE: &str = "https://d2ej7fkh96fzlu.cloudfront.net/sudachidict-raw";

/// S3 ListBucket URL for discovering the latest version directory.
const SUDACHI_S3_LIST_URL: &str =
    "https://sudachi.s3.ap-northeast-1.amazonaws.com/?list-type=2&prefix=sudachidict-raw/&delimiter=/";

/// All three lexicon ZIPs to fetch for the full dictionary. Order matters
/// only for log clarity; entries from all three are merged after parse.
const SUDACHI_ZIPS: &[&str] = &["small_lex.zip", "core_lex.zip", "notcore_lex.zip"];

/// Fetch SudachiDict-full into `dest` (idempotent — skips files that already exist).
/// Returns the version string of what was fetched.
pub fn fetch(dest: &Path) -> Result<String, CandidateError> {
    fs::create_dir_all(dest)?;

    let version = latest_version()?;
    eprintln!(
        "Downloading SudachiDict v{version} to {}...",
        dest.display()
    );

    for zip_name in SUDACHI_ZIPS {
        let csv_name = zip_name.replace(".zip", ".csv");
        let csv_path = dest.join(&csv_name);
        if csv_path.exists() {
            eprintln!("  {csv_name} (already exists, skipping)");
            continue;
        }
        let url = format!("{SUDACHI_CDN_BASE}/{version}/{zip_name}");
        eprintln!("  {zip_name}");
        download_and_extract(&url, ".csv", dest)?;
    }

    fs::write(dest.join(".stamp"), &version)?;
    Ok(version)
}

/// Parse all `*_lex.csv` files in `dir` and return a multimap of
/// `reading -> [Candidate]`. Each row that fails the basic shape check
/// (column count, hiragana-able reading, non-empty surface) is skipped.
pub fn parse_dir(dir: &Path) -> Result<HashMap<String, Vec<Candidate>>, CandidateError> {
    let mut entries: HashMap<String, Vec<Candidate>> = HashMap::new();
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
        let content = fs::read_to_string(&path)?;
        for line in content.lines() {
            total += 1;
            if line.is_empty() || line.starts_with('#') {
                skipped += 1;
                continue;
            }
            let fields: Vec<&str> = line.split(',').collect();
            // Need at least cols 0..=11 (surface, ids, cost, POS×6, reading).
            if fields.len() < 12 {
                skipped += 1;
                continue;
            }
            let surface = fields[0];
            let cost: i32 = match fields[3].parse() {
                Ok(c) => c,
                Err(_) => {
                    skipped += 1;
                    continue;
                }
            };
            let reading = kata_to_hira(fields[11]);
            if reading.is_empty() || !is_hiragana(&reading) || surface.is_empty() {
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
            let pos = fields[5..11.min(fields.len())]
                .iter()
                .filter(|s| !s.is_empty() && **s != "*")
                .copied()
                .collect::<Vec<_>>()
                .join("-");
            entries.entry(reading.clone()).or_default().push(Candidate {
                reading,
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
                if !dir.is_empty() && dir.chars().all(|c| c.is_ascii_digit()) {
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

fn download_and_extract(url: &str, suffix: &str, dest: &Path) -> Result<usize, CandidateError> {
    let body = ureq::get(url)
        .call()
        .map_err(|e| CandidateError::Http(format!("{url}: {e}")))?
        .into_body()
        .with_config()
        .limit(500 * 1024 * 1024)
        .read_to_vec()
        .map_err(|e| CandidateError::Http(format!("{url}: {e}")))?;

    let cursor = Cursor::new(body);
    let mut archive = zip::ZipArchive::new(cursor).map_err(zip_err)?;
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
        let mut out = fs::File::create(&out_path)?;
        io::copy(&mut file, &mut out)?;
        eprintln!("    → {basename}");
        count += 1;
    }
    Ok(count)
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
    fn test_parse_dir_18col_csv() {
        let dir = std::env::temp_dir().join("lexime_test_candidates_sudachi");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        // Sudachi 18-col format:
        //   0=surface, 1-3=ids/cost, 4=normalized,
        //   5-10=POS, 11=reading (katakana), 12+=metadata
        fs::write(
            dir.join("core_lex.csv"),
            "藤椒,1847,1847,8000,藤椒,名詞,普通名詞,一般,*,*,*,タンジャオ,藤椒,*,A,*,*,*,1\n\
             東京都,1847,1847,4500,東京都,名詞,固有名詞,地名,一般,*,*,トウキョウト,東京都,*,A,*,*,*,2\n\
             ABC,100,100,3000,ABC,名詞,普通名詞,一般,*,*,*,ABC,ABC,*,A,*,*,*,3\n",
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
        assert_eq!(toukyou[0].pos, "名詞-固有名詞-地名-一般");

        fs::remove_dir_all(&dir).ok();
    }
}
