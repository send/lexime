//! Incremental lattice cache used by `InputSession`.
//!
//! The cache keeps a single `Arc<Lattice>` keyed by the current reading so
//! consecutive keystrokes can extend the existing lattice instead of rebuilding
//! from scratch. When the reading no longer matches (backspace, auto-commit,
//! prefix mismatch) the cache falls back to a fresh build.

use std::sync::Arc;

use lex_core::converter::{build_lattice, Lattice};
use lex_core::dict::Dictionary;

pub(crate) struct LatticeCache {
    lattice: Option<Arc<Lattice>>,
}

impl LatticeCache {
    pub(crate) fn new() -> Self {
        Self { lattice: None }
    }

    /// Drop any cached lattice (called on backspace, auto-commit, etc.).
    pub(crate) fn invalidate(&mut self) {
        self.lattice = None;
    }

    /// Return a lattice for `reading`, extending the cached one when possible.
    ///
    /// Reuses the cached lattice unchanged when `reading` matches, extends it
    /// when `reading` is a pure suffix append, and rebuilds otherwise.
    pub(crate) fn get_or_build(&mut self, reading: &str, dict: &dyn Dictionary) -> Arc<Lattice> {
        if let Some(arc) = self.lattice.take() {
            if reading == arc.input {
                self.lattice = Some(Arc::clone(&arc));
                return arc;
            }
            if reading.starts_with(&arc.input) {
                let mut owned = Arc::try_unwrap(arc).unwrap_or_else(|shared| (*shared).clone());
                owned.extend(dict, reading);
                let arc = Arc::new(owned);
                self.lattice = Some(Arc::clone(&arc));
                return arc;
            }
        }
        let arc = Arc::new(build_lattice(dict, reading));
        self.lattice = Some(Arc::clone(&arc));
        arc
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lex_core::dict::{DictEntry, TrieDictionary};

    fn test_dict() -> TrieDictionary {
        let entries = vec![
            (
                "きょう".to_string(),
                vec![DictEntry {
                    surface: "今日".to_string(),
                    cost: 3000,
                    left_id: 100,
                    right_id: 100,
                }],
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
                "てんき".to_string(),
                vec![DictEntry {
                    surface: "天気".to_string(),
                    cost: 4000,
                    left_id: 400,
                    right_id: 400,
                }],
            ),
        ];
        TrieDictionary::from_entries(entries)
    }

    #[test]
    fn first_call_builds_fresh_lattice() {
        let dict = test_dict();
        let mut cache = LatticeCache::new();
        let lattice = cache.get_or_build("きょう", &dict);
        assert_eq!(lattice.input, "きょう");
    }

    #[test]
    fn same_reading_returns_identical_arc() {
        let dict = test_dict();
        let mut cache = LatticeCache::new();
        let a = cache.get_or_build("きょう", &dict);
        let b = cache.get_or_build("きょう", &dict);
        assert!(
            Arc::ptr_eq(&a, &b),
            "expected cached Arc reuse when reading matches exactly"
        );
    }

    #[test]
    fn suffix_append_extends_cached_lattice() {
        let dict = test_dict();
        let mut cache = LatticeCache::new();
        let before = cache.get_or_build("きょう", &dict);
        drop(before);
        let extended = cache.get_or_build("きょうは", &dict);
        assert_eq!(extended.input, "きょうは");
    }

    #[test]
    fn prefix_mismatch_rebuilds_from_scratch() {
        let dict = test_dict();
        let mut cache = LatticeCache::new();
        let first = cache.get_or_build("きょう", &dict);
        let rebuilt = cache.get_or_build("てんき", &dict);
        assert_eq!(rebuilt.input, "てんき");
        assert!(
            !Arc::ptr_eq(&first, &rebuilt),
            "prefix mismatch should produce a fresh Arc, not reuse the old one"
        );
    }

    #[test]
    fn invalidate_forces_rebuild_on_same_reading() {
        let dict = test_dict();
        let mut cache = LatticeCache::new();
        let before = cache.get_or_build("きょう", &dict);
        cache.invalidate();
        let after = cache.get_or_build("きょう", &dict);
        assert_eq!(after.input, "きょう");
        assert!(
            !Arc::ptr_eq(&before, &after),
            "invalidate() followed by get_or_build should produce a fresh Arc"
        );
    }
}
