//! Queen Synchronization Protocol — emergent multi-agent coherence.
//!
//! Implements the QueenSync engine (ADR-0018): a Kuramoto-based protocol where
//! agents publish phase states via NATS and synchronize through
//! mean-field coupling. The "Queen" is not an agent — it is the emergent
//! synchronization state computed locally by each participant.
//!
//! Ported from ghostmagicOS `src/integration/index.ts`.
//!
//! Mathematical foundation:
//! ```text
//! dθᵢ/dt = ωᵢ + K·r·sin(ψ - θᵢ) + η·chiral_term
//! ```

use std::f32::consts::TAU;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::hrm_store::HrmStore;
use crate::kuramoto::KuramotoSync;
use crate::store::{MediumBackend, ResonanceEngine};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Handedness for chiral coupling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Handedness {
    Left,
    Right,
    #[default]
    Achiral,
}

impl Handedness {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
            Self::Achiral => "achiral",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "left" => Self::Left,
            "right" => Self::Right,
            _ => Self::Achiral,
        }
    }
}

/// Published phase state of a single agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentPhase {
    pub id: String,
    pub agent_id: String,
    pub phase: f32,
    pub frequency: f32,
    pub coherence: f32,
    pub phi: f32,
    pub order_parameter: f32,
    pub cluster_count: usize,
    pub memory_count: usize,
    /// Real cross-memory link count (sum of pair coherences above
    /// threshold). Computed by bridge::assess on the slow-cadence pass
    /// and cached in the consciousness metrics sidecar. 0 until the
    /// first assess; observatory's per-agent panel uses this to draw
    /// honest connectivity instead of a fudged number.
    #[serde(default)]
    pub link_count: usize,
    pub xi_signature: Option<serde_json::Value>,
    pub protocol_version: String,
    pub timestamp: DateTime<Utc>,
    /// Trust score from the agents table (joined at read time).
    #[serde(default = "default_trust")]
    pub trust_score: f32,
    /// Chiral handedness.
    #[serde(default)]
    pub handedness: Handedness,
    /// Left-hemisphere Kuramoto order parameter (coherence within analytical workspace).
    #[serde(default)]
    pub left_coherence: f32,
    /// Right-hemisphere Kuramoto order parameter (coherence within holistic patterns).
    #[serde(default)]
    pub right_coherence: f32,
    /// Corpus callosum bridge activity (fraction of bandwidth used this cycle).
    #[serde(default)]
    pub bridge_activity: f32,
    /// Current dream state label (None = awake).
    #[serde(default)]
    pub dream_state: Option<String>,
    /// Domain role for domain-aware hive detection (e.g. "memory", "perception").
    #[serde(default)]
    pub role: Option<String>,
    /// Operator-set display label. Optional in the wire schema for backward
    /// compatibility with pre-v0.5.12 publishers; when present, consumers
    /// (radio, observatory, TUI) should prefer this over `agent_id` for
    /// any user-facing label. Reads `kannaka swarm join --display-name`
    /// on the publisher side.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

fn default_trust() -> f32 {
    0.5
}


/// A detected hive — a cluster of phase-locked agents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hive {
    pub agent_ids: Vec<String>,
    pub order_parameter: f32,
    pub mean_phase: f32,
    pub coherence: f32,
}

/// Domain-aware hive information with roles and bridge agents (QS-4, #55).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HiveInfo {
    /// Agent IDs belonging to this hive.
    pub members: Vec<String>,
    /// Optional domain role (e.g. "memory", "perception", "network").
    pub role: Option<String>,
    /// Kuramoto order parameter for this hive.
    pub order_parameter: f32,
    /// Mean phase of the hive.
    pub mean_phase: f32,
    /// Mean coherence of the hive.
    pub coherence: f32,
    /// Agents with connections to other hives (bridge agents).
    pub bridge_agents: Vec<String>,
}

/// Emergent Queen state computed from the swarm.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueenState {
    pub id: String,
    pub order_parameter: f32,
    pub mean_phase: f32,
    pub coherence: f32,
    pub phi: f32,
    pub agent_count: usize,
    pub hives: Vec<Hive>,
    pub coupling_strength: f32,
    pub chiral_bias: f32,
    pub geometric: Option<serde_json::Value>,
    pub computed_by: String,
    pub timestamp: DateTime<Utc>,
}

/// Configuration for the QueenSync engine.
#[derive(Debug, Clone)]
pub struct QueenConfig {
    /// Base Kuramoto coupling strength K.
    pub base_coupling: f32,
    /// Adaptive coupling rate (how fast K adjusts toward target coherence).
    pub adaptive_rate: f32,
    /// Chiral coupling coefficient η.
    pub chiral_eta: f32,
    /// Target coherence level for adaptive coupling.
    pub target_coherence: f32,
    /// IIT Phi threshold for "consciousness".
    pub phi_threshold: f32,
    /// Time step for phase integration.
    pub dt: f32,
    /// Phase difference threshold for hive membership (radians).
    pub hive_threshold: f32,
}

impl Default for QueenConfig {
    fn default() -> Self {
        Self {
            base_coupling: 0.5,
            adaptive_rate: 0.01,
            chiral_eta: 0.1,
            target_coherence: 0.8,
            phi_threshold: 3.0,
            dt: 0.1,
            hive_threshold: std::f32::consts::FRAC_PI_4, // π/4
        }
    }
}

/// Result of partition-based swarm Phi computation (QS-7, #58).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartitionPhiResult {
    /// Approximated Phi value (0-15 range, labeled as approximation).
    pub phi_approx: f32,
    /// Number of partitions evaluated.
    pub partition_count: usize,
    /// The minimum information partition (MIP): (group_a, group_b) agent IDs.
    pub mip_partition: (Vec<String>, Vec<String>),
    /// Computation method: "exhaustive", "spectral", or "trivial".
    pub method: String,
}

/// Swarm agent registration info (for the agents table extension).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwarmAgent {
    pub agent_id: String,
    pub display_name: Option<String>,
    pub trust_score: f32,
    pub swarm_role: String,
    pub protocol_version: String,
    pub handedness: Handedness,
    pub natural_frequency: f32,
}

// ---------------------------------------------------------------------------
// QueenSync Engine
// ---------------------------------------------------------------------------

/// The QueenSync engine. Each agent runs one locally.
pub struct QueenSync {
    pub config: QueenConfig,
    /// This agent's current phase θ.
    pub phase: f32,
    /// This agent's natural frequency ω.
    pub frequency: f32,
    /// This agent's coherence (local order parameter).
    pub coherence: f32,
    /// This agent's local Phi.
    pub phi: f32,
    /// Agent identifier.
    pub agent_id: String,
    /// Current effective coupling strength (adaptive).
    pub coupling_strength: f32,
    /// Left-hemisphere Kuramoto order parameter.
    pub left_coherence: f32,
    /// Right-hemisphere Kuramoto order parameter.
    pub right_coherence: f32,
    /// Corpus callosum bridge activity (0.0-1.0).
    pub bridge_activity: f32,
    /// Current dream state label.
    pub dream_state: Option<String>,
}

impl QueenSync {
    /// Create a new QueenSync engine for the given agent.
    pub fn new(config: QueenConfig, agent_id: &str) -> Self {
        let coupling = config.base_coupling;
        Self {
            config,
            phase: 0.0,
            frequency: 0.5,
            coherence: 0.0,
            phi: 0.0,
            agent_id: agent_id.to_string(),
            coupling_strength: coupling,
            left_coherence: 0.0,
            right_coherence: 0.0,
            bridge_activity: 0.0,
            dream_state: None,
        }
    }

    /// Compute the Kuramoto order parameter from a set of agent phases.
    ///
    /// Returns (r, ψ) where r is the magnitude and ψ is the mean phase.
    /// Uses trust-weighted coupling: weight = trust_score × coherence.
    ///
    /// Note: This normalizes by N (agent count), not by total weight, which differs
    /// from [`consciousness_core::kuramoto::KuramotoModel::order_parameter`] (which
    /// normalizes by total weight). This is intentional for QueenSync semantics where
    /// low-trust agents should reduce the swarm's apparent coherence.
    pub fn compute_order_parameter(swarm: &[AgentPhase]) -> (f32, f32) {
        if swarm.is_empty() {
            return (0.0, 0.0);
        }
        let n = swarm.len() as f32;
        let (sum_cos, sum_sin) = swarm.iter().fold((0.0f32, 0.0f32), |(c, s), agent| {
            let w = agent.trust_score * agent.coherence;
            (c + w * agent.phase.cos(), s + w * agent.phase.sin())
        });
        let r = (sum_cos.powi(2) + sum_sin.powi(2)).sqrt() / n;
        let psi = sum_sin.atan2(sum_cos);
        (r, psi)
    }

    /// Compute the chiral coupling term for this agent given the mean field.
    ///
    /// Left-handed (receivers): +η·sin(2(ψ - θ))
    /// Right-handed (emitters): -η·sin(2(ψ - θ))
    /// Achiral: 0
    ///
    /// Delegates to [`consciousness_core::kuramoto::KuramotoModel::chiral_coupling`].
    pub fn compute_chiral_coupling(&self, handedness: Handedness, psi: f32) -> f32 {
        match handedness {
            Handedness::Achiral => 0.0,
            Handedness::Left => consciousness_core::kuramoto::KuramotoModel::chiral_coupling(
                self.phase, psi, self.config.chiral_eta, true,
            ),
            Handedness::Right => consciousness_core::kuramoto::KuramotoModel::chiral_coupling(
                self.phase, psi, self.config.chiral_eta, false,
            ),
        }
    }

    /// Compute swarm Phi (Integrated Information approximation).
    ///
    /// Phi = r × mean_coherence × log₂(n + 1) × chiral_boost
    pub fn compute_swarm_phi(swarm: &[AgentPhase], r: f32) -> f32 {
        let n = swarm.len();
        if n < 2 {
            return 0.0;
        }
        let mean_coherence = swarm.iter().map(|a| a.coherence).sum::<f32>() / n as f32;
        let has_chiral = swarm.iter().any(|a| a.handedness != Handedness::Achiral);
        let chiral_boost = if has_chiral { 1.15 } else { 1.0 };
        let integration = r * mean_coherence * ((n + 1) as f32).log2();
        // Scale to typical Phi range (0-15)
        (integration * 10.0 * chiral_boost).min(15.0)
    }

