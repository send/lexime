use std::collections::HashMap;
use std::fs;
use std::path::Path;

use super::{
    is_hiragana, parse_dict_files, parse_id_cost, DictSource, DictSourceError, ParsedLine,
};
use lex_core::dict::DictEntry;

/// Bundled TSV of Greek letters and common math symbols.
///
/// Format matches Mozc TSV: `reading\tleft_id\tright_id\tcost\tsurface`.
const SYMBOLS_TSV: &str = include_str!("symbols.tsv");

const SYMBOLS_FILE: &str = "symbols.tsv";

/// Lexime-bundled math/Greek symbol source.
///
/// The TSV is embedded in the binary via `include_str!`. `fetch` materializes
/// it to disk so the existing `compile` pipeline (which reads from a directory)
/// can consume it without special-casing.
pub struct SymbolsSource;

impl DictSource for SymbolsSource {
    fn parse_dir(&self, dir: &Path) -> Result<HashMap<String, Vec<DictEntry>>, DictSourceError> {
        parse_dict_files(
            dir,
            SYMBOLS_FILE,
            |name| name == SYMBOLS_FILE,
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

    fn fetch(&self, dest: &Path) -> Result<(), DictSourceError> {
        fs::create_dir_all(dest).map_err(DictSourceError::Io)?;
        fs::write(dest.join(SYMBOLS_FILE), SYMBOLS_TSV).map_err(DictSourceError::Io)?;
        fs::write(dest.join(".stamp"), "").map_err(DictSourceError::Io)?;
        eprintln!(
            "Wrote bundled symbols TSV to {}",
            dest.join(SYMBOLS_FILE).display()
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_tsv_parses() {
        let dir = std::env::temp_dir().join("lexime_test_symbols");
        let _ = fs::remove_dir_all(&dir);

        let source = SymbolsSource;
        source.fetch(&dir).unwrap();
        let entries = source.parse_dir(&dir).unwrap();

        let shiguma = entries.get("しぐま").expect("しぐま must map to σ/Σ");
        let surfaces: Vec<&str> = shiguma.iter().map(|e| e.surface.as_str()).collect();
        assert!(surfaces.contains(&"σ"));
        assert!(surfaces.contains(&"Σ"));

        let sekibun = entries.get("せきぶん").expect("せきぶん must map to ∫");
        assert!(sekibun.iter().any(|e| e.surface == "∫"));

        let mugendai = entries.get("むげんだい").expect("むげんだい must map to ∞");
        assert!(mugendai.iter().any(|e| e.surface == "∞"));

        // General-purpose symbols
        let hoshi = entries.get("ほし").expect("ほし must map to ★/☆");
        assert!(hoshi.iter().any(|e| e.surface == "★"));
        assert!(hoshi.iter().any(|e| e.surface == "☆"));

        let onpu = entries.get("おんぷ").expect("おんぷ must map to ♪");
        assert!(onpu.iter().any(|e| e.surface == "♪"));

        for entries in entries.values() {
            for entry in entries {
                assert_eq!(entry.left_id, 2643, "symbols use 記号,一般 POS id 2643");
                assert_eq!(entry.right_id, 2643);
            }
        }

        fs::remove_dir_all(&dir).ok();
    }
}
