//! `kannaka ask` — one-shot agent query (local) and `--remote` routing
//! to peer agents over NATS (ADR-0026 Phase 1).
//!
//! Extracted from `bin/kannaka.rs` in v0.3.27 following the pattern
//! documented in `handlers/substrate.rs`.

use std::process;

use super::{compact_input, data_dir, flag_value, parse_flag_value, KannakaConfig};

const ASK_USAGE: &str = "Usage: kannaka ask [--session <id>] [--quiet-tools] [--no-tools] \
[--no-recall|--full-recall] [--recall-query \"text\"] [--remote <agent_id|broadcast>] \
[--remote-timeout <seconds>] [--nats-url <url>] \"your question\"\n\
\n\
--remote carries the recall mode to the peer (#746): the default attention beam, \
--no-recall and --full-recall are all honoured. The peer does NOT run the tool loop, \
so --full-recall warns. --session is never carried: sessions live on the peer disk, \
so honouring it would attach you to its conversation, not yours. A peer too old to \
report a mode is flagged after it answers.";

/// What the remote path can and cannot honour about the requested ask mode
/// (#746).
pub(crate) struct RemoteModeVerdict {
    /// Set when the requested mode is not merely degraded but INVERTED, so
    /// proceeding would do the opposite of what was asked.
    pub fatal: Option<&'static str>,
    pub warnings: Vec<&'static str>,
}

/// Decide what to say about ask-mode flags combined with `--remote`.
///
/// The remote request carries only `{text, recall_query, no_tools}` and the
/// peer always runs `ask_notools_ex` (full recall, no tool loop). Pre-#746 the
/// other flags were silently dropped, so the CLI reported a mode it had
/// discarded. Pure so the policy is testable — the caller only prints and exits.
pub(crate) fn remote_mode_verdict(
    no_recall: bool,
    full_recall: bool,
    no_tools: bool,
    has_session: bool,
) -> RemoteModeVerdict {
    let mut warnings = Vec::new();
    // `--full-recall --no-tools` is exactly what the peer does, so warning
    // there would be noise on a correct invocation.
    if full_recall && !no_tools {
        warnings.push(
            "--full-recall's tool loop is not run over --remote: the peer's loop exposes remember/dream and its read-only mode blocks only the persist, so an in-RAM write would poison the medium it answers everyone from. The full recall itself IS honoured (#746).",
        );
    }
    if has_session {
        warnings.push(
            "--session is not carried over --remote; the peer answers with no conversation continuity (#746).",
        );
    }
    // #746 second half: `--no-recall` is now CARRIED to the peer and honoured,
    // so it is no longer an inversion and no longer fatal. It was only ever a
    // hard error because the peer silently did the opposite; now it does what
    // was asked. A peer too old to understand the mode is caught after the
    // fact by `mode_echo_warning`, which is the only place that can know.
    let _ = no_recall;
    RemoteModeVerdict { fatal: None, warnings }
}

/// What a peer's reply actually said.
///
/// The wire contract has had an error channel all along — `_handle_serve_msg`
/// answers `{from, error}` for bad JSON, for empty text, and for any failure
/// out of the ask itself, and `_process_work_msg` does the same for worker
/// failures. The client read only `text` and defaulted a missing one to
/// `(no text)`, so every one of those turned into a successful-looking answer
/// whose body was a placeholder, on exit code 0.
///
/// That is the worst available outcome. Transport failures already fail loudly;
/// it was specifically the peer's *application* failures — the ones carrying a
/// diagnosis — that were laundered into "here is your answer".
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PeerReply<'a> {
    Answer(&'a str),
    /// The peer reported a failure. The string is its own message.
    Failed(&'a str),
    /// Neither a usable `text` nor an `error`: a reply we cannot interpret.
    /// Not an answer, so it must not be printed as one.
    Unintelligible,
}

/// Classify one parsed reply. Pure, so the contract is testable without a
/// broker — which is why the defect survived: nothing that ran in CI ever
/// looked at a reply body.
pub(crate) fn interpret_reply(parsed: &serde_json::Value) -> PeerReply<'_> {
    // `error` wins over `text`. A reply carrying both is a peer that failed
    // and said something anyway; the failure is the load-bearing half.
    if let Some(e) = parsed.get("error").and_then(|v| v.as_str()) {
        if !e.trim().is_empty() {
            return PeerReply::Failed(e);
        }
    }
    match parsed.get("text").and_then(|v| v.as_str()) {
        // An empty string IS an answer the peer chose to give. Only a missing
        // or non-string `text` is unintelligible.
        Some(t) => PeerReply::Answer(t),
        None => PeerReply::Unintelligible,
    }
}

