//! ADR-0012: Holographic Paradox Engine
//!
//! Information-theoretic resolution of memory consolidation conflicts through
//! holographic projection and thermodynamic efficiency tracking.
//!
//! When multiple dream threads mutate the same memory differently, traditional
//! lock-based approaches pick one and destroy information. This engine treats
//! contradictions as fuel for an information-theoretic heat engine that preserves
//! information through holographic projection onto lower-dimensional surfaces.
//!
//! The three resolution strategies:
//! 1. CONSENSUS: All threads agree → direct apply (η ≈ 1.0)
//! 2. PROJECTION: Threads disagree but vectors compatible → wave superposition (0.5 < η < 1.0)
//! 3. IRREDUCIBLE: Fundamental disagreement → preserve all states as tension links (η < 0.5)

use std::collections::HashMap;
use std::sync::Arc;
use std::f32::consts::PI;

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::consolidation::ConsolidationReport;
use crate::memory::HyperMemory;
use crate::store::ResonanceEngine;
use crate::wave::normalize;

#[cfg(feature = "collective")]
use rayon::prelude::*;

// ---------------------------------------------------------------------------
// Core Types
// ---------------------------------------------------------------------------

/// Immutable snapshot of all memories at a point in time.
/// Arc-wrapped for zero-copy sharing across threads.
#[derive(Debug, Clone)]
pub struct ParadoxSnapshot {
    /// Frozen state of all memories
    pub memories: Arc<HashMap<Uuid, HyperMemory>>,
    /// When this snapshot was taken
    pub timestamp: DateTime<Utc>,
}

/// Delta-based representation of changes made to a memory.
/// Stores only what changed, not absolute values, making paradox detection trivial.
#[derive(Debug, Clone)]
pub struct Mutation {
    pub memory_id: Uuid,
    pub amplitude_delta: f32,  // new - old
    pub phase_delta: f32,      // new - old (wrapped to [-π, π])
    /// Sparse vector delta (only changed dimensions)
    pub vector_delta: Vec<(usize, f32)>,  // (dimension_index, new_value - old_value)
}

impl Mutation {
    /// Create a mutation by diffing new memory against snapshot.
    pub fn from_diff(memory_id: Uuid, new_mem: &HyperMemory, snapshot: &ParadoxSnapshot) -> Option<Self> {
        let old_mem = snapshot.memories.get(&memory_id)?;
        
        let amplitude_delta = new_mem.amplitude - old_mem.amplitude;
        let raw_phase_delta = new_mem.phase - old_mem.phase;
        // Wrap phase delta to [-π, π]
        let phase_delta = if raw_phase_delta > PI {
            raw_phase_delta - 2.0 * PI
        } else if raw_phase_delta < -PI {
            raw_phase_delta + 2.0 * PI
        } else {
            raw_phase_delta
        };
        
        // Only store vector deltas for dimensions that changed significantly
        let vector_delta: Vec<(usize, f32)> = new_mem.vector
            .iter()
            .zip(old_mem.vector.iter())
            .enumerate()
            .filter_map(|(i, (&new_val, &old_val))| {
                let delta = new_val - old_val;
                if delta.abs() > 1e-6 {  // significance threshold
                    Some((i, delta))
                } else {
                    None
                }
            })
            .collect();
        
        // Only create mutation if something actually changed
        if amplitude_delta.abs() < 1e-6 && phase_delta.abs() < 1e-6 && vector_delta.is_empty() {
            None
        } else {
            Some(Mutation {
                memory_id,
                amplitude_delta,
                phase_delta,
                vector_delta,
            })
        }
    }
    
    /// Apply this mutation to a memory.
    pub fn apply_to(&self, memory: &mut HyperMemory) {
        memory.amplitude += self.amplitude_delta;
        memory.phase += self.phase_delta;
        
        // Wrap phase to [0, 2π]
        while memory.phase < 0.0 {
            memory.phase += 2.0 * PI;
        }
        while memory.phase >= 2.0 * PI {
            memory.phase -= 2.0 * PI;
        }
        
        // Apply vector deltas
        for &(dim_idx, delta) in &self.vector_delta {
            if dim_idx < memory.vector.len() {
                memory.vector[dim_idx] += delta;
            }
        }
    }
}

/// Record of changes made by a single dream thread to its assigned cluster.
#[derive(Debug, Clone)]
pub struct DreamTrajectory {
    pub cluster_id: u32,
    pub mutations: Vec<Mutation>,
    pub report: ConsolidationReport,
}

