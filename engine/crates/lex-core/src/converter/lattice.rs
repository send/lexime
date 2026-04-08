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

/// A node in the conversion lattice (legacy owned representation).
///
/// Kept alongside the SoA arrays during the migration period.
/// Will be removed once all consumers use Lattice accessors.
#[derive(Debug, Clone)]
pub struct LatticeNode {
    /// Start position (char index, inclusive)
    pub start: usize,
    /// End position (char index, exclusive)
    pub end: usize,
    /// Kana substring (reading)
    pub reading: String,
    /// Surface form (kanji, etc.)
    pub surface: String,
    /// Word cost (lower = more preferred)
    pub cost: i16,
    /// Left boundary morpheme ID
    pub left_id: u16,
    /// Right boundary morpheme ID
    pub right_id: u16,
}

/// The lattice: all possible segmentations of a kana string.
///
/// Internally stores node data in Structure-of-Arrays (SoA) layout for
/// cache-friendly Viterbi traversal, with a shared string pool for
/// reading/surface strings (zero per-node String allocation).
///
/// During the migration period, a legacy `nodes: Vec<LatticeNode>` is
/// maintained in parallel for backward compatibility.
pub struct Lattice {
    /// The original kana input
    pub input: String,

    // ── Legacy AoS (will be removed in a follow-up commit) ──────────
    /// All nodes in the lattice (legacy, redundant with SoA fields)
    pub nodes: Vec<LatticeNode>,

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
            nodes: Vec::new(),
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

    /// Create a lattice pre-allocated for the given input.
    fn with_capacity(input: &str, char_count: usize) -> Self {
        Self {
            input: input.to_string(),
            nodes: Vec::new(),
            starts: Vec::new(),
            ends: Vec::new(),
            costs: Vec::new(),
            left_ids: Vec::new(),
            right_ids: Vec::new(),
            string_pool: Vec::new(),
            reading_spans: Vec::new(),
            surface_spans: Vec::new(),
            nodes_by_end: vec![Vec::new(); char_count + 1],
            nodes_by_start: vec![Vec::new(); char_count],
            char_count,
        }
    }

    /// Append a node to the lattice, populating both SoA and legacy storage.
    ///
    /// `reading_span` allows reusing an already-pooled reading span (e.g.
    /// when multiple entries share the same reading within a SearchResult).
    /// Pass `None` to append the reading to the pool.
    #[allow(clippy::too_many_arguments)]
    fn push_node(
        &mut self,
        start: usize,
        end: usize,
        reading: &str,
        reading_span: Option<StringSpan>,
        surface: &str,
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
        let r_span = match reading_span {
            Some(span) => span,
            None => self.pool_string(reading),
        };
        self.reading_spans.push(r_span);

        let s_span = if surface.as_ptr() == reading.as_ptr() && surface.len() == reading.len() {
            // reading == surface (same slice) — reuse the reading span
            r_span
        } else {
            self.pool_string(surface)
        };
        self.surface_spans.push(s_span);

        // Index tables
        self.nodes_by_end[end].push(idx);
        self.nodes_by_start[start].push(idx);

        // Legacy AoS (temporary, removed in a follow-up commit)
        self.nodes.push(LatticeNode {
            start,
            end,
            reading: reading.to_string(),
            surface: surface.to_string(),
            cost,
            left_id,
            right_id,
        });

        idx
    }

    /// Append a string to the pool and return its span.
    fn pool_string(&mut self, s: &str) -> StringSpan {
        let offset = self.string_pool.len();
        let len = s.len();
        debug_assert!(offset <= u32::MAX as usize, "string pool offset overflow");
        debug_assert!(len <= u16::MAX as usize, "string length overflow");
        self.string_pool.extend_from_slice(s.as_bytes());
        StringSpan {
            offset: offset as u32,
            len: len as u16,
        }
    }

    // ── Test / migration helpers ───────────────────────────────────

