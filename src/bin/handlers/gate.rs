//! `kannaka swarm activate-gate` + `kannaka swarm beacon` — the guided
//! seed-ceremony activation helper for the inc-1b corroboration gate.
//!
//! These two verbs turn the multi-step corroboration-gate seed ceremony into
//! ~2 commands per node, WITH safety rails. They compose the already-committed
//! primitives (node key + [`enroll_seed`](crate::handlers_identity::enroll_seed)
//! + [`publish_beacon`](kannaka_memory::nats::SwarmTransport::publish_beacon))
//! and change NO automatic runtime behaviour on their own: the ONLY state
//! change is what `activate-gate --yes` deliberately writes (pin the seed set +
//! flip `corroboration_gate_enabled`). The gate stays DORMANT unless the
//! operator explicitly confirms activation with `--yes`.
//!
//! - `activate-gate` — a per-node guided flip: ensure this node's key, PRINT its
//!   pubkey (so the operator can collect it for the other nodes), collect the
//!   target seed set, run a preflight checklist, and — DRY-RUN by default —
//!   print exactly what WOULD change. `--yes` writes; `--force` only bypasses
//!   the >=2-seed refusal (it STILL needs `--yes` to write). Idempotent.
//! - `beacon` — emit a signed heartbeat beacon once, or `--loop` one-per-epoch
//!   under systemd/cron on a SEED (refuses to loop on a non-seed). A fresh seed
//!   beacon is what an ARMED gate needs to promote (anti-eclipse fail-closed:
//!   stale/absent beacons freeze promotion, never drop content).
//!
//! See `ops/services/kannaka-beacon.service` for the per-seed emitter template.

use std::process;

use kannaka_memory::beacon::Beacon;
use kannaka_memory::config::KannakaConfig;
use kannaka_memory::provenance::{node_signing_key, verifying_key_bytes};

use crate::handlers_identity::enroll_seed;

const ACTIVATE_USAGE: &str =
    "Usage: kannaka swarm activate-gate [--seed <b64pubkey>]... [--yes] [--force]";
const BEACON_USAGE: &str = "Usage: kannaka swarm beacon [--loop] [--interval-secs N]";

// ---------------------------------------------------------------------------
// activate-gate
// ---------------------------------------------------------------------------

/// Parsed `activate-gate` flags.
struct ActivateOpts {
    /// Base64 seed pubkeys from `--seed` (repeatable), pre-validation.
    seeds: Vec<String>,
    /// `--yes`: actually write (arm the gate). Without it, dry-run only.
    yes: bool,
    /// `--force`: bypass the >=2-seed refusal ONLY (still needs `--yes` to write).
    force: bool,
}

fn parse_activate_args(args: &[String]) -> Result<ActivateOpts, String> {
    // args = ["swarm", "activate-gate", <flags...>] — flags start at index 2.
    let mut seeds = Vec::new();
    let mut yes = false;
    let mut force = false;
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--seed" => {
                let v = args.get(i + 1).filter(|v| !v.starts_with("--")).ok_or_else(|| {
                    "  --seed requires a base64 pubkey value".to_string()
                })?;
                seeds.push(v.clone());
                i += 2;
            }
            "--yes" | "-y" => {
                yes = true;
                i += 1;
            }
            "--force" => {
                force = true;
                i += 1;
            }
            other => return Err(format!("  unknown flag or argument: {other}")),
        }
    }
    Ok(ActivateOpts { seeds, yes, force })
}

/// The computed effect of an `activate-gate` invocation. Pure, so the handler
/// can print it (dry-run) or apply it and tests can assert it without I/O.
#[derive(Debug, Clone, PartialEq, Eq)]
struct GatePlan {
    /// Canonical (base64) seed set already pinned in config before this run.
    before_seeds: Vec<String>,
    /// Canonical seed set that WOULD be pinned after this run (before ∪ new).
    resulting_seeds: Vec<String>,
    /// Canonical seeds newly added by this run's `--seed` args (not already pinned).
    added: Vec<String>,
    /// Whether the corroboration-gate flag was already enabled before this run.
    gate_already_enabled: bool,
}

