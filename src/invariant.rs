//! δ-Invariant Metrics for Memory Clustering
//!
//! Inspired by the paper's "irrationality measure" δ that's invariant under 
//! formula transformations. Implements metrics that remain stable across 
//! memory transformations for robust clustering.

use std::collections::HashMap;
use uuid::Uuid;

use crate::memory::HyperMemory;
use crate::store::ResonanceEngine;

/// How many memories `cluster_by_delta` will pair up before capping.
///
/// The neighbour build is O(n²·d). At 4096 that is ~8.4M pairs — seconds, not
/// minutes, at a typical embedding width. A live HRM holding tens of thousands
/// of memories would be hundreds of times that, which is where "slow" stops
/// being distinguishable from "hung".
pub const DEFAULT_DELTA_CLUSTER_MAX: usize = 4096;

/// How many nearest wavefronts `compute_delta` reconstructs a memory from.
pub const DELTA_NEIGHBORS: usize = 5;

/// Indices of the `k` most cosine-similar vectors to each input, best-first.
///
/// Split out of `cluster_by_delta` so the part that actually costs something
/// can be tested without a store behind it — the reason the quadratic blowup
/// and the guard bypass both went unnoticed is that nothing could reach this
/// logic on its own.
///
/// #810 — the previous inline version, per vector, allocated an n-element Vec,
/// called `cosine_similarity` n times (each recomputing BOTH norms from
/// scratch, so 2n² norm passes over d floats), and fully sorted n entries to
/// keep 5. This normalises once up front so each pair costs a single dot
/// product, walks only the upper triangle since `sim(i,j) == sim(j,i)`, and
/// keeps a fixed k-slot row instead of sorting n. Same neighbours, same order,
/// far less work — though still O(n²·d), which is why the caller caps n.
///
/// #809 — the old code called `consciousness_core::wave::cosine_similarity`
/// directly, bypassing `crate::wave::cosine_similarity`'s empty-vector guard.
/// The guard is honoured here by construction: a vector with no magnitude
/// scores 0.0 against everything, exactly as the guard returns. Its WARNING is
/// deliberately raised once per input by the caller rather than once per pair —
/// routing every pair through the guard, which is the literal fix, would make
/// a handful of un-embedded memories emit O(n²) identical lines to stderr.
pub fn top_k_neighbors(vectors: &[&[f32]], k: usize) -> Vec<Vec<usize>> {
    let n = vectors.len();
    if n == 0 || k == 0 {
        return vec![Vec::new(); n];
    }

    // Unit-normalised copies. A zero-magnitude (or empty) vector stays empty
    // and is treated as resonating with nothing.
    let units: Vec<Vec<f32>> = vectors
        .iter()
        .map(|v| {
            let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
            if !norm.is_finite() || norm < 1e-12 {
                Vec::new()
            } else {
                v.iter().map(|x| x / norm).collect()
            }
        })
        .collect();

    // (similarity, index) per row, kept worst-first so the weakest entry —
    // the only one that can be displaced — is always at index 0.
    let mut best: Vec<Vec<(f32, usize)>> = vec![Vec::with_capacity(k + 1); n];
    fn offer(row: &mut Vec<(f32, usize)>, sim: f32, idx: usize, k: usize) {
        if row.len() < k {
            row.push((sim, idx));
        } else if sim > row[0].0 {
            row[0] = (sim, idx);
        } else {
            return;
        }
        row.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    }

    for i in 0..n {
        for j in (i + 1)..n {
            // Mismatched widths never resonate; compute_delta skips them
            // anyway, so scoring them 0.0 only avoids pointless work.
            let sim = if units[i].is_empty() || units[j].is_empty() || units[i].len() != units[j].len()
            {
                0.0
            } else {
                units[i].iter().zip(&units[j]).map(|(a, b)| a * b).sum::<f32>()
            };
            offer(&mut best[i], sim, j, k);
            offer(&mut best[j], sim, i, k);
        }
    }

    best.into_iter()
        .map(|mut row| {
            // Back to best-first, matching what the old descending sort yielded.
            row.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
            row.into_iter().map(|(_, j)| j).collect()
        })
        .collect()
}

/// Invariant metrics computed for a memory that remain stable across transformations
#[derive(Debug, Clone)]
pub struct InvariantMetrics {
    /// The δ-invariant: a measure of "irreducibility" - how much information 
    /// this memory carries that can't be derived from its neighbors
    pub delta: f32,
    /// Convergence rate analog: how quickly the memory's wave dynamics settle
    pub convergence_rate: f32,
    /// Irrationality measure: topological complexity from skip link structure
    pub irrationality: f32,
}

