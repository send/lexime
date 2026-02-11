mod mozc;
pub mod pos_map;
mod sudachi;

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;

use super::DictEntry;

pub use mozc::MozcSource;
pub use sudachi::SudachiSource;

/// A pluggable dictionary source that parses raw dictionary files into entries.
pub trait DictSource {
    /// Parse all dictionary files in `dir` and return a map of reading → entries.
    fn parse_dir(&self, dir: &Path) -> Result<HashMap<String, Vec<DictEntry>>, DictSourceError>;

    /// Download raw dictionary files into `dest`.
    fn fetch(&self, dest: &Path) -> Result<(), DictSourceError>;
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

pub(super) use crate::unicode::is_hiragana_reading as is_hiragana;

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
    let left_id: u16 = fields[1].parse().ok()?;
    let right_id: u16 = fields[2].parse().ok()?;
    let cost: i16 = fields[3].parse().ok()?;
    Some((left_id, right_id, cost))
}

/// Create a `DictSource` by name. Returns `None` for unknown source names.
pub fn from_name(name: &str) -> Option<Box<dyn DictSource>> {
    match name {
        "mozc" => Some(Box::new(MozcSource)),
        "sudachi" => Some(Box::new(SudachiSource)),
        _ => None,
    }
}
