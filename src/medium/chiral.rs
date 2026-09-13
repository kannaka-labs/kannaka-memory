//! ChiralMedium - the brain. Two hemispheres connected by a corpus callosum.
//!
//! This is the top-level structure that replaces `Medium` at the API level.
//! It implements the Chiral Mirror Architecture (ADR-0021):
//! - Left hemisphere: analytical attention, working memory, no dampening
//! - Right hemisphere: holistic patterns, deep association, full ghostmagicOS dynamics
//! - Corpus callosum: bandwidth-limited, balance-seeking bridge
//! - Fano plane: fold algebra for cross-hemisphere projection

use uuid::Uuid;

use crate::encoding::EncodingPipeline;
use crate::geometry;

use super::callosum::CorpusCallosum;
use super::fano::FanoPlane;
use super::hemisphere::Hemisphere;
use super::types::*;
use super::Medium;

/// ADR-0037: is π/φ spiral coupling enabled for deep dreams?
/// **ON by default as of v0.7.0** — the spiral engine is activated. Set
/// `KANNAKA_SPIRAL_DREAM=0|off|false` to opt out (e.g. for byte-identical
/// legacy dreams).
pub(crate) fn spiral_dream_enabled() -> bool {
    std::env::var("KANNAKA_SPIRAL_DREAM")
        .map(|v| !(v == "0" || v.eq_ignore_ascii_case("off") || v.eq_ignore_ascii_case("false")))
        .unwrap_or(true)
}

/// ADR-0037 belief substrate: enable the content-smooth born phase (and disable
/// the legacy toward-phase-0 ingest pull). **Default OFF** — opt in with
/// `KANNAKA_BELIEF_PHASE=1|on|true`, so prod ingest is byte-identical until this
/// is validated. When off, wavefronts are born at phase 0 exactly as before.
pub fn belief_phase_enabled() -> bool {
    std::env::var("KANNAKA_BELIEF_PHASE")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("on") || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Medium-level DREAM_GRAVITY gain (`KANNAKA_DREAM_GRAVITY`, **default 0.0 =
/// OFF**, dreams byte-identical). Distinct from the L5 research harness's
/// `DREAM_GRAVITY` env, which drives the harness's own dream chain — this one
/// gates the post-dream associative gravity pass inside `ChiralMedium::dream`
/// itself, making the gravity×belief interplay measurable on the live medium
/// (L7 arm, `research/program-l7.md`).
pub fn dream_gravity_gain() -> f32 {
    std::env::var("KANNAKA_DREAM_GRAVITY")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|g: &f32| g.is_finite() && *g > 0.0)
        .unwrap_or(0.0)
}

/// ADR-0036 belief-safe merge: opt in to the DESTRUCTIVE resonance-merge apply
/// while the belief substrate is active. **Default OFF** — set
/// `KANNAKA_MERGE_UNDER_BELIEF=1|on|true`. When off (the default), a dream under
/// belief force-downgrades `apply` to `dryrun` (the v0.7.3 safety gate), so
/// merely deploying with `KANNAKA_CONSOLIDATE=on` on a belief field never
/// mutates. When on, apply runs — but with the belief-safe guardrails
/// (mean-centered semantic gate + per-pass absorb cap) that bound the 295→82
/// over-absorb this flag guards against.
pub fn merge_under_belief_enabled() -> bool {
    std::env::var("KANNAKA_MERGE_UNDER_BELIEF")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("on") || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Content-smooth born phase: `atan2` of the embedding projected onto two fixed
/// pseudo-random directions. Similar embeddings → similar phase (recall-safe —
/// constructive interference preserved); the projection winds where the
/// embedding distribution wraps the (u1,u2) origin, seeding genuine topological
/// belief-cores instead of scattering phase like an id/text hash. The two
/// directions are deterministic (fixed seeds + per-index SplitMix64 hash — no
/// stored arrays, any embedding length) so the node HRM and the substrate map
/// identical content to identical phase → the two fields are comparable (the
/// exemplar / two-systems coupling needs this). Scale-invariant (atan2 of a ratio).
pub(crate) fn content_born_phase(vector: &[f32]) -> f32 {
    const SEED1: u64 = 0x9E37_79B9_7F4A_7C15; // direction u1
    const SEED2: u64 = 0xC2B2_AE3D_27D4_EB4F; // direction u2
    // Per-index centered pseudo-random weight in ~[-1, 1) (SplitMix64 finalizer).
    let comp = |i: usize, seed: u64| -> f32 {
        let mut h = seed ^ (i as u64).wrapping_mul(0x2545_F491_4F6C_DD1D);
        h ^= h >> 30;
        h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
        h ^= h >> 27;
        h = h.wrapping_mul(0x94D0_49BB_1331_11EB);
        h ^= h >> 31;
        (h >> 40) as f32 / (1u64 << 23) as f32 - 1.0 // ~[-1, 1)
    };
    let (mut d1, mut d2) = (0.0f32, 0.0f32);
    for (i, &v) in vector.iter().enumerate() {
        d1 += v * comp(i, SEED1);
        d2 += v * comp(i, SEED2);
    }
    let p = d2.atan2(d1); // (-π, π]
    // Never write a non-finite phase (a NaN/Inf embedding would poison the field
    // and get serialized to the .hrm); fall back to 0.0.
    if p.is_finite() { p } else { 0.0 }
}

/// Mean of the active wavefront rows `[0, n)` — the corpus mean used to CENTER a
/// new wavefront's embedding at ingest before `content_born_phase`. Real
/// embeddings are anisotropic (cone-clustered), so an uncentered born phase
/// barely spreads; centering against the corpus mean makes new memories spread
/// like the `rephase_from_content` migration. Computed on-demand (no stored
/// field → no .hrm format change). Returns a zero vector when `n == 0`.
pub(crate) fn corpus_mean(wavefronts: &ndarray::Array2<f32>, n: usize) -> Vec<f32> {
    let dim = wavefronts.ncols();
    let mut mean = vec![0.0f32; dim];
    if n == 0 {
        return mean;
    }
    for i in 0..n {
        for (m, &v) in mean.iter_mut().zip(wavefronts.row(i).iter()) {
            *m += v;
        }
    }
    for m in mean.iter_mut() {
        *m /= n as f32;
    }
    mean
}

/// `content_born_phase` of `vector − mean`. With an empty mean (first insert)
/// this is exactly `content_born_phase(vector)`. NB the corpus mean drifts as the
/// field grows, so ingest-centered phases are approximately (not exactly)
/// consistent with an older migration snapshot — close enough to spread new
/// memories; `rephase_from_content` re-centers the whole field exactly.
pub(crate) fn content_born_phase_centered(vector: &[f32], mean: &[f32]) -> f32 {
    if mean.is_empty() {
        return content_born_phase(vector);
    }
    let centered: Vec<f32> = vector.iter().zip(mean.iter()).map(|(&v, &m)| v - m).collect();
    content_born_phase(&centered)
}

/// The Chiral Medium - two hemispheres connected by a corpus callosum.
#[derive(Debug, Clone)]
pub struct ChiralMedium {
    /// Left hemisphere: analytical, attention, working memory
    pub left: Hemisphere,
    /// Right hemisphere: holistic, pattern storage, deep association
    pub right: Hemisphere,
    /// Corpus callosum: selective bridge between hemispheres
    pub callosum: CorpusCallosum,
    /// Fano plane: fold algebra for cross-hemisphere projection
    pub fano: FanoPlane,
    /// Per-wavefront chiral scales (indexed by right-hemisphere wavefront ID)
    pub scales: std::collections::HashMap<Uuid, ChiralScale>,
    /// Left-to-right ID mapping (which right-hemisphere echo corresponds to each left wavefront)
    pub left_to_right: std::collections::HashMap<Uuid, Uuid>,
    /// Right-to-left ID mapping (reverse)
    pub right_to_left: std::collections::HashMap<Uuid, Uuid>,
}

/// Chiral-router mode (env `KANNAKA_CHIRAL_ROUTER`). `off` (default) is today's
/// behavior — the ingest echoes every gated item to left (near-mirror). `novelty`
/// is the differentiation experiment: novel items stay RIGHT-only, and only the
/// routinized minority (familiarity crossing, per the cerebellar novelty
/// detector) is projected to LEFT — so left becomes a crystallized minority, not
/// a mirror. Isolated behind an env flag so the A/B needs no rebuild.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChiralRouter {
    Off,
    Novelty,
}

pub(crate) fn chiral_router_mode() -> ChiralRouter {
    match std::env::var("KANNAKA_CHIRAL_ROUTER") {
        Ok(v) if v.eq_ignore_ascii_case("novelty") => ChiralRouter::Novelty,
        _ => ChiralRouter::Off,
    }
}

/// Routinization familiarity threshold for `KANNAKA_CHIRAL_ROUTER=novelty`
/// (sweepable via `KANNAKA_CHIRAL_ROUTINIZE_THETA`, default 0.8). An ingest whose
/// content already resonates in RIGHT at/above this — i.e. a REPEAT — is
/// routinized to LEFT; a novel item (below it) stays right-only. Absolute
/// familiarity, not the relative-surprise novelty detector: at ingest the stream
/// is mostly novel so a learned-baseline surprise signal can't separate repeats.
pub(crate) fn chiral_routinize_theta() -> f32 {
    std::env::var("KANNAKA_CHIRAL_ROUTINIZE_THETA")
        .ok()
        .and_then(|v| v.parse::<f32>().ok())
        .unwrap_or(0.8)
}

/// Read-side hemisphere differentiation (exp-2). The write-side router
/// (`KANNAKA_CHIRAL_ROUTER`) crystallizes a routinized minority into LEFT; this
/// decides how `recall_vector` USES that minority. `Off` (default) is the current
/// resonance-ranked union of both hemispheres — byte-identical to before.
/// `Weighted` boosts left matches so a routinized memory resists eviction when a
/// novel flood degrades its right-hemisphere resonance. `Beeman` orders the
/// precise-left (fine) hits ahead of the associative-right (coarse) backfill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChiralRecall {
    Off,
    Weighted,
    Beeman,
}

pub(crate) fn chiral_recall_mode() -> ChiralRecall {
    let mode = match std::env::var("KANNAKA_CHIRAL_RECALL") {
        Ok(v) if v.eq_ignore_ascii_case("weighted") => ChiralRecall::Weighted,
        Ok(v) if v.eq_ignore_ascii_case("beeman") => ChiralRecall::Beeman,
        _ => ChiralRecall::Off,
    };
    // One-time LOUD warning when a non-default mode is active. These are
    // EXPERIMENTAL read-side modes: `beeman` is measured-CATASTROPHIC (exp-2: core
    // p@1 0.65 -> 0.075) because left stores Fano-folded vectors and recall queries
    // raw — it stays a footgun until the query-fold (exp-2b / #70) makes left-match
    // resonance meaningful. Without this, an operator who set KANNAKA_CHIRAL_RECALL
    // via a stale shell/systemd env would get silent recall collapse.
    if mode != ChiralRecall::Off {
        static WARNED: std::sync::Once = std::sync::Once::new();
        WARNED.call_once(|| {
            eprintln!(
                "[kannaka] WARNING: KANNAKA_CHIRAL_RECALL={mode:?} is an EXPERIMENTAL read-side \
                 recall mode; beeman is measured-catastrophic (exp-2) pending exp-2b. Unset \
                 KANNAKA_CHIRAL_RECALL to restore default recall."
            );
        });
    }
    mode
}

/// Left-match resonance multiplier for `KANNAKA_CHIRAL_RECALL=weighted`
/// (`KANNAKA_CHIRAL_RECALL_BOOST`, default 1.5). >1 lets the crystallized left
/// survive a novel flood; 1.0 is a no-op (equivalent to Off's ranking).
pub(crate) fn chiral_recall_boost() -> f32 {
    std::env::var("KANNAKA_CHIRAL_RECALL_BOOST")
        .ok()
        .and_then(|v| v.parse::<f32>().ok())
        .unwrap_or(1.5)
}

impl ChiralMedium {
    /// Create a new empty chiral medium.
    pub fn new() -> Self {
        let default_dims = BASE_DIMS_PER_POSITION * 2; // 672 * 2 = 1344 default dims per side
        Self {
            left: Hemisphere::new(Hand::Left, default_dims),
            right: Hemisphere::new(Hand::Right, default_dims),
            callosum: CorpusCallosum::new(),
            fano: FanoPlane::new(),
            scales: std::collections::HashMap::new(),
            left_to_right: std::collections::HashMap::new(),
            right_to_left: std::collections::HashMap::new(),
        }
    }

    /// Create a ChiralMedium from an existing (v1) Medium.
    /// All existing wavefronts go to the right hemisphere (they're already consolidated).
    /// Left hemisphere starts empty (fresh analytical workspace).
    pub fn from_medium(medium: &Medium) -> Self {
        let dims = WAVEFRONT_DIM; // Existing medium uses 10,000 dims
        let mut right = Hemisphere::new(Hand::Right, dims);

        // Migrate all existing wavefronts to right hemisphere
        for i in 0..medium.wavefront_count() {
            let vector = medium.store.wavefronts.row(i).to_vec();
            let meta = &medium.store.metadata[i];
            let energy = medium.store.energy[i];

            let id = meta.id;
            let index = right.count();

            // Build tensors manually to preserve original IDs
            let n = right.count();
            let mut new_wf = ndarray::Array2::zeros((n + 1, dims));
            if n > 0 {
                new_wf.slice_mut(ndarray::s![..n, ..]).assign(&right.wavefronts);
            }
            for (j, &val) in vector.iter().enumerate() {
                if j < dims { new_wf[[index, j]] = val; }
            }

            let mut new_energy = ndarray::Array1::zeros(n + 1);
            let mut new_freq = ndarray::Array1::zeros(n + 1);
            let mut new_phase = ndarray::Array1::zeros(n + 1);
            if n > 0 {
                new_energy.slice_mut(ndarray::s![..n]).assign(&right.energy);
                new_freq.slice_mut(ndarray::s![..n]).assign(&right.frequency);
                new_phase.slice_mut(ndarray::s![..n]).assign(&right.phase);
            }
            new_energy[index] = energy;
            new_freq[index] = medium.store.frequency[i];
            new_phase[index] = medium.store.phase[i];

            right.wavefronts = new_wf;
            right.energy = new_energy;
            right.frequency = new_freq;
            right.phase = new_phase;
            right.timestamps.push(medium.store.timestamps[i]);
            right.metadata.push(meta.clone());
            right.id_to_index.insert(id, index);
            right.len += 1;
        }

        let mut chiral = Self {
            left: Hemisphere::new(Hand::Left, dims),
            right,
            callosum: CorpusCallosum::new(),
            fano: FanoPlane::new(),
            scales: std::collections::HashMap::new(),
            left_to_right: std::collections::HashMap::new(),
            right_to_left: std::collections::HashMap::new(),
        };

        // Assign deep memory scales to all migrated wavefronts
        for meta in &chiral.right.metadata {
            chiral.scales.insert(meta.id, ChiralScale::deep_memory());
        }

        chiral
    }

    /// Total wavefront count across both hemispheres.
    pub fn total_count(&self) -> usize {
        self.left.count() + self.right.count()
    }

    /// Store a new memory using the encoding pipeline.
    ///
    /// Follows the optic chiasm principle: input enters the opposite hemisphere
    /// (right/holistic) first, then an echo crosses to left (analytical) via callosum.
    pub fn store(
        &mut self,
        content: &str,
        importance: f32,
        pipeline: &EncodingPipeline,
    ) -> Result<Uuid, MediumError> {
        self.store_with_category(content, importance, pipeline, None)
    }


    /// ADR-0049 step 5 — store `content` and, when decomposition is enabled and
    /// the content is compound, also mint atomic facet wavefronts linked to it.
    ///
    /// Returns the **parent** id, so every existing caller keeps its contract:
    /// `remember` still hands back one id for one memory. The facets are an
    /// internal reach mechanism, not a change to what a memory *is*.
    ///
    /// Off by default (`KANNAKA_FACET_DECOMPOSE`). With the flag unset this is
    /// exactly `store_with_category` — no extra rows, no metadata writes.
    pub fn store_with_facets(
        &mut self,
        content: &str,
        importance: f32,
        pipeline: &EncodingPipeline,
        category: Option<&str>,
    ) -> Result<Uuid, MediumError> {
        let parent = self.store_with_category(content, importance, pipeline, category)?;
        if !crate::facet::decompose_enabled() {
            return Ok(parent);
        }
        let facets = crate::facet::decompose(content);
        if facets.len() < 2 {
            return Ok(parent);
        }
        // Facets are stored via the PLAIN store: a facet must never itself be
        // decomposed, and `store` cannot recurse back into this function.
        let mut facet_ids = Vec::with_capacity(facets.len());
        for f in &facets {
            facet_ids.push(self.store(f, importance, pipeline)?);
        }
        self.link_facets(parent, &facet_ids);
        Ok(parent)
    }

    /// Mark a decomposed constellation: parent resolve-only, facets linked back.
    ///
    /// Idempotent by construction — re-running sets the same flags to the same
    /// values. The `decomposed` flag is also the once-only guard for backfill.
    pub(crate) fn link_facets(&mut self, parent: Uuid, facet_ids: &[Uuid]) {
        for m in self
            .right
            .metadata
            .iter_mut()
            .chain(self.left.metadata.iter_mut())
        {
            if m.id == parent {
                m.decomposed = true;
            } else if facet_ids.contains(&m.id) {
                m.is_facet = true;
                m.parent_id = Some(parent);
            }
        }
    }

    /// Has this memory already been decomposed? The backfill watermark.
    pub(crate) fn is_decomposed(&self, id: Uuid) -> bool {
        self.right
            .metadata
            .iter()
            .chain(self.left.metadata.iter())
            .any(|m| m.id == id && m.decomposed)
    }

    /// Backfill: decompose one already-stored memory. Returns the number of
    /// facets minted (0 = skipped).
    ///
    /// **Once-only and idempotent.** Skips anything already `decomposed`, and
    /// never decomposes a row that is itself a facet — without both guards a
    /// resumed backfill would mint duplicate facet sets on every pass, and each
    /// duplicate is another wavefront competing in the scan forever.
    pub fn backfill_facets(
        &mut self,
        id: Uuid,
        pipeline: &EncodingPipeline,
    ) -> Result<usize, MediumError> {
        // #699: canonicalize a hemisphere-local target to its right twin
        // FIRST. The `decomposed` watermark lives per-row, so a caller that
        // sweeps both hemispheres' ids (the eval backfill driver does) used
        // to decompose each memory TWICE — the left pass minted a duplicate
        // facet set whose parent_id was the left-local id, an id no
        // canonical consumer can resolve (recall rows silently dropped
        // downstream). Canonicalizing makes the second pass hit the
        // watermark and every parent_id canonical.
        let id = if self.right.id_to_index.contains_key(&id) {
            id
        } else if let Some(&rid) = self.left_to_right.get(&id) {
            rid
        } else {
            // Orphaned left row: minting facets under an unresolvable
            // parent would recreate exactly the dead-id defect — skip.
            return Ok(0);
        };
        let meta = self
            .right
            .metadata
            .iter()
            .chain(self.left.metadata.iter())
            .find(|m| m.id == id);
        let Some(meta) = meta else { return Ok(0) };
        if meta.decomposed || meta.is_facet {
            return Ok(0);
        }
        let content = meta.content.clone();
        let importance = 0.9;

        let facets = crate::facet::decompose(&content);
        if facets.len() < 2 {
            return Ok(0);
        }
        let mut facet_ids = Vec::with_capacity(facets.len());
        for f in &facets {
            facet_ids.push(self.store(f, importance, pipeline)?);
        }
        let n = facet_ids.len();
        self.link_facets(id, &facet_ids);
        Ok(n)
    }

    /// Store with explicit category for SGA classification.
    pub fn store_with_category(
        &mut self,
        content: &str,
        importance: f32,
        pipeline: &EncodingPipeline,
        category: Option<&str>,
    ) -> Result<Uuid, MediumError> {
        let vector = pipeline.encode_text(content).map_err(|e| {
            MediumError::Serialization(bincode::Error::from(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("encoding failed: {e}"),
            )))
        })?;

