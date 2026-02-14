// FFI functions perform null checks before dereferencing raw pointers.
// Clippy cannot verify this statically, so we allow it at crate level.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

pub mod candidates;
pub mod converter;
pub mod dict;
pub mod romaji;
pub mod session;
pub mod trace_init;
pub mod unicode;
pub mod user_history;

use std::ffi::{c_char, CStr, CString};
use std::path::Path;
use std::ptr;
use std::sync::RwLock;

use candidates::{generate_candidates, generate_prediction_candidates, CandidateResponse};
use converter::{convert, convert_nbest, convert_nbest_with_history, convert_with_history};
use dict::connection::ConnectionMatrix;
use dict::{Dictionary, TrieDictionary};
use romaji::{convert_romaji, RomajiTrie, TrieLookupResult};
use user_history::UserHistory;

// --- Generic owned-pointer helpers for FFI resource management ---

/// Allocate a value on the heap and return a raw pointer suitable for FFI.
/// The caller is responsible for eventually passing the pointer to [`owned_drop`].
fn owned_new<T>(value: T) -> *mut T {
    Box::into_raw(Box::new(value))
}

/// Free a heap-allocated value previously created by [`owned_new`].
/// No-op if `ptr` is null.
///
/// # Safety
/// `ptr` must have been produced by [`owned_new`] (i.e. `Box::into_raw`)
/// and must not have been freed already.
unsafe fn owned_drop<T>(ptr: *mut T) {
    if !ptr.is_null() {
        drop(Box::from_raw(ptr));
    }
}

/// Safely convert a C string pointer to a `&str`.
/// Returns `None` if the pointer is null or contains invalid UTF-8.
unsafe fn cptr_to_str<'a>(ptr: *const c_char) -> Option<&'a str> {
    if ptr.is_null() {
        return None;
    }
    CStr::from_ptr(ptr).to_str().ok()
}

/// Convert a nullable ConnectionMatrix pointer to an `Option<&ConnectionMatrix>`.
unsafe fn conn_ref<'a>(conn: *const ConnectionMatrix) -> Option<&'a ConnectionMatrix> {
    if conn.is_null() {
        None
    } else {
        Some(&*conn)
    }
}

// ---------------------------------------------------------------------------
// FFI boilerplate-reduction macros (crate-internal)
// ---------------------------------------------------------------------------
//
// The `extern "C"` functions in this module follow a handful of recurring
// patterns: null-check a pointer, convert a C string, dereference an opaque
// handle, and return a sentinel on failure.  The macros below capture those
// patterns so each FFI entry point can focus on its domain logic rather
// than repeating the same safety scaffolding.

/// Validate one or more FFI arguments and bind them as safe Rust values,
/// returning `$on_err` from the **calling** function if any check fails.
///
/// This is the workhorse macro for reducing FFI boilerplate.  Place it at
/// the top of an `extern "C"` function body and list every raw-pointer
/// argument that needs validation.  The macro expands to a sequence of
/// `let` bindings with early returns, so the rest of the function body
/// can use the bound names as ordinary safe references / slices.
///
/// # Supported argument forms
///
/// | Syntax | What it does |
/// |--------|--------------|
/// | `str: $name = $ptr` | Null-check `$ptr: *const c_char`, convert via [`cptr_to_str`] to `&str`, bind as `$name`. |
/// | `ref: $name = $ptr` | Null-check `$ptr: *const T`, dereference to `&T`, bind as `$name`. |
/// | `nonnull: $ptr`      | Assert `$ptr` is non-null (no new binding is introduced). |
///
/// The first positional argument (`$on_err`) is the expression returned
/// when any check fails.  It is evaluated lazily -- only on the failing
/// branch.
///
/// # Examples
///
/// ```ignore
/// // Inside an extern "C" fn returning LexCandidateList:
/// ffi_guard!(LexCandidateList::empty();
///     ref: dict     = dict_ptr,
///     str: reading  = reading_ptr,
/// );
/// // `dict` is now `&TrieDictionary`, `reading` is `&str`.
/// ```
///
/// ```ignore
/// // Multiple pointer checks with no string conversion:
/// ffi_guard!(LexConversionResultList::empty();
///     ref:     dict    = dict_ptr,
///     nonnull: history,
/// );
/// ```
macro_rules! ffi_guard {
    // Terminal rule -- all arguments processed.
    ($on_err:expr ; ) => {};

    // `str:` -- convert *const c_char to &str.
    ($on_err:expr ; str: $name:ident = $ptr:expr , $($rest:tt)*) => {
        let Some($name) = (unsafe { cptr_to_str($ptr) }) else {
            return $on_err;
        };
        ffi_guard!($on_err ; $($rest)*);
    };

    // `ref:` -- dereference *const T to &T after null check.
    ($on_err:expr ; ref: $name:ident = $ptr:expr , $($rest:tt)*) => {
        if $ptr.is_null() {
            return $on_err;
        }
        let $name = unsafe { &*$ptr };
        ffi_guard!($on_err ; $($rest)*);
    };

    // `nonnull:` -- assert non-null, no binding.
    ($on_err:expr ; nonnull: $ptr:expr , $($rest:tt)*) => {
        if $ptr.is_null() {
            return $on_err;
        }
        ffi_guard!($on_err ; $($rest)*);
    };
}

/// Define an `extern "C"` function that opens a resource from a file path.
///
/// Encapsulates the common pattern shared by `lex_dict_open`,
/// `lex_conn_open`, and similar functions:
///
/// 1. Null-check the incoming `*const c_char` path.
/// 2. Convert to `&str` via [`cptr_to_str`].
/// 3. Call a constructor that returns `Result<T, _>`.
/// 4. On success, box the value and return a raw pointer.
/// 5. On failure, return `ptr::null_mut()`.
///
/// # Syntax
///
/// ```ignore
/// ffi_open!(function_name, ReturnType, |path: &Path| constructor(path));
/// ```
///
/// The closure receives a `&Path` and must return a `Result<ReturnType, _>`.
macro_rules! ffi_open {
    ($fn_name:ident, $T:ty, $open_expr:expr) => {
        #[no_mangle]
        pub extern "C" fn $fn_name(path: *const c_char) -> *mut $T {
            ffi_guard!(ptr::null_mut() ; str: path_str = path ,);
            let opener: fn(&Path) -> _ = $open_expr;
            match opener(Path::new(path_str)) {
                Ok(val) => owned_new(val),
                Err(_) => ptr::null_mut(),
            }
        }
    };
}

