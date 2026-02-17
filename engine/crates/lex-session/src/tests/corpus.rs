use std::path::PathBuf;
use std::sync::Arc;

use lex_core::dict::connection::ConnectionMatrix;
use lex_core::dict::TrieDictionary;

use super::simulator::HeadlessIME;

// ---------------------------------------------------------------------------
// (a) Small-dict corpus — always runs with `cargo test`
// ---------------------------------------------------------------------------

/// Conversion test cases that match make_test_dict() entries.
const SMALL_DICT_CORPUS: &[(&str, &str)] = &[
    ("kyou", "今日"),
    ("tenki", "天気"),
    ("watashi", "私"),
    ("desu", "です"),
    ("ne", "ね"),
    ("ii", "良い"),
];

#[test]
fn test_small_dict_corpus() {
    let dict = super::make_test_dict();
    let mut ime = HeadlessIME::new(dict, None);

    for &(romaji, expected) in SMALL_DICT_CORPUS {
        let result = ime.convert(romaji);
        assert_eq!(
            result, expected,
            "conversion mismatch: romaji={romaji:?}, expected={expected:?}, got={result:?}"
        );
        ime.reset();
    }
}

// ---------------------------------------------------------------------------
// (b) Real-dict corpus — #[ignore], opt-in via `cargo test -- --ignored`
// ---------------------------------------------------------------------------

/// Test cases for the full compiled dictionary + connection matrix.
const REAL_DICT_CORPUS: &[(&str, &str)] = &[
    ("toukyou", "東京"),
    ("oosaka", "大阪"),
    ("nihongo", "日本語"),
    ("kyouhaiitenki", "今日はいい天気"),
    ("watashihagakuseidesu", "私は学生です"),
];

fn real_dict_paths() -> Option<(PathBuf, PathBuf)> {
    let dict_path = std::env::var("LEXIME_DICT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("data/lexime.dict"));
    let conn_path = std::env::var("LEXIME_CONN")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("data/lexime.conn"));

    if dict_path.exists() && conn_path.exists() {
        Some((dict_path, conn_path))
    } else {
        None
    }
}

#[test]
#[ignore]
fn test_real_dict_corpus() {
    let (dict_path, conn_path) = match real_dict_paths() {
        Some(paths) => paths,
        None => {
            eprintln!("skipping real-dict corpus: dictionary files not found");
            return;
        }
    };

    let dict = Arc::new(TrieDictionary::open(&dict_path).expect("failed to open dict"));
    let conn = Arc::new(ConnectionMatrix::open(&conn_path).expect("failed to open conn"));
    let mut ime = HeadlessIME::new(dict, Some(conn));

    for &(romaji, expected) in REAL_DICT_CORPUS {
        let result = ime.convert(romaji);
        assert_eq!(
            result, expected,
            "conversion mismatch: romaji={romaji:?}, expected={expected:?}, got={result:?}"
        );
        ime.reset();
    }
}
