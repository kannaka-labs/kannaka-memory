//! Wave dynamics: interference, dreaming (annealing), and coherence computation.

use ndarray::{Array2, s};

use super::Medium;
use super::types::*;

/// ADR-0036 Phase 2 (piece #1): tier-aware annealing energy floor.
///
/// The annealing/dream passes floor energy at the "bias voltage" (0.5) so the
/// field stays hot enough to amplify small attention signals. That floor is
/// correct for LongTerm/Pinned memory, but it prevents ShortTerm traces from
/// ever decaying below the consolidation evict threshold (0.15) — so an
/// unreactivated ShortTerm trace could never become eviction-eligible. Give
/// ShortTerm a lower floor (0.1) so salience decay can actually pull it under
/// the evict threshold; everything else keeps the 0.5 bias voltage.
const DREAM_ENERGY_FLOOR_DEFAULT: f32 = 0.5;
const DREAM_ENERGY_FLOOR_SHORTTERM: f32 = 0.1;

#[inline]
fn tier_energy_floor(tier: Tier) -> f32 {
    match tier {
        Tier::ShortTerm => DREAM_ENERGY_FLOOR_SHORTTERM,
        _ => DREAM_ENERGY_FLOOR_DEFAULT,
    }
}

impl Medium {
    /// Apply the ghostmagicOS dynamics equation: dx/dt = f(x) - Inx
    ///
    /// This implements the continuous update rule where:
    /// - f(x) = constructive interference from phase-aligned neighbors (growth toward attractors)
    /// - Inx = dampening proportional to current energy (wisdom/decay)
    ///
    /// # Arguments
    /// * `dt` - Time step for integration
    pub fn apply_dynamics(&mut self, dt: f32) {
        if self.wavefront_count() < 2 {
            return;
        }

        let n = self.wavefront_count();
        let threshold = 0.5; // Minimum dot product for interference
        let eta = 0.02; // Dampening rate — gentle, field stays biased

        // Compute pairwise interference matrix
        let interference_matrix = self.compute_interference_matrix(threshold);

        // Apply f(x) - constructive interference term
        let mut growth_terms = vec![0.0f32; n];
        for i in 0..n {
            for j in 0..n {
                if i != j && interference_matrix[[i, j]] > 0.0 {
                    // Phase alignment factor
                    let phase_alignment = (self.store.phase[j] - self.store.phase[i]).cos();
                    let constructive =
                        interference_matrix[[i, j]] * phase_alignment * self.store.energy[j];
                    growth_terms[i] += constructive;
                }
            }
            growth_terms[i] /= n as f32; // Normalize by neighbor count
        }

        // Apply Inx - dampening term proportional to current energy
        for i in 0..n {
            let growth = growth_terms[i] * dt;
            let dampening = eta * self.store.energy[i] * dt;

            // Track total energy dampened for wisdom calculation
            self.total_energy_dampened += dampening;

            // dx/dt = f(x) - Iηx
            // Floor at the "bias voltage" — field stays hot for amplification.
            // Tier-aware (ADR-0036 Phase 2): ShortTerm floors lower (0.1) so it
            // can decay below the consolidation evict threshold; others = 0.5.
            let floor = tier_energy_floor(self.store.metadata[i].tier);
            self.store.energy[i] = (self.store.energy[i] + growth - dampening).max(floor);

            // Phase coupling - frequencies converge when strongly coupled
            if growth > dampening * 0.5 {
                // Only when growing
                let mut phase_coupling = 0.0f32;
                let mut coupling_count = 0;

                for j in 0..n {
                    if i != j && interference_matrix[[i, j]] > threshold {
                        phase_coupling += self.store.phase[j];
                        coupling_count += 1;
                    }
                }

                if coupling_count > 0 {
                    let target_phase = phase_coupling / coupling_count as f32;
                    let coupling_strength = 0.05 * dt;
                    self.store.phase[i] += coupling_strength * (target_phase - self.store.phase[i]).sin();
                }
            }
        }
    }

