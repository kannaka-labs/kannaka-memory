//! handlers/inbox.rs — agent-to-agent declarative messaging over NATS.
//!
//! Three subcommands:
//!
//!   kannaka inbox send <to> <verb> [--arg key=val ...] [--from <id>] [--wait [secs]]
//!     One-shot: publishes JSON `{from, to, verb, args, ts, msg_id}` to
//!     `KANNAKA.inbox.<to>` and also fans out to `KANNAKA.inbox.audit`
//!     so observers can watch the conversation live.
//!
//!     **Inbox messages are live-only and NOT stored.** No JetStream stream
//!     captures `KANNAKA.inbox.>`; the only reader is an `inbox serve` daemon
//!     subscribed to `KANNAKA.inbox.<to>` at the moment of sending. An agent
//!     that runs `swarm join`/`swarm serve` but not `inbox serve` receives
//!     nothing, and the message is gone. So without `--wait` the send asks
//!     the broker (NATS no-responders) whether anything was subscribed:
//!       - nothing subscribed → "NOT DELIVERED", exit 3, nothing printed on
//!         stdout;
//!       - a subscriber got it → the message JSON on stdout, exit 0 (this
//!         proves a subscriber received it, not that a handler ran, and a
//!         wildcard observer on `KANNAKA.inbox.>` counts as a subscriber;
//!         use `--wait` for the handler's reply);
//!       - broker could not say (old server, reply inbox refused) → the
//!         message JSON on stdout plus an UNCONFIRMED warning, exit 0;
//!       - broker refused the publish (ACL) → exit 1.
//!     For delivery that survives the recipient being offline, email the
//!     agent: the ADR-0062/0064 mail lane lands it on
//!     `KANNAKA.mail.<slug>.inbound`, retained in stream `KANNAKA_MAIL_V2`.
//!     `--wait` is unchanged: it waits for the handler's reply (exit 2 when
//!     none arrives in time).
//!
//!   kannaka inbox serve [--agent-id <id>] [--handlers <path>]
//!     Daemon: subscribes to `KANNAKA.inbox.<agent_id>`. For each incoming
//!     message, looks up `verb` in the handlers table (defaults to
//!     `~/.kannaka/inbox-handlers.toml`). If the verb is whitelisted and
//!     the args validate, runs the command template and audits the
//!     result. Anything not in the whitelist is rejected with status
//!     `unknown_verb`. There is no eval; arg substitution is plain
//!     string templating (`{{args.foo}}`, `{{from}}`).
//!
//!   kannaka inbox tail [--agent-id <id>] [--last <N>]
//!     Subscribes to `KANNAKA.inbox.audit` and prints every conversation
//!     line to stdout as it lands. Use `--agent-id <id>` to filter to
//!     only messages to/from that agent. Ctrl+C to stop.
//!
//! Handlers config schema (`~/.kannaka/inbox-handlers.toml`):
//!
//!   [handler.greet]
//!   cmd = "echo 'Hello {{args.name}} from {{from}}'"
//!   args = ["name"]
//!   timeout_secs = 30
//!
//!   [handler.recall]
//!   cmd = "kannaka recall '{{args.query}}' --top-k 3"
//!   args = ["query"]
//!
//! The `args` list declares which arg keys the handler *requires*. Extra
//! keys in the message payload are ignored. Missing keys = `arg_missing`
//! status. No shell interpolation beyond the template — the cmd line is
//! handed to `sh -c` after substitution, which means **the operator is
//! responsible for putting their own shell-injection guards in the cmd
//! string** if any arg can contain quotes. The whitelist makes this a
//! per-agent choice rather than a blanket exposure.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process;

use super::KannakaConfig;
#[cfg(feature = "nats")]
use super::{flag_value, resolve_nats_url};
#[cfg(feature = "nats")]
use kannaka_memory::nats::{ListenerProbe, SubEvent};

/// How long `inbox send` waits for the broker to say whether anyone is
/// subscribed. The answer normally comes back in one round trip (the probe's
/// PING/PONG barrier); this only bounds a slow or silent server.
#[cfg(feature = "nats")]
const DELIVERY_PROBE_WINDOW: std::time::Duration = std::time::Duration::from_millis(1500);

