// FFI functions perform null checks before dereferencing raw pointers.
// Clippy cannot verify this statically, so we allow it at crate level.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

pub mod converter;
pub mod dict;
pub mod user_history;

use std::ffi::{c_char, CStr, CString};
use std::path::Path;
use std::ptr;
use std::sync::RwLock;

use converter::{convert, convert_nbest, convert_nbest_with_cost, convert_with_cost};
use dict::connection::ConnectionMatrix;
use dict::{Dictionary, TrieDictionary};
use user_history::cost::LearnedCostFunction;
use user_history::UserHistory;

/// Safely convert a C string pointer to a `&str`.
/// Returns `None` if the pointer is null or contains invalid UTF-8.
unsafe fn cptr_to_str<'a>(ptr: *const c_char) -> Option<&'a str> {
    if ptr.is_null() {
        return None;
    }
    CStr::from_ptr(ptr).to_str().ok()
}

/// Convert a nullable ConnectionMatrix pointer to an `Option<&ConnectionMatrix>`.
unsafe fn conn_ref<'a>(conn: *const ConnectionMatrix) -> Option<&'a ConnectionMatrix> {
    if conn.is_null() {
        None
    } else {
        Some(&*conn)
    }
}

#[no_mangle]
pub extern "C" fn lex_engine_version() -> *const c_char {
    c"0.1.0".as_ptr()
}

#[no_mangle]
pub extern "C" fn lex_engine_echo(x: i32) -> i32 {
    x
}

// --- Dictionary FFI ---

#[repr(C)]
pub struct LexCandidate {
    pub reading: *const c_char,
    pub surface: *const c_char,
    pub cost: i16,
}

#[repr(C)]
pub struct LexCandidateList {
    pub candidates: *const LexCandidate,
    pub len: u32,
    _owned: *mut CandidateListOwned,
}

struct CandidateListOwned {
    candidates: Vec<LexCandidate>,
    _strings: Vec<CString>,
}

impl LexCandidateList {
    fn empty() -> Self {
        Self {
            candidates: ptr::null(),
            len: 0,
            _owned: ptr::null_mut(),
        }
    }

    fn from_entries(reading: &str, entries: &[dict::DictEntry]) -> Self {
        let Ok(reading_cstr) = CString::new(reading) else {
            return Self::empty();
        };
        let reading_ptr = reading_cstr.as_ptr();

        let mut strings = Vec::with_capacity(entries.len() + 1);
        let mut candidates = Vec::with_capacity(entries.len());

        strings.push(reading_cstr);

        for entry in entries {
            let Ok(surface) = CString::new(entry.surface.as_str()) else {
                continue; // skip entries with interior null bytes
            };
            candidates.push(LexCandidate {
                reading: reading_ptr,
                surface: surface.as_ptr(),
                cost: entry.cost,
            });
            strings.push(surface);
        }

        Self::pack(candidates, strings)
    }

    fn from_flat_entries(pairs: &[(String, dict::DictEntry)]) -> Self {
        let mut strings = Vec::new();
        let mut candidates = Vec::new();

        for (reading, entry) in pairs {
            let Ok(reading_cstr) = CString::new(reading.as_str()) else {
                continue;
            };
            let Ok(surface) = CString::new(entry.surface.as_str()) else {
                continue;
            };
            candidates.push(LexCandidate {
                reading: reading_cstr.as_ptr(),
                surface: surface.as_ptr(),
                cost: entry.cost,
            });
            strings.push(reading_cstr);
            strings.push(surface);
        }

        Self::pack(candidates, strings)
    }

    fn from_search_results(results: Vec<dict::SearchResult>) -> Self {
        let mut strings = Vec::new();
        let mut candidates = Vec::new();

        for result in &results {
            let Ok(reading_cstr) = CString::new(result.reading.as_str()) else {
                continue; // skip results with interior null bytes
            };
            let reading_ptr = reading_cstr.as_ptr();
            strings.push(reading_cstr);

            for entry in &result.entries {
                let Ok(surface) = CString::new(entry.surface.as_str()) else {
                    continue;
                };
                candidates.push(LexCandidate {
                    reading: reading_ptr,
                    surface: surface.as_ptr(),
                    cost: entry.cost,
                });
                strings.push(surface);
            }
        }

        Self::pack(candidates, strings)
    }

