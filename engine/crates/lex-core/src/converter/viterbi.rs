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

/// A scored path from N-best Viterbi, carrying enough info for reranking.
#[derive(Debug, Clone)]
pub(crate) struct ScoredPath {
    pub segments: Vec<RichSegment>,
    pub viterbi_cost: i64,
}

impl ScoredPath {
    /// Create a single-segment path with no POS metadata (for rewriter-generated candidates).
    pub fn single(reading: String, surface: String, cost: i64) -> Self {
        Self {
            segments: vec![RichSegment {
                reading,
                surface,
                left_id: 0,
                right_id: 0,
                word_cost: 0,
            }],
            viterbi_cost: cost,
        }
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

    /// Surface key for deduplication.
    pub fn surface_key(&self) -> String {
        self.segments.iter().map(|s| s.surface.as_str()).collect()
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
pub(crate) fn viterbi_nbest(
    lattice: &Lattice,
    cost_fn: &dyn CostFunction,
    n: usize,
) -> Vec<ScoredPath> {
    let char_count = lattice.char_count;
    let _span = debug_span!("viterbi_nbest", n, char_count).entered();
    if char_count == 0 || n == 0 {
        return Vec::new();
    }

    let num_nodes = lattice.nodes.len();
    // top_k[node_idx] = sorted Vec of KEntry (ascending cost), max `n` entries
    let mut top_k: Vec<Vec<KEntry>> = vec![Vec::new(); num_nodes];

    // Initialize nodes starting at position 0 (BOS transition)
    for &idx in &lattice.nodes_by_start[0] {
        let node = &lattice.nodes[idx];
        let cost = cost_fn.word_cost(node) + cost_fn.bos_cost(node);
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
            let next_node = &lattice.nodes[next_idx];
            let word = cost_fn.word_cost(next_node);

            for &prev_idx in &lattice.nodes_by_end[pos] {
                if top_k[prev_idx].is_empty() {
                    continue;
                }
                let prev_node = &lattice.nodes[prev_idx];
                let transition = cost_fn.transition_cost(prev_node, next_node);

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
        let node = &lattice.nodes[node_idx];
        let eos = cost_fn.eos_cost(node);
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
        let scored = ScoredPath {
            segments,
            viterbi_cost: total_cost,
        };
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
        .map(|&idx| {
            let node = &lattice.nodes[idx];
            RichSegment {
                reading: node.reading.clone(),
                surface: node.surface.clone(),
                left_id: node.left_id,
                right_id: node.right_id,
                word_cost: node.cost,
            }
        })
        .collect()
}
