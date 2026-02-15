//! BPE tokenizer for Zenzai GPT-2, loaded from GGUF metadata.
//!
//! The GGUF file embeds `tokenizer.ggml.tokens` and `tokenizer.ggml.merges`
//! which we parse to build a GPT-2-style **byte-level** BPE encoder.
//!
//! GPT-2 BPE encodes text as UTF-8 bytes, then maps each byte to a
//! displayable Unicode character via a fixed table. BPE merges operate
//! on these mapped characters.

use std::collections::HashMap;

use candle_core::quantized::gguf_file;

/// Zenzai special characters.
pub const CHAR_INPUT: char = '\u{EE00}';
pub const CHAR_OUTPUT: char = '\u{EE01}';
pub const CHAR_CONTEXT: char = '\u{EE02}';

/// Special token IDs (Zenzai convention).
const UNK_ID: u32 = 0;
const EOS_ID: u32 = 3;

/// Special token strings that should be matched atomically.
const SPECIAL_TOKENS: &[&str] = &["[UNK]", "[PAD]", "<s>", "</s>"];

pub struct BpeTokenizer {
    /// token string → token ID
    token_to_id: HashMap<String, u32>,
    /// token ID → token string
    id_to_token: Vec<String>,
    /// Ordered merge rules: (left, right, merged)
    merges: Vec<(String, String, String)>,
    /// byte value → GPT-2 mapped character
    byte_to_char: [char; 256],
    /// GPT-2 mapped character → byte value
    char_to_byte: HashMap<char, u8>,
}

impl BpeTokenizer {
    /// Build the tokenizer from GGUF metadata.
    pub fn from_gguf(content: &gguf_file::Content) -> anyhow::Result<Self> {
        let tokens = get_string_array(&content.metadata, "tokenizer.ggml.tokens")?;
        let merge_strs = get_string_array(&content.metadata, "tokenizer.ggml.merges")?;

        let mut token_to_id = HashMap::with_capacity(tokens.len());
        let mut id_to_token = Vec::with_capacity(tokens.len());
        for (i, tok) in tokens.iter().enumerate() {
            token_to_id.insert(tok.clone(), i as u32);
            id_to_token.push(tok.clone());
        }

        let merges: Vec<(String, String, String)> = merge_strs
            .iter()
            .filter_map(|line| {
                let mut parts = line.splitn(2, ' ');
                let left = parts.next()?.to_string();
                let right = parts.next()?.to_string();
                let merged = format!("{left}{right}");
                Some((left, right, merged))
            })
            .collect();

        let (byte_to_char, char_to_byte) = build_byte_mapping();

        Ok(Self {
            token_to_id,
            id_to_token,
            merges,
            byte_to_char,
            char_to_byte,
        })
    }

    /// Encode text into token IDs using GPT-2 byte-level BPE.
    pub fn encode(&self, text: &str) -> Vec<u32> {
        if text.is_empty() {
            return Vec::new();
        }

        // 1. Extract special tokens first, splitting around them.
        let segments = split_special_tokens(text);

        let mut all_ids = Vec::new();
        for segment in segments {
            if let Some(&id) = self.token_to_id.get(segment.as_str()) {
                // Atomic special token
                all_ids.push(id);
            } else {
                // Byte-level BPE encode
                let ids = self.bpe_encode_segment(&segment);
                all_ids.extend(ids);
            }
        }

        all_ids
    }

    /// Decode token IDs back to text.
    ///
    /// Reverses the byte-level mapping: concatenates token strings,
    /// then converts the GPT-2 mapped characters back to UTF-8 bytes.
    pub fn decode(&self, tokens: &[u32]) -> String {
        let mapped: String = tokens
            .iter()
            .filter_map(|&id| self.id_to_token.get(id as usize))
            .cloned()
            .collect();

        // Check if it looks like a special token (starts with [ or <)
        // For tokens like </s>, [UNK], etc., return as-is
        // For normal text, reverse the byte mapping
        let bytes: Vec<u8> = mapped
            .chars()
            .map(|c| self.char_to_byte.get(&c).copied().unwrap_or(b'?'))
            .collect();

        String::from_utf8(bytes).unwrap_or(mapped)
    }

    /// Decode token IDs back to the raw GPT-2 token strings (no byte mapping reversal).
    pub fn decode_raw(&self, tokens: &[u32]) -> String {
        tokens
            .iter()
            .filter_map(|&id| self.id_to_token.get(id as usize))
            .cloned()
            .collect()
    }