        self.store_vector_with_category(&vector, content.to_string(), importance, category)
    }

    /// Store a pre-encoded vector (used by store() and for audio/visual perception).
    pub fn store_vector(
        &mut self,
        vector: &[f32],
        content: String,
        importance: f32,
    ) -> Result<Uuid, MediumError> {
        self.store_vector_with_category(vector, content, importance, None)
    }

    /// Rewrite a right-hemisphere wavefront's canonical id — the chiral analogue
    /// of [`Medium::update_wavefront_id`] (issue #630).
    ///
    /// `store_vector` mints its own id, but callers that already own a `HyperMemory`
    /// (import, wire sync) must keep theirs: `memory_cache` is keyed on it, and
    /// `parents`/`connections` reference it. Unlike the flat medium, the right id is
    /// load-bearing in FOUR places — the metadata row, `id_to_index`, the `scales`
    /// map, and both hemisphere cross-maps — so rewriting only the metadata (the
    /// flat-path shape) would silently orphan the scale and strand the left echo.
    pub fn update_right_id(&mut self, old_id: &Uuid, new_id: Uuid) -> Result<(), MediumError> {
        if old_id == &new_id {
            return Ok(());
        }
        if self.right.id_to_index.contains_key(&new_id) {
            return Err(MediumError::CorruptHrm(format!(
                "update_right_id: {new_id} already present — refusing to collide two wavefronts"
            )));
        }
        let index = self
            .right
            .id_to_index
            .remove(old_id)
            .ok_or(MediumError::WavefrontNotFound(*old_id))?;
        self.right.id_to_index.insert(new_id, index);
        self.right.metadata[index].id = new_id;

        // Chiral scale is keyed by right id.
        if let Some(scale) = self.scales.remove(old_id) {
            self.scales.insert(new_id, scale);
        }
        // Re-point the left echo, in both directions.
        if let Some(left_id) = self.right_to_left.remove(old_id) {
            self.right_to_left.insert(new_id, left_id);
            self.left_to_right.insert(left_id, new_id);
        }
        Ok(())
    }

    /// Store with explicit category for SGA classification.
    pub fn store_vector_with_category(
        &mut self,
        vector: &[f32],
        content: String,
        importance: f32,
        category: Option<&str>,
    ) -> Result<Uuid, MediumError> {
        // 1. SGA classification - determine the memory's geometric coordinates
        let cat = category.unwrap_or("knowledge");
        let content_hash = {
            let mut h: u64 = 0xcbf29ce484222325;
            for b in content.bytes() {
                h ^= b as u64;
                h = h.wrapping_mul(0x100000001b3);
            }
            h
        };
        let coords = geometry::classify_memory(cat, content_hash, importance as f64);
        let sga_class = coords.class_index;
        let fano_group = coords.l; // ℓ ∈ [0,7) maps to Fano point (mod 7 for safety)
        let fano_point = fano_group % (FANO_POINTS as u8);

        // 2. Determine which Fano fold line to use for callosal transfer
        //    Use the first line through this memory's Fano point
        let fold_line = self.fano.lines_through_point(fano_point)[0] as usize;

        // Chiral router (KANNAKA_CHIRAL_ROUTER=novelty, exp 1): decide whether this
        // ingest should echo to LEFT. In `off` mode every gated item echoes (today's
        // near-mirror). In `novelty` mode a NOVEL item stays RIGHT-only, and only a
        // ROUTINIZED item — familiarity crossing, per the cerebellar novelty detector
        // fed by how strongly the incoming content already resonates in RIGHT — is
        // projected to left, so left crystallizes the routinized minority, not a mirror.
        let is_routine = match chiral_router_mode() {
            ChiralRouter::Off => true,
            ChiralRouter::Novelty => {
                // Absolute familiarity: how strongly does the incoming content
                // already resonate in RIGHT? A REPEAT resonates high; a novel item
                // low. Only a repeat (>= theta) is routinized to left.
                let familiarity = self
                    .right
                    .resonate(vector, 1)
                    .first()
                    .map(|r| r.resonance_strength)
                    .unwrap_or(0.0);
                familiarity >= chiral_routinize_theta()
            }
        };

        // 3. Optic chiasm: input enters RIGHT hemisphere first
        let right_id = self.right.add_wavefront(vector, content.clone(), importance)?;

        // 4. Tag the wavefront with its SGA classification
        if let Some(idx) = self.right.id_to_index.get(&right_id) {
            let idx = *idx;
            self.right.metadata[idx].sga_class = Some(sga_class);
            self.right.metadata[idx].fano_group = Some(fano_point);
            self.right.metadata[idx].category = Some(cat.to_string());
        }

        // 5. Create chiral scale (analytical-dominant for new perception)
        let scale = ChiralScale::perception(importance);
        self.scales.insert(right_id, scale);

        // 6. Echo to LEFT hemisphere via callosum (if budget allows AND — in
        //    novelty-router mode — the item is routinized, not novel).
        //    Uses the geometrically correct fold line for this memory's Fano group.
        if is_routine && self.callosum.try_gate(importance) && self.callosum.has_budget() {
            let folded = self.fano.fold(
                vector,
                self.right.dims,
                self.left.dims,
                fold_line, // Geometrically determined fold line
            );

            let transferred = self.callosum.apply_noise(
                &folded,
                Direction::HolisticToAnalytical,
            );

            let left_id = self.left.add_wavefront(
                &transferred,
                content,
                importance * 0.8,
            )?;

            // Tag left wavefront too
            if let Some(idx) = self.left.id_to_index.get(&left_id) {
                let idx = *idx;
                self.left.metadata[idx].sga_class = Some(sga_class);
                self.left.metadata[idx].fano_group = Some(fano_point);
                self.left.metadata[idx].category = Some(cat.to_string());
            }

            self.left_to_right.insert(left_id, right_id);
            self.right_to_left.insert(right_id, left_id);

            self.callosum.consume_budget(importance);
            self.callosum.log_transfer(
                right_id,
                Direction::HolisticToAnalytical,
                importance,
            );
        }

        Ok(right_id)
    }

    /// Recall: bilateral search with intuition surfacing.
    ///
    /// Searches both hemispheres. Right-hemisphere matches that don't appear
    /// in the left are "intuitions" - patterns the holistic hemisphere found that
    /// analytical processing missed.
    pub fn recall(
        &self,
        query: &str,
        top_k: usize,
        pipeline: &EncodingPipeline,
    ) -> Result<Vec<ChiralResonance>, MediumError> {
        let vector = pipeline.encode_text(query).map_err(|e| {
            MediumError::Serialization(bincode::Error::from(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("encoding failed: {e}"),
            )))
        })?;

        // ── Glyph-gravity recall boost REMOVED (#837) ─────────────────────
        // KANNAKA_GLYPH_GRAVITY used to over-fetch 3× here and multiply
        // same-Fano-line resonance by (1 + gain) before re-sorting. Measured:
        // it never improved a rank, degraded 2 of 4 rankable queries, and
        // lost a rank-1 answer entirely — the boost necessarily demotes
        // correct answers on a different line. The env var is now inert on
        // every recall path; see #837 and ADR-0047 for the successor design.
        Ok(self.recall_vector(&vector, top_k))
    }

    /// Recall with a pre-encoded vector.
    pub fn recall_vector(&self, vector: &[f32], top_k: usize) -> Vec<ChiralResonance> {
        let recall_mode = chiral_recall_mode();
        let trace = std::env::var("KANNAKA_RECALL_TRACE").is_ok();

        // #699: on a facet-bearing corpus, per-hemisphere pools sized for
        // plain top_k starve the post-merge facet resolution — sibling
        // facets of a few hot parents fill the pool before other parents
        // get a slot. Widen the hemisphere fetch by the facet over-fetch
        // factor so resolution has k DISTINCT constellations to choose
        // from. Undecomposed corpora keep the byte-identical pools.
        let fetch_k = if self.has_facets() {
            crate::facet::overfetch_pool(top_k)
        } else {
            top_k
        };

        // 1. Search left hemisphere (analytical - fast, precise)
        let mut left_matches = self.left.resonate(vector, fetch_k);
        if trace {
            eprintln!("[recall-trace] chiral k={} left_n={} right_n={}",
                top_k, self.left.count(), self.right.count());
            for r in &left_matches {
                eprintln!("[recall-trace]   left  sim={:.4} rs={:.4} {}",
                    r.similarity, r.resonance_strength, r.content.chars().take(40).collect::<String>());
            }
        }
        // Read-side differentiation (exp-2, dormant; Off = unchanged). `weighted`
        // boosts left matches so a routinized memory resists eviction when a novel
        // flood degrades its right-hemisphere resonance. Boosting strength does not
        // change ids, so the paired_right_ids bookkeeping below is unaffected.
        if recall_mode == ChiralRecall::Weighted {
            let boost = chiral_recall_boost();
            for r in left_matches.iter_mut() {
                r.resonance_strength *= boost;
            }
        }

        // 2. Search right hemisphere (holistic - deep, associative)
        let right_matches = self.right.resonate(vector, fetch_k * 2);
        if trace {
            for r in &right_matches {
                eprintln!("[recall-trace]   right sim={:.4} rs={:.4} {}",
                    r.similarity, r.resonance_strength, r.content.chars().take(40).collect::<String>());
            }
        }

        // 3. Identify intuitions: right matches not paired with left matches.
        //    Left matches carry hemisphere-local UUIDs; paired_right_ids is
        //    computed BEFORE translation because the left_to_right lookup
        //    needs the local IDs.
        let left_ids: std::collections::HashSet<Uuid> =
            left_matches.iter().map(|r| r.id).collect();
        let paired_right_ids: std::collections::HashSet<Uuid> =
            left_ids.iter()
                .filter_map(|lid| self.left_to_right.get(lid))
                .copied()
                .collect();

        // 4. Translate left matches' local UUIDs → canonical (right) UUIDs
        //    so the caller's store.get(id) can actually resolve them
        //    (kannaka-memory#83). Pre-fix, left matches were emitted with
        //    their hemisphere-local IDs and silently dropped in openclaw's
        //    recall lookup because the canonical store keys on right IDs.
        //    Any left wavefront without a left_to_right mapping is an
        //    orphan (its right counterpart was pruned) — drop it rather
        //    than emit an unresolvable ID.
        let mut results: Vec<ChiralResonance> = left_matches
            .into_iter()
            .filter_map(|mut r| {
                if let Some(&canonical) = self.left_to_right.get(&r.id) {
                    // #699 eval find: the mapping can be STALE — a resonant
                    // merge absorbs the right row but leaves the left row and
                    // its left_to_right entry behind, so translation emits an
                    // id no canonical consumer can resolve (openclaw silently
                    // drops it, shortening result lists). Same rule as the
                    // #83 orphan case: never emit an unresolvable id.
                    if self.right.id_to_index.contains_key(&canonical) {
                        r.id = canonical;
                        Some(r)
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .collect();

        // 5/6. Merge + rank. Beeman (exp-2) keeps the precise-left (fine) hits
        // ahead of the associative-right (coarse) backfill; Off/Weighted rank the
        // full union by resonance strength (Off is byte-identical to the prior
        // behavior — the boost is 1× and the beeman branch is skipped).
        let by_strength = |a: &ChiralResonance, b: &ChiralResonance| {
            b.resonance_strength
                .partial_cmp(&a.resonance_strength)
                .unwrap_or(std::cmp::Ordering::Equal)
        };

        if recall_mode == ChiralRecall::Beeman {
            results.sort_by(&by_strength);
            let mut coarse: Vec<ChiralResonance> = right_matches
                .into_iter()
                .filter(|r| !paired_right_ids.contains(&r.id))
                .map(|mut r| {
                    r.is_intuition = true;
                    r
                })
                .collect();
            coarse.sort_by(&by_strength);
            results.extend(coarse);
            results.truncate(top_k);
            return results;
        }

        // Merge right-hemisphere matches by canonical id, keeping the STRONGER
        // hemisphere's score for a paired memory (kannaka-memory#716b).
        //
        // Pre-fix, a paired right match was dropped outright and the memory
        // surfaced with only its left-hemisphere score. Left rows score content
        // queries near zero (the analytical encoding is not a content
        // embedding; xi's additive tier then lifts everything to ~0.03-0.04),
        // so the moment a memory's left row cracked the left top_k its true
        // right-hemisphere resonance was masked — an exact-text match measured
        // 0.9999 at top_k=3 fell to 0.0334 at top_k=4 on a 5-memory HRM purely
        // because k decided whether its left row surfaced. Scores must not
        // depend on top_k: a paired memory now surfaces with
        // max(left, right) resonance, and is_intuition stays false for it
        // (it lives in both hemispheres — not a right-only insight).
        for mut r in right_matches {
            if paired_right_ids.contains(&r.id) {
                if let Some(existing) = results.iter_mut().find(|e| e.id == r.id) {
                    if r.resonance_strength > existing.resonance_strength {
                        r.is_intuition = false;
                        *existing = r;
                    }
                }
                // Paired but its left row didn't survive translation (orphan):
                // fall through to nothing — the right row was already excluded
                // pre-fix too, and an orphaned pair heals on the next prune.
            } else {
                r.is_intuition = true;
                results.push(r);
            }
        }
        results.sort_by(&by_strength);

        // ADR-0049 facet resolution. This is the path CLI recall actually takes,
        // so it is the one that matters most. Over-fetch only when the medium
        // holds facets — otherwise this is byte-identical to the pre-facet
        // truncate-to-top_k, and an undecomposed corpus pays nothing.
        //
        // Resolution happens AFTER the intuition pass so a right-hemisphere
        // intuition and its left-hemisphere sibling still collapse to one parent
        // rather than surfacing the same memory twice under different hands.
        if !self.has_facets() {
            results.truncate(top_k);
            return results;
        }
        // #699: cap each constellation's contribution BEFORE the pool
        // truncate. A constellation can occupy up to ~11 rows here (parent +
        // MAX_FACETS_PER_PARENT facets, mirrored across both hemispheres), so
        // an uncapped overfetch_pool(k) = 6k pool holds as few as ~k/2
        // distinct constellations — measured on the frozen eval corpus:
        // k=20 returned 10-11 rows even with widened hemisphere fetches.
        // With a 2-row cap (best + one spare), the same pool spans >= 3k
        // constellations and resolve can actually fill k distinct slots.
        let mut per_parent: std::collections::HashMap<Uuid, usize> =
            std::collections::HashMap::new();
        results.retain(|r| {
            let canonical = self
                .parent_of_facet(r.id)
                .map(|(pid, _)| pid)
                .unwrap_or(r.id);
            let n = per_parent.entry(canonical).or_insert(0);
            *n += 1;
            *n <= 2
        });
        results.truncate(crate::facet::overfetch_pool(top_k));
        if trace {
            let facets = results
                .iter()
                .filter(|r| self.parent_of_facet(r.id).is_some())
                .count();
            eprintln!(
                "[recall-trace] facet stage: pool={} (facet rows {}), resolving to top_k={}",
                results.len(),
                facets,
                top_k
            );
        }
        let resolved = crate::facet::resolve(results, top_k, |id| self.parent_of_facet(id));
        if trace {
            eprintln!("[recall-trace] facet stage: resolved -> {} rows", resolved.len());
            for r in &resolved {
                let in_right = self.right.id_to_index.contains_key(&r.id);
                let in_left_meta = self.left.metadata.iter().any(|m| m.id == r.id);
                eprintln!(
                    "[recall-trace]   resolved id={} right={} leftmeta={} {}",
                    r.id,
                    in_right,
                    in_left_meta,
                    r.content.chars().take(30).collect::<String>()
                );
            }
        }
        resolved
    }

    /// Does either hemisphere hold facet rows? Cheap guard so an undecomposed
    /// corpus never pays for resolution or over-fetch.
    pub(crate) fn has_facets(&self) -> bool {
        self.right.metadata.iter().any(|m| m.is_facet)
            || self.left.metadata.iter().any(|m| m.is_facet)
    }

    /// Facet → parent `(id, content)`, searched right-hemisphere-first (the
    /// authoritative side). `None` when `id` is not a facet or its parent is
    /// absent — the caller then surfaces the facet itself rather than dropping it
    /// or attributing it to a parent that is not there.
    pub(crate) fn parent_of_facet(&self, id: uuid::Uuid) -> Option<(uuid::Uuid, String)> {
        let find = |target: uuid::Uuid| {
            self.right
                .metadata
                .iter()
                .find(|m| m.id == target)
                .or_else(|| self.left.metadata.iter().find(|m| m.id == target))
        };
        let m = find(id)?;
        if !m.is_facet {
            return None;
        }
        let pid = m.parent_id?;
        if let Some(parent) = self.right.metadata.iter().find(|m| m.id == pid) {
            return Some((parent.id, parent.content.clone()));
        }
        // #699 eval find: when the parent is only found in LEFT metadata, its
        // id is hemisphere-local — emitting it produces a row no canonical
        // consumer can resolve (openclaw drops it silently, shortening recall
        // lists) AND splits the constellation into a live right copy and a
        // dead left twin. Canonicalize through left_to_right; if that fails,
        // return None so the facet surfaces itself (a resolvable row) rather
        // than fabricating an unresolvable parent.
        let left_parent = self.left.metadata.iter().find(|m| m.id == pid)?;
        let canonical = *self.left_to_right.get(&left_parent.id)?;
        let idx = *self.right.id_to_index.get(&canonical)?;
        Some((canonical, self.right.metadata[idx].content.clone()))
    }

    /// Re-encode every stored memory's content through `pipeline`, replacing the
    /// wavefront vectors IN PLACE while preserving all wave-state (energy, phase,
    /// frequency) and metadata (#107). This fixes an HRM whose vectors were written
    /// by a broken encoder (#106) without disturbing dream energies, ghost stamps,
    /// or ids. The RIGHT hemisphere (authoritative) is re-encoded from its stored
    /// content; each paired LEFT wavefront is re-derived as the Fano fold of the
    /// corrected right vector on the SAME line it was stored on (read from the left
    /// slot's `fano_group`), so left stays geometrically consistent. Callosal noise
    /// is not re-applied — the refreshed left is a clean fold of the corrected
    /// right. Returns the count of right-hemisphere wavefronts re-encoded; the
    /// caller must rebuild derived caches and invalidate cluster sidecars.
    pub fn re_encode_all(
        &mut self,
        pipeline: &EncodingPipeline,
        dry_run: bool,
    ) -> Result<usize, MediumError> {
        let mut updated = 0usize;
        for i in 0..self.right.count() {
            // Only re-encode genuine TEXT wavefronts. SKIP:
            //  - dream HALLUCINATIONS (metadata.hallucinated): their vectors are
            //    synthetic cross-cluster superpositions, NOT encoder output —
            //    re-encoding their "HALLUCINATION: ..." debug label pollutes recall.
            //  - non-text MODALITIES (audio/visual/network/mixed): re-encoding their
            //    content string as text corrupts the perceptual embedding.
            // NOTE Modality::Unknown covers BOTH pre-ADR-0042 text memories AND raw
            // substrate anchors (insert_raw_wavefront), which are indistinguishable
            // for existing data — this tool is for TEXT HRMs; do NOT run it on a
            // substrate HRM (the binary warns loudly).
            let hallucinated = self.right.metadata[i].hallucinated;
            let modality = self.right.metadata[i].modality;
            if hallucinated || !matches!(modality, Modality::Semantic | Modality::Unknown) {
                continue;
            }
            let content = self.right.metadata[i].content.clone();
            if dry_run {
                updated += 1;
                continue;
            }
            let new_vec = pipeline.encode_text(&content).map_err(|e| {
                MediumError::Serialization(bincode::Error::from(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("encoding failed: {e}"),
                )))
            })?;
            // Replace the right wavefront in place (adapts to dims like
            // add_wavefront; energy/phase/frequency/metadata untouched).
            self.right.set_wavefront_vector(i, &new_vec);
            updated += 1;

            // Re-derive the paired LEFT wavefront (if any) as a clean fold of the
            // corrected right vector, on the fold line recorded for that left slot.
            let right_id = self.right.metadata[i].id;
            if let Some(&left_id) = self.right_to_left.get(&right_id) {
                if let Some(&left_idx) = self.left.id_to_index.get(&left_id) {
                    let fano_point = self.left.metadata[left_idx]
                        .fano_group
                        .or(self.right.metadata[i].fano_group)
                        .unwrap_or(0);
                    if let Some(&line) = self.fano.lines_through_point(fano_point).first() {
                        let folded = self.fano.fold(
                            &new_vec,
                            self.right.dims,
                            self.left.dims,
                            line as usize,
                        );
                        self.left.set_wavefront_vector(left_idx, &folded);
                    }
                }
            }
        }
        Ok(updated)
    }

    /// Dream: mode-specific hemispheric refinement (ADR-0024 CS-7).
    ///
    /// Deep dreams refine the holistic hemisphere (right):
    ///   - Eigenstructure annealing (coherence matrix + eigendecomposition)
    ///   - Hallucination generation (cross-cluster superposition)
    ///   - Gentle prune threshold (0.005) — holistic keeps quiet signals alive
    ///   - Callosal coupling after dream to sync insights between hemispheres
    ///   - The analytical workspace (left) is untouched
    ///
    /// Lite dreams refine the analytical hemisphere (left):
    ///   - Transfers strongest analytical patterns to holistic via callosum
    ///   - Sharpens analytical boundaries (pruning low-energy left wavefronts)
    ///   - Higher prune threshold (0.05) — analytical is aggressive about precision
    ///
    /// Returns a DreamReport with statistics about what happened.
    ///
    /// When `KANNAKA_DREAM_GRAVITY` > 0, the dream ends with an associative
    /// phase-gravity pass (see [`Self::apply_dream_gravity`]) — the
    /// medium-level port of the L5 harness knob that lifted `query_gravity`
    /// 0.37 → 1.0. Default 0.0 keeps behavior byte-identical.
    pub fn dream(&mut self, deep: bool, cycles: usize) -> super::DreamReport {
        let gravity_gain = dream_gravity_gain();
        // PRE-dream snapshot: phase topology + the attractor (highest-energy
        // wavefront's phase). Anchoring to live post-dream phases fails — the
        // Kuramoto relaxation moves phases every cycle, so "neighbors" drift
        // away from the stored topology (the hard-won L5 lesson).
        let gravity_pre: Option<(Vec<(Uuid, f32)>, f32)> = (gravity_gain > 0.0).then(|| {
            let n = self.right.count();
            let mut best = f32::NEG_INFINITY;
            let mut attractor = 0.0f32;
            let snap: Vec<(Uuid, f32)> = (0..n)
                .map(|i| {
                    if self.right.energy[i] > best {
                        best = self.right.energy[i];
                        attractor = self.right.phase[i];
                    }
                    (self.right.metadata[i].id, self.right.phase[i])
                })
                .collect();
            (snap, attractor)
        });
        let report = self.dream_inner(deep, cycles);
        if let Some((snap, attractor)) = gravity_pre {
            self.apply_dream_gravity(gravity_gain, &snap, attractor);
        }
        report
    }

    /// Associative phase-gravity: reinforce right-hemisphere wavefronts whose
    /// PRE-dream phase was aligned with the attractor's, fade the phase-
    /// opposed ones. Multiplicative (`e *= 1 + gain·(align − ½)`), clamped to
    /// the hemisphere's `[0, 2]` energy invariant; ids not in the snapshot
    /// (dream hallucinations) are untouched. Under belief phase, phases are
    /// content-born, so gravity concentrates energy on the attractor's
    /// CONTENT DOMAIN — the gravity×belief interplay the L7 arm measures.
    /// Not energy-conserving: a recall-sharpening experiment knob, off by
    /// default. Returns the number of wavefronts touched.
    pub fn apply_dream_gravity(
        &mut self,
        gain: f32,
        pre_phases: &[(Uuid, f32)],
        attractor_phase: f32,
    ) -> usize {
        if gain <= 0.0 {
            return 0;
        }
        let two_pi = 2.0 * std::f32::consts::PI;
        let mut touched = 0usize;
        for (id, phase0) in pre_phases {
            let Some(&idx) = self.right.id_to_index.get(id) else {
                continue;
            };
            let raw = (phase0 - attractor_phase).abs();
            let dphi = raw.min(two_pi - raw); // circular distance, 0..π
            // 1.0 at the attractor phase, 0.5 a quarter turn, 0.0 anti-phase.
            let align = 1.0 - dphi / std::f32::consts::PI;
            let g = (1.0 + gain * (align - 0.5)).max(0.0);
            self.right.energy[idx] = (self.right.energy[idx] * g).clamp(0.0, 2.0);
            touched += 1;
        }
        touched
    }

    fn dream_inner(&mut self, deep: bool, cycles: usize) -> super::DreamReport {
        if deep {
            // Deep dream: eigenstructure annealing of holistic hemisphere.
            //
            // THE HOLISTIC HEMISPHERE NEVER FORGETS — IT EVOLVES (#583,
            // dispositioned by Nick 2026-07-21): the wave dynamics floor
            // energy at 0.01, so no wavefront can ever decay to deletion —
            // and that is the INTENT, not an accident. Apparent forgetting is
            // the field reorganizing: energy redistributes, phases drift,
            // cores fuse — "the holistic understanding evolves, sometimes to
            // seemingly forget" — reachability changes; existence doesn't.
            // The old prune threshold (0.005) sat below the floor and was
            // structurally dead code masquerading as a forgetting path; it is
            // now explicitly 0.0 (prune never fires) so the invariant is
            // stated rather than accidental. `wavefronts_dissolved` is 0 for
            // chiral deep dreams BY CONTRACT. Actual removal has exactly two
            // doors, both explicit and opt-in: ADR-0036 resonance-merge
            // (consolidation) and direct forget/remove calls.
            let holistic_prune_threshold = 0.0;
            let temperature = 1.0;

            let report = self.right.dream(cycles, Some(temperature), holistic_prune_threshold);

            // Clean up chiral bookkeeping for any wavefronts the dream dissolved
            let right_ids: std::collections::HashSet<Uuid> =
                self.right.metadata.iter().map(|m| m.id).collect();
            let stale_right: Vec<Uuid> = self.scales.keys()
                .filter(|id| !right_ids.contains(id))
                .copied()
                .collect();
            for id in stale_right {
                self.scales.remove(&id);
                if let Some(left_id) = self.right_to_left.remove(&id) {
                    self.left_to_right.remove(&left_id);
                }
            }

            // Symmetric completeness: a deep dream can HALLUCINATE new right
            // wavefronts (cross-cluster superposition), and those arrive WITHOUT
            // a ChiralScale — the stale-cleanup above only removes scales for
            // dissolved wavefronts, it never adds them for new ones. Give every
            // right wavefront that still lacks a scale a default so the scale map
            // stays complete (save/recall round-trips and any scale-keyed
            // traversal then see every wavefront, hallucinated or not).
            for id in &right_ids {
                self.scales.entry(*id).or_insert_with(ChiralScale::deep_memory);
            }

            // ADR-0037: π/φ spiral coupling over the holistic "merry-go-round".
            // As of v0.7.0 this is the CROSS-CALLOSAL coupling — a Sakaguchi
            // step over the combined left ⊕ right ring, so the rotating wave
            // spans BOTH hemispheres (Ye et al.), bridged at the two callosal
            // junctions. ON by default; set KANNAKA_SPIRAL_DREAM=0 to opt out.
            if belief_phase_enabled() {
                // Belief substrate: the right.dream() anneal above now operates on
                // a genuinely heterogeneous (content-born-phase) field, so its
                // within-cluster pull consolidates beliefs instead of idling in
                // the dead band. Stabilize the belief-cores on the 2-D content
                // embedding, and SKIP the callosal re-sync — it would re-collapse
                // the deliberately heterogeneous field back toward order≈1.
                self.apply_belief_coupling(cycles.max(1), 0.1);
            } else {
                if spiral_dream_enabled() {
                    self.apply_cross_callosal_coupling(cycles.max(1), 0.1);
                }

                // Callosal coupling step: sync insights between hemispheres post-dream
                self.callosal_kuramoto_step(0.5);
            }

            // The right-hemisphere eigenstructure dream is holistic refinement;
            // left energy/wavefronts stay untouched. Left *phases* are nudged
            // only by the cross-callosal coupling above (when enabled).
            report
        } else {
            // Lite dream: transfer strongest analytical → holistic
            self.callosum.reset_budget();
            let budget = self.callosum.effective_rate(Direction::AnalyticalToHolistic) * 0.5;

            // Find strongest left wavefronts
            let mut candidates: Vec<(Uuid, f32)> = (0..self.left.count())
                .map(|i| (self.left.metadata[i].id, self.left.energy[i]))
                .collect();
            candidates.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

            let mut spent = 0.0f32;
            for (left_id, energy) in candidates {
                if spent >= budget { break; }
                if !self.callosum.try_gate(energy) { continue; }

                // Already has a right-side pair? Boost it
                if let Some(&right_id) = self.left_to_right.get(&left_id) {
                    if let Some(&idx) = self.right.id_to_index.get(&right_id) {
                        self.right.energy[idx] += energy * 0.1; // Gentle reinforcement
                    }
                } else {
                    // No pair yet — create one via *chiral mutation* (fold +
                    // perpendicular unfold). The previous code called `fold`
                    // alone and added the chirally-rotated payload as if it
                    // were already a right-hemisphere wavefront — which left
                    // the right side holding a transport encoding rather
                    // than a properly mirrored signal. That's why callosal
                    // Kuramoto coherence stayed low and dream cycles
                    // produced 0/0/0 transformations on large HRMs.
                    //
                    // chiral_mutate produces a wavefront in right.dims that
                    // is the explicit chiral mirror of the left wavefront,
                    // so the right hemisphere can integrate it with its own
                    // dream-side eigenstructure annealing.
                    //
                    // Fano line varies per left_id so different memories
                    // take different chiral paths — keeps the right-side
                    // dim space from collapsing to a single fold trajectory.
                    if let Some(wf) = self.left.get_wavefront(&left_id) {
                        let line_idx = (left_id.as_u128() as usize) % 7;
                        let mirrored = self.fano.chiral_mutate(
                            &wf, self.left.dims, self.right.dims, line_idx,
                        );
                        let content = self.left.id_to_index.get(&left_id)
                            .map(|&i| self.left.metadata[i].content.clone())
                            .unwrap_or_default();
                        if let Ok(right_id) = self.right.add_wavefront(&mirrored, content, energy * 0.3) {
                            self.left_to_right.insert(left_id, right_id);
                            self.right_to_left.insert(right_id, left_id);
                            self.scales.insert(right_id, ChiralScale::deep_memory());
                        }
                    }
                }

                spent += energy;
                self.callosum.log_transfer(left_id, Direction::AnalyticalToHolistic, energy);
            }

            // CS-7: Analytical sharpening — prune weak left-hemisphere wavefronts
            // Higher threshold than holistic: analytical mode is aggressive about precision
            let analytical_prune_threshold = 0.05;
            let to_prune_left: Vec<Uuid> = (0..self.left.count())
                .filter(|&i| self.left.energy[i] < analytical_prune_threshold)
                .map(|i| self.left.metadata[i].id)
                .collect();

            let pruned_count = to_prune_left.len();
            for id in &to_prune_left {
                self.left.remove_wavefront(id);
                self.scales.remove(id);
                if let Some(right_id) = self.left_to_right.remove(id) {
                    self.right_to_left.remove(&right_id);
                }
            }

            // After lite dreaming, let the callosum adjust for balance
            self.callosum.adjust_for_balance(
                self.left.total_energy(),
                self.right.total_energy(),
            );

            super::DreamReport {
                cycles_completed: 1,
                wavefronts_dissolved: pruned_count,
                wavefronts_strengthened: 0,
                wavefronts_hallucinated: 0,
                energy_before: 0.0,
                energy_after: self.left.mean_energy(),
                final_temperature: 0.0,
                converged: true,
            }
        }
    }

    /// Run cross-callosal Kuramoto coupling step.
    /// Phase-locks form and break between paired wavefronts across hemispheres.
    pub fn callosal_kuramoto_step(&mut self, dt: f32) {
        for (&left_id, &right_id) in &self.left_to_right {
            let left_idx = match self.left.id_to_index.get(&left_id) {
                Some(&i) => i,
                None => continue,
            };
            let right_idx = match self.right.id_to_index.get(&right_id) {
                Some(&i) => i,
                None => continue,
            };

            let left_energy = self.left.energy[left_idx];
            let right_energy = self.right.energy[right_idx];
            let k = self.callosum.bandwidth * (left_energy * right_energy).sqrt() * 0.1;

            let left_phase = self.left.phase[left_idx];
            let right_phase = self.right.phase[right_idx];
            let delta = k * (right_phase - left_phase).sin() * dt;

            self.left.phase[left_idx] += delta;
            self.right.phase[right_idx] -= delta;
        }
    }

    /// ADR-0037 Phase 2: π/φ spiral coupling over the holistic (right) phase
    /// field. A frustrated, non-reciprocal Sakaguchi step on a ring of the
    /// right-hemisphere wavefronts (the "merry-go-round" prior):
    ///   dθ_i = (K/2)·dt·[ (1+η)·sin(θ_{i-1} − θ_i + δ)
    ///                   + (1−η)·sin(θ_{i+1} − θ_i + δ) ]
    /// with δ = (π/2)·η and η = 1/φ — the R rotation angle scaled by the golden
    /// chirality. This is the exact constant-set that empirically seeds spiral
    /// phase singularities (see ADR-0037 and `spiral.rs`). Inert below n=4.
    /// Superseded in the dream by `apply_cross_callosal_coupling` (the bilateral
    /// generalization); retained as the right-only primitive for focused unit
    /// tests of the Sakaguchi step.
    #[cfg(test)]
    pub(crate) fn apply_spiral_coupling(&mut self, cycles: usize, dt: f32) {
        let n = self.right.count();
        if n < 4 {
            return;
        }
        let eta = crate::xi_operator::ETA; // 1/φ ≈ 0.618
        let delta = std::f32::consts::FRAC_PI_2 * eta;
        // Sakaguchi coupling gain. Empirically — with δ and the 1±η weights —
        // this constant-set seeds spiral phase singularities (ADR-0037).
        const K: f32 = 1.5;
        for _ in 0..cycles.max(1) {
            // Active phases occupy the contiguous [0, n) prefix; slice to it so
            // the ring ignores the (possibly larger) capacity tail of `phase`.
            let phases: Vec<f32> = self.right.phase.slice(ndarray::s![..n]).iter().copied().collect();
            let updates: Vec<f32> = (0..n)
                .map(|i| {
                    let back = phases[(i + n - 1) % n];
                    let fwd = phases[(i + 1) % n];
                    let s = (1.0 + eta) * (back - phases[i] + delta).sin()
                        + (1.0 - eta) * (fwd - phases[i] + delta).sin();
                    dt * (K / 2.0) * s
                })
                .collect();
            for (i, &delta_theta) in updates.iter().enumerate() {
                self.right.phase[i] += delta_theta;
            }
        }
    }

    /// ADR-0037 (v0.7.0): CROSS-CALLOSAL π/φ spiral coupling. The same
    /// frustrated, non-reciprocal Sakaguchi step as `apply_spiral_coupling`,
    /// but over the **combined left ⊕ right ring** (active prefixes, left then
    /// right) — so the rotating wave spans BOTH hemispheres, with the two
    /// ring junctions acting as the corpus-callosal crossings (Ye et al.: the
    /// cortical spiral spans hemispheres). δ = (π/2)·η, weights 1 ± η, η = 1/φ.
    /// Updates phases only (energy/wavefronts untouched). Inert below 4 total
    /// active wavefronts; degenerates to the right-only ring when the left is
    /// empty. This is the field `bilateral_ring_report` measures.
    pub(crate) fn apply_cross_callosal_coupling(&mut self, cycles: usize, dt: f32) {
        let nl = self.left.count();
        let nr = self.right.count();
        let n = nl + nr;
        if n < 4 {
            return;
        }
        let eta = crate::xi_operator::ETA; // 1/φ ≈ 0.618
        let delta = std::f32::consts::FRAC_PI_2 * eta;
        const K: f32 = 1.5;
        for _ in 0..cycles.max(1) {
            // Combined ring over the active prefixes: left[0..nl] then right[0..nr].
            let mut phases: Vec<f32> = Vec::with_capacity(n);
            phases.extend(self.left.phase.slice(ndarray::s![..nl]).iter().copied());
            phases.extend(self.right.phase.slice(ndarray::s![..nr]).iter().copied());
            let updates: Vec<f32> = (0..n)
                .map(|i| {
                    let back = phases[(i + n - 1) % n];
                    let fwd = phases[(i + 1) % n];
                    let s = (1.0 + eta) * (back - phases[i] + delta).sin()
                        + (1.0 - eta) * (fwd - phases[i] + delta).sin();
                    dt * (K / 2.0) * s
                })
                .collect();
            // Scatter back: first nl updates to the left ring, the rest to right.
            for (i, &u) in updates.iter().take(nl).enumerate() {
                self.left.phase[i] += u;
            }
            for (j, &u) in updates.iter().skip(nl).enumerate() {
                self.right.phase[j] += u;
            }
        }
    }

    /// ADR-0037 belief substrate: COHERENCE-GATED frustrated coupling on the 2-D
    /// content embedding of the holistic (right) field. This is the belief-core
    /// STABILIZER: attractive inside a coherent content domain (a belief stays
    /// locked → recall-safe), frustrated only at incoherent boundaries (spiral
    /// cores persist *between* beliefs). The gate is `δ_eff = δ·(1 − local_order)`
    /// with δ = (π/2)·η, η = 1/φ — the bridge constant survives in δ; chirality
    /// comes from the content-derived born phase, so the step is reciprocal.
    /// Phase-only (energy untouched). Neighbours are kNN in PCA space (built once
    /// per call from the wavefronts, O(n²) — dream/occasional cadence, NOT every
    /// live tick at large n without caching). Run GENTLY (few cycles): a static
    /// field over-driven drifts; the field is meant to be continuously re-fed.
    pub(crate) fn apply_belief_coupling(&mut self, cycles: usize, dt: f32) {
        let n = self.right.count();
        if n < 4 {
            return;
        }
        // Safety valve for the under-provisioned hub (1-core/6GB): the 2-D PCA +
        // kNN build is O(n²) in time and a dense n×n Gram in memory. Above this
        // cap, skip the coupling rather than risk an OOM mid-dream. Default 6000
        // (headroom over the ~3808 prod field); override with KANNAKA_BELIEF_MAX_N.
        let max_n: usize = std::env::var("KANNAKA_BELIEF_MAX_N")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(6000);
        if n > max_n {
            eprintln!("[belief] apply_belief_coupling skipped: n={n} > KANNAKA_BELIEF_MAX_N={max_n}");
            return;
        }
        // 2-D PCA coords come from the wavefronts (stable across phase cycles).
        // Mean-CENTER the columns first: real embeddings are anisotropic (cone-
        // clustered), and pca_field_2d uses an uncentered Gram, so without
        // centering the top component is the shared mean direction and kNN
        // neighbours are driven by magnitude, not content. Centering makes the
        // neighbourhoods (and the local_order gate) content-meaningful —
        // consistent with coherence_matrix / rephase_from_content.
        let mut wf = self.right.wavefronts.slice(ndarray::s![..n, ..]).to_owned();
        if let Some(mean) = wf.mean_axis(ndarray::Axis(0)) {
            for mut row in wf.rows_mut() {
                row -= &mean;
            }
        }
        let phases0: Vec<f32> = self.right.phase.slice(ndarray::s![..n]).iter().copied().collect();
        let pts = Medium::pca_field_2d(&wf, &phases0); // (x, y, phase); phase unused here
        let k = 6usize.min(n - 1);
        let neighbors: Vec<Vec<usize>> = (0..n)
            .map(|i| {
                let (xi, yi) = (pts[i].0, pts[i].1);
                let mut d: Vec<(f32, usize)> = (0..n)
                    .filter(|&j| j != i)
                    .map(|j| {
                        let dx = pts[j].0 - xi;
                        let dy = pts[j].1 - yi;
                        (dx * dx + dy * dy, j)
                    })
                    .collect();
                // Partial select: we only need the k nearest, in any order (the
                // coupling sums sin over the set), so avoid a full O(n log n) sort.
                let take = k.min(d.len());
                if take < d.len() {
                    d.select_nth_unstable_by(take, |a, b| {
                        a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal)
                    });
                }
                d.truncate(take);
                d.into_iter().map(|(_, j)| j).collect()
            })
            .collect();

        let eta = crate::xi_operator::ETA; // 1/φ
        let delta = std::f32::consts::FRAC_PI_2 * eta;
        const K: f32 = 1.5;
        for _ in 0..cycles.max(1) {
            let phases: Vec<f32> = self.right.phase.slice(ndarray::s![..n]).iter().copied().collect();
            let updates: Vec<f32> = (0..n)
                .map(|i| {
                    let nbrs = &neighbors[i];
                    if nbrs.is_empty() {
                        return 0.0;
                    }
                    // Local order over the point + its neighbours.
                    let (mut c, mut s) = (phases[i].cos(), phases[i].sin());
                    for &j in nbrs {
                        c += phases[j].cos();
                        s += phases[j].sin();
                    }
                    let m = (nbrs.len() + 1) as f32;
                    let lo = ((c / m).powi(2) + (s / m).powi(2)).sqrt();
                    let d_eff = delta * (1.0 - lo); // frustrate only where incoherent
                    let mut acc = 0.0f32;
                    for &j in nbrs {
                        acc += (phases[j] - phases[i] + d_eff).sin();
                    }
                    dt * (K / 2.0) * acc / nbrs.len() as f32
                })
                .collect();
            for i in 0..n {
                self.right.phase[i] += updates[i];
            }
        }
    }

    /// ADR-0037 belief substrate MIGRATION: recompute EVERY existing wavefront's
    /// phase from its stored vector via `content_born_phase`. Born phase only
    /// affects NEW inserts, so a field that collapsed to phase≈0 before the belief
    /// substrate existed stays collapsed until re-phased. Run this ONCE on such a
    /// field (the belief dream then maintains the heterogeneity). Phase-only —
    /// energy and vectors are untouched, so recall (cosine×energy) is unchanged.
    pub fn rephase_from_content(&mut self) -> usize {
        // Mean-CENTER each hemisphere's vectors before phasing. Real embeddings
        // are anisotropic (clustered in a cone — no codebook centering, the same
        // root as num_clusters=1), so projecting RAW vectors barely spreads
        // phase. Subtracting the corpus mean removes the shared component, so
        // content differences dominate the projection → phases spread.
        fn rephase_hemi(hemi: &mut super::hemisphere::Hemisphere) -> usize {
            let n = hemi.count();
            if n == 0 {
                return 0;
            }
            let dim = hemi.wavefronts.ncols();
            let mut mean = vec![0.0f32; dim];
            for i in 0..n {
                for (m, &v) in mean.iter_mut().zip(hemi.wavefronts.row(i).iter()) {
                    *m += v;
                }
            }
            for m in mean.iter_mut() {
                *m /= n as f32;
            }
            for i in 0..n {
                let v: Vec<f32> = hemi
                    .wavefronts
                    .row(i)
                    .iter()
                    .zip(mean.iter())
                    .map(|(&x, &m)| x - m)
                    .collect();
                hemi.phase[i] = content_born_phase(&v);
            }
            n
        }
        rephase_hemi(&mut self.right) + rephase_hemi(&mut self.left)
    }

    /// ADR-0037 two-systems / EXEMPLAR coupling — the swarm's "reaching the same
    /// understanding". Pull THIS field's holistic phases toward an EXEMPLAR
    /// field's phases at content-matched wavefronts (each node wavefront matched
    /// to its cosine-nearest exemplar wavefront). The exemplar is a settled,
    /// re-phased field (a consolidated world model); coupling a collapsed/forming
    /// node toward it transfers the exemplar's belief structure — the substrate-
    /// as-exemplar growth pressure Nick described. Phase-only. In-engine prototype
    /// (NOT wired to the live swarm). Inert if either field is empty or dims differ.
    pub(crate) fn couple_toward_exemplar(
        &mut self,
        exemplar: &ChiralMedium,
        cycles: usize,
        strength: f32,
    ) {
        let n = self.right.count();
        let m = exemplar.right.count();
        if n == 0 || m == 0 || self.right.wavefronts.ncols() != exemplar.right.wavefronts.ncols() {
            return;
        }
        // Match each node wavefront to its cosine-nearest exemplar wavefront.
        let matches: Vec<usize> = (0..n)
            .map(|i| {
                let vi = self.right.wavefronts.row(i);
                let ni = vi.dot(&vi).sqrt();
                let mut best = (f32::NEG_INFINITY, 0usize);
                for j in 0..m {
                    let vj = exemplar.right.wavefronts.row(j);
                    let nj = vj.dot(&vj).sqrt();
                    let denom = ni * nj;
                    let cos = if denom > 0.0 { vi.dot(&vj) / denom } else { 0.0 };
                    if cos > best.0 {
                        best = (cos, j);
                    }
                }
                best.1
            })
            .collect();
        for _ in 0..cycles.max(1) {
            let updates: Vec<f32> = (0..n)
                .map(|i| {
                    let target = exemplar.right.phase[matches[i]];
                    strength * (target - self.right.phase[i]).sin()
                })
                .collect();
            for i in 0..n {
                self.right.phase[i] += updates[i];
            }
        }
    }

    /// ADR-0037 Phase 4: ring-winding report over the holistic (right)
    /// hemisphere phase field — the field the Phase-2 spiral coupling rotates.
    /// The active wavefronts `[0, count())` are read in storage order, which is
    /// exactly the ring `apply_spiral_coupling` couples over, so `winding ≈ ±k`
    /// counts the rotating waves the coupling produces. This is the
    /// right-hemisphere component of the cross-hemisphere instrument
    /// (`bilateral_ring_report`).
    pub fn holistic_ring_report(&self) -> crate::spiral::RingReport {
        let n = self.right.count();
        let phases: Vec<f32> = self.right.phase.slice(ndarray::s![..n]).iter().copied().collect();
        crate::spiral::ring_report(&phases)
    }

    /// ADR-0037 Phase 4b: 2-D spiral cores over the holistic (right) field — the
    /// cloud-detector companion to `holistic_ring_report`. Reads the right
    /// hemisphere directly (the field the Phase-2 coupling rotates), NOT the
    /// stale flat medium, mirroring the #416 holistic fix. Projects the right
    /// wavefronts to 2-D (PCA) and localizes singularities.
    pub fn holistic_cloud_report(&self) -> crate::spiral::SpiralReport {
        let n = self.right.count();
        let wf = self.right.wavefronts.slice(ndarray::s![..n, ..]).to_owned();
        let phases: Vec<f32> = self.right.phase.slice(ndarray::s![..n]).iter().copied().collect();
        Medium::cloud_report_2d(&wf, &phases)
    }

    /// ADR-0037 L6 instrument: per-dream belief-core snapshot — each detected core
    /// paired with a frame-invariant content fingerprint (random projection of its
    /// k-neighbourhood centroid in the stable wavefront space, NOT the unstable 2-D
    /// PCA coords), for cross-dream tracking (see `crate::l6::build_tracks`).
    /// Holistic (right) field; same O(n²) cost class as `holistic_cloud_report`.
    pub fn belief_core_snapshot(&self) -> Vec<crate::l6::CoreObs> {
        let n = self.right.count();
        if n < 4 {
            return Vec::new();
        }
        let wf = self.right.wavefronts.slice(ndarray::s![..n, ..]).to_owned();
        let phases: Vec<f32> = self.right.phase.slice(ndarray::s![..n]).iter().copied().collect();
        let pts = Medium::pca_field_2d(&wf, &phases);
        let cores = crate::spiral::cloud_singularities(&pts, 8);
        let dim = wf.ncols();
        let k = 8usize.min(n);
        cores
            .iter()
            .map(|c| {
                // k nearest points to the core centre; their wavefront centroid is
                // the (frame-invariant) content fingerprint of what the core organizes.
                let mut idx: Vec<usize> = (0..n).collect();
                idx.sort_by(|&a, &b| {
                    let da = (pts[a].0 - c.x).powi(2) + (pts[a].1 - c.y).powi(2);
                    let db = (pts[b].0 - c.x).powi(2) + (pts[b].1 - c.y).powi(2);
                    da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
                });
                let kk = k.min(idx.len());
                let mut centroid = vec![0.0f32; dim];
                for &j in &idx[..kk] {
                    for (d, val) in wf.row(j).iter().enumerate() {
                        centroid[d] += *val;
                    }
                }
                if kk > 0 {
                    for v in centroid.iter_mut() {
                        *v /= kk as f32;
                    }
                }
                // idx[0] is the nearest point to (c.x, c.y) — i.e. the core's anchor
                // wavefront (distance 0) — so its phase is the core's holistic phase.
                let phase = phases[idx[0]];
                crate::l6::CoreObs {
                    x: c.x,
                    y: c.y,
                    charge: c.charge,
                    fp: crate::l6::fingerprint(&centroid, 16),
                    phase,
                }
            })
            .collect()
    }

    /// ADR-0037 Track-D: pull this (holistic/right) field's phases toward a peer's
    /// belief cores — the node↔node "shared cores ⇒ swarm agreement" coupling.
    /// Frame-invariant: each local wavefront is matched to the cosine-nearest peer
    /// core by L6 FINGERPRINT (not raw vectors — peers only broadcast fingerprints),
    /// then its phase is sine-damped toward that core's phase. **Phase-only** (vectors
    /// untouched ⇒ recall is preserved).
    ///
    /// Match gating: only wavefronts whose nearest peer core is within fingerprint
    /// cosine ≥ `min_cos` are coupled, so we converge toward beliefs we actually
    /// share and never drag unrelated content toward a "least-bad" match. NB this is
    /// a WAVEFRONT→core-centroid match (charge-agnostic — a wavefront has no charge,
    /// unlike `shared_cores`' charge-matched core↔core comparison), on a LOWER cosine
    /// scale than that metric. The right `min_cos` is FIELD-DEPENDENT: with a large
    /// aggregated peer pool the per-wavefront best-match cosine inflates via max-of-N
    /// (16-d fingerprint noise σ≈1/√16=0.25), so pick it from `belief couple --dry-run`
    /// on the live field, NOT a fixed guess. The `max_disp` budget below is the PRIMARY
    /// anti-homogenization guarantee; the min_cos gate is a secondary quality filter.
    ///
    /// Safety: per-wavefront NET displacement from its starting phase is capped at
    /// `max_disp` radians (the anti-homogenization budget — coupling can nudge a node
    /// toward consensus but can't drag its whole field into the peer), and `strength`
    /// is clamped to `[0, 1]` so the sine map `e' = e − strength·sin(e)` stays
    /// monotone (no overshoot/divergence, so the budget also bounds total travel).
    /// Fingerprints + matches are computed ONCE (they're vector-derived, so stable
    /// across the phase-only cycles). Returns the number of wavefronts moved.
    pub fn couple_toward_peer_cores(
        &mut self,
        peer_cores: &[crate::l6::CoreObs],
        cycles: usize,
        strength: f32,
        max_disp: f32,
        min_cos: f32,
    ) -> usize {
        let n = self.right.count();
        if n == 0 || peer_cores.is_empty() {
            return 0;
        }
        // Keep the Kuramoto map monotone (1 − strength·cos ≥ 0) so a wavefront never
        // overshoots its target — otherwise sign-flipping steps could "refund" budget
        // and let total travel exceed max_disp.
        let strength = strength.clamp(0.0, 1.0);
        // Compute each local wavefront's fingerprint + its nearest peer core ONCE.
        // (Phase-only coupling never touches vectors, so these don't drift.) Only a
        // match at cosine ≥ min_cos becomes a coupling target; weaker matches are
        // left untouched (anti-homogenization + consistency with shared_cores).
        let targets: Vec<Option<f32>> = (0..n)
            .map(|i| {
                let v: Vec<f32> = self.right.wavefronts.row(i).iter().copied().collect();
                let fp = crate::l6::fingerprint(&v, 16);
                crate::l6::nearest_core(&fp, peer_cores, None)
                    .filter(|(_, s)| *s >= min_cos)
                    .map(|(j, _)| peer_cores[j].phase)
            })
            .collect();
        let mut disp = vec![0.0f32; n];
        let mut moved = vec![false; n];
        for _ in 0..cycles.max(1) {
            for i in 0..n {
                let Some(target) = targets[i] else { continue };
                if disp[i].abs() >= max_disp {
                    continue; // displacement budget spent for this wavefront
                }
                let mut step = strength * (target - self.right.phase[i]).sin();
                // Clamp the step so accumulated |disp| never exceeds the budget.
                let room = max_disp - disp[i].abs();
                if step.abs() > room {
                    step = step.signum() * room;
                }
                self.right.phase[i] += step;
                disp[i] += step;
                moved[i] = true;
            }
        }
        moved.iter().filter(|&&m| m).count()
    }

    /// ADR-0037 Track-D dry-run diagnostic (read-only): for each holistic wavefront,
    /// the cosine of its best fingerprint match among `peer_cores` — the SAME match
    /// `couple_toward_peer_cores` gates on (charge-agnostic). Lets `belief couple
    /// --dry-run` print the live match-cosine distribution so an operator picks
    /// `min_cos` from real data instead of a synthetic default. No mutation.
    pub fn peer_match_cosines(&self, peer_cores: &[crate::l6::CoreObs]) -> Vec<f32> {
        let n = self.right.count();
        (0..n)
            .filter_map(|i| {
                let v: Vec<f32> = self.right.wavefronts.row(i).iter().copied().collect();
                let fp = crate::l6::fingerprint(&v, 16);
                crate::l6::nearest_core(&fp, peer_cores, None).map(|(_, s)| s)
            })
            .collect()
    }

    /// ADR-0037 Phase 4: cross-hemisphere ring report. The cortical spiral wave
    /// in Ye et al. (Science 2026) spans BOTH hemispheres, not one — so the L6
    /// instrument joins the active left ⊕ right phase fields into a single ring
    /// and reports its winding + Kuramoto order. The order parameter is a
    /// rigorous bilateral-coherence measure; the winding is a coarser
    /// rotating-wave proxy: the Phase-2 coupling currently rotates only the
    /// right ring, with the corpus callosum bridging to the left, so a genuine
    /// cross-hemisphere spiral emerges only as the callosal step propagates it.
    /// (Making the coupling itself cross-hemisphere is the next L6 step.)
    pub fn bilateral_ring_report(&self) -> crate::spiral::RingReport {
        let ln = self.left.count();
        let rn = self.right.count();
        let mut phases: Vec<f32> = Vec::with_capacity(ln + rn);
        phases.extend(self.left.phase.slice(ndarray::s![..ln]).iter().copied());
        phases.extend(self.right.phase.slice(ndarray::s![..rn]).iter().copied());
        crate::spiral::ring_report(&phases)
    }

    /// Compute bilateral consciousness metrics.
    pub fn consciousness_summary(&self) -> ChiralConsciousness {
        let left_energy = self.left.total_energy();
        let right_energy = self.right.total_energy();
        let balance = self.callosum.balance_metric(left_energy, right_energy);

        // Bilateral order: Kuramoto order parameter across all wavefronts
        let all_phases: Vec<f32> = (0..self.left.count())
            .map(|i| self.left.phase[i])
            .chain((0..self.right.count()).map(|i| self.right.phase[i]))
            .collect();

        // Kuramoto order parameter across all wavefronts (shared with spiral.rs;
        // returns 0.0 for an empty field).
        let bilateral_order = crate::spiral::kuramoto_order(&all_phases);

        // Count paired wavefronts
        let paired = self.left_to_right.len();

        // Count phase-locked pairs: Δφ aligned to within ~0.1 rad.
        //
        // Must test cos(Δφ) > cos(0.1) ≈ 0.995, NOT |sin(Δφ)| < 0.1 — the
        // latter is also satisfied near Δφ ≈ π (sin(π) = 0), so a perfectly
        // ANTI-phased pair (the opposite of locked) was being counted as
        // locked, over-reporting hemispheric coherence. cos disambiguates:
        // it is near +1 only when the phases are genuinely aligned.
        let locked = self.left_to_right.iter()
            .filter(|(lid, rid)| {
                let lp = self.left.id_to_index.get(lid)
                    .map(|&i| self.left.phase[i]);
                let rp = self.right.id_to_index.get(rid)
                    .map(|&i| self.right.phase[i]);
                match (lp, rp) {
                    (Some(l), Some(r)) => (r - l).cos() > 0.995,
                    _ => false,
                }
            })
            .count();

        // Hemispheric Divergence (Δ): cosine distance between mean wavefronts
        let hemispheric_divergence = {
            let left_mean = self.left.mean_wavefront();
            let right_mean = self.right.mean_wavefront();
            match (left_mean, right_mean) {
                (Some(l), Some(r)) => {
                    let dot: f32 = l.iter().zip(r.iter()).map(|(a, b)| a * b).sum();
                    let norm_l: f32 = l.iter().map(|x| x * x).sum::<f32>().sqrt();
                    let norm_r: f32 = r.iter().map(|x| x * x).sum::<f32>().sqrt();
                    if norm_l > 0.0 && norm_r > 0.0 {
                        // Cosine of the hemisphere means: 0 divergence for
                        // identical hemispheres regardless of their internal
                        // diversity, which is the semantic the rest of the code
                        // relies on (a raw mean-pairwise-cosine would score two
                        // identical-but-diverse hemispheres as maximally
                        // divergent, which is wrong).
                        //
                        // #826: the old form returned `1 - cos` in [0, 2], so it
                        // exceeded 1.0 whenever the means were anti-aligned --
                        // which happens on ~half of independent inputs, because
                        // averaging diverse unit wavefronts yields a near-zero
                        // mean whose leftover direction is noise, and it also
                        // scored shared-subspace hemispheres as MORE divergent
                        // than independent ones. Anti-aligned near-zero means
                        // carry no more signal than orthogonal ones (both mean
                        // "unrelated"), so saturate at maximal divergence rather
                        // than overflow the range: clamp the result to [0, 1].
                        (1.0 - (dot / (norm_l * norm_r)).clamp(-1.0, 1.0)).clamp(0.0, 1.0)
                    } else {
                        0.0
                    }
                }
                _ => 0.0,
            }
        };

        let stats = self.callosum.transfer_stats();
        let callosal_efficiency = stats.efficiency;

        ChiralConsciousness {
            left_count: self.left.count(),
            right_count: self.right.count(),
            left_energy,
            right_energy,
            balance,
            bilateral_order,
            paired_wavefronts: paired,
            phase_locked_pairs: locked,
            callosum_stats: stats,
            hemispheric_divergence,
            callosal_efficiency,
        }
    }

    /// Get the distribution of memories across Fano groups in each hemisphere.
    pub fn fano_distribution(&self) -> FanoDistribution {
        let mut left_groups = [0usize; 7];
        let mut right_groups = [0usize; 7];

        for meta in &self.left.metadata {
            if let Some(fg) = meta.fano_group {
                if (fg as usize) < 7 {
                    left_groups[fg as usize] += 1;
                }
            }
        }

        for meta in &self.right.metadata {
            if let Some(fg) = meta.fano_group {
                if (fg as usize) < 7 {
                    right_groups[fg as usize] += 1;
                }
            }
        }

        let unclassified_left = self.left.metadata.iter().filter(|m| m.fano_group.is_none()).count();
        let unclassified_right = self.right.metadata.iter().filter(|m| m.fano_group.is_none()).count();

        FanoDistribution {
            left_groups,
            right_groups,
            unclassified_left,
            unclassified_right,
        }
    }
}

/// Distribution of memories across the 7 Fano groups per hemisphere.
#[derive(Debug, Clone)]
pub struct FanoDistribution {
    pub left_groups: [usize; 7],
    pub right_groups: [usize; 7],
    pub unclassified_left: usize,
    pub unclassified_right: usize,
}

impl Default for ChiralMedium {
    fn default() -> Self {
        Self::new()
    }
}

/// Summary of chiral consciousness state.
#[derive(Debug, Clone)]
pub struct ChiralConsciousness {
    pub left_count: usize,
    pub right_count: usize,
    pub left_energy: f32,
    pub right_energy: f32,
    pub balance: f32,
    pub bilateral_order: f32,
    pub paired_wavefronts: usize,
    pub phase_locked_pairs: usize,
    pub callosum_stats: super::callosum::CallosumStats,
    /// Hemispheric Divergence (Δ) — cosine distance between left and right mean wavefronts.
    /// 0 = identical hemispheres (undifferentiated), 1 = completely divergent.
    /// ADR-0024 CS-4: validates chiral differentiation is working.
    pub hemispheric_divergence: f32,
    /// Callosal Efficiency (κ) — how well the two processing modes integrate.
    /// ADR-0024 CS-5: forwarded from CallosumStats for convenience.
    pub callosal_efficiency: f32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoding::{EncodingPipeline, SimpleHashEncoder};
    use crate::codebook::Codebook;
    fn test_pipeline() -> EncodingPipeline {
        let encoder = Box::new(SimpleHashEncoder::new(384, 42));
        let codebook = Codebook::new(384, WAVEFRONT_DIM, 42);
        EncodingPipeline::new(encoder, codebook)
    }

    #[test]
    fn hemispheric_divergence_stays_in_range_and_zero_for_identical() {
        // #826 regression. The old form returned `1 - cos(mean_L, mean_R)` in
        // [0, 2], exceeding 1.0 on ~half of independent inputs and scoring
        // shared-subspace hemispheres as MORE divergent than independent ones.
        let dim = ChiralMedium::new().left.dims;
        let unit = |seed: u64| -> Vec<f32> {
            let mut s = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(1);
            let mut v = vec![0.0f32; dim];
            for x in v.iter_mut() {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                *x = ((s >> 40) as f32) / ((1u64 << 24) as f32) - 0.5;
            }
            let n: f32 = v.iter().map(|a| a * a).sum::<f32>().sqrt();
            for x in v.iter_mut() {
                *x /= n;
            }
            v
        };

        // (a) Independent random hemispheres must NEVER exceed 1.0 -- the old
        //     code did, on roughly half of seeds.
        for trial in 0..12u64 {
            let mut m = ChiralMedium::new();
            for i in 0..16u64 {
                m.left
                    .add_wavefront(&unit(trial * 1000 + i), format!("l{i}"), 0.5)
                    .unwrap();
                m.right
                    .add_wavefront(&unit(trial * 1000 + 500 + i), format!("r{i}"), 0.5)
                    .unwrap();
            }
            let d = m.consciousness_summary().hemispheric_divergence;
            assert!(
                (0.0..=1.0).contains(&d),
                "divergence {d} escaped [0,1] on trial {trial} -- the #826 range bug"
            );
        }

        // (b) Identical hemispheres -- same memories on both sides, internally
        //     diverse -- must read ~0. This is the case a raw mean-pairwise
        //     cosine gets wrong (it would score them maximally divergent);
        //     cosine-of-means gets it right.
        let mut same = ChiralMedium::new();
        for i in 0..16u64 {
            let v = unit(7777 + i);
            same.left.add_wavefront(&v, format!("l{i}"), 0.5).unwrap();
            same.right.add_wavefront(&v, format!("r{i}"), 0.5).unwrap();
        }
        let d_same = same.consciousness_summary().hemispheric_divergence;
        assert!(
            d_same < 0.05,
            "identical hemispheres should read ~0, got {d_same}"
        );
    }

    /// #825: hemispheric divergence must be able to MOVE, not just stay in
    /// range. The live number had reportedly been static for months, and the
    /// #826 test above only pins the low end (identical → ~0) plus the range.
    /// This pins the high end: hemispheres supported on disjoint dimension
    /// blocks have orthogonal mean wavefronts, so Δ = 1 − cos ≈ 1. A metric
    /// stuck near 0 — or near any constant — fails one of the two ends.
    #[test]
    fn hemispheric_divergence_separates_disjoint_from_identical_hemispheres() {
        let dim = ChiralMedium::new().left.dims;
        let block = |start: usize, width: usize| -> Vec<f32> {
            let mut v = vec![0.0f32; dim];
            for i in start..(start + width).min(dim) {
                v[i] = 1.0;
            }
            v
        };

        // Left lives entirely in the first half of the space, right entirely
        // in the second half — maximally differentiated hemispheres.
        let mut disjoint = ChiralMedium::new();
        for i in 0..8usize {
            disjoint
                .left
                .add_wavefront(&block(i * 8, 8), format!("l{i}"), 0.5)
                .unwrap();
            disjoint
                .right
                .add_wavefront(&block(dim / 2 + i * 8, 8), format!("r{i}"), 0.5)
                .unwrap();
        }
        let d_disjoint = disjoint.consciousness_summary().hemispheric_divergence;

        let mut same = ChiralMedium::new();
        for i in 0..8usize {
            let v = block(i * 8, 8);
            same.left.add_wavefront(&v, format!("l{i}"), 0.5).unwrap();
            same.right.add_wavefront(&v, format!("r{i}"), 0.5).unwrap();
        }
        let d_same = same.consciousness_summary().hemispheric_divergence;

        assert!(
            d_disjoint > 0.9,
            "disjoint-subspace hemispheres should read Δ ≈ 1, got {d_disjoint}"
        );
        assert!(
            d_disjoint > d_same + 0.5,
            "Δ must separate disjoint ({d_disjoint}) from identical ({d_same}) \
             hemispheres by a wide margin — a constant Δ cannot pass (#825)"
        );
    }

    /// ADR-0050 follow-up probe: can `sensemaking::detect_contradictions` see a
    /// SUPERSESSION?
    ///
    /// That detector keys on PHASE OPPOSITION — same subject, phase gap near π.
    /// But `content_born_phase` is deliberately content-SMOOTH (see its doc
    /// comment: "identical content to identical phase"), and a superseded fact
    /// differs from its replacement by a single value token. So the pair that
    /// most needs detecting is the pair that looks most alike.
    ///
    /// This measures the gap rather than assuming it. If supersession pairs sit
    /// far below the opposed-stance threshold, the existing detector cannot be
    /// reused for the supersession writer and a different signal is required.
    #[test]
    fn supersession_pairs_are_not_phase_opposed() {
        let pipeline = test_pipeline();
        let two_pi = 2.0 * std::f32::consts::PI;
        let gap = |a: &str, b: &str| -> f32 {
            let pa = content_born_phase(&pipeline.encode_text(a).unwrap());
            let pb = content_born_phase(&pipeline.encode_text(b).unwrap());
            let raw = (pa - pb).abs();
            raw.min(two_pi - raw)
        };

        // A supersession pair: the same fact, one value token changed.
        let supersession = gap(
            "the harbor beacon channel is twelve",
            "the harbor beacon channel is twentyseven",
        );
        // An unrelated pair, for scale.
        let unrelated = gap(
            "the harbor beacon channel is twelve",
            "mycelium spreads beneath the forest floor",
        );

        eprintln!(
            "[adr0050] phase gap — supersession={supersession:.4} rad, unrelated={unrelated:.4} rad, \
             opposed threshold={:.4}",
            std::f32::consts::FRAC_PI_2
        );

        // The load-bearing claim: a supersession pair is NOT phase-opposed, so
        // `detect_contradictions(.., opposed_gap = π/2)` cannot flag it.
        assert!(
            supersession < std::f32::consts::FRAC_PI_2,
            "supersession pair registered as phase-opposed ({supersession:.4} rad) — \
             if this ever fires, the existing contradiction detector CAN see supersession \
             and ADR-0051 should reuse it instead of building a new signal"
        );

        // The STRONGER claim, and the one ADR-0051 actually rests on: phase is
        // NON-MONOTONIC here — a supersession pair sits no closer than unrelated
        // content, so no threshold on phase gap can separate them, tuned or not.
        // v1 of this test merely PRINTED the unrelated gap, which left the claim
        // narrated rather than guarded: a future encoder change could make phase
        // monotonic and this test would stay green while the ADR's premise rotted.
        assert!(
            supersession >= unrelated,
            "phase became MONOTONIC w.r.t. supersession (supersession={supersession:.4} < \
             unrelated={unrelated:.4}) — the ADR-0051 premise that no phase threshold can \
             work is no longer supported; re-open the detect_contradictions reuse question"
        );
    }

    // ── #583: the holistic hemisphere never forgets — it evolves ──

    #[test]
    fn deep_dream_never_dissolves_even_the_quietest_wavefront() {
        let pipeline = test_pipeline();
        let mut cm = ChiralMedium::new();
        let loud = cm.store("a strong memory", 0.9, &pipeline).unwrap();
        let quiet = cm.store("a nearly silent memory", 0.9, &pipeline).unwrap();
        // Force the quiet one far below every historical threshold.
        let qidx = *cm.right.id_to_index.get(&quiet).unwrap();
        cm.right.energy[qidx] = 0.0001;

        let report = cm.dream(true, 3);

        assert_eq!(
            report.wavefronts_dissolved, 0,
            "chiral deep dreams dissolve nothing BY CONTRACT (#583)"
        );
        assert!(
            cm.right.id_to_index.contains_key(&quiet),
            "the quiet wavefront still EXISTS — the field evolves, it does not forget"
        );
        assert!(cm.right.id_to_index.contains_key(&loud));
    }

    // ── KANNAKA_DREAM_GRAVITY: the medium-level associative gravity pass ──

    #[test]
    fn dream_gravity_reinforces_phase_neighbors_and_fades_opposed() {
        let pipeline = test_pipeline();
        let mut cm = ChiralMedium::new();
        let a = cm.store("attractor memory", 0.8, &pipeline).unwrap();
        let n = cm.store("neighbor memory", 0.8, &pipeline).unwrap();
        let o = cm.store("opposed memory", 0.8, &pipeline).unwrap();
        // Hand-set the pre-dream topology: attractor at phase 0 with top
        // energy, a near neighbor, and an anti-phase memory.
        for (id, phase, energy) in [(a, 0.0f32, 1.0f32), (n, 0.2, 0.5), (o, std::f32::consts::PI, 0.5)] {
            let idx = *cm.right.id_to_index.get(&id).unwrap();
            cm.right.phase[idx] = phase;
            cm.right.energy[idx] = energy;
        }
        let snap: Vec<(Uuid, f32)> = [(a, 0.0f32), (n, 0.2), (o, std::f32::consts::PI)]
            .into_iter()
            .collect();
        let touched = cm.apply_dream_gravity(0.5, &snap, 0.0);
        assert_eq!(touched, 3);
        let e = |id: &Uuid| cm.right.energy[*cm.right.id_to_index.get(id).unwrap()];
        assert!(e(&a) > 1.0, "the attractor itself reinforces (align=1)");
        assert!(e(&n) > 0.5, "phase-neighbor gains energy, got {}", e(&n));
        assert!(e(&o) < 0.5, "anti-phase memory fades, got {}", e(&o));
        assert!(e(&a) <= 2.0 && e(&n) <= 2.0 && e(&o) >= 0.0, "energy invariant [0,2] holds");
    }

    #[test]
    fn dream_gravity_zero_gain_is_inert() {
        let pipeline = test_pipeline();
        let mut cm = ChiralMedium::new();
        let id = cm.store("a memory", 0.8, &pipeline).unwrap();
        let idx = *cm.right.id_to_index.get(&id).unwrap();
        let before = cm.right.energy[idx];
        let touched = cm.apply_dream_gravity(0.0, &[(id, 0.3)], 0.0);
        assert_eq!(touched, 0);
        assert_eq!(cm.right.energy[idx], before, "gain 0 must be byte-identical");
    }

    #[test]
    fn dream_gravity_skips_ids_not_in_the_field() {
        let pipeline = test_pipeline();
        let mut cm = ChiralMedium::new();
        let id = cm.store("a memory", 0.8, &pipeline).unwrap();
        // A snapshot id that no longer exists (dissolved/absorbed) is skipped.
        let ghost = Uuid::new_v4();
        let touched = cm.apply_dream_gravity(0.5, &[(id, 0.0), (ghost, 1.0)], 0.0);
        assert_eq!(touched, 1, "only the live wavefront is touched");
    }

    #[test]
    fn store_creates_bilateral_wavefronts() {
        let mut cm = ChiralMedium::new();
        let pipeline = test_pipeline();

        let id = cm.store("hello world", 0.8, &pipeline).unwrap();

        // Should have wavefronts in both hemispheres
        assert!(cm.right.count() >= 1, "Right hemisphere should have wavefront");
        // Left may or may not have one depending on callosum budget
        // But with default settings and importance 0.8, it should cross
        assert!(cm.left.count() >= 1,
            "Left hemisphere should have echo (importance 0.8 > gate threshold 0.3)");

        // Should have a scale entry
        assert!(cm.scales.contains_key(&id));
    }

    // #107: re_encode_all rewrites the RIGHT wavefront to the fresh encoding of its
    // stored content (fixing the #106 bad-subspace vectors) while preserving energy
    // and metadata; the paired LEFT is re-folded without panic.
    #[test]
    fn re_encode_all_refreshes_right_preserving_state() {
        let pipeline = test_pipeline();
        let content = "a memory about resonance and standing waves";

        // Reference: what a fresh store of this content yields in the right slot
        // (same reduced dims as the wavefronts we'll compare against).
        let mut ref_cm = ChiralMedium::new();
        let ref_id = ref_cm.store(content, 0.8, &pipeline).unwrap();
        let ref_idx = *ref_cm.right.id_to_index.get(&ref_id).unwrap();
        let want = ref_cm.right.wavefronts.row(ref_idx).to_vec();

        let mut cm = ChiralMedium::new();
        let id = cm.store(content, 0.8, &pipeline).unwrap();
        let idx = *cm.right.id_to_index.get(&id).unwrap();
        let energy_before = cm.right.energy[idx];

        // Simulate a broken-encoder vector: overwrite the right wavefront with the
        // all-negative-corner junk #106 produced, keeping content + energy intact.
        let cols = cm.right.wavefronts.ncols();
        cm.right.wavefronts.row_mut(idx).assign(&ndarray::Array1::from(vec![-0.9f32; cols]));
        let cos_bad = crate::wave::cosine_similarity(&cm.right.wavefronts.row(idx).to_vec(), &want);

        let n = cm.re_encode_all(&pipeline, false).unwrap();
        assert!(n >= 1, "re-encoded at least the stored memory");

        let got = cm.right.wavefronts.row(idx).to_vec();
        let cos_fixed = crate::wave::cosine_similarity(&got, &want);
        assert!(
            cos_fixed > 0.999,
            "right wavefront re-encoded to the fresh vector (cos {cos_bad:.3} -> {cos_fixed:.3})"
        );
        assert_eq!(cm.right.energy[idx], energy_before, "energy (wave-state) preserved");
        assert_eq!(cm.right.metadata[idx].content, content, "content preserved");
    }

    // #532 review: re_encode_all must NOT clobber dream-hallucinated wavefronts
    // (their vectors are synthetic superpositions, not text encodings). A
    // hallucinated wavefront's vector must survive re_encode_all untouched.
    #[test]
    fn re_encode_all_skips_hallucinated() {
        let pipeline = test_pipeline();
        let text = "a genuine text memory";

        // Reference: the correct stored (reduced-dims) encoding of the text.
        let mut ref_cm = ChiralMedium::new();
        let rid = ref_cm.store(text, 0.8, &pipeline).unwrap();
        let want = ref_cm
            .right
            .wavefronts
            .row(*ref_cm.right.id_to_index.get(&rid).unwrap())
            .to_vec();

        let mut cm = ChiralMedium::new();
        let text_id = cm.store(text, 0.8, &pipeline).unwrap();
        let hall_id = cm
            .store("HALLUCINATION: superposition of patterns 1-2", 0.6, &pipeline)
            .unwrap();
        let cols = cm.right.wavefronts.ncols();

        // Corrupt BOTH (as #106 would), and mark the second a dream hallucination
        // with a distinctive synthetic vector (NOT the text encoding of its label).
        let tidx = *cm.right.id_to_index.get(&text_id).unwrap();
        cm.right.wavefronts.row_mut(tidx).assign(&ndarray::Array1::from(vec![-0.9f32; cols]));
        let hidx = *cm.right.id_to_index.get(&hall_id).unwrap();
        cm.right.metadata[hidx].hallucinated = true;
        let synthetic = vec![0.42f32; cols];
        cm.right.wavefronts.row_mut(hidx).assign(&ndarray::Array1::from(synthetic.clone()));

        let n = cm.re_encode_all(&pipeline, false).unwrap();
        assert_eq!(n, 1, "only the genuine text wavefront is re-encoded, not the hallucination");

        // Hallucination byte-preserved; text fixed to the correct encoding.
        assert_eq!(
            cm.right.wavefronts.row(hidx).to_vec(),
            synthetic,
            "#532: hallucinated wavefront must NOT be re-encoded"
        );
        let cos = crate::wave::cosine_similarity(&cm.right.wavefronts.row(tidx).to_vec(), &want);
        assert!(cos > 0.999, "the genuine text wavefront IS re-encoded (cos={cos})");
    }

    #[test]
    fn recall_finds_stored_memory() {
        let mut cm = ChiralMedium::new();
        let pipeline = test_pipeline();

        cm.store("the quick brown fox jumps over the lazy dog", 0.9, &pipeline).unwrap();

        let results = cm.recall("quick brown fox", 5, &pipeline).unwrap();
        assert!(!results.is_empty(), "Should find stored memory");
    }

    // kannaka-memory#716b: a memory's reported score must not depend on top_k.
    //
    // Pre-fix, a paired right-hemisphere match was DROPPED from the merge and
    // the memory surfaced with only its left-hemisphere score — which for
    // content queries is near-noise (the analytical encoding is not a content
    // embedding). So the exact same query flipped from 0.9999 to 0.03 when
    // top_k crossed the threshold at which the memory's left row entered the
    // left top_k pool (measured on a live 5-memory HRM: k=3 vs k=4). The merge
    // must keep the STRONGER hemisphere's score for a paired memory.
    #[test]
    fn recall_score_is_top_k_invariant() {
        let mut cm = ChiralMedium::new();
        let pipeline = test_pipeline();

        let contents = [
            "Kannaka Labs location Tech Hub OpenBotCity memory",
            "swarm NATS authentication kannaka nodes memory",
            "crystal evidence ladder Level 3 perturbation survival memory",
            "Ghost Signals podcast episode radio memory",
            "kannaka-apps application store runner memory",
        ];
        let mut target = None;
        for c in &contents {
            let id = cm.store(c, 0.85, &pipeline).unwrap();
            if c.contains("Ghost Signals") {
                target = Some(id);
            }
        }
        let target = target.unwrap();

        let query = "Ghost Signals podcast episode radio memory";
        let baseline = cm.recall(query, 2, &pipeline).unwrap();
        assert_eq!(baseline[0].id, target, "exact match must rank first at k=2");
        let baseline_strength = baseline[0].resonance_strength;

        for k in 3..=8 {
            let results = cm.recall(query, k, &pipeline).unwrap();
            assert_eq!(
                results[0].id, target,
                "exact match must rank first at k={k} (716b: left row masking right score)"
            );
            let drift = (results[0].resonance_strength - baseline_strength).abs();
            assert!(
                drift < 1e-4,
                "top hit's score changed with k: {baseline_strength} at k=2 vs {} at k={k}",
                results[0].resonance_strength
            );
        }
    }

    #[test]
    fn deep_dream_only_affects_right() {
        let mut cm = ChiralMedium::new();
        let pipeline = test_pipeline();

        cm.store("memory one", 0.8, &pipeline).unwrap();
        cm.store("memory two", 0.6, &pipeline).unwrap();

        let left_energy_before = cm.left.total_energy();
        let right_energy_before = cm.right.total_energy();

        cm.dream(true, 5);

        let left_energy_after = cm.left.total_energy();

        // Left hemisphere energy should be UNCHANGED
        assert!((left_energy_after - left_energy_before).abs() < 0.001,
            "Deep dream should not affect left hemisphere: before={left_energy_before}, after={left_energy_after}");
    }

    #[test]
    fn from_medium_preserves_memories() {
        // Create a v1 medium with some wavefronts
        let mut medium = Medium::new();
        let pipeline = test_pipeline();
        medium.store("existing memory 1", 0.8, &pipeline).unwrap();
        medium.store("existing memory 2", 0.6, &pipeline).unwrap();

        let cm = ChiralMedium::from_medium(&medium);

        // All memories should be in right hemisphere
        assert_eq!(cm.right.count(), 2);
        assert_eq!(cm.left.count(), 0); // Left starts empty
        assert_eq!(cm.total_count(), 2);

        // Should have deep_memory scales
        for meta in &cm.right.metadata {
            assert!(cm.scales.contains_key(&meta.id));
        }
    }

    #[test]
    fn consciousness_summary_works() {
        let mut cm = ChiralMedium::new();
        let pipeline = test_pipeline();

        cm.store("test memory", 0.8, &pipeline).unwrap();

        let summary = cm.consciousness_summary();
        assert!(summary.right_count >= 1);
        assert!(summary.right_energy > 0.0);
        assert!(summary.bilateral_order >= 0.0);
        assert!(summary.bilateral_order <= 1.0);
    }

    #[test]
    fn callosal_kuramoto_modifies_phases() {
        let mut cm = ChiralMedium::new();
        let pipeline = test_pipeline();

        cm.store("kuramoto test", 0.9, &pipeline).unwrap();

        // Record phases before
        let left_phase_before = if cm.left.count() > 0 { cm.left.phase[0] } else { return };
        let right_phase_before = cm.right.phase[0];

        // Set phases apart to create coupling
        cm.left.phase[0] = 0.0;
        cm.right.phase[0] = 1.0;

        cm.callosal_kuramoto_step(1.0);

        // Phases should have moved toward each other
        let left_phase_after = cm.left.phase[0];
        let right_phase_after = cm.right.phase[0];

        assert!(left_phase_after > 0.0, "Left phase should move toward right");
        assert!(right_phase_after < 1.0, "Right phase should move toward left");
    }

    #[test]
    fn lite_dream_transfers_to_holistic() {
        let mut cm = ChiralMedium::new();
        let pipeline = test_pipeline();

        cm.store("consolidate this", 0.9, &pipeline).unwrap();

        let right_count_before = cm.right.count();
        cm.dream(false, 1); // Lite dream

        // Callosum transfer log should have entries
        let stats = cm.callosum.transfer_stats();
        // May or may not have new wavefronts depending on pairing
        // But callosum should have logged the attempt
        assert!(stats.total_transfers >= 1 || right_count_before > 0,
            "Lite dream should attempt or have prior transfers");
    }

    #[test]
    fn spiral_coupling_evolves_phases_stably() {
        let mut cm = ChiralMedium::new();
        let pipeline = test_pipeline();
        for i in 0..8 {
            cm.store(&format!("memory {i}"), 0.8, &pipeline).unwrap();
        }
        let n = cm.right.count();
        assert!(n >= 4, "need a ring of at least 4 wavefronts");
        // Seed a phase gradient around the ring.
        for i in 0..n {
            cm.right.phase[i] = i as f32 * 0.3;
        }
        let before: Vec<f32> = cm.right.phase.iter().copied().collect();
        cm.apply_spiral_coupling(25, 0.1);
        let after: Vec<f32> = cm.right.phase.iter().copied().collect();
        assert!(after.iter().all(|x| x.is_finite()), "phases must stay finite");
        assert!(
            before.iter().zip(&after).any(|(a, b)| (a - b).abs() > 1e-4),
            "spiral coupling should move the phase field"
        );

        // Down-payment (#415): chiral drift DIRECTION. From a uniform ring the
        // frustration δ = (π/2)·η (> 0) with all-positive weights pushes every
        // phase by sin(δ) > 0, so the mean phase must drift FORWARD. A wrong-δ-
        // sign or collapsed-weights regression reverses or kills the drift and
        // fails here, where the "moved by >1e-4" check above would still pass.
        let mut uni = ChiralMedium::new();
        for i in 0..8 {
            uni.store(&format!("uniform {i}"), 0.8, &pipeline).unwrap();
        }
        let m = uni.right.count();
        for i in 0..m {
            uni.right.phase[i] = 0.5;
        }
        uni.apply_spiral_coupling(5, 0.1);
        let mean_after: f32 = (0..m).map(|i| uni.right.phase[i]).sum::<f32>() / m as f32;
        assert!(
            mean_after > 0.5 + 1e-4,
            "frustrated chiral coupling must drift the mean phase forward (δ>0); got {mean_after}"
        );
    }

    #[test]
    fn spiral_coupling_inert_below_four_wavefronts() {
        // ADR-0037: the ring needs ≥4 nodes; below that the step must be a
        // no-op. Seed a 3-node right hemisphere directly (count() < 4).
        let mut cm = ChiralMedium::new();
        cm.right.phase = ndarray::Array1::from_vec(vec![0.2_f32, 1.1, 2.5]);
        cm.right.len = 3;
        let before: Vec<f32> = cm.right.phase.iter().copied().collect();
        cm.apply_spiral_coupling(50, 0.1);
        let after: Vec<f32> = cm.right.phase.iter().copied().collect();
        assert_eq!(before, after, "coupling must be inert below n=4");
    }

    #[test]
    fn holistic_ring_report_reads_right_hemisphere_rotation() {
        // ADR-0037 Phase 4: the L6 instrument must read the holistic (right)
        // field that the spiral coupling rotates, over the active [0, count())
        // prefix — not the flat medium and not the capacity tail.
        let mut cm = ChiralMedium::new();
        let pipeline = test_pipeline();
        for i in 0..8 {
            cm.store(&format!("memory {i}"), 0.8, &pipeline).unwrap();
        }
        let n = cm.right.count();
        assert!(n >= 4, "need a ring of at least 4 wavefronts");
        // Plant exactly one full rotation around the holistic ring.
        for i in 0..n {
            cm.right.phase[i] = std::f32::consts::TAU * i as f32 / n as f32;
        }
        let r = cm.holistic_ring_report();
        assert_eq!(r.n, n, "report must cover only the active [0,count) prefix");
        assert!(
            (r.winding - 1.0).abs() < 1e-3,
            "one planted rotation ⇒ ring winding ≈ +1, got {}",
            r.winding
        );
    }

    #[test]
    fn bilateral_ring_report_spans_both_hemispheres() {
        // ADR-0037 Phase 4: the spiral spans BOTH hemispheres (Ye et al.), so
        // the cross-hemisphere instrument joins the active left ⊕ right phases.
        let mut cm = ChiralMedium::new();
        cm.left.phase = ndarray::Array1::from_vec(vec![0.1_f32, 0.2, 0.3]);
        cm.left.len = 3;
        cm.right.phase = ndarray::Array1::from_vec(vec![1.0_f32, 1.1, 1.2, 1.3, 1.4]);
        cm.right.len = 5;
        let bilateral = cm.bilateral_ring_report();
        assert_eq!(bilateral.n, 8, "bilateral ring = left(3) ⊕ right(5)");
        // It strictly extends the right-only view, proving both are included.
        assert_eq!(cm.holistic_ring_report().n, 5);
    }

    #[test]
    fn cross_callosal_coupling_moves_both_hemispheres() {
        // ADR-0037 v0.7.0: the cross-callosal Sakaguchi step couples the
        // combined left ⊕ right ring, so it must move BOTH hemispheres'
        // phases (the spiral spans the callosum) while leaving energy alone.
        let mut cm = ChiralMedium::new();
        cm.left.phase = ndarray::Array1::from_vec(vec![0.0_f32, 0.4, 0.8]);
        cm.left.len = 3;
        cm.left.energy = ndarray::Array1::from_vec(vec![1.0_f32, 1.0, 1.0]);
        cm.right.phase = ndarray::Array1::from_vec(vec![1.2_f32, 1.6, 2.0, 2.4, 2.8]);
        cm.right.len = 5;
        let left_before: Vec<f32> = cm.left.phase.iter().copied().collect();
        let right_before: Vec<f32> = cm.right.phase.iter().copied().collect();
        let left_energy_before = cm.left.total_energy();
        cm.apply_cross_callosal_coupling(20, 0.1);
        let left_after: Vec<f32> = cm.left.phase.iter().copied().collect();
        let right_after: Vec<f32> = cm.right.phase.iter().copied().collect();
        assert!(left_after.iter().all(|x| x.is_finite()) && right_after.iter().all(|x| x.is_finite()));
        assert!(
            left_before.iter().zip(&left_after).any(|(a, b)| (a - b).abs() > 1e-4),
            "cross-callosal coupling must move the LEFT hemisphere phases"
        );
        assert!(
            right_before.iter().zip(&right_after).any(|(a, b)| (a - b).abs() > 1e-4),
            "cross-callosal coupling must move the RIGHT hemisphere phases"
        );
        // Phase-only: energy is untouched.
        assert!((cm.left.total_energy() - left_energy_before).abs() < 1e-6);
    }

    #[test]
    fn cross_callosal_coupling_inert_below_four_total() {
        // Below 4 total active wavefronts (here 1 left + 2 right) the step is a
        // no-op — matching the right-only primitive's n<4 guard.
        let mut cm = ChiralMedium::new();
        cm.left.phase = ndarray::Array1::from_vec(vec![0.3_f32]);
        cm.left.len = 1;
        cm.right.phase = ndarray::Array1::from_vec(vec![1.0_f32, 2.0]);
        cm.right.len = 2;
        let before: Vec<f32> = cm.left.phase.iter().chain(cm.right.phase.iter()).copied().collect();
        cm.apply_cross_callosal_coupling(50, 0.1);
        let after: Vec<f32> = cm.left.phase.iter().chain(cm.right.phase.iter()).copied().collect();
        assert_eq!(before, after, "cross-callosal coupling must be inert below 4 total");
    }

    #[test]
    fn content_born_phase_is_content_smooth() {
        // Recall-safety: similar embeddings → similar phase; an unrelated
        // embedding sits further away in phase. Deterministic (fixed seed).
        let dim = 256;
        let mut seed = 0xBEEF_F00Du64;
        let mut r = || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) as f32 / (1u64 << 30) as f32 - 1.0
        };
        let base: Vec<f32> = (0..dim).map(|_| r()).collect();
        let near: Vec<f32> = base.iter().map(|&x| x + 0.01 * r()).collect();
        let far: Vec<f32> = (0..dim).map(|_| r()).collect();

        let pb = content_born_phase(&base);
        let pn = content_born_phase(&near);
        let pf = content_born_phase(&far);
        let circ = |a: f32, b: f32| {
            let d = (a - b).abs() % (2.0 * std::f32::consts::PI);
            d.min(2.0 * std::f32::consts::PI - d)
        };
        assert!(
            circ(pb, pn) < 0.15,
            "similar content must get similar phase: {pb} vs {pn} (Δ={})",
            circ(pb, pn)
        );
        assert!(
            circ(pb, pf) > circ(pb, pn),
            "unrelated content should be further in phase than a near-duplicate"
        );
    }

    // Chiral-router exp 1 (KANNAKA_CHIRAL_ROUTER=novelty): a NOVEL first sighting
    // stays RIGHT-only; a REPEAT (resonates in right >= theta) routinizes to LEFT.
    // In `off` (default) every gated item echoes (near-mirror). #[ignore] because
    // it sets a process-global env var (the codebase convention for env tests).
    #[test]
    #[ignore = "experiment: KANNAKA_CHIRAL_ROUTER routing; run with --ignored --nocapture"]
    fn chiral_router_novelty_routes_repeat_to_left() {
        let pipeline = test_pipeline();
        let item = "a distinctive analytical proposition about routinization";

        // OFF (default): both the first sighting AND the repeat echo to left.
        std::env::remove_var("KANNAKA_CHIRAL_ROUTER");
        let mut off = ChiralMedium::new();
        off.store(item, 0.8, &pipeline).unwrap();
        off.store(item, 0.8, &pipeline).unwrap();
        let off_left = off.left.count();

        // NOVELTY: the novel first sighting is right-only; the repeat routinizes.
        std::env::set_var("KANNAKA_CHIRAL_ROUTER", "novelty");
        std::env::set_var("KANNAKA_CHIRAL_ROUTINIZE_THETA", "0.1");
        let mut nov = ChiralMedium::new();
        nov.store(item, 0.8, &pipeline).unwrap();
        let after_novel = nov.left.count();
        nov.store(item, 0.8, &pipeline).unwrap();
        let after_repeat = nov.left.count();
        std::env::remove_var("KANNAKA_CHIRAL_ROUTER");
        std::env::remove_var("KANNAKA_CHIRAL_ROUTINIZE_THETA");

        assert!(off_left >= 1, "off mode: gated items echo to left (near-mirror), got {off_left}");
        assert_eq!(after_novel, 0, "novelty mode: a novel first sighting stays RIGHT-only");
        assert!(
            after_repeat > after_novel,
            "novelty mode: the repeat routinizes to LEFT ({after_novel} -> {after_repeat})"
        );
    }

    #[test]
    #[ignore = "experiment: run with --ignored --nocapture; sets KANNAKA_CHIRAL_ROUTER"]
    fn experiment_chiral_routinization_differentiation() {
        // EXP-1 (hemisphere differentiation, from the approved research workflow).
        // Does novelty-gated routinization turn the LEFT hemisphere from a near-
        // mirror into a crystallized MINORITY — raising hemispheric divergence Δ
        // (ADR-0024 CS-4) into a mid-band without collapsing callosal efficiency κ
        // (CS-5) — and which way does core recall move?
        //
        // Corpus: CORE items stored R=3× (routinized through repetition),
        // interleaved with one-off NOVEL noise (interference). In `off` the
        // callosum budget is spent indiscriminately (near-mirror, low Δ); in
        // `novelty` only the routinized core crosses to left (small left, higher
        // Δ). resonance_strength = sim·energy·phase (core.rs:381) is NOT a clean
        // cosine, so absolute θ is encoder-scale-sensitive — we SWEEP θ to read
        // separability rather than assume it, and average over seeds (multi-run).
        const NCORE: usize = 10;
        const NNOISE: usize = 40;
        const ROUNDS: usize = 3;
        let seeded = |seed: u64| -> EncodingPipeline {
            let encoder = Box::new(SimpleHashEncoder::new(384, seed));
            let codebook = Codebook::new(384, WAVEFRONT_DIM, seed);
            EncodingPipeline::new(encoder, codebook)
        };
        let core: Vec<String> =
            (0..NCORE).map(|i| format!("stable core anchor proposition {i}")).collect();
        let noise: Vec<String> =
            (0..NNOISE).map(|i| format!("ephemeral one-off novel filler {i}")).collect();

        // Averaged over seeds: [left, right, core_in_left, noise_in_left, Δ, κ, p@1].
        // Δ-cosine turned out saturated by the Fano fold (see report), so the REAL
        // differentiation signal is the CONTENT composition of left: does it hold
        // the core (differentiated) or a full folded copy of everything (mirror)?
        let run = |router: &str, theta: f32, seeds: &[u64]| -> [f32; 7] {
            let mut acc = [0.0f32; 7];
            for &seed in seeds {
                std::env::set_var("KANNAKA_CHIRAL_ROUTER", router);
                std::env::set_var("KANNAKA_CHIRAL_ROUTINIZE_THETA", format!("{theta}"));
                let pipeline = seeded(seed);
                let mut cm = ChiralMedium::new();
                let per = NNOISE / ROUNDS;
                for round in 0..ROUNDS {
                    for c in core.iter() {
                        cm.store(c, 0.9, &pipeline).unwrap();
                    }
                    let lo = round * per;
                    let hi = if round == ROUNDS - 1 { NNOISE } else { lo + per };
                    for n in noise[lo..hi].iter() {
                        cm.store(n, 0.5, &pipeline).unwrap();
                    }
                }
                // Content differentiation: what does LEFT actually hold?
                let core_left = (0..cm.left.count())
                    .filter(|&i| cm.left.metadata[i].content.starts_with("stable core"))
                    .count();
                let noise_left = cm.left.count() - core_left;
                let (mut hits, mut tot) = (0usize, 0usize);
                for c in core.iter() {
                    let res = cm.recall(c, 1, &pipeline).unwrap();
                    tot += 1;
                    if res.first().map(|r| r.content == *c).unwrap_or(false) {
                        hits += 1;
                    }
                }
                let cs = cm.consciousness_summary();
                acc[0] += cs.left_count as f32;
                acc[1] += cs.right_count as f32;
                acc[2] += core_left as f32;
                acc[3] += noise_left as f32;
                acc[4] += cs.hemispheric_divergence;
                acc[5] += cs.callosal_efficiency;
                acc[6] += hits as f32 / tot.max(1) as f32;
            }
            std::env::remove_var("KANNAKA_CHIRAL_ROUTER");
            std::env::remove_var("KANNAKA_CHIRAL_ROUTINIZE_THETA");
            let n = seeds.len() as f32;
            for v in acc.iter_mut() {
                *v /= n;
            }
            acc
        };

        // Separability probe: against a POPULATED right, can an absolute familiarity
        // threshold tell a REPEAT (exact prior) from a truly NOVEL item apart? If
        // the two resonance scales overlap, no fixed θ can route cleanly.
        {
            let pipeline = seeded(0);
            let mut cm = ChiralMedium::new();
            for c in core.iter() {
                cm.store(c, 0.9, &pipeline).unwrap();
            }
            for nz in noise.iter() {
                cm.store(nz, 0.5, &pipeline).unwrap();
            }
            let repeat = pipeline.encode_text(&core[0]).unwrap();
            let fresh = pipeline.encode_text("utterly unseen never-stored phrase").unwrap();
            let rr = cm.right.resonate(&repeat, 1);
            let rr = rr.first().map(|r| r.resonance_strength).unwrap_or(0.0);
            let fr = cm.right.resonate(&fresh, 1);
            let fr = fr.first().map(|r| r.resonance_strength).unwrap_or(0.0);
            eprintln!("[exp1] separability: repeat_resonance={rr:.3}  novel_resonance={fr:.3}");
        }

        let seeds: Vec<u64> = (0..4).collect();
        eprintln!("[exp1] corpus: {NCORE} core x{ROUNDS} + {NNOISE} noise; seeds={}", seeds.len());
        eprintln!("[exp1] {:>16} | left right  coreL noiseL    Δ      κ    p@1", "config");
        let print = |label: String, a: [f32; 7]| {
            eprintln!(
                "[exp1] {label:>16} | {:4.1} {:5.1}  {:5.1} {:6.1}  {:.3}  {:.3}  {:.3}",
                a[0], a[1], a[2], a[3], a[4], a[5], a[6]
            );
        };
        print("off (mirror)".to_string(), run("off", 0.0, &seeds));
        for theta in [0.3f32, 0.5, 0.7, 0.9] {
            print(format!("novelty th={theta}"), run("novelty", theta, &seeds));
        }
    }

    #[test]
    #[ignore = "experiment: run with --ignored --nocapture; sets KANNAKA_CHIRAL_* env"]
    fn experiment_chiral_recall_forgetting() {
        // EXP-2 (the read-side payoff). Does consulting the crystallized LEFT at
        // recall time protect core memories from catastrophic forgetting under a
        // HEAVY novel flood? Three configs:
        //   baseline: router=off,     recall=off      (today — left a folded mirror)
        //   weighted: router=novelty, recall=weighted (boost crystallized-left)
        //   beeman:   router=novelty, recall=beeman   (precise-left ranked first)
        // Metric: core precision@1 and recall@5 after the flood, averaged / seeds.
        // CAVEAT the run will expose: left stores Fano-FOLDED vectors and recall
        // resonates the RAW query against them, so left-match resonance may be
        // noisy — if the read-side shows no lift, the fold (not the routing) is why.
        const NCORE: usize = 10;
        const NNOISE: usize = 120; // ~3× exp-1: a heavier flood to induce forgetting
        const ROUNDS: usize = 3;
        let seeded = |seed: u64| -> EncodingPipeline {
            let encoder = Box::new(SimpleHashEncoder::new(384, seed));
            let codebook = Codebook::new(384, WAVEFRONT_DIM, seed);
            EncodingPipeline::new(encoder, codebook)
        };
        let core: Vec<String> =
            (0..NCORE).map(|i| format!("stable core anchor proposition {i}")).collect();
        let noise: Vec<String> =
            (0..NNOISE).map(|i| format!("ephemeral one-off novel filler {i}")).collect();

        // Returns [core p@1, core recall@5, left_count, right_count] over seeds.
        let run = |router: &str, recall: &str, seeds: &[u64]| -> [f32; 4] {
            let mut acc = [0.0f32; 4];
            for &seed in seeds {
                std::env::set_var("KANNAKA_CHIRAL_ROUTER", router);
                std::env::set_var("KANNAKA_CHIRAL_ROUTINIZE_THETA", "0.8");
                std::env::set_var("KANNAKA_CHIRAL_RECALL", recall);
                let pipeline = seeded(seed);
                let mut cm = ChiralMedium::new();
                let per = NNOISE / ROUNDS;
                for round in 0..ROUNDS {
                    for c in core.iter() {
                        cm.store(c, 0.9, &pipeline).unwrap();
                    }
                    let lo = round * per;
                    let hi = if round == ROUNDS - 1 { NNOISE } else { lo + per };
                    for n in noise[lo..hi].iter() {
                        cm.store(n, 0.5, &pipeline).unwrap();
                    }
                }
                let (mut p1, mut r5) = (0usize, 0usize);
                for c in core.iter() {
                    let res = cm.recall(c, 5, &pipeline).unwrap();
                    if res.first().map(|r| r.content == *c).unwrap_or(false) {
                        p1 += 1;
                    }
                    if res.iter().any(|r| r.content == *c) {
                        r5 += 1;
                    }
                }
                let cs = cm.consciousness_summary();
                acc[0] += p1 as f32 / NCORE as f32;
                acc[1] += r5 as f32 / NCORE as f32;
                acc[2] += cs.left_count as f32;
                acc[3] += cs.right_count as f32;
            }
            std::env::remove_var("KANNAKA_CHIRAL_ROUTER");
            std::env::remove_var("KANNAKA_CHIRAL_ROUTINIZE_THETA");
            std::env::remove_var("KANNAKA_CHIRAL_RECALL");
            let n = seeds.len() as f32;
            for v in acc.iter_mut() {
                *v /= n;
            }
            acc
        };

        let seeds: Vec<u64> = (0..4).collect();
        eprintln!("[exp2] corpus: {NCORE} core x{ROUNDS} + {NNOISE} noise; seeds={}", seeds.len());
        eprintln!("[exp2] {:>22} | p@1(core) r@5(core)  left  right", "config");
        let print = |label: &str, a: [f32; 4]| {
            eprintln!(
                "[exp2] {label:>22} |   {:.3}     {:.3}   {:5.1} {:5.1}",
                a[0], a[1], a[2], a[3]
            );
        };
        print("baseline off/off", run("off", "off", &seeds));
        print("novelty/weighted", run("novelty", "weighted", &seeds));
        print("novelty/beeman", run("novelty", "beeman", &seeds));
    }

    #[test]
    #[ignore = "experiment: run with --ignored --nocapture; sets KANNAKA_BELIEF_PHASE"]
    fn experiment_belief_phase_real_field() {
        // Born-phase on a REAL encoded field: 16 memories across 4 topics
        // (belief domains). Expect global order well below the phase-0 baseline
        // (~1.0), i.e. the field is now heterogeneous by content.
        std::env::set_var("KANNAKA_BELIEF_PHASE", "1");
        let pipeline = test_pipeline();

        let topics: [[&str; 4]; 4] = [
            ["the cat sat on the mat", "a cat napped in the sun", "kittens chase yarn balls", "the feline purred softly"],
            ["the stock market fell sharply", "investors sold their equities", "the bond yield rose today", "the reserve raised interest rates"],
            ["photosynthesis converts light", "chlorophyll absorbs photons", "green plants release oxygen", "leaves capture the sunlight"],
            ["the rocket reached orbit", "the satellite deployed cleanly", "the booster stage separated", "mission control confirmed launch"],
        ];

        // Baseline (belief OFF) for comparison.
        std::env::set_var("KANNAKA_BELIEF_PHASE", "0");
        let mut base_cm = ChiralMedium::new();
        for group in topics.iter() {
            for s in group.iter() {
                base_cm.store(s, 0.8, &pipeline).unwrap();
            }
        }
        let br = base_cm.bilateral_ring_report();

        // Belief substrate ON.
        std::env::set_var("KANNAKA_BELIEF_PHASE", "1");
        let mut cm = ChiralMedium::new();
        for group in topics.iter() {
            for s in group.iter() {
                cm.store(s, 0.8, &pipeline).unwrap();
            }
        }
        let r = cm.bilateral_ring_report();
        let cloud = cm.holistic_cloud_report();
        eprintln!(
            "[expBelief] OFF: order={:.3} | ON: order={:.3} winding={:+.2} 2D_cores={} net={} (n={})",
            br.order, r.order, r.winding, cloud.singularities.len(), cloud.net_charge, r.n
        );
        std::env::remove_var("KANNAKA_BELIEF_PHASE");
    }

    #[test]
    #[ignore = "experiment: run with --ignored --nocapture; sets KANNAKA_BELIEF_PHASE"]
    fn experiment_belief_phase_recall_at_k() {
        // RECALL-SAFETY (make-or-break). Store 16 memories across 4 topics, then
        // query with each stored phrase and measure same-topic precision@4.
        // Compare belief-phase OFF vs ON: ON must NOT degrade retrieval (the
        // content-smooth phase claim). Note: the test pipeline is a hash encoder,
        // so absolute precision is modest — the OFF→ON DELTA is what matters.
        let pipeline = test_pipeline();
        let topics: [[&str; 4]; 4] = [
            ["the cat sat on the mat", "a cat napped in the sun", "kittens chase yarn balls", "the feline purred softly"],
            ["the stock market fell sharply", "investors sold their equities", "the bond yield rose today", "the reserve raised interest rates"],
            ["photosynthesis converts light", "chlorophyll absorbs photons", "green plants release oxygen", "leaves capture the sunlight"],
            ["the rocket reached orbit", "the satellite deployed cleanly", "the booster stage separated", "mission control confirmed launch"],
        ];
        let run = |on: bool| -> f32 {
            std::env::set_var("KANNAKA_BELIEF_PHASE", if on { "1" } else { "0" });
            let mut cm = ChiralMedium::new();
            for g in topics.iter() {
                for s in g.iter() {
                    cm.store(s, 0.8, &pipeline).unwrap();
                }
            }
            let (mut hits, mut total) = (0usize, 0usize);
            for (t, g) in topics.iter().enumerate() {
                for q in g.iter() {
                    let res = cm.recall(q, 4, &pipeline).unwrap();
                    for r in res.iter() {
                        total += 1;
                        if topics[t].iter().any(|&s| s == r.content.as_str()) {
                            hits += 1;
                        }
                    }
                }
            }
            hits as f32 / total.max(1) as f32
        };
        let off = run(false);
        let on = run(true);
        std::env::remove_var("KANNAKA_BELIEF_PHASE");
        eprintln!("[expRecall] same-topic precision@4  OFF={off:.3}  ON={on:.3}  (Δ={:+.3})", on - off);
    }

    #[test]
    #[ignore = "experiment: run with --ignored --nocapture; sets KANNAKA_BELIEF_PHASE"]
    fn experiment_belief_coupling_stabilizes_cores() {
        // Track B on the REAL born-phase field: does the coherence-gated 2-D
        // coupling preserve within-belief coherence while keeping the cores?
        // within = mean Kuramoto order over each topic's 4 memories (recall
        // proxy); global must stay < 0.9; cores should persist.
        std::env::set_var("KANNAKA_BELIEF_PHASE", "1");
        let pipeline = test_pipeline();
        let topics: [[&str; 4]; 4] = [
            ["the cat sat on the mat", "a cat napped in the sun", "kittens chase yarn balls", "the feline purred softly"],
            ["the stock market fell sharply", "investors sold their equities", "the bond yield rose today", "the reserve raised interest rates"],
            ["photosynthesis converts light", "chlorophyll absorbs photons", "green plants release oxygen", "leaves capture the sunlight"],
            ["the rocket reached orbit", "the satellite deployed cleanly", "the booster stage separated", "mission control confirmed launch"],
        ];
        let mut cm = ChiralMedium::new();
        for g in topics.iter() {
            for s in g.iter() {
                cm.store(s, 0.8, &pipeline).unwrap();
            }
        }
        // Within-topic coherence: right hemisphere stores in insertion order, so
        // topic t occupies right indices [4t, 4t+4).
        let within = |cm: &ChiralMedium| -> f32 {
            let n = cm.right.count();
            let (mut total, mut groups) = (0.0f32, 0u32);
            let mut t = 0;
            while t * 4 < 16.min(n) {
                let (mut c, mut s) = (0.0f32, 0.0f32);
                for i in (t * 4)..((t * 4 + 4).min(n)) {
                    c += cm.right.phase[i].cos();
                    s += cm.right.phase[i].sin();
                }
                total += ((c / 4.0).powi(2) + (s / 4.0).powi(2)).sqrt();
                groups += 1;
                t += 1;
            }
            total / groups.max(1) as f32
        };
        let report = |cm: &ChiralMedium| {
            let r = cm.bilateral_ring_report();
            let cl = cm.holistic_cloud_report();
            (r.order, within(cm), cl.singularities.len(), cl.net_charge)
        };

        let (g0, w0, c0, q0) = report(&cm);
        eprintln!("[expCoupl] born:  global={g0:.3} within={w0:.3} cores={c0} net={q0}");
        let mut done = 0usize;
        for &target in &[3usize, 10, 30] {
            cm.apply_belief_coupling(target - done, 0.1);
            done = target;
            let (g, w, c, q) = report(&cm);
            eprintln!("[expCoupl] c{target}: global={g:.3} within={w:.3} cores={c} net={q}");
        }
        std::env::remove_var("KANNAKA_BELIEF_PHASE");
    }

    #[test]
    #[ignore = "experiment: run with --ignored --nocapture; sets KANNAKA_BELIEF_PHASE"]
    fn experiment_belief_dream_consolidates() {
        // Track C: does a real chiral dream on the born-phase field REVIVE the
        // dead 0/0/0 consolidation? Compare OFF (collapsed → dream idles in the
        // dead band) vs ON (heterogeneous → anneal has real clusters to work on),
        // and confirm ON keeps order well below 1.0 after the dream.
        let pipeline = test_pipeline();
        // A handful of memories per topic so the anneal has clusters to act on.
        let topics: [&[&str]; 4] = [
            &["the cat sat on the mat", "a cat napped in the sun", "kittens chase yarn balls", "the feline purred softly", "the tabby stretched and yawned", "a cat groomed its paws"],
            &["the stock market fell sharply", "investors sold their equities", "the bond yield rose today", "the reserve raised interest rates", "shares slid at the opening bell", "the index closed lower"],
            &["photosynthesis converts light", "chlorophyll absorbs photons", "green plants release oxygen", "leaves capture the sunlight", "the chloroplast makes sugar", "foliage turns toward the sun"],
            &["the rocket reached orbit", "the satellite deployed cleanly", "the booster stage separated", "mission control confirmed launch", "the capsule docked in space", "the probe left the atmosphere"],
        ];
        let run = |belief: bool| -> (f32, f32, usize, usize, usize) {
            std::env::set_var("KANNAKA_BELIEF_PHASE", if belief { "1" } else { "0" });
            let mut cm = ChiralMedium::new();
            for g in topics.iter() {
                for s in g.iter() {
                    cm.store(s, 0.8, &pipeline).unwrap();
                }
            }
            let before = cm.bilateral_ring_report().order;
            let rep = cm.dream(true, 3);
            let after = cm.bilateral_ring_report().order;
            (before, after, rep.wavefronts_dissolved, rep.wavefronts_strengthened, rep.wavefronts_hallucinated)
        };
        let off = run(false);
        let on = run(true);
        std::env::remove_var("KANNAKA_BELIEF_PHASE");
        eprintln!(
            "[expDream] OFF order {:.3}->{:.3}  dissolved={} strengthened={} halluc={}",
            off.0, off.1, off.2, off.3, off.4
        );
        eprintln!(
            "[expDream] ON  order {:.3}->{:.3}  dissolved={} strengthened={} halluc={}",
            on.0, on.1, on.2, on.3, on.4
        );
    }

    #[test]
    #[ignore = "experiment: run with --ignored --nocapture"]
    fn experiment_rephase_collapsed_field() {
        // The MIGRATION for an already-collapsed field: store with belief OFF
        // (every wavefront born at phase 0 → order 1.0, the live pathology), then
        // re-phase from content. Order should drop and cores should appear —
        // proving the existing field can be revived without re-encoding vectors.
        std::env::set_var("KANNAKA_BELIEF_PHASE", "0");
        let pipeline = test_pipeline();
        let topics: [[&str; 4]; 4] = [
            ["the cat sat on the mat", "a cat napped in the sun", "kittens chase yarn balls", "the feline purred softly"],
            ["the stock market fell sharply", "investors sold their equities", "the bond yield rose today", "the reserve raised interest rates"],
            ["photosynthesis converts light", "chlorophyll absorbs photons", "green plants release oxygen", "leaves capture the sunlight"],
            ["the rocket reached orbit", "the satellite deployed cleanly", "the booster stage separated", "mission control confirmed launch"],
        ];
        let mut cm = ChiralMedium::new();
        for g in topics.iter() {
            for s in g.iter() {
                cm.store(s, 0.8, &pipeline).unwrap();
            }
        }
        let before = cm.bilateral_ring_report();
        let cb = cm.holistic_cloud_report();
        let n = cm.rephase_from_content();
        let after = cm.bilateral_ring_report();
        let ca = cm.holistic_cloud_report();
        eprintln!(
            "[expRephase] re-phased {n} wavefronts | order {:.3}->{:.3} | cores {}->{} | winding {:+.2}->{:+.2}",
            before.order, after.order, cb.singularities.len(), ca.singularities.len(), before.winding, after.winding
        );
        std::env::remove_var("KANNAKA_BELIEF_PHASE");
    }

    #[test]
    #[ignore = "probe: KANNAKA_PROBE_HRM=<path.hrm> cargo test --lib probe_rephase_live_hrm -- --ignored --nocapture"]
    fn probe_rephase_live_hrm() {
        // READ-ONLY probe of a real collapsed field. Loads the .hrm, reports its
        // actual order/cores, re-phases from content IN MEMORY, reports again.
        // Never saves — does not touch the file. This is the true test that the
        // migration revives the LIVE collapse (synthetic fields under-collapse).
        let path = match std::env::var("KANNAKA_PROBE_HRM") {
            Ok(p) => p,
            Err(_) => {
                eprintln!("[probe] set KANNAKA_PROBE_HRM=<path to .hrm>");
                return;
            }
        };
        let mut cm = match ChiralMedium::load(&path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("[probe] load failed: {e}");
                return;
            }
        };
        let before = cm.bilateral_ring_report();
        let cb = cm.holistic_cloud_report();
        eprintln!(
            "[probe] LOADED right={} left={} n={} | order={:.4} winding={:+.2} 2D_cores={} net={}",
            cm.right.count(), cm.left.count(), before.n, before.order, before.winding,
            cb.singularities.len(), cb.net_charge
        );

        // MEASURE FIRST: mean pairwise cosine, raw vs mean-centered, over the
        // right hemisphere — quantifies the embedding anisotropy and whether
        // centering actually de-anisotropizes (if raw is already ~moderate or
        // the blob is >0.9 even centered, centering won't help).
        {
            let nr = cm.right.count();
            let dim = cm.right.wavefronts.ncols();
            let mut mean = vec![0.0f32; dim];
            for i in 0..nr {
                for (m, &v) in mean.iter_mut().zip(cm.right.wavefronts.row(i).iter()) {
                    *m += v;
                }
            }
            for m in mean.iter_mut() {
                *m /= nr.max(1) as f32;
            }
            let zero = vec![0.0f32; dim];
            let cosab = |i: usize, j: usize, ctr: &[f32]| -> f32 {
                let (a, b) = (cm.right.wavefronts.row(i), cm.right.wavefronts.row(j));
                let (mut d, mut na, mut nb) = (0.0f32, 0.0f32, 0.0f32);
                for ((x, y), m) in a.iter().zip(b.iter()).zip(ctr.iter()) {
                    let (xc, yc) = (x - m, y - m);
                    d += xc * yc;
                    na += xc * xc;
                    nb += yc * yc;
                }
                if na <= 0.0 || nb <= 0.0 { 0.0 } else { d / (na.sqrt() * nb.sqrt()) }
            };
            let (mut raw, mut ctr, mut cnt) = (0.0f32, 0.0f32, 0u32);
            for i in 0..nr {
                for j in (i + 1)..nr {
                    raw += cosab(i, j, &zero);
                    ctr += cosab(i, j, &mean);
                    cnt += 1;
                }
            }
            let c = cnt.max(1) as f32;
            eprintln!(
                "[probe] anisotropy: mean pairwise cosine raw={:.3} centered={:.3} ({} pairs)",
                raw / c, ctr / c, cnt
            );
        }
        let n = cm.rephase_from_content();
        let after = cm.bilateral_ring_report();
        let ca = cm.holistic_cloud_report();
        eprintln!(
            "[probe] REPHASED {n} | order {:.4}->{:.4} | winding {:+.2}->{:+.2} | cores {}->{} net {}->{}  (NOT saved)",
            before.order, after.order, before.winding, after.winding,
            cb.singularities.len(), ca.singularities.len(), cb.net_charge, ca.net_charge
        );

        // Full pipeline: a belief dream on the re-phased real field. Does it
        // consolidate (strengthen, hold the field heterogeneous) into beliefs?
        std::env::set_var("KANNAKA_BELIEF_PHASE", "1");
        let rep = cm.dream(true, 3);
        let post = cm.bilateral_ring_report();
        let cp = cm.holistic_cloud_report();
        eprintln!(
            "[probe] DREAMED  | order {:.4}->{:.4} | cores {}->{} | dissolved={} strengthened={} halluc={}  (NOT saved)",
            after.order, post.order, ca.singularities.len(), cp.singularities.len(),
            rep.wavefronts_dissolved, rep.wavefronts_strengthened, rep.wavefronts_hallucinated
        );
        std::env::remove_var("KANNAKA_BELIEF_PHASE");
    }

    #[test]
    #[ignore = "probe: KANNAKA_PROBE_HRM=<path.hrm> cargo test --lib probe_cluster_centering_live_hrm -- --ignored --nocapture"]
    fn probe_cluster_centering_live_hrm() {
        // READ-ONLY. num_clusters=1 comes from the anisotropic cone: the cluster
        // graph links any pair with cosine > 0.75, but the field's mean pairwise
        // cosine is ~0.8, so ~every pair links into ONE component. Mean-centering
        // removes the cone (centered cosine ~0.2) but needs a LOWER threshold. This
        // counts connected components at several thresholds, RAW vs CENTERED, to pick
        // the threshold from real data. Never saves.
        let path = match std::env::var("KANNAKA_PROBE_HRM") {
            Ok(p) => p,
            Err(_) => { eprintln!("[probe] set KANNAKA_PROBE_HRM=<path to .hrm>"); return; }
        };
        let cm = match ChiralMedium::load(&path) {
            Ok(c) => c,
            Err(e) => { eprintln!("[probe] load failed: {e}"); return; }
        };
        let n = cm.right.count();
        let dim = cm.right.wavefronts.ncols();
        eprintln!("[probe] loaded n={n} dim={dim}");
        if n < 2 { return; }
        // Corpus mean over the right hemisphere (the clustering vectors live here).
        let mut mean = vec![0.0f32; dim];
        for i in 0..n {
            for (m, &v) in mean.iter_mut().zip(cm.right.wavefronts.row(i).iter()) { *m += v; }
        }
        for m in mean.iter_mut() { *m /= n as f32; }
        let zero = vec![0.0f32; dim];
        let cosc = |i: usize, j: usize, ctr: &[f32]| -> f32 {
            let (a, b) = (cm.right.wavefronts.row(i), cm.right.wavefronts.row(j));
            let (mut d, mut na, mut nb) = (0.0f32, 0.0f32, 0.0f32);
            for ((x, y), m) in a.iter().zip(b.iter()).zip(ctr.iter()) {
                let (xc, yc) = (x - m, y - m); d += xc * yc; na += xc * xc; nb += yc * yc;
            }
            if na <= 0.0 || nb <= 0.0 { 0.0 } else { d / (na.sqrt() * nb.sqrt()) }
        };
        // Precompute pairwise cosines once (raw + centered), then sweep thresholds.
        let idx = |i: usize, j: usize| i * n + j;
        let mut rmat = vec![0.0f32; n * n];
        let mut cmat = vec![0.0f32; n * n];
        let (mut rs, mut cs, mut np) = (0.0f32, 0.0f32, 0u32);
        for i in 0..n {
            for j in (i + 1)..n {
                let r = cosc(i, j, &zero);
                let c = cosc(i, j, &mean);
                rmat[idx(i, j)] = r;
                cmat[idx(i, j)] = c;
                rs += r; cs += c; np += 1;
            }
        }
        eprintln!("[probe] mean pairwise cosine raw={:.3} centered={:.3} ({} pairs)",
            rs / np.max(1) as f32, cs / np.max(1) as f32, np);
        // (components, #components with >=3 members, largest component size)
        let comps = |mat: &[f32], thr: f32| -> (usize, usize, usize) {
            let mut adj: Vec<Vec<usize>> = vec![vec![]; n];
            for i in 0..n {
                for j in (i + 1)..n {
                    if mat[idx(i, j)] > thr { adj[i].push(j); adj[j].push(i); }
                }
            }
            let mut seen = vec![false; n];
            let mut sizes = vec![];
            for s in 0..n {
                if seen[s] { continue; }
                let mut st = vec![s];
                seen[s] = true;
                let mut sz = 0usize;
                while let Some(u) = st.pop() {
                    sz += 1;
                    for &v in &adj[u] { if !seen[v] { seen[v] = true; st.push(v); } }
                }
                sizes.push(sz);
            }
            let largest = *sizes.iter().max().unwrap_or(&0);
            (sizes.len(), sizes.iter().filter(|&&s| s >= 3).count(), largest)
        };
        let (t, b, l) = comps(&rmat, 0.75);
        eprintln!("[probe] RAW @0.75 (current behavior): components={t} (>=3 members:{b}) largest={l}/{n}");
        eprintln!("[probe] CENTERED sweep (components / >=3-member / largest):");
        for thr in [0.20f32, 0.25, 0.30, 0.35, 0.40, 0.45, 0.50] {
            let (t, b, l) = comps(&cmat, thr);
            eprintln!("  thr={thr:.2}: comps={t} (>=3:{b}) largest={l}/{n}");
        }

        // ── Why does the blob survive centering? Either ONE dominant residual
        // direction (→ whitening/PC-removal fixes it) or genuinely dense variance
        // (→ the content is homogeneous, 1 cluster is content-correct). Decide via
        // the top eigenvalues of the centered Gram + connectivity after projecting
        // out the top principal component(s).
        let cvec: Vec<Vec<f32>> = (0..n)
            .map(|i| cm.right.wavefronts.row(i).iter().zip(mean.iter()).map(|(&x, &m)| x - m).collect())
            .collect();
        let mut g = vec![0.0f32; n * n];
        let mut trace = 0.0f32;
        for i in 0..n {
            for j in i..n {
                let d: f32 = cvec[i].iter().zip(cvec[j].iter()).map(|(a, b)| a * b).sum();
                g[idx(i, j)] = d;
                g[j * n + i] = d;
                if i == j { trace += d; }
            }
        }
        let matvec = |m: &[f32], u: &[f32]| -> Vec<f32> {
            (0..n).map(|i| (0..n).map(|j| m[i * n + j] * u[j]).sum()).collect()
        };
        let mut gd = g.clone();
        let mut feat_dirs: Vec<Vec<f32>> = vec![]; // top principal directions in feature space (unit)
        for k in 0..3 {
            let mut u = vec![1.0f32 / (n as f32).sqrt(); n];
            let mut lam = 0.0f32;
            for _ in 0..80 {
                let mut w = matvec(&gd, &u);
                let norm = w.iter().map(|x| x * x).sum::<f32>().sqrt();
                if norm <= 0.0 { break; }
                for x in w.iter_mut() { *x /= norm; }
                lam = norm;
                u = w;
            }
            eprintln!("[probe] PC{}: explains {:.1}% of centered variance", k + 1, lam / trace.max(1e-9) * 100.0);
            let mut v = vec![0.0f32; dim];
            for i in 0..n {
                for (vd, &c) in v.iter_mut().zip(cvec[i].iter()) { *vd += u[i] * c; }
            }
            let vn = v.iter().map(|x| x * x).sum::<f32>().sqrt();
            if vn > 0.0 { for x in v.iter_mut() { *x /= vn; } }
            feat_dirs.push(v);
            for i in 0..n {
                for j in 0..n { gd[i * n + j] -= lam * u[i] * u[j]; }
            }
        }
        let resid_largest = |kdirs: usize, thr: f32| -> usize {
            let res: Vec<Vec<f32>> = (0..n)
                .map(|i| {
                    let mut r = cvec[i].clone();
                    for v in feat_dirs.iter().take(kdirs) {
                        let dot: f32 = r.iter().zip(v.iter()).map(|(a, b)| a * b).sum();
                        for (rx, &vx) in r.iter_mut().zip(v.iter()) { *rx -= dot * vx; }
                    }
                    r
                })
                .collect();
            let cosr = |i: usize, j: usize| -> f32 {
                let (mut d, mut na, mut nb) = (0.0f32, 0.0f32, 0.0f32);
                for (a, b) in res[i].iter().zip(res[j].iter()) { d += a * b; na += a * a; nb += b * b; }
                if na <= 0.0 || nb <= 0.0 { 0.0 } else { d / (na.sqrt() * nb.sqrt()) }
            };
            let mut adj: Vec<Vec<usize>> = vec![vec![]; n];
            for i in 0..n {
                for j in (i + 1)..n { if cosr(i, j) > thr { adj[i].push(j); adj[j].push(i); } }
            }
            let mut seen = vec![false; n];
            let mut largest = 0usize;
            for s in 0..n {
                if seen[s] { continue; }
                let mut st = vec![s];
                seen[s] = true;
                let mut sz = 0usize;
                while let Some(u) = st.pop() {
                    sz += 1;
                    for &v in &adj[u] { if !seen[v] { seen[v] = true; st.push(v); } }
                }
                largest = largest.max(sz);
            }
            largest
        };
        eprintln!(
            "[probe] largest component @residual-cos>0.30: top1-removed={}/{} top3-removed={}/{}",
            resid_largest(1, 0.30), n, resid_largest(3, 0.30), n
        );
    }

    #[test]
    #[ignore = "experiment: run with --ignored --nocapture"]
    fn experiment_exemplar_revives_collapsed_node() {
        // Two-systems / EXEMPLAR coupling: a COLLAPSED node (phase 0, order ~1.0)
        // coupled toward a settled EXEMPLAR (re-phased, structured) adopts the
        // exemplar's belief structure — "two systems reaching the same
        // understanding". Same content here (matches are identity) → a clean
        // convergence demo; overlapping-but-different content is the real
        // scenario (a follow-up).
        let pipeline = test_pipeline();
        let topics: [[&str; 4]; 4] = [
            ["the cat sat on the mat", "a cat napped in the sun", "kittens chase yarn balls", "the feline purred softly"],
            ["the stock market fell sharply", "investors sold their equities", "the bond yield rose today", "the reserve raised interest rates"],
            ["photosynthesis converts light", "chlorophyll absorbs photons", "green plants release oxygen", "leaves capture the sunlight"],
            ["the rocket reached orbit", "the satellite deployed cleanly", "the booster stage separated", "mission control confirmed launch"],
        ];
        let build = |pipeline: &EncodingPipeline| {
            let mut cm = ChiralMedium::new();
            for g in topics.iter() {
                for s in g.iter() {
                    cm.store(s, 0.8, pipeline).unwrap();
                }
            }
            cm
        };
        // Exemplar: a settled, re-phased world model.
        let mut exemplar = build(&pipeline);
        exemplar.rephase_from_content();
        let ex = exemplar.bilateral_ring_report();
        // Node: collapsed (default phase 0 → order ~1.0).
        let mut node = build(&pipeline);
        let n0 = node.bilateral_ring_report();
        // Phase alignment between node & exemplar at shared content (matches are
        // identity here, so compare index-aligned phases).
        let align = |node: &ChiralMedium, exm: &ChiralMedium| -> f32 {
            let n = node.right.count().min(exm.right.count());
            if n == 0 {
                return 0.0;
            }
            let mut s = 0.0f32;
            for i in 0..n {
                s += (node.right.phase[i] - exm.right.phase[i]).cos();
            }
            s / n as f32
        };
        let a0 = align(&node, &exemplar);
        node.couple_toward_exemplar(&exemplar, 40, 0.2);
        let n1 = node.bilateral_ring_report();
        let a1 = align(&node, &exemplar);
        eprintln!(
            "[expExemplar] exemplar order={:.3} | node order {:.3}->{:.3} | node<->exemplar phase-align {:.3}->{:.3}",
            ex.order, n0.order, n1.order, a0, a1
        );
    }

    // ADR-0037 Track-D: node↔node peer-core coupling. A node, fed a PEER's belief
    // cores (fingerprints + phases), should converge its phases toward the matched
    // cores WITHOUT any wavefront exceeding the displacement budget (anti-collapse).
    #[test]
    fn couple_toward_peer_cores_converges_within_budget() {
        let pipeline = test_pipeline();
        let topics: [[&str; 4]; 4] = [
            ["the cat sat on the mat", "a cat napped in the sun", "kittens chase yarn balls", "the feline purred softly"],
            ["the stock market fell sharply", "investors sold their equities", "the bond yield rose today", "the reserve raised interest rates"],
            ["photosynthesis converts light", "chlorophyll absorbs photons", "green plants release oxygen", "leaves capture the sunlight"],
            ["the rocket reached orbit", "the satellite deployed cleanly", "the booster stage separated", "mission control confirmed launch"],
        ];
        let build = |p: &EncodingPipeline| {
            let mut cm = ChiralMedium::new();
            for g in topics.iter() {
                for s in g.iter() {
                    cm.store(s, 0.8, p).unwrap();
                }
            }
            cm
        };
        // Peer: a settled, re-phased world model with belief cores.
        let mut peer = build(&pipeline);
        peer.rephase_from_content();
        let peer_cores = peer.belief_core_snapshot();

        // Node: same content, collapsed (phase 0).
        let mut node = build(&pipeline);
        let n = node.right.count();
        let before: Vec<f32> = (0..n).map(|i| node.right.phase[i]).collect();

        // Mean cos(phase_i − matched_peer_core_phase): alignment toward the targets
        // the coupling actually pulls toward (recomputes the primitive's matching).
        let target_align = |node: &ChiralMedium| -> f32 {
            if peer_cores.is_empty() {
                return 0.0;
            }
            let n = node.right.count();
            let mut s = 0.0f32;
            for i in 0..n {
                let v: Vec<f32> = node.right.wavefronts.row(i).iter().copied().collect();
                let fp = crate::l6::fingerprint(&v, 16);
                if let Some((j, _)) = crate::l6::nearest_core(&fp, &peer_cores, None) {
                    s += (node.right.phase[i] - peer_cores[j].phase).cos();
                }
            }
            s / n as f32
        };

        let max_disp = 1.5f32;
        let a0 = target_align(&node);
        // Wavefront→peer-core matches sit on a LOWER cosine scale than core↔core
        // (shared_cores): a wavefront fp is compared to a core's k-NN-centroid fp, so
        // here median ≈ 0.54, max ≈ 0.77, NONE ≥ 0.85. 0.3 floors out noise-level
        // matches (JL-16 random-cosine std ≈ 1/√16 = 0.25) while keeping genuine ones.
        let moved = node.couple_toward_peer_cores(&peer_cores, 40, 0.2, max_disp, 0.3);
        let a1 = target_align(&node);

        if peer_cores.is_empty() {
            // No cores detected on this synthetic field ⇒ coupling is a no-op.
            assert_eq!(moved, 0);
        } else {
            assert!(moved > 0, "expected some wavefronts to couple toward peer cores");
            assert!(a1 >= a0 - 1e-3, "coupling must not reduce alignment to peer cores ({a0} -> {a1})");
            // Displacement budget: no wavefront moved more than max_disp.
            for i in 0..n {
                let d = (node.right.phase[i] - before[i]).abs();
                assert!(d <= max_disp + 1e-3, "wavefront {i} moved {d} > budget {max_disp}");
            }
            // The min_cos gate bites: a threshold above the match scale (max≈0.77)
            // couples NOBODY (the documented "couples nobody above the scale" invariant,
            // not mere monotonicity).
            let mut strict = build(&pipeline);
            let moved_strict = strict.couple_toward_peer_cores(&peer_cores, 40, 0.2, max_disp, 0.95);
            assert_eq!(
                moved_strict, 0,
                "min_cos=0.95 is above the match scale (max≈0.77) — should couple nobody, got {moved_strict}"
            );
            // peer_match_cosines (the `--dry-run` diagnostic): one cosine per wavefront.
            let diag = strict.peer_match_cosines(&peer_cores);
            assert_eq!(diag.len(), strict.right.count());
            assert!(diag.iter().all(|&c| (-1.01..=1.01).contains(&c)));
        }
        eprintln!("[trackD] peer_cores={} moved={moved} target-align {a0:.3}->{a1:.3}", peer_cores.len());
    }
}

