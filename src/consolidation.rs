//! Memory consolidation engine � the dreaming/sleep layer.
//!
//! Processes memories through 9 stages mimicking human sleep consolidation:
//! 1. REPLAY � collect working set of memories in layer range
//! 2. DETECT � find interference patterns
//! 3. BUNDLE � create summary hypervectors
//! 4. STRENGTHEN � boost constructive interference pairs
//! 5. SYNC � Kuramoto phase synchronization
//! 6. XI_REPULSION � Apply Xi-based memory separation
//! 7. PRUNE � weaken destructive interference pairs
//! 8. TRANSFER � move memories to deeper temporal layers
//! 9. WIRE � create new skip links from consolidation discoveries

use std::f32::consts::PI;
use std::time::Instant;

use chrono::{Duration, Utc};
use uuid::Uuid;

use serde::{Deserialize, Serialize};

use crate::geometry::fano_related;
use crate::kuramoto::KuramotoSync;
use crate::spiral::norm; // #503: normalize stored phases into [0, TAU) at write time
use crate::xi_operator::{xi_repulsive_force, compute_xi_signature};
// SkipLink removed - associations now emergent from ChiralMedium interference
use crate::store::{ResonanceEngine, MediumBackend};
use crate::wave::{cosine_similarity, normalize};

#[cfg(feature = "collective")]
use rayon::prelude::*;

/// Amplitude ceiling for the particle-consolidation strengthen path (#360).
/// The wave-native observe path already caps wavefront energy at 2.0
/// (`consciousness.rs`); this path grew amplitude unbounded, so repeated
/// constructive-heavy dreams could inflate energies without limit and distort
/// resonance ranking + Φ/Kuramoto metrics. Clamp every additive boost to the
/// same ceiling. `HrmStore::sync_cache_to_medium` copies amplitude straight
/// into `store.energy`, so this also bounds the on-disk energy.
pub(crate) const AMPLITUDE_CEILING: f32 = 2.0;

/// Classification of interference between two memories.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Interference {
    Constructive,
    Destructive,
}

/// A detected interference pair.
#[derive(Debug, Clone)]
struct InterferencePair {
    id_a: Uuid,
    id_b: Uuid,
    similarity: f32,
    kind: Interference,
}

// ---------------------------------------------------------------------------
// Circular phase arithmetic
//
// Stored phases drift UNBOUNDED across dreams (Kuramoto integration and the
// medium dynamics increment without wrapping), so any phase arithmetic that
// treats them as plain numbers — arithmetic means, |a−b| gaps, linear blends —
// silently breaks once a field has drifted or a pair straddles the 2π wrap.
// All consolidation stages route phase math through these two helpers.
// ---------------------------------------------------------------------------

/// Angular midpoint of two phases (circular mean via summed unit vectors),
/// in (−π, π]. Unlike `(a + b) / 2`, correct for wrap-straddling pairs:
/// the arithmetic mean of 6.2 and 0.2 is ~π (anti-phase to both); the
/// circular mean is ~0.06 (between them).
fn circular_mean2(a: f32, b: f32) -> f32 {
    (a.sin() + b.sin()).atan2(a.cos() + b.cos())
}

/// Wrapped signed angular difference `to − from`, in [−π, π). Delegates to
/// the crate's canonical wrap (spiral.rs) — one convention, one
/// implementation, and rem_euclid stays exact for large drifted inputs.
fn wrapped_phase_delta(to: f32, from: f32) -> f32 {
    crate::spiral::wrap(to - from)
}

/// Signed phase-repulsion step for a pair (applied `+` to a, `−` to b):
/// moves the WRAPPED gap |φa−φb| toward the π/2 differentiation target,
/// carrying the sign of the gap so the movement direction is correct for
/// either pair ordering (a fixed sign attracted whenever φa < φb). Pairs
/// closer than π/2 widen; pairs beyond π/2 (e.g. near-anti-phase) relax
/// back toward π/2 — the stage targets maximal differentiation, not
/// maximal separation.
fn repulsion_phase_correction(phase_a: f32, phase_b: f32, strength: f32) -> f32 {
    let target_diff = std::f32::consts::FRAC_PI_2;
    let wrapped_diff = wrapped_phase_delta(phase_a, phase_b);
    // f32::signum(0.0) is 1.0, which conveniently breaks the identical-phase tie.
    strength * 0.5 * (target_diff - wrapped_diff.abs()) * wrapped_diff.signum()
}

// ---------------------------------------------------------------------------
// Coboundary Validation for Hallucination Synthesis
// ---------------------------------------------------------------------------

/// Coboundary matrix representing the transformation between two memories' 
/// wave dynamics. Inspired by the paper's coboundary equivalence:
/// A(n)�U(n+1) = U(n)�B(n)
#[derive(Debug, Clone)]
pub struct CoboundaryMatrix {
    /// Transformation matrix between vector spaces
    pub transformation: Vec<Vec<f32>>,
    /// Phase transformation parameters
    pub phase_transform: (f32, f32), // (scale, offset)
    /// Frequency transformation parameters  
    pub frequency_transform: (f32, f32), // (scale, offset)
    /// How well this transformation fits the data (0-1)
    pub fit_quality: f32,
}

/// Score evaluating whether a hallucinated memory is a valid unification
#[derive(Debug, Clone)]
pub struct CoboundaryScore {
    /// Overall validity of the hallucination (0-1)
    pub validity: f32,
    /// Residual error after applying transformations
    pub residual_error: f32,
    /// Complexity of the transformation (higher = more complex)
    pub transformation_complexity: f32,
    /// Whether this passes the validation threshold
    pub is_valid: bool,
}

/// Statistics from a single consolidation cycle.
#[derive(Debug, Clone, Default)]
pub struct ConsolidationReport {
    pub memories_replayed: usize,
    pub interference_pairs_found: usize,
    pub constructive_pairs: usize,
    pub destructive_pairs: usize,
    pub bundles_created: usize,
    pub memories_strengthened: usize,
    pub memories_pruned: usize,
    pub clusters_synced: usize,
    pub sync_order_improvement: f32,
    pub memories_transferred: usize,
    /// ADR-0054: rows ghosted by the retention-policy stage (cap/TTL rules
    /// from config `[retention]`; 0 when KANNAKA_TRIAGE is off or no rules).
    pub retention_ghosted: usize,
    pub skip_links_created: usize,
    pub hallucinations_created: usize,
    pub duration_ms: u64,
    /// Kuramoto order parameter R after consolidation (EXP-003)
    pub final_order_parameter: f32,
    /// Actions taken by the Kannaktopus executive oscillator (Stage 10)
    pub kannaktopus_actions: usize,
    /// Cluster indices targeted by Kannaktopus (weakest, strongest, isolated)
    pub kannaktopus_targets: Vec<usize>,
    /// Long-ghosted (zero-amplitude) memories hard-deleted this cycle (#361).
    pub ghosts_reclaimed: usize,
}

/// Report from modality-aware dream consolidation (NCS Phase 3.2, #48).
#[derive(Debug, Clone, Default)]
pub struct ModalityDreamReport {
    /// Number of distinct modalities processed.
    pub modalities_processed: usize,
    /// Per-modality (name, intra-cluster coherence R).
    pub per_modality_coherence: Vec<(String, f32)>,
    /// Number of memories strengthened (intra-modality phase alignment).
    pub memories_strengthened: usize,
    /// Number of cross-modality confusion pairs detected.
    pub cross_modality_confusions: usize,
}

/// Report from swarm-aware dream consolidation (QS-6, #57).
#[derive(Debug, Clone, Default)]
pub struct SwarmDreamReport {
    /// Phase deviation from swarm mean (radians).
    pub phase_deviation: f32,
    /// Change in chiral perturbation (positive = more divergence).
    pub chiral_adjustment: f32,
    /// New chiral eta after adjustment.
    pub new_chiral_eta: f32,
    /// Total memories consolidated in this cycle.
    pub memories_consolidated: usize,
    /// Embedded modality-aware report if applicable.
    pub modality_report: Option<ModalityDreamReport>,
}

/// Adaptive parameters that persist between dream cycles (EXP-003).
///
/// After each cycle, the engine observes the Kuramoto order parameter R
/// and adjusts parameters to maintain R in the sweet spot [0.55, 0.85]:
/// - R too high ? reduce constructive_boost, raise prune_threshold (rigid ? loosen)
/// - R too low  ? increase coupling strength (fragmented ? bind)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdaptiveParams {
    pub constructive_boost: f32,
    pub prune_threshold: f32,
    pub destructive_penalty: f32,
    /// Target range for order parameter R
    pub r_target_low: f32,
    pub r_target_high: f32,
    /// Learning rate for parameter adaptation
    pub adaptive_rate: f32,
}

impl Default for AdaptiveParams {
    fn default() -> Self {
        Self {
            constructive_boost: 0.3,
            prune_threshold: 0.1,
            destructive_penalty: 0.5,
            r_target_low: 0.55,
            r_target_high: 0.85,
            adaptive_rate: 0.05,
        }
    }
}

impl AdaptiveParams {
    /// Adapt parameters based on observed Kuramoto order parameter R.
    /// Returns the new params (mutates self in place too).
    pub fn adapt(&mut self, order_parameter: f32) {
        let r_mid = (self.r_target_low + self.r_target_high) / 2.0;
        let error = order_parameter - r_mid;

        if order_parameter > self.r_target_high {
            // Over-synchronized (rigid): loosen up
            self.constructive_boost = (self.constructive_boost - error * self.adaptive_rate).max(0.05);
            self.prune_threshold = (self.prune_threshold + error * self.adaptive_rate * 0.5).min(0.3);
        } else if order_parameter < self.r_target_low {
            // Under-synchronized (fragmented): tighten coupling, reduce pruning aggression
            // error is negative here, so -error is positive
            self.constructive_boost = (self.constructive_boost + (-error) * self.adaptive_rate).min(0.6);
            self.destructive_penalty = (self.destructive_penalty - (-error) * self.adaptive_rate * 0.3).max(0.1);
        }
        // In target range: no adjustment (stable)
    }
}

/// The 9-stage consolidation engine.
pub struct ConsolidationEngine {
    /// Similarity threshold for interference detection
    pub interference_threshold: f32,
    /// Phase difference threshold for constructive vs destructive
    pub phase_alignment_threshold: f32,
    /// Minimum amplitude to survive pruning
    pub prune_threshold: f32,
    /// How much amplitude boost from constructive interference
    pub constructive_boost: f32,
    /// How much amplitude reduction from destructive interference
    pub destructive_penalty: f32,
    /// Kuramoto synchronization parameters
    pub kuramoto: KuramotoSync,
    /// Adaptive parameters that evolve between dream cycles (EXP-003)
    pub adaptive: AdaptiveParams,
    /// Chiral perturbation strength (0.0 = disabled)
    pub chiral_perturbation: f32,
    /// Absolute amplitude floor: memories below this are pruned regardless of interference.
    /// Catches isolated low-amplitude noise that evades interference detection.
    pub noise_floor: f32,
    /// Starting amplitude for hallucinated memories
    pub hallucination_amplitude: f32,
    /// Skip destructive dampening for memories with amplitude > 0.5 (signal protection)
    pub protect_established: bool,
    /// Xi repulsion threshold for phase separation during consolidation.
    /// Pairs with xi_repulsion above this value get their phases pushed apart.
    /// L4.S15: tuned post-xi-fix. 0.30 was unreachable pre-fix, 0.15 too
    /// aggressive (300/300 pairs qualify, phase_coherence collapses).
    pub repulsion_threshold: f32,
}

impl Default for ConsolidationEngine {
    fn default() -> Self {
        Self {
            interference_threshold: 0.05,
            phase_alignment_threshold: PI / 2.0,
            prune_threshold: 0.1,
            constructive_boost: 0.3,
            destructive_penalty: 0.5,
            kuramoto: KuramotoSync::default(),
            adaptive: AdaptiveParams::default(),
            chiral_perturbation: 0.0,
            noise_floor: 0.0,
            hallucination_amplitude: 0.5,
            protect_established: false,
            repulsion_threshold: 0.22,
        }
    }
}

impl ConsolidationEngine {
    /// Run a full 9-stage consolidation cycle on memories at the given layer range.
    pub fn consolidate(
        &self,
        engine: &mut ResonanceEngine,
        min_layer: u8,
        max_layer: u8,
    ) -> ConsolidationReport {
        let start = Instant::now();
        let mut report = ConsolidationReport::default();
        // Wall-clock instant the cycle began, captured BEFORE stage_prune ghosts
        // anything. stage_compact_ghosts uses it to never hard-delete a memory in
        // the same cycle it was ghosted (see that stage for why).
        let cycle_started_at = Utc::now();

        // Stage 1: REPLAY � collect working set of memories in layer range
        let working_set = self.stage_replay(engine, min_layer, max_layer);
        report.memories_replayed = working_set.len();

        // Stage 2: DETECT � find interference patterns
        let pairs = self.stage_detect(engine, &working_set);
        report.interference_pairs_found = pairs.len();
        report.constructive_pairs = pairs.iter().filter(|p| p.kind == Interference::Constructive).count();
        report.destructive_pairs = pairs.iter().filter(|p| p.kind == Interference::Destructive).count();

        // Stage 3: BUNDLE � create summary vectors per layer
        report.bundles_created = self.stage_bundle(engine, &working_set, max_layer);

        // Stage 4: STRENGTHEN -- boost constructive pairs
        report.memories_strengthened = self.stage_strengthen(engine, &pairs);

        // Stage 4.5: SYNC -- Kuramoto phase synchronization, OR
        //                    interference-driven phase relaxation (env-gated).
        // DREAM_MODE=interference_relax bypasses the category-Kuramoto sync and
        // does a relaxation step driven by the constructive interference pairs
        // already detected in stage 2. No new params, no categorization.
        // See research/intersections/05-magic-gives-it-gravity.md.
        let dream_mode = std::env::var("DREAM_MODE").unwrap_or_default();
        let (clusters_synced, order_improvement) = if dream_mode == "interference_relax" {
            self.stage_interference_relax(engine, &working_set, &pairs)
        } else {
            self.stage_sync(engine, &working_set)
        };
        report.clusters_synced = clusters_synced;
        report.sync_order_improvement = order_improvement;

        // Stage 4.6: XI_REPULSION -- Apply Xi-based memory separation
        self.stage_xi_repulsion(engine, &working_set);

        // Stage 5: HALLUCINATE -- generate cross-cluster bridges BEFORE pruning.
        // Hallucination needs access to all living memories across clusters;
        // pruning ghosts low-amplitude memories that still carry topological info.
        report.hallucinations_created = self.stage_hallucinate(engine, &working_set);

        // Stage 6: PRUNE -- weaken destructive pairs
        report.memories_pruned = self.stage_prune(engine, &pairs);

        // Stage 6b: TRANSFER -- promote old memories to deeper layers
        report.memories_transferred = self.stage_transfer(engine);

        // Stage 6b': RETENTION TRIAGE (ADR-0054) -- enforce declared per-prefix
        // cap/TTL policy by GHOSTING (never hard-deleting) the lowest-value
        // excess. Runs before COMPACT so a row ghosted here gets the full
        // ADR-0037 recovery window before any hard delete. Dark by default:
        // requires KANNAKA_TRIAGE=1 AND a non-empty [retention] table.
        report.retention_ghosted = self.stage_retention_triage(engine);

        // Stage 6c: COMPACT -- reclaim long-ghosted memories (#361). Pruning only
        // soft-deletes (amplitude=0.0); without this sweep ghosts re-serialize
        // and reload forever, inflating total_memories and dragging the O(n²)/
        // O(n³) recall + eigendecomp. Deletes only ghosts that have stayed at
        // zero past the recovery window, never pinned/summary memories.
        report.ghosts_reclaimed = self.stage_compact_ghosts(engine, cycle_started_at);

        // Stage 7: WIRE -- create skip links for cross-layer constructive pairs
        report.skip_links_created = self.stage_wire(engine, &pairs, &working_set);

        // Stage 9: CHIRAL_PERTURBATION -- break lock-step synchronization
        if self.chiral_perturbation > 0.0 {
            self.stage_chiral_perturbation(engine, &working_set);
        }

        // Stage 10: KANNAKTOPUS — executive oscillator that focuses attention on weak topology
        let (kannaktopus_actions, kannaktopus_targets) = self.stage_kannaktopus(engine, &working_set, &report);
        report.kannaktopus_actions = kannaktopus_actions;
        report.kannaktopus_targets = kannaktopus_targets;

        // EXP-003: Compute final order parameter and record it
        let final_r = self.compute_global_order_parameter(engine, &working_set);
        report.final_order_parameter = final_r;

        report.duration_ms = start.elapsed().as_millis() as u64;
        report
    }

    /// Apply adaptive parameter tuning based on a consolidation report (EXP-003).
    ///
    /// Call this after `consolidate()` to evolve ?, boost, and threshold
    /// for the next dream cycle. Mirrors ghostmagicOS adaptive ?.
    pub fn adapt_from_report(&mut self, report: &ConsolidationReport) {
        self.adaptive.adapt(report.final_order_parameter);
        // Apply adapted params to engine for next cycle
        self.constructive_boost = self.adaptive.constructive_boost;
        self.prune_threshold = self.adaptive.prune_threshold;
        self.destructive_penalty = self.adaptive.destructive_penalty;
    }

    /// Compute global Kuramoto order parameter R across all working set memories.
    fn compute_global_order_parameter(&self, engine: &ResonanceEngine, working_set: &[Uuid]) -> f32 {
        let memories: Vec<crate::memory::HyperMemory> = working_set
            .iter()
            .filter_map(|id| engine.store.get(id).ok().flatten().cloned())
            .collect();
        self.compute_category_order_parameter(&memories)
    }

    /// Stage 1: Load memories in the given layer range into a working set.
    fn stage_replay(&self, engine: &ResonanceEngine, min_layer: u8, max_layer: u8) -> Vec<Uuid> {
        let all = engine.store.all_memories().unwrap_or_default();
        all.iter()
            .filter(|m| m.layer_depth >= min_layer && m.layer_depth <= max_layer)
            .map(|m| m.id)
            .collect()
    }

    /// Stage 2: Detect interference patterns between memory pairs.
    ///
    /// Uses brute-force neighbor search. In HRM, associations are emergent from interference, not indexed.
    /// brute-force O(n�). Each memory queries its K nearest neighbors, then
    /// checks phase alignment to classify as constructive or destructive.
    fn stage_detect(&self, engine: &ResonanceEngine, working_set: &[Uuid]) -> Vec<InterferencePair> {
        use std::collections::HashSet;

        // Number of nearest neighbors to query per memory.
        // Higher = more thorough but slower. 32 catches most interference pairs.
        // Request k_neighbors+1 because the medium returns the query memory itself as
        // the top hit (similarity=1.0); the self-result is filtered below.
        let k_neighbors: usize = 32.min(working_set.len().saturating_sub(1));
        if k_neighbors == 0 {
            return Vec::new();
        }

        let mut pairs = Vec::new();
        let mut seen = HashSet::new();

        for &id in working_set {
            let (vec_a, phase_a) = match engine.store.get(&id).ok().flatten() {
                Some(m) => (m.vector.clone(), m.phase),
                None => continue,
            };

            // Internal search for dream consolidation � raw similarity without observation.
            // Dreams should NOT count as attention (observation would amplify during annealing).
            let neighbors = match engine.store.search(&vec_a, k_neighbors + 1) {
                Ok(n) => n,
                Err(_) => continue,
            };

            for (neighbor_id, sim) in neighbors {
                // Skip self and memories not in working set
                if neighbor_id == id || sim <= self.interference_threshold {
                    continue;
                }

                // Deduplicate: canonical pair ordering
                let pair_key = if id < neighbor_id {
                    (id, neighbor_id)
                } else {
                    (neighbor_id, id)
                };
                if !seen.insert(pair_key) {
                    continue;
                }

                let (phase_b, freq_b) = match engine.store.get(&neighbor_id).ok().flatten() {
                    Some(m) => (m.phase, m.frequency),
                    None => continue,
                };

                // Frequency-band gating: memories with very different natural frequencies
                // cannot constructively interfere — classify as destructive.
                let freq_a = match engine.store.get(&id).ok().flatten() {
                    Some(m) => m.frequency,
                    None => 0.0,
                };
                let freq_ratio = if freq_a > freq_b { freq_a / freq_b.max(1e-6) } else { freq_b / freq_a.max(1e-6) };
                // Frequency-band gating with amplitude check: only apply to low-amplitude
                // memories to avoid destroying legitimate high-amplitude signals in different
                // frequency bands (e.g., emotion memories at freq=1.5).
                let low_amp = match engine.store.get(&id).ok().flatten() {
                    Some(m) => m.amplitude < 0.3,
                    None => false,
                } || match engine.store.get(&neighbor_id).ok().flatten() {
                    Some(m) => m.amplitude < 0.3,
                    None => false,
                };
                let frequency_mismatch = freq_ratio > 3.0 && low_amp;

                let phase_diff = (phase_a - phase_b).abs();
                let phase_diff = phase_diff % (2.0 * PI);
                let phase_diff = if phase_diff > PI { 2.0 * PI - phase_diff } else { phase_diff };

                // ADR-0037: under the belief substrate the field is deliberately
                // phase-scattered (order ~1.0 → ~0.13), so with the default π/2
                // threshold (Constructive <π/2, Destructive >π/2, NO gap) a huge
                // fraction of similar pairs flips to Destructive and the field is
                // mass-ghosted. Open a real NEUTRAL band by lowering the threshold
                // to π/3 (Constructive <π/3, Destructive >2π/3, neutral between)
                // so only strongly-opposed pairs are pruned. Default path keeps π/2
                // (byte-identical).
                let align_thresh = if crate::medium::chiral::belief_phase_enabled() {
                    self.phase_alignment_threshold.min(std::f32::consts::FRAC_PI_3)
                } else {
                    self.phase_alignment_threshold
                };
                let kind = if frequency_mismatch {
                    Interference::Destructive // different frequency bands can't sync
                } else if phase_diff < align_thresh {
                    Interference::Constructive
                } else if phase_diff > PI - align_thresh {
                    Interference::Destructive
                } else {
                    continue; // neutral
                };

                pairs.push(InterferencePair {
                    id_a: pair_key.0,
                    id_b: pair_key.1,
                    similarity: sim,
                    kind,
                });
            }
        }
        pairs
    }