    fn pack(candidates: Vec<LexCandidate>, strings: Vec<CString>) -> Self {
        if candidates.is_empty() {
            return Self::empty();
        }

        let owned = Box::new(CandidateListOwned {
            candidates,
            _strings: strings,
        });
        let owned_ptr = Box::into_raw(owned);

        // SAFETY: `owned_ptr` was just created from `Box::into_raw` and has not been
        // deallocated. The `candidates` Vec inside the Box is heap-allocated and its
        // data pointer remains stable as long as the Vec is not mutated or dropped.
        // The Box is kept alive via `_owned` in the returned struct, and `_strings`
        // keeps the CString data alive so the char pointers inside candidates are valid.
        let candidates_ptr = unsafe { (*owned_ptr).candidates.as_ptr() };
        let len = unsafe { (*owned_ptr).candidates.len() as u32 };

        Self {
            candidates: candidates_ptr,
            len,
            _owned: owned_ptr,
        }
    }
}

#[no_mangle]
pub extern "C" fn lex_dict_open(path: *const c_char) -> *mut TrieDictionary {
    let Some(path_str) = (unsafe { cptr_to_str(path) }) else {
        return ptr::null_mut();
    };

    match TrieDictionary::open(Path::new(path_str)) {
        Ok(dict) => Box::into_raw(Box::new(dict)),
        Err(_) => ptr::null_mut(),
    }
}

#[no_mangle]
pub extern "C" fn lex_dict_close(dict: *mut TrieDictionary) {
    if !dict.is_null() {
        unsafe {
            drop(Box::from_raw(dict));
        }
    }
}

#[no_mangle]
pub extern "C" fn lex_dict_lookup(
    dict: *const TrieDictionary,
    reading: *const c_char,
) -> LexCandidateList {
    if dict.is_null() {
        return LexCandidateList::empty();
    }
    let Some(reading_str) = (unsafe { cptr_to_str(reading) }) else {
        return LexCandidateList::empty();
    };
    let dict = unsafe { &*dict };

    match dict.lookup(reading_str) {
        Some(entries) => LexCandidateList::from_entries(reading_str, entries),
        None => LexCandidateList::empty(),
    }
}

#[no_mangle]
pub extern "C" fn lex_dict_predict(
    dict: *const TrieDictionary,
    prefix: *const c_char,
    max_results: u32,
) -> LexCandidateList {
    if dict.is_null() {
        return LexCandidateList::empty();
    }
    let Some(prefix_str) = (unsafe { cptr_to_str(prefix) }) else {
        return LexCandidateList::empty();
    };
    let dict = unsafe { &*dict };

    let results = dict.predict(prefix_str, max_results as usize);
    LexCandidateList::from_search_results(results)
}

#[no_mangle]
pub extern "C" fn lex_dict_predict_ranked(
    dict: *const TrieDictionary,
    history: *const LexUserHistoryWrapper,
    prefix: *const c_char,
    max_results: u32,
) -> LexCandidateList {
    if dict.is_null() {
        return LexCandidateList::empty();
    }
    let Some(prefix_str) = (unsafe { cptr_to_str(prefix) }) else {
        return LexCandidateList::empty();
    };
    let dict = unsafe { &*dict };

    // When history is available, over-fetch so boosted entries that rank outside
    // the pure-cost top-N still have a chance to surface after re-sorting.
    let fetch_limit = if history.is_null() {
        max_results as usize
    } else {
        (max_results as usize).max(50)
    };
    let mut ranked = dict.predict_ranked(prefix_str, fetch_limit, 1000);

    // If history is provided, re-sort by unigram boost (descending), then cost (ascending)
    if !history.is_null() {
        let wrapper = unsafe { &*history };
        if let Ok(h) = wrapper.inner.read() {
            ranked.sort_by(|(r_a, e_a), (r_b, e_b)| {
                let boost_a = h.unigram_boost(r_a, &e_a.surface);
                let boost_b = h.unigram_boost(r_b, &e_b.surface);
                boost_b
                    .cmp(&boost_a) // higher boost first
                    .then(e_a.cost.cmp(&e_b.cost)) // then lower cost first
            });
        }
        ranked.truncate(max_results as usize);
    }

    LexCandidateList::from_flat_entries(&ranked)
}

