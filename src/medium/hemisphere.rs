//! Hemisphere - a handed partition of the holographic medium.
//!
//! Each hemisphere is essentially a Medium with awareness of its handedness.
//! Left (analytical): dx/dt = f(x) - pure growth, no dampening
//! Right (holistic): dx/dt = f(x) - Iηx - full ghostmagicOS dynamics

use std::collections::HashMap;

use ndarray::{Array1, Array2, s};
use uuid::Uuid;

use crate::xi_operator::{compute_xi_signature, xi_diversity_boost};

use super::types::*;
use super::types::DreamReport;

/// Recall energy exponent (`KANNAKA_RECALL_ENERGY_EXP`, **default 0.0 =
/// pure similarity**). ADR-0048 energy-neutral ranking: recall ranks by
/// `similarity * energy^exp`, so `0.0` neutralizes the rich-get-richer
/// recall-frequency bias entirely, `0.5` = `similarity * sqrt(energy)`
/// softens it, and `1.0` is the historical `similarity * energy`.
///
/// The default flipped from 1.0 to 0.0 on 2026-09-16 (kannaka-memory#965).
/// Measured on the live O1 store, same probes, same vectors: production
/// r@10 0.514 against 0.960 for plain cosine, and in 80% of the misses the
/// correct memory had the *higher* cosine and lost on energy alone (winner
/// 3.7x the target's). With this exponent at 0 the medium recalls at parity
/// with cosine (~0.97 by content). Ranking-only — the energy array is never
/// written by recall scoring; set `1.0` to reproduce the old ranking.
/// `KANNAKA_RECALL_XI_BOOST` — `off` / `0` / `false` skips the ξ-diversity
/// reranker (`xi_diversity_boost`) so recall ranks by raw resonance. Default
/// on (the shipped behaviour). Read once per process.
///
/// Why it exists: on LongMemEval-S (kannaka-bench, 2026-09-17) with the SAME
/// MiniLM embeddings as an exact-cosine baseline, the medium found the
/// evidence session less often (partial 0.78 vs 0.94 hit@5). Tracing a
/// miss showed raw resonance ≈ cosine, then the reranker lifting candidates
/// above 0.15 cosine with a repelling ξ-signature by up to ×1.8 while the
/// gold turn, whose ξ did not repel, kept its raw score — a rank inversion.
/// Whether the reranker helps or hurts is now a measurement, not a belief.
pub fn recall_xi_boost_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        !matches!(
            std::env::var("KANNAKA_RECALL_XI_BOOST").map(|v| v.to_ascii_lowercase()).as_deref(),
            Ok("off") | Ok("0") | Ok("false") | Ok("no")
        )
    })
}

pub fn recall_energy_exp() -> f32 {
    std::env::var("KANNAKA_RECALL_ENERGY_EXP")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|e: &f32| e.is_finite())
        .map(|e: f32| e.clamp(0.0, 1.0))
        .unwrap_or(0.0)
}

/// Recall temporal exponent (`KANNAKA_RECALL_TEMPORAL_EXP`, **default 0.0 =
/// OFF, byte-identical to the historical timeless ranking**).
///
/// L8 / temporal-recall experiment. Recall has always ranked by
/// `similarity * energy^e`, which reads no timestamp at all: a memory's pull
/// depends on how strongly it RESONATES and how often it has been ACCESSED,
/// never on when it was last CONFIRMED. That collapses two axes that come
/// apart as soon as stored facts contradict each other — a superseded fact and
/// the fact that replaced it are near-identical text, so they land side by side
/// in the candidate pool and only similarity separates them (arbitrarily).
///
/// This exponent adds the missing axis: `resonance = similarity * energy^e *
/// tweight^t`, where `tweight` folds *confirmation recency* and the ADR-0035
/// temporal-truth bounds (see [`temporal_weight`]). `t = 0.0` disables it
/// entirely (the multiply is skipped, not computed as `^0`), `t = 1.0` applies
/// it at full strength.
pub fn recall_temporal_exp() -> f32 {
    // ADR-0051 M9: a caller doing DEDUPLICATION must score with the temporal
    // factor OFF. Stamping a local memory as superseded lowers its own
    // resonance, which would lower its dedup score, which would WIDEN the
    // admission window that dedup exists to close — letting a peer's copy of
    // the stale text back in with a fresh `created_at` that reads as maximally
    // recent, out-ranking the truth it was supposed to have replaced.
    //
    // A process flag rather than a threaded parameter, matching the existing
    // BULK_MODE pattern in hrm_store: the alternative is four layers of
    // plumbing through a trait boundary for a scope that is always short and
    // synchronous.
    if TEMPORAL_SCORING_SUPPRESSED.load(std::sync::atomic::Ordering::Relaxed) {
        return 0.0;
    }
    std::env::var("KANNAKA_RECALL_TEMPORAL_EXP")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|e: &f32| e.is_finite())
        .map(|e: f32| e.clamp(0.0, 1.0))
        .unwrap_or(0.0)
}

/// Set while a dedup-style recall is in flight; see [`SuppressTemporalScoring`].
static TEMPORAL_SCORING_SUPPRESSED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// RAII guard forcing [`recall_temporal_exp`] to 0.0 for its lifetime.
///
/// Hold this around any recall whose result gates an ADMISSION decision —
/// deduplication, novelty, "do we already have this". Ranking recalls must NOT
/// use it: discounting a superseded fact is the entire point there.
///
/// Restores the previous value on drop, so nesting is safe and an early return
/// or a panic cannot leave scoring suppressed for the rest of the process.
pub struct SuppressTemporalScoring(bool);

impl SuppressTemporalScoring {
    pub fn new() -> Self {
        let prev = TEMPORAL_SCORING_SUPPRESSED.swap(true, std::sync::atomic::Ordering::Relaxed);
        Self(prev)
    }
}