/// Proposed state for a memory from a specific dream thread.
#[derive(Debug, Clone)]
pub struct ProposedState {
    pub source_cluster: u32,
    pub amplitude: f32,
    pub phase: f32,
    /// Vector delta from snapshot (sparse representation)
    pub vector_delta: Vec<(usize, f32)>,
}

impl ProposedState {
    pub fn from_mutation(mutation: &Mutation, snapshot_memory: &HyperMemory, cluster_id: u32) -> Self {
        Self {
            source_cluster: cluster_id,
            amplitude: snapshot_memory.amplitude + mutation.amplitude_delta,
            phase: snapshot_memory.phase + mutation.phase_delta,
            vector_delta: mutation.vector_delta.clone(),
        }
    }
}

/// A detected contradiction: multiple threads proposing different states for the same memory.
#[derive(Debug, Clone)]
pub struct Paradox {
    pub memory_id: Uuid,
    pub states: Vec<ProposedState>,
    /// Information content of the disagreement (Shannon entropy)
    pub information_tension: f32,
}

/// Resolution strategy for a paradox.
#[derive(Debug, Clone)]
pub enum Resolution {
    /// All threads agree within tolerance → direct application
    Consensus(ProposedState),
    /// Holographic projection → wave superposition of all states
    Projection {
        amplitude: f32,
        phase: f32,
        vector: Vec<f32>,
        information_preserved: f32,  // 1.0 - (H_input - H_output) / H_input
    },
    /// Irreconcilable differences → preserve all states as tension links
    Irreducible {
        states: Vec<ProposedState>,
        tension_links: Vec<(usize, usize, f32)>,  // (state_idx_a, state_idx_b, tension_weight)
    },
}

/// Report from a paradox resolution cycle.
#[derive(Debug, Clone, Default)]
pub struct ResolutionReport {
    pub paradoxes_found: usize,
    pub consensus_count: usize,
    pub projected_count: usize,
    pub irreducible_count: usize,
    /// Carnot efficiency: η = 1 - S_resolved/S_paradox
    pub efficiency: f32,
    pub entropy_input: f32,   // Total information entropy of all paradoxes
    pub entropy_output: f32,  // Residual entropy after resolution
}

// ---------------------------------------------------------------------------
// Paradox Resolver
// ---------------------------------------------------------------------------

/// Holographic paradox resolution engine.
pub struct ParadoxResolver {
    trajectories: Vec<DreamTrajectory>,
    /// Consensus tolerance for amplitude differences
    pub consensus_amplitude_tolerance: f32,
    /// Consensus tolerance for phase differences (radians)
    pub consensus_phase_tolerance: f32,
    /// Cosine similarity threshold for projection viability
    pub projection_similarity_threshold: f32,
}

impl ParadoxResolver {
    pub fn new() -> Self {
        Self {
            trajectories: Vec::new(),
            consensus_amplitude_tolerance: 0.05,      // 5% amplitude difference
            consensus_phase_tolerance: PI / 8.0,      // 22.5 degrees
            projection_similarity_threshold: 0.6,     // vectors must be somewhat aligned
        }
    }
    
    /// Ingest a dream trajectory from a single cluster.
    pub fn ingest(&mut self, trajectory: &DreamTrajectory) {
        self.trajectories.push(trajectory.clone());
    }
    
    /// Detect all paradoxes: memories mutated by multiple trajectories.
    ///
    /// Requires the snapshot to reconstruct absolute states from deltas.
    /// Without absolute amplitudes, wave superposition and consensus detection
    /// would operate on deltas (BUG 1 from QE: "stores deltas as absolute values").
    pub fn detect_paradoxes(&self, snapshot: &ParadoxSnapshot) -> Vec<Paradox> {
        let mut memory_mutations: HashMap<Uuid, Vec<(u32, &Mutation)>> = HashMap::new();
        
        // Collect all mutations by memory ID
        for trajectory in &self.trajectories {
            for mutation in &trajectory.mutations {
                memory_mutations
                    .entry(mutation.memory_id)
                    .or_default()
                    .push((trajectory.cluster_id, mutation));
            }
        }
        
        let mut paradoxes = Vec::new();
        
        // Find memories with multiple mutations (paradoxes)
        for (memory_id, mutations) in memory_mutations {
            if mutations.len() > 1 {
                // Reconstruct ABSOLUTE states from snapshot + deltas
                let snapshot_mem = match snapshot.memories.get(&memory_id) {
                    Some(m) => m,
                    None => continue, // memory disappeared from snapshot (shouldn't happen)
                };
                
                let states: Vec<ProposedState> = mutations
                    .iter()
                    .map(|(cluster_id, mutation)| {
                        ProposedState::from_mutation(mutation, snapshot_mem, *cluster_id)
                    })
                    .collect();
                
                let information_tension = self.compute_information_tension(&states);
                
                paradoxes.push(Paradox {
                    memory_id,
                    states,
                    information_tension,
                });
            }
        }
        
        paradoxes
    }
    