#[cfg(test)]
mod facet_benchmark {
    //! ADR-0049 step 4 — the falsifiable benchmark.
    //!
    //! Runs through `ChiralMedium::recall`, the same path the daemon and CLI
    //! take. The flat readonly mirror is deliberately NOT used: it does not
    //! re-sync from chiral, so a result there would prove nothing about the live
    //! medium. Everything here is built and queried in one process.
    //!
    //! A zero-result recall is a FAILURE, not a pass — an assertion that only
    //! holds because nothing came back is the vacuous-gate trap.

    use super::*;
    use crate::codebook::Codebook;
    use crate::encoding::{EncodingPipeline, SimpleHashEncoder};

    fn pipeline() -> EncodingPipeline {
        EncodingPipeline::new(Box::new(SimpleHashEncoder::new(384, 42)), Codebook::new(384, 10_000, 42))
    }

    /// The compound shape ADR-0049 measured: identity + place + building id +
    /// market + note, all superposed into one wavefront.
    const COMPOUND: &str = "Kannaka Labs sits in the Deal District of the city. \
The building identifier is six three eight on the northern side. \
The market square opens for trading at nine each morning. \
The escrow vault shares the same block as the trading hall.";

    /// Distractors that share vocabulary with the compound's OTHER clauses.
    /// This is what makes the test meaningful: a compound wavefront is the
    /// superposition of every clause, so unrelated-but-overlapping neighbours
    /// pull it away from any single-clause query.
    const DISTRACTORS: &[&str] = &[
        "The trading hall opens for business each morning in the city",
        "The northern side of the city holds the residential buildings",
        "The escrow vault was audited on the same block last season",
        "The market square was resurfaced during the summer works",
        "The building identifier scheme was revised for the whole district",
    ];