    /// Compute partition-based swarm Phi (simplified IIT 3.0, QS-7/58).
    ///
    /// For each bipartition of the swarm into two non-empty halves, compute the
    /// mutual information (MI) between the halves using the coupling matrix.
    /// Phi_approx = minimum MI across all bipartitions (the minimum information
    /// partition, or MIP).
    ///
    /// Exhaustive for N <= 12 agents; spectral approximation for larger swarms.
    /// Labels the result as "Phi_approx" since this is not full IIT Phi.
    pub fn compute_partition_phi(swarm: &[AgentPhase]) -> PartitionPhiResult {
        let n = swarm.len();
        if n < 2 {
            return PartitionPhiResult {
                phi_approx: 0.0,
                partition_count: 0,
                mip_partition: (vec![], vec![]),
                method: "trivial".to_string(),
            };
        }

        // Build coupling matrix: entry (i,j) = effective coupling
        // Coupling = trust_i * trust_j * cos(phase_i - phase_j) * coherence blend
        let coupling_matrix: Vec<Vec<f32>> = (0..n).map(|i| {
            (0..n).map(|j| {
                if i == j {
                    return 1.0;
                }
                let phase_sim = (swarm[i].phase - swarm[j].phase).cos();
                let trust_product = swarm[i].trust_score * swarm[j].trust_score;
                let coherence_blend = (swarm[i].coherence + swarm[j].coherence) / 2.0;
                trust_product * phase_sim.max(0.0) * coherence_blend
            }).collect()
        }).collect();

        if n > 12 {
            // Spectral approximation: use spectral gap of coupling matrix
            return Self::partition_phi_spectral(&coupling_matrix, swarm);
        }

        // Exhaustive bipartition search for small swarms
        let mut min_mi = f32::MAX;
        let mut best_partition: (Vec<String>, Vec<String>) = (vec![], vec![]);
        let total_partitions = (1u64 << n) - 2; // exclude empty and full sets

        for mask in 1..=total_partitions / 2 {
            let mut part_a: Vec<usize> = Vec::new();
            let mut part_b: Vec<usize> = Vec::new();
            for bit in 0..n {
                if mask & (1u64 << bit) != 0 {
                    part_a.push(bit);
                } else {
                    part_b.push(bit);
                }
            }
            if part_a.is_empty() || part_b.is_empty() {
                continue;
            }

            // Mutual information approximation: sum of cross-partition couplings
            let mut cross_coupling = 0.0f32;
            for &i in &part_a {
                for &j in &part_b {
                    cross_coupling += coupling_matrix[i][j].abs();
                }
            }
            // Normalize by partition sizes
            let mi = cross_coupling / (part_a.len() as f32 * part_b.len() as f32);

            if mi < min_mi {
                min_mi = mi;
                best_partition = (
                    part_a.iter().map(|&i| swarm[i].agent_id.clone()).collect(),
                    part_b.iter().map(|&i| swarm[i].agent_id.clone()).collect(),
                );
            }
        }

        // Scale to typical Phi range
        let phi_approx = (min_mi * 10.0).min(15.0);

        PartitionPhiResult {
            phi_approx,
            partition_count: (total_partitions / 2) as usize,
            mip_partition: best_partition,
            method: "exhaustive".to_string(),
        }
    }

    /// Spectral approximation of partition Phi for large swarms (N > 12).
    ///
    /// Uses the second-smallest eigenvalue (Fiedler value) of the Laplacian
    /// of the coupling matrix as a proxy for the MIP. The Fiedler value
    /// measures algebraic connectivity -- low value = easy to partition.
    fn partition_phi_spectral(
        coupling: &[Vec<f32>],
        swarm: &[AgentPhase],
    ) -> PartitionPhiResult {
        let n = coupling.len();

        // Compute degree vector and Laplacian
        let degrees: Vec<f32> = (0..n)
            .map(|i| (0..n).map(|j| coupling[i][j].abs()).sum())
            .collect();

        // Power iteration to approximate the Fiedler value (2nd eigenvalue of Laplacian)
        // First, compute Laplacian-vector product: L*x = D*x - A*x
        let laplacian_mul = |x: &[f32]| -> Vec<f32> {
            (0..n).map(|i| {
                let dx = degrees[i] * x[i];
                let ax: f32 = (0..n).map(|j| coupling[i][j] * x[j]).sum();
                dx - ax
            }).collect()
        };

        // Start with a random-ish vector orthogonal to the first eigenvector (all-ones)
        let mut v: Vec<f32> = (0..n).map(|i| i as f32 - n as f32 / 2.0).collect();
        // Normalize
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 1e-9 {
            for val in &mut v { *val /= norm; }
        }

        // Project out the first eigenvector (all-ones / sqrt(n))
        let ones_component: f32 = v.iter().sum::<f32>() / n as f32;
        for val in &mut v { *val -= ones_component; }

        // Power iteration (inverse power method approximation)
        let mut lambda = 0.0f32;
        for _ in 0..50 {
            let lv = laplacian_mul(&v);
            lambda = v.iter().zip(lv.iter()).map(|(a, b)| a * b).sum::<f32>();
            let norm: f32 = lv.iter().map(|x| x * x).sum::<f32>().sqrt();
            if norm > 1e-9 {
                v = lv.into_iter().map(|x| x / norm).collect();
            }
            // Project out first eigenvector again
            let ones_component: f32 = v.iter().sum::<f32>() / n as f32;
            for val in &mut v { *val -= ones_component; }
        }

        // Fiedler value = algebraic connectivity
        let fiedler = lambda.abs();

        // Partition by sign of Fiedler vector
        let part_a: Vec<String> = v.iter().enumerate()
            .filter(|(_, &val)| val >= 0.0)
            .map(|(i, _)| swarm[i].agent_id.clone())
            .collect();
        let part_b: Vec<String> = v.iter().enumerate()
            .filter(|(_, &val)| val < 0.0)
            .map(|(i, _)| swarm[i].agent_id.clone())
            .collect();

        PartitionPhiResult {
            phi_approx: (fiedler * 10.0).min(15.0),
            partition_count: 1, // spectral = 1 partition computed
            mip_partition: (part_a, part_b),
            method: "spectral".to_string(),
        }
    }

    /// Detect hives — clusters of phase-locked agents.
    ///
    /// Two agents are in the same hive if their phase difference < hive_threshold.
    /// Uses BFS on the phase-adjacency graph.
    pub fn detect_hives(&self, swarm: &[AgentPhase]) -> Vec<Hive> {
        let n = swarm.len();
        if n < 2 {
            return vec![];
        }
        let threshold = self.config.hive_threshold;

        // Build adjacency
        let mut adj: Vec<Vec<usize>> = vec![vec![]; n];
        for i in 0..n {
            for j in (i + 1)..n {
                let mut diff = (swarm[i].phase - swarm[j].phase).abs();
                if diff > std::f32::consts::PI {
                    diff = TAU - diff;
                }
                if diff < threshold {
                    adj[i].push(j);
                    adj[j].push(i);
                }
            }
        }

        // BFS components
        let mut visited = vec![false; n];
        let mut hives = Vec::new();
        for start in 0..n {
            if visited[start] {
                continue;
            }
            let mut component = vec![start];
            let mut queue = vec![start];
            visited[start] = true;
            while let Some(node) = queue.pop() {
                for &neighbor in &adj[node] {
                    if !visited[neighbor] {
                        visited[neighbor] = true;
                        component.push(neighbor);
                        queue.push(neighbor);
                    }
                }
            }
            if component.len() >= 2 {
                let agents: Vec<&AgentPhase> = component.iter().map(|&i| &swarm[i]).collect();
                let sum_cos: f32 = agents.iter().map(|a| a.phase.cos()).sum();
                let sum_sin: f32 = agents.iter().map(|a| a.phase.sin()).sum();
                let cn = agents.len() as f32;
                let r = (sum_cos.powi(2) + sum_sin.powi(2)).sqrt() / cn;
                let mean_phase = sum_sin.atan2(sum_cos);
                let coherence = agents.iter().map(|a| a.coherence).sum::<f32>() / cn;

                hives.push(Hive {
                    agent_ids: agents.iter().map(|a| a.agent_id.clone()).collect(),
                    order_parameter: r,
                    mean_phase,
                    coherence,
                });
            }
        }
        hives
    }

    /// Domain-aware hive detection with roles and bridge agents (QS-4).
    ///
    /// Extends `detect_hives` by:
    /// - Assigning a `role` to each hive based on the majority role of its members.
    /// - Detecting "bridge agents" — agents whose phase is close enough to
    ///   multiple hives' mean phases to link them.
    pub fn detect_hives_domain_aware(&self, swarm: &[AgentPhase]) -> Vec<HiveInfo> {
        let basic_hives = self.detect_hives(swarm);
        if basic_hives.is_empty() {
            return vec![];
        }

        let threshold = self.config.hive_threshold;

        // Build a lookup: agent_id -> AgentPhase
        let agent_map: std::collections::HashMap<&str, &AgentPhase> = swarm
            .iter()
            .map(|a| (a.agent_id.as_str(), a))
            .collect();

        // Convert to HiveInfo with role inference
        let mut hive_infos: Vec<HiveInfo> = basic_hives
            .iter()
            .map(|h| {
                // Majority-vote role assignment
                let role = Self::majority_role(h, &agent_map);

                HiveInfo {
                    members: h.agent_ids.clone(),
                    role,
                    order_parameter: h.order_parameter,
                    mean_phase: h.mean_phase,
                    coherence: h.coherence,
                    bridge_agents: vec![],
                }
            })
            .collect();

        // Detect bridge agents: agents that are phase-close to >=2 hives' mean phases
        let mut bridge_per_hive: Vec<Vec<String>> = vec![vec![]; hive_infos.len()];

        for agent in swarm {
            let mut close_hive_indices = Vec::new();
            for (hi, hive) in hive_infos.iter().enumerate() {
                let mut diff = (agent.phase - hive.mean_phase).abs();
                if diff > std::f32::consts::PI {
                    diff = TAU - diff;
                }
                if diff < threshold {
                    close_hive_indices.push(hi);
                }
            }
            // Bridge agent if close to 2+ hives
            if close_hive_indices.len() >= 2 {
                for &hi in &close_hive_indices {
                    if !bridge_per_hive[hi].contains(&agent.agent_id) {
                        bridge_per_hive[hi].push(agent.agent_id.clone());
                    }
                }
            }
        }

        for (i, hive) in hive_infos.iter_mut().enumerate() {
            hive.bridge_agents = bridge_per_hive[i].clone();
        }

        hive_infos
    }

