//! The one line grammar and POS resolution shared by the curated sources
//! (`symbols`, `extras`).
//!
//! A curated row is `reading TAB surface TAB pos [TAB cost]`. `pos` is a full
//! 7-field id.def tag, which must be in the source's closed set of allowed
//! tags; the source declares whether the cost column is required or
//! forbidden ([`CostColumn`]). There are no defaults: a row that leaves out
//! a column the source needs is an error, not a silent default.
//!
//! Parsing does not need id.def: a row carries a sentinel id naming the
//! index of its tag in the allowed set, and [`resolve_pos_ids`] replaces the
//! sentinels with the ids of the pinned id.def at compile time (#279).

use std::collections::HashMap;
use std::path::Path;

use super::{is_hiragana, pos_map, DictSourceError, ParsedLine};
use lex_core::dict::DictEntry;

/// Whether a curated source's rows carry a cost column.
#[derive(Clone, Copy, Debug)]
pub(super) enum CostColumn {
    /// The 4th column is the entry's final cost (extras: lattice words).
    Required,
    /// No 4th column (symbols: the cost is the row order, assigned by
    /// compile). The entry's cost is 0 until then.
    Forbidden,
}

/// Base of the sentinel POS ids: the row whose tag is `allowed[i]` carries
/// `POS_UNRESOLVED_BASE - i` until [`resolve_pos_ids`] replaces it.
/// Deliberately out of id.def's range (~2700 ids), so an unresolved entry
/// that somehow reached a compiled dict would fail loudly in conversion.
///
/// Precondition: a sentinel carries only the index into its own source's
/// allowed-tag array; which source it came from is not recoverable from the
/// id. So each source's map must be resolved by that source's own
/// `resolve_pos_ids` before it is mixed with another's (`build_entries`
/// does). If a change ever mixes unresolved maps, give each source its own
/// sentinel range first (recorded as the trigger).
const POS_UNRESOLVED_BASE: u16 = u16::MAX;

/// Sentinel id for `allowed[index]`.
fn sentinel(index: usize) -> u16 {
    POS_UNRESOLVED_BASE - index as u16
}

/// `Some(index)` when `id` is the sentinel of one of `allowed`'s tags.
fn sentinel_index(id: u16, allowed: &[&str]) -> Option<usize> {
    let index = (POS_UNRESOLVED_BASE - id) as usize;
    (index < allowed.len()).then_some(index)
}

/// Parse one curated row (the fields of a non-comment, non-blank line).
/// Every malformed row is an `Err` (the caller adds the file and line).
pub(super) fn parse_curated_line(
    fields: &[&str],
    cost_column: CostColumn,
    allowed: &[&str],
) -> Result<ParsedLine, String> {
    let (expected, shape) = match cost_column {
        CostColumn::Required => (4, "reading, surface, pos, cost"),
        CostColumn::Forbidden => (3, "reading, surface, pos — no cost column"),
    };
    if fields.len() != expected {
        return Err(format!(
            "expected {expected} tab-separated columns ({shape}), found {}",
            fields.len()
        ));
    }
    let (reading, surface, tag) = (fields[0], fields[1], fields[2]);
    if !is_hiragana(reading) {
        return Err(format!("reading {reading:?} is not hiragana"));
    }
    if surface.is_empty() {
        return Err(format!("empty surface for reading {reading:?}"));
    }
    if surface.trim() != surface {
        return Err(format!(
            "surface {surface:?} has leading or trailing whitespace"
        ));
    }
    let index = allowed
        .iter()
        .position(|t| *t == tag)
        .ok_or_else(|| format!("POS tag {tag:?} is not in the allowed set {allowed:?}"))?;
    let cost = match cost_column {
        CostColumn::Forbidden => 0,
        CostColumn::Required => {
            let raw = fields[3];
            let cost: i16 = raw
                .parse()
                .map_err(|_| format!("cost {raw:?} is not an i16"))?;
            if cost < 0 {
                return Err(format!("cost {cost} is negative"));
            }
            cost
        }
    };
    Ok(ParsedLine {
        reading: reading.to_string(),
        surface: surface.to_string(),
        left_id: sentinel(index),
        right_id: sentinel(index),
        cost,
    })
}

