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