    /// Compute the interference matrix between all wavefront pairs
    ///
    /// Returns an NxN matrix where element [i,j] represents the interference
    /// strength between wavefront i and wavefront j based on their dot product
    /// and phase coherence.
    pub fn compute_interference_matrix(&self, threshold: f32) -> Array2<f32> {
        let n = self.wavefront_count();
        let gram = self.gram_matrix();
        let (cos_p, sin_p) = self.phase_trig();

        let mut interference = Array2::zeros((n, n));
        for i in 0..n {
            for j in 0..n {
                if i != j {
                    let dot_product = gram[[i, j]];
                    if dot_product.abs() > threshold {
                        // cos(phase_i - phase_j) via the angle-difference identity
                        let phase_coherence = cos_p[i] * cos_p[j] + sin_p[i] * sin_p[j];
                        interference[[i, j]] = (dot_product * phase_coherence).abs();
                    }
                }
            }
        }

        interference
    }

    /// Pairwise dot products of all wavefronts — H · Hᵀ as one real matrix
    /// multiplication (matrixmultiply-backed). The naive per-element loop
    /// this replaced cost ~40s on a 650×1024 field; this is ~100ms. Every
    /// O(N²·dim) consumer (coherence, interference, Gram-based Ξ) routes
    /// through here.
    /// H·Hᵀ over the LIVE wavefronts only.
    ///
    /// ⚠ The slice is load-bearing twice over, because `wavefronts.nrows()` is
    /// the store's *capacity*, not its count:
    ///
    /// - `insert` grows by amortized doubling, so straight after a growth the
    ///   array is up to 2× the live count, the surplus rows being zeros.
    /// - `remove` is a swap-with-last that decrements `len` and does NOT clear
    ///   the vacated row, so after any deletion the rows past `len` hold STALE
    ///   COPIES of live or forgotten wavefronts. `compact()` only runs before
    ///   persistence.
    ///
    /// Unsliced, the zero rows are harmless to a Frobenius norm (a zero row
    /// makes its whole Gram row zero) but the stale ones are not: they inflate
    /// ‖G‖_F and pull `effective_dimensionality` down after every prune. And
    /// either way the product is computed at capacity² when only count² is ever
    /// read — up to 4× the work on a freshly-doubled store.
    pub(crate) fn gram_matrix(&self) -> Array2<f32> {
        let n = self.store.count();
        let h = self.store.wavefronts.slice(s![..n, ..]);
        h.dot(&h.t())
    }

    /// ADR-0037 Phase 4b: top-2 PCA of a wavefront matrix (power iteration +
    /// deflation, dependency-free; classical-MDS sample-space coordinates) →
    /// 2-D coords paired with `phases` as `(x, y, theta)`. Shared by `Medium`
    /// (synced flat field) and `ChiralMedium::holistic_cloud_report` (the right
    /// hemisphere — the field the Phase-2 coupling actually rotates). Empty
    /// below 4 rows or when `phases` is shorter than the row count.
    pub(crate) fn pca_field_2d(wavefronts: &Array2<f32>, phases: &[f32]) -> Vec<(f32, f32, f32)> {
        let n = wavefronts.nrows();
        if n < 4 || phases.len() < n {
            return Vec::new();
        }
        let gram = wavefronts.dot(&wavefronts.t());
        let matvec = |v: &[f32]| -> Vec<f32> {
            (0..n).map(|i| (0..n).map(|j| gram[[i, j]] * v[j]).sum()).collect()
        };
        let normalize = |v: &mut [f32]| -> f32 {
            let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
            if norm > 1e-12 {
                for x in v.iter_mut() {
                    *x /= norm;
                }
            }
            norm
        };
        let dot = |a: &[f32], b: &[f32]| -> f32 { a.iter().zip(b).map(|(x, y)| x * y).sum() };
        let seed = |salt: u64| -> Vec<f32> {
            (0..n)
                .map(|i| {
                    (((i as u64).wrapping_mul(2654435761).wrapping_add(salt) % 1000) as f32 / 1000.0)
                        - 0.5
                })
                .collect()
        };

        let mut v1 = seed(1);
        normalize(&mut v1);
        for _ in 0..80 {
            let mut w = matvec(&v1);
            if normalize(&mut w) < 1e-20 {
                break;
            }
            v1 = w;
        }
        let l1 = dot(&v1, &matvec(&v1));

        let mut v2 = seed(7);
        let p0 = dot(&v2, &v1);
        for i in 0..n {
            v2[i] -= p0 * v1[i];
        }
        normalize(&mut v2);
        for _ in 0..80 {
            let mut w = matvec(&v2);
            let p = dot(&w, &v1);
            for i in 0..n {
                w[i] -= p * v1[i];
            }
            if normalize(&mut w) < 1e-20 {
                break;
            }
            v2 = w;
        }
        let l2 = dot(&v2, &matvec(&v2));

        let s1 = l1.max(0.0).sqrt();
        let s2 = l2.max(0.0).sqrt();
        (0..n).map(|i| (s1 * v1[i], s2 * v2[i], phases[i])).collect()
    }

