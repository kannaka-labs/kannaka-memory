//! Serve guard — what a node will and will not spend on an inbound ask (#932).
//!
//! `swarm serve` subscribes to `KANNAKA.ask.broadcast`, which the anonymous
//! NATS identity may publish to. Before this module, an inbound ask reached
//! `agent::ask_notools_ex` behind nothing but a 0.4 resonance gate: a node
//! configured with a paid provider was a public, unmetered endpoint for anyone
//! on the bus.
//!
//! ## The invariant
//!
//! > A served (inbound) ask must never be able to spend without a ceiling, and
//! > must never let the caller choose what it costs.
//!
//! That is deliberately *not* ADR-0059 §3's literal wording ("an inbound ask is
//! pinned to local providers"). `kannaka-prime` — the node that *is* the public
//! `ask_kannaka` product — answers from a **remote** gateway
//! (`https://ninja-portal.com/v1`) on a virtual key already capped at $25/30d.
//! Pinning served asks to local providers would take the product off the air to
//! fix an exposure prime does not have. The ceiling is what matters, not where
//! the ceiling lives. See [`SpendPosture`].
//!
//! ## The four things this module decides
//!
//! 1. **The wire never chooses the route** — [`resolve_served_route`] derives
//!    provider and model from the node's own config and from nothing else. Any
//!    routing-shaped field on the envelope is collected by [`wire_route_fields`]
//!    so `serve` can log that it was ignored.
//! 2. **A per-requester rate limit** — [`ServeRateLimiter`], on by default.
//!    This is the actual abuse control in this PR.
//! 3. **`hops`, ceiling [`MAX_HOPS`]** — [`may_forward`] / [`outbound_hops`]. A
//!    hired ask never hires, so two brainless nodes cannot bounce one question
//!    between them forever.
//! 4. **Refuse to start unbounded** — [`spend_posture`] classifies the
//!    configured provider so `serve` can warn loudly (default) or refuse
//!    (opt-in, `KANNAKA_SERVE_REFUSE_UNBOUNDED=1`).
//!
//! Everything here is pure and takes its clock as a parameter, so all four are
//! testable without a broker, without a model and without a wall clock.

use serde_json::Value;
use std::collections::HashMap;

use crate::config::LlmConfig;

// ── 1. The wire never chooses the route ────────────────────────────────────

/// Envelope fields that a future router (#931) could mistake for routing input.
///
/// None of these has ever been honoured by the serve path, and this module
/// exists so none of them ever is. They are listed — rather than simply
/// ignored by omission — because "we happen not to read it" is not a property a
/// test can hold onto, and #931 adds a router right next to this code.
pub const WIRE_ROUTE_FIELDS: &[&str] = &[
    "provider",
    "model",
    "base_url",
    "api_key",
    "api_key_env",
    "kind",
    "route",
    "llm",
    "max_usd_per_day",
];

/// Which routing-shaped fields this envelope carried, in [`WIRE_ROUTE_FIELDS`]
/// order. Non-empty means a caller tried to steer what its ask costs us.
pub fn wire_route_fields(req: &Value) -> Vec<&'static str> {
    WIRE_ROUTE_FIELDS
        .iter()
        .copied()
        .filter(|f| req.get(*f).is_some_and(|v| !v.is_null()))
        .collect()
}

/// The provider and model a served ask will be answered with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServedRoute {
    pub provider: String,
    pub model: String,
    /// Routing-shaped fields the envelope carried and this node ignored.
    pub ignored_wire_fields: Vec<&'static str>,
}

/// Resolve the route for an inbound ask.
///
/// `req` is read **only** to report what was ignored. Provider and model come
/// from `llm` and from nothing else: same config in, same route out, whatever
/// the caller put on the wire. That is the whole of behaviour 1, and the
/// `served_route_ignores_wire_*` tests are what keep it true.
pub fn resolve_served_route(llm: &LlmConfig, req: &Value) -> ServedRoute {
    ServedRoute {
        provider: llm.provider.clone(),
        model: llm.model.clone(),
        ignored_wire_fields: wire_route_fields(req),
    }
}

// ── 3. hops: a hired ask never hires ───────────────────────────────────────

/// How many times an ask may be forwarded between nodes. One node may hire a
/// second; the second may not hire a third.
pub const MAX_HOPS: u32 = 1;

