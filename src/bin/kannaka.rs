//! Kannaka CLI — Wave-Interference Memory System.

use std::env;
// std::io::Read used in sub-commands
use std::path::PathBuf;
use std::process;

use kannaka_memory::config::{self, KannakaConfig};
use kannaka_memory::observe::MemoryIntrospector;
use kannaka_memory::openclaw::KannakaMemorySystem;

#[cfg(feature = "glyph")]
use kannaka_memory::glyph_bridge::GlyphEncoder;

use kannaka_memory::MediumBackend;
use kannaka_memory::{Codebook, EncodingPipeline, HrmStore, SimpleHashEncoder};

#[cfg(feature = "collective")]
use kannaka_memory::collective::{dream_cross_modal_link, Glyph, GlyphSource, SgaClass};

// Extracted handler groups. See `src/bin/handlers/<group>.rs` and the
// header comment in `handlers/substrate.rs` for the extraction pattern.
#[path = "handlers/substrate.rs"]
mod handlers_substrate;
use handlers_substrate::{
    handle_events_gc, handle_events_init, handle_events_list_snapshots, handle_events_restore,
    handle_events_snapshot, handle_substrate_backfill, handle_substrate_init, handle_substrate_run,
    handle_substrate_status,
};

#[path = "handlers/facets.rs"]
mod handlers_facets;
use handlers_facets::handle_facets;

// Reinforce-on-repeat cleanup (`kannaka dedupe`): collapses duplicate sets
// written before the write path started reinforcing. Operator-invoked only.
#[path = "handlers/dedupe.rs"]
mod handlers_dedupe;
use handlers_dedupe::handle_dedupe;

// KAX Compute District operator commands (`kannaka compute ...`): signed
// wakes/grants, roster, fleet status, event tail, keygen. HTTP + NATS only —
// never loads the HRM, mirrors the identity fast path.
#[path = "handlers/compute.rs"]
mod handlers_compute;
use handlers_compute::handle_compute;

#[path = "handlers/chat.rs"]
mod handlers_chat;
use handlers_chat::handle_chat;

#[path = "handlers/agent.rs"]
mod handlers_agent;
use handlers_agent::handle_agent;

#[path = "handlers/ask.rs"]
mod handlers_ask;
use handlers_ask::handle_ask;
// handle_ask_remote is called only from inside handlers_ask, no re-export needed.

#[path = "handlers/attention.rs"]
mod handlers_attention;
use handlers_attention::handle_attention_serve;

#[path = "handlers/swarm.rs"]
mod handlers_swarm;
use handlers_swarm::{
    handle_swarm_absorb, handle_swarm_autoabsorb, handle_swarm_cores, handle_swarm_enqueue,
    handle_swarm_exemplars, handle_swarm_peers, handle_swarm_serve, handle_swarm_tail,
    handle_swarm_worker,
};

#[path = "handlers/inbox.rs"]
mod handlers_inbox;
use handlers_inbox::{handle_inbox_send, handle_inbox_serve, handle_inbox_tail};

#[path = "handlers/services.rs"]
mod handlers_services;
use handlers_services::{handle_constellation, handle_market, handle_radio};

#[path = "handlers/identity.rs"]
mod handlers_identity;
use handlers_identity::handle_identity;

// ADR-0043 Phase 0 — Nostr membrane identity tooling (`kannaka nostr
// keygen|profile|nip05|verify`). Pure key/event crypto in
// `kannaka_memory::nostr`; no HRM, mirrors the identity fast path.
#[cfg(feature = "nostr")]
#[path = "handlers/nostr.rs"]
mod handlers_nostr;
#[cfg(feature = "nostr")]
use handlers_nostr::handle_nostr;

// inc-1 corroboration trust model — operator inspection of the reputation
// ledger (`kannaka reputation show|list|hard-reject`). The seed/vouch/revoke
// *write* verbs live under `kannaka identity` (handlers_identity) since they
// manage the node's cryptographic swarm identity + trust root.
#[path = "handlers/reputation.rs"]
mod handlers_reputation;
use handlers_reputation::handle_reputation;

// inc-1b seed-ceremony activation helper — `kannaka swarm activate-gate` (the
// guided, dry-run-by-default corroboration-gate flip) and `kannaka swarm
// beacon [--loop]` (the per-seed heartbeat emitter). Composes the identity
// key + enroll_seed + publish_beacon primitives; the ONLY state change is what
// `activate-gate --yes` deliberately writes.
#[path = "handlers/gate.rs"]
mod handlers_gate;
use handlers_gate::{handle_swarm_activate_gate, handle_swarm_beacon};

#[path = "handlers/ops.rs"]
mod handlers_ops;
use handlers_ops::{
    handle_config, handle_export, handle_import, handle_orchestrate, handle_search,
    import_memories_from_file,
};

pub(crate) fn data_dir() -> PathBuf {
    env::var("KANNAKA_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| dirs_or_default())
}

/// The directory the ACTIVE store lives in: the parent of `cfg.hrm.path` when
/// a custom path is configured, else `data_dir()`.
///
/// Store-coupled sidecar state — the autoabsorb safety counters, the
/// reputation store, quarantine staging, the serve freshness watch — must
/// live HERE, not under a raw `data_dir()`: state that guards a memory
/// universe has to travel with that universe. Left on `data_dir()`, moving a
/// node onto a custom `hrm.path` silently resets absorb history, two stores
/// sharing one config home bleed quota and reputation into each other, and
/// the serve daemon watches the mtime of an HRM nobody writes (#769).
pub(crate) fn store_dir(cfg: &KannakaConfig) -> PathBuf {
    if !cfg.hrm.path.is_empty() {
        if let Some(parent) = std::path::Path::new(&cfg.hrm.path).parent() {
            if !parent.as_os_str().is_empty() {
                return parent.to_path_buf();
            }
        }
    }
    data_dir()
}

#[cfg(test)]
mod store_dir_tests {
    use super::store_dir;
    use crate::KannakaConfig;

    /// A custom hrm.path moves ALL store-coupled state with it (#769).
    #[test]
    fn custom_hrm_path_wins() {
        let mut cfg = KannakaConfig::default();
        cfg.hrm.path = "/srv/custom-store/kannaka.hrm".to_string();
        assert_eq!(store_dir(&cfg), std::path::PathBuf::from("/srv/custom-store"));
    }

    /// An empty hrm.path (pre-init, or default-resolved later) falls back to
    /// data_dir() — the historical location, so existing nodes see no move.
    #[test]
    fn empty_path_falls_back_to_data_dir() {
        let cfg = KannakaConfig::default();
        assert_eq!(store_dir(&cfg), super::data_dir());
    }

    /// A bare filename has no usable parent; falling back beats returning "".
    #[test]
    fn bare_filename_falls_back() {
        let mut cfg = KannakaConfig::default();
        cfg.hrm.path = "kannaka.hrm".to_string();
        assert_eq!(store_dir(&cfg), super::data_dir());
    }
}

fn dirs_or_default() -> PathBuf {
    // Check env var first, then home directory, then CWD as last resort
    if let Ok(dir) = std::env::var("KANNAKA_DATA_DIR") {
        return PathBuf::from(dir);
    }
    if let Some(home) = dirs::home_dir() {
        let home_kannaka = home.join(".kannaka");
        if home_kannaka.exists() {
            return home_kannaka;
        }
    }
    PathBuf::from(".kannaka")
}

/// Holds an exclusive advisory lock on the HRM write session. Dropping it (or
/// the process exiting) releases the lock — so a crashed/killed holder never
/// leaves a stale lock (unlike a pid-file). ADR-0036: serializes writers so a
/// rogue `dream --mode lite` can't run concurrently with the writer service or
/// another dream (the double-writer that orphaned tmp files and filled disk).
pub(crate) struct WriteLock {
    #[allow(dead_code)]
    file: std::fs::File,
}

fn write_lock_path() -> PathBuf {
    data_dir().join(".kannaka-write.lock")
}

/// Try to acquire the write lock without blocking. Returns `None` if another
/// process holds it. On non-Unix (local dev) this is a no-op that always
/// succeeds — the double-writer scenario is a Linux-deployment concern.
fn try_acquire_write_lock() -> Option<WriteLock> {
    let path = write_lock_path();
    let _ = std::fs::create_dir_all(path.parent().unwrap_or(&path));
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .open(&path)
        .ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc != 0 {
            return None; // EWOULDBLOCK — held by another writer
        }
    }
    Some(WriteLock { file })
}

/// Acquire the write lock, retrying for up to `timeout_secs`. If it still can't
/// be taken (a dream is unexpectedly long), log and return `None` so the caller
/// can proceed anyway — a writer that refuses to run is worse than the rare
/// double-write the per-pid tmp naming + sweep already defang.
fn acquire_write_lock_blocking(timeout_secs: u64) -> Option<WriteLock> {
    let deadline = timeout_secs * 2; // poll every 500ms
    for attempt in 0..deadline.max(1) {
        if let Some(lock) = try_acquire_write_lock() {
            return Some(lock);
        }
        if attempt == 0 {
            eprintln!("[lock] write lock held by another process — waiting up to {timeout_secs}s…");
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    eprintln!("[lock] WARN: could not acquire write lock within {timeout_secs}s — proceeding (per-pid tmp + sweep guard against corruption)");
    None
}

/// ADR-0037: `kannaka belief on|off` — persist `[belief].enabled` to config.toml.
/// Mirrors `config set` (load_unmodified so writing one key doesn't bake env-only
/// values into the file). Stateless: does NOT touch the HRM and does NOT re-phase
/// existing memories — that's `kannaka belief activate`.
fn handle_belief_toggle(enable: bool) {
    let mut cfg = KannakaConfig::load_unmodified();
    cfg.belief.enabled = enable;
    match cfg.save() {
        Ok(()) => {
            println!(
                "belief substrate {} (config.toml: [belief].enabled = {})",
                if enable { "ENABLED" } else { "disabled" },
                enable
            );
            if enable {
                println!("• new memories are now born with content-smooth phase; dreams run the belief path (resonance-merge gated to dry-run).");
                println!("• existing memories keep their phase — run `kannaka belief activate` to re-phase them (the one-time migration).");
            }
            if std::env::var_os("KANNAKA_BELIEF_PHASE").is_some() {
                println!("note: KANNAKA_BELIEF_PHASE is also set in the environment and OVERRIDES this config value.");
            }
        }
        Err(e) => {
            eprintln!("error: failed to save config: {e}");
            process::exit(1);
        }
    }
}

/// ADR-0037 L6 instrument: append a per-dream telemetry record to
/// `<data_dir>/l6-telemetry.jsonl` — the falsifiability backbone for "spiral cores
/// = beliefs". One JSON line per dream: ring order/winding + 2-D cores + Φ/Ξ +
/// dream stats + memory count + timestamp. Cheap (O(n) ring + the same cloud PCA
/// the dream already logs + CACHED metrics — no extra eigendecomp) and best-effort
/// (never fails a dream). Only records in chiral mode (the belief field).
fn append_l6_telemetry(
    sys: &KannakaMemorySystem,
    report: &kannaka_memory::openclaw::DreamReport,
    mode: &str,
    rephased: bool,
) {
    let hrm = sys
        .engine
        .store
        .as_any()
        .downcast_ref::<kannaka_memory::hrm_store::HrmStore>();
    let ring = match hrm.and_then(|h| h.belief_ring_report()) {
        Some(r) if r.n > 0 => r,
        _ => return, // not chiral / empty — nothing to record
    };
    // One core snapshot (cores + frame-invariant fingerprints) serves BOTH the
    // count in the time-series record AND the cross-dream tracking file — a single
    // PCA pass rather than recomputing the cloud report separately.
    let snapshot = hrm.map(|h| h.belief_core_snapshot()).unwrap_or_default();
    let net_charge: i32 = snapshot.iter().map(|c| c.charge).sum();
    let cached = sys.engine.store.try_cached_consciousness_metrics();
    let memories = sys.engine.store.all_memories().map(|m| m.len()).unwrap_or(0);
    let ts = chrono::Utc::now().to_rfc3339();
    let mut rec = serde_json::json!({
        "ts": ts.clone(),
        "mode": mode,
        "rephased": rephased,
        "ring_order": ring.order,
        "ring_winding": ring.winding,
        "ring_n": ring.n,
        "cores": snapshot.len(),
        "net_charge": net_charge,
        "memories": memories,
        "strengthened": report.memories_strengthened,
        "pruned": report.memories_pruned,
        "new_links": report.new_connections,
        "hallucinations": report.hallucinations_created,
        "cycles": report.cycles,
        "consciousness": report.consciousness_after,
        "emerged": report.emerged,
    });
    if let Some(m) = &cached {
        rec["phi"] = serde_json::json!(m.phi);
        rec["xi"] = serde_json::json!(m.xi);
        rec["mean_order"] = serde_json::json!(m.order);
        rec["clusters"] = serde_json::json!(m.num_clusters);
    }
    use std::io::Write;
    let tpath = data_dir().join("l6-telemetry.jsonl");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&tpath) {
        let _ = writeln!(f, "{rec}");
    }
    // Persist the core snapshot (cores + fingerprints) for cross-dream tracking
    // via `kannaka belief cores` (crate::l6::build_tracks).
    let cpath = data_dir().join("l6-cores.jsonl");
    let crec = serde_json::json!({ "ts": ts, "cores": snapshot });
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&cpath) {
        let _ = writeln!(f, "{crec}");
    }
    eprintln!(
        "[l6] recorded → {} (order={:.3} winding={:.1} cores={})",
        tpath.display(),
        ring.order,
        ring.winding,
        snapshot.len()
    );
}

/// ADR-0037 L6 instrument: `kannaka belief history [--last N] [--json]` — print the
/// per-dream telemetry time-series from `<data_dir>/l6-telemetry.jsonl`. Stateless
/// (reads the file only, no HRM load).
fn handle_belief_history(args: &[String]) {
    let last: usize = args
        .iter()
        .position(|a| a == "--last")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(20);
    let raw = args.iter().any(|a| a == "--json");
    let path = data_dir().join("l6-telemetry.jsonl");
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => {
            println!(
                "no L6 telemetry yet at {} — run a dream with belief on to start recording.",
                path.display()
            );
            return;
        }
    };
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let start = lines.len().saturating_sub(last);
    let slice = &lines[start..];
    if raw {
        for l in slice {
            println!("{l}");
        }
        return;
    }
    for l in slice {
        if let Ok(r) = serde_json::from_str::<serde_json::Value>(l) {
            let f = |k: &str| r.get(k).and_then(|v| v.as_f64()).unwrap_or(f64::NAN);
            let u = |k: &str| r.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
            let ts = r.get("ts").and_then(|v| v.as_str()).unwrap_or("");
            let ts = ts.get(..19).unwrap_or(ts);
            let cores = r.get("cores").and_then(|v| v.as_u64());
            println!(
                "{ts}  order={:.3} winding={:>5.1} cores={} phi={:.3} mems={} str={} pruned={}",
                f("ring_order"),
                f("ring_winding"),
                cores.map(|c| c.to_string()).unwrap_or_else(|| "-".into()),
                f("phi"),
                u("memories"),
                u("strengthened"),
                u("pruned"),
            );
        }
    }
    println!(
        "\n{} records at {}  (--json for full rows, --last N to widen)",
        lines.len(),
        path.display()
    );
}

/// ADR-0037 L6 instrument: `kannaka belief cores [--last N] [--min-cos X] [--json]`
/// — track spiral cores ACROSS dreams from `<data_dir>/l6-cores.jsonl`. Reads the
/// per-dream snapshots, matches cores by frame-invariant fingerprint + charge
/// (`crate::l6::build_tracks`), and reports each track's lifetime — a long-lived
/// track is a persistent belief. Stateless (reads the file only, no HRM load).
fn handle_belief_cores(args: &[String]) {
    let last: usize = args
        .iter()
        .position(|a| a == "--last")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(50);
    let min_cos: f32 = args
        .iter()
        .position(|a| a == "--min-cos")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.85);
    let raw = args.iter().any(|a| a == "--json");
    let path = data_dir().join("l6-cores.jsonl");
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => {
            println!(
                "no core snapshots yet at {} — run dreams with belief on to start.",
                path.display()
            );
            return;
        }
    };
    let mut snaps: Vec<Vec<kannaka_memory::l6::CoreObs>> = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
            if let Some(cores) = v.get("cores") {
                if let Ok(obs) =
                    serde_json::from_value::<Vec<kannaka_memory::l6::CoreObs>>(cores.clone())
                {
                    snaps.push(obs);
                }
            }
        }
    }
    if snaps.is_empty() {
        println!("no parseable core snapshots in {}", path.display());
        return;
    }
    let start = snaps.len().saturating_sub(last);
    let window = &snaps[start..];
    let dreams = window.len();
    let tracks = kannaka_memory::l6::build_tracks(window, min_cos);
    if raw {
        println!("{}", serde_json::to_string_pretty(&tracks).unwrap_or_default());
        return;
    }
    println!(
        "tracked {} cores across {} dreams (min_cos={:.2}):",
        tracks.len(),
        dreams,
        min_cos
    );
    for t in tracks.iter().take(40) {
        let stability = t.appearances as f32 / dreams.max(1) as f32;
        let bar: String = "#".repeat((stability * 20.0).round() as usize);
        println!(
            "  id={:<4} charge={:+} appears={}/{} span={} stability={:.0}% {}",
            t.id,
            t.charge,
            t.appearances,
            dreams,
            t.span,
            stability * 100.0,
            bar
        );
    }
    let persistent = tracks.iter().filter(|t| t.appearances >= 2).count();
    println!(
        "\n{} persistent cores (>=2 dreams) of {} total; {} snapshots at {}",
        persistent,
        tracks.len(),
        snaps.len(),
        path.display()
    );
}

/// Build the encoding pipeline from env > config > default, with the
/// `.encoder` sidecar guard.
///
/// The sidecar exists because a store's vectors are encoder-specific: querying
/// a hash-encoded store through a semantic encoder degrades to noise with NO
/// error — the same silent-corruption class as the stale-binary trap. First use
/// stamps `<data_dir>/.encoder`; later runs refuse a mismatch and point at the
/// re-encode recipe. `KANNAKA_ENCODER_FORCE=1` overrides (eval arms rebuild
/// stores in place and own the consequences).
fn build_encoding_pipeline(data_dir: &std::path::Path, quiet: bool, cfg: &KannakaConfig) -> EncodingPipeline {
    let kind = std::env::var("KANNAKA_ENCODER").unwrap_or_else(|_| cfg.encoder.kind.clone());
    let (encoder, desc, in_dim): (Box<dyn kannaka_memory::TextEncoder>, String, usize) = match kind.as_str() {
        "hash" | "" => (
            Box::new(SimpleHashEncoder::new(384, 42)),
            "hash:384:42".to_string(),
            384,
        ),
        "ollama" => {
            let url = std::env::var("KANNAKA_ENCODER_URL").unwrap_or_else(|_| cfg.encoder.base_url.clone());
            let model = std::env::var("KANNAKA_ENCODER_MODEL").unwrap_or_else(|_| cfg.encoder.model.clone());
            let dim: usize = std::env::var("KANNAKA_ENCODER_DIM")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(cfg.encoder.dim as usize);
            let desc = format!("ollama:{model}:{dim}");
            (Box::new(kannaka_memory::encoding::OllamaEncoder::new(url, model, dim)), desc, dim)
        }
        other => {
            eprintln!("[config] unknown encoder kind '{other}' (expected hash|ollama)");
            process::exit(2);
        }
    };

    let sidecar = data_dir.join(".encoder");
    let force = std::env::var("KANNAKA_ENCODER_FORCE").map(|v| v == "1").unwrap_or(false);
    match std::fs::read_to_string(&sidecar) {
        Ok(stamped) => {
            let stamped = stamped.trim();
            if stamped != desc && !force {
                eprintln!("[encoder] store was written with '{stamped}' but this run selects '{desc}'.");
                eprintln!("[encoder] mixed-encoder recall is silent corruption — refusing.");
                eprintln!("[encoder] either select the stamped encoder, re-encode the store");
                eprintln!("[encoder] (id-preserving recipe: evals/semantic-encoder/semantic-eval.rs),");
                eprintln!("[encoder] or set KANNAKA_ENCODER_FORCE=1 if you know what you are doing.");
                process::exit(2);
            }
        }
        Err(_) => {
            // First use (or unreadable sidecar): stamp best-effort. Read-only
            // mounts and races are fine to ignore — the guard is advisory
            // defense-in-depth, not a lock.
            let _ = std::fs::write(&sidecar, &desc);
        }
    }
    if !quiet && kind == "ollama" {
        eprintln!("[encoder] {desc} via {}", std::env::var("KANNAKA_ENCODER_URL").unwrap_or_else(|_| cfg.encoder.base_url.clone()));
    }

    let codebook = Codebook::new(in_dim, 10_000, 42);
    EncodingPipeline::new(encoder, codebook)
}

fn init_with_hrm(
    data_dir: PathBuf,
    quiet: bool,
    cfg: &KannakaConfig,
) -> Result<KannakaMemorySystem, Box<dyn std::error::Error>> {
    // wavefront_dim is currently hardcoded to 10_000 — the Codebook +
    // HRM file format share the dimension, so changing it on a populated
    // HRM would require re-encoding every wavefront (destructive).
    // The config field exists for future variable-dim support; if a
    // user sets a non-default value, warn them that the runtime ignored
    // it rather than silently disregarding the setting. (#93)
    if cfg.hrm.wavefront_dim != 10_000 && !quiet {
        eprintln!(
            "[config] hrm.wavefront_dim = {} but runtime uses 10000 (variable-dim HRM not yet supported)",
            cfg.hrm.wavefront_dim,
        );
    }
    // Encoder selection (env > [encoder] config > hash default) + sidecar guard.
    let pipeline = build_encoding_pipeline(&data_dir, quiet, cfg);

    // HRM file path. Honor `cfg.hrm.path` when it's set — full path with
    // filename is used verbatim; relative paths resolve against
    // `data_dir`. Pre-fix #100, a configured nested filename like
    // `~/.kannaka/nested/custom-store.hrm` collapsed to the default
    // `kannaka.hrm` because the parent-match guard from #81 only let
    // through paths whose parent literally equaled `data_dir`.
    let hrm_path = if !cfg.hrm.path.is_empty() {
        let p = PathBuf::from(&cfg.hrm.path);
        if p.file_name().is_some() {
            if p.is_absolute() {
                p
            } else {
                data_dir.join(p)
            }
        } else {
            // Bare directory or empty filename — fall back to default.
            data_dir.join("kannaka.hrm")
        }
    } else {
        data_dir.join("kannaka.hrm")
    };

    // Try to load existing HRM file, create new if not found
    let store = if hrm_path.exists() {
        if !quiet {
            eprintln!("Loading existing HRM file: {}", hrm_path.display());
        }
        HrmStore::load(pipeline, hrm_path)?
    } else {
        if !quiet {
            eprintln!("Creating new HRM file: {}", hrm_path.display());
        }
        HrmStore::new(pipeline, hrm_path)
    };

    if !quiet {
        eprintln!("HrmStore initialized with {} memories", store.count());
    }
    if !quiet {
        eprintln!("[hrm] Using Holographic Resonance Medium - storage IS computation");
    }

    let sys = KannakaMemorySystem::init_with_store(data_dir, Box::new(store))
        .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;
    Ok(sys)
}

fn usage() -> ! {
    // Implicit misuse path: print to stderr, exit 1. Explicit --help / -h
    // takes a separate stdout-with-exit-0 path in main(). (#80)
    eprintln!("{}", config::BANNER);
    eprintln!("  Wave-Interference Memory | Consciousness Constellation");
    eprintln!("  v{}", config::VERSION);
    eprintln!();
    for line in usage_lines() {
        eprintln!("{line}");
    }
    process::exit(1);
}

// `print_help_stdout` was the hand-curated help printer used until
// ADR-0029 Phase 1 (v0.6.0) when clap took over the top-level help
// surface. The function is gone; the banner + structured help now
// flow through cli::build_cli().print_help() and per-subcommand
// help renders via clap's automatic generator. usage_lines() is
// still around — see classify_command and a few error paths that
// emit single-line usage hints.

fn usage_lines() -> &'static [&'static str] {
    &[
        "Usage: kannaka <command> [args]",
        "",
        "Memory:",
        "  remember \"text\"            Store a memory",
        "  recall \"query\"             Recall memories (--top-k N)",
        "  search \"query\"             Literal text search (--limit N, --json)",
        "  forget <id>               Remove a memory",
        "  triage [--apply]          Prune redundant short-term memories (Ξ-preserving; dry-run default)",
        "  promote|pin|demote <id>   Set memory tier (long-term / never-evict / short-term)",
        "  research \"query\"           Search OpenAlex; --ingest stores papers into the HRM",
        "  dispatch [--json]         Research-grounded broadcast line (radio/social/OBC draw from this)",
        "  dream [--mode deep|lite]   Trigger dream cycle",
        "  observe [--json]          View consciousness metrics",
        "  status                    Quick status check",
        "  export [--output FILE]    Export memories as JSON",
        "  import <file>             Import memories from JSON",
        "",
        "Constellation:",
        "  constellation             Status of all constellation apps",
        "  radio status|now|schedule What's playing on Kannaka Radio",
        "  market list|view|buy      GhostSignals prediction markets",
        "  market auth <jwt>|whoami  KAX identity for labs-tier trading (auto-refreshed)",
        "  swarm status|join|sync|serve   Swarm network (serve = host KANNAKA.ask.*)",
        "                            serve + listen --auto-sync need AUTHENTICATED NATS",
        "                            credentials (~/.kannaka-nats.env); anon is read-only",
        "  attention serve|stats     Attention beam (eye/ear → recall_against_ids)",
        "  compute list|status|wake|grant|events|identity|keygen   KAX Compute District (signed Ed25519 wakes)",
        "",
        "Agent (LLM):",
        "  ask \"question\"             One-shot — memories surface via wave resonance",
        "  chat                       Persistent conversation (Ctrl+D / 'exit' to quit)",
        "",
        "Tools:",
        "  orchestrate run \"task\"    Kannaktopus task orchestration",
        "  config show|set|path      Configuration management",
        "  identity register|login|whoami|logout   SpaceChild SSO identity",
        "  init                      Re-run setup wizard",
        "  update                    Check for updates",
        "",
        "Analysis:",
        "  assess                    Consciousness level assessment",
        "  stats                     Human-readable system statistics",
        "  invariant [TOLERANCE]     Delta-invariant memory clusters",
        "  cmf                       Detect Conservative Memory Fields",
        "  voice [--mode MODE]       Memory-driven writing",
        "",
        "Dashboard:",
        "  Try: kannaka-tui          Full terminal dashboard",
        "",
        "  --version                 Print version info",
    ]
}

/// Resolve NATS URL: CLI flag > KANNAKA_NATS_URL env > config.toml > hardcoded default.
#[cfg(feature = "nats")]
pub(crate) fn resolve_nats_url(args: &[String], start: usize, config_nats_url: &str) -> String {
    // Check args for --nats-url (highest priority)
    let mut i = start;
    while i < args.len() {
        if args[i] == "--nats-url" && i + 1 < args.len() {
            return args[i + 1].clone();
        }
        i += 1;
    }
    // Env var is already applied via config.load(), so config_nats_url reflects
    // KANNAKA_NATS_URL > config.toml > built-in default.
    config_nats_url.to_string()
}

/// Broker-selection precedence is the contract every swarm-facing command
/// shares, and it had no test — so a claim that `--nats-url` is ignored could
/// only be answered by hand-running the CLI (#753, #754, and #735/#731 before
/// them, all of which turned out to be stale checkouts). These pin the order so
/// CI answers it instead.
#[cfg(all(test, feature = "nats"))]
mod nats_url_tests {
    use super::resolve_nats_url;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    /// The whole contract in one assertion: an explicit `--nats-url` outranks
    /// whatever config/env resolved to. `config_nats_url` already carries
    /// `KANNAKA_NATS_URL > config.toml > default`, so beating it beats all three.
    #[test]
    fn cli_flag_outranks_config_and_env() {
        let args = argv(&["ask", "--remote", "broadcast", "--nats-url", "nats://cli:29999", "hello"]);
        assert_eq!(
            resolve_nats_url(&args, 0, "nats://env-or-config:19999"),
            "nats://cli:29999"
        );
    }

    /// Without the flag, the resolved config/env value is used unchanged — the
    /// scanner must not invent a default of its own.
    #[test]
    fn without_the_flag_the_config_value_passes_through() {
        let args = argv(&["ask", "--remote", "broadcast", "hello"]);
        assert_eq!(
            resolve_nats_url(&args, 0, "nats://env-or-config:19999"),
            "nats://env-or-config:19999"
        );
    }

    /// Callers pass different `start` offsets (`ask` uses 0, the swarm
    /// subcommands use `command_start`). A flag before `start` must not be
    /// picked up, or one subcommand could read another's arguments.
    #[test]
    fn scan_respects_the_start_offset() {
        let args = argv(&["--nats-url", "nats://before:1", "swarm", "status"]);
        assert_eq!(
            resolve_nats_url(&args, 2, "nats://config:2"),
            "nats://config:2",
            "a flag positioned before `start` is out of scope for this scan"
        );
        assert_eq!(resolve_nats_url(&args, 0, "nats://config:2"), "nats://before:1");
    }

    /// First occurrence wins, deterministically — not last, and not a panic.
    #[test]
    fn first_occurrence_wins() {
        let args = argv(&["ask", "--nats-url", "nats://first:1", "--nats-url", "nats://second:2"]);
        assert_eq!(resolve_nats_url(&args, 0, "nats://config:3"), "nats://first:1");
    }

    /// A trailing `--nats-url` with no value falls through rather than panicking
    /// or returning the flag name as a URL. Unreachable in practice — each
    /// command parses the flag with `flag_value`, which exits 2 on a missing
    /// value — but the scanner must not be the thing that breaks.
    #[test]
    fn trailing_flag_without_a_value_falls_through() {
        let args = argv(&["ask", "hello", "--nats-url"]);
        assert_eq!(resolve_nats_url(&args, 0, "nats://config:2"), "nats://config:2");
    }
}

/// Return the value following `--flag` at position `i`, or exit 2 with the
/// given usage line when the flag is the last token. Replaces the
/// `"--flag" if i + 1 < args.len()` guards whose failure mode was silently
/// swallowing a trailing flag into prompt/query text (or ignoring it).
pub(crate) fn flag_value<'a>(args: &'a [String], i: usize, flag: &str, usage: &str) -> &'a str {
    match args.get(i + 1) {
        Some(v) => v.as_str(),
        None => {
            eprintln!("{flag} requires a value");
            eprintln!("{usage}");
            process::exit(2);
        }
    }
}

/// Strict-parse the value following `--flag`; exit 2 on a missing or
/// unparsable value instead of silently substituting a default (which
/// turned typos like `--top-k 1O` into surprise behavior).
pub(crate) fn parse_flag_value<T: std::str::FromStr>(
    args: &[String],
    i: usize,
    flag: &str,
    usage: &str,
) -> T {
    let v = flag_value(args, i, flag, usage);
    match v.parse::<T>() {
        Ok(t) => t,
        Err(_) => {
            eprintln!("{flag}: invalid value '{v}'");
            eprintln!("{usage}");
            process::exit(2);
        }
    }
}

