//! Multi-agent synchronization: Kuramoto coupling, phase state export/import, associations.

use std::collections::HashMap;

use chrono::Utc;
use ndarray::s;
use uuid::Uuid;

use super::Medium;
use super::types::*;

/// Blake3 hash of `content` packed into a u64 (first 8 bytes, little-endian).
/// Used to match wavefronts across agents without sharing full content.
fn content_hash(content: &str) -> u64 {
    let mut hasher = blake3::Hasher::new();
    hasher.update(content.as_bytes());
    let hash_bytes = hasher.finalize();
    u64::from_le_bytes([
        hash_bytes.as_bytes()[0],
        hash_bytes.as_bytes()[1],
        hash_bytes.as_bytes()[2],
        hash_bytes.as_bytes()[3],
        hash_bytes.as_bytes()[4],
        hash_bytes.as_bytes()[5],
        hash_bytes.as_bytes()[6],
        hash_bytes.as_bytes()[7],
    ])
}

impl Medium {
    /// Kuramoto coupling between two agent media
    ///
    /// For each wavefront in self, finds the most phase-coherent wavefront in other
    /// (by dot product similarity above threshold). For matched pairs:
    /// - delta_phi_i = coupling * sin(phi_other - phi_self)  (Kuramoto phase coupling)
    /// - energy_self += coupling * energy_other * coherence  (amplitude reinforcement)
    ///
    /// # Arguments
    /// * `other` - The other agent's medium to sync with
    /// * `coupling` - Coupling strength (typically 0.0-1.0)
    pub fn sync_with(&mut self, other: &Medium, coupling: f32) {
        if self.wavefront_count() == 0 || other.wavefront_count() == 0 {
            return;
        }

        let threshold = 0.5; // Minimum dot product for phase coherence

        for i in 0..self.wavefront_count() {
            let self_wavefront = self.store.wavefronts.row(i);
            let mut best_match_idx = None;
            let mut best_coherence = threshold;

            // Find most phase-coherent wavefront in other medium
            for j in 0..other.wavefront_count() {
                let other_wavefront = other.store.wavefronts.row(j);

                // Compute dot product similarity
                let dot_product: f32 = self_wavefront
                    .iter()
                    .zip(other_wavefront.iter())
                    .map(|(a, b)| a * b)
                    .sum();

                if dot_product.abs() > best_coherence {
                    best_coherence = dot_product.abs();
                    best_match_idx = Some(j);
                }
            }

            // Apply Kuramoto coupling if we found a good match
            if let Some(j) = best_match_idx {
                let phase_diff = other.store.phase[j] - self.store.phase[i];

                // Kuramoto phase coupling: delta_phi_i = coupling * sin(phi_other - phi_self)
                let delta_phase = coupling * phase_diff.sin();
                self.store.phase[i] += delta_phase;

                // Amplitude reinforcement: energy_self += coupling * energy_other * coherence
                let amplitude_boost = coupling * other.store.energy[j] * best_coherence;
                self.store.energy[i] += amplitude_boost * 0.1; // Scale down to prevent runaway

                // #997: the ceiling is ENERGY_CAP, not 10.0. This clamp predates
                // the cap and kept its own private limit, so swarm coupling
                // could lift a local memory five times past the bound every
                // other write path enforces — and a PEER's energy is the
                // multiplier here, so this is the ceiling that most needs to be
                // one we control.
                self.store.energy[i] = self.store.energy[i].clamp(0.001, ENERGY_CAP);
            }
        }
    }

    /// Export lightweight phase state for gossip
    ///
    /// Returns a lightweight snapshot containing just phase vectors, energy vectors,
    /// and content hashes for matching across agents without sharing full content.
    ///
    /// # Arguments
    /// * `agent_id` - Identifier for the agent exporting this state
    pub fn export_phase_state(&self, agent_id: &str) -> PhaseState {
        let phases = self.store.phase.slice(s![..self.store.len]).to_vec();
        let energies = self.store.energy.slice(s![..self.store.len]).to_vec();

        // Compute content hashes for matching (blake3 hash of content text)
        let content_hashes = self
            .store.metadata
            .iter()
            .map(|meta| content_hash(&meta.content))
            .collect();

        PhaseState {
            phases,
            energies,
            content_hashes,
            agent_id: agent_id.to_string(),
            timestamp: Utc::now(),
        }
    }

    /// Import and apply remote phase state
    ///
    /// Applies remote phases from another agent by finding content matches
    /// and applying Kuramoto coupling.
    ///
    /// # Arguments
    /// * `remote` - Phase state from another agent
    /// * `coupling` - Coupling strength for applying remote phases
    pub fn import_phase_state(&mut self, remote: &PhaseState, coupling: f32) {
        if self.wavefront_count() == 0 || remote.phases.is_empty() {
            return;
        }

        // Build hash lookup for our content
        let mut our_hash_to_index = HashMap::new();
        for (i, meta) in self.store.metadata.iter().enumerate() {
            our_hash_to_index.insert(content_hash(&meta.content), i);
        }

        // Apply coupling for matched content
        for (remote_idx, &remote_hash) in remote.content_hashes.iter().enumerate() {
            if let Some(&our_idx) = our_hash_to_index.get(&remote_hash) {
                // Found matching content - apply Kuramoto coupling.
                // Use .get() instead of direct indexing: PhaseState is Deserialize and
                // travels over the network, so malformed payloads (version skew, truncation)
                // could have content_hashes longer than phases/energies.
                let Some(&remote_phase) = remote.phases.get(remote_idx) else { continue };
                let Some(&remote_energy) = remote.energies.get(remote_idx) else { continue };
                let phase_diff = remote_phase - self.store.phase[our_idx];

                // Apply phase coupling
                let delta_phase = coupling * phase_diff.sin();
                self.store.phase[our_idx] += delta_phase;

                // Apply energy coupling (amplitude reinforcement)
                let energy_boost = coupling * remote_energy * 0.1; // Scale down
                // #997: ENERGY_CAP, not 10.0 — see the note in `sync_with`. This
                // is the same coupling reached over the wire, where the
                // multiplier is a value we did not compute.
                self.store.energy[our_idx] =
                    (self.store.energy[our_idx] + energy_boost).clamp(0.001, ENERGY_CAP);
            }
        }
    }

    /// Find memories associated with a given wavefront through emergent phase coherence
    ///
    /// This replaces explicit skip links with associations that emerge naturally
    /// from the physics of the medium. High coherence = strong association.
    ///
    /// # Arguments
    /// * `id` - ID of the wavefront to find associations for
    /// * `top_k` - Maximum number of associations to return
    ///
    /// # Returns
    /// Vector of (UUID, coherence_strength) pairs, sorted by strength descending
    pub fn find_associated(&self, id: Uuid, top_k: usize) -> Vec<(Uuid, f32)> {
        let index = match self.store.id_to_index.get(&id) {
            Some(&idx) => idx,
            None => return Vec::new(),
        };

        let coherence = self.coherence_matrix();
        let mut associations = Vec::new();

        for j in 0..self.wavefront_count() {
            if j != index {
                let strength = coherence[[index, j]];
                associations.push((self.store.metadata[j].id, strength));
            }
        }

        // Sort by coherence strength descending
        associations
            .sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        // Return top-k
        associations.truncate(top_k);
        associations
    }
}
