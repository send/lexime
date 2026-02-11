use crate::dict::Dictionary;

/// A node in the conversion lattice.
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
pub struct Lattice {
    /// The original kana input
    pub input: String,
    /// All nodes in the lattice
    pub nodes: Vec<LatticeNode>,
    /// nodes_by_end[i] = indices of nodes that end at position i
    pub nodes_by_end: Vec<Vec<usize>>,
    /// nodes_by_start[i] = indices of nodes that start at position i
    pub nodes_by_start: Vec<Vec<usize>>,
    /// Number of characters in input
    pub char_count: usize,
}

const UNKNOWN_WORD_COST: i16 = 10000;

/// Build a lattice from a kana string using dictionary lookups.
///
/// Uses `common_prefix_search` for efficient trie traversal: a single trie walk
/// per starting position finds all matching prefixes, instead of O(n) individual
/// lookups per position.
/// Adds an unknown-word fallback node (1-char, high cost) to guarantee connectivity.
pub fn build_lattice(dict: &dyn Dictionary, kana: &str) -> Lattice {
    let chars: Vec<char> = kana.chars().collect();
    let char_count = chars.len();
    let mut nodes = Vec::new();
    // nodes_by_end has char_count + 1 slots (position 0 through char_count)
    let mut nodes_by_end: Vec<Vec<usize>> = vec![Vec::new(); char_count + 1];
    let mut nodes_by_start: Vec<Vec<usize>> = vec![Vec::new(); char_count];

    for start in 0..char_count {
        let mut has_single_char_match = false;

        let suffix: String = chars[start..].iter().collect();
        let matches = dict.common_prefix_search(&suffix);

        for result in &matches {
            let reading_char_count = result.reading.chars().count();
            let end = start + reading_char_count;
            for entry in &result.entries {
                let idx = nodes.len();
                nodes.push(LatticeNode {
                    start,
                    end,
                    reading: result.reading.clone(),
                    surface: entry.surface.clone(),
                    cost: entry.cost,
                    left_id: entry.left_id,
                    right_id: entry.right_id,
                });
                nodes_by_end[end].push(idx);
                nodes_by_start[start].push(idx);
                if reading_char_count == 1 {
                    has_single_char_match = true;
                }
            }
        }

        // Add a 1-char fallback node when no dictionary entry covers exactly
        // this single character. This guarantees connectivity: even positions
        // spanned only by longer matches remain reachable via the fallback.
        if !has_single_char_match {
            let ch: String = chars[start..start + 1].iter().collect();
            let idx = nodes.len();
            nodes.push(LatticeNode {
                start,
                end: start + 1,
                reading: ch.clone(),
                surface: ch,
                cost: UNKNOWN_WORD_COST,
                left_id: 0,
                right_id: 0,
            });
            nodes_by_end[start + 1].push(idx);
            nodes_by_start[start].push(idx);
        }
    }

    Lattice {
        input: kana.to_string(),
        nodes,
        nodes_by_end,
        nodes_by_start,
        char_count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dict::TrieDictionary;

    fn sample_dict() -> TrieDictionary {
        use crate::dict::DictEntry;
        let entries = vec![
            (
                "きょう".to_string(),
                vec![
                    DictEntry {
                        surface: "今日".to_string(),
                        cost: 3000,
                        left_id: 100,
                        right_id: 100,
                    },
                    DictEntry {
                        surface: "京".to_string(),
                        cost: 5000,
                        left_id: 100,
                        right_id: 100,
                    },
                ],
            ),
            (
                "は".to_string(),
                vec![DictEntry {
                    surface: "は".to_string(),
                    cost: 2000,
                    left_id: 200,
                    right_id: 200,
                }],
            ),
            (
                "いい".to_string(),
                vec![DictEntry {
                    surface: "良い".to_string(),
                    cost: 3500,
                    left_id: 300,
                    right_id: 300,
                }],
            ),
            (
                "てんき".to_string(),
                vec![DictEntry {
                    surface: "天気".to_string(),
                    cost: 4000,
                    left_id: 400,
                    right_id: 400,
                }],
            ),
            (
                "き".to_string(),
                vec![DictEntry {
                    surface: "木".to_string(),
                    cost: 4500,
                    left_id: 500,
                    right_id: 500,
                }],
            ),
            (
                "い".to_string(),
                vec![DictEntry {
                    surface: "胃".to_string(),
                    cost: 6000,
                    left_id: 600,
                    right_id: 600,
                }],
            ),
            (
                "てん".to_string(),
                vec![DictEntry {
                    surface: "天".to_string(),
                    cost: 5000,
                    left_id: 700,
                    right_id: 700,
                }],
            ),
        ];
        TrieDictionary::from_entries(entries)
    }

    #[test]
    fn test_build_lattice_basic() {
        let dict = sample_dict();
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
        let dict = sample_dict();
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
        let dict = sample_dict();
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
        let dict = sample_dict();
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
}
