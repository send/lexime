//! Neural scoring for IME conversion using Zenzai GPT-2.
//!
//! This module provides neural language model scoring for re-ranking
//! Viterbi N-best candidates. It loads a GGUF quantized GPT-2 model
//! and computes log-probabilities for candidate strings.

mod gpt2;
mod scoring;
pub mod speculative;
#[cfg(test)]
mod tests;
mod tokenizer;

use std::path::Path;

use candle_core::{DType, Device, Tensor};

pub use gpt2::QuantizedGpt2;
pub use scoring::build_prompt;
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
