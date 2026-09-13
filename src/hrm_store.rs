//! HRM-backed memory store that implements the MediumBackend trait.
//!
//! This provides the primary storage backend using the
//! Holographic Resonance Medium as the storage backend.

use std::collections::HashMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::memory::HyperMemory;
use crate::store::{MediumBackend, StoreError};
use crate::encoding::EncodingPipeline;
use crate::medium::{Medium, Resonance};
use crate::medium::types::{Tier, ConsolidateOpts, ConsolidateMode, ConsolidateReport};
use crate::medium::types::{DEFAULT_BELIEF_ABSORB_FRAC};
use crate::medium::chiral::{ChiralMedium, ChiralConsciousness};

// ───────────────────────────────────────────────────────────────────────────
// ADR-0036 belief-safe merge — the single grouping pass shared by
// `plan_consolidation` (read-only) and `apply_consolidation` (mutating).
// Centralising the criteria here is what makes the dry-run projection and the
// destructive apply provably agree about which memories merge, and it is the one
// place the belief-safety guardrails live (mean-centered semantic gate + the
// per-pass absorb cap).
// ───────────────────────────────────────────────────────────────────────────

/// Outcome of the shared grouping pass. `admitted` holds the groups that will
/// actually be merged (each a list of member indices into the caller's
/// id-ordered item slice, length ≥ 2); the `*_before_cap` tallies record what the
/// criteria found before the absorb cap trimmed the plan.
///
/// Public so external harnesses (the L7 belief research arm) can run the
/// EXACT ADR-0036 grouping — semantic gate, belief centering, absorb cap —
/// against their own substrate instead of re-implementing an approximation.
pub struct MergeGrouping {
    pub admitted: Vec<Vec<usize>>,
    pub groups_before_cap: usize,
    pub absorb_before_cap: usize,
    pub absorb_cap: Option<usize>,
    pub centered: bool,
}

impl MergeGrouping {
    pub fn admitted_absorb(&self) -> usize {
        self.admitted.iter().map(|m| m.len().saturating_sub(1)).sum()
    }
    fn capped(&self) -> bool {
        self.admitted_absorb() < self.absorb_before_cap
    }
}

/// Group redundant, phase-locked, non-`Pinned` items and apply the per-pass
/// absorb cap.
///
/// The redundancy criterion mirrors ADR-0036 M1: two items merge iff their
/// vectors are near-parallel (cosine ≥ threshold) AND constructively phase-locked
/// (cos Δphase ≥ `merge_phase_cos`). Two belief-safety changes ride on `belief`:
///
///  * **Semantic gate on the mean-CENTERED embedding.** Real embeddings are
///    anisotropic (cone-clustered — the same root as `num_clusters=1`), so raw
///    cosine is inflated: genuinely-distinct memories clear 0.92 on the shared
///    component alone. The belief phase is itself a lossy 2-D projection of that
///    same centered embedding, so it rubber-stamps the inflated cosine groups
///    rather than discriminating — this is how one dream absorbed 295→82.
///    Centering removes the shared component so only genuine *residual*
///    redundancy groups, and the floor rises to `merge_sim_belief`.
///  * **Per-pass absorb cap.** Groups are admitted in descending cohesion order
///    until the cap (`max_absorb_frac` of the field) is reached; the rest are
///    left intact and the caller logs loudly. This bounds ANY over-grouping —
///    the 295→82 becomes ~295→236 — independent of whether the criteria were
///    fooled.
///
/// The default (belief-off, no explicit `max_absorb_frac`) path is byte-identical
/// to the pre-existing raw-cosine grouping with every group admitted.
pub fn compute_merge_grouping(
    vectors: &[&[f32]],
    phases: &[f32],
    tiers: &[Tier],
    energies: &[f32],
    opts: &ConsolidateOpts,
    belief: bool,
) -> MergeGrouping {
    use ndarray::Array2;

    let n = vectors.len();
    let mut grouping = MergeGrouping {
        admitted: Vec::new(),
        groups_before_cap: 0,
        absorb_before_cap: 0,
        absorb_cap: None,
        centered: belief,
    };
    if n < 2 {
        return grouping;
    }

    let dim = vectors.iter().map(|v| v.len()).find(|&l| l > 0).unwrap_or(0);
    if dim == 0 {
        return grouping;
    }

    // Build the N×D matrix; rows with a mismatched/empty vector or a Pinned tier
    // are marked unusable (never grouped).
    let mut mat = Array2::<f32>::zeros((n, dim));
    let mut usable = vec![false; n];
    for (i, v) in vectors.iter().enumerate() {
        if v.len() == dim && tiers[i] != Tier::Pinned {
            for (d, &x) in v.iter().enumerate() {
                mat[[i, d]] = x;
            }
            usable[i] = true;
        }
    }

    // Belief path: mean-center the usable rows so the cosine gate measures genuine
    // residual redundancy, not the shared anisotropic component.
    if belief {
        let mut mean = vec![0.0f32; dim];
        let mut cnt = 0usize;
        for i in 0..n {
            if usable[i] {
                for d in 0..dim {
                    mean[d] += mat[[i, d]];
                }
                cnt += 1;
            }
        }
        if cnt > 0 {
            for m in mean.iter_mut() {
                *m /= cnt as f32;
            }
            for i in 0..n {
                if usable[i] {
                    for d in 0..dim {
                        mat[[i, d]] -= mean[d];
                    }
                }
            }
        }
    }

    // One matmul for all pairwise dot products; norms from the diagonal.
    let gram = mat.dot(&mat.t());
    let norms: Vec<f32> = (0..n).map(|i| gram[[i, i]].max(0.0).sqrt()).collect();
    let cos_p: Vec<f32> = phases.iter().map(|p| p.cos()).collect();
    let sin_p: Vec<f32> = phases.iter().map(|p| p.sin()).collect();

    let sim_thresh = if belief {
        opts.merge_sim.max(opts.merge_sim_belief)
    } else {
        opts.merge_sim
    };

    fn find(parent: &mut [usize], mut x: usize) -> usize {
        while parent[x] != x {
            parent[x] = parent[parent[x]];
            x = parent[x];
        }
        x
    }
    let mut parent: Vec<usize> = (0..n).collect();
    let eps = 1e-6_f32;
    for i in 0..n {
        if !usable[i] || norms[i] < eps {
            continue;
        }
        for j in (i + 1)..n {
            if !usable[j] || norms[j] < eps {
                continue;
            }
            let cos_sim = gram[[i, j]] / (norms[i] * norms[j]);
            if cos_sim < sim_thresh {
                continue;
            }
            let phase_coh = cos_p[i] * cos_p[j] + sin_p[i] * sin_p[j];
            if phase_coh < opts.merge_phase_cos {
                continue;
            }
            let (ri, rj) = (find(&mut parent, i), find(&mut parent, j));
            if ri != rj {
                parent[ri.max(rj)] = ri.min(rj);
            }
        }
    }

    // Collect members per root; keep only real groups (size ≥ 2).
    let mut roots: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..n {
        if !usable[i] {
            continue;
        }
        let r = find(&mut parent, i);
        roots.entry(r).or_default().push(i);
    }
    let mut group_vec: Vec<Vec<usize>> = roots.into_values().filter(|m| m.len() >= 2).collect();

    grouping.groups_before_cap = group_vec.len();
    grouping.absorb_before_cap = group_vec.iter().map(|m| m.len() - 1).sum();

    // Effective absorb cap. `max_absorb_frac` overrides; otherwise belief caps at
    // DEFAULT_BELIEF_ABSORB_FRAC and the non-belief default is uncapped (so the
    // pre-existing path is byte-identical).
    let frac = opts
        .max_absorb_frac
        .unwrap_or(if belief { DEFAULT_BELIEF_ABSORB_FRAC } else { 1.0 });
    let cap: Option<usize> = if frac >= 1.0 {
        None
    } else {
        Some((frac * n as f32).floor() as usize)
    };
    grouping.absorb_cap = cap;

    match cap {
        None => {
            // Admit every group; order is irrelevant to the resulting set.
            grouping.admitted = group_vec;
        }
        Some(cap_n) => {
            // Cohesion = mean cosine of members to the group's strongest (max-
            // energy) member, on the SAME (centered-or-raw) vectors used to group.
            // Tightest clusters are admitted first; a lone huge transitive blob is
            // the first thing dropped.
            let cohesion = |members: &[usize]| -> f32 {
                let rep = *members
                    .iter()
                    .max_by(|&&a, &&b| {
                        energies[a].partial_cmp(&energies[b]).unwrap_or(std::cmp::Ordering::Equal)
                    })
                    .unwrap();
                if norms[rep] < eps {
                    return 0.0;
                }
                let mut sum = 0.0f32;
                let mut c = 0.0f32;
                for &mi in members {
                    if mi == rep || norms[mi] < eps {
                        continue;
                    }
                    sum += gram[[rep, mi]] / (norms[rep] * norms[mi]);
                    c += 1.0;
                }
                if c == 0.0 { 0.0 } else { sum / c }
            };
            let mut scored: Vec<(f32, usize, Vec<usize>)> = group_vec
                .drain(..)
                .map(|m| {
                    let coh = cohesion(&m);
                    let min_idx = *m.iter().min().unwrap();
                    (coh, min_idx, m)
                })
                .collect();
            // Descending cohesion, then smallest member index for determinism.
            scored.sort_by(|a, b| {
                b.0.partial_cmp(&a.0)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(a.1.cmp(&b.1))
            });
            let mut absorbed = 0usize;
            for (_, _, m) in scored {
                let this = m.len() - 1;
                if absorbed + this <= cap_n {
                    absorbed += this;
                    grouping.admitted.push(m);
                } else if opts.merge_partial_groups && absorbed < cap_n {
                    // The group does not fit whole. Historically it was skipped
                    // entirely — and since nothing about it changes, it was then
                    // skipped again every night, forever. Measured on the local
                    // substrate 2026-08-22: 217 absorbable, 9 absorbed, because one
                    // ~208-member group could never fit under the 20% cap.
                    //
                    // Instead admit a PREFIX of it: the representative plus the
                    // `room` members most cohesive with the representative. The cap
                    // is still honoured exactly (we take at most `room`); the group
                    // simply drains across successive nights rather than never.
                    let room = cap_n - absorbed;
                    // Representative = max energy, matching apply_consolidation's
                    // carrier choice and the cohesion scoring above.
                    let rep = *m
                        .iter()
                        .max_by(|&&a, &&b| {
                            energies[a]
                                .partial_cmp(&energies[b])
                                .unwrap_or(std::cmp::Ordering::Equal)
                        })
                        .unwrap();
                    let mut others: Vec<usize> = m.iter().copied().filter(|&i| i != rep).collect();
                    // Most cohesive with rep first; index tiebreak keeps it deterministic.
                    others.sort_by(|&a, &b| {
                        let ca = if norms[rep] < eps || norms[a] < eps {
                            0.0
                        } else {
                            gram[[rep, a]] / (norms[rep] * norms[a])
                        };
                        let cb = if norms[rep] < eps || norms[b] < eps {
                            0.0
                        } else {
                            gram[[rep, b]] / (norms[rep] * norms[b])
                        };
                        cb.partial_cmp(&ca).unwrap_or(std::cmp::Ordering::Equal).then(a.cmp(&b))
                    });
                    others.truncate(room);
                    if !others.is_empty() {
                        let mut part = Vec::with_capacity(others.len() + 1);
                        part.push(rep);
                        part.extend(others);
                        absorbed += part.len() - 1;
                        grouping.admitted.push(part);
                    }
                }
                // else: cap exhausted, or partial admission disabled — leave intact.
            }
            debug_assert!(absorbed <= cap_n, "partial admission must never exceed the cap");
        }
    }

    grouping
}

/// ADR-0036 **M3 salience decay** — pick the ShortTerm wavefronts that should fade.
///
/// Shared by the plan and apply paths so a dry-run is a faithful preview (the same
/// structural-parity discipline `compute_merge_grouping` already enforces for merges).
///
/// Borrowed from `kannaka-crystal`'s dream engine: it thresholds at a **percentile of
/// the observed distribution**, never at an absolute constant. That matters because
/// the absolute evict floor here (0.15) was measured on 2026-08-22 to sit below the
/// entire live distribution on every node — weakest active wavefront 0.60 on
/// kannaka-prime, 1.73 mean locally — so nothing ever became evictable and every
/// substrate grew without bound. A rank-based percentile has no scale to get wrong.
///
/// Selection is by **rank**, not by value, so a corpus where many amplitudes are
/// exactly equal (the common case — audio telemetry lands on identical amplitudes)
/// still fades the intended fraction rather than all-or-nothing.
///
/// Only unretrieved ShortTerm traces that are not merge participants are eligible:
/// Pinned/LongTerm are structurally excluded, anything ever recalled is spared, and
/// the result is a soft multiply — **never a removal**.
pub fn compute_decay_set(
    amplitudes: &[f32],
    tiers: &[crate::medium::types::Tier],
    retrieval_counts: &[u32],
    merge_participants: &std::collections::HashSet<usize>,
    opts: &crate::medium::types::ConsolidateOpts,
) -> Vec<usize> {
    use crate::medium::types::Tier;
    if opts.shortterm_decay_pctl <= 0.0 || opts.shortterm_decay_factor >= 1.0 {
        return Vec::new();
    }
    let mut candidates: Vec<usize> = (0..amplitudes.len())
        .filter(|&i| {
            tiers.get(i).copied() == Some(Tier::ShortTerm)
                && retrieval_counts.get(i).copied().unwrap_or(0) == 0
                && !merge_participants.contains(&i)
        })
        .collect();
    if candidates.is_empty() {
        return Vec::new();
    }
    // Weakest first; index tiebreak keeps the choice deterministic across runs
    // (dream determinism, #521).
    candidates.sort_by(|&a, &b| {
        amplitudes[a]
            .partial_cmp(&amplitudes[b])
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.cmp(&b))
    });
    let take = ((candidates.len() as f32) * opts.shortterm_decay_pctl).floor() as usize;
    candidates.truncate(take.min(candidates.len()));
    candidates
}

/// T1.4 (#474) gated dream-entropy core — **deterministic given `bytes`**: seed a
/// PRNG from the drawn entropy, apply a small entropy-seeded phase jitter to the
/// dream's hallucinated products, and stamp the TRUE `prov` on them. Returns how
/// many were perturbed. Split out from the live draw so the entropy→dynamics
/// mapping is unit-tested with fixed bytes (reproducibility) without a source.
fn apply_entropy_perturbation(
    metadata: &mut [crate::medium::types::WavefrontMeta],
    phases: &mut [f32],
    bytes: &[u8],
    prov: &crate::entropy::Provenance,
) -> usize {
    use rand::{Rng, SeedableRng};
    let mut rng = rand_chacha::ChaCha8Rng::seed_from_u64(crate::entropy::seed_from_bytes(bytes));
    let mut count = 0;
    for (i, m) in metadata.iter_mut().enumerate() {
        // Only perturb+stamp hallucinations NOT already stamped. Without the
        // provenance.is_none() gate this re-jitters every persisted hallucination
        // on EVERY dream (an unbounded ±0.05 rad phase random-walk that drifts
        // settled wavefronts' recall) and re-draws paid entropy each steady-state
        // dream — the phantom-spend #521 exists to prevent. One draw, one stamp.
        if m.hallucinated && m.provenance.is_none() {
            let jitter = (rng.gen::<f32>() - 0.5) * 0.1; // ±0.05 rad, entropy-seeded
            if let Some(p) = phases.get_mut(i) {
                *p += jitter;
            }
            m.provenance = Some(prov.clone());
            count += 1;
        }
    }
    count
}

/// HRM-backed memory store that implements the MediumBackend trait.
///
/// This wraps a Medium and provides the familiar MediumBackend interface,
/// allowing HRM to be the sole storage backend.
/// Result of a whole-store facet backfill sweep (#836 / ADR-0049).
#[derive(Debug, Default, Clone)]
pub struct FacetBackfillStats {
    /// Right-hemisphere rows examined (the canonical population).
    pub scanned: usize,
    /// Rows that are themselves facets (never decomposed).
    pub facet_rows: usize,
    /// Parents already carrying the `decomposed` watermark.
    pub already_decomposed: usize,
    /// Rows whose content yields fewer than two facets (nothing to mint).
    pub atomic: usize,
    /// Parents decomposed this run (dry run: that WOULD be decomposed).
    pub parents_decomposed: usize,
    /// Facet rows minted this run (dry run: that WOULD be minted).
    pub facets_minted: usize,
    /// Per-row errors — logged and skipped, the sweep continues.
    pub errors: usize,
}

pub struct HrmStore {
    /// The underlying holographic resonance medium
    medium: Medium,
    /// Chiral medium (ADR-0021) — when present, this is the authoritative backend
    chiral: Option<ChiralMedium>,
    /// Encoding pipeline for text → hypervector conversion
    pipeline: EncodingPipeline,
    /// Path to the .hrm file for persistence
    hrm_path: PathBuf,
    /// In-memory cache mapping UUID → HyperMemory for compatibility
    memory_cache: HashMap<Uuid, HyperMemory>,
    /// Dirty flag to track when the medium needs saving
    dirty: bool,
    /// When true, `save_medium` is a no-op — the process loads and mutates
    /// in RAM but never persists. Set via `KANNAKA_READONLY` so the long-
    /// running reader services (swarm serve / inbox serve / attention serve)
    /// can share one HRM with the sole writer (swarm join) without the
    /// last-writer-wins clobbering that silently drops absorbed memories.
    readonly: bool,
}

impl HrmStore {
    /// Whether `KANNAKA_READONLY` requests read-only (no-persist) mode.
    /// Any non-empty value other than "0"/"false" enables it.
    fn env_readonly() -> bool {
        match std::env::var("KANNAKA_READONLY") {
            Ok(v) => !v.is_empty() && v != "0" && !v.eq_ignore_ascii_case("false"),
            Err(_) => false,
        }
    }

    /// Force read-only mode on this store regardless of env. Used by the
    /// OpenClaw fallback so a failed load can never overwrite (and thus
    /// destroy) a corrupt-but-recoverable .hrm file.
    pub fn set_readonly(&mut self, ro: bool) {
        self.readonly = ro;
    }

    /// Gravity-well lookup: memory ids whose content folds onto Fano `line`
    /// (0..6), capped at `limit`. Used by `attention serve` to pull same-line
    /// memories into the beam when kannaka-eye emits a glyph on that line.
    #[cfg(feature = "glyph")]
    pub fn ids_by_fano_line(&self, line: u8, limit: usize) -> Vec<uuid::Uuid> {
        self.medium.ids_by_fano_line(line, limit)
    }

    /// Create a new HRM store with the given encoding pipeline and file path.
    pub fn new(pipeline: EncodingPipeline, hrm_path: PathBuf) -> Self {
        Self {
            medium: Medium::new(),
            chiral: None,
            pipeline,
            hrm_path,
            memory_cache: HashMap::new(),
            dirty: false,
            readonly: Self::env_readonly(),
        }
    }

    /// Load an existing HRM store from a .hrm file.
    /// Auto-detects v1 vs v2 format. v2 loads as ChiralMedium.
    pub fn load(pipeline: EncodingPipeline, hrm_path: PathBuf) -> Result<Self, StoreError> {
        // Detect format version from magic bytes before loading
        let is_v2 = if let Ok(mut f) = std::fs::File::open(&hrm_path) {
            let mut magic = [0u8; 4];
            use std::io::Read;
            f.read_exact(&mut magic).ok();
            magic == crate::medium::HRM_MAGIC_V2
        } else {
            false
        };

        // Try loading as ChiralMedium first (handles both v1 and v2)
        match ChiralMedium::load(&hrm_path) {
            Ok(chiral) => {
                // For v2 files, start with empty Medium (will be synced from chiral)
                // For v1 files auto-converted, also load the raw Medium
                let medium = if is_v2 {
                    Medium::new()
                } else {
                    Medium::load(&hrm_path).unwrap_or_else(|_| Medium::new())
                };

                let mut store = Self {
                    medium,
                    chiral: Some(chiral),
                    pipeline,
                    hrm_path,
                    memory_cache: HashMap::new(),
                    dirty: false,
                    readonly: Self::env_readonly(),
                };
                // Populate flat medium view for backward compat (observe, coherence matrix, etc.)
                store.sync_medium_from_chiral();
                store.rebuild_cache()?;
                store.load_link_graph();
                store.load_reactivation();
                Ok(store)
            }
            Err(chiral_err) => {
                eprintln!("[hrm] ChiralMedium::load failed: {chiral_err}");
                if is_v2 {
                    // For v2 files the v1 fallback would always fail with
                    // InvalidMagic — report the real error instead of layering
                    // a misleading magic-bytes message on top.
                    return Err(StoreError::Other(format!(
                        "Failed to load HRM file: {chiral_err}"
                    )));
                }
                // Legacy v1 path: re-try as plain Medium
                let medium = Medium::load(&hrm_path)
                    .map_err(|e| StoreError::Other(format!("Failed to load HRM file: {e}")))?;
                let mut store = Self {
                    medium,
                    chiral: None,
                    pipeline,
                    hrm_path,
                    memory_cache: HashMap::new(),
                    dirty: false,
                    readonly: Self::env_readonly(),
                };
                store.rebuild_cache()?;
                store.load_link_graph();
                store.load_reactivation();
                Ok(store)
            }
        }
    }

