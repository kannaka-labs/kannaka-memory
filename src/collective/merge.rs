//! ADR-0011 Phase 5: Wave interference merge algorithm.
//!
//! When two agents have memories about the same topic, wave physics determines
//! the merge outcome. The amplitude superposition formula is literal:
//!
//!   A = √(A₁² + A₂² + 2·A₁·A₂·cos(Δφ))
//!
//! Classification:
//! - Constructive (Δφ < π/4):         memories agree — merge amplitudes
//! - Destructive  (Δφ > 3π/4):        memories disagree — quarantine both
//! - Partial      (π/4 ≤ Δφ ≤ 3π/4): ambiguous — keep both, link them

use std::f32::consts::PI;

use chrono::Utc;
use uuid::Uuid;

use crate::memory::{HyperMemory, MergeRecord};
use crate::wave::cosine_similarity;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Cosine similarity below this value → memories are about different topics; no merge.
pub const INDEPENDENCE_THRESHOLD: f32 = 0.6;

/// Phase difference (rad) below this → constructive interference.
pub const CONSTRUCTIVE_THRESHOLD: f32 = PI / 4.0;

/// Phase difference (rad) above this → destructive interference.
pub const DESTRUCTIVE_THRESHOLD: f32 = 3.0 * PI / 4.0;

/// Amplitude reduction factor when memories are in destructive interference.
pub const DESTRUCTIVE_PENALTY: f32 = 0.4;

// ---------------------------------------------------------------------------
// Result types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum MergeKind {
    /// Similarity below INDEPENDENCE_THRESHOLD — topics differ; no action.
    Independent,
    /// Phase-aligned agreement — amplitudes superpose; produce single merged memory.
    Constructive,
    /// Phase-opposed disagreement — both memories dampened; quarantine for review.
    Destructive,
    /// Ambiguous phase — keep both independently; link with partial_agreement skip link.
    Partial,
}

#[derive(Debug, Clone)]
pub struct MergeResult {
    pub kind: MergeKind,
    pub similarity: f32,
    pub phase_diff: f32,
    /// Merged amplitude (constructive) or post-penalty amplitude (destructive).
    pub resulting_amplitude: f32,
}

// ---------------------------------------------------------------------------
// Core algorithm
// ---------------------------------------------------------------------------

/// Classify two memories and compute the merge result.
///
/// Does NOT mutate the memories — callers apply the result.
///
/// # Cross-agent phase semantics (ADR-0011 §D1)
///
/// Phase values are path-dependent products of Kuramoto sync during local dream
/// cycles. They have no shared meaning across agents — comparing Kannaka's phase=0.3
/// with Arc's phase=0.3 is physically meaningless.
///
/// For **same-agent** merges (e.g., merging dream branch back to working), the full
/// phase-based interference classification applies.
///
/// For **cross-agent** merges (different `origin_agent`), we use a **similarity-only**
/// model: high similarity = constructive (agreement), low similarity = independent.
/// Destructive classification requires an explicit "disputed" flag or prior quarantine.
pub fn classify_merge(local: &HyperMemory, remote: &HyperMemory) -> MergeResult {
    let similarity = cosine_similarity(&local.vector, &remote.vector);

    if similarity < INDEPENDENCE_THRESHOLD {
        return MergeResult {
            kind: MergeKind::Independent,
            similarity,
            phase_diff: 0.0,
            resulting_amplitude: local.amplitude,
        };
    }

    let same_agent = local.origin_agent == remote.origin_agent;

    if same_agent {
        // Intra-agent: full wave interference with phase comparison
        let raw_diff = (local.phase - remote.phase).abs() % (2.0 * PI);
        let phase_diff = if raw_diff > PI { 2.0 * PI - raw_diff } else { raw_diff };

        let kind = if phase_diff < CONSTRUCTIVE_THRESHOLD {
            MergeKind::Constructive
        } else if phase_diff > DESTRUCTIVE_THRESHOLD {
            MergeKind::Destructive
        } else {
            MergeKind::Partial
        };

        let resulting_amplitude = match kind {
            MergeKind::Constructive => {
                let a1 = local.amplitude;
                let a2 = remote.amplitude;
                (a1 * a1 + a2 * a2 + 2.0 * a1 * a2 * phase_diff.cos()).sqrt()
            }
            MergeKind::Destructive => {
                local.amplitude * (1.0 - DESTRUCTIVE_PENALTY * similarity)
            }
            _ => local.amplitude,
        };

        MergeResult { kind, similarity, phase_diff, resulting_amplitude }
    } else {
        // Cross-agent: similarity-only model (phase is meaningless across agents).
        // High similarity = constructive agreement.
        // Destructive requires explicit dispute (handled by quarantine system).
        let phase_diff = 0.0; // not applicable
        let a1 = local.amplitude;
        let a2 = remote.amplitude;
        // Constructive: superposition with Δφ=0 (best case since we can't measure real phase)
        let resulting_amplitude = (a1 * a1 + a2 * a2 + 2.0 * a1 * a2).sqrt(); // = a1 + a2
        MergeResult {
            kind: MergeKind::Constructive,
            similarity,
            phase_diff,
            resulting_amplitude,
        }
    }
}