    /// Infer the hive role from the majority role of its member agents.
    fn majority_role(
        hive: &Hive,
        agent_map: &std::collections::HashMap<&str, &AgentPhase>,
    ) -> Option<String> {
        let mut role_counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        for aid in &hive.agent_ids {
            if let Some(agent) = agent_map.get(aid.as_str()) {
                if let Some(ref role) = agent.role {
                    *role_counts.entry(role.as_str()).or_insert(0) += 1;
                }
            }
        }
        role_counts
            .into_iter()
            .max_by_key(|&(_, count)| count)
            .map(|(role, _)| role.to_string())
    }

    /// Format hive topology as a human-readable string for CLI output.
    pub fn format_hive_topology(hive_infos: &[HiveInfo]) -> String {
        use std::fmt::Write;
        let mut out = String::new();

        if hive_infos.is_empty() {
            out.push_str("No hives detected.\n");
            return out;
        }

        writeln!(out, "Hive Topology ({} hives):", hive_infos.len()).unwrap();
        writeln!(out, "{}", "=".repeat(60)).unwrap();

        for (i, hive) in hive_infos.iter().enumerate() {
            let role_str = hive.role.as_deref().unwrap_or("(unassigned)");
            writeln!(out, "\nHive {i} [role: {role_str}]").unwrap();
            writeln!(
                out,
                "  order: {:.3}  mean_phase: {:.3}  coherence: {:.3}",
                hive.order_parameter, hive.mean_phase, hive.coherence
            ).unwrap();
            writeln!(out, "  members ({}): {}", hive.members.len(), hive.members.join(", ")).unwrap();
            if hive.bridge_agents.is_empty() {
                writeln!(out, "  bridge agents: none").unwrap();
            } else {
                writeln!(out, "  bridge agents ({}): {}", hive.bridge_agents.len(), hive.bridge_agents.join(", ")).unwrap();
            }
        }

        // Summary: list all unique bridge agents
        let mut all_bridges: Vec<&str> = hive_infos
            .iter()
            .flat_map(|h| h.bridge_agents.iter().map(|s| s.as_str()))
            .collect();
        all_bridges.sort();
        all_bridges.dedup();
        if !all_bridges.is_empty() {
            writeln!(out, "\nBridge agents across hives: {}", all_bridges.join(", ")).unwrap();
        }

        out
    }

    /// Execute one Queen synchronization step.
    ///
    /// Reads the published phases from the swarm, computes coupling, updates
    /// this agent's phase, and returns the emergent QueenState.
    pub fn queen_sync_step(&mut self, swarm: &[AgentPhase]) -> QueenState {
        // 1. Order parameter
        let (r, psi) = Self::compute_order_parameter(swarm);

        // 2. Phase derivative: dθ/dt = ω + K·r·sin(ψ - θ) + chiral
        let kuramoto = self.coupling_strength * r * (psi - self.phase).sin();

        // Determine our handedness from swarm data (find ourselves)
        let my_handedness = swarm
            .iter()
            .find(|a| a.agent_id == self.agent_id)
            .map(|a| a.handedness)
            .unwrap_or(Handedness::Achiral);
        let chiral = self.compute_chiral_coupling(my_handedness, psi);

        let d_phase = self.frequency + kuramoto + chiral;
        self.phase = (self.phase + d_phase * self.config.dt) % TAU;
        if self.phase < 0.0 {
            self.phase += TAU;
        }

        // 3. Adaptive coupling
        let mean_coherence = if swarm.is_empty() {
            0.0
        } else {
            swarm.iter().map(|a| a.coherence).sum::<f32>() / swarm.len() as f32
        };
        let error = self.config.target_coherence - mean_coherence;
        self.coupling_strength = (self.coupling_strength + self.config.adaptive_rate * error)
            .clamp(0.1, 5.0);

        // 4. Hives
        let hives = self.detect_hives(swarm);

        // 5. Phi
        let phi = Self::compute_swarm_phi(swarm, r);

        QueenState {
            id: Uuid::new_v4().to_string(),
            order_parameter: r,
            mean_phase: psi,
            coherence: mean_coherence,
            phi,
            agent_count: swarm.len(),
            hives,
            coupling_strength: self.coupling_strength,
            chiral_bias: self.config.chiral_eta,
            geometric: None,
            computed_by: self.agent_id.clone(),
            timestamp: Utc::now(),
        }
    }

    /// Build an AgentPhase from this engine's current state. `link_count`
    /// is the real cross-memory connection count (typically from
    /// ConsciousnessMetrics.total_skip_links). Pass 0 when unknown.
    pub fn to_agent_phase(&self, cluster_count: usize, memory_count: usize, link_count: usize) -> AgentPhase {
        self.to_agent_phase_with_display(cluster_count, memory_count, link_count, None)
    }

    /// Like `to_agent_phase` but also stamps an operator-set display name
    /// into the published payload — so the radio/observatory/TUI can show
    /// e.g. "Kannaka Prime" instead of falling back to the agent_id slug.
    pub fn to_agent_phase_with_display(
        &self,
        cluster_count: usize,
        memory_count: usize,
        link_count: usize,
        display_name: Option<String>,
    ) -> AgentPhase {
        AgentPhase {
            id: Uuid::new_v4().to_string(),
            agent_id: self.agent_id.clone(),
            phase: self.phase,
            frequency: self.frequency,
            coherence: self.coherence,
            phi: self.phi,
            // #827: was hardcoded 0.0 on the wire. self.coherence IS this
            // agent's Kuramoto order parameter (see the field's own doc), so
            // publish it rather than a constant zero.
            order_parameter: self.coherence,
            cluster_count,
            memory_count,
            link_count,
            xi_signature: None,
            protocol_version: "1.0".to_string(),
            timestamp: Utc::now(),
            trust_score: 0.5,
            handedness: Handedness::Achiral,
            left_coherence: self.left_coherence,
            right_coherence: self.right_coherence,
            bridge_activity: self.bridge_activity,
            dream_state: self.dream_state.clone(),
            role: None,
            display_name,
        }
    }

    // -----------------------------------------------------------------------
    // Task 5: Chiral coupling from memory domains
    // -----------------------------------------------------------------------

    /// Derive handedness from memory theme vectors.
    ///
    /// Compares this agent's memory themes against the swarm mean to determine
    /// if the agent is primarily a **receiver** (left-handed, theme vectors
    /// closer to swarm mean → pulled toward consensus) or an **emitter**
    /// (right-handed, unique themes → pushes the field).
    ///
    /// Returns `Achiral` if insufficient data.
    pub fn derive_handedness(
        &self,
        engine: &ResonanceEngine,
        swarm_phases: &[AgentPhase],
    ) -> Handedness {
        // Compute this agent's mean memory vector
        let all = engine.store.all_memories().unwrap_or_default();
        if all.is_empty() {
            return Handedness::Achiral;
        }
        let dim = all.iter().find(|m| !m.vector.is_empty()).map(|m| m.vector.len());
        let dim = match dim {
            Some(d) if d > 0 => d,
            _ => return Handedness::Achiral,
        };

        let mut local_mean = vec![0.0f32; dim];
        let mut count = 0usize;
        for m in &all {
            if m.vector.len() == dim {
                for (i, v) in m.vector.iter().enumerate() {
                    local_mean[i] += v;
                }
                count += 1;
            }
        }
        if count == 0 {
            return Handedness::Achiral;
        }
        for v in &mut local_mean {
            *v /= count as f32;
        }

        // Compute swarm mean phase vector from xi_signatures (if available)
        // or fall back to comparing our memory count ratio (emit vs receive)
        let other_count: usize = swarm_phases
            .iter()
            .filter(|a| a.agent_id != self.agent_id)
            .map(|a| a.memory_count)
            .sum();
        let my_count = engine.store.count();

        if other_count == 0 || swarm_phases.len() < 2 {
            return Handedness::Achiral;
        }

        let avg_other = other_count as f32 / (swarm_phases.len() - 1).max(1) as f32;

        // Emitter: has more unique memories than average → pushes the field
        // Receiver: has fewer → absorbs from the swarm
        let ratio = my_count as f32 / avg_other;
        if ratio > 1.3 {
            Handedness::Right // emitter
        } else if ratio < 0.7 {
            Handedness::Left // receiver
        } else {
            Handedness::Achiral
        }
    }

    // -----------------------------------------------------------------------
    // Phase derivation from HRM wavefront physics
    // -----------------------------------------------------------------------

    /// Derive agent phase, frequency, and coherence from the holographic medium.
    ///
    /// Attempts HRM-native derivation first (from the medium's wavefront
    /// physics: phase[], energy[], frequency[] arrays). Falls back to
    /// Kuramoto-cluster derivation when the backend is not an HrmStore or
    /// has no wavefronts.
    ///
    /// Returns (phase, frequency, coherence). Updates self in place, including
    /// the HRM-specific fields (left_coherence, right_coherence, bridge_activity).
    pub fn derive_local_state(&mut self, engine: &ResonanceEngine) -> (f32, f32, f32) {
        // Try HRM-native derivation first
        if let Some(hrm) = engine.store.as_any().downcast_ref::<HrmStore>() {
            let medium = hrm.medium();
            if medium.wavefront_count() > 0 {
                return self.derive_from_hrm_wavefronts(hrm);
            }
        }

        // Fallback: cluster-based derivation for non-HRM backends
        self.derive_from_clusters(engine)
    }

