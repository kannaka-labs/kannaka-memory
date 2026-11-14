//! Tests for the native embedder.
//!
//! Everything here runs without the weights and without the network, except
//! the two `#[ignore]`d tests at the bottom, which need the 45 MB weights file:
//!
//! ```text
//! KANNAKA_TEST_ENCODER_WEIGHTS=/path/to/model.safetensors \
//!   cargo test --lib native_embed -- --ignored
//! ```

use super::*;
use std::io::{Read, Write};

fn cosine(a: &[f32], b: &[f32]) -> f64 {
    let (mut d, mut na, mut nb) = (0f64, 0f64, 0f64);
    for (x, y) in a.iter().zip(b) {
        d += *x as f64 * *y as f64;
        na += *x as f64 * *x as f64;
        nb += *y as f64 * *y as f64;
    }
    d / (na.sqrt() * nb.sqrt())
}

/// `(text, Ollama's token count, Ollama's vector)` for four texts: a short
/// sentence, accented Latin, a 138-character word, and a text past the window.
fn reference() -> Vec<(String, usize, Vec<f32>)> {
    let v: serde_json::Value = serde_json::from_str(include_str!("ollama_reference.json")).unwrap();
    v["cases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["text"].as_str().unwrap().to_string(),
                c["tokens"].as_u64().unwrap() as usize,
                c["vector"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap() as f32).collect(),
            )
        })
        .collect()
}

// ── vocabulary and tokenizer (no weights) ────────────────────────────────

#[test]
fn compiled_in_vocab_is_the_pinned_file() {
    assert_eq!(hex(&Sha256::digest(VOCAB.as_bytes())), VOCAB_SHA256);
    assert_eq!(VOCAB.lines().count(), 30522);
}

#[test]
fn tokenizer_wraps_in_cls_sep_and_does_not_pad() {
    let tok = WordPieceTokenizer::embedded().unwrap();
    let ids = tok.ids("The cat sat on the windowsill.").unwrap();
    assert_eq!(ids.first(), Some(&CLS_ID));
    assert_eq!(ids.last(), Some(&SEP_ID));
    assert!(ids.len() < 16, "a short text is not padded to 128: {} ids", ids.len());
    assert!(!ids.contains(&0), "no [PAD]");
    // lowercasing and accent stripping, as BERT uncased does
    assert_eq!(tok.ids("CAFÉ").unwrap(), tok.ids("cafe").unwrap());
}

#[test]
fn tokenizer_counts_match_ollama_on_the_reference_texts() {
    let tok = WordPieceTokenizer::embedded().unwrap();
    for (text, tokens, _) in reference() {
        assert_eq!(tok.ids(&text).unwrap().len(), tokens, "token count for {:?}", &text[..text.len().min(40)]);
    }
}

#[test]
fn text_over_128_tokens_is_not_truncated_at_128() {
    let tok = WordPieceTokenizer::embedded().unwrap();
    // 200 words: the stock tokenizer.json would cut this to 128 ids.
    let text = "cat ".repeat(200);
    let ids = tok.ids(&text).unwrap();
    assert_eq!(ids.len(), 202);
    assert_eq!(ids, tok.ids_unbounded(&text).unwrap());
}

#[test]
fn text_over_the_window_keeps_255_tokens_and_is_reclosed_with_sep() {
    let tok = WordPieceTokenizer::embedded().unwrap();
    let text = "cat dog ".repeat(300);
    let all = tok.ids_unbounded(&text).unwrap();
    assert_eq!(all.len(), 602);
    let ids = tok.ids(&text).unwrap();
    assert_eq!(ids.len(), MAX_TOKENS);
    assert_eq!(ids[..MAX_TOKENS - 1], all[..MAX_TOKENS - 1]);
    assert_eq!(ids[MAX_TOKENS - 1], SEP_ID);
    // exactly at the window: untouched
    let fits = "cat ".repeat(MAX_TOKENS - 2);
    assert_eq!(tok.ids(&fits).unwrap().len(), MAX_TOKENS);
    assert_eq!(tok.ids(&fits).unwrap(), tok.ids_unbounded(&fits).unwrap());
}

#[test]
fn a_word_over_100_characters_is_split_not_unknown() {
    let tok = WordPieceTokenizer::embedded().unwrap();
    // Stock WordPiece (max_input_chars_per_word = 100) returns [CLS] [UNK] [SEP].
    let word = "a".repeat(138);
    let ids = tok.ids(&word).unwrap();
    assert!(!ids.contains(&100), "no [UNK] in {ids:?}");
    assert!(ids.len() > 10, "split into word pieces, got {} ids", ids.len());
    // A word the vocabulary cannot spell at all is still [UNK], as before.
    assert!(tok.ids("\u{1F355}").unwrap().contains(&100));
}

