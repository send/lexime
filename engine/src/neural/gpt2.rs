//! Quantized GPT-2 model loaded from GGUF format.
//!
//! Architecture: GPT-2 with LayerNorm, learned position embeddings,
//! weight tying (wte == lm_head^T), and gelu_new activation.
//!
//! GGUF tensor names follow the llama.cpp convention for GPT-2.

use std::path::Path;

use candle_core::quantized::gguf_file;
use candle_core::quantized::QMatMul;
use candle_core::{Device, IndexOp, Module, Result, Tensor};
use candle_nn::{Embedding, LayerNorm};

// ---- Configuration ----

struct Gpt2Config {
    n_embd: usize,
    n_head: usize,
    n_layer: usize,
    n_positions: usize,
    vocab_size: usize,
}

impl Gpt2Config {
    fn from_gguf(content: &gguf_file::Content) -> Self {
        let get_u32 = |key: &str, default: u32| -> usize {
            content
                .metadata
                .get(key)
                .and_then(|v| v.to_u32().ok())
                .unwrap_or(default) as usize
        };
        Self {
            n_embd: get_u32("gpt2.embedding_length", 768),
            n_head: get_u32("gpt2.attention.head_count", 12),
            n_layer: get_u32("gpt2.block_count", 12),
            n_positions: get_u32("gpt2.context_length", 1024),
            vocab_size: get_u32("gpt2.vocab_size", 6000),
        }
    }

    fn head_dim(&self) -> usize {
        self.n_embd / self.n_head
    }
}

// ---- Attention ----

struct Attention {
    qkv: QMatMul,
    qkv_bias: Tensor,
    out_proj: QMatMul,
    out_bias: Tensor,
    n_head: usize,
    head_dim: usize,
    kv_cache: Option<(Tensor, Tensor)>,
}

impl Attention {
    fn forward(&mut self, x: &Tensor, pos: usize) -> Result<Tensor> {
        let (batch, seq_len, n_embd) = x.dims3()?;
        let head_dim = self.head_dim;
        let n_head = self.n_head;

        // QKV projection: [batch, seq, 3*n_embd]
        let qkv = self.qkv.forward(x)?.broadcast_add(&self.qkv_bias)?;
        let qkv = qkv.reshape((batch, seq_len, 3, n_head, head_dim))?;

        // Split Q, K, V: each [batch, n_head, seq, head_dim]
        let q = qkv.i((.., .., 0))?.transpose(1, 2)?.contiguous()?;
        let k = qkv.i((.., .., 1))?.transpose(1, 2)?.contiguous()?;
        let v = qkv.i((.., .., 2))?.transpose(1, 2)?.contiguous()?;

        // KV-cache: concatenate along sequence dimension
        let (k, v) = if let Some((prev_k, prev_v)) = &self.kv_cache {
            let k = Tensor::cat(&[prev_k, &k], 2)?;
            let v = Tensor::cat(&[prev_v, &v], 2)?;
            (k, v)
        } else {
            (k, v)
        };
        self.kv_cache = Some((k.clone(), v.clone()));

        let total_len = pos + seq_len;

        // Scaled dot-product attention
        let scale = (head_dim as f64).sqrt();
        let attn_weights = q.matmul(&k.transpose(2, 3)?)? / scale;

        // Causal mask: only attend to positions <= current
        let mask = create_causal_mask(seq_len, total_len, x.device())?;
        let attn_weights = attn_weights?.broadcast_add(&mask)?;

        let attn_weights = candle_nn::ops::softmax_last_dim(&attn_weights)?;
        let attn_out = attn_weights.matmul(&v)?;

        // Reshape back: [batch, seq, n_embd]
        let attn_out = attn_out
            .transpose(1, 2)?
            .reshape((batch, seq_len, n_embd))?;

        // Output projection
        let out = self
            .out_proj
            .forward(&attn_out)?
            .broadcast_add(&self.out_bias)?;
        Ok(out)
    }
}