    // paradox_memory_ids moved inline to dream_parallel in consolidation.rs
    
    /// Compute information entropy (tension) of a set of proposed states.
    /// Uses Shannon entropy of amplitude distribution as a proxy for information content.
    fn compute_information_tension(&self, states: &[ProposedState]) -> f32 {
        if states.len() <= 1 {
            return 0.0;
        }
        
        // Normalize amplitudes to probabilities
        let total_amplitude: f32 = states.iter().map(|s| s.amplitude.abs()).sum();
        if total_amplitude <= 1e-8 {
            return 0.0;
        }
        
        let mut entropy = 0.0f32;
        for state in states {
            let p = (state.amplitude.abs() / total_amplitude).max(1e-8); // avoid log(0)
            entropy -= p * p.log2();
        }
        
        entropy
    }
    
    /// Resolve paradoxes using holographic projection strategies.
    pub fn project(&self, paradoxes: Vec<Paradox>) -> Vec<(Paradox, Resolution)> {
        paradoxes
            .into_iter()
            .map(|paradox| {
                let resolution = self.resolve_single_paradox(&paradox);
                (paradox, resolution)
            })
            .collect()
    }
    
    /// Resolve a single paradox using the three-strategy hierarchy.
    fn resolve_single_paradox(&self, paradox: &Paradox) -> Resolution {
        // Strategy 1: CONSENSUS - check if all states are similar enough
        if self.states_in_consensus(&paradox.states) {
            // Average the consensus states
            let consensus_state = self.average_states(&paradox.states);
            return Resolution::Consensus(consensus_state);
        }
        
        // Strategy 2: PROJECTION - check if vectors are compatible for superposition
        if self.vectors_compatible_for_projection(&paradox.states) {
            return self.project_states(&paradox.states);
        }
        
        // Strategy 3: IRREDUCIBLE - preserve all states as tension links
        let tension_links = self.create_tension_links(&paradox.states);
        Resolution::Irreducible {
            states: paradox.states.clone(),
            tension_links,
        }
    }
    
    /// Check if all states are within consensus tolerance.
    fn states_in_consensus(&self, states: &[ProposedState]) -> bool {
        if states.len() <= 1 {
            return true;
        }
        
        let first = &states[0];
        for state in &states[1..] {
            // Check amplitude tolerance
            if (state.amplitude - first.amplitude).abs() > self.consensus_amplitude_tolerance {
                return false;
            }
            
            // Check phase tolerance (circular distance)
            let phase_diff = (state.phase - first.phase).abs();
            let circular_diff = if phase_diff > PI { 2.0 * PI - phase_diff } else { phase_diff };
            if circular_diff > self.consensus_phase_tolerance {
                return false;
            }
        }
        
        true
    }
    
    /// Average states for consensus resolution.
    fn average_states(&self, states: &[ProposedState]) -> ProposedState {
        if states.is_empty() {
            return ProposedState {
                source_cluster: 0,
                amplitude: 0.0,
                phase: 0.0,
                vector_delta: Vec::new(),
            };
        }
        
        let n = states.len() as f32;
        let avg_amplitude = states.iter().map(|s| s.amplitude).sum::<f32>() / n;
        
        // Circular mean for phase
        let sin_sum: f32 = states.iter().map(|s| s.phase.sin()).sum();
        let cos_sum: f32 = states.iter().map(|s| s.phase.cos()).sum();
        let avg_phase = (sin_sum / n).atan2(cos_sum / n);
        
        // Merge vector deltas (this is approximate - for true consensus should be nearly identical)
        let mut merged_delta: HashMap<usize, f32> = HashMap::new();
        for state in states {
            for &(dim, delta) in &state.vector_delta {
                *merged_delta.entry(dim).or_insert(0.0) += delta / n;
            }
        }
        let vector_delta: Vec<(usize, f32)> = merged_delta.into_iter().collect();
        
        ProposedState {
            source_cluster: states[0].source_cluster, // arbitrary choice
            amplitude: avg_amplitude,
            phase: avg_phase,
            vector_delta,
        }
    }
    