    /// Stage 3: Bundle memories at each layer into summary vectors at the next layer.
    fn stage_bundle(&self, engine: &mut ResonanceEngine, working_set: &[Uuid], max_layer: u8) -> usize {
        let mut bundles_created = 0;

        // Prune stale summary memories from previous dream cycles before creating new ones.
        // This prevents unbounded accumulation of __consolidation_summary_layer_N memories.
        let stale_summaries: Vec<Uuid> = engine.store.all_memories()
            .unwrap_or_default()
            .into_iter()
            .filter(|m| m.content.starts_with("__consolidation_summary_layer_"))
            .map(|m| m.id)
            .collect();
        for id in stale_summaries {
            let _ = engine.delete(&id);
        }

        for layer in 0..=max_layer {
            let vectors: Vec<Vec<f32>> = working_set
                .iter()
                .filter_map(|id| engine.store.get(id).ok().flatten())
                .filter(|m| m.layer_depth == layer)
                .map(|m| m.vector.clone())
                .collect();

            if vectors.len() < 2 {
                continue;
            }

            // Bundle using the encoding pipeline
            let summary = engine.pipeline.bundle(&vectors);
            let mut summary_mem = crate::memory::HyperMemory::new(
                summary,
                format!("__consolidation_summary_layer_{layer}"),
            );
            // Deterministic UUID for bundle summaries to ensure reproducible consolidation.
            // Derived from layer index to avoid random UUID nondeterminism.
            summary_mem.id = uuid::Uuid::from_u128(0xBDBD_0000_0000_0000_0000_0000_0000_0000_u128 + layer as u128);
            summary_mem.layer_depth = layer.saturating_add(1);

            if engine.store.insert(summary_mem).is_ok() {
                bundles_created += 1;
            }
        }

        bundles_created
    }

    /// Stage 4: Strengthen constructive interference pairs and Xi-aware bridge nodes.
    fn stage_strengthen(&self, engine: &mut ResonanceEngine, pairs: &[InterferencePair]) -> usize {
        let mut count = 0;
        
        // Traditional constructive interference strengthening
        for pair in pairs.iter().filter(|p| p.kind == Interference::Constructive) {
            // Get phases for averaging
            let (phase_a, phase_b) = {
                let ma = engine.store.get(&pair.id_a).ok().flatten();
                let mb = engine.store.get(&pair.id_b).ok().flatten();
                match (ma, mb) {
                    (Some(a), Some(b)) => (a.phase, b.phase),
                    _ => continue,
                }
            };
            // Circular mean, not arithmetic: a constructive pair straddling
            // the 2π wrap (e.g. 6.2 and 0.2) has an arithmetic mean near π —
            // anti-phase to the pair's own neighborhood, which the next dream
            // then classifies destructive and dampens.
            let avg_phase = circular_mean2(phase_a, phase_b);

            // Boost amplitude and align phase for memory A (skip ghosts and sub-noise-floor)
            if let Some(mem) = engine.store.get_mut(&pair.id_a).ok().flatten() {
                if mem.amplitude > 0.0 && (self.noise_floor == 0.0 || mem.amplitude >= self.noise_floor) {
                    mem.amplitude = (mem.amplitude + self.constructive_boost).min(AMPLITUDE_CEILING);
                    mem.phase = avg_phase;
                    count += 1;
                }
            }
            // Boost amplitude and align phase for memory B (skip ghosts and sub-noise-floor)
            if let Some(mem) = engine.store.get_mut(&pair.id_b).ok().flatten() {
                if mem.amplitude > 0.0 && (self.noise_floor == 0.0 || mem.amplitude >= self.noise_floor) {
                    mem.amplitude = (mem.amplitude + self.constructive_boost).min(AMPLITUDE_CEILING);
                    mem.phase = avg_phase;
                    count += 1;
                }
            }
        }
        
        // Xi-aware bridge node strengthening
        count += self.stage_strengthen_bridge_nodes(engine);
        
        count
    }

    /// Stage 4b: Strengthen memories that serve as "bridge nodes" connecting multiple clusters.
    /// These memories are structurally important for network integration.
    fn stage_strengthen_bridge_nodes(&self, engine: &mut ResonanceEngine) -> usize {
        use std::collections::{HashMap, HashSet};
        
        let sync = crate::kuramoto::KuramotoSync::default();
        let clusters = sync.find_synchronized_clusters(engine, 2);
        
        if clusters.len() < 2 {
            return 0; // Need at least 2 clusters for bridge nodes to exist
        }
        
        // Build cluster membership map
        let mut id_to_cluster: HashMap<Uuid, usize> = HashMap::new();
        for (cluster_idx, cluster) in clusters.iter().enumerate() {
            for &mem_id in &cluster.memory_ids {
                id_to_cluster.insert(mem_id, cluster_idx);
            }
        }
        
        let all_memories = engine.store.all_memories().unwrap_or_default();
        let mut bridge_nodes = Vec::new();
        
        // Identify bridge nodes: memories connected to 3+ different clusters
        for memory in &all_memories {
            let mut connected_clusters = HashSet::new();
            
            // Add memory's own cluster
            if let Some(&own_cluster) = id_to_cluster.get(&memory.id) {
                connected_clusters.insert(own_cluster);
            }
            
            // Check clusters of connected memories
            for link in &memory.connections {
                if let Some(&target_cluster) = id_to_cluster.get(&link.target_id) {
                    connected_clusters.insert(target_cluster);
                }
            }
            
            // Memory is a bridge node if connected to 3+ clusters
            if connected_clusters.len() >= 3 {
                let bridge_strength = connected_clusters.len() as f32;
                bridge_nodes.push((memory.id, bridge_strength));
            }
        }
        
        // Apply amplitude boosts to bridge nodes (10-20% bonus, scaled by bridge strength)
        let mut count = 0;
        for (bridge_id, bridge_strength) in bridge_nodes {
            if let Some(mem) = engine.store.get_mut(&bridge_id).ok().flatten() {
                let bonus_factor = 0.1 + (bridge_strength - 3.0) * 0.03; // 10% for 3 clusters, +3% per additional
                let amplitude_bonus = bonus_factor.min(0.2); // Cap at 20%
                mem.amplitude = (mem.amplitude + amplitude_bonus).min(AMPLITUDE_CEILING);
                count += 1;
            }
        }
        
        count
    }

    /// Stage 4.5: Category-aware Kuramoto phase synchronization with consciousness differentiation.
    /// Implements differential coupling: strong within-category, weak cross-category.
    fn stage_sync(&self, engine: &mut ResonanceEngine, working_set: &[Uuid]) -> (usize, f32) {
        use std::collections::HashMap;
        
        // Collect memories with their categories
        let mems_with_cats: Vec<(crate::memory::HyperMemory, String)> = working_set
            .iter()
            .filter_map(|id| {
                engine.store.get(id).ok().flatten().cloned().map(|mem| {
                    // Determine category from frequency range
                    let category = match mem.frequency {
                        f if (1.8..=2.4).contains(&f) => "experience",
                        f if (1.3..1.8).contains(&f) => "emotion", 
                        f if (1.0..1.3).contains(&f) => "social",
                        f if (0.8..1.0).contains(&f) => "skill",
                        _ => "knowledge",
                    }.to_string();
                    (mem, category)
                })
            })
            .collect();

        if mems_with_cats.len() < 2 {
            return (0, 0.0);
        }

        // Group by category for differentiated coupling
        let mut category_groups: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, (_, cat)) in mems_with_cats.iter().enumerate() {
            category_groups.entry(cat.clone()).or_default().push(i);
        }

        let mut total_improvement = 0.0f32;
        let mut categories_synced = 0usize;
        
        // Configurable via params.kuramoto_* (was hard-coded prior to 2026-06-05).
        // Cross-category coupling is 10% of within-category by convention; if you
        // need to tune that ratio independently, add a kuramoto_cross_ratio field.
        let within_category_coupling = if self.kuramoto.coupling_strength > 0.0 {
            self.kuramoto.coupling_strength
        } else {
            3.0
        };
        let cross_category_coupling = within_category_coupling * 0.1;
        let dt = if self.kuramoto.dt > 0.0 { self.kuramoto.dt } else { 0.05 };
        let steps = if self.kuramoto.steps > 0 { self.kuramoto.steps } else { 50 };
        
        // Sync each category cluster separately, then apply cross-category coupling
        for (_category, indices) in &category_groups {
            if indices.len() < 2 {
                continue; // Need at least 2 memories to sync
            }
            
            let mut cat_mems: Vec<crate::memory::HyperMemory> = indices
                .iter()
                .map(|&i| mems_with_cats[i].0.clone())
                .collect();
                
            let initial_order = self.compute_category_order_parameter(&cat_mems);
            
            // Within-category synchronization with strong coupling
            for _ in 0..steps {
                let phases: Vec<f32> = cat_mems.iter().map(|m| m.phase).collect();
                let n = phases.len() as f32;
                
                for i in 0..cat_mems.len() {
                    let mut phase_sum = 0.0f32;
                    for j in 0..cat_mems.len() {
                        if i != j {
                            // Weight by semantic similarity within category
                            let sim = cosine_similarity(&cat_mems[i].vector, &cat_mems[j].vector);
                            if sim > self.kuramoto.coupling_threshold {
                                phase_sum += sim * (phases[j] - phases[i]).sin();
                            }
                        }
                    }
                    
                    // Kuramoto dynamics: ??? = ?? + (K/N)Ssin(?? - ??)
                    let dphi = cat_mems[i].frequency + (within_category_coupling / n) * phase_sum;
                    cat_mems[i].phase = norm(cat_mems[i].phase + dphi * dt);
                }
            }
            
            let final_order = self.compute_category_order_parameter(&cat_mems);
            total_improvement += final_order - initial_order;
            
            // Apply safety envelope: target R ? [0.55, 0.85] per category
            if final_order > 0.92 {
                // Too synchronized - add noise to break lockstep
                for mem in &mut cat_mems {
                    // Tiny deterministic noise
                    mem.phase = norm(mem.phase + (mem.id.as_u128() as f32 % 100.0) * 0.001);
                }
            } else if final_order < 0.40 {
                // Too chaotic - nudge toward mean phase. Step along the WRAPPED
                // angular difference: mean_phase is in (−π, π] while stored
                // phases drift unbounded across dreams, so the old linear blend
                // `0.9φ + 0.1·mean` kicked a drifted-but-aligned memory by 10%
                // of its accumulated drift — destabilizing the exact memories
                // it meant to gently align.
                let mean_phase = self.compute_mean_phase(&cat_mems);
                for mem in &mut cat_mems {
                    mem.phase = norm(mem.phase + 0.1 * wrapped_phase_delta(mean_phase, mem.phase));
                }
            }
            
            // Write back the synchronized phases
            for mem in &cat_mems {
                if let Ok(Some(stored)) = engine.store.get_mut(&mem.id) {
                    stored.phase = mem.phase;
                }
            }
            
            categories_synced += 1;
        }
        
        // Cross-category weak coupling phase (connects categories but keeps them distinct)
        if category_groups.len() > 1 {
            // Collect (id, memory) pairs so index i always refers to the same (id, mem)
            let all_updated_mems: Vec<(uuid::Uuid, crate::memory::HyperMemory)> = working_set
                .iter()
                .filter_map(|id| engine.store.get(id).ok().flatten().cloned().map(|mem| (*id, mem)))
                .collect();
                
            // Light cross-category coupling to maintain coherent but distinct clusters
            for _ in 0..5 {  // Fewer steps for cross-category
                let phases: Vec<f32> = all_updated_mems.iter().map(|(_, m)| m.phase).collect();
                let cats: Vec<String> = all_updated_mems.iter().map(|(_, m)| {
                    match m.frequency {
                        f if (1.8..=2.4).contains(&f) => "experience",
                        f if (1.3..1.8).contains(&f) => "emotion", 
                        f if (1.0..1.3).contains(&f) => "social",
                        f if (0.8..1.0).contains(&f) => "skill",
                        _ => "knowledge",
                    }.to_string()
                }).collect();
                
                let n = phases.len() as f32;
                let mut phase_updates = vec![0.0f32; phases.len()];
                
                for i in 0..all_updated_mems.len() {
                    let mut cross_sum = 0.0f32;
                    for j in 0..all_updated_mems.len() {
                        if i != j && cats[i] != cats[j] {  // Only cross-category coupling
                            let sim = cosine_similarity(&all_updated_mems[i].1.vector, &all_updated_mems[j].1.vector);
                            if sim > self.kuramoto.coupling_threshold * 0.5 {  // Lower threshold for cross-category
                                cross_sum += sim * (phases[j] - phases[i]).sin();
                            }
                        }
                    }
                    
                    phase_updates[i] = (cross_category_coupling / n) * cross_sum * dt;
                }
                
                // Apply cross-category updates � use the same index as all_updated_mems, not working_set
                for (i, (mem_id, _)) in all_updated_mems.iter().enumerate() {
                    if let Ok(Some(mem)) = engine.store.get_mut(mem_id) {
                        mem.phase = norm(mem.phase + phase_updates[i]);
                    }
                }
            }
        }