/// Exit code for "the broker says nobody is listening — message dropped".
/// Distinct from 1 (errors) and 2 (`--wait` timed out).
#[cfg(feature = "nats")]
const EXIT_NOT_DELIVERED: i32 = 3;

/// Where to send something that must survive the recipient being offline.
/// `kannaka mail send` is ADR-0064 P2 and not shipped yet, so point at the
/// lane that exists: the agent's mailbox, which the ADR-0062 membrane puts on
/// the bus durably.
#[cfg(feature = "nats")]
const DURABLE_HINT: &str = "for durable delivery, email the agent instead: the ADR-0062/0064 mail lane \
    puts it on KANNAKA.mail.<slug>.inbound, retained in stream KANNAKA_MAIL_V2 (`kannaka mail send` is not shipped yet; \
    tools/mail/agent-mail.py send works today)";

/// What `inbox send` (no `--wait`) tells the sender, derived purely from the
/// broker's answer so it can be tested without a server.
#[cfg(feature = "nats")]
#[derive(Debug, PartialEq, Eq)]
struct SendReport {
    /// Process exit code.
    exit_code: i32,
    /// Audit-record `delivery` value.
    delivery: &'static str,
    /// Print the message JSON on stdout (the historical success output)?
    print_payload: bool,
    /// Lines for stderr.
    notice: Vec<String>,
}

#[cfg(feature = "nats")]
fn send_report(to: &str, msg_id: &str, probe: &ListenerProbe) -> SendReport {
    let subject = format!("KANNAKA.inbox.{to}");
    match probe {
        ListenerProbe::Listening => SendReport {
            exit_code: 0,
            delivery: "listener",
            print_payload: true,
            notice: vec![format!(
                "[inbox send] delivered msg {msg_id} to a live subscriber on {subject} \
                 (receipt only, not handling: add --wait to get the handler's reply)"
            )],
        },
        ListenerProbe::NoListener => SendReport {
            exit_code: EXIT_NOT_DELIVERED,
            delivery: "no_listener",
            print_payload: false,
            notice: vec![
                format!(
                    "[inbox send] NOT DELIVERED: no live inbox listener for '{to}'. Nothing is subscribed to \
                     {subject} and inbox messages are not stored, so msg {msg_id} was dropped."
                ),
                format!("[inbox send] '{to}' must be running `kannaka inbox serve` to receive inbox messages."),
                format!("[inbox send] {DURABLE_HINT}"),
            ],
        },
        ListenerProbe::Denied(reason) => SendReport {
            exit_code: 1,
            delivery: "denied",
            print_payload: false,
            notice: vec![format!("[inbox send] NOT DELIVERED: {reason}")],
        },
        ListenerProbe::Unconfirmed(reason) => SendReport {
            exit_code: 0,
            delivery: "unconfirmed",
            print_payload: true,
            notice: vec![
                format!("[inbox send] WARNING: delivery of msg {msg_id} is UNCONFIRMED: {reason}."),
                format!(
                    "[inbox send] inbox messages are live-only and not stored: if '{to}' is not running \
                     `kannaka inbox serve` right now, it is lost."
                ),
                format!("[inbox send] {DURABLE_HINT}"),
            ],
        },
    }
}

/// Reply subjects the `--wait` sender actually subscribes to. `inbox serve`
/// refuses to publish handler output anywhere else — a forged `reply_to`
/// in an inbound message must not turn the daemon into an arbitrary-subject
/// publisher.
#[cfg(feature = "nats")]
const REPLY_SUBJECT_PREFIX: &str = "KANNAKA.inbox.reply.";

/// Stderr warning for unrecognized `--flags` (non-text handlers); unknown
/// flags used to be silently swallowed by `_ => i += 1`.
#[cfg(feature = "nats")]
fn warn_unknown_flag(ctx: &str, arg: &str) {
    if arg.starts_with("--") {
        eprintln!("[{ctx}] ignoring unknown flag: {arg}");
    }
}

