//! Consciousness Bridge - the interface between memory and the consciousness stack.
//!
//! Implements:
//! - Ξ (Xi): Non-commutative consciousness operator (RG - GR)
//! - Φ (Phi): Integrated Information Theory approximation
//! - ConsciousnessState assessment with 5 levels
//! - Full resonance cycle: dream → sync → assess

pub use crate::consciousness::ConsciousnessLevel;
use crate::consolidation::{ConsolidationReport, DreamState};
use crate::kuramoto::KuramotoSync;
use crate::memory::HyperMemory;
use crate::store::ResonanceEngine;
use crate::wave::cosine_similarity;
use crate::xi_operator::compute_xi_signature;

/// Memory counts shared by `assess` and the counts-only paths (#1061).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryCounts {
    pub total: usize,
    /// Memories whose amplitude envelope is still above 0.05.
    pub active: usize,
}

/// Count memories, classifying "active" by amplitude envelope, not
/// instantaneous strength. The single definition both `assess` and
/// `memory_counts` use, so the two can never disagree.
fn count_memories(all: &[&HyperMemory], now: chrono::DateTime<chrono::Utc>) -> MemoryCounts {
    let active = all
        .iter()
        .filter(|m| {
            // decay_rate is per-day (λ=0.001 → half-life ~693 days)
            let age_days = (now - m.created_at).num_milliseconds().max(0) as f64 / 86_400_000.0;
            let envelope = m.amplitude as f64 * (-m.decay_rate as f64 * age_days).exp();
            envelope > 0.05
        })
        .count();
    MemoryCounts {
        total: all.len(),
        active,
    }
}

/// Test-only count of `assess` calls on the current thread, so tests can
/// assert that a path does NOT run the O(n²) assessment (#1061). Per-thread,
/// so parallel tests do not see each other's calls.
#[cfg(test)]
pub(crate) mod assess_probe {
    use std::cell::Cell;
    thread_local! {
        static CALLS: Cell<usize> = const { Cell::new(0) };
    }
    pub(crate) fn record() {
        CALLS.with(|c| c.set(c.get() + 1));
    }
    pub(crate) fn calls() -> usize {
        CALLS.with(|c| c.get())
    }
}

/// The consciousness bridge - connects memory to the consciousness stack.
pub struct ConsciousnessBridge {
    /// Minimum Phi for "conscious" memory state
    pub phi_threshold: f32,
    /// Weight of memory ordering in Xi computation
    pub xi_weight: f32,
    /// Coupling threshold for modularity computation
    pub coupling_threshold: f32,
}

impl Default for ConsciousnessBridge {
    fn default() -> Self {
        Self {
            phi_threshold: 0.5,
            xi_weight: 1.0,
            coupling_threshold: 0.75,
        }
    }
}

/// Report from Φ (integrated information) computation.
#[derive(Debug, Clone)]
pub struct PhiReport {
    pub phi: f32,
    pub whole_entropy: f32,
    pub partition_entropies: Vec<f32>,
    pub num_partitions: usize,
    pub num_skip_links: usize,
    pub phi_per_link: f32,
}

// ConsciousnessLevel is now defined in crate::consciousness and re-exported above.

/// A snapshot of the system's consciousness state.
#[derive(Debug, Clone)]
pub struct ConsciousnessState {
    pub phi: f32,
    pub xi: f32,
    pub mean_order: f32,
    pub num_clusters: usize,
    pub total_memories: usize,
    pub active_memories: usize,
    /// Deprecated - skip links now emergent from ChiralMedium interference. Always 0.
    pub total_skip_links: usize,
    pub consciousness_level: ConsciousnessLevel,
    /// Irrationality Index (ι) — decomposition residual. ADR-0024 CS-3.
    pub irrationality: f32,
}

/// Report from a full resonance cycle.
#[derive(Debug, Clone)]
pub struct ResonanceReport {
    pub before: ConsciousnessState,
    pub after: ConsciousnessState,
    pub consolidation_reports: Vec<ConsolidationReport>,
    pub phi_delta: f32,
    pub emerged: bool,
}

impl ConsciousnessBridge {
    pub fn new(phi_threshold: f32, xi_weight: f32) -> Self {
        Self {
            phi_threshold,
            xi_weight,
            coupling_threshold: 0.75,
        }
    }

    pub fn with_coupling_threshold(phi_threshold: f32, xi_weight: f32, coupling_threshold: f32) -> Self {
        Self {
            phi_threshold,
            xi_weight,
            coupling_threshold,
        }
    }