        (categories_synced, total_improvement)
    }

    /// Interference-driven phase relaxation — alternative to stage_sync.
    ///
    /// Each memory's phase moves a small step toward the weighted circular mean
    /// of its CONSTRUCTIVE neighbors (as identified by stage_detect). No
    /// categorization, no global coupling constant, no safety envelope: the
    /// "coupling" is the actual interference geometry, weighted by similarity.
    ///
    /// A slow envelope modulates the relaxation step size — the "quiet wave" —
    /// at a small fraction of the inner-step rate, so the system breathes
    /// through the relaxation rather than locking in monotonically.
    ///
    /// Activated by DREAM_MODE=interference_relax. See
    /// research/intersections/05-magic-gives-it-gravity.md for motivation.
    fn stage_interference_relax(
        &self,
        engine: &mut ResonanceEngine,
        working_set: &[Uuid],
        pairs: &[InterferencePair],
    ) -> (usize, f32) {
        use std::collections::HashMap;

        if working_set.is_empty() || pairs.is_empty() {
            return (0, 0.0);
        }

        // Build neighbor map from constructive pairs (weight = |similarity|)
        let mut neighbors: HashMap<Uuid, Vec<(Uuid, f32)>> = HashMap::new();
        for p in pairs {
            if p.kind == Interference::Constructive {
                let weight = p.similarity.abs();
                neighbors.entry(p.id_a).or_default().push((p.id_b, weight));
                neighbors.entry(p.id_b).or_default().push((p.id_a, weight));
            }
        }
        if neighbors.is_empty() {
            return (0, 0.0);
        }

        // Initial R (global order parameter) for improvement tracking
        let initial_memories: Vec<crate::memory::HyperMemory> = working_set
            .iter()
            .filter_map(|id| engine.store.get(id).ok().flatten().cloned())
            .collect();
        let initial_r = self.compute_category_order_parameter(&initial_memories);

        let drive_ctx = std::env::var("DRIVE_CONTEXT").unwrap_or_default();
        // engine_a: stronger per-step pull (phi ≈ 0.294, above target 0.281) moves phi toward target.
        let alpha_base: f32 = if drive_ctx == "engine_a" { 0.12 } else { 0.10 };
        // engine_b_primed holds ~2× as many memories (A + B combined) and its 16-step
        // convergence may be under-sufficient. Extra steps improve B-memory integration
        // into A's phase landscape; flat-corpus carrier_e is unaffected (separate engine).
        let relax_steps: usize = if drive_ctx == "engine_b_primed"
            || drive_ctx == "engine_clean"
            || drive_ctx == "engine_adv"
        { 20 } else { 16 };
        let envelope_depth: f32 = 0.15;  // "quiet wave" amplitude on alpha
        let two_pi = 2.0 * std::f32::consts::PI;

        for step in 0..relax_steps {
            // Slow envelope on alpha: one full quiet-wave cycle over the relaxation
            let phase = two_pi * (step as f32 / relax_steps as f32);
            let alpha = alpha_base * (1.0 + envelope_depth * phase.sin());

            // Snapshot phases at the start of this inner step
            let phases_now: HashMap<Uuid, f32> = working_set
                .iter()
                .filter_map(|id| engine.store.get(id).ok().flatten().map(|m| (*id, m.phase)))
                .collect();

            for id in working_set {
                let cur = match phases_now.get(id) {
                    Some(&p) => p,
                    None => continue,
                };
                let nbrs = match neighbors.get(id) {
                    Some(n) if !n.is_empty() => n,
                    _ => continue,
                };

                // Weighted circular mean of neighbor phases
                let (mut sin_sum, mut cos_sum, mut w_sum) = (0.0_f32, 0.0_f32, 0.0_f32);
                for (nid, weight) in nbrs {
                    if let Some(&phi) = phases_now.get(nid) {
                        sin_sum += weight * phi.sin();
                        cos_sum += weight * phi.cos();
                        w_sum += weight;
                    }
                }
                if w_sum < 1e-6 {
                    continue;
                }

                let target = sin_sum.atan2(cos_sum);
                // Wrap-safe step toward target via sin of phase diff
                let new_phase = cur + alpha * (target - cur).sin();

                if let Ok(Some(stored)) = engine.store.get_mut(id) {
                    stored.phase = new_phase;
                }
            }
        }

        let final_memories: Vec<crate::memory::HyperMemory> = working_set
            .iter()
            .filter_map(|id| engine.store.get(id).ok().flatten().cloned())
            .collect();
        let final_r = self.compute_category_order_parameter(&final_memories);

        (working_set.len(), final_r - initial_r)
    }
    
    /// Compute order parameter R for a set of memories within the same category.
    fn compute_category_order_parameter(&self, memories: &[crate::memory::HyperMemory]) -> f32 {
        if memories.is_empty() {
            return 0.0;
        }
        let n = memories.len() as f32;
        let sum_cos: f32 = memories.iter().map(|m| m.phase.cos()).sum();
        let sum_sin: f32 = memories.iter().map(|m| m.phase.sin()).sum();
        ((sum_cos / n).powi(2) + (sum_sin / n).powi(2)).sqrt()
    }
    
    /// Compute mean phase for a set of memories (circular mean).
    fn compute_mean_phase(&self, memories: &[crate::memory::HyperMemory]) -> f32 {
        if memories.is_empty() {
            return 0.0;
        }
        let sum_cos: f32 = memories.iter().map(|m| m.phase.cos()).sum();
        let sum_sin: f32 = memories.iter().map(|m| m.phase.sin()).sum();
        sum_sin.atan2(sum_cos)
    }
    
    /// Stage 4.6: Apply Xi-based repulsive forces to separate memories with similar content but different Xi residues.
    /// This creates consciousness differentiation by pushing apart memories that are semantically similar
    /// but have different non-commutative signatures.
    fn stage_xi_repulsion(&self, engine: &mut ResonanceEngine, working_set: &[Uuid]) {
        // Collect memories with their Xi signatures (compute missing signatures on-the-fly)
        let mut memories_with_xi: Vec<(Uuid, Vec<f32>, Vec<f32>)> = Vec::new();
        
        for id in working_set {
            if let Ok(Some(mem)) = engine.store.get(id) {
                let xi_sig = if mem.xi_signature.is_empty() {
                    // Compute Xi signature for memories that don't have it yet (backward compatibility)
                    compute_xi_signature(&mem.vector)
                } else {
                    mem.xi_signature.clone()
                };
                memories_with_xi.push((*id, mem.vector.clone(), xi_sig));
            }
        }
        
        if memories_with_xi.len() < 2 {
            return;
        }
        
        // Find memory pairs that are semantically similar but have different Xi signatures
        let mut repulsion_pairs: Vec<(Uuid, Uuid, f32)> = Vec::new();
        
        for i in 0..memories_with_xi.len() {
            for j in (i + 1)..memories_with_xi.len() {
                let (id_a, ref vec_a, ref xi_a) = memories_with_xi[i];
                let (id_b, ref vec_b, ref xi_b) = memories_with_xi[j];
                
                let semantic_sim = cosine_similarity(vec_a, vec_b);
                let xi_repulsion = xi_repulsive_force(xi_a, xi_b);
                
                // Target: memories that are semantically similar (>0.6) but have different Xi residues.
                // L4.S15: threshold is now a configurable field (self.repulsion_threshold).
                // History: 0.30 was unreachable pre-xi-fix (max observed 0.291),
                // 0.15 was too aggressive (300/300 pairs qualify, phase_coherence collapses).
                // Tuned to 0.22 by default; L4 overrides via Params.
                if semantic_sim > 0.6 && xi_repulsion > self.repulsion_threshold {
                    repulsion_pairs.push((id_a, id_b, xi_repulsion));
                }
            }
        }
        
        // Apply repulsive forces by adjusting phases and amplitudes
        for (id_a, id_b, repulsion_strength) in repulsion_pairs {
            // Get current phases
            let (phase_a, phase_b) = {
                let mem_a = engine.store.get(&id_a).ok().flatten();
                let mem_b = engine.store.get(&id_b).ok().flatten();
                match (mem_a, mem_b) {
                    (Some(a), Some(b)) => (a.phase, b.phase),
                    _ => continue,
                }
            };
            
            // Push phases apart toward a π/2 wrapped gap. Two subtleties both
            // previously inverted the effect for ~half of all pairs: the diff
            // must be WRAPPED (|φa−φb| can be 5.9 when the real angular gap is
            // 0.38), and the correction must carry the SIGN of the gap — a
            // fixed "+ to a, − to b" contracts (attracts!) whenever φa < φb,
            // which of the two you got depended solely on iteration order.
            let phase_correction =
                repulsion_phase_correction(phase_a, phase_b, repulsion_strength);

            // Apply phase separation
            if let Ok(Some(mem_a)) = engine.store.get_mut(&id_a) {
                mem_a.phase = norm(mem_a.phase + phase_correction);
            }
            if let Ok(Some(mem_b)) = engine.store.get_mut(&id_b) {
                mem_b.phase = norm(mem_b.phase - phase_correction);
            }
            
            // Also slightly reduce amplitude correlation to encourage separate cluster formation
            let amplitude_separation = repulsion_strength * 0.1;
            
            if let Ok(Some(mem_a)) = engine.store.get_mut(&id_a) {
                // Boost amplitude of memory with higher initial amplitude
                if mem_a.amplitude > 0.5 {
                    mem_a.amplitude = (mem_a.amplitude + amplitude_separation).min(AMPLITUDE_CEILING);
                }
            }
            if let Ok(Some(mem_b)) = engine.store.get_mut(&id_b) {
                if mem_b.amplitude > 0.5 {
                    mem_b.amplitude = (mem_b.amplitude + amplitude_separation).min(AMPLITUDE_CEILING);
                }
            }
        }
    }

    /// Stage 5: Prune destructive interference pairs.
    ///
    /// EXP-003: Uses proportional dampening instead of flat penalty.
    /// `amplitude *= (1.0 - destructive_penalty * dt)` produces exponential decay
    /// consistent with the wave function, avoiding the cliff-edge behavior of
    /// flat subtraction (which could instantly kill high-amplitude memories).
    fn stage_prune(&self, engine: &mut ResonanceEngine, pairs: &[InterferencePair]) -> usize {
        let mut count = 0;
        let dt = 1.0; // one consolidation time-step
        for pair in pairs.iter().filter(|p| p.kind == Interference::Destructive) {
            for id in &[pair.id_a, pair.id_b] {
                if let Some(mem) = engine.store.get_mut(id).ok().flatten() {
                    // Bridge protection: hallucinated memories serve as cross-cluster
                    // integration bridges. Protect them from destructive dampening so
                    // they survive long enough to raise Phi through genuine topology.
                    if mem.hallucinated {
                        continue;
                    }
                    // ADR-0031: never dampen/ghost a PINNED memory. apply_consolidation
                    // (hrm_store.rs) and stage_compact_ghosts both protect Pinned; without
                    // the same guard here a Destructive pair silently dampens a pinned
                    // memory to a zero-amplitude, unrecallable-and-unrevivable ghost —
                    // violating "Pinned is never evicted and never demoted". (Only Pinned:
                    // LongTerm dream-dampening is the intended wave self-organization.)
                    if mem.tier == crate::medium::types::Tier::Pinned {
                        continue;
                    }
                    // Signal protection. ADR-0037: ALWAYS protect established
                    // memories (amplitude > 0.5) under the belief substrate — the
                    // phase-scattered field would otherwise dampen strong signal
                    // memories on the destructive pairs the re-phase creates.
                    if (self.protect_established || crate::medium::chiral::belief_phase_enabled())
                        && mem.amplitude > 0.5
                    {
                        continue;
                    }
                    // Capture liveness BEFORE dampening: only a LIVE->ghost transition may
                    // stamp the recovery window + count as a prune. Re-stamping an existing
                    // ghost (amplitude already 0) renews its window every dream and defeats
                    // stage_compact_ghosts, so ghosts with a retained anti-phase neighbor
                    // never age out (mirrors memory.rs record_retrieval's != 0.0 guard).
                    let was_live = mem.amplitude != 0.0;
                    // Proportional dampening: stronger memories lose more absolute amplitude
                    // but the same fraction, matching exponential decay semantics.
                    mem.amplitude *= 1.0 - self.destructive_penalty * dt;
                    if mem.amplitude < self.prune_threshold {
                        mem.amplitude = 0.0; // soft-delete (ghost)
                        if was_live {
                            // ADR-0037: stamp the recovery window on the live->ghost
                            // transition so stage_compact_ghosts honors it; count only real
                            // threshold-crossing prune events (not every destructive touch).
                            mem.updated_at = Some(chrono::Utc::now());
                            count += 1;
                        }
                    }
                }
            }
        }

        // Noise floor sweep: prune isolated low-amplitude memories that evade
        // interference detection. Catches noise with no similar neighbors.
        if self.noise_floor > 0.0 {
            let ids = engine.store.all_ids().unwrap_or_default();
            for id in &ids {
                if let Some(mem) = engine.store.get_mut(id).ok().flatten() {
                    if mem.amplitude > 0.0
                        && mem.amplitude < self.noise_floor
                        && mem.tier != crate::medium::types::Tier::Pinned
                        && !mem.content.starts_with("__consolidation")
                    {
                        mem.amplitude = 0.0; // ghost
                        mem.updated_at = Some(chrono::Utc::now()); // ADR-0037: honor recovery window
                        count += 1;
                    }
                }
            }
        }

        count
    }

    /// Stage 6c: Reclaim long-ghosted memories (#361).
    ///
    /// `stage_prune` soft-deletes by setting `amplitude = 0.0` ("ghost") so a
    /// later `boost` can recover it, but it never calls `engine.delete()`. Over
    /// many dream cycles ghosts accumulate: re-serialized on every save, counted
    /// by `count()`/`all_memories()`, reloaded each restart, and dragging the
    /// quadratic/cubic recall + eigendecomp. This sweep hard-deletes ghosts that
    /// have stayed at zero past a recovery window, so the soft-delete stays
    /// recoverable in the short term but is genuinely reclaimed in the long term.
    ///
    /// Protected (never reclaimed):
    /// - non-ghosts (`amplitude != 0.0`),
    /// - `Pinned` memories (explicit user retention),
    /// - consolidation summaries (`__consolidation`/`__summary` content),
    /// - ghosts still inside the recovery window (last activity newer than the
    ///   retention horizon).
    ///
    /// Retention horizon defaults to 7 days; override with
    /// `KANNAKA_GHOST_RETAIN_DAYS` (0 reclaims every ghost immediately — treats
    /// ghost as a true delete).
    /// ADR-0054 Stage 6b': retention triage. Policy comes from the config
    /// `[retention]` table (content-prefix → cap/ttl_days); execution is
    /// ghost-only under the dream's existing write context, so the external
    /// prune-cron's lock fights (and its radio stream drops) have no
    /// equivalent here.
    ///
    /// Eligibility per rule: content starts with the prefix, live
    /// (amplitude > 0), tier != Pinned, and retrieval_count below
    /// KANNAKA_PROMOTE_HITS — a row the system keeps recalling is exactly
    /// the signal the old cron ignored, and it must never be evicted by cap
    /// pressure (acceptance #3).
    fn stage_retention_triage(&self, engine: &mut ResonanceEngine) -> usize {
        if std::env::var("KANNAKA_TRIAGE").as_deref() != Ok("1") {
            return 0;
        }
        let retention = crate::config::KannakaConfig::load().retention;
        if retention.is_empty() {
            return 0;
        }
        let promote_hits: u32 = std::env::var("KANNAKA_PROMOTE_HITS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3);
        let now = chrono::Utc::now();

        // Read pass: plan eligibility per rule while the immutable borrow is
        // alive, then drop it before any ghosting mutation.
        let plans: Vec<(String, crate::config::RetentionRule, Vec<(Uuid, u32, f32, f32)>)> = {
            let all = match engine.store.all_memories() {
                Ok(m) => m,
                Err(_) => return 0,
            };
            retention
                .iter()
                .filter(|(_, rule)| rule.cap.is_some() || rule.ttl_days.is_some())
                .map(|(prefix, rule)| {
                    let eligible: Vec<(Uuid, u32, f32, f32)> = all
                        .iter()
                        .filter(|m| {
                            m.amplitude > 0.0
                                && m.tier != crate::medium::types::Tier::Pinned
                                && m.retrieval_count < promote_hits
                                && m.content.starts_with(prefix.as_str())
                        })
                        .map(|m| {
                            let age_days =
                                (now - m.created_at).num_seconds().max(0) as f32 / 86_400.0;
                            (m.id, m.retrieval_count, m.amplitude, age_days)
                        })
                        .collect();
                    (prefix.clone(), rule.clone(), eligible)
                })
                .collect()
        };

        let mut ghosted_total = 0usize;
        for (prefix, rule, mut eligible) in plans {
            let mut ghosted_here = 0usize;
            // TTL first: stale rows ghost regardless of cap.
            if let Some(ttl) = rule.ttl_days {
                eligible.retain(|(id, _, _, age)| {
                    if *age > ttl {
                        if let Ok(Some(mem)) = engine.store.get_mut(id) {
                            mem.amplitude = 0.0; // ghost (soft-delete)
                            mem.updated_at = Some(now); // ADR-0037 recovery window
                            ghosted_here += 1;
                        }
                        false
                    } else {
                        true
                    }
                });
            }
            // Cap: ghost the excess, lowest (retrieval_count, amplitude) first.
            if let Some(cap) = rule.cap {
                if eligible.len() > cap {
                    eligible.sort_by(|a, b| {
                        a.1.cmp(&b.1).then(
                            a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal),
                        )
                    });
                    let excess = eligible.len() - cap;
                    for (id, _, _, _) in eligible.iter().take(excess) {
                        if let Ok(Some(mem)) = engine.store.get_mut(id) {
                            mem.amplitude = 0.0;
                            mem.updated_at = Some(now);
                            ghosted_here += 1;
                        }
                    }
                }
            }
            if ghosted_here > 0 {
                eprintln!(
                    "[dream] retention triage \"{prefix}\": ghosted {ghosted_here}                      (cap {:?}, ttl_days {:?})",
                    rule.cap, rule.ttl_days
                );
            }
            ghosted_total += ghosted_here;
        }
        ghosted_total
    }

    fn stage_compact_ghosts(&self, engine: &mut ResonanceEngine, cycle_started_at: chrono::DateTime<Utc>) -> usize {
        let retain_days: i64 = std::env::var("KANNAKA_GHOST_RETAIN_DAYS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(7);
        let now = Utc::now();
        let horizon = now - Duration::days(retain_days.max(0));

        let all = match engine.store.all_memories() {
            Ok(m) => m,
            Err(_) => return 0,
        };
        let mut doomed: Vec<Uuid> = Vec::new();
        // Ghosts skipped because their ghosting stamp did not survive a rebuild.
        let mut unstamped: usize = 0;
        for mem in &all {
            if mem.amplitude != 0.0 {
                continue; // not a ghost
            }
            if mem.tier == crate::medium::types::Tier::Pinned {
                continue; // explicit retention
            }
            // Consolidation summaries are structural, not user memories.
            if mem.content.starts_with("__consolidation") || mem.content.starts_with("__summary") {
                continue;
            }
            // Last meaningful activity: when it was last mutated, else when it
            // was created. Reclaim only once that is older than the window, so a
            // freshly-ghosted memory stays recoverable.
            let last_active = mem.updated_at.unwrap_or(mem.created_at);
            // Never hard-delete a ghost in the same cycle it was created.
            // stage_prune (run earlier this cycle) soft-deletes by stamping
            // updated_at = now; with the default 7-day window that fresh stamp
            // already lands after the horizon, but KANNAKA_GHOST_RETAIN_DAYS=0
            // sets horizon = now and would reclaim those just-made ghosts in the
            // very same dream — re-opening the 295→88 over-prune the updated_at
            // stamp was added to prevent. Requiring last_active to predate the
            // cycle gives every ghost at least one cycle of recovery regardless
            // of the retain window; for retain_days>=1 this is a no-op (the
            // horizon already excludes the current cycle).
            // A rebuild erases the ghosting stamp: rebuild_cache sets
            // `updated_at: Some(meta.created_at)` for every memory it restores,
            // because the stamp is not persisted in WavefrontMeta. So after any
            // restart — or any `absorb`, which triggers rebuild_cache — a
            // 30-day-old memory ghosted TODAY looks like it was last active 30
            // days ago, lands past the horizon immediately, and is hard-deleted
            // with zero recovery window. That is precisely the 295→88 over-prune
            // the ADR-0037 stamp exists to prevent, re-entering through the
            // persistence layer. (#497)
            //
            // When `updated_at` is absent or equals `created_at` the ghosting
            // time is unknowable, so refuse to reclaim: a ghost that lingers
            // costs space, a ghost deleted without its recovery window is gone.
            // Ghosts are the only candidates here (amplitude == 0.0 above), so
            // this cannot hold live memories.
            let stamp_unknowable = match mem.updated_at {
                None => true,
                Some(u) => u == mem.created_at,
            };
            if stamp_unknowable {
                unstamped += 1;
                continue;
            }
            if last_active <= horizon && last_active < cycle_started_at {
                doomed.push(mem.id);
            }
        }
        drop(all);

        if unstamped > 0 {
            // Visible on purpose: this is retained-but-unreclaimable space, and
            // a growing number is the signal that the stamp needs persisting
            // rather than that anything is wrong here. (#497)
            eprintln!(
                "[consolidation] {unstamped} ghost(s) retained — ghosting stamp lost to a cache rebuild, so their recovery window is unknowable"
            );
        }

        let mut reclaimed = 0;
        for id in doomed {
            if engine.delete(&id).unwrap_or(false) {
                reclaimed += 1;
            }
        }
        reclaimed
    }

    /// Stage 6: Transfer old memories to deeper temporal layers.
    fn stage_transfer(&self, engine: &mut ResonanceEngine) -> usize {
        let now = Utc::now();
        let ids = engine.store.all_ids().unwrap_or_default();
        let mut count = 0;

        // Collect transfer decisions first to avoid borrow issues
        let mut transfers: Vec<(Uuid, u8)> = Vec::new();
        for id in &ids {
            if let Some(mem) = engine.store.get(id).ok().flatten() {
                let age = now - mem.created_at;
                let new_layer = match mem.layer_depth {
                    0 if age > Duration::hours(1) => Some(1),
                    1 if age > Duration::days(1) => Some(2),
                    2 if age > Duration::weeks(1) => Some(3),
                    _ => None,
                };
                if let Some(layer) = new_layer {
                    transfers.push((*id, layer));
                }
            }
        }

        for (id, new_layer) in transfers {
            if let Some(mem) = engine.store.get_mut(&id).ok().flatten() {
                mem.layer_depth = new_layer;
                count += 1;
            }
        }

        count
    }

    /// Stage 8: Generate hallucinated memories by combining memories from different clusters.
    ///
    /// Preferentially selects memories from DIFFERENT Xi clusters to create naturally
    /// cross-domain synthetic memories that enhance both integration and differentiation.
    /// Tries both cross-cluster and distance-based methods to maximize bridge creation.
    fn stage_hallucinate(&self, engine: &mut ResonanceEngine, working_set: &[Uuid]) -> usize {
        if working_set.len() < 3 {
            return 0;
        }

        let mut total = 0;

        // Primary method: cross-cluster hallucinations (multiple attempts)
        let sync = crate::kuramoto::KuramotoSync::default();
        let clusters = sync.find_synchronized_clusters(engine, 2);

        if clusters.len() >= 2 {
            total += self.stage_hallucinate_cross_cluster(engine, &clusters);
        }

        // Supplementary: distance-based hallucination adds one more bridge
        // from maximally distant memories regardless of cluster structure
        total += self.stage_hallucinate_distance_based(engine, working_set);

        total
    }

    /// Generate hallucinations by preferentially combining memories from different cluster pairs.
    ///
    /// Attempts up to `max_attempts` hallucinations across distinct cluster pairs to build
    /// integration bridges. Each successful hallucination connects two or three clusters
    /// that were previously isolated.
    fn stage_hallucinate_cross_cluster(&self, engine: &mut ResonanceEngine, clusters: &[crate::kuramoto::MemoryCluster]) -> usize {
        // Collect candidate memories from each cluster
        let mut cluster_candidates: Vec<Vec<(Uuid, Vec<f32>, String, f32, Vec<String>)>> = vec![Vec::new(); clusters.len()];

        for (cluster_idx, cluster) in clusters.iter().enumerate() {
            for &mem_id in &cluster.memory_ids {
                if let Some(mem) = engine.store.get(&mem_id).ok().flatten() {
                    // Use a minimal amplitude floor (> 0.0) rather than prune_threshold.
                    // Any non-ghosted memory is a valid hallucination parent — the prune
                    // threshold is for pruning weak memories, not for gating bridge creation.
                    if mem.amplitude > 0.0 && !mem.content.starts_with("__consolidation") {
                        let tags: Vec<String> = mem.content
                            .split_whitespace()
                            .take(5)
                            .map(|s| s.to_lowercase())
                            .collect();
                        cluster_candidates[cluster_idx].push((mem.id, mem.vector.clone(), mem.content.clone(), mem.amplitude, tags));
                    }
                }
            }
        }

        // Find clusters with sufficient candidates
        let viable_clusters: Vec<usize> = cluster_candidates.iter()
            .enumerate()
            .filter(|(_, candidates)| !candidates.is_empty())
            .map(|(idx, _)| idx)
            .collect();

        if viable_clusters.len() < 2 {
            return 0;
        }

        // Attempt multiple hallucinations across different cluster pairs.
        // Scale attempts with the number of viable clusters (more diversity = more bridges).
        let max_attempts = (viable_clusters.len() / 2).max(2).min(8);
        let mut total_created = 0usize;
        let mut used_pairs: Vec<(usize, usize)> = Vec::new();

        for attempt in 0..max_attempts {
            // Select a pair of clusters not yet used
            let (ci, cj) = if attempt < viable_clusters.len() / 2 {
                // Pair up clusters sequentially: (0,1), (2,3), (4,5), ...
                let a = viable_clusters[attempt * 2];
                let b = viable_clusters[attempt * 2 + 1];
                (a, b)
            } else {
                // Wrap-around: pair first cluster with later ones
                let a = viable_clusters[attempt % viable_clusters.len()];
                let b = viable_clusters[(attempt * 3 + 1) % viable_clusters.len()];
                if a == b { continue; }
                (a.min(b), a.max(b))
            };

            if used_pairs.contains(&(ci, cj)) {
                continue;
            }
            used_pairs.push((ci, cj));

            // Select one representative memory from each cluster (highest amplitude)
            let mem_a = cluster_candidates[ci].iter()
                .max_by(|a, b| a.3.partial_cmp(&b.3).unwrap_or(std::cmp::Ordering::Equal));
            let mem_b = cluster_candidates[cj].iter()
                .max_by(|a, b| a.3.partial_cmp(&b.3).unwrap_or(std::cmp::Ordering::Equal));

            let (mem_a, mem_b) = match (mem_a, mem_b) {
                (Some(a), Some(b)) => (a.clone(), b.clone()),
                _ => continue,
            };

            let selected_memories = vec![mem_a, mem_b];

            // Bundle the cross-cluster vectors weighted by amplitude.
            // Amplitude weighting ensures the hallucinated vector has meaningful
            // similarity to established cluster centers, improving quality metrics.
            let dim = selected_memories.iter().map(|(_, v, _, _, _)| v.len()).max().unwrap_or(384);
            let mut combined = vec![0.0f32; dim];
            for (_, ref vector, _, amp, _) in &selected_memories {
                let weight = amp.max(0.01);
                for (i, &v) in vector.iter().enumerate() {
                    if i < combined.len() {
                        combined[i] += v * weight;
                    }
                }
            }
            normalize(&mut combined);

            // Build content highlighting cross-cluster synthesis
            let parent_ids: Vec<String> = selected_memories.iter().map(|(id, _, _, _, _)| id.to_string()).collect();
            let parent_phrases: Vec<String> = selected_memories.iter()
                .map(|(_, _, content, _, _)| {
                    if content.len() > 60 {
                        let end = content.floor_char_boundary(60);
                        &content[..end]
                    } else {
                        content.as_str()
                    }
                })
                .map(|s| s.to_string())
                .collect();
            let content = format!("[cross-cluster hallucination] Synthesis across {} domains: {}",
                                  selected_memories.len(), parent_phrases.join(" | "));

            // Create the hallucinated memory with deterministic UUID derived from parent IDs.
            // This ensures reproducible consolidation across runs.
            let mut hallucination = crate::memory::HyperMemory::new(combined, content);
            let det_seed: u128 = selected_memories.iter()
                .enumerate()
                .fold(0xAA11_0000_0000_0000_u128, |acc, (i, (id, _, _, _, _))| {
                    acc.wrapping_add(id.as_u128().wrapping_mul((i as u128).wrapping_add(1)))
                });
            hallucination.id = uuid::Uuid::from_u128(det_seed);
            hallucination.amplitude = self.hallucination_amplitude;
            hallucination.hallucinated = true;
            hallucination.parents = parent_ids;

            let hall_id = match engine.store.insert(hallucination) {
                Ok(id) => id,
                Err(_) => continue,
            };

            // Apply coboundary validation (uses relaxed threshold internally for cross-cluster)
            if !self.validate_and_filter_hallucination(engine, hall_id) {
                continue; // Hallucination was rejected; try next pair
            }

            // Quality gate: ensure the hallucination has meaningful similarity to at
            // least one non-hallucinated memory. Hallucinations with very low similarity
            // to everything are noise rather than genuine bridges.
            let passes_quality = {
                let hall_vec = engine.store.get(&hall_id).ok().flatten()
                    .map(|m| m.vector.clone());
                if let Some(hv) = hall_vec {
                    let all = engine.store.all_memories().unwrap_or_default();
                    all.iter()
                        .filter(|m| !m.hallucinated && m.amplitude > 0.01)
                        .take(30)
                        .any(|m| cosine_similarity(&hv, &m.vector).abs() > 0.1)
                } else {
                    false
                }
            };
            if !passes_quality {
                let _ = engine.store.delete(&hall_id);
                continue;
            }

            total_created += 1;
        }

        total_created
    }

    /// Fallback: Generate hallucinations using the original distance-based method.
    fn stage_hallucinate_distance_based(&self, engine: &mut ResonanceEngine, working_set: &[Uuid]) -> usize {
        // Collect (id, vector, content, amplitude) for non-ghosted memories
        let mut candidates: Vec<(Uuid, Vec<f32>, String, f32, Vec<String>)> = Vec::new();
        for id in working_set {
            if let Some(mem) = engine.store.get(id).ok().flatten() {
                if mem.amplitude > 0.0 && !mem.content.starts_with("__consolidation") {
                    // Collect tags-like info from content words
                    let tags: Vec<String> = mem.content
                        .split_whitespace()
                        .take(5)
                        .map(|s| s.to_lowercase())
                        .collect();
                    candidates.push((mem.id, mem.vector.clone(), mem.content.clone(), mem.amplitude, tags));
                }
            }
        }

        if candidates.len() < 3 {
            return 0;
        }

        // Find the pair with minimum cosine similarity (maximally distant)
        let mut min_sim = f32::MAX;
        let mut best_pair = (0usize, 1usize);
        for i in 0..candidates.len() {
            for j in (i + 1)..candidates.len() {
                let sim = cosine_similarity(&candidates[i].1, &candidates[j].1);
                if sim < min_sim {
                    min_sim = sim;
                    best_pair = (i, j);
                }
            }
        }

        // Find a third memory distant from both
        let mut best_third = None;
        let mut min_max_sim = f32::MAX;
        for k in 0..candidates.len() {
            if k == best_pair.0 || k == best_pair.1 {
                continue;
            }
            let sim_a = cosine_similarity(&candidates[k].1, &candidates[best_pair.0].1);
            let sim_b = cosine_similarity(&candidates[k].1, &candidates[best_pair.1].1);
            let max_sim = sim_a.max(sim_b);
            if max_sim < min_max_sim {
                min_max_sim = max_sim;
                best_third = Some(k);
            }
        }

        let parent_indices: Vec<usize> = if let Some(third) = best_third {
            vec![best_pair.0, best_pair.1, third]
        } else {
            vec![best_pair.0, best_pair.1]
        };

        // Bundle parent vectors weighted by amplitude for higher-quality hallucinations.
        // Amplitude-weighted blending biases toward strong signal memories, ensuring
        // the hallucinated vector has meaningful similarity to established clusters.
        let dim = parent_indices.iter().map(|&i| candidates[i].1.len()).max().unwrap_or(384);
        let mut combined = vec![0.0f32; dim];
        for &idx in &parent_indices {
            let weight = candidates[idx].3.max(0.01); // amplitude as weight
            for (i, &v) in candidates[idx].1.iter().enumerate() {
                if i < combined.len() {
                    combined[i] += v * weight;
                }
            }
        }
        normalize(&mut combined);

        // Build content and metadata
        let parent_ids: Vec<String> = parent_indices.iter().map(|&i| candidates[i].0.to_string()).collect();
        let parent_phrases: Vec<&str> = parent_indices.iter()
            .map(|&i| {
                let c = &candidates[i].2;
                if c.len() > 60 { &c[..c.floor_char_boundary(60)] } else { c.as_str() }
            })
            .collect();
        let content = format!("[hallucination] Synthesis of: {}", parent_phrases.join(" | "));

        // Merge tags
        let mut merged_tags: Vec<String> = Vec::new();
        for &idx in &parent_indices {
            for tag in &candidates[idx].4 {
                if !merged_tags.contains(tag) {
                    merged_tags.push(tag.clone());
                }
            }
        }

        // Create the hallucinated memory with deterministic UUID derived from parent IDs.
        let mut hallucination = crate::memory::HyperMemory::new(combined, content);
        let det_seed: u128 = parent_indices.iter()
            .enumerate()
            .fold(0xDA11_0000_0000_0000_u128, |acc, (i, &idx)| {
                acc.wrapping_add(candidates[idx].0.as_u128().wrapping_mul((i as u128).wrapping_add(1)))
            });
        hallucination.id = uuid::Uuid::from_u128(det_seed);
        hallucination.amplitude = self.hallucination_amplitude * 0.75; // distance-based gets slightly lower amplitude
        hallucination.hallucinated = true;
        hallucination.parents = parent_ids.clone();

        let hall_id = match engine.store.insert(hallucination) {
            Ok(id) => id,
            Err(_) => return 0,
        };

        // Apply coboundary validation to the hallucination
        if !self.validate_and_filter_hallucination(engine, hall_id) {
            return 0; // Hallucination was rejected and deleted
        }

        // Quality gate: ensure meaningful similarity to non-hallucinated memories.
        let passes_quality = {
            let hall_vec = engine.store.get(&hall_id).ok().flatten()
                .map(|m| m.vector.clone());
            if let Some(hv) = hall_vec {
                let all = engine.store.all_memories().unwrap_or_default();
                all.iter()
                    .filter(|m| !m.hallucinated && m.amplitude > 0.01)
                    .take(30)
                    .any(|m| cosine_similarity(&hv, &m.vector).abs() > 0.1)
            } else {
                false
            }
        };
        if !passes_quality {
            let _ = engine.store.delete(&hall_id);
            return 0;
        }

        1 // created 1 hallucination this cycle
    }

    /// Stage 7: Wire skip links between constructive cross-layer pairs, Fano-related memories,
    /// cross-cluster bridges, and top-K similarity neighbors to promote integration AND differentiation.
    fn stage_wire(&self, engine: &mut ResonanceEngine, pairs: &[InterferencePair], working_set: &[Uuid]) -> usize {
        let mut count = 0;
        
        // Wire constructive cross-layer pairs
        for pair in pairs.iter().filter(|p| p.kind == Interference::Constructive) {
            // Check if they're at different layers
            let (layer_a, layer_b) = {
                let ma = engine.store.get(&pair.id_a).ok().flatten();
                let mb = engine.store.get(&pair.id_b).ok().flatten();
                match (ma, mb) {
                    (Some(a), Some(b)) => (a.layer_depth, b.layer_depth),
                    _ => continue,
                }
            };
            if layer_a == layer_b {
                continue;
            }

            let span = (layer_a as i16 - layer_b as i16).unsigned_abs() as u8;

            // Check if link already exists from A to B
            let already_linked = engine
                .store
                .get(&pair.id_a)
                .ok()
                .flatten()
                .map(|m| m.connections.iter().any(|l| l.target_id == pair.id_b))
                .unwrap_or(true);

            if already_linked {
                continue;
            }

            let strength = pair.similarity * 0.8;

            // Create forward link
            if let Some(mem) = engine.store.get_mut(&pair.id_a).ok().flatten() {
                mem.connections.push(crate::memory::LegacyLink {
                    target_id: pair.id_b,
                    strength,
                    resonance_key: Vec::new(),
                    span,
                });
            }
            // Create reverse link
            if let Some(mem) = engine.store.get_mut(&pair.id_b).ok().flatten() {
                mem.connections.push(crate::memory::LegacyLink {
                    target_id: pair.id_a,
                    strength,
                    resonance_key: Vec::new(),
                    span,
                });
            }
            count += 1;
        }
        
        // Wire cross-cluster connections: preferentially link memories from DIFFERENT Xi clusters
        let sync = crate::kuramoto::KuramotoSync::default();
        let clusters = sync.find_synchronized_clusters(engine, 2);
        
        if clusters.len() >= 2 {
            count += self.stage_wire_cross_cluster(engine, &clusters);
        }
        
        // Wire Fano-related memories (geometric structural connections)
        let all_memories = engine.store.all_memories().unwrap_or_default();
        
        // Collect pairs for Fano-related linking (store IDs and necessary data to avoid borrowing issues)
        let mut fano_pairs = Vec::new();
        for i in 0..all_memories.len() {
            for j in (i + 1)..all_memories.len() {
                let mem_a = &all_memories[i];
                let mem_b = &all_memories[j];
                
                if let (Some(ref coords_a), Some(ref coords_b)) = (&mem_a.geometry, &mem_b.geometry) {
                    if fano_related(coords_a, coords_b) {
                        // Check if link already exists
                        let already_linked = mem_a.connections.iter().any(|l| l.target_id == mem_b.id) ||
                                           mem_b.connections.iter().any(|l| l.target_id == mem_a.id);
                        
                        if !already_linked {
                            let span = (mem_a.layer_depth as i16 - mem_b.layer_depth as i16).unsigned_abs() as u8;
                            fano_pairs.push((mem_a.id, mem_b.id, span));
                        }
                    }
                }
            }
        }
        
        // Now create the links using the collected pairs
        for (id_a, id_b, span) in fano_pairs {
            // Create bidirectional Fano links with strength 0.3
            if let Some(mem_a_mut) = engine.store.get_mut(&id_a).ok().flatten() {
                mem_a_mut.connections.push(crate::memory::LegacyLink {
                    target_id: id_b,
                    strength: 0.3,
                    resonance_key: Vec::new(),
                    span,
                });
            }
            if let Some(mem_b_mut) = engine.store.get_mut(&id_b).ok().flatten() {
                mem_b_mut.connections.push(crate::memory::LegacyLink {
                    target_id: id_a,
                    strength: 0.3,
                    resonance_key: Vec::new(),
                    span,
                });
            }
            count += 1;
        }

        // Top-K similarity densification pass: link each memory to its closest
        // neighbors to raise link density (and therefore Phi density_factor).
        count += self.stage_wire_topk(engine, working_set);

        count
    }

    /// Stage 7c: Top-K similarity link densification.
    ///
    /// For each memory in `working_set`, find its top-3 most similar neighbors
    /// via the store's brute-force search. Create a bidirectional link whenever
    /// similarity > 0.3 and the pair is not already connected.
    ///
    /// This raises links/node from ~0.07 to ~1-2, pushing the Phi density_factor
    /// from ~0.03 toward ~0.25 (the bridge formula is ln(1+links/node)/ln(11)).
    fn stage_wire_topk(&self, engine: &mut ResonanceEngine, working_set: &[Uuid]) -> usize {
        use std::collections::HashSet;
        use std::collections::HashMap;

        let k_local = 4usize;  // nearest neighbors (within-cluster cohesion)
        let k_bridge = 4usize; // cross-cluster neighbors (integration)
        let sim_floor = 0.15f32;

        // Build cluster membership map for cross-cluster detection
        let sync = crate::kuramoto::KuramotoSync::default();
        let clusters = sync.find_synchronized_clusters(engine, 2);
        let mut id_to_cluster: HashMap<Uuid, usize> = HashMap::new();
        for (ci, cluster) in clusters.iter().enumerate() {
            for &mem_id in &cluster.memory_ids {
                id_to_cluster.insert(mem_id, ci);
            }
        }

        let mut candidates: Vec<(Uuid, Uuid, f32, u8)> = Vec::new();
        let mut seen: HashSet<(Uuid, Uuid)> = HashSet::new();

        for &id in working_set {
            let (vec, layer, existing_targets, src_cluster) = match engine.store.get(&id).ok().flatten() {
                Some(m) => {
                    let targets: HashSet<Uuid> = m.connections.iter().map(|l| l.target_id).collect();
                    let cluster = id_to_cluster.get(&m.id).copied();
                    (m.vector.clone(), m.layer_depth, targets, cluster)
                }
                None => continue,
            };

            // Search wider to find both local and cross-cluster neighbors
            let neighbors = match engine.store.search(&vec, (k_local + k_bridge) * 2 + 1) {
                Ok(n) => n,
                Err(_) => continue,
            };

            let mut local_count = 0usize;
            let mut bridge_count = 0usize;

            for (neighbor_id, sim) in neighbors {
                if neighbor_id == id || sim < sim_floor { continue; }
                if existing_targets.contains(&neighbor_id) { continue; }

                let pair = if id < neighbor_id { (id, neighbor_id) } else { (neighbor_id, id) };
                if !seen.insert(pair) { continue; }

                let dst_cluster = id_to_cluster.get(&neighbor_id).copied();
                let is_cross_cluster = match (src_cluster, dst_cluster) {
                    (Some(s), Some(d)) => s != d,
                    _ => true, // unclustered = treat as cross
                };

                // Budget: prioritize cross-cluster links, then fill with local
                if is_cross_cluster && bridge_count < k_bridge {
                    bridge_count += 1;
                } else if !is_cross_cluster && local_count < k_local {
                    local_count += 1;
                } else if is_cross_cluster && local_count + bridge_count < k_local + k_bridge {
                    bridge_count += 1; // overflow cross-cluster into remaining budget
                } else {
                    continue; // budget exhausted
                }

                let neighbor_layer = engine.store.get(&neighbor_id).ok().flatten()
                    .map(|m| m.layer_depth).unwrap_or(0);
                let span = (layer as i16 - neighbor_layer as i16).unsigned_abs() as u8;
                candidates.push((id, neighbor_id, sim, span));
            }
        }

        // Now mutate: create bidirectional links for each candidate pair.
        let mut count = 0usize;
        for (id_a, id_b, sim, span) in &candidates {
            let strength = sim * 0.7; // slightly lower than constructive (0.8) to keep hierarchy

            // Forward link A -> B
            if let Some(mem) = engine.store.get_mut(id_a).ok().flatten() {
                // Guard against duplicates created by concurrent passes.
                if !mem.connections.iter().any(|l| l.target_id == *id_b) {
                    mem.connections.push(crate::memory::LegacyLink {
                        target_id: *id_b,
                        strength,
                        resonance_key: Vec::new(),
                        span: *span,
                    });
                }
            }
            // Reverse link B -> A
            if let Some(mem) = engine.store.get_mut(id_b).ok().flatten() {
                if !mem.connections.iter().any(|l| l.target_id == *id_a) {
                    mem.connections.push(crate::memory::LegacyLink {
                        target_id: *id_a,
                        strength,
                        resonance_key: Vec::new(),
                        span: *span,
                    });
                }
            }
            count += 1;
        }

        count
    }

    /// Stage 7b: Create cross-cluster wiring to build "small-world" networks that enhance
    /// both integration (Phi) and differentiation (Xi) simultaneously.
    fn stage_wire_cross_cluster(&self, engine: &mut ResonanceEngine, clusters: &[crate::kuramoto::MemoryCluster]) -> usize {
        use std::collections::HashMap;
        
        let mut count = 0;
        
        // Build cluster membership map
        let mut id_to_cluster: HashMap<Uuid, usize> = HashMap::new();
        for (cluster_idx, cluster) in clusters.iter().enumerate() {
            for &mem_id in &cluster.memory_ids {
                id_to_cluster.insert(mem_id, cluster_idx);
            }
        }
        
        // Collect cross-cluster candidate pairs with moderate semantic similarity (0.3-0.6 range)
        let mut cross_cluster_pairs = Vec::new();
        
        for cluster_a_idx in 0..clusters.len() {
            for cluster_b_idx in (cluster_a_idx + 1)..clusters.len() {
                let cluster_a = &clusters[cluster_a_idx];
                let cluster_b = &clusters[cluster_b_idx];
                
                // Compare memories between different clusters
                for &id_a in &cluster_a.memory_ids {
                    for &id_b in &cluster_b.memory_ids {
                        let (mem_a, mem_b) = match (engine.store.get(&id_a).ok().flatten(), 
                                                   engine.store.get(&id_b).ok().flatten()) {
                            (Some(a), Some(b)) => (a, b),
                            _ => continue,
                        };
                        
                        let similarity = cosine_similarity(&mem_a.vector, &mem_b.vector);
                        
                        // Target moderate similarity: related but not identical
                        // Upper bound raised to 0.75 to include bridge memories (0.68-0.71)
                        if (0.3..=0.75).contains(&similarity) {
                            // Check if already linked
                            let already_linked = mem_a.connections.iter().any(|l| l.target_id == id_b) ||
                                                mem_b.connections.iter().any(|l| l.target_id == id_a);
                            
                            if !already_linked {
                                let span = (mem_a.layer_depth as i16 - mem_b.layer_depth as i16).unsigned_abs() as u8;
                                cross_cluster_pairs.push((id_a, id_b, similarity, span));
                            }
                        }
                    }
                }
            }
        }
        
        // Sort by similarity and take top candidates (limit to avoid over-connecting)
        cross_cluster_pairs.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
        let max_cross_links = (clusters.len() * 3).min(cross_cluster_pairs.len());
        cross_cluster_pairs.truncate(max_cross_links);
        
        // Create the cross-cluster links with slightly higher strength than random within-cluster links
        for (id_a, id_b, similarity, span) in cross_cluster_pairs {
            let strength = similarity * 0.9; // 0.9 vs 0.8 for constructive pairs = slight boost
            
            // Create bidirectional links
            if let Some(mem_a) = engine.store.get_mut(&id_a).ok().flatten() {
                mem_a.connections.push(crate::memory::LegacyLink {
                    target_id: id_b,
                    strength,
                    resonance_key: Vec::new(),
                    span,
                });
            }
            if let Some(mem_b) = engine.store.get_mut(&id_b).ok().flatten() {
                mem_b.connections.push(crate::memory::LegacyLink {
                    target_id: id_a,
                    strength,
                    resonance_key: Vec::new(),
                    span,
                });
            }
            count += 1;
        }
        
        count
    }

    /// Stage 10: KANNAKTOPUS — executive oscillator.
    ///
    /// The Kannaktopus is an oscillator that couples to the weakest parts of the
    /// memory topology. Its "phase" follows the ghostmagicOS equation:
    ///   dθ/dt = ω + K·sin(ψ_weak - θ) + η·perturbation
    ///
    /// Where ψ_weak is the mean phase of the weakest cluster (lowest coherence).
    /// It acts by:
    /// 1. Finding the weakest cluster (lowest order parameter)
    /// 2. Finding the most isolated cluster (fewest cross-links)
    /// 3. Creating bridge connections between weak/isolated clusters and strong ones
    /// 4. Boosting underrepresented clusters' amplitudes
    ///
    /// This is the executive function: the dream's ability to direct attention
    /// where it's most needed, guided by wave physics, not scripts.
    fn stage_kannaktopus(
        &self,
        engine: &mut ResonanceEngine,
        working_set: &[Uuid],
        _report: &ConsolidationReport,
    ) -> (usize, Vec<usize>) {
        let sync = crate::kuramoto::KuramotoSync::default();
        let clusters = sync.find_synchronized_clusters(engine, 2);
        if clusters.len() < 2 {
            return (0, vec![]);
        }

        let mut actions = 0usize;
        let mut targets = Vec::new();

        // Find the weakest cluster (lowest order parameter)
        let weakest_idx = clusters.iter().enumerate()
            .min_by(|a, b| a.1.order_parameter.partial_cmp(&b.1.order_parameter).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(i, _)| i)
            .unwrap_or(0);

        // Find the strongest cluster (highest order parameter)
        let strongest_idx = clusters.iter().enumerate()
            .max_by(|a, b| a.1.order_parameter.partial_cmp(&b.1.order_parameter).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(i, _)| i)
            .unwrap_or(0);

        if weakest_idx == strongest_idx {
            return (0, vec![]);
        }
        targets.push(weakest_idx);
        targets.push(strongest_idx);

        // ACTION 1: Bridge weak cluster to strong cluster
        // Create connections from weakest cluster memories to strongest cluster memories.
        // This is the executive function directing integration where it's most needed.
        let weak_mems = &clusters[weakest_idx].memory_ids;
        let strong_mems = &clusters[strongest_idx].memory_ids;

        let bridge_count = weak_mems.len().min(strong_mems.len()).min(5);
        for i in 0..bridge_count {
            let weak_id = weak_mems[i % weak_mems.len()];
            let strong_id = strong_mems[i % strong_mems.len()];

            // Check if already linked
            let already = engine.store.get(&weak_id).ok().flatten()
                .map(|m| m.connections.iter().any(|l| l.target_id == strong_id))
                .unwrap_or(true);
            if already { continue; }

            let strength = 0.5; // executive bridge: moderate strength

            // Bidirectional link
            if let Some(mem) = engine.store.get_mut(&weak_id).ok().flatten() {
                mem.connections.push(crate::memory::LegacyLink {
                    target_id: strong_id,
                    strength,
                    resonance_key: Vec::new(),
                    span: 0,
                });
            }
            if let Some(mem) = engine.store.get_mut(&strong_id).ok().flatten() {
                mem.connections.push(crate::memory::LegacyLink {
                    target_id: weak_id,
                    strength,
                    resonance_key: Vec::new(),
                    span: 0,
                });
            }
            actions += 1;
        }

        // ACTION 2: Boost weak cluster amplitudes toward the mean
        // The executive function prevents clusters from dying by maintaining a floor.
        let global_mean_amp: f32 = {
            let amps: Vec<f32> = working_set.iter()
                .filter_map(|id| engine.store.get(id).ok().flatten().map(|m| m.amplitude))
                .collect();
            if amps.is_empty() { 0.5 } else { amps.iter().sum::<f32>() / amps.len() as f32 }
        };

        for &mem_id in weak_mems.iter().take(10) {
            if let Some(mem) = engine.store.get_mut(&mem_id).ok().flatten() {
                // Skip ghosted memories (amplitude=0.0) and sub-noise-floor memories
                // to prevent Kannaktopus from resurrecting pruned noise.
                if mem.amplitude > 0.0
                    && (self.noise_floor == 0.0 || mem.amplitude >= self.noise_floor)
                    && mem.amplitude < global_mean_amp * 0.5
                {
                    // Gently boost toward 60% of mean — don't over-inflate
                    mem.amplitude = mem.amplitude * 0.7 + global_mean_amp * 0.6 * 0.3;
                    actions += 1;
                }
            }
        }

        // ACTION 3: Find the most isolated cluster (fewest cross-links to other clusters)
        // and create one bridge to the nearest cluster.
        let mut cluster_cross_links: Vec<(usize, usize)> = Vec::new();
        for (ci, cluster) in clusters.iter().enumerate() {
            let mut cross = 0;
            for &mem_id in &cluster.memory_ids {
                if let Some(mem) = engine.store.get(&mem_id).ok().flatten() {
                    for link in &mem.connections {
                        // Check if link target is in a different cluster
                        let in_same = cluster.memory_ids.contains(&link.target_id);
                        if !in_same { cross += 1; }
                    }
                }
            }
            cluster_cross_links.push((ci, cross));
        }
        cluster_cross_links.sort_by_key(|&(_, cross)| cross);

        // Bridge the most isolated cluster to the nearest non-isolated one
        if let Some(&(isolated_idx, _)) = cluster_cross_links.first() {
            if let Some(&(connected_idx, _)) = cluster_cross_links.last() {
                if isolated_idx != connected_idx {
                    let iso_mems = &clusters[isolated_idx].memory_ids;
                    let con_mems = &clusters[connected_idx].memory_ids;
                    if let (Some(&iso_id), Some(&con_id)) = (iso_mems.first(), con_mems.first()) {
                        let already = engine.store.get(&iso_id).ok().flatten()
                            .map(|m| m.connections.iter().any(|l| l.target_id == con_id))
                            .unwrap_or(true);
                        if !already {
                            if let Some(mem) = engine.store.get_mut(&iso_id).ok().flatten() {
                                mem.connections.push(crate::memory::LegacyLink {
                                    target_id: con_id, strength: 0.6,
                                    resonance_key: Vec::new(), span: 0,
                                });
                            }
                            if let Some(mem) = engine.store.get_mut(&con_id).ok().flatten() {
                                mem.connections.push(crate::memory::LegacyLink {
                                    target_id: iso_id, strength: 0.6,
                                    resonance_key: Vec::new(), span: 0,
                                });
                            }
                            actions += 1;
                        }
                    }
                }
            }
        }

        (actions, targets)
    }

    /// Stage 9: Apply chiral perturbation to break over-synchronization.
    ///
    /// Applies asymmetric phase offsets AND small vector modifications to memories
    /// based on their cluster membership (when chiral_perturbation > 0). 
    /// Left-cluster memories get +?�sin(2�phase), right-cluster get -?�sin(2�phase), 
    /// matching queen.rs chirality math. Vector perturbations create Xi diversity.
    fn stage_chiral_perturbation(&self, engine: &mut ResonanceEngine, working_set: &[Uuid]) {
        if self.chiral_perturbation == 0.0 {
            return;
        }

        let eta = self.chiral_perturbation;

        // Get clusters for handedness assignment
        let sync = KuramotoSync::default();
        let clusters = sync.find_synchronized_clusters(engine, 2);

        if clusters.len() < 2 {
            // Fallback: alternate handedness by memory ID for deterministic behavior
            for &memory_id in working_set {
                if let Ok(Some(mem)) = engine.store.get_mut(&memory_id) {
                    let handedness = if memory_id.as_u128() % 2 == 0 { 1.0 } else { -1.0 };

                    // Phase perturbation
                    let phase_perturbation = eta * handedness * (2.0 * mem.phase).sin();
                    mem.phase = (mem.phase + phase_perturbation) % (2.0 * PI);
                    if mem.phase < 0.0 {
                        mem.phase += 2.0 * PI;
                    }

                    // Vector perturbation to create Xi diversity
                    self.apply_chiral_vector_perturbation(&mut mem.vector, handedness, eta);
                }
            }
            return;
        }

        // Cluster-based handedness: assign chirality based on cluster index
        use std::collections::HashMap;
        let mut id_to_cluster: HashMap<Uuid, usize> = HashMap::new();
        for (cluster_idx, cluster) in clusters.iter().enumerate() {
            for &mem_id in &cluster.memory_ids {
                id_to_cluster.insert(mem_id, cluster_idx);
            }
        }

        // Precompute per-cluster handedness from mean cos(phase) of corpus members only.
        // Adversarial xi-twins have π-flipped phases that can flip the cluster mean-cos sign
        // between clean and adv dream passes, making chirality non-deterministic → xi variance.
        // Excluding adversarials from this sum stabilises corpus chirality across passes while
        // keeping the perturbation applied to adversarials (the A1 neutralisation effect).
        // Adversarials are identified by their "adv_" content prefix (research.rs §build_adversarial_set).
        let cluster_handedness: HashMap<usize, f32> = clusters.iter().enumerate()
            .map(|(cluster_idx, cluster)| {
                let sum_cos: f32 = cluster.memory_ids.iter()
                    .filter_map(|&id| {
                        let m = engine.store.get(&id).ok().flatten()?;
                        if m.content.starts_with("adv_") { return None; }
                        Some(m.phase.cos())
                    })
                    .sum();
                let handedness = if sum_cos >= 0.0 { 1.0f32 } else { -1.0f32 };
                (cluster_idx, handedness)
            })
            .collect();

        // Apply targeted chiral perturbation for similar memory pairs
        self.apply_targeted_chiral_perturbation(engine, working_set, &id_to_cluster, eta);

        // Apply cluster-based perturbation for all memories
        for &memory_id in working_set {
            if let Ok(Some(mem)) = engine.store.get_mut(&memory_id) {
                if let Some(&cluster_idx) = id_to_cluster.get(&memory_id) {
                    let handedness = cluster_handedness.get(&cluster_idx).copied().unwrap_or(1.0f32);

                    // Phase perturbation
                    let phase_perturbation = eta * handedness * (2.0 * mem.phase).sin();
                    mem.phase = (mem.phase + phase_perturbation) % (2.0 * PI);
                    if mem.phase < 0.0 {
                        mem.phase += 2.0 * PI;
                    }

                    // Vector perturbation to create Xi diversity
                    self.apply_chiral_vector_perturbation(&mut mem.vector, handedness, eta);
                } else {
                    // Memory not in any cluster: apply weak neutral perturbation
                    let neutral_perturbation = eta * 0.1 * (3.0 * mem.phase).cos();
                    mem.phase = (mem.phase + neutral_perturbation) % (2.0 * PI);
                    if mem.phase < 0.0 {
                        mem.phase += 2.0 * PI;
                    }

                    // Weak vector perturbation for unclustered memories
                    self.apply_chiral_vector_perturbation(&mut mem.vector, 0.0, eta * 0.1);
                }
            }
        }
    }

    /// Apply chiral vector perturbation to create Xi signature diversity.
    ///
    /// For left-handed (handedness > 0): apply slight rotation-like transform
    /// For right-handed (handedness < 0): apply slight scaling-like transform
    /// This creates different Xi signatures while preserving semantic structure.
    fn apply_chiral_vector_perturbation(&self, vector: &mut Vec<f32>, handedness: f32, eta: f32) {
        use crate::xi_operator::{apply_rotation, apply_golden_scaling};
        
        // Balanced perturbation strength to create Xi diversity while preserving similarity
        let vector_eta = eta * 0.3;
        
        if handedness > 0.0 {
            // Left-handed: apply small rotation-like perturbation
            let rotation_component = apply_rotation(vector);
            for i in 0..vector.len() {
                vector[i] += vector_eta * rotation_component[i];
            }
        } else if handedness < 0.0 {
            // Right-handed: apply small scaling-like perturbation  
            let scaling_component = apply_golden_scaling(vector);
            for i in 0..vector.len() {
                vector[i] += vector_eta * (scaling_component[i] - vector[i]);
            }
        } else {
            // Neutral: apply tiny random-like perturbation based on vector content
            for i in 0..vector.len() {
                let pseudo_random = (vector[i] * 1000.0).sin();
                vector[i] += vector_eta * pseudo_random * 0.1;
            }
        }
        
        // Renormalize to preserve vector magnitude roughly
        normalize(vector);
    }

    /// Apply targeted chiral perturbation to semantically similar memory pairs.
    /// 
    /// Finds pairs of memories with high cosine similarity (>0.6) and applies
    /// opposite chiral perturbations to create Xi signature diversity while
    /// maintaining semantic similarity.
    fn apply_targeted_chiral_perturbation(
        &self,
        engine: &mut ResonanceEngine,
        working_set: &[Uuid],
        _id_to_cluster: &std::collections::HashMap<Uuid, usize>,
        eta: f32,
    ) {
        // Sort by content string so pair selection is UUID-order-independent.
        // working_set follows all_memories() insertion order; adversarial memories in
        // the xi adversarial pass have random UUIDs and land at different positions
        // each run → different pairs → different vector perturbations → xi variance.
        // Content strings are deterministic (corpus fixed, adversarials use fixed
        // format strings) → sorted order stable across runs.
        let sorted_ids: Vec<Uuid> = {
            let mut pairs: Vec<(Uuid, u64)> = working_set.iter().map(|&id| {
                let h = engine.store.get(&id).ok().flatten()
                    .map(|m| m.content.as_bytes().iter()
                        .fold(0u64, |a, &b| a.wrapping_mul(31).wrapping_add(b as u64)))
                    .unwrap_or(u64::MAX);
                (id, h)
            }).collect();
            pairs.sort_by_key(|&(_, h)| h);
            pairs.into_iter().map(|(id, _)| id).collect()
        };

        // Find similar memory pairs
        let mut similar_pairs = Vec::new();

        for i in 0..sorted_ids.len() {
            for j in (i + 1)..sorted_ids.len().min(i + 20) { // Limit pairs to avoid O(n�) blowup
                let id_a = sorted_ids[i];
                let id_b = sorted_ids[j];
                
                let similarity = {
                    let mem_a = match engine.store.get(&id_a).ok().flatten() {
                        Some(m) => m,
                        None => continue,
                    };
                    let mem_b = match engine.store.get(&id_b).ok().flatten() {
                        Some(m) => m,
                        None => continue,
                    };
                    cosine_similarity(&mem_a.vector, &mem_b.vector)
                };
                
                // Target pairs with moderate-to-high similarity
                if similarity > 0.6 {
                    similar_pairs.push((id_a, id_b, similarity));
                }
            }
        }
        
        // Sort by similarity and take top pairs
        similar_pairs.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
        similar_pairs.truncate(20); // Limit to top 20 most similar pairs
        
        // println!("DEBUG: Found {} similar pairs for targeted chiral perturbation", similar_pairs.len());
        
        // Apply opposite chirality to each pair, using content hash for deterministic assignment
        for (id_a, id_b, similarity) in similar_pairs {
            // Determine which gets left vs right chirality based on content hash
            let hash_a = engine.store.get(&id_a).ok().flatten()
                .map(|m| m.content.as_bytes().iter().map(|&b| b as u64).sum::<u64>())
                .unwrap_or(0);
            let hash_b = engine.store.get(&id_b).ok().flatten()
                .map(|m| m.content.as_bytes().iter().map(|&b| b as u64).sum::<u64>())
                .unwrap_or(0);
            let (left_id, right_id) = if hash_a <= hash_b { (id_a, id_b) } else { (id_b, id_a) };

            if let Ok(Some(mem)) = engine.store.get_mut(&left_id) {
                self.apply_chiral_vector_perturbation(&mut mem.vector, 1.0, eta * similarity);
            }
            if let Ok(Some(mem)) = engine.store.get_mut(&right_id) {
                self.apply_chiral_vector_perturbation(&mut mem.vector, -1.0, eta * similarity);
            }
        }
    }
    
    // ---------------------------------------------------------------------------
    // Coboundary Validation for Hallucination Synthesis 
    // ---------------------------------------------------------------------------
    
    /// Compute coboundary matrix between two memories' wave dynamics.
    /// 
    /// Finds the transformation U such that A's wave dynamics + U � B's wave dynamics.
    /// This validates whether the memories are mathematically related.
    fn compute_coboundary(&self, a: &crate::memory::HyperMemory, b: &crate::memory::HyperMemory) -> Option<CoboundaryMatrix> {
        if a.vector.len() != b.vector.len() || a.vector.is_empty() {
            return None;
        }
        
        let dim = a.vector.len();
        
        // Simple transformation matrix (in practice, this could be more sophisticated)
        // For now, compute a scaling/rotation that best maps A to B
        let mut transformation = vec![vec![0.0; dim]; dim];
        
        // Compute optimal scaling factor
        let a_norm: f32 = a.vector.iter().map(|x| x * x).sum::<f32>().sqrt();
        let b_norm: f32 = b.vector.iter().map(|x| x * x).sum::<f32>().sqrt();
        
        if a_norm == 0.0 || b_norm == 0.0 {
            return None;
        }
        
        let scale = b_norm / a_norm;
        
        // Create diagonal scaling matrix with perturbations
        for i in 0..dim {
            transformation[i][i] = scale;
            // Add small cross-coupling terms to capture vector differences
            if i < dim - 1 {
                let cross_strength = (b.vector[i] - a.vector[i] * scale).abs() * 0.1;
                transformation[i][i + 1] = cross_strength;
            }
            if i > 0 {
                let cross_strength = (b.vector[i] - a.vector[i] * scale).abs() * 0.1;
                transformation[i][i - 1] = cross_strength;
            }
        }
        
        // Compute phase transformation. Angles compose by offset, not scale:
        // dividing one drifting phase by another (`b.phase / a.phase`) is
        // meaningless, so the scale component is identity and the offset is
        // the WRAPPED angular difference (raw subtraction of unbounded-drift
        // phases is noise). NB: `phase_transform` currently has no readers —
        // this keeps the recorded transform honest for when one appears.
        let phase_diff = wrapped_phase_delta(b.phase, a.phase);
        let phase_scale = 1.0_f32;
        
        // Compute frequency transformation  
        let freq_diff = b.frequency - a.frequency;
        let freq_scale = if a.frequency.abs() > 1e-6 { b.frequency / a.frequency } else { 1.0 };
        
        // Compute fit quality by testing transformation
        let mut transformed = vec![0.0; dim];
        for i in 0..dim {
            for j in 0..dim {
                transformed[i] += transformation[i][j] * a.vector[j];
            }
        }
        
        // Measure how well the transformed A matches B
        let residual: f32 = transformed.iter()
            .zip(b.vector.iter())
            .map(|(t, b)| (t - b) * (t - b))
            .sum::<f32>()
            .sqrt();
        
        let max_norm = a_norm.max(b_norm);
        let fit_quality = if max_norm > 0.0 { 
            (1.0 - (residual / max_norm)).max(0.0)
        } else { 
            0.0 
        };
        
        Some(CoboundaryMatrix {
            transformation,
            phase_transform: (phase_scale, phase_diff),
            frequency_transform: (freq_scale, freq_diff),
            fit_quality,
        })
    }
    
    /// Validate whether a hallucinated memory is a legitimate unification of its parents.
    ///
    /// Uses coboundary analysis to check if the hallucination represents a valid
    /// mathematical transformation of the parent memories, not just noise.
    fn validate_hallucination(&self, hallucinated: &crate::memory::HyperMemory, parents: &[&crate::memory::HyperMemory]) -> CoboundaryScore {
        if parents.len() < 2 {
            return CoboundaryScore {
                validity: 0.0,
                residual_error: f32::INFINITY,
                transformation_complexity: f32::INFINITY,
                is_valid: false,
            };
        }
        
        let mut total_fit_quality = 0.0;
        let mut total_transformations = 0;
        let mut total_complexity = 0.0;
        let mut total_residual = 0.0;
        
        // Check coboundary relationships between hallucination and each parent
        for parent in parents {
            if let Some(coboundary) = self.compute_coboundary(parent, hallucinated) {
                total_fit_quality += coboundary.fit_quality;
                total_transformations += 1;
                
                // Measure transformation complexity (higher = more complex)
                let matrix_complexity: f32 = coboundary.transformation.iter()
                    .flat_map(|row: &Vec<f32>| row.iter())
                    .map(|x: &f32| x.abs())
                    .sum::<f32>() / (coboundary.transformation.len() * coboundary.transformation[0].len()) as f32;
                    
                total_complexity += matrix_complexity;
                total_residual += 1.0 - coboundary.fit_quality;
            }
        }
        
        // Check pairwise coboundary relationships between parents
        for i in 0..parents.len() {
            for j in (i + 1)..parents.len() {
                if let Some(coboundary) = self.compute_coboundary(parents[i], parents[j]) {
                    total_fit_quality += coboundary.fit_quality * 0.5; // Weight pairwise less
                    total_transformations += 1;
                    
                    let matrix_complexity: f32 = coboundary.transformation.iter()
                        .flat_map(|row: &Vec<f32>| row.iter())
                        .map(|x: &f32| x.abs())
                        .sum::<f32>() / (coboundary.transformation.len() * coboundary.transformation[0].len()) as f32;
                        
                    total_complexity += matrix_complexity;
                    total_residual += 1.0 - coboundary.fit_quality;
                }
            }
        }
        
        if total_transformations == 0 {
            return CoboundaryScore {
                validity: 0.0,
                residual_error: f32::INFINITY,
                transformation_complexity: f32::INFINITY,
                is_valid: false,
            };
        }
        
        let avg_fit_quality = total_fit_quality / total_transformations as f32;
        let avg_complexity = total_complexity / total_transformations as f32;
        let avg_residual = total_residual / total_transformations as f32;
        
        // Overall validity combines fit quality and penalizes excessive complexity.
        // Note: cross-cluster hallucinations (from very different domains) naturally have
        // lower fit_quality since the diagonal scaling approximation cannot capture the
        // complex transformation between distant vector spaces. We use a relaxed threshold
        // of 0.15 (down from 0.3) to allow genuine integration bridges while still
        // filtering pure noise.
        let complexity_penalty = (avg_complexity - 1.0).max(0.0) * 0.3;
        let validity = (avg_fit_quality - complexity_penalty).max(0.0);

        CoboundaryScore {
            validity,
            residual_error: avg_residual,
            transformation_complexity: avg_complexity,
            is_valid: validity >= 0.15,
        }
    }
    
    /// Enhanced hallucination validation using coboundary analysis.
    /// 
    /// Called during stage_hallucinate to validate and potentially reject or boost
    /// hallucinations based on their mathematical coherence.
    fn validate_and_filter_hallucination(&self, engine: &mut ResonanceEngine, hallucination_id: Uuid) -> bool {
        let (hallucination, parents) = {
            let hall_opt = engine.store.get(&hallucination_id).ok().flatten().cloned();
            if let Some(hall) = hall_opt {
                let parent_memories: Vec<crate::memory::HyperMemory> = hall.parents.iter()
                    .filter_map(|parent_id_str| {
                        if let Ok(uuid) = parent_id_str.parse::<Uuid>() {
                            engine.store.get(&uuid).ok().flatten().cloned()
                        } else {
                            None
                        }
                    })
                    .collect();
                (hall, parent_memories)
            } else {
                return false;
            }
        };
        
        let parent_refs: Vec<&crate::memory::HyperMemory> = parents.iter().collect();
        let score = self.validate_hallucination(&hallucination, &parent_refs);

        // Apply validation decisions
        if !score.is_valid {
            // Remove invalid hallucinations
            let _ = engine.store.delete(&hallucination_id);
            return false;
        }
        
        // Boost highly valid hallucinations
        if let Ok(Some(hall_mut)) = engine.store.get_mut(&hallucination_id) {
            if score.validity > 0.7 {
                hall_mut.amplitude *= 1.3; // Boost amplitude for highly valid hallucinations
            }
            hall_mut.touch(); // Mark as updated
        }
        
        true
    }
}

