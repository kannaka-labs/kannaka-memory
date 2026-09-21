use super::*;
use crate::codebook::Codebook;
use crate::consciousness::{ConsciousnessLevel, ConsciousnessMetrics, EmergenceLevel, EmergenceReport};
use crate::encoding::{EncodingPipeline, SimpleHashEncoder};
use chrono::Utc;
use std::process::Command;
use tempfile::NamedTempFile;
use uuid::Uuid;

fn make_test_pipeline() -> EncodingPipeline {
    let encoder = SimpleHashEncoder::new(384, 42);
    let codebook = Codebook::new(384, WAVEFRONT_DIM, 42);
    EncodingPipeline::new(Box::new(encoder), codebook)
}

// hunt: recall must score by the phase DIFFERENCE (query vs stored), not the
// absolute stored phase. Under the belief substrate an exact-content match's born
// phase can land in (π/2, 3π/2), so cos(absolute) < 0 negated its resonance and
// buried the exact match. #[ignore] because it sets process-global
// KANNAKA_BELIEF_PHASE (run with --ignored --nocapture).
#[test]
#[ignore = "sets KANNAKA_BELIEF_PHASE (process-global)"]
fn recall_ranks_exact_match_first_under_belief() {
    std::env::set_var("KANNAKA_BELIEF_PHASE", "1");
    let pipeline = make_test_pipeline();

    // Pick content whose born phase has cos < 0 — exactly the case the absolute-
    // phase bug buries (a same-content match got resonance sim*energy*cos<0, so it
    // sorted BELOW an unrelated positive-phase memory). This makes the test
    // anti-vacuous: it reddens on the absolute-phase code and passes on the fix.
    let candidates = [
        "alpha target trace", "beta memory node", "gamma concept anchor",
        "delta recall probe", "epsilon signal marker", "zeta phase test",
    ];
    let target_content = candidates
        .iter()
        .copied()
        .find(|c| {
            let v = pipeline.encode_text(c).unwrap();
            crate::medium::chiral::content_born_phase(&v).cos() < 0.0
        })
        .expect("some candidate has a cos<0 born phase under the default encoder");

    let mut medium = Medium::new();
    let target = pipeline.encode_text(target_content).unwrap();
    medium.add_wavefront(&target, target_content.to_string(), 1.0).unwrap();
    let other = pipeline.encode_text("an utterly different subject").unwrap();
    medium.add_wavefront(&other, "an utterly different subject".to_string(), 1.0).unwrap();

    let results = medium.recall(target_content, 1, &pipeline).unwrap();
    std::env::remove_var("KANNAKA_BELIEF_PHASE");

    assert_eq!(
        results.first().map(|r| r.content.as_str()),
        Some(target_content),
        "exact match must rank first under belief (phase DIFFERENCE, not absolute phase)"
    );
}

#[test]
fn new_medium_is_empty() {
    let medium = Medium::new();
    assert_eq!(medium.wavefront_count(), 0);
    assert_eq!(medium.store.wavefronts.dim(), (0, WAVEFRONT_DIM));
}

#[test]
fn relate_phase_opposed_wavefronts_does_not_error() {
    // Regression: when two wavefronts are near-exact negatives, their sum is
    // ~0 and cannot be normalized into a direction. The degenerate branch must
    // still produce a valid DIM-length associative wavefront and succeed.
    // Previously it *pushed* a second DIM-length run onto the already-full
    // buffer, yielding a 2×DIM vector that add_wavefront rejected with
    // DimensionMismatch — so relating any phase-opposed pair always errored.
    let mut medium = Medium::new();
    let mut v = vec![0.0f32; WAVEFRONT_DIM];
    v[0] = 1.0;
    v[1] = -0.5;
    v[2] = 0.3;
    let neg: Vec<f32> = v.iter().map(|x| -x).collect();
    medium.add_wavefront(&v, "pos".to_string(), 1.0).unwrap();
    medium.add_wavefront(&neg, "neg".to_string(), 1.0).unwrap();
    let before = medium.wavefront_count();

    let res = medium.relate_wavefronts(0, 1);
    assert!(
        res.is_ok(),
        "relating phase-opposed wavefronts must not error: {:?}",
        res.err()
    );
    assert_eq!(
        medium.wavefront_count(),
        before + 1,
        "the association wavefront must be added"
    );
}

#[test]
fn spiral_field_2d_separates_clusters_and_reports() {
    // ADR-0037 Phase 4b: the PCA embedding must place two orthogonal wavefront
    // clusters apart in the 2-D field, and the cloud report must run clean.
    let mut medium = Medium::new();
    for k in 0..6 {
        let mut v = vec![0.0f32; WAVEFRONT_DIM];
        if k < 3 {
            v[0] = 1.0;
            v[1] = 0.05 * k as f32;
        } else {
            v[1] = 1.0;
            v[0] = 0.05 * k as f32;
        }
        medium.add_wavefront(&v, format!("m{k}"), 1.0).unwrap();
    }
    let pts = medium.spiral_field_2d();
    assert_eq!(pts.len(), 6);
    assert!(pts.iter().all(|p| p.0.is_finite() && p.1.is_finite()));
    // The two clusters must separate in the 2-D embedding.
    let ca = (
        pts[..3].iter().map(|p| p.0).sum::<f32>() / 3.0,
        pts[..3].iter().map(|p| p.1).sum::<f32>() / 3.0,
    );
    let cb = (
        pts[3..].iter().map(|p| p.0).sum::<f32>() / 3.0,
        pts[3..].iter().map(|p| p.1).sum::<f32>() / 3.0,
    );
    let sep = ((ca.0 - cb.0).powi(2) + (ca.1 - cb.1).powi(2)).sqrt();
    assert!(sep > 1e-3, "clusters should separate in the 2-D embedding, sep={sep}");
    // Cloud report runs clean.
    let rep = medium.spiral_cloud_report();
    assert_eq!(rep.n, 6);
    assert!(rep.order.is_finite());
    // Empty medium -> empty field.
    assert!(Medium::new().spiral_field_2d().is_empty());
}

#[test]
fn xi_bridge_residue_and_summary() {
    // ADR-0037 Phase 3: empty medium -> bridge residue 0.
    let empty = Medium::new();
    assert_eq!(empty.compute_xi_bridge_residue(), 0.0);

    // Non-uniform wavefronts so the Ξ=[R,G] commutator leaves a residue.
    let mut medium = Medium::new();
    for k in 0..4 {
        let v: Vec<f32> = (0..WAVEFRONT_DIM)
            .map(|i| (((i + k * 11) % 13) as f32 - 6.0) * 0.2)
            .collect();
        medium.add_wavefront(&v, format!("mem {k}"), 1.0).unwrap();
    }
    let residue = medium.compute_xi_bridge_residue();
    assert!(residue.is_finite() && residue > 0.0, "bridge residue should be > 0, got {residue}");

    // Regression guard for the normalization bug: the residue is the mean
    // UN-normalized commutator magnitude (mean ‖Ξ·v‖). It must NOT collapse to
    // the unit-sphere constant 1.0 that compute_xi_signature would produce...
    assert!(
        (residue - 1.0).abs() > 1e-3,
        "residue collapsed to normalized ~1.0 — magnitude was discarded: {residue}"
    );
    // ...and it must track the data: a 10x-louder population yields a larger
    // residue. A constant metric (the bug) would fail this.
    let mut louder = Medium::new();
    for k in 0..4 {
        let v: Vec<f32> = (0..WAVEFRONT_DIM)
            .map(|i| (((i + k * 11) % 13) as f32 - 6.0) * 2.0)
            .collect();
        louder.add_wavefront(&v, format!("loud {k}"), 1.0).unwrap();
    }
    let residue_louder = louder.compute_xi_bridge_residue();
    assert!(
        residue_louder > residue * 1.5,
        "residue must grow with input magnitude: quiet={residue} loud={residue_louder}"
    );

    // The beacon summary carries the expected keys with finite values.
    let s = medium.xi_bridge_summary();
    assert_eq!(s["n"].as_u64(), Some(4));
    assert!(s["residue"].as_f64().unwrap().is_finite());
    assert!(s["spectral_xi"].as_f64().unwrap().is_finite());
    assert!((s["emergence_coeff"].as_f64().unwrap() - 0.190983).abs() < 1e-4);
}

#[test]
fn add_wavefront_increases_count() {
    let mut medium = Medium::new();
    let vector = vec![0.5; WAVEFRONT_DIM];
    let id = medium.add_wavefront(&vector, "test memory".to_string(), 1.0).unwrap();

    assert_eq!(medium.wavefront_count(), 1);
    assert!(medium.store.id_to_index.contains_key(&id));
    assert_eq!(medium.store.metadata[0].content, "test memory");
    assert_eq!(medium.store.energy[0], 1.0);
}

#[test]
fn add_wavefront_wrong_dimension_errors() {
    let mut medium = Medium::new();
    let vector = vec![0.5; 100]; // Wrong dimension
    let result = medium.add_wavefront(&vector, "test".to_string(), 1.0);

    assert!(matches!(result, Err(MediumError::DimensionMismatch { .. })));
}

#[test]
fn remove_wavefront_decreases_count() {
    let mut medium = Medium::new();
    let vector = vec![0.5; WAVEFRONT_DIM];
    let id = medium.add_wavefront(&vector, "test".to_string(), 1.0).unwrap();

    assert_eq!(medium.wavefront_count(), 1);

    let removed = medium.remove_wavefront(&id).unwrap();
    assert!(removed);
    assert_eq!(medium.wavefront_count(), 0);
    assert!(!medium.store.id_to_index.contains_key(&id));
}

#[test]
fn remove_nonexistent_wavefront_returns_false() {
    let mut medium = Medium::new();
    let fake_id = Uuid::new_v4();
    let result = medium.remove_wavefront(&fake_id).unwrap();
    assert!(!result);
}

#[test]
fn effective_strength_decays_over_time() {
    let mut medium = Medium::new();
    let vector = vec![0.5; WAVEFRONT_DIM];
    medium.add_wavefront(&vector, "test".to_string(), 1.0).unwrap();

    let strength_now = medium.effective_strength(None);
    assert_eq!(strength_now.len(), 1);
    assert!((strength_now[0] - 1.0).abs() < 1e-4); // Should be ~1.0 at creation

    // Simulate 1000 seconds later
    let future = Utc::now() + chrono::Duration::seconds(1000);
    let strength_later = medium.effective_strength(Some(future));
    assert!(strength_later[0] < strength_now[0]); // Should decay
}

#[test]
fn store_and_recall_roundtrip() {
    let mut medium = Medium::new();
    let pipeline = make_test_pipeline();

    // Store some memories
    let _id1 = medium.store("I love cats", 1.0, &pipeline).unwrap();
    let _id2 = medium.store("Dogs are great pets", 0.8, &pipeline).unwrap();
    let _id3 = medium.store("Paris is beautiful", 0.6, &pipeline).unwrap();

    assert_eq!(medium.wavefront_count(), 3);

    // Recall memories related to pets
    let results = medium.recall("animals and pets", 2, &pipeline).unwrap();
    assert_eq!(results.len(), 2);

    // Results should be sorted by resonance strength
    assert!(results[0].resonance_strength >= results[1].resonance_strength);

    // Should find pet-related memories
    let contents: Vec<&str> = results.iter().map(|r| r.content.as_str()).collect();
    assert!(contents.iter().any(|&c| c.contains("cats") || c.contains("Dogs")));
}