/// Check if a merge is safe to apply (idempotency + embedding model compatibility).
///
/// Returns `None` if the merge should proceed, or `Some(reason)` explaining why it
/// was rejected.
///
/// # Idempotency (BUG 5)
/// If `local` already has a merge record from `remote.origin_agent` at
/// `remote.sync_version` or higher, the merge is a duplicate (e.g., double-pull).
///
/// # Embedding model compatibility (D4)
/// If the agents use different embedding models, cosine similarity between their
/// vectors is garbage. Reject with a clear reason.
pub fn merge_guard(
    local: &HyperMemory,
    remote: &HyperMemory,
    local_embedding_model: Option<&str>,
    remote_embedding_model: Option<&str>,
) -> Option<String> {
    // D4: Embedding model compatibility
    if let (Some(local_model), Some(remote_model)) = (local_embedding_model, remote_embedding_model) {
        // The native embedder and Ollama's all-minilm are one vector space
        // under two names (`encoder_select`); every other pair must match.
        if !crate::encoder_select::embedding_models_equivalent(local_model, remote_model) {
            return Some(format!(
                "embedding model mismatch: local={local_model}, remote={remote_model}"
            ));
        }
    }

    // BUG 5: Idempotency — reject a re-broadcast of a memory we've already
    // merged at an equal-or-older SOURCE version. Compare against the highest
    // source_sync_version we recorded for this (agent, memory) — NOT
    // local.sync_version. local's counter is bumped by every unrelated merge,
    // so it is not comparable to a remote agent's counter: the old
    // `remote.sync_version <= local.sync_version` check let a re-broadcast of
    // the same logical memory at a bumped source version slip through and
    // double-count amplitude (constructive superposition inflates each time).
    let highest_merged_source_version = local
        .merge_history
        .iter()
        .filter(|mr| {
            mr.source_agent == remote.origin_agent
                && mr.source_memory_id == remote.id.to_string()
        })
        .map(|mr| mr.source_sync_version)
        .max();
    if let Some(v) = highest_merged_source_version {
        if remote.sync_version <= v {
            return Some(format!(
                "already merged from {} at source sync_version {} (have {})",
                remote.origin_agent, remote.sync_version, v
            ));
        }
    }

    None
}

