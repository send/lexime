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
mod trie_dict_io;

pub use composite::CompositeDictionary;
pub use entry::DictEntry;
pub use trie_dict::TrieDictionary;

use std::io;

/// Unified error type for dictionary and connection-matrix binary I/O.
///
/// Covers loading/saving both `TrieDictionary` (LXDX) and
/// `ConnectionMatrix` (LXCX) files. `InvalidMagic` / `InvalidHeader` /
/// `UnsupportedVersion` refer to lexime's own LXDX/LXCX framing;
/// failures from the embedded `lexime_trie` loader bubble up through
/// the dedicated `Trie` variant.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DictError {
    #[error("IO error: {0}")]
    Io(#[from] io::Error),

    #[error("invalid header (too short)")]
    InvalidHeader,

    #[error("invalid magic bytes (expected LXDX or LXCX)")]
    InvalidMagic,

    #[error("unsupported version: {0}")]
    UnsupportedVersion(u8),

    /// Failure bubbled up from the `lexime_trie` loader for the embedded
    /// trie section. Preserves the original `TrieError` variant so
    /// callers can distinguish (e.g.) alignment problems from structural
    /// corruption without string-matching on messages.
    #[error("trie load failed: {0}")]
    Trie(#[from] lexime_trie::TrieError),

    #[error("serialization error: {0}")]
    Serialize(bincode::Error),

    #[error("deserialization error: {0}")]
    Deserialize(bincode::Error),

    #[error("parse error: {0}")]
    Parse(String),
}

pub struct SearchResult {
    pub reading: String,
    pub entries: Vec<DictEntry>,
}

pub trait Dictionary: Send + Sync {
    fn lookup(&self, reading: &str) -> Vec<DictEntry>;
    fn predict(&self, prefix: &str, max_results: usize) -> Vec<SearchResult>;
    fn common_prefix_search(&self, query: &str) -> Vec<SearchResult>;

    /// Maximum reading length (in characters) across all entries.
    ///
    /// Used by `Lattice::extend` to bound the lookback window for
    /// incremental extension. Implementors should override this whenever
    /// a finite maximum is known. The default returns `usize::MAX` as a
    /// conservative fallback; `extend` also tracks the longest reading
    /// observed during construction, so the lookback is bounded even
    /// when this method is not overridden.
    fn max_reading_len(&self) -> usize {
        usize::MAX
    }

    /// Check whether any entry exists for the given reading.
    ///
    /// Default implementation delegates to `lookup().is_empty()`. Implementors
    /// can override this to avoid allocating entry vectors (e.g. using a trie
    /// `exact_match` check).
    fn contains_reading(&self, reading: &str) -> bool {
        !self.lookup(reading).is_empty()
    }

    /// Prediction candidates ranked by cost, deduplicated by surface.
    ///
    /// Scans up to `scan_limit` readings from predictive search (the window
    /// counts readings, whatever their entries), flattens all entries and
    /// hands them to [`rank_predictions`] — the one place the ranking lives.
    /// `exclude` drops entries before ranking (e.g. untrained symbols).
    fn predict_ranked(
        &self,
        prefix: &str,
        max_results: usize,
        scan_limit: usize,
        exclude: &dyn Fn(&str, &DictEntry) -> bool,
    ) -> Vec<(String, DictEntry)> {
        let mut flat: Vec<(String, DictEntry)> = Vec::new();
        for sr in self.predict(prefix, scan_limit) {
            flat.reserve(sr.entries.len());
            for e in sr.entries {
                flat.push((sr.reading.clone(), e));
            }
        }
        rank_predictions(flat, max_results, exclude)
    }
}

/// Rank flattened `(reading, entry)` predictions: drop what `exclude` says,
/// then order by cost, keep the cheapest entry per surface and take the top
/// `max_results`.
///
/// `exclude` runs before the dedup and the truncation so an excluded entry
/// neither holds a `max_results` slot nor shadows a kept entry of the same
/// surface (e.g. symbol (`おめが`, Ω) must not hide the word (`おーむ`, Ω)).
fn rank_predictions(
    mut flat: Vec<(String, DictEntry)>,
    max_results: usize,
    exclude: &dyn Fn(&str, &DictEntry) -> bool,
) -> Vec<(String, DictEntry)> {
    flat.retain(|(reading, e)| !exclude(reading, e));
    flat.sort_by_key(|(_, e)| e.cost);

    let mut seen = std::collections::HashSet::new();
    flat.retain(|(_, e)| seen.insert(e.surface.clone()));

    flat.truncate(max_results);
    flat
}