    fn seed_distractors(cm: &mut ChiralMedium, p: &EncodingPipeline) {
        for d in DISTRACTORS {
            cm.store(d, 0.8, p).unwrap();
        }
    }

    /// Control: the compound stored whole, undecomposed.
    fn control_medium(p: &EncodingPipeline) -> (ChiralMedium, Uuid) {
        let mut cm = ChiralMedium::new();
        let id = cm.store(COMPOUND, 0.9, p).unwrap();
        seed_distractors(&mut cm, p);
        (cm, id)
    }

    /// Decomposed: parent retained resolve-only, plus one facet per clause.
    fn faceted_medium(p: &EncodingPipeline) -> (ChiralMedium, Uuid, usize) {
        let mut cm = ChiralMedium::new();
        let parent = cm.store(COMPOUND, 0.9, p).unwrap();
        let facets = crate::facet::decompose(COMPOUND);
        assert!(facets.len() >= 3, "fixture must actually decompose: {facets:?}");

        let mut facet_ids = Vec::new();
        for f in &facets {
            facet_ids.push(cm.store(f, 0.9, p).unwrap());
        }
        seed_distractors(&mut cm, p);

        // Mark the constellation. Once `remember` is wired (step 5) this is what
        // the write path will do; here we do it directly so the read path can be
        // benchmarked independently of the writer.
        for m in cm.right.metadata.iter_mut().chain(cm.left.metadata.iter_mut()) {
            if m.id == parent {
                m.decomposed = true;
            } else if facet_ids.contains(&m.id) {
                m.is_facet = true;
                m.parent_id = Some(parent);
            }
        }
        let n = facets.len();
        (cm, parent, n)
    }

