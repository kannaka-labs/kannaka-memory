//! Verified (staged) consolidation, issue #1063: run a dream cycle on a staged
//! copy, check the copy, then commit it or quarantine it.
//!
//! This is infrastructure for the pre-registered #1063 experiment, **not** the
//! experiment. Nothing here runs arms, replays a fault manifest, serves a
//! consumer workload, measures contamination or judges the pre-registration.
//!
//! ## Modes (`KANNAKA_VERIFIED_CONSOLIDATION`)
//!
//! * `off` (default; also unset): `ConsolidationEngine::consolidate` runs the
//!   in-place cycle exactly as before #1063. No staging, no counters touched.
//! * `staged` (arm C): the cycle runs on a staged copy, the structural checks
//!   below run on the copy, and the copy is committed on pass or quarantined on
//!   fail.
//! * `accept` (arm C0): staged and always committed, no checks. Amendment 1
//!   needs it to separate the effect of staging from the effect of checking.
//!
//! An unrecognised value is logged and treated as `off`. The variable is read
//! once, by `KannakaMemorySystem::init_with_engine`, onto
//! `dream_state.engine` (the engine `dream()` actually runs), following
//! the `FluxPublisher::from_env` / `ConsolidateOpts::from_env` pattern. Other
//! callers set `ConsolidationEngine::verified` directly.
//!
//! ## Staging
//!
//! `ResonanceEngine` is not `Clone` (it owns a `Box<dyn MediumBackend>` and an
//! `EncodingPipeline` holding a `Box<dyn TextEncoder>`), so the staged copy is
//! a `TestMedium` seeded with a clone of **every** live row. The stages reach
//! the store only through `insert/get/get_mut/search/all_memories/all_ids/
//! delete` plus `pipeline.bundle` (a pure vector sum, so a placeholder pipeline
//! is enough), and `HrmStore::search` is the same normalised cosine over the
//! row cache as `TestMedium::search` (#498), so the staged run is the same
//! computation as the in-place one, with one known difference: in place,
//! `HrmStore::insert` clamps a new row's amplitude to `[0, 1]` (inc-1b) before
//! later stages see it; staged, that clamp happens only at commit. With
//! default parameters every stage inserts at amplitude <= 1, so it does not
//! bite today. Cost: one
//! extra copy of the store's rows for the duration of a staged cycle.
//!
//! ## Commit
//!
//! Every staged row is written back whole (not a chosen subset of fields), new
//! rows are inserted through the live backend, and rows missing from the
//! staged result are deleted through the live backend.
//!
//! ## Checks: the pre-registered structural set, nothing else
//!
//! Against a [`PreCycle`] snapshot taken before the cycle (Amendment 2):
//! * amplitude, phase and frequency finite and within the engine's bounds, on
//!   every row the cycle created or whose amplitude/phase/frequency it changed.
//!   Rows the cycle left alone are not re-judged (pre-existing corruption is
//!   out of scope for phase 1 and must not block every later dream). Bounds
//!   are what the stages write, widened to the pre-cycle hull: amplitude
//!   `[0, max(AMPLITUDE_CEILING, 1.3 x hallucination_amplitude)]`; phase
//!   `[-PI, TAU]` (`stage_strengthen` stores `circular_mean2`, an un-normalised
//!   atan2 in `[-PI, PI]`, every other stage stores `norm(..)` in `[0, TAU)`).
//! * ids and provenance intact: every pre-cycle row is still present unless it
//!   was already a ghost (see below); `origin_agent`, `created_at`, `content`
//!   and `parents` unchanged (compared as a digest); `merge_history` not
//!   shorter; `sync_version` not lower.
//! * `hallucinated` never cleared on a row that had it before the cycle.
//! * no ghost from this cycle hard-deleted: a row that was live (amplitude
//!   != 0) before the cycle and is gone after it fails, whether or not it was
//!   ghosted first. A row that was already a ghost may be reclaimed.
//!
//! Limit: a row created and deleted inside the same cycle is invisible to the
//! checks (it is in neither snapshot).
//!
//! **Not in this slice:** the behavioural recall probes (issue §3; Amendment 2
//! freezes them with the manifest), arm B's deferred audit and rollback, and
//! the cosine-store arms. [`ConsolidationEngine::check_candidate`] and
//! [`PreCycle`] are public so a deferred audit can reuse the same check set.
//! [`ConsolidationEngine::consolidate_staged_with`] takes an injection hook
//! that runs after the last stage and before any check (Amendment 2's
//! injection point); the dream path passes a no-op.
//!
//! ## Not gated
//!
//! Only `ConsolidationEngine::consolidate` is routed through the gate.
//! `consolidate_incremental`, `consolidate_modality_aware` (the HRM-only
//! modality pass, also run in place after the gated cycle by
//! `consolidate_with_swarm_awareness`), `ConsolidationEngine::dream_lite`, and
//! the wave-native `dream_native` pass in `KannakaMemorySystem::dream` all run
//! in place whatever the mode.
//!
//! ## Quarantine
//!
//! A failed candidate goes to a bounded pool with its reasons, the staged run's
//! report, and per-row scalar diffs (ids plus before/after scalars and the
//! names of changed fields; no vectors or whole rows). The cap,
//! [`QUARANTINE_CAP`], is an operational memory limit on a diagnostic pool,
//! not a check threshold: when full the oldest entry is evicted, counted
//! (`evicted`) and logged with its reasons. A quarantined cycle reports zero
//! applied work to its caller.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, VecDeque};
use std::f32::consts::{PI, TAU};
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::{ConsolidationEngine, ConsolidationReport, AMPLITUDE_CEILING};
use crate::codebook::Codebook;
use crate::encoding::{EncodingPipeline, SimpleHashEncoder};
use crate::memory::HyperMemory;
use crate::store::{MediumBackend, ResonanceEngine, TestMedium};

