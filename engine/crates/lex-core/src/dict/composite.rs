use std::collections::HashMap;
use std::sync::Arc;

use super::{DictEntry, Dictionary, SearchResult};

/// A dictionary that merges results from multiple layers.
///
/// Layers are searched in order; later layers have higher priority.
/// Duplicate entries (same reading + surface) are deduplicated, keeping
/// the lowest cost across all layers.
pub struct CompositeDictionary {
    layers: Vec<Arc<dyn Dictionary>>,
}

impl CompositeDictionary {
    pub fn new(layers: Vec<Arc<dyn Dictionary>>) -> Self {
        Self { layers }
    }
}

/// Deduplicate entries by surface, keeping the lowest cost for each.
fn dedup_entries(entries: Vec<DictEntry>) -> Vec<DictEntry> {
    let mut best: HashMap<String, DictEntry> = HashMap::new();
    for e in entries {
        best.entry(e.surface.clone())
            .and_modify(|existing| {
                if e.cost < existing.cost {
                    *existing = e.clone();
                }
            })
            .or_insert(e);
    }
    let mut result: Vec<DictEntry> = best.into_values().collect();
    result.sort_by_key(|e| e.cost);
    result
}

/// Merge search results by reading, deduplicating entries within each reading.
fn merge_results(results: Vec<SearchResult>) -> Vec<SearchResult> {
    let mut by_reading: HashMap<String, Vec<DictEntry>> = HashMap::new();
    for sr in results {
        by_reading.entry(sr.reading).or_default().extend(sr.entries);
    }
    let mut merged: Vec<SearchResult> = by_reading
        .into_iter()
        .map(|(reading, entries)| SearchResult {
            reading,
            entries: dedup_entries(entries),
        })
        .collect();
    merged.sort_by(|a, b| a.reading.cmp(&b.reading));
    merged
}

impl Dictionary for CompositeDictionary {
    fn lookup(&self, reading: &str) -> Vec<DictEntry> {
        let mut all = Vec::new();
        for layer in &self.layers {
            all.extend(layer.lookup(reading));
        }
        dedup_entries(all)
    }

    fn predict(&self, prefix: &str, max_results: usize) -> Vec<SearchResult> {
        let mut all = Vec::new();
        for layer in &self.layers {
            all.extend(layer.predict(prefix, max_results));
        }
        let mut merged = merge_results(all);
        merged.truncate(max_results);
        merged
    }

    fn common_prefix_search(&self, query: &str) -> Vec<SearchResult> {
        let mut all = Vec::new();
        for layer in &self.layers {
            all.extend(layer.common_prefix_search(query));
        }
        merge_results(all)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dict::TrieDictionary;

    fn layer_a() -> Arc<dyn Dictionary> {
        let entries = vec![
            (
                "きょう".to_string(),
                vec![
                    DictEntry {
                        surface: "今日".to_string(),
                        cost: 3000,
                        left_id: 100,
                        right_id: 100,
                    },
                    DictEntry {
                        surface: "京".to_string(),
                        cost: 5000,
                        left_id: 101,
                        right_id: 101,
                    },
                ],
            ),
            (
                "は".to_string(),
                vec![DictEntry {
                    surface: "は".to_string(),
                    cost: 2000,
                    left_id: 200,
                    right_id: 200,
                }],
            ),
        ];
        Arc::new(TrieDictionary::from_entries(entries))
    }

    fn layer_b() -> Arc<dyn Dictionary> {
        let entries = vec![
            (
                "きょう".to_string(),
                vec![
                    DictEntry {
                        surface: "今日".to_string(),
                        cost: 2000, // lower cost override
                        left_id: 100,
                        right_id: 100,
                    },
                    DictEntry {
                        surface: "教".to_string(),
                        cost: 4000, // new entry
                        left_id: 102,
                        right_id: 102,
                    },
                ],
            ),
            (
                "きょうと".to_string(),
                vec![DictEntry {
                    surface: "京都".to_string(),
                    cost: 3500,
                    left_id: 103,
                    right_id: 103,
                }],
            ),
        ];
        Arc::new(TrieDictionary::from_entries(entries))
    }

    #[test]
    fn test_lookup_merges_and_deduplicates() {
        let dict = CompositeDictionary::new(vec![layer_a(), layer_b()]);
        let results = dict.lookup("きょう");
        let surfaces: Vec<&str> = results.iter().map(|e| e.surface.as_str()).collect();
        // Should contain all unique surfaces
        assert!(surfaces.contains(&"今日"));
        assert!(surfaces.contains(&"京"));
        assert!(surfaces.contains(&"教"));
        // "今日" should have the lower cost (2000 from layer_b)
        let kyou = results.iter().find(|e| e.surface == "今日").unwrap();
        assert_eq!(kyou.cost, 2000);
        // No duplicates
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn test_lookup_empty_layers() {
        let dict = CompositeDictionary::new(vec![]);
        assert!(dict.lookup("きょう").is_empty());
    }

    #[test]
    fn test_lookup_single_layer() {
        let dict = CompositeDictionary::new(vec![layer_a()]);
        let results = dict.lookup("きょう");
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_lookup_not_found() {
        let dict = CompositeDictionary::new(vec![layer_a(), layer_b()]);
        assert!(dict.lookup("そんざい").is_empty());
    }

    #[test]
    fn test_predict_merges() {
        let dict = CompositeDictionary::new(vec![layer_a(), layer_b()]);
        let results = dict.predict("きょう", 100);
        let readings: Vec<&str> = results.iter().map(|r| r.reading.as_str()).collect();
        // Should have both "きょう" (merged) and "きょうと" (from layer_b)
        assert!(readings.contains(&"きょう"));
        assert!(readings.contains(&"きょうと"));
        // "きょう" entries should be merged and deduplicated
        let kyou = results.iter().find(|r| r.reading == "きょう").unwrap();
        assert_eq!(kyou.entries.len(), 3); // 今日, 京, 教
    }

    #[test]
    fn test_common_prefix_search_merges() {
        let dict = CompositeDictionary::new(vec![layer_a(), layer_b()]);
        let results = dict.common_prefix_search("きょうは");
        let readings: Vec<&str> = results.iter().map(|r| r.reading.as_str()).collect();
        // Common prefix search on "きょうは" should find "きょう" (3-char prefix)
        assert!(readings.contains(&"きょう"));
        // Entries for "きょう" should be merged from both layers
        let kyou = results.iter().find(|r| r.reading == "きょう").unwrap();
        assert_eq!(kyou.entries.len(), 3); // 今日, 京, 教
    }

    #[test]
    fn test_predict_max_results() {
        let dict = CompositeDictionary::new(vec![layer_a(), layer_b()]);
        let results = dict.predict("きょう", 1);
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_dedup_keeps_lowest_cost() {
        let entries = vec![
            DictEntry {
                surface: "今日".to_string(),
                cost: 5000,
                left_id: 0,
                right_id: 0,
            },
            DictEntry {
                surface: "今日".to_string(),
                cost: 2000,
                left_id: 1,
                right_id: 1,
            },
            DictEntry {
                surface: "今日".to_string(),
                cost: 3000,
                left_id: 2,
                right_id: 2,
            },
        ];
        let deduped = dedup_entries(entries);
        assert_eq!(deduped.len(), 1);
        assert_eq!(deduped[0].cost, 2000);
    }
}