    /// EOS token ID (</s> = 3).
    pub fn eos_token(&self) -> u32 {
        EOS_ID
    }

    /// Vocabulary size.
    pub fn vocab_size(&self) -> usize {
        self.id_to_token.len()
    }

    /// BPE encode a text segment (no special tokens).
    fn bpe_encode_segment(&self, text: &str) -> Vec<u32> {
        // Convert to byte-level symbols using GPT-2 mapping.
        let mut symbols: Vec<String> = text
            .as_bytes()
            .iter()
            .map(|&b| self.byte_to_char[b as usize].to_string())
            .collect();

        // Apply BPE merges greedily in priority order.
        for (left, right, merged) in &self.merges {
            let mut i = 0;
            while i + 1 < symbols.len() {
                if symbols[i] == *left && symbols[i + 1] == *right {
                    symbols[i] = merged.clone();
                    symbols.remove(i + 1);
                    // Re-check from the previous position
                    i = i.saturating_sub(1);
                } else {
                    i += 1;
                }
            }
        }

        // Map to IDs.
        symbols
            .iter()
            .map(|s| self.token_to_id.get(s).copied().unwrap_or(UNK_ID))
            .collect()
    }
}

/// Build the GPT-2 byte-to-unicode mapping table.
///
/// GPT-2 maps bytes 0-255 to displayable Unicode characters:
/// - Printable ASCII (33-126), Latin-1 supplement (161-172, 174-255)
///   are mapped to themselves.
/// - All other bytes (0-32, 127-160, 173) are mapped to U+0100..U+0143
///   to avoid control characters in the token vocabulary.
fn build_byte_mapping() -> ([char; 256], HashMap<char, u8>) {
    let mut byte_to_char = ['\0'; 256];
    let mut char_to_byte = HashMap::new();

    // Directly mapped bytes: printable ranges
    let mut direct: Vec<u8> = Vec::new();
    direct.extend(33u8..=126); // ASCII printable (excluding space)
    direct.extend(161u8..=172); // ¡ through ¬
    direct.extend(174u8..=255); // ® through ÿ

    for &b in &direct {
        let c = b as char;
        byte_to_char[b as usize] = c;
        char_to_byte.insert(c, b);
    }

    // Remapped bytes: control chars and other non-printable
    let mut remap_idx: u32 = 256; // Start at U+0100
    for b in 0u16..=255 {
        let b = b as u8;
        if byte_to_char[b as usize] == '\0' {
            let c = char::from_u32(remap_idx).unwrap();
            byte_to_char[b as usize] = c;
            char_to_byte.insert(c, b);
            remap_idx += 1;
        }
    }

    (byte_to_char, char_to_byte)
}

/// Split text into segments, isolating special tokens as separate items.
fn split_special_tokens(text: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut remaining = text;

    while !remaining.is_empty() {
        // Try to find the earliest special token match
        let mut earliest: Option<(usize, &str)> = None;
        for &special in SPECIAL_TOKENS {
            if let Some(pos) = remaining.find(special) {
                if earliest.is_none() || pos < earliest.unwrap().0 {
                    earliest = Some((pos, special));
                }
            }
        }

        match earliest {
            Some((pos, special)) => {
                if pos > 0 {
                    segments.push(remaining[..pos].to_string());
                }
                segments.push(special.to_string());
                remaining = &remaining[pos + special.len()..];
            }
            None => {
                segments.push(remaining.to_string());
                break;
            }
        }
    }

    segments
}

/// Extract a string array from GGUF metadata.
fn get_string_array(
    metadata: &HashMap<String, gguf_file::Value>,
    key: &str,
) -> anyhow::Result<Vec<String>> {
    let value = metadata
        .get(key)
        .ok_or_else(|| anyhow::anyhow!("missing GGUF metadata key: {key}"))?;
    let arr = value
        .to_vec()
        .map_err(|e| anyhow::anyhow!("metadata key {key} is not an array: {e}"))?;
    arr.iter()
        .map(|v| {
            v.to_string()
                .cloned()
                .map_err(|e| anyhow::anyhow!("non-string element in {key}: {e}"))
        })
        .collect()
}