/// A cluster of memories with similar δ values (coboundary equivalence candidates)
#[derive(Debug, Clone)]
pub struct DeltaCluster {
    pub representative_delta: f32,
    pub memory_ids: Vec<Uuid>,
    pub coherence: f32, // how tightly clustered the δ values are
}

/// Compute the δ-invariant for a memory based on its relationship to neighbors
///
/// The δ metric captures the "irreducible information content" - how much of this memory's
/// vector cannot be reconstructed from linear combinations of its neighbors via skip links.
/// This mirrors the paper's irrationality measure δ that's invariant under transformations.
pub fn compute_delta(memory: &HyperMemory, neighbors: &[&HyperMemory]) -> f32 {
    if neighbors.is_empty() {
        return 1.0; // isolated memories have maximum δ
    }

    let memory_vec = &memory.vector;
    let dim = memory_vec.len();
    
    if dim == 0 {
        return 0.0;
    }

    // Compute the best linear reconstruction of this memory from neighbors
    let mut min_residual = f32::INFINITY;

    // Try different linear combinations (simplified version of least squares)
    // In the full version, we'd solve the linear system, but this approximation
    // captures the essential idea
    for neighbor in neighbors {
        if neighbor.vector.len() != dim {
            continue;
        }

        // Try simple weighted combinations
        for &weight in &[0.1, 0.3, 0.5, 0.7, 0.9, 1.0] {
            let mut reconstruction = neighbor.vector.clone();
            for val in &mut reconstruction {
                *val *= weight;
            }

            let residual = compute_residual(memory_vec, &reconstruction);
            if residual < min_residual {
                min_residual = residual;
            }
        }
    }

    // For multiple neighbors, try pairwise combinations
    if neighbors.len() >= 2 {
        for i in 0..neighbors.len() {
            for j in (i+1)..neighbors.len() {
                let n1 = &neighbors[i];
                let n2 = &neighbors[j];

                if n1.vector.len() != dim || n2.vector.len() != dim {
                    continue;
                }

                // Try different weight combinations
                for &w1 in &[0.2, 0.4, 0.6, 0.8] {
                    let w2 = 1.0 - w1;
                    let mut reconstruction = vec![0.0; dim];
                    for k in 0..dim {
                        reconstruction[k] = w1 * n1.vector[k] + w2 * n2.vector[k];
                    }

                    let residual = compute_residual(memory_vec, &reconstruction);
                    if residual < min_residual {
                        min_residual = residual;
                    }
                }
            }
        }
    }
    
    // δ is the normalized irreducible residual
    // Higher δ means this memory contains more unique information
    let memory_norm: f32 = memory_vec.iter().map(|x| x * x).sum::<f32>().sqrt();
    if memory_norm == 0.0 {
        return 0.0;
    }
    
    (min_residual / memory_norm).clamp(0.0, 1.0)
}

/// Compute L2 residual between original and reconstruction
fn compute_residual(original: &[f32], reconstruction: &[f32]) -> f32 {
    if original.len() != reconstruction.len() {
        return f32::INFINITY;
    }
    
    original.iter()
        .zip(reconstruction.iter())
        .map(|(a, b)| (a - b) * (a - b))
        .sum::<f32>()
        .sqrt()
}

/// Compute convergence rate analog: how quickly wave dynamics settle
///
/// This is inspired by the paper's convergence rate r = lim(n→∞) (1/n) log|L - p_n/q_n|
/// For memories, we compute how quickly the amplitude/frequency ratio stabilizes
pub fn compute_convergence_rate(memory: &HyperMemory) -> f32 {
    // Convergence rate based on amplitude decay vs frequency
    let freq_norm = if memory.frequency == 0.0 { 0.001 } else { memory.frequency.abs() };
    let decay_to_freq_ratio = memory.decay_rate / freq_norm;
    
    // Higher decay relative to frequency = faster convergence
    // Apply sigmoid to map to [0,1] range
    let normalized = 1.0 / (1.0 + (-decay_to_freq_ratio * 10.0).exp());
    normalized
}

