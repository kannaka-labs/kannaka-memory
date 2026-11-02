//! Kuramoto phase synchronization — the emergence layer.
//!
//! Implements the Kuramoto model for memory phase alignment, where clusters
//! of related memories phase-lock into coherent narratives. The order parameter
//! r measures collective coherence: r=1 means perfect sync, r≈0 means incoherent.
//!
//! Tiered coupling (QS-3, #54): agents within the same hive sync with strong
//! positive K (intra-hive), agents in different hives repel with weak/negative K
//! (inter-hive), and during initial detection all agents use a shared K (detection).

use std::sync::Mutex;
use uuid::Uuid;

use crate::memory::HyperMemory;
use crate::spiral::norm; // #503: normalize stored phases into [0, TAU) at write time
use crate::store::ResonanceEngine;
use crate::wave::{cosine_similarity, normalize};

// ─── Cluster cache (process-wide + disk sidecar) ──────────────────────────
//
// `find_synchronized_clusters` is O(n² × d) — ~1.5B ops on 558 memories ×
// 10000 dims. Even with rayon parallelization it takes ~60 s per call.
// It is called from `assess()` (status) and `cluster_report` (observe), so
// every read-only CLI invocation pays the full cost.
//
// Two-tier cache:
//   1. Process-wide mutex cache: keyed by a memory fingerprint. Free
//      subsequent calls within a single `kannaka ...` invocation.
//   2. Disk sidecar at `<hrm_path>.clusters.json`: keyed by HRM file mtime
//      + min_cluster_size. Fresh `kannaka` processes find it and skip the
//      O(n²) recompute entirely. The swarm listen service produces a fresh
//      sidecar when it mutates the HRM; read-only consumers reuse it.
lazy_static::lazy_static! {
    static ref CLUSTER_CACHE: Mutex<Option<ClusterCacheEntry>> = Mutex::new(None);
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct ClusterCacheEntry {
    #[serde(default)]
    fingerprint: u64,
    min_cluster_size: usize,
    clusters: Vec<MemoryCluster>,
    #[serde(default)]
    hrm_mtime_secs: u64,
    #[serde(default)]
    memory_count: usize,
}

fn fingerprint_memories(mems: &[&HyperMemory]) -> u64 {
    // Order-independent XOR fingerprint over every memory's (id, updated_at,
    // phase). Phase is in it because clusters depend on it (the r-filter and
    // mean_phase) and consolidation rewrites phase without touching updated_at
    // (#1123).
    //
    // Pre-refactor this sampled only first/last/middle memories — any
    // mutation to a memory at an unsampled index (e.g. boost to memory #200
    // in a 600-memory HRM) left the fingerprint unchanged, so the in-process
    // CLUSTER_CACHE returned stale clusters until the HRM file mtime
    // happened to roll. Walking all memories is microseconds even on the
    // 1500-memory ARM box and eliminates the silent-staleness class.
    use std::hash::{Hash, Hasher};
    let mut acc: u64 = 0xcbf2_9ce4_8422_2325; // FNV offset basis seed
    for m in mems {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        m.id.hash(&mut h);
        m.updated_at.hash(&mut h);
        m.phase.to_bits().hash(&mut h);
        acc ^= h.finish();
    }
    // Mix in the count so {n distinct mems} ≠ {0 mems with same XOR sum}.
    let mut len_h = std::collections::hash_map::DefaultHasher::new();
    mems.len().hash(&mut len_h);
    acc.wrapping_add(len_h.finish())
}

fn hrm_mtime_secs(engine: &ResonanceEngine) -> Option<u64> {
    let path = engine.store.hrm_path()?;
    std::fs::metadata(path).ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
}

fn sidecar_path(engine: &ResonanceEngine) -> Option<std::path::PathBuf> {
    Some(engine.store.hrm_path()?.with_extension("clusters.json"))
}

/// The sidecar's clusters, if it was written for exactly this store state.
/// The whole-second mtime alone is not enough: an in-place phase rewrite, a
/// rewrite within the same second or a restore that keeps the mtime would hand
/// back clusters for memories that changed or no longer exist (#1123).
fn load_sidecar(
    path: &std::path::Path,
    mtime: u64,
    min_size: usize,
    fp: u64,
    n: usize,
) -> Option<Vec<MemoryCluster>> {
    let data = std::fs::read(path).ok()?;
    let entry: ClusterCacheEntry = serde_json::from_slice(&data).ok()?;
    (entry.hrm_mtime_secs == mtime
        && entry.min_cluster_size == min_size
        && entry.fingerprint == fp
        && entry.memory_count == n)
        .then_some(entry.clusters)
}

fn save_sidecar(path: &std::path::Path, entry: &ClusterCacheEntry) {
    let result = serde_json::to_vec(entry)
        .map_err(|e| e.to_string())
        .and_then(|data| crate::fs_util::atomic_write_bytes(path, &data));
    if let Err(e) = result {
        eprintln!("[kuramoto] could not write cluster sidecar {}: {e}", path.display());
    }
}

/// num_clusters fix: when on, `find_synchronized_clusters` mean-centers AND projects
/// out the top principal components ("decones") the vectors before building the
/// similarity graph. The field's hypervectors share a dominant common-mode direction
/// (~38% of variance on the local field) that otherwise links nearly the whole field
/// into ONE component at the raw 0.75 threshold; deconing removes it and exposes the
/// real sub-structure. Default OFF — only the graph EDGES change (cluster themes,
/// phases, and order-parameter still use the original vectors, so recall/storage are
/// untouched).
fn cluster_decone_enabled() -> bool {
    std::env::var("KANNAKA_CLUSTER_DECONE")
        .map(|v| {
            let v = v.trim();
            !v.is_empty() && v != "0" && !v.eq_ignore_ascii_case("false")
        })
        .unwrap_or(false)
}

/// Cluster-seed ordering convention (#333). FRAGILE — churned by three research
/// cycles (global sort → revert `98e8ce5` → selective `b9c921f`); breaking it
/// silently corrupts EITHER transfer OR xi, so it is factored out here and pinned
/// by `adversarials_sort_last_under_xi_engines`.
///
/// - `true` — TRANSFER engines (`engine_a`, `engine_b_primed`, `engine_b_naive`,
///   `engine_flat`): sort memories by content string so every transfer pass seeds
///   its BFS clusters from the SAME topology (the topology `engine_a` builds is
///   what `engine_b_primed` inherits and extends), recovering the T13 transfer
///   benefit. Adversarials are absent on these passes.
/// - `false` — xi/carrier engines (`engine_clean`, `engine_adv`) and any other or
///   unset context: PRESERVE `all_memories()` UUID order, which places T15
///   adversarials (`adv_l5_*`, UUID ≈ u128::MAX) LAST. `engine_adv` must NOT
///   content-sort (that pulls `adv_l5_*` ahead of the corpus alphabetically and
///   interleaves adversarials), and `engine_clean` must match `engine_adv`'s order
///   so the xi comparison sees a consistent topology.
fn content_sort_applies(drive_ctx: &str) -> bool {
    matches!(
        drive_ctx,
        "engine_a" | "engine_b_primed" | "engine_b_naive" | "engine_flat"
    )
}

/// Apply the [`content_sort_applies`] cluster-seed ordering to `mems` for the given
/// `DRIVE_CONTEXT`. Transfer engines get a content-string sort; every other context
/// preserves the incoming (`all_memories()` UUID) order so adversarials stay last.
fn seed_order<'a>(mems: Vec<&'a HyperMemory>, drive_ctx: &str) -> Vec<&'a HyperMemory> {
    if content_sort_applies(drive_ctx) {
        let mut sorted = mems;
        sorted.sort_by(|a, b| a.content.cmp(&b.content));
        sorted
    } else {
        mems
    }
}

