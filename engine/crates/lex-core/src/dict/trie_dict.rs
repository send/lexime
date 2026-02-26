use std::collections::HashMap;

use lexime_trie::{DoubleArray, DoubleArrayRef};
use memmap2::Mmap;

use super::{DictEntry, Dictionary, SearchResult};

pub(super) const MAGIC: &[u8; 4] = b"LXDX";
pub(super) const VERSION: u8 = 4;
// magic(4) + version(1) + reserved(3) + trie_len(4) + pool_len(4) + entries_len(4) + reading_count(4) = 24
pub(super) const HEADER_SIZE: usize = 24;
const ENTRY_SIZE: usize = 12; // str_offset(4) + str_len(2) + cost(2) + left_id(2) + right_id(2)
pub(super) const SLOT_SIZE: usize = 6; // entry_offset(4) + count(2)

pub(super) enum TrieStore {
    Owned(DoubleArray<u8>),
    MmapRef(DoubleArrayRef<'static, u8>),
}

/// Dispatch a method call on the inner trie, avoiding `Box<dyn Iterator>`.
///
/// Each match arm produces a concrete iterator type, so the compiler can
/// monomorphize and inline the iteration instead of going through vtable
/// dispatch + heap allocation.
macro_rules! with_trie {
    ($self:expr, |$t:ident| $body:expr) => {
        match &$self.trie {
            TrieStore::Owned($t) => $body,
            TrieStore::MmapRef($t) => $body,
        }
    };
}

pub(super) enum ValuesStore {
    Owned {
        string_pool: Vec<u8>,
        entries_data: Vec<u8>,
        reading_index: Vec<u8>,
    },
    MmapRef {
        string_pool: &'static [u8],
        entries_data: &'static [u8],
        reading_index: &'static [u8],
    },
}

impl ValuesStore {
    pub(super) fn string_pool(&self) -> &[u8] {
        match self {
            ValuesStore::Owned { string_pool, .. } => string_pool,
            ValuesStore::MmapRef { string_pool, .. } => string_pool,
        }
    }

    pub(super) fn entries_data(&self) -> &[u8] {
        match self {
            ValuesStore::Owned { entries_data, .. } => entries_data,
            ValuesStore::MmapRef { entries_data, .. } => entries_data,
        }
    }

    pub(super) fn reading_index(&self) -> &[u8] {
        match self {
            ValuesStore::Owned { reading_index, .. } => reading_index,
            ValuesStore::MmapRef { reading_index, .. } => reading_index,
        }
    }

    fn reading_count(&self) -> usize {
        self.reading_index().len() / SLOT_SIZE
    }

    fn get_entries(&self, value_id: usize) -> Vec<DictEntry> {
        let idx = self.reading_index();
        let slot_start = value_id * SLOT_SIZE;
        if slot_start + SLOT_SIZE > idx.len() {
            return Vec::new();
        }

        let entry_offset =
            u32::from_ne_bytes(idx[slot_start..slot_start + 4].try_into().unwrap()) as usize;
        let count =
            u16::from_ne_bytes(idx[slot_start + 4..slot_start + 6].try_into().unwrap()) as usize;

        let data = self.entries_data();
        let pool = self.string_pool();
        let mut entries = Vec::with_capacity(count);

        for i in 0..count {
            let off = (entry_offset + i) * ENTRY_SIZE;
            if off + ENTRY_SIZE > data.len() {
                break;
            }
            let str_offset = u32::from_ne_bytes(data[off..off + 4].try_into().unwrap()) as usize;
            let str_len = u16::from_ne_bytes(data[off + 4..off + 6].try_into().unwrap()) as usize;
            let cost = i16::from_ne_bytes(data[off + 6..off + 8].try_into().unwrap());
            let left_id = u16::from_ne_bytes(data[off + 8..off + 10].try_into().unwrap());
            let right_id = u16::from_ne_bytes(data[off + 10..off + 12].try_into().unwrap());

            let surface = if str_offset + str_len <= pool.len() {
                String::from_utf8_lossy(&pool[str_offset..str_offset + str_len]).into_owned()
            } else {
                String::new()
            };

            entries.push(DictEntry {
                surface,
                cost,
                left_id,
                right_id,
            });
        }

        entries
    }
}

pub struct TrieDictionary {
    pub(super) trie: TrieStore,
    pub(super) values: ValuesStore,
    pub(super) _mmap: Option<Mmap>,
}

impl TrieDictionary {
    pub fn from_entries(entries: impl IntoIterator<Item = (String, Vec<DictEntry>)>) -> Self {
        let mut pairs: Vec<(String, Vec<DictEntry>)> = entries.into_iter().collect();
        for (_, candidates) in &mut pairs {
            candidates.sort_by_key(|e| e.cost);
        }
        pairs.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));