impl Default for SuppressTemporalScoring {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for SuppressTemporalScoring {
    fn drop(&mut self) {
        TEMPORAL_SCORING_SUPPRESSED.store(self.0, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Sentinel for "no as-of pin set": fall back to the wall clock.
const RECALL_AS_OF_UNSET: i64 = i64::MIN;

/// Millisecond timestamp recall scores against; see [`RecallAsOf`].
static RECALL_AS_OF_MS: std::sync::atomic::AtomicI64 =
    std::sync::atomic::AtomicI64::new(RECALL_AS_OF_UNSET);

/// The instant temporal scoring treats as "now".
///
/// Wall clock unless a [`RecallAsOf`] guard is in scope. Every temporal weight
/// in one recall reads this once, so a single recall stays internally
/// consistent.
pub fn recall_now() -> chrono::DateTime<chrono::Utc> {
    match RECALL_AS_OF_MS.load(std::sync::atomic::Ordering::Relaxed) {
        RECALL_AS_OF_UNSET => chrono::Utc::now(),
        ms => chrono::DateTime::from_timestamp_millis(ms).unwrap_or_else(chrono::Utc::now),
    }
}

/// RAII guard pinning [`recall_now`] to a chosen instant for its lifetime.
///
/// Why this exists: temporal weight decays from `observed_at` to *now*, and
/// `0.5^(age/half_life)` is clamped up to the superseded floor. That clamp
/// binds at two half-lives — 360 days at the default — so on a store older
/// than that, every candidate returns the floor and the temporal factor
/// degrades into a constant multiplier that cannot reorder anything. It does
/// not fail; it just stops doing its job, silently, and lowering the floor
/// only moves the constant.
///
/// Pinning "now" to the time the question is actually about restores the
/// signal, and is the honest question anyway: an agent asking "what did we use
/// last March?" wants recency measured from March.
///
/// Restores the previous value on drop, so nesting is safe and an early return
/// or a panic cannot leave recall pinned to a stale instant.
pub struct RecallAsOf(i64);

impl RecallAsOf {
    pub fn new(at: chrono::DateTime<chrono::Utc>) -> Self {
        let prev = RECALL_AS_OF_MS.swap(
            at.timestamp_millis(),
            std::sync::atomic::Ordering::Relaxed,
        );
        Self(prev)
    }
}

impl Drop for RecallAsOf {
    fn drop(&mut self) {
        RECALL_AS_OF_MS.store(self.0, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Half-life in days for confirmation-recency decay
/// (`KANNAKA_RECALL_TEMPORAL_HALFLIFE_DAYS`, default 180).
///
/// Deliberately far shorter than the flat path's ~693-day amplitude half-life
/// (`medium/core.rs`): that one models a memory fading, this one models a
/// claim going stale. Only active when [`recall_temporal_exp`] > 0.
pub fn recall_temporal_halflife_days() -> f32 {
    std::env::var("KANNAKA_RECALL_TEMPORAL_HALFLIFE_DAYS")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|d: &f32| d.is_finite() && *d > 0.0)
        .unwrap_or(180.0)
}

/// Default floor applied to a fact that is not true *now* — past its
/// `expires_at` (superseded) or before its `effective_at` (not yet in force).
///
/// **Deliberately non-zero.** Discounting a superseded fact is the point;
/// making it unretrievable would just be forgetting with extra steps, and an
/// agent has to be able to answer "what did we use *before*". The L8 P3 gate
/// exists to hold this honest — and L8 showed the floor is the knob that
/// governs it, so it is tunable rather than baked in.
pub const TEMPORAL_SUPERSEDED_FLOOR: f32 = 0.25;

/// Superseded-fact floor (`KANNAKA_RECALL_TEMPORAL_FLOOR`, default
/// [`TEMPORAL_SUPERSEDED_FLOOR`]). Clamped to `[0.05, 1.0]` — never 0, because
/// a superseded fact must stay retrievable.
pub fn recall_temporal_floor() -> f32 {
    std::env::var("KANNAKA_RECALL_TEMPORAL_FLOOR")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|f: &f32| f.is_finite())
        .map(|f: f32| f.clamp(0.05, 1.0))
        .unwrap_or(TEMPORAL_SUPERSEDED_FLOOR)
}

/// Confirmation-recency weight for one wavefront, in `(0, 1]`.
///
/// - Not currently true (`Expired` / `Future`) → [`TEMPORAL_SUPERSEDED_FLOOR`].
/// - Currently true → `0.5^(age_since_confirmed / half_life)`, where "confirmed"
///   is `observed_at` when the caller has set it and `created_at` otherwise, so
///   a corpus that never records observation times reads as pure age.
///
/// Never returns 0: a floored memory must stay reachable (see P3).
pub fn temporal_weight(
    meta: &WavefrontMeta,
    now: chrono::DateTime<chrono::Utc>,
    half_life_days: f32,
    floor: f32,
) -> f32 {
    // Not-true-now short-circuits: a superseded claim is discounted regardless
    // of how recently it was observed.
    if let Some(exp) = meta.expires_at {
        if now >= exp {
            return floor;
        }
    }
    if let Some(eff) = meta.effective_at {
        if now < eff {
            return floor;
        }
    }
    let confirmed = meta.observed_at.unwrap_or(meta.created_at);
    let age_days = (now - confirmed).num_seconds().max(0) as f32 / 86_400.0;
    let w = 0.5f32.powf(age_days / half_life_days.max(1e-3));
    // Clamp above the floor: a merely OLD but still-true fact must never rank
    // below an explicitly EXPIRED one.
    w.clamp(floor, 1.0)
}

/// A single hemisphere of the chiral medium.
#[derive(Debug, Clone)]
pub struct Hemisphere {
    /// Which hand this hemisphere represents
    pub hand: Hand,
    /// N x D_h tensor of wavefront patterns (capacity may exceed active count)
    pub wavefronts: Array2<f32>,
    /// Energy (amplitude) per wavefront
    pub energy: Array1<f32>,
    /// Frequency per wavefront
    pub frequency: Array1<f32>,
    /// Phase per wavefront
    pub phase: Array1<f32>,
    /// Creation timestamps
    pub timestamps: Vec<i64>,
    /// Content metadata
    pub metadata: Vec<WavefrontMeta>,
    /// ID -> index mapping
    pub(crate) id_to_index: HashMap<Uuid, usize>,
    /// Current dimension count for this hemisphere
    pub dims: usize,
    /// Active wavefront count (tensor capacity may be larger for amortized growth)
    pub(crate) len: usize,
}

impl Hemisphere {
    /// Create a new empty hemisphere with given handedness and dimension count.
    pub fn new(hand: Hand, dims: usize) -> Self {
        Self {
            hand,
            wavefronts: Array2::zeros((0, dims)),
            energy: Array1::zeros(0),
            frequency: Array1::zeros(0),
            phase: Array1::zeros(0),
            timestamps: Vec::new(),
            metadata: Vec::new(),
            id_to_index: HashMap::new(),
            dims,
            len: 0,
        }
    }

    /// Number of active wavefronts in this hemisphere.
    pub fn count(&self) -> usize {
        self.len
    }

    /// Shrink tensors to exactly fit active wavefronts.
    pub fn compact(&mut self) {
        if self.len < self.wavefronts.nrows() {
            self.wavefronts = self.wavefronts.slice(s![..self.len, ..]).to_owned();
            self.energy = self.energy.slice(s![..self.len]).to_owned();
            self.frequency = self.frequency.slice(s![..self.len]).to_owned();
            self.phase = self.phase.slice(s![..self.len]).to_owned();
        }
    }

    /// Total energy across all active wavefronts.
    pub fn total_energy(&self) -> f32 {
        if self.len == 0 { return 0.0; }
        self.energy.slice(s![..self.len]).sum()
    }

    /// Mean energy across all active wavefronts (0.0 if empty).
    pub fn mean_energy(&self) -> f32 {
        if self.count() == 0 { 0.0 } else { self.total_energy() / self.count() as f32 }
    }

    /// Mean wavefront vector (centroid) across all wavefronts.
    /// Returns None if hemisphere is empty.
    /// ADR-0024 CS-4: used for hemispheric divergence (Δ) computation.
    pub fn mean_wavefront(&self) -> Option<Vec<f32>> {
        let n = self.count();
        if n == 0 { return None; }
        let dim = self.wavefronts.ncols();
        let mut mean = vec![0.0f32; dim];
        for i in 0..n {
            for j in 0..dim {
                mean[j] += self.wavefronts[[i, j]];
            }
        }
        for v in mean.iter_mut() {
            *v /= n as f32;
        }
        Some(mean)
    }

    /// Add a wavefront to this hemisphere.
    pub fn add_wavefront(
        &mut self,
        vector: &[f32],
        content: String,
        importance: f32,
    ) -> Result<Uuid, MediumError> {
        // Adapt vector to hemisphere dimensions (truncate or zero-pad)
        let adapted = Self::adapt_vector(vector, self.dims);

        let id = Uuid::new_v4();
        let index = self.len;

        // Amortized growth: only reallocate when capacity is exhausted
        let cap = self.wavefronts.nrows();
        if index >= cap {
            let new_cap = if cap == 0 { 8 } else { cap * 2 };
            let mut new_wf = Array2::zeros((new_cap, self.dims));
            if cap > 0 {
                new_wf.slice_mut(s![..cap, ..]).assign(&self.wavefronts);
            }
            self.wavefronts = new_wf;

            let mut new_energy = Array1::zeros(new_cap);
            let mut new_frequency = Array1::zeros(new_cap);
            let mut new_phase = Array1::zeros(new_cap);
            if cap > 0 {
                new_energy.slice_mut(s![..cap]).assign(&self.energy);
                new_frequency.slice_mut(s![..cap]).assign(&self.frequency);
                new_phase.slice_mut(s![..cap]).assign(&self.phase);
            }
            self.energy = new_energy;
            self.frequency = new_frequency;
            self.phase = new_phase;
        }

        // Write into pre-allocated slot
        for (i, &val) in adapted.iter().enumerate() {
            if i < self.dims {
                self.wavefronts[[index, i]] = val;
            }
        }
        self.energy[index] = importance;
        self.frequency[index] = 1.0;
        // Born phase: content-smooth (belief substrate) or legacy phase-0. This
        // is the chiral hemispheres' own ingest chokepoint (parallel to
        // WavefrontStore::insert for the flat medium). Center against the EXISTING
        // corpus mean (self.len is still the pre-insert count here) so a new
        // memory spreads like the rephase migration on anisotropic embeddings.
        self.phase[index] = if crate::medium::chiral::belief_phase_enabled() {
            let mean = crate::medium::chiral::corpus_mean(&self.wavefronts, self.len);
            crate::medium::chiral::content_born_phase_centered(&adapted, &mean)
        } else {
            0.0
        };

        self.timestamps.push(chrono::Utc::now().timestamp_millis());
        self.metadata.push(WavefrontMeta::new(id, content));
        self.id_to_index.insert(id, index);
        self.len += 1;

        Ok(id)
    }

    /// Replace the vector at `index` IN PLACE, adapting it to this hemisphere's
    /// `dims` exactly as `add_wavefront` does (truncate/zero-pad), without touching
    /// energy/phase/frequency/metadata/id. Used by the re-encode migration (#107)
    /// to refresh wavefronts written by a broken encoder. No-op if out of range.
    pub(crate) fn set_wavefront_vector(&mut self, index: usize, vector: &[f32]) {
        if index >= self.len {
            return;
        }
        let adapted = Self::adapt_vector(vector, self.dims);
        for (i, &val) in adapted.iter().enumerate() {
            if i < self.dims {
                self.wavefronts[[index, i]] = val;
            }
        }
    }

    /// Remove a wavefront from this hemisphere using swap-remove (O(1) tensor op).
    pub fn remove_wavefront(&mut self, id: &Uuid) -> bool {
        let index = match self.id_to_index.get(id) {
            Some(&idx) => idx,
            None => return false,
        };

        if self.len == 0 { return false; }

        let last = self.len - 1;
        self.id_to_index.remove(id);

        if index != last {
            let last_row = self.wavefronts.row(last).to_owned();
            self.wavefronts.row_mut(index).assign(&last_row);
            self.energy[index] = self.energy[last];
            self.frequency[index] = self.frequency[last];
            self.phase[index] = self.phase[last];

            self.timestamps.swap(index, last);
            self.metadata.swap(index, last);

            let swapped_id = self.metadata[index].id;
            self.id_to_index.insert(swapped_id, index);
        }

        self.timestamps.pop();
        self.metadata.pop();
        self.len -= 1;

        true
    }

    /// Compute resonance (recall) within this hemisphere.
    /// Returns top-k matches sorted by xi-diversity-boosted resonance strength.
    ///
    /// After initial similarity scoring, a re-ranking pass applies
    /// `xi_diversity_boost` to each candidate's score, promoting results
    /// that are semantically similar but have distinct Xi signatures.
    pub fn resonate(&self, query: &[f32], top_k: usize) -> Vec<ChiralResonance> {
        self.resonate_with_weights(
            query,
            top_k,
            recall_energy_exp(),
            recall_temporal_exp(),
            recall_temporal_halflife_days(),
        )
    }

    /// `resonate` with an explicit energy exponent (ADR-0048 energy-neutral
    /// ranking). `resonance = similarity * energy^exp`. `exp = 1.0` is the
    /// pre-#965 `similarity * energy` (byte-identical fast path); `exp = 0.0`
    /// ranks by pure similarity, neutralizing the rich-get-richer
    /// recall-frequency bias that buries never-surfaced memories (a
    /// frequently-recalled memory's energy climbs toward the 2.0 cap while a
    /// cold one stays at baseline, so weaker-similarity favorites outrank it).
    /// Ranking-only: energy is never written here.
    pub fn resonate_with_energy_exp(
        &self,
        query: &[f32],
        top_k: usize,
        energy_exp: f32,
    ) -> Vec<ChiralResonance> {
        // temporal_exp = 0.0 → the temporal factor is skipped entirely, so this
        // stays exactly the pre-L8 function for every existing caller and test.
        self.resonate_with_weights(query, top_k, energy_exp, 0.0, 180.0)
    }

    /// `resonate` with explicit energy AND temporal exponents.
    ///
    /// `resonance = similarity * energy^energy_exp * tweight^temporal_exp`
    ///
    /// `temporal_exp = 0.0` skips the temporal factor (no [`temporal_weight`]
    /// call, no multiply) — byte-identical to [`Self::resonate_with_energy_exp`].
    /// Ranking-only: neither energy nor any timestamp is written here.
    ///
    /// The temporal factor is applied at BOTH scoring points (the full-corpus
    /// pass and the xi re-rank), so it governs which candidates enter the
    /// `2*k` pool rather than merely reshuffling a pool already chosen without
    /// it — post-fetch re-ranking cannot promote what was never fetched.
    pub fn resonate_with_weights(
        &self,
        query: &[f32],
        top_k: usize,
        energy_exp: f32,
        temporal_exp: f32,
        half_life_days: f32,
    ) -> Vec<ChiralResonance> {
        if self.count() == 0 { return vec![]; }

        let adapted = Self::adapt_vector(query, self.dims);
        let query_arr = Array1::from_vec(adapted.clone());
        let query_norm = query_arr.dot(&query_arr).sqrt();
        if query_norm < 1e-8 { return vec![]; }

        // Energy weight for ranking. Branch keeps the default byte-identical
        // (no powf in the historical path).
        let eweight = |e: f32| -> f32 {
            if energy_exp >= 1.0 { e } else if energy_exp <= 0.0 { 1.0 } else { e.powf(energy_exp) }
        };

        // Temporal weight. One `now` for the whole call so a single recall is
        // internally consistent (and deterministic under test) — the wall clock
        // unless a `RecallAsOf` guard pins it to the time the query is about.
        let temporal_on = temporal_exp > 0.0;
        let now = recall_now();
        let floor = if temporal_on { recall_temporal_floor() } else { TEMPORAL_SUPERSEDED_FLOOR };
        let tweight = |i: usize| -> f32 {
            if !temporal_on { return 1.0; }
            let w = temporal_weight(&self.metadata[i], now, half_life_days, floor);
            if temporal_exp >= 1.0 { w } else { w.powf(temporal_exp) }
        };

        // 1. Score all candidates by raw similarity * energy^exp * tweight^texp
        let mut results: Vec<(usize, f32, f32)> = (0..self.count())
            .map(|i| {
                let wf = self.wavefronts.row(i);
                let wf_norm = wf.dot(&wf).sqrt();
                if wf_norm < 1e-8 { return (i, 0.0, 0.0); }
                let similarity = wf.dot(&query_arr) / (wf_norm * query_norm);
                let mut resonance = similarity * eweight(self.energy[i]);
                if temporal_on { resonance *= tweight(i); }
                (i, resonance, similarity)
            })
            .collect();

        // 2. Take top 2*k candidates for xi re-ranking (avoid computing xi
        //    signatures for the entire hemisphere when it's large).
        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let rerank_pool = top_k.saturating_mul(2).max(top_k);
        results.truncate(rerank_pool);
        if std::env::var("KANNAKA_RECALL_TRACE").is_ok() {
            for (i, r, s) in results.iter().take(6) {
                eprintln!("[recall-trace]     {:?} raw idx={} sim={:.4} res={:.4} e={:.4} {}",
                    self.hand, i, s, r, self.energy[*i],
                    self.metadata[*i].content.chars().take(32).collect::<String>());
            }
        }

        // 3. Xi diversity re-ranking: boost candidates with distinct xi signatures.
        //
        // The previous `filter(|(_, r, _)| *r > 0.0)` here dropped every memory
        // whose cosine similarity to the query was non-positive. With 10K-dim
        // random-projection codebook vectors that's a frequent outcome: a fresh
        // query's projected direction can have negative cosine sim with most
        // wavefronts purely by projection-sign coincidence. The filter then
        // returned 0-1 results for a top_k=5 request even when the medium had
        // 5+ memories that legitimately contained the query terms — see
        // kannaka-memory#83 for the alpha/beta repro.
        //
        // Keep the magnitude-only zero-norm guard (an identically-zero
        // wavefront has no signal), but let all signed-resonance entries
        // through. Final sort by signed resonance still puts the strongest
        // positive matches at the top; negatives fill remaining top_k slots.
        let query_xi = compute_xi_signature(&adapted);

        let mut boosted: Vec<(usize, f32, f32)> = results
            .into_iter()
            .filter(|(_, r, _)| r.abs() > 1e-8)
            .map(|(i, _resonance, sim)| {
                let wf_vec: Vec<f32> = self.wavefronts.row(i).to_vec();
                let wf_xi = compute_xi_signature(&wf_vec);
                let boosted_sim = if recall_xi_boost_enabled() {
                    xi_diversity_boost(sim, &query_xi, &wf_xi)
                } else {
                    sim.clamp(0.0, 1.0)
                };
                let mut boosted_resonance = boosted_sim * eweight(self.energy[i]);
                if temporal_on { boosted_resonance *= tweight(i); }
                (i, boosted_resonance, boosted_sim)
            })
            .collect();

        // 4. Re-sort by boosted resonance and take top-k
        boosted.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        boosted.truncate(top_k);
        if std::env::var("KANNAKA_RECALL_TRACE").is_ok() {
            for (i, r, s) in boosted.iter().take(6) {
                eprintln!("[recall-trace]     {:?} boosted idx={} sim={:.4} res={:.4}",
                    self.hand, i, s, r);
            }
        }

        boosted
            .into_iter()
            .map(|(i, resonance, sim)| {
                ChiralResonance {
                    id: self.metadata[i].id,
                    content: self.metadata[i].content.clone(),
                    hand: self.hand,
                    similarity: sim,
                    resonance_strength: resonance,
                    is_intuition: self.hand == Hand::Right,
                }
            })
            .collect()
    }

    /// Apply dynamics appropriate to this hemisphere's handedness.
    ///
    /// Left (analytical):     dx/dt = f(x) - no dampening, attention stays sharp
    /// Right (holistic): dx/dt = f(x) - Iηx - full ghostmagicOS dynamics
    pub fn apply_dynamics(&mut self, dt: f32) {
        if self.count() < 2 { return; }

        let n = self.count();
        let threshold = 0.5;

        // Compute pairwise dot products for interference
        let mut growth_terms = vec![0.0f32; n];
        for i in 0..n {
            let wi = self.wavefronts.row(i);
            for j in 0..n {
                if i == j { continue; }
                let wj = self.wavefronts.row(j);
                let dot = wi.dot(&wj);
                if dot > threshold {
                    let phase_alignment = (self.phase[j] - self.phase[i]).cos();
                    growth_terms[i] += dot * phase_alignment * self.energy[j];
                }
            }
            growth_terms[i] /= n as f32;
        }

        let eta = match self.hand {
            Hand::Left => 0.0,    // NO dampening - analytical workspace stays sharp
            Hand::Right => 0.02,  // Full ghostmagicOS dampening
        };

        for i in 0..n {
            let growth = growth_terms[i] * dt;
            let dampening = eta * self.energy[i] * dt;
            self.energy[i] = (self.energy[i] + growth - dampening).max(0.01);

            // Phase coupling
            if growth > dampening * 0.5 {
                let mut phase_target = 0.0f32;
                let mut count = 0;
                for j in 0..n {
                    if i != j {
                        let dot = self.wavefronts.row(i).dot(&self.wavefronts.row(j));
                        if dot > threshold {
                            phase_target += self.phase[j];
                            count += 1;
                        }
                    }
                }
                if count > 0 {
                    let target = phase_target / count as f32;
                    self.phase[i] += 0.05 * dt * (target - self.phase[i]).sin();
                }
            }
        }
    }

    /// Eigenstructure-aware dream cycles for this hemisphere.
    ///
    /// Mirrors Medium::dream() but operates on Hemisphere's own fields.
    /// Each cycle: coherence matrix, eigenstructure, annealing, hallucination, pruning.
    ///
    /// # Arguments
    /// * `cycles` - Number of annealing cycles to run
    /// * `initial_temperature` - Starting temperature (default: 1.0)
    /// * `prune_threshold` - Energy below which wavefronts are pruned (holistic = gentler)
    pub fn dream(
        &mut self,
        cycles: usize,
        initial_temperature: Option<f32>,
        prune_threshold: f32,
    ) -> DreamReport {
        let mut temperature = initial_temperature.unwrap_or(1.0);
        let energy_before = if self.count() > 0 {
            self.energy.slice(s![..self.len]).sum() / self.len as f32
        } else {
            0.0
        };

        let mut dissolved_count = 0;
        let mut strengthened_count = 0;
        let mut hallucinated_count = 0;
        let mut converged = false;

        let convergence_threshold = 0.0001;
        let annealing_rate = 0.95;

        for cycle in 0..cycles {
            if self.count() < 2 {
                break;
            }

            let prev_energy: Vec<f32> = self.energy.slice(s![..self.len]).to_vec();

            // 1. Compute coherence matrix and eigenstructure
            let coherence = self.coherence_matrix();
            let eigenstructure = self.compute_eigenstructure(&coherence);

            // 2. Apply eigenstructure-based annealing
            self.apply_eigenstructure_annealing(&eigenstructure, temperature);

            // 3. Hallucination: create novel wavefronts from cross-cluster superposition.
            //
            // This is the chiral dream's hallucination gate (the actual code path
            // when chiral perturbation is enabled). Old gate `cycle % 3 == 0` only
            // fired once across a 3-cycle dream, and the `count < 2000` cap silently
            // disabled hallucination on hemispheres past that size. Replaced with a
            // budget that scales with hemisphere size (~1% growth per dream).
            let hallucination_budget = (self.count() / 100).max(2);
            if temperature > 0.3 && hallucinated_count < hallucination_budget {
                let per_cycle = (hallucination_budget / cycles.max(1)).max(2);
                let hallucinated = self.generate_hallucinated_wavefronts(
                    &eigenstructure, temperature, per_cycle,
                );
                hallucinated_count += hallucinated;
            }

            // 4. Prune wavefronts below threshold (forgetting)
            let pruned = self.prune_low_energy_wavefronts(prune_threshold);
            dissolved_count += pruned;

            // Count strengthened wavefronts. Old absolute +0.01 threshold barely
            // ever triggered on large hemispheres where per-wavefront energy is
            // small — added a relative >5% increase clause as an OR.
            for i in 0..self.count().min(prev_energy.len()) {
                let prev = prev_energy[i];
                let curr = self.energy[i];
                if curr > prev + 0.01 || (prev > 1e-6 && curr > prev * 1.05) {
                    strengthened_count += 1;
                }
            }

            // 5. Reduce temperature (annealing schedule)
            temperature *= annealing_rate;

            // Check for convergence
            let current_count = self.count();
            if current_count == prev_energy.len() && current_count > 0 {
                let mut energy_change = 0.0f32;
                for i in 0..current_count {
                    energy_change += (self.energy[i] - prev_energy[i]).abs();
                }
                energy_change /= current_count as f32;

                if energy_change < convergence_threshold && cycle > 5 {
                    converged = true;
                    break;
                }
            }
        }

        let energy_after = if self.count() > 0 {
            self.energy.slice(s![..self.len]).sum() / self.len as f32
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

    /// Compute pairwise coherence matrix for all wavefronts in this hemisphere.
    fn coherence_matrix(&self) -> Array2<f32> {
        let n = self.count();
        let mut coherence = Array2::zeros((n, n));

        // Belief substrate: mean-CENTER the vectors before the pairwise dot.
        // Real embeddings are anisotropic (cone-clustered), so RAW dots are all
        // large + positive → this matrix is ~rank-1 with a uniform dominant
        // eigenvector → every wavefront alignment ≈ 1/√n lands in the annealing
        // dead band [0.05, 0.1] → 0 strengthened / 0 dissolved (the live "0/0/0"
        // consolidation). Centering removes the shared component so the dominant
        // mode captures CONTENT VARIATION → alignments spread out of the dead
        // band → consolidation revives. Only the dream's internal eigenstructure
        // changes; stored vectors and recall (cosine×energy) are untouched.
        // Gated to the belief substrate so the default path is byte-identical.
        let centered = crate::medium::chiral::belief_phase_enabled();
        let mean: Vec<f32> = if centered && n > 0 {
            let dim = self.wavefronts.ncols();
            let mut m = vec![0.0f32; dim];
            for i in 0..n {
                for (mm, &v) in m.iter_mut().zip(self.wavefronts.row(i).iter()) {
                    *mm += v;
                }
            }
            for mm in m.iter_mut() {
                *mm /= n as f32;
            }
            m
        } else {
            Vec::new()
        };

        for i in 0..n {
            for j in 0..n {
                if i != j {
                    let vec_i = self.wavefronts.row(i);
                    let vec_j = self.wavefronts.row(j);
                    let dot_product: f32 = if centered {
                        vec_i
                            .iter()
                            .zip(vec_j.iter())
                            .zip(mean.iter())
                            .map(|((a, b), m)| (a - m) * (b - m))
                            .sum()
                    } else {
                        vec_i.iter().zip(vec_j.iter()).map(|(a, b)| a * b).sum()
                    };
                    let phase_coherence = (self.phase[i] - self.phase[j]).cos();
                    coherence[[i, j]] = phase_coherence * dot_product;
                } else {
                    coherence[[i, j]] = 1.0;
                }
            }
        }

        coherence
    }

    /// Compute eigenstructure of the coherence matrix using power iteration.
    fn compute_eigenstructure(&self, coherence: &Array2<f32>) -> EigenStructure {
        let n = self.count();
        if n < 2 {
            return EigenStructure::empty();
        }

        // Power iteration to find dominant eigenvalue/eigenvector
        let max_iterations = 20;
        let mut dominant_vector = vec![1.0f32; n];
        let mut eigenvalue = 0.0f32;

        let norm: f32 = dominant_vector.iter().map(|x| x * x).sum::<f32>().sqrt();
        for x in &mut dominant_vector {
            *x /= norm;
        }

        for _ in 0..max_iterations {
            let mut next_vector = vec![0.0f32; n];
            for i in 0..n {
                for j in 0..n {
                    next_vector[i] += coherence[[i, j]] * dominant_vector[j];
                }
            }

            let norm: f32 = next_vector.iter().map(|x| x * x).sum::<f32>().sqrt();
            if norm < 1e-6 {
                break;
            }
            for i in 0..n {
                next_vector[i] /= norm;
            }

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

        let alignment_threshold = 0.3;
        let clusters: Vec<usize> = (0..n)
            .filter(|&i| dominant_vector[i].abs() > alignment_threshold)
            .collect();

        EigenStructure {
            eigenvalues: vec![eigenvalue],
            dominant_cluster: clusters,
            wavefront_alignments: dominant_vector,
            spectral_gap: eigenvalue - 0.1,
        }
    }

    /// Apply eigenstructure-based annealing to wavefronts.
    /// Holistic-tuned: gentler than the flat medium. Uses the triode bias principle
    /// (dreams sculpt, they don't crush -- energy floor preserves bias voltage).
    fn apply_eigenstructure_annealing(&mut self, eigenstructure: &EigenStructure, temperature: f32) {
        let n = self.count();
        if n == 0 || eigenstructure.eigenvalues.is_empty() {
            return;
        }

        let dominant_eigenvalue = eigenstructure.eigenvalues[0];

        // Moderate coefficients -- strong enough to produce visible effects
        // while still preserving the holistic hemisphere's broad patterns
        let consolidation_strength = 0.08 * (1.0 + temperature);
        let noise_reduction = 0.02 * (2.0 - temperature);

        // Energy floor: bias voltage -- resting potential for amplification
        let dream_energy_floor = 0.3;

        for i in 0..n {
            let alignment = eigenstructure.wavefront_alignments[i].abs();

            if alignment > 0.1 && dominant_eigenvalue > 0.1 {
                let boost = consolidation_strength * alignment;
                self.energy[i] = (self.energy[i] + boost).min(ENERGY_CAP);

                if eigenstructure.dominant_cluster.contains(&i) && eigenstructure.dominant_cluster.len() > 1 {
                    let mut cluster_phase = 0.0f32;
                    for &cluster_idx in &eigenstructure.dominant_cluster {
                        cluster_phase += self.phase[cluster_idx];
                    }
                    cluster_phase /= eigenstructure.dominant_cluster.len() as f32;

                    let phase_coupling = 0.05 * (1.0 - temperature);
                    self.phase[i] += phase_coupling * (cluster_phase - self.phase[i]).sin();
                }
            }

            if alignment < 0.05 {
                let reduction = noise_reduction * (0.1 - alignment);
                self.energy[i] = (self.energy[i] - reduction).max(dream_energy_floor);
            }

            if (0.05..=0.1).contains(&alignment) {
                let exploration = temperature * 0.005;
                self.phase[i] += exploration * (alignment * 10.0 - 0.5).sin();
            }

            self.energy[i] = self.energy[i].max(dream_energy_floor);
        }
    }

    /// Generate hallucinated wavefronts by superposing patterns from different clusters.
    fn generate_hallucinated_wavefronts(
        &mut self,
        eigenstructure: &EigenStructure,
        temperature: f32,
        max_per_call: usize,
    ) -> usize {
        if self.count() < 4 || eigenstructure.dominant_cluster.len() < 2 {
            return 0;
        }

        if temperature < 0.4 {
            return 0;
        }

        let mut hallucinated = 0;
        // Caller decides the absolute cap; temperature shrinks it on cooler cycles.
        let temp_cap = ((temperature * 3.0) as usize).max(1);
        let max_hallucinations = max_per_call.min(temp_cap);

        for _ in 0..max_hallucinations {
            let idx1 = eigenstructure.dominant_cluster[0];

            let mut idx2 = None;
            for i in 0..self.count() {
                if !eigenstructure.dominant_cluster.contains(&i)
                    && eigenstructure.wavefront_alignments[i].abs() > 0.1
                {
                    idx2 = Some(i);
                    break;
                }
            }

            if let Some(idx2) = idx2 {
                let vec1 = self.wavefronts.row(idx1);
                let vec2 = self.wavefronts.row(idx2);

                let mix_ratio = 0.6 + temperature * 0.3;
                let mut new_vector = Vec::with_capacity(vec1.len());
                for (v1, v2) in vec1.iter().zip(vec2.iter()) {
                    new_vector.push(mix_ratio * v1 + (1.0 - mix_ratio) * v2);
                }

                let norm: f32 = new_vector.iter().map(|x| x * x).sum::<f32>().sqrt();
                if norm > 1e-6 {
                    for x in &mut new_vector {
                        *x /= norm;
                    }

                    let energy = (self.energy[idx1] + self.energy[idx2]) / 2.0 * 0.8;
                    let phase = (self.phase[idx1] + self.phase[idx2]) / 2.0;

                    let content = format!(
                        "HALLUCINATION: superposition of patterns {idx1}-{idx2} [temp={temperature:.2}]"
                    );

                    if self.add_wavefront(&new_vector, content, energy).is_ok() {
                        let new_idx = self.count() - 1;
                        self.phase[new_idx] = phase;
                        self.metadata[new_idx].hallucinated = true;
                        hallucinated += 1;
                    }
                }
            }
        }

        hallucinated
    }

    /// Prune wavefronts with energy below threshold (forgetting during dreams).
    fn prune_low_energy_wavefronts(&mut self, threshold: f32) -> usize {
        let to_remove: Vec<Uuid> = (0..self.count())
            .filter(|&i| self.energy[i] < threshold)
            .map(|i| self.metadata[i].id)
            .collect();

        let removed_count = to_remove.len();
        for id in to_remove {
            self.remove_wavefront(&id);
        }
        removed_count
    }

    /// Apply chiral field perturbation to break phase lock-step.
    ///
    /// Computes a field-level order parameter (Kuramoto-style) and applies
    /// phase noise proportional to each wavefront's energy.
    pub fn apply_chiral_field_perturbation(&mut self, eta: f32) {
        let n = self.count();
        if n < 2 { return; }

        let sum_cos: f32 = (0..n).map(|i| self.phase[i].cos()).sum();
        let sum_sin: f32 = (0..n).map(|i| self.phase[i].sin()).sum();
        let order = (sum_cos * sum_cos + sum_sin * sum_sin).sqrt() / n as f32;

        let strength = eta * order;

        let mean_energy = self.energy.slice(s![..self.len]).sum() / n as f32;
        if mean_energy < 1e-8 { return; }

        for i in 0..n {
            let noise = strength * (self.energy[i] / mean_energy);
            self.phase[i] += noise * (self.frequency[i] * 7.0 + self.phase[i] * 13.0).sin();
        }
    }

    /// Get the wavefront vector for a given ID.
    pub fn get_wavefront(&self, id: &Uuid) -> Option<Vec<f32>> {
        self.id_to_index.get(id).map(|&idx| self.wavefronts.row(idx).to_vec())
    }

    /// Get the energy for a given wavefront ID.
    pub fn get_energy(&self, id: &Uuid) -> Option<f32> {
        self.id_to_index.get(id).map(|&idx| self.energy[idx])
    }

    /// Adapt a vector to this hemisphere's dimensions (truncate or zero-pad).
    fn adapt_vector(vector: &[f32], target_dims: usize) -> Vec<f32> {
        let mut adapted = vec![0.0f32; target_dims];
        let len = vector.len().min(target_dims);
        adapted[..len].copy_from_slice(&vector[..len]);
        adapted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn left_hemisphere_no_dampening() {
        let mut left = Hemisphere::new(Hand::Left, 100);
        // Add two similar wavefronts
        let v1: Vec<f32> = (0..100).map(|i| (i as f32 * 0.1).sin()).collect();
        let v2: Vec<f32> = (0..100).map(|i| (i as f32 * 0.1 + 0.1).sin()).collect();
        left.add_wavefront(&v1, "test1".into(), 0.8).unwrap();
        left.add_wavefront(&v2, "test2".into(), 0.7).unwrap();

        let energy_before: f32 = left.energy.sum();
        left.apply_dynamics(0.1);
        let energy_after: f32 = left.energy.sum();

        // Left hemisphere should not lose energy to dampening
        // (may gain from constructive interference)
        assert!(energy_after >= energy_before - 0.001,
            "Left hemisphere energy should not decrease: before={energy_before}, after={energy_after}");
    }

    #[test]
    fn right_hemisphere_has_dampening() {
        let mut right = Hemisphere::new(Hand::Right, 100);
        // Add wavefronts with no constructive interference (orthogonal)
        let mut v1 = vec![0.0f32; 100];
        v1[0] = 1.0;
        let mut v2 = vec![0.0f32; 100];
        v2[50] = 1.0;
        right.add_wavefront(&v1, "test1".into(), 5.0).unwrap();
        right.add_wavefront(&v2, "test2".into(), 5.0).unwrap();

        let energy_before: f32 = right.energy.sum();
        // Apply many steps to accumulate dampening
        for _ in 0..100 {
            right.apply_dynamics(0.1);
        }
        let energy_after: f32 = right.energy.sum();

        assert!(energy_after < energy_before,
            "Right hemisphere should lose energy to dampening: before={energy_before}, after={energy_after}");
    }

    /// ADR-0046 energy-neutral ranking: a high-energy "favorite" (frequently
    /// recalled, energy near the 2.0 cap) must NOT bury a higher-similarity
    /// cold memory when the energy exponent is 0; and the default exponent
    /// (1.0) must preserve today's similarity*energy ordering exactly.
    #[test]
    fn energy_neutral_ranking_surfaces_cold_target() {
        let _scoring = TEMPORAL_SCORING_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let mut h = Hemisphere::new(Hand::Left, 64);

        // Cold target: the query IS this vector (similarity ~1.0), baseline energy.
        let target: Vec<f32> = (0..64).map(|i| (i as f32 * 0.37).sin()).collect();
        let target_id = h.add_wavefront(&target, "cold target".into(), 0.5).unwrap();

        // Favorite: partially similar (mix of target + noise), energy at the cap.
        let favorite: Vec<f32> = (0..64)
            .map(|i| 0.5 * (i as f32 * 0.37).sin() + 0.8 * (i as f32 * 1.13).cos())
            .collect();
        let favorite_id = h.add_wavefront(&favorite, "recalled favorite".into(), 2.0).unwrap();

        // Historical ranking (exp=1.0): energy wins — favorite outranks target.
        // Still reachable by explicit exponent; no longer the default.
        let historical = h.resonate_with_energy_exp(&target, 2, 1.0);
        assert_eq!(historical[0].id, favorite_id,
            "with similarity*energy the high-energy favorite should win (bias under test)");

        // Energy-neutral (exp=0.0): pure similarity — the cold target surfaces.
        let neutral = h.resonate_with_energy_exp(&target, 2, 0.0);
        assert_eq!(neutral[0].id, target_id,
            "with pure-similarity ranking the cold exact-match target must win");

        // #965: resonate() with no env override is the NEUTRAL ranking now.
        // This is the contract that changed — the default must surface the
        // cold exact match, not the recalled favorite.
        std::env::remove_var("KANNAKA_RECALL_ENERGY_EXP");
        let via_env_default = h.resonate(&target, 2);
        let ids_a: Vec<_> = neutral.iter().map(|r| r.id).collect();
        let ids_b: Vec<_> = via_env_default.iter().map(|r| r.id).collect();
        assert_eq!(ids_a, ids_b, "default resonate() must equal explicit exp=0.0 (#965)");
        assert_eq!(via_env_default[0].id, target_id,
            "by default the cold exact-match target must outrank the high-energy favorite");
    }

    /// L8 temporal ranking: a SUPERSEDED fact (past its `expires_at`) must not
    /// outrank the fact that replaced it, while the historical ranking — which
    /// reads no timestamp at all — cannot tell them apart.
    ///
    /// Also pins the two properties the L8 gates depend on:
    /// `temporal_exp = 0.0` is byte-identical to the pre-L8 path, and the
    /// superseded fact stays RETRIEVABLE (demoted, never evicted).
    #[test]
    fn temporal_ranking_prefers_the_confirmed_fact_without_losing_the_past() {
        let _scoring = TEMPORAL_SCORING_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        use chrono::{Duration, Utc};

        let mut h = Hemisphere::new(Hand::Left, 64);
        let now = Utc::now();

        // Two near-identical claims — the same fact, different values. By
        // construction similarity cannot separate them; only time can.
        let base: Vec<f32> = (0..64).map(|i| (i as f32 * 0.37).sin()).collect();
        let old_vec: Vec<f32> = base.iter().map(|v| v + 0.02).collect();
        let new_vec: Vec<f32> = base.iter().map(|v| v - 0.02).collect();

        let old_id = h.add_wavefront(&old_vec, "channel is twelve".into(), 1.0).unwrap();
        let new_id = h.add_wavefront(&new_vec, "channel is twentyseven".into(), 1.0).unwrap();

        // The old claim was observed 400d ago and STOPPED being true 200d ago,
        // exactly when the new one was observed.
        for m in h.metadata.iter_mut() {
            if m.id == old_id {
                m.observed_at = Some(now - Duration::days(400));
                m.expires_at = Some(now - Duration::days(200));
            } else if m.id == new_id {
                m.observed_at = Some(now - Duration::days(2));
            }
        }

        // Query is the shared base — deliberately equidistant from both.
        let hist = h.resonate_with_energy_exp(&base, 2, 1.0);
        let unset = h.resonate(&base, 2);
        assert_eq!(
            hist.iter().map(|r| r.id).collect::<Vec<_>>(),
            unset.iter().map(|r| r.id).collect::<Vec<_>>(),
            "temporal ranking must be OFF by default (byte-identical to pre-L8)"
        );

        // With the temporal factor on, the currently-true claim must win.
        let temporal = h.resonate_with_weights(&base, 2, 1.0, 1.0, 180.0);
        assert_eq!(
            temporal[0].id, new_id,
            "the confirmed, still-true claim must outrank the superseded one"
        );

        // ...and the superseded one must still be THERE. Demotion, not deletion:
        // an agent has to be able to answer "what did we use before".
        assert!(
            temporal.iter().any(|r| r.id == old_id),
            "superseded fact must remain retrievable, not be evicted (L8 P3)"
        );
    }

    /// `SuppressTemporalScoring` flips a PROCESS-GLOBAL atomic, and the two
    /// ranking tests above read the same global through `resonate()`. Without
    /// this lock a guard held in one test silently zeroes the temporal factor
    /// in another running beside it — a failure that would look like a ranking
    /// bug and reproduce only under `--test-threads` > 1.
    static TEMPORAL_SCORING_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    // ── ADR-0051 M9: the dedup guard ─────────────────────────────────────
    //
    // The hazard is not "the number is wrong" — it is that a suppression
    // leaking past its scope would silently turn off temporal ranking for the
    // rest of the process, with nothing failing to announce it.

    /// The flag's DEFAULT is 0.0, so a guard test run with the env var unset
    /// would assert `0.0 == 0.0` and hold whether or not the guard did
    /// anything. Every test below turns the flag ON first: suppression is only
    /// observable against a non-zero baseline.
    const LIVE_EXP: &str = "0.6";

    /// Sets the flag for one test and removes it after, so a suppression test
    /// cannot leak an ENABLED flag into the rest of the suite.
    struct LiveTemporalFlag;

    impl LiveTemporalFlag {
        fn new() -> Self {
            std::env::set_var("KANNAKA_RECALL_TEMPORAL_EXP", LIVE_EXP);
            Self
        }
    }

    impl Drop for LiveTemporalFlag {
        fn drop(&mut self) {
            std::env::remove_var("KANNAKA_RECALL_TEMPORAL_EXP");
        }
    }

    #[test]
    fn suppress_guard_forces_zero_and_restores_on_drop() {
        let _scoring = TEMPORAL_SCORING_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _flag = LiveTemporalFlag::new();

        let before = recall_temporal_exp();
        assert_eq!(before, 0.6, "baseline must be non-zero or the test proves nothing");
        {
            let _g = SuppressTemporalScoring::new();
            assert_eq!(recall_temporal_exp(), 0.0, "suppressed inside the scope");
        }
        assert_eq!(
            recall_temporal_exp(),
            before,
            "the previous value must come back — a leak silently disables temporal \
             ranking for every later recall in the process"
        );
    }

    #[test]
    fn suppress_guard_nests_without_the_inner_scope_re_enabling() {
        let _scoring = TEMPORAL_SCORING_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _flag = LiveTemporalFlag::new();
        assert_eq!(recall_temporal_exp(), 0.6, "baseline must be non-zero");

        let _outer = SuppressTemporalScoring::new();
        {
            let _inner = SuppressTemporalScoring::new();
            assert_eq!(recall_temporal_exp(), 0.0);
        }
        // The inner guard restores what it FOUND (suppressed), not the default.
        assert_eq!(
            recall_temporal_exp(),
            0.0,
            "an inner scope ending must not re-enable scoring the outer one suppressed"
        );
    }

    #[test]
    fn suppress_guard_restores_even_when_the_scope_unwinds() {
        let _scoring = TEMPORAL_SCORING_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _flag = LiveTemporalFlag::new();

        let before = recall_temporal_exp();
        assert_eq!(before, 0.6, "baseline must be non-zero");

        // Record what the guard was doing, THEN panic. Asserting inside the
        // closure instead would make the test self-satisfying: a failed
        // assertion is itself a panic, so `r.is_err()` would hold precisely
        // when suppression was broken. (Caught by mutating the guard away —
        // this test passed while the other two failed.)
        let inside = std::sync::atomic::AtomicU32::new(u32::MAX);
        let r = std::panic::catch_unwind(|| {
            let _g = SuppressTemporalScoring::new();
            inside.store(recall_temporal_exp().to_bits(), std::sync::atomic::Ordering::Relaxed);
            panic!("dedup blew up mid-recall");
        });
        assert!(r.is_err(), "the panic is the point of the test");
        assert_eq!(
            f32::from_bits(inside.load(std::sync::atomic::Ordering::Relaxed)),
            0.0,
            "the guard must have been suppressing when the panic happened, or the \
             restore below proves nothing"
        );
        assert_eq!(
            recall_temporal_exp(),
            before,
            "Drop must run on unwind — otherwise one failed dedup disables temporal \
             ranking permanently"
        );
    }

    /// The superseded floor is never zero, and a merely-old-but-still-true fact
    /// never sinks below an explicitly expired one.
    // ── RecallAsOf: scoring as of a query time ───────────────────────────

    #[test]
    fn recall_now_is_the_wall_clock_until_pinned_and_again_after() {
        let _scoring = TEMPORAL_SCORING_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        use chrono::{Duration, TimeZone, Utc};

        let before = recall_now();
        assert!(
            (Utc::now() - before).num_seconds().abs() < 5,
            "unpinned, recall_now must be the wall clock"
        );

        let pinned = Utc.with_ymd_and_hms(2023, 5, 2, 0, 0, 0).unwrap();
        {
            let _g = RecallAsOf::new(pinned);
            assert_eq!(recall_now(), pinned, "pinned inside the scope");
        }
        assert!(
            (Utc::now() - recall_now()).num_seconds().abs() < 5,
            "a leaked pin would freeze temporal scoring at a stale instant for \
             every later recall in the process"
        );
        let _ = Duration::days(1);
    }

    #[test]
    fn recall_as_of_nests_and_restores_what_it_found() {
        let _scoring = TEMPORAL_SCORING_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        use chrono::{TimeZone, Utc};

        let outer_at = Utc.with_ymd_and_hms(2023, 5, 2, 0, 0, 0).unwrap();
        let inner_at = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
        let _outer = RecallAsOf::new(outer_at);
        {
            let _inner = RecallAsOf::new(inner_at);
            assert_eq!(recall_now(), inner_at);
        }
        assert_eq!(
            recall_now(),
            outer_at,
            "the inner scope must restore the OUTER pin, not the wall clock"
        );
    }

    #[test]
    fn recall_as_of_restores_even_when_the_scope_unwinds() {
        let _scoring = TEMPORAL_SCORING_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        use chrono::{TimeZone, Utc};

        let pinned = Utc.with_ymd_and_hms(2023, 5, 2, 0, 0, 0).unwrap();
        // Record inside, panic, assert outside — asserting inside the closure
        // would make the test self-satisfying, since a failed assertion is
        // itself the panic `is_err` checks for.
        let seen = std::sync::Mutex::new(None);
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _g = RecallAsOf::new(pinned);
            *seen.lock().unwrap() = Some(recall_now());
            panic!("recall blew up mid-scan");
        }));
        assert!(r.is_err(), "the panic is the point of the test");
        assert_eq!(*seen.lock().unwrap(), Some(pinned), "the pin was in force");
        assert!(
            (chrono::Utc::now() - recall_now()).num_seconds().abs() < 5,
            "Drop must run on unwind, or one failed recall pins the process forever"
        );
    }

    /// THE reason `--at` exists.
    ///
    /// `temporal_weight` decays from `observed_at` to *now* and clamps the
    /// result up to the superseded floor. That clamp binds at two half-lives
    /// (360 days by default), so on a store whose contents are older than that
    /// — a 2023 corpus read in 2026, say — every candidate returns exactly the
    /// floor. The factor becomes a CONSTANT multiplier: it cannot reorder
    /// anything, it does not fail, and nothing reports it. Measured on
    /// longmemeval: flag off and flag on gave byte-identical rankings.
    ///
    /// Scoring as of the time the question is about restores the signal.
    #[test]
    fn as_of_restores_temporal_ranking_that_the_floor_clamp_had_flattened() {
        let _scoring = TEMPORAL_SCORING_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        use chrono::{Duration, TimeZone, Utc};

        let mut hemi = Hemisphere::new(Hand::Left, 64);

        // Two equally-similar claims, observed three months apart in 2023 —
        // the shape of a real recorded corpus. Similarity cannot separate
        // them, so only the temporal factor can.
        let base: Vec<f32> = (0..64).map(|i| (i as f32 * 0.37).sin()).collect();
        let old_vec: Vec<f32> = base.iter().map(|v| v + 0.02).collect();
        let new_vec: Vec<f32> = base.iter().map(|v| v - 0.02).collect();
        let old_id = hemi.add_wavefront(&old_vec, "target is oracle-one".into(), 1.0).unwrap();
        let new_id = hemi.add_wavefront(&new_vec, "target is oracle-three".into(), 1.0).unwrap();

        let feb = Utc.with_ymd_and_hms(2023, 2, 1, 0, 0, 0).unwrap();
        let may = Utc.with_ymd_and_hms(2023, 5, 1, 0, 0, 0).unwrap();
        for m in hemi.metadata.iter_mut() {
            if m.id == old_id {
                m.observed_at = Some(feb);
            } else if m.id == new_id {
                m.observed_at = Some(may);
            }
        }

        let floor = TEMPORAL_SUPERSEDED_FLOOR;

        // Against the wall clock both are ~2 years old, past the clamp point,
        // so both pin to the floor and the factor is a constant.
        let wall = chrono::Utc::now();
        let w_old = temporal_weight(
            hemi.metadata.iter().find(|m| m.id == old_id).unwrap(), wall, 180.0, floor);
        let w_new = temporal_weight(
            hemi.metadata.iter().find(|m| m.id == new_id).unwrap(), wall, 180.0, floor);
        assert!(wall - may > Duration::days(360), "the premise: the corpus is past the clamp");
        assert_eq!(w_old, floor);
        assert_eq!(w_new, floor);
        assert_eq!(w_old, w_new, "flattened: a constant cannot rank anything");

        // As of the day after the newer claim, the two separate, both ABOVE
        // the floor — the factor discriminates again.
        let asked = Utc.with_ymd_and_hms(2023, 5, 2, 0, 0, 0).unwrap();
        let _pin = RecallAsOf::new(asked);
        assert_eq!(recall_now(), asked);
        let a_old = temporal_weight(
            hemi.metadata.iter().find(|m| m.id == old_id).unwrap(), recall_now(), 180.0, floor);
        let a_new = temporal_weight(
            hemi.metadata.iter().find(|m| m.id == new_id).unwrap(), recall_now(), 180.0, floor);
        assert!(a_new > a_old, "the recently-confirmed claim must weigh more");
        assert!(a_old > floor, "and neither may be pinned to the floor");

        // End to end through the ranking path the CLI uses.
        let ranked = hemi.resonate_with_weights(&base, 2, 1.0, 1.0, 180.0);
        assert_eq!(
            ranked[0].id, new_id,
            "as of the question's date, the newer claim outranks the older one"
        );
        assert!(
            ranked.iter().any(|r| r.id == old_id),
            "and the older one stays retrievable — demotion, not deletion"
        );
    }

    #[test]
    fn temporal_weight_floors_and_orders_correctly() {
        use chrono::{Duration, Utc};

        let now = Utc::now();
        let mut meta = WavefrontMeta::new(Uuid::new_v4(), "x".into());

        // Fresh + current → ~1.0
        meta.observed_at = Some(now - Duration::days(1));
        let fresh = temporal_weight(&meta, now, 180.0, 0.25);
        assert!(fresh > 0.99, "a just-confirmed fact should keep ~full weight, got {fresh}");

        // Ancient but still true → floored, never zero.
        meta.observed_at = Some(now - Duration::days(20_000));
        let ancient = temporal_weight(&meta, now, 180.0, 0.25);
        assert!((ancient - 0.25).abs() < 1e-6, "old-but-true clamps to the floor, got {ancient}");

        // Expired → floor, and never above a still-true fact of any age.
        meta.observed_at = Some(now - Duration::days(1));
        meta.expires_at = Some(now - Duration::days(1));
        let expired = temporal_weight(&meta, now, 180.0, 0.25);
        assert!((expired - 0.25).abs() < 1e-6, "expired clamps to the floor, got {expired}");
        assert!(expired <= ancient, "an expired fact must never outweigh a still-true one");
        assert!(expired > 0.0, "the floor must be non-zero — demotion, not deletion");
    }

    #[test]
    fn hemisphere_store_and_recall() {
        let mut h = Hemisphere::new(Hand::Left, 50);
        let v: Vec<f32> = (0..50).map(|i| (i as f32 * 0.2).cos()).collect();
        let id = h.add_wavefront(&v, "hello world".into(), 0.9).unwrap();

        let results = h.resonate(&v, 5);
        assert!(!results.is_empty());
        assert_eq!(results[0].id, id);
        assert!(results[0].similarity > 0.99);
    }

    /// Regression test for kannaka-memory#83.
    ///
    /// Before the fix, Hemisphere::resonate dropped every candidate whose
    /// raw resonance (signed cos-sim × energy) was non-positive. With
    /// random-projection codebook vectors that can be most of the medium
    /// for a given query direction — so a top_k=5 request against a 5-
    /// memory hemisphere routinely returned 0 or 1 result. Now we drop
    /// only zero-norm wavefronts; signed values are kept and ranked by
    /// the final boosted resonance, so top_k is filled when there's
    /// enough candidate material to fill it.
    #[test]
    fn hemisphere_resonate_returns_top_k_when_candidates_exist() {
        let mut h = Hemisphere::new(Hand::Left, 64);
        // Five orthogonal-ish unit vectors as stored wavefronts. Cosine
        // similarity to a fresh query vector will land at a mix of
        // signs, exercising the previous-filter regression.
        for seed in 1..=5 {
            let v: Vec<f32> = (0..64)
                .map(|i| ((i as f32 * seed as f32 * 0.3).sin() - 0.5 * (i as f32 * 0.7).cos()))
                .collect();
            h.add_wavefront(&v, format!("memory {seed}"), 0.8).unwrap();
        }
        // Query that's NOT one of the stored vectors — directionally
        // ambiguous, similar to what an unrelated text query produces
        // after the codebook projection.
        let query: Vec<f32> = (0..64).map(|i| ((i as f32 * 13.7).sin())).collect();
        let results = h.resonate(&query, 5);
        assert_eq!(
            results.len(), 5,
            "top_k=5 against 5 stored memories should return 5 results, got {}",
            results.len()
        );
        // Sort is signed-descending (positives first, then negatives) — the
        // top result should be the most-positive resonance.
        assert!(
            results.windows(2).all(|w| w[0].resonance_strength >= w[1].resonance_strength - 1e-6),
            "results must be signed-descending by resonance_strength"
        );
        // All returned IDs distinct (no double-counting from the rerank pool).
        let mut ids: Vec<_> = results.iter().map(|r| r.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 5, "all returned results should be distinct");
    }

    #[test]
    fn hemisphere_remove() {
        let mut h = Hemisphere::new(Hand::Right, 30);
        let v: Vec<f32> = vec![1.0; 30];
        let id = h.add_wavefront(&v, "temp".into(), 0.5).unwrap();
        assert_eq!(h.count(), 1);

        assert!(h.remove_wavefront(&id));
        assert_eq!(h.count(), 0);
    }

    #[test]
    fn hemisphere_adapts_dimensions() {
        let mut h = Hemisphere::new(Hand::Left, 50);
        // Vector larger than hemisphere dims - should truncate
        let v: Vec<f32> = vec![1.0; 100];
        let id = h.add_wavefront(&v, "big".into(), 0.5).unwrap();
        let stored = h.get_wavefront(&id).unwrap();
        assert_eq!(stored.len(), 50);

        // Vector smaller than hemisphere dims - should zero-pad
        let mut h2 = Hemisphere::new(Hand::Right, 100);
        let v_small: Vec<f32> = vec![1.0; 30];
        let id2 = h2.add_wavefront(&v_small, "small".into(), 0.5).unwrap();
        let stored2 = h2.get_wavefront(&id2).unwrap();
        assert_eq!(stored2.len(), 100);
        assert!(stored2[29] > 0.0); // Last real value
        assert!((stored2[30] - 0.0).abs() < 0.001); // Zero-padded
    }
}
