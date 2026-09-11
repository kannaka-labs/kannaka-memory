//! NATS real-time transport for QueenSync phase gossip.
//!
//! Implements a minimal NATS client using raw TCP (the `nats` crate is broken
//! with rand 0.9). Supports PUB/SUB for phase announcements, JetStream KV
//! for agent registry / discovery, structured event streams, and automatic
//! reconnection with message buffering.
//!
//! Connection model: one TCP socket per `SwarmTransport`, guarded by a single
//! mutex holding both the write half and a persistent `BufReader` (the reader
//! must persist across calls — recreating it per call discards any bytes it
//! pre-read into its internal buffer and desyncs the protocol). Long-running
//! subscriptions clone the socket and own their own reader; while one is open
//! the transport's RPC methods MUST NOT be used on the same connection (the
//! two readers would steal each other's bytes) — serving agents open a
//! dedicated `SwarmTransport` per subscription.
//!
//! Subject layout:
//! - `QUEEN.phase.<agent_id>` — each agent's latest phase (publish per agent)
//! - `QUEEN.phase.*` — wildcard subscribe to get all phases
//! - `QUEEN.event.<type>` — structured events (join, leave, dream.start, etc.)
//! - `QUEEN.announce` — legacy join/leave (still published for compat)

use std::collections::HashMap;
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::Utc;
use crate::queen::AgentPhase;

/// Inject the canonical NATS envelope fields (schema_version, ts) into a
/// JSON object before publish, matching the contract enforced by the radio
/// + observatory validators (consciousness-core/docs/nats-contract.yaml).
/// agent_id is left to the caller — most payloads already have it. If the
/// payload has a "timestamp" field, mirror it to "ts" (the contract name);
/// otherwise stamp the current UTC time. Old subscribers keep seeing the
/// existing fields untouched; new validators see the envelope they want.
/// Is this presence record a live agent, as opposed to a retraction
/// tombstone written by `publish_presence_left` (#590)?
///
/// **Fails OPEN by design.** Only an explicit `status == "left"` is treated as
/// a retraction; a record with no `status` at all is live. Every presence
/// record written before #590 lacks the field entirely, and a filter that
/// required `status == "active"` would hide the entire existing swarm the
/// moment this shipped — a far worse failure than a lingering ghost.
fn is_active_presence(p: &serde_json::Value) -> bool {
    p.get("status").and_then(|v| v.as_str()) != Some("left")
}

/// How long a presence record keeps counting as a live peer without a refresh
/// (#737).
///
/// `swarm join` heartbeats every 30s by default (floor 5s), so five minutes is
/// a 10x margin: a loaded or briefly-partitioned node is never wrongly hidden,
/// while a node that CRASHED — and therefore never wrote the `status: "left"`
/// tombstone `is_active_presence` looks for — stops being advertised in
/// minutes instead of lingering for the presence stream's 24h `max_age`. It is
/// the same window `live_phase_agents` already applies to `QUEEN.phase`, so the
/// two liveness surfaces cannot disagree about who is here.
pub const PRESENCE_LIVE_WINDOW_SECS: i64 = 300;

/// Read the live window, honouring the `KANNAKA_PRESENCE_LIVE_SECS` escape
/// hatch. A deployment whose agents beacon on a slow cron rather than the
/// 30s heartbeat can widen it; `0` disables freshness filtering entirely and
/// restores the pre-#737 "everything retained is live" behaviour.
fn presence_live_window_secs() -> i64 {
    std::env::var("KANNAKA_PRESENCE_LIVE_SECS")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|v| *v >= 0)
        .unwrap_or(PRESENCE_LIVE_WINDOW_SECS)
}

/// Seconds since this record's `last_seen` heartbeat stamp, or `None` when it
/// carries no parsable one. Negative for a stamp in the future (clock skew).
pub fn presence_age_secs(p: &serde_json::Value, now: chrono::DateTime<Utc>) -> Option<i64> {
    let seen = p.get("last_seen").and_then(|v| v.as_str())?;
    let seen = chrono::DateTime::parse_from_rfc3339(seen)
        .ok()?
        .with_timezone(&Utc);
    Some(now.signed_duration_since(seen).num_seconds())
}

/// Is this presence record fresh enough to present as a reachable peer (#737)?
///
/// **Fails OPEN**, on the same reasoning as [`is_active_presence`]: a record
/// with no parsable `last_seen` reads as live. Agents predating the field
/// would otherwise vanish from the swarm the moment this shipped, which is a
/// worse failure than showing one stale entry.
pub fn is_fresh_presence(p: &serde_json::Value, now: chrono::DateTime<Utc>) -> bool {
    let window = presence_live_window_secs();
    if window == 0 {
        return true;
    }
    match presence_age_secs(p, now) {
        Some(age) => age < window,
        None => true,
    }
}

fn add_envelope(value: &mut serde_json::Value) {
    if let Some(obj) = value.as_object_mut() {
        // Per consciousness-core/docs/nats-contract.yaml:
        //   schema_version: string ("1.0")
        //   ts:             number (unix-ms)
        // Pre-fix these were "1" (string but wrong value) and an RFC3339
        // string respectively; closes #82 and the same defects #90/#91
        // flag in the JetStream events / six bypass publishers.
        obj.entry("schema_version".to_string())
            .or_insert_with(|| serde_json::Value::String("1.0".to_string()));
        if !obj.contains_key("ts") {
            // Promote an existing RFC3339 `timestamp` field to unix-ms.
            let ts = obj
                .get("timestamp")
                .and_then(|v| v.as_str())
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.timestamp_millis())
                .unwrap_or_else(|| Utc::now().timestamp_millis());
            obj.insert(
                "ts".to_string(),
                serde_json::Value::Number(serde_json::Number::from(ts)),
            );
        }
    }
}

/// Identity block attached to swarm announces/presence when the operator
/// has logged in via `kannaka identity` (swarm agent identity, step 2).
/// user_id + email ONLY — tokens never cross the NATS boundary.
///
/// Compatibility: the block rides as an OPTIONAL `"identity"` key on
/// payloads that production v0.6.19/0.6.20 agents parse as loose JSON
/// (`serde_json::Value`), so old agents ignore it and new agents see
/// `None` on old payloads. With no stored identity the payloads are
/// byte-identical to the pre-identity shape.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AnnounceIdentity {
    pub user_id: String,
    pub email: String,
}

impl AnnounceIdentity {
    /// Build from the stored identity file (`<data_dir>/identity.json`).
    /// Not logged in → `None`. Present-but-unreadable → warn and `None`
    /// (the swarm announce must never fail because of a bad identity file).
    pub fn from_store() -> Option<Self> {
        let path = crate::identity::identity_path();
        match crate::identity::IdentityStore::load(&path) {
            Ok(Some(s)) => Some(Self { user_id: s.user_id, email: s.email }),
            Ok(None) => None,
            Err(e) => {
                eprintln!("[nats] Warning: ignoring unreadable identity file: {e}");
                None
            }
        }
    }

    /// Insert the `"identity": {user_id, email}` block into a JSON object
    /// payload. No-op on non-objects.
    pub fn attach_to(&self, payload: &mut serde_json::Value) {
        if let Some(obj) = payload.as_object_mut() {
            obj.insert(
                "identity".to_string(),
                serde_json::json!({ "user_id": self.user_id, "email": self.email }),
            );
        }
    }

    /// Read the identity email out of an announce/presence payload.
    /// Old-shape payloads (no `"identity"` key) → `None`.
    pub fn email_from(payload: &serde_json::Value) -> Option<&str> {
        payload.get("identity")?.get("email")?.as_str()
    }
}

/// Build the legacy `QUEEN.announce` payload (pre-envelope), optionally
/// enriched with the identity block. `identity = None` reproduces the
/// pre-identity shape exactly.
fn legacy_announce_payload(
    event: &str,
    agent_id: &str,
    identity: Option<&AnnounceIdentity>,
) -> serde_json::Value {
    let mut payload = serde_json::json!({
        "event": event,
        "agent_id": agent_id,
    });
    if let Some(idn) = identity {
        idn.attach_to(&mut payload);
    }
    payload
}

pub const DEFAULT_NATS_URL: &str = "nats://swarm.ninja-portal.com:4222";
const STREAM_NAME: &str = "QUEEN_PHASES";
const EVENTS_STREAM_NAME: &str = "QUEEN_EVENTS";

/// Maximum number of messages to buffer during disconnect.
const PUBLISH_BUFFER_LIMIT: usize = 100;

/// Default socket read/write timeout used outside explicit RPC windows.
const DEFAULT_IO_TIMEOUT: Duration = Duration::from_secs(5);

/// Default per-call timeout for JetStream API request/reply exchanges.
const JS_API_TIMEOUT: Duration = Duration::from_secs(3);

/// Upper bound on messages pulled by a single JetStream stream walk.
const STREAM_WALK_LIMIT: usize = 10_000;

/// Reverse lookup table for the standard base64 alphabet. 0xFF = invalid.
const BASE64_REV: [u8; 256] = {
    const TABLE: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut rev = [0xFFu8; 256];
    let mut i = 0;
    while i < 64 {
        rev[TABLE[i] as usize] = i as u8;
        i += 1;
    }
    rev
};

/// Minimal base64 decoder (standard alphabet, with padding).
fn base64_decode(input: &str) -> Result<Vec<u8>, NatsError> {
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let mut buf: u32 = 0;
    let mut bits: u32 = 0;
    for &b in input.as_bytes() {
        if b == b'=' || b == b'\n' || b == b'\r' {
            continue;
        }
        let val = BASE64_REV[b as usize];
        if val == 0xFF {
            return Err(NatsError::Protocol(format!(
                "invalid base64 char: {}",
                b as char
            )));
        }
        buf = (buf << 6) | val as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    Ok(out)
}

/// Errors from the NATS transport layer.
#[derive(Debug)]
pub enum NatsError {
    Connect(String),
    Io(std::io::Error),
    Protocol(String),
    Serialize(String),
    Disconnected(String),
    KvNotFound(String),
}

impl std::fmt::Display for NatsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Connect(msg) => write!(f, "NATS connect: {msg}"),
            Self::Io(e) => write!(f, "NATS I/O: {e}"),
            Self::Protocol(msg) => write!(f, "NATS protocol: {msg}"),
            Self::Serialize(msg) => write!(f, "NATS serialize: {msg}"),
            Self::Disconnected(msg) => write!(f, "NATS disconnected: {msg}"),
            Self::KvNotFound(key) => write!(f, "NATS KV key not found: {key}"),
        }
    }
}

impl std::error::Error for NatsError {}

impl From<std::io::Error> for NatsError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

// ---------------------------------------------------------------------------
// SharedWavefront protocol (QS-5, #56)
// ---------------------------------------------------------------------------

/// A wavefront with routing metadata for resonance-based memory sharing.
///
/// Communication speed 2 (medium): shared memories propagate via constructive
/// interference during dreams. Faster than Dolt merges, slower than NATS gossip.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SharedWavefront {
    /// Source agent that created this wavefront.
    pub source_agent: String,
    /// Target agent ID, or None for broadcast to all peers.
    pub target: Option<String>,
    /// Minimum cosine similarity required for the target to absorb this wavefront.
    pub resonance_threshold: f32,
    /// Dream cycles before expiry (shared wavefronts fade if never absorbed).
    pub ttl: u32,
    /// The wavefront vector (high-dimensional embedding).
    pub vector: Vec<f32>,
    /// Content/description of the shared memory.
    pub content: String,
    /// Amplitude/importance of the shared memory.
    pub amplitude: f32,
    /// Modality tag.
    pub modality: String,
    /// Timestamp of creation.
    pub created_at: String,
}

impl SharedWavefront {
    /// Create a new shared wavefront for broadcasting.
    pub fn broadcast(
        source_agent: &str,
        vector: Vec<f32>,
        content: String,
        amplitude: f32,
        modality: &str,
    ) -> Self {
        Self {
            source_agent: source_agent.to_string(),
            target: None,
            resonance_threshold: 0.4,
            ttl: 7,
            vector,
            content,
            amplitude,
            modality: modality.to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
        }
    }

    /// Create a shared wavefront targeted at a specific agent.
    pub fn targeted(
        source_agent: &str,
        target_agent: &str,
        vector: Vec<f32>,
        content: String,
        amplitude: f32,
        resonance_threshold: f32,
    ) -> Self {
        Self {
            source_agent: source_agent.to_string(),
            target: Some(target_agent.to_string()),
            resonance_threshold,
            ttl: 7,
            vector,
            content,
            amplitude,
            modality: "unknown".to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
        }
    }

    /// Check if this wavefront has expired (ttl decremented each dream cycle).
    pub fn is_expired(&self) -> bool {
        self.ttl == 0
    }

    /// Decrement TTL by one dream cycle.
    pub fn tick(&mut self) {
        self.ttl = self.ttl.saturating_sub(1);
    }
}

/// Parse a NATS URL into (host, port, optional (user, password)).
///
/// Accepts `nats://host`, `nats://host:port`, `nats://user:pass@host:port`,
/// and the same forms without the scheme.
fn parse_nats_url(url: &str) -> Result<(String, u16, Option<(String, String)>), NatsError> {
    let stripped = url.strip_prefix("nats://").unwrap_or(url);
    let (creds, hostport) = match stripped.rsplit_once('@') {
        Some((c, hp)) => {
            let (user, pass) = c.split_once(':').unwrap_or((c, ""));
            if user.is_empty() || pass.is_empty() {
                (None, hp)
            } else {
                (Some((user.to_string(), pass.to_string())), hp)
            }
        }
        None => (None, stripped),
    };
    let parts: Vec<&str> = hostport.split(':').collect();
    match parts.len() {
        1 => Ok((parts[0].to_string(), 4222, creds)),
        2 => {
            let port = parts[1]
                .parse::<u16>()
                .map_err(|e| NatsError::Connect(format!("invalid port: {e}")))?;
            Ok((parts[0].to_string(), port, creds))
        }
        _ => Err(NatsError::Connect(format!("invalid NATS URL: {url}"))),
    }
}

/// Generate a unique inbox subject. Combines pid, a random uuid, and a
/// timestamp so two processes (or two threads in one process) starting a
/// request in the same instant cannot collide and cross-read replies.
fn new_inbox(tag: &str) -> String {
    use std::time::SystemTime;
    let nonce = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!(
        "_INBOX.{}.{}.{}.{}",
        tag,
        std::process::id(),
        uuid::Uuid::new_v4().simple(),
        nonce
    )
}

/// True for server `-ERR` messages that mean this connection is unusable.
fn is_auth_error(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    m.contains("authorization") || m.contains("authentication")
}

/// True for a per-subject ACL refusal (`Permissions Violation for
/// Publish/Subscription to "X"`).
///
/// Deliberately NOT folded into [`is_auth_error`] (#562): an authorization
/// failure means the whole connection is dead, whereas a permissions violation
/// leaves it perfectly usable for every other subject. Conflating them would
/// tear down a working swarm connection over one denied subject. This is an
/// OPERATION error, not a connection error.
fn is_permissions_error(msg: &str) -> bool {
    msg.to_ascii_lowercase().contains("permissions violation")
}

/// Pull the subject out of `... for Subscription to "KANNAKA.memory.new" ...`.
///
/// Needed for multi-subject subscriptions, where the client cannot know which
/// of the subjects it bundled was the one refused — only the server's own
/// message says.
fn subject_from_permissions_error(msg: &str) -> Option<String> {
    let start = msg.find('"')? + 1;
    let rest = &msg[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Turn a server permissions refusal into an error an operator can act on.
///
/// Pre-#562 these arrived as an async `-ERR`, were printed as a stray
/// `[nats] server error: …` line, and the operation returned `Ok` — so
/// `swarm serve` reported itself subscribed to a subject the broker had
/// refused, and sat deaf forever. Naming the subject, and the credentials file
/// when the connection is anonymous, is the difference between a two-minute
/// fix and an afternoon.
/// Whether a connection should attempt `$JS.API.STREAM.CREATE` at all.
///
/// An anonymous connection is *structurally* denied it — ADR-0042 closed anon's
/// control lane — so the create cannot succeed, and issuing it purely to learn
/// that costs a broker-side "Permissions Violation" on every connection. On the
/// live swarm that was ~113 denials a minute, 79 in 83 of them anon
/// (2026-08-23); the useful information (can I READ the stream?) comes from
/// `stream_readable` instead, which anon IS granted.
///
/// Authenticated identities still attempt it: that is what creates the stream
/// on a fresh cluster, syncs its config, and proves `jetstream_writable`. An
/// authenticated identity that is *also* denied create (e.g. `serve`) still
/// logs one denial per connection — the broker is the right place to learn
/// that, and it is rare enough to be signal rather than noise.
fn should_attempt_stream_create(authenticated: bool) -> bool {
    authenticated
}

/// True for a `Permissions Violation` that names the JetStream stream-create
/// subject — the broker telling us, in its own words, that this connection may
/// not create streams.
fn names_denied_stream_create(msg: &str) -> bool {
    is_permissions_error(msg) && msg.contains("$JS.API.STREAM.CREATE")
}

/// Whether to attempt `$JS.API.STREAM.CREATE` for the PRESENCE stream (#933).
///
/// `should_attempt_stream_create` gates on "we supplied credentials", which is
/// not the same question as "the broker demands them". Against an OPEN broker
/// — `nats-server -js` on a developer's laptop, a throwaway CI bus, the first
/// boot of a new swarm — an unauthenticated client is allowed to create the
/// stream, and refusing to try means `KANNAKA_PRESENCE` is never created at
/// all and `swarm peers` stays empty forever.
///
/// So the gate is "is the create known to be denied?", not "are we anonymous?":
///
///   - authenticated: attempt it, as before, unless this connection has
///     already been refused once;
///   - anonymous and the stream is ALREADY THERE (the production swarm, where
///     anon is denied the control lane by ADR-0042): nothing to create, and
///     issuing it would only buy a broker-side "Permissions Violation" per
///     join — the ~113-a-minute noise #928 removed;
///   - anonymous and the stream is ABSENT: try. On an open broker this is the
///     bootstrap; on a closed one it costs a single denial, which is then
///     remembered for the life of the connection.
fn should_attempt_presence_create(
    authenticated: bool,
    create_denied_here: bool,
    stream_absent: bool,
) -> bool {
    if create_denied_here {
        return false;
    }
    authenticated || stream_absent
}

/// JetStream API error code for "stream not found".
const JS_ERR_STREAM_NOT_FOUND: u64 = 10059;

/// Read a JetStream API reply as evidence about the STREAM rather than the
/// request (#928): a clean reply, or an error about anything but the stream's
/// existence (10037 "no message found" is the usual one from a MSG.GET probe),
/// means the stream is there; 10059 means it is not.
fn js_reply_names_an_existing_stream(reply: &serde_json::Value) -> bool {
    match reply.get("error") {
        None => true,
        Some(err) => err.get("err_code").and_then(|c| c.as_u64()) != Some(JS_ERR_STREAM_NOT_FOUND),
    }
}

/// The line `swarm join` prints when this identity could not create the
/// presence stream (#928). Only when the stream is genuinely absent is
/// presence actually dropped; when it exists, the node is listed by other
/// hosts — an anonymous member as `(unverified)`, which is what anonymous
/// membership costs, not invisibility.
pub fn presence_stream_notice(create_err: &str, stream_exists: bool, authenticated: bool) -> String {
    if stream_exists {
        if authenticated {
            "[nats] presence stream present (read-only identity); this agent will be listed by other hosts"
                .to_string()
        } else {
            "[nats] presence stream present (read-only identity); this agent will be listed by other \
             hosts as (unverified) — anonymous membership"
                .to_string()
        }
    } else {
        format!(
            "[nats] WARNING: presence stream unavailable ({create_err}) — this agent will NOT appear \
             in `swarm peers`. Presence publishes will be accepted by the broker and dropped."
        )
    }
}

fn permissions_error(op: &str, subject: &str, raw: &str, authenticated: bool) -> NatsError {
    let hint = if authenticated {
        "this connection IS authenticated, so the broker's ACL does not grant this subject to your user"
    } else {
        "this connection is ANONYMOUS — the subject requires authenticated NATS credentials (~/.kannaka-nats.env)"
    };
    NatsError::Protocol(format!(
        "{op} denied by broker for \"{subject}\": {hint} (server said: {raw})"
    ))
}

// ---------------------------------------------------------------------------
// Wire-protocol frame reader
// ---------------------------------------------------------------------------

/// One parsed NATS protocol frame.
enum Frame {
    Msg {
        subject: String,
        sid: String,
        reply_to: Option<String>,
        payload: Vec<u8>,
    },
    Ping,
    Pong,
    OkAck,
    Info,
    ServerErr(String),
}

/// Outcome of one frame-read attempt.
enum ReadOutcome {
    Frame(Frame),
    /// Clean read timeout at a frame boundary — no bytes were consumed,
    /// the stream is still in sync. Safe to poll again.
    TimedOut,
    /// EOF — the server closed the connection.
    Closed,
}

/// Ceiling on a single MSG payload allocation. The NATS server default
/// max_payload is 1 MiB and nothing in the constellation raises it past
/// that; 8 MiB leaves generous headroom while making `MSG x 1 99999999999`
/// from a compromised/desynced peer a protocol error instead of an
/// allocation abort (#501).
const MAX_MSG_PAYLOAD: usize = 8 * 1024 * 1024;

/// Ceiling on one control line. INFO with auth/cluster metadata runs a few
/// KB; MSG headers and -ERR lines are tiny. A line that hasn't ended after
/// 16 KiB is not NATS protocol traffic.
const MAX_CONTROL_LINE: u64 = 16 * 1024;

/// Read one control line into `out`, BOUNDED to `MAX_CONTROL_LINE` bytes. A peer
/// (or MITM) that never sends `\n` — a desynced/garbage/hostile server — would
/// otherwise grow `out` without bound and OOM us. `take` caps the pull; a line
/// that hits the cap without a newline is a protocol desync. Returns bytes read.
fn read_control_line<R: BufRead>(reader: &mut R, out: &mut String) -> Result<usize, NatsError> {
    let n = {
        let mut limited = (&mut *reader).take(MAX_CONTROL_LINE);
        limited.read_line(out)?
    };
    if n as u64 >= MAX_CONTROL_LINE && !out.ends_with('\n') {
        return Err(NatsError::Protocol(format!(
            "control line exceeds {MAX_CONTROL_LINE} bytes without newline — protocol desync"
        )));
    }
    Ok(n)
}

/// How long the payload (+trailing CRLF) of a single MSG frame may take to
/// arrive once its header line has been read. The byte count is KNOWN at this
/// point, so a read timeout mid-payload is a transient stall — a lost TCP
/// segment has an RTO of 200ms–1s+, and a multi-KB ask/recall reply spans
/// several segments — NOT a desync. We ride timeouts out up to this bound
/// (#499). Before the fix, `swarm serve`'s 250ms socket timeout turned any
/// such stall into a fatal "short MSG payload read" and killed the daemon on
/// one WAN retransmission. Only after this whole budget elapses with zero
/// progress is the stream declared dead.
const MSG_FRAME_DEADLINE: Duration = Duration::from_secs(30);

/// Why a `read_exact_resumable` did not fill its buffer.
#[derive(Debug)]
enum ResumableReadError {
    /// The peer closed (read returned 0) partway through — the stream cannot
    /// be resynced, so the connection is dead.
    Eof,
    /// The overall frame deadline passed with the buffer still unfilled: the
    /// payload never finished arriving. Dead.
    Timeout,
    /// A non-timeout I/O error.
    Io(std::io::Error),
}

/// Fill `buf` completely, tolerating transient read timeouts.
///
/// `Read::read_exact` maps a `TimedOut`/`WouldBlock` to a fatal
/// `UnexpectedEof`-class error. For a NATS MSG payload the byte count is known
/// up front, so a timeout mid-payload is resumable: loop `read()` into the
/// unfilled tail, treating `TimedOut`/`WouldBlock`/`Interrupted` as "no
/// progress this poll — try again" until the buffer fills or `deadline`
/// passes. `Ok(0)` is a real EOF (peer closed mid-payload) and IS fatal — the
/// byte stream can no longer be resynced (#499).
fn read_exact_resumable<R: Read>(
    reader: &mut R,
    buf: &mut [u8],
    deadline: Instant,
) -> Result<(), ResumableReadError> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => return Err(ResumableReadError::Eof),
            Ok(n) => filled += n,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::Interrupted
                ) =>
            {
                if Instant::now() >= deadline {
                    return Err(ResumableReadError::Timeout);
                }
                // else: transient stall — poll again until the deadline.
            }
            Err(e) => return Err(ResumableReadError::Io(e)),
        }
    }
    Ok(())
}