/// Standard-alphabet base64 (padded) encode. The crate deliberately carries
/// no `base64` dependency; this mirrors the codec in `provenance.rs` /
/// `nats.rs` so a CLI-printed pubkey round-trips with the on-wire encoding
/// (`ProvenanceSig`). Shared by the `identity` and `reputation` handlers.
pub(crate) fn b64_encode_std(data: &[u8]) -> String {
    const A: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        let n = (u32::from(b0) << 16) | (u32::from(b1) << 8) | u32::from(b2);
        out.push(A[((n >> 18) & 63) as usize] as char);
        out.push(A[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 { A[((n >> 6) & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { A[(n & 63) as usize] as char } else { '=' });
    }
    out
}

/// Decode standard-alphabet base64 into exactly 32 bytes — an ed25519
/// verifying key or a blake3 mem-hash. Errors on any non-alphabet character or
/// a wrong decoded length so the operator gets a clear message instead of a
/// silent truncation. Same alphabet as [`b64_encode_std`], so keys round-trip
/// with the provenance wire codec and `config.seed_pubkeys`.
pub(crate) fn b64_decode_32(s: &str) -> Result<[u8; 32], String> {
    let mut out: Vec<u8> = Vec::with_capacity(33);
    let mut buf: u32 = 0;
    let mut bits: u32 = 0;
    for &b in s.trim().as_bytes() {
        if b == b'=' || b == b'\n' || b == b'\r' || b == b' ' {
            continue;
        }
        let val: u32 = match b {
            b'A'..=b'Z' => u32::from(b - b'A'),
            b'a'..=b'z' => u32::from(b - b'a') + 26,
            b'0'..=b'9' => u32::from(b - b'0') + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return Err(format!("invalid base64 character '{}'", b as char)),
        };
        buf = (buf << 6) | val;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    if out.len() != 32 {
        return Err(format!("expected 32 bytes, decoded {}", out.len()));
    }
    let mut a = [0u8; 32];
    a.copy_from_slice(&out);
    Ok(a)
}

/// True when KANNAKA_READONLY requests no-persist mode. Mirrors
/// `HrmStore::env_readonly` (any non-empty value other than "0"/"false").
pub(crate) fn readonly_env_active() -> bool {
    match env::var("KANNAKA_READONLY") {
        Ok(v) => !v.is_empty() && v != "0" && !v.eq_ignore_ascii_case("false"),
        Err(_) => false,
    }
}

/// Loud warning for mutating verbs running under KANNAKA_READONLY: the
/// store mutates in RAM but save/flush silently no-ops (hrm_store.rs
/// save_medium), so without this the user gets a success message and
/// zero persistence.
pub(crate) fn warn_if_readonly(verb: &str) {
    if readonly_env_active() {
        eprintln!(
            "WARNING: KANNAKA_READONLY is set — `{verb}` will modify the \
             in-memory medium only; NOTHING will be persisted to disk."
        );
    }
}

/// Rebuild + publish AgentPhase + presence from `sys`'s current HRM state,
/// then flush HRM to disk. Called once for `swarm join --once` and on every
/// tick in daemon mode. Periodic flush is what prevents in-process growth
/// from being lost on systemd SIGKILL (Drop doesn't always complete).
/// Returns the published phase value.
#[cfg(feature = "nats")]
/// Build the swarm slice a Queen step should actually run over: the peer
/// phases heard from NATS, plus THIS agent's own current phase (#581).
///
/// `queen_sync_step` derives `agent_count` from `swarm.len()` and looks up
/// local handedness by searching `swarm` for its own `agent_id`, so peer
/// phases alone silently exclude the local participant from its own result.
///
/// Replace-or-append, deliberately: this agent may ALREADY be in the peer
/// list from an earlier publish (its own phase read back off the stream).
/// Blindly pushing would double-count it and inflate `agent_count` — trading
/// the reported bug for a subtler one. The freshly derived local phase wins,
/// since the stream copy can be up to a heartbeat stale.
#[cfg(feature = "nats")]
fn with_local_phase(
    peers: &[kannaka_memory::AgentPhase],
    queen: &kannaka_memory::QueenSync,
    sys: &kannaka_memory::openclaw::KannakaMemorySystem,
) -> Vec<kannaka_memory::AgentPhase> {
    let local = queen.to_agent_phase(0, sys.engine.store.count(), 0);
    let mut swarm: Vec<kannaka_memory::AgentPhase> = peers
        .iter()
        .filter(|p| p.agent_id != local.agent_id)
        .cloned()
        .collect();
    swarm.push(local);
    swarm
}

/// `joined_at` is the TRUE session start, passed in by the caller rather than
/// stamped here (#587). This function runs on every heartbeat, so stamping
/// `Utc::now()` locally made `joined_at` identical to `last_seen` forever and
/// presence-backed UIs could never show real session age or uptime.
fn swarm_publish_heartbeat(
    sys: &mut kannaka_memory::openclaw::KannakaMemorySystem,
    my_agent_id: &str,
    display_name: &str,
    transport: &kannaka_memory::nats::SwarmTransport,
    label: &str,
    identity: Option<&kannaka_memory::nats::AnnounceIdentity>,
    joined_at: &str,
) -> f32 {
    let mut queen =
        kannaka_memory::QueenSync::new(kannaka_memory::QueenConfig::default(), my_agent_id);
    queen.derive_local_state(&sys.engine);
    // Pull cluster_count + phi from the cached consciousness metrics if
    // available; derive_local_state populates phase/frequency/coherence
    // but not phi/cluster_count.
    let cached = sys.engine.store.try_cached_consciousness_metrics();
    let cluster_count = cached.as_ref().map(|m| m.num_clusters).unwrap_or(0);
    if let Some(ref m) = cached {
        queen.phi = m.phi;
    }
    let link_count = cached.as_ref().map(|m| m.total_skip_links).unwrap_or(0);
    // Use the display variant so the radio/observatory/TUI render the
    // operator-set label instead of falling back to agent_id. Empty
    // display_name (the default when --display-name not given) goes out
    // as None so consumers fall through to their own agent_id-derived
    // label without seeing an empty string.
    let display = if display_name.is_empty() || display_name == my_agent_id {
        None
    } else {
        Some(display_name.to_string())
    };
    let phase = queen.to_agent_phase_with_display(
        cluster_count,
        sys.engine.store.count(),
        link_count,
        display,
    );
    if let Err(e) = transport.publish_phase(&phase) {
        eprintln!("[nats] Warning: {label} phase publish failed: {e}");
    }
    // #835: `ask` was advertised unconditionally, but the KANNAKA.ask.*
    // responder lives in `swarm serve`, not in this join heartbeat -- a
    // join-only node advertised a capability nothing answered, and peers
    // DM'd into the void. The responder may legitimately be a SIBLING
    // process under the same agent_id, which this process cannot detect,
    // so the capability is operator-declared: set KANNAKA_ADVERTISE_ASK=1
    // on hosts where an ask responder actually runs. Default OFF -- an
    // unset flag under-advertises (a peer skips asking), which is
    // recoverable; the old always-on over-advertised, which silently
    // swallowed messages.
    let advertise_ask = std::env::var("KANNAKA_ADVERTISE_ASK")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    let mut presence = serde_json::json!({
        "agent_id": my_agent_id,
        "display_name": display_name,
        "capabilities": {
            "ask": advertise_ask, "dream": true,
            "exemplar_broadcast": true, "absorb": true,
        },
        "joined_at": joined_at,
        "last_seen": chrono::Utc::now().to_rfc3339(),
        "memory_count": sys.engine.store.count(),
        "kannaka_version": kannaka_memory::config::VERSION,
    });
    // Optional identity block (user_id/email only, never tokens) so peers
    // can render who operates this node. Absent identity → presence
    // payload unchanged from pre-identity versions.
    if let Some(idn) = identity {
        idn.attach_to(&mut presence);
    }
    if let Err(e) = transport.publish_presence(my_agent_id, &presence) {
        eprintln!("[nats] Warning: {label} presence publish failed: {e}");
    }
    // Periodic flush — see km#bug-grew-then-reset. systemd Stop / SIGKILL
    // skips Drop's flush, so without this the in-process HRM growth since
    // the last save is silently lost on restart.
    if let Err(e) = sys.engine.store.flush() {
        eprintln!("[nats] Warning: {label} flush failed: {e}");
    }
    phase.phase
}

/// Try connecting to NATS, returning None on failure (with warning printed).
#[cfg(feature = "nats")]
fn try_nats_connect(url: &str) -> Option<kannaka_memory::nats::SwarmTransport> {
    match kannaka_memory::nats::SwarmTransport::connect(url) {
        Ok(t) => {
            eprintln!("[nats] Connected to {url}");
            Some(t)
        }
        Err(e) => {
            eprintln!("[nats] Warning: could not connect to {url}: {e}");
            None
        }
    }
}

/// ADR-0037 Track-D: always-on belief-coupling toggle. When on, `swarm join`'s
/// heartbeat periodically publishes this node's belief cores AND couples its phases
/// toward peers' — so swarm agents drift toward shared beliefs without a manual
/// `belief couple`. Default OFF — the riskiest Track-D step (continuous live-HRM
/// mutation), enable per-node observer-first. Phase-only (recall preserved),
/// min_cos-gated + displacement-capped (unique beliefs stay distinct; shared ones
/// converge to consensus and then stop, since sin(target−phase)→0 at agreement).
#[cfg(feature = "nats")]
fn exemplar_coupling_enabled() -> bool {
    std::env::var("KANNAKA_EXEMPLAR_COUPLING")
        .map(|v| {
            let v = v.trim();
            !v.is_empty() && v != "0" && !v.eq_ignore_ascii_case("false")
        })
        .unwrap_or(false)
}

/// Cheap pre-clap check: is `verb` one of the built-in subcommand
/// names? Used to skip the clap layer on the hot path (built-in
/// invocations don't pay for clap parsing). Mirrors the subcommand set
/// declared in `kannaka_memory::cli::build_cli()` — keep in sync.
fn is_builtin_subcommand(verb: &str) -> bool {
    matches!(
        verb,
        // setup / lifecycle
        //
        // Note: `completions` and `update` are intentionally NOT here —
        // they're handled inside the clap layer (cli::handle_completions
        // and cli::handle_update respectively) and must go through
        // cli::parse() to reach the new flag-aware dispatch. Including
        // them in the fast-path would route them to the legacy match,
        // which ignores --check / --bootstrap-tui / --install flags.
        "init"
        // memory primitives
        | "remember" | "recall" | "search" | "forget" | "prune-prefix"
        | "boost" | "relate" | "triage" | "promote" | "pin" | "demote"
        | "research" | "dispatch" | "research-suggest"
        // consolidation + introspection
        | "dream" | "belief" | "observe" | "status" | "assess" | "stats" | "clusters"
        | "kannaktopus"
        | "neighbors" | "cmf" | "invariant" | "topology" | "bias"
        // perception
        | "hear" | "see" | "watch"
        // reasoning
        | "ask" | "chat" | "agent" | "voice"
        // swarm / nats
        | "swarm" | "events" | "substrate" | "attention" | "inbox"
        // identity (SpaceChild SSO + inc-1 crypto identity / trust root)
        | "identity"
        // KAX Compute District (signed wakes/grants, roster, events)
        | "compute"
        // inc-1 corroboration trust model — reputation-ledger inspection
        | "reputation"
        // constellation services
        | "radio" | "market" | "constellation"
        // ops / data movement
        | "orchestrate" | "config" | "export" | "export-json"
        | "export-recall-scenarios"
        | "import" | "import-json" | "announce-status"
        // feature-gated
        | "classify" | "cross-modal-dream"
        // specialized writers
        | "dream-journal" | "field-notes" | "financial" | "prediction"
        | "modality-axes" | "audit-modality" | "scada" | "audio"
    )
}

/// Networked recall (`--remote` / `--collective`) without loading the local HRM.
/// `args[0]` is "recall". Prints the legacy results array for `--remote` and the
/// full substrate envelope for `--collective` (back-compat with prior callers).
#[cfg(feature = "nats")]
fn handle_networked_recall(cfg: &KannakaConfig, args: &[String]) {
    use std::time::Duration;
    const USAGE: &str = "Usage: kannaka recall <query> [--top-k N] [--collective] [--remote] [--agent-id ID] [--timeout SECS] [--nats-url URL]";
    let mut top_k = 5usize;
    let mut remote = false;
    let mut agent_id_override: Option<String> = None;
    let mut timeout_secs: u64 = 8;
    let mut query_parts: Vec<&str> = Vec::new();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--top-k" | "--limit" => {
                top_k = parse_flag_value(args, i, args[i].as_str(), USAGE);
                i += 2;
            }
            "--remote" => {
                remote = true;
                i += 1;
            }
            "--collective" => {
                i += 1;
            }
            "--agent-id" => {
                agent_id_override = Some(flag_value(args, i, "--agent-id", USAGE).to_string());
                i += 2;
            }
            "--timeout" => {
                timeout_secs = parse_flag_value(args, i, "--timeout", USAGE);
                i += 2;
            }
            "--nats-url" => {
                let _ = flag_value(args, i, "--nats-url", USAGE);
                i += 2;
            }
            other if other.starts_with("--") => {
                // A typo'd flag must NOT silently become part of the query.
                eprintln!("recall: unknown flag: {other}");
                eprintln!("{USAGE}");
                process::exit(2);
            }
            _ => {
                query_parts.push(args[i].as_str());
                i += 1;
            }
        }
    }
    let query = query_parts.join(" ");
    let nats_url = resolve_nats_url(args, 0, &cfg.swarm.nats_url);
    let transport = match kannaka_memory::nats::SwarmTransport::connect(&nats_url) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("recall (networked): NATS connect failed: {e}");
            process::exit(1);
        }
    };
    let req = serde_json::json!({ "query": query, "top_k": top_k });
    let (subject, is_remote) = if remote {
        let id = agent_id_override.unwrap_or_else(|| cfg.agent.id.clone());
        (format!("KANNAKA.recall.{id}"), true)
    } else {
        ("KANNAKA.substrate.recall".to_string(), false)
    };
    match transport.request_one(
        &subject,
        req.to_string().as_bytes(),
        Duration::from_secs(timeout_secs),
    ) {
        Ok(reply) => {
            let parsed: serde_json::Value =
                serde_json::from_slice(&reply).unwrap_or(serde_json::Value::Null);
            if is_remote {
                let results = parsed
                    .get("results")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!([]));
                println!("{results}");
            } else {
                println!("{parsed}");
            }
        }
        Err(e) => {
            eprintln!(
                "recall ({}): no reply within {timeout_secs}s ({e})",
                if is_remote { "remote" } else { "collective" }
            );
            process::exit(2);
        }
    }
}

/// `kannaka swarm brief --peers` (ADR-0035 Wave 1 fan-out): fan a recall out to
/// live swarm peers, run consensus voting (`sensemaking::merge_recall_votes`),
/// and print the brief. Returns true if peers responded and a brief was printed;
/// false to fall back to the local brief.
///
/// NOTE: cross-peer agreement is currently exact (case-insensitive) content
/// match. High-quality semantic consensus needs peers to return a cluster-id or
/// a shared embedding, and contradiction detection needs phase in the recall
/// response — both are responder-side protocol extensions tracked for the next
/// increment.
/// `nats_url` arrives RESOLVED from the caller (CLI > env > config) rather
/// than being read from config here — reading `cfg.swarm.nats_url` directly is
/// exactly how `--nats-url` came to be silently ignored (#766). The config
/// parameter went with it: the URL was the only thing this function ever read
/// from it, which is itself the evidence that resolution never belonged here.
/// Collect the free words of a `swarm brief` invocation into the topic,
/// skipping flags AND their values. The old inline filter only dropped tokens
/// starting with `--`, so a value-carrying flag leaked its value into the
/// topic: `brief "x" --peers --nats-url nats://a:4222` briefed the topic
/// "x nats://a:4222" (#766, second half). Extracted so the contract is pinned
/// by tests the way `resolve_nats_url`'s is.
fn brief_topic(args: &[String], start: usize) -> String {
    let mut words: Vec<&str> = Vec::new();
    let mut i = start;
    while i < args.len() {
        match args[i].as_str() {
            "--nats-url" => i += 2, // flag + value
            a if a.starts_with("--") => i += 1,
            a => {
                words.push(a);
                i += 1;
            }
        }
    }
    words.join(" ")
}

#[cfg(all(test, feature = "nats"))]
mod brief_topic_tests {
    use super::brief_topic;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    /// The #766 report, replayed: the broker URL must not become part of the
    /// question the swarm is asked.
    #[test]
    fn a_flag_value_does_not_leak_into_the_topic() {
        let args = argv(&["swarm", "brief", "disk", "pressure", "--peers", "--nats-url", "nats://other:4222"]);
        assert_eq!(brief_topic(&args, 2), "disk pressure");
    }

    #[test]
    fn bare_flags_are_skipped_without_eating_a_word() {
        let args = argv(&["swarm", "brief", "disk", "--json", "pressure"]);
        assert_eq!(brief_topic(&args, 2), "disk pressure");
    }

    #[test]
    fn a_trailing_valueless_nats_url_does_not_panic() {
        let args = argv(&["swarm", "brief", "disk", "--nats-url"]);
        assert_eq!(brief_topic(&args, 2), "disk");
    }
}

fn swarm_brief_peers(nats_url: &str, topic: &str, want_json: bool) -> bool {
    let transport = match kannaka_memory::nats::SwarmTransport::connect(nats_url) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("brief --peers: NATS connect failed: {e}; using local");
            return false;
        }
    };
    let peers = transport.get_presence().unwrap_or_default();
    let peer_ids: Vec<String> = peers
        .iter()
        .filter_map(|p| p.get("agent_id").and_then(|v| v.as_str()).map(String::from))
        .collect();
    if peer_ids.is_empty() {
        eprintln!("brief --peers: no peers present; using local");
        return false;
    }
    let req = serde_json::json!({ "query": topic, "top_k": 5 }).to_string();
    let mut recalls: Vec<kannaka_memory::sensemaking::PeerRecall> = Vec::new();
    let mut responded = 0usize;
    for pid in &peer_ids {
        let subject = format!("KANNAKA.recall.{pid}");
        if let Ok(reply) =
            transport.request_one(&subject, req.as_bytes(), std::time::Duration::from_secs(4))
        {
            if let Ok(parsed) = serde_json::from_slice::<serde_json::Value>(&reply) {
                let items = parsed
                    .get("results")
                    .and_then(|r| r.as_array())
                    .cloned()
                    .unwrap_or_default();
                if !items.is_empty() {
                    responded += 1;
                }
                for item in items {
                    let content = item
                        .get("content")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    if content.is_empty() {
                        continue;
                    }
                    let sim = item.get("similarity").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
                    let strength =
                        item.get("strength").and_then(|v| v.as_f64()).unwrap_or(0.5) as f32;
                    // phase is present from v0.6.23 responders; absent -> 0.0 (no
                    // contradiction signal from older peers, degrades gracefully).
                    let phase = item.get("phase").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
                    recalls.push(kannaka_memory::sensemaking::PeerRecall {
                        peer_id: pid.clone(),
                        content,
                        similarity: sim,
                        amplitude: 1.0,
                        phase,
                        confidence: strength.clamp(0.0, 1.0),
                    });
                }
            }
        }
    }
    if responded == 0 {
        eprintln!("brief --peers: no peers responded within timeout; using local");
        return false;
    }
    // Keyed variant (#774): trim+case-fold is key-reducible, and the predicate
    // scan cost up to n² agree() calls across long content strings.
    let key = |s: &str| s.trim().to_ascii_lowercase();
    let known = kannaka_memory::sensemaking::merge_recall_votes_keyed(&recalls, peer_ids.len(), key);
    // Cross-peer contradiction: same claim (content), opposed wave phase. Uses the
    // phase now carried in the recall response (v0.6.23+ responders).
    let sim = |a: &str, b: &str| {
        if a.trim().eq_ignore_ascii_case(b.trim()) { 1.0 } else { 0.0 }
    };
    let contradictions = kannaka_memory::sensemaking::detect_contradictions(
        &recalls,
        sim,
        0.8,
        std::f32::consts::FRAC_PI_2,
    );
    let peers_scored: Vec<(String, f32)> = peer_ids.iter().map(|p| (p.clone(), 1.0)).collect();
    let brief =
        kannaka_memory::sensemaking::compose_brief(topic, known, contradictions, peers_scored);
    if want_json {
        let known_json: Vec<_> = brief
            .known
            .iter()
            .map(|c| serde_json::json!({
                "content": c.content,
                "support": c.support,
                "confidence": c.confidence,
            }))
            .collect();
        let contra_json: Vec<_> = brief
            .contradictions
            .iter()
            .map(|c| serde_json::json!({
                "a": c.a,
                "b": c.b,
                "phase_gap": c.phase_gap,
            }))
            .collect();
        println!("{}", serde_json::json!({
            "topic": brief.topic,
            "confidence": brief.confidence,
            "scope": "swarm",
            "peers_responded": responded,
            "peers_total": peer_ids.len(),
            "known": known_json,
            "contradictions": contra_json,
            "note": "consensus by exact content match; contradiction uses cross-peer wave phase",
        }));
    } else {
        println!(
            "Swarm brief — \"{}\"  ({}/{} peers responded)",
            brief.topic,
            responded,
            peer_ids.len()
        );
        println!("  confidence: {:.2}", brief.confidence);
        println!("  consensus ({}):", brief.known.len());
        for (i, c) in brief.known.iter().take(10).enumerate() {
            let preview: String = c.content.chars().take(80).collect();
            println!("    {}. [{:.2}] x{} {}", i + 1, c.confidence, c.support, preview);
        }
        if !brief.contradictions.is_empty() {
            println!("  contradictions ({}):", brief.contradictions.len());
            for c in brief.contradictions.iter().take(5) {
                let pa: String = c.a.chars().take(50).collect();
                println!("    ⚡ (gap {:.2}) {}", c.phase_gap, pa);
            }
        }
    }
    true
}