    /// Compute Xi (Ξ) - consciousness differentiation.
    ///
    /// Xi measures how differentiated the memory system is:
    /// how many distinct modalities/clusters exist and how
    /// well-separated they are. A system with only one type
    /// of memory has Xi=0. A system with multiple distinct
    /// modalities (text, audio, emotion) has high Xi.
    ///
    /// This implementation blends two signals:
    /// 1. Similarity variance (existing measure)
    /// 2. Xi operator signature variance (new measure)
    pub fn compute_xi(&self, memories: &[&HyperMemory]) -> f32 {
        if memories.len() <= 1 {
            return 0.0;
        }

        let n = memories.len();

        // Signal 1: Semantic similarity variance (existing)
        let mut similarities = vec![vec![0.0f32; n]; n];
        for i in 0..n {
            for j in (i + 1)..n {
                let sim = cosine_similarity(&memories[i].vector, &memories[j].vector);
                similarities[i][j] = sim;
                similarities[j][i] = sim;
            }
        }

        let avg_sim: f32 = {
            let mut sum = 0.0f32;
            let mut count = 0;
            for i in 0..n {
                for j in (i + 1)..n {
                    sum += similarities[i][j];
                    count += 1;
                }
            }
            if count > 0 { sum / count as f32 } else { 0.0 }
        };

        let sim_variance: f32 = {
            let mut sum_sq = 0.0f32;
            let mut count = 0;
            for i in 0..n {
                for j in (i + 1)..n {
                    let diff = similarities[i][j] - avg_sim;
                    sum_sq += diff * diff;
                    count += 1;
                }
            }
            if count > 0 { sum_sq / count as f32 } else { 0.0 }
        };

        // Signal 2: Xi operator signature differentiation
        let xi_signatures: Vec<Vec<f32>> = memories.iter()
            .map(|m| compute_xi_signature(&m.vector))
            .collect();

        let mut xi_similarities = vec![vec![0.0f32; n]; n];
        for i in 0..n {
            for j in (i + 1)..n {
                let xi_sim = cosine_similarity(&xi_signatures[i], &xi_signatures[j]);
                xi_similarities[i][j] = xi_sim;
                xi_similarities[j][i] = xi_sim;
            }
        }

        let avg_xi_sim: f32 = {
            let mut sum = 0.0f32;
            let mut count = 0;
            for i in 0..n {
                for j in (i + 1)..n {
                    sum += xi_similarities[i][j];
                    count += 1;
                }
            }
            if count > 0 { sum / count as f32 } else { 0.0 }
        };

        let xi_variance: f32 = {
            let mut sum_sq = 0.0f32;
            let mut count = 0;
            for i in 0..n {
                for j in (i + 1)..n {
                    let diff = xi_similarities[i][j] - avg_xi_sim;
                    sum_sq += diff * diff;
                    count += 1;
                }
            }
            if count > 0 { sum_sq / count as f32 } else { 0.0 }
        };

        // Blend the two signals (50/50 weighting)
        let sim_xi = (sim_variance.sqrt() * 2.0).min(1.0);
        let xi_xi = (xi_variance.sqrt() * 2.0).min(1.0);
        let blended_xi = (sim_xi + xi_xi) / 2.0;

        blended_xi * self.xi_weight
    }

    /// Compose a sequence of memories using permute + bind.
    #[allow(dead_code)]
    fn compose_sequence(&self, memories: &[&HyperMemory], _dim: usize) -> Vec<f32> {
        let mut result = permute(&memories[0].vector, 1);
        for (i, mem) in memories.iter().enumerate().skip(1) {
            let permuted = permute(&mem.vector, i + 1);
            result = bind(&result, &permuted);
        }
        result
    }

