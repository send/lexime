use super::*;
use crate::dict::Dictionary;

#[test]
fn register_and_lookup() {
    let dict = UserDictionary::new();
    assert!(dict.register("しゅうじ", "週次"));
    let entries = dict.lookup("しゅうじ");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].surface, "週次");
    assert_eq!(entries[0].cost, USER_COST);
    assert_eq!(entries[0].left_id, USER_POS_ID);
}

#[test]
fn register_duplicate() {
    let dict = UserDictionary::new();
    assert!(dict.register("しゅうじ", "週次"));
    assert!(!dict.register("しゅうじ", "週次"));
    assert_eq!(dict.lookup("しゅうじ").len(), 1);
}

#[test]
fn register_multiple_surfaces() {
    let dict = UserDictionary::new();
    dict.register("しゅうじ", "週次");
    dict.register("しゅうじ", "修辞");
    let entries = dict.lookup("しゅうじ");
    assert_eq!(entries.len(), 2);
    let surfaces: Vec<&str> = entries.iter().map(|e| e.surface.as_str()).collect();
    assert!(surfaces.contains(&"週次"));
    assert!(surfaces.contains(&"修辞"));
}

#[test]
fn unregister() {
    let dict = UserDictionary::new();
    dict.register("しゅうじ", "週次");
    dict.register("しゅうじ", "修辞");
    assert!(dict.unregister("しゅうじ", "週次"));
    let entries = dict.lookup("しゅうじ");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].surface, "修辞");
}

#[test]
fn unregister_last_entry_removes_key() {
    let dict = UserDictionary::new();
    dict.register("しゅうじ", "週次");
    assert!(dict.unregister("しゅうじ", "週次"));
    assert!(dict.lookup("しゅうじ").is_empty());
    assert!(dict.list().is_empty());
}

#[test]
fn unregister_not_found() {
    let dict = UserDictionary::new();
    assert!(!dict.unregister("しゅうじ", "週次"));
}

#[test]
fn list_sorted() {
    let dict = UserDictionary::new();
    dict.register("みかん", "蜜柑");
    dict.register("あいう", "愛雨");
    dict.register("かき", "柿");
    let list = dict.list();
    assert_eq!(list.len(), 3);
    assert_eq!(list[0], ("あいう".to_string(), "愛雨".to_string()));
    assert_eq!(list[1], ("かき".to_string(), "柿".to_string()));
    assert_eq!(list[2], ("みかん".to_string(), "蜜柑".to_string()));
}

#[test]
fn predict_by_prefix() {
    let dict = UserDictionary::new();
    dict.register("きょう", "今日");
    dict.register("きょうと", "京都");
    dict.register("かき", "柿");

    let results = dict.predict("きょう", 100);
    assert_eq!(results.len(), 2);
    let readings: Vec<&str> = results.iter().map(|r| r.reading.as_str()).collect();
    assert!(readings.contains(&"きょう"));
    assert!(readings.contains(&"きょうと"));
}

#[test]
fn predict_max_results() {
    let dict = UserDictionary::new();
    dict.register("きょう", "今日");
    dict.register("きょうと", "京都");
    let results = dict.predict("きょう", 1);
    assert_eq!(results.len(), 1);
}

#[test]
fn common_prefix_search_finds_prefixes() {
    let dict = UserDictionary::new();
    dict.register("き", "木");
    dict.register("きょう", "今日");
    dict.register("きょうと", "京都");

    let results = dict.common_prefix_search("きょうは");
    let readings: Vec<&str> = results.iter().map(|r| r.reading.as_str()).collect();
    assert!(readings.contains(&"き"));
    assert!(readings.contains(&"きょう"));
    // "きょうと" is NOT a prefix of "きょうは"
    assert!(!readings.contains(&"きょうと"));
}

#[test]
fn serialize_roundtrip() {
    let dict = UserDictionary::new();
    dict.register("しゅうじ", "週次");
    dict.register("かき", "柿");

    let bytes = dict.to_bytes().unwrap();
    let loaded = UserDictionary::from_bytes(&bytes).unwrap();

    let entries = loaded.lookup("しゅうじ");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].surface, "週次");
    assert_eq!(entries[0].cost, USER_COST);
    assert_eq!(loaded.lookup("かき").len(), 1);
}

