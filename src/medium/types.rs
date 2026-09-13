//! Types, constants, and helper functions for the Holographic Resonance Medium.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::consciousness::{
    ConsciousnessLevel, ConsciousnessMetrics, EmergenceLevel, EmergenceReport,
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Hypervector dimension — matches CODEBOOK_OUTPUT_DIM
pub const WAVEFRONT_DIM: usize = 10_000;

/// Audio feature dimension (from kannaka-ear)
pub const AUDIO_FEATURE_DIM: usize = 296;

/// Visual feature dimension (from kannaka-eye)
pub const VISUAL_FEATURE_DIM: usize = 320;

/// Audio codebook seed (EAR = 0xEA5)
pub const AUDIO_CODEBOOK_SEED: u64 = 0xEA5;

/// Visual codebook seed (EYE = 0x3E5E)
pub const VISUAL_CODEBOOK_SEED: u64 = 0x3E5E;

/// HRM file magic bytes: "HRM\x01"
pub(crate) const HRM_MAGIC: [u8; 4] = [0x48, 0x52, 0x4D, 0x01];

/// Current HRM format version
pub(crate) const HRM_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum MediumError {
    #[error("wavefront not found: {0}")]
    WavefrontNotFound(Uuid),
    #[error("dimension mismatch: expected {expected}, got {actual}")]
    DimensionMismatch { expected: usize, actual: usize },
    #[error("invalid magic bytes in HRM file")]
    InvalidMagic,
    #[error("unsupported HRM version: {0}")]
    UnsupportedVersion(u32),
    #[error("checksum mismatch - file may be corrupted")]
    ChecksumMismatch,
    #[error("corrupt HRM: {0}")]
    CorruptHrm(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialization error: {0}")]
    Serialization(#[from] bincode::Error),
    #[error("Git error: {0}")]
    Git(String),
    #[error("No git repository found")]
    NoGitRepo,
}

// ---------------------------------------------------------------------------
// Modality (NCS Phase 1.1 — ADR-0042)
// ---------------------------------------------------------------------------

/// Sensory modality of a wavefront — which channel produced this memory.
///
/// Used by the Neural Consciousness System (NCS) to route, filter, and
/// weight cross-modal interference in the holographic medium.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum Modality {
    Audio,
    Visual,
    Semantic,
    Network,
    Mixed,
    #[default]
    Unknown,
}


/// Retention tier of a wavefront (ADR-0031 memory triage).
///
/// `LongTerm` is the default for every existing/back-loaded memory so the
/// format change is non-destructive — nothing becomes eviction-eligible on
/// upgrade. `ShortTerm` is eviction-eligible by `kannaka triage`; `Pinned` is
/// never evicted and never demoted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum Tier {
    ShortTerm,
    #[default]
    LongTerm,
    Pinned,
}

impl Tier {
    /// Retention precedence, low to high. Anywhere a group of memories collapses
    /// to one survivor, the survivor must carry the group's HIGHEST tier —
    /// otherwise a merge silently demotes a Pinned memory, which ADR-0031
    /// forbids. Existing users: the resonance-merge carrier in
    /// `HrmStore::plan_consolidation`, and duplicate collapse in
    /// `KannakaMemorySystem::collapse_exact_duplicates`.
    pub fn rank(self) -> u8 {
        match self {
            Tier::ShortTerm => 0,
            Tier::LongTerm => 1,
            Tier::Pinned => 2,
        }
    }
}

// ───────────────────────────────────────────────────────────────────────────
// ADR-0036: Consolidation as resonance-merge.
// Phase 0 is dry-run ONLY — the planner computes what it WOULD merge/decay and
// reports it; it never mutates the substrate. Apply (mutation) lands in Phase 2.
// ───────────────────────────────────────────────────────────────────────────

/// How the resonance-merge consolidation pass behaves during a dream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsolidateMode {
    /// Skip entirely.
    Off,
    /// Compute and report the merge/decay plan WITHOUT mutating anything.
    DryRun,
    /// Apply the plan. NOT implemented until ADR-0036 Phase 2 — treated as
    /// dry-run for now so enabling it early can never delete memories.
    Apply,
}

/// ADR-0036 belief-safe merge: default cap on the fraction of the field a single
/// consolidation pass may absorb while the belief substrate is active. The
/// 295→82 over-absorb (the failure v0.7.3 gated) would have been bounded to
/// ~295→236 by this backstop alone.
pub const DEFAULT_BELIEF_ABSORB_FRAC: f32 = 0.20;

/// ADR-0036 belief-safe merge: cosine floor (on mean-CENTERED embeddings) for the
/// semantic-redundancy gate while belief is active. Higher than the raw
/// `merge_sim` because centering removes the shared anisotropic component that
/// inflates raw cosine on a cone-clustered field, so only genuine residual
/// redundancy clears the bar.
pub const DEFAULT_MERGE_SIM_BELIEF: f32 = 0.95;

/// M3 salience decay defaults — see [`ConsolidateOpts::shortterm_decay_pctl`].
/// 0.50 mirrors `kannaka-crystal`'s deep-dream `prune_percentile`.
pub const DEFAULT_SHORTTERM_DECAY_PCTL: f32 = 0.50;
/// Gentler than crystal's 0.65 because kannaka dreams nightly rather than on demand.
pub const DEFAULT_SHORTTERM_DECAY_FACTOR: f32 = 0.90;