    /// Check if vector deltas are compatible for holographic projection.
    ///
    /// Computes pairwise cosine similarity between the delta direction vectors.
    /// If ANY pair has similarity below `projection_similarity_threshold`, the
    /// states are incompatible → Irreducible (event horizon).
    fn vectors_compatible_for_projection(&self, states: &[ProposedState]) -> bool {
        if states.len() <= 1 {
            return true;
        }
        
        // Collect states with non-empty vector deltas
        let non_empty: Vec<&ProposedState> = states.iter()
            .filter(|s| !s.vector_delta.is_empty())
            .collect();
        
        // If 0 or 1 states have vector changes, they're compatible (no vector conflict)
        if non_empty.len() <= 1 {
            return true;
        }
        
        // Build dense vectors from sparse deltas for cosine similarity
        let max_dim = non_empty.iter()
            .flat_map(|s| s.vector_delta.iter().map(|(d, _)| *d))
            .max()
            .unwrap_or(0) + 1;
        
        let dense_vecs: Vec<Vec<f32>> = non_empty.iter().map(|s| {
            let mut v = vec![0.0f32; max_dim];
            for &(dim, delta) in &s.vector_delta {
                if dim < max_dim { v[dim] = delta; }
            }
            v
        }).collect();
        
        // Pairwise cosine similarity check
        for i in 0..dense_vecs.len() {
            for j in (i + 1)..dense_vecs.len() {
                let sim = crate::wave::cosine_similarity(&dense_vecs[i], &dense_vecs[j]);
                if sim < self.projection_similarity_threshold {
                    return false; // Vectors are too dissimilar → Irreducible
                }
            }
        }
        
        true
    }
    
    /// Perform holographic projection: wave superposition of all states.
    fn project_states(&self, states: &[ProposedState]) -> Resolution {
        if states.is_empty() {
            return Resolution::Projection {
                amplitude: 0.0,
                phase: 0.0,
                vector: Vec::new(),
                information_preserved: 0.0,
            };
        }
        
        // Wave amplitude superposition: A = √(Σ aᵢ² + 2·Σᵢ<ⱼ aᵢ·aⱼ·cos(Δφᵢⱼ))
        let mut amplitude_squared = 0.0f32;
        
        // Σ aᵢ² term
        for state in states {
            amplitude_squared += state.amplitude * state.amplitude;
        }
        
        // 2·Σᵢ<ⱼ aᵢ·aⱼ·cos(Δφᵢⱼ) term
        for i in 0..states.len() {
            for j in (i + 1)..states.len() {
                let a_i = states[i].amplitude;
                let a_j = states[j].amplitude;
                let phase_diff = (states[i].phase - states[j].phase).abs();
                let circular_diff = if phase_diff > PI { 2.0 * PI - phase_diff } else { phase_diff };
                amplitude_squared += 2.0 * a_i * a_j * circular_diff.cos();
            }
        }
        
        let projected_amplitude = amplitude_squared.max(0.0).sqrt();
        
        // Phase: circular mean weighted by amplitude
        let total_amplitude: f32 = states.iter().map(|s| s.amplitude.abs()).sum();
        let phase = if total_amplitude > 1e-8 {
            let sin_sum: f32 = states.iter().map(|s| s.amplitude * s.phase.sin()).sum();
            let cos_sum: f32 = states.iter().map(|s| s.amplitude * s.phase.cos()).sum();
            (sin_sum / total_amplitude).atan2(cos_sum / total_amplitude)
        } else {
            0.0
        };
        
        // Vector: merge all deltas (simplified - real implementation would be more sophisticated)
        let mut merged_vector_delta: HashMap<usize, f32> = HashMap::new();
        for state in states {
            let weight = if total_amplitude > 1e-8 { state.amplitude / total_amplitude } else { 1.0 / states.len() as f32 };
            for &(dim, delta) in &state.vector_delta {
                *merged_vector_delta.entry(dim).or_insert(0.0) += weight * delta;
            }
        }
        
        // Vector reconstruction happens in apply_projection using snapshot base + weighted deltas.
        // We don't store a full vector here — it would require the snapshot context.
        let vector = Vec::new();
        
        // Information preservation: how much of the input signal survives projection.
        // Perfect alignment (all states agree) → projected_amplitude = sum of amplitudes → η ≈ 1
        // Perfect opposition → projected_amplitude ≈ 0 → η ≈ 0
        // The ratio of projected amplitude to sum-of-amplitudes measures preservation.
        let max_possible_amplitude = total_amplitude; // if all perfectly aligned
        let information_preserved = if max_possible_amplitude > 1e-8 {
            (projected_amplitude / max_possible_amplitude).min(1.0)
        } else {
            1.0
        };
        
        Resolution::Projection {
            amplitude: projected_amplitude,
            phase,
            vector,
            information_preserved,
        }
    }
    