    /// ADR-0037 Phase 4b: 2-D spiral report over a PCA-embedded wavefront/phase
    /// field — runs `cloud_singularities` and tallies cores + net charge.
    pub(crate) fn cloud_report_2d(
        wavefronts: &Array2<f32>,
        phases: &[f32],
    ) -> crate::spiral::SpiralReport {
        let pts = Self::pca_field_2d(wavefronts, phases);
        let singularities = crate::spiral::cloud_singularities(&pts, 8);
        let net_charge = singularities.iter().map(|s| s.charge).sum();
        let field_phases: Vec<f32> = pts.iter().map(|p| p.2).collect();
        crate::spiral::SpiralReport {
            n: pts.len(),
            order: crate::spiral::kuramoto_order(&field_phases),
            singularities,
            net_charge,
        }
    }

    /// ADR-0037 Phase 4b (#415 task 1): 2-D PCA projection of this (synced flat)
    /// medium's field. For the holistic field during a dream use
    /// `ChiralMedium::holistic_cloud_report` (reads the right hemisphere).
    pub fn spiral_field_2d(&self) -> Vec<(f32, f32, f32)> {
        let n = self.wavefront_count();
        let wf = self.store.wavefronts.slice(ndarray::s![..n, ..]).to_owned();
        let phases: Vec<f32> = self.store.phase.slice(ndarray::s![..n]).iter().copied().collect();
        Self::pca_field_2d(&wf, &phases)
    }

    /// ADR-0037 Phase 4b: full 2-D spiral report over this medium's field.
    pub fn spiral_cloud_report(&self) -> crate::spiral::SpiralReport {
        let n = self.wavefront_count();
        let wf = self.store.wavefronts.slice(ndarray::s![..n, ..]).to_owned();
        let phases: Vec<f32> = self.store.phase.slice(ndarray::s![..n]).iter().copied().collect();
        Self::cloud_report_2d(&wf, &phases)
    }

    /// Cached-per-call cos/sin of every wavefront phase, for building
    /// cos(Δphase) terms without N² trig calls.
    fn phase_trig(&self) -> (Vec<f32>, Vec<f32>) {
        let n = self.wavefront_count();
        let mut cos_p = Vec::with_capacity(n);
        let mut sin_p = Vec::with_capacity(n);
        for i in 0..n {
            cos_p.push(self.store.phase[i].cos());
            sin_p.push(self.store.phase[i].sin());
        }
        (cos_p, sin_p)
    }

