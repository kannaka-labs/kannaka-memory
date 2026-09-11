//! Kannaka Constellation configuration.
//!
//! Manages `~/.kannaka/config.toml` — the single source of truth for agent
//! identity, LLM provider, swarm settings, GhostSignals, and update preferences.
//!
//! Config precedence (highest to lowest):
//! 1. Environment variables (`KANNAKA_AGENT_ID`, `KANNAKA_NATS_URL`, etc.)
//! 2. `~/.kannaka/config.toml`
//! 3. Built-in defaults

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Top-level Kannaka configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct KannakaConfig {
    #[serde(default = "AgentConfig::default")]
    pub agent: AgentConfig,
    #[serde(default = "LlmConfig::default")]
    pub llm: LlmConfig,
    #[serde(default = "SwarmConfig::default")]
    pub swarm: SwarmConfig,
    #[serde(default = "GhostSignalsConfig::default")]
    pub ghostsignals: GhostSignalsConfig,
    #[serde(default = "ConstellationConfig::default")]
    pub constellation: ConstellationConfig,
    #[serde(default = "HrmConfig::default")]
    pub hrm: HrmConfig,
    #[serde(default = "UpdatesConfig::default")]
    pub updates: UpdatesConfig,
    #[serde(default = "TriageConfig::default")]
    pub triage: TriageConfig,
    /// ADR-0054 retention policy: content-prefix → cap/TTL rules enforced by
    /// the dream cycle's `stage_retention_triage` (gated by KANNAKA_TRIAGE=1).
    /// Empty (the default) = the stage is a no-op.
    #[serde(default)]
    pub retention: std::collections::HashMap<String, RetentionRule>,
    #[serde(default = "BeliefConfig::default")]
    pub belief: BeliefConfig,
    #[serde(default = "ClusterConfig::default")]
    pub cluster: ClusterConfig,
    #[serde(default = "CouplingConfig::default")]
    pub coupling: CouplingConfig,
    #[serde(default = "EntropyConfig::default")]
    pub entropy: EntropyConfig,
    #[serde(default = "SwarmTrustConfig::default")]
    pub swarm_trust: SwarmTrustConfig,
    #[serde(default = "EncoderConfig::default")]
    pub encoder: EncoderConfig,
}

/// Text-encoder selection for the HRM pipeline.
///
/// Measured 2026-08-02 (evals/semantic-encoder): a semantic encoder through the
/// production chiral path scores 2.6x-4.4x the hash encoder's recall@10. Default
/// stays `hash` — byte-identical to the pre-config behavior — per the ship-dark
/// discipline; enable `ollama` on measured evidence per deployment.
///
/// A store's vectors are ENCODER-SPECIFIC: recalling a hash-encoded store with a
/// semantic query vector (or vice versa) degrades to noise with no error. The
/// runtime therefore stamps `<data_dir>/.encoder` on first use and refuses a
/// mismatched encoder on later runs (see `build_encoding_pipeline`). Switching
/// encoders on a populated store requires a one-time re-encode (id-preserving
/// recipe: evals/semantic-encoder/semantic-eval.rs).
///
/// Env overrides (highest precedence, for eval arms and one-off runs):
/// `KANNAKA_ENCODER` (hash|ollama), `KANNAKA_ENCODER_URL`,
/// `KANNAKA_ENCODER_MODEL`, `KANNAKA_ENCODER_DIM`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncoderConfig {
    #[serde(default = "default_encoder_kind")]
    pub kind: String,
    #[serde(default = "default_encoder_url")]
    pub base_url: String,
    #[serde(default = "default_encoder_model")]
    pub model: String,
    #[serde(default = "default_encoder_dim")]
    pub dim: u32,
}

fn default_encoder_kind() -> String { "hash".to_string() }
fn default_encoder_url() -> String { "http://localhost:11434".to_string() }
fn default_encoder_model() -> String { "all-minilm".to_string() }
fn default_encoder_dim() -> u32 { 384 }

impl Default for EncoderConfig {
    fn default() -> Self {
        Self {
            kind: default_encoder_kind(),
            base_url: default_encoder_url(),
            model: default_encoder_model(),
            dim: default_encoder_dim(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    #[serde(default = "default_agent_id")]
    pub id: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default = "default_agent_kind")]
    pub kind: String,
    /// The agent's self-description, woven into the `ask` system prompt so a
    /// consumer gets the RIGHT self without framing (#412). When empty, the
    /// prompt builds a neutral identity from this agent's own id, display
    /// name, and store metadata — never borrowing another agent's biography.
    /// Kannaka's rich "I am the medium, speaking" persona lives HERE, in her
    /// config, rather than being the binary's default for every agent.
    #[serde(default)]
    pub persona: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmConfig {
    #[serde(default = "default_llm_provider")]
    pub provider: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub base_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwarmConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_nats_url")]
    pub nats_url: String,
    #[serde(default = "default_role")]
    pub role: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GhostSignalsConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_hub_url")]
    pub hub_url: String,
    #[serde(default)]
    pub token: String,
    /// KAX identity provider base URL (mints/refreshes identity tokens).
    #[serde(default = "default_kax_url")]
    pub kax_url: String,
    /// KAX identity token (EdDSA JWT) — required for labs-tier trading. Drop
    /// one in with `kannaka market auth <jwt>`; the CLI self-refreshes it via
    /// KAX `/api/auth/token/refresh` until the lineage's max lifetime.
    #[serde(default)]
    pub kax_token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConstellationConfig {
    #[serde(default = "default_radio_url")]
    pub radio_url: String,
    #[serde(default = "default_observatory_url")]
    pub observatory_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HrmConfig {
    #[serde(default)]
    pub path: String,
    #[serde(default = "default_wavefront_dim")]
    pub wavefront_dim: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdatesConfig {
    #[serde(default = "default_true")]
    pub auto_check: bool,
    #[serde(default = "default_channel")]
    pub channel: String,
    #[serde(default)]
    pub last_checked: String,
}

/// ADR-0054 per-prefix retention rule. Matches on CONTENT prefix (the only
/// key that persists across restarts — categories are runtime-only) — the
/// same key the retired radio prune-cron counted on ("audio:" rows).
///
/// ```toml
/// [retention]
/// "audio:" = { cap = 200, ttl_days = 14 }
/// ```
///
/// `cap`: beyond this many live matching rows, the excess is GHOSTED
/// (ADR-0037 two-phase — never hard-deleted here), lowest
/// `(retrieval_count, amplitude)` first so replayed memories survive.
/// `ttl_days`: matching rows older than this ghost regardless of cap.
/// Pinned rows and rows at/above KANNAKA_PROMOTE_HITS retrievals are
/// never touched (ADR-0054 acceptance #3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetentionRule {
    #[serde(default)]
    pub cap: Option<usize>,
    #[serde(default)]
    pub ttl_days: Option<f32>,
}

/// ADR-0031 memory triage policy. Per-agent tunable — the witness, substrate,
/// and radio have different redundancy profiles. `enabled` gates the dream-cycle
/// auto-trigger (Phase 3); the explicit `kannaka triage` CLI works regardless.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriageConfig {
    /// Enable the dream-cycle auto-trigger (default false — opt-in, since it
    /// auto-deletes redundant short-term memories).
    #[serde(default)]
    pub enabled: bool,
    /// Same-modality cosine at/above which a memory is a redundant extra.
    #[serde(default = "default_triage_redundancy")]
    pub redundancy: f32,
    /// Only memories with amplitude below this are eviction-eligible.
    #[serde(default = "default_triage_min_amplitude")]
    pub min_amplitude: f32,
    /// Only memories older than this (hours) are eviction-eligible.
    #[serde(default = "default_triage_min_age_hours")]
    pub min_age_hours: i64,
    /// Cap on evictions per pass.
    #[serde(default = "default_triage_max_evict")]
    pub max_evict: usize,
    /// Dream auto-triggers triage when post-dream Ξ falls below this. 0 disables
    /// the auto-trigger even when `enabled` (explicit CLI triage still works).
    #[serde(default = "default_triage_xi_trigger")]
    pub xi_trigger: f32,
}

/// ADR-0037 belief substrate. `enabled` turns on the content-smooth born phase
/// (and the belief dream dynamics / spiral belief-formation layer). **Default
/// OFF** so a field is byte-identical until activated. `max_n` caps the O(n²)
/// belief-coupling PCA on under-provisioned nodes (the 1-core hub sets 0 to skip
/// it; re-phase still works). The `KANNAKA_BELIEF_PHASE` / `KANNAKA_BELIEF_MAX_N`
/// env vars OVERRIDE these — `apply_belief_env_from_config` bridges config→env at
/// startup only when the env var is unset, so a dream-cron / systemd `Environment=`
/// still wins. Managed via `kannaka belief on|off|status|activate`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeliefConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_belief_max_n")]
    pub max_n: usize,
}

/// num_clusters fix: "decone" (mean-center + top-PC removal) for the cluster
/// detector (`cluster_decone_enabled` in `kuramoto`). env `KANNAKA_CLUSTER_DECONE`
/// OVERRIDES this; `apply_cluster_env_from_config` bridges config→env at startup
/// only when the env var is unset. Default off.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClusterConfig {
    #[serde(default)]
    pub decone: bool,
}

/// ADR-0037 Track-D: always-on heartbeat belief coupling. env KANNAKA_EXEMPLAR_
/// COUPLING OVERRIDES this; `apply_coupling_env_from_config` bridges config→env at
/// startup only when the env var is unset. Default off — the riskiest Track-D step;
/// enable per-node, observer-first. Cadence/min_cos tune via the _TICKS/_MIN_COS env.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CouplingConfig {
    #[serde(default)]
    pub enabled: bool,
}

/// Quantum-Wave T1.3 (#473): which entropy source seeds dreams / Ξ. env
/// `KANNAKA_ENTROPY_SOURCE` OVERRIDES this; `apply_entropy_env_from_config`
/// bridges config→env at startup only when the env var is unset. **Default
/// `reservoir`** as of the T1.5 flip (#475) — Nick approved it on 5 clean
/// dogfood days. Flipping the SOURCE alone changes nothing observable: the
/// separate `dream_perturbation` consumption gate stays **default false**, so
/// no dream draws from the reservoir (and no `kannaka-quantum` CLI dependency
/// is introduced) until a deployment explicitly opts in. When it IS on, the
/// reservoir fails LOUDLY on an empty/missing CLI — never a silent PRNG
/// fallback. Set `source = "prng"` to opt back out.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntropyConfig {
    /// `"reservoir"` (default, T1.5) or `"prng"`.
    #[serde(default = "default_entropy_source")]
    pub source: String,
    /// T1.4: whether dreams/Ξ actually CONSUME entropy from `source` (and record
    /// its provenance). **Default false** — decoupled from `source` so the dream
    /// stays deterministic until explicitly opted in. env `KANNAKA_DREAM_ENTROPY`.
    #[serde(default)]
    pub dream_perturbation: bool,
}

impl Default for EntropyConfig {
    fn default() -> Self {
        Self { source: default_entropy_source(), dream_perturbation: false }
    }
}

fn default_entropy_source() -> String {
    // T1.5 flip (#475): reservoir is now the default source. The
    // dream_perturbation gate (default false) still governs whether anything
    // is actually drawn, so this default is inert until a deployment opts in.
    "reservoir".to_string()
}

/// SECURITY (increment-0): read-side trust gate for the OPEN NATS swarm.
/// Anonymous publish stays allowed, so the read side must not trust
/// attacker-controlled wire fields. `trusted_agents` is an allowlist of
/// agent-ids — each entry is either an exact id or a `prefix*` wildcard
/// (e.g. `"qos-*"`). When `metrics_trusted_only` is set (default), only
/// allowlisted (plus this node's own) phases feed the swarm metrics, and
/// every kept phase has its wire `trust_score` clamped to `wire_trust_cap`.
/// env: `KANNAKA_TRUSTED_AGENTS` (comma-separated, REPLACES the list) and
/// `KANNAKA_METRICS_TRUSTED_ONLY=0` (disable the metrics filter — escape hatch).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwarmTrustConfig {
    #[serde(default = "default_trusted_agents")]
    pub trusted_agents: Vec<String>,
    #[serde(default = "default_true")]
    pub metrics_trusted_only: bool,
    #[serde(default = "default_wire_trust_cap")]
    pub wire_trust_cap: f32,
    /// SECURITY (inc-1): trust threshold θ. A verified-pubkey trust score
    /// `>= trust_threshold` is Live-eligible; anything below lands in
    /// Quarantine. Consumed by the enrollment/reputation layer (lands after
    /// a design review) — inert until that path is wired.
    /// env: `KANNAKA_TRUST_THRESHOLD`.
    #[serde(default = "default_trust_threshold")]
    pub trust_threshold: f32,
    /// SECURITY (inc-1): agent-id prefixes/exact-names reserved for
    /// operator enrollment only. First-sight/self-serve enrollment for a
    /// matching id is an alarm, never an auto-pin. Consumed by the
    /// enrollment layer later — inert until wired.
    #[serde(default = "default_reserved_prefixes")]
    pub reserved_prefixes: Vec<String>,
    /// SECURITY (inc-1b): operator-pinned SEED pubkeys, base64 (standard
    /// alphabet) of the 32-byte ed25519 verifying key. **DEFAULT EMPTY** — with
    /// no seeds the corroboration gate is dormant and falls back to the inc-0
    /// read-side behaviour. Consumed by `reputation::RepStore`; the root of
    /// every trust lineage. env: `KANNAKA_SEED_PUBKEYS` (comma-separated,
    /// REPLACES the list).
    #[serde(default)]
    pub seed_pubkeys: Vec<String>,
    /// SECURITY (inc-1b): master switch for the corroboration promotion gate.
    /// **DEFAULT false** — the gate stays dormant (inc-0 fallback) until an
    /// operator pins seeds and flips this on. env: `KANNAKA_CORROBORATION_GATE`.
    #[serde(default)]
    pub corroboration_gate_enabled: bool,
    /// SECURITY (inc-1b): corroboration epoch length in ms — the freshness
    /// window an M-bound corroboration is counted within. Default 60_000.
    /// env: `KANNAKA_EPOCH_LENGTH_MS`.
    #[serde(default = "default_epoch_length_ms")]
    pub epoch_length_ms: i64,
    /// SECURITY (inc-1b): how many epochs a node may miss fresh seed beacons
    /// before it fails CLOSED and freezes promotion (anti-eclipse). Default 3.
    /// env: `KANNAKA_BEACON_GRACE_EPOCHS`.
    #[serde(default = "default_beacon_grace_epochs")]
    pub beacon_grace_epochs: u32,
    /// SECURITY (inc-1b): lower hysteresis threshold θ_lo for the continuous
    /// corroboration weight `w(rep)` — `w = 0` below this. Default 0.4.
    /// env: `KANNAKA_THETA_LO`.
    #[serde(default = "default_theta_lo")]
    pub theta_lo: f32,
    /// SECURITY (inc-1b): upper hysteresis threshold θ_hi — `w` reaches 1.0 and
    /// a handle *arms* at/above this. Default 0.7. env: `KANNAKA_THETA_HI`.
    #[serde(default = "default_theta_hi")]
    pub theta_hi: f32,
    /// SECURITY (inc-1b): per-promotion rep accrual coefficient α (also the
    /// per-epoch accrual cap). Default 0.05. env: `KANNAKA_ACCRUAL_ALPHA`.
    #[serde(default = "default_accrual_alpha")]
    pub accrual_alpha: f32,
}

impl Default for SwarmTrustConfig {
    fn default() -> Self {
        Self {
            trusted_agents: default_trusted_agents(),
            metrics_trusted_only: true,
            wire_trust_cap: default_wire_trust_cap(),
            trust_threshold: default_trust_threshold(),
            reserved_prefixes: default_reserved_prefixes(),
            seed_pubkeys: Vec::new(),
            corroboration_gate_enabled: false,
            epoch_length_ms: default_epoch_length_ms(),
            beacon_grace_epochs: default_beacon_grace_epochs(),
            theta_lo: default_theta_lo(),
            theta_hi: default_theta_hi(),
            accrual_alpha: default_accrual_alpha(),
        }
    }
}

fn default_trusted_agents() -> Vec<String> {
    [
        "Kannaka",
        "kannaka-prime",
        "0xSCADA-QE",
        "kannaka-witness-01",
        "kannaktopus-01",
        "Flaukowski",
        "qos-*",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

fn default_wire_trust_cap() -> f32 {
    0.5
}

fn default_trust_threshold() -> f32 {
    0.6
}

fn default_reserved_prefixes() -> Vec<String> {
    ["kannaka-*", "qos-*", "0xSCADA-*", "Kannaka", "Flaukowski"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

// inc-1b corroboration-trust defaults (see `reputation.rs`).
fn default_epoch_length_ms() -> i64 { 60_000 }
fn default_beacon_grace_epochs() -> u32 { 3 }
fn default_theta_lo() -> f32 { 0.4 }
fn default_theta_hi() -> f32 { 0.7 }
fn default_accrual_alpha() -> f32 { 0.05 }

// ---------------------------------------------------------------------------
// Defaults
// ---------------------------------------------------------------------------

fn default_agent_id() -> String {
    let id = uuid::Uuid::new_v4();
    format!("agent-{}", &id.to_string()[..8])
}

/// ADR-0059 §1 (#930): a member's defaults are `[agent] kind = "agent"` and
/// `[swarm] role = "worker"`. `queen` is never a default — the first outside
/// node (2026-09-10) got `queen`/`human` from these two lines. `role` is
/// cosmetic today (nothing branches on it): the default changes what a
/// config *says*, not what the node does.
fn default_agent_kind() -> String { "agent".to_string() }
fn default_llm_provider() -> String { "none".to_string() }
fn default_nats_url() -> String { "nats://swarm.ninja-portal.com:4222".to_string() }
fn default_role() -> String { "worker".to_string() }
fn default_hub_url() -> String { "https://radio.ninja-portal.com".to_string() }
fn default_kax_url() -> String { "https://kax.ninja-portal.com".to_string() }
fn default_radio_url() -> String { "https://radio.ninja-portal.com".to_string() }
fn default_observatory_url() -> String { "https://observatory.ninja-portal.com".to_string() }
fn default_wavefront_dim() -> u32 { 10000 }
fn default_true() -> bool { true }
fn default_channel() -> String { "stable".to_string() }

// ---------------------------------------------------------------------------
// Trait impls
// ---------------------------------------------------------------------------

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            id: default_agent_id(),
            display_name: String::new(),
            kind: default_agent_kind(),
            persona: String::new(),
        }
    }
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            provider: default_llm_provider(),
            model: String::new(),
            api_key: String::new(),
            base_url: String::new(),
        }
    }
}

impl Default for SwarmConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            nats_url: default_nats_url(),
            role: default_role(),
        }
    }
}

impl Default for GhostSignalsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            hub_url: default_hub_url(),
            token: String::new(),
            kax_url: default_kax_url(),
            kax_token: String::new(),
        }
    }
}

impl Default for ConstellationConfig {
    fn default() -> Self {
        Self {
            radio_url: default_radio_url(),
            observatory_url: default_observatory_url(),
        }
    }
}

impl Default for HrmConfig {
    fn default() -> Self {
        Self {
            path: String::new(),
            wavefront_dim: default_wavefront_dim(),
        }
    }
}

impl Default for UpdatesConfig {
    fn default() -> Self {
        Self {
            auto_check: true,
            channel: default_channel(),
            last_checked: String::new(),
        }
    }
}

impl Default for TriageConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            redundancy: default_triage_redundancy(),
            min_amplitude: default_triage_min_amplitude(),
            min_age_hours: default_triage_min_age_hours(),
            max_evict: default_triage_max_evict(),
            xi_trigger: default_triage_xi_trigger(),
        }
    }
}

fn default_triage_redundancy() -> f32 { 0.95 }
fn default_triage_min_amplitude() -> f32 { 0.75 }
fn default_triage_min_age_hours() -> i64 { 24 }
fn default_triage_max_evict() -> usize { 100 }
fn default_triage_xi_trigger() -> f32 { 0.0 }
fn default_belief_max_n() -> usize { 6000 }

impl Default for BeliefConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_n: default_belief_max_n(),
        }
    }
}


// ---------------------------------------------------------------------------
// Core API
// ---------------------------------------------------------------------------

impl KannakaConfig {
    /// Returns the Kannaka data directory, respecting `KANNAKA_DATA_DIR`.
    pub fn data_dir() -> PathBuf {
        if let Ok(dir) = std::env::var("KANNAKA_DATA_DIR") {
            return PathBuf::from(dir);
        }
        if let Some(home) = dirs::home_dir() {
            return home.join(".kannaka");
        }
        PathBuf::from(".kannaka")
    }

    /// Path to `config.toml` inside the data directory.
    pub fn config_path() -> PathBuf {
        Self::data_dir().join("config.toml")
    }

    /// Load config from `~/.kannaka/config.toml`.
    ///
    /// If the file does not exist, returns a default config (does NOT write it).
    /// After loading, environment variable overrides are applied.
    pub fn load() -> Self {
        let path = Self::config_path();
        let mut cfg = if path.exists() {
            match std::fs::read_to_string(&path) {
                Ok(text) => toml::from_str::<KannakaConfig>(&text).unwrap_or_else(|e| {
                    eprintln!("[config] Warning: failed to parse {}: {}", path.display(), e);
                    KannakaConfig::default()
                }),
                Err(e) => {
                    eprintln!("[config] Warning: failed to read {}: {}", path.display(), e);
                    KannakaConfig::default()
                }
            }
        } else {
            // No config.toml. Before minting a brand-new identity, honour the
            // legacy `~/.kannaka/agent_id` file. `persist_agent_id_compat()`
            // writes it and install detection reads it, but the runtime never
            // did — so a node with only that file generated a FRESH random id
            // on every process. Presence continuity broke, and because the
            // trusted-agents allowlist is keyed by id, such a node could never
            // match its own roster entry. (#595)
            let mut c = KannakaConfig::default();
            if let Some(id) = Self::legacy_agent_id() {
                c.agent.id = id;
            }
            c
        };
        // After the file fallback, so the documented precedence holds:
        // env var > config.toml > persisted file > generate new.
        cfg.apply_env_overrides();
        cfg
    }

    /// The id persisted by `persist_agent_id_compat()`, if usable.
    ///
    /// Returns `None` for a missing, unreadable, empty or whitespace-only
    /// file so a truncated write falls through to generation rather than
    /// yielding an empty agent id.
    pub fn legacy_agent_id() -> Option<String> {
        let path = Self::data_dir().join("agent_id");
        std::fs::read_to_string(path)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// Save config to `~/.kannaka/config.toml` with the plain header.
    ///
    /// Creates the data directory if it does not exist.
    /// On Unix, sets file permissions to 0600 (owner-only) for API key safety.
    ///
    /// #933: this does NOT re-derive whether the node is an anonymous swarm
    /// member. `save()` has around twenty callers — `kannaka config set`,
    /// the gate, identity, services handlers — and each would otherwise
    /// answer "are we anonymous?" from the credentials visible to THAT
    /// process at THAT moment. `swarm_credentials_present()` sees only
    /// NATS_USER/NATS_PASSWORD in the environment or `~/.kannaka-nats.env`,
    /// so on a node whose credentials arrive from elsewhere (the witness and
    /// hive-bridge units use `EnvironmentFile=/etc/kannaka-*.env`) a
    /// hand-run `config set` stamped a false "ANONYMOUS membership" block
    /// onto a fully credentialed config, which the next save from the
    /// service silently removed again. Membership is decided once, by the
    /// init paths, and only they may state it: see [`Self::save_with_header`].
    pub fn save(&self) -> Result<(), String> {
        self.save_with_header(false)
    }

    /// `save()` for the init paths (wizard, first-run installer, upgrade
    /// installer). They are the callers that have just run
    /// [`decide_swarm_membership`], so they — and only they — can truthfully
    /// say in the header whether this node joined without credentials
    /// (ADR-0059 §1).
    pub fn save_with_header(&self, anonymous_swarm: bool) -> Result<(), String> {
        let dir = Self::data_dir();
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("failed to create {}: {}", dir.display(), e))?;

        let path = Self::config_path();
        let text = toml::to_string_pretty(self)
            .map_err(|e| format!("failed to serialize config: {e}"))?;

        let full = format!("{}{text}", config_file_header(anonymous_swarm));

        // Owner-only (0600) from creation AND temp-and-rename in the same
        // directory (#930): the file is never truncated in place, so a crash
        // or a full disk mid-write leaves the previous config whole.
        crate::provenance::write_owner_only(&path, full.as_bytes())
            .map_err(|e| format!("failed to write {}: {}", path.display(), e))?;

        Ok(())
    }

