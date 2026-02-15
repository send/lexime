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

        let trie_len = trie_data.len() as u32;
        let values_len = values_data.len() as u32;

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

    /// Return prediction candidates ranked by cost (lowest first).
    ///
    /// Scans up to `scan_limit` readings from the trie's predictive search,
    /// flattens all entries, deduplicates by surface (keeping the lowest cost),
    /// and returns the top `max_results` entries as `(reading, DictEntry)` pairs.
    pub fn predict_ranked(
        &self,
        prefix: &str,
        max_results: usize,
        scan_limit: usize,
    ) -> Vec<(String, DictEntry)> {
        let mut flat: Vec<(String, DictEntry)> = self
            .trie
            .predictive_search(prefix.as_bytes())
            .take(scan_limit)
            .flat_map(|m| {
                let reading = String::from_utf8(m.key)
                    .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned());
                self.values[m.value_id as usize]
                    .iter()
                    .map(move |e| (reading.clone(), e.clone()))
            })
            .collect();

        flat.sort_by_key(|(_, e)| e.cost);

        let mut seen = std::collections::HashSet::new();
        flat.retain(|(_, e)| seen.insert(e.surface.clone()));

        flat.truncate(max_results);
        flat
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_dict() -> TrieDictionary {
        let entries = vec![
            (
                "かん".to_string(),
                vec![
                    DictEntry {
                        surface: "缶".to_string(),
                        cost: 5000,
                        left_id: 0,
                        right_id: 0,
                    },
                    DictEntry {
                        surface: "管".to_string(),
                        cost: 5200,
                        left_id: 0,
                        right_id: 0,
                    },
                ],
            ),
            (
                "かんじ".to_string(),
                vec![
                    DictEntry {
                        surface: "漢字".to_string(),
                        cost: 5100,
                        left_id: 0,
                        right_id: 0,
                    },
                    DictEntry {
                        surface: "感じ".to_string(),
                        cost: 5150,
                        left_id: 0,
                        right_id: 0,
                    },
                    DictEntry {
                        surface: "幹事".to_string(),
                        cost: 5300,
                        left_id: 0,
                        right_id: 0,
                    },
                ],
            ),
            (
                "かんじょう".to_string(),
                vec![
                    DictEntry {
                        surface: "感情".to_string(),
                        cost: 5000,
                        left_id: 0,
                        right_id: 0,
                    },
                    DictEntry {
                        surface: "勘定".to_string(),
                        cost: 5400,
                        left_id: 0,
                        right_id: 0,
                    },
                ],
            ),
            (
                "き".to_string(),
                vec![DictEntry {
                    surface: "木".to_string(),
                    cost: 4000,
                    left_id: 0,
                    right_id: 0,
                }],
            ),
        ];
        TrieDictionary::from_entries(entries)
    }

    #[test]
    fn test_lookup_exact() {
        let dict = sample_dict();
        let results = dict.lookup("かんじ").unwrap();
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].surface, "漢字");
        assert_eq!(results[1].surface, "感じ");
        assert_eq!(results[2].surface, "幹事");
    }

    #[test]
    fn test_lookup_not_found() {
        let dict = sample_dict();
        assert!(dict.lookup("そんざい").is_none());
    }

    #[test]
    fn test_predict() {
        let dict = sample_dict();
        let results = dict.predict("かん", 100);
        assert_eq!(results.len(), 3); // かん, かんじ, かんじょう
        let readings: Vec<&str> = results.iter().map(|r| r.reading.as_str()).collect();
        assert!(readings.contains(&"かん"));
        assert!(readings.contains(&"かんじ"));
        assert!(readings.contains(&"かんじょう"));
    }

    #[test]
    fn test_predict_max_results() {
        let dict = sample_dict();
        let results = dict.predict("かん", 2);
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_predict_max_results_zero() {
        let dict = sample_dict();
        let results = dict.predict("かん", 0);
        assert!(results.is_empty());
    }

    #[test]
    fn test_predict_no_match() {
        let dict = sample_dict();
        let results = dict.predict("そ", 100);
        assert!(results.is_empty());
    }

    #[test]
    fn test_cost_ordering() {
        let dict = sample_dict();
        let results = dict.lookup("かんじ").unwrap();
        for w in results.windows(2) {
            assert!(w[0].cost <= w[1].cost, "entries should be sorted by cost");
        }
    }

    #[test]
    fn test_serialize_roundtrip() {
        let dict = sample_dict();
        let bytes = dict.to_bytes().unwrap();
        let dict2 = TrieDictionary::from_bytes(&bytes).unwrap();

        let r1 = dict.lookup("かんじ").unwrap();
        let r2 = dict2.lookup("かんじ").unwrap();
        assert_eq!(r1.len(), r2.len());
        for (a, b) in r1.iter().zip(r2.iter()) {
            assert_eq!(a.surface, b.surface);
            assert_eq!(a.cost, b.cost);
        }
    }

    #[test]
    fn test_invalid_magic() {
        let result = TrieDictionary::from_bytes(b"XXXX\x02data");
        assert!(matches!(result, Err(DictError::InvalidMagic)));
    }

    #[test]
    fn test_header_too_short() {
        let result = TrieDictionary::from_bytes(b"LXD");
        assert!(matches!(result, Err(DictError::InvalidHeader)));
    }

    #[test]
    fn test_unsupported_version() {
        let result = TrieDictionary::from_bytes(b"LXDX\x99");
        assert!(matches!(result, Err(DictError::UnsupportedVersion(0x99))));
    }

    #[test]
    fn test_predict_ranked_cost_order() {
        let dict = sample_dict();
        let results = dict.predict_ranked("かん", 100, 200);
        // Should be sorted by cost ascending
        for w in results.windows(2) {
            assert!(
                w[0].1.cost <= w[1].1.cost,
                "predict_ranked should be cost-ordered: {} <= {}",
                w[0].1.cost,
                w[1].1.cost,
            );
        }
    }

    #[test]
    fn test_predict_ranked_dedup_surface() {
        // Create a dict where two different readings produce the same surface
        let entries = vec![
            (
                "かん".to_string(),
                vec![DictEntry {
                    surface: "感".to_string(),
                    cost: 5200,
                    left_id: 0,
                    right_id: 0,
                }],
            ),
            (
                "かんじ".to_string(),
                vec![DictEntry {
                    surface: "感".to_string(),
                    cost: 5000,
                    left_id: 0,
                    right_id: 0,
                }],
            ),
        ];
        let dict = TrieDictionary::from_entries(entries);
        let results = dict.predict_ranked("かん", 100, 200);
        // "感" should appear only once, with the lower cost (5000)
        let surfaces: Vec<&str> = results.iter().map(|(_, e)| e.surface.as_str()).collect();
        assert_eq!(
            surfaces.iter().filter(|&&s| s == "感").count(),
            1,
            "duplicate surface should be deduplicated"
        );
        let entry = results.iter().find(|(_, e)| e.surface == "感").unwrap();
        assert_eq!(entry.1.cost, 5000, "should keep lowest cost");
    }

    #[test]
    fn test_predict_ranked_max_results() {
        let dict = sample_dict();
        let results = dict.predict_ranked("かん", 2, 200);
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_predict_ranked_no_match() {
        let dict = sample_dict();
        let results = dict.predict_ranked("そ", 100, 200);
        assert!(results.is_empty());
    }

    // --- Integration tests (require compiled Mozc dictionary) ---

    #[test]
    #[ignore]
    fn test_mozc_dict_known_entries() {
        let dict_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("data")
            .join("lexime-sudachi.dict");
        let dict = TrieDictionary::open(&dict_path)
            .expect("failed to open lexime-sudachi.dict — run `make dict` first");

        // かんじ should have 漢字
        let results = dict.lookup("かんじ").expect("かんじ should exist");
        let surfaces: Vec<&str> = results.iter().map(|e| e.surface.as_str()).collect();
        assert!(
            surfaces.contains(&"漢字"),
            "漢字 not found in: {surfaces:?}"
        );
        assert!(
            surfaces.contains(&"感じ"),
            "感じ not found in: {surfaces:?}"
        );

        // にほん should have 日本
        let results = dict.lookup("にほん").expect("にほん should exist");
        let surfaces: Vec<&str> = results.iter().map(|e| e.surface.as_str()).collect();
        assert!(
            surfaces.contains(&"日本"),
            "日本 not found in: {surfaces:?}"
        );
    }

    #[test]
    #[ignore]
    fn test_mozc_dict_predict_performance() {
        let dict_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("data")
            .join("lexime-sudachi.dict");
        let dict = TrieDictionary::open(&dict_path)
            .expect("failed to open lexime-sudachi.dict — run `make dict` first");

        let prefixes = ["か", "かん", "と", "たべ", "に"];
        for prefix in &prefixes {
            let start = std::time::Instant::now();
            let results = dict.predict(prefix, 100);
            let elapsed = start.elapsed();
            assert!(
                elapsed.as_millis() < 5,
                "predict({prefix}) took {elapsed:?}, expected <5ms"
            );
            assert!(!results.is_empty(), "predict({prefix}) returned no results");
        }
    }
}
