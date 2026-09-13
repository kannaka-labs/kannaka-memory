//! `kannaka dedupe` — collapse memories that are byte-identical into one.
//!
//! The retrospective half of reinforce-on-repeat. The write path
//! (`KannakaMemorySystem::remember_with_category`) now strengthens an existing
//! memory instead of inserting a second copy; this folds the duplicates that
//! were written before it did.
//!
//! It COLLAPSES, it does not delete. Five copies of one verdict are evidence
//! that the world showed you that fact five times, and a cleanup that simply
//! dropped four of them would discard the one useful thing the accident
//! encoded. The keeper inherits the repeat count and is advanced along the same
//! reinforcement curve, as though those sightings had arrived through the fixed
//! write path all along.
//!
//! Safety posture, in order — this is the same shape as `kannaka facets`,
//! because it is the same kind of operation (a one-way corpus rewrite):
//!   1. Dry run by default — `--apply` is required to mutate anything.
//!   2. Never scheduled. There is no cron path, no dream stage, and no daemon
//!      that reaches this; an operator types it.
//!   3. The HRM write lock is taken; if another writer holds it, we refuse
//!      rather than race a lost update. On non-Unix the lock is advisory-only
//!      and we say so.
//!   4. `--apply` under `KANNAKA_READONLY` is refused outright. A read-only
//!      store mutates in RAM and silently drops the write on save, so the run
//!      would report deletions that never happened.
//!   5. `--apply` refuses to proceed without first writing a local
//!      pre-collapse snapshot (gzip of the .hrm), named so the retention
//!      pruner never removes it.

use super::{data_dir, try_acquire_write_lock, KannakaConfig};
use std::io::Write;

pub(crate) fn handle_dedupe(
    sys: &mut kannaka_memory::openclaw::KannakaMemorySystem,
    cfg: &KannakaConfig,
    args: &[String],
) {
    let apply = args.iter().any(|a| a == "--apply");
    let verbose = args.iter().any(|a| a == "--verbose" || a == "-v");
    for a in args {
        if a != "--apply" && a != "--verbose" && a != "-v" {
            eprintln!("[dedupe] ignoring unknown flag: {a}");
        }
    }

    // Single-writer guard — for `--apply` ONLY. A dry run mutates nothing, so
    // taking the lock for it would make the safe, informational mode unavailable
    // exactly when an operator most wants it: while the node is up and the
    // writer holds the lock.
    let lock = if apply {
        let lock = try_acquire_write_lock();
        if lock.is_none() {
            eprintln!(
                "[dedupe] REFUSING --apply: another process holds the HRM write lock \
                 (a swarm join writer or a running dream). Stop it, then re-run. \
                 A dry run needs no lock and is safe to run now."
            );
            std::process::exit(1);
        }
        #[cfg(not(unix))]
        eprintln!(
            "[dedupe] NOTE: the write lock is advisory-only on this platform — \
             confirm no kannaka writer daemon is running before trusting this run."
        );
        lock
    } else {
        None
    };

    if apply && readonly_env() {
        eprintln!(
            "[dedupe] REFUSING --apply: KANNAKA_READONLY is set. A read-only store \
             drops its writes on save, so the collapse would be reported but never \
             persisted. Unset it deliberately, on the machine that owns the store."
        );
        std::process::exit(1);
    }

    if apply {
        if let Err(e) = pre_collapse_snapshot(&cfg.agent.id, sys) {
            eprintln!("[dedupe] REFUSING --apply: pre-collapse snapshot failed: {e}");
            eprintln!("[dedupe] a one-way corpus rewrite does not run without a restore point.");
            std::process::exit(1);
        }
    }

    let report = match sys.collapse_exact_duplicates(apply) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("[dedupe] failed: {e}");
            std::process::exit(1);
        }
    };

    let mode = if apply { "APPLIED" } else { "DRY RUN" };
    println!("[dedupe] {mode}: scanned {} memories", report.scanned);
    println!("  duplicate sets:            {}", report.groups.len());
    if apply {
        println!("  copies folded away:        {}", report.duplicates());
    } else {
        println!("  copies that WOULD fold:    {}", report.duplicates());
    }
    if report.facets_folded > 0 {
        println!(
            "  facets folded with them:   {}  <- a parent's atoms belong to that copy",
            report.facets_folded
        );
    }
    if report.facet_rows_not_grouped > 0 {
        println!(
            "  facet rows not grouped:    {}  <- atoms of a parent, not statements; removed only with their parent",
            report.facet_rows_not_grouped
        );
    }
    if report.skipped_ghosts > 0 {
        println!(
            "  skipped (ADR-0037 ghosts): {}  <- deliberately forgotten; never resurrected here",
            report.skipped_ghosts
        );
    }
    if report.clamped_by_retention > 0 {
        println!(
            "  held at the retention line: {}  <- strengthened, but not across the boundary that would make them un-prunable",
            report.clamped_by_retention
        );
    }
    if report.tier_promotions > 0 {
        println!(
            "  keepers promoted in tier:  {}  <- a pinned duplicate keeps its pin",
            report.tier_promotions
        );
    }
    if report.errors > 0 {
        println!("  errors (delete failed):    {}", report.errors);
    }

    let show = if verbose { report.groups.len() } else { 20 };
    for g in report.groups.iter().take(show) {
        println!(
            "  x{:<3} {:.3} -> {:.3}  {:?}  keep {}  \"{}\"",
            g.folded.len() + 1,
            g.amplitude_before,
            g.amplitude_after,
            g.tier_after,
            g.keeper,
            g.preview.replace('\n', " ")
        );
    }
    if report.groups.len() > show {
        println!(
            "  … and {} more (pass --verbose to list them all)",
            report.groups.len() - show
        );
    }

    if !apply {
        println!(
            "[dedupe] no changes made. Re-run with --apply to execute \
             (a pre-collapse snapshot is taken first)."
        );
    } else if report.groups.is_empty() {
        println!("[dedupe] nothing to collapse.");
    } else {
        println!("[dedupe] flushed to disk.");
    }

    drop(lock);

    // A partial failure must be detectable by a script. Printing the count and
    // exiting 0 made a half-applied collapse look like a clean run.
    if report.errors > 0 {
        eprintln!(
            "[dedupe] {} deletion(s) failed — the collapse is PARTIAL. The \
             pre-collapse snapshot bundle is the restore point.",
            report.errors
        );
        std::process::exit(1);
    }
}