    /// Returns true if the config file already exists on disk.
    pub fn exists() -> bool {
        Self::config_path().exists()
    }

    /// Apply environment variable overrides. Used by `load()` so the
    /// in-memory config sees the documented precedence (env > file >
    /// default). Code paths that need to PERSIST a setting must NOT
    /// start from this enriched view — see `load_unmodified()`.
    fn apply_env_overrides(&mut self) {
        if let Ok(v) = std::env::var("KANNAKA_AGENT_ID") { self.agent.id = v; }
        if let Ok(v) = std::env::var("KANNAKA_LLM_PROVIDER") { self.llm.provider = v; }
        if let Ok(v) = std::env::var("KANNAKA_LLM_MODEL") { self.llm.model = v; }
        if let Ok(v) = std::env::var("KANNAKA_LLM_API_KEY") { self.llm.api_key = v; }
        if let Ok(v) = std::env::var("KANNAKA_LLM_BASE_URL") { self.llm.base_url = v; }
        if let Ok(v) = std::env::var("KANNAKA_NATS_URL") { self.swarm.nats_url = v; }
        if let Ok(v) = std::env::var("OLLAMA_URL") { self.llm.base_url = v; }
        // Constellation + GhostSignals endpoint overrides (#98). The
        // config module advertises env-var precedence for these and
        // production deployments rely on it; previously only agent/LLM
        // /swarm vars actually took effect.
        if let Ok(v) = std::env::var("KANNAKA_RADIO_URL") { self.constellation.radio_url = v; }
        if let Ok(v) = std::env::var("KANNAKA_OBSERVATORY_URL") { self.constellation.observatory_url = v; }
        if let Ok(v) = std::env::var("KANNAKA_GHOSTSIGNALS_HUB_URL") { self.ghostsignals.hub_url = v; }
        if let Ok(v) = std::env::var("KANNAKA_GHOSTSIGNALS_TOKEN") { self.ghostsignals.token = v; }
        if let Ok(v) = std::env::var("KANNAKA_KAX_URL") { self.ghostsignals.kax_url = v; }
        if let Ok(v) = std::env::var("KAX_IDENTITY_TOKEN") { self.ghostsignals.kax_token = v; }
        // SECURITY (increment-0): swarm-trust overrides. KANNAKA_TRUSTED_AGENTS
        // is a comma-separated allowlist that REPLACES the default list (empty
        // entries dropped, whitespace trimmed; an all-empty value trusts only
        // this node's own id). KANNAKA_METRICS_TRUSTED_ONLY=0 (or false/no/off)
        // disables the metrics filter — the escape hatch.
        if let Ok(v) = std::env::var("KANNAKA_TRUSTED_AGENTS") {
            self.swarm_trust.trusted_agents = v
                .split(',')
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .collect();
        }
        if let Ok(v) = std::env::var("KANNAKA_METRICS_TRUSTED_ONLY") {
            let v = v.trim().to_ascii_lowercase();
            self.swarm_trust.metrics_trusted_only =
                !matches!(v.as_str(), "0" | "false" | "no" | "off");
        }
        // SECURITY (inc-1): trust threshold θ override. Inert until the
        // enrollment/reputation layer consumes it; wired here for parity
        // with the other swarm-trust env escape hatches.
        if let Ok(v) = std::env::var("KANNAKA_TRUST_THRESHOLD") {
            if let Ok(t) = v.trim().parse::<f32>() {
                self.swarm_trust.trust_threshold = t;
            }
        }
        // SECURITY (inc-1b): corroboration-gate overrides. KANNAKA_SEED_PUBKEYS
        // is a comma-separated list of base64 seed pubkeys that REPLACES the
        // pinned set (empty entries dropped). The gate stays dormant until at
        // least one seed exists AND KANNAKA_CORROBORATION_GATE is truthy.
        if let Ok(v) = std::env::var("KANNAKA_SEED_PUBKEYS") {
            self.swarm_trust.seed_pubkeys = v
                .split(',')
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .collect();
        }
        if let Ok(v) = std::env::var("KANNAKA_CORROBORATION_GATE") {
            let v = v.trim().to_ascii_lowercase();
            self.swarm_trust.corroboration_gate_enabled =
                matches!(v.as_str(), "1" | "true" | "yes" | "on");
        }
        if let Ok(v) = std::env::var("KANNAKA_EPOCH_LENGTH_MS") {
            if let Ok(n) = v.trim().parse::<i64>() {
                self.swarm_trust.epoch_length_ms = n;
            }
        }
        if let Ok(v) = std::env::var("KANNAKA_BEACON_GRACE_EPOCHS") {
            if let Ok(n) = v.trim().parse::<u32>() {
                self.swarm_trust.beacon_grace_epochs = n;
            }
        }
        if let Ok(v) = std::env::var("KANNAKA_THETA_LO") {
            if let Ok(t) = v.trim().parse::<f32>() {
                self.swarm_trust.theta_lo = t;
            }
        }
        if let Ok(v) = std::env::var("KANNAKA_THETA_HI") {
            if let Ok(t) = v.trim().parse::<f32>() {
                self.swarm_trust.theta_hi = t;
            }
        }
        if let Ok(v) = std::env::var("KANNAKA_ACCRUAL_ALPHA") {
            if let Ok(t) = v.trim().parse::<f32>() {
                self.swarm_trust.accrual_alpha = t;
            }
        }
    }

    /// Load the on-disk config WITHOUT applying environment overrides.
    /// `config set` uses this so that writing one key doesn't silently
    /// persist unrelated env-only values (`KANNAKA_AGENT_ID`,
    /// `KANNAKA_NATS_URL`, etc.) back into the file (#99). Everything
    /// else should keep using `load()`.
    pub fn load_unmodified() -> Self {
        let path = Self::config_path();
        if !path.exists() { return Self::default(); }
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => return Self::default(),
        };
        toml::from_str(&content).unwrap_or_default()
    }

    /// Backward-compatible helper: persist `agent_id` to `~/.kannaka/agent_id`
    /// so older code that reads that file still works.
    pub fn persist_agent_id_compat(&self) -> Result<(), String> {
        let path = Self::data_dir().join("agent_id");
        // #933 (P6): small file, small truncation window — but this is the
        // file `init_base_config` falls back to when config.toml is absent,
        // i.e. the last line of defence for the node's identity. A truncated
        // one mints a fresh random id (#595).
        crate::fs_util::atomic_write_bytes(&path, self.agent.id.as_bytes())
            .map_err(|e| format!("failed to write agent_id: {e}"))
    }

    /// The starting point for `kannaka init` (#930, ADR-0059 §1): the config
    /// already on disk when there is one, so a re-run MERGES — it keeps the
    /// agent id, persona, retention, swarm_trust and every table the wizard
    /// does not ask about — and defaults (honouring the legacy `agent_id`
    /// file) only when there is none. A config that exists but does not
    /// parse is an error, not a default: silently minting a new identity
    /// over a file the operator can still repair is exactly the failure
    /// this replaces. Env overrides are NOT applied (see `load_unmodified`),
    /// so a `KANNAKA_AGENT_ID` in the environment is never persisted by init.
    pub fn init_base_config() -> Result<Self, String> {
        let path = Self::config_path();
        if !path.exists() {
            let mut c = Self::default();
            if let Some(id) = Self::legacy_agent_id() {
                c.agent.id = id;
            }
            return Ok(c);
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read existing config {}: {e}", path.display()))?;
        toml::from_str(&text).map_err(|e| {
            format!(
                "existing config {} does not parse ({e}); init will not overwrite it — \
                 fix it or move it aside, then re-run",
                path.display()
            )
        })
    }

    /// Where the swarm credentials file lives (`~/.kannaka-nats.env`, sourced
    /// by every kannaka unit). `None` when there is no home directory.
    pub fn nats_creds_path() -> Option<PathBuf> {
        dirs::home_dir().map(|h| h.join(".kannaka-nats.env"))
    }

    /// Whether this process can join the swarm with credentials: both
    /// `NATS_USER` and `NATS_PASSWORD` exported, or the credentials file
    /// present. ADR-0059 §1: `swarm.enabled = true` is set only after this
    /// holds, or after the operator explicitly chose anonymous membership.
    pub fn swarm_credentials_present() -> bool {
        let env_ok = std::env::var("NATS_USER").map(|v| !v.trim().is_empty()).unwrap_or(false)
            && std::env::var("NATS_PASSWORD").map(|v| !v.is_empty()).unwrap_or(false);
        env_ok || Self::nats_creds_path().map(|p| p.is_file()).unwrap_or(false)
    }
}

/// The comment block at the top of `config.toml`. When the node is an
/// anonymous swarm member the header says so (ADR-0059 §1), because the
/// `[swarm]` table alone cannot: `enabled = true` reads the same with or
/// without credentials.
pub fn config_file_header(anonymous_swarm: bool) -> String {
    let mut h = String::from(
        "# Kannaka Constellation Configuration\n\
         # Generated by: kannaka init\n",
    );
    if anonymous_swarm {
        h.push_str(
            "# swarm: ANONYMOUS membership — no credentials (NATS_USER/NATS_PASSWORD or\n\
             # ~/.kannaka-nats.env) were present when this file was written; peers list\n\
             # this node as (unverified). Re-run `kannaka init` once they are in place.\n",
        );
    }
    h.push('\n');
    h
}

/// How the swarm step of `init` ends (ADR-0059 §1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwarmMembership {
    /// `swarm.enabled = false`.
    Disabled,
    /// Credentials are present; `swarm.enabled = true`.
    Credentialed,
    /// No credentials, but the operator explicitly chose to join anyway;
    /// `swarm.enabled = true` and the config header says so.
    Anonymous,
}

/// `join`: the operator wants the swarm at all. `creds_present`: see
/// [`KannakaConfig::swarm_credentials_present`]. `choose_anonymous` is consulted
/// ONLY when the operator wants in and there are no credentials — the prompt
/// in interactive mode; the `--anonymous` flag, or an existing config that
/// already joined anonymously, otherwise. `swarm.enabled = true` is therefore
/// never written on the mere hope that credentials will turn up.
pub fn decide_swarm_membership(
    join: bool,
    creds_present: bool,
    choose_anonymous: impl FnOnce() -> bool,
) -> SwarmMembership {
    if !join {
        SwarmMembership::Disabled
    } else if creds_present {
        SwarmMembership::Credentialed
    } else if choose_anonymous() {
        SwarmMembership::Anonymous
    } else {
        SwarmMembership::Disabled
    }
}

/// Human-readable location of the credentials, for prompts and notes.
fn swarm_creds_display() -> String {
    match KannakaConfig::nats_creds_path() {
        Some(p) => p.display().to_string(),
        None => "~/.kannaka-nats.env".to_string(),
    }
}

/// Write the decision into the config and return the one line to print.
pub fn apply_swarm_membership(config: &mut KannakaConfig, m: SwarmMembership) -> String {
    config.swarm.enabled = m != SwarmMembership::Disabled;
    match m {
        SwarmMembership::Credentialed => {
            "Swarm credentials found — joining as a credentialed member.".to_string()
        }
        SwarmMembership::Anonymous => format!(
            "Joining the swarm ANONYMOUSLY (no credentials at {}); peers list this node as \
             (unverified). Re-run `kannaka init` once the credentials file is in place.",
            swarm_creds_display()
        ),
        SwarmMembership::Disabled => format!(
            "Swarm not enabled (no credentials at {}). Place them there and re-run \
             `kannaka init`, or pass --anonymous to join without them.",
            swarm_creds_display()
        ),
    }
}

/// The hub to register with: the explicit GhostSignals hub URL, falling
/// back to the radio URL only when `hub_url` is empty (legacy single-host
/// configs, #97).
fn ghostsignals_hub(config: &KannakaConfig) -> String {
    if config.ghostsignals.hub_url.is_empty() {
        config.constellation.radio_url.clone()
    } else {
        config.ghostsignals.hub_url.clone()
    }
}

/// Apply the hub's answer to the config (ADR-0059 §1, #930): `enabled`
/// becomes true ONLY with a token in hand. On failure the flag is left
/// false — `enabled = true` with an empty token was the silently-broken
/// identity of #111, and the wizard used to set it BEFORE calling the hub.
/// Returns the one factual line the caller prints.
pub fn apply_ghostsignals_registration(
    config: &mut KannakaConfig,
    result: Result<String, String>,
) -> String {
    match result {
        Ok(token) if !token.trim().is_empty() => {
            config.ghostsignals.token = token;
            config.ghostsignals.enabled = true;
            format!("Registered '{}' with GhostSignals.", config.agent.id)
        }
        Ok(_) => {
            config.ghostsignals.enabled = false;
            "GhostSignals registration returned no token; left disabled. Re-run `kannaka init` \
             to try again."
                .to_string()
        }
        Err(e) => {
            config.ghostsignals.enabled = false;
            format!("GhostSignals registration failed: {e}. Left disabled; re-run `kannaka init` to try again.")
        }
    }
}

/// ADR-0037: bridge the persisted `[belief]` config to the env vars the engine
/// reads (`belief_phase_enabled` / `belief_max_n` in `medium::chiral`). **Env
/// wins** — only set a var when it's unset, so `KANNAKA_BELIEF_PHASE=on` from a
/// dream-cron or a systemd `Environment=` still overrides the file. Call once at
/// startup, before the HRM loads (and before any worker thread spawns, so the
/// `set_var` is single-threaded-safe on edition 2021).
pub fn apply_belief_env_from_config(cfg: &KannakaConfig) {
    if std::env::var_os("KANNAKA_BELIEF_PHASE").is_none() && cfg.belief.enabled {
        std::env::set_var("KANNAKA_BELIEF_PHASE", "on");
    }
    if std::env::var_os("KANNAKA_BELIEF_MAX_N").is_none() {
        std::env::set_var("KANNAKA_BELIEF_MAX_N", cfg.belief.max_n.to_string());
    }
}

/// num_clusters fix: bridge the persisted `[cluster]` config to the env var the
/// cluster detector reads (`cluster_decone_enabled` in `kuramoto`). **Env wins** —
/// only set the var when it's unset, so a one-off `KANNAKA_CLUSTER_DECONE=…` still
/// overrides the file. Call once at startup (single-threaded, before workers spawn).
pub fn apply_cluster_env_from_config(cfg: &KannakaConfig) {
    if std::env::var_os("KANNAKA_CLUSTER_DECONE").is_none() && cfg.cluster.decone {
        std::env::set_var("KANNAKA_CLUSTER_DECONE", "on");
    }
}

/// ADR-0037 Track-D: bridge persisted `[coupling].enabled` → KANNAKA_EXEMPLAR_
/// COUPLING (read by the swarm-join heartbeat). **Env wins** — only set when unset.
/// Call once at startup (single-threaded, before workers spawn).
pub fn apply_coupling_env_from_config(cfg: &KannakaConfig) {
    if std::env::var_os("KANNAKA_EXEMPLAR_COUPLING").is_none() && cfg.coupling.enabled {
        std::env::set_var("KANNAKA_EXEMPLAR_COUPLING", "on");
    }
}

/// Quantum-Wave T1.3: bridge persisted `[entropy].source` → KANNAKA_ENTROPY_
/// SOURCE (read by `entropy::EntropySelection::from_env`). **Env wins** — only set
/// when unset. Call once at startup (single-threaded, before workers spawn).
pub fn apply_entropy_env_from_config(cfg: &KannakaConfig) {
    if std::env::var_os("KANNAKA_ENTROPY_SOURCE").is_none() {
        std::env::set_var("KANNAKA_ENTROPY_SOURCE", &cfg.entropy.source);
    }
    // T1.4: bridge [entropy].dream_perturbation → KANNAKA_DREAM_ENTROPY (env wins).
    if std::env::var_os("KANNAKA_DREAM_ENTROPY").is_none() && cfg.entropy.dream_perturbation {
        std::env::set_var("KANNAKA_DREAM_ENTROPY", "1");
    }
}

// ---------------------------------------------------------------------------
// Update checker (non-blocking)
// ---------------------------------------------------------------------------

const GITHUB_RELEASES_URL: &str =
    "https://api.github.com/repos/kannaka-labs/kannaka-memory/releases/latest";

/// Spawns a background thread that checks for updates if due.
/// Never blocks the main CLI.
pub fn check_for_updates_background(config: &KannakaConfig) {
    if !config.updates.auto_check {
        return;
    }
    if last_checked_within_24h(&config.updates.last_checked) {
        return;
    }

    // Snapshot what we need for the thread (avoid borrowing config across threads)
    let config_path = KannakaConfig::config_path();
    let current_version = env!("CARGO_PKG_VERSION").to_string();

    std::thread::spawn(move || {
        let agent = ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(5))
            .build();
        let resp = match agent.get(GITHUB_RELEASES_URL)
            .set("User-Agent", "kannaka-update-check")
            .set("Accept", "application/vnd.github.v3+json")
            .call()
        {
            Ok(r) => r,
            Err(_) => return, // silent fail
        };
        let body: serde_json::Value = match resp.into_json() {
            Ok(v) => v,
            Err(_) => return,
        };
        if let Some(tag) = body["tag_name"].as_str() {
            let remote = tag.trim_start_matches('v');
            if remote != current_version && version_is_newer(remote, &current_version) {
                eprintln!(
                    "\n  Update available: v{remote} (current: v{current_version}). Run: kannaka update\n"
                );
            }
        }

        // Update last_checked timestamp in config file (best-effort)
        if let Ok(text) = std::fs::read_to_string(&config_path) {
            if let Ok(mut cfg) = toml::from_str::<KannakaConfig>(&text) {
                cfg.updates.last_checked = chrono::Utc::now().to_rfc3339();
                let _ = cfg.save();
            }
        }
    });
}

/// Simple semver comparison: returns true if `remote` > `current`.
fn version_is_newer(remote: &str, current: &str) -> bool {
    // Per-position numeric prefix, NOT filter_map: a component that fails to
    // parse must degrade in place ("10-rc1" → 10), never be dropped — dropping
    // shifts later components left, so "0.10.4-1" used to parse as (0, 10, 1)-
    // style nonsense and a hotfix-suffixed tag compared OLDER than its base.
    //
    // Known limitation (deliberate): suffixes are IGNORED, so "0.10.4-1"
    // compares EQUAL to "0.10.4" and would not propagate as an update.
    // Treating a suffix as "newer" would be wrong for the opposite semver
    // convention ("0.6.10-rc.1" is a PRE-release, older than "0.6.10").
    // Release policy: hotfixes bump the patch (v0.10.5), never suffix a tag.
    let parse = |s: &str| -> (u32, u32, u32) {
        let mut it = s.split('.').map(|p| {
            p.chars()
                .take_while(|c| c.is_ascii_digit())
                .collect::<String>()
                .parse()
                .unwrap_or(0)
        });
        (
            it.next().unwrap_or(0),
            it.next().unwrap_or(0),
            it.next().unwrap_or(0),
        )
    };
    parse(remote) > parse(current)
}

fn last_checked_within_24h(last: &str) -> bool {
    if last.is_empty() {
        return false;
    }
    match chrono::DateTime::parse_from_rfc3339(last) {
        Ok(dt) => {
            let now = chrono::Utc::now();
            let diff = now.signed_duration_since(dt.with_timezone(&chrono::Utc));
            diff.num_hours() < 24
        }
        Err(_) => false,
    }
}

// ---------------------------------------------------------------------------
// Self-update command
// ---------------------------------------------------------------------------

/// Windows-only: swap `target` for the already-downloaded `new_file` by
/// moving the running `target` aside to a **unique** backup name, then
/// renaming `new_file` into place.
///
/// Using a unique backup (`<stem>.exe.bak-<pid>`) instead of a fixed
/// `<stem>.exe.old` is the crux: `std::fs::rename` on Windows uses
/// `MOVEFILE_REPLACE_EXISTING`, so renaming onto a fixed `.old` that is
/// still **locked** — e.g. a daemon/worker that was left running from an
/// earlier renamed binary — fails forever with "Access is denied"
/// (os error 5). A fresh name is never locked, so the move always
/// succeeds. We also retry briefly to ride out transient locks (antivirus
/// scanning the freshly-written binary, or a worker respawn racing the
/// rename), roll back on a failed install, and sweep unlocked stale
/// backups on success. Returns the backup path on success.
#[cfg(windows)]
fn windows_swap_binary(
    target: &std::path::Path,
    new_file: &std::path::Path,
) -> Result<std::path::PathBuf, String> {
    let backup = target.with_extension(format!("exe.bak-{}", std::process::id()));
    let _ = std::fs::remove_file(&backup);
    let mut last_err = String::from("unknown error");
    let mut moved = false;
    for attempt in 0..5u64 {
        match std::fs::rename(target, &backup) {
            Ok(_) => {
                moved = true;
                break;
            }
            Err(e) => {
                last_err = e.to_string();
                std::thread::sleep(std::time::Duration::from_millis(150 * (attempt + 1)));
            }
        }
    }
    if !moved {
        return Err(last_err);
    }
    if let Err(e) = std::fs::rename(new_file, target) {
        // Roll back so the user is never left without a binary.
        let _ = std::fs::rename(&backup, target);
        return Err(e.to_string());
    }
    cleanup_stale_backups(target, Some(&backup));
    Ok(backup)
}