// ── weights format and forward pass on a tiny synthetic model (no weights) ─

#[test]
fn half_precision_decodes_exactly() {
    for (bits, want) in [
        (0x0000u16, 0.0f32),
        (0x3c00, 1.0),
        (0xc000, -2.0),
        (0x3555, 0.333_251_95),
        (0x7bff, 65504.0),
        (0x0001, 5.960_464_5e-8),
        (0x03ff, 6.097_555e-5),
        (0x0400, 6.103_515_6e-5),
    ] {
        assert_eq!(f16_to_f32(bits), want, "{bits:#06x}");
    }
    assert!(f16_to_f32(0x8000).is_sign_negative() && f16_to_f32(0x8000) == 0.0);
    assert_eq!(f16_to_f32(0x7c00), f32::INFINITY);
    assert_eq!(f16_to_f32(0xfc00), f32::NEG_INFINITY);
    assert!(f16_to_f32(0x7e00).is_nan());
}

/// f32 to f16 for values that are exactly representable (the tiny model's).
fn f32_to_f16_exact(v: f32) -> u16 {
    let bits = v.to_bits();
    let sign = ((bits >> 31) as u16) << 15;
    if v == 0.0 {
        return sign;
    }
    let exp = ((bits >> 23) & 0xff) as i32 - 127 + 15;
    assert!((1..31).contains(&exp) && bits & 0x1fff == 0, "{v} is not exact in f16");
    sign | ((exp as u16) << 10) | ((bits >> 13) & 0x3ff) as u16
}

/// A two-layer, two-head, 8-wide BERT with deterministic weights that are
/// exact in half precision, serialised as safetensors.
fn tiny_model(f16: bool) -> Vec<u8> {
    let (d, inter, vocab, positions) = (8usize, 16usize, 16usize, 12usize);
    let mut shapes: Vec<(String, Vec<usize>)> = vec![
        ("embeddings.word_embeddings.weight".into(), vec![vocab, d]),
        ("embeddings.position_embeddings.weight".into(), vec![positions, d]),
        ("embeddings.token_type_embeddings.weight".into(), vec![2, d]),
        ("embeddings.LayerNorm.weight".into(), vec![d]),
        ("embeddings.LayerNorm.bias".into(), vec![d]),
    ];
    for l in 0..2 {
        let p = format!("encoder.layer.{l}");
        for name in ["attention.self.query", "attention.self.key", "attention.self.value", "attention.output.dense"] {
            shapes.push((format!("{p}.{name}.weight"), vec![d, d]));
            shapes.push((format!("{p}.{name}.bias"), vec![d]));
        }
        shapes.push((format!("{p}.attention.output.LayerNorm.weight"), vec![d]));
        shapes.push((format!("{p}.attention.output.LayerNorm.bias"), vec![d]));
        shapes.push((format!("{p}.intermediate.dense.weight"), vec![inter, d]));
        shapes.push((format!("{p}.intermediate.dense.bias"), vec![inter]));
        shapes.push((format!("{p}.output.dense.weight"), vec![d, inter]));
        shapes.push((format!("{p}.output.dense.bias"), vec![d]));
        shapes.push((format!("{p}.output.LayerNorm.weight"), vec![d]));
        shapes.push((format!("{p}.output.LayerNorm.bias"), vec![d]));
    }
    let (mut header, mut data, mut seed) = (serde_json::Map::new(), Vec::new(), 0x2545_f491_4f6c_dd1du64);
    for (name, shape) in shapes {
        let start = data.len();
        for _ in 0..shape.iter().product::<usize>() {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            // multiples of 1/64 in [-1, 1): exact in f16 and f32
            let v = ((seed >> 40) % 128) as f32 / 64.0 - 1.0;
            if f16 {
                data.extend_from_slice(&f32_to_f16_exact(v).to_le_bytes());
            } else {
                data.extend_from_slice(&v.to_le_bytes());
            }
        }
        header.insert(
            name,
            serde_json::json!({ "dtype": if f16 { "F16" } else { "F32" }, "shape": shape, "data_offsets": [start, data.len()] }),
        );
    }
    header.insert("__metadata__".into(), serde_json::json!({ "format": "pt" }));
    let h = serde_json::to_vec(&header).unwrap();
    let mut out = (h.len() as u64).to_le_bytes().to_vec();
    out.extend_from_slice(&h);
    out.extend_from_slice(&data);
    out
}