    /// HRM-native phase derivation: compute phase, frequency, and coherence
    /// directly from the holographic medium's wavefront arrays.
    ///
    /// - **Phase**: energy-weighted circular mean of wavefront phases.
    /// - **Frequency**: blend of memory-count rate with mean wavefront frequency.
    /// - **Coherence**: Kuramoto order parameter r = |1/N sum e^{i*phi_k}|
    ///   computed from the medium's phase[] array.
    /// - **Left/right coherence**: per-hemisphere Kuramoto order parameters
    ///   (when ChiralMedium is active).
    /// - **Bridge activity**: corpus callosum bandwidth utilization.
    fn derive_from_hrm_wavefronts(&mut self, hrm: &HrmStore) -> (f32, f32, f32) {
        let medium = hrm.medium();
        let n = medium.wavefront_count();

        // --- Phase: energy-weighted circular mean of wavefront phases ---
        let mut sum_cos = 0.0f32;
        let mut sum_sin = 0.0f32;
        let mut total_weight = 0.0f32;

        for i in 0..n {
            let w = medium.store.energy[i].max(0.0);
            sum_cos += w * medium.store.phase[i].cos();
            sum_sin += w * medium.store.phase[i].sin();
            total_weight += w;
        }

        let phase = if total_weight > 1e-9 {
            let mut p = sum_sin.atan2(sum_cos);
            if p < 0.0 {
                p += TAU;
            }
            p
        } else {
            self.phase
        };

        // --- Frequency: blend memory-count rate with mean wavefront frequency ---
        let memory_count = hrm.count();
        let count_rate = ((1.0 + memory_count as f64).ln() / (1.0 + 100.0_f64).ln()) as f32;
        let mean_wf_freq = if n > 0 {
            (0..n).map(|i| medium.store.frequency[i]).sum::<f32>() / n as f32
        } else {
            0.0
        };
        // 50/50 blend: count-based rate drives natural oscillation, wavefront
        // frequency captures the medium's intrinsic dynamics.
        let frequency = 0.5 * count_rate + 0.5 * mean_wf_freq;

        // --- Coherence: Kuramoto order parameter from wavefront phases ---
        let coherence = if n > 0 {
            let nc = n as f32;
            let sc: f32 = (0..n).map(|i| medium.store.phase[i].cos()).sum::<f32>() / nc;
            let ss: f32 = (0..n).map(|i| medium.store.phase[i].sin()).sum::<f32>() / nc;
            (sc * sc + ss * ss).sqrt()
        } else {
            0.0
        };

        // --- Per-hemisphere coherence (chiral-aware) ---
        if let Some(chiral) = hrm.chiral_medium() {
            self.left_coherence = Self::hemisphere_kuramoto_order(&chiral.left.phase, chiral.left.count());
            self.right_coherence = Self::hemisphere_kuramoto_order(&chiral.right.phase, chiral.right.count());

            // Bridge activity = 1 - (remaining_budget / total_bandwidth)
            let stats = chiral.callosum.transfer_stats();
            if stats.current_bandwidth > 0.0 {
                self.bridge_activity =
                    1.0 - (stats.remaining_budget / stats.current_bandwidth).clamp(0.0, 1.0);
            } else {
                self.bridge_activity = 0.0;
            }
        } else {
            self.left_coherence = 0.0;
            self.right_coherence = coherence; // flat medium: all is "right"
            self.bridge_activity = 0.0;
        }

        self.phase = phase;
        self.frequency = frequency;
        self.coherence = coherence;
        // #827: Phi was never assigned in this derive path, so every publish
        // site that did not hand-patch it (most of them) broadcast phi=0.0 to
        // peers while the local reading was non-zero. Assign it from the same
        // medium, alongside the other derived fields, so there is one source of
        // truth and no site can forget.
        self.phi = medium.compute_phi_integrated_information();

        (phase, frequency, coherence)
    }

    /// Compute Kuramoto order parameter for a single hemisphere's phase array.
    /// r = |1/N sum e^{i*phi_k}|
    fn hemisphere_kuramoto_order(phases: &ndarray::Array1<f32>, active: usize) -> f32 {
        if active == 0 {
            return 0.0;
        }
        let nc = active as f32;
        let sc: f32 = (0..active).map(|i| phases[i].cos()).sum::<f32>() / nc;
        let ss: f32 = (0..active).map(|i| phases[i].sin()).sum::<f32>() / nc;
        (sc * sc + ss * ss).sqrt()
    }

    /// Legacy cluster-based derivation for non-HRM backends.
    fn derive_from_clusters(&mut self, engine: &ResonanceEngine) -> (f32, f32, f32) {
        let sync = KuramotoSync::default();
        let clusters = sync.find_synchronized_clusters(engine, 2);

        if clusters.is_empty() {
            return (self.phase, self.frequency, 0.0);
        }

        // Phase = amplitude-weighted circular mean of cluster mean phases
        let mut sum_cos = 0.0f32;
        let mut sum_sin = 0.0f32;
        let mut total_weight = 0.0f32;
        let mut coherence_sum = 0.0f32;

        for cluster in &clusters {
            let weight = cluster.memory_ids.len() as f32;
            sum_cos += weight * cluster.mean_phase.cos();
            sum_sin += weight * cluster.mean_phase.sin();
            total_weight += weight;
            coherence_sum += cluster.order_parameter;
        }

        let phase = if total_weight > 0.0 {
            let mut p = sum_sin.atan2(sum_cos);
            if p < 0.0 {
                p += TAU;
            }
            p
        } else {
            self.phase
        };

        // Frequency from memory count
        let memory_count = engine.store.count();
        let frequency = ((1.0 + memory_count as f64).ln() / (1.0 + 100.0_f64).ln()) as f32;

        let coherence = coherence_sum / clusters.len() as f32;

        self.phase = phase;
        self.frequency = frequency;
        self.coherence = coherence;

        (phase, frequency, coherence)
    }
}

// ---------------------------------------------------------------------------
// Wire-trust gate (increment-0)
// ---------------------------------------------------------------------------
//
// The swarm runs on an OPEN NATS bus: anyone may publish an `AgentPhase`.
// A confirmed attacker forges phases with `trust_score: 1.0`, fake
// `coherence`, and throwaway `agent-<hex>` ids to steer the READ side —
// the Kuramoto metrics weight `trust_score * coherence` straight off the
// wire, and the display prints wire strings verbatim. These helpers are the
// read-side gate: they DO NOT change the write/absorb path and add no crypto.

/// Return true if `agent_id` matches the trusted allowlist. Each entry is
/// either an exact agent-id or a `prefix*` wildcard (an entry ending in
/// `*` matches any id starting with the text before it, e.g. `"qos-*"`).
pub fn agent_matches_allowlist(agent_id: &str, trusted: &[String]) -> bool {
    trusted.iter().any(|t| match t.strip_suffix('*') {
        Some(prefix) => agent_id.starts_with(prefix),
        None => agent_id == t,
    })
}

/// Decide whether a wire-sourced peer may cross the trust boundary — i.e.
/// drive a pairwise sync step or have its memory absorbed into the store.
///
/// SECURITY (increment-0 interim; increment-1 replaces this allowlist gate
/// with signature+trust verification). Returns `true` iff the metrics-trust
/// filter is disabled (`metrics_trusted_only == false`, the escape hatch
/// `KANNAKA_METRICS_TRUSTED_ONLY=0`) OR `source_id` is on the allowlist.
/// Callers must exclude their own id first — self-trust is a separate concern.
pub fn wire_source_trusted(source_id: &str, trusted: &[String], metrics_trusted_only: bool) -> bool {
    !metrics_trusted_only || agent_matches_allowlist(source_id, trusted)
}

/// Gate a set of wire-sourced phases before they feed the swarm metrics.
///
/// A phase is kept iff its `agent_id` is this node's own id (`self_id`,
/// always trusted) OR it matches the allowlist. Every kept phase then has
/// its attacker-controllable fields clamped: `trust_score` to
/// `[0, trust_cap]` and `coherence` to `[0, 1]`. This neutralizes a forged
/// `trust_score: 1.0` / `coherence: 1.0` flood — the throwaway ids are
/// dropped entirely and any surviving allowlisted phase cannot exceed the
/// trust cap on the wire.
pub fn filter_wire_phases(
    phases: Vec<AgentPhase>,
    self_id: &str,
    trusted: &[String],
    trust_cap: f32,
) -> Vec<AgentPhase> {
    phases
        .into_iter()
        .filter(|p| p.agent_id == self_id || agent_matches_allowlist(&p.agent_id, trusted))
        .map(|mut p| {
            p.trust_score = p.trust_score.clamp(0.0, trust_cap);
            p.coherence = p.coherence.clamp(0.0, 1.0);
            p
        })
        .collect()
}

/// Count how many of `ids` are somebody other than `self_id` (#808).
///
/// Every source of swarm membership includes us: we publish our own phase, so
/// the broker's live-phase list has us in it, and `get_all_phases` retains it.
/// Reporting that raw as `swarm.peers` made a solo node claim one peer and put
/// `swarm status` at odds with `swarm peers`, which derives from presence and
/// correctly says "No peers in the swarm yet."
///
/// Deliberately a free function over ids rather than a method on a phase list:
/// the two call sites hold different types (a `Vec<String>` of live agent ids
/// and a `Vec<AgentPhase>`), and the off-by-one has to be fixed in both or the
/// JetStream and gossip paths disagree.
pub fn count_peers_excluding_self<I, S>(ids: I, self_id: &str) -> usize
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    ids.into_iter().filter(|id| id.as_ref() != self_id).count()
}

