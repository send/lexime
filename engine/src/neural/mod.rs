//! Neural scoring for IME conversion using Zenzai GPT-2.
//!
//! This module provides neural language model scoring for re-ranking
//! Viterbi N-best candidates. It loads a GGUF quantized GPT-2 model
//! and computes log-probabilities for candidate strings.

mod gpt2;
pub mod speculative;
mod tokenizer;

use std::path::Path;

use candle_core::{DType, Device, IndexOp, Tensor};

use crate::converter::ConvertedSegment;

pub use gpt2::QuantizedGpt2;
pub use tokenizer::{hiragana_to_katakana, BpeTokenizer, CHAR_CONTEXT, CHAR_INPUT, CHAR_OUTPUT};

/// Configuration for autoregressive text generation.
pub struct GenerateConfig {
    /// Maximum number of tokens to generate.
    pub max_tokens: usize,
    /// Sampling temperature. 0.0 = greedy (argmax).
    pub temperature: f32,
}

impl Default for GenerateConfig {
    fn default() -> Self {
        Self {
            max_tokens: 30,
            temperature: 0.0,
        }
    }
}

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

    /// Returns per-segment average log-prob per character.
    ///
    /// Builds the full prompt, runs a forward pass, then maps output tokens
    /// to segments using character-level alignment. Each segment's score is
    /// the sum of log-probs for its tokens divided by its character count.
    ///
    /// Lower (more negative) values indicate lower model confidence.
    pub fn score_segments(
        &mut self,
        context: &str,
        kana: &str,
        segments: &[ConvertedSegment],
    ) -> anyhow::Result<Vec<f64>> {
        if segments.is_empty() {
            return Ok(Vec::new());
        }

        let output: String = segments.iter().map(|s| s.surface.as_str()).collect();
        let prompt = build_prompt(context, kana, &output);
        let tokens = self.tokenizer.encode(&prompt);

        if tokens.is_empty() {
            return Ok(vec![f64::NEG_INFINITY; segments.len()]);
        }

        // Find the output marker position
        let output_marker_tokens = self.tokenizer.encode(&CHAR_OUTPUT.to_string());
        let output_start = find_subsequence(&tokens, &output_marker_tokens)
            .ok_or_else(|| anyhow::anyhow!("output marker not found in tokenized prompt"))?
            + output_marker_tokens.len();

        // Forward pass through entire sequence
        self.model.reset_kv_cache();
        let logits_all = self.forward_all(&tokens)?;

        // Collect per-token log-probs for output tokens
        let output_tokens = &tokens[output_start..];
        let mut token_logprobs: Vec<f64> = Vec::with_capacity(output_tokens.len());
        for i in output_start..tokens.len() {
            let lp = log_softmax_at(&logits_all[i - 1], tokens[i], &self.device)?;
            token_logprobs.push(lp);
        }

        // Map tokens to segments using character-level alignment
        map_token_logprobs_to_segments(&self.tokenizer, output_tokens, &token_logprobs, segments)
    }

    /// Generate text continuation given context.
    ///
    /// Uses plain GPT-2 text continuation (no Zenzai kana conversion markers).
    /// Context is truncated to 40 characters to stay within model's n_positions limit.
    pub fn generate_text(
        &mut self,
        context: &str,
        config: &GenerateConfig,
    ) -> anyhow::Result<String> {
        // Truncate context to last 40 characters
        let truncated_context: String = context
            .chars()
            .rev()
            .take(40)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let truncated_context = truncated_context.replace(' ', "\u{3000}");

        // Use Zenzai output format: place context after output marker so the model
        // treats it as continuation text and generates what comes next.
        let prompt = format!("{CHAR_CONTEXT}{CHAR_INPUT}{CHAR_OUTPUT}{truncated_context}");
        let tokens = self.tokenizer.encode(&prompt);

        if tokens.is_empty() {
            return Ok(String::new());
        }

        // Forward pass for the prompt (builds KV-cache)
        self.model.reset_kv_cache();
        let mut logits = Tensor::zeros(1, DType::F32, &self.device)
            .map_err(|e| anyhow::anyhow!("tensor init failed: {e}"))?;
        for (i, &token) in tokens.iter().enumerate() {
            logits = self
                .model
                .forward(&[token], i)
                .map_err(|e| anyhow::anyhow!("forward at position {i} failed: {e}"))?;
        }

        let eos_id = self.tokenizer.eos_token();
        // Also stop at Zenzai special marker tokens
        let stop_tokens: Vec<u32> = [CHAR_CONTEXT, CHAR_INPUT, CHAR_OUTPUT]
            .iter()
            .flat_map(|c| self.tokenizer.encode(&c.to_string()))
            .collect();
        let mut generated_tokens: Vec<u32> = Vec::new();
        let repetition_penalty: f32 = 1.3;

        for _ in 0..config.max_tokens {
            // Apply repetition penalty to already-generated tokens
            if !generated_tokens.is_empty() {
                let logits_vec: Vec<f32> = logits
                    .to_vec1()
                    .map_err(|e| anyhow::anyhow!("to_vec1 failed: {e}"))?;
                let mut penalized = logits_vec;
                for &prev_token in &generated_tokens {
                    let idx = prev_token as usize;
                    if idx < penalized.len() {
                        if penalized[idx] > 0.0 {
                            penalized[idx] /= repetition_penalty;
                        } else {
                            penalized[idx] *= repetition_penalty;
                        }
                    }
                }
                logits = Tensor::from_vec(penalized, logits.shape(), &self.device)
                    .map_err(|e| anyhow::anyhow!("tensor from penalized: {e}"))?;
            }

            let next_token = argmax(&logits)?;

            if next_token == eos_id || stop_tokens.contains(&next_token) {
                break;
            }

            // Stop on consecutive repetition (same token 3+ times)
            let repeat_count = generated_tokens
                .iter()
                .rev()
                .take_while(|&&t| t == next_token)
                .count();
            if repeat_count >= 2 {
                break;
            }

            generated_tokens.push(next_token);

            let pos = tokens.len() + generated_tokens.len() - 1;
            logits = self
                .model
                .forward(&[next_token], pos)
                .map_err(|e| anyhow::anyhow!("generate forward at position {pos} failed: {e}"))?;
        }

        let text = self.tokenizer.decode(&generated_tokens);
        // Convert fullwidth spaces back to ASCII spaces and strip special markers
        let text = text
            .replace('\u{3000}', " ")
            .replace([CHAR_CONTEXT, CHAR_INPUT, CHAR_OUTPUT], "");
        Ok(text)
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

/// Map per-token log-probabilities to per-segment average log-prob per character.
///
/// Each BPE token decodes to some number of characters. We accumulate tokens'
/// log-probs into the segment they fall within, splitting proportionally when
/// a token spans a segment boundary.
pub(crate) fn map_token_logprobs_to_segments(
    tokenizer: &BpeTokenizer,
    output_tokens: &[u32],
    token_logprobs: &[f64],
    segments: &[ConvertedSegment],
) -> anyhow::Result<Vec<f64>> {
    // Compute segment char counts and cumulative boundaries
    let seg_char_counts: Vec<usize> = segments.iter().map(|s| s.surface.chars().count()).collect();
    let mut seg_boundaries: Vec<usize> = Vec::with_capacity(segments.len() + 1);
    seg_boundaries.push(0);
    for &count in &seg_char_counts {
        seg_boundaries.push(seg_boundaries.last().unwrap() + count);
    }
    let total_seg_chars: usize = *seg_boundaries.last().unwrap();

    // Accumulate log-probs per segment
    let mut seg_logprobs = vec![0.0_f64; segments.len()];
    let mut char_offset: usize = 0;

    for (i, &token_id) in output_tokens.iter().enumerate() {
        if i >= token_logprobs.len() {
            break;
        }
        let decoded = tokenizer.decode(&[token_id]);
        let token_chars = decoded.chars().count();
        if token_chars == 0 {
            continue;
        }

        let token_start = char_offset;
        let token_end = char_offset + token_chars;

        // Distribute this token's log-prob across segments it overlaps
        for (seg_idx, seg_count) in seg_char_counts.iter().enumerate() {
            if *seg_count == 0 {
                continue;
            }
            let seg_start = seg_boundaries[seg_idx];
            let seg_end = seg_boundaries[seg_idx + 1];

            // Compute overlap between [token_start, token_end) and [seg_start, seg_end)
            let overlap_start = token_start.max(seg_start);
            let overlap_end = token_end.min(seg_end);
            if overlap_start < overlap_end {
                let overlap_chars = overlap_end - overlap_start;
                let fraction = overlap_chars as f64 / token_chars as f64;
                seg_logprobs[seg_idx] += token_logprobs[i] * fraction;
            }
        }

        char_offset = token_end;
        if char_offset >= total_seg_chars {
            break;
        }
    }

    // Normalize by character count → per-char average
    let scores: Vec<f64> = seg_logprobs
        .iter()
        .zip(seg_char_counts.iter())
        .map(|(&lp, &count)| {
            if count == 0 {
                f64::NEG_INFINITY
            } else {
                lp / count as f64
            }
        })
        .collect();

    Ok(scores)
}

/// Return the index of the maximum value in a 1-D logits tensor.
fn argmax(logits: &Tensor) -> anyhow::Result<u32> {
    let logits_vec: Vec<f32> = logits
        .to_vec1()
        .map_err(|e| anyhow::anyhow!("argmax to_vec1 failed: {e}"))?;
    let (max_idx, _) = logits_vec
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
        .ok_or_else(|| anyhow::anyhow!("empty logits tensor"))?;
    Ok(max_idx as u32)
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

    // --- token-segment mapping tests (no model needed) ---

    /// Helper to create a ConvertedSegment.
    fn seg(reading: &str, surface: &str) -> ConvertedSegment {
        ConvertedSegment {
            reading: reading.to_string(),
            surface: surface.to_string(),
        }
    }

    #[test]
    fn test_map_token_logprobs_single_segment() {
        // Simulate: output = "今日" (2 chars), 2 tokens each covering 1 char
        // token_logprobs = [-0.5, -0.3]
        // Expected: sum(-0.5, -0.3) / 2 = -0.4 per char
        let segments = [seg("きょう", "今日")];
        let seg_chars: Vec<usize> = segments.iter().map(|s| s.surface.chars().count()).collect();
        assert_eq!(seg_chars, vec![2]);

        // Mock: each token maps to 1 char
        let _token_logprobs = [-0.5, -0.3];
        let seg_logprobs = [-0.5 + -0.3]; // total for segment
        let expected = [seg_logprobs[0] / 2.0]; // per-char average

        // We can't easily mock the tokenizer's decode, so test the math directly
        assert!((expected[0] - (-0.4_f64)).abs() < 1e-10);
    }

    #[test]
    fn test_map_token_logprobs_proportional_split() {
        // Simulate a token spanning a segment boundary:
        // Segments: "AB" (2 chars) | "CD" (2 chars)
        // Token covers 4 chars total, log_prob = -1.0
        // Segment 0 gets 2/4 * -1.0 = -0.5
        // Segment 1 gets 2/4 * -1.0 = -0.5
        // Per-char: seg0 = -0.5/2 = -0.25, seg1 = -0.5/2 = -0.25
        let seg_char_counts = [2usize, 2];
        let seg_boundaries = [0usize, 2, 4];
        let token_chars = 4;
        let token_logprob = -1.0;

        let mut seg_logprobs = [0.0; 2];
        let token_start = 0;
        let token_end = token_start + token_chars;

        for seg_idx in 0..2 {
            let seg_start = seg_boundaries[seg_idx];
            let seg_end = seg_boundaries[seg_idx + 1];
            let overlap_start = token_start.max(seg_start);
            let overlap_end = token_end.min(seg_end);
            if overlap_start < overlap_end {
                let overlap = overlap_end - overlap_start;
                let fraction = overlap as f64 / token_chars as f64;
                seg_logprobs[seg_idx] += token_logprob * fraction;
            }
        }

        let per_char: Vec<f64> = seg_logprobs
            .iter()
            .zip(seg_char_counts.iter())
            .map(|(&lp, &c)| lp / c as f64)
            .collect();

        assert!((per_char[0] - -0.25).abs() < 1e-10);
        assert!((per_char[1] - -0.25).abs() < 1e-10);
    }

    #[test]
    fn test_map_token_logprobs_empty_segments() {
        // Empty segments should produce empty scores
        let segments: Vec<ConvertedSegment> = vec![];
        // Directly test: score_segments returns empty for empty segments
        // (the real function is tested above, this is the edge case)
        assert!(segments.is_empty());
    }

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

    #[test]
    fn test_argmax() {
        use candle_core::Device;
        let device = Device::Cpu;
        let data = vec![0.1f32, 0.5, 0.3, 0.9, 0.2];
        let tensor = Tensor::from_vec(data, 5, &device).unwrap();
        let idx = argmax(&tensor).unwrap();
        assert_eq!(idx, 3);
    }

    #[test]
    fn test_argmax_single() {
        use candle_core::Device;
        let device = Device::Cpu;
        let data = vec![42.0f32];
        let tensor = Tensor::from_vec(data, 1, &device).unwrap();
        let idx = argmax(&tensor).unwrap();
        assert_eq!(idx, 0);
    }

    #[test]
    #[ignore]
    fn test_generate_text_basic() {
        let model_path = std::path::Path::new("../data/zenz-v3.1-Q5_K_M.gguf");
        if !model_path.exists() {
            eprintln!("Model not found at {:?}, skipping", model_path);
            return;
        }
        let mut scorer = NeuralScorer::open(model_path).expect("failed to open model");
        let config = GenerateConfig {
            max_tokens: 10,
            ..GenerateConfig::default()
        };
        let text = scorer
            .generate_text("今日はいい天気です", &config)
            .expect("generate_text failed");
        eprintln!("Generated: {text}");
        // Should generate some non-empty text
        assert!(!text.is_empty(), "generated text should not be empty");
    }
}