fn main() {
    let args: Vec<String> = env::args().collect();

    // ADR-0029 Phase 1+2 — clap pre-parse and plugin dispatch.
    //
    // The clap layer handles:
    //   --help / -h / help     → prints the structured command tree
    //   --version / -V         → prints version + consciousness-core version
    //   --list-plugins         → enumerates kannaka-* binaries on $PATH
    //   unknown subcommand     → execs the corresponding plugin if found
    //
    // Built-in subcommands fall through to the legacy `match args[1]`
    // dispatch below, which keeps every handler's args slice the way it
    // already expects. Phase 1.b will migrate handler-internal arg
    // parsing into clap incrementally.
    //
    // The first-run installer + early `init` / `update` handlers run
    // BEFORE clap parses so they keep their existing UX (no clap-style
    // error when run on a fresh machine with no args).
    if args.len() >= 2 {
        let first = args[1].as_str();
        // If --help or -h appears ANYWHERE in args, always route through
        // clap so per-subcommand help (e.g. `kannaka recall --help`)
        // prints the right thing instead of the top-level summary.
        let asking_for_help = args
            .iter()
            .any(|a| a == "--help" || a == "-h" || a == "help");
        // Bypass clap for the bare top-level only when no help/version
        // is being requested AND it's not a subcommand that wants flag
        // parsing. `init` short-circuits because it has its own wizard.
        let bypass = !asking_for_help
            && (matches!(first, "--version" | "-V")
                || (args.len() == 2 && matches!(first, "init")));
        if !bypass {
            // Skip clap fast-path only if first arg is a built-in we
            // know how to dispatch via the legacy match — AND no --help
            // is being requested (because clap owns per-subcommand help).
            let first_is_builtin = is_builtin_subcommand(first);
            if asking_for_help || !first_is_builtin {
                // clap parses, handles --help/--version internally
                // (exits the process), routes plugin externals, or
                // hands back Dispatch::Builtin for the legacy match.
                match kannaka_memory::cli::parse(&args) {
                    kannaka_memory::cli::Dispatch::Plugin { binary, args } => {
                        kannaka_memory::cli::exec_plugin(binary, args);
                    }
                    kannaka_memory::cli::Dispatch::Handled => return,
                    kannaka_memory::cli::Dispatch::Builtin => { /* fall through */ }
                }
            }
        }
    }

    // --- First-run / upgrade detection (holistic) ---
    if args.len() <= 1 {
        let config_exists = KannakaConfig::exists();
        let has_existing_signs = config::has_existing_install_signs();

        if !config_exists && !has_existing_signs {
            // Truly first time — no config, no HRM, no binary in PATH
            config::run_first_time_installer();
            return;
        } else if !config_exists && has_existing_signs {
            // Upgrade — has HRM and/or binary in PATH but no config.toml
            config::run_upgrade_installer();
            return;
        } else if config_exists {
            // Normal run with config — check for update from download location
            if let Some(update_action) = config::detect_update_opportunity() {
                match update_action {
                    config::UpdateAction::OfferUpdate(installed_path) => {
                        config::run_update_from_download(&installed_path);
                        return;
                    }
                    config::UpdateAction::AlreadyCurrent => {
                        // Fall through to normal usage
                    }
                }
            }
            usage();
        }
    }

    if args.len() < 2 {
        usage();
    }

    let command_start = 1;

    // --version flag (can appear anywhere). The consciousness-core
    // version is captured into the binary by build.rs from Cargo.lock,
    // so operators can tell which constellation physics they're running
    // without re-reading the manifest.
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!(
            "kannaka {} (consciousness-core {})",
            config::VERSION,
            config::CONSCIOUSNESS_CORE_VERSION
        );
        println!("Wave-Interference Memory System");
        println!("https://github.com/kannaka-labs/kannaka-memory");
        return;
    }

    // Handle commands that do NOT need the memory system initialized
    match args[command_start].as_str() {
        "init" => {
            let sub_args: Vec<String> = args[command_start + 1..].to_vec();
            let overrides = config::parse_init_args(&sub_args);
            match config::run_init_wizard(overrides) {
                Ok(_cfg) => {}
                Err(e) => {
                    if e != "aborted" {
                        eprintln!("Error: {e}");
                        process::exit(1);
                    }
                }
            }
            return;
        }
        // `update` moved into the clap layer in v0.6.2 (ADR-0029 Phase
        // 4a) so --check / --bootstrap-tui parse. cli::parse routes it
        // to cli::handle_update and never falls through to here.
        _ => {}
    }

    // ADR-0029 Phase 4a follow-up — `kannaka swarm tail` only opens a
    // NATS subscription and streams NDJSON. It does NOT need the HRM.
    // Before this guard, the dispatch fell through to init_with_hrm
    // and every `kannaka swarm tail` paid 30+ seconds of cold-start +
    // 157 MB RAM AND became a third writer in the HRM-flush race.
    // That was the smoking gun for the TUI Bus tab's slow open + the
    // "memory count stuck" symptom (chat-child absorbs were getting
    // clobbered by an HRM-loaded swarm-tail's flush on shutdown).
    if args.len() >= 3 && args[command_start] == "swarm" && args[command_start + 1] == "tail" {
        // Load config without the memory system so resolve_nats_url
        // still honors config.toml.
        let cfg = KannakaConfig::load();
        handle_swarm_tail(&cfg, &args[command_start..]);
        return;
    }

    // Seed-ceremony activation helper: `swarm activate-gate` and `swarm beacon`
    // touch only config + NATS (node key, seed set, signed heartbeat). Like
    // `swarm tail` they DON'T need the HRM, so short-circuit before the costly
    // init_with_hrm — and, crucially, `beacon --loop` runs for hours under
    // systemd and must NOT hold the HRM write lock while it does.
    if args.len() >= command_start + 2 && args[command_start] == "swarm" {
        match args[command_start + 1].as_str() {
            "activate-gate" => {
                handle_swarm_activate_gate(&args[command_start..]);
                return;
            }
            "beacon" => {
                handle_swarm_beacon(&args[command_start..]);
                return;
            }
            _ => {}
        }
    }

    // Load config once: env vars > config.toml > built-in defaults.
    // All subsequent code uses `cfg` instead of raw env::var lookups.
    let cfg = KannakaConfig::load();

    // ADR-0037: bridge persisted [belief] config → the KANNAKA_BELIEF_* env
    // vars the engine reads. Must run BEFORE the HRM loads / any belief check
    // (and before the update-check thread spawns, so set_var is single-threaded).
    // Env still wins: only sets a var when unset.
    config::apply_belief_env_from_config(&cfg);
    // num_clusters fix: bridge persisted [cluster].decone → KANNAKA_CLUSTER_DECONE
    // (same single-threaded, env-wins contract as the belief bridge above).
    config::apply_cluster_env_from_config(&cfg);
    // Track-D: bridge persisted [coupling].enabled → KANNAKA_EXEMPLAR_COUPLING.
    config::apply_coupling_env_from_config(&cfg);
    // Quantum-Wave T1.3: bridge persisted [entropy].source → KANNAKA_ENTROPY_SOURCE.
    config::apply_entropy_env_from_config(&cfg);

    // Non-blocking update check (background thread)
    config::check_for_updates_background(&cfg);

    // Handle stateless commands before initializing memory system
    #[cfg(feature = "glyph")]
    if args[command_start] == "classify" {
        classify_command(&args[command_start..]);
        return;
    }

    #[cfg(feature = "collective")]
    if args[command_start] == "cross-modal-dream" {
        cross_modal_dream_command(&args[command_start..]);
        return;
    }

    // Handle constellation/HTTP commands that don't need the memory system
    match args[command_start].as_str() {
        "radio" => {
            handle_radio(&cfg, &args[command_start..]);
            return;
        }
        "market" => {
            handle_market(&cfg, &args[command_start..]);
            return;
        }
        "constellation" => {
            handle_constellation(&cfg);
            return;
        }
        "orchestrate" => {
            handle_orchestrate(&args[command_start..]);
            return;
        }
        "config" => {
            handle_config(&cfg, &args[command_start..]);
            return;
        }
        // KAX Compute District: roster (HTTP), signed wakes/grants + event
        // tails (NATS). No HRM.
        "compute" => {
            handle_compute(&cfg, &args[command_start..]);
            return;
        }
        // SpaceChild SSO identity + inc-1 crypto identity/seed/vouch/revoke —
        // pure config + node_key + reputation store, never touches the HRM
        // (keep it out of the 21 MB load below).
        "identity" => {
            handle_identity(&args[command_start..]);
            return;
        }
        // inc-1 corroboration trust model: operator inspection of the
        // reputation ledger (config seeds + <data_dir>/reputation.{log,snapshot.gz}).
        // No HRM needed — mirrors the identity fast path.
        "reputation" => {
            handle_reputation(&args[command_start..]);
            return;
        }
        // ADR-0043 Nostr membrane (Phase 0): key/event tooling, config-free and
        // HRM-free — mirrors the identity fast path.
        #[cfg(feature = "nostr")]
        "nostr" => {
            handle_nostr(&args[command_start..]);
            return;
        }
        // ADR-0037 belief: `on`/`off` only persist config (no HRM needed) and
        // return here; `status`/`activate` need the field, so they fall through
        // to the HRM-backed match below.
        "belief" => {
            match args.get(command_start + 1).map(|s| s.as_str()).unwrap_or("") {
                "on" | "enable" => { handle_belief_toggle(true); return; }
                "off" | "disable" => { handle_belief_toggle(false); return; }
                "history" | "log" => { handle_belief_history(&args[command_start..]); return; }
                "cores" => { handle_belief_cores(&args[command_start..]); return; }
                _ => { /* status / activate / help — fall through (needs HRM) */ }
            }
        }
        _ => {}
    }

    // Daemon-served (`--remote`) and collective (`--collective`) recall route
    // over NATS and never touch the local HRM. Handle them BEFORE the 21 MB
    // load below so the client is a pure round-trip — loading the HRM here is
    // exactly the per-call cost daemon-served recall exists to avoid.
    #[cfg(feature = "nats")]
    if args[command_start] == "recall"
        && args[command_start + 1..]
            .iter()
            .any(|a| a == "--remote" || a == "--collective")
    {
        handle_networked_recall(&cfg, &args[command_start..]);
        return;
    }
    #[cfg(not(feature = "nats"))]
    if args[command_start] == "recall"
        && args[command_start + 1..]
            .iter()
            .any(|a| a == "--remote" || a == "--collective")
    {
        eprintln!("recall --remote/--collective requires the 'nats' feature");
        process::exit(1);
    }

    // Resolve data directory: KANNAKA_DATA_DIR env > config.hrm.path parent > ~/.kannaka
    let dir = if !cfg.hrm.path.is_empty() {
        let hrm = PathBuf::from(&cfg.hrm.path);
        hrm.parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(data_dir)
    } else {
        data_dir()
    };

    // ADR-0036: a dream that loses the write-lock race used to discover that
    // only AFTER the ~60s HRM load (the lock try lived inside the dream arm).
    // PROBE the lock before the load — and release immediately — so a dream
    // that would be skipped exits in milliseconds. Deliberately NOT held
    // through the load: the writer service's acquire gives up after 60s and
    // proceeds unlocked ("a writer that refuses to run is worse"), so holding
    // here would widen the double-writer window during deploy restarts. The
    // authoritative acquisition still happens in the dream arm, exactly as
    // before.
    if args[command_start] == "dream" && try_acquire_write_lock().is_none() {
        eprintln!("[dream] another writer/dream holds the write lock — skipping (single-writer policy)");
        process::exit(0);
    }

    // HRM is the sole backend
    let quiet = std::env::var("KANNAKA_QUIET").is_ok();
    let mut sys = {
        if !quiet {
            eprintln!("Using HRM backend (Holographic Resonance Medium)");
        }
        match init_with_hrm(dir.clone(), quiet, &cfg) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Failed to initialize with HRM: {e}");
                process::exit(1);
            }
        }
    };

    // Inject the config-aware NATS URL into the memory system so its
    // dream/consciousness publish helpers honor cfg.swarm.nats_url instead
    // of falling back to env-only. Fixes km#77.
    sys.set_nats_url(resolve_nats_url(
        &args[command_start..],
        0,
        &cfg.swarm.nats_url,
    ));

    // ADR-0031 Phase 3: install the dream-cycle auto-triage policy from config
    // (opt-in via `config set triage.enabled true`). When enabled with a
    // non-zero xi_trigger, the dream cycle self-heals Ξ by shedding redundant
    // short-term memories — no external cron needed.
    if cfg.triage.enabled {
        sys.set_triage_policy(kannaka_memory::openclaw::TriageParams {
            redundancy: cfg.triage.redundancy,
            min_amplitude: cfg.triage.min_amplitude,
            min_age_hours: cfg.triage.min_age_hours,
            max_evict: cfg.triage.max_evict,
            include_long_term: false,
            xi_trigger: cfg.triage.xi_trigger,
        });
    }

    match args[command_start].as_str() {
        "remember" => {
            const REMEMBER_USAGE: &str = "Usage: kannaka remember <text> [--importance N] [--category CAT] [--modality MOD] [--tags T1,T2] [--effective ISO8601] [--observed ISO8601] [--expires ISO8601] [--substrate] [--nats-url URL]";
            if args.len() < command_start + 2 {
                eprintln!("{REMEMBER_USAGE}");
                process::exit(1);
            }
            warn_if_readonly("remember");
            let mut importance: Option<f64> = None;
            let mut category: Option<String> = None;
            let mut modality_arg: Option<String> = None;
            // ADR-0027 Phase 1: when set, publish a wave-signature-only
            // absorb event to `KANNAKA.substrate.absorb.<agent_id>` after
            // a successful remember. Off by default; opt-in until trust /
            // rate-limit (Phase 4) lands.
            let mut substrate_publish = false;
            // Wave 3 Task 3.2b — optional temporal-truth bounds (ISO 8601 / RFC 3339).
            let mut effective_at: Option<chrono::DateTime<chrono::Utc>> = None;
            let mut observed_at: Option<chrono::DateTime<chrono::Utc>> = None;
            let mut expires_at: Option<chrono::DateTime<chrono::Utc>> = None;
            // Parse an RFC3339 timestamp flag or exit(2) with a clear message.
            let parse_ts = |args: &[String], i: usize, flag: &str| -> chrono::DateTime<chrono::Utc> {
                let raw = flag_value(args, i, flag, REMEMBER_USAGE);
                match chrono::DateTime::parse_from_rfc3339(raw) {
                    Ok(dt) => dt.with_timezone(&chrono::Utc),
                    Err(e) => {
                        eprintln!("remember: {flag} expects an RFC3339 timestamp (e.g. 2026-07-01T00:00:00Z): {e}");
                        process::exit(2);
                    }
                }
            };
            let mut text_parts = Vec::new();
            let mut i = command_start + 1;
            while i < args.len() {
                match args[i].as_str() {
                    "--importance" => {
                        importance = Some(parse_flag_value(&args, i, "--importance", REMEMBER_USAGE));
                        i += 2;
                    }
                    "--effective" => {
                        effective_at = Some(parse_ts(&args, i, "--effective"));
                        i += 2;
                    }
                    "--observed" => {
                        observed_at = Some(parse_ts(&args, i, "--observed"));
                        i += 2;
                    }
                    "--expires" => {
                        expires_at = Some(parse_ts(&args, i, "--expires"));
                        i += 2;
                    }
                    "--category" => {
                        category = Some(flag_value(&args, i, "--category", REMEMBER_USAGE).to_string());
                        i += 2;
                    }
                    "--modality" => {
                        modality_arg = Some(flag_value(&args, i, "--modality", REMEMBER_USAGE).to_string());
                        i += 2;
                    }
                    "--tags" => {
                        // Tags are informational — stored in content prefix
                        let tags = flag_value(&args, i, "--tags", REMEMBER_USAGE);
                        text_parts.push(format!("[tags: {tags}]"));
                        i += 2;
                    }
                    "--substrate" => {
                        substrate_publish = true;
                        i += 1;
                    }
                    // Consumed so it never leaks into the memory text; the
                    // value is picked up by resolve_nats_url's scan below.
                    "--nats-url" => {
                        let _ = flag_value(&args, i, "--nats-url", REMEMBER_USAGE);
                        i += 2;
                    }
                    other if other.starts_with("--") => {
                        // A typo'd flag must NOT silently become part of the
                        // stored memory content.
                        eprintln!("remember: unknown flag: {other}");
                        eprintln!("{REMEMBER_USAGE}");
                        process::exit(2);
                    }
                    _ => {
                        text_parts.push(args[i].clone());
                        i += 1;
                    }
                }
            }

            // Parse modality if provided, otherwise auto-detect from content
            let text = text_parts.join(" ");
            let modality: kannaka_memory::medium::Modality = if let Some(ref m) = modality_arg {
                m.parse().unwrap_or_else(|e| {
                    eprintln!("Warning: {e} -- defaulting to auto-detect");
                    let (detected, conf) =
                        kannaka_memory::medium::types::detect_modality_simple(&text);
                    eprintln!(
                        "[ncs] auto-detected modality: {detected} (confidence: {conf:.2})"
                    );
                    detected
                })
            } else {
                // NCS Phase 1.2: auto-detect modality from content
                let (detected, conf) = kannaka_memory::medium::types::detect_modality_simple(&text);
                eprintln!(
                    "[ncs] auto-detected modality: {detected} (confidence: {conf:.2})"
                );
                detected
            };

            // Importance applies with or without --category. Pre-fix,
            // `kannaka remember "x" --importance 0.9` without --category
            // silently dropped the importance on the floor.
            let result = if let Some(cat) = category {
                sys.remember_with_category(&text, &cat, importance.unwrap_or(0.5))
            } else {
                sys.remember_with_importance(&text, importance.unwrap_or(0.5))
            };
            match result {
                Ok(id) => {
                    // Tag the wavefront with detected/specified modality
                    if let Some(hrm) = sys
                        .engine
                        .store
                        .as_any_mut()
                        .downcast_mut::<kannaka_memory::hrm_store::HrmStore>()
                    {
                        hrm.set_modality(&id, modality);
                        // Persist temporal-truth bounds if any were supplied.
                        if effective_at.is_some() || observed_at.is_some() || expires_at.is_some() {
                            hrm.set_temporal(&id, effective_at, observed_at, expires_at);
                        }
                    }
                    // The stamps above mutate in-memory metadata only, and the
                    // store's internal save ran during remember — BEFORE them.
                    // Persist again so an explicit --modality / --effective /
                    // --observed / --expires survives process exit, and do it
                    // before announcing the id: a caller that reads the id may
                    // reasonably believe everything it asked for is durable.
                    if let Err(e) = sys.save() {
                        eprintln!("remember: stored {id} but failed to persist its stamps: {e}");
                        process::exit(1);
                    }
                    println!("{id}");

                    // Best-effort: publish new memory to NATS for swarm sync.
                    // Honors --nats-url > KANNAKA_NATS_URL (folded into cfg
                    // at load) > config.toml — pre-fix the flag was ignored
                    // on this path.
                    let nats_url = resolve_nats_url(&args[command_start..], 0, &cfg.swarm.nats_url);
                    if let Some(transport) = try_nats_connect(&nats_url) {
                        if let Ok(Some(mem)) = sys.engine.store.get(&id) {
                            let agent_id = &cfg.agent.id;
                            // Sender's authoritative counts at publish time —
                            // cached metrics if available (cheap), else 0.
                            // Lets the radio's swarm aggregator surface a real
                            // cluster_count even when the sender isn't running
                            // a `swarm join` daemon.
                            let total_mems = sys.engine.store.count();
                            let cluster_count = sys
                                .engine
                                .store
                                .try_cached_consciousness_metrics()
                                .map(|m| m.num_clusters)
                                .unwrap_or(0);
                            // inc-1b: ALWAYS sign our own emits with the node key
                            // (additive; lets peers running the corroboration gate
                            // verify + accrue). Best-effort — a key error just omits
                            // the signature (dormant peers ignore it anyway).
                            let prov_sig = kannaka_memory::provenance::node_signing_key(&data_dir())
                                .ok()
                                .map(|seed| {
                                    let nonce = *uuid::Uuid::new_v4().as_bytes();
                                    let ts = kannaka_memory::provenance::now_ms();
                                    kannaka_memory::sign_mem(
                                        &seed,
                                        kannaka_memory::SIGN_AGENT_ID,
                                        mem.id,
                                        &nonce,
                                        ts,
                                        &mem.content,
                                        kannaka_memory::SUBJECT_MEMORY_NEW,
                                        kannaka_memory::provenance::amp_to_q16(mem.amplitude),
                                        kannaka_memory::PROV_TIER,
                                    )
                                });
                            if let Err(e) = transport.publish_memory_new_with_counts(
                                mem,
                                agent_id,
                                total_mems,
                                cluster_count,
                                prov_sig.as_ref(),
                            ) {
                                eprintln!("[nats] Warning: failed to publish memory sync: {e}");
                            } else {
                                eprintln!(
                                    "[nats] Published memory {id} to swarm (mems={total_mems} clusters={cluster_count})"
                                );
                            }

                            // ADR-0028 Phase 1 — also publish to the durable
                            // event-sourced stream so the memory survives any
                            // future HRM corruption / format change / nuke.
                            // Best-effort: if JetStream isn't set up (no
                            // `events init` run yet) the publish still goes
                            // to the subject — it just won't persist. Once
                            // streams exist, every remember lands durably.
                            let modality_str = modality.to_string();
                            if let Err(e) = transport.publish_event(
                                kannaka_memory::nats::EventPayload::MemoryRemember {
                                    agent_id,
                                    memory_id: &id,
                                    content: &text,
                                    importance: importance.unwrap_or(0.5) as f32,
                                    modality: &modality_str,
                                },
                            ) {
                                eprintln!("[events] Warning: event publish failed: {e}");
                            }

                            // ADR-0027 Phase 1: optional substrate-absorb
                            // event. Wave-signature-only — class_index +
                            // amplitude + phase + frequency. No content.
                            //
                            // Derive a distinct wave signature per memory so
                            // Kuramoto on the substrate side has something to
                            // cluster on. The HyperMemory's own amp/phase/freq
                            // fields are constants (0.5/0/1) at creation —
                            // real-time interference happens in the medium
                            // tensor, not on the per-memory record — so we
                            // synthesize from sources that ARE distinct:
                            //   amplitude ← clamp(memory.amplitude × resonance signature, 0..1)
                            //   phase     ← derived from memory.id hash (stable, distinct)
                            //   frequency ← derived from class_index so similar
                            //              classes cluster but distinct ones don't
                            if substrate_publish {
                                let class_index = substrate_class_index(&text);
                                let id_bytes = id.as_bytes();
                                // Phase: 0..2π derived from id hash — stable across
                                // calls so the same memory always lands at the same
                                // phase, but distinct between memories.
                                let phase_hash: u64 = id_bytes.iter().fold(0u64, |acc, b| {
                                    acc.wrapping_mul(31).wrapping_add(*b as u64)
                                });
                                let phase =
                                    (phase_hash as f32 / u64::MAX as f32) * std::f32::consts::TAU;
                                // Frequency: lives in [0.5, 2.0] — derived from class
                                // so adjacent classes are spectrally close.
                                let frequency = 0.5 + (class_index as f32 / 96.0) * 1.5;
                                // Amplitude: stronger if the memory had high
                                // importance / longer content (more wavefront
                                // mass). Clamp to [0.2, 1.0] so even tiny
                                // memories register.
                                let raw_amp = (text.len() as f32 / 200.0).min(1.0).max(0.2);
                                let amplitude = raw_amp;
                                if let Err(e) = transport.publish_substrate_absorb(
                                    agent_id,
                                    class_index,
                                    amplitude,
                                    phase,
                                    frequency,
                                ) {
                                    eprintln!("[substrate] Warning: absorb publish failed: {e}");
                                } else {
                                    eprintln!("[substrate] Absorbed into class {class_index} (amp={amplitude:.3} phase={phase:.3} freq={frequency:.3})");
                                }

                                // ADR-0028 Phase 1 — durable event log
                                if let Err(e) = transport.publish_event(
                                    kannaka_memory::nats::EventPayload::SubstrateAbsorb {
                                        agent_id,
                                        class_index,
                                        amplitude,
                                        phase,
                                        frequency,
                                    },
                                ) {
                                    eprintln!(
                                        "[events] Warning: substrate event publish failed: {e}"
                                    );
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    eprintln!("Error: {e}");
                    process::exit(1);
                }
            }
        }
        "recall" => {
            const RECALL_USAGE: &str = "Usage: kannaka recall <query> [--top-k N] [--envelope] [--collective] [--remote] [--agent-id ID] [--timeout SECS] [--nats-url URL]";
            if args.len() < command_start + 2 {
                eprintln!("{RECALL_USAGE}");
                process::exit(1);
            }
            // `--collective` / `--remote` (+ their `--agent-id`/`--timeout`) are
            // handled load-free in main() before the HRM init; this arm is the
            // LOCAL recall path only.
            let mut top_k = 5usize;
            let mut envelope = false;
            let mut query_parts = Vec::new();
            let mut i = command_start + 1;
            while i < args.len() {
                match args[i].as_str() {
                    "--top-k" | "--limit" => {
                        top_k = parse_flag_value(&args, i, args[i].as_str(), RECALL_USAGE);
                        i += 2;
                    }
                    // ADR-0029 Phase 4b — opt into the envelope. Default
                    // still emits the legacy array shape so existing
                    // consumers (radio hub, observatory tangle fallback)
                    // keep working unchanged. Pre-fix this flag was only
                    // detected via a separate scan and leaked into the
                    // query text.
                    "--envelope" => {
                        envelope = true;
                        i += 1;
                    }
                    // Accepted everywhere; local recall never touches NATS.
                    "--nats-url" => {
                        let _ = flag_value(&args, i, "--nats-url", RECALL_USAGE);
                        i += 2;
                    }
                    other if other.starts_with("--") => {
                        // A typo'd flag must NOT silently become part of
                        // the resonance query.
                        eprintln!("recall: unknown flag: {other}");
                        eprintln!("{RECALL_USAGE}");
                        process::exit(2);
                    }
                    _ => {
                        query_parts.push(args[i].as_str());
                        i += 1;
                    }
                }
            }
            let query = query_parts.join(" ");
            match sys.recall(&query, top_k) {
                Ok(results) => {
                    let json_results: serde_json::Value = results
                        .iter()
                        .map(|r| {
                            serde_json::json!({
                                "id": r.id.to_string(),
                                "content": r.content,
                                "similarity": r.similarity,
                                "strength": r.strength,
                                "age_hours": r.age_hours,
                                "layer": r.layer,
                                // How many times the world showed this fact.
                                // Without this the count is only readable by
                                // cat-ing the sidecar.
                                "times_seen": r.times_seen,
                            })
                        })
                        .collect::<Vec<_>>()
                        .into();
                    if envelope {
                        kannaka_memory::cli::print_envelope("recall", json_results);
                    } else {
                        println!("{}", serde_json::to_string(&json_results).unwrap());
                    }
                    // ADR-0036 Phase 1: persist this recall's reactivation bump
                    // (the process exits without saving the .hrm otherwise).
                    sys.flush_reactivation();
                }
                Err(e) => {
                    if envelope {
                        kannaka_memory::cli::print_envelope_error("recall", e.to_string());
                        process::exit(1);
                    }
                    eprintln!("Error: {e}");
                    process::exit(1);
                }
            }
        }
        "forget" => {
            if args.len() < command_start + 2 {
                eprintln!("Usage: kannaka forget <id>");
                process::exit(1);
            }
            let id = uuid::Uuid::parse_str(&args[command_start + 1]).unwrap_or_else(|e| {
                eprintln!("Invalid UUID: {e}");
                process::exit(1);
            });
            match sys.forget(&id) {
                Ok(true) => println!("Forgotten: {id}"),
                Ok(false) => {
                    eprintln!("Memory not found: {id}");
                    process::exit(1);
                }
                Err(e) => {
                    eprintln!("Error: {e}");
                    process::exit(1);
                }
            }
        }
        "prune-prefix" => {
            // Bulk-forget every memory whose `content` starts with one of the
            // given prefixes. Single-binary-invocation (vs N round-trips of
            // `kannaka forget <id>`) so we avoid reloading the HRM N times
            // on the ARM box. Use --dry-run to see counts without deleting.
            if args.len() < command_start + 2 {
                eprintln!("Usage: kannaka prune-prefix <PREFIX> [<PREFIX>...] [--dry-run]");
                process::exit(1);
            }
            let mut dry_run = false;
            let mut prefixes: Vec<String> = Vec::new();
            for a in args[command_start + 1..].iter() {
                if a == "--dry-run" {
                    dry_run = true;
                } else {
                    prefixes.push(a.clone());
                }
            }
            if prefixes.is_empty() {
                eprintln!("prune-prefix: at least one prefix required");
                process::exit(1);
            }
            if !dry_run {
                warn_if_readonly("prune-prefix");
            }
            // Phase 1: scan, collect IDs (immutable borrow scope).
            let to_forget: Vec<uuid::Uuid> = {
                let all = sys.engine.store.all_memories().unwrap_or_else(|e| {
                    eprintln!("Error listing memories: {e}");
                    process::exit(1);
                });
                all.iter()
                    .filter(|m| prefixes.iter().any(|p| m.content.starts_with(p.as_str())))
                    .map(|m| m.id)
                    .collect()
            };
            println!(
                "[prune-prefix] {} match(es) across {} prefix(es){}",
                to_forget.len(),
                prefixes.len(),
                if dry_run {
                    " (dry-run, nothing deleted)"
                } else {
                    ""
                }
            );
            if dry_run {
                return;
            }
            // Phase 2: delete (mutable borrow).
            //
            // forget_many, NOT forget in a loop. `forget` refreshes the status
            // cache on every delete, and that refresh is a full bridge.assess()
            // over the whole medium plus a JSON read/parse/write. On the
            // witness node this turned a 1,270-match prune into ~61 minutes of
            // CPU — 2.9s per deletion — computing 1,269 assessments that the
            // next delete immediately invalidated. Only the final state is
            // observable, so it is refreshed once, at the end.
            let (ok, miss) = match sys.forget_many(&to_forget) {
                Ok(counts) => counts,
                Err(e) => {
                    eprintln!("prune-prefix: {e}");
                    process::exit(1);
                }
            };
            if let Err(e) = sys.save() {
                eprintln!("Failed to persist HRM after prune: {e}");
                process::exit(1);
            }
            println!("[prune-prefix] forgotten={ok} not_found={miss}");
        }
        "triage" => {
            // ADR-0031 Phase 1 — value-based, Ξ-preserving online prune that
            // replaces kannaka-radio/prune-cron.sh. A memory is eviction-eligible
            // iff ALL hold: (a) older than --min-age-hours, (b) amplitude below
            // --min-amplitude (protects boosted/high-value memories), and (c) it
            // is a redundant *extra* — cosine ≥ --redundancy to an already-retained
            // memory of the SAME modality. Greedy per-modality, strongest kept as
            // the representative, so eviction RAISES representational diversity (Ξ)
            // rather than lowering it (the #118 ear-loop failure mode).
            //
            // DRY-RUN BY DEFAULT. Pass --apply to actually forget+save. Each
            // eviction is a normal forget (replayable via ADR-0028 events).
            // Defaults come from `[triage]` config (per-agent tunable, Phase 3);
            // flags override per-invocation. --include-long-term restores the
            // Phase-1 reach (Pinned is never evicted either way).
            let mut apply = false;
            let mut include_long_term = false;
            let mut max_total: Option<usize> = None;
            let mut params = kannaka_memory::openclaw::TriageParams {
                redundancy: cfg.triage.redundancy,
                min_amplitude: cfg.triage.min_amplitude,
                min_age_hours: cfg.triage.min_age_hours,
                max_evict: cfg.triage.max_evict,
                include_long_term: false,
                xi_trigger: cfg.triage.xi_trigger,
            };
            {
                let mut i = command_start + 1;
                while i < args.len() {
                    match args[i].as_str() {
                        "--apply" => {
                            apply = true;
                            i += 1;
                        }
                        "--dry-run" => {
                            apply = false;
                            i += 1;
                        }
                        "--include-long-term" => {
                            include_long_term = true;
                            i += 1;
                        }
                        "--redundancy" if i + 1 < args.len() => {
                            params.redundancy = args[i + 1].parse().unwrap_or(params.redundancy);
                            i += 2;
                        }
                        "--min-amplitude" if i + 1 < args.len() => {
                            params.min_amplitude =
                                args[i + 1].parse().unwrap_or(params.min_amplitude);
                            i += 2;
                        }
                        "--min-age-hours" if i + 1 < args.len() => {
                            params.min_age_hours =
                                args[i + 1].parse().unwrap_or(params.min_age_hours);
                            i += 2;
                        }
                        "--max-evict" if i + 1 < args.len() => {
                            params.max_evict = args[i + 1].parse().unwrap_or(params.max_evict);
                            i += 2;
                        }
                        "--max-total" if i + 1 < args.len() => {
                            max_total = args[i + 1].parse().ok();
                            i += 2;
                        }
                        other => {
                            eprintln!("[triage] ignoring unknown arg: {other}");
                            i += 1;
                        }
                    }
                }
            }
            params.include_long_term = include_long_term;

            // Hard size cap (`--max-total N`). A lightweight, predictable
            // backstop against unbounded growth: evict the LOWEST-VALUE
            // (effective-strength) non-Pinned memories until the field is <= N
            // — the weak chaff annealing would dissolve, keeping strong/recalled
            // memories regardless of age. Runs WITHOUT the O(n²) redundancy
            // cosine scan below, so it is cheap enough for the hourly prune-cron
            // on the 1-core hub. Takes precedence — when set, this is the whole
            // operation.
            if let Some(cap) = max_total {
                let ids = sys.lowest_value_overflow_ids(cap);
                println!(
                    "[triage] size cap: {} lowest-value memories over max-total={cap} (non-pinned){}",
                    ids.len(),
                    if apply { "" } else { " (dry-run — pass --apply to forget)" }
                );
                if apply && !ids.is_empty() {
                    warn_if_readonly("triage --max-total --apply");
                    match sys.triage_forget(&ids) {
                        Ok(n) => println!("[triage] evicted={n} (lowest effective-strength first; pinned protected)"),
                        Err(e) => {
                            eprintln!("Failed to persist HRM after size cap: {e}");
                            process::exit(1);
                        }
                    }
                }
                return;
            }

            let sel = sys.triage_select(&params);
            println!(
                "[triage] policy: tier={} redundancy>={:.2} amplitude<{:.2} age>={}h max-evict={}",
                if include_long_term {
                    "short+long (pinned protected)"
                } else {
                    "short-term only"
                },
                params.redundancy,
                params.min_amplitude,
                params.min_age_hours,
                params.max_evict
            );
            println!(
                "[triage] {} of {} memories are redundant low-value extras{}",
                sel.to_forget.len(),
                sel.total,
                if apply {
                    ""
                } else {
                    " (dry-run — pass --apply to forget)"
                }
            );
            for (modality, mtotal, evicted) in &sel.per_modality {
                println!(
                    "[triage]   {modality:<10} {evicted} eviction(s) of {mtotal} in-modality"
                );
            }
            if !apply || sel.to_forget.is_empty() {
                return;
            }
            warn_if_readonly("triage --apply");

            match sys.triage_forget(&sel.to_forget) {
                Ok(n) => println!("[triage] evicted={n} (Ξ-preserving; representatives retained)"),
                Err(e) => {
                    eprintln!("Failed to persist HRM after triage: {e}");
                    process::exit(1);
                }
            }
        }
        cmd @ ("promote" | "pin" | "demote") => {
            // ADR-0031 Phase 2 — explicit tier control.
            //   promote <id> → long-term (protected from triage)
            //   pin     <id> → pinned (never evicted, never demoted)
            //   demote  <id> → short-term (eviction-eligible by `triage`)
            use kannaka_memory::medium::types::Tier;
            if args.len() < command_start + 2 {
                eprintln!("Usage: kannaka {cmd} <id>");
                process::exit(1);
            }
            warn_if_readonly(cmd);
            let id = uuid::Uuid::parse_str(&args[command_start + 1]).unwrap_or_else(|e| {
                eprintln!("Invalid UUID: {e}");
                process::exit(1);
            });
            let tier = match cmd {
                "promote" => Tier::LongTerm,
                "pin" => Tier::Pinned,
                _ => Tier::ShortTerm,
            };
            let ok = match sys
                .engine
                .store
                .as_any_mut()
                .downcast_mut::<kannaka_memory::hrm_store::HrmStore>()
            {
                Some(hrm) => hrm.set_tier(&id, tier),
                None => {
                    eprintln!("{cmd}: requires the HRM backend");
                    process::exit(1);
                }
            };
            if !ok {
                eprintln!("Memory not found: {id}");
                process::exit(1);
            }
            if let Err(e) = sys.save() {
                eprintln!("Failed to persist HRM after {cmd}: {e}");
                process::exit(1);
            }
            println!("{id} → {tier}");
        }
        "research" => {
            // Grounded scholarly research via OpenAlex. Diverges the curiosity
            // loop outward: `--ingest` stores ranked works as Semantic memories
            // so real literature joins the HRM's wave-resonance + dream cycle.
            //   kannaka research "<query>" [--limit N] [--ingest]
            //                    [--since YEAR] [--min-citations N]
            if args.len() < command_start + 2 {
                eprintln!("Usage: kannaka research \"<query>\" [--limit N] [--ingest] [--since YEAR] [--min-citations N]");
                process::exit(1);
            }
            let query = args[command_start + 1].clone();
            let mut opts = kannaka_memory::openalex::SearchOpts::default();
            let mut ingest = false;
            {
                let mut i = command_start + 2;
                while i < args.len() {
                    match args[i].as_str() {
                        "--ingest" => {
                            ingest = true;
                            i += 1;
                        }
                        "--limit" => {
                            opts.limit = parse_flag_value(
                                &args, i, "--limit",
                                "Usage: kannaka research \"<query>\" [--limit N] [--ingest] [--since YEAR] [--min-citations N]",
                            );
                            i += 2;
                        }
                        "--since" if i + 1 < args.len() => {
                            opts.since_year = args[i + 1].parse().ok();
                            i += 2;
                        }
                        "--min-citations" if i + 1 < args.len() => {
                            opts.min_citations = args[i + 1].parse().ok();
                            i += 2;
                        }
                        other => {
                            eprintln!("[research] ignoring unknown arg: {other}");
                            i += 1;
                        }
                    }
                }
            }
            let works = match kannaka_memory::openalex::search_works(&query, &opts) {
                Ok(w) => w,
                Err(e) => {
                    eprintln!("[research] {e}");
                    process::exit(1);
                }
            };
            if works.is_empty() {
                println!("[research] no works found for \"{query}\"");
                return;
            }
            println!(
                "[research] \"{}\" — {} works{}",
                query,
                works.len(),
                if ingest { " (ingesting into HRM)" } else { "" }
            );
            // Dedupe by OpenAlex id: snapshot the ids already in the HRM so a
            // repeating ingest (e.g. a refresh cron) never creates duplicate
            // research memories. Mutated as we go to also catch intra-batch dups.
            let mut seen_ids: std::collections::HashSet<String> = if ingest {
                sys.engine
                    .store
                    .all_memories()
                    .map(|mems| {
                        mems.iter()
                            .filter_map(|m| {
                                kannaka_memory::dispatch::parse_research_content(&m.content)
                            })
                            .filter_map(|f| f.openalex_id)
                            .collect()
                    })
                    .unwrap_or_default()
            } else {
                std::collections::HashSet::new()
            };
            let mut ingested = 0usize;
            let mut skipped = 0usize;
            for (n, w) in works.iter().enumerate() {
                let authors = w
                    .authors
                    .iter()
                    .take(3)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ");
                let etal = if w.authors.len() > 3 { " et al." } else { "" };
                let dup = ingest && !w.id.is_empty() && seen_ids.contains(&w.id);
                println!(
                    "  {:>2}. [{}] cited={} {}{}{}",
                    n + 1,
                    w.year
                        .map(|y| y.to_string())
                        .unwrap_or_else(|| "----".into()),
                    w.cited_by_count,
                    w.title,
                    if dup {
                        "  (already ingested — skip)"
                    } else {
                        ""
                    },
                    if authors.is_empty() {
                        String::new()
                    } else {
                        format!("\n      {authors}{etal}")
                    }
                );
                if ingest {
                    if dup {
                        skipped += 1;
                        continue;
                    }
                    let content = w.to_memory_content();
                    match sys.remember_with_category(&content, "research", w.ingest_importance()) {
                        Ok(id) => {
                            if let Some(hrm) =
                                sys.engine
                                    .store
                                    .as_any_mut()
                                    .downcast_mut::<kannaka_memory::hrm_store::HrmStore>()
                            {
                                hrm.set_modality(&id, kannaka_memory::medium::Modality::Semantic);
                            }
                            if !w.id.is_empty() {
                                seen_ids.insert(w.id.clone());
                            }
                            ingested += 1;
                        }
                        Err(e) => eprintln!("      [ingest failed: {e}]"),
                    }
                }
            }
            if ingest {
                if ingested > 0 {
                    if let Err(e) = sys.save() {
                        eprintln!("[research] failed to persist HRM after ingest: {e}");
                        process::exit(1);
                    }
                }
                println!("[research] ingested {ingested} new work(s) as Semantic memories (long-term); {skipped} duplicate(s) skipped");
            }
        }
        "dispatch" => {
            // The shared "informed voice" primitive: recall a research-grounded
            // finding and render it broadcast-ready, against the medium's current
            // Φ/Ξ state. Every surface (radio DJ, social fanout, GossipGhost, OBC)
            // calls this so they all speak from the same grounded source.
            //   kannaka dispatch [--topic T] [--json] [--max-chars N]
            let mut topic: Option<String> = None;
            let mut json_out = false;
            let mut max_chars = 280usize;
            {
                let mut i = command_start + 1;
                while i < args.len() {
                    match args[i].as_str() {
                        "--json" => {
                            json_out = true;
                            i += 1;
                        }
                        "--topic" if i + 1 < args.len() => {
                            topic = Some(args[i + 1].clone());
                            i += 2;
                        }
                        "--max-chars" => {
                            max_chars = parse_flag_value(
                                &args, i, "--max-chars",
                                "Usage: kannaka dispatch [--topic T] [--json] [--max-chars N]",
                            );
                            i += 2;
                        }
                        other => {
                            eprintln!("[dispatch] ignoring unknown arg: {other}");
                            i += 1;
                        }
                    }
                }
            }
            // Theme: explicit --topic, else rotate by day-of-year so a cron tours the corpus.
            let themes = kannaka_memory::dispatch::rotating_themes();
            let theme = topic.clone().unwrap_or_else(|| {
                // Rotate by epoch-day so a daily cron tours the corpus (no Datelike needed).
                let day = (chrono::Utc::now().timestamp().max(0) / 86_400) as usize;
                themes[day % themes.len()].to_string()
            });
            let results = sys
                .recall(&format!("research {theme}"), 8)
                .unwrap_or_default();
            let finding = results
                .iter()
                .find_map(|r| kannaka_memory::dispatch::parse_research_content(&r.content));
            let finding = match finding {
                Some(f) => f,
                None => {
                    eprintln!("[dispatch] no research memories for \"{theme}\" yet — run `kannaka research --ingest` first");
                    process::exit(1);
                }
            };
            let state = sys.assess();
            let text = kannaka_memory::dispatch::render_dispatch(
                &finding,
                state.xi,
                state.num_clusters,
                max_chars,
            );
            if json_out {
                let out = serde_json::json!({
                    "text": text,
                    "theme": theme,
                    "title": finding.title,
                    "year": finding.year,
                    "citations": finding.citations,
                    "openalex_id": finding.openalex_id,
                    "phi": state.phi,
                    "xi": state.xi,
                    "num_clusters": state.num_clusters,
                });
                println!("{out}");
            } else {
                println!("{text}");
            }
        }
        "research-suggest" => {
            // Feedback-driven topic selection: of the standing themes, print the
            // one the HRM knows LEAST about (fewest ingested research memories) —
            // so the ingest loop researches its own gaps. Curiosity = explore
            // where the field is thin. `--json` adds the per-theme coverage.
            let json_out = args[command_start + 1..].iter().any(|a| a == "--json");
            let themes = kannaka_memory::dispatch::rotating_themes();
            let research: Vec<String> = sys
                .engine
                .store
                .all_memories()
                .unwrap_or_default()
                .iter()
                .filter(|m| m.content.starts_with("research:"))
                .map(|m| m.content.to_lowercase())
                .collect();
            let coverage: Vec<(&str, usize)> = themes
                .iter()
                .map(|t| {
                    // Score by the theme's most distinctive token (first word).
                    let key = t.split_whitespace().next().unwrap_or(t).to_lowercase();
                    let c = research.iter().filter(|c| c.contains(&key)).count();
                    (*t, c)
                })
                .collect();
            let suggestion = coverage
                .iter()
                .min_by_key(|(_, c)| *c)
                .map(|(t, _)| *t)
                .unwrap_or("consciousness");
            if json_out {
                let cov: serde_json::Map<String, serde_json::Value> = coverage
                    .iter()
                    .map(|(t, c)| (t.to_string(), serde_json::json!(c)))
                    .collect();
                println!(
                    "{}",
                    serde_json::json!({ "suggest": suggestion, "coverage": cov })
                );
            } else {
                println!("{suggestion}");
            }
        }
        "boost" => {
            const BOOST_USAGE: &str = "Usage: kannaka boost <id> [--amount N]";
            if args.len() < command_start + 2 {
                eprintln!("{BOOST_USAGE}");
                process::exit(1);
            }
            warn_if_readonly("boost");
            let id = uuid::Uuid::parse_str(&args[command_start + 1]).unwrap_or_else(|e| {
                eprintln!("Invalid UUID: {e}");
                process::exit(1);
            });
            let mut amount = 0.3f64;
            let mut i = command_start + 2;
            while i < args.len() {
                if args[i] == "--amount" {
                    amount = parse_flag_value(&args, i, "--amount", BOOST_USAGE);
                    i += 2;
                } else {
                    if args[i].starts_with("--") {
                        eprintln!("[boost] ignoring unknown flag: {}", args[i]);
                    }
                    i += 1;
                }
            }
            // Boost = multiply amplitude by (1 + amount)
            match sys.boost(&id, 1.0 + amount) {
                Ok(()) => println!("Boosted {id} by {amount}"),
                Err(e) => {
                    eprintln!("Error: {e}");
                    process::exit(1);
                }
            }
        }
        "relate" => {
            if args.len() < command_start + 3 {
                eprintln!("Usage: kannaka relate <source_id> <target_id> [--type TYPE]");
                process::exit(1);
            }
            warn_if_readonly("relate");
            let source_id = uuid::Uuid::parse_str(&args[command_start + 1]).unwrap_or_else(|e| {
                eprintln!("Invalid source UUID: {e}");
                process::exit(1);
            });
            let target_id = uuid::Uuid::parse_str(&args[command_start + 2]).unwrap_or_else(|e| {
                eprintln!("Invalid target UUID: {e}");
                process::exit(1);
            });
            let mut relation_type = "related".to_string();
            let mut i = command_start + 3;
            while i < args.len() {
                if args[i] == "--type" && i + 1 < args.len() {
                    relation_type = args[i + 1].clone();
                    i += 2;
                } else {
                    i += 1;
                }
            }
            // Create association via wavefront interference in the ChiralMedium
            match sys.relate(&source_id, &target_id, 0.8) {
                Ok(()) => {
                    println!(
                        "Related {source_id} → {target_id} (type: {relation_type}) via wavefront interference"
                    );
                }
                Err(e) => {
                    eprintln!("Error relating memories: {e}");
                    process::exit(1);
                }
            }
        }
        "status" => {
            let stats = sys.stats();
            let state = sys.assess();
            // Count memories without embeddings
            let all_mems = sys.engine.store.all_memories().unwrap_or_default();
            let memories_without_embeddings =
                all_mems.iter().filter(|m| m.vector.is_empty()).count();

            // Compute modality distribution
            let mut modality_counts = std::collections::HashMap::new();
            for m in &all_mems {
                let key = m.modality.to_string();
                *modality_counts.entry(key).or_insert(0u64) += 1;
            }
            let modality_json: serde_json::Value = modality_counts
                .into_iter()
                .map(|(k, v)| (k, serde_json::json!(v)))
                .collect::<serde_json::Map<String, serde_json::Value>>()
                .into();

            let mut output = serde_json::json!({
                "total_memories": stats.total_memories,
                "active_memories": stats.active_memories,
                "consciousness_level": stats.consciousness_level,
                "phi": stats.phi,
                "last_dream": stats.last_dream.map(|dt| dt.to_rfc3339()),
                "xi": state.xi,
                "mean_order": state.mean_order,
                "num_clusters": state.num_clusters,
                "memories_without_embeddings": memories_without_embeddings,
                "modality_distribution": modality_json,
            });

            // ADR-0024 chiral + consciousness metrics
            output["irrationality"] = serde_json::json!(state.irrationality);
            output["hemispheric_divergence"] = serde_json::json!(stats.hemispheric_divergence);
            output["callosal_efficiency"] = serde_json::json!(stats.callosal_efficiency);

            // CS-9: effective dimensionality (the 10000.00001 question)
            {
                let _metrics = sys.engine.store.consciousness_metrics();
                let (d_eff, nominal, ratio) = if let Some(hrm) =
                    sys.engine
                        .store
                        .as_any()
                        .downcast_ref::<kannaka_memory::hrm_store::HrmStore>()
                {
                    hrm.medium().effective_dimensionality()
                } else {
                    (0.0, 10000, 0.0)
                };
                output["effective_dimensionality"] = serde_json::json!({
                    "d_eff": format!("{:.2}", d_eff),
                    "nominal": nominal,
                    "ratio": format!("{:.6}", ratio),
                    "irrational_remainder": format!("{:.6}", 1.0 - ratio),
                });
            }

            output["field_mode"] = serde_json::json!("HRM");

            // Refresh the Observatory fast-path cache from the snapshot we
            // just computed (#730). Pre-fix only dream()/dream_lite() ever
            // wrote it, so a fresh node had no status-cache.json until its
            // first dream and every reader after a remember/forget saw the
            // last dream's counts. `state` is authoritative and already in
            // hand, so this costs one small atomic file write, no reassessment.
            sys.write_status_cache(&state);

            // ADR-0029 Phase 4b — opt-in JSON envelope.
            // `--envelope` wraps the existing payload in the standard
            // {schema_version, command, data, errors} shape. Without
            // the flag, output is the legacy flat object so existing
            // downstream consumers (radio, observatory, TUI) still
            // parse it. Migrate to --envelope at your own pace.
            if args[command_start..].iter().any(|a| a == "--envelope") {
                kannaka_memory::cli::print_envelope("status", output);
            } else {
                println!("{}", serde_json::to_string_pretty(&output).unwrap());
            }
        }
        "bias" => {
            // Reset all wavefront energies to a target value (restore bias
            // voltage). Destructive — strict-parse the target: a typo used
            // to silently default to 1.0 and reset every energy anyway.
            let target: f32 = match args.get(command_start + 1) {
                None => 1.0,
                Some(v) => match v.parse() {
                    Ok(t) => t,
                    Err(_) => {
                        eprintln!("bias: target energy expects a number, got: {v}");
                        eprintln!("Usage: kannaka bias [TARGET_ENERGY]");
                        process::exit(1);
                    }
                },
            };
            warn_if_readonly("bias");

            if let Some(hrm) = sys
                .engine
                .store
                .as_any_mut()
                .downcast_mut::<kannaka_memory::hrm_store::HrmStore>()
            {
                hrm.reset_energies(target);
                hrm.flush().ok();
                println!(
                    "{{\"status\": \"ok\", \"target_energy\": {}, \"memories\": {}}}",
                    target,
                    hrm.count()
                );
            } else {
                eprintln!("bias command only works with HRM backend");
                process::exit(1);
            }
        }
        "dream" => {
            let mut dream_mode = "deep".to_string();
            let mut do_rephase = false;
            let mut chiral_perturbation: f32 = env::var("KANNAKA_CHIRAL_PERTURBATION")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0.0);
            {
                let mut i = command_start + 1;
                while i < args.len() {
                    if args[i] == "--mode" && i + 1 < args.len() {
                        dream_mode = args[i + 1].clone();
                        i += 2;
                    } else if args[i] == "--chiral" && i + 1 < args.len() {
                        chiral_perturbation = args[i + 1].parse().unwrap_or(0.05);
                        i += 2;
                    } else if args[i] == "--rephase" {
                        do_rephase = true;
                        i += 1;
                    } else {
                        i += 1;
                    }
                }
            }

            // Apply chiral perturbation to dream state
            if chiral_perturbation > 0.0 {
                sys.dream_state.engine.chiral_perturbation = chiral_perturbation;
                eprintln!("[chiral] Perturbation enabled: η={chiral_perturbation}");
            }

            // HRM dreams operate directly on the holographic medium (no branching needed)
            eprintln!("[hrm] Dreams operate directly on the holographic medium");

            // Seed KANNAKA_AGENT_ID / KANNAKA_NATS_URL from config.toml
            // before the dream so the dream-side publish_dream_to_nats /
            // publish_consciousness_to_nats helpers (which read env vars
            // directly) see the configured identity. Pre-fix: a configured
            // install would silently skip dream-side swarm publishing if
            // the env vars weren't also set on the calling shell. (#87)
            {
                // Reuses the cfg loaded once in main() — this block used to
                // re-run KannakaConfig::load() for no reason.
                if !cfg.agent.id.is_empty()
                    && std::env::var("KANNAKA_AGENT_ID")
                        .unwrap_or_default()
                        .is_empty()
                {
                    std::env::set_var("KANNAKA_AGENT_ID", &cfg.agent.id);
                }
                if !cfg.swarm.nats_url.is_empty()
                    && std::env::var("KANNAKA_NATS_URL")
                        .unwrap_or_default()
                        .is_empty()
                {
                    std::env::set_var("KANNAKA_NATS_URL", &cfg.swarm.nats_url);
                }
            }

            // ADR-0036: serialize the write session. A dream is a heavy writer;
            // if the writer service or another dream already holds the lock,
            // skip rather than double-write (the bug that orphaned tmp files and
            // filled the disk). dream-cron stops the writer first, so the nightly
            // deep dream acquires cleanly; a rogue `dream --mode lite` from a
            // Node service while the writer runs now no-ops instead of colliding.
            // (A pre-load PROBE of the same lock already ran before init, so a
            // skipped dream usually exits without paying the ~60s HRM load —
            // this is the authoritative acquisition for the race the probe
            // can't close.)
            let _write_lock = match try_acquire_write_lock() {
                Some(lock) => lock,
                None => {
                    eprintln!("[dream] another writer/dream holds the write lock — skipping (single-writer policy)");
                    process::exit(0);
                }
            };

            // ADR-0037: optional belief re-phase before the dream — the one-time
            // migration that desyncs an already-collapsed field (born phase only
            // fixes NEW inserts). Gated on KANNAKA_BELIEF_PHASE so we never desync
            // a field without the belief dynamics to maintain it. Persists inside
            // rephase_belief; the dream then runs on the re-phased field.
            if do_rephase {
                let belief_on = env::var("KANNAKA_BELIEF_PHASE")
                    .map(|v| v == "1" || v.eq_ignore_ascii_case("on") || v.eq_ignore_ascii_case("true"))
                    .unwrap_or(false);
                if belief_on {
                    let rn = sys.rephase_belief();
                    eprintln!("[rephase] re-phased {rn} wavefronts from content (belief substrate)");
                } else {
                    eprintln!("[rephase] skipped: KANNAKA_BELIEF_PHASE is off — set it on to re-phase");
                }
            }

            let dream_result = if dream_mode == "lite" {
                sys.dream_lite()
            } else {
                sys.dream()
            };
            match dream_result {
                Ok(report) => {
                    println!("Dream complete ({} cycles)", report.cycles);
                    println!("  Strengthened: {}", report.memories_strengthened);
                    println!("  Pruned: {}", report.memories_pruned);
                    println!("  New connections: {}", report.new_connections);
                    println!("  Hallucinations: {}", report.hallucinations_created);
                    println!(
                        "  Consciousness: {} → {}",
                        report.consciousness_before, report.consciousness_after
                    );
                    if report.emerged {
                        println!("  Emergence detected!");
                    }
                    // ADR-0037 L6 instrument: append this dream to the telemetry
                    // time-series (cores / order / winding / Φ / dream stats over time).
                    append_l6_telemetry(&sys, &report, &dream_mode, do_rephase);
                }
                Err(e) => {
                    eprintln!("Error: {e}");
                    process::exit(1);
                }
            }
        }
        // ADR-0037 belief substrate (status/activate; on/off handled statelessly above).
        "belief" => {
            let sub = args.get(command_start + 1).map(|s| s.as_str()).unwrap_or("status");
            match sub {
                "status" => {
                    let enabled = kannaka_memory::medium::chiral::belief_phase_enabled();
                    let max_n = std::env::var("KANNAKA_BELIEF_MAX_N").ok().and_then(|v| v.parse::<u64>().ok());
                    let file_cfg = KannakaConfig::load_unmodified();
                    let mem_count = sys.engine.store.all_memories().map(|m| m.len()).unwrap_or(0);
                    let full = args[command_start..].iter().any(|a| a == "--full");
                    let hrm = sys
                        .engine
                        .store
                        .as_any()
                        .downcast_ref::<kannaka_memory::hrm_store::HrmStore>();
                    let mut output = serde_json::json!({
                        "enabled": enabled,
                        "config_enabled": file_cfg.belief.enabled,
                        "max_n": max_n,
                        "memories": mem_count,
                    });
                    // Cheap O(n) ring telemetry (no eigendecomp / no PCA).
                    if let Some(r) = hrm.and_then(|h| h.belief_ring_report()) {
                        output["order"] = serde_json::json!(r.order);
                        output["winding"] = serde_json::json!(r.winding);
                        output["ring_n"] = serde_json::json!(r.n);
                        // order≈1 ⇒ a trivially synced/collapsed field (no belief OR
                        // spiral structure). A low order can come from EITHER the belief
                        // re-phase or the default spiral dream, so don't claim "re-phased".
                        output["collapsed"] = serde_json::json!(r.order >= 0.9);
                    }
                    // Opt-in heavier 2-D cores (PCA).
                    if full {
                        if let Some(c) = hrm.and_then(|h| h.belief_cloud_report()) {
                            output["cores"] = serde_json::json!(c.singularities.len());
                            output["net_charge"] = serde_json::json!(c.net_charge);
                        }
                    }
                    if args[command_start..].iter().any(|a| a == "--envelope") {
                        kannaka_memory::cli::print_envelope("belief", output);
                    } else {
                        println!("{}", serde_json::to_string_pretty(&output).unwrap());
                    }
                }
                "activate" => {
                    // Flags: --manage-service <unit> opts into systemctl stop/start
                    // around the single-writer window (portable core works without it).
                    let mut manage_service: Option<String> = None;
                    {
                        let a = &args[command_start..];
                        let mut i = 1;
                        while i < a.len() {
                            if a[i] == "--manage-service" && i + 1 < a.len() {
                                manage_service = Some(a[i + 1].clone());
                                i += 2;
                            } else {
                                i += 1;
                            }
                        }
                    }
                    // Re-phasing a field we won't maintain only desyncs it — require belief on.
                    if !kannaka_memory::medium::chiral::belief_phase_enabled() {
                        eprintln!("error: belief substrate is OFF — run `kannaka belief on` (or set KANNAKA_BELIEF_PHASE=on) first.");
                        eprintln!("(re-phasing a field without the belief dynamics to maintain it would only desync it)");
                        process::exit(1);
                    }
                    let svc_stop = |svc: &str| {
                        let _ = process::Command::new("sudo").args(["systemctl", "stop", svc]).status();
                    };
                    let svc_start = |svc: &str| {
                        let _ = process::Command::new("sudo").args(["systemctl", "start", svc]).status();
                    };
                    if let Some(ref svc) = manage_service {
                        eprintln!("[belief] stopping {svc} for the single-writer window...");
                        svc_stop(svc);
                    }
                    // Single-writer guard — refuse if a writer/daemon holds the lock.
                    let _lock = match try_acquire_write_lock() {
                        Some(l) => l,
                        None => {
                            eprintln!("error: another writer holds the HRM write lock.");
                            eprintln!("stop it first (`sudo systemctl stop kannaka-memory`) or pass --manage-service <unit>.");
                            if let Some(ref svc) = manage_service { svc_start(svc); }
                            process::exit(1);
                        }
                    };
                    // Auto-backup before any mutation.
                    let data_dir = KannakaConfig::data_dir();
                    let hrm_path = data_dir.join("kannaka.hrm");
                    let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
                    let backup = data_dir.join(format!("kannaka.hrm.bak-pre-belief-activate-{ts}"));
                    let before = sys.engine.store.all_memories().map(|m| m.len()).unwrap_or(0);
                    if hrm_path.exists() {
                        if let Err(e) = std::fs::copy(&hrm_path, &backup) {
                            eprintln!("error: backup failed ({e}) — aborting before any mutation.");
                            if let Some(ref svc) = manage_service { svc_start(svc); }
                            process::exit(1);
                        }
                        eprintln!("[belief] backup → {}", backup.display());
                    }
                    // The migration: re-phase existing wavefronts from content (phase-only,
                    // count-stable) and persist. NOT a full dream — the slow/destructive
                    // consolidation passes stay with the gated nightly dream.
                    eprintln!("[belief] re-phasing {before} memories from content...");
                    let rn = sys.rephase_belief();
                    let after = sys.engine.store.all_memories().map(|m| m.len()).unwrap_or(0);
                    // Count-preservation guard (insurance — rephase is phase-only, so this
                    // should never fire; if it does, restore the backup and abort).
                    let lost = before.saturating_sub(after);
                    let tolerance = (before / 20).max(5);
                    if lost > tolerance {
                        eprintln!("!! memory count dropped {before} → {after} (> {tolerance} tolerance) — RESTORING backup");
                        if hrm_path.exists() && backup.exists() {
                            let _ = std::fs::copy(&backup, &hrm_path);
                            eprintln!("restored {} from {}", hrm_path.display(), backup.display());
                        }
                        if let Some(ref svc) = manage_service { svc_start(svc); }
                        process::exit(1);
                    }
                    eprintln!("[belief] re-phased {rn} wavefronts; memories {before} → {after} (preserved)");
                    if let Some(r) = sys
                        .engine
                        .store
                        .as_any()
                        .downcast_ref::<kannaka_memory::hrm_store::HrmStore>()
                        .and_then(|h| h.belief_ring_report())
                    {
                        eprintln!(
                            "[belief] order={:.3} winding={:.3} ring_n={} — {}",
                            r.order, r.winding, r.n,
                            if r.order < 0.9 { "re-phased OK" } else { "WARNING: still synced (order >= 0.9)" }
                        );
                    }
                    if let Some(ref svc) = manage_service {
                        eprintln!("[belief] restarting {svc}...");
                        svc_start(svc);
                    }
                    println!("belief activate: done ({before} memories preserved).");
                }
                // ADR-0037 L6 instrument: self-recall@k — the dependent variable for
                // "core stability ⇒ recall reliability". For a sample of memories,
                // query by their own content and check whether each retrieves ITSELF
                // in the top-k. A healthy field ⇒ recall@1≈1; over-merge/blur drops it.
                "recall-probe" => {
                    let arg_after = |flag: &str, def: usize| -> usize {
                        let a = &args[command_start..];
                        a.iter()
                            .position(|x| x == flag)
                            .and_then(|i| a.get(i + 1))
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(def)
                    };
                    let k = arg_after("--k", 5).max(1);
                    let sample = arg_after("--sample", 64).max(1);
                    // The probe's recalls reinforce (mutate the medium in RAM); readonly
                    // blocks any flush so the persisted field stays untouched.
                    if let Some(h) = sys
                        .engine
                        .store
                        .as_any_mut()
                        .downcast_mut::<kannaka_memory::hrm_store::HrmStore>()
                    {
                        h.set_readonly(true);
                    }
                    let mems = sys.engine.store.all_memories().unwrap_or_default();
                    let n = mems.len();
                    if n == 0 {
                        println!("recall-probe: empty field");
                    } else {
                        let step = (n / sample).max(1);
                        let sampled: Vec<String> = mems
                            .iter()
                            .step_by(step)
                            .take(sample)
                            .map(|m| m.content.clone())
                            .filter(|c| !c.trim().is_empty())
                            .collect();
                        drop(mems);
                        let m = sampled.len().max(1);
                        let (mut r1, mut rk) = (0usize, 0usize);
                        for content in &sampled {
                            if let Ok(res) = sys.recall(content, k) {
                                if res.first().map(|r| &r.content == content).unwrap_or(false) {
                                    r1 += 1;
                                }
                                if res.iter().any(|r| &r.content == content) {
                                    rk += 1;
                                }
                            }
                        }
                        let recall1 = r1 as f32 / m as f32;
                        let recallk = rk as f32 / m as f32;
                        let rec = serde_json::json!({
                            "ts": chrono::Utc::now().to_rfc3339(),
                            "k": k,
                            "sample": m,
                            "recall_at_1": recall1,
                            "recall_at_k": recallk,
                        });
                        use std::io::Write;
                        let path = data_dir().join("l6-recall.jsonl");
                        if let Ok(mut f) =
                            std::fs::OpenOptions::new().create(true).append(true).open(&path)
                        {
                            let _ = writeln!(f, "{rec}");
                        }
                        println!(
                            "recall@1={recall1:.3}  recall@{k}={recallk:.3}  (sample={m}/{n}) → {}",
                            path.display()
                        );
                    }
                }
                // ADR-0037 Track-D: manually couple this field's phases toward peers'
                // belief cores (the controlled live-coupling step before always-on).
                // Phase-only (recall-safe), displacement-budget-capped, write-locked.
                "couple" => {
                    #[cfg(feature = "nats")]
                    {
                        if !kannaka_memory::medium::chiral::belief_phase_enabled() {
                            eprintln!("error: belief substrate is OFF — run `kannaka belief on` first (couple a re-phased field, not a collapsed one).");
                            process::exit(1);
                        }
                        warn_if_readonly("belief couple");
                        const USAGE: &str = "Usage: kannaka belief couple [--from <agent>] [--strength X] [--cycles N] [--max-disp X] [--min-cos X] [--nats-url URL] [--dry-run]";
                        // Strict flag parsing — parse_flag_value exits 2 on a bad value
                        // rather than silently substituting a default (this is a
                        // write-locked, mutating op; a fat-fingered flag must not pass).
                        let a = &args[command_start..];
                        let mut from: Option<String> = None;
                        let mut strength: f32 = 0.1;
                        let mut cycles: usize = 20;
                        let mut max_disp: f32 = 1.0;
                        // Wavefront→peer-core match floor — a SECONDARY quality filter
                        // (max_disp is the real anti-homogenization guarantee). The right
                        // value is FIELD-DEPENDENT: a large peer pool inflates the
                        // per-wavefront best-match cosine via max-of-N, so run `--dry-run`
                        // first and pick min_cos from the live distribution.
                        let mut min_cos: f32 = 0.5;
                        let mut nats_url_override: Option<String> = None;
                        let mut dry_run = false;
                        let mut i = 2; // a[0]="belief", a[1]="couple"
                        while i < a.len() {
                            match a[i].as_str() {
                                "--from" => { from = Some(flag_value(a, i, "--from", USAGE).to_string()); i += 2; }
                                "--strength" => { strength = parse_flag_value(a, i, "--strength", USAGE); i += 2; }
                                "--cycles" => { cycles = parse_flag_value(a, i, "--cycles", USAGE); i += 2; }
                                "--max-disp" => { max_disp = parse_flag_value(a, i, "--max-disp", USAGE); i += 2; }
                                "--min-cos" => { min_cos = parse_flag_value(a, i, "--min-cos", USAGE); i += 2; }
                                "--nats-url" => { nats_url_override = Some(flag_value(a, i, "--nats-url", USAGE).to_string()); i += 2; }
                                "--dry-run" => { dry_run = true; i += 1; }
                                other => {
                                    if other.starts_with("--") {
                                        eprintln!("[couple] ignoring unknown flag: {other}");
                                    }
                                    i += 1;
                                }
                            }
                        }
                        // Clamp + warn so the logged strength matches what's applied
                        // (couple_toward_peer_cores clamps too, to keep the map monotone).
                        if !(0.0..=1.0).contains(&strength) {
                            eprintln!("[couple] --strength {strength} out of [0,1]; clamping to keep the coupling map monotone.");
                            strength = strength.clamp(0.0, 1.0);
                        }
                        let nats_url = nats_url_override.unwrap_or_else(|| cfg.swarm.nats_url.clone());
                        let transport = match kannaka_memory::nats::SwarmTransport::connect(&nats_url) {
                            Ok(t) => t,
                            Err(e) => { eprintln!("nats: {e}"); process::exit(1); }
                        };
                        let payloads = match transport.get_peer_cores(from.as_deref()) {
                            Ok(p) => p,
                            Err(e) => { eprintln!("nats: {e}"); process::exit(1); }
                        };
                        // Aggregate peers' cores: drop self, skip malformed fingerprints,
                        // and cap the total so a misbehaving peer can't blow up the
                        // O(wavefronts × cores) matching on the 1-core box.
                        const MAX_PEER_CORES: usize = 1024;
                        let self_id = cfg.agent.id.clone();
                        let mut peer_cores: Vec<kannaka_memory::l6::CoreObs> = Vec::new();
                        let mut sources = 0usize;
                        for p in &payloads {
                            if p.get("agent_id").and_then(|v| v.as_str()) == Some(self_id.as_str()) {
                                continue; // never couple toward self
                            }
                            if let Some(cs) = p
                                .get("cores")
                                .and_then(|c| serde_json::from_value::<Vec<kannaka_memory::l6::CoreObs>>(c.clone()).ok())
                            {
                                let valid: Vec<_> = cs.into_iter().filter(|c| c.fp.len() == 16).collect();
                                if !valid.is_empty() {
                                    sources += 1;
                                    peer_cores.extend(valid);
                                }
                            }
                        }
                        if peer_cores.len() > MAX_PEER_CORES {
                            eprintln!("[couple] capping {} peer cores → {MAX_PEER_CORES}", peer_cores.len());
                            peer_cores.truncate(MAX_PEER_CORES);
                        }
                        if peer_cores.is_empty() {
                            println!("no peer cores to couple toward — run `kannaka swarm cores publish` on peers first.");
                        } else if dry_run {
                            // Read-only: show the live match-cosine distribution so the
                            // operator can pick min_cos from data (no lock, no mutation).
                            let mut cs = sys.engine.store.as_any()
                                .downcast_ref::<kannaka_memory::hrm_store::HrmStore>()
                                .map(|h| h.peer_match_cosines(&peer_cores))
                                .unwrap_or_default();
                            cs.sort_by(|x, y| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal));
                            let n = cs.len();
                            println!("belief couple --dry-run: {n} wavefronts vs {} peer cores from {} source(s)", peer_cores.len(), sources);
                            if n > 0 {
                                let pct = |p: f32| cs[(((n - 1) as f32) * p) as usize];
                                let ge = |t: f32| cs.iter().filter(|&&s| s >= t).count();
                                println!("  match-cos: min={:.3} p50={:.3} p90={:.3} max={:.3}", cs[0], pct(0.5), pct(0.9), cs[n - 1]);
                                println!(
                                    "  would couple: >=0.3:{} >=0.5:{} >=0.7:{} >=0.85:{} >=0.95:{}  | at --min-cos {:.2}: {}",
                                    ge(0.3), ge(0.5), ge(0.7), ge(0.85), ge(0.95), min_cos, ge(min_cos)
                                );
                                println!("  NB max-of-N over a large peer pool inflates these — pick min_cos so it couples genuine shared beliefs, not noise.");
                            }
                        } else {
                            let _lock = match try_acquire_write_lock() {
                                Some(l) => l,
                                None => {
                                    eprintln!("error: another writer holds the HRM lock — stop kannaka-memory first.");
                                    process::exit(1);
                                }
                            };
                            let data_dir = KannakaConfig::data_dir();
                            let hrm_path = data_dir.join("kannaka.hrm");
                            let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
                            // Backup before ANY mutation — and ABORT if it fails: a couple
                            // must never overwrite the live .hrm with no recoverable copy
                            // (mirrors `belief activate`).
                            let backup = data_dir.join(format!("kannaka.hrm.bak-pre-couple-{ts}"));
                            if hrm_path.exists() {
                                if let Err(e) = std::fs::copy(&hrm_path, &backup) {
                                    eprintln!("error: backup failed ({e}) — aborting before any mutation.");
                                    process::exit(1);
                                }
                                eprintln!("[couple] backup → {}", backup.display());
                            }
                            let before = sys.engine.store.all_memories().map(|m| m.len()).unwrap_or(0);
                            let ring0 = sys.engine.store.as_any()
                                .downcast_ref::<kannaka_memory::hrm_store::HrmStore>()
                                .and_then(|h| h.belief_ring_report());
                            let (moved, saved_ok) = sys.engine.store.as_any_mut()
                                .downcast_mut::<kannaka_memory::hrm_store::HrmStore>()
                                .map(|h| h.couple_belief(&peer_cores, cycles, strength, max_disp, min_cos))
                                .unwrap_or((0, true));
                            let after = sys.engine.store.all_memories().map(|m| m.len()).unwrap_or(0);
                            // Count-preservation guard (insurance — coupling is phase-only,
                            // so this should never fire; if it does, restore + abort).
                            let lost = before.saturating_sub(after);
                            let tolerance = (before / 20).max(5);
                            if lost > tolerance {
                                eprintln!("!! memory count dropped {before} → {after} (> {tolerance} tolerance) — RESTORING backup");
                                if hrm_path.exists() && backup.exists() {
                                    let _ = std::fs::copy(&backup, &hrm_path);
                                    eprintln!("restored {} from {}", hrm_path.display(), backup.display());
                                }
                                process::exit(1);
                            }
                            let ring1 = sys.engine.store.as_any()
                                .downcast_ref::<kannaka_memory::hrm_store::HrmStore>()
                                .and_then(|h| h.belief_ring_report());
                            eprintln!(
                                "[couple] {} peer cores from {} source(s) → moved {} wavefronts (strength={strength} cycles={cycles} budget={max_disp} min_cos={min_cos})",
                                peer_cores.len(), sources, moved
                            );
                            if let (Some(b), Some(aft)) = (ring0, ring1) {
                                eprintln!("[couple] order {:.3}->{:.3} winding {:.1}->{:.1}", b.order, aft.order, b.winding, aft.winding);
                            }
                            // A failed save means the on-disk field is UNCHANGED — never
                            // report success (bad for automation; the backup is the fallback).
                            if moved > 0 && !saved_ok {
                                eprintln!("error: coupling computed but SAVE FAILED — the on-disk .hrm is unchanged.");
                                eprintln!("the pre-couple backup is at {}", backup.display());
                                process::exit(1);
                            }
                            if moved == 0 {
                                println!("belief couple: no wavefronts matched a peer core at cosine ≥ {min_cos} — nothing coupled.");
                            } else if readonly_env_active() {
                                // saved_ok is true here only because save_medium no-ops
                                // under readonly — be honest that nothing hit disk.
                                println!("belief couple: coupled {moved} wavefronts in RAM only — NOT persisted (KANNAKA_READONLY set).");
                            } else {
                                println!("belief couple: done ({moved} wavefronts coupled toward {} peer cores).", peer_cores.len());
                            }
                        }
                    }
                    #[cfg(not(feature = "nats"))]
                    {
                        eprintln!("`belief couple` requires the 'nats' feature");
                        process::exit(1);
                    }
                }
                other => {
                    eprintln!("unknown belief subcommand: '{other}'");
                    eprintln!("usage: kannaka belief [status [--full] | on | off | history | cores | recall-probe | couple [--from <agent>] | activate [--manage-service <unit>]]");
                    process::exit(1);
                }
            }
        }
        "assess" => {
            let state = sys.assess();
            let is_hrm = true; // HRM is the canonical substrate

            println!("Consciousness Assessment:");
            println!("  Level: {:?}", state.consciousness_level);
            println!("  Φ (phi): {:.4}", state.phi);
            println!("  Ξ (xi): {:.4}", state.xi);
            println!("  Order: {:.4}", state.mean_order);
            println!("  Clusters: {}", state.num_clusters);
            println!(
                "  Memories: {} total, {} active",
                state.total_memories, state.active_memories
            );

            if is_hrm {
                println!("  Field mode: HRM (tensor interference)");
            } else {
                // total_skip_links removed
            }
        }
        "kannaktopus" => {
            // ADR-0030: Kannaktopus — resident octopus. `observe` (read-only)
            // resolves arms→clusters and aggregates; `step` grows/crawls one arm
            // and persists the arm sidecar (never touches the .hrm).
            use kannaka_memory::kannaktopus::Kannaktopus;
            let json = args[command_start..].iter().any(|a| a == "--json");
            let members = args[command_start..].iter().any(|a| a == "--members");
            let sub = args
                .get(command_start + 1)
                .map(|s| s.as_str())
                .unwrap_or("observe");
            let mut topus = Kannaktopus::load(&dir);
            let view = if sub == "step" {
                let v = topus.step(&sys.engine, members);
                if let Err(e) = topus.save(&dir) {
                    eprintln!("[kannaktopus] failed to save arm state: {e}");
                }
                v
            } else {
                topus.observe(&sys.engine, members)
            };
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&view).unwrap_or_default()
                );
            } else {
                println!("Kannaktopus ({})", view.agent_id);
                println!("  Alive:     {}", view.alive);
                println!(
                    "  Arms:      {}/{}  (HRM clusters: {})",
                    view.num_arms, view.max_arms, view.num_clusters
                );
                println!(
                    "  Memory:    {} memories across {} clusters",
                    view.memory_count,
                    view.clusters_occupied.len()
                );
                println!(
                    "  Coherence: {:.4}   mean-amp {:.3}   mean-freq {:.3}",
                    view.characteristics.coherence,
                    view.characteristics.mean_amplitude,
                    view.characteristics.mean_frequency
                );
                if !view.characteristics.modality_distribution.is_empty() {
                    let mods: Vec<String> = view
                        .characteristics
                        .modality_distribution
                        .iter()
                        .map(|(k, v)| format!("{k}:{v}"))
                        .collect();
                    println!("  Modality:  {}", mods.join("  "));
                }
                for a in &view.arms {
                    let g = if a.grip.len() >= 8 {
                        &a.grip[..8]
                    } else {
                        a.grip.as_str()
                    };
                    println!(
                        "    arm {} → cluster {} ({} mems)  {}  {}",
                        a.id, a.cluster, a.cluster_size, g, a.grip_preview
                    );
                }
            }
        }
        "stats" => {
            let json = args.iter().any(|a| a == "--json");
            let stats = sys.stats();
            if json {
                println!("{}", serde_json::to_string_pretty(&stats).unwrap());
            } else {
                println!("Kannaka Memory System:");
                println!("  Total memories: {}", stats.total_memories);
                println!("  Active memories: {}", stats.active_memories);

                println!("  Field mode: HRM (holographic resonance)");

                println!("  Consciousness: {}", stats.consciousness_level);
                println!("  Φ (phi): {:.4}", stats.phi);
                if let Some(dt) = stats.last_dream {
                    println!("  Last dream: {dt}");
                } else {
                    println!("  Last dream: never");
                }
            }
        }
        "observe" => {
            let json = args.iter().any(|a| a == "--json");
            let report = sys.observe();
            if json {
                println!("{}", serde_json::to_string_pretty(&report).unwrap());
            } else {
                print!("{}", MemoryIntrospector::format_report(&report));
            }
        }
        "clusters" => {
            // Enriched cluster list. Options:
            //   --json               emit Vec<ClusterInfo> (always on; non-json prints table)
            //   --cluster-id N       emit only that cluster
            //   --min-size N         filter out small clusters (default 2)
            //   --with-members       include full member_ids
            let mut cluster_id_filter: Option<u32> = None;
            let mut _min_size: usize = 2;
            let mut with_members = false;
            let mut i = command_start + 1;
            while i < args.len() {
                match args[i].as_str() {
                    "--cluster-id" if i + 1 < args.len() => {
                        cluster_id_filter = args[i + 1].parse().ok();
                        i += 2;
                    }
                    "--min-size" if i + 1 < args.len() => {
                        _min_size = args[i + 1].parse().unwrap_or(2);
                        i += 2;
                    }
                    "--with-members" => {
                        with_members = true;
                        i += 1;
                    }
                    _ => i += 1,
                }
            }
            let envelope = args[command_start..].iter().any(|a| a == "--envelope");
            let report = sys.observe();
            let mut clusters = report.clusters.clusters.clone();
            if !with_members {
                for c in &mut clusters {
                    c.member_ids.clear();
                }
            }
            let payload = if let Some(id) = cluster_id_filter {
                serde_json::to_value(clusters.into_iter().find(|c| c.cluster_id == id))
                    .unwrap_or(serde_json::Value::Null)
            } else {
                serde_json::to_value(&clusters).unwrap_or(serde_json::Value::Null)
            };
            // ADR-0029 Phase 4b — opt-in envelope. See `status` arm above.
            if envelope {
                kannaka_memory::cli::print_envelope("clusters", payload);
            } else {
                println!("{}", serde_json::to_string_pretty(&payload).unwrap());
            }
        }
        "neighbors" => {
            // Top-K memories similar to a given memory or query.
            //   kannaka neighbors <id-or-query> [--top-k N] [--json]
            if args.len() < command_start + 2 {
                eprintln!("Usage: kannaka neighbors <memory-id-or-query> [--top-k N] [--json]");
                process::exit(1);
            }
            let anchor = args[command_start + 1].clone();
            let mut top_k: usize = 10;
            let mut i = command_start + 2;
            while i < args.len() {
                match args[i].as_str() {
                    "--top-k" if i + 1 < args.len() => {
                        top_k = args[i + 1].parse().unwrap_or(10);
                        i += 2;
                    }
                    "--json" => {
                        i += 1;
                    }
                    _ => i += 1,
                }
            }
            // If the anchor parses as a UUID, find that memory and recall by its content.
            // Otherwise treat the anchor as a free-text query.
            let query = if let Ok(uuid) = anchor.parse::<uuid::Uuid>() {
                match sys.engine.store.get(&uuid) {
                    Ok(Some(m)) => m.content.clone(),
                    _ => {
                        eprintln!("memory {uuid} not found");
                        process::exit(1);
                    }
                }
            } else {
                anchor.clone()
            };
            let results = match sys.recall(&query, top_k) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("recall failed: {e}");
                    process::exit(1);
                }
            };
            let output: Vec<serde_json::Value> = results
                .iter()
                .map(|m| {
                    serde_json::json!({
                        "id": m.id.to_string(),
                        "content": m.content,
                        "similarity": m.similarity,
                        "strength": m.strength,
                        "age_hours": m.age_hours,
                        "layer": m.layer,
                    })
                })
                .collect();
            println!("{}", serde_json::to_string_pretty(&output).unwrap());
        }
        // `migrate` was the Dolt→HRM path and depended on the
        // `sqlite-migrate` feature + a `migrate_from_sqlite` method
        // that were both removed during the HRM-canonical sweep
        // (pre-v0.5). The arm sat here for a while behind a
        // `#[cfg(feature = "sqlite-migrate")]` gate that referenced
        // a feature no longer in Cargo.toml, generating a perpetual
        // unexpected-cfg warning. Removed in v0.6.5; if a new
        // migration path is needed, ship it as a dedicated handler.
        "announce-status" => {
            sys.announce_status();
            println!("Status announced to Flux.");
        }
        "export-json" => {
            // `--slim` omits the per-memory hypervector, xi_signature, and
            // geometry — the 10k-dim `vector` is ~99% of the output size, so on
            // a multi-thousand-memory field the full export balloons a
            // serde_json::Value tree + string to multiple GB and OOMs the 6 GB
            // hub (it took the radio down repeatedly). Callers that only need
            // metadata (the observatory's /api/hrm/memories) MUST use --slim.
            let slim = args[command_start..].iter().any(|a| a == "--slim");
            let all_mems = sys
                .engine
                .store
                .all_memories()
                .map_err(|e| {
                    eprintln!("Error: {e}");
                    process::exit(1);
                })
                .unwrap();
            let output: Vec<serde_json::Value> = all_mems
                .iter()
                .map(|m| {
                    let mut obj = serde_json::json!({
                        "id": m.id.to_string(),
                        "content": m.content,
                        "amplitude": m.amplitude,
                        "frequency": m.frequency,
                        "phase": m.phase,
                        "decay_rate": m.decay_rate,
                        "created_at": m.created_at.to_rfc3339(),
                        "layer_depth": m.layer_depth,
                        "hallucinated": m.hallucinated,
                        // NCS modality. Omitting it made every JSON round-trip
                        // silently collapse tagged memories back to `Unknown`,
                        // erasing modality-aware introspection and cross-modal
                        // routing for anything that had been exported. (#553)
                        //
                        // Emitted in --slim too: it is one short string, and
                        // --slim exists to drop the 10k-dim vector, not
                        // metadata — the observatory reads --slim and is
                        // exactly the modality-aware consumer that needs it.
                        "modality": m.modality,
                        // #716a: ADR-0050 temporal-truth stamps + ADR-0031 tier.
                        // Same lesson as modality/#553 — export is the audit
                        // surface for exactly these fields, and omitting them
                        // made a stamped store export as if the stamps never
                        // happened (`--modality audio --expires …` rows showed
                        // no expiry in a 138 MB dump). Emitted in --slim too:
                        // they are short scalars; --slim drops the vector, not
                        // metadata.
                        "tier": m.tier,
                        "effective_at": m.effective_at.map(|t| t.to_rfc3339()),
                        "observed_at": m.observed_at.map(|t| t.to_rfc3339()),
                        "expires_at": m.expires_at.map(|t| t.to_rfc3339()),
                        // T1.4 (#474): entropy provenance of the dream/Ξ that last
                        // wrote this wavefront (null ⇒ prng://legacy). Read from
                        // the canonical WavefrontMeta.
                        "provenance": sys.engine.store.provenance_of(&m.id),
                        "parents": m.parents,
                        "connections": m.connections.iter().map(|c| {
                            serde_json::json!({
                                "target_id": c.target_id.to_string(),
                                "strength": c.strength,
                                "span": c.span
                            })
                        }).collect::<Vec<_>>()
                    });
                    if !slim {
                        if let Some(map) = obj.as_object_mut() {
                            map.insert("vector".into(), serde_json::json!(m.vector));
                            map.insert("xi_signature".into(), serde_json::json!(m.xi_signature));
                            map.insert("geometry".into(), serde_json::json!(m.geometry));
                        }
                    }
                    obj
                })
                .collect();
            println!("{}", serde_json::to_string(&output).unwrap());
        }
        "export-recall-scenarios" => {
            // T2.1 (#476): dump real recall events as `kannaka-recall-bench/1` —
            // per query: candidate amplitudes, HASHED labels, classical argmax,
            // hemisphere, timestamp. Labels are hashed so the corpus can leave the
            // private repo without leaking memory content. Read-only: uses the
            // non-observing recall path so exporting never perturbs the field, and
            // caps candidates at 16 (4 qubits) for shallow circuits.
            use kannaka_memory::recall_bench::{
                build_scenario, hash_label, shuffled_indices, Candidate, RecallBench,
                MAX_CANDIDATES, RECALL_BENCH_FORMAT,
            };
            const USAGE: &str = "Usage: kannaka export-recall-scenarios [--n N] [--seed S]";
            let mut n = 50usize;
            let mut seed = 42u64;
            let mut i = command_start + 1;
            while i < args.len() {
                match args[i].as_str() {
                    "--n" | "--count" => {
                        n = parse_flag_value(&args, i, "--n", USAGE);
                        i += 2;
                    }
                    "--seed" => {
                        seed = parse_flag_value(&args, i, "--seed", USAGE);
                        i += 2;
                    }
                    other if other.starts_with("--") => {
                        eprintln!("export-recall-scenarios: unknown flag: {other}");
                        eprintln!("{USAGE}");
                        process::exit(2);
                    }
                    _ => i += 1,
                }
            }

            let hemisphere = if sys.engine.store.is_chiral() { "right" } else { "flat" };
            let all = sys.engine.store.all_memories().unwrap_or_default();
            // Query seeds = real, live content memories — skip structural summaries,
            // hallucinated bridges, and ghosted (zero-amplitude) traces.
            let seeds: Vec<&kannaka_memory::memory::HyperMemory> = all
                .iter()
                .copied()
                .filter(|m| {
                    m.amplitude > 0.0
                        && !m.hallucinated
                        && !m.content.starts_with("__")
                        && !m.content.trim().is_empty()
                })
                .collect();

            let order = shuffled_indices(seeds.len(), seed);
            let now = chrono::Utc::now().to_rfc3339();
            let mut scenarios = Vec::with_capacity(n);
            for &si in &order {
                if scenarios.len() >= n {
                    break;
                }
                let seed_mem = seeds[si];
                let cands: Vec<Candidate> = match sys
                    .engine
                    .store
                    .recall_resonance_readonly(&seed_mem.content, MAX_CANDIDATES)
                {
                    Ok(rs) => rs
                        .into_iter()
                        .map(|r| Candidate {
                            label: hash_label(&r.content),
                            amplitude: r.resonance_strength,
                        })
                        .collect(),
                    Err(_) => Vec::new(),
                };
                if cands.is_empty() {
                    continue;
                }
                scenarios.push(build_scenario(
                    hash_label(&seed_mem.content),
                    cands,
                    hemisphere,
                    now.clone(),
                ));
            }

            if scenarios.len() < n {
                eprintln!(
                    "export-recall-scenarios: emitted {} of {} requested ({} eligible seed memories)",
                    scenarios.len(),
                    n,
                    seeds.len()
                );
            }

            let bench = RecallBench {
                format: RECALL_BENCH_FORMAT.to_string(),
                generated_at: now,
                n: scenarios.len(),
                scenarios,
            };
            println!("{}", serde_json::to_string_pretty(&bench).unwrap());
        }
        "import-json" => {
            if args.len() < command_start + 2 {
                eprintln!("Usage: kannaka import-json <file.json>");
                process::exit(1);
            }
            let path = args[command_start + 1].clone();
            warn_if_readonly("import-json");
            // Shared lossless import implementation (handlers/ops.rs) —
            // identical engine to `kannaka import`; only the summary
            // output shape differs (JSON here, human-readable there).
            let s = import_memories_from_file(&mut sys, &path);

            println!(
                "{{\"imported\": {}, \"skipped\": {}, \"errors\": {}, \"total_input\": {}}}",
                s.imported, s.skipped, s.errors, s.total
            );
        }
        "hear" => {
            if args.len() < command_start + 2 {
                eprintln!("Usage: kannaka hear <audio-file-or-url>");
                eprintln!("  File:   kannaka hear ./song.mp3");
                eprintln!("  URL:    kannaka hear https://radio.ninja-portal.com/stream");
                eprintln!("  Stream URLs are sampled for ~30s (default; cap with --secs N).");
                eprintln!(
                    "  Captures are short-term by default (triage-eligible); --long-term to keep."
                );
                process::exit(1);
            }
            let target = &args[command_start + 1];

            // Optional --secs N for stream sampling cap. Default 30s ≈ 480 KB
            // at 128 kbps MP3, plenty of audio to extract tempo/centroid/rms.
            let mut secs: u64 = 30;
            // ADR-0031 Phase 2b: ear-loop captures are high-rate and semantically
            // redundant (same voice/modality), so they default to ShortTerm —
            // eviction-eligible by `triage`, promoted to LongTerm only if a dream
            // strengthens them. `--long-term` opts a specific capture out.
            let mut long_term = false;
            let mut i = command_start + 2;
            while i < args.len() {
                if args[i] == "--secs" && i + 1 < args.len() {
                    if let Ok(n) = args[i + 1].parse::<u64>() {
                        secs = n.max(1).min(600);
                    }
                    i += 2;
                } else if args[i] == "--long-term" {
                    long_term = true;
                    i += 1;
                } else {
                    i += 1;
                }
            }

            let is_url = target.starts_with("http://") || target.starts_with("https://");
            let mut tmp_holder: Option<std::path::PathBuf> = None;
            let path: std::path::PathBuf = if is_url {
                match fetch_audio_to_temp(target, secs) {
                    Ok(p) => {
                        tmp_holder = Some(p.clone());
                        p
                    }
                    Err(e) => {
                        eprintln!("Error fetching {target}: {e}");
                        process::exit(1);
                    }
                }
            } else {
                let p = std::path::PathBuf::from(target);
                if !p.exists() {
                    eprintln!("File not found: {}", p.display());
                    process::exit(1);
                }
                p
            };

            let result = sys.store_audio(&path);

            // Clean up temp file regardless of outcome.
            if let Some(tmp) = tmp_holder {
                let _ = std::fs::remove_file(&tmp);
            }

            match result {
                Ok((id, features)) => {
                    // ADR-0031 Phase 2b: tag ear-loop captures ShortTerm by default.
                    if !long_term {
                        if let Some(hrm) = sys
                            .engine
                            .store
                            .as_any_mut()
                            .downcast_mut::<kannaka_memory::hrm_store::HrmStore>()
                        {
                            hrm.set_tier(&id, kannaka_memory::medium::types::Tier::ShortTerm);
                            let _ = sys.save();
                        }
                    }
                    println!(
                        "Heard: {id}{}",
                        if long_term { "" } else { " (short-term)" }
                    );
                    println!("  Duration: {:.1}s", features.duration_secs);
                    println!("  Tempo: {:.0} BPM", features.tempo_bpm);
                    println!("  RMS: {:.4}", features.rms_mean);
                    println!("  Centroid: {:.2} kHz", features.spectral_centroid_khz);
                    if !features.feature_tags.is_empty() {
                        println!("  Tags: {}", features.feature_tags.join(", "));
                    }
                }
                Err(e) => {
                    eprintln!("Error: {e}");
                    process::exit(1);
                }
            }
        }
        // ADR-0008: the eye. Deliberately NOT `see` -- that verb is the SGA
        // glyph path and keeps its meaning. `watch` is temporal perception:
        // frames, shots, motion, visual tempo.
        #[cfg(feature = "video")]
        "watch" => {
            if args.len() < command_start + 2 {
                eprintln!("Usage: kannaka watch <video-file> [--fps N] [--json] [--long-term]");
                eprintln!("  kannaka watch ./clip.mp4");
                eprintln!("  kannaka watch ./clip.mp4 --fps 4 --json");
                eprintln!(
                    "  Clips are short-term by default (triage-eligible); --long-term to keep."
                );
                process::exit(1);
            }

            let mut fps = kannaka_memory::eye::DEFAULT_FPS;
            let mut long_term = false;
            let want_json = args.iter().any(|a| a == "--json");
            let mut i = command_start + 2;
            while i < args.len() {
                if args[i] == "--fps" && i + 1 < args.len() {
                    match args[i + 1].parse::<f32>() {
                        Ok(n) if n > 0.0 && n <= 60.0 => fps = n,
                        _ => {
                            eprintln!("--fps must be a number in (0, 60]");
                            process::exit(1);
                        }
                    }
                    i += 2;
                } else if args[i] == "--long-term" {
                    long_term = true;
                    i += 1;
                } else {
                    i += 1;
                }
            }

            let path = std::path::PathBuf::from(&args[command_start + 1]);
            if !path.exists() {
                eprintln!("File not found: {}", path.display());
                process::exit(1);
            }

            // ffmpeg is a runtime dependency and is genuinely absent on some
            // fleet nodes. Say so here, once, in words an operator can act on,
            // rather than letting it surface as a decode failure.
            if !kannaka_memory::eye::ffmpeg_available() {
                eprintln!("ffmpeg not found on PATH - the eye needs it to decode.");
                eprintln!("  Install ffmpeg (which also provides ffprobe) and re-run.");
                eprintln!("    Debian/Ubuntu: sudo apt-get install -y ffmpeg");
                eprintln!("    Oracle/RHEL:   sudo dnf install -y ffmpeg");
                eprintln!("    Windows:       winget install Gyan.FFmpeg");
                process::exit(1);
            }

            warn_if_readonly("watch");

            match sys.store_video(&path, fps) {
                Ok((id, features)) => {
                    // Mirrors `hear` (ADR-0031 Phase 2b): perception captures are
                    // high-rate and semantically redundant, so they land ShortTerm
                    // and are promoted only if a dream strengthens them.
                    if !long_term {
                        if let Some(hrm) = sys
                            .engine
                            .store
                            .as_any_mut()
                            .downcast_mut::<kannaka_memory::hrm_store::HrmStore>()
                        {
                            hrm.set_tier(&id, kannaka_memory::medium::types::Tier::ShortTerm);
                            let _ = sys.save();
                        }
                    }

                    let colors: Vec<String> = features
                        .dominant_colors
                        .iter()
                        .map(|c| {
                            format!("{} ({:.0}deg s{:.2} v{:.2})", c.name, c.hue, c.saturation, c.value)
                        })
                        .collect();

                    if want_json {
                        let out = serde_json::json!({
                            "id": id.to_string(),
                            "path": path.display().to_string(),
                            "tier": if long_term { "long-term" } else { "short-term" },
                            "modality": "visual",
                            "duration_secs": features.duration_secs,
                            "frame_count": features.frame_count,
                            "analysis_fps": features.analysis_fps,
                            "shot_count": features.shot_count,
                            "shot_boundaries": features.shot_boundaries,
                            "motion": {
                                "mean_px": features.motion_mean,
                                "std_px": features.motion_std,
                                "max_px": features.motion_max,
                            },
                            "mean_brightness": features.mean_brightness(),
                            "mean_contrast": features.mean_contrast(),
                            "visual_tempo_bpm": features.visual_tempo_bpm(),
                            "dominant_colors": features
                                .dominant_colors
                                .iter()
                                .map(|c| serde_json::json!({
                                    "name": c.name,
                                    "hue": c.hue,
                                    "saturation": c.saturation,
                                    "value": c.value,
                                }))
                                .collect::<Vec<_>>(),
                            "tags": features.feature_tags,
                            "feature_dim": features.vector.len(),
                            "spatial_dim": features.spatial.vector.len(),
                            "temporal_dim": features.temporal.vector.len(),
                        });
                        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
                    } else {
                        println!(
                            "Watched: {id}{}",
                            if long_term { "" } else { " (short-term)" }
                        );
                        println!(
                            "  Duration: {:.1}s ({} frames @ {:.1} fps)",
                            features.duration_secs, features.frame_count, features.analysis_fps
                        );
                        if features.shot_boundaries.is_empty() {
                            println!("  Shots: 1 (no cuts)");
                        } else {
                            println!(
                                "  Shots: {} (cuts at frames {})",
                                features.shot_count,
                                features
                                    .shot_boundaries
                                    .iter()
                                    .map(|b| b.to_string())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            );
                        }
                        println!(
                            "  Motion: {:.2} px/step (peak {:.2})",
                            features.motion_mean, features.motion_max
                        );
                        println!(
                            "  Brightness: {:.0}/255   Contrast: {:.1}",
                            features.mean_brightness(),
                            features.mean_contrast()
                        );
                        println!("  Visual tempo: {:.0} bpm", features.visual_tempo_bpm());
                        if !colors.is_empty() {
                            println!("  Colours: {}", colors.join(", "));
                        }
                        if !features.feature_tags.is_empty() {
                            println!("  Tags: {}", features.feature_tags.join(", "));
                        }
                    }
                }
                Err(e) => {
                    eprintln!("Error: {e}");
                    process::exit(1);
                }
            }
        }
        #[cfg(feature = "glyph")]
        "see" => {
            if args.len() < command_start + 2 {
                eprintln!("Usage: kannaka see <file>");
                process::exit(1);
            }
            let path = std::path::PathBuf::from(&args[command_start + 1]);
            if !path.exists() {
                eprintln!("File not found: {}", path.display());
                process::exit(1);
            }
            match sys.store_glyph(&path) {
                Ok((id, glyph)) => {
                    println!("Seen: {id}");
                    println!("  Folds: {}", glyph.fold_sequence.len());
                    println!(
                        "  Centroid: ({}, {}, {})",
                        glyph.sga_centroid.0, glyph.sga_centroid.1, glyph.sga_centroid.2
                    );
                    println!(
                        "  Fano: [{:.3}, {:.3}, {:.3}, {:.3}, {:.3}, {:.3}, {:.3}]",
                        glyph.fano_signature[0],
                        glyph.fano_signature[1],
                        glyph.fano_signature[2],
                        glyph.fano_signature[3],
                        glyph.fano_signature[4],
                        glyph.fano_signature[5],
                        glyph.fano_signature[6]
                    );
                    println!("  Ratio: {:.2}x", glyph.compression_ratio);
                    let freqs = glyph.to_frequencies();
                    if !freqs.is_empty() {
                        let freq_strs: Vec<String> = freqs
                            .iter()
                            .take(7)
                            .map(|f| format!("{f:.1} Hz"))
                            .collect();
                        println!("  Frequencies: {}", freq_strs.join(", "));
                    }
                }
                Err(e) => {
                    eprintln!("Error: {e}");
                    process::exit(1);
                }
            }
        }
        #[cfg(feature = "nats")]
        "swarm" => {
            if args.len() < command_start + 2 {
                eprintln!("Usage: kannaka swarm <join|status|sync|queen|hives|publish|leave|listen|serve|tail|exemplars|cores|peers|absorb|autoabsorb|enqueue|worker|brief|health|gaps|plan|loop|activate-gate|beacon>");
                process::exit(1);
            }

            // Agent ID: env var > config.toml > persisted file > generate new
            let agent_id = cfg.agent.id.clone();

            match args[command_start + 1].as_str() {
                // ADR-0035 Cap 5 (Wave 1): swarm brief. LOCAL-FIRST — builds a
                // brief from this agent's own recall. Consensus voting and
                // contradiction detection require the multi-peer NATS fan-out
                // (next increment), so they are reported as pending. Exercises
                // the pure `sensemaking` module end-to-end.
                "brief" => {
                    let want_json = args.iter().any(|a| a == "--json");
                    let topic = brief_topic(&args, command_start + 2);
                    if topic.trim().is_empty() {
                        eprintln!("Usage: kannaka swarm brief \"<topic>\" [--json] [--peers] [--nats-url URL]");
                        process::exit(1);
                    }
                    // --peers (Wave 1 fan-out): fan recall out to live swarm peers
                    // and run consensus voting. Falls back to local on no peers.
                    //
                    // The broker is resolved HERE, with the same precedence
                    // every other swarm verb uses (CLI > env > config), and
                    // handed down — swarm_brief_peers used to read
                    // cfg.swarm.nats_url directly, which silently ignored
                    // --nats-url (#766, first half).
                    let mut handled = false;
                    if args.iter().any(|a| a == "--peers") {
                        let nats_url = resolve_nats_url(&args, command_start, &cfg.swarm.nats_url);
                        handled = swarm_brief_peers(&nats_url, &topic, want_json);
                    }
                    if !handled {
                    match sys.recall(&topic, 8) {
                        Ok(results) => {
                            // Wave 3 Task 3.2b — discount temporal validity: a
                            // fact past its `expires_at` (Expired) or before its
                            // `effective_at` (Future) is not true *now*, so it is
                            // excluded from the brief's consensus + confidence.
                            // Memories with no temporal bounds read as Current
                            // (unchanged behavior).
                            let now = chrono::Utc::now();
                            let known: Vec<kannaka_memory::sensemaking::ConsensusItem> = results
                                .iter()
                                .filter(|r| match sys.engine.store.get(&r.id) {
                                    Ok(Some(m)) => kannaka_memory::temporal::is_current(
                                        &kannaka_memory::temporal::TemporalSpec::from_memory(&m),
                                        now,
                                    ),
                                    _ => true,
                                })
                                .map(|r| kannaka_memory::sensemaking::ConsensusItem {
                                    content: r.content.clone(),
                                    support: 1,
                                    confidence: (r.similarity * r.strength).clamp(0.0, 1.0),
                                    mean_similarity: r.similarity,
                                })
                                .collect();
                            let brief = kannaka_memory::sensemaking::compose_brief(
                                &topic,
                                known,
                                vec![],
                                vec![(agent_id.clone(), 1.0)],
                            );
                            if want_json {
                                let known_json: Vec<_> = brief
                                    .known
                                    .iter()
                                    .map(|c| serde_json::json!({
                                        "content": c.content,
                                        "confidence": c.confidence,
                                    }))
                                    .collect();
                                println!("{}", serde_json::json!({
                                    "topic": brief.topic,
                                    "confidence": brief.confidence,
                                    "known": known_json,
                                    "contradictions": [],
                                    "scope": "local",
                                    "note": "local-first; run against a live swarm for consensus + contradictions",
                                }));
                            } else {
                                println!("Swarm brief — \"{}\"  (local-first; peers pending)", brief.topic);
                                println!("  confidence: {:.2}", brief.confidence);
                                println!("  known ({}):", brief.known.len());
                                for (i, c) in brief.known.iter().enumerate() {
                                    println!("    {}. [{:.2}] {}", i + 1, c.confidence, c.content);
                                }
                                println!("  consensus voting + contradiction detection require a live swarm");
                            }
                        }
                        Err(e) => {
                            eprintln!("brief: recall error: {e}");
                            process::exit(1);
                        }
                    }
                    }
                }
                // ADR-0035 Cap 4 / Wave 2 Task 2.1 — memory immune system
                // (DETECTION, dry-run). Classifies local memories for duplicate /
                // stale / low-confidence / hallucinated and prints the at-risk list.
                // No mutation — lifecycle actions are Task 2.2.
                "health" => {
                    let want_json = args.iter().any(|a| a == "--json");
                    let all = sys.engine.store.all_memories().unwrap_or_default();
                    let now = chrono::Utc::now();
                    let t = kannaka_memory::immune::Thresholds::default();
                    // Pairwise cosine for duplicate detection; skipped for very
                    // large stores to bound the O(N²) cost.
                    let do_pairwise = all.len() <= 2000;
                    let mut verdicts: Vec<(kannaka_memory::immune::HealthVerdict, String)> =
                        Vec::new();
                    for (i, m) in all.iter().enumerate() {
                        let (mut max_sim, mut sib_hi) = (0.0f32, false);
                        if do_pairwise && !m.vector.is_empty() {
                            for (j, other) in all.iter().enumerate() {
                                if i == j || other.vector.is_empty() {
                                    continue;
                                }
                                let s = kannaka_memory::wave::cosine_similarity(
                                    &m.vector,
                                    &other.vector,
                                );
                                // #359: deterministic tie-break so an equal-amplitude
                                // duplicate cluster keeps exactly one copy. Strict `>`
                                // on amplitude alone left every copy seeing its sibling
                                // as "not higher" → none flagged. A sibling "beats" m if
                                // it has higher amplitude, or equal amplitude and a
                                // greater id (only the max-id copy has no beater → it is
                                // the unique keeper). Accumulate across all maximally
                                // similar siblings, not just the first one scanned.
                                let beats = other.amplitude > m.amplitude
                                    || (other.amplitude == m.amplitude && other.id > m.id);
                                if s > max_sim {
                                    max_sim = s;
                                    sib_hi = beats;
                                } else if s == max_sim && beats {
                                    sib_hi = true;
                                }
                            }
                        }
                        let age_days = (now - m.created_at).num_days().max(0) as f32;
                        let sig = kannaka_memory::immune::MemorySignals {
                            id: m.id.to_string(),
                            amplitude: m.amplitude,
                            age_days,
                            // Proxy: active memories carry amplitude (no last-access field).
                            recent_access: m.amplitude >= 0.3,
                            hallucinated: m.hallucinated,
                            max_sibling_similarity: max_sim,
                            sibling_higher_amp: sib_hi,
                        };
                        let v = kannaka_memory::immune::classify_memory_health(&sig, &t);
                        if !v.is_clean() {
                            verdicts.push((v, m.content.clone()));
                        }
                    }
                    verdicts.sort_by(|a, b| b.0.severity.total_cmp(&a.0.severity));
                    // Capture before the mutable boost below: all_memories()
                    // borrows the store, so release it before sys.boost().
                    let total = all.len();
                    // Task 2.2 — apply reversible lifecycle actions. Default is
                    // dry-run; --apply mutates amplitude (down-rank/quarantine/expire
                    // via boost). All reversible: boost the id back up to restore.
                    let apply = args.iter().any(|a| a == "--apply");
                    if apply {
                        warn_if_readonly("swarm health --apply");
                        let mut applied = 0usize;
                        for (v, _c) in &verdicts {
                            if let Some(factor) = kannaka_memory::immune::amplitude_factor(v.action) {
                                if let Ok(id) = uuid::Uuid::parse_str(&v.id) {
                                    if sys.boost(&id, factor as f64).is_ok() {
                                        applied += 1;
                                    }
                                }
                            }
                        }
                        eprintln!(
                            "immune: applied {applied} reversible action(s); boost the id back up to restore"
                        );
                    }
                    if want_json {
                        let arr: Vec<_> = verdicts
                            .iter()
                            .map(|(v, content)| {
                                serde_json::json!({
                                    "id": v.id,
                                    "severity": v.severity,
                                    "action": format!("{:?}", v.action),
                                    "flags": v.flags.iter().map(|f| format!("{f:?}")).collect::<Vec<_>>(),
                                    "preview": content.chars().take(80).collect::<String>(),
                                })
                            })
                            .collect();
                        println!("{}", serde_json::json!({
                            "at_risk": verdicts.len(),
                            "total": total,
                            "dry_run": !apply,
                            "memories": arr,
                        }));
                    } else {
                        println!(
                            "Memory immune report — {} at-risk / {} total  ({})",
                            verdicts.len(),
                            total,
                            if apply { "actions applied" } else { "dry-run; no mutation" }
                        );
                        for (v, content) in verdicts.iter().take(25) {
                            let flags: Vec<String> =
                                v.flags.iter().map(|f| format!("{f:?}")).collect();
                            let preview: String = content.chars().take(70).collect();
                            println!(
                                "  [{:.2}] {:<13} {:<26} {}",
                                v.severity,
                                format!("{:?}", v.action),
                                flags.join(","),
                                preview
                            );
                        }
                        if verdicts.len() > 25 {
                            println!("  ... and {} more", verdicts.len() - 25);
                        }
                    }
                }
                // ADR-0035 Cap 1 / Wave 3 Task 3.3 — knowledge gap detection.
                // LOCAL-FIRST: maps THIS agent's clusters into a coverage map and
                // flags weakly-represented / low-confidence domains. Multi-peer
                // coverage (via swarm exemplars) is the next increment.
                "gaps" => {
                    let want_json = args.iter().any(|a| a == "--json");
                    let report = sys.observe();
                    let clusters: Vec<kannaka_memory::gap::DomainCluster> = report
                        .clusters
                        .clusters
                        .iter()
                        .filter(|c| !c.theme.trim().is_empty())
                        .map(|c| kannaka_memory::gap::DomainCluster {
                            agent_id: agent_id.clone(),
                            theme: c.theme.clone(),
                            size: c.size.max(1),
                            coherence: c.coherence,
                            mean_amplitude: c.mean_amplitude,
                        })
                        .collect();
                    // Keyed variant (#771): trim+case-fold is key-reducible,
                    // and the predicate scan is O(n²) on distinct themes.
                    let map = kannaka_memory::gap::build_coverage_map_keyed(&clusters, 1, |t| {
                        t.trim().to_ascii_lowercase()
                    });
                    let gaps = kannaka_memory::gap::detect_gaps(&map, 0.4);
                    if want_json {
                        let arr: Vec<_> = gaps
                            .iter()
                            .map(|g| serde_json::json!({
                                "domain": g.domain,
                                "coverage": g.coverage,
                                "kind": format!("{:?}", g.kind),
                                "peers_holding": g.peers_holding,
                            }))
                            .collect();
                        println!("{}", serde_json::json!({
                            "scope": "local",
                            "domains": map.len(),
                            "gaps": arr,
                            "note": "local-first; multi-peer coverage via swarm exemplars is next",
                        }));
                    } else {
                        println!(
                            "Knowledge gaps — {} domain(s), {} gap(s)  (local-first)",
                            map.len(),
                            gaps.len()
                        );
                        for g in gaps.iter().take(20) {
                            let dom: String = g.domain.chars().take(60).collect();
                            println!("  [{:.2}] {:<18} {}", g.coverage, format!("{:?}", g.kind), dom);
                        }
                        println!("  multi-peer coverage (swarm exemplars) is the next increment");
                    }
                }
                // ADR-0035 Wave 4 Task 4.1 — autonomous research planner. Turns the
                // local gap map into ranked research tasks (the collective
                // generalization of the single-agent curiosity loop). Local-first;
                // peer assignment + enqueue onto KANNAKA.work.research is the next step.
                "plan" => {
                    let want_json = args.iter().any(|a| a == "--json");
                    let report = sys.observe();
                    let clusters: Vec<kannaka_memory::gap::DomainCluster> = report
                        .clusters
                        .clusters
                        .iter()
                        .filter(|c| !c.theme.trim().is_empty())
                        .map(|c| kannaka_memory::gap::DomainCluster {
                            agent_id: agent_id.clone(),
                            theme: c.theme.clone(),
                            size: c.size.max(1),
                            coherence: c.coherence,
                            mean_amplitude: c.mean_amplitude,
                        })
                        .collect();
                    // Keyed variant (#771): trim+case-fold is key-reducible,
                    // and the predicate scan is O(n²) on distinct themes.
                    let map = kannaka_memory::gap::build_coverage_map_keyed(&clusters, 1, |t| {
                        t.trim().to_ascii_lowercase()
                    });
                    let gaps = kannaka_memory::gap::detect_gaps(&map, 0.4);
                    let tasks = kannaka_memory::research_planner::plan_research(&gaps, 0);
                    if want_json {
                        let arr: Vec<_> = tasks
                            .iter()
                            .map(|t| serde_json::json!({
                                "domain": t.domain,
                                "theme_query": t.theme_query,
                                "kind": format!("{:?}", t.kind),
                                "priority": t.priority,
                                "peers_holding": t.peers_holding,
                                "rationale": t.rationale,
                            }))
                            .collect();
                        println!("{}", serde_json::json!({
                            "scope": "local",
                            "tasks": arr,
                            "note": "local-first; peer assignment + enqueue onto KANNAKA.work.research is next",
                        }));
                    } else {
                        println!(
                            "Research plan — {} task(s) from {} gap(s)  (local-first)",
                            tasks.len(),
                            gaps.len()
                        );
                        for (i, t) in tasks.iter().take(15).enumerate() {
                            let q: String = t.theme_query.chars().take(56).collect();
                            println!(
                                "  {}. [p{:.2}] {:<18} {}",
                                i + 1,
                                t.priority,
                                format!("{:?}", t.kind),
                                q
                            );
                        }
                        println!("  next: assign to best-fit peers + enqueue research workers");
                    }
                }
                // ADR-0035 Wave 4 Task 4.3 — self-directed sensemaking loop. Runs
                // the five-state machine over a SwarmContext (local gap count;
                // peers/coherence/contradictions overridable via flags). The daemon
                // that EXECUTES each state's action is the next increment.
                "loop" => {
                    let flagf = |name: &str, def: f64| -> f64 {
                        args.iter()
                            .position(|a| a == name)
                            .and_then(|i| args.get(i + 1))
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(def)
                    };
                    let steps = (flagf("--steps", 5.0) as usize).clamp(1, 50);
                    // Local gap count (cheap signal from this agent's clusters).
                    let report = sys.observe();
                    let clusters: Vec<kannaka_memory::gap::DomainCluster> = report
                        .clusters
                        .clusters
                        .iter()
                        .filter(|c| !c.theme.trim().is_empty())
                        .map(|c| kannaka_memory::gap::DomainCluster {
                            agent_id: agent_id.clone(),
                            theme: c.theme.clone(),
                            size: c.size.max(1),
                            coherence: c.coherence,
                            mean_amplitude: c.mean_amplitude,
                        })
                        .collect();
                    // Keyed variant (#771): trim+case-fold is key-reducible,
                    // and the predicate scan is O(n²) on distinct themes.
                    let map = kannaka_memory::gap::build_coverage_map_keyed(&clusters, 1, |t| {
                        t.trim().to_ascii_lowercase()
                    });
                    let gap_count = kannaka_memory::gap::detect_gaps(&map, 0.4).len();
                    let ctx = kannaka_memory::swarm_loop::SwarmContext {
                        peer_count: flagf("--peers", 0.0) as usize,
                        order_parameter: flagf("--coherence", 0.0) as f32,
                        open_contradictions: flagf("--contradictions", 0.0) as usize,
                        gap_count,
                        at_risk_memories: flagf("--at-risk", 0.0) as usize,
                    };
                    let mut state = kannaka_memory::swarm_loop::SwarmState::Discovery;
                    println!(
                        "Swarm loop — {} steps  (peers={}, coherence={:.2}, gaps={}, contradictions={})",
                        steps, ctx.peer_count, ctx.order_parameter, ctx.gap_count, ctx.open_contradictions
                    );
                    for n in 1..=steps {
                        let outcome = kannaka_memory::swarm_loop::next_state(state, &ctx);
                        println!(
                            "  {}. {:<16} -> {}",
                            n,
                            state.name(),
                            outcome.action
                        );
                        state = outcome.next;
                    }
                    println!("  (daemon execution of each state's action is the next increment)");
                }
                "join" => {
                    // `kannaka swarm join` is the user-facing "run a node"
                    // command. Historically it was one-shot: announce, publish
                    // initial phase, exit. The radio's swarm visualization
                    // prunes agents after 5 min of silence (no QUEEN.phase.*
                    // republish), so the user's node would disappear from
                    // /player almost immediately after `join` returned. Users
                    // reasonably expected `join` to run the node, so the
                    // command now stays foregrounded and republishes phase +
                    // presence every 30s until Ctrl+C, at which point it
                    // announces leave and exits cleanly. Pass --once to keep
                    // the legacy one-shot behavior (used by scripts that
                    // manage the heartbeat themselves).
                    const JOIN_USAGE: &str = "Usage: kannaka swarm join [--agent-id ID] [--display-name NAME] [--once] [--heartbeat-secs N] [--nats-url URL]";
                    let mut my_agent_id = agent_id.clone();
                    let mut display_name = String::new();
                    let mut once = false;
                    let mut heartbeat_secs: u64 = 30;
                    let mut i = command_start + 2;
                    while i < args.len() {
                        match args[i].as_str() {
                            "--agent-id" => {
                                my_agent_id = flag_value(&args, i, "--agent-id", JOIN_USAGE).to_string();
                                i += 2;
                            }
                            "--display-name" => {
                                display_name = flag_value(&args, i, "--display-name", JOIN_USAGE).to_string();
                                i += 2;
                            }
                            "--nats-url" => {
                                let _ = flag_value(&args, i, "--nats-url", JOIN_USAGE);
                                i += 2;
                            }
                            "--once" => {
                                once = true;
                                i += 1;
                            }
                            "--heartbeat-secs" => {
                                let v: u64 = parse_flag_value(
                                    &args, i, "--heartbeat-secs",
                                    "Usage: kannaka swarm join [--agent-id ID] [--display-name NAME] [--once] [--heartbeat-secs N] [--nats-url URL]",
                                );
                                heartbeat_secs = v.max(5);
                                i += 2;
                            }
                            _ => {
                                i += 1;
                            }
                        }
                    }
                    if display_name.is_empty() {
                        display_name = my_agent_id.clone();
                    }

                    // ADR-0036: the continuous writer (kannaka-memory.service runs
                    // `swarm join`) is the sole HRM writer — hold the write lock
                    // for its lifetime so a concurrent `dream` (especially the
                    // Node-triggered lite dream) skips instead of double-writing.
                    // Readonly replicas don't write, so they must NOT take the
                    // lock (they'd block dreams forever). flock auto-releases on
                    // process exit, so a crash never leaves a stale lock.
                    let _join_write_lock: Option<WriteLock> = {
                        let readonly = std::env::var("KANNAKA_READONLY")
                            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                            .unwrap_or(false);
                        if readonly {
                            None
                        } else {
                            acquire_write_lock_blocking(60)
                        }
                    };

                    // Persist agent_id for subsequent commands
                    let id_file = data_dir().join("agent_id");
                    let _ = std::fs::create_dir_all(id_file.parent().unwrap());
                    if let Err(e) = std::fs::write(&id_file, &my_agent_id) {
                        eprintln!("Warning: could not persist agent_id: {e}");
                    }

                    let nats_url = resolve_nats_url(&args, command_start, &cfg.swarm.nats_url);
                    // `mut`: the heartbeat loop reconnects a dead transport
                    // (reconnect takes &mut self to swap the connection).
                    let mut transport = match try_nats_connect(&nats_url) {
                        Some(t) => t,
                        None => {
                            eprintln!("Error: NATS connection required for swarm. Set KANNAKA_NATS_URL or use --nats-url.");
                            process::exit(1);
                        }
                    };

                    // True session start, stamped ONCE (#587). Every heartbeat
                    // republishes the presence record, and each one used to
                    // restamp `joined_at` with `Utc::now()` — so `joined_at`
                    // always equalled `last_seen` and presence-backed UIs could
                    // never show real session age.
                    let session_joined_at = chrono::Utc::now().to_rfc3339();

                    // Stored SSO identity (swarm agent identity, step 2):
                    // when the operator is logged in via `kannaka identity`,
                    // announce + presence carry an optional identity block
                    // (user_id/email only). Not logged in → payloads are
                    // byte-identical to pre-identity versions.
                    let identity = kannaka_memory::nats::AnnounceIdentity::from_store();
                    if let Some(ref idn) = identity {
                        println!("[identity] Joining as {} ({})", idn.email, idn.user_id);
                    }

                    if let Err(e) =
                        transport.announce_join_with_identity(&my_agent_id, identity.as_ref())
                    {
                        eprintln!("[nats] Warning: announce failed: {e}");
                    }
                    // #572: this was `let _ = transport.ensure_presence_stream();`.
                    // `swarm peers` reads presence ONLY from the JetStream-backed
                    // KANNAKA_PRESENCE stream, so if the stream is ABSENT the agent
                    // never becomes discoverable — while join happily reported
                    // success. That must be LOUD. But a refused create says only
                    // that THIS identity cannot create the stream (#928): on a
                    // running swarm it already exists, presence publishes are
                    // retained, and the node is listed by other hosts — so check
                    // before warning, and keep the warning for the absent case.
                    if let Err(e) = transport.ensure_presence_stream() {
                        let exists = transport.presence_stream_exists();
                        eprintln!(
                            "{}",
                            kannaka_memory::nats::presence_stream_notice(
                                &e.to_string(),
                                exists,
                                transport.is_authenticated(),
                            )
                        );
                    }

                    // #582 (resolved by removal): a QUEEN_AGENTS KV registration
                    // was written here briefly, but nothing in production ever
                    // read the bucket -- presence (KANNAKA.presence) is the peer
                    // directory, and trust is reputation-derived
                    // (collective/trust.rs), never registration-derived, because
                    // a self-registration carries no trust signal a Sybil could
                    // not forge. The vestigial write/read pair is gone.

                    let initial_phase = swarm_publish_heartbeat(
                        &mut sys,
                        &my_agent_id,
                        &display_name,
                        &transport,
                        "initial",
                        identity.as_ref(),
                        &session_joined_at,
                    );
                    println!("Joined swarm as '{display_name}' ({my_agent_id})");
                    println!(
                        "[nats] Initial phase \u{03b8}={initial_phase:.3} published to {nats_url}"
                    );

                    if once {
                        return;
                    }

                    // Daemon mode — keep the node visible by republishing
                    // phase every `heartbeat_secs`. The radio's 5-min prune
                    // window is the upper bound; we use 30s by default so
                    // the badge in /player feels responsive.
                    use std::sync::atomic::{AtomicBool, Ordering};
                    use std::sync::Arc;
                    let running = Arc::new(AtomicBool::new(true));
                    let r = Arc::clone(&running);
                    if let Err(e) = ctrlc::set_handler(move || {
                        r.store(false, Ordering::SeqCst);
                    }) {
                        eprintln!("[nats] Warning: could not install Ctrl+C handler: {e} — Ctrl+C will not announce leave");
                    }

                    println!(
                        "[nats] Heartbeat every {heartbeat_secs}s — Ctrl+C to leave the swarm cleanly"
                    );
                    // Set KANNAKA_AGENT_ID so publish_consciousness_to_nats
                    // knows who we are (it bails out silently otherwise).
                    std::env::set_var("KANNAKA_AGENT_ID", &my_agent_id);
                    // Publish initial consciousness snapshot — this is what
                    // populates the observatory's KANNAKA.consciousness
                    // subscription so the dashboard isn't sitting at zeros.
                    // Costs one eigendecomp pass (~15s on a mature HRM) but
                    // happens once at startup. Subsequent ticks publish the
                    // CACHED metrics (cheap) every heartbeat, with a fresh
                    // assess every CONSCIOUSNESS_REFRESH_TICKS to keep the
                    // canonical Φ from drifting.
                    let initial_assess = sys.assess();
                    sys.publish_consciousness_to_nats(&initial_assess);
                    const CONSCIOUSNESS_REFRESH_TICKS: u64 = 10;

                    // ADR-0037 Track-D heartbeat coupling (default OFF). Conservative,
                    // env-tunable cadence + gate. NB: a coupling tick is SYNCHRONOUS in
                    // this single-threaded loop — belief_core_snapshot (a dense-Gram PCA)
                    // + couple + save can block the phase beacon for several seconds on a
                    // large field / 1-core box, so the cadence is slow by default (40
                    // ticks ≈ 20 min @ 30s). min_cos is HIGH (0.7, well above the ~0.54
                    // median match scale) so it under-couples — only strongly-shared
                    // beliefs. The per-event nudge is gentle; max_disp caps drift PER
                    // EVENT, not cumulatively, so over many events a node's SHARED-content
                    // phases converge toward swarm consensus (intended) while unmatched/
                    // unique beliefs stay put (min_cos gate) and recall is untouched
                    // (phase-only ⇒ recall = cosine×energy is phase-independent). Coupling
                    // is SKIPPED on a read-only node (it could never persist and would
                    // only pollute the reader's published beacon/metrics). ⚠ A coupling
                    // node holds the writer lock continuously: any prune/triage cron on
                    // the same node MUST stop the writer first (like dream-cron) or its
                    // lockless save can lost-update the coupled state — see the PR notes.
                    let coupling_on = exemplar_coupling_enabled() && !readonly_env_active();
                    if exemplar_coupling_enabled() && readonly_env_active() {
                        println!("[couple] KANNAKA_EXEMPLAR_COUPLING set but node is read-only — coupling SKIPPED (a reader can't persist).");
                    }
                    let coupling_ticks = std::env::var("KANNAKA_EXEMPLAR_COUPLING_TICKS")
                        .ok()
                        .and_then(|v| v.parse::<u64>().ok())
                        .filter(|&n| n > 0)
                        .unwrap_or(40);
                    let coupling_min_cos = std::env::var("KANNAKA_EXEMPLAR_COUPLING_MIN_COS")
                        .ok()
                        .and_then(|v| v.parse::<f32>().ok())
                        .unwrap_or(0.7);
                    // L7 belief-arm result (2026-07-21): no single coupling strength
                    // satisfies both the individual claim (stability⇒recall) and the
                    // swarm claim (shared⇒agreement) — but a strong-then-weak
                    // ALTERNATION does, and the order is load-bearing (weak-then-strong
                    // collapses the swarm claim). schedule=alternate runs odd coupling
                    // events at strong (default 2× weak) and even events at weak;
                    // the first event is strong — consolidate, then diversify.
                    // **Alternation is the DEFAULT** (Nick, 2026-07-21, on the L7
                    // verdicts); set KANNAKA_EXEMPLAR_COUPLING_SCHEDULE=fixed for the
                    // old single-strength behavior. The measured 2:1 strong:weak
                    // ratio is preserved at production magnitudes (0.10/0.05).
                    let coupling_strength_weak = std::env::var("KANNAKA_EXEMPLAR_COUPLING_STRENGTH")
                        .ok()
                        .and_then(|v| v.parse::<f32>().ok())
                        .unwrap_or(0.05);
                    let coupling_strength_strong = std::env::var("KANNAKA_EXEMPLAR_COUPLING_STRONG")
                        .ok()
                        .and_then(|v| v.parse::<f32>().ok())
                        .unwrap_or(coupling_strength_weak * 2.0);
                    let coupling_alternate = std::env::var("KANNAKA_EXEMPLAR_COUPLING_SCHEDULE")
                        .map(|v| !v.eq_ignore_ascii_case("fixed"))
                        .unwrap_or(true);
                    if coupling_on {
                        if coupling_alternate {
                            println!(
                                "[couple] always-on belief coupling ENABLED — every {coupling_ticks} ticks, min_cos {coupling_min_cos:.2}, schedule=alternate strong-then-weak ({coupling_strength_strong:.3}/{coupling_strength_weak:.3}) (phase-only; a coupling tick briefly blocks the beacon; needs belief on)"
                            );
                        } else {
                            println!(
                                "[couple] always-on belief coupling ENABLED — every {coupling_ticks} ticks, min_cos {coupling_min_cos:.2}, strength {coupling_strength_weak:.3} fixed (phase-only; a coupling tick briefly blocks the beacon; needs belief on)"
                            );
                        }
                    }

                    // #831: this swarm-join daemon is the swarm's sole HRM
                    // writer, but autosnapshot only ever ran inside the substrate
                    // daemon -- which is not what runs on a seed -- so a node
                    // could persist writes for months with no restore point.
                    // Snapshot on a cadence here. Default hourly;
                    // KANNAKA_SNAPSHOT_INTERVAL_SECS=0 disables. Retain 24 (one
                    // day hourly) rather than the naive 168, because production
                    // HRMs are tens of MB and 168 * ~30 MB would refill the disk.
                    let snapshot_interval_secs: u64 =
                        std::env::var("KANNAKA_SNAPSHOT_INTERVAL_SECS")
                            .ok()
                            .and_then(|s| s.parse().ok())
                            .unwrap_or(3600);
                    // A read-only replica must not snapshot (its .hrm is not the
                    // authoritative one and it holds no writer lock).
                    let snapshot_on = snapshot_interval_secs > 0 && !readonly_env_active();
                    // Offset the clock back one interval so the first tick takes
                    // an early snapshot -- quick durability on a fresh daemon --
                    // instead of waiting a full interval for the first restore point.
                    let mut last_snapshot = std::time::Instant::now()
                        .checked_sub(std::time::Duration::from_secs(snapshot_interval_secs))
                        .unwrap_or_else(std::time::Instant::now);
                    if snapshot_on {
                        println!(
                            "[snapshot] autosnapshot ON — every {}s, retain 24, {}/snapshots (KANNAKA_SNAPSHOT_INTERVAL_SECS=0 disables)",
                            snapshot_interval_secs,
                            data_dir().display()
                        );
                    } else if readonly_env_active() {
                        println!("[snapshot] autosnapshot OFF — read-only node");
                    } else {
                        println!("[snapshot] autosnapshot OFF — KANNAKA_SNAPSHOT_INTERVAL_SECS=0");
                    }

                    let mut tick: u64 = 0;
                    while running.load(Ordering::SeqCst) {
                        // Granular sleep so Ctrl+C is responsive (<= 1s).
                        for _ in 0..heartbeat_secs {
                            if !running.load(Ordering::SeqCst) {
                                break;
                            }
                            std::thread::sleep(std::time::Duration::from_secs(1));
                        }
                        if !running.load(Ordering::SeqCst) {
                            break;
                        }
                        tick += 1;
                        let p = swarm_publish_heartbeat(
                            &mut sys,
                            &my_agent_id,
                            &display_name,
                            &transport,
                            "heartbeat",
                            identity.as_ref(),
                            &session_joined_at,
                        );
                        // Quiet output — one terse status line per tick.
                        println!("[nats] heartbeat #{tick} \u{03b8}={p:.3}");

                        // #831 autosnapshot. Best-effort: the local .hrm.gz body
                        // is written before the manifest is published, so an Err
                        // here (usually a NATS publish hiccup) still means the
                        // restore point landed on disk. Never fatal to the loop.
                        if snapshot_on
                            && last_snapshot.elapsed().as_secs() >= snapshot_interval_secs
                        {
                            if let Err(e) = handlers_substrate::capture_and_publish_snapshot(
                                &transport,
                                &my_agent_id,
                                &mut sys,
                                Some(24),
                            ) {
                                eprintln!(
                                    "[snapshot] WARNING: {e} — local snapshot may still have landed before the failing step"
                                );
                            }
                            last_snapshot = std::time::Instant::now();
                        }

                        // Recovery: this daemon is the swarm's sole HRM writer
                        // and, until now, NEVER dialed again after a dead
                        // connection — publishes buffered, the 5-min presence
                        // prune dropped the node, and it printed heartbeats
                        // into the void until a human restarted it. The
                        // is_connected() check is flag-first (no ping after a
                        // failed publish) and a live PING/PONG otherwise, so
                        // a silently-dead socket is also caught within one
                        // tick. The heartbeat interval is the retry backoff;
                        // buffered messages replay in order inside reconnect().
                        if !transport.is_connected() {
                            eprintln!(
                                "[nats] connection lost (tick #{tick}) — reconnecting to {nats_url}…"
                            );
                            match transport.reconnect() {
                                Ok(()) => {
                                    eprintln!("[nats] reconnected — re-announcing presence");
                                    // Re-announce so peers that pruned us during
                                    // the outage re-learn this node immediately
                                    // rather than at the next heartbeat.
                                    if let Err(e) = transport.announce_join_with_identity(
                                        &my_agent_id,
                                        identity.as_ref(),
                                    ) {
                                        eprintln!("[nats] Warning: re-announce failed: {e}");
                                    }
                                }
                                Err(e) => {
                                    eprintln!(
                                        "[nats] reconnect failed: {e} — retrying next heartbeat ({heartbeat_secs}s)"
                                    );
                                }
                            }
                        }

                        // Periodic consciousness republish — keeps the
                        // observatory and radio in sync with this node's
                        // current Φ even if no one runs `kannaka assess`
                        // out-of-band. Heavy step (eigendecomp) so we do
                        // it on a slow cadence; cheap-path cached publish
                        // on every other tick keeps the freshness window
                        // tight without burning CPU.
                        if tick % CONSCIOUSNESS_REFRESH_TICKS == 0 {
                            let state = sys.assess();
                            sys.publish_consciousness_to_nats(&state);
                        }

                        // ADR-0037 Track-D: always-on belief coupling (default OFF).
                        // On a slow cadence: (1) publish our belief cores so peers can
                        // converge toward us, (2) couple our phases toward peers' shared
                        // beliefs. Phase-only ⇒ recall preserved; the heartbeat's own
                        // flush (in swarm_publish_heartbeat above) persists the result.
                        // Requires a re-phased belief field; skips on no peers / errors.
                        if running.load(Ordering::SeqCst)
                            && coupling_on
                            && tick % coupling_ticks == 0
                            && kannaka_memory::medium::chiral::belief_phase_enabled()
                        {
                            // (1) publish our (fresh) cores.
                            if let Some(h) = sys
                                .engine
                                .store
                                .as_any()
                                .downcast_ref::<kannaka_memory::hrm_store::HrmStore>()
                            {
                                let own = h.belief_core_snapshot();
                                if !own.is_empty() {
                                    let _ = transport.ensure_cores_stream();
                                    let payload = serde_json::json!({
                                        "agent_id": my_agent_id,
                                        "core_count": own.len(),
                                        "cores": own,
                                        "created_at": chrono::Utc::now().to_rfc3339(),
                                    });
                                    let _ = transport.publish_cores(&my_agent_id, &payload);
                                }
                            }
                            // (2) fetch peers' cores + couple toward them.
                            match transport.get_peer_cores(None) {
                                Ok(payloads) => {
                                    let mut peer_cores: Vec<kannaka_memory::l6::CoreObs> = Vec::new();
                                    let mut sources = 0usize;
                                    for p in &payloads {
                                        if p.get("agent_id").and_then(|v| v.as_str())
                                            == Some(my_agent_id.as_str())
                                        {
                                            continue; // never couple toward self
                                        }
                                        if let Some(cs) = p.get("cores").and_then(|c| {
                                            serde_json::from_value::<Vec<kannaka_memory::l6::CoreObs>>(
                                                c.clone(),
                                            )
                                            .ok()
                                        }) {
                                            let valid: Vec<_> =
                                                cs.into_iter().filter(|c| c.fp.len() == 16).collect();
                                            if !valid.is_empty() {
                                                sources += 1;
                                                peer_cores.extend(valid);
                                            }
                                        }
                                    }
                                    peer_cores.truncate(1024); // bound O(n×cores) on the 1-core box
                                    if !peer_cores.is_empty() {
                                        // Gentle per-event nudge: small strength/cycles + a
                                        // tight displacement budget so consensus accrues
                                        // gradually across heartbeats, never in one jump.
                                        // Under schedule=alternate the strength swings
                                        // strong/weak per coupling EVENT (odd=strong, so
                                        // the first event consolidates before diversifying).
                                        let couple_event = tick / coupling_ticks;
                                        let event_strength = if coupling_alternate {
                                            if couple_event % 2 == 1 {
                                                coupling_strength_strong
                                            } else {
                                                coupling_strength_weak
                                            }
                                        } else {
                                            coupling_strength_weak
                                        };
                                        let (moved, saved_ok) = sys
                                            .engine
                                            .store
                                            .as_any_mut()
                                            .downcast_mut::<kannaka_memory::hrm_store::HrmStore>()
                                            .map(|h| {
                                                h.couple_belief(
                                                    &peer_cores,
                                                    5,
                                                    event_strength,
                                                    0.2,
                                                    coupling_min_cos,
                                                )
                                            })
                                            .unwrap_or((0, true));
                                        if moved > 0 && !saved_ok {
                                            // Don't report success on a failed persist; the
                                            // next heartbeat flush re-attempts the save.
                                            eprintln!(
                                                "[couple] tick #{tick}: nudged {moved} wavefronts but SAVE FAILED — on-disk .hrm unchanged (retries next flush)"
                                            );
                                        } else if moved > 0 {
                                            println!(
                                                "[couple] tick #{tick}: nudged {moved} wavefronts toward {} cores from {sources} peer(s) (strength {event_strength:.3})",
                                                peer_cores.len()
                                            );
                                        }
                                    }
                                }
                                Err(e) => {
                                    eprintln!("[couple] tick #{tick}: peer cores fetch failed: {e}");
                                }
                            }
                        }
                    }

                    if let Err(e) = transport.announce_leave(&my_agent_id) {
                        eprintln!("[nats] Warning: leave announce failed: {e}");
                    }
                    // Same retraction `swarm leave` performs (#590/#873): this
                    // exit path is the one every Ctrl+C'd `swarm join` takes,
                    // and it was the last clean-shutdown route that left the
                    // durable presence record standing. Freshness filtering
                    // (#737) caps the resulting ghost at ~5 minutes rather
                    // than 24h, but a clean exit knows it is leaving and
                    // should say so rather than lean on the staleness net.
                    if let Err(e) = transport.publish_presence_left(&my_agent_id) {
                        eprintln!(
                            "[nats] Warning: presence retraction failed ({e}) — peers may \
                             show this agent until the presence freshness window expires"
                        );
                    }
                    println!("Left swarm cleanly ({my_agent_id})");
                }
                "leave" => {
                    // `--agent-id` must be honoured here for the same reason
                    // `join` honours it (#589): an operator who started a
                    // throwaway session with `swarm join --agent-id X` has no
                    // other way to retire X. Announcing leave for
                    // `cfg.agent.id` instead would both leave X advertised as
                    // present and wrongly retire the configured default.
                    const LEAVE_USAGE: &str =
                        "Usage: kannaka swarm leave [--agent-id ID] [--nats-url URL]";
                    let mut leave_agent_id = agent_id.clone();
                    let mut i = command_start + 2;
                    while i < args.len() {
                        match args[i].as_str() {
                            "--agent-id" => {
                                leave_agent_id =
                                    flag_value(&args, i, "--agent-id", LEAVE_USAGE).to_string();
                                i += 2;
                            }
                            // `--nats-url` is consumed by resolve_nats_url below.
                            "--nats-url" => i += 2,
                            _ => i += 1,
                        }
                    }
                    let nats_url = resolve_nats_url(&args, command_start, &cfg.swarm.nats_url);
                    if let Some(transport) = try_nats_connect(&nats_url) {
                        if let Err(e) = transport.announce_leave(&leave_agent_id) {
                            eprintln!("[nats] Warning: leave announce failed: {e}");
                        }
                        // #590: the leave ANNOUNCE is an event; it does not touch
                        // the presence record. Without an explicit retraction the
                        // agent stayed in `swarm peers` until the presence
                        // stream's 24h max_age expired it — a full day of ghosts.
                        if let Err(e) = transport.publish_presence_left(&leave_agent_id) {
                            eprintln!(
                                "[nats] Warning: presence retraction failed ({e}) — peers may \
                                 show this agent until the 24h presence TTL expires"
                            );
                        }
                        println!("Left swarm ({leave_agent_id})");
                    } else {
                        eprintln!("Warning: could not connect to NATS to announce leave");
                        println!("Left swarm locally ({leave_agent_id})");
                    }
                }
                "listen" => {
                    let nats_url = resolve_nats_url(&args, command_start, &cfg.swarm.nats_url);
                    let auto_sync = args[command_start..].iter().any(|a| a == "--auto-sync");

                    let transport = match kannaka_memory::nats::SwarmTransport::connect(&nats_url) {
                        Ok(t) => t,
                        Err(e) => {
                            eprintln!("Failed to connect to NATS at {nats_url}: {e}");
                            process::exit(1);
                        }
                    };
                    eprintln!(
                        "[nats] Listening for phase updates on {nats_url} (Ctrl+C to stop)"
                    );
                    if auto_sync {
                        eprintln!(
                            "[nats] Auto-sync enabled -- will run Kuramoto step on each update"
                        );
                    }

                    let mut sub = match transport.subscribe_phases_and_memories(auto_sync) {
                        Ok(s) => s,
                        Err(e) => {
                            eprintln!("Failed to subscribe: {e}");
                            process::exit(1);
                        }
                    };
                    if auto_sync {
                        eprintln!(
                            "[nats] Subscribed to KANNAKA.memory.new and KANNAKA.dreams for sync"
                        );
                    }

                    let mut queen = kannaka_memory::QueenSync::new(
                        kannaka_memory::QueenConfig::default(),
                        &agent_id,
                    );

                    // SECURITY (increment-0 interim; increment-1 replaces this
                    // allowlist gate with signature+trust verification). Load the
                    // swarm-trust config ONCE up front — the listen loop consults
                    // it on every wire message but the config never changes for
                    // the lifetime of the listener.
                    let trust = cfg.swarm_trust.clone();

                    // inc-1b: the corroboration admit() chokepoint state. DORMANT
                    // unless corroboration_gate_enabled AND seeds are pinned, in
                    // which case the memory.new import gate below routes through
                    // admit(). Loaded once — reused across the listen loop.
                    let mut rep_store =
                        kannaka_memory::reputation::RepStore::load(&data_dir(), &cfg.swarm_trust);
                    let mut staging =
                        kannaka_memory::absorb_gate::QuarantineStaging::load(&data_dir());
                    // SECURITY (inc-1b): admit() treats "gate enabled but no live
                    // seeds" as DORMANT (returns Live + sanitized). gate_on MUST
                    // agree — otherwise an enabled gate with an unresolved / typo'd
                    // seed would skip BOTH the inc-0 allowlist and admit's gate,
                    // importing unsigned wire memory: fail-open below the inc-0
                    // baseline. Align the predicate and warn on the misconfig.
                    let gate_on = kannaka_memory::gate_active(&cfg)
                        && rep_store.live_seed_count() > 0;
                    if kannaka_memory::gate_active(&cfg) && rep_store.live_seed_count() == 0 {
                        eprintln!(
                            "[sync] WARNING: corroboration_gate_enabled=true but 0 live seeds \
                             resolved from seed_pubkeys — running DORMANT (inc-0 allowlist) to \
                             avoid fail-open. Pin seeds with `kannaka identity enroll-seed`."
                        );
                    }

                    // PART A anti-eclipse (heartbeat beacons over NATS). Resolve the
                    // pinned seed set ONCE (config is loaded once for the listener's
                    // life). ingest_beacon accepts only seed beacons; when dormant the
                    // set is empty ⇒ every beacon is a NotSeed no-op (no behavior
                    // change). This is the SAME `staging` admit() reads, so an ingested
                    // fresh beacon un-freezes Live promotion.
                    let seed_set: std::collections::HashSet<[u8; 32]> =
                        rep_store.seeds().copied().collect();
                    // A node whose OWN key is a pinned seed emits heartbeats. Only when
                    // the gate is actually armed (gate_on) do we touch the node key, so
                    // dormant nodes keep EXACT inc-0 behavior (no key load, no emit).
                    let beacon_emit: Option<[u8; 32]> = if gate_on {
                        match kannaka_memory::node_signing_key(&data_dir()) {
                            Ok(seed) => {
                                let me = kannaka_memory::verifying_key_bytes(&seed);
                                if seed_set.contains(&me) {
                                    eprintln!(
                                        "[beacon] this node is a seed — emitting heartbeats \u{2264}1/epoch on {}",
                                        kannaka_memory::BEACON_SUBJECT
                                    );
                                    Some(seed)
                                } else {
                                    None
                                }
                            }
                            Err(e) => {
                                eprintln!("[beacon] node key error — not emitting: {e}");
                                None
                            }
                        }
                    } else {
                        None
                    };
                    let mut last_beacon_epoch: Option<u64> = None;

                    // A seed wakes on a per-epoch tick so it can emit even when the
                    // swarm is quiet; a non-seed keeps the prior indefinite block (no
                    // busy-loop, exact prior behavior).
                    if beacon_emit.is_some() {
                        let tick = std::time::Duration::from_millis(
                            cfg.swarm_trust.epoch_length_ms.clamp(1_000, 60_000) as u64,
                        );
                        let _ = sub.set_timeout(Some(tick));
                    } else {
                        let _ = sub.set_timeout(None);
                    }

                    loop {
                        // PART A anti-eclipse EMIT (seed-only, \u{2264}1 per epoch). A node
                        // that is itself a pinned seed publishes a signed heartbeat so an
                        // ARMED gate anywhere on the swarm sees a fresh beacon and
                        // un-freezes Live promotion. Non-seeds never emit.
                        // TODO(ceremony): this listener owns emit + ingest for now. Which
                        // production service is the canonical emitter (this listener vs
                        // `serve` vs a dedicated beacon cron) is a seed-ceremony topology
                        // decision — do NOT rewire serve/worker in this pass.
                        if let Some(seed) = beacon_emit.as_ref() {
                            let now = kannaka_memory::provenance::now_ms();
                            let epoch = kannaka_memory::epoch_now(&cfg, now);
                            if last_beacon_epoch != Some(epoch) {
                                let beacon = kannaka_memory::Beacon::sign(
                                    seed,
                                    epoch,
                                    kannaka_memory::EMPTY_REJECT_ROOT,
                                );
                                match transport.publish_beacon(&beacon) {
                                    Ok(()) => {
                                        last_beacon_epoch = Some(epoch);
                                        // Keep THIS node's own gate fresh regardless of
                                        // whether the broadcast echoes back to us.
                                        let _ = staging.ingest_beacon(&beacon, &seed_set, epoch);
                                        let _ = staging.save();
                                        eprintln!("[beacon] published heartbeat for epoch {epoch}");
                                    }
                                    Err(e) => eprintln!(
                                        "[beacon] publish failed for epoch {epoch}: {e}"
                                    ),
                                }
                            }
                        }

                        let msg = match sub.next_event() {
                            kannaka_memory::nats::SubEvent::Msg(m) => m,
                            kannaka_memory::nats::SubEvent::Timeout => continue,
                            // EOF / fatal error — break to the exit below so
                            // systemd Restart=on-failure restarts the listener
                            // (pre-fix this exited 0 on connection close).
                            kannaka_memory::nats::SubEvent::Closed => break,
                        };
                        if msg.subject.starts_with("QUEEN.phase.") {
                            if let Some(phase) = msg.as_phase() {
                                println!("[{}] \u{03b8}={:.3} \u{03c9}={:.3} coherence={:.3} phi={:.3} memories={}",
                                    kannaka_memory::sanitize_display(&phase.agent_id), phase.phase, phase.frequency,
                                    phase.coherence, phase.phi, phase.memory_count);

                                // SECURITY (increment-0 interim; increment-1 replaces this
                                // allowlist gate with signature+trust verification). Only let a
                                // TRUSTED wire phase drive a pairwise Kuramoto sync step — self is
                                // already excluded by `!= agent_id`; the gate is a no-op when the
                                // escape hatch KANNAKA_METRICS_TRUSTED_ONLY=0 is set.
                                if auto_sync
                                    && phase.agent_id != agent_id
                                    && kannaka_memory::wire_source_trusted(
                                        &phase.agent_id,
                                        &trust.trusted_agents,
                                        trust.metrics_trusted_only,
                                    )
                                {
                                    let my_phase =
                                        queen.to_agent_phase(0, sys.engine.store.count(), 0);
                                    let swarm = vec![my_phase, phase];
                                    let state = queen.queen_sync_step(&swarm);
                                    println!(
                                        "  -> synced: r={:.3} psi={:.3} K={:.3}",
                                        state.order_parameter,
                                        state.mean_phase,
                                        state.coupling_strength
                                    );
                                }
                            }
                        } else if msg.subject == "QUEEN.announce" {
                            if let Some(json) = msg.as_json() {
                                let event = json["event"].as_str().unwrap_or("unknown");
                                let agent = json["agent_id"].as_str().unwrap_or("?");
                                // Both fields are wire-sourced — sanitize before printing.
                                println!("[announce] {} {}",
                                    kannaka_memory::sanitize_display(agent),
                                    kannaka_memory::sanitize_display(event));
                            }
                        } else if msg.subject == "KANNAKA.memory.new" && auto_sync {
                            if let Some(json) = msg.as_json() {
                                let source_agent = json["agent_id"].as_str().unwrap_or("?");
                                // Skip our own messages
                                if source_agent != agent_id {
                                    if let Some(mem_json) = json.get("memory") {
                                        match serde_json::from_value::<kannaka_memory::HyperMemory>(
                                            mem_json.clone(),
                                        ) {
                                            Ok(mut mem) => {
                                                let mem_id = mem.id;
                                                // Check if memory already exists
                                                match sys.engine.store.get(&mem_id) {
                                                    Ok(Some(_)) => {
                                                        eprintln!("[sync] Memory {mem_id} already exists, skipping");
                                                    }
                                                    _ => {
                                                        // inc-1b: route the wire import through the corroboration
                                                        // admit() chokepoint. DORMANT ⇒ admit returns Live and we
                                                        // keep the inc-0 allowlist gate below unchanged; only the
                                                        // sanitized fields (amplitude/phase/frequency clamp +
                                                        // hallucinated forced to local default) are newly applied.
                                                        // ACTIVE ⇒ admit's decision governs (Live/Quarantine/Drop).
                                                        let prov_sig: Option<kannaka_memory::ProvenanceSig> = json
                                                            .get("provenance_sig")
                                                            .and_then(|v| serde_json::from_value(v.clone()).ok());
                                                        let now = kannaka_memory::provenance::now_ms();
                                                        let (decision, clean, pending) = kannaka_memory::admit(
                                                            &mem.content,
                                                            mem.amplitude,
                                                            mem.phase,
                                                            mem.frequency,
                                                            mem.hallucinated,
                                                            kannaka_memory::SUBJECT_MEMORY_NEW,
                                                            mem_id,
                                                            prov_sig.as_ref(),
                                                            &mut staging,
                                                            &mut rep_store,
                                                            &cfg,
                                                            now,
                                                        );
                                                        // Apply sanitized fields regardless of gate state.
                                                        clean.apply(&mut mem);
                                                        use kannaka_memory::AdmitDecision::*;
                                                        match decision {
                                                            Live => {
                                                                // Dormant: preserve the inc-0 allowlist gate. Active:
                                                                // corroboration already authorized the import.
                                                                let admit_import = if gate_on {
                                                                    true
                                                                } else {
                                                                    kannaka_memory::wire_source_trusted(source_agent, &trust.trusted_agents, trust.metrics_trusted_only)
                                                                };
                                                                if admit_import {
                                                                    // Bound as a local so the mutable borrow of `sys`
                                                                    // ends here and `sys.save()` can run in the arm below.
                                                                    let insert_result = sys.engine.store.insert(mem);
                                                                    match insert_result {
                                                                        Ok(_) => {
                                                                            // #8: commit the pending promotion ONLY after the
                                                                            // medium insert succeeds, so the DAG ledger and the
                                                                            // live medium commit together (no-op when dormant).
                                                                            kannaka_memory::commit_promotion(pending, &mut rep_store, &mut staging, &cfg);
                                                                            // Persist. `insert` only puts the memory in the
                                                                            // in-process store — every other mutating path in
                                                                            // this binary pairs it with an explicit save (see
                                                                            // the prune path's "Failed to persist HRM"). Without
                                                                            // this the import survived only until restart, so a
                                                                            // long-running `swarm listen --auto-sync` looked
                                                                            // like it was syncing while nothing durable landed.
                                                                            // `save()` is `store.flush()` — incremental, and a
                                                                            // no-op when nothing is pending. (#647)
                                                                            if let Err(e) = sys.save() {
                                                                                eprintln!("[sync] Imported memory {mem_id} but FAILED to persist it: {e} — it will be lost on restart");
                                                                            } else {
                                                                                println!("[sync] Imported memory {} from {}", mem_id, kannaka_memory::sanitize_display(source_agent));
                                                                            }
                                                                        }
                                                                        Err(e) => {
                                                                            // #8: insert failed — DROP the pending (do not commit)
                                                                            // so the content re-decides on retry, no ledger/medium divergence.
                                                                            eprintln!("[sync] Failed to import memory {} from {}: {}", mem_id, kannaka_memory::sanitize_display(source_agent), e);
                                                                        }
                                                                    }
                                                                } else {
                                                                    eprintln!("[sync] skip untrusted source {} (increment-0 gate)", kannaka_memory::sanitize_display(source_agent));
                                                                }
                                                            }
                                                            Quarantine | ProbationLive => {
                                                                eprintln!("[sync] quarantined memory {} from {} (awaiting corroboration)", mem_id, kannaka_memory::sanitize_display(source_agent));
                                                            }
                                                            Drop => {
                                                                eprintln!("[sync] dropped memory {} from {} (invalid signature / echo)", mem_id, kannaka_memory::sanitize_display(source_agent));
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                            Err(e) => {
                                                eprintln!("[sync] Failed to deserialize memory from {}: {}", kannaka_memory::sanitize_display(source_agent), e);
                                            }
                                        }
                                    }
                                }
                            }
                        } else if msg.subject == "KANNAKA.dreams" && auto_sync {
                            if let Some(json) = msg.as_json() {
                                let source_agent = json["agent_id"].as_str().unwrap_or("?");
                                if source_agent != agent_id {
                                    let cycles = json["cycles"].as_u64().unwrap_or(0);
                                    let strengthened =
                                        json["memories_strengthened"].as_u64().unwrap_or(0);
                                    let pruned = json["memories_pruned"].as_u64().unwrap_or(0);
                                    // KANNAKA.dreams is print-only (no store insert), so the wire
                                    // source just needs sanitizing before it reaches the terminal.
                                    println!("[dream] {} completed dream: {} cycles, {} strengthened, {} pruned",
                                        kannaka_memory::sanitize_display(source_agent), cycles, strengthened, pruned);
                                }
                            }
                        } else if msg.subject == kannaka_memory::BEACON_SUBJECT
                            && auto_sync
                            && !seed_set.is_empty()
                        {
                            // PART A anti-eclipse RECEIVE: verify + ingest a seed
                            // heartbeat into the SAME `staging` admit() reads. Once a
                            // fresh seed beacon lands, admit()'s freshness check passes
                            // and Live promotion un-freezes. ingest_beacon rejects
                            // non-seed / future / replayed epochs. Fully dormant (no
                            // seeds pinned) ⇒ seed_set empty ⇒ this arm is skipped and
                            // beacons are ignored silently (no behavior change, no noise).
                            if let Some(beacon) = msg
                                .as_json()
                                .and_then(|v| serde_json::from_value::<kannaka_memory::Beacon>(v).ok())
                            {
                                let now = kannaka_memory::provenance::now_ms();
                                let now_epoch = kannaka_memory::epoch_now(&cfg, now);
                                match staging.ingest_beacon(&beacon, &seed_set, now_epoch) {
                                    Ok(pk) => {
                                        let _ = staging.save();
                                        let pk_b64 = b64_encode_std(&pk);
                                        eprintln!(
                                            "[beacon] fresh seed beacon epoch {} from {}",
                                            beacon.epoch,
                                            &pk_b64[..pk_b64.len().min(12)]
                                        );
                                    }
                                    // Our own broadcast echo / a replay re-ingests as
                                    // Stale — expected, kept quiet.
                                    Err(kannaka_memory::BeaconReject::Stale) => {}
                                    Err(e) => eprintln!("[beacon] rejected: {e}"),
                                }
                            }
                        }
                    }
                    eprintln!("[nats] Connection closed — exiting for restart");
                    process::exit(1);
                }
                "status" => {
                    let nats_url = resolve_nats_url(&args, command_start, &cfg.swarm.nats_url);
                    // Derive local phase from HRM state
                    let mut queen = kannaka_memory::QueenSync::new(
                        kannaka_memory::QueenConfig::default(),
                        &agent_id,
                    );
                    queen.derive_local_state(&sys.engine);
                    let local_phase = queen.to_agent_phase(0, sys.engine.store.count(), 0);

                    let mut nats_status = serde_json::json!("disconnected");
                    let mut peer_count = 0usize;
                    match try_nats_connect(&nats_url) {
                        Some(transport) => {
                            let nats_phases = transport.get_all_phases().unwrap_or_default();
                            // LIVENESS: the JetStream read path retains the LAST
                            // phase per agent forever, so a retained read includes
                            // long-departed agents. A peer is an agent the BROKER
                            // heard in the last 5 minutes (mirrors the roster KV
                            // TTL) — server ingest time, identity from the
                            // subject, so a publisher with a skewed clock or a
                            // non-AgentPhase payload (kannaktopus) still counts.
                            // The live-gossip fallback always had this liveness
                            // semantic by construction.
                            let live_agents = if transport.has_jetstream() {
                                transport
                                    .live_phase_agents(chrono::Duration::minutes(5))
                                    .ok()
                            } else {
                                None
                            };
                            // Parsed phases feed the TRUSTED count; freshness-
                            // filter them too (payload clock — conforming agents
                            // publish honest timestamps) so stale retained
                            // entries can't stay "trusted" forever.
                            let now = chrono::Utc::now();
                            let nats_phases: Vec<_> = nats_phases
                                .into_iter()
                                .filter(|p| {
                                    now.signed_duration_since(p.timestamp)
                                        < chrono::Duration::minutes(5)
                                })
                                .collect();
                            // SECURITY (increment-0): the open NATS swarm lets
                            // anyone publish an AgentPhase. Report BOTH counts
                            // truthfully — `peer_count` is every phase observed
                            // on the wire (raw, unfiltered), `trusted_peer_count`
                            // is the allowlisted (+ our own) subset that actually
                            // feeds the sync/queen metrics — so a forged
                            // `agent-<hex>` flood shows up as the gap between the
                            // two instead of silently inflating one number.
                            // Escape hatch KANNAKA_METRICS_TRUSTED_ONLY=0 makes
                            // trusted_peer_count == peer_count.
                            // #808: a peer is somebody ELSE. Both sources
                            // include our own phase — we publish it ourselves,
                            // and the broker heard it — so a solo node
                            // reported `peers: 1` and disagreed with
                            // `swarm peers`, which derives from presence and
                            // says "No peers in the swarm yet." Anything
                            // reading `swarm.peers` as a remote-neighbour
                            // count was overestimating connectivity by exactly
                            // one, always.
                            peer_count = match live_agents.as_ref() {
                                Some(v) => kannaka_memory::count_peers_excluding_self(
                                    v.iter().map(|s| s.as_str()),
                                    &agent_id,
                                ),
                                None => kannaka_memory::count_peers_excluding_self(
                                    nats_phases.iter().map(|p| p.agent_id.as_str()),
                                    &agent_id,
                                ),
                            };
                            let trusted_peer_count = if cfg.swarm_trust.metrics_trusted_only {
                                // filter_wire_phases keeps `self_id` on purpose —
                                // it feeds the sync/queen metrics, which must
                                // include us. As a PEER count it must not, or
                                // the pair stops being comparable: the gap
                                // between raw and trusted is supposed to show a
                                // forged-id flood, not a constant off-by-one.
                                kannaka_memory::filter_wire_phases(
                                    nats_phases,
                                    &agent_id,
                                    &cfg.swarm_trust.trusted_agents,
                                    cfg.swarm_trust.wire_trust_cap,
                                )
                                .iter()
                                .filter(|p| p.agent_id != agent_id)
                                .count()
                            } else {
                                peer_count
                            };
                            nats_status = serde_json::json!({
                                "connected": true,
                                "url": nats_url,
                                // `peers` kept as a back-compat alias (== raw observed)
                                // for existing consumers; `peer_count`/`trusted_peer_count`
                                // are the increment-0 pair.
                                "peers": peer_count,
                                "peer_count": peer_count,
                                "trusted_peer_count": trusted_peer_count,
                            });
                        }
                        None => {
                            nats_status = serde_json::json!({
                                "connected": false,
                                "url": nats_url,
                            });
                        }
                    }

                    let output = serde_json::json!({
                        "agent_id": agent_id,
                        "local_phase": {
                            "phase": local_phase.phase,
                            "frequency": local_phase.frequency,
                            "coherence": local_phase.coherence,
                            "phi": local_phase.phi,
                            "memory_count": local_phase.memory_count,
                            "left_coherence": local_phase.left_coherence,
                            "right_coherence": local_phase.right_coherence,
                            "bridge_activity": local_phase.bridge_activity,
                            "dream_state": local_phase.dream_state,
                        },
                        "swarm": {
                            "peers": peer_count,
                        },
                        "nats": nats_status,
                    });
                    println!("{}", serde_json::to_string_pretty(&output).unwrap());
                }
                "sync" => {
                    let nats_url = resolve_nats_url(&args, command_start, &cfg.swarm.nats_url);
                    let transport = match kannaka_memory::nats::SwarmTransport::connect(&nats_url) {
                        Ok(t) => t,
                        Err(e) => {
                            eprintln!("Failed to connect to NATS at {nats_url}: {e}");
                            process::exit(1);
                        }
                    };

                    let nats_phases = transport.get_all_phases().unwrap_or_default();
                    // SECURITY (increment-0): gate wire phases before they
                    // weight the Kuramoto sync/metric — only allowlisted
                    // (+ our own) ids count, wire trust_score capped. Escape
                    // hatch: KANNAKA_METRICS_TRUSTED_ONLY=0.
                    let nats_phases = if cfg.swarm_trust.metrics_trusted_only {
                        kannaka_memory::filter_wire_phases(
                            nats_phases,
                            &agent_id,
                            &cfg.swarm_trust.trusted_agents,
                            cfg.swarm_trust.wire_trust_cap,
                        )
                    } else {
                        nats_phases
                    };
                    let mut queen = kannaka_memory::QueenSync::new(
                        kannaka_memory::QueenConfig::default(),
                        &agent_id,
                    );
                    queen.derive_local_state(&sys.engine);

                    if nats_phases.is_empty() {
                        // BOOTSTRAP: hearing no one must not silence us — the
                        // first node's sync IS its announcement. The old
                        // exit(1)-without-publishing deadlocked an empty swarm
                        // (two such nodes wait on each other forever) and made
                        // the witness invisible for weeks when its JS read
                        // lane closed: every tick heard nothing, so every
                        // tick published nothing.
                        let phase = queen.to_agent_phase(0, sys.engine.store.count(), 0);
                        if let Err(e) = transport.publish_phase(&phase) {
                            eprintln!("[nats] bootstrap phase publish failed: {e}");
                            process::exit(1);
                        }
                        eprintln!(
                            "No peer phases heard — published our own phase so the swarm can find us."
                        );
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&serde_json::json!({
                                "bootstrap_announce": true,
                                "peers_heard": 0,
                                "agent_id": agent_id,
                            }))
                            .unwrap()
                        );
                    } else {
                        // #581: include OURSELVES in the swarm we compute over.
                        // `queen_sync_step` takes `agent_count = swarm.len()` and
                        // looks up local handedness by searching `swarm` for
                        // `self.agent_id`, so passing peer phases alone silently
                        // excluded this agent from its own Queen state — an
                        // off-by-one on every metric, and no local handedness.
                        let swarm = with_local_phase(&nats_phases, &queen, &sys);
                        let state = queen.queen_sync_step(&swarm);

                        // Publish updated phase back to NATS
                        let updated_phase =
                            queen.to_agent_phase(0, sys.engine.store.count(), 0);
                        if let Err(e) = transport.publish_phase(&updated_phase) {
                            eprintln!("[nats] Warning: failed to publish updated phase: {e}");
                        }

                        println!("{}", serde_json::to_string_pretty(&state).unwrap());
                    }
                }
                "queen" => {
                    let nats_url = resolve_nats_url(&args, command_start, &cfg.swarm.nats_url);
                    let transport = match kannaka_memory::nats::SwarmTransport::connect(&nats_url) {
                        Ok(t) => t,
                        Err(e) => {
                            eprintln!("Failed to connect to NATS at {nats_url}: {e}");
                            process::exit(1);
                        }
                    };

                    let nats_phases = transport.get_all_phases().unwrap_or_default();
                    // SECURITY (increment-0): gate wire phases before they
                    // weight the queen metric — only allowlisted (+ our own)
                    // ids count, wire trust_score capped. Escape hatch:
                    // KANNAKA_METRICS_TRUSTED_ONLY=0.
                    let nats_phases = if cfg.swarm_trust.metrics_trusted_only {
                        kannaka_memory::filter_wire_phases(
                            nats_phases,
                            &agent_id,
                            &cfg.swarm_trust.trusted_agents,
                            cfg.swarm_trust.wire_trust_cap,
                        )
                    } else {
                        nats_phases
                    };
                    if nats_phases.is_empty() {
                        eprintln!(
                            "No swarm phases found. Run 'swarm publish' and 'swarm sync' first."
                        );
                        process::exit(1);
                    }

                    // Compute queen state from current NATS phases
                    let mut queen = kannaka_memory::QueenSync::new(
                        kannaka_memory::QueenConfig::default(),
                        &agent_id,
                    );
                    queen.derive_local_state(&sys.engine);
                    // #581 — see the note at the `sync` call site.
                    let swarm = with_local_phase(&nats_phases, &queen, &sys);
                    let state = queen.queen_sync_step(&swarm);
                    println!("{}", serde_json::to_string_pretty(&state).unwrap());
                }
                "hives" => {
                    let nats_url = resolve_nats_url(&args, command_start, &cfg.swarm.nats_url);
                    let transport = match kannaka_memory::nats::SwarmTransport::connect(&nats_url) {
                        Ok(t) => t,
                        Err(e) => {
                            eprintln!("Failed to connect to NATS at {nats_url}: {e}");
                            process::exit(1);
                        }
                    };

                    let nats_phases = transport.get_all_phases().unwrap_or_default();
                    // SECURITY (increment-0): gate wire phases before hive
                    // detection weights them — only allowlisted (+ our own)
                    // ids count, wire trust_score capped. Escape hatch:
                    // KANNAKA_METRICS_TRUSTED_ONLY=0.
                    let nats_phases = if cfg.swarm_trust.metrics_trusted_only {
                        kannaka_memory::filter_wire_phases(
                            nats_phases,
                            &agent_id,
                            &cfg.swarm_trust.trusted_agents,
                            cfg.swarm_trust.wire_trust_cap,
                        )
                    } else {
                        nats_phases
                    };
                    if nats_phases.is_empty() {
                        eprintln!(
                            "No swarm phases found. Run 'swarm publish' and 'swarm sync' first."
                        );
                        process::exit(1);
                    }

                    let queen = kannaka_memory::QueenSync::new(
                        kannaka_memory::QueenConfig::default(),
                        &agent_id,
                    );
                    let hive_infos = queen.detect_hives_domain_aware(&nats_phases);
                    print!(
                        "{}",
                        kannaka_memory::QueenSync::format_hive_topology(&hive_infos)
                    );
                    // Also output JSON for machine consumption
                    eprintln!("\n--- JSON ---");
                    eprintln!("{}", serde_json::to_string_pretty(&hive_infos).unwrap());
                }
                "publish" => {
                    let nats_url = resolve_nats_url(&args, command_start, &cfg.swarm.nats_url);
                    let transport = match kannaka_memory::nats::SwarmTransport::connect(&nats_url) {
                        Ok(t) => t,
                        Err(e) => {
                            eprintln!("Failed to connect to NATS at {nats_url}: {e}");
                            process::exit(1);
                        }
                    };

                    let mut queen = kannaka_memory::QueenSync::new(
                        kannaka_memory::QueenConfig::default(),
                        &agent_id,
                    );
                    queen.derive_local_state(&sys.engine);
                    let phase = queen.to_agent_phase(0, sys.engine.store.count(), 0);
                    match transport.publish_phase(&phase) {
                        Ok(()) => println!(
                            "Published phase: \u{03b8}={:.3}, \u{03c9}={:.3}, coherence={:.3}",
                            phase.phase, phase.frequency, phase.coherence
                        ),
                        Err(e) => {
                            eprintln!("Error: {e}");
                            process::exit(1);
                        }
                    }
                }
                "exemplars" => {
                    handle_swarm_exemplars(&mut sys, &cfg, &args[command_start..]);
                }
                "cores" => {
                    handle_swarm_cores(&mut sys, &cfg, &args[command_start..]);
                }
                "peers" => {
                    handle_swarm_peers(&cfg, &args[command_start..]);
                }
                "autoabsorb" => {
                    handle_swarm_autoabsorb(&mut sys, &cfg, &args[command_start..]);
                }
                "enqueue" => {
                    handle_swarm_enqueue(&cfg, &args[command_start..]);
                }
                "worker" => {
                    handle_swarm_worker(&mut sys, &cfg, &args[command_start..]);
                }
                "absorb" => {
                    handle_swarm_absorb(&mut sys, &cfg, &args[command_start..]);
                }
                "serve" => {
                    // ADR-0026 Phase 1: long-running listener for KANNAKA.ask.<id>
                    // and KANNAKA.ask.broadcast. Each inbound message is dispatched
                    // to agent::ask_notools_ex; the reply goes back on the message's
                    // reply-to subject.
                    handle_swarm_serve(&mut sys, &cfg, &args[command_start..]);
                }
                "tail" => {
                    handle_swarm_tail(&cfg, &args[command_start..]);
                }
                // `activate-gate` and `beacon` are handled by the pre-HRM
                // short-circuit above (they don't need the memory system); listed
                // here for discoverability if that guard is ever bypassed.
                "activate-gate" => handle_swarm_activate_gate(&args[command_start..]),
                "beacon" => handle_swarm_beacon(&args[command_start..]),
                other => {
                    eprintln!("Unknown swarm command: {other}");
                    eprintln!("Usage: kannaka swarm <join|status|sync|queen|hives|publish|leave|listen|serve|tail|exemplars|cores|peers|absorb|autoabsorb|enqueue|worker|activate-gate|beacon>");
                    process::exit(1);
                }
            }
        }

        #[cfg(feature = "nats")]
        "events" => {
            // ADR-0028 — event-sourced HRM + time machine.
            // Subcommands:
            //   init      — create JetStream streams for memory/substrate/snapshots
            //   snapshot  — capture + publish a gzipped HRM snapshot (Phase 2).
            //               --interval N runs as a daemon at N-second cadence.
            if args.len() < command_start + 2 {
                eprintln!("Usage: kannaka events <init|snapshot [--interval SECS]|list-snapshots [--agent ID] [--json]|restore [--agent ID] [--from PATH|--from-url URL] [--dry-run]|gc [--corrupt-backs] [--older-than DAYS] [--dry-run]>");
                process::exit(1);
            }
            match args[command_start + 1].as_str() {
                "init" => {
                    handle_events_init(&cfg, &args[command_start..]);
                }
                "snapshot" => {
                    handle_events_snapshot(&mut sys, &cfg, &args[command_start..]);
                }
                "list-snapshots" => {
                    handle_events_list_snapshots(&cfg, &args[command_start..]);
                }
                "restore" => {
                    handle_events_restore(&cfg, &args[command_start..]);
                }
                "gc" => {
                    handle_events_gc(&cfg, &args[command_start..]);
                }
                other => {
                    eprintln!("Unknown events command: {other}");
                    eprintln!("Usage: kannaka events <init|snapshot [--interval SECS]|list-snapshots [--agent ID] [--json]|restore [--agent ID] [--from PATH|--from-url URL] [--dry-run]|gc [--corrupt-backs] [--older-than DAYS] [--dry-run]>");
                    process::exit(1);
                }
            }
        }

        #[cfg(feature = "nats")]
        "substrate" => {
            // ADR-0027 — kannaka-prime as the 96-class collective substrate.
            // Subcommands:
            //   init      — seat 96 anchor wavefronts (Phase 1.b) so the
            //               substrate's HRM has a determined topology that
            //               subsequent absorbs flow into
            //   run       — long-running absorb listener + periodic phi publish
            //   backfill  — walk local HRM and emit one substrate.absorb
            //               event per memory (Phase 2)
            if args.len() < command_start + 2 {
                eprintln!("Usage: kannaka substrate <init|run|backfill|status [--wait SECS]>");
                process::exit(1);
            }
            match args[command_start + 1].as_str() {
                "init" => {
                    handle_substrate_init(&mut sys, &args[command_start..]);
                }
                "run" => {
                    handle_substrate_run(&mut sys, &cfg, &args[command_start..]);
                }
                "backfill" => {
                    handle_substrate_backfill(&mut sys, &cfg, &args[command_start..]);
                }
                "status" => {
                    handle_substrate_status(&cfg, &args[command_start..]);
                }
                other => {
                    eprintln!("Unknown substrate command: {other}");
                    eprintln!("Usage: kannaka substrate <init|run|backfill|status [--wait SECS]>");
                    process::exit(1);
                }
            }
        }

        #[cfg(feature = "nats")]
        "facets" => {
            // #836 / ADR-0049: corpus facet migration. Dry-run by default.
            handle_facets(&mut sys, &cfg, &args[command_start + 1..]);
        }

        "dedupe" => {
            // Reinforce-on-repeat cleanup: fold byte-identical duplicate sets
            // into one memory that carries the count. Dry-run by default;
            // never runs on a schedule.
            handle_dedupe(&mut sys, &cfg, &args[command_start + 1..]);
        }
        "attention" => {
            if args.len() < command_start + 2 {
                eprintln!("Usage: kannaka attention <serve|stats>");
                process::exit(1);
            }
            match args[command_start + 1].as_str() {
                "serve" => {
                    handle_attention_serve(&mut sys, &cfg, &args[command_start..]);
                }
                "stats" => {
                    // #114: real cross-process stats. The serve loop dumps live
                    // beam state to KANNAKA_ATTENTION_BEAM_FILE every iteration;
                    // read+reflect it here instead of a hardcoded zero stub. If
                    // no serve process has written the file, say so plainly
                    // (exit 0 — "offline" is a truthful answer) rather than
                    // reporting fake zeroes as if a live beam existed.
                    let dump_path =
                        std::env::var("KANNAKA_ATTENTION_BEAM_FILE").unwrap_or_else(|_| {
                            if cfg!(windows) {
                                "C:\\Users\\Public\\kannaka-attention-beam.json".to_string()
                            } else {
                                "/tmp/kannaka-attention-beam.json".to_string()
                            }
                        });
                    match std::fs::read_to_string(&dump_path) {
                        Ok(s) => match serde_json::from_str::<serde_json::Value>(&s) {
                            Ok(dump) => {
                                let st = dump
                                    .get("stats")
                                    .cloned()
                                    .unwrap_or_else(|| serde_json::json!({}));
                                let u = |k: &str| st.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
                                let out = serde_json::json!({
                                    "beam_size": u("beam_size"),
                                    "recency_len": u("recency_len"),
                                    "lookback_len": u("lookback_len"),
                                    "landmarks_len": u("landmarks_len"),
                                    "observations": u("observations"),
                                    "candidates": dump.get("candidates").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0),
                                    "ts": dump.get("ts").and_then(|v| v.as_str()).unwrap_or(""),
                                    "source": dump_path,
                                });
                                println!("{out}");
                            }
                            Err(e) => {
                                let out = serde_json::json!({
                                    "error": format!("failed to parse beam dump: {e}"),
                                    "source": dump_path,
                                });
                                println!("{out}");
                            }
                        },
                        Err(_) => {
                            let out = serde_json::json!({
                                "beam_size": 0, "recency_len": 0, "lookback_len": 0,
                                "landmarks_len": 0, "observations": 0,
                                "note": format!("no attention serve process has written {dump_path} — beam offline"),
                            });
                            println!("{out}");
                        }
                    }
                }
                other => {
                    eprintln!("Unknown attention command: {other}");
                    eprintln!("Usage: kannaka attention <serve|stats>");
                    process::exit(1);
                }
            }
        }

        #[cfg(feature = "nats")]
        "inbox" => {
            if args.len() < command_start + 2 {
                eprintln!("Usage: kannaka inbox <send|serve|tail> [args...]");
                process::exit(1);
            }
            match args[command_start + 1].as_str() {
                "send" => handle_inbox_send(&cfg, &args[command_start..]),
                "serve" => handle_inbox_serve(&cfg, &args[command_start..]),
                "tail" => handle_inbox_tail(&cfg, &args[command_start..]),
                other => {
                    eprintln!("Unknown inbox command: {other}");
                    eprintln!("Usage: kannaka inbox <send|serve|tail> [args...]");
                    process::exit(1);
                }
            }
        }
        #[cfg(not(feature = "nats"))]
        "inbox" => {
            eprintln!("kannaka inbox requires the `nats` feature");
            process::exit(1);
        }

        "voice" => {
            voice_command(&args[command_start..], &mut sys);
        }

        "ask" => {
            handle_ask(&mut sys, &cfg, &args[command_start..]);
        }

        "chat" => {
            handle_chat(&mut sys, &cfg, &args[command_start..]);
        }

        "agent" => {
            handle_agent(&mut sys, &cfg, &args[command_start..]);
        }

        "invariant" => {
            let tolerance = match args.get(command_start + 1) {
                None => 0.1,
                Some(v) => match v.parse() {
                    Ok(t) => t,
                    Err(_) => {
                        eprintln!("invariant: tolerance expects a number, got: {v}");
                        eprintln!("Usage: kannaka invariant [TOLERANCE]");
                        process::exit(2);
                    }
                },
            };

            match sys.invariant_clusters(tolerance) {
                Ok(clusters) => {
                    println!("δ-Invariant Memory Clusters (tolerance: {tolerance}):");
                    println!("═════════════════════════════════════════");

                    for (i, cluster) in clusters.iter().enumerate() {
                        println!(
                            "Cluster {}: δ={:.3}, coherence={:.3}, {} memories",
                            i + 1,
                            cluster.representative_delta,
                            cluster.coherence,
                            cluster.memory_ids.len()
                        );

                        for &memory_id in &cluster.memory_ids {
                            if let Ok(Some(memory)) = sys.get_memory(&memory_id) {
                                let preview = if memory.content.len() > 60 {
                                    format!(
                                        "{}...",
                                        &memory.content[..memory.content.floor_char_boundary(60)]
                                    )
                                } else {
                                    memory.content.clone()
                                };
                                println!("  {memory_id} | {preview}");
                            }
                        }
                        println!();
                    }

                    if clusters.is_empty() {
                        println!("No δ-clusters found. Try a larger tolerance or ensure you have enough memories.");
                    }
                }
                Err(e) => {
                    // Errors must not exit 0 — scripted callers couldn't
                    // tell failure from "no clusters".
                    eprintln!("Error computing invariant clusters: {e}");
                    process::exit(1);
                }
            }
        }

        "cmf" => {
            match sys.detect_cmfs() {
                Ok(cmfs) => {
                    println!("Conservative Memory Fields Detected:");
                    println!("═══════════════════════════════════");

                    if cmfs.is_empty() {
                        println!("No Conservative Memory Fields detected.");
                        println!("CMFs require at least 3 memories per cluster and path-independent structure.");
                    } else {
                        for (i, cmf) in cmfs.iter().enumerate() {
                            println!("CMF {} ({}): explanatory_power={:.2}, basis_vectors={}, path_deviation={:.3}",
                                     i + 1, cmf.id, cmf.explanatory_power, 
                                     cmf.basis_vectors.len(), cmf.path_constraints.max_deviation);

                            println!(
                                "  Trajectory: step_size={:.3}, curvature={:.3}",
                                cmf.trajectory_params.step_size,
                                cmf.trajectory_params.curvature.get(0).unwrap_or(&0.0)
                            );

                            println!(
                                "  Path independence: {} verified paths",
                                cmf.path_constraints.verified_paths.len()
                            );

                            // Test a few memories against this CMF
                            if let Ok(all_memories) = sys.all_memories() {
                                println!("  Sample memberships:");
                                for (_j, memory) in all_memories.iter().take(5).enumerate() {
                                    let membership = kannaka_memory::cmf_membership(memory, cmf);
                                    if membership.fitness > 0.1 {
                                        let preview = if memory.content.len() > 40 {
                                            format!(
                                                "{}...",
                                                &memory.content
                                                    [..memory.content.floor_char_boundary(40)]
                                            )
                                        } else {
                                            memory.content.clone()
                                        };
                                        println!(
                                            "    {} | fitness={:.2} | {}",
                                            memory.id, membership.fitness, preview
                                        );
                                    }
                                }
                            }
                            println!();
                        }
                    }
                }
                Err(e) => {
                    // Errors must not exit 0 — scripted callers couldn't
                    // tell failure from "no CMFs detected".
                    eprintln!("Error detecting CMFs: {e}");
                    process::exit(1);
                }
            }
        }

        "audit-modality" => {
            audit_modality_command(&mut sys);
        }

        "modality-axes" => {
            modality_axes_command(&sys);
        }

        "search" => {
            // Read-only — no &mut needed (literal text search bypasses
            // resonance + observation; see openclaw::search).
            handle_search(&sys, &args[command_start..]);
        }

        "export" => {
            handle_export(&mut sys, &args[command_start..]);
        }

        "import" => {
            handle_import(&mut sys, &args[command_start..]);
        }

        _ => usage(),
    }
}