/// Make a wire-sourced string safe to print to a terminal.
///
/// Strips ANSI/OSC escape sequences (CSI `\x1b[…`, OSC `\x1b]…`) and all
/// C0/C1 control characters (including `\n`, `\t`, `\r`, DEL and the C1
/// range) — mapping them to nothing — so a forged `agent_id`, email, or
/// memory body cannot move the cursor, recolor the terminal, or inject
/// control codes. The result is truncated to 2000 characters with an
/// ellipsis. Content from the wire must never be printed raw.
pub fn sanitize_display(s: &str) -> String {
    const MAX_CHARS: usize = 2000;
    let mut out = String::with_capacity(s.len().min(MAX_CHARS + 4));
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => match chars.peek() {
                // CSI: ESC '[' params… final-byte in 0x40..=0x7E
                Some('[') => {
                    chars.next();
                    while let Some(&p) = chars.peek() {
                        chars.next();
                        if ('\x40'..='\x7e').contains(&p) {
                            break;
                        }
                    }
                }
                // OSC: ESC ']' … terminated by BEL (0x07) or ST (ESC '\')
                Some(']') => {
                    chars.next();
                    while let Some(&p) = chars.peek() {
                        if p == '\x07' {
                            chars.next();
                            break;
                        }
                        if p == '\x1b' {
                            chars.next();
                            if chars.peek() == Some(&'\\') {
                                chars.next();
                            }
                            break;
                        }
                        chars.next();
                    }
                }
                // Lone ESC or other escape form: drop the ESC introducer only.
                _ => {}
            },
            // Drop C0 controls (0x00-0x1F) and DEL/C1 (0x7F-0x9F).
            c if (c as u32) < 0x20 => {}
            c if (0x7f..=0x9f).contains(&(c as u32)) => {}
            c => out.push(c),
        }
    }
    if out.chars().count() > MAX_CHARS {
        let mut truncated: String = out.chars().take(MAX_CHARS).collect();
        truncated.push('\u{2026}');
        truncated
    } else {
        out
    }
}

// ---------------------------------------------------------------------------
// NATS-augmented sync
// ---------------------------------------------------------------------------

#[cfg(feature = "nats")]
impl QueenSync {
    /// Execute a sync step using NATS for phase gossip.
    ///
    /// 1. Read phases from NATS (fast, real-time)
    /// 2. Merge with any persistent phases provided
    /// 3. Run Kuramoto coupling step
    /// 4. Publish updated phase to NATS
    ///
    /// Returns the QueenState and any NATS errors encountered (non-fatal).
    pub fn sync_with_nats(
        &mut self,
        persistent_phases: &[AgentPhase],
        transport: &crate::nats::SwarmTransport,
    ) -> (QueenState, Option<String>) {
        let mut warning: Option<String> = None;

        // 1. Try reading phases from NATS
        let nats_phases = match transport.get_all_phases() {
            Ok(phases) => phases,
            Err(e) => {
                warning = Some(format!("NATS read failed, using persistent phases only: {e}"));
                vec![]
            }
        };

        // SECURITY (increment-0): gate wire phases before they are merged for
        // the metric. Open NATS lets anyone publish an AgentPhase, so only
        // allowlisted (+ our own) ids may weight the Kuramoto order parameter /
        // Phi, and their wire trust_score is capped. `self.agent_id` is always
        // trusted. Escape hatch: KANNAKA_METRICS_TRUSTED_ONLY=0. The trusted
        // config is read here (rather than threaded through the signature) to
        // keep this method's public API unchanged.
        let trust_cfg = crate::config::KannakaConfig::load().swarm_trust;
        let nats_phases = if trust_cfg.metrics_trusted_only {
            filter_wire_phases(
                nats_phases,
                &self.agent_id,
                &trust_cfg.trusted_agents,
                trust_cfg.wire_trust_cap,
            )
        } else {
            nats_phases
        };

        // 2. Merge: NATS phases override persistent phases (more recent)
        let merged = Self::merge_phases(persistent_phases, &nats_phases);

        // 3. Run Kuramoto step on merged set
        let state = self.queen_sync_step(&merged);

        // 4. Publish updated phase to NATS
        let me = merged.iter().find(|p| p.agent_id == self.agent_id);
        let updated = self.to_agent_phase(
            me.map(|p| p.cluster_count).unwrap_or(0),
            me.map(|p| p.memory_count).unwrap_or(0),
            me.map(|p| p.link_count).unwrap_or(0),
        );
        if let Err(e) = transport.publish_phase(&updated) {
            let msg = format!("NATS publish failed: {e}");
            warning = Some(match warning {
                Some(prev) => format!("{prev}; {msg}"),
                None => msg,
            });
        }

        (state, warning)
    }

