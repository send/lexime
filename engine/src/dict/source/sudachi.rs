use std::collections::HashMap;
use std::fs;
use std::io::{self, Cursor};
use std::path::Path;

use super::{is_hiragana, DictSource, DictSourceError};
use crate::dict::DictEntry;

const SUDACHI_BASE_URL: &str = "https://sudachi.s3.ap-northeast-1.amazonaws.com/sudachidict-raw";
const SUDACHI_S3_LIST_URL: &str = "https://sudachi.s3.ap-northeast-1.amazonaws.com/?list-type=2&prefix=sudachidict-raw/&delimiter=/";

/// ZIP files to download for the default (core) dictionary.
const SUDACHI_ZIPS: &[&str] = &["small_lex.zip", "core_lex.zip"];

/// SudachiDict CSV dictionary source.
///
/// File format: 18-column CSV (comma-separated).
/// Columns: surface(0), left_id(1), right_id(2), cost(3), ..., reading(11), ...
/// Reading is in katakana and gets converted to hiragana.
/// Files matched: `*.csv` in the input directory.
pub struct SudachiSource;

impl SudachiSource {
    /// Query S3 bucket listing and return the latest version string (e.g. "20260116").
    fn latest_version() -> Result<String, DictSourceError> {
        let body = ureq::get(SUDACHI_S3_LIST_URL)
            .call()
            .map_err(|e| DictSourceError::Http(format!("S3 listing: {e}")))?
            .into_body()
            .read_to_string()
            .map_err(|e| DictSourceError::Http(format!("S3 listing: {e}")))?;
        parse_latest_version(&body)
    }
}

/// Extract the latest numeric version from S3 ListBucket XML response.
///
/// Looks for `<Prefix>sudachidict-raw/YYYYMMDD/</Prefix>` entries and returns the
/// lexicographically largest pure-numeric version string.
fn parse_latest_version(xml: &str) -> Result<String, DictSourceError> {
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
        .ok_or_else(|| DictSourceError::Http("no versions found in S3 listing".to_string()))
}

impl DictSource for SudachiSource {
    fn parse_dir(&self, dir: &Path) -> Result<HashMap<String, Vec<DictEntry>>, DictSourceError> {
        let mut files: Vec<_> = fs::read_dir(dir)
            .map_err(DictSourceError::Io)?
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                let name = entry.file_name();
                name.to_string_lossy().ends_with(".csv")
            })
            .collect();
        files.sort_by_key(|e| e.file_name());

        if files.is_empty() {
            return Err(DictSourceError::Parse(format!(
                "no *.csv files found in {}",
                dir.display()
            )));
        }

        let mut entries: HashMap<String, Vec<DictEntry>> = HashMap::new();
        let mut total_lines = 0u64;
        let mut skipped = 0u64;

        for file_entry in &files {
            let path = file_entry.path();
            eprintln!("Reading {}...", path.display());
            let content = fs::read_to_string(&path).map_err(DictSourceError::Io)?;

            for line in content.lines() {
                total_lines += 1;
                if line.is_empty() || line.starts_with('#') {
                    skipped += 1;
                    continue;
                }

                let fields: Vec<&str> = line.split(',').collect();
                if fields.len() < 12 {
                    skipped += 1;
                    continue;
                }

                let surface = fields[0];
                let left_id: u16 = match fields[1].parse() {
                    Ok(v) => v,
                    Err(_) => {
                        skipped += 1;
                        continue;
                    }
                };
                let right_id: u16 = match fields[2].parse() {
                    Ok(v) => v,
                    Err(_) => {
                        skipped += 1;
                        continue;
                    }
                };
                let cost: i16 = match fields[3].parse() {
                    Ok(v) => v,
                    Err(_) => {
                        skipped += 1;
                        continue;
                    }
                };
                let reading_kata = fields[11];
                let reading = kata_to_hira(reading_kata);

                if reading.is_empty() {
                    skipped += 1;
                    continue;
                }

                // Only keep entries whose reading is pure hiragana after conversion
                if !is_hiragana(&reading) {
                    skipped += 1;
                    continue;
                }

                entries.entry(reading).or_default().push(DictEntry {
                    surface: surface.to_string(),
                    cost,
                    left_id,
                    right_id,
                });
            }
        }

        eprintln!("  (skipped {skipped} of {total_lines} lines)");
        Ok(entries)
    }

    fn fetch(&self, dest: &Path) -> Result<(), DictSourceError> {
        fs::create_dir_all(dest).map_err(DictSourceError::Io)?;

        let version = Self::latest_version()?;
        eprintln!(
            "Downloading SudachiDict (version {version}) to {}...",
            dest.display()
        );

        for zip_name in SUDACHI_ZIPS {
            let csv_name = zip_name.replace(".zip", ".csv");
            let csv_path = dest.join(&csv_name);
            if csv_path.exists() {
                eprintln!("  {csv_name} (already exists, skipping)");
                continue;
            }

            let url = format!("{SUDACHI_BASE_URL}/{version}/{zip_name}");
            eprintln!("  {zip_name}");

            let body = ureq::get(&url)
                .call()
                .map_err(|e| DictSourceError::Http(format!("{url}: {e}")))?
                .into_body()
                .read_to_vec()
                .map_err(|e| DictSourceError::Http(format!("{url}: {e}")))?;

            let cursor = Cursor::new(body);
            let mut archive = zip::ZipArchive::new(cursor)
                .map_err(|e| DictSourceError::Io(io::Error::new(io::ErrorKind::InvalidData, e)))?;

            for i in 0..archive.len() {
                let mut file = archive.by_index(i).map_err(|e| {
                    DictSourceError::Io(io::Error::new(io::ErrorKind::InvalidData, e))
                })?;
                let raw_name = file.name().to_string();
                if !raw_name.ends_with(".csv") {
                    continue;
                }
                // Sanitize: use only the file basename to prevent zip slip
                let basename = Path::new(&raw_name)
                    .file_name()
                    .ok_or_else(|| {
                        DictSourceError::Parse(format!("invalid ZIP entry name: {raw_name}"))
                    })?
                    .to_string_lossy();
                let out_path = dest.join(&*basename);
                let mut out = fs::File::create(&out_path).map_err(DictSourceError::Io)?;
                io::copy(&mut file, &mut out).map_err(DictSourceError::Io)?;
                eprintln!("    → {basename}");
            }
        }

        // Create stamp file
        fs::write(dest.join(".stamp"), "").map_err(DictSourceError::Io)?;
        eprintln!("Done. Files saved to {}", dest.display());
        Ok(())
    }
}