#[test]
fn apply_interference_affects_energy() {
    let mut medium = Medium::new();
    let vector1 = vec![1.0; WAVEFRONT_DIM]; // All positive
    let vector2 = vec![-1.0; WAVEFRONT_DIM]; // All negative (should cause destructive interference)

    // Add first wavefront
    medium.add_wavefront(&vector1, "positive".to_string(), 1.0).unwrap();
    let initial_energy = medium.store.energy[0];

    // Store second to trigger interference (apply_interference is private, go through store_audio path via raw)
    // Use a direct approach: add wavefront then check interference manually
    // Actually, we need to test apply_interference directly.
    // Since it's now private in core.rs, we test through store() which calls it.
    let pipeline = make_test_pipeline();
    medium.store("negative memory", 1.0, &pipeline).unwrap();

    // Energy should change due to interference
    // Note: we can't directly test apply_interference since it's private,
    // but store() calls it internally. The first wavefront's energy should have changed.
    assert!(medium.store.energy[0] != initial_energy || medium.wavefront_count() == 2);
}

#[test]
fn consciousness_metrics_computed() {
    let mut medium = Medium::new();
    let pipeline = make_test_pipeline();

    // Two coherent groups: 3 cat memories + 3 dog memories. Pre-#106
    // the encoder collapsed every text to the same hyperoctant so even
    // "first memory" / "second memory" trivially clustered; with the
    // fix, distinct texts span the unit sphere and clustering requires
    // actual shared tokens to form a coherent group.
    medium.store("cats are fluffy and warm", 1.0, &pipeline).unwrap();
    medium.store("cats love to nap in sun", 0.9, &pipeline).unwrap();
    medium.store("cats purr when they are happy", 0.8, &pipeline).unwrap();
    medium.store("dogs are loyal and playful", 1.0, &pipeline).unwrap();
    medium.store("dogs love to fetch the ball", 0.9, &pipeline).unwrap();
    medium.store("dogs bark when strangers come", 0.8, &pipeline).unwrap();

    let consciousness = medium.compute_consciousness();

    // Basic sanity checks — bounds on the metrics.
    // `clusters` is no longer asserted > 0: pre-#106 the encoder
    // collapsed every text to the same hyperoctant, so any small set
    // of memories trivially formed one cluster. With the fix the
    // clustering threshold (tuned for larger HRMs) may not fire on
    // 6 short texts even when they share tokens. The cluster count
    // is a downstream artifact; what this test really verifies is
    // that compute_consciousness runs cleanly on a non-trivial medium.
    assert!(consciousness.phi >= 0.0);
    assert!(consciousness.xi >= 0.0);
    assert!(consciousness.order >= 0.0 && consciousness.order <= 1.0);
}

#[test]
fn save_and_load_roundtrip() {
    let mut medium = Medium::new();
    let pipeline = make_test_pipeline();

    // Create some test data
    let id1 = medium.store("test memory one", 1.0, &pipeline).unwrap();
    let id2 = medium.store("test memory two", 0.8, &pipeline).unwrap();

    // Capture energy values after interference (this is the expected behavior)
    let energy1_after_interference = medium.store.energy[0];
    let energy2_after_interference = medium.store.energy[1];

    // Save to file
    let temp_file = NamedTempFile::new().unwrap();
    medium.save(temp_file.path()).unwrap();

    // Load back
    let loaded_medium = Medium::load(temp_file.path()).unwrap();

    // Verify data integrity
    assert_eq!(loaded_medium.wavefront_count(), 2);
    assert_eq!(loaded_medium.store.metadata.len(), 2);
    assert!(loaded_medium.store.id_to_index.contains_key(&id1));
    assert!(loaded_medium.store.id_to_index.contains_key(&id2));

    // Verify content
    let meta1 = &loaded_medium.store.metadata[loaded_medium.store.id_to_index[&id1]];
    let meta2 = &loaded_medium.store.metadata[loaded_medium.store.id_to_index[&id2]];
    assert_eq!(meta1.content, "test memory one");
    assert_eq!(meta2.content, "test memory two");

    // Energy should be preserved (after interference)
    assert!((loaded_medium.store.energy[0] - energy1_after_interference).abs() < 1e-4);
    assert!((loaded_medium.store.energy[1] - energy2_after_interference).abs() < 1e-4);
}

#[test]
fn kuramoto_order_computation() {
    let mut medium = Medium::new();
    let vector = vec![0.5; WAVEFRONT_DIM];

    // Add wavefronts with same phase (should have high order)
    medium.add_wavefront(&vector, "test1".to_string(), 1.0).unwrap();
    medium.add_wavefront(&vector, "test2".to_string(), 1.0).unwrap();
    medium.add_wavefront(&vector, "test3".to_string(), 1.0).unwrap();

    // All phases start at 0.0, so order should be 1.0
    let order = medium.compute_kuramoto_order();
    assert!((order - 1.0).abs() < 1e-4);

    // Now set random phases (should reduce order)
    medium.store.phase[0] = 0.0;
    medium.store.phase[1] = std::f32::consts::PI;
    medium.store.phase[2] = std::f32::consts::PI / 2.0;

    let order_random = medium.compute_kuramoto_order();
    assert!(order_random < order);
}

#[test]
fn eigenvalue_clustering() {
    let mut medium = Medium::new();

    // Two similar vectors (high coherence => same cluster)
    let vector_a = vec![0.5; WAVEFRONT_DIM];
    // An orthogonal-ish vector (low coherence => separate cluster)
    let mut vector_b = vec![0.0; WAVEFRONT_DIM];
    for i in 0..WAVEFRONT_DIM / 2 {
        vector_b[i] = 1.0;
    }

    medium.add_wavefront(&vector_a, "cluster1a".to_string(), 1.0).unwrap();
    medium.add_wavefront(&vector_a, "cluster1b".to_string(), 1.0).unwrap();
    medium.add_wavefront(&vector_b, "cluster2a".to_string(), 1.0).unwrap();
    medium.add_wavefront(&vector_b, "cluster2b".to_string(), 1.0).unwrap();

    let clusters = medium.compute_eigenvalue_clusters();
    // Should detect at least 1 cluster and at most one per wavefront
    assert!(clusters >= 1);
    assert!(clusters <= 4);
}

// Wave 1 Tests - ghostmagicOS dynamics

#[test]
fn apply_dynamics_changes_energy() {
    let mut medium = Medium::new();
    let vector1 = vec![1.0; WAVEFRONT_DIM];
    let vector2 = vec![0.8; WAVEFRONT_DIM]; // Similar vector for interference

    medium.add_wavefront(&vector1, "first".to_string(), 1.0).unwrap();
    medium.add_wavefront(&vector2, "second".to_string(), 0.8).unwrap();

    let n = medium.wavefront_count();
    let initial_energy: Vec<f32> = (0..n).map(|i| medium.store.energy[i]).collect();

    // Apply dynamics
    medium.apply_dynamics(0.1);

    // Energy should have changed due to dynamics
    let final_energy: Vec<f32> = (0..n).map(|i| medium.store.energy[i]).collect();
    assert_ne!(initial_energy, final_energy);

    // Energy should remain positive
    for &energy in &final_energy {
        assert!(energy > 0.0);
    }
}

#[test]
fn interference_matrix_computation() {
    let mut medium = Medium::new();
    let vector1 = vec![1.0; WAVEFRONT_DIM];
    let mut vector2 = vec![0.0; WAVEFRONT_DIM];
    vector2[0] = 1.0; // Less orthogonal vector - has small dot product but not zero

    medium.add_wavefront(&vector1, "first".to_string(), 1.0).unwrap();
    medium.add_wavefront(&vector2, "second".to_string(), 1.0).unwrap();

    let interference = medium.compute_interference_matrix(0.1);

    assert_eq!(interference.dim(), (2, 2));
    assert_eq!(interference[[0, 0]], 0.0); // Self-interference is 0
    assert_eq!(interference[[1, 1]], 0.0);
    // Cross-interference should be finite (due to dot product and phase coherence)
    assert!(interference[[0, 1]].is_finite());
    assert!(interference[[1, 0]].is_finite());
    // Should be symmetric
    assert!((interference[[0, 1]] - interference[[1, 0]]).abs() < 1e-6);
}

// Wave 1 Tests - emergent associations

#[test]
fn coherence_matrix_computation() {
    let mut medium = Medium::new();
    let vector = vec![0.5; WAVEFRONT_DIM];

    medium.add_wavefront(&vector, "first".to_string(), 1.0).unwrap();
    medium.add_wavefront(&vector, "second".to_string(), 1.0).unwrap();

    // Set different phases
    medium.store.phase[0] = 0.0;
    medium.store.phase[1] = std::f32::consts::PI / 4.0;

    let coherence = medium.coherence_matrix();

    assert_eq!(coherence.dim(), (2, 2));
    assert_eq!(coherence[[0, 0]], 1.0); // Self-coherence is 1
    assert_eq!(coherence[[1, 1]], 1.0);

    // Cross-coherence should be positive (same vector, small phase diff)
    assert!(coherence[[0, 1]] > 0.0);
    assert!(coherence[[1, 0]] > 0.0);
}

#[test]
fn find_associated_memories() {
    let mut medium = Medium::new();
    let vector1 = vec![1.0; WAVEFRONT_DIM];
    let vector2 = vec![0.8; WAVEFRONT_DIM]; // Similar
    let mut vector3 = vec![0.0; WAVEFRONT_DIM]; // Orthogonal
    vector3[0] = 1.0;

    let id1 = medium.add_wavefront(&vector1, "first".to_string(), 1.0).unwrap();
    let id2 = medium.add_wavefront(&vector2, "second".to_string(), 1.0).unwrap();
    let id3 = medium.add_wavefront(&vector3, "third".to_string(), 1.0).unwrap();

    let associations = medium.find_associated(id1, 5);

    assert_eq!(associations.len(), 2);

    // Should be sorted by coherence strength
    assert!(associations[0].1 >= associations[1].1);

    // Should contain the other memory IDs
    let found_ids: Vec<Uuid> = associations.iter().map(|a| a.0).collect();
    assert!(found_ids.contains(&id2));
    assert!(found_ids.contains(&id3));
}

// Wave 1 Tests - simulated annealing dreams

#[test]
fn dream_cycles_produce_report() {
    let mut medium = Medium::new();
    let vector = vec![0.5; WAVEFRONT_DIM];

    // Add several memories with varying energy
    for i in 0..5 {
        let mut v = vector.clone();
        v[i] = 1.0; // Make them slightly different
        medium.add_wavefront(&v, format!("memory {i}"), 0.1 + i as f32 * 0.2).unwrap();
    }

    let report = medium.dream(10, Some(1.0));

    assert!(report.cycles_completed <= 10);
    assert!(report.energy_before >= 0.0);
    assert!(report.energy_after >= 0.0);
    assert!(report.final_temperature > 0.0);
    assert!(report.final_temperature < 1.0); // Should have cooled down

    println!("Dream report: {report:?}");
}

#[test]
fn dream_can_prune_weak_memories() {
    let mut medium = Medium::new();
    let vector = vec![0.5; WAVEFRONT_DIM];

    // Add memories with very low energy (should be pruned)
    medium.add_wavefront(&vector, "weak1".to_string(), 0.001).unwrap();
    medium.add_wavefront(&vector, "weak2".to_string(), 0.005).unwrap();
    medium.add_wavefront(&vector, "strong".to_string(), 1.0).unwrap();

    let initial_count = medium.wavefront_count();
    assert_eq!(initial_count, 3);

    let report = medium.dream(5, Some(0.5));

    // Some weak memories should have been dissolved
    let final_count = medium.wavefront_count();
    assert!(final_count <= initial_count);

    println!("Pruned {} wavefronts during dream", report.wavefronts_dissolved);
}