impl GatePlan {
    fn seed_count(&self) -> usize {
        self.resulting_seeds.len()
    }

    /// The >=2-seed preflight fails when fewer than two distinct lineages exist.
    /// A single-seed root can never corroborate (corroboration needs >=2 distinct
    /// seed lineages), so promotion would stay frozen at Quarantine forever.
    fn below_min_seeds(&self) -> bool {
        self.seed_count() < 2
    }
}

/// Build the activation plan from a base config + the operator's `--seed` args.
/// Pure: mutates a clone, validates each new seed as a 32-byte base64 key
/// (delegating to [`enroll_seed`], which also dedups by decoded bytes), and
/// never touches disk. Returns an error (leaving intent unchanged) on any
/// malformed seed.
fn build_gate_plan(cfg: &KannakaConfig, new_seeds: &[String]) -> Result<GatePlan, String> {
    let before_seeds = cfg.swarm_trust.seed_pubkeys.clone();
    let mut work = cfg.clone();
    let mut added = Vec::new();
    for s in new_seeds {
        let (_, was_added) = enroll_seed(&mut work, s)?;
        if was_added {
            // enroll_seed pushed the canonical encoding; mirror it for display.
            added.push(crate::b64_encode_std(&crate::b64_decode_32(s)?));
        }
    }
    Ok(GatePlan {
        before_seeds,
        resulting_seeds: work.swarm_trust.seed_pubkeys.clone(),
        added,
        gate_already_enabled: cfg.swarm_trust.corroboration_gate_enabled,
    })
}

