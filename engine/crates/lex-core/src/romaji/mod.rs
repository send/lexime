//! Romaji-to-kana conversion engine.
//!
//! Uses a trie-based lookup table to incrementally convert ASCII keystrokes
//! into hiragana, handling sokuon (っ), hatsuon (ん), and yōon (きゃ).

mod convert;
mod table;
mod trie;

pub use convert::{convert_romaji, RomajiConvertResult};
pub use trie::{RomajiTrie, TrieLookupResult};
