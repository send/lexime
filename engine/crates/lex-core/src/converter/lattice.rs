use tracing::{debug, debug_span};

use crate::dict::Dictionary;
use crate::settings::settings;

use super::viterbi::RichSegment;

/// Byte range within the Lattice string pool.
#[derive(Clone, Copy, Debug)]
struct StringSpan {
    offset: u32,
    len: u16,
}

/// The lattice: all possible segmentations of a kana string.
///
/// Stores node data in Structure-of-Arrays (SoA) layout for cache-friendly
/// Viterbi traversal, with a shared string pool for reading/surface strings
/// (zero per-node String allocation).
pub struct Lattice {
    /// The original kana input
    pub input: String,

    // ── SoA numeric fields (hot during Viterbi forward pass) ────────
    starts: Vec<usize>,
    ends: Vec<usize>,
    costs: Vec<i16>,
    left_ids: Vec<u16>,
    right_ids: Vec<u16>,

    // ── String pool (cold — accessed during backtrace/postprocess) ──
    string_pool: Vec<u8>,
    reading_spans: Vec<StringSpan>,
    surface_spans: Vec<StringSpan>,

    // ── Index tables ────────────────────────────────────────────────
    /// nodes_by_end[i] = indices of nodes that end at position i
    pub nodes_by_end: Vec<Vec<usize>>,
    /// nodes_by_start[i] = indices of nodes that start at position i
    pub nodes_by_start: Vec<Vec<usize>>,
    /// Number of characters in input
    pub char_count: usize,
}

impl Lattice {
    /// An empty lattice (no input, no nodes).
    ///
    /// Maintains the invariant `nodes_by_end.len() == char_count + 1` so
    /// that downstream code (e.g. `viterbi_nbest`) can safely index into it.
    pub fn empty() -> Self {
        Self {
            input: String::new(),
            starts: Vec::new(),
            ends: Vec::new(),
            costs: Vec::new(),
            left_ids: Vec::new(),
            right_ids: Vec::new(),
            string_pool: Vec::new(),
            reading_spans: Vec::new(),
            surface_spans: Vec::new(),
            nodes_by_end: vec![Vec::new()],
            nodes_by_start: Vec::new(),
            char_count: 0,
        }
    }

    /// Create a lattice for the given input with pre-allocated SoA storage.
    ///
    /// Estimates ~3 nodes per character (typical dictionary density) and
    /// ~10 bytes of string pool per node.
    fn new(input: &str, char_count: usize) -> Self {
        let est_nodes = char_count * 3;
        let est_pool = est_nodes * 10;
        Self {
            input: input.to_string(),
            starts: Vec::with_capacity(est_nodes),
            ends: Vec::with_capacity(est_nodes),
            costs: Vec::with_capacity(est_nodes),
            left_ids: Vec::with_capacity(est_nodes),
            right_ids: Vec::with_capacity(est_nodes),
            string_pool: Vec::with_capacity(est_pool),
            reading_spans: Vec::with_capacity(est_nodes),
            surface_spans: Vec::with_capacity(est_nodes),
            nodes_by_end: vec![Vec::new(); char_count + 1],
            nodes_by_start: vec![Vec::new(); char_count],
            char_count,
        }
    }

    /// Append a node to the lattice.
    ///
    /// `reading_span` / `surface_span` allow reusing already-pooled spans
    /// (e.g. shared reading within a SearchResult, or reading == surface
    /// for fallback nodes). Pass `None` to append the string to the pool.
    #[allow(clippy::too_many_arguments)]
    fn push_node(
        &mut self,
        start: usize,
        end: usize,
        reading: &str,
        reading_span: Option<StringSpan>,
        surface: &str,
        surface_span: Option<StringSpan>,
        cost: i16,
        left_id: u16,
        right_id: u16,
    ) -> usize {
        let idx = self.costs.len();

        // SoA numeric
        self.starts.push(start);
        self.ends.push(end);
        self.costs.push(cost);
        self.left_ids.push(left_id);
        self.right_ids.push(right_id);

        // String pool
        let r_span = reading_span.unwrap_or_else(|| self.pool_string(reading));
        self.reading_spans.push(r_span);

        let s_span = surface_span.unwrap_or_else(|| self.pool_string(surface));
        self.surface_spans.push(s_span);

        // Index tables
        self.nodes_by_end[end].push(idx);
        self.nodes_by_start[start].push(idx);

        idx
    }