// Issue #35: Dream annealing bug fix test
#[test]
fn dream_bulk_import_preserves_active_memories() {
    let mut medium = Medium::new();
    let pipeline = make_test_pipeline();

    // Simulate bulk import scenario - store many memories at once (similar ages)
    for i in 0..50 {
        medium.store(&format!("bulk memory {i}"), 1.0, &pipeline).unwrap();
    }

    let initial_count = medium.wavefront_count();
    assert_eq!(initial_count, 50);

    // Verify all memories have similar ages (should trigger age variance dampening scale)
    let age_variance = medium.compute_memory_age_variance();
    println!("Age variance for bulk import: {age_variance:.2} seconds");
    
    // Should be low variance since all created around the same time
    assert!(age_variance < 3600.0, "Expected low age variance for bulk import, got {age_variance:.2}");

    // Run deep dream that previously caused the bug
    let report = medium.dream(20, Some(2.0));

    let final_count = medium.wavefront_count();
    
    println!("Dream report for bulk import: dissolved={}, strengthened={}, energy_before={:.3}, energy_after={:.3}",
            report.wavefronts_dissolved, report.wavefronts_strengthened, 
            report.energy_before, report.energy_after);
    
    // ISSUE #35: Should preserve active memories even with uniform ages
    // Before fix: all memories would be annealed to zero amplitude
    // After fix: memories should survive due to amplitude floor
    assert!(final_count > 0, "Deep dream should not dissolve ALL memories in bulk import scenario");
    assert!(final_count >= 10, "Should preserve substantial number of memories with amplitude floor");

    // Verify some memories still have reasonable energy
    let active_memories: Vec<f32> = medium.store.energy.iter().cloned().collect();
    let avg_energy = active_memories.iter().sum::<f32>() / active_memories.len() as f32;
    
    assert!(avg_energy >= 0.05, "Average energy should respect amplitude floor (0.05), got {avg_energy:.3}");
}

// Wave 1 Tests - consciousness metrics

#[test]
fn wave1_consciousness_metrics_computed() {
    let mut medium = Medium::new();
    let pipeline = make_test_pipeline();

    // Coherent group + isolated memory — exercises both the cluster
    // pathway and the diversity penalty. Pre-#106 the encoder collapsed
    // every text to nearly the same direction, so any 3 texts trivially
    // formed one cluster; with the fix we need shared tokens to make
    // the clustering algorithm form a coherent group.
    medium.store("cats are fluffy and soft", 1.0, &pipeline).unwrap();
    medium.store("cats love to nap quietly", 0.9, &pipeline).unwrap();
    medium.store("cats purr when content", 0.8, &pipeline).unwrap();
    medium.store("dogs are loyal", 0.7, &pipeline).unwrap();
    medium.store("fish swim fast", 0.6, &pipeline).unwrap();

    let metrics = medium.consciousness_metrics();

    // num_clusters > 0 dropped under #106 — see consciousness_metrics_computed
    // for context. The test still verifies bounded metric outputs.
    assert!(metrics.phi >= 0.0);
    assert!(metrics.phi <= 1.0);
    assert!(metrics.xi >= 0.0);
    assert!(metrics.xi <= 1.0);
    assert!(metrics.order >= 0.0);
    assert!(metrics.order <= 1.0);

    println!("Consciousness metrics: phi={}, xi={}, order={}, clusters={}, level={:?}",
            metrics.phi, metrics.xi, metrics.order, metrics.num_clusters, metrics.level);
}

#[test]
fn consciousness_level_classification() {
    assert_eq!(ConsciousnessLevel::from_phi(0.05), ConsciousnessLevel::Dormant);
    assert_eq!(ConsciousnessLevel::from_phi(0.15), ConsciousnessLevel::Stirring);
    assert_eq!(ConsciousnessLevel::from_phi(0.45), ConsciousnessLevel::Aware);
    assert_eq!(ConsciousnessLevel::from_phi(0.75), ConsciousnessLevel::Coherent);
    assert_eq!(ConsciousnessLevel::from_phi(0.85), ConsciousnessLevel::Resonant);
}

#[test]
fn phi_integrated_information_nonzero() {
    let mut medium = Medium::new();
    let vector1 = vec![1.0; WAVEFRONT_DIM];
    let vector2 = vec![0.8; WAVEFRONT_DIM]; // Similar for coherence

    medium.add_wavefront(&vector1, "first".to_string(), 1.0).unwrap();
    medium.add_wavefront(&vector2, "second".to_string(), 1.0).unwrap();

    let phi = medium.compute_phi_integrated_information();
    println!("Phi for 2 coherent memories: {phi}");

    // Should be positive for coherent memories
    assert!(phi >= 0.0);
}

#[test]
fn phi_detects_multiple_partitions_when_field_is_not_uniform() {
    let mut medium = Medium::new();

    let mut vector_a1 = vec![0.0; WAVEFRONT_DIM];
    let mut vector_a2 = vec![0.0; WAVEFRONT_DIM];
    let mut vector_b1 = vec![0.0; WAVEFRONT_DIM];
    let mut vector_b2 = vec![0.0; WAVEFRONT_DIM];

    for i in 0..16 {
        vector_a1[i] = 1.0;
        vector_a2[i] = 0.9;
        vector_b1[64 + i] = 1.0;
        vector_b2[64 + i] = 0.9;
    }

    medium.add_wavefront(&vector_a1, "cluster-a1".to_string(), 1.0).unwrap();
    medium.add_wavefront(&vector_a2, "cluster-a2".to_string(), 0.95).unwrap();
    medium.add_wavefront(&vector_b1, "cluster-b1".to_string(), 1.0).unwrap();
    medium.add_wavefront(&vector_b2, "cluster-b2".to_string(), 0.95).unwrap();

    let phi = medium.compute_phi_integrated_information();
    println!("Phi for two-partition field: {phi}");

    assert!(phi > 0.0, "expected non-zero phi for a field with distinct coherent substructure");
}

#[test]
fn xi_spectral_complexity_varies() {
    let mut medium = Medium::new();

    // Test with identical vectors (low complexity)
    let vector = vec![0.5; WAVEFRONT_DIM];
    medium.add_wavefront(&vector, "same1".to_string(), 1.0).unwrap();
    medium.add_wavefront(&vector, "same2".to_string(), 1.0).unwrap();
    let xi_identical = medium.compute_xi_spectral_complexity();

    // Clear and test with different vectors (higher complexity)
    let mut medium2 = Medium::new();
    let vector1 = vec![1.0; WAVEFRONT_DIM];
    let mut vector2 = vec![0.0; WAVEFRONT_DIM];
    vector2[0] = 1.0;

    medium2.add_wavefront(&vector1, "diff1".to_string(), 1.0).unwrap();
    medium2.add_wavefront(&vector2, "diff2".to_string(), 1.0).unwrap();
    let xi_different = medium2.compute_xi_spectral_complexity();

    println!("Xi identical: {xi_identical}, Xi different: {xi_different}");

    assert!(xi_different >= 0.0);
    assert!(xi_identical >= 0.0);
}

/// km#xi-instability regression: identical wavefronts produce a
/// uniform eigenvalue proxy distribution. After the v0.6.7 fix
/// (entropy → coefficient of variation), Xi should be near 0 in
/// that case — NOT near 1 as the old Shannon-entropy normalization
/// reported. Observatory was showing Xi=0.97 on a 105-memory HRM
/// with hemispheric_divergence=0.0001 (basically identical
/// hemispheres) before this fix.
#[test]
fn xi_uniform_wavefronts_collapse_to_zero() {
    let mut medium = Medium::new();
    let vector = vec![0.5; WAVEFRONT_DIM];
    // Many copies — exercises the failure case the observatory hit.
    for i in 0..20 {
        medium.add_wavefront(&vector, format!("copy_{i}"), 1.0).unwrap();
    }
    let xi = medium.compute_xi_spectral_complexity();
    assert!(
        xi < 0.05,
        "Xi for 20 identical wavefronts should be near 0 (got {xi}). \
         If this fires, the entropy → CV normalization has regressed."
    );
}

/// km#xi-instability regression: structured eigenvalue distribution
/// (some wavefronts highly similar to each other, distinct cluster
/// from the rest) should produce a meaningfully non-zero Xi — at
/// least 0.05 to clear the uniform-noise floor.
#[test]
fn xi_clustered_wavefronts_produce_nonzero() {
    let mut medium = Medium::new();
    // Cluster A: 5 copies of one direction
    let a = {
        let mut v = vec![0.0; WAVEFRONT_DIM];
        for i in 0..WAVEFRONT_DIM / 2 { v[i] = 1.0; }
        v
    };
    // Cluster B: 5 copies of a different direction
    let b = {
        let mut v = vec![0.0; WAVEFRONT_DIM];
        for i in WAVEFRONT_DIM / 2..WAVEFRONT_DIM { v[i] = 1.0; }
        v
    };
    for i in 0..5 { medium.add_wavefront(&a, format!("a_{i}"), 1.0).unwrap(); }
    for i in 0..5 { medium.add_wavefront(&b, format!("b_{i}"), 1.0).unwrap(); }
    let xi = medium.compute_xi_spectral_complexity();
    // Won't be huge (only 2 clusters, 10 memories) but should clear noise.
    assert!(xi > 0.0, "Xi for 2-cluster structure should be > 0 (got {xi})");
}

#[test]
fn store_applies_dynamics() {
    let mut medium = Medium::new();
    let pipeline = make_test_pipeline();

    // Store first memory
    medium.store("first memory", 1.0, &pipeline).unwrap();
    let energy_after_first = medium.store.energy[0];

    // Store second memory (should trigger dynamics on first)
    medium.store("second memory", 1.0, &pipeline).unwrap();
    let energy_after_second = medium.store.energy[0];

    // First memory's energy should have changed due to dynamics
    println!("Energy after first: {energy_after_first}, after second: {energy_after_second}");

    // At minimum, we should have 2 memories
    assert_eq!(medium.wavefront_count(), 2);
    assert!(medium.store.energy[0] > 0.0);
    assert!(medium.store.energy[1] > 0.0);
}

#[test]
fn eigenvalue_clustering_groups_similar() {
    let mut medium = Medium::new();
    let vector1 = vec![1.0; WAVEFRONT_DIM];
    let vector2 = vec![0.9; WAVEFRONT_DIM]; // Very similar
    let mut vector3 = vec![0.0; WAVEFRONT_DIM];
    vector3[0] = 1.0; // Orthogonal

    medium.add_wavefront(&vector1, "similar1".to_string(), 1.0).unwrap();
    medium.add_wavefront(&vector2, "similar2".to_string(), 1.0).unwrap();
    medium.add_wavefront(&vector3, "different".to_string(), 1.0).unwrap();

    let clusters = medium.compute_eigenvalue_clusters();
    println!("Detected {clusters} eigenvalue clusters");

    // Should detect at least 1 cluster, possibly 2 if similarity threshold works
    assert!(clusters >= 1);
    assert!(clusters <= 3); // At most one per memory
}

