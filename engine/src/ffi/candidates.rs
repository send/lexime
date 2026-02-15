use std::ffi::{c_char, CString};
use std::ptr;

use super::{conn_ref, ffi_guard, owned_drop};
use crate::candidates::CandidateResponse;
use crate::candidates::{generate_candidates, generate_prediction_candidates};
use crate::dict::connection::ConnectionMatrix;
use crate::dict::TrieDictionary;
use crate::user_history::UserHistory;

use super::convert::{pack_conversion_result, LexConversionResult};
use super::history::LexUserHistoryWrapper;

// --- Unified Candidate Generation FFI ---

#[repr(C)]
pub struct LexCandidateResponse {
    pub surfaces: *const *const c_char,
    pub surfaces_len: u32,
    pub paths: *const LexConversionResult,
    pub paths_len: u32,
    pub(crate) _owned: *mut OwnedCandidateResponse,
}

pub(crate) struct OwnedCandidateResponse {
    pub(crate) _surface_ptrs: Vec<*const c_char>,
    _surface_strings: Vec<CString>,
    pub(crate) _paths: Vec<LexConversionResult>,
}

impl LexCandidateResponse {
    pub(crate) fn empty() -> Self {
        Self {
            surfaces: ptr::null(),
            surfaces_len: 0,
            paths: ptr::null(),
            paths_len: 0,
            _owned: ptr::null_mut(),
        }
    }
}

pub(crate) fn pack_candidate_response(resp: CandidateResponse) -> LexCandidateResponse {
    let mut surface_strings: Vec<CString> = Vec::new();
    let mut surface_ptrs: Vec<*const c_char> = Vec::new();

    for s in &resp.surfaces {
        let Ok(cs) = CString::new(s.as_str()) else {
            continue;
        };
        surface_ptrs.push(cs.as_ptr());
        surface_strings.push(cs);
    }

    let paths: Vec<LexConversionResult> =
        resp.paths.into_iter().map(pack_conversion_result).collect();

    let owned = Box::new(OwnedCandidateResponse {
        _surface_ptrs: surface_ptrs,
        _surface_strings: surface_strings,
        _paths: paths,
    });
    let owned_ptr = Box::into_raw(owned);

    let (surfaces_ptr, surfaces_len) = unsafe {
        let ptrs = &(*owned_ptr)._surface_ptrs;
        if ptrs.is_empty() {
            (ptr::null(), 0)
        } else {
            (ptrs.as_ptr(), ptrs.len() as u32)
        }
    };
    let (paths_ptr, paths_len) = unsafe {
        let p = &(*owned_ptr)._paths;
        if p.is_empty() {
            (ptr::null(), 0)
        } else {
            (p.as_ptr(), p.len() as u32)
        }
    };

    LexCandidateResponse {
        surfaces: surfaces_ptr,
        surfaces_len,
        paths: paths_ptr,
        paths_len,
        _owned: owned_ptr,
    }
}

#[no_mangle]
pub extern "C" fn lex_generate_candidates(
    dict: *const TrieDictionary,
    conn: *const ConnectionMatrix,
    history: *const LexUserHistoryWrapper,
    reading: *const c_char,
    max_results: u32,
) -> LexCandidateResponse {
    ffi_guard!(LexCandidateResponse::empty();
        ref: dict        = dict,
        str: reading_str = reading,
    );
    let conn = unsafe { conn_ref(conn) };
    let hist: Option<std::sync::RwLockReadGuard<'_, UserHistory>> = if history.is_null() {
        None
    } else {
        let wrapper = unsafe { &*history };
        wrapper.inner.read().ok()
    };
    let hist_ref = hist.as_deref();

    let resp = generate_candidates(dict, conn, hist_ref, reading_str, max_results as usize);
    pack_candidate_response(resp)
}

#[no_mangle]
pub extern "C" fn lex_generate_prediction_candidates(
    dict: *const TrieDictionary,
    conn: *const ConnectionMatrix,
    history: *const LexUserHistoryWrapper,
    reading: *const c_char,
    max_results: u32,
) -> LexCandidateResponse {
    ffi_guard!(LexCandidateResponse::empty();
        ref: dict        = dict,
        str: reading_str = reading,
    );
    let conn = unsafe { conn_ref(conn) };
    let hist: Option<std::sync::RwLockReadGuard<'_, UserHistory>> = if history.is_null() {
        None
    } else {
        let wrapper = unsafe { &*history };
        wrapper.inner.read().ok()
    };
    let hist_ref = hist.as_deref();

    let resp =
        generate_prediction_candidates(dict, conn, hist_ref, reading_str, max_results as usize);
    pack_candidate_response(resp)
}

#[no_mangle]
pub extern "C" fn lex_candidate_response_free(response: LexCandidateResponse) {
    if !response._owned.is_null() {
        unsafe {
            let mut owned = Box::from_raw(response._owned);
            for path in &mut owned._paths {
                owned_drop(path._owned);
                path._owned = std::ptr::null_mut();
            }
        }
    }
}