/// Define an `extern "C"` function that closes (frees) a heap-allocated
/// resource previously returned by an `ffi_open!`-generated function.
///
/// Null-checks the pointer, then reclaims the `Box` and drops it.
///
/// # Syntax
///
/// ```ignore
/// ffi_close!(function_name, ResourceType);
/// ```
macro_rules! ffi_close {
    ($fn_name:ident, $T:ty) => {
        #[no_mangle]
        pub extern "C" fn $fn_name(ptr: *mut $T) {
            unsafe { owned_drop(ptr) };
        }
    };
}

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
    trace_init::init_tracing(Path::new(dir_str));
}

// --- Dictionary FFI ---

#[repr(C)]
pub struct LexCandidate {
    pub reading: *const c_char,
    pub surface: *const c_char,
    pub cost: i16,
}

#[repr(C)]
pub struct LexCandidateList {
    pub candidates: *const LexCandidate,
    pub len: u32,
    _owned: *mut OwnedVec<LexCandidate>,
}

/// Generic FFI-owned buffer: keeps a `Vec<T>` (whose pointer is exposed to C)
/// alive together with the `CString`s that back any `*const c_char` inside `T`.
struct OwnedVec<T> {
    items: Vec<T>,
    _strings: Vec<CString>,
}

impl<T> OwnedVec<T> {
    /// Box the items + strings, return (data_ptr, len, owned_ptr).
    /// Returns null pointers when `items` is empty.
    fn pack(items: Vec<T>, strings: Vec<CString>) -> (*const T, u32, *mut Self) {
        if items.is_empty() {
            return (ptr::null(), 0, ptr::null_mut());
        }
        let owned = Box::new(Self {
            items,
            _strings: strings,
        });
        let owned_ptr = Box::into_raw(owned);
        let data_ptr = unsafe { (*owned_ptr).items.as_ptr() };
        let len = unsafe { (*owned_ptr).items.len() as u32 };
        (data_ptr, len, owned_ptr)
    }
}

impl LexCandidateList {
    fn empty() -> Self {
        Self {
            candidates: ptr::null(),
            len: 0,
            _owned: ptr::null_mut(),
        }
    }

    fn from_entries(reading: &str, entries: &[dict::DictEntry]) -> Self {
        let Ok(reading_cstr) = CString::new(reading) else {
            return Self::empty();
        };
        let reading_ptr = reading_cstr.as_ptr();

        let mut strings = Vec::with_capacity(entries.len() + 1);
        let mut candidates = Vec::with_capacity(entries.len());

        strings.push(reading_cstr);

        for entry in entries {
            let Ok(surface) = CString::new(entry.surface.as_str()) else {
                continue; // skip entries with interior null bytes
            };
            candidates.push(LexCandidate {
                reading: reading_ptr,
                surface: surface.as_ptr(),
                cost: entry.cost,
            });
            strings.push(surface);
        }

        Self::pack(candidates, strings)
    }

    fn from_flat_entries(pairs: &[(String, dict::DictEntry)]) -> Self {
        let mut strings = Vec::new();
        let mut candidates = Vec::new();

        for (reading, entry) in pairs {
            let Ok(reading_cstr) = CString::new(reading.as_str()) else {
                continue;
            };
            let Ok(surface) = CString::new(entry.surface.as_str()) else {
                continue;
            };
            candidates.push(LexCandidate {
                reading: reading_cstr.as_ptr(),
                surface: surface.as_ptr(),
                cost: entry.cost,
            });
            strings.push(reading_cstr);
            strings.push(surface);
        }

        Self::pack(candidates, strings)
    }

    fn from_search_results(results: Vec<dict::SearchResult<'_>>) -> Self {
        let mut strings = Vec::new();
        let mut candidates = Vec::new();

        for result in &results {
            let Ok(reading_cstr) = CString::new(result.reading.as_str()) else {
                continue; // skip results with interior null bytes
            };
            let reading_ptr = reading_cstr.as_ptr();
            strings.push(reading_cstr);

            for entry in result.entries {
                let Ok(surface) = CString::new(entry.surface.as_str()) else {
                    continue;
                };
                candidates.push(LexCandidate {
                    reading: reading_ptr,
                    surface: surface.as_ptr(),
                    cost: entry.cost,
                });
                strings.push(surface);
            }
        }

        Self::pack(candidates, strings)
    }

    fn pack(candidates: Vec<LexCandidate>, strings: Vec<CString>) -> Self {
        let (ptr, len, owned) = OwnedVec::pack(candidates, strings);
        if owned.is_null() {
            return Self::empty();
        }
        Self {
            candidates: ptr,
            len,
            _owned: owned,
        }
    }
}

ffi_open!(lex_dict_open, TrieDictionary, |p| TrieDictionary::open(p));
ffi_close!(lex_dict_close, TrieDictionary);

#[no_mangle]
pub extern "C" fn lex_dict_lookup(
    dict: *const TrieDictionary,
    reading: *const c_char,
) -> LexCandidateList {
    ffi_guard!(LexCandidateList::empty();
        ref: dict        = dict,
        str: reading_str = reading,
    );

    match dict.lookup(reading_str) {
        Some(entries) => LexCandidateList::from_entries(reading_str, entries),
        None => LexCandidateList::empty(),
    }
}

#[no_mangle]
pub extern "C" fn lex_dict_predict(
    dict: *const TrieDictionary,
    prefix: *const c_char,
    max_results: u32,
) -> LexCandidateList {
    ffi_guard!(LexCandidateList::empty();
        ref: dict       = dict,
        str: prefix_str = prefix,
    );

    let results = dict.predict(prefix_str, max_results as usize);
    LexCandidateList::from_search_results(results)
}