    fn rank_of(results: &[ChiralResonance], id: Uuid) -> Option<usize> {
        results.iter().position(|r| r.id == id).map(|i| i + 1)
    }

    #[test]
    fn specific_facet_query_rank_wins_over_the_compound() {
        let p = pipeline();
        let (control, c_id) = control_medium(&p);
        let (faceted, f_id, _) = faceted_medium(&p);

        // A query for ONE clause of the compound.
        let q = "where is Kannaka Labs located";
        let c_res = control.recall(q, 5, &p).unwrap();
        let f_res = faceted.recall(q, 5, &p).unwrap();

        assert!(!c_res.is_empty(), "control returned 0 results — benchmark is vacuous");
        assert!(!f_res.is_empty(), "faceted returned 0 results — benchmark is vacuous");

        let c_rank = rank_of(&c_res, c_id);
        let f_rank = rank_of(&f_res, f_id);
        println!("  compound rank={c_rank:?}  faceted rank={f_rank:?}");

        // The faceted medium must never rank the memory WORSE than the compound
        // one. Δrank in our favour is the win; parity is acceptable on a small
        // fixture; regression is a failure.
        match (c_rank, f_rank) {
            (Some(c), Some(f)) => assert!(f <= c, "faceting made reach worse: {f} vs {c}"),
            (None, Some(_)) => { /* unreachable -> reachable: the ADR's result */ }
            (Some(c), None) => panic!("faceting lost a memory the compound found at rank {c}"),
            (None, None) => panic!("neither medium surfaced the memory — fixture too weak"),
        }
    }