#[cfg(test)]
mod tests {
    use super::{interpret_reply, mode_echo_warning, remote_mode_verdict, PeerReply};
    use kannaka_memory::agent::RemoteAskMode;

    // ---- #820: a peer's failure must not read as an answer -----------------

    #[test]
    fn an_error_only_reply_is_a_failure_not_a_blank_answer() {
        let r = serde_json::json!({ "from": "peer-a", "error": "upstream model timeout" });
        assert_eq!(interpret_reply(&r), PeerReply::Failed("upstream model timeout"));
    }

    #[test]
    fn the_serve_side_error_shapes_are_all_recognised() {
        // These are the literal shapes _handle_serve_msg and _process_work_msg
        // put on the wire. If one of them ever stopped being classified as a
        // failure it would silently become "(no text)" again.
        for body in [
            serde_json::json!({ "from": "p", "error": "bad json: expected value" }),
            serde_json::json!({ "from": "p", "error": "empty text" }),
            serde_json::json!({ "from": "p", "error": "model unavailable", "mode_used": "full_recall_no_tools" }),
        ] {
            assert!(
                matches!(interpret_reply(&body), PeerReply::Failed(_)),
                "not classified as a failure: {body}"
            );
        }
    }

    #[test]
    fn an_error_alongside_text_still_reports_the_failure() {
        let r = serde_json::json!({ "from": "p", "error": "tool loop aborted", "text": "partial" });
        assert_eq!(interpret_reply(&r), PeerReply::Failed("tool loop aborted"));
    }

    #[test]
    fn a_real_answer_is_still_an_answer() {
        let r = serde_json::json!({ "from": "p", "text": "the answer" });
        assert_eq!(interpret_reply(&r), PeerReply::Answer("the answer"));
    }

    #[test]
    fn an_empty_string_answer_is_an_answer_but_a_missing_one_is_not() {
        // The distinction the old `.unwrap_or("(no text)")` erased: a peer that
        // deliberately answered with nothing is not the same event as a peer
        // that sent us something we cannot read.
        assert_eq!(
            interpret_reply(&serde_json::json!({ "from": "p", "text": "" })),
            PeerReply::Answer("")
        );
        assert_eq!(
            interpret_reply(&serde_json::json!({ "from": "p" })),
            PeerReply::Unintelligible
        );
        assert_eq!(
            interpret_reply(&serde_json::json!({ "from": "p", "text": 42 })),
            PeerReply::Unintelligible
        );
    }

    #[test]
    fn an_empty_error_string_does_not_manufacture_a_failure() {
        let r = serde_json::json!({ "from": "p", "error": "  ", "text": "fine" });
        assert_eq!(interpret_reply(&r), PeerReply::Answer("fine"));
    }

    /// #746 second half: `--no-recall` is now CARRIED and honoured, so the
    /// hard error from the first half is gone. It was only fatal because the
    /// peer silently did the opposite.
    #[test]
    fn no_recall_over_remote_is_no_longer_fatal() {
        let v = remote_mode_verdict(true, false, false, false);
        assert!(
            v.fatal.is_none(),
            "--no-recall is carried to the peer now, so it must not be refused"
        );
    }

    // ---- the four client/server vintage combinations (#746) ----------------

    /// NEW client → NEW server, mode honoured: nothing to say.
    #[test]
    fn matrix_new_client_new_server_honoured_is_silent() {
        for m in [RemoteAskMode::Attention, RemoteAskMode::NoRecall, RemoteAskMode::FullRecall] {
            assert_eq!(
                mode_echo_warning(m, Some(m.mode_used_name())),
                None,
                "an honoured {m:?} must not warn"
            );
        }
    }