/// The hop count an envelope declares. A missing, non-numeric or negative field
/// is 0 — an envelope written before this field existed is a first-hop ask, and
/// must keep working exactly as it always did.
pub fn hops_of(req: &Value) -> u32 {
    req.get("hops")
        .and_then(|v| v.as_u64())
        .unwrap_or(0)
        .min(u32::MAX as u64) as u32
}

/// May a node holding an ask at `inbound` hops forward it to another node?
///
/// Answering locally is always allowed — that is what terminates the chain.
/// Only *re-hiring* is capped.
pub fn may_forward(inbound: u32) -> bool {
    inbound < MAX_HOPS
}

/// The `hops` value to stamp on an ask this node is sending. `None` = this node
/// originated the ask.
pub fn outbound_hops(inbound: Option<u32>) -> u32 {
    match inbound {
        None => 0,
        Some(h) => h.saturating_add(1),
    }
}

/// Hop count of the served ask this process is currently answering, or `None`
/// when it is not serving one.
///
/// A process-wide cell rather than a parameter because the outbound publisher
/// ([`crate`]'s `ask --remote` handler) and the serve loop do not share a call
/// stack. `u32::MAX` is the sentinel for "not serving"; a real ask can never
/// reach it because [`may_forward`] caps forwarding at [`MAX_HOPS`].
static SERVING_HOPS: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(u32::MAX);

/// Mark this process as answering a served ask at `hops` (or, with `None`, as
/// no longer serving one). Call it around the answer, not around the whole loop.
pub fn set_serving_hops(hops: Option<u32>) {
    SERVING_HOPS.store(
        hops.unwrap_or(u32::MAX),
        std::sync::atomic::Ordering::SeqCst,
    );
}

/// The hop count of the ask being served right now, if any.
pub fn serving_hops() -> Option<u32> {
    match SERVING_HOPS.load(std::sync::atomic::Ordering::SeqCst) {
        u32::MAX => None,
        h => Some(h),
    }
}

// ── 2. Per-requester rate limit ────────────────────────────────────────────

/// Window the per-requester and global counters roll over on.
pub const WINDOW_SECS: u64 = 3600;

/// Asks one requester may have answered per hour before this node declines.
///
/// **Not calibrated against a measured prime load**: no JetStream stream
/// captures `KANNAKA.ask.>`, so there is no history to derive a real rate from.
/// 60/hour is one ask a minute sustained — comfortably above what the public
/// `ask_kannaka` tool drives and far below a drain loop. An operator who finds
/// it low raises it with `KANNAKA_SERVE_ASKS_PER_HOUR`; `serve` prints both
/// numbers at startup so the knob is discoverable from the log alone.
pub const DEFAULT_ASKS_PER_HOUR: u32 = 60;

/// Asks this node answers per hour across *all* requesters. The backstop for
/// the identity problem below: a caller who rotates identity gets past the
/// per-requester limit and lands here.
pub const DEFAULT_ASKS_PER_HOUR_TOTAL: u32 = 300;

/// Requesters tracked at once. A caller who rotates `from` on every ask would
/// otherwise grow this map without bound — the rate limit's own memory becoming
/// the DoS. Past this, new identities are refused rather than recorded.
pub const MAX_TRACKED_REQUESTERS: usize = 4096;

/// Identity this node will rate-limit an inbound ask against.
///
/// **Both inputs are chosen by the caller.** NATS core carries no publisher
/// identity on a message — `NatsMessage` has a subject, a payload and a
/// reply-to, and nothing else — so there is no authenticated user to key on,
/// even when the connection itself was authenticated. What is available:
///
/// - `from`, the id the envelope declares. Honest clients set their agent id;
///   an anonymous caller sets whatever it likes.
/// - the reply inbox, `_INBOX.<tag>.<pid>.<uuid>.<nonce>`. The uuid and nonce
///   are fresh per request, so only the `_INBOX.<tag>.<pid>` prefix is stable —
///   stable across one calling *process*, rotated by restarting it.
///
/// So this key raises the cost of abuse; it does not make it impossible. The
/// per-requester limit is the polite bound on an honest neighbour, and the
/// global ceiling is the bound that actually holds against a caller who rotates.
/// Say so in the log, not only here.
pub fn requester_key(from: Option<&str>, reply_to: Option<&str>) -> String {
    let declared = from.map(str::trim).filter(|s| !s.is_empty() && *s != "?");
    if let Some(id) = declared {
        return format!("from:{id}");
    }
    match reply_to {
        Some(inbox) => format!("inbox:{}", inbox_identity(inbox)),
        None => "anonymous".to_string(),
    }
}