/// Read exactly one protocol frame. Returns `Err` on any condition that
/// desyncs the byte stream (partial line consumed before a timeout, short
/// payload read, unparseable MSG header, missing CRLF, non-UTF-8 control
/// line) — after such an error the connection must be considered dead.
fn read_frame(reader: &mut BufReader<TcpStream>) -> Result<ReadOutcome, NatsError> {
    let mut line = String::new();
    // Bound control-line growth: `take` caps how many bytes this read_line
    // can pull, so a stream that never sends '\n' (desync, garbage peer)
    // errors out instead of growing `line` without bound.
    let read_result = {
        let mut limited = (&mut *reader).take(MAX_CONTROL_LINE);
        limited.read_line(&mut line)
    };
    if let Ok(n) = read_result {
        if n as u64 >= MAX_CONTROL_LINE && !line.ends_with('\n') {
            return Err(NatsError::Protocol(format!("control line exceeds {MAX_CONTROL_LINE} bytes without newline — protocol desync")));
        }
    }
    match read_result {
        Ok(0) => return Ok(ReadOutcome::Closed),
        Ok(_) => {}
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
            ) =>
        {
            if line.is_empty() {
                return Ok(ReadOutcome::TimedOut);
            }
            // Bytes of a control line were consumed before the timeout hit;
            // they are lost and the stream is desynced. Fatal.
            return Err(NatsError::Protocol(format!(
                "read timed out mid-frame ({} bytes consumed) — protocol desync",
                line.len()
            )));
        }
        Err(e) => return Err(NatsError::Io(e)),
    }

    let trimmed = line.trim();
    if let Some(rest) = trimmed.strip_prefix("MSG ") {
        // Wire format:
        //   MSG <subject> <sid> <#bytes>
        //   MSG <subject> <sid> <reply-to> <#bytes>
        let parts: Vec<&str> = rest.split_whitespace().collect();
        let (subject, sid, reply_to, nbytes_str) = match parts.len() {
            3 => (parts[0], parts[1], None, parts[2]),
            4 => (parts[0], parts[1], Some(parts[2]), parts[3]),
            n => {
                return Err(NatsError::Protocol(format!("malformed MSG header ({n} fields): {trimmed}")))
            }
        };
        let nbytes: usize = nbytes_str.parse().map_err(|_| {
            NatsError::Protocol(format!("invalid MSG byte count: {trimmed}"))
        })?;
        if nbytes > MAX_MSG_PAYLOAD {
            // Wire-controlled size: allocating it unchecked lets one bogus
            // header abort the process. Nothing legitimate approaches this
            // (server max_payload defaults to 1 MiB).
            return Err(NatsError::Protocol(format!("MSG payload {nbytes} exceeds {MAX_MSG_PAYLOAD} byte cap — refusing allocation")));
        }
        // Resumable payload read: the byte count is known, so a mid-payload
        // read timeout (a lost segment straddling a short socket timeout) is
        // ridden out to MSG_FRAME_DEADLINE rather than treated as a desync
        // (#499). One shared deadline covers the payload AND the trailing CRLF.
        let frame_deadline = Instant::now() + MSG_FRAME_DEADLINE;
        let mut payload = vec![0u8; nbytes];
        match read_exact_resumable(reader, &mut payload, frame_deadline) {
            Ok(()) => {}
            // Peer closed mid-payload — a genuine connection close, so hand the
            // caller a clean Closed (reconnect/exit) rather than a desync error.
            Err(ResumableReadError::Eof) => return Ok(ReadOutcome::Closed),
            Err(ResumableReadError::Timeout) => {
                return Err(NatsError::Protocol(format!(
                    "MSG payload stalled past {MSG_FRAME_DEADLINE:?} — connection dead"
                )))
            }
            Err(ResumableReadError::Io(e)) => return Err(NatsError::Io(e)),
        }
        let mut crlf = [0u8; 2];
        match read_exact_resumable(reader, &mut crlf, frame_deadline) {
            Ok(()) => {}
            Err(ResumableReadError::Eof) => return Ok(ReadOutcome::Closed),
            Err(ResumableReadError::Timeout) => {
                return Err(NatsError::Protocol(
                    "MSG trailing CRLF stalled — connection dead".to_string(),
                ))
            }
            Err(ResumableReadError::Io(e)) => return Err(NatsError::Io(e)),
        }
        if &crlf != b"\r\n" {
            return Err(NatsError::Protocol(
                "MSG payload not followed by CRLF".to_string(),
            ));
        }
        return Ok(ReadOutcome::Frame(Frame::Msg {
            subject: subject.to_string(),
            sid: sid.to_string(),
            reply_to: reply_to.map(String::from),
            payload,
        }));
    }
    if trimmed == "PING" {
        return Ok(ReadOutcome::Frame(Frame::Ping));
    }
    if trimmed == "PONG" {
        return Ok(ReadOutcome::Frame(Frame::Pong));
    }
    if trimmed == "+OK" {
        return Ok(ReadOutcome::Frame(Frame::OkAck));
    }
    if let Some(rest) = trimmed.strip_prefix("-ERR") {
        return Ok(ReadOutcome::Frame(Frame::ServerErr(
            rest.trim().trim_matches('\'').to_string(),
        )));
    }
    if trimmed == "INFO" || trimmed.starts_with("INFO ") {
        return Ok(ReadOutcome::Frame(Frame::Info));
    }
    Err(NatsError::Protocol(format!(
        "unexpected protocol line: {}",
        trimmed.chars().take(80).collect::<String>()
    )))
}

/// Write one PUB frame (header + payload + CRLF) and flush, through the
/// dead-flag choke point. Building the frame in one buffer means a failure
/// is all-or-mostly-nothing at the syscall level, and `write_frames` marks
/// the Conn dead on any partial landing either way.
fn write_pub(conn: &mut Conn, subject: &str, payload: &[u8]) -> Result<(), NatsError> {
    let mut frame = Vec::with_capacity(payload.len() + subject.len() + 32);
    let _ = write!(frame, "PUB {} {}\r\n", subject, payload.len());
    frame.extend_from_slice(payload);
    frame.extend_from_slice(b"\r\n");
    conn.write_frames(&frame)
}

/// The two halves of one NATS connection. The reader is persistent: it must
/// never be recreated mid-connection because `BufReader` may have pre-read
/// bytes past the kernel cursor; dropping it would silently discard them.
struct Conn {
    writer: TcpStream,
    reader: BufReader<TcpStream>,
    /// Set when a write failed partway through a frame (e.g. a write timeout
    /// after the PUB header but before the payload). The outbound byte stream
    /// is desynced at that point: any further bytes — including a buffered
    /// replay — would be parsed by the server as the remainder of the
    /// truncated frame, silently publishing garbage. A dead Conn must never
    /// be written again; `reconnect()` replaces it wholesale.
    dead: bool,
    /// Did this connection's CONNECT present credentials? Used only to phrase
    /// a permissions refusal helpfully (#562) — an anonymous connection gets
    /// pointed at `~/.kannaka-nats.env`. Never used to PREDICT what the broker
    /// will allow: an unauthenticated local broker permits everything, and the
    /// committed server config has drifted from deployed reality in both
    /// directions, so the broker's own answer is the only reliable source.
    authenticated: bool,
    /// Has THIS broker refused `$JS.API.STREAM.CREATE` on THIS connection?
    ///
    /// The broker's own answer, which is the only reliable one (#933). Set
    /// when a `Permissions Violation` naming the create subject arrives, and
    /// gone when the connection is replaced by `reconnect()`. Used to stop
    /// re-issuing a create that is known to be denied, rather than to guess
    /// from "we supplied no credentials" that it would be.
    stream_create_denied: bool,
}

impl Conn {
    fn set_read_timeout(&self, t: Option<Duration>) -> Result<(), NatsError> {
        // writer/reader are clones of the same socket; the timeout is a
        // property of the shared socket, so setting it once is enough.
        self.writer.set_read_timeout(t).map_err(NatsError::Io)
    }

    /// Current read timeout, falling back to the connection default if the
    /// getter fails.
    fn read_timeout(&self) -> Option<Duration> {
        self.writer.read_timeout().unwrap_or(Some(DEFAULT_IO_TIMEOUT))
    }

    fn pong(&mut self) -> Result<(), NatsError> {
        self.write_frames(b"PONG\r\n")
    }

    /// Round-trip a PING and fail if the server refused `subject` in the
    /// meantime (#562).
    ///
    /// NATS answers a denied SUB/PUB with an ASYNC `-ERR Permissions
    /// Violation`, so a fire-and-forget operation returns `Ok` on something the
    /// broker threw away — `swarm serve` announced itself subscribed to
    /// `KANNAKA.ask.*` and then sat deaf forever. PING/PONG is ordered behind
    /// the preceding frame, so by the time PONG arrives any refusal of it has
    /// already been delivered. Same pattern `handshake` uses.
    ///
    /// Errors are scoped to this operation: a permissions violation does not
    /// kill the connection, and an unrelated `-ERR` is left to the paths that
    /// already interpret it.
    fn confirm(&mut self, op: &str, subject: &str) -> Result<(), NatsError> {
        self.write_frames(b"PING\r\n")?;
        // Bounded: a handful of frames may be queued ahead of our PONG.
        for _ in 0..16 {
            match read_frame(&mut self.reader)? {
                ReadOutcome::Frame(Frame::Pong) => return Ok(()),
                ReadOutcome::Frame(Frame::Ping) => self.pong()?,
                ReadOutcome::Frame(Frame::ServerErr(m)) => {
                    if is_permissions_error(&m) {
                        return Err(permissions_error(op, subject, &m, self.authenticated));
                    }
                    if is_auth_error(&m) {
                        return Err(NatsError::Disconnected(m));
                    }
                    eprintln!("[nats] server error: {m}");
                }
                // A MSG that raced in front of our PONG is real traffic; the
                // subscription reader will see it on the socket after us.
                ReadOutcome::Frame(_) => continue,
                // Never fail an operation because confirmation was slow — the
                // publish/subscribe itself already went out. Silence is not
                // evidence of refusal.
                ReadOutcome::TimedOut => return Ok(()),
                ReadOutcome::Closed => {
                    return Err(NatsError::Disconnected(
                        "connection closed awaiting confirmation".to_string(),
                    ))
                }
            }
        }
        Ok(())
    }

    /// The single choke point for outbound bytes. Refuses to touch a
    /// desynced stream, and marks the stream desynced when a write fails
    /// (any I/O error here may have left partial frame bytes on the wire —
    /// further writes would be parsed as the tail of the truncated frame).
    /// Every writer MUST route through this; direct `self.writer` access
    /// is reserved for `try_clone` in subscription setup.
    fn write_frames(&mut self, bytes: &[u8]) -> Result<(), NatsError> {
        if self.dead {
            return Err(NatsError::Disconnected(
                "connection desynced by an earlier mid-frame write failure — reconnect required"
                    .to_string(),
            ));
        }
        let result = self
            .writer
            .write_all(bytes)
            .and_then(|()| self.writer.flush());
        if let Err(e) = result {
            self.dead = true;
            return Err(NatsError::Io(e));
        }
        Ok(())
    }
}

/// Perform the TCP connect + INFO/CONNECT/PING/PONG handshake. Shared by
/// `connect()` and `reconnect()` so both send the same authenticated
/// CONNECT payload (NATS_USER/NATS_PASSWORD env, falling back to URL
/// credentials). ADR-0026 #73.
/// Pick the identity to present in CONNECT: explicit > env > URL.
///
/// Split out of `handshake` because it is the whole of the identity decision
/// and `handshake` itself needs a live socket to exercise.
fn resolve_creds(
    explicit: Option<&(String, String)>,
    env_user: String,
    env_pass: String,
    url_creds: Option<(String, String)>,
) -> Option<(String, String)> {
    if let Some((u, p)) = explicit {
        return Some((u.clone(), p.clone()));
    }
    if !env_user.is_empty() && !env_pass.is_empty() {
        return Some((env_user, env_pass));
    }
    url_creds
}

/// Open an authenticated connection.
///
/// `explicit` is for callers that carry their OWN credential namespace and
/// must not inherit the ambient swarm identity — the hive bridge authenticates
/// as `HIVE_NATS_USER`, and on a box where the generic `NATS_USER` is also
/// exported (every kannaka unit that sources `.kannaka-nats.env`) the env
/// branch below would silently connect it as the wrong principal. Explicit
/// creds therefore outrank env, which outranks `user:pass@` in the URL.
/// `None` is exactly the previous behaviour.
fn handshake(url: &str, explicit: Option<&(String, String)>) -> Result<Conn, NatsError> {
    let (host, port, url_creds) = parse_nats_url(url)?;
    let addr = format!("{host}:{port}");
    use std::net::ToSocketAddrs;
    let socket_addr = addr
        .to_socket_addrs()
        .map_err(|e| NatsError::Connect(format!("DNS resolution failed for {addr}: {e}")))?
        .next()
        .ok_or_else(|| NatsError::Connect(format!("no addresses found for {addr}")))?;
    let stream = TcpStream::connect_timeout(&socket_addr, DEFAULT_IO_TIMEOUT)
        .map_err(|e| NatsError::Connect(format!("failed to connect to {addr}: {e}")))?;

    stream.set_read_timeout(Some(DEFAULT_IO_TIMEOUT))?;
    stream.set_write_timeout(Some(DEFAULT_IO_TIMEOUT))?;

    let mut writer = stream.try_clone()?;
    // This BufReader lives for the whole connection (it becomes Conn.reader),
    // so no pre-read bytes are ever discarded by an into_inner()/drop.
    let mut reader = BufReader::new(stream);

    // Read the INFO line (bounded — a hostile/MITM server could otherwise stream
    // an unbounded INFO during the pre-auth handshake and OOM the client).
    let mut info_line = String::new();
    read_control_line(&mut reader, &mut info_line)?;
    if !info_line.starts_with("INFO ") {
        return Err(NatsError::Protocol(format!(
            "expected INFO, got: {}",
            info_line.trim()
        )));
    }
    // Inspect the server INFO: we cannot speak TLS, so fail with a clear
    // message instead of a confusing "expected PONG" later.
    if let Ok(info) =
        serde_json::from_str::<serde_json::Value>(info_line["INFO ".len()..].trim())
    {
        if info
            .get("tls_required")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            return Err(NatsError::Connect(
                "server requires TLS, which this client does not support".to_string(),
            ));
        }
    }

    // Credentials: env vars take precedence, then URL user:pass@. Anonymous
    // connections get the server's anonymous permissions (read-only).
    let env_user = std::env::var("NATS_USER").unwrap_or_default();
    let env_pass = std::env::var("NATS_PASSWORD").unwrap_or_default();
    let creds = resolve_creds(explicit, env_user, env_pass, url_creds);
    let connect_payload = match &creds {
        Some((user, pass)) => {
            // Escape JSON safely. Both fields are short tokens — quotation
            // and backslash are the only chars worth handling.
            let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
            format!(
                r#"{{"verbose":false,"pedantic":false,"name":"kannaka","lang":"rust","version":"0.1.0","protocol":1,"user":"{}","pass":"{}"}}"#,
                esc(user),
                esc(pass)
            )
        }
        None => {
            r#"{"verbose":false,"pedantic":false,"name":"kannaka","lang":"rust","version":"0.1.0","protocol":1}"#
                .to_string()
        }
    };
    write!(writer, "CONNECT {connect_payload}\r\n")?;
    write!(writer, "PING\r\n")?;
    writer.flush()?;

    // Wait for the actual PONG. +OK / async INFO lines are skipped (they are
    // not success); -ERR (e.g. Authorization Violation) fails the handshake.
    for _ in 0..10 {
        match read_frame(&mut reader)? {
            ReadOutcome::Frame(Frame::Pong) => {
                return Ok(Conn {
                    writer,
                    reader,
                    dead: false,
                    authenticated: creds.is_some(),
                    stream_create_denied: false,
                })
            }
            ReadOutcome::Frame(Frame::Ping) => {
                write!(writer, "PONG\r\n")?;
                writer.flush()?;
            }
            ReadOutcome::Frame(Frame::ServerErr(m)) => {
                return Err(NatsError::Connect(format!("server rejected connect: {m}")));
            }
            ReadOutcome::Frame(_) => continue,
            ReadOutcome::TimedOut => {
                return Err(NatsError::Protocol(
                    "timed out waiting for PONG during handshake".to_string(),
                ));
            }
            ReadOutcome::Closed => {
                return Err(NatsError::Connect(
                    "connection closed during handshake".to_string(),
                ));
            }
        }
    }
    Err(NatsError::Protocol(
        "no PONG after CONNECT (too many interleaved frames)".to_string(),
    ))
}

/// A message buffered during disconnect, replayed on reconnect.
#[derive(Clone)]
struct BufferedMessage {
    subject: String,
    payload: Vec<u8>,
    /// Failed replay attempts. Replay is strictly order-preserving, so a
    /// message that can never be written (e.g. it exceeds the server's
    /// max_payload and kills the connection every time) would otherwise
    /// pin the buffer head forever — every later message, including
    /// presence announces, queues behind it while the daemon logs
    /// "reconnected" each cycle. Dropped loudly after MAX_REPLAY_ATTEMPTS.
    attempts: u32,
}

/// Replay attempts before a head-of-buffer message is declared poison and
/// dropped so the queue behind it can drain.
const MAX_REPLAY_ATTEMPTS: u32 = 5;

/// A minimal synchronous NATS client with JetStream KV, events, and reconnection.
pub struct SwarmTransport {
    conn: Arc<Mutex<Conn>>,
    url: String,
    /// Monotonic subscription-id allocator. Every SUB (long-running
    /// subscriptions AND one-shot RPC inboxes) gets a unique sid so a
    /// timed-out request can never collide with a later subscription.
    next_sid: AtomicU64,
    jetstream_ok: bool,
    /// True only when this connection may CREATE/UPDATE streams (the writer
    /// identity). `jetstream_ok` alone means readable — a create-denied
    /// client (anon) reads retained state but must not attempt bucket/stream
    /// management.
    jetstream_writable: bool,
    connected: Arc<Mutex<bool>>,
    publish_buffer: Arc<Mutex<VecDeque<BufferedMessage>>>,
    /// Last in-place revival attempt (see `try_revive_locked`). Rate-limits
    /// redials so a daemon publishing frequently against a downed server
    /// pays one connect timeout per REVIVE_INTERVAL, not per publish.
    last_revive: Arc<Mutex<Option<Instant>>>,
    /// Credentials supplied by the caller rather than read from the ambient
    /// environment. Held so `reconnect()` and `try_revive_locked()` re-present
    /// the SAME identity — a redial that silently fell back to env/URL creds
    /// would come back as a different principal with different ACLs.
    explicit_creds: Option<(String, String)>,
}

/// Minimum spacing between in-place redial attempts from `try_revive_locked`.
const REVIVE_INTERVAL: Duration = Duration::from_secs(15);

// ──────────────────────────────────────────────────────────────────
// ADR-0028 — durable event-sourced HRM. Streams declared as data;
// `ensure_event_stream(kind)` builds the JetStream config from the
// spec table so adding a new stream is one variant + one arm.
// ──────────────────────────────────────────────────────────────────

/// Declarative spec for a JetStream stream. All ADR-0028 streams share
/// `retention=limits, storage=file, discard=old, num_replicas=1`; the
/// per-stream fields below are the only knobs that vary.
pub struct StreamSpec {
    pub name: &'static str,
    pub subjects: &'static [&'static str],
    pub max_age_days: Option<i64>,
    pub max_msgs_per_subject: Option<i64>,
    pub max_msg_size: Option<i64>,
}

/// The three durable streams that back ADR-0028's event-sourced HRM.
/// Iterate via `StreamKind::ALL` for bulk operations (e.g. `events init`).
#[derive(Clone, Copy, Debug)]
pub enum StreamKind {
    /// Per-agent remember/forget/dream log. 90-day window.
    /// Subject: `KANNAKA.events.memory.<agent_id>.>`
    MemoryEvents,
    /// Substrate absorb/anchor/flush log. 365-day window — the substrate IS
    /// the long memory; its event log is the constellation's diary.
    /// Subject: `KANNAKA.events.substrate.>`
    SubstrateEvents,
    /// Periodic gzipped HRM snapshots. Last 168 per agent (one week of
    /// hourly snapshots). Replay can warm-start from a snapshot instead
    /// of from event zero. Subject: `KANNAKA.snapshots.>`
    Snapshots,
}

impl StreamKind {
    pub const ALL: &'static [StreamKind] = &[
        StreamKind::MemoryEvents,
        StreamKind::SubstrateEvents,
        StreamKind::Snapshots,
    ];

    pub fn spec(self) -> StreamSpec {
        match self {
            StreamKind::MemoryEvents => StreamSpec {
                name: "KANNAKA_MEMORY_EVENTS",
                subjects: &["KANNAKA.events.memory.>"],
                max_age_days: Some(90),
                max_msgs_per_subject: None,
                max_msg_size: None,
            },
            StreamKind::SubstrateEvents => StreamSpec {
                name: "KANNAKA_SUBSTRATE_EVENTS",
                subjects: &["KANNAKA.events.substrate.>"],
                max_age_days: Some(365),
                max_msgs_per_subject: None,
                max_msg_size: None,
            },
            StreamKind::Snapshots => StreamSpec {
                name: "KANNAKA_SNAPSHOTS",
                subjects: &["KANNAKA.snapshots.>"],
                max_age_days: None,
                max_msgs_per_subject: Some(168),
                max_msg_size: Some(100 * 1024 * 1024),
            },
        }
    }
}

