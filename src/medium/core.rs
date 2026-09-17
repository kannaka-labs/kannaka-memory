//! Core wavefront operations: add, remove, store, recall.

use chrono::{DateTime, Utc};
use ndarray::Array1;
use uuid::Uuid;

use crate::encoding::EncodingPipeline;
use crate::xi_operator::{compute_xi_signature, xi_diversity_boost};

use super::Medium;
use super::types::*;

impl Medium {
    /// Add a new wavefront to the medium.
    ///
    /// # Arguments
    /// * `vector` - D-dimensional hypervector (must be exactly WAVEFRONT_DIM)
    /// * `content` - Original text content
    /// * `importance` - Initial energy/amplitude (typically 0.0-1.0)
    ///
    /// # Returns
    /// UUID of the new wavefront
    pub fn add_wavefront(
        &mut self,
        vector: &[f32],
        content: String,
        importance: f32,
    ) -> Result<Uuid, MediumError> {
        if vector.len() != WAVEFRONT_DIM {
            return Err(MediumError::DimensionMismatch {
                expected: WAVEFRONT_DIM,
                actual: vector.len(),
            });
        }

        let id = self.store.insert(vector, content, importance);

        // Track energy added for wisdom calculation
        self.total_energy_added += importance;

        Ok(id)
    }

    /// Remove a wavefront from the medium using swap-remove (O(1) tensor op).
    pub fn remove_wavefront(&mut self, id: &Uuid) -> Result<bool, MediumError> {
        Ok(self.store.remove(id))
    }

    /// Compute effective strength of all wavefronts with temporal decay.
    ///
    /// Returns energy * exp(-decay_rate * age) for each wavefront.
    pub fn effective_strength(&self, now: Option<DateTime<Utc>>) -> Array1<f32> {
        let current_time = now.unwrap_or_else(Utc::now).timestamp_millis();
        let decay_rate = 0.001; // Per-day decay rate (half-life ~693 days)

        self.store.timestamps
            .iter()
            .enumerate()
            .map(|(i, &created_at)| {
                let age_days = ((current_time - created_at) as f64 / 86_400_000.0).max(0.0);
                let decay = (-decay_rate * age_days as f32).exp();
                self.store.energy[i] * decay
            })
            .collect()
    }

    /// Reset all wavefront energies to a target value (the "bias voltage").
    /// Use after dream over-dampening or migration to restore field potential.
    pub fn reset_energies(&mut self, target: f32) {
        for i in 0..self.wavefront_count() {
            self.store.energy[i] = target;
        }
    }

    /// Store a new memory using the encoding pipeline.
    ///
    /// This implements the interference-based storage where new waves interact with existing ones.
    /// After storage, dynamics are applied to let the medium settle.
    pub fn store(
        &mut self,
        content: &str,
        importance: f32,
        pipeline: &EncodingPipeline,
    ) -> Result<Uuid, MediumError> {
        // 1. Encode content to D-dimensional hypervector
        let vector = pipeline.encode_text(content).map_err(|e| {
            MediumError::Serialization(bincode::Error::from(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("encoding failed: {e}"),
            )))
        })?;

        // 2. Apply interference with existing wavefronts
        self.apply_interference(&vector, importance);

        // 3. Add wavefront to the medium
        let id = self.add_wavefront(&vector, content.to_string(), importance)?;

        // 4. Apply dynamics to let the medium settle after new addition
        self.apply_dynamics(0.1);

