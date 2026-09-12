//! Storage layer: MediumBackend trait, TestMedium, and ResonanceEngine.

use std::collections::HashMap;

use chrono::Utc;
use thiserror::Error;
use uuid::Uuid;

use crate::encoding::{EncodingError, EncodingPipeline};
use crate::memory::HyperMemory;
use crate::wave::cosine_similarity;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("memory not found: {0}")]
    NotFound(Uuid),
    #[error("duplicate id: {0}")]
    DuplicateId(Uuid),
    #[error("store error: {0}")]
    Other(String),
}

#[derive(Debug, Error)]
pub enum EngineError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Encoding(#[from] EncodingError),
}

// ---------------------------------------------------------------------------
// QueryResult
// ---------------------------------------------------------------------------

/// Rich search result with wave-modulated scoring.
#[derive(Debug, Clone)]
pub struct QueryResult {
    pub id: Uuid,
    pub similarity: f32,
    pub effective_strength: f32,
    pub combined_score: f32,
}

// ---------------------------------------------------------------------------
// MediumBackend trait
// ---------------------------------------------------------------------------

/// Storage backend for holographic resonance memories.
///
/// The canonical implementation is HrmStore (Chiral Holographic Resonance Medium).
/// HRM is the substrate — storing changes the interference field, recall is
/// resonance, consciousness metrics emerge from tensor topology, and associations
/// are emergent from phase coherence.
///
/// TestMedium exists only for testing.
///
/// ## HRM-first semantics
///
/// - `store()` / `recall()` — encode and store/recall via the holographic medium
/// - `consciousness_metrics()` — eigendecomposition Phi, spectral Xi, Kuramoto order
/// - `relate()` — create resonance-based association (interference, not a link table)
/// - `flush()` — persist the holographic tensor to disk
/// - `dream()` — anneal the medium (right hemisphere only in chiral mode)
///
/// The `insert()` / `search()` methods accept raw HyperMemory/vectors for
/// compatibility with ResonanceEngine. New code should prefer `store()`/`recall()`.
pub trait MediumBackend: Send + Sync {
    // -- Core HRM operations --

    /// Absorb content into the holographic medium as a new wavefront.
    /// Encodes text → SGA classification → Fano fold routing → interference.
    /// The medium is permanently changed by the absorption.
    fn absorb(&mut self, content: &str, importance: f32, category: Option<&str>) -> Result<Uuid, StoreError> {
        let _ = (content, importance, category);
        Err(StoreError::Other("absorb not implemented".into()))
    }

    /// Backward-compatible alias for absorb().
    fn store_text(&mut self, content: &str, importance: f32, category: Option<&str>) -> Result<Uuid, StoreError> {
        self.absorb(content, importance, category)
    }

    /// Resonate a query through the holographic medium.
    /// Recall IS observation — attention reshapes the field.
    /// The medium is permanently changed by the resonance.
    fn resonate_query(&mut self, query: &str, top_k: usize) -> Result<Vec<(Uuid, f32)>, StoreError> {
        let _ = (query, top_k);
        Err(StoreError::Other("resonate_query not implemented".into()))
    }

    /// Backward-compatible alias for resonate_query().
    fn recall_text(&mut self, query: &str, top_k: usize) -> Result<Vec<(Uuid, f32)>, StoreError> {
        self.resonate_query(query, top_k)
    }

    /// Read-only resonance recall — resonance scoring WITHOUT the observation
    /// write-back, so it never mutates the field. Unlike [`resonate_query`], a
    /// caller can run many of these without amplifying amplitudes or marking the
    /// store dirty. Used by the recall-scenario benchmark export (#476). Default
    /// errors; the HRM backend overrides it.
    ///
    /// [`resonate_query`]: MediumBackend::resonate_query
    fn recall_resonance_readonly(
        &self,
        query: &str,
        top_k: usize,
    ) -> Result<Vec<crate::medium::Resonance>, StoreError> {
        let _ = (query, top_k);
        Err(StoreError::Other(
            "recall_resonance_readonly not supported by this backend".into(),
        ))
    }