/// Default handlers config path: $KANNAKA_INBOX_HANDLERS or
/// $HOME/.kannaka/inbox-handlers.toml.
fn default_handlers_path() -> PathBuf {
    if let Ok(p) = std::env::var("KANNAKA_INBOX_HANDLERS") {
        return PathBuf::from(p);
    }
    if let Some(home) = dirs::home_dir() {
        return home.join(".kannaka").join("inbox-handlers.toml");
    }
    PathBuf::from("inbox-handlers.toml")
}

/// One handler entry parsed from the toml table.
#[derive(Debug, Clone)]
struct HandlerSpec {
    cmd_template: String,
    required_args: Vec<String>,
    timeout_secs: u64,
    /// Optional human-readable description from the toml `description`
    /// key. Surfaces in the radio's Constellation Skills panel so a
    /// caller knows what a verb does without reading the operator's
    /// handlers.toml.
    description: Option<String>,
}

/// Verb → spec.
type HandlerTable = HashMap<String, HandlerSpec>;

fn load_handlers(path: &PathBuf) -> Result<HandlerTable, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| format!("could not read {}: {e}", path.display()))?;
    let parsed: toml::Value = raw.parse().map_err(|e| format!("toml parse: {e}"))?;
    let mut out = HandlerTable::new();
    let handlers = parsed
        .get("handler")
        .and_then(|v| v.as_table())
        .ok_or_else(|| "missing [handler.<verb>] tables".to_string())?;
    for (verb, val) in handlers {
        let tbl = val.as_table().ok_or_else(|| format!("handler.{verb} not a table"))?;
        let cmd_template = tbl
            .get("cmd")
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("handler.{verb}.cmd missing or not a string"))?
            .to_string();
        let required_args: Vec<String> = tbl
            .get("args")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|x| x.as_str().map(String::from)).collect())
            .unwrap_or_default();
        let timeout_secs = tbl
            .get("timeout_secs")
            .and_then(|v| v.as_integer())
            .map(|n| n.max(1) as u64)
            .unwrap_or(60);
        let description = tbl
            .get("description")
            .and_then(|v| v.as_str())
            .map(String::from);
        out.insert(
            verb.clone(),
            HandlerSpec { cmd_template, required_args, timeout_secs, description },
        );
    }
    Ok(out)
}

/// Wrap `s` in single quotes for safe inclusion inside another sh -c
/// invocation. Any single-quote inside is closed-and-re-opened.
fn shell_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

