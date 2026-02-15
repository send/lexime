use std::ffi::{c_char, CString};
use std::ptr;

use super::{ffi_close, ffi_guard, ffi_open, owned_drop, OwnedVec};
use crate::dict::{self, Dictionary, TrieDictionary};

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
        // SAFETY: CString stores its data on the heap. Taking a pointer here and
        // then moving the CString into `strings` is safe because Vec::push does
        // not invalidate the CString's internal heap buffer.
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
pub extern "C" fn lex_candidates_free(list: LexCandidateList) {
    unsafe { owned_drop(list._owned) };
}