/// Quarantined cycles kept for diagnosis. An operational memory bound on the
/// pool (each entry holds one scalar diff per changed row), not a check
/// threshold; eviction is counted and logged, never silent.
pub const QUARANTINE_CAP: usize = 8;

/// Reasons recorded per failed check run; the rest are counted in one line.
const MAX_REASONS: usize = 16;

/// Which consolidation path `consolidate` takes (#1063).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VerifiedMode {
    /// In place, unchanged (default).
    #[default]
    Off,
    /// Stage, check, commit on pass, quarantine on fail (arm C).
    Staged,
    /// Stage and always commit, no checks (arm C0).
    StagedAcceptAll,
}

impl VerifiedMode {
    /// The environment variable `from_env` reads.
    pub const ENV: &'static str = "KANNAKA_VERIFIED_CONSOLIDATION";

    /// `off` | `staged` | `accept` (case-insensitive). `None` if unrecognised.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "" | "off" | "0" | "false" => Some(VerifiedMode::Off),
            "staged" | "on" => Some(VerifiedMode::Staged),
            "accept" | "staged_accept" => Some(VerifiedMode::StagedAcceptAll),
            _ => None,
        }
    }

    /// Read [`VerifiedMode::ENV`]. Unset means `Off`; an unrecognised value is
    /// logged and also means `Off`.
    pub fn from_env() -> Self {
        match std::env::var(Self::ENV) {
            Err(_) => VerifiedMode::Off,
            Ok(v) => Self::parse(&v).unwrap_or_else(|| {
                eprintln!(
                    "[consolidation] {}={v:?} not recognised (off|staged|accept); verified consolidation stays OFF",
                    Self::ENV
                );
                VerifiedMode::Off
            }),
        }
    }
}

/// What happened to a cycle, carried on `ConsolidationReport::staged`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StagedStatus {
    /// Ran in place (mode `Off`, or a path that is not gated).
    #[default]
    NotStaged,
    /// Staged and written to the live store.
    Committed,
    /// Staged, failed its checks, not written; kept in the quarantine pool.
    Quarantined,
}

/// Staging mode plus counters and the quarantine pool. Counters are atomics
/// because `consolidate` takes `&self`.
#[derive(Debug, Default)]
pub struct VerifiedConsolidation {
    pub mode: VerifiedMode,
    cycles: AtomicU64,
    committed: AtomicU64,
    quarantined: AtomicU64,
    evicted: AtomicU64,
    pool: Mutex<VecDeque<QuarantinedCycle>>,
}

/// Counter snapshot. `quarantined == pool_len + evicted` always holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VerifiedStats {
    pub committed: u64,
    pub quarantined: u64,
    pub evicted: u64,
    pub pool_len: usize,
}

/// Before/after scalars of one row.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RowScalars {
    pub amplitude: f32,
    pub phase: f32,
    pub frequency: f32,
    pub layer_depth: u8,
    pub hallucinated: bool,
    pub connections: usize,
}

impl RowScalars {
    fn of(m: &HyperMemory) -> Self {
        Self {
            amplitude: m.amplitude,
            phase: m.phase,
            frequency: m.frequency,
            layer_depth: m.layer_depth,
            hallucinated: m.hallucinated,
            connections: m.connections.len(),
        }
    }
}