    /// Sparse-attention recall — score only the memories in `beam`.
    ///
    /// Default impl falls back to full `resonate_query` so backends that
    /// haven't been beam-aware yet keep working. The HRM backend overrides
    /// this to route through `Medium::recall_against_ids` for O(K) scoring.
    fn resonate_query_with_beam(
        &mut self,
        _beam: &[Uuid],
        query: &str,
        top_k: usize,
    ) -> Result<Vec<(Uuid, f32)>, StoreError> {
        // Conservative fallback: a backend that doesn't know about beams
        // shouldn't silently drop the call. Run full recall instead.
        self.resonate_query(query, top_k)
    }

    /// Consciousness metrics from the holographic medium topology.
    /// Eigendecomposition Phi, spectral Xi, Kuramoto order parameter.
    fn consciousness_metrics(&self) -> crate::consciousness::ConsciousnessMetrics {
        crate::consciousness::ConsciousnessMetrics {
            phi: 0.0, xi: 0.0, order: 0.0, num_clusters: 0, irrationality: 0.0,
            level: crate::consciousness::ConsciousnessLevel::Dormant,
            computed_at: chrono::Utc::now(),
            total_skip_links: 0,
        }
    }

    /// Stale-tolerant cached metrics. Returns whatever was last persisted
    /// to disk/in-process, ignoring fingerprint drift. Returns None only
    /// if no metrics have ever been computed. Default = None.
    fn try_cached_consciousness_metrics(&self) -> Option<crate::consciousness::ConsciousnessMetrics> {
        None
    }

    /// Update cached total_skip_links. bridge::assess computes the real
    /// per-memory connection count from outside the medium's tensor view;
    /// this lets it persist that count back into the sidecar so the next
    /// hot-path read sees the accurate value. Default no-op for backends
    /// that don't maintain a metrics cache.
    fn set_cached_total_skip_links(&self, _n: usize) {}

    /// Update cached num_clusters. bridge::assess runs the canonical
    /// Kuramoto-BFS algorithm and writes the result back here so the
    /// cached ConsciousnessMetrics.num_clusters matches what observe()
    /// and status() report (refactor #2 — single source of truth for
    /// cluster count). Default no-op for backends without a cache.
    fn set_cached_num_clusters(&self, _n: usize) {}

    /// Create a resonance-based association between two memories.
    /// Creates an associative wavefront from interference of the two sources.
    fn relate(&mut self, _id_a: &Uuid, _id_b: &Uuid) -> Result<Uuid, StoreError> {
        Err(StoreError::Other("relate not supported".into()))
    }

    /// Persist the holographic tensor to disk.
    fn flush(&mut self) -> Result<usize, StoreError> { Ok(0) }

    // -- Low-level access (internal / read-only) --
    /// On-disk path of the HRM file backing this store, if any. Default
    /// returns `None` for in-memory backends. Used by the cluster sidecar
    /// cache to place `.clusters.json` next to the HRM.
    fn hrm_path(&self) -> Option<&std::path::Path> { None }

    fn get(&self, id: &Uuid) -> Result<Option<&HyperMemory>, StoreError>;
    fn get_mut(&mut self, id: &Uuid) -> Result<Option<&mut HyperMemory>, StoreError>;

    /// Ids that carry ADR-0049 facet structure: a minted facet row, or a
    /// parent that has already been decomposed into facets.
    ///
    /// Maintenance tools must never delete either. Deleting a decomposed
    /// parent dangles every facet that points at it ("parent retention is an
    /// invariant" — `WavefrontMeta::decomposed`), and deleting a facet drops
    /// an atom recall depends on. Backends with no facet structure return the
    /// empty set, which is why the default is `HashSet::new()` rather than an
    /// error: absence of facets is a correct answer, not an unsupported one.
    fn facet_structured_ids(&self) -> std::collections::HashSet<Uuid> {
        std::collections::HashSet::new()
    }
    fn all_memories(&self) -> Result<Vec<&HyperMemory>, StoreError>;
    fn all_ids(&self) -> Result<Vec<Uuid>, StoreError>;
    fn delete(&mut self, id: &Uuid) -> Result<bool, StoreError>;
    fn count(&self) -> usize;

    // -- Legacy Compatibility (quarantined) -----------------------------------------------
    // These methods exist only for ResonanceEngine backward compat.
    // New code MUST use absorb()/resonate_query()/relate()/dream_native()/flush().
    //
    // insert() bypasses HRM encoding/classification/routing.
    // search() bypasses observation effects (attention does not reshape the field).
    //
    // DEPRECATED: Use absorb() instead of insert(). Use resonate_query() instead of search().
    // These will be removed once all internal consumers (dream consolidation, paradox engine,
    // hallucination pipeline) migrate to HRM-native paths.
    // -------------------------------------------------------------------------------------

