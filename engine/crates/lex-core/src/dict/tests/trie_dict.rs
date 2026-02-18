use crate::dict::{DictEntry, DictError, Dictionary, TrieDictionary};

fn sample_dict() -> TrieDictionary {
    let entries = vec![
        (
            "かん".to_string(),
            vec![
                DictEntry {
                    surface: "缶".to_string(),
                    cost: 5000,
                    left_id: 0,
                    right_id: 0,
                },
                DictEntry {
                    surface: "管".to_string(),
                    cost: 5200,
                    left_id: 0,
                    right_id: 0,
                },
            ],
        ),
        (
            "かんじ".to_string(),
            vec![
                DictEntry {
                    surface: "漢字".to_string(),
                    cost: 5100,
                    left_id: 0,
                    right_id: 0,
                },
                DictEntry {
                    surface: "感じ".to_string(),
                    cost: 5150,
                    left_id: 0,
                    right_id: 0,
                },
                DictEntry {
                    surface: "幹事".to_string(),
                    cost: 5300,
                    left_id: 0,
                    right_id: 0,
                },
            ],
        ),
        (
            "かんじょう".to_string(),
            vec![
                DictEntry {
                    surface: "感情".to_string(),
                    cost: 5000,
                    left_id: 0,
                    right_id: 0,
                },
                DictEntry {
                    surface: "勘定".to_string(),
                    cost: 5400,
                    left_id: 0,
                    right_id: 0,
                },
            ],
        ),
        (
            "き".to_string(),
            vec![DictEntry {
                surface: "木".to_string(),
                cost: 4000,
                left_id: 0,
                right_id: 0,
            }],
        ),
    ];
    TrieDictionary::from_entries(entries)
}

#[test]
fn test_lookup_exact() {
    let dict = sample_dict();
    let results = dict.lookup("かんじ");
    assert_eq!(results.len(), 3);
    assert_eq!(results[0].surface, "漢字");
    assert_eq!(results[1].surface, "感じ");
    assert_eq!(results[2].surface, "幹事");
}

#[test]
fn test_lookup_not_found() {
    let dict = sample_dict();
    assert!(dict.lookup("そんざい").is_empty());
}

#[test]
fn test_predict() {
    let dict = sample_dict();
    let results = dict.predict("かん", 100);
    assert_eq!(results.len(), 3); // かん, かんじ, かんじょう
    let readings: Vec<&str> = results.iter().map(|r| r.reading.as_str()).collect();
    assert!(readings.contains(&"かん"));
    assert!(readings.contains(&"かんじ"));
    assert!(readings.contains(&"かんじょう"));
}

#[test]
fn test_predict_max_results() {
    let dict = sample_dict();
    let results = dict.predict("かん", 2);
    assert_eq!(results.len(), 2);
}

#[test]
fn test_predict_max_results_zero() {
    let dict = sample_dict();
    let results = dict.predict("かん", 0);
    assert!(results.is_empty());
}

#[test]
fn test_predict_no_match() {
    let dict = sample_dict();
    let results = dict.predict("そ", 100);
    assert!(results.is_empty());
}

#[test]
fn test_cost_ordering() {
    let dict = sample_dict();
    let results = dict.lookup("かんじ");
    for w in results.windows(2) {
        assert!(w[0].cost <= w[1].cost, "entries should be sorted by cost");
    }
}

#[test]
fn test_serialize_roundtrip() {
    let dict = sample_dict();
    let bytes = dict.to_bytes().unwrap();
    let dict2 = TrieDictionary::from_bytes(&bytes).unwrap();

    let r1 = dict.lookup("かんじ");
    let r2 = dict2.lookup("かんじ");
    assert_eq!(r1.len(), r2.len());
    for (a, b) in r1.iter().zip(r2.iter()) {
        assert_eq!(a.surface, b.surface);
        assert_eq!(a.cost, b.cost);
    }
}

#[test]
fn test_invalid_magic() {
    let result = TrieDictionary::from_bytes(b"XXXX\x02data");
    assert!(matches!(result, Err(DictError::InvalidMagic)));
}

#[test]
fn test_header_too_short() {
    let result = TrieDictionary::from_bytes(b"LXD");
    assert!(matches!(result, Err(DictError::InvalidHeader)));
}

#[test]
fn test_unsupported_version() {
    let result = TrieDictionary::from_bytes(b"LXDX\x99");
    assert!(matches!(result, Err(DictError::UnsupportedVersion(0x99))));
}

