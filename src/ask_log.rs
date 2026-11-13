//! Opt-in ask log: one JSON line per answered ask, holding the recall
//! context the model was shown and the answer it gave back.
//!
//! **Why it exists.** This is work item 1 of the self-improving dogfood loop,
//! ADR-0065 (`docs/adr/ADR-0065-self-improving-dogfood-loop.md`, merged as
//! #1083). The grader described there reads this file: to judge whether an
//! answer was grounded it needs the exact memories the prompt carried, not a
//! fresh recall run later against a medium that has since moved. Nothing else
//! in the codebase reads it.
//!
//! Set [`ENV`] (`KANNAKA_ASK_LOG=<path>`) and `kannaka swarm serve` and
//! `kannaka ask` append to that file. Unset, nothing here runs: no struct is
//! built, no file is opened, and the ask path is byte-identical to before.
//!
//! **What the file holds.** Every line carries the full text of every memory
//! that was folded into the prompt (`context[].text`), the question, and the
//! answer. That is memory text, verbatim, so the file stays local: it is
//! created `0600` on unix, it is never published to NATS, and nothing reads it
//! but the operator. Point it somewhere under the data dir, not `/tmp`.
//!
//! **What is recorded as `null` on purpose.** `temperature` and `model_digest`
//! are always `null`: the clients in `crate::agent` send no temperature and
//! learn nothing about the weights they hit, so writing anything else would be
//! a guess dressed as a measurement. `provider` and `model` are what this
//! node's own `[llm]` resolves to, not what the request asked for. `arm` is
//! `null` unless `swarm serve` was started with `KANNAKA_SERVE_PROMPT_ARM`
//! set, so a row from an unlabelled run cannot be mistaken for a study arm.
//!
//! **What is caller-declared.** `from_declared`, `reply_inbox` and
//! `requester_key` are copied from the request envelope. NATS attaches no
//! publisher identity to a message, so none of them is verified: they say who
//! the caller *claimed* to be, which is all the serve loop ever knew.
//!
//! Writes are best-effort. A failure is reported once to stderr and never
//! again, and it never fails the ask. The parent directory is created on the
//! first append.
//!
//! **Telling the asker.** Set [`NOTICE_ENV`] (`KANNAKA_ASK_LOG_NOTICE=<text>`)
//! beside [`ENV`] and `swarm serve` ends every reply it logs with that text,
//! on its own paragraph, and repeats it in the reply's `notice` field. The log
//! keeps other people's questions, so the operator says in one line what is
//! kept, for how long and who may read it. The notice is not part of the
//! answer: `answer_text` and `answer_sha256` stay the model's own words and the
//! row records the notice in `notice`. Without [`ENV`] the notice variable does
//! nothing, and a failed ask carries no notice. `swarm serve` warns at startup
//! when the log is on and no notice is set.
//!
//! Two asymmetries a reader of the file should know: a row whose `error` is set
//! by an LLM failure carries `context: []`, because the serve loop gives up
//! before the recall runs; and `kannaka ask` (the CLI path) logs only asks that
//! produced an answer, since a failed CLI ask exits before the hook.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;
use uuid::Uuid;

use crate::openclaw::RecallResult;
use crate::remember_events::content_sha256;

/// Environment variable naming the log file. Unset or empty disables the log.
pub const ENV: &str = "KANNAKA_ASK_LOG";

/// Environment variable holding the line `swarm serve` appends to every logged
/// reply so the asker knows the exchange is kept. Unset or blank: no notice.
pub const NOTICE_ENV: &str = "KANNAKA_ASK_LOG_NOTICE";

/// Read [`NOTICE_ENV`]. `None` when it is unset or blank.
pub fn notice_from_env() -> Option<String> {
    notice_from_value(std::env::var(NOTICE_ENV).ok())
}

