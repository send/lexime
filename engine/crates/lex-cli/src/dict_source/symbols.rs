use std::collections::HashMap;
use std::fs;
use std::path::Path;

use super::curated::{self, CostColumn};
use super::{parse_dict_files, DictSource, DictSourceError};
use lex_core::dict::DictEntry;

/// Bundled TSV of Greek letters, math symbols, and other curated symbols.
///
/// Format: `reading\tsurface\tpos` — the curated grammar
/// ([`curated::parse_curated_line`]) with no cost column. `pos` is a full
/// 7-field id.def tag from [`ALLOWED_POS_TAGS`]. Symbol entries live outside
/// the lattice (see [`DictEntry::symbol`]); their cost is the row order,
/// assigned by compile (`add_symbols`).
const SYMBOLS_TSV: &str = include_str!("symbols.tsv");

const SYMBOLS_FILE: &str = "symbols.tsv";

/// Closed set of POS tags the bundled symbols TSV may use, split the way
/// Mozc's own symbol dictionary splits its rows: Greek letters are
/// 記号,アルファベット, everything else 記号,一般. Keeping the set in one
/// reviewable place lets a typo in the TSV fail at parse time instead of
/// silently matching some other tag in id.def. Enumerated by POS, not by
/// word.
const ALLOWED_POS_TAGS: &[&str] = &["記号,一般,*,*,*,*,*", "記号,アルファベット,*,*,*,*,*"];

/// Lexime-bundled symbol source.
///
/// The TSV is embedded in the binary via `include_str!`. `fetch` materializes
/// it to disk so the existing `compile` pipeline (which reads from a directory)
/// can consume it without special-casing.
pub struct SymbolsSource;

impl DictSource for SymbolsSource {
    /// Every entry is a symbol entry (`DictEntry::symbol`), kept in file
    /// order per reading, with sentinel POS ids until
    /// [`resolve_pos_ids`](DictSource::resolve_pos_ids). The cost is a
    /// placeholder (0): `add_symbols` numbers the row order where symbols
    /// enter the final dictionary.
    fn parse_dir(&self, dir: &Path) -> Result<HashMap<String, Vec<DictEntry>>, DictSourceError> {
        let mut entries = parse_dict_files(
            dir,
            SYMBOLS_FILE,
            |name| name == SYMBOLS_FILE,
            '\t',
            |fields| {
                curated::parse_curated_line(fields, CostColumn::Forbidden, ALLOWED_POS_TAGS)
                    .map(Some)
            },
        )?;
        for list in entries.values_mut() {
            for entry in list {
                entry.symbol = true;
            }
        }
        Ok(entries)
    }