    /// NEW client → OLD server. The old server never saw `mode` and echoes
    /// nothing, so the ABSENCE of the echo is what identifies it. This is the
    /// combination that would otherwise silently mislead the caller.
    #[test]
    fn matrix_new_client_old_server_warns_on_missing_echo() {
        let w = mode_echo_warning(RemoteAskMode::Attention, None)
            .expect("a peer that reports no mode must produce a warning");
        assert!(w.contains("predates"), "{w}");
        assert!(w.contains("attention"), "the warning must name what was requested: {w}");
    }

    /// OLD client → NEW server: the request carries no `mode`, and the server
    /// must resolve that to exactly what every pre-#746 server did. Tested on
    /// the server's parser, which is the side that decides.
    #[test]
    fn matrix_old_client_new_server_falls_back_to_legacy_behaviour() {
        assert_eq!(
            RemoteAskMode::from_wire(None),
            RemoteAskMode::FullRecall,
            "an old client's payload must behave exactly as it always has"
        );
    }

    /// A mode-aware peer that ran something ELSE — e.g. a future server
    /// declining a mode. Both sides must be named or the difference is not
    /// actionable.
    #[test]
    fn matrix_new_client_new_server_mismatch_names_both_sides() {
        let w = mode_echo_warning(RemoteAskMode::Attention, Some("no_recall"))
            .expect("a mode mismatch must warn");
        assert!(w.contains("no_recall"), "must name what the peer ran: {w}");
        assert!(w.contains("attention"), "must name what we asked for: {w}");
    }

    /// `full_recall` echoes `full_recall_no_tools`, so the match must be on the
    /// echoed spelling — not the requested one, or every full-recall ask would
    /// warn spuriously.
    #[test]
    fn full_recall_echo_uses_the_no_tools_spelling() {
        assert_eq!(RemoteAskMode::FullRecall.mode_used_name(), "full_recall_no_tools");
        assert_eq!(
            mode_echo_warning(RemoteAskMode::FullRecall, Some("full_recall_no_tools")),
            None
        );
        assert!(
            mode_echo_warning(RemoteAskMode::FullRecall, Some("full_recall")).is_some(),
            "a peer claiming plain `full_recall` ran something we did not ask for"
        );
    }

    /// `--full-recall --no-tools` is precisely what the peer does — warning on a
    /// correct invocation would train operators to ignore the warnings.
    #[test]
    fn full_recall_with_no_tools_is_silent() {
        let v = remote_mode_verdict(false, true, true, false);
        assert!(v.fatal.is_none());
        assert!(
            v.warnings.is_empty(),
            "the one combination that already matches the peer must not warn: {:?}",
            v.warnings
        );
    }

    /// ...but the tool loop genuinely is unavailable, so that case does warn.
    #[test]
    fn full_recall_with_tools_warns_about_the_tool_loop() {
        let v = remote_mode_verdict(false, true, false, false);
        assert!(v.fatal.is_none());
        assert_eq!(v.warnings.len(), 1);
        assert!(v.warnings[0].contains("tool loop"));
    }

    #[test]
    fn session_over_remote_warns_about_lost_continuity() {
        let v = remote_mode_verdict(false, false, false, true);
        assert!(v.fatal.is_none());
        assert!(v.warnings.iter().any(|w| w.contains("--session")));
    }

    /// The common path must stay quiet — a warning on every remote ask is noise,
    /// and the default's mismatch is the protocol half, not a caller error.
    #[test]
    fn plain_remote_is_quiet() {
        let v = remote_mode_verdict(false, false, false, false);
        assert!(v.fatal.is_none());
        assert!(v.warnings.is_empty());
    }
}