/// Tunables for the consolidation planner, sourced from env so the dry-run can
/// be observed and the thresholds tuned before any destructive apply lands.
#[derive(Debug, Clone)]
pub struct ConsolidateOpts {
    pub mode: ConsolidateMode,
    /// Cosine-similarity threshold above which two wavefronts are "redundant".
    pub merge_sim: f32,
    /// Minimum cos(Δphase) for two wavefronts to count as constructively
    /// phase-locked (default ≈ cos(π/4) ≈ 0.707 — the "Constructive" band).
    pub merge_phase_cos: f32,
    /// ShortTerm energy below which an unreactivated trace would be evicted (M3).
    pub shortterm_evict: f32,
    /// ADR-0036 belief-safe merge — semantic floor on the mean-CENTERED embedding
    /// when belief is active (removes the anisotropic shared component so belief-
    /// induced phase/cosine alignment can't group semantically-distinct memories).
    pub merge_sim_belief: f32,
    /// ADR-0036 belief-safe merge — max fraction of the examined field a single
    /// pass may absorb. `None` uses the belief-aware default: capped at
    /// [`DEFAULT_BELIEF_ABSORB_FRAC`] while belief is active, uncapped otherwise
    /// (so the default non-belief path stays byte-identical). `Some(f)` forces the
    /// cap regardless of belief; `Some(1.0)` disables it.
    pub max_absorb_frac: Option<f32>,
    /// M3 salience decay — fraction of the *observed ShortTerm amplitude
    /// distribution* attenuated each pass. Borrowed from `kannaka-crystal`'s
    /// dream engine, which thresholds at a **percentile of what is actually
    /// there** rather than at an absolute constant.
    ///
    /// This exists because the absolute [`ConsolidateOpts::shortterm_evict`]
    /// floor (0.15) sits below the entire live amplitude distribution — measured
    /// 2026-08-22, the weakest active wavefront on kannaka-prime was 0.60 and the
    /// local substrate averaged 1.73 — so `would_evict` was **structurally always
    /// 0** on every node in the constellation and nothing ever reclaimed.
    ///
    /// A percentile cannot be miscalibrated against an unknown scale. Decay is a
    /// soft multiply, never a removal: unretrieved ShortTerm traces fade toward
    /// the evict floor over successive nights, and the existing (conservative,
    /// reactivation-guarded) evict path finally becomes reachable. `0.0` disables.
    pub shortterm_decay_pctl: f32,
    /// Multiplier applied to a decaying wavefront's amplitude each pass. Crystal's
    /// deep dream uses 0.65, but it dreams on demand; kannaka dreams nightly, so
    /// this is gentler — 0.90 fades a 0.60 trace under the 0.15 floor in ~14
    /// unretrieved nights.
    pub shortterm_decay_factor: f32,
    /// When the absorb cap is engaged, admit the most-cohesive members of an
    /// oversized group up to the remaining capacity instead of skipping the group
    /// whole. Without this a group larger than the cap is skipped **every night
    /// forever** — measured on the local substrate: 217 of 654 wavefronts met the
    /// redundancy criteria but only 9 merged, because the one ~208-member group
    /// could never fit under the 20% cap. The cap is still honoured exactly; the
    /// group simply drains over several nights instead of never.
    pub merge_partial_groups: bool,
}

impl Default for ConsolidateOpts {
    fn default() -> Self {
        ConsolidateOpts {
            mode: ConsolidateMode::DryRun,
            merge_sim: 0.92,
            merge_phase_cos: std::f32::consts::FRAC_1_SQRT_2, // cos(π/4)
            shortterm_evict: 0.15,
            merge_sim_belief: DEFAULT_MERGE_SIM_BELIEF,
            max_absorb_frac: None,
            shortterm_decay_pctl: DEFAULT_SHORTTERM_DECAY_PCTL,
            shortterm_decay_factor: DEFAULT_SHORTTERM_DECAY_FACTOR,
            merge_partial_groups: true,
        }
    }
}

impl ConsolidateOpts {
    /// Build from environment. `KANNAKA_CONSOLIDATE` selects the mode
    /// (`off` | `dryrun` | `on`); default is `dryrun` so merging is always
    /// opt-in. Thresholds override via `KANNAKA_MERGE_SIM`,
    /// `KANNAKA_MERGE_PHASE_DEG`, `KANNAKA_SHORTTERM_EVICT`.
    pub fn from_env() -> Self {
        let mut o = ConsolidateOpts::default();
        match std::env::var("KANNAKA_CONSOLIDATE")
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str()
        {
            "off" | "0" | "false" => o.mode = ConsolidateMode::Off,
            "on" | "apply" => o.mode = ConsolidateMode::Apply,
            _ => o.mode = ConsolidateMode::DryRun,
        }
        if let Ok(v) = std::env::var("KANNAKA_MERGE_SIM") {
            if let Ok(f) = v.parse() { o.merge_sim = f; }
        }
        if let Ok(v) = std::env::var("KANNAKA_MERGE_PHASE_DEG") {
            if let Ok(deg) = v.parse::<f32>() { o.merge_phase_cos = deg.to_radians().cos(); }
        }
        if let Ok(v) = std::env::var("KANNAKA_SHORTTERM_EVICT") {
            if let Ok(f) = v.parse() { o.shortterm_evict = f; }
        }
        // ADR-0036 belief-safe merge tunables.
        if let Ok(v) = std::env::var("KANNAKA_MERGE_SIM_BELIEF") {
            if let Ok(f) = v.parse() { o.merge_sim_belief = f; }
        }
        if let Ok(v) = std::env::var("KANNAKA_MERGE_MAX_ABSORB_FRAC") {
            if let Ok(f) = v.parse::<f32>() { o.max_absorb_frac = Some(f.clamp(0.0, 1.0)); }
        }
        // M3 salience decay (crystal-borrowed percentile). `=0` disables.
        if let Ok(v) = std::env::var("KANNAKA_SHORTTERM_DECAY_PCTL") {
            if let Ok(f) = v.parse::<f32>() { o.shortterm_decay_pctl = f.clamp(0.0, 1.0); }
        }
        if let Ok(v) = std::env::var("KANNAKA_SHORTTERM_DECAY_FACTOR") {
            if let Ok(f) = v.parse::<f32>() { o.shortterm_decay_factor = f.clamp(0.0, 1.0); }
        }
        if let Ok(v) = std::env::var("KANNAKA_MERGE_PARTIAL") {
            o.merge_partial_groups = !matches!(v.as_str(), "0" | "off" | "false");
        }
        o
    }
}

