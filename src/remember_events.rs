//! #1057 — `KANNAKA.events.memory.<agent>.remember` from the shared write path.
//!
//! Until #1057 only the `kannaka remember` subcommand published
//! `MemoryRemember`, so memories written by the agent loop, dreams, peer
//! absorb, wire sync, import and every other path never reached the durable
//! memory-event stream. Now every write is journaled by the store
//! (`MediumBackend::take_new_memory_ids`), and `KannakaMemorySystem::save`
//! publishes one event per new memory after the flush succeeds.
//!
//! ## Payload levels (`[events] remember`, env `KANNAKA_EVENTS_REMEMBER`)
//!
//! | level     | payload                                                        |
//! |-----------|----------------------------------------------------------------|
//! | `off`     | nothing is published                                           |
//! | `ids`     | memory_id, agent_id, importance, modality, via, content_sha256 |
//! | `content` | everything in `ids`, plus `content`                            |
//!
//! Every event also carries the envelope (`event_id`, `schema_version`, `ts`).
//!
//! **The default depends on where the write came from.** When the level is
//! unset, an explicit `kannaka remember` (`via = "cli"`) publishes `content`,
//! as it always has, so consumers that read the text keep receiving it. Every
//! other origin publishes `ids`. The reason for the difference: the memory
//! lane is readable (and forgeable) by `anon` (ADR-0039), and a CLI remember
//! is an operator deliberately saying something, while a dream or a peer
//! absorb is not. A configured level applies to EVERY origin, CLI included,
//! so `off` really is off.

/// Write origins carried in the event's `via` field.
pub const VIA_CLI: &str = "cli";
/// The agentic loop's `remember` tool (`kannaka agent`).
pub const VIA_AGENT: &str = "agent";
/// `/remember` inside `kannaka chat`.
pub const VIA_CHAT: &str = "chat";
/// Rows a dream created (consolidation hallucinations, wave-native dreaming).
pub const VIA_DREAM: &str = "dream";
/// `KannakaMemorySystem::hallucinate` (LLM-generated dream content).
pub const VIA_HALLUCINATE: &str = "hallucinate";
/// A peer's exemplar absorbed by `swarm join` / `swarm absorb`.
pub const VIA_ABSORB: &str = "absorb";
/// A peer's memory taken from `KANNAKA.memory.new` by `swarm sync`.
pub const VIA_SYNC: &str = "sync";
/// `kannaka import` / `import-json`.
pub const VIA_IMPORT: &str = "import";
/// `kannaka research --ingest`.
pub const VIA_RESEARCH: &str = "research";
/// `hear` / `watch` / `see`.
pub const VIA_PERCEPTION: &str = "perception";
/// The seed memories `kannaka init` writes.
pub const VIA_SEED: &str = "seed";
/// Anything that did not name itself (library callers).
pub const VIA_API: &str = "api";

/// How much of a new memory its `remember` event carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RememberLevel {
    Off,
    Ids,
    Content,
}

impl RememberLevel {
    /// Env override, above `[events] remember` in config.toml.
    pub const ENV: &'static str = "KANNAKA_EVENTS_REMEMBER";

    /// Parse a configured value. `""` and `"default"` mean "use the
    /// per-origin default" and return `Ok(None)`.
    pub fn parse(s: &str) -> Result<Option<Self>, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "" | "default" => Ok(None),
            "off" | "none" | "0" | "false" => Ok(Some(Self::Off)),
            "ids" | "id" => Ok(Some(Self::Ids)),
            "content" | "full" => Ok(Some(Self::Content)),
            other => Err(format!(
                "events.remember expects off|ids|content|default, got: {other}"
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Ids => "ids",
            Self::Content => "content",
        }
    }

    /// The level used when none is configured: `content` for an explicit
    /// CLI remember (unchanged from before #1057), `ids` for everything else.
    pub fn default_for(via: &str) -> Self {
        if via == VIA_CLI {
            Self::Content
        } else {
            Self::Ids
        }
    }

    /// A configured level wins for every origin; otherwise the per-origin default.
    pub fn resolve(configured: Option<Self>, via: &str) -> Self {
        configured.unwrap_or_else(|| Self::default_for(via))
    }