/// Compute irrationality measure from vector entropy.
///
/// Measures the spectral complexity of this memory's hypervector.
/// Higher values indicate more complex, "irrational" interference patterns —
/// vectors with distributed energy across many dimensions are more irrational
/// than vectors concentrated in a few dimensions.
pub fn compute_irrationality(memory: &HyperMemory) -> f32 {
    if memory.vector.is_empty() {
        return 0.0;
    }

    // Compute normalized entropy of the vector's magnitude distribution
    let magnitudes: Vec<f32> = memory.vector.iter().map(|x| x.abs()).collect();
    let total: f32 = magnitudes.iter().sum();
    if total < 1e-8 {
        return 0.0;
    }

    // Shannon entropy of the normalized magnitude distribution
    let entropy: f32 = magnitudes.iter()
        .map(|&m| {
            let p = m / total;
            if p > 1e-10 { -p * p.ln() } else { 0.0 }
        })
        .sum();

    // Normalize by max entropy (uniform distribution) → [0, 1]
    let max_entropy = (memory.vector.len() as f32).ln();
    if max_entropy < 1e-8 {
        return 0.0;
    }

    (entropy / max_entropy).clamp(0.0, 1.0)
}

/// Compute all invariant metrics for a memory
pub fn compute_invariant_metrics(
    memory: &HyperMemory, 
    neighbors: &[&HyperMemory]
) -> InvariantMetrics {
    InvariantMetrics {
        delta: compute_delta(memory, neighbors),
        convergence_rate: compute_convergence_rate(memory),
        irrationality: compute_irrationality(memory),
    }
}

/// Cluster memories by their δ values - memories with similar δ are coboundary equivalent candidates
pub fn cluster_by_delta(engine: &ResonanceEngine, tolerance: f32) -> Vec<DeltaCluster> {
    let all_memories = match engine.store.all_memories() {
        Ok(mems) => mems,
        Err(_) => return Vec::new(),
    };

    if all_memories.is_empty() {
        return Vec::new();
    }

    // The pair loop below is quadratic. On a live HRM that is not slow, it is
    // indistinguishable from a hang — and this runs behind `invariant_clusters`
    // and `detect_cmfs`, which a person types expecting an answer.
    //
    // So it is bounded, and the bound is ANNOUNCED. A cap that quietly analyses
    // a slice of the medium and reports the result as though it covered all of
    // it is worse than being slow: the caller cannot tell a real coboundary
    // structure from an artefact of which memories happened to be included.
    // Override with KANNAKA_DELTA_CLUSTER_MAX; 0 disables the cap entirely.
    let cap = std::env::var("KANNAKA_DELTA_CLUSTER_MAX")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_DELTA_CLUSTER_MAX);
    let all_memories: Vec<&HyperMemory> = if cap > 0 && all_memories.len() > cap {
        eprintln!(
            "[warn] cluster_by_delta: {} memories exceeds the {} cap — analysing the {} \
             most recent only. These clusters describe that subset, not the whole medium. \
             Raise or disable with KANNAKA_DELTA_CLUSTER_MAX.",
            all_memories.len(),
            cap,
            cap
        );
        let mut sorted = all_memories;
        sorted.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        sorted.truncate(cap);
        sorted
    } else {
        all_memories
    };

    // Build neighbor map via cosine similarity (top-5 nearest wavefronts).
    //
    // This is the whole cost of the function, and it used to be avoidably
    // brutal. Per memory it allocated an n-element Vec, called
    // `cosine_similarity` n times — each recomputing BOTH vectors' norms from
    // scratch, so 2n² norm passes over d floats — and then fully sorted n
    // entries to keep 5.
    //
    // Three changes, none of which alter a single returned cluster:
    //   1. Normalise once, up front (n·d). Cosine of unit vectors is a plain
    //      dot product, so the per-pair work drops to one fused pass and the
    //      2n² redundant norm computations disappear.
    //   2. Compute the upper triangle only and mirror it: sim(i,j) == sim(j,i).
    //      Halves the pair count.
    //   3. Keep a fixed 5-slot top-k per row instead of sorting n and taking 5.
    //      Removes the O(n log n) sort AND the n-element allocation per memory.
    //
    // It is still O(n²·d) asymptotically — genuinely fixing that needs an ANN
    // index, which is a much larger change than this function deserves — so
    // there is also an explicit cap below rather than a silent hang.
    // #809: warn ONCE per un-embedded memory, not once per pair.
    let empty_count = all_memories.iter().filter(|m| m.vector.is_empty()).count();
    if empty_count > 0 {
        eprintln!(
            "[warn] cluster_by_delta: {empty_count} of {} memories have empty vectors \
             (missing embeddings?) — they resonate with nothing and cluster as isolated",
            all_memories.len()
        );
    }

    let vectors: Vec<&[f32]> = all_memories.iter().map(|m| m.vector.as_slice()).collect();
    let rows = top_k_neighbors(&vectors, DELTA_NEIGHBORS);

    let mut neighbor_map: HashMap<Uuid, Vec<&HyperMemory>> = HashMap::new();
    for (i, memory) in all_memories.iter().enumerate() {
        let neighbors: Vec<&HyperMemory> =
            rows[i].iter().map(|&j| &*all_memories[j]).collect();
        neighbor_map.insert(memory.id, neighbors);
    }
    
    // Compute δ values for all memories
    let mut delta_values: Vec<(Uuid, f32)> = Vec::new();
    let empty_neighbors = Vec::new();
    
    for memory in &all_memories {
        let neighbors = neighbor_map.get(&memory.id).unwrap_or(&empty_neighbors);
        let delta = compute_delta(memory, neighbors);
        delta_values.push((memory.id, delta));
    }
    
    // Sort by δ value
    delta_values.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    
    // Group into clusters
    let mut clusters = Vec::new();
    let mut current_cluster = Vec::new();
    let mut cluster_deltas = Vec::new();
    let mut current_delta = if !delta_values.is_empty() { delta_values[0].1 } else { 0.0 };
    
    for (id, delta) in delta_values {
        if (delta - current_delta).abs() <= tolerance {
            current_cluster.push(id);
            cluster_deltas.push(delta);
        } else {
            push_delta_cluster(
                &mut clusters,
                std::mem::take(&mut current_cluster),
                std::mem::take(&mut cluster_deltas),
            );
            current_cluster = vec![id];
            cluster_deltas = vec![delta];
            current_delta = delta;
        }
    }
    push_delta_cluster(&mut clusters, current_cluster, cluster_deltas);

    clusters
}

