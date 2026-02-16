pub mod api;
pub mod candidates;
pub mod converter;
pub mod dict;
// FFI functions perform null checks before dereferencing raw pointers.
// Clippy cannot verify this statically, so we scope the allow to the FFI module.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
mod ffi;
#[cfg(feature = "neural")]
pub mod neural;
pub mod romaji;
pub mod session;
pub mod trace_init;
pub mod unicode;
pub mod user_history;

pub use ffi::*;

uniffi::setup_scaffolding!();
