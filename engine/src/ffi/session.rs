use std::ffi::{c_char, CStr, CString};
use std::ptr;

use super::{cptr_to_str, ffi_close, owned_new};
use crate::converter;
use crate::dict::connection::ConnectionMatrix;
use crate::dict::TrieDictionary;
use crate::session::{self, CandidateAction, InputSession, KeyResponse};
use crate::user_history::UserHistory;

use super::candidates::LexCandidateResponse;
use super::history::LexUserHistoryWrapper;

// --- InputSession FFI ---

/// Opaque wrapper to hold InputSession with raw pointers for FFI lifetime.
///
/// The caller guarantees dict/conn/history outlive the session.
pub struct LexSession {
    inner: InputSession<'static>,
    history_ptr: *const LexUserHistoryWrapper,
}

#[repr(C)]
pub struct LexKeyResponse {
    pub consumed: u8,
    pub commit_text: *const c_char,
    pub marked_text: *const c_char,
    pub is_dashed_underline: u8,
    pub candidates: *const *const c_char,
    pub candidates_len: u32,
    pub selected_index: u32,
    pub show_candidates: u8,
    pub hide_candidates: u8,
    pub switch_to_abc: u8,
    pub save_history: u8,
    pub needs_candidates: u8,
    pub candidate_reading: *const c_char,
    pub candidate_dispatch: u8,
    /// Ghost text: NULL=no change, ""=clear, string=show
    pub ghost_text: *const c_char,
    /// 1 = caller should generate ghost text async
    pub needs_ghost_text: u8,
    /// Context for ghost text generation (valid when needs_ghost_text=1)
    pub ghost_context: *const c_char,
    /// Generation counter for staleness check
    pub ghost_generation: u64,
    pub(crate) _owned: *mut OwnedKeyResponse,
}

pub(crate) struct OwnedKeyResponse {
    _commit_text: Option<CString>,
    _marked_text: Option<CString>,
    _candidate_ptrs: Vec<*const c_char>,
    _candidate_strings: Vec<CString>,
    _candidate_reading: Option<CString>,
    _ghost_text: Option<CString>,
    _ghost_context: Option<CString>,
    /// History records to be fed to UserHistory::record().
    pub(crate) history_records: Vec<Vec<(String, String)>>,
}

impl LexKeyResponse {
    pub(crate) fn empty() -> Self {
        Self {
            consumed: 0,
            commit_text: ptr::null(),
            marked_text: ptr::null(),
            is_dashed_underline: 0,
            candidates: ptr::null(),
            candidates_len: 0,
            selected_index: 0,
            show_candidates: 0,
            hide_candidates: 0,
            switch_to_abc: 0,
            save_history: 0,
            needs_candidates: 0,
            candidate_reading: ptr::null(),
            candidate_dispatch: 0,
            ghost_text: ptr::null(),
            needs_ghost_text: 0,
            ghost_context: ptr::null(),
            ghost_generation: 0,
            _owned: ptr::null_mut(),
        }
    }
}