    /// **DEPRECATED** -- Use `absorb()` instead.
    /// Raw insert bypasses HRM encoding, SGA classification, and Fano routing.
    /// Kept only for ResonanceEngine compat fallback and hallucination pipeline.
    fn insert(&mut self, memory: HyperMemory) -> Result<Uuid, StoreError>;

    /// **DEPRECATED** -- Use `resonate_query()` instead.
    /// Raw similarity search with NO observation effects.
    /// Kept for dream consolidation neighbor-finding and paradox engine
    /// where raw similarity is needed without deforming the field.
    fn search(&self, query: &[f32], top_k: usize) -> Result<Vec<(Uuid, f32)>, StoreError>;

    /// Wave-native dream using Medium's eigenstructure annealing.
    /// Only supported by HrmStore. Default returns an error for other backends.
    fn dream_native(
        &mut self,
        _cycles: usize,
        _temperature: Option<f32>,
        _chiral_eta: f32,
    ) -> Result<crate::medium::types::DreamReport, StoreError> {
        Err(StoreError::Other("dream_native not supported by this backend".into()))
    }

    /// Run callosal Kuramoto coupling step to sync hemispheres.
    /// Only meaningful for HrmStore with a chiral medium. Default is a no-op.
    fn callosal_kuramoto(&mut self, _dt: f32) {}

    /// Perform a chiral dream pass (right hemisphere only for deep=true).
    /// Only meaningful for HrmStore with a chiral medium. Default is a no-op.
    fn chiral_dream(&mut self, _deep: bool, _cycles: usize) {}

    /// Whether this backend is running the chiral (two-hemisphere) medium.
    /// Default false; HrmStore reports its actual mode. Lets read-only callers
    /// (e.g. the recall-scenario export) label which store a recall ran against.
    fn is_chiral(&self) -> bool {
        false
    }

    /// Quantum-Wave T1.4 (#474): provenance of the entropy that last seeded a
    /// dream/Ξ touching this wavefront, read from the canonical
    /// `WavefrontMeta.provenance`. `None` ⇒ never stamped ⇒ `prng://legacy`.
    /// Default `None`; HrmStore reads the authoritative metadata. Surfaced via
    /// CLI inspect (`export-json`).
    fn provenance_of(&self, _id: &Uuid) -> Option<crate::entropy::Provenance> {
        None
    }

    /// ADR-0036 Phase 1: flush per-memory reactivation counts to the sidecar.
    /// Default no-op; HrmStore writes `.reactivation.json` (sidecar only, safe
    /// under readonly). Called by the serve daemon and CLI `recall` so recall
    /// reactivation survives even though those processes never persist the .hrm.
    fn flush_reactivation(&self) {}

    /// ADR-0036: plan (and, from Phase 2, apply) resonance-merge consolidation.
    /// Default is a no-op returning an "off" report; only HrmStore implements it.
    /// Takes `&mut self` to accommodate the future apply path; Phase 0 never mutates.
    fn consolidate_resonance(
        &mut self,
        _opts: &crate::medium::types::ConsolidateOpts,
    ) -> crate::medium::types::ConsolidateReport {
        crate::medium::types::ConsolidateReport {
            mode: "off".to_string(),
            ..Default::default()
        }
    }

    /// Downcasting support.
    fn as_any(&self) -> &dyn std::any::Any;
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;
}

// ---------------------------------------------------------------------------
// TestMedium
// ---------------------------------------------------------------------------

/// HashMap-backed reference implementation with brute-force cosine similarity.
pub struct TestMedium {
    memories: HashMap<Uuid, HyperMemory>,
}

impl TestMedium {
    pub fn new() -> Self {
        Self {
            memories: HashMap::new(),
        }
    }
}

impl Default for TestMedium {
    fn default() -> Self {
        Self::new()
    }
}

impl MediumBackend for TestMedium {
    fn insert(&mut self, memory: HyperMemory) -> Result<Uuid, StoreError> {
        let id = memory.id;
        if self.memories.contains_key(&id) {
            return Err(StoreError::DuplicateId(id));
        }
        self.memories.insert(id, memory);
        Ok(id)
    }

