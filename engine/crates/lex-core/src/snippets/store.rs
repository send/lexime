use std::collections::HashMap;

use super::variables::VariableResolver;

pub struct SnippetStore {
    entries: HashMap<String, String>,
    resolver: VariableResolver,
}

impl SnippetStore {
    pub fn new(entries: HashMap<String, String>, resolver: VariableResolver) -> Self {
        Self { entries, resolver }
    }

    /// Return all entries matching the given prefix, with variables expanded.
    /// Results are sorted by key for stable ordering.
    pub fn prefix_search(&self, prefix: &str) -> Vec<(String, String)> {
        let mut results: Vec<(String, String)> = self
            .entries
            .iter()
            .filter(|(key, _)| key.starts_with(prefix))
            .map(|(key, body)| (key.clone(), self.resolver.expand(body)))
            .collect();
        results.sort_by(|a, b| a.0.cmp(&b.0));
        results
    }

    /// Return all entries with variables expanded (empty prefix).
    pub fn all_entries(&self) -> Vec<(String, String)> {
        self.prefix_search("")
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
        let store = SnippetStore::new(entries, resolver);

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
        let store = SnippetStore::new(entries, resolver);

        let results = store.prefix_search("gh");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "gh");
    }

    #[test]
    fn test_prefix_search_no_match() {
        let mut entries = HashMap::new();
        entries.insert("gh".to_string(), "https://github.com/".to_string());

        let resolver = VariableResolver::new(HashMap::new());
        let store = SnippetStore::new(entries, resolver);

        let results = store.prefix_search("xyz");
        assert!(results.is_empty());
    }

    #[test]
    fn test_all_entries() {
        let mut entries = HashMap::new();
        entries.insert("b".to_string(), "beta".to_string());
        entries.insert("a".to_string(), "alpha".to_string());

        let resolver = VariableResolver::new(HashMap::new());
        let store = SnippetStore::new(entries, resolver);

        let all = store.all_entries();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].0, "a");
        assert_eq!(all[1].0, "b");
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
        let store = SnippetStore::new(entries, resolver);

        let results = store.prefix_search("sig");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].1, "Name: Taro");
    }
}