pub(crate) fn pack_key_response(
    resp: KeyResponse,
    history_records: Vec<Vec<(String, String)>>,
) -> LexKeyResponse {
    let commit_cstr = resp.commit.and_then(|s| CString::new(s).ok());
    let is_dashed = resp.marked.as_ref().is_some_and(|m| m.dashed);
    let marked_cstr = resp.marked.and_then(|m| CString::new(m.text).ok());

    let (show, hide) = match &resp.candidates {
        CandidateAction::Keep => (false, false),
        CandidateAction::Show { .. } => (true, false),
        CandidateAction::Hide => (false, true),
    };

    let mut candidate_strings: Vec<CString> = Vec::new();
    let mut candidate_ptrs: Vec<*const c_char> = Vec::new();
    let selected_index = match &resp.candidates {
        CandidateAction::Show { surfaces, selected } => {
            for s in surfaces {
                if let Ok(cs) = CString::new(s.as_str()) {
                    candidate_ptrs.push(cs.as_ptr());
                    candidate_strings.push(cs);
                }
            }
            *selected
        }
        _ => 0,
    };

    let (needs_candidates, reading_cstr, candidate_dispatch) = match resp.async_request {
        Some(req) => (true, CString::new(req.reading).ok(), req.candidate_dispatch),
        None => (false, None, 0),
    };

    let ghost_text_cstr = resp.ghost_text.and_then(|s| CString::new(s).ok());
    let (needs_ghost, ghost_ctx_cstr, ghost_gen) = match resp.ghost_request {
        Some(req) => (true, CString::new(req.context).ok(), req.generation),
        None => (false, None, 0),
    };

    let owned = Box::new(OwnedKeyResponse {
        _commit_text: commit_cstr,
        _marked_text: marked_cstr,
        _candidate_ptrs: candidate_ptrs,
        _candidate_strings: candidate_strings,
        _candidate_reading: reading_cstr,
        _ghost_text: ghost_text_cstr,
        _ghost_context: ghost_ctx_cstr,
        history_records,
    });
    let owned_ptr = Box::into_raw(owned);

    let commit_ptr = unsafe {
        (*owned_ptr)
            ._commit_text
            .as_ref()
            .map_or(ptr::null(), |cs| cs.as_ptr())
    };
    let marked_ptr = unsafe {
        (*owned_ptr)
            ._marked_text
            .as_ref()
            .map_or(ptr::null(), |cs| cs.as_ptr())
    };
    let reading_ptr = unsafe {
        (*owned_ptr)
            ._candidate_reading
            .as_ref()
            .map_or(ptr::null(), |cs| cs.as_ptr())
    };
    let (cand_ptr, cand_len) = unsafe {
        let ptrs = &(*owned_ptr)._candidate_ptrs;
        if ptrs.is_empty() {
            (ptr::null(), 0)
        } else {
            (ptrs.as_ptr(), ptrs.len() as u32)
        }
    };
    let ghost_text_ptr = unsafe {
        (*owned_ptr)
            ._ghost_text
            .as_ref()
            .map_or(ptr::null(), |cs| cs.as_ptr())
    };
    let ghost_ctx_ptr = unsafe {
        (*owned_ptr)
            ._ghost_context
            .as_ref()
            .map_or(ptr::null(), |cs| cs.as_ptr())
    };

    LexKeyResponse {
        consumed: resp.consumed as u8,
        commit_text: commit_ptr,
        marked_text: marked_ptr,
        is_dashed_underline: is_dashed as u8,
        candidates: cand_ptr,
        candidates_len: cand_len,
        selected_index,
        show_candidates: show as u8,
        hide_candidates: hide as u8,
        switch_to_abc: resp.side_effects.switch_to_abc as u8,
        save_history: resp.side_effects.save_history as u8,
        needs_candidates: needs_candidates as u8,
        candidate_reading: reading_ptr,
        candidate_dispatch,
        ghost_text: ghost_text_ptr,
        needs_ghost_text: needs_ghost as u8,
        ghost_context: ghost_ctx_ptr,
        ghost_generation: ghost_gen,
        _owned: owned_ptr,
    }
}

#[no_mangle]
pub extern "C" fn lex_session_new(
    dict: *const TrieDictionary,
    conn: *const ConnectionMatrix,
    history: *const LexUserHistoryWrapper,
) -> *mut LexSession {
    if dict.is_null() {
        return ptr::null_mut();
    }
    let dict_ref: &'static TrieDictionary = unsafe { &*dict };
    let conn_ref: Option<&'static ConnectionMatrix> = if conn.is_null() {
        None
    } else {
        Some(unsafe { &*conn })
    };

    let inner = InputSession::new(dict_ref, conn_ref, None);

    owned_new(LexSession {
        inner,
        history_ptr: history,
    })
}

ffi_close!(lex_session_free, LexSession);

#[no_mangle]
pub extern "C" fn lex_session_set_programmer_mode(session: *mut LexSession, enabled: u8) {
    if session.is_null() {
        return;
    }
    let session = unsafe { &mut *session };
    session.inner.set_programmer_mode(enabled != 0);
}

#[no_mangle]
pub extern "C" fn lex_session_set_defer_candidates(session: *mut LexSession, enabled: u8) {
    if session.is_null() {
        return;
    }
    let session = unsafe { &mut *session };
    session.inner.set_defer_candidates(enabled != 0);
}

/// Set the conversion mode. mode: 0=Standard, 1=Predictive, 2=GhostText.
#[no_mangle]
pub extern "C" fn lex_session_set_conversion_mode(session: *mut LexSession, mode: u8) {
    if session.is_null() {
        return;
    }
    let session = unsafe { &mut *session };
    let conversion_mode = match mode {
        1 => session::ConversionMode::Predictive,
        2 => session::ConversionMode::GhostText,
        _ => session::ConversionMode::Standard,
    };
    session.inner.set_conversion_mode(conversion_mode);
}