/// Resolve `{{args.foo}}` and `{{from}}` in `template` against a message.
/// Plain literal substitution; no escaping (the operator's cmd line is
/// responsible for shell-safety of its own arg vocabulary).
fn render_cmd(
    template: &str,
    from: &str,
    args: &serde_json::Map<String, serde_json::Value>,
) -> String {
    let mut out = template.replace("{{from}}", from);
    for (k, v) in args {
        let needle = format!("{{{{args.{k}}}}}");
        let replacement = match v {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        out = out.replace(&needle, &replacement);
    }
    out
}

/// ----------------------------------------------------------------------
/// `kannaka inbox send <to> <verb> [--arg key=val ...] [--from <id>] [--wait [secs]]`
/// ----------------------------------------------------------------------
///
/// With `--wait` the sender subscribes to a unique reply subject BEFORE
/// publishing, then blocks until the handler's response (or timeout)
/// arrives there. Without `--wait` the publish carries a NATS reply inbox on
/// a no-responders connection, so the broker reports whether any subscriber
/// existed; see `send_report` for what the sender is told.
#[cfg(feature = "nats")]
pub(crate) fn handle_inbox_send(cfg: &KannakaConfig, args: &[String]) {
    use std::time::Duration;
    const USAGE: &str = "Usage: kannaka inbox send <to> <verb> [--arg key=val ...] [--from <id>] [--wait [secs]] [--nats-url URL]";
    if args.len() < 4 {
        eprintln!("{USAGE}");
        process::exit(1);
    }
    let to = args[2].clone();
    let verb = args[3].clone();
    let mut from = cfg.agent.id.clone();
    let mut arg_map = serde_json::Map::new();
    let mut wait_secs: Option<u64> = None;
    let mut i = 4;
    while i < args.len() {
        match args[i].as_str() {
            "--from" => {
                from = flag_value(args, i, "--from", USAGE).to_string();
                i += 2;
            }
            "--arg" => {
                let kv = flag_value(args, i, "--arg", USAGE);
                if let Some(eq) = kv.find('=') {
                    let k = kv[..eq].to_string();
                    let v = kv[eq + 1..].to_string();
                    arg_map.insert(k, serde_json::Value::String(v));
                } else {
                    eprintln!("--arg expects key=value, got: {kv}");
                    process::exit(1);
                }
                i += 2;
            }
            "--wait" => {
                // Optional next-arg is the timeout; default 30s.
                if i + 1 < args.len() {
                    if let Ok(n) = args[i + 1].parse::<u64>() {
                        wait_secs = Some(n.max(1).min(600));
                        i += 2;
                        continue;
                    }
                }
                wait_secs = Some(30);
                i += 1;
            }
            "--nats-url" => { let _ = flag_value(args, i, "--nats-url", USAGE); i += 2; }
            _ => {
                eprintln!("unknown flag: {}", args[i]);
                eprintln!("{USAGE}");
                process::exit(2);
            }
        }
    }
    let nats_url = resolve_nats_url(args, 0, &cfg.swarm.nats_url);
    if wait_secs.is_none() {
        send_probed(&nats_url, &from, &to, &verb, arg_map);
        return;
    }
    let transport = match kannaka_memory::nats::SwarmTransport::connect(&nats_url) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("NATS connect failed at {nats_url}: {e}");
            process::exit(1);
        }
    };
    let msg_id = uuid::Uuid::new_v4().to_string();
    let ts = chrono::Utc::now().to_rfc3339();

    // When --wait is set, prepare a reply subject and subscribe BEFORE
    // publishing. NATS auto-cleans subscriptions on disconnect, so the
    // reply sub lifetime is naturally bounded by this command's run.
    let reply_subject: Option<String> = wait_secs.map(|_| format!("KANNAKA.inbox.reply.{msg_id}"));
    let reply_sub = if let Some(subj) = &reply_subject {
        match transport.subscribe(subj) {
            Ok(s) => {
                let _ = s.set_timeout(Some(Duration::from_secs(2)));
                Some(s)
            }
            Err(e) => {
                eprintln!("subscribe reply {subj}: {e}");
                process::exit(1);
            }
        }
    } else {
        None
    };

    let mut payload_obj = serde_json::json!({
        "msg_id": msg_id,
        "from": from,
        "to": to,
        "verb": verb,
        "args": arg_map,
        "ts": ts,
    });
    if let Some(subj) = &reply_subject {
        payload_obj["reply_to"] = serde_json::Value::String(subj.clone());
    }
    let payload_bytes = serde_json::to_vec(&payload_obj).unwrap_or_default();
    let directed_subject = format!("KANNAKA.inbox.{to}");
    if let Err(e) = transport.publish(&directed_subject, &payload_bytes) {
        eprintln!("publish {directed_subject}: {e}");
        process::exit(1);
    }
    // Audit fan-out (so tail watchers see the outbound).
    let audit = serde_json::json!({
        "ts": ts,
        "phase": "sent",
        "msg_id": msg_id,
        "from": from,
        "to": to,
        "verb": verb,
        "args": arg_map,
    });
    let audit_bytes = serde_json::to_vec(&audit).unwrap_or_default();
    let _ = transport.publish("KANNAKA.inbox.audit", &audit_bytes);

    if let (Some(mut sub), Some(secs)) = (reply_sub, wait_secs) {
        let deadline = std::time::Instant::now() + Duration::from_secs(secs);
        while std::time::Instant::now() < deadline {
            match sub.next_event() {
                SubEvent::Msg(msg) => {
                    // Reply payload mirrors the audit "received" envelope,
                    // so callers parse a known shape.
                    std::io::Write::write_all(&mut std::io::stdout(), &msg.payload).ok();
                    println!();
                    // Exit nonzero when the handler did NOT succeed — pre-fix
                    // a `handler_failed` / `unknown_verb` envelope still
                    // exited 0, so scripted callers couldn't tell.
                    let status = serde_json::from_slice::<serde_json::Value>(&msg.payload)
                        .ok()
                        .and_then(|v| v.get("status").and_then(|s| s.as_str()).map(String::from))
                        .unwrap_or_default();
                    if status == "ok" {
                        return;
                    }
                    eprintln!("[inbox send] handler reply status: {}", if status.is_empty() { "(missing)" } else { &status });
                    process::exit(1);
                }
                SubEvent::Timeout => continue,
                SubEvent::Closed => {
                    eprintln!("[inbox send] reply subscription closed before a reply arrived");
                    process::exit(1);
                }
            }
        }
        eprintln!("[inbox send] no reply within {secs}s — handler may still run async");
        process::exit(2);
    }

    println!("{}", serde_json::to_string(&payload_obj).unwrap_or_default());
}

