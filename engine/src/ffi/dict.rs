use std::ffi::{c_char, CString};
use std::ptr;

use super::{ffi_close, ffi_guard, ffi_open, owned_drop, OwnedVec};
use crate::dict::{self, Dictionary, TrieDictionary};

use super::history::LexUserHistoryWrapper;

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
    pub(crate) _owned: *mut OwnedVec<LexCandidate>,
}

impl LexCandidateList {
    pub(crate) fn empty() -> Self {
        Self {
            candidates: ptr::null(),
            len: 0,
            _owned: ptr::null_mut(),
        }
    }

    pub(crate) fn from_entries(reading: &str, entries: &[dict::DictEntry]) -> Self {
        let Ok(reading_cstr) = CString::new(reading) else {
            return Self::empty();
        };
        let reading_ptr = reading_cstr.as_ptr();

        let mut strings = Vec::with_capacity(entries.len() + 1);
        let mut candidates = Vec::with_capacity(entries.len());

        strings.push(reading_cstr);

        for entry in entries {
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

        Self::pack(candidates, strings)
    }

    pub(crate) fn from_flat_entries(pairs: &[(String, dict::DictEntry)]) -> Self {
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

    pub(crate) fn from_search_results(results: Vec<dict::SearchResult<'_>>) -> Self {
        let mut strings = Vec::new();
        let mut candidates = Vec::new();

        for result in &results {
            let Ok(reading_cstr) = CString::new(result.reading.as_str()) else {
                continue;
            };
            let reading_ptr = reading_cstr.as_ptr();
            strings.push(reading_cstr);

            for entry in result.entries {
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
        let (ptr, len, owned) = OwnedVec::pack(candidates, strings);
        if owned.is_null() {
            return Self::empty();
        }
        Self {
            candidates: ptr,
            len,
            _owned: owned,
        }
    }
}

ffi_open!(lex_dict_open, TrieDictionary, |p| TrieDictionary::open(p));
ffi_close!(lex_dict_close, TrieDictionary);

#[no_mangle]
pub extern "C" fn lex_dict_lookup(
    dict: *const TrieDictionary,
    reading: *const c_char,
) -> LexCandidateList {
    ffi_guard!(LexCandidateList::empty();
        ref: dict        = dict,
        str: reading_str = reading,
    );

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
    ffi_guard!(LexCandidateList::empty();
        ref: dict       = dict,
        str: prefix_str = prefix,
    );

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
    ffi_guard!(LexCandidateList::empty();
        ref: dict       = dict,
        str: prefix_str = prefix,
    );

    let fetch_limit = if history.is_null() {
        max_results as usize
    } else {
        (max_results as usize).max(200)
    };
    let mut ranked = dict.predict_ranked(prefix_str, fetch_limit, 1000);

    if !history.is_null() {
        let wrapper = unsafe { &*history };
        if let Ok(h) = wrapper.inner.read() {
            ranked.sort_by(|(r_a, e_a), (r_b, e_b)| {
                let boost_a = h.unigram_boost(r_a, &e_a.surface);
                let boost_b = h.unigram_boost(r_b, &e_b.surface);
                boost_b.cmp(&boost_a).then(e_a.cost.cmp(&e_b.cost))
            });
        }
        ranked.truncate(max_results as usize);
    }

    LexCandidateList::from_flat_entries(&ranked)
}

#[no_mangle]
pub extern "C" fn lex_candidates_free(list: LexCandidateList) {
    unsafe { owned_drop(list._owned) };
}

#[no_mangle]
pub extern "C" fn lex_dict_lookup_with_history(
    dict: *const TrieDictionary,
    history: *const LexUserHistoryWrapper,
    reading: *const c_char,
) -> LexCandidateList {
    ffi_guard!(LexCandidateList::empty();
        ref: dict        = dict,
        ref: wrapper     = history,
        str: reading_str = reading,
    );
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
