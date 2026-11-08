//! #1157 — `KANNAKA.events.memory.<agent>.forget` from every delete path.
//!
//! Until #1157 the `MemoryForget` event was defined (ADR-0028) but never
//! sent: `kannaka forget` printed one line to whoever ran it, a dream logged
//! how many rows it pruned but not which, and nothing else was recorded. Now
//! every removal is journaled by the store at the moment it happens
//! (`MediumBackend::take_deleted_rows`, including rows a dream prunes or
//! merges inside the medium without going through `delete`), and
//! `KannakaMemorySystem::save` publishes one event per removed memory after
//! the flush that made the removal durable.
//!
//! ## Payload (`[events] forget`, env `KANNAKA_EVENTS_FORGET` = `off|ids`)
//!
//! | key            | meaning                                                  |
//! |----------------|----------------------------------------------------------|
//! | `agent_id`     | the node whose store lost the row (ADR-0028, unchanged)  |
//! | `memory_id`    | the removed row (ADR-0028, unchanged)                    |
//! | `via`          | who removed it: `cli`, `chat`, `dream`, `api`            |
//! | `reason`       | why: see the `REASON_*` constants                        |
//! | `merged_into`  | the row it was folded into; ABSENT when not a merge      |
//!
//! plus the envelope (`event_id`, `schema_version`, `ts`). The event never
//! carries the deleted text or a hash of it: the memory lane is readable by
//! `anon` (ADR-0039), and a hash of short text is a fingerprint of it. The id
//! is all replay needs.
//!
//! ## What it does not promise
//!
//! Best-effort, like `remember`: with no agent id, NATS down, a denied
//! publish, or a crash between the flush and the publish, the delete stands
//! and its event is lost. The caller is told (`ForgetPublishOutcome`); the
//! CLI prints it. A durable outbox is a follow-up. Events are also unsigned:
//! anyone who can publish on the memory lane can forge one.

use uuid::Uuid;

/// The operator named the row (`kannaka forget`, chat `/forget`).
pub const REASON_OPERATOR: &str = "operator";
/// `kannaka prune-prefix`.
pub const REASON_PREFIX: &str = "prefix";
/// ADR-0031 triage: a redundant low-value short-term extra.
pub const REASON_TRIAGE_REDUNDANT: &str = "triage-redundant";
/// The hard size cap (`triage --max-total`, `KANNAKA_MAX_MEMORIES`).
pub const REASON_TRIAGE_CAP: &str = "triage-cap";
/// `kannaka dedupe --apply`: an exact duplicate folded into its keeper.
pub const REASON_DEDUPE_FOLD: &str = "dedupe-fold";
/// Particle consolidation inside a dream (ghost compaction, pruning).
pub const REASON_DREAM_COMPACT: &str = "dream-compact";
/// Removed by a dream stage that did not name itself (fallback diff).
pub const REASON_DREAM_UNATTRIBUTED: &str = "dream-unattributed";
/// A library caller that did not say why.
pub const REASON_UNSPECIFIED: &str = "unspecified";

/// How much a forget event carries. There is no `content` level: the event
/// never carries the deleted text (see the module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForgetLevel {
    Off,
    Ids,
}

impl ForgetLevel {
    /// Env override, above `[events] forget` in config.toml.
    pub const ENV: &'static str = "KANNAKA_EVENTS_FORGET";

    /// Parse a configured value. `""` and `"default"` mean `ids`.
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "" | "default" | "ids" | "id" | "on" => Ok(Self::Ids),
            "off" | "none" | "0" | "false" => Ok(Self::Off),
            other => Err(format!("events.forget expects off|ids|default, got: {other}")),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Ids => "ids",
        }
    }

    /// Read [`Self::ENV`]; unset or unparseable (with a warning) is `ids`.
    pub fn from_env() -> Self {
        match std::env::var(Self::ENV) {
            Ok(raw) => Self::parse(&raw).unwrap_or_else(|e| {
                eprintln!("[events] {}: {e}; using ids", Self::ENV);
                Self::Ids
            }),
            Err(_) => Self::Ids,
        }
    }
}