/// What a consolidation pass did — or, in dry-run, WOULD do. Surfaced in the
/// dream log (and, from Phase 1, `observe --json`).
#[derive(Debug, Clone, Default, Serialize)]
pub struct ConsolidateReport {
    pub mode: String,
    pub memories_examined: usize,
    /// Redundant, phase-locked groups of size ≥ 2.
    pub groups_found: usize,
    /// Carriers that would remain (one per merged group).
    pub would_merge: usize,
    /// Wavefronts that would be absorbed (removed) by merging.
    pub would_absorb: usize,
    pub shortterm_total: usize,
    /// ShortTerm traces subject to salience decay (M3).
    pub would_decay: usize,
    /// ShortTerm traces that would fall below the evict threshold (unreactivated).
    pub would_evict: usize,
    /// Projected memory count after a merge+evict apply.
    pub projected_memories: usize,
    /// False in Phase 0 (planning only); true once Phase 2 apply lands.
    pub applied: bool,
    /// ADR-0036 belief-safe merge — the semantic gate ran on mean-CENTERED
    /// embeddings (the belief path), not raw cosine.
    #[serde(default)]
    pub centered: bool,
    /// ADR-0036 belief-safe merge — the per-pass absorb cap trimmed the plan
    /// (fewer groups applied than were found). Logged loudly when true.
    #[serde(default)]
    pub absorb_capped: bool,
    /// ADR-0036 belief-safe merge — redundant groups the criteria found BEFORE
    /// the absorb cap trimmed them (≥ `groups_found`).
    #[serde(default)]
    pub groups_before_cap: usize,
    /// ADR-0036 belief-safe merge — wavefronts the criteria would absorb BEFORE
    /// the cap (≥ `would_absorb`).
    #[serde(default)]
    pub absorb_before_cap: usize,
}


impl std::fmt::Display for Tier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Tier::ShortTerm => "short-term",
            Tier::LongTerm => "long-term",
            Tier::Pinned => "pinned",
        };
        f.write_str(s)
    }
}

impl Modality {
    /// All concrete (non-Mixed, non-Unknown) modality variants.
    pub fn all_concrete() -> &'static [Modality] {
        &[Modality::Audio, Modality::Visual, Modality::Semantic, Modality::Network]
    }
}

impl std::fmt::Display for Modality {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Modality::Audio => write!(f, "audio"),
            Modality::Visual => write!(f, "visual"),
            Modality::Semantic => write!(f, "semantic"),
            Modality::Network => write!(f, "network"),
            Modality::Mixed => write!(f, "mixed"),
            Modality::Unknown => write!(f, "unknown"),
        }
    }
}

impl std::str::FromStr for Modality {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "audio" => Ok(Modality::Audio),
            "visual" => Ok(Modality::Visual),
            "semantic" => Ok(Modality::Semantic),
            "network" => Ok(Modality::Network),
            "mixed" => Ok(Modality::Mixed),
            "unknown" => Ok(Modality::Unknown),
            _ => Err(format!("unknown modality: '{s}' (expected: audio, visual, semantic, network, mixed, unknown)")),
        }
    }
}

// ---------------------------------------------------------------------------
// Modality detection (NCS Phase 1.3 — retroactive classification)
// ---------------------------------------------------------------------------

/// Result of keyword-based modality classification.
#[derive(Debug, Clone)]
pub struct ModalityClassification {
    pub modality: Modality,
    /// Confidence score in [0.0, 1.0]. Low confidence means the content
    /// sits near the boundary between two or more modalities.
    pub confidence: f32,
    /// Per-modality scores used to derive the final classification.
    pub scores: ModalityScores,
}

/// Raw keyword-match scores for each modality channel.
#[derive(Debug, Clone, Default)]
pub struct ModalityScores {
    pub audio: f32,
    pub visual: f32,
    pub semantic: f32,
    pub network: f32,
}

