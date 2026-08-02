//! User conversion history with time-decayed unigram/bigram boosting.
//!
//! Records confirmed conversions and uses frequency × recency scoring to
//! promote learned candidates in subsequent sessions.

pub mod deletion_marker;
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
use tracing::warn;

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
/// Within a process, memory and the durable set diverge in this direction
/// through two routes — capacity eviction, and a deletion whose frame never
/// reached the WAL — and both are settled by one event: a checkpoint that
/// lands. While a key is listed here, "absent from memory" no longer proves
/// "absent from disk", so the no-op-deletion skip must not fire for it.
///
/// A deletion whose frame *did* reach the WAL but whose flush failed
/// (`SyncFailed`) is deliberately not tracked: the frame is readable, so a
/// replay re-applies the deletion, and the synchronous checkpoint fallback
/// covers the flush. Only power loss can undo that, and it takes this
/// process-local set with it.
///
/// Each entry carries the `epoch` at which it was raised. A plain set cannot
/// work: it is idempotent, so a key re-raised while a checkpoint is being
/// written is indistinguishable from the same key raised before it, and the
/// covering pass would drop a key the new checkpoint still holds.
///
/// The maps nest exactly like [`UserHistory`]'s own, so probing them borrows
/// instead of building owned keys — this check runs on the key-processing
/// thread inside the wal mutex, which is why
/// [`UserHistory::contains_entries`] avoids allocating there too.
///
/// No `Debug`: the keys are raw readings and surfaces, and neither
/// [`UserHistory`] nor [`HistoryEntry`] derives it either — user input must
/// not become reachable through a `{:?}` in a log line.
#[derive(Clone, Default)]
struct DurableResidue {
    /// reading → (surface → epoch raised)
    unigrams: HashMap<String, HashMap<String, u64>>,
    /// prev_surface → (next_reading → (next_surface → epoch raised))
    ///
    /// One level deeper than [`UserHistory::bigrams`] on purpose. That map's
    /// linear scan over a prev-surface bucket is justified by the bucket
    /// being small — a surface has few successors in practice. The residue's
    /// buckets are not filled by successor distribution but by eviction,
    /// which picks the globally lowest-scoring entries and so piles up under
    /// whatever prev-surfaces are common (particles), bounded only by
    /// [`MAX_RESIDUE`]. Nesting keeps the probe an O(1) borrowed lookup on
    /// the key-processing thread instead of a scan whose length is set by
    /// how long the disk has been failing.
    bigrams: HashMap<String, HashMap<String, HashMap<String, u64>>>,
    /// Tracked keys across both maps, so the size check need not walk them.
    tracked: usize,
    /// Advances on every raise, including ones dropped while saturated —
    /// that is what lets `cover` tell a quiescent moment from one where
    /// untracked keys appeared.
    ///
    /// Rising within a residue, but NOT across `clear`: that installs a
    /// fresh `UserHistory`, so the counter restarts at 0. Comparing stamps
    /// from either side of a clear is therefore meaningless, and what keeps
    /// `cover` from doing so is `compact_gate` — the clear protocol parks on
    /// it, so a wipe cannot land inside a compaction's save window (the one
    /// stretch where `run_compact_impl` holds no lock at all).
    epoch: u64,
    /// Key tracking was abandoned past [`MAX_RESIDUE`]; every query answers
    /// "possible" until a checkpoint lands on a quiescent state.
    saturated: bool,
}

/// Cap on tracked residue keys.
///
/// This bounds memory, so it is a constant rather than a function of
/// `max_unigrams` / `max_bigrams`. Deriving it from those was tried and is
/// wrong: they are user-settable, the compaction that clears the residue
/// fires on a fixed WAL-frame threshold instead, and a configuration with
/// small caps would then saturate during ordinary typing on a healthy disk
/// — reinstating exactly the per-delete flush the per-key design avoids.
///
/// Sized so a single compaction interval cannot reach it (a commit raises a
/// handful of keys at most, and compaction runs every ~1000 frames), which
/// keeps saturation a symptom of checkpoints failing repeatedly, whatever
/// the configured capacity. At ~100 bytes per key the ceiling is a few MB,
/// held only while the disk is refusing writes.
const MAX_RESIDUE: usize = 50_000;

/// The bigram keys a segment list touches: for each adjacent pair, the
/// previous surface plus the next (reading, surface). Every bigram map in
/// this module is keyed this way, and one definition is what keeps the
/// history and its residue answering about the same key space.
fn bigram_keys(segments: &[(String, String)]) -> impl Iterator<Item = (&str, &str, &str)> {
    segments
        .windows(2)
        .map(|pair| (pair[0].1.as_str(), pair[1].0.as_str(), pair[1].1.as_str()))
}

