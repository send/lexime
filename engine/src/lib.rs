// FFI functions perform null checks before dereferencing raw pointers.
// Clippy cannot verify this statically, so we allow it at crate level.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

pub mod candidates;
pub mod converter;
pub mod dict;
mod ffi;
#[cfg(feature = "neural")]
pub mod neural;
pub mod romaji;
pub mod session;
pub mod trace_init;
pub mod unicode;
pub mod user_history;

pub use ffi::*;
