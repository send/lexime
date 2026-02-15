//! Neural scoring for IME conversion using Zenzai GPT-2.
//!
//! This module provides neural language model scoring for re-ranking
//! Viterbi N-best candidates. It loads a GGUF quantized GPT-2 model
//! and computes log-probabilities for candidate strings.

mod gpt2;
mod tokenizer;

use std::path::Path;

use candle_core::{Device, IndexOp, Tensor};

use crate::converter::ConvertedSegment;

pub use gpt2::QuantizedGpt2;
pub use tokenizer::{hiragana_to_katakana, BpeTokenizer, CHAR_CONTEXT, CHAR_INPUT, CHAR_OUTPUT};

pub struct NeuralScorer {
    model: QuantizedGpt2,
    tokenizer: BpeTokenizer,
    device: Device,
}

impl NeuralScorer {
    /// Load a neural scorer from a GGUF model file.
    pub fn open(model_path: &Path) -> anyhow::Result<Self> {
        let device = Device::Cpu;

        // Read GGUF content for both model weights and tokenizer metadata
        let mut file = std::fs::File::open(model_path)?;
        let content = candle_core::quantized::gguf_file::Content::read(&mut file)
            .map_err(|e| anyhow::anyhow!("failed to read GGUF: {e}"))?;

        let tokenizer = BpeTokenizer::from_gguf(&content)?;
        drop(file); // Close file before loading model (which reopens it)
        let model = QuantizedGpt2::from_gguf(model_path, &device)?;

        Ok(Self {
            model,
            tokenizer,
            device,
        })
    }

    /// Compute the log-probability of `output` given `context` and `kana`.
    ///
    /// Builds the Zenzai prompt:
    ///   `\uEE02{context}\uEE00{katakana}\uEE01{output}</s>`
    /// and sums the log-probabilities of the output tokens.
    pub fn score_text(&mut self, context: &str, kana: &str, output: &str) -> anyhow::Result<f64> {
        let prompt = build_prompt(context, kana, output);
        let tokens = self.tokenizer.encode(&prompt);

        if tokens.is_empty() {
            return Ok(f64::NEG_INFINITY);
        }

        // Find the position of U+EE01 (output marker)
        let output_marker_tokens = self.tokenizer.encode(&CHAR_OUTPUT.to_string());
        let output_start = find_subsequence(&tokens, &output_marker_tokens)
            .ok_or_else(|| anyhow::anyhow!("output marker not found in tokenized prompt"))?
            + output_marker_tokens.len();

        // Forward pass through entire sequence
        self.model.reset_kv_cache();
        let logits_all = self.forward_all(&tokens)?;

        // Sum log-probabilities for output tokens (from output_start to end)
        let mut log_prob = 0.0;
        for i in output_start..tokens.len() {
            // logits at position i-1 predict token at position i
            let logits = &logits_all[i - 1];
            let lp = log_softmax_at(logits, tokens[i], &self.device)?;
            log_prob += lp;
        }

        Ok(log_prob)
    }

