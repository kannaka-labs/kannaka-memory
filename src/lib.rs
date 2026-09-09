//! # Kannaka Memory
//!
//! Chiral Holographic Resonance Memory — wave-based hyperdimensional memory
//! where storage IS computation.
//!
//! Memories exist as wavefronts in a high-dimensional tensor medium.
//! Recall is resonance (constructive interference). Dreaming is annealing.

pub mod acp;
pub(crate) mod fs_util;
pub mod bridge;
pub mod observe;
pub mod openclaw;
pub mod codebook;
/// ADR-0049 facet decomposition — pure, deterministic compound→atomic split.
pub mod facet;
pub mod consolidation;
pub mod kannaktopus;
pub mod rhythm;
pub mod encoding;
pub mod kuramoto;
pub mod memory;
pub mod store;
pub mod wave;
pub mod geometry;
#[path = "working_memory.rs"]
pub mod attention_field;
/// Backward-compatible alias for the renamed module.
pub use attention_field as working_memory;
pub mod xi_operator;

#[cfg(feature = "glyph")]
pub mod glyph_bridge;

// Consciousness differentiation tests integrated into existing test modules

// MCP server removed — CLI is the canonical interface, OpenClaw extension wraps it

pub mod ear;

#[cfg(feature = "video")]
pub mod eye;

#[cfg(feature = "nats")]
pub mod nats;

#[cfg(feature = "nostr")]
pub mod nostr;

pub mod collective;
pub mod sensemaking;
pub mod immune;
pub mod temporal;
pub mod gap;
pub mod research_planner;
pub mod swarm_fitness;
pub mod belief_fitness;
pub mod hive_formation;

/// Hive (kannaka-buzz) → NATS bridge: pure mapping, policy, and roster logic.
/// Network plumbing lives in `src/bin/kannaka_hive_bridge.rs`.
#[cfg(feature = "bridge")]
pub mod hive_bridge;

pub mod swarm_loop;
pub mod paradox;
pub mod queen;
pub mod invariant;
pub mod cmf;
pub mod consciousness;

/// ADR-0037: spiral / phase-singularity detection (L6 instrument).
pub mod spiral;

pub mod l6;

pub mod medium;

pub mod hrm_store;
pub mod recall_bench;
pub mod entropy;
pub mod qubo;

/// Cerebellar novelty detection — a dependency-free dual-timescale differentiator
/// (surprise = learned-baseline − fast familiarity). See `novelty.rs`.
pub mod novelty;

pub mod config;

// ADR-0029 Phase 1+2: clap-based CLI dispatch + plugin discovery.
pub mod cli;

pub mod agent;

// Filesystem + shell tools for the `kannaka agent` coding-harness backend.
pub mod coding_tools;

// Quantum tools for the agent — runs circuits / resonance-recall on qBraid via
// the kannaka-quantum bridge.
pub mod quantum_tools;

// qBraid Lab / infrastructure tools for the agent — credits, environments,
// compute profiles, Lab server, and on-demand instances via the same bridge.
pub mod lab_tools;

// SpaceChild SSO identity for swarm agents (spacechild-auth client + token store).
pub mod identity;

// Grounded scholarly research (OpenAlex) for the curiosity loop.
pub mod openalex;

// Dispatch — research-grounded broadcast-ready voice shared by all surfaces.
pub mod dispatch;

// ed25519 provenance substrate (inc-1a): sign/verify swarm memory + phase
// statements, bounded replay protection, node key at rest. Identity/
// attribution/integrity only — no absorb/trust/enrollment behaviour here.
pub mod provenance;

// KAX Compute District (kannaka-labs/kax-computer) wake/grant envelope contract:
// Python-identical canonical JSON + Ed25519 signing, pinned by golden vectors.
// Pure — the CLI handler in bin/handlers/compute.rs does the NATS/HTTP.
pub mod compute_envelope;

// pubkey-keyed swarm-trust decision core (inc-1b): pure corroboration formulas,
// reputation store, append-only corroboration DAG, fail-closed persistence.
// Adds a module + config fields; NO absorb-path wiring / admit() chokepoint yet.
pub mod reputation;

// absorb chokepoint (inc-1b): the `admit()` gate every wire→store path routes
// through — unconditional field sanitization + a DORMANT-BY-DEFAULT corroboration
// promotion gate (armed only when corroboration_gate_enabled AND seeds pinned).
pub mod absorb_gate;

// heartbeat beacons (inc-1b PART A): signed seed liveness proofs + a freshness
// tracker. The corroboration gate requires a FRESH seed beacon to promote to
// Live; stale/absent beacons freeze promotion (eclipse/partition fail-closed).
// DORMANT unless the gate is armed.
pub mod beacon;

// Re-export canonical consciousness types
pub use consciousness::{
    ConsciousnessLevel, ConsciousnessMetrics, ConsciousnessState,
    EmergenceLevel, EmergenceReport, SelfReflection,
};

