use std::ffi::c_char;
use std::ptr;
use std::sync::Arc;

use super::{ffi_close, ffi_guard, owned_new};
use crate::dict::connection::ConnectionMatrix;

/// FFI wrapper that holds a connection matrix in an `Arc` for shared ownership with sessions.
pub struct LexConnWrapper {
    pub(crate) inner: Arc<ConnectionMatrix>,
}

#[no_mangle]
#[must_use]
pub extern "C" fn lex_conn_open(path: *const c_char) -> *mut LexConnWrapper {
    ffi_guard!(ptr::null_mut() ; str: path_str = path ,);
    match ConnectionMatrix::open(std::path::Path::new(path_str)) {
        Ok(conn) => owned_new(LexConnWrapper {
            inner: Arc::new(conn),
        }),
        Err(_) => ptr::null_mut(),
    }
}

ffi_close!(lex_conn_close, LexConnWrapper);