#[no_mangle]
pub extern "C" fn lex_dict_predict_ranked(
    dict: *const TrieDictionary,
    history: *const LexUserHistoryWrapper,
    prefix: *const c_char,
    max_results: u32,
) -> LexCandidateList {
    ffi_guard!(LexCandidateList::empty();
        ref: dict       = dict,
        str: prefix_str = prefix,
    );

    // When history is available, over-fetch so boosted entries that rank outside
    // the pure-cost top-N still have a chance to surface after re-sorting.
    let fetch_limit = if history.is_null() {
        max_results as usize
    } else {
        (max_results as usize).max(200)
    };
    let mut ranked = dict.predict_ranked(prefix_str, fetch_limit, 1000);

    // If history is provided, re-sort by unigram boost (descending), then cost (ascending)
    if !history.is_null() {
        let wrapper = unsafe { &*history };
        if let Ok(h) = wrapper.inner.read() {
            ranked.sort_by(|(r_a, e_a), (r_b, e_b)| {
                let boost_a = h.unigram_boost(r_a, &e_a.surface);
                let boost_b = h.unigram_boost(r_b, &e_b.surface);
                boost_b
                    .cmp(&boost_a) // higher boost first
                    .then(e_a.cost.cmp(&e_b.cost)) // then lower cost first
            });
        }
        ranked.truncate(max_results as usize);
    }

    LexCandidateList::from_flat_entries(&ranked)
}

#[no_mangle]
pub extern "C" fn lex_candidates_free(list: LexCandidateList) {
    unsafe { owned_drop(list._owned) };
}

// --- Connection matrix FFI ---

ffi_open!(lex_conn_open, ConnectionMatrix, |p| ConnectionMatrix::open(
    p
));
ffi_close!(lex_conn_close, ConnectionMatrix);

// --- Conversion FFI ---

#[repr(C)]
pub struct LexSegment {
    pub reading: *const c_char,
    pub surface: *const c_char,
}

#[repr(C)]
pub struct LexConversionResult {
    pub segments: *const LexSegment,
    pub len: u32,
    _owned: *mut OwnedVec<LexSegment>,
}

impl LexConversionResult {
    fn empty() -> Self {
        Self {
            segments: ptr::null(),
            len: 0,
            _owned: ptr::null_mut(),
        }
    }
}

/// Pack a list of ConvertedSegments into a C-compatible LexConversionResult.
fn pack_conversion_result(result: Vec<converter::ConvertedSegment>) -> LexConversionResult {
    let mut strings = Vec::with_capacity(result.len() * 2);
    let mut segments = Vec::with_capacity(result.len());

    for seg in &result {
        let Ok(reading) = CString::new(seg.reading.as_str()) else {
            continue;
        };
        let Ok(surface) = CString::new(seg.surface.as_str()) else {
            continue;
        };
        segments.push(LexSegment {
            reading: reading.as_ptr(),
            surface: surface.as_ptr(),
        });
        strings.push(reading);
        strings.push(surface);
    }

    let (ptr, len, owned) = OwnedVec::pack(segments, strings);
    if owned.is_null() {
        return LexConversionResult::empty();
    }
    LexConversionResult {
        segments: ptr,
        len,
        _owned: owned,
    }
}

#[no_mangle]
pub extern "C" fn lex_convert(
    dict: *const TrieDictionary,
    conn: *const ConnectionMatrix,
    kana: *const c_char,
) -> LexConversionResult {
    ffi_guard!(LexConversionResult::empty();
        ref: dict     = dict,
        str: kana_str = kana,
    );
    let conn = unsafe { conn_ref(conn) };

    pack_conversion_result(convert(dict, conn, kana_str))
}

#[no_mangle]
pub extern "C" fn lex_conversion_free(result: LexConversionResult) {
    unsafe { owned_drop(result._owned) };
}

// --- N-best Conversion FFI ---

#[repr(C)]
pub struct LexConversionResultList {
    pub results: *const LexConversionResult,
    pub len: u32,
    _owned: *mut OwnedVec<LexConversionResult>,
}

impl LexConversionResultList {
    fn empty() -> Self {
        Self {
            results: ptr::null(),
            len: 0,
            _owned: ptr::null_mut(),
        }
    }
}

fn pack_conversion_result_list(
    paths: Vec<Vec<converter::ConvertedSegment>>,
) -> LexConversionResultList {
    let results: Vec<LexConversionResult> = paths.into_iter().map(pack_conversion_result).collect();
    let (ptr, len, owned) = OwnedVec::pack(results, Vec::new());
    if owned.is_null() {
        return LexConversionResultList::empty();
    }
    LexConversionResultList {
        results: ptr,
        len,
        _owned: owned,
    }
}

#[no_mangle]
pub extern "C" fn lex_convert_nbest(
    dict: *const TrieDictionary,
    conn: *const ConnectionMatrix,
    kana: *const c_char,
    n: u32,
) -> LexConversionResultList {
    ffi_guard!(LexConversionResultList::empty();
        ref: dict     = dict,
        str: kana_str = kana,
    );
    let conn = unsafe { conn_ref(conn) };

    pack_conversion_result_list(convert_nbest(dict, conn, kana_str, n as usize))
}

#[no_mangle]
pub extern "C" fn lex_convert_nbest_with_history(
    dict: *const TrieDictionary,
    conn: *const ConnectionMatrix,
    history: *const LexUserHistoryWrapper,
    kana: *const c_char,
    n: u32,
) -> LexConversionResultList {
    ffi_guard!(LexConversionResultList::empty();
        ref:     dict    = dict,
        nonnull:           history,
        str:     kana_str = kana,
    );
    let conn = unsafe { conn_ref(conn) };
    let wrapper = unsafe { &*history };
    let Ok(h) = wrapper.inner.read() else {
        return LexConversionResultList::empty();
    };

    pack_conversion_result_list(convert_nbest_with_history(
        dict, conn, &h, kana_str, n as usize,
    ))
}