/// A durable event published to JetStream. The variant determines the
/// subject and payload shape; every payload carries `event_id`,
/// `schema_version`, `agent_id`, `ts` so replay is idempotent.
///
/// Adding a new event type = one variant + one arm in `subject()` and
/// `payload_json()`. No surgery on the publish path.
pub enum EventPayload<'a> {
    /// A new memory was stored. Replay reconstructs the wavefront.
    /// Subject: `KANNAKA.events.memory.<agent_id>.remember`
    MemoryRemember {
        agent_id: &'a str,
        memory_id: &'a uuid::Uuid,
        content: &'a str,
        importance: f32,
        modality: &'a str,
    },
    /// A memory was deleted. Replay drops the matching memory_id.
    /// Subject: `KANNAKA.events.memory.<agent_id>.forget`
    MemoryForget {
        agent_id: &'a str,
        memory_id: &'a uuid::Uuid,
    },
    /// Wave-signature absorbed into the substrate. No per-agent suffix —
    /// every absorb in one stream for easier collective time-machine
    /// reconstruction. Subject: `KANNAKA.events.substrate.absorb`
    SubstrateAbsorb {
        agent_id: &'a str,
        class_index: u32,
        amplitude: f32,
        phase: f32,
        frequency: f32,
    },
    /// Full-HRM snapshot manifest. ADR-0028 Phase 2 + ADR-0026 Phase 5:
    /// the gzipped HRM body lives on local disk at `body_path` (or a
    /// future Object Store URL), the JetStream event carries only the
    /// manifest + path + size. This is because NATS silently caps
    /// payloads ~8-10MB even with max_payload bumped to 64MB; 35MB raw
    /// HRMs need out-of-band storage.
    /// Subject: `KANNAKA.snapshots.<agent_id>.full`
    SnapshotFull {
        agent_id: &'a str,
        version: &'a str,
        wavefronts: u64,
        clusters: u64,
        phi: f32,
        /// Path to the gzipped HRM body (local FS path today; URL once
        /// ADR-0026 Phase 5 Object Store lands).
        body_path: &'a str,
        /// Compressed body size in bytes.
        body_gz_bytes: u64,
    },
}

impl<'a> EventPayload<'a> {
    pub fn subject(&self) -> String {
        match self {
            EventPayload::MemoryRemember { agent_id, .. } => {
                format!("KANNAKA.events.memory.{agent_id}.remember")
            }
            EventPayload::MemoryForget { agent_id, .. } => {
                format!("KANNAKA.events.memory.{agent_id}.forget")
            }
            EventPayload::SubstrateAbsorb { .. } => {
                "KANNAKA.events.substrate.absorb".to_string()
            }
            EventPayload::SnapshotFull { agent_id, .. } => {
                format!("KANNAKA.snapshots.{agent_id}.full")
            }
        }
    }

    pub fn payload_json(&self) -> serde_json::Value {
        // Envelope matches the canonical contract — schema_version: "1.0"
        // (string), ts: unix-ms (number). The bare event_id stays.
        // Pre-fix this codepath bypassed add_envelope entirely (issue #90)
        // and emitted schema_version as an integer + ts as RFC3339 string.
        let base = serde_json::json!({
            "event_id": uuid::Uuid::new_v4(),
            "schema_version": "1.0",
            "ts": chrono::Utc::now().timestamp_millis(),
        });
        let mut obj = base.as_object().cloned().unwrap_or_default();
        match self {
            EventPayload::MemoryRemember {
                agent_id, memory_id, content, importance, modality,
            } => {
                obj.insert("agent_id".into(), serde_json::json!(agent_id));
                obj.insert("memory_id".into(), serde_json::json!(memory_id));
                obj.insert("content".into(), serde_json::json!(content));
                obj.insert("importance".into(), serde_json::json!(importance));
                obj.insert("modality".into(), serde_json::json!(modality));
            }
            EventPayload::MemoryForget { agent_id, memory_id } => {
                obj.insert("agent_id".into(), serde_json::json!(agent_id));
                obj.insert("memory_id".into(), serde_json::json!(memory_id));
            }
            EventPayload::SubstrateAbsorb {
                agent_id, class_index, amplitude, phase, frequency,
            } => {
                obj.insert("agent_id".into(), serde_json::json!(agent_id));
                obj.insert("class_index".into(), serde_json::json!(class_index));
                obj.insert("amplitude".into(), serde_json::json!(amplitude));
                obj.insert("phase".into(), serde_json::json!(phase));
                obj.insert("frequency".into(), serde_json::json!(frequency));
            }
            EventPayload::SnapshotFull {
                agent_id, version, wavefronts, clusters, phi,
                body_path, body_gz_bytes,
            } => {
                obj.insert("agent_id".into(), serde_json::json!(agent_id));
                obj.insert("manifest".into(), serde_json::json!({
                    "version": version,
                    "wavefronts": wavefronts,
                    "clusters": clusters,
                    "phi": phi,
                }));
                obj.insert("body_path".into(), serde_json::json!(body_path));
                obj.insert("body_gz_bytes".into(), serde_json::json!(body_gz_bytes));
            }
        }
        serde_json::Value::Object(obj)
    }
}

impl SwarmTransport {
    /// Connect to a NATS server at the given URL.
    pub fn connect(url: &str) -> Result<Self, NatsError> {
        Self::connect_with_creds(url, None)
    }

    /// Connect presenting caller-supplied credentials instead of the ambient
    /// `NATS_USER`/`NATS_PASSWORD`. See `handshake` for why this outranks env.
    pub fn connect_with_creds(
        url: &str,
        explicit_creds: Option<(String, String)>,
    ) -> Result<Self, NatsError> {
        let conn = handshake(url, explicit_creds.as_ref())?;
        let mut transport = Self {
            conn: Arc::new(Mutex::new(conn)),
            url: url.to_string(),
            explicit_creds,
            next_sid: AtomicU64::new(1),
            jetstream_ok: false,
            jetstream_writable: false,
            connected: Arc::new(Mutex::new(true)),
            publish_buffer: Arc::new(Mutex::new(VecDeque::new())),
            last_revive: Arc::new(Mutex::new(None)),
        };

        // Try to ensure JetStream streams exist. A client whose NATS user is
        // denied $JS.API.STREAM.CREATE (the open swarm's anon user, since
        // ADR-0042 closed its control lane) can still READ streams the writer
        // identity created — probe MSG.GET before giving up on JetStream, so
        // such clients keep the retained-phase read path instead of falling
        // back to live-gossip sniffing (whose 1.5s-silence window misses
        // agents that beacon every 30s and reported 0 peers on live swarms).
        //
        // For an ANONYMOUS connection the create is not just likely to fail —
        // it CANNOT succeed, so issuing it only to learn that costs a broker
        // "Permissions Violation ... $JS.API.STREAM.CREATE" on every single
        // connection. Measured on the live swarm 2026-08-23: ~113 denials a
        // minute, of which 79 in 83 were anon. So anon probes the read path
        // first and skips the doomed create; authenticated identities keep the
        // create/update path unchanged (it is also what syncs stream config,
        // and what proves `jetstream_writable`).
        let (created, readable) = if should_attempt_stream_create(transport.is_authenticated()) {
            let created = transport.ensure_stream().is_ok();
            (created, created || transport.stream_readable(STREAM_NAME))
        } else {
            (false, transport.stream_readable(STREAM_NAME))
        };
        transport.jetstream_writable = created;
        transport.jetstream_ok = created || readable;
        if created {
            let _ = transport.ensure_events_stream();
        } else if transport.jetstream_ok {
            // Read-only is a normal, fully usable state for a non-writer
            // identity — say so, so nobody reads it as a failure. (Anon no
            // longer even attempts the create, so this path is now quiet on
            // the broker instead of logging a Permissions Violation.)
            eprintln!(
                "[nats] JetStream read-only for this user — retained reads active (stream create not available to this identity)"
            );
        }

        Ok(transport)
    }

    /// Connect to the default NATS URL.
    pub fn connect_default() -> Result<Self, NatsError> {
        Self::connect(DEFAULT_NATS_URL)
    }

    /// Whether JetStream is READABLE on this connection (retained-state
    /// reads via MSG.GET). A create-denied client still reports true.
    pub fn has_jetstream(&self) -> bool {
        self.jetstream_ok
    }

    /// Whether this connection may also CREATE/UPDATE streams and buckets.
    /// Gate stream/bucket management (and the integration tests that do it)
    /// on this, not on `has_jetstream()`.
    pub fn has_jetstream_write(&self) -> bool {
        self.jetstream_writable
    }

    /// Get the URL this transport is connected to.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Allocate a fresh, connection-unique subscription id.
    fn alloc_sid(&self) -> u64 {
        self.next_sid.fetch_add(1, Ordering::Relaxed)
    }

