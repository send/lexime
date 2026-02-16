//! FFI layer – each sub-module exposes one domain area of the C API.
//!
//! Types and helper functions that are shared across sub-modules live here
//! (macros, `OwnedVec`, pointer helpers).

use std::ffi::{c_char, CStr, CString};
use std::path::Path;
use std::ptr;

// Domain modules
pub mod candidates;
pub mod connection;
pub mod convert;
pub mod dict;
pub mod history;
pub mod neural;
pub mod romaji;
pub mod session;

#[cfg(test)]
mod tests;

// Re-export all public FFI symbols so `pub use ffi::*;` in lib.rs works.
pub use candidates::*;
pub use connection::*;
pub use convert::*;
pub use dict::*;
pub use history::*;
pub use neural::*;
pub use romaji::*;
pub use session::*;

// --- Generic owned-pointer helpers for FFI resource management ---

/// Allocate a value on the heap and return a raw pointer suitable for FFI.
/// The caller is responsible for eventually passing the pointer to [`owned_drop`].
pub(crate) fn owned_new<T>(value: T) -> *mut T {
    Box::into_raw(Box::new(value))
}

/// Free a heap-allocated value previously created by [`owned_new`].
/// No-op if `ptr` is null.
///
/// # Safety
/// `ptr` must have been produced by [`owned_new`] (i.e. `Box::into_raw`)
/// and must not have been freed already.
pub(crate) unsafe fn owned_drop<T>(ptr: *mut T) {
    if !ptr.is_null() {
        drop(Box::from_raw(ptr));
    }
}

/// Safely convert a C string pointer to a `&str`.
/// Returns `None` if the pointer is null or contains invalid UTF-8.
pub(crate) unsafe fn cptr_to_str<'a>(ptr: *const c_char) -> Option<&'a str> {
    if ptr.is_null() {
        return None;
    }
    CStr::from_ptr(ptr).to_str().ok()
}

// ---------------------------------------------------------------------------
// FFI boilerplate-reduction macros (crate-internal)
// ---------------------------------------------------------------------------

/// Validate one or more FFI arguments and bind them as safe Rust values,
/// returning `$on_err` from the **calling** function if any check fails.
///
/// # Supported argument forms
///
/// | Syntax | What it does |
/// |--------|--------------|
/// | `str: $name = $ptr` | Null-check `$ptr: *const c_char`, convert via [`cptr_to_str`] to `&str`, bind as `$name`. |
/// | `ref: $name = $ptr` | Null-check `$ptr: *const T`, dereference to `&T`, bind as `$name`. |
/// | `nonnull: $ptr`      | Assert `$ptr` is non-null (no new binding is introduced). |
///
/// # Examples
///
/// ```ignore
/// ffi_guard!(LexCandidateList::empty();
///     ref: dict     = dict_ptr,
///     str: reading  = reading_ptr,
/// );
/// ```
macro_rules! ffi_guard {
    ($on_err:expr ; ) => {};

    ($on_err:expr ; str: $name:ident = $ptr:expr , $($rest:tt)*) => {
        let Some($name) = (unsafe { $crate::ffi::cptr_to_str($ptr) }) else {
            return $on_err;
        };
        $crate::ffi::ffi_guard!($on_err ; $($rest)*);
    };

    ($on_err:expr ; ref: $name:ident = $ptr:expr , $($rest:tt)*) => {
        if $ptr.is_null() {
            return $on_err;
        }
        let $name = unsafe { &*$ptr };
        $crate::ffi::ffi_guard!($on_err ; $($rest)*);
    };

    ($on_err:expr ; nonnull: $ptr:expr , $($rest:tt)*) => {
        if $ptr.is_null() {
            return $on_err;
        }
        $crate::ffi::ffi_guard!($on_err ; $($rest)*);
    };
}

/// Define an `extern "C"` function that closes (frees) a heap-allocated resource.
macro_rules! ffi_close {
    ($fn_name:ident, $T:ty) => {
        #[no_mangle]
        pub extern "C" fn $fn_name(ptr: *mut $T) {
            unsafe { $crate::ffi::owned_drop(ptr) };
        }
    };
}

// Make macros available to sub-modules.
pub(crate) use ffi_close;
pub(crate) use ffi_guard;

// --- Shared FFI types ---

/// Generic FFI-owned buffer: keeps a `Vec<T>` (whose pointer is exposed to C)
/// alive together with the `CString`s that back any `*const c_char` inside `T`.
pub(crate) struct OwnedVec<T> {
    pub(crate) items: Vec<T>,
    pub(crate) _strings: Vec<CString>,
}

impl<T> OwnedVec<T> {
    /// Box the items + strings, return (data_ptr, len, owned_ptr).
    /// Returns null pointers when `items` is empty.
    pub(crate) fn pack(items: Vec<T>, strings: Vec<CString>) -> (*const T, u32, *mut Self) {
        if items.is_empty() {
            return (ptr::null(), 0, ptr::null_mut());
        }
        let owned = Box::new(Self {
            items,
            _strings: strings,
        });
        // Capture pointer and length before consuming the Box.
        // This is safe because Box::into_raw does not move or reallocate
        // the Vec's heap buffer — it only converts the Box into a raw pointer.
        let data_ptr = owned.items.as_ptr();
        let len = owned.items.len() as u32;
        let owned_ptr = Box::into_raw(owned);
        (data_ptr, len, owned_ptr)
    }
}

// --- Top-level FFI functions ---

#[no_mangle]
pub extern "C" fn lex_engine_version() -> *const c_char {
    c"0.1.0".as_ptr()
}

#[no_mangle]
pub extern "C" fn lex_engine_echo(x: i32) -> i32 {
    x
}

#[no_mangle]
#[allow(clippy::unused_unit)]
pub extern "C" fn lex_trace_init(log_dir: *const c_char) {
    ffi_guard!(();
        str: dir_str = log_dir,
    );
    crate::trace_init::init_tracing(Path::new(dir_str));
}