#[no_mangle]
pub extern "C" fn lex_candidates_free(list: LexCandidateList) {
    if !list._owned.is_null() {
        unsafe {
            drop(Box::from_raw(list._owned));
        }
    }
}

// --- Connection matrix FFI ---

#[no_mangle]
pub extern "C" fn lex_conn_open(path: *const c_char) -> *mut ConnectionMatrix {
    let Some(path_str) = (unsafe { cptr_to_str(path) }) else {
        return ptr::null_mut();
    };

    match ConnectionMatrix::open(Path::new(path_str)) {
        Ok(conn) => Box::into_raw(Box::new(conn)),
        Err(_) => ptr::null_mut(),
    }
}

#[no_mangle]
pub extern "C" fn lex_conn_close(conn: *mut ConnectionMatrix) {
    if !conn.is_null() {
        unsafe {
            drop(Box::from_raw(conn));
        }
    }
}

// --- Conversion FFI ---

#[repr(C)]
pub struct LexSegment {
    pub reading: *const c_char,
    pub surface: *const c_char,
}

#[repr(C)]
pub struct LexConversionResult {
    pub segments: *const LexSegment,
    pub len: u32,
    _owned: *mut ConversionResultOwned,
}

struct ConversionResultOwned {
    segments: Vec<LexSegment>,
    _strings: Vec<CString>,
}

impl LexConversionResult {
    fn empty() -> Self {
        Self {
            segments: ptr::null(),
            len: 0,
            _owned: ptr::null_mut(),
        }
    }
}

/// Pack a list of ConvertedSegments into a C-compatible LexConversionResult.
fn pack_conversion_result(result: Vec<converter::ConvertedSegment>) -> LexConversionResult {
    if result.is_empty() {
        return LexConversionResult::empty();
    }

    let mut strings = Vec::with_capacity(result.len() * 2);
    let mut segments = Vec::with_capacity(result.len());

    for seg in &result {
        let Ok(reading) = CString::new(seg.reading.as_str()) else {
            continue;
        };
        let Ok(surface) = CString::new(seg.surface.as_str()) else {
            continue;
        };
        segments.push(LexSegment {
            reading: reading.as_ptr(),
            surface: surface.as_ptr(),
        });
        strings.push(reading);
        strings.push(surface);
    }

    let owned = Box::new(ConversionResultOwned {
        segments,
        _strings: strings,
    });
    let owned_ptr = Box::into_raw(owned);

    // SAFETY: Same pattern as CandidateListOwned::pack — `owned_ptr` is freshly
    // created from Box::into_raw, `segments` Vec data is stable, and `_strings`
    // keeps CString data alive for the lifetime of the returned struct.
    let segments_ptr = unsafe { (*owned_ptr).segments.as_ptr() };
    let len = unsafe { (*owned_ptr).segments.len() as u32 };

    LexConversionResult {
        segments: segments_ptr,
        len,
        _owned: owned_ptr,
    }
}

#[no_mangle]
pub extern "C" fn lex_convert(
    dict: *const TrieDictionary,
    conn: *const ConnectionMatrix,
    kana: *const c_char,
) -> LexConversionResult {
    if dict.is_null() {
        return LexConversionResult::empty();
    }
    let Some(kana_str) = (unsafe { cptr_to_str(kana) }) else {
        return LexConversionResult::empty();
    };
    let dict = unsafe { &*dict };
    let conn = unsafe { conn_ref(conn) };

    pack_conversion_result(convert(dict, conn, kana_str))
}

#[no_mangle]
pub extern "C" fn lex_conversion_free(result: LexConversionResult) {
    if !result._owned.is_null() {
        unsafe {
            drop(Box::from_raw(result._owned));
        }
    }
}

// --- N-best Conversion FFI ---

#[repr(C)]
pub struct LexConversionResultList {
    pub results: *const LexConversionResult,
    pub len: u32,
    _owned: *mut ConversionResultListOwned,
}

struct ConversionResultListOwned {
    results: Vec<LexConversionResult>,
}

impl LexConversionResultList {
    fn empty() -> Self {
        Self {
            results: ptr::null(),
            len: 0,
            _owned: ptr::null_mut(),
        }
    }
}

