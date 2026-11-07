//! Text → embedding → hypervector encoding pipeline.
//!
//! Provides the `TextEncoder` trait for pluggable embedding backends,
//! a `SimpleHashEncoder` for offline/testing use, and the `EncodingPipeline`
//! that chains text encoding with codebook projection and HDC algebra.

use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use thiserror::Error;

use crate::codebook::Codebook;
use crate::memory::HyperMemory;
use crate::wave::normalize;

/// Errors that can occur during encoding.
#[derive(Debug, Error)]
pub enum EncodingError {
    #[error("empty input text")]
    EmptyInput,
    #[error("dimension mismatch: expected {expected}, got {got}")]
    DimensionMismatch { expected: usize, got: usize },
    #[error("encoding failed: {0}")]
    Other(String),
}

/// Abstraction for text → dense embedding backends.
pub trait TextEncoder: Send + Sync {
    fn embed(&self, text: &str) -> Result<Vec<f32>, EncodingError>;
    fn embedding_dim(&self) -> usize;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HashVariant {
    Standard, // hash
    Cleaned,  // hash2
}

/// A fast, deterministic hash-based encoder for testing.
/// Tokenizes on whitespace, hashes each token to a vector, then bundles.
pub struct SimpleHashEncoder {
    dim: usize,
    seed: u64,
    variant: HashVariant,
}

impl SimpleHashEncoder {
    pub fn new(dim: usize, seed: u64) -> Self {
        Self {
            dim,
            seed,
            variant: HashVariant::Standard,
        }
    }

    pub fn new_cleaned(dim: usize, seed: u64) -> Self {
        Self {
            dim,
            seed,
            variant: HashVariant::Cleaned,
        }
    }

    /// Deterministic hash-based vector for a single token.
    ///
    /// One-line subtle bug fixed under #106: the shift used to be `>> 33`,
    /// which keeps only the high 31 bits of `h`. After `as f32 / u32::MAX`
    /// that's range [0, 0.5), then `* 2 - 1` → [-1, 0) — **every
    /// per-dimension value was always negative**, so every text projected
    /// into the same all-negative hyperoctant of 384-dim space. Pairwise
    /// cosine similarity across distinct texts measured 0.93 instead of
    /// ~0, which is why recall ranking was biased toward the first-stored
    /// memory regardless of query content (#106). The correct shift is
    /// `>> 32` — keep the high 32 bits and reinterpret as u32, giving
    /// [0, u32::MAX] → [0, 1] → [-1, 1].
    fn token_vector(&self, token: &str) -> Vec<f32> {
        let mut v = vec![0.0f32; self.dim];
        // Use a simple hash mixing scheme
        let mut h = self.seed;
        for byte in token.bytes() {
            h = h.wrapping_mul(6364136223846793005).wrapping_add(byte as u64);
        }
        for i in 0..self.dim {
            // Derive per-dimension value from hash
            h = h.wrapping_mul(6364136223846793005).wrapping_add(i as u64);
            // Map to [-1, 1] range. High 32 bits → u32 → [0, 1] → [-1, 1].
            v[i] = ((h >> 32) as u32 as f32 / (u32::MAX as f32)) * 2.0 - 1.0;
        }
        v
    }
}

impl TextEncoder for SimpleHashEncoder {
    fn embed(&self, text: &str) -> Result<Vec<f32>, EncodingError> {
        let tokens: Vec<String> = if self.variant == HashVariant::Cleaned {
            text.split_whitespace()
                .map(|t| {
                    t.to_lowercase()
                        .trim_matches(|c: char| c.is_ascii_punctuation())
                        .to_string()
                })
                .filter(|t| !t.is_empty())
                .collect()
        } else {
            text.split_whitespace().map(|t| t.to_string()).collect()
        };

        if tokens.is_empty() {
            return Err(EncodingError::EmptyInput);
        }
        // Bundle all token vectors
        let mut result = vec![0.0f32; self.dim];
        for token in &tokens {
            let tv = self.token_vector(token);
            for (i, val) in tv.iter().enumerate() {
                result[i] += val;
            }
        }
        normalize(&mut result);
        Ok(result)
    }

