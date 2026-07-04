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
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use bincode::Options as _;
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::dict::{DictEntry, Dictionary, SearchResult};

const MAGIC: &[u8; 4] = b"LXUW";
/// On-disk format version. LXUW is intentionally CRC-less and single-file (a
/// few-dozen-entry store with explicit `save`), so a future field addition
/// bumps this byte. Per the LXUD version-bump policy (design §7), an unknown
/// version must never hard-fail startup: [`open_recovering`] treats it as
/// corruption → quarantine + empty (the bytes are preserved for a downgrade),
/// and a bumped release must ship a reader + one-time rewrite for the old
/// version.
const VERSION: u8 = 1;

/// How many quarantined (`.corrupt-*`) user-dict files to retain, matching the
/// history store (design §8). Repeated corruption cannot accumulate junk.
const QUARANTINE_KEEP: usize = 3;

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
        let mut map = self.entries.write().expect("user_dict lock poisoned");
        let entries = map.entry(reading.to_string()).or_default();
        if entries.iter().any(|e| e.surface == surface) {
            return false;
        }
        entries.push(make_entry(surface));
        true
    }

    /// Unregister a word. Returns `true` if removed, `false` if not found.
    pub fn unregister(&self, reading: &str, surface: &str) -> bool {
        let mut map = self.entries.write().expect("user_dict lock poisoned");
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
        let map = self.entries.read().expect("user_dict lock poisoned");
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
        let map = self.entries.read().expect("user_dict lock poisoned");
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
        // LXUW is a no-CRC / v1-class format, so use the trailing-tolerant
        // reader (matches the original plain `bincode::deserialize`) with an
        // allocation cap: a corrupt length prefix must not trigger a giant
        // allocation before any data is read.
        let body = &bytes[5..];
        let records: Vec<UserWordRecord> = crate::persist::bincode_reader_v1(body.len())
            .deserialize(body)
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

    /// Durable atomic write: tmp → fsync → rename → best-effort dir fsync (via
    /// the shared [`crate::persist::write_atomic`]). Registration is off the
    /// hot path, so the F_FULLFSYNC is free; without it a rename could become
    /// durable before the contents (the self-inflicted corruption LXUD fixed),
    /// and the old `with_extension("tmp")` also left a stray `<stem>.tmp`
    /// outside the file family.
    pub fn save(&self, path: &Path) -> Result<(), io::Error> {
        let bytes = self.to_bytes()?;
        crate::persist::write_atomic(path, &bytes)
    }

    /// Strict open (no side effects, corrupt = `Err`), for offline tooling
    /// (the CLI): an audit tool must never rename a live user's file. The
    /// engine uses [`open_recovering`](Self::open_recovering) instead. Returns
    /// an empty dictionary if the file doesn't exist.
    pub fn open(path: &Path) -> Result<Self, io::Error> {
        match fs::read(path) {
            Ok(bytes) => Self::from_bytes(&bytes),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::new()),
            Err(e) => Err(e),
        }
    }

    /// Engine-side open with recovery: a corrupt file is quarantined (renamed
    /// aside as `<name>.corrupt-<ts>`, bytes preserved) and an empty dictionary
    /// is returned, so corruption never permanently disables user-word
    /// registration (the pre-hardening path threw, leaving the corrupt file in
    /// place to re-throw every startup — registered words lost forever). `Err`
    /// is reserved for environmental read failures (EACCES etc.): visible
    /// degradation, not a silent stop. Quarantine-rename failure is **not** an
    /// `Err` — an empty usable dictionary is still returned and the next `save`
    /// overwrites the file; the report still flags the loss.
    pub fn open_recovering(path: &Path) -> Result<(Self, UserDictOpenReport), io::Error> {
        match fs::read(path) {
            Ok(bytes) => match Self::from_bytes(&bytes) {
                Ok(dict) => Ok((dict, UserDictOpenReport::default())),
                Err(e) => {
                    warn!("user dictionary corrupt ({e}); quarantining");
                    let mut report = UserDictOpenReport {
                        quarantined: true,
                        quarantined_paths: Vec::new(),
                    };
                    if let crate::persist::Quarantine::Renamed(dest) =
                        crate::persist::quarantine(path, crate::user_history::now_epoch())
                    {
                        report.quarantined_paths.push(dest);
                    }
                    crate::persist::rotate_quarantined(path, QUARANTINE_KEEP);
                    Ok((Self::new(), report))
                }
            },
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                Ok((Self::new(), UserDictOpenReport::default()))
            }
            Err(e) => Err(e),
        }
    }
}

/// What [`UserDictionary::open_recovering`] found and did. The user dictionary
/// has a single recovery event — corruption → quarantine — so the report is a
/// flag plus the rescued path(s); policy (`data_loss_suspected`) is computed
/// here so the Swift side stays presentational (mirrors the history report).
#[derive(Default)]
pub struct UserDictOpenReport {
    /// The file was corrupt and quarantined (renamed aside, or removed if the
    /// rename failed); an empty dictionary was returned and the previously
    /// registered words are lost. Set regardless of whether the rename
    /// succeeded, so a byte-destroying fallback is still surfaced.
    pub quarantined: bool,
    /// Where the corrupt bytes were preserved (`.corrupt-<ts>`), when the
    /// rename succeeded. Empty if the file was removed as a last resort.
    pub quarantined_paths: Vec<PathBuf>,
}

impl UserDictOpenReport {
    /// Whether registered words were lost in a way the user should hear about.
    /// Any quarantine qualifies (the corrupt file is gone from the live path),
    /// including the byte-destroying `Removed`/`Failed` fallback.
    pub fn data_loss_suspected(&self) -> bool {
        self.quarantined
    }
}

impl Default for UserDictionary {
    fn default() -> Self {
        Self::new()
    }
}

impl Dictionary for UserDictionary {
    fn lookup(&self, reading: &str) -> Vec<DictEntry> {
        let map = self.entries.read().expect("user_dict lock poisoned");
        map.get(reading).cloned().unwrap_or_default()
    }

    fn predict(&self, prefix: &str, max_results: usize) -> Vec<SearchResult> {
        let map = self.entries.read().expect("user_dict lock poisoned");
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
        let map = self.entries.read().expect("user_dict lock poisoned");
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
