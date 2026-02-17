//! Romaji-to-kana conversion engine.
//!
//! Uses a trie-based lookup table to incrementally convert ASCII keystrokes
//! into hiragana, handling sokuon (っ), hatsuon (ん), and yōon (きゃ).

mod config;
mod convert;
mod table;
mod trie;

pub use config::{parse_romaji_toml, RomajiConfigError};
pub use convert::{convert_romaji, RomajiConvertResult};
pub use trie::{RomajiTrie, TrieLookupResult};

/// Returns the embedded default romaji TOML content.
pub fn default_toml() -> &'static str {
    table::DEFAULT_TOML
}
