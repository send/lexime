use tracing::{debug, debug_span};

use super::cost::CostFunction;
use super::lattice::Lattice;

/// A segment in the conversion result.
#[derive(Debug, Clone)]
pub struct ConvertedSegment {
    /// The kana reading of this segment
    pub reading: String,
    /// The converted surface form (kanji, etc.)
    pub surface: String,
}

/// A segment with POS metadata, used internally for reranking.
#[derive(Debug, Clone)]
pub(crate) struct RichSegment {
    pub reading: String,
    pub surface: String,
    pub left_id: u16,
    pub right_id: u16,
    pub word_cost: i16,
}

/// Which stage produced a path's segments, or set its price.
///
/// `ScoredPath::origin` records who made the segments and
/// `ScoredPath::priced_by` who set the price. A price is the model's
/// (Viterbi + rerank), an offer's (a kana↔kanji variant of a listed path,
/// capped at its source + 2000 so the model's orthography preference can be
/// overridden), or a policy's (the kana rescue #263, Numeric #239,
/// Katakana — surfaces the lattice cannot represent).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PathOrigin {
    Viterbi,
    Resegment,
    KanjiVariant,
    HiraganaVariant,
    PartialHiragana,
    Numeric,
    Katakana,
}

impl PathOrigin {
    /// Every segment is a lattice node (applied to `origin`). A
    /// PartialHiragana path may carry a kanji node under its kana reading.
    pub fn is_lattice_path(self) -> bool {
        matches!(self, Self::Viterbi | Self::Resegment | Self::KanjiVariant)
    }

    /// Made by the cost model itself. As `priced_by`: a price that uses up
    /// the N-best budget (an offer's or a rescue's does not), and a path the
    /// variant rewriters may start from.
    pub fn is_model(self) -> bool {
        matches!(self, Self::Viterbi | Self::Resegment)
    }
}

/// A scored path from N-best Viterbi, carrying enough info for reranking.
#[derive(Debug, Clone)]
pub(crate) struct ScoredPath {
    pub segments: Vec<RichSegment>,
    pub viterbi_cost: i64,
    /// History boost subtracted from `viterbi_cost` by `apply_history_boost`
    /// (0 before history reranking, or when no history is applied).
    ///
    /// Kept so that steps running *after* history_rerank (cost-gap
    /// admission, Numeric / Katakana) recover the pre-boost cost via
    /// [`Self::pre_history_cost`] instead of pricing from a boosted one.
    pub history_boost: i64,
    /// The whole-path part of `history_boost`: > 0 marks a surface committed
    /// for this reading (`is_learned`), which cost-gap admission always
    /// keeps. Recorded by `apply_history_boost` — in history reranking, and
    /// in the Override stage for the paths it creates or reprices.
    pub whole_path_boost: i64,
    /// Who produced `segments`.
    pub origin: PathOrigin,
    /// Who set `viterbi_cost` (differs from `origin` when a duplicate's
    /// price was adopted by another stage).
    pub priced_by: PathOrigin,
}

impl ScoredPath {
    /// A path with no history applied, priced by the stage that made it.
    pub fn new(segments: Vec<RichSegment>, cost: i64, origin: PathOrigin) -> Self {
        Self {
            segments,
            viterbi_cost: cost,
            history_boost: 0,
            whole_path_boost: 0,
            origin,
            priced_by: origin,
        }
    }

    /// Create a single-segment path with no POS metadata (for rewriter-generated candidates).
    pub fn single(reading: String, surface: String, cost: i64, origin: PathOrigin) -> Self {
        Self::new(
            vec![RichSegment {
                reading,
                surface,
                left_id: 0,
                right_id: 0,
                word_cost: 0,
            }],
            cost,
            origin,
        )
    }

    /// The user has learned this surface for this reading
    /// (`whole_path_boost > 0`, recorded by history reranking).
    pub fn is_learned(&self) -> bool {
        self.whole_path_boost > 0
    }

    /// Every segment's surface is its reading: the user's typed input.
    pub fn is_identity(&self) -> bool {
        self.segments.iter().all(|s| s.surface == s.reading)
    }

    /// Cost before any history boost was applied.
    ///
    /// `history_rerank_at` subtracts the boost from `viterbi_cost`; adding it
    /// back recovers the intrinsic Viterbi/rerank cost. Candidate generators
    /// that derive a new path's cost from a base path should use this so the
    /// base's whole-path history boost does not leak into the derived surface.
    pub fn pre_history_cost(&self) -> i64 {
        self.viterbi_cost.saturating_add(self.history_boost)
    }

    /// Convert to public ConvertedSegment, dropping POS metadata.
    pub fn into_segments(self) -> Vec<ConvertedSegment> {
        self.segments
            .into_iter()
            .map(|s| ConvertedSegment {
                reading: s.reading,
                surface: s.surface,
            })
            .collect()
    }

    /// Concatenated reading of all segments.
    pub fn full_reading(&self) -> String {
        self.segments.iter().map(|s| s.reading.as_str()).collect()
    }

    /// Surface key for deduplication.
    pub fn surface_key(&self) -> String {
        self.segments.iter().map(|s| s.surface.as_str()).collect()
    }

    /// Compare surface key without allocating a String.
    pub fn surface_key_eq(&self, key: &str) -> bool {
        let mut remaining = key;
        for seg in &self.segments {
            if let Some(rest) = remaining.strip_prefix(seg.surface.as_str()) {
                remaining = rest;
            } else {
                return false;
            }
        }
        remaining.is_empty()
    }
}