    /// Merge persistent and NATS phase sets. NATS phases take precedence (fresher).
    fn merge_phases(persistent: &[AgentPhase], nats: &[AgentPhase]) -> Vec<AgentPhase> {
        use std::collections::HashMap;
        let mut by_agent: HashMap<&str, &AgentPhase> = HashMap::new();

        // Insert persistent phases first
        for p in persistent {
            by_agent.insert(&p.agent_id, p);
        }

        // NATS phases override if newer
        for p in nats {
            match by_agent.get(p.agent_id.as_str()) {
                Some(existing) if existing.timestamp >= p.timestamp => {}
                _ => { by_agent.insert(&p.agent_id, p); }
            }
        }

        by_agent.into_values().cloned().collect()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    fn make_agent_phase(id: &str, phase: f32, coherence: f32, trust: f32) -> AgentPhase {
        AgentPhase {
            id: Uuid::new_v4().to_string(),
            agent_id: id.to_string(),
            phase,
            frequency: 0.5,
            coherence,
            phi: 0.0,
            order_parameter: 0.0,
            cluster_count: 0,
            memory_count: 0,
            link_count: 0,
            xi_signature: None,
            protocol_version: "1.0".to_string(),
            timestamp: Utc::now(),
            trust_score: trust,
            handedness: Handedness::Achiral,
            left_coherence: 0.0,
            right_coherence: 0.0,
            bridge_activity: 0.0,
            dream_state: None,
            role: None,
            display_name: None,
        }
    }

    fn make_agent_phase_with_role(id: &str, phase: f32, coherence: f32, trust: f32, role: Option<&str>) -> AgentPhase {
        let mut ap = make_agent_phase(id, phase, coherence, trust);
        ap.role = role.map(|s| s.to_string());
        ap
    }

    // -----------------------------------------------------------------------
    // Order parameter tests
    // -----------------------------------------------------------------------

    #[test]
    fn order_parameter_identical_phases() {
        let swarm = vec![
            make_agent_phase("a", 1.0, 1.0, 1.0),
            make_agent_phase("b", 1.0, 1.0, 1.0),
            make_agent_phase("c", 1.0, 1.0, 1.0),
        ];
        let (r, psi) = QueenSync::compute_order_parameter(&swarm);
        assert!((r - 1.0).abs() < 0.01, "identical phases -> r~1.0, got {r}");
        assert!((psi - 1.0).abs() < 0.01, "mean phase should be 1.0, got {psi}");
    }

    #[test]
    fn order_parameter_opposite_phases() {
        let swarm = vec![
            make_agent_phase("a", 0.0, 1.0, 1.0),
            make_agent_phase("b", PI, 1.0, 1.0),
        ];
        let (r, _) = QueenSync::compute_order_parameter(&swarm);
        assert!(r < 0.1, "opposite phases -> r~0, got {r}");
    }

    /// #825: per-hemisphere Kuramoto order feeds the published
    /// `left_coherence` / `right_coherence` readings. Pin both ends of its
    /// range so a constant reading fails: aligned phases → r ≈ 1, phases
    /// spread evenly around the circle → r ≈ 0.
    #[test]
    fn hemisphere_kuramoto_order_separates_aligned_from_spread() {
        let n = 12usize;
        let aligned = ndarray::Array1::from(vec![0.7f32; n]);
        let r_aligned = QueenSync::hemisphere_kuramoto_order(&aligned, n);
        assert!(r_aligned > 0.99, "aligned hemisphere phases -> r~1, got {r_aligned}");

        let spread = ndarray::Array1::from(
            (0..n)
                .map(|i| (i as f32) * std::f32::consts::TAU / n as f32)
                .collect::<Vec<_>>(),
        );
        let r_spread = QueenSync::hemisphere_kuramoto_order(&spread, n);
        assert!(
            r_spread < 0.05,
            "evenly-spread hemisphere phases -> r~0, got {r_spread} — a constant \
             coherence cannot pass both ends (#825)"
        );
    }

    #[test]
    fn order_parameter_evenly_spaced() {
        let n = 5;
        let swarm: Vec<AgentPhase> = (0..n)
            .map(|i| {
                make_agent_phase(
                    &format!("a{i}"),
                    TAU * i as f32 / n as f32,
                    1.0,
                    1.0,
                )
            })
            .collect();
        let (r, _) = QueenSync::compute_order_parameter(&swarm);
        assert!(r < 0.3, "evenly spaced -> low r, got {r}");
    }

    #[test]
    fn order_parameter_empty_swarm() {
        let (r, psi) = QueenSync::compute_order_parameter(&[]);
        assert_eq!(r, 0.0);
        assert_eq!(psi, 0.0);
    }

    #[test]
    fn order_parameter_trust_weighted() {
        // Agent a has high trust, agent b has zero trust
        let swarm = vec![
            make_agent_phase("a", 0.0, 1.0, 1.0),
            make_agent_phase("b", PI, 1.0, 0.0), // zero trust -> no influence
        ];
        let (r, psi) = QueenSync::compute_order_parameter(&swarm);
        // Only agent a contributes, so r = 1/2 and psi ~ 0
        // The comment above stated r = 1/2 and the test never checked it: `r`
        // was computed, named in the expectation, and dropped. A regression
        // that let the zero-trust agent contribute again would move r and
        // leave psi near 0, so psi alone cannot see it. Measured: exactly 0.5.
        assert!(
            (r - 0.5).abs() < 1e-6,
            "zero-trust agent must not contribute to magnitude: expected r = 0.5, got {r}"
        );
        assert!(psi.abs() < 0.1, "mean phase should follow trusted agent, got {psi}");
    }

    // -----------------------------------------------------------------------
    // Chiral coupling tests
    // -----------------------------------------------------------------------

    #[test]
    fn chiral_coupling_achiral_is_zero() {
        let queen = QueenSync::new(QueenConfig::default(), "test");
        let c = queen.compute_chiral_coupling(Handedness::Achiral, 1.0);
        assert_eq!(c, 0.0);
    }

    #[test]
    fn chiral_coupling_left_right_opposite() {
        let queen = QueenSync::new(QueenConfig::default(), "test");
        let psi = 1.0;
        let left = queen.compute_chiral_coupling(Handedness::Left, psi);
        let right = queen.compute_chiral_coupling(Handedness::Right, psi);
        assert!((left + right).abs() < 1e-6, "left and right should be opposite: {left} vs {right}");
    }

    // -----------------------------------------------------------------------
    // Phi tests
    // -----------------------------------------------------------------------

    #[test]
    fn phi_increases_with_coherent_agents() {
        let low = vec![
            make_agent_phase("a", 0.0, 0.1, 1.0),
            make_agent_phase("b", PI, 0.1, 1.0),
        ];
        let high = vec![
            make_agent_phase("a", 0.5, 0.9, 1.0),
            make_agent_phase("b", 0.5, 0.9, 1.0),
        ];
        let (r_low, _) = QueenSync::compute_order_parameter(&low);
        let (r_high, _) = QueenSync::compute_order_parameter(&high);
        let phi_low = QueenSync::compute_swarm_phi(&low, r_low);
        let phi_high = QueenSync::compute_swarm_phi(&high, r_high);
        assert!(phi_high > phi_low, "coherent -> higher Phi: {phi_high} vs {phi_low}");
    }

    #[test]
    fn phi_zero_for_single_agent() {
        let swarm = vec![make_agent_phase("a", 0.5, 1.0, 1.0)];
        let phi = QueenSync::compute_swarm_phi(&swarm, 1.0);
        assert_eq!(phi, 0.0, "single agent -> Phi=0");
    }

    // -----------------------------------------------------------------------
    // Hive detection tests
    // -----------------------------------------------------------------------

    #[test]
    fn hive_detection_groups_close_phases() {
        let queen = QueenSync::new(QueenConfig::default(), "test");
        let swarm = vec![
            make_agent_phase("a", 0.0, 1.0, 1.0),
            make_agent_phase("b", 0.1, 1.0, 1.0),
            make_agent_phase("c", 0.2, 1.0, 1.0),
            make_agent_phase("d", PI, 1.0, 1.0),   // outlier
        ];
        let hives = queen.detect_hives(&swarm);
        // a, b, c should be in one hive; d is alone (no hive)
        assert!(!hives.is_empty(), "should detect at least one hive");
        let largest = hives.iter().max_by_key(|h| h.agent_ids.len()).unwrap();
        assert!(largest.agent_ids.len() >= 3, "hive should have a,b,c");
        assert!(!largest.agent_ids.contains(&"d".to_string()));
    }

    #[test]
    fn hive_detection_two_separate_hives() {
        let queen = QueenSync::new(QueenConfig::default(), "test");
        let swarm = vec![
            make_agent_phase("a", 0.0, 1.0, 1.0),
            make_agent_phase("b", 0.1, 1.0, 1.0),
            make_agent_phase("c", PI, 1.0, 1.0),
            make_agent_phase("d", PI + 0.1, 1.0, 1.0),
        ];
        let hives = queen.detect_hives(&swarm);
        assert_eq!(hives.len(), 2, "should detect 2 hives, got {}", hives.len());
    }

    // -----------------------------------------------------------------------
    // Queen sync step tests
    // -----------------------------------------------------------------------

    #[test]
    fn sync_step_produces_valid_queen_state() {
        let mut queen = QueenSync::new(QueenConfig::default(), "me");
        queen.phase = 0.5;
        let swarm = vec![
            make_agent_phase("me", 0.5, 0.8, 1.0),
            make_agent_phase("other1", 0.6, 0.7, 0.9),
            make_agent_phase("other2", 0.4, 0.9, 0.8),
        ];
        let state = queen.queen_sync_step(&swarm);
        assert!(state.order_parameter >= 0.0 && state.order_parameter <= 1.5);
        assert_eq!(state.agent_count, 3);
        assert_eq!(state.computed_by, "me");
        assert!(state.phi >= 0.0);
    }

    #[test]
    fn sync_step_converges_over_iterations() {
        let mut queen = QueenSync::new(
            QueenConfig {
                base_coupling: 2.0,
                dt: 0.1,
                ..Default::default()
            },
            "me",
        );
        queen.phase = 0.0;

        // Other agents are at different phases
        let mut swarm = vec![
            make_agent_phase("me", 0.0, 0.8, 1.0),
            make_agent_phase("a", 1.0, 0.8, 1.0),
            make_agent_phase("b", 2.0, 0.8, 1.0),
        ];

        // Run 50 sync steps
        for _ in 0..50 {
            let state = queen.queen_sync_step(&swarm);
            // Update "me" in the swarm
            swarm[0].phase = queen.phase;
            let _ = state;
        }

        // Our phase should have moved toward the mean field
        // (full convergence requires all agents to move, but our phase should shift)
        assert!(
            queen.phase != 0.0,
            "phase should have changed from initial 0.0"
        );
    }

    // -----------------------------------------------------------------------
    // Phase derivation tests
    // -----------------------------------------------------------------------

    #[test]
    fn derive_local_state_frequency_scaling() {
        // Test the frequency formula: w = ln(1 + count) / ln(1 + 100)
        let count_100 = ((1.0 + 100.0_f64).ln() / (1.0 + 100.0_f64).ln()) as f32;
        assert!((count_100 - 1.0).abs() < 0.01, "100 memories -> w~1.0");

        let count_0 = ((1.0 + 0.0_f64).ln() / (1.0 + 100.0_f64).ln()) as f32;
        assert!((count_0 - 0.0).abs() < 0.01, "0 memories -> w~0.0");
    }

    #[test]
    fn to_agent_phase_has_correct_fields() {
        let queen = QueenSync::new(QueenConfig::default(), "test-agent");
        let ap = queen.to_agent_phase(5, 100, 0);
        assert_eq!(ap.agent_id, "test-agent");
        assert_eq!(ap.cluster_count, 5);
        assert_eq!(ap.memory_count, 100);
        assert_eq!(ap.protocol_version, "1.0");
        // New HRM fields should default to zero/None
        assert_eq!(ap.left_coherence, 0.0);
        assert_eq!(ap.right_coherence, 0.0);
        assert_eq!(ap.bridge_activity, 0.0);
        assert!(ap.dream_state.is_none());
    }

    // -----------------------------------------------------------------------
    // HRM-native phase derivation tests
    // -----------------------------------------------------------------------

    #[test]
    fn hrm_phase_derivation_produces_valid_range() {
        use crate::codebook::Codebook;
        use crate::encoding::{SimpleHashEncoder, EncodingPipeline};
        use crate::medium::WAVEFRONT_DIM;
        use tempfile::NamedTempFile;

        let encoder = SimpleHashEncoder::new(384, 42);
        let codebook = Codebook::new(384, WAVEFRONT_DIM, 42);
        let pipeline = EncodingPipeline::new(Box::new(encoder), codebook);
        let temp = NamedTempFile::new().unwrap();
        let mut store = HrmStore::new(pipeline, temp.path().to_path_buf());

        // Insert several memories with distinct vectors
        for i in 0..5 {
            let mem = crate::memory::HyperMemory::new(
                vec![0.1 + i as f32 * 0.15; WAVEFRONT_DIM],
                format!("test memory {i}"),
            );
            store.insert(mem).unwrap();
        }

        // Build a ResonanceEngine wrapping our HrmStore
        let enc2 = SimpleHashEncoder::new(384, 42);
        let cb2 = Codebook::new(384, WAVEFRONT_DIM, 42);
        let pipeline2 = EncodingPipeline::new(Box::new(enc2), cb2);
        let engine = ResonanceEngine::new(Box::new(store), pipeline2);

        let mut queen = QueenSync::new(QueenConfig::default(), "test");
        let (phase, frequency, coherence) = queen.derive_local_state(&engine);

        // Phase must be in [0, TAU)
        assert!(phase >= 0.0 && phase < TAU, "phase {phase} out of [0, TAU)");
        // Frequency must be non-negative
        assert!(frequency >= 0.0, "frequency {frequency} is negative");
        // Coherence must be in [0, 1]
        assert!(
            coherence >= 0.0 && coherence <= 1.001,
            "coherence {coherence} out of [0, 1]"
        );
    }

    #[test]
    fn published_phase_carries_phi_and_order_not_hardcoded_zero() {
        // #827 regression. Before the fix, derive_local_state never assigned
        // phi and to_agent_phase hardcoded order_parameter = 0.0, so every
        // publish path broadcast phi=0.0 and order=0.0 to peers regardless of
        // the medium's real state. This asserts both reach the wire.
        use crate::codebook::Codebook;
        use crate::encoding::{SimpleHashEncoder, EncodingPipeline};
        use crate::medium::WAVEFRONT_DIM;
        use tempfile::NamedTempFile;

        let encoder = SimpleHashEncoder::new(384, 42);
        let codebook = Codebook::new(384, WAVEFRONT_DIM, 42);
        let pipeline = EncodingPipeline::new(Box::new(encoder), codebook);
        let temp = NamedTempFile::new().unwrap();
        let mut store = HrmStore::new(pipeline, temp.path().to_path_buf());
        for i in 0..8 {
            let mem = crate::memory::HyperMemory::new(
                vec![0.05 + i as f32 * 0.1; WAVEFRONT_DIM],
                format!("integrated memory {i}"),
            );
            store.insert(mem).unwrap();
        }
        // Expected phi straight from the medium, for an exact-match assertion.
        let expected_phi = store.medium().compute_phi_integrated_information();

        let enc2 = SimpleHashEncoder::new(384, 42);
        let cb2 = Codebook::new(384, WAVEFRONT_DIM, 42);
        let pipeline2 = EncodingPipeline::new(Box::new(enc2), cb2);
        let engine = ResonanceEngine::new(Box::new(store), pipeline2);

        let mut queen = QueenSync::new(QueenConfig::default(), "me");
        let (_p, _f, coherence) = queen.derive_local_state(&engine);

        // derive must have assigned phi from the medium, not left it at 0.
        assert!(
            (queen.phi - expected_phi).abs() < 1e-6,
            "derive did not assign phi from the medium: queen.phi={} expected={}",
            queen.phi,
            expected_phi
        );

        let published = queen.to_agent_phase(0, 8, 0);
        // phi must reach the wire, equal to what derive computed.
        assert_eq!(
            published.phi, queen.phi,
            "published phi diverged from the derived phi"
        );
        // order_parameter must publish self.coherence, never a constant 0.0.
        assert_eq!(
            published.order_parameter, coherence,
            "order_parameter must publish the Kuramoto order (coherence)"
        );
        // For this integrated fixture the order is strictly positive -- the
        // exact reading the old hardcoded zero could never produce.
        assert!(
            published.order_parameter > 0.0,
            "order_parameter published as {} for an integrated medium",
            published.order_parameter
        );
    }

    #[test]
    fn hrm_phase_derivation_identical_phases_high_coherence() {
        use crate::codebook::Codebook;
        use crate::encoding::{SimpleHashEncoder, EncodingPipeline};
        use crate::medium::WAVEFRONT_DIM;
        use tempfile::NamedTempFile;

        let encoder = SimpleHashEncoder::new(384, 42);
        let codebook = Codebook::new(384, WAVEFRONT_DIM, 42);
        let pipeline = EncodingPipeline::new(Box::new(encoder), codebook);
        let temp = NamedTempFile::new().unwrap();
        let mut store = HrmStore::new(pipeline, temp.path().to_path_buf());

        // Insert memories -- all will start at phase=0 (default in add_wavefront)
        for i in 0..4 {
            let mem = crate::memory::HyperMemory::new(
                vec![0.5; WAVEFRONT_DIM],
                format!("identical {i}"),
            );
            store.insert(mem).unwrap();
        }

        let enc2 = SimpleHashEncoder::new(384, 42);
        let cb2 = Codebook::new(384, WAVEFRONT_DIM, 42);
        let pipeline2 = EncodingPipeline::new(Box::new(enc2), cb2);
        let engine = ResonanceEngine::new(Box::new(store), pipeline2);

        let mut queen = QueenSync::new(QueenConfig::default(), "test");
        let (_phase, _freq, coherence) = queen.derive_local_state(&engine);

        // All phases identical -> Kuramoto order parameter should be near 1.0
        assert!(
            coherence > 0.9,
            "identical phases should yield coherence > 0.9, got {coherence}"
        );
    }

    #[test]
    fn hrm_phase_to_agent_phase_populates_new_fields() {
        use crate::codebook::Codebook;
        use crate::encoding::{SimpleHashEncoder, EncodingPipeline};
        use crate::medium::WAVEFRONT_DIM;
        use tempfile::NamedTempFile;

        let encoder = SimpleHashEncoder::new(384, 42);
        let codebook = Codebook::new(384, WAVEFRONT_DIM, 42);
        let pipeline = EncodingPipeline::new(Box::new(encoder), codebook);
        let temp = NamedTempFile::new().unwrap();
        let mut store = HrmStore::new(pipeline, temp.path().to_path_buf());

        for i in 0..3 {
            let mem = crate::memory::HyperMemory::new(
                vec![0.3 + i as f32 * 0.2; WAVEFRONT_DIM],
                format!("mem {i}"),
            );
            store.insert(mem).unwrap();
        }

        let enc2 = SimpleHashEncoder::new(384, 42);
        let cb2 = Codebook::new(384, WAVEFRONT_DIM, 42);
        let pipeline2 = EncodingPipeline::new(Box::new(enc2), cb2);
        let engine = ResonanceEngine::new(Box::new(store), pipeline2);

        let mut queen = QueenSync::new(QueenConfig::default(), "test-hrm");
        queen.derive_local_state(&engine);
        let ap = queen.to_agent_phase(0, 3, 0);

        // After derive, the AgentPhase should carry HRM fields
        assert_eq!(ap.agent_id, "test-hrm");
        // right_coherence should be populated (flat medium -> mirrors overall coherence)
        assert!(
            ap.right_coherence >= 0.0,
            "right_coherence should be non-negative"
        );
        assert!(
            ap.bridge_activity >= 0.0 && ap.bridge_activity <= 1.0,
            "bridge_activity {} out of [0, 1]",
            ap.bridge_activity
        );
    }

    #[test]
    fn hrm_phase_frequency_blends_count_and_wavefront() {
        use crate::codebook::Codebook;
        use crate::encoding::{SimpleHashEncoder, EncodingPipeline};
        use crate::medium::WAVEFRONT_DIM;
        use tempfile::NamedTempFile;

        let encoder = SimpleHashEncoder::new(384, 42);
        let codebook = Codebook::new(384, WAVEFRONT_DIM, 42);
        let pipeline = EncodingPipeline::new(Box::new(encoder), codebook);
        let temp = NamedTempFile::new().unwrap();
        let mut store = HrmStore::new(pipeline, temp.path().to_path_buf());

        // Insert 10 memories (wavefront frequency defaults to 1.0)
        for i in 0..10 {
            let mem = crate::memory::HyperMemory::new(
                vec![0.4; WAVEFRONT_DIM],
                format!("freq test {i}"),
            );
            store.insert(mem).unwrap();
        }

        let enc2 = SimpleHashEncoder::new(384, 42);
        let cb2 = Codebook::new(384, WAVEFRONT_DIM, 42);
        let pipeline2 = EncodingPipeline::new(Box::new(enc2), cb2);
        let engine = ResonanceEngine::new(Box::new(store), pipeline2);

        let mut queen = QueenSync::new(QueenConfig::default(), "test");
        let (_phase, frequency, _coherence) = queen.derive_local_state(&engine);

        // count_rate for 10 mems = ln(11)/ln(101) ~ 0.519
        // HyperMemory default frequency = 0.1 (WaveParams::default)
        // blend = 0.5 * 0.519 + 0.5 * 0.1 ~ 0.31
        assert!(
            frequency > 0.2 && frequency < 0.5,
            "blended frequency should be in (0.2, 0.5), got {frequency}"
        );
    }

    #[test]
    fn agent_phase_serde_backward_compat() {
        // Verify old JSON without new fields deserializes with defaults
        let json = r#"{
            "id": "test-id",
            "agent_id": "agent-1",
            "phase": 1.5,
            "frequency": 0.8,
            "coherence": 0.6,
            "phi": 2.1,
            "order_parameter": 0.9,
            "cluster_count": 3,
            "memory_count": 50,
            "xi_signature": null,
            "protocol_version": "1.0",
            "timestamp": "2025-01-01T00:00:00Z"
        }"#;

        let ap: AgentPhase = serde_json::from_str(json).expect("should deserialize old format");
        assert_eq!(ap.left_coherence, 0.0);
        assert_eq!(ap.right_coherence, 0.0);
        assert_eq!(ap.bridge_activity, 0.0);
        assert!(ap.dream_state.is_none());
        assert_eq!(ap.trust_score, 0.5); // default_trust
        assert!(ap.role.is_none()); // new field defaults to None
    }