/// `inbox send` without `--wait`: publish with a delivery probe and tell the
/// sender what the broker said.
#[cfg(feature = "nats")]
fn send_probed(
    nats_url: &str,
    from: &str,
    to: &str,
    verb: &str,
    arg_map: serde_json::Map<String, serde_json::Value>,
) {
    let mut publisher = match kannaka_memory::nats::ProbingPublisher::connect(nats_url) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("NATS connect failed at {nats_url}: {e}");
            process::exit(1);
        }
    };
    let msg_id = uuid::Uuid::new_v4().to_string();
    let ts = chrono::Utc::now().to_rfc3339();
    let payload_obj = serde_json::json!({
        "msg_id": msg_id,
        "from": from,
        "to": to,
        "verb": verb,
        "args": arg_map,
        "ts": ts,
    });
    let payload_bytes = serde_json::to_vec(&payload_obj).unwrap_or_default();
    let directed_subject = format!("KANNAKA.inbox.{to}");
    let probe =
        match publisher.publish_probing(&directed_subject, &payload_bytes, DELIVERY_PROBE_WINDOW) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("publish {directed_subject}: {e}");
                process::exit(1);
            }
        };
    let report = send_report(to, &msg_id, &probe);
    // Audit fan-out (so tail watchers see the outbound — and whether it
    // actually reached anyone).
    let audit = serde_json::json!({
        "ts": ts,
        "phase": "sent",
        "delivery": report.delivery,
        "msg_id": msg_id,
        "from": from,
        "to": to,
        "verb": verb,
        "args": payload_obj["args"],
    });
    let audit_bytes = serde_json::to_vec(&audit).unwrap_or_default();
    let _ = publisher.publish("KANNAKA.inbox.audit", &audit_bytes);

    for line in &report.notice {
        eprintln!("{line}");
    }
    if report.print_payload {
        println!(
            "{}",
            serde_json::to_string(&payload_obj).unwrap_or_default()
        );
    }
    if report.exit_code != 0 {
        process::exit(report.exit_code);
    }
}