    /// Append a string to the pool and return its span.
    fn pool_string(&mut self, s: &str) -> StringSpan {
        let offset = u32::try_from(self.string_pool.len()).expect("string pool offset overflow");
        let len = u16::try_from(s.len()).expect("string length overflow");
        self.string_pool.extend_from_slice(s.as_bytes());
        StringSpan { offset, len }
    }

    // ── Test helpers ──────────────────────────────────────────────

    /// Build a Lattice from a list of (start, end, reading, surface, cost,
    /// left_id, right_id) tuples.
    #[cfg(test)]
    pub(crate) fn from_test_nodes(
        input: &str,
        nodes: &[(usize, usize, &str, &str, i16, u16, u16)],
    ) -> Self {
        let char_count = input.chars().count();
        let mut lattice = Self::new(input, char_count);
        for &(start, end, reading, surface, cost, left_id, right_id) in nodes {
            lattice.push_node(
                start, end, reading, None, surface, None, cost, left_id, right_id,
            );
        }
        lattice
    }

    // ── Accessors ───────────────────────────────────────────────────

    /// Number of nodes in the lattice.
    pub fn node_count(&self) -> usize {
        self.costs.len()
    }

    /// Start position (char index, inclusive) of node `idx`.
    pub fn start(&self, idx: usize) -> usize {
        self.starts[idx]
    }

    /// End position (char index, exclusive) of node `idx`.
    pub fn end(&self, idx: usize) -> usize {
        self.ends[idx]
    }

    /// Word cost of node `idx`.
    pub fn cost(&self, idx: usize) -> i16 {
        self.costs[idx]
    }

    /// Left boundary morpheme ID of node `idx`.
    pub fn left_id(&self, idx: usize) -> u16 {
        self.left_ids[idx]
    }

    /// Right boundary morpheme ID of node `idx`.
    pub fn right_id(&self, idx: usize) -> u16 {
        self.right_ids[idx]
    }

    /// Reading (kana) of node `idx` — zero-copy from the string pool.
    pub fn reading(&self, idx: usize) -> &str {
        self.span_str(&self.reading_spans[idx])
    }

    /// Surface form of node `idx` — zero-copy from the string pool.
    pub fn surface(&self, idx: usize) -> &str {
        self.span_str(&self.surface_spans[idx])
    }

    /// Build a `RichSegment` from node `idx` (allocates owned Strings).
    pub(crate) fn to_rich_segment(&self, idx: usize) -> RichSegment {
        RichSegment {
            reading: self.reading(idx).to_string(),
            surface: self.surface(idx).to_string(),
            left_id: self.left_id(idx),
            right_id: self.right_id(idx),
            word_cost: self.cost(idx),
        }
    }

    /// Resolve a `StringSpan` to a `&str`.
    fn span_str(&self, span: &StringSpan) -> &str {
        let start = span.offset as usize;
        let end = start + span.len as usize;
        // SAFETY: we only append valid UTF-8 (&str bytes) to string_pool.
        unsafe { std::str::from_utf8_unchecked(&self.string_pool[start..end]) }
    }
}