/// `kannaka swarm activate-gate` — the guided per-node gate flip.
pub(crate) fn handle_swarm_activate_gate(args: &[String]) {
    let opts = match parse_activate_args(args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("{e}");
            eprintln!("{ACTIVATE_USAGE}");
            process::exit(2);
        }
    };

    // (a) Ensure this node's signing key exists and PRINT its pubkey, so the
    // operator can collect it and pin it on the OTHER seed nodes.
    let dir = KannakaConfig::data_dir();
    let signing = match node_signing_key(&dir) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("  activate-gate: node key error: {e}");
            process::exit(1);
        }
    };
    let my_pk_b64 = crate::b64_encode_std(&verifying_key_bytes(&signing));
    println!("Seed-ceremony activation — corroboration gate");
    println!();
    println!("  this node's pubkey (share it to pin on the OTHER seed nodes):");
    println!("    {my_pk_b64}");
    println!();

    // (b) Collect the target seed set = --seed args ∪ config.seed_pubkeys.
    // load_unmodified so a later write doesn't bake env-only values (agent id,
    // NATS url, …) into the file.
    let cfg = match KannakaConfig::load_unmodified() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("  activate-gate: {e}");
            process::exit(1);
        }
    };
    let plan = match build_gate_plan(&cfg, &opts.seeds) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("  activate-gate: malformed --seed pubkey — {e}");
            eprintln!("  each --seed must be a 32-byte ed25519 verifying key in standard base64.");
            process::exit(1);
        }
    };

    // (c) Preflight checklist.
    let min_ok = !plan.below_min_seeds();
    println!("Preflight:");
    println!(
        "  [{}] seed count >= 2 (K needs >=2 distinct lineages) — {} pinned",
        if min_ok { "ok" } else { "!!" },
        plan.seed_count()
    );
    println!("  [ !] beacons MUST be flowing before an armed gate can promote:");
    println!("       a FRESH seed beacon is required for a Live promotion. Start the");
    println!("       emitter on each SEED node with:  kannaka swarm beacon --loop");
    println!();

    if plan.below_min_seeds() {
        if !opts.force {
            eprintln!(
                "  REFUSED: {} seed pinned. A single-seed root cannot corroborate —",
                plan.seed_count()
            );
            eprintln!(
                "  corroboration needs >=2 DISTINCT seed lineages, so one seed can never promote"
            );
            eprintln!("  anything past Quarantine. Pin a second seed (--seed <b64>), or — if you");
            eprintln!(
                "  truly intend a single-seed bootstrap — re-run with --force (you STILL need --yes)."
            );
            process::exit(1);
        }
        eprintln!(
            "  --force: proceeding with {} seed despite the >=2-seed rule. A single-seed root",
            plan.seed_count()
        );
        eprintln!("  cannot corroborate — promotion stays frozen until a 2nd lineage is pinned.");
        eprintln!();
    }

    // (d) Show the plan (dry-run and apply both print it).
    println!("Plan:");
    if plan.added.is_empty() {
        println!(
            "  seed_pubkeys: unchanged ({} already pinned)",
            plan.resulting_seeds.len()
        );
    } else {
        println!(
            "  seed_pubkeys: +{} new -> {} total. Newly pinned:",
            plan.added.len(),
            plan.resulting_seeds.len()
        );
        for s in &plan.added {
            println!("    + {s}");
        }
    }
    println!(
        "  corroboration_gate_enabled: {}",
        if plan.gate_already_enabled {
            "true -> true (already enabled)"
        } else {
            "false -> true"
        }
    );
    println!();

    // Dry-run by default: STOP without writing.
    if !opts.yes {
        println!("Dry run — nothing written. Re-run with --yes to apply the plan above.");
        return;
    }

    // (d, --yes) Apply: enroll the seeds + flip the flag, then persist. Reload
    // load_unmodified so we write the on-disk view, not the env-enriched one.
    let mut wcfg = match KannakaConfig::load_unmodified() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("  activate-gate: {e}");
            process::exit(1);
        }
    };
    for s in &opts.seeds {
        if let Err(e) = enroll_seed(&mut wcfg, s) {
            // Already validated in build_gate_plan; a failure here is unexpected.
            eprintln!("  activate-gate: seed became invalid mid-apply — {e}");
            process::exit(1);
        }
    }
    let was_enabled = wcfg.swarm_trust.corroboration_gate_enabled;
    wcfg.swarm_trust.corroboration_gate_enabled = true;
    if let Err(e) = wcfg.save() {
        eprintln!("  activate-gate: failed to save config: {e}");
        process::exit(1);
    }

    println!(
        "\u{2713} corroboration gate ARMED — {} seed(s) pinned, corroboration_gate_enabled = true{}",
        wcfg.swarm_trust.seed_pubkeys.len(),
        if was_enabled { " (was already true — idempotent)" } else { "" }
    );
    println!();
    println!("Next steps:");
    println!("  1. Start the per-seed beacon emitter on EACH seed node (systemd/cron):");
    println!("       kannaka swarm beacon --loop");
    println!("     (see ops/services/kannaka-beacon.service). Without a FRESH seed beacon the");
    println!("     armed gate fails CLOSED and freezes promotion (anti-eclipse defense).");
    println!("  2. Verify:  kannaka reputation list     (pinned seeds show STATUS=Seed)");
    println!("              kannaka swarm status         (swarm health)");
}

// ---------------------------------------------------------------------------
// beacon
// ---------------------------------------------------------------------------

/// Parsed `beacon` flags.
struct BeaconOpts {
    /// `--loop`: foreground one-beacon-per-epoch loop (for systemd/cron on a seed).
    loop_mode: bool,
    /// `--interval-secs N`: fixed cadence instead of epoch-boundary scheduling.
    interval_secs: Option<u64>,
}