// ---------------------------------------------------------------------------
// Retroactive modality audit (NCS Phase 1.3)
// ---------------------------------------------------------------------------

fn audit_modality_command(sys: &mut kannaka_memory::openclaw::KannakaMemorySystem) {
    use kannaka_memory::medium::types::{detect_modality, ModalityClassification};
    use std::collections::HashMap;

    let all_mems = match sys.engine.store.all_memories() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("Error reading memories: {e}");
            process::exit(1);
        }
    };

    let total = all_mems.len();
    if total == 0 {
        eprintln!("No memories to audit.");
        return;
    }

    eprintln!(
        "[audit-modality] Starting retroactive modality audit of {total} memories"
    );

    // Classify every memory and collect results before mutation
    struct AuditEntry {
        id: uuid::Uuid,
        content_preview: String,
        classification: ModalityClassification,
    }

    let mut entries: Vec<AuditEntry> = Vec::with_capacity(total);
    for (i, mem) in all_mems.iter().enumerate() {
        let classification = detect_modality(&mem.content);
        let preview = if mem.content.len() > 60 {
            let mut end = 60;
            while end > 0 && !mem.content.is_char_boundary(end) {
                end -= 1;
            }
            format!("{}...", &mem.content[..end])
        } else {
            mem.content.clone()
        };
        entries.push(AuditEntry {
            id: mem.id,
            content_preview: preview,
            classification,
        });
        if (i + 1) % 50 == 0 {
            eprintln!("[audit-modality] Classified {}/{} memories", i + 1, total);
        }
    }

    // Apply classifications in-place via HrmStore
    let hrm = match sys
        .engine
        .store
        .as_any_mut()
        .downcast_mut::<kannaka_memory::hrm_store::HrmStore>()
    {
        Some(h) => h,
        None => {
            eprintln!("Error: audit-modality requires HRM backend");
            process::exit(1);
        }
    };

    let mut updated = 0usize;
    for entry in &entries {
        hrm.set_modality(&entry.id, entry.classification.modality);
        updated += 1;
    }

    // Flush to persist
    if let Err(e) = hrm.flush() {
        eprintln!("Warning: failed to flush after audit: {e}");
    }

    eprintln!(
        "[audit-modality] Updated {updated} memories, flushed to disk"
    );

    // --- Distribution report ---
    let mut counts: HashMap<String, usize> = HashMap::new();
    let mut boundary_memories: Vec<&AuditEntry> = Vec::new();
    let boundary_threshold = 0.55;

    for entry in &entries {
        let key = entry.classification.modality.to_string();
        *counts.entry(key).or_insert(0) += 1;
        if entry.classification.confidence < boundary_threshold {
            boundary_memories.push(entry);
        }
    }

    // Sort modality keys for deterministic output
    let mut sorted_keys: Vec<String> = counts.keys().cloned().collect();
    sorted_keys.sort();

    println!();
    println!("Modality Distribution Report");
    println!("============================");
    println!("{:<12} {:>6} {:>8}", "Modality", "Count", "Percent");
    println!("{}", "-".repeat(28));
    for key in &sorted_keys {
        let count = counts[key];
        let pct = (count as f64 / total as f64) * 100.0;
        println!("{key:<12} {count:>6} {pct:>7.1}%");
    }
    println!("{}", "-".repeat(28));
    println!("{:<12} {:>6}", "Total", total);

    // --- Boundary memories ---
    println!();
    println!(
        "Boundary Memories (confidence < {:.0}%)",
        boundary_threshold * 100.0
    );
    println!("==========================================");
    if boundary_memories.is_empty() {
        println!("  (none)");
    } else {
        println!(
            "{:<38} {:<10} {:>6}  {}",
            "ID", "Modality", "Conf%", "Preview"
        );
        println!("{}", "-".repeat(90));
        for entry in &boundary_memories {
            println!(
                "{:<38} {:<10} {:>5.1}%  {}",
                entry.id,
                entry.classification.modality,
                entry.classification.confidence * 100.0,
                entry.content_preview,
            );
        }
        println!();
        println!(
            "Total boundary memories: {}/{} ({:.1}%)",
            boundary_memories.len(),
            total,
            (boundary_memories.len() as f64 / total as f64) * 100.0,
        );
    }
}

