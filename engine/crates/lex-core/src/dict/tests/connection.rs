use std::fs;

use crate::dict::connection::ConnectionMatrix;
use crate::dict::DictError;

fn sample_matrix() -> ConnectionMatrix {
    let text = "3 3\n0\n10\n20\n30\n40\n50\n60\n70\n80\n";
    ConnectionMatrix::from_text(text).unwrap()
}

#[test]
fn test_from_text() {
    let m = sample_matrix();
    assert_eq!(m.num_ids(), 3);
    // Row 0: [0, 10, 20]
    assert_eq!(m.cost(0, 0), 0);
    assert_eq!(m.cost(0, 1), 10);
    assert_eq!(m.cost(0, 2), 20);
    // Row 1: [30, 40, 50]
    assert_eq!(m.cost(1, 0), 30);
    assert_eq!(m.cost(1, 1), 40);
    assert_eq!(m.cost(1, 2), 50);
    // Row 2: [60, 70, 80]
    assert_eq!(m.cost(2, 0), 60);
    assert_eq!(m.cost(2, 1), 70);
    assert_eq!(m.cost(2, 2), 80);
}

#[test]
fn test_serialize_roundtrip() {
    let m = sample_matrix();
    let bytes = m.to_bytes();
    // Always V3
    assert_eq!(bytes[4], 3);
    let m2 = ConnectionMatrix::from_bytes(&bytes).unwrap();
    assert_eq!(m2.num_ids(), m.num_ids());
    for left in 0..m.num_ids() {
        for right in 0..m.num_ids() {
            assert_eq!(m.cost(left, right), m2.cost(left, right));
        }
    }
}

#[test]
fn test_file_roundtrip() {
    let dir = std::env::temp_dir().join("lexime_test_conn");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("test.conn");

    let m = sample_matrix();
    m.save(&path).unwrap();

    let m2 = ConnectionMatrix::open(&path).unwrap();
    assert_eq!(m2.num_ids(), 3);
    assert_eq!(m.cost(1, 2), m2.cost(1, 2));

    fs::remove_dir_all(&dir).ok();
}

#[test]
fn test_invalid_magic() {
    let result = ConnectionMatrix::from_bytes(b"XXXX\x03\x03\x00\x00\x00\x00\x00");
    assert!(matches!(result, Err(DictError::InvalidMagic)));
}

#[test]
fn test_header_too_short() {
    let result = ConnectionMatrix::from_bytes(b"LXC");
    assert!(matches!(result, Err(DictError::InvalidHeader)));
}

#[test]
fn test_unsupported_version() {
    let result = ConnectionMatrix::from_bytes(b"LXCX\x99\x01\x00\x00\x00\x00\x00");
    assert!(matches!(result, Err(DictError::UnsupportedVersion(0x99))));
}

#[test]
fn test_negative_costs() {
    let text = "2 2\n-100\n200\n-300\n400\n";
    let m = ConnectionMatrix::from_text(text).unwrap();
    assert_eq!(m.cost(0, 0), -100);
    assert_eq!(m.cost(0, 1), 200);
    assert_eq!(m.cost(1, 0), -300);
    assert_eq!(m.cost(1, 1), 400);
}

#[test]
fn test_wrong_count() {
    let text = "2 2\n0\n10\n20\n"; // only 3 costs instead of 4
    let result = ConnectionMatrix::from_text(text);
    assert!(matches!(result, Err(DictError::Parse(_))));
}

#[test]
fn test_mecab_triplet_format() {
    let text = "2 2\n0 0 10\n0 1 20\n1 0 30\n1 1 40\n";
    let m = ConnectionMatrix::from_text(text).unwrap();
    assert_eq!(m.num_ids(), 2);
    assert_eq!(m.cost(0, 0), 10);
    assert_eq!(m.cost(1, 0), 20);
    assert_eq!(m.cost(0, 1), 30);
    assert_eq!(m.cost(1, 1), 40);
}

#[test]
fn test_mecab_triplet_sparse() {
    let text = "2 2\n0 1 100\n1 0 -200\n";
    let m = ConnectionMatrix::from_text(text).unwrap();
    assert_eq!(m.cost(0, 0), 0);
    assert_eq!(m.cost(0, 1), -200);
    assert_eq!(m.cost(1, 0), 100);
    assert_eq!(m.cost(1, 1), 0);
}

