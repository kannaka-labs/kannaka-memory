//! ADR-0037 L7 instrument — scoring the belief falsification predictions.
//!
//! The README's falsifiability clause: a core only earns the word "belief" if
//! it maps to a recallable content cluster AND its dynamics predict:
//!   1. core stability  ⇒ recall reliability,
//!   2. core merge      ⇒ a consolidation event,
//!   3. shared cores    ⇒ swarm agreement.
//!
//! This module turns each prediction into a [0,1] score (1 = the prediction
//! holds in the observed data, 0 = the data anti-predicts, 0.5 = no evidence)
//! plus a weighted `1 - score` fitness — lower is better, matching the
//! research ladder's convention (`swarm_fitness::l6_fitness`). Pure functions
//! only: the L7 research session (`research --level 7`) supplies observed
//! (stability, recall), (merge epoch, absorb epoch), and (shared, agreement)
//! samples; nothing here touches the medium.

/// Weights for the three predictions. Stability⇒recall is the primary
/// single-agent claim; shared⇒agreement is the swarm claim (Track-D's whole
/// point); merge⇒consolidation is the noisiest observable (absorb events are
/// coarse), so it carries the least weight.
#[derive(Debug, Clone, Copy)]
pub struct L7Weights {
    pub stability_recall: f32,
    pub merge_consolidation: f32,
    pub shared_agreement: f32,
}

impl Default for L7Weights {
    fn default() -> Self {
        Self { stability_recall: 0.40, merge_consolidation: 0.25, shared_agreement: 0.35 }
    }
}

/// Average-rank (ties share the mean of their positions) — standard Spearman
/// pre-processing so plateaus of equal stability don't fabricate order.
fn average_ranks(values: &[f32]) -> Vec<f32> {
    let n = values.len();
    let mut idx: Vec<usize> = (0..n).collect();
    idx.sort_by(|&a, &b| values[a].partial_cmp(&values[b]).unwrap_or(std::cmp::Ordering::Equal));
    let mut ranks = vec![0.0f32; n];
    let mut i = 0;
    while i < n {
        let mut j = i;
        while j + 1 < n && values[idx[j + 1]] == values[idx[i]] {
            j += 1;
        }
        // positions i..=j (0-based) share the average 1-based rank
        let avg = (i + j) as f32 / 2.0 + 1.0;
        for &k in &idx[i..=j] {
            ranks[k] = avg;
        }
        i = j + 1;
    }
    ranks
}

/// Spearman rank correlation of paired samples, mapped from [-1, 1] to [0, 1].
/// Fewer than 3 pairs, or zero variance on either side, is NO EVIDENCE — the
/// prediction is neither supported nor refuted — and returns exactly 0.5.
pub fn rank_correlation_01(pairs: &[(f32, f32)]) -> f32 {
    if pairs.len() < 3 {
        return 0.5;
    }
    let (xs, ys): (Vec<f32>, Vec<f32>) = pairs.iter().copied().unzip();
    let rx = average_ranks(&xs);
    let ry = average_ranks(&ys);
    let n = rx.len() as f32;
    let mx = rx.iter().sum::<f32>() / n;
    let my = ry.iter().sum::<f32>() / n;
    let mut cov = 0.0f32;
    let mut vx = 0.0f32;
    let mut vy = 0.0f32;
    for (rx_i, ry_i) in rx.iter().zip(ry.iter()) {
        let dx = *rx_i - mx;
        let dy = *ry_i - my;
        cov += dx * dy;
        vx += dx * dx;
        vy += dy * dy;
    }
    if vx <= f32::EPSILON || vy <= f32::EPSILON {
        return 0.5; // a constant side carries no ordering information
    }
    let rho = (cov / (vx.sqrt() * vy.sqrt())).clamp(-1.0, 1.0);
    (rho + 1.0) / 2.0
}

/// Prediction 1 — core stability ⇒ recall reliability. Each pair is one
/// belief track: (stability = appearances / epochs, recall rate of the content
/// domain that track organizes). Holds when more-stable cores rank with
/// more-reliable recall.
pub fn stability_recall_score(pairs: &[(f32, f32)]) -> f32 {
    rank_correlation_01(pairs)
}