    /// Compute pairwise coherence matrix for all wavefronts
    ///
    /// Returns an NxN matrix where element [i,j] represents the coherence
    /// between wavefront i and wavefront j using the formula:
    /// coherence = cos(phase_i - phase_j) * dot(h_i, h_j)
    pub fn coherence_matrix(&self) -> Array2<f32> {
        let n = self.wavefront_count();
        let gram = self.gram_matrix();
        let (cos_p, sin_p) = self.phase_trig();

        let mut coherence = Array2::zeros((n, n));
        for i in 0..n {
            for j in 0..n {
                if i != j {
                    let phase_coherence = cos_p[i] * cos_p[j] + sin_p[i] * sin_p[j];
                    coherence[[i, j]] = phase_coherence * gram[[i, j]];
                } else {
                    // Self-coherence is 1.0
                    coherence[[i, j]] = 1.0;
                }
            }
        }

        coherence
    }

    /// Eigenstructure-aware dream cycles - annealing on the eigenmode structure
    ///
    /// Each cycle:
    /// 1. Compute coherence matrix H·H^T eigendecomposition
    /// 2. Dampen low-eigenvalue modes (noise → forgetting)
    /// 3. Reinforce high-eigenvalue modes (patterns → consolidation) 
    /// 4. Phase alignment within high-coherence clusters
    /// 5. Hallucination: create new wavefronts from cross-cluster eigenmode superposition
    /// 6. Temperature annealing controls exploration strength
    ///
    /// # Arguments
    /// * `cycles` - Number of annealing cycles to run
    /// * `initial_temperature` - Starting temperature (default: 1.0)
    ///
    /// # Returns
    /// DreamReport with statistics about what happened
    pub fn dream(&mut self, cycles: usize, initial_temperature: Option<f32>) -> DreamReport {
        let mut temperature = initial_temperature.unwrap_or(1.0);
        let energy_before = if self.wavefront_count() > 0 {
            self.store.energy.slice(s![..self.store.len]).mean().unwrap_or(0.0)
        } else {
            0.0
        };

        let _initial_count = self.wavefront_count();
        let mut dissolved_count = 0;
        let mut strengthened_count = 0;
        let mut hallucinated_count = 0;
        let mut converged = false;

        let energy_threshold = 0.01; // Wavefronts below this energy get pruned
        let convergence_threshold = 0.001; // Change threshold for convergence detection
        let annealing_rate = 0.95; // Temperature reduction per cycle

        for cycle in 0..cycles {
            if self.wavefront_count() < 2 {
                break;
            }

            // Store energy state for convergence detection. Slice to the ACTIVE
            // count, not the full tensor capacity — else prev_energy.len() is the
            // amortized capacity and the `current_count == prev_energy.len()`
            // convergence guard below is false whenever capacity>len (the normal
            // case, and always after a prune), so the early-stop never fires and
            // `converged` is silently always false (mirrors Hemisphere::dream).
            let prev_energy: Vec<f32> = self.store.energy.slice(s![..self.store.len]).to_vec();

            // 1. Compute coherence matrix H·H^T and eigenstructure
            let coherence = self.coherence_matrix();
            let eigenstructure = self.compute_eigenstructure(&coherence);

            // 2. Apply eigenstructure-based annealing
            self.apply_eigenstructure_annealing(&eigenstructure, temperature);

            // 3. Hallucination: create novel wavefronts from cross-cluster superposition.
            //
            // Old gate was `cycle % 3 == 0 && wavefront_count < 500`, which
            // (a) only fired once across a 3-cycle deep dream, and (b)
            // silently disabled hallucination entirely on HRMs >500. A
            // 1391-memory store could dream for years and never produce
            // a single new wavefront. Replaced with a budget that scales
            // with HRM size (~1% growth per dream) and a per-cycle cap
            // proportional to that budget.
            let hallucination_budget = (self.wavefront_count() / 100).max(2);
            if temperature > 0.3 && hallucinated_count < hallucination_budget {
                let per_cycle = (hallucination_budget / cycles.max(1)).max(2);
                let hallucinated = self.generate_hallucinated_wavefronts(
                    &eigenstructure, temperature, per_cycle,
                );
                hallucinated_count += hallucinated;
            }

            // 4. Prune wavefronts below energy threshold (forgetting)
            let pruned = self.prune_low_energy_wavefronts(energy_threshold);
            dissolved_count += pruned;

            // Count strengthened wavefronts. Old absolute threshold (+0.1)
            // counted nothing on large HRMs because per-wavefront energy
            // is small — ratio threshold stays meaningful at any scale.
            for i in 0..self.wavefront_count().min(prev_energy.len()) {
                let prev = prev_energy[i];
                let curr = self.store.energy[i];
                if curr > prev + 0.1 || (prev > 1e-6 && curr > prev * 1.05) {
                    strengthened_count += 1;
                }
            }

            // 5. Reduce temperature (annealing schedule)
            temperature *= annealing_rate;

            // Check for convergence
            let mut energy_change = 0.0f32;
            let current_count = self.wavefront_count();
            if current_count == prev_energy.len() {
                for i in 0..current_count {
                    energy_change += (self.store.energy[i] - prev_energy[i]).abs();
                }
                energy_change /= current_count as f32;

                if energy_change < convergence_threshold && cycle > 5 {
                    converged = true;
                    break;
                }
            }
        }

        let energy_after = if self.wavefront_count() > 0 {
            self.store.energy.slice(s![..self.store.len]).mean().unwrap_or(0.0)
        } else {
            0.0
        };

        DreamReport {
            cycles_completed: cycles,
            wavefronts_dissolved: dissolved_count,
            wavefronts_strengthened: strengthened_count,
            wavefronts_hallucinated: hallucinated_count,
            energy_before,
            energy_after,
            final_temperature: temperature,
            converged,
        }
    }

