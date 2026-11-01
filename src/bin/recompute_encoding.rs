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

/// The `.encoder` stamp for the strict (ollama) pipeline this tool builds.
fn encoder_stamp(model: &str, dim: usize) -> String {
    format!("ollama:{model}:{dim}")
}

/// Probe the endpoint we are ACTUALLY going to embed against, not a fixed one.
fn embedder_reachable(url: &str) -> bool {
    ureq::get(&format!("{url}/api/tags"))
        .timeout(Duration::from_secs(3))
        .call()
        .is_ok()
}

const USAGE: &str = "Usage: kannaka-recompute-encoding [data-dir] [--dry-run]\n\
Re-encodes every text memory in <data-dir>/kannaka.hrm (default: KANNAKA_DATA_DIR, else ~/.kannaka).\n\
SNAPSHOT the HRM first and run with --dry-run to see the count. Never run on a substrate HRM.";

/// What the command line asks for. `--help`/`-h` and any unknown flag are
/// resolved here, before anything touches a store: through 0.16.15 every
/// argument except `--dry-run` that started with `--` was ignored, so
/// `kannaka-recompute-encoding --help` re-encoded the default store, and `-h`
/// was taken as the data directory.
#[derive(Debug, PartialEq)]
enum Invocation {
    Help,
    BadFlag(String),
    Run { dry_run: bool, data_dir: Option<PathBuf> },
}

fn parse_invocation(args: &[String]) -> Invocation {
    let mut dry_run = false;
    let mut data_dir = None;
    for a in args.iter().skip(1) {
        match a.as_str() {
            "--help" | "-h" => return Invocation::Help,
            "--dry-run" => dry_run = true,
            f if f.starts_with('-') => return Invocation::BadFlag(f.to_string()),
            d if data_dir.is_none() => data_dir = Some(PathBuf::from(d)),
            extra => return Invocation::BadFlag(extra.to_string()),
        }
    }
    Invocation::Run { dry_run, data_dir }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (dry_run, data_dir_arg) = match parse_invocation(&args) {
        Invocation::Help => {
            println!("{USAGE}");
            return;
        }
        Invocation::BadFlag(f) => {
            eprintln!("kannaka-recompute-encoding: unexpected argument {f:?}\n{USAGE}");
            std::process::exit(2);
        }
        Invocation::Run { dry_run, data_dir } => (dry_run, data_dir),
    };
    let data_dir = data_dir_arg
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

    // TODO(#1116 part 4): batch the embeds through Ollama's `/api/embed` with an
    // array `input`; `OllamaEncoder` only has a single-string path today.
    let started = std::time::Instant::now();
    let mut progress = |done: usize, total: usize| {
        eprintln!("progress: {done}/{total} rows, {:.1}s elapsed", started.elapsed().as_secs_f64());
    };
    match store.recompute_encoding_with_progress(dry_run, &mut progress) {
        Ok((scanned, updated)) => {
            if dry_run {
                eprintln!("[dry-run] would re-encode {updated} of {scanned} memories (text only)");
            } else {
                eprintln!("Re-encoded {updated} of {scanned} memories through the current pipeline");
                eprintln!("Cluster sidecar invalidated; verify recall before rolling out further.");
                // #1116 part 3: the vectors changed, so the stamp must too —
                // otherwise every later open adopts the old one.
                let new_stamp = encoder_stamp(&model, dim);
                match kannaka_memory::write_encoder_stamp(&data_dir, &new_stamp) {
                    Ok(old) => eprintln!(
                        ".encoder stamp: old={} new={new_stamp}",
                        old.as_deref().unwrap_or("(none)")
                    ),
                    Err(e) => {
                        eprintln!("Error: vectors rewritten but .encoder stamp NOT updated: {e}");
                        eprintln!("Set {}/.encoder to '{new_stamp}' by hand.", data_dir.display());
                        std::process::exit(1);
                    }
                }
            }
        }
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod invocation_tests {
    use super::{encoder_stamp, parse_invocation, Invocation};
    use std::path::PathBuf;

    fn argv(rest: &[&str]) -> Vec<String> {
        std::iter::once("kannaka-recompute-encoding")
            .chain(rest.iter().copied())
            .map(String::from)
            .collect()
    }

    #[test]
    fn help_never_reaches_the_store() {
        // Through 0.16.15 `--help` was skipped as an unknown flag and the
        // default store was re-encoded.
        assert_eq!(parse_invocation(&argv(&["--help"])), Invocation::Help);
        assert_eq!(parse_invocation(&argv(&["-h"])), Invocation::Help);
        assert_eq!(parse_invocation(&argv(&["/data", "--help"])), Invocation::Help);
        assert_eq!(parse_invocation(&argv(&["--dry-run", "-h"])), Invocation::Help);
    }

    #[test]
    fn unknown_flags_and_extra_positionals_are_refused() {
        assert_eq!(parse_invocation(&argv(&["--force"])), Invocation::BadFlag("--force".into()));
        assert_eq!(parse_invocation(&argv(&["-x"])), Invocation::BadFlag("-x".into()));
        assert_eq!(parse_invocation(&argv(&["/a", "/b"])), Invocation::BadFlag("/b".into()));
    }

    #[test]
    fn the_documented_forms_still_run() {
        assert_eq!(parse_invocation(&argv(&[])), Invocation::Run { dry_run: false, data_dir: None });
        assert_eq!(
            parse_invocation(&argv(&["/data", "--dry-run"])),
            Invocation::Run { dry_run: true, data_dir: Some(PathBuf::from("/data")) }
        );
        assert_eq!(
            parse_invocation(&argv(&["--dry-run", "/data"])),
            Invocation::Run { dry_run: true, data_dir: Some(PathBuf::from("/data")) }
        );
    }

    #[test]
    fn stamp_is_rewritten_atomically_and_reports_the_old_one() {
        let dir = std::env::temp_dir().join(format!("kannaka-recompute-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(".encoder"), "hash:384:42
").unwrap();
        let new = encoder_stamp("all-minilm", 384);
        let old = kannaka_memory::write_encoder_stamp(&dir, &new).unwrap();
        assert_eq!(old.as_deref(), Some("hash:384:42"));
        assert_eq!(std::fs::read_to_string(dir.join(".encoder")).unwrap(), "ollama:all-minilm:384");
        // No stamp before: old is None, and no temp litter remains.
        std::fs::remove_file(dir.join(".encoder")).unwrap();
        assert_eq!(kannaka_memory::write_encoder_stamp(&dir, &new).unwrap(), None);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