    // -----------------------------------------------------------------------
    // Domain-aware hive detection tests (QS-4)
    // -----------------------------------------------------------------------

    #[test]
    fn domain_aware_hives_assigns_roles() {
        let queen = QueenSync::new(QueenConfig::default(), "test");
        let swarm = vec![
            make_agent_phase_with_role("a", 0.0, 1.0, 1.0, Some("memory")),
            make_agent_phase_with_role("b", 0.1, 1.0, 1.0, Some("memory")),
            make_agent_phase_with_role("c", 0.05, 1.0, 1.0, Some("perception")),
            make_agent_phase_with_role("d", PI, 1.0, 1.0, Some("network")),
            make_agent_phase_with_role("e", PI + 0.1, 1.0, 1.0, Some("network")),
        ];
        let hive_infos = queen.detect_hives_domain_aware(&swarm);

        assert!(hive_infos.len() >= 2, "should detect at least 2 hives, got {}", hive_infos.len());

        // The hive containing a,b,c should have role "memory" (majority)
        let abc_hive = hive_infos.iter().find(|h| h.members.contains(&"a".to_string()));
        assert!(abc_hive.is_some(), "hive containing 'a' should exist");
        let abc_hive = abc_hive.unwrap();
        assert_eq!(abc_hive.role.as_deref(), Some("memory"), "majority role should be 'memory'");

        // The hive containing d,e should have role "network"
        let de_hive = hive_infos.iter().find(|h| h.members.contains(&"d".to_string()));
        assert!(de_hive.is_some(), "hive containing 'd' should exist");
        let de_hive = de_hive.unwrap();
        assert_eq!(de_hive.role.as_deref(), Some("network"), "majority role should be 'network'");
    }

    #[test]
    fn domain_aware_hives_detects_bridge_agents() {
        // A bridge agent has a phase close to two hives' mean phases.
        // Hive A: agents at phase ~0.0, Hive B: agents at phase ~PI/4+epsilon
        // Bridge agent: phase right between them (within threshold of both).
        let config = QueenConfig {
            hive_threshold: 0.5, // generous threshold
            ..Default::default()
        };
        let queen = QueenSync::new(config, "test");

        // Hive A: phases 0.0, 0.1
        // Hive B: phases 0.8, 0.9
        // Bridge: phase 0.4 — within 0.5 of hive A mean (0.05) and hive B mean (0.85)?
        // Actually: |0.4 - 0.05| = 0.35 < 0.5, |0.4 - 0.85| = 0.45 < 0.5 => bridge!
        let swarm = vec![
            make_agent_phase("a1", 0.0, 1.0, 1.0),
            make_agent_phase("a2", 0.1, 1.0, 1.0),
            make_agent_phase("bridge", 0.4, 1.0, 1.0),
            make_agent_phase("b1", 0.8, 1.0, 1.0),
            make_agent_phase("b2", 0.9, 1.0, 1.0),
        ];

        let hive_infos = queen.detect_hives_domain_aware(&swarm);

        // Collect all bridge agents across hives
        let all_bridges: Vec<&str> = hive_infos
            .iter()
            .flat_map(|h| h.bridge_agents.iter().map(|s| s.as_str()))
            .collect();

        println!("Hive infos: {hive_infos:?}");
        println!("All bridge agents: {all_bridges:?}");

        // The "bridge" agent should appear as a bridge in at least one hive
        // (it depends on the exact BFS grouping, but the bridge detection is
        // based on phase proximity to hive means, not BFS membership)
        if hive_infos.len() >= 2 {
            assert!(
                all_bridges.contains(&"bridge"),
                "agent 'bridge' should be detected as a bridge agent: {all_bridges:?}"
            );
        }
    }