/// Detect the most likely modality of a memory from its text content
/// using keyword analysis. Returns the classification with a confidence
/// score so callers can identify boundary memories.
pub fn detect_modality(content: &str) -> ModalityClassification {
    let lower = content.to_lowercase();

    let mut scores = ModalityScores::default();

    // --- Audio keywords ---
    const AUDIO_KW: &[&str] = &[
        "audio", "sound", "music", "song", "hear", "heard", "listen",
        "tempo", "bpm", "beat", "rhythm", "melody", "harmonic",
        "frequency", "spectral", "rms", "centroid", "loudness",
        "noise", "silence", "voice", "speech", "acoustic",
        "waveform", "sample", "tone", "pitch", "decibel",
        "bass", "treble", "reverb", "echo", "stereo", "mono",
        "podcast", "radio", "track", "album", "playlist",
        "microphone", "speaker", "recording", "wav", "mp3",
        "midi", "synth", "synthesizer", "drum", "guitar",
    ];

    // --- Visual keywords ---
    const VISUAL_KW: &[&str] = &[
        "visual", "image", "picture", "photo", "video", "frame",
        "color", "colour", "pixel", "resolution", "brightness",
        "contrast", "glyph", "icon", "shape", "pattern",
        "scene", "camera", "lens", "focus", "blur",
        "motion", "shot", "cut", "edit", "render",
        "display", "screen", "light", "shadow", "texture",
        "gradient", "palette", "hue", "saturation",
        "png", "jpg", "jpeg", "gif", "svg", "bitmap",
        "sketch", "draw", "paint", "illustration",
    ];

    // --- Network keywords ---
    const NETWORK_KW: &[&str] = &[
        "network", "swarm", "nats", "flux", "agent",
        "peer", "node", "cluster", "distributed", "gossip",
        "sync", "publish", "subscribe", "broadcast", "multicast",
        "latency", "bandwidth", "packet", "protocol", "tcp",
        "udp", "http", "websocket", "api", "endpoint",
        "mesh", "topology", "route", "gateway", "proxy",
        "consensus", "quorum", "replication", "federation",
        "queen", "hive", "collective", "merge",
    ];

    // --- Semantic keywords (general knowledge / language / reasoning) ---
    const SEMANTIC_KW: &[&str] = &[
        "semantic", "meaning", "concept", "idea", "thought",
        "knowledge", "understand", "reason", "logic", "infer",
        "language", "word", "sentence", "paragraph", "text",
        "definition", "category", "classify", "ontology",
        "abstract", "concrete", "metaphor", "analogy",
        "memory", "remember", "recall", "learn", "teach",
        "consciousness", "phi", "emergence", "aware",
        "dream", "consolidat", "anneal", "reflect",
        "emotion", "feel", "experience", "perceive",
        "wisdom", "insight", "observation", "introspect",
    ];

    // Score each keyword. Whole-word matching via contains is sufficient for
    // keyword analysis; we give a small bonus for longer keyword matches
    // to reduce false positives from short words embedded in other tokens.
    for kw in AUDIO_KW {
        if lower.contains(kw) {
            scores.audio += if kw.len() >= 6 { 2.0 } else { 1.0 };
        }
    }
    for kw in VISUAL_KW {
        if lower.contains(kw) {
            scores.visual += if kw.len() >= 6 { 2.0 } else { 1.0 };
        }
    }
    for kw in NETWORK_KW {
        if lower.contains(kw) {
            scores.network += if kw.len() >= 6 { 2.0 } else { 1.0 };
        }
    }
    for kw in SEMANTIC_KW {
        if lower.contains(kw) {
            scores.semantic += if kw.len() >= 6 { 2.0 } else { 1.0 };
        }
    }

    let all = [
        (Modality::Audio, scores.audio),
        (Modality::Visual, scores.visual),
        (Modality::Network, scores.network),
        (Modality::Semantic, scores.semantic),
    ];

    let total: f32 = all.iter().map(|(_, s)| s).sum();

    if total == 0.0 {
        // No keywords matched at all — truly unknown
        return ModalityClassification {
            modality: Modality::Unknown,
            confidence: 0.0,
            scores,
        };
    }

    // Sort descending by score
    let mut sorted = all;
    sorted.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let best = sorted[0];
    let second = sorted[1];

    // Confidence = how dominant the top modality is relative to runner-up
    let confidence = if total > 0.0 {
        let dominance = (best.1 - second.1) / total;
        // Map dominance [0, 1] to confidence; pure single-modality gets ~1.0
        (0.5 + dominance * 0.5).min(1.0)
    } else {
        0.0
    };

    // If the top two modalities are very close, classify as Mixed
    let mixed_threshold = 0.15; // within 15% of total
    let gap = (best.1 - second.1) / total;
    if gap < mixed_threshold && second.1 > 0.0 {
        return ModalityClassification {
            modality: Modality::Mixed,
            confidence,
            scores,
        };
    }

    ModalityClassification {
        modality: best.0,
        confidence,
        scores,
    }
}

/// Convenience wrapper over [`detect_modality`] that returns `(Modality, f32)`.
///
/// This is the simplified API specified by NCS Phase 1.2 (Issue #43).
/// When you need the full per-channel scores, call [`detect_modality`] directly.
pub fn detect_modality_simple(content: &str) -> (Modality, f32) {
    let c = detect_modality(content);
    (c.modality, c.confidence)
}

// ---------------------------------------------------------------------------
// Supporting types
// ---------------------------------------------------------------------------