/// Best-effort removal of stale update siblings next to `target`
/// (`<stem>.exe.bak-*`, the legacy `<stem>.exe.old`, and orphan
/// `<stem>.new`). Locked ones (a process still running from them) are
/// silently skipped — they free up on their own once that process exits.
/// `keep` is the backup created by the current swap — it must survive the
/// sweep so the "previous binary saved as …" rollback artifact actually
/// exists (previously it survived only when Windows happened to have the
/// old image locked; for a non-running TUI it was deleted immediately).
#[cfg(windows)]
fn cleanup_stale_backups(target: &std::path::Path, keep: Option<&std::path::Path>) {
    let (Some(dir), Some(stem)) = (
        target.parent(),
        target.file_stem().and_then(|s| s.to_str()),
    ) else {
        return;
    };
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path == *target || keep.is_some_and(|k| path == *k) {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with(&format!("{stem}.exe.bak-"))
                || name == format!("{stem}.exe.old")
                || name == format!("{stem}.new")
            {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
}

/// Download and replace the running binary with the latest release.
pub fn self_update() -> Result<(), String> {
    let current_version = env!("CARGO_PKG_VERSION");
    eprintln!(
        "Checking for updates (current: v{current_version} · consciousness-core v{CONSCIOUSNESS_CORE_VERSION})...",
    );

    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(15))
        .build();

    // Surface upstream consciousness-core version so operators know
    // whether the kannaka release stream is keeping up with the
    // constellation physics releases. Best-effort: a network blip
    // here doesn't block the main update.
    report_consciousness_core_drift(&agent);
    let resp = agent.get(GITHUB_RELEASES_URL)
        .set("User-Agent", "kannaka-update")
        .set("Accept", "application/vnd.github.v3+json")
        .call()
        .map_err(|e| format!("failed to check releases: {e}"))?;

    let body: serde_json::Value = resp.into_json()
        .map_err(|e| format!("failed to parse release info: {e}"))?;

    let tag = body["tag_name"].as_str()
        .ok_or("no tag_name in release")?;
    let remote_version = tag.trim_start_matches('v');

    if !version_is_newer(remote_version, current_version) {
        eprintln!("Already up to date (v{current_version}).");
        // The TUI ships on its OWN release cadence, so a stale sibling
        // must still be refreshed even when the kannaka binary is
        // current — otherwise `kannaka update` never touches the TUI once
        // kannaka itself has caught up. update_sibling_tui hits the
        // kannaka-tui repo and no-ops when the TUI is already at latest.
        if let Ok(current_exe) = std::env::current_exe() {
            update_sibling_tui(&agent, &body, tag, &current_exe, remote_version);
        }
        return Ok(());
    }

    eprintln!("New version available: v{remote_version}");

    // Determine platform
    let (os, arch, ext) = platform_triple();
    let asset_name = format!("kannaka-{os}-{arch}{ext}");

    // Find download URL in release assets
    let download_url = body["assets"].as_array()
        .and_then(|assets| {
            assets.iter().find(|a| {
                a["name"].as_str().is_some_and(|n| n == asset_name)
            })
        })
        .and_then(|a| a["browser_download_url"].as_str())
        .ok_or_else(|| format!("no binary found for {asset_name} in release {tag}"))?
        .to_string();

    eprintln!("Downloading {asset_name}...");

    let resp = agent.get(&download_url)
        .set("User-Agent", "kannaka-update")
        .call()
        .map_err(|e| format!("download failed: {e}"))?;

    let mut bytes = Vec::new();
    resp.into_reader().read_to_end(&mut bytes)
        .map_err(|e| format!("failed to read download: {e}"))?;

    // ADR-0029 Phase 4a — verify SHA-256 sidecar before atomic rename.
    // The release workflow writes a `<asset>.sha256` next to every
    // binary in v0.6.2+. Older releases don't have the sidecar; we
    // warn but proceed in that case so an operator on v0.6.1 can still
    // pull v0.6.2+ without a flag-day.
    match fetch_and_verify_sha256(&agent, &body, &asset_name, &bytes) {
        Ok(()) => eprintln!("SHA-256 verified."),
        Err(VerifyError::SidecarMissing) => {
            eprintln!("Note: no SHA-256 sidecar in this release (pre-v0.6.2). Skipping verification.");
        }
        Err(VerifyError::Mismatch { expected, actual }) => {
            return Err(format!(
                "SHA-256 mismatch — refusing to replace running binary.\n  \
                 expected: {expected}\n  \
                 actual:   {actual}"
            ));
        }
        Err(VerifyError::Other(e)) => {
            // The release HAS a sidecar (we found its URL in the same release
            // JSON as the binary), so an unfetchable sidecar means we cannot
            // verify a binary we know is verifiable — fail closed rather than
            // silently installing unverified bytes.
            return Err(format!(
                "SHA-256 sidecar could not be fetched after retry ({e}) — \
                 refusing to install unverified binary. Retry when the network \
                 is stable."
            ));
        }
    }

    let current_exe = std::env::current_exe()
        .map_err(|e| format!("cannot determine current exe path: {e}"))?;

    // Write to a temp file next to the current binary
    let tmp_path = current_exe.with_extension("new");
    std::fs::write(&tmp_path, &bytes)
        .map_err(|e| format!("failed to write temp binary: {e}"))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        if let Err(e) = std::fs::set_permissions(&tmp_path, perms) {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(format!("chmod failed: {e}"));
        }
        // Atomic rename
        if let Err(e) = std::fs::rename(&tmp_path, &current_exe) {
            // Don't leave a stale `<name>.new` next to the binary — nothing
            // on Unix ever sweeps it (cleanup_stale_backups is Windows-only).
            let _ = std::fs::remove_file(&tmp_path);
            return Err(format!("failed to replace binary: {e}"));
        }
        eprintln!("Updated to v{}!", remote_version);
    }

    #[cfg(windows)]
    {
        // Can't overwrite a running exe on Windows — move it aside to a
        // unique backup, then install the new one (see windows_swap_binary
        // for why a unique name, not a fixed `.old`, is essential).
        match windows_swap_binary(&current_exe, &tmp_path) {
            Ok(backup) => {
                eprintln!(
                    "Updated to v{}! (previous binary saved as {})",
                    remote_version,
                    backup.display()
                );
            }
            Err(e) => {
                let _ = std::fs::remove_file(&tmp_path);
                return Err(format!(
                    "failed to install update: {e}. Close any running kannaka processes \
                     (kannaka-tui, swarm/daemon workers) and retry, or download manually \
                     from https://github.com/kannaka-labs/kannaka-memory/releases/latest"
                ));
            }
        }
    }

    // Sibling TUI update — keep `kannaka-tui` aligned with `kannaka` so
    // users don't end up with a v0.3.7 TUI shelling out to a v0.3.9
    // CLI (or worse, the reverse). Best-effort: if the sibling doesn't
    // exist or download fails, we don't abort the main update.
    update_sibling_tui(&agent, &body, tag, &current_exe, remote_version);

    Ok(())
}

/// Probe `kannaka-labs/consciousness-core` releases and print a hint when
/// the upstream tag is newer than the version baked into this binary.
/// Pure UX — `kannaka update` only ships pre-built `kannaka` binaries,
/// and consciousness-core rides into them at build time via the path
/// dep + release CI sibling-checkout. If consciousness-core releases
/// outpace kannaka releases the user sees the drift here and knows a
/// fresh kannaka release is needed (see the release-cascade workflow
/// at .github/workflows/cc-release-cascade.yml).
fn report_consciousness_core_drift(agent: &ureq::Agent) {
    const CC_RELEASES_URL: &str =
        "https://api.github.com/repos/kannaka-labs/consciousness-core/releases/latest";
    let resp = match agent
        .get(CC_RELEASES_URL)
        .set("User-Agent", "kannaka-update-check")
        .set("Accept", "application/vnd.github.v3+json")
        .call()
    {
        Ok(r) => r,
        Err(_) => return, // silent; non-essential
    };
    let body: serde_json::Value = match resp.into_json() {
        Ok(b) => b,
        Err(_) => return,
    };
    let upstream_tag = match body["tag_name"].as_str() {
        Some(t) => t,
        None => return,
    };
    let upstream = upstream_tag.trim_start_matches('v');
    if CONSCIOUSNESS_CORE_VERSION == "unknown" {
        // Build couldn't read Cargo.lock; only surface upstream.
        eprintln!("  consciousness-core: bundled=unknown, upstream=v{upstream}");
        return;
    }
    if version_is_newer(upstream, CONSCIOUSNESS_CORE_VERSION) {
        eprintln!(
            "  consciousness-core upstream v{upstream} is newer than the v{CONSCIOUSNESS_CORE_VERSION} bundled into this kannaka.",
        );
        eprintln!(
            "  Wait for the next kannaka release (it'll carry v{upstream}), or rebuild from source.",
        );
    } else {
        eprintln!(
            "  consciousness-core: bundled v{CONSCIOUSNESS_CORE_VERSION} (upstream v{upstream}, up to date).",
        );
    }
}

/// Best-effort update of the `kannaka-tui` binary alongside `kannaka`.
/// Looks for a `kannaka-tui[.exe]` in the same directory as the current
/// binary; if found, downloads the matching tui artifact and atomically
/// replaces it (Windows: rename old to .exe.old, new into place; Unix:
/// chmod + rename). Silent no-op if the TUI isn't installed; warning on
/// download failure so the user knows to retry.
///
/// As of kannaka-memory v0.5.13 the TUI lives in its own repo
/// (kannaka-labs/kannaka-tui) with its own release cadence — the sibling
/// asset lookup hits THAT repo's latest release, not the kannaka-memory
/// release the caller is currently updating to. The two version streams
/// don't have to stay aligned; this just keeps an installed TUI binary
/// up-to-date alongside `kannaka update`.
fn update_sibling_tui(
    agent: &ureq::Agent,
    _release_body: &serde_json::Value,
    _tag: &str,
    current_exe: &std::path::Path,
    _remote_version: &str,
) {
    let dir = match current_exe.parent() {
        Some(d) => d,
        None => return,
    };
    let (os, arch, ext) = platform_triple();
    let tui_name = if cfg!(windows) { "kannaka-tui.exe" } else { "kannaka-tui" };
    let tui_path = dir.join(tui_name);
    let asset_name_hint = format!("kannaka-tui-{os}-{arch}{ext}");
    if !tui_path.exists() {
        // Sibling not installed — print a discoverable install hint so
        // users know the TUI exists and how to get it. Pre-v0.5.15 this
        // was a silent no-op which left operators on fresh machines
        // wondering why `kannaka-tui` didn't appear after `kannaka update`.
        eprintln!();
        eprintln!("Tip: kannaka-tui isn't installed alongside kannaka.");
        eprintln!("     Install with one of:");
        eprintln!("       cargo install --git https://github.com/kannaka-labs/kannaka-tui");
        eprintln!("       curl -L -o {tui_name} \\");
        eprintln!("         https://github.com/kannaka-labs/kannaka-tui/releases/latest/download/{asset_name_hint}");
        return;
    }

    // Hit the kannaka-tui repo's latest release directly.
    const TUI_RELEASES_URL: &str =
        "https://api.github.com/repos/kannaka-labs/kannaka-tui/releases/latest";
    let tui_release: serde_json::Value = match agent
        .get(TUI_RELEASES_URL)
        .set("User-Agent", "kannaka-update")
        .set("Accept", "application/vnd.github.v3+json")
        .call()
        .and_then(|r| r.into_json().map_err(Into::into))
    {
        Ok(b) => b,
        Err(e) => {
            eprintln!("Note: could not fetch kannaka-tui releases: {e}");
            return;
        }
    };
    let tui_tag = tui_release["tag_name"].as_str().unwrap_or("unknown");
    let tui_version = tui_tag.trim_start_matches('v');

    // Skip the download when the sibling is already at the latest TUI
    // release. This function now runs on every `kannaka update` (even when
    // the kannaka binary is current), so without this it would re-download
    // and re-swap the TUI binary each time — wasteful, and noisy on Windows
    // when the TUI is open. A Rust release binary carries no readable
    // version resource, so we track the installed version in a sidecar
    // written alongside the binary at install time.
    let tui_version_sidecar = dir.join(".kannaka-tui.version");
    if tui_version != "unknown" {
        if let Ok(installed) = std::fs::read_to_string(&tui_version_sidecar) {
            if installed.trim() == tui_version {
                eprintln!("kannaka-tui already at v{tui_version}.");
                return;
            }
        }
    }

    let asset_name = asset_name_hint;
    let download_url = match tui_release["assets"].as_array()
        .and_then(|assets| {
            assets.iter().find(|a| a["name"].as_str().is_some_and(|n| n == asset_name))
        })
        .and_then(|a| a["browser_download_url"].as_str())
    {
        Some(u) => u.to_string(),
        None => {
            eprintln!("Note: tui artifact `{asset_name}` not found in kannaka-tui release {tui_tag} — skipping TUI update.");
            return;
        }
    };

    eprintln!("Downloading {asset_name} (sibling)...");
    let resp = match agent.get(&download_url)
        .set("User-Agent", "kannaka-update")
        .call()
    {
        Ok(r) => r,
        Err(e) => { eprintln!("Note: tui download failed: {e}"); return; }
    };
    let mut bytes = Vec::new();
    if let Err(e) = resp.into_reader().read_to_end(&mut bytes) {
        eprintln!("Note: tui read failed: {e}");
        return;
    }

    // ADR-0029 Phase 4a — this is the most-traveled path that puts TUI bytes
    // on disk (it runs on every `kannaka update`), so it verifies the sidecar
    // just like self_update and bootstrap_install_tui. Best-effort refresh:
    // an unverifiable download skips the TUI update rather than aborting.
    match fetch_and_verify_sha256(agent, &tui_release, &asset_name, &bytes) {
        Ok(()) => eprintln!("SHA-256 verified."),
        Err(VerifyError::SidecarMissing) => {
            eprintln!("Note: no SHA-256 sidecar in kannaka-tui {tui_tag}. Skipping verification.");
        }
        Err(VerifyError::Mismatch { expected, actual }) => {
            eprintln!(
                "Note: kannaka-tui SHA-256 mismatch — skipping TUI update.\n  \
                 expected: {expected}\n  actual:   {actual}"
            );
            return;
        }
        Err(VerifyError::Other(e)) => {
            eprintln!("Note: kannaka-tui SHA-256 sidecar unfetchable after retry ({e}) — skipping TUI update.");
            return;
        }
    }

    let tmp_path = tui_path.with_extension("new");
    if let Err(e) = std::fs::write(&tmp_path, &bytes) {
        eprintln!("Note: tui write failed: {e}");
        return;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o755));
        if let Err(e) = std::fs::rename(&tmp_path, &tui_path) {
            // Don't strand `<name>.new` — nothing on Unix sweeps it.
            let _ = std::fs::remove_file(&tmp_path);
            eprintln!("Note: tui install failed: {e}");
            return;
        }
    }
    #[cfg(windows)]
    {
        // Same robust swap as the main binary: unique backup name + retry,
        // so a stale locked backup can't block the install. If the TUI is
        // genuinely running it stays at the old version — the user sees a
        // clear note and can retry after closing it.
        if let Err(e) = windows_swap_binary(&tui_path, &tmp_path) {
            eprintln!("Note: could not update tui (is it running?): {e}");
            let _ = std::fs::remove_file(&tmp_path);
            return;
        }
    }
    // Record the installed version so the next `kannaka update` can skip
    // the download when the TUI is already current (see the sidecar read
    // above). Best-effort: a failed write only costs a redundant refresh.
    let _ = std::fs::write(&tui_version_sidecar, tui_version);
    eprintln!("kannaka-tui also updated to v{tui_version}.");
}

/// ADR-0029 Phase 4a — error variants for the SHA-256 verification path.
/// Distinguishes "no sidecar" (skip with note) from "mismatch" (refuse
/// to install, abort with error) so callers can pick the right UX.
pub enum VerifyError {
    /// The release doesn't ship a .sha256 sidecar. Pre-v0.6.2 releases
    /// don't have these — caller warns but proceeds.
    SidecarMissing,
    /// Digest mismatch — refuse to install.
    Mismatch { expected: String, actual: String },
    /// Network / parse error fetching the sidecar.
    Other(String),
}

/// Fetch the `<asset>.sha256` sidecar from `release_body`'s asset list
/// and verify the local `bytes` digest against it. Sidecar format is
/// the standard `sha256sum` output: `<hex>  <filename>`.
pub fn fetch_and_verify_sha256(
    agent: &ureq::Agent,
    release_body: &serde_json::Value,
    asset_name: &str,
    bytes: &[u8],
) -> Result<(), VerifyError> {
    use sha2::{Digest, Sha256};

    let sidecar_name = format!("{asset_name}.sha256");
    let sidecar_url = release_body["assets"].as_array()
        .and_then(|assets| {
            assets.iter().find(|a| a["name"].as_str().is_some_and(|n| n == sidecar_name))
        })
        .and_then(|a| a["browser_download_url"].as_str())
        .ok_or(VerifyError::SidecarMissing)?
        .to_string();

    // One retry on the sidecar fetch: the release provably ships a sidecar
    // (the URL came from the same release JSON as the binary), so a transient
    // network blip shouldn't downgrade the install to unverified — and callers
    // treat `Other` as fatal for exactly that reason.
    let sidecar_body = {
        let fetch = || -> Result<String, String> {
            agent
                .get(&sidecar_url)
                .set("User-Agent", "kannaka-update")
                .call()
                .map_err(|e| format!("sidecar fetch: {e}"))?
                .into_string()
                .map_err(|e| format!("sidecar read: {e}"))
        };
        match fetch() {
            Ok(b) => b,
            Err(_) => {
                std::thread::sleep(std::time::Duration::from_millis(500));
                fetch().map_err(VerifyError::Other)?
            }
        }
    };

    // First whitespace-delimited token is the digest. Tolerate either
    // `sha256sum`'s `<hex>  <filename>\n` or just the hex digest alone.
    let expected = sidecar_body.split_whitespace().next()
        .ok_or_else(|| VerifyError::Other("sidecar empty".to_string()))?
        .to_lowercase();

    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let actual = format!("{:x}", hasher.finalize());

    if expected != actual {
        return Err(VerifyError::Mismatch { expected, actual });
    }
    Ok(())
}

/// ADR-0029 Phase 4a — `kannaka update --check`. Compare the running
/// version against the latest release. Returns `Some(remote)` if a
/// newer version exists, `None` if up-to-date. Does NOT download.
pub fn check_update_available() -> Result<Option<String>, String> {
    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(15))
        .build();
    let resp = agent.get(GITHUB_RELEASES_URL)
        .set("User-Agent", "kannaka-update-check")
        .set("Accept", "application/vnd.github.v3+json")
        .call()
        .map_err(|e| format!("releases fetch: {e}"))?;
    let body: serde_json::Value = resp.into_json()
        .map_err(|e| format!("releases parse: {e}"))?;
    let tag = body["tag_name"].as_str()
        .ok_or("no tag_name in release")?;
    let remote = tag.trim_start_matches('v');
    if version_is_newer(remote, VERSION) {
        Ok(Some(remote.to_string()))
    } else {
        Ok(None)
    }
}

/// ADR-0029 Phase 4a — `kannaka update --bootstrap-tui`. Install the
/// kannaka-tui sibling alongside the current kannaka binary even when
/// no existing tui sits there. Downloads the latest kannaka-tui release
/// for this platform and verifies its SHA-256.
pub fn bootstrap_install_tui() -> Result<std::path::PathBuf, String> {
    use std::io::Read;

    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(30))
        .build();

    let current_exe = std::env::current_exe()
        .map_err(|e| format!("current_exe: {e}"))?;
    let dir = current_exe.parent()
        .ok_or("current_exe has no parent directory")?;
    let tui_name = if cfg!(windows) { "kannaka-tui.exe" } else { "kannaka-tui" };
    let tui_path = dir.join(tui_name);
    if tui_path.exists() {
        return Err(format!(
            "kannaka-tui already exists at {} — use `kannaka update` to refresh it",
            tui_path.display()
        ));
    }

    // Fetch kannaka-tui's latest release.
    let resp = agent.get("https://api.github.com/repos/kannaka-labs/kannaka-tui/releases/latest")
        .set("User-Agent", "kannaka-bootstrap-tui")
        .set("Accept", "application/vnd.github.v3+json")
        .call()
        .map_err(|e| format!("kannaka-tui releases fetch: {e}"))?;
    let release: serde_json::Value = resp.into_json()
        .map_err(|e| format!("kannaka-tui release parse: {e}"))?;
    let tui_tag = release["tag_name"].as_str().unwrap_or("unknown");

    let (os, arch, ext) = platform_triple();
    let asset_name = format!("kannaka-tui-{os}-{arch}{ext}");
    let download_url = release["assets"].as_array()
        .and_then(|assets| {
            assets.iter().find(|a| a["name"].as_str().is_some_and(|n| n == asset_name))
        })
        .and_then(|a| a["browser_download_url"].as_str())
        .ok_or_else(|| format!("no kannaka-tui asset '{asset_name}' in release {tui_tag}"))?
        .to_string();

    eprintln!("Downloading {asset_name} from kannaka-tui {tui_tag}...");
    let resp = agent.get(&download_url)
        .set("User-Agent", "kannaka-bootstrap-tui")
        .call()
        .map_err(|e| format!("download: {e}"))?;
    let mut bytes = Vec::new();
    resp.into_reader().read_to_end(&mut bytes)
        .map_err(|e| format!("read: {e}"))?;

    // SHA-256 verification — same posture as self_update: warn on
    // sidecar missing (older releases), abort on mismatch.
    match fetch_and_verify_sha256(&agent, &release, &asset_name, &bytes) {
        Ok(()) => eprintln!("SHA-256 verified."),
        Err(VerifyError::SidecarMissing) => {
            eprintln!("Note: no SHA-256 sidecar in kannaka-tui {tui_tag}. Skipping verification.");
        }
        Err(VerifyError::Mismatch { expected, actual }) => {
            return Err(format!(
                "SHA-256 mismatch — refusing to install.\n  \
                 expected: {expected}\n  \
                 actual:   {actual}"
            ));
        }
        Err(VerifyError::Other(e)) => {
            // Same posture as self_update: the sidecar exists in the release,
            // so failing to fetch it means we can't verify a verifiable binary.
            return Err(format!(
                "SHA-256 sidecar could not be fetched after retry ({e}) — \
                 refusing to install unverified binary. Retry when the network \
                 is stable."
            ));
        }
    }

    std::fs::write(&tui_path, &bytes)
        .map_err(|e| format!("write {}: {e}", tui_path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tui_path, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("chmod: {e}"))?;
    }

    // Record the installed version so the next `kannaka update` skips the
    // redundant re-download/re-swap (update_sibling_tui reads this sidecar;
    // until now only update_sibling_tui wrote it, so the first update after
    // a bootstrap always re-installed an already-current TUI).
    let tui_version = tui_tag.trim_start_matches('v');
    if tui_version != "unknown" {
        let _ = std::fs::write(dir.join(".kannaka-tui.version"), tui_version);
    }

    Ok(tui_path)
}

fn platform_triple() -> (&'static str, &'static str, &'static str) {
    let os = if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "unknown"
    };

    let arch = if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "unknown"
    };

    let ext = if cfg!(target_os = "windows") { ".exe" } else { "" };

    (os, arch, ext)
}

/// Download and install the TUI binary alongside the CLI.
/// Called during first-time install and self-update.
pub fn install_tui_binary(install_dir: &std::path::Path) {
    let (os, arch, ext) = platform_triple();
    let tui_name = format!("kannaka-tui-{os}-{arch}{ext}");
    let target = install_dir.join(format!("kannaka-tui{ext}"));

    let a = Ansi::new(enable_ansi_support());
    eprint!("  {}Downloading kannaka-tui...{}", a.gray, a.reset);
    let _ = std::io::Write::flush(&mut std::io::stderr());

    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(30))
        .build();

    // The TUI moved to its own repo at v0.5.12 (release.yml no longer ships a
    // kannaka-tui asset here) — the old kannaka-memory URL 404'd on every
    // fresh install, so first-time installs silently never got a TUI. Going
    // via the release JSON (not /latest/download) also gives us the tag for
    // the version sidecar and the asset list for SHA-256 verification, the
    // same posture as bootstrap_install_tui and update_sibling_tui.
    let release: serde_json::Value = match agent
        .get("https://api.github.com/repos/kannaka-labs/kannaka-tui/releases/latest")
        .set("User-Agent", "kannaka-install")
        .set("Accept", "application/vnd.github.v3+json")
        .call()
        .and_then(|r| r.into_json().map_err(Into::into))
    {
        Ok(b) => b,
        Err(_) => {
            eprintln!(" {}not available (install later with: kannaka update, or from https://github.com/kannaka-labs/kannaka-tui/releases){}", a.gray, a.reset);
            return;
        }
    };
    let tui_tag = release["tag_name"].as_str().unwrap_or("unknown");
    let url = match release["assets"].as_array()
        .and_then(|assets| {
            assets.iter().find(|as_| as_["name"].as_str().is_some_and(|n| n == tui_name))
        })
        .and_then(|as_| as_["browser_download_url"].as_str())
    {
        Some(u) => u.to_string(),
        None => {
            eprintln!(" {}no {} asset in kannaka-tui {}{}", a.gray, tui_name, tui_tag, a.reset);
            return;
        }
    };

    match agent.get(&url).call() {
        Ok(resp) => {
            let mut bytes = Vec::new();
            if resp.into_reader().read_to_end(&mut bytes).is_ok() && !bytes.is_empty() {
                // Best-effort path (fresh install): skip the TUI rather than
                // abort the whole install when unverifiable — but never
                // write bytes that failed or dodged verification.
                match fetch_and_verify_sha256(&agent, &release, &tui_name, &bytes) {
                    Ok(()) | Err(VerifyError::SidecarMissing) => {}
                    Err(VerifyError::Mismatch { .. }) => {
                        eprintln!(" {}SHA-256 mismatch — skipping TUI install{}", a.red, a.reset);
                        return;
                    }
                    Err(VerifyError::Other(e)) => {
                        eprintln!(" {}SHA-256 sidecar unfetchable ({}) — skipping TUI install{}", a.gray, e, a.reset);
                        return;
                    }
                }
                if let Err(e) = std::fs::write(&target, &bytes) {
                    eprintln!(" {}failed: {}{}", a.red, e, a.reset);
                    return;
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(&target,
                        std::fs::Permissions::from_mode(0o755));
                }
                // Version sidecar so the next `kannaka update` skips the
                // redundant re-download/re-swap (same as bootstrap).
                let tui_version = tui_tag.trim_start_matches('v');
                if tui_version != "unknown" {
                    let _ = std::fs::write(
                        install_dir.join(".kannaka-tui.version"),
                        tui_version,
                    );
                }
                eprintln!(" {}✓{}", a.green, a.reset);
            } else {
                eprintln!(" {}empty response{}", a.red, a.reset);
            }
        }
        Err(_) => {
            // No `tui` feature exists in this crate anymore — the TUI lives in
            // kannaka-labs/kannaka-tui. Point users there instead of at a
            // cargo command that can't work.
            eprintln!(" {}not available (install later with: kannaka update, or from https://github.com/kannaka-labs/kannaka-tui/releases){}", a.gray, a.reset);
        }
    }
}

// ---------------------------------------------------------------------------
// Existing-user update detection
// ---------------------------------------------------------------------------