/// The stable prefix of a reply inbox: `_INBOX.<tag>.<pid>`. Anything past the
/// third token is fresh per request and would make every ask a new identity.
pub fn inbox_identity(reply_to: &str) -> String {
    reply_to
        .split('.')
        .take(3)
        .collect::<Vec<_>>()
        .join(".")
}

/// What the limiter decided about one inbound ask.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RateDecision {
    /// Answer it.
    Allow,
    /// This requester is over [`ServeRateLimiter::per_requester`] for the hour.
    Requester {
        limit: u32,
        /// First refusal for this requester in this window. `serve` logs on
        /// `true` only, so a caller hammering the bus produces one line an
        /// hour, not one line an ask.
        first_in_window: bool,
    },
    /// The node is over its hourly ceiling across all requesters.
    Global {
        limit: u32,
        first_in_window: bool,
    },
    /// Too many distinct requesters to track. Identity rotation, almost
    /// certainly; refuse rather than let the limiter's map grow.
    TooManyRequesters { tracked: usize },
}

impl RateDecision {
    pub fn is_allow(&self) -> bool {
        matches!(self, RateDecision::Allow)
    }

    /// Short, polite sentence for the requester. Names no internal state beyond
    /// the limit it hit — a refusal is not a reconnaissance surface.
    pub fn refusal_text(&self) -> Option<String> {
        match self {
            RateDecision::Allow => None,
            RateDecision::Requester { limit, .. } => Some(format!(
                "rate limited: this node answers at most {limit} asks per requester per hour. Try again later."
            )),
            RateDecision::Global { limit, .. } => Some(format!(
                "rate limited: this node answers at most {limit} asks per hour in total. Try again later."
            )),
            RateDecision::TooManyRequesters { .. } => Some(
                "rate limited: this node is tracking too many requesters right now. Try again later."
                    .to_string(),
            ),
        }
    }
}

#[derive(Debug, Clone)]
struct Bucket {
    window_start: u64,
    count: u32,
    refusal_logged: bool,
}

impl Bucket {
    fn new(now: u64) -> Self {
        Self { window_start: now, count: 0, refusal_logged: false }
    }

    /// Roll the window if it has expired. A clock that jumped backwards
    /// (`now < window_start`) also rolls — the alternative is a counter frozen
    /// at its limit until the clock catches up.
    fn roll(&mut self, now: u64) {
        let expired = now < self.window_start || now - self.window_start >= WINDOW_SECS;
        if expired {
            *self = Bucket::new(now);
        }
    }

    /// Record a refusal; returns true the first time in this window.
    fn mark_refused(&mut self) -> bool {
        let first = !self.refusal_logged;
        self.refusal_logged = true;
        first
    }
}

/// Per-requester and global hourly ceilings for a serving node.
///
/// Counts **answers attempted**, not tokens: this PR has no per-call cost
/// accounting (that arrives with the providers table, #931), so an ask is the
/// unit it can actually meter.
#[derive(Debug)]
pub struct ServeRateLimiter {
    per_requester: u32,
    global_limit: u32,
    requesters: HashMap<String, Bucket>,
    global: Bucket,
}

impl ServeRateLimiter {
    pub fn new(per_requester: u32, global_limit: u32) -> Self {
        Self {
            per_requester,
            global_limit,
            requesters: HashMap::new(),
            global: Bucket::new(0),
        }
    }

    /// Limits from the environment, falling back to the documented defaults.
    /// A malformed or zero value is ignored rather than treated as "refuse
    /// everything" — a typo in a unit file must not silently take a node off
    /// the air.
    pub fn from_env() -> Self {
        Self::new(
            env_limit("KANNAKA_SERVE_ASKS_PER_HOUR", DEFAULT_ASKS_PER_HOUR),
            env_limit("KANNAKA_SERVE_ASKS_PER_HOUR_TOTAL", DEFAULT_ASKS_PER_HOUR_TOTAL),
        )
    }

    pub fn per_requester(&self) -> u32 {
        self.per_requester
    }

    pub fn global_limit(&self) -> u32 {
        self.global_limit
    }