/// Receive asynchronously generated candidates and update session state.
#[no_mangle]
pub extern "C" fn lex_session_receive_candidates(
    session: *mut LexSession,
    reading: *const c_char,
    candidates: *const LexCandidateResponse,
) -> LexKeyResponse {
    if session.is_null() || candidates.is_null() {
        return LexKeyResponse::empty();
    }
    let reading_str = unsafe { cptr_to_str(reading) }.unwrap_or("");
    let session = unsafe { &mut *session };
    let cand_resp = unsafe { &*candidates };

    let surfaces = unpack_candidate_surfaces(cand_resp);
    let paths = unpack_candidate_paths(cand_resp);

    let resp = unsafe {
        with_history(session.history_ptr, &mut session.inner, |inner| {
            inner.receive_candidates(reading_str, surfaces, paths)
        })
    };
    match resp {
        Some(resp) => {
            let records = session.inner.take_history_records();
            pack_key_response(resp, records)
        }
        None => LexKeyResponse::empty(),
    }
}

fn unpack_candidate_surfaces(resp: &LexCandidateResponse) -> Vec<String> {
    let mut result = Vec::new();
    if resp.surfaces.is_null() || resp.surfaces_len == 0 {
        return result;
    }
    for i in 0..resp.surfaces_len as usize {
        let ptr = unsafe { *resp.surfaces.add(i) };
        if !ptr.is_null() {
            if let Ok(s) = unsafe { CStr::from_ptr(ptr) }.to_str() {
                result.push(s.to_owned());
            }
        }
    }
    result
}

fn unpack_candidate_paths(resp: &LexCandidateResponse) -> Vec<Vec<converter::ConvertedSegment>> {
    let mut result = Vec::new();
    if resp.paths.is_null() || resp.paths_len == 0 {
        return result;
    }
    for i in 0..resp.paths_len as usize {
        let path_result = unsafe { &*resp.paths.add(i) };
        let mut segments = Vec::new();
        if !path_result.segments.is_null() && path_result.len > 0 {
            for j in 0..path_result.len as usize {
                let seg = unsafe { &*path_result.segments.add(j) };
                if !seg.reading.is_null() && !seg.surface.is_null() {
                    if let (Ok(r), Ok(s)) = (
                        unsafe { CStr::from_ptr(seg.reading) }.to_str(),
                        unsafe { CStr::from_ptr(seg.surface) }.to_str(),
                    ) {
                        segments.push(converter::ConvertedSegment {
                            reading: r.to_owned(),
                            surface: s.to_owned(),
                        });
                    }
                }
            }
        }
        result.push(segments);
    }
    result
}