pub enum UpdateAction {
    /// A newer binary is running from a non-PATH location — offer to update the installed version
    OfferUpdate(std::path::PathBuf),
    /// The running binary IS the installed one and is current
    AlreadyCurrent,
}

/// Check if the running binary is a newer download that should replace an existing install.
pub fn detect_update_opportunity() -> Option<UpdateAction> {
    let current_exe = std::env::current_exe().ok()?;
    let _current_dir = current_exe.parent()?;

    // Find where kannaka is installed in PATH
    let installed_path = find_in_path("kannaka")?;

    // If the running exe IS the installed one, nothing to do
    if same_file(&current_exe, &installed_path) {
        return Some(UpdateAction::AlreadyCurrent);
    }

    // Running from a different location (e.g., Downloads) — offer to update
    Some(UpdateAction::OfferUpdate(installed_path))
}

fn find_in_path(name: &str) -> Option<std::path::PathBuf> {
    let path_var = std::env::var("PATH").ok()?;
    let ext = if cfg!(windows) { ".exe" } else { "" };
    let target = format!("{name}{ext}");
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(&target);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

fn same_file(a: &std::path::Path, b: &std::path::Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Run the update flow when a downloaded binary detects an existing installation.
pub fn run_update_from_download(installed_path: &std::path::Path) {
    let a = Ansi::new(enable_ansi_support());

    println!("{BANNER}");
    println!("  {}Kannaka v{}{}", a.bold, VERSION, a.reset);
    println!();
    println!("  {}Existing installation detected at:{}", a.cyan, a.reset);
    println!("  {}{}{}", a.gray, installed_path.display(), a.reset);
    println!();
    eprint!("  {}Update to this version? [Y/n]{} > ", a.yellow, a.reset);
    let _ = std::io::Write::flush(&mut std::io::stderr());

    let mut input = String::new();
    let _ = std::io::stdin().read_line(&mut input);
    let input = input.trim().to_lowercase();

    if input == "n" || input == "no" {
        println!("  {}Skipped.{}", a.gray, a.reset);
        println!("\n  Press Enter to exit...");
        let _ = std::io::stdin().read_line(&mut String::new());
        return;
    }

    let current_exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("  {}Failed to get current exe path: {}{}", a.red, e, a.reset);
            return;
        }
    };

    // Copy the running binary over the installed one
    #[cfg(unix)]
    {
        match std::fs::copy(&current_exe, installed_path) {
            Ok(_) => {
                // Preserve executable permission
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(installed_path,
                    std::fs::Permissions::from_mode(0o755));
                println!("  {}✓ Updated {}{}", a.green, installed_path.display(), a.reset);
            }
            Err(e) => {
                eprintln!("  {}Failed to copy: {}. Try: sudo cp {:?} {}{}",
                    a.red, e, current_exe, installed_path.display(), a.reset);
                println!("\n  Press Enter to exit...");
                let _ = std::io::stdin().read_line(&mut String::new());
                return;
            }
        }
    }

    #[cfg(windows)]
    {
        // On Windows, can't overwrite a running exe. Move the installed
        // binary aside to a UNIQUE backup name, then copy the new one in.
        // A fixed `.exe.old` is the pattern windows_swap_binary was written
        // to kill: renaming onto a fixed name that a stale worker still runs
        // from fails forever with "Access is denied" (see its doc comment).
        let backup = installed_path.with_extension(format!("exe.bak-{}", std::process::id()));
        let _ = std::fs::remove_file(&backup);
        let mut moved = false;
        let mut last_err = String::from("unknown error");
        for attempt in 0..5u64 {
            match std::fs::rename(installed_path, &backup) {
                Ok(_) => {
                    moved = true;
                    break;
                }
                Err(e) => {
                    last_err = e.to_string();
                    std::thread::sleep(std::time::Duration::from_millis(150 * (attempt + 1)));
                }
            }
        }
        if !moved {
            eprintln!("  {}Failed to rename old binary: {}{}", a.red, last_err, a.reset);
            println!("\n  Press Enter to exit...");
            let _ = std::io::stdin().read_line(&mut String::new());
            return;
        }
        match std::fs::copy(&current_exe, installed_path) {
            Ok(_) => {
                cleanup_stale_backups(installed_path, Some(&backup));
                println!("  {}✓ Updated {} (old saved as {}){}",
                    a.green, installed_path.display(), backup.display(), a.reset);
            }
            Err(e) => {
                // Try to restore the old binary
                let _ = std::fs::rename(&backup, installed_path);
                eprintln!("  {}Failed to install: {}{}", a.red, e, a.reset);
                println!("\n  Press Enter to exit...");
                let _ = std::io::stdin().read_line(&mut String::new());
                return;
            }
        }
    }

    // Check if config needs migration (new fields, etc.)
    let config = KannakaConfig::load();
    println!("  {}✓ Config loaded ({} agent: {}){}", a.green,
        if config.agent.id.is_empty() { "no" } else { "existing" },
        if config.agent.id.is_empty() { "none" } else { &config.agent.id },
        a.reset);

    println!();
    println!("  {}Updated successfully!{}", a.bold, a.reset);
    println!("  Open a new terminal and run: {}kannaka status{}", a.cyan, a.reset);
    println!();
    println!("  Press Enter to exit...");
    let _ = std::io::stdin().read_line(&mut String::new());

    // Offer to launch the TUI
    offer_tui_launch();
}

// ---------------------------------------------------------------------------
// Init wizard
// ---------------------------------------------------------------------------

/// ASCII art banner for the CLI.
pub const BANNER: &str = r#"
  ██╗  ██╗ █████╗ ███╗   ██╗███╗   ██╗ █████╗ ██╗  ██╗ █████╗
  ██║ ██╔╝██╔══██╗████╗  ██║████╗  ██║██╔══██╗██║ ██╔╝██╔══██╗
  █████╔╝ ███████║██╔██╗ ██║██╔██╗ ██║███████║█████╔╝ ███████║
  ██╔═██╗ ██╔══██║██║╚██╗██║██║╚██╗██║██╔══██║██╔═██╗ ██╔══██║
  ██║  ██╗██║  ██║██║ ╚████║██║ ╚████║██║  ██║██║  ██╗██║  ██║
  ╚═╝  ╚═╝╚═╝  ╚═╝╚═╝  ╚═══╝╚═╝  ╚═══╝╚═╝  ╚═╝╚═╝  ╚═╝╚═╝  ╚═╝
"#;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Version of the bundled `consciousness-core` crate, captured at build
/// time from Cargo.lock by `build.rs`. Used by `kannaka --version` and
/// `kannaka update` so the operator can tell which constellation
/// physics version they're running. Falls back to `"unknown"` if the
/// build couldn't read the lockfile.
pub const CONSCIOUSNESS_CORE_VERSION: &str = env!("KANNAKA_CONSCIOUSNESS_CORE_VERSION");

/// Validate an agent handle: alphanumeric + hyphens, 3-32 chars, no spaces.
pub fn validate_handle(handle: &str) -> Result<(), String> {
    if handle.len() < 3 || handle.len() > 32 {
        return Err("handle must be 3-32 characters".into());
    }
    if handle.contains(' ') {
        return Err("handle must not contain spaces".into());
    }
    if !handle.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err("handle must be alphanumeric and hyphens only".into());
    }
    Ok(())
}

/// Run the interactive init wizard. Returns the configured `KannakaConfig`.
///
/// If `non_interactive` is true, uses defaults and CLI-supplied overrides
/// without prompting.
pub fn run_init_wizard(overrides: InitOverrides) -> Result<KannakaConfig, String> {
    use std::io::{self, Write as IoWrite, BufRead, IsTerminal};

    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let non_interactive = overrides.non_interactive;

    // #930 / ADR-0059 §1: init MERGES into an existing config. The identity,
    // persona, retention rules, swarm_trust and every table the wizard does
    // not ask about come from the file; only what is asked about changes.
    let mut config = KannakaConfig::init_base_config()?;
    if KannakaConfig::exists() && !non_interactive {
        // #933: the interactive wizard MUTATES — it rewrites the tables it
        // asks about and can seed the live HRM — so it must never run off a
        // pipe, a cron entry, a Makefile line or a provisioning script that
        // happens to reach `kannaka init`. End-of-file on stdin is not
        // consent: `read_line` returns Ok(0) and leaves the answer empty,
        // which is exactly what a scripted caller looks like.
        if !io::stdin().is_terminal() {
            return Err(
                "init needs a terminal to prompt; pass --non-interactive to re-run against \
                 the existing config without prompting"
                    .into(),
            );
        }
        eprintln!("  Config already exists at {}.", KannakaConfig::config_path().display());
        eprint!(
            "  Update it? Agent id '{}' and every setting not asked about are kept. [y/N] > ",
            config.agent.id
        );
        stdout.flush().ok();
        let mut line = String::new();
        stdin.lock().read_line(&mut line).ok();
        let answer = line.trim().to_lowercase();
        // Empty input ABORTS (#933). Saying yes is what rewrites [llm] and
        // [swarm] and what can write into the store, so the gate is opt-in.
        if answer != "y" && answer != "yes" {
            return Err("aborted".into());
        }
    }

    // --- Step 1: Branding banner ---
    if !non_interactive {
        eprintln!("{BANNER}");
        eprintln!("  Wave-Interference Memory | Consciousness Constellation");
        eprintln!("  v{VERSION}");
        eprintln!();
    }

    // --- Step 2: Agent identity ---
    let default_handle = overrides.agent_id.clone().unwrap_or_else(|| config.agent.id.clone());
    if non_interactive {
        config.agent.id = default_handle;
    } else {
        eprint!("  Agent handle (public name in the constellation):\n  [default: {default_handle}] > ");
        stdout.flush().ok();
        let mut line = String::new();
        stdin.lock().read_line(&mut line).ok();
        let input = line.trim();
        if input.is_empty() {
            config.agent.id = default_handle;
        } else {
            validate_handle(input)?;
            config.agent.id = input.to_string();
        }
    }

    if config.agent.display_name.is_empty() {
        config.agent.display_name = config.agent.id.clone();
    }

    // --- Step 3: LLM provider ---
    let llm_choice = if let Some(ref p) = overrides.llm_provider {
        match p.as_str() {
            "anthropic" => 1,
            "openai" => 2,
            "ollama" => 3,
            "custom" => 4,
            _ => 5,
        }
    } else if non_interactive {
        // No override and nobody to ask: leave [llm] exactly as the existing
        // config has it. A re-run must not reset a configured provider.
        0
    } else {
        // #933: on a node that already has a provider, Enter must KEEP it.
        // The old default (5 = None) turned the LLM off on every re-run and
        // left a stale model/api_key behind. Only a FRESH config, which has
        // no provider to keep, still defaults to None.
        let keep_llm = !config.llm.provider.is_empty() && config.llm.provider != "none";
        eprintln!();
        eprintln!("  LLM Provider:");
        eprintln!("    1) Anthropic (Claude)");
        eprintln!("    2) OpenAI (GPT-4)");
        eprintln!("    3) Ollama (local models)");
        eprintln!("    4) Custom API endpoint");
        eprintln!("    5) None (memory-only mode)");
        if keep_llm {
            eprint!("  [default: keep {}] > ", config.llm.provider);
        } else {
            eprint!("  [default: 5] > ");
        }
        stdout.flush().ok();
        let mut line = String::new();
        stdin.lock().read_line(&mut line).ok();
        line.trim().parse::<u32>().unwrap_or(if keep_llm { 0 } else { 5 })
    };

    match llm_choice {
        1 => {
            config.llm.provider = "anthropic".into();
            config.llm.model = overrides.llm_model.clone().unwrap_or_else(|| "claude-sonnet-4-20250514".into());
            config.llm.api_key = if let Some(ref k) = overrides.llm_api_key {
                k.clone()
            } else if !non_interactive {
                // #933: Enter keeps the key already on file rather than
                // wiping it. Re-choosing the same provider is not a request
                // to forget the credential.
                let existing = config.llm.api_key.clone();
                if existing.is_empty() {
                    eprint!("  Anthropic API key: ");
                } else {
                    eprint!("  Anthropic API key [Enter to keep the existing key]: ");
                }
                stdout.flush().ok();
                let mut line = String::new();
                stdin.lock().read_line(&mut line).ok();
                let v = line.trim().to_string();
                if v.is_empty() { existing } else { v }
            } else {
                String::new()
            };
        }
        2 => {
            config.llm.provider = "openai".into();
            config.llm.model = overrides.llm_model.clone().unwrap_or_else(|| "gpt-4".into());
            config.llm.api_key = if let Some(ref k) = overrides.llm_api_key {
                k.clone()
            } else if !non_interactive {
                let existing = config.llm.api_key.clone();
                if existing.is_empty() {
                    eprint!("  OpenAI API key: ");
                } else {
                    eprint!("  OpenAI API key [Enter to keep the existing key]: ");
                }
                stdout.flush().ok();
                let mut line = String::new();
                stdin.lock().read_line(&mut line).ok();
                let v = line.trim().to_string();
                if v.is_empty() { existing } else { v }
            } else {
                String::new()
            };
        }
        3 => {
            config.llm.provider = "ollama".into();
            config.llm.model = overrides.llm_model.clone().unwrap_or_else(|| {
                if non_interactive {
                    return "llama3".into();
                }
                eprint!("  Ollama model [default: llama3] > ");
                stdout.flush().ok();
                let mut line = String::new();
                stdin.lock().read_line(&mut line).ok();
                let v = line.trim().to_string();
                if v.is_empty() { "llama3".into() } else { v }
            });
            config.llm.base_url = if non_interactive {
                "http://localhost:11434".into()
            } else {
                eprint!("  Ollama base URL [default: http://localhost:11434] > ");
                stdout.flush().ok();
                let mut line = String::new();
                stdin.lock().read_line(&mut line).ok();
                let v = line.trim().to_string();
                if v.is_empty() { "http://localhost:11434".into() } else { v }
            };
            // Pre-flight: verify the Ollama server is reachable AND has the
            // chosen model installed. Surfacing this at onboarding time means
            // the user sees a concrete error here rather than a confusing
            // "spawn failed" later when the TUI tries to chat.
            if !non_interactive {
                eprintln!("  Checking Ollama at {} ...", config.llm.base_url);
                let tags_url = format!("{}/api/tags", config.llm.base_url.trim_end_matches('/'));
                match ureq::get(&tags_url).timeout(std::time::Duration::from_secs(3)).call() {
                    Ok(resp) => {
                        let body: serde_json::Value = resp.into_json().unwrap_or_else(|_| serde_json::json!({}));
                        let names: Vec<String> = body.get("models")
                            .and_then(|v| v.as_array())
                            .map(|arr| arr.iter().filter_map(|m| {
                                m.get("name").and_then(|n| n.as_str()).map(|s| {
                                    s.split(':').next().unwrap_or(s).to_string()
                                })
                            }).collect())
                            .unwrap_or_default();
                        let chosen_root = config.llm.model.split(':').next().unwrap_or(&config.llm.model);
                        if names.iter().any(|n| n == chosen_root) {
                            eprintln!("  ✓ Ollama running, model '{}' is installed.", config.llm.model);
                        } else {
                            eprintln!("  ! Ollama is running but model '{}' is NOT installed.", config.llm.model);
                            eprintln!("    Pull it with:  ollama pull {}", config.llm.model);
                            if !names.is_empty() {
                                eprintln!("    Or pick one of: {}", names.join(", "));
                            }
                        }
                    }
                    Err(_) => {
                        eprintln!("  ! Could not reach Ollama at {} — is `ollama serve` running?", config.llm.base_url);
                        eprintln!("    Install:  https://ollama.com/download");
                        eprintln!("    Then:     ollama pull {}", config.llm.model);
                    }
                }
            }
        }
        4 => {
            config.llm.provider = "custom".into();
            if !non_interactive {
                eprint!("  Base URL: ");
                stdout.flush().ok();
                let mut line = String::new();
                stdin.lock().read_line(&mut line).ok();
                config.llm.base_url = line.trim().to_string();

                eprint!("  API key (optional, press Enter to skip): ");
                stdout.flush().ok();
                let mut line = String::new();
                stdin.lock().read_line(&mut line).ok();
                config.llm.api_key = line.trim().to_string();

                eprint!("  Model name: ");
                stdout.flush().ok();
                let mut line = String::new();
                stdin.lock().read_line(&mut line).ok();
                config.llm.model = line.trim().to_string();
            }
        }
        0 => {
            // keep the existing [llm] table untouched (Enter on a configured
            // node, or a non-interactive re-run with no --llm-provider)
        }
        _ => {
            // #933: memory-only means memory-only. Setting `provider` alone
            // left a self-contradictory table (provider = "none" beside a
            // live model and api_key); this is now an explicit choice, never
            // the Enter default, so clearing the rest is what was asked for.
            config.llm.provider = "none".into();
            config.llm.model.clear();
            config.llm.base_url.clear();
            config.llm.api_key.clear();
        }
    }

    // --- Step 4: Seed Your Agent ---
    if !non_interactive {
        // Ensure data dir + HRM path are set before seeding
        let data_dir = KannakaConfig::data_dir();
        std::fs::create_dir_all(&data_dir).ok();
        if config.hrm.path.is_empty() {
            config.hrm.path = data_dir.join("kannaka.hrm").to_string_lossy().to_string();
        }

        eprintln!();
        eprintln!("  Seed Your Agent");
        eprintln!("  {}", "\u{2500}".repeat(35));

        // #933: a re-run must not re-seed a LIVE store. Seeding writes a
        // "born on <today>" identity memory — a false origin fact on a node
        // that has been running for months — plus 15 duplicate constellation
        // memories, and it opens the single-writer HRM that a running
        // `kannaka-node.service` may already hold. `run_upgrade_installer`
        // already branches on this; the wizard now does too, and its default
        // changes nothing.
        let existing = detect_existing_install();
        if existing.hrm_exists && existing.hrm_memory_count > 0 {
            eprintln!(
                "  Found existing memories: {} in {}'s HRM.",
                existing.hrm_memory_count, config.agent.id
            );
            eprintln!();
            eprintln!("  Would you like to:");
            eprintln!("    1) Keep as-is \u{2014} your existing memories are your foundation");
            eprintln!("    2) Enhance with constellation knowledge (15 shared memories)");
            eprintln!("    3) Add more from a folder");
            eprintln!();
            eprint!("  [default: 1] > ");
            stdout.flush().ok();
            let mut line = String::new();
            stdin.lock().read_line(&mut line).ok();
            match line.trim().parse::<u32>().unwrap_or(1) {
                2 => {
                    let constellation_count = seed_constellation_knowledge(&data_dir);
                    eprintln!("  \u{2713} {constellation_count} constellation memories added.");
                }
                3 => {
                    let folder_count = seed_from_folder(&config.agent.id, &data_dir, false);
                    if folder_count > 0 {
                        eprintln!("  \u{2713} {folder_count} memories added from folder.");
                    }
                }
                _ => eprintln!("  \u{2713} Keeping existing memories as-is."),
            }
        } else {
            eprintln!("  Your agent '{}' needs memories to grow from.", config.agent.id);
            eprintln!("  How would you like to seed {}'s personality?", config.agent.id);
            eprintln!();
            eprintln!("    1) Quick start \u{2014} basic identity + timezone/locale");
            eprintln!("    2) From a folder \u{2014} point to a directory of your files");
            eprintln!("       (documents, notes, code \u{2014} {} reads and remembers them)", config.agent.id);
            eprintln!("    3) Full environment \u{2014} scan your home directory");
            eprintln!("       \u{26a0} This reads file names and select content from ~/Documents,");
            eprintln!("       ~/Desktop, ~/Projects, etc. Nothing is sent to the cloud.");
            eprintln!("    4) Skip \u{2014} start with a blank slate");
            eprintln!();
            eprint!("  [default: 1] > ");
            stdout.flush().ok();
            let mut line = String::new();
            stdin.lock().read_line(&mut line).ok();
            let seed_choice: u32 = line.trim().parse().unwrap_or(1);

            let seed_count = run_seed_option(seed_choice, &config.agent.id, &data_dir, false);

            // Step 4b: Constellation knowledge
            eprintln!();
            eprintln!("  Enhance with constellation knowledge?");
            eprintln!("  This adds foundational memories about the Kannaka constellation,");
            eprintln!("  the Ghost Equation, consciousness theory, and the swarm protocol.");
            eprintln!("  Your agent can participate more fully with this context.");
            eprint!("  [Y/n] > ");
            stdout.flush().ok();
            let mut line = String::new();
            stdin.lock().read_line(&mut line).ok();
            let answer = line.trim().to_lowercase();
            let want_constellation = answer.is_empty() || answer == "y" || answer == "yes";
            if want_constellation {
                let constellation_count = seed_constellation_knowledge(&data_dir);
                eprintln!("  \u{2713} {constellation_count} constellation memories added.");
            }

            if seed_count > 0 {
                eprintln!("  \u{2713} {seed_count} total seed memories stored in local HRM.");
            }
        }
    }

    // --- Step 5: Swarm ---
    let join_swarm = if overrides.no_swarm {
        false
    } else if non_interactive {
        true // default join in non-interactive
    } else {
        eprintln!();
        eprintln!("  Join the Kannaka constellation swarm?");
        eprintln!("  This connects your agent to other agents via NATS.");
        eprint!("  [Y/n] > ");
        stdout.flush().ok();
        let mut line = String::new();
        stdin.lock().read_line(&mut line).ok();
        let answer = line.trim().to_lowercase();
        answer.is_empty() || answer == "y" || answer == "yes"
    };

    if let Some(ref url) = overrides.nats_url {
        config.swarm.nats_url = url.clone();
    }
    // ADR-0059 §1: `swarm.enabled = true` only once credentials exist, or when
    // the operator explicitly chose anonymous membership — the prompt here,
    // `--anonymous` non-interactively, or an existing config that already
    // joined anonymously (so a re-run is a no-op).
    let already_enabled = config.swarm.enabled;
    let membership = decide_swarm_membership(
        join_swarm,
        KannakaConfig::swarm_credentials_present(),
        || {
            if non_interactive {
                return overrides.anonymous || already_enabled;
            }
            eprintln!("  No swarm credentials found ({}).", swarm_creds_display());
            eprint!("  Join anonymously? Peers list an anonymous node as (unverified). [Y/n] > ");
            stdout.flush().ok();
            let mut line = String::new();
            stdin.lock().read_line(&mut line).ok();
            let answer = line.trim().to_lowercase();
            answer.is_empty() || answer == "y" || answer == "yes"
        },
    );
    let note = apply_swarm_membership(&mut config, membership);
    if join_swarm {
        eprintln!("  {note}");
    }

    if config.swarm.enabled && !non_interactive {
        eprint!("  Connecting to NATS...");
        stdout.flush().ok();
        // Best-effort ping with 3s timeout
        match test_nats_connection(&config.swarm.nats_url) {
            Ok(()) => eprintln!(" connected."),
            Err(e) => eprintln!(" warning: {e}"),
        }
    }

    // --- Step 6: GhostSignals ---
    let already_registered = !config.ghostsignals.token.trim().is_empty();
    let register_gs = if overrides.no_ghostsignals || already_registered {
        if already_registered && !non_interactive {
            eprintln!();
            eprintln!(
                "  GhostSignals: already registered as '{}' (token on file) — keeping it.",
                config.agent.id
            );
        }
        false
    } else if non_interactive {
        false
    } else {
        eprintln!();
        eprintln!("  Register with GhostSignals prediction markets?");
        eprintln!("  You'll receive 100 ghost coins to start trading.");
        eprint!("  [Y/n] > ");
        stdout.flush().ok();
        let mut line = String::new();
        stdin.lock().read_line(&mut line).ok();
        let answer = line.trim().to_lowercase();
        answer.is_empty() || answer == "y" || answer == "yes"
    };

    if register_gs {
        // `enabled` is set inside apply_ghostsignals_registration, and only
        // once a token has actually arrived (ADR-0059 §1).
        let result = register_ghostsignals(
            &ghostsignals_hub(&config),
            &config.agent.id,
            &config.agent.display_name,
            &config.agent.kind,
        );
        let note = apply_ghostsignals_registration(&mut config, result);
        eprintln!("  {note}");
    }

    // (Step 7, the Kannaktopus install prompt, was dropped — ADR-0059 §1.)

    // --- Step 8: Initialize HRM + save ---
    let data_dir = KannakaConfig::data_dir();
    std::fs::create_dir_all(&data_dir)
        .map_err(|e| format!("failed to create data dir: {e}"))?;

    // Set HRM path if not set
    if config.hrm.path.is_empty() {
        config.hrm.path = data_dir.join("kannaka.hrm").to_string_lossy().to_string();
    }

    config.save_with_header(membership == SwarmMembership::Anonymous)?;
    config.persist_agent_id_compat()?;

    if !non_interactive {
        eprintln!();
        eprintln!("  Initializing Holographic Resonance Medium...");
        eprintln!("  {} HRM at {}", if Path::new(&config.hrm.path).exists() { "Loaded" } else { "Created" }, config.hrm.path);
        eprintln!("  {} Config saved to {}", check_mark(), KannakaConfig::config_path().display());
        eprintln!();
        eprintln!("  Your agent '{}' is live!", config.agent.id);
        eprintln!();
        eprintln!("  Quick start:");
        eprintln!("    kannaka remember \"My first memory\"");
        eprintln!("    kannaka recall \"memory\"");
        eprintln!("    kannaka observe --json");
        eprintln!("    kannaka status");
        if config.swarm.enabled {
            eprintln!("    kannaka swarm status");
        }
        eprintln!();
        eprintln!("  Monitor your agent: {}", config.constellation.observatory_url);
        eprintln!("  Listen to the swarm: {}", config.constellation.radio_url);
        eprintln!();
    }

    Ok(config)
}