/// The rule behind [`notice_from_env`], on an already-read value. The text is
/// trimmed; a blank value is no notice.
pub fn notice_from_value(raw: Option<String>) -> Option<String> {
    let trimmed = raw?.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

/// The reply text the asker receives: the answer, then the notice on its own
/// paragraph. With no notice the answer is returned unchanged.
pub fn with_notice(answer: &str, notice: Option<&str>) -> String {
    match notice {
        Some(n) => format!("{}

{}", answer.trim_end(), n),
        None => answer.to_string(),
    }
}

/// Append-only JSONL sink for [`AskLogEntry`] rows.
pub struct AskLog {
    path: PathBuf,
    warned: AtomicBool,
    /// The asker notice read with the path ([`NOTICE_ENV`]), if any.
    notice: Option<String>,
}

impl AskLog {
    /// Read [`ENV`]. `None` when it is unset or blank, so the caller can hold
    /// an `Option<AskLog>` and pay nothing when the log is off.
    pub fn from_env() -> Option<Self> {
        Self::from_value(std::env::var(ENV).ok()).map(|log| log.notifying(notice_from_env()))
    }

    /// The rule behind [`Self::from_env`], on an already-read value.
    pub fn from_value(raw: Option<String>) -> Option<Self> {
        let raw = raw?;
        if raw.trim().is_empty() {
            return None;
        }
        Some(Self::at(PathBuf::from(raw)))
    }

    /// A log at an explicit path. Nothing is opened until the first append.
    pub fn at(path: PathBuf) -> Self {
        Self {
            path,
            warned: AtomicBool::new(false),
            notice: None,
        }
    }

    /// The same log, telling askers `notice` on every reply it records.
    pub fn notifying(mut self, notice: Option<String>) -> Self {
        self.notice = notice;
        self
    }

    /// The line appended to each logged reply, when one is configured.
    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    /// Where rows go.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one row as a single JSON line. Never returns an error: the first
    /// failure is reported to stderr, later ones are dropped silently.
    pub fn append(&self, entry: &AskLogEntry) {
        if let Err(e) = self.try_append(entry) {
            if !self.warned.swap(true, Ordering::Relaxed) {
                eprintln!(
                    "[ask log] cannot append to {}: {e}; further failures are not logged",
                    self.path.display()
                );
            }
        }
    }

    fn try_append(&self, entry: &AskLogEntry) -> std::io::Result<()> {
        let mut line = serde_json::to_vec(entry)?;
        line.push(b'\n');
        // The operator names the file; the directory may not exist yet on a
        // fresh data dir, and a missing parent must not cost the first row.
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let mut opts = OpenOptions::new();
        opts.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // Applies only when this call creates the file; an existing file
            // keeps whatever mode the operator gave it.
            opts.mode(0o600);
        }
        let mut file = opts.open(&self.path)?;
        // One write per row so concurrent appenders (serve + a CLI ask on the
        // same box) interleave whole lines, not fragments.
        file.write_all(&line)
    }
}

/// One memory the model was shown, as it stood at the time of the ask.
#[derive(Debug, Clone, Serialize)]
pub struct AskLogContext {
    pub memory_id: Uuid,
    /// Hex SHA-256 of `text`, so a row can be matched against a
    /// `MemoryRemember` event that carried only the hash.
    pub content_sha256: String,
    pub similarity: f32,
    pub strength: f32,
    /// The memory's content, verbatim.
    pub text: String,
}

/// One answered ask.
#[derive(Debug, Clone, Serialize)]
pub struct AskLogEntry {
    /// Unix time in milliseconds when the row was built (after the reply).
    pub ts_ms: i64,
    /// The identity that answered — the served `--agent-id` when given.
    pub agent_id: String,
    /// `ask.<agent>` for a directed ask, `ask.broadcast`, or `cli`.
    pub channel: String,
    /// The envelope's `from`. Caller-declared, not verified.
    pub from_declared: Option<String>,
    /// The envelope's reply subject. Caller-chosen.
    pub reply_inbox: Option<String>,
    /// The rate limiter's key for this caller. Derived from the two above.
    pub requester_key: Option<String>,
    /// The recall mode that actually ran, in `mode_used` vocabulary.
    pub mode_used: String,
    /// The prompt arm `swarm serve` ran under (`KANNAKA_SERVE_PROMPT_ARM`, by
    /// its value name). `None` when the variable was unset, and always `None`
    /// on the CLI path, which has no such switch.
    pub arm: Option<String>,
    /// Hex SHA-256 of the question.
    pub query_sha256: String,
    /// The question. `None` when a caller chose to keep it out of the file.
    pub query_text: Option<String>,
    /// Every memory folded into the prompt, in prompt order.
    pub context: Vec<AskLogContext>,
    /// This node's `[llm] provider`, or `"unknown"` when no client resolves.
    pub provider: String,
    /// The model name requests are sent with.
    pub model: Option<String>,
    /// Always `None`: the client is not told which weights it reached.
    pub model_digest: Option<String>,
    /// Always `None`: no client sends a temperature.
    pub temperature: Option<f32>,
    /// The `max_tokens` cap the ask path used.
    pub max_tokens: u32,
    /// The answer, when the model returned one.
    pub answer_text: Option<String>,
    /// Hex SHA-256 of `answer_text`.
    pub answer_sha256: Option<String>,
    /// The ask error, when the model returned none.
    pub error: Option<String>,
    /// Wall time from dispatch to the end of the reply attempt.
    pub latency_ms: u64,
    /// Whether the answer reached the caller (`true` for a printed CLI answer).
    pub reply_ok: bool,
    /// The notice appended to the reply ([`NOTICE_ENV`]). `None` when no notice
    /// was sent: the variable was unset, the ask failed, or this is the CLI path.
    pub notice: Option<String>,
}

/// Map surfaced memories to log rows, in the order the prompt saw them.
pub fn context_from(surfaced: &[RecallResult]) -> Vec<AskLogContext> {
    surfaced
        .iter()
        .map(|r| AskLogContext {
            memory_id: r.id,
            content_sha256: content_sha256(&r.content),
            similarity: r.similarity,
            strength: r.strength,
            text: r.content.clone(),
        })
        .collect()
}

/// Unix time in milliseconds, for `ts_ms`.
pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(channel: &str, answer: Option<&str>) -> AskLogEntry {
        AskLogEntry {
            ts_ms: 1_700_000_000_000,
            agent_id: "kannaka-test".to_string(),
            channel: channel.to_string(),
            from_declared: Some("peer".to_string()),
            reply_inbox: Some("_INBOX.abc".to_string()),
            requester_key: Some("peer".to_string()),
            mode_used: "attention".to_string(),
            arm: None,
            query_sha256: content_sha256("why?"),
            query_text: Some("why?".to_string()),
            context: Vec::new(),
            provider: "anthropic".to_string(),
            model: Some("claude-sonnet-4-5".to_string()),
            model_digest: None,
            temperature: None,
            max_tokens: 512,
            answer_text: answer.map(str::to_string),
            answer_sha256: answer.map(content_sha256),
            error: if answer.is_none() {
                Some("boom".to_string())
            } else {
                None
            },
            latency_ms: 42,
            reply_ok: answer.is_some(),
            notice: None,
        }
    }

    fn recall(id: Uuid, content: &str, similarity: f32, strength: f32) -> RecallResult {
        RecallResult {
            id,
            content: content.to_string(),
            similarity,
            strength,
            intuition: false,
            age_hours: 1.0,
            layer: 0,
            times_seen: 1,
        }
    }

    #[test]
    fn a_log_carries_its_notice_and_has_none_by_default() {
        let log = AskLog::at(PathBuf::from("asks.jsonl"));
        assert!(log.notice().is_none());
        let log = log.notifying(Some("kept 90 days".to_string()));
        assert_eq!(log.notice(), Some("kept 90 days"));
    }

    #[test]
    fn notice_is_trimmed_and_blank_means_none() {
        assert!(notice_from_value(None).is_none());
        assert!(notice_from_value(Some(String::new())).is_none());
        assert!(notice_from_value(Some("  
 ".to_string())).is_none());
        assert_eq!(
            notice_from_value(Some("  kept 90 days 
".to_string())).as_deref(),
            Some("kept 90 days")
        );
    }

    #[test]
    fn with_notice_appends_one_paragraph_and_leaves_a_bare_answer_alone() {
        assert_eq!(with_notice("an answer
", None), "an answer
");
        assert_eq!(
            with_notice("an answer
", Some("kept 90 days")),
            "an answer

kept 90 days"
        );
    }

    #[test]
    fn notice_is_recorded_beside_the_answer_not_inside_it() {
        let mut e = entry("ask.kannaka-test", Some("an answer"));
        e.notice = Some("kept 90 days".to_string());
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["notice"], "kept 90 days");
        assert_eq!(v["answer_text"], "an answer");
        assert_eq!(v["answer_sha256"], content_sha256("an answer"));
    }

    #[test]
    fn append_writes_one_json_line_per_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("asks.jsonl");
        let log = AskLog::at(path.clone());

        log.append(&entry("ask.kannaka-test", Some("because")));
        log.append(&entry("cli", None));

        let raw = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = raw.lines().collect();
        assert_eq!(lines.len(), 2, "two appends, two lines: {raw:?}");
        assert!(raw.ends_with('\n'), "every row is newline-terminated");

        let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(first["channel"], "ask.kannaka-test");
        assert_eq!(first["agent_id"], "kannaka-test");
        assert_eq!(first["answer_text"], "because");
        assert_eq!(first["answer_sha256"], content_sha256("because"));
        assert_eq!(first["query_sha256"], content_sha256("why?"));
        assert!(first["error"].is_null());
        assert!(first["temperature"].is_null());
        assert!(first["model_digest"].is_null());
        assert!(first["arm"].is_null(), "unset arm must serialize as null, not be omitted");
        assert_eq!(first["max_tokens"], 512);
        assert_eq!(first["reply_ok"], true);
        for key in [
            "ts_ms",
            "from_declared",
            "reply_inbox",
            "requester_key",
            "mode_used",
            "query_text",
            "context",
            "provider",
            "model",
            "latency_ms",
        ] {
            assert!(first.get(key).is_some(), "missing key {key}");
        }

        let second: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(second["channel"], "cli");
        assert!(second["answer_text"].is_null());
        assert_eq!(second["error"], "boom");
        assert_eq!(second["reply_ok"], false);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "log holds memory text; must be owner-only");
        }
    }

    #[test]
    fn append_never_panics_on_an_unwritable_path() {
        let dir = tempfile::tempdir().unwrap();
        // A directory where the file should be: open() fails, append() must
        // swallow it (once to stderr) rather than propagate.
        let log = AskLog::at(dir.path().to_path_buf());
        log.append(&entry("cli", Some("x")));
        log.append(&entry("cli", Some("y")));
        assert!(log.warned.load(Ordering::Relaxed));
    }

    #[test]
    fn from_value_is_none_when_unset_or_blank() {
        // Exercises the rule on an explicit value rather than the process
        // environment, so it cannot race other tests over `set_var`.
        assert!(AskLog::from_value(None).is_none());
        assert!(AskLog::from_value(Some(String::new())).is_none());
        assert!(AskLog::from_value(Some("   ".to_string())).is_none());
        let log = AskLog::from_value(Some("/tmp/asks.jsonl".to_string())).unwrap();
        assert_eq!(log.path(), Path::new("/tmp/asks.jsonl"));
    }

    #[test]
    fn context_from_maps_every_field() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let surfaced = vec![
            recall(a, "the sky is blue", 0.9, 0.8),
            recall(b, "water is wet", 0.5, 0.4),
        ];

        let ctx = context_from(&surfaced);

        assert_eq!(ctx.len(), 2);
        assert_eq!(ctx[0].memory_id, a);
        assert_eq!(ctx[0].text, "the sky is blue");
        assert_eq!(ctx[0].content_sha256, content_sha256("the sky is blue"));
        assert_eq!(ctx[0].similarity, 0.9);
        assert_eq!(ctx[0].strength, 0.8);
        assert_eq!(ctx[1].memory_id, b);
        assert_eq!(ctx[1].text, "water is wet");
        assert_eq!(ctx[1].similarity, 0.5);
        assert_eq!(ctx[1].strength, 0.4);
        assert!(context_from(&[]).is_empty());
    }
}
