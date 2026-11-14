//! The native text embedder: all-MiniLM-L6-v2 run in-process.
//!
//! A hand-written BERT forward pass on `ndarray` plus the `tokenizers` crate's
//! WordPiece, built to return what Ollama's `all-minilm` returns for the same
//! text, so a store written through one can be read through the other. What
//! that takes was measured in `spikes/native-embed` (PR #1180):
//!
//! - mean pooling over every token, then L2 normalisation;
//! - a 256-token window: longer input keeps its first 255 tokens and is
//!   re-closed with `[SEP]`;
//! - ggml's tanh approximation of GELU, not erf;
//! - the stock tokenizer settings switched off: no padding, no truncation at
//!   128 tokens, no 100-character word limit (stock WordPiece turns a longer
//!   word into `[UNK]`; llama.cpp does not).
//!
//! Known gap: Ollama tokenizes with llama.cpp's own WordPiece, which keeps
//! Hangul syllables whole and keeps Devanagari vowel signs. This tokenizer does
//! neither, so vectors for Korean and Devanagari text differ from Ollama's
//! (measured cosine 0.659 and 0.964 on one sentence each). Latin, Cyrillic,
//! Greek, Arabic, Hebrew, CJK ideographs and kana matched in the spike.
//!
//! The vocabulary is compiled in. The weights (fp16 safetensors, 45 MB) are
//! fetched once, checked against a pinned sha256 and kept in
//! `<data_dir>/models/`. A failed download or a failed hash is an error; there
//! is no hash-encoder fallback anywhere in this module.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ndarray::{s, Array1, Array2, Axis};
use sha2::{Digest, Sha256};

use crate::encoding::{EncodingError, TextEncoder};

/// Where the weights are published. `v0` is a tag, so the bytes do not change.
pub const WEIGHTS_URL: &str =
    "https://huggingface.co/Kannaka/kannaka-encoder/resolve/v0/model.safetensors";
/// sha256 of the fp16 safetensors file at [`WEIGHTS_URL`].
pub const WEIGHTS_SHA256: &str = "ef4f66adbac55a55f9d5a2d865d84a17f42e2f202b49d0235c77d3cdd5b19ce9";
/// Size of that file in bytes.
pub const WEIGHTS_BYTES: u64 = 45_141_872;
/// File name under `<data_dir>/models/`.
pub const WEIGHTS_FILE: &str = "all-minilm-l6-v2-f16.safetensors";
/// sha256 of the compiled-in `vocab.txt` (the model's published vocabulary).
pub const VOCAB_SHA256: &str = "07eced375cec144d27c900241f3e339478dec958f92fddbc551f295c992038a3";

/// Embedding width.
pub const DIM: usize = 384;
/// Ollama's `num_ctx` for `all-minilm`: the window, `[CLS]` and `[SEP]` included.
pub const MAX_TOKENS: usize = 256;
/// all-MiniLM-L6-v2 has 12 attention heads of width 32.
const HEADS: usize = 12;
const CLS_ID: u32 = 101;
const SEP_ID: u32 = 102;

const VOCAB: &str = include_str!("native_embed/vocab.txt");

// ---------------------------------------------------------------------------
// Tokenizer
// ---------------------------------------------------------------------------

/// BERT WordPiece over the compiled-in vocabulary, with the three overrides
/// that make it agree with Ollama (see the module docs).
pub struct WordPieceTokenizer(tokenizers::Tokenizer);