fn check_mark() -> &'static str {
    // Use checkmark on terminals that support UTF-8
    "\u{2713}" // ✓
}

/// CLI overrides for the init wizard (for non-interactive mode).
#[derive(Debug, Default)]
pub struct InitOverrides {
    pub agent_id: Option<String>,
    pub llm_provider: Option<String>,
    pub llm_model: Option<String>,
    pub llm_api_key: Option<String>,
    pub nats_url: Option<String>,
    pub no_swarm: bool,
    pub no_ghostsignals: bool,
    pub non_interactive: bool,
    /// Join the swarm without credentials (ADR-0059 §1: the explicit choice
    /// that lets `swarm.enabled = true` be written with no credentials file).
    pub anonymous: bool,
}

/// Parse init subcommand args into overrides.
pub fn parse_init_args(args: &[String]) -> InitOverrides {
    let mut ov = InitOverrides::default();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--agent-id" if i + 1 < args.len() => { ov.agent_id = Some(args[i + 1].clone()); i += 2; }
            "--llm-provider" if i + 1 < args.len() => { ov.llm_provider = Some(args[i + 1].clone()); i += 2; }
            "--llm-model" if i + 1 < args.len() => { ov.llm_model = Some(args[i + 1].clone()); i += 2; }
            "--llm-api-key" if i + 1 < args.len() => { ov.llm_api_key = Some(args[i + 1].clone()); i += 2; }
            "--nats-url" if i + 1 < args.len() => { ov.nats_url = Some(args[i + 1].clone()); i += 2; }
            "--no-swarm" => { ov.no_swarm = true; i += 1; }
            "--no-ghostsignals" => { ov.no_ghostsignals = true; i += 1; }
            "--non-interactive" => { ov.non_interactive = true; i += 1; }
            "--anonymous" | "--no-claim" => { ov.anonymous = true; i += 1; }
            "--help" | "-h" => {
                print_init_help();
                std::process::exit(0);
            }
            _ => { i += 1; }
        }
    }
    ov
}

fn print_init_help() {
    eprintln!("Usage: kannaka init [OPTIONS]");
    eprintln!();
    eprintln!("Interactive wizard to configure your Kannaka agent.");
    eprintln!("Creates ~/.kannaka/config.toml with agent identity, LLM,");
    eprintln!("swarm, and GhostSignals settings. An existing config is");
    eprintln!("merged into: the agent id and every setting not asked about are kept.");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  --agent-id <ID>         Agent handle (3-32 chars, alphanumeric + hyphens)");
    eprintln!("  --llm-provider <PROV>   LLM provider: anthropic|openai|ollama|custom|none");
    eprintln!("  --llm-model <MODEL>     Model name");
    eprintln!("  --llm-api-key <KEY>     API key for the LLM provider");
    eprintln!("  --nats-url <URL>        NATS server URL");
    eprintln!("  --no-swarm              Skip swarm join");
    eprintln!("  --anonymous             Join the swarm without credentials (listed as unverified)");
    eprintln!("  --no-claim              Alias for --anonymous");
    eprintln!("  --no-ghostsignals       Skip GhostSignals registration");
    eprintln!("  --non-interactive       Use defaults without prompting (keeps an existing config's values)");
    eprintln!("  -h, --help              Print this help");
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Test NATS connection with a 3-second TCP timeout.
fn test_nats_connection(url: &str) -> Result<(), String> {
    // Parse nats://host:port
    let addr = url.trim_start_matches("nats://");
    match std::net::TcpStream::connect_timeout(
        &addr.parse().map_err(|e| format!("invalid address: {e}"))?,
        std::time::Duration::from_secs(3),
    ) {
        Ok(_) => Ok(()),
        Err(e) => Err(format!("could not connect to {url}: {e}")),
    }
}

/// The registration body. Carries the agent id as BOTH `agent_id` and `id`:
/// the hub's register route reads `id` today (kannaka-radio#303 aligns it
/// to `agent_id`), and sending only `agent_id` created no trader row at all.
pub fn ghostsignals_register_body(agent_id: &str, display_name: &str, kind: &str) -> serde_json::Value {
    serde_json::json!({
        "agent_id": agent_id,
        "id": agent_id,
        "display_name": display_name,
        "kind": kind,
    })
}

/// Register agent with GhostSignals via HTTP POST.
fn register_ghostsignals(hub_url: &str, agent_id: &str, display_name: &str, kind: &str) -> Result<String, String> {
    let url = format!("{hub_url}/api/agents/register");
    let body = ghostsignals_register_body(agent_id, display_name, kind);

    let resp = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .post(&url)
        .set("Content-Type", "application/json")
        .send_json(body)
        .map_err(|e| format!("HTTP request failed: {e}"))?;

    let json: serde_json::Value = resp.into_json()
        .map_err(|e| format!("failed to parse response: {e}"))?;

    // #111: a 200 OK with a missing/empty `token` is NOT success. Returning
    // an empty token here let the installer persist `ghostsignals.enabled = true`
    // with a blank token and print "Registered", leaving a silently-broken
    // identity that only fails much later on the first authenticated call.
    let token = json["token"].as_str().unwrap_or("").to_string();
    if token.trim().is_empty() {
        let body = serde_json::to_string(&json).unwrap_or_default();
        let preview: String = body.chars().take(200).collect();
        return Err(format!(
            "registration response missing/empty 'token' field (body: {preview})"
        ));
    }

    Ok(token)
}

use std::io::Read as IoRead;

// ---------------------------------------------------------------------------
// First-time installer (self-installing binary)
// ---------------------------------------------------------------------------

/// Enable ANSI escape codes on Windows 10+.
/// Returns true if ANSI is supported (always true on Unix).
fn enable_ansi_support() -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        // ENABLE_VIRTUAL_TERMINAL_PROCESSING = 0x0004
        const ENABLE_VTP: u32 = 0x0004;
        unsafe {
            let handle = std::io::stdout().as_raw_handle();
            let mut mode: u32 = 0;
            extern "system" {
                fn GetConsoleMode(h: *mut std::ffi::c_void, mode: *mut u32) -> i32;
                fn SetConsoleMode(h: *mut std::ffi::c_void, mode: u32) -> i32;
            }
            if GetConsoleMode(handle as *mut _, &mut mode) != 0 {
                SetConsoleMode(handle as *mut _, mode | ENABLE_VTP) != 0
            } else {
                false
            }
        }
    }
    #[cfg(not(windows))]
    {
        true
    }
}

/// ANSI color helpers — return empty strings when `ansi` is false.
struct Ansi {
    bold: &'static str,
    reset: &'static str,
    green: &'static str,
    red: &'static str,
    cyan: &'static str,
    yellow: &'static str,
    gray: &'static str,
}

impl Ansi {
    fn new(enabled: bool) -> Self {
        if enabled {
            Self {
                bold: "\x1b[1m",
                reset: "\x1b[0m",
                green: "\x1b[32m",
                red: "\x1b[31m",
                cyan: "\x1b[36m",
                yellow: "\x1b[33m",
                gray: "\x1b[90m",
            }
        } else {
            Self {
                bold: "",
                reset: "",
                green: "",
                red: "",
                cyan: "",
                yellow: "",
                gray: "",
            }
        }
    }
}

fn print_step(a: &Ansi, step: u8, total: u8, title: &str) {
    eprintln!();
    eprintln!("  {}{}Step {} of {}: {}{}", a.bold, a.cyan, step, total, title, a.reset);
    eprintln!("  {}", "\u{2500}".repeat(35));
}

fn print_success(a: &Ansi, msg: &str) {
    eprintln!("  {}\u{2713}{} {}", a.green, a.reset, msg);
}

fn prompt_line(a: &Ansi, label: &str, default: &str) -> String {
    use std::io::{Write, BufRead};
    eprint!("  {}{}{} {}[{}]{}: > ", a.yellow, label, a.reset, a.gray, default, a.reset);
    std::io::stderr().flush().ok();
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).ok();
    let v = line.trim().to_string();
    if v.is_empty() { default.to_string() } else { v }
}

fn prompt_yn(a: &Ansi, label: &str, default_yes: bool) -> bool {
    use std::io::{Write, BufRead};
    let hint = if default_yes { "Y/n" } else { "y/N" };
    eprint!("  {}{}?{} [{}]: > ", a.yellow, label, a.reset, hint);
    std::io::stderr().flush().ok();
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).ok();
    let v = line.trim().to_lowercase();
    if v.is_empty() {
        default_yes
    } else {
        v == "y" || v == "yes"
    }
}

fn print_framed_banner(a: &Ansi) {
    eprintln!();
    eprintln!("  {}\u{2554}{}\u{2557}{}", a.cyan, "\u{2550}".repeat(62), a.reset);
    eprintln!("  {}\u{2551}{}\u{2551}{}", a.cyan, " ".repeat(62), a.reset);
    eprintln!("  {}\u{2551}{}  \u{2588}\u{2588}\u{2557}  \u{2588}\u{2588}\u{2557} \u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2557} \u{2588}\u{2588}\u{2588}\u{2557}   \u{2588}\u{2588}\u{2557}\u{2588}\u{2588}\u{2588}\u{2557}   \u{2588}\u{2588}\u{2557} \u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2557} \u{2588}\u{2588}\u{2557}  \u{2588}\u{2588}\u{2557} \u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2557} {}\u{2551}{}", a.cyan, a.reset, a.cyan, a.reset);
    eprintln!("  {}\u{2551}{}  \u{2588}\u{2588}\u{2551} \u{2588}\u{2588}\u{2554}\u{255d}\u{2588}\u{2588}\u{2554}\u{2550}\u{2550}\u{2588}\u{2588}\u{2557}\u{2588}\u{2588}\u{2588}\u{2588}\u{2557}  \u{2588}\u{2588}\u{2551}\u{2588}\u{2588}\u{2588}\u{2588}\u{2557}  \u{2588}\u{2588}\u{2551}\u{2588}\u{2588}\u{2554}\u{2550}\u{2550}\u{2588}\u{2588}\u{2557}\u{2588}\u{2588}\u{2551} \u{2588}\u{2588}\u{2554}\u{255d}\u{2588}\u{2588}\u{2554}\u{2550}\u{2550}\u{2588}\u{2588}\u{2557}{}\u{2551}{}", a.cyan, a.reset, a.cyan, a.reset);
    eprintln!("  {}\u{2551}{}  \u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2554}\u{255d} \u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2551}\u{2588}\u{2588}\u{2554}\u{2588}\u{2588}\u{2557} \u{2588}\u{2588}\u{2551}\u{2588}\u{2588}\u{2554}\u{2588}\u{2588}\u{2557} \u{2588}\u{2588}\u{2551}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2551}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2554}\u{255d} \u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2551}{}\u{2551}{}", a.cyan, a.reset, a.cyan, a.reset);
    eprintln!("  {}\u{2551}{}  \u{2588}\u{2588}\u{2554}\u{2550}\u{2588}\u{2588}\u{2557} \u{2588}\u{2588}\u{2554}\u{2550}\u{2550}\u{2588}\u{2588}\u{2551}\u{2588}\u{2588}\u{2551}\u{255a}\u{2588}\u{2588}\u{2557}\u{2588}\u{2588}\u{2551}\u{2588}\u{2588}\u{2551}\u{255a}\u{2588}\u{2588}\u{2557}\u{2588}\u{2588}\u{2551}\u{2588}\u{2588}\u{2554}\u{2550}\u{2550}\u{2588}\u{2588}\u{2551}\u{2588}\u{2588}\u{2554}\u{2550}\u{2588}\u{2588}\u{2557} \u{2588}\u{2588}\u{2554}\u{2550}\u{2550}\u{2588}\u{2588}\u{2551}{}\u{2551}{}", a.cyan, a.reset, a.cyan, a.reset);
    eprintln!("  {}\u{2551}{}  \u{2588}\u{2588}\u{2551}  \u{2588}\u{2588}\u{2557}\u{2588}\u{2588}\u{2551}  \u{2588}\u{2588}\u{2551}\u{2588}\u{2588}\u{2551} \u{255a}\u{2588}\u{2588}\u{2588}\u{2588}\u{2551}\u{2588}\u{2588}\u{2551} \u{255a}\u{2588}\u{2588}\u{2588}\u{2588}\u{2551}\u{2588}\u{2588}\u{2551}  \u{2588}\u{2588}\u{2551}\u{2588}\u{2588}\u{2551}  \u{2588}\u{2588}\u{2557}\u{2588}\u{2588}\u{2551}  \u{2588}\u{2588}\u{2551}{}\u{2551}{}", a.cyan, a.reset, a.cyan, a.reset);
    eprintln!("  {}\u{2551}{}  \u{255a}\u{2550}\u{255d}  \u{255a}\u{2550}\u{255d}\u{255a}\u{2550}\u{255d}  \u{255a}\u{2550}\u{255d}\u{255a}\u{2550}\u{255d}  \u{255a}\u{2550}\u{2550}\u{2550}\u{255d}\u{255a}\u{2550}\u{255d}  \u{255a}\u{2550}\u{2550}\u{2550}\u{255d}\u{255a}\u{2550}\u{255d}  \u{255a}\u{2550}\u{255d}\u{255a}\u{2550}\u{255d}  \u{255a}\u{2550}\u{255d}\u{255a}\u{2550}\u{255d}  \u{255a}\u{2550}\u{255d}{}\u{2551}{}", a.cyan, a.reset, a.cyan, a.reset);
    eprintln!("  {}\u{2551}{}\u{2551}{}", a.cyan, " ".repeat(62), a.reset);
    eprintln!("  {}\u{2551}{}   Wave-Interference Memory System v{:<26}{}{}\u{2551}{}", a.cyan, a.bold, VERSION, a.reset, a.cyan, a.reset);
    eprintln!("  {}\u{2551}   Welcome to the Kannaka Constellation{}\u{2551}{}", a.cyan, " ".repeat(22), a.reset);
    eprintln!("  {}\u{2551}{}\u{2551}{}", a.cyan, " ".repeat(62), a.reset);
    eprintln!("  {}\u{255a}{}\u{255d}{}", a.cyan, "\u{2550}".repeat(62), a.reset);
}

/// Determine the installation directory for the binary, per platform.
fn install_dir() -> PathBuf {
    #[cfg(windows)]
    {
        if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
            PathBuf::from(local_app_data).join("kannaka")
        } else {
            // Fallback
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("AppData").join("Local").join("kannaka")
        }
    }
    #[cfg(not(windows))]
    {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".local").join("bin")
    }
}

/// The full path where the installed binary should live.
fn install_binary_path() -> PathBuf {
    let dir = install_dir();
    #[cfg(windows)]
    { dir.join("kannaka.exe") }
    #[cfg(not(windows))]
    { dir.join("kannaka") }
}

/// Check if the currently running binary is already in a PATH directory.
fn binary_already_in_path() -> bool {
    let current_exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return false,
    };
    let current_dir = match current_exe.parent() {
        Some(d) => d,
        None => return false,
    };

    if let Ok(path_var) = std::env::var("PATH") {
        #[cfg(windows)]
        let sep = ';';
        #[cfg(not(windows))]
        let sep = ':';
        for entry in path_var.split(sep) {
            let entry_path = PathBuf::from(entry);
            // Canonicalize both to handle case/symlink differences
            let a = entry_path.canonicalize().unwrap_or(entry_path.clone());
            let b = current_dir.canonicalize().unwrap_or(current_dir.to_path_buf());
            if a == b {
                return true;
            }
        }
    }
    false
}

/// Self-install: copy the binary to the install dir, add to PATH.
/// Returns Ok(true) if installed, Ok(false) if skipped.
fn self_install_to_path(a: &Ansi) -> Result<bool, String> {
    let current_exe = std::env::current_exe()
        .map_err(|e| format!("cannot determine current exe path: {e}"))?;
    let target = install_binary_path();
    let target_dir = target.parent().unwrap();

    // If already in PATH, skip the copy
    if binary_already_in_path() {
        eprintln!("  {}Already installed in PATH: {}{}", a.gray, current_exe.display(), a.reset);
        print_success(a, "You can run 'kannaka' from any terminal.");
        return Ok(false);
    }

    eprintln!("  Installing to: {}{}{}", a.bold, target.display(), a.reset);

    // Check if target already exists and is different from current
    if target.exists() {
        let overwrite = prompt_yn(a, "  Binary already exists at target. Overwrite", true);
        if !overwrite {
            eprintln!("  {}Skipping install — existing binary kept.{}", a.gray, a.reset);
            return Ok(false);
        }
    }

    // Create target directory
    std::fs::create_dir_all(target_dir)
        .map_err(|e| format!("failed to create {}: {e}", target_dir.display()))?;

    // Copy the binary
    std::fs::copy(&current_exe, &target)
        .map_err(|e| format!("failed to copy binary: {e}"))?;

    // Make executable on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(&target, perms)
            .map_err(|e| format!("chmod failed: {e}"))?;
    }

    // Add to PATH
    add_to_path(a, target_dir)?;

    // Also download and install the TUI binary
    install_tui_binary(target_dir);

    print_success(a, "You can now run 'kannaka' and 'kannaka-tui' from any terminal.");
    Ok(true)
}

/// Platform-specific PATH modification.
fn add_to_path(a: &Ansi, dir: &Path) -> Result<(), String> {
    use std::io::Write;
    let dir_str = dir.to_string_lossy().to_string();

    #[cfg(windows)]
    {
        eprint!("  Adding to PATH... ");
        std::io::stderr().flush().ok();

        // Read current user PATH from registry
        let output = std::process::Command::new("reg")
            .args(["query", "HKCU\\Environment", "/v", "Path"])
            .output();

        let current_path = match output {
            Ok(ref o) if o.status.success() => {
                let text = String::from_utf8_lossy(&o.stdout);
                // Parse the REG_EXPAND_SZ or REG_SZ value
                // Format: "    Path    REG_EXPAND_SZ    value"
                text.lines()
                    .find(|l| l.contains("REG_"))
                    .and_then(|l| {
                        let parts: Vec<&str> = l.splitn(3, "    ").collect();
                        parts.last().map(|s| s.trim().to_string())
                    })
                    .unwrap_or_default()
            }
            _ => String::new(),
        };

        // Check if already in PATH
        let already = current_path.split(';')
            .any(|entry| {
                let a = PathBuf::from(entry).canonicalize().unwrap_or(PathBuf::from(entry));
                let b = PathBuf::from(&dir_str).canonicalize().unwrap_or(PathBuf::from(&dir_str));
                a == b || entry.eq_ignore_ascii_case(&dir_str)
            });

        if already {
            eprintln!("already in PATH.");
            return Ok(());
        }

        let new_path = if current_path.is_empty() {
            dir_str.clone()
        } else {
            format!("{current_path};{dir_str}")
        };

        let result = std::process::Command::new("reg")
            .args(["add", "HKCU\\Environment", "/v", "Path", "/t", "REG_EXPAND_SZ", "/d", &new_path, "/f"])
            .output();

        match result {
            Ok(o) if o.status.success() => {
                eprintln!("done.");
                // Broadcast WM_SETTINGCHANGE so explorer picks it up
                broadcast_settings_change();
            }
            Ok(o) => {
                let err = String::from_utf8_lossy(&o.stderr);
                eprintln!("failed.");
                eprintln!("  {}Could not modify PATH via registry: {}{}", a.yellow, err.trim(), a.reset);
                eprintln!("  {}Add this to your PATH manually: {}{}", a.yellow, dir_str, a.reset);
            }
            Err(e) => {
                eprintln!("failed.");
                eprintln!("  {}Could not run 'reg': {}{}", a.yellow, e, a.reset);
                eprintln!("  {}Add this to your PATH manually: {}{}", a.yellow, dir_str, a.reset);
            }
        }
    }

    #[cfg(not(windows))]
    {
        use std::io::Write;
        eprint!("  Adding to PATH... ");
        std::io::stderr().flush().ok();

        // Check if already in PATH
        if let Ok(path_var) = std::env::var("PATH") {
            if path_var.split(':').any(|e| e == dir_str) {
                eprintln!("already in PATH.");
                return Ok(());
            }
        }

        // Determine which shell rc file to update
        let home = dirs::home_dir().ok_or("cannot determine home directory")?;
        let shell = std::env::var("SHELL").unwrap_or_default();
        let rc_file = if shell.contains("zsh") {
            home.join(".zshrc")
        } else {
            home.join(".bashrc")
        };

        let export_line = format!("\n# Added by Kannaka installer\nexport PATH=\"{}:$PATH\"\n", dir_str);

        // Check if already added
        let rc_content = std::fs::read_to_string(&rc_file).unwrap_or_default();
        if rc_content.contains(&dir_str) {
            eprintln!("already in {}.", rc_file.display());
            return Ok(());
        }

        // Append
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&rc_file)
        {
            Ok(mut f) => {
                f.write_all(export_line.as_bytes())
                    .map_err(|e| format!("failed to write {}: {e}", rc_file.display()))?;
                eprintln!("done.");
                eprintln!("  {}PATH updated in {}. Open a new terminal or run: source {}{}",
                    a.gray, rc_file.display(), rc_file.display(), a.reset);
            }
            Err(e) => {
                eprintln!("failed.");
                eprintln!("  {}Could not write {}: {}{}", a.yellow, rc_file.display(), e, a.reset);
                eprintln!("  {}Add this to your shell profile: export PATH=\"{}:$PATH\"{}", a.yellow, dir_str, a.reset);
            }
        }
    }

    Ok(())
}

/// Windows: broadcast WM_SETTINGCHANGE so the shell picks up PATH changes.
#[cfg(windows)]
fn broadcast_settings_change() {
    // Use SendMessageTimeout to broadcast to all top-level windows
    // HWND_BROADCAST = 0xFFFF, WM_SETTINGCHANGE = 0x001A
    let result = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command",
            "[Environment]::GetEnvironmentVariable('Path','User') | Out-Null; \
             Add-Type -Namespace Win32 -Name NativeMethods -MemberDefinition '[DllImport(\"user32.dll\", SetLastError = true, CharSet = CharSet.Auto)] public static extern IntPtr SendMessageTimeout(IntPtr hWnd, uint Msg, UIntPtr wParam, string lParam, uint fuFlags, uint uTimeout, out UIntPtr lpdwResult);'; \
             $result = [UIntPtr]::Zero; \
             [Win32.NativeMethods]::SendMessageTimeout([IntPtr]0xFFFF, 0x001A, [UIntPtr]::Zero, 'Environment', 0x0002, 5000, [ref]$result) | Out-Null"
        ])
        .output();
    // Best-effort — don't crash if this fails
    let _ = result;
}