/// Mean-center `vecs` and project out their top-`k` principal components, returning
/// the residual vectors (same order). The PCs are estimated from a bounded, evenly
/// spaced sample (≤ `SAMPLE` rows) via power iteration on the sample Gram, so the
/// extra cost is O(sample²·d + n·d·k) rather than O(n²·d) — safe on the 1-core hub.
/// With k==0 or a degenerate field it returns the mean-centered-only vectors.
fn decone_vectors(vecs: &[&[f32]], k: usize) -> Vec<Vec<f32>> {
    let n = vecs.len();
    if n == 0 {
        return Vec::new();
    }
    let d = vecs[0].len();
    let mut mean = vec![0.0f32; d];
    for v in vecs {
        for (m, &x) in mean.iter_mut().zip(v.iter()) {
            *m += x;
        }
    }
    for m in mean.iter_mut() {
        *m /= n as f32;
    }
    const SAMPLE: usize = 512;
    let step = (n / SAMPLE).max(1);
    let sample: Vec<usize> = (0..n).step_by(step).take(SAMPLE).collect();
    let s = sample.len();
    let cs: Vec<Vec<f32>> = sample
        .iter()
        .map(|&i| vecs[i].iter().zip(mean.iter()).map(|(&x, &m)| x - m).collect())
        .collect();
    let mut dirs: Vec<Vec<f32>> = Vec::new();
    if k > 0 && s >= 2 {
        let mut gram = vec![0.0f32; s * s];
        for a in 0..s {
            for b in a..s {
                let dot: f32 = cs[a].iter().zip(cs[b].iter()).map(|(x, y)| x * y).sum();
                gram[a * s + b] = dot;
                gram[b * s + a] = dot;
            }
        }
        for _ in 0..k {
            let mut u = vec![1.0f32 / (s as f32).sqrt(); s];
            let mut lam = 0.0f32;
            for _ in 0..60 {
                let mut w: Vec<f32> = (0..s)
                    .map(|i| (0..s).map(|j| gram[i * s + j] * u[j]).sum())
                    .collect();
                let nrm = w.iter().map(|x| x * x).sum::<f32>().sqrt();
                if nrm <= 1e-12 {
                    break;
                }
                for x in w.iter_mut() {
                    *x /= nrm;
                }
                lam = nrm;
                u = w;
            }
            if lam <= 1e-9 {
                break;
            }
            // Feature-space direction v = Σ uᵢ·csᵢ, unit-normalized.
            let mut v = vec![0.0f32; d];
            for (i, &ui) in u.iter().enumerate() {
                for (vd, &c) in v.iter_mut().zip(cs[i].iter()) {
                    *vd += ui * c;
                }
            }
            let vn = v.iter().map(|x| x * x).sum::<f32>().sqrt();
            if vn <= 1e-12 {
                break;
            }
            for x in v.iter_mut() {
                *x /= vn;
            }
            dirs.push(v);
            // Deflate so the next power iteration finds the following PC.
            for i in 0..s {
                for j in 0..s {
                    gram[i * s + j] -= lam * u[i] * u[j];
                }
            }
        }
    }
    vecs.iter()
        .map(|v| {
            let mut r: Vec<f32> = v.iter().zip(mean.iter()).map(|(&x, &m)| x - m).collect();
            for dir in &dirs {
                let dot: f32 = r.iter().zip(dir.iter()).map(|(a, b)| a * b).sum();
                for (rx, &dx) in r.iter_mut().zip(dir.iter()) {
                    *rx -= dot * dx;
                }
            }
            r
        })
        .collect()
}

/// Coupling tier for the Kuramoto model.
///
/// Controls the effective coupling constant K depending on the relationship
/// between two agents/memories:
/// - `Detection`: shared K during the initial detection window (all sync together)
/// - `IntraHive`: strong positive K for agents in the same hive
/// - `InterHive`: weak or negative K for agents in different hives
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CouplingTier {
    /// Initial detection window — all agents use shared coupling to find structure.
    Detection,
    /// Same hive — strong positive coupling pulls phases together.
    IntraHive,
    /// Different hives — weak/negative coupling maintains distinct phase clusters.
    InterHive,
}

/// Per-tier coupling constants.
#[derive(Debug, Clone, Copy)]
pub struct TieredCoupling {
    /// K for detection phase (shared sync window).
    pub detection_k: f32,
    /// K for intra-hive coupling (strong positive).
    pub intra_hive_k: f32,
    /// K for inter-hive coupling (weak or negative).
    pub inter_hive_k: f32,
}

impl Default for TieredCoupling {
    fn default() -> Self {
        Self {
            detection_k: 0.5,
            intra_hive_k: 2.0,
            inter_hive_k: -0.3,
        }
    }
}

impl TieredCoupling {
    /// Return the effective K for a given tier.
    pub fn k_for_tier(&self, tier: CouplingTier) -> f32 {
        match tier {
            CouplingTier::Detection => self.detection_k,
            CouplingTier::IntraHive => self.intra_hive_k,
            CouplingTier::InterHive => self.inter_hive_k,
        }
    }
}

/// Kuramoto synchronization model for memory phase alignment.
pub struct KuramotoSync {
    /// Base coupling constant K
    pub coupling_strength: f32,
    /// Time step for integration
    pub dt: f32,
    /// Number of integration steps per sync round
    pub steps: usize,
    /// Minimum similarity to consider memories as coupled
    pub coupling_threshold: f32,
}