pub(crate) fn handle_ask(
    sys: &mut kannaka_memory::openclaw::KannakaMemorySystem,
    cfg: &KannakaConfig,
    args: &[String],
) {
    // Parse flags: --session <id>, --quiet-tools, --no-tools, --no-recall,
    // --full-recall, --recall-query <text>, --remote <agent_id|broadcast>,
    // --remote-timeout <seconds>, --nats-url <url>
    //
    // Recall mode precedence: --no-recall > --full-recall > attention (default).
    //   attention   — query-aware beam + recall_with_beam. Default. ~3-5s.
    //   --full-recall — scan the full medium with xi-rerank. 60-90s on
    //                   a mature HRM. Use only when attention misses.
    //   --no-recall — skip resonance entirely. ~2-3s. No memory context.
    let mut session: Option<String> = None;
    let mut quiet_tools = false;
    let mut no_tools = false;
    let mut no_recall = false;
    let mut full_recall = false;
    let mut recall_query: Option<String> = None;
    let mut remote: Option<String> = None;
    let mut remote_timeout_secs: u64 = 60;
    let mut parts: Vec<String> = Vec::new();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--session" => { session = Some(flag_value(args, i, "--session", ASK_USAGE).to_string()); i += 2; }
            "--quiet-tools" => { quiet_tools = true; i += 1; }
            "--no-tools" => { no_tools = true; i += 1; }
            "--no-recall" => { no_recall = true; i += 1; }
            "--full-recall" => { full_recall = true; i += 1; }
            "--recall-query" => { recall_query = Some(flag_value(args, i, "--recall-query", ASK_USAGE).to_string()); i += 2; }
            "--remote" => { remote = Some(flag_value(args, i, "--remote", ASK_USAGE).to_string()); i += 2; }
            "--remote-timeout" => {
                remote_timeout_secs = parse_flag_value(args, i, "--remote-timeout", ASK_USAGE);
                i += 2;
            }
            // Consumed here so it never leaks into the prompt; the value is
            // picked up by resolve_nats_url's scan in handle_ask_remote.
            "--nats-url" => { let _ = flag_value(args, i, "--nats-url", ASK_USAGE); i += 2; }
            other if other.starts_with("--") => {
                // A typo'd flag must NOT silently become part of the question.
                eprintln!("ask: unknown flag: {other}");
                eprintln!("{ASK_USAGE}");
                process::exit(2);
            }
            _ => { parts.push(args[i].clone()); i += 1; }
        }
    }
    let prompt = parts.join(" ").trim().to_string();
    if prompt.is_empty() {
        eprintln!("{ASK_USAGE}");
        process::exit(1);
    }

    // --remote: route the question over NATS to a peer (or broadcast) running
    // `kannaka swarm serve`. ADR-0026 Phase 1.
    if let Some(target) = remote {
        // #746: the request payload carries only {text, recall_query, no_tools}
        // and the peer always answers with `ask_notools_ex` (full recall, no
        // tool loop). Recall-mode flags therefore CANNOT be honoured remotely.
        // Pre-fix they were silently dropped, so the CLI claimed a mode it had
        // discarded. Say so instead. (Actually carrying the mode is a protocol
        // change with a version-compat trap — see the issue.)
        let verdict = remote_mode_verdict(no_recall, full_recall, no_tools, session.is_some());
        for w in &verdict.warnings {
            eprintln!("ask: warning: {w}");
        }
        if let Some(fatal) = verdict.fatal {
            eprintln!("ask: {fatal}");
            process::exit(2);
        }
        // Mirror the local precedence (--no-recall > --full-recall > attention)
        // so `--remote` asks the peer for the SAME path the caller would have
        // run locally — which is the whole point of #746.
        let mode = if no_recall {
            kannaka_memory::agent::RemoteAskMode::NoRecall
        } else if full_recall {
            kannaka_memory::agent::RemoteAskMode::FullRecall
        } else {
            kannaka_memory::agent::RemoteAskMode::Attention
        };
        return handle_ask_remote(cfg, args, &target, &prompt, recall_query.as_deref(),
            no_tools, remote_timeout_secs, quiet_tools, mode);
    }

    let result = if no_recall {
        // No memory context — fastest possible round-trip.
        kannaka_memory::agent::ask_no_recall(sys, cfg, &prompt)
    } else if full_recall && no_tools {
        // Explicit slow path, single round-trip (legacy radio caller).
        kannaka_memory::agent::ask_notools_ex(sys, cfg, &prompt, recall_query.as_deref())
    } else if full_recall {
        // Explicit slow path with tool loop — opt-in 60-90s scan.
        match session {
            Some(id) => {
                let path = data_dir().join("sessions").join(format!("{id}.json"));
                kannaka_memory::agent::ask_with_session(sys, cfg, &path, &prompt)
            }
            None => kannaka_memory::agent::ask(sys, cfg, &prompt),
        }
    } else {
        // Default — attention-driven recall: query-aware beam prefilter
        // then full wave resonance against the beam only. Resonance
        // semantics preserved, scan cost is O(beam) not O(medium).
        //
        // The attention path is single-shot (no tool loop) by design —
        // the beam already surfaces the relevant memories, so the model
        // doesn't need to call `recall` itself. `--no-tools` is therefore
        // a no-op here; the path is always tool-free.
        let _ = no_tools;
        match session {
            Some(id) => {
                let path = data_dir().join("sessions").join(format!("{id}.json"));
                kannaka_memory::agent::ask_attention_with_session(sys, cfg, &path, &prompt)
            }
            None => kannaka_memory::agent::ask_attention(sys, cfg, &prompt),
        }
    };

    match result {
        Ok(result) => {
            if !quiet_tools && !result.tool_calls.is_empty() {
                eprintln!("[agent] {} tool call(s):", result.tool_calls.len());
                for tc in &result.tool_calls {
                    let mark = if tc.is_error { "!" } else { "·" };
                    eprintln!("  {mark} {}({})", tc.name, compact_input(&tc.input));
                }
            }
            // Hardening priority #6 — surface the silent-empty path. The
            // 2026-05-02 outage: HRM grew past ~1000 wavefronts, the
            // recall-laden system prompt blew Anthropic's input ceiling,
            // the response had zero Text blocks, and we printed empty
            // stdout + exited 0. Downstream consumers (radio's oration
            // path) couldn't tell that from a successful empty-string
            // generation. Now: surface the empty case explicitly so it
            // can't hide.
            if result.text.is_empty() {
                eprintln!(
                    "agent warning: empty response (no Text content blocks). \
                    Likely causes: bloated HRM (>1000 memories) blew the \
                    input ceiling, model returned only tool_use blocks \
                    that weren't wired, or upstream truncation. \
                    Check `kannaka observe` and consider `kannaka dream`."
                );
                process::exit(2);
            }
            println!("{}", result.text);
            // Best-effort pulse so local asks show up in the swarm-tail /
            // statusline activity feed. Runs AFTER the answer is printed
            // so the user never waits on NATS; failures never affect the
            // exit code.
            publish_ask_activity(cfg, &prompt);
        }
        Err(e) => {
            eprintln!("agent error: {e}");
            process::exit(1);
        }
    }
}

