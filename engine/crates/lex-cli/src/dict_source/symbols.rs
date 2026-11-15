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
/// Symbol entries live outside the lattice (see [`DictEntry::symbol`]); the
/// cost column is not used — see [`SymbolsSource::parse_dir`] (`add_symbols`
/// assigns the row-order cost).
const SYMBOLS_TSV: &str = include_str!("symbols.tsv");

const SYMBOLS_FILE: &str = "symbols.tsv";

/// Lexime-bundled math/Greek symbol source.
///
/// The TSV is embedded in the binary via `include_str!`. `fetch` materializes
/// it to disk so the existing `compile` pipeline (which reads from a directory)
/// can consume it without special-casing.
pub struct SymbolsSource;

impl DictSource for SymbolsSource {
    /// Every entry is a symbol entry (`DictEntry::symbol`), kept in file
    /// order per reading. The cost is a placeholder (0): `add_symbols`
    /// numbers the row order where symbols enter the final dictionary. The
    /// TSV cost column must still parse (the format is the Mozc 5-column
    /// one) but its value is not used.
    fn parse_dir(&self, dir: &Path) -> Result<HashMap<String, Vec<DictEntry>>, DictSourceError> {
        let mut entries = parse_dict_files(
            dir,
            SYMBOLS_FILE,
            |name| name == SYMBOLS_FILE,
            '\t',
            |fields| {
                if fields.len() < 5 {
                    return Err(format!(
                        "expected 5 tab-separated fields, found {}",
                        fields.len()
                    ));
                }
                let reading = fields[0];
                let Some((left_id, right_id, _unused_cost)) = parse_id_cost(fields) else {
                    return Err(format!(
                        "unparsable id/cost columns for reading {reading:?}"
                    ));
                };
                let surface = fields[4];
                if surface.is_empty() {
                    return Err(format!("empty surface for reading {reading:?}"));
                }
                if !is_hiragana(reading) {
                    return Err(format!("reading {reading:?} is not hiragana"));
                }
                Ok(Some(ParsedLine {
                    reading: reading.to_string(),
                    surface: surface.to_string(),
                    left_id,
                    right_id,
                    cost: 0,
                }))
            },
        )?;
        for list in entries.values_mut() {
            for entry in list {
                entry.symbol = true;
            }
        }
        Ok(entries)
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

    #[test]
    fn empty_surface_is_err_with_line() {
        let dir =
            std::env::temp_dir().join(format!("lexime_test_symbols_empty_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join(SYMBOLS_FILE),
            "# header\nほし\t2643\t2643\t5000\t★\nまる\t2643\t2643\t5000\t\n",
        )
        .unwrap();
        let err = SymbolsSource.parse_dir(&dir).unwrap_err().to_string();
        fs::remove_dir_all(&dir).ok();
        assert!(err.contains("symbols.tsv:3"), "{err}");
        assert!(err.contains("empty surface"), "{err}");
    }

    fn parse_err(name: &str, body: &str) -> String {
        let dir =
            std::env::temp_dir().join(format!("lexime_test_symbols_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(SYMBOLS_FILE), body).unwrap();
        let err = SymbolsSource.parse_dir(&dir).unwrap_err().to_string();
        fs::remove_dir_all(&dir).ok();
        err
    }

    #[test]
    fn short_row_is_err_with_line() {
        let err = parse_err(
            "short",
            "# h

ほし\t2643\t2643\t5000\t★\nまる\t2643\t2643\n",
        );
        assert!(err.contains("symbols.tsv:4"), "{err}");
        assert!(err.contains("5 tab-separated fields"), "{err}");
    }

    #[test]
    fn non_hiragana_reading_is_err_with_line() {
        let err = parse_err(
            "kana",
            "ほし\t2643\t2643\t5000\t★\nstar\t2643\t2643\t5000\t☆\n",
        );
        assert!(err.contains("symbols.tsv:2"), "{err}");
        assert!(err.contains("not hiragana"), "{err}");
    }

    #[test]
    fn unparsable_id_or_cost_is_err_with_line() {
        for (name, row) in [
            ("lid", "まる\tx\t2643\t5000\t○\n"),
            ("cost", "まる\t2643\t2643\tx\t○\n"),
        ] {
            let err = parse_err(name, &format!("ほし\t2643\t2643\t5000\t★\n{row}"));
            assert!(err.contains("symbols.tsv:2"), "{err}");
            assert!(err.contains("unparsable id/cost"), "{err}");
        }
    }
}