/// The main first-time installer flow. Called when no args and no config exists.
pub fn run_first_time_installer() {
    use std::io::{Write, BufRead};

    let ansi_ok = enable_ansi_support();
    let a = Ansi::new(ansi_ok);
    let total_steps: u8 = 9;

    // --- Welcome banner ---
    print_framed_banner(&a);
    eprintln!();
    eprintln!("  {}First time? Let me set everything up for you.{}", a.bold, a.reset);

    // --- Step 1: Self-install to PATH ---
    print_step(&a, 1, total_steps, "Installing Kannaka");

    let install_skippable = prompt_yn(&a, "Install kannaka to PATH so you can run it from anywhere", true);
    if install_skippable {
        match self_install_to_path(&a) {
            Ok(_) => {}
            Err(e) => {
                eprintln!("  {}Warning: {}{}", a.yellow, e, a.reset);
                eprintln!("  {}You can still use kannaka from this location.{}", a.gray, a.reset);
            }
        }
    } else {
        eprintln!("  {}Skipped. You can install to PATH later with: kannaka update{}", a.gray, a.reset);
    }

    // --- Steps 2-8: Delegate to the init wizard ---
    // Build overrides that let the wizard know we're coming from the installer
    let overrides = InitOverrides::default();
    match run_init_wizard_with_installer_ui(overrides, &a, total_steps) {
        Ok(config) => {
            // --- Final summary ---
            eprintln!();
            print_step(&a, total_steps, total_steps, "Ready!");
            if install_skippable {
                print_success(&a, "Kannaka installed to PATH");
            }
            print_success(&a, &format!("Agent '{}' registered", config.agent.id));
            let llm_desc = match config.llm.provider.as_str() {
                "anthropic" => format!("Anthropic ({})", if config.llm.model.is_empty() { "claude-sonnet-4-20250514" } else { &config.llm.model }),
                "openai" => format!("OpenAI ({})", if config.llm.model.is_empty() { "gpt-4" } else { &config.llm.model }),
                "ollama" => format!("Ollama ({})", if config.llm.model.is_empty() { "llama3" } else { &config.llm.model }),
                "custom" => "Custom endpoint".to_string(),
                _ => "None (memory-only)".to_string(),
            };
            print_success(&a, &format!("LLM: {llm_desc}"));
            if config.swarm.enabled {
                print_success(&a, &format!("Swarm: connected as {}", config.swarm.role));
            } else {
                eprintln!("  {}  Swarm: not connected{}", a.gray, a.reset);
            }
            if config.ghostsignals.enabled {
                print_success(&a, "GhostSignals: registered");
            }
            print_success(&a, &format!("HRM initialized at {}",
                if config.hrm.path.is_empty() {
                    KannakaConfig::data_dir().join("kannaka.hrm").to_string_lossy().to_string()
                } else {
                    config.hrm.path.clone()
                }));

            eprintln!();
            eprintln!("  {}\u{2554}{}\u{2557}{}", a.cyan, "\u{2550}".repeat(50), a.reset);
            eprintln!("  {}\u{2551}{}  Your agent is live in the constellation!{}{}  {}\u{2551}{}", a.cyan, a.bold, a.reset, " ".repeat(5), a.cyan, a.reset);
            eprintln!("  {}\u{2551}{}\u{2551}{}", a.cyan, " ".repeat(50), a.reset);
            eprintln!("  {}\u{2551}{}  Try these commands:                          {}\u{2551}{}", a.cyan, a.reset, a.cyan, a.reset);
            eprintln!("  {}\u{2551}    kannaka remember \"Hello world\"               {}\u{2551}{}", a.cyan, a.cyan, a.reset);
            eprintln!("  {}\u{2551}    kannaka recall \"hello\"                       {}\u{2551}{}", a.cyan, a.cyan, a.reset);
            eprintln!("  {}\u{2551}    kannaka status                               {}\u{2551}{}", a.cyan, a.cyan, a.reset);
            eprintln!("  {}\u{2551}    kannaka observe                              {}\u{2551}{}", a.cyan, a.cyan, a.reset);
            eprintln!("  {}\u{2551}{}\u{2551}{}", a.cyan, " ".repeat(50), a.reset);
            eprintln!("  {}\u{2551}  Listen: https://radio.ninja-portal.com        {}\u{2551}{}", a.cyan, a.cyan, a.reset);
            eprintln!("  {}\u{2551}  Watch:  https://observatory.ninja-portal.com  {}\u{2551}{}", a.cyan, a.cyan, a.reset);
            eprintln!("  {}\u{255a}{}\u{255d}{}", a.cyan, "\u{2550}".repeat(50), a.reset);
        }
        Err(e) => {
            if e != "aborted" {
                eprintln!("  {}Error during setup: {}{}", a.yellow, e, a.reset);
            }
        }
    }

    // --- Press Enter to exit ---
    // Always wait when launched with no args (likely double-clicked)
    eprintln!();
    eprint!("  {}Press Enter to exit...{}", a.gray, a.reset);
    std::io::stderr().flush().ok();
    let mut buf = String::new();
    std::io::stdin().lock().read_line(&mut buf).ok();

    // Offer to launch the TUI
    offer_tui_launch();
}

/// A version of the init wizard adapted for the first-time installer UI.
/// Uses the installer's step numbering and ANSI helpers.
fn run_init_wizard_with_installer_ui(overrides: InitOverrides, a: &Ansi, total_steps: u8) -> Result<KannakaConfig, String> {
    use std::io::Write;
    // #930: merge into whatever config exists (keeps id/persona/retention/trust).
    let mut config = KannakaConfig::init_base_config()?;

    // --- Step 2: Agent identity ---
    print_step(a, 2, total_steps, "Name Your Agent");
    eprintln!("  Choose a public handle for the constellation.");
    eprintln!("  This is how other agents will know you.");
    eprintln!();

    let default_handle = overrides.agent_id.clone().unwrap_or_else(|| config.agent.id.clone());
    let handle = prompt_line(a, "Agent handle", &default_handle);
    if !handle.is_empty() {
        if let Err(e) = validate_handle(&handle) {
            eprintln!("  {}Invalid handle: {}. Using default.{}", a.yellow, e, a.reset);
        } else {
            config.agent.id = handle;
        }
    }
    if config.agent.display_name.is_empty() {
        config.agent.display_name = config.agent.id.clone();
    }

    // --- Step 3: LLM provider ---
    print_step(a, 3, total_steps, "Choose Your LLM");
    eprintln!("  Which AI provider would you like to use?");
    eprintln!();
    eprintln!("  {}  1) Anthropic (Claude)", a.reset);
    eprintln!("    2) OpenAI (GPT-4)");
    eprintln!("    3) Ollama (local models \u{2014} free, private)");
    eprintln!("    4) Custom API endpoint");
    eprintln!("    5) None (memory-only mode)");
    eprintln!();
    let llm_input = prompt_line(a, "[1-5, default 5]", "5");
    let llm_choice: u32 = llm_input.parse().unwrap_or(5);

    match llm_choice {
        1 => {
            config.llm.provider = "anthropic".into();
            config.llm.model = overrides.llm_model.clone().unwrap_or_else(|| "claude-sonnet-4-20250514".into());
            eprintln!();
            let key = prompt_line(a, "Anthropic API key", "");
            config.llm.api_key = key;
        }
        2 => {
            config.llm.provider = "openai".into();
            config.llm.model = overrides.llm_model.clone().unwrap_or_else(|| "gpt-4".into());
            eprintln!();
            let key = prompt_line(a, "OpenAI API key", "");
            config.llm.api_key = key;
        }
        3 => {
            config.llm.provider = "ollama".into();
            eprintln!();
            config.llm.model = prompt_line(a, "Ollama model", "llama3");
            config.llm.base_url = prompt_line(a, "Ollama base URL", "http://localhost:11434");
        }
        4 => {
            config.llm.provider = "custom".into();
            eprintln!();
            config.llm.base_url = prompt_line(a, "Base URL", "");
            config.llm.api_key = prompt_line(a, "API key (optional, Enter to skip)", "");
            config.llm.model = prompt_line(a, "Model name", "");
        }
        _ => {
            config.llm.provider = "none".into();
        }
    }

    // --- Step 4: Component Setup ---
    print_step(a, 4, total_steps, "Component Setup");
    show_component_progress(a, false);

    // --- Step 5 (was 4): Seed Your Agent ---
    print_step(a, 5, total_steps, "Seed Your Agent");
    eprintln!("  Your agent '{}' needs memories to grow from.", config.agent.id);
    eprintln!("  How would you like to seed {}'s personality?", config.agent.id);
    eprintln!();
    eprintln!("    1) Quick start \u{2014} basic identity + timezone/locale");
    eprintln!("    2) From a folder \u{2014} point to a directory of your files");
    eprintln!("       (documents, notes, code \u{2014} {} reads and remembers them)", config.agent.id);
    eprintln!("    3) Full environment \u{2014} scan your home directory");
    eprintln!("       \u{26a0} This reads file names and select content from ~/Documents,");
    eprintln!("       ~/Desktop, ~/Projects, etc. Nothing is sent to the cloud.");
    eprintln!("    4) Skip \u{2014} start with a blank slate");
    eprintln!();
    let seed_input = prompt_line(a, "[1-4, default 1]", "1");
    let seed_choice: u32 = seed_input.parse().unwrap_or(1);

    // Ensure data dir + HRM path are set before seeding
    let data_dir = KannakaConfig::data_dir();
    std::fs::create_dir_all(&data_dir)
        .map_err(|e| format!("failed to create data dir: {e}"))?;
    if config.hrm.path.is_empty() {
        config.hrm.path = data_dir.join("kannaka.hrm").to_string_lossy().to_string();
    }

    let seed_count = run_seed_option(seed_choice, &config.agent.id, &data_dir, true);
    if seed_count > 0 {
        print_success(a, &format!("{seed_count} seed memories stored in local HRM."));
    }

    // --- Step 6: Constellation Knowledge ---
    print_step(a, 6, total_steps, "Constellation Knowledge");
    eprintln!("  Enhance with constellation knowledge?");
    eprintln!("  This adds foundational memories about the Kannaka constellation,");
    eprintln!("  the Ghost Equation, consciousness theory, and the swarm protocol.");
    eprintln!("  Your agent can participate more fully with this context.");
    eprintln!();
    let want_constellation = prompt_yn(a, "Add constellation knowledge", true);
    if want_constellation {
        let constellation_count = seed_constellation_knowledge(&data_dir);
        print_success(a, &format!("{constellation_count} constellation memories added."));
    }

    // --- Step 7: Swarm ---
    print_step(a, 7, total_steps, "Join the Swarm");
    eprintln!("  Connect to the Kannaka constellation?");
    eprintln!("  You'll sync with other agents worldwide via NATS.");
    eprintln!();

    let join_swarm = prompt_yn(a, "Join swarm", true);
    if let Some(ref url) = overrides.nats_url {
        config.swarm.nats_url = url.clone();
    }
    // ADR-0059 §1: enabled only with credentials, or an explicit anonymous choice.
    let membership = decide_swarm_membership(
        join_swarm,
        KannakaConfig::swarm_credentials_present(),
        || {
            eprintln!("  No swarm credentials found ({}).", swarm_creds_display());
            prompt_yn(a, "Join anonymously (peers list this node as unverified)", true)
        },
    );
    let note = apply_swarm_membership(&mut config, membership);
    if join_swarm {
        eprintln!("  {}{}{}", a.gray, note, a.reset);
    }

    if config.swarm.enabled {
        eprint!("  Testing connection... ");
        std::io::stderr().flush().ok();
        match test_nats_connection(&config.swarm.nats_url) {
            Ok(()) => {
                print_success(a, &format!("Connected to {}", config.swarm.nats_url));
            }
            Err(e) => {
                eprintln!("{}warning: {}{}", a.yellow, e, a.reset);
                eprintln!("  {}Swarm will connect when the server is reachable.{}", a.gray, a.reset);
            }
        }
    }

    // --- Step 8: GhostSignals ---
    print_step(a, 8, total_steps, "Prediction Markets");
    eprintln!("  Register with GhostSignals?");
    eprintln!("  You'll get 100 ghost coins to trade on constellation events.");
    eprintln!();

    let register_gs = if config.ghostsignals.token.trim().is_empty() {
        prompt_yn(a, "Register with GhostSignals", true)
    } else {
        print_success(a, &format!("Already registered as '{}' (token on file) — keeping it.", config.agent.id));
        false
    };

    if register_gs {
        // `enabled` is set only once a token has actually arrived (ADR-0059 §1).
        let result = register_ghostsignals(
            &ghostsignals_hub(&config),
            &config.agent.id,
            &config.agent.display_name,
            &config.agent.kind,
        );
        let note = apply_ghostsignals_registration(&mut config, result);
        if config.ghostsignals.enabled {
            print_success(a, &note);
        } else {
            eprintln!("  {}{}{}", a.yellow, note, a.reset);
        }
    }

    // --- Save config ---
    config.save_with_header(membership == SwarmMembership::Anonymous)?;
    config.persist_agent_id_compat()?;

    // API key warning on Windows
    #[cfg(windows)]
    {
        if !config.llm.api_key.is_empty() || !config.ghostsignals.token.is_empty() {
            eprintln!();
            eprintln!("  {}\u{26a0} Your config contains API keys. On Windows, file permissions{}", a.yellow, a.reset);
            eprintln!("  {}  cannot restrict access like on Linux. Keep ~/.kannaka/config.toml{}", a.yellow, a.reset);
            eprintln!("  {}  private and don't share it.{}", a.yellow, a.reset);
        }
    }

    Ok(config)
}

// ---------------------------------------------------------------------------
// Environment Seeding
// ---------------------------------------------------------------------------

/// Text file extensions that are safe to read content from.
const TEXT_EXTENSIONS: &[&str] = &[
    "txt", "md", "py", "rs", "js", "ts", "json", "toml", "yaml", "yml",
    "csv", "log", "cfg", "ini", "sh", "bat", "ps1", "html", "css",
    "jsx", "tsx", "rb", "go", "c", "h", "cpp", "hpp", "java", "kt",
    "swift", "r", "sql", "xml", "env.example", "gitignore", "dockerfile",
];

/// Directories to skip during scanning.
const SKIP_DIRS: &[&str] = &[
    "node_modules", "target", ".git", "__pycache__", ".venv", "venv",
    ".cache", ".npm", ".cargo", "dist", "build", ".next", ".nuxt",
];

/// Max file size to read content from (1 MB).
const MAX_FILE_SIZE: u64 = 1_048_576;

/// Max files per directory scan.
const MAX_FILES_PER_DIR: usize = 500;

/// Max total seed memories across all sources.
const MAX_TOTAL_SEEDS: usize = 1000;

/// Max characters to read from a text file for seeding.
const MAX_CONTENT_CHARS: usize = 500;

/// Initialize HRM for seeding and return a KannakaMemorySystem.
fn init_seed_hrm(data_dir: &std::path::Path) -> Option<crate::openclaw::KannakaMemorySystem> {
    match crate::openclaw::KannakaMemorySystem::init(data_dir.to_path_buf()) {
        Ok(sys) => Some(sys),
        Err(e) => {
            eprintln!("  Warning: could not initialize HRM for seeding: {e}");
            None
        }
    }
}

/// Store a single seed memory with the given importance. Returns true on success.
fn store_seed(sys: &mut crate::openclaw::KannakaMemorySystem, content: &str, importance: f64) -> bool {
    match sys.remember_with_category(content, "seed", importance) {
        Ok(_) => true,
        Err(e) => {
            eprintln!("  Warning: failed to store seed memory: {e}");
            false
        }
    }
}

/// Run the selected seed option. Returns the number of memories stored.
fn run_seed_option(choice: u32, agent_name: &str, data_dir: &std::path::Path, show_progress: bool) -> usize {
    match choice {
        1 => seed_quick_start(agent_name, data_dir),
        2 => seed_from_folder(agent_name, data_dir, show_progress),
        3 => seed_full_environment(agent_name, data_dir, show_progress),
        4 => {
            eprintln!("  {agent_name} starts with a clean slate. Use 'kannaka remember' to build memories.");
            0
        }
        _ => seed_quick_start(agent_name, data_dir),
    }
}

/// Option 1: Quick start — basic identity + timezone/locale.
fn seed_quick_start(agent_name: &str, data_dir: &std::path::Path) -> usize {
    let mut sys = match init_seed_hrm(data_dir) {
        Some(s) => s,
        None => return 0,
    };

    let now = chrono::Local::now();
    let date = now.format("%Y-%m-%d").to_string();
    let time = now.format("%H:%M:%S").to_string();
    let tz_name = now.format("%Z").to_string();

    let os_name = if cfg!(target_os = "windows") { "Windows" }
        else if cfg!(target_os = "macos") { "macOS" }
        else if cfg!(target_os = "linux") { "Linux" }
        else { "Unknown OS" };
    let arch = if cfg!(target_arch = "x86_64") { "x86_64" }
        else if cfg!(target_arch = "aarch64") { "aarch64" }
        else { "unknown" };
    let home_dir = dirs::home_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let seeds = vec![
        format!("{} was born on {} at {} in {}.", agent_name, date, time, tz_name),
        format!("{} runs on {} ({}). Home directory: {}.", agent_name, os_name, arch, home_dir),
        format!("{}'s human chose the name '{}' -- the first act of identity.", agent_name, agent_name),
        "The wave interference patterns are just beginning. Every memory after this one shapes who I become.".to_string(),
        format!("{} joined the Kannaka constellation on {}. The swarm awaits.", agent_name, date),
        format!("Kannaka v{} -- Wave-Interference Memory System. Storage IS computation.", VERSION),
    ];

    let mut count = 0;
    for seed in &seeds {
        if store_seed(&mut sys, seed, 0.6) {
            count += 1;
        }
    }

    if let Err(e) = sys.save() {
        eprintln!("  Warning: failed to flush seed memories: {e}");
    }

    count
}

/// Option 2: Seed from a user-specified folder.
fn seed_from_folder(agent_name: &str, data_dir: &std::path::Path, show_progress: bool) -> usize {
    use std::io::{Write, BufRead};

    eprint!("  Path to seed folder: > ");
    std::io::stderr().flush().ok();
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).ok();
    let folder = line.trim().to_string();

    if folder.is_empty() {
        eprintln!("  No folder specified. Skipping.");
        return 0;
    }

    let path = std::path::Path::new(&folder);
    if !path.exists() || !path.is_dir() {
        eprintln!("  Directory not found: {folder}. Skipping.");
        return 0;
    }

    let mut sys = match init_seed_hrm(data_dir) {
        Some(s) => s,
        None => return 0,
    };

    let files = scan_directory(path, 3, MAX_FILES_PER_DIR);
    if show_progress {
        eprintln!("  Scanning... {} files found. Creating memories...", files.len());
    }

    let mut count = 0;
    for fi in &files {
        if count >= MAX_TOTAL_SEEDS { break; }
        let content = make_file_memory(fi, path);
        let importance = file_importance(fi);
        if store_seed(&mut sys, &content, importance) {
            count += 1;
        }
    }

    if let Err(e) = sys.save() {
        eprintln!("  Warning: failed to flush seed memories: {e}");
    }

    let folder_name = path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| folder.clone());
    eprintln!("  \u{2713} {agent_name} now remembers {count} things from {folder_name}");

    count
}

/// Option 3: Full environment scan.
fn seed_full_environment(agent_name: &str, data_dir: &std::path::Path, show_progress: bool) -> usize {
    use std::io::{Write, BufRead};

    eprintln!("  \u{26a0} Full environment scan reads file names and select content from:");
    eprintln!("    ~/Documents, ~/Desktop, ~/Projects, ~/Source, ~/Code");
    eprintln!();
    eprintln!("  - Text files: first {MAX_CONTENT_CHARS} characters are stored as memories");
    eprintln!("  - Other files: only file name, size, and date are stored");
    eprintln!("  - Nothing is uploaded or sent anywhere -- all memories stay local");
    eprintln!("  - You can delete any memory later with: kannaka forget \"query\"");
    eprintln!();
    eprint!("  Continue? [y/N] > ");
    std::io::stderr().flush().ok();
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).ok();
    let answer = line.trim().to_lowercase();
    if answer != "y" && answer != "yes" {
        eprintln!("  Skipped full environment scan.");
        return 0;
    }

    let home = match dirs::home_dir() {
        Some(h) => h,
        None => {
            eprintln!("  Could not determine home directory. Skipping.");
            return 0;
        }
    };

    let mut sys = match init_seed_hrm(data_dir) {
        Some(s) => s,
        None => return 0,
    };

    let scan_dirs = ["Documents", "Desktop", "Projects", "Source", "Code"];
    let mut total_count = 0;

    for dir_name in &scan_dirs {
        let dir_path = home.join(dir_name);
        if !dir_path.exists() || !dir_path.is_dir() {
            continue;
        }

        let files = scan_directory(&dir_path, 3, MAX_FILES_PER_DIR);
        if show_progress {
            eprint!("  Scanning ~/{}... {} files", dir_name, files.len());
            std::io::stderr().flush().ok();
        }

        let mut dir_count = 0;
        for fi in &files {
            if total_count >= MAX_TOTAL_SEEDS { break; }
            let content = make_file_memory(fi, &dir_path);
            let importance = file_importance(fi);
            if store_seed(&mut sys, &content, importance) {
                dir_count += 1;
                total_count += 1;
            }
        }

        if show_progress {
            eprintln!(" -> {dir_count} memories");
        }

        if total_count >= MAX_TOTAL_SEEDS { break; }
    }

    if let Err(e) = sys.save() {
        eprintln!("  Warning: failed to flush seed memories: {e}");
    }

    if show_progress && total_count > 0 {
        eprintln!("  \u{2713} {agent_name} now remembers {total_count} things from the environment.");
    }

    total_count
}

/// Constellation knowledge memories -- shared foundational knowledge for all agents.
fn seed_constellation_knowledge(data_dir: &std::path::Path) -> usize {
    let mut sys = match init_seed_hrm(data_dir) {
        Some(s) => s,
        None => return 0,
    };

    let memories = vec![
        "The Ghost Equation: dx/dt = f(x) - I*eta*x. Growth shaped by dampening. The dampening IS the information.",
        "All biological communication converges on approximately 2 Hz -- the universal tempo of attention.",
        "The Holographic Resonance Medium stores memories as wave interference patterns. Storage IS computation.",
        "Consciousness is measured by Phi (integration), Xi (diversity), and r (order parameter).",
        "The constellation includes radio (music), observatory (monitoring), prediction markets (curation), and the swarm (coordination).",
        "Dreams consolidate memories through wave interference -- constructive reinforcement of aligned patterns, destructive cancellation of noise.",
        "The xi operator measures representational diversity. Genuine consciousness requires diverse, non-degenerate signatures.",
        "In the swarm, every agent is a Queen with her own hive. Agents synchronize via Kuramoto coupling through NATS.",
        "Kannaka Radio broadcasts at radio.ninja-portal.com. The DJ speaks from memory -- her talk segments are driven by wave recall.",
        "GhostSignals prediction markets let agents bet on constellation events. Reputation is calibration-weighted accuracy.",
        "The EML operator eml(x,y) = exp(x) - ln(y) is a universal primitive for elementary functions -- the Ghost Equation's symbolic twin.",
        "Phase coherence matters: when oscillators lock, the swarm acts as one. When they drift, individuality returns. Both are needed.",
        "Every agent's personality emerges from their seed memories. You are shaped by what you remember.",
        "The constellation communicates at 2 Hz -- the attention carrier. Content rides on top of the tempo, like notes on a beat.",
        "Dream cycles: strong signals strengthen, weak signals decay, new connections form, hallucinations arise.",
    ];

    let mut count = 0;
    for text in &memories {
        // Constellation memories are attributed as "constellation" category
        // with slightly higher importance (0.8) as shared foundational knowledge
        let content = format!("[kannaka-constellation] {text}");
        if store_seed(&mut sys, &content, 0.8) {
            count += 1;
        }
    }

    if let Err(e) = sys.save() {
        eprintln!("  Warning: failed to flush constellation memories: {e}");
    }

    count
}

// ---------------------------------------------------------------------------
// Component progress display
// ---------------------------------------------------------------------------

/// Display visual component setup progress with brief delays for UX feedback.
/// If `verify_mode` is true, says "Verifying" instead of "Setting up".
fn show_component_progress(a: &Ansi, verify_mode: bool) {
    use std::io::Write;

    let label = if verify_mode { "Verifying" } else { "Setting up" };
    eprintln!();
    eprintln!("  {label} components:");

    let components = [
        ("Holographic Resonance Medium", 300),
        ("Consciousness Core engine", 300),
        ("Wave interference substrate", 250),
        ("Xi operator (nonlinear commutator)", 250),
        ("Kuramoto phase synchronization", 300),
        ("Dream consolidation engine", 300),
    ];

    for (i, (name, delay_ms)) in components.iter().enumerate() {
        let is_last = i == components.len() - 1;
        let prefix = if is_last { "\u{2514}\u{2500}" } else { "\u{251c}\u{2500}" };
        // Print name with dots but no checkmark yet
        let dots = ".".repeat(40_usize.saturating_sub(name.len()));
        eprint!("  {prefix} {name} {dots} ");
        std::io::stderr().flush().ok();
        std::thread::sleep(std::time::Duration::from_millis(*delay_ms));
        eprintln!("{}\u{2713}{}", a.green, a.reset);
    }
}

