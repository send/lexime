mod mozc;
pub mod pos_map;
mod sudachi;

use std::collections::HashMap;
use std::fmt;
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

#[derive(Debug)]
pub enum DictSourceError {
    Io(io::Error),
    Parse(String),
    Http(String),
}

impl fmt::Display for DictSourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "IO error: {e}"),
            Self::Parse(msg) => write!(f, "parse error: {msg}"),
            Self::Http(msg) => write!(f, "HTTP error: {msg}"),
        }
    }
}

impl std::error::Error for DictSourceError {}

/// Check if a string consists entirely of hiragana (U+3040..U+309F) and prolonged sound mark ー.
pub(super) fn is_hiragana(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| ('\u{3040}'..='\u{309F}').contains(&c) || c == 'ー')
}

/// Create a `DictSource` by name. Returns `None` for unknown source names.
pub fn from_name(name: &str) -> Option<Box<dyn DictSource>> {
    match name {
        "mozc" => Some(Box::new(MozcSource)),
        "sudachi" => Some(Box::new(SudachiSource)),
        _ => None,
    }
}
