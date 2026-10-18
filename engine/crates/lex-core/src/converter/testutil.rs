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

/// The surface a conversion path spells.
pub fn joined(path: &[super::ConvertedSegment]) -> String {
    path.iter().map(|s| s.surface.as_str()).collect()
}

/// A moment ahead of any wall clock a test runs under: a history last used
/// here ages as 0 against the clock (a negative age saturates), so a read that
/// fell back to the wall clock sees every boost whole.
pub const T0: u64 = 4_000_000_000;

/// One half-life after [`T0`]: a boost is exactly half its undecayed value
/// (`frequency * boost_per_use`, capped at `max_boost`).
pub fn half() -> u64 {
    T0 + (crate::settings::settings().history.half_life_hours * 3600.0) as u64
}

/// ~1100 years after [`T0`]: every boost decayed to 0.
pub const STALE: u64 = T0 + 10_000_000 * 3600;

/// A dictionary entry with no POS ids.
pub fn entry(surface: &str, cost: i16) -> DictEntry {
    DictEntry {
        surface: surface.into(),
        cost,
        left_id: 0,
        right_id: 0,
    }
}

/// A dictionary entry whose left and right POS id are both `id`.
pub fn entry_with_id(surface: &str, cost: i16, id: u16) -> DictEntry {
    DictEntry {
        left_id: id,
        right_id: id,
        ..entry(surface, cost)
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

/// `かなや`: 仮名屋 (one segment, the #1) and 可|な|や, whose two 5000
/// transitions (10000 in its Viterbi price) put it far above 仮名屋 —
/// cost-gap admission drops it.
pub fn far_multi_segment_kanaya() -> (TrieDictionary, ConnectionMatrix) {
    let e = entry_with_id;
    let dict = TrieDictionary::from_entries(vec![
        ("か".into(), vec![e("可", 0, 1)]),
        ("な".into(), vec![e("な", 0, 2)]),
        ("や".into(), vec![e("や", 0, 4)]),
        ("かなや".into(), vec![e("仮名屋", 0, 5)]),
    ]);
    let mut costs = vec![0i16; 36];
    costs[6 + 2] = 5000; // 1 → 2
    costs[2 * 6 + 4] = 5000; // 2 → 4
    (
        dict,
        ConnectionMatrix::new_owned(6, 0, 0, Vec::new(), costs),
    )
}

/// `かなや` with `depth` katakana spellings (cheapest in Viterbi order, but
/// penalised by rerank's script feature) ahead of 金谷, so that 金谷 is
/// rerank's argmin without a history and Viterbi's `depth + 1`-th path.
/// A population of at most `depth` never sees it (#361). Every path is one
/// segment.
pub fn deep_argmin_kanaya(depth: usize) -> (TrieDictionary, ConnectionMatrix) {
    let mut entries: Vec<DictEntry> = (0..depth)
        .map(|i| entry_with_id(&format!("カナヤ{}", "ア".repeat(i)), (i as i16) * 10, 1))
        .collect();
    entries.push(entry_with_id("金谷", (depth as i16) * 10, 1));
    (
        TrieDictionary::from_entries(vec![("かなや".into(), entries)]),
        ConnectionMatrix::new_owned(2, 0, 0, Vec::new(), vec![0i16; 4]),
    )
}

/// `かなや` with `rank` kanji spellings in Viterbi order and 学習 at
/// position `rank + 1`. Learned once, it is the #1 of any population that
/// reaches it and nothing else is. Every path is one segment.
pub fn deep_learned_kanaya(rank: usize) -> (TrieDictionary, ConnectionMatrix) {
    let mut entries: Vec<DictEntry> = (0..rank)
        .map(|i| {
            let c = char::from_u32(0x4E00 + i as u32).unwrap();
            entry_with_id(&c.to_string(), (i as i16) * 10, 1)
        })
        .collect();
    entries.push(entry_with_id(DEEP_LEARNED, (rank as i16) * 10, 1));
    (
        TrieDictionary::from_entries(vec![("かなや".into(), entries)]),
        ConnectionMatrix::new_owned(2, 0, 0, Vec::new(), vec![0i16; 4]),
    )
}

/// The surface [`deep_learned_kanaya`] puts at the learned position.
pub const DEEP_LEARNED: &str = "学習";