fn create_causal_mask(seq_len: usize, total_len: usize, device: &Device) -> Result<Tensor> {
    let offset = total_len - seq_len;
    // For each query position i (0..seq_len), mask out key positions j where j > i + offset
    let mask: Vec<f32> = (0..seq_len)
        .flat_map(|i| {
            (0..total_len).map(move |j| {
                if j <= i + offset {
                    0.0f32
                } else {
                    f32::NEG_INFINITY
                }
            })
        })
        .collect();
    Tensor::from_vec(mask, (1, 1, seq_len, total_len), device)
}

// ---- MLP ----

struct Mlp {
    fc: QMatMul,
    fc_bias: Tensor,
    proj: QMatMul,
    proj_bias: Tensor,
}

impl Mlp {
    fn forward(&self, x: &Tensor) -> Result<Tensor> {
        let h = self.fc.forward(x)?.broadcast_add(&self.fc_bias)?;
        let h = gelu_new(&h)?;
        self.proj.forward(&h)?.broadcast_add(&self.proj_bias)
    }
}

/// GPT-2's gelu_new activation: 0.5 * x * (1 + tanh(sqrt(2/pi) * (x + 0.044715 * x^3)))
fn gelu_new(x: &Tensor) -> Result<Tensor> {
    let x3 = x.powf(3.0)?;
    let inner = ((x + (x3 * 0.044715)?)? * (2.0f64 / std::f64::consts::PI).sqrt())?;
    let tanh_inner = inner.tanh()?;
    (x * (tanh_inner + 1.0)?)? * 0.5
}

// ---- Transformer Block ----

struct Block {
    ln_1: LayerNorm,
    attn: Attention,
    ln_2: LayerNorm,
    mlp: Mlp,
}

impl Block {
    fn forward(&mut self, x: &Tensor, pos: usize) -> Result<Tensor> {
        // Pre-norm attention
        let residual = x;
        let h = self.ln_1.forward(x)?;
        let h = self.attn.forward(&h, pos)?;
        let x = (residual + h)?;

        // Pre-norm MLP
        let residual = &x;
        let h = self.ln_2.forward(&x)?;
        let h = self.mlp.forward(&h)?;
        residual + h
    }
}

// ---- Full GPT-2 Model ----

pub struct QuantizedGpt2 {
    wte: Embedding,
    wpe: Embedding,
    blocks: Vec<Block>,
    ln_f: LayerNorm,
    lm_head: Option<QMatMul>,
    config: Gpt2Config,
}

/// Load and dequantize a tensor from GGUF.
fn load_tensor(
    content: &gguf_file::Content,
    file: &mut std::fs::File,
    name: &str,
    device: &Device,
) -> anyhow::Result<Tensor> {
    let qt = content
        .tensor(file, name, device)
        .map_err(|e| anyhow::anyhow!("failed to load tensor {name}: {e}"))?;
    qt.dequantize(device)
        .map_err(|e| anyhow::anyhow!("failed to dequantize {name}: {e}"))
}

/// Load a quantized tensor as QMatMul from GGUF.
fn load_qmatmul(
    content: &gguf_file::Content,
    file: &mut std::fs::File,
    name: &str,
    device: &Device,
) -> anyhow::Result<QMatMul> {
    let qt = content
        .tensor(file, name, device)
        .map_err(|e| anyhow::anyhow!("failed to load tensor {name}: {e}"))?;
    QMatMul::from_qtensor(qt)
        .map_err(|e| anyhow::anyhow!("failed to create QMatMul for {name}: {e}"))
}