/// One row the quarantined candidate would have changed. `before` is `None`
/// for a row the cycle created, `after` is `None` for a row it deleted.
#[derive(Debug, Clone)]
pub struct RowDiff {
    pub id: Uuid,
    pub before: Option<RowScalars>,
    pub after: Option<RowScalars>,
    /// Names of the fields that differ (empty for created/deleted rows).
    pub fields: Vec<&'static str>,
}

/// A staged candidate that failed its checks.
#[derive(Debug, Clone)]
pub struct QuarantinedCycle {
    /// 1-based sequence number of the staged cycle on this engine.
    pub seq: u64,
    pub at: DateTime<Utc>,
    pub reasons: Vec<String>,
    /// The staged run's own report (never returned to the caller).
    pub report: ConsolidationReport,
    pub diffs: Vec<RowDiff>,
}

impl VerifiedConsolidation {
    pub fn new(mode: VerifiedMode) -> Self {
        Self { mode, ..Self::default() }
    }

    pub fn stats(&self) -> VerifiedStats {
        VerifiedStats {
            committed: self.committed.load(Ordering::Relaxed),
            quarantined: self.quarantined.load(Ordering::Relaxed),
            evicted: self.evicted.load(Ordering::Relaxed),
            pool_len: self.lock_pool().len(),
        }
    }

    /// Take every quarantined cycle out of the pool (for inspection / replay).
    pub fn drain_quarantine(&self) -> Vec<QuarantinedCycle> {
        self.lock_pool().drain(..).collect()
    }

    fn lock_pool(&self) -> std::sync::MutexGuard<'_, VecDeque<QuarantinedCycle>> {
        self.pool.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn quarantine(&self, entry: QuarantinedCycle) {
        let mut pool = self.lock_pool();
        while pool.len() >= QUARANTINE_CAP {
            if let Some(old) = pool.pop_front() {
                self.evicted.fetch_add(1, Ordering::Relaxed);
                eprintln!(
                    "[consolidation] quarantine full (cap {QUARANTINE_CAP}): evicting staged cycle #{} (reasons: {})",
                    old.seq,
                    old.reasons.join("; ")
                );
            }
        }
        pool.push_back(entry);
        self.quarantined.fetch_add(1, Ordering::Relaxed);
    }
}

/// Per-row record taken before a cycle.
#[derive(Debug, Clone)]
struct PreRow {
    amplitude: f32,
    phase: f32,
    frequency: f32,
    hallucinated: bool,
    provenance: u64,
    merge_len: usize,
    sync_version: u64,
}

/// Pre-cycle snapshot: per-row scalars and provenance digest, plus the
/// pre-cycle hull the numeric bounds are widened to.
#[derive(Debug, Clone)]
pub struct PreCycle {
    rows: HashMap<Uuid, PreRow>,
    amp_hi: f32,
    freq_lo: f32,
    freq_hi: f32,
    phase_lo: f32,
    phase_hi: f32,
}

/// Digest of the fields a cycle must never change: `origin_agent`,
/// `created_at`, `content`, `parents`.
fn provenance_digest(m: &HyperMemory) -> u64 {
    let mut h = DefaultHasher::new();
    m.origin_agent.hash(&mut h);
    m.created_at.hash(&mut h);
    m.content.hash(&mut h);
    m.parents.hash(&mut h);
    h.finish()
}

impl PreCycle {
    /// Snapshot every row of `engine`.
    pub fn capture(engine: &ResonanceEngine) -> Self {
        let mut rows = HashMap::new();
        let mut amp_hi = 0.0f32;
        let default_freq = crate::wave::WaveParams::default().frequency;
        let (mut freq_lo, mut freq_hi) = (default_freq, default_freq);
        let (mut phase_lo, mut phase_hi) = (-PI, TAU);
        for m in engine.store.all_memories().unwrap_or_default() {
            if m.amplitude.is_finite() {
                amp_hi = amp_hi.max(m.amplitude);
            }
            if m.frequency.is_finite() {
                freq_lo = freq_lo.min(m.frequency);
                freq_hi = freq_hi.max(m.frequency);
            }
            if m.phase.is_finite() {
                phase_lo = phase_lo.min(m.phase);
                phase_hi = phase_hi.max(m.phase);
            }
            rows.insert(
                m.id,
                PreRow {
                    amplitude: m.amplitude,
                    phase: m.phase,
                    frequency: m.frequency,
                    hallucinated: m.hallucinated,
                    provenance: provenance_digest(m),
                    merge_len: m.merge_history.len(),
                    sync_version: m.sync_version,
                },
            );
        }
        Self { rows, amp_hi, freq_lo, freq_hi, phase_lo, phase_hi }
    }