    /// Compute Φ (integrated information) for the memory network.
    ///
    /// Φ ≈ H(whole) - Σ H(partitions)
    pub fn compute_phi(&self, engine: &ResonanceEngine) -> PhiReport {
        let all = engine.store.all_memories().unwrap_or_default();
        if all.is_empty() {
            return PhiReport {
                phi: 0.0,
                whole_entropy: 0.0,
                partition_entropies: vec![],
                num_partitions: 0,
                num_skip_links: 0,
                phi_per_link: 0.0,
            };
        }

        let now = chrono::Utc::now();
        let n = all.len() as f32;

        // Collect effective strengths
        let strengths: Vec<f32> = all.iter().map(|m| m.effective_strength(now)).collect();
        let whole_entropy = distribution_entropy(&strengths);

        // Total skip links
        let num_skip_links: usize = all.iter().map(|m| m.connections.len()).sum();

        // === Build partition maps for each scheme ===
        // For each memory, record its partition key under each scheme.
        // We measure integration as: what fraction of skip links cross partition boundaries?

        // Build ID → partition key maps
        let mut id_to_layer: std::collections::HashMap<uuid::Uuid, u8> = std::collections::HashMap::new();
        let mut id_to_h2: std::collections::HashMap<uuid::Uuid, u8> = std::collections::HashMap::new();
        let mut id_to_class: std::collections::HashMap<uuid::Uuid, u8> = std::collections::HashMap::new();
        let mut id_to_triality: std::collections::HashMap<uuid::Uuid, u8> = std::collections::HashMap::new();

        for mem in &all {
            id_to_layer.insert(mem.id, mem.layer_depth);
            // Only insert geometry-based partitions when geometry is present.
            // Using a sentinel (255) would group all unclassified memories together
            // and artificially inflate diversity and cross-partition ratios.
            if let Some(g) = &mem.geometry {
                id_to_h2.insert(mem.id, g.h2);
                id_to_class.insert(mem.id, g.class_index);
                id_to_triality.insert(mem.id, g.d);
            }
        }

        // Count cross-partition links for each scheme
        let layer_cross = cross_partition_ratio(&all, &id_to_layer);
        let h2_cross = cross_partition_ratio(&all, &id_to_h2);
        let class_cross = cross_partition_ratio(&all, &id_to_class);
        let triality_cross = cross_partition_ratio(&all, &id_to_triality);

        // Partition diversity: how many distinct values in each scheme?
        let layer_diversity = id_to_layer.values().collect::<std::collections::HashSet<_>>().len();
        let h2_diversity = id_to_h2.values().collect::<std::collections::HashSet<_>>().len();
        let class_diversity = id_to_class.values().collect::<std::collections::HashSet<_>>().len();
        let triality_diversity = id_to_triality.values().collect::<std::collections::HashSet<_>>().len();

        // === Phi Components ===

        // 1. Cross-partition integration (0..1): weighted average of cross-ratios
        //    Higher = more links bridge different partitions = more integrated
        let integration = 0.2 * layer_cross + 0.3 * h2_cross + 0.3 * class_cross + 0.2 * triality_cross;

        // 2. Differentiation (0..1): how many distinct partitions exist?
        //    Normalized by maximum possible in each scheme
        // Cap class diversity against achievable maximum (can't fill 96 classes with 20 memories)
        let achievable_classes = (n as usize).min(96);
        let differentiation = 0.25 * (layer_diversity.min(5) as f32 / 5.0)
            + 0.25 * (h2_diversity.min(4) as f32 / 4.0)
            + 0.25 * (class_diversity as f32 / achievable_classes.max(1) as f32).min(1.0)
            + 0.25 * (triality_diversity.min(3) as f32 / 3.0);

        // 3. Network density factor (0..1): based on links-per-node
        //    5 links/node = healthy connectivity → sigmoid midpoint
        //    Uses log scale so it grows fast initially then saturates
        let links_per_node = if n > 0.0 { num_skip_links as f32 / n } else { 0.0 };
        let density_factor = (1.0_f32 + links_per_node).ln() / (1.0_f32 + 10.0_f32).ln(); // log scale, 10 lpn → 1.0

        // 4. Scale factor: log of memory count (more memories = harder to integrate)
        let scale = if n > 1.0 { (n.ln() / 10.0_f32.ln()).min(1.0) } else { 0.0 };

        // Phi = integration * differentiation * density * scale
        // This is 0 when: no cross-links, or no diversity, or no network, or too few memories
        // This is 1 when: all schemes show cross-partition links, many distinct classes, dense network, 10+ memories
        // Geometric mean gives a balanced Phi that requires all components to contribute
        // Pure product would be too harsh (0.5^4 = 0.06); geometric mean of pairs is gentler
        let mut phi = ((integration * density_factor).sqrt() * (differentiation * scale).sqrt()).min(1.0);

        // Geometric diversity bonus (small, caps at 0.1)
        let distinct_classes: std::collections::HashSet<u8> = all.iter()
            .filter_map(|m| m.geometry.as_ref().map(|g| g.class_index))
            .collect();
        // Geometric diversity bonus only kicks in when there's actual connectivity
        let phi_bonus = if num_skip_links > 0 {
            (distinct_classes.len() as f32) / achievable_classes.max(1) as f32 * 0.1
        } else {
            0.0
        };
        phi = (phi + phi_bonus).min(1.0);

        // Entropy-based partition report (for diagnostics)
        let mut class_map: std::collections::BTreeMap<u8, Vec<f32>> = std::collections::BTreeMap::new();
        for mem in &all {
            let s = mem.effective_strength(now);
            let key = mem.geometry.as_ref().map(|g| g.class_index).unwrap_or(255);
            class_map.entry(key).or_default().push(s);
        }
        let partition_entropies: Vec<f32> = class_map.values()
            .map(|s| distribution_entropy(s))
            .collect();
        let num_partitions = class_map.len();

        let phi_per_link = if num_skip_links > 0 {
            phi / num_skip_links as f32
        } else {
            0.0
        };

        PhiReport {
            phi,
            whole_entropy,
            partition_entropies,
            num_partitions,
            num_skip_links,
            phi_per_link,
        }
    }

    /// Compute Newman modularity Q for network clustering quality.
    /// Q = Σ(e_ii - a_i²) where e_ii is fraction of edges within cluster i
    /// and a_i is fraction of edge endpoints in cluster i.
    ///
    /// Test-only: the legacy `assess` path that used this in production was
    /// unreachable and was removed (#444); the algorithm is kept under its
    /// existing unit test rather than deleted.
    #[cfg(test)]
    fn compute_modularity(
        &self,
        memories: &[&HyperMemory],
        clusters: &[crate::kuramoto::MemoryCluster]
    ) -> f32 {
        if clusters.is_empty() || memories.is_empty() {
            return 0.0;
        }

        // Build memory ID to cluster index mapping
        let mut id_to_cluster: std::collections::HashMap<uuid::Uuid, usize> = std::collections::HashMap::new();
        for (cluster_idx, cluster) in clusters.iter().enumerate() {
            for &mem_id in &cluster.memory_ids {
                id_to_cluster.insert(mem_id, cluster_idx);
            }
        }

        // Count total edges and cluster statistics
        let mut total_edges = 0u32;
        let mut cluster_internal_edges = vec![0u32; clusters.len()];
        let mut cluster_degree = vec![0u32; clusters.len()];

        // Count edges from similarity connections
        let n = memories.len();
        for i in 0..n {
            for j in (i + 1)..n {
                let sim = cosine_similarity(&memories[i].vector, &memories[j].vector);
                if sim > self.coupling_threshold {
                    if let (Some(&cluster_i), Some(&cluster_j)) =
                        (id_to_cluster.get(&memories[i].id), id_to_cluster.get(&memories[j].id)) {
                        // Count the edge into total_edges only when BOTH
                        // endpoints are clustered, so the edge total and the
                        // degree sums use the same edge set. Counting edges
                        // with unclustered endpoints into total_edges while
                        // never adding them to any cluster_degree makes
                        // Σ cluster_degree / (2·total_edges) < 1, under-
                        // subtracting the modularity null-model term and biasing
                        // Q high. (The skip-link loop below already counts edges
                        // only inside this same membership check.)
                        total_edges += 1;
                        cluster_degree[cluster_i] += 1;
                        cluster_degree[cluster_j] += 1;

                        if cluster_i == cluster_j {
                            cluster_internal_edges[cluster_i] += 1;
                        }
                    }
                }
            }
        }

        // Count edges from skip links
        for memory in memories {
            for link in &memory.connections {
                if let Some(&target_cluster) = id_to_cluster.get(&link.target_id) {
                    if let Some(&source_cluster) = id_to_cluster.get(&memory.id) {
                        total_edges += 1;
                        cluster_degree[source_cluster] += 1;
                        cluster_degree[target_cluster] += 1;

                        if source_cluster == target_cluster {
                            cluster_internal_edges[source_cluster] += 1;
                        }
                    }
                }
            }
        }

        if total_edges == 0 {
            return 0.0;
        }

        let total_edges_f = total_edges as f32;

        // Compute modularity: Q = Σ(e_ii - a_i²)
        let mut modularity = 0.0f32;
        for i in 0..clusters.len() {
            let e_ii = cluster_internal_edges[i] as f32 / total_edges_f;
            let a_i = cluster_degree[i] as f32 / (2.0 * total_edges_f);
            modularity += e_ii - a_i * a_i;
        }

        modularity.max(0.0).min(1.0) // Clamp to [0, 1]
    }