/// Build a lattice from a kana string using dictionary lookups.
///
/// Uses `common_prefix_search` for efficient trie traversal: a single trie walk
/// per starting position finds all matching prefixes, instead of O(n) individual
/// lookups per position.
/// Adds an unknown-word fallback node (1-char, high cost) to guarantee connectivity.
pub fn build_lattice(dict: &dyn Dictionary, kana: &str) -> Lattice {
    let char_count = kana.chars().count();
    let _span = debug_span!("build_lattice", char_count).entered();
    // Pre-compute byte offsets for each char position so we can slice
    // the original &str directly instead of allocating a new String per position.
    let byte_offsets: Vec<usize> = kana.char_indices().map(|(i, _)| i).collect();
    let mut lattice = Lattice::new(kana, char_count);

    for start in 0..char_count {
        let mut has_single_char_match = false;

        let suffix = &kana[byte_offsets[start]..];
        let matches = dict.common_prefix_search(suffix);

        for result in &matches {
            let reading_char_count = result.reading.chars().count();
            let end = start + reading_char_count;
            // Pool the reading once and reuse the span for all entries
            let reading_span = lattice.pool_string(&result.reading);
            for entry in &result.entries {
                lattice.push_node(
                    start,
                    end,
                    &result.reading,
                    Some(reading_span),
                    &entry.surface,
                    None,
                    entry.cost,
                    entry.left_id,
                    entry.right_id,
                );
                if reading_char_count == 1 {
                    has_single_char_match = true;
                }
            }
        }

        // Add a 1-char fallback node when no dictionary entry covers exactly
        // this single character. This guarantees connectivity: even positions
        // spanned only by longer matches remain reachable via the fallback.
        if !has_single_char_match {
            let next_offset = byte_offsets.get(start + 1).copied().unwrap_or(kana.len());
            let ch = &kana[byte_offsets[start]..next_offset];
            // reading == surface for fallback — pool once, reuse for both
            let span = lattice.pool_string(ch);
            lattice.push_node(
                start,
                start + 1,
                ch,
                Some(span),
                ch,
                Some(span),
                settings().cost.unknown_word_cost,
                0,
                0,
            );
        }
    }

    debug!(node_count = lattice.node_count());
    lattice
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::converter::testutil::test_dict;

    #[test]
    fn test_build_lattice_basic() {
        let dict = test_dict();
        let lattice = build_lattice(&dict, "きょうは");

        assert!(lattice.node_count() > 0);
        assert_eq!(lattice.char_count, 4); // き, ょ, う, は

        // Check that "きょう" nodes exist
        let kyou_indices: Vec<usize> = (0..lattice.node_count())
            .filter(|&i| lattice.reading(i) == "きょう")
            .collect();
        assert_eq!(kyou_indices.len(), 2);
        assert!(kyou_indices.iter().any(|&i| lattice.surface(i) == "今日"));
        assert!(kyou_indices.iter().any(|&i| lattice.surface(i) == "京"));
    }

    #[test]
    fn test_unknown_word_fallback() {
        let dict = test_dict();
        let lattice = build_lattice(&dict, "ぬ");

        assert!(lattice.node_count() > 0);
        assert_eq!(lattice.reading(0), "ぬ");
        assert_eq!(lattice.surface(0), "ぬ");
        assert_eq!(lattice.cost(0), 10000);
    }

    #[test]
    fn test_lattice_connectivity() {
        let dict = test_dict();
        let lattice = build_lattice(&dict, "きょうはいいてんき");

        for pos in 1..=lattice.char_count {
            assert!(
                !lattice.nodes_by_end[pos].is_empty(),
                "no nodes end at position {pos}"
            );
        }
    }

    #[test]
    fn test_nodes_by_start_end_consistency() {
        let dict = test_dict();
        let lattice = build_lattice(&dict, "きょうはいいてんき");

        for idx in 0..lattice.node_count() {
            assert!(
                lattice.nodes_by_start[lattice.start(idx)].contains(&idx),
                "node {idx} not in nodes_by_start[{}]",
                lattice.start(idx)
            );
            assert!(
                lattice.nodes_by_end[lattice.end(idx)].contains(&idx),
                "node {idx} not in nodes_by_end[{}]",
                lattice.end(idx)
            );
        }

        for (pos, indices) in lattice.nodes_by_start.iter().enumerate() {
            for &idx in indices {
                assert_eq!(
                    lattice.start(idx),
                    pos,
                    "nodes_by_start[{pos}] contains node {idx} with start={}",
                    lattice.start(idx)
                );
            }
        }

        for (pos, indices) in lattice.nodes_by_end.iter().enumerate() {
            for &idx in indices {
                assert_eq!(
                    lattice.end(idx),
                    pos,
                    "nodes_by_end[{pos}] contains node {idx} with end={}",
                    lattice.end(idx)
                );
            }
        }
    }

    #[test]
    fn test_to_rich_segment() {
        let dict = test_dict();
        let lattice = build_lattice(&dict, "きょうはいいてんき");

        for idx in 0..lattice.node_count() {
            let seg = lattice.to_rich_segment(idx);
            assert_eq!(seg.reading, lattice.reading(idx));
            assert_eq!(seg.surface, lattice.surface(idx));
            assert_eq!(seg.left_id, lattice.left_id(idx));
            assert_eq!(seg.right_id, lattice.right_id(idx));
            assert_eq!(seg.word_cost, lattice.cost(idx));
        }
    }

    #[test]
    fn test_string_pool_reading_dedup() {
        let dict = test_dict();
        let lattice = build_lattice(&dict, "きょう");

        // "きょう" has 2 entries (今日, 京) — they should share the same reading span
        let kyou_indices: Vec<usize> = (0..lattice.node_count())
            .filter(|&i| lattice.reading(i) == "きょう")
            .collect();
        assert!(kyou_indices.len() >= 2);

        // Reading spans should point to the same pool region
        let span0 = lattice.reading_spans[kyou_indices[0]];
        let span1 = lattice.reading_spans[kyou_indices[1]];
        assert_eq!(span0.offset, span1.offset);
        assert_eq!(span0.len, span1.len);
    }
}