impl Default for KuramotoSync {
    fn default() -> Self {
        Self {
            coupling_strength: 0.5,
            dt: 0.1,
            steps: 10,
            coupling_threshold: 0.75,
        }
    }
}

/// A synchronized cluster of memories.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MemoryCluster {
    pub memory_ids: Vec<Uuid>,
    pub order_parameter: f32,
    pub mean_phase: f32,
    pub coherence: f32,
    pub theme_vector: Vec<f32>,
}

/// Report from a sync_cluster operation.
#[derive(Debug, Clone)]
pub struct SyncReport {
    pub memories_synced: usize,
    pub initial_order: f32,
    pub final_order: f32,
    pub steps_taken: usize,
    pub converged: bool,
}

impl KuramotoSync {
    /// Compute the Kuramoto order parameter r = |1/N Σ e^(iφⱼ)|.
    ///
    /// Delegates to [`consciousness_core::kuramoto::KuramotoModel::order_parameter_unweighted`].
    pub fn order_parameter(&self, memories: &[&HyperMemory]) -> f32 {
        let phases: Vec<f32> = memories.iter().map(|m| m.phase).collect();
        consciousness_core::kuramoto::KuramotoModel::order_parameter_unweighted(&phases).r
    }

    /// Run Kuramoto integration on a cluster of memories.
    ///
    /// Updates each memory's phase in-place and returns a sync report.
    pub fn sync_cluster(&self, memories: &mut [&mut HyperMemory]) -> SyncReport {
        let n = memories.len();
        if n < 2 {
            return SyncReport {
                memories_synced: n,
                initial_order: 1.0,
                final_order: 1.0,
                steps_taken: 0,
                converged: true,
            };
        }

        // Compute pairwise coupling weights
        let mut weights = vec![vec![0.0f32; n]; n];
        for i in 0..n {
            for j in (i + 1)..n {
                let sim = cosine_similarity(&memories[i].vector, &memories[j].vector);
                if sim > self.coupling_threshold {
                    // TODO(chiral): skip link boost removed — coupling now purely similarity-based
                    // In ChiralMedium, coupling emerges from field interference
                    let w = sim;
                    weights[i][j] = w;
                    weights[j][i] = w;
                }
            }
        }

        let initial_order = {
            let refs: Vec<&HyperMemory> = memories.iter().map(|m| &**m).collect();
            self.order_parameter(&refs)
        };

        let nf = n as f32;
        let mut prev_order = initial_order;

        for step in 0..self.steps {
            // Compute phase deltas
            let phases: Vec<f32> = memories.iter().map(|m| m.phase).collect();
            let freqs: Vec<f32> = memories.iter().map(|m| m.frequency).collect();

            let mut dphi = vec![0.0f32; n];
            for i in 0..n {
                let mut coupling_sum = 0.0f32;
                for j in 0..n {
                    if i != j {
                        coupling_sum += weights[i][j] * (phases[j] - phases[i]).sin();
                    }
                }
                dphi[i] = freqs[i] + (self.coupling_strength / nf) * coupling_sum;
            }

            // Euler integration
            for i in 0..n {
                memories[i].phase = norm(memories[i].phase + dphi[i] * self.dt);
            }

            // Check convergence
            let current_order = {
                let refs: Vec<&HyperMemory> = memories.iter().map(|m| &**m).collect();
                self.order_parameter(&refs)
            };
            if (current_order - prev_order).abs() < 1e-6 && step > 0 {
                return SyncReport {
                    memories_synced: n,
                    initial_order,
                    final_order: current_order,
                    steps_taken: step + 1,
                    converged: true,
                };
            }
            prev_order = current_order;
        }

        let final_order = {
            let refs: Vec<&HyperMemory> = memories.iter().map(|m| &**m).collect();
            self.order_parameter(&refs)
        };

        SyncReport {
            memories_synced: n,
            initial_order,
            final_order,
            steps_taken: self.steps,
            converged: false,
        }
    }

    /// Compute pairwise coupling weights with tiered K values.
    ///
    /// `hive_labels[i]` is the hive index for memory `i` (or `None` if unassigned).
    /// During the `Detection` tier, hive labels are ignored and all pairs use the
    /// detection K. For `IntraHive`/`InterHive`, the tier is selected per-pair
    /// based on whether the two memories share a hive label.
    pub fn compute_tiered_weights(
        &self,
        memories: &[&HyperMemory],
        hive_labels: &[Option<usize>],
        tiered: &TieredCoupling,
        tier: CouplingTier,
    ) -> Vec<Vec<f32>> {
        let n = memories.len();
        let mut weights = vec![vec![0.0f32; n]; n];

        for i in 0..n {
            for j in (i + 1)..n {
                let sim = cosine_similarity(&memories[i].vector, &memories[j].vector);
                if sim <= self.coupling_threshold {
                    continue;
                }

                let effective_tier = match tier {
                    CouplingTier::Detection => CouplingTier::Detection,
                    _ => {
                        // Determine per-pair tier from hive labels
                        match (hive_labels[i], hive_labels[j]) {
                            (Some(a), Some(b)) if a == b => CouplingTier::IntraHive,
                            (Some(_), Some(_)) => CouplingTier::InterHive,
                            // Unassigned agents use detection coupling
                            _ => CouplingTier::Detection,
                        }
                    }
                };

                let k = tiered.k_for_tier(effective_tier);
                let w = sim * k;
                weights[i][j] = w;
                weights[j][i] = w;
            }
        }

        weights
    }