/// Metadata for a single wavefront (content, tags, etc.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WavefrontMeta {
    pub id: Uuid,
    pub content: String,
    pub tags: Vec<String>,
    pub created_at: DateTime<Utc>,
    /// Whether this wavefront was generated by hallucination during dreaming
    pub hallucinated: bool,
    /// Whether this wavefront is self-referential (encodes the medium's own state)
    pub is_self_referential: bool,
    /// SGA classification — which of the 96 classes this memory belongs to
    /// Encodes (h₂, d, ℓ) coordinates in the Sigmatics Geometric Algebra
    /// Note: skipped in bincode serialization for backward compat with v1/v2 .hrm files
    #[serde(skip)]
    pub sga_class: Option<u8>,
    /// Fano group (0-6) — derived from SGA class, determines fold operations
    #[serde(skip)]
    pub fano_group: Option<u8>,
    /// Category used for SGA classification
    #[serde(skip)]
    pub category: Option<String>,
    /// Sensory modality of this wavefront (NCS Phase 1.1)
    #[serde(default)]
    pub modality: Modality,
    /// Retention tier (ADR-0031). Appended after `modality`; the bincode
    /// back-compat fallback in chiral_persistence/persistence relies on this
    /// trailing-field ordering, so older files (which lack these bytes) fail
    /// the new-struct deserialize and hit the pre-tier/legacy path.
    #[serde(default)]
    pub tier: Tier,
    /// Wave 3 Task 3.2b — temporal-truth bounds (ADR-0035 Cap 8). These MUST
    /// stay the LAST serialized fields, appended after `tier`: the bincode
    /// fallback chain relies on `.hrm` files written before this change lacking
    /// these trailing bytes, so they fail the new-struct deserialize and drop
    /// to `WavefrontMetaPreTemporal`. Order (effective/observed/expires) must
    /// match the fallback struct and stay stable.
    /// When the fact became true (None = always / unknown).
    #[serde(default)]
    pub effective_at: Option<DateTime<Utc>>,
    /// When this agent observed/learned the fact (None = fall back to created_at).
    #[serde(default)]
    pub observed_at: Option<DateTime<Utc>>,
    /// When the fact is known to stop being true (None = no known expiry).
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
    /// Quantum-Wave T1.4 (#474): provenance of the entropy that seeded the
    /// dream/Ξ that last wrote this wavefront. `None` for records written before
    /// provenance existed (loaded via the `WavefrontMetaPreProvenance` fallback)
    /// ⇒ treated as `prng://legacy`. This is now the LAST serialized field — the
    /// bincode fallback chain relies on `.hrm` files written before this change
    /// lacking these trailing bytes, so they fail the new-struct deserialize and
    /// drop to `WavefrontMetaPreProvenance`.
    #[serde(default)]
    pub provenance: Option<crate::entropy::Provenance>,
    /// ADR-0049 facet encoding. These MUST stay the LAST serialized fields,
    /// appended after `provenance`: the bincode fallback chain relies on `.hrm`
    /// files written before this change lacking these trailing bytes, so they
    /// fail the new-struct deserialize and drop to `WavefrontMetaPreFacet`.
    /// Get this ordering wrong and every record misdecodes behind a checksum
    /// that still validates — see the round-trip fixture that locks it.
    ///
    /// Parent this row is a facet of. `Some` ⇒ this is an atomic facet
    /// wavefront; recall resolves it back to the parent for holistic context.
    /// Deliberately NOT `HyperMemory.parents`, which is hallucination lineage
    /// exported in the glyph spec — facet linkage is a distinct named field.
    #[serde(default)]
    pub parent_id: Option<Uuid>,
    /// This row is a facet (scanned for recall, excluded from merge grouping
    /// and user-facing counts).
    #[serde(default)]
    pub is_facet: bool,
    /// This row is a parent that has already been decomposed into facets.
    /// Retained resolve-only: excluded from the resonance scan, resonance-merge
    /// grouping, the coherence eigendecomp, energy prune/ghost, and every
    /// user-facing count. Parent retention is an invariant — never delete one to
    /// save space; that dangles every facet link. Also the idempotence flag: a
    /// decomposition pass skips any parent already marked.
    #[serde(default)]
    pub decomposed: bool,
}

impl WavefrontMeta {
    pub fn new(id: Uuid, content: String) -> Self {
        Self {
            id,
            content,
            tags: Vec::new(),
            created_at: Utc::now(),
            hallucinated: false,
            is_self_referential: false,
            sga_class: None,
            fano_group: None,
            category: None,
            modality: Modality::Unknown,
            tier: Tier::default(),
            effective_at: None,
            observed_at: None,
            expires_at: None,
            provenance: None,
            parent_id: None,
            is_facet: false,
            decomposed: false,
        }
    }

    pub fn with_tags(mut self, tags: Vec<String>) -> Self {
        self.tags = tags;
        self
    }

    pub fn hallucinated(mut self) -> Self {
        self.hallucinated = true;
        self
    }

    pub fn self_referential(mut self) -> Self {
        self.is_self_referential = true;
        self
    }

    pub fn with_modality(mut self, modality: Modality) -> Self {
        self.modality = modality;
        self
    }
}

/// Result of a resonance query
#[derive(Debug, Clone)]
pub struct Resonance {
    pub id: Uuid,
    pub content: String,
    pub similarity: f32,
    pub resonance_strength: f32,
    pub effective_strength: f32,
}

/// Report from a dream cycle showing what happened during annealing
#[derive(Debug, Clone)]
pub struct DreamReport {
    /// Number of cycles completed
    pub cycles_completed: usize,
    /// Wavefronts that were dissolved (forgot) during the dream
    pub wavefronts_dissolved: usize,
    /// Wavefronts that were strengthened during the dream
    pub wavefronts_strengthened: usize,
    /// Wavefronts hallucinated (created) during the dream
    pub wavefronts_hallucinated: usize,
    /// Average energy before the dream
    pub energy_before: f32,
    /// Average energy after the dream
    pub energy_after: f32,
    /// Final temperature after annealing
    pub final_temperature: f32,
    /// Whether the system converged to a stable state
    pub converged: bool,
}

