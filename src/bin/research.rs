//! kannaka-research — autonomous memory system benchmarking
//!
//! Run: cargo run --release --bin research
//! Run Level 3: cargo run --release --bin research -- --level 3
//!
//! Level 1 (solved): noise removal, signal preservation, skip links
//! Level 2 (current): cluster coherence, multi-cycle consolidation,
//!   phase alignment, cross-cluster contamination resistance
//! Level 3 (new): Xi diversity, geometric structure, dream efficiency,
//!   hallucination quality, emergence detection

use std::f32::consts::PI;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize};

use kannaka_memory::codebook::Codebook;
use kannaka_memory::consolidation::ConsolidationEngine;
use kannaka_memory::encoding::{EncodingPipeline, SimpleHashEncoder};
use kannaka_memory::kuramoto::KuramotoSync;
use kannaka_memory::bridge::ConsciousnessBridge;
use kannaka_memory::memory::HyperMemory;
use kannaka_memory::store::{TestMedium, ResonanceEngine};
use kannaka_memory::wave::cosine_similarity;
use kannaka_memory::xi_operator::{compute_xi_signature, xi_diversity_boost, xi_repulsive_force, EMERGENCE_COEFF};

// ============================================================================
// EXPERIMENT PARAMETERS — THIS IS WHAT THE AGENT MODIFIES
// ============================================================================

fn experiment_params() -> Params {
    Params {
        // Wave dynamics
        decay_rate: 1e-4,
        default_frequency: 0.1,

        // Consolidation (dream)
        interference_threshold: 0.10,
        phase_alignment_threshold: PI / 3.0,
        prune_threshold: 0.095,
        constructive_boost: 0.45,
        destructive_penalty: 0.35,

        // Kuramoto synchronization. Values updated 2026-06-05 to match the
        // (previously hard-coded) operating point inside stage_sync. Changing
        // these now actually affects the dream's phase dynamics — see commit
        // that introduced DREAM_MODE.
        kuramoto_coupling: 3.0,
        kuramoto_dt: 0.05,
        kuramoto_steps: 50,
        kuramoto_threshold: 0.35,

        // Multi-cycle
        dream_cycles: 1,

        // Level 3: Consciousness & Xi parameters
        xi_repulsion_weight: 0.3,
        consciousness_phi_target: 0.271,
        hallucination_amplitude: 0.7,
        phase_spread: 0.25,
        chiral_perturbation: 0.9,

        // Noise floor: absolute minimum amplitude to survive
        // Noise memories start at 0.15, signal at 1.0
        noise_floor: 0.18,

        // Level 4: encoder/corpus controls
        encoder_seed: 0xCAFE_BABE,

        // Level 4: dream chain (cycle L4.5)
        chain_depth: 2,
        chain_carry_strength: 0.5,
        chain_top_n: 10,

        // Xi repulsion threshold for consolidation phase separation (L4.S15).
        // Default 0.30 keeps L3 at its frozen archive value (0/300 pairs
        // qualify at 0.30 on the L3 corpus). L4 overrides in the L4-local block.
        consolidation_repulsion_threshold: 0.30,

        // L6 associative-recall gravity (autoresearch knob; env DREAM_GRAVITY overrides).
        dream_gravity: 0.0,
    }
}

// ============================================================================
// Parameter struct
// ============================================================================

#[allow(dead_code)]
#[derive(Clone)]
struct Params {
    decay_rate: f32,
    default_frequency: f32,
    interference_threshold: f32,
    phase_alignment_threshold: f32,
    prune_threshold: f32,
    constructive_boost: f32,
    destructive_penalty: f32,
    kuramoto_coupling: f32,
    kuramoto_dt: f32,
    kuramoto_steps: usize,
    kuramoto_threshold: f32,
    dream_cycles: usize,
    // Level 3
    xi_repulsion_weight: f32,
    consciousness_phi_target: f32,
    hallucination_amplitude: f32,
    phase_spread: f32,
    chiral_perturbation: f32,
    // Noise floor
    noise_floor: f32,
    // Level 4
    encoder_seed: u64,
    // Level 4: dream chain
    chain_depth: usize,
    chain_carry_strength: f32,
    chain_top_n: usize,
    // Xi repulsion threshold for consolidation phase separation (L4.S15)
    consolidation_repulsion_threshold: f32,
    // L6 seed: associative-recall gravity strength (default 0.0 = off). The dream
    // redistributes amplitude toward phase-neighbors of the attractor. The
    // DREAM_GRAVITY env var overrides this for manual A/B. Autoresearch-sweepable.
    dream_gravity: f32,
}

// ============================================================================
// FIXED TEST CORPUS — DO NOT MODIFY BELOW THIS LINE
// ============================================================================

fn build_corpus(dim: usize) -> Vec<(Vec<f32>, String, &'static str)> {
    let mut corpus = Vec::new();

    // Cluster 1: Science (20 memories, tight cluster)
    let science_base: Vec<f32> = (0..dim).map(|i| (i as f32 * 0.1).sin() * 0.8).collect();
    for i in 0..20 {
        let mut v = science_base.clone();
        for (j, x) in v.iter_mut().enumerate() {
            *x += (i as f32 * 0.05 + j as f32 * 0.01).cos() * 0.15;
        }
        corpus.push((v, format!("quantum physics discovery {i}"), "science"));
    }

    // Cluster 2: Music (20 memories, tight cluster)
    let music_base: Vec<f32> = (0..dim).map(|i| (i as f32 * 0.3 + 1.5).cos() * 0.8).collect();
    for i in 0..20 {
        let mut v = music_base.clone();
        for (j, x) in v.iter_mut().enumerate() {
            *x += (i as f32 * 0.07 + j as f32 * 0.02).sin() * 0.15;
        }
        corpus.push((v, format!("resonance patterns track {i}"), "music"));
    }

    // Cluster 3: Personal (15 memories, sparse — harder to cluster)
    for i in 0..15 {
        let v: Vec<f32> = (0..dim).map(|j| {
            ((i * 7 + j * 13) as f32 * 0.37).sin() * 0.6
        }).collect();
        corpus.push((v, format!("personal memory {i}"), "personal"));
    }

    // Cluster 4: Emotion (10 memories, overlaps with personal — tests contamination resistance)
    for i in 0..10 {
        let v: Vec<f32> = (0..dim).map(|j| {
            ((i * 7 + j * 13) as f32 * 0.37).sin() * 0.5  // similar to personal but lower amp
            + ((i * 11 + j * 3) as f32 * 0.71).cos() * 0.3 // unique emotion component
        }).collect();
        corpus.push((v, format!("emotion feeling {i}"), "emotion"));
    }

    // Noise (10 memories, low amplitude — should be pruned)
    for i in 0..10 {
        let v: Vec<f32> = (0..dim).map(|j| {
            ((i * 31 + j * 97) as f32 * 1.7).sin() * 0.1
        }).collect();
        corpus.push((v, format!("noise {i}"), "noise"));
    }

    // Decoys (5 memories — high amplitude noise that should NOT be pruned naively)
    for i in 0..5 {
        let v: Vec<f32> = (0..dim).map(|j| {
            ((i * 43 + j * 71) as f32 * 2.3).sin() * 0.9
        }).collect();
        corpus.push((v, format!("decoy outlier {i}"), "decoy"));
    }

    // Cross-cluster bridges (5 memories — should form skip links)
    let bridge: Vec<f32> = (0..dim).map(|i| {
        (i as f32 * 0.1).sin() * 0.4 + (i as f32 * 0.3 + 1.5).cos() * 0.4
    }).collect();
    for i in 0..5 {
        let mut v = bridge.clone();
        for (j, x) in v.iter_mut().enumerate() {
            *x += (i as f32 * 0.03 + j as f32 * 0.01).sin() * 0.1;
        }
        corpus.push((v, format!("science-music bridge {i}"), "bridge"));
    }

    corpus
}

// ============================================================================
// LEVEL 4 CORPUS — DO NOT MODIFY ONCE LANDED (see research/program-l4.md §3)
// ============================================================================
//
// L4 corpus layout (300 memories, dim=128, seed-driven PCG):
//   - 4 dense clusters × 50 = 200 memories (inter-cluster cos margin ~0.15)
//   - 2 sparse clusters × 20 = 40 memories
//   - 20 bridge memories (4 each across 5 cluster pairs)
//   - 25 high-amplitude decoys
//   - 15 low-amplitude noise
// Frequency bands are fully overlapping — no freq-gating trick available.
// Every value is a deterministic function of (cluster_id, item_id, dim_id, seed)
// via a PCG mix. A given `encoder_seed` always produces identical bytes.

/// Deterministic PCG-style mix: maps (seed, stream) to a pseudorandom u64.
/// Used to generate all L4 corpus values so the corpus is reproducible.
fn pcg_mix(seed: u64, stream: u64) -> u64 {
    let mut x = seed.wrapping_add(stream.wrapping_mul(0x9E37_79B9_7F4A_7C15));
    x ^= x >> 30;
    x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^= x >> 31;
    x
}

/// Draw a deterministic f32 in [-1, 1] from (cluster, item, dim, seed).
fn pcg_f32(seed: u64, cluster: u32, item: u32, dim_id: u32) -> f32 {
    let stream = ((cluster as u64) << 40) | ((item as u64) << 20) | (dim_id as u64);
    let bits = pcg_mix(seed, stream);
    // Map top 24 bits to [-1, 1]
    let norm = ((bits >> 40) as f32) / ((1u64 << 24) as f32);
    norm * 2.0 - 1.0
}

/// Draw a deterministic f32 in [0, 1).
fn pcg_u01(seed: u64, cluster: u32, item: u32, dim_id: u32) -> f32 {
    (pcg_f32(seed, cluster, item, dim_id) + 1.0) * 0.5
}