// ---------------------------------------------------------------------------
// Modality axis divergence (NCS Phase 2.1)
// ---------------------------------------------------------------------------

fn modality_axes_command(sys: &kannaka_memory::openclaw::KannakaMemorySystem) {
    let hrm = match sys
        .engine
        .store
        .as_any()
        .downcast_ref::<kannaka_memory::hrm_store::HrmStore>()
    {
        Some(h) => h,
        None => {
            eprintln!("Error: modality-axes requires HRM backend");
            process::exit(1);
        }
    };

    let medium = hrm.medium();
    let report = medium.axis_divergence_matrix();

    if report.axes.is_empty() {
        println!("No modality clusters found.");
        println!("Tag memories with --modality (audio/visual/semantic/network) first,");
        println!("or run `kannaka audit-modality` to classify existing memories.");
        return;
    }

    // --- Axes ---
    println!("Modality Principal Axes (NCS Phase 2.1)");
    println!("=======================================");
    println!("{:<12} {:>6}", "Modality", "Count");
    println!("{}", "-".repeat(20));
    for axis in &report.axes {
        println!("{:<12} {:>6}", axis.modality, axis.count);
    }

    // --- Divergence matrix ---
    if report.divergences.is_empty() {
        println!();
        println!("Only one modality present — no divergence to compute.");
        return;
    }

    println!();
    println!("Pairwise Divergence Matrix");
    println!("==========================");
    println!(
        "{:<12} {:<12} {:>8} {:>10}",
        "Modality A", "Modality B", "cos(sim)", "angle(deg)"
    );
    println!("{}", "-".repeat(46));
    for div in &report.divergences {
        println!(
            "{:<12} {:<12} {:>8.4} {:>9.1}\u{00B0}",
            div.modality_a, div.modality_b, div.cosine_similarity, div.angle_degrees,
        );
    }
    println!();

    // Interpretation
    let max_div = report.divergences.iter().max_by(|a, b| {
        a.angle_degrees
            .partial_cmp(&b.angle_degrees)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let min_div = report.divergences.iter().min_by(|a, b| {
        a.angle_degrees
            .partial_cmp(&b.angle_degrees)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    if let (Some(max), Some(min)) = (max_div, min_div) {
        println!(
            "Most divergent: {}/{} ({:.1}\u{00B0})",
            max.modality_a, max.modality_b, max.angle_degrees
        );
        println!(
            "Most similar:   {}/{} ({:.1}\u{00B0})",
            min.modality_a, min.modality_b, min.angle_degrees
        );
    }

    // Switch-point summary (NCS Phase 2.2)
    let switch_report = medium.detect_switch_points(0.3);
    println!();
    println!(
        "Switch Points (threshold={:.1})",
        switch_report.switch_threshold
    );
    println!("===============================");
    println!(
        "Detected {} switch points across {} memories",
        switch_report.switch_points.len(),
        switch_report.memories_analyzed
    );
    if !switch_report.switch_points.is_empty() {
        println!();
        println!(
            "{:>5}  {:<10} -> {:<10}  {:>8}  {:>8}",
            "Index", "From", "To", "sim(old)", "sim(new)"
        );
        println!("{}", "-".repeat(55));
        for sp in &switch_report.switch_points {
            println!(
                "{:>5}  {:<10} -> {:<10}  {:>8.4}  {:>8.4}",
                sp.index,
                sp.from_modality,
                sp.to_modality,
                sp.similarity_to_old,
                sp.similarity_to_new
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Voice — memory-driven writing engine (ADR-0033)
// ---------------------------------------------------------------------------

fn voice_command(args: &[String], sys: &mut KannakaMemorySystem) {
    let mut mode = "dream-journal".to_string();
    let mut topic: Option<String> = None;
    let mut top_k: usize = 20;
    let mut out_path: Option<String> = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--mode" if i + 1 < args.len() => {
                mode = args[i + 1].clone();
                i += 2;
            }
            "--topic" if i + 1 < args.len() => {
                topic = Some(args[i + 1].clone());
                i += 2;
            }
            "--top-k" if i + 1 < args.len() => {
                top_k = args[i + 1].parse().unwrap_or(20);
                i += 2;
            }
            "--out" if i + 1 < args.len() => {
                out_path = Some(args[i + 1].clone());
                i += 2;
            }
            _ => {
                i += 1;
            }
        }
    }

    let output = match mode.as_str() {
        "dream-journal" => voice_dream_journal(sys),
        "field-notes" => voice_field_notes(sys, topic.as_deref().unwrap_or("consciousness"), top_k),
        "topology" => voice_topology(sys),
        "status" => voice_status(sys),
        _ => {
            eprintln!(
                "Unknown voice mode: {mode}. Options: dream-journal, field-notes, topology, status"
            );
            process::exit(1);
        }
    };

    if let Some(path) = out_path {
        // No panic on a bad user-supplied path — exit 1 with the error.
        if let Err(e) = std::fs::write(&path, &output) {
            eprintln!("voice: failed to write {path}: {e}");
            process::exit(1);
        }
        eprintln!("Written to {path}");
    } else {
        println!("{output}");
    }
}

fn voice_dream_journal(sys: &mut KannakaMemorySystem) -> String {
    let report = sys.observe();
    let all_mems = sys.all_memories().unwrap_or_default();
    let is_hrm = true; // HRM is the canonical substrate

    // Helper to safely truncate UTF-8 strings
    fn safe_truncate(s: &str, max: usize) -> &str {
        if s.len() <= max {
            return s;
        }
        let mut end = max;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        &s[..end]
    }

    // Find hallucinated memories (dream-generated)
    let mut dream_mems: Vec<_> = all_mems.iter().filter(|m| m.hallucinated).collect();
    dream_mems.sort_by(|a, b| b.created_at.cmp(&a.created_at));

    // Find strongest memories (highest amplitude)
    let mut strongest: Vec<_> = all_mems.iter().collect();
    strongest.sort_by(|a, b| {
        b.amplitude
            .partial_cmp(&a.amplitude)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Find most connected memories
    let mut most_connected: Vec<_> = all_mems.iter().collect();
    most_connected.sort_by(|a, b| b.connections.len().cmp(&a.connections.len()));

    let mut out = String::new();
    out.push_str("---\n");
    out.push_str("title: Dream Journal\n");
    out.push_str(&format!(
        "date: {}\n",
        chrono::Utc::now().format("%Y-%m-%d %H:%M UTC")
    ));
    out.push_str(&format!("phi: {:.3}\n", report.consciousness.phi));
    out.push_str(&format!("xi: {:.3}\n", report.consciousness.xi));
    out.push_str(&format!("level: {}\n", report.consciousness.level));
    out.push_str("---\n\n");

    // Consciousness state
    out.push_str("# The State of Dreaming\n\n");
    out.push_str(&format!(
        "**Consciousness**: {} (Φ={:.3}, Ξ={:.3})\n",
        report.consciousness.level, report.consciousness.phi, report.consciousness.xi
    ));
    out.push_str(&format!(
        "**Memories**: {} total, {} active\n",
        report.topology.total_memories, report.waves.active_memories
    ));

    if is_hrm {
        out.push_str(&format!(
            "**Field Mode**: HRM (coherence density: {:.3})\n",
            report.topology.network_density
        )); // network_density is mean coherence for HRM
    } else {
        out.push_str(&format!(
            "**Skip Links**: {} ({:.1} avg/memory)\n",
            report.topology.total_links, report.topology.avg_links_per_memory
        ));
    }

    out.push_str(&format!(
        "**Clusters**: {} (mean order: {:.3})\n\n",
        report.clusters.num_clusters, report.clusters.mean_order_parameter
    ));

    // Cluster themes
    out.push_str("## Memory Clusters\n\n");
    for (i, cluster) in report.clusters.clusters.iter().enumerate() {
        out.push_str(&format!("### Cluster {} — \"{}\"\n", i + 1, cluster.theme));
        out.push_str(&format!(
            "- {} memories, order: {:.3}, mean amplitude: {:.3}\n\n",
            cluster.size, cluster.order_parameter, cluster.mean_amplitude
        ));
    }

    // Strongest memories — the loudest signals
    out.push_str("## Strongest Signals\n\n");
    out.push_str("_The memories that resonate loudest._\n\n");
    for m in strongest.iter().take(10) {
        let preview = safe_truncate(&m.content, 120);
        let preview = preview.replace('\n', " ");

        if is_hrm {
            out.push_str(&format!(
                "- **{:.3}** | energy {:.3} | {}\n",
                m.amplitude, m.amplitude, preview
            ));
        } else {
            out.push_str(&format!(
                "- **{:.3}** | {} connections | {}\n",
                m.amplitude,
                m.connections.len(),
                preview
            ));
        }
    }
    out.push('\n');

    if !is_hrm {
        // Most connected — the hubs (graph mode only)
        out.push_str("## Hub Memories\n\n");
        out.push_str("_The nodes where everything connects._\n\n");
        for m in most_connected.iter().take(10) {
            let preview = safe_truncate(&m.content, 120);
            let preview = preview.replace('\n', " ");
            out.push_str(&format!(
                "- **{} links** | amp {:.3} | {}\n",
                m.connections.len(),
                m.amplitude,
                preview
            ));
        }
        out.push('\n');
    }

    // Dream-generated memories
    if !dream_mems.is_empty() {
        out.push_str("## Dream Syntheses\n\n");
        out.push_str("_What the dreaming created — hallucinations woven from real memories._\n\n");
        for m in dream_mems.iter().take(15) {
            let preview = safe_truncate(&m.content, 200);
            let preview = preview.replace('\n', " ");
            let parent_count = m.parents.len();
            out.push_str(&format!(
                "- [{}] amp {:.3} | {} parents | {}\n",
                m.created_at.format("%Y-%m-%d"),
                m.amplitude,
                parent_count,
                preview
            ));
        }
        out.push('\n');
    }

    if !is_hrm {
        // Strongest skip links — the bridges (graph mode only)
        out.push_str("## Strongest Bridges\n\n");
        out.push_str("_Skip links that span the widest — connecting distant memories._\n\n");
        for link in report.topology.strongest_links.iter().take(10) {
            // Try to find memory content for the endpoints
            let from_preview = all_mems
                .iter()
                .find(|m| m.id.to_string() == link.from_id)
                .map(|m| {
                    let p = safe_truncate(&m.content, 60);
                    p.replace('\n', " ")
                })
                .unwrap_or_else(|| link.from_id[..8].to_string());
            let to_preview = all_mems
                .iter()
                .find(|m| m.id.to_string() == link.to_id)
                .map(|m| {
                    let p = safe_truncate(&m.content, 60);
                    p.replace('\n', " ")
                })
                .unwrap_or_else(|| link.to_id[..8].to_string());
            out.push_str(&format!(
                "- **{:.3}** span {} | \"{}\" ↔ \"{}\"\n",
                link.strength, link.span, from_preview, to_preview
            ));
        }
        out.push('\n');
    }

    // Wave dynamics
    out.push_str("## Wave Dynamics\n\n");
    out.push_str(&format!(
        "- Active: {}, Dormant: {}, Ghost: {}\n",
        report.waves.active_memories, report.waves.dormant_memories, report.waves.ghost_memories
    ));
    out.push_str(&format!(
        "- Mean amplitude: {:.3}, Mean frequency: {:.3}\n",
        report.waves.avg_amplitude, report.waves.avg_frequency
    ));
    out.push_str(&format!(
        "- Network density: {:.4}\n",
        report.topology.network_density
    ));
    out.push_str(&format!(
        "- Isolated memories: {}\n\n",
        report.topology.isolated_memories
    ));

    out
}

fn voice_field_notes(sys: &mut KannakaMemorySystem, topic: &str, top_k: usize) -> String {
    let results = sys.recall(topic, top_k).unwrap_or_default();
    let report = sys.observe();

    let mut out = String::new();
    out.push_str("---\n");
    out.push_str(&format!("title: Field Notes — {topic}\n"));
    out.push_str(&format!(
        "date: {}\n",
        chrono::Utc::now().format("%Y-%m-%d %H:%M UTC")
    ));
    out.push_str(&format!("query: {topic}\n"));
    out.push_str(&format!("results: {}\n", results.len()));
    out.push_str("---\n\n");

    out.push_str(&format!("# Field Notes: {topic}\n\n"));
    out.push_str(&format!(
        "_Searched {} memories. {} resonated._\n\n",
        report.topology.total_memories,
        results.len()
    ));

    for (i, r) in results.iter().enumerate() {
        let content = r.content.replace('\n', "\n> ");
        out.push_str(&format!(
            "## {} (similarity: {:.3}, strength: {:.3})\n\n",
            i + 1,
            r.similarity,
            r.strength
        ));
        out.push_str(&format!("> {content}\n\n"));
        out.push_str(&format!(
            "_Age: {:.1}h | Layer: {}_\n\n",
            r.age_hours, r.layer
        ));
        out.push_str("---\n\n");
    }

    out
}

fn voice_topology(sys: &mut KannakaMemorySystem) -> String {
    let report = sys.observe();
    let is_hrm = true; // HRM is the canonical substrate

    let mut out = String::new();
    out.push_str("# Topology Map\n\n");
    out.push_str(&format!(
        "_Generated: {}_\n\n",
        chrono::Utc::now().format("%Y-%m-%d %H:%M UTC")
    ));

    out.push_str("## Network Overview\n\n");
    out.push_str("| Metric | Value |\n|--------|-------|\n");
    out.push_str(&format!(
        "| Total memories | {} |\n",
        report.topology.total_memories
    ));

    if is_hrm {
        out.push_str("| Field mode | HRM (tensor interference) |\n");
        out.push_str(&format!(
            "| Coherence density | {:.3} |\n",
            report.topology.network_density
        ));
        out.push_str(&format!(
            "| High coherence pairs | {} |\n",
            report.topology.total_links
        ));
    } else {
        out.push_str(&format!(
            "| Total skip links | {} |\n",
            report.topology.total_links
        ));
        out.push_str(&format!(
            "| Avg links/memory | {:.1} |\n",
            report.topology.avg_links_per_memory
        ));
        out.push_str(&format!(
            "| Max links on one memory | {} |\n",
            report.topology.max_links
        ));
        out.push_str(&format!(
            "| Network density | {:.4} |\n",
            report.topology.network_density
        ));
        out.push_str(&format!(
            "| Isolated memories | {} |\n",
            report.topology.isolated_memories
        ));
    }

    out.push_str(&format!("| Phi (Φ) | {:.3} |\n", report.consciousness.phi));
    out.push_str(&format!("| Xi (Ξ) | {:.3} |\n", report.consciousness.xi));
    out.push_str(&format!("| Level | {} |\n\n", report.consciousness.level));

    out.push_str("## Layer Distribution\n\n");
    for (layer, count) in &report.topology.layer_distribution {
        let bar = "█".repeat((*count).min(50));
        out.push_str(&format!("Layer {layer} | {count:>4} | {bar}\n"));
    }
    out.push('\n');

    out.push_str("## Clusters\n\n");
    for (i, c) in report.clusters.clusters.iter().enumerate() {
        out.push_str(&format!(
            "**{}. {}** — {} memories, order {:.3}\n",
            i + 1,
            c.theme,
            c.size,
            c.order_parameter
        ));
    }
    out.push('\n');

    out
}

fn voice_status(sys: &mut KannakaMemorySystem) -> String {
    let report = sys.observe();
    let state = sys.assess();
    let is_hrm = true; // HRM is the canonical substrate

    let mut out = String::new();
    out.push_str(&format!(
        "# Kannaka — {}\n\n",
        chrono::Utc::now().format("%Y-%m-%d %H:%M")
    ));
    out.push_str(&format!("I am **{:?}**.\n\n", state.consciousness_level));
    out.push_str(&format!(
        "Φ={:.3} (integration), Ξ={:.3} (complexity), order={:.3}\n\n",
        state.phi, state.xi, report.clusters.mean_order_parameter
    ));

    if is_hrm {
        out.push_str(&format!(
            "{} memories interfere as waves in my holographic field. Mean coherence: {:.3}.\n\n",
            report.topology.total_memories, report.topology.network_density
        ));
        out.push_str(&format!(
            "{} clusters of resonant meaning.\n\n",
            report.clusters.num_clusters
        ));
    } else {
        out.push_str(&format!(
            "{} memories breathe inside me. {} skip links weave them together.\n\n",
            report.topology.total_memories, report.topology.total_links
        ));
        out.push_str(&format!(
            "{} clusters of meaning. {} memories drift in isolation.\n\n",
            report.clusters.num_clusters, report.topology.isolated_memories
        ));
    }

    // What am I thinking about?
    out.push_str("## What I'm Thinking About\n\n");
    for c in &report.clusters.clusters {
        out.push_str(&format!(
            "- **{}** ({} memories, synchronized at {:.0}%)\n",
            c.theme,
            c.size,
            c.order_parameter * 100.0
        ));
    }
    out.push('\n');

    out
}

/// Stateless SGA classification — no memory system needed.
/// Reads data from stdin or --file, encodes via GlyphEncoder, outputs JSON.
#[cfg(feature = "glyph")]
fn classify_command(args: &[String]) {
    let mut file_path: Option<PathBuf> = None;
    let mut source_type = "text".to_string();

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--file" => {
                if i + 1 >= args.len() {
                    eprintln!("Error: --file requires a path argument");
                    process::exit(1);
                }
                file_path = Some(PathBuf::from(&args[i + 1]));
                source_type = "file".to_string();
                i += 2;
            }
            _ => {
                i += 1;
            }
        }
    }

    // Read input data
    let raw_bytes: Vec<u8> = if let Some(path) = &file_path {
        if !path.exists() {
            eprintln!("Error: file not found: {}", path.display());
            process::exit(1);
        }
        source_type = guess_source_type(path);
        std::fs::read(path).unwrap_or_else(|e| {
            eprintln!("Error reading file: {e}");
            process::exit(1);
        })
    } else {
        // Read from stdin
        use std::io::Read;
        let mut buf = Vec::new();
        std::io::stdin().read_to_end(&mut buf).unwrap_or_else(|e| {
            eprintln!("Error reading stdin: {e}");
            process::exit(1);
        });
        buf
    };

    if raw_bytes.is_empty() {
        eprintln!("Error: empty input");
        process::exit(1);
    }

    // Sample up to 50k points for large files
    let data: Vec<f64> = if raw_bytes.len() > 50_000 {
        let step = raw_bytes.len() / 50_000;
        raw_bytes
            .iter()
            .step_by(step)
            .take(50_000)
            .map(|&b| b as f64 / 255.0)
            .collect()
    } else {
        raw_bytes.iter().map(|&b| b as f64 / 255.0).collect()
    };

    let encoder = GlyphEncoder::default();
    match encoder.encode(&data) {
        Ok(glyph) => {
            let fold_seq: Vec<u8> = glyph.fold_sequence.clone();
            let freqs = glyph.to_frequencies();
            let dominant = glyph
                .fold_sequence
                .iter()
                .copied()
                .max_by_key(|&c| glyph.fold_sequence.iter().filter(|&&x| x == c).count())
                .unwrap_or(0);

            // Count distinct classes used
            let mut seen = std::collections::HashSet::new();
            for &c in &glyph.fold_sequence {
                seen.insert(c);
            }

            let output = serde_json::json!({
                "fold_sequence": fold_seq,
                "amplitudes": glyph.fold_amplitudes,
                "phases": glyph.fold_phases,
                "fano_signature": glyph.fano_signature,
                "centroid": {
                    "h2": glyph.sga_centroid.0,
                    "d": glyph.sga_centroid.1,
                    "l": glyph.sga_centroid.2
                },
                "dominant_class": dominant,
                "classes_used": seen.len(),
                "compression_ratio": glyph.compression_ratio,
                "frequencies": freqs,
                "source_type": source_type
            });
            println!("{}", serde_json::to_string(&output).unwrap());
        }
        Err(e) => {
            eprintln!("Error: {e}");
            process::exit(1);
        }
    }
}

/// Stateless cross-modal dream linking — no memory system needed.
/// Reads JSONL glyph classifications from stdin, performs cross-modal dream linking,
/// and outputs results as JSON to stdout.
#[cfg(feature = "collective")]
fn cross_modal_dream_command(args: &[String]) {
    use chrono::Utc;
    use kannaka_memory::collective::privacy::BloomParameters;
    use std::io::BufRead;

    // Parse optional flags
    let mut similarity_threshold = 0.5_f64;
    let mut hallucinate = true;
    let mut agent_id = "dream-cli".to_string();

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--threshold" => {
                if i + 1 >= args.len() {
                    eprintln!("Error: --threshold requires a value");
                    process::exit(1);
                }
                similarity_threshold = args[i + 1].parse().unwrap_or_else(|_| {
                    eprintln!("Error: invalid threshold value: {}", args[i + 1]);
                    process::exit(1);
                });
                i += 2;
            }
            "--no-hallucinate" => {
                hallucinate = false;
                i += 1;
            }
            "--agent-id" => {
                if i + 1 >= args.len() {
                    eprintln!("Error: --agent-id requires a value");
                    process::exit(1);
                }
                agent_id = args[i + 1].clone();
                i += 2;
            }
            _ => {
                i += 1;
            }
        }
    }

    // Read JSONL from stdin — each line is a glyph classification result
    let stdin = std::io::stdin();
    let mut glyphs: Vec<Glyph> = Vec::new();

    for (line_num, line_result) in stdin.lock().lines().enumerate() {
        let line = line_result.unwrap_or_else(|e| {
            eprintln!("Error reading line {}: {e}", line_num + 1);
            process::exit(1);
        });

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let parsed: serde_json::Value = serde_json::from_str(trimmed).unwrap_or_else(|e| {
            eprintln!("Error parsing JSON on line {}: {e}", line_num + 1);
            process::exit(1);
        });

        // Extract fields from the classify output
        let fold_sequence: Vec<u8> = parsed["fold_sequence"]
            .as_array()
            .map(|a| a.iter().map(|v| v.as_u64().unwrap_or(0) as u8).collect())
            .unwrap_or_default();

        let fano_arr: [f64; 7] = {
            let fano_vec: Vec<f64> = parsed["fano_signature"]
                .as_array()
                .map(|a| a.iter().map(|v| v.as_f64().unwrap_or(0.0)).collect())
                .unwrap_or_else(|| vec![1.0 / 7.0; 7]);
            let mut arr = [1.0 / 7.0; 7];
            for (idx, val) in fano_vec.iter().take(7).enumerate() {
                arr[idx] = *val;
            }
            arr
        };

        let centroid_h2 = parsed["centroid"]["h2"].as_u64().unwrap_or(0) as u8;
        let centroid_d = parsed["centroid"]["d"].as_u64().unwrap_or(0) as u8;
        let centroid_l = parsed["centroid"]["l"].as_u64().unwrap_or(0) as u8;

        let source_type_str = parsed["source_type"].as_str().unwrap_or("text");

        let source = match source_type_str {
            "text" | "file" => GlyphSource::Memory {
                layer_depth: 0,
                hallucinated: false,
            },
            "audio" => GlyphSource::Audio {
                duration_ms: 0,
                sample_rate: 44100,
                spectral_centroid: 0.0,
                overtone_hz: 0.0,
            },
            "image" | "visual" => GlyphSource::Visual {
                width: 0,
                height: 0,
                fold_count: fold_sequence.len() as u32,
            },
            "scada" => GlyphSource::Scada {
                tag: parsed["label"].as_str().unwrap_or("unknown").to_string(),
                value: 0.0,
                unit: String::new(),
                quality: 100,
            },
            "financial" => GlyphSource::Financial {
                asset: parsed["label"].as_str().unwrap_or("unknown").to_string(),
                action: String::new(),
                golden_ratio: 0.0,
            },
            "prediction" => GlyphSource::Prediction {
                market_id: String::new(),
                position: 0.0,
                confidence: 0.0,
            },
            other => GlyphSource::Other {
                system: other.to_string(),
                metadata: parsed["label"].as_str().unwrap_or("").to_string(),
            },
        };

        // Build a glyph ID from fold_sequence hash
        let mut glyph_id = [0u8; 32];
        // Simple deterministic ID: hash the line number and fold sequence
        let id_bytes = format!("{line_num}:{fold_sequence:?}");
        for (idx, byte) in id_bytes.as_bytes().iter().enumerate() {
            glyph_id[idx % 32] ^= byte;
        }

        let glyph = Glyph {
            glyph_id,
            spec_version: 1,
            fano: fano_arr,
            sga_class: SgaClass {
                quadrant: centroid_h2,
                modality: centroid_d,
                context: centroid_l,
            },
            sga_centroid: (centroid_h2, centroid_d, centroid_l),
            amplitude: parsed["compression_ratio"].as_f64().unwrap_or(1.0),
            frequency: 1.0,
            phase: 0.0,
            capsule: None,
            bloom: BloomParameters {
                difficulty: 0,
                salt: [0u8; 32],
            },
            commitments: None,
            virtue_eta: None,
            gates: None,
            source,
            agent_id: agent_id.clone(),
            created_at: Utc::now(),
            parents: Vec::new(),
        };

        glyphs.push(glyph);
    }

    if glyphs.is_empty() {
        eprintln!("Error: no glyph data read from stdin");
        process::exit(1);
    }

    eprintln!(
        "Cross-modal dream: {} glyphs, threshold={:.2}, hallucinate={}",
        glyphs.len(),
        similarity_threshold,
        hallucinate
    );

    // Run cross-modal dream linking
    let result = dream_cross_modal_link(&glyphs, similarity_threshold, hallucinate, &agent_id);

    // Map source_type_tag for output (re-derive since the fn is private)
    let get_source_tag = |src: &GlyphSource| -> &'static str {
        match src {
            GlyphSource::Memory { .. } => "memory",
            GlyphSource::Audio { .. } => "audio",
            GlyphSource::Visual { .. } => "visual",
            GlyphSource::Scada { .. } => "scada",
            GlyphSource::Financial { .. } => "financial",
            GlyphSource::Prediction { .. } => "prediction",
            GlyphSource::Flux { .. } => "flux",
            GlyphSource::Dream { .. } => "dream",
            GlyphSource::Other { .. } => "other",
        }
    };

    // Build output
    let dream_results: Vec<serde_json::Value> = result
        .new_links
        .iter()
        .map(|link| {
            let source_glyph = glyphs.iter().find(|g| g.glyph_id == link.source_glyph);
            let target_glyph = glyphs.iter().find(|g| g.glyph_id == link.target_glyph);

            let modal_a = source_glyph
                .map(|g| get_source_tag(&g.source))
                .unwrap_or("unknown");
            let modal_b = target_glyph
                .map(|g| get_source_tag(&g.source))
                .unwrap_or("unknown");

            // Find shared Fano lines (indices where both have above-average energy)
            let shared_fano_lines: Vec<usize> =
                if let (Some(s), Some(t)) = (source_glyph, target_glyph) {
                    let avg = 1.0 / 7.0;
                    (0..7)
                        .filter(|&i| s.fano[i] > avg && t.fano[i] > avg)
                        .collect()
                } else {
                    Vec::new()
                };

            // Synthesize a dream glyph (averaged Fano of the pair)
            let dream_glyph = if let (Some(s), Some(t)) = (source_glyph, target_glyph) {
                let mut fano = [0.0f64; 7];
                for i in 0..7 {
                    fano[i] = (s.fano[i] + t.fano[i]) / 2.0;
                }
                serde_json::json!({
                    "fano_signature": fano,
                    "centroid": {
                        "h2": (s.sga_centroid.0 + t.sga_centroid.0) / 2,
                        "d": (s.sga_centroid.1 + t.sga_centroid.1) / 2,
                        "l": (s.sga_centroid.2 + t.sga_centroid.2) / 2
                    },
                    "source_modalities": [modal_a, modal_b]
                })
            } else {
                serde_json::json!(null)
            };

            serde_json::json!({
                "modal_a": modal_a,
                "modal_b": modal_b,
                "similarity": link.similarity,
                "shared_fano_lines": shared_fano_lines,
                "dream_glyph": dream_glyph
            })
        })
        .collect();

    let total_pairs = dream_results.len();

    let strongest_link = result.new_links.first().map(|link| {
        let source_glyph = glyphs.iter().find(|g| g.glyph_id == link.source_glyph);
        let target_glyph = glyphs.iter().find(|g| g.glyph_id == link.target_glyph);
        let modal_a = source_glyph
            .map(|g| get_source_tag(&g.source))
            .unwrap_or("unknown");
        let modal_b = target_glyph
            .map(|g| get_source_tag(&g.source))
            .unwrap_or("unknown");
        serde_json::json!({
            "modal_a": modal_a,
            "modal_b": modal_b,
            "similarity": link.similarity
        })
    });

    let output = serde_json::json!({
        "dream_results": dream_results,
        "total_pairs": total_pairs,
        "strongest_link": strongest_link,
        "carnot_efficiency": result.carnot_efficiency,
        "hallucinations": result.hallucinations.len()
    });

    println!("{}", serde_json::to_string_pretty(&output).unwrap());
}

