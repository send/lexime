use std::ffi::c_char;
use std::path::Path;
use std::ptr;
use std::sync::RwLock;

use super::{conn_ref, cptr_to_str, ffi_close, ffi_guard, owned_new};
use crate::converter::convert_with_history;
use crate::dict::connection::ConnectionMatrix;
use crate::dict::TrieDictionary;
use crate::user_history::UserHistory;

use super::convert::{pack_conversion_result, LexConversionResult, LexSegment};

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
#[allow(clippy::unused_unit)]
pub extern "C" fn lex_history_record(
    history: *const LexUserHistoryWrapper,
    segments: *const LexSegment,
    len: u32,
) {
    ffi_guard!(();
        ref:     wrapper = history,
        nonnull:           segments,
    );
    if len == 0 {
        return;
    }
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

#[no_mangle]
pub extern "C" fn lex_convert_with_history(
    dict: *const TrieDictionary,
    conn: *const ConnectionMatrix,
    history: *const LexUserHistoryWrapper,
    kana: *const c_char,
) -> LexConversionResult {
    ffi_guard!(LexConversionResult::empty();
        ref: dict    = dict,
        ref: wrapper = history,
        str: kana_str = kana,
    );
    let conn = unsafe { conn_ref(conn) };
    let Ok(h) = wrapper.inner.read() else {
        return LexConversionResult::empty();
    };

    pack_conversion_result(convert_with_history(dict, conn, &h, kana_str))
}