#[test]
fn test_predict_ranked_cost_order() {
    let dict = sample_dict();
    let results = dict.predict_ranked("かん", 100, 200);
    // Should be sorted by cost ascending
    for w in results.windows(2) {
        assert!(
            w[0].1.cost <= w[1].1.cost,
            "predict_ranked should be cost-ordered: {} <= {}",
            w[0].1.cost,
            w[1].1.cost,
        );
    }
}

#[test]
fn test_predict_ranked_dedup_surface() {
    // Create a dict where two different readings produce the same surface
    let entries = vec![
        (
            "かん".to_string(),
            vec![DictEntry {
                surface: "感".to_string(),
                cost: 5200,
                left_id: 0,
                right_id: 0,
            }],
        ),
        (
            "かんじ".to_string(),
            vec![DictEntry {
                surface: "感".to_string(),
                cost: 5000,
                left_id: 0,
                right_id: 0,
            }],
        ),
    ];
    let dict = TrieDictionary::from_entries(entries);
    let results = dict.predict_ranked("かん", 100, 200);
    // "感" should appear only once, with the lower cost (5000)
    let surfaces: Vec<&str> = results.iter().map(|(_, e)| e.surface.as_str()).collect();
    assert_eq!(
        surfaces.iter().filter(|&&s| s == "感").count(),
        1,
        "duplicate surface should be deduplicated"
    );
    let entry = results.iter().find(|(_, e)| e.surface == "感").unwrap();
    assert_eq!(entry.1.cost, 5000, "should keep lowest cost");
}

#[test]
fn test_predict_ranked_max_results() {
    let dict = sample_dict();
    let results = dict.predict_ranked("かん", 2, 200);
    assert_eq!(results.len(), 2);
}

#[test]
fn test_predict_ranked_no_match() {
    let dict = sample_dict();
    let results = dict.predict_ranked("そ", 100, 200);
    assert!(results.is_empty());
}

#[test]
fn test_open_mmap() {
    let dict = sample_dict();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.dict");
    dict.save(&path).unwrap();

    let dict2 = TrieDictionary::open(&path).unwrap();
    let r1 = dict.lookup("かんじ");
    let r2 = dict2.lookup("かんじ");
    assert_eq!(r1.len(), r2.len());
    for (a, b) in r1.iter().zip(r2.iter()) {
        assert_eq!(a.surface, b.surface);
        assert_eq!(a.cost, b.cost);
    }

    // Verify predictive search also works with mmap-backed trie
    let p1 = dict.predict("かん", 100);
    let p2 = dict2.predict("かん", 100);
    assert_eq!(p1.len(), p2.len());
}

// --- Integration tests (require compiled Mozc dictionary) ---

#[test]
#[ignore]
fn test_mozc_dict_known_entries() {
    let dict_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("data")
        .join("lexime-sudachi.dict");
    let dict = TrieDictionary::open(&dict_path)
        .expect("failed to open lexime-sudachi.dict — run `make dict` first");

    // かんじ should have 漢字
    let results = dict.lookup("かんじ");
    assert!(!results.is_empty(), "かんじ should exist");
    let surfaces: Vec<&str> = results.iter().map(|e| e.surface.as_str()).collect();
    assert!(
        surfaces.contains(&"漢字"),
        "漢字 not found in: {surfaces:?}"
    );
    assert!(
        surfaces.contains(&"感じ"),
        "感じ not found in: {surfaces:?}"
    );

    // にほん should have 日本
    let results = dict.lookup("にほん");
    assert!(!results.is_empty(), "にほん should exist");
    let surfaces: Vec<&str> = results.iter().map(|e| e.surface.as_str()).collect();
    assert!(
        surfaces.contains(&"日本"),
        "日本 not found in: {surfaces:?}"
    );
}

#[test]
#[ignore]
fn test_mozc_dict_predict_performance() {
    let dict_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("data")
        .join("lexime-sudachi.dict");
    let dict = TrieDictionary::open(&dict_path)
        .expect("failed to open lexime-sudachi.dict — run `make dict` first");

    let prefixes = ["か", "かん", "と", "たべ", "に"];
    for prefix in &prefixes {
        let start = std::time::Instant::now();
        let results = dict.predict(prefix, 100);
        let elapsed = start.elapsed();
        assert!(
            elapsed.as_millis() < 5,
            "predict({prefix}) took {elapsed:?}, expected <5ms"
        );
        assert!(!results.is_empty(), "predict({prefix}) returned no results");
    }
}