    fn embedding_dim(&self) -> usize {
        self.dim
    }
}

/// Default for how long one embedding request may take (#1153). A recall
/// embeds its query before it scans, and a request with no timeout could
/// hold a served ask (and the node) forever. `KANNAKA_EMBED_TIMEOUT_SECS`
/// overrides it; 0 or an unparsable value keeps the default.
pub const EMBED_TIMEOUT_DEFAULT_SECS: u64 = 90;

/// The embedding request timeout, from `KANNAKA_EMBED_TIMEOUT_SECS`.
pub fn embed_timeout() -> Duration {
    let secs = std::env::var("KANNAKA_EMBED_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|&s| s > 0)
        .unwrap_or(EMBED_TIMEOUT_DEFAULT_SECS);
    Duration::from_secs(secs)
}

/// Send one embedding request with `timeout`, marking progress around it
/// (#1153): a bounded mark before (the request may be silent for up to its
/// timeout, plus a little for the body), a plain one when it returns.
fn post_embed(
    req: ureq::Request,
    body: serde_json::Value,
    timeout: Duration,
    what: &str,
) -> Result<serde_json::Value, EncodingError> {
    crate::progress::mark_bounded("embed request", timeout.saturating_add(Duration::from_secs(5)));
    let json = req
        .timeout(timeout)
        .send_json(body)
        .map_err(|e| EncodingError::Other(format!("{what} request failed: {e}")))
        .and_then(|resp| {
            resp.into_json::<serde_json::Value>()
                .map_err(|e| EncodingError::Other(format!("failed to parse {what} response: {e}")))
        });
    crate::progress::mark("embed reply");
    json
}

/// Calls an OpenAI-compatible HTTP embedding API.
pub struct HttpEmbeddingEncoder {
    api_url: String,
    api_key: Option<String>,
    model: String,
    embedding_dim: usize,
    timeout: Duration,
}

impl HttpEmbeddingEncoder {
    /// Create a new HTTP embedding encoder.
    ///
    /// `api_url` should be the base URL (e.g. `https://api.openai.com`).
    /// The encoder will POST to `{api_url}/v1/embeddings`.
    pub fn new(api_url: String, api_key: Option<String>, model: String, embedding_dim: usize) -> Self {
        Self { api_url, api_key, model, embedding_dim, timeout: embed_timeout() }
    }

    /// Override the request timeout (default [`embed_timeout`]).
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Create an encoder configured for OpenAI's `text-embedding-3-small` (1536 dims).
    pub fn openai_small(api_key: String) -> Self {
        Self::new(
            "https://api.openai.com".to_string(),
            Some(api_key),
            "text-embedding-3-small".to_string(),
            1536,
        )
    }
}

impl TextEncoder for HttpEmbeddingEncoder {
    fn embed(&self, text: &str) -> Result<Vec<f32>, EncodingError> {
        if text.trim().is_empty() {
            return Err(EncodingError::EmptyInput);
        }

        let url = format!("{}/v1/embeddings", self.api_url.trim_end_matches('/'));
        let body = serde_json::json!({
            "input": text,
            "model": &self.model,
        });

        let mut req = ureq::post(&url).set("Content-Type", "application/json");
        if let Some(key) = &self.api_key {
            req = req.set("Authorization", &format!("Bearer {key}"));
        }

        let json = post_embed(req, body, self.timeout, "HTTP")?;

        let embedding = json["data"][0]["embedding"]
            .as_array()
            .ok_or_else(|| EncodingError::Other("missing embedding in response".to_string()))?
            .iter()
            .map(|v| v.as_f64().unwrap_or(0.0) as f32)
            .collect::<Vec<f32>>();

        if embedding.len() != self.embedding_dim {
            return Err(EncodingError::DimensionMismatch {
                expected: self.embedding_dim,
                got: embedding.len(),
            });
        }

        Ok(embedding)
    }

    fn embedding_dim(&self) -> usize {
        self.embedding_dim
    }
}

/// Ollama embedding encoder — calls the local Ollama `/api/embed` endpoint.
pub struct OllamaEncoder {
    base_url: String,
    model: String,
    embedding_dim: usize,
    timeout: Duration,
}

impl OllamaEncoder {
    pub fn new(base_url: String, model: String, embedding_dim: usize) -> Self {
        Self { base_url, model, embedding_dim, timeout: embed_timeout() }
    }

    /// Override the request timeout (default [`embed_timeout`]).
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Default: all-minilm on localhost, 384 dims.
    pub fn default_local() -> Self {
        Self::new(
            "http://localhost:11434".to_string(),
            "all-minilm".to_string(),
            384,
        )
    }
}

impl TextEncoder for OllamaEncoder {
    fn embed(&self, text: &str) -> Result<Vec<f32>, EncodingError> {
        if text.trim().is_empty() {
            return Err(EncodingError::EmptyInput);
        }

        let url = format!("{}/api/embed", self.base_url.trim_end_matches('/'));
        let body = serde_json::json!({
            "model": &self.model,
            "input": text,
        });

        let req = ureq::post(&url).set("Content-Type", "application/json");
        let json = post_embed(req, body, self.timeout, "Ollama")?;

        // Ollama returns { "embeddings": [[...]] }
        let embedding = json["embeddings"][0]
            .as_array()
            .ok_or_else(|| EncodingError::Other("No embedding in Ollama response".to_string()))?
            .iter()
            .map(|v| v.as_f64().unwrap_or(0.0) as f32)
            .collect::<Vec<f32>>();

        if embedding.len() != self.embedding_dim {
            return Err(EncodingError::DimensionMismatch {
                expected: self.embedding_dim,
                got: embedding.len(),
            });
        }

        Ok(embedding)
    }

    fn embedding_dim(&self) -> usize {
        self.embedding_dim
    }
}

/// Wrapper that caches embeddings to avoid redundant API calls.
pub struct CachedEncoder<E: TextEncoder> {
    inner: E,
    cache: std::sync::RwLock<HashMap<String, Vec<f32>>>,
}

impl<E: TextEncoder> CachedEncoder<E> {
    pub fn new(inner: E) -> Self {
        Self {
            inner,
            cache: std::sync::RwLock::new(HashMap::new()),
        }
    }
}

impl<E: TextEncoder> TextEncoder for CachedEncoder<E> {
    fn embed(&self, text: &str) -> Result<Vec<f32>, EncodingError> {
        let key = text.to_string();
        let read_guard = self.cache.read().unwrap_or_else(|e| e.into_inner());
        if let Some(cached) = read_guard.get(&key) {
            return Ok(cached.clone());
        }
        drop(read_guard);
        let result = self.inner.embed(text)?;
        self.cache.write().unwrap_or_else(|e| e.into_inner()).insert(key, result.clone());
        Ok(result)
    }

    fn embedding_dim(&self) -> usize {
        self.inner.embedding_dim()
    }
}

/// Fallback chain: tries primary encoder, falls back on error.
pub struct CompositeEncoder {
    primary: Box<dyn TextEncoder>,
    fallback: Box<dyn TextEncoder>,
}

impl CompositeEncoder {
    pub fn new(primary: Box<dyn TextEncoder>, fallback: Box<dyn TextEncoder>) -> Self {
        Self { primary, fallback }
    }
}

impl TextEncoder for CompositeEncoder {
    fn embed(&self, text: &str) -> Result<Vec<f32>, EncodingError> {
        match self.primary.embed(text) {
            Ok(v) => Ok(v),
            Err(_) => self.fallback.embed(text),
        }
    }

    fn embedding_dim(&self) -> usize {
        self.primary.embedding_dim()
    }
}

/// Full text → hypervector encoding pipeline with HDC algebra.
pub struct EncodingPipeline {
    encoder: Box<dyn TextEncoder>,
    codebook: Codebook,
    pub stamp: String,
    pub adopted: bool,
}

impl EncodingPipeline {
    pub fn new(encoder: Box<dyn TextEncoder>, codebook: Codebook) -> Self {
        assert_eq!(
            encoder.embedding_dim(),
            codebook.input_dim,
            "encoder dim must match codebook input dim"
        );
        Self {
            encoder,
            codebook,
            stamp: "unknown".to_string(),
            adopted: false,
        }
    }

    pub fn with_metadata(mut self, stamp: String, adopted: bool) -> Self {
        self.stamp = stamp;
        self.adopted = adopted;
        self
    }

    /// Access the codebook.
    pub fn codebook(&self) -> &Codebook {
        &self.codebook
    }

    /// Encode text to a unit-length hypervector (10K dims).
    pub fn encode_text(&self, text: &str) -> Result<Vec<f32>, EncodingError> {
        let embedding = self.encoder.embed(text)?;
        Ok(self.codebook.project(&embedding))
    }

    /// Full pipeline: encode text → create HyperMemory with default wave params.
    pub fn encode_memory(
        &self,
        text: &str,
        _timestamp: DateTime<Utc>,
    ) -> Result<HyperMemory, EncodingError> {
        let hv = self.encode_text(text)?;
        Ok(HyperMemory::new(hv, text.to_string()))
    }

    /// Binding operation ⊗: element-wise multiply.
    pub fn bind(&self, a: &[f32], b: &[f32]) -> Vec<f32> {
        a.iter().zip(b.iter()).map(|(x, y)| x * y).collect()
    }

    /// Bundling operation ⊕: element-wise sum + normalize.
    pub fn bundle(&self, vectors: &[Vec<f32>]) -> Vec<f32> {
        assert!(!vectors.is_empty());
        // Find dimension from first non-empty vector; skip empty vectors gracefully
        let dim = vectors.iter().find(|v| !v.is_empty()).map(|v| v.len()).unwrap_or(0);
        if dim == 0 {
            return Vec::new();
        }
        let mut result = vec![0.0f32; dim];
        for v in vectors {
            if v.len() == dim {
                for (i, val) in v.iter().enumerate() {
                    result[i] += val;
                }
            }
            // Skip vectors with wrong dimensionality (empty or mismatched)
        }
        normalize(&mut result);
        result
    }

    /// Permutation operation Π: circular shift of coordinates.
    pub fn permute(&self, v: &[f32], shifts: usize) -> Vec<f32> {
        let n = v.len();
        if n == 0 {
            return vec![];
        }
        let shifts = shifts % n;
        let mut result = vec![0.0f32; n];
        for i in 0..n {
            result[(i + shifts) % n] = v[i];
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wave::cosine_similarity;

    fn make_pipeline() -> EncodingPipeline {
        let encoder = SimpleHashEncoder::new(384, 42);
        let codebook = Codebook::new(384, 10_000, 42);
        EncodingPipeline::new(Box::new(encoder), codebook)
    }

    #[test]
    fn hash_encoder_consistent() {
        let enc = SimpleHashEncoder::new(128, 42);
        let v1 = enc.embed("hello world").unwrap();
        let v2 = enc.embed("hello world").unwrap();
        assert_eq!(v1, v2);
    }

    #[test]
    fn hash_encoder_different_texts_differ() {
        let enc = SimpleHashEncoder::new(128, 42);
        let v1 = enc.embed("hello world").unwrap();
        let v2 = enc.embed("goodbye moon").unwrap();
        let sim = cosine_similarity(&v1, &v2);
        assert!(sim < 0.9, "different texts should produce different vectors, sim={sim}");
    }

    #[test]
    fn hash_encoder_empty_input_errors() {
        let enc = SimpleHashEncoder::new(128, 42);
        assert!(enc.embed("").is_err());
        assert!(enc.embed("   ").is_err());
    }

    #[test]
    fn encode_text_produces_unit_10k_vector() {
        let pipeline = make_pipeline();
        let hv = pipeline.encode_text("the quick brown fox").unwrap();
        assert_eq!(hv.len(), 10_000);
        let norm: f32 = hv.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-4, "norm was {norm}");
    }

    #[test]
    fn bind_recovers_similarity() {
        let pipeline = make_pipeline();
        let a = pipeline.encode_text("concept alpha").unwrap();
        let b = pipeline.encode_text("concept beta").unwrap();
        let bound = pipeline.bind(&a, &b);
        // Unbind: bound ⊗ a should be similar to b
        let recovered = pipeline.bind(&bound, &a);
        let sim = cosine_similarity(&recovered, &b);
        // For random-ish unit vectors, binding then unbinding recovers positive similarity
        assert!(sim > 0.1, "expected positive similarity after unbind, got {sim}");
    }

    #[test]
    fn bundle_has_positive_similarity_to_components() {
        let pipeline = make_pipeline();
        let v1 = pipeline.encode_text("red").unwrap();
        let v2 = pipeline.encode_text("green").unwrap();
        let v3 = pipeline.encode_text("blue").unwrap();
        let bundled = pipeline.bundle(&[v1.clone(), v2.clone(), v3.clone()]);
        assert!(cosine_similarity(&bundled, &v1) > 0.0);
        assert!(cosine_similarity(&bundled, &v2) > 0.0);
        assert!(cosine_similarity(&bundled, &v3) > 0.0);
    }

    #[test]
    fn permute_dissimilar_to_original() {
        let pipeline = make_pipeline();
        let v = pipeline.encode_text("sequence test").unwrap();
        let pv = pipeline.permute(&v, 1);
        let sim = cosine_similarity(&v, &pv);
        assert!(sim < 0.5, "permuted vector should be dissimilar, sim={sim}");
    }

    // --- Mock encoder that counts calls ---
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    struct CountingEncoder {
        dim: usize,
        call_count: Arc<AtomicUsize>,
    }

    impl CountingEncoder {
        fn new(dim: usize) -> (Self, Arc<AtomicUsize>) {
            let count = Arc::new(AtomicUsize::new(0));
            (Self { dim, call_count: count.clone() }, count)
        }
    }

    impl TextEncoder for CountingEncoder {
        fn embed(&self, _text: &str) -> Result<Vec<f32>, EncodingError> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            Ok(vec![1.0; self.dim])
        }
        fn embedding_dim(&self) -> usize { self.dim }
    }

    struct FailingEncoder { dim: usize }
    impl TextEncoder for FailingEncoder {
        fn embed(&self, _text: &str) -> Result<Vec<f32>, EncodingError> {
            Err(EncodingError::Other("always fails".to_string()))
        }
        fn embedding_dim(&self) -> usize { self.dim }
    }

    #[test]
    fn cached_encoder_second_call_uses_cache() {
        let (enc, count) = CountingEncoder::new(64);
        let cached = CachedEncoder::new(enc);
        let _ = cached.embed("hello").unwrap();
        let _ = cached.embed("hello").unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn cached_encoder_different_texts_call_inner() {
        let (enc, count) = CountingEncoder::new(64);
        let cached = CachedEncoder::new(enc);
        let _ = cached.embed("hello").unwrap();
        let _ = cached.embed("world").unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn composite_uses_primary_when_available() {
        let primary = SimpleHashEncoder::new(64, 1);
        let fallback = SimpleHashEncoder::new(64, 2);
        let composite = CompositeEncoder::new(Box::new(primary), Box::new(fallback));
        // Should succeed via primary
        let v = composite.embed("test").unwrap();
        assert_eq!(v.len(), 64);
    }

    #[test]
    fn composite_falls_back_on_primary_error() {
        let primary = FailingEncoder { dim: 64 };
        let fallback = SimpleHashEncoder::new(64, 42);
        let composite = CompositeEncoder::new(Box::new(primary), Box::new(fallback));
        let v = composite.embed("test").unwrap();
        assert_eq!(v.len(), 64);
    }

    // #1153: a peer that accepts the connection and never answers must not
    // hold the caller past the timeout, and the call marks progress around
    // the request whatever its outcome. Loopback only.
    #[test]
    fn ollama_embed_times_out_and_marks_progress() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let p = crate::progress::Progress::new();
            let _scope = crate::progress::install(p.clone());
            let enc = OllamaEncoder::new(url, "m".into(), 384)
                .with_timeout(Duration::from_millis(300));
            let out = enc.embed("hello");
            let _ = tx.send((out.is_err(), p.marks(), p.phase(), p.allowance()));
        });
        let (failed, marks, phase, allowance) = rx
            .recv_timeout(Duration::from_secs(20))
            .expect("the embed request ignored its timeout");
        drop(listener);
        assert!(failed, "a silent peer must be an error");
        assert_eq!(marks, 2, "one bounded mark before the request, one after");
        assert_eq!(phase, "embed reply");
        assert_eq!(allowance, Duration::ZERO, "the request's allowance must not outlive it");
    }

    #[test]
    fn encoders_default_to_the_embed_timeout() {
        let o = OllamaEncoder::default_local();
        assert_eq!(o.timeout, embed_timeout());
        let h = HttpEmbeddingEncoder::openai_small("k".into());
        assert_eq!(h.with_timeout(Duration::from_secs(7)).timeout, Duration::from_secs(7));
        assert!(embed_timeout() > Duration::ZERO);
    }

    #[test]
    fn http_encoder_construction() {
        let enc = HttpEmbeddingEncoder::openai_small("test-key".to_string());
        assert_eq!(enc.embedding_dim(), 1536);
        assert_eq!(enc.model, "text-embedding-3-small");
    }

    #[test]
    fn encode_memory_produces_valid_hypermemory() {
        let pipeline = make_pipeline();
        let mem = pipeline.encode_memory("I met Alice at the park", Utc::now()).unwrap();
        assert_eq!(mem.vector.len(), 10_000);
        assert_eq!(mem.content, "I met Alice at the park");
        assert_eq!(mem.layer_depth, 0);
        assert!(mem.amplitude > 0.0);
        let norm: f32 = mem.vector.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-4);
    }

    #[test]
    fn test_hash2_tokenizer_normalization() {
        let dim = 384;
        let seed = 42;
        let h1 = SimpleHashEncoder::new(dim, seed);
        let h2 = SimpleHashEncoder::new_cleaned(dim, seed);

        // Standard hash is case-sensitive and punctuation-sensitive
        let v_word = h1.embed("Word").unwrap();
        let v_lower = h1.embed("word").unwrap();
        let v_punct = h1.embed("word.").unwrap();
        assert_ne!(v_word, v_lower);
        assert_ne!(v_lower, v_punct);

        // hash2 (Cleaned) is case-insensitive and ignores trailing punctuation
        let v2_word = h2.embed("Word").unwrap();
        let v2_lower = h2.embed("word").unwrap();
        let v2_punct = h2.embed("word.").unwrap();

        assert_eq!(v2_word, v2_lower, "Word and word should hash equal under hash2");
        assert_eq!(v2_lower, v2_punct, "word and word. should hash equal under hash2");

        // Leading punctuation and multiple punctuation
        let v2_multi = h2.embed("...Word...").unwrap();
        assert_eq!(v2_lower, v2_multi, "word and ...Word... should hash equal under hash2");
    }
}