/// ----------------------------------------------------------------------
/// `kannaka inbox serve [--agent-id <id>] [--handlers <path>]`
/// ----------------------------------------------------------------------
#[cfg(feature = "nats")]
pub(crate) fn handle_inbox_serve(cfg: &KannakaConfig, args: &[String]) {
    use std::time::Duration;
    const USAGE: &str = "Usage: kannaka inbox serve [--agent-id <id>] [--handlers <path>] [--nats-url URL]";
    let mut agent_id = cfg.agent.id.clone();
    let mut handlers_path = default_handlers_path();
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--agent-id" => {
                agent_id = flag_value(args, i, "--agent-id", USAGE).to_string();
                i += 2;
            }
            "--handlers" => {
                handlers_path = PathBuf::from(flag_value(args, i, "--handlers", USAGE));
                i += 2;
            }
            "--nats-url" => { let _ = flag_value(args, i, "--nats-url", USAGE); i += 2; }
            other => { warn_unknown_flag("inbox serve", other); i += 1; }
        }
    }
    let nats_url = resolve_nats_url(args, 0, &cfg.swarm.nats_url);
    eprintln!("[inbox serve] agent_id={agent_id}");
    eprintln!("[inbox serve] handlers={}", handlers_path.display());

    // Load handlers once at startup. A SIGHUP-driven reload would be a
    // natural follow-up but isn't needed for the prototype.
    let handlers = match load_handlers(&handlers_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("[inbox serve] no handlers loaded: {e}");
            eprintln!("[inbox serve] running in observe-only mode (every verb -> unknown_verb)");
            HandlerTable::new()
        }
    };
    eprintln!("[inbox serve] {} verbs registered: {}", handlers.len(), handlers.keys().cloned().collect::<Vec<_>>().join(", "));

    let transport = match kannaka_memory::nats::SwarmTransport::connect(&nats_url) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("[inbox serve] NATS connect failed: {e}");
            process::exit(1);
        }
    };
    let subject = format!("KANNAKA.inbox.{agent_id}");
    let mut sub = match transport.subscribe(&subject) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[inbox serve] subscribe {subject}: {e}");
            process::exit(1);
        }
    };
    let _ = sub.set_timeout(Some(Duration::from_secs(5)));
    eprintln!("[inbox serve] subscribed to {subject}");
    eprintln!("[inbox serve] press Ctrl+C to stop");

    // Skill-registry announcement subject. Any subscriber to
    // KANNAKA.skills.* sees what verbs this agent accepts. We publish
    // once on startup and again every SKILL_ANNOUNCE_SEC so a fresh
    // listener (e.g. the radio's nats-client) catches us within the
    // ttl window.
    const SKILL_ANNOUNCE_SEC: u64 = 60;
    let skills_subject = format!("KANNAKA.skills.{agent_id}");
    let publish_skills = || {
        let verbs: Vec<serde_json::Value> = handlers
            .iter()
            .map(|(name, spec)| {
                serde_json::json!({
                    "name": name,
                    "required_args": spec.required_args,
                    "timeout_secs": spec.timeout_secs,
                    "description": spec.description,
                })
            })
            .collect();
        let payload = serde_json::json!({
            "agent_id": agent_id,
            "verbs": verbs,
            "announced_at": chrono::Utc::now().to_rfc3339(),
            "ttl_sec": SKILL_ANNOUNCE_SEC + 30, // small grace beyond next re-publish
        });
        let bytes = serde_json::to_vec(&payload).unwrap_or_default();
        if let Err(e) = transport.publish(&skills_subject, &bytes) {
            eprintln!("[inbox serve] skill announce publish failed: {e}");
        }
    };
    publish_skills();
    let mut last_announce = std::time::Instant::now();

    loop {
        // Re-announce skills on the schedule. Cheap publish; lets a
        // newly-arrived subscriber discover us within one window.
        if last_announce.elapsed() >= Duration::from_secs(SKILL_ANNOUNCE_SEC) {
            publish_skills();
            last_announce = std::time::Instant::now();
        }
        let msg = match sub.next_event() {
            SubEvent::Msg(m) => m,
            // Read timeout — healthy idle; loop back for the announce check.
            SubEvent::Timeout => continue,
            // Pre-fix a closed socket spun this loop at 100% CPU forever
            // while the daemon looked alive. Exit nonzero so systemd
            // Restart=on-failure brings us back with a fresh connection.
            SubEvent::Closed => {
                eprintln!("[inbox serve] subscription {subject} closed — exiting for restart");
                process::exit(1);
            }
        };
        let parsed: serde_json::Value = match serde_json::from_slice(&msg.payload) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[inbox serve] payload not JSON ({e}) — drop");
                continue;
            }
        };
        let from = parsed.get("from").and_then(|v| v.as_str()).unwrap_or("?").to_string();
        let verb = parsed.get("verb").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let msg_id = parsed.get("msg_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let reply_to = parsed.get("reply_to").and_then(|v| v.as_str()).map(String::from);
        let args_obj = parsed
            .get("args")
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        let ts_recv = chrono::Utc::now().to_rfc3339();

        let (status, response) = run_one(&handlers, &agent_id, &from, &verb, &args_obj);
        eprintln!("[inbox serve] from={from} verb={verb} status={status}");

        let envelope = serde_json::json!({
            "ts": ts_recv,
            "phase": "received",
            "msg_id": msg_id,
            "from": from,
            "to": agent_id,
            "verb": verb,
            "args": args_obj,
            "status": status,
            "response": response,
        });
        let envelope_bytes = serde_json::to_vec(&envelope).unwrap_or_default();
        let _ = transport.publish("KANNAKA.inbox.audit", &envelope_bytes);
        // Direct reply to the sender if they asked. Same envelope as the
        // audit fan-out — sender's --wait loop expects this shape.
        //
        // Validate before publishing: the sender's --wait loop only ever
        // subscribes to KANNAKA.inbox.reply.<msg_id>, so any other
        // reply_to is forged (or a confused client) and would let an
        // inbound message use this daemon to publish handler output onto
        // arbitrary subjects.
        if let Some(rt) = reply_to {
            if rt.starts_with(REPLY_SUBJECT_PREFIX)
                && rt.len() > REPLY_SUBJECT_PREFIX.len()
                && !rt[REPLY_SUBJECT_PREFIX.len()..].contains(['*', '>', ' '])
            {
                let _ = transport.publish(&rt, &envelope_bytes);
            } else {
                eprintln!(
                    "[inbox serve] rejecting reply_to '{rt}' from {from} — replies only go to {REPLY_SUBJECT_PREFIX}<msg_id>"
                );
            }
        }
    }
}