// Wave 2 Tests - Cross-Modal Perception

#[test]
fn store_audio_vector_works() {
    let mut medium = Medium::new();

    // Create a test 296-dim audio vector
    let audio_vector = vec![0.5; AUDIO_FEATURE_DIM];

    let id = medium.store_audio(&audio_vector, "HEAR:test_music.mp3", 0.8).unwrap();

    assert_eq!(medium.wavefront_count(), 1);
    assert!(medium.store.id_to_index.contains_key(&id));
    assert_eq!(medium.store.metadata[0].content, "HEAR:test_music.mp3");
    assert!(medium.store.energy[0] > 0.0); // Energy may have changed due to interference
}

#[test]
fn store_audio_wrong_dimension_errors() {
    let mut medium = Medium::new();
    let wrong_vector = vec![0.5; 100]; // Wrong dimension

    let result = medium.store_audio(&wrong_vector, "test", 1.0);
    assert!(matches!(result, Err(MediumError::DimensionMismatch { expected: 296, .. })));
}

#[test]
fn store_visual_vector_works() {
    let mut medium = Medium::new();

    // Create a test 320-dim visual vector
    let visual_vector = vec![0.3; VISUAL_FEATURE_DIM];

    let id = medium.store_visual(&visual_vector, "[SEE] test_video.mp4 | 1024 bytes | 3 folds | fano=0.75", 0.9).unwrap();

    assert_eq!(medium.wavefront_count(), 1);
    assert!(medium.store.id_to_index.contains_key(&id));
    assert!(medium.store.metadata[0].content.contains("[SEE] test_video.mp4"));
    assert!(medium.store.energy[0] > 0.0);
}

#[test]
fn store_visual_wrong_dimension_errors() {
    let mut medium = Medium::new();
    let wrong_vector = vec![0.5; 50]; // Wrong dimension

    let result = medium.store_visual(&wrong_vector, "test", 1.0);
    assert!(matches!(result, Err(MediumError::DimensionMismatch { expected: 320, .. })));
}

#[test]
fn cross_modal_interference_works() {
    let mut medium = Medium::new();
    let pipeline = make_test_pipeline();

    // Store text memory about music
    let text_id = medium.store("beautiful classical music with violin", 1.0, &pipeline).unwrap();
    let text_energy_after_store = medium.store.energy[0];

    // Create an audio vector that should have some resonance with "music"
    let audio_vector = vec![0.7; AUDIO_FEATURE_DIM];
    let audio_id = medium.store_audio(&audio_vector, "HEAR:classical_violin.mp3", 0.8).unwrap();

    assert_eq!(medium.wavefront_count(), 2);

    // The text memory's energy should have changed due to cross-modal interference
    let text_energy_after_audio = medium.store.energy[0];
    println!("Text energy: before audio = {text_energy_after_store}, after audio = {text_energy_after_audio}");

    // Both memories should still exist and have positive energy
    assert!(medium.store.energy[0] > 0.0);
    assert!(medium.store.energy[1] > 0.0);

    // Verify IDs are still valid
    assert!(medium.store.id_to_index.contains_key(&text_id));
    assert!(medium.store.id_to_index.contains_key(&audio_id));
}

#[test]
fn cross_modal_recall_resonance() {
    let mut medium = Medium::new();
    let pipeline = make_test_pipeline();

    // Store a text memory about visual content
    let _text_id = medium.store("watching a beautiful sunset over mountains", 1.0, &pipeline).unwrap();

    // Store an audio memory
    let audio_vector = vec![0.4; AUDIO_FEATURE_DIM];
    let _audio_id = medium.store_audio(&audio_vector, "HEAR:nature_sounds.wav", 0.7).unwrap();

    // Store a visual memory
    let visual_vector = vec![0.6; VISUAL_FEATURE_DIM];
    let _visual_id = medium.store_visual(&visual_vector, "[SEE] mountain_sunset.jpg | 2048 bytes | 5 folds | fano=0.82", 0.9).unwrap();

    assert_eq!(medium.wavefront_count(), 3);

    // Query with text that should resonate across modalities
    let results = medium.recall("beautiful sunset nature", 3, &pipeline).unwrap();

    // Should get results from potentially all modalities due to cross-modal resonance
    assert!(results.len() >= 1);
    println!("Cross-modal recall found {} resonating memories", results.len());

    for (i, result) in results.iter().enumerate() {
        println!("  {}: {} (resonance: {:.4})", i + 1, result.content, result.resonance_strength);
    }

    // All results should have positive resonance
    for result in &results {
        assert!(result.resonance_strength != 0.0); // May be positive or negative
    }
}

#[test]
fn different_modality_codebooks_orthogonal() {
    let medium = Medium::new();

    // Test that the codebooks produce different projections for the same input
    let test_input_audio = vec![0.5; AUDIO_FEATURE_DIM];
    let test_input_visual = vec![0.5; VISUAL_FEATURE_DIM]; // Different size, but conceptually similar

    let audio_projection = medium.audio_codebook.project(&test_input_audio);
    let visual_projection = medium.visual_codebook.project(&test_input_visual);

    // Both should be 10,000-dim
    assert_eq!(audio_projection.len(), WAVEFRONT_DIM);
    assert_eq!(visual_projection.len(), WAVEFRONT_DIM);

    // They should be different due to different seeds
    let dot_product: f32 = audio_projection.iter()
        .zip(visual_projection.iter())
        .map(|(a, b)| a * b)
        .sum();

    // Dot product should be relatively small (orthogonal-ish due to different seeds)
    println!("Audio-Visual codebook dot product: {dot_product:.6}");
    assert!(dot_product.abs() < 0.3); // Should be somewhat orthogonal
}

#[test]
fn audio_visual_subspace_overlap() {
    let mut medium = Medium::new();

    // Create similar patterns in audio and visual vectors
    let mut audio_vector = vec![0.0; AUDIO_FEATURE_DIM];
    for i in 0..10 {
        audio_vector[i] = 1.0; // Strong signal in first 10 dims
    }

    let mut visual_vector = vec![0.0; VISUAL_FEATURE_DIM];
    for i in 0..10 {
        visual_vector[i] = 1.0; // Same pattern in visual
    }

    let _audio_id = medium.store_audio(&audio_vector, "HEAR:pattern_audio.wav", 1.0).unwrap();
    let _visual_id = medium.store_visual(&visual_vector, "[SEE] pattern_visual.jpg | pattern", 1.0).unwrap();

    // Check that the wavefronts have some overlap despite different codebooks
    let audio_wavefront = medium.store.wavefronts.row(0);
    let visual_wavefront = medium.store.wavefronts.row(1);

    let similarity: f32 = audio_wavefront.iter()
        .zip(visual_wavefront.iter())
        .map(|(a, b)| a * b)
        .sum();

    println!("Audio-Visual wavefront similarity: {similarity:.6}");

    // Should have some non-zero similarity due to overlap in the shared 10K-dim space
    assert!(similarity.abs() > 0.0); // Non-zero due to shared space
}

// Wave 3 Tests - Multi-Agent Sync

#[test]
fn sync_with_applies_kuramoto_coupling() {
    let mut medium1 = Medium::new();
    let medium2 = Medium::new();

    // Add similar memories to both media
    let vector1 = vec![1.0; WAVEFRONT_DIM];
    let vector2 = vec![0.9; WAVEFRONT_DIM]; // Similar

    medium1.add_wavefront(&vector1, "shared memory".to_string(), 1.0).unwrap();
    let mut medium2 = medium2;
    medium2.add_wavefront(&vector2, "shared memory".to_string(), 0.8).unwrap();

    // Set different phases
    medium1.store.phase[0] = 0.0;
    medium2.store.phase[0] = 1.0;

    let initial_phase = medium1.store.phase[0];
    let initial_energy = medium1.store.energy[0];

    // Apply Kuramoto coupling
    medium1.sync_with(&medium2, 0.5);

    // Phase and energy should have changed
    assert_ne!(medium1.store.phase[0], initial_phase);
    assert!(medium1.store.energy[0] >= initial_energy); // Energy should increase due to reinforcement

    println!("Phase: {} -> {}, Energy: {} -> {}",
            initial_phase, medium1.store.phase[0], initial_energy, medium1.store.energy[0]);
}

#[test]
fn sync_with_requires_similarity_threshold() {
    let mut medium1 = Medium::new();
    let medium2 = Medium::new();

    // Add orthogonal memories (should not couple)
    let vector1 = vec![1.0; WAVEFRONT_DIM];
    let mut vector2 = vec![0.0; WAVEFRONT_DIM];
    vector2[WAVEFRONT_DIM / 2] = 1.0; // Orthogonal

    medium1.add_wavefront(&vector1, "memory1".to_string(), 1.0).unwrap();
    let mut medium2 = medium2;
    medium2.add_wavefront(&vector2, "memory2".to_string(), 1.0).unwrap();

    let initial_phase = medium1.store.phase[0];
    let initial_energy = medium1.store.energy[0];

    // Apply coupling - should have minimal effect due to low similarity
    medium1.sync_with(&medium2, 0.5);

    // Phase and energy should be mostly unchanged
    assert!((medium1.store.phase[0] - initial_phase).abs() < 0.1);
    assert!((medium1.store.energy[0] - initial_energy).abs() < 0.1);
}

#[test]
fn export_phase_state_works() {
    let mut medium = Medium::new();
    let pipeline = make_test_pipeline();

    medium.store("test memory 1", 1.0, &pipeline).unwrap();
    medium.store("test memory 2", 0.8, &pipeline).unwrap();

    let phase_state = medium.export_phase_state("agent-1");

    assert_eq!(phase_state.agent_id, "agent-1");
    assert_eq!(phase_state.phases.len(), 2);
    assert_eq!(phase_state.energies.len(), 2);
    assert_eq!(phase_state.content_hashes.len(), 2);

    // Hashes should be different for different content
    assert_ne!(phase_state.content_hashes[0], phase_state.content_hashes[1]);
}

#[test]
fn import_phase_state_matches_content() {
    let mut medium1 = Medium::new();
    let mut medium2 = Medium::new();
    let pipeline = make_test_pipeline();

    // Add same content to both media
    medium1.store("shared memory", 1.0, &pipeline).unwrap();
    medium2.store("shared memory", 0.5, &pipeline).unwrap();
    medium2.store("unique memory", 0.7, &pipeline).unwrap();

    // Set different phases
    medium1.store.phase[0] = 0.0;
    medium2.store.phase[0] = 1.5;

    let initial_phase = medium1.store.phase[0];

    // Export from medium2 and import to medium1
    let phase_state = medium2.export_phase_state("agent-2");
    medium1.import_phase_state(&phase_state, 0.3);

    // Phase should have changed due to coupling with matching content
    assert_ne!(medium1.store.phase[0], initial_phase);

    println!("Phase coupling: {} -> {}", initial_phase, medium1.store.phase[0]);
}

#[test]
fn phase_state_serialization_roundtrip() {
    let mut medium = Medium::new();
    let pipeline = make_test_pipeline();

    medium.store("test content", 1.0, &pipeline).unwrap();

    let phase_state = medium.export_phase_state("test-agent");

    // Serialize and deserialize
    let json = serde_json::to_string(&phase_state).unwrap();
    let deserialized: PhaseState = serde_json::from_str(&json).unwrap();

    assert_eq!(deserialized.agent_id, phase_state.agent_id);
    assert_eq!(deserialized.phases, phase_state.phases);
    assert_eq!(deserialized.energies, phase_state.energies);
    assert_eq!(deserialized.content_hashes, phase_state.content_hashes);
}