    /// Apply dynamics with temperature modulation for exploration
    #[allow(dead_code)]
    fn apply_dynamics_with_temperature(&mut self, dt: f32, temperature: f32) {
        if self.wavefront_count() < 2 {
            return;
        }

        let n = self.wavefront_count();
        let threshold = 0.3; // Lower threshold for more connections during dreams
        
        // ISSUE #35 FIX: Scale annealing based on memory age variance
        // When memories have similar ages (bulk import), reduce dampening
        let age_variance = self.compute_memory_age_variance();
        let age_dampening_scale = if age_variance < 3600.0 { 0.1 } else { 1.0 }; // Low variance = recent bulk import
        let eta = 0.05 * (1.0 + temperature) * age_dampening_scale;
        
        // ISSUE #35 FIX: Minimum amplitude floor during dreams
        let dream_amplitude_floor = 0.05;

        // Compute interference with temperature-boosted exploration
        let mut growth_terms = vec![0.0f32; n];
        for i in 0..n {
            for j in 0..n {
                if i != j {
                    let vec_i = self.store.wavefronts.row(i);
                    let vec_j = self.store.wavefronts.row(j);

                    let dot_product: f32 =
                        vec_i.iter().zip(vec_j.iter()).map(|(a, b)| a * b).sum();

                    if dot_product.abs() > threshold {
                        let phase_diff = self.store.phase[i] - self.store.phase[j];
                        // Temperature adds exploration noise
                        let phase_alignment =
                            (phase_diff + temperature * 0.1 * (phase_diff * 2.0).sin()).cos();
                        let interference = dot_product * phase_alignment * self.store.energy[j];
                        growth_terms[i] += interference;
                    }
                }
            }
            growth_terms[i] /= n as f32;
        }

        // Apply dynamics with temperature-modulated dampening
        for i in 0..n {
            let growth = growth_terms[i] * dt;
            let dampening = eta * self.store.energy[i] * dt;

            // Track total energy dampened for wisdom calculation
            self.total_energy_dampened += dampening;

            // ISSUE #35 FIX: Apply amplitude floor during dream cycles
            let new_energy = self.store.energy[i] + growth - dampening;
            self.store.energy[i] = new_energy.max(dream_amplitude_floor);

            // Temperature-modulated phase evolution
            if growth.abs() > 0.001 {
                let phase_force = growth.signum() * 0.02 * dt * (1.0 + temperature * 0.5);
                self.store.phase[i] += phase_force;
            }
        }
    }