/// Prediction 2 — core merge ⇒ a consolidation event. `merge_epochs` are the
/// epochs where two tracks fused (one ended while a same-charge,
/// content-similar track survived); `absorb_epochs` are the epochs whose dream
/// reported wavefronts dissolved (the consolidation observable). The score is
/// the fraction of merges with an absorb within ±`window` epochs.
///
/// No merges observed = no evidence = 0.5. No absorb events observed AT ALL is
/// ALSO no evidence: a session whose substrate never dissolved anything has a
/// blind observable, and merges against a blind observable neither support nor
/// refute the implication. (Verified the hard way: ChiralMedium's holistic
/// energy floor (0.01) sits above the deep-dream prune threshold (0.005), so
/// `wavefronts_dissolved` is structurally 0 there — scoring that 0.0 would
/// report a falsification the substrate cannot even express.)
pub fn merge_consolidation_score(
    merge_epochs: &[usize],
    absorb_epochs: &[usize],
    window: usize,
) -> f32 {
    merge_consolidation_score_proven(merge_epochs, absorb_epochs, window, false)
}

/// Like [`merge_consolidation_score`], with the blind-channel ambiguity
/// resolved by an explicit capability proof. `absorb_epochs` must contain only
/// ORGANIC consolidation events — canary absorptions are experimenter
/// artifacts and belong in `channel_proven`, not the alignment set. With the
/// channel proven live, merges against zero organic events are genuine
/// counter-evidence (0.0), not no-evidence.
pub fn merge_consolidation_score_proven(
    merge_epochs: &[usize],
    absorb_epochs: &[usize],
    window: usize,
    channel_proven: bool,
) -> f32 {
    if merge_epochs.is_empty() {
        return 0.5;
    }
    if absorb_epochs.is_empty() {
        return if channel_proven { 0.0 } else { 0.5 };
    }
    let hits = merge_epochs
        .iter()
        .filter(|&&m| {
            absorb_epochs
                .iter()
                .any(|&a| a.abs_diff(m) <= window)
        })
        .count();
    hits as f32 / merge_epochs.len() as f32
}

/// Prediction 3 — shared cores ⇒ swarm agreement. Each pair is one agent
/// pair: (shared-core fraction between the two nodes, recall-agreement rate on
/// probes both nodes hold). Holds when pairs sharing more beliefs rank with
/// higher recall agreement.
pub fn shared_agreement_score(pairs: &[(f32, f32)]) -> f32 {
    rank_correlation_01(pairs)
}