/// Publish a `KANNAKA.activity.<agent_id>` event for a successful local ask.
/// Only attempted when a NATS URL is explicitly configured (config or
/// KANNAKA_NATS_URL env) — never falls back to a hardcoded public host.
/// Failures are at most a single eprintln and never change the exit code.
#[cfg(feature = "nats")]
fn publish_ask_activity(cfg: &KannakaConfig, prompt: &str) {
    let nats_url = if !cfg.swarm.nats_url.is_empty() {
        cfg.swarm.nats_url.clone()
    } else {
        match std::env::var("KANNAKA_NATS_URL") {
            Ok(u) if !u.is_empty() => u,
            _ => return, // not configured — stay quiet
        }
    };
    let display_name = if cfg.agent.display_name.is_empty() {
        cfg.agent.id.clone()
    } else {
        cfg.agent.display_name.clone()
    };
    let preview: String = prompt.chars().take(48).collect();
    let payload = serde_json::json!({
        "agent_id": cfg.agent.id,
        "display_name": display_name,
        "kind": "ask",
        "preview": preview,
        "ts": chrono::Utc::now().timestamp_millis(),
    });
    let subject = format!("KANNAKA.activity.{}", cfg.agent.id);
    match kannaka_memory::nats::SwarmTransport::connect(&nats_url) {
        Ok(t) => {
            if let Err(e) = t.publish(&subject, payload.to_string().as_bytes()) {
                eprintln!("[ask] activity publish failed: {e}");
            }
        }
        Err(_) => {} // best-effort: silent on connect failure
    }
}

#[cfg(not(feature = "nats"))]
fn publish_ask_activity(_: &KannakaConfig, _: &str) {}

// ── ADR-0026 Phase 1: remote ask routing over NATS ─────────────────────────

