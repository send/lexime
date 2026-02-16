use super::*;
use crate::converter::testutil::test_dict;
use crate::dict::connection::ConnectionMatrix;
use crate::dict::{DictEntry, TrieDictionary};

#[test]
fn test_convert_unigram() {
    let dict = test_dict();
    let result = convert(&dict, None, "きょうはいいてんき");

    let surfaces: Vec<&str> = result.iter().map(|s| s.surface.as_str()).collect();
    // Without connection costs, should pick lowest-cost words
    // Expected: 今日(3000) + は(2000) + 良い(3500) + 天気(4000) = 12500
    assert_eq!(surfaces, vec!["今日", "は", "良い", "天気"]);
}

#[test]
fn test_convert_empty() {
    let dict = test_dict();
    let result = convert(&dict, None, "");
    assert!(result.is_empty());
}

#[test]
fn test_convert_single_word() {
    let dict = test_dict();
    let result = convert(&dict, None, "きょう");

    assert_eq!(result.len(), 1);
    assert_eq!(result[0].surface, "今日");
    assert_eq!(result[0].reading, "きょう");
}

#[test]
fn test_convert_unknown_chars() {
    let dict = test_dict();
    let result = convert(&dict, None, "ぬ");

    assert_eq!(result.len(), 1);
    assert_eq!(result[0].surface, "ぬ");
}

#[test]
fn test_convert_watashi() {
    let dict = test_dict();
    let result = convert(&dict, None, "わたしはがくせいです");

    let surfaces: Vec<&str> = result.iter().map(|s| s.surface.as_str()).collect();
    assert_eq!(surfaces, vec!["私", "は", "学生", "です"]);
}

#[test]
fn test_convert_with_connection_costs() {
    // Build a dict where "きょう" has two entries with similar word costs:
    //   "今日" (cost=5000, id=10) and "京" (cost=4900, id=20)
    // Without connection costs, "京" wins (lower cost).
    // With connection costs that penalize id=20→id=30 ("は") heavily,
    // "今日" should win instead.
    let entries = vec![
        (
            "きょう".to_string(),
            vec![
                DictEntry {
                    surface: "今日".to_string(),
                    cost: 5000,
                    left_id: 10,
                    right_id: 10,
                },
                DictEntry {
                    surface: "京".to_string(),
                    cost: 4900,
                    left_id: 20,
                    right_id: 20,
                },
            ],
        ),
        (
            "は".to_string(),
            vec![DictEntry {
                surface: "は".to_string(),
                cost: 2000,
                left_id: 30,
                right_id: 30,
            }],
        ),
    ];
    let dict = TrieDictionary::from_entries(entries);

    // Without connection costs, "京" (4900) wins over "今日" (5000)
    let result_unigram = convert(&dict, None, "きょうは");
    assert_eq!(result_unigram[0].surface, "京");

    // Build a connection matrix (31×31 to cover ids 0..30)
    // Set high penalty for id 20→30 transition, low for id 10→30
    let num_ids = 31;
    let mut text = format!("{num_ids} {num_ids}\n");
    for left in 0..num_ids {
        for right in 0..num_ids {
            let cost = if left == 20 && right == 30 {
                500 // heavy penalty: 京→は
            } else {
                0
            };
            text.push_str(&format!("{cost}\n"));
        }
    }
    let conn = ConnectionMatrix::from_text(&text).unwrap();

    // With connection costs: "京"(4900) + conn(20→30=500) = 5400
    //                        "今日"(5000) + conn(10→30=0) = 5000
    // "今日" should now win
    let result_bigram = convert(&dict, Some(&conn), "きょうは");
    assert_eq!(result_bigram[0].surface, "今日");
    assert_eq!(result_bigram[1].surface, "は");
}

#[test]
fn test_viterbi_tiebreak_deterministic() {
    let entries = vec![(
        "あ".to_string(),
        vec![
            DictEntry {
                surface: "亜".to_string(),
                cost: 5000,
                left_id: 0,
                right_id: 0,
            },
            DictEntry {
                surface: "阿".to_string(),
                cost: 5000,
                left_id: 0,
                right_id: 0,
            },
        ],
    )];
    let dict = TrieDictionary::from_entries(entries);
    let first = convert(&dict, None, "あ");
    assert_eq!(first.len(), 1);
    for _ in 0..10 {
        let result = convert(&dict, None, "あ");
        assert_eq!(
            result[0].surface, first[0].surface,
            "Viterbi tie-breaking must be deterministic"
        );
    }
}
