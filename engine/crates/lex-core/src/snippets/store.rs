use std::collections::HashMap;

use super::config::SnippetConfigError;
use super::variables::VariableResolver;

pub struct SnippetStore {
    entries: HashMap<String, String>,
    resolver: VariableResolver,
}

impl SnippetStore {
    /// Build a store, rejecting empty keys.
    ///
    /// An empty key sorts first and, with an empty filter, would make the
    /// snippet picker render empty marked text for a live selection —
    /// reintroducing the confirming-key leak on Chromium/Electron hosts. Making
    /// the constructor fallible keeps the "every key is non-empty" invariant
    /// with the data, so `SnippetState::display_text` cannot observe an empty
    /// key (see its doc).
    pub fn new(
        entries: HashMap<String, String>,
        resolver: VariableResolver,
    ) -> Result<Self, SnippetConfigError> {
        if entries.keys().any(|k| k.is_empty()) {
            return Err(SnippetConfigError::EmptyKey);
        }
        Ok(Self { entries, resolver })
    }

    /// Return the usable entries matching the given prefix, with variables
    /// expanded. Results are sorted by key for stable ordering.
    ///
    /// Entries whose body expands to nothing are dropped: confirming one would
    /// insert an empty string, and a `KeyResponse::commit` is contractually
    /// non-empty. Filtering happens here, on the expanded value, because
    /// expansion is not a fixed property of the config — `SnippetVariable::Date`
    /// resolves against the clock, so a verdict reached when the store was built
    /// can go stale. Dropping at the point of use means callers never have to
    /// handle an unusable entry, and the existing "no matches" paths already
    /// cover a store with nothing usable in it.
    ///
    /// The common case — a body written empty, or one that is only a static
    /// variable with an empty value — *is* decidable when the store is built, and
    /// `snippets_build_store` reports it there so it does not vanish silently.
    /// This filter is the structural guarantee, not the diagnostic.
    pub fn prefix_search(&self, prefix: &str) -> Vec<(String, String)> {
        let mut results: Vec<(String, String)> = self
            .entries
            .iter()
            .filter(|(key, _)| key.starts_with(prefix))
            .map(|(key, body)| (key.clone(), self.resolver.expand(body)))
            .filter(|(_, expanded)| Self::is_usable(expanded))
            .collect();
        results.sort_by(|a, b| a.0.cmp(&b.0));
        results
    }

    /// Whether an expanded body is worth offering. The single definition of
    /// "usable": `prefix_search` drops what this rejects and `unusable_keys`
    /// reports it, so the picker and the diagnostic cannot disagree about which
    /// entries disappeared. Takes the *expanded* value so the hot path does not
    /// have to expand twice.
    fn is_usable(expanded: &str) -> bool {
        !expanded.is_empty()
    }

    /// Every usable entry, with variables expanded (empty prefix). Callers use
    /// the emptiness of this to decide whether there is anything to offer, so it
    /// carries `prefix_search`'s drop of unusable entries.
    pub fn all_entries(&self) -> Vec<(String, String)> {
        self.prefix_search("")
    }

    /// Keys whose body expands to nothing right now, so the caller that owns the
    /// config can tell the user. `prefix_search` drops these, which is correct
    /// for the picker but invisible to whoever wrote the file.
    pub fn unusable_keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = self
            .entries
            .iter()
            .filter(|(_, body)| !Self::is_usable(&self.resolver.expand(body)))
            .map(|(key, _)| key.clone())
            .collect();
        keys.sort();
        keys
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snippets::variables::SnippetVariable;

    #[test]
    fn test_prefix_search() {
        let mut entries = HashMap::new();
        entries.insert("gh".to_string(), "https://github.com/".to_string());
        entries.insert("gmail".to_string(), "https://mail.google.com/".to_string());
        entries.insert("email".to_string(), "user@example.com".to_string());

        let resolver = VariableResolver::new(HashMap::new());
        let store = SnippetStore::new(entries, resolver).unwrap();

        let results = store.prefix_search("g");
        assert_eq!(results.len(), 2);
        // Should be sorted by key
        assert_eq!(results[0].0, "gh");
        assert_eq!(results[1].0, "gmail");
    }