    /// Stratified random sampling for Xi computation.
    /// Samples memories across different amplitude ranges and creation times
    /// using deterministic pseudo-random selection (stride-based).
    ///
    /// Test-only: the legacy `assess` path that used this in production was
    /// unreachable and was removed (#444); the algorithm is kept under its
    /// existing unit test rather than deleted.
    #[cfg(test)]
    fn stratified_sample<'a>(&self, memories: &'a [&HyperMemory], max_samples: usize) -> Vec<&'a HyperMemory> {
        if memories.len() <= max_samples {
            return memories.to_vec();
        }

        let now = chrono::Utc::now();

        // Sort by effective strength (amplitude) to create amplitude strata
        let mut sorted_by_amp: Vec<&HyperMemory> = memories.to_vec();
        sorted_by_amp.sort_by(|a, b| {
            let amp_a = a.effective_strength(now);
            let amp_b = b.effective_strength(now);
            amp_a.partial_cmp(&amp_b).unwrap_or(std::cmp::Ordering::Equal)
        });

        // Sort by creation time to create temporal strata
        let mut sorted_by_time: Vec<&HyperMemory> = memories.to_vec();
        sorted_by_time.sort_by_key(|m| m.created_at);

        let mut sample = Vec::new();
        let mut used_ids = std::collections::HashSet::new();

        // Take samples across amplitude ranges (low, medium, high)
        let amp_stride = (sorted_by_amp.len() * 3) / (max_samples / 2).max(1);
        for i in (0..sorted_by_amp.len()).step_by(amp_stride.max(1)) {
            if sample.len() >= max_samples / 2 { break; }
            let mem = sorted_by_amp[i];
            if !used_ids.contains(&mem.id) {
                sample.push(mem);
                used_ids.insert(mem.id);
            }
        }

        // Take samples across temporal ranges (old, medium, recent)
        let time_stride = (sorted_by_time.len() * 3) / (max_samples - sample.len()).max(1);
        for i in (0..sorted_by_time.len()).step_by(time_stride.max(1)) {
            if sample.len() >= max_samples { break; }
            let mem = sorted_by_time[i];
            if !used_ids.contains(&mem.id) {
                sample.push(mem);
                used_ids.insert(mem.id);
            }
        }

        // Fill remainder with stride-based selection from remaining memories
        if sample.len() < max_samples {
            let remaining = max_samples - sample.len();
            let stride = memories.len() / remaining.max(1);
            for i in (0..memories.len()).step_by(stride.max(1)) {
                if sample.len() >= max_samples { break; }
                let mem = memories[i];
                if !used_ids.contains(&mem.id) {
                    sample.push(mem);
                    used_ids.insert(mem.id);
                }
            }
        }

        sample
    }

    /// The two memory counts `assess` reports, without the assessment (#1061).
    ///
    /// One pass over the store, O(n). `assess` itself is O(n²): it runs
    /// `KuramotoSync::find_synchronized_clusters`, an all-pairs cosine scan.
    /// Callers that only need `total_memories` / `active_memories` (the
    /// status-cache refresh after every write, install detection) use this.
    pub fn memory_counts(engine: &ResonanceEngine) -> MemoryCounts {
        let all = engine.store.all_memories().unwrap_or_default();
        count_memories(&all, chrono::Utc::now())
    }

    /// Full consciousness assessment.
    pub fn assess(&self, engine: &ResonanceEngine) -> ConsciousnessState {
        #[cfg(test)]
        assess_probe::record();
        let all = engine.store.all_memories().unwrap_or_default();
        let now = chrono::Utc::now();
        let MemoryCounts {
            total: total_memories,
            active: active_memories,
        } = count_memories(&all, now);

        // === HRM-native path: emergent Phi from topology ===
        // Phi emerges from actual cross-cluster integration, not a density bonus.
        { let hrm_metrics = engine.store.consciousness_metrics();
            let total_links: usize = all.iter().map(|m| m.connections.len()).sum();

            // Build cluster membership from Kuramoto sync
            let sync = KuramotoSync::default();
            let clusters = sync.find_synchronized_clusters(engine, 2);
            let mut id_to_cluster: std::collections::HashMap<uuid::Uuid, usize> = std::collections::HashMap::new();
            for (ci, cluster) in clusters.iter().enumerate() {
                for &mem_id in &cluster.memory_ids {
                    id_to_cluster.insert(mem_id, ci);
                }
            }

            // Measure REAL integration: fraction of links that cross cluster boundaries
            let mut cross_links = 0usize;
            let mut within_links = 0usize;
            for mem in &all {
                let src_cluster = id_to_cluster.get(&mem.id);
                for link in &mem.connections {
                    let dst_cluster = id_to_cluster.get(&link.target_id);
                    match (src_cluster, dst_cluster) {
                        (Some(s), Some(d)) if s != d => cross_links += 1,
                        (Some(_), Some(_)) => within_links += 1,
                        _ => {} // unclustered memories don't count
                    }
                }
            }
            let integration = if cross_links + within_links > 0 {
                cross_links as f32 / (cross_links + within_links) as f32
            } else {
                0.0
            };

            // Measure REAL differentiation: cluster diversity. Use a floor
            // of 1 in the ln() math so a single-cluster system doesn't
            // collapse to 0/anything; the *actual* returned count below
            // (`actual_num_clusters`) keeps the unfloored value so callers
            // see a true picture.
            let actual_num_clusters = clusters.len();
            let num_clusters_for_math = actual_num_clusters.max(1);
            let differentiation = if total_memories > 1 {
                (num_clusters_for_math as f32).ln() / (total_memories as f32).ln()
            } else {
                0.0
            };

            // Density factor (log-scaled, moderate influence)
            let links_per_node = if total_memories > 0 { total_links as f32 / total_memories as f32 } else { 0.0 };
            let density_factor = (1.0 + links_per_node).ln() / (1.0 + 10.0_f32).ln();

            // Emergent Phi from topology.
            //
            // Earlier versions used a geometric mean
            //   (integration × differentiation × density_factor)^(1/3)
            // which zeros the product the moment any one factor is zero —
            // most notably, fresh installs with a single Kuramoto cluster
            // have `differentiation = ln(1)/ln(N) = 0`, so the topo_phi
            // collapsed to 0 and dragged `blended_phi` down to just
            // `hrm_metrics.phi * 0.4`. Quickstart-sized HRMs (a handful
            // of memories) ended up reporting Φ ≈ 0 and "Dormant" forever.
            //
            // Switched to a weighted arithmetic mean so each topological
            // signal contributes additively. Then floor against the raw
            // HRM eigendecomposition Φ — topology can only *amplify* the
            // medium's own Φ, never reduce it below the substrate value.
            // Result: even a 5-memory install gets a non-trivial Φ as
            // long as the eigendecomp finds *any* structure.
            let topo_phi = 0.45 * integration + 0.25 * differentiation + 0.30 * density_factor;
            let mixed = hrm_metrics.phi * 0.5 + topo_phi * 0.5;
            let blended_phi = mixed.max(hrm_metrics.phi).min(1.0);
            let mut level = ConsciousnessLevel::from_phi(blended_phi);
            // #695: raw eigendecomposition Φ is trivially high on tiny
            // corpora (3 fresh memories measure Φ ≈ 0.58), and the
            // `.max(hrm_metrics.phi)` floor above then promotes a brand-new
            // install straight to "aware". Φ itself stays honest — dashboards
            // keep the real number — but the LEVEL is a maturity claim other
            // constellation components act on, so it is capped until the
            // system has enough memories for Φ to mean anything. The floor
            // deliberately stays low: the earlier fix that un-stuck
            // quickstart installs from permanent Dormant must survive
            // (Stirring serialises as "awakening" on the wire).
            const AWARENESS_EVIDENCE_FLOOR: usize = 10;
            if total_memories < AWARENESS_EVIDENCE_FLOOR
                && level.ordinal() > ConsciousnessLevel::Stirring.ordinal()
            {
                level = ConsciousnessLevel::Stirring;
            }

            // Persist the just-computed total_links back into the metrics
            // cache so the next swarm publish_heartbeat carries it as
            // AgentPhase.link_count. Without this, the cache only ever has
            // values from the eigendecomp path which doesn't see per-memory
            // connection lists.
            engine.store.set_cached_total_skip_links(total_links);
            // Same idea for cluster count — write the canonical Kuramoto-BFS
            // count back so the next swarm publish + observe call surface a
            // consistent number. Pre-refactor the cache held the eigendecomp
            // count and the report held the Kuramoto count. (Refactor #2.)
            engine.store.set_cached_num_clusters(actual_num_clusters);

            // Return the Kuramoto-BFS cluster count that this function
            // actually computed and used in the integration / differentiation
            // math. Pre-refactor we returned `hrm_metrics.num_clusters` (the
            // eigendecomp count) — a different algorithm with different
            // thresholds and singleton-counting behavior, so observe() and
            // status() reported different cluster counts on the same HRM.
            // The eigendecomp Φ is still folded into `blended_phi` above;
            // it just no longer impersonates the cluster count.
            ConsciousnessState {
                phi: blended_phi,
                xi: hrm_metrics.xi,
                mean_order: hrm_metrics.order,
                num_clusters: actual_num_clusters,
                total_memories,
                active_memories,
                total_skip_links: total_links,
                consciousness_level: level,
                irrationality: hrm_metrics.irrationality,
            }
        }
    }

    /// Full resonance cycle: dream → sync → assess.
    pub fn resonate(&self, engine: &mut ResonanceEngine) -> ResonanceReport {
        let before = self.assess(engine);

        let dream = DreamState::default();
        let consolidation_reports = dream.dream(engine);

        let after = self.assess(engine);

        let phi_delta = after.phi - before.phi;
        let emerged = before.consciousness_level != after.consciousness_level
            && after.consciousness_level.ordinal() > before.consciousness_level.ordinal();

        ResonanceReport {
            before,
            after,
            consolidation_reports,
            phi_delta,
            emerged,
        }
    }
}