    /// Rebuild the memory cache from the medium data.
    /// Preserves existing connections (skip links) from the previous cache state.
    fn rebuild_cache(&mut self) -> Result<(), StoreError> {
        // Snapshot existing connections before clearing
        let saved_connections: std::collections::HashMap<uuid::Uuid, Vec<crate::memory::LegacyLink>> =
            self.memory_cache.iter()
                .filter(|(_, m)| !m.connections.is_empty())
                .map(|(id, m)| (*id, m.connections.clone()))
                .collect();

        // ADR-0036 Phase 1: reactivation (recall access) is the replay signal for
        // tier promotion. rebuild_cache runs after every dream/absorb, so without
        // this snapshot the count would reset to 0 on each rebuild. Preserve it
        // across the clear, exactly like connections above.
        //
        // #497: ALSO preserve the updated_at of a GHOST — a memory with
        // retrieval_count 0 but a real ADR-0037 ghosting stamp (updated_at !=
        // created_at). rebuild reconstructs from the medium's WavefrontMeta (which
        // has no updated_at) and defaults it to created_at; if we don't restore the
        // stamp, the next deep dream's stage_compact_ghosts sees the ghost past the
        // 7-day horizon and hard-deletes it with zero recovery window — re-opening
        // the 295→88 over-prune loss the stamp exists to prevent.
        let saved_reactivation: std::collections::HashMap<uuid::Uuid, (u32, Option<DateTime<Utc>>)> =
            self.memory_cache.iter()
                .filter(|(_, m)| {
                    m.retrieval_count > 0
                        || (m.updated_at.is_some() && m.updated_at != Some(m.created_at))
                })
                .map(|(id, m)| (*id, (m.retrieval_count, m.updated_at)))
                .collect();

        // #497 (remaining half): layer_depth and last_consolidated_at also
        // live only in the cache. Rebuilding from WavefrontMeta zeroed both —
        // discarding stage_transfer's layer promotions (DreamState's
        // layer-banded cycles restarted from scratch) and defeating
        // consolidate_incremental's changed-since check. Snapshot and
        // restore them exactly like reactivation above.
        let saved_consolidation: std::collections::HashMap<uuid::Uuid, (u8, Option<DateTime<Utc>>)> =
            self.memory_cache.iter()
                .filter(|(_, m)| m.layer_depth != 0 || m.last_consolidated_at.is_some())
                .map(|(id, m)| (*id, (m.layer_depth, m.last_consolidated_at)))
                .collect();

        self.memory_cache.clear();

        if let Some(ref chiral) = self.chiral {
            // Build cache from right hemisphere (authoritative memory store)
            for (i, meta) in chiral.right.metadata.iter().enumerate() {
                let vector = chiral.right.wavefronts.row(i).to_vec();
                let memory = HyperMemory {
                    id: meta.id,
                    vector,
                    amplitude: chiral.right.energy[i],
                    frequency: chiral.right.frequency[i],
                    phase: chiral.right.phase[i],
                    decay_rate: 0.001,
                    created_at: meta.created_at,
                    layer_depth: 0,
                    connections: Vec::new(),
                    content: meta.content.clone(),
                    hallucinated: meta.hallucinated,
                    parents: Vec::new(),
                    geometry: None,
                    xi_signature: Vec::new(),
                    origin_agent: "local".to_string(),
                    sync_version: 0,
                    merge_history: Vec::new(),
                    last_consolidated_at: None,
                    disputed: false,
                    updated_at: Some(meta.created_at),
                    retrieval_count: 0,
                    modality: meta.modality,
                    tier: meta.tier,
                    effective_at: meta.effective_at,
                    observed_at: meta.observed_at,
                    expires_at: meta.expires_at,
                };
                self.memory_cache.insert(meta.id, memory);
            }
        } else {
            // Legacy: build from flat medium
            for (i, meta) in self.medium.store.metadata.iter().enumerate() {
                let vector = self.medium.store.wavefronts.row(i).to_vec();
                let memory = HyperMemory {
                    id: meta.id,
                    vector,
                    amplitude: self.medium.store.energy[i],
                    frequency: self.medium.store.frequency[i],
                    phase: self.medium.store.phase[i],
                    decay_rate: 0.001,
                    created_at: meta.created_at,
                    layer_depth: 0,
                    connections: Vec::new(),
                    content: meta.content.clone(),
                    hallucinated: meta.hallucinated,
                    parents: Vec::new(),
                    geometry: None,
                    xi_signature: Vec::new(),
                    origin_agent: "local".to_string(),
                    sync_version: 0,
                    merge_history: Vec::new(),
                    last_consolidated_at: None,
                    disputed: false,
                    updated_at: Some(meta.created_at),
                    retrieval_count: 0,
                    modality: meta.modality,
                    tier: meta.tier,
                    effective_at: meta.effective_at,
                    observed_at: meta.observed_at,
                    expires_at: meta.expires_at,
                };
                self.memory_cache.insert(meta.id, memory);
            }
        }

        // Restore saved connections from previous cache state, dropping any
        // link whose target was removed since the snapshot. rebuild_cache runs
        // after every removal path (resonance-merge absorb + ShortTerm evict in
        // apply_consolidation, ghost compaction, delete), and none of those
        // paths strip inbound links from survivors — so without this filter a
        // LegacyLink{ target_id } pointing at a merged/evicted/compacted memory
        // would survive here, re-serialize to the *.links.json sidecar, and
        // accumulate every dream. Dangling targets corrupt bridge-node
        // detection and cross-cluster link counts (id_to_cluster.get on a dead
        // id). The authoritative store is fully reflected in memory_cache at
        // this point, so any target_id not present is genuinely gone.
        let live: std::collections::HashSet<uuid::Uuid> =
            self.memory_cache.keys().copied().collect();
        for (id, mut conns) in saved_connections {
            if let Some(mem) = self.memory_cache.get_mut(&id) {
                conns.retain(|l| live.contains(&l.target_id));
                mem.connections = conns;
            }
        }

        // Restore reactivation counts (ADR-0036 Phase 1).
        for (id, (count, last)) in saved_reactivation {
            if let Some(mem) = self.memory_cache.get_mut(&id) {
                mem.retrieval_count = count;
                if last.is_some() {
                    mem.updated_at = last;
                }
            }
        }

        // Restore layer promotions + incremental-consolidation stamps (#497).
        for (id, (layer, consolidated)) in saved_consolidation {
            if let Some(mem) = self.memory_cache.get_mut(&id) {
                mem.layer_depth = layer;
                mem.last_consolidated_at = consolidated;
            }
        }

        Ok(())
    }

    /// Sync any mutations made via `get_mut()` back to the medium tensor.
    ///
    /// The `MediumBackend` trait returns `&mut HyperMemory`, so callers can mutate
    /// wave parameters (amplitude, phase, frequency) on the cached copy. This
    /// method writes those changes back to the authoritative tensor storage.
    fn sync_cache_to_medium(&mut self) {
        // #496: in chiral mode the RIGHT hemisphere is authoritative — it is what
        // `save_medium` persists and what `rebuild_cache` reloads. The particle
        // dream strengthens ENERGY on the cached copy via get_mut; pre-fix that
        // was written to the flat `self.medium` (never saved in chiral mode) and
        // reverted on every reload, while the dream's prunes (applied to
        // chiral.right) stuck — the asymmetry #496 describes.
        //
        // ONLY energy is synced here (not frequency/phase). frequency and phase
        // are set once by Hemisphere::add_wavefront (freq=1.0, a born phase) and
        // are only ever mutated on chiral.right DIRECTLY (kuramoto/callosal/dream,
        // each of which rebuilds the cache) — the cache is a reflection, never an
        // authoritative source, for those two. Writing them back from the cache
        // would clobber the authoritative values with a stale default whenever a
        // wavefront was added via insert_raw_wavefront (whose cache record keeps
        // WaveParams::default freq=0.1/phase=0.0) — corrupting the 96 substrate
        // anchors on their first flush. Pre-PR the flat-medium sync never reached
        // disk in chiral mode, so energy-only here is the exact scope #496 needs
        // and matches pre-PR behavior for freq/phase.
        if let Some(ref mut chiral) = self.chiral {
            for (id, mem) in &self.memory_cache {
                if let Some(&index) = chiral.right.id_to_index.get(id) {
                    chiral.right.energy[index] = mem.amplitude;
                }
            }
        } else {
            for (id, mem) in &self.memory_cache {
                if let Some(index) = self.medium.get_wavefront_index(id) {
                    self.medium.store.energy[index] = mem.amplitude;
                    self.medium.store.frequency[index] = mem.frequency;
                    self.medium.store.phase[index] = mem.phase;
                }
            }
        }
    }

    /// Save the medium to the .hrm file.
    fn save_medium(&mut self) -> Result<(), StoreError> {
        // Read-only processes mutate in RAM but never persist. This is how
        // single-writer is enforced: only the sole writer (swarm join /
        // dream / ad-hoc remember) flushes to disk; the long-running reader
        // services hold KANNAKA_READONLY and drop their dirty flag silently.
        if self.readonly {
            self.dirty = false;
            return Ok(());
        }

        if !self.dirty {
            return Ok(());
        }

        // Sync any cache mutations back to the medium before saving
        self.sync_cache_to_medium();

        if let Some(ref chiral) = self.chiral {
            chiral.save(&self.hrm_path)
                .map_err(|e| StoreError::Other(format!("Failed to save chiral HRM file: {e}")))?;
        } else {
            self.medium.save(&self.hrm_path)
                .map_err(|e| StoreError::Other(format!("Failed to save HRM file: {e}")))?;
        }

        // Save link graph sidecar (connections not stored in HRM binary)
        self.save_link_graph();
        // Save reactivation sidecar (ADR-0036 Phase 1; not in the HRM binary).
        // The single writer holds the full cache, so it may prune stale ids.
        self.save_reactivation_merge(true);

        self.dirty = false;
        Ok(())
    }

    /// Atomically write `bytes` to `path`: write a unique per-process temp file
    /// then rename it over the target. A plain `fs::write` is not atomic — a
    /// crash or a concurrent writer can leave a truncated file, and the sidecar
    /// loaders (`load_link_graph`, `load_reactivation`) silently discard ALL
    /// state when the JSON fails to parse. `rename` within the same directory
    /// is atomic, so a reader always sees either the old or the new file whole.
    fn atomic_write(path: &std::path::Path, bytes: &[u8]) {
        // #777: delegates to crate::fs_util. This copy was the divergent one,
        // twice over: no sync_all (a crash after write-complete but before
        // flush could lose the sidecar even though the rename "succeeded"),
        // and a `with_extension("tmp.<pid>")` temp name that REPLACES a
        // dotless basename instead of creating a sibling. Sidecar saves stay
        // deliberately best-effort, so the Result is dropped here — but the
        // write itself is now durable and the temp name always safe.
        let _ = crate::fs_util::atomic_write_bytes(path, bytes);
    }

    /// Save the link graph as a sidecar JSON file alongside the HRM file.
    fn save_link_graph(&self) {
        let links_path = self.hrm_path.with_extension("links.json");
        let graph: std::collections::HashMap<String, Vec<&crate::memory::LegacyLink>> = self.memory_cache.iter()
            .filter(|(_, m)| !m.connections.is_empty())
            .map(|(id, m)| (id.to_string(), m.connections.iter().collect()))
            .collect();
        if graph.is_empty() { return; }
        if let Ok(json) = serde_json::to_vec(&graph) {
            Self::atomic_write(&links_path, &json);
        }
    }

    /// Load link graph from sidecar file and merge into memory cache.
    fn load_link_graph(&mut self) {
        let links_path = self.hrm_path.with_extension("links.json");
        if !links_path.exists() { return; }
        let data = match std::fs::read(&links_path) {
            Ok(d) => d,
            Err(_) => return,
        };
        let graph: std::collections::HashMap<String, Vec<crate::memory::LegacyLink>> = match serde_json::from_slice(&data) {
            Ok(g) => g,
            Err(_) => return,
        };
        for (id_str, links) in graph {
            if let Ok(id) = id_str.parse::<uuid::Uuid>() {
                if let Some(mem) = self.memory_cache.get_mut(&id) {
                    // Merge: add sidecar links that don't already exist
                    for link in links {
                        if !mem.connections.iter().any(|l| l.target_id == link.target_id) {
                            mem.connections.push(link);
                        }
                    }
                }
            }
        }
    }

    /// Save per-memory reactivation (recall access) as a sidecar JSON, mirroring
    /// the link-graph sidecar. Reactivation is deliberately NOT in the `.hrm`
    /// binary — appending fields to the bincode `WavefrontMeta` layout is risky
    /// (positional format + a fallback-struct chain), and a sidecar matches the
    /// existing pattern (`.links.json`, `.clusters.json`) with zero format risk.
    /// ADR-0036 Phase 1. (Read-only processes never reach here, so reactivation
    /// recorded in read replicas is not persisted — a known limitation tracked
    /// for a later swarm-aggregation phase.)
    /// Merge this process's reactivation counts into the sidecar (read existing
    /// → take the MAX per id → write back). Merge-on-write is required because
    /// several processes touch the sidecar — the single writer on dream-save,
    /// the readonly serve daemon, and short-lived CLI `recall` — and a plain
    /// overwrite would let them clobber each other's counts. Counts are
    /// monotonic, so MAX is the correct reconciliation for a heuristic.
    ///
    /// `prune_stale` drops entries whose id is absent from this cache; only the
    /// single writer (which holds the full cache) may prune — partial-cache
    /// callers (daemon/CLI) must pass `false` or they would delete other
    /// memories' counts.
    fn save_reactivation_merge(&self, prune_stale: bool) {
        let path = self.hrm_path.with_extension("reactivation.json");
        let mut merged: std::collections::HashMap<String, (u32, Option<DateTime<Utc>>)> =
            std::fs::read(&path)
                .ok()
                .and_then(|d| serde_json::from_slice(&d).ok())
                .unwrap_or_default();

        for (id, m) in &self.memory_cache {
            // #497: persist a GHOST's updated_at too — a memory with retrieval_count
            // 0 but a real ADR-0037 ghosting stamp (updated_at != created_at) — so
            // the stamp survives a restart and stage_compact_ghosts doesn't
            // hard-delete the ghost past the horizon. Skip only truly-untouched
            // memories (no recalls AND no real update).
            let real_update = m.updated_at.is_some() && m.updated_at != Some(m.created_at);
            if m.retrieval_count == 0 && !real_update {
                continue;
            }
            let entry = merged.entry(id.to_string()).or_insert((0, None));
            if m.retrieval_count > entry.0 {
                entry.0 = m.retrieval_count;
            }
            if m.updated_at.is_some() && (entry.1.is_none() || m.updated_at > entry.1) {
                entry.1 = m.updated_at;
            }
        }

        if prune_stale {
            merged.retain(|k, _| {
                k.parse::<uuid::Uuid>()
                    .map(|id| self.memory_cache.contains_key(&id))
                    .unwrap_or(false)
            });
        }

        if merged.is_empty() {
            return;
        }
        if let Ok(json) = serde_json::to_vec(&merged) {
            Self::atomic_write(&path, &json);
        }
    }

    /// Flush reactivation to the sidecar from a possibly-readonly, partial-cache
    /// process (the serve daemon or a CLI `recall`). ADR-0036 Phase 1. This is
    /// intentionally NOT gated by `readonly`: it writes only the heuristic
    /// sidecar, never the `.hrm`, so it does not violate single-writer.
    pub fn flush_reactivation(&self) {
        self.save_reactivation_merge(false);
    }

    /// Load reactivation counts from the sidecar into the cache. ADR-0036 Phase 1.
    fn load_reactivation(&mut self) {
        let path = self.hrm_path.with_extension("reactivation.json");
        if !path.exists() {
            return;
        }
        let data = match std::fs::read(&path) {
            Ok(d) => d,
            Err(_) => return,
        };
        let map: std::collections::HashMap<String, (u32, Option<DateTime<Utc>>)> =
            match serde_json::from_slice(&data) {
                Ok(m) => m,
                Err(_) => return,
            };
        for (id_str, (count, last)) in map {
            if let Ok(id) = id_str.parse::<uuid::Uuid>() {
                if let Some(mem) = self.memory_cache.get_mut(&id) {
                    mem.retrieval_count = count;
                    if last.is_some() {
                        mem.updated_at = last;
                    }
                }
            }
        }
    }

    /// Mark the medium as dirty (needing save).
    fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Get the underlying medium (read-only).
    /// In chiral mode, returns the flat medium (may be empty if loaded from v2).
    /// Use chiral_medium() for the authoritative chiral state.
    pub fn medium(&self) -> &Medium {
        &self.medium
    }

    /// ADR-0027 Phase 1.c — direct wavefront insertion with a pre-built
    /// hypervector, bypassing the text-encoding pipeline. The substrate
    /// uses this to seed 96 class anchors at maximally distinct points
    /// in vector space so Kuramoto's eigenvalue clustering (which
    /// thresholds on coherence > 0.5 to join a cluster) can actually
    /// find multiple clusters. Text-encoded markers always produce
    /// highly correlated vectors and collapse to a single cluster.
    ///
    /// `vector` must be exactly WAVEFRONT_DIM long. `content` is
    /// human-readable text for debugging; `importance` becomes the
    /// initial wavefront energy.
    pub fn insert_raw_wavefront(
        &mut self,
        vector: Vec<f32>,
        content: String,
        importance: f32,
    ) -> Result<Uuid, StoreError> {
        // If chiral mode is active, route through the chiral medium so the
        // wavefront lands in the right hemisphere (which is what
        // chiral.save() serializes to disk). Direct medium.add_wavefront
        // would only update the flat medium, and chiral save would ignore
        // it — losing the wavefront on next process restart and showing
        // stale counts to the observatory's status shell-out.
        let id = if let Some(ref mut chiral) = self.chiral {
            chiral.store_vector(&vector, content.clone(), importance)
                .map_err(|e| StoreError::Other(format!("chiral.store_vector failed: {e}")))?
        } else {
            self.medium.add_wavefront(&vector, content.clone(), importance)
                .map_err(|e| StoreError::Other(format!("add_wavefront failed: {e}")))?
        };
        // The medium has the wavefront for tensor/vector math, but the
        // higher-level `all_memories()` / `assess()` paths read from
        // `memory_cache`. Insert a matching HyperMemory record so
        // total_memories, cluster counts, and on-disk persistence
        // reflect the new wavefront. Use the importance as initial
        // amplitude so it shows up in observation tables.
        let mut memory = crate::memory::HyperMemory::new(vector, content);
        memory.id = id;
        memory.amplitude = importance;
        self.memory_cache.insert(id, memory);
        self.mark_dirty();
        Ok(id)
    }

    /// Path to the on-disk HRM file. Used by the cluster sidecar cache to
    /// place `.clusters.json` next to it and check mtime for invalidation.
    pub fn hrm_path(&self) -> &std::path::Path {
        &self.hrm_path
    }

    /// Ensure the flat medium is populated from chiral state (for backward compat).
    /// Call this before operations that need the flat medium view.
    pub fn sync_medium_from_chiral(&mut self) {
        if let Some(ref chiral) = self.chiral {
            if self.medium.wavefront_count() == 0 && chiral.right.count() > 0 {
                // Build flat medium from right hemisphere
                for i in 0..chiral.right.count() {
                    let vector = chiral.right.wavefronts.row(i).to_vec();
                    let meta = &chiral.right.metadata[i];
                    let _ = self.medium.add_wavefront(
                        &vector,
                        meta.content.clone(),
                        chiral.right.energy[i],
                    );
                    // Fix the ID to match
                    let new_id = self.medium.store.metadata.last().unwrap().id;
                    let _ = self.medium.update_wavefront_id(&new_id, meta.id);
                    // Copy wave params
                    let idx = self.medium.wavefront_count() - 1;
                    self.medium.store.frequency[idx] = chiral.right.frequency[i];
                    self.medium.store.phase[idx] = chiral.right.phase[i];
                }
            }
        }
    }

    /// Resonance recall with observation — the canonical read path.
    ///
    /// Reading IS observation. Attention reshapes the field:
    /// - Higher-ranked results receive stronger observation intensity
    /// - Observation boosts wavefront energy (recalled memories persist)
    /// - The medium is permanently changed by the act of recall
    ///
    /// There is no "read without observation" path. If you query the medium,
    /// you change it. This is the holographic equivalent of quantum measurement.
    pub fn recall_resonance(&mut self, query: &str, top_k: usize) -> Result<Vec<Resonance>, StoreError> {
        let results = self.medium.recall(query, top_k, &self.pipeline)
            .map_err(|e| StoreError::Other(format!("Resonance recall failed: {e}")))?;

        // Observation: attention shapes the field
        self.apply_observation(&results);

        Ok(results)
    }

    /// Read-only resonance recall — same scoring as [`recall_resonance`] but
    /// WITHOUT the observation write-back, so it never mutates energies or marks
    /// the store dirty. Used by the recall-scenario benchmark export (#476):
    /// dumping a corpus must not perturb the live substrate, and back-to-back
    /// export queries must not amplify each other's amplitudes. Runs against the
    /// flat medium (which mirrors the right hemisphere in chiral mode),
    /// consistent with [`consciousness_metrics`](Self::consciousness_metrics).
    pub fn recall_resonance_readonly(&self, query: &str, top_k: usize) -> Result<Vec<Resonance>, StoreError> {
        self.medium.recall(query, top_k, &self.pipeline)
            .map_err(|e| StoreError::Other(format!("readonly resonance recall failed: {e}")))
    }

    /// Beam-aware recall — score only the memories in the attention beam.
    ///
    /// This is the system-level entry to `Medium::recall_against_ids`. The
    /// chiral hemisphere logic is bypassed for the sparse path (chiral
    /// attention is a future fold-in); the recall runs against the right
    /// hemisphere / flat medium directly, then applies observation.
    ///
    /// Empty beam in -> empty results out (intentional — the sparsity is
    /// meaningless if we silently fall back to full recall).
    pub fn recall_resonance_with_beam(
        &mut self,
        beam: &[uuid::Uuid],
        query: &str,
        top_k: usize,
    ) -> Result<Vec<Resonance>, StoreError> {
        let results = self.medium.recall_against_ids(beam, query, top_k, &self.pipeline)
            .map_err(|e| StoreError::Other(format!("beam-aware recall failed: {e}")))?;
        // Observation still shapes the field — sparse recall doesn't excuse
        // a passive read. Same intensity formula as the dense path.
        self.apply_observation(&results);
        Ok(results)
    }

    /// Apply observation effects to recall results.
    /// Ranked results get proportionally stronger observation.
    /// Batched: one field-settle pass per recall, not per result.
    fn apply_observation(&mut self, results: &[Resonance]) {
        if results.is_empty() { return; }
        let observations: Vec<(usize, f32)> = results.iter().enumerate()
            .filter_map(|(i, resonance)| {
                self.medium.get_wavefront_index(&resonance.id).map(|index| {
                    let ranking_factor = 1.0 - (i as f32 / results.len() as f32);
                    let intensity = resonance.resonance_strength.abs().min(1.0).max(0.1) * ranking_factor;
                    (index, intensity)
                })
            })
            .collect();
        self.medium.observe_wavefronts(&observations);
        self.mark_dirty();
    }
    
