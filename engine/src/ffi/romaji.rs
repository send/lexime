use std::ffi::{c_char, CString};
use std::ptr;

use super::ffi_guard;
use crate::romaji::{convert_romaji, RomajiTrie, TrieLookupResult};

// --- Romaji Lookup FFI ---

/// Result of a romaji trie lookup, returned to Swift.
/// tag: 0=none, 1=prefix, 2=exact, 3=exactAndPrefix
#[repr(C)]
pub struct LexRomajiLookupResult {
    pub tag: u8,
    pub kana: *const c_char,
    _owned: *mut CString,
}

impl LexRomajiLookupResult {
    fn none() -> Self {
        Self {
            tag: 0,
            kana: ptr::null(),
            _owned: ptr::null_mut(),
        }
    }

    fn prefix() -> Self {
        Self {
            tag: 1,
            kana: ptr::null(),
            _owned: ptr::null_mut(),
        }
    }

    fn exact(kana: &str) -> Self {
        let Ok(cs) = CString::new(kana) else {
            return Self::none();
        };
        let ptr = cs.as_ptr();
        let owned = Box::into_raw(Box::new(cs));
        Self {
            tag: 2,
            kana: ptr,
            _owned: owned,
        }
    }

    fn exact_and_prefix(kana: &str) -> Self {
        let Ok(cs) = CString::new(kana) else {
            return Self::none();
        };
        let ptr = cs.as_ptr();
        let owned = Box::into_raw(Box::new(cs));
        Self {
            tag: 3,
            kana: ptr,
            _owned: owned,
        }
    }
}

#[no_mangle]
pub extern "C" fn lex_romaji_lookup(romaji: *const c_char) -> LexRomajiLookupResult {
    ffi_guard!(LexRomajiLookupResult::none();
        str: romaji_str = romaji,
    );
    let trie = RomajiTrie::global();
    match trie.lookup(romaji_str) {
        TrieLookupResult::None => LexRomajiLookupResult::none(),
        TrieLookupResult::Prefix => LexRomajiLookupResult::prefix(),
        TrieLookupResult::Exact(kana) => LexRomajiLookupResult::exact(&kana),
        TrieLookupResult::ExactAndPrefix(kana) => LexRomajiLookupResult::exact_and_prefix(&kana),
    }
}

#[no_mangle]
pub extern "C" fn lex_romaji_lookup_free(result: LexRomajiLookupResult) {
    if !result._owned.is_null() {
        unsafe {
            drop(Box::from_raw(result._owned));
        }
    }
}

// --- Romaji Convert FFI ---

#[repr(C)]
pub struct LexRomajiConvertResult {
    pub composed_kana: *const c_char,
    pub pending_romaji: *const c_char,
    _owned: *mut (CString, CString),
}

impl LexRomajiConvertResult {
    fn empty() -> Self {
        Self {
            composed_kana: ptr::null(),
            pending_romaji: ptr::null(),
            _owned: ptr::null_mut(),
        }
    }
}

#[no_mangle]
pub extern "C" fn lex_romaji_convert(
    composed_kana: *const c_char,
    pending_romaji: *const c_char,
    force: u8,
) -> LexRomajiConvertResult {
    ffi_guard!(LexRomajiConvertResult::empty();
        str: kana_str    = composed_kana,
        str: pending_str = pending_romaji,
    );
    let result = convert_romaji(kana_str, pending_str, force != 0);
    let Ok(kana_c) = CString::new(result.composed_kana) else {
        return LexRomajiConvertResult::empty();
    };
    let Ok(pending_c) = CString::new(result.pending_romaji) else {
        return LexRomajiConvertResult::empty();
    };
    let kana_ptr = kana_c.as_ptr();
    let pending_ptr = pending_c.as_ptr();
    let owned = Box::into_raw(Box::new((kana_c, pending_c)));
    LexRomajiConvertResult {
        composed_kana: kana_ptr,
        pending_romaji: pending_ptr,
        _owned: owned,
    }
}

#[no_mangle]
pub extern "C" fn lex_romaji_convert_free(result: LexRomajiConvertResult) {
    if !result._owned.is_null() {
        unsafe {
            drop(Box::from_raw(result._owned));
        }
    }
}