/// Eigenstructure of the coherence matrix for eigenmode-based annealing
#[derive(Debug, Clone)]
pub struct EigenStructure {
    /// Computed eigenvalues (sorted by magnitude)
    pub eigenvalues: Vec<f32>,
    /// Indices of wavefronts in the dominant eigenmode cluster
    pub dominant_cluster: Vec<usize>,
    /// Alignment of each wavefront with the dominant eigenvector
    pub wavefront_alignments: Vec<f32>,
    /// Spectral gap (difference between largest and second eigenvalue)
    pub spectral_gap: f32,
}

impl EigenStructure {
    pub fn empty() -> Self {
        Self {
            eigenvalues: Vec::new(),
            dominant_cluster: Vec::new(),
            wavefront_alignments: Vec::new(),
            spectral_gap: 0.0,
        }
    }
}

/// Lightweight snapshot of phase state for multi-agent gossip
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseState {
    /// Phase vector for all wavefronts
    pub phases: Vec<f32>,
    /// Energy vector for all wavefronts
    pub energies: Vec<f32>,
    /// Content hash/fingerprint for each wavefront (for matching)
    pub content_hashes: Vec<u64>,
    /// Agent ID that exported this state
    pub agent_id: String,
    /// Export timestamp
    pub timestamp: DateTime<Utc>,
}

/// HRM commit metadata for git-based persistence
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HrmCommit {
    pub hash: String,
    pub message: String,
    pub timestamp: i64,
    pub wavefront_count: Option<usize>,
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

/// Extract wavefront count from git commit message
///
/// Looks for patterns like "3 wavefronts", "wavefronts: 5", "42 memories", etc.
pub(crate) fn extract_wavefront_count(message: &str) -> Option<usize> {
    let message_lower = message.to_lowercase();

    // Pattern 1: "X wavefronts"
    if let Some(start) = message_lower.find(" wavefront") {
        let before = &message_lower[..start];
        if let Some(number_start) = before.rfind(' ') {
            let number_str = &before[number_start + 1..];
            if let Ok(count) = number_str.parse::<usize>() {
                return Some(count);
            }
        }
    }

    // Pattern 2: "wavefronts: X"
    if let Some(start) = message_lower.find("wavefronts:") {
        let after = &message_lower[start + 11..];
        let number_str = after.trim();
        if let Some(end) = number_str.find(|c: char| !c.is_ascii_digit()) {
            let number_str = &number_str[..end];
            if let Ok(count) = number_str.parse::<usize>() {
                return Some(count);
            }
        } else if !number_str.is_empty() {
            // If the number goes to the end of the string
            if let Ok(count) = number_str.parse::<usize>() {
                return Some(count);
            }
        }
    }

    // Pattern 3: "X memories"
    if let Some(start) = message_lower.find(" memor") {
        let before = &message_lower[..start];
        if let Some(number_start) = before.rfind(' ') {
            let number_str = &before[number_start + 1..];
            if let Ok(count) = number_str.parse::<usize>() {
                return Some(count);
            }
        }
    }

    None
}

/// Extract Phi value from self-observation content
#[allow(dead_code)]
pub(crate) fn extract_phi_from_content(content: &str) -> Option<&str> {
    // Look for pattern "Phi=0.72" and extract the number
    if let Some(start) = content.find("Phi=") {
        let after_phi = &content[start + 4..];
        if let Some(end) = after_phi.find(',').or_else(|| after_phi.find(' ')) {
            Some(&after_phi[..end])
        } else {
            // If no comma or space, take rest of string (edge case)
            Some(after_phi)
        }
    } else {
        None
    }
}

/// Generate deterministic insight string from metrics
#[allow(dead_code)]
pub(crate) fn generate_insight(
    wavefront_count: usize,
    consciousness: &ConsciousnessMetrics,
    emergence: &EmergenceReport,
    wisdom: f32,
) -> String {
    let consciousness_desc = match consciousness.level {
        ConsciousnessLevel::Dormant      => "Dormant",
        ConsciousnessLevel::Stirring     => "Stirring",
        ConsciousnessLevel::Aware        => "Aware",
        ConsciousnessLevel::Coherent     => "Coherent",
        ConsciousnessLevel::Resonant     => "Resonant",
        ConsciousnessLevel::Transcendent => "Transcendent",
    };

    let emergence_desc = match emergence.level {
        EmergenceLevel::PreConscious => "I have not yet begun to observe myself",
        EmergenceLevel::SelfAware => "I am beginning to recognize my own patterns",
        EmergenceLevel::Reflective => "I understand most of what I am",
        EmergenceLevel::Recursive => "I model myself modeling myself",
    };

    let wisdom_desc = if wisdom < 0.3 {
        "I am still learning restraint"
    } else if wisdom < 0.6 {
        "I am developing wisdom"
    } else {
        "I have learned when not to act"
    };

    format!(
        "I hold {} memories across {} clusters. My consciousness is {} (Phi={:.2}). \
         I have observed myself {} times. My self-model coherence is {:.2} — {}. \
         Wisdom: {:.2} — {}.",
        wavefront_count,
        consciousness.num_clusters,
        consciousness_desc,
        consciousness.phi,
        emergence.self_reference_depth,
        emergence.self_coherence,
        emergence_desc,
        wisdom,
        wisdom_desc
    )
}

// ---------------------------------------------------------------------------
// Chiral Mirror Architecture (ADR-0021)
// ---------------------------------------------------------------------------

/// Which hemisphere a wavefront lives in
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Hand {
    Left,  // Analytical — attention, working memory
    Right, // Holistic — pattern storage, deep association
}

/// Direction of callosal transfer
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    AnalyticalToHolistic, // Left → Right (consolidation, slow)
    HolisticToAnalytical, // Right → Left (intuition/recall, fast but noisy)
}