    /// Get consciousness metrics from the holographic medium.
    /// In chiral mode, computes from the synced flat medium (which mirrors the right hemisphere).
    /// This uses the full eigendecomposition-based Phi, spectral Xi, and Kuramoto order.
    pub fn consciousness_metrics(&self) -> crate::medium::ConsciousnessMetrics {
        // Always compute from the flat medium — it's synced from right hemisphere on load
        self.medium.consciousness_metrics()
    }

    /// ADR-0037 Phase 3: aggregate π/φ bridge-operator signature (residue +
    /// spectral xi) for the substrate beacon. Computed from the flat medium.
    pub fn xi_bridge_summary(&self) -> serde_json::Value {
        self.medium.xi_bridge_summary()
    }

    /// Cheap, stale-tolerant lookup. See Medium::try_cached_consciousness_metrics.
    /// Used by the agent's system_prompt path so a `kannaka ask` doesn't
    /// pay an O(n³) eigendecomp on every call.
    pub fn try_cached_consciousness_metrics(&self) -> Option<crate::medium::ConsciousnessMetrics> {
        self.medium.try_cached_consciousness_metrics()
    }
    
    /// Find memories associated with a given memory through emergent coherence.
    pub fn find_associated(&self, id: Uuid, top_k: usize) -> Vec<(Uuid, f32)> {
        self.medium.find_associated(id, top_k)
    }
    
    /// Apply dynamics to the medium (wave evolution).
    pub fn apply_dynamics(&mut self, dt: f32) {
        self.medium.apply_dynamics(dt);
        self.mark_dirty();
    }
    
    /// ADR-0037 (L6 loop): per-consolidation spiral telemetry. The cortical
    /// spiral wave (Ye et al.) spans BOTH hemispheres, so the headline metric
    /// is the cross-hemisphere ring winding/order; the right ring (which the
    /// coupling directly rotates) and the 2-D PCA cloud cores are reported
    /// beside it. Gated on the same flag as the coupling — emitted by every
    /// chiral deep dream (incl. the production `dream_native` path) when
    /// spiral dreams are enabled (the v0.7.0 default). stderr only.
    fn log_spiral_telemetry(&self) {
        if !crate::medium::chiral::spiral_dream_enabled() {
            return;
        }
        if let Some(ref chiral) = self.chiral {
            let x = chiral.bilateral_ring_report();
            if x.n > 0 {
                let right = chiral.holistic_ring_report();
                let c = chiral.holistic_cloud_report();
                eprintln!(
                    "[spiral] deep dream: cross-hemi winding={:.3} order={:.3} n={} (right winding={:.3}, 2D cores={} net={})",
                    x.winding, x.order, x.n, right.winding, c.singularities.len(), c.net_charge
                );
            }
        }
    }

    /// ADR-0037: cheap (O(n), no eigendecomp / no PCA) cross-hemisphere ring
    /// report for `kannaka belief status` — the same metric the dream telemetry
    /// headlines (`bilateral_ring_report`: order + winding over the active
    /// left⊕right phase ring). `None` when not in chiral mode. The 2-D PCA cores
    /// are the heavier `belief_cloud_report` (opt-in via `--full`).
    pub fn belief_ring_report(&self) -> Option<crate::spiral::RingReport> {
        self.chiral.as_ref().map(|c| c.bilateral_ring_report())
    }

    /// ADR-0037: 2-D spiral cores over the holistic field (PCA — O(n²), heavier).
    /// Backs `kannaka belief status --full`. `None` when not in chiral mode.
    pub fn belief_cloud_report(&self) -> Option<crate::spiral::SpiralReport> {
        self.chiral.as_ref().map(|c| c.holistic_cloud_report())
    }

    /// ADR-0037 L6 instrument: per-dream belief-core snapshot (cores + frame-
    /// invariant content fingerprints) for cross-dream tracking. Empty when not
    /// in chiral mode.
    pub fn belief_core_snapshot(&self) -> Vec<crate::l6::CoreObs> {
        self.chiral.as_ref().map(|c| c.belief_core_snapshot()).unwrap_or_default()
    }

    /// Perform a dream cycle (simulated annealing).
    /// In chiral mode, deep dreams only affect the right hemisphere.
    pub fn dream(&mut self, cycles: usize, initial_temperature: Option<f32>) -> crate::medium::DreamReport {
        if self.chiral.is_some() {
            // Chiral dream: right hemisphere only (deep dream)
            let report = self.chiral.as_mut().unwrap().dream(true, cycles);
            self.apply_dream_entropy();
            self.rebuild_cache().ok();
            self.mark_dirty();
            self.log_spiral_telemetry();
            report
        } else {
            let report = self.medium.dream(cycles, initial_temperature);
            self.apply_dream_entropy();
            self.mark_dirty();
            report
        }
    }

    /// T1.4 (#474): GATED dream-entropy participation. When
    /// `KANNAKA_DREAM_ENTROPY` is enabled, draw from the configured
    /// [`EntropySource`](crate::entropy::EntropySource), let this dream's products
    /// (hallucinations) genuinely depend on the drawn entropy (a small
    /// entropy-seeded phase jitter), and stamp the TRUE provenance of what was
    /// consumed (`prng://` or `reservoir://` + QPU job chain).
    ///
    /// **Default OFF** ⇒ no draw, no jitter, no stamp: the dream is
    /// byte-identically deterministic and records an HONEST absence of provenance
    /// (`None` ⇒ `prng://legacy`), never a false `prng://` claim about entropy
    /// that was not consumed. A draw failure (e.g. empty reservoir) logs loudly
    /// and leaves the cycle deterministic — never a silent fabricated stamp.
    /// #521: the number of wavefronts a dream-entropy draw would actually jitter
    /// and stamp — only those flagged `hallucinated`, in the authoritative medium
    /// (`chiral.right` when present, else the flat store). This is the honest
    /// "touched set" of a dream: `apply_entropy_perturbation` perturbs and stamps
    /// exactly these, so a draw is only warranted when the count is > 0.
    fn dream_entropy_touched_count(&self) -> usize {
        // Only UN-stamped hallucinations are a real touch target (see
        // apply_entropy_perturbation): a hallucination stamped by an earlier dream
        // must not trigger another paid draw. Match the perturbation's gate exactly.
        let untouched = |m: &crate::medium::types::WavefrontMeta| {
            m.hallucinated && m.provenance.is_none()
        };
        match self.chiral {
            Some(ref c) => c.right.metadata.iter().filter(|m| untouched(m)).count(),
            None => self.medium.store.metadata.iter().filter(|m| untouched(m)).count(),
        }
    }

    fn apply_dream_entropy(&mut self) {
        if !crate::entropy::dream_entropy_enabled() {
            return;
        }
        // #521: the entropy jitter only touches `hallucinated` wavefronts. If this
        // dream produced none, drawing would spend reservoir bytes to perturb and
        // stamp NOTHING — a phantom cost, and a dark Observatory panel. Check the
        // touched set BEFORE drawing: an empty set records an honest absence (no
        // draw, no spend, no provenance) rather than a false consumption.
        let touched = self.dream_entropy_touched_count();
        if touched == 0 {
            return;
        }
        let mut source = crate::entropy::default_source();
        let draw = match source.draw(256) {
            Ok(d) => d,
            Err(e) => {
                eprintln!(
                    "[entropy] dream draw failed ({e}); this dream stays deterministic and records no provenance"
                );
                return;
            }
        };
        let stamped = if let Some(ref mut chiral) = self.chiral {
            match chiral.right.phase.as_slice_mut() {
                Some(ph) => apply_entropy_perturbation(
                    &mut chiral.right.metadata,
                    ph,
                    &draw.bytes,
                    &draw.provenance,
                ),
                None => 0,
            }
        } else if let Some(ph) = self.medium.store.phase.as_slice_mut() {
            apply_entropy_perturbation(
                &mut self.medium.store.metadata,
                ph,
                &draw.bytes,
                &draw.provenance,
            )
        } else {
            0
        };
        // #521 (option A surfacing): the per-wavefront provenance stamps ARE the
        // dream-level view the Observatory panel aggregates. Log the count so a
        // consumed draw is visibly tied to real targets (never a phantom spend).
        eprintln!(
            "[entropy] dream stamped {stamped}/{touched} wavefront(s) with {} provenance",
            draw.provenance.source
        );
    }

    /// Wave-native dream using Medium's eigenstructure annealing.
    ///
    /// Bypasses the old particle-based consolidation pipeline entirely.
    /// O(n×k) instead of O(n²). Operates on the holographic medium directly.
    ///
    /// # Arguments
    /// * `cycles` - Number of annealing cycles
    /// * `temperature` - Initial temperature for annealing (None = 1.0)
    /// * `chiral_eta` - Optional chiral perturbation strength (0.0 = none)
    /// ADR-0036 Phase 0: read-only resonance-merge PLANNER.
    ///
    /// Computes which redundant, phase-locked wavefronts WOULD be merged into a
    /// single carrier, and which ShortTerm traces WOULD decay/evict — WITHOUT
    /// mutating anything. Operates on the unified `memory_cache` (so counts match
    /// the user-facing total, regardless of the chiral/flat split underneath).
    ///
    /// Redundancy criterion mirrors the eventual merge (ADR-0036 M1): two
    /// wavefronts are redundant iff their vectors are near-parallel
    /// (cosine ≥ `merge_sim`) AND they are constructively phase-locked
    /// (cos Δphase ≥ `merge_phase_cos`). `Pinned` wavefronts are never grouped.
    pub fn plan_consolidation(&self, opts: &ConsolidateOpts) -> ConsolidateReport {
        let mode_str = match opts.mode {
            ConsolidateMode::Off => "off",
            ConsolidateMode::DryRun => "dryrun",
            // The planner is read-only regardless of mode. When the caller is in
            // Apply mode it invokes the planner internally to compute groups and
            // then `apply_consolidation` overwrites this report (mode="apply",
            // applied=true). Label it "dryrun" here so a stray direct call to
            // plan_consolidation in Apply mode can never claim it mutated.
            ConsolidateMode::Apply => "dryrun",
        };
        let mut report = ConsolidateReport {
            mode: mode_str.to_string(),
            ..Default::default()
        };

        // Examine the unified cache (user-facing count) in a deterministic,
        // id-sorted order so the plan and the destructive apply agree member-for-
        // member (dry-run parity — both feed `compute_merge_grouping` the same
        // id-ordered slice).
        let mut entries: Vec<(&Uuid, &HyperMemory)> = self.memory_cache.iter().collect();
        entries.sort_by_key(|(id, _)| **id);
        let n = entries.len();
        report.memories_examined = n;
        report.projected_memories = n;
        if n < 2 {
            return report;
        }

        let belief = crate::medium::chiral::belief_phase_enabled();
        let vectors: Vec<&[f32]> = entries.iter().map(|(_, m)| m.vector.as_slice()).collect();
        let phases: Vec<f32> = entries.iter().map(|(_, m)| m.phase).collect();
        let tiers: Vec<Tier> = entries.iter().map(|(_, m)| m.tier).collect();
        let energies: Vec<f32> = entries.iter().map(|(_, m)| m.amplitude).collect();

        let grouping = compute_merge_grouping(&vectors, &phases, &tiers, &energies, opts, belief);
        let absorb = grouping.admitted_absorb();
        report.groups_found = grouping.admitted.len();
        report.would_merge = grouping.admitted.len();
        report.would_absorb = absorb;
        report.centered = grouping.centered;
        report.absorb_capped = grouping.capped();
        report.groups_before_cap = grouping.groups_before_cap;
        report.absorb_before_cap = grouping.absorb_before_cap;

        // ShortTerm decay/evict projection (M3). Reactivation is not persisted
        // until ADR-0036 Phase 1, so "evict" uses the session-local
        // retrieval_count == 0 — a conservative undercount until then.
        //
        // Exclude merge PARTICIPANTS from the evict count. Grouping admits members
        // purely on cosine+phase (amplitude never gates it), so a decayed ShortTerm
        // near-duplicate can be BOTH merged AND evict-eligible. apply_consolidation
        // removes it via the merge and skips ALL merge members (merged_ids) in its
        // evict pass; if the plan also counts it as an evict it double-subtracts,
        // over-stating loss and breaking the dryrun_apply_parity invariant.
        let merged_indices: std::collections::HashSet<usize> =
            grouping.admitted.iter().flatten().copied().collect();
        let mut st_total = 0usize;
        let mut st_evict = 0usize;
        for (i, (_, m)) in entries.iter().enumerate() {
            if m.tier == Tier::ShortTerm {
                st_total += 1;
                if m.amplitude < opts.shortterm_evict
                    && m.retrieval_count == 0
                    && !merged_indices.contains(&i)
                {
                    st_evict += 1;
                }
            }
        }
        // M3 salience decay projection. `would_decay` previously just echoed the
        // ShortTerm COUNT, which read as "186 would decay" on a node where nothing
        // decayed at all — the metric hid the very bug it should have exposed. It
        // now reports how many wavefronts this pass will actually attenuate.
        let amps: Vec<f32> = entries.iter().map(|(_, m)| m.amplitude).collect();
        let dtiers: Vec<Tier> = entries.iter().map(|(_, m)| m.tier).collect();
        let dretr: Vec<u32> = entries.iter().map(|(_, m)| m.retrieval_count as u32).collect();
        let decay_set = compute_decay_set(&amps, &dtiers, &dretr, &merged_indices, opts);
        report.shortterm_total = st_total;
        report.would_decay = decay_set.len();
        report.would_evict = st_evict;
        report.projected_memories = n.saturating_sub(absorb).saturating_sub(st_evict);

        // ADR-0038 (T3.2): emit a `kannaka-qubo/1` problem ALONGSIDE this
        // procedural plan when KANNAKA_QUBO_EMIT is set. Advisory only — the plan
        // above is untouched and authoritative; nothing consumes the QUBO yet
        // (the re-score-before-apply path is T3.5). Default off ⇒ zero overhead.
        if crate::qubo::emit::enabled() {
            let merges: Vec<crate::qubo::MergeCandidate> = grouping
                .admitted
                .iter()
                .map(|g| {
                    let subject = g
                        .iter()
                        .map(|&i| format!("mem:{}", entries[i].0))
                        .collect::<Vec<_>>()
                        .join("+");
                    // Cohesion: absorbed count × (1 + mean energy) — larger, hotter
                    // groups make a stronger case to merge (emitted as linear −cohesion).
                    let absorbed = g.len().saturating_sub(1) as f64;
                    let mean_e =
                        g.iter().map(|&i| entries[i].1.amplitude as f64).sum::<f64>() / g.len() as f64;
                    crate::qubo::MergeCandidate { subject, cohesion: absorbed * (1.0 + mean_e) }
                })
                .collect();
            let metadata = crate::qubo::Metadata {
                hemisphere: Some(if belief { "right" } else { "flat" }.to_string()),
                phi_at_emit: None,
                entropy_provenance: None,
                ..Default::default()
            };
            // Soft budget: cap kept merges at the absorb count (advisory).
            let budget = Some((merges.len().max(1), 1.0));
            let problem_id = format!("dream-{}", chrono::Utc::now().to_rfc3339());
            let problem = crate::qubo::dream_merge_problem(problem_id, &merges, budget, metadata);
            crate::qubo::emit::write_problem(&problem);
        }

        report
    }