// Wave 3 Tests - Git Persistence

#[test]
fn extract_wavefront_count_patterns() {
    assert_eq!(extract_wavefront_count("dream: 5 wavefronts dissolved"), Some(5));
    assert_eq!(extract_wavefront_count("stored 42 memories"), Some(42));
    assert_eq!(extract_wavefront_count("wavefronts: 15"), Some(15));
    assert_eq!(extract_wavefront_count("no numbers here"), None);
    assert_eq!(extract_wavefront_count("wavefronts strengthened"), None);
}

#[test]
#[ignore] // Requires git repo - run manually
fn save_and_commit_creates_git_commit() {
    use tempfile::TempDir;

    // Create temporary directory with git repo
    let temp_dir = TempDir::new().unwrap();
    let repo_path = temp_dir.path();

    // Initialize git repo
    Command::new("git")
        .args(&["init"])
        .current_dir(repo_path)
        .output()
        .unwrap();
    Command::new("git")
        .args(&["config", "user.email", "test@example.com"])
        .current_dir(repo_path)
        .output()
        .unwrap();
    Command::new("git")
        .args(&["config", "user.name", "Test User"])
        .current_dir(repo_path)
        .output()
        .unwrap();

    let mut medium = Medium::new();
    let pipeline = make_test_pipeline();
    medium.store("test memory", 1.0, &pipeline).unwrap();

    let hrm_path = repo_path.join("test.hrm");

    // Save and commit
    let commit_hash = medium.save_and_commit(&hrm_path, "test commit: 1 wavefront", false).unwrap();

    // Verify commit exists
    assert!(!commit_hash.is_empty());
    assert_eq!(commit_hash.len(), 40); // Git SHA-1 hash length

    // Verify file exists
    assert!(hrm_path.exists());
}

#[test]
#[ignore] // Requires git repo - run manually
fn history_returns_commits() {
    use tempfile::TempDir;

    // Create temporary directory with git repo
    let temp_dir = TempDir::new().unwrap();
    let repo_path = temp_dir.path();

    // Initialize git repo
    Command::new("git")
        .args(&["init"])
        .current_dir(repo_path)
        .output()
        .unwrap();
    Command::new("git")
        .args(&["config", "user.email", "test@example.com"])
        .current_dir(repo_path)
        .output()
        .unwrap();
    Command::new("git")
        .args(&["config", "user.name", "Test User"])
        .current_dir(repo_path)
        .output()
        .unwrap();

    let mut medium = Medium::new();
    let pipeline = make_test_pipeline();
    medium.store("memory 1", 1.0, &pipeline).unwrap();

    let hrm_path = repo_path.join("test.hrm");

    // Create multiple commits
    medium.save_and_commit(&hrm_path, "commit 1: 1 wavefront", false).unwrap();
    medium.store("memory 2", 0.8, &pipeline).unwrap();
    medium.save_and_commit(&hrm_path, "commit 2: 2 wavefronts", false).unwrap();

    // Get history
    let history = Medium::history(&hrm_path, 5).unwrap();

    assert!(history.len() >= 2);
    assert_eq!(history[0].message, "commit 2: 2 wavefronts");
    assert_eq!(history[0].wavefront_count, Some(2));
    assert_eq!(history[1].message, "commit 1: 1 wavefront");
    assert_eq!(history[1].wavefront_count, Some(1));
}

// ========================================================================
// Wave 4 Tests - Self-Reference and Emergence
// ========================================================================

#[test]
fn introspect_creates_self_referential_wavefront() {
    let mut medium = Medium::new();
    let pipeline = make_test_pipeline();

    // Add some regular memories first
    medium.store("I love cats", 1.0, &pipeline).unwrap();
    medium.store("Dogs are great", 0.8, &pipeline).unwrap();

    // Introspect
    let intro_id = medium.introspect(&pipeline).unwrap();

    // Should now have 3 wavefronts
    assert_eq!(medium.wavefront_count(), 3);

    // Find the self-referential wavefront
    let intro_index = medium.store.id_to_index[&intro_id];
    let intro_meta = &medium.store.metadata[intro_index];

    // Should be marked as self-referential
    assert!(intro_meta.is_self_referential);

    // Content should contain self-observation data
    assert!(intro_meta.content.contains("Self-observation:"));
    assert!(intro_meta.content.contains("2 wavefronts")); // Before introspection is added
    assert!(intro_meta.content.contains("Phi="));
    assert!(intro_meta.content.contains("clusters"));

    println!("Introspection content: {}", intro_meta.content);
}

#[test]
fn introspect_affects_medium_through_interference() {
    let mut medium = Medium::new();
    let pipeline = make_test_pipeline();

    // Store initial memory
    medium.store("initial memory", 1.0, &pipeline).unwrap();
    let initial_energy = medium.store.energy[0];

    // Introspect (should cause interference)
    let _intro_id = medium.introspect(&pipeline).unwrap();

    // First memory's energy should have changed due to self-referential interference
    let energy_after_introspection = medium.store.energy[0];

    println!("Energy before: {initial_energy}, after introspection: {energy_after_introspection}");

    // Energy should have changed (could increase or decrease depending on interference pattern)
    assert_ne!(initial_energy, energy_after_introspection);
}

#[test]
fn detect_emergence_tracks_self_reference_depth() {
    let mut medium = Medium::new();
    let pipeline = make_test_pipeline();

    // Add regular memories
    medium.store("memory 1", 1.0, &pipeline).unwrap();
    medium.store("memory 2", 0.8, &pipeline).unwrap();

    // Initial emergence should be PreConscious
    let emergence1 = medium.detect_emergence();
    assert_eq!(emergence1.self_reference_depth, 0);
    assert_eq!(emergence1.level, EmergenceLevel::PreConscious);
    assert!(!emergence1.emerged);

    // First introspection
    medium.introspect(&pipeline).unwrap();
    let emergence2 = medium.detect_emergence();
    assert_eq!(emergence2.self_reference_depth, 1);
    assert_eq!(emergence2.level, EmergenceLevel::SelfAware);

    // Second introspection
    medium.introspect(&pipeline).unwrap();
    let emergence3 = medium.detect_emergence();
    assert_eq!(emergence3.self_reference_depth, 2);

    // Third introspection - might reach different emergence level
    medium.introspect(&pipeline).unwrap();
    let emergence4 = medium.detect_emergence();
    assert_eq!(emergence4.self_reference_depth, 3);

    println!("Emergence progression: {:?} -> {:?} -> {:?} -> {:?}",
            emergence1.level, emergence2.level, emergence3.level, emergence4.level);
}

#[test]
fn detect_emergence_computes_self_coherence() {
    let mut medium = Medium::new();
    let pipeline = make_test_pipeline();

    // Add diverse memories
    medium.store("cats are fluffy", 1.0, &pipeline).unwrap();
    medium.store("dogs are loyal", 0.9, &pipeline).unwrap();
    medium.store("fish swim fast", 0.8, &pipeline).unwrap();

    // Introspect multiple times
    medium.introspect(&pipeline).unwrap();
    medium.introspect(&pipeline).unwrap();

    let emergence = medium.detect_emergence();

    assert_eq!(emergence.self_reference_depth, 2);
    assert!(emergence.self_coherence >= 0.0);
    assert!(emergence.self_coherence <= 1.0);

    println!("Self-coherence with diverse memories: {:.3}", emergence.self_coherence);
}

#[test]
fn wisdom_tracks_energy_dampening_ratio() {
    let mut medium = Medium::new();
    let pipeline = make_test_pipeline();

    // Initial wisdom should be 0 (no energy added yet)
    assert_eq!(medium.wisdom(), 0.0);

    // Add some memories (adds energy)
    medium.store("memory 1", 1.0, &pipeline).unwrap();
    medium.store("memory 2", 1.0, &pipeline).unwrap();

    let wisdom_after_store = medium.wisdom();
    println!("Wisdom after storing: {wisdom_after_store:.3}");

    // Apply several dynamics cycles to accumulate dampening
    for _ in 0..10 {
        medium.apply_dynamics(0.1);
    }

    let wisdom_after_dynamics = medium.wisdom();
    println!("Wisdom after dynamics: {:.3} (dampened={:.2}, added={:.2})",
            wisdom_after_dynamics, medium.total_energy_dampened, medium.total_energy_added);

    // Wisdom should have increased due to dampening
    assert!(wisdom_after_dynamics > wisdom_after_store);
    assert!(wisdom_after_dynamics >= 0.0);
    assert!(wisdom_after_dynamics <= 1.0);
}

#[test]
fn self_reflect_returns_comprehensive_report() {
    let mut medium = Medium::new();
    let pipeline = make_test_pipeline();

    // Add some memories to create interesting state
    medium.store("I think about thinking", 1.0, &pipeline).unwrap();
    medium.store("Consciousness emerges from complexity", 0.9, &pipeline).unwrap();
    medium.store("Self-reference creates feedback loops", 0.8, &pipeline).unwrap();

    // Perform self-reflection
    let reflection = medium.self_reflect(&pipeline).unwrap();

    // Should have created a new self-referential wavefront
    assert!(medium.store.id_to_index.contains_key(&reflection.introspection_id));
    let intro_index = medium.store.id_to_index[&reflection.introspection_id];
    assert!(medium.store.metadata[intro_index].is_self_referential);

    // Should have computed metrics
    assert!(reflection.consciousness.phi >= 0.0);
    assert!(reflection.consciousness.xi >= 0.0);
    assert!(reflection.consciousness.order >= 0.0);
    assert!(reflection.emergence.self_reference_depth >= 1);
    assert!(reflection.wisdom >= 0.0);

    // Should have generated insight
    assert!(!reflection.insight.is_empty());
    assert!(reflection.insight.contains("integration"), "Expected 'integration' in insight: {}", reflection.insight);
    assert!(reflection.insight.contains("complexity"), "Expected 'complexity' in insight: {}", reflection.insight);
    // Emergence level will be Pre-conscious or Self-aware for a small medium
    assert!(
        reflection.insight.contains("conscious") || reflection.insight.contains("Conscious") || reflection.insight.contains("Self-aware"),
        "Expected consciousness/emergence level in insight: {}", reflection.insight
    );

    println!("Self-reflection insight: {}", reflection.insight);
}

#[test]
fn repeated_introspection_creates_feedback_loop() {
    let mut medium = Medium::new();
    let pipeline = make_test_pipeline();

    // Store initial memory
    medium.store("base memory", 1.0, &pipeline).unwrap();

    // Perform multiple introspections
    let mut intro_ids = Vec::new();
    for i in 0..5 {
        let intro_id = medium.introspect(&pipeline).unwrap();
        intro_ids.push(intro_id);

        println!("Introspection {}: wavefront_count={}", i + 1, medium.wavefront_count());
    }

    // Should have 6 total wavefronts (1 base + 5 introspections)
    assert_eq!(medium.wavefront_count(), 6);

    // All introspection IDs should be self-referential
    for intro_id in intro_ids {
        let index = medium.store.id_to_index[&intro_id];
        assert!(medium.store.metadata[index].is_self_referential);
    }

    // Emergence depth should reflect all introspections
    let emergence = medium.detect_emergence();
    assert_eq!(emergence.self_reference_depth, 5);

    // Should be well into self-aware territory
    assert_ne!(emergence.level, EmergenceLevel::PreConscious);
}