    /// Decide one inbound ask, and count it when the answer is yes.
    ///
    /// The global bucket is checked first: it is the ceiling that holds when a
    /// caller rotates identity, and checking it first means a rotating caller
    /// cannot spend the node's hour by seeding 300 fresh per-requester buckets.
    pub fn check(&mut self, key: &str, now_secs: u64) -> RateDecision {
        self.global.roll(now_secs);
        if self.global.count >= self.global_limit {
            let first = self.global.mark_refused();
            return RateDecision::Global { limit: self.global_limit, first_in_window: first };
        }

        // Drop expired entries before consulting capacity, so an hour of
        // honest traffic never counts against a rotating attacker's budget.
        if self.requesters.len() >= MAX_TRACKED_REQUESTERS {
            self.requesters
                .retain(|_, b| now_secs >= b.window_start && now_secs - b.window_start < WINDOW_SECS);
        }
        if !self.requesters.contains_key(key) && self.requesters.len() >= MAX_TRACKED_REQUESTERS {
            return RateDecision::TooManyRequesters { tracked: self.requesters.len() };
        }

        let bucket = self
            .requesters
            .entry(key.to_string())
            .or_insert_with(|| Bucket::new(now_secs));
        bucket.roll(now_secs);
        if bucket.count >= self.per_requester {
            let first = bucket.mark_refused();
            return RateDecision::Requester { limit: self.per_requester, first_in_window: first };
        }

        bucket.count += 1;
        self.global.count += 1;
        RateDecision::Allow
    }

    /// Requesters currently tracked. For the log line only.
    pub fn tracked(&self) -> usize {
        self.requesters.len()
    }
}

fn env_limit(var: &str, default: u32) -> u32 {
    match std::env::var(var) {
        Ok(v) => v.trim().parse::<u32>().ok().filter(|n| *n > 0).unwrap_or(default),
        Err(_) => default,
    }
}

// ── 4. Refuse to start unbounded ───────────────────────────────────────────

