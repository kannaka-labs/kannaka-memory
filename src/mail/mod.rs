//! ADR-0064 native mail — Phase 0 (read-only).
//!
//! The mailbox is the record; kannaka holds **references** into it. `sync`
//! pulls headers + flags from each account's authority (IMAP for Zoho, JMAP
//! for Stalwart) and writes [`MailRef`]s to a sidecar, `<data_dir>/mail/refs.jsonl`.
//! No bodies are stored: only a body hash and three surface features of the
//! sender's own words ([`parse::OwnText`]). Nothing here touches the HRM
//! (ADR-0063: no mail state as waves).
//!
//! `status` computes open loops per thread with deterministic rules
//! ([`rules`]); `close` records a resolution that happened elsewhere
//! ([`store::Closure`]), which `status` honours until a newer message arrives.
//!
//! Strictly read-only against the servers: IMAP `EXAMINE` + `BODY.PEEK` /
//! `UID FETCH` / `UID SEARCH` only; JMAP `Mailbox/get`, `Email/query`,
//! `Email/get`, `Thread/get` only. Both clients refuse anything else before it
//! reaches the wire (see `imap::guard_read_only`, `jmap::guard_method`).

pub mod config;
pub mod imap;
pub mod jmap;
pub mod parse;
pub mod rules;
pub mod store;
pub mod sync;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// An email address with its display name. `email` is lowercased.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Addr {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    pub email: String,
}

/// Which side of the mailbox a message was found in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Inbox,
    Sent,
}

/// Header facts the automated-mail rules read. Raw-ish on purpose, so a rule
/// change does not need a re-sync.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoHeaders {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub list_id: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub list_unsubscribe: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_submitted: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub precedence: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub feedback_id: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub return_path: Option<String>,
}

/// A reference to one message on its authority (ADR-0064 §2). No body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MailRef {
    /// Account name from the accounts config (e.g. `zoho`, `np`).
    pub account: String,
    /// Server folder / mailbox name as the authority names it.
    pub folder: String,
    pub role: Role,
    /// Server identity: IMAP `"<uidvalidity>:<uid>"`, JMAP the Email id.
    pub server_id: String,
    /// IMAP UID when the transport is IMAP.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uid: Option<u32>,
    /// JMAP threadId when the transport provides one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_thread: Option<String>,
    pub message_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub in_reply_to: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<String>,
    pub from: Addr,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub to: Vec<Addr>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cc: Vec<Addr>,
    pub subject: String,
    pub date: Option<DateTime<Utc>>,
    /// Flags / keywords snapshot at sync time, IMAP spelling (`\Seen`, `\Answered`).
    #[serde(default)]
    pub flags: Vec<String>,
    #[serde(default)]
    pub auto: AutoHeaders,
    /// blake3 of the body as served (see [`parse::body_hash`]).
    pub body_hash: String,
    #[serde(default)]
    pub own: parse::OwnText,
    pub synced_at: DateTime<Utc>,
}

impl MailRef {
    pub fn has_flag(&self, flag: &str) -> bool {
        self.flags.iter().any(|f| f.eq_ignore_ascii_case(flag))
    }
    /// `account:folder:uid` (IMAP) or `account:folder:id` (JMAP).
    pub fn anchor(&self) -> String {
        match self.uid {
            Some(u) => format!("{}:{}:{}", self.account, self.folder, u),
            None => format!("{}:{}:{}", self.account, self.folder, self.server_id),
        }
    }
}

/// Errors from the mail subsystem. Never carries a credential.
#[derive(Debug, thiserror::Error)]
pub enum MailError {
    #[error("config: {0}")]
    Config(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("imap: {0}")]
    Imap(String),
    #[error("jmap: {0}")]
    Jmap(String),
    #[error("refused (read-only P0): {0}")]
    ReadOnly(String),
}

pub type Result<T> = std::result::Result<T, MailError>;
