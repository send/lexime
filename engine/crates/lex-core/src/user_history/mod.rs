//! User conversion history with time-decayed unigram/bigram boosting.
//!
//! Records confirmed conversions and uses frequency × recency scoring to
//! promote learned candidates in subsequent sessions.

mod persistence;
pub mod recovery;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_recovery;
pub mod wal;

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::dict::DictEntry;
use crate::settings::settings;

#[derive(Clone)]
pub struct UserHistory {
    /// reading → (surface → HistoryEntry)
    pub(super) unigrams: HashMap<String, HashMap<String, HistoryEntry>>,
    /// prev_surface → ((next_reading, next_surface) → HistoryEntry)
    pub(super) bigrams: HashMap<String, HashMap<(String, String), HistoryEntry>>,
    /// Highest WAL sequence number whose effect is reflected in this state.
    /// Stored in the v2 checkpoint header (not the bincode body); replay
    /// applies only frames with `seq > applied_seq`, which is what makes a
    /// checkpoint + leftover WAL crash state safe (no double application).
    /// 0 for `new()` and v1 checkpoints.
    applied_seq: u64,
    /// Entries the durable set may still hold that memory no longer does.
    /// Lives here rather than beside the engine's locks so a snapshot clone
    /// carries it: the covering rule then compares two values from the same
    /// consistent state instead of needing an ordering proof between an
    /// atomic and the clone (the reason `applied_seq` is a field too).
    /// Not part of the checkpoint body — `to_data`/`from_data` enumerate
    /// their fields explicitly, so this cannot reach the file format.
    durable_residue: DurableResidue,
}

/// Keys that may survive in the durable set (checkpoint, or WAL frames a
/// replay would re-apply) after memory dropped them.
///
/// Memory and the durable set diverge in this direction through exactly two
/// routes — capacity eviction, and a deletion whose durability was never
/// established — and both are settled by one event: a checkpoint that lands.
/// While a key is listed here, "absent from memory" no longer proves "absent
/// from disk", so the no-op-deletion skip must not fire for it.
///
/// Each entry carries the `epoch` at which it was raised. A plain set cannot
/// work: it is idempotent, so a key re-raised while a checkpoint is being
/// written is indistinguishable from the same key raised before it, and the
/// covering pass would drop a key the new checkpoint still holds.
#[derive(Clone, Default, Debug)]
pub struct DurableResidue {
    /// (reading, surface) → epoch raised
    unigrams: HashMap<(String, String), u64>,
    /// (prev_surface, next_reading, next_surface) → epoch raised
    bigrams: HashMap<(String, String, String), u64>,
    /// Monotonic; advances on every raise, including ones dropped while
    /// saturated. That is what lets `cover` tell a quiescent moment from one
    /// where untracked keys appeared.
    epoch: u64,
    /// Key tracking was abandoned past [`MAX_RESIDUE`]; every query answers
    /// "possible" until a checkpoint lands on a quiescent state.
    saturated: bool,
}

/// Cap on tracked residue keys. Reached only when checkpoints keep failing
/// (a healthy compaction clears the set), so the bound exists to keep a
/// failing disk from growing the set without limit — not as a steady-state
/// budget. Sized against the history's own capacity: past this the set is no
/// longer cheaper than being conservative.
const MAX_RESIDUE: usize = 10_000;

impl DurableResidue {
    fn raise_unigram(&mut self, reading: String, surface: String) {
        self.epoch += 1;
        if self.saturate_if_full() {
            return;
        }
        self.unigrams.insert((reading, surface), self.epoch);
    }

    fn raise_bigram(&mut self, prev: String, next_reading: String, next_surface: String) {
        self.epoch += 1;
        if self.saturate_if_full() {
            return;
        }
        self.bigrams
            .insert((prev, next_reading, next_surface), self.epoch);
    }

