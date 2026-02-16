use super::*;
use crate::dict::{self, TrieDictionary};
use std::ffi::{CStr, CString};
use std::sync::Arc;

fn make_test_dict() -> *mut LexDictWrapper {
    let entries = vec![
        (
            "かんじ".to_string(),
            vec![
                dict::DictEntry {
                    surface: "漢字".to_string(),
                    cost: 5100,
                    left_id: 0,
                    right_id: 0,
                },
                dict::DictEntry {
                    surface: "感じ".to_string(),
                    cost: 5150,
                    left_id: 0,
                    right_id: 0,
                },
            ],
        ),
        (
            "かんじょう".to_string(),
            vec![dict::DictEntry {
                surface: "感情".to_string(),
                cost: 5000,
                left_id: 0,
                right_id: 0,
            }],
        ),
    ];
    let dict = TrieDictionary::from_entries(entries);
    owned_new(LexDictWrapper {
        inner: Arc::new(dict),
    })
}

#[test]
fn test_ffi_lookup_roundtrip() {
    let dict = make_test_dict();
    let reading = CString::new("かんじ").unwrap();

    let list = lex_dict_lookup(dict, reading.as_ptr());
    assert_eq!(list.len, 2);

    unsafe {
        let candidates = std::slice::from_raw_parts(list.candidates, list.len as usize);

        let r = CStr::from_ptr(candidates[0].reading).to_str().unwrap();
        assert_eq!(r, "かんじ");

        let s0 = CStr::from_ptr(candidates[0].surface).to_str().unwrap();
        let s1 = CStr::from_ptr(candidates[1].surface).to_str().unwrap();
        assert_eq!(s0, "漢字");
        assert_eq!(s1, "感じ");

        assert!(candidates[0].cost <= candidates[1].cost);
    }

    lex_candidates_free(list);
    lex_dict_close(dict);
}

#[test]
fn test_ffi_lookup_not_found() {
    let dict = make_test_dict();
    let reading = CString::new("そんざい").unwrap();

    let list = lex_dict_lookup(dict, reading.as_ptr());
    assert_eq!(list.len, 0);
    assert!(list.candidates.is_null());

    lex_candidates_free(list);
    lex_dict_close(dict);
}

#[test]
fn test_ffi_null_safety() {
    let reading = CString::new("かんじ").unwrap();
    let list = lex_dict_lookup(std::ptr::null(), reading.as_ptr());
    assert_eq!(list.len, 0);
    lex_candidates_free(list);

    let dict = make_test_dict();
    let list = lex_dict_lookup(dict, std::ptr::null());
    assert_eq!(list.len, 0);
    lex_candidates_free(list);

    lex_dict_close(dict);
}