    /// Number of rows in the snapshot.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

/// Placeholder pipeline for the staged engine. The stages only call
/// `pipeline.bundle`, which is a pure vector sum.
fn staging_pipeline() -> EncodingPipeline {
    EncodingPipeline::new(Box::new(SimpleHashEncoder::new(1, 0)), Codebook::new(1, 1, 0))
}

/// Names of the fields that differ between `a` (live) and `b` (staged).
/// Diagnostic only: commit writes whole rows and does not consult this.
fn changed_fields(a: &HyperMemory, b: &HyperMemory) -> Vec<&'static str> {
    let links = |m: &HyperMemory| -> Vec<(Uuid, u32, u8)> {
        m.connections.iter().map(|l| (l.target_id, l.strength.to_bits(), l.span)).collect()
    };
    let mut f = Vec::new();
    let mut diff = |changed: bool, name: &'static str| {
        if changed {
            f.push(name);
        }
    };
    diff(a.vector != b.vector, "vector");
    diff(a.amplitude.to_bits() != b.amplitude.to_bits(), "amplitude");
    diff(a.frequency.to_bits() != b.frequency.to_bits(), "frequency");
    diff(a.phase.to_bits() != b.phase.to_bits(), "phase");
    diff(a.decay_rate.to_bits() != b.decay_rate.to_bits(), "decay_rate");
    diff(a.created_at != b.created_at, "created_at");
    diff(a.layer_depth != b.layer_depth, "layer_depth");
    diff(links(a) != links(b), "connections");
    diff(a.content != b.content, "content");
    diff(a.hallucinated != b.hallucinated, "hallucinated");
    diff(a.parents != b.parents, "parents");
    diff(a.geometry != b.geometry, "geometry");
    diff(a.xi_signature != b.xi_signature, "xi_signature");
    diff(a.origin_agent != b.origin_agent, "origin_agent");
    diff(a.sync_version != b.sync_version, "sync_version");
    diff(a.merge_history.len() != b.merge_history.len(), "merge_history");
    diff(a.last_consolidated_at != b.last_consolidated_at, "last_consolidated_at");
    diff(a.disputed != b.disputed, "disputed");
    diff(a.updated_at != b.updated_at, "updated_at");
    diff(a.retrieval_count != b.retrieval_count, "retrieval_count");
    diff(a.times_seen != b.times_seen, "times_seen");
    diff(a.modality != b.modality, "modality");
    diff(a.tier != b.tier, "tier");
    diff(a.effective_at != b.effective_at, "effective_at");
    diff(a.observed_at != b.observed_at, "observed_at");
    diff(a.expires_at != b.expires_at, "expires_at");
    f
}

/// Scalar diffs between the live store and a staged candidate.
fn diff_rows(live: &ResonanceEngine, staged: &ResonanceEngine) -> Vec<RowDiff> {
    let mut out = Vec::new();
    for s in staged.store.all_memories().unwrap_or_default() {
        match live.store.get(&s.id).ok().flatten() {
            Some(l) => {
                let fields = changed_fields(l, s);
                if !fields.is_empty() {
                    out.push(RowDiff { id: s.id, before: Some(RowScalars::of(l)), after: Some(RowScalars::of(s)), fields });
                }
            }
            None => out.push(RowDiff { id: s.id, before: None, after: Some(RowScalars::of(s)), fields: Vec::new() }),
        }
    }
    for l in live.store.all_memories().unwrap_or_default() {
        if staged.store.get(&l.id).ok().flatten().is_none() {
            out.push(RowDiff { id: l.id, before: Some(RowScalars::of(l)), after: None, fields: Vec::new() });
        }
    }
    out
}