/// Convert hiragana to katakana (for Zenzai input format).
pub fn hiragana_to_katakana(s: &str) -> String {
    s.chars()
        .map(|c| {
            if ('\u{3040}'..='\u{309F}').contains(&c) {
                char::from_u32(c as u32 + 0x60).unwrap_or(c)
            } else {
                c
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- byte mapping tests ---

    #[test]
    fn test_byte_mapping_roundtrip() {
        let (byte_to_char, char_to_byte) = build_byte_mapping();
        // Every byte should map to a unique char and back
        for b in 0u16..=255 {
            let b = b as u8;
            let c = byte_to_char[b as usize];
            assert_ne!(c, '\0', "byte {b} should be mapped");
            assert_eq!(char_to_byte[&c], b, "char {c} should map back to byte {b}");
        }
        // All 256 chars should be unique
        let mut chars: Vec<char> = byte_to_char.to_vec();
        chars.sort();
        chars.dedup();
        assert_eq!(chars.len(), 256);
    }

    #[test]
    fn test_byte_mapping_ascii() {
        let (byte_to_char, _) = build_byte_mapping();
        // Printable ASCII should map to themselves
        assert_eq!(byte_to_char[b'A' as usize], 'A');
        assert_eq!(byte_to_char[b'z' as usize], 'z');
        assert_eq!(byte_to_char[b'!' as usize], '!');
        // Space (32) is NOT directly mapped
        assert_ne!(byte_to_char[b' ' as usize], ' ');
    }

    #[test]
    fn test_split_special_tokens() {
        let result = split_special_tokens("hello</s>");
        assert_eq!(result, vec!["hello", "</s>"]);

        let result = split_special_tokens("</s>");
        assert_eq!(result, vec!["</s>"]);

        let result = split_special_tokens("abc<s>def</s>");
        assert_eq!(result, vec!["abc", "<s>", "def", "</s>"]);

        let result = split_special_tokens("no specials here");
        assert_eq!(result, vec!["no specials here"]);

        let result = split_special_tokens("");
        assert!(result.is_empty());
    }

    #[test]
    fn test_hiragana_to_katakana() {
        assert_eq!(hiragana_to_katakana("きょうは"), "キョウハ");
        assert_eq!(hiragana_to_katakana("らーめん"), "ラーメン");
        assert_eq!(hiragana_to_katakana("abc"), "abc");
    }

    // --- GGUF integration tests (require model file) ---

    #[test]
    #[ignore]
    fn inspect_gguf_vocab() {
        let model_path = std::path::Path::new(
            "/Users/kazuaki.sakai/repos/send.sh/lexime/data/zenz-v3.1-Q5_K_M.gguf",
        );
        if !model_path.exists() {
            println!("GGUF file not found, skipping");
            return;
        }

        let mut file = std::fs::File::open(model_path).expect("failed to open GGUF");
        let content = candle_core::quantized::gguf_file::Content::read(&mut file)
            .expect("failed to read GGUF content");

        let tok = BpeTokenizer::from_gguf(&content).expect("failed to build tokenizer");
        println!("Vocab size: {}", tok.vocab_size());

        // Test encoding of Japanese text
        let text = "今日はいい天気です";
        let ids = tok.encode(text);
        println!("encode('{text}'): {ids:?}");
        let decoded = tok.decode(&ids);
        println!("decode back: '{decoded}'");
        assert_eq!(decoded, text, "encode/decode roundtrip failed");

        // Test full Zenzai prompt
        let prompt = "\u{EE02}\u{EE00}キョウハイイテンキデス\u{EE01}今日はいい天気です</s>";
        let ids = tok.encode(prompt);
        println!("prompt IDs ({} tokens): {:?}", ids.len(), ids);
        // Should not contain UNK (0) for known characters
        let unk_count = ids.iter().filter(|&&id| id == 0).count();
        println!("UNK count: {unk_count}");
        assert_eq!(unk_count, 0, "prompt should not contain UNK tokens");

        // Verify </s> is a single token at the end
        assert_eq!(*ids.last().unwrap(), EOS_ID, "last token should be EOS");

        // Test different outputs produce different encodings
        let ids_a = tok.encode("今日はいい天気です");
        let ids_b = tok.encode("教派いい天気です");
        assert_ne!(
            ids_a, ids_b,
            "different text should produce different token IDs"
        );
        println!("'今日はいい天気です' => {} tokens", ids_a.len());
        println!("'教派いい天気です'  => {} tokens", ids_b.len());
    }
}
