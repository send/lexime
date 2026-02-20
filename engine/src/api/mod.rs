//! UniFFI export layer — type-safe Swift bindings for the Lexime engine.
//!
//! Each public type here maps to a generated Swift class, struct, or enum.

mod engine;
mod resources;
mod session;
mod types;
mod user_dict;

pub use engine::LexEngine;
pub use resources::{LexConnection, LexDictionary, LexUserHistory};
pub use session::LexSession;
pub use types::{
    LexCandidateResult, LexDictEntry, LexError, LexEvent, LexKeyEvent, LexKeyResponse,
    LexRomajiConvert, LexRomajiLookup, LexSegment, LexUserWord,
};
pub use user_dict::LexUserDictionary;

use std::path::Path;

use crate::romaji::{convert_romaji, RomajiTrie, TrieLookupResult};

// ---------------------------------------------------------------------------
// Top-level functions
// ---------------------------------------------------------------------------

#[uniffi::export]
fn engine_version() -> String {
    "0.1.0".to_string()
}

#[uniffi::export]
fn romaji_lookup(romaji: String) -> LexRomajiLookup {
    let trie = RomajiTrie::global();
    match trie.lookup(&romaji) {
        TrieLookupResult::None => LexRomajiLookup::None,
        TrieLookupResult::Prefix => LexRomajiLookup::Prefix,
        TrieLookupResult::Exact(kana) => LexRomajiLookup::Exact { kana },
        TrieLookupResult::ExactAndPrefix(kana) => LexRomajiLookup::ExactAndPrefix { kana },
    }
}

#[uniffi::export]
fn romaji_convert(kana: String, pending: String, force: bool) -> LexRomajiConvert {
    let result = convert_romaji(&kana, &pending, force);
    LexRomajiConvert {
        composed_kana: result.composed_kana,
        pending_romaji: result.pending_romaji,
    }
}

#[uniffi::export]
fn romaji_load_config(path: String) -> Result<(), LexError> {
    let content = std::fs::read_to_string(&path).map_err(|e| LexError::Io {
        msg: format!("{path}: {e}"),
    })?;
    RomajiTrie::init_custom(content).map_err(|e| LexError::InvalidData { msg: e.to_string() })?;
    Ok(())
}

#[uniffi::export]
fn settings_load_config(path: String) -> Result<(), LexError> {
    let content = std::fs::read_to_string(&path).map_err(|e| LexError::Io {
        msg: format!("{path}: {e}"),
    })?;
    crate::settings::init_custom(content)
        .map_err(|e| LexError::InvalidData { msg: e.to_string() })?;
    Ok(())
}

#[uniffi::export]
fn romaji_default_config() -> String {
    crate::romaji::default_toml().to_string()
}

#[uniffi::export]
fn settings_default_config() -> String {
    crate::settings::DEFAULT_SETTINGS_TOML.to_string()
}

#[uniffi::export]
fn keymap_get(key_code: u16, has_shift: bool) -> Option<String> {
    crate::settings::settings()
        .keymap_get(key_code, has_shift)
        .map(|s| s.to_string())
}

#[uniffi::export]
fn trace_init(log_dir: String) {
    crate::trace_init::init_tracing(Path::new(&log_dir));
}