impl WordPieceTokenizer {
    /// Build from the compiled-in vocabulary.
    pub fn embedded() -> Result<Self, String> {
        let mut vocab = serde_json::Map::new();
        for (id, token) in VOCAB.lines().enumerate() {
            vocab.insert(token.to_string(), serde_json::json!(id));
        }
        let special = |id: u32, content: &str| {
            serde_json::json!({
                "id": id, "special": true, "content": content, "single_word": false,
                "lstrip": false, "rstrip": false, "normalized": false
            })
        };
        // The model's own tokenizer.json, minus `truncation` (128) and
        // `padding` (fixed 128), and with the word-length limit lifted.
        let spec = serde_json::json!({
            "version": "1.0",
            "truncation": null,
            "padding": null,
            "added_tokens": [
                special(0, "[PAD]"), special(100, "[UNK]"), special(CLS_ID, "[CLS]"),
                special(SEP_ID, "[SEP]"), special(103, "[MASK]")
            ],
            "normalizer": {
                "type": "BertNormalizer", "clean_text": true, "handle_chinese_chars": true,
                "strip_accents": null, "lowercase": true
            },
            "pre_tokenizer": { "type": "BertPreTokenizer" },
            "post_processor": {
                "type": "TemplateProcessing",
                "single": [
                    { "SpecialToken": { "id": "[CLS]", "type_id": 0 } },
                    { "Sequence": { "id": "A", "type_id": 0 } },
                    { "SpecialToken": { "id": "[SEP]", "type_id": 0 } }
                ],
                "pair": [
                    { "SpecialToken": { "id": "[CLS]", "type_id": 0 } },
                    { "Sequence": { "id": "A", "type_id": 0 } },
                    { "SpecialToken": { "id": "[SEP]", "type_id": 0 } },
                    { "Sequence": { "id": "B", "type_id": 1 } },
                    { "SpecialToken": { "id": "[SEP]", "type_id": 1 } }
                ],
                "special_tokens": {
                    "[CLS]": { "id": "[CLS]", "ids": [CLS_ID], "tokens": ["[CLS]"] },
                    "[SEP]": { "id": "[SEP]", "ids": [SEP_ID], "tokens": ["[SEP]"] }
                }
            },
            "decoder": { "type": "WordPiece", "prefix": "##", "cleanup": true },
            "model": {
                "type": "WordPiece", "unk_token": "[UNK]", "continuing_subword_prefix": "##",
                "max_input_chars_per_word": 1_000_000_000u64,
                "vocab": vocab
            }
        });
        let bytes = serde_json::to_vec(&spec).map_err(|e| format!("tokenizer spec: {e}"))?;
        let tok = tokenizers::Tokenizer::from_bytes(bytes).map_err(|e| format!("tokenizer: {e}"))?;
        Ok(Self(tok))
    }

    /// Every token id for `text`, `[CLS]` … `[SEP]`, with no length limit.
    pub fn ids_unbounded(&self, text: &str) -> Result<Vec<u32>, String> {
        let enc = self.0.encode(text, true).map_err(|e| format!("tokenize: {e}"))?;
        Ok(enc.get_ids().to_vec())
    }

    /// Token ids as the model sees them: at most [`MAX_TOKENS`], and when the
    /// text is longer, its first 255 tokens re-closed with `[SEP]`.
    pub fn ids(&self, text: &str) -> Result<Vec<u32>, String> {
        let mut ids = self.ids_unbounded(text)?;
        if ids.len() > MAX_TOKENS {
            ids.truncate(MAX_TOKENS - 1);
            ids.push(SEP_ID);
        }
        Ok(ids)
    }
}

// ---------------------------------------------------------------------------
// Weights file: safetensors, read by hand (an 8-byte length, a JSON header,
// then raw little-endian tensors)
// ---------------------------------------------------------------------------

/// IEEE 754 half precision to single precision. Exact: every f16 is an f32.
fn f16_to_f32(h: u16) -> f32 {
    let sign = ((h >> 15) as u32) << 31;
    let exp = ((h >> 10) & 0x1f) as u32;
    let frac = (h & 0x3ff) as u32;
    match exp {
        0 => {
            // zero or subnormal: frac * 2^-24
            let v = frac as f32 * (1.0 / 16_777_216.0);
            if sign != 0 { -v } else { v }
        }
        31 => f32::from_bits(sign | 0x7f80_0000 | (frac << 13)),
        _ => f32::from_bits(sign | ((exp + 112) << 23) | (frac << 13)),
    }
}

struct SafeTensors<'a> {
    header: serde_json::Map<String, serde_json::Value>,
    data: &'a [u8],
}

