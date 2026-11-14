# Spike: native all-MiniLM-L6-v2 embedder vs Ollama `all-minilm`

Standalone probe crate (not part of the `kannaka` binary). It embeds 200 varied
texts with a native runtime and compares every vector with what Ollama
`all-minilm` returns for the same text. Measured 2026-11-14 on a Windows 11
laptop (i7-1065G7, 8 threads), Ollama 0.34.4, release builds, default profile.

## What Ollama `all-minilm` actually does

- Weights: all-MiniLM-L6-v2 as an F16 GGUF, run by llama.cpp.
- Pooling: mean over all tokens (`bert.pooling_type = 1`), then L2 normalisation.
- Window: `num_ctx 256`. Longer input keeps the first 255 tokens and re-closes
  with `[SEP]` (256 total). Measured against four candidate schemes; this one
  scores 0.99998, the others 0.997 to 0.9999.
- GELU: ggml's tanh approximation, not erf.
- Tokenizer: llama.cpp's own WordPiece, not Hugging Face's. Three differences
  showed up in 200 texts: no 100-character word limit (HF emits `[UNK]`),
  Hangul is not decomposed, Devanagari vowel signs are kept.
- The stock `tokenizer.json` pads to and truncates at 128 tokens. Both must be
  switched off or long texts diverge silently.

## Parity (cosine against Ollama, same text)

Ollama is not bit-stable across its own backends, so both are shown. "198"
excludes the Korean and Devanagari texts (tokenizer difference, see below).

| pair | min (198) | median (198) | top-5 same order | top-5 same set | top-1 |
|---|---|---|---|---|---|
| Ollama CPU vs Ollama GPU (its own noise floor) | 0.999936 | 0.999986 | 191/198 | 193/198 | 198/198 |
| native, erf GELU, fp32 vs Ollama GPU | 0.999936 | 0.999985 | 187/198 | 192/198 | 198/198 |
| native, erf GELU, fp32 vs Ollama CPU | 0.999993 | 0.999999 | 192/198 | 195/198 | 198/198 |
| native, tanh GELU, fp16 file vs Ollama GPU | 0.999937 | 0.999986 | 188/198 | 192/198 | 198/198 |
| native, tanh GELU, fp16 file vs Ollama CPU | 0.9999986 | 0.9999995 | 194/198 | 196/198 | 198/198 |

The hand-written, candle and tract runtimes give the same numbers to the last
digit shown (tract has erf GELU only). Top-5 lists are not identical: every
change is a swap between neighbours whose reference scores differ by less than
4e-5 (native vs Ollama CPU) or 1.1e-3 (anything vs Ollama GPU).

Not compatible: Korean 0.659, Devanagari 0.964 (2 of 200). With the stock
100-character word limit a third text (a 138-character unbroken word) drops to
0.206; the probe lifts the limit. A naive port of llama.cpp's WordPiece from
current upstream fixed Korean and the long word but broke four other texts, so
a faithful port is not demonstrated here.

Dynamic int8 ONNX (23 MB): median 0.992, min 0.21, top-5 same order 25/200.
Not usable for existing stores. tract cannot load that file at all.

## Speed (ms per text, one text per call)

| | cold: process start to first vector | warm median | warm median, <= 32 tokens | mean over the 200 |
|---|---|---|---|---|
| hand-written (ndarray 0.15) | 240 to 350 | 20 to 32 | 20 to 28 | 47 to 57 |
| candle 0.11 | 180 to 200 | 17 to 18 | 16 to 17 | 38 to 40 |
| tract 0.23 | 530 to 640 | 11 | 10 | 31 to 33 |
| Ollama via `127.0.0.1`, model loaded | 1100 to 2400 to load the model | 23 to 27 (Python), 33 (ureq) | 21 to 24 | 34 to 36 |
| Ollama via `localhost` (kannaka's default URL) | 12200 | 2050 (ureq), 2340 (Python) | | 2400 |
| ONNX Runtime 1.24 from Python, for scale | not measured | 4.6 | 4.2 | 11.8 |

On this Windows machine every request to `http://localhost:11434` takes about
2 s longer than the same request to `http://127.0.0.1:11434`.

## Size

| | Windows release exe | delta |
|---|---|---|
| baseline (JSON only) | 0.14 MB | |
| + `tokenizers` 0.22 (fancy-regex, no onig) | 3.25 MB | +3.1 MB |
| + hand-written forward pass (ndarray, safetensors, half, libm) | 3.98 MB | +0.7 MB more |
| + candle 0.11 | 5.88 MB | +2.6 MB more |
| + tract-onnx 0.23 | 27.4 MB | +24.1 MB more |

Measured in this probe, not in `kannaka` (14.5 MB today); shared dependencies
would make the real delta somewhat smaller. Weights: fp32 safetensors 90.9 MB,
fp32 ONNX 90.4 MB, fp16 safetensors 45.1 MB (same parity as fp32), int8 ONNX
23.0 MB (fails parity).

## Builds on the five release targets

| | windows-msvc | linux-musl x86_64 / aarch64 | macOS x86_64 / aarch64 |
|---|---|---|---|
| hand-written | built and run | `cargo check` passes, no C compiler involved | `cargo check` passes |
| candle | built and run | `cargo check` passes, no C compiler involved | `cargo check` passes |
| tract | built and run | `cargo check` stops in `tract-linalg`'s build script, which needs a C/asm cross compiler (CI has one; not tried) | same build-script stop |
| ort / fastembed | not built | no prebuilt ONNX Runtime for musl (read from `ort-sys` `dist.tsv`) | none for x86_64 macOS either |

`cargo check` type-checks only; nothing was linked or run off Windows.

## Reproduce

    # models/: model.safetensors, tokenizer.json, config.json, vocab.txt, model.onnx
    # from https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2 (Apache-2.0)
    python gen_sentences.py
    python fetch_ollama.py http://127.0.0.1:11434        # data/ollama_ref.json
    python fetch_ollama.py http://127.0.0.1:11434 cpu    # data/ollama_ref_cpu.json
    cargo run --release --features scratch -- scratch --gelu-tanh --out data/out_scratch_tanh.json
    cargo run --release --features candle  -- candle  --gelu-tanh --out data/out_candle_tanh.json
    cargo run --release --features tract   -- tract   --out data/out_tract.json
    python compare.py data/out_scratch_tanh.json data/out_candle_tanh.json data/out_tract.json