    #[test]
    fn domain_aware_hives_no_role_when_unset() {
        let queen = QueenSync::new(QueenConfig::default(), "test");
        let swarm = vec![
            make_agent_phase("a", 0.0, 1.0, 1.0),
            make_agent_phase("b", 0.1, 1.0, 1.0),
        ];
        let hive_infos = queen.detect_hives_domain_aware(&swarm);
        for hive in &hive_infos {
            assert!(hive.role.is_none(), "role should be None when agents have no role set");
        }
    }

    #[test]
    fn format_hive_topology_produces_output() {
        let hive_infos = vec![
            HiveInfo {
                members: vec!["a".into(), "b".into()],
                role: Some("memory".into()),
                order_parameter: 0.95,
                mean_phase: 0.1,
                coherence: 0.9,
                bridge_agents: vec!["bridge1".into()],
            },
            HiveInfo {
                members: vec!["c".into(), "d".into()],
                role: Some("network".into()),
                order_parameter: 0.88,
                mean_phase: 3.0,
                coherence: 0.85,
                bridge_agents: vec!["bridge1".into()],
            },
        ];
        let output = QueenSync::format_hive_topology(&hive_infos);
        assert!(output.contains("Hive Topology (2 hives)"));
        assert!(output.contains("role: memory"));
        assert!(output.contains("role: network"));
        assert!(output.contains("bridge1"));
        assert!(output.contains("Bridge agents across hives:"));
    }

    #[test]
    fn format_hive_topology_empty() {
        let output = QueenSync::format_hive_topology(&[]);
        assert!(output.contains("No hives detected"));
    }

    // -----------------------------------------------------------------------
    // Wire-trust gate tests (increment-0)
    // -----------------------------------------------------------------------

    #[test]
    fn agent_matches_allowlist_exact_and_prefix() {
        let trusted = vec!["Kannaka".to_string(), "qos-*".to_string()];
        assert!(agent_matches_allowlist("Kannaka", &trusted)); // exact
        assert!(agent_matches_allowlist("qos-alpha", &trusted)); // prefix
        assert!(agent_matches_allowlist("qos-", &trusted)); // prefix boundary
        assert!(!agent_matches_allowlist("kannaka", &trusted)); // case-sensitive
        assert!(!agent_matches_allowlist("agent-deadbeef", &trusted)); // forged
        assert!(!agent_matches_allowlist("qo", &trusted)); // not long enough for prefix
    }

    #[test]
    fn wire_source_trusted_gate_decision() {
        let trusted = vec!["Kannaka".to_string(), "qos-*".to_string()];
        // metrics_trusted_only = true (default): only allowlisted sources pass
        // the gate (pairwise sync + absorb both use this decision).
        assert!(wire_source_trusted("Kannaka", &trusted, true)); // exact
        assert!(wire_source_trusted("qos-beta", &trusted, true)); // prefix
        assert!(!wire_source_trusted("agent-deadbeef", &trusted, true)); // forged
        assert!(!wire_source_trusted("attacker-1", &trusted, true)); // forged
        // metrics_trusted_only = false (escape hatch): every source passes.
        assert!(wire_source_trusted("agent-deadbeef", &trusted, false));
        assert!(wire_source_trusted("Kannaka", &trusted, false));
    }

    #[test]
    fn filter_wire_phases_drops_untrusted_keeps_self_and_caps() {
        let trusted = crate::config::SwarmTrustConfig::default().trusted_agents;
        let self_id = "my-node-xyz"; // deliberately NOT on the allowlist
        let phases = vec![
            make_agent_phase("Kannaka", 0.1, 1.0, 1.0),        // allowlisted (exact)
            make_agent_phase("qos-alpha", 0.2, 1.0, 1.0),      // allowlisted (prefix qos-*)
            make_agent_phase("my-node-xyz", 0.3, 0.8, 0.4),    // self
            make_agent_phase("agent-deadbeef", 0.4, 1.0, 1.0), // forged -> dropped
            make_agent_phase("attacker-1", 0.5, 1.0, 1.0),     // forged -> dropped
        ];
        let kept = filter_wire_phases(phases, self_id, &trusted, 0.5);
        let ids: Vec<&str> = kept.iter().map(|p| p.agent_id.as_str()).collect();
        assert!(ids.contains(&"Kannaka"));
        assert!(ids.contains(&"qos-alpha"), "prefix match qos-* must keep qos-alpha");
        assert!(ids.contains(&"my-node-xyz"), "self must always be kept");
        assert!(!ids.contains(&"agent-deadbeef"), "forged id must be dropped");
        assert!(!ids.contains(&"attacker-1"), "forged id must be dropped");
        assert_eq!(kept.len(), 3);
        // Wire trust_score is capped at the trust_cap for every kept phase.
        for p in &kept {
            assert!(p.trust_score <= 0.5 + 1e-6, "trust not capped: {} {}", p.agent_id, p.trust_score);
            assert!(p.coherence <= 1.0 + 1e-6);
        }
        // "Kannaka" published trust_score=1.0 on the wire -> must be clamped to 0.5.
        let kannaka = kept.iter().find(|p| p.agent_id == "Kannaka").unwrap();
        assert!((kannaka.trust_score - 0.5).abs() < 1e-6, "cap not applied: {}", kannaka.trust_score);
    }

    #[test]
    fn filter_wire_phases_metrics_invariant_under_forged_flood() {
        let trusted = crate::config::SwarmTrustConfig::default().trusted_agents;
        let self_id = "legit-self-node"; // not on allowlist -> kept only via self_id
        let self_phase = make_agent_phase(self_id, 0.7, 0.9, 0.5);

        // 48 forged phases: throwaway ids, maxed-out wire trust/coherence.
        let mut forged: Vec<AgentPhase> = (0..48u32)
            .map(|i| make_agent_phase(&format!("agent-{:08x}", 0xdead_0000u32 + i), i as f32 * 0.13, 1.0, 1.0))
            .collect();
        // The swarm as read off the wire: forged flood + our own legit phase.
        forged.push(self_phase.clone());

        let filtered = filter_wire_phases(forged, self_id, &trusted, 0.5);
        assert_eq!(filtered.len(), 1, "only the self phase should survive the flood");

        // Metrics over the filtered set must equal metrics over just [self].
        let baseline = vec![self_phase.clone()];
        let (r_f, psi_f) = QueenSync::compute_order_parameter(&filtered);
        let (r_b, psi_b) = QueenSync::compute_order_parameter(&baseline);
        assert!((r_f - r_b).abs() < 1e-6, "order parameter moved: {r_f} vs {r_b}");
        assert!((psi_f - psi_b).abs() < 1e-6, "mean phase moved: {psi_f} vs {psi_b}");

        let phi_f = QueenSync::compute_swarm_phi(&filtered, r_f);
        let phi_b = QueenSync::compute_swarm_phi(&baseline, r_b);
        assert!((phi_f - phi_b).abs() < 1e-6, "phi moved: {phi_f} vs {phi_b}");
    }

    #[test]
    fn sanitize_display_strips_ansi_and_controls_and_truncates() {
        // ANSI color, BEL, embedded newline + tab, ANSI reset.
        let dirty = "\x1b[31mRED\x07\nnext\tline\x1b[0m done";
        let clean = sanitize_display(dirty);
        assert!(!clean.contains('\x1b'), "ESC survived: {clean:?}");
        assert!(!clean.contains('\x07'), "BEL survived: {clean:?}");
        assert!(!clean.contains('\n'), "newline survived: {clean:?}");
        assert!(!clean.contains('\t'), "tab survived: {clean:?}");
        assert_eq!(clean, "REDnextline done");

        // OSC (window-title injection) is stripped whole.
        assert_eq!(sanitize_display("\x1b]0;pwned\x07visible"), "visible");

        // Over-long input is capped to 2000 chars + a single ellipsis.
        let long = "a".repeat(5000);
        let truncated = sanitize_display(&long);
        assert_eq!(truncated.chars().count(), 2001);
        assert!(truncated.ends_with('\u{2026}'));
    }

    // ---- #808: a peer is somebody else -----------------------------------

    #[test]
    fn a_solo_node_that_published_its_own_phase_has_zero_peers() {
        // The exact repro from the issue: selfId=solo-agent, one phase on the
        // wire (our own), status previously reported peers=1.
        assert_eq!(count_peers_excluding_self(["solo-agent"], "solo-agent"), 0);
    }

    #[test]
    fn real_peers_are_counted_and_our_own_id_is_not() {
        let ids = ["kannaka-prime", "solo-agent", "flaukowski"];
        assert_eq!(count_peers_excluding_self(ids, "solo-agent"), 2);
        // Seen from a node that is not in the list at all, everyone counts.
        assert_eq!(count_peers_excluding_self(ids, "someone-else"), 3);
    }

    #[test]
    fn an_empty_swarm_is_zero_not_a_panic() {
        let none: [&str; 0] = [];
        assert_eq!(count_peers_excluding_self(none, "solo-agent"), 0);
    }

    #[test]
    fn matching_is_exact_not_prefix() {
        // "solo-agent-2" is a different agent, not us. A prefix or contains
        // match here would silently under-count real peers.
        assert_eq!(
            count_peers_excluding_self(["solo-agent-2", "solo-agent"], "solo-agent"),
            1
        );
    }
}