/// Finalize a pending cluster and append it to `clusters`.
/// No-op when `ids` is empty — caller does not need to guard.
fn push_delta_cluster(clusters: &mut Vec<DeltaCluster>, ids: Vec<Uuid>, deltas: Vec<f32>) {
    if ids.is_empty() {
        return;
    }
    let mean_delta = deltas.iter().sum::<f32>() / deltas.len() as f32;
    let variance = deltas.iter()
        .map(|d| (d - mean_delta) * (d - mean_delta))
        .sum::<f32>() / deltas.len() as f32;
    clusters.push(DeltaCluster {
        representative_delta: mean_delta,
        memory_ids: ids,
        coherence: 1.0 / (1.0 + variance * 10.0),
    });
}

/// Compute distance between two memories in invariant space
///
/// This combines their δ values, convergence rates, and irrationality measures
/// to produce a metric that's stable across transformations
pub fn delta_distance(a: &HyperMemory, b: &HyperMemory) -> f32 {
    // Get neighbors for both memories (simplified - in practice we'd maintain this)
    let empty_neighbors = Vec::new();
    let metrics_a = compute_invariant_metrics(a, &empty_neighbors);
    let metrics_b = compute_invariant_metrics(b, &empty_neighbors);
    
    // Weighted distance in 3D invariant space
    let delta_diff = (metrics_a.delta - metrics_b.delta).abs();
    let convergence_diff = (metrics_a.convergence_rate - metrics_b.convergence_rate).abs();
    let irrationality_diff = (metrics_a.irrationality - metrics_b.irrationality).abs();
    
    // Weighted combination (δ is most important)
    0.6 * delta_diff + 0.25 * convergence_diff + 0.15 * irrationality_diff
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::HyperMemory;

    
    #[test]
    fn delta_isolated_memory() {
        let memory = HyperMemory::new(vec![1.0, 2.0, 3.0], "isolated".to_string());
        let neighbors = Vec::new();
        let delta = compute_delta(&memory, &neighbors);
        assert_eq!(delta, 1.0, "isolated memory should have maximum δ");
    }
    
    #[test]
    fn delta_with_similar_neighbor() {
        let memory = HyperMemory::new(vec![1.0, 2.0, 3.0], "target".to_string());
        let similar_memory = HyperMemory::new(vec![1.1, 2.1, 3.1], "similar".to_string());
        let neighbors = vec![&similar_memory];
        
        let delta = compute_delta(&memory, &neighbors);
        assert!(delta < 1.0 && delta > 0.0, "similar neighbor should reduce δ");
    }
    
    #[test]
    fn convergence_rate_computation() {
        let mut memory = HyperMemory::new(vec![1.0; 10], "test".to_string());
        memory.frequency = 0.1;
        memory.decay_rate = 0.01;
        
        let rate = compute_convergence_rate(&memory);
        assert!(rate >= 0.0 && rate <= 1.0, "convergence rate should be in [0,1]");
    }
    
    #[test]
    fn irrationality_uniform_vector_is_high() {
        // Uniform distribution = max entropy = high irrationality
        let memory = HyperMemory::new(vec![1.0; 10], "test".to_string());
        let irrationality = compute_irrationality(&memory);
        assert!(irrationality > 0.9, "uniform vector should have high irrationality: {irrationality}");
    }

    #[test]
    fn irrationality_sparse_vector_is_low() {
        // Sparse/concentrated vector = low entropy = low irrationality
        let mut vec = vec![0.0; 10];
        vec[0] = 1.0;
        let memory = HyperMemory::new(vec, "test".to_string());
        let irrationality = compute_irrationality(&memory);
        assert!(irrationality < 0.5, "sparse vector should have low irrationality: {irrationality}");
    }

    #[test]
    fn irrationality_empty_vector() {
        let memory = HyperMemory::new(vec![], "test".to_string());
        let irrationality = compute_irrationality(&memory);
        assert_eq!(irrationality, 0.0, "empty vector should give zero irrationality");
    }
    
    #[test]
    fn delta_distance_identical_memories() {
        let a = HyperMemory::new(vec![1.0, 2.0, 3.0], "a".to_string());
        let b = HyperMemory::new(vec![1.0, 2.0, 3.0], "b".to_string());
        
        let distance = delta_distance(&a, &b);
        assert!(distance < 0.1, "identical memories should have small distance");
    }

    // ---- #809 / #810: the neighbour build ---------------------------------

    /// Reference implementation: exactly what the pre-fix inline code did.
    /// The optimisation must be behaviour-preserving, so the cheapest way to
    /// say that is to keep the slow version and compare against it.
    fn naive_top_k(vectors: &[&[f32]], k: usize) -> Vec<Vec<usize>> {
        (0..vectors.len())
            .map(|i| {
                let mut sims: Vec<(usize, f32)> = (0..vectors.len())
                    .filter(|j| *j != i)
                    .map(|j| (j, crate::wave::cosine_similarity(vectors[i], vectors[j])))
                    .collect();
                sims.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                sims.iter().take(k).map(|(j, _)| *j).collect()
            })
            .collect()
    }

    #[test]
    fn top_k_matches_the_naive_full_sort_it_replaced() {
        // Deterministic, well-separated vectors so there are no similarity
        // ties — with ties the two implementations may legitimately disagree
        // on ORDER among equals, and asserting on that would be asserting on
        // sort stability rather than on neighbours.
        let raw: Vec<Vec<f32>> = (0..24)
            .map(|i| {
                let mut v = vec![0.0f32; 8];
                v[i % 8] = 1.0;
                v[(i + 3) % 8] = 0.1 + (i as f32) * 0.017;
                v
            })
            .collect();
        let vectors: Vec<&[f32]> = raw.iter().map(|v| v.as_slice()).collect();
        assert_eq!(top_k_neighbors(&vectors, 5), naive_top_k(&vectors, 5));
    }

    #[test]
    fn neighbours_are_best_first() {
        // a is nearest to b, then c, then the orthogonal d.
        let a = vec![1.0f32, 0.0, 0.0];
        let b = vec![0.99f32, 0.14, 0.0];
        let c = vec![0.7f32, 0.7, 0.0];
        let d = vec![0.0f32, 0.0, 1.0];
        let raw = [a, b, c, d];
        let vectors: Vec<&[f32]> = raw.iter().map(|v| v.as_slice()).collect();
        assert_eq!(top_k_neighbors(&vectors, 3)[0], vec![1, 2, 3]);
    }

    #[test]
    fn an_empty_vector_resonates_with_nothing_and_does_not_panic() {
        // #809: the guard bypass meant this path was never exercised at all.
        let a = vec![1.0f32, 0.0];
        let empty: Vec<f32> = Vec::new();
        let b = vec![0.9f32, 0.1];
        let raw = [a, empty, b];
        let vectors: Vec<&[f32]> = raw.iter().map(|v| v.as_slice()).collect();
        let rows = top_k_neighbors(&vectors, 2);
        // The real neighbour outranks the empty one for everybody.
        assert_eq!(rows[0][0], 2);
        assert_eq!(rows[2][0], 0);
        // The empty vector still gets a row rather than being dropped.
        assert_eq!(rows[1].len(), 2);
    }

    #[test]
    fn a_zero_magnitude_vector_is_treated_like_an_empty_one() {
        // Normalising this would be a divide by zero and produce NaNs, which
        // then poison every comparison they touch.
        let raw = [vec![0.0f32, 0.0], vec![1.0f32, 0.0], vec![0.8f32, 0.6]];
        let vectors: Vec<&[f32]> = raw.iter().map(|v| v.as_slice()).collect();
        let rows = top_k_neighbors(&vectors, 2);
        assert_eq!(rows[1][0], 2, "the two real vectors must find each other");
        assert!(rows[0].iter().all(|j| *j < 3));
    }

    #[test]
    fn mismatched_widths_do_not_panic_or_resonate() {
        let raw = [vec![1.0f32, 0.0], vec![1.0f32, 0.0, 0.0], vec![0.9f32, 0.1]];
        let vectors: Vec<&[f32]> = raw.iter().map(|v| v.as_slice()).collect();
        let rows = top_k_neighbors(&vectors, 2);
        assert_eq!(rows[0][0], 2, "the same-width partner must win over the wider one");
    }

    #[test]
    fn degenerate_inputs_are_shaped_correctly() {
        let raw = [vec![1.0f32, 0.0]];
        let one: Vec<&[f32]> = raw.iter().map(|v| v.as_slice()).collect();
        assert_eq!(top_k_neighbors(&one, 5), vec![Vec::<usize>::new()]);
        assert_eq!(top_k_neighbors(&[], 5), Vec::<Vec<usize>>::new());
        assert_eq!(top_k_neighbors(&one, 0), vec![Vec::<usize>::new()]);
    }

    #[test]
    fn k_is_a_ceiling_not_a_target() {
        let raw = [vec![1.0f32, 0.0], vec![0.0f32, 1.0], vec![0.7f32, 0.7]];
        let vectors: Vec<&[f32]> = raw.iter().map(|v| v.as_slice()).collect();
        // Only two other vectors exist, so a k of 5 yields 2, not 5.
        assert!(top_k_neighbors(&vectors, 5).iter().all(|r| r.len() == 2));
    }
}