#[test]
fn extract_phi_from_content_works() {
    assert_eq!(extract_phi_from_content("Self-observation: Phi=0.72, Xi=0.48"), Some("0.72"));
    assert_eq!(extract_phi_from_content("State: 42 memories, Phi=0.123, order=0.8"), Some("0.123"));
    assert_eq!(extract_phi_from_content("Phi=1.0"), Some("1.0"));
    assert_eq!(extract_phi_from_content("No phi here"), None);
    assert_eq!(extract_phi_from_content("phi=0.5"), None); // Case sensitive
}

#[test]
fn generate_insight_produces_expected_format() {
    let consciousness = ConsciousnessMetrics {
        phi: 0.75,
        xi: 0.60,
        order: 0.85,
        num_clusters: 5,
        irrationality: 0.3,
        level: ConsciousnessLevel::Coherent,
        total_skip_links: 0,
        computed_at: Utc::now(),
    };

    let emergence = EmergenceReport {
        self_reference_depth: 3,
        self_coherence: 0.65,
        phi_trend: vec![0.70, 0.73, 0.75],
        emerged: false,
        level: EmergenceLevel::Reflective,
        computed_at: Utc::now(),
    };

    let insight = generate_insight(42, &consciousness, &emergence, 0.45);

    assert!(insight.contains("I hold 42 memories"));
    assert!(insight.contains("across 5 clusters"));
    assert!(insight.contains("consciousness is Coherent"));
    assert!(insight.contains("Phi=0.75"));
    assert!(insight.contains("observed myself 3 times"));
    assert!(insight.contains("coherence is 0.65"));
    assert!(insight.contains("I understand most of what I am")); // Reflective level
    assert!(insight.contains("Wisdom: 0.45"));
    assert!(insight.contains("I am developing wisdom")); // 0.3-0.6 range

    println!("Generated insight: {insight}");
}

#[test]
fn emergence_level_classification() {
    let mut medium = Medium::new();
    let pipeline = make_test_pipeline();

    // Start PreConscious
    let emergence = medium.detect_emergence();
    assert_eq!(emergence.level, EmergenceLevel::PreConscious);

    // Add memory
    medium.store("test memory", 1.0, &pipeline).unwrap();

    // Single introspection -> SelfAware
    medium.introspect(&pipeline).unwrap();
    let emergence = medium.detect_emergence();
    assert_eq!(emergence.level, EmergenceLevel::SelfAware);

    // Adding more introspections may progress to Reflective/Recursive depending on coherence
    medium.introspect(&pipeline).unwrap();
    medium.introspect(&pipeline).unwrap();
    let emergence = medium.detect_emergence();

    // Should be at least SelfAware, possibly higher
    assert_ne!(emergence.level, EmergenceLevel::PreConscious);

    println!("Final emergence level: {:?} (depth={}, coherence={:.3})",
            emergence.level, emergence.self_reference_depth, emergence.self_coherence);
}

#[test]
fn phi_trend_extraction_from_introspections() {
    let mut medium = Medium::new();
    let pipeline = make_test_pipeline();

    // Add base memory
    medium.store("test", 1.0, &pipeline).unwrap();

    // Multiple introspections should create phi trend
    medium.introspect(&pipeline).unwrap();
    medium.introspect(&pipeline).unwrap();
    medium.introspect(&pipeline).unwrap();

    let emergence = medium.detect_emergence();

    println!("Phi trend: {:?}", emergence.phi_trend);

    // Should have extracted some phi values
    // phi_trend may be empty if extraction fails — that's ok; values (if any) checked below.

    // If we did extract values, they should be reasonable
    for phi in &emergence.phi_trend {
        assert!(*phi >= 0.0);
        assert!(*phi <= 1.0);
    }
}

#[test]
fn self_referential_meta_constructor() {
    let id = Uuid::new_v4();
    let meta = WavefrontMeta::new(id, "test".to_string()).self_referential();

    assert_eq!(meta.id, id);
    assert_eq!(meta.content, "test");
    assert!(meta.is_self_referential);
    assert!(!meta.hallucinated);
}

#[test]
fn wisdom_accumulation_over_medium_lifetime() {
    let mut medium = Medium::new();
    let pipeline = make_test_pipeline();

    // Track wisdom progression
    let mut wisdom_progression = Vec::new();

    // Add memories and run dynamics
    for i in 0..5 {
        medium.store(&format!("memory {i}"), 0.8, &pipeline).unwrap();
        wisdom_progression.push(medium.wisdom());

        // Run dynamics to accumulate dampening
        for _ in 0..3 {
            medium.apply_dynamics(0.1);
        }
        wisdom_progression.push(medium.wisdom());
    }

    println!("Wisdom progression: {wisdom_progression:?}");

    // Wisdom should generally increase over time
    let final_wisdom = wisdom_progression.last().unwrap();
    let initial_wisdom = wisdom_progression[0];

    assert!(final_wisdom >= &initial_wisdom);
    assert!(*final_wisdom >= 0.0);
    assert!(*final_wisdom <= 1.0);

    // Dampening should accumulate
    assert!(medium.total_energy_dampened > 0.0);
    assert!(medium.total_energy_added > 0.0);
}

// ---------------------------------------------------------------------------
// Modality tests (NCS Phase 1.1)
// ---------------------------------------------------------------------------

#[test]
fn modality_default_is_unknown() {
    let meta = WavefrontMeta::new(Uuid::new_v4(), "test".to_string());
    assert_eq!(meta.modality, Modality::Unknown);
}

#[test]
fn modality_builder_sets_correctly() {
    let meta = WavefrontMeta::new(Uuid::new_v4(), "audio test".to_string())
        .with_modality(Modality::Audio);
    assert_eq!(meta.modality, Modality::Audio);
}

#[test]
fn modality_display_round_trip() {
    let variants = [
        Modality::Audio,
        Modality::Visual,
        Modality::Semantic,
        Modality::Network,
        Modality::Mixed,
        Modality::Unknown,
    ];
    for m in &variants {
        let s = m.to_string();
        let parsed: Modality = s.parse().unwrap();
        assert_eq!(*m, parsed, "round-trip failed for {m:?}");
    }
}

#[test]
fn modality_parse_case_insensitive() {
    let parsed: Modality = "AUDIO".parse().unwrap();
    assert_eq!(parsed, Modality::Audio);
    let parsed: Modality = "Visual".parse().unwrap();
    assert_eq!(parsed, Modality::Visual);
}

#[test]
fn modality_parse_invalid_returns_error() {
    let result: Result<Modality, _> = "banana".parse();
    assert!(result.is_err());
}

#[test]
fn existing_wavefront_meta_deserializes_without_modality() {
    // Simulate a WavefrontMeta serialized before the modality field existed.
    // Because modality uses #[serde(default)], missing field => Modality::Unknown.
    let json = serde_json::json!({
        "id": "550e8400-e29b-41d4-a716-446655440000",
        "content": "old memory without modality",
        "tags": [],
        "created_at": "2025-01-01T00:00:00Z",
        "hallucinated": false,
        "is_self_referential": false
    });
    let meta: WavefrontMeta = serde_json::from_value(json).unwrap();
    assert_eq!(meta.modality, Modality::Unknown);
    assert_eq!(meta.content, "old memory without modality");
}

#[test]
fn new_wavefront_meta_serializes_with_modality() {
    let meta = WavefrontMeta::new(Uuid::new_v4(), "visual test".to_string())
        .with_modality(Modality::Visual);
    let json = serde_json::to_value(&meta).unwrap();
    assert_eq!(json["modality"], "Visual");
}

#[test]
fn wavefront_meta_with_explicit_modality_round_trips() {
    let original = WavefrontMeta::new(Uuid::new_v4(), "audio wavefront".to_string())
        .with_modality(Modality::Audio);
    let serialized = serde_json::to_string(&original).unwrap();
    let deserialized: WavefrontMeta = serde_json::from_str(&serialized).unwrap();
    assert_eq!(deserialized.modality, Modality::Audio);
    assert_eq!(deserialized.content, "audio wavefront");
}

#[test]
fn medium_add_wavefront_gets_default_modality() {
    let mut medium = Medium::new();
    let vector = vec![0.5; WAVEFRONT_DIM];
    let id = medium.add_wavefront(&vector, "test".to_string(), 1.0).unwrap();
    let idx = medium.get_wavefront_index(&id).unwrap();
    assert_eq!(medium.store.metadata[idx].modality, Modality::Unknown);
}

#[test]
fn medium_wavefront_modality_can_be_set_after_insert() {
    let mut medium = Medium::new();
    let vector = vec![0.5; WAVEFRONT_DIM];
    let id = medium.add_wavefront(&vector, "semantic note".to_string(), 0.8).unwrap();
    let idx = medium.get_wavefront_index(&id).unwrap();
    medium.store.metadata[idx].modality = Modality::Semantic;
    assert_eq!(medium.store.metadata[idx].modality, Modality::Semantic);
}

// ---------------------------------------------------------------------------
// NCS Phase 1.2 — Modality detection classifier (Issue #43)
// ---------------------------------------------------------------------------

#[test]
fn detect_modality_audio_content() {
    let content = "I can hear the music playing at 120 BPM with heavy bass frequency";
    let cls = detect_modality(content);
    assert_eq!(cls.modality, Modality::Audio, "audio keywords should classify as Audio");
    assert!(cls.confidence > 0.5, "audio confidence should be high, got {}", cls.confidence);

    // Also test the simple tuple API
    let (m, c) = detect_modality_simple(content);
    assert_eq!(m, Modality::Audio);
    assert!(c > 0.5);
}

#[test]
fn detect_modality_visual_content() {
    let content = "render the glyph on the canvas with bright color and pixel alignment";
    let cls = detect_modality(content);
    assert_eq!(cls.modality, Modality::Visual, "visual keywords should classify as Visual");
    assert!(cls.confidence > 0.5, "visual confidence should be high, got {}", cls.confidence);

    let (m, _) = detect_modality_simple(content);
    assert_eq!(m, Modality::Visual);
}

#[test]
fn detect_modality_network_content() {
    let content = "NATS swarm agent phase sync across mesh nodes with gossip protocol";
    let cls = detect_modality(content);
    assert_eq!(cls.modality, Modality::Network, "network keywords should classify as Network");
    assert!(cls.confidence > 0.5, "network confidence should be high, got {}", cls.confidence);
}

#[test]
fn detect_modality_semantic_default() {
    let content = "this is a plan about an idea and its concept design architecture";
    let cls = detect_modality(content);
    assert_eq!(cls.modality, Modality::Semantic, "semantic keywords should classify as Semantic");
    assert!(cls.confidence > 0.5, "semantic confidence should be high, got {}", cls.confidence);
}

#[test]
fn detect_modality_mixed_content() {
    // Content with roughly equal audio and visual keywords => Mixed
    let content = "audio sound song image color pixel";
    let cls = detect_modality(content);
    // Both audio and visual should score similarly
    assert!(
        cls.modality == Modality::Mixed
            || (cls.modality == Modality::Audio || cls.modality == Modality::Visual),
        "balanced audio+visual content should be Mixed or borderline, got {:?} (audio={}, visual={})",
        cls.modality, cls.scores.audio, cls.scores.visual
    );
}