impl QuantizedGpt2 {
    /// Load a quantized GPT-2 model from a GGUF file.
    pub fn from_gguf(path: &Path, device: &Device) -> anyhow::Result<Self> {
        let mut file = std::fs::File::open(path)?;
        let content = gguf_file::Content::read(&mut file)
            .map_err(|e| anyhow::anyhow!("failed to read GGUF: {e}"))?;

        let config = Gpt2Config::from_gguf(&content);

        // Embeddings (typically F16/F32 in GGUF, not quantized)
        let wte_weight = load_tensor(&content, &mut file, "token_embd.weight", device)?;
        let wte = Embedding::new(wte_weight, config.n_embd);

        let wpe_weight = load_tensor(&content, &mut file, "position_embd.weight", device)?;
        let wpe = Embedding::new(wpe_weight, config.n_embd);

        // Transformer blocks
        let mut blocks = Vec::with_capacity(config.n_layer);
        for i in 0..config.n_layer {
            let ln_1 = LayerNorm::new(
                load_tensor(
                    &content,
                    &mut file,
                    &format!("blk.{i}.attn_norm.weight"),
                    device,
                )?,
                load_tensor(
                    &content,
                    &mut file,
                    &format!("blk.{i}.attn_norm.bias"),
                    device,
                )?,
                1e-5,
            );

            let attn = Attention {
                qkv: load_qmatmul(
                    &content,
                    &mut file,
                    &format!("blk.{i}.attn_qkv.weight"),
                    device,
                )?,
                qkv_bias: load_tensor(
                    &content,
                    &mut file,
                    &format!("blk.{i}.attn_qkv.bias"),
                    device,
                )?,
                out_proj: load_qmatmul(
                    &content,
                    &mut file,
                    &format!("blk.{i}.attn_output.weight"),
                    device,
                )?,
                out_bias: load_tensor(
                    &content,
                    &mut file,
                    &format!("blk.{i}.attn_output.bias"),
                    device,
                )?,
                n_head: config.n_head,
                head_dim: config.head_dim(),
                kv_cache: None,
            };

            let ln_2 = LayerNorm::new(
                load_tensor(
                    &content,
                    &mut file,
                    &format!("blk.{i}.ffn_norm.weight"),
                    device,
                )?,
                load_tensor(
                    &content,
                    &mut file,
                    &format!("blk.{i}.ffn_norm.bias"),
                    device,
                )?,
                1e-5,
            );

            let mlp = Mlp {
                fc: load_qmatmul(
                    &content,
                    &mut file,
                    &format!("blk.{i}.ffn_up.weight"),
                    device,
                )?,
                fc_bias: load_tensor(&content, &mut file, &format!("blk.{i}.ffn_up.bias"), device)?,
                proj: load_qmatmul(
                    &content,
                    &mut file,
                    &format!("blk.{i}.ffn_down.weight"),
                    device,
                )?,
                proj_bias: load_tensor(
                    &content,
                    &mut file,
                    &format!("blk.{i}.ffn_down.bias"),
                    device,
                )?,
            };

            blocks.push(Block {
                ln_1,
                attn,
                ln_2,
                mlp,
            });
        }

        // Final layer norm
        let ln_f = LayerNorm::new(
            load_tensor(&content, &mut file, "output_norm.weight", device)?,
            load_tensor(&content, &mut file, "output_norm.bias", device)?,
            1e-5,
        );

        // lm_head: may be absent if weight tying (use wte instead)
        let lm_head = if content.tensor_infos.contains_key("output.weight") {
            Some(load_qmatmul(&content, &mut file, "output.weight", device)?)
        } else {
            None
        };

        Ok(Self {
            wte,
            wpe,
            blocks,
            ln_f,
            lm_head,
            config,
        })
    }

    /// Run forward pass and return logits for the last token.
    ///
    /// `tokens`: input token IDs
    /// `pos`: position offset (for KV-cache continuation)
    ///
    /// Returns logits tensor of shape `[vocab_size]`.
    pub fn forward(&mut self, tokens: &[u32], pos: usize) -> Result<Tensor> {
        let device = self.wte.embeddings().device().clone();
        let seq_len = tokens.len();

        // Token IDs → embedding
        let token_ids = Tensor::new(tokens, &device)?;
        let token_embd = self.wte.forward(&token_ids)?;

        // Position IDs → embedding
        let positions: Vec<u32> = (pos as u32..(pos + seq_len) as u32).collect();
        let pos_ids = Tensor::new(positions.as_slice(), &device)?;
        let pos_embd = self.wpe.forward(&pos_ids)?;

        // Initial hidden state
        let mut h = (token_embd + pos_embd)?;

        // Add batch dimension: [1, seq_len, n_embd]
        h = h.unsqueeze(0)?;

        // Transformer blocks
        for block in &mut self.blocks {
            h = block.forward(&h, pos)?;
        }

        // Final layer norm
        h = self.ln_f.forward(&h)?;

        // Take last token: [1, n_embd]
        let last = h.i((.., seq_len - 1, ..))?;

        // Project to vocab: [1, vocab_size]
        let logits = if let Some(ref lm_head) = self.lm_head {
            lm_head.forward(&last)?
        } else {
            // Weight tying: logits = last @ wte.T
            let wte_weight = self.wte.embeddings();
            last.matmul(&wte_weight.t()?)?
        };

        // Remove batch dim → [vocab_size]
        logits.squeeze(0)
    }

