//! Dictionary and connection-matrix storage.
//!
//! `TrieDictionary` stores reading → entries mappings in a serialized trie.
//! `ConnectionMatrix` stores POS bigram transition costs for Viterbi scoring.

mod composite;
pub mod connection;
mod connection_io;
mod entry;
#[cfg(test)]
mod tests;
mod trie_dict;

pub use composite::CompositeDictionary;
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

pub struct SearchResult {
    pub reading: String,
    pub entries: Vec<DictEntry>,
}

pub trait Dictionary: Send + Sync {
    fn lookup(&self, reading: &str) -> Vec<DictEntry>;
    fn predict(&self, prefix: &str, max_results: usize) -> Vec<SearchResult>;
    fn common_prefix_search(&self, query: &str) -> Vec<SearchResult>;

    /// Prediction candidates ranked by cost, deduplicated by surface.
    ///
    /// Scans up to `scan_limit` readings from predictive search, flattens all
    /// entries, deduplicates by surface (keeping the lowest cost), and returns
    /// the top `max_results` entries as `(reading, DictEntry)` pairs.
    fn predict_ranked(
        &self,
        prefix: &str,
        max_results: usize,
        scan_limit: usize,
    ) -> Vec<(String, DictEntry)> {
        let mut flat: Vec<(String, DictEntry)> = Vec::new();
        for sr in self.predict(prefix, scan_limit) {
            flat.reserve(sr.entries.len());
            for e in sr.entries {
                flat.push((sr.reading.clone(), e));
            }
        }

        flat.sort_by_key(|(_, e)| e.cost);

        let mut seen = std::collections::HashSet::new();
        flat.retain(|(_, e)| seen.insert(e.surface.clone()));

        flat.truncate(max_results);
        flat
    }
}