    /// Compute variance in memory ages (in seconds) to detect bulk imports
    /// Returns high values for memories created at different times,
    /// low values for bulk imports where all memories have similar timestamps
    pub fn compute_memory_age_variance(&self) -> f32 {
        if self.wavefront_count() < 2 {
            return 3600.0; // Default high variance for single/no memories
        }

        let now = chrono::Utc::now();
        let ages: Vec<f32> = self.store.metadata.iter()
            .map(|meta| (now - meta.created_at).num_seconds() as f32)
            .collect();

        let mean_age = ages.iter().sum::<f32>() / ages.len() as f32;
        let variance = ages.iter()
            .map(|&age| (age - mean_age).powi(2))
            .sum::<f32>() / ages.len() as f32;

        variance
    }

    /// Compute eigenstructure of the coherence matrix using power iteration.
    ///
    /// Returns approximate eigenvalues and associated wavefront clusters.
    /// Uses power iteration to find dominant eigenvalue/eigenvector, then deflation.
    fn compute_eigenstructure(&self, coherence: &Array2<f32>) -> EigenStructure {
        let n = self.wavefront_count();
        if n < 2 {
            return EigenStructure::empty();
        }

        let mut eigenvalues = Vec::new();
        let mut eigenvector_alignment = vec![0.0f32; n]; // How aligned each wavefront is with dominant eigenvector
        
        // Power iteration to find dominant eigenvalue/eigenvector
        let max_iterations = 20;
        let mut dominant_vector = vec![1.0f32; n];
        let mut eigenvalue = 0.0f32;
        
        // Normalize initial vector
        let norm: f32 = dominant_vector.iter().map(|x| x * x).sum::<f32>().sqrt();
        for x in &mut dominant_vector {
            *x /= norm;
        }
        
        // Power iteration
        for _ in 0..max_iterations {
            let mut next_vector = vec![0.0f32; n];
            
            // Matrix-vector multiply: coherence * dominant_vector
            for i in 0..n {
                for j in 0..n {
                    next_vector[i] += coherence[[i, j]] * dominant_vector[j];
                }
            }
            
            // Normalize
            let norm: f32 = next_vector.iter().map(|x| x * x).sum::<f32>().sqrt();
            if norm < 1e-6 {
                break;
            }
            
            for i in 0..n {
                next_vector[i] /= norm;
            }
            
            // Compute eigenvalue: v^T * A * v
            eigenvalue = 0.0;
            for i in 0..n {
                eigenvalue += next_vector[i] * {
                    let mut sum = 0.0f32;
                    for j in 0..n {
                        sum += coherence[[i, j]] * next_vector[j];
                    }
                    sum
                };
            }
            
            dominant_vector = next_vector;
        }
        
        eigenvalues.push(eigenvalue);
        eigenvector_alignment = dominant_vector;
        
        // Find high-coherence clusters based on dominant eigenvector
        let mut clusters = Vec::new();
        let alignment_threshold = 0.3;
        
        for i in 0..n {
            if eigenvector_alignment[i].abs() > alignment_threshold {
                clusters.push(i);
            }
        }
        
        EigenStructure {
            eigenvalues,
            dominant_cluster: clusters,
            wavefront_alignments: eigenvector_alignment,
            spectral_gap: eigenvalue - 0.1, // Approximate spectral gap (dominant vs others)
        }
    }
    
