//! Parity probe: embed data/sentences.json with a native runtime and compare
//! each vector with what Ollama `all-minilm` returned (data/ollama_ref.json).
//!
//!   cargo run --release --features candle  -- candle  [--max-tokens 256] [--f16]
//!   cargo run --release --features tract   -- tract   [--model models/model.onnx]
//!   cargo run --release --features scratch -- scratch [--f16]
//!
//! With no backend feature the binary is the size baseline (JSON only).

use std::time::Instant;

#[allow(dead_code)]
struct Args {
    backend: String,
    max_tokens: usize,
    f16: bool,
    model: Option<String>,
    out: Option<String>,
    one: bool,
}

fn args() -> Args {
    let a: Vec<String> = std::env::args().collect();
    let get = |k: &str| a.iter().position(|x| x == k).and_then(|i| a.get(i + 1)).cloned();
    Args {
        backend: a.get(1).cloned().unwrap_or_else(|| "none".into()),
        max_tokens: get("--max-tokens").and_then(|v| v.parse().ok()).unwrap_or(256),
        f16: a.iter().any(|x| x == "--f16"),
        model: get("--model"),
        out: get("--out"),
        one: a.iter().any(|x| x == "--one"),
    }
}

#[allow(dead_code)]
trait Backend {
    /// Token ids (already [CLS] .. [SEP], truncated) -> last hidden state, row-major [n, 384].
    fn hidden(&self, ids: &[u32]) -> Vec<f32>;
}

#[allow(dead_code)]
fn pool_normalize(h: &[f32], n: usize) -> Vec<f32> {
    let d = h.len() / n;
    let mut v = vec![0f32; d];
    for t in 0..n {
        for j in 0..d {
            v[j] += h[t * d + j];
        }
    }
    let inv = 1.0 / n as f32;
    v.iter_mut().for_each(|x| *x *= inv);
    let norm = v.iter().map(|x| (*x as f64) * (*x as f64)).sum::<f64>().sqrt() as f32;
    v.iter_mut().for_each(|x| *x /= norm);
    v
}

fn cosine(a: &[f32], b: &[f32]) -> f64 {
    let (mut d, mut na, mut nb) = (0f64, 0f64, 0f64);
    for (x, y) in a.iter().zip(b) {
        d += *x as f64 * *y as f64;
        na += *x as f64 * *x as f64;
        nb += *y as f64 * *y as f64;
    }
    d / (na.sqrt() * nb.sqrt())
}

/// Top-k neighbours of every vector (cosine, self excluded, ties by index).
fn topk(vs: &[Vec<f32>], k: usize) -> Vec<Vec<usize>> {
    (0..vs.len())
        .map(|i| {
            let mut s: Vec<(usize, f64)> =
                (0..vs.len()).filter(|j| *j != i).map(|j| (j, cosine(&vs[i], &vs[j]))).collect();
            s.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap().then(a.0.cmp(&b.0)));
            s.into_iter().take(k).map(|x| x.0).collect()
        })
        .collect()
}

fn compare(ours: &[Vec<f32>], reference: &[Vec<f32>], sents: &[String]) {
    let mut cs: Vec<(f64, usize)> =
        ours.iter().zip(reference).enumerate().map(|(i, (a, b))| (cosine(a, b), i)).collect();
    cs.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    let n = cs.len();
    println!(
        "cosine vs Ollama: min={:.6} p05={:.6} median={:.6} max={:.6}  (<0.9999: {}, <0.999: {})",
        cs[0].0,
        cs[n / 20].0,
        cs[n / 2].0,
        cs[n - 1].0,
        cs.iter().filter(|c| c.0 < 0.9999).count(),
        cs.iter().filter(|c| c.0 < 0.999).count()
    );
    for (c, i) in cs.iter().take(6) {
        let t: String = sents[*i].chars().take(60).collect();
        println!("  worst {:.6}  #{i} {:?}", c, t);
    }
    let (ta, tb) = (topk(ours, 5), topk(reference, 5));
    let same_order = ta.iter().zip(&tb).filter(|(a, b)| a == b).count();
    let same_set = ta
        .iter()
        .zip(&tb)
        .filter(|(a, b)| {
            let (mut x, mut y) = ((*a).clone(), (*b).clone());
            x.sort();
            y.sort();
            x == y
        })
        .count();
    let top1 = ta.iter().zip(&tb).filter(|(a, b)| a[0] == b[0]).count();
    println!("top-5 neighbours: identical order {same_order}/{n}, identical set {same_set}/{n}, top-1 {top1}/{n}");
}