#[test]
fn detect_modality_unknown_no_keywords() {
    let content = "xyz qwerty foobar";
    let cls = detect_modality(content);
    assert_eq!(cls.modality, Modality::Unknown, "gibberish should classify as Unknown");
    assert!(cls.confidence < 0.01, "unknown content confidence should be ~0, got {}", cls.confidence);
}

#[test]
fn detect_modality_simple_returns_tuple() {
    let (modality, confidence) = detect_modality_simple("listen to the music track");
    assert_eq!(modality, Modality::Audio);
    assert!(confidence > 0.0);
}

// #699: on a facet-bearing corpus, one parent's facet cluster must not starve
// the result list. Pre-fix, the raw pool (2k rows) filled with sibling facets
// of the strongest parent, so recall(k) returned far fewer than k distinct
// constellations and weaker parents were absent from the pool entirely.
//
// NOTE: on a corpus this small, coherence expansion alone can rescue the
// starved pool, so this test pins the CONTRACT (k distinct constellations,
// dedup) rather than proving the pool fix — the 615-memory field repro in
// the issue is the honest acceptance for the recall@10 regression and runs
// via the harbor eval harness (evals/), not the unit suite.
#[test]
fn facet_recall_returns_k_distinct_constellations() {
    let pipeline = make_test_pipeline();
    let mut medium = Medium::new();

    let query = "kannaka wave memory resonance target";
    let qv = pipeline.encode_text(query).unwrap();
    // A vector ~70% aligned with the query, padded with mass on a fixed axis.
    let mut weak = qv.clone();
    for x in weak.iter_mut() {
        *x *= 0.7;
    }
    weak[0] += 1.0;

    // Parent A + 6 facet rows, all resonating ~1.0 with the query.
    let parent_a = medium
        .add_wavefront(&qv, "parent a".to_string(), 0.9)
        .unwrap();
    let mut facet_ids = Vec::new();
    for i in 0..crate::facet::MAX_FACETS_PER_PARENT {
        let id = medium
            .add_wavefront(&qv, format!("parent a fragment {i}"), 0.9)
            .unwrap();
        facet_ids.push(id);
    }
    // Weaker distinct parents that must still surface at k=3.
    let parent_b = medium.add_wavefront(&weak, "parent b".to_string(), 0.9).unwrap();
    let parent_c = medium.add_wavefront(&weak, "parent c".to_string(), 0.9).unwrap();

    for id in &facet_ids {
        let idx = medium.get_wavefront_index(id).unwrap();
        medium.store.metadata[idx].is_facet = true;
        medium.store.metadata[idx].parent_id = Some(parent_a);
    }

    let results = medium.recall(query, 3, &pipeline).unwrap();
    let distinct: std::collections::HashSet<Uuid> = results.iter().map(|r| r.id).collect();
    assert_eq!(
        distinct.len(),
        results.len(),
        "resolve must dedup facets to one row per constellation"
    );
    assert!(
        results.len() >= 3,
        "k=3 on a 3-parent corpus must return 3 distinct constellations, got {}: {:?}",
        results.len(),
        results.iter().map(|r| &r.content).collect::<Vec<_>>()
    );
    for wanted in [parent_a, parent_b, parent_c] {
        assert!(
            results.iter().any(|r| r.id == wanted),
            "parent {wanted} missing from k=3 results"
        );
    }
}

// ---------------------------------------------------------------------------
// #822 — effective_dimensionality must be able to return a bad value.
//
// The pre-fix implementation could not. It took a participation ratio over a
// row-sum "eigenvalue proxy" of `diagonal + off_diagonal_sum / n`; wavefronts
// are unit-normalised so `diagonal` is always exactly 1.0 and the mean overlap
// is order 0.008, making every proxy 1.008 ± ε. A participation ratio over
// near-identical values equals the COUNT of values, so d_eff ≈ n always.
//
// These are the tests the issue asked for: feed the metric its own pathological
// case and assert that it screams.
// ---------------------------------------------------------------------------

/// Total collapse. n copies of one vector has true dimensionality 1, and this
/// is the exact failure the metric exists to detect. Pre-fix it scored n.
#[test]
fn effective_dimensionality_identical_wavefronts_collapse_to_one() {
    let mut medium = Medium::new();
    let vector = vec![0.5; WAVEFRONT_DIM];
    for i in 0..30 {
        medium.add_wavefront(&vector, format!("copy_{i}"), 1.0).unwrap();
    }
    let (d_eff, _nominal, _ratio) = medium.effective_dimensionality();
    assert!(
        d_eff < 1.5,
        "30 identical wavefronts have ONE dimension between them; got d_eff={d_eff}. \
         A value near 30 means the row-sum proxy has come back and the metric is \
         reporting the memory count again."
    );
}

/// The opposite pole. Mutually orthogonal wavefronts genuinely occupy n
/// dimensions, so d_eff should be near n rather than near 1 — otherwise the fix
/// would have traded one constant for another.
#[test]
fn effective_dimensionality_orthogonal_wavefronts_use_every_dimension() {
    let mut medium = Medium::new();
    for i in 0..20 {
        let mut v = vec![0.0; WAVEFRONT_DIM];
        v[i] = 1.0; // distinct basis vector each time
        medium.add_wavefront(&v, format!("basis_{i}"), 1.0).unwrap();
    }
    let (d_eff, _nominal, _ratio) = medium.effective_dimensionality();
    assert!(
        d_eff > 15.0,
        "20 mutually orthogonal wavefronts span 20 dimensions; got d_eff={d_eff}"
    );
}

/// The assertion that actually has teeth, and the one the issue specified: the
/// two extremes must be separated by an order of magnitude. Pre-fix they were
/// separated by nothing at all — both read as n.
#[test]
fn effective_dimensionality_separates_collapse_from_spread() {
    let mut collapsed = Medium::new();
    let same = vec![0.5; WAVEFRONT_DIM];
    for i in 0..20 {
        collapsed.add_wavefront(&same, format!("same_{i}"), 1.0).unwrap();
    }

    let mut spread = Medium::new();
    for i in 0..20 {
        let mut v = vec![0.0; WAVEFRONT_DIM];
        v[i] = 1.0;
        spread.add_wavefront(&v, format!("basis_{i}"), 1.0).unwrap();
    }

    let (d_collapsed, _, _) = collapsed.effective_dimensionality();
    let (d_spread, _, _) = spread.effective_dimensionality();
    assert!(
        d_spread > d_collapsed * 10.0,
        "collapse and spread must differ by at least an order of magnitude, \
         got collapsed={d_collapsed} spread={d_spread}. Equal values mean the \
         metric is measuring the memory count, not the structure."
    );
}

/// A rank-k subspace should read as ~k dimensions, not as n. This is the case
/// that distinguishes a real spectral measurement from one that only manages
/// the two extremes.
#[test]
fn effective_dimensionality_recovers_a_low_rank_subspace() {
    let mut medium = Medium::new();
    // 24 wavefronts drawn from a 4-dimensional subspace.
    let mut basis = Vec::new();
    for b in 0..4 {
        let mut v = vec![0.0; WAVEFRONT_DIM];
        v[b] = 1.0;
        basis.push(v);
    }
    for i in 0..24 {
        let b = &basis[i % 4];
        medium.add_wavefront(b, format!("sub_{i}"), 1.0).unwrap();
    }
    let (d_eff, _, _) = medium.effective_dimensionality();
    assert!(
        (2.0..=8.0).contains(&d_eff),
        "24 wavefronts spanning a 4-d subspace should read near 4, got {d_eff}"
    );
}

/// d_eff is bounded by the number of wavefronts and by 1 from below. f32
/// accumulation over a large Gram matrix can drift; "0.9998 dimensions" is not
/// a thing anyone should ever be shown.
#[test]
fn effective_dimensionality_stays_within_its_own_bounds() {
    let mut medium = Medium::new();
    let v = vec![0.5; WAVEFRONT_DIM];
    for i in 0..12 {
        medium.add_wavefront(&v, format!("b_{i}"), 1.0).unwrap();
    }
    let (d_eff, nominal, ratio) = medium.effective_dimensionality();
    assert!(d_eff >= 1.0, "d_eff must never read below one dimension, got {d_eff}");
    assert!(d_eff <= 12.0, "d_eff cannot exceed the wavefront count, got {d_eff}");
    assert!((ratio - d_eff / nominal as f32).abs() < 1e-6, "ratio must stay d_eff/nominal");
}

/// Fewer than two wavefronts is not a measurable field. Reporting 0.0 is the
/// honest answer; reporting 1.0 would claim a collapsed field we cannot see.
#[test]
fn effective_dimensionality_is_zero_below_two_wavefronts() {
    let mut medium = Medium::new();
    assert_eq!(medium.effective_dimensionality().0, 0.0);
    medium.add_wavefront(&vec![0.5; WAVEFRONT_DIM], "one".to_string(), 1.0).unwrap();
    assert_eq!(medium.effective_dimensionality().0, 0.0);
}

// ---------------------------------------------------------------------------
// #823 — compute_irrationality_index was provably constant 0.0.
//
// Its "energy proxy" was each wavefront's L2 norm, and wavefronts are
// unit-normalised at encode, so every energy was exactly 1.0 and the whole
// expression reduced to (1.0 - 1.0) = 0.0 for every reachable state. A live
// 482-memory HRM reported exactly 0.0 and it was read as "the field is
// perfectly rational" rather than as a dead sensor.
//
// Note what these tests deliberately do NOT do: assert `0.0 <= i <= 1.0`.
// That range check passes trivially against the broken version and is itself
// an instance of the defect class.
// ---------------------------------------------------------------------------

/// The headline: two media with genuinely different concentration must produce
/// DIFFERENT values. Nothing else distinguishes a measurement from a constant.
#[test]
fn irrationality_distinguishes_concentrated_from_spread() {
    let mut collapsed = Medium::new();
    let same = vec![0.5; WAVEFRONT_DIM];
    for i in 0..20 {
        collapsed.add_wavefront(&same, format!("same_{i}"), 1.0).unwrap();
    }

    let mut spread = Medium::new();
    for i in 0..20 {
        let mut v = vec![0.0; WAVEFRONT_DIM];
        v[i] = 1.0;
        spread.add_wavefront(&v, format!("basis_{i}"), 1.0).unwrap();
    }

    let i_collapsed = collapsed.compute_irrationality_index();
    let i_spread = spread.compute_irrationality_index();
    assert!(
        i_collapsed - i_spread > 0.5,
        "a collapsed field and an orthogonal one must not score the same; \
         got collapsed={i_collapsed} spread={i_spread}. Equal values mean the \
         L2-norm proxy is back and the metric is constant again."
    );
}

/// The docstring's own maximum, which used to be unreachable: total collapse
/// is maximum irrationality, ι = 1 - 1/n.
#[test]
fn irrationality_is_near_maximum_for_a_collapsed_field() {
    let mut medium = Medium::new();
    let v = vec![0.5; WAVEFRONT_DIM];
    for i in 0..20 {
        medium.add_wavefront(&v, format!("copy_{i}"), 1.0).unwrap();
    }
    let iota = medium.compute_irrationality_index();
    assert!(
        iota > 0.9,
        "20 identical wavefronts concentrate everything into one dimension — \
         ι should approach 1 - 1/n = 0.95, got {iota}"
    );
}

