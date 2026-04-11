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

/// A string that is either already pooled (reuse existing span) or
/// new (will be appended to the pool on use).
#[derive(Clone, Copy)]
enum PooledStr<'a> {
    New(&'a str),
    Reuse(StringSpan),
}

/// The lattice: all possible segmentations of a kana string.
///
/// Stores node data in Structure-of-Arrays (SoA) layout for cache-friendly
/// Viterbi traversal, with a shared string pool for reading/surface strings
/// (zero per-node String allocation).
#[derive(Clone)]
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
    /// Longest reading (in chars) seen during lattice construction.
    /// Used by `extend` to bound the lookback window.
    max_reading_chars: usize,
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
            max_reading_chars: 0,
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
            max_reading_chars: 0,
        }
    }

    /// Append a node to the lattice.
    fn push_node(
        &mut self,
        pos: std::ops::Range<usize>,
        reading: PooledStr<'_>,
        surface: PooledStr<'_>,
        cost: i16,
        left_id: u16,
        right_id: u16,
    ) -> usize {
        let idx = self.costs.len();

        self.starts.push(pos.start);
        self.ends.push(pos.end);
        self.costs.push(cost);
        self.left_ids.push(left_id);
        self.right_ids.push(right_id);

        let r_span = self.resolve(reading);
        let s_span = self.resolve(surface);
        self.reading_spans.push(r_span);
        self.surface_spans.push(s_span);

        self.nodes_by_end[pos.end].push(idx);
        self.nodes_by_start[pos.start].push(idx);

        idx
    }

    /// Resolve a `PooledStr` to a `StringSpan`, appending to the pool if new.
    fn resolve(&mut self, s: PooledStr<'_>) -> StringSpan {
        match s {
            PooledStr::Reuse(span) => span,
            PooledStr::New(s) => self.pool_string(s),
        }
    }

    /// Append a string to the pool and return a reusable `PooledStr`.
    fn pool(&mut self, s: &str) -> PooledStr<'static> {
        PooledStr::Reuse(self.pool_string(s))
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
                start..end,
                PooledStr::New(reading),
                PooledStr::New(surface),
                cost,
                left_id,
                right_id,
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

    /// Extend the lattice with additional kana characters.
    ///
    /// `new_kana` must be an extension of `self.input` (i.e., start with the
    /// same characters). Only new nodes are added:
    /// - Existing positions near the boundary get new longer matches
    /// - New positions get full dictionary search + fallback nodes
    ///
    /// This avoids rebuilding the entire lattice on each keystroke.
    pub fn extend(&mut self, dict: &dyn Dictionary, new_kana: &str) {
        debug_assert!(
            new_kana.starts_with(&self.input),
            "extend: new_kana must start with current input"
        );
        if !new_kana.starts_with(&self.input) {
            // Precondition violated — rebuild from scratch instead of panicking.
            *self = build_lattice(dict, new_kana);
            return;
        }
        let old_char_count = self.char_count;
        let new_char_count = new_kana.chars().count();
        if new_char_count <= old_char_count {
            return;
        }

        let _span = debug_span!("lattice_extend", old_char_count, new_char_count).entered();

        let byte_offsets: Vec<usize> = new_kana.char_indices().map(|(i, _)| i).collect();

        // Update lattice metadata
        self.input = new_kana.to_string();
        self.char_count = new_char_count;
        self.nodes_by_start.resize_with(new_char_count, Vec::new);
        self.nodes_by_end.resize_with(new_char_count + 1, Vec::new);

        // New positions first: full search + fallback nodes.
        add_nodes_for_range(
            self,
            dict,
            new_kana,
            &byte_offsets,
            old_char_count,
            new_char_count,
            None,
        );

        // Existing positions near the boundary: find new longer matches
        // that extend into the appended suffix. Lookback is bounded by
        // the longest reading observed so far (tracked in add_nodes_for_range)
        // combined with the dictionary's declared max, keeping extend
        // O(max_word) per keystroke rather than O(input_length).
        let lookback = self.max_reading_chars.min(dict.max_reading_len());
        let lookback_start = old_char_count.saturating_sub(lookback);
        add_nodes_for_range(
            self,
            dict,
            new_kana,
            &byte_offsets,
            lookback_start,
            old_char_count,
            Some(old_char_count),
        );

        debug!(node_count = self.node_count());
    }

    /// Resolve a `StringSpan` to a `&str`.
    fn span_str(&self, span: &StringSpan) -> &str {
        let start = span.offset as usize;
        let end = start + span.len as usize;
        // SAFETY: we only append valid UTF-8 (&str bytes) to string_pool.
        unsafe { std::str::from_utf8_unchecked(&self.string_pool[start..end]) }
    }
}