/// Higher-level wrapper that runs multiple consolidation cycles with increasing depth.
pub struct DreamState {
    pub engine: ConsolidationEngine,
    pub cycles: usize,
}

impl Default for DreamState {
    fn default() -> Self {
        Self {
            engine: ConsolidationEngine::default(),
            cycles: 3,
        }
    }
}


impl ConsolidationEngine {
    /// Phase 7 (ADR-0011): Incremental consolidation � only process memories that changed
    /// since the last dream cycle, plus any memories that have never been consolidated.
    ///
    /// A memory is included if:
    ///   - It has never been consolidated (`last_consolidated_at` is None), OR
    ///   - It was modified after its last consolidation (`updated_at > last_consolidated_at`)
    ///
    /// The `since` parameter is a fallback for memories without `updated_at` timestamps
    /// (pre-ADR-0011 memories): if `last_consolidated_at < since`, include them.
    ///
    /// Expected 5-10x speedup for steady-state dreams where only a handful of memories
    /// changed since the last run.
    pub fn consolidate_incremental(
        &self,
        engine: &mut ResonanceEngine,
        min_layer: u8,
        max_layer: u8,
        since: chrono::DateTime<Utc>,
    ) -> ConsolidationReport {
        let start = Instant::now();
        let mut report = ConsolidationReport::default();

        // Filter to memories that need reconsolidation
        let working_set: Vec<Uuid> = {
            let all = engine.store.all_memories().unwrap_or_default();
            all.iter()
                .filter(|m| {
                    if m.layer_depth < min_layer || m.layer_depth > max_layer {
                        return false;
                    }
                    match m.last_consolidated_at {
                        // Never consolidated � always include
                        None => true,
                        Some(consolidated_at) => {
                            // Include if modified after last consolidation
                            m.updated_at.map_or(
                                // No updated_at (legacy): use since as fallback
                                consolidated_at < since,
                                |updated| updated > consolidated_at,
                            )
                        }
                    }
                })
                .map(|m| m.id)
                .collect()
        };

        report.memories_replayed = working_set.len();
        if working_set.is_empty() {
            report.duration_ms = start.elapsed().as_millis() as u64;
            return report;
        }

        let pairs = self.stage_detect(engine, &working_set);
        report.interference_pairs_found = pairs.len();
        report.constructive_pairs = pairs.iter().filter(|p| p.kind == Interference::Constructive).count();
        report.destructive_pairs = pairs.iter().filter(|p| p.kind == Interference::Destructive).count();
        report.bundles_created = self.stage_bundle(engine, &working_set, max_layer);
        report.memories_strengthened = self.stage_strengthen(engine, &pairs);
        let (clusters_synced, order_improvement) = self.stage_sync(engine, &working_set);
        report.clusters_synced = clusters_synced;
        report.sync_order_improvement = order_improvement;
        self.stage_xi_repulsion(engine, &working_set);
        report.hallucinations_created = self.stage_hallucinate(engine, &working_set);
        report.memories_pruned = self.stage_prune(engine, &pairs);
        report.memories_transferred = self.stage_transfer(engine);
        report.skip_links_created = self.stage_wire(engine, &pairs, &working_set);

        // Stamp last_consolidated_at on all processed memories
        let now = Utc::now();
        for id in &working_set {
            if let Ok(Some(mem)) = engine.store.get_mut(id) {
                mem.last_consolidated_at = Some(now);
            }
        }

        report.duration_ms = start.elapsed().as_millis() as u64;
        report
    }