impl<'a> SafeTensors<'a> {
    fn parse(bytes: &'a [u8]) -> Result<Self, String> {
        if bytes.len() < 8 {
            return Err("weights file is shorter than a safetensors header".into());
        }
        let n = u64::from_le_bytes(bytes[..8].try_into().unwrap());
        let end = usize::try_from(n).ok().and_then(|n| n.checked_add(8)).filter(|e| *e <= bytes.len());
        let end = end.ok_or_else(|| "weights file: header length runs past the end of the file".to_string())?;
        let header: serde_json::Value = serde_json::from_slice(&bytes[8..end])
            .map_err(|e| format!("weights file: header is not JSON ({e})"))?;
        let header = match header {
            serde_json::Value::Object(m) => m,
            _ => return Err("weights file: header is not a JSON object".into()),
        };
        Ok(Self { header, data: &bytes[end..] })
    }

    fn has(&self, name: &str) -> bool {
        self.header.contains_key(name)
    }

    /// `(shape, values as f32)` for an F16 or F32 tensor.
    fn tensor(&self, name: &str) -> Result<(Vec<usize>, Vec<f32>), String> {
        let t = self.header.get(name).ok_or_else(|| format!("weights file: missing tensor {name}"))?;
        let bad = || format!("weights file: malformed entry for {name}");
        let shape: Vec<usize> = t["shape"]
            .as_array()
            .ok_or_else(bad)?
            .iter()
            .map(|v| v.as_u64().map(|v| v as usize).ok_or_else(bad))
            .collect::<Result<_, _>>()?;
        let offs = t["data_offsets"].as_array().ok_or_else(bad)?;
        let (a, b) = match offs.as_slice() {
            [a, b] => (a.as_u64().ok_or_else(bad)? as usize, b.as_u64().ok_or_else(bad)? as usize),
            _ => return Err(bad()),
        };
        if a > b || b > self.data.len() {
            return Err(format!("weights file: {name} lies outside the file"));
        }
        let raw = &self.data[a..b];
        let count = shape.iter().try_fold(1usize, |acc, d| acc.checked_mul(*d)).ok_or_else(bad)?;
        let values: Vec<f32> = match t["dtype"].as_str() {
            Some("F16") if raw.len() == count * 2 => {
                raw.chunks_exact(2).map(|c| f16_to_f32(u16::from_le_bytes([c[0], c[1]]))).collect()
            }
            Some("F32") if raw.len() == count * 4 => {
                raw.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
            }
            Some(d @ ("F16" | "F32")) => {
                return Err(format!("weights file: {name} ({d}) has {} bytes for {count} values", raw.len()))
            }
            other => return Err(format!("weights file: {name} has unsupported dtype {other:?}")),
        };
        Ok((shape, values))
    }

    fn m2(&self, name: &str) -> Result<Array2<f32>, String> {
        let (shape, values) = self.tensor(name)?;
        match shape.as_slice() {
            [r, c] => Array2::from_shape_vec((*r, *c), values).map_err(|e| format!("weights file: {name}: {e}")),
            _ => Err(format!("weights file: {name} is not a matrix")),
        }
    }

    fn m1(&self, name: &str) -> Result<Array1<f32>, String> {
        let (shape, values) = self.tensor(name)?;
        if shape.len() != 1 {
            return Err(format!("weights file: {name} is not a vector"));
        }
        Ok(Array1::from_vec(values))
    }
}

// ---------------------------------------------------------------------------
// The forward pass
// ---------------------------------------------------------------------------

struct Lin {
    w: Array2<f32>, // stored transposed: [in, out]
    b: Array1<f32>,
}

impl Lin {
    fn load(st: &SafeTensors, prefix: &str) -> Result<Self, String> {
        let w = st.m2(&format!("{prefix}.weight"))?;
        let b = st.m1(&format!("{prefix}.bias"))?;
        if w.nrows() != b.len() {
            return Err(format!("weights file: {prefix} weight and bias disagree"));
        }
        Ok(Self { w: w.t().as_standard_layout().to_owned(), b })
    }

    fn f(&self, x: &Array2<f32>) -> Array2<f32> {
        x.dot(&self.w) + &self.b
    }
}

struct LayerNorm {
    g: Array1<f32>,
    b: Array1<f32>,
}

impl LayerNorm {
    fn load(st: &SafeTensors, prefix: &str) -> Result<Self, String> {
        Ok(Self { g: st.m1(&format!("{prefix}.weight"))?, b: st.m1(&format!("{prefix}.bias"))? })
    }

