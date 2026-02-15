use std::ffi::{c_char, CString};
use std::ptr;

use super::{conn_ref, ffi_guard, owned_drop, OwnedVec};
use crate::converter::{self, convert, convert_nbest, convert_nbest_with_history};
use crate::dict::connection::ConnectionMatrix;
use crate::dict::TrieDictionary;

use super::history::LexUserHistoryWrapper;

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
    pub(crate) _owned: *mut OwnedVec<LexSegment>,
}

impl LexConversionResult {
    pub(crate) fn empty() -> Self {
        Self {
            segments: ptr::null(),
            len: 0,
            _owned: ptr::null_mut(),
        }
    }
}

/// Pack a list of ConvertedSegments into a C-compatible LexConversionResult.
pub(crate) fn pack_conversion_result(
    result: Vec<converter::ConvertedSegment>,
) -> LexConversionResult {
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

    let (ptr, len, owned) = OwnedVec::pack(segments, strings);
    if owned.is_null() {
        return LexConversionResult::empty();
    }
    LexConversionResult {
        segments: ptr,
        len,
        _owned: owned,
    }
}

#[no_mangle]
pub extern "C" fn lex_convert(
    dict: *const TrieDictionary,
    conn: *const ConnectionMatrix,
    kana: *const c_char,
) -> LexConversionResult {
    ffi_guard!(LexConversionResult::empty();
        ref: dict     = dict,
        str: kana_str = kana,
    );
    let conn = unsafe { conn_ref(conn) };

    pack_conversion_result(convert(dict, conn, kana_str))
}

#[no_mangle]
pub extern "C" fn lex_conversion_free(result: LexConversionResult) {
    unsafe { owned_drop(result._owned) };
}

// --- N-best Conversion FFI ---

#[repr(C)]
pub struct LexConversionResultList {
    pub results: *const LexConversionResult,
    pub len: u32,
    pub(crate) _owned: *mut OwnedVec<LexConversionResult>,
}

impl LexConversionResultList {
    pub(crate) fn empty() -> Self {
        Self {
            results: ptr::null(),
            len: 0,
            _owned: ptr::null_mut(),
        }
    }
}

pub(crate) fn pack_conversion_result_list(
    paths: Vec<Vec<converter::ConvertedSegment>>,
) -> LexConversionResultList {
    let results: Vec<LexConversionResult> = paths.into_iter().map(pack_conversion_result).collect();
    let (ptr, len, owned) = OwnedVec::pack(results, Vec::new());
    if owned.is_null() {
        return LexConversionResultList::empty();
    }
    LexConversionResultList {
        results: ptr,
        len,
        _owned: owned,
    }
}

#[no_mangle]
pub extern "C" fn lex_convert_nbest(
    dict: *const TrieDictionary,
    conn: *const ConnectionMatrix,
    kana: *const c_char,
    n: u32,
) -> LexConversionResultList {
    ffi_guard!(LexConversionResultList::empty();
        ref: dict     = dict,
        str: kana_str = kana,
    );
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
    ffi_guard!(LexConversionResultList::empty();
        ref:     dict    = dict,
        nonnull:           history,
        str:     kana_str = kana,
    );
    let conn = unsafe { conn_ref(conn) };
    let wrapper = unsafe { &*history };
    let Ok(h) = wrapper.inner.read() else {
        return LexConversionResultList::empty();
    };

    pack_conversion_result_list(convert_nbest_with_history(
        dict, conn, &h, kana_str, n as usize,
    ))
}

#[no_mangle]
pub extern "C" fn lex_conversion_result_list_free(list: LexConversionResultList) {
    if !list._owned.is_null() {
        unsafe {
            let owned = Box::from_raw(list._owned);
            for result in &owned.items {
                owned_drop(result._owned);
            }
        }
    }
}