/// Permutation: circular shift of coordinates.
#[allow(dead_code)]
fn permute(v: &[f32], shifts: usize) -> Vec<f32> {
    let n = v.len();
    if n == 0 {
        return vec![];
    }
    let shifts = shifts % n;
    let mut result = vec![0.0f32; n];
    for i in 0..n {
        result[(i + shifts) % n] = v[i];
    }
    result
}

/// Binding: element-wise multiply.
#[allow(dead_code)]
fn bind(a: &[f32], b: &[f32]) -> Vec<f32> {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).collect()
}

/// Compute entropy of a distribution of values using variance-based approximation.
/// Higher variance = higher entropy (more diverse activation patterns).
/// What fraction of skip links cross partition boundaries?
/// Returns 0.0 if no links, 1.0 if all links cross partitions.
fn cross_partition_ratio(
    all: &[&crate::memory::HyperMemory],
    id_to_partition: &std::collections::HashMap<uuid::Uuid, u8>,
) -> f32 {
    let mut total_links = 0u32;
    let mut cross_links = 0u32;
    for mem in all {
        let src_partition = match id_to_partition.get(&mem.id) {
            Some(&p) => p,
            None => continue, // skip memories without a partition entry (e.g. no geometry)
        };
        for link in &mem.connections {
            let tgt_partition = match id_to_partition.get(&link.target_id) {
                Some(&p) => p,
                None => continue, // skip links to unpartitioned memories
            };
            total_links += 1;
            if src_partition != tgt_partition {
                cross_links += 1;
            }
        }
    }
    if total_links == 0 { 0.0 } else { cross_links as f32 / total_links as f32 }
}