    /// Reset the KV cache (call between independent sequences).
    pub fn reset_kv_cache(&mut self) {
        for block in &mut self.blocks {
            block.attn.kv_cache = None;
        }
    }

    /// Save a snapshot of the current KV cache state.
    ///
    /// `Tensor::clone()` is O(1) (Arc-based reference counting),
    /// so this is cheap even for large caches.
    pub fn save_kv_cache(&self) -> Vec<Option<(Tensor, Tensor)>> {
        self.blocks
            .iter()
            .map(|b| b.attn.kv_cache.clone())
            .collect()
    }

    /// Restore KV cache from a previously saved snapshot.
    pub fn restore_kv_cache(&mut self, snapshot: &[Option<(Tensor, Tensor)>]) {
        for (block, cache) in self.blocks.iter_mut().zip(snapshot.iter()) {
            block.attn.kv_cache = cache.clone();
        }
    }

    /// Get model configuration summary.
    pub fn config_summary(&self) -> String {
        format!(
            "GPT-2: {}L/{}H/{}E, vocab={}, ctx={}",
            self.config.n_layer,
            self.config.n_head,
            self.config.n_embd,
            self.config.vocab_size,
            self.config.n_positions,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gelu_new_zero() {
        let device = Device::Cpu;
        let x = Tensor::new(&[0.0f32], &device).unwrap();
        let y = gelu_new(&x).unwrap();
        let val: Vec<f32> = y.to_vec1().unwrap();
        assert!((val[0]).abs() < 1e-6, "gelu_new(0) should be ~0");
    }

    #[test]
    fn test_gelu_new_positive() {
        let device = Device::Cpu;
        let x = Tensor::new(&[1.0f32], &device).unwrap();
        let y = gelu_new(&x).unwrap();
        let val: Vec<f32> = y.to_vec1().unwrap();
        // gelu_new(1.0) ≈ 0.8412
        assert!(
            (val[0] - 0.8412).abs() < 0.01,
            "gelu_new(1.0) ≈ 0.8412, got {}",
            val[0]
        );
    }

    #[test]
    fn test_causal_mask() {
        let mask = create_causal_mask(3, 3, &Device::Cpu).unwrap();
        let vals: Vec<f32> = mask.flatten_all().unwrap().to_vec1().unwrap();
        // Row 0: [0, -inf, -inf]
        // Row 1: [0, 0, -inf]
        // Row 2: [0, 0, 0]
        assert_eq!(vals[0], 0.0); // (0,0)
        assert!(vals[1].is_infinite()); // (0,1)
        assert!(vals[2].is_infinite()); // (0,2)
        assert_eq!(vals[3], 0.0); // (1,0)
        assert_eq!(vals[4], 0.0); // (1,1)
        assert!(vals[5].is_infinite()); // (1,2)
        assert_eq!(vals[6], 0.0); // (2,0)
        assert_eq!(vals[7], 0.0); // (2,1)
        assert_eq!(vals[8], 0.0); // (2,2)
    }

    #[test]
    fn test_causal_mask_with_offset() {
        // Simulating KV-cache scenario: seq_len=1, total_len=4 (3 cached + 1 new)
        let mask = create_causal_mask(1, 4, &Device::Cpu).unwrap();
        let vals: Vec<f32> = mask.flatten_all().unwrap().to_vec1().unwrap();
        // Single query at position 3 can attend to all 4 positions
        assert_eq!(vals, vec![0.0, 0.0, 0.0, 0.0]);
    }
}