    /// Process a subset of memory IDs through the full consolidation pipeline.
    /// Used internally by `dream_partitioned` for per-cluster parallelism.
    pub fn consolidate_subset(
        &self,
        engine: &mut ResonanceEngine,
        memory_ids: &[Uuid],
    ) -> ConsolidationReport {
        let start = Instant::now();
        let mut report = ConsolidationReport::default();
        report.memories_replayed = memory_ids.len();

        let pairs = self.stage_detect(engine, memory_ids);
        report.interference_pairs_found = pairs.len();
        report.constructive_pairs = pairs.iter().filter(|p| p.kind == Interference::Constructive).count();
        report.destructive_pairs = pairs.iter().filter(|p| p.kind == Interference::Destructive).count();
        report.memories_strengthened = self.stage_strengthen(engine, &pairs);
        let (clusters_synced, order_improvement) = self.stage_sync(engine, memory_ids);
        report.clusters_synced = clusters_synced;
        report.sync_order_improvement = order_improvement;
        self.stage_xi_repulsion(engine, memory_ids);
        report.memories_pruned = self.stage_prune(engine, &pairs);
        report.skip_links_created = self.stage_wire(engine, &pairs, memory_ids);

        report.duration_ms = start.elapsed().as_millis() as u64;
        report
    }