    /// Drop key tracking (and its memory) once the set stops being worth its
    /// size. Returns whether the residue is now saturated.
    fn saturate_if_full(&mut self) -> bool {
        if self.saturated {
            return true;
        }
        if self.unigrams.len() + self.bigrams.len() < MAX_RESIDUE {
            return false;
        }
        self.saturated = true;
        self.unigrams.clear();
        self.bigrams.clear();
        true
    }

    /// Whether these segments could still be in the durable set. Mirrors
    /// [`UserHistory::contains_entries`]'s shape so the two answer the same
    /// question about the same key space.
    fn contains_entries(&self, segments: &[(String, String)]) -> bool {
        if self.saturated {
            return true;
        }
        segments.iter().any(|(reading, surface)| {
            self.unigrams
                .contains_key(&(reading.clone(), surface.clone()))
        }) || segments.windows(2).any(|pair| {
            self.bigrams
                .contains_key(&(pair[0].1.clone(), pair[1].0.clone(), pair[1].1.clone()))
        })
    }

    /// Retire the keys a landed checkpoint accounts for.
    ///
    /// `snapshot` is this residue as it stood when the saved snapshot was
    /// cloned. A key is retired only if its epoch has not moved since: an
    /// unchanged epoch means the key was already absent from memory when the
    /// snapshot was taken, so the checkpoint written from that snapshot does
    /// not contain it either. A key re-raised during the write carries a
    /// newer epoch and stays — the checkpoint does hold that one.
    ///
    /// Saturation lifts only when the epoch has not moved at all, i.e. no
    /// raise (tracked or dropped) happened during the write. Otherwise keys
    /// may have been discarded untracked and the conservative answer stands.
    fn cover(&mut self, snapshot: &DurableResidue) {
        self.unigrams
            .retain(|k, e| snapshot.unigrams.get(k).is_none_or(|se| *e > *se));
        self.bigrams
            .retain(|k, e| snapshot.bigrams.get(k).is_none_or(|se| *e > *se));
        if self.saturated && self.epoch == snapshot.epoch {
            self.saturated = false;
        }
    }
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
pub(super) struct UserHistoryData {
    pub(super) unigrams: Vec<UnigramRecord>,
    pub(super) bigrams: Vec<BigramRecord>,
}

#[derive(Serialize, Deserialize)]
pub(super) struct UnigramRecord {
    pub(super) reading: String,
    pub(super) surface: String,
    pub(super) frequency: u32,
    pub(super) last_used: u64,
}

#[derive(Serialize, Deserialize)]
pub(super) struct BigramRecord {
    pub(super) prev_surface: String,
    pub(super) next_reading: String,
    pub(super) next_surface: String,
    pub(super) frequency: u32,
    pub(super) last_used: u64,
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
/// Returns the evicted keys: eviction is the one mutation that removes an
/// entry from memory without removing it from disk, so the caller has to
/// record them (see [`DurableResidue`]). Selection is unchanged — only the
/// reporting is new.
fn evict_map<K: Clone + Eq + std::hash::Hash>(
    map: &mut HashMap<String, HashMap<K, HistoryEntry>>,
    max: usize,
    now: u64,
) -> Vec<(String, K)> {
    let count: usize = map.values().map(|inner| inner.len()).sum();
    if count <= max {
        return Vec::new();
    }
    let mut all: Vec<(String, K, f64)> = Vec::with_capacity(count);
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
    let mut evicted = Vec::with_capacity(to_remove);
    for (outer_key, inner_key, _) in all[..to_remove].iter() {
        if let Some(inner) = map.get_mut(outer_key) {
            if inner.remove(inner_key).is_some() {
                evicted.push((outer_key.clone(), inner_key.clone()));
            }
            if inner.is_empty() {
                map.remove(outer_key);
            }
        }
    }
    evicted
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
            applied_seq: 0,
            durable_residue: DurableResidue::default(),
        }
    }

    /// Highest WAL sequence number reflected in this state.
    pub fn applied_seq(&self) -> u64 {
        self.applied_seq
    }