#[cfg(test)]
mod perf_probe {
    use super::*;

    /// Not a correctness test — a measurement, kept out of the normal run.
    /// `cargo test --release --lib perf_probe -- --ignored --nocapture`
    #[test]
    #[ignore = "perf probe, not a correctness test: cargo test --release --lib perf_probe -- --ignored --nocapture"]
    fn measure_against_naive() {
        use std::time::Instant;
        let n = 1500usize;
        let d = 256usize;
        let raw: Vec<Vec<f32>> = (0..n)
            .map(|i| (0..d).map(|k| (((i * 31 + k * 17) % 97) as f32) / 97.0 - 0.5).collect())
            .collect();
        let vectors: Vec<&[f32]> = raw.iter().map(|v| v.as_slice()).collect();

        let t0 = Instant::now();
        let fast = top_k_neighbors(&vectors, 5);
        let fast_ms = t0.elapsed().as_millis();

        let t1 = Instant::now();
        let naive: Vec<Vec<usize>> = (0..n)
            .map(|i| {
                let mut sims: Vec<(usize, f32)> = (0..n)
                    .filter(|j| *j != i)
                    .map(|j| (j, crate::wave::cosine_similarity(vectors[i], vectors[j])))
                    .collect();
                sims.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                sims.iter().take(5).map(|(j, _)| *j).collect()
            })
            .collect();
        let naive_ms = t1.elapsed().as_millis();

        println!("n={n} d={d}  new={fast_ms}ms  old={naive_ms}ms");

        // Compare the SIMILARITY PROFILE, not the index list. With synthetic
        // data there are exact ties, and two correct top-k implementations may
        // legitimately break a tie differently. What must match is the set of
        // scores selected, in order.
        let score = |i: usize, j: usize| crate::wave::cosine_similarity(vectors[i], vectors[j]);
        let mut differing_indices = 0usize;
        for i in 0..n {
            let a: Vec<f32> = fast[i].iter().map(|&j| score(i, j)).collect();
            let b: Vec<f32> = naive[i].iter().map(|&j| score(i, j)).collect();
            for (x, y) in a.iter().zip(&b) {
                assert!((x - y).abs() < 1e-6, "row {i}: picked a genuinely worse neighbour: {a:?} vs {b:?}");
            }
            if fast[i] != naive[i] {
                differing_indices += 1;
            }
        }
        println!("rows whose tie-break order differs: {differing_indices}/{n}");
    }
}