    /// Create tension links between incompatible states.
    fn create_tension_links(&self, states: &[ProposedState]) -> Vec<(usize, usize, f32)> {
        let mut links = Vec::new();
        
        // Create links between all pairs, weighted by their disagreement
        for i in 0..states.len() {
            for j in (i + 1)..states.len() {
                let amplitude_diff = (states[i].amplitude - states[j].amplitude).abs();
                let phase_diff = (states[i].phase - states[j].phase).abs();
                let circular_phase_diff = if phase_diff > PI { 2.0 * PI - phase_diff } else { phase_diff };
                
                // Tension weight based on degree of disagreement
                let tension = amplitude_diff + circular_phase_diff / PI;
                links.push((i, j, tension));
            }
        }
        
        links
    }
    
    /// Apply resolutions to the memory engine and return efficiency report.
    pub fn apply(
        self, 
        engine: &mut ResonanceEngine, 
        resolutions: Vec<(Paradox, Resolution)>,
        snapshot: &ParadoxSnapshot,
    ) -> ResolutionReport {
        let mut report = ResolutionReport {
            paradoxes_found: resolutions.len(),
            ..Default::default()
        };
        
        let mut entropy_input = 0.0f32;
        let mut entropy_output = 0.0f32;
        
        for (paradox, resolution) in resolutions {
            entropy_input += paradox.information_tension;
            
            match resolution {
                Resolution::Consensus(state) => {
                    self.apply_consensus_state(engine, &paradox, &state, snapshot);
                    report.consensus_count += 1;
                    entropy_output += 0.0; // Perfect consensus preserves all information
                }
                Resolution::Projection { amplitude, phase, information_preserved, .. } => {
                    self.apply_projection(engine, &paradox, amplitude, phase, snapshot);
                    report.projected_count += 1;
                    entropy_output += paradox.information_tension * (1.0 - information_preserved);
                }
                Resolution::Irreducible { states, tension_links } => {
                    self.apply_irreducible(engine, &paradox, &states, &tension_links, snapshot);
                    report.irreducible_count += 1;
                    entropy_output += paradox.information_tension; // All entropy preserved
                }
            }
        }
        
        // Carnot efficiency
        report.efficiency = if entropy_input > 1e-8 {
            1.0 - entropy_output / entropy_input
        } else {
            1.0
        };
        
        report.entropy_input = entropy_input;
        report.entropy_output = entropy_output;
        
        report
    }
    
    /// Apply a consensus state to the memory.
    fn apply_consensus_state(
        &self,
        engine: &mut ResonanceEngine,
        paradox: &Paradox,
        state: &ProposedState,
        snapshot: &ParadoxSnapshot,
    ) {
        if let Ok(Some(memory)) = engine.store.get_mut(&paradox.memory_id) {
            // Apply the consensus state
            memory.amplitude = state.amplitude;
            memory.phase = state.phase;
            
            // Apply vector deltas to snapshot base
            if let Some(snapshot_memory) = snapshot.memories.get(&paradox.memory_id) {
                memory.vector = snapshot_memory.vector.clone();
                for &(dim, delta) in &state.vector_delta {
                    if dim < memory.vector.len() {
                        memory.vector[dim] += delta;
                    }
                }
                // Re-normalize vector
                normalize(&mut memory.vector);
            }
            
            memory.touch();
        }
    }
    