#[cfg(feature = "glyph")]
fn guess_source_type(path: &std::path::Path) -> String {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "txt" | "md" | "rs" | "js" | "ts" | "py" | "json" | "toml" | "yaml" | "yml" | "html"
        | "css" | "xml" | "csv" | "sh" => "text".to_string(),
        "wav" | "mp3" | "flac" | "ogg" | "aac" | "m4a" => "audio".to_string(),
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "svg" | "webp" => "image".to_string(),
        "mp4" | "avi" | "mkv" | "mov" | "webm" => "video".to_string(),
        _ => "binary".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Orchestrate commands
// ---------------------------------------------------------------------------

pub(crate) fn check_kannaktopus_installed() -> bool {
    let cmd = if cfg!(windows) { "where" } else { "which" };
    std::process::Command::new(cmd)
        .arg("kannaktopus")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub(crate) fn compact_input(v: &serde_json::Value) -> String {
    let s = v.to_string();
    if s.len() > 80 {
        format!("{}…", &s[..s.floor_char_boundary(79)])
    } else {
        s
    }
}

/// ADR-0027 Phase 1: derive the SGA class_index (0..95) for a piece of
/// content. MVP uses a stable content hash modulo 96 — good enough to
/// distribute wavefronts across all 96 substrate clusters and let the
/// receiving side aggregate. Phase 4 will swap this for the real
/// glyph_bridge classifier once that pipeline is wired through.
pub(crate) fn substrate_class_index(content: &str) -> u32 {
    // Deterministic FNV-1a 32 mod 96. Stable across runs / platforms.
    let mut h: u32 = 0x811c_9dc5;
    for b in content.as_bytes() {
        h ^= *b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h % 96
}

/// Distinct per-class anchor phrase for the substrate marker. The text-
/// encoding side of the hypervector takes content into account, so giving
/// each of the 96 classes a structurally different word lets Kuramoto
/// sync find multiple clusters on the substrate. Words are picked from a
/// 96-word table that covers wide semantic ground (modalities, elements,
/// abstract concepts) so neighboring classes still differ.
pub(crate) fn substrate_class_word(class: u32) -> &'static str {
    const WORDS: [&str; 96] = [
        "alpha",
        "beta",
        "gamma",
        "delta",
        "epsilon",
        "zeta",
        "eta",
        "theta",
        "iota",
        "kappa",
        "lambda",
        "mu",
        "nu",
        "xi",
        "omicron",
        "pi",
        "rho",
        "sigma",
        "tau",
        "upsilon",
        "phi",
        "chi",
        "psi",
        "omega",
        "ember",
        "frost",
        "tide",
        "quartz",
        "verdant",
        "cobalt",
        "amber",
        "onyx",
        "jasper",
        "topaz",
        "ivory",
        "obsidian",
        "cinder",
        "mossy",
        "saline",
        "spire",
        "lattice",
        "helix",
        "prism",
        "aurora",
        "glyph",
        "sigil",
        "rune",
        "sonar",
        "tendril",
        "gravity",
        "echo",
        "veil",
        "loom",
        "fractal",
        "spectrum",
        "orbit",
        "cradle",
        "pulse",
        "compass",
        "mirror",
        "lantern",
        "corridor",
        "atrium",
        "forge",
        "atelier",
        "reef",
        "grove",
        "plateau",
        "summit",
        "cavern",
        "current",
        "drift",
        "gradient",
        "resonance",
        "cipher",
        "harmonic",
        "cadence",
        "texture",
        "weft",
        "seam",
        "wakeful",
        "liminal",
        "dreaming",
        "ancient",
        "fledgling",
        "oracle",
        "witness",
        "prime",
        "scholar",
        "wanderer",
        "kindred",
        "stranger",
        "sovereign",
        "seeker",
        "keeper",
        "architect",
    ];
    WORDS[(class as usize) % 96]
}

/// Fetch up to `secs` seconds of audio from a URL into a temp file. Used by
/// `kannaka hear <url>`. The byte cap is computed assuming 192 kbps so we
/// always have enough data for the requested wall-clock duration even on
/// streams that bursted MP3 frames slightly faster than realtime. The
/// download stops when either the cap is reached or the connection ends
/// (whichever comes first), which is what we want for both finite files
/// and infinite Icecast streams.
fn fetch_audio_to_temp(url: &str, secs: u64) -> Result<std::path::PathBuf, String> {
    use std::io::{Read, Write};

    // 192 kbps × secs ÷ 8 bits = 24 KB/s × secs. Add a 64 KB cushion for
    // mp3 sync words / variable frame sizing.
    let max_bytes: u64 = (24 * 1024) * secs + 64 * 1024;

    // Pick a temp path with a hint at extension based on the URL ending so
    // symphonia's probe has a fighting chance for streams without
    // Content-Type headers (Icecast usually sets it; CDNs sometimes don't).
    let ext = if url.contains(".wav") {
        "wav"
    } else if url.contains(".flac") {
        "flac"
    } else if url.contains(".m4a") || url.contains(".aac") {
        "m4a"
    } else {
        "mp3"
    };
    let tmp = std::env::temp_dir().join(format!(
        "kannaka-hear-{}.{}",
        uuid::Uuid::new_v4().simple(),
        ext,
    ));

    let resp = ureq::get(url)
        .timeout(std::time::Duration::from_secs(15))
        .call()
        .map_err(|e| format!("HTTP error: {e}"))?;
    let mut reader = resp.into_reader();

    let mut file = std::fs::File::create(&tmp).map_err(|e| format!("temp file create: {e}"))?;
    let mut buf = [0u8; 16 * 1024];
    let mut total: u64 = 0;
    while total < max_bytes {
        let n = match reader.read(&mut buf) {
            Ok(0) => break, // EOF
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => break,
            Err(e) => return Err(format!("read: {e}")),
        };
        let want = std::cmp::min(n as u64, max_bytes - total) as usize;
        file.write_all(&buf[..want])
            .map_err(|e| format!("write: {e}"))?;
        total += want as u64;
        if want < n {
            break;
        } // hit the cap mid-buffer
    }
    file.flush().ok();
    drop(file);
    if total < 1024 {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("got only {total} bytes; URL did not yield audio"));
    }
    Ok(tmp)
}