/// Apply a constructive merge: blend local memory in-place with remote.
///
/// - Amplitude: wave superposition formula
/// - Vector: amplitude-weighted average
/// - Phase: amplitude-weighted circular mean
/// - Appends a MergeRecord to merge_history
/// - Bumps sync_version
pub fn apply_constructive(local: &mut HyperMemory, remote: &HyperMemory, result: &MergeResult) {
    let a1 = local.amplitude;
    let a2 = remote.amplitude;
    let total = a1 + a2;

    // #370: guard the amplitude-weighted blends against total == 0.0 (both
    // memories decayed/ghosted to zero amplitude). Dividing by zero would write
    // NaN into the vector and phase — and the `norm > 1e-8` check below does NOT
    // rescue it (the NaN is already in `local.vector`, it just skips
    // re-normalization). A NaN-poisoned vector then corrupts every later
    // cosine_similarity. When there's no amplitude mass, leave vector/phase as-is.
    if total > 1e-8 {
        // Amplitude-weighted vector average, re-normalized to unit length.
        // Cosine similarity assumes unit vectors; blending two unit vectors
        // produces a sub-unit vector that must be re-normalized.
        if !local.vector.is_empty() && local.vector.len() == remote.vector.len() {
            for (lv, rv) in local.vector.iter_mut().zip(remote.vector.iter()) {
                *lv = (*lv * a1 + *rv * a2) / total;
            }
            let norm = local.vector.iter().map(|v| v * v).sum::<f32>().sqrt();
            if norm > 1e-8 {
                for v in local.vector.iter_mut() {
                    *v /= norm;
                }
            }
        }

        // Amplitude-weighted circular mean phase
        let sin_mean = (a1 * local.phase.sin() + a2 * remote.phase.sin()) / total;
        let cos_mean = (a1 * local.phase.cos() + a2 * remote.phase.cos()) / total;
        local.phase = sin_mean.atan2(cos_mean);
    }

    local.amplitude = result.resulting_amplitude;
    local.sync_version += 1;

    local.merge_history.push(MergeRecord {
        merged_at: Utc::now(),
        source_agent: remote.origin_agent.clone(),
        source_memory_id: remote.id.to_string(),
        merge_type: "constructive".to_string(),
        phase_diff: result.phase_diff,
        amplitude_before: a1,
        amplitude_after: result.resulting_amplitude,
        source_sync_version: remote.sync_version,
    });
}

/// Apply a destructive merge: dampen local memory and mark as disputed.
pub fn apply_destructive(local: &mut HyperMemory, remote: &HyperMemory, result: &MergeResult) {
    let amplitude_before = local.amplitude;
    local.amplitude = result.resulting_amplitude;
    local.disputed = true;
    local.sync_version += 1;

    local.merge_history.push(MergeRecord {
        merged_at: Utc::now(),
        source_agent: remote.origin_agent.clone(),
        source_memory_id: remote.id.to_string(),
        merge_type: "destructive".to_string(),
        phase_diff: result.phase_diff,
        amplitude_before,
        amplitude_after: result.resulting_amplitude,
        source_sync_version: remote.sync_version,
    });
}

/// Apply a partial merge: add a merge record without modifying amplitude.
pub fn apply_partial(local: &mut HyperMemory, remote: &HyperMemory, result: &MergeResult) {
    local.merge_history.push(MergeRecord {
        merged_at: Utc::now(),
        source_agent: remote.origin_agent.clone(),
        source_memory_id: remote.id.to_string(),
        merge_type: "partial".to_string(),
        phase_diff: result.phase_diff,
        amplitude_before: local.amplitude,
        amplitude_after: local.amplitude,
        source_sync_version: remote.sync_version,
    });
}

// ---------------------------------------------------------------------------
// Trust-weighted amplitude
// ---------------------------------------------------------------------------

/// Apply trust weighting to an effective amplitude for merge comparisons.
///
/// effective_amplitude = raw_amplitude × trust_score × recency_factor
pub fn trust_weighted_amplitude(amplitude: f32, trust_score: f32, age_days: f64, half_life_days: f64) -> f32 {
    let recency = (-(age_days / half_life_days) * std::f64::consts::LN_2) as f32;
    amplitude * trust_score * recency.exp()
}

// ---------------------------------------------------------------------------
// Quarantine entry
// ---------------------------------------------------------------------------