    /// Score multiple N-best paths and return them sorted by neural score (descending).
    ///
    /// Uses KV-cache prefix sharing: the common prefix (context + kana + output marker)
    /// is processed once, then each candidate's output tokens are scored individually
    /// by restoring the cached prefix state.
    ///
    /// Returns `Vec<(path_index, log_prob)>` sorted by `log_prob` descending.
    pub fn score_paths(
        &mut self,
        context: &str,
        kana: &str,
        paths: &[Vec<ConvertedSegment>],
    ) -> anyhow::Result<Vec<(usize, f64)>> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }

        // Build and process the shared prefix: \uEE02{context}\uEE00{katakana}\uEE01
        let katakana = hiragana_to_katakana(kana);
        let katakana = katakana.replace(' ', "\u{3000}");
        let context = context.replace(' ', "\u{3000}");
        let prefix = format!("{CHAR_CONTEXT}{context}{CHAR_INPUT}{katakana}{CHAR_OUTPUT}");
        let prefix_tokens = self.tokenizer.encode(&prefix);

        if prefix_tokens.is_empty() {
            return Ok(paths
                .iter()
                .enumerate()
                .map(|(i, _)| (i, f64::NEG_INFINITY))
                .collect());
        }

        // Forward pass for the shared prefix (builds KV-cache)
        self.model.reset_kv_cache();
        let mut prefix_logits = None;
        for (i, &token) in prefix_tokens.iter().enumerate() {
            let logits = self
                .model
                .forward(&[token], i)
                .map_err(|e| anyhow::anyhow!("prefix forward at position {i} failed: {e}"))?;
            prefix_logits = Some(logits);
        }
        let prefix_logits =
            prefix_logits.ok_or_else(|| anyhow::anyhow!("empty prefix after encoding"))?;

        // Snapshot the KV-cache after processing the prefix
        let kv_snapshot = self.model.save_kv_cache();
        let prefix_len = prefix_tokens.len();

        // Score each candidate by restoring the prefix cache
        let mut scores: Vec<(usize, f64)> = Vec::with_capacity(paths.len());

        for (i, path) in paths.iter().enumerate() {
            let output: String = path.iter().map(|s| s.surface.as_str()).collect();
            let output = output.replace(' ', "\u{3000}");
            let output_with_eos = format!("{output}</s>");
            let output_tokens = self.tokenizer.encode(&output_with_eos);

            if output_tokens.is_empty() {
                scores.push((i, f64::NEG_INFINITY));
                continue;
            }

            // Restore KV-cache to the prefix state
            self.model.restore_kv_cache(&kv_snapshot);

            // First output token scored from prefix logits
            let mut log_prob = log_softmax_at(&prefix_logits, output_tokens[0], &self.device)?;

            // Forward remaining output tokens one by one
            for j in 0..output_tokens.len() - 1 {
                let logits = self
                    .model
                    .forward(&[output_tokens[j]], prefix_len + j)
                    .map_err(|e| {
                        anyhow::anyhow!("output forward at position {} failed: {e}", prefix_len + j)
                    })?;
                log_prob += log_softmax_at(&logits, output_tokens[j + 1], &self.device)?;
            }

            scores.push((i, log_prob));
        }

        // Sort by log_prob descending (higher = better)
        scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        Ok(scores)
    }

    /// Get model configuration summary.
    pub fn config_summary(&self) -> String {
        self.model.config_summary()
    }

    /// Run forward pass for all positions and collect per-position logits.
    fn forward_all(&mut self, tokens: &[u32]) -> anyhow::Result<Vec<Tensor>> {
        let mut logits_list = Vec::with_capacity(tokens.len());

        // Process token-by-token with KV-cache for scoring.
        // We need logits at every position to compute per-token log-probabilities.
        self.model.reset_kv_cache();

        for (i, &token) in tokens.iter().enumerate() {
            let logits = self
                .model
                .forward(&[token], i)
                .map_err(|e| anyhow::anyhow!("forward pass at position {i} failed: {e}"))?;
            logits_list.push(logits);
        }

        Ok(logits_list)
    }
}

/// Build the Zenzai v3 prompt format.
///
/// Format: `\uEE02{context}\uEE00{katakana}\uEE01{output}</s>`
pub fn build_prompt(context: &str, kana: &str, output: &str) -> String {
    let katakana = hiragana_to_katakana(kana);
    // Replace ASCII spaces with fullwidth spaces (U+0020 → U+3000)
    let katakana = katakana.replace(' ', "\u{3000}");
    let output = output.replace(' ', "\u{3000}");
    let context = context.replace(' ', "\u{3000}");

    format!("{CHAR_CONTEXT}{context}{CHAR_INPUT}{katakana}{CHAR_OUTPUT}{output}</s>")
}

/// Find the starting index of `needle` in `haystack`.
fn find_subsequence(haystack: &[u32], needle: &[u32]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Compute log_softmax for a specific token at a given logits vector.
fn log_softmax_at(logits: &Tensor, token_id: u32, _device: &Device) -> anyhow::Result<f64> {
    let log_probs = candle_nn::ops::log_softmax(logits, 0)
        .map_err(|e| anyhow::anyhow!("log_softmax failed: {e}"))?;
    let val = log_probs
        .i(token_id as usize)
        .map_err(|e| anyhow::anyhow!("index failed: {e}"))?
        .to_scalar::<f32>()
        .map_err(|e| anyhow::anyhow!("to_scalar failed: {e}"))?;
    Ok(val as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_prompt() {
        let prompt = build_prompt("", "きょうはいいてんきです", "今日はいい天気です");
        assert_eq!(
            prompt,
            "\u{EE02}\u{EE00}キョウハイイテンキデス\u{EE01}今日はいい天気です</s>"
        );
    }

    #[test]
    fn test_build_prompt_with_context() {
        let prompt = build_prompt("東京は", "きょうはいいてんきです", "今日はいい天気です");
        assert_eq!(
            prompt,
            "\u{EE02}東京は\u{EE00}キョウハイイテンキデス\u{EE01}今日はいい天気です</s>"
        );
    }

    #[test]
    fn test_build_prompt_space_replacement() {
        let prompt = build_prompt("a b", "a b", "a b");
        assert!(prompt.contains('\u{3000}'));
        assert!(!prompt.contains(' '));
    }

    #[test]
    fn test_find_subsequence() {
        assert_eq!(find_subsequence(&[1, 2, 3, 4, 5], &[3, 4]), Some(2));
        assert_eq!(find_subsequence(&[1, 2, 3], &[4, 5]), None);
        assert_eq!(find_subsequence(&[1, 2, 3], &[1]), Some(0));
    }
}
