use std::fs::{self, File};
use std::path::Path;

use lexime_trie::DoubleArray;
use memmap2::Mmap;

use super::{DictEntry, DictError, Dictionary, SearchResult};

const MAGIC: &[u8; 4] = b"LXDX";
const VERSION: u8 = 2;
const HEADER_SIZE: usize = 4 + 1 + 4 + 4; // magic + version + trie_len + values_len = 13

pub struct TrieDictionary {
    trie: DoubleArray<u8>,
    values: Vec<Vec<DictEntry>>,
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
        let values: Vec<Vec<DictEntry>> = pairs.into_iter().map(|(_, v)| v).collect();

        Self { trie, values }
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, DictError> {
        let trie_data = self.trie.as_bytes();
        let values_data = bincode::serialize(&self.values).map_err(DictError::Serialize)?;

        let trie_len: u32 = trie_data
            .len()
            .try_into()
            .map_err(|_| DictError::Parse("trie data exceeds u32::MAX".to_string()))?;
        let values_len: u32 = values_data
            .len()
            .try_into()
            .map_err(|_| DictError::Parse("values data exceeds u32::MAX".to_string()))?;

        let mut buf = Vec::with_capacity(HEADER_SIZE + trie_data.len() + values_data.len());
        buf.extend_from_slice(MAGIC);
        buf.push(VERSION);
        buf.extend_from_slice(&trie_len.to_le_bytes());
        buf.extend_from_slice(&values_len.to_le_bytes());
        buf.extend_from_slice(&trie_data);
        buf.extend_from_slice(&values_data);

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

        let trie_len = u32::from_le_bytes(data[5..9].try_into().unwrap()) as usize;
        let values_len = u32::from_le_bytes(data[9..13].try_into().unwrap()) as usize;

        let expected = HEADER_SIZE + trie_len + values_len;
        if data.len() < expected {
            return Err(DictError::InvalidHeader);
        }

        let trie_start = HEADER_SIZE;
        let values_start = trie_start + trie_len;

        let trie = DoubleArray::<u8>::from_bytes(&data[trie_start..trie_start + trie_len])?;
        let values: Vec<Vec<DictEntry>> =
            bincode::deserialize(&data[values_start..values_start + values_len])
                .map_err(DictError::Deserialize)?;

        Ok(Self { trie, values })
    }

    /// Open a dictionary file, using mmap to avoid doubling peak memory.
    ///
    /// The trie is deserialized from the mapped region (avoiding a separate
    /// heap allocation for the raw file bytes), then the mapping is dropped.
    pub fn open(path: &Path) -> Result<Self, DictError> {
        let file = File::open(path)?;
        // SAFETY: The file is opened read-only and the mapping is immutable.
        // The Mmap is dropped after deserialization completes below.
        let mmap = unsafe { Mmap::map(&file)? };
        Self::from_bytes(&mmap)
    }

    pub fn save(&self, path: &Path) -> Result<(), DictError> {
        Ok(fs::write(path, self.to_bytes()?)?)
    }

    /// Iterate over all `(reading, entries)` pairs in the trie.
    pub fn iter(&self) -> impl Iterator<Item = (String, &Vec<DictEntry>)> {
        self.trie.predictive_search(b"").map(move |m| {
            let reading = String::from_utf8(m.key)
                .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned());
            (reading, &self.values[m.value_id as usize])
        })
    }

    /// Returns (reading_count, entry_count).
    pub fn stats(&self) -> (usize, usize) {
        let readings = self.values.len();
        let entries: usize = self.values.iter().map(|v| v.len()).sum();
        (readings, entries)
    }
}

impl Dictionary for TrieDictionary {
    fn lookup(&self, reading: &str) -> Option<&[DictEntry]> {
        self.trie
            .exact_match(reading.as_bytes())
            .map(|id| self.values[id as usize].as_slice())
    }

    fn predict(&self, prefix: &str, max_results: usize) -> Vec<SearchResult<'_>> {
        self.trie
            .predictive_search(prefix.as_bytes())
            .take(max_results)
            .map(|m| SearchResult {
                reading: String::from_utf8(m.key)
                    .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned()),
                entries: self.values[m.value_id as usize].as_slice(),
            })
            .collect()
    }

    fn predict_ranked(
        &self,
        prefix: &str,
        max_results: usize,
        scan_limit: usize,
    ) -> Vec<(String, DictEntry)> {
        let mut flat: Vec<(String, DictEntry)> = Vec::new();
        for m in self
            .trie
            .predictive_search(prefix.as_bytes())
            .take(scan_limit)
        {
            let reading = String::from_utf8(m.key)
                .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned());
            let entries = &self.values[m.value_id as usize];
            flat.reserve(entries.len());
            for e in entries {
                flat.push((reading.clone(), e.clone()));
            }
        }

        flat.sort_by_key(|(_, e)| e.cost);

        let mut seen = std::collections::HashSet::new();
        flat.retain(|(_, e)| seen.insert(e.surface.clone()));

        flat.truncate(max_results);
        flat
    }

    fn common_prefix_search(&self, query: &str) -> Vec<SearchResult<'_>> {
        let query_bytes = query.as_bytes();
        self.trie
            .common_prefix_search(query_bytes)
            .filter_map(|m| {
                let reading = std::str::from_utf8(&query_bytes[..m.len]).ok()?;
                Some(SearchResult {
                    reading: reading.to_string(),
                    entries: self.values[m.value_id as usize].as_slice(),
                })
            })
            .collect()
    }
}