    #[test]
    fn test_prefix_search_exact() {
        let mut entries = HashMap::new();
        entries.insert("gh".to_string(), "https://github.com/".to_string());

        let resolver = VariableResolver::new(HashMap::new());
        let store = SnippetStore::new(entries, resolver).unwrap();

        let results = store.prefix_search("gh");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "gh");
    }

    #[test]
    fn test_prefix_search_no_match() {
        let mut entries = HashMap::new();
        entries.insert("gh".to_string(), "https://github.com/".to_string());

        let resolver = VariableResolver::new(HashMap::new());
        let store = SnippetStore::new(entries, resolver).unwrap();

        let results = store.prefix_search("xyz");
        assert!(results.is_empty());
    }

    #[test]
    fn test_all_entries() {
        let mut entries = HashMap::new();
        entries.insert("b".to_string(), "beta".to_string());
        entries.insert("a".to_string(), "alpha".to_string());

        let resolver = VariableResolver::new(HashMap::new());
        let store = SnippetStore::new(entries, resolver).unwrap();

        let all = store.all_entries();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].0, "a");
        assert_eq!(all[1].0, "b");
    }

    #[test]
    fn prefix_search_drops_entries_that_expand_to_nothing() {
        // Both origins of an unusable body: written empty, and a variable that
        // resolves to an empty string. Neither may reach the picker, because
        // confirming one would commit "".
        let mut entries = HashMap::new();
        entries.insert("blank".to_string(), String::new());
        entries.insert("viavar".to_string(), "$blank".to_string());
        entries.insert("real".to_string(), "text".to_string());

        let mut user_vars = HashMap::new();
        user_vars.insert(
            "blank".to_string(),
            SnippetVariable::Static {
                value: String::new(),
            },
        );
        let store = SnippetStore::new(entries, VariableResolver::new(user_vars)).unwrap();

        let all = store.all_entries();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].0, "real");
        assert!(store.prefix_search("blank").is_empty());
        assert!(store.prefix_search("viavar").is_empty());
        // ...and the drop is reportable, so it does not vanish silently.
        assert_eq!(store.unusable_keys(), vec!["blank", "viavar"]);
    }

    #[test]
    fn unusable_keys_is_empty_when_every_body_expands() {
        let mut entries = HashMap::new();
        entries.insert("a".to_string(), "alpha".to_string());
        entries.insert("b".to_string(), "$name".to_string());
        let mut user_vars = HashMap::new();
        user_vars.insert(
            "name".to_string(),
            SnippetVariable::Static {
                value: "Taro".to_string(),
            },
        );
        let store = SnippetStore::new(entries, VariableResolver::new(user_vars)).unwrap();
        assert!(store.unusable_keys().is_empty());
        assert_eq!(store.all_entries().len(), 2);
    }

    #[test]
    fn new_rejects_empty_key() {
        let mut entries = HashMap::new();
        entries.insert(String::new(), "body".to_string());
        let result = SnippetStore::new(entries, VariableResolver::new(HashMap::new()));
        assert!(matches!(result, Err(SnippetConfigError::EmptyKey)));
    }

    #[test]
    fn test_variable_expansion_in_search() {
        let mut entries = HashMap::new();
        entries.insert("sig".to_string(), "Name: $name".to_string());

        let mut user_vars = HashMap::new();
        user_vars.insert(
            "name".to_string(),
            SnippetVariable::Static {
                value: "Taro".to_string(),
            },
        );
        let resolver = VariableResolver::new(user_vars);
        let store = SnippetStore::new(entries, resolver).unwrap();

        let results = store.prefix_search("sig");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].1, "Name: Taro");
    }
}
