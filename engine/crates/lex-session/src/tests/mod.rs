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

use super::types::{KeyEvent, SessionState};
use super::InputSession;
use super::KeyResponse;

/// The reading to convert, or `None` when there is nothing to convert.
///
/// The single form for "read the composition if there is one". `None` covers all
/// three no-op cases so no caller has to spell them out: `Idle`, snippet mode
/// (which `is_composing()` also reports but which has no `Composition` behind
/// it — `comp()` panics there, so `is_composing()` is *not* a safe guard), and a
/// composition with an empty reading.
pub(super) fn composing_reading(session: &InputSession) -> Option<&str> {
    match &session.state {
        SessionState::Composing(c) if !c.kana.is_empty() => Some(&c.kana),
        _ => None,
    }
}

/// Complete one async candidate cycle: generate candidates for the current
/// reading and feed them back as a fresh (non-stale) response. `None` when
/// there was nothing to generate for, or when the response was discarded.
///
/// Uses `lex_core::candidates::generate_candidates` rather than dispatching on
/// the session's conversion mode on purpose: `test_predictive_mode_no_auto_commit`
/// needs the *Standard* generator's candidates to reach the auto-commit path it
/// asserts about, so the mode must not silently change the generator here.
pub(super) fn complete_candidate_cycle(
    session: &mut InputSession,
    dict: &dyn Dictionary,
) -> Option<KeyResponse> {
    let reading = composing_reading(session)?.to_string();
    let cand = lex_core::candidates::generate_candidates(dict, None, None, &reading, 20);
    // Simulate a fresh (non-stale) response: snapshot the current epoch.
    let epoch = session.epoch;
    session.receive_candidates(epoch, &reading, cand.surfaces, cand.paths)
}

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
    // Collecting into a HashMap would silently dedup a repeated key and pick a
    // body at random, which production refuses to do (`snippets_build_store`
    // returns `InvalidData` for a duplicate). A fixture that quietly disagrees
    // with the loader turns a typo in an entry table into an unrelated
    // assertion failure elsewhere, so reject it here too.
    let mut map: HashMap<String, String> = HashMap::with_capacity(entries.len());
    for (key, body) in entries {
        assert!(
            map.insert((*key).to_string(), (*body).to_string())
                .is_none(),
            "duplicate snippet fixture key: {key:?}",
        );
    }
    // The constructor rejects only empty keys, and every call site passes
    // non-empty key literals. Empty *bodies* are constructible on purpose —
    // `test_snippet_with_empty_body_is_never_offered` needs one, and checks that
    // `prefix_search` is what keeps it out of the picker.
    let store = SnippetStore::new(map, VariableResolver::new(user_vars))
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
