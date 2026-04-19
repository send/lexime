//! UniFFI export layer — type-safe Swift bindings for the Lexime engine.
//!
//! Each public type here maps to a generated Swift class, struct, or enum.

mod engine;
mod mapping;
mod resources;
mod session;
mod snippet_store;
mod types;
mod user_dict;

pub use engine::LexEngine;
pub use resources::{LexConnection, LexDictionary, LexUserHistory};
pub use session::LexSession;
pub use snippet_store::LexSnippetStore;
pub use types::{
    LexConversionMode, LexDictEntry, LexError, LexEvent, LexKeyEvent, LexKeyResponse,
    LexRomajiConvert, LexRomajiLookup, LexSnippetEntry, LexTriggerKey, LexUserWord,
};
pub use user_dict::LexUserDictionary;

use std::path::Path;

use crate::romaji::{convert_romaji, RomajiTrie, TrieLookupResult};

// ---------------------------------------------------------------------------
// Top-level functions
// ---------------------------------------------------------------------------

#[uniffi::export]
fn engine_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
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

#[uniffi::export]
fn snippet_trigger_key() -> Option<LexTriggerKey> {
    crate::settings::settings()
        .snippet_trigger()
        .map(|t| LexTriggerKey {
            char_: t.char,
            ctrl: t.ctrl,
            shift: t.shift,
            alt: t.alt,
            cmd: t.cmd,
        })
}

/// Build a `LexSnippetStore` from entries parsed by the Swift layer.
///
/// Swift now owns snippets.toml I/O and TOML parsing; Rust only validates
/// variable references against settings-defined variables and wraps the
/// entries in a resolver-equipped store.
#[uniffi::export]
fn snippets_build_store(
    entries: Vec<LexSnippetEntry>,
) -> Result<std::sync::Arc<LexSnippetStore>, LexError> {
    use lex_core::snippets::{validate_snippet_entries, SnippetStore, VariableResolver};

    let settings = crate::settings::settings();
    let resolver = VariableResolver::new(settings.snippets.variables.clone());
    let known = resolver.known_names();

    let mut map: std::collections::HashMap<String, String> =
        std::collections::HashMap::with_capacity(entries.len());
    for LexSnippetEntry { key, body } in entries {
        match map.entry(key) {
            std::collections::hash_map::Entry::Vacant(vacant) => {
                vacant.insert(body);
            }
            std::collections::hash_map::Entry::Occupied(occupied) => {
                return Err(LexError::InvalidData {
                    msg: format!(
                        "duplicate snippet key: \"{}\"",
                        occupied.key().escape_default()
                    ),
                });
            }
        }
    }
    validate_snippet_entries(&map, &known)
        .map_err(|e| LexError::InvalidData { msg: e.to_string() })?;

    let store = SnippetStore::new(map, resolver);
    Ok(LexSnippetStore::new(std::sync::Arc::new(store)))
}