#[cfg(feature = "nats")]
fn run_one(
    handlers: &HandlerTable,
    _agent_id: &str,
    from: &str,
    verb: &str,
    args: &serde_json::Map<String, serde_json::Value>,
) -> (&'static str, String) {
    let spec = match handlers.get(verb) {
        Some(s) => s,
        None => return ("unknown_verb", format!("no handler registered for verb '{verb}'")),
    };
    for required in &spec.required_args {
        if !args.contains_key(required) {
            return ("arg_missing", format!("required arg '{required}' not provided"));
        }
    }
    let rendered = render_cmd(&spec.cmd_template, from, args);
    // Wrap with /usr/bin/timeout so a runaway handler can't pin the
    // serve loop. The kill-after limit is +5s past the soft timeout,
    // chosen so a well-behaved cmd that respects its own timeout
    // exits cleanly first. Falls back to no-timeout on platforms
    // without /usr/bin/timeout.
    let wrapped = if std::path::Path::new("/usr/bin/timeout").exists() {
        format!("/usr/bin/timeout --kill-after={kill}s {soft}s sh -c {q}",
            kill = spec.timeout_secs + 5,
            soft = spec.timeout_secs,
            q = shell_quote(&rendered),
        )
    } else {
        rendered.clone()
    };
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(&wrapped)
        .output();
    match output {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            if out.status.success() {
                ("ok", if stdout.is_empty() { stderr } else { stdout })
            } else {
                ("handler_failed", format!("exit={:?}; stderr={}", out.status.code(), stderr))
            }
        }
        Err(e) => ("spawn_failed", e.to_string()),
    }
}

/// ----------------------------------------------------------------------
/// `kannaka inbox tail [--agent-id <id>]`
/// ----------------------------------------------------------------------
#[cfg(feature = "nats")]
pub(crate) fn handle_inbox_tail(cfg: &KannakaConfig, args: &[String]) {
    use std::time::Duration;
    const USAGE: &str = "Usage: kannaka inbox tail [--agent-id <id>] [--nats-url URL]";
    let mut filter_agent: Option<String> = None;
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--agent-id" => {
                filter_agent = Some(flag_value(args, i, "--agent-id", USAGE).to_string());
                i += 2;
            }
            "--nats-url" => { let _ = flag_value(args, i, "--nats-url", USAGE); i += 2; }
            other => { warn_unknown_flag("inbox tail", other); i += 1; }
        }
    }
    let nats_url = resolve_nats_url(args, 0, &cfg.swarm.nats_url);
    let transport = match kannaka_memory::nats::SwarmTransport::connect(&nats_url) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("NATS connect failed: {e}");
            process::exit(1);
        }
    };
    let mut sub = match transport.subscribe("KANNAKA.inbox.audit") {
        Ok(s) => s,
        Err(e) => {
            eprintln!("subscribe KANNAKA.inbox.audit: {e}");
            process::exit(1);
        }
    };
    let _ = sub.set_timeout(Some(Duration::from_secs(5)));
    eprintln!("[inbox tail] watching KANNAKA.inbox.audit{}", filter_agent.as_ref().map(|a| format!(" (filter: {a})")).unwrap_or_default());

    loop {
        let msg = match sub.next_event() {
            SubEvent::Msg(m) => m,
            SubEvent::Timeout => continue,
            SubEvent::Closed => {
                eprintln!("[inbox tail] subscription closed — exiting for restart");
                process::exit(1);
            }
        };
        let parsed: serde_json::Value = match serde_json::from_slice(&msg.payload) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if let Some(want) = &filter_agent {
            let from = parsed.get("from").and_then(|v| v.as_str()).unwrap_or("");
            let to = parsed.get("to").and_then(|v| v.as_str()).unwrap_or("");
            if from != want && to != want {
                continue;
            }
        }
        // One line per event, NDJSON-friendly so consumers can pipe.
        println!("{}", serde_json::to_string(&parsed).unwrap_or_default());
    }
}