    fn f(&self, mut x: Array2<f32>) -> Array2<f32> {
        for mut r in x.rows_mut() {
            let m = r.mean().unwrap_or(0.0);
            r.mapv_inplace(|v| v - m);
            let var = r.iter().map(|v| v * v).sum::<f32>() / r.len() as f32;
            let inv = 1.0 / (var + 1e-12).sqrt();
            r.mapv_inplace(|v| v * inv);
        }
        x * &self.g + &self.b
    }
}

struct Layer {
    q: Lin,
    k: Lin,
    v: Lin,
    o: Lin,
    ln1: LayerNorm,
    i: Lin,
    out: Lin,
    ln2: LayerNorm,
}

/// A BERT encoder stack with mean pooling. Shapes come from the weights file.
pub struct BertModel {
    word: Array2<f32>,
    pos: Array2<f32>,
    typ0: Array1<f32>,
    ln: LayerNorm,
    layers: Vec<Layer>,
    heads: usize,
}

/// ggml's GELU, which is what llama.cpp (and so Ollama) computes.
fn gelu_tanh(v: f32) -> f32 {
    0.5 * v * (1.0 + (0.797_884_6_f32 * v * (1.0 + 0.044715 * v * v)).tanh())
}

impl BertModel {
    /// Parse a safetensors file (F16 or F32 tensors, BERT names without a
    /// `bert.` prefix). `heads` is not recorded in the file.
    pub fn from_safetensors(bytes: &[u8], heads: usize) -> Result<Self, String> {
        let st = SafeTensors::parse(bytes)?;
        let mut layers = Vec::new();
        loop {
            let p = format!("encoder.layer.{}", layers.len());
            if !st.has(&format!("{p}.attention.self.query.weight")) {
                break;
            }
            layers.push(Layer {
                q: Lin::load(&st, &format!("{p}.attention.self.query"))?,
                k: Lin::load(&st, &format!("{p}.attention.self.key"))?,
                v: Lin::load(&st, &format!("{p}.attention.self.value"))?,
                o: Lin::load(&st, &format!("{p}.attention.output.dense"))?,
                ln1: LayerNorm::load(&st, &format!("{p}.attention.output.LayerNorm"))?,
                i: Lin::load(&st, &format!("{p}.intermediate.dense"))?,
                out: Lin::load(&st, &format!("{p}.output.dense"))?,
                ln2: LayerNorm::load(&st, &format!("{p}.output.LayerNorm"))?,
            });
        }
        if layers.is_empty() {
            return Err("weights file: no encoder layers".into());
        }
        let word = st.m2("embeddings.word_embeddings.weight")?;
        let pos = st.m2("embeddings.position_embeddings.weight")?;
        let typ = st.m2("embeddings.token_type_embeddings.weight")?;
        let d = word.ncols();
        if heads == 0 || d == 0 || d % heads != 0 || pos.ncols() != d || typ.ncols() != d || typ.nrows() == 0 {
            return Err("weights file: embedding shapes do not fit the model".into());
        }
        Ok(Self {
            word,
            pos,
            typ0: typ.row(0).to_owned(),
            ln: LayerNorm::load(&st, "embeddings.LayerNorm")?,
            layers,
            heads,
        })
    }

    /// Output width.
    pub fn dim(&self) -> usize {
        self.word.ncols()
    }

    /// Longest token sequence the position table covers.
    pub fn max_positions(&self) -> usize {
        self.pos.nrows()
    }