#[test]
fn tiny_model_loads_and_returns_unit_vectors() {
    let m = BertModel::from_safetensors(&tiny_model(false), 2).unwrap();
    assert_eq!((m.dim(), m.max_positions(), m.layers.len()), (8, 12, 2));
    let v = m.embed_ids(&[1, 5, 9, 2]).unwrap();
    assert_eq!(v.len(), 8);
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    assert!((norm - 1.0).abs() < 1e-5, "norm {norm}");
    assert!(v.iter().all(|x| x.is_finite()));
    // deterministic, sensitive to which tokens and to their order (positions)
    assert_eq!(v, m.embed_ids(&[1, 5, 9, 2]).unwrap());
    assert!(cosine(&v, &m.embed_ids(&[1, 6, 9, 2]).unwrap()) < 0.999_999);
    assert!(cosine(&v, &m.embed_ids(&[1, 9, 5, 2]).unwrap()) < 0.999_999);
}

#[test]
fn f16_and_f32_files_of_the_same_weights_give_the_same_vector() {
    let a = BertModel::from_safetensors(&tiny_model(false), 2).unwrap();
    let b = BertModel::from_safetensors(&tiny_model(true), 2).unwrap();
    for ids in [vec![1u32, 2], vec![3, 4, 5, 6, 7], (0..12).collect()] {
        assert_eq!(a.embed_ids(&ids).unwrap(), b.embed_ids(&ids).unwrap());
    }
}

#[test]
fn mean_pooling_covers_every_token() {
    // The sentence vector is the normalised mean of the last hidden state.
    let m = BertModel::from_safetensors(&tiny_model(false), 2).unwrap();
    let ids = [1u32, 7, 3, 12, 2];
    let h = m.hidden(&ids).unwrap();
    assert_eq!(h.dim(), (5, 8));
    let mut mean = vec![0f32; 8];
    for row in h.rows() {
        for (acc, x) in mean.iter_mut().zip(row.iter()) {
            *acc += x / 5.0;
        }
    }
    assert!(cosine(&mean, &m.embed_ids(&ids).unwrap()) > 0.999_999);
    // ...and is not the [CLS] row alone.
    assert!(cosine(&h.row(0).to_vec(), &m.embed_ids(&ids).unwrap()) < 0.999_999);
}

#[test]
fn gelu_is_the_tanh_approximation() {
    // erf GELU(1.0) = 0.841345; ggml's tanh form gives 0.841192.
    assert!((gelu_tanh(1.0) - 0.841_192).abs() < 2e-6, "{}", gelu_tanh(1.0));
    assert_eq!(gelu_tanh(0.0), 0.0);
    assert!((gelu_tanh(-3.0) - -0.003_637_4).abs() < 1e-5, "{}", gelu_tanh(-3.0));
}

#[test]
fn malformed_weights_and_inputs_are_errors_not_panics() {
    let good = tiny_model(false);
    assert!(BertModel::from_safetensors(&[], 2).is_err());
    assert!(BertModel::from_safetensors(&good[..7], 2).is_err());
    assert!(BertModel::from_safetensors(&good[..good.len() - 64], 2).is_err(), "truncated data");
    assert!(BertModel::from_safetensors(&u64::MAX.to_le_bytes(), 2).is_err(), "absurd header length");
    assert!(BertModel::from_safetensors(b"\x02\0\0\0\0\0\0\0{}", 2).is_err(), "no tensors");
    assert!(BertModel::from_safetensors(&good, 3).is_err(), "8 is not divisible into 3 heads");
    let m = BertModel::from_safetensors(&good, 2).unwrap();
    assert!(m.embed_ids(&[]).is_err());
    assert!(m.embed_ids(&[1, 16]).is_err(), "id outside the vocabulary");
    assert!(m.embed_ids(&[1; 13]).is_err(), "longer than the position table");
    // A tiny model is not the encoder: the engine refuses its shape.
    assert!(NativeEngine::from_weights(&good).is_err());
}

// ── weights acquisition: hash check and refusal (loopback only) ──────────

/// Serve `body` once over HTTP on a loopback port; returns the URL.
fn serve_once(body: &'static [u8]) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/model.safetensors", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        if let Ok((mut s, _)) = listener.accept() {
            let (mut req, mut b) = (Vec::new(), [0u8; 512]);
            while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                match s.read(&mut b) {
                    Ok(n) if n > 0 => req.extend_from_slice(&b[..n]),
                    _ => break,
                }
            }
            let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
            let _ = s.write_all(head.as_bytes());
            let _ = s.write_all(body);
        }
    });
    url
}

fn dir_names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> =
        std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    v.sort();
    v
}

#[test]
fn a_download_with_the_wrong_hash_is_refused_and_installs_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("models").join(WEIGHTS_FILE);
    let url = serve_once(b"these bytes are not the encoder weights");
    let err = fetch_verified(&url, &dest, WEIGHTS_SHA256, WEIGHTS_BYTES).unwrap_err();
    assert!(err.contains("integrity check"), "{err}");
    assert!(err.contains(WEIGHTS_SHA256), "the error names the expected hash: {err}");
    assert!(err.contains("nothing was installed"), "{err}");
    assert!(!dest.exists(), "a file that failed its hash must not be installed");
    assert_eq!(dir_names(dest.parent().unwrap()), Vec::<String>::new(), "no temporary file left behind");
}