/// Convert katakana string to hiragana.
/// Maps U+30A1..U+30F6 (ァ-ヶ) to U+3041..U+3096 (ぁ-ゖ).
/// Prolonged sound mark ー (U+30FC) is kept as-is.
/// Non-katakana characters are passed through unchanged.
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_kata_to_hira() {
        assert_eq!(kata_to_hira("カンジ"), "かんじ");
        assert_eq!(kata_to_hira("ニホン"), "にほん");
        assert_eq!(kata_to_hira("ラーメン"), "らーめん");
        assert_eq!(kata_to_hira("ア"), "あ");
        assert_eq!(kata_to_hira(""), "");
    }

    #[test]
    fn test_parse_sudachi_csv() {
        let dir = std::env::temp_dir().join("lexime_test_sudachi");
        fs::create_dir_all(&dir).unwrap();
        let csv_file = dir.join("small_lex.csv");
        // 18-column SudachiDict format (simplified — only columns 0-11 matter)
        fs::write(
            &csv_file,
            "漢字,1847,1847,5100,名詞,普通名詞,一般,*,*,*,*,カンジ,*,*,*,*,*,*\n\
             感じ,1847,1847,5150,名詞,普通名詞,一般,*,*,*,*,カンジ,*,*,*,*,*,*\n\
             日本,1847,1847,4500,名詞,固有名詞,地名,*,*,*,*,ニホン,*,*,*,*,*,*\n\
             test,100,100,3000,名詞,普通名詞,一般,*,*,*,*,test,*,*,*,*,*,*\n",
        )
        .unwrap();

        let source = SudachiSource;
        let entries = source.parse_dir(&dir).unwrap();

        // "test" reading (non-hiragana after pass-through) should be skipped
        assert!(!entries.contains_key("test"));

        // かんじ (converted from カンジ) should have 2 entries
        let kanji = entries.get("かんじ").unwrap();
        assert_eq!(kanji.len(), 2);
        assert_eq!(kanji[0].surface, "漢字");
        assert_eq!(kanji[0].cost, 5100);
        assert_eq!(kanji[1].surface, "感じ");

        // にほん should have 1 entry
        let nihon = entries.get("にほん").unwrap();
        assert_eq!(nihon.len(), 1);
        assert_eq!(nihon[0].surface, "日本");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_parse_sudachi_empty_dir() {
        let dir = std::env::temp_dir().join("lexime_test_sudachi_empty");
        fs::create_dir_all(&dir).unwrap();

        let source = SudachiSource;
        let result = source.parse_dir(&dir);
        assert!(result.is_err());

        fs::remove_dir_all(&dir).ok();
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
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<ListBucketResult>
  <Prefix>sudachidict-raw/</Prefix>
  <CommonPrefixes>
    <Prefix>sudachidict-raw/20210608/</Prefix>
  </CommonPrefixes>
  <CommonPrefixes>
    <Prefix>sudachidict-raw/20260116/</Prefix>
  </CommonPrefixes>
  <CommonPrefixes>
    <Prefix>sudachidict-raw/20201023_2/</Prefix>
  </CommonPrefixes>
  <CommonPrefixes>
    <Prefix>sudachidict-raw/20250129/</Prefix>
  </CommonPrefixes>
</ListBucketResult>"#;
        let version = parse_latest_version(xml).unwrap();
        assert_eq!(version, "20260116");
    }

    #[test]
    fn test_parse_latest_version_empty() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<ListBucketResult></ListBucketResult>"#;
        assert!(parse_latest_version(xml).is_err());
    }
}