    /// Lock the connection, mapping a poisoned mutex to a protocol error
    /// (the connection state under a panic mid-write is unknowable).
    fn lock_conn(&self) -> Result<std::sync::MutexGuard<'_, Conn>, NatsError> {
        self.conn
            .lock()
            .map_err(|e| NatsError::Protocol(format!("connection lock poisoned: {e}")))
    }

    // -----------------------------------------------------------------------
    // Reconnection
    // -----------------------------------------------------------------------

    /// Attempt to reconnect to the NATS server, replaying any buffered
    /// messages in their original order. Uses the same authenticated
    /// handshake as `connect()` (NATS_USER/NATS_PASSWORD are NOT dropped
    /// on reconnect).
    ///
    /// **Existing subscriptions do NOT survive this call (#706).** Each
    /// `NatsSubscription` reads from a clone of the pre-reconnect socket, so
    /// after a reconnect their `next_event()` reports `Closed`. Repair them
    /// explicitly with [`NatsSubscription::resubscribe_via`], or treat
    /// `Closed` as exit-for-restart (the pattern the long-lived services
    /// use). Silently continuing to poll an old subscription after reconnect
    /// listens to a dead socket.
    pub fn reconnect(&mut self) -> Result<(), NatsError> {
        let new_conn = handshake(&self.url, self.explicit_creds.as_ref())?;

        // Swap in the new connection. A poisoned lock just means a previous
        // holder panicked — the old Conn is being replaced wholesale anyway.
        {
            let mut guard = self.conn.lock().unwrap_or_else(|p| p.into_inner());
            *guard = new_conn;
        }
        *self.connected.lock().unwrap_or_else(|p| p.into_inner()) = true;

        // Re-check JetStream (same read-only degradation as connect(), and the
        // same anon skip — a reconnect loop was re-issuing the doomed create
        // on every single attempt).
        let (created, readable) = if should_attempt_stream_create(self.is_authenticated()) {
            let created = self.ensure_stream().is_ok();
            (created, created || self.stream_readable(STREAM_NAME))
        } else {
            (false, self.stream_readable(STREAM_NAME))
        };
        self.jetstream_writable = created;
        self.jetstream_ok = created || readable;
        if created {
            let _ = self.ensure_events_stream();
        } else if self.jetstream_ok {
            eprintln!(
                "[nats] JetStream read-only for this user (stream create denied — expected for non-writer identities); retained reads active"
            );
        }

        // Replay buffered messages in order. On failure the unsent remainder
        // stays queued (in order, at the front) for the next attempt — and
        // the reconnect reports FAILURE, so callers don't log "reconnected"
        // and re-announce presence on a connection whose replay just
        // re-killed it (the node would look recovered in its own logs while
        // still absent from the swarm). Poison head messages are dropped by
        // flush_buffer_locked after MAX_REPLAY_ATTEMPTS, so repeated calls
        // converge instead of looping forever.
        let replay_result = {
            let mut conn = self.lock_conn()?;
            self.flush_buffer_locked(&mut conn)
        };
        if let Err(e) = replay_result {
            self.mark_disconnected();
            return Err(NatsError::Disconnected(format!(
                "reconnected but buffered replay incomplete ({} still queued): {}",
                self.buffered_count(),
                e
            )));
        }

        Ok(())
    }

    /// Check if connection is still alive. Sends a PING and waits for the
    /// PONG, so a dead socket whose writes still land in the kernel buffer
    /// is detected.
    pub fn is_connected(&self) -> bool {
        {
            let c = self.connected.lock().unwrap_or_else(|p| p.into_inner());
            if !*c {
                return false;
            }
        }
        self.ping().is_ok()
    }

    /// Mark the connection as disconnected (used internally on I/O failure).
    fn mark_disconnected(&self) {
        *self.connected.lock().unwrap_or_else(|p| p.into_inner()) = false;
    }

    /// Buffer a message for later replay (up to PUBLISH_BUFFER_LIMIT).
    fn buffer_message(&self, subject: &str, payload: &[u8]) {
        let mut buf = self.publish_buffer.lock().unwrap_or_else(|p| p.into_inner());
        if buf.len() >= PUBLISH_BUFFER_LIMIT {
            if let Some(dropped) = buf.pop_front() {
                eprintln!(
                    "[nats] publish buffer full ({}) — dropping oldest message on {}",
                    PUBLISH_BUFFER_LIMIT, dropped.subject
                );
            }
        }
        buf.push_back(BufferedMessage {
            subject: subject.to_string(),
            payload: payload.to_vec(),
            attempts: 0,
        });
    }

    /// Return the number of messages currently buffered.
    pub fn buffered_count(&self) -> usize {
        self.publish_buffer
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .len()
    }

    /// Drain the publish buffer onto the wire in FIFO order. If a message
    /// fails to send it is pushed back to the FRONT (preserving order) and
    /// the error is returned; nothing after it is attempted.
    fn flush_buffer_locked(&self, conn: &mut Conn) -> Result<(), NatsError> {
        if conn.dead {
            return Err(NatsError::Disconnected(
                "connection desynced — buffered replay deferred until reconnect".to_string(),
            ));
        }
        loop {
            let next = self
                .publish_buffer
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .pop_front();
            let Some(mut msg) = next else { return Ok(()) };
            if let Err(e) = write_pub(conn, &msg.subject, &msg.payload) {
                // write_pub's choke point already marked the Conn dead.
                msg.attempts += 1;
                if msg.attempts >= MAX_REPLAY_ATTEMPTS {
                    eprintln!(
                        "[nats] DROPPING poison buffered message on {} after {} failed replays ({} bytes): {}",
                        msg.subject, msg.attempts, msg.payload.len(), e
                    );
                    // Don't re-queue — let the messages behind it drain on
                    // the next replay instead of starving forever.
                } else {
                    eprintln!(
                        "[nats] buffered replay failed on {} (attempt {}) — re-queueing: {}",
                        msg.subject, msg.attempts, e
                    );
                    self.publish_buffer
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .push_front(msg);
                }
                return Err(e);
            }
        }
    }

    // -----------------------------------------------------------------------
    // JetStream API request/reply core
    // -----------------------------------------------------------------------

    /// One JetStream API request/reply exchange over the held connection.
    ///
    /// SUBs a unique inbox with a unique sid (auto-unsubscribed after one
    /// message), PUBs the request with the inbox as reply-to, then reads
    /// frames until the reply lands on OUR inbox — frames for other sids
    /// (gossip, stray replies) are consumed and skipped, server PINGs are
    /// answered, `-ERR` lines are logged (auth errors are fatal).
    ///
    /// Returns `Ok(None)` on a clean no-reply-within-timeout; `Err` only
    /// for conditions that invalidate the connection (which is then marked
    /// disconnected). The previous read timeout is restored on all paths.
    fn js_api_call_locked(
        &self,
        conn: &mut Conn,
        api_subject: &str,
        request: &[u8],
        timeout: Duration,
    ) -> Result<Option<serde_json::Value>, NatsError> {
        let sid = self.alloc_sid();
        let inbox = new_inbox("js");

        let mut frame = Vec::with_capacity(request.len() + 128);
        let _ = write!(frame, "SUB {inbox} {sid}\r\n");
        let _ = write!(frame, "UNSUB {sid} 1\r\n");
        let _ = write!(frame, "PUB {} {} {}\r\n", api_subject, inbox, request.len());
        frame.extend_from_slice(request);
        frame.extend_from_slice(b"\r\n");
        conn.write_frames(&frame)?;

        let prev = conn.read_timeout();
        conn.set_read_timeout(Some(timeout))?;
        let deadline = Instant::now() + timeout;

        let mut fatal = false;
        let result = loop {
            if Instant::now() > deadline {
                break Ok(None);
            }
            match read_frame(&mut conn.reader) {
                Ok(ReadOutcome::Frame(Frame::Msg { subject, payload, .. }))
                    if subject == inbox =>
                {
                    break serde_json::from_slice::<serde_json::Value>(&payload)
                        .map(Some)
                        .map_err(|e| {
                            NatsError::Protocol(format!("bad JetStream API reply: {e}"))
                        });
                }
                // A frame for some other subscription on this connection —
                // its payload was consumed correctly; skip it.
                Ok(ReadOutcome::Frame(Frame::Msg { .. })) => continue,
                Ok(ReadOutcome::Frame(Frame::Ping)) => {
                    let _ = conn.pong();
                }
                Ok(ReadOutcome::Frame(Frame::ServerErr(m))) => {
                    eprintln!("[nats] server error: {m}");
                    // #933: a denied JetStream request is answered with an
                    // async -ERR and no reply, so the call below simply times
                    // out. Record the refusal while we can still see it —
                    // "the broker said no to STREAM.CREATE on this
                    // connection" is the fact worth remembering, and it is
                    // the only honest basis for not trying again.
                    if names_denied_stream_create(&m) {
                        conn.stream_create_denied = true;
                    }
                    if is_auth_error(&m) {
                        fatal = true;
                        break Err(NatsError::Disconnected(m));
                    }
                }
                Ok(ReadOutcome::Frame(_)) => continue,
                Ok(ReadOutcome::TimedOut) => break Ok(None),
                Ok(ReadOutcome::Closed) => {
                    fatal = true;
                    break Err(NatsError::Disconnected(
                        "connection closed during JetStream request".to_string(),
                    ));
                }
                Err(e) => {
                    fatal = true;
                    break Err(e);
                }
            }
        };

        // If no reply was delivered, the auto-unsub never fired — remove the
        // subscription explicitly so it can't leak server-side.
        if !matches!(result, Ok(Some(_))) {
            let _ = conn.write_frames(format!("UNSUB {sid}\r\n").as_bytes());
        }
        let _ = conn.set_read_timeout(prev);
        if fatal {
            self.mark_disconnected();
        }
        result
    }

    /// Walk a JetStream stream by subject filter, returning the raw stored
    /// messages as (subject, server ingest time RFC3339, decoded payload)
    /// triples. Shared engine behind `get_stream_messages`, `kv_keys` and the
    /// phase readers; keeps the ONE persistent connection reader for the
    /// whole walk (recreating a reader per request used to lose pre-read
    /// bytes and return 0 rows).
    fn stream_walk(
        &self,
        stream_name: &str,
        subject_filter: &str,
        max_messages: usize,
    ) -> Result<Vec<(String, String, Vec<u8>)>, NatsError> {
        let mut conn = self.lock_conn()?;
        let api_subject = format!("$JS.API.STREAM.MSG.GET.{stream_name}");
        let mut out: Vec<(String, String, Vec<u8>)> = Vec::new();
        let mut next_seq: u64 = 1;

        while out.len() < max_messages {
            let req = serde_json::json!({
                "seq": next_seq,
                "next_by_subj": subject_filter,
            })
            .to_string();
            let resp = match self.js_api_call_locked(
                &mut conn,
                &api_subject,
                req.as_bytes(),
                JS_API_TIMEOUT,
            ) {
                Ok(Some(v)) => v,
                Ok(None) => break, // no reply — treat as end of stream
                Err(e) => return Err(e),
            };
            if resp.get("error").is_some() {
                break; // typically "no message found" — end of matches
            }
            let Some(msg) = resp.get("message") else { break };
            // #371: a single stored message without a usable seq must NOT
            // truncate the whole forward walk. Advance the cursor past it and
            // keep going — the strictly-increasing `next_seq` + finite stream
            // guarantees termination at the next no-match.
            let Some(seq) = msg.get("seq").and_then(|s| s.as_u64()) else {
                next_seq += 1;
                continue;
            };
            // Bookkeeping first so non-decodable payloads don't stall the
            // iteration (older test data in front of real manifests).
            next_seq = seq + 1;
            let subject = msg
                .get("subject")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string();
            // Broker receive time — the liveness authority (a publisher's
            // own clock can be skewed, and some payloads carry no timestamp).
            let time = msg
                .get("time")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string();
            let data = msg
                .get("data")
                .and_then(|d| d.as_str())
                .and_then(|b64| base64_decode(b64).ok())
                .unwrap_or_default();
            out.push((subject, time, data));
        }
        Ok(out)
    }

    // -----------------------------------------------------------------------
    // JetStream stream management
    // -----------------------------------------------------------------------

    /// Ensure the QUEEN_PHASES JetStream stream exists.
    fn ensure_stream(&self) -> Result<(), NatsError> {
        self.ensure_js_stream(
            STREAM_NAME,
            serde_json::json!({
                "name": STREAM_NAME,
                "subjects": ["QUEEN.phase.>"],
                "retention": "limits",
                "max_msgs_per_subject": 1,
                "storage": "file",
                "discard": "old",
                "num_replicas": 1
            }),
        )
    }

    /// Ensure the QUEEN_EVENTS JetStream stream exists.
    fn ensure_events_stream(&self) -> Result<(), NatsError> {
        self.ensure_js_stream(
            EVENTS_STREAM_NAME,
            serde_json::json!({
                "name": EVENTS_STREAM_NAME,
                "subjects": ["QUEEN.event.>"],
                "retention": "limits",
                "max_msgs": 10000,
                "storage": "file",
                "discard": "old",
                "num_replicas": 1
            }),
        )
    }

    /// Whether this connection supplied credentials.
    ///
    /// Read under its own short-lived lock: `stream_readable` takes the same
    /// (non-reentrant) mutex, so the guard must be dropped before calling it.
    pub fn is_authenticated(&self) -> bool {
        match self.lock_conn() {
            Ok(conn) => conn.authenticated,
            // Unknown — assume authenticated so we keep the create path rather
            // than silently downgrading a writer.
            Err(_) => true,
        }
    }

    /// True when the stream's read API answers this connection. A client can
    /// be denied stream CREATE yet allowed MSG.GET (the anon swarm user) —
    /// any JSON reply to the probe, including a "no message found" error,
    /// proves readability; a permission denial produces no reply (timeout).
    fn stream_readable(&self, stream_name: &str) -> bool {
        let api_subject = format!("$JS.API.STREAM.MSG.GET.{stream_name}");
        let req = serde_json::json!({ "seq": 1, "next_by_subj": ">" }).to_string();
        let Ok(mut conn) = self.lock_conn() else {
            return false;
        };
        matches!(
            self.js_api_call_locked(&mut conn, &api_subject, req.as_bytes(), JS_API_TIMEOUT),
            Ok(Some(_))
        )
    }

    /// Generic helper: create-or-update a JetStream stream.
    fn ensure_js_stream(
        &self,
        stream_name: &str,
        config: serde_json::Value,
    ) -> Result<(), NatsError> {
        let payload = config.to_string();
        let mut conn = self.lock_conn()?;

        let create_subject = format!("$JS.API.STREAM.CREATE.{stream_name}");
        let resp = self
            .js_api_call_locked(&mut conn, &create_subject, payload.as_bytes(), JS_API_TIMEOUT)?
            .ok_or_else(|| {
                NatsError::Protocol(format!("no JetStream reply for stream create: {stream_name}"))
            })?;

        if let Some(err) = resp.get("error") {
            let code = err.get("err_code").and_then(|c| c.as_u64()).unwrap_or(0);
            if code == 10058 {
                // Stream already exists — attempt UPDATE to sync config.
                // Best-effort: an UPDATE rejection (e.g. immutable field)
                // still leaves a usable stream, so the reply is drained
                // and ignored.
                let update_subject = format!("$JS.API.STREAM.UPDATE.{stream_name}");
                let _ = self.js_api_call_locked(
                    &mut conn,
                    &update_subject,
                    payload.as_bytes(),
                    JS_API_TIMEOUT,
                );
                return Ok(());
            }
            return Err(NatsError::Protocol(format!("JetStream stream create error: {err}")));
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Low-level publish (with disconnect buffering)
    // -----------------------------------------------------------------------

    /// Public façade over the raw PUB. Use this for ad-hoc subjects
    /// (e.g. the inbox prototype) that don't have a typed publish_X
    /// method. The disconnect-buffering behavior of publish_raw is
    /// preserved.
    pub fn publish(&self, subject: &str, payload: &[u8]) -> Result<(), NatsError> {
        self.publish_raw(subject, payload)
    }

    /// Publish a raw message to a subject, buffering on disconnect.
    ///
    /// If earlier messages are sitting in the disconnect buffer they are
    /// replayed (in order) BEFORE this message, so the wire order matches
    /// the call order. On failure this message joins the back of the buffer.
    fn publish_raw(&self, subject: &str, payload: &[u8]) -> Result<(), NatsError> {
        let mut conn = self.lock_conn()?;
        // A dead Conn's outbound stream is desynced mid-frame — writing
        // anything more (this message OR the buffered replay) would be parsed
        // by the server as the tail of the truncated frame. Try an in-place
        // revival (rate-limited); if that fails, fail fast to the buffer.
        // The revival is what lets long-lived daemons that hold an IMMUTABLE
        // transport (swarm serve, attention, substrate, inbox watch) recover
        // from one transient write timeout — only `swarm join` has a `mut`
        // transport and the full reconnect() path.
        if conn.dead && !self.try_revive_locked(&mut conn) {
            drop(conn);
            self.mark_disconnected();
            self.buffer_message(subject, payload);
            return Err(NatsError::Disconnected(
                "connection desynced by an earlier mid-frame write failure — awaiting revival"
                    .to_string(),
            ));
        }
        let result = self
            .flush_buffer_locked(&mut conn)
            .and_then(|()| write_pub(&mut conn, subject, payload));
        if result.is_err() {
            // The choke point already marked the Conn dead on a write error.
            drop(conn);
            self.mark_disconnected();
            self.buffer_message(subject, payload);
        }
        result
    }

    /// `publish_raw` plus a broker-acceptance round-trip (#562).
    ///
    /// Reserved for the publishes the README ADVERTISES as public capabilities
    /// — memory sync, dream publish, exemplar publish. Those are the ones an
    /// operator reasonably believes are working, so a silent ACL refusal there
    /// is a lie rather than a dropped packet.
    ///
    /// Deliberately NOT the default, for two reasons:
    ///
    /// 1. The confirmation costs a round-trip, and the hot paths
    ///    (phase/presence heartbeats, every 30s per node) should not pay it.
    /// 2. **Only safe on a transport with no live subscription.** Confirming
    ///    reads from the transport's own `BufReader`, while a
    ///    [`NatsSubscription`] reads a separate `BufReader` over the same
    ///    socket — so a MSG consumed here would be invisible to that
    ///    subscription forever. All three call sites (dream / exemplar /
    ///    memory.new publish) run on publish-only transports. `reply()`
    ///    deliberately stays on plain `publish_raw` because `swarm serve`
    ///    calls it on a transport that IS subscribed.
    fn publish_raw_confirmed(&self, subject: &str, payload: &[u8]) -> Result<(), NatsError> {
        self.publish_raw(subject, payload)?;
        let mut conn = self.lock_conn()?;
        conn.confirm("publish", subject)
    }

    /// Replace a dead `Conn` with a freshly-handshaked one, in place and
    /// through interior mutability — the counterpart to `reconnect()` for
    /// callers that hold `&self`. Rate-limited to one dial per
    /// REVIVE_INTERVAL so a busy publisher against a downed server pays one
    /// connect timeout per window, not per publish. Does NOT re-ensure
    /// JetStream streams (stream config is durable server-side) and does not
    /// touch subscriptions — a subscription's reader owns clones of the OLD
    /// socket and either keeps working (server still serves that
    /// connection) or sees EOF and takes its own Closed path.
    fn try_revive_locked(&self, conn: &mut Conn) -> bool {
        {
            let mut last = self.last_revive.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(t) = *last {
                if t.elapsed() < REVIVE_INTERVAL {
                    return false;
                }
            }
            *last = Some(Instant::now());
        }
        match handshake(&self.url, self.explicit_creds.as_ref()) {
            Ok(fresh) => {
                *conn = fresh;
                *self.connected.lock().unwrap_or_else(|p| p.into_inner()) = true;
                eprintln!("[nats] revived connection to {}", self.url);
                true
            }
            Err(e) => {
                eprintln!("[nats] revive failed ({}): retry in {}s", e, REVIVE_INTERVAL.as_secs());
                false
            }
        }
    }

    // -----------------------------------------------------------------------
    // Phase publishing
    // -----------------------------------------------------------------------

    /// Publish this agent's phase state.
    pub fn publish_phase(&self, phase: &AgentPhase) -> Result<(), NatsError> {
        let subject = format!("QUEEN.phase.{}", phase.agent_id);
        let mut value = serde_json::to_value(phase)
            .map_err(|e| NatsError::Serialize(e.to_string()))?;
        add_envelope(&mut value);
        let payload = serde_json::to_vec(&value)
            .map_err(|e| NatsError::Serialize(e.to_string()))?;
        self.publish_raw(&subject, &payload)
    }

    /// Publish consciousness state to KANNAKA.consciousness.
    pub fn publish_consciousness(&self, state: &serde_json::Value) -> Result<(), NatsError> {
        let mut value = state.clone();
        add_envelope(&mut value);
        let payload = serde_json::to_vec(&value)
            .map_err(|e| NatsError::Serialize(e.to_string()))?;
        self.publish_raw("KANNAKA.consciousness", &payload)
    }

    /// Publish dream report to KANNAKA.dreams.
    pub fn publish_dreams(&self, report: &serde_json::Value) -> Result<(), NatsError> {
        let mut value = report.clone();
        add_envelope(&mut value);
        let payload = serde_json::to_vec(&value)
            .map_err(|e| NatsError::Serialize(e.to_string()))?;
        // #562: advertised public capability — a refused publish must not
        // report success.
        self.publish_raw_confirmed("KANNAKA.dreams", &payload)
    }

    /// Publish a single cluster exemplar to KANNAKA.exemplar.<agent>.<cluster>.
    /// ADR-0026 Phase 2 — distilled-memory broadcast for cross-agent absorption.
    /// Uses one subject per (agent, cluster) so JetStream's
    /// max_msgs_per_subject=1 retention keeps the latest snapshot.
    pub fn publish_exemplar(
        &self,
        agent_id: &str,
        cluster_id: u32,
        payload: &serde_json::Value,
    ) -> Result<(), NatsError> {
        let subject = format!("KANNAKA.exemplar.{agent_id}.{cluster_id}");
        // #703: exemplars were the last publisher bypassing the canonical
        // envelope — every other structured subject goes through
        // add_envelope. (The contract's required `centroid` is a separate
        // problem: a full wave-memory centroid is WAVEFRONT_DIM floats and
        // cannot fit NATS's default 1 MB payload cap — see the issue for
        // the contract-amendment direction.)
        let mut enveloped = payload.clone();
        add_envelope(&mut enveloped);
        let bytes = serde_json::to_vec(&enveloped)
            .map_err(|e| NatsError::Serialize(e.to_string()))?;
        // #562: advertised public capability — confirm the broker took it.
        self.publish_raw_confirmed(&subject, &bytes)
    }

    /// Ensure the KANNAKA_EXEMPLARS JetStream stream exists.
    /// ADR-0026 Phase 2.
    pub fn ensure_exemplar_stream(&self) -> Result<(), NatsError> {
        self.ensure_js_stream(
            "KANNAKA_EXEMPLARS",
            serde_json::json!({
                "name": "KANNAKA_EXEMPLARS",
                "subjects": ["KANNAKA.exemplar.>"],
                "retention": "limits",
                "max_msgs_per_subject": 1,
                "max_age": 604_800_000_000_000_i64, // 7 days in nanoseconds
                "storage": "file",
                "discard": "old",
                "num_replicas": 1
            }),
        )
    }

    /// Pull all stored exemplar messages for one agent (or all agents).
    /// Convenience wrapper around `get_stream_messages`.
    pub fn get_exemplars(
        &self,
        from_agent: Option<&str>,
    ) -> Result<Vec<serde_json::Value>, NatsError> {
        let subject_filter = match from_agent {
            Some(a) => format!("KANNAKA.exemplar.{a}.>"),
            None => "KANNAKA.exemplar.>".to_string(),
        };
        self.get_stream_messages("KANNAKA_EXEMPLARS", &subject_filter, 500)
    }

    /// ADR-0037 Track-D: publish this agent's belief-core snapshot (L6
    /// fingerprints + phases) so peers can content-match + couple toward shared
    /// beliefs. Subject `KANNAKA.cores.<agent_id>`; KANNAKA_CORES keeps the
    /// latest per agent (max_msgs_per_subject=1).
    pub fn publish_cores(
        &self,
        agent_id: &str,
        payload: &serde_json::Value,
    ) -> Result<(), NatsError> {
        let subject = format!("KANNAKA.cores.{agent_id}");
        let bytes = serde_json::to_vec(payload)
            .map_err(|e| NatsError::Serialize(e.to_string()))?;
        self.publish_raw(&subject, &bytes)
    }

    /// Ensure the KANNAKA_CORES JetStream stream exists (ADR-0037 Track-D).
    pub fn ensure_cores_stream(&self) -> Result<(), NatsError> {
        self.ensure_js_stream(
            "KANNAKA_CORES",
            serde_json::json!({
                "name": "KANNAKA_CORES",
                "subjects": ["KANNAKA.cores.>"],
                "retention": "limits",
                "max_msgs_per_subject": 1,
                "max_age": 604_800_000_000_000_i64, // 7 days
                "max_msg_size": 1_048_576i64, // 1 MiB — a core snapshot is ≤8 cores (~KB); bound abuse
                "storage": "file",
                "discard": "old",
                "num_replicas": 1
            }),
        )
    }

    /// Pull peers' belief-core snapshots (one latest per agent), optionally
    /// filtered to a single agent. ADR-0037 Track-D.
    pub fn get_peer_cores(
        &self,
        from_agent: Option<&str>,
    ) -> Result<Vec<serde_json::Value>, NatsError> {
        let subject_filter = match from_agent {
            Some(a) => format!("KANNAKA.cores.{a}"),
            None => "KANNAKA.cores.>".to_string(),
        };
        self.get_stream_messages("KANNAKA_CORES", &subject_filter, 500)
    }

    /// Generic helper: iterate a JetStream stream's messages by subject filter.
    /// Used by both exemplars and presence (and future ADR-0026 streams).
    /// Non-JSON payloads are skipped (but still advance the walk).
    pub fn get_stream_messages(
        &self,
        stream_name: &str,
        subject_filter: &str,
        max_messages: usize,
    ) -> Result<Vec<serde_json::Value>, NatsError> {
        Ok(self
            .stream_walk(stream_name, subject_filter, max_messages)?
            .into_iter()
            .filter_map(|(_, _, data)| serde_json::from_slice(&data).ok())
            .collect())
    }

    /// Publish this agent's presence record. ADR-0026 Phase 5.
    /// Subject: `KANNAKA.presence.<agent_id>` — JetStream KANNAKA_PRESENCE
    /// keeps only the latest per agent (max_msgs_per_subject=1).
    pub fn publish_presence(
        &self,
        agent_id: &str,
        payload: &serde_json::Value,
    ) -> Result<(), NatsError> {
        let subject = format!("KANNAKA.presence.{agent_id}");
        let bytes = serde_json::to_vec(payload)
            .map_err(|e| NatsError::Serialize(e.to_string()))?;
        self.publish_raw(&subject, &bytes)
    }

    /// Retract this agent's presence record (#590).
    ///
    /// The presence stream is `max_msgs_per_subject: 1`, so publishing to the
    /// agent's own subject REPLACES its live record — that is what makes a
    /// tombstone the right retraction primitive here. There is no per-subject
    /// delete, and without this the record simply ages out at the stream's
    /// 24h `max_age`, which is why a departed agent lingered as a ghost peer
    /// for a full day.
    ///
    /// Readers drop `status == "left"` in [`Self::get_presence`], so the
    /// tombstone is invisible to every consumer rather than each one needing
    /// to know about it.
    pub fn publish_presence_left(&self, agent_id: &str) -> Result<(), NatsError> {
        let mut payload = serde_json::json!({
            "agent_id": agent_id,
            "status": "left",
            "last_seen": chrono::Utc::now().to_rfc3339(),
        });
        add_envelope(&mut payload);
        self.publish_presence(agent_id, &payload)
    }

    /// Ensure the KANNAKA_PRESENCE stream exists. ADR-0026 Phase 5.
    ///
    /// #928: on the production broker an anonymous connection is denied
    /// STREAM.CREATE (ADR-0042 closed anon's control lane), and issuing it
    /// only bought a broker-side "Permissions Violation" per join. The `Err`
    /// says the create was not available to this identity; whether the stream
    /// is nevertheless THERE is `presence_stream_exists`'s question.
    ///
    /// #933: but "anonymous" is not the same as "denied". Against an open
    /// broker an anonymous client MAY create the stream, and skipping the
    /// attempt left a fresh bus with no `KANNAKA_PRESENCE` at all. So an
    /// anonymous connection probes first and attempts the create only when
    /// the stream is genuinely absent — and stops for good once this
    /// connection has actually been refused. See
    /// [`should_attempt_presence_create`].
    pub fn ensure_presence_stream(&self) -> Result<(), NatsError> {
        let authenticated = self.is_authenticated();
        let denied_here = self.stream_create_denied();
        // The probe is skipped entirely for the cases that do not need it:
        // an authenticated identity attempts the create regardless, and a
        // connection already refused never attempts it again.
        let stream_absent = !authenticated && !denied_here && !self.presence_stream_exists();
        if !should_attempt_presence_create(authenticated, denied_here, stream_absent) {
            // Name the actual reason: since the open-broker bootstrap landed,
            // an anonymous connection skips the create because the stream is
            // already there, OR because this connection has been refused once.
            // "not available to an anonymous connection" covered only the
            // second and read as a permissions verdict in both cases.
            return Err(NatsError::Protocol(
                if denied_here {
                    "this connection was refused $JS.API.STREAM.CREATE; not retrying".to_string()
                } else {
                    "presence stream already exists; no create needed".to_string()
                },
            ));
        }
        self.ensure_js_stream(
            "KANNAKA_PRESENCE",
            serde_json::json!({
                "name": "KANNAKA_PRESENCE",
                "subjects": ["KANNAKA.presence.>"],
                "retention": "limits",
                "max_msgs_per_subject": 1,
                "max_age": 86_400_000_000_000i64, // 24h — agents who haven't refreshed are gone
                "storage": "file",
                "discard": "old",
                "num_replicas": 1
            }),
        )
    }

    /// #928: does KANNAKA_PRESENCE exist on this broker, as far as THIS
    /// identity can tell? A refused create says only that this identity
    /// cannot create the stream; on a running swarm it already exists, the
    /// anonymous user's presence publishes are retained by it, and the node
    /// IS listed by other hosts.
    ///
    /// Credentialed identities ask `$JS.API.STREAM.INFO` (granted to the
    /// read-only users such as `radio`). Anonymous ones are denied INFO and
    /// NAMES but granted MSG.GET (config/nats-accounts.conf), so they probe
    /// that instead: any reply proves the stream is there EXCEPT error 10059
    /// "stream not found". A permission denial produces no reply, which is
    /// the one case that stays `false`.
    pub fn presence_stream_exists(&self) -> bool {
        self.stream_exists("KANNAKA_PRESENCE")
    }

    /// Has the broker refused `$JS.API.STREAM.CREATE` on this connection?
    /// Unknown (an unlockable connection) counts as "not refused" — the
    /// safe direction is to let the attempt happen and learn the answer.
    fn stream_create_denied(&self) -> bool {
        self.lock_conn().map(|c| c.stream_create_denied).unwrap_or(false)
    }

    fn stream_exists(&self, stream_name: &str) -> bool {
        let Ok(mut conn) = self.lock_conn() else {
            return false;
        };
        if conn.authenticated {
            let info_subject = format!("$JS.API.STREAM.INFO.{stream_name}");
            if let Ok(Some(reply)) =
                self.js_api_call_locked(&mut conn, &info_subject, b"", JS_API_TIMEOUT)
            {
                return js_reply_names_an_existing_stream(&reply);
            }
        }
        let get_subject = format!("$JS.API.STREAM.MSG.GET.{stream_name}");
        let req = serde_json::json!({ "seq": 1, "next_by_subj": ">" }).to_string();
        match self.js_api_call_locked(&mut conn, &get_subject, req.as_bytes(), JS_API_TIMEOUT) {
            Ok(Some(reply)) => js_reply_names_an_existing_stream(&reply),
            _ => false,
        }
    }

    // ──────────────────────────────────────────────────────────────────
    // ADR-0028 — event-sourced HRM + time-machine replay. See `StreamKind`
    // and `EventPayload` (defined above) for the data model. Subject order
    // puts the literal kind token (`memory` / `substrate`) at position 3
    // so JetStream's subject-overlap check sees the streams as disjoint.
    // ──────────────────────────────────────────────────────────────────

    /// Idempotently create the JetStream stream described by `kind`.
    pub fn ensure_event_stream(&self, kind: StreamKind) -> Result<(), NatsError> {
        let spec = kind.spec();
        let mut cfg = serde_json::json!({
            "name": spec.name,
            "subjects": spec.subjects,
            "retention": "limits",
            "storage": "file",
            "discard": "old",
            "num_replicas": 1,
        });
        if let Some(days) = spec.max_age_days {
            cfg["max_age"] = serde_json::json!(days * 24 * 3_600 * 1_000_000_000i64);
        }
        if let Some(n) = spec.max_msgs_per_subject {
            cfg["max_msgs_per_subject"] = serde_json::json!(n);
        }
        if let Some(sz) = spec.max_msg_size {
            cfg["max_msg_size"] = serde_json::json!(sz);
        }
        self.ensure_js_stream(spec.name, cfg)
    }

    /// Publish a durable event to its JetStream subject. The variant
    /// determines both the subject and the JSON payload shape; every
    /// event carries `event_id`, `schema_version`, `agent_id`, `ts` so
    /// replay (ADR-0028 Phase 3) is idempotent.
    ///
    /// Best-effort: failures don't abort the originating action — the
    /// in-memory state has already changed.
    pub fn publish_event(&self, event: EventPayload<'_>) -> Result<(), NatsError> {
        let subject = event.subject();
        let payload = event.payload_json();
        let bytes = serde_json::to_vec(&payload)
            .map_err(|e| NatsError::Serialize(e.to_string()))?;
        self.publish_raw(&subject, &bytes)
    }

    /// Read the presence records of peers that are actually live right now.
    ///
    /// Two filters, both applied HERE rather than in each consumer (#590).
    /// There are at least two independent readers (`swarm peers`,
    /// `brief --peers`) and filtering at the shared reader means a new one
    /// cannot forget and resurrect ghosts:
    ///
    /// - retraction tombstones (`status == "left"`, from a clean
    ///   [`Self::publish_presence_left`]) — a departure the agent announced;
    /// - records whose `last_seen` has gone stale (#737) — a departure nobody
    ///   announced, which is the common case: a crash, a lost network, or a
    ///   closed laptop writes no tombstone at all, so without this the dead
    ///   node advertised itself as reachable for the stream's full 24h
    ///   `max_age`.
    ///
    /// Use [`Self::get_presence_all`] for the forensic view that keeps stale
    /// records.
    pub fn get_presence(&self) -> Result<Vec<serde_json::Value>, NatsError> {
        let now = Utc::now();
        Ok(self
            .get_presence_all()?
            .into_iter()
            .filter(|p| is_fresh_presence(p, now))
            .collect())
    }

    /// Every non-tombstoned presence record the stream still retains, stale
    /// ones included.
    ///
    /// For forensics and for `swarm peers --all`, which reports staleness
    /// rather than hiding it. Anything that presents peers as *reachable* —
    /// counts, capability advertisements, request fan-out — wants
    /// [`Self::get_presence`] instead.
    pub fn get_presence_all(&self) -> Result<Vec<serde_json::Value>, NatsError> {
        let all = self.get_stream_messages("KANNAKA_PRESENCE", "KANNAKA.presence.>", 200)?;
        Ok(all.into_iter().filter(is_active_presence).collect())
    }

    /// Publish a wave-signature-only absorb event to
    /// `KANNAKA.substrate.absorb.<agent_id>` (ADR-0027 Phase 1).
    ///
    /// Privacy by design: ONLY the wave signature crosses the boundary —
    /// `class_index`, `amplitude`, `phase`, `frequency`. No content, no
    /// memory id, no tags. The receiving substrate (kannaka-prime) adapts
    /// the signature into its fixed-topology 96-class HRM. Content stays
    /// in the sending agent's local HRM where it belongs.
    pub fn publish_substrate_absorb(
        &self,
        agent_id: &str,
        class_index: u32,
        amplitude: f32,
        phase: f32,
        frequency: f32,
    ) -> Result<(), NatsError> {
        let subject = format!("KANNAKA.substrate.absorb.{agent_id}");
        let mut payload = serde_json::json!({
            "agent_id": agent_id,
            "class_index": class_index,
            "amplitude": amplitude,
            "phase": phase,
            "frequency": frequency,
        });
        add_envelope(&mut payload);
        let bytes = serde_json::to_vec(&payload)
            .map_err(|e| NatsError::Serialize(e.to_string()))?;
        self.publish_raw(&subject, &bytes)
    }

    /// Publish collective consciousness metrics from the substrate to
    /// `KANNAKA.substrate.phi` (ADR-0027 Phase 1). Periodically emitted by
    /// `kannaka substrate run` so observatory + radio can display the
    /// constellation's integrated Phi separately from any individual
    /// agent's local Phi.
    pub fn publish_substrate_phi(
        &self,
        phi: f32,
        xi: f32,
        order: f32,
        num_clusters: usize,
        total_wavefronts: usize,
        contributing_agents: &[String],
    ) -> Result<(), NatsError> {
        // agent_id was missing pre-fix, which made observatory attribute
        // the collective metric to "unknown". Stamping the substrate's
        // identity here. (#91)
        let mut payload = serde_json::json!({
            "agent_id": "kannaka-substrate",
            "collective_phi": phi,
            "collective_xi": xi,
            "collective_order": order,
            "num_active_clusters": num_clusters,
            "total_wavefronts": total_wavefronts,
            "contributing_agents": contributing_agents,
            "source": "substrate",
        });
        add_envelope(&mut payload);
        let bytes = serde_json::to_vec(&payload)
            .map_err(|e| NatsError::Serialize(e.to_string()))?;
        self.publish_raw("KANNAKA.substrate.phi", &bytes)
    }

    /// Publish a new memory to KANNAKA.memory.new for cross-agent synchronization.
    ///
    /// The payload is a JSON object wrapping the HyperMemory plus the source agent_id
    /// so receivers can skip their own messages. Optional `memory_count` and
    /// `cluster_count` fields let the radio's swarm aggregator surface the
    /// sender's authoritative counts even when the sender isn't running a
    /// `swarm join` daemon — without these, agents that only `remember` from
    /// the CLI always showed up with cl:0 in the per-agent dropdown.
    pub fn publish_memory_new_with_counts(
        &self,
        memory: &crate::memory::HyperMemory,
        agent_id: &str,
        memory_count: usize,
        cluster_count: usize,
        sig: Option<&crate::provenance::ProvenanceSig>,
    ) -> Result<(), NatsError> {
        let mut payload = serde_json::json!({
            "agent_id": agent_id,
            "memory": memory,
            "memory_count": memory_count,
            "cluster_count": cluster_count,
        });
        add_envelope(&mut payload);
        // inc-1b: attach the optional provenance signature so peers running the
        // corroboration gate can verify + accrue. Additive + harmless when off.
        if let Some(s) = sig {
            if let (Some(obj), Ok(v)) = (payload.as_object_mut(), serde_json::to_value(s)) {
                obj.insert("provenance_sig".to_string(), v);
            }
        }
        let bytes = serde_json::to_vec(&payload)
            .map_err(|e| NatsError::Serialize(e.to_string()))?;
        // #562: the README documents this as THE memory-sync path, so an
        // ACL refusal here has to surface rather than look like a no-op.
        self.publish_raw_confirmed("KANNAKA.memory.new", &bytes)
    }

    /// Backward-compat shim — same as publish_memory_new_with_counts but
    /// without the memory/cluster counts. Kept so older call sites keep
    /// compiling; new call sites should prefer the _with_counts variant.
    pub fn publish_memory_new(
        &self,
        memory: &crate::memory::HyperMemory,
        agent_id: &str,
    ) -> Result<(), NatsError> {
        self.publish_memory_new_with_counts(memory, agent_id, 0, 0, None)
    }

    // -----------------------------------------------------------------------
    // Heartbeat beacons (inc-1b PART A anti-eclipse)
    // -----------------------------------------------------------------------

    /// Publish a signed heartbeat [`Beacon`](crate::beacon::Beacon) to
    /// [`BEACON_SUBJECT`](crate::beacon::BEACON_SUBJECT). A seed emits one per
    /// epoch; receivers (`swarm listen --auto-sync`) verify + feed
    /// `QuarantineStaging::ingest_beacon` so an ARMED corroboration gate sees a
    /// fresh beacon and un-freezes Live promotion (a stale/absent seed beacon
    /// freezes it — anti-eclipse fail-closed).
    ///
    /// The subject is under `KANNAKA.events.>`, which the deployed server's
    /// anon ACL already allows, so no server ACL change is required. The
    /// canonical `schema_version`/`ts` envelope is stamped (matching every other
    /// publisher); the extra keys are ignored when the beacon is decoded.
    pub fn publish_beacon(&self, beacon: &crate::beacon::Beacon) -> Result<(), NatsError> {
        let mut value =
            serde_json::to_value(beacon).map_err(|e| NatsError::Serialize(e.to_string()))?;
        add_envelope(&mut value);
        let payload =
            serde_json::to_vec(&value).map_err(|e| NatsError::Serialize(e.to_string()))?;
        self.publish_raw(crate::beacon::BEACON_SUBJECT, &payload)
    }

    // -----------------------------------------------------------------------
    // SharedWavefront protocol (QS-5, #56)
    // -----------------------------------------------------------------------

    /// Publish a shared wavefront for resonance-based memory sharing.
    ///
    /// Subject: `queen.memory.shared.{target}` where target is an agent_id
    /// or "broadcast" for all agents. JetStream retains for 7 days.
    pub fn publish_shared_wavefront(&self, wavefront: &SharedWavefront) -> Result<(), NatsError> {
        let target = wavefront.target.as_deref().unwrap_or("broadcast");
        let subject = format!("queen.memory.shared.{target}");
        let payload = serde_json::to_vec(wavefront)
            .map_err(|e| NatsError::Serialize(e.to_string()))?;
        self.publish_raw(&subject, &payload)
    }

    /// Publish a dream lifecycle event (start or end).
    ///
    /// Used by swarm-aware consolidation to coordinate dream timing.
    pub fn publish_dream_lifecycle(
        &self,
        event_type: &str,
        agent_id: &str,
        details: &serde_json::Value,
    ) -> Result<(), NatsError> {
        // #705 residual: the contract requires dream-report fields
        // (memories_strengthened, memories_faded, ...) at the TOP level of
        // queen.event.dream.end — nesting them under `details` hid them
        // from every contract-validating consumer. Flatten object details
        // into the payload; agent_id/timestamp keep priority on collision.
        let mut payload = serde_json::json!({
            "agent_id": agent_id,
            "timestamp": chrono::Utc::now().to_rfc3339(),
        });
        if let Some(obj) = payload.as_object_mut() {
            match details.as_object() {
                Some(detail_map) => {
                    for (k, v) in detail_map {
                        obj.entry(k.clone()).or_insert_with(|| v.clone());
                    }
                }
                // Non-object details (scalar/array) can't flatten — keep the
                // legacy nesting rather than dropping the data.
                None if !details.is_null() => {
                    obj.insert("details".to_string(), details.clone());
                }
                None => {}
            }
        }
        let event_name = format!("dream.{event_type}");
        self.announce_event(&event_name, &payload)
    }

    // -----------------------------------------------------------------------
    // Phase reading
    // -----------------------------------------------------------------------

    /// Read all current agent phases.
    pub fn get_all_phases(&self) -> Result<Vec<AgentPhase>, NatsError> {
        if self.jetstream_ok {
            self.get_all_phases_jetstream()
        } else {
            self.get_all_phases_legacy()
        }
    }

    /// Fetch all phases from JetStream by iterating stored messages.
    fn get_all_phases_jetstream(&self) -> Result<Vec<AgentPhase>, NatsError> {
        let msgs = self.stream_walk(STREAM_NAME, "QUEEN.phase.>", STREAM_WALK_LIMIT)?;
        let mut phases: HashMap<String, AgentPhase> = HashMap::new();
        for (_, _, data) in msgs {
            if let Ok(phase) = serde_json::from_slice::<AgentPhase>(&data) {
                phases.insert(phase.agent_id.clone(), phase);
            }
        }
        Ok(phases.into_values().collect())
    }

    /// Agents with a QUEEN.phase.* message ingested by the SERVER within
    /// `max_age`. Identity is the subject token, so publishers whose payloads
    /// don't conform to AgentPhase (no timestamp, missing fields) still
    /// count; liveness is broker receive time, so a skewed or absent
    /// publisher clock can't fake or forfeit freshness. This is the raw
    /// "observed on the wire" peer set — trust filtering stays separate.
    pub fn live_phase_agents(
        &self,
        max_age: chrono::Duration,
    ) -> Result<Vec<String>, NatsError> {
        let msgs = self.stream_walk(STREAM_NAME, "QUEEN.phase.>", STREAM_WALK_LIMIT)?;
        let now = Utc::now();
        let mut out = std::collections::HashSet::new();
        for (subject, time, _) in msgs {
            let Ok(t) = chrono::DateTime::parse_from_rfc3339(&time) else {
                continue;
            };
            if now.signed_duration_since(t.with_timezone(&Utc)) < max_age {
                if let Some(id) = subject.strip_prefix("QUEEN.phase.") {
                    out.insert(id.to_string());
                }
            }
        }
        Ok(out.into_iter().collect())
    }

    /// Legacy PUB/SUB phase collection (fallback when JetStream is unavailable).
    /// Subscribes to the live gossip subject and collects whatever arrives
    /// until 1.5s of silence (capped at 10s total). Only frames carrying our
    /// own sid are interpreted as phases.
    fn get_all_phases_legacy(&self) -> Result<Vec<AgentPhase>, NatsError> {
        let mut conn = self.lock_conn()?;
        let sid = self.alloc_sid();
        let sid_str = sid.to_string();

        conn.write_frames(format!("SUB QUEEN.phase.* {sid}\r\nPING\r\n").as_bytes())?;

        let prev = conn.read_timeout();
        conn.set_read_timeout(Some(Duration::from_millis(1500)))?;
        let deadline = Instant::now() + Duration::from_secs(10);

        let mut phases: HashMap<String, AgentPhase> = HashMap::new();
        let mut fatal: Option<NatsError> = None;
        loop {
            if Instant::now() > deadline {
                break;
            }
            match read_frame(&mut conn.reader) {
                Ok(ReadOutcome::Frame(Frame::Msg { sid: msid, payload, .. })) => {
                    if msid == sid_str {
                        if let Ok(phase) = serde_json::from_slice::<AgentPhase>(&payload) {
                            phases.insert(phase.agent_id.clone(), phase);
                        }
                    }
                }
                Ok(ReadOutcome::Frame(Frame::Ping)) => {
                    let _ = conn.pong();
                }
                Ok(ReadOutcome::Frame(Frame::ServerErr(m))) => {
                    eprintln!("[nats] server error: {m}");
                    if is_auth_error(&m) {
                        fatal = Some(NatsError::Disconnected(m));
                        break;
                    }
                }
                Ok(ReadOutcome::Frame(_)) => continue,
                Ok(ReadOutcome::TimedOut) | Ok(ReadOutcome::Closed) => break,
                Err(e) => {
                    fatal = Some(e);
                    break;
                }
            }
        }

        let _ = conn.write_frames(format!("UNSUB {sid}\r\n").as_bytes());
        let _ = conn.set_read_timeout(prev);

        if let Some(e) = fatal {
            drop(conn);
            self.mark_disconnected();
            return Err(e);
        }
        Ok(phases.into_values().collect())
    }

    // -----------------------------------------------------------------------
    // Structured event publishing
    // -----------------------------------------------------------------------

    /// Publish a structured event to `queen.event.<event_type>`.
    ///
    /// Event types: "join", "leave", "dream.start", "dream.end", "memory.shared".
    /// Events are stored in the QUEEN_EVENTS JetStream stream (if available).
    ///
    /// Pre-fix this published to uppercase `QUEEN.event.<type>` with a
    /// wrapper envelope `{event, timestamp, payload}` — NATS subjects are
    /// case-sensitive, so kannaka-radio (which subscribes to lowercase
    /// `queen.event.<type>` per the contract, expecting a flat payload)
    /// never received them. (#88)
    pub fn announce_event(
        &self,
        event_type: &str,
        payload: &serde_json::Value,
    ) -> Result<(), NatsError> {
        // Flatten: merge payload fields with the envelope. The subject
        // already names the event, so the `event` wrapper field is gone.
        let mut flat = match payload {
            serde_json::Value::Object(map) => serde_json::Value::Object(map.clone()),
            _ => serde_json::json!({}),
        };
        if let Some(obj) = flat.as_object_mut() {
            obj.entry("event_type".to_string())
                .or_insert_with(|| serde_json::Value::String(event_type.to_string()));
        }
        add_envelope(&mut flat);
        let bytes = serde_json::to_vec(&flat)
            .map_err(|e| NatsError::Serialize(e.to_string()))?;
        let subject = format!("queen.event.{event_type}");
        self.publish_raw(&subject, &bytes)
    }

    /// Announce joining the swarm.
    ///
    /// Publishes to both the new event stream (`QUEEN.event.join`) and the
    /// legacy `QUEEN.announce` subject for backward compatibility.
    pub fn announce_join(&self, agent_id: &str) -> Result<(), NatsError> {
        self.announce_join_with_identity(agent_id, None)
    }

    /// Announce joining the swarm, optionally carrying the operator's
    /// stored identity (swarm agent identity, step 2). `identity = None`
    /// publishes payloads byte-identical to `announce_join` pre-identity,
    /// so v0.6.19/0.6.20 peers see no change.
    pub fn announce_join_with_identity(
        &self,
        agent_id: &str,
        identity: Option<&AnnounceIdentity>,
    ) -> Result<(), NatsError> {
        let mut event_payload = serde_json::json!({ "agent_id": agent_id });
        if let Some(idn) = identity {
            idn.attach_to(&mut event_payload);
        }
        self.announce_event("join", &event_payload)?;

        let mut legacy = legacy_announce_payload("join", agent_id, identity);
        add_envelope(&mut legacy);
        let bytes = serde_json::to_vec(&legacy)
            .map_err(|e| NatsError::Serialize(e.to_string()))?;
        self.publish_raw("QUEEN.announce", &bytes)
    }

    /// Announce leaving the swarm.
    ///
    /// Publishes to both the new event stream (`QUEEN.event.leave`) and the
    /// legacy `QUEEN.announce` subject for backward compatibility.
    pub fn announce_leave(&self, agent_id: &str) -> Result<(), NatsError> {
        let event_payload = serde_json::json!({ "agent_id": agent_id });
        self.announce_event("leave", &event_payload)?;

        let mut legacy = serde_json::json!({
            "event": "leave",
            "agent_id": agent_id,
        });
        add_envelope(&mut legacy);
        let bytes = serde_json::to_vec(&legacy)
            .map_err(|e| NatsError::Serialize(e.to_string()))?;
        self.publish_raw("QUEEN.announce", &bytes)
    }

    // -----------------------------------------------------------------------
    // JetStream KV bucket operations
    // -----------------------------------------------------------------------

    /// Create a NATS JetStream KV bucket.
    ///
    /// Under the hood this creates a stream named `KV_<name>` with subjects
    /// `$KV.<name>.>`, max 1 message per subject (last-value semantics),
    /// and an optional TTL in seconds.
    pub fn create_kv_bucket(&self, name: &str, ttl_seconds: u64) -> Result<(), NatsError> {
        let stream_name = format!("KV_{name}");
        let subjects = format!("$KV.{name}.>");
        let config = if ttl_seconds > 0 {
            serde_json::json!({
                "name": stream_name,
                "subjects": [subjects],
                "retention": "limits",
                "max_msgs_per_subject": 1,
                "max_age": ttl_seconds * 1_000_000_000_u64,
                "storage": "file",
                "discard": "old",
                "num_replicas": 1,
                "allow_rollup_hdrs": true
            })
        } else {
            serde_json::json!({
                "name": stream_name,
                "subjects": [subjects],
                "retention": "limits",
                "max_msgs_per_subject": 1,
                "storage": "file",
                "discard": "old",
                "num_replicas": 1,
                "allow_rollup_hdrs": true
            })
        };
        self.ensure_js_stream(&stream_name, config)
    }

    /// Put a value into a NATS KV bucket.
    pub fn kv_put(&self, bucket: &str, key: &str, value: &str) -> Result<(), NatsError> {
        let subject = format!("$KV.{bucket}.{key}");
        self.publish_raw(&subject, value.as_bytes())
    }

    /// Get a value from a NATS KV bucket.
    ///
    /// Returns the latest value for the given key, or `NatsError::KvNotFound`
    /// if the key does not exist (or the server didn't reply in time).
    pub fn kv_get(&self, bucket: &str, key: &str) -> Result<String, NatsError> {
        let stream_name = format!("KV_{bucket}");
        let target_subject = format!("$KV.{bucket}.{key}");
        let api_subject = format!("$JS.API.STREAM.MSG.GET.{stream_name}");
        let req = serde_json::json!({ "last_by_subj": target_subject }).to_string();

        let resp = {
            let mut conn = self.lock_conn()?;
            self.js_api_call_locked(&mut conn, &api_subject, req.as_bytes(), JS_API_TIMEOUT)?
        };

        resp.as_ref()
            .filter(|r| r.get("error").is_none())
            .and_then(|r| r.get("message"))
            .and_then(|m| m.get("data"))
            .and_then(|d| d.as_str())
            .and_then(|b64| base64_decode(b64).ok())
            .map(|decoded| String::from_utf8_lossy(&decoded).to_string())
            .ok_or_else(|| NatsError::KvNotFound(format!("{bucket}/{key}")))
    }

    /// List all keys in a NATS KV bucket.
    pub fn kv_keys(&self, bucket: &str) -> Result<Vec<String>, NatsError> {
        let stream_name = format!("KV_{bucket}");
        let prefix = format!("$KV.{bucket}.");
        let filter = format!("$KV.{bucket}.>");
        let msgs = self.stream_walk(&stream_name, &filter, STREAM_WALK_LIMIT)?;
        Ok(msgs
            .into_iter()
            .filter_map(|(subject, _, _)| subject.strip_prefix(&prefix).map(String::from))
            .collect())
    }

    // QUEEN_AGENTS discovery removed (#582): `discover_peers` was consumed by
    // nothing but its own integration test. The peer directory is
    // KANNAKA.presence; trust is reputation-derived (collective/trust.rs),
    // never registration-derived -- a self-registration carries no trust
    // signal a Sybil could not forge.

    // -----------------------------------------------------------------------
    // Subscriptions / PING
    // -----------------------------------------------------------------------

    /// Subscribe to phase updates and memory sync messages.
    ///
    /// Subscribes to:
    /// - `QUEEN.phase.*` — agent phase gossip
    /// - `QUEEN.announce` — join/leave announcements
    /// - `KANNAKA.memory.new` + `KANNAKA.dreams` — memory sync (when
    ///   `include_memories` is true)
    pub fn subscribe_phases_and_memories(&self, include_memories: bool) -> Result<NatsSubscription, NatsError> {
        let mut conn = self.lock_conn()?;
        let first_sid = self.alloc_sid();
        let mut frame = Vec::new();
        let _ = write!(frame, "SUB QUEEN.phase.* {first_sid}\r\n");
        let _ = write!(frame, "SUB QUEEN.announce {}\r\n", self.alloc_sid());
        if include_memories {
            let _ = write!(frame, "SUB KANNAKA.memory.new {}\r\n", self.alloc_sid());
            let _ = write!(frame, "SUB KANNAKA.dreams {}\r\n", self.alloc_sid());
            // PART A anti-eclipse: heartbeat beacons ride the SAME auto-sync
            // subscription so the corroboration gate's freshness advances from
            // the one reader this connection already polls. Under the
            // anon-allowed KANNAKA.events.> ACL (no server ACL change).
            let _ = write!(frame, "SUB {} {}\r\n", crate::beacon::BEACON_SUBJECT, self.alloc_sid());
        }
        conn.write_frames(&frame)?;
        let stream_clone = conn.writer.try_clone()?;
        // Finite read timeout so `next_event` can run its liveness check even
        // if the caller never calls `set_timeout` (#500). This subscription
        // also carries beacon heartbeats, so a silent death here would freeze
        // the corroboration gate — it MUST detect the dead socket and exit for
        // a systemd restart rather than hang deaf.
        stream_clone.set_read_timeout(Some(SUB_POLL_MAX))?;

        let authenticated = conn.authenticated;
        let mut sub = NatsSubscription {
            reader: BufReader::new(stream_clone),
            sid: first_sid.to_string(),
            // Multi-subject subscription — resubscribe_via (#706) cannot
            // rebuild it from a single subject; callers recreate it via
            // subscribe_phases_and_memories on the reconnected transport.
            subject: String::new(),
            queue_group: None,
            last_frame: Instant::now(),
            probe_sent: false,
            ping_idle: SUB_PING_IDLE,
            liveness_timeout: SUB_LIVENESS_TIMEOUT,
            pending: VecDeque::new(),
            denied: Vec::new(),
        };
        // #562: COLLECT refusals rather than failing. Anon may read
        // QUEEN.phase.* but not KANNAKA.memory.new, and killing the whole
        // subscription would take working phase gossip down to report a dead
        // auto-sync. `denied_subjects()` lets the caller say precisely which
        // advertised feature is off.
        sub.collect_denials(authenticated);
        Ok(sub)
    }

    /// Subscribe to phase updates. Returns a NatsSubscription that can be iterated.
    pub fn subscribe_phases(&self) -> Result<NatsSubscription, NatsError> {
        self.subscribe_phases_and_memories(false)
    }

    /// Subscribe to memory sync messages only. Returns a NatsSubscription.
    pub fn subscribe_memories(&self) -> Result<NatsSubscription, NatsError> {
        self.subscribe_with_queue("KANNAKA.memory.new", None)
    }

    /// Send a PING and wait for the PONG (2s budget) to check connection
    /// health. A write that merely lands in a dead socket's kernel buffer
    /// no longer counts as "connected". Marks the transport disconnected
    /// on failure. The previous read timeout is restored on all paths.
    pub fn ping(&self) -> Result<(), NatsError> {
        let mut conn = self.lock_conn()?;
        let prev = conn.read_timeout();
        let _ = conn.set_read_timeout(Some(Duration::from_secs(2)));
        let deadline = Instant::now() + Duration::from_secs(2);

        let result = (|conn: &mut Conn| -> Result<(), NatsError> {
            conn.write_frames(b"PING\r\n")?;
            loop {
                if Instant::now() > deadline {
                    return Err(NatsError::Protocol("no PONG within 2s".to_string()));
                }
                match read_frame(&mut conn.reader) {
                    Ok(ReadOutcome::Frame(Frame::Pong)) => return Ok(()),
                    Ok(ReadOutcome::Frame(Frame::Ping)) => {
                        let _ = conn.pong();
                    }
                    Ok(ReadOutcome::Frame(Frame::ServerErr(m))) => {
                        eprintln!("[nats] server error: {m}");
                        if is_auth_error(&m) {
                            return Err(NatsError::Disconnected(m));
                        }
                    }
                    Ok(ReadOutcome::Frame(_)) => continue,
                    Ok(ReadOutcome::TimedOut) => {
                        return Err(NatsError::Protocol(
                            "timed out waiting for PONG".to_string(),
                        ));
                    }
                    Ok(ReadOutcome::Closed) => {
                        return Err(NatsError::Disconnected(
                            "connection closed".to_string(),
                        ));
                    }
                    Err(e) => return Err(e),
                }
            }
        })(&mut conn);

        let _ = conn.set_read_timeout(prev);
        if result.is_err() {
            drop(conn);
            self.mark_disconnected();
        }
        result
    }

    // -----------------------------------------------------------------------
    // Request / Reply (ADR-0026 Phase 1)
    // -----------------------------------------------------------------------

    /// Synchronous request: publish to `subject` with a unique reply-to inbox,
    /// await the FIRST reply on that inbox, return its payload. Used for
    /// directed agent queries (`kannaka ask --remote <agent_id>`).
    ///
    /// Holds the connection lock for the whole exchange. Replies are matched
    /// by inbox subject — messages for other subscriptions on this connection
    /// are consumed and skipped, never returned as the reply. The previous
    /// read timeout is restored on all paths.
    pub fn request_one(
        &self,
        subject: &str,
        payload: &[u8],
        timeout: Duration,
    ) -> Result<Vec<u8>, NatsError> {
        let inbox = new_inbox("req");
        let sid = self.alloc_sid();
        let mut conn = self.lock_conn()?;

        // SUB inbox first so we don't race the reply; UNSUB <sid> 1 lets the
        // server clean up automatically after the first delivery.
        let mut frame = Vec::with_capacity(payload.len() + 128);
        let _ = write!(frame, "SUB {inbox} {sid}\r\n");
        let _ = write!(frame, "PUB {} {} {}\r\n", subject, inbox, payload.len());
        frame.extend_from_slice(payload);
        frame.extend_from_slice(b"\r\n");
        let _ = write!(frame, "UNSUB {sid} 1\r\n");
        conn.write_frames(&frame)?;

        let prev = conn.read_timeout();
        conn.set_read_timeout(Some(timeout))?;
        let deadline = Instant::now() + timeout;

        let mut fatal = false;
        let result = loop {
            if Instant::now() > deadline {
                break Err(NatsError::Protocol("request_one timed out".to_string()));
            }
            match read_frame(&mut conn.reader) {
                Ok(ReadOutcome::Frame(Frame::Msg { subject: msub, payload, .. }))
                    if msub == inbox =>
                {
                    break Ok(payload);
                }
                Ok(ReadOutcome::Frame(Frame::Msg { .. })) => continue,
                Ok(ReadOutcome::Frame(Frame::Ping)) => {
                    let _ = conn.pong();
                }
                Ok(ReadOutcome::Frame(Frame::ServerErr(m))) => {
                    eprintln!("[nats] server error: {m}");
                    if is_auth_error(&m) {
                        fatal = true;
                        break Err(NatsError::Disconnected(m));
                    }
                }
                Ok(ReadOutcome::Frame(_)) => continue,
                Ok(ReadOutcome::TimedOut) => {
                    break Err(NatsError::Protocol("request_one timed out".to_string()));
                }
                Ok(ReadOutcome::Closed) => {
                    fatal = true;
                    break Err(NatsError::Disconnected(
                        "connection closed during request".to_string(),
                    ));
                }
                Err(e) => {
                    fatal = true;
                    break Err(e);
                }
            }
        };

        if result.is_err() {
            // No reply was delivered, so the auto-unsub never fired —
            // remove the subscription explicitly.
            let _ = conn.write_frames(format!("UNSUB {sid}\r\n").as_bytes());
        }
        let _ = conn.set_read_timeout(prev);
        if fatal {
            drop(conn);
            self.mark_disconnected();
        }
        result
    }

    /// Broadcast request: publish to `subject` with a reply-to inbox, collect
    /// every reply that arrives on that inbox within `timeout`, return them
    /// all. Used for `kannaka ask --remote broadcast` where any number of
    /// agents may answer. The previous read timeout is restored on all paths.
    pub fn request_many(
        &self,
        subject: &str,
        payload: &[u8],
        timeout: Duration,
    ) -> Result<Vec<Vec<u8>>, NatsError> {
        let inbox = new_inbox("reqm");
        let sid = self.alloc_sid();
        let mut conn = self.lock_conn()?;

        let mut frame = Vec::with_capacity(payload.len() + 128);
        let _ = write!(frame, "SUB {inbox} {sid}\r\n");
        let _ = write!(frame, "PUB {} {} {}\r\n", subject, inbox, payload.len());
        frame.extend_from_slice(payload);
        frame.extend_from_slice(b"\r\n");
        conn.write_frames(&frame)?;

        let prev = conn.read_timeout();
        conn.set_read_timeout(Some(Duration::from_millis(500)))?;
        let deadline = Instant::now() + timeout;

        let mut replies: Vec<Vec<u8>> = Vec::new();
        let mut fatal = false;
        while Instant::now() < deadline {
            match read_frame(&mut conn.reader) {
                Ok(ReadOutcome::Frame(Frame::Msg { subject: msub, payload, .. }))
                    if msub == inbox =>
                {
                    replies.push(payload);
                }
                Ok(ReadOutcome::Frame(Frame::Msg { .. })) => continue,
                Ok(ReadOutcome::Frame(Frame::Ping)) => {
                    let _ = conn.pong();
                }
                Ok(ReadOutcome::Frame(Frame::ServerErr(m))) => {
                    eprintln!("[nats] server error: {m}");
                    if is_auth_error(&m) {
                        fatal = true;
                        break;
                    }
                }
                Ok(ReadOutcome::Frame(_)) => continue,
                // Clean per-read timeout: the outer loop re-checks the deadline.
                Ok(ReadOutcome::TimedOut) => continue,
                Ok(ReadOutcome::Closed) => {
                    fatal = true;
                    break;
                }
                Err(e) => {
                    // Hard error (desync, reset) — do NOT spin on it.
                    eprintln!("[nats] request_many read error: {e}");
                    fatal = true;
                    break;
                }
            }
        }

        // UNSUB so the server stops delivering on this inbox.
        let _ = conn.write_frames(format!("UNSUB {sid}\r\n").as_bytes());
        let _ = conn.set_read_timeout(prev);
        if fatal {
            drop(conn);
            self.mark_disconnected();
        }
        Ok(replies)
    }

    /// Publish a reply to the given reply-to subject. Used by `swarm serve`
    /// after handling an ask.
    pub fn reply(&self, reply_to: &str, payload: &[u8]) -> Result<(), NatsError> {
        self.publish_raw(reply_to, payload)
    }

    /// Open a long-running subscription on `subject` and return a stream of
    /// messages. The caller drives the loop and decides when to stop.
    /// Note: while the subscription is open, calls to other methods on this
    /// transport will block (single-mutex stream) and the two readers would
    /// steal each other's bytes; a serving agent should run on a dedicated
    /// SwarmTransport instance per subscription.
    pub fn subscribe(&self, subject: &str) -> Result<NatsSubscription, NatsError> {
        self.subscribe_with_queue(subject, None)
    }

    /// Subscribe with an optional queue group. NATS delivers each message to
    /// exactly ONE subscriber per queue group — this is how we get
    /// work-queue semantics across multiple workers (#74). Wire format:
    ///   SUB <subject> [queue-group] <sid>
    pub fn subscribe_with_queue(
        &self,
        subject: &str,
        queue_group: Option<&str>,
    ) -> Result<NatsSubscription, NatsError> {
        let mut conn = self.lock_conn()?;
        let sid = self.alloc_sid();
        match queue_group {
            Some(g) => conn.write_frames(format!("SUB {subject} {g} {sid}\r\n").as_bytes())?,
            None => conn.write_frames(format!("SUB {subject} {sid}\r\n").as_bytes())?,
        }
        let authenticated = conn.authenticated;
        let stream_clone = conn.writer.try_clone().map_err(NatsError::Io)?;
        // NOTE: the read timeout is a property of the underlying socket,
        // which is shared with the transport — this also clears the
        // transport's default timeout. Acceptable under the documented
        // one-subscription-per-connection model.
        //
        // A finite cap (not None) is REQUIRED for liveness (#500): an
        // infinite read blocks forever on a silently-dead socket and never
        // wakes to run the liveness check. `set_timeout(None)` and any value
        // above SUB_POLL_MAX are clamped to it.
        stream_clone.set_read_timeout(Some(SUB_POLL_MAX))?;
        let mut sub = NatsSubscription {
            reader: BufReader::new(stream_clone),
            sid: sid.to_string(),
            subject: subject.to_string(),
            queue_group: queue_group.map(str::to_string),
            last_frame: Instant::now(),
            probe_sent: false,
            ping_idle: SUB_PING_IDLE,
            liveness_timeout: SUB_LIVENESS_TIMEOUT,
            pending: VecDeque::new(),
            denied: Vec::new(),
        };
        // #562: confirm the broker ACCEPTED the SUB before handing back a
        // subscription. Pre-fix a denied subject returned Ok and the caller
        // blocked forever on a subscription the server had discarded — `swarm
        // serve` printed "press Ctrl+C to stop" and sat deaf.
        //
        // Done on the SUBSCRIPTION's reader, not the transport's: they are
        // separate BufReaders over the same socket, so a frame consumed by one
        // is invisible to the other. Reading here keeps any raced MSG inside
        // this subscription's own pending queue.
        sub.confirm_accepted(authenticated)?;
        Ok(sub)
    }
}

/// Subscription liveness (#500).
///
/// A subscription socket only ever ANSWERS server PINGs and never enables TCP
/// keepalive, so a silent connection death (NAT/conntrack drop, firewall RST
/// loss, peer power-off with no FIN/RST) delivers nothing forever and the
/// listener/worker hangs deaf while looking alive. We defend on two axes:
///  - `SUB_POLL_MAX` caps the socket read timeout so an otherwise-blocking
///    `recv` wakes periodically to run the liveness check (a `None`/infinite
///    timeout is clamped to this).
///  - after `SUB_PING_IDLE` of total silence we PROACTIVELY send our own PING
///    (the client never used to initiate one) so liveness no longer depends on
///    the server's ~2min ping_interval, and after `SUB_LIVENESS_TIMEOUT` of
///    silence with no frame of any kind we declare the socket dead (`Closed`).
const SUB_POLL_MAX: Duration = Duration::from_secs(30);
const SUB_PING_IDLE: Duration = Duration::from_secs(60);
const SUB_LIVENESS_TIMEOUT: Duration = Duration::from_secs(150);

/// What the liveness policy wants after an idle read timeout on a subscription.
#[derive(Debug, PartialEq, Eq)]
enum LivenessAction {
    /// Still within tolerance — report `Timeout` and keep polling.
    Poll,
    /// Idle past the ping threshold and no probe outstanding — send a client
    /// PING to elicit a PONG, then keep polling.
    Ping,
    /// Idle past the death threshold — declare the connection `Closed`.
    Dead,
}

/// Pure liveness decision (kept separate from the socket so it is unit-testable
/// without a live server). `idle` is time since the last frame of any kind;
/// `probe_outstanding` is whether we already sent a PING this idle gap.
fn liveness_action(
    idle: Duration,
    probe_outstanding: bool,
    ping_idle: Duration,
    deadline: Duration,
) -> LivenessAction {
    if idle >= deadline {
        LivenessAction::Dead
    } else if idle >= ping_idle && !probe_outstanding {
        LivenessAction::Ping
    } else {
        LivenessAction::Poll
    }
}

/// A subscription that yields NATS messages.
pub struct NatsSubscription {
    reader: BufReader<TcpStream>,
    /// The (first) subscription id registered for this subscription.
    sid: String,
    /// Subject + queue group this subscription was created with — kept so a
    /// dead subscription can be rebuilt on a reconnected transport (#706).
    subject: String,
    queue_group: Option<String>,
    /// Instant of the last frame of ANY kind read from the server. Reset on
    /// every Msg/Ping/Pong/etc.; drives the idle-death check (#500).
    last_frame: Instant,
    /// Set when we sent a client PING during the current idle gap and are
    /// awaiting its PONG. Cleared when any frame arrives. Stops us re-PINGing
    /// on every poll.
    probe_sent: bool,
    /// Idle gap after which we proactively PING (defaulted from
    /// `SUB_PING_IDLE`; a field so tests can shrink it).
    ping_idle: Duration,
    /// Idle gap after which the socket is declared dead (defaulted from
    /// `SUB_LIVENESS_TIMEOUT`; a field so tests can shrink it).
    liveness_timeout: Duration,
    /// Messages read off the socket by [`Self::confirm_accepted`] before the
    /// caller's first `next_event`.
    ///
    /// The acceptance round-trip (#562) has to read frames to see the server's
    /// answer, and a MSG for this very subscription can arrive ahead of the
    /// PONG. Frames consumed there are GONE from the socket, so they are
    /// stashed here and drained first rather than silently dropped — losing
    /// the first message of a busy subject would be a nastier bug than the
    /// silent-no-op this confirmation exists to fix.
    pending: VecDeque<NatsMessage>,
    /// Subjects the broker refused on a MULTI-subject subscription (#562).
    ///
    /// A single-subject subscribe fails outright when refused. A multi-subject
    /// one cannot: `swarm listen --auto-sync` bundles `QUEEN.phase.*` (which
    /// anon may read) with `KANNAKA.memory.new` (which it may not), so failing
    /// the whole thing would kill working phase gossip to report a dead
    /// auto-sync. Denials are recorded here instead and the caller decides
    /// which advertised feature to declare unavailable.
    denied: Vec<String>,
}

/// A received NATS message.
#[derive(Debug)]
pub struct NatsMessage {
    pub subject: String,
    pub payload: Vec<u8>,
    /// Reply-to subject if the publisher set one (NATS request/reply pattern).
    /// Present when the wire MSG line had 5 parts instead of 4. Used by
    /// `kannaka swarm serve` to respond to inbound `KANNAKA.ask.*` messages.
    pub reply_to: Option<String>,
}

impl NatsMessage {
    /// Try to parse the payload as an AgentPhase.
    pub fn as_phase(&self) -> Option<AgentPhase> {
        serde_json::from_slice(&self.payload).ok()
    }

    /// Try to parse as a JSON value (for announce events).
    pub fn as_json(&self) -> Option<serde_json::Value> {
        serde_json::from_slice(&self.payload).ok()
    }
}

/// Outcome of one `NatsSubscription::next_event` poll. Lets serve loops
/// distinguish "nothing arrived within the read timeout — poll again"
/// from "the connection is dead — reconnect or exit" (which `next_message`
/// conflates into `None`, historically causing 100%-CPU spin loops on a
/// closed socket).
#[derive(Debug)]
pub enum SubEvent {
    /// A message arrived.
    Msg(NatsMessage),
    /// The read timeout (see `set_timeout`) elapsed with no traffic.
    /// The connection is still healthy; poll again.
    Timeout,
    /// EOF, a fatal protocol desync, or an authorization error from the
    /// server. This subscription will never yield again — reconnect or exit.
    Closed,
}

impl NatsSubscription {
    /// The subscription id this subscription registered with the server.
    pub fn sid(&self) -> &str {
        &self.sid
    }

    /// Rebuild this subscription on a (typically just-reconnected) transport
    /// (#706). A subscription reads from a clone of the socket it was created
    /// on, so `SwarmTransport::reconnect()` repairs publishes but leaves
    /// existing subscriptions pinned to the dead stream — their next_event()
    /// reports `Closed`. Callers that want to survive a reconnect replace the
    /// subscription:
    ///
    /// ```ignore
    /// transport.reconnect()?;
    /// sub = sub.resubscribe_via(&transport)?;
    /// ```
    ///
    /// Returns an error for multi-subject subscriptions (created via
    /// `subscribe_phases_and_memories`) — recreate those through their own
    /// constructor.
    pub fn resubscribe_via(
        &self,
        transport: &SwarmTransport,
    ) -> Result<NatsSubscription, NatsError> {
        if self.subject.is_empty() {
            return Err(NatsError::Protocol(
                "multi-subject subscription: recreate it via its original constructor".into(),
            ));
        }
        transport.subscribe_with_queue(&self.subject, self.queue_group.as_deref())
    }

    /// Record that a frame of any kind just arrived: the connection is alive,
    /// so the idle clock resets and any outstanding liveness probe is answered.
    fn mark_frame(&mut self) {
        self.last_frame = Instant::now();
        self.probe_sent = false;
    }

    /// Write a raw control frame (PING/PONG) on a clone of the subscription
    /// socket. Best-effort: a failed write on a dying socket just means the
    /// liveness deadline will fire instead.
    fn send_control(&self, bytes: &[u8]) {
        if let Ok(mut s) = self.reader.get_ref().try_clone() {
            let _ = s.write_all(bytes);
            let _ = s.flush();
        }
    }

    /// Poll for the next event, distinguishing timeout from connection
    /// death. Server PINGs are answered transparently; `-ERR` lines are
    /// logged (authorization errors close the subscription).
    ///
    /// Liveness (#500): every frame refreshes the idle clock. When a read
    /// times out we consult `liveness_action` — after `ping_idle` of silence
    /// we send our own PING to elicit a PONG, and after `liveness_timeout` of
    /// silence with no frame at all we report `Closed` so the caller
    /// reconnects/exits instead of hanging deaf on a silently-dead socket.
    /// Round-trip a PING and fail if the broker refused this subscription
    /// (#562).
    ///
    /// PING is ordered behind the SUB, so once PONG comes back any refusal of
    /// the SUB has already been delivered. MSG frames that raced ahead are
    /// stashed in `pending`, never dropped.
    fn confirm_accepted(&mut self, authenticated: bool) -> Result<(), NatsError> {
        self.send_control(b"PING\r\n");
        for _ in 0..16 {
            match read_frame(&mut self.reader) {
                Ok(ReadOutcome::Frame(Frame::Pong)) => {
                    self.mark_frame();
                    return Ok(());
                }
                Ok(ReadOutcome::Frame(Frame::Msg { subject, reply_to, payload, .. })) => {
                    self.mark_frame();
                    self.pending.push_back(NatsMessage { subject, payload, reply_to });
                }
                Ok(ReadOutcome::Frame(Frame::Ping)) => {
                    self.mark_frame();
                    self.send_control(b"PONG\r\n");
                }
                Ok(ReadOutcome::Frame(Frame::ServerErr(m))) => {
                    self.mark_frame();
                    if is_permissions_error(&m) {
                        // Multi-subject subscriptions carry an empty
                        // `self.subject`, so take the name the server itself
                        // reported — otherwise the error names nothing.
                        let subject = if self.subject.is_empty() {
                            subject_from_permissions_error(&m)
                                .unwrap_or_else(|| "<multi-subject>".to_string())
                        } else {
                            self.subject.clone()
                        };
                        return Err(permissions_error(
                            "subscribe",
                            &subject,
                            &m,
                            authenticated,
                        ));
                    }
                    if is_auth_error(&m) {
                        return Err(NatsError::Disconnected(m));
                    }
                    eprintln!("[nats] server error: {m}");
                }
                Ok(ReadOutcome::Frame(_)) => {
                    self.mark_frame();
                }
                // Silence is not evidence of refusal — the SUB is already on
                // the wire, so a slow broker must not fail the subscription.
                Ok(ReadOutcome::TimedOut) => return Ok(()),
                Ok(ReadOutcome::Closed) => {
                    return Err(NatsError::Disconnected(
                        "connection closed awaiting subscribe confirmation".to_string(),
                    ))
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// Subjects this multi-subject subscription asked for and the broker
    /// refused (#562). Empty on a healthy subscription.
    pub fn denied_subjects(&self) -> &[String] {
        &self.denied
    }

    /// Non-fatal variant of [`Self::confirm_accepted`] for multi-subject
    /// subscriptions: records each refusal and warns, but keeps the
    /// subscription so the subjects that WERE accepted keep flowing.
    fn collect_denials(&mut self, authenticated: bool) {
        if let Err(e) = self.confirm_accepted(authenticated) {
            // confirm_accepted stops at the first refusal; capture it, warn
            // loudly, and carry on with whatever the broker did accept.
            let msg = e.to_string();
            eprintln!("[nats] WARNING: {msg}");
            self.denied.push(msg);
        }
    }

    pub fn next_event(&mut self) -> SubEvent {
        // Anything the acceptance round-trip already pulled off the socket is
        // real traffic and outranks a fresh read (#562).
        if let Some(msg) = self.pending.pop_front() {
            return SubEvent::Msg(msg);
        }
        loop {
            match read_frame(&mut self.reader) {
                Ok(ReadOutcome::Frame(Frame::Msg { subject, reply_to, payload, .. })) => {
                    self.mark_frame();
                    return SubEvent::Msg(NatsMessage { subject, payload, reply_to });
                }
                Ok(ReadOutcome::Frame(Frame::Ping)) => {
                    self.mark_frame();
                    self.send_control(b"PONG\r\n");
                }
                Ok(ReadOutcome::Frame(Frame::ServerErr(m))) => {
                    self.mark_frame();
                    eprintln!("[nats] server error: {m}");
                    if is_auth_error(&m) {
                        return SubEvent::Closed;
                    }
                }
                // Pong (answering our probe), +OK, INFO: liveness refreshed,
                // nothing else to do — keep polling.
                Ok(ReadOutcome::Frame(_)) => {
                    self.mark_frame();
                    continue;
                }
                Ok(ReadOutcome::TimedOut) => {
                    match liveness_action(
                        self.last_frame.elapsed(),
                        self.probe_sent,
                        self.ping_idle,
                        self.liveness_timeout,
                    ) {
                        LivenessAction::Poll => return SubEvent::Timeout,
                        LivenessAction::Ping => {
                            self.send_control(b"PING\r\n");
                            self.probe_sent = true;
                            return SubEvent::Timeout;
                        }
                        LivenessAction::Dead => {
                            eprintln!(
                                "[nats] subscription {} silent for {:?} (no frame, no PONG) — treating as closed (#500)",
                                self.sid,
                                self.last_frame.elapsed()
                            );
                            return SubEvent::Closed;
                        }
                    }
                }
                Ok(ReadOutcome::Closed) => return SubEvent::Closed,
                Err(e) => {
                    eprintln!("[nats] subscription protocol error: {e}");
                    return SubEvent::Closed;
                }
            }
        }
    }

    /// Block until the next message arrives. Returns None on read timeout
    /// AND on connection close — callers that need to tell those apart
    /// (e.g. serve loops that must not spin on a dead socket) should use
    /// `next_event` instead.
    pub fn next_message(&mut self) -> Option<NatsMessage> {
        match self.next_event() {
            SubEvent::Msg(m) => Some(m),
            SubEvent::Timeout | SubEvent::Closed => None,
        }
    }

    /// Set read timeout for the subscription stream.
    ///
    /// NOTE: the timeout is a property of the underlying socket, which is
    /// shared with the SwarmTransport that created this subscription — fine
    /// under the one-subscription-per-connection model.
    ///
    /// The requested timeout is clamped to at most `SUB_POLL_MAX`, and `None`
    /// (previously an infinite block) becomes `SUB_POLL_MAX`. This guarantees
    /// the read wakes periodically so the liveness check in `next_event` can
    /// run even when a caller asked to block indefinitely (#500). Callers all
    /// loop on `SubEvent::Timeout`, so the extra wakeups are transparent.
    pub fn set_timeout(&self, timeout: Option<Duration>) -> Result<(), NatsError> {
        let effective = match timeout {
            Some(t) => t.min(SUB_POLL_MAX),
            None => SUB_POLL_MAX,
        };
        self.reader.get_ref().set_read_timeout(Some(effective))?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// #701: the canonical envelope must stamp `schema_version` as the
    /// string "1.0" and `ts` as a unix-ms NUMBER — value AND type. An
    /// RFC3339 `timestamp` on the payload is promoted to numeric `ts`,
    /// not mirrored as a string.
    #[test]
    fn add_envelope_emits_contract_types() {
        let mut fresh = serde_json::json!({ "agent_id": "a" });
        add_envelope(&mut fresh);
        assert_eq!(fresh["schema_version"], serde_json::json!("1.0"));
        let ts = fresh["ts"].as_i64().expect("ts must be a NUMBER (unix-ms)");
        assert!(ts > 1_600_000_000_000, "ts must be unix-ms, got {ts}");

        let mut with_timestamp =
            serde_json::json!({ "timestamp": "2026-08-02T10:00:00Z" });
        add_envelope(&mut with_timestamp);
        assert_eq!(
            with_timestamp["ts"].as_i64(),
            Some(1_785_664_800_000),
            "an RFC3339 timestamp field must promote to numeric unix-ms ts"
        );

        // Idempotent: existing envelope fields are never clobbered.
        let mut pinned = serde_json::json!({ "schema_version": "1.0", "ts": 42 });
        add_envelope(&mut pinned);
        assert_eq!(pinned["ts"].as_i64(), Some(42));
    }

    /// #703: an exemplar payload, run through the same `add_envelope` that
    /// `publish_exemplar` applies, must carry every key the amended contract
    /// (consciousness-core docs/nats-contract.yaml) requires — with the
    /// contract's TYPES, not just the names. `centroid` is deliberately
    /// absent: the contract moved it to optional, because HRM encodings are
    /// per-agent and an absorbing node re-encodes the exemplar's CONTENT
    /// into its own wave space — a foreign centroid was never usable.
    #[test]
    fn exemplar_payload_meets_the_amended_contract() {
        // Mirror the swarm exemplar builder's shape (handlers/swarm.rs).
        let mut payload = serde_json::json!({
            "agent_id": "test-agent",
            "cluster_id": 7,
            "size": 3,
            "content": "distilled cluster exemplar",
            "amplitude": 0.42,
            "created_at": chrono::Utc::now().to_rfc3339(),
        });
        add_envelope(&mut payload);

        // The contract's four required keys, value AND type.
        assert_eq!(payload["schema_version"], serde_json::json!("1.0"));
        assert!(
            payload["ts"].as_i64().is_some_and(|t| t > 1_600_000_000_000),
            "ts must be numeric unix-ms"
        );
        assert!(payload["agent_id"].is_string());
        assert!(
            payload["cluster_id"].as_u64().is_some(),
            "cluster_id must be an integer"
        );
    }

    /// #705 residual: dream lifecycle payloads must carry the report fields
    /// at the TOP level (the contract requires memories_strengthened /
    /// memories_faded on queen.event.dream.end) — not nested in `details`.
    #[test]
    fn dream_lifecycle_flattens_report_details() {
        // Mirror publish_dream_lifecycle's payload construction.
        let details =
            serde_json::json!({ "memories_strengthened": 7, "memories_faded": 2 });
        let mut payload = serde_json::json!({
            "agent_id": "test-agent",
            "timestamp": chrono::Utc::now().to_rfc3339(),
        });
        if let Some(obj) = payload.as_object_mut() {
            for (k, v) in details.as_object().unwrap() {
                obj.entry(k.clone()).or_insert_with(|| v.clone());
            }
        }
        assert_eq!(payload["memories_strengthened"].as_i64(), Some(7));
        assert_eq!(payload["memories_faded"].as_i64(), Some(2));
        assert!(payload.get("details").is_none());
    }

    /// #590: presence retraction must hide the departed agent WITHOUT hiding
    /// anyone else — in particular every record written before this change,
    /// which carries no `status` field at all.
    #[test]
    fn presence_filter_drops_only_explicit_left_records() {
        let legacy = serde_json::json!({ "agent_id": "old-node" });
        let active = serde_json::json!({ "agent_id": "live-node", "status": "active" });
        let left = serde_json::json!({ "agent_id": "gone-node", "status": "left" });
        let odd = serde_json::json!({ "agent_id": "weird-node", "status": 7 });

        // The whole existing swarm predates the `status` field. If this ever
        // flips, shipping the filter blanks `swarm peers` for everyone.
        assert!(
            is_active_presence(&legacy),
            "a pre-#590 record with no status field must read as LIVE"
        );
        assert!(is_active_presence(&active));
        assert!(
            !is_active_presence(&left),
            "an explicit tombstone must be filtered out"
        );
        assert!(
            is_active_presence(&odd),
            "a non-string status is not a retraction — fail open, never hide a live peer"
        );
    }

    /// The tombstone this writes must be the thing the filter catches. Pins the
    /// two halves together so a later rename of the sentinel can't silently
    /// resurrect ghosts.
    #[test]
    fn presence_left_payload_is_filtered_by_the_reader() {
        let mut payload = serde_json::json!({
            "agent_id": "gone-node",
            "status": "left",
            "last_seen": "2026-07-27T00:00:00Z",
        });
        add_envelope(&mut payload);
        assert!(
            !is_active_presence(&payload),
            "publish_presence_left's payload shape must be filtered by get_presence"
        );
        // Envelope still applied, so the tombstone is contract-valid on the wire.
        assert_eq!(payload.get("schema_version").and_then(|v| v.as_str()), Some("1.0"));
    }

    /// #562: a permissions violation must NOT read as an auth failure. The two
    /// have opposite consequences — auth kills the connection, permissions
    /// leaves it fully usable for every other subject — so conflating them
    /// would tear down a working swarm connection over one denied subject.
    #[test]
    fn permissions_violation_is_not_an_auth_error() {
        let perms = "Permissions Violation for Subscription to \"KANNAKA.ask.broadcast\"";
        assert!(is_permissions_error(perms));
        assert!(
            !is_auth_error(perms),
            "a per-subject ACL refusal must not be treated as a dead connection"
        );

        let authz = "Authorization Violation";
        assert!(is_auth_error(authz));
        assert!(
            !is_permissions_error(authz),
            "a dead-connection auth failure must not be downgraded to a per-subject refusal"
        );
    }

    /// An anonymous connection must NOT ask the broker to create a stream.
    ///
    /// It is denied by construction (ADR-0042 closed anon's control lane), so
    /// the request cannot succeed — it only emits a "Permissions Violation for
    /// Publish to $JS.API.STREAM.CREATE.QUEEN_PHASES" on the server, once per
    /// connection. Across the live swarm that was ~113 denials a minute with
    /// 79 in 83 anon (2026-08-23), which is what buried /var/log/messages.
    /// Anon learns what it actually needs from `stream_readable` instead.
    ///
    /// This pins the POLICY, so re-introducing an unconditional
    /// "always ensure the stream" fails here rather than quietly on a broker.
    #[test]
    fn anonymous_connections_do_not_attempt_stream_create() {
        assert!(
            !should_attempt_stream_create(false),
            "an anonymous connection must not issue a create it cannot possibly be granted"
        );
        assert!(
            should_attempt_stream_create(true),
            "an authenticated identity must still create/update the stream — that is what \
             provisions a fresh cluster, syncs stream config, and proves jetstream_writable"
        );
    }

    /// #928: the presence notice is a warning ONLY when the stream is
    /// genuinely absent. A refused create on an existing stream is the
    /// normal state of every anonymous node on a running swarm, and those
    /// nodes DO appear in `swarm peers` (as unverified).
    #[test]
    fn presence_notice_warns_only_when_the_stream_is_absent() {
        let absent = presence_stream_notice("no JetStream reply for stream create", false, false);
        assert!(absent.contains("WARNING"), "{absent}");
        assert!(absent.contains("will NOT appear"), "{absent}");
        assert!(absent.contains("no JetStream reply"), "must carry the create error: {absent}");

        let anon = presence_stream_notice("permissions", true, false);
        assert!(!anon.contains("WARNING"), "{anon}");
        assert!(!anon.contains("NOT appear"), "{anon}");
        assert!(anon.contains("presence stream present (read-only identity)"), "{anon}");
        assert!(anon.contains("listed by other hosts"), "{anon}");
        assert!(anon.contains("(unverified)"), "anonymous membership is named: {anon}");

        let creds = presence_stream_notice("permissions", true, true);
        assert!(creds.contains("listed by other hosts"), "{creds}");
        assert!(!creds.contains("unverified"), "a credentialed read-only identity is not anonymous: {creds}");
    }

    /// #933: the presence create is gated on "the broker is known to refuse
    /// it", not on "we have no credentials". The production swarm keeps its
    /// silence (anon + stream already there = no attempt), and an open broker
    /// can still be bootstrapped by an unauthenticated client.
    #[test]
    fn presence_create_is_attempted_until_the_broker_refuses_it() {
        // Production: anonymous, KANNAKA_PRESENCE already exists. No attempt,
        // so no Permissions Violation on the broker — what #928 bought.
        assert!(
            !should_attempt_presence_create(false, false, false),
            "an anon node on a running swarm must not re-issue the doomed create"
        );
        // Open broker / fresh swarm: anonymous and the stream is absent.
        assert!(
            should_attempt_presence_create(false, false, true),
            "an absent stream on an open bus must still be bootstrapped"
        );
        // Once refused on this connection, never again — whoever we are.
        assert!(!should_attempt_presence_create(false, true, true));
        assert!(!should_attempt_presence_create(true, true, false));
        // Authenticated identities are unchanged: always attempt.
        assert!(should_attempt_presence_create(true, false, false));
        assert!(should_attempt_presence_create(true, false, true));
    }

    /// Only a refusal that names the create subject counts. A denial of some
    /// other subject leaves the create path alone, and a non-permissions
    /// error is not a refusal at all.
    #[test]
    fn only_a_stream_create_denial_closes_the_create_path() {
        assert!(names_denied_stream_create(
            "Permissions Violation for Publish to \"$JS.API.STREAM.CREATE.KANNAKA_PRESENCE\""
        ));
        assert!(!names_denied_stream_create(
            "Permissions Violation for Subscription to \"KANNAKA.ask.*\""
        ));
        assert!(!names_denied_stream_create("Authorization Violation"));
        assert!(!names_denied_stream_create("Unknown Protocol Operation"));
    }

    /// #928: how a JetStream reply is read as evidence about the stream. The
    /// MSG.GET probe an anonymous user is granted answers 10037 "no message
    /// found" on an EMPTY existing stream — that is still an existing stream.
    #[test]
    fn js_reply_distinguishes_missing_stream_from_other_errors() {
        let clean = serde_json::json!({"type": "io.nats.jetstream.api.v1.stream_info_response", "config": {"name": "KANNAKA_PRESENCE"}});
        assert!(js_reply_names_an_existing_stream(&clean));
        let empty_stream = serde_json::json!({"error": {"code": 404, "err_code": 10037, "description": "no message found"}});
        assert!(js_reply_names_an_existing_stream(&empty_stream), "an empty stream still exists");
        let not_found = serde_json::json!({"error": {"code": 404, "err_code": 10059, "description": "stream not found"}});
        assert!(!js_reply_names_an_existing_stream(&not_found));
        let no_code = serde_json::json!({"error": {"code": 500, "description": "server error"}});
        assert!(js_reply_names_an_existing_stream(&no_code), "an unrelated error is not evidence of absence");
    }

    /// Both real server phrasings, publish and subscribe, must be recognised —
    /// the live broker emits both and missing one restores the silent no-op.
    #[test]
    fn permissions_predicate_matches_both_server_phrasings() {
        for m in [
            "Permissions Violation for Subscription to \"KANNAKA.memory.new\"",
            "Permissions Violation for Publish to \"$JS.API.STREAM.CREATE.QUEEN_PHASES\"",
            "permissions violation for publish to \"x\"", // case-insensitive
        ] {
            assert!(is_permissions_error(m), "must match: {m}");
        }
        assert!(!is_permissions_error("Unknown Protocol Operation"));
    }

    /// The error has to carry the two things an operator needs: WHICH subject,
    /// and whether adding credentials could plausibly fix it.
    #[test]
    fn permissions_error_names_subject_and_points_anonymous_users_at_creds() {
        let raw = "Permissions Violation for Subscription to \"KANNAKA.ask.broadcast\"";

        let anon = permissions_error("subscribe", "KANNAKA.ask.broadcast", raw, false).to_string();
        assert!(anon.contains("KANNAKA.ask.broadcast"), "must name the subject: {anon}");
        assert!(anon.contains("ANONYMOUS"), "must say the connection is anonymous: {anon}");
        assert!(anon.contains(".kannaka-nats.env"), "must point at the creds file: {anon}");

        // An AUTHENTICATED caller must not be told to go find credentials it
        // already supplied — that sends them down the wrong path entirely.
        let authed = permissions_error("subscribe", "KANNAKA.ask.broadcast", raw, true).to_string();
        assert!(authed.contains("KANNAKA.ask.broadcast"));
        assert!(
            !authed.contains(".kannaka-nats.env"),
            "an authenticated caller must NOT be sent to the credentials file: {authed}"
        );
        assert!(authed.contains("IS authenticated"), "{authed}");
    }

    /// Build a presence record whose `last_seen` is `age_secs` old.
    fn presence_aged(agent: &str, now: chrono::DateTime<Utc>, age_secs: i64) -> serde_json::Value {
        serde_json::json!({
            "agent_id": agent,
            "last_seen": (now - chrono::Duration::seconds(age_secs)).to_rfc3339(),
        })
    }

    /// #737: a crashed agent writes no `status: "left"` tombstone, so the
    /// tombstone filter alone cannot retire it — only `last_seen` can. This is
    /// the whole liveness claim `swarm peers` and `brief --peers` rest on.
    #[test]
    fn stale_presence_is_not_live_even_without_a_tombstone() {
        let now = Utc::now();
        let fresh = presence_aged("beating", now, 30); // one heartbeat ago
        let crashed = presence_aged("crashed", now, PRESENCE_LIVE_WINDOW_SECS + 60);

        assert!(
            is_active_presence(&crashed),
            "precondition: a crash leaves NO tombstone, so the #590 filter still calls it active"
        );
        assert!(is_fresh_presence(&fresh, now));
        assert!(
            !is_fresh_presence(&crashed, now),
            "a record older than the live window must stop counting as a reachable peer"
        );
    }

    /// Fails OPEN, matching `is_active_presence`: hiding every agent too old to
    /// stamp `last_seen` would be a worse failure than showing one ghost.
    #[test]
    fn presence_without_parsable_last_seen_reads_as_live() {
        let now = Utc::now();
        let no_field = serde_json::json!({ "agent_id": "legacy" });
        let unparsable = serde_json::json!({ "agent_id": "odd", "last_seen": "not-a-timestamp" });
        let wrong_type = serde_json::json!({ "agent_id": "odd2", "last_seen": 12345 });

        assert_eq!(presence_age_secs(&no_field, now), None);
        for p in [&no_field, &unparsable, &wrong_type] {
            assert!(
                is_fresh_presence(p, now),
                "missing/unparsable last_seen must fail OPEN, never hide a live peer: {p}"
            );
        }
    }

    /// A peer whose clock runs ahead of ours yields a negative age. That is
    /// skew, not staleness — it must not read as "in the future, therefore gone".
    #[test]
    fn presence_from_a_skewed_future_clock_is_live() {
        let now = Utc::now();
        let ahead = presence_aged("skewed", now, -600);
        assert!(presence_age_secs(&ahead, now).is_some_and(|a| a < 0));
        assert!(is_fresh_presence(&ahead, now));
    }

    /// The window is a wide multiple of the 30s default heartbeat, so an agent
    /// that merely missed a beat or two is never wrongly retired.
    #[test]
    fn live_window_leaves_room_for_missed_heartbeats() {
        assert!(
            PRESENCE_LIVE_WINDOW_SECS >= 30 * 5,
            "window must tolerate several missed 30s heartbeats"
        );
    }

    // hunt: the handshake INFO read (and any control line) must be BOUNDED — a
    // hostile/MITM server streaming an unbounded line would otherwise OOM us.
    #[test]
    fn read_control_line_rejects_overlong_line() {
        use std::io::Cursor;
        // A line longer than the cap with no newline is a protocol desync, not
        // an unbounded read (a bare read_line would have swallowed the whole thing).
        let overlong = vec![b'x'; MAX_CONTROL_LINE as usize + 100];
        let mut cur = Cursor::new(overlong);
        let mut out = String::new();
        assert!(
            matches!(read_control_line(&mut cur, &mut out), Err(NatsError::Protocol(_))),
            "an over-long control line must be rejected as a protocol desync"
        );
        // A normal line is accepted and returned intact.
        let mut ok = Cursor::new(b"INFO {\"server_id\":\"x\"}\r\n".to_vec());
        let mut line = String::new();
        let n = read_control_line(&mut ok, &mut line).unwrap();
        assert!(n > 0 && line.starts_with("INFO "), "a normal INFO line reads fine");
    }

    // ── credential precedence (#643) ────────────────────────────
    //
    // The hive bridge authenticates as HIVE_NATS_USER, its own principal. It
    // now reaches NATS through SwarmTransport instead of a bespoke client, and
    // the whole point of `connect_with_creds` is that adopting the shared
    // transport must NOT quietly re-identify it: on a box where the generic
    // NATS_USER is also exported (any host running other kannaka units), env
    // creds would otherwise win and the bridge would connect with the wrong
    // ACLs — publishing under the swarm identity, or being denied outright.

    fn creds(u: &str, p: &str) -> (String, String) {
        (u.to_string(), p.to_string())
    }

    #[test]
    fn explicit_creds_outrank_env() {
        let got = resolve_creds(
            Some(&creds("hive", "hive-secret")),
            "swarm".to_string(),
            "swarm-secret".to_string(),
            Some(creds("urluser", "urlpass")),
        );
        assert_eq!(
            got,
            Some(creds("hive", "hive-secret")),
            "a caller that passed its own identity must keep it even when NATS_USER is set"
        );
    }

    #[test]
    fn env_outranks_url_when_no_explicit_creds() {
        // Unchanged pre-existing behaviour — the refactor must not disturb it.
        let got = resolve_creds(
            None,
            "swarm".to_string(),
            "swarm-secret".to_string(),
            Some(creds("urluser", "urlpass")),
        );
        assert_eq!(got, Some(creds("swarm", "swarm-secret")));
    }

    #[test]
    fn url_creds_used_when_env_is_absent_or_partial() {
        let from_url = Some(creds("urluser", "urlpass"));
        assert_eq!(
            resolve_creds(None, String::new(), String::new(), from_url.clone()),
            from_url,
        );
        // A half-set env pair is not a usable identity and must not shadow the
        // URL — this is why the check is on BOTH fields.
        assert_eq!(
            resolve_creds(None, "swarm".to_string(), String::new(), from_url.clone()),
            from_url,
        );
    }

    #[test]
    fn anonymous_when_nothing_is_supplied() {
        assert_eq!(resolve_creds(None, String::new(), String::new(), None), None);
    }

    // ── the bridge must not re-grow a bespoke NATS client (#643) ──
    //
    // The hand-rolled copy drifted from this module in four ways at once: no
    // connect timeout (the liveness bug — a per-message connect to an
    // unreachable host stalled the relay loop for the OS SYN timeout on EVERY
    // event), `"`-only credential escaping, a fixed 2048-byte single-read INFO,
    // and no tls_required detection. Deleting it is only half a fix if the next
    // "just publish one message" patch reintroduces it, so this asserts the
    // bridge speaks NATS exclusively through SwarmTransport.
    /// Both relay bridges, not just the hive one. The DM bridge carried the
    /// SAME hand-rolled client — the hive bridge's comment literally said
    /// "matching the DM bridge's approach" — so a guard on one of them leaves
    /// the copy that was actually copied FROM unprotected. It was in fact worse:
    /// it never sent PING or read PONG, so an `-ERR Authorization Violation`
    /// was never seen and every DM "succeeded" while the server dropped it.
    #[test]
    fn dm_bridge_publishes_through_the_shared_transport() {
        let src = include_str!("bin/kannaka_nostr_bridge.rs");
        let code: String = src
            .lines()
            .filter(|l| !l.trim_start().starts_with("//") && !l.trim_start().starts_with("///"))
            .collect::<Vec<_>>()
            .join("
");

        assert!(
            code.contains("SwarmTransport::connect_with_creds"),
            "the DM bridge should reach NATS via the shared, hardened transport"
        );
        assert!(
            !code.contains("TcpStream::connect"),
            "the DM bridge must not dial its own NATS socket again"
        );
        assert!(
            !code.contains("CONNECT {"),
            "the DM bridge must not hand-build CONNECT JSON (escaping lives in handshake)"
        );
        assert!(
            !code.contains("PUB "),
            "the DM bridge must not hand-frame PUB lines"
        );
    }

    #[test]
    fn hive_bridge_publishes_through_the_shared_transport() {
        let src = include_str!("bin/kannaka_hive_bridge.rs");
        // Comments in this file legitimately DESCRIBE the old client; strip
        // them so the prose does not trip its own guard.
        let code: String = src
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            code.contains("SwarmTransport::connect_with_creds"),
            "the bridge should reach NATS via the shared, hardened transport"
        );
        // Narrowly: no *dialling* its own socket. The bridge legitimately
        // touches the websocket's `TcpStream` to bound reads, so a bare
        // "TcpStream" ban would be noise rather than a guard.
        assert!(
            !code.contains("TcpStream::connect"),
            "the bridge must not dial its own NATS socket again"
        );
        assert!(
            !code.contains("CONNECT {"),
            "the bridge must not hand-build CONNECT JSON (escaping lives in handshake)"
        );
        assert!(
            !code.contains("PUB "),
            "the bridge must not hand-frame PUB lines"
        );
    }

    #[test]
    fn parse_nats_url_default_port() {
        let (host, port, creds) = parse_nats_url("nats://localhost").unwrap();
        assert_eq!(host, "localhost");
        assert_eq!(port, 4222);
        assert!(creds.is_none());
    }

    #[test]
    fn parse_nats_url_custom_port() {
        let (host, port, creds) = parse_nats_url("nats://swarm.ninja-portal.com:4222").unwrap();
        assert_eq!(host, "swarm.ninja-portal.com");
        assert_eq!(port, 4222);
        assert!(creds.is_none());
    }

    #[test]
    fn parse_nats_url_no_scheme() {
        let (host, port, creds) = parse_nats_url("localhost:4222").unwrap();
        assert_eq!(host, "localhost");
        assert_eq!(port, 4222);
        assert!(creds.is_none());
    }

    #[test]
    fn parse_nats_url_with_credentials() {
        let (host, port, creds) = parse_nats_url("nats://alice:s3cr3t@example.com:4223").unwrap();
        assert_eq!(host, "example.com");
        assert_eq!(port, 4223);
        assert_eq!(creds, Some(("alice".to_string(), "s3cr3t".to_string())));
    }

    #[test]
    fn parse_nats_url_creds_default_port() {
        let (host, port, creds) = parse_nats_url("nats://bob:pw@example.com").unwrap();
        assert_eq!(host, "example.com");
        assert_eq!(port, 4222);
        assert_eq!(creds, Some(("bob".to_string(), "pw".to_string())));
    }

    #[test]
    fn new_inbox_unique() {
        let a = new_inbox("t");
        let b = new_inbox("t");
        assert_ne!(a, b);
        assert!(a.starts_with("_INBOX.t."));
    }

    #[test]
    fn connect_default_graceful_failure() {
        match SwarmTransport::connect("nats://127.0.0.1:19999") {
            Ok(_) => panic!("should not connect to nonexistent server"),
            Err(e) => {
                let msg = format!("{e}");
                assert!(
                    msg.contains("connect") || msg.contains("Connect"),
                    "error should mention connect: {msg}"
                );
            }
        }
    }

    #[test]
    fn phase_serialization_roundtrip() {
        use chrono::Utc;
        let phase = AgentPhase {
            id: "test-id".to_string(),
            agent_id: "agent-1".to_string(),
            phase: 1.5,
            frequency: 0.5,
            coherence: 0.8,
            phi: 3.2,
            order_parameter: 0.9,
            cluster_count: 3,
            memory_count: 42,
            link_count: 0,
            xi_signature: None,
            protocol_version: "1.0".to_string(),
            timestamp: Utc::now(),
            trust_score: 0.5,
            handedness: crate::queen::Handedness::Achiral,
            left_coherence: 0.0,
            right_coherence: 0.0,
            bridge_activity: 0.0,
            dream_state: None,
            role: None,
            display_name: None,
        };
        let bytes = serde_json::to_vec(&phase).unwrap();
        let decoded: AgentPhase = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(decoded.agent_id, "agent-1");
        assert!((decoded.phase - 1.5).abs() < 0.001);
    }

    #[test]
    fn base64_decode_roundtrip() {
        let decoded = base64_decode("aGVsbG8=").unwrap();
        assert_eq!(&decoded, b"hello");
    }

    #[test]
    fn base64_decode_empty() {
        let decoded = base64_decode("").unwrap();
        assert!(decoded.is_empty());
    }

    #[test]
    fn base64_decode_rejects_invalid() {
        assert!(base64_decode("ab!c").is_err());
    }

    #[test]
    fn nats_error_display_variants() {
        let e = NatsError::Disconnected("lost connection".into());
        assert!(format!("{e}").contains("disconnected"));

        let e = NatsError::KvNotFound("mybucket/mykey".into());
        assert!(format!("{e}").contains("KV key not found"));
    }

    #[test]
    fn buffered_message_clone() {
        let msg = BufferedMessage {
            subject: "QUEEN.event.join".to_string(),
            payload: b"test".to_vec(),
            attempts: 0,
        };
        let cloned = msg.clone();
        assert_eq!(cloned.subject, msg.subject);
        assert_eq!(cloned.payload, msg.payload);
    }

    #[test]
    fn publish_buffer_limit() {
        let buf: Arc<Mutex<VecDeque<BufferedMessage>>> =
            Arc::new(Mutex::new(VecDeque::new()));
        {
            let mut guard = buf.lock().unwrap();
            for i in 0..PUBLISH_BUFFER_LIMIT + 20 {
                if guard.len() >= PUBLISH_BUFFER_LIMIT {
                    guard.pop_front();
                }
                guard.push_back(BufferedMessage {
                    subject: format!("test.{i}"),
                    payload: vec![],
                    attempts: 0,
                });
            }
            assert_eq!(guard.len(), PUBLISH_BUFFER_LIMIT);
            assert_eq!(guard.front().unwrap().subject, "test.20");
        }
    }

    // -----------------------------------------------------------------------
    // Announce identity (swarm agent identity, step 2)
    // -----------------------------------------------------------------------

    fn sample_identity() -> AnnounceIdentity {
        AnnounceIdentity {
            user_id: "user-123".into(),
            email: "agent@spacechild.love".into(),
        }
    }

    #[test]
    fn announce_payload_without_identity_is_byte_compatible() {
        // No stored identity → exactly the payload v0.6.19/0.6.20 publish.
        let new = legacy_announce_payload("join", "agent-a", None);
        let old = serde_json::json!({ "event": "join", "agent_id": "agent-a" });
        assert_eq!(
            serde_json::to_string(&new).unwrap(),
            serde_json::to_string(&old).unwrap(),
        );
    }

    #[test]
    fn announce_payload_with_identity_adds_optional_block() {
        let payload = legacy_announce_payload("join", "agent-a", Some(&sample_identity()));
        // Existing fields untouched (old agents read these by name).
        assert_eq!(payload["event"], "join");
        assert_eq!(payload["agent_id"], "agent-a");
        // Identity block carries user_id/email ONLY — never tokens.
        assert_eq!(payload["identity"]["user_id"], "user-123");
        assert_eq!(payload["identity"]["email"], "agent@spacechild.love");
        let block = payload["identity"].as_object().unwrap();
        assert_eq!(block.len(), 2, "identity block must be user_id + email only");
        let text = serde_json::to_string(&payload).unwrap();
        assert!(!text.contains("token"), "no token material on the wire");
    }

    #[test]
    fn old_shape_announce_still_deserializes() {
        // What a v0.6.19/0.6.20 agent publishes today (post-envelope).
        let old = r#"{"event":"join","agent_id":"agent-old","schema_version":"1.0","ts":1765400000000}"#;
        let v: serde_json::Value = serde_json::from_str(old).expect("old payload parses");
        assert_eq!(v["agent_id"], "agent-old");
        // New code sees no identity on old payloads — no error, just None.
        assert_eq!(AnnounceIdentity::email_from(&v), None);
    }

    #[test]
    fn new_shape_announce_keeps_legacy_fields_intact() {
        // Old agents parse announces as loose JSON by field name — verify
        // the fields they use survive the identity enrichment + envelope.
        let mut payload = legacy_announce_payload("join", "agent-new", Some(&sample_identity()));
        add_envelope(&mut payload);
        let bytes = serde_json::to_vec(&payload).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["event"], "join");
        assert_eq!(v["agent_id"], "agent-new");
        assert_eq!(v["schema_version"], "1.0");
        assert!(v["ts"].is_number());
        assert_eq!(AnnounceIdentity::email_from(&v), Some("agent@spacechild.love"));
    }

    #[test]
    fn presence_identity_email_extraction() {
        // Presence record from a pre-identity agent: no identity key.
        let old = serde_json::json!({ "agent_id": "a", "memory_count": 5 });
        assert_eq!(AnnounceIdentity::email_from(&old), None);

        // Presence record with an attached identity.
        let mut new = serde_json::json!({ "agent_id": "a", "memory_count": 5 });
        sample_identity().attach_to(&mut new);
        assert_eq!(new["agent_id"], "a", "existing fields untouched");
        assert_eq!(AnnounceIdentity::email_from(&new), Some("agent@spacechild.love"));

        // attach_to on a non-object is a no-op, not a panic.
        let mut not_obj = serde_json::json!("scalar");
        sample_identity().attach_to(&mut not_obj);
        assert_eq!(AnnounceIdentity::email_from(&not_obj), None);
    }

    // -----------------------------------------------------------------------
    // Integration tests -- skip if NATS unavailable
    // -----------------------------------------------------------------------

    fn connect_or_skip() -> Option<SwarmTransport> {
        match SwarmTransport::connect(DEFAULT_NATS_URL) {
            Ok(t) => Some(t),
            Err(_) => {
                eprintln!("NATS not available, skipping integration test");
                None
            }
        }
    }

    #[test]
    fn integration_publish_and_read() {
        let transport = match connect_or_skip() {
            Some(t) => t,
            None => return,
        };

        let phase = AgentPhase {
            id: "int-test".to_string(),
            agent_id: "test-integration".to_string(),
            phase: 2.0,
            frequency: 0.5,
            coherence: 0.7,
            phi: 1.0,
            order_parameter: 0.0,
            cluster_count: 0,
            memory_count: 0,
            link_count: 0,
            xi_signature: None,
            protocol_version: "1.0".to_string(),
            timestamp: chrono::Utc::now(),
            trust_score: 0.5,
            handedness: crate::queen::Handedness::Achiral,
            left_coherence: 0.0,
            right_coherence: 0.0,
            bridge_activity: 0.0,
            dream_state: None,
            role: None,
            display_name: None,
        };

        transport.publish_phase(&phase).expect("publish should work");
        transport.announce_join("test-integration").expect("announce should work");

        let phases = transport.get_all_phases().unwrap_or_default();
        eprintln!("Got {} phases from NATS", phases.len());
    }

    #[test]
    #[ignore = "requires live NATS server with JetStream; run locally with `cargo test --ignored`"]
    fn integration_kv_put_get() {
        let transport = match connect_or_skip() {
            Some(t) => t,
            None => return,
        };
        if !transport.has_jetstream_write() {
            eprintln!("JetStream not available, skipping KV test");
            return;
        }

        transport.create_kv_bucket("TEST_KV", 60).expect("create bucket");
        transport.kv_put("TEST_KV", "hello", "world").expect("kv_put");
        // Small delay to let JetStream persist
        std::thread::sleep(Duration::from_millis(200));
        let val = transport.kv_get("TEST_KV", "hello").expect("kv_get");
        assert_eq!(val, "world");

        // Overwrite: may not take effect if the bucket was previously created
        // with discard="new" (stale server state). On a fresh bucket, updates work.
        transport.kv_put("TEST_KV", "hello", "updated").expect("kv_put overwrite");
        std::thread::sleep(Duration::from_millis(200));
        let val2 = transport.kv_get("TEST_KV", "hello").expect("kv_get after overwrite");
        // Accept either value (old bucket: "world", fresh bucket: "updated")
        assert!(
            val2 == "updated" || val2 == "world",
            "expected 'updated' or 'world', got: {val2}"
        );
    }

    #[test]
    #[ignore = "requires live NATS server with JetStream; run locally with `cargo test --ignored`"]
    fn integration_kv_keys() {
        let transport = match connect_or_skip() {
            Some(t) => t,
            None => return,
        };
        if !transport.has_jetstream_write() {
            eprintln!("JetStream not available, skipping KV keys test");
            return;
        }

        transport.create_kv_bucket("TEST_KEYS", 60).expect("create bucket");
        transport.kv_put("TEST_KEYS", "a", "1").expect("put a");
        transport.kv_put("TEST_KEYS", "b", "2").expect("put b");
        std::thread::sleep(Duration::from_millis(200));

        let keys = transport.kv_keys("TEST_KEYS").expect("kv_keys");
        assert!(keys.contains(&"a".to_string()), "keys should contain 'a': {keys:?}");
        assert!(keys.contains(&"b".to_string()), "keys should contain 'b': {keys:?}");
    }

    #[test]
    fn integration_kv_get_missing() {
        let transport = match connect_or_skip() {
            Some(t) => t,
            None => return,
        };
        if !transport.has_jetstream_write() {
            eprintln!("JetStream not available, skipping KV missing test");
            return;
        }

        transport.create_kv_bucket("TEST_MISSING", 60).expect("create bucket");
        match transport.kv_get("TEST_MISSING", "nonexistent") {
            Err(NatsError::KvNotFound(_)) => {}
            other => panic!("expected KvNotFound, got: {other:?}"),
        }
    }

    #[test]
    fn integration_announce_event() {
        let transport = match connect_or_skip() {
            Some(t) => t,
            None => return,
        };

        let payload = serde_json::json!({ "agent_id": "test-events" });
        transport.announce_event("join", &payload).expect("announce join event");
        transport.announce_event("dream.start", &payload).expect("announce dream.start");
        transport.announce_event("memory.shared", &payload).expect("announce memory.shared");
    }

    #[test]
    fn integration_reconnect_live() {
        let mut transport = match connect_or_skip() {
            Some(t) => t,
            None => return,
        };

        assert!(transport.is_connected());
        transport.reconnect().expect("reconnect to live server");
        assert!(transport.is_connected());

        let payload = serde_json::json!({ "agent_id": "reconnect-test" });
        transport.announce_event("join", &payload).expect("announce after reconnect");
    }

    // -----------------------------------------------------------------------
    // Liveness + resumable frame reads (#499, #500)
    //
    // These are hermetic: `read_exact_resumable` and `liveness_action` are
    // pure and driven by scripted mocks; the frame/subscription tests use a
    // loopback TCP pair (no NATS server) so we can inject stalls, silence, and
    // EOF deterministically.
    // -----------------------------------------------------------------------

    /// A `Read` that replays a scripted sequence of chunks, timeouts, and EOF.
    struct ScriptReader {
        steps: VecDeque<ScriptStep>,
    }
    enum ScriptStep {
        Data(Vec<u8>),
        /// Yield a `WouldBlock` (a read timeout).
        Timeout,
        /// Yield `Ok(0)` (peer closed).
        Eof,
    }
    impl Read for ScriptReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            match self.steps.pop_front() {
                Some(ScriptStep::Data(d)) => {
                    let n = d.len().min(buf.len());
                    buf[..n].copy_from_slice(&d[..n]);
                    if n < d.len() {
                        self.steps.push_front(ScriptStep::Data(d[n..].to_vec()));
                    }
                    Ok(n)
                }
                Some(ScriptStep::Timeout) => {
                    Err(std::io::Error::new(std::io::ErrorKind::WouldBlock, "would block"))
                }
                Some(ScriptStep::Eof) | None => Ok(0),
            }
        }
    }

    #[test]
    fn resumable_read_survives_interspersed_timeouts() {
        // #499: a payload split across segments, with a read timeout landing
        // INSIDE the frame, still fills completely (pre-fix this was fatal).
        let mut r = ScriptReader {
            steps: VecDeque::from(vec![
                ScriptStep::Data(b"he".to_vec()),
                ScriptStep::Timeout,
                ScriptStep::Timeout,
                ScriptStep::Data(b"ll".to_vec()),
                ScriptStep::Data(b"o".to_vec()),
            ]),
        };
        let mut buf = [0u8; 5];
        let deadline = Instant::now() + Duration::from_secs(5);
        read_exact_resumable(&mut r, &mut buf, deadline).expect("should fill despite timeouts");
        assert_eq!(&buf, b"hello");
    }

    #[test]
    fn resumable_read_gives_up_after_deadline() {
        // Only timeouts, deadline already elapsed → declared dead, not looped.
        let mut r = ScriptReader {
            steps: VecDeque::from(vec![ScriptStep::Timeout, ScriptStep::Timeout]),
        };
        let mut buf = [0u8; 4];
        let deadline = Instant::now(); // already reached
        match read_exact_resumable(&mut r, &mut buf, deadline) {
            Err(ResumableReadError::Timeout) => {}
            _ => panic!("expected Timeout past deadline"),
        }
    }

    #[test]
    fn resumable_read_eof_midway_is_fatal() {
        // Peer closes with the buffer half-filled → EOF (unresyncable).
        let mut r = ScriptReader {
            steps: VecDeque::from(vec![ScriptStep::Data(b"ab".to_vec()), ScriptStep::Eof]),
        };
        let mut buf = [0u8; 5];
        let deadline = Instant::now() + Duration::from_secs(5);
        match read_exact_resumable(&mut r, &mut buf, deadline) {
            Err(ResumableReadError::Eof) => {}
            _ => panic!("expected Eof"),
        }
    }

    #[test]
    fn liveness_action_transitions() {
        let ping = Duration::from_secs(60);
        let dead = Duration::from_secs(150);
        // Fresh / lightly idle → keep polling.
        assert_eq!(
            liveness_action(Duration::from_secs(10), false, ping, dead),
            LivenessAction::Poll
        );
        // Past the ping threshold, no probe yet → send one.
        assert_eq!(
            liveness_action(Duration::from_secs(90), false, ping, dead),
            LivenessAction::Ping
        );
        // Past the ping threshold but a probe is already outstanding → poll
        // (don't flood PINGs every wakeup).
        assert_eq!(
            liveness_action(Duration::from_secs(90), true, ping, dead),
            LivenessAction::Poll
        );
        // Past the death threshold → dead, even with a probe outstanding.
        assert_eq!(
            liveness_action(Duration::from_secs(200), true, ping, dead),
            LivenessAction::Dead
        );
    }

    fn loopback_pair() -> (TcpStream, TcpStream) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().unwrap();
        let client = TcpStream::connect(addr).expect("connect loopback");
        let (server, _) = listener.accept().expect("accept loopback");
        (client, server)
    }

    /// Poll `read_frame` past benign timeouts. A single call can legitimately
    /// see `TimedOut` before the writer thread is scheduled — especially when
    /// the whole suite runs in parallel and starves it — so loop until a real
    /// outcome (Frame or Closed) lands, bounded by a generous wall-clock budget
    /// that only trips on a true hang.
    fn read_frame_until(reader: &mut BufReader<TcpStream>, budget: Duration) -> ReadOutcome {
        let deadline = Instant::now() + budget;
        loop {
            match read_frame(reader).expect("read_frame returned a protocol error") {
                ReadOutcome::TimedOut => {
                    if Instant::now() >= deadline {
                        panic!("no frame within {budget:?}");
                    }
                }
                other => return other,
            }
        }
    }

    #[test]
    fn read_frame_reads_whole_msg() {
        let (client, mut server) = loopback_pair();
        client.set_read_timeout(Some(Duration::from_millis(100))).unwrap();
        server.write_all(b"MSG sub.a 1 5\r\nhello\r\n").unwrap();
        server.flush().unwrap();
        let mut reader = BufReader::new(client);
        match read_frame_until(&mut reader, Duration::from_secs(10)) {
            ReadOutcome::Frame(Frame::Msg { subject, payload, .. }) => {
                assert_eq!(subject, "sub.a");
                assert_eq!(payload, b"hello");
            }
            _ => panic!("expected a MSG frame"),
        }
    }

    #[test]
    fn read_frame_survives_mid_payload_stall() {
        // #499 end-to-end over real sockets: the second half of the payload
        // arrives AFTER the socket read timeout fires mid-frame. Pre-fix this
        // returned a fatal "short MSG payload read"; now the frame completes.
        let (client, mut server) = loopback_pair();
        client.set_read_timeout(Some(Duration::from_millis(100))).unwrap();
        let writer = std::thread::spawn(move || {
            server.write_all(b"MSG sub.b 1 5\r\nhel").unwrap();
            server.flush().unwrap();
            std::thread::sleep(Duration::from_millis(250)); // straddle the 100ms timeout
            server.write_all(b"lo\r\n").unwrap();
            server.flush().unwrap();
            // keep the socket open until the reader is done
            std::thread::sleep(Duration::from_millis(100));
        });
        // read_frame_until rides out the header-not-written-yet timeout; the
        // mid-PAYLOAD stall is ridden out INSIDE read_frame itself (the #499 fix).
        let mut reader = BufReader::new(client);
        match read_frame_until(&mut reader, Duration::from_secs(10)) {
            ReadOutcome::Frame(Frame::Msg { payload, .. }) => assert_eq!(payload, b"hello"),
            _ => panic!("expected a MSG frame after the stall"),
        }
        writer.join().unwrap();
    }

    #[test]
    fn read_frame_reports_closed_on_eof() {
        let (client, server) = loopback_pair();
        client.set_read_timeout(Some(Duration::from_millis(100))).unwrap();
        drop(server); // peer closes cleanly
        let mut reader = BufReader::new(client);
        match read_frame_until(&mut reader, Duration::from_secs(10)) {
            ReadOutcome::Closed => {}
            _ => panic!("expected Closed on EOF"),
        }
    }

    #[test]
    fn subscription_self_initiates_ping_when_idle() {
        // #500: the client PROACTIVELY sends its own PING after `ping_idle` of
        // silence (it never used to initiate one). A large liveness_timeout
        // keeps this test purely about the ping — it cannot race a death.
        let (client, mut server) = loopback_pair();
        client.set_read_timeout(Some(Duration::from_millis(20))).unwrap();
        let mut sub = NatsSubscription {
            reader: BufReader::new(client),
            sid: "42".to_string(),
            subject: String::new(),
            queue_group: None,
            last_frame: Instant::now(),
            probe_sent: false,
            ping_idle: Duration::from_millis(30),
            liveness_timeout: Duration::from_secs(30), // far away — no death here
            pending: VecDeque::new(),
            denied: Vec::new(),
        };
        // Poll well past ping_idle. Each next_event blocks ~20ms, so ~40 polls
        // is ~0.8s of wall time — comfortably past 30ms even under load — and
        // the 30s deadline means no Closed can occur.
        for _ in 0..40 {
            match sub.next_event() {
                SubEvent::Timeout => {}
                SubEvent::Closed => panic!("must not die with a 30s liveness timeout"),
                SubEvent::Msg(_) => panic!("no message was ever sent"),
            }
        }
        server.set_read_timeout(Some(Duration::from_millis(500))).unwrap();
        let mut buf = [0u8; 64];
        let n = server.read(&mut buf).unwrap_or(0);
        let got = String::from_utf8_lossy(&buf[..n]);
        assert!(got.contains("PING"), "client should self-initiate a PING, got {got:?}");
    }

    #[test]
    fn subscription_declares_silent_socket_dead() {
        // #500: an OPEN but silent socket (no frames, no server PING) is
        // eventually declared Closed so the caller reconnects/exits instead of
        // hanging deaf. Generous poll budget so a loaded runner can't false-fail.
        let (client, _server) = loopback_pair();
        client.set_read_timeout(Some(Duration::from_millis(20))).unwrap();
        let mut sub = NatsSubscription {
            reader: BufReader::new(client),
            sid: "43".to_string(),
            subject: String::new(),
            queue_group: None,
            last_frame: Instant::now(),
            probe_sent: false,
            ping_idle: Duration::from_millis(40),
            liveness_timeout: Duration::from_millis(200),
            pending: VecDeque::new(),
            denied: Vec::new(),
        };
        let start = Instant::now();
        let mut saw_closed = false;
        while start.elapsed() < Duration::from_secs(15) {
            match sub.next_event() {
                SubEvent::Timeout => {}
                SubEvent::Closed => {
                    saw_closed = true;
                    break;
                }
                SubEvent::Msg(_) => panic!("no message was ever sent"),
            }
        }
        assert!(saw_closed, "a silent socket must eventually be declared Closed");
    }

    #[test]
    fn subscription_stays_alive_while_frames_flow() {
        // A message arriving keeps liveness fresh — the subscription yields it
        // and does NOT report Closed. A large liveness_timeout guarantees a
        // starved writer thread cannot trip a false death.
        let (client, mut server) = loopback_pair();
        client.set_read_timeout(Some(Duration::from_millis(20))).unwrap();
        let mut sub = NatsSubscription {
            reader: BufReader::new(client),
            sid: "44".to_string(),
            subject: String::new(),
            queue_group: None,
            last_frame: Instant::now(),
            probe_sent: false,
            ping_idle: Duration::from_millis(40),
            liveness_timeout: Duration::from_secs(30), // cannot race the writer
            pending: VecDeque::new(),
            denied: Vec::new(),
        };
        let writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(80));
            server.write_all(b"MSG sub.c 1 2\r\nhi\r\n").unwrap();
            server.flush().unwrap();
            std::thread::sleep(Duration::from_millis(50));
        });
        let start = Instant::now();
        let mut got_msg = false;
        while start.elapsed() < Duration::from_secs(10) {
            match sub.next_event() {
                SubEvent::Timeout => {}
                SubEvent::Msg(m) => {
                    assert_eq!(m.payload, b"hi");
                    got_msg = true;
                    break;
                }
                SubEvent::Closed => panic!("live socket wrongly declared closed"),
            }
        }
        assert!(got_msg, "message should have been delivered");
        writer.join().unwrap();
    }

    #[test]
    fn set_timeout_clamps_none_and_large_to_poll_max() {
        // `None` and any value above SUB_POLL_MAX must be clamped so the read
        // still wakes to run the liveness check (#500).
        let (client, _server) = loopback_pair();
        let sub = NatsSubscription {
            reader: BufReader::new(client),
            sid: "44".to_string(),
            subject: String::new(),
            queue_group: None,
            last_frame: Instant::now(),
            probe_sent: false,
            ping_idle: SUB_PING_IDLE,
            liveness_timeout: SUB_LIVENESS_TIMEOUT,
            pending: VecDeque::new(),
            denied: Vec::new(),
        };
        sub.set_timeout(None).unwrap();
        assert_eq!(sub.reader.get_ref().read_timeout().unwrap(), Some(SUB_POLL_MAX));
        sub.set_timeout(Some(Duration::from_secs(3600))).unwrap();
        assert_eq!(sub.reader.get_ref().read_timeout().unwrap(), Some(SUB_POLL_MAX));
        // A value below the cap is preserved.
        sub.set_timeout(Some(Duration::from_millis(250))).unwrap();
        assert_eq!(
            sub.reader.get_ref().read_timeout().unwrap(),
            Some(Duration::from_millis(250))
        );
    }
}
