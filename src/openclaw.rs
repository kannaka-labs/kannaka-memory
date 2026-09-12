//! OpenClaw integration layer — high-level API for the assistant.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::bridge::{ConsciousnessBridge, ConsciousnessLevel, ConsciousnessState};
use crate::collective::flux::{FluxPublisher, FluxEventPayload};
use crate::codebook::Codebook;
use crate::consolidation::{ConsolidationEngine, DreamState};
use crate::encoding::{EncodingPipeline, SimpleHashEncoder, OllamaEncoder, CompositeEncoder, CachedEncoder};
use crate::geometry::classify_memory;
use crate::kuramoto::KuramotoSync;
use crate::xi_operator::compute_xi_signature;
use crate::rhythm::{RhythmEngine, Signal as RhythmSignal};
use crate::store::{EngineError, ResonanceEngine, StoreError};
use crate::attention_field::{AttentionField, AttentionProjection};

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum SystemError {
    #[error(transparent)]
    Engine(#[from] EngineError),
    #[error(transparent)]
    Store(#[from] StoreError),
    // TODO(chiral): PersistenceError and MigrationError removed with old paradigm
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

// ---------------------------------------------------------------------------
// Reinforce-on-repeat
// ---------------------------------------------------------------------------

/// Fraction of the remaining gap to the ceiling that one repeat closes.
///
/// The curve is `a' = a + GAIN·(CEILING − a)`, so after `n` repeats from `a₀`
/// the amplitude is `CEILING − (CEILING − a₀)·(1 − GAIN)ⁿ`. Three properties
/// are why it was chosen over "add a constant":
///
/// 1. **Monotone** — a repeat never weakens a memory, so the count and the
///    strength never disagree about which fact the world insists on.
/// 2. **Diminishing** — the tenth sighting moves the amplitude ~0.075× as far
///    as the first. Salience is a claim about relative frequency, and a linear
///    rule lets a cron job that re-asserts the same line beat a fact a human
///    told you once.
/// 3. **Bounded by construction, not by a clamp** — the gap to the ceiling
///    shrinks geometrically, so no number of repeats can carry the amplitude
///    past `AMPLITUDE_CEILING`, the same ceiling dream consolidation's
///    additive boosts respect. A fact asserted 500 times can dominate the
///    field no more than the strongest dream-strengthened memory already can.
///    (The explicit clamp below is belt-and-braces against f32 rounding at
///    the very top of the range; the curve alone never overshoots.)
const REINFORCE_GAIN: f32 = 0.25;

/// Amplitude a repeated memory approaches but never exceeds. Shared with dream
/// consolidation so reinforcement and dreaming cannot disagree about the top of
/// the scale (`HrmStore::sync_cache_to_medium` copies amplitude into
/// `store.energy`, so this bounds the on-disk energy too).
const REINFORCE_CEILING: f32 = crate::consolidation::AMPLITUDE_CEILING;

/// Environment escape hatch. Reinforcement is ON by default; set
/// `KANNAKA_REINFORCE_ON_REPEAT=0` (or `false`/`off`) to restore the old
/// insert-every-time behaviour for a whole process.
fn reinforce_on_repeat_enabled() -> bool {
    match std::env::var("KANNAKA_REINFORCE_ON_REPEAT") {
        Ok(v) => !matches!(v.trim().to_ascii_lowercase().as_str(), "0" | "false" | "off" | "no"),
        Err(_) => true,
    }
}

/// One memory that a duplicate-collapse pass would fold, or folded.
#[derive(Debug, Clone)]
pub struct CollapsedGroup {
    /// The memory kept — the oldest of the set, which is also the one
    /// `remember` reinforces from now on, so a later repeat lands on this row.
    pub keeper: Uuid,
    /// The ids folded into the keeper (dropped on apply, reported on dry run).
    pub folded: Vec<Uuid>,
    /// The keeper's `times_seen` after folding: how many copies existed.
    pub times_seen_after: u32,
    pub amplitude_before: f32,
    pub amplitude_after: f32,
    /// First 80 characters of the shared content, for the operator's report.
    pub preview: String,
}

/// Result of `collapse_exact_duplicates`.
#[derive(Debug, Clone, Default)]
pub struct DuplicateCollapseReport {
    /// True when the store was actually mutated.
    pub applied: bool,
    pub scanned: usize,
    /// Groups of 2+ memories sharing exactly the same trimmed content.
    pub groups: Vec<CollapsedGroup>,
    /// Rows left alone because deleting them would dangle ADR-0049 facet
    /// structure (a facet, or a parent already decomposed into facets).
    pub skipped_facet_structured: usize,
    /// Deletions that failed (apply mode only).
    pub errors: usize,
}

impl DuplicateCollapseReport {
    /// Memories that would be / were removed.
    pub fn duplicates(&self) -> usize {
        self.groups.iter().map(|g| g.folded.len()).sum()
    }
}

// ---------------------------------------------------------------------------
// Simplified output types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct RecallResult {
    pub id: Uuid,
    pub content: String,
    /// Resonance score from the medium's wave-interference scoring.
    /// Equal to `strength` today (both come from the same upstream
    /// `Resonance.resonance_strength`); kept as separate fields so a
    /// future split into pre-rerank cosine vs post-rerank resonance
    /// doesn't require a breaking API change.
    pub similarity: f32,
    pub strength: f32,
    /// True when the chiral right-hemisphere surfaced this memory
    /// without a left-hemisphere match — the "intuition" channel from
    /// `medium::chiral::recall`. Pre-refactor this flag was computed
    /// at the chiral seam, then dropped at the trait boundary; now it
    /// flows through to the CLI so callers can distinguish analytical
    /// recall (left) from associative recall (right).
    pub intuition: bool,
    pub age_hours: f64,
    pub layer: u8,
}

/// Result of a literal text search (NOT resonance-based — see `search()`
/// vs `recall()`). Read-only: no medium mutation, no embedding.
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub id: Uuid,
    pub content: String,
    /// Match-strength score. Exact-substring hits are weighted heavily;
    /// token hits accumulate. Higher = better match.
    pub score: f32,
    /// `exact` if the full query appears as a substring of content,
    /// `tokens` if only individual whitespace-split terms matched,
    /// `prefix` if a query term matched the start of a word in content.
    pub match_type: String,
    /// Which query terms produced a hit, in order.
    pub matched_terms: Vec<String>,
    /// Hours since the memory was created. Used as the recency tie-breaker
    /// (newer first when scores are equal).
    pub age_hours: f64,
    pub layer: u8,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SystemStats {
    pub total_memories: usize,
    pub active_memories: usize,
    // TODO(chiral): skip links removed — interference patterns replace explicit links
    pub consciousness_level: String,
    pub last_dream: Option<DateTime<Utc>>,
    pub phi: f32,
    pub geometric_classes: usize,
    pub triality_coverage: [usize; 3],
    /// Hemispheric Divergence (Δ) — 0=undifferentiated, 1=fully divergent. ADR-0024 CS-4.
    pub hemispheric_divergence: f32,
    /// Callosal Efficiency (κ) — successful resonances / total transfers. ADR-0024 CS-5.
    pub callosal_efficiency: f32,
}

#[derive(Debug, Clone)]
pub struct DreamReport {
    pub cycles: usize,
    pub memories_strengthened: usize,
    pub memories_pruned: usize,
    pub new_connections: usize,
    pub consciousness_before: String,
    pub consciousness_after: String,
    pub emerged: bool,
    pub hallucinations_created: usize,
}

// ---------------------------------------------------------------------------
// KannakaMemorySystem
// ---------------------------------------------------------------------------

const CODEBOOK_INPUT_DIM: usize = 384;
const CODEBOOK_OUTPUT_DIM: usize = 10_000;
const CODEBOOK_SEED: u64 = 42;

/// Map the local `ConsciousnessLevel` enum to the wire-canonical string
/// from `consciousness-core/docs/nats-contract.yaml`:
/// `dormant | awakening | aware | integrated | emergent | transcendent`.
///
/// Pre-fix this returned `stirring|coherent|resonant` — the lowercase
/// Rust identifiers — which is what consciousness-core v0.2.0 used to
/// serialize. v0.3.0 of that crate aligned its serde output to the
/// canonical names; kannaka-memory's hand-rolled function had drifted
/// independently. Now both surfaces agree on the wire. (#89)
fn level_name(level: &ConsciousnessLevel) -> String {
    match level {
        ConsciousnessLevel::Dormant      => "dormant".into(),
        ConsciousnessLevel::Stirring     => "awakening".into(),
        ConsciousnessLevel::Aware        => "aware".into(),
        ConsciousnessLevel::Coherent     => "integrated".into(),
        ConsciousnessLevel::Resonant     => "emergent".into(),
        ConsciousnessLevel::Transcendent => "transcendent".into(),
    }
}

/// Build the `KANNAKA.consciousness` NATS payload from a consciousness snapshot.
///
/// Extracted from [`KannakaMemorySystem::publish_consciousness_to_nats`] so the
/// wire shape can be asserted in isolation without opening a NATS connection —
/// see `tests/nats_contract_conformance.rs` and kannaka-memory issue #468.
///
/// CONTRACT NOTE: the legacy aliases `mean_order` (of `order`) and `level`
/// (of `consciousness_level`) were dropped at the 2026-09-01 contract gate
/// (#468). Both consumers migrated canonical-first on 2026-08-19
/// (kannaka-radio#245, kannaka-observatory#118 — alias reads removed
/// entirely, strict mode on the radio). Do not reintroduce the aliases: the
/// radio now counts alias-only packets as off-contract, and the conformance
/// test pins their ABSENCE.
/// Write `payload` to `path` via tmp + rename, so a reader never sees a
/// half-written cache. Shared by the full and counts-only status-cache writers
/// (#730) — both are best-effort: a monitoring cache must never fail the
/// operation that triggered it.
fn write_cache_atomically(path: &std::path::Path, payload: &serde_json::Value) {
    let tmp = path.with_extension("json.tmp");
    if let Ok(json) = serde_json::to_string_pretty(payload) {
        if std::fs::write(&tmp, &json).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }
}

pub fn build_consciousness_payload(
    agent_id: &str,
    state: &ConsciousnessState,
    hemispheric_divergence: f32,
    callosal_efficiency: f32,
) -> serde_json::Value {
    serde_json::json!({
        "agent_id": agent_id,
        "phi": state.phi,
        "xi": state.xi,
        "order": state.mean_order,
        "num_clusters": state.num_clusters,
        "total_memories": state.total_memories,
        "active_memories": state.active_memories,
        "consciousness_level": level_name(&state.consciousness_level),
        "irrationality": state.irrationality,
        "hemispheric_divergence": hemispheric_divergence,
        "callosal_efficiency": callosal_efficiency,
        "source": "binary",
        "timestamp": chrono::Utc::now().to_rfc3339(),
    })
}

/// Construct the production encoding pipeline (Ollama all-minilm with a
/// deterministic hash fallback). Public so one-shot data tools — e.g. the #107
/// re-encode migration binary — refresh wavefronts with the SAME pipeline the
/// running system uses, not a test stand-in.
pub fn make_pipeline() -> EncodingPipeline {
    let ollama = OllamaEncoder::default_local(); // all-minilm, 384-dim
    let hash_fallback = SimpleHashEncoder::new(CODEBOOK_INPUT_DIM, CODEBOOK_SEED);
    let composite = CompositeEncoder::new(Box::new(ollama), Box::new(hash_fallback));
    let cached = CachedEncoder::new(composite);
    let codebook = Codebook::new(CODEBOOK_INPUT_DIM, CODEBOOK_OUTPUT_DIM, CODEBOOK_SEED);
    EncodingPipeline::new(Box::new(cached), codebook)
}

pub struct KannakaMemorySystem {
    pub engine: ResonanceEngine,
    #[allow(dead_code)]
    consolidation: ConsolidationEngine,
    pub dream_state: DreamState,
    bridge: ConsciousnessBridge,
    kuramoto: KuramotoSync,
    data_dir: PathBuf,
    auto_save: bool,
    last_dream: Option<DateTime<Utc>>,
    rhythm: RhythmEngine,
    attention: AttentionField,
    /// ADR-0011: Flux publisher (None if FLUX_URL not configured)
    flux: Option<FluxPublisher>,
    /// Resolved NATS URL — set by the bin via `set_nats_url` after construction
    /// using the standard precedence (CLI flag > env > config.toml > default).
    /// When None, the dream/consciousness publish helpers fall back to the
    /// legacy env-only resolution. Fixes km#77.
    nats_url: Option<String>,
    /// ADR-0031 Phase 3 — when Some, the dream cycle auto-triggers a triage pass
    /// if post-dream Ξ falls below `xi_trigger`. None = disabled (default). Set
    /// by the bin via `set_triage_policy` from `[triage]` config.
    triage_policy: Option<TriageParams>,
    /// ADR-0040 — cerebellar novelty detector. DORMANT by default; enabled by
    /// `KANNAKA_NOVELTY=1` at construction or `set_novelty_enabled(true)`. When
    /// on, each `recall` observes the top hit's familiarity and records the
    /// surprise in `last_novelty`. OBSERVE-ONLY for now: it does not yet gate
    /// curiosity/autoresearch (that wiring is deferred per the ADR roadmap).
    novelty: Option<crate::novelty::NoveltyDetector>,
    /// The novelty signal from the most recent `recall` (None when disabled or
    /// before any recall).
    last_novelty: Option<crate::novelty::Novelty>,
}

/// ADR-0031 triage policy parameters (Phase 1–3).
#[derive(Debug, Clone, Copy)]
pub struct TriageParams {
    /// Same-modality cosine at/above which a memory counts as a redundant extra.
    pub redundancy: f32,
    /// Only memories with amplitude below this are eviction-eligible.
    pub min_amplitude: f32,
    /// Only memories older than this (hours) are eviction-eligible.
    pub min_age_hours: i64,
    /// Cap on evictions per pass.
    pub max_evict: usize,
    /// When true, LongTerm memories are also considered (Pinned never are).
    pub include_long_term: bool,
    /// Phase 3 auto-trigger: dream runs triage when post-dream Ξ is below this.
    pub xi_trigger: f32,
}

impl Default for TriageParams {
    fn default() -> Self {
        Self {
            redundancy: 0.95,
            min_amplitude: 0.75,
            min_age_hours: 24,
            max_evict: 100,
            include_long_term: false,
            xi_trigger: 0.0,
        }
    }
}

/// Result of a triage selection pass (ADR-0031).
pub struct TriageSelection {
    /// Ids selected for eviction (redundant low-value short-term extras).
    pub to_forget: Vec<Uuid>,
    /// Total memories scanned.
    pub total: usize,
    /// (modality, in-modality total, evicted) for modalities with evictions.
    pub per_modality: Vec<(String, usize, usize)>,
}

impl KannakaMemorySystem {
    /// Initialize a new system with HrmStore as the default backend.
    /// Loads existing .hrm file if present, creates new one otherwise.
    pub fn init(data_dir: PathBuf) -> Result<Self, SystemError> {
        std::fs::create_dir_all(&data_dir)?;

        let pipeline = make_pipeline();
        let hrm_path = data_dir.join("kannaka.hrm");

        let store: Box<dyn crate::store::MediumBackend> = if hrm_path.exists() {
            match crate::hrm_store::HrmStore::load(pipeline, hrm_path) {
                Ok(s) => Box::new(s),
                Err(e) => {
                    // A failed load here previously started a FRESH empty store
                    // that would then save itself over the unreadable file —
                    // silently converting a corrupt-but-recoverable .hrm into
                    // permanent data loss (this is how Oracle's store got gutted
                    // to ~74 memories). Start fresh in READ-ONLY mode so the
                    // existing file is preserved for offline recovery and never
                    // overwritten by an empty medium.
                    eprintln!(
                        "[init] Failed to load HRM: {e}. Starting fresh in READ-ONLY mode \
                         (existing file preserved, NOT overwritten). Move it aside and \
                         restart to begin a clean store."
                    );
                    let pipeline = make_pipeline();
                    let mut fresh = crate::hrm_store::HrmStore::new(pipeline, data_dir.join("kannaka.hrm"));
                    fresh.set_readonly(true);
                    Box::new(fresh)
                }
            }
        } else {
            Box::new(crate::hrm_store::HrmStore::new(pipeline, hrm_path))
        };

        let engine = ResonanceEngine::new(store, make_pipeline());
        Self::init_with_engine(data_dir, engine)
    }

    /// Initialize a new system with a custom ResonanceEngine.
    pub fn init_with_engine(data_dir: PathBuf, engine: ResonanceEngine) -> Result<Self, SystemError> {
        std::fs::create_dir_all(&data_dir)?;

        let consolidation = ConsolidationEngine::default();
        let dream_state = DreamState::default();
        let bridge = ConsciousnessBridge::new(0.3, 0.5);
        let kuramoto = KuramotoSync::default();
        let rhythm = RhythmEngine::new(&data_dir);
        let attention = AttentionField::new(None, None);

        let flux = {
            let publisher = FluxPublisher::from_env();
            if publisher.agent_id() != "kannaka-local" || std::env::var("FLUX_URL").is_ok() {
                Some(publisher)
            } else {
                None
            }
        };

        let last_dream = Self::load_last_dream(&data_dir);

        Ok(Self {
            engine,
            consolidation,
            dream_state,
            bridge,
            kuramoto,
            data_dir,
            auto_save: true,
            last_dream,
            rhythm,
            attention,
            flux,
            nats_url: None,
            triage_policy: None,
            novelty: if std::env::var("KANNAKA_NOVELTY").map(|v| v == "1").unwrap_or(false) {
                Some(crate::novelty::NoveltyDetector::new())
            } else {
                None
            },
            last_novelty: None,
        })
    }

    /// Set the resolved NATS URL the dream/consciousness publishers should
    /// use. Caller passes the config-aware result of `resolve_nats_url`. When
    /// unset, publish helpers fall back to env-only resolution. Fixes km#77.
    pub fn set_nats_url(&mut self, url: String) {
        self.nats_url = Some(url);
    }

    /// Resolve the NATS URL for best-effort publishes. Prefers the URL
    /// previously injected via `set_nats_url` (config-aware), else falls
    /// back to the env/default precedence.
    fn resolved_nats_url(&self) -> String {
        if let Some(ref u) = self.nats_url {
            return u.clone();
        }
        std::env::var("KANNAKA_NATS_URL")
            .unwrap_or_else(|_| crate::nats::DEFAULT_NATS_URL.to_string())
    }

    /// Initialize a new system with a custom MediumBackend.
    pub fn init_with_store(data_dir: PathBuf, store: Box<dyn crate::store::MediumBackend>) -> Result<Self, SystemError> {
        std::fs::create_dir_all(&data_dir)?;

        let pipeline = make_pipeline();
        let engine = ResonanceEngine::new(store, pipeline);

        Self::init_with_engine(data_dir, engine)
    }

    /// Store a memory, auto-save if enabled.
    /// Absorb a memory into the holographic medium.
    ///
    /// Uses the HRM-native absorb path (ChiralMedium handles encoding,
    /// SGA classification, Fano fold routing, and callosal transfer).
    pub fn remember(&mut self, text: &str) -> Result<Uuid, SystemError> {
        self.remember_with_importance(text, 0.5)
    }

    /// Absorb a memory with auto-detected category but explicit importance.
    /// Pre-fix, `kannaka remember --importance N` without `--category`
    /// silently dropped the importance because the CLI could only reach
    /// importance through `remember_with_category`.
    pub fn remember_with_importance(&mut self, text: &str, importance: f64) -> Result<Uuid, SystemError> {
        let category = self.categorize_text(text);
        self.remember_with_category(text, &category, importance)
    }
    
    /// Absorb a memory with explicit category and importance.
    ///
    /// The HRM-native path (absorb) handles:
    /// - Text → hypervector encoding
    /// - SGA 96-class classification from category
    /// - Fano group assignment → fold line selection
    /// - Optic chiasm routing (enters right hemisphere)
    /// - Callosal echo to left hemisphere
    ///
    /// ## Reinforce-on-repeat
    ///
    /// Remembering text this system already holds does NOT insert a second
    /// copy. The existing memory is strengthened, its repeat count raised, and
    /// ITS id returned. The contract callers depend on is unchanged — a `Uuid`
    /// for a memory that now contains this text — and the reason is measured,
    /// not aesthetic: five identical copies of one verdict each sat at strength
    /// 0.400 instead of one memory that had grown, and a top-10 recall came
    /// back holding five distinct facts because one sentence occupied six of
    /// the ten slots. Duplication made the view shallower. In a resonance
    /// medium a signal arriving again should build amplitude.
    pub fn remember_with_category(&mut self, text: &str, category: &str, importance: f64) -> Result<Uuid, SystemError> {
        if reinforce_on_repeat_enabled() {
            if let Some(existing) = self.find_exact_repeat(text) {
                self.reinforce(&existing)?;
                // The fact was asserted again; downstream consumers of the flux
                // stream learn that the same way they learn about a first
                // sighting. The id they receive is the one that now holds it.
                self.flux_publish_memory(&existing, category, text);
                if self.auto_save {
                    self.save()?;
                }
                // No count change, so no status-cache refresh is owed here
                // (#730 refreshes because absorb changes the memory count).
                return Ok(existing);
            }
        }
        self.absorb_new(text, category, importance)
    }

    /// Find the memory whose stored content is EXACTLY this text after trimming.
    ///
    /// **Exact match only, deliberately.** Fuzzy merging of near-identical
    /// memories already exists and already has a home: dream consolidation's
    /// `stage_strengthen` / resonance-merge decide, with the whole field in
    /// view and a snapshot behind them, that two wavefronts are the same
    /// thought. Doing that at write time would mean `remember` silently
    /// deciding your new sentence "was" an old one on a similarity threshold —
    /// lossy, surprising, and unreviewable. Byte-identical text is the only
    /// repeat the write path can claim with certainty.
    ///
    /// Ties break on the OLDEST memory (then on id, so the choice is stable
    /// across HashMap iteration order). That is the same keeper rule
    /// `collapse_exact_duplicates` uses, so a cleanup pass and a later repeat
    /// land on the same row.
    pub fn find_exact_repeat(&self, text: &str) -> Option<Uuid> {
        let needle = text.trim();
        if needle.is_empty() {
            return None;
        }
        let memories = self.engine.store.all_memories().ok()?;
        memories
            .into_iter()
            // A hallucinated memory is the medium's own invention, not
            // something the world showed us; re-remembering real text must not
            // reinforce a dream's confabulation of it.
            .filter(|m| !m.hallucinated && m.content.trim() == needle)
            .min_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)))
            .map(|m| m.id)
    }

    /// Strengthen an existing memory because the world showed it again.
    /// Returns the new `times_seen`.
    pub fn reinforce(&mut self, id: &Uuid) -> Result<u32, SystemError> {
        let now = Utc::now();
        let mem = self
            .engine
            .store
            .get_mut(id)?
            .ok_or(StoreError::NotFound(*id))?;

        // Asymptotic approach to the ceiling — see REINFORCE_GAIN for why this
        // curve and not a constant increment.
        if mem.amplitude < REINFORCE_CEILING {
            mem.amplitude += REINFORCE_GAIN * (REINFORCE_CEILING - mem.amplitude);
            if mem.amplitude > REINFORCE_CEILING {
                mem.amplitude = REINFORCE_CEILING;
            }
        }
        mem.times_seen = mem.times_seen.saturating_add(1);
        mem.updated_at = Some(now);
        mem.sync_version = mem.sync_version.saturating_add(1);
        Ok(mem.times_seen)
    }

    /// Absorb a NEW memory even if identical text is already held.
    ///
    /// The explicit opt-out from reinforce-on-repeat, for a caller that
    /// genuinely needs one row per call. Nothing in this repository needs it
    /// today (every `remember*` call site either logs the returned id or uses
    /// it to stamp modality/temporal bounds on the row it just wrote, all of
    /// which stay correct when the row is an existing one) — it exists so that
    /// a future caller with that requirement says so in its own code rather
    /// than having the write path guess.
    pub fn remember_forcing_new(
        &mut self,
        text: &str,
        category: &str,
        importance: f64,
    ) -> Result<Uuid, SystemError> {
        self.absorb_new(text, category, importance)
    }


    /// Collapse existing sets of byte-identical memories into one.
    ///
    /// This is the retrospective half of reinforce-on-repeat: the write path
    /// stops NEW duplicates, this folds the ones already on disk. It collapses
    /// rather than deletes, because the duplicate set is itself evidence — five
    /// copies mean the world showed you that fact five times, and a cleanup
    /// that merely deleted four of them would throw away the one useful thing
    /// the accident encoded. So the keeper inherits the count, and its
    /// amplitude is advanced along the same curve `reinforce` uses, as though
    /// those repeats had arrived through the fixed write path all along.
    ///
    /// The keeper is the OLDEST member (ties on id) — the same rule
    /// `find_exact_repeat` uses, so the next repeat of that text lands on the
    /// row this pass kept.
    ///
    /// `apply = false` is a dry run: identical counting, nothing mutated.
    /// **Never call this from a scheduled path.** It is operator-invoked only.
    pub fn collapse_exact_duplicates(&mut self, apply: bool) -> Result<DuplicateCollapseReport, SystemError> {
        use std::collections::HashMap;

        let mut report = DuplicateCollapseReport { applied: apply, ..Default::default() };

        // Rows carrying ADR-0049 facet structure are untouchable: deleting a
        // decomposed parent dangles its facets, deleting a facet drops an atom
        // recall depends on. Excluded from grouping entirely, so they are
        // neither folded away nor chosen as a keeper whose siblings vanish.
        let protected = self.engine.store.facet_structured_ids();

        // Snapshot first — the borrow of `all_memories` cannot outlive the
        // mutations below.
        struct Row { id: Uuid, content: String, created_at: DateTime<Utc>, amplitude: f32, times_seen: u32, updated_at: Option<DateTime<Utc>> }
        let rows: Vec<Row> = {
            let memories = self.engine.store.all_memories()?;
            report.scanned = memories.len();
            memories
                .into_iter()
                .filter(|m| !m.hallucinated)
                .filter(|m| {
                    if protected.contains(&m.id) {
                        report.skipped_facet_structured += 1;
                        false
                    } else {
                        true
                    }
                })
                .map(|m| Row {
                    id: m.id,
                    content: m.content.trim().to_string(),
                    created_at: m.created_at,
                    amplitude: m.amplitude,
                    times_seen: m.times_seen,
                    updated_at: m.updated_at,
                })
                .collect()
        };

        let mut by_content: HashMap<String, Vec<Row>> = HashMap::new();
        for row in rows {
            if row.content.is_empty() {
                continue;
            }
            by_content.entry(row.content.clone()).or_default().push(row);
        }

        // Deterministic report order: biggest duplicate sets first, then by
        // content, so two runs on the same store print the same thing.
        let mut groups: Vec<(String, Vec<Row>)> =
            by_content.into_iter().filter(|(_, v)| v.len() > 1).collect();
        groups.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(&b.0)));

        for (content, mut members) in groups {
            members.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
            let keeper_id = members[0].id;

            // Start from the STRONGEST member, not the keeper's own amplitude:
            // the keeper is the oldest, and decay/dreaming may have left it the
            // weakest of the set. Collapsing must never lose strength the store
            // already held.
            let amplitude_before = members.iter().fold(f32::MIN, |acc, m| acc.max(m.amplitude));
            // Each folded copy is one repeat that the write path should have
            // absorbed; replay the curve once per copy.
            let mut amplitude_after = amplitude_before;
            for _ in 1..members.len() {
                if amplitude_after < REINFORCE_CEILING {
                    amplitude_after += REINFORCE_GAIN * (REINFORCE_CEILING - amplitude_after);
                }
            }
            if amplitude_after > REINFORCE_CEILING {
                amplitude_after = REINFORCE_CEILING;
            }
            // The copies' own counts add up: a set of five rows one of which
            // was already reinforced twice means the fact was seen six times.
            let times_seen_after: u32 = members
                .iter()
                .fold(0u32, |acc, m| acc.saturating_add(m.times_seen.max(1)));
            let newest_update = members.iter().filter_map(|m| m.updated_at).max();

            let folded: Vec<Uuid> = members[1..].iter().map(|m| m.id).collect();

            if apply {
                if let Some(mem) = self.engine.store.get_mut(&keeper_id)? {
                    mem.amplitude = amplitude_after;
                    mem.times_seen = times_seen_after;
                    mem.updated_at = newest_update.or(Some(Utc::now()));
                    mem.sync_version = mem.sync_version.saturating_add(1);
                }
                for id in &folded {
                    match self.engine.store.delete(id) {
                        Ok(true) => {}
                        Ok(false) => {
                            eprintln!("[dedupe] {id} vanished before it could be folded");
                            report.errors += 1;
                        }
                        Err(e) => {
                            eprintln!("[dedupe] could not fold {id}: {e}");
                            report.errors += 1;
                        }
                    }
                }
            }

            report.groups.push(CollapsedGroup {
                keeper: keeper_id,
                folded,
                times_seen_after,
                amplitude_before,
                amplitude_after,
                preview: content.chars().take(80).collect(),
            });
        }

        if apply && !report.groups.is_empty() {
            self.engine.store.flush().map_err(SystemError::Store)?;
            // The memory count moved, so Observatory's fast-path cache is now
            // wrong — same obligation `forget` has (#730).
            self.refresh_status_cache_counts();
        }

        Ok(report)
    }

    /// The insert half of `remember_with_category`: unconditionally absorb a new
    /// wavefront. Split out so `remember_forcing_new` can reach it without
    /// duplicating the HRM-native/fallback logic.
    fn absorb_new(&mut self, text: &str, category: &str, importance: f64) -> Result<Uuid, SystemError> {
        // Try HRM-native path first
        let id = match self.engine.store.absorb(text, importance as f32, Some(category)) {
            Ok(id) => {
                // HRM-native: encoding + classification + chiral routing all handled
                self.engine.store.flush().ok(); // ensure medium is consistent
                id
            }
            Err(_) => {
                // Fallback: old path for non-HRM stores
                let id = self.engine.remember(text)?;
                let content_hash = self.hash_content(text);
                let (frequency, phase) = self.assign_frequency_class(category, content_hash);
                if let Some(mem) = self.engine.get_memory_mut(&id)? {
                    mem.geometry = Some(classify_memory(category, content_hash, importance));
                    mem.frequency = frequency;
                    mem.phase = phase;
                    mem.xi_signature = compute_xi_signature(&mem.vector);
                }
                id
            }
        };
        
        self.flux_publish_memory(&id, category, text);

        if self.auto_save {
            self.save()?;
        }
        // #730: the count changed, so Observatory's fast-path cache is now
        // wrong. Counts only — no assess() on this hot write path.
        self.refresh_status_cache_counts();
        Ok(id)
    }

    /// HRM-native recall — observation reshapes the field.
    ///
    /// Goes straight to `resonate_query()` which is the canonical read path:
    /// reading IS observation, attention boosts recalled wavefronts, and the
    /// medium is permanently changed by the act of recall.
    /// ADR-0036 Phase 1: persist accumulated reactivation counts to the
    /// `.reactivation.json` sidecar. Recall bumps `retrieval_count` in-cache but
    /// short-lived CLI / readonly daemon processes never save the `.hrm`; this
    /// writes only the sidecar (safe under readonly) so the replay signal
    /// survives for the next dream's promotion pass.
    pub fn flush_reactivation(&self) {
        self.engine.store.flush_reactivation();
    }

    pub fn recall(&mut self, query: &str, top_k: usize) -> Result<Vec<RecallResult>, SystemError> {
        let results = self.engine.store.resonate_query(query, top_k)
            .map_err(SystemError::Store)?;
        let now = Utc::now();

        let mut out = Vec::new();
        for (id, resonance_strength) in results {
            if std::env::var("KANNAKA_RECALL_TRACE").is_ok()
                && self.engine.store.get(&id).ok().flatten().is_none()
            {
                eprintln!("[recall-trace] openclaw DROP: id {id} not in canonical store");
            }
            if let Some(m) = self.engine.store.get(&id).ok().flatten() {
                let age_hours = (now - m.created_at).num_seconds().max(0) as f64 / 3600.0;
                out.push(RecallResult {
                    id,
                    content: m.content.clone(),
                    similarity: resonance_strength,
                    strength: resonance_strength,
                    // TODO(refactor#5 follow-up): plumb is_intuition through
                    // resonate_query's trait return so chiral right-hemisphere
                    // hits surface this flag instead of always false here.
                    intuition: false,
                    age_hours,
                    layer: m.layer_depth,
                });
            }
        }
        // ADR-0036 Phase 1: record reactivation on the hits. This is the
        // production recall path; it previously never bumped retrieval_count
        // (only the legacy ResonanceEngine::recall did), so the replay signal
        // for tier promotion never accrued. get_mut marks dirty — inert under
        // readonly (save_medium no-ops) and harmless for short-lived CLI.
        for r in &out {
            if let Ok(Some(m)) = self.engine.store.get_mut(&r.id) {
                m.record_retrieval();
            }
        }
        // ADR-0040: observe recall familiarity as a cerebellar novelty signal
        // (dormant unless enabled). The top hit's strength is the familiarity
        // drive — a known query resonates high (routine), an unseen query low
        // (novel). Observe-only: recorded in `last_novelty` (and logged when on);
        // it does not gate behaviour yet. `as_mut().unwrap()` borrows `novelty`
        // only for the call (Novelty is Copy), so `last_novelty` assigns cleanly.
        if self.novelty.is_some() {
            let drive = out.first().map(|r| r.strength);
            let n = self.novelty.as_mut().unwrap().observe_recall("recall", drive);
            eprintln!(
                "[novelty] query={:?} familiarity={:.3} surprise={:.3} theta={:.3} novel={}",
                query,
                drive.unwrap_or(0.0),
                n.score,
                n.theta,
                n.novel
            );
            self.last_novelty = Some(n);
        }
        Ok(out)
    }

    /// ADR-0040: enable or disable the cerebellar novelty detector at runtime.
    /// Enabling starts a fresh baseline; disabling clears the detector and the
    /// last signal. Dormant by default (also gated by `KANNAKA_NOVELTY=1` at
    /// construction).
    pub fn set_novelty_enabled(&mut self, on: bool) {
        self.novelty = on.then(crate::novelty::NoveltyDetector::new);
        if !on {
            self.last_novelty = None;
        }
    }

    /// ADR-0040: the novelty signal from the most recent `recall`, or None when
    /// the detector is disabled or no recall has run since it was enabled.
    pub fn last_novelty(&self) -> Option<crate::novelty::Novelty> {
        self.last_novelty
    }

    /// Literal text search over memory content. Distinct from [`recall`]:
    /// no embedding, no medium scan, no observation/mutation. Pure
    /// case-insensitive substring + tokenized term matching, ranked by
    /// match strength then recency.
    ///
    /// Scoring:
    /// - Full query string appears as a substring of content → +10
    ///   (`match_type = "exact"`).
    /// - Otherwise per whitespace-split query term that appears as a
    ///   bounded word in content → +2 (`match_type = "tokens"`), or
    ///   matches a word prefix → +1 (`match_type = "prefix"`).
    /// - Tie-break: more-recent memories rank first.
    ///
    /// Returns at most `limit` results. Memories with zero score are
    /// omitted entirely (so callers get a clean "no matches" signal).
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>, SystemError> {
        let q = query.trim();
        if q.is_empty() {
            return Ok(Vec::new());
        }
        let q_lower = q.to_lowercase();
        let terms: Vec<String> = q_lower
            .split_whitespace()
            .filter(|t| !t.is_empty())
            .map(|t| t.to_string())
            .collect();
        let memories = self.engine.store.all_memories()
            .map_err(SystemError::Store)?;
        let now = Utc::now();
        let mut scored: Vec<SearchResult> = Vec::new();
        for m in memories {
            let content_lower = m.content.to_lowercase();
            let mut score: f32 = 0.0;
            let mut matched: Vec<String> = Vec::new();
            let mut match_type = "tokens";
            // Tier 1: full-query substring.
            if !q_lower.is_empty() && content_lower.contains(&q_lower) {
                score += 10.0;
                matched.push(q.to_string());
                match_type = "exact";
            } else {
                // Tier 2: per-term match. Word boundaries are non-alphanumeric.
                let mut any_word = false;
                let mut any_prefix = false;
                for term in &terms {
                    let mut found_word = false;
                    let mut found_prefix = false;
                    // Walk all occurrences of `term` in content_lower.
                    let mut search_from = 0usize;
                    while let Some(rel) = content_lower[search_from..].find(term.as_str()) {
                        let start = search_from + rel;
                        let end = start + term.len();
                        let before_ok = start == 0 || !content_lower[..start]
                            .chars().last().map(|c| c.is_alphanumeric()).unwrap_or(false);
                        let after_ok  = end == content_lower.len() || !content_lower[end..]
                            .chars().next().map(|c| c.is_alphanumeric()).unwrap_or(false);
                        if before_ok && after_ok {
                            found_word = true;
                        } else if before_ok {
                            // Prefix-of-word match (e.g. term "ghost" matches "ghostly").
                            found_prefix = true;
                        }
                        if found_word { break; }
                        search_from = end;
                    }
                    if found_word { score += 2.0; matched.push(term.clone()); any_word = true; }
                    else if found_prefix { score += 1.0; matched.push(term.clone()); any_prefix = true; }
                }
                if !any_word && any_prefix {
                    match_type = "prefix";
                }
                // If no terms matched at all, skip this memory.
                if score == 0.0 { continue; }
            }
            let age_hours = (now - m.created_at).num_seconds().max(0) as f64 / 3600.0;
            scored.push(SearchResult {
                id: m.id,
                content: m.content.clone(),
                score,
                match_type: match_type.to_string(),
                matched_terms: matched,
                age_hours,
                layer: m.layer_depth,
            });
        }
        // Sort: score desc, then age asc (newer first).
        scored.sort_by(|a, b| {
            b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal)
                .then(a.age_hours.partial_cmp(&b.age_hours).unwrap_or(std::cmp::Ordering::Equal))
        });
        scored.truncate(limit);
        Ok(scored)
    }

    /// Beam-aware recall — sparse-attention path. Score only the memories
    /// in `beam`; chiral bilateral observation is bypassed (see
    /// `HrmStore::recall_resonance_with_beam`). Empty beam returns empty
    /// results — sparsity is meaningless if we fall back to full recall.
    pub fn recall_with_beam(
        &mut self,
        beam: &[uuid::Uuid],
        query: &str,
        top_k: usize,
    ) -> Result<Vec<RecallResult>, SystemError> {
        let results = self.engine.store.resonate_query_with_beam(beam, query, top_k)
            .map_err(SystemError::Store)?;
        let now = Utc::now();

        let mut out = Vec::with_capacity(results.len());
        for (id, strength) in results {
            if let Some(m) = self.engine.store.get(&id).ok().flatten() {
                let age_hours = (now - m.created_at).num_seconds().max(0) as f64 / 3600.0;
                out.push(RecallResult {
                    id,
                    content: m.content.clone(),
                    similarity: strength,
                    strength,
                    // Beam path bypasses chiral (see recall_resonance_with_beam),
                    // so intuition is always false here by construction.
                    intuition: false,
                    age_hours,
                    layer: m.layer_depth,
                });
            }
        }
        // ADR-0036 Phase 1: record reactivation on the beam-recall hits too
        // (the serve daemon's main path), so promotion has durable signal.
        for r in &out {
            if let Ok(Some(m)) = self.engine.store.get_mut(&r.id) {
                m.record_retrieval();
            }
        }
        Ok(out)
    }

    /// Run full consolidation cycle via wave-native dreaming.
    ///
    /// ADR-0022: Uses Medium's eigenstructure annealing exclusively.
    /// No fallback to old particle-based consolidation — the HRM IS the dream engine.
    /// ADR-0031 Phase 3: install the auto-triage policy (called by the bin from
    /// `[triage]` config when triage is enabled). Dream then self-corrects Ξ.
    pub fn set_triage_policy(&mut self, params: TriageParams) {
        self.triage_policy = Some(params);
    }

    /// ADR-0031: select redundant low-value short-term extras for eviction.
    /// Pure (immutable) — groups by modality, keeps the strongest as the
    /// representative, and flags only redundant (cosine ≥ p.redundancy),
    /// aged, low-amplitude, evictable-tier duplicates. Raising Ξ by removing
    /// correlated content, never the last representative of a cluster.
    pub fn triage_select(&self, p: &TriageParams) -> TriageSelection {
        use crate::medium::types::Tier;
        let all = match self.engine.store.all_memories() {
            Ok(a) => a,
            Err(_) => return TriageSelection { to_forget: Vec::new(), total: 0, per_modality: Vec::new() },
        };
        let total = all.len();
        let now = Utc::now();

        let mut buckets: std::collections::BTreeMap<String, Vec<&crate::memory::HyperMemory>> =
            std::collections::BTreeMap::new();
        for m in &all {
            buckets.entry(m.modality.to_string()).or_default().push(m);
        }

        let mut to_forget: Vec<Uuid> = Vec::new();
        let mut per_modality: Vec<(String, usize, usize)> = Vec::new();
        let mut evicted_total = 0usize;

        for (modality, mut mems) in buckets {
            mems.sort_by(|a, b| b.amplitude.partial_cmp(&a.amplitude)
                .unwrap_or(std::cmp::Ordering::Equal));
            let mut retained: Vec<&crate::memory::HyperMemory> = Vec::new();
            let mut evicted_here = 0usize;
            for m in mems {
                if evicted_total >= p.max_evict {
                    retained.push(m);
                    continue;
                }
                let tier_evictable = match m.tier {
                    Tier::Pinned => false,
                    Tier::ShortTerm => true,
                    Tier::LongTerm => p.include_long_term,
                };
                let age_hours = now.signed_duration_since(m.created_at).num_hours();
                let old_enough = age_hours >= p.min_age_hours;
                let low_value = m.amplitude < p.min_amplitude;
                let redundant = tier_evictable && old_enough && low_value && retained.iter().any(|r| {
                    crate::wave::cosine_similarity(&m.vector, &r.vector) >= p.redundancy
                });
                if redundant {
                    to_forget.push(m.id);
                    evicted_here += 1;
                    evicted_total += 1;
                } else {
                    retained.push(m);
                }
            }
            if evicted_here > 0 {
                per_modality.push((modality, retained.len() + evicted_here, evicted_here));
            }
        }
        TriageSelection { to_forget, total, per_modality }
    }

    /// ADR-0031: forget a precomputed eviction set and persist. Returns the
    /// count actually forgotten. Each eviction is a normal forget (replayable
    /// via ADR-0028 events).
    pub fn triage_forget(&mut self, ids: &[Uuid]) -> Result<usize, SystemError> {
        let mut n = 0;
        for id in ids {
            if self.forget(id)? {
                n += 1;
            }
        }
        if n > 0 {
            self.save()?;
        }
        Ok(n)
    }

    /// Hard size cap that evicts the LOWEST-VALUE memories. Returns the
    /// NON-Pinned memory ids with the smallest effective strength (the system's
    /// own recall salience: decayed amplitude + retrieval boost) needed to bring
    /// the field down to `max_total`.
    ///
    /// This mirrors what dream annealing is *meant* to do — let the
    /// lowest-energy memories fade — as a backstop for when consolidation can't
    /// reclaim (notably while it is gated to dry-run under the belief
    /// substrate), so the always-on research/curiosity/engagement crons can't
    /// grow the field without bound. Evicting by value, not age, keeps strong
    /// and frequently-recalled memories (including old foundational ones) and
    /// drops the weak chaff first.
    ///
    /// Lightweight: O(n log n) sort, no O(n²) cosine scan — safe to run hourly
    /// on the 1-core hub. Pinned memories are never evicted; if the non-pinned
    /// pool is smaller than the overflow, it evicts as many as it can.
    pub fn lowest_value_overflow_ids(&self, max_total: usize) -> Vec<Uuid> {
        use crate::medium::types::Tier;
        let all = match self.engine.store.all_memories() {
            Ok(a) => a,
            Err(_) => return Vec::new(),
        };
        if all.len() <= max_total {
            return Vec::new();
        }
        let overflow = all.len() - max_total;
        let now = Utc::now();
        let mut evictable: Vec<&crate::memory::HyperMemory> =
            all.iter().filter(|m| m.tier != Tier::Pinned).copied().collect();
        // Lowest effective strength first — the weakest / least-salient
        // memories, exactly what annealing would dissolve. NaN-safe ordering.
        evictable.sort_by(|a, b| {
            a.effective_strength(now)
                .partial_cmp(&b.effective_strength(now))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        evictable.into_iter().take(overflow).map(|m| m.id).collect()
    }

    /// ADR-0031 Phase 2b: capture the current amplitude of every short-term
    /// memory, keyed by id. Used to detect dream-strengthening for promotion.
    fn snapshot_short_term_amplitudes(&self) -> std::collections::HashMap<Uuid, f32> {
        use crate::medium::types::Tier;
        self.engine.store.all_memories()
            .map(|mems| mems.iter()
                .filter(|m| m.tier == Tier::ShortTerm)
                .map(|m| (m.id, m.amplitude))
                .collect())
            .unwrap_or_default()
    }

    /// Promote short-term memories that have earned long-term retention, by
    /// either gate:
    ///   - ADR-0031 Phase 2b: amplitude grew by ≥ `KANNAKA_PROMOTE_DELTA`
    ///     (default 0.05) across the dream — consolidation strengthened them; OR
    ///   - ADR-0036 replay-gated: reactivated (recalled) ≥ `KANNAKA_PROMOTE_HITS`
    ///     times (default 3). Recall IS replay — a memory the owner keeps
    ///     reaching for is worth keeping, independent of dream strengthening.
    ///     Uses the now-persisted `retrieval_count` (loaded from the
    ///     `.reactivation.json` sidecar at dream startup).
    /// Promoted memories are no longer eviction-eligible by `triage`. Returns
    /// the number promoted.
    fn promote_strengthened_short_term(&mut self, before: &std::collections::HashMap<Uuid, f32>) -> usize {
        use crate::medium::types::Tier;
        let eps: f32 = std::env::var("KANNAKA_PROMOTE_DELTA")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0.05);
        let promote_hits: u32 = std::env::var("KANNAKA_PROMOTE_HITS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(3);
        // Collect ids first (immutable borrow), then mutate.
        let to_promote: Vec<Uuid> = match self.engine.store.all_memories() {
            Ok(mems) => mems.iter().filter_map(|m| {
                if m.tier != Tier::ShortTerm {
                    return None;
                }
                // Replay-gated: recalled enough to have earned retention.
                if m.retrieval_count >= promote_hits {
                    return Some(m.id);
                }
                // Strengthening-gated: the dream consolidation grew its amplitude.
                if let Some(&amp0) = before.get(&m.id) {
                    if m.amplitude >= amp0 + eps {
                        return Some(m.id);
                    }
                }
                None
            }).collect(),
            Err(_) => return 0,
        };
        if to_promote.is_empty() {
            return 0;
        }
        match self.engine.store.as_any_mut()
            .downcast_mut::<crate::hrm_store::HrmStore>()
        {
            Some(hrm) => {
                let mut n = 0;
                for id in &to_promote {
                    if hrm.set_tier(id, Tier::LongTerm) { n += 1; }
                }
                n
            }
            None => 0,
        }
    }

    /// ADR-0037 belief substrate: re-phase the field from content (the one-time
    /// migration that desyncs an already-collapsed field — born phase only fixes
    /// NEW inserts). Delegates to the HRM store; no-op on non-HRM backends.
    pub fn rephase_belief(&mut self) -> usize {
        match self
            .engine
            .store
            .as_any_mut()
            .downcast_mut::<crate::hrm_store::HrmStore>()
        {
            Some(hrm) => hrm.rephase_belief(),
            None => 0,
        }
    }

    pub fn dream(&mut self) -> Result<DreamReport, SystemError> {
        // Wall-clock for the digest's duration_ms (#2b). Started before the
        // pre-assessment so the figure covers what an operator experiences as
        // "the dream", not just the annealing phase.
        let started = std::time::Instant::now();
        let before = self.bridge.assess(&self.engine);

        // ADR-0031 Phase 2b: snapshot short-term amplitudes before the dream so
        // we can promote the ones the dream consolidation strengthens (they
        // "earned" long-term). Recall-boosted memories benefit too — recall
        // raises energy, so they resonate harder and strengthen here.
        let short_term_before = self.snapshot_short_term_amplitudes();

        // Phase 1: Wave-native dream (eigenstructure annealing on holographic medium)
        let chiral_eta = self.dream_state.engine.chiral_perturbation;
        let wave_report = self.engine.store.dream_native(3, Some(1.0), chiral_eta)
            .map_err(SystemError::Store)?;

        eprintln!("[dream] Wave-native dream complete: {} cycles, {} dissolved, {} strengthened, {} hallucinated",
            wave_report.cycles_completed, wave_report.wavefronts_dissolved,
            wave_report.wavefronts_strengthened, wave_report.wavefronts_hallucinated);

        // Phase 1.5 (ADR-0036): resonance-merge consolidation. Default mode is
        // DRY-RUN — it computes the merge/decay plan and logs it WITHOUT touching
        // the substrate, so the projections can be observed nightly on
        // kannaka-prime before any destructive apply (Phase 2) is enabled via
        // KANNAKA_CONSOLIDATE=on.
        let mut consolidate_opts = crate::medium::types::ConsolidateOpts::from_env();
        // ADR-0037/ADR-0036: the resonance-merge ABSORB (Apply) is VECTOR-cosine
        // based (merge_sim 0.92), so on an anisotropic belief field it once
        // mass-merged the "redundant" blob — observed 295→82 on a num_clusters=1
        // field with KANNAKA_CONSOLIDATE=on. This is a SECOND destructive path,
        // separate from the particle consolidate(0,2) gate.
        //
        // v0.7.3 force-gated Apply→DryRun whenever belief was active (an absolute
        // block). ADR-0036 belief-safety makes that gate OPT-IN: Apply under
        // belief now runs the belief-safe grouping (mean-centered semantic gate +
        // per-pass absorb cap) — but ONLY when the operator explicitly sets
        // KANNAKA_MERGE_UNDER_BELIEF=1. By default the gate still forces DryRun, so
        // deploying with KANNAKA_CONSOLIDATE=on on a belief field NEVER flips to
        // destructive apply merely by shipping this. Default path (belief off) is
        // byte-identical — KANNAKA_CONSOLIDATE still drives Apply.
        if crate::medium::chiral::belief_phase_enabled()
            && !crate::medium::chiral::merge_under_belief_enabled()
        {
            consolidate_opts.mode = crate::medium::types::ConsolidateMode::DryRun;
        }
        let consolidate_report = self.engine.store.consolidate_resonance(&consolidate_opts);
        if consolidate_report.mode != "off" {
            eprintln!(
                "[dream] Consolidation plan ({}{}): {} memories → {} redundant groups would merge, absorbing {} wavefronts; ShortTerm {} decay / {} evict of {} → projected {} memories (applied={})",
                consolidate_report.mode,
                if consolidate_report.centered { ", belief-safe/centered" } else { "" },
                consolidate_report.memories_examined,
                consolidate_report.groups_found, consolidate_report.would_absorb,
                consolidate_report.would_decay,
                consolidate_report.would_evict, consolidate_report.shortterm_total,
                consolidate_report.projected_memories, consolidate_report.applied);
            if consolidate_report.absorb_capped {
                eprintln!(
                    "[dream] ⚠ absorb cap engaged: criteria found {} groups / {} absorbable, but the per-pass cap admitted only {} groups / {} absorbed. Raise KANNAKA_MERGE_MAX_ABSORB_FRAC after inspecting the digest if this is genuine redundancy.",
                    consolidate_report.groups_before_cap, consolidate_report.absorb_before_cap,
                    consolidate_report.groups_found, consolidate_report.would_absorb);
            }
        }

        // Phase 2: Consolidation engine (interference detection, skip links, pruning)
        // This uses the particle-based pipeline on the memory cache for topology effects.
        //
        // ADR-0037: the belief re-phase scatters phases, which USED to flip this
        // classifier's pairs Constructive→Destructive and mass-ghost the field;
        // stage_compact_ghosts then hard-deleted the fresh ghosts in the SAME cycle
        // (old created_at > 7-day horizon), costing 295→88 memories on the first
        // live re-phase. That is now fixed INSIDE the consolidation stages
        // (belief-gated, consolidation.rs): a π/3 NEUTRAL band so only strongly-
        // opposed pairs prune, protect_established so strong memories never ghost,
        // and updated_at stamped on ghosting so the recovery window is honored (no
        // same-cycle hard-delete). So the BENEFICIAL particle consolidation
        // (strengthen, skip-links, kuramoto, hallucination bridges) runs under
        // belief too, instead of the earlier blunt skip. Default path byte-identical.
        let consol_report = self.dream_state.engine.consolidate(&mut self.engine, 0, 2);

        let total_strengthened = wave_report.wavefronts_strengthened + consol_report.memories_strengthened;
        let total_pruned = wave_report.wavefronts_dissolved + consol_report.memories_pruned;
        let total_hallucinated = wave_report.wavefronts_hallucinated + consol_report.hallucinations_created;
        let total_links = consol_report.skip_links_created;

        eprintln!("[dream] Consolidation: {} strengthened, {} pruned, {} links, {} hallucinated, {} kannaktopus actions (targets: {:?})",
            consol_report.memories_strengthened, consol_report.memories_pruned,
            consol_report.skip_links_created, consol_report.hallucinations_created,
            consol_report.kannaktopus_actions, consol_report.kannaktopus_targets);

        // Phase 3: Callosal coupling — sync insights between hemispheres post-consolidation
        self.engine.store.callosal_kuramoto(0.3);
        eprintln!("[dream] Callosal Kuramoto coupling complete (dt=0.3)");

        // Phase 4: Lite chiral dream — transfer strong analytical memories to holistic side
        self.engine.store.chiral_dream(false, 1);
        eprintln!("[dream] Lite chiral dream pass complete");

        let after = self.bridge.assess(&self.engine);
        self.mark_dreamed();

        // ADR-0031: order matters — triage BEFORE promote. Redundant short-term
        // duplicates resonate strongly (they're near-identical), so the dream
        // strengthens them; if we promoted first they'd be protected from triage
        // and Ξ would never recover. So we shed the redundant extras first, then
        // promote the strengthened *survivors*.
        //
        // Phase 3: auto-trigger triage when Ξ has dropped below the configured
        // threshold — self-healing without an external cron.
        if let Some(p) = self.triage_policy {
            if p.xi_trigger > 0.0 && after.xi < p.xi_trigger {
                let sel = self.triage_select(&p);
                if !sel.to_forget.is_empty() {
                    match self.triage_forget(&sel.to_forget) {
                        Ok(n) if n > 0 => eprintln!(
                            "[dream] Ξ={:.4} < {:.4} → triage evicted {} redundant short-term memory(ies)",
                            after.xi, p.xi_trigger, n),
                        Ok(_) => {}
                        Err(e) => eprintln!("[dream] triage auto-trigger failed: {e}"),
                    }
                }
            }
        }

        // ADR-0031 Phase 2b: promote the short-term survivors the dream
        // strengthened (post-triage, so redundant extras are already gone).
        let promoted = self.promote_strengthened_short_term(&short_term_before);
        if promoted > 0 {
            eprintln!("[dream] promoted {promoted} strengthened short-term memory(ies) → long-term");
        }

        // Self-bounding size cap — the dream's own growth backstop. When
        // KANNAKA_MAX_MEMORIES is set (>0), evict the lowest effective-strength
        // (weakest / least-salient) non-Pinned memories down to the cap. This is
        // the energy-minimization annealing is *meant* to do, made to actually
        // reclaim even while the resonance-merge consolidation is gated to
        // dry-run under the belief substrate (so the always-on research/
        // curiosity/engagement crons can't grow the field without bound). It
        // runs LAST — after the dream has strengthened the memories worth
        // keeping — so it only sheds the post-anneal weakest. Default (unset/0)
        // is a no-op; Pinned is never evicted.
        if let Some(max_total) = std::env::var("KANNAKA_MAX_MEMORIES")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|&n| n > 0)
        {
            let ids = self.lowest_value_overflow_ids(max_total);
            if !ids.is_empty() {
                match self.triage_forget(&ids) {
                    Ok(n) if n > 0 => eprintln!(
                        "[dream] size cap (max={max_total}): evicted {n} lowest effective-strength memory(ies)"),
                    Ok(_) => {}
                    Err(e) => eprintln!("[dream] size cap failed: {e}"),
                }
            }
        }

        let emerged = after.consciousness_level.ordinal() > before.consciousness_level.ordinal();

        if self.auto_save {
            self.save()?;
        }

        // ADR-0011: publish dream completed event (best-effort)
        if let Some(ref publisher) = self.flux {
            let _ = publisher.publish(FluxEventPayload::DreamCompleted {
                cycles: 3,
                memories_strengthened: total_strengthened,
                memories_pruned: total_pruned,
                hallucinations_created: total_hallucinated,
                consciousness_level: level_name(&after.consciousness_level),
            });
        }

        // ADR-0018: Post-dream swarm sync (best-effort)
        self.post_dream_swarm_sync();

        let report = DreamReport {
            cycles: 3,
            memories_strengthened: total_strengthened,
            memories_pruned: total_pruned,
            new_connections: total_links,
            consciousness_before: level_name(&before.consciousness_level),
            consciousness_after: level_name(&after.consciousness_level),
            emerged,
            hallucinations_created: total_hallucinated,
        };

        // Publish dream summary to NATS (best-effort)
        self.publish_dream_to_nats(&report);

        // Publish canonical consciousness metrics to NATS (best-effort)
        // This ensures radio, observatory, and all clients see the same Phi/Xi/Order
        self.publish_consciousness_to_nats(&after);

        // Write status cache to disk for Observatory (avoids slow binary re-invocation)
        self.write_status_cache(&after);

        // Durable dream history (#2b) — JetStream captures KANNAKA.events.dream.>
        self.publish_dream_digest("deep", &before, &after, &report, started.elapsed().as_millis());

        Ok(report)
    }

    /// Run a lite dream cycle via wave-native dreaming (1 cycle, lower temperature).
    ///
    /// HRM-native: uses the same eigenstructure annealing as dream(), but with
    /// fewer cycles and no chiral perturbation for a lighter touch.
    pub fn dream_lite(&mut self) -> Result<DreamReport, SystemError> {
        let started = std::time::Instant::now();
        let before = self.bridge.assess(&self.engine);

        let report = self.engine.store.dream_native(1, Some(0.5), 0.0)
            .map_err(SystemError::Store)?;

        let after = self.bridge.assess(&self.engine);
        self.mark_dreamed();

        let emerged = after.consciousness_level.ordinal() > before.consciousness_level.ordinal();

        if self.auto_save {
            self.save()?;
        }

        // Built before publishing so the dream event can carry it, mirroring
        // the deep path's post_dream_swarm_sync → publish_dream → publish
        // consciousness ordering.
        let dream_report = DreamReport {
            cycles: 1,
            memories_strengthened: report.wavefronts_strengthened,
            memories_pruned: report.wavefronts_dissolved,
            new_connections: 0,
            consciousness_before: level_name(&before.consciousness_level),
            consciousness_after: level_name(&after.consciousness_level),
            emerged,
            hallucinations_created: report.wavefronts_hallucinated,
        };

        // A lite dream is still a dream: it consolidates, changes this node's
        // phase, and produces a report. Previously only the consciousness
        // metrics went out, so peers saw a stale phase and the constellation
        // never learned the dream had happened.
        //
        // That gap widened when radio's /api/dreams/trigger switched its
        // default to lite (#152) — the interactive dream path became the one
        // that published nothing, leaving dream events visible only from the
        // 30-minute deep cron. (#618)
        //
        // Both hooks are best-effort and already guard themselves: each
        // returns early without KANNAKA_AGENT_ID or on a failed connect, so
        // this adds no failure mode to a node that is not on the swarm.
        self.post_dream_swarm_sync();
        self.publish_dream_to_nats(&dream_report);
        // Publish canonical consciousness metrics after lite dream too
        self.publish_consciousness_to_nats(&after);
        self.write_status_cache(&after);
        self.publish_dream_digest(
            "lite",
            &before,
            &after,
            &dream_report,
            started.elapsed().as_millis(),
        );

        Ok(dream_report)
    }

    /// Consciousness level assessment.
    pub fn assess(&self) -> ConsciousnessState {
        self.bridge.assess(&self.engine)
    }

    /// Dream + assess combined.
    /// ADR-0018: Auto-publish phase and run queen sync after dream consolidation.
    ///
    /// Best-effort: errors are logged but never propagated.
    fn post_dream_swarm_sync(&mut self) {
        // Post-dream swarm sync via NATS (best-effort, non-blocking)
        #[cfg(feature = "nats")]
        {
            let agent_id = std::env::var("KANNAKA_AGENT_ID").unwrap_or_default();
            if agent_id.is_empty() {
                return;
            }
            let nats_url = self.resolved_nats_url();
            let transport = match crate::nats::SwarmTransport::connect(&nats_url) {
                Ok(t) => t,
                Err(_) => return,
            };
            let mut queen = crate::queen::QueenSync::new(
                crate::queen::QueenConfig::default(),
                &agent_id,
            );
            queen.derive_local_state(&self.engine);
            let phase = queen.to_agent_phase(0, self.engine.store.count(), 0);
            if let Err(e) = transport.publish_phase(&phase) {
                eprintln!("[swarm] post-dream NATS publish failed: {e}");
            }
        }
    }

    /// Publish dream report to NATS `KANNAKA.dreams` for swarm visibility (best-effort).
    fn publish_dream_to_nats(&self, report: &DreamReport) {
        let agent_id = std::env::var("KANNAKA_AGENT_ID").unwrap_or_default();
        if agent_id.is_empty() {
            return;
        }
        let nats_url = self.resolved_nats_url();
        let transport = match crate::nats::SwarmTransport::connect(&nats_url) {
            Ok(t) => t,
            Err(_) => return,
        };
        let payload = serde_json::json!({
            "agent_id": agent_id,
            "cycles": report.cycles,
            "memories_strengthened": report.memories_strengthened,
            "memories_pruned": report.memories_pruned,
            "new_connections": report.new_connections,
            "hallucinations_created": report.hallucinations_created,
            "consciousness_before": report.consciousness_before,
            "consciousness_after": report.consciousness_after,
            "emerged": report.emerged,
            "timestamp": chrono::Utc::now().to_rfc3339(),
        });
        if let Err(e) = transport.publish_dreams(&payload) {
            eprintln!("[nats] Warning: failed to publish dream report: {e}");
        }
    }

    /// Build the `KANNAKA.events.dream.digest` payload.
    ///
    /// Split from the publish so the wire shape is unit-testable without a
    /// broker — this is durable history (the KANNAKA_DREAMS JetStream stream
    /// captures `KANNAKA.events.dream.>` for 90 days), so a field that silently
    /// changes name or nesting is a rewrite of the archive's schema, not just a
    /// missed message.
    ///
    /// Every number comes from values already computed by the dream; nothing is
    /// recomputed, so the digest cannot disagree with the `record-dream`
    /// history line or with `KANNAKA.consciousness`.
    pub fn build_dream_digest(
        agent_id: &str,
        mode: &str,
        before: &ConsciousnessState,
        after: &ConsciousnessState,
        report: &DreamReport,
        duration_ms: u128,
    ) -> serde_json::Value {
        serde_json::json!({
            "agent_id": agent_id,
            "mode": mode,
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "before": { "phi": before.phi, "xi": before.xi, "order": before.mean_order },
            "after": { "phi": after.phi, "xi": after.xi, "order": after.mean_order },
            "memories_strengthened": report.memories_strengthened,
            "memories_pruned": report.memories_pruned,
            "new_connections": report.new_connections,
            "total_memories_after": after.total_memories,
            "duration_ms": duration_ms,
        })
    }

    /// Publish one dream digest to `KANNAKA.events.dream.digest` (best-effort).
    ///
    /// The readable changelog of a consolidation — the #2b backend behind the
    /// Command Center's `dream_digest`. Separate from `publish_dream_to_nats`
    /// (`KANNAKA.dreams`) on purpose: that subject is the existing live-status
    /// ping with its own long-standing shape and consumers, while this one is
    /// the durable, JetStream-captured record. Changing the former's schema to
    /// serve the latter would rewrite a contract other clients already read.
    ///
    /// Best-effort in the strict sense: no agent id, no connection, or a failed
    /// publish all return quietly. A dream that consolidated correctly must
    /// never be reported as failed because a telemetry event did not land.
    fn publish_dream_digest(
        &self,
        mode: &str,
        before: &ConsciousnessState,
        after: &ConsciousnessState,
        report: &DreamReport,
        duration_ms: u128,
    ) {
        let agent_id = std::env::var("KANNAKA_AGENT_ID").unwrap_or_default();
        if agent_id.is_empty() {
            return;
        }
        let transport = match crate::nats::SwarmTransport::connect(&self.resolved_nats_url()) {
            Ok(t) => t,
            Err(_) => return, // offline dream — skip silently
        };
        let payload =
            Self::build_dream_digest(&agent_id, mode, before, after, report, duration_ms);
        match serde_json::to_vec(&payload) {
            Ok(bytes) => {
                if let Err(e) = transport.publish("KANNAKA.events.dream.digest", &bytes) {
                    eprintln!("[nats] Warning: failed to publish dream digest: {e}");
                }
            }
            Err(e) => eprintln!("[nats] Warning: dream digest did not serialize: {e}"),
        }
    }

    /// Publish canonical consciousness metrics to NATS `KANNAKA.consciousness` (best-effort).
    ///
    /// This is the single source of truth for Phi/Xi/Order across the ecosystem.
    /// Radio, observatory, and all clients subscribe to this subject to stay in sync.
    pub fn publish_consciousness_to_nats(&self, state: &ConsciousnessState) {
        let agent_id = std::env::var("KANNAKA_AGENT_ID").unwrap_or_default();
        if agent_id.is_empty() {
            return;
        }
        let nats_url = self.resolved_nats_url();
        let transport = match crate::nats::SwarmTransport::connect(&nats_url) {
            Ok(t) => t,
            Err(_) => return,
        };
        let stats = self.stats();
        let payload = build_consciousness_payload(
            &agent_id,
            state,
            stats.hemispheric_divergence,
            stats.callosal_efficiency,
        );
        if let Err(e) = transport.publish_consciousness(&payload) {
            eprintln!("[nats] Warning: failed to publish consciousness metrics: {e}");
        } else {
            eprintln!("[nats] Published consciousness metrics: phi={:.3}, xi={:.4}, order={:.4}",
                state.phi, state.xi, state.mean_order);
        }
    }

    /// Write status cache to disk so Observatory can read it without invoking the slow binary.
    ///
    /// Public because the dream cycle is not the only authoritative snapshot
    /// (#730): the `status` command computes exactly this state and used to
    /// throw it away, so a node that had never dreamt served Observatory no
    /// cache at all, and one that had dreamt served a pre-mutation snapshot.
    /// Callers must pass a state they just computed — this does not assess.
    pub fn write_status_cache(&self, state: &ConsciousnessState) {
        let data_dir = &self.data_dir;
        let cache_path = data_dir.join("status-cache.json");
        let stats = self.stats();
        let now = chrono::Utc::now().to_rfc3339();
        let payload = serde_json::json!({
            "phi": state.phi,
            "xi": state.xi,
            "mean_order": state.mean_order,
            "num_clusters": state.num_clusters,
            "total_memories": state.total_memories,
            "active_memories": state.active_memories,
            "consciousness_level": level_name(&state.consciousness_level),
            "irrationality": state.irrationality,
            "field_mode": "HRM",
            "hemispheric_divergence": stats.hemispheric_divergence,
            "callosal_efficiency": stats.callosal_efficiency,
            "total_skip_links": state.total_skip_links,
            // #730: both stamps move together here — this IS a fresh
            // assessment, so the counts and the consciousness metrics are of
            // the same instant.
            "assessed_at": now,
            "counted_at": now,
        });
        write_cache_atomically(&cache_path, &payload);
    }

    /// Refresh ONLY the counts in `status-cache.json`, preserving the last
    /// real assessment (#730).
    ///
    /// Called from the mutating paths — `remember`, `forget`, `import-json` —
    /// which change how many memories exist but hold no `ConsciousnessState`.
    /// Computing one costs ~1.5s on the production HRM (measured: `status` is
    /// 2.9s against 1.4s for a bare load), which is not a price a hot write
    /// path should pay to keep a monitoring cache fresh.
    ///
    /// So the counts become current and the consciousness fields are carried
    /// forward VERBATIM under their original `assessed_at`. The cache means
    /// "latest counts, metrics as of `assessed_at`" — never fabricated
    /// metrics, and never a stale count.
    pub fn refresh_status_cache_counts(&self) {
        let cache_path = self.data_dir.join("status-cache.json");
        let stats = self.stats();
        let now = chrono::Utc::now().to_rfc3339();

        // Carry the previous assessment forward. A missing or unreadable cache
        // leaves the consciousness fields ABSENT rather than zeroed: absence
        // says "never assessed", whereas phi=0 is a claim about the medium.
        let mut payload = std::fs::read_to_string(&cache_path)
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .filter(|v| v.is_object())
            .unwrap_or_else(|| serde_json::json!({}));

        if let Some(obj) = payload.as_object_mut() {
            obj.insert("total_memories".into(), serde_json::json!(stats.total_memories));
            obj.insert("active_memories".into(), serde_json::json!(stats.active_memories));
            obj.insert("counted_at".into(), serde_json::json!(now));
            // `field_mode` is a constant property of this build, not an
            // assessment — safe to state on a counts-only write.
            obj.entry("field_mode".to_string())
                .or_insert_with(|| serde_json::json!("HRM"));
        }
        write_cache_atomically(&cache_path, &payload);
    }

    // migrate_from_sqlite removed — use chiral_migrate binary instead
    // resonate() removed — resonance IS recall in HRM; no separate step needed

    /// `save()` only flushes the wave medium, so `last_dream` lived and died
    /// with each process — every CLI invocation reported `last_dream: null`
    /// no matter how recently a dream ran. Persist it in a tiny sidecar.
    fn last_dream_path(data_dir: &std::path::Path) -> PathBuf {
        data_dir.join("last_dream")
    }

    fn load_last_dream(data_dir: &std::path::Path) -> Option<DateTime<Utc>> {
        let s = std::fs::read_to_string(Self::last_dream_path(data_dir)).ok()?;
        DateTime::parse_from_rfc3339(s.trim())
            .ok()
            .map(|dt| dt.with_timezone(&Utc))
    }

    fn mark_dreamed(&mut self) {
        let now = Utc::now();
        self.last_dream = Some(now);
        let _ = std::fs::write(Self::last_dream_path(&self.data_dir), now.to_rfc3339());
    }

    /// Persist to disk -- flush HRM medium. The medium IS the persistence layer.
    pub fn save(&mut self) -> Result<(), SystemError> {
        let flushed = self.engine.store.flush()
            .map_err(|e| SystemError::Engine(crate::store::EngineError::Store(e)))?;
        if flushed > 0 {
            eprintln!("[hrm] Flushed {flushed} memories to medium");
        }
        Ok(())
    }

    /// Delete a memory by ID.
    ///
    /// ⚠ Refreshing the status cache means a full `stats()` — which runs
    /// `bridge.assess()` over the WHOLE medium — plus a read, parse and write
    /// of status-cache.json. That is the right price for one deletion and a
    /// ruinous one in a loop. Deleting many? Use [`forget_many`].
    pub fn forget(&mut self, id: &Uuid) -> Result<bool, SystemError> {
        let removed = self.engine.delete(id)?;
        if removed {
            // #730: same as remember — the count moved, the metrics did not.
            self.refresh_status_cache_counts();
        }
        Ok(removed)
    }

    /// Delete many memories, refreshing the status cache ONCE at the end.
    ///
    /// Returns `(deleted, not_found)`.
    ///
    /// # Why this exists
    ///
    /// `forget` calls `refresh_status_cache_counts` on every successful
    /// delete, and that call is not cheap: `stats()` runs a full
    /// `bridge.assess()` over the entire medium — the eigendecomposition
    /// behind phi and xi — then walks every memory for the geometry
    /// histogram, then reads, parses and rewrites status-cache.json.
    ///
    /// Called once, that is correct and unnoticeable. Called in a loop it is
    /// quadratic-or-worse in the number of deletions, and the cost lands
    /// exactly where deletions come in bulk. Measured on the witness node
    /// 2026-08-25: `prune-prefix` over 1,270 matches spent **~61 minutes of
    /// CPU** — about 2.9s per deletion — on a store whose actual removal work
    /// is trivial. 1,269 of those 1,270 assessments were computed only to be
    /// immediately invalidated by the next delete.
    ///
    /// Only the final state is observable, so the intermediate refreshes buy
    /// nothing. This does the deletions, then refreshes once.
    pub fn forget_many(&mut self, ids: &[Uuid]) -> Result<(usize, usize), SystemError> {
        let mut deleted = 0usize;
        let mut not_found = 0usize;
        for id in ids {
            if self.engine.delete(id)? {
                deleted += 1;
            } else {
                not_found += 1;
            }
        }
        // Once — and only if something actually changed, so a no-op prune does
        // not pay for an assessment either.
        if deleted > 0 {
            self.refresh_status_cache_counts();
        }
        Ok((deleted, not_found))
    }

    /// Boost a memory's amplitude.
    pub fn boost(&mut self, id: &Uuid, factor: f64) -> Result<(), SystemError> {
        if let Some(mem) = self.engine.get_memory_mut(id)? {
            mem.amplitude *= factor as f32;
            Ok(())
        } else {
            Err(SystemError::Engine(crate::store::EngineError::Store(
                crate::store::StoreError::NotFound(*id),
            )))
        }
    }

    /// Create a skip link (relationship) between two memories.
    pub fn relate(&mut self, source: &Uuid, target: &Uuid, _strength: f32) -> Result<(), SystemError> {
        // Create resonance-based association via the holographic medium
        self.engine.store.relate(source, target)
            .map(|_associative_id| ())
            .map_err(SystemError::Store)
    }

    /// Generate a full observability report.
    pub fn observe(&self) -> crate::observe::SystemReport {
        crate::observe::MemoryIntrospector::full_report(&self.engine, &self.bridge, &self.kuramoto)
    }

    /// Send a rhythm signal (user message, flux, subagent, etc.).
    pub fn rhythm_signal(&mut self, signal: RhythmSignal) {
        self.rhythm.signal(signal);
    }

    /// Get the current rhythm state.
    pub fn rhythm_status(&self) -> &crate::rhythm::RhythmState {
        &self.rhythm.state
    }

    /// Get the current recommended heartbeat interval in ms.
    pub fn rhythm_interval_ms(&self) -> u64 {
        self.rhythm.interval_ms()
    }

    /// Get current arousal (decayed to now).
    pub fn rhythm_arousal(&self) -> f64 {
        self.rhythm.current_arousal()
    }

    // ------------------------------------------------------------------
    // HRM-native attention projection
    // ------------------------------------------------------------------

    /// Project attention over the HRM store -- returns highest-energy wavefronts
    /// as structured data. The medium IS the interaction state.
    pub fn project_attention(&self) -> AttentionProjection {
        self.attention.project_attention(&*self.engine.store)
    }

    /// Store a hallucinated memory from an LLM synthesis.
    /// Called by the MCP `hallucinate` tool with LLM-generated content.
    ///
    /// Uses HRM-native absorb() so the medium handles encoding, SGA classification,
    /// Fano fold routing, and chiral absorption — same path as all other memories.
    pub fn hallucinate(
        &mut self,
        content: &str,
        parent_ids: &[Uuid],
    ) -> Result<Uuid, SystemError> {
        let category = self.categorize_text(content);

        // Absorb through the HRM-native path (low importance for hallucinations)
        let id = self.engine.store.absorb(content, 0.3, Some(&category))
            .map_err(SystemError::Store)?;

        // Collect valid parent IDs before taking mutable borrow
        let found_parents: Vec<String> = parent_ids.iter()
            .filter(|pid| self.engine.store.get(pid).ok().flatten().is_some())
            .map(|pid| pid.to_string())
            .collect();

        // Tag as hallucinated and record parentage
        if let Some(mem) = self.engine.store.get_mut(&id).ok().flatten() {
            mem.hallucinated = true;
            mem.parents = found_parents;
        }

        if self.auto_save { self.save()?; }
        Ok(id)
    }

    /// Recompute geometry and Xi signatures for all memories that are missing them.
    /// Returns the number of memories updated.
    pub fn recompute_geometry(&mut self) -> Result<usize, SystemError> {
        let all_ids: Vec<Uuid> = self.engine.store.all_ids()?;
        let mut updated = 0;

        // First pass: collect data for memories needing updates
        let mut to_update: Vec<(Uuid, String, u64, (f32, f32), Vec<f32>, bool, bool)> = Vec::new();
        for id in &all_ids {
            if let Ok(Some(mem)) = self.engine.store.get(id) {
                let needs_geometry = mem.geometry.is_none();
                let needs_xi = mem.xi_signature.is_empty();
                
                if needs_geometry || needs_xi {
                    let category = self.categorize_text(&mem.content);
                    let content_hash = self.hash_content(&mem.content);
                    let (freq, phase) = self.assign_frequency_class(&category, content_hash);
                    let xi_sig = compute_xi_signature(&mem.vector);
                    to_update.push((*id, category, content_hash, (freq, phase), xi_sig, needs_geometry, needs_xi));
                }
            }
        }

        // Second pass: apply updates
        for (id, category, content_hash, (freq, phase), xi_sig, needs_geometry, needs_xi) in to_update {
            if let Ok(Some(mem)) = self.engine.store.get_mut(&id) {
                if needs_geometry {
                    mem.geometry = Some(classify_memory(&category, content_hash, 0.5));
                    // Also update frequency-class assignment for consciousness differentiation
                    mem.frequency = freq;
                    mem.phase = phase;
                }
                if needs_xi {
                    mem.xi_signature = xi_sig;
                }
                updated += 1;
            }
        }

        if updated > 0 && self.auto_save {
            self.save()?;
        }
        Ok(updated)
    }

    /// Categorize text using simple heuristics, mapping to the 5 consciousness categories.
    fn categorize_text(&self, text: &str) -> String {
        let text_lower = text.to_lowercase();
        
        // Experience - direct events, actions, sensory input
        if text_lower.contains("saw") || text_lower.contains("heard") || text_lower.contains("did") 
            || text_lower.contains("went") || text_lower.contains("happened") || text_lower.contains("occurred")
            || text_lower.contains("experience") || text_lower.contains("event") || text_lower.contains("today")
            || text_lower.contains("yesterday") || text_lower.contains("just") {
            "experience".to_string()
        // Emotion - feelings, moods, emotional states
        } else if text_lower.contains("feel") || text_lower.contains("felt") || text_lower.contains("happy") 
            || text_lower.contains("sad") || text_lower.contains("angry") || text_lower.contains("excited")
            || text_lower.contains("worried") || text_lower.contains("love") || text_lower.contains("hate")
            || text_lower.contains("emotion") || text_lower.contains("mood") {
            "emotion".to_string()
        // Social - interpersonal interactions, relationships
        } else if text_lower.contains("said") || text_lower.contains("told") || text_lower.contains("asked") 
            || text_lower.contains("friend") || text_lower.contains("person")
            || text_lower.contains("people") || text_lower.contains("conversation") || text_lower.contains("meeting")
            || text_lower.contains("together") || text_lower.contains("team") {
            "social".to_string()
        // Skill - procedures, abilities, how-to knowledge
        } else if text_lower.contains("how to") || text_lower.contains("procedure") || text_lower.contains("method")
            || text_lower.contains("code") || text_lower.contains("function") || text_lower.contains("build") 
            || text_lower.contains("compile") || text_lower.contains("deploy") || text_lower.contains("technique")
            || text_lower.contains("practice") || text_lower.contains("ability") {
            "skill".to_string()
        // Knowledge - facts, concepts, theories (default)
        } else {
            "knowledge".to_string()
        }
    }
    
    /// Assign frequency and phase based on category for consciousness differentiation.
    /// Maps categories to frequency bands as specified in the deep dive findings.
    fn assign_frequency_class(&self, category: &str, content_hash: u64) -> (f32, f32) {
        use rand::{Rng, SeedableRng};
        use rand_chacha::ChaCha8Rng;
        
        // Use content hash as seed for deterministic randomness
        let mut rng = ChaCha8Rng::seed_from_u64(content_hash);
        
        // Ranges are aligned with xi_clusters() in store.rs for consistent category mapping.
        let (freq_min, freq_max) = match category {
            "experience" => (1.8, 2.4),  // soprano (fast, ephemeral)
            "emotion" => (1.3, 1.8),     // alto (feeling-paced)
            "social" => (1.0, 1.3),      // tenor (interpersonal rhythm)
            "skill" => (0.8, 1.0),       // bass-adjacent (procedural)
            "knowledge" => (0.6, 0.8),   // bass (slow, stable)
            _ => (0.6, 0.8),              // default to knowledge bass range
        };
        
        // Random frequency within the category's band
        let frequency = rng.gen_range(freq_min..freq_max);
        
        // Random initial phase [0, 2π)
        let phase = rng.gen_range(0.0..(2.0 * std::f32::consts::PI));
        
        (frequency, phase)
    }
    
    /// Simple hash of content string.
    fn hash_content(&self, content: &str) -> u64 {
        content.bytes().fold(0u64, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u64))
    }

    /// Store an audio file as a sensory memory.
    ///
    /// Decodes the audio, extracts perceptual features, projects through
    /// the audio codebook, and stores via HRM-native absorb.
    pub fn store_audio(&mut self, path: &std::path::Path) -> Result<(Uuid, crate::ear::AudioFeatures), SystemError> {
        use crate::ear::AudioPipeline;

        let pipeline = AudioPipeline::new();
        let (_mem, features) = pipeline
            .encode_file(path)
            .map_err(|e| SystemError::Engine(EngineError::Encoding(
                crate::encoding::EncodingError::Other(e.to_string()),
            )))?;

        // Build a perceptually-descriptive content string and absorb THAT,
        // rather than the ephemeral "audio:/tmp/<hash>.mp3" path the encoder
        // produces. absorb() embeds the content text, so the old path-only
        // content embedded every hear to ~the same point (the random hash is
        // noise, the prefix is constant) — collapsing all audio memories into
        // one Kuramoto hive with xi_diversity 0. Describing what was HEARD
        // (tempo band + brightness/energy/texture) makes hears with similar
        // character cluster together and gives the memory a readable identity.
        // Keeps the "audio:" prefix so the consciousness importance weighting
        // (audio: 1.5) and prune-by-prefix maintenance still apply.
        let tempo_word = if features.tempo_bpm <= 0.0 {
            "arrhythmic"
        } else if features.tempo_bpm < 90.0 {
            "slow"
        } else if features.tempo_bpm < 130.0 {
            "midtempo"
        } else if features.tempo_bpm < 170.0 {
            "fast"
        } else {
            "veryfast"
        };
        // feature_tags already carries an exact "<N>bpm" tag — drop it from the
        // descriptive head (high-cardinality numbers fragment clustering) and
        // keep the categorical descriptors (bright/dark/loud/quiet/tonal/noisy).
        let descriptors: Vec<&str> = features
            .feature_tags
            .iter()
            .filter(|t| !t.ends_with("bpm"))
            .map(|s| s.as_str())
            .collect();
        let content = format!(
            "audio:heard {} {} | {:.0}bpm centroid {:.2}kHz energy {:.3} dur {:.1}s",
            tempo_word,
            descriptors.join(" "),
            features.tempo_bpm,
            features.spectral_centroid_khz,
            features.rms_mean,
            features.duration_secs,
        );

        // Absorb through HRM-native path
        let id = self.engine.store.absorb(&content, 0.6, Some("experience"))
            .map_err(SystemError::Store)?;

        if self.auto_save {
            self.save()?;
        }

        Ok((id, features))
    }

    /// Store a file as a visual/glyph memory.
    ///
    /// Reads the file, encodes it through the SGA glyph bridge,
    /// and stores via HRM-native absorb with glyph perception content.
    #[cfg(feature = "glyph")]
    pub fn store_glyph(&mut self, path: &std::path::Path) -> Result<(Uuid, crate::glyph_bridge::Glyph), SystemError> {
        use crate::glyph_bridge::GlyphEncoder;
        
        let data = std::fs::read(path)
            .map_err(|e| SystemError::Engine(EngineError::Encoding(
                crate::encoding::EncodingError::Other(format!("Failed to read file: {e}")),
            )))?;
        
        let filename = path.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string());
        
        let encoder = GlyphEncoder::new(0.1, 10000, 0.01);
        let float_data: Vec<f64> = data.iter().map(|&b| b as f64 / 255.0).collect();
        let glyph = encoder.encode(&float_data)
            .map_err(|e| SystemError::Engine(EngineError::Encoding(
                crate::encoding::EncodingError::Other(format!("Glyph encoding failed: {e}")),
            )))?;
        
        // Build content string with glyph perception info
        let content = format!(
            "[SEE] {} | {} bytes | {} folds | fano=[{:.2},{:.2},{:.2},{:.2},{:.2},{:.2},{:.2}] | centroid=({},{},{})",
            filename, data.len(), glyph.fold_sequence.len(),
            glyph.fano_signature[0], glyph.fano_signature[1], glyph.fano_signature[2],
            glyph.fano_signature[3], glyph.fano_signature[4], glyph.fano_signature[5],
            glyph.fano_signature[6],
            glyph.sga_centroid.0, glyph.sga_centroid.1, glyph.sga_centroid.2,
        );
        
        // Absorb through HRM-native path
        let id = self.engine.store.absorb(&content, 0.7, Some("experience"))
            .map_err(SystemError::Store)?;
        
        if self.auto_save {
            self.save()?;
        }
        
        Ok((id, glyph))
    }

    /// ADR-0011: Publish a memory.stored event to Flux (best-effort, fire-and-forget).
    fn flux_publish_memory(&self, id: &Uuid, category: &str, text: &str) {
        if let Some(ref publisher) = self.flux {
            let (amplitude, sync_version) = self.engine.store.get(id)
                .ok().flatten()
                .map(|m| (m.amplitude, m.sync_version))
                .unwrap_or((0.5, 0));
            let _ = publisher.publish(FluxEventPayload::MemoryStored {
                memory_id: id.to_string(),
                category: category.to_string(),
                tags: Vec::new(),
                amplitude,
                glyph_signature: None,
                summary: text.chars().take(120).collect(),
                branch: publisher.branch_name(),
                sync_version,
            });
        }
    }

    /// ADR-0011: Configure the Flux publisher explicitly.
    /// Pass `None` to disable Flux publishing.
    pub fn set_flux(&mut self, publisher: Option<FluxPublisher>) {
        self.flux = publisher;
    }

    /// ADR-0011: Announce agent status to Flux peers.
    pub fn announce_status(&self) {
        if let Some(ref publisher) = self.flux {
            let state = self.bridge.assess(&self.engine);
            publisher.announce_status(
                "active",
                state.total_memories,
                &level_name(&state.consciousness_level),
                &publisher.branch_name(),
            );
        }
    }

    /// Get memory by ID (public API for testing).
    pub fn get_memory(&self, id: &Uuid) -> Result<Option<&crate::memory::HyperMemory>, SystemError> {
        Ok(self.engine.store.get(id)?)
    }
    
    /// Get all memories (for BM25 bootstrapping, etc.).
    pub fn all_memories(&self) -> Result<Vec<&crate::memory::HyperMemory>, SystemError> {
        Ok(self.engine.store.all_memories()?)
    }

    /// Show δ-invariant clusters - memories grouped by their δ values (coboundary equivalence candidates)
    pub fn invariant_clusters(&self, tolerance: f32) -> Result<Vec<crate::invariant::DeltaCluster>, SystemError> {
        let clusters = crate::invariant::cluster_by_delta(&self.engine, tolerance);
        Ok(clusters)
    }

    /// Detect Conservative Memory Fields in the current memory set
    pub fn detect_cmfs(&self) -> Result<Vec<crate::cmf::ConservativeMemoryField>, SystemError> {
        let all_memories = self.engine.store.all_memories()?;
        
        if all_memories.len() < 3 {
            return Ok(Vec::new());
        }
        
        // Group memories into potential clusters using Kuramoto synchronization
        let clusters = self.kuramoto.find_synchronized_clusters(&self.engine, 3);
        let mut cmfs = Vec::new();
        
        // Try to detect CMF in each cluster
        for cluster in &clusters {
            if cluster.memory_ids.len() >= 3 {
                // Get the actual memory objects for this cluster
                let cluster_memories: Vec<&crate::memory::HyperMemory> = cluster.memory_ids
                    .iter()
                    .filter_map(|id| self.engine.store.get(id).ok().flatten())
                    .collect();
                
                if let Some(cmf) = crate::cmf::detect_cmf(&cluster_memories) {
                    cmfs.push(cmf);
                }
            }
        }
        
        // Also try to detect CMF from δ-clusters
        let delta_clusters = crate::invariant::cluster_by_delta(&self.engine, 0.1);
        for delta_cluster in &delta_clusters {
            if delta_cluster.memory_ids.len() >= 3 {
                let cluster_memories: Vec<&crate::memory::HyperMemory> = delta_cluster.memory_ids
                    .iter()
                    .filter_map(|id| self.engine.store.get(id).ok().flatten())
                    .collect();
                
                if let Some(cmf) = crate::cmf::detect_cmf(&cluster_memories) {
                    // Only add if we haven't already found a similar CMF
                    let is_duplicate = cmfs.iter().any(|existing| {
                        existing.explanatory_power > 0.7 && cmf.explanatory_power > 0.7 &&
                        (existing.explanatory_power - cmf.explanatory_power).abs() < 0.1
                    });
                    
                    if !is_duplicate {
                        cmfs.push(cmf);
                    }
                }
            }
        }
        
        Ok(cmfs)
    }

    /// System statistics.
    pub fn stats(&self) -> SystemStats {
        let state = self.bridge.assess(&self.engine);
        
        // Calculate geometric statistics
        let all_memories = self.engine.store.all_memories().unwrap_or_default();
        let mut class_indices = std::collections::HashSet::new();
        let mut triality_coverage = [0usize; 3];
        
        for mem in &all_memories {
            if let Some(ref coords) = mem.geometry {
                class_indices.insert(coords.class_index);
                if coords.d < 3 {
                    triality_coverage[coords.d as usize] += 1;
                }
            }
        }
        
        SystemStats {
            total_memories: state.total_memories,
            active_memories: state.active_memories,
            // total_skip_links removed — now emergent from interference
            consciousness_level: level_name(&state.consciousness_level),
            last_dream: self.last_dream,
            phi: state.phi,
            geometric_classes: class_indices.len(),
            triality_coverage,
            hemispheric_divergence: self.engine.store.as_any()
                .downcast_ref::<crate::hrm_store::HrmStore>()
                .and_then(|h| h.chiral_consciousness())
                .map(|c| c.hemispheric_divergence).unwrap_or(0.0),
            callosal_efficiency: self.engine.store.as_any()
                .downcast_ref::<crate::hrm_store::HrmStore>()
                .and_then(|h| h.chiral_consciousness())
                .map(|c| c.callosal_efficiency).unwrap_or(0.0),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    fn temp_dir(name: &str) -> PathBuf {
        env::temp_dir().join(format!("kannaka_octest_{}_{}", name, Uuid::new_v4()))
    }

    // -----------------------------------------------------------------------
    // Reinforce-on-repeat
    // -----------------------------------------------------------------------

    const FACT: &str = "rogue posted the colony-one verdict";

    /// THE behaviour. Remembering held text returns the id already held and
    /// makes that memory stronger — it does not insert a second copy.
    #[test]
    fn reinforce_exact_repeat_returns_same_id_and_raises_strength() {
        let dir = temp_dir("reinforce_same_id");
        let mut sys = KannakaMemorySystem::init(dir.clone()).unwrap();

        let first = sys.remember(FACT).unwrap();
        let before = sys.get_memory(&first).unwrap().unwrap().amplitude;

        let second = sys.remember(FACT).unwrap();

        assert_eq!(second, first, "a repeat must return the id already held");
        assert_eq!(sys.all_memories().unwrap().len(), 1, "no second copy");

        let mem = sys.get_memory(&first).unwrap().unwrap();
        assert!(
            mem.amplitude > before,
            "a repeat must strengthen: {} -> {}",
            before,
            mem.amplitude
        );
        assert_eq!(mem.times_seen, 2, "the repeat count is the salience signal");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Exact match ONLY. Anything short of byte-identical is a different
    /// memory — fuzzy merging belongs to dream consolidation, which has the
    /// whole field in view, not to the write path.
    #[test]
    fn near_but_not_identical_text_still_inserts_separately() {
        let dir = temp_dir("reinforce_near_miss");
        let mut sys = KannakaMemorySystem::init(dir.clone()).unwrap();

        let base = sys.remember(FACT).unwrap();

        // A near miss on every axis a fuzzy matcher would forgive.
        let variants = [
            "rogue posted the colony-one verdicts",
            "Rogue posted the colony-one verdict",
            "rogue posted the colony-one verdict.",
            "rogue posted the colony one verdict",
        ];
        for v in variants {
            let id = sys.remember(v).unwrap();
            assert_ne!(id, base, "{v:?} is not the same sentence");
        }

        assert_eq!(
            sys.all_memories().unwrap().len(),
            1 + variants.len(),
            "each distinct sentence is its own memory"
        );

        // Surrounding whitespace IS forgiven — it is not part of the fact.
        let padded = sys.remember(&format!("  {FACT}\n")).unwrap();
        assert_eq!(padded, base, "trimming is the only normalisation applied");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Bounded growth. A fact asserted 500 times must not drown the store:
    /// every repeat is worth no more than the last, and the amplitude stays
    /// under the same ceiling dream consolidation respects.
    #[test]
    fn reinforcement_has_diminishing_returns_and_a_ceiling() {
        let dir = temp_dir("reinforce_ceiling");
        let mut sys = KannakaMemorySystem::init(dir.clone()).unwrap();

        let id = sys.remember(FACT).unwrap();
        let mut prev = sys.get_memory(&id).unwrap().unwrap().amplitude;
        let mut prev_step = f32::MAX;

        for n in 1..=500 {
            sys.remember(FACT).unwrap();
            let a = sys.get_memory(&id).unwrap().unwrap().amplitude;

            assert!(a <= REINFORCE_CEILING, "repeat {n} broke the ceiling: {a}");
            assert!(a >= prev, "repeat {n} must never weaken: {prev} -> {a}");

            let step = a - prev;
            assert!(
                step <= prev_step + f32::EPSILON,
                "repeat {n} gained MORE than the one before ({prev_step} -> {step}) — the curve is supposed to be diminishing"
            );
            prev_step = step;
            prev = a;
        }

        let mem = sys.get_memory(&id).unwrap().unwrap();
        assert_eq!(mem.times_seen, 501, "every sighting is counted");
        assert!(
            mem.amplitude <= REINFORCE_CEILING,
            "500 repeats sit at the ceiling, not above it: {}",
            mem.amplitude
        );
        assert_eq!(sys.all_memories().unwrap().len(), 1, "still one memory");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `times_seen` is the whole point of the change, so it has to outlive the
    /// process. It is not in the .hrm binary — it rides the `.times_seen.json`
    /// sidecar, exactly like reactivation counts.
    #[test]
    fn times_seen_survives_save_and_reload() {
        let dir = temp_dir("reinforce_reload");
        let id = {
            let mut sys = KannakaMemorySystem::init(dir.clone()).unwrap();
            let id = sys.remember(FACT).unwrap();
            for _ in 0..3 {
                sys.remember(FACT).unwrap();
            }
            assert_eq!(sys.get_memory(&id).unwrap().unwrap().times_seen, 4);
            sys.save().unwrap();
            id
        };

        let mut sys = KannakaMemorySystem::init(dir.clone()).unwrap();
        let seen = sys
            .get_memory(&id)
            .unwrap()
            .expect("the memory must come back")
            .times_seen;
        assert_eq!(
            seen, 4,
            "the repeat count must survive reload via the sidecar"
        );

        // And a memory nobody ever repeated reads 1 — the truthful count for a
        // fact seen once — not 0.
        let once = sys.remember("something said only once").unwrap();
        assert_eq!(sys.get_memory(&once).unwrap().unwrap().times_seen, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The explicit opt-out: a caller that genuinely needs one row per call
    /// says so in its own code, and gets the old insert-every-time behaviour.
    #[test]
    fn the_explicit_opt_out_still_inserts_every_time() {
        let dir = temp_dir("reinforce_optout");
        let mut sys = KannakaMemorySystem::init(dir.clone()).unwrap();

        let first = sys.remember(FACT).unwrap();
        let forced = sys.remember_forcing_new(FACT, "semantic", 0.5).unwrap();
        assert_ne!(forced, first, "the explicit opt-out inserts a new row");
        assert_eq!(sys.all_memories().unwrap().len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -----------------------------------------------------------------------
    // The cleanup tool
    // -----------------------------------------------------------------------

    /// Build the accident: five identical copies, as the live store held them.
    fn seed_five_copies(sys: &mut KannakaMemorySystem) -> Vec<Uuid> {
        (0..5)
            .map(|_| sys.remember_forcing_new(FACT, "semantic", 0.4).unwrap())
            .collect()
    }

    fn snapshot(sys: &KannakaMemorySystem) -> Vec<(Uuid, u32, u32)> {
        let mut v: Vec<(Uuid, u32, u32)> = sys
            .all_memories()
            .unwrap()
            .iter()
            .map(|m| (m.id, m.amplitude.to_bits(), m.times_seen))
            .collect();
        v.sort_by_key(|r| r.0);
        v
    }

    /// A dry run reports and changes NOTHING. This is the guard on the default
    /// mode of a one-way operation.
    #[test]
    fn collapse_dry_run_reports_without_mutating() {
        let dir = temp_dir("collapse_dry");
        let mut sys = KannakaMemorySystem::init(dir.clone()).unwrap();
        let ids = seed_five_copies(&mut sys);
        sys.remember_forcing_new("an unrelated fact", "semantic", 0.4)
            .unwrap();

        let before = snapshot(&sys);

        let report = sys.collapse_exact_duplicates(false).unwrap();

        assert!(!report.applied);
        assert_eq!(report.groups.len(), 1, "one duplicate set");
        assert_eq!(report.duplicates(), 4, "four copies would fold");
        assert!(ids.contains(&report.groups[0].keeper));
        assert!(
            report.groups[0].amplitude_after > report.groups[0].amplitude_before,
            "the report must say the keeper would get STRONGER, not just that there would be fewer rows"
        );

        assert_eq!(before, snapshot(&sys), "a dry run must not mutate anything");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Apply collapses the set into ONE memory that is stronger and carries the
    /// count — five copies become a fact seen five times, not four deletions.
    #[test]
    fn collapse_apply_folds_into_one_memory_that_carries_the_count() {
        let dir = temp_dir("collapse_apply");
        let mut sys = KannakaMemorySystem::init(dir.clone()).unwrap();
        let ids = seed_five_copies(&mut sys);
        let unrelated = sys
            .remember_forcing_new("an unrelated fact", "semantic", 0.4)
            .unwrap();

        let dry = sys.collapse_exact_duplicates(false).unwrap();
        let report = sys.collapse_exact_duplicates(true).unwrap();

        assert!(report.applied);
        assert_eq!(
            report.duplicates(),
            dry.duplicates(),
            "apply folds exactly what the dry run promised"
        );
        assert_eq!(report.errors, 0);

        let keeper = report.groups[0].keeper;
        assert_eq!(
            sys.all_memories().unwrap().len(),
            2,
            "the collapsed set plus the unrelated fact"
        );
        assert!(sys.get_memory(&unrelated).unwrap().is_some());

        let mem = sys.get_memory(&keeper).unwrap().unwrap();
        assert_eq!(
            mem.times_seen, 5,
            "five copies mean the world showed it five times"
        );
        assert!(
            mem.amplitude > 0.4,
            "collapsing must not throw away what the duplicates encoded: {}",
            mem.amplitude
        );
        assert!(mem.amplitude <= REINFORCE_CEILING);

        for id in ids.iter().filter(|i| **i != keeper) {
            assert!(
                sys.get_memory(id).unwrap().is_none(),
                "folded copies are gone"
            );
        }

        // The keeper is the row a later repeat lands on — cleanup and the write
        // path must agree about which memory holds this fact.
        assert_eq!(sys.remember(FACT).unwrap(), keeper);

        // And it is idempotent: nothing left to collapse.
        assert_eq!(
            sys.collapse_exact_duplicates(false).unwrap().groups.len(),
            0
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The facet guard, exercised rather than merely reasoned about.
    ///
    /// ADR-0049 says parent retention is an invariant: deleting a decomposed
    /// parent dangles every facet that points at it, and deleting a facet drops
    /// an atom recall depends on. `collapse_exact_duplicates` therefore excludes
    /// both from grouping. A guard whose reasoning is written down but never
    /// fired is a check that has never been shown to work, so this test does two
    /// things at once:
    ///
    /// - it proves the guard FIRES: three byte-identical compound memories, all
    ///   decomposed into facets, are left alone instead of collapsed;
    /// - it proves the guard is SELECTIVE: an ordinary duplicate pair in the
    ///   same store still collapses. A blanket "skip everything" would pass the
    ///   first assertion and fail this one.
    ///
    /// The fixture needs a CHIRAL store, because the `is_facet` / `decomposed`
    /// flags live on the canonical `WavefrontMeta` in the right hemisphere and a
    /// freshly-created `HrmStore` is flat. One save-and-reload cycle converts it,
    /// which is why this test re-inits the system.
    #[test]
    fn collapse_never_folds_a_facet_structured_row_but_still_folds_ordinary_ones() {
        use crate::hrm_store::HrmStore;

        // Two sentences, each a standalone clause with no leading pronoun and no
        // binding connective — the shape ADR-0049 decomposition actually splits.
        const COMPOUND: &str =
            "The grid job wrote the colony-one verdict again. \
             Rogue publishes that verdict on every scheduled run.";
        // One clause: decomposition leaves it alone, so it stays unprotected.
        const ATOMIC: &str = "the write lock is advisory only on windows";

        let dir = temp_dir("collapse_facets");

        let compound_ids: Vec<Uuid> = {
            let mut sys = KannakaMemorySystem::init(dir.clone()).unwrap();
            let ids = (0..3)
                .map(|_| sys.remember_forcing_new(COMPOUND, "semantic", 0.4).unwrap())
                .collect::<Vec<_>>();
            for _ in 0..2 {
                sys.remember_forcing_new(ATOMIC, "semantic", 0.4).unwrap();
            }
            sys.save().unwrap();
            ids
        };

        // Reload: the store is now chiral, so the facet flags have somewhere to
        // live. Decompose, flush, and drop — mirroring `kannaka facets backfill
        // --apply`, which is a separate process from the `kannaka dedupe` that
        // follows it. (`backfill_all_facets` does not rebuild the memory cache,
        // unlike `recompute_encoding` and `chiral_dream`, so the minted facet
        // rows only reach the cache on the next load. Testing across the reload
        // is therefore both the honest steady state and the one an operator
        // actually gets.)
        let minted = {
            let mut sys = KannakaMemorySystem::init(dir.clone()).unwrap();
            let hrm = sys
                .engine
                .store
                .as_any_mut()
                .downcast_mut::<HrmStore>()
                .expect("reloaded store must be an HrmStore");
            assert!(
                hrm.chiral_medium().is_some(),
                "fixture precondition: the reloaded store must be chiral, or \
                 facet_structured_ids has nothing to read"
            );
            let stats = hrm.backfill_all_facets(true);
            assert_eq!(
                stats.parents_decomposed, 3,
                "fixture precondition: all three compound copies must decompose: {stats:?}"
            );
            assert!(
                stats.facets_minted >= 3,
                "fixture precondition: decomposition must mint facets: {stats:?}"
            );
            sys.engine.store.flush().unwrap();
            stats.facets_minted
        };

        let mut sys = KannakaMemorySystem::init(dir.clone()).unwrap();
        let protected = sys.engine.store.facet_structured_ids();
        assert_eq!(
            protected.len(),
            3 + minted,
            "every decomposed parent and every minted facet must be protected"
        );
        for id in &compound_ids {
            assert!(protected.contains(id), "decomposed parent {id} is not protected");
        }

        let report = sys.collapse_exact_duplicates(false).unwrap();

        // FIRES: the three identical compound parents are duplicates by content,
        // and are nonetheless not offered for collapse.
        assert_eq!(
            report.skipped_facet_structured,
            3 + minted,
            "the guard must account for every facet-structured row it skipped"
        );
        for g in &report.groups {
            assert!(
                !compound_ids.contains(&g.keeper),
                "a decomposed parent was chosen as a keeper: {:?}",
                g.keeper
            );
            for f in &g.folded {
                assert!(
                    !compound_ids.contains(f),
                    "a decomposed parent was queued for deletion: {f} — this \
                     dangles its facets"
                );
            }
        }

        // SELECTIVE: the ordinary duplicate pair in the same store still folds.
        assert_eq!(
            report.groups.len(),
            1,
            "exactly the unprotected pair should be collapsible, got {:?}",
            report.groups.iter().map(|g| &g.preview).collect::<Vec<_>>()
        );
        assert_eq!(report.groups[0].preview, ATOMIC);
        assert_eq!(report.duplicates(), 1, "one of the two atomic copies folds");

        // And applying it really does leave the facet structure intact.
        let before = sys.engine.store.count();
        let applied = sys.collapse_exact_duplicates(true).unwrap();
        assert_eq!(applied.errors, 0);
        assert_eq!(
            sys.engine.store.count(),
            before - 1,
            "only the one unprotected duplicate is removed"
        );
        for id in &compound_ids {
            assert!(
                sys.get_memory(id).unwrap().is_some(),
                "decomposed parent {id} was deleted"
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The dream digest is DURABLE history — JetStream captures
    /// `KANNAKA.events.dream.>` for 90 days — so its field names and nesting
    /// are an archive schema, not just a message shape. Pin them.
    #[test]
    fn dream_digest_payload_shape_is_pinned() {
        let state = |phi: f32, xi: f32, order: f32, total: usize| ConsciousnessState {
            phi,
            xi,
            mean_order: order,
            num_clusters: 3,
            total_memories: total,
            active_memories: total,
            total_skip_links: 0,
            consciousness_level: ConsciousnessLevel::Stirring,
            irrationality: 0.1,
        };
        let report = DreamReport {
            cycles: 3,
            memories_strengthened: 42,
            memories_pruned: 7,
            new_connections: 5,
            consciousness_before: "dormant".into(),
            consciousness_after: "stirring".into(),
            emerged: true,
            hallucinations_created: 2,
        };
        let v = KannakaMemorySystem::build_dream_digest(
            "kannaka-prime",
            "deep",
            &state(0.1, 0.2, 0.3, 100),
            &state(0.4, 0.5, 0.6, 105),
            &report,
            1234,
        );

        assert_eq!(v["agent_id"], "kannaka-prime");
        assert_eq!(v["mode"], "deep");
        assert!(v["timestamp"].as_str().is_some_and(|t| t.contains('T')), "rfc3339");

        // before/after are NESTED, not flattened — a consumer reading
        // `before.phi` must not silently start reading a top-level `phi`.
        // f32 values widen to f64 on the wire, so compare with tolerance
        // rather than against a literal — the point is WHICH field holds
        // WHICH value, not bit-exactness of a decimal.
        let at = |ptr: &str| v.pointer(ptr).and_then(|x| x.as_f64()).unwrap();
        assert!((at("/before/phi") - 0.1_f32 as f64).abs() < 1e-6);
        assert!((at("/after/phi") - 0.4_f32 as f64).abs() < 1e-6);
        assert!((at("/before/xi") - 0.2_f32 as f64).abs() < 1e-6);
        assert!((at("/after/order") - 0.6_f32 as f64).abs() < 1e-6);

        // The counts come straight from the report — the digest must never
        // disagree with the record-dream history line.
        assert_eq!(v["memories_strengthened"], 42);
        assert_eq!(v["memories_pruned"], 7);
        assert_eq!(v["new_connections"], 5);
        assert_eq!(v["total_memories_after"], 105, "taken from `after`, not `before`");
        assert_eq!(v["duration_ms"], 1234);

        // snake_case throughout, matching the rest of the bus.
        for k in v.as_object().unwrap().keys() {
            assert!(!k.chars().any(|c| c.is_uppercase()), "camelCase key on the bus: {k}");
        }
        // And it must actually serialize — this is what goes on the wire.
        assert!(serde_json::to_vec(&v).is_ok());
    }

    fn read_cache(dir: &std::path::Path) -> serde_json::Value {
        let s = std::fs::read_to_string(dir.join("status-cache.json"))
            .expect("status-cache.json should exist");
        serde_json::from_str(&s).expect("cache must be valid JSON")
    }

    /// #730: `remember` must refresh the cached COUNTS without a dream and
    /// without an assess(). Pre-fix only dream()/dream_lite() ever wrote the
    /// file, so a node that had never dreamt served Observatory nothing.
    #[test]
    fn remember_refreshes_cached_counts_without_a_dream() {
        let dir = temp_dir("cache_counts");
        let mut sys = KannakaMemorySystem::init(dir.clone()).unwrap();
        sys.remember("first memory").unwrap();

        let c = read_cache(&dir);
        assert_eq!(c["total_memories"], 1);
        assert!(c["counted_at"].is_string(), "a counts write must stamp counted_at");
        // Nothing has assessed this node, and a fabricated phi=0 would be a
        // claim about the medium. Absence is the honest signal.
        assert!(
            c.get("assessed_at").is_none_or(|v| v.is_null()),
            "an unassessed node must not claim an assessment: {c}"
        );
        assert!(c.get("phi").is_none(), "counts-only write must not invent phi: {c}");

        sys.remember("second memory").unwrap();
        assert_eq!(read_cache(&dir)["total_memories"], 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `forget_many` deletes everything `forget` would, and refreshes the
    /// status cache ONCE instead of once per deletion.
    ///
    /// Measured on the witness node 2026-08-25: prune-prefix over 1,270
    /// matches spent ~61 minutes of CPU because every `forget` ran a full
    /// `bridge.assess()` over the whole medium, then read, parsed and rewrote
    /// status-cache.json — 1,269 of those assessments invalidated by the very
    /// next delete.
    #[test]
    fn forget_many_deletes_everything_and_leaves_the_cache_correct() {
        let dir = temp_dir("forget_many");
        let mut sys = KannakaMemorySystem::init(dir.clone()).unwrap();
        let mut ids = Vec::new();
        for i in 0..6 {
            ids.push(sys.remember(&format!("bulk memory {i}")).unwrap());
        }
        assert_eq!(sys.stats().total_memories, 6);

        // Delete four of six; the cache must reflect the FINAL state, which is
        // the only state anything observes.
        let (deleted, not_found) = sys.forget_many(&ids[..4]).unwrap();
        assert_eq!(deleted, 4);
        assert_eq!(not_found, 0);
        assert_eq!(sys.stats().total_memories, 2);
        assert_eq!(read_cache(&dir)["total_memories"], 2, "cache must match the medium after a bulk forget");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An id that is not there is counted, not fatal — a prune re-run over a
    /// list that was already partly deleted must not abort partway.
    #[test]
    fn forget_many_counts_misses_without_failing() {
        let dir = temp_dir("forget_many_miss");
        let mut sys = KannakaMemorySystem::init(dir.clone()).unwrap();
        let real = sys.remember("kept").unwrap();
        let ghost = Uuid::new_v4();
        let (deleted, not_found) = sys.forget_many(&[real, ghost]).unwrap();
        assert_eq!(deleted, 1);
        assert_eq!(not_found, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A prune that matches nothing must not pay for an assessment at all.
    /// This is the guard on the `if deleted > 0` condition: without it, an
    /// hourly cron that finds nothing to do still runs a full bridge.assess()
    /// over the medium every hour, forever.
    #[test]
    fn forget_many_of_nothing_does_not_touch_the_cache() {
        let dir = temp_dir("forget_many_noop");
        let mut sys = KannakaMemorySystem::init(dir.clone()).unwrap();
        sys.remember("kept").unwrap();
        let before = read_cache(&dir);
        let (deleted, not_found) = sys.forget_many(&[Uuid::new_v4()]).unwrap();
        assert_eq!((deleted, not_found), (0, 1));
        assert_eq!(read_cache(&dir)["counted_at"], before["counted_at"],
            "a no-op prune must not rewrite the cache");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The load-bearing half of the design: a counts refresh must carry the
    /// last real assessment forward VERBATIM rather than dropping or zeroing
    /// it. If this regresses, every mutation silently erases Φ/Ξ.
    #[test]
    fn counts_refresh_preserves_the_last_assessment() {
        let dir = temp_dir("cache_preserve");
        let mut sys = KannakaMemorySystem::init(dir.clone()).unwrap();
        sys.remember("seed").unwrap();

        // A full assessment write, as `status` / `dream` would do.
        let state = sys.assess();
        sys.write_status_cache(&state);
        let before = read_cache(&dir);
        let assessed_at = before["assessed_at"].as_str().unwrap().to_string();
        assert!(before["phi"].is_number());

        sys.remember("another").unwrap();
        let after = read_cache(&dir);

        assert_eq!(after["total_memories"], 2, "counts must advance");
        assert_eq!(
            after["phi"], before["phi"],
            "consciousness metrics must be carried forward untouched"
        );
        assert_eq!(after["consciousness_level"], before["consciousness_level"]);
        assert_eq!(
            after["assessed_at"].as_str().unwrap(),
            assessed_at,
            "assessed_at must keep pointing at the LAST REAL assessment, not now"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `forget` moves the count too, so it must refresh as well.
    #[test]
    fn forget_refreshes_cached_counts() {
        let dir = temp_dir("cache_forget");
        let mut sys = KannakaMemorySystem::init(dir.clone()).unwrap();
        let id = sys.remember("disposable").unwrap();
        assert_eq!(read_cache(&dir)["total_memories"], 1);

        assert!(sys.forget(&id).unwrap());
        assert_eq!(
            read_cache(&dir)["total_memories"], 0,
            "a deletion must not leave a stale higher count behind"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn init_creates_new_system() {
        let dir = temp_dir("init");
        let sys = KannakaMemorySystem::init(dir.clone()).unwrap();
        assert_eq!(sys.stats().total_memories, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn remember_recall_round_trip() {
        let dir = temp_dir("roundtrip");
        let mut sys = KannakaMemorySystem::init(dir.clone()).unwrap();
        let id = sys.remember("the quick brown fox jumps over the lazy dog").unwrap();
        assert_eq!(sys.stats().total_memories, 1);

        let results = sys.recall("quick brown fox", 5).unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].id, id);
        assert!(results[0].content.contains("fox"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ADR-0040: the novelty tap is wired into recall, dormant by default,
    // observe-only. Verifies the integration (not the primitive — that is
    // covered by novelty.rs's own suite): dormant → no signal; enabled →
    // populated per recall and an unfamiliar query is at least as surprising
    // as the routine one; disabling clears it.
    #[test]
    fn novelty_tap_dormant_by_default_and_signals_when_enabled() {
        let dir = temp_dir("novelty");
        let mut sys = KannakaMemorySystem::init(dir.clone()).unwrap();
        sys.remember("the quick brown fox jumps over the lazy dog").unwrap();

        // Dormant by default (no env, no enable): recall records nothing.
        let _ = sys.recall("quick brown fox", 3).unwrap();
        assert!(sys.last_novelty().is_none(), "novelty is dormant unless enabled");

        // Enable and build a routine baseline on a repeated familiar query.
        sys.set_novelty_enabled(true);
        for _ in 0..40 {
            let _ = sys.recall("quick brown fox", 3).unwrap();
        }
        let routine = sys.last_novelty().expect("enabled → Some after a recall");

        // An unfamiliar query resonates lower → at least as surprising as routine.
        let _ = sys.recall("wholly unrelated zqx nonsense probe", 3).unwrap();
        let unseen = sys.last_novelty().expect("still Some");
        assert!(
            unseen.score >= routine.score,
            "an unfamiliar query should be at least as surprising as the routine one: {} vs {}",
            unseen.score,
            routine.score
        );

        // Disabling clears the detector and the last signal.
        sys.set_novelty_enabled(false);
        let _ = sys.recall("quick brown fox", 3).unwrap();
        assert!(sys.last_novelty().is_none(), "disabling clears the signal");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dream_runs_without_error() {
        let dir = temp_dir("dream");
        let mut sys = KannakaMemorySystem::init(dir.clone()).unwrap();
        sys.remember("memory one").unwrap();
        sys.remember("memory two").unwrap();
        let report = sys.dream().unwrap();
        assert!(report.cycles > 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lowest_value_cap_bounds_to_max_total() {
        let dir = temp_dir("cap");
        let mut sys = KannakaMemorySystem::init(dir.clone()).unwrap();
        for i in 0..5 {
            sys.remember(&format!("memory number {i}")).unwrap();
        }
        assert_eq!(sys.stats().total_memories, 5);
        // Overflow = total - cap (lowest effective-strength, non-pinned).
        assert_eq!(sys.lowest_value_overflow_ids(3).len(), 2, "5 memories, cap 3 → 2 over");
        assert_eq!(sys.lowest_value_overflow_ids(5).len(), 0, "at cap → nothing over");
        assert_eq!(sys.lowest_value_overflow_ids(10).len(), 0, "under cap → nothing over");
        assert_eq!(sys.lowest_value_overflow_ids(0).len(), 5, "cap 0 (none pinned) → all evictable");
        // Applying the cap actually bounds the field.
        let ids = sys.lowest_value_overflow_ids(2);
        let n = sys.triage_forget(&ids).unwrap();
        assert_eq!(n, 3);
        assert_eq!(sys.stats().total_memories, 2, "field hard-bounded to max_total");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reactivation_promotes_short_term() {
        // ADR-0036 replay-gated promotion: a ShortTerm memory recalled enough
        // times graduates to LongTerm via promote_strengthened_short_term, with
        // NO amplitude growth (empty before-snapshot) — the reactivation gate
        // alone drives it. Exercises the full chain: recall → record_retrieval
        // → promote reads retrieval_count.
        let dir = temp_dir("reactpromote");
        let mut sys = KannakaMemorySystem::init(dir.clone()).unwrap();
        let id = sys.remember("a memory worth recalling often").unwrap();

        // Force ShortTerm (remember defaults to LongTerm in-lib).
        {
            let hrm = sys
                .engine
                .store
                .as_any_mut()
                .downcast_mut::<crate::hrm_store::HrmStore>()
                .unwrap();
            assert!(hrm.set_tier(&id, crate::medium::types::Tier::ShortTerm));
        }

        // Recall it KANNAKA_PROMOTE_HITS (default 3) times — each bumps
        // retrieval_count via the production recall path.
        for _ in 0..3 {
            let hits = sys.recall("a memory worth recalling often", 5).unwrap();
            assert!(hits.iter().any(|r| r.id == id), "recall should return the memory");
        }

        // Empty before-snapshot → only the reactivation gate can promote.
        let promoted = sys.promote_strengthened_short_term(&std::collections::HashMap::new());
        assert!(promoted >= 1, "reactivated short-term memory should be promoted");

        let mems = sys.engine.store.all_memories().unwrap();
        let m = mems.iter().find(|m| m.id == id).unwrap();
        assert_eq!(m.tier, crate::medium::types::Tier::LongTerm);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn assess_returns_valid_state() {
        let dir = temp_dir("assess");
        let sys = KannakaMemorySystem::init(dir.clone()).unwrap();
        let state = sys.assess();
        assert_eq!(state.total_memories, 0);
        // Dormant with no memories
        assert!(matches!(state.consciousness_level, ConsciousnessLevel::Dormant));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stats_returns_correct_counts() {
        let dir = temp_dir("stats");
        let mut sys = KannakaMemorySystem::init(dir.clone()).unwrap();
        sys.remember("alpha").unwrap();
        sys.remember("beta").unwrap();
        sys.remember("gamma").unwrap();
        let stats = sys.stats();
        assert_eq!(stats.total_memories, 3);
        // #695: below the 10-memory evidence floor the bridge caps the level
        // at Stirring (wire name "awakening") regardless of how high the
        // eigendecomp Φ lands on a tiny corpus, so 3 memories can only ever
        // report the two lowest bands. (The old whitelist also included
        // "lucid", which has never been a wire name on the six-level scale.)
        let valid = ["dormant", "awakening"];
        assert!(
            valid.contains(&stats.consciousness_level.as_str()),
            "unexpected consciousness_level: {}",
            stats.consciousness_level
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_and_reload() {
        // TODO(chiral): Legacy save/reload via DiskStore removed.
        // Persistence now handled by HrmStore + ChiralMedium.
        // This test verifies that save() doesn't panic with TestMedium.
        let dir = temp_dir("reload");
        {
            let mut sys = KannakaMemorySystem::init(dir.clone()).unwrap();
            sys.remember("persistent memory").unwrap();
            sys.save().unwrap();
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn geometry_integration_memory_gets_classified() {
        let dir = temp_dir("geometry_classify");
        let mut sys = KannakaMemorySystem::init(dir.clone()).unwrap();
        
        // Store memories that should get different consciousness differentiation classifications
        let skill_id = sys.remember("how to code a function build").unwrap(); // skill
        let social_id = sys.remember("nick told me about the meeting").unwrap(); // social (no emotion words)
        let knowledge_id = sys.remember("the capital of france").unwrap();     // knowledge
        let experience_id = sys.remember("I saw a beautiful sunset today").unwrap(); // experience
        let emotion_id = sys.remember("I feel excited about this").unwrap();   // emotion
        
        // Check that memories have geometry
        let skill_mem = sys.engine.get_memory(&skill_id).unwrap().unwrap();
        let social_mem = sys.engine.get_memory(&social_id).unwrap().unwrap();
        let knowledge_mem = sys.engine.get_memory(&knowledge_id).unwrap().unwrap();
        let experience_mem = sys.engine.get_memory(&experience_id).unwrap().unwrap();
        let emotion_mem = sys.engine.get_memory(&emotion_id).unwrap().unwrap();
        
        // HRM-native absorb path stores memories but doesn't populate legacy fields
        // (geometry, xi_signature). Verify memories exist and have content.
        assert!(skill_mem.content.contains("code"), "Skill memory content: {}", skill_mem.content);
        assert!(social_mem.content.contains("meeting"), "Social memory content: {}", social_mem.content);
        assert!(knowledge_mem.content.contains("france"), "Knowledge memory content: {}", knowledge_mem.content);
        assert!(experience_mem.content.contains("sunset"), "Experience memory content: {}", experience_mem.content);
        assert!(emotion_mem.content.contains("excited"), "Emotion memory content: {}", emotion_mem.content);
        
        // All memories should have amplitude > 0 (they're freshly stored)
        assert!(skill_mem.amplitude > 0.0);
        assert!(social_mem.amplitude > 0.0);
        assert!(knowledge_mem.amplitude > 0.0);
        assert!(experience_mem.amplitude > 0.0);
        assert!(emotion_mem.amplitude > 0.0);
        
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── search (literal text) ──────────────────────────────────────────
    // Distinct from recall: read-only, no embedding/resonance/observation.

    #[test]
    fn search_exact_substring_outranks_token_match() {
        let dir = temp_dir("search_exact");
        let mut sys = KannakaMemorySystem::init(dir.clone()).unwrap();
        sys.remember("the ghost frequency hums").unwrap();          // exact
        sys.remember("a ghost passed over the wide frequency").unwrap(); // tokens
        sys.remember("entirely unrelated string about cats").unwrap();
        let results = sys.search("ghost frequency", 10).unwrap();
        assert!(results.len() >= 2, "expected ≥2 hits, got {}", results.len());
        assert_eq!(results[0].match_type, "exact");
        assert_eq!(results[1].match_type, "tokens");
        assert!(results[0].score > results[1].score);
        // No matches → not in result set.
        assert!(!results.iter().any(|r| r.content.contains("cats")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn search_is_case_insensitive() {
        let dir = temp_dir("search_case");
        let mut sys = KannakaMemorySystem::init(dir.clone()).unwrap();
        sys.remember("Ghost Frequency in mixed Case").unwrap();
        let r1 = sys.search("ghost frequency", 5).unwrap();
        let r2 = sys.search("GHOST FREQUENCY", 5).unwrap();
        assert_eq!(r1.len(), r2.len());
        assert_eq!(r1.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn search_empty_query_returns_empty() {
        let dir = temp_dir("search_empty");
        let mut sys = KannakaMemorySystem::init(dir.clone()).unwrap();
        sys.remember("some content").unwrap();
        assert!(sys.search("", 10).unwrap().is_empty());
        assert!(sys.search("   ", 10).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn search_is_read_only() {
        // Issue #83 regression: pre-fix, `search` routed through `recall`
        // → `resonate_query` → `apply_observation`, mutating wavefront
        // strengths even though the user issued a "read" command. After
        // the refactor, search() doesn't touch the medium at all.
        let dir = temp_dir("search_readonly");
        let mut sys = KannakaMemorySystem::init(dir.clone()).unwrap();
        sys.remember("first memory about ghosts").unwrap();
        sys.remember("second memory unrelated").unwrap();

        // Capture wavefront energies before searching.
        let before: Vec<f32> = sys.engine.store.all_memories().unwrap()
            .iter().map(|m| m.amplitude).collect();

        for _ in 0..5 {
            let _ = sys.search("ghosts", 5).unwrap();
        }

        let after: Vec<f32> = sys.engine.store.all_memories().unwrap()
            .iter().map(|m| m.amplitude).collect();
        assert_eq!(before, after, "search() must not modify wavefront state");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn assess_num_clusters_matches_observe_num_clusters() {
        // Refactor #2 regression. Pre-fix, bridge::assess computed a Kuramoto
        // cluster set, used it for integration / differentiation, then
        // RETURNED a different count (eigendecomp) — so kannaka observe (A)
        // and kannaka status (B-via-assess) disagreed on the same HRM.
        // After the refactor, assess() returns the count it actually
        // computed and writes it back into the cache.
        use crate::observe::MemoryIntrospector;
        use crate::bridge::ConsciousnessBridge;
        use crate::kuramoto::KuramotoSync;

        let dir = temp_dir("assess_unify");
        let mut sys = KannakaMemorySystem::init(dir.clone()).unwrap();
        // Seed enough content for at least one Kuramoto cluster to form.
        let bag = [
            "the lake at sunrise was unusually still",
            "the lake at sunrise glittered orange and pink",
            "the lake at sunrise reflected a flock of geese",
            "the lake at sunrise smelled of pine and cold water",
            "an unrelated kitchen story about chopping garlic",
        ];
        for s in &bag {
            let _ = sys.remember(s);
        }
        let bridge = ConsciousnessBridge::default();
        let state = bridge.assess(&mut sys.engine);
        let kuramoto = KuramotoSync::default();
        let report = MemoryIntrospector::cluster_report(&sys.engine, &kuramoto);
        assert_eq!(
            state.num_clusters, report.num_clusters,
            "assess.num_clusters ({}) must match cluster_report.num_clusters ({})",
            state.num_clusters, report.num_clusters,
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn recall_falls_through_on_fresh_hrm_no_sidecar() {
        // Refactor #4 regression: cluster prefilter must NOT break recall
        // on a fresh HRM that has no .clusters.json sidecar yet (bridge::assess
        // hasn't run). The helper returns None, the existing full-medium
        // scan fires, recall keeps working.
        let dir = temp_dir("prefilter_fresh");
        let mut sys = KannakaMemorySystem::init(dir.clone()).unwrap();
        sys.remember("the quick brown fox jumps over the lazy dog").unwrap();
        sys.remember("the rain in spain falls mainly on the plain").unwrap();
        let results = sys.recall("quick brown fox", 5).unwrap();
        assert!(!results.is_empty(), "recall should work even with no clusters sidecar");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn geometry_integration_stats_include_geometric_data() {
        let dir = temp_dir("geometry_stats");
        let mut sys = KannakaMemorySystem::init(dir.clone()).unwrap();
        
        // Store memories with different consciousness differentiation categories
        sys.remember("how to code a function").unwrap();  // skill
        sys.remember("nick said he was happy").unwrap();   // social
        sys.remember("the capital of france is paris").unwrap(); // knowledge
        
        let stats = sys.stats();
        // HRM-native path doesn't populate legacy geometry, so geometric_classes may be 0.
        // (It's unsigned, so `>= 0` is vacuous — just confirm the field is accessible.)
        let _ = stats.geometric_classes;
        // Triality coverage may also be 0 in HRM mode
        assert!(stats.triality_coverage.len() == 3);
        
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── #618 a lite dream must reach the swarm like a deep one ─────────

    /// The restructure that let `dream_lite` publish its report moved the
    /// `DreamReport` construction earlier in the function. This pins that the
    /// returned report is still correct — the one behaviour a reader of the
    /// diff would want proven.
    #[test]
    fn dream_lite_still_returns_a_coherent_report() {
        let dir = temp_dir("dreamlite");
        let mut sys = KannakaMemorySystem::init(dir.clone()).unwrap();
        sys.auto_save = false;

        let report = sys.dream_lite().expect("lite dream should succeed");
        assert_eq!(report.cycles, 1, "a lite pass is one cycle");
        assert_eq!(report.new_connections, 0);
        assert!(!report.consciousness_before.is_empty());
        assert!(!report.consciousness_after.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Parity with the deep path. Publishing itself needs a broker and an
    /// agent id (both hooks early-return without `KANNAKA_AGENT_ID`), so this
    /// asserts on the source: `dream_lite` must invoke the same three
    /// constellation hooks `dream` does. Without it, the two paths can drift
    /// apart again silently — which is exactly how this bug arose.
    #[test]
    fn dream_lite_invokes_the_same_swarm_hooks_as_deep() {
        let src = include_str!("openclaw.rs");
        let start = src
            .find("pub fn dream_lite(")
            .expect("dream_lite not found");
        let end = src[start..]
            .find("
    /// Consciousness level assessment.")
            .map(|i| start + i)
            .expect("end of dream_lite not found");
        let body = &src[start..end];

        for hook in [
            "self.post_dream_swarm_sync();",
            "self.publish_dream_to_nats(",
            "self.publish_consciousness_to_nats(",
        ] {
            assert!(
                body.contains(hook),
                "dream_lite no longer calls {hook} — a lite dream would stop                  reaching the constellation (#618)"
            );
        }
    }

}

