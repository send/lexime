pub mod cost;

use std::cmp::Ordering;
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::dict::DictEntry;

const MAGIC: &[u8; 4] = b"LXUD";
const VERSION: u8 = 1;
const MAX_UNIGRAMS: usize = 10_000;
const MAX_BIGRAMS: usize = 10_000;
const BOOST_PER_USE: i64 = 1500;
const MAX_BOOST: i64 = 10000;
const HALF_LIFE_HOURS: f64 = 168.0;

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
        let raw = (self.frequency as i64 * BOOST_PER_USE).min(MAX_BOOST);
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

fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn decay(last_used: u64, now: u64) -> f64 {
    let hours = (now.saturating_sub(last_used)) as f64 / 3600.0;
    1.0 / (1.0 + hours / HALF_LIFE_HOURS)
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

    fn unigram_count(&self) -> usize {
        self.unigrams.values().map(|inner| inner.len()).sum()
    }

    fn bigram_count(&self) -> usize {
        self.bigrams.values().map(|inner| inner.len()).sum()
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
    pub fn unigram_boost(&self, reading: &str, surface: &str) -> i64 {
        let now = now_epoch();
        self.unigrams
            .get(reading)
            .and_then(|inner| inner.get(surface))
            .map_or(0, |entry| entry.boost(now))
    }

    /// Compute bigram boost for (prev_surface → next_reading, next_surface).
    pub fn bigram_boost(&self, prev_surface: &str, next_reading: &str, next_surface: &str) -> i64 {
        let now = now_epoch();
        self.bigrams
            .get(prev_surface)
            .and_then(|inner| {
                inner
                    .iter()
                    .find(|((reading, surface), _)| {
                        reading == next_reading && surface == next_surface
                    })
                    .map(|(_, entry)| entry)
            })
            .map_or(0, |entry| entry.boost(now))
    }

    /// Reorder dictionary candidates so learned entries appear first.
    pub fn reorder_candidates(&self, reading: &str, entries: &[DictEntry]) -> Vec<DictEntry> {
        let mut with_boost: Vec<(i64, usize, &DictEntry)> = entries
            .iter()
            .enumerate()
            .map(|(i, e)| (self.unigram_boost(reading, &e.surface), i, e))
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
        let now = now_epoch();

        // Evict unigrams
        let count = self.unigram_count();
        if count > MAX_UNIGRAMS {
            let mut all: Vec<(String, String, f64)> = Vec::new();
            for (reading, inner) in &self.unigrams {
                for (surface, entry) in inner {
                    let score = entry.frequency as f64 * decay(entry.last_used, now);
                    all.push((reading.clone(), surface.clone(), score));
                }
            }
            all.sort_by(|a, b| a.2.partial_cmp(&b.2).unwrap_or(Ordering::Equal));
            let to_remove = count - MAX_UNIGRAMS;
            for (reading, surface, _) in all.iter().take(to_remove) {
                if let Some(inner) = self.unigrams.get_mut(reading) {
                    inner.remove(surface);
                    if inner.is_empty() {
                        self.unigrams.remove(reading);
                    }
                }
            }
        }

        // Evict bigrams
        let count = self.bigram_count();
        if count > MAX_BIGRAMS {
            let mut all: Vec<(String, (String, String), f64)> = Vec::new();
            for (prev, inner) in &self.bigrams {
                for (key, entry) in inner {
                    let score = entry.frequency as f64 * decay(entry.last_used, now);
                    all.push((prev.clone(), key.clone(), score));
                }
            }
            all.sort_by(|a, b| a.2.partial_cmp(&b.2).unwrap_or(Ordering::Equal));
            let to_remove = count - MAX_BIGRAMS;
            for (prev, key, _) in all.iter().take(to_remove) {
                if let Some(inner) = self.bigrams.get_mut(prev) {
                    inner.remove(key);
                    if inner.is_empty() {
                        self.bigrams.remove(prev);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_record_unigram() {
        let mut h = UserHistory::new();
        h.record(&[("きょう".into(), "今日".into())]);
        assert!(h.unigram_boost("きょう", "今日") > 0);
    }

    #[test]
    fn test_record_bigram() {
        let mut h = UserHistory::new();
        h.record(&[("きょう".into(), "今日".into()), ("は".into(), "は".into())]);
        assert!(h.bigram_boost("今日", "は", "は") > 0);
    }

    #[test]
    fn test_frequency_increment() {
        let mut h = UserHistory::new();
        h.record(&[("きょう".into(), "今日".into())]);
        h.record(&[("きょう".into(), "今日".into())]);
        let entry = &h.unigrams["きょう"]["今日"];
        assert_eq!(entry.frequency, 2);
    }

    #[test]
    fn test_serialize_roundtrip() {
        let mut h = UserHistory::new();
        h.record(&[("きょう".into(), "今日".into()), ("は".into(), "は".into())]);
        let bytes = h.to_bytes().unwrap();
        let h2 = UserHistory::from_bytes(&bytes).unwrap();
        assert!(h2.unigram_boost("きょう", "今日") > 0);
        assert!(h2.bigram_boost("今日", "は", "は") > 0);
    }

    #[test]
    fn test_file_roundtrip() {
        let dir = std::env::temp_dir().join("lexime_test_history");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.lxud");

        let mut h = UserHistory::new();
        h.record(&[("きょう".into(), "今日".into())]);
        h.save(&path).unwrap();

        let h2 = UserHistory::open(&path).unwrap();
        assert!(h2.unigram_boost("きょう", "今日") > 0);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_open_nonexistent() {
        let h = UserHistory::open(Path::new("/nonexistent/path/history.lxud")).unwrap();
        assert_eq!(h.unigram_boost("きょう", "今日"), 0);
    }

    #[test]
    fn test_evict() {
        let mut h = UserHistory::new();
        // Insert MAX_UNIGRAMS + 1 entries
        for i in 0..=MAX_UNIGRAMS {
            h.record(&[(format!("r{i}"), format!("s{i}"))]);
        }
        assert!(h.unigram_count() <= MAX_UNIGRAMS);
    }

    #[test]
    fn test_reorder_candidates() {
        let mut h = UserHistory::new();
        h.record(&[("きょう".into(), "京".into())]);

        let entries = vec![
            DictEntry {
                surface: "今日".into(),
                cost: 3000,
                left_id: 0,
                right_id: 0,
            },
            DictEntry {
                surface: "京".into(),
                cost: 5000,
                left_id: 0,
                right_id: 0,
            },
        ];
        let reordered = h.reorder_candidates("きょう", &entries);
        assert_eq!(reordered[0].surface, "京");
    }

    #[test]
    fn test_no_boost_for_unrecorded() {
        let h = UserHistory::new();
        assert_eq!(h.unigram_boost("きょう", "今日"), 0);
        assert_eq!(h.bigram_boost("今日", "は", "は"), 0);
    }

    #[test]
    fn test_decay_recent() {
        // Just recorded → decay ≈ 1.0
        let now = now_epoch();
        let d = decay(now, now);
        assert!(
            (d - 1.0).abs() < 0.01,
            "recent decay should be ~1.0, got {d}"
        );
    }

    #[test]
    fn test_decay_one_week_old() {
        // 1 week (168 hours) ago → decay = 1/(1+1) = 0.5
        let now = now_epoch();
        let one_week_ago = now - 168 * 3600;
        let d = decay(one_week_ago, now);
        assert!(
            (d - 0.5).abs() < 0.01,
            "1-week decay should be ~0.5, got {d}"
        );
    }

    #[test]
    fn test_decay_very_old() {
        // Very old entry → decay approaches 0
        let now = now_epoch();
        let very_old = now.saturating_sub(365 * 24 * 3600);
        let d = decay(very_old, now);
        assert!(d < 0.02, "very old decay should be near 0, got {d}");
    }

    #[test]
    fn test_decay_future_timestamp() {
        // Future timestamp → saturating_sub yields 0 hours → decay = 1.0
        let now = now_epoch();
        let future = now + 3600;
        let d = decay(future, now);
        assert!(
            (d - 1.0).abs() < 0.001,
            "future decay should be 1.0, got {d}"
        );
    }

    #[test]
    fn test_decay_known_timestamps() {
        // Use fixed timestamps so the test is fully deterministic (no system clock).
        let now: u64 = 1_700_000_000; // arbitrary epoch value

        // 0 hours elapsed → decay = 1/(1+0/168) = 1.0
        assert!(
            (decay(now, now) - 1.0).abs() < 1e-9,
            "zero elapsed: expected 1.0"
        );

        // Exactly 1 half-life (168 h) elapsed → decay = 1/(1+1) = 0.5
        let one_hl = now - 168 * 3600;
        assert!(
            (decay(one_hl, now) - 0.5).abs() < 1e-9,
            "one half-life: expected 0.5"
        );

        // Exactly 2 half-lives (336 h) elapsed → decay = 1/(1+2) ≈ 0.333…
        let two_hl = now - 336 * 3600;
        let expected = 1.0 / 3.0;
        assert!(
            (decay(two_hl, now) - expected).abs() < 1e-9,
            "two half-lives: expected {expected}"
        );

        // 24 hours elapsed → decay = 1/(1+24/168) = 168/192 = 0.875
        let day_ago = now - 24 * 3600;
        let expected_day = 168.0 / 192.0;
        assert!(
            (decay(day_ago, now) - expected_day).abs() < 1e-9,
            "24h elapsed: expected {expected_day}"
        );

        // Future timestamp (last_used > now) → saturating_sub gives 0 → decay = 1.0
        let future = now + 9999;
        assert!(
            (decay(future, now) - 1.0).abs() < 1e-9,
            "future timestamp: expected 1.0"
        );
    }

    #[test]
    fn test_reorder_candidates_no_boost_preserves_order() {
        let h = UserHistory::new();
        let entries = vec![
            DictEntry {
                surface: "今日".into(),
                cost: 3000,
                left_id: 0,
                right_id: 0,
            },
            DictEntry {
                surface: "京".into(),
                cost: 5000,
                left_id: 0,
                right_id: 0,
            },
            DictEntry {
                surface: "教".into(),
                cost: 6000,
                left_id: 0,
                right_id: 0,
            },
        ];
        let reordered = h.reorder_candidates("きょう", &entries);
        // All boosts are 0 → original order preserved
        assert_eq!(reordered[0].surface, "今日");
        assert_eq!(reordered[1].surface, "京");
        assert_eq!(reordered[2].surface, "教");
    }

    #[test]
    fn test_save_to_invalid_path() {
        let h = UserHistory::new();
        let result = h.save(Path::new("/nonexistent/deeply/nested/dir/history.lxud"));
        // create_dir_all on a path under /nonexistent should fail on macOS
        assert!(result.is_err());
    }
}
