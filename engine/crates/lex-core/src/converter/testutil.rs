#![cfg(test)]

use crate::dict::connection::ConnectionMatrix;
use crate::dict::{DictEntry, TrieDictionary};

/// Shared test dictionary for converter tests.
///
/// Contains entries for a representative set of words used across
/// lattice and viterbi tests.
pub fn test_dict() -> TrieDictionary {
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
                    left_id: 101,
                    right_id: 101,
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
        (
            "です".to_string(),
            vec![DictEntry {
                surface: "です".to_string(),
                cost: 2500,
                left_id: 800,
                right_id: 800,
            }],
        ),
        (
            "ね".to_string(),
            vec![DictEntry {
                surface: "ね".to_string(),
                cost: 2000,
                left_id: 900,
                right_id: 900,
            }],
        ),
        (
            "わたし".to_string(),
            vec![DictEntry {
                surface: "私".to_string(),
                cost: 3000,
                left_id: 1000,
                right_id: 1000,
            }],
        ),
        (
            "がくせい".to_string(),
            vec![DictEntry {
                surface: "学生".to_string(),
                cost: 4000,
                left_id: 1100,
                right_id: 1100,
            }],
        ),
    ];
    TrieDictionary::from_entries(entries)
}

/// Create a zero-cost connection matrix with the given function-word ID range.
pub fn zero_conn_with_fw(num_ids: u16, fw_min: u16, fw_max: u16) -> ConnectionMatrix {
    let n = num_ids as usize;
    let text = format!("{num_ids} {num_ids}\n{}", "0\n".repeat(n * n));
    ConnectionMatrix::from_text_with_metadata(&text, fw_min, fw_max).unwrap()
}

/// Create a zero-cost connection matrix with roles.
pub fn zero_conn_with_roles(num_ids: u16, roles: Vec<u8>) -> ConnectionMatrix {
    let n = num_ids as usize;
    let text = format!("{num_ids} {num_ids}\n{}", "0\n".repeat(n * n));
    ConnectionMatrix::from_text_with_roles(&text, 0, 0, roles).unwrap()
}

/// A dictionary entry with no POS ids.
pub fn entry(surface: &str, cost: i16) -> DictEntry {
    DictEntry {
        surface: surface.into(),
        cost,
        left_id: 0,
        right_id: 0,
    }
}

/// 食べる against the fragments 田|辺留 (two nodes, two segment penalties):
/// the fragments sit far above the best, more than the default cost-gap
/// bound (the RC-2 田辺る shape).
pub fn taberu_dict() -> TrieDictionary {
    TrieDictionary::from_entries(vec![
        ("たべる".into(), vec![entry("食べる", 0)]),
        ("た".into(), vec![entry("田", 3000)]),
        ("べる".into(), vec![entry("辺留", 3000)]),
    ])
}