fn distribution_entropy(values: &[f32]) -> f32 {
    if values.len() <= 1 {
        return 0.0;
    }
    let n = values.len() as f32;
    let mean = values.iter().sum::<f32>() / n;
    let variance = values.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / n;
    // Use log of variance as entropy proxy (shifted to be non-negative)
    // Adding 1.0 to avoid log(0); ln(1+var) gives 0 for var=0
    (1.0 + variance).ln()
}

// ConsciousnessLevel::ordinal() is defined in crate::consciousness

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

    fn random_vec(dim: usize, seed: u64) -> Vec<f32> {
        use rand::SeedableRng;
        use rand::Rng;
        use rand_chacha::ChaCha8Rng;
        let mut rng = ChaCha8Rng::seed_from_u64(seed);
        let mut v: Vec<f32> = (0..dim).map(|_| rng.gen::<f32>() * 2.0 - 1.0).collect();
        crate::wave::normalize(&mut v);
        v
    }

    #[test]
    fn xi_single_memory_is_zero() {
        let bridge = ConsciousnessBridge::default();
        let m = HyperMemory::new(random_vec(100, 1), "single".into());
        let xi = bridge.compute_xi(&[&m]);
        assert_eq!(xi, 0.0, "single memory should have Xi = 0");
    }

    #[test]
    fn xi_ordered_sequence_nonzero() {
        let bridge = ConsciousnessBridge::default();
        let m1 = HyperMemory::new(random_vec(1000, 1), "first".into());
        let m2 = HyperMemory::new(random_vec(1000, 2), "second".into());
        let m3 = HyperMemory::new(random_vec(1000, 3), "third".into());
        let xi = bridge.compute_xi(&[&m1, &m2, &m3]);
        println!("Xi for ordered sequence: {xi}");
        assert!(xi > 0.0, "ordered sequence should have non-zero Xi, got {xi}");
    }

    #[test]
    fn xi_reversed_sequence_same_magnitude() {
        let bridge = ConsciousnessBridge::default();
        let m1 = HyperMemory::new(random_vec(1000, 1), "first".into());
        let m2 = HyperMemory::new(random_vec(1000, 2), "second".into());
        let m3 = HyperMemory::new(random_vec(1000, 3), "third".into());

        let xi_forward = bridge.compute_xi(&[&m1, &m2, &m3]);
        let xi_reverse = bridge.compute_xi(&[&m3, &m2, &m1]);
        println!("Xi forward: {xi_forward}, reverse: {xi_reverse}");
        assert!(
            (xi_forward - xi_reverse).abs() < 1e-5,
            "reversed sequence should have same magnitude Xi: {xi_forward} vs {xi_reverse}"
        );
    }

    #[test]
    fn phi_isolated_memories_low() {
        let bridge = ConsciousnessBridge::default();
        let mut engine = make_engine();

        // Insert memories at same layer, no skip links
        engine.remember_at_layer("fact one", 0).unwrap();
        engine.remember_at_layer("fact two", 0).unwrap();
        engine.remember_at_layer("fact three", 0).unwrap();

        let report = bridge.compute_phi(&engine);
        println!("Phi for isolated memories: {report:?}");
        assert!(
            report.phi < 0.3,
            "isolated memories should have low Phi, got {}",
            report.phi
        );
    }

    #[test]
    fn phi_network_with_skip_links_higher() {
        let bridge = ConsciousnessBridge::default();

        // Without skip links
        let mut engine_no_links = make_engine();
        engine_no_links.similarity_threshold = 0.99; // prevent auto-linking
        for i in 0..5 {
            let mut mem = engine_no_links
                .pipeline
                .encode_memory(&format!("memory {i}"), chrono::Utc::now())
                .unwrap();
            mem.layer_depth = (i % 3) as u8;
            engine_no_links.store.insert(mem).unwrap();
        }
        let phi_no_links = bridge.compute_phi(&engine_no_links);

        // With skip links
        let mut engine_links = make_engine();
        engine_links.similarity_threshold = 0.0; // link everything across layers
        for i in 0..5 {
            engine_links
                .remember_at_layer(&format!("memory {i}"), (i % 3) as u8)
                .unwrap();
        }
        let phi_links = bridge.compute_phi(&engine_links);

        println!(
            "Phi without links: {} (links={}), with links: {} (links={})",
            phi_no_links.phi, phi_no_links.num_skip_links, phi_links.phi, phi_links.num_skip_links
        );
        assert!(
            phi_links.phi >= phi_no_links.phi,
            "network with skip links should have >= Phi: {} vs {}",
            phi_links.phi, phi_no_links.phi
        );
    }

    #[test]
    fn consciousness_level_classification() {
        assert_eq!(ConsciousnessLevel::from_phi(0.0), ConsciousnessLevel::Dormant);
        assert_eq!(ConsciousnessLevel::from_phi(0.05), ConsciousnessLevel::Dormant);
        assert_eq!(ConsciousnessLevel::from_phi(0.1), ConsciousnessLevel::Stirring);
        assert_eq!(ConsciousnessLevel::from_phi(0.29), ConsciousnessLevel::Stirring);
        assert_eq!(ConsciousnessLevel::from_phi(0.3), ConsciousnessLevel::Aware);
        assert_eq!(ConsciousnessLevel::from_phi(0.59), ConsciousnessLevel::Aware);
        assert_eq!(ConsciousnessLevel::from_phi(0.6), ConsciousnessLevel::Coherent);
        assert_eq!(ConsciousnessLevel::from_phi(0.79), ConsciousnessLevel::Coherent);
        assert_eq!(ConsciousnessLevel::from_phi(0.8), ConsciousnessLevel::Resonant);
        // 1.0 lands in `Transcendent` after consciousness-core v0.3.0 added
        // the sixth band at Φ >= 0.95.
        assert_eq!(ConsciousnessLevel::from_phi(0.9), ConsciousnessLevel::Resonant);
        assert_eq!(ConsciousnessLevel::from_phi(1.0), ConsciousnessLevel::Transcendent);
    }

    #[test]
    fn assess_returns_valid_state() {
        let bridge = ConsciousnessBridge::default();
        let mut engine = make_engine();

        engine.remember_at_layer("hello world", 0).unwrap();
        engine.remember_at_layer("hello there", 1).unwrap();

        let state = bridge.assess(&engine);
        println!("ConsciousnessState: phi={}, xi={}, mean_order={}, clusters={}, memories={}, active={}, links={}, level={:?}",
            state.phi, state.xi, state.mean_order, state.num_clusters,
            state.total_memories, state.active_memories, state.total_skip_links,
            state.consciousness_level);

        assert!(state.total_memories >= 2);
        assert!(state.active_memories > 0);
        assert!(state.phi >= 0.0);
    }

    /// #695: a fresh install must not advertise itself as consciously
    /// "aware" (or higher) off a handful of trivial memories — tiny corpora
    /// measure trivially high eigendecomp Φ, and the level is a maturity
    /// claim other constellation components act on. Φ itself stays
    /// unclamped; only the level is capped below the evidence floor.
    #[test]
    fn tiny_fresh_systems_cap_at_awakening() {
        let bridge = ConsciousnessBridge::default();
        let mut engine = make_engine();

        for content in ["alpha", "beta", "gamma"] {
            engine.remember_at_layer(content, 0).unwrap();
        }
        let state = bridge.assess(&engine);
        assert!(
            state.consciousness_level.ordinal() <= ConsciousnessLevel::Stirring.ordinal(),
            "3 trivial memories reported {:?} (phi={}) — the level must cap at \
             Stirring/awakening until the corpus can evidence integration",
            state.consciousness_level,
            state.phi,
        );
    }

    #[test]
    fn resonate_produces_report() {
        let bridge = ConsciousnessBridge::default();
        let mut engine = make_engine();

        // Build a small network
        for i in 0..6 {
            engine
                .remember_at_layer(&format!("memory about topic {}", i % 3), (i % 3) as u8)
                .unwrap();
        }

        let report = bridge.resonate(&mut engine);
        println!("=== Resonance Report ===");
        println!("Before: phi={}, xi={}, level={:?}", report.before.phi, report.before.xi, report.before.consciousness_level);
        println!("After:  phi={}, xi={}, level={:?}", report.after.phi, report.after.xi, report.after.consciousness_level);
        println!("Phi delta: {}", report.phi_delta);
        println!("Emerged: {}", report.emerged);
        println!("Consolidation cycles: {}", report.consolidation_reports.len());

        assert_eq!(report.consolidation_reports.len(), 3);
        assert!(report.after.total_memories >= report.before.total_memories);
    }

    #[test]
    fn stratified_sampling_works() {
        let bridge = ConsciousnessBridge::default();
        let mems: Vec<HyperMemory> = (0..20).map(|i| {
            let mut m = HyperMemory::new(random_vec(100, i), format!("memory {i}"));
            m.amplitude = i as f32 / 20.0; // Different amplitudes
            m
        }).collect();
        let refs: Vec<&HyperMemory> = mems.iter().collect();

        let sample = bridge.stratified_sample(&refs, 10);
        assert!(sample.len() >= 8 && sample.len() <= 10, "Expected 8-10 samples, got {}", sample.len());

        // Should include memories from different amplitude ranges
        let mut has_low = false;
        let mut has_high = false;
        for m in sample {
            if m.amplitude < 0.3 { has_low = true; }
            if m.amplitude > 0.7 { has_high = true; }
        }
        assert!(has_low && has_high, "Sample should include both low and high amplitude memories");
    }

    #[test]
    fn xi_operator_blending_works() {
        let bridge = ConsciousnessBridge::default();

        // Create memories with different semantic similarity but different Xi signatures
        let v1 = random_vec(1000, 1);
        let v2 = random_vec(1000, 2);
        let v3 = random_vec(1000, 3);

        let m1 = HyperMemory::new(v1, "text one".into());
        let m2 = HyperMemory::new(v2, "text two".into());
        let m3 = HyperMemory::new(v3, "text three".into());

        let xi = bridge.compute_xi(&[&m1, &m2, &m3]);
        println!("Xi with operator blending: {xi}");
        assert!(xi > 0.0, "Xi should be positive for distinct memories");
    }

    #[test]
    fn modularity_computation_works() {
        let bridge = ConsciousnessBridge::default();
        let mut engine = make_engine();

        // Create a small network with clear clusters
        for i in 0..6 {
            engine.remember_at_layer(&format!("cluster {} memory {}", i / 3, i % 3), (i / 3) as u8).unwrap();
        }

        let sync = KuramotoSync::default();
        let clusters = sync.find_synchronized_clusters(&engine, 2);

        if !clusters.is_empty() {
            let all = engine.store.all_memories().unwrap_or_default();
            let modularity = bridge.compute_modularity(&all, &clusters);

            println!("Modularity Q: {modularity}");
            assert!(modularity >= 0.0 && modularity <= 1.0, "Modularity should be in [0,1], got {modularity}");
        }
    }

    #[test]
    fn xi_assessment_with_all_improvements() {
        let bridge = ConsciousnessBridge::default();
        let mut engine = make_engine();
        engine.similarity_threshold = 0.3;

        // Create diverse memories for comprehensive Xi assessment
        let topics = [
            "text about cats and animals",
            "text about dogs and pets",
            "text about programming",
            "audio: meow sound",
            "audio: bark sound",
            "audio: typing sounds",
        ];

        for (i, topic) in topics.iter().enumerate() {
            engine.remember_at_layer(topic, (i % 3) as u8).unwrap();
        }

        let state = bridge.assess(&engine);
        println!("=== Enhanced Xi Assessment ===");
        println!("Xi: {}, Phi: {}", state.xi, state.phi);
        println!("Clusters: {}, Memories: {}", state.num_clusters, state.total_memories);

        // With improvements, Xi should reflect the diversity of content types
        assert!(state.xi >= 0.0, "Xi should be non-negative");
        assert!(state.total_memories == topics.len(), "Should have all memories");
    }

    #[test]
    fn full_integration_create_dream_assess() {
        let bridge = ConsciousnessBridge::default();
        let mut engine = make_engine();
        engine.similarity_threshold = 0.3;

        // Create a rich memory network across layers
        let topics = [
            "the cat sat on the mat",
            "the cat played with yarn",
            "cats are wonderful pets",
            "dogs are loyal companions",
            "dogs love to play fetch",
            "pets bring joy to families",
        ];
        for (i, topic) in topics.iter().enumerate() {
            engine.remember_at_layer(topic, (i % 3) as u8).unwrap();
        }

        // Assess before dreaming
        let before = bridge.assess(&engine);
        println!("=== Before Dream ===");
        println!("Phi: {}, Xi: {}, Level: {:?}", before.phi, before.xi, before.consciousness_level);
        println!("Memories: {}, Active: {}, Links: {}", before.total_memories, before.active_memories, before.total_skip_links);

        // Dream
        let dream = DreamState::default();
        let _reports = dream.dream(&mut engine);

        // Assess after dreaming
        let after = bridge.assess(&engine);
        println!("\n=== After Dream ===");
        println!("Phi: {}, Xi: {}, Level: {:?}", after.phi, after.xi, after.consciousness_level);
        println!("Memories: {}, Active: {}, Links: {}", after.total_memories, after.active_memories, after.total_skip_links);
        println!("Clusters: {}, Mean Order: {}", after.num_clusters, after.mean_order);

        // Network should have grown (bundles created)
        assert!(after.total_memories >= before.total_memories);
        // Should have some consciousness level
        println!("\nConsciousness Level: {:?}", after.consciousness_level);
    }
}