#[test]
fn a_download_with_the_right_hash_is_installed_by_rename() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("models").join("blob.bin");
    let body: &'static [u8] = b"forty-five megabytes, in spirit";
    let sha = hex(&Sha256::digest(body));
    fetch_verified(&serve_once(body), &dest, &sha, body.len() as u64).unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), body);
    assert_eq!(dir_names(dest.parent().unwrap()), vec!["blob.bin".to_string()]);
    assert_eq!(sha256_file(&dest).unwrap(), (sha.clone(), body.len() as u64));
    verify_file(&dest, &sha).unwrap();
}

#[test]
fn a_failed_download_is_an_error_and_installs_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join(WEIGHTS_FILE);
    // A loopback port with nothing listening.
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let err = fetch_verified(&format!("http://127.0.0.1:{port}/x"), &dest, WEIGHTS_SHA256, WEIGHTS_BYTES).unwrap_err();
    assert!(err.contains("download of"), "{err}");
    assert!(!dest.exists());
    assert_eq!(dir_names(dir.path()), Vec::<String>::new());
}

#[test]
fn a_local_file_with_the_wrong_hash_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("model.safetensors");
    // A well-formed model file that is not the pinned one.
    std::fs::write(&path, tiny_model(true)).unwrap();
    let err = verify_file(&path, WEIGHTS_SHA256).unwrap_err();
    assert!(err.contains("not the pinned encoder weights") && err.contains(WEIGHTS_SHA256), "{err}");
    let err = NativeEngine::from_file(&path).err().expect("refused");
    assert!(err.contains("not the pinned encoder weights"), "{err}");
    assert!(path.exists(), "a refused local file is left alone");
    // The marker shortcut only ever skips the hash for a file it recorded.
    assert!(verify_kept(&path, true).is_err());
    assert!(!marker_path(&path).exists(), "no marker for a file that failed");
}

// ── with the real weights (ignored: needs the 45 MB file) ────────────────

fn real_weights() -> PathBuf {
    PathBuf::from(
        std::env::var_os("KANNAKA_TEST_ENCODER_WEIGHTS")
            .expect("set KANNAKA_TEST_ENCODER_WEIGHTS to the weights file (see the module docs)"),
    )
}

#[test]
#[ignore = "needs the 45 MB weights: KANNAKA_TEST_ENCODER_WEIGHTS=<file> cargo test --lib native_embed -- --ignored"]
fn forward_pass_matches_ollama_reference_vectors() {
    let engine = NativeEngine::from_file(&real_weights()).unwrap();
    for (text, tokens, want) in reference() {
        assert_eq!(engine.tokenizer.ids(&text).unwrap().len(), tokens);
        let got = engine.embed(&text).unwrap();
        assert_eq!(got.len(), DIM);
        let c = cosine(&got, &want);
        // Spike, 198 texts, this configuration against Ollama's CPU backend:
        // minimum 0.9999986.
        assert!(c > 0.99999, "cosine {c:.7} for {:?}", &text[..text.len().min(40)]);
        let worst = got.iter().zip(&want).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
        assert!(worst < 2e-3, "largest component difference {worst}");
    }
}

#[test]
#[ignore = "needs the 45 MB weights: KANNAKA_TEST_ENCODER_WEIGHTS=<file> cargo test --lib native_embed -- --ignored"]
fn encoder_ranks_the_right_memory_first_through_the_pipeline() {
    let encoder = NativeEncoder::with_engine(NativeEngine::from_file(&real_weights()).unwrap());
    assert_eq!(encoder.embedding_dim(), DIM);
    assert!(matches!(encoder.embed("   "), Err(EncodingError::EmptyInput)));
    let pipeline = crate::encoding::EncodingPipeline::new(
        Box::new(encoder),
        crate::codebook::Codebook::new(DIM, crate::WAVEFRONT_DIM, 42),
    );
    let memories = [
        "The spare key is under the blue flowerpot by the back door.",
        "Dentist appointment is on the 3rd of March at 9:30.",
        "The wifi password at the cabin is written inside the fuse box.",
    ];
    let q = pipeline.encode_text("where is the spare key").unwrap();
    let scores: Vec<f32> =
        memories.iter().map(|m| crate::wave::cosine_similarity(&q, &pipeline.encode_text(m).unwrap())).collect();
    assert!(scores[0] > scores[1] + 0.1 && scores[0] > scores[2] + 0.1, "{scores:?}");
}