fn pack_conversion_result_list(
    paths: Vec<Vec<converter::ConvertedSegment>>,
) -> LexConversionResultList {
    if paths.is_empty() {
        return LexConversionResultList::empty();
    }

    let results: Vec<LexConversionResult> = paths.into_iter().map(pack_conversion_result).collect();

    let owned = Box::new(ConversionResultListOwned { results });
    let owned_ptr = Box::into_raw(owned);

    // SAFETY: `owned_ptr` was just created from Box::into_raw. The `results` Vec
    // inside is heap-allocated and stable. Each LexConversionResult inside owns
    // its own strings via its `_owned` pointer.
    let results_ptr = unsafe { (*owned_ptr).results.as_ptr() };
    let len = unsafe { (*owned_ptr).results.len() as u32 };

    LexConversionResultList {
        results: results_ptr,
        len,
        _owned: owned_ptr,
    }
}

#[no_mangle]
pub extern "C" fn lex_convert_nbest(
    dict: *const TrieDictionary,
    conn: *const ConnectionMatrix,
    kana: *const c_char,
    n: u32,
) -> LexConversionResultList {
    if dict.is_null() {
        return LexConversionResultList::empty();
    }
    let Some(kana_str) = (unsafe { cptr_to_str(kana) }) else {
        return LexConversionResultList::empty();
    };
    let dict = unsafe { &*dict };
    let conn = unsafe { conn_ref(conn) };

    pack_conversion_result_list(convert_nbest(dict, conn, kana_str, n as usize))
}

#[no_mangle]
pub extern "C" fn lex_convert_nbest_with_history(
    dict: *const TrieDictionary,
    conn: *const ConnectionMatrix,
    history: *const LexUserHistoryWrapper,
    kana: *const c_char,
    n: u32,
) -> LexConversionResultList {
    if dict.is_null() || history.is_null() {
        return LexConversionResultList::empty();
    }
    let Some(kana_str) = (unsafe { cptr_to_str(kana) }) else {
        return LexConversionResultList::empty();
    };
    let dict = unsafe { &*dict };
    let conn = unsafe { conn_ref(conn) };
    let wrapper = unsafe { &*history };
    let Ok(h) = wrapper.inner.read() else {
        return LexConversionResultList::empty();
    };

    let cost_fn = LearnedCostFunction::new(conn, &h);
    pack_conversion_result_list(convert_nbest_with_cost(
        dict, &cost_fn, conn, kana_str, n as usize,
    ))
}

#[no_mangle]
pub extern "C" fn lex_conversion_result_list_free(list: LexConversionResultList) {
    if !list._owned.is_null() {
        unsafe {
            // First free each individual ConversionResult's owned data
            let owned = Box::from_raw(list._owned);
            for result in &owned.results {
                if !result._owned.is_null() {
                    drop(Box::from_raw(result._owned));
                }
            }
            // The owned box (containing the Vec<LexConversionResult>) is dropped here
        }
    }
}

// --- User History FFI ---

pub struct LexUserHistoryWrapper {
    inner: RwLock<UserHistory>,
}

#[no_mangle]
pub extern "C" fn lex_history_open(path: *const c_char) -> *mut LexUserHistoryWrapper {
    let Some(path_str) = (unsafe { cptr_to_str(path) }) else {
        return ptr::null_mut();
    };

    match UserHistory::open(Path::new(path_str)) {
        Ok(history) => Box::into_raw(Box::new(LexUserHistoryWrapper {
            inner: RwLock::new(history),
        })),
        Err(_) => ptr::null_mut(),
    }
}

#[no_mangle]
pub extern "C" fn lex_history_close(history: *mut LexUserHistoryWrapper) {
    if !history.is_null() {
        unsafe {
            drop(Box::from_raw(history));
        }
    }
}

#[no_mangle]
pub extern "C" fn lex_history_record(
    history: *const LexUserHistoryWrapper,
    segments: *const LexSegment,
    len: u32,
) {
    if history.is_null() || segments.is_null() || len == 0 {
        return;
    }
    let wrapper = unsafe { &*history };
    let segs = unsafe { std::slice::from_raw_parts(segments, len as usize) };

    let pairs: Vec<(String, String)> = segs
        .iter()
        .filter_map(|s| {
            let reading = unsafe { cptr_to_str(s.reading) }?;
            let surface = unsafe { cptr_to_str(s.surface) }?;
            Some((reading.to_string(), surface.to_string()))
        })
        .collect();

    if let Ok(mut h) = wrapper.inner.write() {
        h.record(&pairs);
    }
}