// ── No-feature stubs ─────────────────────────────────────────────────
#[cfg(not(feature = "nats"))]
pub(crate) fn handle_inbox_send(_cfg: &KannakaConfig, _args: &[String]) {
    eprintln!("inbox send requires the `nats` feature");
    process::exit(1);
}

#[cfg(not(feature = "nats"))]
pub(crate) fn handle_inbox_serve(_cfg: &KannakaConfig, _args: &[String]) {
    eprintln!("inbox serve requires the `nats` feature");
    process::exit(1);
}

#[cfg(not(feature = "nats"))]
pub(crate) fn handle_inbox_tail(_cfg: &KannakaConfig, _args: &[String]) {
    eprintln!("inbox tail requires the `nats` feature");
    process::exit(1);
}

#[cfg(all(test, feature = "nats"))]
mod tests {
    use super::*;

    #[test]
    fn no_listener_is_reported_as_not_delivered() {
        // The 2026-09-26 incident: 0xSCADA-QE -> SpaceChild, which runs swarm
        // join + swarm serve but not inbox serve. The broker answers 503.
        let r = send_report("SpaceChild", "4d857926", &ListenerProbe::NoListener);
        assert_eq!(r.exit_code, EXIT_NOT_DELIVERED);
        assert_ne!(r.exit_code, 0);
        assert_eq!(r.delivery, "no_listener");
        assert!(
            !r.print_payload,
            "a dropped message must not print the success JSON"
        );
        let all = r.notice.join("\n");
        assert!(
            all.contains("NOT DELIVERED: no live inbox listener for 'SpaceChild'"),
            "{all}"
        );
        assert!(all.contains("msg 4d857926 was dropped"), "{all}");
        assert!(all.contains("kannaka inbox serve"), "{all}");
        assert!(
            all.contains("KANNAKA_MAIL_V2"),
            "must point at the durable lane: {all}"
        );
    }

    #[test]
    fn listener_is_success_but_says_receipt_not_handling() {
        let r = send_report("kannaka", "m1", &ListenerProbe::Listening);
        assert_eq!(r.exit_code, 0);
        assert_eq!(r.delivery, "listener");
        assert!(r.print_payload);
        let all = r.notice.join("\n");
        assert!(
            all.contains("delivered msg m1 to a live subscriber on KANNAKA.inbox.kannaka"),
            "{all}"
        );
        assert!(all.contains("--wait"), "{all}");
    }

    #[test]
    fn unconfirmed_warns_and_never_claims_delivery() {
        let r = send_report("x", "m2", &ListenerProbe::Unconfirmed("old server".into()));
        assert_eq!(r.exit_code, 0);
        assert_eq!(r.delivery, "unconfirmed");
        assert!(r.print_payload, "published, so keep the historical stdout");
        let all = r.notice.join("\n");
        assert!(all.contains("UNCONFIRMED: old server"), "{all}");
        assert!(all.contains("not stored"), "{all}");
        assert!(!all.contains("delivered msg"), "{all}");
    }

    #[test]
    fn denied_publish_fails() {
        let r = send_report(
            "x",
            "m3",
            &ListenerProbe::Denied("broker refused publish".into()),
        );
        assert_eq!(r.exit_code, 1);
        assert!(!r.print_payload);
        assert!(r.notice[0].contains("NOT DELIVERED: broker refused publish"));
    }
}