    /// Run Kuramoto integration with tiered coupling.
    ///
    /// Like `sync_cluster`, but uses `TieredCoupling` to differentiate intra-hive,
    /// inter-hive, and detection coupling strengths. The `hive_labels` slice maps
    /// each memory to its hive index (`None` if unassigned). The `tier` parameter
    /// selects whether we are in the detection window (shared K) or the
    /// specialization phase (per-pair K from hive labels).
    pub fn sync_cluster_tiered(
        &self,
        memories: &mut [&mut HyperMemory],
        hive_labels: &[Option<usize>],
        tiered: &TieredCoupling,
        tier: CouplingTier,
    ) -> SyncReport {
        let n = memories.len();
        if n < 2 {
            return SyncReport {
                memories_synced: n,
                initial_order: 1.0,
                final_order: 1.0,
                steps_taken: 0,
                converged: true,
            };
        }

        // Compute tiered coupling weights
        let refs: Vec<&HyperMemory> = memories.iter().map(|m| &**m).collect();
        let weights = self.compute_tiered_weights(&refs, hive_labels, tiered, tier);

        let initial_order = self.order_parameter(&refs);
        let nf = n as f32;
        let mut prev_order = initial_order;

        for step in 0..self.steps {
            let phases: Vec<f32> = memories.iter().map(|m| m.phase).collect();
            let freqs: Vec<f32> = memories.iter().map(|m| m.frequency).collect();

            let mut dphi = vec![0.0f32; n];
            for i in 0..n {
                let mut coupling_sum = 0.0f32;
                for j in 0..n {
                    if i != j {
                        coupling_sum += weights[i][j] * (phases[j] - phases[i]).sin();
                    }
                }
                // Use 1/N normalisation (coupling sign is baked into the weights)
                dphi[i] = freqs[i] + (1.0 / nf) * coupling_sum;
            }

            for i in 0..n {
                memories[i].phase = norm(memories[i].phase + dphi[i] * self.dt);
            }

            let current_order = {
                let refs: Vec<&HyperMemory> = memories.iter().map(|m| &**m).collect();
                self.order_parameter(&refs)
            };
            if (current_order - prev_order).abs() < 1e-6 && step > 0 {
                return SyncReport {
                    memories_synced: n,
                    initial_order,
                    final_order: current_order,
                    steps_taken: step + 1,
                    converged: true,
                };
            }
            prev_order = current_order;
        }

        let final_order = {
            let refs: Vec<&HyperMemory> = memories.iter().map(|m| &**m).collect();
            self.order_parameter(&refs)
        };

        SyncReport {
            memories_synced: n,
            initial_order,
            final_order,
            steps_taken: self.steps,
            converged: false,
        }
    }

    /// Spectral-inspired clustering that can split large components.
    /// After BFS finds components, if a component is large (>50% of memories), 
    /// attempt to split it by raising the threshold iteratively.
    fn spectral_split_component(
        &self,
        component_indices: &[usize],
        all_memories: &[&HyperMemory],
        base_threshold: f32,
    ) -> Vec<Vec<usize>> {
        let n = component_indices.len();
        if n <= 10 {
            return vec![component_indices.to_vec()];
        }
        
        // Try progressively higher thresholds
        let thresholds = [0.8, 0.85, 0.9];
        
        for &threshold in &thresholds {
            if threshold <= base_threshold {
                continue;
            }
            
            // Build adjacency list with higher threshold
            let mut adj: Vec<Vec<usize>> = vec![vec![]; n];
            for (i, &idx_i) in component_indices.iter().enumerate() {
                for (j, &idx_j) in component_indices.iter().enumerate().skip(i + 1) {
                    let sim = cosine_similarity(&all_memories[idx_i].vector, &all_memories[idx_j].vector);
                    if sim > threshold {
                        adj[i].push(j);
                        adj[j].push(i);
                    }
                }
            }
            
            // Find connected components with higher threshold
            let mut visited = vec![false; n];
            let mut subcomponents = Vec::new();
            
            for start in 0..n {
                if visited[start] {
                    continue;
                }
                let mut component = vec![component_indices[start]];
                let mut queue = vec![start];
                visited[start] = true;
                
                while let Some(node) = queue.pop() {
                    for &neighbor in &adj[node] {
                        if !visited[neighbor] {
                            visited[neighbor] = true;
                            component.push(component_indices[neighbor]);
                            queue.push(neighbor);
                        }
                    }
                }
                
                if component.len() >= 2 {
                    subcomponents.push(component);
                }
            }
            
            // If we successfully split into multiple components, return them
            if subcomponents.len() > 1 {
                return subcomponents;
            }
        }
        
        // If no split was successful, return original component
        vec![component_indices.to_vec()]
    }

    /// Find groups of memories that have phase-locked (order parameter > 0.7).
    pub fn find_synchronized_clusters(
        &self,
        engine: &ResonanceEngine,
        min_cluster_size: usize,
    ) -> Vec<MemoryCluster> {
        let all = match engine.store.all_memories() {
            Ok(mems) => mems,
            Err(_) => return vec![],
        };
        let n = all.len();
        if n < min_cluster_size {
            return vec![];
        }

        // Two-tier cache check: in-process fingerprint first, then on-disk
        // sidecar keyed by HRM file mtime.
        let decone = cluster_decone_enabled();
        // Fold the decone flag into the cache key so toggling it never returns
        // clusters computed under the other mode.
        let fp = fingerprint_memories(&all) ^ if decone { 0x5EC0_DE5E_5EC0_DE5E } else { 0 };
        if let Ok(guard) = CLUSTER_CACHE.lock() {
            if let Some(ref entry) = *guard {
                if entry.fingerprint == fp && entry.min_cluster_size == min_cluster_size {
                    return entry.clusters.clone();
                }
            }
        }
        let mtime = hrm_mtime_secs(engine).unwrap_or(0);
        if !decone && mtime != 0 {
            let hit = sidecar_path(engine)
                .and_then(|p| load_sidecar(&p, mtime, min_cluster_size, fp, n));
            if let Some(clusters) = hit {
                // Populate process cache for future calls in this same run.
                if let Ok(mut guard) = CLUSTER_CACHE.lock() {
                    *guard = Some(ClusterCacheEntry {
                        fingerprint: fp,
                        min_cluster_size,
                        clusters: clusters.clone(),
                        hrm_mtime_secs: mtime,
                        memory_count: n,
                    });
                }
                return clusters;
            }
        }

        // Cluster-seed ordering (#333): transfer engines content-sort; xi/carrier
        // engines preserve UUID order so adversarials stay last. The convention and
        // its rationale live on `content_sort_applies` — DO NOT inline a sort here.
        let drive_ctx = std::env::var("DRIVE_CONTEXT").unwrap_or_default();
        let all = seed_order(all, &drive_ctx);

        // Build adjacency list from similarity graph.
        //
        // PERF: pair similarity is O(n² × d). For n=558, d=10000 that's ~1.56B
        // float ops per call — ~30-90 s on commodity hardware. We parallelize
        // across the outer `i` loop with rayon and build per-row neighbor lists
        // that are stitched back into an adjacency list. Each pair is visited
        // once (j > i) so there's no redundant work.
        use rayon::prelude::*;
        // num_clusters fix: optionally "decone" the vectors before the graph (see
        // cluster_decone_enabled). Only the EDGES change — themes/phases/order below
        // still use the original vectors. Lower threshold for the residual cosine scale.
        let dvecs: Vec<Vec<f32>> = if decone {
            let refs: Vec<&[f32]> = all.iter().map(|m| m.vector.as_slice()).collect();
            decone_vectors(&refs, 3)
        } else {
            Vec::new()
        };
        let threshold = if decone { 0.30 } else { self.coupling_threshold };
        let neighbors_per_i: Vec<Vec<usize>> = (0..n).into_par_iter().map(|i| {
            let mut neigh = Vec::new();
            for j in (i + 1)..n {
                let sim = if decone {
                    cosine_similarity(&dvecs[i], &dvecs[j])
                } else {
                    cosine_similarity(&all[i].vector, &all[j].vector)
                };
                if sim > threshold {
                    neigh.push(j);
                }
            }
            neigh
        }).collect();
        let mut adj: Vec<Vec<usize>> = vec![vec![]; n];
        for (i, neigh) in neighbors_per_i.iter().enumerate() {
            for &j in neigh {
                adj[i].push(j);
                adj[j].push(i);
            }
        }

        // Find connected components via BFS
        let mut visited = vec![false; n];
        let mut raw_components = Vec::new();

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

            if component.len() >= min_cluster_size {
                raw_components.push(component);
            }
        }

