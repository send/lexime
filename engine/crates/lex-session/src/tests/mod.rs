mod basic;
mod candidates;
mod corpus;
mod proptest_fsm;
mod simulator;
mod snippets;

use std::collections::HashMap;
use std::sync::Arc;

use lex_core::dict::{DictEntry, Dictionary, TrieDictionary};
use lex_core::snippets::{SnippetStore, SnippetVariable, VariableResolver};

use super::types::KeyEvent;
use super::InputSession;
use super::KeyResponse;

/// Build a snippet store from `(key, body)` pairs, with `$name` expanding to
/// "Taro".
///
/// Only the construction is shared: each caller keeps its own entry table,
/// because the tables are not interchangeable. The example tests assert exact
/// match counts and sort positions against theirs, while the proptest needs
/// keys its randomly generated romaji can actually reach.
pub(super) fn make_snippet_store(entries: &[(&str, &str)]) -> Arc<SnippetStore> {
    let mut user_vars = HashMap::new();
    user_vars.insert(
        "name".to_string(),
        SnippetVariable::Static {
            value: "Taro".to_string(),
        },
    );
    let entries: HashMap<String, String> = entries
        .iter()
        .map(|(key, body)| ((*key).to_string(), (*body).to_string()))
        .collect();
    // The constructor rejects only empty keys, and every call site passes
    // non-empty literals.
    let store = SnippetStore::new(entries, VariableResolver::new(user_vars))
        .expect("snippet fixture keys must be non-empty");
    Arc::new(store)
}

pub(super) fn make_test_dict() -> Arc<dyn Dictionary> {
    let entries = vec![
        (
            "きょう".to_string(),
            vec![
                DictEntry {
                    surface: "今日".to_string(),
                    cost: 3000,
                    left_id: 0,
                    right_id: 0,
                },
                DictEntry {
                    surface: "京".to_string(),
                    cost: 5000,
                    left_id: 0,
                    right_id: 0,
                },
            ],
        ),
        (
            "は".to_string(),
            vec![DictEntry {
                surface: "は".to_string(),
                cost: 2000,
                left_id: 0,
                right_id: 0,
            }],
        ),
        (
            "いい".to_string(),
            vec![
                DictEntry {
                    surface: "良い".to_string(),
                    cost: 3500,
                    left_id: 0,
                    right_id: 0,
                },
                DictEntry {
                    surface: "いい".to_string(),
                    cost: 4000,
                    left_id: 0,
                    right_id: 0,
                },
            ],
        ),
        (
            "てんき".to_string(),
            vec![DictEntry {
                surface: "天気".to_string(),
                cost: 4000,
                left_id: 0,
                right_id: 0,
            }],
        ),
        (
            "い".to_string(),
            vec![DictEntry {
                surface: "胃".to_string(),
                cost: 6000,
                left_id: 0,
                right_id: 0,
            }],
        ),
        (
            "き".to_string(),
            vec![DictEntry {
                surface: "木".to_string(),
                cost: 4500,
                left_id: 0,
                right_id: 0,
            }],
        ),
        (
            "てん".to_string(),
            vec![DictEntry {
                surface: "天".to_string(),
                cost: 5000,
                left_id: 0,
                right_id: 0,
            }],
        ),
        (
            "わたし".to_string(),
            vec![DictEntry {
                surface: "私".to_string(),
                cost: 3000,
                left_id: 0,
                right_id: 0,
            }],
        ),
        (
            "です".to_string(),
            vec![DictEntry {
                surface: "です".to_string(),
                cost: 2500,
                left_id: 0,
                right_id: 0,
            }],
        ),
        (
            "ね".to_string(),
            vec![DictEntry {
                surface: "ね".to_string(),
                cost: 2000,
                left_id: 0,
                right_id: 0,
            }],
        ),
        (
            "。".to_string(),
            vec![DictEntry {
                surface: "。".to_string(),
                cost: 1000,
                left_id: 0,
                right_id: 0,
            }],
        ),
    ];
    Arc::new(TrieDictionary::from_entries(entries))
}

// Helper: simulate typing a string one character at a time
pub(super) fn type_string(session: &mut InputSession, s: &str) -> Vec<KeyResponse> {
    let mut responses = Vec::new();
    for ch in s.chars() {
        let resp = session.handle_key(KeyEvent::text(&ch.to_string()));
        responses.push(resp);
    }
    responses
}
