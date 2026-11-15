mod curated;
#[cfg(test)]
pub(crate) use curated::test_support;
mod extras;
#[cfg(test)]
pub(crate) use extras::bundled_rows as bundled_extras_rows;
mod mozc;
pub mod pos_map;
mod symbols;

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;

use lex_core::dict::DictEntry;

/// Constructor for a [`DictSource`] stored in the [`SOURCES`] registry.
type SourceCtor = fn() -> Box<dyn DictSource>;

/// Registry of dictionary sources. Single source of truth for both
/// [`from_name`] and [`available_sources`] — add new sources here.
const SOURCES: &[(&str, SourceCtor)] = &[
    ("mozc", || Box::new(mozc::MozcSource)),
    ("symbols", || Box::new(symbols::SymbolsSource)),
    ("extras", || Box::new(extras::ExtrasSource)),
];

/// Comma-separated list of source names recognized by [`from_name`]. Shown in
/// error messages so users know what to pick.
pub fn available_sources() -> String {
    SOURCES
        .iter()
        .map(|(name, _)| *name)
        .collect::<Vec<_>>()
        .join(", ")
}

/// A pluggable dictionary source that parses raw dictionary files into entries.
pub trait DictSource {
    /// Parse all dictionary files in `dir` and return a map of reading → entries.
    fn parse_dir(&self, dir: &Path) -> Result<HashMap<String, Vec<DictEntry>>, DictSourceError>;

    /// Download raw dictionary files into `dest`.
    fn fetch(&self, dest: &Path) -> Result<(), DictSourceError>;

    /// Download raw dictionary files into `dest`, pinned to `sha` (a full
    /// 40-hex commit SHA) instead of whatever upstream currently serves.
    /// Only sources backed by a git remote can honor a commit pin, so the
    /// default implementation rejects it — `dictool fetch --sha` on a
    /// source that would silently ignore the pin must fail loudly.
    fn fetch_pinned(&self, _dest: &Path, _sha: &str) -> Result<(), DictSourceError> {
        Err(DictSourceError::Parse(
            "this source does not support --sha (only 'mozc' does)".to_string(),
        ))
    }

    /// Resolve POS ids that depend on the Mozc `id.def` in use. `compile`
    /// calls this on every source's parsed entries, passing the same id.def
    /// it uses for cost adjustments (`None` when compiling without one).
    ///
    /// Default: no-op — most sources carry explicit POS ids in their raw
    /// files. Sources that write a POS by tag name (the curated sources,
    /// symbols and extras) override this to resolve the tag against id.def,
    /// so a pin bump that renumbers ids can't silently degrade their entries
    /// (#279).
    fn resolve_pos_ids(
        &self,
        _entries: &mut HashMap<String, Vec<DictEntry>>,
        _id_def: Option<&Path>,
    ) -> Result<(), DictSourceError> {
        Ok(())
    }

