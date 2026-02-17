use std::sync::OnceLock;

use lexime_trie::DoubleArray;

use super::config::{parse_romaji_toml, RomajiConfigError};
use super::table::DEFAULT_TOML;

static CUSTOM_TOML: OnceLock<String> = OnceLock::new();

#[derive(Debug, PartialEq)]
pub enum TrieLookupResult {
    None,
    Prefix,
    Exact(String),
    ExactAndPrefix(String),
}

pub struct RomajiTrie {
    da: DoubleArray<u8>,
    values: Vec<String>,
}

impl RomajiTrie {
    /// Set custom TOML before first `global()` call.
    pub fn init_custom(toml_content: String) -> Result<(), RomajiConfigError> {
        // Validate eagerly
        parse_romaji_toml(&toml_content)?;
        CUSTOM_TOML
            .set(toml_content)
            .map_err(|_| RomajiConfigError::AlreadyInitialized)
    }

    /// Get or initialize the global singleton.
    pub fn global() -> &'static RomajiTrie {
        static INSTANCE: OnceLock<RomajiTrie> = OnceLock::new();
        INSTANCE.get_or_init(|| {
            let toml_str = CUSTOM_TOML
                .get()
                .map(|s| s.as_str())
                .unwrap_or(DEFAULT_TOML);
            let map = parse_romaji_toml(toml_str).expect("romaji TOML must be valid");
            // BTreeMap is already sorted — DoubleArray build needs sorted keys
            let keys: Vec<&[u8]> = map.keys().map(|r| r.as_bytes()).collect();
            let values: Vec<String> = map.values().cloned().collect();
            let da = DoubleArray::<u8>::build(&keys);
            RomajiTrie { da, values }
        })
    }

    pub fn lookup(&self, romaji: &str) -> TrieLookupResult {
        let pr = self.da.probe(romaji.as_bytes());
        match (pr.value, pr.has_children) {
            (None, false) => TrieLookupResult::None,
            (None, true) => TrieLookupResult::Prefix,
            (Some(id), false) => TrieLookupResult::Exact(self.values[id as usize].clone()),
            (Some(id), true) => TrieLookupResult::ExactAndPrefix(self.values[id as usize].clone()),
        }
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
        let map = parse_romaji_toml(DEFAULT_TOML).unwrap();
        for (romaji, kana) in &map {
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