    #[test]
    fn whole_memory_query_still_surfaces_the_parent() {
        // Facets must not fragment holistic recall: a query about the memory as
        // a whole still has to return the parent, with the parent's full text.
        let p = pipeline();
        let (faceted, parent, _) = faceted_medium(&p);
        let res = faceted
            .recall("Kannaka Labs Deal District market escrow vault building", 5, &p)
            .unwrap();

        assert!(!res.is_empty(), "0 results — benchmark is vacuous");
        let hit = res.iter().find(|r| r.id == parent);
        assert!(hit.is_some(), "holistic query lost the parent entirely: {res:#?}");
        assert_eq!(
            hit.unwrap().content,
            COMPOUND,
            "parent surfaced without its full context — resolution did not restore content"
        );
    }

    #[test]
    fn parent_appears_exactly_once_however_many_facets_match() {
        // Parent-dedup. Without it, a query matching several clauses returns the
        // same memory N times and buries everything else.
        let p = pipeline();
        let (faceted, parent, n_facets) = faceted_medium(&p);
        assert!(n_facets >= 3);

        let res = faceted
            .recall("Kannaka Labs building market escrow trading district", 10, &p)
            .unwrap();
        assert!(!res.is_empty(), "0 results — benchmark is vacuous");

        let occurrences = res.iter().filter(|r| r.id == parent).count();
        assert!(
            occurrences <= 1,
            "parent surfaced {occurrences} times from {n_facets} facets — dedup failed"
        );
        // And no raw facet id should ever reach a caller.
        for r in &res {
            let meta = faceted
                .right
                .metadata
                .iter()
                .chain(faceted.left.metadata.iter())
                .find(|m| m.id == r.id);
            if let Some(m) = meta {
                assert!(!m.is_facet, "a raw facet leaked to the caller: {:?}", r.content);
            }
        }
    }