impl DurableResidue {
    /// Raise every key a segment list touches (the deletion path, which
    /// works in segments rather than map keys).
    fn raise_segments(&mut self, segments: &[(String, String)]) {
        for (reading, surface) in segments {
            self.raise_unigram(reading.clone(), surface.clone());
        }
        for (prev, next_reading, next_surface) in bigram_keys(segments) {
            self.raise_bigram(
                prev.to_string(),
                next_reading.to_string(),
                next_surface.to_string(),
            );
        }
    }

    fn raise_unigram(&mut self, reading: String, surface: String) {
        let Some(epoch) = self.stamp() else { return };
        let added = self
            .unigrams
            .entry(reading)
            .or_default()
            .insert(surface, epoch)
            .is_none();
        self.account(added);
    }

    fn raise_bigram(&mut self, prev: String, next_reading: String, next_surface: String) {
        let Some(epoch) = self.stamp() else { return };
        let added = self
            .bigrams
            .entry(prev)
            .or_default()
            .entry(next_reading)
            .or_default()
            .insert(next_surface, epoch)
            .is_none();
        self.account(added);
    }

    /// Advance the epoch and report the stamp to record under, or `None`
    /// once tracking has been abandoned. The epoch advances either way: a
    /// raise that went untracked is exactly what must stop [`Self::cover`]
    /// from lifting saturation.
    fn stamp(&mut self) -> Option<u64> {
        self.epoch += 1;
        if self.saturated {
            return None;
        }
        Some(self.epoch)
    }

    /// Fold a completed insert into the size accounting, saturating once the
    /// set actually outgrows the cap.
    ///
    /// The check lives here, after the insert, rather than in [`Self::stamp`]
    /// before it: a re-raise of an already-tracked key cannot grow the set,
    /// and gating on raise *count* let one such duplicate at the cap throw
    /// away every tracked key — turning a precise residue into the blanket
    /// "everything might be on disk" answer for no gain.
    fn account(&mut self, added: bool) {
        if !added {
            return;
        }
        self.tracked += 1;
        if self.tracked <= MAX_RESIDUE {
            return;
        }
        warn!(
            "durable-residue tracking saturated at {MAX_RESIDUE} keys (checkpoints \
             are not landing); every deletion now pays a full flush until one does"
        );
        self.saturated = true;
        self.unigrams.clear();
        self.bigrams.clear();
        self.tracked = 0;
    }