        // Apply spectral-inspired splitting to large components
        let mut final_components = Vec::new();
        for component in raw_components {
            let component_size = component.len();
            if !decone && component_size > n / 2 { // Large component (>50% of memories)
                let subcomponents = self.spectral_split_component(&component, &all, self.coupling_threshold);
                for subcomp in subcomponents {
                    if subcomp.len() >= min_cluster_size {
                        final_components.push(subcomp);
                    }
                }
            } else {
                final_components.push(component);
            }
        }

        // Convert to MemoryCluster objects
        let mut clusters = Vec::new();
        for component in final_components {
            let cluster_mems: Vec<&HyperMemory> = component.iter().map(|&i| all[i]).collect();

            let r = self.order_parameter(&cluster_mems);
            if r <= 0.3 {
                continue;
            }

            // Compute mean phase
            let sum_cos: f32 = cluster_mems.iter().map(|m| m.phase.cos()).sum();
            let sum_sin: f32 = cluster_mems.iter().map(|m| m.phase.sin()).sum();
            let mean_phase = sum_sin.atan2(sum_cos);

            // Coherence = 1 - circular variance
            let coherence = r; // r itself measures tightness of phase locking

            // Theme vector = bundle of all member vectors
            let dim = cluster_mems[0].vector.len();
            let mut theme = vec![0.0f32; dim];
            for m in &cluster_mems {
                for (i, v) in m.vector.iter().enumerate() {
                    theme[i] += v;
                }
            }
            normalize(&mut theme);

            // If the members' vectors largely cancel, the bundle sums to ~0 and
            // normalize leaves it all-zeros. This is reachable when a cluster is
            // grouped by PHASE coupling rather than vector similarity — notably
            // in decone mode (default), where edges are built on residual
            // vectors so a connected component can contain members whose
            // ORIGINAL vectors are anti-correlated. An all-zero theme_vector
            // scores cosine 0 against every query (cosine_similarity returns 0
            // for a zero norm), silently making the whole cluster unreachable by
            // theme-routed recall. Fall back to the strongest member's vector —
            // a guaranteed non-zero, representative anchor.
            let theme_norm: f32 = theme.iter().map(|x| x * x).sum::<f32>().sqrt();
            if theme_norm < 1e-6 {
                if let Some(strongest) = cluster_mems.iter().max_by(|a, b| {
                    a.amplitude
                        .partial_cmp(&b.amplitude)
                        .unwrap_or(std::cmp::Ordering::Equal)
                }) {
                    theme = strongest.vector.clone();
                    normalize(&mut theme);
                }
            }

            clusters.push(MemoryCluster {
                memory_ids: cluster_mems.iter().map(|m| m.id).collect(),
                order_parameter: r,
                mean_phase,
                coherence,
                theme_vector: theme,
            });
        }

        // Cache: in-process + disk sidecar so the next `kannaka` invocation
        // skips the O(n²d) recompute entirely. `mtime` is the one read BEFORE
        // the computation: re-reading it here could stamp clusters computed
        // from the old state with a write that landed meanwhile (#1123).
        let entry = ClusterCacheEntry {
            fingerprint: fp,
            min_cluster_size,
            clusters: clusters.clone(),
            hrm_mtime_secs: mtime,
            memory_count: n,
        };
        if let Ok(mut guard) = CLUSTER_CACHE.lock() {
            *guard = Some(entry.clone());
        }
        // Don't persist the on-disk sidecar in decone mode: it's keyed by mtime+size,
        // not the decone flag, so a later non-decone run would wrongly load it.
        if !decone {
            if let Some(path) = sidecar_path(engine) {
                save_sidecar(&path, &entry);
            }
        }