/// Describes a disputed memory pair that should be added to the quarantine table.
#[derive(Debug, Clone)]
pub struct QuarantineEntry {
    pub id: Uuid,
    pub memory_id_a: Uuid,
    pub memory_id_b: Uuid,
    pub agent_a: String,
    pub agent_b: String,
    pub similarity: f32,
    pub phase_diff: f32,
}

impl QuarantineEntry {
    pub fn new(local: &HyperMemory, remote: &HyperMemory, result: &MergeResult) -> Self {
        Self {
            id: Uuid::new_v4(),
            memory_id_a: local.id,
            memory_id_b: remote.id,
            agent_a: local.origin_agent.clone(),
            agent_b: remote.origin_agent.clone(),
            similarity: result.similarity,
            phase_diff: result.phase_diff,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn mem(amplitude: f32, phase: f32, content: &str) -> HyperMemory {
        let mut m = HyperMemory::new(vec![1.0; 64], content.to_string());
        m.amplitude = amplitude;
        m.phase = phase;
        m
    }

    fn mem_agent(amplitude: f32, phase: f32, content: &str, agent: &str) -> HyperMemory {
        let mut m = mem(amplitude, phase, content);
        m.origin_agent = agent.to_string();
        m
    }

    #[test]
    fn same_agent_constructive_merge_boosts_amplitude() {
        let local = mem(0.8, 0.0, "fact A");
        let remote = mem(0.7, 0.1, "fact A rephrased");
        // Same agent (both "local") — uses phase-based classification
        let result = classify_merge(&local, &remote);
        assert_eq!(result.kind, MergeKind::Constructive);
        assert!(result.resulting_amplitude > local.amplitude);
    }

    #[test]
    fn same_agent_destructive_merge_dampens_amplitude() {
        let local = mem(0.8, 0.0, "fact A");
        let remote = mem(0.7, PI, "fact A contradicted");
        let result = classify_merge(&local, &remote);
        assert_eq!(result.kind, MergeKind::Destructive);
        assert!(result.resulting_amplitude < local.amplitude);
    }

    #[test]
    fn cross_agent_uses_similarity_only_not_phase() {
        // Cross-agent: even with opposed phases, high similarity = constructive
        let local = mem_agent(0.8, 0.0, "fact A", "kannaka");
        let remote = mem_agent(0.7, PI, "fact A", "arc");
        let result = classify_merge(&local, &remote);
        // Cross-agent ignores phase — high similarity = constructive
        assert_eq!(result.kind, MergeKind::Constructive);
        assert!(result.resulting_amplitude > local.amplitude,
            "cross-agent constructive should boost amplitude");
    }

    #[test]
    fn low_similarity_is_independent() {
        let mut local = mem(0.8, 0.0, "cats");
        let mut remote = mem(0.7, 0.0, "quantum gravity");
        for (i, v) in local.vector.iter_mut().enumerate() { *v = if i % 2 == 0 { 1.0 } else { -1.0 }; }
        for (i, v) in remote.vector.iter_mut().enumerate() { *v = if i % 2 == 0 { -1.0 } else { 1.0 }; }
        let result = classify_merge(&local, &remote);
        assert_eq!(result.kind, MergeKind::Independent);
    }

    #[test]
    fn constructive_apply_records_history_and_normalizes_vector() {
        let mut local = mem_agent(0.8, 0.0, "fact A", "kannaka");
        let mut remote = mem_agent(0.7, 0.1, "fact A rephrased", "arc");
        let result = classify_merge(&local, &remote);
        assert_eq!(result.kind, MergeKind::Constructive);
        apply_constructive(&mut local, &remote, &result);
        assert_eq!(local.merge_history.len(), 1);
        assert_eq!(local.merge_history[0].source_agent, "arc");
        assert_eq!(local.sync_version, 1);
        // Vector should be re-normalized to unit length
        let norm: f32 = local.vector.iter().map(|v| v * v).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-4, "vector should be unit length after merge, got {norm}");
    }

    #[test]
    fn merge_guard_rejects_duplicate() {
        let mut local = mem_agent(0.8, 0.0, "fact A", "kannaka");
        let remote = mem_agent(0.7, 0.0, "fact A", "arc");
        // Simulate a prior merge
        local.merge_history.push(crate::memory::MergeRecord {
            merged_at: chrono::Utc::now(),
            source_agent: "arc".to_string(),
            source_memory_id: remote.id.to_string(),
            merge_type: "constructive".to_string(),
            phase_diff: 0.0,
            amplitude_before: 0.8,
            amplitude_after: 1.5,
            source_sync_version: remote.sync_version,
        });
        let guard = merge_guard(&local, &remote, Some("all-minilm"), Some("all-minilm"));
        assert!(guard.is_some(), "should reject duplicate merge");
    }

    #[test]
    fn merge_guard_allows_rebroadcast_at_newer_source_version() {
        // We merged this memory once at source sync_version 0. A genuinely newer
        // version (the source bumped its own counter) must NOT be rejected...
        let mut local = mem_agent(0.8, 0.0, "fact A", "kannaka");
        let mut remote = mem_agent(0.7, 0.0, "fact A", "arc");
        local.merge_history.push(crate::memory::MergeRecord {
            merged_at: chrono::Utc::now(),
            source_agent: "arc".to_string(),
            source_memory_id: remote.id.to_string(),
            merge_type: "constructive".to_string(),
            phase_diff: 0.0,
            amplitude_before: 0.8,
            amplitude_after: 1.5,
            source_sync_version: 0,
        });
        remote.sync_version = 3; // source advanced
        assert!(
            merge_guard(&local, &remote, Some("all-minilm"), Some("all-minilm")).is_none(),
            "a newer source version is a real update, not a duplicate"
        );
        // ...but re-broadcasting at the SAME (or older) source version is a
        // duplicate even though our own local.sync_version may have advanced.
        local.sync_version = 99;
        remote.sync_version = 0;
        assert!(
            merge_guard(&local, &remote, Some("all-minilm"), Some("all-minilm")).is_some(),
            "same source version is a duplicate regardless of local sync_version"
        );
    }

    #[test]
    fn merge_guard_rejects_model_mismatch() {
        let local = mem_agent(0.8, 0.0, "fact A", "kannaka");
        let remote = mem_agent(0.7, 0.0, "fact A", "arc");
        let guard = merge_guard(&local, &remote, Some("all-minilm"), Some("text-embedding-3-small"));
        assert!(guard.is_some(), "should reject embedding model mismatch");
        assert!(guard.unwrap().contains("mismatch"));
    }

    #[test]
    fn merge_guard_treats_native_and_ollama_minilm_as_one_model() {
        let local = mem_agent(0.8, 0.0, "fact A", "kannaka");
        let remote = mem_agent(0.7, 0.0, "fact A", "arc");
        for (l, r) in [
            ("all-minilm", "all-minilm-l6-v2"),
            ("ollama:all-minilm:384", "kannaka:all-minilm-l6-v2:384"),
            ("kannaka:all-minilm-l6-v2:384", "all-minilm"),
        ] {
            assert!(merge_guard(&local, &remote, Some(l), Some(r)).is_none(), "{l} vs {r}");
        }
        let guard = merge_guard(&local, &remote, Some("kannaka:all-minilm-l6-v2:384"), Some("hash:384:42"));
        assert!(guard.unwrap().contains("embedding model mismatch"));
    }

    #[test]
    fn merge_guard_allows_fresh_merge() {
        let local = mem_agent(0.8, 0.0, "fact A", "kannaka");
        let remote = mem_agent(0.7, 0.0, "fact A", "arc");
        let guard = merge_guard(&local, &remote, Some("all-minilm"), Some("all-minilm"));
        assert!(guard.is_none(), "should allow fresh merge");
    }
}