    /// Record that all WAL frames up to `seq` are reflected in this state.
    /// Monotonic: a lower `seq` never rolls the value back.
    pub fn advance_applied_seq(&mut self, seq: u64) {
        self.applied_seq = self.applied_seq.max(seq);
    }

    /// Apply one WAL record and advance `applied_seq`. Does not evict —
    /// replay applies many frames and evicts once afterwards.
    pub fn apply(&mut self, record: &wal::WalRecord, seq: u64) {
        self.apply_effect(record);
        self.advance_applied_seq(seq);
    }

    /// Apply a batch of records and settle capacity once (§5.2). A record's
    /// `seq` is `None` when its frame never reached the WAL (append
    /// failure): the effect still applies, but coverage must not advance —
    /// a checkpoint claiming a frame that cannot replay would let the
    /// conditional truncation destroy learning.
    pub fn apply_batch(&mut self, records: &[(wal::WalRecord, Option<u64>)]) {
        let mut any_committed = false;
        for (record, seq) in records {
            self.apply_effect(record);
            if let Some(seq) = seq {
                self.advance_applied_seq(*seq);
            }
            any_committed |= matches!(record, wal::WalRecord::Committed { .. });
        }
        // Deletions only shrink the maps; skip the O(entries) capacity scan
        // unless something grew.
        if any_committed {
            self.evict();
        }
    }

    fn apply_effect(&mut self, record: &wal::WalRecord) {
        match record {
            wal::WalRecord::Committed {
                segments,
                timestamp,
            } => self.record_segments_at(segments, *timestamp),
            wal::WalRecord::Tombstone { segments, .. } => {
                self.remove_entries(segments);
            }
        }
    }

    /// Record a confirmed conversion: list of (reading, surface) segments.
    pub fn record(&mut self, segments: &[(String, String)]) {
        self.record_at(segments, now_epoch());
    }

    /// Record with an explicit timestamp.
    pub fn record_at(&mut self, segments: &[(String, String)], now: u64) {
        self.record_segments_at(segments, now);
        self.evict();
    }

    /// Apply one recorded conversion without evicting (replay applies many
    /// frames and evicts once afterwards; the live path evicts per record).
    fn record_segments_at(&mut self, segments: &[(String, String)], now: u64) {
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
        results.sort_by_key(|b| std::cmp::Reverse(b.2));
        results
    }

    /// Return surfaces the user has previously confirmed for this reading,
    /// sorted by boost descending. Only returns entries with positive boost.
    pub fn learned_surfaces(&self, reading: &str, now: u64) -> Vec<(String, i64)> {
        let Some(inner) = self.unigrams.get(reading) else {
            return Vec::new();
        };
        let mut results: Vec<(String, i64)> = inner
            .iter()
            .map(|(surface, entry)| (surface.clone(), entry.boost(now)))
            .filter(|(_, boost)| *boost > 0)
            .collect();
        results.sort_by_key(|b| std::cmp::Reverse(b.1));
        results
    }