#[test]
fn test_ffi_open_close_file() {
    let dir = std::env::temp_dir().join("lexime_test_ffi");
    std::fs::create_dir_all(&dir).unwrap();
    let dict_path = dir.join("test.dict");

    let entries = vec![(
        "てすと".to_string(),
        vec![dict::DictEntry {
            surface: "テスト".to_string(),
            cost: 1000,
            left_id: 0,
            right_id: 0,
        }],
    )];
    let dict = TrieDictionary::from_entries(entries);
    dict.save(&dict_path).unwrap();

    let path_cstr = CString::new(dict_path.to_str().unwrap()).unwrap();
    let dict_ptr = lex_dict_open(path_cstr.as_ptr());
    assert!(!dict_ptr.is_null());

    let reading = CString::new("てすと").unwrap();
    let list = lex_dict_lookup(dict_ptr, reading.as_ptr());
    assert_eq!(list.len, 1);
    unsafe {
        let candidates = std::slice::from_raw_parts(list.candidates, list.len as usize);
        let s = CStr::from_ptr(candidates[0].surface).to_str().unwrap();
        assert_eq!(s, "テスト");
    }
    lex_candidates_free(list);
    lex_dict_close(dict_ptr);

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn test_ffi_open_nonexistent() {
    let path = CString::new("/nonexistent/path/dict.bin").unwrap();
    let dict_ptr = lex_dict_open(path.as_ptr());
    assert!(dict_ptr.is_null());
}

#[test]
fn test_ffi_conn_null_safety() {
    let conn = lex_conn_open(std::ptr::null());
    assert!(conn.is_null());

    let path = CString::new("/nonexistent/path/conn.bin").unwrap();
    let conn = lex_conn_open(path.as_ptr());
    assert!(conn.is_null());

    lex_conn_close(std::ptr::null_mut());
}

#[test]
fn test_ffi_history_save_invalid_path() {
    let dir = std::env::temp_dir().join("lexime_test_ffi_save_fail");
    std::fs::create_dir_all(&dir).unwrap();
    let open_path = dir.join("temp.lxud");
    let open_cstr = CString::new(open_path.to_str().unwrap()).unwrap();

    let history = lex_history_open(open_cstr.as_ptr());
    assert!(!history.is_null());

    let bad_path = CString::new("/nonexistent/deeply/nested/history.lxud").unwrap();
    assert_eq!(lex_history_save(history, bad_path.as_ptr()), -1);

    lex_history_close(history);
    std::fs::remove_dir_all(&dir).ok();
}

// --- Romaji Lookup FFI tests ---

#[test]
fn test_ffi_romaji_lookup_exact() {
    let romaji = CString::new("ka").unwrap();
    let result = lex_romaji_lookup(romaji.as_ptr());
    assert_eq!(result.tag, 2, "ka should be exact");
    let kana = unsafe { CStr::from_ptr(result.kana).to_str().unwrap() };
    assert_eq!(kana, "か");
    lex_romaji_lookup_free(result);
}

#[test]
fn test_ffi_romaji_lookup_prefix() {
    let romaji = CString::new("k").unwrap();
    let result = lex_romaji_lookup(romaji.as_ptr());
    assert_eq!(result.tag, 1, "k should be prefix");
    assert!(result.kana.is_null());
    lex_romaji_lookup_free(result);
}

#[test]
fn test_ffi_romaji_lookup_none() {
    let romaji = CString::new("xyz").unwrap();
    let result = lex_romaji_lookup(romaji.as_ptr());
    assert_eq!(result.tag, 0, "xyz should be none");
    assert!(result.kana.is_null());
    lex_romaji_lookup_free(result);
}

#[test]
fn test_ffi_romaji_lookup_exact_and_prefix() {
    let romaji = CString::new("chi").unwrap();
    let result = lex_romaji_lookup(romaji.as_ptr());
    assert!(
        result.tag == 2 || result.tag == 3,
        "chi should be exact or exactAndPrefix"
    );
    let kana = unsafe { CStr::from_ptr(result.kana).to_str().unwrap() };
    assert_eq!(kana, "ち");
    lex_romaji_lookup_free(result);
}

#[test]
fn test_ffi_romaji_lookup_null_safety() {
    let result = lex_romaji_lookup(std::ptr::null());
    assert_eq!(result.tag, 0);
    assert!(result.kana.is_null());
    lex_romaji_lookup_free(result);
}

// --- Romaji Convert FFI tests ---

#[test]
fn test_ffi_romaji_convert_basic() {
    let kana = CString::new("").unwrap();
    let pending = CString::new("ka").unwrap();
    let result = lex_romaji_convert(kana.as_ptr(), pending.as_ptr(), 0);
    let composed = unsafe { CStr::from_ptr(result.composed_kana).to_str().unwrap() };
    let pend = unsafe { CStr::from_ptr(result.pending_romaji).to_str().unwrap() };
    assert_eq!(composed, "か");
    assert_eq!(pend, "");
    lex_romaji_convert_free(result);
}

#[test]
fn test_ffi_romaji_convert_sokuon() {
    let kana = CString::new("").unwrap();
    let pending = CString::new("kka").unwrap();
    let result = lex_romaji_convert(kana.as_ptr(), pending.as_ptr(), 0);
    let composed = unsafe { CStr::from_ptr(result.composed_kana).to_str().unwrap() };
    assert_eq!(composed, "っか");
    lex_romaji_convert_free(result);
}

#[test]
fn test_ffi_romaji_convert_force_n() {
    let kana = CString::new("").unwrap();
    let pending = CString::new("n").unwrap();
    let result = lex_romaji_convert(kana.as_ptr(), pending.as_ptr(), 1);
    let composed = unsafe { CStr::from_ptr(result.composed_kana).to_str().unwrap() };
    assert_eq!(composed, "ん");
    lex_romaji_convert_free(result);
}

#[test]
fn test_ffi_romaji_convert_collapse() {
    let kana = CString::new("kあ").unwrap();
    let pending = CString::new("").unwrap();
    let result = lex_romaji_convert(kana.as_ptr(), pending.as_ptr(), 0);
    let composed = unsafe { CStr::from_ptr(result.composed_kana).to_str().unwrap() };
    assert_eq!(composed, "か");
    lex_romaji_convert_free(result);
}

#[test]
fn test_ffi_romaji_convert_null_safety() {
    let pending = CString::new("ka").unwrap();
    let result = lex_romaji_convert(std::ptr::null(), pending.as_ptr(), 0);
    assert!(result.composed_kana.is_null());
    lex_romaji_convert_free(result);

    let kana = CString::new("あ").unwrap();
    let result = lex_romaji_convert(kana.as_ptr(), std::ptr::null(), 0);
    assert!(result.composed_kana.is_null());
    lex_romaji_convert_free(result);
}

// --- Unified Candidate Generation FFI tests ---

#[test]
fn test_ffi_generate_candidates_roundtrip() {
    let dict = make_test_dict();
    let reading = CString::new("かんじ").unwrap();

    let resp = lex_generate_candidates(
        dict,
        std::ptr::null(),
        std::ptr::null(),
        reading.as_ptr(),
        10,
    );
    assert!(
        resp.surfaces_len >= 1,
        "should return at least one candidate"
    );

    unsafe {
        let surfaces = std::slice::from_raw_parts(resp.surfaces, resp.surfaces_len as usize);
        let all: Vec<&str> = surfaces
            .iter()
            .map(|&p| CStr::from_ptr(p).to_str().unwrap())
            .collect();
        assert!(
            all.contains(&"かんじ"),
            "kana should be present in candidates: {:?}",
            all,
        );
    }

    lex_candidate_response_free(resp);
    lex_dict_close(dict);
}

#[test]
fn test_ffi_generate_candidates_null_safety() {
    let reading = CString::new("かんじ").unwrap();

    let resp = lex_generate_candidates(
        std::ptr::null(),
        std::ptr::null(),
        std::ptr::null(),
        reading.as_ptr(),
        10,
    );
    assert_eq!(resp.surfaces_len, 0);
    lex_candidate_response_free(resp);

    let dict = make_test_dict();
    let resp = lex_generate_candidates(
        dict,
        std::ptr::null(),
        std::ptr::null(),
        std::ptr::null(),
        10,
    );
    assert_eq!(resp.surfaces_len, 0);
    lex_candidate_response_free(resp);

    lex_dict_close(dict);
}

#[test]
fn test_ffi_generate_candidates_punctuation() {
    let dict = make_test_dict();
    let reading = CString::new("。").unwrap();

    let resp = lex_generate_candidates(
        dict,
        std::ptr::null(),
        std::ptr::null(),
        reading.as_ptr(),
        10,
    );
    assert!(
        resp.surfaces_len >= 1,
        "punctuation should return candidates"
    );

    unsafe {
        let surfaces = std::slice::from_raw_parts(resp.surfaces, resp.surfaces_len as usize);
        let s0 = CStr::from_ptr(surfaces[0]).to_str().unwrap();
        assert_eq!(s0, "。", "first candidate should be the punctuation itself");
    }

    lex_candidate_response_free(resp);
    lex_dict_close(dict);
}

// --- InputSession FFI tests ---

#[test]
fn test_ffi_session_roundtrip() {
    let dict = make_test_dict();
    let session = lex_session_new(dict, std::ptr::null(), std::ptr::null());
    assert!(!session.is_null());

    let k = CString::new("k").unwrap();
    let resp = lex_session_handle_key(session, 0, k.as_ptr(), 0);
    assert_eq!(resp.consumed, 1);
    assert_eq!(lex_session_is_composing(session), 1);
    lex_key_response_free(resp);

    let a = CString::new("a").unwrap();
    let resp = lex_session_handle_key(session, 0, a.as_ptr(), 0);
    assert_eq!(resp.consumed, 1);
    assert!(!resp.marked_text.is_null());
    lex_key_response_free(resp);

    let empty = CString::new("").unwrap();
    let resp = lex_session_handle_key(session, 36, empty.as_ptr(), 0);
    assert_eq!(resp.consumed, 1);
    assert!(!resp.commit_text.is_null());
    assert_eq!(lex_session_is_composing(session), 0);
    lex_key_response_free(resp);

    lex_session_free(session);
    lex_dict_close(dict);
}

#[test]
fn test_ffi_session_null_safety() {
    let resp = lex_session_handle_key(std::ptr::null_mut(), 0, std::ptr::null(), 0);
    assert_eq!(resp.consumed, 0);
    lex_key_response_free(resp);

    let resp = lex_session_commit(std::ptr::null_mut());
    assert_eq!(resp.consumed, 0);
    lex_key_response_free(resp);

    assert_eq!(lex_session_is_composing(std::ptr::null()), 0);

    let session = lex_session_new(std::ptr::null(), std::ptr::null(), std::ptr::null());
    assert!(session.is_null());
}

#[test]
fn test_ffi_session_with_history() {
    let dict = make_test_dict();
    let dir = std::env::temp_dir().join("lexime_test_ffi_session_hist");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("test.lxud");
    let path_cstr = CString::new(path.to_str().unwrap()).unwrap();

    let history = lex_history_open(path_cstr.as_ptr());
    assert!(!history.is_null());

    let session = lex_session_new(dict, std::ptr::null(), history);
    assert!(!session.is_null());

    for ch in "kyou".chars() {
        let s = CString::new(ch.to_string()).unwrap();
        let resp = lex_session_handle_key(session, 0, s.as_ptr(), 0);
        lex_key_response_free(resp);
    }

    let empty = CString::new("").unwrap();
    let resp = lex_session_handle_key(session, 36, empty.as_ptr(), 0);
    assert_eq!(resp.consumed, 1);
    assert_eq!(resp.save_history, 1);
    lex_key_response_record_history(&resp, history);
    lex_key_response_free(resp);

    assert_eq!(lex_history_save(history, path_cstr.as_ptr()), 0);

    lex_session_free(session);
    lex_history_close(history);
    lex_dict_close(dict);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn test_ffi_session_programmer_mode() {
    let dict = make_test_dict();
    let session = lex_session_new(dict, std::ptr::null(), std::ptr::null());
    lex_session_set_programmer_mode(session, 1);

    let yen = CString::new("¥").unwrap();
    let resp = lex_session_handle_key(session, 93, yen.as_ptr(), 0);
    assert_eq!(resp.consumed, 1);
    unsafe {
        let text = CStr::from_ptr(resp.commit_text).to_str().unwrap();
        assert_eq!(text, "\\");
    }
    lex_key_response_free(resp);

    lex_session_free(session);
    lex_dict_close(dict);
}