    /// Whether `compile` applies its morpheme-role cost offsets (person name /
    /// pronoun / non-independent kanji) to this source's entries, whether it
    /// is the primary source or an extra one. Those offsets are part of the
    /// Mozc import; a curated source's cost is final, so the default is
    /// `false` and only Mozc declares `true`.
    fn applies_role_cost_offsets(&self) -> bool {
        false
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DictSourceError {
    #[error("IO error: {0}")]
    Io(#[from] io::Error),

    #[error("parse error: {0}")]
    Parse(String),

    #[error("HTTP error: {0}")]
    Http(String),
}

pub(crate) use lex_core::unicode::is_hiragana_reading as is_hiragana;

/// List files in `dir` whose names satisfy `predicate`, sorted by name.
///
/// Returns an error if no matching files are found, using `label` in the
/// message (e.g. `"dictionary*.txt"` or `"*.csv"`).
pub(super) fn list_dict_files(
    dir: &Path,
    label: &str,
    predicate: impl Fn(&str) -> bool,
) -> Result<Vec<fs::DirEntry>, DictSourceError> {
    let mut files: Vec<fs::DirEntry> = fs::read_dir(dir)
        .map_err(DictSourceError::Io)?
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            let name = entry.file_name();
            predicate(&name.to_string_lossy())
        })
        .collect();
    files.sort_by_key(|e| e.file_name());

    if files.is_empty() {
        return Err(DictSourceError::Parse(format!(
            "no {label} files found in {}",
            dir.display()
        )));
    }

    Ok(files)
}

/// Parse fields `[1]`, `[2]`, `[3]` as `(left_id: u16, right_id: u16, cost: i16)`.
///
/// Returns `None` if any field fails to parse — callers should skip the line.
pub(super) fn parse_id_cost(fields: &[&str]) -> Option<(u16, u16, i16)> {
    let left_id: u16 = fields.get(1)?.parse().ok()?;
    let right_id: u16 = fields.get(2)?.parse().ok()?;
    let cost: i16 = fields.get(3)?.parse().ok()?;
    Some((left_id, right_id, cost))
}

/// A single parsed dictionary line, returned by the per-line parser closure
/// passed to [`parse_dict_files`].
pub(super) struct ParsedLine {
    pub reading: String,
    pub surface: String,
    pub left_id: u16,
    pub right_id: u16,
    pub cost: i16,
}

/// Parse dictionary files with shared boilerplate: file listing, line iteration,
/// empty/comment skipping, and stats logging.
///
/// `parse_line` receives the split fields for each non-empty, non-comment line.
/// Return `Ok(Some(ParsedLine))` to add the entry, `Ok(None)` to skip the line
/// (counted in the skip stats), or `Err(msg)` to abort the whole parse with a
/// [`DictSourceError::Parse`] naming the file and 1-based line number.
pub(super) fn parse_dict_files(
    dir: &Path,
    label: &str,
    predicate: impl Fn(&str) -> bool,
    delimiter: char,
    parse_line: impl Fn(&[&str]) -> Result<Option<ParsedLine>, String>,
) -> Result<HashMap<String, Vec<DictEntry>>, DictSourceError> {
    let (entries, stats) = parse_dict_files_counted(dir, label, predicate, delimiter, parse_line)?;
    eprintln!(
        "  (skipped {} of {} lines)",
        stats.skipped, stats.total_lines
    );
    Ok(entries)
}

/// Line counts of one [`parse_dict_files`] run: every line read, and the
/// skipped ones (blank, comment, or `Ok(None)` from the line parser).
#[derive(Debug, PartialEq, Eq)]
struct ParseStats {
    total_lines: u64,
    skipped: u64,
}

fn parse_dict_files_counted(
    dir: &Path,
    label: &str,
    predicate: impl Fn(&str) -> bool,
    delimiter: char,
    parse_line: impl Fn(&[&str]) -> Result<Option<ParsedLine>, String>,
) -> Result<(HashMap<String, Vec<DictEntry>>, ParseStats), DictSourceError> {
    let files = list_dict_files(dir, label, predicate)?;
    let mut entries: HashMap<String, Vec<DictEntry>> = HashMap::new();
    let mut total_lines = 0u64;
    let mut skipped = 0u64;

    for file_entry in &files {
        let path = file_entry.path();
        eprintln!("Reading {}...", path.display());
        let content = fs::read_to_string(&path).map_err(DictSourceError::Io)?;

        for (idx, line) in content.lines().enumerate() {
            total_lines += 1;
            if line.is_empty() || line.starts_with('#') {
                skipped += 1;
                continue;
            }

            let fields: Vec<&str> = line.split(delimiter).collect();
            let parsed = parse_line(&fields).map_err(|msg| {
                DictSourceError::Parse(format!("{}:{}: {msg}", path.display(), idx + 1))
            })?;
            let Some(parsed) = parsed else {
                skipped += 1;
                continue;
            };

            entries.entry(parsed.reading).or_default().push(DictEntry {
                surface: parsed.surface,
                cost: parsed.cost,
                left_id: parsed.left_id,
                right_id: parsed.right_id,
                symbol: false,
            });
        }
    }

    Ok((
        entries,
        ParseStats {
            total_lines,
            skipped,
        },
    ))
}

/// Create a `DictSource` by name. Returns `None` for unknown source names.
pub fn from_name(name: &str) -> Option<Box<dyn DictSource>> {
    SOURCES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, ctor)| ctor())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A source that doesn't override `fetch_pinned` must reject `--sha`
    /// instead of silently fetching an unpinned snapshot — the caller asked
    /// for determinism it cannot provide. `symbols` and `extras` are bundled
    /// TSVs with no upstream commit to pin.
    #[test]
    fn test_fetch_pinned_rejected_by_default() {
        for name in ["symbols", "extras"] {
            let source = from_name(name).unwrap();
            let err = source
                .fetch_pinned(
                    Path::new("/nonexistent"),
                    "0123456789abcdef0123456789abcdef01234567",
                )
                .unwrap_err();
            assert!(
                err.to_string().contains("--sha"),
                "{name}: unexpected error {err}"
            );
        }
    }

    /// T9: a line the parser declines (`Ok(None)` — the mozc skip) is
    /// counted in the skip stats with blank and comment lines, adds no
    /// entry, and does not stop the rows after it.
    #[test]
    fn t9_declined_line_is_counted_as_skipped() {
        let dir = test_support::scratch("parse_stats");
        fs::write(dir.join("d.txt"), "# comment\n\nあ\tA\nskip\tme\nい\tI\n").unwrap();
        let (entries, stats) = parse_dict_files_counted(
            &dir,
            "test",
            |_| true,
            '\t',
            |fields| {
                if fields[0] == "skip" {
                    return Ok(None);
                }
                Ok(Some(ParsedLine {
                    reading: fields[0].to_string(),
                    surface: fields[1].to_string(),
                    left_id: 1,
                    right_id: 1,
                    cost: 0,
                }))
            },
        )
        .unwrap();
        assert_eq!(
            stats,
            ParseStats {
                total_lines: 5,
                skipped: 3
            }
        );
        let mut readings: Vec<&String> = entries.keys().collect();
        readings.sort();
        assert_eq!(readings, ["あ", "い"]);
        fs::remove_dir_all(&dir).ok();
    }
}
