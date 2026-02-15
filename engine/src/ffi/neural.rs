use std::ffi::{c_char, CString};
use std::ptr;

use crate::dict::connection::ConnectionMatrix;
use crate::dict::TrieDictionary;

use super::candidates::LexCandidateResponse;
use super::history::LexUserHistoryWrapper;

#[cfg(feature = "neural")]
mod inner {
    use super::*;
    use std::sync::Mutex;

    use crate::ffi::candidates::pack_candidate_response;
    use crate::ffi::{conn_ref, cptr_to_str, ffi_close, ffi_guard, owned_new};
    use crate::user_history::UserHistory;

    pub struct LexNeuralScorer {
        inner: Mutex<crate::neural::NeuralScorer>,
    }

    #[no_mangle]
    #[must_use]
    pub extern "C" fn lex_neural_open(model_path: *const c_char) -> *mut LexNeuralScorer {
        ffi_guard!(ptr::null_mut() ; str: path_str = model_path ,);
        match crate::neural::NeuralScorer::open(std::path::Path::new(path_str)) {
            Ok(scorer) => owned_new(LexNeuralScorer {
                inner: Mutex::new(scorer),
            }),
            Err(_) => ptr::null_mut(),
        }
    }

    ffi_close!(lex_neural_close, LexNeuralScorer);

    /// Ghost text result. Caller must free with lex_ghost_text_free.
    #[repr(C)]
    pub struct LexGhostTextResult {
        pub text: *const c_char,
        _owned: *mut CString,
    }

    impl LexGhostTextResult {
        fn empty() -> Self {
            Self {
                text: ptr::null(),
                _owned: ptr::null_mut(),
            }
        }
    }

    /// Generate ghost text (called from background thread).
    #[no_mangle]
    pub extern "C" fn lex_neural_generate_ghost(
        scorer: *mut LexNeuralScorer,
        context: *const c_char,
        max_tokens: u32,
    ) -> LexGhostTextResult {
        if scorer.is_null() {
            return LexGhostTextResult::empty();
        }
        let context_str = unsafe { cptr_to_str(context) }.unwrap_or("");
        let scorer_wrapper = unsafe { &*scorer };
        let Ok(mut guard) = scorer_wrapper.inner.lock() else {
            return LexGhostTextResult::empty();
        };
        let config = crate::neural::GenerateConfig {
            max_tokens: max_tokens as usize,
            ..crate::neural::GenerateConfig::default()
        };
        match guard.generate_text(context_str, &config) {
            Ok(text) => {
                let Ok(cs) = CString::new(text) else {
                    return LexGhostTextResult::empty();
                };
                let ptr = cs.as_ptr();
                let owned = Box::into_raw(Box::new(cs));
                LexGhostTextResult {
                    text: ptr,
                    _owned: owned,
                }
            }
            Err(_) => LexGhostTextResult::empty(),
        }
    }

    #[no_mangle]
    pub extern "C" fn lex_ghost_text_free(result: LexGhostTextResult) {
        if !result._owned.is_null() {
            unsafe {
                drop(Box::from_raw(result._owned));
            }
        }
    }

    /// Generate neural candidates (called from background thread, dispatch=2).
    #[no_mangle]
    pub extern "C" fn lex_generate_neural_candidates(
        scorer: *mut LexNeuralScorer,
        dict: *const TrieDictionary,
        conn: *const ConnectionMatrix,
        history: *const LexUserHistoryWrapper,
        context: *const c_char,
        reading: *const c_char,
        max_results: u32,
    ) -> LexCandidateResponse {
        if scorer.is_null() || dict.is_null() {
            return LexCandidateResponse::empty();
        }
        let reading_str = unsafe { cptr_to_str(reading) }.unwrap_or("");
        let context_str = unsafe { cptr_to_str(context) }.unwrap_or("");
        let dict_ref = unsafe { &*dict };
        let conn_opt = unsafe { conn_ref(conn) };
        let scorer_wrapper = unsafe { &*scorer };
        let Ok(mut guard) = scorer_wrapper.inner.lock() else {
            return LexCandidateResponse::empty();
        };
        let hist: Option<std::sync::RwLockReadGuard<'_, UserHistory>> = if history.is_null() {
            None
        } else {
            let wrapper = unsafe { &*history };
            wrapper.inner.read().ok()
        };
        let hist_ref = hist.as_deref();

        let resp = crate::candidates::generate_neural_candidates(
            &mut guard,
            dict_ref,
            conn_opt,
            hist_ref,
            context_str,
            reading_str,
            max_results as usize,
        );
        pack_candidate_response(resp)
    }
}

#[cfg(not(feature = "neural"))]
mod inner {
    use super::*;

    pub struct LexNeuralScorer;

    #[no_mangle]
    pub extern "C" fn lex_neural_open(_model_path: *const c_char) -> *mut LexNeuralScorer {
        ptr::null_mut()
    }

    #[no_mangle]
    pub extern "C" fn lex_neural_close(_scorer: *mut LexNeuralScorer) {}

    #[repr(C)]
    pub struct LexGhostTextResult {
        pub text: *const c_char,
        _owned: *mut CString,
    }

    #[no_mangle]
    pub extern "C" fn lex_neural_generate_ghost(
        _scorer: *mut LexNeuralScorer,
        _context: *const c_char,
        _max_tokens: u32,
    ) -> LexGhostTextResult {
        LexGhostTextResult {
            text: ptr::null(),
            _owned: ptr::null_mut(),
        }
    }

    #[no_mangle]
    pub extern "C" fn lex_ghost_text_free(result: LexGhostTextResult) {
        if !result._owned.is_null() {
            unsafe {
                drop(Box::from_raw(result._owned));
            }
        }
    }

    #[no_mangle]
    pub extern "C" fn lex_generate_neural_candidates(
        _scorer: *mut LexNeuralScorer,
        _dict: *const TrieDictionary,
        _conn: *const ConnectionMatrix,
        _history: *const LexUserHistoryWrapper,
        _context: *const c_char,
        _reading: *const c_char,
        _max_results: u32,
    ) -> LexCandidateResponse {
        LexCandidateResponse::empty()
    }
}

pub use inner::*;