        let keys: Vec<&[u8]> = pairs.iter().map(|(r, _)| r.as_bytes()).collect();
        let trie = DoubleArray::<u8>::build(&keys);

        // Build string pool with global deduplication
        let mut pool = Vec::new();
        let mut pool_map: HashMap<String, u32> = HashMap::new();

        // Build entry records and reading index
        let mut entries_data = Vec::new();
        let mut reading_index = Vec::with_capacity(pairs.len() * SLOT_SIZE);

        for (_, candidates) in &pairs {
            let entry_offset = (entries_data.len() / ENTRY_SIZE) as u32;
            let count = candidates.len() as u16;

            for e in candidates {
                let str_offset = *pool_map.entry(e.surface.clone()).or_insert_with(|| {
                    let offset = pool.len() as u32;
                    pool.extend_from_slice(e.surface.as_bytes());
                    offset
                });
                let str_len = e.surface.len() as u16;

                entries_data.extend_from_slice(&str_offset.to_ne_bytes());
                entries_data.extend_from_slice(&str_len.to_ne_bytes());
                entries_data.extend_from_slice(&e.cost.to_ne_bytes());
                entries_data.extend_from_slice(&e.left_id.to_ne_bytes());
                entries_data.extend_from_slice(&e.right_id.to_ne_bytes());
            }

            reading_index.extend_from_slice(&entry_offset.to_ne_bytes());
            reading_index.extend_from_slice(&count.to_ne_bytes());
        }

        Self {
            trie: TrieStore::Owned(trie),
            values: ValuesStore::Owned {
                string_pool: pool,
                entries_data,
                reading_index,
            },
            _mmap: None,
        }
    }

    /// Iterate over all `(reading, entries)` pairs in the trie.
    pub fn iter(&self) -> impl Iterator<Item = (String, Vec<DictEntry>)> + '_ {
        let pairs: Vec<_> = with_trie!(self, |t| {
            t.predictive_search(b"")
                .map(|m| {
                    let reading = String::from_utf8(m.key)
                        .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned());
                    let idx = m.value_id as usize;
                    (reading, idx)
                })
                .collect()
        });
        pairs
            .into_iter()
            .map(move |(reading, idx)| (reading, self.values.get_entries(idx)))
    }

    /// Returns (reading_count, entry_count).
    pub fn stats(&self) -> (usize, usize) {
        let readings = self.values.reading_count();
        let entries = self.values.entries_data().len() / ENTRY_SIZE;
        (readings, entries)
    }
}

impl Dictionary for TrieDictionary {
    fn lookup(&self, reading: &str) -> Vec<DictEntry> {
        with_trie!(self, |t| {
            t.exact_match(reading.as_bytes())
                .map(|id| self.values.get_entries(id as usize))
                .unwrap_or_default()
        })
    }

    fn contains_reading(&self, reading: &str) -> bool {
        with_trie!(self, |t| t.exact_match(reading.as_bytes()).is_some())
    }

    fn predict(&self, prefix: &str, max_results: usize) -> Vec<SearchResult> {
        with_trie!(self, |t| {
            t.predictive_search(prefix.as_bytes())
                .take(max_results)
                .map(|m| SearchResult {
                    reading: String::from_utf8(m.key)
                        .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned()),
                    entries: self.values.get_entries(m.value_id as usize),
                })
                .collect()
        })
    }

    fn predict_ranked(
        &self,
        prefix: &str,
        max_results: usize,
        scan_limit: usize,
    ) -> Vec<(String, DictEntry)> {
        let mut flat: Vec<(String, DictEntry)> = Vec::new();
        with_trie!(self, |t| {
            for m in t.predictive_search(prefix.as_bytes()).take(scan_limit) {
                let reading = String::from_utf8(m.key)
                    .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned());
                let entries = self.values.get_entries(m.value_id as usize);
                flat.reserve(entries.len());
                for e in entries {
                    flat.push((reading.clone(), e));
                }
            }
        });

        flat.sort_by_key(|(_, e)| e.cost);

        let mut seen = std::collections::HashSet::new();
        flat.retain(|(_, e)| seen.insert(e.surface.clone()));

        flat.truncate(max_results);
        flat
    }

    fn common_prefix_search(&self, query: &str) -> Vec<SearchResult> {
        let query_bytes = query.as_bytes();
        with_trie!(self, |t| {
            t.common_prefix_search(query_bytes)
                .filter_map(|m| {
                    let reading = std::str::from_utf8(&query_bytes[..m.len]).ok()?;
                    Some(SearchResult {
                        reading: reading.to_string(),
                        entries: self.values.get_entries(m.value_id as usize),
                    })
                })
                .collect()
        })
    }
}