    /// Read [`Self::ENV`]. An unparseable value warns and falls back to the
    /// per-origin default rather than silently choosing a level.
    pub fn from_env() -> Option<Self> {
        let raw = std::env::var(Self::ENV).ok()?;
        match Self::parse(&raw) {
            Ok(level) => level,
            Err(e) => {
                eprintln!("[events] {}: {e}; using the per-origin default", Self::ENV);
                None
            }
        }
    }
}

/// Hex SHA-256 of a memory's content, so an `ids`-level event can still be
/// matched against text a reader already holds.
pub fn content_sha256(content: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(content.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Where remember events go. The system publishes to NATS by default; tests
/// and embedders can substitute their own.
pub trait RememberEventSink: Send {
    fn publish(&mut self, subject: &str, payload: &serde_json::Value) -> Result<(), String>;
}

/// Build one `remember` event: `(subject, payload)`, or `None` at `off`.
#[cfg(feature = "nats")]
pub fn build_remember_event(
    level: RememberLevel,
    agent_id: &str,
    memory_id: &uuid::Uuid,
    content: &str,
    importance: f32,
    modality: &str,
    via: &str,
) -> Option<(String, serde_json::Value)> {
    let content_field = match level {
        RememberLevel::Off => return None,
        RememberLevel::Ids => None,
        RememberLevel::Content => Some(content),
    };
    let sha = content_sha256(content);
    let event = crate::nats::EventPayload::MemoryRemember {
        agent_id,
        memory_id,
        content: content_field,
        content_sha256: &sha,
        importance,
        modality,
        via,
    };
    Some((event.subject(), event.payload_json()))
}

/// How long a failed connect suppresses further attempts. A daemon that
/// writes often against a downed server pays one connect timeout per window,
/// not one per write.
#[cfg(feature = "nats")]
const CONNECT_RETRY: std::time::Duration = std::time::Duration::from_secs(60);

/// The default sink: one lazily-opened NATS connection, reused for every
/// event this system publishes, and shared with callers that want to publish
/// on the same connection (`KannakaMemorySystem::event_transport`).
#[cfg(feature = "nats")]
pub struct NatsRememberSink {
    url: String,
    transport: Option<std::sync::Arc<crate::nats::SwarmTransport>>,
    last_failed: Option<std::time::Instant>,
}

#[cfg(feature = "nats")]
impl NatsRememberSink {
    pub fn new(url: String) -> Self {
        Self {
            url,
            transport: None,
            last_failed: None,
        }
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// The connection, opening it on first use. `None` while the server is
    /// unreachable; a failed attempt is not retried for [`CONNECT_RETRY`].
    ///
    /// Opened with `connect_events_only`, not `connect`: this sink, and the
    /// callers that share its connection through
    /// `KannakaMemorySystem::event_transport` (the CLI's `KANNAKA.memory.new`
    /// and substrate absorb), only ever publish. `connect`'s JetStream probes
    /// cost a JetStream-denied identity two `JS_API_TIMEOUT` waits, about
    /// 6 s, and two broker Permissions Violations per CLI write (O1's `radio`
    /// user, 2026-09-30). Nothing is lost by skipping them: the stream that
    /// retains `KANNAKA.events.memory.>` is `KANNAKA_MEMORY_EVENTS`, created
    /// by `kannaka events init`, and `connect` never created it anyway (its
    /// `ensure_events_stream` makes `QUEEN_EVENTS`, on `QUEEN.event.>`).
    pub fn transport(&mut self) -> Option<std::sync::Arc<crate::nats::SwarmTransport>> {
        if let Some(t) = &self.transport {
            return Some(t.clone());
        }
        if let Some(at) = self.last_failed {
            if at.elapsed() < CONNECT_RETRY {
                return None;
            }
        }
        match crate::nats::SwarmTransport::connect_events_only(&self.url) {
            Ok(t) => {
                let t = std::sync::Arc::new(t);
                self.transport = Some(t.clone());
                self.last_failed = None;
                Some(t)
            }
            Err(e) => {
                eprintln!(
                    "[events] NATS unavailable at {}: {e}; memory events skipped",
                    self.url
                );
                self.last_failed = Some(std::time::Instant::now());
                None
            }
        }
    }
}

#[cfg(feature = "nats")]
impl RememberEventSink for NatsRememberSink {
    fn publish(&mut self, subject: &str, payload: &serde_json::Value) -> Result<(), String> {
        let transport = self
            .transport()
            .ok_or_else(|| format!("NATS unavailable at {}", self.url))?;
        let bytes = serde_json::to_vec(payload).map_err(|e| e.to_string())?;
        transport
            .publish_memory_event(subject, &bytes)
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_accepts_levels_and_default() {
        assert_eq!(RememberLevel::parse("off"), Ok(Some(RememberLevel::Off)));
        assert_eq!(RememberLevel::parse(" IDS "), Ok(Some(RememberLevel::Ids)));
        assert_eq!(
            RememberLevel::parse("content"),
            Ok(Some(RememberLevel::Content))
        );
        assert_eq!(RememberLevel::parse(""), Ok(None));
        assert_eq!(RememberLevel::parse("default"), Ok(None));
        assert!(RememberLevel::parse("everything").is_err());
    }

    #[test]
    fn cli_defaults_to_content_everything_else_to_ids() {
        assert_eq!(
            RememberLevel::resolve(None, VIA_CLI),
            RememberLevel::Content
        );
        for via in [
            VIA_AGENT,
            VIA_CHAT,
            VIA_DREAM,
            VIA_HALLUCINATE,
            VIA_ABSORB,
            VIA_SYNC,
            VIA_IMPORT,
            VIA_RESEARCH,
            VIA_PERCEPTION,
            VIA_SEED,
            VIA_API,
        ] {
            assert_eq!(
                RememberLevel::resolve(None, via),
                RememberLevel::Ids,
                "{via}"
            );
        }
        // A configured level applies to the CLI too, so `off` means off.
        assert_eq!(
            RememberLevel::resolve(Some(RememberLevel::Off), VIA_CLI),
            RememberLevel::Off
        );
        assert_eq!(
            RememberLevel::resolve(Some(RememberLevel::Content), VIA_DREAM),
            RememberLevel::Content
        );
    }

    #[test]
    fn content_sha256_is_hex_sha256() {
        assert_eq!(
            content_sha256("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[cfg(feature = "nats")]
    #[test]
    fn ids_level_has_hash_and_no_content() {
        let id = uuid::Uuid::new_v4();
        let (subject, p) = build_remember_event(
            RememberLevel::Ids,
            "kannaka-prime",
            &id,
            "secret text",
            0.7,
            "semantic",
            VIA_DREAM,
        )
        .expect("ids publishes");
        assert_eq!(subject, "KANNAKA.events.memory.kannaka-prime.remember");
        assert!(
            p.get("content").is_none(),
            "ids must not carry content: {p}"
        );
        assert_eq!(p["content_sha256"], content_sha256("secret text"));
        assert_eq!(p["memory_id"], id.to_string());
        assert_eq!(p["agent_id"], "kannaka-prime");
        assert_eq!(p["via"], "dream");
        assert_eq!(p["modality"], "semantic");
        assert!((p["importance"].as_f64().unwrap() - 0.7).abs() < 1e-6);
        assert_eq!(p["schema_version"], "1.0");
        assert!(p["ts"].is_i64());
        assert!(p["event_id"].is_string());
    }

    #[cfg(feature = "nats")]
    #[test]
    fn content_level_adds_content_and_off_publishes_nothing() {
        let id = uuid::Uuid::new_v4();
        let (_, p) = build_remember_event(
            RememberLevel::Content,
            "a",
            &id,
            "hello",
            0.5,
            "unknown",
            VIA_CLI,
        )
        .expect("content publishes");
        assert_eq!(p["content"], "hello");
        assert_eq!(p["content_sha256"], content_sha256("hello"));
        assert_eq!(p["via"], "cli");
        assert!(
            build_remember_event(RememberLevel::Off, "a", &id, "hello", 0.5, "x", VIA_CLI)
                .is_none()
        );
    }
}