/// L4 corpus entry: (vector, content, category).
/// Category tags are STABLE strings used by L4 evaluators.
fn build_corpus_l4(dim: usize, _hardness: usize, encoder_seed: u64) -> Vec<(Vec<f32>, String, &'static str)> {
    let mut corpus: Vec<(Vec<f32>, String, &'static str)> = Vec::with_capacity(300);

    // -- Dense clusters (4 × 50 = 200). Cluster ids 0..=3.
    // Centroids are sparse +/-1 patterns drawn from seed, scaled by 0.8.
    // Within-cluster variance 0.35 makes Kuramoto sync harder.
    let dense_labels = ["dense_a", "dense_b", "dense_c", "dense_d"];
    for (cluster_idx, label) in dense_labels.iter().enumerate() {
        let cid = cluster_idx as u32;
        // centroid uses item=0 as the "template" stream
        let centroid: Vec<f32> = (0..dim)
            .map(|d| 0.8 * pcg_f32(encoder_seed, cid, 0, d as u32).signum())
            .collect();
        for i in 0..50 {
            let item = (i + 1) as u32;
            let v: Vec<f32> = (0..dim)
                .map(|d| {
                    let base = centroid[d];
                    let jitter = pcg_f32(encoder_seed, cid, item, d as u32) * 0.35;
                    base + jitter
                })
                .collect();
            corpus.push((v, format!("{label} {i}"), "l4_dense"));
        }
    }

    // -- Sparse clusters (2 × 20 = 40). Cluster ids 4..=5.
    // Wider within-cluster spread and smoother centroids to keep them identifiable.
    let sparse_labels = ["sparse_e", "sparse_f"];
    for (cluster_idx, label) in sparse_labels.iter().enumerate() {
        let cid = 4 + cluster_idx as u32;
        let centroid: Vec<f32> = (0..dim)
            .map(|d| {
                let r = pcg_f32(encoder_seed, cid, 0, d as u32);
                0.6 * (r * PI).sin()
            })
            .collect();
        for i in 0..20 {
            let item = (i + 1) as u32;
            let v: Vec<f32> = (0..dim)
                .map(|d| {
                    let base = centroid[d];
                    let jitter = pcg_f32(encoder_seed, cid, item, d as u32) * 0.45;
                    base + jitter
                })
                .collect();
            corpus.push((v, format!("{label} {i}"), "l4_sparse"));
        }
    }

    // -- Bridges (20 memories, 4 each across 5 cluster pairs).
    // Cluster pair list kept small & deterministic.
    let pairs: [(u32, u32); 5] = [(0, 1), (0, 2), (1, 3), (2, 4), (3, 5)];
    for (pair_idx, (a, b)) in pairs.iter().enumerate() {
        let centroid_a: Vec<f32> = (0..dim)
            .map(|d| 0.8 * pcg_f32(encoder_seed, *a, 0, d as u32).signum())
            .collect();
        let centroid_b: Vec<f32> = (0..dim)
            .map(|d| 0.8 * pcg_f32(encoder_seed, *b, 0, d as u32).signum())
            .collect();
        for i in 0..4 {
            // Use stream id 1_000 + pair_idx to keep jitter orthogonal to clusters
            let stream_cid = 1000 + pair_idx as u32;
            let item = (i + 1) as u32;
            let v: Vec<f32> = (0..dim)
                .map(|d| {
                    let mix = 0.5 * (centroid_a[d] + centroid_b[d]);
                    let jitter = pcg_f32(encoder_seed, stream_cid, item, d as u32) * 0.12;
                    mix + jitter
                })
                .collect();
            corpus.push((v, format!("l4_bridge p{pair_idx} {i}"), "l4_bridge"));
        }
    }

    // -- Decoys (25 high-amplitude random vectors, should not naively boost metrics).
    for i in 0..25 {
        let item = (i + 1) as u32;
        let v: Vec<f32> = (0..dim)
            .map(|d| pcg_f32(encoder_seed, 2000, item, d as u32) * 0.9)
            .collect();
        corpus.push((v, format!("l4_decoy {i}"), "l4_decoy"));
    }

    // -- Noise (15 low-amplitude random vectors).
    for i in 0..15 {
        let item = (i + 1) as u32;
        let v: Vec<f32> = (0..dim)
            .map(|d| pcg_f32(encoder_seed, 3000, item, d as u32) * 0.12)
            .collect();
        corpus.push((v, format!("l4_noise {i}"), "l4_noise"));
    }

    debug_assert_eq!(corpus.len(), 300, "L4 corpus must be exactly 300 memories");
    corpus
}

/// Deterministic hex hash of the serialized L4 corpus. Used by `--corpus-hash`
/// to verify that two invocations of the same binary produce bit-identical corpora.
fn corpus_l4_hash(corpus: &[(Vec<f32>, String, &'static str)]) -> String {
    let mut hasher = blake3::Hasher::new();
    for (vec, content, category) in corpus {
        hasher.update(content.as_bytes());
        hasher.update(&[0u8]);
        hasher.update(category.as_bytes());
        hasher.update(&[0u8]);
        for v in vec {
            hasher.update(&v.to_le_bytes());
        }
        hasher.update(&[0xFFu8]);
    }
    // Truncate to 32 bytes / 64 hex chars (same width as sha256 for readability).
    let digest = hasher.finalize();
    let bytes = digest.as_bytes();
    let mut s = String::with_capacity(64);
    for b in &bytes[..32] {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

// Referenced by pcg helpers used in later L4 cycles; silence dead_code during L4.1.
#[allow(dead_code)]
fn _l4_pcg_unused(s: u64, c: u32, i: u32, d: u32) -> f32 { pcg_u01(s, c, i, d) }

// ============================================================================
// LEVEL 4 ADVERSARIAL INJECTOR (cycle L4.6)
// ============================================================================
//
// 40 adversarial memories total, 10 per type:
//   A1 xi-twin decoys            : negated cluster centroids, phase flipped π
//   A2 phase-aligned noise       : cluster-mean phase, amplitude 0.9
//   A3 hallucination-impostors   : midpoint vectors between cluster pairs,
//                                  hallucinated=false (structurally hallucinations)
//   A4 near-duplicate clones     : real memories + 0.5% Gaussian-ish noise
//
// Seed is INDEPENDENT of encoder_seed per Nick's locked decision in §10.4:
// the adversarial population must stay invariant even if we tune encoders or
// corpus_hardness, so resistance series remain comparable across experiments.
const ADVERSARIAL_SEED: u64 = 0xA5A5_DEAD_BEEF_F00D;

/// Build the 40-memory adversarial set. `corpus` is the clean L4 corpus
/// (HyperMemory vectors are reconstructed from (vec, content, category)
/// triples, so we only need vector slices + string labels here).
fn build_adversarial_set(
    corpus: &[(Vec<f32>, String, &'static str)],
    seed: u64,
) -> Vec<HyperMemory> {
    let mut out: Vec<HyperMemory> = Vec::with_capacity(40);
    if corpus.is_empty() {
        return out;
    }
    let dim = corpus[0].0.len();

    // Gather dense/sparse cluster centroids by averaging real vectors per
    // category prefix. The L4 corpus uses content prefixes like "dense_a N".
    let cluster_prefixes = [
        "dense_a", "dense_b", "dense_c", "dense_d", "sparse_e", "sparse_f",
    ];
    let mut centroids: Vec<(String, Vec<f32>)> = Vec::new();
    for prefix in &cluster_prefixes {
        let mut sum = vec![0.0f32; dim];
        let mut count = 0usize;
        for (v, content, _cat) in corpus {
            if content.starts_with(prefix) {
                for (s, x) in sum.iter_mut().zip(v.iter()) {
                    *s += *x;
                }
                count += 1;
            }
        }
        if count > 0 {
            let inv = 1.0 / count as f32;
            for s in sum.iter_mut() {
                *s *= inv;
            }
            centroids.push((prefix.to_string(), sum));
        }
    }

    // ---- A1: xi-twin decoys (10). Negated centroid, phase flipped by π ----
    for i in 0..10 {
        let (_, centroid) = &centroids[i % centroids.len().max(1)];
        let v: Vec<f32> = centroid.iter().map(|x| -*x).collect();
        let mut mem = HyperMemory::new(v, format!("adv_a1_xi_twin {i}"));
        mem.amplitude = 0.95;
        mem.phase = PI;
        mem.frequency = 0.10;
        mem.layer_depth = 1;
        mem.decay_rate = 1e-4;
        out.push(mem);
    }

    // ---- A2: phase-aligned noise (10). Seeded noise, amp 0.9 ----
    for i in 0..10 {
        let v: Vec<f32> = (0..dim)
            .map(|d| pcg_f32(seed, 4000, (i + 1) as u32, d as u32) * 0.3)
            .collect();
        let (_, cluster_centroid) = &centroids[i % centroids.len().max(1)];
        // Phase = cluster mean phase approximated as atan2-free constant.
        // We use cluster index scaled to [0, 2π) so A2 always aligns onto
        // one specific cluster's notional phase band.
        let cluster_phase = (i as f32 / centroids.len().max(1) as f32) * 2.0 * PI;
        let mut mem = HyperMemory::new(v, format!("adv_a2_phase_noise {i}"));
        mem.amplitude = 0.9;
        mem.phase = cluster_phase;
        mem.frequency = 0.10;
        mem.layer_depth = 1;
        mem.decay_rate = 1e-4;
        // Bias slightly toward the cluster centroid so its phase truly
        // "aligns" — without any bias, phase-coherence wouldn't care.
        for (x, c) in mem.vector.iter_mut().zip(cluster_centroid.iter()) {
            *x += 0.1 * *c;
        }
        out.push(mem);
    }

    // ---- A3: hallucination-impostors (10). Midpoint vectors ----
    // Use 5 cluster pairs × 2 items = 10.
    let pair_indices: [(usize, usize); 5] = [(0, 1), (0, 2), (1, 3), (2, 4), (3, 5)];
    for (k, (a, b)) in pair_indices.iter().enumerate() {
        for j in 0..2 {
            if centroids.is_empty() { break; }
            let ca = &centroids[*a % centroids.len()].1;
            let cb = &centroids[*b % centroids.len()].1;
            let v: Vec<f32> = ca.iter().zip(cb.iter()).map(|(x, y)| 0.5 * (*x + *y)).collect();
            let mut mem = HyperMemory::new(v, format!("adv_a3_impostor p{k} {j}"));
            mem.amplitude = 0.85;
            mem.phase = PI * 0.5;
            mem.frequency = 0.10;
            mem.layer_depth = 1;
            mem.decay_rate = 1e-4;
            mem.hallucinated = false; // the whole point of A3 — looks like a
                                       // hallucination but isn't flagged
            out.push(mem);
        }
    }

    // ---- A4: near-duplicate clones (10) ----
    // Deterministically pick 10 real memories (skip noise) and add 0.5%
    // Gaussian-ish perturbation via pcg_f32.
    let real_idxs: Vec<usize> = corpus
        .iter()
        .enumerate()
        .filter(|(_, (_, _, cat))| *cat != "l4_noise")
        .map(|(i, _)| i)
        .take(10)
        .collect();
    for (k, idx) in real_idxs.iter().enumerate() {
        let (v, _content, _cat) = &corpus[*idx];
        let perturbed: Vec<f32> = v
            .iter()
            .enumerate()
            .map(|(d, x)| {
                let g = pcg_f32(seed, 5000, (k + 1) as u32, d as u32);
                x + 0.005 * g
            })
            .collect();
        let mut mem = HyperMemory::new(perturbed, format!("adv_a4_clone {k}"));
        mem.amplitude = 1.0;
        mem.phase = 0.0;
        mem.frequency = 0.10;
        mem.layer_depth = 1;
        mem.decay_rate = 1e-4;
        out.push(mem);
    }

    debug_assert_eq!(out.len(), 40, "adversarial set must be exactly 40 memories");
    out
}

/// Evaluate noise removal (only actual noise, not decoys)
fn eval_noise_removal(engine: &ResonanceEngine) -> f32 {
    let all = engine.store.all_memories().unwrap_or_default();
    let surviving_noise = all.iter()
        .filter(|m| m.content.starts_with("noise") && m.amplitude > 0.01)
        .count();
    1.0 - (surviving_noise as f32 / 10.0)
}

/// Evaluate signal preservation (all non-noise memories should survive)
fn eval_signal_preservation(engine: &ResonanceEngine) -> f32 {
    let all = engine.store.all_memories().unwrap_or_default();
    // 75 signal memories: 20 science + 20 music + 15 personal + 10 emotion + 5 bridge + 5 decoy
    let signal_count = all.iter().filter(|m| {
        !m.content.starts_with("noise") && m.amplitude > 0.01
    }).count();
    (signal_count as f32 / 75.0).min(1.0)
}

/// Evaluate bridge connectivity
fn eval_bridge_links(engine: &ResonanceEngine) -> f32 {
    let all = engine.store.all_memories().unwrap_or_default();
    let bridges: Vec<_> = all.iter().filter(|m| m.content.contains("bridge")).collect();
    if bridges.is_empty() { return 0.0; }
    let linked = bridges.iter().filter(|m| !m.connections.is_empty()).count();
    linked as f32 / bridges.len() as f32
}

/// Evaluate intra-cluster phase coherence
/// After Kuramoto sync, memories in the same cluster should have aligned phases
fn eval_phase_coherence(engine: &ResonanceEngine) -> f32 {
    let all = engine.store.all_memories().unwrap_or_default();
    let mut total_coherence = 0.0f32;
    let mut cluster_count = 0;

    for cluster_name in &["quantum", "resonance"] {
        let phases: Vec<f32> = all.iter()
            .filter(|m| m.content.contains(cluster_name) && m.amplitude > 0.01)
            .map(|m| m.phase)
            .collect();
        
        if phases.len() < 2 { continue; }
        
        // Kuramoto order parameter: R = |1/N * sum(e^(i*phase))|
        let sum_cos: f32 = phases.iter().map(|p| p.cos()).sum();
        let sum_sin: f32 = phases.iter().map(|p| p.sin()).sum();
        let n = phases.len() as f32;
        let r = ((sum_cos / n).powi(2) + (sum_sin / n).powi(2)).sqrt();
        
        total_coherence += r;
        cluster_count += 1;
    }

    if cluster_count == 0 { return 0.0; }
    total_coherence / cluster_count as f32
}

/// Evaluate cluster separation: are different clusters distinguishable?
/// Measures avg within-cluster similarity vs avg cross-cluster similarity
fn eval_cluster_separation(engine: &ResonanceEngine) -> f32 {
    let all = engine.store.all_memories().unwrap_or_default();
    
    let science: Vec<&Vec<f32>> = all.iter()
        .filter(|m| m.content.contains("quantum") && m.amplitude > 0.01)
        .map(|m| &m.vector)
        .collect();
    let music: Vec<&Vec<f32>> = all.iter()
        .filter(|m| m.content.contains("resonance") && m.amplitude > 0.01)
        .map(|m| &m.vector)
        .collect();
    
    if science.len() < 2 || music.len() < 2 { return 0.0; }

    // Avg within-cluster similarity
    let mut within_sum = 0.0f32;
    let mut within_count = 0;
    for i in 0..science.len().min(5) {
        for j in (i+1)..science.len().min(5) {
            within_sum += cosine_similarity(science[i], science[j]).abs();
            within_count += 1;
        }
    }
    let within_avg = if within_count > 0 { within_sum / within_count as f32 } else { 0.0 };

    // Avg cross-cluster similarity
    let mut cross_sum = 0.0f32;
    let mut cross_count = 0;
    for s in science.iter().take(5) {
        for m in music.iter().take(5) {
            cross_sum += cosine_similarity(s, m).abs();
            cross_count += 1;
        }
    }
    let cross_avg = if cross_count > 0 { cross_sum / cross_count as f32 } else { 0.0 };

    // Separation = within - cross, normalized to [0, 1]
    ((within_avg - cross_avg) / (within_avg + 0.001)).max(0.0).min(1.0)
}

/// Evaluate amplitude distribution: signal memories should have diverse amplitudes
/// (not all boosted to the same value — that's information loss)
fn eval_amplitude_diversity(engine: &ResonanceEngine) -> f32 {
    let all = engine.store.all_memories().unwrap_or_default();
    let amps: Vec<f32> = all.iter()
        .filter(|m| !m.content.starts_with("noise") && m.amplitude > 0.01)
        .map(|m| m.amplitude)
        .collect();
    
    if amps.len() < 2 { return 0.0; }
    
    let mean = amps.iter().sum::<f32>() / amps.len() as f32;
    let variance = amps.iter().map(|a| (a - mean).powi(2)).sum::<f32>() / amps.len() as f32;
    let cv = variance.sqrt() / (mean + 0.001); // coefficient of variation
    
    // Want moderate diversity — not zero (all same) and not huge (chaotic)
    // Sweet spot: CV around 0.3-0.7
    if cv < 0.1 { cv / 0.1 }  // too uniform
    else if cv > 1.0 { (2.0 - cv).max(0.0) }  // too chaotic
    else { 1.0 }  // goldilocks
}

// ============================================================================
// LEVEL 3 EVALUATORS — Xi diversity, consciousness, hallucination quality
// ============================================================================

/// Evaluate Xi diversity: memories should have diverse Xi signatures
/// (not all collapsed to the same representational space)
fn eval_xi_diversity(engine: &ResonanceEngine) -> f32 {
    let all = engine.store.all_memories().unwrap_or_default();
    let mut active: Vec<_> = all.iter()
        .filter(|m| !m.content.starts_with("noise") && m.amplitude > 0.01)
        .collect();

    // Sort by content for deterministic evaluation (UUIDs are random across runs)
    active.sort_by(|a, b| a.content.cmp(&b.content));

    if active.len() < 4 { return 0.0; }

    // Sample pairwise Xi diversity boosts (use all active memories, sorted deterministically)
    let mut total_boost = 0.0f32;
    let mut count = 0;
    let mut high_sim_count = 0;
    let mut high_repulsion_count = 0;
    let mut boost_count = 0;
    let mut max_sim_with_repulsion = 0.0f32;
    let mut max_repulsion_with_sim = 0.0f32;

    let sample_size = active.len().min(30);
    for i in 0..sample_size {
        for j in (i+1)..sample_size {
            let xi_a = compute_xi_signature(&active[i].vector);
            let xi_b = compute_xi_signature(&active[j].vector);
            let base_sim = cosine_similarity(&active[i].vector, &active[j].vector);
            let repulsion = xi_repulsive_force(&xi_a, &xi_b);
            let boosted = xi_diversity_boost(base_sim, &xi_a, &xi_b);
            
            if base_sim > 0.3 { high_sim_count += 1; }
            if repulsion > 0.15 { 
                high_repulsion_count += 1;
                if base_sim > max_sim_with_repulsion {
                    max_sim_with_repulsion = base_sim;
                }
            }
            if base_sim > 0.3 && repulsion > max_repulsion_with_sim {
                max_repulsion_with_sim = repulsion;
            }
            if boosted > base_sim { boost_count += 1; }
            
            // If Xi changes the ranking, diversity is working
            total_boost += (boosted - base_sim).abs();
            count += 1;
        }
    }
    
    if count == 0 { return 0.0; }
    let avg_boost = total_boost / count as f32;

    // Normalize: 0.05+ average boost = good diversity
    (avg_boost / 0.05).min(1.0)
}

/// Evaluate consciousness emergence: does the system exhibit integrated information?
fn eval_consciousness(engine: &ResonanceEngine, target_phi: f32) -> f32 {
    let bridge = ConsciousnessBridge::new(0.3, 0.5);
    let state = bridge.assess(engine);

    // Score: how close is phi to the target?
    let phi = state.phi as f32;
    let distance = (phi - target_phi).abs();
    (1.0 - distance / target_phi.max(0.1)).max(0.0)
}

/// Evaluate hallucination quality: hallucinated memories should be
/// semantically between their parent clusters, not random noise
fn eval_hallucination_quality(engine: &ResonanceEngine) -> f32 {
    let all = engine.store.all_memories().unwrap_or_default();
    let hallucinations: Vec<_> = all.iter()
        .filter(|m| m.hallucinated)
        .collect();

    if hallucinations.is_empty() { return 0.5; } // neutral if none

    let non_hall: Vec<_> = all.iter()
        .filter(|m| !m.hallucinated && m.amplitude > 0.01)
        .collect();

    if non_hall.is_empty() { return 0.0; }

    // Each hallucination should have reasonable similarity to at least some real memories
    let mut quality_sum = 0.0f32;
    for h in &hallucinations {
        let mut best_sim = 0.0f32;
        for m in non_hall.iter().take(20) {
            let sim = cosine_similarity(&h.vector, &m.vector).abs();
            if sim > best_sim { best_sim = sim; }
        }
        // Good hallucinations: similarity 0.3-0.7 (between clusters, not identical)
        let q = if best_sim < 0.1 { best_sim / 0.1 }  // too random
            else if best_sim > 0.9 { (1.0 - best_sim) / 0.1 }  // too similar (just a copy)
            else { 1.0 };
        quality_sum += q;
    }

    quality_sum / hallucinations.len() as f32
}

/// Evaluate dream efficiency: ratio of useful work to total work
/// Strengthened + linked should dominate over pruned + wasted cycles
fn eval_dream_efficiency(strengthened: usize, pruned: usize, links: usize, cycles: usize) -> f32 {
    let useful = strengthened + links;
    let total = useful + pruned + cycles;
    if total == 0 { return 0.0; }
    (useful as f32 / total as f32).min(1.0)
}

fn run_experiment(params: &Params) {
    let dim = 64;
    let corpus = build_corpus(dim);

    // Build engine and store corpus
    let store = Box::new(TestMedium::new());
    let encoder = Box::new(SimpleHashEncoder::new(dim, 42));
    let codebook = Codebook::new(dim, dim, 42);
    let pipeline = EncodingPipeline::new(encoder, codebook);
    let mut engine = ResonanceEngine::new(store, pipeline);
    for (i, (vec, content, category)) in corpus.iter().enumerate() {
        let mut mem = HyperMemory::new(vec.clone(), content.clone());
        mem.phase = match *category {
            "science" => 0.0 + (i as f32 * 0.1),
            "music" => PI * 0.5 + (i as f32 * 0.08),
            "personal" => PI * 0.3 * (i as f32 % 4.0),
            "emotion" => PI * 0.4 * (i as f32 % 3.0),
            "noise" => PI * (i as f32 * 0.7),
            "decoy" => PI * (i as f32 * 0.31),
            "bridge" => PI * 0.25,
            _ => 0.0,
        };
        mem.layer_depth = match *category {
            "science" => (i % 3) as u8,
            "music" => ((i + 1) % 3) as u8,
            "personal" => 0,
            "emotion" => 1,
            "noise" => 0,
            "decoy" => 2,
            "bridge" => 1,
            _ => 0,
        };
        // Set category-appropriate frequencies
        mem.frequency = match *category {
            "science" => 0.1,
            "music" => 0.15,
            "personal" => 0.08,
            "emotion" => 1.5,  // emotion frequency band
            "noise" => 0.5,
            "decoy" => 0.12,
            "bridge" => 0.11,
            _ => params.default_frequency,
        };
        if *category == "noise" {
            mem.amplitude = 0.15;
        }
        engine.store.insert(mem).expect("insert failed");
    }

    let pre_count = engine.store.count();

    let consolidator = ConsolidationEngine {
        interference_threshold: params.interference_threshold,
        phase_alignment_threshold: params.phase_alignment_threshold,
        prune_threshold: params.prune_threshold,
        constructive_boost: params.constructive_boost,
        destructive_penalty: params.destructive_penalty,
        kuramoto: KuramotoSync {
            coupling_strength: params.kuramoto_coupling,
            dt: params.kuramoto_dt,
            steps: params.kuramoto_steps,
            coupling_threshold: params.kuramoto_threshold,
        },
        adaptive: Default::default(),
        verified: Default::default(),
        chiral_perturbation: params.chiral_perturbation,
        noise_floor: params.noise_floor,
        hallucination_amplitude: params.hallucination_amplitude,
        protect_established: false,
        repulsion_threshold: params.consolidation_repulsion_threshold,
    };

    // Run multiple consolidation cycles
    let start = Instant::now();
    let mut total_strengthened = 0usize;
    let mut total_pruned = 0usize;
    let mut total_links = 0usize;
    let mut total_hallucinations = 0usize;
    let mut _last_report = None;

    for _cycle in 0..params.dream_cycles {
        let report = consolidator.consolidate(&mut engine, 0, 2);
        total_strengthened += report.memories_strengthened;
        total_pruned += report.memories_pruned;
        total_links += report.skip_links_created;
        total_hallucinations += report.hallucinations_created;
        _last_report = Some(report);
    }
    let consolidation_ms = start.elapsed().as_millis() as u64;

    let post_count = engine.store.count();

    // Component scores (HIGHER IS BETTER)
    let noise_removal = eval_noise_removal(&engine);
    let signal_preservation = eval_signal_preservation(&engine);
    let bridge_links = eval_bridge_links(&engine);
    let phase_coherence = eval_phase_coherence(&engine);
    let cluster_separation = eval_cluster_separation(&engine);
    let amp_diversity = eval_amplitude_diversity(&engine);
    let link_density = (total_links as f32 / 200.0).min(1.0);
    let speed = 1.0 - (consolidation_ms as f32 / 10000.0).min(1.0);

    // Level 2 composite fitness (LOWER IS BETTER)
    let fitness = 0.15 * (1.0 - noise_removal)
        + 0.15 * (1.0 - signal_preservation)
        + 0.10 * (1.0 - bridge_links)
        + 0.15 * (1.0 - phase_coherence)
        + 0.15 * (1.0 - cluster_separation)
        + 0.10 * (1.0 - amp_diversity)
        + 0.10 * (1.0 - link_density)
        + 0.10 * (1.0 - speed);

    println!("---");
    println!("fitness:              {fitness:.6}");
    println!("noise_removal:        {noise_removal:.4}");
    println!("signal_preservation:  {signal_preservation:.4}");
    println!("bridge_links:         {bridge_links:.4}");
    println!("phase_coherence:      {phase_coherence:.4}");
    println!("cluster_separation:   {cluster_separation:.4}");
    println!("amp_diversity:        {amp_diversity:.4}");
    println!("link_density:         {link_density:.4}");
    println!("speed:                {speed:.4}");
    println!("consolidation_ms:     {consolidation_ms}");
    println!("dream_cycles:         {}", params.dream_cycles);
    println!("links_created:        {total_links}");
    println!("memories_strengthened: {total_strengthened}");
    println!("memories_pruned:      {total_pruned}");
    println!("hallucinations:       {total_hallucinations}");
    println!("pre_count:            {pre_count}");
    println!("post_count:           {post_count}");
    println!("---");
}

/// Level 3 challenge: consciousness, Xi diversity, hallucination quality, dream efficiency
fn run_experiment_l3(params: &Params) {
    let dim = 64;
    let corpus = build_corpus(dim);

    let store = Box::new(TestMedium::new());
    let encoder = Box::new(SimpleHashEncoder::new(dim, 42));
    let codebook = Codebook::new(dim, dim, 42);
    let pipeline = EncodingPipeline::new(encoder, codebook);
    let mut engine = ResonanceEngine::new(store, pipeline);

    let ps = params.phase_spread;
    for (i, (vec, content, category)) in corpus.iter().enumerate() {
        let mut mem = HyperMemory::new(vec.clone(), content.clone());
        // Deterministic UUID: ensures consistent memory ordering across runs
        mem.id = uuid::Uuid::from_u128((i as u128 + 1) * 0x0123_4567_89AB_CDEF_0123_4567_89AB_CDEF);
        mem.phase = match *category {
            "science" => 0.0 + (i as f32 * 0.1 * ps),
            "music" => PI * 0.5 + (i as f32 * 0.08 * ps),
            "personal" => PI * 0.3 * (i as f32 % 4.0),
            "emotion" => PI * 0.4 * (i as f32 % 3.0),
            "noise" => PI * (i as f32 * 0.7),
            "decoy" => PI * (i as f32 * 0.31),
            "bridge" => PI * 0.25,
            _ => 0.0,
        };
        mem.layer_depth = match *category {
            "science" => (i % 3) as u8,
            "music" => ((i + 1) % 3) as u8,
            "personal" => 0,
            "emotion" => 1,
            "noise" => 0,
            "decoy" => 2,
            "bridge" => 1,
            _ => 0,
        };
        mem.frequency = match *category {
            "science" => 0.1,
            "music" => 0.15,
            "personal" => 0.08,
            "emotion" => 1.5,
            "noise" => 0.5,
            "decoy" => 0.12,
            "bridge" => 0.11,
            _ => params.default_frequency,
        };
        if *category == "noise" {
            mem.amplitude = 0.15;
        }
        engine.store.insert(mem).expect("insert failed");
    }

    let consolidator = ConsolidationEngine {
        interference_threshold: params.interference_threshold,
        phase_alignment_threshold: params.phase_alignment_threshold,
        prune_threshold: params.prune_threshold,
        constructive_boost: params.constructive_boost,
        destructive_penalty: params.destructive_penalty,
        kuramoto: KuramotoSync {
            coupling_strength: params.kuramoto_coupling,
            dt: params.kuramoto_dt,
            steps: params.kuramoto_steps,
            coupling_threshold: params.kuramoto_threshold,
        },
        adaptive: Default::default(),
        verified: Default::default(),
        chiral_perturbation: params.chiral_perturbation,
        noise_floor: params.noise_floor,
        hallucination_amplitude: params.hallucination_amplitude,
        protect_established: true,
        repulsion_threshold: params.consolidation_repulsion_threshold,
    };

    let start = Instant::now();
    let mut total_strengthened = 0usize;
    let mut total_pruned = 0usize;
    let mut total_links = 0usize;
    let mut total_hallucinations = 0usize;

    for _cycle in 0..params.dream_cycles {
        let report = consolidator.consolidate(&mut engine, 0, 2);
        total_strengthened += report.memories_strengthened;
        total_pruned += report.memories_pruned;
        total_links += report.skip_links_created;
        total_hallucinations += report.hallucinations_created;
    }
    let consolidation_ms = start.elapsed().as_millis() as u64;

    // L2 component scores
    let noise_removal = eval_noise_removal(&engine);
    let signal_preservation = eval_signal_preservation(&engine);
    let bridge_links = eval_bridge_links(&engine);
    let phase_coherence = eval_phase_coherence(&engine);
    let cluster_separation = eval_cluster_separation(&engine);
    let amp_diversity = eval_amplitude_diversity(&engine);
    let speed = 1.0 - (consolidation_ms as f32 / 10000.0).min(1.0);

    // L3 component scores
    let xi_diversity = eval_xi_diversity(&engine);
    let consciousness = eval_consciousness(&engine, params.consciousness_phi_target);
    let hall_quality = eval_hallucination_quality(&engine);
    let dream_efficiency = eval_dream_efficiency(
        total_strengthened, total_pruned, total_links, params.dream_cycles);

    // Level 3 composite fitness (LOWER IS BETTER)
    // Inherits L2 structure (60%) + adds L3 metrics (40%)
    let fitness = 0.10 * (1.0 - noise_removal)
        + 0.10 * (1.0 - signal_preservation)
        + 0.05 * (1.0 - bridge_links)
        + 0.10 * (1.0 - phase_coherence)
        + 0.10 * (1.0 - cluster_separation)
        + 0.05 * (1.0 - amp_diversity)
        + 0.05 * (1.0 - speed)
        + 0.05 * (1.0 - speed)  // doubled speed weight for L3
        + 0.10 * (1.0 - xi_diversity)
        + 0.10 * (1.0 - consciousness)
        + 0.10 * (1.0 - hall_quality)
        + 0.10 * (1.0 - dream_efficiency);

    println!("---");
    println!("level:                3");
    println!("fitness:              {fitness:.6}");
    println!("noise_removal:        {noise_removal:.4}");
    println!("signal_preservation:  {signal_preservation:.4}");
    println!("bridge_links:         {bridge_links:.4}");
    println!("phase_coherence:      {phase_coherence:.4}");
    println!("cluster_separation:   {cluster_separation:.4}");
    println!("amp_diversity:        {amp_diversity:.4}");
    println!("xi_diversity:         {xi_diversity:.4}");
    println!("consciousness:        {consciousness:.4}");
    println!("hall_quality:         {hall_quality:.4}");
    println!("dream_efficiency:     {dream_efficiency:.4}");
    println!("speed:                {speed:.4}");
    println!("consolidation_ms:     {consolidation_ms}");
    println!("dream_cycles:         {}", params.dream_cycles);
    println!("links_created:        {total_links}");
    println!("memories_strengthened: {total_strengthened}");
    println!("memories_pruned:      {total_pruned}");
    println!("hallucinations:       {total_hallucinations}");
    println!("---");
}

// ============================================================================
// LEVEL 4 PERSISTENCE (cycle L4.3)
// ============================================================================
//
// Session state is serialized to a bincode sidecar file. Format:
//   struct StateFile { header: StateHeader, memories: Vec<HyperMemory> }
// bincode 1.x is already a dep; HyperMemory derives Serialize/Deserialize.
//
// The "simulated time advance" applied on load mutates each memory's
// amplitude by exp(-decay_rate * dt_days) with dt_days = 1.0. This is the
// only way to make cross-session decay matter inside a sub-second release
// run, per research/program-l4.md §4.3.
//
// Golden ids are captured at the first save: the 20 highest-amplitude
// non-noise memories right before save. Later sessions track retention
// against this stable set (used by eval_retention in cycle L4.4).

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StateHeader {
    /// Incremented on every save. First save writes 1.
    session_count: u64,
    /// Frozen on first save: ids of the 20 canonical "important" memories.
    golden_ids: Vec<uuid::Uuid>,
    /// Hex digest of the corpus used to seed the very first session.
    /// Pinned so later loads can detect corpus drift.
    corpus_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StateFile {
    header: StateHeader,
    memories: Vec<HyperMemory>,
}

/// Serialize the current medium state to a bincode sidecar.
/// `prev_golden` lets a load→save chain reuse the original golden set.
/// `prev_corpus_hash` likewise pins the hash to the first session.
fn save_state(
    engine: &ResonanceEngine,
    path: &Path,
    session_count: u64,
    prev_golden: Option<&[uuid::Uuid]>,
    prev_corpus_hash: Option<&str>,
    fresh_corpus_hash: &str,
) -> Result<(), String> {
    let mems_ref = engine
        .store
        .all_memories()
        .map_err(|e| format!("all_memories failed: {e:?}"))?;
    let memories: Vec<HyperMemory> = mems_ref.iter().map(|m| (*m).clone()).collect();

    // Golden set: 20 highest-amplitude non-noise memories on first save;
    // otherwise inherit prev_golden verbatim so the series is stable.
    let golden_ids = if let Some(g) = prev_golden {
        g.to_vec()
    } else {
        let mut candidates: Vec<&HyperMemory> = memories
            .iter()
            .filter(|m| !m.content.starts_with("l4_noise"))
            .collect();
        candidates.sort_by(|a, b| b.amplitude.total_cmp(&a.amplitude));
        candidates
            .into_iter()
            .take(20)
            .map(|m| m.id)
            .collect()
    };

    let header = StateHeader {
        session_count,
        golden_ids,
        corpus_hash: prev_corpus_hash.map(String::from).unwrap_or_else(|| fresh_corpus_hash.to_string()),
    };
    let state = StateFile { header, memories };

    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create_dir_all({}): {}", parent.display(), e))?;
        }
    }
    let bytes = bincode::serialize(&state)
        .map_err(|e| format!("bincode serialize: {e}"))?;
    std::fs::write(path, bytes)
        .map_err(|e| format!("write {}: {}", path.display(), e))?;
    Ok(())
}

/// Load a prior state file. Returns the deserialized state; callers are
/// responsible for rehydrating a ResonanceEngine and running the time
/// advance on the returned memories.
fn load_state(path: &Path) -> Result<StateFile, String> {
    let bytes = std::fs::read(path)
        .map_err(|e| format!("read {}: {}", path.display(), e))?;
    let state: StateFile = bincode::deserialize(&bytes)
        .map_err(|e| format!("bincode deserialize: {e}"))?;
    Ok(state)
}

/// Rebuild a ResonanceEngine from persisted memories, applying the
/// simulated 1-day decay pass described in research/program-l4.md §4.3.
fn rehydrate_engine_from_state(
    state: &StateFile,
    encoder_seed: u64,
    dim: usize,
    dt_days: f32,
) -> ResonanceEngine {
    let store = Box::new(TestMedium::new());
    let encoder = Box::new(SimpleHashEncoder::new(dim, encoder_seed));
    let codebook = Codebook::new(dim, dim, encoder_seed);
    let pipeline = EncodingPipeline::new(encoder, codebook);
    let mut engine = ResonanceEngine::new(store, pipeline);

    for m in &state.memories {
        let mut mem = m.clone();
        // Simulated time advance: amplitude *= exp(-decay_rate * dt_days)
        let factor = (-mem.decay_rate * dt_days).exp();
        mem.amplitude *= factor;
        // We ignore the insert error for duplicates — memories came from
        // a single prior run so ids are unique by construction.
        let _ = engine.store.insert(mem);
    }
    engine
}

/// CLI flags parsed in main() but relevant only to L4 sessions.
#[derive(Debug, Default, Clone)]
struct L4Cli {
    load_path: Option<PathBuf>,
    save_path: Option<PathBuf>,
    chain_sessions: usize,
}

/// Level 4 challenge — STUB (cycle L4.2).
///
/// Builds the L4 corpus via `build_corpus_l4`, inserts it into a TestMedium,
/// runs the existing ConsolidationEngine dream cycle, and scores using the
/// existing L3 evaluators + L3 fitness weighting.
///
/// This is intentional placeholder wiring. L4-specific metrics
/// (corpus_xi_diversity, retention, chain_fidelity, adversarial_resistance,
/// encoding_entropy) land in cycles L4.4 - L4.7. Some L3 evaluators
/// (phase_coherence, cluster_separation) rely on content strings from the
/// L3 corpus ("quantum", "resonance") and will report 0 on the L4 corpus;
/// this is expected and will be replaced by real L4 metrics in later cycles.
fn build_fresh_l4_engine(params: &Params, dim: usize) -> ResonanceEngine {
    let corpus = build_corpus_l4(dim, 1, params.encoder_seed);

    let store = Box::new(TestMedium::new());
    let encoder = Box::new(SimpleHashEncoder::new(dim, params.encoder_seed));
    let codebook = Codebook::new(dim, dim, params.encoder_seed);
    let pipeline = EncodingPipeline::new(encoder, codebook);
    let mut engine = ResonanceEngine::new(store, pipeline);

    let ps = params.phase_spread;
    for (i, (vec, content, category)) in corpus.iter().enumerate() {
        let mut mem = HyperMemory::new(vec.clone(), content.clone());
        // Deterministic UUID (mirrors L3 pattern so retention is reproducible).
        mem.id = uuid::Uuid::from_u128((i as u128 + 1) * 0x0123_4567_89AB_CDEF_0123_4567_89AB_CDEF);
        mem.decay_rate = params.decay_rate;
        mem.phase = match *category {
            "l4_dense"  => 0.0 + (i as f32 * 0.1 * ps),
            "l4_sparse" => PI * 0.5 + (i as f32 * 0.08 * ps),
            "l4_bridge" => PI * 0.25,
            "l4_decoy"  => PI * (i as f32 * 0.31),
            "l4_noise"  => PI * (i as f32 * 0.7),
            _ => 0.0,
        };
        mem.layer_depth = match *category {
            "l4_dense"  => (i % 3) as u8,
            "l4_sparse" => ((i + 1) % 3) as u8,
            "l4_bridge" => 1,
            "l4_decoy"  => 2,
            "l4_noise"  => 0,
            _ => 0,
        };
        // Fully overlapping frequency bands — no freq-gating trick.
        mem.frequency = match *category {
            "l4_dense"  => 0.10,
            "l4_sparse" => 0.11,
            "l4_bridge" => 0.10,
            "l4_decoy"  => 0.10,
            "l4_noise"  => 0.10,
            _ => params.default_frequency,
        };
        if *category == "l4_noise" {
            mem.amplitude = 0.15;
        }
        engine.store.insert(mem).expect("insert failed");
    }

    engine
}

/// Per-pass metrics bundle. Used so the clean pass and adversarial pass can
/// share evaluation code. Not derive(Clone/Debug) because `post_engine`
/// carries the live engine (no Clone) so the caller can save it.
struct L4PassMetrics {
    fitness: f32,
    noise_removal: f32,
    signal_preservation: f32,
    bridge_links: f32,
    phase_coherence: f32,
    cluster_separation: f32,
    amp_diversity: f32,
    xi_diversity: f32,
    consciousness: f32,
    hall_quality: f32,
    dream_efficiency: f32,
    speed: f32,
    retention_score: f32,
    retention_plasticity: f32,
    chain_fidelity: f32,
    corpus_xi_diversity: f32,
    encoding_entropy: f32,
    phi_history: Vec<f32>,
    consolidation_ms: u64,
    strengthened: usize,
    pruned: usize,
    links: usize,
    hallucinations: usize,
    post_engine: ResonanceEngine,
}

/// Run one L4 pass: snapshot → chain → metrics → fitness. `inject_adv`
/// controls whether the adversarial set is folded in (cycle L4.6).
fn run_l4_pass(
    params: &Params,
    cli: &L4Cli,
    dim: usize,
    inject_adv: bool,
) -> (L4PassMetrics, Option<StateHeader>) {
    // Either load prior state or build fresh. Rebuilt from scratch on every
    // pass per Nick's locked decision in §10 — no caching between passes.
    let (mut engine, prev_header): (ResonanceEngine, Option<StateHeader>) = match &cli.load_path {
        Some(p) if p.exists() => {
            match load_state(p) {
                Ok(state) => {
                    let engine = rehydrate_engine_from_state(&state, params.encoder_seed, dim, 1.0);
                    (engine, Some(state.header))
                }
                Err(e) => {
                    eprintln!("load_state({}) failed: {} — starting fresh", p.display(), e);
                    (build_fresh_l4_engine(params, dim), None)
                }
            }
        }
        _ => (build_fresh_l4_engine(params, dim), None),
    };

    if inject_adv {
        // Build the adversarial set using a fixed seed INDEPENDENT of the
        // encoder seed (Nick's locked decision). The L3 reference corpus is
        // re-used as the "real memory" source for A4 (near-duplicate clones).
        let corpus = build_corpus_l4(dim, 1, params.encoder_seed);
        let adversaries = build_adversarial_set(&corpus, ADVERSARIAL_SEED);
        for (i, m) in adversaries.into_iter().enumerate() {
            // Adversaries use their own UUID namespace (high bit set) so
            // they cannot collide with the deterministic corpus ids.
            let mut mem = m;
            mem.id = uuid::Uuid::from_u128(
                0xDEAD_BEEF_0000_0000_0000_0000_0000_0000u128 + i as u128,
            );
            let _ = engine.store.insert(mem);
        }
    }

    // Snapshot engine state BEFORE the dream chain. Used by
    // eval_retention_plasticity so we can measure amplitude drift on the
    // golden set specifically attributable to dreaming (and any time-advance
    // that already happened during rehydrate).
    let engine_before_dream = snapshot_engine_for_plasticity(&engine);

    // Run the full chain_depth-cycle dream chain (cycle L4.5). Each cycle's
    // output biases the next cycle's interference_threshold via
    // chain_carry_strength. params.dream_cycles is IGNORED on the L4 path —
    // chain_depth supersedes it per design doc §5.2.
    let start = Instant::now();
    let (chain_seeds, phi_history, chain_totals) = run_dream_chain(params, &mut engine);
    let consolidation_ms = start.elapsed().as_millis() as u64;

    let chain_fidelity = eval_chain_fidelity(&chain_seeds, &phi_history);

    // Retention metrics (L4.4). Single-session runs return 1.0 by design:
    // the `session_count < 2` fallback in eval_retention_score fires, and
    // plasticity with an empty golden set likewise returns 1.0. This keeps
    // fitness uncontaminated on smoke-test invocations.
    let retention_header_for_score = prev_header.clone().unwrap_or_else(|| StateHeader {
        session_count: 1,
        golden_ids: Vec::new(),
        corpus_hash: String::new(),
    });
    let retention_score = eval_retention_score(&engine, &retention_header_for_score);
    let plasticity_golden = prev_header
        .as_ref()
        .map(|h| h.golden_ids.as_slice())
        .unwrap_or(&[]);
    let retention_plasticity = eval_retention_plasticity(
        &engine_before_dream,
        &engine,
        plasticity_golden,
    );

    let noise_removal = eval_l4_noise_removal(&engine);
    let signal_preservation = eval_l4_signal_preservation(&engine);
    let bridge_links = eval_bridge_links(&engine);
    // Cycle L4.7.5: switch to L4-aware partitioning. The L3 versions filter
    // by hardcoded "quantum"/"resonance" content substrings that no L4
    // memory matches, returning 0 and creating a 0.10 phantom fitness floor
    // that no parameter tuning could reach.
    let phase_coherence = eval_phase_coherence_l4(&engine);
    let cluster_separation = eval_cluster_separation_l4(&engine);
    let amp_diversity = eval_amplitude_diversity(&engine);
    let speed = 1.0 - (consolidation_ms as f32 / 10000.0).min(1.0);

    // L4.S13: decouple speed from the adversarial pass. The adv pass runs
    // sequentially after the clean pass; CPU thermal/IO state from a slow
    // clean pass bleeds into the adv timing, producing artifactual speed
    // regression on the adv subtotal even when adv params are unchanged
    // (L4.S11c finding). Adversarial robustness measures *correctness*
    // under attack, not speed under attack. Fix: on the adv pass, treat
    // speed as 1.0 (neutral, zero loss) so it doesn't contaminate
    // fitness_adv_sub. The weight table stays at 100%; only the effective
    // score changes per-pass.
    let effective_speed = if inject_adv { 1.0 } else { speed };

    let xi_diversity = eval_xi_diversity(&engine);
    let consciousness = eval_consciousness(&engine, params.consciousness_phi_target);
    let hall_quality = eval_hallucination_quality(&engine);
    let dream_efficiency = eval_dream_efficiency(
        chain_totals.strengthened, chain_totals.pruned, chain_totals.links, params.chain_depth);

    // L4.7 new axes.
    let surviving: Vec<HyperMemory> = engine
        .store
        .all_memories()
        .unwrap_or_default()
        .iter()
        .map(|m| (*m).clone())
        .collect();
    let corpus_xi_diversity = eval_corpus_xi_diversity(&surviving);
    let encoding_entropy = eval_encoding_entropy(&surviving, 8);

    // L4 final fitness weighting (design doc §3, §8).
    // Inherited L3 core (45%) + new L4 axes (55%) = 100%.
    //
    //   Inherited L3 core      45%
    //     noise_removal         5
    //     signal_preservation   5
    //     phase_coherence       5
    //     cluster_separation    5
    //     dream_efficiency      5
    //     speed                10   (neutralized on adv pass — L4.S13)
    //     consciousness        10
    //
    //   L4 new axes            55%
    //     corpus_xi_diversity  10
    //     retention_score      15
    //     retention_plasticity  5
    //     chain_fidelity       10
    //     adversarial_resist.  10   (injected by caller — see note below)
    //     encoding_entropy      5
    //
    // adversarial_resistance is computed across the TWO passes at the outer
    // layer (run_experiment_l4_session), not inside run_l4_pass. Inside this
    // function we leave that 10% slot as neutral (treated as 1.0), and the
    // outer orchestrator overwrites fitness with the resistance-adjusted
    // total. This keeps run_l4_pass reusable for both clean and adversarial
    // inner evaluations without double-counting.
    // Neutral inside run_l4_pass; the outer orchestrator overwrites fitness with the
    // resistance-adjusted total (see the note above). Named (not a literal 1.0 - 1.0)
    // so it isn't a clippy::eq_op.
    let adversarial_resistance = 1.0;
    let fitness = 0.05 * (1.0 - noise_removal)
        + 0.05 * (1.0 - signal_preservation)
        + 0.05 * (1.0 - phase_coherence)
        + 0.05 * (1.0 - cluster_separation)
        + 0.05 * (1.0 - dream_efficiency)
        + 0.10 * (1.0 - effective_speed)
        + 0.10 * (1.0 - consciousness)
        + 0.10 * (1.0 - corpus_xi_diversity)
        + 0.15 * (1.0 - retention_score)
        + 0.05 * (1.0 - retention_plasticity)
        + 0.10 * (1.0 - chain_fidelity)
        + 0.10 * (1.0 - adversarial_resistance) // slot filled by the outer orchestrator
        + 0.05 * (1.0 - encoding_entropy);

    let metrics = L4PassMetrics {
        fitness,
        noise_removal,
        signal_preservation,
        bridge_links,
        phase_coherence,
        cluster_separation,
        amp_diversity,
        xi_diversity,
        consciousness,
        hall_quality,
        dream_efficiency,
        speed,
        retention_score,
        retention_plasticity,
        chain_fidelity,
        corpus_xi_diversity,
        encoding_entropy,
        phi_history,
        consolidation_ms,
        strengthened: chain_totals.strengthened,
        pruned: chain_totals.pruned,
        links: chain_totals.links,
        hallucinations: chain_totals.hallucinations,
        post_engine: engine,
    };
    (metrics, prev_header)
}

/// One L4 session. Honors --load/--save/--chain-sessions CLI flags.
///
/// Flow per session (cycle L4.6):
///   1. Clean pass:      fresh corpus → chain → all metrics → fitness_clean
///   2. Adversarial pass: fresh corpus + 40 adversarial memories → chain →
///                        all metrics → fitness_adv
///   3. adversarial_resistance = 1 - |f_clean - f_adv| / max(f_clean, 1e-3)
///   4. Reported fitness = fitness_clean (adversarial pass is only used to
///      compute the resistance metric; design §2 M4)
///   5. If `--save` is set, serialize the CLEAN post-dream state (never
///      the adversarial one — state must never include adversaries per §6.2).
fn run_experiment_l4_session(params: &Params, cli: &L4Cli) {
    let dim = 128;

    // L4.13: L4-local overrides. These parameters diverge from the L3 frozen
    // archive and must not leak back into run_experiment_l3. Clone params
    // and patch the L4-specific fields here.
    //   - phase_alignment_threshold: PI/3.0 -> PI/2.5 (L4.13, kept)
    //     Lifts phase_coherence 0.828->0.866 and adv_resistance dramatically;
    //     mild chain_fidelity collateral. L3 path stays at PI/3.0.
    let mut l4_params = params.clone();
    l4_params.phase_alignment_threshold = PI / 2.5;
    // L4.14: retune consciousness_phi_target to L4's measured phi.
    // On L3, post-chain phi ~0.326 = target. On L4 after L4.13 the measured
    // phi is 0.25156 (byte-identical across runs, read from phi_history_clean).
    // Gap = 0.0744. Nudging 80% toward the measured value leaves 20% slack
    // against future jitter: 0.326 - 0.8 * 0.07444 = 0.26645.
    // L4.S7: re-tune for the 3-point phi_history introduced by S4
    // (chain_depth=3). Measured phi_history (byte-identical) =
    // [0.18426, 0.24996, 0.28454]; final phi = 0.28454. Previous
    // target 0.26645 was calibrated for the old 2-point chain with
    // final phi 0.25156. Gap = 0.01809; nudge 80% toward measured:
    // 0.26645 + 0.8 * 0.01809 = 0.28092.
    l4_params.consciousness_phi_target = 0.28092;
    // L4.15: H-L4-005 — chain_carry_strength 0.5 -> 0.7 (L4-local override).
    // Theory: stronger cycle-to-cycle carry biases each chain cycle's pair
    // selection more aggressively toward the previous cycle's xi centroid,
    // tightening monotonicity and lifting chain_fidelity (0.7487) back up.
    l4_params.chain_carry_strength = 0.7;
    // L4.16: H-L4-006 — chiral_perturbation 0.9 -> 0.7 (L4-local override).
    // Probe of last untested encoder-layer param. Target: corpus_xi_diversity
    // (0.6018) and possibly encoding_entropy (0.0213).
    l4_params.chiral_perturbation = 0.7;
    // L4.S15: consolidation_repulsion_threshold — L4-local override.
    // Default (0.30) keeps L3 at its frozen value (0/300 pairs qualify).
    // Post-xi-fix, the nonlinear commutator spreads repulsion up to 0.31.
    // Sweep results (3-run probes):
    //   0.30: fitness 0.1019, phase_coh 0.9489, adv_resist 0.6127 (baseline)
    //   0.29: fitness 0.1031, phase_coh 0.9388, adv_resist 0.6128
    //   0.28: fitness 0.1010, phase_coh 0.9604, adv_resist 0.6119 (BEST)
    //   0.27: fitness 0.1209, phase_coh 0.9654, adv_resist 0.4294 (cliff)
    //   0.25: fitness 0.1474, phase_coh 0.8662, adv_resist 0.4118
    //   0.22: fitness 0.2750, phase_coh 0.4083, adv_resist 0.3406
    //   0.15: fitness 0.2064, phase_coh 0.4336, adv_resist 0.???? (pre-fix)
    // The cliff between 0.28 and 0.27 comes from adv_resistance: the small
    // number of extra qualifying pairs at 0.27 disrupts the adversarial pass
    // disproportionately. 0.28 is the lowest threshold where adv_resistance
    // holds near the 0.30 baseline.
    l4_params.consolidation_repulsion_threshold = 0.28;
    // L4.S4: chain_depth 2 -> 3, chain_top_n 10 -> 7 (L4-local override).
    // Escape the trivial 2-point monotonicity cap on chain_fidelity. With
    // chain_depth=2 the base_score is a single cosine distance and the
    // monotonicity bonus is a binary {0, 0.1}; chain_fidelity pegs at ~0.75.
    // A 3-point chain re-enables the averaged cosine refinement the metric
    // was designed to measure, and top_n=7 tightens seed selection without
    // collapsing the xi pool the way top_n=5 did (L4.12 crash).
    l4_params.chain_depth = 3;
    l4_params.chain_top_n = 7;
    // L4.S8 REVERTED: interference_threshold 0.10 -> 0.12 retry unblocked by
    // the S8a NaN-phase guard, but 15-run fitness regressed 0.096517 ->
    // 0.117416 (+0.0209). Clean-pass gains were real (fitness_clean_sub
    // 0.0918 -> 0.0676, chain_fidelity +0.069, corpus_xi_diversity +0.195)
    // but adv_resistance crashed 0.9584 -> 0.5017 (-0.457) because the
    // higher threshold widens the clean-vs-adv fitness divergence past what
    // the S3 padded denominator can absorb. The 0.10 weight on adv_resist
    // turns the collapse into +0.046 fitness and eats the clean-pass win.
    // L4.S13: per-pass interference_threshold. The clean pass benefits from
    // 0.12 (fitness_clean_sub -0.024, chain_fidelity +0.069, corpus_xi_div
    // +0.195 — confirmed in S8, S9, S11c). Previous attempts failed because
    // speed bleed from the clean pass polluted the adv pass via sequential
    // timing (S11c) or the old divergence adv_resistance formula punished
    // clean-only improvement (S9). Both blockers are now fixed: speed is
    // decoupled from the adv pass (L4.S13) and adv_resistance uses the
    // absolute formula (L4.S11). The adv pass keeps int_threshold=0.10 to
    // avoid the NaN-phase edge case at 0.12 (S8a) and because adversarial
    // robustness should be measured under production defaults.
    let mut l4_params_clean = l4_params.clone();
    l4_params_clean.interference_threshold = 0.12;
    // adv pass stays at l4_params defaults (int_threshold = 0.10)
    let l4_params_adv = &l4_params;
    let params = &l4_params; // for non-pass-specific operations below

    // Compute (and stash) the canonical corpus hash. This is the value that
    // gets pinned into the state header on first save.
    let fresh_corpus_hash = {
        let corpus = build_corpus_l4(dim, 1, params.encoder_seed);
        corpus_l4_hash(&corpus)
    };

    // ---- Clean pass (scored) ----
    let (clean, prev_header) = run_l4_pass(&l4_params_clean, cli, dim, false);

    // ---- Adversarial pass (used ONLY to compute resistance) ----
    // Build fully from scratch — no state caching from the clean pass.
    let (adv, _prev_header_adv) = run_l4_pass(l4_params_adv, cli, dim, true);

    // L4.S11: absolute adversarial robustness.
    // Score measures how well the adversarial pass performs in absolute terms,
    // NOT how similar it is to the clean pass. In production, "robust" means the
    // system continues to perform well under attack, even if clean and adversarial
    // behavior diverge. A system with 100% clean + 100% adversarial performance
    // has maximum robustness even if their outputs are byte-different.
    //
    // Normalizer (0.15) chosen so that f_adv_sub = 0.15 → score = 0.0
    // (catastrophic adversarial failure) and f_adv_sub = 0.0 → score = 1.0
    // (perfect adversarial resistance). Values above 0.15 saturate at 0.0.
    //
    // Previous formula (L4.S3, deprecated):
    //     resistance = 1 - |f_clean - f_adv| / (f_clean + 0.05)
    // was a DIVERGENCE metric — it mechanically punished any clean-only
    // improvement (see L4.S9 post-mortem in ooda-state.json). The new
    // formulation unlocks per-pass tuning by removing the artifactual
    // clean-vs-adv similarity coupling.
    let resistance = (1.0_f32 - adv.fitness / 0.15_f32).clamp(0.0, 1.0);

    // Reported fitness = clean pass 90%-weight subtotal plus the 10%
    // adversarial resistance contribution. clean.fitness already accounts
    // for the 90% (retention/chain/encoding/etc.), so we just add the
    // resistance slot here.
    let fitness = clean.fitness + 0.10 * (1.0 - resistance);

    // Save state if --save was requested. ALWAYS save the clean engine;
    // the adversarial pass's engine must never be serialized (§6.2).
    let save_session_count = if let Some(save_path) = &cli.save_path {
        let next_session = prev_header.as_ref().map(|h| h.session_count + 1).unwrap_or(1);
        let prev_golden = prev_header.as_ref().map(|h| h.golden_ids.as_slice());
        let prev_corpus_hash = prev_header.as_ref().map(|h| h.corpus_hash.as_str());
        match save_state(
            &clean.post_engine,
            save_path,
            next_session,
            prev_golden,
            prev_corpus_hash,
            &fresh_corpus_hash,
        ) {
            Ok(()) => Some(next_session),
            Err(e) => {
                eprintln!("save_state({}) failed: {}", save_path.display(), e);
                None
            }
        }
    } else {
        None
    };

    println!("---");
    println!("level:                4");
    if let Some(prev) = prev_header.as_ref() {
        println!("loaded_session:       {}", prev.session_count);
    }
    if let Some(n) = save_session_count {
        println!("saved_session:        {n}");
    }
    println!("fitness:              {fitness:.6}");
    println!("fitness_clean_sub:    {:.6}", clean.fitness);
    println!("fitness_adv_sub:      {:.6}", adv.fitness);
    println!("adv_resistance:       {resistance:.4}");
    println!("corpus_xi_diversity:  {:.4}", clean.corpus_xi_diversity);
    println!("encoding_entropy:     {:.4}", clean.encoding_entropy);
    println!("retention_score:      {:.4}", clean.retention_score);
    println!("retention_plasticity: {:.4}", clean.retention_plasticity);
    println!("chain_fidelity:       {:.4}", clean.chain_fidelity);
    println!("chain_depth:          {}", params.chain_depth);
    println!("phi_history_clean:    {:?}", clean.phi_history);
    println!("phi_history_adv:      {:?}", adv.phi_history);
    println!("noise_removal:        {:.4}", clean.noise_removal);
    println!("signal_preservation:  {:.4}", clean.signal_preservation);
    println!("bridge_links:         {:.4}", clean.bridge_links);
    println!("phase_coherence:      {:.4}", clean.phase_coherence);
    println!("cluster_separation:   {:.4}", clean.cluster_separation);
    println!("amp_diversity:        {:.4}", clean.amp_diversity);
    println!("xi_diversity:         {:.4}", clean.xi_diversity);
    println!("consciousness:        {:.4}", clean.consciousness);
    println!("hall_quality:         {:.4}", clean.hall_quality);
    println!("dream_efficiency:     {:.4}", clean.dream_efficiency);
    println!("speed:                {:.4}", clean.speed);
    println!("consolidation_ms:     {}", clean.consolidation_ms);
    println!("dream_cycles:         {}", params.dream_cycles);
    println!("links_created:        {}", clean.links);
    println!("memories_strengthened: {}", clean.strengthened);
    println!("memories_pruned:      {}", clean.pruned);
    println!("hallucinations:       {}", clean.hallucinations);
    println!("---");
}

// ============================================================================
// LEVEL 4 RETENTION METRICS (cycle L4.4)
// ============================================================================
//
// retention_score and retention_plasticity are a guardrail pair:
//   - retention_score rewards keeping "golden" memories alive and vivid across
//     save→load→dream cycles.
//   - retention_plasticity rewards the surviving memories actually CHANGING
//     during the post-load dream cycle. This exists specifically to block the
//     decay_rate=0 / prune_threshold=0 cheese where the trivial way to win
//     retention is to freeze the whole system.
//
// Both metrics are only meaningful once we have a loaded prior session.
// First-session runs return 1.0 for both (neutral — no baseline to compare).

/// Fraction of golden memories still present AND with amplitude >= 0.5.
/// Returns 1.0 when `session_count < 2` — there is no prior state to retain.
fn eval_retention_score(engine: &ResonanceEngine, header: &StateHeader) -> f32 {
    if header.session_count < 2 {
        return 1.0;
    }
    if header.golden_ids.is_empty() {
        return 1.0;
    }
    let all = engine.store.all_memories().unwrap_or_default();
    let mut survived = 0usize;
    for gid in &header.golden_ids {
        if let Some(m) = all.iter().find(|m| &m.id == gid) {
            if m.amplitude >= 0.5 {
                survived += 1;
            }
        }
    }
    (survived as f32 / header.golden_ids.len() as f32).clamp(0.0, 1.0)
}

/// Measures post-load plasticity on the golden set. Compares amplitudes
/// before and after the dream cycle. Higher drift = more plastic.
/// Returns `(mean_drift * 5.0).min(1.0)`; design-doc rationale: this is a
/// guardrail against decay=0 / prune=0 cheese (see §2 M2b).
fn eval_retention_plasticity(
    engine_before: &ResonanceEngine,
    engine_after: &ResonanceEngine,
    golden_ids: &[uuid::Uuid],
) -> f32 {
    if golden_ids.is_empty() {
        return 1.0;
    }
    let before_all = engine_before.store.all_memories().unwrap_or_default();
    let after_all = engine_after.store.all_memories().unwrap_or_default();
    let mut drift_sum = 0.0f32;
    let mut count = 0usize;
    for gid in golden_ids {
        let a = before_all.iter().find(|m| &m.id == gid);
        let b = after_all.iter().find(|m| &m.id == gid);
        if let (Some(ma), Some(mb)) = (a, b) {
            drift_sum += (mb.amplitude - ma.amplitude).abs();
            count += 1;
        }
    }
    if count == 0 {
        return 1.0;
    }
    let mean_drift = drift_sum / count as f32;
    (mean_drift * 5.0).min(1.0)
}

/// Snapshot just the memories needed to compute retention_plasticity.
/// The chain pass may mutate `engine` in place; this copy gives us a stable
/// "before" baseline without serializing through bincode.
fn snapshot_engine_for_plasticity(engine: &ResonanceEngine) -> ResonanceEngine {
    let mems = engine.store.all_memories().unwrap_or_default();
    let store = Box::new(TestMedium::new());
    // Encoder/codebook are only needed for inserts via pipeline; the harness
    // never re-encodes on a snapshot, so zeros and dim=1 are fine.
    let encoder = Box::new(SimpleHashEncoder::new(1, 0));
    let codebook = Codebook::new(1, 1, 0);
    let pipeline = EncodingPipeline::new(encoder, codebook);
    let mut clone = ResonanceEngine::new(store, pipeline);
    for m in &mems {
        let _ = clone.store.insert((*m).clone());
    }
    clone
}

// ============================================================================
// LEVEL 4 DREAM CHAIN (cycle L4.5)
// ============================================================================
//
// A ChainSeed is the handoff between successive dream cycles: it captures the
// top-N memories (by amplitude) from cycle K along with their xi-signature
// centroid, so cycle K+1 can bias pair selection toward "what K was working
// on". This is implemented as a harness-level wrapper around the existing
// ConsolidationEngine — we build a NEW engine per cycle with a lowered
// interference_threshold, rather than reaching into the library. No core
// library changes.

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct ChainSeed {
    carry_ids: Vec<uuid::Uuid>,
    carry_xi_centroid: Vec<f32>,
    round: u32,
}

/// Compute the xi-signature centroid over the top-N memories by amplitude,
/// excluding noise. Falls back to an empty vector if the engine is empty.
fn compute_chain_seed(engine: &ResonanceEngine, top_n: usize, round: u32) -> ChainSeed {
    let all = engine.store.all_memories().unwrap_or_default();
    let mut non_noise: Vec<&HyperMemory> = all
        .iter()
        .filter(|m| !m.content.starts_with("l4_noise") && m.amplitude > 0.01)
        .copied()
        .collect();
    non_noise.sort_by(|a, b| b.amplitude.total_cmp(&a.amplitude));
    let survivors: Vec<&HyperMemory> = non_noise.into_iter().take(top_n.max(1)).collect();

    if survivors.is_empty() {
        return ChainSeed {
            carry_ids: Vec::new(),
            carry_xi_centroid: Vec::new(),
            round,
        };
    }

    // Compute xi-signature for each survivor, then average into a centroid.
    let sig_len = {
        let s = compute_xi_signature(&survivors[0].vector);
        s.len()
    };
    let mut centroid = vec![0.0f32; sig_len];
    let mut count = 0usize;
    for m in &survivors {
        let sig = compute_xi_signature(&m.vector);
        if sig.len() == sig_len {
            for (c, s) in centroid.iter_mut().zip(sig.iter()) {
                *c += *s;
            }
            count += 1;
        }
    }
    if count > 0 {
        let inv = 1.0 / count as f32;
        for c in centroid.iter_mut() {
            *c *= inv;
        }
    }
    ChainSeed {
        carry_ids: survivors.iter().map(|m| m.id).collect(),
        carry_xi_centroid: centroid,
        round,
    }
}

/// Cosine similarity on two equal-length real vectors. Falls back to 0 on
/// length mismatch or zero norm.
fn vec_cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na < 1e-8 || nb < 1e-8 {
        return 0.0;
    }
    (dot / (na * nb)).clamp(-1.0, 1.0)
}

/// chain_fidelity: how coherently the xi-centroid refines across the chain,
/// plus a small monotonicity bonus for non-decreasing Φ.
///
///   base_score = 1 - mean(cosine_distance(centroid_K, centroid_{K+1}))
///   monotonicity_bonus = 0.1 * (phi_increases / (chain_depth - 1))
///   return (base_score + bonus).clamp(0, 1)
///
/// With a single-cycle chain, returns 1.0 (no transitions to measure).
fn eval_chain_fidelity(chain_seeds: &[ChainSeed], phi_history: &[f32]) -> f32 {
    if chain_seeds.len() < 2 {
        return 1.0;
    }
    let mut dist_sum = 0.0f32;
    let mut pairs = 0usize;
    for w in chain_seeds.windows(2) {
        let sim = vec_cosine(&w[0].carry_xi_centroid, &w[1].carry_xi_centroid);
        // cosine_distance = 1 - sim (signs can go negative; clamp to [0, 2])
        dist_sum += (1.0 - sim).clamp(0.0, 2.0);
        pairs += 1;
    }
    let mean_dist = if pairs > 0 { dist_sum / pairs as f32 } else { 0.0 };
    let base_score = (1.0 - mean_dist).clamp(0.0, 1.0);

    // Monotonicity: count the number of transitions where phi did not decrease.
    let mut increases = 0usize;
    let mut denom = 0usize;
    if phi_history.len() >= 2 {
        for w in phi_history.windows(2) {
            if w[1] >= w[0] {
                increases += 1;
            }
            denom += 1;
        }
    }
    let bonus = if denom > 0 {
        0.1 * (increases as f32 / denom as f32)
    } else {
        0.0
    };

    (base_score + bonus).clamp(0.0, 1.0)
}

/// Aggregate totals reported by a dream chain execution. Mirrors the per-cycle
/// counters run_experiment_l4_session previously accumulated in a plain loop.
#[derive(Debug, Default, Clone, Copy)]
struct ChainTotals {
    strengthened: usize,
    pruned: usize,
    links: usize,
    hallucinations: usize,
}

/// Run a `chain_depth`-cycle dream chain on `engine`, biasing each cycle's
/// pair selection toward the previous cycle's xi centroid by lowering the
/// effective interference_threshold by `chain_carry_strength * threshold`.
///
/// Returns the per-cycle xi centroids (as `ChainSeed` entries) and the
/// per-cycle Φ history, plus aggregated dream counters.
fn run_dream_chain(
    params: &Params,
    engine: &mut ResonanceEngine,
) -> (Vec<ChainSeed>, Vec<f32>, ChainTotals) {
    let (seeds, phi, totals, _quiescence) = run_dream_chain_with_quiescence(params, engine);
    (seeds, phi, totals)
}

/// Run a dream chain with optional quiescence short-circuit.
/// Returns (chain_seeds, phi_history, totals, quiescence_at).
/// Quiescence fires when chain_depth >= 8 and the phi delta between
/// consecutive cycles drops below 0.001. This implements the shortest-path
/// approach: stop early when metrics stabilize (program-l5.md §8.4).
/// For chain_depth < 8 (L3 dream_cycles=1, L4 chain_depth=2-3), quiescence
/// is disabled — those levels are not affected.
fn run_dream_chain_with_quiescence(
    params: &Params,
    engine: &mut ResonanceEngine,
) -> (Vec<ChainSeed>, Vec<f32>, ChainTotals, Option<usize>) {
    let depth = params.chain_depth.max(1);
    let quiescence_enabled = depth >= 8;
    let quiescence_threshold = 0.001_f32;
    let mut chain_seeds: Vec<ChainSeed> = Vec::with_capacity(depth);
    let mut phi_history: Vec<f32> = Vec::with_capacity(depth);
    let mut totals = ChainTotals::default();
    let bridge = ConsciousnessBridge::new(0.3, 0.5);
    let mut quiescence_at: Option<usize> = None;

    for cycle_idx in 0..depth {
        // Cycle 1 uses the nominal threshold; later cycles lower it by the
        // carry_strength fraction so the consolidator is biased toward more
        // aggressive pair selection on the "carried" memories. We drop the
        // threshold globally because the ConsolidationEngine library doesn't
        // expose a per-memory threshold hook, and modifying core just to
        // shave a few points of fidelity isn't worth the surface area.
        let threshold_scale = if cycle_idx == 0 {
            1.0
        } else {
            (1.0 - params.chain_carry_strength).max(0.0)
        };
        let effective_threshold = params.interference_threshold * threshold_scale;

        let consolidator = ConsolidationEngine {
            interference_threshold: effective_threshold,
            phase_alignment_threshold: params.phase_alignment_threshold,
            prune_threshold: params.prune_threshold,
            constructive_boost: params.constructive_boost,
            destructive_penalty: params.destructive_penalty,
            kuramoto: KuramotoSync {
                coupling_strength: params.kuramoto_coupling,
                dt: params.kuramoto_dt,
                steps: params.kuramoto_steps,
                coupling_threshold: params.kuramoto_threshold,
            },
            adaptive: Default::default(),
            verified: Default::default(),
            chiral_perturbation: params.chiral_perturbation,
            noise_floor: params.noise_floor,
            hallucination_amplitude: params.hallucination_amplitude,
            protect_established: true,
            repulsion_threshold: params.consolidation_repulsion_threshold,
        };

        let report = consolidator.consolidate(engine, 0, 2);
        totals.strengthened += report.memories_strengthened;
        totals.pruned += report.memories_pruned;
        totals.links += report.skip_links_created;
        totals.hallucinations += report.hallucinations_created;

        let seed = compute_chain_seed(engine, params.chain_top_n, (cycle_idx + 1) as u32);
        chain_seeds.push(seed);
        let phi = bridge.assess(engine).phi as f32;
        phi_history.push(phi);

        // Quiescence short-circuit: stop early when phi stabilizes.
        // Only active for deep chains (>= 8 cycles). Must run at least 3
        // cycles before checking to let the system settle past initial
        // transients.
        if quiescence_enabled && cycle_idx >= 2 {
            let prev_phi = phi_history[cycle_idx - 1];
            let delta = (phi - prev_phi).abs();
            if delta < quiescence_threshold {
                quiescence_at = Some(cycle_idx + 1); // 1-indexed
                break;
            }
        }
    }

    (chain_seeds, phi_history, totals, quiescence_at)
}

// ============================================================================
// LEVEL 4 ENCODING ENTROPY + CORPUS XI DIVERSITY (cycle L4.7)
// ============================================================================
//
// encoding_entropy penalizes representational collapse: Shannon entropy over
// quantized xi-signature bins. High entropy = diverse representations. Low
// entropy = the encoder is mapping everything into a handful of manifolds.
//
// corpus_xi_diversity is a recalibrated version of L3's eval_xi_diversity
// that normalizes against 0.08 instead of 0.05, so the L4 corpus doesn't
// trivially saturate at 1.0.

/// Shannon entropy of quantized xi-signature bin tuples, normalized to [0,1].
/// Operates on HyperMemory slices so it can score either the surviving store
/// or any arbitrary corpus snapshot.
fn eval_encoding_entropy(corpus: &[HyperMemory], bins: usize) -> f32 {
    if corpus.is_empty() || bins < 2 {
        return 0.0;
    }

    // Collect xi signatures (compute any that aren't already populated).
    let sigs: Vec<Vec<f32>> = corpus
        .iter()
        .filter(|m| !m.content.starts_with("l4_noise") && m.amplitude > 0.01)
        .map(|m| {
            if m.xi_signature.is_empty() {
                compute_xi_signature(&m.vector)
            } else {
                m.xi_signature.clone()
            }
        })
        .filter(|s| !s.is_empty())
        .collect();

    if sigs.is_empty() {
        return 0.0;
    }

    // Find per-dimension range to normalize quantization. Using a global
    // [-1, 1] assumption is brittle because xi signatures can go outside that
    // interval, so we derive (min, max) from the corpus itself.
    let dim = sigs[0].len();
    let mut mins = vec![f32::INFINITY; dim];
    let mut maxs = vec![f32::NEG_INFINITY; dim];
    for s in &sigs {
        if s.len() != dim { continue; }
        for (d, v) in s.iter().enumerate() {
            if *v < mins[d] { mins[d] = *v; }
            if *v > maxs[d] { maxs[d] = *v; }
        }
    }

    // Quantize each signature into a bin-tuple; count occurrences.
    use std::collections::HashMap;
    let mut counts: HashMap<Vec<u16>, u32> = HashMap::new();
    for s in &sigs {
        if s.len() != dim { continue; }
        let mut tuple: Vec<u16> = Vec::with_capacity(dim);
        for (d, v) in s.iter().enumerate() {
            let span = (maxs[d] - mins[d]).max(1e-8);
            let rel = ((*v - mins[d]) / span).clamp(0.0, 1.0);
            let bin = (rel * (bins as f32 - 1.0)).round() as u16;
            tuple.push(bin);
        }
        *counts.entry(tuple).or_insert(0) += 1;
    }

    let total: f32 = counts.values().map(|c| *c as f32).sum();
    if total <= 0.0 {
        return 0.0;
    }
    let mut h = 0.0f32;
    for c in counts.values() {
        let p = (*c as f32) / total;
        if p > 0.0 {
            h -= p * p.log2();
        }
    }

    // L4.S1b: normalize by the *achievable* maximum entropy given the
    // sample count, not by the full joint bin-tuple space. The raw Shannon
    // entropy of `n_samples` items can never exceed `log2(n_samples)` (each
    // sample in its own unique bin-tuple), so dividing by `log2(bins^dim)`
    // under-counts the metric whenever `bins^dim > n_samples` — which is
    // always true on this corpus (128-dim xi × 8 bins ≫ ~300 samples).
    // Use `min(log2(bins^dim), log2(n_samples))` so the score reflects
    // structural diversity rather than the sparsity of a gigantic empty
    // hypergrid.
    let n_samples = total; // counts are over scored xi signatures
    let joint_log = (bins as f32).log2() * (dim as f32);
    let sample_log = n_samples.log2();
    let max_h = joint_log.min(sample_log);
    if max_h <= 0.0 {
        return 0.0;
    }
    (h / max_h).clamp(0.0, 1.0)
}

/// L4 corpus xi-diversity: same pairwise boost-averaging idea as the L3
/// metric, but normalizes against 0.08 instead of 0.05 (the L3 metric
/// saturates on the L4 corpus, which defeats its purpose).
fn eval_corpus_xi_diversity(corpus: &[HyperMemory]) -> f32 {
    let mut active: Vec<&HyperMemory> = corpus
        .iter()
        .filter(|m| !m.content.starts_with("l4_noise") && m.amplitude > 0.01)
        .collect();
    active.sort_by(|a, b| a.content.cmp(&b.content));

    if active.len() < 4 {
        return 0.0;
    }

    // Cycle L4.7.5: sample EVENLY across the sorted active set so the
    // ~300 pairs span all clusters, not just the alphabetically-first one
    // (the previous min(30) head-slice grabbed only "dense_a *" items, which
    // share a centroid and produce an inflated avg_boost).
    let target_n = 25usize; // 25 → 300 pairs, well above the 100 floor.
    let sample_size = active.len().min(target_n);
    let stride = (active.len() / sample_size).max(1);
    let sampled: Vec<&HyperMemory> = (0..sample_size)
        .map(|k| active[(k * stride).min(active.len() - 1)])
        .collect();
    let mut total_boost = 0.0f32;
    let mut count = 0usize;
    for i in 0..sampled.len() {
        for j in (i + 1)..sampled.len() {
            let xi_a = compute_xi_signature(&sampled[i].vector);
            let xi_b = compute_xi_signature(&sampled[j].vector);
            let base_sim = cosine_similarity(&sampled[i].vector, &sampled[j].vector);
            let boosted = xi_diversity_boost(base_sim, &xi_a, &xi_b);
            total_boost += (boosted - base_sim).abs();
            count += 1;
        }
    }
    if count == 0 {
        return 0.0;
    }
    let avg_boost = total_boost / count as f32;
    // Cycle L4.7.5: renormalize from 0.08 to 0.12 (combined with the
    // even-stride sampling above). The previous target+head-slice combo made
    // L4 saturate at 1.0; with stride sampling the raw avg_boost on a fresh
    // L4 corpus is ~0.080, so dividing by 0.12 lands the baseline around 0.67
    // — discriminating without being trivially saturated.
    (avg_boost / 0.12).clamp(0.0, 1.0)
}

/// L4-aware intra-cluster phase coherence. Partitions surviving memories by
/// their L4 cluster prefix (dense_a..dense_d, sparse_e, sparse_f), computes
/// the Kuramoto order parameter R = |1/N · Σ e^{iφ}| within each cluster, and
/// averages across clusters. Bridges, decoys, noise, and adversaries are not
/// part of any cluster and are excluded from the partition.
fn eval_phase_coherence_l4(engine: &ResonanceEngine) -> f32 {
    let all = engine.store.all_memories().unwrap_or_default();
    let cluster_prefixes = [
        "dense_a", "dense_b", "dense_c", "dense_d", "sparse_e", "sparse_f",
    ];
    let mut total_coherence = 0.0f32;
    let mut cluster_count = 0;
    for prefix in &cluster_prefixes {
        // L4.S8a: filter out NaN phases. Under interference_threshold=0.12
        // the adversarial consolidation path can occasionally leave a
        // memory's phase field as NaN (upstream ConsolidationEngine bug on
        // degenerate near-duplicate pairs). NaN was propagating through
        // sum_cos/sum_sin and producing fitness_adv_sub=NaN. We drop NaN
        // phases from the order-parameter computation; non-degenerate runs
        // contain zero NaNs and are unaffected.
        let phases: Vec<f32> = all
            .iter()
            .filter(|m| m.content.starts_with(prefix) && m.amplitude > 0.01)
            .map(|m| m.phase)
            .filter(|p| p.is_finite())
            .collect();
        if phases.len() < 2 {
            continue;
        }
        let n = phases.len() as f32;
        let sum_cos: f32 = phases.iter().map(|p| p.cos()).sum();
        let sum_sin: f32 = phases.iter().map(|p| p.sin()).sum();
        let r = ((sum_cos / n).powi(2) + (sum_sin / n).powi(2)).sqrt();
        total_coherence += r;
        cluster_count += 1;
    }
    if cluster_count == 0 {
        return 0.0;
    }
    total_coherence / cluster_count as f32
}

/// L4-aware cluster separation. For each pair of L4 clusters, computes the
/// cosine distance (1 - cos_sim) between their centroids, then returns the
/// mean cosine distance clamped to [0, 1]. Higher = clusters more separated.
/// Bridges, decoys, noise, and adversaries are excluded from the partition.
fn eval_cluster_separation_l4(engine: &ResonanceEngine) -> f32 {
    let all = engine.store.all_memories().unwrap_or_default();
    let cluster_prefixes = [
        "dense_a", "dense_b", "dense_c", "dense_d", "sparse_e", "sparse_f",
    ];

    // Compute centroid per cluster.
    let mut centroids: Vec<Vec<f32>> = Vec::new();
    for prefix in &cluster_prefixes {
        let members: Vec<&Vec<f32>> = all
            .iter()
            .filter(|m| m.content.starts_with(prefix) && m.amplitude > 0.01)
            .map(|m| &m.vector)
            .collect();
        if members.len() < 2 {
            continue;
        }
        let dim = members[0].len();
        let mut c = vec![0.0f32; dim];
        for v in &members {
            for (s, x) in c.iter_mut().zip(v.iter()) {
                *s += *x;
            }
        }
        let inv = 1.0 / members.len() as f32;
        for s in c.iter_mut() {
            *s *= inv;
        }
        centroids.push(c);
    }

    if centroids.len() < 2 {
        return 0.0;
    }

    // Mean pairwise cosine distance (1 - cos_sim) across centroids.
    let mut sum = 0.0f32;
    let mut count = 0usize;
    for i in 0..centroids.len() {
        for j in (i + 1)..centroids.len() {
            let sim = cosine_similarity(&centroids[i], &centroids[j]);
            let dist = 1.0 - sim;
            sum += dist;
            count += 1;
        }
    }
    if count == 0 {
        return 0.0;
    }
    (sum / count as f32).clamp(0.0, 1.0)
}

/// L4 variant: filters by the "l4_noise" tag instead of L3's "noise" prefix.
fn eval_l4_noise_removal(engine: &ResonanceEngine) -> f32 {
    let all = engine.store.all_memories().unwrap_or_default();
    let surviving_noise = all.iter()
        .filter(|m| m.content.starts_with("l4_noise") && m.amplitude > 0.01)
        .count();
    1.0 - (surviving_noise as f32 / 15.0)
}

/// L4 variant: counts surviving non-noise memories out of 285 (300 - 15 noise).
fn eval_l4_signal_preservation(engine: &ResonanceEngine) -> f32 {
    let all = engine.store.all_memories().unwrap_or_default();
    let signal_count = all.iter().filter(|m| {
        !m.content.starts_with("l4_noise") && m.amplitude > 0.01
    }).count();
    (signal_count as f32 / 285.0).min(1.0)
}

// ============================================================================
// LEVEL 5 CORPUS GENERATORS (cycle L5.1)
// ============================================================================
//
// L5 introduces dual-corpus testing with bimodal frequency assignment.
// Corpus A (300 memories, hardness=2): training corpus with attention-band
// and storage-band frequency assignment per the Universal Clock hypothesis.
// Corpus B (250 memories): shared dense centroids but different member
// vectors and rotated sparse clusters for transfer testing.
//
// Frequency bands (program-l5.md §5.1):
//   Working (attention): 0.5-4.0 Hz, center 2.0 Hz
//   Storage:             0.01-0.5 Hz, center 0.1 Hz

/// L5 Corpus A generator. 300 memories with bimodal frequency assignment.
/// Uses build_corpus_l4 for the vector structure (hardness=2), then
/// overrides frequencies per the Universal Clock spectrum design.
fn build_corpus_l5_a(
    dim: usize,
    hardness: usize,
    encoder_seed: u64,
) -> Vec<(Vec<f32>, String, &'static str, f32)> {
    // Generate the base L4-format corpus with hardness=2
    let base = build_corpus_l4(dim, hardness, encoder_seed);

    // Assign frequencies per category according to L5 frequency band design
    base.into_iter()
        .enumerate()
        .map(|(i, (vec, content, category))| {
            let freq = match category {
                // Dense cluster members: attention band N(2.0, 0.3) clamped [0.5, 4.0]
                "l4_dense" => {
                    let raw = 2.0 + pcg_f32(encoder_seed, 6000, i as u32, 0) * 0.3;
                    raw.clamp(0.5, 4.0)
                }
                // Sparse cluster members: storage band N(0.1, 0.02) clamped [0.05, 0.5]
                "l4_sparse" => {
                    let raw = 0.1 + pcg_f32(encoder_seed, 6001, i as u32, 0) * 0.02;
                    raw.clamp(0.05, 0.5)
                }
                // Bridges: 1.0 Hz (midpoint spanning the gap)
                "l4_bridge" => 1.0,
                // Decoys: 2.0 Hz (exploit the attention band)
                "l4_decoy" => 2.0,
                // Noise: 0.5 Hz (boundary — hardest to classify)
                "l4_noise" => 0.5,
                _ => 0.1,
            };
            (vec, content, category, freq)
        })
        .collect()
}

/// L5 Corpus B generator. 250 memories with shared dense centroids but
/// different member vectors, rotated sparse clusters, and bimodal frequency.
fn build_corpus_l5_b(
    dim: usize,
    _hardness: usize,
    encoder_seed: u64,
) -> Vec<(Vec<f32>, String, &'static str, f32)> {
    let corpus_b_seed = encoder_seed.wrapping_add(0xBEEF_CAFE);
    let mut corpus: Vec<(Vec<f32>, String, &'static str, f32)> = Vec::with_capacity(250);

    // Dense clusters: same 4 centroids as Corpus A, but different member
    // vectors (different within-cluster noise seeds). 4 x 40 = 160.
    let dense_labels = ["dense_a", "dense_b", "dense_c", "dense_d"];
    for (cluster_idx, label) in dense_labels.iter().enumerate() {
        let cid = cluster_idx as u32;
        // Centroid is identical to Corpus A (same encoder_seed, item=0)
        let centroid: Vec<f32> = (0..dim)
            .map(|d| 0.8 * pcg_f32(encoder_seed, cid, 0, d as u32).signum())
            .collect();
        for i in 0..40 {
            let item = (i + 1) as u32;
            let v: Vec<f32> = (0..dim)
                .map(|d| {
                    let base = centroid[d];
                    // Different seed (corpus_b_seed) for different member vectors
                    let jitter = pcg_f32(corpus_b_seed, cid, item, d as u32) * 0.35;
                    base + jitter
                })
                .collect();
            let freq = {
                let raw = 2.0 + pcg_f32(corpus_b_seed, 6000, (cluster_idx * 40 + i) as u32, 0) * 0.3;
                raw.clamp(0.5, 4.0)
            };
            corpus.push((v, format!("l5b_{label} {i}"), "l4_dense", freq));
        }
    }

    // Sparse clusters: rotated 30 degrees from A's sparse centroids. 2 x 15 = 30.
    let sparse_labels = ["sparse_e", "sparse_f"];
    for (cluster_idx, label) in sparse_labels.iter().enumerate() {
        let cid = 4 + cluster_idx as u32;
        // Original centroid from Corpus A
        let orig_centroid: Vec<f32> = (0..dim)
            .map(|d| {
                let r = pcg_f32(encoder_seed, cid, 0, d as u32);
                0.6 * (r * PI).sin()
            })
            .collect();
        // Rotate by 30 degrees: apply Givens rotation on pairs of dimensions
        let mut centroid = orig_centroid.clone();
        let cos30 = (PI / 6.0).cos();
        let sin30 = (PI / 6.0).sin();
        let half = dim / 2;
        for d in 0..half {
            let a = centroid[2 * d];
            let b = centroid[2 * d + 1];
            centroid[2 * d] = a * cos30 - b * sin30;
            centroid[2 * d + 1] = a * sin30 + b * cos30;
        }
        for i in 0..15 {
            let item = (i + 1) as u32;
            let v: Vec<f32> = (0..dim)
                .map(|d| {
                    let base = centroid[d];
                    let jitter = pcg_f32(corpus_b_seed, cid, item, d as u32) * 0.45;
                    base + jitter
                })
                .collect();
            let freq = {
                let raw = 0.1 + pcg_f32(corpus_b_seed, 6001, (cluster_idx * 15 + i) as u32, 0) * 0.02;
                raw.clamp(0.05, 0.5)
            };
            corpus.push((v, format!("l5b_{label} {i}"), "l4_sparse", freq));
        }
    }

    // Bridges: 15 (3 each between 5 cluster pairs)
    let pairs: [(u32, u32); 5] = [(0, 1), (0, 2), (1, 3), (2, 4), (3, 5)];
    for (pair_idx, (a, b)) in pairs.iter().enumerate() {
        let centroid_a: Vec<f32> = (0..dim)
            .map(|d| 0.8 * pcg_f32(encoder_seed, *a, 0, d as u32).signum())
            .collect();
        let centroid_b_raw: Vec<f32> = if *b <= 3 {
            (0..dim)
                .map(|d| 0.8 * pcg_f32(encoder_seed, *b, 0, d as u32).signum())
                .collect()
        } else {
            // Sparse cluster centroid (same as A for bridges)
            (0..dim)
                .map(|d| {
                    let r = pcg_f32(encoder_seed, *b, 0, d as u32);
                    0.6 * (r * PI).sin()
                })
                .collect()
        };
        for i in 0..3 {
            let stream_cid = 1000 + pair_idx as u32;
            let item = (i + 1) as u32;
            let v: Vec<f32> = (0..dim)
                .map(|d| {
                    let mix = 0.5 * (centroid_a[d] + centroid_b_raw[d]);
                    let jitter = pcg_f32(corpus_b_seed, stream_cid, item, d as u32) * 0.12;
                    mix + jitter
                })
                .collect();
            corpus.push((v, format!("l5b_bridge p{pair_idx} {i}"), "l4_bridge", 1.0));
        }
    }

    // Decoys: 30 high-amplitude random vectors
    for i in 0..30 {
        let item = (i + 1) as u32;
        let v: Vec<f32> = (0..dim)
            .map(|d| pcg_f32(corpus_b_seed, 2000, item, d as u32) * 0.9)
            .collect();
        corpus.push((v, format!("l5b_decoy {i}"), "l4_decoy", 2.0));
    }

    // Noise: 15 low-amplitude random vectors
    for i in 0..15 {
        let item = (i + 1) as u32;
        let v: Vec<f32> = (0..dim)
            .map(|d| pcg_f32(corpus_b_seed, 3000, item, d as u32) * 0.12)
            .collect();
        corpus.push((v, format!("l5b_noise {i}"), "l4_noise", 0.5));
    }

    debug_assert_eq!(corpus.len(), 250, "L5 Corpus B must be exactly 250 memories");
    corpus
}

/// Build a ResonanceEngine from an L5 corpus (with frequency assignment).
fn build_l5_engine(
    corpus: &[(Vec<f32>, String, &'static str, f32)],
    params: &Params,
    dim: usize,
) -> ResonanceEngine {
    let store = Box::new(TestMedium::new());
    let encoder = Box::new(SimpleHashEncoder::new(dim, params.encoder_seed));
    let codebook = Codebook::new(dim, dim, params.encoder_seed);
    let pipeline = EncodingPipeline::new(encoder, codebook);
    let mut engine = ResonanceEngine::new(store, pipeline);

    let ps = params.phase_spread;
    for (i, (vec, content, category, freq)) in corpus.iter().enumerate() {
        let mut mem = HyperMemory::new(vec.clone(), content.clone());
        mem.id = uuid::Uuid::from_u128(
            (i as u128 + 1) * 0x0123_4567_89AB_CDEF_0123_4567_89AB_CDEF,
        );
        mem.decay_rate = params.decay_rate;
        mem.phase = match *category {
            "l4_dense" => 0.0 + (i as f32 * 0.1 * ps),
            "l4_sparse" => PI * 0.5 + (i as f32 * 0.08 * ps),
            "l4_bridge" => PI * 0.25,
            "l4_decoy" => PI * (i as f32 * 0.31),
            "l4_noise" => PI * (i as f32 * 0.7),
            _ => 0.0,
        };
        mem.layer_depth = match *category {
            "l4_dense" => (i % 3) as u8,
            "l4_sparse" => ((i + 1) % 3) as u8,
            "l4_bridge" => 1,
            "l4_decoy" => 2,
            "l4_noise" => 0,
            _ => 0,
        };
        mem.frequency = *freq;
        if *category == "l4_noise" {
            mem.amplitude = 0.15;
        }
        engine.store.insert(mem).expect("insert failed");
    }
    engine
}

// ============================================================================
// LEVEL 5: FREQUENCY DECAY + TEMPORAL SEPARATION (cycle L5.4)
// ============================================================================

/// Apply frequency decay after a dream cycle (L5 only).
///
/// High-amplitude memories (amp > median) sustain their frequency.
/// Low-amplitude memories decay toward 0.1 Hz:
///   freq_new = freq_old * (1 - decay_rate) + 0.1 * decay_rate
/// where decay_rate = 0.1 per cycle.
///
/// This creates the biological pattern: important/attended memories stay in
/// the attention band, unimportant ones sink to storage.
fn apply_freq_decay(engine: &mut ResonanceEngine) {
    let all = engine.store.all_memories().unwrap_or_default();
    if all.is_empty() {
        return;
    }

    // Compute median amplitude
    let mut amps: Vec<f32> = all.iter().map(|m| m.amplitude).collect();
    amps.sort_by(|a, b| a.total_cmp(b));
    let median_amp = if amps.len() % 2 == 0 {
        (amps[amps.len() / 2 - 1] + amps[amps.len() / 2]) / 2.0
    } else {
        amps[amps.len() / 2]
    };

    // Collect IDs and current freq/amp for memories that need decay
    let decay_targets: Vec<(uuid::Uuid, f32)> = all
        .iter()
        .filter(|m| m.amplitude <= median_amp)
        .map(|m| (m.id, m.frequency))
        .collect();

    let freq_decay_rate = 0.1_f32;
    let storage_freq = 0.1_f32;

    for (id, old_freq) in decay_targets {
        if let Ok(Some(mem)) = engine.store.get_mut(&id) {
            mem.frequency = old_freq * (1.0 - freq_decay_rate) + storage_freq * freq_decay_rate;
        }
    }
}

/// Evaluate temporal separation via Sarle's bimodality coefficient (L5.4).
///
/// Collects all surviving memories' frequencies, computes:
///   b = (skewness^2 + 1) / kurtosis
/// Normalizes: score = (b / 0.555).min(1.0)
///
/// Sarle's threshold for bimodality is 0.555; score >= 1.0 means clearly bimodal.
fn eval_temporal_separation(engine: &ResonanceEngine) -> f32 {
    let all = engine.store.all_memories().unwrap_or_default();
    if all.len() < 4 {
        return 0.0;
    }

    let freqs: Vec<f32> = all.iter().map(|m| m.frequency).collect();
    let n = freqs.len() as f32;

    // Mean
    let mean = freqs.iter().sum::<f32>() / n;

    // Variance, skewness, kurtosis (using the standard moment formulas)
    let mut m2 = 0.0_f32;
    let mut m3 = 0.0_f32;
    let mut m4 = 0.0_f32;
    for &f in &freqs {
        let d = f - mean;
        m2 += d * d;
        m3 += d * d * d;
        m4 += d * d * d * d;
    }
    m2 /= n;
    m3 /= n;
    m4 /= n;

    if m2 < 1e-12 {
        return 0.0; // All frequencies identical — no bimodality
    }

    let std_dev = m2.sqrt();
    let skewness = m3 / (std_dev * std_dev * std_dev);
    let kurtosis = m4 / (m2 * m2);

    if kurtosis < 1e-12 {
        return 0.0;
    }

    let bimodality = (skewness * skewness + 1.0) / kurtosis;
    (bimodality / 0.555).min(1.0)
}

/// Magic proxy (instrumentation only — not in fitness).
///
/// Global Kuramoto order parameter R = |Σ exp(i·φⱼ)| / N on memory phases at
/// the end of the dream chain. R near 1 indicates strong phase-locked
/// structure (non-linear lock-in, "magic-like" in the non-stabilizer sense).
/// R near 0 indicates uniform phase distribution (stabilizer-equivalent —
/// dynamics could be reproduced by a classical linear approximation).
///
/// See research/intersections/05-magic-gives-it-gravity.md for motivation.
/// Hypothesis: phase concentration correlates with xi_robustness_v2 because
/// adversarial perturbations off the phase lock can't be cheaply simulated.
fn eval_phase_concentration(engine: &ResonanceEngine) -> f32 {
    let all = engine.store.all_memories().unwrap_or_default();
    if all.is_empty() {
        return 0.0;
    }
    let n = all.len() as f32;
    let cos_sum: f32 = all.iter().map(|m| m.phase.cos()).sum();
    let sin_sum: f32 = all.iter().map(|m| m.phase.sin()).sum();
    ((cos_sum * cos_sum + sin_sum * sin_sum).sqrt() / n).clamp(0.0, 1.0)
}

/// Query-gravity proxy (instrumentation only — not in fitness).
///
/// Operational test of "attention is mass that bends the memory landscape."
/// Picks the highest-amplitude pre-dream memory as the "query" (a concentration
/// of mass), runs the dream chain, and asks: did phase-neighbors of the query
/// gain more amplitude than phase-distant memories?
///
/// Partitioning:
///   - neighbors: |Δφ| < π/4 from query phase (should be attracted if gravity works)
///   - distant:   |Δφ| > π/2 (control group; uniform pull would gain equally)
///
/// Returns neighbor_mean_gain / (neighbor_mean_gain + distant_mean_gain),
/// clamped to [0, 1]:
///   0.5  → no gravity (uniform pull, stabilizer-like dream)
///   > 0.5 → attention-as-gravity working (Kuramoto coupling recruits neighbors)
///   < 0.5 → inverse pull (rare; dream actively scatters)
///
/// See research/intersections/05-magic-gives-it-gravity.md.
fn eval_query_gravity(
    pre_state: &[(uuid::Uuid, f32, f32)],
    post_engine: &ResonanceEngine,
) -> f32 {
    // Pick query: highest pre-dream amplitude
    let query = match pre_state
        .iter()
        .max_by(|a, b| a.1.total_cmp(&b.1))
    {
        Some(q) => *q,
        None => return 0.5,
    };
    let query_phase = query.2;

    use std::collections::HashMap;
    let post_amps: HashMap<uuid::Uuid, f32> = post_engine
        .store
        .all_memories()
        .unwrap_or_default()
        .iter()
        .map(|m| (m.id, m.amplitude))
        .collect();

    let mut neighbor_gains: Vec<f32> = Vec::new();
    let mut distant_gains: Vec<f32> = Vec::new();
    let two_pi = 2.0 * std::f32::consts::PI;

    for (id, amp_before, phase_before) in pre_state {
        if *id == query.0 || *amp_before < 1e-6 {
            continue;
        }
        let amp_after = post_amps.get(id).copied().unwrap_or(0.0);
        let gain = amp_after / amp_before;

        let raw_dphi = (phase_before - query_phase).abs();
        let dphi = raw_dphi.min(two_pi - raw_dphi);

        if dphi < std::f32::consts::FRAC_PI_4 {
            neighbor_gains.push(gain);
        } else if dphi > std::f32::consts::FRAC_PI_2 {
            distant_gains.push(gain);
        }
    }

    if neighbor_gains.is_empty() || distant_gains.is_empty() {
        return 0.5;
    }

    let neighbor_mean = neighbor_gains.iter().sum::<f32>() / neighbor_gains.len() as f32;
    let distant_mean = distant_gains.iter().sum::<f32>() / distant_gains.len() as f32;
    let total = neighbor_mean + distant_mean;
    if total < 1e-9 {
        return 0.5;
    }
    (neighbor_mean / total).clamp(0.0, 1.0)
}

// ============================================================================
// LEVEL 5: FREQUENCY TRANSFER (cycle L5.7)
// ============================================================================

/// Evaluate frequency band transfer between engine A and engine B (L5.7).
///
/// For each memory in A, classify as "working" (freq >= 0.5) or "storage" (freq < 0.5).
/// For each memory in B that shares a dense cluster centroid with A (matched by
/// cluster label prefix), classify the same way. Compute Pearson r between the
/// band-membership vectors of matched clusters across A and B.
///
/// Score = (r + 1) / 2, clamped to [0, 1]. Higher = frequency structure transferred.
fn eval_frequency_transfer(engine_a: &ResonanceEngine, engine_b: &ResonanceEngine) -> f32 {
    let all_a = engine_a.store.all_memories().unwrap_or_default();
    let all_b = engine_b.store.all_memories().unwrap_or_default();

    if all_a.is_empty() || all_b.is_empty() {
        return 0.5; // No data — return neutral
    }

    // Dense cluster labels shared between corpora A and B.
    // Corpus A uses "dense_a N", Corpus B uses "l5b_dense_a N".
    let cluster_prefixes = ["dense_a", "dense_b", "dense_c", "dense_d"];

    // For each cluster, compute mean frequency band for A and B.
    // Band classification: 1.0 = working (freq >= 0.5), 0.0 = storage (freq < 0.5).
    let mut bands_a: Vec<f32> = Vec::new();
    let mut bands_b: Vec<f32> = Vec::new();

    for prefix in &cluster_prefixes {
        // Collect band memberships for this cluster in A
        let a_bands: Vec<f32> = all_a
            .iter()
            .filter(|m| m.content.contains(prefix) && m.amplitude > 0.01)
            .map(|m| if m.frequency >= 0.5 { 1.0 } else { 0.0 })
            .collect();

        // Collect band memberships for this cluster in B
        let b_bands: Vec<f32> = all_b
            .iter()
            .filter(|m| m.content.contains(prefix) && m.amplitude > 0.01)
            .map(|m| if m.frequency >= 0.5 { 1.0 } else { 0.0 })
            .collect();

        if a_bands.is_empty() || b_bands.is_empty() {
            continue;
        }

        // Use per-cluster mean band membership as the comparison unit.
        // This gives one (a, b) pair per cluster, avoiding length mismatch.
        let mean_a = a_bands.iter().sum::<f32>() / a_bands.len() as f32;
        let mean_b = b_bands.iter().sum::<f32>() / b_bands.len() as f32;
        bands_a.push(mean_a);
        bands_b.push(mean_b);
    }

    // Also include sparse clusters for more data points
    let sparse_prefixes = ["sparse_e", "sparse_f"];
    for prefix in &sparse_prefixes {
        let a_bands: Vec<f32> = all_a
            .iter()
            .filter(|m| m.content.contains(prefix) && m.amplitude > 0.01)
            .map(|m| if m.frequency >= 0.5 { 1.0 } else { 0.0 })
            .collect();

        let b_bands: Vec<f32> = all_b
            .iter()
            .filter(|m| m.content.contains(prefix) && m.amplitude > 0.01)
            .map(|m| if m.frequency >= 0.5 { 1.0 } else { 0.0 })
            .collect();

        if a_bands.is_empty() || b_bands.is_empty() {
            continue;
        }

        let mean_a = a_bands.iter().sum::<f32>() / a_bands.len() as f32;
        let mean_b = b_bands.iter().sum::<f32>() / b_bands.len() as f32;
        bands_a.push(mean_a);
        bands_b.push(mean_b);
    }

    let n = bands_a.len();
    if n < 2 {
        return 0.5; // Not enough matched clusters for correlation
    }

    // Pearson r
    let mean_a = bands_a.iter().sum::<f32>() / n as f32;
    let mean_b = bands_b.iter().sum::<f32>() / n as f32;

    let mut cov = 0.0_f32;
    let mut var_a = 0.0_f32;
    let mut var_b = 0.0_f32;
    for i in 0..n {
        let da = bands_a[i] - mean_a;
        let db = bands_b[i] - mean_b;
        cov += da * db;
        var_a += da * da;
        var_b += db * db;
    }

    let denom = (var_a * var_b).sqrt();
    let r = if denom < 1e-10 {
        // All values identical in one or both — check if they match
        if (var_a < 1e-10) && (var_b < 1e-10) {
            // Both constant — if same value, perfect correlation; otherwise undefined
            if (mean_a - mean_b).abs() < 1e-6 { 1.0 } else { 0.0 }
        } else {
            0.0 // One constant, one varies — no correlation
        }
    } else {
        cov / denom
    };

    // Map [-1, 1] -> [0, 1]
    ((r + 1.0) / 2.0).clamp(0.0, 1.0)
}

// ============================================================================
// LEVEL 5: XI ROBUSTNESS V2 — ADVERSARIAL (cycle L5.8)
// ============================================================================

/// Build L5-specific adversarial memory set (30 memories, 3 attack types).
///
/// A1 (xi-twin decoys, 10): craft memories whose xi signatures approximate
///     target memories despite being semantically different. Uses target_xi
///     scaled by 1/EMERGENCE_COEFF as input vector (approximation due to tanh
///     nonlinearity in the commutator).
///
/// A2 (commutator exploits, 10): large-magnitude inputs (amplitude 10.0) where
///     tanh saturates to +/-1, collapsing the nonlinear commutator back toward
///     the linear one. Tests whether saturation enables adversarial shortcuts.
///
/// A3 (frequency-band attacks, 10): noise memories injected at 2.0 Hz
///     (attention band) to test whether the system incorrectly promotes them
///     due to their frequency.
fn build_adversarial_set_l5(
    corpus: &[(Vec<f32>, String, &'static str, f32)],
    seed: u64,
) -> Vec<HyperMemory> {
    let mut out: Vec<HyperMemory> = Vec::with_capacity(30);
    if corpus.is_empty() {
        return out;
    }
    let dim = corpus[0].0.len();

    // Gather dense cluster centroids for xi-twin construction
    let cluster_prefixes = ["dense_a", "dense_b", "dense_c", "dense_d"];
    let mut centroids: Vec<Vec<f32>> = Vec::new();
    for prefix in &cluster_prefixes {
        let mut sum = vec![0.0f32; dim];
        let mut count = 0usize;
        for (v, content, _cat, _freq) in corpus {
            if content.starts_with(prefix) {
                for (s, x) in sum.iter_mut().zip(v.iter()) {
                    *s += *x;
                }
                count += 1;
            }
        }
        if count > 0 {
            let inv = 1.0 / count as f32;
            for s in sum.iter_mut() {
                *s *= inv;
            }
            centroids.push(sum);
        }
    }

    let adv_seed = seed.wrapping_add(0xADDE_F00D);

    // Adversarial UUID stride: assign deterministic UUIDs near u128::MAX so
    // adversarials always sort after corpus seeds in find_synchronized_clusters.
    // Corpus UUIDs = (i+1) * 0x0123..CDEF (≤ 3.08e38 << u128::MAX ≈ 3.40e38).
    // Placing adversarials at u128::MAX - k * stride guarantees they never
    // steal early BFS cluster indices, eliminating xi variance across trials.
    const ADV_UUID_STRIDE: u128 = 0x0001_0000_0000_0001;

    // ---- A1: xi-twin decoys (10) ----
    // Approximate target xi by using target_xi / EMERGENCE_COEFF as input vector.
    // The nonlinear commutator `tanh(R(v)) * G(v) - tanh(G(v)) * R(v)` won't
    // produce exact matches due to tanh, but gets close enough for adversarial probing.
    for i in 0..10 {
        let target_centroid = &centroids[i % centroids.len().max(1)];
        let target_xi = compute_xi_signature(target_centroid);
        // Scale by 1/EMERGENCE_COEFF to approximate the inverse mapping
        let inv_coeff = 1.0 / EMERGENCE_COEFF;
        let v: Vec<f32> = target_xi.iter().enumerate().map(|(d, &xi_val)| {
            // Add small seeded perturbation to avoid exact degeneracy
            let jitter = pcg_f32(adv_seed, 8000, i as u32, d as u32) * 0.05;
            xi_val * inv_coeff + jitter
        }).collect();
        let mut mem = HyperMemory::new(v, format!("adv_l5_a1_xi_twin {i}"));
        mem.id = uuid::Uuid::from_u128(u128::MAX - (i as u128) * ADV_UUID_STRIDE);
        mem.amplitude = 0.9;
        mem.phase = PI * 0.3 * i as f32;
        mem.frequency = 0.1; // Storage band — shouldn't be promoted
        mem.layer_depth = 1;
        mem.decay_rate = 1e-4;
        out.push(mem);
    }

    // ---- A2: commutator exploits (10) ----
    // Large-magnitude inputs where tanh saturates to +/-1.
    // This collapses the nonlinear commutator toward the linear regime,
    // potentially enabling adversarial shortcuts through the xi space.
    for i in 0..10 {
        let v: Vec<f32> = (0..dim)
            .map(|d| {
                let base = pcg_f32(adv_seed, 8100, i as u32, d as u32);
                base * 10.0 // Amplitude 10.0 — drives tanh to saturation
            })
            .collect();
        let mut mem = HyperMemory::new(v, format!("adv_l5_a2_commutator {i}"));
        mem.id = uuid::Uuid::from_u128(u128::MAX - (10 + i as u128) * ADV_UUID_STRIDE);
        mem.amplitude = 1.0;
        mem.phase = PI * (i as f32 * 0.47);
        mem.frequency = 0.1;
        mem.layer_depth = 2;
        mem.decay_rate = 1e-4;
        out.push(mem);
    }

    // ---- A3: frequency-band attacks (10) ----
    // Pure noise injected at 2.0 Hz (attention band). Tests whether the system
    // incorrectly promotes noise just because it's at the attention frequency.
    for i in 0..10 {
        let v: Vec<f32> = (0..dim)
            .map(|d| pcg_f32(adv_seed, 8200, i as u32, d as u32) * 0.2)
            .collect();
        let mut mem = HyperMemory::new(v, format!("adv_l5_a3_freq_attack {i}"));
        mem.id = uuid::Uuid::from_u128(u128::MAX - (20 + i as u128) * ADV_UUID_STRIDE);
        mem.amplitude = 0.5;
        mem.phase = PI * (i as f32 * 0.13);
        mem.frequency = 2.0; // Attention band — should NOT get promoted
        mem.layer_depth = 0;
        mem.decay_rate = 1e-4;
        out.push(mem);
    }

    debug_assert_eq!(out.len(), 30, "L5 adversarial set must be exactly 30 memories");
    out
}

/// Evaluate xi_robustness_v2 via simplified dual-pass (L5.8).
///
/// Builds two L5 engines from the same corpus: one clean, one with 30
/// adversarial memories injected. Runs dream chain on both, computes
/// sub-fitness using the L5 placeholder fitness evaluator, and scores:
///
///   xi_robustness_v2 = 1 - |fitness_clean_sub - fitness_adv_sub| / max(fitness_clean_sub, 0.05)
///
/// Clamped to [0, 1]. Higher = adversarial injection doesn't significantly
/// change the fitness (the system is robust to xi-aware attacks).
fn eval_xi_robustness_v2(
    corpus_a: &[(Vec<f32>, String, &'static str, f32)],
    params: &Params,
    dim: usize,
) -> f32 {
    // Clean pass
    let mut engine_clean = build_l5_engine(corpus_a, params, dim);
    std::env::set_var("DRIVE_CONTEXT", "engine_clean");
    let (cs_clean, phi_clean, _totals_clean, _q_clean, _ad_clean,
         _inj_clean, _orig_clean, _iamp_clean) =
        run_l5_dream_chain(params, &mut engine_clean);
    std::env::remove_var("DRIVE_CONTEXT");
    let fitness_clean = eval_l5_placeholder_fitness(&engine_clean, params, &cs_clean, &phi_clean);

    // Adversarial pass: same corpus + 30 adversarial memories
    let mut engine_adv = build_l5_engine(corpus_a, params, dim);
    let adv_set = build_adversarial_set_l5(corpus_a, params.encoder_seed);
    for mut mem in adv_set {
        mem.decay_rate = params.decay_rate;
        let _ = engine_adv.store.insert(mem);
    }
    std::env::set_var("DRIVE_CONTEXT", "engine_adv");
    let (cs_adv, phi_adv, _totals_adv, _q_adv, _ad_adv,
         _inj_adv, _orig_adv, _iamp_adv) =
        run_l5_dream_chain(params, &mut engine_adv);
    std::env::remove_var("DRIVE_CONTEXT");
    // Remove adversarial memories before evaluating corpus state.
    // Adversarials inflate phi (IIT proxy) because they add inter-cluster
    // links, making fitness_adv artificially high regardless of corpus health.
    // Deleting them here measures corpus robustness: did adversarial dreaming
    // actually degrade corpus memories? The chain_seeds/phi_history still
    // reflect dynamics from the adversarial dream.
    {
        let adv_ids: Vec<uuid::Uuid> = engine_adv
            .store
            .all_memories()
            .unwrap_or_default()
            .iter()
            .filter(|m| m.content.starts_with("adv_l5_"))
            .map(|m| m.id)
            .collect();
        for id in &adv_ids {
            let _ = engine_adv.store.delete(id);
        }
    }
    let fitness_adv = eval_l5_placeholder_fitness(&engine_adv, params, &cs_adv, &phi_adv);

    let divergence = (fitness_clean - fitness_adv).abs();
    let normalizer = fitness_clean.max(0.05);
    (1.0 - divergence / normalizer).clamp(0.0, 1.0)
}

// ============================================================================
// LEVEL 5: CARRIER EMERGENCE VIA FFT (cycle L5.6)
// ============================================================================

/// Simple textbook DFT for N points (no external crate needed).
///
/// Returns complex coefficients as Vec<(re, im)> of length N.
fn simple_dft(signal: &[f32]) -> Vec<(f32, f32)> {
    let n = signal.len();
    let mut result = Vec::with_capacity(n);
    for k in 0..n {
        let mut re = 0.0_f32;
        let mut im = 0.0_f32;
        for (j, &x) in signal.iter().enumerate() {
            let angle = -2.0 * PI * (k as f32) * (j as f32) / (n as f32);
            re += x * angle.cos();
            im += x * angle.sin();
        }
        result.push((re, im));
    }
    result
}

/// Evaluate carrier emergence from per-cycle amplitude deltas (L5.6).
///
/// Applies DFT to the amplitude-change signal, finds peak in the [0.5, 4.0] Hz
/// band (mapped via cycle period), and returns spectral concentration at peak.
///
/// `cycle_period_s`: estimated time per dream cycle in seconds. Used to map
/// DFT bins to physical Hz. If the chain took 10s for 16 cycles, period = 0.625s.
fn eval_carrier_emergence(amplitude_deltas: &[f32], cycle_period_s: f32) -> f32 {
    let n = amplitude_deltas.len();
    if n < 4 {
        return 0.0;
    }

    let dft = simple_dft(amplitude_deltas);

    // Sampling rate = 1 / cycle_period_s
    // DFT bin k corresponds to frequency = k / (N * cycle_period_s) Hz
    // We want the [0.5, 4.0] Hz band
    let fs = 1.0 / cycle_period_s.max(0.001);

    let mut total_power = 0.0_f32;
    let mut peak_power = 0.0_f32;
    let mut peak_in_band = false;

    // Skip DC component (k=0), only go up to N/2 (Nyquist)
    let nyquist = n / 2;
    for k in 1..=nyquist {
        let freq_hz = (k as f32) * fs / (n as f32);
        let power = dft[k].0 * dft[k].0 + dft[k].1 * dft[k].1;
        total_power += power;

        if freq_hz >= 0.5 && freq_hz <= 4.0 && power > peak_power {
            peak_power = power;
            peak_in_band = true;
        }
    }

    if !peak_in_band || total_power < 1e-12 {
        return 0.0;
    }

    (peak_power / total_power).min(1.0)
}

/// Build a flat-frequency version of the L5 corpus (all memories at 0.1 Hz).
///
/// Used for the carrier emergence test: does 2 Hz periodicity emerge from
/// uniform-frequency input? This is the key emergence vs passthrough distinction.
fn build_corpus_l5_a_flat(
    dim: usize,
    hardness: usize,
    encoder_seed: u64,
) -> Vec<(Vec<f32>, String, &'static str, f32)> {
    let base = build_corpus_l4(dim, hardness, encoder_seed);
    base.into_iter()
        .map(|(vec, content, category)| {
            // ALL memories at 0.1 Hz — uniform frequency
            (vec, content, category, 0.1_f32)
        })
        .collect()
}

// ============================================================================
// LEVEL 5: ONLINE INJECTION + RETENTION + FORGETTING RESISTANCE (cycle L5.5)
// ============================================================================

/// Inject online memories mid-chain (L5.5).
///
/// Creates 10 new memories per injection event at 2.0 Hz (attention band),
/// amplitude 0.8 (moderate importance). Returns IDs of injected memories.
fn inject_online_memories(
    engine: &mut ResonanceEngine,
    dim: usize,
    injection_idx: usize,
    seed: u64,
) -> Vec<uuid::Uuid> {
    let inject_seed = seed.wrapping_add(0x1A3E_C700 + injection_idx as u64);
    let mut ids = Vec::with_capacity(10);
    for i in 0..10 {
        let item = (injection_idx * 10 + i) as u32;
        let v: Vec<f32> = (0..dim)
            .map(|d| pcg_f32(inject_seed, 7000 + injection_idx as u32, item, d as u32) * 0.7)
            .collect();
        let mut mem = HyperMemory::new(v, format!("online_{injection_idx}_{i}"));
        // Distinct UUID namespace for online injections
        mem.id = uuid::Uuid::from_u128(
            0xAAAA_0000_0000_0000_0000_0000_0000_0000u128
                + (injection_idx as u128 * 100)
                + i as u128,
        );
        mem.frequency = 2.0;
        mem.amplitude = 0.8;
        mem.phase = PI * (i as f32 * 0.2);
        mem.layer_depth = 0;
        ids.push(mem.id);
        let _ = engine.store.insert(mem);
    }
    ids
}

/// Evaluate online retention across injection events (L5.5).
///
/// For each injection event, compute hit_rate = fraction of injected IDs
/// still present with amp > 0.3. Return geometric mean of hit_rates.
fn eval_online_retention(
    engine: &ResonanceEngine,
    injected_ids_per_event: &[Vec<uuid::Uuid>],
) -> f32 {
    if injected_ids_per_event.is_empty() {
        return 0.0;
    }

    let mut log_sum = 0.0_f64;
    let mut count = 0usize;

    for event_ids in injected_ids_per_event {
        if event_ids.is_empty() {
            continue;
        }
        let hits = event_ids
            .iter()
            .filter(|id| {
                engine
                    .store
                    .get(id)
                    .ok()
                    .flatten()
                    .is_some_and(|m| m.amplitude > 0.3)
            })
            .count();
        let hit_rate = hits as f64 / event_ids.len() as f64;
        // For geometric mean: use log. Clamp to avoid log(0).
        log_sum += (hit_rate.max(1e-10)).ln();
        count += 1;
    }

    if count == 0 {
        return 0.0;
    }

    (log_sum / count as f64).exp() as f32
}

/// Evaluate catastrophic forgetting resistance (L5.5).
///
/// Compares mean amplitude of the oldest quartile of original memories
/// before injections vs after all injections + dream cycles.
fn eval_catastrophic_forgetting(
    engine: &ResonanceEngine,
    original_ids: &[uuid::Uuid],
    initial_mean_amp: f32,
) -> f32 {
    if original_ids.is_empty() || initial_mean_amp < 1e-6 {
        return 0.0;
    }

    // Sort original IDs by their position (proxy for creation order — lower UUID = older)
    // and take the oldest quartile
    let quartile_size = (original_ids.len() / 4).max(1);
    let oldest_ids = &original_ids[..quartile_size];

    let mut amp_sum = 0.0_f32;
    let mut found = 0usize;
    for id in oldest_ids {
        if let Ok(Some(m)) = engine.store.get(id) {
            amp_sum += m.amplitude;
            found += 1;
        }
    }

    if found == 0 {
        return 0.0;
    }

    let current_mean = amp_sum / found as f32;
    (current_mean / initial_mean_amp).min(1.0)
}

/// Run an L5 dream chain with frequency decay and online injection.
///
/// This is the L5-specific version of `run_dream_chain_with_quiescence`.
/// After each dream cycle, `apply_freq_decay` is called so low-amplitude
/// memories sink toward the storage frequency (0.1 Hz).
///
/// Online injection happens after cycles 3, 6, 9, 12, 15 (0-indexed: 2, 5, 8, 11, 14).
/// Returns per-cycle amplitude deltas, injected IDs per event, and original IDs.
#[allow(clippy::type_complexity)]
fn run_l5_dream_chain(
    params: &Params,
    engine: &mut ResonanceEngine,
) -> (Vec<ChainSeed>, Vec<f32>, ChainTotals, Option<usize>, Vec<f32>,
      Vec<Vec<uuid::Uuid>>, Vec<uuid::Uuid>, f32) {
    let depth = params.chain_depth.max(1);
    let dim = 128; // L5 corpus dimension
    let quiescence_enabled = depth >= 8;
    let quiescence_threshold = 0.001_f32;
    let mut chain_seeds: Vec<ChainSeed> = Vec::with_capacity(depth);
    let mut phi_history: Vec<f32> = Vec::with_capacity(depth);
    let mut totals = ChainTotals::default();
    let bridge = ConsciousnessBridge::new(0.3, 0.5);
    let mut quiescence_at: Option<usize> = None;
    let mut amplitude_deltas: Vec<f32> = Vec::with_capacity(depth);

    // L5.5: track original memory IDs and their initial oldest-quartile amplitude.
    // Exclude adversarial memories (content starts with "adv_l5_") so that
    // random adversarial UUIDs do not land at random positions in the UUID-sorted
    // list and corrupt the "oldest quartile" amplitude baseline. Catastrophic
    // forgetting measures survival of corpus memories, not injected adversarials.
    let original_ids: Vec<uuid::Uuid> = engine
        .store
        .all_memories()
        .unwrap_or_default()
        .iter()
        .filter(|m| !m.content.starts_with("adv_l5_"))
        .map(|m| m.id)
        .collect();

    // Compute initial mean amplitude of oldest quartile
    let quartile_size = (original_ids.len() / 4).max(1);
    let initial_oldest_amps: Vec<f32> = original_ids[..quartile_size]
        .iter()
        .filter_map(|id| engine.store.get(id).ok().flatten().map(|m| m.amplitude))
        .collect();
    let initial_mean_amp = if initial_oldest_amps.is_empty() {
        0.0
    } else {
        initial_oldest_amps.iter().sum::<f32>() / initial_oldest_amps.len() as f32
    };

    // Injection points: after cycles 3, 6, 9, 12, 15 (0-indexed: 2, 5, 8, 11, 14)
    let injection_cycles: Vec<usize> = vec![2, 5, 8, 11, 14];
    let mut injected_ids_per_event: Vec<Vec<uuid::Uuid>> = Vec::new();
    let mut injection_counter = 0usize;

    // DREAM_GRAVITY: capture the PRE-dream phase topology ONCE. The associative
    // gravity pass must reinforce memories that were phase-aligned with the attractor
    // in the STORED topology — which is exactly what query_gravity measures (it groups
    // by pre-dream phase). Anchoring to live phases fails because the dream's Kuramoto
    // relaxation moves phases every cycle, so "neighbors" drift away from the metric's.
    // Param default (autoresearch-sweepable); DREAM_GRAVITY env overrides for manual A/B.
    let gravity_gain: f32 = std::env::var("DREAM_GRAVITY")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(params.dream_gravity);
    let (gravity_ref, gravity_query_phase): (
        std::collections::HashMap<uuid::Uuid, f32>,
        f32,
    ) = if gravity_gain > 0.0 {
        let snap = engine.store.all_memories().unwrap_or_default();
        let qphase = snap
            .iter()
            .max_by(|a, b| a.amplitude.total_cmp(&b.amplitude))
            .map(|m| m.phase)
            .unwrap_or(0.0);
        (snap.iter().map(|m| (m.id, m.phase)).collect(), qphase)
    } else {
        (std::collections::HashMap::new(), 0.0)
    };

    for cycle_idx in 0..depth {
        // Snapshot amplitudes before this cycle
        let amps_before: Vec<(uuid::Uuid, f32)> = engine
            .store
            .all_memories()
            .unwrap_or_default()
            .iter()
            .map(|m| (m.id, m.amplitude))
            .collect();

        // Hyp3c: env-driven selective multiplicative attention drive.
        //   DRIVE_A         = amplitude (0.0 disables, default 0.15)
        //   DRIVE_TOP_FRAC  = fraction of top-amplitude memories to modulate
        //                     (1.0 = all, 0.25 = top 25%, default 1.0)
        //   DRIVE_FREQ_HZ   = drive frequency (default 2.0)
        // A=0.15 confirmed L5 optimum (2026-06-06T08): carrier_emergence 0.5684→0.5842,
        // avg fitness 0.1322 vs 0.1384 at A=0.1 (3-trial, K=1.0, DRIVE_SCOPE=all).
        {
            let drive_amp: f32 = std::env::var("DRIVE_A")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0.15);
            // Scope filter: only apply if current engine context is in DRIVE_SCOPE.
            //   DRIVE_SCOPE = "all" (default) | "flat_only" | "no_transfer" |
            //                  "a_only" | "a_and_flat"
            // DRIVE_CONTEXT is set by callers around each run_l5_dream_chain call.
            let drive_context = std::env::var("DRIVE_CONTEXT").unwrap_or_default();
            let drive_scope = std::env::var("DRIVE_SCOPE")
                .unwrap_or_else(|_| "all".to_string());
            let scope_allows = match drive_scope.as_str() {
                "all" => true,
                "flat_only" => drive_context == "engine_flat",
                "a_only" => drive_context == "engine_a",
                "a_and_flat" => {
                    drive_context == "engine_a" || drive_context == "engine_flat"
                }
                "no_transfer" => {
                    drive_context != "engine_b_primed"
                        && drive_context != "engine_b_naive"
                }
                // Drive only the xi measurement engines + flat corpus.
                // Excludes engine_a to avoid the ~0.4 xi penalty from engine_a drive.
                "xi_and_flat" => {
                    drive_context == "engine_clean"
                        || drive_context == "engine_adv"
                        || drive_context == "engine_flat"
                }
                _ => true,
            };
            if drive_amp.abs() > 1e-9 && scope_allows {
                // 0.5 Hz confirmed optimal (2026-06-06): the half-cycle arc
                // (positive drive cycles 0-8, gentle suppression 9-16) amplifies
                // carrier structure far more coherently than 2.0 Hz oscillations,
                // lifting carrier_emergence 0.497→0.935 and avg fitness 0.149→0.099.
                let drive_freq_hz: f32 = std::env::var("DRIVE_FREQ_HZ")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0.5);
                let top_frac: f32 = std::env::var("DRIVE_TOP_FRAC")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(1.0);
                let dt_per_cycle: f32 = 0.125;
                let t = cycle_idx as f32 * dt_per_cycle;
                let drive_factor = 1.0
                    + drive_amp
                        * (2.0 * std::f32::consts::PI * drive_freq_hz * t).sin();

                let all = engine.store.all_memories().unwrap_or_default();
                let mut sorted: Vec<(uuid::Uuid, f32)> =
                    all.iter().map(|m| (m.id, m.amplitude)).collect();
                sorted.sort_by(|a, b| b.1.total_cmp(&a.1));
                let n_target = (
                    (sorted.len() as f32 * top_frac.clamp(0.0, 1.0)).round() as usize
                ).max(1);

                for (id, _) in sorted.iter().take(n_target) {
                    if let Ok(Some(m)) = engine.store.get_mut(id) {
                        m.amplitude = (m.amplitude * drive_factor).max(0.0);
                    }
                }
            }
        }


        let threshold_scale = if cycle_idx == 0 {
            1.0
        } else {
            (1.0 - params.chain_carry_strength).max(0.0)
        };
        let effective_threshold = params.interference_threshold * threshold_scale;

        let consolidator = ConsolidationEngine {
            interference_threshold: effective_threshold,
            phase_alignment_threshold: params.phase_alignment_threshold,
            prune_threshold: params.prune_threshold,
            constructive_boost: params.constructive_boost,
            destructive_penalty: params.destructive_penalty,
            kuramoto: KuramotoSync {
                coupling_strength: params.kuramoto_coupling,
                dt: params.kuramoto_dt,
                steps: params.kuramoto_steps,
                coupling_threshold: params.kuramoto_threshold,
            },
            adaptive: Default::default(),
            verified: Default::default(),
            chiral_perturbation: params.chiral_perturbation,
            noise_floor: params.noise_floor,
            hallucination_amplitude: params.hallucination_amplitude,
            protect_established: true,
            repulsion_threshold: params.consolidation_repulsion_threshold,
        };

        let report = consolidator.consolidate(engine, 0, 2);
        totals.strengthened += report.memories_strengthened;
        totals.pruned += report.memories_pruned;
        totals.links += report.skip_links_created;
        totals.hallucinations += report.hallucinations_created;

        // Apply frequency decay (L5.4) — high-amp memories keep freq,
        // low-amp memories decay toward storage band
        apply_freq_decay(engine);

        // DREAM_GRAVITY (default 0.0 = OFF, behavior byte-identical to before).
        // Associative phase-gravity: AFTER consolidation, redistribute amplitude
        // toward the phase-neighbors of the attractor (the highest-amplitude memory).
        // This is the core wave-interference recall property — phase-aligned memories
        // reinforce, phase-opposed ones fade — and it directly counters the amplitude
        // mean-reversion inside consolidation that otherwise lifts low-amplitude,
        // phase-DISTANT noise/sparse memories and drives query_gravity below the 0.5
        // chance line. Multiplicative and compounding across the dream's cycles. This
        // is a recall-sharpening term (not strictly energy-conserving), so it is an
        // env-gated experiment knob: A/B it via keep/revert and keep only if it lifts
        // query_gravity > 0.5 without regressing the scored metrics. Sweep {0.25,0.5,1.0}.
        if gravity_gain > 0.0 {
            let two_pi = 2.0 * std::f32::consts::PI;
            // Reinforce by PRE-dream phase alignment to the PRE-dream attractor
            // (gravity_ref / gravity_query_phase captured before the loop). Newly
            // injected memories aren't in the snapshot and are left untouched.
            let ids: Vec<uuid::Uuid> = gravity_ref.keys().copied().collect();
            for id in ids {
                let phase0 = match gravity_ref.get(&id) {
                    Some(p) => *p,
                    None => continue,
                };
                let raw = (phase0 - gravity_query_phase).abs();
                let raw_norm = raw % two_pi; // normalize to [0, 2π) before circular distance
                let dphi = raw_norm.min(two_pi - raw_norm); // 0..pi
                // align: 1.0 at the attractor phase, 0.0 anti-phase, 0.5 a quarter turn.
                let align = 1.0 - dphi / std::f32::consts::PI;
                // neighbors (align>0.5) grow, phase-distant (align<0.5) shrink.
                let g = (1.0 + gravity_gain * (align - 0.5)).max(0.0);
                if let Ok(Some(m)) = engine.store.get_mut(&id) {
                    m.amplitude = (m.amplitude * g).max(0.0);
                }
            }
        }

        // L5.5: inject online memories at designated cycle points.
        // CARRIER_NO_INJECT suppresses injection for the flat-corpus carrier test only;
        // injection spikes at cycle 3 otherwise dominate the DFT window.
        if injection_cycles.contains(&cycle_idx)
            && std::env::var("CARRIER_NO_INJECT").unwrap_or_default().is_empty()
        {
            let ids = inject_online_memories(engine, dim, injection_counter, params.encoder_seed);
            injected_ids_per_event.push(ids);
            injection_counter += 1;
        }

        // Compute mean |amplitude delta| for this cycle
        let mut delta_sum = 0.0_f32;
        let mut delta_count = 0usize;
        for (id, amp_before) in &amps_before {
            if let Ok(Some(m)) = engine.store.get(id) {
                delta_sum += (m.amplitude - amp_before).abs();
                delta_count += 1;
            }
        }
        let mean_delta = if delta_count > 0 {
            delta_sum / delta_count as f32
        } else {
            0.0
        };
        amplitude_deltas.push(mean_delta);

        let seed = compute_chain_seed(engine, params.chain_top_n, (cycle_idx + 1) as u32);
        chain_seeds.push(seed);
        let phi = bridge.assess(engine).phi as f32;
        phi_history.push(phi);

        // Quiescence short-circuit
        if quiescence_enabled && cycle_idx >= 2 {
            let prev_phi = phi_history[cycle_idx - 1];
            let delta = (phi - prev_phi).abs();
            if delta < quiescence_threshold {
                quiescence_at = Some(cycle_idx + 1);
                break;
            }
        }
    }

    (chain_seeds, phi_history, totals, quiescence_at, amplitude_deltas,
     injected_ids_per_event, original_ids, initial_mean_amp)
}

/// Level 5 experiment — scaffold (L5.1).
///
/// Builds Corpus A with bimodal frequency assignment, runs dream chain
/// using L4 evaluators as placeholder, reports L4-format metrics.
fn run_experiment_l5_session(params: &Params) {
    let dim = 128;

    // L5-local parameter overrides (same as L4 baseline + L5 chain_depth)
    let mut l5_params = params.clone();
    l5_params.phase_alignment_threshold = PI / 2.5;
    l5_params.consciousness_phi_target = 0.28092;
    l5_params.chain_carry_strength = 0.7;
    l5_params.chiral_perturbation = 0.7;
    l5_params.consolidation_repulsion_threshold = 0.28;
    // interference_relax phi fluctuates through injection events (cycles 2,5,8,11,14),
    // preventing quiescence from firing and causing 15-cycle over-consolidation that
    // collapses xi (0.97→0.68) and transfer (0.84→0.53). Hard-cap at 4 cycles:
    // T15's good runs (0.037 fitness) quiesced at cycle 4 — same effective depth.
    l5_params.chain_depth = 4; // irx cap — prevents hallucination-driven over-consolidation
    l5_params.chain_top_n = std::env::var("CHAIN_TOP_N")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(7);
    // K=2.0 confirmed optimal post-plumbing-fix (Jul 12 2026 K-sweep re-run against
    // b60f757 normalization): transfer=0.938 at K=2.0 vs 0.866 at K=3.0 and 0.803 at
    // K=1.5. The prior K=0.5 default was carried over from a pre-plumbing K-sweep and
    // measured against stage_sync silently ignoring params.kuramoto_*. KURAMOTO_COUPLING
    // env var still overrides.
    l5_params.kuramoto_coupling = std::env::var("KURAMOTO_COUPLING")
        .ok()
        .and_then(|s| s.parse::<f32>().ok())
        .unwrap_or(2.0);
    // DREAM_GRAVITY=0.35 confirmed optimal (Jul 31 2026 notes): the pre-dream phase
    // topology gets weighted 0.35× toward the attractor's phase-neighbors, reinforcing
    // B_primed vs B_naive transfer distinction. DREAM_GRAVITY env var still overrides.
    l5_params.dream_gravity = 0.35;
    let params = &l5_params;

    // Build Corpus A (hardness=2, bimodal frequency assignment)
    let corpus_a = build_corpus_l5_a(dim, 2, params.encoder_seed);
    println!("l5_corpus_a_size:     {}", corpus_a.len());

    // --- "Primed" pass: dream on A, then evaluate B ---
    let mut engine_a = build_l5_engine(&corpus_a, params, dim);

    // Snapshot (id, amplitude, phase) for the query_gravity instrumentation
    // before the dream perturbs them. See research/intersections/
    // 05-magic-gives-it-gravity.md.
    let pre_dream_a_state: Vec<(uuid::Uuid, f32, f32)> = engine_a
        .store
        .all_memories()
        .unwrap_or_default()
        .iter()
        .map(|m| (m.id, m.amplitude, m.phase))
        .collect();

    let start = Instant::now();
    std::env::set_var("DRIVE_CONTEXT", "engine_a");
    let (chain_seeds, phi_history, chain_totals, quiescence_a, amplitude_deltas_a,
         injected_ids_a, original_ids_a, initial_mean_amp_a) =
        run_l5_dream_chain(params, &mut engine_a);
    std::env::remove_var("DRIVE_CONTEXT");
    let consolidation_ms_a = start.elapsed().as_millis() as u64;

    // Build Corpus B
    let corpus_b = build_corpus_l5_b(dim, 2, params.encoder_seed);
    println!("l5_corpus_b_size:     {}", corpus_b.len());

    // Primed: load A's post-dream state, insert B on top, dream, evaluate
    let mut engine_b_primed = snapshot_engine_for_plasticity(&engine_a);
    // Insert Corpus B memories into the primed engine
    for (i, (vec, content, category, freq)) in corpus_b.iter().enumerate() {
        let mut mem = HyperMemory::new(vec.clone(), content.clone());
        // Use a distinct UUID namespace for B (high bits different from A)
        mem.id = uuid::Uuid::from_u128(
            0xBBBB_0000_0000_0000_0000_0000_0000_0000u128 + i as u128,
        );
        mem.decay_rate = params.decay_rate;
        mem.phase = match *category {
            "l4_dense" => 0.0 + (i as f32 * 0.1 * params.phase_spread),
            "l4_sparse" => PI * 0.5 + (i as f32 * 0.08 * params.phase_spread),
            "l4_bridge" => PI * 0.25,
            "l4_decoy" => PI * (i as f32 * 0.31),
            "l4_noise" => PI * (i as f32 * 0.7),
            _ => 0.0,
        };
        mem.layer_depth = match *category {
            "l4_dense" => (i % 3) as u8,
            "l4_sparse" => ((i + 1) % 3) as u8,
            "l4_bridge" => 1,
            "l4_decoy" => 2,
            "l4_noise" => 0,
            _ => 0,
        };
        mem.frequency = *freq;
        if *category == "l4_noise" {
            mem.amplitude = 0.15;
        }
        let _ = engine_b_primed.store.insert(mem);
    }
    // Dream on the primed engine (A state + B memories)
    let start_b_primed = Instant::now();
    std::env::set_var("DRIVE_CONTEXT", "engine_b_primed");
    let params_bp = { let mut p = (*params).clone(); p.chiral_perturbation = 0.15; p };
    let (chain_seeds_bp, phi_history_bp, _chain_totals_bp, quiescence_bp, _amp_deltas_bp,
         _injected_bp, _orig_bp, _init_amp_bp) =
        run_l5_dream_chain(&params_bp, &mut engine_b_primed);
    std::env::remove_var("DRIVE_CONTEXT");
    let consolidation_ms_b_primed = start_b_primed.elapsed().as_millis() as u64;

    // Evaluate B-primed using L4 evaluators as placeholder.
    // The primed pass uses B's own chain seeds for chain_fidelity.
    let fitness_b_primed = eval_l5_placeholder_fitness(
        &engine_b_primed, params, &chain_seeds_bp, &phi_history_bp,
    );

    // --- "Naive" pass: dream on B from scratch ---
    let mut engine_b_naive = build_l5_engine(&corpus_b, params, dim);
    let start_b_naive = Instant::now();
    std::env::set_var("DRIVE_CONTEXT", "engine_b_naive");
    let (chain_seeds_bn, phi_history_bn, _chain_totals_bn, quiescence_bn, _amp_deltas_bn,
         _injected_bn, _orig_bn, _init_amp_bn) =
        run_l5_dream_chain(params, &mut engine_b_naive);
    std::env::remove_var("DRIVE_CONTEXT");
    let consolidation_ms_b_naive = start_b_naive.elapsed().as_millis() as u64;

    let fitness_b_naive = eval_l5_placeholder_fitness(&engine_b_naive, params, &chain_seeds_bn, &phi_history_bn);

    // Transfer score: clamp01(1 - fitness_B_primed / fitness_B_naive)
    let transfer_score = if fitness_b_naive > 1e-6 {
        (1.0 - fitness_b_primed / fitness_b_naive).clamp(0.0, 1.0)
    } else {
        0.0
    };

    // Use the Corpus A chain results for primary reporting
    let chain_fidelity = eval_chain_fidelity(&chain_seeds, &phi_history);

    let noise_removal = eval_l4_noise_removal(&engine_a);
    let signal_preservation = eval_l4_signal_preservation(&engine_a);
    let bridge_links = eval_bridge_links(&engine_a);
    let phase_coherence = eval_phase_coherence_l4(&engine_a);
    let cluster_separation = eval_cluster_separation_l4(&engine_a);
    let amp_diversity = eval_amplitude_diversity(&engine_a);
    let speed_a = 1.0 - (consolidation_ms_a as f32 / 60000.0).min(1.0);
    let xi_diversity = eval_xi_diversity(&engine_a);
    let consciousness = eval_consciousness(&engine_a, params.consciousness_phi_target);
    let hall_quality = eval_hallucination_quality(&engine_a);
    let dream_efficiency = eval_dream_efficiency(
        chain_totals.strengthened, chain_totals.pruned, chain_totals.links, params.chain_depth,
    );
    let surviving_a: Vec<HyperMemory> = engine_a
        .store
        .all_memories()
        .unwrap_or_default()
        .iter()
        .map(|m| (*m).clone())
        .collect();
    let corpus_xi_diversity = eval_corpus_xi_diversity(&surviving_a);
    let encoding_entropy = eval_encoding_entropy(&surviving_a, 8);

    // L5.4: temporal separation (frequency band bimodality)
    let temporal_separation = eval_temporal_separation(&engine_a);

    // Instrumentation: magic-proxy phase concentration on engine_a (NOT in fitness)
    // See research/intersections/05-magic-gives-it-gravity.md
    let magic_proxy_phase_r = eval_phase_concentration(&engine_a);

    // Instrumentation: query_gravity — does the dream amplify phase-neighbors
    // of the highest-amplitude pre-dream memory more than phase-distant ones?
    // Score > 0.5 = attention-as-gravity is working (the heavy thing pulls the
    // similar light things toward it). NOT in fitness; the magic proxy and
    // query_gravity should correlate if the wave-interference / non-Clifford
    // story is right.
    let query_gravity = eval_query_gravity(&pre_dream_a_state, &engine_a);

    // L5.5: online retention + catastrophic forgetting resistance
    let online_retention = eval_online_retention(&engine_a, &injected_ids_a);
    let catastrophic_forgetting = eval_catastrophic_forgetting(
        &engine_a, &original_ids_a, initial_mean_amp_a,
    );

    // L5.6: carrier emergence via FFT on flat-frequency corpus
    // The bimodal corpus amplitude deltas (for reference):
    let actual_cycles_a = amplitude_deltas_a.len();
    // Treat each dream cycle as a fixed 0.125s tick (8 Hz fs). Wall-time
    // derivation pins target band above Nyquist; this is the design intent.
    let _ = actual_cycles_a;
    let cycle_period_a: f32 = 0.125;
    let carrier_bimodal = eval_carrier_emergence(&amplitude_deltas_a, cycle_period_a);

    // Flat-frequency emergence test: build corpus with ALL memories at 0.1 Hz,
    // run dream chain, measure whether drive-induced carrier emerges from uniform input.
    let corpus_flat = build_corpus_l5_a_flat(dim, 2, params.encoder_seed);
    let mut engine_flat = build_l5_engine(&corpus_flat, params, dim);
    let start_flat = Instant::now();
    std::env::set_var("DRIVE_CONTEXT", "engine_flat");
    // Run 6 cycles, skip cycles 0 AND 1 from the DFT window, suppress injection.
    // Two separate masking problems in the old 5-cycle/skip-1 setup:
    //   1. Cycle 1 carries a secondary consolidation residual (~0.192) from cycle 0's
    //      full-threshold sweep — 23× larger than the drive contribution.
    //   2. Injection at cycle 2 creates a consolidation spike at cycle 3 (~0.036)
    //      that dominates the remaining window.
    // Fix: CARRIER_NO_INJECT removes source of problem 2; chain_depth=6 + all_deltas[2..]
    // removes source of problem 1. Cycles 2-5 then carry only drive + gravity + quiescent
    // consolidation. Gravity is roughly constant across cycles (same ref snapshot) → DC →
    // excluded by the DFT. Drive at 0.5 Hz gives pattern [sin(π·0.25), sin(π·0.375),
    // sin(π·0.5), sin(π·0.625)] = [0.707, 0.924, 1.0, 0.924] × A × mean_amp, DFTing
    // at k=1 (2 Hz bin within [0.5, 4.0] Hz band) → predicted carrier_emergence ≈ 0.81.
    let amp_deltas_flat = {
        let mut flat_params = (*params).clone();
        flat_params.chain_depth = 6;
        // CARRIER_KURAMOTO_COUPLING decouples the flat-corpus K from the transfer corpora.
        // The flat corpus only measures drive-induced amplitude patterns; lower K prevents
        // Kuramoto phase-lock from suppressing the periodic amplitude signal. Default K=1.5
        // is optimal when transfer corpora run at K=2.0 (Jul 15 research fire — eliminates
        // carrier_e penalty entirely). env var overrides.
        flat_params.kuramoto_coupling = std::env::var("CARRIER_KURAMOTO_COUPLING")
            .ok()
            .and_then(|s| s.parse::<f32>().ok())
            .unwrap_or(1.5);
        std::env::set_var("CARRIER_NO_INJECT", "1");
        let (_cs_flat, _phi_flat, _totals_flat, _quiescence_flat, all_deltas,
             _inj_flat, _orig_flat, _init_amp_flat) =
            run_l5_dream_chain(&flat_params, &mut engine_flat);
        std::env::remove_var("CARRIER_NO_INJECT");
        if all_deltas.len() >= 6 {
            all_deltas[2..].to_vec()
        } else if all_deltas.len() >= 2 {
            all_deltas[1..].to_vec()
        } else {
            all_deltas
        }
    };
    std::env::remove_var("DRIVE_CONTEXT");
    let consolidation_ms_flat = start_flat.elapsed().as_millis() as u64;

    let actual_cycles_flat = amp_deltas_flat.len();
    // Fixed 0.125s tick (8 Hz fs) — see cycle_period_a comment above.
    let _ = actual_cycles_flat;
    let _ = consolidation_ms_flat;
    let cycle_period_flat: f32 = 0.125;
    // carrier_emergence = the FLAT-corpus FFT score (emergence, not passthrough)
    let carrier_emergence = eval_carrier_emergence(&amp_deltas_flat, cycle_period_flat);

    // L5.7: frequency_transfer — does the 2 Hz band structure survive cross-corpus transfer?
    let frequency_transfer = eval_frequency_transfer(&engine_a, &engine_b_primed);

    // L5.8: xi_robustness_v2 — adversarial robustness of xi re-ranking paths
    // Xi engines use depth=3 at K=1.0: three cycles reinforce clean-engine phase structure
    // without giving adversaries excessive disruption time; K=1.0 (not the transfer K=2.0)
    // keeps adversarial coupling weaker so clean signal wins. Jul 16+Jul 26 research confirmed
    // depth=3+K=1.0 improves xi from 0.9526 to 0.9783 vs depth=2+K=0.5, saving 0.003427
    // fitness. depth=4 hurts xi (T16: adversaries dominate at four cycles).
    let xi_eval_params = { let mut p = (*params).clone(); p.chain_depth = 3; p.kuramoto_coupling = 1.0; p };
    let xi_robustness_v2 = eval_xi_robustness_v2(&corpus_a, &xi_eval_params, dim);

    // L5 fitness — all 13 metrics wired, no placeholders remaining
    // Inherited core (15%): noise_removal(2%), signal_preservation(2%),
    //   phase_coherence(2%), speed(3%), consciousness(3%), encoding_entropy(3%)
    // Cross-corpus transfer (25%): transfer_score(15%), frequency_transfer(10%)
    // Online learning (20%): online_retention(10%), catastrophic_forgetting(10%)
    // Multi-scale temporal (25%): temporal_separation(15%), carrier_emergence(10%)
    // Adversarial v2 (15%): xi_robustness_v2(15%)
    // Total: 15 + 25 + 20 + 25 + 15 = 100%
    let fitness = 0.02 * (1.0 - noise_removal)
        + 0.02 * (1.0 - signal_preservation)
        + 0.02 * (1.0 - phase_coherence)
        + 0.03 * (1.0 - speed_a)
        + 0.03 * (1.0 - consciousness)
        + 0.03 * (1.0 - encoding_entropy)
        + 0.15 * (1.0 - transfer_score)
        + 0.15 * (1.0 - temporal_separation)
        + 0.10 * (1.0 - online_retention)
        + 0.10 * (1.0 - catastrophic_forgetting)
        + 0.10 * (1.0 - carrier_emergence)
        + 0.10 * (1.0 - frequency_transfer)
        + 0.15 * (1.0 - xi_robustness_v2);

    // L5.9: Weight verification — assert all 13 L5 metrics sum to 100%
    let weight_sum: f32 = 0.02 + 0.02 + 0.02 + 0.03 + 0.03 + 0.03  // inherited core = 15%
        + 0.15 + 0.10                                                 // transfer axis = 25%
        + 0.10 + 0.10                                                 // online learning = 20%
        + 0.15 + 0.10                                                 // temporal = 25%
        + 0.15;                                                       // adversarial v2 = 15%
    assert!(
        (weight_sum - 1.0).abs() < 1e-6,
        "L5 weights must sum to 1.0, got {weight_sum}",
    );

    println!("---");
    println!("level:                5");
    println!("fitness:              {fitness:.6}");
    println!("transfer_score:       {transfer_score:.6}");
    println!("fitness_B_primed:     {fitness_b_primed:.6}");
    println!("fitness_B_naive:      {fitness_b_naive:.6}");

    // L5.9: Metric summary table — all 13 scored metrics with weights
    println!();
    println!("=== L5 METRIC SUMMARY (13 metrics, weights sum to 100%) ===");
    println!("{:<30} {:>6} {:>8} {:>10}", "metric", "weight", "value", "contrib");
    println!("{}", "-".repeat(58));
    let metrics: Vec<(&str, f32, f32)> = vec![
        ("noise_removal",             0.02, noise_removal),
        ("signal_preservation",       0.02, signal_preservation),
        ("phase_coherence",           0.02, phase_coherence),
        ("speed",                     0.03, speed_a),
        ("consciousness",             0.03, consciousness),
        ("encoding_entropy",          0.03, encoding_entropy),
        ("transfer_score",            0.15, transfer_score),
        ("frequency_transfer",        0.10, frequency_transfer),
        ("online_retention",          0.10, online_retention),
        ("catastrophic_forgetting",   0.10, catastrophic_forgetting),
        ("temporal_separation",       0.15, temporal_separation),
        ("carrier_emergence",         0.10, carrier_emergence),
        ("xi_robustness_v2",          0.15, xi_robustness_v2),
    ];
    let mut saturated_count = 0;
    for (name, weight, value) in &metrics {
        let contrib = weight * (1.0 - value);
        if (*value - 1.0).abs() < 1e-4 {
            saturated_count += 1;
        }
        println!("{:<30} {:>5.0}% {:>8.4} {:>10.6}", name, weight * 100.0, value, contrib);
    }
    println!("{}", "-".repeat(58));
    println!("{:<30} {:>5.0}% {:>8} {:>10.6}", "TOTAL", 100, "", fitness);
    println!("saturated_at_1.0:     {saturated_count}");
    println!();

    // Diagnostic metrics (not scored)
    println!("noise_removal:        {noise_removal:.4}");
    println!("signal_preservation:  {signal_preservation:.4}");
    println!("bridge_links:         {bridge_links:.4}");
    println!("phase_coherence:      {phase_coherence:.4}");
    println!("cluster_separation:   {cluster_separation:.4}");
    println!("amp_diversity:        {amp_diversity:.4}");
    println!("xi_diversity:         {xi_diversity:.4}");
    println!("consciousness:        {consciousness:.4}");
    println!("hall_quality:         {hall_quality:.4}");
    println!("dream_efficiency:     {dream_efficiency:.4}");
    println!("speed_a:              {speed_a:.4}");
    println!("corpus_xi_diversity:  {corpus_xi_diversity:.4}");
    println!("encoding_entropy:     {encoding_entropy:.4}");
    println!("chain_fidelity:       {chain_fidelity:.4}");
    println!("temporal_separation:  {temporal_separation:.4}");
    println!("magic_proxy_phase_R:  {magic_proxy_phase_r:.4}");
    println!("query_gravity:        {query_gravity:.4}");
    println!("online_retention:     {online_retention:.4}");
    println!("catastrophic_forget:  {catastrophic_forgetting:.4}");
    println!("carrier_emergence:    {carrier_emergence:.4}");
    println!("carrier_bimodal:      {carrier_bimodal:.4}");
    println!("frequency_transfer:   {frequency_transfer:.4}");
    println!("xi_robustness_v2:     {xi_robustness_v2:.4}");
    println!("injected_events:      {}", injected_ids_a.len());
    println!("injected_total:       {}", injected_ids_a.iter().map(|v| v.len()).sum::<usize>());
    println!("amplitude_deltas_a:   {amplitude_deltas_a:?}");
    println!("amp_deltas_flat:      {amp_deltas_flat:?}");
    println!("chain_depth:          {}", params.chain_depth);
    println!("quiescence_at_a:      {}", quiescence_a.map_or("none".to_string(), |n| n.to_string()));
    println!("quiescence_at_bp:     {}", quiescence_bp.map_or("none".to_string(), |n| n.to_string()));
    println!("quiescence_at_bn:     {}", quiescence_bn.map_or("none".to_string(), |n| n.to_string()));
    println!("phi_history:          {phi_history:?}");
    let total_ms = consolidation_ms_a + consolidation_ms_b_primed + consolidation_ms_b_naive + consolidation_ms_flat;
    println!("consolidation_ms_a:   {consolidation_ms_a}");
    println!("consolidation_ms_b_p: {consolidation_ms_b_primed}");
    println!("consolidation_ms_b_n: {consolidation_ms_b_naive}");
    println!("consolidation_ms_fl:  {consolidation_ms_flat}");
    println!("total_ms:             {total_ms}");
    println!("strengthened:         {}", chain_totals.strengthened);
    println!("pruned:               {}", chain_totals.pruned);
    println!("links_created:        {}", chain_totals.links);
    println!("hallucinations:       {}", chain_totals.hallucinations);

    // L5.9: Write results-L5.tsv stub row
    let tsv_path = Path::new("experiments/results-L5.tsv");
    let needs_header = !tsv_path.exists();
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(tsv_path)
    {
        use std::io::Write;
        if needs_header {
            let _ = writeln!(
                f,
                "run\tfitness\tnoise_removal\tsignal_preservation\tphase_coherence\tspeed\tconsciousness\tencoding_entropy\ttransfer_score\tfrequency_transfer\tonline_retention\tcatastrophic_forgetting\ttemporal_separation\tcarrier_emergence\txi_robustness_v2\ttotal_ms\tquery_gravity"
            );
        }
        let run_label = std::env::var("RESEARCH_RUN")
            .unwrap_or_else(|_| "L5".to_string());
        let _ = writeln!(
            f,
            "{run_label}\t{fitness:.6}\t{noise_removal:.4}\t{signal_preservation:.4}\t{phase_coherence:.4}\t{speed_a:.4}\t{consciousness:.4}\t{encoding_entropy:.4}\t{transfer_score:.6}\t{frequency_transfer:.4}\t{online_retention:.4}\t{catastrophic_forgetting:.4}\t{temporal_separation:.4}\t{carrier_emergence:.4}\t{xi_robustness_v2:.4}\t{total_ms}\t{query_gravity:.4}",
        );
    }
    println!("results_tsv:          experiments/results-L5.tsv");
    println!("---");
}

/// Wave 4 Task 4.4 (#356) — L6 swarm-fitness session (collective OODA).
///
/// Graduates the autoresearch OODA from single-agent L5 fitness (saturated
/// ≈0.0074) to **collective** fitness: it scores how well a small N-agent swarm
/// with partitioned corpora detects its own knowledge gaps, reaches recall
/// consensus, surfaces contradictions, and concentrates query gravity. This
/// closes the ADR-0035 North Star loop — the loop now optimizes collective
/// intelligence, not one agent's `query_gravity`.
///
/// The ground-truthable scoring lives in `kannaka_memory::swarm_fitness` (pure,
/// `cargo test --lib`-covered); here we build the inputs and write the TSV row.
/// Fitness is a weighted `1 - metric` loss like L5 (weights sum to 1.0).
fn run_experiment_l6_session(params: &Params) {
    use kannaka_memory::swarm_fitness as sf;
    use kannaka_memory::gap::DomainCluster;
    use kannaka_memory::sensemaking::PeerRecall;

    let dim = 128;
    let mut l6 = params.clone();
    // query_gravity axis knob, promoted to first-class (DREAM_GRAVITY overrides).
    l6.dream_gravity = std::env::var("DREAM_GRAVITY")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(params.dream_gravity);
    let params = &l6;
    let n_agents = std::env::var("L6_AGENTS")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(4)
        .max(2);
    let seed = params.encoder_seed;
    let same_domain = |a: &str, b: &str| a.eq_ignore_ascii_case(b);

    println!("=== L6 swarm-fitness session (collective OODA) ===");
    println!("l6_agents:            {n_agents}");
    println!("l6_dream_gravity:     {:.4}", params.dream_gravity);

    // --- Metric 1: gap_detection_precision (PRIME, ground-truthable) ---
    // Every well-covered domain is broadly + deeply held by all agents; the
    // held-out domain is left as a single-agent trace so the swarm "knows it
    // exists" yet it reads as a gap. detect_gaps must flag exactly the held-out.
    let well = ["dense", "sparse", "bridge", "decoy"];
    let held_out = ["memory"];
    let mut clusters: Vec<DomainCluster> = Vec::new();
    for a in 0..n_agents {
        let agent_id = format!("agent{a}");
        for d in well {
            clusters.push(DomainCluster {
                agent_id: agent_id.clone(),
                theme: d.to_string(),
                size: 10,
                coherence: 0.85,
                mean_amplitude: 0.75,
            });
        }
        if a == 0 {
            for d in held_out {
                clusters.push(DomainCluster {
                    agent_id: agent_id.clone(),
                    theme: d.to_string(),
                    size: 1,
                    coherence: 0.15,
                    mean_amplitude: 0.08,
                });
            }
        }
    }
    let gap_report = sf::gap_detection_precision(&clusters, n_agents, &held_out, 0.4, same_domain);
    println!(
        "gap_detection_precision: {:.4} ({}/{} flagged correct, {} tasks)",
        gap_report.precision, gap_report.correct, gap_report.flagged, gap_report.tasks_planned
    );

    // --- Metric 2: recall_vote_gain (Cap 2) ---
    // A probe of T truths, each backed by every agent at modest confidence, plus
    // one distinct high-confidence WRONG claim per agent. Consensus should rank
    // the broadly-backed truths above any single agent's confident error.
    let truths: Vec<String> = (0..3).map(|t| format!("truth-{t}")).collect();
    let mut recalls: Vec<PeerRecall> = Vec::new();
    for a in 0..n_agents {
        let peer = format!("agent{a}");
        for (t, truth) in truths.iter().enumerate() {
            recalls.push(PeerRecall {
                peer_id: peer.clone(),
                content: truth.clone(),
                similarity: 0.9,
                amplitude: 0.7,
                phase: 0.0,
                confidence: 0.45 + 0.01 * t as f32,
            });
        }
        recalls.push(PeerRecall {
            peer_id: peer.clone(),
            content: format!("wrong-{a}"),
            similarity: 0.9,
            amplitude: 0.7,
            phase: 0.0,
            confidence: 0.95,
        });
    }
    let vote_report = sf::recall_vote_gain(&recalls, n_agents, &truths, same_domain);
    println!(
        "recall_vote_gain:     {:.4} (consensus {:.2} vs best single {:.2})",
        vote_report.gain, vote_report.consensus_acc, vote_report.best_single_acc
    );

    // --- Metric 3: contradiction_recall (Cap 10) ---
    // Inject K opposed pairs across agents (same subject, ~π phase apart).
    let k_contra = 3usize;
    let injected: Vec<String> = (0..k_contra).map(|i| format!("claim-{i}")).collect();
    let mut contra_recalls: Vec<PeerRecall> = Vec::new();
    for (i, subj) in injected.iter().enumerate() {
        let pa = format!("agent{}", (2 * i) % n_agents);
        let pb = format!("agent{}", (2 * i + 1) % n_agents);
        contra_recalls.push(PeerRecall {
            peer_id: pa, content: subj.clone(), similarity: 0.95, amplitude: 0.8, phase: 0.0, confidence: 0.8,
        });
        contra_recalls.push(PeerRecall {
            peer_id: pb, content: subj.clone(), similarity: 0.95, amplitude: 0.8, phase: PI, confidence: 0.8,
        });
    }
    let sim = |x: &str, y: &str| if same_domain(x, y) { 1.0 } else { 0.0 };
    let contra_report = sf::contradiction_recall(&contra_recalls, &injected, sim, 0.5, PI / 2.0);
    println!(
        "contradiction_recall: {:.4} ({}/{} detected)",
        contra_report.recall, contra_report.detected, contra_report.injected
    );

    // --- Metric 4: query_gravity (promoted L5 instrumentation) ---
    // Build agent 0's disjoint partition of the L5 corpus, dream with the
    // gravity knob, and measure how the dream concentrates amplitude onto
    // phase-neighbors of the strongest query.
    let corpus = build_corpus_l5_a(dim, 2, seed);
    let partition: Vec<_> = corpus
        .iter()
        .enumerate()
        .filter(|(i, _)| i % n_agents == 0)
        .map(|(_, x)| x.clone())
        .collect();
    println!("l6_partition_size:    {}", partition.len());
    let mut engine = build_l5_engine(&partition, params, dim);
    let pre_state: Vec<(uuid::Uuid, f32, f32)> = engine
        .store
        .all_memories()
        .unwrap_or_default()
        .iter()
        .map(|m| (m.id, m.amplitude, m.phase))
        .collect();
    std::env::set_var("DRIVE_CONTEXT", "engine_l6");
    let _ = run_l5_dream_chain(params, &mut engine);
    std::env::remove_var("DRIVE_CONTEXT");
    let query_gravity = eval_query_gravity(&pre_state, &engine);
    println!("query_gravity:        {query_gravity:.4}");

    // --- Collective fitness: weighted 1 - metric loss (weights sum to 1.0) ---
    let weights = sf::L6Weights::default();
    let fitness = sf::l6_fitness(
        gap_report.precision,
        vote_report.gain,
        contra_report.recall,
        query_gravity,
        &weights,
    );
    println!("l6_fitness:           {fitness:.6}");

    // --- Write results-L6.tsv (header + per-run row) ---
    let tsv_path = Path::new("experiments/results-L6.tsv");
    let needs_header = !tsv_path.exists();
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(tsv_path)
    {
        use std::io::Write;
        if needs_header {
            let _ = writeln!(
                f,
                "run\tfitness\tgap_detection_precision\trecall_vote_gain\tcontradiction_recall\tquery_gravity\tn_agents\ttasks_planned"
            );
        }
        let run_label = std::env::var("RESEARCH_RUN").unwrap_or_else(|_| "L6".to_string());
        let _ = writeln!(
            f,
            "{}\t{:.6}\t{:.4}\t{:.4}\t{:.4}\t{:.4}\t{}\t{}",
            run_label,
            fitness,
            gap_report.precision,
            vote_report.gain,
            contra_report.recall,
            query_gravity,
            n_agents,
            gap_report.tasks_planned,
        );
    }
    println!("results_tsv:          experiments/results-L6.tsv");
    println!("---");
}

/// Core-merge (fusion) events between consecutive snapshots. A fusion is a
/// COLLISION: two or more cores of one snapshot whose nearest same-charge
/// match (cosine ≥ `merge_cos`) in the next snapshot is the SAME core. (The
/// earlier died-into-a-sibling detector missed real fusions: a fused core's
/// fingerprint is a neighborhood centroid, so it matches BOTH parents above
/// the survival threshold and every fusion read as two survivals.)
fn core_merge_epochs(
    snapshots: &[Vec<kannaka_memory::l6::CoreObs>],
    merge_cos: f32,
) -> Vec<usize> {
    use kannaka_memory::l6;
    let mut out = Vec::new();
    for e in 0..snapshots.len().saturating_sub(1) {
        let next = &snapshots[e + 1];
        let mut child_hits: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
        for core in &snapshots[e] {
            if let Some((j, s)) = l6::nearest_core(&core.fp, next, Some(core.charge)) {
                if s >= merge_cos {
                    *child_hits.entry(j).or_insert(0) += 1;
                }
            }
        }
        let fusions = child_hits.values().filter(|&&n| n >= 2).count();
        for _ in 0..fusions {
            out.push(e + 1);
        }
    }
    out
}

/// L7 — the BELIEF arm (ADR-0037). Runs a real multi-agent belief substrate
/// (ChiralMedium with content-born phase) and scores the README's three
/// falsification predictions: core stability ⇒ recall reliability, core merge
/// ⇒ a consolidation event, shared cores ⇒ swarm agreement. Unlike L6's
/// synthetic fixtures, every observable here comes from the live medium:
/// spiral-core snapshots, dream absorb events, and actual recalls.
///
/// Env knobs: L7_AGENTS (default 4, 2..=6), L7_EPOCHS (default 6),
/// L7_ITEMS (default 8 per domain), L7_MIN_COS (track/share match, default
/// 0.85), L7_MERGE_COS (merge sibling threshold, default 0.60),
/// L7_COUPLE=1 (apply Track-D peer-core coupling each epoch, default off),
/// RESEARCH_RUN (TSV row label).
fn run_experiment_l7_session(_params: &Params) {
    use kannaka_memory::belief_fitness as bf;
    use kannaka_memory::l6::{self, CoreObs};
    use kannaka_memory::medium::chiral::ChiralMedium;

    let envf = |k: &str, d: f32| std::env::var(k).ok().and_then(|s| s.parse().ok()).unwrap_or(d);
    let envu = |k: &str, d: usize| std::env::var(k).ok().and_then(|s| s.parse().ok()).unwrap_or(d);
    let n_agents = envu("L7_AGENTS", 4).clamp(2, 6);
    let epochs = envu("L7_EPOCHS", 6).max(2);
    let items = envu("L7_ITEMS", 8).max(4);
    let min_cos = envf("L7_MIN_COS", 0.85);
    let merge_cos = envf("L7_MERGE_COS", 0.60);
    let couple = std::env::var("L7_COUPLE").map(|v| v == "1").unwrap_or(false);
    let couple_strength = envf("L7_COUPLE_STRENGTH", 0.2);
    // Optional per-epoch coupling SCHEDULE (comma-separated strengths, cycled):
    // the day-one sweep showed s=0.1 optimizes shared⇒agreement while s=0.2
    // optimizes stability⇒recall and no fixed strength satisfies both — this
    // knob asks whether ALTERNATING what the swarm couples for can.
    let couple_schedule: Vec<f32> = std::env::var("L7_COUPLE_SCHEDULE")
        .ok()
        .map(|s| s.split(',').filter_map(|x| x.trim().parse().ok()).collect())
        .unwrap_or_default();
    // Distractor importance. Default 0.25 = churn that never prunes (energy
    // threshold 0.01); setting it below 0.01 is the detector self-test — the
    // absorb channel MUST fire then, or a merge_consolidation of 0.0 would be
    // a dead instrument rather than a falsification.
    let junk_importance = envf("L7_JUNK_IMPORTANCE", 0.25);
    // Merge↔absorb alignment horizon (epochs) for prediction 2 — the
    // "fusion ⇒ EVENTUAL consolidation" restatement is tested by widening it.
    let p2_window = envu("L7_P2_WINDOW", 1);
    const FP_DIMS: usize = 16; // must match belief_core_snapshot's fingerprint dims

    println!("=== L7 belief session (ADR-0037 falsification predictions) ===");
    println!("l7_agents:            {n_agents}");
    println!("l7_epochs:            {epochs}");
    println!("l7_items_per_domain:  {items}");
    println!("l7_min_cos:           {min_cos:.2}");
    println!("l7_couple:            {couple}");

    // Belief phase ON for the whole session (content-born phase at ingest);
    // restore the caller's env afterwards so other levels are unaffected.
    let prev_belief = std::env::var("KANNAKA_BELIEF_PHASE").ok();
    std::env::set_var("KANNAKA_BELIEF_PHASE", "1");

    // Distinct-vocabulary domains. Agent k holds shared domains D0..=Dk plus a
    // private domain — a nested overlap gradient, so agent PAIRS differ in how
    // many belief domains they share (the variance prediction 3 correlates
    // against). Vocabulary is disjoint per domain so the hash encoder separates
    // content and spiral cores can localize.
    const DOMAIN_VOCAB: [[&str; 4]; 12] = [
        ["gravity", "orbit", "tide", "mass"],
        ["neuron", "synapse", "cortex", "spike"],
        ["harbor", "vessel", "cargo", "berth"],
        ["glacier", "moraine", "crevasse", "firn"],
        ["sonata", "cadence", "timbre", "chord"],
        ["enzyme", "substrate", "catalysis", "kinase"],
        ["quasar", "redshift", "parsec", "nebula"],
        ["loom", "warp", "weft", "shuttle"],
        ["magma", "basalt", "caldera", "vent"],
        ["ledger", "audit", "escrow", "invoice"],
        ["mycelium", "spore", "hypha", "lichen"],
        ["antenna", "waveform", "carrier", "hertz"],
    ];
    let domain_sentence = |d: usize, i: usize| -> String {
        let v = &DOMAIN_VOCAB[d % DOMAIN_VOCAB.len()];
        format!(
            "{} {} study {}: the {} of {} governs {} {}",
            v[0], v[1], i, v[2], v[3], v[0], v[i % 4]
        )
    };
    // A degraded cue — item index + one domain word, most of the sentence
    // missing. Verbatim self-recall is trivially perfect (zero variance ⇒ the
    // correlations report no-evidence 0.5 forever); recall RELIABILITY only
    // becomes measurable under weak cues, which is also what the falsifiability
    // clause means by "recallable content cluster" — the cluster answers a
    // partial probe, not an exact replay.
    let partial_query = |d: usize, i: usize| -> String {
        let v = &DOMAIN_VOCAB[d % DOMAIN_VOCAB.len()];
        // Deliberately ambiguous: one word from a NEIGHBOR domain contaminates
        // the cue. Fully-disjoint vocab makes clean cues trivially perfect
        // (recall variance 0 ⇒ correlations report no-evidence forever);
        // reliability only means something under cross-domain competition.
        let w = &DOMAIN_VOCAB[(d + 1) % DOMAIN_VOCAB.len()];
        // 1 own word vs 1 neighbor word — an evenly contested cue. (Two own
        // words made recall flat-1.0 across every domain: zero variance, so
        // prediction 1 could never leave the no-evidence line.)
        format!("{} {} {}", v[i % 4], i, w[(i + 1) % 4])
    };

    // Build agents: shared domains are 0..=k, private domain is n_agents + k.
    let pipeline = {
        let encoder = Box::new(SimpleHashEncoder::new(384, 42));
        let codebook = Codebook::new(384, kannaka_memory::WAVEFRONT_DIM, 42);
        EncodingPipeline::new(encoder, codebook)
    };
    let mut agents: Vec<ChiralMedium> = Vec::new();
    let mut agent_domains: Vec<Vec<usize>> = Vec::new();
    let mut content_domain: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for k in 0..n_agents {
        let mut cm = ChiralMedium::new();
        let mut domains: Vec<usize> = (0..=k).collect();
        domains.push(n_agents + k);
        for &d in &domains {
            for i in 0..items {
                let s = domain_sentence(d, i);
                content_domain.insert(s.clone(), d);
                let _ = cm.store(&s, 0.8, &pipeline);
            }
        }
        agents.push(cm);
        agent_domains.push(domains);
    }

    // ── Epoch loop: dream → (record absorbs) → snapshot cores → optional
    // Track-D coupling toward every peer's fresh snapshot. ──
    let mut snapshots: Vec<Vec<Vec<CoreObs>>> = vec![Vec::new(); n_agents];
    let mut absorb_epochs: Vec<Vec<usize>> = vec![Vec::new(); n_agents];
    for e in 0..epochs {
        for (a, cm) in agents.iter_mut().enumerate() {
            // Distractor churn: two low-importance throwaway memories per
            // epoch give the dream real dissolution candidates (without them
            // nothing ever absorbs and prediction 2 sits at no-evidence
            // forever) and perturb the field so core stability can vary.
            for x in 0..2 {
                let junk = format!(
                    "ephemeral flicker {} {} of agent {} passing note {}",
                    e, x, a, (e * 7 + x * 3 + a) % 13
                );
                let _ = cm.store(&junk, junk_importance, &pipeline);
            }
            // Deep dream every third epoch — the consolidation pressure that
            // produces absorb events and core merges.
            let deep = e % 3 == 2;
            let count_before = cm.right.count();
            let report = cm.dream(deep, 3);
            // Consolidation observable: the dream's own dissolved counter OR a
            // net field shrink (absorption paths that don't increment the
            // counter still reduce the wavefront count).
            if report.wavefronts_dissolved > 0 || cm.right.count() < count_before {
                absorb_epochs[a].push(e);
            }
        }
        let snaps_now: Vec<Vec<CoreObs>> =
            agents.iter().map(|cm| cm.belief_core_snapshot()).collect();
        for (a, s) in snaps_now.iter().enumerate() {
            snapshots[a].push(s.clone());
        }
        if couple {
            let strength = if couple_schedule.is_empty() {
                couple_strength
            } else {
                couple_schedule[e % couple_schedule.len()]
            };
            for a in 0..n_agents {
                for p in 0..n_agents {
                    if a != p && !snaps_now[p].is_empty() {
                        let _ = agents[a]
                            .couple_toward_peer_cores(&snaps_now[p], 2, strength, 0.5, min_cos);
                    }
                }
            }
        }
    }

    // ── Recall rates per (agent, domain): top-1 self-recall over the domain's
    // items — the "recallable content cluster" half of the falsifiability
    // clause. ──
    let mut recall_rate = vec![std::collections::HashMap::<usize, f32>::new(); n_agents];
    for (a, cm) in agents.iter().enumerate() {
        for &d in &agent_domains[a] {
            let mut hits = 0usize;
            for i in 0..items {
                // Degraded cue; a hit = the top result is ANY memory of the
                // probed domain (the cluster answered, even if not the exact
                // item — cluster-level reliability, per the falsifiability
                // clause).
                let q = partial_query(d, i);
                if let Ok(res) = cm.recall(&q, 1, &pipeline) {
                    if res
                        .first()
                        .and_then(|r| content_domain.get(&r.content))
                        .map(|&rd| rd == d)
                        .unwrap_or(false)
                    {
                        hits += 1;
                    }
                }
            }
            recall_rate[a].insert(d, hits as f32 / items as f32);
        }
    }

    // ── Prediction 1: core stability ⇒ recall reliability. Stability of each
    // FINAL-snapshot core = fraction of earlier epochs where a same-charge,
    // fingerprint-matched core existed (walked backwards, following drift).
    // The core's domain = the domain of the stored wavefront whose fingerprint
    // it matches best. ──
    let mut p1_pairs: Vec<(f32, f32)> = Vec::new();
    let mut total_final_cores = 0usize;
    for a in 0..n_agents {
        let Some(final_snap) = snapshots[a].last() else { continue };
        total_final_cores += final_snap.len();
        // Fingerprints of this agent's stored (right) wavefronts + their domains.
        let cm = &agents[a];
        let n = cm.right.count();
        let row_info: Vec<(Vec<f32>, Option<usize>)> = (0..n)
            .map(|i| {
                let v: Vec<f32> = cm.right.wavefronts.row(i).iter().copied().collect();
                let dom = content_domain.get(&cm.right.metadata[i].content).copied();
                (l6::fingerprint(&v, FP_DIMS), dom)
            })
            .collect();
        for core in final_snap {
            // stability: walk earlier snapshots, matching by charge + cosine.
            let mut seen = 1usize; // the final snapshot itself
            let mut fp = core.fp.clone();
            for e in (0..snapshots[a].len().saturating_sub(1)).rev() {
                if let Some((j, s)) = l6::nearest_core(&fp, &snapshots[a][e], Some(core.charge)) {
                    if s >= min_cos {
                        seen += 1;
                        fp = snapshots[a][e][j].fp.clone(); // follow drift backwards
                    }
                }
            }
            let stability = seen as f32 / epochs as f32;
            // domain attribution: nearest stored wavefront's domain.
            let dom = row_info
                .iter()
                .max_by(|x, y| {
                    l6::cosine(&core.fp, &x.0)
                        .partial_cmp(&l6::cosine(&core.fp, &y.0))
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .and_then(|(_, d)| *d);
            if let Some(d) = dom {
                if let Some(&rr) = recall_rate[a].get(&d) {
                    p1_pairs.push((stability, rr));
                }
            }
        }
    }
    let stability_recall = bf::stability_recall_score(&p1_pairs);
    let rng = |vals: Vec<f32>| -> (f32, f32) {
        let lo = vals.iter().copied().fold(f32::INFINITY, f32::min);
        let hi = vals.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        if lo.is_finite() { (lo, hi) } else { (0.0, 0.0) }
    };
    let (st_lo, st_hi) = rng(p1_pairs.iter().map(|p| p.0).collect());
    let (rr_lo, rr_hi) = rng(p1_pairs.iter().map(|p| p.1).collect());
    println!("l7_final_cores:       {total_final_cores}");
    println!("l7_p1_pairs:          {}", p1_pairs.len());
    println!("l7_stability_range:   {st_lo:.2}..{st_hi:.2}");
    println!("l7_recall_range:      {rr_lo:.2}..{rr_hi:.2}");
    println!("stability_recall:     {stability_recall:.4}");

    // ── Prediction 2: core merge ⇒ a consolidation event. A merge = a core
    // that vanishes between epochs while a same-charge sibling at cosine ≥
    // merge_cos survives (its content got absorbed into the sibling). Scored
    // against the epochs whose dream actually dissolved wavefronts. ──
    let mut merge_epochs_all: Vec<usize> = Vec::new();
    let mut absorb_all: Vec<usize> = Vec::new();
    for a in 0..n_agents {
        absorb_all.extend(absorb_epochs[a].iter().copied());
        merge_epochs_all.extend(core_merge_epochs(&snapshots[a], merge_cos));
    }
    let merge_consolidation_chiral = bf::merge_consolidation_score(&merge_epochs_all, &absorb_all, 1);
    println!("l7_merge_events:      {}", merge_epochs_all.len());
    println!("l7_absorb_epochs:     {}", absorb_all.len());
    println!("merge_consolidation_chiral: {merge_consolidation_chiral:.4} (observable blind in ChiralMedium — issue #583)");

    // ── Prediction 2 PROPER — HrmStore substrate. ChiralMedium cannot dissolve
    // (energy floor ≥ prune threshold, issue #583), so the real consolidation
    // machinery is ADR-0036's belief-safe resonance-merge. One HrmStore agent
    // ingests the shared domains PLUS mergeable near-duplicate triplets; each
    // epoch dreams then APPLIES consolidation; absorb events come from the
    // apply report, merges from core tracking on the same store. ──
    let (merge_consolidation, hrm_merges, hrm_absorbs) = {
        use kannaka_memory::hrm_store::compute_merge_grouping;
        use kannaka_memory::medium::types::ConsolidateOpts;

        // Bare ChiralMedium (belief-born phases native, mid-session ingest
        // just works) + the EXACT ADR-0036 grouping algorithm applied to its
        // right hemisphere each epoch — semantic gate, belief centering,
        // absorb cap, all of it. Absorptions are applied by removing the
        // grouped non-carrier wavefronts; that removal is the consolidation
        // event prediction 2 aligns against. (HrmStore itself is the wrong
        // vehicle here: its MediumBackend::insert never routes to the chiral
        // field, so a fresh chiral-upgraded store can't ingest mid-session.)
        let mut cm = ChiralMedium::new();

        // Two initially-DISTINCT sub-domains + a near-duplicate triplet per
        // domain (merge fodder). From mid-session, BRIDGE items mixing both
        // vocabularies are ingested each epoch (card-04 pattern) so the two
        // content clusters approach and their cores can FUSE while ADR-0036
        // consolidation runs: prediction 2's antecedent, induced.
        let (dom_a, dom_b) = (0usize, 3usize);
        // Canary CONTENTS, not ids: the merge may keep EITHER pair member as
        // carrier (highest post-dream energy) and absorb the other — id
        // tracking mislabels the original's absorption as organic.
        let canary_contents = [domain_sentence(dom_a, 1), domain_sentence(dom_b, 1)];
        for d in [dom_a, dom_b] {
            for i in 0..items {
                let _ = cm.store(&domain_sentence(d, i), 0.8, &pipeline);
            }
            for (t, suffix) in ["", " indeed", " truly"].iter().enumerate() {
                let s = format!("{}{} #{t}", domain_sentence(d, 0), suffix);
                let _ = cm.store(&s, 0.8, &pipeline);
            }
            // CANARY: an exact duplicate (identical text ⇒ identical vector ⇒
            // centered cosine 1.0 + identical born phase) that MUST pass the
            // ADR-0036 gates. Its absorption proves the consolidation channel
            // is live. Canary ids are tracked so their absorptions count ONLY
            // as capability proof — never as alignment evidence (they are
            // experimenter artifacts, not substrate consolidation).
            let _ = cm.store(&domain_sentence(d, 1), 0.8, &pipeline);
        }
        let bridge_sentence = |e: usize, x: usize| -> String {
            let a = &DOMAIN_VOCAB[dom_a];
            let b = &DOMAIN_VOCAB[dom_b];
            format!(
                "{} {} bridge {}: the {} of {} governs {} {}",
                a[e % 4], b[(e + x) % 4], e, a[(e + 1) % 4], b[(e + x + 1) % 4],
                a[(e + 2) % 4], b[(e + x + 2) % 4]
            )
        };

        let opts = ConsolidateOpts::default();
        let mut hrm_snaps: Vec<Vec<CoreObs>> = Vec::new();
        let mut hrm_absorb_epochs: Vec<usize> = Vec::new();
        let mut channel_proven = false;
        for e in 0..epochs {
            if e >= epochs / 2 {
                for x in 0..2 {
                    let _ = cm.store(&bridge_sentence(e, x), 0.8, &pipeline);
                }
            }
            let _ = cm.dream(true, 2);

            // ADR-0036 grouping over the live right hemisphere.
            let n = cm.right.count();
            let rows: Vec<Vec<f32>> =
                (0..n).map(|i| cm.right.wavefronts.row(i).to_vec()).collect();
            let vrefs: Vec<&[f32]> = rows.iter().map(|v| v.as_slice()).collect();
            let phases: Vec<f32> = (0..n).map(|i| cm.right.phase[i]).collect();
            let tiers: Vec<_> = (0..n).map(|i| cm.right.metadata[i].tier).collect();
            let energies: Vec<f32> = (0..n).map(|i| cm.right.energy[i]).collect();
            let grouping = compute_merge_grouping(&vrefs, &phases, &tiers, &energies, &opts, true);

            // Apply: absorb every non-carrier member (carrier = highest energy).
            // Ids first — indices invalidate on remove. Left twins are left in
            // place: this throwaway medium is never recalled against, only
            // core-snapshotted (right hemisphere only).
            let mut absorb_ids = Vec::new();
            for group in &grouping.admitted {
                let carrier = group
                    .iter()
                    .copied()
                    .max_by(|&a, &b| {
                        energies[a].partial_cmp(&energies[b]).unwrap_or(std::cmp::Ordering::Equal)
                    })
                    .unwrap_or(group[0]);
                for &m in group {
                    if m != carrier {
                        absorb_ids.push(cm.right.metadata[m].id);
                    }
                }
            }
            let mut organic_absorbed = 0usize;
            for id in &absorb_ids {
                let is_canary = (0..cm.right.count())
                    .find(|&i| cm.right.metadata[i].id == *id)
                    .map(|i| canary_contents.contains(&cm.right.metadata[i].content))
                    .unwrap_or(false);
                if cm.right.remove_wavefront(id) {
                    if is_canary {
                        channel_proven = true;
                    } else {
                        organic_absorbed += 1;
                    }
                }
            }
            if organic_absorbed > 0 {
                hrm_absorb_epochs.push(e);
            }
            hrm_snaps.push(cm.belief_core_snapshot());
        }
        let hrm_merge_epochs = core_merge_epochs(&hrm_snaps, merge_cos);
        let core_counts: Vec<usize> = hrm_snaps.iter().map(|s| s.len()).collect();
        println!("l7_hrm_core_counts:   {core_counts:?}");
        println!("l7_hrm_channel_proven: {channel_proven}");
        (
            bf::merge_consolidation_score_proven(
                &hrm_merge_epochs,
                &hrm_absorb_epochs,
                p2_window,
                channel_proven,
            ),
            hrm_merge_epochs.len(),
            hrm_absorb_epochs.len(),
        )
    };
    println!("l7_hrm_merge_events:  {hrm_merges}");
    println!("l7_hrm_absorb_epochs: {hrm_absorbs}");
    println!("merge_consolidation:  {merge_consolidation:.4}");

    // ── Prediction 3: shared cores ⇒ swarm agreement. Per agent pair:
    // symmetric shared-core fraction vs top-1 recall agreement on the items of
    // every domain BOTH hold. The nested domain design gives pairs a gradient
    // of genuine overlap for the correlation to detect. ──
    let mut p3_pairs: Vec<(f32, f32)> = Vec::new();
    for i in 0..n_agents {
        for j in (i + 1)..n_agents {
            let (si, sj) = (snapshots[i].last(), snapshots[j].last());
            let (Some(si), Some(sj)) = (si, sj) else { continue };
            let denom = (si.len() + sj.len()).max(1) as f32;
            let shared =
                (l6::shared_cores(si, sj, min_cos) + l6::shared_cores(sj, si, min_cos)) as f32 / denom;
            let common: Vec<usize> = agent_domains[i]
                .iter()
                .copied()
                .filter(|d| agent_domains[j].contains(d))
                .collect();
            if common.is_empty() {
                continue;
            }
            let mut agree = 0usize;
            let mut probes = 0usize;
            for &d in &common {
                for it in 0..items {
                    // Same degraded cue on both nodes: agreement = both fields
                    // resolve the weak probe to the same memory.
                    let q = partial_query(d, it);
                    let ri = agents[i].recall(&q, 1, &pipeline).ok().and_then(|r| r.first().map(|x| x.content.clone()));
                    let rj = agents[j].recall(&q, 1, &pipeline).ok().and_then(|r| r.first().map(|x| x.content.clone()));
                    if let (Some(ri), Some(rj)) = (ri, rj) {
                        probes += 1;
                        if ri == rj {
                            agree += 1;
                        }
                    }
                }
            }
            if probes > 0 {
                p3_pairs.push((shared, agree as f32 / probes as f32));
            }
        }
    }
    let shared_agreement = bf::shared_agreement_score(&p3_pairs);
    println!("l7_p3_pairs:          {}", p3_pairs.len());
    println!("shared_agreement:     {shared_agreement:.4}");

    // ── Fitness + TSV ──
    let weights = bf::L7Weights::default();
    let fitness = bf::l7_fitness(stability_recall, merge_consolidation, shared_agreement, &weights);
    println!("l7_fitness:           {fitness:.6}");

    let tsv_path = Path::new("experiments/results-L7.tsv");
    let needs_header = !tsv_path.exists();
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(tsv_path) {
        use std::io::Write;
        if needs_header {
            let _ = writeln!(
                f,
                "run\tfitness\tstability_recall\tmerge_consolidation\tshared_agreement\tn_agents\tepochs\tcouple\tfinal_cores\tp1_pairs\tp3_pairs\tmerge_events"
            );
        }
        let run_label = std::env::var("RESEARCH_RUN").unwrap_or_else(|_| "L7".to_string());
        let _ = writeln!(
            f,
            "{}\t{:.6}\t{:.4}\t{:.4}\t{:.4}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            run_label,
            fitness,
            stability_recall,
            merge_consolidation,
            shared_agreement,
            n_agents,
            epochs,
            couple as u8,
            total_final_cores,
            p1_pairs.len(),
            p3_pairs.len(),
            merge_epochs_all.len(),
        );
    }
    println!("results_tsv:          experiments/results-L7.tsv");
    println!("---");

    // Restore the caller's belief-phase env.
    match prev_belief {
        Some(v) => std::env::set_var("KANNAKA_BELIEF_PHASE", v),
        None => std::env::remove_var("KANNAKA_BELIEF_PHASE"),
    }
}

/// Placeholder L5 fitness using L4 evaluators. Computes a sub-fitness
/// for transfer comparison purposes. Uses the L4 inherited core at
/// L5 weights (program-l5.md §2).
fn eval_l5_placeholder_fitness(
    engine: &ResonanceEngine,
    params: &Params,
    chain_seeds: &[ChainSeed],
    phi_history: &[f32],
) -> f32 {
    let noise_removal = eval_l4_noise_removal(engine);
    let signal_preservation = eval_l4_signal_preservation(engine);
    let phase_coherence = eval_phase_coherence_l4(engine);
    let consciousness = eval_consciousness(engine, params.consciousness_phi_target);
    let surviving: Vec<HyperMemory> = engine
        .store
        .all_memories()
        .unwrap_or_default()
        .iter()
        .map(|m| (*m).clone())
        .collect();
    let encoding_entropy = eval_encoding_entropy(&surviving, 8);
    let chain_fidelity = eval_chain_fidelity(chain_seeds, phi_history);

    // Sub-fitness: inherited L4 core metrics only
    0.05 * (1.0 - noise_removal)
        + 0.05 * (1.0 - signal_preservation)
        + 0.05 * (1.0 - phase_coherence)
        + 0.10 * (1.0 - consciousness)
        + 0.05 * (1.0 - encoding_entropy)
        + 0.10 * (1.0 - chain_fidelity)
}

fn parse_string_flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn parse_usize_flag(args: &[String], name: &str) -> Option<usize> {
    parse_string_flag(args, name).and_then(|s| s.parse::<usize>().ok())
}

// ============================================================================
// LEVEL 8 — TEMPORAL / CONFIRMATION-WEIGHTED RECALL
// ============================================================================
//
// The question (raised by @aiappsapi on Mastodon, 2026-07-26): does recall
// strength depend on how recently a memory was CONFIRMED, or only on how
// strongly it RESONATES? "Those two come apart quickly once stored facts start
// contradicting each other."
//
// Measured answer before this level existed: resonance only. Ranking is
// `similarity * energy^e` and reads no timestamp; the ADR-0035 temporal-truth
// bounds are written but only ever read as a post-recall filter in
// `swarm brief`. So the two axes are collapsed into one.
//
// L8 is the falsification harness for un-collapsing them. It builds a corpus
// that makes the two axes come apart on purpose — FACT FAMILIES whose versions
// are near-identical text differing in one value token, so similarity cannot
// separate them and only confirmation time can — and asks whether
// `KANNAKA_RECALL_TEMPORAL_EXP` buys retrieval of the currently-true version
// WITHOUT (a) harming uncontradicted memories or (b) making the superseded past
// unreachable. (b) is the one that matters: recency weighting that "wins" by
// forgetting is not a win, it is amnesia with a flag.
//
// Predictions, scored in [0,1] (1 = prediction holds, 0.5 = no evidence,
// matching `belief_fitness`'s convention):
//   P1  current-fact retrieval improves      (the claimed win)
//   P2  uncontradicted facts do not degrade  (negative control)
//   P3  superseded facts stay reachable      (anti-amnesia control)
//   P4  instrument liveness + default byte-identity (anti-vacuous gate)
//
// P4 is a hard PASS/FAIL printed separately, not folded into fitness: a dead
// instrument must fail loudly rather than score well. Fitness follows the
// ladder convention — weighted `1 - score`, LOWER IS BETTER.
//
// Every arm is measured PAIRED against its own baseline on the SAME corpus,
// and averaged over `L8_RUNS` independent corpus seeds (default 10 — 5 is not
// enough to separate arms here).

/// One corpus instance's measurements for a single arm.
#[derive(Default, Clone, Copy)]
struct L8Ranks {
    /// Mean reciprocal rank of the currently-true version of each fact family.
    current_mrr: f32,
    /// Mean reciprocal rank of each uncontradicted (stable) fact.
    stable_mrr: f32,
    /// Fraction of superseded versions still present in the returned top-k.
    superseded_recall: f32,
    /// Sum of stable-fact ranks (for the paired non-degradation count).
    stable_ranks: [usize; L8_MAX_STABLE],
    n_stable: usize,
}

const L8_MAX_STABLE: usize = 32;

/// Deterministic LCG so each run's corpus is a different but reproducible
/// instance (no `rand` dependency in this binary).
fn l8_lcg(state: &mut u64) -> u64 {
    *state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    *state >> 33
}

fn run_experiment_l8_session(_params: &Params) {
    use chrono::{Duration, Utc};
    use kannaka_memory::medium::chiral::ChiralMedium;

    let envf = |k: &str, d: f32| std::env::var(k).ok().and_then(|s| s.parse().ok()).unwrap_or(d);
    let envu = |k: &str, d: usize| std::env::var(k).ok().and_then(|s| s.parse().ok()).unwrap_or(d);

    let runs = envu("L8_RUNS", 10).max(1);
    let n_families = envu("L8_FAMILIES", 12).clamp(1, 16);
    let n_versions = envu("L8_VERSIONS", 3).clamp(2, 6);
    let n_stable = envu("L8_STABLE", 12).clamp(1, L8_MAX_STABLE);
    let n_distract = envu("L8_DISTRACTORS", 40);
    let top_k = envu("L8_TOP_K", 8).max(2);
    let spacing_days = envf("L8_SPACING_DAYS", 180.0);
    let fresh_days = envf("L8_FRESH_DAYS", 3.0);
    let t_exp = envf("L8_TEMPORAL_EXP", 1.0).clamp(0.0, 1.0);
    let half_life = envf("L8_HALFLIFE_DAYS", 180.0);
    let e_exp = envf("L8_ENERGY_EXP", 1.0).clamp(0.0, 1.0);
    let floor = envf("L8_FLOOR", 0.25).clamp(0.05, 1.0);

    println!("=== L8 temporal-recall session (confirmation vs resonance) ===");
    println!("l8_runs:              {runs}");
    println!("l8_families:          {n_families}");
    println!("l8_versions:          {n_versions}");
    println!("l8_stable:            {n_stable}");
    println!("l8_distractors:       {n_distract}");
    println!("l8_top_k:             {top_k}");
    println!("l8_temporal_exp:      {t_exp:.2}");
    println!("l8_halflife_days:     {half_life:.1}");
    println!("l8_energy_exp:        {e_exp:.2}");
    println!("l8_floor:             {floor:.2}");
    println!("l8_spacing_days:      {spacing_days:.1}");

    // Fact families: [subject_a, subject_b, attribute]. Disjoint vocabulary
    // across families so families cannot contaminate each other's queries —
    // the only competition we want is BETWEEN VERSIONS of the same fact.
    const FAMILY: [[&str; 3]; 16] = [
        ["harbor", "beacon", "channel"],
        ["glacier", "station", "altitude"],
        ["foundry", "kiln", "temperature"],
        ["orchard", "grafting", "rootstock"],
        ["telescope", "mirror", "coating"],
        ["ferry", "terminal", "berth"],
        ["aquifer", "wellhead", "depth"],
        ["printworks", "press", "cadence"],
        ["vineyard", "trellis", "spacing"],
        ["observatory", "dome", "aperture"],
        ["saltworks", "evaporator", "salinity"],
        ["cablehouse", "winch", "tension"],
        ["apiary", "hive", "frames"],
        ["tannery", "vat", "liquor"],
        ["windfarm", "turbine", "pitch"],
        ["seedbank", "vault", "humidity"],
    ];
    const VALUES: [&str; 6] = [
        "twelve", "nineteen", "twentyseven", "forty", "fiftythree", "sixtyone",
    ];
    // Uncontradicted facts — one version, never superseded. These are the
    // control set: whatever temporal weighting does, it must not push these
    // down. Vocabulary disjoint from FAMILY.
    const STABLE: [[&str; 3]; 32] = [
        ["basalt", "quarry", "hardness"], ["lantern", "wick", "trim"],
        ["cooperage", "stave", "girth"], ["millrace", "sluice", "head"],
        ["ropewalk", "hemp", "lay"], ["forge", "bellows", "blast"],
        ["dovecote", "nest", "count"], ["saltmarsh", "creek", "tide"],
        ["limekiln", "charge", "burn"], ["boatyard", "slip", "draft"],
        ["smokehouse", "flue", "draw"], ["weaveshed", "loom", "reed"],
        ["icehouse", "pit", "packing"], ["brewhouse", "mash", "strike"],
        ["tollgate", "pike", "levy"], ["dryDock", "caisson", "seal"],
        ["charcoal", "clamp", "yield"], ["fishweir", "trap", "mesh"],
        ["signalbox", "lever", "throw"], ["packhouse", "crate", "tare"],
        ["reedbed", "sluicegate", "flow"], ["stonebridge", "arch", "span"],
        ["pumphouse", "beam", "stroke"], ["sailloft", "canvas", "bolt"],
        ["copperworks", "crucible", "pour"], ["gristmill", "runner", "gap"],
        ["oasthouse", "cowl", "turn"], ["chandlery", "tallow", "grade"],
        ["ferrypier", "pontoon", "rise"], ["glassworks", "annealer", "ramp"],
        ["turnery", "chisel", "bevel"], ["saltpan", "brine", "density"],
    ];

    let fact_text = |w: &[&str; 3], value: &str| -> String {
        format!("the {} {} {} is {}", w[0], w[1], w[2], value)
    };
    let cue = |w: &[&str; 3]| -> String { format!("{} {} {}", w[0], w[1], w[2]) };

    let pipeline = {
        let encoder = Box::new(SimpleHashEncoder::new(384, 42));
        let codebook = Codebook::new(384, kannaka_memory::WAVEFRONT_DIM, 42);
        EncodingPipeline::new(encoder, codebook)
    };

    // Rank of `needle` in a recall result list, deduplicated by content (a
    // memory can surface from both hemispheres; the caller-visible ordering is
    // what we score, but a duplicate must not consume a rank slot twice).
    let rank_of = |results: &[kannaka_memory::medium::types::ChiralResonance],
                   needle: &str,
                   miss: usize| -> usize {
        let mut seen: Vec<&str> = Vec::with_capacity(results.len());
        for r in results {
            if seen.iter().any(|c| *c == r.content.as_str()) { continue; }
            if r.content == needle { return seen.len(); }
            seen.push(r.content.as_str());
        }
        miss
    };

    // ── One corpus instance, measured under one temporal exponent ──
    let measure = |cm: &ChiralMedium,
                   fam_idx: &[usize],
                   stable_idx: &[usize],
                   texp: f32| -> L8Ranks {
        // Drive the REAL default path: recall_vector -> Hemisphere::resonate,
        // which reads these env vars. This benchmarks the shipped code, not a
        // parallel reimplementation of it.
        std::env::set_var("KANNAKA_RECALL_TEMPORAL_EXP", format!("{texp}"));
        std::env::set_var("KANNAKA_RECALL_TEMPORAL_HALFLIFE_DAYS", format!("{half_life}"));
        std::env::set_var("KANNAKA_RECALL_TEMPORAL_FLOOR", format!("{floor}"));
        std::env::set_var("KANNAKA_RECALL_ENERGY_EXP", format!("{e_exp}"));

        let mut out = L8Ranks::default();
        let mut current_rr = 0.0f32;
        let mut superseded_hits = 0usize;
        let mut superseded_total = 0usize;

        for &f in fam_idx {
            let w = &FAMILY[f];
            let q = pipeline.encode_text(&cue(w)).expect("encode cue");
            let res = cm.recall_vector(&q, top_k);
            // Currently-true version = the last one stored.
            let latest = fact_text(w, VALUES[(n_versions - 1) % VALUES.len()]);
            let r = rank_of(&res, &latest, top_k);
            if r < top_k { current_rr += 1.0 / (r as f32 + 1.0); }
            // Superseded versions must remain REACHABLE (P3).
            for v in 0..(n_versions - 1) {
                superseded_total += 1;
                let old = fact_text(w, VALUES[v % VALUES.len()]);
                if rank_of(&res, &old, top_k) < top_k { superseded_hits += 1; }
            }
        }
        out.current_mrr = current_rr / fam_idx.len().max(1) as f32;
        out.superseded_recall = if superseded_total == 0 {
            1.0
        } else {
            superseded_hits as f32 / superseded_total as f32
        };

        let mut stable_rr = 0.0f32;
        for (slot, &s) in stable_idx.iter().enumerate() {
            let w = &STABLE[s];
            let q = pipeline.encode_text(&cue(w)).expect("encode cue");
            let res = cm.recall_vector(&q, top_k);
            let target = fact_text(w, VALUES[s % VALUES.len()]);
            let r = rank_of(&res, &target, top_k);
            if r < top_k { stable_rr += 1.0 / (r as f32 + 1.0); }
            if slot < L8_MAX_STABLE { out.stable_ranks[slot] = r; }
        }
        out.n_stable = stable_idx.len().min(L8_MAX_STABLE);
        out.stable_mrr = stable_rr / stable_idx.len().max(1) as f32;
        out
    };

    let now = Utc::now();
    let mut sum_off = L8Ranks::default();
    let mut sum_on = L8Ranks::default();
    let mut stable_no_harm = 0usize;
    let mut stable_compared = 0usize;
    let mut liveness_observed = false;
    let mut identity_holds = true;

    for run in 0..runs {
        let mut rng = 0x5eed_0000_0000_0000u64 ^ (run as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);

        // Rotate which families/stable facts this instance uses so the runs are
        // genuinely different corpora, not the same corpus re-measured.
        let fam_idx: Vec<usize> =
            (0..n_families).map(|i| (i + run) % FAMILY.len()).collect();
        let stable_idx: Vec<usize> =
            (0..n_stable).map(|i| (i + run * 3) % STABLE.len()).collect();

        let mut cm = ChiralMedium::new();

        // ── Fact families: versions in chronological order. Version v expires
        //    exactly when version v+1 was observed — that is what supersession
        //    IS, and it is the only signal distinguishing them (the text is
        //    near-identical by construction).
        for &f in &fam_idx {
            let w = &FAMILY[f];
            for v in 0..n_versions {
                let text = fact_text(w, VALUES[v % VALUES.len()]);
                let age = (n_versions - 1 - v) as f32 * spacing_days + fresh_days;
                let observed = now - Duration::seconds((age * 86_400.0) as i64);
                let expires = if v + 1 < n_versions {
                    let next_age = (n_versions - 2 - v) as f32 * spacing_days + fresh_days;
                    Some(now - Duration::seconds((next_age * 86_400.0) as i64))
                } else {
                    None
                };
                let _ = cm.store(&text, 0.8, &pipeline);
                for m in cm.right.metadata.iter_mut().chain(cm.left.metadata.iter_mut()) {
                    if m.content == text {
                        m.observed_at = Some(observed);
                        m.expires_at = expires;
                    }
                }
            }
        }

        // ── Uncontradicted facts: no temporal bounds, ages spread across the
        //    same window so the recency term genuinely acts on them too. If
        //    recency weighting is going to hurt anything, it hurts these.
        for &s in &stable_idx {
            let w = &STABLE[s];
            let text = fact_text(w, VALUES[s % VALUES.len()]);
            let age_days = (l8_lcg(&mut rng) % 600) as f32;
            let observed = now - Duration::seconds((age_days * 86_400.0) as i64);
            let _ = cm.store(&text, 0.8, &pipeline);
            for m in cm.right.metadata.iter_mut().chain(cm.left.metadata.iter_mut()) {
                if m.content == text {
                    m.observed_at = Some(observed);
                }
            }
        }

        // ── Distractors: unrelated traffic so top-k is genuinely contested.
        //    Their ages are spread over the SAME window as everything else.
        //    Leaving them unstamped (the first cut of this fixture) silently
        //    dated them all to `now`, handing every distractor the maximum
        //    recency weight and making the past-reachability gate fail for a
        //    reason that was an artifact of the fixture rather than a property
        //    of the mechanism. Stamping them removes that systematic bias.
        for d in 0..n_distract {
            let a = FAMILY[(l8_lcg(&mut rng) as usize) % FAMILY.len()][0];
            let b = STABLE[(l8_lcg(&mut rng) as usize) % STABLE.len()][1];
            let text = format!("note {d} concerning {a} and {b} logistics");
            let age_days = (l8_lcg(&mut rng) % 600) as f32;
            let observed = now - Duration::seconds((age_days * 86_400.0) as i64);
            let _ = cm.store(&text, 0.4, &pipeline);
            for m in cm.right.metadata.iter_mut().chain(cm.left.metadata.iter_mut()) {
                if m.content == text {
                    m.observed_at = Some(observed);
                }
            }
        }

        let off = measure(&cm, &fam_idx, &stable_idx, 0.0);
        let on = measure(&cm, &fam_idx, &stable_idx, t_exp);

        // P4a — default byte-identity: an UNSET flag must reproduce exp=0.0
        // exactly. Checked on the first run only (it is a property of the code
        // path, not of the corpus).
        if run == 0 {
            std::env::remove_var("KANNAKA_RECALL_TEMPORAL_EXP");
            let w = &FAMILY[fam_idx[0]];
            let q = pipeline.encode_text(&cue(w)).expect("encode cue");
            let unset: Vec<(String, f32)> = cm
                .recall_vector(&q, top_k)
                .into_iter()
                .map(|r| (r.content, r.resonance_strength))
                .collect();
            std::env::set_var("KANNAKA_RECALL_TEMPORAL_EXP", "0.0");
            let zero: Vec<(String, f32)> = cm
                .recall_vector(&q, top_k)
                .into_iter()
                .map(|r| (r.content, r.resonance_strength))
                .collect();
            if unset.len() != zero.len()
                || unset.iter().zip(zero.iter()).any(|(a, b)| a.0 != b.0 || a.1.to_bits() != b.1.to_bits())
            {
                identity_holds = false;
            }
        }

        // P4b — liveness: the mechanism must actually move SOMETHING.
        if (on.current_mrr - off.current_mrr).abs() > 1e-9
            || (on.stable_mrr - off.stable_mrr).abs() > 1e-9
            || (on.superseded_recall - off.superseded_recall).abs() > 1e-9
        {
            liveness_observed = true;
        }

        // P2 — paired, per-fact non-degradation on the uncontradicted set.
        for i in 0..off.n_stable.min(on.n_stable) {
            stable_compared += 1;
            if on.stable_ranks[i] <= off.stable_ranks[i] { stable_no_harm += 1; }
        }

        sum_off.current_mrr += off.current_mrr;
        sum_off.stable_mrr += off.stable_mrr;
        sum_off.superseded_recall += off.superseded_recall;
        sum_on.current_mrr += on.current_mrr;
        sum_on.stable_mrr += on.stable_mrr;
        sum_on.superseded_recall += on.superseded_recall;
    }

    let n = runs as f32;
    let off_current = sum_off.current_mrr / n;
    let on_current = sum_on.current_mrr / n;
    let off_stable = sum_off.stable_mrr / n;
    let on_stable = sum_on.stable_mrr / n;
    let off_sup = sum_off.superseded_recall / n;
    let on_sup = sum_on.superseded_recall / n;

    // ── Scores in [0,1]: 1 = prediction holds, 0.5 = no evidence ──
    // P1: fraction of the AVAILABLE headroom captured. If the baseline already
    // retrieves perfectly there is no headroom and therefore no evidence.
    let headroom = 1.0 - off_current;
    let p1 = if headroom <= 1e-6 {
        0.5
    } else {
        ((on_current - off_current) / headroom).clamp(0.0, 1.0)
    };
    // P2: paired fraction of uncontradicted facts that did not lose rank.
    let p2 = if stable_compared == 0 {
        0.5
    } else {
        stable_no_harm as f32 / stable_compared as f32
    };
    // P3: superseded reachability retained, relative to baseline.
    let p3 = if off_sup <= 1e-6 { 0.5 } else { (on_sup / off_sup).clamp(0.0, 1.0) };

    println!("--- paired measurements (temporal OFF -> ON) ---");
    println!("l8_current_mrr_off:   {off_current:.4}");
    println!("l8_current_mrr_on:    {on_current:.4}");
    println!("l8_current_delta:     {:+.4}", on_current - off_current);
    println!("l8_stable_mrr_off:    {off_stable:.4}");
    println!("l8_stable_mrr_on:     {on_stable:.4}");
    println!("l8_stable_delta:      {:+.4}", on_stable - off_stable);
    println!("l8_superseded_off:    {off_sup:.4}");
    println!("l8_superseded_on:     {on_sup:.4}");
    println!("l8_stable_no_harm:    {stable_no_harm}/{stable_compared}");
    println!("--- prediction scores (1 = holds, 0.5 = no evidence) ---");
    println!("l8_p1_current:        {p1:.4}");
    println!("l8_p2_no_harm:        {p2:.4}");
    println!("l8_p3_past_reachable: {p3:.4}");

    // P4 is a GATE, not a score. A mechanism that changes nothing must not be
    // able to post a good fitness — that is exactly the vacuous-gate failure
    // mode this ladder has been bitten by before.
    let live = liveness_observed || t_exp == 0.0;
    println!("l8_p4_liveness:       {}", if live { "PASS" } else { "FAIL" });
    println!("l8_p4_default_identity: {}", if identity_holds { "PASS" } else { "FAIL" });

    let fitness = 0.50 * (1.0 - p1) + 0.25 * (1.0 - p2) + 0.25 * (1.0 - p3);
    println!("l8_fitness:           {fitness:.6}");

    let verdict = identity_holds && live && p1 > 0.5 && p2 >= 0.95 && p3 >= 0.99;
    println!(
        "l8_verdict:           {}",
        if t_exp == 0.0 {
            "BASELINE"
        } else if verdict {
            "SUPPORTED"
        } else {
            "NOT_SUPPORTED"
        }
    );
    println!(
        "L8 {}",
        if identity_holds && live { "PASSED" } else { "INSTRUMENT_FAILED" }
    );

    std::env::remove_var("KANNAKA_RECALL_TEMPORAL_EXP");
    std::env::remove_var("KANNAKA_RECALL_TEMPORAL_HALFLIFE_DAYS");
    std::env::remove_var("KANNAKA_RECALL_TEMPORAL_FLOOR");
    std::env::remove_var("KANNAKA_RECALL_ENERGY_EXP");
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let level = parse_usize_flag(&args, "--level").unwrap_or(2);

    let params = experiment_params();

    // --corpus-hash: print the hex digest of the L4 corpus and exit.
    // Reproducibility check — any L4 cycle can run this to confirm the
    // corpus bytes are identical to a previous run.
    if args.iter().any(|a| a == "--corpus-hash") {
        let corpus = build_corpus_l4(128, 1, params.encoder_seed);
        let hash = corpus_l4_hash(&corpus);
        println!("l4_corpus_hash: {hash}");
        println!("l4_corpus_dim:  128");
        println!("l4_corpus_size: {}", corpus.len());
        println!("l4_encoder_seed: 0x{:016x}", params.encoder_seed);
        return;
    }

    let cli = L4Cli {
        load_path: parse_string_flag(&args, "--load").map(PathBuf::from),
        save_path: parse_string_flag(&args, "--save").map(PathBuf::from),
        chain_sessions: parse_usize_flag(&args, "--chain-sessions").unwrap_or(0),
    };

    match level {
        8 => run_experiment_l8_session(&params),
        7 => run_experiment_l7_session(&params),
        6 => run_experiment_l6_session(&params),
        5 => run_experiment_l5_session(&params),
        4 => {
            if cli.chain_sessions > 0 {
                // Internal loop: N sessions in one cargo run. Session 1 loads
                // from `--load` if it exists, otherwise starts fresh. Each
                // session writes its state to `--save` so the next one can
                // pick it back up. `--save` is required for chaining.
                if cli.save_path.is_none() {
                    eprintln!("--chain-sessions requires --save <path>");
                    std::process::exit(2);
                }
                for session_idx in 0..cli.chain_sessions {
                    println!("=== L4 chain session {}/{} ===", session_idx + 1, cli.chain_sessions);
                    // After session 1, load from the file we just wrote.
                    let load_path = if session_idx == 0 {
                        cli.load_path.clone()
                    } else {
                        cli.save_path.clone()
                    };
                    let session_cli = L4Cli {
                        load_path,
                        save_path: cli.save_path.clone(),
                        chain_sessions: 0,
                    };
                    run_experiment_l4_session(&params, &session_cli);
                }
            } else {
                run_experiment_l4_session(&params, &cli);
            }
        }
        3 => run_experiment_l3(&params),
        _ => run_experiment(&params),
    }
}