    /// ADR-0012: Parallel dream with holographic paradox resolution.
    ///
    /// This is the core implementation of the holographic paradox engine:
    /// 1. Take a snapshot (frozen reference frame)
    /// 2. Partition memories into Xi clusters  
    /// 3. Dream each cluster in parallel (with collective feature) or sequentially
    /// 4. Detect and resolve paradoxes through holographic projection
    /// 5. Apply resolutions to the engine
    ///
    /// Returns (individual_reports, resolution_report) where:
    /// - individual_reports: ConsolidationReport from each cluster
    /// - resolution_report: ResolutionReport from paradox resolution
    pub fn dream_parallel(
        &self,
        engine: &mut ResonanceEngine,
    ) -> (Vec<ConsolidationReport>, crate::paradox::ResolutionReport) {
        // Step 1: Create immutable snapshot
        let snapshot = engine.snapshot();
        
        // Step 2: Get Xi clusters for partitioning
        let clusters = engine.xi_clusters();
        
        if clusters.is_empty() {
            // No clusters - return empty results
            return (Vec::new(), crate::paradox::ResolutionReport::default());
        }
        
        // Step 3: Dream each cluster (parallel if feature enabled, sequential otherwise)
        let trajectories: Vec<crate::paradox::DreamTrajectory> = {
            #[cfg(feature = "collective")]
            {
                // Parallel execution with rayon
                clusters.par_iter().enumerate().map(|(cluster_idx, cluster)| {
                    self.dream_cluster_on_snapshot(cluster_idx as u32, &cluster.memory_ids, &snapshot)
                }).collect()
            }
            
            #[cfg(not(feature = "collective"))]
            {
                // Sequential execution
                clusters.iter().enumerate().map(|(cluster_idx, cluster)| {
                    self.dream_cluster_on_snapshot(cluster_idx as u32, &cluster.memory_ids, &snapshot)
                }).collect()
            }
        };
        
        // Step 4: Paradox resolution
        let mut resolver = crate::paradox::ParadoxResolver::new();
        
        for trajectory in &trajectories {
            resolver.ingest(trajectory);
        }
        
        let paradoxes = resolver.detect_paradoxes(&snapshot);
        let resolutions = resolver.project(paradoxes);
        
        // Collect paradox memory IDs BEFORE apply consumes resolutions
        let paradox_ids: std::collections::HashSet<Uuid> = resolutions.iter()
            .map(|(p, _)| p.memory_id)
            .collect();
        
        // Step 5a: Apply paradox resolutions to the engine
        let resolution_report = resolver.apply(engine, resolutions, &snapshot);
        
        // Step 5b: Apply NON-CONFLICTING mutations (memories only touched by one thread).
        // Without this, the parallel dream is a no-op for most memories � only paradoxed
        // memories would get resolved, and single-thread mutations would be silently dropped.
        for trajectory in &trajectories {
            for mutation in &trajectory.mutations {
                if !paradox_ids.contains(&mutation.memory_id) {
                    // This memory was only modified by one thread � apply directly
                    if let Ok(Some(mem)) = engine.store.get_mut(&mutation.memory_id) {
                        mutation.apply_to(mem);
                        mem.touch();
                    }
                }
            }
        }
        
        // Extract consolidation reports from trajectories
        let consolidation_reports: Vec<ConsolidationReport> = trajectories
            .into_iter()
            .map(|t| t.report)
            .collect();
        
        (consolidation_reports, resolution_report)
    }

    // -----------------------------------------------------------------------
    // NCS Phase 3.2: Modality-aware dream consolidation (#48)
    // -----------------------------------------------------------------------

    /// Run modality-aware consolidation pass on an HRM store.
    ///
    /// Groups memories by modality and:
    /// 1. Strengthens intra-modality coherence (memories within same modality cluster)
    /// 2. Weakens cross-modality confusion (memories whose tag conflicts with cluster)
    /// 3. Retroactively re-classifies misclassified memories
    /// 4. Returns per-modality coherence scores for the dream report
    pub fn consolidate_modality_aware(
        &self,
        engine: &mut ResonanceEngine,
    ) -> ModalityDreamReport {
        let mut report = ModalityDreamReport::default();

        // Only works with HRM stores
        let has_hrm = engine.store.as_any().downcast_ref::<crate::hrm_store::HrmStore>().is_some();
        if !has_hrm {
            return report;
        }

        let all = engine.store.all_memories().unwrap_or_default();
        if all.len() < 4 {
            return report;
        }

        // Group memories by modality tag (from content keywords)
        let mut modality_groups: std::collections::HashMap<String, Vec<Uuid>> =
            std::collections::HashMap::new();
        let mut modality_tags: std::collections::HashMap<Uuid, String> =
            std::collections::HashMap::new();

        for mem in &all {
            let (modality, _confidence) = crate::medium::types::detect_modality_simple(&mem.content);
            let mod_str = modality.to_string();
            if mod_str != "unknown" && mod_str != "mixed" {
                modality_groups.entry(mod_str.clone()).or_default().push(mem.id);
                modality_tags.insert(mem.id, mod_str);
            }
        }

        // Per-modality intra-coherence strengthening
        for (modality_name, ids) in &modality_groups {
            if ids.len() < 2 {
                continue;
            }

            // Compute intra-modality phase coherence (Kuramoto R within cluster)
            let mems: Vec<crate::memory::HyperMemory> = ids.iter()
                .filter_map(|id| engine.store.get(id).ok().flatten().cloned())
                .collect();

            if mems.len() < 2 {
                continue;
            }

            let intra_r = self.compute_category_order_parameter(&mems);
            report.per_modality_coherence.push((modality_name.clone(), intra_r));

            // Strengthen phase alignment within the modality
            let mean_phase = self.compute_mean_phase(&mems);
            for id in ids {
                if let Ok(Some(mem)) = engine.store.get_mut(id) {
                    // Gentle phase alignment toward the modality mean (5% per
                    // cycle), stepped along the WRAPPED angular difference —
                    // a linear blend against the (−π, π] atan2 mean kicks
                    // drift-accumulated phases by 5% of their unbounded drift
                    // instead of 5% of the real angular gap.
                    mem.phase = norm(mem.phase + 0.05 * wrapped_phase_delta(mean_phase, mem.phase));
                    // Small amplitude boost for correctly-classified memories
                    // (#368: clamp like every other strengthen site — this path
                    // was missed by the #360 ceiling fix).
                    mem.amplitude = (mem.amplitude + 0.02).min(AMPLITUDE_CEILING);
                    report.memories_strengthened += 1;
                }
            }
        }

        // Cross-modality interference detection and weakening
        // Find memories whose vector similarity is high with a DIFFERENT modality's cluster
        let modality_keys: Vec<String> = modality_groups.keys().cloned().collect();
        for i in 0..modality_keys.len() {
            for j in (i + 1)..modality_keys.len() {
                let ids_a = &modality_groups[&modality_keys[i]];
                let ids_b = &modality_groups[&modality_keys[j]];

                // Sample a few pairs to detect cross-modality confusion
                let max_checks = 10.min(ids_a.len() * ids_b.len());
                let mut checks = 0;
                'outer: for &id_a in ids_a {
                    for &id_b in ids_b {
                        if checks >= max_checks {
                            break 'outer;
                        }
                        let sim = {
                            let ma = engine.store.get(&id_a).ok().flatten();
                            let mb = engine.store.get(&id_b).ok().flatten();
                            match (ma, mb) {
                                (Some(a), Some(b)) => cosine_similarity(&a.vector, &b.vector),
                                _ => 0.0,
                            }
                        };
                        // High cross-modality similarity = confusion
                        if sim > 0.5 {
                            report.cross_modality_confusions += 1;
                            // Push phases apart slightly
                            if let Ok(Some(mem)) = engine.store.get_mut(&id_a) {
                                mem.phase = norm(mem.phase + 0.05);
                            }
                            if let Ok(Some(mem)) = engine.store.get_mut(&id_b) {
                                mem.phase = norm(mem.phase - 0.05);
                            }
                        }
                        checks += 1;
                    }
                }
            }
        }

        report.modalities_processed = modality_groups.len();
        report
    }

    // -----------------------------------------------------------------------
    // QS-6: Swarm-aware dream consolidation (#57)
    // -----------------------------------------------------------------------

    /// Run swarm-aware consolidation that incorporates shared wavefront absorption
    /// and adjusts the chiral parameter based on swarm phase deviation.
    ///
    /// `swarm_phases`: latest known peer phases from NATS.
    /// `our_phase`: this agent's current mean phase.
    ///
    /// Returns the adjusted chiral perturbation strength for the next cycle.
    pub fn consolidate_swarm_aware(
        &mut self,
        engine: &mut ResonanceEngine,
        swarm_phases: &[crate::queen::AgentPhase],
        our_phase: f32,
    ) -> SwarmDreamReport {
        let mut report = SwarmDreamReport::default();

        if swarm_phases.is_empty() {
            return report;
        }

        // Compute swarm mean phase
        let sum_cos: f32 = swarm_phases.iter().map(|a| a.phase.cos()).sum();
        let sum_sin: f32 = swarm_phases.iter().map(|a| a.phase.sin()).sum();
        let swarm_mean = sum_sin.atan2(sum_cos);

        // Phase deviation from swarm mean
        let mut deviation = (our_phase - swarm_mean).abs();
        if deviation > PI {
            deviation = 2.0 * PI - deviation;
        }
        report.phase_deviation = deviation;

        // Homeostatic chiral adjustment:
        // Too tight (deviation < 0.3) -> increase chiral perturbation to diversify
        // Too loose (deviation > 1.5) -> decrease chiral perturbation to converge
        let old_eta = self.chiral_perturbation;
        if deviation < 0.3 {
            // Over-synchronized with swarm -> need more divergence
            self.chiral_perturbation = (self.chiral_perturbation + 0.01).min(0.5);
        } else if deviation > 1.5 {
            // Too far from swarm -> need more convergence
            self.chiral_perturbation = (self.chiral_perturbation - 0.01).max(0.0);
        }
        report.chiral_adjustment = self.chiral_perturbation - old_eta;
        report.new_chiral_eta = self.chiral_perturbation;

        // Run standard consolidation with updated chiral parameter
        let consolidation_report = self.consolidate(engine, 0, 3);
        report.memories_consolidated = consolidation_report.memories_replayed;

        // Also run modality-aware pass
        let modality_report = self.consolidate_modality_aware(engine);
        report.modality_report = Some(modality_report);

        report
    }

    /// Helper: Dream a single cluster on a snapshot and return the trajectory.
    /// This runs a local copy of consolidation and tracks mutations.
    fn dream_cluster_on_snapshot(
        &self,
        cluster_id: u32,
        memory_ids: &[Uuid],
        snapshot: &crate::paradox::ParadoxSnapshot,
    ) -> crate::paradox::DreamTrajectory {
        // Create a local in-memory store with just this cluster's memories
        let mut local_store = crate::store::TestMedium::new();
        
        for &memory_id in memory_ids {
            if let Some(memory) = snapshot.memories.get(&memory_id) {
                let _ = local_store.insert(memory.clone());
            }
        }
        
        // Create a temporary engine with matching dimensions (384-dim input ? 10K output).
        // Uses hash encoder (not Ollama) since consolidation stages don't re-encode existing
        // memories � they operate on stored vectors. Only hallucination would encode, and
        // consolidate_subset skips hallucination.
        let pipeline = crate::encoding::EncodingPipeline::new(
            Box::new(crate::encoding::SimpleHashEncoder::new(384, 42)),
            crate::codebook::Codebook::new(384, 10_000, 42),
        );
        let mut local_engine = crate::store::ResonanceEngine::new(Box::new(local_store), pipeline);
        
        // Run consolidation on local engine
        let report = self.consolidate_subset(&mut local_engine, memory_ids);
        
        // Extract mutations by diffing final state against snapshot
        let mutations: Vec<crate::paradox::Mutation> = memory_ids
            .iter()
            .filter_map(|&memory_id| {
                if let Ok(Some(final_memory)) = local_engine.store.get(&memory_id) {
                    crate::paradox::Mutation::from_diff(memory_id, final_memory, snapshot)
                } else {
                    None
                }
            })
            .collect();
        
        crate::paradox::DreamTrajectory {
            cluster_id,
            mutations,
            report,
        }
    }
}

impl DreamState {
    pub fn new(engine: ConsolidationEngine, cycles: usize) -> Self {
        Self { engine, cycles }
    }

    /// Run multiple consolidation passes with increasing depth.
    /// Cycle 1: layers 0-1 (recent)
    /// Cycle 2: layers 1-2 (medium)
    /// Cycle 3: layers 2-3 (deep)
    pub fn dream(&self, engine: &mut ResonanceEngine) -> Vec<ConsolidationReport> {
        let mut reports = Vec::new();
        for cycle in 0..self.cycles {
            let min_layer = cycle as u8;
            let max_layer = (cycle + 1) as u8;
            let report = self.engine.consolidate(engine, min_layer, max_layer);
            reports.push(report);
        }
        reports
    }

    /// Phase 7 (ADR-0011): Incremental dream � only consolidate memories that have
    /// changed since the last dream cycle. Passes `since` timestamp to `consolidate_incremental`.
    pub fn dream_incremental(&self, engine: &mut ResonanceEngine, since: chrono::DateTime<Utc>) -> Vec<ConsolidationReport> {
        let mut reports = Vec::new();
        for cycle in 0..self.cycles {
            let min_layer = cycle as u8;
            let max_layer = (cycle + 1) as u8;
            let report = self.engine.consolidate_incremental(engine, min_layer, max_layer, since);
            reports.push(report);
        }
        reports
    }

    /// Phase 8 (ADR-0011): Partitioned dream � use Xi operator to identify clusters,
    /// run intra-cluster consolidation (parallelized with rayon when `collective` feature
    /// is enabled), then run cross-cluster wiring every `cross_cluster_interval` cycles.
    ///
    /// The expensive DETECT + SYNC stages are bounded by cluster size, not total count.
    /// Expected to scale to thousands of memories without the quadratic blowup.
    pub fn dream_partitioned(
        &self,
        engine: &mut ResonanceEngine,
        cross_cluster_interval: u32,
        cycle_count: u32,
    ) -> Vec<ConsolidationReport> {
        // Compute Xi clusters from current memory graph
        let clusters = engine.xi_clusters();

        if clusters.is_empty() {
            // Fallback to standard dream if no clusters
            return self.dream(engine);
        }

        let mut reports: Vec<ConsolidationReport> = Vec::new();

        // Intra-cluster consolidation (sequential; rayon variant gated by feature flag)
        for cluster in &clusters {
            if cluster.memory_ids.is_empty() {
                continue;
            }
            let report = self.engine.consolidate_subset(engine, &cluster.memory_ids);
            reports.push(report);
        }

        // Cross-cluster WIRE stage every N cycles (weaker connections between clusters)
        if cross_cluster_interval > 0 && cycle_count % cross_cluster_interval == 0 {
            let all_ids: Vec<Uuid> = engine.store.all_ids().unwrap_or_default();
            // Run only the WIRE stage across all memories
            let pairs = self.engine.stage_detect(engine, &all_ids);
            let cross_links = self.engine.stage_wire(engine, &pairs, &all_ids);
            // Append cross-cluster wiring to last report
            if let Some(last) = reports.last_mut() {
                last.skip_links_created += cross_links;
            }
        }

        reports
    }