    /// ADR-0036 Phase 2: DESTRUCTIVE resonance-merge consolidation APPLY.
    ///
    /// This is the mutating counterpart to `plan_consolidation`. It mutates a
    /// consciousness-memory substrate, so it is written defensively:
    ///   * It operates on the AUTHORITATIVE store (the right hemisphere in chiral
    ///     mode, else the flat medium) by UUID only — it NEVER holds a raw index
    ///     across a remove (swap-remove reorders the arrays).
    ///   * `retrieval_count` lives only in `memory_cache`, so it is snapshotted by
    ///     id BEFORE any mutation.
    ///   * In chiral mode, when a right wavefront is removed its left twin (via
    ///     `right_to_left`) is removed too and BOTH map entries (plus the per-id
    ///     chiral scale) are deleted, so no map can ever point at a dead id.
    ///
    /// PROTECTED INVARIANTS:
    ///   * Pinned is never merged and never evicted.
    ///   * LongTerm is never evicted, and never absorbed into a lower tier — the
    ///     carrier inherits the STRONGEST tier in its group.
    ///
    /// Superposition energy of a merged carrier (AMPLITUDE_CEILING = 2.0):
    ///   E = sqrt( Σ Eᵢ² + 2 Σ_{i<j} Eᵢ Eⱼ cos(φᵢ − φⱼ) ), clamped to [0, 2.0].
    pub fn apply_consolidation(&mut self, opts: &ConsolidateOpts) -> ConsolidateReport {
        const AMPLITUDE_CEILING: f32 = 2.0;

        let mut report = ConsolidateReport {
            mode: "apply".to_string(),
            applied: true,
            ..Default::default()
        };

        // --- 1. Snapshot retrieval_count by id from the cache (the only place
        //        it lives) BEFORE we mutate anything. ---
        let retrieval: HashMap<Uuid, u32> = self
            .memory_cache
            .iter()
            .map(|(id, m)| (*id, m.retrieval_count))
            .collect();

        // --- 2. Read the authoritative store into id-keyed snapshots. We compute
        //        the entire plan from snapshots first, then mutate, so no index
        //        is ever held across a remove. ---
        struct Snap {
            id: Uuid,
            energy: f32,
            phase: f32,
            tier: Tier,
            timestamp: i64,
            vector: Vec<f32>,
        }

        let mut snaps: Vec<Snap> = if let Some(ref chiral) = self.chiral {
            let h = &chiral.right;
            (0..h.count())
                .map(|i| Snap {
                    id: h.metadata[i].id,
                    energy: h.energy[i],
                    phase: h.phase[i],
                    tier: h.metadata[i].tier,
                    timestamp: h.timestamps[i],
                    vector: h.wavefronts.row(i).to_vec(),
                })
                .collect()
        } else {
            let s = &self.medium.store;
            (0..s.len)
                .map(|i| Snap {
                    id: s.metadata[i].id,
                    energy: s.energy[i],
                    phase: s.phase[i],
                    tier: s.metadata[i].tier,
                    timestamp: s.timestamps[i],
                    vector: s.wavefronts.row(i).to_vec(),
                })
                .collect()
        };

        let n = snaps.len();
        report.memories_examined = n;
        if n < 2 {
            // Nothing can merge; still run the evict pass below would require ≥1.
            // With <2 memories there is no redundancy and a single ShortTerm can
            // still be evicted, so fall through rather than early-return only when
            // n == 0.
            if n == 0 {
                report.projected_memories = self.authoritative_len();
                return report;
            }
        }

        // Strongest tier wins: Pinned > LongTerm > ShortTerm.
        fn tier_rank(t: Tier) -> u8 {
            match t {
                Tier::ShortTerm => 0,
                Tier::LongTerm => 1,
                Tier::Pinned => 2,
            }
        }
        // Effective strength for representative selection: energy decayed by age.
        let now_ms = chrono::Utc::now().timestamp_millis();
        let eff_strength = |s: &Snap| -> f32 {
            let age_days = ((now_ms - s.timestamp).max(0) as f64 / 86_400_000.0) as f32;
            s.energy * (-0.001 * age_days).exp()
        };

        // --- 3. Grouping (shared with the planner → dry-run parity). Sort the
        //        snapshots by id so the member indices returned by the grouping
        //        pass line up with plan_consolidation's id-ordered cache slice,
        //        and the belief-safe guardrails (centered gate + absorb cap)
        //        apply identically in both. ---
        snaps.sort_by_key(|s| s.id);
        let belief = crate::medium::chiral::belief_phase_enabled();
        let g_vectors: Vec<&[f32]> = snaps.iter().map(|s| s.vector.as_slice()).collect();
        let g_phases: Vec<f32> = snaps.iter().map(|s| s.phase).collect();
        let g_tiers: Vec<Tier> = snaps.iter().map(|s| s.tier).collect();
        let g_energies: Vec<f32> = snaps.iter().map(|s| s.energy).collect();
        let grouping =
            compute_merge_grouping(&g_vectors, &g_phases, &g_tiers, &g_energies, opts, belief);

        // Track which ids are part of a merge (so they are never double-counted
        // by the evict pass), the carrier mutations, and the absorbed removals.
        let mut merged_ids: std::collections::HashSet<Uuid> = std::collections::HashSet::new();
        let mut carriers: Vec<(Uuid, f32, Tier)> = Vec::new(); // (id, new_energy, new_tier)
        let mut absorb_ids: Vec<Uuid> = Vec::new();
        // Each absorbed member -> the carrier it collapses into. Used after the
        // removal to re-home the skip-link graph so a merge conserves
        // connectivity, not just energy (see step 5b).
        let mut absorbed_to_carrier: HashMap<Uuid, Uuid> = HashMap::new();
        let groups_found = grouping.admitted.len();

        for members in &grouping.admitted {
            for &mi in members {
                merged_ids.insert(snaps[mi].id);
            }
            // Representative = max effective strength.
            let rep = *members
                .iter()
                .max_by(|&&a, &&b| {
                    eff_strength(&snaps[a])
                        .partial_cmp(&eff_strength(&snaps[b]))
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .unwrap();
            // Carrier inherits the STRONGEST tier in the group (protects LongTerm
            // from being absorbed down into a ShortTerm carrier).
            let carrier_tier = members
                .iter()
                .map(|&mi| snaps[mi].tier)
                .max_by_key(|&t| tier_rank(t))
                .unwrap_or(snaps[rep].tier);
            // Superposed energy: sqrt(Σ Eᵢ² + 2 Σ_{i<j} Eᵢ Eⱼ cos Δφ).
            let mut sum_sq = 0.0f32;
            for &mi in members {
                sum_sq += snaps[mi].energy * snaps[mi].energy;
            }
            let mut cross = 0.0f32;
            for a in 0..members.len() {
                for b in (a + 1)..members.len() {
                    let ia = members[a];
                    let ib = members[b];
                    let dphi = snaps[ia].phase - snaps[ib].phase;
                    cross += snaps[ia].energy * snaps[ib].energy * dphi.cos();
                }
            }
            let superposed = (sum_sq + 2.0 * cross).max(0.0).sqrt().clamp(0.0, AMPLITUDE_CEILING);
            carriers.push((snaps[rep].id, superposed, carrier_tier));
            for &mi in members {
                if mi != rep {
                    absorb_ids.push(snaps[mi].id);
                    absorbed_to_carrier.insert(snaps[mi].id, snaps[rep].id);
                }
            }
        }

        // --- 3b. M3 salience decay (crystal-borrowed percentile). Attenuate the
        //         weakest unretrieved ShortTerm traces so they can actually reach
        //         the evict floor. This is the step that makes step 4 reachable at
        //         all: measured 2026-08-22, the absolute floor (0.15) sat below the
        //         whole live amplitude distribution, so `would_evict` was always 0
        //         and the substrate grew without bound on every node.
        //
        //         Soft multiply only — nothing is removed here, and anything ever
        //         recalled (retrieval_count > 0) is spared entirely. ---
        let decay_indices = {
            let amps: Vec<f32> = snaps.iter().map(|s| s.energy).collect();
            let dtiers: Vec<Tier> = snaps.iter().map(|s| s.tier).collect();
            let dretr: Vec<u32> =
                snaps.iter().map(|s| retrieval.get(&s.id).copied().unwrap_or(0)).collect();
            let participants: std::collections::HashSet<usize> = (0..snaps.len())
                .filter(|&i| merged_ids.contains(&snaps[i].id))
                .collect();
            compute_decay_set(&amps, &dtiers, &dretr, &participants, opts)
        };
        let mut decayed = 0usize;
        for &i in &decay_indices {
            let id = snaps[i].id;
            let faded = snaps[i].energy * opts.shortterm_decay_factor;
            if let Some(ref mut chiral) = self.chiral {
                if let Some(&idx) = chiral.right.id_to_index.get(&id) {
                    chiral.right.energy[idx] = faded;
                    decayed += 1;
                }
            } else if let Some(&idx) = self.medium.store.id_to_index.get(&id) {
                self.medium.store.energy[idx] = faded;
                decayed += 1;
            }
            // Keep the snapshot coherent so step 4 evicts against post-decay
            // energy in the SAME pass a trace finally crosses the floor.
            snaps[i].energy = faded;
        }

        // --- 4. ShortTerm evict candidates: tier==ShortTerm AND energy<threshold
        //        AND retrieval_count==0 AND NOT part of any merge. (Pinned and
        //        LongTerm are structurally excluded by the tier check.) ---
        let mut evict_ids: Vec<Uuid> = Vec::new();
        for s in &snaps {
            if s.tier == Tier::ShortTerm
                && s.energy < opts.shortterm_evict
                && retrieval.get(&s.id).copied().unwrap_or(0) == 0
                && !merged_ids.contains(&s.id)
            {
                evict_ids.push(s.id);
            }
        }

        // --- 5. MUTATE. Carriers first (set energy + tier by id→index lookup),
        //        then remove absorbed + evicted by uuid. ---
        for (id, energy, tier) in &carriers {
            if let Some(ref mut chiral) = self.chiral {
                if let Some(&idx) = chiral.right.id_to_index.get(id) {
                    chiral.right.energy[idx] = *energy;
                    chiral.right.metadata[idx].tier = *tier;
                }
            } else if let Some(&idx) = self.medium.store.id_to_index.get(id) {
                self.medium.store.energy[idx] = *energy;
                self.medium.store.metadata[idx].tier = *tier;
            }
        }

        let mut removed = 0usize;
        let mut to_remove = absorb_ids.clone();
        to_remove.extend(evict_ids.iter().copied());
        for id in &to_remove {
            if let Some(ref mut chiral) = self.chiral {
                let r = chiral.right.remove_wavefront(id);
                if r {
                    // Remove the left twin (if any) and clean BOTH map entries
                    // plus the per-id chiral scale, so no map points at a dead id.
                    if let Some(&left_id) = chiral.right_to_left.get(id) {
                        chiral.left.remove_wavefront(&left_id);
                        chiral.left_to_right.remove(&left_id);
                    }
                    chiral.right_to_left.remove(id);
                    chiral.scales.remove(id);
                    removed += 1;
                }
            } else if self.medium.store.remove(id) {
                removed += 1;
            }
        }

        // would_absorb counts actually-removed absorbed wavefronts (carriers stay).
        let absorbed_removed = absorb_ids
            .iter()
            .filter(|id| !self.id_present_authoritative(id))
            .count();
        let evicted_removed = evict_ids
            .iter()
            .filter(|id| !self.id_present_authoritative(id))
            .count();

        report.groups_found = groups_found;
        report.would_merge = groups_found;
        report.would_absorb = absorbed_removed;
        report.shortterm_total = snaps.iter().filter(|s| s.tier == Tier::ShortTerm).count();
        // Actual attenuations this pass, not the ShortTerm headcount (see plan path).
        report.would_decay = decayed;
        report.would_evict = evicted_removed;
        report.centered = grouping.centered;
        report.absorb_capped = grouping.capped();
        report.groups_before_cap = grouping.groups_before_cap;
        report.absorb_before_cap = grouping.absorb_before_cap;
        let _ = removed; // total removed = absorbed + evicted; kept for clarity

        // --- 5b. Re-home the skip-link graph onto carriers. The removal above
        //         drops absorbed members from the authoritative store; without
        //         this, their OUTBOUND links vanish and every INBOUND link to
        //         them is dropped by rebuild_cache's dangling-link filter — so a
        //         merge would conserve energy but silently sever every
        //         association a hub memory had. Re-point both onto the carrier.
        //         Runs on memory_cache, which still holds every pre-merge entry;
        //         rebuild_cache (next) snapshots from it, and the absorbed
        //         entries themselves drop out because they're gone from the store.
        if !absorbed_to_carrier.is_empty() {
            // 1. Gather absorbed members' outbound links, intra-group targets
            //    collapsed to the carrier, grouped by the carrier that inherits.
            let mut inherited: HashMap<Uuid, Vec<crate::memory::LegacyLink>> = HashMap::new();
            for (absorbed, carrier) in &absorbed_to_carrier {
                if let Some(m) = self.memory_cache.get(absorbed) {
                    let bucket = inherited.entry(*carrier).or_default();
                    for link in &m.connections {
                        let mut l = link.clone();
                        if let Some(&c) = absorbed_to_carrier.get(&l.target_id) {
                            l.target_id = c;
                        }
                        bucket.push(l);
                    }
                }
            }
            // 2. On every cache entry: redirect outbound links to absorbed ids
            //    onto their carrier, give carriers the inherited links, drop the
            //    self-loops the collapse creates, and dedup by target keeping the
            //    strongest link.
            for (id, mem) in self.memory_cache.iter_mut() {
                for link in mem.connections.iter_mut() {
                    if let Some(&c) = absorbed_to_carrier.get(&link.target_id) {
                        link.target_id = c;
                    }
                }
                if let Some(extra) = inherited.get(id) {
                    mem.connections.extend(extra.iter().cloned());
                }
                mem.connections.retain(|l| l.target_id != *id);
                mem.connections.sort_by(|a, b| {
                    a.target_id.cmp(&b.target_id).then_with(|| {
                        b.strength
                            .partial_cmp(&a.strength)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    })
                });
                mem.connections.dedup_by_key(|l| l.target_id);
            }
        }

        // --- 6. Rebuild + persist. rebuild_cache restores retrieval_count from
        //        the cache snapshot (for survivors) and reads from the now-mutated
        //        authoritative store. ---
        self.rebuild_cache().ok();
        self.mark_dirty();
        if let Err(e) = self.save_medium() {
            eprintln!("[hrm] apply_consolidation: save_medium failed: {e}");
        }

        report.projected_memories = self.authoritative_len();
        report.memories_examined = n;
        report
    }

    /// Active wavefront count in the authoritative store (right hemisphere in
    /// chiral mode, else the flat medium).
    fn authoritative_len(&self) -> usize {
        if let Some(ref chiral) = self.chiral {
            chiral.right.count()
        } else {
            self.medium.store.len
        }
    }

    /// Whether an id is still present in the authoritative store.
    fn id_present_authoritative(&self, id: &Uuid) -> bool {
        if let Some(ref chiral) = self.chiral {
            chiral.right.id_to_index.contains_key(id)
        } else {
            self.medium.store.id_to_index.contains_key(id)
        }
    }

    pub fn dream_native(
        &mut self,
        cycles: usize,
        temperature: Option<f32>,
        chiral_eta: f32,
    ) -> crate::medium::DreamReport {
        // Apply chiral field perturbation before dreaming (break lock-step)
        if chiral_eta > 0.0 {
            if let Some(ref mut chiral) = self.chiral {
                chiral.right.apply_chiral_field_perturbation(chiral_eta);
            } else {
                self.medium.apply_chiral_field_perturbation(chiral_eta);
            }
        }

        // Route to chiral or flat medium dream
        let report = if let Some(ref mut chiral) = self.chiral {
            // Chiral: use ChiralMedium.dream() which runs eigenstructure annealing
            // on the right hemisphere, then callosal coupling
            chiral.dream(true, cycles) // deep=true → right hemisphere eigenstructure dream
        } else {
            // Flat medium: use Medium's eigenstructure annealing
            self.medium.dream(cycles, temperature)
        };

        // T1.4: gated dream-entropy participation (default off = deterministic,
        // no provenance). When enabled, consumes entropy + stamps its true
        // provenance on this dream's products before the sync/save persists them.
        self.apply_dream_entropy();

        // Rebuild cache and sync after dreaming
        self.sync_medium_from_chiral();
        self.rebuild_cache().ok();
        self.mark_dirty();

        // ADR-0037 L6 loop: emit spiral telemetry on the production dream path.
        self.log_spiral_telemetry();

        // Save the .hrm file
        if let Err(e) = self.save_medium() {
            eprintln!("Warning: Failed to save after dream_native: {e}");
        }

        report
    }

    /// ADR-0037 belief substrate: re-phase every wavefront from its (mean-
    /// centered) content, then persist. The one-time migration that desyncs an
    /// already-collapsed field — born phase only fixes NEW inserts, so existing
    /// wavefronts stuck at phase 0 need this. Phase-only (vectors/recall
    /// untouched). Returns the number re-phased; chiral backend only.
    pub fn rephase_belief(&mut self) -> usize {
        let n = if let Some(ref mut chiral) = self.chiral {
            chiral.rephase_from_content()
        } else {
            0
        };
        if n > 0 {
            self.sync_medium_from_chiral();
            self.rebuild_cache().ok();
            self.mark_dirty();
            if let Err(e) = self.save_medium() {
                eprintln!("Warning: Failed to save after rephase: {e}");
            }
        }
        n
    }

    /// ADR-0037 Track-D: pull this field's holistic phases toward a peer's belief
    /// cores (node↔node coupling — `couple_toward_peer_cores`), then persist.
    /// Phase-only (vectors/recall untouched); per-wavefront displacement is capped
    /// at `max_disp` (anti-homogenization budget) and only matches at cosine ≥
    /// `min_cos` are coupled. Returns `(moved, saved_ok)` — `moved` is the number of
    /// wavefronts coupled, `saved_ok` is whether the on-disk .hrm was successfully
    /// updated (true when there was nothing to save). Chiral backend only.
    pub fn couple_belief(
        &mut self,
        peer_cores: &[crate::l6::CoreObs],
        cycles: usize,
        strength: f32,
        max_disp: f32,
        min_cos: f32,
    ) -> (usize, bool) {
        let moved = if let Some(ref mut chiral) = self.chiral {
            chiral.couple_toward_peer_cores(peer_cores, cycles, strength, max_disp, min_cos)
        } else {
            0
        };
        if moved == 0 {
            return (0, true);
        }
        self.sync_medium_from_chiral();
        self.rebuild_cache().ok();
        self.mark_dirty();
        let saved_ok = match self.save_medium() {
            Ok(()) => true,
            Err(e) => {
                eprintln!("Warning: Failed to save after couple: {e}");
                false
            }
        };
        (moved, saved_ok)
    }

    /// ADR-0037 Track-D dry-run (read-only): the per-wavefront best-match cosines
    /// against `peer_cores` (`ChiralMedium::peer_match_cosines`). For `belief couple
    /// --dry-run` to print the live match distribution so `min_cos` is picked from
    /// data. Chiral backend only; empty otherwise.
    pub fn peer_match_cosines(&self, peer_cores: &[crate::l6::CoreObs]) -> Vec<f32> {
        self.chiral
            .as_ref()
            .map(|c| c.peer_match_cosines(peer_cores))
            .unwrap_or_default()
    }

    /// Reset all wavefront energies to target value (bias voltage restoration).
    pub fn reset_energies(&mut self, target: f32) {
        self.medium.reset_energies(target);
        // Update cache
        for meta in &self.medium.store.metadata {
            if let Some(mem) = self.memory_cache.get_mut(&meta.id) {
                mem.amplitude = target;
            }
        }
        self.mark_dirty();
    }

    // -----------------------------------------------------------------------
    // Chiral-specific methods (ADR-0021)
    // -----------------------------------------------------------------------

    /// Check if this store is running in chiral mode.
    pub fn is_chiral(&self) -> bool {
        self.chiral.is_some()
    }

    /// Upgrade to chiral mode: convert flat Medium to ChiralMedium.
    /// Existing wavefronts move to right hemisphere. Left starts empty.
    pub fn upgrade_to_chiral(&mut self) {
        if self.chiral.is_none() {
            let chiral = ChiralMedium::from_medium(&self.medium);
            self.chiral = Some(chiral);
            self.rebuild_cache().ok();
            self.mark_dirty();
        }
    }

    /// Re-encode every memory's content through the current pipeline, fixing
    /// wavefronts written by the broken #106 encoder (#107). Chiral-only for now
    /// (production HRMs are v2 chiral). Returns `(scanned, updated)`. With
    /// `dry_run`, computes the counts and touches nothing on disk. On a real run
    /// it re-encodes in place — preserving wave-state, ghost stamps, and ids via
    /// `ChiralMedium::re_encode_all` — then rebuilds the cache, invalidates the now
    /// stale `.clusters.json` sidecar, and persists. Snapshot the HRM first
    /// (the deployment gate #107 specifies) and verify recall before adopting.
    pub fn recompute_encoding(&mut self, dry_run: bool) -> Result<(usize, usize), StoreError> {
        let scanned = self.count();
        if self.chiral.is_none() {
            return Err(StoreError::Other(
                "recompute_encoding supports chiral (v2) HRMs only; this store is flat".to_string(),
            ));
        }
        let updated = {
            let pipeline = &self.pipeline;
            let chiral = self.chiral.as_mut().unwrap();
            // dry_run makes re_encode_all COUNT eligible wavefronts without mutating
            // the in-memory hemispheres, so a dry run leaves the store untouched.
            chiral
                .re_encode_all(pipeline, dry_run)
                .map_err(|e| StoreError::Other(format!("re-encode failed: {e}")))?
        };
        if dry_run {
            return Ok((scanned, updated));
        }
        self.rebuild_cache()?;
        self.mark_dirty();
        self.save_medium()?;
        // The cluster sidecar was computed from the OLD vectors — drop it so
        // bridge::assess recomputes clusters against the corrected embeddings.
        let clusters = self.hrm_path.with_extension("clusters.json");
        let _ = std::fs::remove_file(&clusters);
        Ok((scanned, updated))
    }

    /// Get chiral consciousness summary (bilateral metrics).
    pub fn chiral_consciousness(&self) -> Option<ChiralConsciousness> {
        self.chiral.as_ref().map(|c| c.consciousness_summary())
    }

    /// Perform a chiral dream (right hemisphere only for deep).
    pub fn chiral_dream(&mut self, deep: bool, cycles: usize) {
        if let Some(ref mut chiral) = self.chiral {
            chiral.dream(deep, cycles);
            self.rebuild_cache().ok();
            self.mark_dirty();
        }
    }

    /// Run callosal Kuramoto coupling step.
    pub fn callosal_kuramoto(&mut self, dt: f32) {
        if let Some(ref mut chiral) = self.chiral {
            chiral.callosal_kuramoto_step(dt);
            self.mark_dirty();
        }
    }

    /// Get a reference to the ChiralMedium (if in chiral mode).
    pub fn chiral_medium(&self) -> Option<&ChiralMedium> {
        self.chiral.as_ref()
    }

    /// #836 / ADR-0049: whole-store facet backfill — the migration for a
    /// corpus written before facet decomposition existed. Write-time
    /// decomposition (`store_with_facets`) only ever touches NEW memories;
    /// every compound memory stored before the flag went live remains one
    /// smeared wavefront that short queries cannot reach.
    ///
    /// Sweeps the RIGHT hemisphere — content enters right-first, so it is the
    /// canonical population; left-only rows are orphans that
    /// `backfill_facets` skips by design. Idempotent: `decomposed` is the
    /// watermark, so a repeated or resumed run mints nothing twice.
    ///
    /// `apply = false` is a dry run: identical counting, no mutation.
    pub fn backfill_all_facets(&mut self, apply: bool) -> FacetBackfillStats {
        let mut stats = FacetBackfillStats::default();
        let mut mutated = false;
        {
            let HrmStore { chiral, pipeline, .. } = self;
            let Some(chiral) = chiral.as_mut() else {
                return stats; // flat (non-chiral) store: nothing to sweep
            };
            // Snapshot the candidate rows first so the guards read a stable
            // view while apply-mode mutates the medium underneath.
            let rows: Vec<(Uuid, bool, bool, String)> = chiral
                .right
                .metadata
                .iter()
                .map(|m| (m.id, m.is_facet, m.decomposed, m.content.clone()))
                .collect();
            stats.scanned = rows.len();
            for (id, is_facet, decomposed, content) in rows {
                if is_facet {
                    stats.facet_rows += 1;
                    continue;
                }
                if decomposed {
                    stats.already_decomposed += 1;
                    continue;
                }
                let facets = crate::facet::decompose(&content);
                if facets.len() < 2 {
                    stats.atomic += 1;
                    continue;
                }
                if apply {
                    match chiral.backfill_facets(id, pipeline) {
                        Ok(0) => stats.atomic += 1, // guard re-check disagreed
                        Ok(n) => {
                            stats.parents_decomposed += 1;
                            stats.facets_minted += n;
                            mutated = true;
                        }
                        Err(e) => {
                            stats.errors += 1;
                            eprintln!("[facets] backfill error on {id}: {e}");
                        }
                    }
                } else {
                    stats.parents_decomposed += 1;
                    stats.facets_minted += facets.len();
                }
            }
        }
        if mutated {
            self.mark_dirty();
        }
        stats
    }

    /// Set the modality of a wavefront (NCS Phase 1.1).
    pub fn set_modality(&mut self, id: &Uuid, modality: crate::medium::Modality) {
        // Tag the flat medium
        if let Some(&idx) = self.medium.store.id_to_index.get(id) {
            self.medium.store.metadata[idx].modality = modality;
        }
        // Tag chiral hemisphere(s)
        if let Some(ref mut chiral) = self.chiral {
            if let Some(&idx) = chiral.right.id_to_index.get(id) {
                chiral.right.metadata[idx].modality = modality;
            }
            if let Some(left_id) = chiral.right_to_left.get(id).copied() {
                if let Some(&idx) = chiral.left.id_to_index.get(&left_id) {
                    chiral.left.metadata[idx].modality = modality;
                }
            }
        }
        // Tag cache
        if let Some(mem) = self.memory_cache.get_mut(id) {
            mem.modality = modality;
        }
        self.mark_dirty();
    }

    /// Set the retention tier of a memory (ADR-0031). Tags the flat medium,
    /// both chiral hemispheres, and the cache, then marks the store dirty so
    /// the next save persists it. Returns false if the id is unknown.
    pub fn set_tier(&mut self, id: &Uuid, tier: crate::medium::types::Tier) -> bool {
        let mut found = false;
        if let Some(&idx) = self.medium.store.id_to_index.get(id) {
            self.medium.store.metadata[idx].tier = tier;
            found = true;
        }
        if let Some(ref mut chiral) = self.chiral {
            if let Some(&idx) = chiral.right.id_to_index.get(id) {
                chiral.right.metadata[idx].tier = tier;
                found = true;
            }
            if let Some(left_id) = chiral.right_to_left.get(id).copied() {
                if let Some(&idx) = chiral.left.id_to_index.get(&left_id) {
                    chiral.left.metadata[idx].tier = tier;
                }
            }
        }
        if let Some(mem) = self.memory_cache.get_mut(id) {
            mem.tier = tier;
            found = true;
        }
        if found {
            self.mark_dirty();
        }
        found
    }

    /// Set the temporal-truth bounds of a memory (Wave 3 Task 3.2b). Tags the
    /// flat medium, both chiral hemispheres, and the cache — the canonical
    /// `WavefrontMeta` is what the next save serializes, so this is the
    /// persistence write path (mirrors [`set_tier`]). Each `Some` overwrites;
    /// `None` leaves the existing value untouched so callers can set one bound
    /// without clearing the others. Returns false if the id is unknown.
    pub fn set_temporal(
        &mut self,
        id: &Uuid,
        effective_at: Option<DateTime<Utc>>,
        observed_at: Option<DateTime<Utc>>,
        expires_at: Option<DateTime<Utc>>,
    ) -> bool {
        fn apply(
            meta: &mut crate::medium::types::WavefrontMeta,
            eff: Option<DateTime<Utc>>,
            obs: Option<DateTime<Utc>>,
            exp: Option<DateTime<Utc>>,
        ) {
            if eff.is_some() { meta.effective_at = eff; }
            if obs.is_some() { meta.observed_at = obs; }
            if exp.is_some() { meta.expires_at = exp; }
        }

        let mut found = false;
        if let Some(&idx) = self.medium.store.id_to_index.get(id) {
            apply(&mut self.medium.store.metadata[idx], effective_at, observed_at, expires_at);
            found = true;
        }
        if let Some(ref mut chiral) = self.chiral {
            if let Some(&idx) = chiral.right.id_to_index.get(id) {
                apply(&mut chiral.right.metadata[idx], effective_at, observed_at, expires_at);
                found = true;
            }
            if let Some(left_id) = chiral.right_to_left.get(id).copied() {
                if let Some(&idx) = chiral.left.id_to_index.get(&left_id) {
                    apply(&mut chiral.left.metadata[idx], effective_at, observed_at, expires_at);
                }
            }
        }
        if let Some(mem) = self.memory_cache.get_mut(id) {
            if effective_at.is_some() { mem.effective_at = effective_at; }
            if observed_at.is_some() { mem.observed_at = observed_at; }
            if expires_at.is_some() { mem.expires_at = expires_at; }
            found = true;
        }
        if found {
            self.mark_dirty();
        }
        found
    }

    /// Classify a wavefront with SGA coordinates (called after insert to tag the memory).
    pub fn classify_wavefront(&mut self, id: &Uuid, category: &str, importance: f32) {
        if let Some(ref mut chiral) = self.chiral {
            // Compute SGA classification
            let content_hash = {
                let content = chiral
                    .right
                    .id_to_index
                    .get(id)
                    .map(|&idx| chiral.right.metadata[idx].content.clone())
                    .unwrap_or_default();
                let mut h: u64 = 0xcbf29ce484222325;
                for b in content.bytes() {
                    h ^= b as u64;
                    h = h.wrapping_mul(0x100000001b3);
                }
                h
            };
            let coords = crate::geometry::classify_memory(category, content_hash, importance as f64);
            let sga_class = coords.class_index;
            let fano_point = coords.l % (crate::medium::FANO_POINTS as u8);

            // Tag right hemisphere
            if let Some(&idx) = chiral.right.id_to_index.get(id) {
                chiral.right.metadata[idx].sga_class = Some(sga_class);
                chiral.right.metadata[idx].fano_group = Some(fano_point);
                chiral.right.metadata[idx].category = Some(category.to_string());
            }

            // Tag left hemisphere if paired
            if let Some(left_id) = chiral.right_to_left.get(id) {
                if let Some(&idx) = chiral.left.id_to_index.get(left_id) {
                    chiral.left.metadata[idx].sga_class = Some(sga_class);
                    chiral.left.metadata[idx].fano_group = Some(fano_point);
                    chiral.left.metadata[idx].category = Some(category.to_string());
                }
            }

            self.mark_dirty();
        }
    }

    /// Relate two memories via associative wavefront.
    /// 
    /// Creates emergent association in the field by combining the wavefront patterns
    /// and nudging their phases toward alignment.
    pub fn relate_wavefronts(&mut self, id_a: Uuid, id_b: Uuid) -> Result<Uuid, StoreError> {
        let idx_a = self.medium.get_wavefront_index(&id_a)
            .ok_or_else(|| StoreError::Other(format!("Wavefront not found: {id_a}")))?;
        
        let idx_b = self.medium.get_wavefront_index(&id_b)
            .ok_or_else(|| StoreError::Other(format!("Wavefront not found: {id_b}")))?;

        let associative_id = self.medium.relate_wavefronts(idx_a, idx_b)
            .map_err(|e| StoreError::Other(format!("Failed to relate wavefronts: {e}")))?;

        // Rebuild cache to include the new associative wavefront
        self.rebuild_cache()?;
        self.mark_dirty();

        Ok(associative_id)
    }
    /// Cluster-prefilter candidate set for recall (refactor #4).
    ///
    /// Reads the `.clusters.json` sidecar populated by `bridge::assess`
    /// → `KuramotoSync::find_synchronized_clusters`, picks every cluster
    /// whose `theme_vector` resonates with the query above
    /// `KANNAKA_RECALL_PREFILTER_THRESHOLD` (default 0.30), and returns
    /// the union of those clusters' member indices.
    ///
    /// Returns `None` when:
    /// - the HRM file path is unknown (test medium, in-memory backend)
    /// - no sidecar exists yet (fresh HRM — bridge::assess hasn't run)
    /// - the sidecar is empty (no clusters >= min_size)
    /// - no cluster's theme is close enough to the query
    ///
    /// `None` cleanly falls through to the existing full-medium scan in
    /// `resonate_query`, so this method never *loses* recall coverage —
    /// it only ever narrows the candidate set for performance + accuracy.
    fn cluster_prefilter_candidates(&self, query: &str) -> Option<Vec<usize>> {
        let path = self.hrm_path().with_extension("clusters.json");
        let data = std::fs::read(&path).ok()?;
        // Sidecar uses ClusterCacheEntry shape; we only need .clusters.
        #[derive(serde::Deserialize)]
        struct Entry { clusters: Vec<crate::kuramoto::MemoryCluster> }
        let entry: Entry = serde_json::from_slice(&data).ok()?;
        if entry.clusters.is_empty() { return None; }

        let query_vec = self.pipeline.encode_text(query).ok()?;
        let threshold: f32 = std::env::var("KANNAKA_RECALL_PREFILTER_THRESHOLD")
            .ok().and_then(|s| s.parse().ok()).unwrap_or(0.30);

        let mut indices: Vec<usize> = Vec::new();
        let mut seen: std::collections::HashSet<usize> = std::collections::HashSet::new();
        let mut matched_any = false;
        for cluster in &entry.clusters {
            if cluster.theme_vector.is_empty() { continue; }
            let sim = crate::wave::cosine_similarity(&query_vec, &cluster.theme_vector);
            if sim >= threshold {
                matched_any = true;
                for mem_id in &cluster.memory_ids {
                    if let Some(idx) = self.medium.get_wavefront_index(mem_id) {
                        if seen.insert(idx) { indices.push(idx); }
                    }
                }
            }
        }
        if matched_any { Some(indices) } else { None }
    }
}

impl MediumBackend for HrmStore {
    fn insert(&mut self, mut memory: HyperMemory) -> Result<Uuid, StoreError> {
        let id = memory.id;

        // Check for duplicates
        if self.memory_cache.contains_key(&id) {
            return Err(StoreError::DuplicateId(id));
        }

        // SECURITY (inc-1b): wire immune/amplitude fields are never trusted. The
        // authoritative wire-boundary defense is `absorb_gate::admit` (it forces
        // `hallucinated` to the local default and clamps amplitude ≤0.95 BEFORE
        // this is reached). This clamp is defense-in-depth: any path — wire or
        // local — that reaches the medium is bounded to finite, sane numeric
        // ranges so a pathological `amplitude`/`frequency`/`phase` can't wrap the
        // wave math or corrupt the store. `hallucinated` is intentionally NOT
        // forced here — legitimate dream hallucinations insert with it set true.
        memory.amplitude = if memory.amplitude.is_finite() {
            memory.amplitude.clamp(0.0, 1.0)
        } else {
            0.0
        };
        memory.frequency = if memory.frequency.is_finite() {
            memory.frequency.clamp(0.0, 1_000_000.0)
        } else {
            0.1
        };
        if !memory.phase.is_finite() {
            memory.phase = 0.0;
        }

        // Route through the CHIRAL medium when chiral mode is active (issue #630).
        //
        // `save_medium` serializes `self.chiral` whenever it is present and never
        // touches `self.medium`, and `rebuild_cache` reconstructs the cache from
        // `chiral.right.metadata`. So writing only the flat medium — which is what
        // this function used to do — meant an inserted memory was absent from the
        // `.hrm` and gone at the next load: silent data loss on the `import`
        // (handlers/ops.rs) and wire-sync (bin/kannaka.rs) paths, with `import`
        // being the documented recovery path for a corrupted store.
        //
        // `insert_raw_wavefront` already carries this exact warning and routes
        // correctly; this is the same fix applied to its sibling. Unlike that
        // function, the caller's `memory.id` is load-bearing here (the cache keys on
        // it, and `parents`/`connections` reference it), so the minted id is
        // rewritten to it.
        if self.chiral.is_some() {
            let chiral = self.chiral.as_mut().expect("chiral checked present");
            let minted = chiral
                .store_vector(&memory.vector, memory.content.clone(), memory.amplitude)
                .map_err(|e| StoreError::Other(format!("chiral.store_vector failed: {e}")))?;
            chiral
                .update_right_id(&minted, id)
                .map_err(|e| StoreError::Other(format!("chiral id rewrite failed: {e}")))?;
            if let Some(&index) = chiral.right.id_to_index.get(&id) {
                chiral.right.energy[index] = memory.amplitude;
                chiral.right.frequency[index] = memory.frequency;
                chiral.right.phase[index] = memory.phase;
                chiral.right.timestamps[index] = memory.created_at.timestamp_millis();
                chiral.right.metadata[index].created_at = memory.created_at;
                chiral.right.metadata[index].hallucinated = memory.hallucinated;
            }
            // NOTE (#630, deliberately NOT fixed here): the temporal triple
            // (`effective_at`/`observed_at`/`expires_at`), `tier` and `modality` are
            // still dropped. Carrying them is only safe once the wire boundary
            // sanitizes them — `absorb_gate::CleanFields` covers four fields and
            // `provenance::canonical_mem` signs nothing temporal, so a peer would
            // otherwise gain ungated control of `expires_at` (floor any memory of
            // ours) and `tier` (pin memories in our store). That change lands WITH
            // the sanitization, not before it. See ADR-0051.
        } else {
            // Flat-medium path, unchanged.
            let wavefront_id = self.medium.add_wavefront(&memory.vector, memory.content.clone(), memory.amplitude)
                .map_err(|e| StoreError::Other(format!("Failed to add wavefront to medium: {e}")))?;

            self.medium.update_wavefront_id(&wavefront_id, id)
                .map_err(|e| StoreError::Other(format!("Failed to update wavefront ID: {e}")))?;

            if let Some(index) = self.medium.get_wavefront_index(&id) {
                self.medium.store.energy[index] = memory.amplitude;
                self.medium.store.frequency[index] = memory.frequency;
                self.medium.store.phase[index] = memory.phase;
                self.medium.store.timestamps[index] = memory.created_at.timestamp_millis();
                self.medium.store.metadata[index].created_at = memory.created_at;
                self.medium.store.metadata[index].hallucinated = memory.hallucinated;
            }
        }

        // Add to cache
        self.memory_cache.insert(id, memory);
        self.mark_dirty();

        Ok(id)
    }

    fn hrm_path(&self) -> Option<&std::path::Path> {
        Some(&self.hrm_path)
    }

    fn get(&self, id: &Uuid) -> Result<Option<&HyperMemory>, StoreError> {
        Ok(self.memory_cache.get(id))
    }

    fn get_mut(&mut self, id: &Uuid) -> Result<Option<&mut HyperMemory>, StoreError> {
        if self.memory_cache.contains_key(id) {
            self.mark_dirty(); // Mark dirty when getting mutable reference
        }
        Ok(self.memory_cache.get_mut(id))
    }

    fn search(&self, query: &[f32], top_k: usize) -> Result<Vec<(Uuid, f32)>, StoreError> {
        // #498: score over the LIVE `memory_cache` (the authoritative set `get`
        // reads), NOT the flat `medium.store` view. The flat view retains dropped
        // hallucinations / stale energies after a chiral rebuild (only
        // `sync_medium_from_chiral` refreshes it), so scoring it let dead ids rank
        // in and silently consume the top_k budget (callers then skip them via
        // `get().ok().flatten()`, shrinking the real neighbour set). Use NORMALIZED
        // cosine similarity — like `TestMedium::search` — so a similarity threshold
        // in `stage_detect` / `stage_wire_topk` means the same thing on both
        // backends instead of scaling with raw hypervector norm.
        let mut scores: Vec<(Uuid, f32)> = self
            .memory_cache
            .values()
            .map(|m| (m.id, crate::wave::cosine_similarity(query, &m.vector)))
            .collect();
        scores.sort_by(|a, b| b.1.total_cmp(&a.1));
        scores.truncate(top_k);
        Ok(scores)
    }



    fn all_memories(&self) -> Result<Vec<&HyperMemory>, StoreError> {
        Ok(self.memory_cache.values().collect())
    }

    fn all_ids(&self) -> Result<Vec<Uuid>, StoreError> {
        Ok(self.memory_cache.keys().copied().collect())
    }

    fn delete(&mut self, id: &Uuid) -> Result<bool, StoreError> {
        if self.memory_cache.remove(id).is_some() {
            // Remove from flat medium (best-effort).
            if let Err(e) = self.medium.remove_wavefront(id) {
                // Log error but don't fail - cache was already updated
                eprintln!("Warning: Failed to remove wavefront from medium: {e}");
            }

            // Remove from the chiral hemispheres if present. Without this the
            // .hrm file (which serializes from `self.chiral`, not `self.medium`)
            // re-hydrates the supposedly-forgotten wavefronts on the next load —
            // making delete() a no-op as far as persistence is concerned.
            if let Some(chiral) = &mut self.chiral {
                chiral.right.remove_wavefront(id);
                if let Some(&left_id) = chiral.right_to_left.get(id) {
                    chiral.left.remove_wavefront(&left_id);
                    chiral.left_to_right.remove(&left_id);
                }
                chiral.right_to_left.remove(id);
                chiral.scales.remove(id);
            }

            self.mark_dirty();
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn count(&self) -> usize {
        self.memory_cache.len()
    }

    fn flush(&mut self) -> Result<usize, StoreError> {
        self.save_medium()?;
        Ok(self.count())
    }

    fn consciousness_metrics(&self) -> crate::consciousness::ConsciousnessMetrics {
        // Use the public method which handles chiral vs flat medium
        Self::consciousness_metrics(self)
    }

    fn try_cached_consciousness_metrics(&self) -> Option<crate::consciousness::ConsciousnessMetrics> {
        Self::try_cached_consciousness_metrics(self)
    }

    fn set_cached_total_skip_links(&self, n: usize) {
        self.medium.set_cached_total_skip_links(n);
    }

    fn set_cached_num_clusters(&self, n: usize) {
        self.medium.set_cached_num_clusters(n);
    }

    fn dream_native(
        &mut self,
        cycles: usize,
        temperature: Option<f32>,
        chiral_eta: f32,
    ) -> Result<crate::medium::types::DreamReport, StoreError> {
        Ok(Self::dream_native(self, cycles, temperature, chiral_eta))
    }

    fn callosal_kuramoto(&mut self, dt: f32) {
        Self::callosal_kuramoto(self, dt);
    }

    fn chiral_dream(&mut self, deep: bool, cycles: usize) {
        Self::chiral_dream(self, deep, cycles);
    }

    fn flush_reactivation(&self) {
        Self::flush_reactivation(self);
    }

    fn recall_resonance_readonly(
        &self,
        query: &str,
        top_k: usize,
    ) -> Result<Vec<Resonance>, StoreError> {
        Self::recall_resonance_readonly(self, query, top_k)
    }

    fn is_chiral(&self) -> bool {
        Self::is_chiral(self)
    }

    fn provenance_of(&self, id: &Uuid) -> Option<crate::entropy::Provenance> {
        if let Some(ref chiral) = self.chiral {
            chiral
                .right
                .id_to_index
                .get(id)
                .and_then(|&i| chiral.right.metadata.get(i))
                .and_then(|m| m.provenance.clone())
        } else {
            self.medium
                .store
                .id_to_index
                .get(id)
                .and_then(|&i| self.medium.store.metadata.get(i))
                .and_then(|m| m.provenance.clone())
        }
    }

    fn consolidate_resonance(&mut self, opts: &ConsolidateOpts) -> ConsolidateReport {
        // ADR-0036: Off skips, DryRun plans (read-only), Apply mutates (Phase 2).
        match opts.mode {
            ConsolidateMode::Off => {
                ConsolidateReport { mode: "off".to_string(), ..Default::default() }
            }
            ConsolidateMode::DryRun => Self::plan_consolidation(self, opts),
            ConsolidateMode::Apply => Self::apply_consolidation(self, opts),
        }
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn absorb(&mut self, content: &str, importance: f32, category: Option<&str>) -> Result<Uuid, StoreError> {
        if let Some(ref mut chiral) = self.chiral {
            // ADR-0049: mints atomic facets alongside the parent when
            // KANNAKA_FACET_DECOMPOSE is set. Flag unset (the default) makes this
            // exactly the previous `store_with_category` call. Returns the PARENT
            // id either way, so `remember`'s contract is unchanged.
            let id = chiral.store_with_facets(content, importance, &self.pipeline, category)
                .map_err(|e| StoreError::Other(format!("chiral store failed: {e}")))?;
            self.rebuild_cache().ok();
            self.mark_dirty();
            Ok(id)
        } else {
            let id = self.medium.store(content, importance, &self.pipeline)
                .map_err(|e| StoreError::Other(format!("store failed: {e}")))?;
            self.rebuild_cache().ok();
            self.mark_dirty();
            Ok(id)
        }
    }

    fn resonate_query(&mut self, query: &str, top_k: usize) -> Result<Vec<(Uuid, f32)>, StoreError> {
        // Try cluster prefilter first (refactor #4) — only on the flat
        // (non-chiral) path for now. Loads the .clusters.json sidecar
        // written by bridge::assess, picks members of clusters whose
        // theme_vector resonates with the query, and runs the existing
        // recall_against seam against just those indices. Falls through
        // to the full medium scan if no clusters are loaded (fresh HRM)
        // or if no cluster's theme is close enough to the query.
        let prefilter_on = std::env::var("KANNAKA_RECALL_PREFILTER")
            .map(|v| !matches!(v.as_str(), "off" | "0" | "false"))
            .unwrap_or(true);
        if self.chiral.is_none() && prefilter_on {
            if let Some(candidates) = self.cluster_prefilter_candidates(query) {
                if !candidates.is_empty() {
                    let resonances = self.medium.recall_against(
                        Some(&candidates), query, top_k, &self.pipeline,
                    ).map_err(|e| StoreError::Other(format!("prefiltered recall failed: {e}")))?;
                    self.apply_observation(&resonances);
                    return Ok(resonances.iter().map(|r| (r.id, r.resonance_strength)).collect());
                }
            }
        }

        if std::env::var("KANNAKA_RECALL_TRACE").is_ok() {
            eprintln!("[recall-trace] resonate_query top_k={} path={}",
                top_k, if self.chiral.is_some() { "chiral" } else { "flat" });
        }
        if let Some(ref chiral) = self.chiral {
            // Chiral bilateral resonance — TODO: fold cluster prefilter
            // into the chiral path; for v1 chiral users skip the prefilter
            // and get the existing bilateral observation flow.
            let results = chiral.recall(query, top_k, &self.pipeline)
                .map_err(|e| StoreError::Other(format!("chiral recall failed: {e}")))?;

            // Observation: recall reshapes the field — attention IS computation.
            // Batched: one field-settle pass per recall, not per result.
            let observations: Vec<(usize, f32)> = results.iter().enumerate()
                .filter_map(|(i, r)| {
                    self.medium.get_wavefront_index(&r.id).map(|index| {
                        let ranking_factor = 1.0 - (i as f32 / results.len().max(1) as f32);
                        let intensity = r.resonance_strength.abs().min(1.0).max(0.1) * ranking_factor;
                        (index, intensity)
                    })
                })
                .collect();
            self.medium.observe_wavefronts(&observations);
            self.mark_dirty();

            Ok(results.iter().map(|r| (r.id, r.resonance_strength)).collect())
        } else {
            // Flat medium: resonance recall (always with observation)
            let results = self.recall_resonance(query, top_k)?;
            Ok(results.iter().map(|r| (r.id, r.resonance_strength)).collect())
        }
    }

    fn resonate_query_with_beam(
        &mut self,
        beam: &[Uuid],
        query: &str,
        top_k: usize,
    ) -> Result<Vec<(Uuid, f32)>, StoreError> {
        // Sparse-attention path: score only the memories in `beam`. Chiral
        // bilateral observation is bypassed for v1 — the sparse path runs
        // against the medium directly. Future fold-in: a chiral beam-aware
        // recall once both hemispheres are addressed by an attention beam.
        let results = self.recall_resonance_with_beam(beam, query, top_k)?;
        Ok(results.iter().map(|r| (r.id, r.resonance_strength)).collect())
    }

    fn relate(&mut self, id_a: &Uuid, id_b: &Uuid) -> Result<Uuid, StoreError> {
        self.relate_wavefronts(*id_a, *id_b)
    }
}

impl Drop for HrmStore {
    fn drop(&mut self) {
        // Auto-save on drop if dirty
        if self.dirty {
            if let Err(e) = self.save_medium() {
                eprintln!("Warning: Failed to auto-save HRM store on drop: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codebook::Codebook;
    use crate::encoding::{SimpleHashEncoder, EncodingPipeline};
    use crate::memory::HyperMemory;
    use crate::medium::WAVEFRONT_DIM;
    use tempfile::NamedTempFile;

    fn make_test_pipeline() -> EncodingPipeline {
        let encoder = SimpleHashEncoder::new(384, 42);
        let codebook = Codebook::new(384, WAVEFRONT_DIM, 42);
        EncodingPipeline::new(Box::new(encoder), codebook)
    }

    #[test]
    fn backfill_all_facets_migrates_then_is_idempotent() {
        let _flag = crate::facet::lock_decompose_flag();
        // #836 regression: write-time decomposition never touches memories
        // stored before the flag existed. The sweep must (a) count correctly
        // in dry-run without mutating, (b) mint facets on --apply, and
        // (c) mint NOTHING on a second apply -- `decomposed` is the watermark.
        std::env::remove_var("KANNAKA_FACET_DECOMPOSE"); // pre-facet corpus

        const COMPOUND_A: &str = "The harbor beacon channel moved to twentyseven last spring. \
            The lighthouse keeper still logs every crossing by hand. \
            Gulls avoid the eastern pier when the foghorn is running.";
        const COMPOUND_B: &str = "The northern trail floods after two days of rain. \
            The rangers close the gate before the water reaches the bridge.";
        const ATOMIC: &str = "hello harbor";

        // Self-validating fixture: if the qualifier ever stops splitting these,
        // fail HERE, not mysteriously in the counts below.
        assert!(
            crate::facet::decompose(COMPOUND_A).len() >= 2,
            "fixture A no longer decomposes"
        );
        assert!(
            crate::facet::decompose(COMPOUND_B).len() >= 2,
            "fixture B no longer decomposes"
        );
        assert!(crate::facet::decompose(ATOMIC).len() < 2, "fixture ATOMIC splits");

        let temp = NamedTempFile::new().unwrap();
        let mut store = HrmStore::new(make_test_pipeline(), temp.path().to_path_buf());
        store.upgrade_to_chiral();
        let p = make_test_pipeline();
        for content in [COMPOUND_A, COMPOUND_B, ATOMIC] {
            let vector = p.encode_text(content).unwrap();
            store.insert(HyperMemory::new(vector, content.to_string())).unwrap();
        }
        let rows_before = store.chiral_medium().unwrap().right.count();

        // (a) dry run: counts, no mutation.
        let dry = store.backfill_all_facets(false);
        assert_eq!(dry.parents_decomposed, 2, "two compound parents: {dry:?}");
        assert!(dry.facets_minted >= 4, "each parent >=2 facets: {dry:?}");
        assert_eq!(dry.atomic, 1, "one atomic row: {dry:?}");
        assert_eq!(
            store.chiral_medium().unwrap().right.count(),
            rows_before,
            "dry run must not mutate"
        );

        // (b) apply: mints exactly what the dry run promised.
        let applied = store.backfill_all_facets(true);
        assert_eq!(applied.parents_decomposed, 2, "{applied:?}");
        assert_eq!(
            applied.facets_minted, dry.facets_minted,
            "apply must mint what dry-run counted"
        );
        assert_eq!(
            store.chiral_medium().unwrap().right.count(),
            rows_before + applied.facets_minted,
            "row count grows by exactly the minted facets"
        );
        let facet_rows = store
            .chiral_medium()
            .unwrap()
            .right
            .metadata
            .iter()
            .filter(|m| m.is_facet)
            .count();
        assert_eq!(facet_rows, applied.facets_minted, "facet flags on the minted rows");

        // (c) idempotent: a second apply mints nothing.
        let again = store.backfill_all_facets(true);
        assert_eq!(again.parents_decomposed, 0, "watermark must hold: {again:?}");
        assert_eq!(again.facets_minted, 0, "{again:?}");
        assert_eq!(again.already_decomposed, 2, "{again:?}");
        assert_eq!(
            store.chiral_medium().unwrap().right.count(),
            rows_before + applied.facets_minted,
            "second apply must not grow the store"
        );
    }

    #[test]
    fn hrm_store_basic_operations() {
        let pipeline = make_test_pipeline();
        let temp_file = NamedTempFile::new().unwrap();
        let mut store = HrmStore::new(pipeline, temp_file.path().to_path_buf());

        // Test insert
        let memory = HyperMemory::new(vec![0.5; WAVEFRONT_DIM], "test content".to_string());
        let id = memory.id;
        let result = store.insert(memory).unwrap();
        assert_eq!(result, id);
        assert_eq!(store.count(), 1);

        // Test get
        let retrieved = store.get(&id).unwrap();
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().content, "test content");

        // Test search
        let query = vec![0.5; WAVEFRONT_DIM];
        let results = store.search(&query, 5).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, id);

        // Test delete
        let deleted = store.delete(&id).unwrap();
        assert!(deleted);
        assert_eq!(store.count(), 0);
    }

    /// #497 (remaining half): layer promotions and incremental-consolidation
    /// stamps live only in the cache; rebuild_cache used to zero both,
    /// discarding stage_transfer's work and defeating
    /// consolidate_incremental's changed-since premise on every dream/absorb.
    #[test]
    fn rebuild_cache_preserves_layer_and_consolidation_stamps() {
        let temp_file = NamedTempFile::new().unwrap();
        let mut store = HrmStore::new(make_test_pipeline(), temp_file.path().to_path_buf());

        let memory = HyperMemory::new(vec![0.5; WAVEFRONT_DIM], "layered".to_string());
        let id = store.insert(memory).unwrap();

        let stamp = chrono::Utc::now();
        {
            let mem = store.get_mut(&id).unwrap().unwrap();
            mem.layer_depth = 3;
            mem.last_consolidated_at = Some(stamp);
        }
        store.rebuild_cache().unwrap();

        let mem = store.get(&id).unwrap().unwrap();
        assert_eq!(mem.layer_depth, 3, "stage_transfer promotion must survive rebuild");
        assert_eq!(
            mem.last_consolidated_at,
            Some(stamp),
            "consolidate_incremental stamp must survive rebuild"
        );
    }

    // #498: search scores the LIVE cache with NORMALIZED cosine, not raw dot
    // products over the (possibly stale) flat medium view.
    #[test]
    fn search_normalizes_and_excludes_non_live_ids() {
        let temp = NamedTempFile::new().unwrap();
        let mut store = HrmStore::new(make_test_pipeline(), temp.path().to_path_buf());
        // Non-unit vectors so cosine (≈1.0 self-match) is distinguishable from a
        // raw dot product (which would scale with the vector norm, ≈0.25·DIM).
        let va = vec![0.5f32; WAVEFRONT_DIM];
        let mut vb = vec![0.0f32; WAVEFRONT_DIM];
        for x in vb.iter_mut().take(WAVEFRONT_DIM / 2) {
            *x = 1.0;
        }
        let ida = store.insert(HyperMemory::new(va.clone(), "alpha".into())).unwrap();
        let idb = store.insert(HyperMemory::new(vb.clone(), "beta".into())).unwrap();

        // Normalized: querying with alpha's own vector self-matches at ~1.0.
        let hits = store.search(&va, 5).unwrap();
        let top = hits.first().expect("a hit");
        assert_eq!(top.0, ida, "alpha ranks first for its own vector");
        assert!(
            (top.1 - 1.0).abs() < 1e-3,
            "cosine self-match must be ~1.0 (a raw dot would be ~0.25·DIM), got {}",
            top.1
        );

        // Dead-id exclusion: simulate the post-chiral-rebuild staleness where an
        // id lingers in the flat medium view but is dropped from the live cache.
        store.memory_cache.remove(&idb);
        let hits2 = store.search(&vb, 5).unwrap();
        assert!(
            hits2.iter().all(|(id, _)| *id != idb),
            "an id absent from the live cache must not appear (dead ids must not eat the k-budget)"
        );
    }

    // #497: rebuild_cache reconstructs memories from the medium (which has no
    // updated_at), so a GHOST — retrieval_count 0 but a real ADR-0037 ghosting
    // stamp — used to have its stamp reset to created_at and get hard-deleted past
    // the 7-day horizon. rebuild must now PRESERVE the ghost's stamp.
    #[test]
    fn rebuild_preserves_ghost_updated_at() {
        let temp = NamedTempFile::new().unwrap();
        let mut store = HrmStore::new(make_test_pipeline(), temp.path().to_path_buf());
        let id = store
            .insert(HyperMemory::new(vec![0.3f32; WAVEFRONT_DIM], "ghost".into()))
            .unwrap();

        // Ghost it: a distinct recent stamp, and NO recalls — the exact shape that
        // used to lose its stamp on rebuild.
        let ghost_time = Utc::now() - chrono::Duration::days(1);
        {
            let m = store.get_mut(&id).unwrap().unwrap();
            m.updated_at = Some(ghost_time);
            assert_eq!(m.retrieval_count, 0, "precondition: a ghost has no recalls");
            assert_ne!(m.updated_at, Some(m.created_at), "precondition: stamp != created_at");
        }

        store.rebuild_cache().unwrap();

        let m = store.get(&id).unwrap().expect("memory survives rebuild");
        assert_eq!(
            m.updated_at,
            Some(ghost_time),
            "#497: rebuild must preserve a ghost's ADR-0037 stamp, not reset it to created_at"
        );
    }

    // #107: recompute_encoding re-encodes chiral wavefronts in place. dry-run
    // reports the counts without writing; a real run rewrites the corrupted right
    // vector to match a fresh encoding of the same content.
    #[test]
    fn recompute_encoding_chiral_fixes_vectors() {
        let temp = NamedTempFile::new().unwrap();
        let mut store = HrmStore::new(make_test_pipeline(), temp.path().to_path_buf());
        store.upgrade_to_chiral();
        let content = "resonance and standing waves";
        // Seed with broken-encoder junk (the all-negative corner #106 produced).
        let bad = vec![-0.9f32; WAVEFRONT_DIM];
        let id = store.insert_raw_wavefront(bad, content.into(), 0.7).unwrap();

        // What a correct encoding of this content lands as in a fresh right slot.
        let want = {
            let tmp2 = NamedTempFile::new().unwrap();
            let mut refs = HrmStore::new(make_test_pipeline(), tmp2.path().to_path_buf());
            refs.upgrade_to_chiral();
            let good = make_test_pipeline().encode_text(content).unwrap();
            let rid = refs.insert_raw_wavefront(good, content.into(), 0.7).unwrap();
            let cm = refs.chiral_medium().unwrap();
            let ridx = *cm.right.id_to_index.get(&rid).unwrap();
            cm.right.wavefronts.row(ridx).to_vec()
        };

        // dry-run: counts only, nothing written.
        assert_eq!(store.recompute_encoding(true).unwrap(), (1, 1));

        // real run fixes the corrupted vector.
        assert_eq!(store.recompute_encoding(false).unwrap(), (1, 1));
        let cm = store.chiral_medium().unwrap();
        let idx = *cm.right.id_to_index.get(&id).unwrap();
        let got = cm.right.wavefronts.row(idx).to_vec();
        let cos = crate::wave::cosine_similarity(&got, &want);
        assert!(cos > 0.999, "recompute_encoding fixed the right vector (cos={cos})");
    }

    /// Issue #630 regression: `insert` must write the CHIRAL hemisphere.
    ///
    /// `save_medium` serializes `self.chiral` whenever it is present and never
    /// touches `self.medium`, and `rebuild_cache` reconstructs from
    /// `chiral.right.metadata`. `insert` used to write only the flat medium and the
    /// cache, so an inserted memory was absent from the `.hrm` and gone at the next
    /// load — silent data loss on the `import` and wire-sync paths, `import` being
    /// the documented recovery path for a corrupted store.
    ///
    /// There was no insert-then-reload test on a chiral store, which is why this
    /// survived. Before the fix this test fails at the `is_some()` assertion.
    #[test]
    fn insert_survives_save_and_reload_on_a_chiral_store() {
        const CONTENT: &str = "the harbor beacon channel is twentyseven";
        let temp = NamedTempFile::new().unwrap();
        let path = temp.path().to_path_buf();

        let id = {
            let mut store = HrmStore::new(make_test_pipeline(), path.clone());
            store.upgrade_to_chiral();
            assert!(
                store.chiral_medium().is_some(),
                "fixture must be chiral or the bug cannot reproduce"
            );

            let vector = make_test_pipeline().encode_text(CONTENT).unwrap();
            let mut memory = HyperMemory::new(vector, CONTENT.to_string());
            memory.amplitude = 0.77;
            let id = memory.id;

            assert_eq!(
                store.insert(memory).unwrap(),
                id,
                "insert must preserve the caller's id — the cache keys on it"
            );

            // The id rewrite must leave the chiral bookkeeping coherent, not just
            // the metadata row: a half-rewrite orphans the scale and strands the
            // left echo.
            {
                let cm = store.chiral_medium().unwrap();
                assert!(
                    cm.right.id_to_index.contains_key(&id),
                    "right hemisphere must be keyed by the caller's id"
                );
                assert!(
                    cm.scales.contains_key(&id),
                    "chiral scale must follow the id rewrite"
                );
                if let Some(left_id) = cm.right_to_left.get(&id) {
                    assert_eq!(
                        cm.left_to_right.get(left_id),
                        Some(&id),
                        "both hemisphere cross-maps must agree after the rewrite"
                    );
                }
            }

            store.flush().unwrap();
            id
        };

        // Reload from disk — this is where the memory used to disappear.
        let reloaded = HrmStore::load(make_test_pipeline(), path).unwrap();
        let got = reloaded.get(&id).unwrap();
        assert!(
            got.is_some(),
            "#630: inserted memory must survive save+reload on a chiral store"
        );
        assert_eq!(got.unwrap().content, CONTENT);
    }

    #[test]
    fn hrm_store_persistence() {
        let pipeline1 = make_test_pipeline();
        let pipeline2 = make_test_pipeline();
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().to_path_buf();

        // Create and populate store
        {
            let mut store = HrmStore::new(pipeline1, path.clone());
            let memory = HyperMemory::new(vec![0.5; WAVEFRONT_DIM], "persistent content".to_string());
            let id = store.insert(memory).unwrap();
            store.flush().unwrap(); // Force save
            assert_eq!(store.count(), 1);
        }

        // Load store and verify
        {
            let store = HrmStore::load(pipeline2, path).unwrap();
            assert_eq!(store.count(), 1);
            let memories = store.all_memories().unwrap();
            assert_eq!(memories[0].content, "persistent content");
        }
    }

    // #496: in chiral mode the RIGHT hemisphere is the authoritative store that
    // chiral.save() persists and rebuild_cache reloads. sync_cache_to_medium used
    // to write dream mutations (energy strengthening) to the FLAT medium, which is
    // never saved in chiral mode — so the particle dream's constructive work
    // silently reverted on every reload while its prunes (applied to chiral.right)
    // stuck. sync must write to chiral.right so strengthening persists too.
    #[test]
    fn chiral_dream_strengthening_persists_across_reload() {
        let pipeline1 = make_test_pipeline();
        let pipeline2 = make_test_pipeline();
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().to_path_buf();

        let id = {
            let mut store = HrmStore::new(pipeline1, path.clone());
            store.upgrade_to_chiral();
            // Seed a wavefront into the right hemisphere at a known low energy.
            let id = store
                .insert_raw_wavefront(vec![0.3f32; WAVEFRONT_DIM], "dreamed".into(), 0.40)
                .unwrap();
            // A deep dream strengthens it (raises amplitude) by mutating the cache
            // via get_mut — exactly the path sync_cache_to_medium must persist.
            store.get_mut(&id).unwrap().unwrap().amplitude = 0.95;
            store.flush().unwrap();
            id
        };

        // Reload from disk: rebuild_cache reads chiral.right. If the mutation only
        // reached the (unsaved) flat medium, the reloaded energy is the original
        // 0.40 and this assertion fails — the #496 revert.
        let store = HrmStore::load(pipeline2, path).unwrap();
        let amp = store.get(&id).unwrap().expect("wavefront survives reload").amplitude;
        assert!(
            (amp - 0.95).abs() < 1e-4,
            "#496: chiral dream-strengthened energy must persist across reload, got {amp}"
        );
    }

    // #530 review regression: the chiral sync must persist ONLY energy, not
    // frequency/phase. insert_raw_wavefront sets chiral.right.frequency=1.0 (via
    // add_wavefront) but its cache record keeps WaveParams::default (0.1), so a
    // freq-syncing flush would clobber the authoritative 1.0 -> 0.1 on disk. The
    // strengthened energy must still persist.
    #[test]
    fn chiral_sync_preserves_raw_insert_frequency() {
        let temp = NamedTempFile::new().unwrap();
        let path = temp.path().to_path_buf();
        let id = {
            let mut store = HrmStore::new(make_test_pipeline(), path.clone());
            store.upgrade_to_chiral();
            let id = store
                .insert_raw_wavefront(vec![0.3f32; WAVEFRONT_DIM], "anchor".into(), 0.8)
                .unwrap();
            let cm = store.chiral_medium().unwrap();
            let idx = *cm.right.id_to_index.get(&id).unwrap();
            assert_eq!(cm.right.frequency[idx], 1.0, "precondition: add_wavefront set freq 1.0");
            // Strengthen energy (the #496 path), then flush.
            store.get_mut(&id).unwrap().unwrap().amplitude = 0.95;
            store.flush().unwrap();
            id
        };

        let store = HrmStore::load(make_test_pipeline(), path).unwrap();
        let amp = store.get(&id).unwrap().expect("survives reload").amplitude;
        assert!((amp - 0.95).abs() < 1e-4, "#496: strengthened energy persists, got {amp}");
        let cm = store.chiral_medium().unwrap();
        let idx = *cm.right.id_to_index.get(&id).unwrap();
        assert_eq!(
            cm.right.frequency[idx], 1.0,
            "#530: raw-insert frequency 1.0 must NOT be clobbered to the cache default 0.1"
        );
    }

    #[test]
    fn reactivation_persists_across_reload() {
        // ADR-0036 Phase 1: a recall-reactivation count must survive a reload
        // via the `.reactivation.json` sidecar (it is not in the .hrm binary).
        let pipeline1 = make_test_pipeline();
        let pipeline2 = make_test_pipeline();
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().to_path_buf();

        let id;
        {
            let mut store = HrmStore::new(pipeline1, path.clone());
            let memory = HyperMemory::new(vec![0.5; WAVEFRONT_DIM], "reactivated content".to_string());
            id = store.insert(memory).unwrap();
            // Ensure the cache holds the inserted memory, then simulate a recall
            // reactivation on it.
            store.rebuild_cache().unwrap();
            let m = store.memory_cache.get_mut(&id).expect("inserted memory in cache");
            m.retrieval_count = 5;
            m.updated_at = Some(chrono::Utc::now());
            store.mark_dirty();
            store.flush().unwrap(); // save_medium → save_reactivation writes the sidecar
        }

        // A fresh load must rehydrate the reactivation count from the sidecar.
        {
            let store = HrmStore::load(pipeline2, path).unwrap();
            let m = store.memory_cache.get(&id).expect("memory present after reload");
            assert_eq!(
                m.retrieval_count, 5,
                "reactivation count should survive reload via the sidecar"
            );
        }
    }

    #[test]
    fn reactivation_survives_rebuild_cache() {
        // rebuild_cache runs after every dream/absorb; it must preserve the
        // reactivation count instead of resetting it to 0 (ADR-0036 Phase 1).
        let pipeline = make_test_pipeline();
        let temp_file = NamedTempFile::new().unwrap();
        let mut store = HrmStore::new(pipeline, temp_file.path().to_path_buf());
        let memory = HyperMemory::new(vec![0.5; WAVEFRONT_DIM], "rebuild content".to_string());
        let id = store.insert(memory).unwrap();
        store.rebuild_cache().unwrap();
        store.memory_cache.get_mut(&id).unwrap().retrieval_count = 7;

        store.rebuild_cache().unwrap(); // would previously wipe the count to 0

        assert_eq!(store.memory_cache.get(&id).unwrap().retrieval_count, 7);
    }

    #[test]
    fn reactivation_sidecar_merges_max_no_clobber() {
        // Several processes touch the sidecar; a flush must MERGE (max), never
        // overwrite a higher count with a lower one (ADR-0036 Phase 1).
        fn sidecar_count(hrm_path: &std::path::Path, id: &uuid::Uuid) -> u32 {
            let p = hrm_path.with_extension("reactivation.json");
            let data = std::fs::read(&p).unwrap();
            let map: std::collections::HashMap<String, (u32, Option<DateTime<Utc>>)> =
                serde_json::from_slice(&data).unwrap();
            map.get(&id.to_string()).map(|e| e.0).unwrap_or(0)
        }

        let pipeline = make_test_pipeline();
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().to_path_buf();
        let mut store = HrmStore::new(pipeline, path.clone());
        let id = store
            .insert(HyperMemory::new(vec![0.5; WAVEFRONT_DIM], "m".to_string()))
            .unwrap();
        store.rebuild_cache().unwrap();

        store.memory_cache.get_mut(&id).unwrap().retrieval_count = 4;
        store.flush_reactivation();
        assert_eq!(sidecar_count(&path, &id), 4);

        // A lower count must not clobber the higher one.
        store.memory_cache.get_mut(&id).unwrap().retrieval_count = 1;
        store.flush_reactivation();
        assert_eq!(sidecar_count(&path, &id), 4, "merge=max: lower must not win");

        // A higher count advances it.
        store.memory_cache.get_mut(&id).unwrap().retrieval_count = 9;
        store.flush_reactivation();
        assert_eq!(sidecar_count(&path, &id), 9);
    }

    #[test]
    fn hrm_store_resonance_recall() {
        let pipeline = make_test_pipeline();
        let temp_file = NamedTempFile::new().unwrap();
        let mut store = HrmStore::new(pipeline, temp_file.path().to_path_buf());

        // Insert related memories
        let memory1 = HyperMemory::new(vec![0.5; WAVEFRONT_DIM], "cats are fluffy".to_string());
        let memory2 = HyperMemory::new(vec![0.4; WAVEFRONT_DIM], "dogs are loyal".to_string());
        
        store.insert(memory1).unwrap();
        store.insert(memory2).unwrap();

        // Test resonance-based recall
        let results = store.recall_resonance("pets and animals", 5).unwrap();
        assert!(results.len() > 0);
        
        // Results should be sorted by resonance strength
        if results.len() > 1 {
            assert!(results[0].resonance_strength >= results[1].resonance_strength);
        }
    }
    
    #[test]
    fn hrm_store_consciousness_metrics() {
        let pipeline = make_test_pipeline();
        let temp_file = NamedTempFile::new().unwrap();
        let mut store = HrmStore::new(pipeline, temp_file.path().to_path_buf());

        // Insert some memories for metrics
        let memory1 = HyperMemory::new(vec![0.5; WAVEFRONT_DIM], "first thought".to_string());
        let memory2 = HyperMemory::new(vec![0.4; WAVEFRONT_DIM], "second idea".to_string());
        
        store.insert(memory1).unwrap();
        store.insert(memory2).unwrap();

        // Get consciousness metrics
        let metrics = store.consciousness_metrics();
        
        assert!(metrics.phi >= 0.0 && metrics.phi <= 1.0);
        assert!(metrics.xi >= 0.0 && metrics.xi <= 1.0);
        assert!(metrics.order >= 0.0 && metrics.order <= 1.0);
        assert!(metrics.num_clusters > 0);
        
        println!("HRM Store consciousness: phi={}, xi={}, order={}, clusters={}, level={:?}", 
                metrics.phi, metrics.xi, metrics.order, metrics.num_clusters, metrics.level);
    }
    
    #[test]
    fn hrm_store_find_associated() {
        let pipeline = make_test_pipeline();
        let temp_file = NamedTempFile::new().unwrap();
        let mut store = HrmStore::new(pipeline, temp_file.path().to_path_buf());

        // Insert related memories
        let memory1 = HyperMemory::new(vec![0.8; WAVEFRONT_DIM], "similar content".to_string());
        let memory2 = HyperMemory::new(vec![0.7; WAVEFRONT_DIM], "related content".to_string());
        let memory3 = HyperMemory::new(vec![0.1; WAVEFRONT_DIM], "different content".to_string());
        
        let id1 = store.insert(memory1).unwrap();
        let id2 = store.insert(memory2).unwrap();
        let id3 = store.insert(memory3).unwrap();

        // Find associations for first memory
        let associations = store.find_associated(id1, 5);
        
        assert_eq!(associations.len(), 2);
        assert!(associations.iter().any(|(id, _)| *id == id2));
        assert!(associations.iter().any(|(id, _)| *id == id3));
        
        // Should be sorted by coherence strength
        assert!(associations[0].1 >= associations[1].1);
    }
    
    #[test]
    fn hrm_store_dream_cycles() {
        let pipeline = make_test_pipeline();
        let temp_file = NamedTempFile::new().unwrap();
        let mut store = HrmStore::new(pipeline, temp_file.path().to_path_buf());

        // Insert memories for dreaming
        for i in 0..5 {
            let memory = HyperMemory::new(vec![0.1 + i as f32 * 0.2; WAVEFRONT_DIM], 
                                        format!("dream memory {i}"));
            store.insert(memory).unwrap();
        }
        
        let _initial_count = store.count();
        let report = store.dream(3, Some(1.0));
        
        assert!(report.cycles_completed <= 3);
        assert!(report.energy_before >= 0.0);
        assert!(report.energy_after >= 0.0);
        assert!(report.final_temperature < 1.0);
        
        // Store should be marked dirty after dreaming
        assert!(store.dirty);

        println!("Dream report: {report:?}");
    }

    // -----------------------------------------------------------------------
    // ADR-0036 Phase 2 — destructive apply_consolidation safety gate.
    // -----------------------------------------------------------------------

    use crate::medium::types::{Tier, ConsolidateOpts, ConsolidateMode};

    /// Build a flat (non-chiral) store and insert a wavefront with full control
    /// over vector / energy / phase / tier directly in the authoritative store.
    /// Returns the assigned UUID. Caller should `rebuild_cache()` afterward.
    fn insert_ctl(
        store: &mut HrmStore,
        vector: Vec<f32>,
        content: &str,
        energy: f32,
        phase: f32,
        tier: Tier,
        retrieval_count: u32,
    ) -> Uuid {
        let id = store
            .medium
            .add_wavefront(&vector, content.to_string(), energy)
            .expect("add_wavefront");
        let idx = store.medium.get_wavefront_index(&id).unwrap();
        store.medium.store.energy[idx] = energy;
        store.medium.store.phase[idx] = phase;
        store.medium.store.metadata[idx].tier = tier;
        // rebuild_cache resets retrieval_count to 0 and then restores it from the
        // cache snapshot — so seed the cache entry's count before rebuild.
        store.rebuild_cache().unwrap();
        if retrieval_count > 0 {
            store.memory_cache.get_mut(&id).unwrap().retrieval_count = retrieval_count;
        }
        id
    }

    fn apply_opts() -> ConsolidateOpts {
        ConsolidateOpts {
            mode: ConsolidateMode::Apply,
            merge_sim: 0.92,
            merge_phase_cos: std::f32::consts::FRAC_1_SQRT_2,
            shortterm_evict: 0.15,
            ..Default::default()
        }
    }

    #[test]
    fn apply_holographic_preservation() {
        // N near-duplicate, phase-locked memories + some unrelated ones.
        let mut store = HrmStore::new(make_test_pipeline(), NamedTempFile::new().unwrap().path().to_path_buf());

        // Cluster: 3 near-identical vectors (cos ~ 1.0), phase 0 (locked).
        let base = vec![0.5f32; WAVEFRONT_DIM];
        let mut dup_ids = Vec::new();
        for k in 0..3 {
            let mut v = base.clone();
            v[k] += 0.001; // tiny perturbation keeps cos ≥ 0.95
            dup_ids.push(insert_ctl(&mut store, v, &format!("dup {k}"), 1.0, 0.0, Tier::LongTerm, 0));
        }
        // Two unrelated, orthogonal-ish vectors.
        let mut u1 = vec![0.0f32; WAVEFRONT_DIM]; u1[100] = 1.0;
        let mut u2 = vec![0.0f32; WAVEFRONT_DIM]; u2[200] = 1.0;
        let uid1 = insert_ctl(&mut store, u1, "unrelated 1", 1.0, 0.0, Tier::LongTerm, 0);
        let uid2 = insert_ctl(&mut store, u2, "unrelated 2", 1.0, 0.0, Tier::LongTerm, 0);

        let before = store.count();
        assert_eq!(before, 5);

        let report = store.apply_consolidation(&apply_opts());
        assert_eq!(report.mode, "apply");
        assert!(report.applied);
        assert_eq!(report.groups_found, 1, "the 3 dups form exactly one group");
        assert_eq!(report.would_absorb, 2, "3-1 carriers absorbed");

        let after = store.count();
        // (a) total decreased by exactly #absorbed.
        assert_eq!(after, before - report.would_absorb);
        // (c) count NEVER increases.
        assert!(after <= before);

        // Exactly one of the 3 dup ids survives (the carrier); unrelated kept.
        let surviving_dups = dup_ids.iter().filter(|id| store.get(id).unwrap().is_some()).count();
        assert_eq!(surviving_dups, 1, "one carrier remains for the dup group");
        assert!(store.get(&uid1).unwrap().is_some());
        assert!(store.get(&uid2).unwrap().is_some());

        // Carrier energy is the superposition of 3 in-phase unit-ish energies,
        // clamped to the 2.0 ceiling: sqrt(3 + 2*3) = 3.0 -> clamp 2.0.
        let carrier_id = *dup_ids.iter().find(|id| store.get(id).unwrap().is_some()).unwrap();
        let carrier_e = store.get(&carrier_id).unwrap().unwrap().amplitude;
        assert!((carrier_e - 2.0).abs() < 1e-4, "in-phase merge clamps to ceiling, got {carrier_e}");

        // (b) recalling an ABSORBED memory's content still returns a result above
        // threshold (the carrier represents the absorbed content).
        let absorbed_id = *dup_ids.iter().find(|id| store.get(id).unwrap().is_none()).unwrap();
        // Search by the absorbed vector itself — the carrier (near-identical) must
        // dominate the search results.
        let q = vec![0.5f32; WAVEFRONT_DIM];
        let hits = store.search(&q, 5).unwrap();
        assert!(!hits.is_empty(), "search returns the carrier for absorbed content");
        assert_eq!(hits[0].0, carrier_id, "carrier is the top match for the merged content");
        assert!(store.get(&absorbed_id).unwrap().is_none(), "absorbed id is gone");
    }

    #[test]
    fn apply_merge_rehomes_connections_onto_carrier() {
        // A merge must conserve the skip-link graph, not just energy: a link INTO
        // an absorbed member is redirected to the carrier, and an absorbed
        // member's link OUT is inherited by the carrier. Regression for the
        // dropped/dangling-connection gap on the ADR-0036 destructive path.
        let mut store =
            HrmStore::new(make_test_pipeline(), NamedTempFile::new().unwrap().path().to_path_buf());

        // Merge group of 3 near-identical, phase-locked dups. dup[0] is strongest
        // so it is deterministically the carrier; dup[1]/dup[2] are absorbed.
        let base = vec![0.5f32; WAVEFRONT_DIM];
        let mut dup_ids = Vec::new();
        for k in 0..3 {
            let mut v = base.clone();
            v[k] += 0.001;
            let energy = if k == 0 { 2.0 } else { 1.0 };
            dup_ids.push(insert_ctl(&mut store, v, &format!("dup {k}"), energy, 0.0, Tier::LongTerm, 0));
        }
        // An unrelated survivor to link to/from.
        let mut u = vec![0.0f32; WAVEFRONT_DIM];
        u[100] = 1.0;
        let other = insert_ctl(&mut store, u, "other", 1.0, 0.0, Tier::LongTerm, 0);

        let carrier = dup_ids[0];
        let absorbed_a = dup_ids[1];
        let absorbed_b = dup_ids[2];

        let link = |target: Uuid| crate::memory::LegacyLink {
            target_id: target,
            strength: 0.7,
            resonance_key: Vec::new(),
            span: 0,
        };
        // Inbound: other -> absorbed_a (must redirect to the carrier).
        store.memory_cache.get_mut(&other).unwrap().connections.push(link(absorbed_a));
        // Outbound: absorbed_b -> other (the carrier must inherit this).
        store.memory_cache.get_mut(&absorbed_b).unwrap().connections.push(link(other));

        let report = store.apply_consolidation(&apply_opts());
        assert_eq!(report.groups_found, 1);
        assert_eq!(report.would_absorb, 2);
        assert!(store.get(&carrier).unwrap().is_some(), "carrier survives");
        assert!(store.get(&absorbed_a).unwrap().is_none(), "absorbed_a removed");

        // Inbound link redirected: `other` points at the carrier, not a dead id.
        let other_conns = store.get(&other).unwrap().unwrap().connections.clone();
        assert!(
            other_conns.iter().any(|l| l.target_id == carrier),
            "inbound link must be redirected onto the carrier"
        );
        assert!(
            !other_conns.iter().any(|l| l.target_id == absorbed_a || l.target_id == absorbed_b),
            "no surviving link may point at an absorbed id"
        );

        // Outbound link inherited: the carrier carries the absorbed member's
        // link to `other`.
        let carrier_conns = store.get(&carrier).unwrap().unwrap().connections.clone();
        assert!(
            carrier_conns.iter().any(|l| l.target_id == other),
            "carrier must inherit the absorbed member's outbound link"
        );
    }

    #[test]
    fn apply_pinned_and_longterm_protected() {
        let mut store = HrmStore::new(make_test_pipeline(), NamedTempFile::new().unwrap().path().to_path_buf());

        // A Pinned duplicate pair — Pinned must never be grouped/removed.
        let base = vec![0.5f32; WAVEFRONT_DIM];
        let pin1 = insert_ctl(&mut store, base.clone(), "pin 1", 1.0, 0.0, Tier::Pinned, 0);
        let mut p2 = base.clone(); p2[0] += 0.001;
        let pin2 = insert_ctl(&mut store, p2, "pin 2", 1.0, 0.0, Tier::Pinned, 0);

        // A mixed LongTerm+ShortTerm duplicate group — carrier must end LongTerm.
        let mut lt = base.clone(); lt[5000] += 0.0005;
        let mut st = base.clone(); st[5000] += 0.0006;
        let lt_id = insert_ctl(&mut store, lt, "long", 0.9, 0.0, Tier::LongTerm, 0);
        let st_id = insert_ctl(&mut store, st, "short", 1.5, 0.0, Tier::ShortTerm, 0);

        let report = store.apply_consolidation(&apply_opts());

        // Pinned never removed.
        assert!(store.get(&pin1).unwrap().is_some());
        assert!(store.get(&pin2).unwrap().is_some());

        // The LongTerm+ShortTerm pair merges into one carrier. Even though the
        // ShortTerm member has the higher effective strength (1.5 vs 0.9) and is
        // thus the representative, the carrier tier must be the STRONGEST tier
        // present = LongTerm, so the LongTerm is never absorbed into a lower tier.
        let lt_alive = store.get(&lt_id).unwrap().is_some();
        let st_alive = store.get(&st_id).unwrap().is_some();
        assert!(lt_alive ^ st_alive, "exactly one of the lt/st pair survives as carrier");
        let carrier = if lt_alive { lt_id } else { st_id };
        assert_eq!(
            store.get(&carrier).unwrap().unwrap().tier,
            Tier::LongTerm,
            "carrier inherits the strongest tier (LongTerm) in a mixed group"
        );
        // Note: pinned pair grouped? No — pinned excluded, so groups_found counts
        // only the lt/st group.
        assert_eq!(report.groups_found, 1);
    }

    #[test]
    fn apply_longterm_never_evicted() {
        // A low-energy LongTerm with no reactivation must NOT be evicted (only
        // ShortTerm is eviction-eligible).
        let mut store = HrmStore::new(make_test_pipeline(), NamedTempFile::new().unwrap().path().to_path_buf());
        let mut v = vec![0.0f32; WAVEFRONT_DIM]; v[42] = 1.0;
        let lt = insert_ctl(&mut store, v, "weak long", 0.01, 0.0, Tier::LongTerm, 0);
        let report = store.apply_consolidation(&apply_opts());
        assert!(store.get(&lt).unwrap().is_some(), "LongTerm is never evicted");
        assert_eq!(report.would_evict, 0);
    }

    #[test]
    fn apply_shortterm_evict_respects_reactivation() {
        let mut store = HrmStore::new(make_test_pipeline(), NamedTempFile::new().unwrap().path().to_path_buf());
        // Below threshold, unreactivated -> evicted.
        let mut v1 = vec![0.0f32; WAVEFRONT_DIM]; v1[10] = 1.0;
        let evicted = insert_ctl(&mut store, v1, "cold short", 0.05, 0.0, Tier::ShortTerm, 0);
        // Below threshold but reactivated -> kept.
        let mut v2 = vec![0.0f32; WAVEFRONT_DIM]; v2[20] = 1.0;
        let kept = insert_ctl(&mut store, v2, "warm short", 0.05, 0.0, Tier::ShortTerm, 3);

        let report = store.apply_consolidation(&apply_opts());
        assert!(store.get(&evicted).unwrap().is_none(), "cold ShortTerm evicted");
        assert!(store.get(&kept).unwrap().is_some(), "reactivated ShortTerm kept");
        assert_eq!(report.would_evict, 1);
    }

    #[test]
    fn apply_is_idempotent_second_run_absorbs_zero() {
        let mut store = HrmStore::new(make_test_pipeline(), NamedTempFile::new().unwrap().path().to_path_buf());
        let base = vec![0.5f32; WAVEFRONT_DIM];
        for k in 0..4 {
            let mut v = base.clone(); v[k] += 0.001;
            insert_ctl(&mut store, v, &format!("d{k}"), 1.0, 0.0, Tier::LongTerm, 0);
        }
        let r1 = store.apply_consolidation(&apply_opts());
        assert_eq!(r1.would_absorb, 3, "4 dups -> 3 absorbed");
        let count_after_first = store.count();

        let r2 = store.apply_consolidation(&apply_opts());
        assert_eq!(r2.would_absorb, 0, "already merged: nothing left to absorb");
        assert_eq!(store.count(), count_after_first, "second run does not change count");
    }

    #[test]
    fn apply_chiral_map_consistency() {
        // Construct a ChiralMedium with right wavefronts, some of which have left
        // twins, then apply and assert no map points at a removed id and every
        // surviving twin still resolves to a live left.
        use crate::medium::chiral::ChiralMedium;
        use crate::medium::hemisphere::Hemisphere;
        use crate::medium::types::Hand;

        let mut store = HrmStore::new(make_test_pipeline(), NamedTempFile::new().unwrap().path().to_path_buf());
        let dims = WAVEFRONT_DIM;
        let mut chiral = ChiralMedium::new();
        // Replace hemispheres with WAVEFRONT_DIM-sized ones so vectors line up.
        chiral.left = Hemisphere::new(Hand::Left, dims);
        chiral.right = Hemisphere::new(Hand::Right, dims);

        let base = vec![0.5f32; dims];
        // 3 near-duplicate right wavefronts (one group); give 2 of them left twins.
        let mut right_ids = Vec::new();
        for k in 0..3 {
            let mut v = base.clone(); v[k] += 0.001;
            let rid = chiral.right.add_wavefront(&v, format!("r{k}"), 1.0).unwrap();
            let ridx = *chiral.right.id_to_index.get(&rid).unwrap();
            chiral.right.metadata[ridx].tier = Tier::LongTerm;
            right_ids.push(rid);
            if k < 2 {
                // left twin
                let mut lv = vec![0.1f32; dims]; lv[k] += 0.01;
                let lid = chiral.left.add_wavefront(&lv, format!("l{k}"), 0.8).unwrap();
                chiral.left_to_right.insert(lid, rid);
                chiral.right_to_left.insert(rid, lid);
                chiral.scales.insert(rid, crate::medium::types::ChiralScale::deep_memory());
            }
        }
        // One unrelated right wavefront with a twin (must survive + keep its twin).
        let mut uv = vec![0.0f32; dims]; uv[500] = 1.0;
        let urid = chiral.right.add_wavefront(&uv, "unrelated".to_string(), 1.0).unwrap();
        let uridx = *chiral.right.id_to_index.get(&urid).unwrap();
        chiral.right.metadata[uridx].tier = Tier::LongTerm;
        let mut ulv = vec![0.2f32; dims];
        let ulid = chiral.left.add_wavefront(&ulv_fix(&mut ulv), "unrelated-l".to_string(), 0.8).unwrap();
        chiral.left_to_right.insert(ulid, urid);
        chiral.right_to_left.insert(urid, ulid);

        store.chiral = Some(chiral);
        store.rebuild_cache().unwrap();

        let right_before = store.chiral.as_ref().unwrap().right.count();
        let left_before = store.chiral.as_ref().unwrap().left.count();
        assert_eq!(right_before, 4);
        assert_eq!(left_before, 3);

        let report = store.apply_consolidation(&apply_opts());
        assert_eq!(report.groups_found, 1);
        assert_eq!(report.would_absorb, 2, "3 dups -> 2 absorbed in right hemisphere");

        let chiral = store.chiral.as_ref().unwrap();
        // Right shrank by 2.
        assert_eq!(chiral.right.count(), right_before - 2);

        // No right_to_left entry points at a removed right or a removed left.
        for (rid, lid) in &chiral.right_to_left {
            assert!(chiral.right.id_to_index.contains_key(rid), "right_to_left key {rid} is dead");
            assert!(chiral.left.id_to_index.contains_key(lid), "right_to_left value (left) {lid} is dead");
        }
        // No left_to_right entry points at a removed left or right.
        for (lid, rid) in &chiral.left_to_right {
            assert!(chiral.left.id_to_index.contains_key(lid), "left_to_right key {lid} is dead");
            assert!(chiral.right.id_to_index.contains_key(rid), "left_to_right value (right) {rid} is dead");
        }
        // Every surviving right twin resolves to a live left.
        for rid in chiral.right.id_to_index.keys() {
            if let Some(lid) = chiral.right_to_left.get(rid) {
                assert!(chiral.left.id_to_index.contains_key(lid));
            }
        }
        // The unrelated right + its twin both survive.
        assert!(chiral.right.id_to_index.contains_key(&urid));
        assert!(chiral.left.id_to_index.contains_key(&ulid));
        // Scales for removed right ids are cleaned up.
        for rid in chiral.scales.keys() {
            assert!(chiral.right.id_to_index.contains_key(rid), "scale for dead right {rid}");
        }
    }

    // Tiny helper to avoid an all-zeros left vector tripping the norm<eps guard
    // in unrelated paths (purely cosmetic for the chiral test fixture).
    fn ulv_fix(v: &mut [f32]) -> &[f32] {
        v[0] = 0.2;
        v
    }

    // -----------------------------------------------------------------------
    // ADR-0036 belief-safety — mean-centered semantic gate, per-pass absorb
    // cap, and dry-run/apply parity. These exercise the shared grouping pass
    // (`compute_merge_grouping`) directly with `belief` as a parameter, so they
    // never touch the process-global `KANNAKA_BELIEF_PHASE` env (no flakiness
    // from parallel tests), plus one end-to-end apply for the cap.
    // -----------------------------------------------------------------------

    /// The failure class v0.7.3 gated. Under belief the phase field is content-
    /// derived, so semantically-DISTINCT-but-anisotropic memories (a large shared
    /// component + small distinct residuals) share BOTH an inflated raw cosine and
    /// a manufactured phase coherence — and the old raw-cosine grouping chained
    /// them into one blob (the live 295→82). The mean-centered belief gate must
    /// refuse to group them, while the raw gate (belief off) still would.
    #[test]
    fn belief_centered_gate_rejects_anisotropic_distinct() {
        let dim = 64usize;
        // Shared anisotropic base dominates; each memory adds a distinct residual.
        // Raw cosine ≈ 0.9996 (base dominates); centered cosine < 0 (residuals
        // point away from each other once the shared mean is removed).
        let base = vec![1.0f32; dim];
        let n = 6usize;
        let owned: Vec<Vec<f32>> = (0..n)
            .map(|k| {
                let mut v = base.clone();
                v[k] += 0.15;
                v
            })
            .collect();
        let vectors: Vec<&[f32]> = owned.iter().map(|v| v.as_slice()).collect();
        // Manufactured phase coherence: all phases identical, as the belief born-
        // phase yields when its 2-D projection collapses on an anisotropic field.
        let phases = vec![0.0f32; n];
        let tiers = vec![Tier::LongTerm; n];
        let energies = vec![1.0f32; n];
        let opts = ConsolidateOpts { mode: ConsolidateMode::Apply, ..Default::default() };

        // belief OFF: raw cosine is inflated by the shared base → they group.
        let raw = compute_merge_grouping(&vectors, &phases, &tiers, &energies, &opts, false);
        assert!(raw.groups_before_cap >= 1, "raw cosine groups the anisotropic blob");
        assert!(raw.absorb_before_cap >= 1);
        assert!(!raw.centered);

        // belief ON: centered cosine sees only the distinct residuals → no group.
        let safe = compute_merge_grouping(&vectors, &phases, &tiers, &energies, &opts, true);
        assert_eq!(
            safe.groups_before_cap, 0,
            "centered gate must not group semantically-distinct memories under belief"
        );
        assert_eq!(safe.admitted.len(), 0);
        assert!(safe.centered);
    }

    /// Genuinely-redundant memories (identical residuals) still merge under the
    /// centered belief gate — the fix must not neuter consolidation entirely.
    #[test]
    fn belief_centered_gate_still_merges_true_duplicates() {
        let dim = 64usize;
        // Two genuine duplicates (identical) + two unrelated one-hots. Even after
        // centering, the duplicates' residuals coincide → they group.
        let mut d1 = vec![0.2f32; dim]; d1[0] = 1.0;
        let d2 = d1.clone();
        let mut u1 = vec![0.0f32; dim]; u1[20] = 1.0;
        let mut u2 = vec![0.0f32; dim]; u2[40] = 1.0;
        let owned = vec![d1, d2, u1, u2];
        let vectors: Vec<&[f32]> = owned.iter().map(|v| v.as_slice()).collect();
        let phases = vec![0.0f32; 4];
        let tiers = vec![Tier::LongTerm; 4];
        let energies = vec![1.0f32; 4];
        let opts = ConsolidateOpts { mode: ConsolidateMode::Apply, ..Default::default() };

        let safe = compute_merge_grouping(&vectors, &phases, &tiers, &energies, &opts, true);
        assert_eq!(safe.groups_before_cap, 1, "true duplicates still group under belief");
        assert_eq!(safe.absorb_before_cap, 1);
    }

    /// Cohesion ordering: when the per-pass cap admits fewer groups than found,
    /// the tightest (highest mean-cosine-to-carrier) group is admitted first.
    #[test]
    fn absorb_cap_admits_tightest_group_first() {
        let dim = 64usize;
        // Group A: two IDENTICAL vectors (cohesion 1.0). Group B: a looser pair
        // (cohesion ≈ 0.96). Orthogonal anchors so the groups never cross-merge.
        let mut a1 = vec![0.0f32; dim]; a1[0] = 1.0;
        let a2 = a1.clone();
        let mut b1 = vec![0.0f32; dim]; b1[32] = 1.0;
        let mut b2 = vec![0.0f32; dim]; b2[32] = 1.0; b2[33] = 0.30;
        let owned = vec![a1, a2, b1, b2];
        let vectors: Vec<&[f32]> = owned.iter().map(|v| v.as_slice()).collect();
        let phases = vec![0.0f32; 4];
        let tiers = vec![Tier::LongTerm; 4];
        let energies = vec![1.0f32; 4];
        // n=4, cap_n = floor(0.25*4) = 1 → only one group's single absorb fits.
        let opts = ConsolidateOpts {
            mode: ConsolidateMode::Apply,
            max_absorb_frac: Some(0.25),
            ..Default::default()
        };

        let g = compute_merge_grouping(&vectors, &phases, &tiers, &energies, &opts, false);
        assert_eq!(g.groups_before_cap, 2, "both pairs pass the raw sim gate");
        assert_eq!(g.absorb_before_cap, 2);
        assert_eq!(g.admitted.len(), 1, "cap admits exactly one group");
        assert!(g.capped());
        let admitted: std::collections::HashSet<usize> = g.admitted[0].iter().copied().collect();
        assert!(
            admitted.contains(&0) && admitted.contains(&1),
            "the identical pair (tightest cohesion) is admitted, not the looser pair"
        );
    }

    // ---- M3 salience decay (crystal-borrowed percentile) --------------------

    fn decay_opts(pctl: f32) -> ConsolidateOpts {
        ConsolidateOpts { shortterm_decay_pctl: pctl, shortterm_decay_factor: 0.9,
                          ..Default::default() }
    }

    /// The bug this whole mechanism exists for: an absolute floor cannot select
    /// anything when the distribution never reaches it. A rank-based percentile
    /// still fades the intended fraction even when EVERY amplitude is identical —
    /// which is the live case (audio telemetry all lands on the same amplitude).
    #[test]
    fn decay_is_rank_based_so_equal_amplitudes_still_fade() {
        let n = 10;
        let amps = vec![0.6f32; n]; // all equal, all far above the 0.15 evict floor
        let tiers = vec![Tier::ShortTerm; n];
        let retr = vec![0u32; n];
        let none = std::collections::HashSet::new();
        let set = compute_decay_set(&amps, &tiers, &retr, &none, &decay_opts(0.5));
        assert_eq!(set.len(), 5, "half the field fades despite identical amplitudes");
        // Deterministic tie-break by index (dream determinism, #521).
        assert_eq!(set, vec![0, 1, 2, 3, 4]);
        // And the absolute predicate selects nothing at all on this same field.
        let evictable = amps.iter().filter(|&&a| a < 0.15).count();
        assert_eq!(evictable, 0, "this is precisely why decay had to be added");
    }

    /// Weakest-first ordering.
    #[test]
    fn decay_takes_the_weakest_first() {
        let amps = vec![0.9f32, 0.1, 0.5, 0.3];
        let tiers = vec![Tier::ShortTerm; 4];
        let retr = vec![0u32; 4];
        let none = std::collections::HashSet::new();
        let set = compute_decay_set(&amps, &tiers, &retr, &none, &decay_opts(0.5));
        assert_eq!(set, vec![1, 3], "the two weakest, in ascending amplitude order");
    }

    /// Protections: only unretrieved ShortTerm non-merge-participants may fade.
    #[test]
    fn decay_spares_protected_tiers_retrieved_and_merge_members() {
        let amps = vec![0.1f32; 8];
        let tiers = vec![
            Tier::ShortTerm, Tier::ShortTerm, Tier::LongTerm, Tier::Pinned,
            Tier::ShortTerm, Tier::ShortTerm, Tier::ShortTerm, Tier::ShortTerm,
        ];
        // index 4 has been recalled; index 5 is being merged this pass.
        let retr = vec![0, 0, 0, 0, 3, 0, 0, 0];
        let mut participants = std::collections::HashSet::new();
        participants.insert(5usize);
        // pctl 1.0 => take every eligible candidate, so exclusions are the only filter.
        let set = compute_decay_set(&amps, &tiers, &retr, &participants, &decay_opts(1.0));
        let got: std::collections::HashSet<usize> = set.into_iter().collect();
        assert_eq!(got, [0usize, 1, 6, 7].into_iter().collect());
    }

    /// `pctl = 0` is an exact opt-out — the pre-existing behaviour, byte-identical.
    #[test]
    fn decay_disabled_selects_nothing() {
        let amps = vec![0.1f32; 6];
        let tiers = vec![Tier::ShortTerm; 6];
        let retr = vec![0u32; 6];
        let none = std::collections::HashSet::new();
        assert!(compute_decay_set(&amps, &tiers, &retr, &none, &decay_opts(0.0)).is_empty());
        // A factor of 1.0 is a no-op multiply and must also select nothing.
        let noop = ConsolidateOpts { shortterm_decay_pctl: 0.5, shortterm_decay_factor: 1.0,
                                     ..Default::default() };
        assert!(compute_decay_set(&amps, &tiers, &retr, &none, &noop).is_empty());
    }

    /// Decay must converge below the evict floor in bounded time, or it has
    /// merely slowed unbounded growth rather than fixed it.
    #[test]
    fn decay_reaches_the_evict_floor_in_bounded_nights() {
        let o = ConsolidateOpts::default();
        let mut a = 0.60f32; // measured weakest-active on kannaka-prime, 2026-08-22
        let mut nights = 0;
        while a >= o.shortterm_evict && nights < 100 {
            a *= o.shortterm_decay_factor;
            nights += 1;
        }
        assert!(a < o.shortterm_evict, "must actually cross the floor");
        assert!((10..=20).contains(&nights), "≈2 weeks unretrieved, got {nights}");
    }

    // ---- partial admission of an oversized group ----------------------------

    /// Build `k` near-identical vectors around an anchor axis so they form one
    /// transitive group, plus a tight orthogonal pair.
    fn oversized_group_field(k: usize, dim: usize) -> (Vec<Vec<f32>>, usize) {
        let mut owned = Vec::new();
        for i in 0..k {
            let mut v = vec![0.0f32; dim];
            v[0] = 1.0;
            v[1] = 0.001 * i as f32; // tiny spread, still ≥ any sim gate
            owned.push(v);
        }
        (owned, k)
    }

    /// The stranding bug: a group bigger than the per-pass cap used to be skipped
    /// WHOLE, every night, forever. Measured locally 2026-08-22 — 217 absorbable,
    /// 9 absorbed. Partial admission drains it instead, without ever exceeding
    /// the cap.
    #[test]
    fn partial_admission_drains_an_oversized_group_within_the_cap() {
        let dim = 64usize;
        let (owned, k) = oversized_group_field(20, dim);
        let vectors: Vec<&[f32]> = owned.iter().map(|v| v.as_slice()).collect();
        let phases = vec![0.0f32; k];
        let tiers = vec![Tier::LongTerm; k];
        let energies: Vec<f32> = (0..k).map(|i| 1.0 + i as f32 * 0.01).collect();
        // cap_n = floor(0.20 * 20) = 4.
        let opts = ConsolidateOpts {
            mode: ConsolidateMode::Apply,
            max_absorb_frac: Some(0.20),
            merge_partial_groups: true,
            ..Default::default()
        };
        let g = compute_merge_grouping(&vectors, &phases, &tiers, &energies, &opts, false);
        assert_eq!(g.groups_before_cap, 1, "one transitive blob");
        assert_eq!(g.absorb_before_cap, k - 1);
        let absorbed: usize = g.admitted.iter().map(|m| m.len() - 1).sum();
        assert!(absorbed > 0, "the group must no longer be stranded");
        assert!(absorbed <= 4, "cap honoured exactly, got {absorbed}");
        assert_eq!(absorbed, 4, "drains the full remaining capacity");
    }

    /// The cap invariant is the safety property; partial admission must not weaken it.
    #[test]
    fn partial_admission_never_exceeds_the_cap() {
        let dim = 64usize;
        for k in [5usize, 12, 33, 50] {
            for frac in [0.05f32, 0.1, 0.2, 0.5] {
                let (owned, _) = oversized_group_field(k, dim);
                let vectors: Vec<&[f32]> = owned.iter().map(|v| v.as_slice()).collect();
                let phases = vec![0.0f32; k];
                let tiers = vec![Tier::LongTerm; k];
                let energies = vec![1.0f32; k];
                let opts = ConsolidateOpts {
                    mode: ConsolidateMode::Apply,
                    max_absorb_frac: Some(frac),
                    merge_partial_groups: true,
                    ..Default::default()
                };
                let g =
                    compute_merge_grouping(&vectors, &phases, &tiers, &energies, &opts, false);
                let cap_n = (frac * k as f32).floor() as usize;
                let absorbed: usize = g.admitted.iter().map(|m| m.len() - 1).sum();
                assert!(absorbed <= cap_n, "k={k} frac={frac}: {absorbed} > cap {cap_n}");
                // A partially-admitted group is still a valid group (≥2 members).
                assert!(g.admitted.iter().all(|m| m.len() >= 2));
            }
        }
    }

    /// Opt-out restores the historical skip-whole behaviour exactly.
    #[test]
    fn partial_admission_off_strands_the_group_as_before() {
        let dim = 64usize;
        let (owned, k) = oversized_group_field(20, dim);
        let vectors: Vec<&[f32]> = owned.iter().map(|v| v.as_slice()).collect();
        let phases = vec![0.0f32; k];
        let tiers = vec![Tier::LongTerm; k];
        let energies = vec![1.0f32; k];
        let opts = ConsolidateOpts {
            mode: ConsolidateMode::Apply,
            max_absorb_frac: Some(0.20),
            merge_partial_groups: false,
            ..Default::default()
        };
        let g = compute_merge_grouping(&vectors, &phases, &tiers, &energies, &opts, false);
        assert!(g.admitted.is_empty(), "historical behaviour: skipped whole");
        assert!(g.capped());
    }

    /// End-to-end: even if the criteria are fully fooled into one giant redundant
    /// group, the per-pass absorb cap bounds how much a single apply can remove —
    /// the property that would have turned 295→82 into a bounded event.
    #[test]
    fn apply_absorb_cap_bounds_over_merge() {
        let mut store =
            HrmStore::new(make_test_pipeline(), NamedTempFile::new().unwrap().path().to_path_buf());
        // Three orthogonal clusters of 3 near-identical LongTerm memories each
        // (9) + one unrelated survivor = 10. All 3 clusters WOULD merge (absorb 6).
        for cluster in 0..3usize {
            let anchor = cluster * 100;
            for k in 0..3usize {
                let mut v = vec![0.0f32; WAVEFRONT_DIM];
                v[anchor] = 1.0;
                v[anchor + 1 + k] = 0.001; // tiny within-cluster perturbation
                insert_ctl(&mut store, v, &format!("c{cluster}-{k}"), 1.0, 0.0, Tier::LongTerm, 0);
            }
        }
        let mut u = vec![0.0f32; WAVEFRONT_DIM]; u[900] = 1.0;
        insert_ctl(&mut store, u, "solo", 1.0, 0.0, Tier::LongTerm, 0);
        assert_eq!(store.count(), 10);

        // Cap at 20% of the field: floor(0.2 * 10) = 2 absorbs allowed.
        let opts = ConsolidateOpts {
            mode: ConsolidateMode::Apply,
            max_absorb_frac: Some(0.20),
            ..Default::default()
        };
        let report = store.apply_consolidation(&opts);

        assert_eq!(report.groups_before_cap, 3, "criteria found all 3 clusters");
        assert_eq!(report.absorb_before_cap, 6, "would absorb 6 without the cap");
        assert!(report.absorb_capped, "the cap engaged");
        assert!(report.would_absorb <= 2, "absorbed bounded to floor(0.2*10)=2, got {}", report.would_absorb);
        assert_eq!(store.count(), 10 - report.would_absorb, "count drops by exactly the bounded absorb");
        assert!(store.count() >= 8, "the cap prevented the catastrophic collapse");
    }

    /// Dry-run parity: `plan_consolidation` (read-only) and `apply_consolidation`
    /// must agree on groups / absorb / evict / projection for the same state, so
    /// the nightly dry-run projection faithfully predicts a destructive apply.
    #[test]
    fn dryrun_apply_parity() {
        let build = || {
            let mut store = HrmStore::new(
                make_test_pipeline(),
                NamedTempFile::new().unwrap().path().to_path_buf(),
            );
            let base = vec![0.5f32; WAVEFRONT_DIM];
            for k in 0..4usize {
                let mut v = base.clone();
                v[k] += 0.001;
                insert_ctl(&mut store, v, &format!("d{k}"), 1.0, 0.0, Tier::LongTerm, 0);
            }
            // A cold ShortTerm evict candidate + an unrelated LongTerm survivor.
            let mut e = vec![0.0f32; WAVEFRONT_DIM]; e[10] = 1.0;
            insert_ctl(&mut store, e, "cold", 0.05, 0.0, Tier::ShortTerm, 0);
            let mut u = vec![0.0f32; WAVEFRONT_DIM]; u[200] = 1.0;
            insert_ctl(&mut store, u, "solo", 1.0, 0.0, Tier::LongTerm, 0);
            store
        };

        let plan_store = build();
        let plan = plan_store.plan_consolidation(&ConsolidateOpts {
            mode: ConsolidateMode::DryRun,
            ..Default::default()
        });

        let mut apply_store = build();
        let applied = apply_store.apply_consolidation(&apply_opts());

        assert_eq!(plan.groups_found, applied.groups_found, "groups parity");
        assert_eq!(plan.would_absorb, applied.would_absorb, "absorb parity");
        assert_eq!(plan.would_evict, applied.would_evict, "evict parity");
        assert_eq!(
            plan.projected_memories, applied.projected_memories,
            "projected-count parity"
        );
        // Sanity: the 4 dups collapse to one carrier and the cold ShortTerm evicts.
        assert_eq!(plan.would_absorb, 3);
        assert_eq!(plan.would_evict, 1);
    }

    // hunt regression: dream-entropy perturbation must touch each hallucination
    // ONCE. Re-touching a stamped hallucination every dream would re-draw paid
    // entropy and random-walk its phase (unbounded ±0.05 rad drift).
    #[test]
    fn entropy_perturbation_stamps_each_hallucination_once() {
        use crate::medium::types::WavefrontMeta;
        let mut meta = vec![
            WavefrontMeta::new(uuid::Uuid::new_v4(), "real".into()),
            WavefrontMeta::new(uuid::Uuid::new_v4(), "hallu".into()),
        ];
        meta[1].hallucinated = true;
        let mut phases = vec![0.0f32, 1.0f32];
        let bytes = [7u8; 32];
        let prov = crate::entropy::Provenance::default();

        let n1 = apply_entropy_perturbation(&mut meta, &mut phases, &bytes, &prov);
        assert_eq!(n1, 1, "the un-stamped hallucination is perturbed once");
        assert!(meta[1].provenance.is_some(), "hallucination is stamped");
        let phase_after_first = phases[1];

        let n2 = apply_entropy_perturbation(&mut meta, &mut phases, &bytes, &prov);
        assert_eq!(n2, 0, "an already-stamped hallucination is NOT re-drawn/re-perturbed");
        assert_eq!(
            phases[1], phase_after_first,
            "phase must not random-walk on repeat entropy passes"
        );
    }

    // hunt regression: a ShortTerm near-duplicate pair that is BOTH merge-eligible
    // (near-identical) AND evict-eligible (amplitude 0.05 < 0.15, retr 0). The plan
    // must not double-count them — apply absorbs one via the merge and skips ALL
    // merge members in its evict pass, so plan/apply projections must still match.
    #[test]
    fn dryrun_apply_parity_merge_evict_overlap() {
        let build = || {
            let mut store = HrmStore::new(
                make_test_pipeline(),
                NamedTempFile::new().unwrap().path().to_path_buf(),
            );
            let base = vec![0.5f32; WAVEFRONT_DIM];
            let mut a = base.clone();
            a[0] += 0.001;
            insert_ctl(&mut store, a, "dupA", 0.05, 0.0, Tier::ShortTerm, 0);
            let mut b = base.clone();
            b[1] += 0.001;
            insert_ctl(&mut store, b, "dupB", 0.05, 0.0, Tier::ShortTerm, 0);
            let mut u = vec![0.0f32; WAVEFRONT_DIM];
            u[200] = 1.0;
            insert_ctl(&mut store, u, "solo", 1.0, 0.0, Tier::LongTerm, 0);
            store
        };

        let plan = build().plan_consolidation(&ConsolidateOpts {
            mode: ConsolidateMode::DryRun,
            ..Default::default()
        });
        let mut apply_store = build();
        let applied = apply_store.apply_consolidation(&apply_opts());

        assert_eq!(plan.would_evict, applied.would_evict, "evict parity (merge/evict overlap)");
        assert_eq!(
            plan.projected_memories, applied.projected_memories,
            "projected-count parity (merge/evict overlap)"
        );
    }

    // -----------------------------------------------------------------------
    // T1.4 (#474) — gated dream entropy: default-off determinism + honest,
    // reproducible provenance only when a draw actually occurs.
    // -----------------------------------------------------------------------

    #[test]
    fn dream_entropy_gate_off_is_deterministic_and_records_no_provenance() {
        // Default (KANNAKA_DREAM_ENTROPY unset) ⇒ apply_dream_entropy is a no-op:
        // no draw, no jitter, no stamp — the honest absence, never a false prng://.
        let mut store =
            HrmStore::new(make_test_pipeline(), NamedTempFile::new().unwrap().path().to_path_buf());
        let mut v = vec![0.0f32; WAVEFRONT_DIM]; v[0] = 1.0;
        let hallu = insert_ctl(&mut store, v, "dream product", 1.0, 0.3, Tier::LongTerm, 0);
        let idx = store.medium.get_wavefront_index(&hallu).unwrap();
        store.medium.store.metadata[idx].hallucinated = true;
        let phase_before = store.medium.store.phase[idx];

        store.apply_dream_entropy(); // gate off (default)

        assert_eq!(store.provenance_of(&hallu), None, "no provenance stamped when gate off");
        assert_eq!(
            store.medium.store.phase[idx], phase_before,
            "phase unchanged — dream stays byte-identically deterministic"
        );
    }

    #[test]
    fn entropy_perturbation_is_reproducible_and_honest() {
        use crate::entropy::Provenance;
        use crate::medium::types::WavefrontMeta;

        let prov = Provenance::reservoir("drbg-expand", vec!["job-x".into()], vec!["dev".into()]);
        let mk = || {
            let mut h = WavefrontMeta::new(uuid::Uuid::new_v4(), "hallu".into());
            h.hallucinated = true;
            let ordinary = WavefrontMeta::new(uuid::Uuid::new_v4(), "ordinary".into());
            vec![h, ordinary]
        };
        let bytes = [1u8, 2, 3, 4, 5, 6, 7, 8, 9, 10];

        // Same bytes ⇒ identical perturbation (reproducible dynamics).
        let (mut ma, mut pa) = (mk(), vec![0.5f32, 0.5]);
        let (mut mb, mut pb) = (mk(), vec![0.5f32, 0.5]);
        let ca = apply_entropy_perturbation(&mut ma, &mut pa, &bytes, &prov);
        let cb = apply_entropy_perturbation(&mut mb, &mut pb, &bytes, &prov);
        assert_eq!(ca, 1, "only the hallucination is perturbed");
        assert_eq!(cb, 1);
        assert_eq!(pa, pb, "same entropy ⇒ same phase jitter (reproducible)");

        // Honest stamping: the TRUE drawn provenance on the hallucination only.
        assert_eq!(ma[0].provenance.as_ref().unwrap().source, "reservoir://drbg-expand");
        assert_eq!(ma[0].provenance.as_ref().unwrap().qpu_jobs, vec!["job-x".to_string()]);
        assert_eq!(ma[1].provenance, None, "a non-hallucination is never stamped");
        assert_ne!(pa[0], 0.5, "the hallucination's phase actually jittered");
        assert_eq!(pa[1], 0.5, "the ordinary wavefront's phase is untouched");

        // Different bytes ⇒ different jitter: the dynamics genuinely depend on
        // the drawn entropy (not a cosmetic stamp).
        let (mut mc, mut pc) = (mk(), vec![0.5f32, 0.5]);
        apply_entropy_perturbation(&mut mc, &mut pc, &[42u8; 10], &prov);
        assert_ne!(pa[0], pc[0], "different entropy ⇒ different dream perturbation");
    }

    // #521: the dream-entropy draw is gated on a non-empty touched set. A dream
    // with no hallucinated wavefronts has a zero touched count, so `apply_dream_
    // entropy` returns before drawing — an inert dream spends no reservoir bytes
    // and records an honest absence, instead of stamping nothing at real cost.
    #[test]
    fn dream_entropy_touched_count_gates_the_draw() {
        use crate::medium::types::WavefrontMeta;
        let temp = NamedTempFile::new().unwrap();
        let mut store = HrmStore::new(make_test_pipeline(), temp.path().to_path_buf());

        // Nothing hallucinated ⇒ zero touched set ⇒ no draw is warranted.
        assert_eq!(
            store.dream_entropy_touched_count(),
            0,
            "no hallucinations ⇒ nothing to perturb ⇒ the dream must not draw"
        );

        // Add one hallucination + one ordinary wavefront to whichever medium the
        // gate reads; only the hallucination is a real stamp target.
        let push = |metas: &mut Vec<WavefrontMeta>| {
            let mut h = WavefrontMeta::new(Uuid::new_v4(), "hallu".into());
            h.hallucinated = true;
            metas.push(h);
            metas.push(WavefrontMeta::new(Uuid::new_v4(), "ordinary".into()));
        };
        match store.chiral {
            Some(ref mut c) => push(&mut c.right.metadata),
            None => push(&mut store.medium.store.metadata),
        }
        assert_eq!(
            store.dream_entropy_touched_count(),
            1,
            "only the hallucinated wavefront counts toward the touched set"
        );
    }
}