/// Replace the sentinel ids written by [`parse_curated_line`] with the ids
/// of `allowed`'s tags in `id_def`, printing the resolution. Fails without
/// an id.def, when an allowed tag is missing from it, and when a tag resolves
/// into the sentinel range (so a sentinel cannot survive resolution).
pub(super) fn resolve_pos_ids(
    entries: &mut HashMap<String, Vec<DictEntry>>,
    id_def: Option<&Path>,
    allowed: &[&str],
    source_label: &str,
) -> Result<(), DictSourceError> {
    let id_def = id_def.ok_or_else(|| {
        DictSourceError::Parse(format!(
            "{source_label} requires id.def to resolve its POS ids — pass --id-def or place id.def in the primary input dir"
        ))
    })?;
    let map = pos_map::parse_mozc_id_def(id_def)?;
    let mut resolved = Vec::with_capacity(allowed.len());
    for tag in allowed {
        let id = pos_map::resolve_tag(&map, tag, source_label, id_def)?;
        eprintln!("  {source_label} POS id: {id} ({tag})");
        // A resolved id inside the sentinel range would be replaced again
        // (or mistaken for one) — the structural check is on the table, once.
        if sentinel_index(id, allowed).is_some() {
            return Err(DictSourceError::Parse(format!(
                "{source_label} tag {tag:?} resolves to {id}, inside the unresolved POS id sentinel range"
            )));
        }
        resolved.push(id);
    }
    for entry_list in entries.values_mut() {
        for entry in entry_list.iter_mut() {
            for id in [&mut entry.left_id, &mut entry.right_id] {
                if let Some(index) = sentinel_index(*id, allowed) {
                    *id = resolved[index];
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::collections::HashMap;
    use std::fs;
    use std::path::{Path, PathBuf};

    use crate::dict_source::DictSource;
    use lex_core::dict::DictEntry;

    /// The id of `tag` in [`PIN_SHAPED_IDS`].
    pub fn pin_id(tag: &str) -> u16 {
        PIN_SHAPED_IDS
            .iter()
            .find(|(_, t)| *t == tag)
            .map(|&(id, _)| id as u16)
            .unwrap()
    }

    /// The data rows (comments and blank lines dropped) of a TSV, split on tabs.
    pub fn data_rows(content: &str) -> impl Iterator<Item = Vec<&str>> {
        content
            .lines()
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(|l| l.split('\t').collect())
    }

    /// `fetch` the source's bundled data into a scratch dir and parse it.
    pub fn parse_bundled(
        source: &dyn DictSource,
        name: &str,
    ) -> (PathBuf, HashMap<String, Vec<DictEntry>>) {
        let dir = scratch(name);
        source.fetch(&dir).unwrap();
        let entries = source.parse_dir(&dir).unwrap();
        (dir, entries)
    }

    /// Parse `body` as the source's file `file`; the error text.
    pub fn parse_err(source: &dyn DictSource, file: &str, name: &str, body: &str) -> String {
        let dir = scratch(name);
        fs::write(dir.join(file), body).unwrap();
        let err = source.parse_dir(&dir).unwrap_err().to_string();
        fs::remove_dir_all(&dir).ok();
        err
    }

    /// Fresh scratch dir per test (tests run in parallel).
    pub fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("lexime_test_curated_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Synthetic id.def with `n_rows` rows (ids 0..n_rows); `overrides`
    /// place a tag at the given id, every other row is a vocab-specific
    /// filler that no generic tag matches.
    pub fn write_id_def(dir: &Path, n_rows: usize, overrides: &[(usize, &str)]) -> PathBuf {
        let lines: Vec<String> = (0..n_rows)
            .map(|id| {
                let tag = overrides
                    .iter()
                    .find(|(i, _)| *i == id)
                    .map(|(_, t)| t.to_string())
                    .unwrap_or_else(|| format!("名詞,一般,*,*,*,*,filler{id}"));
                format!("{id} {tag}")
            })
            .collect();
        let path = dir.join("id.def");
        fs::write(&path, lines.join("\n")).unwrap();
        path
    }

    /// The ids of the pinned id.def that T1 checks: 2643 記号,アルファベット,
    /// 2644 記号,一般, 1851 名詞,一般, the six 名詞,固有名詞 subdivisions
    /// extras allows, and 2011 助数詞.
    pub const PIN_SHAPED_IDS: &[(usize, &str)] = &[
        (1851, "名詞,一般,*,*,*,*,*"),
        (1920, "名詞,固有名詞,一般,*,*,*,*"),
        (1921, "名詞,固有名詞,人名,一般,*,*,*"),
        (1922, "名詞,固有名詞,人名,名,*,*,*"),
        (1924, "名詞,固有名詞,地域,一般,*,*,*"),
        (1928, "名詞,固有名詞,地域,国,*,*,*"),
        (1929, "名詞,固有名詞,組織,*,*,*,*"),
        (2011, "名詞,接尾,助数詞,*,*,*,*"),
        (2643, "記号,アルファベット,*,*,*,*,*"),
        (2644, "記号,一般,*,*,*,*,*"),
    ];

    /// Pin-shaped synthetic id.def ([`PIN_SHAPED_IDS`], 2672 rows like the pin).
    pub fn pin_shaped_id_def(dir: &Path) -> PathBuf {
        write_id_def(dir, 2672, PIN_SHAPED_IDS)
    }
}