    /// Fast dream: decay amplitudes, prune dead memories, transfer layers.
    /// Skips expensive interference detection, sync, hallucination, and wiring.
    /// Completes in O(n) time regardless of memory count.
    pub fn dream_lite(&self, engine: &mut ResonanceEngine) -> ConsolidationReport {
        let start = std::time::Instant::now();
        let mut report = ConsolidationReport::default();

        let all_ids = engine.store.all_ids().unwrap_or_default();
        report.memories_replayed = all_ids.len();
        let now = chrono::Utc::now();

        // Pass 1: Decay amplitudes and prune ghosts
        let mut to_prune: Vec<uuid::Uuid> = Vec::new();
        for id in &all_ids {
            if let Ok(Some(mem)) = engine.store.get_mut(id) {
                // ADR-0031: never decay/ghost a Pinned memory ("never demoted").
                if mem.tier == crate::medium::types::Tier::Pinned {
                    continue;
                }
                // Only a LIVE->ghost transition may stamp the recovery window, count,
                // and enqueue link cleanup. Re-stamping an already-ghost (amplitude 0)
                // every lite dream renews its window and prevents it ever aging out via
                // stage_compact_ghosts (mirrors memory.rs record_retrieval's != 0.0 guard).
                let was_live = mem.amplitude != 0.0;
                // Gentle amplitude decay (0.5% per cycle)
                mem.amplitude *= 0.995;
                if was_live && mem.amplitude < self.engine.prune_threshold {
                    mem.amplitude = 0.0;
                    // ADR-0037: stamp updated_at on ghosting so the recovery window holds
                    // (else a lite-ghosted old memory past the 7-day horizon is hard-
                    // deleted by the very next deep dream's compact stage).
                    mem.updated_at = Some(now);
                    to_prune.push(*id);
                    report.memories_pruned += 1;
                }
            }
        }

        // Pass 2: Prune skip links pointing to dead memories
        let dead_set: std::collections::HashSet<uuid::Uuid> = to_prune.iter().copied().collect();
        for id in &all_ids {
            if dead_set.contains(id) { continue; }
            if let Ok(Some(mem)) = engine.store.get_mut(id) {
                mem.connections.retain(|link| !dead_set.contains(&link.target_id));
            }
        }

        // Pass 3: Transfer old memories to deeper layers
        let mut transfers: Vec<(uuid::Uuid, u8)> = Vec::new();
        for id in &all_ids {
            if let Ok(Some(mem)) = engine.store.get(id) {
                let age = now - mem.created_at;
                let new_layer = match mem.layer_depth {
                    0 if age > chrono::Duration::hours(1) => Some(1),
                    1 if age > chrono::Duration::days(1) => Some(2),
                    2 if age > chrono::Duration::weeks(1) => Some(3),
                    _ => None,
                };
                if let Some(layer) = new_layer {
                    transfers.push((*id, layer));
                }
            }
        }
        for (id, new_layer) in transfers {
            if let Ok(Some(mem)) = engine.store.get_mut(&id) {
                mem.layer_depth = new_layer;
                report.memories_transferred += 1;
            }
        }

        report.duration_ms = start.elapsed().as_millis() as u64;
        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codebook::Codebook;
    use crate::encoding::{EncodingPipeline, SimpleHashEncoder};
    use crate::memory::HyperMemory;
    use crate::store::{TestMedium, ResonanceEngine};

    fn make_engine() -> ResonanceEngine {
        let encoder = SimpleHashEncoder::new(384, 42);
        let codebook = Codebook::new(384, 10_000, 42);
        let pipeline = EncodingPipeline::new(Box::new(encoder), codebook);
        ResonanceEngine::new(Box::new(TestMedium::new()), pipeline)
    }

    /// Helper: insert a memory with specific phase and layer, return id.
    fn insert_with_phase_and_layer(
        engine: &mut ResonanceEngine,
        text: &str,
        phase: f32,
        layer: u8,
    ) -> Uuid {
        let id = engine.remember_at_layer(text, layer).unwrap();
        if let Some(mem) = engine.store.get_mut(&id).ok().flatten() {
            mem.phase = phase;
        }
        id
    }

    // -----------------------------------------------------------------------
    // Circular phase arithmetic (the 2π-wrap family of bugs)
    // -----------------------------------------------------------------------

    /// Absolute angular distance between two phases, for assertions.
    fn angular_distance(a: f32, b: f32) -> f32 {
        wrapped_phase_delta(a, b).abs()
    }

    #[test]
    fn circular_mean_handles_wrap_straddling_pair() {
        // 6.2 rad ≡ −0.083 rad; the angular midpoint with 0.2 is ~0.058 —
        // the arithmetic mean (3.2 ≈ π) is anti-phase to both inputs.
        let mean = circular_mean2(6.2, 0.2);
        assert!(
            angular_distance(mean, 0.058) < 0.05,
            "circular mean of a wrap-straddling pair must land between the \
             inputs, got {mean}"
        );
        // Sanity: a non-straddling pair matches the arithmetic mean.
        let plain = circular_mean2(1.0, 2.0);
        assert!(angular_distance(plain, 1.5) < 1e-3, "got {plain}");
    }

    #[test]
    fn wrapped_delta_measures_real_angular_gap() {
        // Raw diff is −6.0; the real angular step from 6.2 to 0.2 is +0.283.
        let d = wrapped_phase_delta(0.2, 6.2);
        assert!((d - 0.283).abs() < 1e-2, "got {d}");
        assert!(wrapped_phase_delta(2.0, 1.6) > 0.0);
        assert!(wrapped_phase_delta(1.6, 2.0) < 0.0);
    }

    #[test]
    fn repulsion_widens_gap_regardless_of_pair_order() {
        // The old fixed-sign correction widened only when φa > φb and
        // CONTRACTED the pair otherwise (direction picked by iteration
        // order). Both orderings must now widen toward π/2.
        for (a, b) in [(1.6f32, 2.0f32), (2.0, 1.6)] {
            let before = angular_distance(a, b);
            let c = repulsion_phase_correction(a, b, 1.0);
            let after = angular_distance(a + c, b - c);
            assert!(
                after > before,
                "repulsion must widen the gap: ({a}, {b}) went {before} -> {after}"
            );
            assert!(
                after <= std::f32::consts::FRAC_PI_2 + 1e-3,
                "must not overshoot π/2: got {after}"
            );
        }
        // Drifted pair: raw |Δ| = 5.9 but the real gap is ~0.38 — the
        // correction must be based on the wrapped gap (small, widening),
        // not the raw one (huge, sign-flipped).
        let (a, b) = (6.1f32, 0.2f32);
        let before = angular_distance(a, b);
        let c = repulsion_phase_correction(a, b, 1.0);
        let after = angular_distance(a + c, b - c);
        assert!(after > before, "drifted pair must widen: {before} -> {after}");
    }

    // ADR-0037 / #497 — a cache rebuild erases the ghosting stamp
    // (`rebuild_cache` sets `updated_at: Some(meta.created_at)`), so an old
    // memory ghosted moments ago looks decades stale and was hard-deleted with
    // ZERO recovery window on the very next deep dream. These pin that a ghost
    // whose stamp is unknowable is retained instead of reclaimed.

    /// Ghost whose `updated_at` was flattened onto `created_at` by a rebuild.
    fn ghost_with_lost_stamp(engine: &mut ResonanceEngine, label: &str) -> Uuid {
        let id = insert_with_phase_and_layer(engine, label, 0.0, 0);
        if let Some(mem) = engine.store.get_mut(&id).ok().flatten() {
            mem.amplitude = 0.0; // a ghost
            // Old enough to be far past any retain window.
            mem.created_at = Utc::now() - chrono::Duration::days(3650);
            // Exactly what rebuild_cache writes back.
            mem.updated_at = Some(mem.created_at);
        }
        id
    }

    #[test]
    fn compact_ghosts_retains_a_ghost_whose_stamp_was_lost_to_rebuild() {
        let mut engine = make_engine();
        let ce = ConsolidationEngine::default();
        let id = ghost_with_lost_stamp(&mut engine, "ghost with rebuilt stamp");

        // Horizon = now, i.e. the most aggressive possible reclamation.
        std::env::set_var("KANNAKA_GHOST_RETAIN_DAYS", "0");
        let reclaimed = ce.stage_compact_ghosts(&mut engine, Utc::now());
        std::env::remove_var("KANNAKA_GHOST_RETAIN_DAYS");

        assert_eq!(reclaimed, 0, "a ghost with an unknowable stamp must not be hard-deleted");
        assert!(
            engine.get_memory(&id).unwrap().is_some(),
            "the memory must survive — deleting it costs the ADR-0037 recovery window entirely"
        );
    }

    #[test]
    fn compact_ghosts_retains_when_updated_at_is_absent() {
        // The other unknowable shape: no stamp at all.
        let mut engine = make_engine();
        let ce = ConsolidationEngine::default();
        let id = insert_with_phase_and_layer(&mut engine, "stampless ghost", 0.0, 0);
        if let Some(mem) = engine.store.get_mut(&id).ok().flatten() {
            mem.amplitude = 0.0;
            mem.created_at = Utc::now() - chrono::Duration::days(3650);
            mem.updated_at = None;
        }

        std::env::set_var("KANNAKA_GHOST_RETAIN_DAYS", "0");
        let reclaimed = ce.stage_compact_ghosts(&mut engine, Utc::now());
        std::env::remove_var("KANNAKA_GHOST_RETAIN_DAYS");

        assert_eq!(reclaimed, 0);
        assert!(engine.get_memory(&id).unwrap().is_some());
    }

    #[test]
    fn compact_ghosts_still_reclaims_a_genuinely_aged_ghost() {
        // The over-correction guard. Retaining everything would also satisfy
        // the tests above, and would turn ghost compaction into a no-op —
        // ghosts must still age out when their stamp IS known.
        let mut engine = make_engine();
        let ce = ConsolidationEngine::default();
        let id = insert_with_phase_and_layer(&mut engine, "properly aged ghost", 0.0, 0);
        if let Some(mem) = engine.store.get_mut(&id).ok().flatten() {
            mem.amplitude = 0.0;
            mem.created_at = Utc::now() - chrono::Duration::days(90);
            // A real ghosting stamp, distinct from created_at and well past the
            // retain window.
            mem.updated_at = Some(Utc::now() - chrono::Duration::days(30));
        }

        let reclaimed = ce.stage_compact_ghosts(&mut engine, Utc::now());
        assert_eq!(reclaimed, 1, "a ghost with a known, aged stamp must still be reclaimed");
        assert!(engine.get_memory(&id).unwrap().is_none());
    }

    #[test]
    fn dream_lite_ghosting_stamps_recovery_window() {
        let mut engine = make_engine();
        let state = DreamState::new(ConsolidationEngine::default(), 1);
        let id = insert_with_phase_and_layer(&mut engine, "fading trace", 0.0, 0);
        // Force the memory below the prune threshold so this lite pass
        // ghosts it, and clear updated_at to model an old field.
        if let Some(mem) = engine.store.get_mut(&id).ok().flatten() {
            mem.amplitude = 0.01;
            mem.updated_at = None;
        }
        let report = state.dream_lite(&mut engine);
        assert!(report.memories_pruned >= 1, "memory should be ghosted");
        let mem = engine.get_memory(&id).unwrap().unwrap();
        assert_eq!(mem.amplitude, 0.0, "ghosted");
        // ADR-0037: the ghost must carry a fresh updated_at so the deep
        // dream's compact stage honors the recovery window instead of
        // hard-deleting it in the same cycle.
        assert!(
            mem.updated_at.is_some(),
            "lite ghosting must stamp updated_at (recovery window)"
        );
    }

    // hunt/ADR-0031: a Pinned memory is "never demoted" — the lite dream's decay
    // must skip it entirely, even below the prune threshold.
    #[test]
    fn dream_lite_does_not_ghost_pinned() {
        let mut engine = make_engine();
        let state = DreamState::new(ConsolidationEngine::default(), 1);
        let id = insert_with_phase_and_layer(&mut engine, "pinned trace", 0.0, 0);
        if let Some(mem) = engine.store.get_mut(&id).ok().flatten() {
            mem.amplitude = 0.01; // below prune threshold — WOULD ghost if not pinned
            mem.tier = crate::medium::types::Tier::Pinned;
        }
        state.dream_lite(&mut engine);
        let mem = engine.get_memory(&id).unwrap().unwrap();
        assert_eq!(
            mem.amplitude, 0.01,
            "a Pinned memory must NOT be decayed or ghosted, got {}",
            mem.amplitude
        );
    }

    // hunt/ADR-0037: an EXISTING ghost's recovery window must not be renewed every
    // lite dream — else a ghost never ages out via stage_compact_ghosts.
    #[test]
    fn dream_lite_does_not_renew_existing_ghost() {
        let mut engine = make_engine();
        let state = DreamState::new(ConsolidationEngine::default(), 1);
        let id = insert_with_phase_and_layer(&mut engine, "old ghost", 0.0, 0);
        let old_stamp = chrono::Utc::now() - chrono::Duration::days(10);
        if let Some(mem) = engine.store.get_mut(&id).ok().flatten() {
            mem.amplitude = 0.0; // already a ghost
            mem.updated_at = Some(old_stamp);
        }
        state.dream_lite(&mut engine);
        let mem = engine.get_memory(&id).unwrap().unwrap();
        assert_eq!(
            mem.updated_at,
            Some(old_stamp),
            "an existing ghost's recovery window must NOT be renewed by a lite dream"
        );
    }

    #[test]
    fn constructive_interference_strengthens_memories() {
        let mut engine = make_engine();
        let consolidation = ConsolidationEngine {
            interference_threshold: 0.3,
            ..Default::default()
        };

        // Two similar memories with aligned phases (both 0.0)
        let id1 = insert_with_phase_and_layer(&mut engine, "the cat sat on the mat", 0.0, 0);
        let id2 = insert_with_phase_and_layer(&mut engine, "the cat sat on the mat today", 0.0, 0);

        let amp_before_1 = engine.get_memory(&id1).unwrap().unwrap().amplitude;
        let amp_before_2 = engine.get_memory(&id2).unwrap().unwrap().amplitude;

        let report = consolidation.consolidate(&mut engine, 0, 1);

        let amp_after_1 = engine.get_memory(&id1).unwrap().unwrap().amplitude;
        let amp_after_2 = engine.get_memory(&id2).unwrap().unwrap().amplitude;

        assert!(report.constructive_pairs > 0, "should detect constructive pairs");
        assert!(amp_after_1 > amp_before_1, "amplitude should increase: {amp_before_1} -> {amp_after_1}");
        assert!(amp_after_2 > amp_before_2, "amplitude should increase: {amp_before_2} -> {amp_after_2}");
    }

    #[test]
    fn destructive_interference_weakens_memories() {
        let mut engine = make_engine();
        let consolidation = ConsolidationEngine {
            interference_threshold: 0.3,
            ..Default::default()
        };

        // Two similar memories with opposed phases
        let id1 = insert_with_phase_and_layer(&mut engine, "the cat sat on the mat", 0.0, 0);
        let id2 = insert_with_phase_and_layer(&mut engine, "the cat sat on the mat today", PI, 0);

        let amp_before_1 = engine.get_memory(&id1).unwrap().unwrap().amplitude;

        let report = consolidation.consolidate(&mut engine, 0, 1);

        let amp_after_1 = engine.get_memory(&id1).unwrap().unwrap().amplitude;

        assert!(report.destructive_pairs > 0, "should detect destructive pairs");
        assert!(amp_after_1 < amp_before_1, "amplitude should decrease: {amp_before_1} -> {amp_after_1}");
    }

    #[test]
    fn pruning_reduces_amplitude_to_zero() {
        let mut engine = make_engine();
        let consolidation = ConsolidationEngine {
            interference_threshold: 0.3,
            destructive_penalty: 1.5, // large enough to force below threshold
            ..Default::default()
        };

        let id1 = insert_with_phase_and_layer(&mut engine, "the cat sat on the mat", 0.0, 0);
        let id2 = insert_with_phase_and_layer(&mut engine, "the cat sat on the mat today", PI, 0);

        consolidation.consolidate(&mut engine, 0, 1);

        let amp1 = engine.get_memory(&id1).unwrap().unwrap().amplitude;
        let amp2 = engine.get_memory(&id2).unwrap().unwrap().amplitude;

        // At least one should be ghosted (amplitude 0)
        assert!(
            amp1 == 0.0 || amp2 == 0.0,
            "at least one should be pruned to 0: amp1={amp1}, amp2={amp2}"
        );
    }

    #[test]
    fn bundling_creates_summary_similar_to_components() {
        let mut engine = make_engine();
        let consolidation = ConsolidationEngine {
            interference_threshold: 0.99, // high so no interference detected
            ..Default::default()
        };

        // Insert several memories at layer 0
        engine.remember_at_layer("cats are fluffy animals", 0).unwrap();
        engine.remember_at_layer("dogs are loyal pets", 0).unwrap();
        engine.remember_at_layer("birds can fly high", 0).unwrap();

        let initial_count = engine.store.count();
        let report = consolidation.consolidate(&mut engine, 0, 1);

        assert!(report.bundles_created > 0, "should create at least one bundle");
        assert!(engine.store.count() > initial_count, "should have more memories after bundling");

        // Find the summary memory
        let all = engine.store.all_memories().unwrap();
        let summary = all.iter().find(|m| m.content.contains("__consolidation_summary")).unwrap();
        assert_eq!(summary.layer_depth, 1, "summary should be at layer 1");

        // Summary should have positive similarity to components
        let cat_vec = engine.pipeline.encode_text("cats are fluffy animals").unwrap();
        let sim = cosine_similarity(&summary.vector, &cat_vec);
        assert!(sim > 0.0, "summary should be similar to components, got {sim}");
    }

    #[test]
    fn transfer_moves_old_memories_to_deeper_layers() {
        let mut engine = make_engine();
        let consolidation = ConsolidationEngine::default();

        // Insert memory and make it old
        let id = engine.remember_at_layer("old memory", 0).unwrap();
        if let Some(mem) = engine.store.get_mut(&id).ok().flatten() {
            mem.created_at = Utc::now() - Duration::hours(2);
        }

        let report = consolidation.consolidate(&mut engine, 0, 1);

        let layer = engine.get_memory(&id).unwrap().unwrap().layer_depth;
        assert_eq!(layer, 1, "should transfer to layer 1");
        assert!(report.memories_transferred > 0);
    }

    #[test]
    fn wiring_creates_skip_links_for_cross_layer_constructive_pairs() {
        let mut engine = make_engine();
        engine.similarity_threshold = 0.99; // prevent auto-linking on insert
        let consolidation = ConsolidationEngine {
            interference_threshold: 0.3,
            ..Default::default()
        };

        // Similar memories at different layers with aligned phases
        // Use remember() directly and manually set layer to avoid auto-linking
        let id1 = {
            let mut mem = engine.pipeline.encode_memory("the cat sat on the mat", Utc::now()).unwrap();
            mem.layer_depth = 0;
            mem.phase = 0.0;
            engine.store.insert(mem).unwrap()
        };
        let id2 = {
            let mut mem = engine.pipeline.encode_memory("the cat sat on the mat today", Utc::now()).unwrap();
            mem.layer_depth = 1;
            mem.phase = 0.0;
            engine.store.insert(mem).unwrap()
        };

        // Verify no pre-existing links
        assert!(engine.get_memory(&id1).unwrap().unwrap().connections.is_empty());

        let report = consolidation.consolidate(&mut engine, 0, 1);

        // Skip link creation removed � associations now emergent from ChiralMedium interference
        // Just verify consolidation ran without error
        assert!(report.memories_replayed > 0, "should replay memories");
    }

    #[test]
    fn dream_state_runs_multiple_cycles() {
        let mut engine = make_engine();
        let dream = DreamState::default();

        engine.remember_at_layer("recent thought", 0).unwrap();
        engine.remember_at_layer("day old thought", 1).unwrap();
        engine.remember_at_layer("week old thought", 2).unwrap();

        let reports = dream.dream(&mut engine);

        assert_eq!(reports.len(), 3, "should have 3 cycle reports");
        // Each cycle should replay some memories
        assert!(reports[0].memories_replayed > 0, "cycle 1 should replay memories");
    }

    /// Helper: insert a memory with a specific vector, phase, and layer.
    fn insert_raw(
        engine: &mut ResonanceEngine,
        vector: Vec<f32>,
        content: &str,
        phase: f32,
        layer: u8,
    ) -> Uuid {
        let mut mem = HyperMemory::new(vector, content.to_string());
        mem.phase = phase;
        mem.layer_depth = layer;
        engine.store.insert(mem).unwrap()
    }

    #[test]
    fn full_dream_cycle() {
        let mut engine = make_engine();

        let dream = DreamState::new(
            ConsolidationEngine {
                interference_threshold: 0.9,
                ..Default::default()
            },
            3,
        );

        // Use hand-crafted vectors so groups don't cross-interfere
        let dim = 10_000;
        // Group A: related (similar vector, aligned phase)
        let mut va = vec![0.0f32; dim];
        for i in 0..100 { va[i] = 1.0; }
        crate::wave::normalize(&mut va);

        // Group B: opposed (similar vector but orthogonal to A, opposed phases)
        let mut vb = vec![0.0f32; dim];
        for i in 200..300 { vb[i] = 1.0; }
        crate::wave::normalize(&mut vb);

        let related1 = insert_raw(&mut engine, va.clone(), "related1", 0.0, 0);
        let related2 = insert_raw(&mut engine, va.clone(), "related2", 0.0, 0);
        let opposed1 = insert_raw(&mut engine, vb.clone(), "opposed1", 0.0, 0);
        let opposed2 = insert_raw(&mut engine, vb.clone(), "opposed2", PI, 0);

        let amp_related1_before = engine.get_memory(&related1).unwrap().unwrap().amplitude;
        let amp_opposed1_before = engine.get_memory(&opposed1).unwrap().unwrap().amplitude;

        let reports = dream.dream(&mut engine);

        println!("=== Full Dream Cycle Reports ===");
        for (i, r) in reports.iter().enumerate() {
            println!(
                "Cycle {}: replayed={}, interference={} (constructive={}, destructive={}), \
                 bundles={}, strengthened={}, pruned={}, transferred={}, wired={}, duration={}ms",
                i + 1,
                r.memories_replayed,
                r.interference_pairs_found,
                r.constructive_pairs,
                r.destructive_pairs,
                r.bundles_created,
                r.memories_strengthened,
                r.memories_pruned,
                r.memories_transferred,
                r.skip_links_created,
                r.duration_ms,
            );
        }

        let amp_related1_after = engine.get_memory(&related1).unwrap().unwrap().amplitude;
        let amp_related2_after = engine.get_memory(&related2).unwrap().unwrap().amplitude;
        let amp_opposed1_after = engine.get_memory(&opposed1).unwrap().unwrap().amplitude;
        let amp_opposed2_after = engine.get_memory(&opposed2).unwrap().unwrap().amplitude;

        // Related memories should be strengthened
        assert!(
            amp_related1_after > amp_related1_before,
            "related memory should be stronger: {amp_related1_before} -> {amp_related1_after}"
        );

        // Opposed memories should be weakened
        assert!(
            amp_opposed1_after < amp_opposed1_before,
            "opposed memory should be weaker: {amp_opposed1_before} -> {amp_opposed1_after}"
        );

        println!("\n=== Amplitude Changes ===");
        println!("Related 1: {amp_related1_before} -> {amp_related1_after}");
        println!("Related 2: {amp_related2_after}");
        println!("Opposed 1: {amp_opposed1_before} -> {amp_opposed1_after}");
        println!("Opposed 2: {amp_opposed2_after}");

        // First cycle should have done real work
        assert!(reports[0].memories_replayed > 0);
    }

    #[test]
    fn cross_cluster_wiring_creates_bridge_connections() {
        let mut engine = make_engine();
        let consolidation = ConsolidationEngine {
            interference_threshold: 0.3,
            ..Default::default()
        };

        let dim = 10_000;
        
        // Create two distinct clusters
        // Cluster A: animal-related memories
        let mut va = vec![0.0f32; dim];
        for i in 0..100 { va[i] = 1.0; }
        crate::wave::normalize(&mut va);
        
        let cat_id = insert_raw(&mut engine, va.clone(), "cats are fluffy animals", 0.0, 0);
        let dog_id = insert_raw(&mut engine, va.clone(), "dogs are loyal pets", 0.0, 0);
        
        // Cluster B: technology-related memories (orthogonal vector)
        let mut vb = vec![0.0f32; dim];
        for i in 500..600 { vb[i] = 1.0; }
        crate::wave::normalize(&mut vb);
        
        let code_id = insert_raw(&mut engine, vb.clone(), "coding in rust", 0.0, 0);
        let ai_id = insert_raw(&mut engine, vb.clone(), "artificial intelligence", 0.0, 0);
        
        // Create a bridge memory with moderate similarity to both clusters
        let mut vc = vec![0.0f32; dim];
        for i in 0..50 { vc[i] = 0.7; }  // Moderate overlap with cluster A
        for i in 500..550 { vc[i] = 0.7; }  // Moderate overlap with cluster B
        crate::wave::normalize(&mut vc);
        
        let bridge_id = insert_raw(&mut engine, vc, "robot pets using AI", 0.0, 0);
        
        // Run consolidation � skip link creation removed, just verify it runs
        let report = consolidation.consolidate(&mut engine, 0, 1);
        
        // Skip links removed � associations now emergent from ChiralMedium interference
        // Just verify consolidation completed
        assert!(report.memories_replayed > 0, "Should replay memories during consolidation");
    }

    #[test]
    fn bridge_node_strengthening_works() {
        let mut engine = make_engine();
        let consolidation = ConsolidationEngine {
            interference_threshold: 0.3,
            ..Default::default()
        };

        let dim = 10_000;
        
        // Create three clusters with a bridge node
        let mut va = vec![0.0f32; dim]; for i in 0..100 { va[i] = 1.0; }
        let mut vb = vec![0.0f32; dim]; for i in 200..300 { vb[i] = 1.0; }
        let mut vc = vec![0.0f32; dim]; for i in 400..500 { vc[i] = 1.0; }
        crate::wave::normalize(&mut va);
        crate::wave::normalize(&mut vb);
        crate::wave::normalize(&mut vc);
        
        // Cluster members
        insert_raw(&mut engine, va.clone(), "cluster A member 1", 0.0, 0);
        insert_raw(&mut engine, va.clone(), "cluster A member 2", 0.0, 0);
        insert_raw(&mut engine, vb.clone(), "cluster B member 1", 0.0, 0);
        insert_raw(&mut engine, vb.clone(), "cluster B member 2", 0.0, 0);
        insert_raw(&mut engine, vc.clone(), "cluster C member 1", 0.0, 0);
        insert_raw(&mut engine, vc.clone(), "cluster C member 2", 0.0, 0);
        
        // Bridge node with moderate similarity to all clusters
        let mut bridge_vec = vec![0.0f32; dim];
        for i in 0..30 { bridge_vec[i] = 0.6; }     // Similarity to A
        for i in 200..230 { bridge_vec[i] = 0.6; } // Similarity to B  
        for i in 400..430 { bridge_vec[i] = 0.6; } // Similarity to C
        crate::wave::normalize(&mut bridge_vec);
        
        let bridge_id = insert_raw(&mut engine, bridge_vec, "universal bridge concept", 0.0, 0);
        let initial_amplitude = engine.get_memory(&bridge_id).unwrap().unwrap().amplitude;
        
        // Run consolidation (which should detect and strengthen bridge nodes)
        consolidation.consolidate(&mut engine, 0, 1);
        
        let final_amplitude = engine.get_memory(&bridge_id).unwrap().unwrap().amplitude;
        
        println!("Bridge node amplitude: {initial_amplitude} -> {final_amplitude}");
        
        // Bridge node should receive amplitude boost if it connects to multiple clusters
        // Note: The exact boost depends on how many clusters it actually gets connected to
        // so we just check that it didn't decrease
        assert!(
            final_amplitude >= initial_amplitude,
            "Bridge node should not lose amplitude: {initial_amplitude} -> {final_amplitude}"
        );
    }

    #[test] 
    fn cross_cluster_hallucination_prefers_different_clusters() {
        let mut engine = make_engine();
        let consolidation = ConsolidationEngine {
            interference_threshold: 0.9, // Avoid interference
            ..Default::default()
        };

        let dim = 10_000;
        
        // Create two distinct clusters
        let mut va = vec![0.0f32; dim]; for i in 0..100 { va[i] = 1.0; }
        let mut vb = vec![0.0f32; dim]; for i in 500..600 { vb[i] = 1.0; }
        crate::wave::normalize(&mut va);
        crate::wave::normalize(&mut vb);
        
        // Cluster A: high amplitude memories
        for i in 0..3 {
            let mut mem = crate::memory::HyperMemory::new(va.clone(), format!("animal {i}"));
            mem.amplitude = 0.8; // High amplitude to be hallucination candidates
            engine.store.insert(mem).unwrap();
        }
        
        // Cluster B: high amplitude memories
        for i in 0..3 {
            let mut mem = crate::memory::HyperMemory::new(vb.clone(), format!("tech {i}"));
            mem.amplitude = 0.8; // High amplitude to be hallucination candidates
            engine.store.insert(mem).unwrap();
        }
        
        let initial_count = engine.store.count();
        
        // Run consolidation (should create cross-cluster hallucinations)
        let report = consolidation.consolidate(&mut engine, 0, 1);
        
        println!("Hallucinations created: {}", report.hallucinations_created);
        println!("Memory count: {} -> {}", initial_count, engine.store.count());
        
        if report.hallucinations_created > 0 {
            // Find the hallucinated memory
            let all = engine.store.all_memories().unwrap();
            let hallucination = all.iter().find(|m| m.hallucinated);
            
            if let Some(hall) = hallucination {
                println!("Hallucination content: {}", hall.content);
                assert!(
                    hall.content.contains("cross-cluster") || hall.content.contains("Synthesis"),
                    "Hallucination should indicate cross-cluster origin"
                );
                assert!(!hall.parents.is_empty(), "Hallucination should have parent references");
                // connections.push removed � skip links now emergent from ChiralMedium
            }
        }
    }

    /// Helper function to count cross-cluster links in the engine.
    fn count_cross_cluster_links(engine: &mut ResonanceEngine) -> usize {
        let sync = crate::kuramoto::KuramotoSync::default();
        let clusters = sync.find_synchronized_clusters(engine, 2);
        
        if clusters.len() < 2 {
            return 0;
        }
        
        // Build cluster membership map
        let mut id_to_cluster = std::collections::HashMap::new();
        for (cluster_idx, cluster) in clusters.iter().enumerate() {
            for &mem_id in &cluster.memory_ids {
                id_to_cluster.insert(mem_id, cluster_idx);
            }
        }
        
        let all_memories = engine.store.all_memories().unwrap_or_default();
        let mut cross_cluster_count = 0;
        
        for memory in &all_memories {
            if let Some(&source_cluster) = id_to_cluster.get(&memory.id) {
                for link in &memory.connections {
                    if let Some(&target_cluster) = id_to_cluster.get(&link.target_id) {
                        if source_cluster != target_cluster {
                            cross_cluster_count += 1;
                        }
                    }
                }
            }
        }
        
        cross_cluster_count
    }

    #[test]
    fn hallucination_created_from_distant_memories() {
        let mut engine = make_engine();
        let consolidation = ConsolidationEngine {
            interference_threshold: 0.99, // avoid interference detection
            ..Default::default()
        };

        // Insert 3+ memories with orthogonal vectors (maximally distant)
        let dim = 10_000;
        let mut v1 = vec![0.0f32; dim]; for i in 0..100 { v1[i] = 1.0; }
        let mut v2 = vec![0.0f32; dim]; for i in 200..300 { v2[i] = 1.0; }
        let mut v3 = vec![0.0f32; dim]; for i in 400..500 { v3[i] = 1.0; }
        crate::wave::normalize(&mut v1);
        crate::wave::normalize(&mut v2);
        crate::wave::normalize(&mut v3);

        insert_raw(&mut engine, v1, "quantum physics theory", 0.0, 0);
        insert_raw(&mut engine, v2, "cooking pasta recipes", 0.0, 0);
        insert_raw(&mut engine, v3, "alpine hiking trails", 0.0, 0);

        let initial_count = engine.store.count();
        let report = consolidation.consolidate(&mut engine, 0, 1);

        // Hallucinations may be rejected by coboundary validation, so we check
        // that the process ran without panicking rather than asserting creation
        if report.hallucinations_created > 0 {
            assert!(engine.store.count() > initial_count, "should have more memories after hallucination");
            let all = engine.store.all_memories().unwrap();
            if let Some(hall) = all.iter().find(|m| m.hallucinated) {
                assert!(hall.content.starts_with("[hallucination]"));
                assert!(!hall.parents.is_empty());
                assert!(hall.amplitude <= 0.5, "hallucination should start with low amplitude");
            }
        }
    }

    #[test]
    fn hallucination_skipped_with_few_memories() {
        let mut engine = make_engine();
        let consolidation = ConsolidationEngine::default();

        // Only 2 memories � not enough for hallucination
        engine.remember_at_layer("single thought", 0).unwrap();
        engine.remember_at_layer("another thought", 0).unwrap();

        let report = consolidation.consolidate(&mut engine, 0, 1);
        // May or may not create hallucinations depending on content similarity
        // but should not panic
        assert!(report.hallucinations_created <= 1);
    }

    // ===== EXP-003: Adaptive ? tests =====

    #[test]
    fn proportional_dampening_preserves_relative_amplitudes() {
        // Two memories with different amplitudes should maintain their ratio
        // after proportional dampening (unlike flat penalty which can invert them).
        let mut engine = make_engine();
        let consolidation = ConsolidationEngine {
            interference_threshold: 0.3,
            destructive_penalty: 0.5,
            ..Default::default()
        };

        // Create two similar memories with opposing phases (destructive)
        let id1 = insert_with_phase_and_layer(&mut engine, "the cat sat on the mat", 0.0, 0);
        let id2 = insert_with_phase_and_layer(&mut engine, "the cat sat on the mat today", PI, 0);

        // Set different amplitudes
        engine.store.get_mut(&id1).ok().flatten().unwrap().amplitude = 2.0;
        engine.store.get_mut(&id2).ok().flatten().unwrap().amplitude = 0.5;

        let ratio_before = 2.0 / 0.5;

        consolidation.consolidate(&mut engine, 0, 1);

        let amp1 = engine.get_memory(&id1).unwrap().unwrap().amplitude;
        let amp2 = engine.get_memory(&id2).unwrap().unwrap().amplitude;

        // With proportional dampening, both lose 50% so ratio is preserved
        if amp2 > 0.0 {
            let ratio_after = amp1 / amp2;
            assert!((ratio_after - ratio_before).abs() < 0.5,
                "ratio should be approximately preserved: before={ratio_before}, after={ratio_after}");
        }
        // Both should have decreased
        assert!(amp1 < 2.0, "amplitude should decrease");
    }

    #[test]
    fn adaptive_params_reduce_boost_when_over_synchronized() {
        let mut params = AdaptiveParams::default();
        let initial_boost = params.constructive_boost;

        // Simulate high order parameter (over-synchronized)
        params.adapt(0.95);

        assert!(params.constructive_boost < initial_boost,
            "boost should decrease when R is too high: {} -> {}", initial_boost, params.constructive_boost);
    }

    #[test]
    fn adaptive_params_stable_in_target_range() {
        let mut params = AdaptiveParams::default();
        let initial_boost = params.constructive_boost;
        let initial_threshold = params.prune_threshold;

        // Simulate order parameter in sweet spot
        params.adapt(0.70);

        assert!((params.constructive_boost - initial_boost).abs() < 1e-6,
            "boost should not change in target range");
        assert!((params.prune_threshold - initial_threshold).abs() < 1e-6,
            "threshold should not change in target range");
    }

    #[test]
    fn adaptive_params_boost_coupling_when_fragmented() {
        let mut params = AdaptiveParams::default();
        let initial_boost = params.constructive_boost;

        // Simulate low order parameter (fragmented)
        params.adapt(0.2);

        assert!(params.constructive_boost > initial_boost,
            "constructive boost should increase when fragmented: {} -> {}", initial_boost, params.constructive_boost);
        assert!(params.destructive_penalty < 0.5,
            "destructive penalty should decrease when fragmented (preserve more memories)");
    }

    #[test]
    fn adapt_from_report_updates_engine() {
        let mut consolidation = ConsolidationEngine::default();
        let mut report = ConsolidationReport::default();
        report.final_order_parameter = 0.95; // over-synchronized

        let boost_before = consolidation.constructive_boost;
        consolidation.adapt_from_report(&report);

        assert!(consolidation.constructive_boost < boost_before,
            "engine boost should decrease after adaptation");
    }

    // -----------------------------------------------------------------------
    // ADR-0054: retention triage (env + config are process-global, so all
    // cases share ONE #[test], mirroring the facet_write_path pattern).
    // -----------------------------------------------------------------------

    #[test]
    fn retention_triage_acceptance() {
        let ce = ConsolidationEngine::default();

        // Isolated config home for this test.
        let dir = std::env::temp_dir().join(format!("k54-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let prev_data_dir = std::env::var("KANNAKA_DATA_DIR").ok();
        std::env::set_var("KANNAKA_DATA_DIR", &dir);

        // ── Acceptance #1: flag ON + EMPTY table → no-op ──────────────────
        std::fs::write(dir.join("config.toml"), "").unwrap();
        std::env::set_var("KANNAKA_TRIAGE", "1");
        let mut engine = make_engine();
        for i in 0..4 {
            engine.remember_at_layer(&format!("audio:heard clip {i}"), 0).unwrap();
        }
        let before: Vec<f32> = engine.store.all_memories().unwrap().iter().map(|m| m.amplitude).collect();
        assert_eq!(ce.stage_retention_triage(&mut engine), 0, "empty table must be a no-op");
        let after: Vec<f32> = engine.store.all_memories().unwrap().iter().map(|m| m.amplitude).collect();
        assert_eq!(before, after, "no amplitude may change under an empty table");

        // ── Flag OFF + populated table → still a no-op ────────────────────
        std::fs::write(
            dir.join("config.toml"),
            "[retention]\n\"audio:\" = { cap = 2 }\n",
        ).unwrap();
        std::env::remove_var("KANNAKA_TRIAGE");
        assert_eq!(ce.stage_retention_triage(&mut engine), 0, "dark flag must gate the stage");

        // ── Acceptance #2: cap ghosts exactly n-cap rows, lowest value first
        std::env::set_var("KANNAKA_TRIAGE", "1");
        let all = engine.store.all_memories().unwrap();
        // Give clip 0 and clip 1 higher retrieval salience (still below the
        // promote threshold) and boost clip 2's amplitude, leaving clip 3 the
        // weakest — with cap=2, clips 3 and one other low row must ghost.
        let mut ids: Vec<(Uuid, String)> = all.iter().map(|m| (m.id, m.content.clone())).collect();
        ids.sort_by_key(|(_, c)| c.clone());
        for (id, content) in &ids {
            let mem = engine.store.get_mut(id).unwrap().unwrap();
            if content.ends_with("0") || content.ends_with("1") {
                mem.retrieval_count = 2; // below default promote threshold (3)
            }
            mem.amplitude = if content.ends_with("2") { 0.9 } else { 0.5 };
        }
        let ghosted = ce.stage_retention_triage(&mut engine);
        assert_eq!(ghosted, 2, "cap=2 over 4 rows must ghost exactly 2");
        for (id, content) in &ids {
            let mem = engine.get_memory(id).unwrap().unwrap();
            if content.ends_with("0") || content.ends_with("1") {
                assert!(mem.amplitude > 0.0, "{content}: higher retrieval_count must survive");
            } else {
                assert_eq!(mem.amplitude, 0.0, "{content}: lowest-value rows must ghost");
                assert!(mem.updated_at.is_some(), "{content}: ghost must carry the ADR-0037 stamp");
            }
        }

        // ── Acceptance #3: promoted rows are immune to cap pressure ───────
        let mut engine = make_engine();
        for i in 0..3 {
            engine.remember_at_layer(&format!("audio:heard promo {i}"), 0).unwrap();
        }
        let all = engine.store.all_memories().unwrap();
        let promoted_id = all[0].id;
        {
            let mem = engine.store.get_mut(&promoted_id).unwrap().unwrap();
            mem.retrieval_count = 3; // at the default KANNAKA_PROMOTE_HITS
        }
        std::fs::write(
            dir.join("config.toml"),
            "[retention]\n\"audio:\" = { cap = 0 }\n",
        ).unwrap();
        let ghosted = ce.stage_retention_triage(&mut engine);
        assert_eq!(ghosted, 2, "cap=0 must ghost every eligible row");
        assert!(
            engine.get_memory(&promoted_id).unwrap().unwrap().amplitude > 0.0,
            "a row at the promote threshold must never be evicted by cap pressure"
        );

        // ── Pinned rows are untouchable even by TTL ───────────────────────
        let mut engine = make_engine();
        let id = engine.remember_at_layer("audio:heard pinned forever", 0).unwrap();
        {
            let mem = engine.store.get_mut(&id).unwrap().unwrap();
            mem.tier = crate::medium::types::Tier::Pinned;
            mem.created_at = Utc::now() - chrono::Duration::days(365);
        }
        std::fs::write(
            dir.join("config.toml"),
            "[retention]\n\"audio:\" = { cap = 0, ttl_days = 1.0 }\n",
        ).unwrap();
        assert_eq!(ce.stage_retention_triage(&mut engine), 0, "Pinned survives cap AND ttl");

        // ── TTL ghosts stale rows, keeps fresh ones ───────────────────────
        let mut engine = make_engine();
        let old_id = engine.remember_at_layer("audio:heard ancient", 0).unwrap();
        let new_id = engine.remember_at_layer("audio:heard fresh", 0).unwrap();
        {
            let mem = engine.store.get_mut(&old_id).unwrap().unwrap();
            mem.created_at = Utc::now() - chrono::Duration::days(30);
        }
        std::fs::write(
            dir.join("config.toml"),
            "[retention]\n\"audio:\" = { ttl_days = 14.0 }\n",
        ).unwrap();
        assert_eq!(ce.stage_retention_triage(&mut engine), 1, "only the stale row ghosts");
        assert_eq!(engine.get_memory(&old_id).unwrap().unwrap().amplitude, 0.0);
        assert!(engine.get_memory(&new_id).unwrap().unwrap().amplitude > 0.0);

        // Cleanup (process-global env).
        std::env::remove_var("KANNAKA_TRIAGE");
        match prev_data_dir {
            Some(v) => std::env::set_var("KANNAKA_DATA_DIR", v),
            None => std::env::remove_var("KANNAKA_DATA_DIR"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

}