/// Add nodes for dictionary matches at a range of character positions.
///
/// For each position in `start_pos..end_pos`, runs `common_prefix_search`
/// and adds matching nodes. If `min_end` is set, only nodes with
/// `end > min_end` are added (used by `extend` to skip already-known matches).
fn add_nodes_for_range(
    lattice: &mut Lattice,
    dict: &dyn Dictionary,
    kana: &str,
    byte_offsets: &[usize],
    start_pos: usize,
    end_pos: usize,
    min_end: Option<usize>,
) {
    for start in start_pos..end_pos {
        let mut has_single_char_match = min_end.is_some(); // existing positions already have fallbacks

        let suffix = &kana[byte_offsets[start]..];
        let matches = dict.common_prefix_search(suffix);

        for result in &matches {
            let reading_char_count = result.reading.chars().count();
            let end = start + reading_char_count;

            if let Some(min) = min_end {
                if end <= min {
                    // Already exists from previous build
                    if reading_char_count == 1 {
                        has_single_char_match = true;
                    }
                    continue;
                }
            }

            lattice.max_reading_chars = lattice.max_reading_chars.max(reading_char_count);
            let reading = lattice.pool(&result.reading);
            for entry in &result.entries {
                let surface = if entry.surface == result.reading {
                    reading
                } else {
                    PooledStr::New(&entry.surface)
                };
                lattice.push_node(
                    start..end,
                    reading,
                    surface,
                    entry.cost,
                    entry.left_id,
                    entry.right_id,
                );
            }
            if reading_char_count == 1 {
                has_single_char_match = true;
            }
        }

        if !has_single_char_match {
            let next_offset = byte_offsets.get(start + 1).copied().unwrap_or(kana.len());
            let ch = &kana[byte_offsets[start]..next_offset];
            // reading == surface for fallback — pool once, reuse for both
            let span = lattice.pool(ch);
            lattice.push_node(
                start..start + 1,
                span,
                span,
                settings().cost.unknown_word_cost,
                0,
                0,
            );
        }
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
    let byte_offsets: Vec<usize> = kana.char_indices().map(|(i, _)| i).collect();
    let mut lattice = Lattice::new(kana, char_count);
    lattice.max_reading_chars = dict.max_reading_len();

    add_nodes_for_range(&mut lattice, dict, kana, &byte_offsets, 0, char_count, None);

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

    // ── extend tests ──────────────────────────────────────────────

    /// Collect the full set of node tuples from a lattice for equivalence testing.
    fn node_set(
        lattice: &Lattice,
    ) -> std::collections::HashSet<(usize, usize, i16, u16, u16, String, String)> {
        (0..lattice.node_count())
            .map(|i| {
                (
                    lattice.start(i),
                    lattice.end(i),
                    lattice.cost(i),
                    lattice.left_id(i),
                    lattice.right_id(i),
                    lattice.reading(i).to_string(),
                    lattice.surface(i).to_string(),
                )
            })
            .collect()
    }

    #[test]
    fn test_extend_equivalence() {
        let dict = test_dict();

        // Build incrementally: "きょう" → "きょうは" → "きょうはいいてんき"
        let mut lattice = build_lattice(&dict, "きょう");
        lattice.extend(&dict, "きょうは");
        lattice.extend(&dict, "きょうはいいてんき");

        // Build from scratch
        let full = build_lattice(&dict, "きょうはいいてんき");

        // Same node set
        assert_eq!(node_set(&lattice), node_set(&full));
        assert_eq!(lattice.char_count, full.char_count);
    }

    #[test]
    fn test_extend_adds_longer_matches() {
        let dict = test_dict();

        let mut lattice = build_lattice(&dict, "きょう");
        let old_count = lattice.node_count();

        lattice.extend(&dict, "きょうは");

        // Should have new nodes at position 3 (は) and potentially longer matches
        assert!(
            lattice.node_count() > old_count,
            "extend should add nodes: {} -> {}",
            old_count,
            lattice.node_count()
        );
        assert_eq!(lattice.char_count, 4);

        // Verify connectivity: every position reachable
        for pos in 1..=lattice.char_count {
            assert!(
                !lattice.nodes_by_end[pos].is_empty(),
                "no nodes end at position {pos} after extend"
            );
        }
    }

    #[test]
    fn test_extend_noop() {
        let dict = test_dict();

        let mut lattice = build_lattice(&dict, "きょう");
        let count_before = lattice.node_count();

        lattice.extend(&dict, "きょう"); // same input
        assert_eq!(lattice.node_count(), count_before);
    }

    #[test]
    fn test_extend_multi_char() {
        let dict = test_dict();

        // Start with "き", extend by 2 chars to "きょう"
        let mut lattice = build_lattice(&dict, "き");
        lattice.extend(&dict, "きょう");

        let full = build_lattice(&dict, "きょう");
        assert_eq!(node_set(&lattice), node_set(&full));
    }

    #[test]
    fn test_extend_single_char_at_a_time() {
        let dict = test_dict();
        let input = "きょうはいいてんき";
        let chars: Vec<char> = input.chars().collect();

        // Build one char at a time
        let first: String = chars[..1].iter().collect();
        let mut lattice = build_lattice(&dict, &first);
        for i in 2..=chars.len() {
            let prefix: String = chars[..i].iter().collect();
            lattice.extend(&dict, &prefix);
        }

        // Should match full build
        let full = build_lattice(&dict, input);
        assert_eq!(node_set(&lattice), node_set(&full));
    }

    #[test]
    #[ignore]
    fn test_extend_real_dict_junbi() {
        use crate::dict::TrieDictionary;
        let dict_path = std::path::PathBuf::from(
            std::env::var("LEXIME_DICT").unwrap_or_else(|_| "data/lexime.dict".to_string()),
        );
        if !dict_path.exists() {
            eprintln!("skipping: dict not found at {:?}", dict_path);
            return;
        }
        let dict = TrieDictionary::open(&dict_path).unwrap();

        let input = "じゅんびをしましょうか";
        let chars: Vec<char> = input.chars().collect();

        // Build one char at a time via extend
        let first: String = chars[..1].iter().collect();
        let mut lattice = build_lattice(&dict, &first);
        for i in 2..=chars.len() {
            let prefix: String = chars[..i].iter().collect();
            lattice.extend(&dict, &prefix);
        }

        // Full build
        let full = build_lattice(&dict, input);

        let ext_set = node_set(&lattice);
        let full_set = node_set(&full);

        let missing: Vec<_> = full_set.difference(&ext_set).collect();
        let extra: Vec<_> = ext_set.difference(&full_set).collect();

        if !missing.is_empty() {
            eprintln!("MISSING from extend ({} nodes):", missing.len());
            for n in &missing {
                eprintln!(
                    "  [{},{}] {} -> {} cost={} L={} R={}",
                    n.0, n.1, n.5, n.6, n.2, n.3, n.4
                );
            }
        }
        if !extra.is_empty() {
            eprintln!("EXTRA in extend ({} nodes):", extra.len());
            for n in &extra {
                eprintln!(
                    "  [{},{}] {} -> {} cost={} L={} R={}",
                    n.0, n.1, n.5, n.6, n.2, n.3, n.4
                );
            }
        }

        assert_eq!(
            ext_set,
            full_set,
            "extend lattice differs from full build: {} missing, {} extra",
            missing.len(),
            extra.len()
        );
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
