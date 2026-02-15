use std::ffi::c_char;
use std::path::Path;
use std::ptr;
use std::sync::RwLock;

use super::{ffi_close, ffi_guard, owned_new};
use crate::user_history::UserHistory;

// --- User History FFI ---

pub struct LexUserHistoryWrapper {
    pub(crate) inner: RwLock<UserHistory>,
}

#[no_mangle]
pub extern "C" fn lex_history_open(path: *const c_char) -> *mut LexUserHistoryWrapper {
    ffi_guard!(ptr::null_mut() ; str: path_str = path ,);

    match UserHistory::open(Path::new(path_str)) {
        Ok(history) => owned_new(LexUserHistoryWrapper {
            inner: RwLock::new(history),
        }),
        Err(_) => ptr::null_mut(),
    }
}

ffi_close!(lex_history_close, LexUserHistoryWrapper);

#[no_mangle]
pub extern "C" fn lex_history_save(
    history: *const LexUserHistoryWrapper,
    path: *const c_char,
) -> i32 {
    ffi_guard!(-1;
        ref: wrapper  = history,
        str: path_str = path,
    );
    let snapshot = {
        let Ok(h) = wrapper.inner.read() else {
            return -1;
        };
        h.clone()
    };
    match snapshot.save(Path::new(path_str)) {
        Ok(()) => 0,
        Err(_) => -1,
    }
}