/// A single entry in the top-K list for a node: (accumulated cost, previous node index, rank at
/// that node). `prev_rank` identifies which of the K paths at the previous node this entry
/// continues from.
#[derive(Clone, Copy)]
struct KEntry {
    cost: i64,
    prev_idx: Option<usize>,
    prev_rank: usize,
}

/// Run N-best Viterbi: keep top-K cost/backpointer pairs per node.
///
/// Returns up to `n` distinct `ScoredPath`s, sorted by Viterbi cost (best first).
/// Paths that produce identical surface strings are deduplicated.
///
/// Generic over `C: CostFunction` so that the cost function calls are
/// monomorphized and inlined in the hot forward-pass loop (no vtable dispatch).
pub(crate) fn viterbi_nbest<C: CostFunction>(
    lattice: &Lattice,
    cost_fn: &C,
    n: usize,
) -> Vec<ScoredPath> {
    let char_count = lattice.char_count;
    let _span = debug_span!("viterbi_nbest", n, char_count).entered();
    if char_count == 0 || n == 0 {
        return Vec::new();
    }

    let num_nodes = lattice.node_count();
    // top_k[node_idx] = sorted Vec of KEntry (ascending cost), max `n` entries
    let mut top_k: Vec<Vec<KEntry>> = vec![Vec::new(); num_nodes];

    // Initialize nodes starting at position 0 (BOS transition)
    for &idx in &lattice.nodes_by_start[0] {
        let cost = cost_fn.word_cost(lattice, idx) + cost_fn.bos_cost(lattice.left_id(idx));
        top_k[idx].push(KEntry {
            cost,
            prev_idx: None,
            prev_rank: 0,
        });
    }

    // Forward pass — next_idx loop is outermost so word_cost is computed
    // once per next_node (O(P)) instead of once per (prev, next) pair (O(P²)).
    for pos in 1..char_count {
        for &next_idx in &lattice.nodes_by_start[pos] {
            let word = cost_fn.word_cost(lattice, next_idx);
            let next_left_id = lattice.left_id(next_idx);

            for &prev_idx in &lattice.nodes_by_end[pos] {
                if top_k[prev_idx].is_empty() {
                    continue;
                }
                let prev_right_id = lattice.right_id(prev_idx);
                let transition = cost_fn.transition_cost(prev_right_id, next_left_id);

                for rank in 0..top_k[prev_idx].len() {
                    let prev_cost = top_k[prev_idx][rank].cost;
                    let total = prev_cost + transition + word;

                    insert_top_k(
                        &mut top_k[next_idx],
                        n,
                        KEntry {
                            cost: total,
                            prev_idx: Some(prev_idx),
                            prev_rank: rank,
                        },
                    );
                }
            }
        }
    }

    // Collect top-K at EOS
    let mut eos_entries: Vec<(i64, usize, usize)> = Vec::new(); // (total_cost, node_idx, rank)
    for &node_idx in &lattice.nodes_by_end[char_count] {
        let eos = cost_fn.eos_cost(lattice.right_id(node_idx));
        for (rank, entry) in top_k[node_idx].iter().enumerate() {
            let total = entry.cost + eos;
            eos_entries.push((total, node_idx, rank));
        }
    }
    eos_entries.sort_by_key(|&(cost, _, _)| cost);

    // Backtrace each path, deduplicate by surface string
    let mut results: Vec<ScoredPath> = Vec::new();
    let mut seen_surfaces: std::collections::HashSet<String> = std::collections::HashSet::new();

    for &(total_cost, end_idx, end_rank) in &eos_entries {
        if results.len() >= n {
            break;
        }
        let segments = backtrace_nbest(&top_k, end_idx, end_rank, lattice);
        let scored = ScoredPath::new(segments, total_cost, PathOrigin::Viterbi);
        if seen_surfaces.insert(scored.surface_key()) {
            results.push(scored);
        }
    }

    debug!(
        result_count = results.len(),
        best_cost = results.first().map(|p| p.viterbi_cost)
    );
    results
}

/// Insert a KEntry into a top-K list, maintaining ascending sort by cost and max size `k`.
///
/// `Vec::insert` is O(k) due to memmove, but k is small (30-50) and KEntry is 32 bytes,
/// so the shift fits in L1 cache. A BinaryHeap would give O(log k) insert but breaks
/// the stable-index invariant that `backtrace_nbest` relies on (`prev_rank` indexes
/// into the finalized Vec of a predecessor node).
fn insert_top_k(list: &mut Vec<KEntry>, k: usize, entry: KEntry) {
    // Find insertion point (binary search for ascending order)
    let pos = list.partition_point(|e| e.cost <= entry.cost);
    if pos >= k {
        return; // worse than all K existing entries
    }
    list.insert(pos, entry);
    if list.len() > k {
        list.pop();
    }
}

/// Backtrace from a specific (node_idx, rank) to reconstruct a path.
fn backtrace_nbest(
    top_k: &[Vec<KEntry>],
    end_idx: usize,
    end_rank: usize,
    lattice: &Lattice,
) -> Vec<RichSegment> {
    let mut path_indices = Vec::new();
    let mut cur_idx = end_idx;
    let mut cur_rank = end_rank;

    loop {
        path_indices.push(cur_idx);
        let entry = &top_k[cur_idx][cur_rank];
        match entry.prev_idx {
            Some(prev) => {
                cur_rank = entry.prev_rank;
                cur_idx = prev;
            }
            None => break,
        }
    }
    path_indices.reverse();

    path_indices
        .iter()
        .map(|&idx| lattice.to_rich_segment(idx))
        .collect()
}
