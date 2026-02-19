use std::collections::HashMap;
use std::fs::{self, File};
use std::path::Path;

use lexime_trie::{DoubleArray, DoubleArrayRef};
use memmap2::Mmap;

use super::{DictEntry, DictError, Dictionary, SearchResult};

const MAGIC: &[u8; 4] = b"LXDX";
const VERSION: u8 = 4;
// magic(4) + version(1) + reserved(3) + trie_len(4) + pool_len(4) + entries_len(4) + reading_count(4) = 24
const HEADER_SIZE: usize = 24;
const ENTRY_SIZE: usize = 12; // str_offset(4) + str_len(2) + cost(2) + left_id(2) + right_id(2)
const SLOT_SIZE: usize = 6; // entry_offset(4) + count(2)

enum TrieStore {
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

enum ValuesStore {
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
    fn string_pool(&self) -> &[u8] {
        match self {
            ValuesStore::Owned { string_pool, .. } => string_pool,
            ValuesStore::MmapRef { string_pool, .. } => string_pool,
        }
    }

    fn entries_data(&self) -> &[u8] {
        match self {
            ValuesStore::Owned { entries_data, .. } => entries_data,
            ValuesStore::MmapRef { entries_data, .. } => entries_data,
        }
    }

    fn reading_index(&self) -> &[u8] {
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
            u32::from_le_bytes(idx[slot_start..slot_start + 4].try_into().unwrap()) as usize;
        let count =
            u16::from_le_bytes(idx[slot_start + 4..slot_start + 6].try_into().unwrap()) as usize;

        let data = self.entries_data();
        let pool = self.string_pool();
        let mut entries = Vec::with_capacity(count);

        for i in 0..count {
            let off = (entry_offset + i) * ENTRY_SIZE;
            if off + ENTRY_SIZE > data.len() {
                break;
            }
            let str_offset = u32::from_le_bytes(data[off..off + 4].try_into().unwrap()) as usize;
            let str_len = u16::from_le_bytes(data[off + 4..off + 6].try_into().unwrap()) as usize;
            let cost = i16::from_le_bytes(data[off + 6..off + 8].try_into().unwrap());
            let left_id = u16::from_le_bytes(data[off + 8..off + 10].try_into().unwrap());
            let right_id = u16::from_le_bytes(data[off + 10..off + 12].try_into().unwrap());

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
    trie: TrieStore,
    values: ValuesStore,
    _mmap: Option<Mmap>,
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

                entries_data.extend_from_slice(&str_offset.to_le_bytes());
                entries_data.extend_from_slice(&str_len.to_le_bytes());
                entries_data.extend_from_slice(&e.cost.to_le_bytes());
                entries_data.extend_from_slice(&e.left_id.to_le_bytes());
                entries_data.extend_from_slice(&e.right_id.to_le_bytes());
            }

            reading_index.extend_from_slice(&entry_offset.to_le_bytes());
            reading_index.extend_from_slice(&count.to_le_bytes());
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

    pub fn to_bytes(&self) -> Result<Vec<u8>, DictError> {
        let trie_data = match &self.trie {
            TrieStore::Owned(da) => da.as_bytes(),
            TrieStore::MmapRef(_) => {
                return Err(DictError::Parse(
                    "cannot serialize mmap-backed dictionary".into(),
                ));
            }
        };

        let pool = self.values.string_pool();
        let entries = self.values.entries_data();
        let index = self.values.reading_index();

        let trie_len: u32 = trie_data
            .len()
            .try_into()
            .map_err(|_| DictError::Parse("trie data exceeds u32::MAX".to_string()))?;
        let pool_len: u32 = pool
            .len()
            .try_into()
            .map_err(|_| DictError::Parse("string pool exceeds u32::MAX".to_string()))?;
        let entries_len: u32 = entries
            .len()
            .try_into()
            .map_err(|_| DictError::Parse("entries data exceeds u32::MAX".to_string()))?;
        let reading_count: u32 = (index.len() / SLOT_SIZE) as u32;

        let total = HEADER_SIZE + trie_data.len() + pool.len() + entries.len() + index.len();
        let mut buf = Vec::with_capacity(total);
        buf.extend_from_slice(MAGIC);
        buf.push(VERSION);
        buf.extend_from_slice(&[0u8; 3]); // reserved
        buf.extend_from_slice(&trie_len.to_le_bytes());
        buf.extend_from_slice(&pool_len.to_le_bytes());
        buf.extend_from_slice(&entries_len.to_le_bytes());
        buf.extend_from_slice(&reading_count.to_le_bytes());
        buf.extend_from_slice(&trie_data);
        buf.extend_from_slice(pool);
        buf.extend_from_slice(entries);
        buf.extend_from_slice(index);

        Ok(buf)
    }

    pub fn from_bytes(data: &[u8]) -> Result<Self, DictError> {
        if data.len() < 5 {
            return Err(DictError::InvalidHeader);
        }
        if &data[..4] != MAGIC {
            return Err(DictError::InvalidMagic);
        }
        if data[4] != VERSION {
            return Err(DictError::UnsupportedVersion(data[4]));
        }
        if data.len() < HEADER_SIZE {
            return Err(DictError::InvalidHeader);
        }

        let trie_len = u32::from_le_bytes(data[8..12].try_into().unwrap()) as usize;
        let pool_len = u32::from_le_bytes(data[12..16].try_into().unwrap()) as usize;
        let entries_len = u32::from_le_bytes(data[16..20].try_into().unwrap()) as usize;
        let reading_count = u32::from_le_bytes(data[20..24].try_into().unwrap()) as usize;
        let index_len = reading_count * SLOT_SIZE;

        let expected = HEADER_SIZE + trie_len + pool_len + entries_len + index_len;
        if data.len() < expected {
            return Err(DictError::InvalidHeader);
        }

        let trie_start = HEADER_SIZE;
        let pool_start = trie_start + trie_len;
        let entries_start = pool_start + pool_len;
        let index_start = entries_start + entries_len;

        let trie = DoubleArray::<u8>::from_bytes(&data[trie_start..trie_start + trie_len])?;

        Ok(Self {
            trie: TrieStore::Owned(trie),
            values: ValuesStore::Owned {
                string_pool: data[pool_start..pool_start + pool_len].to_vec(),
                entries_data: data[entries_start..entries_start + entries_len].to_vec(),
                reading_index: data[index_start..index_start + index_len].to_vec(),
            },
            _mmap: None,
        })
    }

    /// Open a dictionary file, using mmap for zero-copy access.
    ///
    /// Both the trie and values data are referenced directly from the
    /// memory-mapped region, eliminating ~60-80MB of heap allocation.
    pub fn open(path: &Path) -> Result<Self, DictError> {
        let file = File::open(path)?;
        // SAFETY: The file is opened read-only and the mapping is immutable.
        let mmap = unsafe { Mmap::map(&file)? };

        if mmap.len() < 5 {
            return Err(DictError::InvalidHeader);
        }
        if &mmap[..4] != MAGIC {
            return Err(DictError::InvalidMagic);
        }
        if mmap[4] != VERSION {
            return Err(DictError::UnsupportedVersion(mmap[4]));
        }
        if mmap.len() < HEADER_SIZE {
            return Err(DictError::InvalidHeader);
        }

        let trie_len = u32::from_le_bytes(mmap[8..12].try_into().unwrap()) as usize;
        let pool_len = u32::from_le_bytes(mmap[12..16].try_into().unwrap()) as usize;
        let entries_len = u32::from_le_bytes(mmap[16..20].try_into().unwrap()) as usize;
        let reading_count = u32::from_le_bytes(mmap[20..24].try_into().unwrap()) as usize;
        let index_len = reading_count * SLOT_SIZE;

        let expected = HEADER_SIZE + trie_len + pool_len + entries_len + index_len;
        if mmap.len() < expected {
            return Err(DictError::InvalidHeader);
        }

        let trie_start = HEADER_SIZE;
        let pool_start = trie_start + trie_len;
        let entries_start = pool_start + pool_len;
        let index_start = entries_start + entries_len;

        // Zero-copy trie from mmap
        let trie_ref =
            DoubleArrayRef::<u8>::from_bytes_ref(&mmap[trie_start..trie_start + trie_len])?;
        // SAFETY: The mmap is stored in self._mmap and will be dropped after trie and values
        // (Rust drops fields in declaration order: trie, values, _mmap).
        let trie_ref = unsafe {
            std::mem::transmute::<DoubleArrayRef<'_, u8>, DoubleArrayRef<'static, u8>>(trie_ref)
        };

        // SAFETY: The slices reference mmap data. The mmap is stored in self._mmap
        // and will outlive these references (dropped last due to field order).
        let string_pool = unsafe {
            std::mem::transmute::<&[u8], &'static [u8]>(&mmap[pool_start..pool_start + pool_len])
        };
        let entries_data = unsafe {
            std::mem::transmute::<&[u8], &'static [u8]>(
                &mmap[entries_start..entries_start + entries_len],
            )
        };
        let reading_index = unsafe {
            std::mem::transmute::<&[u8], &'static [u8]>(&mmap[index_start..index_start + index_len])
        };

        Ok(Self {
            trie: TrieStore::MmapRef(trie_ref),
            values: ValuesStore::MmapRef {
                string_pool,
                entries_data,
                reading_index,
            },
            _mmap: Some(mmap),
        })
    }

    pub fn save(&self, path: &Path) -> Result<(), DictError> {
        Ok(fs::write(path, self.to_bytes()?)?)
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
