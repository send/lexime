pub mod connection;
mod entry;
pub mod source;
mod trie_dict;

pub use entry::DictEntry;
pub use trie_dict::TrieDictionary;

use std::io;

/// Unified error type for dictionary and connection-matrix binary I/O.
///
/// Covers loading/saving both `TrieDictionary` (LXDX) and
/// `ConnectionMatrix` (LXCX) files. Previously these were separate
/// `DictError` and `ConnectionError` enums with overlapping variants.
#[derive(Debug, thiserror::Error)]
pub enum DictError {
    #[error("IO error: {0}")]
    Io(#[from] io::Error),

    #[error("invalid header (too short)")]
    InvalidHeader,

    #[error("invalid magic bytes (expected LXDX or LXCX)")]
    InvalidMagic,

    #[error("unsupported version: {0}")]
    UnsupportedVersion(u8),

    #[error("serialization error: {0}")]
    Serialize(bincode::Error),

    #[error("deserialization error: {0}")]
    Deserialize(bincode::Error),

    #[error("parse error: {0}")]
    Parse(String),
}

impl From<lexime_trie::TrieError> for DictError {
    fn from(e: lexime_trie::TrieError) -> Self {
        match e {
            lexime_trie::TrieError::InvalidMagic => DictError::InvalidMagic,
            lexime_trie::TrieError::InvalidVersion => DictError::UnsupportedVersion(0),
            lexime_trie::TrieError::TruncatedData => DictError::InvalidHeader,
        }
    }
}

pub struct SearchResult<'a> {
    pub reading: String,
    pub entries: &'a [DictEntry],
}

pub trait Dictionary: Send + Sync {
    fn lookup(&self, reading: &str) -> Option<&[DictEntry]>;
    fn predict(&self, prefix: &str, max_results: usize) -> Vec<SearchResult<'_>>;
    fn common_prefix_search(&self, query: &str) -> Vec<SearchResult<'_>>;
}