    /// Iterate all unigram records as (reading, surface, entry).
    /// Used by offline tooling (`lextool history-audit`) to mine the history.
    pub fn unigrams(&self) -> impl Iterator<Item = (&str, &str, &HistoryEntry)> {
        self.unigrams.iter().flat_map(|(reading, inner)| {
            inner
                .iter()
                .map(move |(surface, entry)| (reading.as_str(), surface.as_str(), entry))
        })
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

    /// Whether any unigram/bigram entry exists for these segments — exactly
    /// the set [`Self::remove_entries`] would touch (the equivalence is
    /// pinned by a test). Lets the engine skip writing a Tombstone (and its
    /// F_FULLFSYNC on the key-processing thread) for a no-op deletion, e.g.
    /// pruning a dictionary-only candidate that was never learned. The
    /// bigram probe scans keys linearly: the per-prev-surface maps are
    /// small, and it avoids allocating a `(String, String)` key inside the
    /// wal-mutex critical section.
    pub fn contains_entries(&self, segments: &[(String, String)]) -> bool {
        segments.iter().any(|(reading, surface)| {
            self.unigrams
                .get(reading)
                .is_some_and(|inner| inner.contains_key(surface))
        }) || segments.windows(2).any(|pair| {
            self.bigrams.get(&pair[0].1).is_some_and(|inner| {
                inner
                    .keys()
                    .any(|(r, s)| *r == pair[1].0 && *s == pair[1].1)
            })
        })
    }

    /// Remove history entries for the given segments.
    /// Returns true if any entries were actually removed.
    pub fn remove_entries(&mut self, segments: &[(String, String)]) -> bool {
        let mut removed = false;
        for (reading, surface) in segments {
            if let Some(inner) = self.unigrams.get_mut(reading) {
                if inner.remove(surface).is_some() {
                    removed = true;
                }
                if inner.is_empty() {
                    self.unigrams.remove(reading);
                }
            }
        }
        for pair in segments.windows(2) {
            let (_, prev_surface) = &pair[0];
            let (next_reading, next_surface) = &pair[1];
            let key = (next_reading.clone(), next_surface.clone());
            if let Some(inner) = self.bigrams.get_mut(prev_surface) {
                if inner.remove(&key).is_some() {
                    removed = true;
                }
                if inner.is_empty() {
                    self.bigrams.remove(prev_surface);
                }
            }
        }
        removed
    }

    /// Evict lowest-score entries when exceeding capacity. Batch paths
    /// ([`Self::apply_batch`], replay) apply many records and settle
    /// capacity once afterwards.
    ///
    /// Both maps are always processed — binding each call separately rather
    /// than combining them with `||`, which would short-circuit the bigram
    /// eviction whenever unigrams evicted and let `max_bigrams` go
    /// unenforced.
    pub fn evict(&mut self) {
        let s = settings();
        let now = now_epoch();
        let uni = evict_map(&mut self.unigrams, s.history.max_unigrams, now);
        let bi = evict_map(&mut self.bigrams, s.history.max_bigrams, now);
        for (reading, surface) in uni {
            self.durable_residue.raise_unigram(reading, surface);
        }
        for (prev, (next_reading, next_surface)) in bi {
            self.durable_residue
                .raise_bigram(prev, next_reading, next_surface);
        }
    }

    /// Whether these segments could still be in the durable set even though
    /// memory no longer holds them (see [`DurableResidue`]). The engine ORs
    /// this with [`Self::contains_entries`] before skipping a deletion as a
    /// no-op: memory-absent does not imply disk-absent.
    pub fn durable_residue_contains(&self, segments: &[(String, String)]) -> bool {
        self.durable_residue.contains_entries(segments)
    }

    /// Record that a deletion never became durable, so the entry it removed
    /// from memory may still be in the durable set with no tombstone to
    /// neutralise it.
    pub fn raise_durable_residue(&mut self, segments: &[(String, String)]) {
        for (reading, surface) in segments {
            self.durable_residue
                .raise_unigram(reading.clone(), surface.clone());
        }
        for pair in segments.windows(2) {
            self.durable_residue.raise_bigram(
                pair[0].1.clone(),
                pair[1].0.clone(),
                pair[1].1.clone(),
            );
        }
    }

    /// The residue as it stands, for a compaction to carry with its snapshot
    /// and hand back to [`Self::cover_durable_residue`].
    pub fn durable_residue(&self) -> &DurableResidue {
        &self.durable_residue
    }

    /// Retire the residue keys a landed checkpoint accounts for. `snapshot`
    /// must be the residue of the very state that was saved.
    pub fn cover_durable_residue(&mut self, snapshot: &DurableResidue) {
        self.durable_residue.cover(snapshot);
    }

    /// Drop the whole residue: the caller made the durable set empty
    /// (`clear`), so nothing can be left on disk that memory lacks.
    pub fn reset_durable_residue(&mut self) {
        self.durable_residue = DurableResidue::default();
    }
}