/// One removal waiting for a durable save, with who did it and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgetRecord {
    pub id: Uuid,
    pub via: &'static str,
    pub reason: &'static str,
    pub merged_into: Option<Uuid>,
}

/// What happened to the forget events of one save. `announced` counts the
/// removals that should have produced an event; `published` the ones that
/// did. Every other one is in `not_published`, with the last reason, so a
/// delete that leaves no trace says so instead of failing silently.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ForgetPublishOutcome {
    pub announced: usize,
    pub published: usize,
    pub not_published: usize,
    pub reason: Option<String>,
}

impl ForgetPublishOutcome {
    pub fn merge(&mut self, other: &ForgetPublishOutcome) {
        self.announced += other.announced;
        self.published += other.published;
        self.not_published += other.not_published;
        if other.reason.is_some() {
            self.reason = other.reason.clone();
        }
    }

    /// The one line the CLI prints after a delete.
    pub fn summary_line(&self) -> String {
        if self.not_published == 0 {
            format!("forget event: published {}", self.published)
        } else {
            format!(
                "forget event: published {} / NOT published {} ({})",
                self.published,
                self.not_published,
                self.reason.as_deref().unwrap_or("unknown")
            )
        }
    }
}

/// Build one `forget` event: `(subject, payload)`.
#[cfg(feature = "nats")]
pub fn build_forget_event(agent_id: &str, rec: &ForgetRecord) -> (String, serde_json::Value) {
    let event = crate::nats::EventPayload::MemoryForget {
        agent_id,
        memory_id: &rec.id,
        via: rec.via,
        reason: rec.reason,
        merged_into: rec.merged_into.as_ref(),
    };
    (event.subject(), event.payload_json())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_levels() {
        assert_eq!(ForgetLevel::parse("off"), Ok(ForgetLevel::Off));
        assert_eq!(ForgetLevel::parse(" IDS "), Ok(ForgetLevel::Ids));
        assert_eq!(ForgetLevel::parse(""), Ok(ForgetLevel::Ids));
        assert_eq!(ForgetLevel::parse("default"), Ok(ForgetLevel::Ids));
        assert!(ForgetLevel::parse("content").is_err(), "no content level by design");
    }

    #[test]
    fn summary_line_names_what_was_not_published() {
        let ok = ForgetPublishOutcome { announced: 2, published: 2, not_published: 0, reason: None };
        assert_eq!(ok.summary_line(), "forget event: published 2");
        let bad = ForgetPublishOutcome {
            announced: 1,
            published: 0,
            not_published: 1,
            reason: Some("no agent id".into()),
        };
        assert_eq!(bad.summary_line(), "forget event: published 0 / NOT published 1 (no agent id)");
    }

    #[cfg(feature = "nats")]
    #[test]
    fn forget_payload_has_ids_via_reason_and_no_text() {
        let id = Uuid::new_v4();
        let keeper = Uuid::new_v4();
        let (subject, p) = build_forget_event(
            "kannaka-prime",
            &ForgetRecord { id, via: "cli", reason: REASON_DEDUPE_FOLD, merged_into: Some(keeper) },
        );
        assert_eq!(subject, "KANNAKA.events.memory.kannaka-prime.forget");
        assert_eq!(p["memory_id"], id.to_string());
        assert_eq!(p["agent_id"], "kannaka-prime");
        assert_eq!(p["via"], "cli");
        assert_eq!(p["reason"], "dedupe-fold");
        assert_eq!(p["merged_into"], keeper.to_string());
        assert!(p.get("content").is_none() && p.get("content_sha256").is_none(), "{p}");

        let (_, plain) = build_forget_event(
            "a",
            &ForgetRecord { id, via: "dream", reason: "dream-anneal", merged_into: None },
        );
        assert!(plain.get("merged_into").is_none(), "absent, not null: {plain}");
    }
}