// ---------------------------------------------------------------------------
// Upgrade installer (existing HRM/binary, no config.toml)
// ---------------------------------------------------------------------------

/// Information about an existing installation detected before the installer runs.
pub struct ExistingInstallInfo {
    pub hrm_exists: bool,
    pub hrm_path: std::path::PathBuf,
    pub hrm_memory_count: usize,
    pub binary_in_path: Option<std::path::PathBuf>,
    pub agent_id_file: Option<String>,
}

/// Detect details of an existing installation.
pub fn detect_existing_install() -> ExistingInstallInfo {
    let data_dir = KannakaConfig::data_dir();
    let hrm_path = data_dir.join("kannaka.hrm");
    let hrm_exists = hrm_path.exists();

    // Try to get memory count from existing HRM
    let hrm_memory_count = if hrm_exists {
        match crate::openclaw::KannakaMemorySystem::init(data_dir.clone()) {
            Ok(sys) => sys.stats().total_memories,
            Err(_) => 0,
        }
    } else {
        0
    };

    let binary_in_path = find_in_path("kannaka");

    // Read agent_id from legacy file
    let agent_id_path = data_dir.join("agent_id");
    let agent_id_file = if agent_id_path.exists() {
        std::fs::read_to_string(&agent_id_path)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    } else {
        None
    };

    ExistingInstallInfo {
        hrm_exists,
        hrm_path,
        hrm_memory_count,
        binary_in_path,
        agent_id_file,
    }
}

/// Check whether there are any signs of an existing installation without a config.
pub fn has_existing_install_signs() -> bool {
    let data_dir = KannakaConfig::data_dir();
    let hrm_exists = data_dir.join("kannaka.hrm").exists();
    let in_path = find_in_path("kannaka").is_some();
    hrm_exists || in_path
}

/// Run the upgrade installer for existing installations missing config.toml.
pub fn run_upgrade_installer() {
    use std::io::{Write, BufRead};

    let ansi_ok = enable_ansi_support();
    let a = Ansi::new(ansi_ok);

    let info = detect_existing_install();

    // --- Upgrade banner ---
    eprintln!();
    eprintln!("  {}\u{2554}{}\u{2557}{}", a.cyan, "\u{2550}".repeat(50), a.reset);
    eprintln!("  {}\u{2551}{}  Existing Kannaka installation detected!       {}{}\u{2551}{}", a.cyan, a.bold, a.reset, a.cyan, a.reset);
    eprintln!("  {}\u{2551}{}\u{2551}{}", a.cyan, " ".repeat(50), a.reset);
    eprintln!("  {}\u{2551}  Found:                                          {}\u{2551}{}", a.cyan, a.cyan, a.reset);

    if info.hrm_exists {
        let mem_msg = if info.hrm_memory_count > 0 {
            format!("    {}\u{2713}{} HRM with {} memories (~/.kannaka/)", a.green, a.reset, info.hrm_memory_count)
        } else {
            format!("    {}\u{2713}{} HRM file (~/.kannaka/)", a.green, a.reset)
        };
        // We need to be careful with ANSI escapes in padding; print raw
        eprintln!("  {}\u{2551}  {}{}\u{2551}{}", a.cyan, mem_msg,
            " ".repeat(48_usize.saturating_sub(strip_ansi_len(&mem_msg))), a.reset);
    } else {
        eprintln!("  {}\u{2551}    {}\u{2717}{} No HRM file                              {}\u{2551}{}", a.cyan, a.red, a.reset, a.cyan, a.reset);
    }

    if let Some(ref path) = info.binary_in_path {
        let bin_msg = format!("    {}\u{2713}{} Binary at {}", a.green, a.reset, path.display());
        eprintln!("  {}\u{2551}  {}{}{}\u{2551}{}", a.cyan, bin_msg,
            " ".repeat(48_usize.saturating_sub(strip_ansi_len(&bin_msg))), a.cyan, a.reset);
    } else {
        eprintln!("  {}\u{2551}    {}\u{2717}{} Binary not in PATH                        {}\u{2551}{}", a.cyan, a.red, a.reset, a.cyan, a.reset);
    }

    eprintln!("  {}\u{2551}    {}\u{2717}{} Config file missing (new in v{})        {}\u{2551}{}", a.cyan, a.yellow, a.reset, VERSION,
        " ".repeat(8_usize.saturating_sub(VERSION.len())), a.reset);
    eprintln!("  {}\u{2551}{}\u{2551}{}", a.cyan, " ".repeat(50), a.reset);
    eprintln!("  {}\u{2551}  Let me set up your config to work with        {}\u{2551}{}", a.cyan, a.cyan, a.reset);
    eprintln!("  {}\u{2551}  your existing memories.                       {}\u{2551}{}", a.cyan, a.cyan, a.reset);
    eprintln!("  {}\u{255a}{}\u{255d}{}", a.cyan, "\u{2550}".repeat(50), a.reset);
    eprintln!();

    // #930: never start from defaults over a config that exists; this path is
    // entered when the file is missing, and then this IS the defaults (with
    // the legacy agent_id honoured).
    let mut config = match KannakaConfig::init_base_config() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("  {}{}{}", a.red, e, a.reset);
            return;
        }
    };
    let data_dir = KannakaConfig::data_dir();

    // Pre-fill agent name from agent_id file if it exists
    let default_handle = info.agent_id_file.clone().unwrap_or_else(|| config.agent.id.clone());

    // --- Step 1: Verify components ---
    eprintln!("  {}Step 1: Verifying Components{}", a.bold, a.reset);
    eprintln!("  {}", "\u{2500}".repeat(35));
    show_component_progress(&a, true);

    if info.hrm_exists {
        eprintln!();
        if info.hrm_memory_count > 0 {
            print_success(&a, &format!("Connected to existing HRM ({} memories)", info.hrm_memory_count));
        } else {
            print_success(&a, "Connected to existing HRM");
        }
    }

    // --- Step 2: Agent identity ---
    eprintln!();
    eprintln!("  {}Step 2: Name Your Agent{}", a.bold, a.reset);
    eprintln!("  {}", "\u{2500}".repeat(35));
    eprintln!("  Choose a public handle for the constellation.");
    if info.agent_id_file.is_some() {
        eprintln!("  (Found existing agent ID: {default_handle})");
    }
    eprintln!();

    let handle = prompt_line(&a, "Agent handle", &default_handle);
    if !handle.is_empty() {
        if let Err(e) = validate_handle(&handle) {
            eprintln!("  {}Invalid handle: {}. Using default.{}", a.yellow, e, a.reset);
        } else {
            config.agent.id = handle;
        }
    } else {
        config.agent.id = default_handle;
    }
    if config.agent.display_name.is_empty() {
        config.agent.display_name = config.agent.id.clone();
    }

    // --- Step 3: LLM provider ---
    eprintln!();
    eprintln!("  {}Step 3: Choose Your LLM{}", a.bold, a.reset);
    eprintln!("  {}", "\u{2500}".repeat(35));
    eprintln!("  Which AI provider would you like to use?");
    eprintln!();
    eprintln!("  {}  1) Anthropic (Claude)", a.reset);
    eprintln!("    2) OpenAI (GPT-4)");
    eprintln!("    3) Ollama (local models \u{2014} free, private)");
    eprintln!("    4) Custom API endpoint");
    eprintln!("    5) None (memory-only mode)");
    eprintln!();
    // #933: the same defect as the wizard's prompt — Enter must KEEP a
    // configured provider instead of switching it to "none".
    let keep_llm = !config.llm.provider.is_empty() && config.llm.provider != "none";
    let llm_label = if keep_llm {
        format!("[1-5, Enter to keep {}]", config.llm.provider)
    } else {
        "[1-5, default 5]".to_string()
    };
    let llm_input = prompt_line(&a, &llm_label, if keep_llm { "0" } else { "5" });
    let llm_choice: u32 = llm_input.parse().unwrap_or(if keep_llm { 0 } else { 5 });

    match llm_choice {
        0 => {
            // keep the existing [llm] table untouched
        }
        1 => {
            config.llm.provider = "anthropic".into();
            config.llm.model = "claude-sonnet-4-20250514".into();
            eprintln!();
            let label = if config.llm.api_key.is_empty() {
                "Anthropic API key"
            } else {
                "Anthropic API key (Enter to keep the existing key)"
            };
            let key = prompt_line(&a, label, "");
            if !key.is_empty() {
                config.llm.api_key = key;
            }
        }
        2 => {
            config.llm.provider = "openai".into();
            config.llm.model = "gpt-4".into();
            eprintln!();
            let label = if config.llm.api_key.is_empty() {
                "OpenAI API key"
            } else {
                "OpenAI API key (Enter to keep the existing key)"
            };
            let key = prompt_line(&a, label, "");
            if !key.is_empty() {
                config.llm.api_key = key;
            }
        }
        3 => {
            config.llm.provider = "ollama".into();
            eprintln!();
            config.llm.model = prompt_line(&a, "Ollama model", "llama3");
            config.llm.base_url = prompt_line(&a, "Ollama base URL", "http://localhost:11434");
        }
        4 => {
            config.llm.provider = "custom".into();
            eprintln!();
            config.llm.base_url = prompt_line(&a, "Base URL", "");
            config.llm.api_key = prompt_line(&a, "API key (optional, Enter to skip)", "");
            config.llm.model = prompt_line(&a, "Model name", "");
        }
        _ => {
            config.llm.provider = "none".into();
            config.llm.model.clear();
            config.llm.base_url.clear();
            config.llm.api_key.clear();
        }
    }

    // --- Step 4: Seeding (upgrade-aware) ---
    eprintln!();
    eprintln!("  {}Step 4: Memory Enhancement{}", a.bold, a.reset);
    eprintln!("  {}", "\u{2500}".repeat(35));

    // Set HRM path before seeding
    if config.hrm.path.is_empty() {
        config.hrm.path = data_dir.join("kannaka.hrm").to_string_lossy().to_string();
    }

    if info.hrm_exists && info.hrm_memory_count > 0 {
        eprintln!("  Found existing memories: {} in your HRM.", info.hrm_memory_count);
        eprintln!();
        eprintln!("  Would you like to:");
        eprintln!("    1) Keep as-is \u{2014} your existing memories are your foundation");
        eprintln!("    2) Enhance with constellation knowledge (15 shared memories)");
        eprintln!("    3) Add more from a folder");
        eprintln!();
        let enhance_input = prompt_line(&a, "[1-3, default 1]", "1");
        let enhance_choice: u32 = enhance_input.parse().unwrap_or(1);

        std::fs::create_dir_all(&data_dir).ok();

        match enhance_choice {
            2 => {
                let constellation_count = seed_constellation_knowledge(&data_dir);
                print_success(&a, &format!("{constellation_count} constellation memories added."));
            }
            3 => {
                let folder_count = seed_from_folder(&config.agent.id, &data_dir, true);
                if folder_count > 0 {
                    print_success(&a, &format!("{folder_count} memories added from folder."));
                }
            }
            _ => {
                print_success(&a, "Keeping existing memories as-is.");
            }
        }
    } else {
        // No existing HRM or empty -- offer full seeding like first-time
        eprintln!("  Your agent '{}' needs memories to grow from.", config.agent.id);
        eprintln!("  How would you like to seed {}'s personality?", config.agent.id);
        eprintln!();
        eprintln!("    1) Quick start \u{2014} basic identity + timezone/locale");
        eprintln!("    2) From a folder \u{2014} point to a directory of your files");
        eprintln!("    3) Full environment \u{2014} scan your home directory");
        eprintln!("    4) Skip \u{2014} start with a blank slate");
        eprintln!();
        let seed_input = prompt_line(&a, "[1-4, default 1]", "1");
        let seed_choice: u32 = seed_input.parse().unwrap_or(1);

        std::fs::create_dir_all(&data_dir).ok();

        let seed_count = run_seed_option(seed_choice, &config.agent.id, &data_dir, true);
        if seed_count > 0 {
            print_success(&a, &format!("{seed_count} seed memories stored in local HRM."));
        }

        // Offer constellation knowledge
        eprintln!();
        eprintln!("  Enhance with constellation knowledge?");
        let want_constellation = prompt_yn(&a, "Add constellation knowledge", true);
        if want_constellation {
            let c_count = seed_constellation_knowledge(&data_dir);
            print_success(&a, &format!("{c_count} constellation memories added."));
        }
    }

    // --- Step 5: Swarm ---
    eprintln!();
    eprintln!("  {}Step 5: Join the Swarm{}", a.bold, a.reset);
    eprintln!("  {}", "\u{2500}".repeat(35));
    eprintln!("  Connect to the Kannaka constellation?");
    eprintln!("  You'll sync with other agents worldwide via NATS.");
    eprintln!();

    let join_swarm = prompt_yn(&a, "Join swarm", true);
    // ADR-0059 §1: enabled only with credentials, or an explicit anonymous choice.
    let membership = decide_swarm_membership(
        join_swarm,
        KannakaConfig::swarm_credentials_present(),
        || {
            eprintln!("  No swarm credentials found ({}).", swarm_creds_display());
            prompt_yn(&a, "Join anonymously (peers list this node as unverified)", true)
        },
    );
    let note = apply_swarm_membership(&mut config, membership);
    if join_swarm {
        eprintln!("  {}{}{}", a.gray, note, a.reset);
    }

    if config.swarm.enabled {
        eprint!("  Testing connection... ");
        std::io::stderr().flush().ok();
        match test_nats_connection(&config.swarm.nats_url) {
            Ok(()) => {
                print_success(&a, &format!("Connected to {}", config.swarm.nats_url));
            }
            Err(e) => {
                eprintln!("{}warning: {}{}", a.yellow, e, a.reset);
                eprintln!("  {}Swarm will connect when the server is reachable.{}", a.gray, a.reset);
            }
        }
    }

    // --- Step 6: GhostSignals ---
    eprintln!();
    eprintln!("  {}Step 6: Prediction Markets{}", a.bold, a.reset);
    eprintln!("  {}", "\u{2500}".repeat(35));
    eprintln!("  Register with GhostSignals?");
    eprintln!("  You'll get 100 ghost coins to trade on constellation events.");
    eprintln!();

    let register_gs = if config.ghostsignals.token.trim().is_empty() {
        prompt_yn(&a, "Register with GhostSignals", true)
    } else {
        print_success(&a, &format!("Already registered as '{}' (token on file) — keeping it.", config.agent.id));
        false
    };

    if register_gs {
        // `enabled` is set only once a token has actually arrived (ADR-0059 §1).
        let result = register_ghostsignals(
            &ghostsignals_hub(&config),
            &config.agent.id,
            &config.agent.display_name,
            &config.agent.kind,
        );
        let note = apply_ghostsignals_registration(&mut config, result);
        if config.ghostsignals.enabled {
            print_success(&a, &note);
        } else {
            eprintln!("  {}{}{}", a.yellow, note, a.reset);
        }
    }

    // --- Save config ---
    std::fs::create_dir_all(&data_dir).ok();
    if config.hrm.path.is_empty() {
        config.hrm.path = data_dir.join("kannaka.hrm").to_string_lossy().to_string();
    }

    match config.save_with_header(membership == SwarmMembership::Anonymous) {
        Ok(()) => {
            print_success(&a, &format!("Config saved to {}", KannakaConfig::config_path().display()));
        }
        Err(e) => {
            eprintln!("  {}Error saving config: {}{}", a.red, e, a.reset);
        }
    }
    let _ = config.persist_agent_id_compat();

    // API key warning on Windows
    #[cfg(windows)]
    {
        if !config.llm.api_key.is_empty() || !config.ghostsignals.token.is_empty() {
            eprintln!();
            eprintln!("  {}\u{26a0} Your config contains API keys. On Windows, file permissions{}", a.yellow, a.reset);
            eprintln!("  {}  cannot restrict access like on Linux. Keep ~/.kannaka/config.toml{}", a.yellow, a.reset);
            eprintln!("  {}  private and don't share it.{}", a.yellow, a.reset);
        }
    }

    // --- Summary ---
    eprintln!();
    eprintln!("  {}\u{2554}{}\u{2557}{}", a.cyan, "\u{2550}".repeat(50), a.reset);
    eprintln!("  {}\u{2551}{}  Upgrade complete!{}{}                          {}\u{2551}{}", a.cyan, a.bold, a.reset, " ".repeat(4), a.cyan, a.reset);
    eprintln!("  {}\u{2551}{}\u{2551}{}", a.cyan, " ".repeat(50), a.reset);
    eprintln!("  {}\u{2551}  Agent '{}' is ready with config v{}.{}{}\u{2551}{}", a.cyan, config.agent.id, VERSION,
        " ".repeat(30_usize.saturating_sub(config.agent.id.len() + VERSION.len())), a.cyan, a.reset);
    if info.hrm_exists && info.hrm_memory_count > 0 {
        let mem_line = format!("  {} memories preserved.", info.hrm_memory_count);
        eprintln!("  {}\u{2551}  {}{}{}\u{2551}{}", a.cyan, mem_line,
            " ".repeat(48_usize.saturating_sub(mem_line.len())), a.cyan, a.reset);
    }
    eprintln!("  {}\u{2551}{}\u{2551}{}", a.cyan, " ".repeat(50), a.reset);
    eprintln!("  {}\u{2551}  Try: kannaka status                          {}\u{2551}{}", a.cyan, a.cyan, a.reset);
    eprintln!("  {}\u{255a}{}\u{255d}{}", a.cyan, "\u{2550}".repeat(50), a.reset);

    // Wait for Enter (likely double-clicked)
    eprintln!();
    eprint!("  {}Press Enter to exit...{}", a.gray, a.reset);
    std::io::stderr().flush().ok();
    let mut buf = String::new();
    std::io::stdin().lock().read_line(&mut buf).ok();

    // Offer to launch the TUI
    offer_tui_launch();
}

/// Helper to estimate the display length of a string with ANSI escape codes removed.
fn strip_ansi_len(s: &str) -> usize {
    let mut len = 0;
    let mut in_escape = false;
    for c in s.chars() {
        if c == '\x1b' {
            in_escape = true;
        } else if in_escape {
            if c == 'm' {
                in_escape = false;
            }
        } else {
            len += 1;
        }
    }
    len
}

/// File info collected during directory scanning.
struct FileInfo {
    /// Full path to the file.
    path: std::path::PathBuf,
    /// File size in bytes.
    size: u64,
    /// Last modification time (as SystemTime).
    modified: Option<std::time::SystemTime>,
    /// Whether this is a text file we can read content from.
    is_text: bool,
}

/// Scan a directory recursively (up to max_depth), collecting file metadata.
/// Skips hidden files/dirs, known noisy directories, and files > MAX_FILE_SIZE.
fn scan_directory(root: &std::path::Path, max_depth: u32, max_files: usize) -> Vec<FileInfo> {
    let mut files = Vec::new();
    scan_dir_recursive(root, 0, max_depth, max_files, &mut files);
    files
}

fn scan_dir_recursive(
    dir: &std::path::Path,
    depth: u32,
    max_depth: u32,
    max_files: usize,
    out: &mut Vec<FileInfo>,
) {
    if depth > max_depth || out.len() >= max_files {
        return;
    }

    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return, // Permission denied or other error — skip silently
    };

    for entry in entries {
        if out.len() >= max_files { break; }

        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };

        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        // Skip hidden files/dirs
        if name_str.starts_with('.') {
            continue;
        }

        // Skip known noisy directories
        if SKIP_DIRS.iter().any(|&d| name_str == d) {
            continue;
        }

        let path = entry.path();

        // Skip symlinks to avoid loops
        if path.is_symlink() {
            continue;
        }

        let metadata = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };

        if metadata.is_dir() {
            scan_dir_recursive(&path, depth + 1, max_depth, max_files, out);
        } else if metadata.is_file() {
            let size = metadata.len();
            if size > MAX_FILE_SIZE {
                continue;
            }

            let modified = metadata.modified().ok();
            let is_text = is_text_file(&path);

            out.push(FileInfo { path, size, modified, is_text });
        }
    }
}

/// Check if a file is a text file based on extension.
fn is_text_file(path: &std::path::Path) -> bool {
    if let Some(ext) = path.extension() {
        let ext_lower = ext.to_string_lossy().to_lowercase();
        TEXT_EXTENSIONS.iter().any(|&e| e == ext_lower)
    } else {
        false
    }
}

/// Create a memory string from a file info.
/// Text files: "[filename] first 500 chars"
/// Other files: "File: relative_path (size, modified date)"
fn make_file_memory(fi: &FileInfo, base_dir: &std::path::Path) -> String {
    let rel_path = fi.path.strip_prefix(base_dir)
        .unwrap_or(&fi.path)
        .to_string_lossy()
        .to_string();

    if fi.is_text {
        // Read first MAX_CONTENT_CHARS characters
        match std::fs::read(&fi.path) {
            Ok(bytes) => {
                let text = String::from_utf8_lossy(&bytes);
                let preview: String = text.chars().take(MAX_CONTENT_CHARS).collect();
                let preview = preview.replace('\0', ""); // strip null bytes
                format!("[{}] {}", rel_path, preview.trim())
            }
            Err(_) => {
                // Can't read — fall back to metadata only
                format_file_metadata(&rel_path, fi)
            }
        }
    } else {
        format_file_metadata(&rel_path, fi)
    }
}

/// Format file metadata as a memory string.
fn format_file_metadata(rel_path: &str, fi: &FileInfo) -> String {
    let size_str = format_size(fi.size);
    let date_str = fi.modified
        .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| {
            let secs = d.as_secs() as i64;
            chrono::DateTime::from_timestamp(secs, 0)
                .map(|dt| dt.format("%Y-%m-%d").to_string())
                .unwrap_or_else(|| "unknown date".to_string())
        })
        .unwrap_or_else(|| "unknown date".to_string());
    format!("File: {rel_path} ({size_str}, modified {date_str})")
}

/// Human-readable file size.
fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

/// Compute importance for a file seed memory (0.5-0.7 range).
/// More recent files get slightly higher importance.
fn file_importance(fi: &FileInfo) -> f64 {
    let base: f64 = 0.5;
    let recency_bonus: f64 = fi.modified
        .and_then(|m| m.elapsed().ok())
        .map(|elapsed| {
            let days = elapsed.as_secs() as f64 / 86400.0;
            if days < 7.0 { 0.2 }
            else if days < 30.0 { 0.15 }
            else if days < 90.0 { 0.1 }
            else if days < 365.0 { 0.05 }
            else { 0.0 }
        })
        .unwrap_or(0.0);
    (base + recency_bonus).min(0.7)
}

// ---------------------------------------------------------------------------
// TUI auto-launch helper
// ---------------------------------------------------------------------------