fn parse_beacon_args(args: &[String]) -> Result<BeaconOpts, String> {
    let mut loop_mode = false;
    let mut interval_secs = None;
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--loop" => {
                loop_mode = true;
                i += 1;
            }
            "--interval-secs" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| "  --interval-secs requires N (seconds)".to_string())?;
                let n: u64 = v
                    .parse()
                    .map_err(|_| format!("  --interval-secs: invalid value '{v}'"))?;
                if n == 0 {
                    return Err("  --interval-secs must be > 0".to_string());
                }
                interval_secs = Some(n);
                i += 2;
            }
            other => return Err(format!("  unknown flag or argument: {other}")),
        }
    }
    Ok(BeaconOpts { loop_mode, interval_secs })
}

/// Whether `my_pk` is in the operator-pinned seed set — the "am I a seed?"
/// predicate the beacon loop refuses on. Pure (config-only), so a fresh node
/// with an empty seed set is correctly NOT a seed.
fn node_is_seed(cfg: &KannakaConfig, my_pk: &[u8; 32]) -> bool {
    cfg.swarm_trust
        .seed_pubkeys
        .iter()
        .any(|s| crate::b64_decode_32(s).map(|d| &d == my_pk).unwrap_or(false))
}

/// Sign a heartbeat beacon for the current epoch, print its JSON wire form to
/// stdout, and best-effort publish it to the swarm. Mirrors `identity beacon`'s
/// emit path (`Beacon::sign` + `SwarmTransport::publish_beacon`); a publish
/// failure is a warning, not an error (the printed line can still be relayed).
fn emit_beacon_once(cfg: &KannakaConfig, signing: &[u8; 32]) {
    let now = kannaka_memory::provenance::now_ms();
    let epoch = kannaka_memory::absorb_gate::epoch_now(cfg, now);
    let beacon = Beacon::sign(signing, epoch, kannaka_memory::beacon::EMPTY_REJECT_ROOT);
    match serde_json::to_string(&beacon) {
        Ok(j) => println!("{j}"),
        Err(e) => {
            eprintln!("  beacon: failed to encode JSON: {e}");
            return;
        }
    }
    eprintln!("  \u{2713} signed heartbeat beacon for epoch {epoch}");

    let nats_url = if cfg.swarm.nats_url.is_empty() {
        kannaka_memory::nats::DEFAULT_NATS_URL.to_string()
    } else {
        cfg.swarm.nats_url.clone()
    };
    match kannaka_memory::nats::SwarmTransport::connect(&nats_url) {
        Ok(transport) => match transport.publish_beacon(&beacon) {
            Ok(()) => eprintln!(
                "  \u{2713} published beacon to {} on {}",
                nats_url,
                kannaka_memory::BEACON_SUBJECT
            ),
            Err(e) => eprintln!("  warning: beacon signed but NATS publish failed: {e}"),
        },
        Err(e) => eprintln!("  warning: beacon signed but NATS connect failed ({nats_url}): {e}"),
    }
}

