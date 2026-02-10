// FFI functions perform null checks before dereferencing raw pointers.
// Clippy cannot verify this statically, so we allow it at crate level.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

pub mod dict;

use std::ffi::{c_char, CStr, CString};
use std::path::Path;
use std::ptr;

use dict::{Dictionary, TrieDictionary};

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
        let reading_cstr = CString::new(reading).unwrap_or_default();
        let reading_ptr = reading_cstr.as_ptr();

        let mut strings = Vec::with_capacity(entries.len() + 1);
        let mut candidates = Vec::with_capacity(entries.len());

        strings.push(reading_cstr);

        for entry in entries {
            let surface = CString::new(entry.surface.as_str()).unwrap_or_default();
            candidates.push(LexCandidate {
                reading: reading_ptr,
                surface: surface.as_ptr(),
                cost: entry.cost,
            });
            strings.push(surface);
        }

        Self::pack(candidates, strings)
    }

    fn from_search_results(results: Vec<dict::SearchResult>) -> Self {
        let mut strings = Vec::new();
        let mut candidates = Vec::new();

        for result in &results {
            let reading_cstr = CString::new(result.reading.as_str()).unwrap_or_default();
            let reading_ptr = reading_cstr.as_ptr();
            strings.push(reading_cstr);

            for entry in &result.entries {
                let surface = CString::new(entry.surface.as_str()).unwrap_or_default();
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

        // Safety: owned_ptr is valid and its candidates vec won't move
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
    if path.is_null() {
        return ptr::null_mut();
    }
    let path_str = unsafe { CStr::from_ptr(path) };
    let path_str = match path_str.to_str() {
        Ok(s) => s,
        Err(_) => return ptr::null_mut(),
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
    if dict.is_null() || reading.is_null() {
        return LexCandidateList::empty();
    }
    let dict = unsafe { &*dict };
    let reading_str = unsafe { CStr::from_ptr(reading) };
    let reading_str = match reading_str.to_str() {
        Ok(s) => s,
        Err(_) => return LexCandidateList::empty(),
    };

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
    if dict.is_null() || prefix.is_null() {
        return LexCandidateList::empty();
    }
    let dict = unsafe { &*dict };
    let prefix_str = unsafe { CStr::from_ptr(prefix) };
    let prefix_str = match prefix_str.to_str() {
        Ok(s) => s,
        Err(_) => return LexCandidateList::empty(),
    };

    let results = dict.predict(prefix_str, max_results as usize);
    LexCandidateList::from_search_results(results)
}

#[no_mangle]
pub extern "C" fn lex_candidates_free(list: LexCandidateList) {
    if !list._owned.is_null() {
        unsafe {
            drop(Box::from_raw(list._owned));
        }
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
}