/// Run a closure with the user-history reference temporarily set on the session.
///
/// # Safety
/// `history_ptr` must be null or point to a valid `LexUserHistoryWrapper` that outlives
/// this call.
unsafe fn with_history<F, R>(
    history_ptr: *const LexUserHistoryWrapper,
    inner: &mut InputSession<'static>,
    f: F,
) -> R
where
    F: FnOnce(&mut InputSession<'static>) -> R,
{
    let _guard: Option<std::sync::RwLockReadGuard<'static, UserHistory>> = if history_ptr.is_null()
    {
        inner.set_history(None);
        None
    } else {
        let wrapper = &*history_ptr;
        match wrapper.inner.read() {
            Ok(guard) => {
                let hist_ref: &UserHistory = &guard;
                let hist_static: &'static UserHistory = std::mem::transmute(hist_ref);
                inner.set_history(Some(hist_static));
                let guard: std::sync::RwLockReadGuard<'static, UserHistory> =
                    std::mem::transmute(guard);
                Some(guard)
            }
            Err(_) => {
                inner.set_history(None);
                None
            }
        }
    };

    let result = f(inner);
    inner.set_history(None);
    result
}

#[no_mangle]
pub extern "C" fn lex_session_handle_key(
    session: *mut LexSession,
    key_code: u16,
    text: *const c_char,
    flags: u8,
) -> LexKeyResponse {
    if session.is_null() {
        return LexKeyResponse::empty();
    }
    let session = unsafe { &mut *session };
    let text_str = unsafe { cptr_to_str(text) }.unwrap_or("");
    let resp = unsafe {
        with_history(session.history_ptr, &mut session.inner, |inner| {
            inner.handle_key(key_code, text_str, flags)
        })
    };
    let records = session.inner.take_history_records();
    pack_key_response(resp, records)
}

#[no_mangle]
pub extern "C" fn lex_session_commit(session: *mut LexSession) -> LexKeyResponse {
    if session.is_null() {
        return LexKeyResponse::empty();
    }
    let session = unsafe { &mut *session };
    let resp = unsafe {
        with_history(session.history_ptr, &mut session.inner, |inner| {
            inner.commit()
        })
    };
    let records = session.inner.take_history_records();
    pack_key_response(resp, records)
}

#[no_mangle]
pub extern "C" fn lex_session_is_composing(session: *const LexSession) -> u8 {
    if session.is_null() {
        return 0;
    }
    let session = unsafe { &*session };
    session.inner.is_composing() as u8
}

/// Get the composed string for IMKit's composedString callback.
#[no_mangle]
pub extern "C" fn lex_session_composed_string(_session: *const LexSession) -> *const c_char {
    c"".as_ptr()
}

/// Get the committed context string for neural candidate generation.
/// Returns the concatenated surfaces of recently committed segments.
/// Returns null if the context is empty.
/// The caller must free the returned string with `lex_committed_context_free`.
#[no_mangle]
pub extern "C" fn lex_session_committed_context(session: *const LexSession) -> *mut c_char {
    if session.is_null() {
        return ptr::null_mut();
    }
    let session = unsafe { &*session };
    let context = session.inner.committed_context();
    if context.is_empty() {
        return ptr::null_mut();
    }
    match CString::new(context) {
        Ok(cs) => cs.into_raw(),
        Err(_) => ptr::null_mut(),
    }
}

/// Free a string returned by `lex_session_committed_context`.
/// No-op if ptr is null.
#[no_mangle]
pub extern "C" fn lex_committed_context_free(ptr: *mut c_char) {
    if ptr.is_null() {
        return;
    }
    unsafe {
        drop(CString::from_raw(ptr));
    }
}

#[no_mangle]
pub extern "C" fn lex_key_response_free(response: LexKeyResponse) {
    if !response._owned.is_null() {
        unsafe {
            drop(Box::from_raw(response._owned));
        }
    }
}

/// Get the history records from the last key response.
#[no_mangle]
pub extern "C" fn lex_key_response_history_count(response: *const LexKeyResponse) -> u32 {
    if response.is_null() {
        return 0;
    }
    let resp = unsafe { &*response };
    if resp._owned.is_null() {
        return 0;
    }
    let owned = unsafe { &*resp._owned };
    owned.history_records.len() as u32
}

// --- Ghost text session FFI ---

/// Receive async ghost text and update session state.
#[no_mangle]
pub extern "C" fn lex_session_receive_ghost_text(
    session: *mut LexSession,
    generation: u64,
    text: *const c_char,
) -> LexKeyResponse {
    if session.is_null() {
        return LexKeyResponse::empty();
    }
    let text_str = unsafe { cptr_to_str(text) }.unwrap_or("");
    let session = unsafe { &mut *session };
    match session
        .inner
        .receive_ghost_text(generation, text_str.to_string())
    {
        Some(resp) => pack_key_response(resp, Vec::new()),
        None => LexKeyResponse::empty(),
    }
}

/// Get the current ghost generation counter (for staleness checks).
#[no_mangle]
pub extern "C" fn lex_session_ghost_generation(session: *const LexSession) -> u64 {
    if session.is_null() {
        return 0;
    }
    let session = unsafe { &*session };
    session.inner.ghost_generation()
}

/// Record history entries from a key response into the user history.
#[no_mangle]
pub extern "C" fn lex_key_response_record_history(
    response: *const LexKeyResponse,
    history: *const LexUserHistoryWrapper,
) {
    if response.is_null() || history.is_null() {
        return;
    }
    let resp = unsafe { &*response };
    if resp._owned.is_null() {
        return;
    }
    let owned = unsafe { &*resp._owned };
    let wrapper = unsafe { &*history };
    if let Ok(mut h) = wrapper.inner.write() {
        for records in &owned.history_records {
            h.record(records);
        }
    }
}