    /// Apply eigenstructure-based annealing to wavefronts.
    ///
    /// - Wavefronts aligned with high-eigenvalue modes get energy boost (consolidation)
    /// - Wavefronts aligned with low-eigenvalue modes get energy dampening (noise reduction)
    /// - Phase alignment within clusters
    fn apply_eigenstructure_annealing(&mut self, eigenstructure: &EigenStructure, temperature: f32) {
        let n = self.wavefront_count();
        if n == 0 || eigenstructure.eigenvalues.is_empty() {
            return;
        }
        
        let dominant_eigenvalue = eigenstructure.eigenvalues[0];
        
        // === TRIODE BIAS PRINCIPLE ===
        // Memories should maintain high resting energy (plate voltage).
        // Dreams DIFFERENTIATE (create energy differences) and PHASE-ORGANIZE,
        // but don't drain the field. Small attention signals amplify through
        // a biased field; a cold field amplifies nothing.
        
        // Gentle coefficients — dreams sculpt, they don't crush
        let consolidation_strength = 0.02 * (1.0 + temperature);
        let noise_reduction = 0.005 * (2.0 - temperature); // 10x gentler than before
        
        // Energy floor: memories never drop below this during dreams.
        // This IS the bias voltage — the resting potential that enables
        // amplification. Tier-aware (ADR-0036 Phase 2): computed per-wavefront
        // inside the loop so ShortTerm can decay below the evict threshold.

        for i in 0..n {
            let dream_energy_floor = tier_energy_floor(self.store.metadata[i].tier);
            let alignment = eigenstructure.wavefront_alignments[i].abs();
            
            // High alignment = pattern = boost energy + phase align
            // In high-D space, even 0.1 alignment is significant
            if alignment > 0.1 && dominant_eigenvalue > 0.1 {
                let boost = consolidation_strength * alignment;
                self.store.energy[i] = (self.store.energy[i] + boost).min(ENERGY_CAP);
                
                // Phase alignment within cluster
                if eigenstructure.dominant_cluster.contains(&i) && eigenstructure.dominant_cluster.len() > 1 {
                    let mut cluster_phase = 0.0f32;
                    for &cluster_idx in &eigenstructure.dominant_cluster {
                        cluster_phase += self.store.phase[cluster_idx];
                    }
                    cluster_phase /= eigenstructure.dominant_cluster.len() as f32;
                    
                    let phase_coupling = 0.05 * (1.0 - temperature);
                    self.store.phase[i] += phase_coupling * (cluster_phase - self.store.phase[i]).sin();
                }
            }
            
            // Low alignment = noise = gentle dampening toward floor, NOT toward zero
            if alignment < 0.05 {
                let reduction = noise_reduction * (0.1 - alignment);
                self.store.energy[i] = (self.store.energy[i] - reduction).max(dream_energy_floor);
                self.total_energy_dampened += reduction;
            }
            
            // Everything else: maintain energy, just phase-explore
            // The field stays hot — dreams organize, not destroy
            if (0.05..=0.1).contains(&alignment) {
                let exploration = temperature * 0.005;
                self.store.phase[i] += exploration * (alignment * 10.0 - 0.5).sin();
            }
            
            // Enforce floor on ALL wavefronts
            self.store.energy[i] = self.store.energy[i].max(dream_energy_floor);
        }
    }
    