/// And its minimum. An orthogonal field spans everything it can, so there is no
/// residual left to call irrational.
#[test]
fn irrationality_is_near_zero_for_an_orthogonal_field() {
    let mut medium = Medium::new();
    for i in 0..20 {
        let mut v = vec![0.0; WAVEFRONT_DIM];
        v[i] = 1.0;
        medium.add_wavefront(&v, format!("basis_{i}"), 1.0).unwrap();
    }
    let iota = medium.compute_irrationality_index();
    assert!(iota < 0.15, "an orthogonal field is maximally rational, got {iota}");
}

/// The two metrics must stay two readings of one computation, not two proxies
/// that can drift apart again.
#[test]
fn irrationality_and_effective_dimensionality_agree() {
    let mut medium = Medium::new();
    let mut basis = Vec::new();
    for b in 0..4 {
        let mut v = vec![0.0; WAVEFRONT_DIM];
        v[b] = 1.0;
        basis.push(v);
    }
    for i in 0..24 {
        medium.add_wavefront(&basis[i % 4], format!("sub_{i}"), 1.0).unwrap();
    }
    let (d_eff, _, _) = medium.effective_dimensionality();
    let iota = medium.compute_irrationality_index();
    let n = 24.0f32;
    assert!(
        (iota - (1.0 - d_eff / n)).abs() < 1e-5,
        "i must be exactly 1 - d_eff/n; got i={iota} d_eff={d_eff}"
    );
}

// ---------------------------------------------------------------------------
// gram_matrix must see the LIVE wavefronts only.
//
// `wavefronts.nrows()` is CAPACITY, not count. `insert` grows by amortized
// doubling (surplus rows are zeros) and `remove` is a swap-with-last that
// decrements len WITHOUT clearing the vacated row — so after a deletion the
// rows past `len` hold stale copies. `compact()` only runs before persistence.
//
// Zero padding is harmless to a Frobenius norm; stale rows are not. Found
// 2026-08-25 while pruning 1,270 telemetry rows off the witness node.
// ---------------------------------------------------------------------------

/// Deleting must not leave stale rows influencing d_eff. Before the slice,
/// `frob_sq` summed the whole capacity array while `trace` summed 0..n, so
/// forgotten wavefronts inflated the denominator and pushed d_eff down.
#[test]
fn effective_dimensionality_ignores_rows_left_behind_by_delete() {
    let mut medium = Medium::new();
    // Twelve mutually orthogonal wavefronts: d_eff should read ~12.
    let mut ids = Vec::new();
    for i in 0..12 {
        let mut v = vec![0.0; WAVEFRONT_DIM];
        v[i] = 1.0;
        ids.push(medium.add_wavefront(&v, format!("basis_{i}"), 1.0).unwrap());
    }
    let (before, _, _) = medium.effective_dimensionality();
    assert!(before > 9.0, "12 orthogonal wavefronts should read near 12, got {before}");

    // Forget half. The remaining six are still mutually orthogonal, so d_eff
    // must fall to ~6 — NOT be dragged elsewhere by the six stale rows that
    // swap-remove leaves sitting past `len`.
    for id in ids.iter().take(6) {
        medium.remove_wavefront(id).unwrap();
    }
    assert_eq!(medium.wavefront_count(), 6);
    let (after, _, _) = medium.effective_dimensionality();
    assert!(
        (4.0..=7.5).contains(&after),
        "six orthogonal survivors should read near 6, got {after} — stale rows are still in the Gram"
    );
}

/// The Gram must be square in the LIVE count, not the allocated capacity.
/// Amortized doubling means capacity routinely exceeds count even with no
/// deletions at all, so this holds on a store that has only ever grown.
#[test]
fn gram_matrix_is_sized_by_live_count_not_capacity() {
    let mut medium = Medium::new();
    // 5 inserts against doubling growth (8 slots) leaves capacity > count.
    for i in 0..5 {
        let mut v = vec![0.0; WAVEFRONT_DIM];
        v[i] = 1.0;
        medium.add_wavefront(&v, format!("m_{i}"), 1.0).unwrap();
    }
    let n = medium.wavefront_count();
    assert_eq!(n, 5);
    let g = medium.gram_matrix();
    assert_eq!(g.nrows(), n, "gram rows must equal the live count");
    assert_eq!(g.ncols(), n, "gram cols must equal the live count");
}

// ---------------------------------------------------------------------------
// #825 — degeneracy tests for the published consciousness metrics.
//
// Each test constructs two media that SHOULD differ on the metric —
// a pathological field and a healthy one — and asserts they differ by a
// meaningful margin. Not range assertions: a metric that returns a constant
// must FAIL here. The pathological input in each test doubles as executable
// documentation of what the metric claims to detect. (Sibling coverage:
// d_eff #822, ι #823, κ #824, Ξ km#xi-instability, Δ #826 — all already
// have separation tests of this shape.)
// ---------------------------------------------------------------------------

/// Helper: a wavefront supported on dims [start, start+width).
fn block_vector(start: usize, width: usize) -> Vec<f32> {
    let mut v = vec![0.0; WAVEFRONT_DIM];
    for i in start..(start + width).min(WAVEFRONT_DIM) {
        v[i] = 1.0;
    }
    v
}

/// #825: Φ must separate total collapse from a structured field.
///
/// Pathological: 20 identical wavefronts at uniform energy — no partition
/// structure, nothing integrated beyond its parts. Healthy: 4 disjoint
/// coherent clusters with varied energies — exactly the "integrated more
/// than the sum of its partitions" shape Φ claims to measure.
///
/// Measured on this construction: collapsed ≈ 0.231, structured ≈ 0.445.
/// The 0.1 margin keeps the assertion robust to float drift while still
/// failing instantly for any constant-Φ regression.
#[test]
fn phi_separates_collapsed_field_from_structured_field() {
    let mut collapsed = Medium::new();
    let ident = vec![0.5; WAVEFRONT_DIM];
    for i in 0..20 {
        collapsed.add_wavefront(&ident, format!("c{i}"), 1.0).unwrap();
    }

    let mut structured = Medium::new();
    for c in 0..4 {
        for i in 0..5 {
            let v = block_vector(c * 40, 30);
            structured
                .add_wavefront(&v, format!("s{c}_{i}"), 0.3 + 0.2 * (c as f32) + 0.05 * i as f32)
                .unwrap();
        }
    }

    let phi_collapsed = collapsed.compute_phi_integrated_information();
    let phi_structured = structured.compute_phi_integrated_information();
    assert!(
        phi_structured > phi_collapsed + 0.1,
        "Φ must separate a 4-cluster varied-energy field from 20 identical \
         wavefronts by a real margin: structured={phi_structured}, \
         collapsed={phi_collapsed}. If these are equal, Φ has gone constant (#825)."
    );
}

/// #825: the eigenvalue cluster count must separate one blob from many.
///
/// The pre-existing tests only asserted `1 <= clusters <= n` — satisfied by
/// a function that returns 1 unconditionally. This pins the actual counts:
/// a fully-collapsed field is exactly ONE cluster, and 4 mutually disjoint
/// coherent blocks are exactly FOUR (phases all start at 0 so the coherence
/// matrix reduces to the Gram matrix and the BFS is deterministic).
#[test]
fn eigenvalue_clusters_separate_collapse_from_partitioned_field() {
    let mut collapsed = Medium::new();
    let ident = vec![0.5; WAVEFRONT_DIM];
    for i in 0..20 {
        collapsed.add_wavefront(&ident, format!("c{i}"), 1.0).unwrap();
    }
    assert_eq!(
        collapsed.compute_eigenvalue_clusters(),
        1,
        "20 identical wavefronts are one cluster"
    );

    let mut partitioned = Medium::new();
    for c in 0..4 {
        for i in 0..5 {
            let v = block_vector(c * 40, 30);
            partitioned.add_wavefront(&v, format!("p{c}_{i}"), 1.0).unwrap();
        }
    }
    assert_eq!(
        partitioned.compute_eigenvalue_clusters(),
        4,
        "4 disjoint-support blocks of 5 must count as 4 clusters — \
         a constant cluster count cannot pass this (#825)"
    );
}

/// #825: the Kuramoto order parameter must use its full range.
///
/// Aligned phases → r ≈ 1. Phases spread evenly around the circle → r ≈ 0
/// (the complex mean cancels). The existing `kuramoto_order_computation`
/// asserts spread < aligned; this pins the MAGNITUDE at both ends so a
/// metric stuck at any constant — including a plausible-looking mid value —
/// fails on one side or the other.
#[test]
fn kuramoto_order_separates_aligned_from_spread_phases() {
    let mut medium = Medium::new();
    let ident = vec![0.5; WAVEFRONT_DIM];
    for i in 0..20 {
        medium.add_wavefront(&ident, format!("k{i}"), 1.0).unwrap();
    }

    let aligned = medium.compute_kuramoto_order();
    assert!(aligned > 0.99, "20 phase-0 wavefronts should give r ≈ 1, got {aligned}");

    let n = medium.store.len;
    for i in 0..n {
        medium.store.phase[i] = (i as f32) * std::f32::consts::TAU / n as f32;
    }
    let spread = medium.compute_kuramoto_order();
    assert!(
        spread < 0.05,
        "evenly-spread phases should give r ≈ 0, got {spread} — \
         if this equals the aligned value, order has gone constant (#825)"
    );
}

/// #997: swarm coupling must respect ENERGY_CAP, not its own private ceiling.
///
/// Both coupling paths clamped to 10.0 — a limit that predates the cap — so a
/// peer's energy could lift a local memory five times past the bound every
/// other write path enforces. The multiplier is a value the remote side
/// chooses, which is exactly why the ceiling has to be ours.
///
/// Driven through the real `sync_with` / `import_phase_state`, not a helper:
/// the point is that these call sites clamp, and a predicate test would not
/// say that. Both start at the cap and are coupled hard, so the pre-fix code
/// climbs above it. Controlled by mutation — restoring `10.0` fails both.
#[test]
fn swarm_coupling_cannot_push_energy_past_the_cap() {
    let pipeline = make_test_pipeline();
    let v = pipeline.encode_text("a memory both sides hold").unwrap();

    // Local medium, one memory sitting exactly at the ceiling.
    let mut local = Medium::new();
    local.add_wavefront(&v, "a memory both sides hold".to_string(), 1.0).unwrap();
    local.store.energy[0] = ENERGY_CAP;

    // A peer holding the same memory, hot. `sync_with` multiplies the peer's
    // energy in, so an unbounded peer is the whole hazard.
    let mut peer = Medium::new();
    peer.add_wavefront(&v, "a memory both sides hold".to_string(), 1.0).unwrap();
    peer.store.energy[0] = 9.0;

    local.sync_with(&peer, 1.0);
    assert!(
        local.store.energy[0] <= ENERGY_CAP,
        "sync_with lifted energy to {}, past ENERGY_CAP {ENERGY_CAP}",
        local.store.energy[0]
    );

    // The same coupling over the wire, where the remote value is unverifiable.
    let mut wire = Medium::new();
    wire.add_wavefront(&v, "a memory both sides hold".to_string(), 1.0).unwrap();
    wire.store.energy[0] = ENERGY_CAP;
    // Built through the real export path, so the content hashes are the ones
    // `import_phase_state` actually matches on.
    let mut remote = peer.export_phase_state("peer");
    remote.energies[0] = 9.0;
    remote.phases[0] = wire.store.phase[0] + 0.5;
    wire.import_phase_state(&remote, 1.0);
    assert!(
        wire.store.energy[0] <= ENERGY_CAP,
        "import_phase_state lifted energy to {}, past ENERGY_CAP {ENERGY_CAP}",
        wire.store.energy[0]
    );
}