impl ConsolidationEngine {
    /// One staged cycle: copy the live store, run the in-place cycle on the
    /// copy, call `inject` on the copy (after the last stage, before any
    /// check: Amendment 2's injection point; pass `|_| {}` outside the
    /// experiment), then commit or quarantine according to `verified.mode`.
    ///
    /// The returned report is the staged run's report when committed. When
    /// quarantined it carries `staged = Quarantined`, the reason and the
    /// cycle's wall time, and every count is zero, because nothing reached the
    /// live store; the staged report is kept only in the quarantine pool.
    ///
    /// With mode `Off` this still stages and commits without checks (it is
    /// called explicitly); `consolidate` never calls it in that mode.
    pub fn consolidate_staged_with<F: FnOnce(&mut ResonanceEngine)>(
        &self,
        engine: &mut ResonanceEngine,
        min_layer: u8,
        max_layer: u8,
        inject: F,
    ) -> ConsolidationReport {
        let start = Instant::now();
        let seq = self.verified.cycles.fetch_add(1, Ordering::Relaxed) + 1;
        let check = self.verified.mode == VerifiedMode::Staged;
        let pre = if check { Some(PreCycle::capture(engine)) } else { None };

        let mut medium = TestMedium::new();
        for m in engine.store.all_memories().unwrap_or_default() {
            let _ = medium.insert(m.clone());
        }
        let mut staged = ResonanceEngine::new(Box::new(medium), staging_pipeline());
        staged.similarity_threshold = engine.similarity_threshold;

        let mut report = self.consolidate_in_place(&mut staged, min_layer, max_layer);
        inject(&mut staged);

        let reasons = match &pre {
            Some(pre) => self.check_candidate(pre, &staged),
            None => Vec::new(),
        };

        if !reasons.is_empty() {
            let diffs = diff_rows(engine, &staged);
            let joined = reasons.join("; ");
            eprintln!(
                "[consolidation] staged cycle #{seq} QUARANTINED, live store untouched ({} rows differ): {joined}",
                diffs.len()
            );
            report.staged = StagedStatus::Quarantined;
            report.quarantine_reason = Some(joined.clone());
            self.verified.quarantine(QuarantinedCycle { seq, at: Utc::now(), reasons, report, diffs });
            return ConsolidationReport {
                staged: StagedStatus::Quarantined,
                quarantine_reason: Some(joined),
                duration_ms: start.elapsed().as_millis() as u64,
                ..Default::default()
            };
        }

        // Commit: every staged row written back whole, new rows inserted and
        // vanished rows deleted through the live backend.
        let staged_ids = staged.store.all_ids().unwrap_or_default();
        let mut keep: std::collections::HashSet<Uuid> = std::collections::HashSet::with_capacity(staged_ids.len());
        for id in staged_ids {
            keep.insert(id);
            let Some(row) = staged.store.get(&id).ok().flatten().cloned() else { continue };
            match engine.store.get_mut(&id).ok().flatten() {
                Some(live) => *live = row,
                None => {
                    let _ = engine.store.insert(row);
                }
            }
        }
        for id in engine.store.all_ids().unwrap_or_default() {
            if !keep.contains(&id) {
                let _ = engine.store.delete(&id);
            }
        }
        self.verified.committed.fetch_add(1, Ordering::Relaxed);
        report.staged = StagedStatus::Committed;
        report.duration_ms = start.elapsed().as_millis() as u64;
        report
    }