#[no_mangle]
pub extern "C" fn lex_history_save(
    history: *const LexUserHistoryWrapper,
    path: *const c_char,
) -> i32 {
    if history.is_null() {
        return -1;
    }
    let Some(path_str) = (unsafe { cptr_to_str(path) }) else {
        return -1;
    };
    let wrapper = unsafe { &*history };
    let Ok(h) = wrapper.inner.read() else {
        return -1;
    };
    match h.save(Path::new(path_str)) {
        Ok(()) => 0,
        Err(_) => -1,
    }
}

#[no_mangle]
pub extern "C" fn lex_convert_with_history(
    dict: *const TrieDictionary,
    conn: *const ConnectionMatrix,
    history: *const LexUserHistoryWrapper,
    kana: *const c_char,
) -> LexConversionResult {
    if dict.is_null() || history.is_null() {
        return LexConversionResult::empty();
    }
    let Some(kana_str) = (unsafe { cptr_to_str(kana) }) else {
        return LexConversionResult::empty();
    };
    let dict = unsafe { &*dict };
    let conn = unsafe { conn_ref(conn) };
    let wrapper = unsafe { &*history };
    let Ok(h) = wrapper.inner.read() else {
        return LexConversionResult::empty();
    };

    let cost_fn = LearnedCostFunction::new(conn, &h);
    pack_conversion_result(convert_with_cost(dict, &cost_fn, conn, kana_str))
}

