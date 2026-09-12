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
//!    This is the actual abuse control in this PR. It is deliberately split in
//!    two: [`ServeRateLimiter::check`] before the work, and
//!    [`ServeRateLimiter::commit_global`] at the spend. An abuse control that a
//!    stranger can turn into an outage cheaper than the abuse is not a control,
//!    and metering the hourly ceiling before the resonance gate did exactly
//!    that — see `check`'s own doc comment.
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

/// Every hop count at or above this one means the same thing — do not forward —
/// so a wire value is clamped here rather than carried through the code as the
/// number a stranger chose.
pub const HOPS_CLAMP: u32 = MAX_HOPS + 1;

/// The hop count an envelope declares. A missing, non-numeric or negative field
/// is 0 — an envelope written before this field existed is a first-hop ask, and
/// must keep working exactly as it always did.
///
/// The clamp is [`HOPS_CLAMP`], not `u32::MAX`. An earlier revision clamped to
/// `u32::MAX` while `u32::MAX` was also the in-band sentinel for "this process
/// is not serving anything", so a wire value of `4294967295` round-tripped
/// through [`set_serving_hops`] and came back as "I originated this ask" — a
/// forged number reinstating the hop budget it was supposed to exhaust. The
/// sentinel is gone (see [`serving_hops`]) and the clamp no longer reaches it;
/// both halves of that bug are closed independently, because the wire is the
/// threat model and reasoning about the values *this* code produces is not a
/// defence against values it merely receives.
pub fn hops_of(req: &Value) -> u32 {
    req.get("hops")
        .and_then(|v| v.as_u64())
        .unwrap_or(0)
        .min(HOPS_CLAMP as u64) as u32
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
/// stack.
///
/// `Option<u32>` behind a mutex, **not** an atomic with an in-band sentinel.
/// "Not serving" and "serving an ask whose hop count happens to equal the
/// sentinel" are different facts, and a wire-supplied number must never be able
/// to become the first by spelling the second. The type keeps them apart, so no
/// clamp anywhere else in this module can reintroduce the collision.
static SERVING_HOPS: std::sync::Mutex<Option<u32>> = std::sync::Mutex::new(None);

/// Mark this process as answering a served ask at `hops` (or, with `None`, as
/// no longer serving one). Call it around the answer, not around the whole loop.
pub fn set_serving_hops(hops: Option<u32>) {
    // A poisoned lock means some other thread panicked mid-update. The cell is
    // one `Option<u32>` with no invariant spanning two fields, so recovering
    // the value is sound — and failing closed here would be worse than the
    // panic it inherited: `serve` would stop being able to say it is serving.
    let mut cell = SERVING_HOPS.lock().unwrap_or_else(|e| e.into_inner());
    *cell = hops;
}

/// The hop count of the ask being served right now, if any.
pub fn serving_hops() -> Option<u32> {
    *SERVING_HOPS.lock().unwrap_or_else(|e| e.into_inner())
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

/// Longest requester id this node will store verbatim, in bytes.
///
/// A cap on the *number* of tracked requesters bounds entries, not bytes, and
/// the bytes are the ones an attacker picks: `from` comes off the wire and the
/// broker's `max_payload` is 64MB (`config/nats-accounts.conf`), so ~85 asks
/// under distinct 64MB ids would retain ~5.4GB — more than O1 has, and well
/// inside a 300/hour ceiling. Anything longer than this is replaced by a hash
/// of itself, which bounds what one entry can retain to a constant while
/// keeping two different long ids in two different buckets.
pub const MAX_REQUESTER_ID_BYTES: usize = 128;

/// Bound on the bytes one tracked requester can retain: two capped components
/// plus their tags. Multiplied by [`MAX_TRACKED_REQUESTERS`] this is the
/// limiter's whole memory footprint, and it is what
/// [`ServeRateLimiter::tracked_bytes`] measures.
pub const MAX_REQUESTER_KEY_BYTES: usize = 2 * MAX_REQUESTER_ID_BYTES + 16;

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
/// The key is **the pair**, not whichever is present. Keying on `from` alone
/// was not merely evadable, it was aimable: three asks declaring
/// `from = "kannaka-prime"` exhausted that peer's bucket, so any caller could
/// spend an honest neighbour's quota by claiming its name. Keying on the inbox
/// alone has the same shape, because `reply_to` is caller-chosen too. Requiring
/// both means a caller can only exhaust the bucket it actually owns unless it
/// also guesses the victim's calling process — strictly harder than either
/// half, and the most this layer can do.
///
/// Because it is the most this layer can do, say the rest plainly: **there is
/// no unforgeable identity on this path.** NATS core attaches none to a message
/// even on an authenticated connection. So the per-requester limit is not
/// meaningful on its own — it raises the cost of abuse and keeps honest
/// neighbours from spending each other's quota. The hourly total is the bound
/// that holds against a determined caller. The startup banner says this too.
///
/// Every returned key is at most [`MAX_REQUESTER_KEY_BYTES`] bytes, whatever
/// the caller sent — see [`bounded_id`].
pub fn requester_key(from: Option<&str>, reply_to: Option<&str>) -> String {
    let declared = from
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != "?")
        .map(bounded_id)
        .unwrap_or_else(|| "-".to_string());
    let inbox = reply_to
        .map(|r| bounded_id(&inbox_identity(r)))
        .unwrap_or_else(|| "-".to_string());
    format!("inbox:{inbox}+from:{declared}")
}

/// Is this reply subject one a serving node will publish to?
///
/// Only `_INBOX.<something>` — the subject space NATS reserves for request
/// inboxes — and never a wildcard.
///
/// `reply_to` comes off the wire, so without this a stranger can aim any of
/// this handler's replies at a third party, or at an ordinary subject. The
/// sharp edge is not volume (a refusal is ~110 bytes against a ~30-byte ask)
/// but **privilege**: the serving node authenticates with publish `>`, while
/// `anon` is explicitly denied publish on `KANNAKA.work.>`, `KANNAKA.inbox.>`
/// and the JetStream admin subjects (`config/nats-accounts.conf`). Reflecting
/// through a serving node is therefore a way to emit onto subjects the caller
/// may not publish to itself.
///
/// This guard runs before every reply in the handler, including the two that
/// predate this work (`bad json`, `empty text`), so it closes the primitive
/// rather than only the refusal this change added.
pub fn is_valid_reply_inbox(reply_to: &str) -> bool {
    reply_to.starts_with("_INBOX.")
        && reply_to.len() > "_INBOX.".len()
        && !reply_to.contains(['*', '>', ' ', '\t', '\r', '\n'])
}

/// Is this declared id short enough to be worth answering at all?
///
/// `requester_key` never stores an oversized id, so this is not what bounds the
/// limiter — `serve` uses it to refuse an absurd envelope before it costs a
/// recall, and because an id that long is not a name, it is a payload.
pub fn id_is_oversized(from: &str) -> bool {
    from.len() > MAX_REQUESTER_ID_BYTES
}

/// An id capped at [`MAX_REQUESTER_ID_BYTES`] bytes.
///
/// Short ids pass through unchanged, so the common case stays readable in the
/// log. A long one becomes `#<32 hex>` — a hash, not a truncation, because
/// truncating to a shared prefix would let one caller choose which *other*
/// caller's bucket to exhaust by prefixing its id.
pub fn bounded_id(id: &str) -> String {
    if id.len() <= MAX_REQUESTER_ID_BYTES {
        return id.to_string();
    }
    let digest = blake3::hash(id.as_bytes());
    format!("#{}", &digest.to_hex().as_str()[..32])
}

/// The stable prefix of a reply inbox: `_INBOX.<tag>.<pid>`. Anything past the
/// third token is fresh per request and would make every ask a new identity.
///
/// The tokens themselves are wire-sized, so the result still goes through
/// [`bounded_id`] before it is stored.
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

    /// Decide one inbound ask.
    ///
    /// Counts it against **the requester's** bucket when the answer is yes, and
    /// leaves the global bucket alone: the global ceiling is committed by
    /// [`commit_global`](Self::commit_global), at the point the ask actually
    /// reaches the model.
    ///
    /// That split is the whole design, and getting it wrong made the control
    /// worse than the problem. `swarm serve` decides whether to answer a
    /// broadcast at all with a resonance gate *after* this call. If this
    /// function committed the global bucket, a stranger publishing ~30-byte
    /// non-resonant asks — every one silently dropped by that gate, none of
    /// them costing a token — would still burn the node's hourly ceiling at one
    /// ask every twelve seconds and take the public `ask_kannaka` off the air
    /// for everybody. An abuse control that a 30-byte publish can turn into an
    /// outage is a cheaper attack than the one it was written to stop.
    ///
    /// The per-requester bucket *is* committed here, because everything past
    /// this point costs a full recall against the medium on a 1-vCPU hub. That
    /// is CPU the caller spends on itself, and it denies nobody else.
    ///
    /// The global ceiling is still *checked* first, so a caller who rotates
    /// identity cannot seed a hundred fresh buckets past an exhausted node.
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
        RateDecision::Allow
    }

    /// Commit one ask against the hourly ceiling. Call it where the spend
    /// happens — after every gate that could still drop the ask, immediately
    /// before the model call — so the ceiling bounds what it claims to bound.
    pub fn commit_global(&mut self, now_secs: u64) {
        self.global.roll(now_secs);
        self.global.count = self.global.count.saturating_add(1);
    }

    /// Requesters currently tracked. For the log line only.
    pub fn tracked(&self) -> usize {
        self.requesters.len()
    }

    /// Bytes the tracked keys retain. The cap that matters is this one, not the
    /// entry count: entries are bounded by [`MAX_TRACKED_REQUESTERS`], but the
    /// bytes inside them come off the wire, so a test that counts entries would
    /// pass while the map held gigabytes.
    pub fn tracked_bytes(&self) -> usize {
        self.requesters.keys().map(|k| k.len()).sum()
    }

    /// Asks committed against the hourly ceiling in the current window.
    pub fn global_committed(&self) -> u32 {
        self.global.count
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
    /// A keyed provider with no declared ceiling, reached through a gateway the
    /// operator interposed — a `base_url` that is neither this box nor the
    /// vendor's own API host.
    ///
    /// This is `kannaka-prime`'s shape, and a gateway is where budgets live: the
    /// operator put something in front of the vendor, and a virtual key with a
    /// cap is the usual reason. That is evidence, not proof, so this still says
    /// something — it just does not shout. Shouting at the one node everybody
    /// watches, about a key that *is* capped, is how a banner stops being read.
    UndeclaredGateway { base_url: String },
    /// A keyed provider with no declared ceiling, against the vendor's own API.
    /// This is the #932 exposure at its plainest: every anonymous broadcast this
    /// node answers spends the operator's money against no stated limit and
    /// nothing in between.
    Unbounded,
}