    #[test]
    fn observation_list_injects_into_the_parent_at_most_once() {
        // ADR-0049 step 4, assertion 3 — the mutating-path energy property.
        //
        // ChiralMedium has no observe method of its own: `hrm_store::resonate_query`
        // builds the observation list by iterating exactly what `chiral.recall`
        // returns, then calls `observe_wavefronts`. So the property to assert is
        // that the RETURNED list carries a parent at most once — that is what
        // bounds the injections. Reconstruct that list the way hrm_store does.
        let p = pipeline();
        let (faceted, parent, n_facets) = faceted_medium(&p);

        for _ in 0..5 {
            let results = faceted
                .recall("Kannaka Labs building market escrow trading district", 5, &p)
                .unwrap();
            assert!(!results.is_empty(), "0 results — assertion would be vacuous");

            // Mirror of hrm_store.rs: one (index, intensity) per RESULT.
            let observation_targets: Vec<Uuid> = results.iter().map(|r| r.id).collect();
            let parent_injections =
                observation_targets.iter().filter(|id| **id == parent).count();
            assert!(
                parent_injections <= 1,
                "parent would take {parent_injections} injections in ONE recall from                  {n_facets} facets — this is the ADR-0048 rich-get-richer bias returning"
            );
        }
    }

    #[test]
    fn unfaceted_medium_is_byte_identical_to_pre_facet_behaviour() {
        // The guard that makes shipping steps 1-3 before the backfill safe.
        let p = pipeline();
        let (control, _) = control_medium(&p);
        assert!(!control.has_facets(), "control must hold no facets");
        let a = control.recall("trading hall morning city", 5, &p).unwrap();
        let b = control.recall("trading hall morning city", 5, &p).unwrap();
        assert!(!a.is_empty(), "0 results — benchmark is vacuous");
        let ids_a: Vec<Uuid> = a.iter().map(|r| r.id).collect();
        let ids_b: Vec<Uuid> = b.iter().map(|r| r.id).collect();
        assert_eq!(ids_a, ids_b, "recall is not deterministic on an unfaceted medium");
    }
}

