//! User dictionary with runtime word registration.
//!
//! HashMap-based dictionary that implements `Dictionary` trait for integration
//! with `CompositeDictionary`. Uses `RwLock` for interior mutability so that
//! `register`/`unregister` can be called while sessions hold a read reference.

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;
use std::sync::RwLock;

use serde::{Deserialize, Serialize};

use crate::dict::{DictEntry, Dictionary, SearchResult};

const MAGIC: &[u8; 4] = b"LXUW";
const VERSION: u8 = 1;

/// POS ID for 名詞,一般 (from id.def).
const USER_POS_ID: u16 = 1852;
/// Cost lower than any system entry so user words always win.
const USER_COST: i16 = -1;

fn make_entry(surface: &str) -> DictEntry {
    DictEntry {
        surface: surface.to_string(),
        cost: USER_COST,
        left_id: USER_POS_ID,
        right_id: USER_POS_ID,
    }
}

pub struct UserDictionary {
    entries: RwLock<HashMap<String, Vec<DictEntry>>>,
}

impl UserDictionary {
    pub fn new() -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
        }
    }

    /// Register a word. Returns `true` if newly added, `false` if already exists.
    pub fn register(&self, reading: &str, surface: &str) -> bool {
        let mut map = self.entries.write().unwrap();
        let entries = map.entry(reading.to_string()).or_default();
        if entries.iter().any(|e| e.surface == surface) {
            return false;
        }
        entries.push(make_entry(surface));
        true
    }

    /// Unregister a word. Returns `true` if removed, `false` if not found.
    pub fn unregister(&self, reading: &str, surface: &str) -> bool {
        let mut map = self.entries.write().unwrap();
        let Some(entries) = map.get_mut(reading) else {
            return false;
        };
        let before = entries.len();
        entries.retain(|e| e.surface != surface);
        let removed = entries.len() < before;
        if entries.is_empty() {
            map.remove(reading);
        }
        removed
    }

    /// List all entries as (reading, surface) pairs, sorted by reading.
    pub fn list(&self) -> Vec<(String, String)> {
        let map = self.entries.read().unwrap();
        let mut result: Vec<(String, String)> = Vec::new();
        for (reading, entries) in map.iter() {
            for e in entries {
                result.push((reading.clone(), e.surface.clone()));
            }
        }
        result.sort();
        result
    }

    /// Serialize to bytes (LXUW format).
    pub fn to_bytes(&self) -> Result<Vec<u8>, io::Error> {
        let map = self.entries.read().unwrap();
        let records: Vec<UserWordRecord> = map
            .iter()
            .flat_map(|(reading, entries)| {
                entries.iter().map(move |e| UserWordRecord {
                    reading: reading.clone(),
                    surface: e.surface.clone(),
                })
            })
            .collect();

        let body = bincode::serialize(&records).map_err(io::Error::other)?;
        let mut buf = Vec::with_capacity(5 + body.len());
        buf.extend_from_slice(MAGIC);
        buf.push(VERSION);
        buf.extend_from_slice(&body);
        Ok(buf)
    }

    /// Deserialize from bytes (LXUW format).
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, io::Error> {
        if bytes.len() < 5 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "too short"));
        }
        if &bytes[0..4] != MAGIC {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "bad magic"));
        }
        if bytes[4] != VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unsupported version",
            ));
        }
        let records: Vec<UserWordRecord> = bincode::deserialize(&bytes[5..])
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        let mut map: HashMap<String, Vec<DictEntry>> = HashMap::new();
        for rec in records {
            map.entry(rec.reading)
                .or_default()
                .push(make_entry(&rec.surface));
        }
        Ok(Self {
            entries: RwLock::new(map),
        })
    }

    /// Atomic write: write to .tmp then rename.
    pub fn save(&self, path: &Path) -> Result<(), io::Error> {
        let bytes = self.to_bytes()?;
        let tmp = path.with_extension("tmp");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&tmp, &bytes)?;
        fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Open from file, returning empty UserDictionary if file doesn't exist.
    pub fn open(path: &Path) -> Result<Self, io::Error> {
        match fs::read(path) {
            Ok(bytes) => Self::from_bytes(&bytes),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::new()),
            Err(e) => Err(e),
        }
    }
}

impl Default for UserDictionary {
    fn default() -> Self {
        Self::new()
    }
}

impl Dictionary for UserDictionary {
    fn lookup(&self, reading: &str) -> Vec<DictEntry> {
        let map = self.entries.read().unwrap();
        map.get(reading).cloned().unwrap_or_default()
    }

    fn predict(&self, prefix: &str, max_results: usize) -> Vec<SearchResult> {
        let map = self.entries.read().unwrap();
        let mut results: Vec<SearchResult> = map
            .iter()
            .filter(|(k, _)| k.starts_with(prefix))
            .map(|(k, v)| SearchResult {
                reading: k.clone(),
                entries: v.clone(),
            })
            .collect();
        results.sort_by(|a, b| a.reading.cmp(&b.reading));
        results.truncate(max_results);
        results
    }

    fn common_prefix_search(&self, query: &str) -> Vec<SearchResult> {
        let map = self.entries.read().unwrap();
        let mut results = Vec::new();
        // Check all prefixes of query against the HashMap
        for end in 1..=query.len() {
            // Only split at char boundaries
            if !query.is_char_boundary(end) {
                continue;
            }
            let prefix = &query[..end];
            if let Some(entries) = map.get(prefix) {
                results.push(SearchResult {
                    reading: prefix.to_string(),
                    entries: entries.clone(),
                });
            }
        }
        results
    }
}

/// Flat serialization record — only (reading, surface) pairs are persisted.
/// POS and cost are constants, restored on load.
#[derive(Serialize, Deserialize)]
struct UserWordRecord {
    reading: String,
    surface: String,
}