    fn resolve_pos_ids(
        &self,
        entries: &mut HashMap<String, Vec<DictEntry>>,
        id_def: Option<&Path>,
    ) -> Result<(), DictSourceError> {
        curated::resolve_pos_ids(entries, id_def, ALLOWED_POS_TAGS, "symbols")
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
    use super::curated::test_support::{pin_shaped_id_def, scratch, write_id_def};
    use super::*;

    const IPPAN: &str = "記号,一般,*,*,*,*,*";
    const ALPHABET: &str = "記号,アルファベット,*,*,*,*,*";

    fn parse_bundled(name: &str) -> (std::path::PathBuf, HashMap<String, Vec<DictEntry>>) {
        let dir = scratch(&format!("symbols_{name}"));
        SymbolsSource.fetch(&dir).unwrap();
        let entries = SymbolsSource.parse_dir(&dir).unwrap();
        (dir, entries)
    }

    fn parse_err(name: &str, body: &str) -> String {
        let dir = scratch(&format!("symbols_{name}"));
        fs::write(dir.join(SYMBOLS_FILE), body).unwrap();
        let err = SymbolsSource.parse_dir(&dir).unwrap_err().to_string();
        fs::remove_dir_all(&dir).ok();
        err
    }

    fn is_greek(surface: &str) -> bool {
        surface
            .chars()
            .all(|c| ('\u{0370}'..='\u{03FF}').contains(&c))
    }

    /// The data rows of the bundled TSV with the section header above each.
    fn bundled_rows() -> Vec<(&'static str, Vec<&'static str>)> {
        let mut section = "";
        let mut rows = Vec::new();
        for line in SYMBOLS_TSV.lines() {
            if let Some(header) = line.strip_prefix("# --- ") {
                section = header;
            } else if !line.is_empty() && !line.starts_with('#') {
                rows.push((section, line.split('\t').collect()));
            }
        }
        rows
    }

    #[test]
    fn bundled_tsv_parses() {
        let (dir, entries) = parse_bundled("bundled");

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

        // parse_dir alone leaves the POS as a sentinel: compile always
        // follows up with resolve_pos_ids.
        for entry in entries.values().flatten() {
            assert!(entry.symbol);
            assert!(entry.left_id > 2700 && entry.right_id > 2700, "{entry:?}");
        }

        fs::remove_dir_all(&dir).ok();
    }

    /// T1: the bundled TSV resolves by tag name against a pin-shaped id.def —
    /// Greek rows to 記号,アルファベット (2643), every other row to 記号,一般
    /// (2644). Fails if either id goes back to being written as a number.
    #[test]
    fn t1_bundled_resolves_by_tag_name() {
        let (dir, mut entries) = parse_bundled("t1");
        let id_def = pin_shaped_id_def(&dir);
        SymbolsSource
            .resolve_pos_ids(&mut entries, Some(&id_def))
            .unwrap();
        let (mut greek, mut other) = (0, 0);
        for entry in entries.values().flatten() {
            let want = if is_greek(&entry.surface) {
                greek += 1;
                2643
            } else {
                other += 1;
                2644
            };
            assert_eq!((entry.left_id, entry.right_id), (want, want), "{entry:?}");
        }
        assert_eq!((greek, other), (37, 118));
        fs::remove_dir_all(&dir).ok();
    }

    /// T2: the resolved id follows id.def, wherever the tag sits.
    #[test]
    fn t2_follows_renumbered_id() {
        let (dir, mut entries) = parse_bundled("t2");
        let id_def = write_id_def(&dir, 10, &[(7, IPPAN), (8, ALPHABET)]);
        SymbolsSource
            .resolve_pos_ids(&mut entries, Some(&id_def))
            .unwrap();
        for entry in entries.values().flatten() {
            let want = if is_greek(&entry.surface) { 8 } else { 7 };
            assert_eq!((entry.left_id, entry.right_id), (want, want), "{entry:?}");
        }
        fs::remove_dir_all(&dir).ok();
    }

    /// T3: a tag outside the allowed set is an error naming line and tag.
    #[test]
    fn t3_disallowed_tag_is_err() {
        let err = parse_err(
            "t3",
            "あるふぁ\tα\t記号,アルファベット,*,*,*,*,*\nびょう\t″\t名詞,接尾,助数詞,*,*,*,*\n",
        );
        assert!(err.contains("symbols.tsv:2:"), "{err}");
        assert!(err.contains("名詞,接尾,助数詞,*,*,*,*"), "{err}");
        assert!(err.contains("not in the allowed set"), "{err}");
    }

    /// T4: an allowed tag missing from id.def is an error naming the tag; a
    /// vocab-specific near miss does not satisfy it.
    #[test]
    fn t4_tag_missing_from_id_def_is_err() {
        let (dir, mut entries) = parse_bundled("t4");
        let id_def = write_id_def(
            &dir,
            10,
            &[(7, IPPAN), (8, "記号,アルファベット,*,*,*,*,あ")],
        );
        let err = SymbolsSource
            .resolve_pos_ids(&mut entries, Some(&id_def))
            .unwrap_err()
            .to_string();
        assert!(err.contains(ALPHABET), "{err}");
        fs::remove_dir_all(&dir).ok();
    }

    /// T5: no id.def is an error, not a silent no-op.
    #[test]
    fn t5_no_id_def_is_err() {
        let (dir, mut entries) = parse_bundled("t5");
        let err = SymbolsSource
            .resolve_pos_ids(&mut entries, None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("symbols requires id.def"), "{err}");
        fs::remove_dir_all(&dir).ok();
    }

    /// T6: rows that break the grammar are errors with their line number.
    #[test]
    fn t6_malformed_rows_are_err() {
        let cases = [
            (
                "cost_column",
                "ほし\t★\t記号,一般,*,*,*,*,*\t5000",
                "3 tab-separated",
            ),
            (
                "old_5_columns",
                "ほし\t2643\t2643\t5000\t★",
                "3 tab-separated",
            ),
            ("two_columns", "ほし\t★", "3 tab-separated"),
            (
                "non_hiragana",
                "star\t★\t記号,一般,*,*,*,*,*",
                "not hiragana",
            ),
            ("empty_reading", "\t★\t記号,一般,*,*,*,*,*", "not hiragana"),
            (
                "empty_surface",
                "ほし\t\t記号,一般,*,*,*,*,*",
                "empty surface",
            ),
            (
                "padded_surface",
                "ほし\t★ \t記号,一般,*,*,*,*,*",
                "whitespace",
            ),
        ];
        for (name, row, want) in cases {
            let err = parse_err(
                &format!("t6_{name}"),
                &format!("# h\n\nほし\t☆\t記号,一般,*,*,*,*,*\n{row}\n"),
            );
            assert!(err.contains("symbols.tsv:4:"), "{name}: {err}");
            assert!(err.contains(want), "{name}: {err}");
        }
    }

    /// T7: every non-comment, non-blank line becomes an entry.
    #[test]
    fn t7_no_row_is_dropped() {
        let (dir, entries) = parse_bundled("t7");
        let parsed = entries.values().map(Vec::len).sum::<usize>();
        assert_eq!(parsed, bundled_rows().len());
        assert_eq!(parsed, 155);
        fs::remove_dir_all(&dir).ok();
    }

    /// T8: the tag of each row by section — the two Greek sections are
    /// 記号,アルファベット (37 rows), every other section 記号,一般 (118).
    #[test]
    fn t8_bundled_tags_by_section() {
        let (mut greek, mut other) = (0, 0);
        for (section, fields) in bundled_rows() {
            let want = if section.starts_with("Greek letters") {
                greek += 1;
                ALPHABET
            } else {
                other += 1;
                IPPAN
            };
            assert_eq!(fields[2], want, "{section}: {fields:?}");
        }
        assert_eq!((greek, other), (37, 118));
    }

    /// T10: after resolution every id is a row of id.def — no sentinel left.
    #[test]
    fn t10_resolved_ids_within_id_def() {
        let (dir, mut entries) = parse_bundled("t10");
        let id_def = pin_shaped_id_def(&dir);
        let rows = fs::read_to_string(&id_def).unwrap().lines().count();
        SymbolsSource
            .resolve_pos_ids(&mut entries, Some(&id_def))
            .unwrap();
        for entry in entries.values().flatten() {
            assert!((entry.left_id as usize) < rows && (entry.right_id as usize) < rows);
        }
        fs::remove_dir_all(&dir).ok();
    }

    /// A sentinel that survives resolution is an error (structural assert):
    /// an id.def that maps the tag to a sentinel value must not pass.
    #[test]
    fn leftover_sentinel_is_err() {
        let (dir, mut entries) = parse_bundled("leftover");
        let id_def = dir.join("id_sentinel.def");
        fs::write(&id_def, format!("7 {IPPAN}\n{} {ALPHABET}\n", u16::MAX - 1)).unwrap();
        let err = SymbolsSource
            .resolve_pos_ids(&mut entries, Some(&id_def))
            .unwrap_err()
            .to_string();
        assert!(err.contains("unresolved POS id"), "{err}");
        fs::remove_dir_all(&dir).ok();
    }
}
