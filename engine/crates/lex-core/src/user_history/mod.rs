//! User conversion history with time-decayed unigram/bigram boosting.
//!
//! Records confirmed conversions and uses frequency × recency scoring to
//! promote learned candidates in subsequent sessions.

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::dict::DictEntry;
use crate::settings::settings;

const MAGIC: &[u8; 4] = b"LXUD";
const VERSION: u8 = 1;

#[derive(Clone)]
pub struct UserHistory {
    /// reading → (surface → HistoryEntry)
    unigrams: HashMap<String, HashMap<String, HistoryEntry>>,
    /// prev_surface → ((next_reading, next_surface) → HistoryEntry)
    bigrams: HashMap<String, HashMap<(String, String), HistoryEntry>>,
}

#[derive(Clone)]
pub struct HistoryEntry {
    pub frequency: u32,
    pub last_used: u64,
}

impl HistoryEntry {
    /// Compute boost score with time decay.
    fn boost(&self, now: u64) -> i64 {
        let s = settings();
        let raw = (self.frequency as i64 * s.history.boost_per_use).min(s.history.max_boost);
        (raw as f64 * decay(self.last_used, now)) as i64
    }
}

/// Flat serialization format for bincode.
#[derive(Serialize, Deserialize)]
struct UserHistoryData {
    unigrams: Vec<UnigramRecord>,
    bigrams: Vec<BigramRecord>,
}

#[derive(Serialize, Deserialize)]
struct UnigramRecord {
    reading: String,
    surface: String,
    frequency: u32,
    last_used: u64,
}

#[derive(Serialize, Deserialize)]
struct BigramRecord {
    prev_surface: String,
    next_reading: String,
    next_surface: String,
    frequency: u32,
    last_used: u64,
}

pub fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn decay(last_used: u64, now: u64) -> f64 {
    let hours = (now.saturating_sub(last_used)) as f64 / 3600.0;
    1.0 / (1.0 + hours / settings().history.half_life_hours)
}

/// Evict lowest-score entries from a nested HashMap when exceeding capacity.
fn evict_map<K: Clone + Eq + std::hash::Hash>(
    map: &mut HashMap<String, HashMap<K, HistoryEntry>>,
    max: usize,
    now: u64,
) {
    let count: usize = map.values().map(|inner| inner.len()).sum();
    if count <= max {
        return;
    }
    let mut all: Vec<(String, K, f64)> = Vec::new();
    for (outer_key, inner) in map.iter() {
        for (inner_key, entry) in inner {
            let score = entry.frequency as f64 * decay(entry.last_used, now);
            all.push((outer_key.clone(), inner_key.clone(), score));
        }
    }
    let to_remove = count - max;
    // Partial sort: partition so the lowest-score `to_remove` entries are in all[..to_remove].
    // O(n) average vs O(n log n) for a full sort.
    all.select_nth_unstable_by(to_remove - 1, |a, b| {
        a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal)
    });
    for (outer_key, inner_key, _) in all[..to_remove].iter() {
        if let Some(inner) = map.get_mut(outer_key) {
            inner.remove(inner_key);
            if inner.is_empty() {
                map.remove(outer_key);
            }
        }
    }
}

impl Default for UserHistory {
    fn default() -> Self {
        Self::new()
    }
}

impl UserHistory {
    pub fn new() -> Self {
        Self {
            unigrams: HashMap::new(),
            bigrams: HashMap::new(),
        }
    }

    /// Record a confirmed conversion: list of (reading, surface) segments.
    pub fn record(&mut self, segments: &[(String, String)]) {
        let now = now_epoch();

        for (reading, surface) in segments {
            let entry = self
                .unigrams
                .entry(reading.clone())
                .or_default()
                .entry(surface.clone())
                .or_insert(HistoryEntry {
                    frequency: 0,
                    last_used: now,
                });
            entry.frequency += 1;
            entry.last_used = now;
        }

        // Bigram: consecutive pairs
        for pair in segments.windows(2) {
            let (_, prev_surface) = &pair[0];
            let (next_reading, next_surface) = &pair[1];

            let key = (next_reading.clone(), next_surface.clone());
            let entry = self
                .bigrams
                .entry(prev_surface.clone())
                .or_default()
                .entry(key)
                .or_insert(HistoryEntry {
                    frequency: 0,
                    last_used: now,
                });
            entry.frequency += 1;
            entry.last_used = now;
        }

        self.evict();
    }

    /// Compute unigram boost for a (reading, surface) pair.
    /// `now` should be obtained from [`now_epoch()`] once per batch operation.
    pub fn unigram_boost(&self, reading: &str, surface: &str, now: u64) -> i64 {
        self.unigrams
            .get(reading)
            .and_then(|inner| inner.get(surface))
            .map_or(0, |entry| entry.boost(now))
    }

