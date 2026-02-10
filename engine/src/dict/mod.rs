mod entry;
pub mod source;
mod trie_dict;

pub use entry::DictEntry;
pub use trie_dict::{DictError, TrieDictionary};

pub struct SearchResult {
    pub reading: String,
    pub entries: Vec<DictEntry>,
}

pub trait Dictionary: Send + Sync {
    fn lookup(&self, reading: &str) -> Option<&[DictEntry]>;
    fn predict(&self, prefix: &str, max_results: usize) -> Vec<SearchResult>;
}