#[no_mangle]
pub extern "C" fn lex_dict_lookup_with_history(
    dict: *const TrieDictionary,
    history: *const LexUserHistoryWrapper,
    reading: *const c_char,
) -> LexCandidateList {
    if dict.is_null() || history.is_null() {
        return LexCandidateList::empty();
    }
    let Some(reading_str) = (unsafe { cptr_to_str(reading) }) else {
        return LexCandidateList::empty();
    };
    let dict = unsafe { &*dict };
    let wrapper = unsafe { &*history };
    let Ok(h) = wrapper.inner.read() else {
        return LexCandidateList::empty();
    };

    match dict.lookup(reading_str) {
        Some(entries) => {
            let reordered = h.reorder_candidates(reading_str, entries);
            LexCandidateList::from_entries(reading_str, &reordered)
        }
        None => LexCandidateList::empty(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    fn make_test_dict() -> *mut TrieDictionary {
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
        Box::into_raw(Box::new(dict))
    }

    #[test]
    fn test_ffi_lookup_roundtrip() {
        let dict = make_test_dict();
        let reading = CString::new("かんじ").unwrap();

        let list = lex_dict_lookup(dict, reading.as_ptr());
        assert_eq!(list.len, 2);

        unsafe {
            let candidates = std::slice::from_raw_parts(list.candidates, list.len as usize);

            // Check reading
            let r = CStr::from_ptr(candidates[0].reading).to_str().unwrap();
            assert_eq!(r, "かんじ");

            // Check surfaces
            let s0 = CStr::from_ptr(candidates[0].surface).to_str().unwrap();
            let s1 = CStr::from_ptr(candidates[1].surface).to_str().unwrap();
            assert_eq!(s0, "漢字");
            assert_eq!(s1, "感じ");

            // Check cost ordering
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
    fn test_ffi_predict_with_reading() {
        let dict = make_test_dict();
        let prefix = CString::new("かん").unwrap();

        let list = lex_dict_predict(dict, prefix.as_ptr(), 100);
        assert!(list.len >= 3); // 漢字, 感じ from かんじ + 感情 from かんじょう

        unsafe {
            let candidates = std::slice::from_raw_parts(list.candidates, list.len as usize);

            // All candidates should have non-null reading
            for c in candidates {
                let r = CStr::from_ptr(c.reading).to_str().unwrap();
                assert!(r.starts_with("かん"));
            }
        }

        lex_candidates_free(list);
        lex_dict_close(dict);
    }

    #[test]
    fn test_ffi_null_safety() {
        // null dict
        let reading = CString::new("かんじ").unwrap();
        let list = lex_dict_lookup(ptr::null(), reading.as_ptr());
        assert_eq!(list.len, 0);
        lex_candidates_free(list);

        // null reading
        let dict = make_test_dict();
        let list = lex_dict_lookup(dict, ptr::null());
        assert_eq!(list.len, 0);
        lex_candidates_free(list);

        // null for predict
        let list = lex_dict_predict(dict, ptr::null(), 10);
        assert_eq!(list.len, 0);
        lex_candidates_free(list);

        lex_dict_close(dict);
    }

    #[test]
    fn test_ffi_open_close_file() {
        let dir = std::env::temp_dir().join("lexime_test_ffi");
        std::fs::create_dir_all(&dir).unwrap();
        let dict_path = dir.join("test.dict");

        // Create a small dict and save it
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

        // Open via FFI
        let path_cstr = CString::new(dict_path.to_str().unwrap()).unwrap();
        let dict_ptr = lex_dict_open(path_cstr.as_ptr());
        assert!(!dict_ptr.is_null());

        // Lookup via FFI
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

        // Cleanup
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_ffi_open_nonexistent() {
        let path = CString::new("/nonexistent/path/dict.bin").unwrap();
        let dict_ptr = lex_dict_open(path.as_ptr());
        assert!(dict_ptr.is_null());
    }

    #[test]
    fn test_ffi_predict_ranked_roundtrip() {
        let dict = make_test_dict();
        let prefix = CString::new("かん").unwrap();

        let list = lex_dict_predict_ranked(dict, ptr::null(), prefix.as_ptr(), 10);
        assert!(list.len >= 3); // 漢字, 感じ from かんじ + 感情 from かんじょう

        unsafe {
            let candidates = std::slice::from_raw_parts(list.candidates, list.len as usize);
            // Should be sorted by cost
            for w in candidates.windows(2) {
                assert!(
                    w[0].cost <= w[1].cost,
                    "predict_ranked FFI should be cost-ordered"
                );
            }
        }

        lex_candidates_free(list);
        lex_dict_close(dict);
    }

    #[test]
    fn test_ffi_predict_ranked_null_safety() {
        let prefix = CString::new("かん").unwrap();

        // null dict
        let list = lex_dict_predict_ranked(ptr::null(), ptr::null(), prefix.as_ptr(), 10);
        assert_eq!(list.len, 0);
        lex_candidates_free(list);

        // null prefix
        let dict = make_test_dict();
        let list = lex_dict_predict_ranked(dict, ptr::null(), ptr::null(), 10);
        assert_eq!(list.len, 0);
        lex_candidates_free(list);

        // null history is OK (pure cost order)
        let list = lex_dict_predict_ranked(dict, ptr::null(), prefix.as_ptr(), 10);
        assert!(list.len >= 1);
        lex_candidates_free(list);

        lex_dict_close(dict);
    }

    fn make_convert_test_dict() -> *mut TrieDictionary {
        let entries = vec![
            (
                "きょう".to_string(),
                vec![dict::DictEntry {
                    surface: "今日".to_string(),
                    cost: 3000,
                    left_id: 0,
                    right_id: 0,
                }],
            ),
            (
                "は".to_string(),
                vec![dict::DictEntry {
                    surface: "は".to_string(),
                    cost: 2000,
                    left_id: 0,
                    right_id: 0,
                }],
            ),
            (
                "いい".to_string(),
                vec![dict::DictEntry {
                    surface: "良い".to_string(),
                    cost: 3500,
                    left_id: 0,
                    right_id: 0,
                }],
            ),
        ];
        let dict = TrieDictionary::from_entries(entries);
        Box::into_raw(Box::new(dict))
    }

    #[test]
    fn test_ffi_convert_roundtrip() {
        let dict = make_convert_test_dict();
        let kana = CString::new("きょうはいい").unwrap();

        let result = lex_convert(dict, ptr::null(), kana.as_ptr());
        assert!(result.len >= 3);

        unsafe {
            let segments = std::slice::from_raw_parts(result.segments, result.len as usize);
            let s0 = CStr::from_ptr(segments[0].surface).to_str().unwrap();
            assert_eq!(s0, "今日");
        }

        lex_conversion_free(result);
        lex_dict_close(dict);
    }

    #[test]
    fn test_ffi_convert_null_safety() {
        let kana = CString::new("きょう").unwrap();

        // null dict
        let result = lex_convert(ptr::null(), ptr::null(), kana.as_ptr());
        assert_eq!(result.len, 0);
        lex_conversion_free(result);

        // null kana
        let dict = make_convert_test_dict();
        let result = lex_convert(dict, ptr::null(), ptr::null());
        assert_eq!(result.len, 0);
        lex_conversion_free(result);
        lex_dict_close(dict);
    }

    #[test]
    fn test_ffi_convert_empty_input() {
        let dict = make_convert_test_dict();
        let kana = CString::new("").unwrap();

        let result = lex_convert(dict, ptr::null(), kana.as_ptr());
        assert_eq!(result.len, 0);

        lex_conversion_free(result);
        lex_dict_close(dict);
    }

    #[test]
    fn test_ffi_conn_null_safety() {
        // null path
        let conn = lex_conn_open(ptr::null());
        assert!(conn.is_null());

        // nonexistent path
        let path = CString::new("/nonexistent/path/conn.bin").unwrap();
        let conn = lex_conn_open(path.as_ptr());
        assert!(conn.is_null());

        // close null
        lex_conn_close(ptr::null_mut());
    }

    #[test]
    fn test_ffi_history_roundtrip() {
        let dir = std::env::temp_dir().join("lexime_test_ffi_history");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.lxud");
        let path_cstr = CString::new(path.to_str().unwrap()).unwrap();

        // Open (creates empty)
        let history = lex_history_open(path_cstr.as_ptr());
        assert!(!history.is_null());

        // Record segments
        let reading = CString::new("きょう").unwrap();
        let surface = CString::new("京").unwrap();
        let seg = LexSegment {
            reading: reading.as_ptr(),
            surface: surface.as_ptr(),
        };
        lex_history_record(history, &seg, 1);

        // Save
        assert_eq!(lex_history_save(history, path_cstr.as_ptr()), 0);
        lex_history_close(history);

        // Reopen and verify boost via lookup_with_history
        let history2 = lex_history_open(path_cstr.as_ptr());
        assert!(!history2.is_null());

        let dict = make_test_dict();
        let reading_lookup = CString::new("かんじ").unwrap();
        let list = lex_dict_lookup_with_history(dict, history2, reading_lookup.as_ptr());
        // Should still return entries (just potentially reordered)
        assert!(list.len >= 1);
        lex_candidates_free(list);

        lex_history_close(history2);
        lex_dict_close(dict);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_ffi_history_save_invalid_path() {
        let dir = std::env::temp_dir().join("lexime_test_ffi_save_fail");
        std::fs::create_dir_all(&dir).unwrap();
        let open_path = dir.join("temp.lxud");
        let open_cstr = CString::new(open_path.to_str().unwrap()).unwrap();

        let history = lex_history_open(open_cstr.as_ptr());
        assert!(!history.is_null());

        // Save to an invalid path should return -1
        let bad_path = CString::new("/nonexistent/deeply/nested/history.lxud").unwrap();
        assert_eq!(lex_history_save(history, bad_path.as_ptr()), -1);

        lex_history_close(history);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_ffi_convert_nbest_roundtrip() {
        let dict = make_convert_test_dict();
        let kana = CString::new("きょうはいい").unwrap();

        let list = lex_convert_nbest(dict, ptr::null(), kana.as_ptr(), 5);
        assert!(list.len >= 1, "should return at least 1 result");

        unsafe {
            let results = std::slice::from_raw_parts(list.results, list.len as usize);
            // First result should match 1-best
            assert!(results[0].len >= 3);
            let segments = std::slice::from_raw_parts(results[0].segments, results[0].len as usize);
            let s0 = CStr::from_ptr(segments[0].surface).to_str().unwrap();
            assert_eq!(s0, "今日");
        }

        lex_conversion_result_list_free(list);
        lex_dict_close(dict);
    }

    #[test]
    fn test_ffi_convert_nbest_null_safety() {
        let kana = CString::new("きょう").unwrap();

        // null dict
        let list = lex_convert_nbest(ptr::null(), ptr::null(), kana.as_ptr(), 5);
        assert_eq!(list.len, 0);
        lex_conversion_result_list_free(list);

        // null kana
        let dict = make_convert_test_dict();
        let list = lex_convert_nbest(dict, ptr::null(), ptr::null(), 5);
        assert_eq!(list.len, 0);
        lex_conversion_result_list_free(list);

        // n = 0
        let list = lex_convert_nbest(dict, ptr::null(), kana.as_ptr(), 0);
        assert_eq!(list.len, 0);
        lex_conversion_result_list_free(list);

        lex_dict_close(dict);
    }
}