    /// Compute bigram boost for (prev_surface → next_reading, next_surface).
    /// `now` should be obtained from [`now_epoch()`] once per batch operation.
    pub fn bigram_boost(
        &self,
        prev_surface: &str,
        next_reading: &str,
        next_surface: &str,
        now: u64,
    ) -> i64 {
        let key = (next_reading.to_string(), next_surface.to_string());
        self.bigrams
            .get(prev_surface)
            .and_then(|inner| inner.get(&key))
            .map_or(0, |entry| entry.boost(now))
    }

    /// Return successor words for a given previous surface, sorted by boost descending.
    /// Used by predictive mode to chain bigram phrases (Copilot-like completions).
    pub fn bigram_successors(&self, prev_surface: &str) -> Vec<(String, String, i64)> {
        let now = now_epoch();
        let Some(inner) = self.bigrams.get(prev_surface) else {
            return Vec::new();
        };
        let mut results: Vec<(String, String, i64)> = inner
            .iter()
            .map(|((reading, surface), entry)| (reading.clone(), surface.clone(), entry.boost(now)))
            .filter(|(_, _, boost)| *boost > 0)
            .collect();
        results.sort_by(|a, b| b.2.cmp(&a.2));
        results
    }

    /// Reorder dictionary candidates so learned entries appear first.
    pub fn reorder_candidates(&self, reading: &str, entries: &[DictEntry]) -> Vec<DictEntry> {
        let now = now_epoch();
        let mut with_boost: Vec<(i64, usize, &DictEntry)> = entries
            .iter()
            .enumerate()
            .map(|(i, e)| (self.unigram_boost(reading, &e.surface, now), i, e))
            .collect();

        // Boosted entries first (descending boost), then original order (ascending cost via index)
        with_boost.sort_by(|a, b| {
            b.0.cmp(&a.0) // higher boost first
                .then(a.1.cmp(&b.1)) // then original order (stable)
        });

        with_boost.iter().map(|(_, _, e)| (*e).clone()).collect()
    }

    /// Serialize to bytes (LXUD format).
    pub fn to_bytes(&self) -> Result<Vec<u8>, io::Error> {
        let data = self.to_data();
        let body = bincode::serialize(&data).map_err(io::Error::other)?;

        let mut buf = Vec::with_capacity(5 + body.len());
        buf.extend_from_slice(MAGIC);
        buf.push(VERSION);
        buf.extend_from_slice(&body);
        Ok(buf)
    }

    /// Deserialize from bytes (LXUD format).
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
        let data: UserHistoryData = bincode::deserialize(&bytes[5..])
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        Ok(Self::from_data(data))
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

    /// Open from file, returning empty UserHistory if file doesn't exist.
    pub fn open(path: &Path) -> Result<Self, io::Error> {
        match fs::read(path) {
            Ok(bytes) => Self::from_bytes(&bytes),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::new()),
            Err(e) => Err(e),
        }
    }

    fn to_data(&self) -> UserHistoryData {
        let mut unigrams = Vec::new();
        for (reading, inner) in &self.unigrams {
            for (surface, entry) in inner {
                unigrams.push(UnigramRecord {
                    reading: reading.clone(),
                    surface: surface.clone(),
                    frequency: entry.frequency,
                    last_used: entry.last_used,
                });
            }
        }

        let mut bigrams = Vec::new();
        for (prev, inner) in &self.bigrams {
            for ((next_r, next_s), entry) in inner {
                bigrams.push(BigramRecord {
                    prev_surface: prev.clone(),
                    next_reading: next_r.clone(),
                    next_surface: next_s.clone(),
                    frequency: entry.frequency,
                    last_used: entry.last_used,
                });
            }
        }

        UserHistoryData { unigrams, bigrams }
    }

    fn from_data(data: UserHistoryData) -> Self {
        let mut unigrams: HashMap<String, HashMap<String, HistoryEntry>> = HashMap::new();
        for rec in data.unigrams {
            unigrams.entry(rec.reading).or_default().insert(
                rec.surface,
                HistoryEntry {
                    frequency: rec.frequency,
                    last_used: rec.last_used,
                },
            );
        }

        let mut bigrams: HashMap<String, HashMap<(String, String), HistoryEntry>> = HashMap::new();
        for rec in data.bigrams {
            bigrams.entry(rec.prev_surface).or_default().insert(
                (rec.next_reading, rec.next_surface),
                HistoryEntry {
                    frequency: rec.frequency,
                    last_used: rec.last_used,
                },
            );
        }

        Self { unigrams, bigrams }
    }

    /// Evict lowest-score entries when exceeding capacity.
    fn evict(&mut self) {
        let s = settings();
        let now = now_epoch();
        evict_map(&mut self.unigrams, s.history.max_unigrams, now);
        evict_map(&mut self.bigrams, s.history.max_bigrams, now);
    }
}