/// Can this node's configured provider spend money, and is that spend bounded?
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpendPosture {
    /// Nothing to spend: no provider, or a local model on this box.
    Free { why: &'static str },
    /// A keyed provider with a ceiling the operator has declared.
    Capped { how: String },
    /// A keyed provider with no declared ceiling. This is the #932 exposure:
    /// every anonymous broadcast this node answers spends the operator's money
    /// against no stated limit.
    Unbounded,
}

impl SpendPosture {
    pub fn is_unbounded(&self) -> bool {
        matches!(self, SpendPosture::Unbounded)
    }
}

/// Classify the configured provider.
///
/// `key_present` is passed in rather than read here so the decision stays pure;
/// [`api_key_present`] is the thin impure wrapper `serve` actually calls.
///
/// The order of the tests matters. `provider = "openai"` says nothing on its
/// own: the installer writes it for both the hosted gateway and a local Ollama
/// brain on `localhost:11434` (ADR-0059 §3 migrates by `(base_url, model)` for
/// exactly this reason), so the base URL decides before the provider string does.
pub fn spend_posture(llm: &LlmConfig, key_present: bool) -> SpendPosture {
    match llm.provider.as_str() {
        "" | "none" => return SpendPosture::Free { why: "no provider configured" },
        "ollama" => return SpendPosture::Free { why: "ollama is a local model" },
        _ => {}
    }
    if is_local_base_url(&llm.base_url) {
        return SpendPosture::Free { why: "base_url is on this box" };
    }
    if !key_present {
        return SpendPosture::Free { why: "no API key — this node cannot spend" };
    }
    // A ceiling of zero or less is not a ceiling; treat it as undeclared rather
    // than as "spend nothing", which is not what this PR enforces either way.
    if let Some(usd) = llm.max_usd_per_day.filter(|v| *v > 0.0) {
        return SpendPosture::Capped { how: format!("declared max_usd_per_day = ${usd:.2}") };
    }
    if llm.externally_capped {
        return SpendPosture::Capped {
            how: "operator declares the key is capped upstream (externally_capped = true)".to_string(),
        };
    }
    SpendPosture::Unbounded
}

/// Is this base URL served from the machine `serve` runs on?
///
/// Empty counts as local only for providers that default to a local daemon;
/// callers reach here after the `ollama` arm, and an empty `base_url` on
/// `openai`/`anthropic` means the vendor's own host, so empty is **not** local.
pub fn is_local_base_url(url: &str) -> bool {
    let u = url.trim();
    if u.is_empty() {
        return false;
    }
    let after_scheme = u.split("://").nth(1).unwrap_or(u);
    let authority = after_scheme.split(['/', '?', '#']).next().unwrap_or("");
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    // Strip the port, taking care not to cut an unbracketed IPv6 literal.
    let host = if let Some(rest) = authority.strip_prefix('[') {
        rest.split(']').next().unwrap_or(rest)
    } else {
        authority.split(':').next().unwrap_or(authority)
    };
    matches!(
        host.to_ascii_lowercase().as_str(),
        "localhost" | "127.0.0.1" | "0.0.0.0" | "::1" | "host.docker.internal"
    ) || host.starts_with("127.")
}

/// Whether `cfg.llm` has an API key available, from the config or from the same
/// environment variables [`crate::agent::client_from_config`] reads. Impure by
/// design; kept next to [`spend_posture`] so the two cannot drift.
pub fn api_key_present(llm: &LlmConfig) -> bool {
    if !llm.api_key.is_empty() {
        return true;
    }
    let vendor_var = match llm.provider.as_str() {
        "anthropic" => "ANTHROPIC_API_KEY",
        "openai" => "OPENAI_API_KEY",
        _ => "",
    };
    (!vendor_var.is_empty() && std::env::var(vendor_var).is_ok_and(|v| !v.is_empty()))
        || std::env::var("KANNAKA_LLM_API_KEY").is_ok_and(|v| !v.is_empty())
}

/// Has the operator opted in to `serve` refusing to start when the posture is
/// [`SpendPosture::Unbounded`]?
///
/// Opt-in, never the default. `kannaka-prime` upgrades to this build the moment
/// it is deployed; a hard refusal on by default would take the public
/// `ask_kannaka` product off the air to close an exposure prime does not have
/// (its key is capped upstream). A loud warning plus the rate limit is the
/// default; the hard stop is for operators who want it.
pub fn refuse_unbounded_requested() -> bool {
    matches!(
        std::env::var("KANNAKA_SERVE_REFUSE_UNBOUNDED").as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn llm(provider: &str, model: &str, base_url: &str) -> LlmConfig {
        LlmConfig {
            provider: provider.to_string(),
            model: model.to_string(),
            api_key: String::new(),
            base_url: base_url.to_string(),
            max_usd_per_day: None,
            externally_capped: false,
        }
    }

    // ── 1. the wire never chooses the route ────────────────────────────────

    #[test]
    fn served_route_ignores_wire_provider_and_model() {
        let cfg = llm("openai", "kannaka-claude-haiku", "https://ninja-portal.com/v1");
        let plain = json!({ "from": "someone", "text": "hello" });
        let steered = json!({
            "from": "someone",
            "text": "hello",
            "provider": "anthropic",
            "model": "claude-opus-5",
            "kind": "reason",
            "route": ["anthropic", "openai"],
            "api_key": "sk-not-yours",
        });

        let a = resolve_served_route(&cfg, &plain);
        let b = resolve_served_route(&cfg, &steered);

        assert_eq!(a.provider, "openai");
        assert_eq!(a.model, "kannaka-claude-haiku");
        assert_eq!(
            (a.provider.as_str(), a.model.as_str()),
            (b.provider.as_str(), b.model.as_str()),
            "an ask carrying routing fields must be answered by the configured provider"
        );
        assert!(a.ignored_wire_fields.is_empty());
        assert_eq!(
            b.ignored_wire_fields,
            vec!["provider", "model", "api_key", "kind", "route"],
            "serve must be able to say which fields it ignored"
        );
    }

    #[test]
    fn wire_route_fields_ignores_nulls_and_unrelated_keys() {
        let req = json!({ "text": "hi", "mode": "attention", "provider": null, "hops": 0 });
        assert!(wire_route_fields(&req).is_empty());
    }

    // ── 3. hops ────────────────────────────────────────────────────────────

    #[test]
    fn missing_hops_is_zero_so_old_envelopes_still_work() {
        assert_eq!(hops_of(&json!({ "text": "hi" })), 0);
        assert_eq!(hops_of(&json!({ "text": "hi", "hops": null })), 0);
        assert_eq!(hops_of(&json!({ "text": "hi", "hops": "two" })), 0);
        assert_eq!(hops_of(&json!({ "text": "hi", "hops": -1 })), 0);
        assert_eq!(hops_of(&json!({ "text": "hi", "hops": 1 })), 1);
    }

    #[test]
    fn a_hired_ask_never_hires() {
        assert!(may_forward(0), "a node may hire once");
        assert!(!may_forward(1), "an ask already hired once may not hire again");
        assert!(!may_forward(7));
        assert_eq!(outbound_hops(None), 0, "an originated ask starts at zero");
        assert_eq!(outbound_hops(Some(0)), 1, "forwarding increments");
        assert_eq!(outbound_hops(Some(u32::MAX)), u32::MAX, "no overflow");
    }

    #[test]
    fn serving_hops_cell_round_trips() {
        assert_eq!(serving_hops(), None, "idle process is not serving");
        set_serving_hops(Some(0));
        assert_eq!(serving_hops(), Some(0));
        set_serving_hops(Some(1));
        assert_eq!(serving_hops(), Some(1));
        set_serving_hops(None);
        assert_eq!(serving_hops(), None);
    }

    // ── 2. rate limit ──────────────────────────────────────────────────────

    #[test]
    fn requester_key_prefers_the_declared_id_then_the_inbox_prefix() {
        assert_eq!(requester_key(Some("kannaka-prime"), None), "from:kannaka-prime");
        assert_eq!(
            requester_key(Some("  "), Some("_INBOX.req.4242.deadbeef.17")),
            "inbox:_INBOX.req.4242"
        );
        assert_eq!(
            requester_key(Some("?"), Some("_INBOX.req.4242.cafe.18")),
            "inbox:_INBOX.req.4242",
            "the `?` placeholder is not an identity"
        );
        assert_eq!(
            requester_key(None, Some("_INBOX.req.4242.other.19")),
            requester_key(None, Some("_INBOX.req.4242.another.20")),
            "two requests from one process share an inbox prefix"
        );
        assert_ne!(
            requester_key(None, Some("_INBOX.req.4242.x.1")),
            requester_key(None, Some("_INBOX.req.9999.x.1")),
            "a different process is a different requester"
        );
        assert_eq!(requester_key(None, None), "anonymous");
    }

    #[test]
    fn per_requester_limit_refuses_and_logs_once_per_window() {
        let mut rl = ServeRateLimiter::new(3, 100);
        for i in 0..3 {
            assert!(rl.check("from:a", 1000).is_allow(), "ask {i} should pass");
        }
        match rl.check("from:a", 1000) {
            RateDecision::Requester { limit, first_in_window } => {
                assert_eq!(limit, 3);
                assert!(first_in_window, "the first refusal in a window is logged");
            }
            other => panic!("expected a per-requester refusal, got {other:?}"),
        }
        match rl.check("from:a", 1000) {
            RateDecision::Requester { first_in_window, .. } => {
                assert!(!first_in_window, "later refusals must not log again");
            }
            other => panic!("expected a per-requester refusal, got {other:?}"),
        }
        // A different requester is unaffected by the first one's exhaustion.
        assert!(rl.check("from:b", 1000).is_allow());
        // ...and the window rolls.
        assert!(rl.check("from:a", 1000 + WINDOW_SECS).is_allow());
    }

    #[test]
    fn the_global_ceiling_holds_when_a_caller_rotates_identity() {
        let mut rl = ServeRateLimiter::new(2, 5);
        // Five asks, each under a fresh identity: the per-requester limit never
        // fires, and the global ceiling is the only thing standing.
        for i in 0..5 {
            assert!(rl.check(&format!("from:sock{i}"), 1000).is_allow(), "ask {i}");
        }
        match rl.check("from:sock99", 1000) {
            RateDecision::Global { limit, first_in_window } => {
                assert_eq!(limit, 5);
                assert!(first_in_window);
            }
            other => panic!("expected a global refusal, got {other:?}"),
        }
        assert!(
            !rl.check("from:honest", 1000).is_allow(),
            "the global ceiling is global — an honest caller waits too"
        );
        assert!(rl.check("from:honest", 1000 + WINDOW_SECS).is_allow());
    }

    #[test]
    fn a_refusal_says_something_polite() {
        let mut rl = ServeRateLimiter::new(1, 10);
        assert!(rl.check("from:a", 0).is_allow());
        let d = rl.check("from:a", 0);
        let text = d.refusal_text().expect("a refusal must have something to say");
        assert!(text.contains("rate limited"), "{text}");
        assert!(!text.contains("from:a"), "a refusal is not a reconnaissance surface: {text}");
        assert_eq!(RateDecision::Allow.refusal_text(), None);
    }

    #[test]
    fn identity_rotation_cannot_grow_the_limiter_without_bound() {
        // Global ceiling above the tracking cap so the map, not the ceiling, is
        // what this test exercises.
        let mut rl = ServeRateLimiter::new(1, u32::MAX);
        for i in 0..MAX_TRACKED_REQUESTERS {
            assert!(rl.check(&format!("from:s{i}"), 1000).is_allow(), "ask {i}");
        }
        assert_eq!(rl.tracked(), MAX_TRACKED_REQUESTERS);
        match rl.check("from:one-too-many", 1000) {
            RateDecision::TooManyRequesters { tracked } => {
                assert_eq!(tracked, MAX_TRACKED_REQUESTERS);
            }
            other => panic!("expected a capacity refusal, got {other:?}"),
        }
        assert_eq!(rl.tracked(), MAX_TRACKED_REQUESTERS, "a refusal must not record a new key");
        // An hour later the stale entries are reclaimed and the node serves again.
        assert!(rl.check("from:later", 1000 + WINDOW_SECS).is_allow());
    }

    #[test]
    fn a_backwards_clock_does_not_freeze_a_bucket() {
        let mut rl = ServeRateLimiter::new(1, 10);
        assert!(rl.check("from:a", 10_000).is_allow());
        assert!(!rl.check("from:a", 10_000).is_allow());
        assert!(rl.check("from:a", 9_000).is_allow(), "a clock jump back rolls the window");
    }

    // ── 4. spend posture ───────────────────────────────────────────────────

    #[test]
    fn a_keyed_provider_with_no_ceiling_is_unbounded() {
        let mut c = llm("anthropic", "claude-sonnet-5", "");
        assert_eq!(spend_posture(&c, true), SpendPosture::Unbounded);
        assert!(spend_posture(&c, true).is_unbounded());
        // ...and a declared ceiling settles it.
        c.max_usd_per_day = Some(2.0);
        assert!(matches!(spend_posture(&c, true), SpendPosture::Capped { .. }));
        c.max_usd_per_day = Some(0.0);
        assert_eq!(
            spend_posture(&c, true),
            SpendPosture::Unbounded,
            "a ceiling of zero is not a ceiling"
        );
        c.max_usd_per_day = None;
        c.externally_capped = true;
        assert!(matches!(spend_posture(&c, true), SpendPosture::Capped { .. }));
    }

    #[test]
    fn prime_on_a_capped_remote_gateway_is_not_unbounded() {
        // The live kannaka-prime config, plus the marker this PR adds.
        let mut c = llm("openai", "kannaka-claude-haiku", "https://ninja-portal.com/v1");
        assert_eq!(
            spend_posture(&c, true),
            SpendPosture::Unbounded,
            "undeclared, a remote gateway key looks exactly like any other paid key"
        );
        c.externally_capped = true;
        match spend_posture(&c, true) {
            SpendPosture::Capped { how } => assert!(how.contains("externally_capped"), "{how}"),
            other => panic!("expected Capped, got {other:?}"),
        }
    }

    #[test]
    fn a_local_brain_never_warns() {
        // The installer writes provider = "openai" for the local Ollama brain,
        // so the base URL has to be what decides.
        let local_openai = llm("openai", "kannaka-brain", "http://localhost:11434/v1");
        assert!(!spend_posture(&local_openai, true).is_unbounded());
        assert!(!spend_posture(&llm("ollama", "kannaka-brain", ""), true).is_unbounded());
        assert!(!spend_posture(&llm("none", "", ""), true).is_unbounded());
        assert!(!spend_posture(&llm("", "", ""), false).is_unbounded());
        assert!(
            !spend_posture(&llm("anthropic", "claude-sonnet-5", ""), false).is_unbounded(),
            "no key means nothing to spend"
        );
    }

    #[test]
    fn local_base_urls_are_recognised() {
        for u in [
            "http://localhost:11434",
            "https://127.0.0.1/v1",
            "http://127.0.0.53:11434/v1",
            "http://[::1]:11434/v1",
            "http://0.0.0.0:8080",
            "http://host.docker.internal:11434",
            "localhost:11434",
        ] {
            assert!(is_local_base_url(u), "should be local: {u}");
        }
        for u in [
            "",
            "https://ninja-portal.com/v1",
            "https://api.openai.com/v1",
            "http://10.30.0.20:11434/v1",
            "http://localhost.evil.example/v1",
            "http://notlocalhost/v1",
        ] {
            assert!(!is_local_base_url(u), "should NOT be local: {u}");
        }
    }
}