    /// Apply a holographic projection to the memory.
    fn apply_projection(
        &self,
        engine: &mut ResonanceEngine,
        paradox: &Paradox,
        amplitude: f32,
        phase: f32,
        snapshot: &ParadoxSnapshot,
    ) {
        if let Ok(Some(memory)) = engine.store.get_mut(&paradox.memory_id) {
            memory.amplitude = amplitude;
            memory.phase = phase;
            
            // For vector, merge all deltas from the projection
            if let Some(snapshot_memory) = snapshot.memories.get(&paradox.memory_id) {
                memory.vector = snapshot_memory.vector.clone();
                
                // Apply weighted average of all vector deltas
                let total_amplitude: f32 = paradox.states.iter().map(|s| s.amplitude.abs()).sum();
                
                if total_amplitude > 1e-8 {
                    for state in &paradox.states {
                        let weight = state.amplitude.abs() / total_amplitude;
                        for &(dim, delta) in &state.vector_delta {
                            if dim < memory.vector.len() {
                                memory.vector[dim] += weight * delta;
                            }
                        }
                    }
                }
                
                // Re-normalize vector (critical for cosine similarity)
                normalize(&mut memory.vector);
            }
            
            memory.touch();
        }
    }
    
    /// Apply irreducible resolution: preserve original snapshot state and create tension metadata.
    fn apply_irreducible(
        &self,
        engine: &mut ResonanceEngine,
        paradox: &Paradox,
        _states: &[ProposedState],
        _tension_links: &[(usize, usize, f32)],
        snapshot: &ParadoxSnapshot,
    ) {
        if let Ok(Some(memory)) = engine.store.get_mut(&paradox.memory_id) {
            // Restore original snapshot state (no mutation applied)
            if let Some(snapshot_memory) = snapshot.memories.get(&paradox.memory_id) {
                memory.amplitude = snapshot_memory.amplitude;
                memory.phase = snapshot_memory.phase;
                memory.vector = snapshot_memory.vector.clone();
            }
            
            // Mark as disputed to indicate unresolved paradox
            memory.disputed = true;
            
            // In a full implementation, would store tension links as metadata
            // For now, just touch to mark as processed
            memory.touch();
        }
    }
}

impl Default for ParadoxResolver {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::HyperMemory;
    
    fn make_test_memory(id: Uuid, amplitude: f32, phase: f32) -> HyperMemory {
        let mut mem = HyperMemory::new(vec![1.0; 10], "test".to_string());
        mem.id = id;
        mem.amplitude = amplitude;
        mem.phase = phase;
        mem
    }
    
    fn make_snapshot(memories: Vec<HyperMemory>) -> ParadoxSnapshot {
        let memory_map: HashMap<Uuid, HyperMemory> = memories
            .into_iter()
            .map(|m| (m.id, m))
            .collect();
        
        ParadoxSnapshot {
            memories: Arc::new(memory_map),
            timestamp: Utc::now(),
        }
    }
    
    #[test]
    fn mutation_from_diff_detects_amplitude_change() {
        let id = Uuid::new_v4();
        let old_mem = make_test_memory(id, 0.8, 0.0);
        let mut new_mem = old_mem.clone();
        new_mem.amplitude = 1.0;
        
        let snapshot = make_snapshot(vec![old_mem]);
        let mutation = Mutation::from_diff(id, &new_mem, &snapshot).unwrap();
        
        assert_eq!(mutation.memory_id, id);
        assert!((mutation.amplitude_delta - 0.2).abs() < 1e-6);
        assert!(mutation.phase_delta.abs() < 1e-6);
    }
    
    #[test]
    fn mutation_from_diff_wraps_phase() {
        let id = Uuid::new_v4();
        let old_mem = make_test_memory(id, 0.8, 0.1);
        let mut new_mem = old_mem.clone();
        new_mem.phase = 2.0 * PI - 0.1; // Should wrap to small negative delta
        
        let snapshot = make_snapshot(vec![old_mem]);
        let mutation = Mutation::from_diff(id, &new_mem, &snapshot).unwrap();
        
        assert!(mutation.phase_delta < 0.0);
        assert!(mutation.phase_delta > -0.3); // Should be small after wrapping
    }
    
