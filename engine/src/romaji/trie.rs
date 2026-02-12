use std::collections::HashMap;
use std::sync::OnceLock;

use super::table::MAPPINGS;

#[derive(Debug, PartialEq)]
pub enum TrieLookupResult {
    None,
    Prefix,
    Exact(String),
    ExactAndPrefix(String),
}

struct Node {
    children: HashMap<u8, Node>,
    kana: Option<String>,
}

impl Node {
    fn new() -> Self {
        Self {
            children: HashMap::new(),
            kana: None,
        }
    }
}

pub struct RomajiTrie {
    root: Node,
}

impl RomajiTrie {
    /// Get or initialize the global singleton.
    pub fn global() -> &'static RomajiTrie {
        static INSTANCE: OnceLock<RomajiTrie> = OnceLock::new();
        INSTANCE.get_or_init(|| {
            let mut trie = RomajiTrie { root: Node::new() };
            for &(romaji, kana) in MAPPINGS {
                trie.insert(romaji, kana);
            }
            trie
        })
    }

    pub fn lookup(&self, romaji: &str) -> TrieLookupResult {
        let mut node = &self.root;
        for &b in romaji.as_bytes() {
            match node.children.get(&b) {
                Some(child) => node = child,
                None => return TrieLookupResult::None,
            }
        }
        let has_children = !node.children.is_empty();
        match &node.kana {
            Some(kana) => {
                if has_children {
                    TrieLookupResult::ExactAndPrefix(kana.clone())
                } else {
                    TrieLookupResult::Exact(kana.clone())
                }
            }
            None => {
                if has_children {
                    TrieLookupResult::Prefix
                } else {
                    TrieLookupResult::None
                }
            }
        }
    }

    fn insert(&mut self, romaji: &str, kana: &str) {
        let mut node = &mut self.root;
        for &b in romaji.as_bytes() {
            node = node.children.entry(b).or_insert_with(Node::new);
        }
        node.kana = Some(kana.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vowel_exact() {
        let trie = RomajiTrie::global();
        assert_eq!(trie.lookup("a"), TrieLookupResult::Exact("あ".into()));
    }

    #[test]
    fn test_prefix_k() {
        let trie = RomajiTrie::global();
        assert_eq!(trie.lookup("k"), TrieLookupResult::Prefix);
    }

    #[test]
    fn test_prefix_q() {
        let trie = RomajiTrie::global();
        assert_eq!(trie.lookup("q"), TrieLookupResult::Prefix);
    }

    #[test]
    fn test_symbol_hyphen() {
        let trie = RomajiTrie::global();
        assert_eq!(trie.lookup("-"), TrieLookupResult::Exact("ー".into()));
    }

    #[test]
    fn test_youon_sha() {
        let trie = RomajiTrie::global();
        assert_eq!(trie.lookup("sha"), TrieLookupResult::Exact("しゃ".into()));
    }

    #[test]
    fn test_chi_exact_or_prefix() {
        let trie = RomajiTrie::global();
        // "chi" matches ち and is also a prefix for "cho", "cha", etc.
        match trie.lookup("chi") {
            TrieLookupResult::Exact(ref k) | TrieLookupResult::ExactAndPrefix(ref k) => {
                assert_eq!(k, "ち");
            }
            other => panic!("expected Exact or ExactAndPrefix, got {:?}", other),
        }
    }

    #[test]
    fn test_ka_exact() {
        let trie = RomajiTrie::global();
        assert_eq!(trie.lookup("ka"), TrieLookupResult::Exact("か".into()));
    }

    #[test]
    fn test_sh_prefix() {
        let trie = RomajiTrie::global();
        assert_eq!(trie.lookup("sh"), TrieLookupResult::Prefix);
    }

    #[test]
    fn test_nn_exact() {
        let trie = RomajiTrie::global();
        assert_eq!(trie.lookup("nn"), TrieLookupResult::Exact("ん".into()));
    }

    #[test]
    fn test_punctuation() {
        let trie = RomajiTrie::global();
        assert_eq!(trie.lookup("."), TrieLookupResult::Exact("。".into()));
        assert_eq!(trie.lookup(","), TrieLookupResult::Exact("、".into()));
        assert_eq!(trie.lookup("?"), TrieLookupResult::Exact("？".into()));
    }

    #[test]
    fn test_z_sequences() {
        let trie = RomajiTrie::global();
        assert_eq!(trie.lookup("zh"), TrieLookupResult::Exact("←".into()));
        assert_eq!(trie.lookup("zj"), TrieLookupResult::Exact("↓".into()));
        assert_eq!(trie.lookup("z."), TrieLookupResult::Exact("…".into()));
    }

    #[test]
    fn test_none_for_unknown() {
        let trie = RomajiTrie::global();
        assert_eq!(trie.lookup("xyz"), TrieLookupResult::None);
    }

    #[test]
    fn test_all_mappings_roundtrip() {
        let trie = RomajiTrie::global();
        for &(romaji, kana) in MAPPINGS {
            match trie.lookup(romaji) {
                TrieLookupResult::Exact(ref k) | TrieLookupResult::ExactAndPrefix(ref k) => {
                    assert_eq!(k, kana, "mapping mismatch for romaji={romaji}");
                }
                other => panic!(
                    "expected Exact/ExactAndPrefix for {romaji}, got {:?}",
                    other
                ),
            }
        }
    }
}
