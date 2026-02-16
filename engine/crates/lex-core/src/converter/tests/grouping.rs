use super::*;
use crate::converter::postprocess::group_segments;
use crate::converter::testutil::{test_dict, zero_conn_with_fw, zero_conn_with_roles};
use crate::converter::viterbi::RichSegment;
use crate::dict::connection::ConnectionMatrix;

fn rich(reading: &str, surface: &str, id: u16) -> RichSegment {
    RichSegment {
        reading: reading.into(),
        surface: surface.into(),
        left_id: id,
        right_id: id,
        word_cost: 0,
    }
}

#[test]
fn test_group_segments_basic() {
    // content(100) + func(200) + content(300) → 2 segments
    let conn = zero_conn_with_fw(301, 200, 200);
    let mut segs = vec![
        rich("きょう", "今日", 100),
        rich("は", "は", 200),
        rich("いい", "良い", 300),
    ];
    group_segments(&mut segs, &conn);
    assert_eq!(segs.len(), 2);
    assert_eq!(segs[0].reading, "きょうは");
    assert_eq!(segs[0].surface, "今日は");
    assert_eq!(segs[0].left_id, 100);
    assert_eq!(segs[0].right_id, 200);
    assert_eq!(segs[1].reading, "いい");
    assert_eq!(segs[1].surface, "良い");
}

#[test]
fn test_group_segments_leading_func() {
    // Leading function word stays standalone
    let conn = zero_conn_with_fw(301, 200, 200);
    let mut segs = vec![rich("は", "は", 200), rich("きょう", "今日", 100)];
    group_segments(&mut segs, &conn);
    assert_eq!(segs.len(), 2);
    assert_eq!(segs[0].surface, "は");
    assert_eq!(segs[1].surface, "今日");
}

#[test]
fn test_group_segments_consecutive_func() {
    // content + func + func → all merged into one segment
    let conn = zero_conn_with_fw(301, 200, 210);
    let mut segs = vec![
        rich("たべ", "食べ", 100),
        rich("て", "て", 200),
        rich("は", "は", 210),
    ];
    group_segments(&mut segs, &conn);
    assert_eq!(segs.len(), 1);
    assert_eq!(segs[0].reading, "たべては");
    assert_eq!(segs[0].surface, "食べては");
    assert_eq!(segs[0].left_id, 100);
    assert_eq!(segs[0].right_id, 210);
}

#[test]
fn test_group_segments_all_content() {
    // All content words → no grouping
    let conn = zero_conn_with_fw(301, 200, 200);
    let mut segs = vec![rich("きょう", "今日", 100), rich("いい", "良い", 300)];
    group_segments(&mut segs, &conn);
    assert_eq!(segs.len(), 2);
}

#[test]
fn test_group_segments_single_and_empty() {
    let conn = zero_conn_with_fw(301, 200, 200);

    let mut single = vec![rich("きょう", "今日", 100)];
    group_segments(&mut single, &conn);
    assert_eq!(single.len(), 1);

    let mut empty: Vec<RichSegment> = vec![];
    group_segments(&mut empty, &conn);
    assert!(empty.is_empty());
}

#[test]
fn test_group_segments_suffix() {
    // CW(0) + Suffix(1) → merged into one segment "田中さん"
    // roles: 0=CW, 1=Suffix
    let mut roles = vec![0u8; 5];
    roles[1] = 2; // suffix
    let conn = zero_conn_with_roles(5, roles);
    let mut segs = vec![rich("たなか", "田中", 0), rich("さん", "さん", 1)];
    group_segments(&mut segs, &conn);
    assert_eq!(segs.len(), 1);
    assert_eq!(segs[0].surface, "田中さん");
    assert_eq!(segs[0].reading, "たなかさん");
}

#[test]
fn test_group_segments_prefix() {
    // Prefix(2) + CW(0) → merged into one segment "お茶"
    // roles: 0=CW, 2=Prefix
    let mut roles = vec![0u8; 5];
    roles[2] = 3; // prefix
    let conn = zero_conn_with_roles(5, roles);
    let mut segs = vec![rich("お", "お", 2), rich("ちゃ", "茶", 0)];
    group_segments(&mut segs, &conn);
    assert_eq!(segs.len(), 1);
    assert_eq!(segs[0].surface, "お茶");
    assert_eq!(segs[0].reading, "おちゃ");
}

#[test]
fn test_group_segments_prefix_cw_suffix() {
    // Prefix(3) + CW(0) + Suffix(2) → all merged into one segment
    let mut roles = vec![0u8; 5];
    roles[2] = 2; // suffix
    roles[3] = 3; // prefix
    let conn = zero_conn_with_roles(5, roles);
    let mut segs = vec![
        rich("お", "お", 3),
        rich("ちゃ", "茶", 0),
        rich("さん", "さん", 2),
    ];
    group_segments(&mut segs, &conn);
    assert_eq!(segs.len(), 1);
    assert_eq!(segs[0].surface, "お茶さん");
    assert_eq!(segs[0].reading, "おちゃさん");
}

#[test]
fn test_group_segments_cw_suffix_fw() {
    // CW(0) + Suffix(2) + FW(1) → all merged
    // Need both fw_range (for id=1) and roles (for suffix id=2)
    let text = "5 5\n".to_owned() + &"0\n".repeat(25);
    let mut roles = vec![0u8; 5];
    roles[2] = 2; // suffix
    let conn = ConnectionMatrix::from_text_with_roles(&text, 1, 1, roles).unwrap();
    let mut segs = vec![
        rich("たなか", "田中", 0),
        rich("さん", "さん", 2),
        rich("は", "は", 1),
    ];
    group_segments(&mut segs, &conn);
    assert_eq!(segs.len(), 1);
    assert_eq!(segs[0].surface, "田中さんは");
    assert_eq!(segs[0].reading, "たなかさんは");
}

#[test]
fn test_group_segments_leading_suffix() {
    // Leading suffix with no preceding CW stays standalone
    let mut roles = vec![0u8; 5];
    roles[1] = 2; // suffix
    let conn = zero_conn_with_roles(5, roles);
    let mut segs = vec![rich("さん", "さん", 1), rich("たなか", "田中", 0)];
    group_segments(&mut segs, &conn);
    assert_eq!(segs.len(), 2);
    assert_eq!(segs[0].surface, "さん");
    assert_eq!(segs[1].surface, "田中");
}

#[test]
fn test_convert_groups_with_conn() {
    // Integration test: convert with a conn that has fw_range covering は(id=200)
    let dict = test_dict();
    let conn = zero_conn_with_fw(1200, 200, 200);
    let result = convert(&dict, Some(&conn), "きょうは");
    // "今日" + "は" should be grouped into one segment "今日は"
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].surface, "今日は");
    assert_eq!(result[0].reading, "きょうは");
}