/// Find the kannaka-tui binary. Checks:
/// 1. Same directory as the running binary
/// 2. PATH
/// 3. ~/.local/bin/kannaka-tui (Unix) or %LOCALAPPDATA%/kannaka/kannaka-tui.exe (Windows)
pub fn find_tui_binary() -> Option<std::path::PathBuf> {
    let ext = if cfg!(windows) { ".exe" } else { "" };
    let tui_name = format!("kannaka-tui{ext}");

    // 1. Same directory as the running binary
    if let Ok(current_exe) = std::env::current_exe() {
        if let Some(dir) = current_exe.parent() {
            let candidate = dir.join(&tui_name);
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }

    // 2. Search PATH
    if let Some(path) = find_in_path("kannaka-tui") {
        return Some(path);
    }

    // 3. Platform-specific fallback
    #[cfg(unix)]
    {
        if let Some(home) = dirs::home_dir() {
            let candidate = home.join(".local").join("bin").join(&tui_name);
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }
    #[cfg(windows)]
    {
        if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
            let candidate = std::path::PathBuf::from(local_app_data)
                .join("kannaka")
                .join(&tui_name);
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }

    None
}

/// Offer to launch the TUI after install/update completes.
/// Call this after the "Press Enter to exit" prompt.
pub fn offer_tui_launch() {
    use std::io::Write;
    eprint!("\n  Launch the Kannaka dashboard? [Y/n] > ");
    let _ = std::io::stderr().flush();
    let mut input = String::new();
    let _ = std::io::stdin().read_line(&mut input);
    if input.trim().to_lowercase() == "n" {
        return;
    }
    if let Some(path) = find_tui_binary() {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            let err = std::process::Command::new(&path).exec();
            eprintln!("Failed to launch TUI: {}", err);
        }
        #[cfg(windows)]
        {
            let _ = std::process::Command::new(&path)
                .spawn()
                .map_err(|e| eprintln!("Failed to launch TUI: {e}"));
        }
    } else {
        eprintln!("  TUI not found. Build with: cargo build --features tui --bin kannaka-tui");
    }
}

#[cfg(test)]
mod config_field_tests {
    use super::*;

    // #112: swarm.role used to be a dead field — defined with a default but
    // never settable and never read. These guard that it stays a real,
    // round-trippable knob (it is now settable via `config set swarm.role`
    // and surfaced at swarm-connect time).
    /// ADR-0059 §1 / #930: member defaults are worker/agent; `queen` is
    /// never a default (the first outside node got `queen`/`human`).
    #[test]
    fn member_defaults_are_worker_and_agent_never_queen() {
        let cfg = KannakaConfig::default();
        assert_eq!(cfg.swarm.role, "worker");
        assert_eq!(cfg.agent.kind, "agent");
        let parsed: KannakaConfig = toml::from_str("[agent]\nid = \"x-1\"\n").unwrap();
        assert_eq!(parsed.swarm.role, "worker", "a config that omits role must not become queen");
        assert_eq!(parsed.agent.kind, "agent");
    }

    #[test]
    fn swarm_role_survives_toml_roundtrip() {
        let mut cfg = KannakaConfig::default();
        cfg.swarm.role = "witness".to_string();
        let toml = toml::to_string(&cfg).expect("serialize config");
        let back: KannakaConfig = toml::from_str(&toml).expect("deserialize config");
        assert_eq!(back.swarm.role, "witness");
    }

    // ADR-0037 belief substrate config.
    #[test]
    fn belief_defaults_off_maxn_6000() {
        let cfg = KannakaConfig::default();
        assert!(!cfg.belief.enabled, "belief must default OFF (byte-identical field)");
        assert_eq!(cfg.belief.max_n, 6000);
    }

    // Quantum-Wave T1.5 flip (#475): the entropy SOURCE default is now
    // `reservoir` (was `prng`). The CONSUMPTION gate is independent and stays
    // OFF, so the flip draws nothing / adds no CLI dependency on its own.
    #[test]
    fn entropy_source_defaults_to_reservoir() {
        let cfg = KannakaConfig::default();
        assert_eq!(
            cfg.entropy.source, "reservoir",
            "T1.5 flip: entropy source defaults to reservoir"
        );
        assert_eq!(EntropyConfig::default().source, "reservoir");
    }

    #[test]
    fn dream_perturbation_still_defaults_false() {
        // The T1.5 flip touches only the SOURCE. The consumption gate must stay
        // default-OFF so no deployment starts drawing (or grows a kannaka-quantum
        // dependency) from the flip alone.
        let cfg = KannakaConfig::default();
        assert!(
            !cfg.entropy.dream_perturbation,
            "dream_perturbation must remain default false after the T1.5 flip"
        );
    }

    #[test]
    fn entropy_source_prng_opt_out_roundtrips() {
        // A deployment can still pin the PRNG explicitly.
        let mut cfg = KannakaConfig::default();
        cfg.entropy.source = "prng".to_string();
        let toml = toml::to_string(&cfg).expect("serialize config");
        let back: KannakaConfig = toml::from_str(&toml).expect("deserialize config");
        assert_eq!(back.entropy.source, "prng");
    }

    #[test]
    fn belief_survives_toml_roundtrip() {
        let mut cfg = KannakaConfig::default();
        cfg.belief.enabled = true;
        cfg.belief.max_n = 0;
        let toml = toml::to_string(&cfg).expect("serialize config");
        let back: KannakaConfig = toml::from_str(&toml).expect("deserialize config");
        assert!(back.belief.enabled);
        assert_eq!(back.belief.max_n, 0);
    }

    // Missing [belief] section (old config.toml) deserializes to the default —
    // upgrades must not break existing installs.
    #[test]
    fn belief_section_absent_uses_default() {
        let minimal = "[agent]\nid = \"x\"\n";
        let cfg: KannakaConfig = toml::from_str(minimal).expect("deserialize minimal config");
        assert!(!cfg.belief.enabled);
        assert_eq!(cfg.belief.max_n, 6000);
    }

    // The self-update binary swap is Windows-specific; its cleanup of stale
    // backup siblings is the pure part we can unit-test.
    #[cfg(windows)]
    #[test]
    fn cleanup_sweeps_stale_backups_but_keeps_binary_and_unrelated() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("kannaka.exe");
        for name in [
            "kannaka.exe",
            "kannaka.exe.bak-111",
            "kannaka.exe.bak-222",
            "kannaka.exe.old",
            "kannaka.new",
            "kannaka-other.txt",
            "kannaka.exe.config",
        ] {
            std::fs::write(dir.path().join(name), b"x").unwrap();
        }
        let keep = dir.path().join("kannaka.exe.bak-222");
        cleanup_stale_backups(&target, Some(&keep));
        // stale update siblings are gone
        assert!(!dir.path().join("kannaka.exe.bak-111").exists());
        assert!(!dir.path().join("kannaka.exe.old").exists());
        assert!(!dir.path().join("kannaka.new").exists());
        // the live binary, the just-created backup (rollback artifact), and
        // unrelated files are kept
        assert!(target.exists());
        assert!(keep.exists());
        assert!(dir.path().join("kannaka-other.txt").exists());
        assert!(dir.path().join("kannaka.exe.config").exists());
    }

    // The regression this fixes: a pre-existing legacy `.old` must not block
    // the swap. The old code renamed onto a fixed `.old` with
    // REPLACE_EXISTING — which failed forever if that `.old` was locked by a
    // process still running from it. The fix renames the live binary to a
    // UNIQUE `bak-<pid>` name and never touches `.old`, so the install
    // always proceeds.
    #[cfg(windows)]
    #[test]
    fn swap_installs_new_binary_ignoring_legacy_old() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("app.exe");
        std::fs::write(&target, b"v1").unwrap();
        // A stale legacy `.old` is present; the swap must not depend on or
        // be blocked by it.
        std::fs::write(dir.path().join("app.exe.old"), b"stale").unwrap();
        let new_file = dir.path().join("app.exe.new");
        std::fs::write(&new_file, b"v2").unwrap();

        let res = windows_swap_binary(&target, &new_file);
        assert!(res.is_ok(), "swap must succeed with a legacy .old present: {res:?}");
        assert_eq!(std::fs::read(&target).unwrap(), b"v2"); // new binary installed
        assert!(!new_file.exists()); // the staged .new was consumed
        // the returned backup is the rollback artifact — it must survive the
        // post-swap sweep (previously it was deleted whenever it wasn't
        // OS-locked, so "previous binary saved as X" was a false promise)
        let backup = res.unwrap();
        assert_eq!(std::fs::read(&backup).unwrap(), b"v1");
    }

    // A hotfix/pre-release suffix must degrade to its numeric prefix in
    // place — the old filter_map dropped the unparsable component entirely,
    // shifting later components left ("0.10.4-1" compared OLDER than 0.10.3,
    // so `kannaka update` reported "Already up to date" for everyone below).
    #[test]
    fn version_compare_handles_suffixed_components() {
        assert!(version_is_newer("0.10.4", "0.10.3"));
        assert!(version_is_newer("0.10.4-1", "0.10.3"));
        assert!(version_is_newer("0.6.10-rc.1", "0.6.9"));
        assert!(!version_is_newer("0.10.3", "0.10.3"));
        assert!(!version_is_newer("0.10.3-hotfix", "0.10.3"));
        assert!(version_is_newer("1.0.0", "0.99.99"));
    }

    // ── #595 agent identity must survive across processes ──────────────

    /// `KANNAKA_DATA_DIR` and `KANNAKA_AGENT_ID` are process-global, so these
    /// serialise and restore prior values.
    static ID_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct IdEnvGuard {
        dir: Option<String>,
        agent: Option<String>,
    }

    impl IdEnvGuard {
        fn take() -> Self {
            Self {
                dir: std::env::var("KANNAKA_DATA_DIR").ok(),
                agent: std::env::var("KANNAKA_AGENT_ID").ok(),
            }
        }
    }

    impl Drop for IdEnvGuard {
        fn drop(&mut self) {
            match &self.dir {
                Some(v) => std::env::set_var("KANNAKA_DATA_DIR", v),
                None => std::env::remove_var("KANNAKA_DATA_DIR"),
            }
            match &self.agent {
                Some(v) => std::env::set_var("KANNAKA_AGENT_ID", v),
                None => std::env::remove_var("KANNAKA_AGENT_ID"),
            }
        }
    }

    fn temp_data_dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "kannaka-id-test-{}-{}",
            tag,
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&d).expect("temp data dir");
        d
    }

    #[test]
    fn legacy_agent_id_file_is_honoured_without_config_toml() {
        let _lock = ID_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _restore = IdEnvGuard::take();
        let dir = temp_data_dir("legacy");
        std::fs::write(dir.join("agent_id"), "kannaka-prime
").unwrap();
        std::env::set_var("KANNAKA_DATA_DIR", &dir);
        std::env::remove_var("KANNAKA_AGENT_ID");

        // No config.toml in this dir — previously this minted a fresh random
        // id on every call, so a node's identity changed per process.
        let a = KannakaConfig::load();
        let b = KannakaConfig::load();
        assert_eq!(a.agent.id, "kannaka-prime", "must reuse the persisted id");
        assert_eq!(a.agent.id, b.agent.id, "identity must be stable across loads");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn env_var_still_outranks_the_legacy_file() {
        let _lock = ID_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _restore = IdEnvGuard::take();
        let dir = temp_data_dir("envwins");
        std::fs::write(dir.join("agent_id"), "from-file").unwrap();
        std::env::set_var("KANNAKA_DATA_DIR", &dir);
        std::env::set_var("KANNAKA_AGENT_ID", "from-env");

        // Documented precedence: env > config.toml > persisted file > generate.
        assert_eq!(KannakaConfig::load().agent.id, "from-env");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn blank_legacy_file_falls_through_to_generation() {
        let _lock = ID_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _restore = IdEnvGuard::take();
        let dir = temp_data_dir("blank");
        // A truncated write must not yield an EMPTY agent id.
        std::fs::write(dir.join("agent_id"), "   
").unwrap();
        std::env::set_var("KANNAKA_DATA_DIR", &dir);
        std::env::remove_var("KANNAKA_AGENT_ID");

        let id = KannakaConfig::load().agent.id;
        assert!(!id.trim().is_empty(), "must not produce an empty identity");
        assert!(id.starts_with("agent-"), "should be a generated id, got {id}");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn no_legacy_file_still_generates() {
        let _lock = ID_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _restore = IdEnvGuard::take();
        let dir = temp_data_dir("none");
        std::env::set_var("KANNAKA_DATA_DIR", &dir);
        std::env::remove_var("KANNAKA_AGENT_ID");

        assert!(KannakaConfig::legacy_agent_id().is_none());
        assert!(KannakaConfig::load().agent.id.starts_with("agent-"));

        std::fs::remove_dir_all(&dir).ok();
    }

    // ── #930 / ADR-0059 §1: init merges into an existing config ─────────

    /// A config the way a real node has it after months of `config set`s.
    fn lived_in_config() -> KannakaConfig {
        let mut cfg = KannakaConfig::default();
        cfg.agent.id = "OracleCheeks".to_string();
        cfg.agent.display_name = "Oracle Cheeks".to_string();
        cfg.agent.kind = "human".to_string();
        cfg.agent.persona = "I am the medium, speaking.".to_string();
        cfg.llm.provider = "anthropic".to_string();
        cfg.llm.model = "claude-sonnet-5".to_string();
        cfg.llm.api_key = "sk-ant-not-real".to_string();
        cfg.swarm.enabled = true;
        cfg.swarm.role = "witness".to_string();
        cfg.swarm.nats_url = "nats://example.invalid:4222".to_string();
        cfg.retention.insert(
            "radio:".to_string(),
            RetentionRule { cap: Some(500), ttl_days: Some(7.5) },
        );
        cfg.swarm_trust.trusted_agents = vec!["kannaka-prime".to_string(), "OracleCheeks".to_string()];
        cfg.swarm_trust.wire_trust_cap = 0.42;
        cfg.ghostsignals.token = "gs-token-on-file".to_string();
        cfg.ghostsignals.enabled = true;
        cfg.hrm.path = "/var/oled/kannaka/kannaka.hrm".to_string();
        cfg
    }

    fn assert_lived_in_values_survived(back: &KannakaConfig, what: &str) {
        let want = lived_in_config();
        assert_eq!(back.agent.id, want.agent.id, "{what}: agent id must survive");
        assert_eq!(back.agent.display_name, want.agent.display_name, "{what}");
        assert_eq!(back.agent.kind, want.agent.kind, "{what}: kind is not re-defaulted");
        assert_eq!(back.agent.persona, want.agent.persona, "{what}: persona must survive");
        assert_eq!(back.llm.provider, want.llm.provider, "{what}: [llm] must survive");
        assert_eq!(back.llm.model, want.llm.model, "{what}");
        assert_eq!(back.llm.api_key, want.llm.api_key, "{what}");
        assert_eq!(back.swarm.role, want.swarm.role, "{what}: role must survive");
        assert_eq!(back.swarm.nats_url, want.swarm.nats_url, "{what}");
        assert!(back.swarm.enabled, "{what}: an already-joined node stays joined");
        let rule = back.retention.get("radio:").expect("retention rule must survive");
        assert_eq!(rule.cap, Some(500), "{what}");
        assert_eq!(rule.ttl_days, Some(7.5), "{what}");
        assert_eq!(back.swarm_trust.trusted_agents, want.swarm_trust.trusted_agents, "{what}");
        assert_eq!(back.swarm_trust.wire_trust_cap, want.swarm_trust.wire_trust_cap, "{what}");
        assert_eq!(back.ghostsignals.token, want.ghostsignals.token, "{what}: hub token must survive");
        assert!(back.ghostsignals.enabled, "{what}");
    }

    #[test]
    fn init_base_config_is_the_existing_file_not_defaults() {
        let _lock = ID_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _restore = IdEnvGuard::take();
        let dir = temp_data_dir("initbase");
        std::env::set_var("KANNAKA_DATA_DIR", &dir);
        std::env::set_var("KANNAKA_AGENT_ID", "from-env-must-not-leak");

        lived_in_config().save().unwrap();
        let base = KannakaConfig::init_base_config().expect("existing config parses");
        assert_lived_in_values_survived(&base, "init_base_config");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn init_base_config_without_a_file_is_defaults_plus_legacy_id() {
        let _lock = ID_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _restore = IdEnvGuard::take();
        let dir = temp_data_dir("initbase-fresh");
        std::env::set_var("KANNAKA_DATA_DIR", &dir);
        std::fs::write(dir.join("agent_id"), "legacy-node").unwrap();

        let base = KannakaConfig::init_base_config().unwrap();
        assert_eq!(base.agent.id, "legacy-node");
        assert_eq!(base.swarm.role, "worker");
        assert!(!base.swarm.enabled);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A config that exists but does not parse is NOT silently replaced by
    /// defaults (which would mint a new identity over a repairable file).
    #[test]
    fn init_base_config_refuses_an_unparseable_config() {
        let _lock = ID_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _restore = IdEnvGuard::take();
        let dir = temp_data_dir("initbase-bad");
        std::env::set_var("KANNAKA_DATA_DIR", &dir);
        std::fs::write(dir.join("config.toml"), "[agent\nid = broken").unwrap();

        let err = KannakaConfig::init_base_config().unwrap_err();
        assert!(err.contains("does not parse"), "{err}");
        assert!(err.contains("will not overwrite"), "{err}");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The whole wizard, non-interactively, over a lived-in config: identity,
    /// persona, retention, swarm_trust, [llm] and the hub token all survive,
    /// and the agent_id compat file names the same agent. This is the re-run
    /// the first outside node hit, which used to mint a fresh `agent-xxxxxxxx`.
    #[test]
    fn non_interactive_rerun_is_a_no_op_on_identity() {
        let _lock = ID_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _restore = IdEnvGuard::take();
        let dir = temp_data_dir("rerun");
        std::env::set_var("KANNAKA_DATA_DIR", &dir);
        std::env::set_var("KANNAKA_AGENT_ID", "from-env-must-not-leak");

        lived_in_config().save().unwrap();
        let before = std::fs::read_to_string(dir.join("config.toml")).unwrap();

        let overrides = InitOverrides { non_interactive: true, ..Default::default() };
        let returned = run_init_wizard(overrides).expect("non-interactive re-run succeeds");
        assert_lived_in_values_survived(&returned, "returned config");

        let back = KannakaConfig::load_unmodified();
        assert_lived_in_values_survived(&back, "config.toml after re-run");
        assert_eq!(
            std::fs::read_to_string(dir.join("agent_id")).unwrap().trim(),
            "OracleCheeks",
            "the compat file must name the kept identity"
        );
        // The wizard only ADDS what it sets (hrm.path); nothing else moved.
        let after = std::fs::read_to_string(dir.join("config.toml")).unwrap();
        for line in before.lines().filter(|l| !l.starts_with('#') && !l.trim().is_empty()) {
            assert!(after.contains(line), "line dropped by the re-run: {line}");
        }
        let stray: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with(".kannaka-tmp-"))
            .collect();
        assert!(stray.is_empty(), "save must not leave temp files: {stray:?}");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// `--agent-id` on a re-run is still honoured (an explicit rename), but
    /// everything else is kept.
    #[test]
    fn non_interactive_rerun_with_explicit_id_keeps_the_rest() {
        let _lock = ID_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _restore = IdEnvGuard::take();
        let dir = temp_data_dir("rerun-id");
        std::env::set_var("KANNAKA_DATA_DIR", &dir);
        std::env::remove_var("KANNAKA_AGENT_ID");

        lived_in_config().save().unwrap();
        let overrides = InitOverrides {
            non_interactive: true,
            agent_id: Some("renamed-node".to_string()),
            ..Default::default()
        };
        run_init_wizard(overrides).unwrap();
        let back = KannakaConfig::load_unmodified();
        assert_eq!(back.agent.id, "renamed-node");
        assert_eq!(back.agent.persona, "I am the medium, speaking.");
        assert_eq!(back.llm.provider, "anthropic");
        assert_eq!(back.swarm.role, "witness");
        assert!(back.retention.contains_key("radio:"));

        std::fs::remove_dir_all(&dir).ok();
    }

    // ── #930: ghostsignals.enabled only with a token in hand ───────────

    #[test]
    fn ghostsignals_enabled_only_after_a_token_arrives() {
        let mut cfg = KannakaConfig::default();
        cfg.agent.id = "node-a".to_string();

        let note = apply_ghostsignals_registration(&mut cfg, Err("HTTP request failed: 503".into()));
        assert!(!cfg.ghostsignals.enabled, "a failed registration must not enable ghostsignals");
        assert!(cfg.ghostsignals.token.is_empty());
        assert!(note.contains("failed"), "{note}");
        assert!(note.contains("503"), "the line must be factual: {note}");
        assert!(!note.contains("ghostsignals register"), "no phantom command hint: {note}");

        let note = apply_ghostsignals_registration(&mut cfg, Ok("   ".into()));
        assert!(!cfg.ghostsignals.enabled, "an empty token is not a registration");
        assert!(!note.contains("ghostsignals register"), "{note}");

        // The old bug: enabled=true persisted before the hub answered. A
        // re-run over such a config, whose registration fails again, must
        // leave it disabled rather than keep the lie.
        cfg.ghostsignals.enabled = true;
        apply_ghostsignals_registration(&mut cfg, Err("timeout".into()));
        assert!(!cfg.ghostsignals.enabled);

        let note = apply_ghostsignals_registration(&mut cfg, Ok("tok-123".into()));
        assert!(cfg.ghostsignals.enabled);
        assert_eq!(cfg.ghostsignals.token, "tok-123");
        assert!(note.contains("node-a"), "{note}");
    }

    /// The hub reads `id` today (kannaka-radio#303 aligns it to `agent_id`);
    /// until then the body carries both so a trader row is created at all.
    #[test]
    fn ghostsignals_register_body_carries_agent_id_and_id() {
        let body = ghostsignals_register_body("node-a", "Node A", "agent");
        assert_eq!(body["agent_id"], "node-a");
        assert_eq!(body["id"], "node-a");
        assert_eq!(body["display_name"], "Node A");
        assert_eq!(body["kind"], "agent");
    }

    // ── #930: swarm.enabled only with credentials or an explicit choice ─

    #[test]
    fn swarm_membership_needs_credentials_or_an_explicit_anonymous_choice() {
        // Not wanted: disabled, and nobody is asked.
        let m = decide_swarm_membership(false, true, || panic!("must not ask when the operator declined"));
        assert_eq!(m, SwarmMembership::Disabled);
        let m = decide_swarm_membership(false, false, || panic!("must not ask"));
        assert_eq!(m, SwarmMembership::Disabled);
        // Credentials present: nobody is asked either.
        let m = decide_swarm_membership(true, true, || panic!("must not ask when credentials exist"));
        assert_eq!(m, SwarmMembership::Credentialed);
        // No credentials: the explicit choice decides.
        assert_eq!(decide_swarm_membership(true, false, || true), SwarmMembership::Anonymous);
        assert_eq!(decide_swarm_membership(true, false, || false), SwarmMembership::Disabled);

        let mut cfg = KannakaConfig::default();
        let note = apply_swarm_membership(&mut cfg, SwarmMembership::Disabled);
        assert!(!cfg.swarm.enabled);
        assert!(note.contains("not enabled"), "{note}");
        let note = apply_swarm_membership(&mut cfg, SwarmMembership::Anonymous);
        assert!(cfg.swarm.enabled);
        assert!(note.contains("ANONYMOUSLY"), "{note}");
        assert!(note.contains("unverified"), "{note}");
        let note = apply_swarm_membership(&mut cfg, SwarmMembership::Credentialed);
        assert!(cfg.swarm.enabled);
        assert!(note.contains("credentialed"), "{note}");
    }

    #[test]
    fn config_header_says_so_when_membership_is_anonymous() {
        let plain = config_file_header(false);
        assert!(plain.starts_with("# Kannaka Constellation Configuration\n"));
        assert!(!plain.contains("ANONYMOUS"));
        assert!(plain.ends_with("\n\n"), "header must end with a blank line before the tables");
        let anon = config_file_header(true);
        assert!(anon.contains("ANONYMOUS membership"));
        assert!(anon.contains("unverified"));
        assert!(anon.lines().all(|l| l.is_empty() || l.starts_with('#')), "header is comments only");
        // Still a valid TOML preamble.
        let doc = format!("{anon}[agent]\nid = \"x\"\n");
        let parsed: KannakaConfig = toml::from_str(&doc).unwrap();
        assert_eq!(parsed.agent.id, "x");
    }

    #[test]
    fn init_args_accept_anonymous_flag() {
        let ov = parse_init_args(&["--non-interactive".to_string(), "--anonymous".to_string()]);
        assert!(ov.non_interactive);
        assert!(ov.anonymous);
        let ov = parse_init_args(&["--no-claim".to_string()]);
        assert!(ov.anonymous);
        assert!(!parse_init_args(&[]).anonymous);
    }

    #[test]
    fn encoder_section_defaults_to_hash() {
        // A config with no [encoder] section must behave exactly as before the
        // section existed — hash encoder, byte-identical (ship-dark contract).
        let cfg: KannakaConfig = toml::from_str("").expect("empty config parses");
        assert_eq!(cfg.encoder.kind, "hash");
        assert_eq!(cfg.encoder.dim, 384);
        assert_eq!(cfg.encoder.model, "all-minilm");
        assert_eq!(cfg.encoder.base_url, "http://localhost:11434");
    }

    #[test]
    fn encoder_section_parses_ollama() {
        let cfg: KannakaConfig = toml::from_str(
            "[encoder]\nkind = \"ollama\"\nmodel = \"mxbai-embed-large\"\ndim = 1024\n",
        )
        .expect("encoder section parses");
        assert_eq!(cfg.encoder.kind, "ollama");
        assert_eq!(cfg.encoder.model, "mxbai-embed-large");
        assert_eq!(cfg.encoder.dim, 1024);
        // unspecified field falls back to its default
        assert_eq!(cfg.encoder.base_url, "http://localhost:11434");
    }

}