    /// Last hidden state, one row per token.
    fn hidden(&self, ids: &[u32]) -> Result<Array2<f32>, String> {
        let n = ids.len();
        let d = self.dim();
        if n == 0 || n > self.pos.nrows() {
            return Err(format!("{n} tokens does not fit the model's {} positions", self.pos.nrows()));
        }
        let hd = d / self.heads;
        let mut x = Array2::<f32>::zeros((n, d));
        for (t, id) in ids.iter().enumerate() {
            let id = *id as usize;
            if id >= self.word.nrows() {
                return Err(format!("token id {id} is outside the vocabulary"));
            }
            let mut r = x.row_mut(t);
            r += &self.word.row(id);
            r += &self.pos.row(t);
            r += &self.typ0;
        }
        let mut x = self.ln.f(x);
        let scale = 1.0 / (hd as f32).sqrt();
        for l in &self.layers {
            let (q, k, v) = (l.q.f(&x), l.k.f(&x), l.v.f(&x));
            let mut ctx = Array2::<f32>::zeros((n, d));
            for h in 0..self.heads {
                let r = s![.., h * hd..(h + 1) * hd];
                let mut a = q.slice(r).dot(&k.slice(r).t()) * scale;
                for mut row in a.axis_iter_mut(Axis(0)) {
                    let m = row.fold(f32::NEG_INFINITY, |m, v| m.max(*v));
                    row.mapv_inplace(|v| (v - m).exp());
                    let sum = row.sum();
                    row.mapv_inplace(|v| v / sum);
                }
                ctx.slice_mut(r).assign(&a.dot(&v.slice(r)));
            }
            x = l.ln1.f(l.o.f(&ctx) + &x);
            let mut mid = l.i.f(&x);
            mid.mapv_inplace(gelu_tanh);
            x = l.ln2.f(l.out.f(&mid) + &x);
        }
        Ok(x)
    }

    /// Token ids to a unit-length sentence vector: mean over every token of
    /// the last hidden state, then L2 normalisation.
    pub fn embed_ids(&self, ids: &[u32]) -> Result<Vec<f32>, String> {
        let h = self.hidden(ids)?;
        let mut v = h.mean_axis(Axis(0)).ok_or_else(|| "no tokens".to_string())?.to_vec();
        let norm = v.iter().map(|x| (*x as f64) * (*x as f64)).sum::<f64>().sqrt() as f32;
        if !norm.is_finite() || norm == 0.0 {
            return Err("the model returned a zero or non-finite vector".into());
        }
        v.iter_mut().for_each(|x| *x /= norm);
        Ok(v)
    }
}