/// What the peer's `mode_used` echo says about whether our mode was honoured
/// (#746).
///
/// The echo IS the version negotiation: it is discovered per call from the
/// reply itself, so there is no capability registry, no presence-record field
/// and no deploy ordering. Pure so all four client/server vintage combinations
/// are testable without a broker.
///
/// `None` means nothing worth saying. `Some(msg)` is a warning for stderr —
/// never fatal, because the answer itself is still valid and useful.
pub(crate) fn mode_echo_warning(
    requested: kannaka_memory::agent::RemoteAskMode,
    echoed: Option<&str>,
) -> Option<String> {
    match echoed {
        // Pre-#746 peer: it never saw our `mode` and ran its fixed path.
        None => Some(format!(
            "peer reported no mode; it likely predates mode support and ran full recall without tools, not the requested `{}` (#746).",
            requested.wire_name()
        )),
        Some(used) if used == requested.mode_used_name() => None,
        // Mode-aware peer that ran something else — e.g. a future server
        // declining a mode. Name both sides so the difference is actionable.
        Some(used) => Some(format!(
            "peer ran `{used}`, not the requested `{}` (#746).",
            requested.wire_name()
        )),
    }
}

#[cfg(feature = "nats")]
#[allow(clippy::too_many_arguments)]
pub(crate) fn handle_ask_remote(
    cfg: &KannakaConfig,
    args: &[String],
    target: &str,
    prompt: &str,
    recall_query: Option<&str>,
    no_tools: bool,
    timeout_secs: u64,
    quiet_tools: bool,
    mode: kannaka_memory::agent::RemoteAskMode,
) {
    use std::time::Duration;
    // Honors --nats-url > KANNAKA_NATS_URL (folded into cfg at load) >
    // config.toml. No hardcoded public-host fallback.
    let nats_url = super::resolve_nats_url(args, 0, &cfg.swarm.nats_url);
    if nats_url.is_empty() {
        eprintln!("ask --remote: no NATS URL configured — set swarm.nats_url, KANNAKA_NATS_URL, or pass --nats-url");
        process::exit(1);
    }
    let transport = match kannaka_memory::nats::SwarmTransport::connect(&nats_url) {
        Ok(t) => t,
        Err(e) => { eprintln!("Failed to connect to NATS at {nats_url}: {e}"); process::exit(1); }
    };

    // #932: `hops` — a hired ask never hires. When this process is answering a
    // served ask (`swarm serve` sets the cell for the duration of the answer),
    // publishing another ask IS forwarding, and the ceiling applies. When it is
    // not, this ask originates here and goes out at hops = 0.
    //
    // Today no serve path routes onward, so this refusal is dormant by
    // construction — it is here so the router #931 adds lands on top of a
    // ceiling that is already enforced, instead of after one.
    let inbound_hops = kannaka_memory::serve_guard::serving_hops();
    if let Some(h) = inbound_hops {
        if !kannaka_memory::serve_guard::may_forward(h) {
            eprintln!(
                "ask --remote: refusing to forward an ask already at {h} hop(s) (ceiling {}) — a hired ask never hires (#932)",
                kannaka_memory::serve_guard::MAX_HOPS
            );
            process::exit(3);
        }
    }

    // #746: `mode` is additive — a pre-#746 server ignores the field and
    // answers exactly as before, so adding it cannot break an old peer.
    // `hops` is additive the same way: a pre-#932 server never reads it, and a
    // pre-#932 client omits it, which reads as 0.
    let request = serde_json::json!({
        "from": cfg.agent.id,
        "text": prompt,
        "recall_query": recall_query,
        "no_tools": no_tools,
        "mode": mode.wire_name(),
        "hops": kannaka_memory::serve_guard::outbound_hops(inbound_hops),
    });
    let payload = match serde_json::to_vec(&request) {
        Ok(b) => b,
        Err(e) => { eprintln!("serialize: {e}"); process::exit(1); }
    };

    let timeout = Duration::from_secs(timeout_secs);
    if target == "broadcast" {
        let subject = "KANNAKA.ask.broadcast";
        eprintln!("[ask --remote broadcast] published; collecting replies for {timeout_secs}s...");
        match transport.request_many(subject, &payload, timeout) {
            Ok(replies) => {
                if replies.is_empty() {
                    eprintln!("(no replies within {timeout_secs}s)");
                    process::exit(2);
                }
                let mut answered = 0usize;
                for (i, reply) in replies.iter().enumerate() {
                    let parsed: serde_json::Value = serde_json::from_slice(reply)
                        .unwrap_or_else(|_| serde_json::json!({"raw": String::from_utf8_lossy(reply)}));
                    let from = parsed.get("from").and_then(|v| v.as_str()).unwrap_or("?");
                    // #820: a peer that reported a failure is not a peer that
                    // answered. Its diagnosis goes to stderr, where a failure
                    // belongs, and it does not count toward having been
                    // answered at all.
                    let text = match interpret_reply(&parsed) {
                        PeerReply::Answer(t) => {
                            answered += 1;
                            t
                        }
                        PeerReply::Failed(e) => {
                            eprintln!(
                                "ask: peer {} failed: {}",
                                kannaka_memory::sanitize_display(from),
                                kannaka_memory::sanitize_display(e)
                            );
                            continue;
                        }
                        PeerReply::Unintelligible => {
                            eprintln!(
                                "ask: peer {} sent a reply with neither text nor error",
                                kannaka_memory::sanitize_display(from)
                            );
                            continue;
                        }
                    };
                    // SECURITY (increment-0): reply came from a peer over the
                    // open swarm — sanitize the id + body before printing and
                    // flag replies from ids not on the trusted allowlist.
                    let from_s = kannaka_memory::sanitize_display(from);
                    let trusted = from == cfg.agent.id
                        || kannaka_memory::agent_matches_allowlist(from, &cfg.swarm_trust.trusted_agents);
                    let mark = if trusted { "" } else { " (unverified)" };
                    if !quiet_tools {
                        eprintln!("─── reply {} from {}{} ───", i + 1, from_s, mark);
                    }
                    if let Some(w) =
                        mode_echo_warning(mode, parsed.get("mode_used").and_then(|v| v.as_str()))
                    {
                        eprintln!("ask: warning: {from_s}: {w}");
                    }
                    println!("{}", kannaka_memory::sanitize_display(text));
                    if !quiet_tools && i + 1 < replies.len() { println!(); }
                }
                // Replies arrived, but not one of them was an answer. That is a
                // different fact from "no replies" (exit 2) and from a
                // transport failure (exit 1), and it must not be exit 0.
                if answered == 0 {
                    eprintln!(
                        "ask: {} peer(s) replied, none with an answer",
                        replies.len()
                    );
                    process::exit(3);
                }
            }
            Err(e) => { eprintln!("request_many: {e}"); process::exit(1); }
        }
    } else {
        let subject = format!("KANNAKA.ask.{target}");
        match transport.request_one(&subject, &payload, timeout) {
            Ok(reply) => {
                let parsed: serde_json::Value = serde_json::from_slice(&reply)
                    .unwrap_or_else(|_| serde_json::json!({"raw": String::from_utf8_lossy(&reply)}));
                if let Some(w) =
                    mode_echo_warning(mode, parsed.get("mode_used").and_then(|v| v.as_str()))
                {
                    eprintln!("ask: warning: {w}");
                }
                // #820: on a directed ask there is exactly one peer, so its
                // failure is the whole outcome. Report it on stderr and exit
                // non-zero — printing the peer's diagnosis to stdout as though
                // it were the answer is how a caller ends up storing "upstream
                // model timeout" as a result.
                let text = match interpret_reply(&parsed) {
                    PeerReply::Answer(t) => t,
                    PeerReply::Failed(e) => {
                        eprintln!(
                            "ask: peer {} failed: {}",
                            kannaka_memory::sanitize_display(target),
                            kannaka_memory::sanitize_display(e)
                        );
                        process::exit(3);
                    }
                    PeerReply::Unintelligible => {
                        eprintln!(
                            "ask: peer {} sent a reply with neither text nor error",
                            kannaka_memory::sanitize_display(target)
                        );
                        process::exit(3);
                    }
                };
                // SECURITY (increment-0): wire-sourced reply body — never print raw.
                println!("{}", kannaka_memory::sanitize_display(text));
            }
            Err(e) => { eprintln!("request_one: {e}"); process::exit(1); }
        }
    }
}

#[cfg(not(feature = "nats"))]
#[allow(clippy::too_many_arguments)]
pub(crate) fn handle_ask_remote(_: &KannakaConfig, _: &[String], _: &str, _: &str, _: Option<&str>, _: bool, _: u64, _: bool, _: kannaka_memory::agent::RemoteAskMode) {
    eprintln!("--remote requires the 'nats' feature");
    process::exit(1);
}
