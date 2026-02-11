use std::collections::HashMap;
use std::fs;
use std::path::Path;

use super::{is_hiragana, DictSource, DictSourceError};
use crate::dict::DictEntry;

const MOZC_CONTENTS_URL: &str =
    "https://api.github.com/repos/google/mozc/contents/src/data/dictionary_oss";
const MOZC_LICENSE_URL: &str = "https://raw.githubusercontent.com/google/mozc/master/LICENSE";
const MOZC_CONNECTION_URL: &str = "https://raw.githubusercontent.com/google/mozc/master/src/data/dictionary_oss/connection_single_column.txt";

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

    /// List dictionary files via GitHub Contents API and return (name, download_url) pairs
    /// for `dictionary*.txt` and `id.def`.
    fn list_remote_files() -> Result<Vec<(String, String)>, DictSourceError> {
        let body = ureq::get(MOZC_CONTENTS_URL)
            .call()
            .map_err(|e| DictSourceError::Http(format!("GitHub API: {e}")))?
            .into_body()
            .read_to_string()
            .map_err(|e| DictSourceError::Http(format!("GitHub API: {e}")))?;
        parse_remote_files(&body)
    }
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
        let mut files: Vec<_> = fs::read_dir(dir)
            .map_err(DictSourceError::Io)?
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                name.starts_with("dictionary") && name.ends_with(".txt")
            })
            .collect();
        files.sort_by_key(|e| e.file_name());

        if files.is_empty() {
            return Err(DictSourceError::Parse(format!(
                "no dictionary*.txt files found in {}",
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

                let fields: Vec<&str> = line.split('\t').collect();
                if fields.len() < 5 {
                    skipped += 1;
                    continue;
                }

                let reading = fields[0];
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
                let surface = fields[4];

                if !is_hiragana(reading) {
                    skipped += 1;
                    continue;
                }

                entries
                    .entry(reading.to_string())
                    .or_default()
                    .push(DictEntry {
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

        eprintln!("Downloading Mozc dictionary files to {}...", dest.display());

        let remote_files = Self::list_remote_files()?;
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
            Self::download_file(MOZC_CONNECTION_URL, &connection)?;
        }

        // Download LICENSE
        let license = dest.join("LICENSE");
        if license.exists() {
            eprintln!("  LICENSE (already exists, skipping)");
        } else {
            eprintln!("  LICENSE");
            Self::download_file(MOZC_LICENSE_URL, &license)?;
        }

        // Create stamp file
        fs::write(dest.join(".stamp"), "").map_err(DictSourceError::Io)?;
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
}