fn load_sents() -> Vec<String> {
    let v: serde_json::Value =
        serde_json::from_slice(&std::fs::read("data/sentences.json").expect("data/sentences.json")).unwrap();
    v.as_array().unwrap().iter().map(|s| s.as_str().unwrap().to_string()).collect()
}

fn load_ref() -> (Vec<Vec<f32>>, Vec<u64>) {
    let v: serde_json::Value =
        serde_json::from_slice(&std::fs::read("data/ollama_ref.json").expect("data/ollama_ref.json")).unwrap();
    let vecs = v["vectors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r.as_array().unwrap().iter().map(|x| x.as_f64().unwrap() as f32).collect())
        .collect();
    let counts = v["prompt_eval_count"].as_array().unwrap().iter().map(|x| x.as_u64().unwrap_or(0)).collect();
    (vecs, counts)
}

#[cfg(feature = "candle")]
mod candle_backend {
    use candle_core::{DType, Device, Tensor};
    use candle_nn::VarBuilder;
    use candle_transformers::models::bert::{BertModel, Config};

    pub struct Candle(BertModel);
    impl Candle {
        pub fn load(path: &str) -> Self {
            let mut cj: serde_json::Value =
                serde_json::from_slice(&std::fs::read("models/config.json").unwrap()).unwrap();
            if std::env::args().any(|x| x == "--gelu-tanh") {
                cj["hidden_act"] = serde_json::json!("geluapproximate");
            }
            let cfg: Config = serde_json::from_value(cj).unwrap();
            let vb = VarBuilder::from_buffered_safetensors(std::fs::read(path).unwrap(), DType::F32, &Device::Cpu)
                .unwrap();
            Candle(BertModel::load(vb, &cfg).unwrap())
        }
    }
    impl super::Backend for Candle {
        fn hidden(&self, ids: &[u32]) -> Vec<f32> {
            let t = Tensor::new(ids, &Device::Cpu).unwrap().unsqueeze(0).unwrap();
            let tt = t.zeros_like().unwrap();
            let h = self.0.forward(&t, &tt, None).unwrap();
            h.squeeze(0).unwrap().flatten_all().unwrap().to_vec1::<f32>().unwrap()
        }
    }
}

#[cfg(feature = "tract")]
mod tract_backend {
    use tract_onnx::prelude::*;

    pub struct Tract(std::sync::Arc<TypedSimplePlan>);
    impl Tract {
        pub fn load(path: &str) -> Self {
            let mut m = tract_onnx::onnx().model_for_path(path).unwrap();
            let s = m.symbols.sym("S");
            for i in 0..3 {
                m.set_input_fact(i, InferenceFact::dt_shape(i64::datum_type(), tvec![1.to_dim(), s.to_dim()]))
                    .unwrap();
            }
            Tract(m.into_optimized().unwrap().into_runnable().unwrap())
        }
    }
    impl super::Backend for Tract {
        fn hidden(&self, ids: &[u32]) -> Vec<f32> {
            let n = ids.len();
            let i: Tensor =
                tract_ndarray::Array2::from_shape_vec((1, n), ids.iter().map(|x| *x as i64).collect()).unwrap().into();
            let ones: Tensor = tract_ndarray::Array2::<i64>::ones((1, n)).into();
            let zeros: Tensor = tract_ndarray::Array2::<i64>::zeros((1, n)).into();
            // ONNX input order: input_ids, attention_mask, token_type_ids
            let out = self.0.run(tvec![i.into(), ones.into(), zeros.into()]).unwrap();
            out[0].to_plain_array_view::<f32>().unwrap().iter().copied().collect()
        }
    }
}

/// Hand-written BERT encoder: ~100 lines on ndarray, no ML framework.
#[cfg(feature = "scratch")]
mod scratch_backend {
    use ndarray::{s, Array1, Array2, Axis};
    use safetensors::SafeTensors;

    struct Lin {
        w: Array2<f32>, // stored transposed: [in, out]
        b: Array1<f32>,
    }
    impl Lin {
        fn f(&self, x: &Array2<f32>) -> Array2<f32> {
            x.dot(&self.w) + &self.b
        }
    }
    struct Ln {
        g: Array1<f32>,
        b: Array1<f32>,
    }
    impl Ln {
        fn f(&self, mut x: Array2<f32>) -> Array2<f32> {
            for mut r in x.rows_mut() {
                let m = r.mean().unwrap();
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
        ln1: Ln,
        i: Lin,
        out: Lin,
        ln2: Ln,
    }
    pub struct Scratch {
        word: Array2<f32>,
        pos: Array2<f32>,
        typ0: Array1<f32>,
        ln: Ln,
        layers: Vec<Layer>,
        heads: usize,
        gelu_tanh: bool,
    }

    fn tensor(st: &SafeTensors, name: &str, f16: bool) -> (Vec<usize>, Vec<f32>) {
        let t = st.tensor(name).unwrap_or_else(|_| panic!("missing tensor {name}"));
        let shape = t.shape().to_vec();
        let data: Vec<f32> = match t.dtype() {
            safetensors::Dtype::F32 => t
                .data()
                .chunks_exact(4)
                .map(|c| {
                    let v = f32::from_le_bytes([c[0], c[1], c[2], c[3]]);
                    // --f16: round-trip through half precision, as Ollama's F16 GGUF stores it
                    if f16 { half::f16::from_f32(v).to_f32() } else { v }
                })
                .collect(),
            safetensors::Dtype::F16 => {
                t.data().chunks_exact(2).map(|c| half::f16::from_le_bytes([c[0], c[1]]).to_f32()).collect()
            }
            d => panic!("unsupported dtype {d:?}"),
        };
        (shape, data)
    }
    fn m2(st: &SafeTensors, n: &str, f16: bool) -> Array2<f32> {
        let (s, d) = tensor(st, n, f16);
        Array2::from_shape_vec((s[0], s[1]), d).unwrap()
    }
    fn m1(st: &SafeTensors, n: &str, f16: bool) -> Array1<f32> {
        Array1::from_vec(tensor(st, n, f16).1)
    }
    fn lin(st: &SafeTensors, p: &str, f16: bool) -> Lin {
        Lin {
            w: m2(st, &format!("{p}.weight"), f16).t().as_standard_layout().to_owned(),
            b: m1(st, &format!("{p}.bias"), f16),
        }
    }
    fn ln(st: &SafeTensors, p: &str, f16: bool) -> Ln {
        Ln { g: m1(st, &format!("{p}.weight"), f16), b: m1(st, &format!("{p}.bias"), f16) }
    }

    impl Scratch {
        pub fn load(path: &str, f16: bool) -> Self {
            let bytes = std::fs::read(path).unwrap();
            let st = SafeTensors::deserialize(&bytes).unwrap();
            let layers = (0..6)
                .map(|i| {
                    let p = format!("encoder.layer.{i}");
                    Layer {
                        q: lin(&st, &format!("{p}.attention.self.query"), f16),
                        k: lin(&st, &format!("{p}.attention.self.key"), f16),
                        v: lin(&st, &format!("{p}.attention.self.value"), f16),
                        o: lin(&st, &format!("{p}.attention.output.dense"), f16),
                        ln1: ln(&st, &format!("{p}.attention.output.LayerNorm"), f16),
                        i: lin(&st, &format!("{p}.intermediate.dense"), f16),
                        out: lin(&st, &format!("{p}.output.dense"), f16),
                        ln2: ln(&st, &format!("{p}.output.LayerNorm"), f16),
                    }
                })
                .collect();
            Scratch {
                word: m2(&st, "embeddings.word_embeddings.weight", f16),
                pos: m2(&st, "embeddings.position_embeddings.weight", f16),
                typ0: m2(&st, "embeddings.token_type_embeddings.weight", f16).row(0).to_owned(),
                ln: ln(&st, "embeddings.LayerNorm", f16),
                layers,
                heads: 12,
                gelu_tanh: std::env::args().any(|x| x == "--gelu-tanh"),
            }
        }
    }

    impl super::Backend for Scratch {
        fn hidden(&self, ids: &[u32]) -> Vec<f32> {
            let n = ids.len();
            let d = self.word.ncols();
            let hd = d / self.heads;
            let mut x = Array2::<f32>::zeros((n, d));
            for (t, id) in ids.iter().enumerate() {
                let mut r = x.row_mut(t);
                r += &self.word.row(*id as usize);
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
                if self.gelu_tanh {
                    // ggml's GELU (what llama.cpp / Ollama computes): the tanh approximation
                    mid.mapv_inplace(|v| 0.5 * v * (1.0 + (0.797_884_56_f32 * v * (1.0 + 0.044715 * v * v)).tanh()));
                } else {
                    mid.mapv_inplace(|v| 0.5 * v * (1.0 + libm::erff(v * std::f32::consts::FRAC_1_SQRT_2)));
                }
                x = l.ln2.f(l.out.f(&mid) + &x);
            }
            x.into_raw_vec()
        }
    }
}

#[cfg(feature = "tok")]
fn run(a: &Args, t0: Instant) {
    let sents = load_sents();
    // tokenizer.json as shipped differs from what Ollama (llama.cpp WPM) does in three ways:
    //  - it pads to and truncates at 128 tokens; Ollama's window is num_ctx = 256, no padding;
    //  - WordPiece gives up ([UNK]) on words over 100 chars; llama.cpp has no such limit.
    // `--stock-words` keeps the 100-char limit to show the difference.
    let mut tj: serde_json::Value = serde_json::from_slice(&std::fs::read("models/tokenizer.json").unwrap()).unwrap();
    if !std::env::args().any(|x| x == "--stock-words") {
        tj["model"]["max_input_chars_per_word"] = serde_json::json!(1_000_000);
    }
    let mut tok = tokenizers::Tokenizer::from_bytes(serde_json::to_vec(&tj).unwrap()).unwrap();
    tok.with_padding(None);
    tok.with_truncation(None).unwrap();
    let default_model = if a.backend == "tract" { "models/model.onnx" } else { "models/model.safetensors" };
    let path = a.model.clone().unwrap_or_else(|| default_model.to_string());
    let be: Box<dyn Backend> = match a.backend.as_str() {
        #[cfg(feature = "candle")]
        "candle" => Box::new(candle_backend::Candle::load(&path)),
        #[cfg(feature = "tract")]
        "tract" => Box::new(tract_backend::Tract::load(&path)),
        #[cfg(feature = "scratch")]
        "scratch" => Box::new(scratch_backend::Scratch::load(&path, a.f16)),
        #[allow(unreachable_code)]
        other => return eprintln!("backend {other:?} not compiled in"),
    };
    let load_ms = t0.elapsed().as_secs_f64() * 1e3;

    let embed = |text: &str| -> (Vec<f32>, usize) {
        let enc = tok.encode(text, true).unwrap();
        let mut ids = enc.get_ids().to_vec();
        if ids.len() > a.max_tokens {
            // keep the first max_tokens-1 tokens and re-close with [SEP]
            ids.truncate(a.max_tokens - 1);
            ids.push(102);
        }
        let n = ids.len();
        (pool_normalize(&be.hidden(&ids), n), n)
    };

    let (v0, _) = embed(&sents[0]);
    let first_ms = t0.elapsed().as_secs_f64() * 1e3;
    if a.one {
        println!("cold: load {load_ms:.0} ms, process start -> first vector {first_ms:.0} ms (dim {})", v0.len());
        return;
    }
    let (mut vecs, mut counts, mut times) = (Vec::new(), Vec::new(), Vec::new());
    for s in &sents {
        let t = Instant::now();
        let (v, n) = embed(s);
        times.push((t.elapsed().as_secs_f64() * 1e3, n));
        vecs.push(v);
        counts.push(n as u64);
    }
    let total: f64 = times.iter().map(|t| t.0).sum();
    let mut short: Vec<f64> = times.iter().filter(|t| t.1 <= 32).map(|t| t.0).collect();
    let mut all: Vec<f64> = times.iter().map(|t| t.0).collect();
    short.sort_by(|a, b| a.partial_cmp(b).unwrap());
    all.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!(
        "[{}{}] cold: load {load_ms:.0} ms, start -> first vector {first_ms:.0} ms | warm: median {:.2} ms, mean {:.2} ms, median(<=32 tok) {:.2} ms, max {:.1} ms",
        a.backend,
        if a.f16 { "+f16" } else { "" },
        all[all.len() / 2],
        total / all.len() as f64,
        short[short.len() / 2],
        all[all.len() - 1]
    );
    let (reference, ref_counts) = load_ref();
    let mism: Vec<usize> = (0..counts.len()).filter(|i| ref_counts[*i] != 0 && ref_counts[*i] != counts[*i]).collect();
    println!("token-count mismatches vs Ollama prompt_eval_count: {} {:?}", mism.len(), &mism[..mism.len().min(12)]);
    for i in mism.iter().take(12) {
        let t: String = sents[*i].chars().take(40).collect();
        println!("  #{i} ours={} ollama={} {:?}", counts[*i], ref_counts[*i], t);
    }
    compare(&vecs, &reference, &sents);
    if let Some(o) = &a.out {
        std::fs::write(o, serde_json::to_vec(&vecs).unwrap()).unwrap();
    }
}

#[cfg(not(feature = "tok"))]
fn run(_a: &Args, _t0: Instant) {
    // size baseline: no tokenizer, no runtime
    let _ = (load_sents as fn() -> Vec<String>, load_ref as fn() -> (Vec<Vec<f32>>, Vec<u64>));
    let _ = compare as fn(&[Vec<f32>], &[Vec<f32>], &[String]);
    println!("baseline build (no backend feature)");
}

/// Times `POST /api/embed` through ureq exactly as kannaka's OllamaEncoder does (one request per text).
#[cfg(feature = "http-timing")]
fn http_timing(base: &str) {
    let sents = load_sents();
    let mut ts = Vec::new();
    for s in sents.iter().take(40) {
        let t = Instant::now();
        let r = ureq::post(&format!("{base}/api/embed"))
            .set("Content-Type", "application/json")
            .send_json(serde_json::json!({"model": "all-minilm", "input": s}));
        let ok = r.is_ok();
        ts.push(t.elapsed().as_secs_f64() * 1e3);
        if !ok {
            println!("request failed");
            return;
        }
    }
    let first = ts[0];
    ts.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!("ureq {base}: first {first:.0} ms, median {:.1} ms over {} texts", ts[ts.len() / 2], ts.len());
}

fn main() {
    let t0 = Instant::now();
    let a = args();
    #[cfg(feature = "http-timing")]
    if a.backend == "http" {
        return http_timing(a.model.as_deref().unwrap_or("http://localhost:11434"));
    }
    run(&a, t0);
}