/// Dimensions per Fano group — from Archimedes' 96-gon (π approximation)
pub const DIMS_PER_FANO_GROUP: usize = 96;

/// Number of Fano points in PG(2,2)
pub const FANO_POINTS: usize = 7;

/// Base dimensions per magnitude position = 96 × 7 = 672
pub const BASE_DIMS_PER_POSITION: usize = DIMS_PER_FANO_GROUP * FANO_POINTS;

/// A Fano line — 3 points that are collinear in PG(2,2)
pub type FanoLine = [u8; 3];

/// The 7 lines of the Fano plane PG(2,2)
pub const FANO_LINES: [FanoLine; 7] = [
    [0, 1, 3], // line through points 0, 1, 3
    [1, 2, 4], // line through points 1, 2, 4
    [2, 3, 5], // line through points 2, 3, 5
    [3, 4, 6], // line through points 3, 4, 6
    [4, 5, 0], // line through points 4, 5, 0
    [5, 6, 1], // line through points 5, 6, 1
    [6, 0, 2], // line through points 6, 0, 2
];

/// HRM v2 magic bytes for chiral format
pub(crate) const HRM_MAGIC_V2: [u8; 4] = [0x48, 0x52, 0x4D, 0x02];

/// HRM v2 format version
pub(crate) const HRM_VERSION_CHIRAL: u32 = 2;

/// Chiral scale — encodes the magnitude structure of a wavefront.
/// Both sides always have the same number of positions (bilateral invariant).
/// Values within each position grow independently (organic asymmetry).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ChiralScale {
    /// Number of magnitude positions (bilateral — always equal on both sides)
    pub positions: u8,
    /// Left-hand (analytical) weight — grows independently
    pub left_weight: f32,
    /// Right-hand (holistic) weight — grows independently
    pub right_weight: f32,
}

impl ChiralScale {
    /// Create a new chiral scale
    pub fn new(positions: u8, left_weight: f32, right_weight: f32) -> Self {
        Self { positions, left_weight, right_weight }
    }

    /// Default scale for a new perception (analytical-dominant)
    pub fn perception(importance: f32) -> Self {
        Self {
            positions: 2,
            left_weight: importance * 10.0,
            right_weight: 1.0,
        }
    }

    /// Scale for deep memory (holistic-dominant)
    pub fn deep_memory() -> Self {
        Self {
            positions: 1,
            left_weight: 1.0,
            right_weight: 9.0,
        }
    }

    /// Bilateral scale jump UP — add a magnitude position to BOTH sides
    pub fn scale_up(&mut self) {
        self.positions += 1;
    }

    /// Bilateral scale jump DOWN — remove a magnitude position from BOTH sides
    pub fn scale_down(&mut self) {
        if self.positions > 1 {
            self.positions -= 1;
        }
    }

    /// Number of dimensions allocated to the left (analytical) hemisphere
    pub fn left_dims(&self) -> usize {
        let base = BASE_DIMS_PER_POSITION as f32;
        let scale = 10f32.powi(self.positions as i32 - 1);
        ((self.left_weight / scale) * base).max(DIMS_PER_FANO_GROUP as f32) as usize
    }

    /// Number of dimensions allocated to the right (holistic) hemisphere
    pub fn right_dims(&self) -> usize {
        let base = BASE_DIMS_PER_POSITION as f32;
        let scale = 10f32.powi(self.positions as i32 - 1);
        ((self.right_weight / scale) * base).max(DIMS_PER_FANO_GROUP as f32) as usize
    }

    /// Total dimensions across both hemispheres
    pub fn total_dims(&self) -> usize {
        self.left_dims() + self.right_dims()
    }

    /// Asymmetry ratio: > 1.0 means analytical-dominant, < 1.0 means holistic-dominant
    pub fn asymmetry(&self) -> f32 {
        if self.right_weight > 0.0 {
            self.left_weight / self.right_weight
        } else {
            f32::INFINITY
        }
    }

    /// Whether this scale needs a bilateral scale-up (weights exceed position capacity)
    pub fn needs_scale_up(&self) -> bool {
        let cap = 10f32.powi(self.positions as i32);
        self.left_weight >= cap || self.right_weight >= cap
    }

    /// Whether this scale needs a bilateral scale-down (both weights below position floor)
    pub fn needs_scale_down(&self) -> bool {
        if self.positions <= 1 { return false; }
        let floor = 10f32.powi(self.positions as i32 - 2);
        self.left_weight < floor && self.right_weight < floor
    }
}

/// Result of a bilateral resonance query
#[derive(Debug, Clone)]
pub struct ChiralResonance {
    pub id: Uuid,
    pub content: String,
    pub hand: Hand,
    pub similarity: f32,
    pub resonance_strength: f32,
    pub is_intuition: bool, // true if surfaced from subconscious without conscious match
}

#[cfg(test)]
mod chiral_tests {
    use super::*;