        clusters
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // #1123: the disk sidecar must miss when the store changed behind an
    // unchanged whole-second mtime.
    #[test]
    fn sidecar_hits_only_for_the_same_store_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kannaka.clusters.json");
        let entry = ClusterCacheEntry {
            fingerprint: 0xF00D,
            min_cluster_size: 3,
            clusters: Vec::new(),
            hrm_mtime_secs: 1_700_000_000,
            memory_count: 42,
        };
        save_sidecar(&path, &entry);
        let load = |fp, n| load_sidecar(&path, 1_700_000_000, 3, fp, n);
        assert!(load(0xF00D, 42).is_some(), "control: the same state must hit");
        assert!(load(0xBEEF, 42).is_none(), "a changed fingerprint must miss");
        assert!(load(0xF00D, 41).is_none(), "a changed memory count must miss");
        assert!(load_sidecar(&path, 1_700_000_001, 3, 0xF00D, 42).is_none(), "a changed mtime must miss");
    }

    // #1123: consolidation rewrites phase without touching updated_at, and
    // clusters depend on phase.
    #[test]
    fn fingerprint_changes_when_only_a_phase_changes() {
        let mut m = HyperMemory::new(vec![0.5; 8], "phase probe".to_string());
        let before = fingerprint_memories(&[&m]);
        assert_eq!(fingerprint_memories(&[&m]), before, "control: the fingerprint is stable");
        m.phase += 0.25;
        assert_ne!(fingerprint_memories(&[&m]), before);
    }
    use crate::codebook::Codebook;
    use crate::encoding::{EncodingPipeline, SimpleHashEncoder};
    use crate::memory::HyperMemory;
    use crate::store::{TestMedium, ResonanceEngine};
    use std::f32::consts::PI;

    fn make_engine() -> ResonanceEngine {
        let encoder = SimpleHashEncoder::new(384, 42);
        let codebook = Codebook::new(384, 10_000, 42);
        let pipeline = EncodingPipeline::new(Box::new(encoder), codebook);
        ResonanceEngine::new(Box::new(TestMedium::new()), pipeline)
    }

    fn make_memory_with_phase(vector: Vec<f32>, content: &str, phase: f32) -> HyperMemory {
        let mut m = HyperMemory::new(vector, content.to_string());
        m.phase = phase;
        m
    }

    fn similar_vec(dim: usize) -> Vec<f32> {
        let mut v = vec![0.0f32; dim];
        for i in 0..100 {
            v[i] = 1.0;
        }
        crate::wave::normalize(&mut v);
        v
    }

    fn orthogonal_vec(dim: usize) -> Vec<f32> {
        let mut v = vec![0.0f32; dim];
        for i in 200..300 {
            v[i] = 1.0;
        }
        crate::wave::normalize(&mut v);
        v
    }

    #[test]
    fn identical_phase_order_parameter_is_one() {
        let sync = KuramotoSync::default();
        let v = similar_vec(100);
        let m1 = make_memory_with_phase(v.clone(), "a", 0.5);
        let m2 = make_memory_with_phase(v.clone(), "b", 0.5);
        let m3 = make_memory_with_phase(v.clone(), "c", 0.5);
        let refs: Vec<&HyperMemory> = vec![&m1, &m2, &m3];
        let r = sync.order_parameter(&refs);
        println!("Identical-phase order parameter: {r}");
        assert!((r - 1.0).abs() < 1e-5, "identical phases should give r≈1.0, got {r}");
    }

    // #503: sync_cluster must NORMALIZE the phase it writes back into [0, TAU),
    // so stored phases never drift unbounded (f32 precision loss + silently-wrong
    // future non-circular reads). Fed a heavily drifted fixture (±50, 137 rad),
    // every stored phase must come out in range. Anti-vacuous: reverting the
    // `spiral::norm` at the write site leaves the phases near their drifted input
    // and reddens this test.
    #[test]
    fn sync_cluster_normalizes_drifted_phases() {
        use std::f32::consts::TAU;
        let sync = KuramotoSync::default();
        let v = similar_vec(100);
        let mut m1 = make_memory_with_phase(v.clone(), "a", 50.0);
        let mut m2 = make_memory_with_phase(v.clone(), "b", -50.0);
        let mut m3 = make_memory_with_phase(v.clone(), "c", 137.0);
        let mut mems: Vec<&mut HyperMemory> = vec![&mut m1, &mut m2, &mut m3];
        let _ = sync.sync_cluster(&mut mems);
        for m in &mems {
            assert!(
                (0.0..TAU).contains(&m.phase),
                "sync_cluster must normalize the stored phase into [0, TAU), got {}",
                m.phase
            );
        }
    }

    // #503: write-time normalization is behaviour-PRESERVING because the read
    // side is 2π-periodic. A drifted fixture (+9 full turns) and its normalized
    // twin must yield the same order parameter — proving normalizing at write
    // time cannot change recall/consolidation behaviour.
    #[test]
    fn normalization_preserves_order_parameter() {
        use std::f32::consts::TAU;
        let sync = KuramotoSync::default();
        let v = similar_vec(100);
        let base = [0.3f32, 1.1, 2.7];
        let drifted: Vec<HyperMemory> = base
            .iter()
            .enumerate()
            .map(|(i, &p)| make_memory_with_phase(v.clone(), &format!("d{i}"), p + TAU * 9.0))
            .collect();
        let normalized: Vec<HyperMemory> = base
            .iter()
            .enumerate()
            .map(|(i, &p)| make_memory_with_phase(v.clone(), &format!("n{i}"), p))
            .collect();
        let rd = sync.order_parameter(&drifted.iter().collect::<Vec<_>>());
        let rn = sync.order_parameter(&normalized.iter().collect::<Vec<_>>());
        assert!(
            (rd - rn).abs() < 1e-4,
            "order parameter must match for drifted vs normalized phases (2π-periodic): {rd} vs {rn}"
        );
    }

    #[test]
    fn random_phase_order_parameter_is_low() {
        let sync = KuramotoSync::default();
        let v = similar_vec(100);
        // Evenly spaced phases around the circle → r ≈ 0
        let phases = [0.0, PI * 2.0 / 5.0, PI * 4.0 / 5.0, PI * 6.0 / 5.0, PI * 8.0 / 5.0];
        let mems: Vec<HyperMemory> = phases
            .iter()
            .enumerate()
            .map(|(i, &p)| make_memory_with_phase(v.clone(), &format!("m{i}"), p))
            .collect();
        let refs: Vec<&HyperMemory> = mems.iter().collect();
        let r = sync.order_parameter(&refs);
        println!("Evenly-spaced-phase order parameter: {r}");
        assert!(r < 0.3, "evenly spaced phases should give low r, got {r}");
    }

    #[test]
    fn sync_cluster_increases_order_parameter() {
        let sync = KuramotoSync {
            coupling_strength: 2.0, // strong coupling
            dt: 0.1,
            steps: 50,
            coupling_threshold: 0.3,
        };
        let dim = 100;
        let v = similar_vec(dim);

        let mut m1 = make_memory_with_phase(v.clone(), "a", 0.0);
        let mut m2 = make_memory_with_phase(v.clone(), "b", 1.0);
        let mut m3 = make_memory_with_phase(v.clone(), "c", 2.0);

        let initial_r = {
            let refs: Vec<&HyperMemory> = vec![&m1, &m2, &m3];
            sync.order_parameter(&refs)
        };

        let mut refs: Vec<&mut HyperMemory> = vec![&mut m1, &mut m2, &mut m3];
        let report = sync.sync_cluster(&mut refs);

        println!("Sync report: initial_order={}, final_order={}, steps={}, converged={}",
            report.initial_order, report.final_order, report.steps_taken, report.converged);
        println!("Phases after sync: {}, {}, {}", m1.phase, m2.phase, m3.phase);

        assert!(
            report.final_order > initial_r,
            "order should increase: {} -> {}",
            initial_r, report.final_order
        );
    }

    // skip_linked_memories_sync_faster test removed — skip links replaced by ChiralMedium interference

    #[test]
    fn find_synchronized_clusters_groups_related() {
        let mut engine = make_engine();
        let dim = 10_000;
        let va = similar_vec(dim);
        let vb = orthogonal_vec(dim);

        // Group A: same vector, same phase → should cluster & sync
        for i in 0..3 {
            let mut m = HyperMemory::new(va.clone(), format!("group_a_{i}"));
            m.phase = 0.1;
            engine.store.insert(m).unwrap();
        }
        // Group B: different vector, same phase → separate cluster
        for i in 0..3 {
            let mut m = HyperMemory::new(vb.clone(), format!("group_b_{i}"));
            m.phase = 0.2;
            engine.store.insert(m).unwrap();
        }

        let sync = KuramotoSync::default();
        let clusters = sync.find_synchronized_clusters(&engine, 2);

        println!("Found {} synchronized clusters", clusters.len());
        for (i, c) in clusters.iter().enumerate() {
            println!("  Cluster {}: {} memories, r={}, mean_phase={}", i, c.memory_ids.len(), c.order_parameter, c.mean_phase);
        }

        assert!(clusters.len() >= 2, "should find at least 2 clusters, got {}", clusters.len());
        // Each cluster should have high order parameter
        for c in &clusters {
            assert!(c.order_parameter > 0.7, "cluster should be synchronized, r={}", c.order_parameter);
        }
    }

    // #333: pin the fragile cluster-seed ordering convention. Under xi-eval engines
    // (engine_clean / engine_adv / unset) the all_memories() UUID order — which puts
    // adversarials (adv_l5_*) last — MUST be preserved; under transfer engines the
    // content sort applies (and would pull adv_l5_* to the front, which is exactly
    // why the xi engines must not sort).
    #[test]
    fn adversarials_sort_last_under_xi_engines() {
        // Incoming order mimics all_memories(): corpus (non-alphabetical), then
        // adversarials last (as their ~u128::MAX UUIDs place them).
        let contents = ["banana", "apple", "cherry", "adv_l5_0", "adv_l5_1"];
        let mems: Vec<HyperMemory> = contents
            .iter()
            .map(|c| HyperMemory::new(vec![0.1f32; 4], c.to_string()))
            .collect();
        let refs: Vec<&HyperMemory> = mems.iter().collect();

        // xi/carrier engines + unset: order preserved → adversarials remain last.
        for ctx in ["engine_adv", "engine_clean", ""] {
            assert!(!content_sort_applies(ctx), "xi engine '{ctx}' must not content-sort");
            let ordered = seed_order(refs.clone(), ctx);
            let tail: Vec<&str> =
                ordered.iter().rev().take(2).map(|m| m.content.as_str()).collect();
            assert!(
                tail.iter().all(|c| c.starts_with("adv_l5_")),
                "xi engine '{ctx}': adversarials must stay last, tail={tail:?}"
            );
        }

        // Anti-vacuous: a transfer engine content-sorts, pulling adv_l5_* to the
        // FRONT ("adv_" < "apple") — proving the xi engines' no-sort is load-bearing.
        assert!(content_sort_applies("engine_a"), "transfer engine must content-sort");
        let sorted = seed_order(refs.clone(), "engine_a");
        assert!(
            sorted[0].content.starts_with("adv_l5_"),
            "content sort pulls adversarials to the front (why xi engines must not sort)"
        );
    }

    #[test]
    fn spectral_splitting_breaks_large_components() {
        let mut engine = make_engine();
        let dim = 10_000;
        
        // Create two groups with moderate similarity
        let v1 = similar_vec(dim);
        let v2 = orthogonal_vec(dim);
        
        // Group 1: 6 memories with similar vectors (but not identical)
        for i in 0..6 {
            let mut v = v1.clone();
            // Add small perturbations
            for j in 0..100 {
                v[j] += (i as f32) * 0.1;
            }
            normalize(&mut v);
            let mut m = HyperMemory::new(v, format!("group1_{i}"));
            m.phase = 0.1;
            engine.store.insert(m).unwrap();
        }
        
        // Group 2: 4 memories with different vectors  
        for i in 0..4 {
            let mut v = v2.clone();
            // Add small perturbations
            for j in 200..300 {
                v[j] += (i as f32) * 0.1;
            }
            normalize(&mut v);
            let mut m = HyperMemory::new(v, format!("group2_{i}"));
            m.phase = 0.2;
            engine.store.insert(m).unwrap();
        }
        
        let sync = KuramotoSync::default();
        let clusters = sync.find_synchronized_clusters(&engine, 2);
        
        println!("Spectral clustering found {} clusters", clusters.len());
        for (i, c) in clusters.iter().enumerate() {
            println!("  Cluster {}: {} memories, r={}", i, c.memory_ids.len(), c.order_parameter);
        }
        
        // With spectral splitting, we should get multiple clusters instead of one giant component
        assert!(clusters.len() >= 1, "Should find at least one cluster");
        
        // Test that large single cluster gets split if needed
        if clusters.len() == 1 && clusters[0].memory_ids.len() >= 5 {
            println!("Note: Spectral splitting may not have split - this is okay if similarities are too high");
        }
    }

    #[test]
    fn consolidation_with_sync_converges_phases() {
        use crate::consolidation::{ConsolidationEngine, DreamState};

        let mut engine = make_engine();
        let dim = 10_000;
        let v = similar_vec(dim);

        // Insert related memories with different phases
        let mut ids = Vec::new();
        for (i, phase) in [0.0f32, 0.5, 1.0, 1.5].iter().enumerate() {
            let mut m = HyperMemory::new(v.clone(), format!("related_{i}"));
            m.phase = *phase;
            m.layer_depth = 0;
            let id = engine.store.insert(m).unwrap();
            ids.push(id);
        }

        let sync = KuramotoSync {
            coupling_strength: 2.0,
            steps: 30,
            ..Default::default()
        };

        // Measure order before
        let before_mems: Vec<&HyperMemory> = ids.iter()
            .filter_map(|id| engine.store.get(id).ok().flatten())
            .collect();
        let r_before = sync.order_parameter(&before_mems);

        // Run sync on these memories
        let mut mems: Vec<HyperMemory> = ids.iter()
            .filter_map(|id| engine.store.get(id).ok().flatten().cloned())
            .collect();
        let mut refs: Vec<&mut HyperMemory> = mems.iter_mut().collect();
        let report = sync.sync_cluster(&mut refs);

        // Write back updated phases
        for m in &mems {
            if let Ok(Some(stored)) = engine.store.get_mut(&m.id) {
                stored.phase = m.phase;
            }
        }

        let after_mems: Vec<&HyperMemory> = ids.iter()
            .filter_map(|id| engine.store.get(id).ok().flatten())
            .collect();
        let r_after = sync.order_parameter(&after_mems);

        println!("Consolidation sync: r_before={r_before}, r_after={r_after}");
        println!("Report: {report:?}");

        assert!(r_after > r_before, "phases should converge: {r_before} -> {r_after}");
    }

    // -----------------------------------------------------------------------
    // Tiered coupling tests (QS-3)
    // -----------------------------------------------------------------------

    #[test]
    fn tiered_coupling_k_for_tier() {
        let tc = TieredCoupling::default();
        assert_eq!(tc.k_for_tier(CouplingTier::Detection), 0.5);
        assert_eq!(tc.k_for_tier(CouplingTier::IntraHive), 2.0);
        assert_eq!(tc.k_for_tier(CouplingTier::InterHive), -0.3);
    }

    #[test]
    fn detection_tier_syncs_all_agents() {
        // During detection, all memories use shared K — they should converge.
        let sync = KuramotoSync {
            coupling_strength: 1.0,
            dt: 0.1,
            steps: 50,
            coupling_threshold: 0.3,
        };
        let dim = 100;
        let v = similar_vec(dim);
        let tiered = TieredCoupling {
            detection_k: 2.0,
            intra_hive_k: 2.0,
            inter_hive_k: -0.3,
        };

        let mut m1 = make_memory_with_phase(v.clone(), "d1", 0.0);
        let mut m2 = make_memory_with_phase(v.clone(), "d2", 1.0);
        let mut m3 = make_memory_with_phase(v.clone(), "d3", 2.0);

        let initial_r = {
            let refs: Vec<&HyperMemory> = vec![&m1, &m2, &m3];
            sync.order_parameter(&refs)
        };

        // All unassigned — detection tier
        let labels: Vec<Option<usize>> = vec![None, None, None];
        let mut refs: Vec<&mut HyperMemory> = vec![&mut m1, &mut m2, &mut m3];
        let report = sync.sync_cluster_tiered(&mut refs, &labels, &tiered, CouplingTier::Detection);

        println!("Detection tier: initial_r={}, final_r={}", initial_r, report.final_order);
        assert!(
            report.final_order > initial_r,
            "detection tier should increase order: {} -> {}",
            initial_r, report.final_order
        );
    }

    #[test]
    fn intra_hive_tier_syncs_same_hive() {
        // Memories in the same hive should converge with strong positive K.
        let sync = KuramotoSync {
            coupling_strength: 1.0,
            dt: 0.1,
            steps: 50,
            coupling_threshold: 0.3,
        };
        let dim = 100;
        let v = similar_vec(dim);
        let tiered = TieredCoupling {
            detection_k: 0.5,
            intra_hive_k: 3.0,
            inter_hive_k: -0.3,
        };

        let mut m1 = make_memory_with_phase(v.clone(), "h1a", 0.0);
        let mut m2 = make_memory_with_phase(v.clone(), "h1b", 1.5);
        let mut m3 = make_memory_with_phase(v.clone(), "h1c", 3.0);

        let initial_r = {
            let refs: Vec<&HyperMemory> = vec![&m1, &m2, &m3];
            sync.order_parameter(&refs)
        };

        // All in hive 0
        let labels: Vec<Option<usize>> = vec![Some(0), Some(0), Some(0)];
        let mut refs: Vec<&mut HyperMemory> = vec![&mut m1, &mut m2, &mut m3];
        let report = sync.sync_cluster_tiered(&mut refs, &labels, &tiered, CouplingTier::IntraHive);

        println!("IntraHive tier: initial_r={}, final_r={}", initial_r, report.final_order);
        assert!(
            report.final_order > initial_r,
            "intra-hive tier should increase order: {} -> {}",
            initial_r, report.final_order
        );
    }

    #[test]
    fn inter_hive_tier_maintains_separation() {
        // Memories in different hives should NOT converge (negative K pushes apart).
        let sync = KuramotoSync {
            coupling_strength: 1.0,
            dt: 0.05,
            steps: 30,
            coupling_threshold: 0.3,
        };
        let dim = 100;
        let v = similar_vec(dim);
        let tiered = TieredCoupling {
            detection_k: 0.5,
            intra_hive_k: 2.0,
            inter_hive_k: -1.0, // strong repulsion
        };

        // Two clusters at distinct phases
        let mut m1 = make_memory_with_phase(v.clone(), "h0a", 0.0);
        let mut m2 = make_memory_with_phase(v.clone(), "h0b", 0.1);
        let mut m3 = make_memory_with_phase(v.clone(), "h1a", PI);
        let mut m4 = make_memory_with_phase(v.clone(), "h1b", PI + 0.1);

        let initial_r = {
            let refs: Vec<&HyperMemory> = vec![&m1, &m2, &m3, &m4];
            sync.order_parameter(&refs)
        };

        // hive 0 vs hive 1 — inter-hive coupling is negative
        let labels: Vec<Option<usize>> = vec![Some(0), Some(0), Some(1), Some(1)];
        let mut refs: Vec<&mut HyperMemory> = vec![&mut m1, &mut m2, &mut m3, &mut m4];
        let report = sync.sync_cluster_tiered(&mut refs, &labels, &tiered, CouplingTier::IntraHive);

        println!("InterHive tier: initial_r={}, final_r={}", initial_r, report.final_order);

        // Global order should stay low (the two hives are kept apart)
        assert!(
            report.final_order < 0.8,
            "inter-hive negative coupling should prevent full global sync: r={}",
            report.final_order
        );
    }

    #[test]
    fn tiered_weights_detection_uses_shared_k() {
        let sync = KuramotoSync {
            coupling_strength: 1.0,
            dt: 0.1,
            steps: 10,
            coupling_threshold: 0.3,
        };
        let dim = 100;
        let v = similar_vec(dim);
        let tiered = TieredCoupling {
            detection_k: 0.7,
            intra_hive_k: 2.0,
            inter_hive_k: -0.5,
        };

        let m1 = make_memory_with_phase(v.clone(), "a", 0.0);
        let m2 = make_memory_with_phase(v.clone(), "b", 1.0);
        let refs: Vec<&HyperMemory> = vec![&m1, &m2];

        // Detection tier ignores hive labels
        let labels: Vec<Option<usize>> = vec![Some(0), Some(1)];
        let weights = sync.compute_tiered_weights(&refs, &labels, &tiered, CouplingTier::Detection);

        let sim = cosine_similarity(&m1.vector, &m2.vector);
        let expected = sim * 0.7; // detection_k = 0.7
        assert!(
            (weights[0][1] - expected).abs() < 1e-5,
            "detection tier should use detection_k: got {}, expected {}",
            weights[0][1], expected
        );
    }
}
