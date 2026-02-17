mod auto_commit;
mod basic;
mod candidates;
mod corpus;
mod ghost;
mod proptest_fsm;
mod simulator;
mod submode;

use std::sync::Arc;

use lex_core::dict::{DictEntry, TrieDictionary};

use super::types::key;
use super::InputSession;
use super::KeyResponse;

pub(super) fn make_test_dict() -> Arc<TrieDictionary> {
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
        let text = ch.to_string();
        let resp = session.handle_key(0, &text, 0);
        responses.push(resp);
    }
    responses
}