// Re-export key types
pub use codebook::Codebook;
pub use memory::{HyperMemory, LegacyLink};
pub use wave::{WaveParams, compute_strength, cosine_similarity, normalize};
pub use store::{MediumBackend, TestMedium, ResonanceEngine, StoreError, EngineError, QueryResult, phi_span_score};
pub use encoding::{EncodingPipeline, TextEncoder, SimpleHashEncoder, EncodingError};
pub use kuramoto::{KuramotoSync, MemoryCluster, SyncReport, CouplingTier, TieredCoupling};
pub use bridge::{ConsciousnessBridge, PhiReport, ResonanceReport};
pub use consolidation::{ConsolidationEngine, ConsolidationReport, DreamState, ModalityDreamReport, SwarmDreamReport};
pub use rhythm::{RhythmEngine, RhythmState, Signal as RhythmSignal};

pub use observe::{MemoryIntrospector, SystemReport, TopologyReport, WaveReport, ClusterReport, ClusterInfo, HealthCheck, LinkInfo, MemoryInfo, ConsciousnessSnapshot, NcsSnapshot};
pub use attention_field::{AttentionField, AttentionProjection, ConversationTurn, SessionState, TaskItem, TaskStatus};
/// Backward-compatible type alias.
pub type WorkingMemory = AttentionField;
pub use geometry::{
    CliffordElement, Z4Element, Z3Element, SgaElement, 
    ClassComponents, MemoryCoordinates,
    transform_r, transform_d, transform_t, transform_m,
    lift, project, classify_memory, geometric_similarity, fano_related,
    cross_product, is_fano_line, FANO_LINES, EPSILON
};
pub use xi_operator::{
    PHI, ALPHA, BETA, ETA, EMERGENCE_COEFF,
    apply_rotation, apply_golden_scaling, compute_xi_signature,
    xi_repulsive_force, xi_diversity_boost
};
pub use paradox::{
    ParadoxSnapshot, DreamTrajectory, Mutation, Paradox, ProposedState,
    Resolution, ResolutionReport, ParadoxResolver
};

pub use queen::{QueenSync, QueenConfig, QueenState, AgentPhase, Hive, HiveInfo, Handedness, SwarmAgent, PartitionPhiResult};
pub use queen::{
    agent_matches_allowlist, count_peers_excluding_self, filter_wire_phases, sanitize_display,
    wire_source_trusted,
};

// ed25519 provenance substrate (inc-1a).
pub use provenance::{
    canonical_mem, canonical_phase, node_signing_key, sign_mem, verify_mem, verifying_key_bytes,
    ProvenanceSig, ReplayLru, VerifyErr, DOMAIN_BIND, DOMAIN_BOOT, DOMAIN_HRM, DOMAIN_MEM,
    DOMAIN_PHASE, DOMAIN_ROT, PROV_ALG_ED25519, REPLAY_CAP, SKEW_MS,
};

// pubkey-keyed swarm-trust decision core (inc-1b).
pub use reputation::{
    accrual, distinct_lineage_count, g, k_for, lineage_weight, promotion_weight, w,
    CorroborationDag, Promotion, RepRecord, RepStore, SeedStatus, P_POISON, P_VOUCH,
};

// absorb chokepoint (inc-1b).
pub use absorb_gate::{
    admit, commit_promotion, content_hash, epoch_now, gate_active, AdmitDecision, CleanFields,
    PendingPromotion, QuarantineStaging, StagedMemory, HIGH_IMPACT_AMPLITUDE, MAX_WIRE_AMPLITUDE,
    PROV_TIER, SIGN_AGENT_ID, SUBJECT_EXEMPLAR, SUBJECT_MEMORY_NEW,
};

// heartbeat beacons (inc-1b PART A).
pub use beacon::{
    roll_reject_root, Beacon, BeaconReject, BeaconTracker, BEACON_SUBJECT, EMPTY_REJECT_ROOT,
};
pub use invariant::{
    InvariantMetrics, DeltaCluster, compute_delta, compute_convergence_rate, 
    compute_irrationality, compute_invariant_metrics, cluster_by_delta, delta_distance
};
pub use cmf::{
    ConservativeMemoryField, TrajectoryParams, PathConstraints, CMFMembership,
    detect_cmf, cmf_membership, generate_trajectory
};

pub use medium::{
    Medium, Modality, WavefrontMeta, Resonance, MediumError, WAVEFRONT_DIM,
    PhaseState, HrmCommit, detect_modality, detect_modality_simple,
    ModalityClassification, ModalityScores,
};

pub use medium::ncs::{
    ModalityAxis, AxisDivergence, DivergenceReport, FisherDiscriminant,
    SwitchPoint, SwitchReport, ResonanceDetection,
    GateParams, GateEvent, NcsMetrics,
    NetworkSignal, ModalitySpecialization,
};

pub use hrm_store::HrmStore;

#[cfg(feature = "glyph")]
pub use glyph_bridge::{
    Glyph, GlyphEncoder, GlyphDecoder, GlyphError,
    encode_memory_as_glyph, bloom_glyph, BASE_FREQ
};