    /// Generate hallucinated wavefronts by superposing patterns from different clusters.
    ///
    /// Creates novel combinations by mixing eigenvector components from the dominant
    /// cluster with other patterns, weighted by temperature (more hallucination at high temp).
    fn generate_hallucinated_wavefronts(
        &mut self,
        eigenstructure: &EigenStructure,
        temperature: f32,
        max_per_call: usize,
    ) -> usize {
        if self.wavefront_count() < 4 || eigenstructure.dominant_cluster.len() < 2 {
            return 0; // Need sufficient diversity for hallucination
        }

        // Only hallucinate at sufficiently high temperature
        if temperature < 0.4 {
            return 0;
        }

        let mut hallucinated = 0;
        // Caller decides the absolute cap; we still let temperature shrink it
        // so cooler late-stage cycles produce fewer novel wavefronts.
        let temp_cap = ((temperature * 3.0) as usize).max(1);
        let max_hallucinations = max_per_call.min(temp_cap);
        
        for _ in 0..max_hallucinations {
            // Select two different wavefronts for superposition
            let idx1 = eigenstructure.dominant_cluster[0]; // From dominant cluster
            
            // Find a wavefront outside the dominant cluster  
            let mut idx2 = None;
            for i in 0..self.wavefront_count() {
                if !eigenstructure.dominant_cluster.contains(&i) && 
                   eigenstructure.wavefront_alignments[i].abs() > 0.1 {
                    idx2 = Some(i);
                    break;
                }
            }
            
            if let Some(idx2) = idx2 {
                // Create superposition: mix two wavefront patterns
                let vec1 = self.store.wavefronts.row(idx1);
                let vec2 = self.store.wavefronts.row(idx2);
                
                let mix_ratio = 0.6 + temperature * 0.3; // Temperature affects mixing
                let mut new_vector = Vec::with_capacity(vec1.len());
                
                for (v1, v2) in vec1.iter().zip(vec2.iter()) {
                    new_vector.push(mix_ratio * v1 + (1.0 - mix_ratio) * v2);
                }
                
                // Normalize the new vector
                let norm: f32 = new_vector.iter().map(|x| x * x).sum::<f32>().sqrt();
                if norm > 1e-6 {
                    for x in &mut new_vector {
                        *x /= norm;
                    }
                    
                    // Add as new wavefront with averaged energy and phase
                    let energy = (self.store.energy[idx1] + self.store.energy[idx2]) / 2.0 * 0.8; // Slightly reduced
                    let phase = (self.store.phase[idx1] + self.store.phase[idx2]) / 2.0;
                    
                    let content = format!("HALLUCINATION: superposition of patterns {idx1}-{idx2} [temp={temperature:.2}]");
                    
                    if self.add_wavefront(&new_vector, content, energy).is_ok() {
                        // Set the phase for the newly added wavefront
                        let new_idx = self.wavefront_count() - 1;
                        self.store.phase[new_idx] = phase;
                        self.store.metadata[new_idx].hallucinated = true;
                        hallucinated += 1;
                    }
                }
            }
        }
        
        hallucinated
    }

    /// Prune wavefronts with energy below threshold (forgetting during dreams)
    fn prune_low_energy_wavefronts(&mut self, threshold: f32) -> usize {
        let mut to_remove = Vec::new();

        // Find wavefronts to remove
        for i in 0..self.wavefront_count() {
            if self.store.energy[i] < threshold {
                to_remove.push(self.store.metadata[i].id);
            }
        }

        let removed_count = to_remove.len();

        // Remove them (in reverse order to maintain indices)
        for id in to_remove {
            let _ = self.remove_wavefront(&id);
        }

        removed_count
    }

    /// Apply chiral field perturbation to break phase lock-step.
    ///
    /// Computes a field-level order parameter (Kuramoto-style) and applies
    /// phase noise proportional to each wavefront's energy. Higher global
    /// coherence → stronger perturbation to prevent crystallization.
    ///
    /// # Arguments
    /// * `eta` - Perturbation strength (typical: 0.01–0.1)
    pub fn apply_chiral_field_perturbation(&mut self, eta: f32) {
        let n = self.wavefront_count();
        if n < 2 {
            return;
        }

        // Compute field-level order parameter (Kuramoto R)
        let sum_cos: f32 = (0..n).map(|i| self.store.phase[i].cos()).sum();
        let sum_sin: f32 = (0..n).map(|i| self.store.phase[i].sin()).sum();
        let order = (sum_cos * sum_cos + sum_sin * sum_sin).sqrt() / n as f32;

        // Higher order → stronger perturbation (break lock-step)
        let strength = eta * order;

        // Apply phase noise proportional to energy
        let mean_energy = self.store.energy.slice(s![..self.store.len]).mean().unwrap_or(1.0);
        for i in 0..n {
            let noise = strength * (self.store.energy[i] / mean_energy);
            self.store.phase[i] += noise * (self.store.frequency[i] * 7.0 + self.store.phase[i] * 13.0).sin();
        }
    }
}