#[no_mangle]
pub extern "C" fn lex_conversion_result_list_free(list: LexConversionResultList) {
    if !list._owned.is_null() {
        unsafe {
            // First free each individual ConversionResult's owned data
            let owned = Box::from_raw(list._owned);
            for result in &owned.items {
                owned_drop(result._owned);
            }
            // The owned box (containing the Vec<LexConversionResult>) is dropped here
        }
    }
}

// --- User History FFI ---

pub struct LexUserHistoryWrapper {
    inner: RwLock<UserHistory>,
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
    // Clone the data under a short-lived read lock, then write to disk
    // without holding the lock.  This prevents blocking write-lock callers
    // (record_history) during file I/O, which in turn unblocks handle_key's
    // read-lock acquisition (RwLock starves readers while a writer waits).
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

#[no_mangle]
pub extern "C" fn lex_dict_lookup_with_history(
    dict: *const TrieDictionary,
    history: *const LexUserHistoryWrapper,
    reading: *const c_char,
) -> LexCandidateList {
    ffi_guard!(LexCandidateList::empty();
        ref: dict        = dict,
        ref: wrapper     = history,
        str: reading_str = reading,
    );
    let Ok(h) = wrapper.inner.read() else {
        return LexCandidateList::empty();
    };

    match dict.lookup(reading_str) {
        Some(entries) => {
            let reordered = h.reorder_candidates(reading_str, entries);
            LexCandidateList::from_entries(reading_str, &reordered)
        }
        None => LexCandidateList::empty(),
    }
}

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

// --- Unified Candidate Generation FFI ---

#[repr(C)]
pub struct LexCandidateResponse {
    pub surfaces: *const *const c_char,
    pub surfaces_len: u32,
    pub paths: *const LexConversionResult,
    pub paths_len: u32,
    _owned: *mut OwnedCandidateResponse,
}

struct OwnedCandidateResponse {
    _surface_ptrs: Vec<*const c_char>,
    _surface_strings: Vec<CString>,
    _paths: Vec<LexConversionResult>,
}

impl LexCandidateResponse {
    fn empty() -> Self {
        Self {
            surfaces: ptr::null(),
            surfaces_len: 0,
            paths: ptr::null(),
            paths_len: 0,
            _owned: ptr::null_mut(),
        }
    }
}

fn pack_candidate_response(resp: CandidateResponse) -> LexCandidateResponse {
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

    // Box first, then read pointers (matching OwnedVec::pack pattern)
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
            let owned = Box::from_raw(response._owned);
            // Free each path's owned data
            for path in &owned._paths {
                owned_drop(path._owned);
            }
            // owned box (surface_ptrs, surface_strings, paths) is dropped here
        }
    }
}

// --- InputSession FFI ---

use session::{InputSession, KeyResponse};

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
    _owned: *mut OwnedKeyResponse,
}

struct OwnedKeyResponse {
    _commit_text: Option<CString>,
    _marked_text: Option<CString>,
    _candidate_ptrs: Vec<*const c_char>,
    _candidate_strings: Vec<CString>,
    _candidate_reading: Option<CString>,
    /// History records to be fed to UserHistory::record().
    history_records: Vec<Vec<(String, String)>>,
}

impl LexKeyResponse {
    fn empty() -> Self {
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
            _owned: ptr::null_mut(),
        }
    }
}