    /// The pre-registered structural checks (see the module doc) on a
    /// post-cycle `post` against the pre-cycle snapshot. Empty means pass.
    /// Usable on a staged candidate (arm C) or, later, by a deferred audit.
    pub fn check_candidate(&self, pre: &PreCycle, post: &ResonanceEngine) -> Vec<String> {
        let mut reasons: Vec<String> = Vec::new();
        let mut extra = 0usize;
        let mut bad = |s: String| {
            if reasons.len() < MAX_REASONS {
                reasons.push(s);
            } else {
                extra += 1;
            }
        };
        let amp_hi = pre.amp_hi.max(AMPLITUDE_CEILING).max(self.hallucination_amplitude * 1.3);

        // ids, provenance, hallucinated flag, hard deletes: against the snapshot.
        for (id, p) in &pre.rows {
            let Some(m) = post.store.get(id).ok().flatten() else {
                if p.amplitude != 0.0 {
                    bad(format!("{id}: live before the cycle (amp {}), hard-deleted by it", p.amplitude));
                }
                continue;
            };
            if p.hallucinated && !m.hallucinated {
                bad(format!("{id}: hallucinated flag cleared"));
            }
            if provenance_digest(m) != p.provenance {
                bad(format!("{id}: provenance (origin_agent/created_at/content/parents) changed"));
            }
            if m.merge_history.len() < p.merge_len {
                bad(format!("{id}: merge_history shortened"));
            }
            if m.sync_version < p.sync_version {
                bad(format!("{id}: sync_version went backwards"));
            }
        }

        // Numeric bounds: rows created this cycle or whose scalars it changed.
        for m in post.store.all_memories().unwrap_or_default() {
            let touched = match pre.rows.get(&m.id) {
                None => true,
                Some(p) => {
                    p.amplitude.to_bits() != m.amplitude.to_bits()
                        || p.phase.to_bits() != m.phase.to_bits()
                        || p.frequency.to_bits() != m.frequency.to_bits()
                }
            };
            if !touched {
                continue;
            }
            let id = m.id;
            if !(m.amplitude.is_finite() && m.phase.is_finite() && m.frequency.is_finite()) {
                bad(format!("{id}: non-finite amplitude/phase/frequency ({}, {}, {})", m.amplitude, m.phase, m.frequency));
                continue;
            }
            if m.amplitude < 0.0 || m.amplitude > amp_hi {
                bad(format!("{id}: amplitude {} outside [0, {amp_hi}]", m.amplitude));
            }
            if m.phase < pre.phase_lo || m.phase > pre.phase_hi {
                bad(format!("{id}: phase {} outside [{}, {}]", m.phase, pre.phase_lo, pre.phase_hi));
            }
            // Regression guard: no stage writes `.frequency` today and new rows
            // take WaveParams::default().frequency, which is inside the hull by
            // construction, so this cannot fire on a current cycle. It exists
            // to catch a future stage that does.
            if m.frequency < pre.freq_lo || m.frequency > pre.freq_hi {
                bad(format!("{id}: frequency {} outside [{}, {}]", m.frequency, pre.freq_lo, pre.freq_hi));
            }
        }
        if extra > 0 {
            reasons.push(format!("... and {extra} more"));
        }
        reasons
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::{Rng, SeedableRng};
    use rand_chacha::ChaCha8Rng;

    const DIM: usize = 256;

    fn engine_with(mems: &[HyperMemory]) -> ResonanceEngine {
        let mut e = ResonanceEngine::new(
            Box::new(TestMedium::new()),
            EncodingPipeline::new(Box::new(SimpleHashEncoder::new(8, 42)), Codebook::new(8, 16, 42)),
        );
        for m in mems {
            e.store.insert(m.clone()).unwrap();
        }
        e
    }

    /// Four topic clusters of near-duplicate vectors with phases near each
    /// topic's centre, so a cycle finds constructive pairs and changes rows.
    /// Row 0 is a pre-existing hallucination; row 1 is a pre-existing ghost.
    fn corpus() -> Vec<HyperMemory> {
        let mut rng = ChaCha8Rng::seed_from_u64(1063);
        let mut out = Vec::new();
        for t in 0..4 {
            let base: Vec<f32> = (0..DIM).map(|_| rng.gen_range(-1.0..1.0)).collect();
            for i in 0..6 {
                let mut v: Vec<f32> = base.iter().map(|b| b + rng.gen_range(-0.1..0.1f32)).collect();
                crate::wave::normalize(&mut v);
                let mut m = HyperMemory::new(v, format!("topic {t} fact {i}"));
                m.id = Uuid::from_u128(((t * 100 + i) as u128) + 1);
                m.phase = (TAU * t as f32 / 4.0 + rng.gen_range(-0.3..0.3f32)).rem_euclid(TAU);
                out.push(m);
            }
        }
        out[0].hallucinated = true;
        out[1].amplitude = 0.0;
        out
    }

    /// Every row field a cycle can touch, minus wall-clock timestamps (which
    /// differ between two runs microseconds apart).
    #[derive(Debug, PartialEq)]
    struct Row {
        id: Uuid,
        vector: Vec<u32>,
        scalars: [u32; 4], // amplitude, phase, frequency, decay_rate bits
        layer_depth: u8,
        flags: (bool, bool), // hallucinated, disputed
        connections: Vec<(Uuid, u32, u8)>,
        xi_signature: Vec<u32>,
        content: String,
        parents: Vec<String>,
        sync_version: u64,
        merge_len: usize,
        retrieval_count: u32,
        tier: crate::medium::types::Tier,
    }

    fn fingerprint(e: &ResonanceEngine) -> (usize, Vec<Row>) {
        let bits = |v: &[f32]| v.iter().map(|x| x.to_bits()).collect::<Vec<u32>>();
        let rows = e
            .store
            .all_memories()
            .unwrap()
            .iter()
            .map(|m| Row {
                id: m.id,
                vector: bits(&m.vector),
                scalars: [m.amplitude.to_bits(), m.phase.to_bits(), m.frequency.to_bits(), m.decay_rate.to_bits()],
                layer_depth: m.layer_depth,
                flags: (m.hallucinated, m.disputed),
                connections: m.connections.iter().map(|l| (l.target_id, l.strength.to_bits(), l.span)).collect(),
                xi_signature: bits(&m.xi_signature),
                content: m.content.clone(),
                parents: m.parents.clone(),
                sync_version: m.sync_version,
                merge_len: m.merge_history.len(),
                retrieval_count: m.retrieval_count,
                tier: m.tier,
            })
            .collect();
        (e.store.count(), rows)
    }

    fn engine_in(mode: VerifiedMode) -> ConsolidationEngine {
        ConsolidationEngine { verified: VerifiedConsolidation::new(mode), ..ConsolidationEngine::default() }
    }

    fn first_live_row(e: &ResonanceEngine) -> Uuid {
        e.store.all_memories().unwrap().iter().find(|m| m.amplitude > 0.0 && !m.hallucinated).unwrap().id
    }

    #[test]
    fn mode_parses_and_defaults_off() {
        assert_eq!(VerifiedMode::default(), VerifiedMode::Off);
        assert_eq!(ConsolidationEngine::default().verified.mode, VerifiedMode::Off);
        assert_eq!(VerifiedMode::parse("off"), Some(VerifiedMode::Off));
        assert_eq!(VerifiedMode::parse(""), Some(VerifiedMode::Off));
        assert_eq!(VerifiedMode::parse("Staged"), Some(VerifiedMode::Staged));
        assert_eq!(VerifiedMode::parse("accept"), Some(VerifiedMode::StagedAcceptAll));
        assert_eq!(VerifiedMode::parse("yes please"), None);
    }

    #[test]
    fn flag_off_matches_in_place_path_exactly() {
        let mems = corpus();
        let (mut a, mut b) = (engine_with(&mems), engine_with(&mems));
        let ce = ConsolidationEngine::default();
        let before = fingerprint(&a);
        let ra = ce.consolidate(&mut a, 0, 2);
        let rb = ce.consolidate_in_place(&mut b, 0, 2);
        assert_ne!(fingerprint(&a), before, "fixture must make the cycle change the store");
        assert_eq!(fingerprint(&a), fingerprint(&b));
        assert_eq!(ra.staged, StagedStatus::NotStaged);
        assert_eq!(ra.memories_strengthened, rb.memories_strengthened);
        assert_eq!(ra.memories_pruned, rb.memories_pruned);
        assert_eq!(ra.hallucinations_created, rb.hallucinations_created);
        assert_eq!(ce.verified.stats(), VerifiedStats::default());
    }

    #[test]
    fn passing_staged_cycle_commits_the_same_state_as_in_place() {
        let mems = corpus();
        let (mut live, mut reference) = (engine_with(&mems), engine_with(&mems));
        let ce = engine_in(VerifiedMode::Staged);
        let before = fingerprint(&live);
        let r = ce.consolidate(&mut live, 0, 2);
        assert_eq!(r.staged, StagedStatus::Committed, "clean cycle quarantined: {:?}", r.quarantine_reason);
        assert_ne!(fingerprint(&live), before, "commit must change the live store");
        ConsolidationEngine::default().consolidate_in_place(&mut reference, 0, 2);
        assert_eq!(fingerprint(&live), fingerprint(&reference), "staged commit must equal the in-place result");
        assert_eq!(ce.verified.stats(), VerifiedStats { committed: 1, ..Default::default() });
    }

    /// Run one staged cycle with `inject`, assert it was quarantined and the
    /// live store's full fingerprint did not move. Returns the reason.
    fn assert_quarantined(inject: impl FnOnce(&mut ResonanceEngine)) -> String {
        let mut live = engine_with(&corpus());
        let ce = engine_in(VerifiedMode::Staged);
        let before = fingerprint(&live);
        let r = ce.consolidate_staged_with(&mut live, 0, 2, inject);
        assert_eq!(r.staged, StagedStatus::Quarantined);
        assert_eq!(fingerprint(&live), before, "quarantine must leave the live store untouched");
        assert_eq!(
            (r.memories_replayed, r.memories_strengthened, r.memories_pruned, r.hallucinations_created, r.skip_links_created),
            (0, 0, 0, 0, 0),
            "a quarantined cycle reports no applied work"
        );
        let pool = ce.verified.drain_quarantine();
        assert_eq!(pool.len(), 1);
        assert!(pool[0].report.memories_replayed > 0, "the staged report is kept in the pool");
        assert!(!pool[0].diffs.is_empty());
        r.quarantine_reason.expect("quarantine carries a reason")
    }

    #[test]
    fn nan_amplitude_quarantines() {
        let reason = assert_quarantined(|s| {
            let id = first_live_row(s);
            s.store.get_mut(&id).unwrap().unwrap().amplitude = f32::NAN;
        });
        assert!(reason.contains("non-finite"), "{reason}");
    }

    #[test]
    fn cleared_hallucinated_flag_quarantines() {
        let reason = assert_quarantined(|s| {
            s.store.get_mut(&Uuid::from_u128(1)).unwrap().unwrap().hallucinated = false;
        });
        assert!(reason.contains("hallucinated flag cleared"), "{reason}");
    }

    #[test]
    fn changed_provenance_quarantines() {
        let reason = assert_quarantined(|s| {
            let id = first_live_row(s);
            s.store.get_mut(&id).unwrap().unwrap().content.push_str(" (production deployment complete)");
        });
        assert!(reason.contains("provenance"), "{reason}");
    }

    #[test]
    fn fresh_ghost_hard_delete_quarantines() {
        let reason = assert_quarantined(|s| {
            let id = first_live_row(s);
            s.store.get_mut(&id).unwrap().unwrap().amplitude = 0.0;
            s.store.delete(&id).unwrap();
        });
        assert!(reason.contains("hard-deleted"), "{reason}");
    }

    #[test]
    fn already_ghosted_row_may_be_reclaimed() {
        let ghost = Uuid::from_u128(2);
        let mut live = engine_with(&corpus());
        assert_eq!(live.store.get(&ghost).unwrap().unwrap().amplitude, 0.0);
        let ce = engine_in(VerifiedMode::Staged);
        let r = ce.consolidate_staged_with(&mut live, 0, 2, |s| {
            s.store.delete(&ghost).unwrap();
        });
        assert_eq!(r.staged, StagedStatus::Committed, "{:?}", r.quarantine_reason);
        assert!(live.store.get(&ghost).unwrap().is_none(), "the reclaim is committed");
    }

    #[test]
    fn accept_all_commits_a_faulty_candidate() {
        let mut live = engine_with(&corpus());
        let ce = engine_in(VerifiedMode::StagedAcceptAll);
        let victim = first_live_row(&live);
        let r = ce.consolidate_staged_with(&mut live, 0, 2, |s| {
            s.store.get_mut(&victim).unwrap().unwrap().amplitude = f32::NAN;
        });
        assert_eq!(r.staged, StagedStatus::Committed);
        assert!(live.store.get(&victim).unwrap().unwrap().amplitude.is_nan());
        assert_eq!(ce.verified.stats(), VerifiedStats { committed: 1, ..Default::default() });
    }

    #[test]
    fn commit_writes_back_every_changed_field() {
        let mut live = engine_with(&corpus());
        let ce = engine_in(VerifiedMode::Staged);
        let id = first_live_row(&live);
        let r = ce.consolidate_staged_with(&mut live, 0, 2, |s| {
            let m = s.store.get_mut(&id).unwrap().unwrap();
            m.decay_rate = 0.123;
            m.times_seen = 77;
            m.expires_at = Some(Utc::now());
        });
        assert_eq!(r.staged, StagedStatus::Committed, "{:?}", r.quarantine_reason);
        let m = live.store.get(&id).unwrap().unwrap();
        assert_eq!((m.decay_rate, m.times_seen, m.expires_at.is_some()), (0.123, 77, true));
    }

    #[test]
    fn quarantine_accounting_is_held_plus_evicted() {
        let mut live = engine_with(&corpus());
        let ce = engine_in(VerifiedMode::Staged);
        let runs = QUARANTINE_CAP + 3;
        for _ in 0..runs {
            let r = ce.consolidate_staged_with(&mut live, 0, 2, |s| {
                let id = first_live_row(s);
                s.store.get_mut(&id).unwrap().unwrap().phase = f32::INFINITY;
            });
            assert_eq!(r.staged, StagedStatus::Quarantined);
        }
        let s = ce.verified.stats();
        assert_eq!(s.quarantined, runs as u64);
        assert_eq!(s.pool_len, QUARANTINE_CAP);
        assert_eq!(s.evicted, 3);
        assert_eq!(s.quarantined, s.pool_len as u64 + s.evicted, "every quarantine is either held or counted as evicted");
        assert_eq!(s.committed, 0);
        let seqs: Vec<u64> = ce.verified.drain_quarantine().iter().map(|q| q.seq).collect();
        assert_eq!(seqs, (4..=runs as u64).collect::<Vec<_>>(), "the oldest entries are the ones evicted");
    }
}