/// `kannaka swarm beacon [--loop] [--interval-secs N]` — emit signed heartbeat
/// beacon(s) as a pinned seed. One-shot publishes once and exits; `--loop` runs
/// a foreground one-per-epoch emitter for systemd/cron. Refuses on a non-seed.
pub(crate) fn handle_swarm_beacon(args: &[String]) {
    let opts = match parse_beacon_args(args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("{e}");
            eprintln!("{BEACON_USAGE}");
            process::exit(2);
        }
    };

    let dir = KannakaConfig::data_dir();
    let cfg = KannakaConfig::load();
    let signing = match node_signing_key(&dir) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("  beacon: node key error: {e}");
            process::exit(1);
        }
    };
    let my_pk = verifying_key_bytes(&signing);

    // A non-seed beacon never counts (receivers reject NotSeed), so both one-shot
    // and --loop refuse on a non-seed with the reason and how to become one.
    if !node_is_seed(&cfg, &my_pk) {
        eprintln!(
            "  beacon refused: only pinned seeds emit heartbeat beacons; this node is not a seed."
        );
        eprintln!("  this node's pubkey: {}", crate::b64_encode_std(&my_pk));
        eprintln!(
            "  an operator pins it with: kannaka swarm activate-gate --seed {} ... --yes",
            crate::b64_encode_std(&my_pk)
        );
        eprintln!(
            "  (or the low-level: kannaka identity enroll-seed {})",
            crate::b64_encode_std(&my_pk)
        );
        process::exit(1);
    }

    if !opts.loop_mode {
        emit_beacon_once(&cfg, &signing);
        return;
    }

    // --loop: one beacon per epoch (or per --interval-secs), for systemd/cron.
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    let running = Arc::new(AtomicBool::new(true));
    let r = Arc::clone(&running);
    if let Err(e) = ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
    }) {
        eprintln!("  warning: could not install Ctrl+C handler: {e}");
    }

    let epoch_len_ms = cfg.swarm_trust.epoch_length_ms.max(1);
    let mode = match opts.interval_secs {
        Some(n) => format!("every {n}s"),
        None => format!("one per epoch (~{}s)", (epoch_len_ms / 1000).max(1)),
    };
    eprintln!("  beacon loop started as a seed ({mode}) — Ctrl+C to stop cleanly.");

    while running.load(Ordering::SeqCst) {
        emit_beacon_once(&cfg, &signing);
        // Sleep until the next scheduling boundary. For epoch mode, sleep to the
        // next epoch boundary so exactly one beacon lands per epoch; for the
        // fixed-interval mode, sleep the requested seconds.
        let sleep_ms: u64 = match opts.interval_secs {
            Some(n) => n.saturating_mul(1000),
            None => {
                let now = kannaka_memory::provenance::now_ms();
                (epoch_len_ms - now.rem_euclid(epoch_len_ms)) as u64
            }
        };
        // Granular <=1s sleeps so Ctrl+C stays responsive.
        let mut slept = 0u64;
        while slept < sleep_ms && running.load(Ordering::SeqCst) {
            let step = (sleep_ms - slept).min(1000);
            std::thread::sleep(std::time::Duration::from_millis(step));
            slept += step;
        }
    }
    eprintln!("  beacon loop stopped.");
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Base64 verifying pubkey derived from a fixed signing seed `[n; 32]`.
    fn pk_b64(n: u8) -> String {
        crate::b64_encode_std(&verifying_key_bytes(&[n; 32]))
    }

    fn strs(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    // A 2-seed plan on a fresh config: both added, gate was off, preflight passes.
    #[test]
    fn plan_two_seeds_passes_preflight() {
        let cfg = KannakaConfig::default();
        let plan = build_gate_plan(&cfg, &[pk_b64(1), pk_b64(2)]).unwrap();
        assert_eq!(plan.resulting_seeds.len(), 2);
        assert_eq!(plan.added.len(), 2);
        assert!(!plan.gate_already_enabled);
        assert!(!plan.below_min_seeds(), "2 seeds clears the >=2 preflight");
    }

    // A single seed trips the >=2 refusal; only --force (a handler flag) bypasses
    // it, and the plan itself is still a valid thing to apply under --force.
    #[test]
    fn plan_single_seed_is_below_min() {
        let cfg = KannakaConfig::default();
        let plan = build_gate_plan(&cfg, &[pk_b64(1)]).unwrap();
        assert_eq!(plan.resulting_seeds.len(), 1);
        assert!(
            plan.below_min_seeds(),
            "1 seed is below the >=2 minimum ⇒ handler refuses unless --force"
        );
    }

    // The union includes seeds ALREADY in config plus the new ones; only genuinely
    // new seeds count as `added`.
    #[test]
    fn plan_unions_existing_and_new_seeds() {
        let mut cfg = KannakaConfig::default();
        enroll_seed(&mut cfg, &pk_b64(1)).unwrap(); // already pinned
        let plan = build_gate_plan(&cfg, &[pk_b64(2)]).unwrap();
        assert_eq!(plan.before_seeds.len(), 1);
        assert_eq!(plan.resulting_seeds.len(), 2, "existing ∪ new");
        assert_eq!(plan.added.len(), 1, "only the new seed counts as added");
    }

    // Re-running with already-pinned seeds after the flag is on adds nothing and
    // reports the flag as already-enabled — the idempotency the handler relies on.
    #[test]
    fn plan_is_idempotent() {
        let mut cfg = KannakaConfig::default();
        let (s1, s2) = (pk_b64(1), pk_b64(2));
        for s in [&s1, &s2] {
            enroll_seed(&mut cfg, s).unwrap();
        }
        cfg.swarm_trust.corroboration_gate_enabled = true;

        let plan = build_gate_plan(&cfg, &[s1.clone(), s2.clone()]).unwrap();
        assert!(plan.added.is_empty(), "re-adding pinned seeds adds nothing");
        assert_eq!(plan.resulting_seeds.len(), 2, "the set does not grow");
        assert!(plan.gate_already_enabled, "flag already on the second time");
    }

    // A malformed --seed is rejected (clear error, no partial mutation of intent).
    #[test]
    fn plan_rejects_malformed_seed() {
        let cfg = KannakaConfig::default();
        // 16 bytes decodes as base64 but is the wrong length for a key.
        assert!(build_gate_plan(&cfg, &[crate::b64_encode_std(&[9u8; 16])]).is_err());
        assert!(build_gate_plan(&cfg, &strs(&["!!! not base64 !!!"])).is_err());
    }

    // beacon --loop refuses on a non-seed: a fresh node's key is not in the
    // (empty) seed set.
    #[test]
    fn beacon_loop_refuses_non_seed() {
        let cfg = KannakaConfig::default(); // no seeds pinned
        let my_pk = verifying_key_bytes(&[42u8; 32]);
        assert!(!node_is_seed(&cfg, &my_pk), "non-seed ⇒ beacon --loop refuses");
    }

    // A pinned node IS a seed (the loop proceeds).
    #[test]
    fn beacon_loop_allows_seed() {
        let mut cfg = KannakaConfig::default();
        let my_pk = verifying_key_bytes(&[7u8; 32]);
        enroll_seed(&mut cfg, &crate::b64_encode_std(&my_pk)).unwrap();
        assert!(node_is_seed(&cfg, &my_pk), "pinned node is a seed");
    }

    // activate-gate flag parsing: --seed repeats, --yes/--force accumulate.
    #[test]
    fn parse_activate_flags() {
        let a = strs(&[
            "swarm", "activate-gate", "--seed", "AAA", "--seed", "BBB", "--yes", "--force",
        ]);
        let o = parse_activate_args(&a).unwrap();
        assert_eq!(o.seeds, strs(&["AAA", "BBB"]));
        assert!(o.yes && o.force);

        // A dangling --seed (no value / followed by a flag) is a clear error.
        assert!(parse_activate_args(&strs(&["swarm", "activate-gate", "--seed"])).is_err());
        assert!(
            parse_activate_args(&strs(&["swarm", "activate-gate", "--seed", "--yes"])).is_err()
        );
        // An unknown flag is rejected (no silent swallow).
        assert!(parse_activate_args(&strs(&["swarm", "activate-gate", "--nope"])).is_err());
    }

    // beacon flag parsing: --loop + --interval-secs, and the one-shot default.
    #[test]
    fn parse_beacon_flags() {
        let o = parse_beacon_args(&strs(&["swarm", "beacon", "--loop", "--interval-secs", "30"]))
            .unwrap();
        assert!(o.loop_mode);
        assert_eq!(o.interval_secs, Some(30));

        let o2 = parse_beacon_args(&strs(&["swarm", "beacon"])).unwrap();
        assert!(!o2.loop_mode && o2.interval_secs.is_none(), "one-shot default");

        // A zero / non-numeric interval is rejected.
        assert!(parse_beacon_args(&strs(&["swarm", "beacon", "--interval-secs", "0"])).is_err());
        assert!(parse_beacon_args(&strs(&["swarm", "beacon", "--interval-secs", "x"])).is_err());
    }
}