fn pack_key_response(
    resp: KeyResponse,
    history_records: Vec<Vec<(String, String)>>,
) -> LexKeyResponse {
    use session::CandidateAction;

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

    let owned = Box::new(OwnedKeyResponse {
        _commit_text: commit_cstr,
        _marked_text: marked_cstr,
        _candidate_ptrs: candidate_ptrs,
        _candidate_strings: candidate_strings,
        _candidate_reading: reading_cstr,
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

    // History is set temporarily during handle_key/commit via with_history_lock().
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

/// Set the conversion mode. mode: 0=Standard, 1=Predictive.
#[no_mangle]
pub extern "C" fn lex_session_set_conversion_mode(session: *mut LexSession, mode: u8) {
    if session.is_null() {
        return;
    }
    let session = unsafe { &mut *session };
    let conversion_mode = match mode {
        1 => session::ConversionMode::Predictive,
        _ => session::ConversionMode::Standard,
    };
    session.inner.set_conversion_mode(conversion_mode);
}

/// Receive asynchronously generated candidates and update session state.
/// Called on the main thread after `lex_generate_candidates` completes on a background thread.
/// `reading` must match the reading used for generation (staleness check).
/// Returns a `LexKeyResponse` with updated marked text and candidates,
/// or an empty response if the candidates are stale.
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

    // Unpack surfaces from the FFI struct
    let surfaces = unpack_candidate_surfaces(cand_resp);
    let paths = unpack_candidate_paths(cand_resp);

    // Acquire history lock for potential history recording during auto-commit
    let _guard = unsafe { acquire_history_lock(session.history_ptr, &mut session.inner) };

    let resp = match session
        .inner
        .receive_candidates(reading_str, surfaces, paths)
    {
        Some(resp) => resp,
        None => {
            session.inner.set_history(None);
            return LexKeyResponse::empty();
        }
    };

    session.inner.set_history(None);
    drop(_guard);
    let records = session.inner.take_history_records();
    pack_key_response(resp, records)
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

/// Acquire a read lock on the history and set it on the session's inner.
/// Returns the guard so it stays alive during use.
///
/// # Safety
/// The returned guard must be kept alive while the session's history reference is used.
/// Call `session.inner.set_history(None)` before dropping the guard.
unsafe fn acquire_history_lock(
    history_ptr: *const LexUserHistoryWrapper,
    inner: &mut InputSession<'static>,
) -> Option<std::sync::RwLockReadGuard<'static, UserHistory>> {
    if history_ptr.is_null() {
        inner.set_history(None);
        return None;
    }
    let wrapper = &*history_ptr;
    match wrapper.inner.read() {
        Ok(guard) => {
            // SAFETY: The guard is kept alive by the caller. The transmute extends
            // the lifetime to 'static which is safe as long as the guard outlives
            // the session's use of the reference.
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
    let _guard = unsafe { acquire_history_lock(session.history_ptr, &mut session.inner) };
    let text_str = unsafe { cptr_to_str(text) }.unwrap_or("");
    let resp = session.inner.handle_key(key_code, text_str, flags);
    session.inner.set_history(None);
    drop(_guard);
    let records = session.inner.take_history_records();
    pack_key_response(resp, records)
}

#[no_mangle]
pub extern "C" fn lex_session_commit(session: *mut LexSession) -> LexKeyResponse {
    if session.is_null() {
        return LexKeyResponse::empty();
    }
    let session = unsafe { &mut *session };
    let _guard = unsafe { acquire_history_lock(session.history_ptr, &mut session.inner) };
    let resp = session.inner.commit();
    session.inner.set_history(None);
    drop(_guard);
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
/// Note: the caller should prefer using marked_text from LexKeyResponse
/// since this function cannot return the internal string without extra allocation.
#[no_mangle]
pub extern "C" fn lex_session_composed_string(_session: *const LexSession) -> *const c_char {
    c"".as_ptr()
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
/// Returns the number of record groups. The caller should iterate and call
/// lex_history_record for each group, then lex_history_save asynchronously.
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

/// Record history entries from a key response into the user history.
/// This should be called before lex_key_response_free.
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    fn make_test_dict() -> *mut TrieDictionary {
        let entries = vec![
            (
                "かんじ".to_string(),
                vec![
                    dict::DictEntry {
                        surface: "漢字".to_string(),
                        cost: 5100,
                        left_id: 0,
                        right_id: 0,
                    },
                    dict::DictEntry {
                        surface: "感じ".to_string(),
                        cost: 5150,
                        left_id: 0,
                        right_id: 0,
                    },
                ],
            ),
            (
                "かんじょう".to_string(),
                vec![dict::DictEntry {
                    surface: "感情".to_string(),
                    cost: 5000,
                    left_id: 0,
                    right_id: 0,
                }],
            ),
        ];
        let dict = TrieDictionary::from_entries(entries);
        owned_new(dict)
    }

    #[test]
    fn test_ffi_lookup_roundtrip() {
        let dict = make_test_dict();
        let reading = CString::new("かんじ").unwrap();

        let list = lex_dict_lookup(dict, reading.as_ptr());
        assert_eq!(list.len, 2);

        unsafe {
            let candidates = std::slice::from_raw_parts(list.candidates, list.len as usize);

            // Check reading
            let r = CStr::from_ptr(candidates[0].reading).to_str().unwrap();
            assert_eq!(r, "かんじ");

            // Check surfaces
            let s0 = CStr::from_ptr(candidates[0].surface).to_str().unwrap();
            let s1 = CStr::from_ptr(candidates[1].surface).to_str().unwrap();
            assert_eq!(s0, "漢字");
            assert_eq!(s1, "感じ");

            // Check cost ordering
            assert!(candidates[0].cost <= candidates[1].cost);
        }

        lex_candidates_free(list);
        lex_dict_close(dict);
    }

    #[test]
    fn test_ffi_lookup_not_found() {
        let dict = make_test_dict();
        let reading = CString::new("そんざい").unwrap();

        let list = lex_dict_lookup(dict, reading.as_ptr());
        assert_eq!(list.len, 0);
        assert!(list.candidates.is_null());

        lex_candidates_free(list);
        lex_dict_close(dict);
    }

    #[test]
    fn test_ffi_predict_with_reading() {
        let dict = make_test_dict();
        let prefix = CString::new("かん").unwrap();

        let list = lex_dict_predict(dict, prefix.as_ptr(), 100);
        assert!(list.len >= 3); // 漢字, 感じ from かんじ + 感情 from かんじょう

        unsafe {
            let candidates = std::slice::from_raw_parts(list.candidates, list.len as usize);

            // All candidates should have non-null reading
            for c in candidates {
                let r = CStr::from_ptr(c.reading).to_str().unwrap();
                assert!(r.starts_with("かん"));
            }
        }

        lex_candidates_free(list);
        lex_dict_close(dict);
    }

    #[test]
    fn test_ffi_null_safety() {
        // null dict
        let reading = CString::new("かんじ").unwrap();
        let list = lex_dict_lookup(ptr::null(), reading.as_ptr());
        assert_eq!(list.len, 0);
        lex_candidates_free(list);

        // null reading
        let dict = make_test_dict();
        let list = lex_dict_lookup(dict, ptr::null());
        assert_eq!(list.len, 0);
        lex_candidates_free(list);

        // null for predict
        let list = lex_dict_predict(dict, ptr::null(), 10);
        assert_eq!(list.len, 0);
        lex_candidates_free(list);

        lex_dict_close(dict);
    }

    #[test]
    fn test_ffi_open_close_file() {
        let dir = std::env::temp_dir().join("lexime_test_ffi");
        std::fs::create_dir_all(&dir).unwrap();
        let dict_path = dir.join("test.dict");

        // Create a small dict and save it
        let entries = vec![(
            "てすと".to_string(),
            vec![dict::DictEntry {
                surface: "テスト".to_string(),
                cost: 1000,
                left_id: 0,
                right_id: 0,
            }],
        )];
        let dict = TrieDictionary::from_entries(entries);
        dict.save(&dict_path).unwrap();

        // Open via FFI
        let path_cstr = CString::new(dict_path.to_str().unwrap()).unwrap();
        let dict_ptr = lex_dict_open(path_cstr.as_ptr());
        assert!(!dict_ptr.is_null());

        // Lookup via FFI
        let reading = CString::new("てすと").unwrap();
        let list = lex_dict_lookup(dict_ptr, reading.as_ptr());
        assert_eq!(list.len, 1);
        unsafe {
            let candidates = std::slice::from_raw_parts(list.candidates, list.len as usize);
            let s = CStr::from_ptr(candidates[0].surface).to_str().unwrap();
            assert_eq!(s, "テスト");
        }
        lex_candidates_free(list);
        lex_dict_close(dict_ptr);

        // Cleanup
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_ffi_open_nonexistent() {
        let path = CString::new("/nonexistent/path/dict.bin").unwrap();
        let dict_ptr = lex_dict_open(path.as_ptr());
        assert!(dict_ptr.is_null());
    }

    #[test]
    fn test_ffi_predict_ranked_roundtrip() {
        let dict = make_test_dict();
        let prefix = CString::new("かん").unwrap();

        let list = lex_dict_predict_ranked(dict, ptr::null(), prefix.as_ptr(), 10);
        assert!(list.len >= 3); // 漢字, 感じ from かんじ + 感情 from かんじょう

        unsafe {
            let candidates = std::slice::from_raw_parts(list.candidates, list.len as usize);
            // Should be sorted by cost
            for w in candidates.windows(2) {
                assert!(
                    w[0].cost <= w[1].cost,
                    "predict_ranked FFI should be cost-ordered"
                );
            }
        }

        lex_candidates_free(list);
        lex_dict_close(dict);
    }

    #[test]
    fn test_ffi_predict_ranked_null_safety() {
        let prefix = CString::new("かん").unwrap();

        // null dict
        let list = lex_dict_predict_ranked(ptr::null(), ptr::null(), prefix.as_ptr(), 10);
        assert_eq!(list.len, 0);
        lex_candidates_free(list);

        // null prefix
        let dict = make_test_dict();
        let list = lex_dict_predict_ranked(dict, ptr::null(), ptr::null(), 10);
        assert_eq!(list.len, 0);
        lex_candidates_free(list);

        // null history is OK (pure cost order)
        let list = lex_dict_predict_ranked(dict, ptr::null(), prefix.as_ptr(), 10);
        assert!(list.len >= 1);
        lex_candidates_free(list);

        lex_dict_close(dict);
    }

    fn make_convert_test_dict() -> *mut TrieDictionary {
        let entries = vec![
            (
                "きょう".to_string(),
                vec![dict::DictEntry {
                    surface: "今日".to_string(),
                    cost: 3000,
                    left_id: 0,
                    right_id: 0,
                }],
            ),
            (
                "は".to_string(),
                vec![dict::DictEntry {
                    surface: "は".to_string(),
                    cost: 2000,
                    left_id: 0,
                    right_id: 0,
                }],
            ),
            (
                "いい".to_string(),
                vec![dict::DictEntry {
                    surface: "良い".to_string(),
                    cost: 3500,
                    left_id: 0,
                    right_id: 0,
                }],
            ),
        ];
        let dict = TrieDictionary::from_entries(entries);
        owned_new(dict)
    }

    #[test]
    fn test_ffi_convert_roundtrip() {
        let dict = make_convert_test_dict();
        let kana = CString::new("きょうはいい").unwrap();

        let result = lex_convert(dict, ptr::null(), kana.as_ptr());
        assert!(result.len >= 3);

        unsafe {
            let segments = std::slice::from_raw_parts(result.segments, result.len as usize);
            let s0 = CStr::from_ptr(segments[0].surface).to_str().unwrap();
            assert_eq!(s0, "今日");
        }

        lex_conversion_free(result);
        lex_dict_close(dict);
    }

    #[test]
    fn test_ffi_convert_null_safety() {
        let kana = CString::new("きょう").unwrap();

        // null dict
        let result = lex_convert(ptr::null(), ptr::null(), kana.as_ptr());
        assert_eq!(result.len, 0);
        lex_conversion_free(result);

        // null kana
        let dict = make_convert_test_dict();
        let result = lex_convert(dict, ptr::null(), ptr::null());
        assert_eq!(result.len, 0);
        lex_conversion_free(result);
        lex_dict_close(dict);
    }

    #[test]
    fn test_ffi_convert_empty_input() {
        let dict = make_convert_test_dict();
        let kana = CString::new("").unwrap();

        let result = lex_convert(dict, ptr::null(), kana.as_ptr());
        assert_eq!(result.len, 0);

        lex_conversion_free(result);
        lex_dict_close(dict);
    }

    #[test]
    fn test_ffi_conn_null_safety() {
        // null path
        let conn = lex_conn_open(ptr::null());
        assert!(conn.is_null());

        // nonexistent path
        let path = CString::new("/nonexistent/path/conn.bin").unwrap();
        let conn = lex_conn_open(path.as_ptr());
        assert!(conn.is_null());

        // close null
        lex_conn_close(ptr::null_mut());
    }

    #[test]
    fn test_ffi_history_roundtrip() {
        let dir = std::env::temp_dir().join("lexime_test_ffi_history");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.lxud");
        let path_cstr = CString::new(path.to_str().unwrap()).unwrap();

        // Open (creates empty)
        let history = lex_history_open(path_cstr.as_ptr());
        assert!(!history.is_null());

        // Record segments
        let reading = CString::new("きょう").unwrap();
        let surface = CString::new("京").unwrap();
        let seg = LexSegment {
            reading: reading.as_ptr(),
            surface: surface.as_ptr(),
        };
        lex_history_record(history, &seg, 1);

        // Save
        assert_eq!(lex_history_save(history, path_cstr.as_ptr()), 0);
        lex_history_close(history);

        // Reopen and verify boost via lookup_with_history
        let history2 = lex_history_open(path_cstr.as_ptr());
        assert!(!history2.is_null());

        let dict = make_test_dict();
        let reading_lookup = CString::new("かんじ").unwrap();
        let list = lex_dict_lookup_with_history(dict, history2, reading_lookup.as_ptr());
        // Should still return entries (just potentially reordered)
        assert!(list.len >= 1);
        lex_candidates_free(list);

        lex_history_close(history2);
        lex_dict_close(dict);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_ffi_history_save_invalid_path() {
        let dir = std::env::temp_dir().join("lexime_test_ffi_save_fail");
        std::fs::create_dir_all(&dir).unwrap();
        let open_path = dir.join("temp.lxud");
        let open_cstr = CString::new(open_path.to_str().unwrap()).unwrap();

        let history = lex_history_open(open_cstr.as_ptr());
        assert!(!history.is_null());

        // Save to an invalid path should return -1
        let bad_path = CString::new("/nonexistent/deeply/nested/history.lxud").unwrap();
        assert_eq!(lex_history_save(history, bad_path.as_ptr()), -1);

        lex_history_close(history);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_ffi_convert_nbest_roundtrip() {
        let dict = make_convert_test_dict();
        let kana = CString::new("きょうはいい").unwrap();

        let list = lex_convert_nbest(dict, ptr::null(), kana.as_ptr(), 5);
        assert!(list.len >= 1, "should return at least 1 result");

        unsafe {
            let results = std::slice::from_raw_parts(list.results, list.len as usize);
            // First result should match 1-best
            assert!(results[0].len >= 3);
            let segments = std::slice::from_raw_parts(results[0].segments, results[0].len as usize);
            let s0 = CStr::from_ptr(segments[0].surface).to_str().unwrap();
            assert_eq!(s0, "今日");
        }

        lex_conversion_result_list_free(list);
        lex_dict_close(dict);
    }

    #[test]
    fn test_ffi_convert_nbest_null_safety() {
        let kana = CString::new("きょう").unwrap();

        // null dict
        let list = lex_convert_nbest(ptr::null(), ptr::null(), kana.as_ptr(), 5);
        assert_eq!(list.len, 0);
        lex_conversion_result_list_free(list);

        // null kana
        let dict = make_convert_test_dict();
        let list = lex_convert_nbest(dict, ptr::null(), ptr::null(), 5);
        assert_eq!(list.len, 0);
        lex_conversion_result_list_free(list);

        // n = 0
        let list = lex_convert_nbest(dict, ptr::null(), kana.as_ptr(), 0);
        assert_eq!(list.len, 0);
        lex_conversion_result_list_free(list);

        lex_dict_close(dict);
    }

    // --- Romaji Lookup FFI tests ---

    #[test]
    fn test_ffi_romaji_lookup_exact() {
        let romaji = CString::new("ka").unwrap();
        let result = lex_romaji_lookup(romaji.as_ptr());
        assert_eq!(result.tag, 2, "ka should be exact");
        let kana = unsafe { CStr::from_ptr(result.kana).to_str().unwrap() };
        assert_eq!(kana, "か");
        lex_romaji_lookup_free(result);
    }

    #[test]
    fn test_ffi_romaji_lookup_prefix() {
        let romaji = CString::new("k").unwrap();
        let result = lex_romaji_lookup(romaji.as_ptr());
        assert_eq!(result.tag, 1, "k should be prefix");
        assert!(result.kana.is_null());
        lex_romaji_lookup_free(result);
    }

    #[test]
    fn test_ffi_romaji_lookup_none() {
        let romaji = CString::new("xyz").unwrap();
        let result = lex_romaji_lookup(romaji.as_ptr());
        assert_eq!(result.tag, 0, "xyz should be none");
        assert!(result.kana.is_null());
        lex_romaji_lookup_free(result);
    }

    #[test]
    fn test_ffi_romaji_lookup_exact_and_prefix() {
        // "chi" → ち, and is also prefix for "cha", "chu", etc.
        let romaji = CString::new("chi").unwrap();
        let result = lex_romaji_lookup(romaji.as_ptr());
        assert!(
            result.tag == 2 || result.tag == 3,
            "chi should be exact or exactAndPrefix"
        );
        let kana = unsafe { CStr::from_ptr(result.kana).to_str().unwrap() };
        assert_eq!(kana, "ち");
        lex_romaji_lookup_free(result);
    }

    #[test]
    fn test_ffi_romaji_lookup_null_safety() {
        let result = lex_romaji_lookup(ptr::null());
        assert_eq!(result.tag, 0);
        assert!(result.kana.is_null());
        lex_romaji_lookup_free(result);
    }

    // --- Romaji Convert FFI tests ---

    #[test]
    fn test_ffi_romaji_convert_basic() {
        let kana = CString::new("").unwrap();
        let pending = CString::new("ka").unwrap();
        let result = lex_romaji_convert(kana.as_ptr(), pending.as_ptr(), 0);
        let composed = unsafe { CStr::from_ptr(result.composed_kana).to_str().unwrap() };
        let pend = unsafe { CStr::from_ptr(result.pending_romaji).to_str().unwrap() };
        assert_eq!(composed, "か");
        assert_eq!(pend, "");
        lex_romaji_convert_free(result);
    }

    #[test]
    fn test_ffi_romaji_convert_sokuon() {
        let kana = CString::new("").unwrap();
        let pending = CString::new("kka").unwrap();
        let result = lex_romaji_convert(kana.as_ptr(), pending.as_ptr(), 0);
        let composed = unsafe { CStr::from_ptr(result.composed_kana).to_str().unwrap() };
        assert_eq!(composed, "っか");
        lex_romaji_convert_free(result);
    }

    #[test]
    fn test_ffi_romaji_convert_force_n() {
        let kana = CString::new("").unwrap();
        let pending = CString::new("n").unwrap();
        let result = lex_romaji_convert(kana.as_ptr(), pending.as_ptr(), 1);
        let composed = unsafe { CStr::from_ptr(result.composed_kana).to_str().unwrap() };
        assert_eq!(composed, "ん");
        lex_romaji_convert_free(result);
    }

    #[test]
    fn test_ffi_romaji_convert_collapse() {
        let kana = CString::new("kあ").unwrap();
        let pending = CString::new("").unwrap();
        let result = lex_romaji_convert(kana.as_ptr(), pending.as_ptr(), 0);
        let composed = unsafe { CStr::from_ptr(result.composed_kana).to_str().unwrap() };
        assert_eq!(composed, "か");
        lex_romaji_convert_free(result);
    }

    #[test]
    fn test_ffi_romaji_convert_null_safety() {
        // null composed_kana
        let pending = CString::new("ka").unwrap();
        let result = lex_romaji_convert(ptr::null(), pending.as_ptr(), 0);
        assert!(result.composed_kana.is_null());
        lex_romaji_convert_free(result);

        // null pending_romaji
        let kana = CString::new("あ").unwrap();
        let result = lex_romaji_convert(kana.as_ptr(), ptr::null(), 0);
        assert!(result.composed_kana.is_null());
        lex_romaji_convert_free(result);
    }

    // --- Unified Candidate Generation FFI tests ---

    #[test]
    fn test_ffi_generate_candidates_roundtrip() {
        let dict = make_test_dict();
        let reading = CString::new("かんじ").unwrap();

        let resp = lex_generate_candidates(dict, ptr::null(), ptr::null(), reading.as_ptr(), 10);
        assert!(
            resp.surfaces_len >= 1,
            "should return at least one candidate"
        );

        unsafe {
            let surfaces = std::slice::from_raw_parts(resp.surfaces, resp.surfaces_len as usize);
            // First candidate is Viterbi #1 (conversion result); kana should also be present
            let all: Vec<&str> = surfaces
                .iter()
                .map(|&p| CStr::from_ptr(p).to_str().unwrap())
                .collect();
            assert!(
                all.contains(&"かんじ"),
                "kana should be present in candidates: {:?}",
                all,
            );
        }

        lex_candidate_response_free(resp);
        lex_dict_close(dict);
    }

    #[test]
    fn test_ffi_generate_candidates_null_safety() {
        let reading = CString::new("かんじ").unwrap();

        // null dict
        let resp =
            lex_generate_candidates(ptr::null(), ptr::null(), ptr::null(), reading.as_ptr(), 10);
        assert_eq!(resp.surfaces_len, 0);
        lex_candidate_response_free(resp);

        // null reading
        let dict = make_test_dict();
        let resp = lex_generate_candidates(dict, ptr::null(), ptr::null(), ptr::null(), 10);
        assert_eq!(resp.surfaces_len, 0);
        lex_candidate_response_free(resp);

        lex_dict_close(dict);
    }

    #[test]
    fn test_ffi_generate_candidates_punctuation() {
        // 句読点候補生成テスト
        let dict = make_test_dict();
        let reading = CString::new("。").unwrap();

        let resp = lex_generate_candidates(dict, ptr::null(), ptr::null(), reading.as_ptr(), 10);
        assert!(
            resp.surfaces_len >= 1,
            "punctuation should return candidates"
        );

        unsafe {
            let surfaces = std::slice::from_raw_parts(resp.surfaces, resp.surfaces_len as usize);
            let s0 = CStr::from_ptr(surfaces[0]).to_str().unwrap();
            assert_eq!(s0, "。", "first candidate should be the punctuation itself");
        }

        lex_candidate_response_free(resp);
        lex_dict_close(dict);
    }

    // --- InputSession FFI tests ---

    #[test]
    fn test_ffi_session_roundtrip() {
        let dict = make_test_dict();
        let session = lex_session_new(dict, ptr::null(), ptr::null());
        assert!(!session.is_null());

        // Type "ka" → should produce か
        let k = CString::new("k").unwrap();
        let resp = lex_session_handle_key(session, 0, k.as_ptr(), 0);
        assert_eq!(resp.consumed, 1);
        assert_eq!(lex_session_is_composing(session), 1);
        lex_key_response_free(resp);

        let a = CString::new("a").unwrap();
        let resp = lex_session_handle_key(session, 0, a.as_ptr(), 0);
        assert_eq!(resp.consumed, 1);
        assert!(!resp.marked_text.is_null());
        lex_key_response_free(resp);

        // Commit with Enter (key code 36)
        let empty = CString::new("").unwrap();
        let resp = lex_session_handle_key(session, 36, empty.as_ptr(), 0);
        assert_eq!(resp.consumed, 1);
        assert!(!resp.commit_text.is_null());
        assert_eq!(lex_session_is_composing(session), 0);
        lex_key_response_free(resp);

        lex_session_free(session);
        lex_dict_close(dict);
    }

    #[test]
    fn test_ffi_session_null_safety() {
        // null session
        let resp = lex_session_handle_key(ptr::null_mut(), 0, ptr::null(), 0);
        assert_eq!(resp.consumed, 0);
        lex_key_response_free(resp);

        let resp = lex_session_commit(ptr::null_mut());
        assert_eq!(resp.consumed, 0);
        lex_key_response_free(resp);

        assert_eq!(lex_session_is_composing(ptr::null()), 0);

        // null dict → null session
        let session = lex_session_new(ptr::null(), ptr::null(), ptr::null());
        assert!(session.is_null());
    }

    #[test]
    fn test_ffi_session_with_history() {
        let dict = make_test_dict();
        let dir = std::env::temp_dir().join("lexime_test_ffi_session_hist");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.lxud");
        let path_cstr = CString::new(path.to_str().unwrap()).unwrap();

        let history = lex_history_open(path_cstr.as_ptr());
        assert!(!history.is_null());

        let session = lex_session_new(dict, ptr::null(), history);
        assert!(!session.is_null());

        // Type "kyou" and commit → should record history
        for ch in "kyou".chars() {
            let s = CString::new(ch.to_string()).unwrap();
            let resp = lex_session_handle_key(session, 0, s.as_ptr(), 0);
            lex_key_response_free(resp);
        }

        let empty = CString::new("").unwrap();
        let resp = lex_session_handle_key(session, 36, empty.as_ptr(), 0);
        assert_eq!(resp.consumed, 1);
        assert_eq!(resp.save_history, 1);
        // Record history entries
        lex_key_response_record_history(&resp, history);
        lex_key_response_free(resp);

        // Save history
        assert_eq!(lex_history_save(history, path_cstr.as_ptr()), 0);

        lex_session_free(session);
        lex_history_close(history);
        lex_dict_close(dict);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_ffi_session_programmer_mode() {
        let dict = make_test_dict();
        let session = lex_session_new(dict, ptr::null(), ptr::null());
        lex_session_set_programmer_mode(session, 1);

        // ¥ key (93) should produce backslash
        let yen = CString::new("¥").unwrap();
        let resp = lex_session_handle_key(session, 93, yen.as_ptr(), 0);
        assert_eq!(resp.consumed, 1);
        unsafe {
            let text = CStr::from_ptr(resp.commit_text).to_str().unwrap();
            assert_eq!(text, "\\");
        }
        lex_key_response_free(resp);

        lex_session_free(session);
        lex_dict_close(dict);
    }
}
