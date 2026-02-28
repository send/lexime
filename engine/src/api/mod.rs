//! UniFFI export layer — type-safe Swift bindings for the Lexime engine.
//!
//! Each public type here maps to a generated Swift class, struct, or enum.

mod engine;
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
    LexCandidateResult, LexConversionMode, LexDictEntry, LexError, LexEvent, LexKeyEvent,
    LexKeyResponse, LexRomajiConvert, LexRomajiLookup, LexSegment, LexSnippetEntry, LexTriggerKey,
    LexUserWord,
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

/// Parse snippets.toml into a flat list for UI display.
///
/// This intentionally performs only TOML syntax parsing without variable
/// validation.  Variable references are validated at load time by
/// `snippets_load()`, which is called on save via `reloadSnippets()`.
/// Keeping this function lightweight lets the settings UI display raw
/// entries (including those with invalid variable references) so users
/// can see and fix them.
#[uniffi::export]
fn snippets_parse(content: String) -> Result<Vec<LexSnippetEntry>, LexError> {
    let table: std::collections::HashMap<String, String> =
        toml::from_str(&content).map_err(|e| LexError::InvalidData { msg: e.to_string() })?;
    let mut entries: Vec<LexSnippetEntry> = table
        .into_iter()
        .map(|(key, body)| LexSnippetEntry { key, body })
        .collect();
    entries.sort_by(|a, b| a.key.cmp(&b.key));
    Ok(entries)
}

#[uniffi::export]
fn snippets_serialize(entries: Vec<LexSnippetEntry>) -> String {
    let mut sorted = entries;
    sorted.sort_by(|a, b| a.key.cmp(&b.key));
    let mut out = String::new();
    for entry in &sorted {
        let key = if entry
            .key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            && !entry.key.is_empty()
        {
            entry.key.clone()
        } else {
            format!("{}", toml::Value::String(entry.key.clone()))
        };
        out.push_str(&format!(
            "{} = {}\n",
            key,
            toml::Value::String(entry.body.clone())
        ));
    }
    out
}

#[uniffi::export]
fn snippets_load(path: String) -> Result<std::sync::Arc<LexSnippetStore>, LexError> {
    use lex_core::snippets::{parse_snippets_toml, SnippetStore, VariableResolver};

    let content = std::fs::read_to_string(&path).map_err(|e| LexError::Io {
        msg: format!("{path}: {e}"),
    })?;

    let settings = crate::settings::settings();
    let resolver = VariableResolver::new(settings.snippets.variables.clone());
    let known = resolver.known_names();
    let entries = parse_snippets_toml(&content, &known)
        .map_err(|e| LexError::InvalidData { msg: e.to_string() })?;

    let store = SnippetStore::new(entries, resolver);
    Ok(LexSnippetStore::new(std::sync::Arc::new(store)))
}