        Ok(id)
    }

    /// Store an audio memory using a pre-computed 296-dimensional audio vector.
    ///
    /// This projects the audio vector into the same 10,000-dimensional wavefront space
    /// as text memories using the audio-specific codebook. Cross-modal interference
    /// occurs naturally through the shared superposition space.
    ///
    /// # Arguments
    /// * `audio_vector` - 296-dimensional perceptual audio features from kannaka-ear
    /// * `content` - Content string (e.g., "HEAR:path/to/file.mp3" or "audio:description")
    /// * `importance` - Initial energy/amplitude (typically 0.0-1.0)
    ///
    /// # Returns
    /// UUID of the mediumd audio wavefront
    pub fn store_audio(
        &mut self,
        audio_vector: &[f32],
        content: &str,
        importance: f32,
    ) -> Result<Uuid, MediumError> {
        if audio_vector.len() != AUDIO_FEATURE_DIM {
            return Err(MediumError::DimensionMismatch {
                expected: AUDIO_FEATURE_DIM,
                actual: audio_vector.len(),
            });
        }

        // Project 296-dim audio vector into 10,000-dim wavefront space using audio codebook
        let wavefront_vector = self.audio_codebook.project(audio_vector);

        // Apply interference with existing wavefronts (cross-modal included)
        self.apply_interference(&wavefront_vector, importance);

        // Add wavefront to the medium
        let id = self.add_wavefront(&wavefront_vector, content.to_string(), importance)?;

        // Apply dynamics to let the medium settle after new addition
        self.apply_dynamics(0.1);

        Ok(id)
    }

    /// Store a visual memory using a pre-computed 320-dimensional visual vector.
    ///
    /// This projects the visual vector into the same 10,000-dimensional wavefront space
    /// as text and audio memories using the visual-specific codebook. Cross-modal
    /// interference occurs naturally through the shared superposition space.
    ///
    /// # Arguments
    /// * `visual_vector` - 320-dimensional perceptual visual features from kannaka-eye
    /// * `content` - Content string (e.g., "[SEE] filename.jpg | bytes | folds | fano=...")
    /// * `importance` - Initial energy/amplitude (typically 0.0-1.0)
    ///
    /// # Returns
    /// UUID of the mediumd visual wavefront
    pub fn store_visual(
        &mut self,
        visual_vector: &[f32],
        content: &str,
        importance: f32,
    ) -> Result<Uuid, MediumError> {
        if visual_vector.len() != VISUAL_FEATURE_DIM {
            return Err(MediumError::DimensionMismatch {
                expected: VISUAL_FEATURE_DIM,
                actual: visual_vector.len(),
            });
        }

        // Project 320-dim visual vector into 10,000-dim wavefront space using visual codebook
        let wavefront_vector = self.visual_codebook.project(visual_vector);

        // Apply interference with existing wavefronts (cross-modal included)
        self.apply_interference(&wavefront_vector, importance);

        // Add wavefront to the medium
        let id = self.add_wavefront(&wavefront_vector, content.to_string(), importance)?;

        // Apply dynamics to let the medium settle after new addition
        self.apply_dynamics(0.1);

        Ok(id)
    }

    /// Apply interference between new wavefront and existing medium.
    ///
    /// Constructive interference boosts energy of phase-aligned wavefronts.
    /// Destructive interference dampens phase-opposed wavefronts.
    fn apply_interference(&mut self, new_vector: &[f32], importance: f32) {
        if self.wavefront_count() == 0 {
            return; // No existing wavefronts to interfere with
        }

        // The incoming wavefront's phase: content-smooth born phase under the
        // belief substrate, else legacy phase-0. Used so interference is measured
        // against the NEW wavefront's ACTUAL phase, not a hardcoded 0.
        let belief = crate::medium::chiral::belief_phase_enabled();
        let new_phase = if belief {
            crate::medium::chiral::content_born_phase(new_vector)
        } else {
            0.0
        };

        // Compute dot products between new vector and all existing wavefronts
        for i in 0..self.wavefront_count() {
            let existing_vector = self.store.wavefronts.row(i);

            let dot_product: f32 = existing_vector
                .iter()
                .zip(new_vector.iter())
                .map(|(a, b)| a * b)
                .sum();

            // Phase difference affects interference pattern
            let phase_diff = (self.store.phase[i] - new_phase).cos();
            let interference = dot_product * phase_diff * importance * 0.1; // Scale interference

            // Apply constructive/destructive interference
            self.store.energy[i] = (self.store.energy[i] + interference).max(0.0); // Energy can't go negative

            // Legacy Kuramoto pull toward phase 0 — a constant synchronizer that
            // collapses the field to order≈1. DISABLED under the belief substrate:
            // heterogeneity must survive ingest; belief coherence is restored by
            // the coherence-gated dream/tick coupling, not by ingest sync.
            if !belief && dot_product.abs() > 0.5 {
                // High similarity threshold
                let coupling = 0.05;
                self.store.phase[i] += coupling * (0.0 - self.store.phase[i]).sin(); // Pull toward phase 0
            }
        }
    }

    /// Recall memories through resonance — query wave interferes with stored patterns.
    ///
    /// Now includes coherence expansion: after finding top-K results, includes
    /// high-coherence neighbors to capture associative recall patterns.
    ///
    /// Internally delegates to `recall_against(None, ...)` — i.e. score the full
    /// medium. For the sparse-attention path, callers pass an explicit candidate
    /// set via `recall_against(Some(beam), ...)` to get O(K) scoring instead of
    /// O(N), regardless of medium size.
    pub fn recall(
        &self,
        query: &str,
        top_k: usize,
        pipeline: &EncodingPipeline,
    ) -> Result<Vec<Resonance>, MediumError> {
        self.recall_against(None, query, top_k, pipeline)
    }

    /// All memory ids whose content folds onto Fano `line` (0..6), capped at
    /// `limit`. This is the **gravity-well lookup**: given a glyph's dominant
    /// line (e.g. from a kannaka-eye event), pull the same-line memories into
    /// the attention beam so subsequent recall is both sparse and on-theme.
    /// O(N) over metadata (one glyph encode per memory); scan stops at `limit`.
    #[cfg(feature = "glyph")]
    pub fn ids_by_fano_line(&self, line: u8, limit: usize) -> Vec<uuid::Uuid> {
        let mut out = Vec::new();
        for m in &self.store.metadata {
            if out.len() >= limit {
                break;
            }
            if crate::glyph_bridge::fano_line_of(&m.content) == line {
                out.push(m.id);
            }
        }
        out
    }

    /// Recall against an attention beam expressed as memory IDs.
    ///
    /// The kannaka-attention crate tracks beam membership in Uuids (stable
    /// across wavefront insertions). This helper resolves Uuids to internal
    /// indices and then runs the sparse path. Unknown ids are silently
    /// dropped — beam reconciliation lives in the attention crate.
    pub fn recall_against_ids(
        &self,
        beam: &[uuid::Uuid],
        query: &str,
        top_k: usize,
        pipeline: &EncodingPipeline,
    ) -> Result<Vec<Resonance>, MediumError> {
        let indices: Vec<usize> = beam.iter()
            .filter_map(|id| self.get_wavefront_index(id))
            .collect();
        if indices.is_empty() {
            // An empty beam after resolution would otherwise fall through to
            // full-medium recall, which silently defeats the sparsity. Be
            // explicit: empty beam in → empty result out.
            return Ok(Vec::new());
        }
        self.recall_against(Some(&indices), query, top_k, pipeline)
    }

    /// Recall against an explicit candidate set — the sparse-attention seam.
    ///
    /// `candidates`: when `Some(&[i])`, score only the wavefronts at these
    /// indices (the "attention beam" picked by the kannaka-attention crate's
    /// recency ring + log-stride snapshots + landmark exemplars). When `None`,
    /// fall back to scoring every wavefront — equivalent to the original
    /// `recall` semantics.
    ///
    /// Coherence expansion stays restricted to the same candidate set so a
    /// caller running with a tight beam doesn't accidentally pull in
    /// off-beam memories through the expansion side-channel.
    pub fn recall_against(
        &self,
        candidates: Option<&[usize]>,
        query: &str,
        top_k: usize,
        pipeline: &EncodingPipeline,
    ) -> Result<Vec<Resonance>, MediumError> {
        if self.wavefront_count() == 0 {
            return Ok(Vec::new());
        }

        // 1. Encode query as wave
        let query_vector = pipeline.encode_text(query).map_err(|e| {
            MediumError::Serialization(bincode::Error::from(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("query encoding failed: {e}"),
            )))
        })?;

        // Resolve the iteration set. `None` means "full medium" (original
        // behavior). A `Some` slice is the attention beam — typically 64-512
        // indices regardless of how many memories live in the medium.
        let all_indices: Vec<usize>;
        let cand_slice: &[usize] = match candidates {
            Some(c) if !c.is_empty() => c,
            _ => {
                all_indices = (0..self.wavefront_count()).collect();
                &all_indices
            }
        };

        // 2. Compute basic resonance pattern — matrix multiplication H @ q,
        // but only across the candidate set.
        let mut resonances: Vec<(usize, Resonance)> = Vec::with_capacity(cand_slice.len());
        let effective_strengths = self.effective_strength(None);
        // #362: `effective_strengths` is sized by `timestamps.len()`, which can
        // drift below `wavefront_count()` (the documented count=142/ts=139 Oracle
        // desync). Clamp the iteration to the smaller of the two so `i` can never
        // index `effective_strengths` out of bounds and panic mid-recall.
        let safe_count = self.wavefront_count().min(effective_strengths.len());

        // ── Glyph-gravity recall boost REMOVED (#837) ─────────────────────
        // KANNAKA_GLYPH_GRAVITY used to multiply same-Fano-line resonance by
        // (1 + gain) here and in the xi re-rank below. Measured across six
        // queries and four gains it never improved a rank, degraded half the
        // queries, and dropped one rank-1 answer out of the top 5: a 7-bucket
        // categorical prior applied multiplicatively cannot express "relevant
        // to this query" — boosting same-line results necessarily demotes the
        // ~6/7 of correct answers that land on a different line. The env var
        // no longer touches ranking anywhere; ADR-0047 (embedding-space query
        // gravity) is the designed successor.

        // Query born phase for phase-DIFFERENCE resonance. Recall must compare the
        // query's phase to each stored phase — using the stored phase ABSOLUTELY
        // (`store.phase[i].cos()`) negates any memory whose born phase lands in
        // (π/2, 3π/2), so under the belief substrate an exact match (phase π,
        // cos=-1) scored -1 and sorted LAST — inverting recall. Every other phase
        // interaction (apply_interference, dynamics) uses the difference, and
        // content_born_phase is documented "recall-safe" precisely on that basis.
        // Inert in the default text path (both phases 0 ⇒ cos(0)=1).
        let query_phase = if crate::medium::chiral::belief_phase_enabled() {
            crate::medium::chiral::content_born_phase(&query_vector)
        } else {
            0.0
        };

        for &i in cand_slice {
            if i >= safe_count { continue; } // guard against stale beam indices / section desync
            let wavefront = self.store.wavefronts.row(i);

            // Dot product (similarity)
            let similarity: f32 = wavefront
                .iter()
                .zip(query_vector.iter())
                .map(|(a, b)| a * b)
                .sum();

            // Modulate by wave dynamics (energy, phase, temporal decay)
            let effective_strength = effective_strengths[i];
            // Phase DIFFERENCE (constructive interference), not absolute phase.
            let phase_modulation = (self.store.phase[i] - query_phase).cos();
            let resonance_strength = similarity * effective_strength * phase_modulation;

            resonances.push((i, Resonance {
                id: self.store.metadata[i].id,
                content: self.store.metadata[i].content.clone(),
                similarity,
                resonance_strength,
                effective_strength,
            }));
        }

        // 3. Sort by raw resonance strength and take top 2*K for xi re-ranking.
        //
        // #699: on a facet-bearing corpus the raw pool is dominated by facet
        // clusters of a few strongly-resonating parents — a 2k pool collapsed
        // to ~10 distinct constellations for k=20, and lost parents were
        // absent from the pool entirely. Scale the rerank pool by the facet
        // over-fetch factor AND cap how many rows one constellation may
        // occupy, so pool slots buy distinct parents rather than sibling
        // facets. Both changes are gated on has_facets: an undecomposed
        // corpus keeps the byte-identical 2k pool.
        resonances.sort_by(|a, b| b.1.resonance_strength.total_cmp(&a.1.resonance_strength));
        let has_facets = self.has_facets();
        let rerank_pool = if has_facets {
            crate::facet::overfetch_pool(top_k).saturating_mul(2).max(top_k)
        } else {
            top_k.saturating_mul(2).max(top_k)
        };
        if has_facets {
            // Per-constellation cap: keep at most 2 rows per canonical id
            // (best + one spare for the xi re-rank to shuffle) before the
            // pool truncate. facet::resolve dedups to one later anyway.
            let mut per_parent: std::collections::HashMap<Uuid, usize> =
                std::collections::HashMap::new();
            resonances.retain(|(_, r)| {
                let canonical = self
                    .parent_of_facet(r.id)
                    .map(|(pid, _)| pid)
                    .unwrap_or(r.id);
                let n = per_parent.entry(canonical).or_insert(0);
                *n += 1;
                *n <= 2
            });
        }
        resonances.truncate(rerank_pool);
        if std::env::var("KANNAKA_RECALL_TRACE").is_ok() {
            for (i, r) in resonances.iter().take(6) {
                eprintln!("[recall-trace]   flat raw idx={} sim={:.4} rs={:.4} eff={:.4} {}",
                    i, r.similarity, r.resonance_strength, r.effective_strength,
                    r.content.chars().take(32).collect::<String>());
            }
        }

        // 4. Xi diversity re-ranking: boost candidates with distinct xi signatures
        let query_xi = compute_xi_signature(&query_vector);
        let initial_results: Vec<Resonance> = resonances
            .into_iter()
            .map(|(i, mut r)| {
                let wf_vec: Vec<f32> = self.store.wavefronts.row(i).to_vec();
                let wf_xi = compute_xi_signature(&wf_vec);
                let boosted_sim = if super::hemisphere::recall_xi_boost_enabled() {
                    xi_diversity_boost(r.similarity, &query_xi, &wf_xi)
                } else {
                    r.similarity.clamp(0.0, 1.0)
                };
                r.similarity = boosted_sim;
                r.resonance_strength = boosted_sim * r.effective_strength
                    * (self.store.phase[i] - query_phase).cos();
                r
            })
            .collect();

        // 5. Re-sort by boosted resonance and take the pool.
        //
        // ADR-0049: when the medium holds facets, several rows can collapse to a
        // single parent, so taking exactly `top_k` here would return fewer than
        // `top_k` distinct memories after resolution. Over-fetch in that case —
        // and ONLY in that case, so a corpus with no facets is byte-identical to
        // the pre-facet behaviour.
        let pool = if has_facets { crate::facet::overfetch_pool(top_k) } else { top_k };
        let mut sorted = initial_results;
        sorted.sort_by(|a, b| b.resonance_strength.total_cmp(&a.resonance_strength));
        sorted.truncate(pool);

        // 6. Coherence expansion — restricted to the candidate set when one was
        // provided, full medium otherwise. The restriction is what keeps a
        // tight beam tight; without it the expansion side-channel would pull
        // off-beam memories back in and defeat the sparsity win.
        if std::env::var("KANNAKA_RECALL_TRACE").is_ok() {
            for r in sorted.iter().take(6) {
                eprintln!("[recall-trace]   flat post-xi sim={:.4} rs={:.4} {}",
                    r.similarity, r.resonance_strength, r.content.chars().take(32).collect::<String>());
            }
        }
        let coherence_expansion_results = self.expand_with_coherence_in_set(&sorted, pool, candidates);
        if std::env::var("KANNAKA_RECALL_TRACE").is_ok() {
            for r in coherence_expansion_results.iter().take(8) {
                eprintln!("[recall-trace]   flat post-expand sim={:.4} rs={:.4} {}",
                    r.similarity, r.resonance_strength, r.content.chars().take(32).collect::<String>());
            }
        }

        // 7. ADR-0049 facet resolution: rewrite facets to their parents, dedup
        // by canonical id keeping the best score, truncate to top_k. Callers
        // observe the list this returns, so an N-facet parent takes exactly one
        // energy injection rather than N — see facet::resolve.
        if !has_facets {
            return Ok(coherence_expansion_results);
        }
        Ok(crate::facet::resolve(coherence_expansion_results, top_k, |id| {
            self.parent_of_facet(id)
        }))
    }

    /// Does this medium hold any facet rows? Cheap guard so an undecomposed
    /// corpus never pays for resolution or over-fetch.
    pub(crate) fn has_facets(&self) -> bool {
        self.store.metadata.iter().any(|m| m.is_facet)
    }

    /// Facet → parent `(id, content)`. `None` when `id` is not a facet, or when
    /// its parent is missing — the caller then surfaces the facet itself rather
    /// than dropping it or attributing it to a parent that is not there.
    pub(crate) fn parent_of_facet(&self, id: Uuid) -> Option<(Uuid, String)> {
        let m = self.store.metadata.iter().find(|m| m.id == id)?;
        if !m.is_facet {
            return None;
        }
        let pid = m.parent_id?;
        let parent = self.store.metadata.iter().find(|m| m.id == pid)?;
        Some((parent.id, parent.content.clone()))
    }

    /// Coherence expansion that respects an attention beam.
    ///
    /// When `candidates` is `Some`, neighbor scanning is restricted to that
    /// index set — keeps a sparse-attention recall path actually sparse.
    /// When `None`, behaves like the original `expand_with_coherence` (full
    /// medium scan).
    fn expand_with_coherence_in_set(
        &self,
        initial_results: &[Resonance],
        max_total: usize,
        candidates: Option<&[usize]>,
    ) -> Vec<Resonance> {
        match candidates {
            Some(c) if !c.is_empty() => {
                // Build a set for O(1) membership when scanning neighbors.
                let cand_set: std::collections::HashSet<usize> =
                    c.iter().copied().filter(|&i| i < self.wavefront_count()).collect();
                self.expand_with_coherence_filtered(initial_results, max_total, Some(&cand_set))
            }
            _ => self.expand_with_coherence_filtered(initial_results, max_total, None),
        }
    }

    /// Internal expansion — `allowed` gates which indices may be admitted as
    /// neighbors. `None` = no gate = original behavior.
    fn expand_with_coherence_filtered(
        &self,
        initial_results: &[Resonance],
        max_total: usize,
        allowed: Option<&std::collections::HashSet<usize>>,
    ) -> Vec<Resonance> {
        let coherence_matrix = self.coherence_matrix();
        let coherence_threshold = 0.3;
        let mut expanded_results = initial_results.to_vec();
        let mut added_ids = std::collections::HashSet::new();

        for result in initial_results {
            added_ids.insert(result.id);
        }

        for initial in initial_results {
            if let Some(result_idx) = self.get_wavefront_index(&initial.id) {
                let mut neighbor_candidates = Vec::new();

                for i in 0..self.wavefront_count() {
                    if i == result_idx || added_ids.contains(&self.store.metadata[i].id) {
                        continue;
                    }
                    if let Some(set) = allowed {
                        if !set.contains(&i) { continue; }
                    }

                    let coherence = coherence_matrix[[result_idx, i]].abs();
                    if coherence > coherence_threshold {
                        let expansion_strength = coherence * self.store.energy[i];
                        neighbor_candidates.push((i, expansion_strength, coherence));
                    }
                }

                neighbor_candidates.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

                let max_neighbors = 2;
                for (neighbor_idx, expansion_strength, coherence) in neighbor_candidates.into_iter().take(max_neighbors) {
                    if expanded_results.len() >= max_total { break; }
                    let neighbor_id = self.store.metadata[neighbor_idx].id;
                    if !added_ids.contains(&neighbor_id) {
                        expanded_results.push(Resonance {
                            id: neighbor_id,
                            content: self.store.metadata[neighbor_idx].content.clone(),
                            similarity: coherence,
                            resonance_strength: expansion_strength,
                            effective_strength: self.store.energy[neighbor_idx],
                        });
                        added_ids.insert(neighbor_id);
                    }
                }
            }
        }

        expanded_results.sort_by(|a, b| b.resonance_strength.total_cmp(&a.resonance_strength));
        expanded_results.truncate(max_total);
        expanded_results
    }

    /// Expand recall results with high-coherence neighbors.
    ///
    /// For each result, finds other wavefronts with coherence above threshold (0.3)
    /// and includes them weighted by coherence * energy.
    //
    // Currently unused — superseded by `expand_with_coherence_filtered`
    // which adds the candidate-set gate needed for the attention-beam
    // sparse-recall path. Kept for reference + as the docs-example of
    // the unfiltered expansion algorithm. If you find yourself wanting
    // unfiltered expansion in new code, just call
    // `expand_with_coherence_filtered(..., None)` instead.
    #[allow(dead_code)]
    fn expand_with_coherence(&self, initial_results: &[Resonance], max_total: usize) -> Vec<Resonance> {
        let coherence_matrix = self.coherence_matrix();
        let coherence_threshold = 0.3;
        let mut expanded_results = initial_results.to_vec();
        let mut added_ids = std::collections::HashSet::new();

        // Mark initial results as already added
        for result in initial_results {
            added_ids.insert(result.id);
        }

        // For each initial result, find high-coherence neighbors
        for initial in initial_results {
            if let Some(result_idx) = self.get_wavefront_index(&initial.id) {
                let mut neighbor_candidates = Vec::new();

                for i in 0..self.wavefront_count() {
                    if i == result_idx || added_ids.contains(&self.store.metadata[i].id) {
                        continue;
                    }

                    let coherence = coherence_matrix[[result_idx, i]].abs();
                    if coherence > coherence_threshold {
                        // Weight by coherence * energy
                        let expansion_strength = coherence * self.store.energy[i];

                        neighbor_candidates.push((i, expansion_strength, coherence));
                    }
                }

                // Sort neighbors by expansion strength
                neighbor_candidates.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

                // Add top neighbors (limit to avoid explosion)
                let max_neighbors = 2; // Limit neighbors per result
                for (neighbor_idx, expansion_strength, coherence) in neighbor_candidates.into_iter().take(max_neighbors) {
                    if expanded_results.len() >= max_total {
                        break;
                    }

                    let neighbor_id = self.store.metadata[neighbor_idx].id;
                    if !added_ids.contains(&neighbor_id) {
                        // Create resonance entry for the expanded neighbor
                        let neighbor_resonance = Resonance {
                            id: neighbor_id,
                            content: self.store.metadata[neighbor_idx].content.clone(),
                            similarity: coherence, // Use coherence as similarity proxy
                            resonance_strength: expansion_strength, // Coherence * energy
                            effective_strength: self.store.energy[neighbor_idx],
                        };

                        expanded_results.push(neighbor_resonance);
                        added_ids.insert(neighbor_id);
                    }
                }
            }
        }

        // Final sort by resonance strength and truncate to max_total
        expanded_results.sort_by(|a, b| b.resonance_strength.total_cmp(&a.resonance_strength));
        expanded_results.truncate(max_total);

        expanded_results
    }

    /// Relate two wavefronts via associative connection.
    ///
    /// Creates emergent association through the field by:
    /// 1. Creating a new associative wavefront = normalize(vec_a + vec_b)
    /// 2. Setting energy = average(energy_a, energy_b)
    /// 3. Nudging phases of a and b toward each other
    ///
    /// This replaces explicit skip links with field-based associations.
    pub fn relate_wavefronts(&mut self, idx_a: usize, idx_b: usize) -> Result<Uuid, MediumError> {
        if idx_a >= self.wavefront_count() || idx_b >= self.wavefront_count() || idx_a == idx_b {
            return Err(MediumError::DimensionMismatch { 
                expected: self.wavefront_count(), 
                actual: idx_a.max(idx_b) 
            });
        }

        // 1. Create associative wavefront by combining the two patterns
        let vec_a = self.store.wavefronts.row(idx_a);
        let vec_b = self.store.wavefronts.row(idx_b);
        
        let mut associative_vector = Vec::with_capacity(WAVEFRONT_DIM);
        for (a, b) in vec_a.iter().zip(vec_b.iter()) {
            associative_vector.push(a + b);
        }

        // Normalize the associative vector
        let norm: f32 = associative_vector.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 1e-6 {
            for x in &mut associative_vector {
                *x /= norm;
            }
        } else {
            // Degenerate case: vec_a ≈ -vec_b, so their sum — and its 0.5
            // average, which is collinear with the sum — is ~0 and has no
            // meaningful direction to normalize. Fall back to vec_a's
            // direction as the association anchor.
            //
            // NOTE: the previous code *pushed* a second DIM-length run onto
            // the already-full `associative_vector`, producing a 2×DIM vector
            // that `add_wavefront` then rejected with DimensionMismatch — so
            // relating two phase-opposed wavefronts always errored instead of
            // forming an association. Overwrite in place to keep length == DIM.
            for (slot, a) in associative_vector.iter_mut().zip(vec_a.iter()) {
                *slot = *a;
            }
        }

        // 2. Set energy as average of the two wavefronts
        let associative_energy = (self.store.energy[idx_a] + self.store.energy[idx_b]) / 2.0;
        
        // 3. Create content describing the association
        let content_a = &self.store.metadata[idx_a].content;
        let content_b = &self.store.metadata[idx_b].content;
        let associative_content = format!(
            "ASSOCIATION: [{}] <-> [{}]", 
            content_a.chars().take(50).collect::<String>(),
            content_b.chars().take(50).collect::<String>()
        );

        // 4. Add the associative wavefront to the medium
        let associative_id = self.add_wavefront(&associative_vector, associative_content, associative_energy)?;

        // 5. Nudge phases of original wavefronts toward each other (mutual alignment)
        let phase_coupling_strength = 0.15; // Stronger than normal coupling for explicit relations
        
        let phase_a = self.store.phase[idx_a];
        let phase_b = self.store.phase[idx_b];
        
        // Nudge A toward B
        let phase_diff_a = phase_b - phase_a;
        self.store.phase[idx_a] += phase_coupling_strength * phase_diff_a.sin();
        
        // Nudge B toward A  
        let phase_diff_b = phase_a - phase_b;
        self.store.phase[idx_b] += phase_coupling_strength * phase_diff_b.sin();

        // 6. Apply light dynamics to let the field settle
        self.apply_dynamics(0.05);

        Ok(associative_id)
    }
}