    fn get(&self, id: &Uuid) -> Result<Option<&HyperMemory>, StoreError> {
        Ok(self.memories.get(id))
    }

    fn get_mut(&mut self, id: &Uuid) -> Result<Option<&mut HyperMemory>, StoreError> {
        Ok(self.memories.get_mut(id))
    }

    fn search(&self, query: &[f32], top_k: usize) -> Result<Vec<(Uuid, f32)>, StoreError> {
        let mut scored: Vec<(Uuid, f32)> = self
            .memories
            .values()
            .map(|m| (m.id, cosine_similarity(query, &m.vector)))
            .collect();
        scored.sort_by(|a, b| b.1.total_cmp(&a.1));
        scored.truncate(top_k);
        Ok(scored)
    }

    fn all_memories(&self) -> Result<Vec<&HyperMemory>, StoreError> {
        let mut mems: Vec<&HyperMemory> = self.memories.values().collect();
        mems.sort_by_key(|m| m.id);
        Ok(mems)
    }

    fn all_ids(&self) -> Result<Vec<Uuid>, StoreError> {
        let mut ids: Vec<Uuid> = self.memories.keys().copied().collect();
        ids.sort();
        Ok(ids)
    }

    fn delete(&mut self, id: &Uuid) -> Result<bool, StoreError> {
        Ok(self.memories.remove(id).is_some())
    }