    #[test]
    fn paradox_detection_finds_conflicting_mutations() {
        let mut resolver = ParadoxResolver::new();
        
        let memory_id = Uuid::new_v4();
        let snapshot_mem = make_test_memory(memory_id, 0.8, 0.5);
        let snapshot = make_snapshot(vec![snapshot_mem]);
        
        // Two trajectories mutate the same memory differently
        let traj1 = DreamTrajectory {
            cluster_id: 1,
            mutations: vec![Mutation {
                memory_id,
                amplitude_delta: 0.1,
                phase_delta: 0.0,
                vector_delta: Vec::new(),
            }],
            report: ConsolidationReport::default(),
        };
        
        let traj2 = DreamTrajectory {
            cluster_id: 2,
            mutations: vec![Mutation {
                memory_id,
                amplitude_delta: -0.1,
                phase_delta: PI,
                vector_delta: Vec::new(),
            }],
            report: ConsolidationReport::default(),
        };
        
        resolver.ingest(&traj1);
        resolver.ingest(&traj2);
        
        let paradoxes = resolver.detect_paradoxes(&snapshot);
        assert_eq!(paradoxes.len(), 1);
        assert_eq!(paradoxes[0].memory_id, memory_id);
        assert_eq!(paradoxes[0].states.len(), 2);
        // States should have ABSOLUTE values (snapshot + delta), not raw deltas
        assert!((paradoxes[0].states[0].amplitude - 0.9).abs() < 1e-5,
            "expected 0.8+0.1=0.9, got {}", paradoxes[0].states[0].amplitude);
        assert!((paradoxes[0].states[1].amplitude - 0.7).abs() < 1e-5,
            "expected 0.8-0.1=0.7, got {}", paradoxes[0].states[1].amplitude);
        assert!(paradoxes[0].information_tension > 0.0);
    }
    
    #[test]
    fn consensus_detection_works() {
        let states = vec![
            ProposedState { source_cluster: 1, amplitude: 1.0, phase: 0.0, vector_delta: Vec::new() },
            ProposedState { source_cluster: 2, amplitude: 1.02, phase: 0.01, vector_delta: Vec::new() },
        ];
        
        let resolver = ParadoxResolver::new();
        assert!(resolver.states_in_consensus(&states));
    }
    
    #[test]
    fn consensus_rejection_works() {
        let states = vec![
            ProposedState { source_cluster: 1, amplitude: 1.0, phase: 0.0, vector_delta: Vec::new() },
            ProposedState { source_cluster: 2, amplitude: 0.5, phase: PI, vector_delta: Vec::new() },
        ];
        
        let resolver = ParadoxResolver::new();
        assert!(!resolver.states_in_consensus(&states));
    }
    
    #[test]
    fn irreducible_reached_with_dissimilar_vectors() {
        let resolver = ParadoxResolver::new();
        // Two states with opposing vector deltas → cosine similarity < threshold
        let states = vec![
            ProposedState {
                source_cluster: 1,
                amplitude: 0.8,
                phase: 0.0,
                vector_delta: vec![(0, 1.0), (1, 0.0), (2, 0.0)],
            },
            ProposedState {
                source_cluster: 2,
                amplitude: 0.7,
                phase: 0.5,
                vector_delta: vec![(0, -1.0), (1, 0.0), (2, 0.0)],
            },
        ];
        // cosine_similarity of [1,0,0] vs [-1,0,0] = -1.0 < 0.6 threshold
        assert!(!resolver.vectors_compatible_for_projection(&states),
            "opposing vectors should be incompatible for projection");
        
        // Full resolution should produce Irreducible
        let paradox = Paradox {
            memory_id: Uuid::new_v4(),
            states: states.clone(),
            information_tension: 0.5,
        };
        match resolver.resolve_single_paradox(&paradox) {
            Resolution::Irreducible { tension_links, .. } => {
                assert!(!tension_links.is_empty(), "should have tension links");
            }
            other => panic!("expected Irreducible, got {:?}", std::mem::discriminant(&other)),
        }
    }

    #[test]
    fn projection_information_preserved_varies_with_alignment() {
        let resolver = ParadoxResolver::new();
        
        // Perfectly aligned → high preservation
        let aligned = vec![
            ProposedState { source_cluster: 1, amplitude: 0.8, phase: 0.0, vector_delta: Vec::new() },
            ProposedState { source_cluster: 2, amplitude: 0.6, phase: 0.0, vector_delta: Vec::new() },
        ];
        if let Resolution::Projection { information_preserved: ip_aligned, .. } = resolver.project_states(&aligned) {
            // Opposed → lower preservation
            let opposed = vec![
                ProposedState { source_cluster: 1, amplitude: 0.8, phase: 0.0, vector_delta: Vec::new() },
                ProposedState { source_cluster: 2, amplitude: 0.6, phase: PI, vector_delta: Vec::new() },
            ];
            if let Resolution::Projection { information_preserved: ip_opposed, .. } = resolver.project_states(&opposed) {
                assert!(ip_aligned > ip_opposed,
                    "aligned ({ip_aligned}) should preserve more info than opposed ({ip_opposed})");
            }
        }
    }