/// Weighted `1 - score` aggregate — LOWER is better (ladder convention). A
/// substrate whose belief dynamics fully honor all three predictions scores
/// ~0; one whose dynamics anti-predict scores ~1; pure no-evidence sits at
/// the weighted 0.5 line.
pub fn l7_fitness(
    stability_recall: f32,
    merge_consolidation: f32,
    shared_agreement: f32,
    w: &L7Weights,
) -> f32 {
    let total = (w.stability_recall + w.merge_consolidation + w.shared_agreement).max(f32::EPSILON);
    (w.stability_recall * (1.0 - stability_recall)
        + w.merge_consolidation * (1.0 - merge_consolidation)
        + w.shared_agreement * (1.0 - shared_agreement))
        / total
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── rank correlation: real signal must move the score, junk must not ──

    #[test]
    fn monotone_pairs_score_high() {
        let pairs: Vec<(f32, f32)> = (0..10).map(|i| (i as f32, i as f32 * 2.0 + 1.0)).collect();
        assert!(rank_correlation_01(&pairs) > 0.95);
    }

    #[test]
    fn anti_monotone_pairs_score_low() {
        let pairs: Vec<(f32, f32)> = (0..10).map(|i| (i as f32, -(i as f32))).collect();
        assert!(rank_correlation_01(&pairs) < 0.05);
    }

    #[test]
    fn independent_pairs_score_near_neutral() {
        // Deterministic scramble: y is a bit-mixed permutation of x's order.
        let pairs: Vec<(f32, f32)> = (0..32u32)
            .map(|i| {
                let mixed = (i.wrapping_mul(2654435761) >> 27) as f32;
                (i as f32, mixed)
            })
            .collect();
        let s = rank_correlation_01(&pairs);
        assert!((0.2..=0.8).contains(&s), "independent data must sit near 0.5, got {s}");
    }

    #[test]
    fn no_evidence_is_exactly_neutral() {
        assert_eq!(rank_correlation_01(&[]), 0.5);
        assert_eq!(rank_correlation_01(&[(1.0, 2.0), (3.0, 4.0)]), 0.5, "<3 pairs");
        // Constant stability: no ordering info even with many pairs.
        let flat: Vec<(f32, f32)> = (0..8).map(|i| (0.7, i as f32)).collect();
        assert_eq!(rank_correlation_01(&flat), 0.5);
    }

    #[test]
    fn ties_use_average_ranks() {
        // Two tied x plateaus, y strictly rising: still positive, not 1.0-perfect.
        let pairs = vec![(1.0, 1.0), (1.0, 2.0), (2.0, 3.0), (2.0, 4.0)];
        let s = rank_correlation_01(&pairs);
        assert!(s > 0.75 && s < 1.0, "tied ranks should attenuate, got {s}");
    }

    // ── merge ⇒ consolidation ──

    #[test]
    fn merges_matched_by_absorbs_score_one() {
        assert_eq!(merge_consolidation_score(&[2, 4], &[2, 5], 1), 1.0);
    }

    #[test]
    fn merges_with_no_nearby_absorb_score_zero() {
        assert_eq!(merge_consolidation_score(&[4, 6], &[0], 1), 0.0);
    }

    #[test]
    fn merge_window_is_symmetric_and_bounded() {
        // absorb one epoch BEFORE the merge counts; two epochs away doesn't.
        assert_eq!(merge_consolidation_score(&[3], &[2], 1), 1.0);
        assert_eq!(merge_consolidation_score(&[3], &[1], 1), 0.0);
    }

    #[test]
    fn no_merges_is_no_evidence() {
        assert_eq!(merge_consolidation_score(&[], &[0, 1, 2], 1), 0.5);
    }

    #[test]
    fn blind_absorb_observable_is_no_evidence_not_refutation() {
        // Merges happened but the substrate never dissolved anything — the
        // observable is blind, so the implication is untested, not false.
        assert_eq!(merge_consolidation_score(&[2, 4, 5], &[], 1), 0.5);
    }

    #[test]
    fn proven_channel_with_zero_organic_absorbs_is_refutation() {
        // The canary proved consolidation CAN fire; fusions occurred; nothing
        // organic consolidated — that is counter-evidence, not ignorance.
        assert_eq!(merge_consolidation_score_proven(&[2, 4, 5], &[], 1, true), 0.0);
        // Without the proof, the same data stays no-evidence.
        assert_eq!(merge_consolidation_score_proven(&[2, 4, 5], &[], 1, false), 0.5);
        // No merges is no evidence regardless of proof.
        assert_eq!(merge_consolidation_score_proven(&[], &[], 1, true), 0.5);
        // With organic absorbs present, alignment scores as before.
        assert_eq!(merge_consolidation_score_proven(&[2], &[2], 1, true), 1.0);
    }

    // ── fitness aggregation ──

    #[test]
    fn fitness_bounds_and_direction() {
        let w = L7Weights::default();
        assert!(l7_fitness(1.0, 1.0, 1.0, &w) < 1e-6, "all predictions hold => ~0");
        assert!((l7_fitness(0.0, 0.0, 0.0, &w) - 1.0).abs() < 1e-6, "all anti-predict => ~1");
        let neutral = l7_fitness(0.5, 0.5, 0.5, &w);
        assert!((neutral - 0.5).abs() < 1e-6, "no evidence => 0.5 line");
        // Better belief dynamics must LOWER fitness.
        assert!(l7_fitness(0.9, 0.5, 0.5, &w) < neutral);
    }

    #[test]
    fn weights_are_normalized() {
        // Doubling every weight must not change the fitness.
        let w1 = L7Weights::default();
        let w2 = L7Weights {
            stability_recall: w1.stability_recall * 2.0,
            merge_consolidation: w1.merge_consolidation * 2.0,
            shared_agreement: w1.shared_agreement * 2.0,
        };
        let a = l7_fitness(0.8, 0.3, 0.6, &w1);
        let b = l7_fitness(0.8, 0.3, 0.6, &w2);
        assert!((a - b).abs() < 1e-6);
    }
}