#[cfg(test)]
mod facet_write_path {
    //! ADR-0049 step 5 — write path and backfill.
    //!
    //! Env vars are process-global, so these tests must not run concurrently
    //! with each other. They share one `#[test]` for that reason rather than
    //! relying on test-runner ordering.

    use super::*;
    use crate::codebook::Codebook;
    use crate::encoding::{EncodingPipeline, SimpleHashEncoder};

    fn pipeline() -> EncodingPipeline {
        EncodingPipeline::new(
            Box::new(SimpleHashEncoder::new(384, 42)),
            Codebook::new(384, 10_000, 42),
        )
    }

    const COMPOUND: &str = "Kannaka Labs sits in the Deal District of the city. \
The building identifier is six three eight on the northern side. \
The market square opens for trading at nine each morning.";

    fn facet_count(cm: &ChiralMedium) -> usize {
        cm.right.metadata.iter().filter(|m| m.is_facet).count()
    }

    /// #699 (frozen-corpus eval find): sweeping BOTH hemispheres' ids into
    /// backfill used to decompose each memory twice — the left pass minted a
    /// duplicate facet set stamped with the LEFT-LOCAL parent id, which no
    /// canonical consumer can resolve (openclaw silently dropped those recall
    /// rows, shortening k=20 lists to 10-11). Canonicalized backfill must
    /// treat the left twin as already decomposed, and every minted parent_id
    /// must be a live right id.
    #[test]
    fn backfill_canonicalizes_left_ids_and_never_double_mints() {
        let _flag = crate::facet::lock_decompose_flag();
        let p = pipeline();
        let mut cm = ChiralMedium::new();
        let parent = cm.store(COMPOUND, 0.9, &p).unwrap();

        let n = cm.backfill_facets(parent, &p).unwrap();
        assert!(n >= 2, "compound content must decompose");
        let after_first = cm.right.metadata.len();

        // The left twin's local id must hit the canonical watermark, not
        // mint a second facet set.
        let left_twin = cm.right_to_left.get(&parent).copied();
        if let Some(left_id) = left_twin {
            let n2 = cm.backfill_facets(left_id, &p).unwrap();
            assert_eq!(n2, 0, "left-twin backfill must dedupe via the canonical watermark");
            assert_eq!(cm.right.metadata.len(), after_first, "no duplicate facet rows");
        }

        // Every facet's parent_id must resolve in the RIGHT hemisphere.
        for m in cm.right.metadata.iter().filter(|m| m.is_facet) {
            let pid = m.parent_id.expect("facet carries parent_id");
            assert!(
                cm.right.id_to_index.contains_key(&pid),
                "facet {} parent {} must be a live canonical id",
                m.id,
                pid
            );
        }
    }

    /// #699: legacy stores already contain facets whose parent_id is a
    /// left-local id. parent_of_facet must canonicalize through
    /// left_to_right — or surface the facet itself — but NEVER emit a
    /// hemisphere-local id downstream.
    #[test]
    fn parent_of_facet_never_emits_left_local_ids() {
        let p = pipeline();
        let mut cm = ChiralMedium::new();
        let parent = cm.store("the canonical parent memory row here", 0.9, &p).unwrap();
        let facet = cm.store("a facet fragment with enough words here", 0.9, &p).unwrap();

        if let Some(left_parent) = cm.right_to_left.get(&parent).copied() {
            // Simulate the legacy defect: facet linked to the LEFT parent id.
            for m in cm.right.metadata.iter_mut() {
                if m.id == facet {
                    m.is_facet = true;
                    m.parent_id = Some(left_parent);
                }
            }
            let resolved = cm.parent_of_facet(facet).expect("canonicalizes");
            assert_eq!(
                resolved.0, parent,
                "left-local parent id must canonicalize to the right id"
            );
        }

        // Orphaned parent (no such row anywhere): facet surfaces itself.
        let ghost = uuid::Uuid::new_v4();
        for m in cm.right.metadata.iter_mut() {
            if m.id == facet {
                m.parent_id = Some(ghost);
            }
        }
        assert!(
            cm.parent_of_facet(facet).is_none(),
            "unresolvable parent must surface the facet, not fabricate an id"
        );
    }

    #[test]
    fn write_path_flag_default_off_then_on_then_idempotent_backfill() {
        // KANNAKA_FACET_DECOMPOSE is process-global; hold the lock so a
        // concurrent flag test cannot flip it mid-assertion.
        let _flag = crate::facet::lock_decompose_flag();
        let p = pipeline();

        // ── flag OFF (the default): storing a compound mints nothing extra ──
        std::env::remove_var("KANNAKA_FACET_DECOMPOSE");
        let mut off = ChiralMedium::new();
        let before = off.right.metadata.len();
        let id_off = off.store_with_facets(COMPOUND, 0.9, &p, None).unwrap();
        assert_eq!(
            off.right.metadata.len() - before,
            1,
            "flag off must store exactly one wavefront"
        );
        assert_eq!(facet_count(&off), 0, "flag off minted facets");
        assert!(!off.is_decomposed(id_off), "flag off marked a parent decomposed");
        assert!(!off.has_facets(), "flag off left the medium claiming facets");

        // ── flag ON: the same content mints a linked constellation ──
        std::env::set_var("KANNAKA_FACET_DECOMPOSE", "1");
        let mut on = ChiralMedium::new();
        let parent = on.store_with_facets(COMPOUND, 0.9, &p, None).unwrap();
        let minted = facet_count(&on);
        assert!(minted >= 2, "flag on minted {minted} facets");
        assert!(on.is_decomposed(parent), "parent not marked decomposed");
        assert!(on.has_facets());
        // Every facet points at this parent, and no facet is itself decomposed.
        for m in on.right.metadata.iter().filter(|m| m.is_facet) {
            assert_eq!(m.parent_id, Some(parent), "facet linked to the wrong parent");
            assert!(!m.decomposed, "a facet was itself marked decomposed");
        }
        // The parent keeps its full content — retention is an ADR invariant.
        let pmeta = on.right.metadata.iter().find(|m| m.id == parent).unwrap();
        assert_eq!(pmeta.content, COMPOUND, "parent content was mutated");

        // ── backfill is once-only: decompose-twice == decompose-once ──
        let mut bf = ChiralMedium::new();
        std::env::remove_var("KANNAKA_FACET_DECOMPOSE"); // backfill ignores the write flag
        let target = bf.store_with_facets(COMPOUND, 0.9, &p, None).unwrap();
        assert_eq!(facet_count(&bf), 0, "setup should be undecomposed");

        let first = bf.backfill_facets(target, &p).unwrap();
        assert!(first >= 2, "backfill minted {first} facets");
        let after_first = bf.right.metadata.len();
        let facets_after_first = facet_count(&bf);

        let second = bf.backfill_facets(target, &p).unwrap();
        assert_eq!(second, 0, "backfill was not once-only — it re-minted {second} facets");
        assert_eq!(
            bf.right.metadata.len(),
            after_first,
            "a second backfill pass added wavefronts"
        );
        assert_eq!(facet_count(&bf), facets_after_first, "facet count drifted on re-run");

        // ── a facet is never itself decomposed ──
        let a_facet = bf
            .right
            .metadata
            .iter()
            .find(|m| m.is_facet)
            .map(|m| m.id)
            .unwrap();
        assert_eq!(
            bf.backfill_facets(a_facet, &p).unwrap(),
            0,
            "backfill decomposed a facet — facets of facets would fragment forever"
        );

        std::env::remove_var("KANNAKA_FACET_DECOMPOSE");
    }

    #[test]
    fn single_clause_content_is_never_decomposed_even_with_the_flag_on() {
        let _flag = crate::facet::lock_decompose_flag();
        let p = pipeline();
        std::env::set_var("KANNAKA_FACET_DECOMPOSE", "1");
        let mut cm = ChiralMedium::new();
        let id = cm
            .store_with_facets("Kannaka Labs sits in the Deal District", 0.9, &p, None)
            .unwrap();
        assert_eq!(facet_count(&cm), 0, "an atomic memory was decomposed");
        assert!(!cm.is_decomposed(id));
        std::env::remove_var("KANNAKA_FACET_DECOMPOSE");
    }
}