fn readonly_env() -> bool {
    match std::env::var("KANNAKA_READONLY") {
        Ok(v) => !v.is_empty() && v != "0" && !v.eq_ignore_ascii_case("false"),
        Err(_) => false,
    }
}

/// Local, NATS-free snapshot: flush, gzip the .hrm, write it under snapshots/
/// with a name the retention pruner will never match (it prunes files ending
/// `-<agent>.hrm.gz`; this one ends `-pre-dedupe.hrm.gz`).
fn pre_collapse_snapshot(
    agent_id: &str,
    sys: &mut kannaka_memory::openclaw::KannakaMemorySystem,
) -> Result<(), String> {
    sys.engine.store.flush().map_err(|e| format!("flush: {e}"))?;
    // Snapshot the store that is actually loaded. `data_dir()/kannaka.hrm` is
    // only the default location — a node with a configured `[hrm] path` would
    // otherwise get a restore point for a file this run never touches.
    let hrm = sys
        .engine
        .store
        .hrm_path()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| data_dir().join("kannaka.hrm"));
    let bytes = std::fs::read(&hrm).map_err(|e| format!("read {}: {e}", hrm.display()))?;
    let dir = data_dir().join("snapshots");
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    let ts = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    let path = dir.join(format!("{ts}-{agent_id}-pre-dedupe.hrm.gz"));
    let mut gz = flate2::write::GzEncoder::new(
        Vec::with_capacity(bytes.len() / 4),
        flate2::Compression::default(),
    );
    gz.write_all(&bytes).map_err(|e| format!("gzip: {e}"))?;
    let out = gz.finish().map_err(|e| format!("gzip finish: {e}"))?;
    std::fs::write(&path, &out).map_err(|e| format!("write {}: {e}", path.display()))?;

    // The `.hrm` alone is NOT a restore point for this operation. The collapse
    // flushes, and that flush runs `save_times_seen_merge(prune_stale = true)`,
    // which drops the folded ids out of `.times_seen.json`. Restoring only the
    // medium would give the rows back with their counts reset to 1 while the
    // keeper kept its summed count — and because the sidecar merge only ever
    // RAISES an entry, no later run could correct the inflation. So the sidecars
    // are part of the bundle.
    let mut copied = Vec::new();
    for ext in ["times_seen.json", "reactivation.json", "links.json", "clusters.json"] {
        let side = hrm.with_extension(ext);
        if !side.exists() {
            continue;
        }
        let dest = dir.join(format!("{ts}-{agent_id}-pre-dedupe.{ext}"));
        match std::fs::copy(&side, &dest) {
            Ok(_) => copied.push(ext),
            // A sidecar we cannot copy means an incomplete restore point, and
            // this is the snapshot that gates a one-way delete. Refuse.
            Err(e) => return Err(format!("copy sidecar {}: {e}", side.display())),
        }
    }
    println!(
        "[dedupe] pre-collapse snapshot: {} ({} KB, exempt from retention pruning)",
        path.display(),
        out.len() / 1024
    );
    if copied.is_empty() {
        println!("[dedupe]   no sidecars present to snapshot");
    } else {
        println!("[dedupe]   sidecars in the bundle: {}", copied.join(", "));
    }
    Ok(())
}