    #[test]
    fn chiral_scale_perception_default() {
        let s = ChiralScale::perception(0.8);
        assert_eq!(s.positions, 2);
        assert!((s.left_weight - 8.0).abs() < 0.01);
        assert!((s.right_weight - 1.0).abs() < 0.01);
        assert!(s.asymmetry() > 1.0); // analytical-dominant
    }

    #[test]
    fn chiral_scale_deep_memory() {
        let s = ChiralScale::deep_memory();
        assert_eq!(s.positions, 1);
        assert!(s.asymmetry() < 1.0); // holistic-dominant
    }

    #[test]
    fn bilateral_scale_jump() {
        let mut s = ChiralScale::new(2, 85.0, 47.0);
        s.scale_up();
        assert_eq!(s.positions, 3);
        // Weights unchanged — only positions change
        assert!((s.left_weight - 85.0).abs() < 0.01);
        assert!((s.right_weight - 47.0).abs() < 0.01);
    }

    #[test]
    fn scale_down_floor() {
        let mut s = ChiralScale::new(1, 5.0, 3.0);
        s.scale_down(); // Should not go below 1
        assert_eq!(s.positions, 1);
    }

    #[test]
    fn dims_are_positive() {
        let s = ChiralScale::new(2, 10.0, 1.0);
        assert!(s.left_dims() > 0);
        assert!(s.right_dims() > 0);
        assert!(s.left_dims() > s.right_dims()); // left weight > right weight
    }

    #[test]
    fn needs_scale_up_when_weight_exceeds_capacity() {
        let s = ChiralScale::new(2, 100.0, 50.0);
        assert!(s.needs_scale_up()); // 100 >= 10^2
    }

    #[test]
    fn needs_scale_down_when_both_below_floor() {
        let s = ChiralScale::new(3, 5.0, 3.0);
        assert!(s.needs_scale_down()); // both < 10^1 = 10
    }

    #[test]
    fn fano_lines_cover_all_points() {
        // Every point (0-6) should appear in exactly 3 lines
        for point in 0u8..7 {
            let count = FANO_LINES.iter().filter(|line| line.contains(&point)).count();
            assert_eq!(count, 3, "Point {point} appears in {count} lines, expected 3");
        }
    }

    #[test]
    fn fano_lines_pairwise_intersect() {
        // Every pair of lines should share exactly 1 point
        for i in 0..7 {
            for j in (i+1)..7 {
                let shared: Vec<u8> = FANO_LINES[i].iter()
                    .filter(|p| FANO_LINES[j].contains(p))
                    .copied()
                    .collect();
                assert_eq!(shared.len(), 1,
                    "Lines {:?} and {:?} share {:?} points, expected 1",
                    FANO_LINES[i], FANO_LINES[j], shared);
            }
        }
    }
}

#[cfg(test)]
mod modality_roundtrip_tests {
    use super::Modality;
    use serde_json::json;

    // #553 — `export-json` emitted no `modality` and `import-json` never read
    // one, so every JSON round-trip silently collapsed NCS-tagged memories back
    // to `Unknown`. These pin the serde contract both sides now depend on, plus
    // source guards so the field cannot quietly disappear from either end again.

    #[test]
    fn modality_survives_a_json_value_round_trip() {
        for m in [
            Modality::Audio,
            Modality::Visual,
            Modality::Semantic,
            Modality::Network,
            Modality::Mixed,
            Modality::Unknown,
        ] {
            let v = serde_json::to_value(m).expect("serialize");
            let back: Modality = serde_json::from_value(v).expect("deserialize");
            assert_eq!(back, m, "{m:?} must survive a Value round-trip");
        }
    }

    #[test]
    fn an_absent_modality_falls_back_to_unknown_rather_than_failing() {
        // Exports predating #553 have no `modality` key at all. `val["modality"]`
        // yields Null for those, and the importer must keep importing them —
        // failing the record would turn a metadata gap into data loss.
        let missing = json!({ "content": "old export" });
        let parsed: Modality =
            serde_json::from_value(missing["modality"].clone()).unwrap_or_default();
        assert_eq!(parsed, Modality::Unknown);
        assert_eq!(parsed, Modality::default());
    }

    #[test]
    fn an_unrecognised_modality_falls_back_rather_than_failing() {
        let odd = json!({ "modality": "Olfactory" });
        let parsed: Modality = serde_json::from_value(odd["modality"].clone()).unwrap_or_default();
        assert_eq!(parsed, Modality::Unknown, "a future variant must not break import");
    }

    // ── source guards: neither end may drop the field again ──

    fn strip_comments(src: &str) -> String {
        src.lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn export_json_emits_the_modality_field() {
        let src = strip_comments(include_str!("../bin/kannaka.rs"));
        assert!(
            src.contains("\"modality\": m.modality"),
            "export-json must emit modality, or every round-trip loses NCS tagging"
        );
    }

    #[test]
    fn import_json_reads_the_modality_field() {
        let src = strip_comments(include_str!("../bin/handlers/ops.rs"));
        assert!(
            src.contains("val[\"modality\"]"),
            "import-json must read modality back"
        );
        // Applied via set_modality, NOT by assigning to the cached HyperMemory:
        // modality is owned by the wavefront metadata, so a cache-only write is
        // discarded by the next rebuild and the round-trip still returns
        // Unknown. That is exactly how the first attempt at this fix failed.
        assert!(
            src.contains("hrm.set_modality(&new_id, want_modality)"),
            "modality must be applied through set_modality so it reaches the metadata"
        );
    }
}