    /// Build a Lattice from a legacy `Vec<LatticeNode>`, populating both
    /// the SoA arrays and the legacy `nodes` field.
    ///
    /// Used by test helpers during the migration period; will be removed
    /// once all tests use `push_node` directly.
    #[cfg(test)]
    pub(crate) fn from_nodes(input: &str, nodes: Vec<LatticeNode>) -> Self {
        let char_count = input.chars().count();
        let mut lattice = Self::with_capacity(input, char_count);
        for node in &nodes {
            lattice.push_node(
                node.start,
                node.end,
                &node.reading,
                None,
                &node.surface,
                node.cost,
                node.left_id,
                node.right_id,
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
    #[allow(dead_code)] // used in Commit 2
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
    let mut lattice = Lattice::with_capacity(kana, char_count);

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

        // Should have nodes for "きょう" (2 entries), "は" (1 entry), and "き" (1 entry)
        assert!(!lattice.nodes.is_empty());
        assert_eq!(lattice.char_count, 4); // き, ょ, う, は

        // Check that "きょう" nodes exist
        let kyou_nodes: Vec<_> = lattice
            .nodes
            .iter()
            .filter(|n| n.reading == "きょう")
            .collect();
        assert_eq!(kyou_nodes.len(), 2);
        assert!(kyou_nodes.iter().any(|n| n.surface == "今日"));
        assert!(kyou_nodes.iter().any(|n| n.surface == "京"));
    }

    #[test]
    fn test_unknown_word_fallback() {
        let dict = test_dict();
        // "zzz" is not in dictionary — each char gets an unknown node
        let lattice = build_lattice(&dict, "ぬ");

        assert!(!lattice.nodes.is_empty());
        let unknown = &lattice.nodes[0];
        assert_eq!(unknown.reading, "ぬ");
        assert_eq!(unknown.surface, "ぬ");
        assert_eq!(unknown.cost, 10000);
    }

    #[test]
    fn test_lattice_connectivity() {
        let dict = test_dict();
        let lattice = build_lattice(&dict, "きょうはいいてんき");

        // Every position should be reachable: nodes_by_end[i] should be non-empty
        // for all i in 1..=char_count
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

        // All nodes are correctly indexed in nodes_by_start and nodes_by_end
        for (idx, node) in lattice.nodes.iter().enumerate() {
            assert!(
                lattice.nodes_by_start[node.start].contains(&idx),
                "node {idx} not in nodes_by_start[{}]",
                node.start
            );
            assert!(
                lattice.nodes_by_end[node.end].contains(&idx),
                "node {idx} not in nodes_by_end[{}]",
                node.end
            );
        }

        // Reverse: indices in nodes_by_start point to nodes with correct start
        for (pos, indices) in lattice.nodes_by_start.iter().enumerate() {
            for &idx in indices {
                assert_eq!(
                    lattice.nodes[idx].start, pos,
                    "nodes_by_start[{pos}] contains node {idx} with start={}",
                    lattice.nodes[idx].start
                );
            }
        }

        // Reverse: indices in nodes_by_end point to nodes with correct end
        for (pos, indices) in lattice.nodes_by_end.iter().enumerate() {
            for &idx in indices {
                assert_eq!(
                    lattice.nodes[idx].end, pos,
                    "nodes_by_end[{pos}] contains node {idx} with end={}",
                    lattice.nodes[idx].end
                );
            }
        }
    }

    /// Verify that SoA accessors return identical values to legacy nodes.
    #[test]
    fn test_soa_accessors_match_legacy() {
        let dict = test_dict();
        let lattice = build_lattice(&dict, "きょうはいいてんき");

        assert_eq!(lattice.node_count(), lattice.nodes.len());
        for idx in 0..lattice.node_count() {
            let node = &lattice.nodes[idx];
            assert_eq!(lattice.start(idx), node.start, "start mismatch at {idx}");
            assert_eq!(lattice.end(idx), node.end, "end mismatch at {idx}");
            assert_eq!(lattice.cost(idx), node.cost, "cost mismatch at {idx}");
            assert_eq!(
                lattice.left_id(idx),
                node.left_id,
                "left_id mismatch at {idx}"
            );
            assert_eq!(
                lattice.right_id(idx),
                node.right_id,
                "right_id mismatch at {idx}"
            );
            assert_eq!(
                lattice.reading(idx),
                node.reading,
                "reading mismatch at {idx}"
            );
            assert_eq!(
                lattice.surface(idx),
                node.surface,
                "surface mismatch at {idx}"
            );
        }
    }

    /// Verify that to_rich_segment produces the same result as From<&LatticeNode>.
    #[test]
    fn test_to_rich_segment_matches_legacy() {
        let dict = test_dict();
        let lattice = build_lattice(&dict, "きょうはいいてんき");

        for idx in 0..lattice.node_count() {
            let from_soa = lattice.to_rich_segment(idx);
            let from_legacy = RichSegment::from(&lattice.nodes[idx]);
            assert_eq!(from_soa.reading, from_legacy.reading);
            assert_eq!(from_soa.surface, from_legacy.surface);
            assert_eq!(from_soa.left_id, from_legacy.left_id);
            assert_eq!(from_soa.right_id, from_legacy.right_id);
            assert_eq!(from_soa.word_cost, from_legacy.word_cost);
        }
    }
}