#[test]
fn file_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test_user_dict.lxuw");

    let dict = UserDictionary::new();
    dict.register("しゅうじ", "週次");
    dict.register("かき", "柿");
    dict.save(&path).unwrap();

    let loaded = UserDictionary::open(&path).unwrap();
    assert_eq!(loaded.list().len(), 2);
    assert_eq!(loaded.lookup("しゅうじ")[0].surface, "週次");
}

#[test]
fn open_nonexistent_returns_empty() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("does_not_exist.lxuw");
    let dict = UserDictionary::open(&path).unwrap();
    assert!(dict.list().is_empty());
}

#[test]
fn from_bytes_bad_magic() {
    let bytes = b"BADXsome data here";
    assert!(UserDictionary::from_bytes(bytes).is_err());
}

#[test]
fn from_bytes_too_short() {
    let bytes = b"LX";
    assert!(UserDictionary::from_bytes(bytes).is_err());
}

// --- Hardening: durable save + recovering open (design §9) ---

/// `.corrupt-*` quarantine files in `dir`.
fn corrupt_files(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.contains(".corrupt-"))
        })
        .collect()
}

#[test]
fn open_recovering_quarantines_corrupt() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("user_dict.lxuw");
    let corrupt = b"BADX not a real lxuw file".to_vec();
    std::fs::write(&path, &corrupt).unwrap();

    let (dict, report) = UserDictionary::open_recovering(&path).unwrap();

    // Empty but usable; data loss surfaced.
    assert!(dict.list().is_empty());
    assert!(report.data_loss_suspected());
    assert_eq!(report.quarantined_paths.len(), 1);

    // Original path cleared; bytes preserved verbatim in the quarantine file.
    assert!(!path.exists());
    let quarantined = corrupt_files(dir.path());
    assert_eq!(quarantined.len(), 1);
    assert_eq!(std::fs::read(&quarantined[0]).unwrap(), corrupt);
}

#[test]
fn open_recovering_nonexistent_is_clean() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("user_dict.lxuw");
    let (dict, report) = UserDictionary::open_recovering(&path).unwrap();
    assert!(dict.list().is_empty());
    assert!(!report.data_loss_suspected());
}

#[test]
fn open_recovering_valid_is_clean() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("user_dict.lxuw");
    let dict = UserDictionary::new();
    dict.register("しゅうじ", "週次");
    dict.save(&path).unwrap();

    let (loaded, report) = UserDictionary::open_recovering(&path).unwrap();
    assert!(!report.data_loss_suspected());
    assert_eq!(loaded.lookup("しゅうじ")[0].surface, "週次");
    assert!(corrupt_files(dir.path()).is_empty());
}

#[test]
fn strict_open_corrupt_errors_without_side_effects() {
    // The CLI's strict open must NOT quarantine — an audit tool must never
    // rename a live user's file.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("user_dict.lxuw");
    std::fs::write(&path, b"BADX corrupt").unwrap();

    assert!(UserDictionary::open(&path).is_err());
    assert!(path.exists()); // untouched, not renamed aside
    assert!(corrupt_files(dir.path()).is_empty());
}

#[test]
fn save_leaves_no_stray_tmp() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("user_dict.lxuw");
    let dict = UserDictionary::new();
    dict.register("かき", "柿");
    dict.save(&path).unwrap();

    // The durable write removes its tmp via rename and never leaves a stray
    // sibling (the old `with_extension("tmp")` produced `user_dict.tmp`).
    let mut names: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, vec!["user_dict.lxuw".to_string()]);
}

#[test]
fn from_bytes_rejects_oversized_length() {
    // Valid header + a bincode Vec length prefix claiming u64::MAX elements.
    // The allocation cap must reject it (fast Err), never attempt a giant
    // allocation.
    let mut bytes = Vec::new();
    bytes.extend_from_slice(MAGIC);
    bytes.push(VERSION);
    bytes.extend_from_slice(&[0xFF; 8]);
    assert!(UserDictionary::from_bytes(&bytes).is_err());
}

#[test]
fn open_recovering_rotates_quarantine() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("user_dict.lxuw");
    // Each corrupt open quarantines then rotates; only the newest are kept.
    for _ in 0..(QUARANTINE_KEEP + 1) {
        std::fs::write(&path, b"BADX corrupt").unwrap();
        let (_dict, report) = UserDictionary::open_recovering(&path).unwrap();
        assert!(report.data_loss_suspected());
    }
    assert_eq!(corrupt_files(dir.path()).len(), QUARANTINE_KEEP);
}