impl SpendPosture {
    /// The vendor-direct, no-ceiling case — the one that gets the loud banner.
    pub fn is_unbounded(&self) -> bool {
        matches!(self, SpendPosture::Unbounded)
    }

    /// This node can spend and has declared no ceiling *here*, whether or not
    /// something upstream caps it. What `KANNAKA_SERVE_REFUSE_UNBOUNDED` acts on.
    pub fn declares_no_ceiling(&self) -> bool {
        matches!(self, SpendPosture::Unbounded | SpendPosture::UndeclaredGateway { .. })
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
    if !is_vendor_api_host(&llm.base_url) {
        return SpendPosture::UndeclaredGateway { base_url: llm.base_url.clone() };
    }
    SpendPosture::Unbounded
}

/// Does this `base_url` point at the model vendor's own API, rather than at
/// something the operator put in front of it?
///
/// Empty counts as vendor: `client_from_config` fills in `api.anthropic.com` /
/// `api.openai.com` for an empty `base_url`, so an unset URL is the vendor
/// direct. Callers reach here only after the local check, so a gateway on this
/// box has already been classified as free.
pub fn is_vendor_api_host(base_url: &str) -> bool {
    let u = base_url.trim();
    if u.is_empty() {
        return true;
    }
    let after_scheme = u.split("://").nth(1).unwrap_or(u);
    let authority = after_scheme.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority
        .rsplit('@')
        .next()
        .unwrap_or(authority)
        .split(':')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    matches!(
        host.as_str(),
        "api.anthropic.com" | "api.openai.com" | "anthropic.com" | "openai.com"
    )
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

    /// `SERVING_HOPS` is process-wide and `cargo test` runs tests in parallel
    /// threads, so the two tests that write it must not interleave.
    static CELL_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
    fn a_forged_hop_count_cannot_become_not_serving() {
        let _guard = CELL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // The regression: `hops` clamped to u32::MAX, which was also the in-band
        // sentinel for "this process originated the ask". A wire value of
        // 4294967295 round-tripped back to None and handed the forger a fresh
        // hop budget. Both halves are closed — the clamp, and the sentinel.
        for forged in [u32::MAX as u64, u32::MAX as u64 + 1, u64::MAX, 9_999_999] {
            let h = hops_of(&json!({ "text": "hi", "hops": forged }));
            assert_eq!(h, HOPS_CLAMP, "wire {forged} must clamp below any sentinel");
            assert!(!may_forward(h), "a forged hop count must not be forwardable");
            set_serving_hops(Some(h));
            assert_eq!(
                serving_hops(),
                Some(h),
                "wire {forged} must not read back as `not serving`"
            );
            set_serving_hops(None);
        }
    }

    #[test]
    fn serving_hops_cell_round_trips() {
        let _guard = CELL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(serving_hops(), None, "idle process is not serving");
        set_serving_hops(Some(0));
        assert_eq!(serving_hops(), Some(0));
        set_serving_hops(Some(1));
        assert_eq!(serving_hops(), Some(1));
        set_serving_hops(Some(u32::MAX));
        assert_eq!(
            serving_hops(),
            Some(u32::MAX),
            "no u32 value may double as `not serving`"
        );
        set_serving_hops(None);
        assert_eq!(serving_hops(), None);
    }

    // ── 2. rate limit ──────────────────────────────────────────────────────

    #[test]
    fn requester_key_is_the_pair_of_inbox_and_declared_id() {
        assert_eq!(
            requester_key(Some("kannaka-prime"), Some("_INBOX.req.4242.deadbeef.17")),
            "inbox:_INBOX.req.4242+from:kannaka-prime"
        );
        // Neither half alone is an identity, so neither half alone is the key.
        assert_eq!(
            requester_key(Some("  "), Some("_INBOX.req.4242.cafe.18")),
            "inbox:_INBOX.req.4242+from:-"
        );
        assert_eq!(
            requester_key(Some("?"), Some("_INBOX.req.4242.cafe.18")),
            "inbox:_INBOX.req.4242+from:-",
            "the `?` placeholder is not an identity"
        );
        assert_eq!(
            requester_key(Some("a"), Some("_INBOX.req.4242.other.19")),
            requester_key(Some("a"), Some("_INBOX.req.4242.another.20")),
            "two requests from one process share an inbox prefix"
        );
        assert_ne!(
            requester_key(Some("a"), Some("_INBOX.req.4242.x.1")),
            requester_key(Some("a"), Some("_INBOX.req.9999.x.1")),
            "a different process is a different requester"
        );
    }

    #[test]
    fn claiming_another_peers_name_cannot_spend_its_quota() {
        // Keying on `from` alone was not merely evadable, it was AIMABLE: three
        // asks declaring `from = "kannaka-prime"` exhausted that peer's bucket.
        // The pair means a caller can only exhaust the bucket it actually owns.
        let victim_inbox = "_INBOX.req.1111.aaaa.1";
        let attacker_inbox = "_INBOX.req.2222.bbbb.1";
        let mut rl = ServeRateLimiter::new(3, 1000);

        for i in 0..3 {
            let forged = requester_key(Some("kannaka-prime"), Some(attacker_inbox));
            assert!(rl.check(&forged, 500).is_allow(), "forged ask {i}");
        }
        // The attacker has spent only its own bucket.
        assert!(
            !rl.check(
                &requester_key(Some("kannaka-prime"), Some(attacker_inbox)),
                500
            )
            .is_allow(),
            "the attacker must exhaust itself"
        );
        // The peer whose name was claimed is untouched.
        let victim = requester_key(Some("kannaka-prime"), Some(victim_inbox));
        for i in 0..3 {
            assert!(
                rl.check(&victim, 500).is_allow(),
                "the honest peer's ask {i} must still be answered"
            );
        }
    }

    #[test]
    fn a_reply_is_only_ever_sent_to_an_inbox() {
        // `reply_to` is caller-chosen, and a serving node authenticates with
        // publish `>` while anon is denied KANNAKA.work.> / KANNAKA.inbox.> and
        // the JetStream admin subjects. Reflecting through this handler must not
        // become a way to emit onto subjects the caller may not publish to.
        for ok in ["_INBOX.req.1.2.3", "_INBOX.x", "_INBOX.reqm.9.a.b"] {
            assert!(is_valid_reply_inbox(ok), "should be accepted: {ok}");
        }
        for bad in [
            // The subjects anon is DENIED publish on come first: if this guard
            // ever weakens, the failure should name the privileged reflection,
            // not an empty string.
            "KANNAKA.work.render",
            "KANNAKA.inbox.someone",
            "$JS.API.STREAM.DELETE.KANNAKA",
            "KANNAKA.ask.broadcast",
            "",
            "_INBOX.",
            "_INBOX.>",
            "_INBOX.*.x",
            "_INBOX.a b",
            "_INBOX.a\r\nPUB evil 0",
            " _INBOX.a",
        ] {
            assert!(!is_valid_reply_inbox(bad), "should be refused: {bad:?}");
        }
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
        // Five asks, each under a fresh identity and each reaching the model:
        // the per-requester limit never fires, and the global ceiling is the
        // only thing standing.
        for i in 0..5 {
            assert!(rl.check(&format!("from:sock{i}"), 1000).is_allow(), "ask {i}");
            rl.commit_global(1000);
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
    fn asks_that_never_reach_the_model_do_not_consume_global_budget() {
        // The outage the earlier revision created: `swarm serve` decides whether
        // to answer a broadcast with a resonance gate AFTER the limiter runs, and
        // anon may publish KANNAKA.ask.broadcast. If passing `check` spent the
        // hourly ceiling, a stranger publishing ~30-byte non-resonant asks — every
        // one dropped by that gate, none of them costing a token — would take the
        // public ask_kannaka off the air at one publish every twelve seconds.
        let mut rl = ServeRateLimiter::new(u32::MAX, 5);
        for i in 0..500 {
            assert!(
                rl.check(&format!("from:flood{i}"), 1000).is_allow(),
                "non-resonant ask {i} passes the limiter"
            );
            // ...and is then dropped by the resonance gate. No commit_global.
        }
        assert_eq!(
            rl.global_committed(),
            0,
            "an ask that never reached the model must not have spent the ceiling"
        );
        // The product is still on the air for everybody.
        for i in 0..5 {
            assert!(rl.check("from:honest", 1000).is_allow(), "honest ask {i}");
            rl.commit_global(1000);
        }
        assert!(
            !rl.check("from:honest", 1000).is_allow(),
            "and the ceiling still bounds what actually spends"
        );
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
    fn an_oversized_id_is_stored_as_a_bounded_hash() {
        // `from` comes off the wire and the broker's max_payload is 64MB, so the
        // id is attacker-SIZED, not just attacker-chosen. Counting entries would
        // pass while the map held gigabytes.
        let huge = "A".repeat(900_000);
        let key = requester_key(Some(&huge), None);
        assert!(
            key.len() <= MAX_REQUESTER_KEY_BYTES,
            "a 900KB id produced a {}-byte key",
            key.len()
        );
        assert!(id_is_oversized(&huge));
        assert!(!id_is_oversized("kannaka-prime"));
        // A hash, not a truncation: one caller must not be able to land in
        // another's bucket by sharing a prefix.
        let sibling = format!("{huge}-different-tail");
        assert_ne!(
            requester_key(Some(&huge), None),
            requester_key(Some(&sibling), None),
            "two long ids must stay two buckets"
        );
        assert_eq!(
            requester_key(Some(&huge), None),
            requester_key(Some(&huge), None),
            "and the same id must stay one bucket"
        );
        // Short ids are untouched, so the log stays readable.
        assert_eq!(
            requester_key(Some("kannaka-prime"), Some("_INBOX.req.7.a.b")),
            "inbox:_INBOX.req.7+from:kannaka-prime"
        );
        // The inbox fallback is bounded the same way — its tokens are wire-sized too.
        let huge_inbox = format!("_INBOX.req.{}", "9".repeat(900_000));
        assert!(requester_key(None, Some(&huge_inbox)).len() <= MAX_REQUESTER_KEY_BYTES);
    }

    #[test]
    fn identity_rotation_cannot_grow_the_limiter_beyond_a_byte_bound() {
        // Global ceiling above the tracking cap so the map, not the ceiling, is
        // what this test exercises.
        let mut rl = ServeRateLimiter::new(1, u32::MAX);
        // Fill it the way an attacker would: one huge distinct id per ask.
        let pad = "Z".repeat(4096);
        for i in 0..MAX_TRACKED_REQUESTERS {
            let key = requester_key(Some(&format!("{pad}{i}")), None);
            assert!(rl.check(&key, 1000).is_allow(), "ask {i}");
        }
        assert_eq!(rl.tracked(), MAX_TRACKED_REQUESTERS);
        // BYTES, not entries — the check has to be as strong as the thing it
        // checks. 4096 ids of 4KB each would be 16MiB retained; hashed, it is
        // two orders of magnitude less.
        let bytes = rl.tracked_bytes();
        assert!(
            bytes <= MAX_TRACKED_REQUESTERS * MAX_REQUESTER_KEY_BYTES,
            "limiter retained {bytes} bytes, over the {} byte bound",
            MAX_TRACKED_REQUESTERS * MAX_REQUESTER_KEY_BYTES
        );
        assert!(
            bytes < 4096 * MAX_TRACKED_REQUESTERS / 8,
            "retained {bytes} bytes — the cap is not actually bounding the keys"
        );
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
        // The live kannaka-prime config, EXACTLY as it upgrades: the new
        // `externally_capped` field defaults to false, so nothing in the file
        // has changed. It must not draw the loud banner — prime is the one node
        // everybody watches, and a false alarm there is how a banner stops being
        // read. A gateway the operator interposed is evidence of a budget.
        let mut c = llm("openai", "kannaka-claude-haiku", "https://ninja-portal.com/v1");
        assert!(!c.externally_capped, "the field is new — prime upgrades with it unset");
        match spend_posture(&c, true) {
            SpendPosture::UndeclaredGateway { base_url } => {
                assert_eq!(base_url, "https://ninja-portal.com/v1");
            }
            other => panic!("expected UndeclaredGateway, got {other:?}"),
        }
        assert!(
            !spend_posture(&c, true).is_unbounded(),
            "prime must not draw the vendor-direct banner"
        );
        assert!(
            spend_posture(&c, true).declares_no_ceiling(),
            "it still has not declared one here, and the notice must say so"
        );
        // Once the operator sets the flag, it is settled outright.
        c.externally_capped = true;
        match spend_posture(&c, true) {
            SpendPosture::Capped { how } => assert!(how.contains("externally_capped"), "{how}"),
            other => panic!("expected Capped, got {other:?}"),
        }
        assert!(!spend_posture(&c, true).declares_no_ceiling());
    }

    #[test]
    fn a_vendor_direct_key_still_gets_the_loud_banner() {
        // The gateway carve-out must not swallow the plain case it exists beside.
        for base in ["", "https://api.anthropic.com", "https://api.openai.com/v1"] {
            let c = llm(
                if base.contains("openai") { "openai" } else { "anthropic" },
                "m",
                base,
            );
            assert_eq!(
                spend_posture(&c, true),
                SpendPosture::Unbounded,
                "a vendor-direct key with no ceiling is the plain #932 exposure: {base:?}"
            );
        }
        assert!(is_vendor_api_host(""));
        assert!(is_vendor_api_host("https://API.OpenAI.com/v1"));
        assert!(!is_vendor_api_host("https://ninja-portal.com/v1"));
        assert!(!is_vendor_api_host("https://api.openai.com.evil.example/v1"));
    }

    #[test]
    fn a_local_brain_never_warns() {
        // The installer writes provider = "openai" for the local Ollama brain,
        // so the base URL has to be what decides. `declares_no_ceiling` is the
        // assertion, not `is_unbounded`: a local node must draw NEITHER notice.
        let local_openai = llm("openai", "kannaka-brain", "http://localhost:11434/v1");
        assert!(!spend_posture(&local_openai, true).declares_no_ceiling());
        assert!(!spend_posture(&llm("ollama", "kannaka-brain", ""), true).declares_no_ceiling());
        assert!(!spend_posture(&llm("none", "", ""), true).declares_no_ceiling());
        assert!(!spend_posture(&llm("", "", ""), false).declares_no_ceiling());
        assert!(
            !spend_posture(&llm("anthropic", "claude-sonnet-5", ""), false).declares_no_ceiling(),
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