    /// Whether these segments could still be in the durable set. Mirrors
    /// [`UserHistory::contains_entries`] over the same keys.
    fn contains_entries(&self, segments: &[(String, String)]) -> bool {
        if self.saturated {
            return true;
        }
        segments.iter().any(|(reading, surface)| {
            self.unigrams
                .get(reading)
                .is_some_and(|inner| inner.contains_key(surface))
        }) || bigram_keys(segments).any(|(prev, next_reading, next_surface)| {
            self.bigrams
                .get(prev)
                .and_then(|by_reading| by_reading.get(next_reading))
                .is_some_and(|by_surface| by_surface.contains_key(next_surface))
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
        let uni = retain_newer(&mut self.unigrams, &snapshot.unigrams);
        let bi = retain_newer_nested(&mut self.bigrams, &snapshot.bigrams);
        self.tracked = uni + bi;
        // Strict on purpose: an equal epoch is the only proof that nothing
        // was raised — and therefore silently dropped — while the checkpoint
        // was being written. Lifting after a busier window would re-arm the
        // precise answer without the keys to back it, which is the one way
        // this structure could under-report.
        if self.saturated && self.epoch == snapshot.epoch {
            self.saturated = false;
        }
    }
}

/// Drop every key whose stamp has not advanced past the one `snapshot` saw,
/// returning how many survived. The count comes from the same pass that does
/// the dropping — a second walk to recount runs under both the wal mutex and
/// the `inner` write lock, where the conversion path is waiting.
fn retain_newer(
    map: &mut HashMap<String, HashMap<String, u64>>,
    snapshot: &HashMap<String, HashMap<String, u64>>,
) -> usize {
    let mut kept = 0;
    map.retain(|outer, inner| {
        let covered = snapshot.get(outer);
        inner.retain(|k, epoch| {
            covered
                .and_then(|c| c.get(k))
                .is_none_or(|seen| *epoch > *seen)
        });
        kept += inner.len();
        !inner.is_empty()
    });
    kept
}

/// [`retain_newer`] for the residue's three-level bigram map.
fn retain_newer_nested(
    map: &mut HashMap<String, HashMap<String, HashMap<String, u64>>>,
    snapshot: &HashMap<String, HashMap<String, HashMap<String, u64>>>,
) -> usize {
    let mut kept = 0;
    map.retain(|outer, by_reading| {
        let covered = snapshot.get(outer);
        by_reading.retain(|reading, by_surface| {
            let covered = covered.and_then(|c| c.get(reading));
            by_surface.retain(|surface, epoch| {
                covered
                    .and_then(|c| c.get(surface))
                    .is_none_or(|seen| *epoch > *seen)
            });
            kept += by_surface.len();
            !by_surface.is_empty()
        });
        !by_reading.is_empty()
    });
    kept
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
    for (outer_key, inner_key, _) in all.drain(..to_remove) {
        let Some(inner) = map.get_mut(&outer_key) else {
            continue;
        };
        let removed = inner.remove(&inner_key).is_some();
        if inner.is_empty() {
            map.remove(&outer_key);
        }
        if removed {
            evicted.push((outer_key, inner_key));
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
            match seq {
                Some(seq) => self.advance_applied_seq(*seq),
                // No frame reached the WAL. For a deletion that means the
                // entry left memory while the durable set kept it, with no
                // tombstone to neutralise it — the same divergence eviction
                // creates, recorded from the same witness that already gates
                // `applied_seq`, so the two cannot drift apart.
                None => {
                    if let wal::WalRecord::Tombstone { segments, .. } = record {
                        self.durable_residue.raise_segments(segments);
                    }
                }
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
        for (prev_surface, next_reading, next_surface) in bigram_keys(segments) {
            let key = (next_reading.to_string(), next_surface.to_string());
            let entry = self
                .bigrams
                .entry(prev_surface.to_string())
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
    /// Whether this history holds nothing at all.
    ///
    /// Used by recovery to settle an unpersisted-deletion marker: a claim that
    /// some entry survived a deletion is false, not merely stale, when there
    /// is no entry. Mirrors the reasoning `clear` uses when it covers the
    /// ledger from its empty checkpoint.
    pub fn is_empty(&self) -> bool {
        self.unigrams.is_empty() && self.bigrams.is_empty()
    }

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
        }) || bigram_keys(segments).any(|(prev, next_reading, next_surface)| {
            self.bigrams.get(prev).is_some_and(|inner| {
                inner
                    .keys()
                    .any(|(r, s)| r == next_reading && s == next_surface)
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
        for (prev_surface, next_reading, next_surface) in bigram_keys(segments) {
            let key = (next_reading.to_string(), next_surface.to_string());
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
    /// Eviction is the one mutation that drops an entry from memory without
    /// dropping it from disk, so the evicted keys are recorded as residue
    /// here — inside the operation itself, where no caller can forget.
    ///
    /// Both maps are always processed — binding each call separately rather
    /// than combining them with `||`, which would short-circuit the bigram
    /// eviction whenever unigrams evicted and let `max_bigrams` go
    /// unenforced.
    fn evict(&mut self) {
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

    /// Whether a deletion of these segments could have any durable effect —
    /// the question the engine must ask before skipping one as a no-op.
    ///
    /// It is deliberately one predicate rather than two the caller ORs
    /// together: answering only the in-memory half is exactly the bug this
    /// exists to prevent (an entry evicted for capacity is gone from the
    /// maps while the last checkpoint still holds it).
    pub fn deletion_has_durable_target(&self, segments: &[(String, String)]) -> bool {
        self.contains_entries(segments) || self.durable_residue.contains_entries(segments)
    }

    /// Drop the residue wholesale, for a caller that just made the durable
    /// set equal to (or a superset of) this very state — a checkpoint
    /// written from `self`, or a wipe that emptied both sides. Unlike
    /// [`Self::cover_durable_residue`] there is no snapshot to compare
    /// against, because the saved state *is* this one.
    pub(super) fn reset_durable_residue(&mut self) {
        self.durable_residue = DurableResidue::default();
    }

    /// Retire the residue keys a landed checkpoint accounts for. `snapshot`
    /// must be the very state that was saved, so its residue is the one this
    /// checkpoint settles.
    pub fn cover_durable_residue(&mut self, snapshot: &UserHistory) {
        self.durable_residue.cover(&snapshot.durable_residue);
    }
}

/// Test support: the temporary path a durable checkpoint write goes through.
///
/// Exported so a fault-injection test in a dependent crate can put a directory
/// here and fail the checkpoint write — and only that write — leaving the rest
/// of the family (notably the unpersisted-deletion marker) writable and the
/// existing checkpoint readable, which is what a restart-crossing test needs.
///
/// It forwards to the definition `write_atomic` itself calls. Re-spelling the
/// convention here would have made the obstacle independent of the path the
/// writer actually uses, so a change inside `write_atomic` would leave the
/// injector planting a directory nobody visits and the tests passing
/// vacuously. No production caller; `persist` is crate-private, so this is the
/// seam. A proper injectable writer for checkpoints — the shape `WalIo` gives
/// the WAL — would retire both this and the older parent-as-a-file trick.
pub fn checkpoint_tmp_path(checkpoint_path: &std::path::Path) -> std::path::PathBuf {
    crate::persist::tmp_path(checkpoint_path)
}