// ---------------------------------------------------------------------------
// Getting the weights: local file, else one verified download
// ---------------------------------------------------------------------------

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `(sha256 hex, length)` of a file, read in chunks.
pub fn sha256_file(path: &Path) -> Result<(String, u64), String> {
    let mut f = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let (mut h, mut len, mut buf) = (Sha256::new(), 0u64, vec![0u8; 1 << 16]);
    loop {
        let n = f.read(&mut buf).map_err(|e| format!("{}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        len += n as u64;
    }
    Ok((hex(&h.finalize()), len))
}

/// Does the file at `path` have exactly this sha256? The error names the path
/// and both digests; nothing is deleted.
pub fn verify_file(path: &Path, expected_sha256: &str) -> Result<(), String> {
    let (got, len) = sha256_file(path)?;
    if got != expected_sha256 {
        return Err(format!(
            "{} is not the pinned encoder weights: sha256 {got} ({len} bytes), expected {expected_sha256}",
            path.display()
        ));
    }
    Ok(())
}

/// Download `url` to `dest`, refusing anything whose sha256 is not
/// `expected_sha256`. The bytes go to a temporary sibling first and are renamed
/// into place only after the hash passes, so `dest` either does not exist or is
/// the verified file. Prints one line on stderr before it starts.
pub fn fetch_verified(url: &str, dest: &Path, expected_sha256: &str, expected_bytes: u64) -> Result<(), String> {
    let dir = dest.parent().ok_or_else(|| format!("{} has no parent directory", dest.display()))?;
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    eprintln!(
        "[encoder] downloading the text encoder weights (all-MiniLM-L6-v2 fp16, {:.0} MB, one time) from {url} to {}",
        expected_bytes as f64 / 1e6,
        dest.display()
    );
    let name = dest.file_name().and_then(|n| n.to_str()).unwrap_or("weights");
    let tmp = dir.join(format!(".{name}.{}.part", std::process::id()));
    let result = (|| -> Result<(), String> {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(20))
            .timeout_read(Duration::from_secs(60))
            .user_agent(concat!("kannaka-memory/", env!("CARGO_PKG_VERSION")))
            .build();
        let resp = agent.get(url).call().map_err(|e| format!("download of {url} failed: {e}"))?;
        let mut reader = resp.into_reader();
        let mut out = std::fs::File::create(&tmp).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
        let (mut h, mut len, mut buf) = (Sha256::new(), 0u64, vec![0u8; 1 << 16]);
        loop {
            let n = reader.read(&mut buf).map_err(|e| format!("download of {url} failed after {len} bytes: {e}"))?;
            if n == 0 {
                break;
            }
            len += n as u64;
            // A wrong file is refused by its hash below; this only stops a
            // server that never ends from filling the disk.
            if len > expected_bytes.saturating_mul(2).max(1 << 20) {
                return Err(format!("download of {url} is larger than the {expected_bytes} bytes expected"));
            }
            h.update(&buf[..n]);
            out.write_all(&buf[..n]).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
        }
        out.sync_all().map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
        drop(out);
        let got = hex(&h.finalize());
        if got != expected_sha256 {
            return Err(format!(
                "download of {url} failed its integrity check: sha256 {got} ({len} bytes), expected {expected_sha256} ({expected_bytes} bytes); nothing was installed"
            ));
        }
        std::fs::rename(&tmp, dest).map_err(|e| format!("cannot move the download to {}: {e}", dest.display()))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// `<data_dir>/models/`, where the weights are kept. Follows `KANNAKA_DATA_DIR`.
pub fn models_dir() -> PathBuf {
    crate::config::KannakaConfig::data_dir().join("models")
}

/// A file that passed its hash is remembered by `(length, mtime)` in a sidecar
/// next to it, so later process starts do not re-hash 45 MB.
fn marker_path(weights: &Path) -> PathBuf {
    let mut name = weights.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".verified");
    weights.with_file_name(name)
}

fn marker_value(weights: &Path) -> Option<String> {
    let md = std::fs::metadata(weights).ok()?;
    let mtime = md.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(format!("{WEIGHTS_SHA256} {} {}.{:09}", md.len(), mtime.as_secs(), mtime.subsec_nanos()))
}

/// Verify a weights file in a models directory, using and refreshing the marker.
fn verify_kept(weights: &Path, write_marker: bool) -> Result<(), String> {
    let marker = marker_path(weights);
    let current = marker_value(weights);
    if current.is_some() && std::fs::read_to_string(&marker).ok().map(|s| s.trim().to_string()) == current {
        return Ok(());
    }
    verify_file(weights, WEIGHTS_SHA256)?;
    if write_marker {
        if let Some(v) = current {
            let _ = std::fs::write(&marker, v);
        }
    }
    Ok(())
}

/// The path of a verified weights file, downloading it if no local copy exists.
///
/// Order: `KANNAKA_ENCODER_MODEL_PATH` (a local file; never downloads),
/// `<data_dir>/models/`, `~/.kannaka/models/` (read only, when the data dir is
/// somewhere else), then one download into `<data_dir>/models/` from
/// `KANNAKA_ENCODER_MODEL_URL` or [`WEIGHTS_URL`]. Every route ends in the same
/// sha256 check.
pub fn ensure_weights() -> Result<PathBuf, String> {
    if let Some(p) = std::env::var_os("KANNAKA_ENCODER_MODEL_PATH").filter(|v| !v.is_empty()) {
        let p = PathBuf::from(p);
        if !p.is_file() {
            return Err(format!("KANNAKA_ENCODER_MODEL_PATH={} is not a file", p.display()));
        }
        verify_file(&p, WEIGHTS_SHA256)?;
        return Ok(p);
    }
    let kept = models_dir().join(WEIGHTS_FILE);
    if kept.is_file() {
        verify_kept(&kept, true).map_err(|e| format!("{e}; delete it to download it again"))?;
        return Ok(kept);
    }
    if let Some(home) = dirs::home_dir() {
        let shared = home.join(".kannaka").join("models").join(WEIGHTS_FILE);
        if shared != kept && shared.is_file() && verify_kept(&shared, false).is_ok() {
            return Ok(shared);
        }
    }
    let url = std::env::var("KANNAKA_ENCODER_MODEL_URL")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| WEIGHTS_URL.to_string());
    fetch_verified(url.trim(), &kept, WEIGHTS_SHA256, WEIGHTS_BYTES).map_err(|e| {
        format!(
            "{e}. To work offline, fetch {WEIGHTS_URL} on another machine and set KANNAKA_ENCODER_MODEL_PATH to the file"
        )
    })?;
    if let Some(v) = marker_value(&kept) {
        let _ = std::fs::write(marker_path(&kept), v);
    }
    Ok(kept)
}

// ---------------------------------------------------------------------------
// The encoder
// ---------------------------------------------------------------------------

/// Tokenizer plus model: everything one embed needs.
pub struct NativeEngine {
    tokenizer: WordPieceTokenizer,
    model: BertModel,
}

impl NativeEngine {
    /// From safetensors bytes that the caller has already verified.
    pub fn from_weights(bytes: &[u8]) -> Result<Self, String> {
        let model = BertModel::from_safetensors(bytes, HEADS)?;
        if model.dim() != DIM || model.max_positions() < MAX_TOKENS {
            return Err(format!(
                "weights are {} wide with {} positions; the native encoder needs {DIM} and at least {MAX_TOKENS}",
                model.dim(),
                model.max_positions()
            ));
        }
        Ok(Self { tokenizer: WordPieceTokenizer::embedded()?, model })
    }

    /// From a weights file, after checking it against the pinned sha256.
    pub fn from_file(path: &Path) -> Result<Self, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let got = hex(&Sha256::digest(&bytes));
        if got != WEIGHTS_SHA256 {
            return Err(format!(
                "{} is not the pinned encoder weights: sha256 {got}, expected {WEIGHTS_SHA256}",
                path.display()
            ));
        }
        Self::from_weights(&bytes)
    }

    pub fn embed(&self, text: &str) -> Result<Vec<f32>, String> {
        self.model.embed_ids(&self.tokenizer.ids(text)?)
    }
}

/// The process-wide engine and the last failure to build it.
struct Shared {
    engine: Option<Arc<NativeEngine>>,
    failed: Option<(Instant, String)>,
}

static SHARED: Mutex<Shared> = Mutex::new(Shared { engine: None, failed: None });

/// How long a failed load is remembered before the next embed tries again, so
/// a long-running process recovers when the network does without paying a
/// connect timeout on every embed in between.
const RETRY_AFTER: Duration = Duration::from_secs(30);

fn shared_engine() -> Result<Arc<NativeEngine>, String> {
    let mut g = SHARED.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(e) = &g.engine {
        return Ok(e.clone());
    }
    if let Some((at, err)) = &g.failed {
        if at.elapsed() < RETRY_AFTER {
            return Err(err.clone());
        }
    }
    let built = ensure_weights().and_then(|path| {
        let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        NativeEngine::from_weights(&bytes).map_err(|e| format!("{}: {e}", path.display()))
    });
    match built {
        Ok(engine) => {
            let engine = Arc::new(engine);
            g.engine = Some(engine.clone());
            g.failed = None;
            Ok(engine)
        }
        Err(e) => {
            g.failed = Some((Instant::now(), e.clone()));
            Err(e)
        }
    }
}

/// [`TextEncoder`] over the native engine.
///
/// [`NativeEncoder::shared`] costs nothing to construct: the weights are located
/// (and on first use downloaded), verified and parsed on the first `embed`, once
/// per process, however many encoders exist. A failure there is the embed's
/// error. There is no fallback encoder.
pub struct NativeEncoder {
    own: Option<Arc<NativeEngine>>,
}

impl NativeEncoder {
    /// The process-wide encoder, loaded on first use.
    pub fn shared() -> Self {
        Self { own: None }
    }

    /// An encoder over an engine the caller built (tests, tools).
    pub fn with_engine(engine: NativeEngine) -> Self {
        Self { own: Some(Arc::new(engine)) }
    }
}

impl TextEncoder for NativeEncoder {
    fn embed(&self, text: &str) -> Result<Vec<f32>, EncodingError> {
        if text.trim().is_empty() {
            return Err(EncodingError::EmptyInput);
        }
        let engine = match &self.own {
            Some(e) => e.clone(),
            None => shared_engine().map_err(|e| EncodingError::Other(format!("native encoder unavailable: {e}")))?,
        };
        engine.embed(text).map_err(|e| EncodingError::Other(format!("native encoder: {e}")))
    }

    fn embedding_dim(&self) -> usize {
        DIM
    }
}

#[cfg(test)]
mod tests;