#[test]
fn test_mecab_triplet_roundtrip() {
    let text = "2 2\n0 0 10\n0 1 20\n1 0 30\n1 1 40\n";
    let m = ConnectionMatrix::from_text(text).unwrap();
    let bytes = m.to_bytes();
    let m2 = ConnectionMatrix::from_bytes(&bytes).unwrap();
    assert_eq!(m2.num_ids(), 2);
    for left in 0..2 {
        for right in 0..2 {
            assert_eq!(m.cost(left, right), m2.cost(left, right));
        }
    }
}

#[test]
fn test_metadata_roundtrip() {
    let text = "3 3\n0\n10\n20\n30\n40\n50\n60\n70\n80\n";
    let m = ConnectionMatrix::from_text_with_metadata(text, 29, 433).unwrap();
    assert!(m.is_function_word(29));
    assert!(m.is_function_word(200));
    assert!(m.is_function_word(433));
    assert!(!m.is_function_word(28));
    assert!(!m.is_function_word(434));

    let bytes = m.to_bytes();
    assert_eq!(bytes[4], 3); // always V3
    let m2 = ConnectionMatrix::from_bytes(&bytes).unwrap();
    assert_eq!(m2.num_ids(), 3);
    assert!(m2.is_function_word(100));
    assert!(!m2.is_function_word(0));
    for left in 0..3 {
        for right in 0..3 {
            assert_eq!(m.cost(left, right), m2.cost(left, right));
        }
    }
}

#[test]
fn test_is_function_word_no_range() {
    let m = sample_matrix();
    assert!(!m.is_function_word(0));
    assert!(!m.is_function_word(100));
}

#[test]
fn test_metadata_file_roundtrip() {
    let dir = std::env::temp_dir().join("lexime_test_conn_meta");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("test_meta.conn");

    let text = "2 2\n10\n20\n30\n40\n";
    let m = ConnectionMatrix::from_text_with_metadata(text, 50, 300).unwrap();
    m.save(&path).unwrap();

    let m2 = ConnectionMatrix::open(&path).unwrap();
    assert_eq!(m2.num_ids(), 2);
    assert!(m2.is_function_word(100));
    assert!(!m2.is_function_word(49));
    assert_eq!(m2.cost(0, 0), 10);
    assert_eq!(m2.cost(1, 1), 40);
    // roles default to 0
    assert_eq!(m2.role(0), 0);
    assert_eq!(m2.role(1), 0);

    fs::remove_dir_all(&dir).ok();
}

#[test]
fn test_roles_roundtrip() {
    let text = "4 4\n";
    let costs_text: String = (0..16).map(|i| format!("{}\n", i * 10)).collect::<String>();
    let full_text = format!("{text}{costs_text}");
    let roles = vec![0, 1, 2, 3];
    let m = ConnectionMatrix::from_text_with_roles(&full_text, 1, 1, roles).unwrap();

    assert_eq!(m.role(0), 0);
    assert_eq!(m.role(1), 1);
    assert_eq!(m.role(2), 2);
    assert_eq!(m.role(3), 3);
    assert!(m.is_suffix(2));
    assert!(m.is_prefix(3));
    assert!(m.is_function_word(1));

    let bytes = m.to_bytes();
    assert_eq!(bytes[4], 3);

    let m2 = ConnectionMatrix::from_bytes(&bytes).unwrap();
    assert_eq!(m2.num_ids(), 4);
    assert_eq!(m2.role(0), 0);
    assert_eq!(m2.role(1), 1);
    assert_eq!(m2.role(2), 2);
    assert_eq!(m2.role(3), 3);
    assert!(m2.is_suffix(2));
    assert!(m2.is_prefix(3));
    for left in 0..4 {
        for right in 0..4 {
            assert_eq!(m.cost(left, right), m2.cost(left, right));
        }
    }
}

#[test]
fn test_roles_file_roundtrip() {
    let dir = std::env::temp_dir().join("lexime_test_conn_roles");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("test_roles.conn");

    let text = "3 3\n0\n10\n20\n30\n40\n50\n60\n70\n80\n";
    let roles = vec![0, 2, 3]; // content, suffix, prefix
    let m = ConnectionMatrix::from_text_with_roles(text, 0, 0, roles).unwrap();
    m.save(&path).unwrap();

    let m2 = ConnectionMatrix::open(&path).unwrap();
    assert_eq!(m2.num_ids(), 3);
    assert_eq!(m2.role(0), 0);
    assert_eq!(m2.role(1), 2);
    assert_eq!(m2.role(2), 3);
    assert!(m2.is_suffix(1));
    assert!(m2.is_prefix(2));
    assert_eq!(m2.cost(0, 1), 10);
    assert_eq!(m2.cost(2, 2), 80);

    fs::remove_dir_all(&dir).ok();
}