    fn count(&self) -> usize {
        self.memories.len()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

// ---------------------------------------------------------------------------
// ResonanceEngine
// ---------------------------------------------------------------------------

/// Minimum link strength for traversal during query expansion.
#[allow(dead_code)]
const MIN_LINK_STRENGTH: f32 = 0.1;

/// φ (golden ratio) for span scoring.
const PHI: f64 = 1.618033988749895;

/// Score a temporal span based on proximity to golden ratio sequence values.
/// Returns higher scores for spans near φ^k: 2, 3, 4, 7, 11, 18, 29...
pub fn phi_span_score(span: u8) -> f32 {
    if span == 0 {
        return 0.0;
    }
    let s = span as f64;
    let mut best = f64::MAX;
    // Check φ^k for k=1..8 (covers spans up to ~47)
    for k in 1..=8 {
        let phi_k = PHI.powi(k);
        let dist = (s - phi_k).abs() / phi_k; // relative distance
        if dist < best {
            best = dist;
        }
    }
    // Convert: closer → higher score, max 1.0
    (1.0 - best.min(1.0)) as f32
}

/// High-level API over the holographic resonance medium.
///
/// Core paths assume HRM. The pipeline is:
/// - remember() → store.absorb() → ChiralMedium (encode + classify + fold + absorb)
/// - recall()   → store.resonate_query() → ChiralMedium (bilateral resonance + observation)
///
/// The pipeline field is kept for compatibility callers that need raw encoding.
pub struct ResonanceEngine {
    pub store: Box<dyn MediumBackend>,
    pub(crate) pipeline: EncodingPipeline,
    /// Legacy — kept for callers that check it. Not used by core paths.
    pub similarity_threshold: f32,
}

impl ResonanceEngine {
    /// ADR-0037 Phase 3: aggregate π/φ bridge-operator signature for the
    /// substrate beacon. `None` when the backend isn't an HRM.
    pub fn xi_bridge_summary(&self) -> Option<serde_json::Value> {
        self.store
            .as_any()
            .downcast_ref::<crate::hrm_store::HrmStore>()
            .map(|hrm| hrm.xi_bridge_summary())
    }

    /// #836 / ADR-0049: decompose every existing compound memory into facets —
    /// the corpus migration for stores written before facet decomposition
    /// existed. Dry run unless `apply`. `None` when the backend isn't an HRM.
    pub fn backfill_all_facets(
        &mut self,
        apply: bool,
    ) -> Option<crate::hrm_store::FacetBackfillStats> {
        self.store
            .as_any_mut()
            .downcast_mut::<crate::hrm_store::HrmStore>()
            .map(|hrm| hrm.backfill_all_facets(apply))
    }

    pub fn new(store: Box<dyn MediumBackend>, pipeline: EncodingPipeline) -> Self {
        Self {
            store,
            pipeline,
            similarity_threshold: 0.7,
        }
    }

    /// Absorb text into the holographic medium as a new wavefront.
    /// The medium handles encoding, SGA classification, Fano routing, and chiral absorption.
    pub fn remember(&mut self, text: &str) -> Result<Uuid, EngineError> {
        self.store.absorb(text, 0.5, None)
            .or_else(|_| {
                // Compat fallback for test stores
                let memory = self.pipeline.encode_memory(text, Utc::now())?;
                Ok(self.store.insert(memory)?)
            })
    }

    /// Store with explicit layer depth (legacy concept — HRM ignores it).
    pub fn remember_at_layer(&mut self, text: &str, _layer_depth: u8) -> Result<Uuid, EngineError> {
        self.remember(text)
    }

    /// Resonate a query through the medium. Bilateral search + intuition surfacing + observation.
    pub fn recall(&mut self, query: &str, top_k: usize) -> Result<Vec<QueryResult>, EngineError> {
        let results = self.store.resonate_query(query, top_k)
            .or_else(|_| {
                // Compat fallback for test stores
                let qvec = self.pipeline.encode_text(query)?;
                self.store.search(&qvec, top_k)
                    .map_err(|e| EncodingError::Other(e.to_string()))
            })?;

        let qr: Vec<QueryResult> = results.into_iter().map(|(id, score)| {
            QueryResult { id, similarity: score, effective_strength: score, combined_score: score }
        }).collect();

        // Record retrieval events (f(x) term — recalled memories gain energy)
        for r in &qr {
            if let Ok(Some(mem)) = self.store.get_mut(&r.id) {
                mem.record_retrieval();
            }
        }

        Ok(qr)
    }

    /// Alias for recall() — expansion is now inherent in bilateral resonance.
    pub fn recall_with_expansion(&mut self, query: &str, top_k: usize) -> Result<Vec<QueryResult>, EngineError> {
        self.recall(query, top_k)
    }

    /// No-op — associations are emergent from interference in the holographic medium.
    pub fn decay_links(&mut self, _decay_factor: f32) {}

    /// No-op — reinforcement is emergent from constructive interference.
    pub fn reinforce_link(&mut self, _memory_id: &Uuid, _target_id: &Uuid, _boost: f32) {}

    /// Get a memory by id.
    pub fn get_memory(&self, id: &Uuid) -> Result<Option<&HyperMemory>, EngineError> {
        Ok(self.store.get(id)?)
    }

    pub fn get_memory_mut(&mut self, id: &Uuid) -> Result<Option<&mut HyperMemory>, EngineError> {
        Ok(self.store.get_mut(id)?)
    }

    pub fn delete(&mut self, id: &Uuid) -> Result<bool, EngineError> {
        Ok(self.store.delete(id)?)
    }

    /// ADR-0012: Create an immutable snapshot of all memories for parallel dreaming.
    /// 
    /// Returns an Arc-wrapped frozen state that can be shared across threads without locks.
    /// This is the "reference frame" for the holographic paradox engine.
    pub fn snapshot(&self) -> crate::paradox::ParadoxSnapshot {
        let all_memories = self.store.all_memories().unwrap_or_default();
        let memory_map: std::collections::HashMap<Uuid, crate::memory::HyperMemory> = all_memories
            .into_iter()
            .map(|mem| (mem.id, mem.clone()))
            .collect();
        
        crate::paradox::ParadoxSnapshot {
            memories: std::sync::Arc::new(memory_map),
            timestamp: Utc::now(),
        }
    }

    /// Phase 8 (ADR-0011): Return Xi-based memory clusters for partitioned dreaming.
    ///
    /// Groups memories by frequency-category (experience / emotion / social / skill / knowledge),
    /// matching the category bands already used in consolidation's SYNC stage.  Each group
    /// becomes an independent dream partition.  An "other" catch-all bucket holds any memory
    /// that doesn't match the standard bands.
    ///
    /// Returns `Vec<MemoryCluster>` (empty if the medium is empty).
    pub fn xi_clusters(&self) -> Vec<crate::kuramoto::MemoryCluster> {
        use std::collections::HashMap;
        use crate::kuramoto::MemoryCluster;

        let all = match self.store.all_memories() {
            Ok(v) => v,
            Err(_) => return Vec::new(),
        };

        if all.is_empty() {
            return Vec::new();
        }

        let mut buckets: HashMap<&'static str, Vec<Uuid>> = HashMap::new();

        for mem in &all {
            let cat = match mem.frequency {
                f if (1.8..=2.4).contains(&f) => "experience",
                f if (1.3..1.8).contains(&f)  => "emotion",
                f if (1.0..1.3).contains(&f)  => "social",
                f if (0.8..1.0).contains(&f)  => "skill",
                f if (0.0..0.8).contains(&f)  => "knowledge",
                _                         => "other",
            };
            buckets.entry(cat).or_default().push(mem.id);
        }

        buckets
            .into_iter()
            .filter(|(_, ids)| !ids.is_empty())
            .map(|(_, ids)| {
                let phases: Vec<f32> = ids.iter()
                    .filter_map(|id| self.store.get(id).ok().flatten())
                    .map(|m| m.phase)
                    .collect();
                let n = phases.len() as f32;
                let (mean_phase, order) = if n > 0.0 {
                    let sc: f32 = phases.iter().map(|p| p.cos()).sum::<f32>() / n;
                    let ss: f32 = phases.iter().map(|p| p.sin()).sum::<f32>() / n;
                    (ss.atan2(sc), (sc * sc + ss * ss).sqrt())
                } else {
                    (0.0, 0.0)
                };
                MemoryCluster {
                    memory_ids: ids,
                    order_parameter: order,
                    mean_phase,
                    coherence: order,
                    theme_vector: Vec::new(),
                }
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codebook::Codebook;
    use crate::encoding::SimpleHashEncoder;
    use crate::wave::normalize;
    use chrono::Duration;

    fn make_pipeline() -> EncodingPipeline {
        let encoder = SimpleHashEncoder::new(384, 42);
        let codebook = Codebook::new(384, 10_000, 42);
        EncodingPipeline::new(Box::new(encoder), codebook)
    }

    fn make_memory(vector: Vec<f32>, content: &str) -> HyperMemory {
        HyperMemory::new(vector, content.to_string())
    }

    fn unit_vec(dim: usize, index: usize) -> Vec<f32> {
        let mut v = vec![0.0; dim];
        v[index] = 1.0;
        v
    }

    // -- TestMedium tests --

    #[test]
    fn store_insert_get_count() {
        let mut store = TestMedium::new();
        assert_eq!(store.count(), 0);
        let mem = make_memory(vec![1.0; 10], "hello");
        let id = store.insert(mem).unwrap();
        assert_eq!(store.count(), 1);
        let got = store.get(&id).unwrap().unwrap();
        assert_eq!(got.content, "hello");
    }

    #[test]
    fn store_delete() {
        let mut store = TestMedium::new();
        let mem = make_memory(vec![1.0; 10], "bye");
        let id = store.insert(mem).unwrap();
        assert!(store.delete(&id).unwrap());
        assert_eq!(store.count(), 0);
        assert!(!store.delete(&id).unwrap());
    }

    #[test]
    fn store_duplicate_id_rejected() {
        let mut store = TestMedium::new();
        let mem = make_memory(vec![1.0; 10], "a");
        let id = mem.id;
        store.insert(mem).unwrap();
        let mut mem2 = make_memory(vec![2.0; 10], "b");
        mem2.id = id;
        assert!(matches!(store.insert(mem2), Err(StoreError::DuplicateId(_))));
    }

    #[test]
    fn search_returns_closest_first() {
        let mut store = TestMedium::new();
        let mut v1 = unit_vec(100, 0);
        let mut v2 = unit_vec(100, 1);
        let mut v3 = unit_vec(100, 2);
        normalize(&mut v1);
        normalize(&mut v2);
        normalize(&mut v3);
        let m1 = make_memory(v1.clone(), "v1");
        let m2 = make_memory(v2, "v2");
        let m3 = make_memory(v3, "v3");
        let id1 = store.insert(m1).unwrap();
        store.insert(m2).unwrap();
        store.insert(m3).unwrap();

        let results = store.search(&v1, 3).unwrap();
        assert_eq!(results[0].0, id1);
        assert!((results[0].1 - 1.0).abs() < 1e-5);
    }

    // -- ResonanceEngine tests --

    #[test]
    fn engine_remember_recall_roundtrip() {
        let store = TestMedium::new();
        let pipeline = make_pipeline();
        let mut engine = ResonanceEngine::new(Box::new(store), pipeline);

        let id = engine.remember("the cat sat on the mat").unwrap();
        let results = engine.recall("cat on mat", 5).unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].id, id);
        assert!(results[0].combined_score > 0.0);
    }

    #[test]
    fn engine_recall_ranks_relevant_higher() {
        let store = TestMedium::new();
        let pipeline = make_pipeline();
        let mut engine = ResonanceEngine::new(Box::new(store), pipeline);

        let id_cat = engine.remember("the cat sat on the mat").unwrap();
        engine.remember("quantum physics and string theory").unwrap();
        engine.remember("dogs playing in the park").unwrap();

        // Use raw similarity (not wave-modulated) to avoid timing flakiness
        let qvec = engine.pipeline.encode_text("the cat sat on the mat").unwrap();
        let results = engine.store.search(&qvec, 3).unwrap();
        assert_eq!(results[0].0, id_cat, "exact text match should be top result by raw similarity");
        assert!((results[0].1 - 1.0).abs() < 1e-4, "exact match should have sim ~1.0");
    }

    #[test]
    fn engine_get_memory() {
        let store = TestMedium::new();
        let pipeline = make_pipeline();
        let mut engine = ResonanceEngine::new(Box::new(store), pipeline);

        let id = engine.remember("test memory").unwrap();
        let mem = engine.get_memory(&id).unwrap().unwrap();
        assert_eq!(mem.content, "test memory");

        let fake = Uuid::new_v4();
        assert!(engine.get_memory(&fake).unwrap().is_none());
    }

    #[test]
    fn engine_xi_bridge_summary_some_for_hrm_none_otherwise() {
        // ADR-0037 Phase 3: the substrate beacon stops publishing
        // `xi_signature: null` for HRM backends. Guard the downcast contract:
        // a non-HRM backend must stay None (prior behavior), an HRM backend
        // must yield Some with the bridge keys. A broken downcast would
        // silently restore the null beacon while staying green elsewhere.
        let none_engine = ResonanceEngine::new(Box::new(TestMedium::new()), make_pipeline());
        assert!(
            none_engine.xi_bridge_summary().is_none(),
            "non-HRM backend must yield None"
        );

        let temp = tempfile::NamedTempFile::new().unwrap();
        let mut hrm = crate::hrm_store::HrmStore::new(make_pipeline(), temp.path().to_path_buf());
        for i in 0..4 {
            hrm.insert(make_memory(vec![0.1 + i as f32 * 0.15; 10_000], "m"))
                .unwrap();
        }
        let hrm_engine = ResonanceEngine::new(Box::new(hrm), make_pipeline());
        let summary = hrm_engine
            .xi_bridge_summary()
            .expect("HRM backend must yield Some(xi_signature)");
        assert!(summary["n"].as_u64().is_some(), "summary must carry n");
        assert!(summary["residue"].as_f64().unwrap().is_finite());
        assert!(summary["spectral_xi"].as_f64().unwrap().is_finite());
    }

    // Skip link tests removed — associations now emergent from ChiralMedium interference

    #[test]
    fn phi_span_scoring() {
        // φ^1 ≈ 1.618, φ^2 ≈ 2.618, φ^3 ≈ 4.236, φ^4 ≈ 6.854, φ^5 ≈ 11.09
        let s0 = phi_span_score(0);
        let s2 = phi_span_score(2);
        let s3 = phi_span_score(3);
        let s4 = phi_span_score(4);
        let s7 = phi_span_score(7);
        let s11 = phi_span_score(11);

        assert_eq!(s0, 0.0, "span 0 should score 0");
        assert!(s2 > 0.5, "span 2 near φ^1 should score high, got {s2}");
        assert!(s3 > 0.5, "span 3 near φ^2 should score high, got {s3}");
        assert!(s4 > 0.5, "span 4 near φ^3 should score high, got {s4}");
        assert!(s7 > 0.5, "span 7 near φ^4 should score high, got {s7}");
        assert!(s11 > 0.5, "span 11 near φ^5 should score high, got {s11}");

        // Spans far from any φ^k should score lower
        let s20 = phi_span_score(20);
        assert!(s11 > s20, "φ-aligned spans should score higher than non-aligned");
    }
}