    #[test]
    fn wave_superposition_formula_is_correct() {
        // Test the core holographic projection formula
        let states = vec![
            ProposedState { source_cluster: 1, amplitude: 0.8, phase: 0.0, vector_delta: Vec::new() },
            ProposedState { source_cluster: 2, amplitude: 0.6, phase: 0.0, vector_delta: Vec::new() },
        ];
        
        let resolver = ParadoxResolver::new();
        if let Resolution::Projection { amplitude, .. } = resolver.project_states(&states) {
            // For aligned phases (Δφ = 0), should get A = √(0.8² + 0.6² + 2*0.8*0.6*cos(0))
            // = √(0.64 + 0.36 + 0.96) = √1.96 = 1.4
            assert!((amplitude - 1.4).abs() < 0.01, "expected ~1.4, got {amplitude}");
        } else {
            panic!("expected projection resolution");
        }
    }
    
    #[test]
    fn efficiency_metric_works() {
        let mut resolver = ParadoxResolver::new();
        
        // Create a simple consensus case (high efficiency)
        let memory_id = Uuid::new_v4();
        // (no snapshot: `resolve_single_paradox` takes only the paradox)
        
        let paradox = Paradox {
            memory_id,
            states: vec![
                ProposedState { source_cluster: 1, amplitude: 1.1, phase: 0.0, vector_delta: Vec::new() },
                ProposedState { source_cluster: 2, amplitude: 1.09, phase: 0.01, vector_delta: Vec::new() },
            ],
            information_tension: 0.5,
        };
        
        let resolution = resolver.resolve_single_paradox(&paradox);
        
        // Consensus should have high efficiency (low entropy loss)
        match resolution {
            Resolution::Consensus(_) => {
                // Good - consensus preserves information
            }
            _ => panic!("expected consensus for similar states"),
        }
    }
    
    #[test]
    fn dream_parallel_integration_test() {
        use crate::codebook::Codebook;
        use crate::encoding::{EncodingPipeline, SimpleHashEncoder};
        use crate::store::{TestMedium, ResonanceEngine};
        use crate::consolidation::ConsolidationEngine;
        
        // Set up engine with multiple memories across different frequency categories
        let encoder = SimpleHashEncoder::new(384, 42);
        let codebook = Codebook::new(384, 10_000, 42);
        let pipeline = EncodingPipeline::new(Box::new(encoder), codebook);
        let mut engine = ResonanceEngine::new(Box::new(TestMedium::new()), pipeline);
        
        // Add memories to create multiple Xi clusters
        let id1 = engine.remember("high frequency experience memory").unwrap();
        let id2 = engine.remember("medium frequency emotional memory").unwrap();
        let id3 = engine.remember("low frequency knowledge fact").unwrap();
        
        // Set frequencies to create distinct clusters
        if let Ok(Some(mem)) = engine.store.get_mut(&id1) {
            mem.frequency = 2.0; // experience category
            mem.amplitude = 0.8;
        }
        if let Ok(Some(mem)) = engine.store.get_mut(&id2) {
            mem.frequency = 1.5; // emotion category 
            mem.amplitude = 0.7;
        }
        if let Ok(Some(mem)) = engine.store.get_mut(&id3) {
            mem.frequency = 0.5; // knowledge category
            mem.amplitude = 0.6;
        }
        
        let initial_count = engine.store.count();
        let consolidation = ConsolidationEngine::default();
        
        // Run parallel dream
        let (consolidation_reports, resolution_report) = consolidation.dream_parallel(&mut engine);
        
        // Verify results
        assert!(!consolidation_reports.is_empty(), "should have consolidation reports");
        assert_eq!(resolution_report.paradoxes_found, 0, "no paradoxes expected with distinct memories");
        assert_eq!(resolution_report.efficiency, 1.0, "perfect efficiency with no paradoxes");
        
        // Engine should still have the same number of memories (no hallucinations created yet with simple test setup)
        assert!(engine.store.count() >= initial_count, "memory count should not decrease");
        
        // Memories should still exist and be accessible
        assert!(engine.get_memory(&id1).unwrap().is_some());
        assert!(engine.get_memory(&id2).unwrap().is_some());
        assert!(engine.get_memory(&id3).unwrap().is_some());
        
        println!("Parallel dream completed successfully!");
        println!("Consolidation reports: {}", consolidation_reports.len());
        println!("Resolution report: {resolution_report:#?}");
    }
}