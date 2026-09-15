//! One-shot tool: re-encode every TEXT wavefront's content through the current
//! pipeline, fixing HRM files whose vectors were written by the broken #106
//! encoder (#107). Chiral (v2) HRMs only.
//!
//! Usage: kannaka-recompute-encoding [data-dir] [--dry-run]
//!   KANNAKA_ENCODER_URL   (default http://localhost:11434)
//!   KANNAKA_ENCODER_MODEL (default all-minilm)
//!   KANNAKA_ENCODER_DIM   (default 384 — must match the codebook input dim)
//!
//! SCOPE / SAFETY:
//!   - TEXT HRMs only. Dream-hallucinated wavefronts and non-text (audio/visual)
//!     modalities are skipped automatically. But do NOT run this on a SUBSTRATE
//!     HRM (ADR-0027): its raw one-hot anchors are Modality::Unknown, which is
//!     indistinguishable from pre-ADR-0042 text memories, so they WOULD be
//!     re-encoded and their orthogonal Kuramoto seed destroyed.
//!   - Builds a STRICT pipeline (`make_strict_pipeline`) with **no hash
//!     fallback**. This is the safety property, and it is not the same one the
//!     up-front reachability probe provides. The probe proves the embedder
//!     answered once; it says nothing about embed number 300. Under the default
//!     `make_pipeline`, a mid-run outage does not fail — `CompositeEncoder`
//!     quietly returns a hash vector — so the corpus would end up part semantic
//!     and part hashed, in one file, under one `.encoder` stamp that cannot
//!     express the difference and with nothing to detect it later.
//!     With no fallback, that outage is an error; `re_encode_all` propagates it
//!     with `?`; and the store is only written after it returns `Ok`. All or
//!     nothing, for real rather than by hope.
//!
//! ROLLOUT: SNAPSHOT the HRM first, run --dry-run to see the count, then run for
//! real and verify recall (#83 repro) before adopting / rolling to the next box.
//! `d_eff` from `kannaka status` is the cheapest before/after signal: it is the
//! participation ratio of the Gram spectrum, bounded in [1, n], so it reports how
//! separated the memories actually are rather than how many exist.

use kannaka_memory::hrm_store::HrmStore;
use kannaka_memory::openclaw::make_strict_pipeline;
use std::path::PathBuf;
use std::time::Duration;

/// Where the embedder lives. Defaults match `OllamaEncoder::default_local`, and
/// are overridable because the box that holds a store is not always the box that
/// can run a model — O1 serves the swarm on one core and has no Ollama at all.
const DEFAULT_URL: &str = "http://localhost:11434";
const DEFAULT_MODEL: &str = "all-minilm";
const DEFAULT_DIM: usize = 384;

fn encoder_url() -> String {
    std::env::var("KANNAKA_ENCODER_URL").unwrap_or_else(|_| DEFAULT_URL.to_string())
}

fn encoder_model() -> String {
    std::env::var("KANNAKA_ENCODER_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string())
}

fn encoder_dim() -> usize {
    std::env::var("KANNAKA_ENCODER_DIM")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_DIM)
}

/// Probe the endpoint we are ACTUALLY going to embed against, not a fixed one.
fn embedder_reachable(url: &str) -> bool {
    ureq::get(&format!("{url}/api/tags"))
        .timeout(Duration::from_secs(3))
        .call()
        .is_ok()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dry_run = args.iter().any(|a| a == "--dry-run");
    let data_dir = args
        .iter()
        .skip(1)
        .find(|a| !a.starts_with("--"))
        .map(PathBuf::from)
        .or_else(|| std::env::var("KANNAKA_DATA_DIR").ok().map(PathBuf::from))
        .unwrap_or_else(|| {
            let home = std::env::var("USERPROFILE")
                .or_else(|_| std::env::var("HOME"))
                .unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join(".kannaka")
        });

    let hrm_path = data_dir.join("kannaka.hrm");
    eprintln!("HRM: {}", hrm_path.display());
    eprintln!(
        "NOTE: TEXT HRMs only — hallucinations + non-text modalities are skipped; do NOT run on a \
         substrate HRM (raw anchors would be corrupted)."
    );
    if dry_run {
        eprintln!("(dry run — no changes will be written)");
    }

    let (url, model, dim) = (encoder_url(), encoder_model(), encoder_dim());
    eprintln!("encoder: {model} @ {url} ({dim}d)");

    // Up-front probe so the common failure is a clean refusal rather than a
    // half-finished corpus. It is NOT the real guarantee: it proves the embedder
    // answered once, not that it survives all N embeds. The guarantee is the
    // strict pipeline below — with no hash fallback, an outage mid-run becomes an
    // error, `re_encode_all` propagates it, and nothing is written to disk.
    if !dry_run && !embedder_reachable(&url) {
        eprintln!(
            "ABORT: embedder ({url}) is unreachable. Re-encoding needs the real model — \
             refusing to proceed. Start it (or point KANNAKA_ENCODER_URL at one) and retry, \
             or pass --dry-run."
        );
        std::process::exit(1);
    }

    let pipeline = match make_strict_pipeline(url, model, dim) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("ABORT: {e}");
            std::process::exit(1);
        }
    };
    let mut store = HrmStore::load(pipeline, hrm_path).unwrap_or_else(|e| {
        eprintln!("Failed to load HRM: {e}");
        std::process::exit(1);
    });

    match store.recompute_encoding(dry_run) {
        Ok((scanned, updated)) => {
            if dry_run {
                eprintln!("[dry-run] would re-encode {updated} of {scanned} memories (text only)");
            } else {
                eprintln!("Re-encoded {updated} of {scanned} memories through the current pipeline");
                eprintln!("Cluster sidecar invalidated; verify recall before rolling out further.");
            }
        }
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    }
}
