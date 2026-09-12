//! `kannaka swarm` handlers — peer discovery, exemplar broadcast,
//! cross-agent absorb, work-queue serve/worker, and the directed/broadcast
//! `kannaka ask` serve loop (ADR-0026).
//!
//! Extracted from `bin/kannaka.rs` in v0.3.29 following the pattern
//! documented in `handlers/substrate.rs`. The largest single extraction
//! this session — moves ~830 lines including the 8 public swarm
//! handlers and the private helpers (_handle_serve_msg, _process_work_msg,
//! _neighbors_reply) that are only called from within this module.

use std::process;

use super::{data_dir, resolve_nats_url, store_dir, KannakaConfig};
#[cfg(feature = "nats")]
use super::{flag_value, parse_flag_value};

#[cfg(feature = "nats")]
use kannaka_memory::nats::SubEvent;

/// Stderr warning for unrecognized `--flags` in non-text handlers. Unknown
/// flags used to be silently swallowed by `_ => i += 1` arms, so typos like
/// `--thresold 0.5` vanished without a trace.
#[cfg(feature = "nats")]
fn warn_unknown_flag(ctx: &str, arg: &str) {
    if arg.starts_with("--") {
        eprintln!("[{ctx}] ignoring unknown flag: {arg}");
    }
}

#[cfg(feature = "nats")]
pub(crate) fn handle_swarm_serve(
    sys: &mut kannaka_memory::openclaw::KannakaMemorySystem,
    cfg: &KannakaConfig,
    args: &[String],
) {
    use std::time::Duration;
    const USAGE: &str =
        "Usage: kannaka swarm serve [--threshold 0.4] [--nats-url URL] [--agent-id ID]
         Requires AUTHENTICATED NATS credentials (~/.kannaka-nats.env): the public
         anon identity may not subscribe to KANNAKA.ask.* or KANNAKA.recall.* (#562).";
    // Single-writer policy: the serve daemon is a long-running READER. It
    // observes/recalls (which mutate the medium in RAM) but must never
    // flush over the sole writer's .hrm. Enforce read-only here instead of
    // trusting the operator to export KANNAKA_READONLY.
    std::env::set_var("KANNAKA_READONLY", "1");
    if let Some(hrm) = sys
        .engine
        .store
        .as_any_mut()
        .downcast_mut::<kannaka_memory::hrm_store::HrmStore>()
    {
        hrm.set_readonly(true);
    }
    eprintln!("[swarm serve] read-only mode enforced (single-writer policy)");

    // Parse: kannaka swarm serve [--threshold 0.4] [--nats-url ...] [--agent-id ...]
    let mut threshold: f32 = 0.4;
    let mut agent_id_override: Option<String> = None;
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--threshold" => {
                threshold = parse_flag_value(args, i, "--threshold", USAGE);
                i += 2;
            }
            "--nats-url" => {
                let _ = flag_value(args, i, "--nats-url", USAGE);
                i += 2;
            }
            "--agent-id" => {
                agent_id_override = Some(flag_value(args, i, "--agent-id", USAGE).to_string());
                i += 2;
            }
            other => {
                warn_unknown_flag("swarm serve", other);
                i += 1;
            }
        }
    }
    let nats_url = resolve_nats_url(args, 0, &cfg.swarm.nats_url);
    let transport = match kannaka_memory::nats::SwarmTransport::connect(&nats_url) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("Failed to connect to NATS at {nats_url}: {e}");
            process::exit(1);
        }
    };
    let agent_id = agent_id_override.unwrap_or_else(|| cfg.agent.id.clone());
    let directed = format!("KANNAKA.ask.{agent_id}");

    // Capability gate (see the subscription block below): decided BEFORE the
    // banner so the banner cannot announce subjects this node then declines to
    // subscribe. Advertising a capability you do not have is the exact failure
    // this gate exists to remove; printing it would reintroduce it in the log.
    let llm_ok = kannaka_memory::agent::llm_available(cfg);

    eprintln!("[swarm serve] agent_id={agent_id}");
    if llm_ok {
        eprintln!(
            "[swarm serve] subscribing to {directed} and KANNAKA.ask.broadcast"
        );
        eprintln!(
            "[swarm serve] broadcast resonance threshold: {threshold:.2}"
        );
    } else {
        eprintln!(
            "[swarm serve] recall-only mode (no LLM provider configured) — not serving KANNAKA.ask.*"
        );
    }
    // #932: `KANNAKA.ask.broadcast` is publishable by the anonymous NATS
    // identity, so everything below this line is money a stranger can spend.
    //
    // ADR-0059 §3 says an inbound ask is "pinned to local providers". Taken
    // literally that would take `kannaka-prime` — the public `ask_kannaka`
    // product — off the air, because prime answers from a REMOTE gateway
    // (ninja-portal.com/v1) on a virtual key already capped at $25/30d. The
    // invariant that actually matters is the ceiling, not the locality:
    //
    //   a served ask must never spend without a ceiling, and must never let
    //   the caller choose what it costs.
    //
    // So: warn loudly when the posture is unbounded, refuse only on request,
    // and rate-limit per requester unconditionally.
    let posture = kannaka_memory::serve_guard::spend_posture(
        &cfg.llm,
        kannaka_memory::serve_guard::api_key_present(&cfg.llm),
    );
    if llm_ok {
        match &posture {
            kannaka_memory::serve_guard::SpendPosture::Free { why } => {
                eprintln!("[swarm serve] spend: free ({why})");
            }
            kannaka_memory::serve_guard::SpendPosture::Capped { how } => {
                eprintln!("[swarm serve] spend: bounded — {how}");
                eprintln!(
                    "[swarm serve] NOTE: max_usd_per_day is DECLARED, not enforced by this build (#931 adds per-call cost accounting); the rate limit below is what bounds spend today"
                );
            }
            // A gateway the operator interposed is evidence of a budget — it is
            // where virtual keys and caps live — so this says what it knows and
            // does not shout. `kannaka-prime` has exactly this shape, and
            // shouting at the one node everybody watches, about a key that IS
            // capped, is how a banner stops being read.
            kannaka_memory::serve_guard::SpendPosture::UndeclaredGateway { base_url } => {
                eprintln!(
                    "[swarm serve] spend: this node can spend and has declared no ceiling here; it reaches its provider through {base_url}. If that key is capped upstream, record it with `[llm] externally_capped = true`; otherwise set `[llm] max_usd_per_day`."
                );
                if kannaka_memory::serve_guard::refuse_unbounded_requested() {
                    eprintln!(
                        "[swarm serve] KANNAKA_SERVE_REFUSE_UNBOUNDED=1 — refusing to serve with no declared ceiling"
                    );
                    process::exit(1);
                }
            }
            kannaka_memory::serve_guard::SpendPosture::Unbounded => {
                eprintln!("[swarm serve] ============================================================");
                eprintln!("[swarm serve] WARNING: serving KANNAKA.ask.broadcast with a KEYED provider");
                eprintln!("[swarm serve] WARNING: against the vendor's own API, and NO declared ceiling");
                eprintln!("[swarm serve] WARNING: anywhere. The anonymous NATS identity may publish");
                eprintln!("[swarm serve] WARNING: there, so this key is spendable by anyone on the bus");
                eprintln!("[swarm serve] WARNING: (#932). Declare a ceiling in config.toml:");
                eprintln!("[swarm serve] WARNING:   [llm] max_usd_per_day = 2.00");
                eprintln!("[swarm serve] WARNING:   [llm] externally_capped = true   # if capped upstream");
                eprintln!("[swarm serve] WARNING: or set KANNAKA_SERVE_REFUSE_UNBOUNDED=1 to refuse to start.");
                eprintln!("[swarm serve] ============================================================");
                if kannaka_memory::serve_guard::refuse_unbounded_requested() {
                    eprintln!(
                        "[swarm serve] KANNAKA_SERVE_REFUSE_UNBOUNDED=1 — refusing to serve unbounded"
                    );
                    process::exit(1);
                }
            }
        }
    }

    // Per-requester rate limit — the actual abuse control. On by default; the
    // numbers are printed so the knob is discoverable from the log alone.
    let mut rate_limit = kannaka_memory::serve_guard::ServeRateLimiter::from_env();
    if llm_ok {
        eprintln!(
            "[swarm serve] rate limit: {}/requester/hour, {}/hour total (KANNAKA_SERVE_ASKS_PER_HOUR, KANNAKA_SERVE_ASKS_PER_HOUR_TOTAL)",
            rate_limit.per_requester(),
            rate_limit.global_limit()
        );
        eprintln!(
            "[swarm serve] NOTE: NATS carries no publisher identity on a message, so a requester is keyed by the envelope's `from` or the reply-inbox prefix — both caller-chosen. The per-hour TOTAL is the ceiling that holds against a caller who rotates."
        );
    }

    eprintln!("[swarm serve] press Ctrl+C to stop");

    // Single subscription per subject; in v1 we run them sequentially via
    // a switch, accepting that a busy listener processes one ask at a time.
    // Future: dedicated reader thread + channel per subject.
    //
    // ADR-0042 Phase 4: every serve subscription joins a per-identity queue
    // group, so N `swarm serve` instances answering for the same agent_id
    // (e.g. oracle1 + oracle3 both serving kannaka-prime) each receive
    // exactly ONE copy of a request — redundant reflex, no duplicate replies.
    // With a single instance the semantics are identical to a plain SUB.
    // The group must be per-identity (not global): ask.broadcast is a shared
    // subject, and a global group would split broadcasts BETWEEN identities.
    let serve_group = format!("serve_{agent_id}");

    // Capability gate: only join the ask queue group if this node can actually
    // answer. O1 and O3 both served `kannaka-prime` in `serve_kannaka-prime`,
    // but only O1 had an LLM provider — NATS round-robins a queue group, so
    // roughly half of all asks landed on O3 and came back "no LLM provider
    // configured". Declining the subscription removes the keyless node from
    // the rotation entirely, which is the durable fix; a client-side retry can
    // only paper over it.
    //
    // Recall and neighbors need no LLM and are subscribed regardless, below.
    // `llm_ok` was resolved above, before the banner.
    let mut directed_sub = if llm_ok {
        match transport.subscribe_with_queue(&directed, Some(&serve_group)) {
            Ok(s) => Some(s),
            Err(e) => {
                eprintln!("subscribe directed: {e}");
                process::exit(1);
            }
        }
    } else {
        None
    };
    // Short read timeout so the loop multiplexes all subjects responsively.
    // (Was 5s — with directed+broadcast+recall round-robined, a 5s timeout meant
    // the recall sub was only polled every ~12s, making daemon-served recall
    // slower than local. 250ms keeps recall sub-second; idle cost is a blocking
    // read, near-zero CPU.)
    if let Some(s) = directed_sub.as_mut() {
        let _ = s.set_timeout(Some(Duration::from_millis(250)));
    }

    // Broadcast on a separate connection so the directed sub doesn't block it.
    //
    // A failure here now leaves `bcast_sub` as None and the main loop carries
    // on with whatever else subscribed. Pre-fix it diverted into a
    // directed-only loop that ALSO abandoned the recall responder — a
    // degradation well beyond the thing that had actually failed. Every
    // subscription being optional makes that special case unnecessary.
    let bcast_transport = if llm_ok {
        match kannaka_memory::nats::SwarmTransport::connect(&nats_url) {
            Ok(t) => Some(t),
            Err(e) => {
                eprintln!("[swarm serve] WARN: broadcast subscription unavailable: {e}");
                None
            }
        }
    } else {
        None
    };
    let mut bcast_sub = bcast_transport.as_ref().and_then(|t| {
        match t.subscribe_with_queue("KANNAKA.ask.broadcast", Some(&serve_group)) {
            Ok(s) => Some(s),
            Err(e) => {
                eprintln!("[swarm serve] WARN: broadcast subscribe failed: {e}");
                None
            }
        }
    });
    if let Some(s) = bcast_sub.as_mut() {
        let _ = s.set_timeout(Some(Duration::from_millis(250)));
    }

    // Daemon-served recall (KANNAKA.recall.<agent_id>): the observatory, OBC
    // pulses, and the radio DJ can recall against this agent's warm in-memory
    // HRM instead of paying a 21 MB load + full xi-rerank per CLI call on the
    // 1-vCPU box. Uses the attention-beam prefilter + recall_with_beam
    // (O(beam), sub-second) — the same path the substrate responder uses. swarm
    // serve runs read-only (enforced above), so the observation mutation never
    // persists.
    let recall_subject = format!("KANNAKA.recall.{agent_id}");
    let recall_transport = kannaka_memory::nats::SwarmTransport::connect(&nats_url).ok();
    let mut recall_sub = recall_transport
        .as_ref()
        .and_then(|t| t.subscribe_with_queue(&recall_subject, Some(&serve_group)).ok());
    match recall_sub.as_mut() {
        Some(s) => {
            let _ = s.set_timeout(Some(Duration::from_millis(250)));
            eprintln!("[swarm serve] serving recall on {recall_subject}");
        }
        None => eprintln!(
            "[swarm serve] WARN: recall responder unavailable (extra NATS connection failed)"
        ),
    }

    // Associative neighbours (KANNAKA.neighbors.<agent_id>): the "resonate"
    // backend — top-K memories near a query OR near an existing memory, the
    // wire twin of the `kannaka neighbors` CLI subcommand.
    //
    // Deliberately BESIDE recall rather than behind the ask gate: this needs no
    // LLM, so a keyless node still serves it. That is the point of recall-only
    // mode — such a node keeps contributing everything its memory can answer,
    // and declines only what it genuinely cannot.
    //
    // Own connection, matching recall: the transport documents a
    // one-subscription-per-connection model, and sharing would have the two
    // readers steal each other's bytes.
    let neighbors_subject = format!("KANNAKA.neighbors.{agent_id}");
    let neighbors_transport = kannaka_memory::nats::SwarmTransport::connect(&nats_url).ok();
    let mut neighbors_sub = neighbors_transport
        .as_ref()
        .and_then(|t| t.subscribe_with_queue(&neighbors_subject, Some(&serve_group)).ok());
    match neighbors_sub.as_mut() {
        Some(s) => {
            let _ = s.set_timeout(Some(Duration::from_millis(250)));
            eprintln!("[swarm serve] serving neighbors on {neighbors_subject}");
        }
        None => eprintln!(
            "[swarm serve] WARN: neighbors responder unavailable (extra NATS connection failed)"
        ),
    }

    // ADR-0036 Phase 1: this daemon is read-only and never saves the .hrm, but
    // it serves the bulk of recall traffic — so its reactivation bumps must be
    // flushed to the sidecar (sidecar-only write, safe under readonly) or the
    // replay signal is lost. Flush at most once a minute.
    let mut last_reactivation_flush = std::time::Instant::now();

    // Freshness (#563): this daemon serves a warm in-memory copy of the HRM
    // loaded at boot. The single writer (and the nightly dream) rewrite the
    // file on disk, so the served mind drifts stale until a restart. Watch the
    // file's mtime and restart-to-reload when it changes — crash-only reload
    // reuses the proven boot path with ZERO transient double-memory on the
    // small hub (an in-place reload would briefly hold two full HRMs). The
    // settle window avoids reloading mid-write. Replaces the hourly cron
    // restart, whose only surviving job (post-#500 liveness) was freshness.
    // The CONFIGURED store, not the default: with a custom `cfg.hrm.path`
    // this used to watch the mtime of a file nobody writes, so the served
    // mind silently never reloaded (#769 — the serve half).
    let hrm_path = if cfg.hrm.path.is_empty() {
        data_dir().join("kannaka.hrm")
    } else {
        std::path::PathBuf::from(&cfg.hrm.path)
    };
    let hrm_loaded_mtime = std::fs::metadata(&hrm_path).and_then(|m| m.modified()).ok();
    let mut hrm_pending: Option<(std::time::SystemTime, std::time::Instant)> = None;
    let mut last_freshness_check = std::time::Instant::now();
    const FRESHNESS_CHECK_SECS: u64 = 60;
    const FRESHNESS_SETTLE: Duration = Duration::from_secs(20);

    // systemd watchdog (#563): belt to the liveness braces. If this loop
    // itself wedges (a pathological recall, a stuck syscall), WATCHDOG=1
    // stops flowing and systemd kills + restarts us (WatchdogSec in the
    // unit). READY=1 is sent once for Type=notify compatibility. No-op when
    // NOTIFY_SOCKET is unset (dev shells, Windows).
    sd_notify("READY=1");
    let mut last_watchdog = std::time::Instant::now();

    loop {
        // Watchdog heartbeat at most every 10s — one datagram, no allocation.
        if last_watchdog.elapsed().as_secs() >= 10 {
            sd_notify("WATCHDOG=1");
            last_watchdog = std::time::Instant::now();
        }

        // Freshness: cheap stat once a minute.
        if last_freshness_check.elapsed().as_secs() >= FRESHNESS_CHECK_SECS {
            last_freshness_check = std::time::Instant::now();
            if let (Some(loaded), Ok(meta)) = (hrm_loaded_mtime, std::fs::metadata(&hrm_path)) {
                if let Ok(current) = meta.modified() {
                    let (next_pending, reload) = freshness_decision(
                        loaded,
                        current,
                        hrm_pending,
                        std::time::Instant::now(),
                        FRESHNESS_SETTLE,
                    );
                    hrm_pending = next_pending;
                    if reload {
                        eprintln!(
                            "[swarm serve] HRM changed on disk and settled — restarting to serve the fresh mind (#563)"
                        );
                        sd_notify("STOPPING=1");
                        process::exit(1); // Restart=always brings us back on the new file
                    }
                }
            }
        }
        // Round-robin: try directed first, then broadcast. Timeout means
        // "nothing right now — poll the next subject"; Closed means the
        // socket is dead and the loop must NOT spin on it (pre-fix this
        // burned 100% CPU forever after a NATS restart).
        // Both ask subjects are absent in recall-only mode (no LLM provider).
        if let Some(sub) = directed_sub.as_mut() {
            match sub.next_event() {
                SubEvent::Msg(msg) => {
                    _handle_serve_msg(
                        sys, cfg, &transport, &msg, /*is_broadcast*/ false, threshold, &agent_id,
                        &nats_url, &mut rate_limit,
                    );
                }
                SubEvent::Timeout => {}
                SubEvent::Closed => {
                    eprintln!(
                        "[swarm serve] directed subscription ({directed}) closed — exiting for restart"
                    );
                    process::exit(1);
                }
            }
        }
        if let (Some(sub), Some(bt)) = (bcast_sub.as_mut(), bcast_transport.as_ref()) {
            match sub.next_event() {
                SubEvent::Msg(msg) => {
                    _handle_serve_msg(
                        sys,
                        cfg,
                        bt,
                        &msg,
                        /*is_broadcast*/ true,
                        threshold,
                        &agent_id,
                        &nats_url,
                        &mut rate_limit,
                    );
                }
                SubEvent::Timeout => {}
                SubEvent::Closed => {
                    eprintln!("[swarm serve] broadcast subscription closed — exiting for restart");
                    process::exit(1);
                }
            }
        }
        // Daemon-served recall — reply with the agent's own memories (full content).
        let mut recall_closed = false;
        if let Some(ref mut rsub) = recall_sub {
            match rsub.next_event() {
                SubEvent::Timeout => {}
                SubEvent::Closed => {
                    // Best-effort responder — degrade to ask-only (mirrors
                    // the startup behavior when the extra connection fails).
                    eprintln!("[swarm serve] WARN: recall subscription closed — continuing without recall responder");
                    recall_closed = true;
                }
                SubEvent::Msg(msg) => {
                    let reply_to = msg.reply_to.clone();
                    let req: serde_json::Value =
                        serde_json::from_slice(&msg.payload).unwrap_or(serde_json::Value::Null);
                    let query = req
                        .get("query")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    // Clamp peer-supplied top_k: it comes straight off the NATS
                    // wire and drives recall + result-Vec allocation + per-result
                    // store.get + JSON serialization. An unbounded value (e.g.
                    // {"top_k": 4_000_000_000}) is an OOM/DoS vector on the
                    // 1-core/6GB hub. The sibling `cores` handler caps peer input
                    // the same way (truncate(1024)).
                    let top_k = req
                        .get("top_k")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(8)
                        .min(100) as usize;
                    if let (false, Some(reply)) = (query.is_empty(), reply_to) {
                        let beam = kannaka_memory::agent::attention_beam_for_prompt(
                            sys,
                            &query,
                            kannaka_memory::agent::DEFAULT_ATTENTION_BEAM,
                        );
                        let recall = if beam.is_empty() {
                            sys.recall(&query, top_k) // cold/small HRM fallback
                        } else {
                            sys.recall_with_beam(&beam, &query, top_k)
                        };
                        let results: Vec<serde_json::Value> = recall
                            .map(|rs| {
                                rs.iter()
                                    .map(|r| {
                                        // Include the memory's wave phase so swarm-side
                                        // sensemaking (contradiction detection) can use the
                                        // wave-native stance signal across peers (ADR-0035).
                                        let phase = sys
                                            .engine
                                            .store
                                            .get(&r.id)
                                            .ok()
                                            .flatten()
                                            .map(|m| m.phase)
                                            .unwrap_or(0.0);
                                        serde_json::json!({
                                            "id": r.id.to_string(),
                                            "content": r.content,
                                            "similarity": r.similarity,
                                            "strength": r.strength,
                                            "age_hours": r.age_hours,
                                            "phase": phase,
                                        })
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        if let Some(ref rt) = recall_transport {
                            let payload = serde_json::json!({ "from": agent_id, "query": query, "results": results });
                            let _ = rt.reply(&reply, payload.to_string().as_bytes());
                        }
                    }
                }
            }
        }
        if recall_closed {
            recall_sub = None;
        }
        // Associative neighbours — same best-effort posture as recall: a closed
        // subscription degrades this one responder rather than taking the
        // daemon down, because the ask/recall paths are still serving.
        let mut neighbors_closed = false;
        if let Some(ref mut nsub) = neighbors_sub {
            match nsub.next_event() {
                SubEvent::Timeout => {}
                SubEvent::Closed => {
                    eprintln!("[swarm serve] WARN: neighbors subscription closed — continuing without neighbors responder");
                    neighbors_closed = true;
                }
                SubEvent::Msg(msg) => {
                    if let (Some(reply_to), Some(nt)) =
                        (msg.reply_to.clone(), neighbors_transport.as_ref())
                    {
                        let payload = _neighbors_reply(sys, &agent_id, &msg.payload);
                        let _ = nt.reply(&reply_to, payload.to_string().as_bytes());
                    }
                }
            }
        }
        if neighbors_closed {
            neighbors_sub = None;
        }
        if last_reactivation_flush.elapsed() >= Duration::from_secs(60) {
            sys.flush_reactivation();
            last_reactivation_flush = std::time::Instant::now();
        }
    }
}

/// Clamp a peer-supplied `top_k` for the neighbours responder.
///
/// The value arrives straight off the NATS wire and drives recall plus a
/// per-result Vec allocation, a `store.get` and JSON serialization, so an
/// unbounded one (`{"top_k": 4000000000}`) is an OOM lever on the 1-vCPU hub.
/// Same cap the recall responder and the `cores` handler already apply.
#[cfg(feature = "nats")]
pub(crate) fn neighbors_top_k(req: &serde_json::Value) -> usize {
    req.get("top_k")
        .and_then(|v| v.as_u64())
        .unwrap_or(10)
        .clamp(1, 100) as usize
}

/// Build the reply for one `KANNAKA.neighbors.<agent_id>` request.
///
/// The wire twin of the `kannaka neighbors` CLI subcommand: top-K memories
/// near a free-text `query`, or — when `id` is a UUID — near that memory's own
/// content, which is the associative "what sits next to this?" mode.
///
/// Read-only. `swarm serve` enforces `KANNAKA_READONLY`, so the observation
/// recall performs mutates the medium in RAM and never persists.
#[cfg(feature = "nats")]
fn _neighbors_reply(
    sys: &mut kannaka_memory::openclaw::KannakaMemorySystem,
    agent_id: &str,
    payload: &[u8],
) -> serde_json::Value {
    let req: serde_json::Value = match serde_json::from_slice(payload) {
        Ok(v) => v,
        Err(e) => {
            return serde_json::json!({ "from": agent_id, "error": format!("bad json: {e}") })
        }
    };
    let top_k = neighbors_top_k(&req);

    // An `id` that parses as a UUID anchors on that memory's content; anything
    // else falls back to `query`. Mirrors the CLI's precedence.
    let id_str = req.get("id").and_then(|v| v.as_str());
    let (query, anchor) = match id_str.and_then(|s| s.parse::<uuid::Uuid>().ok()) {
        Some(uuid) => match sys.engine.store.get(&uuid) {
            Ok(Some(m)) => (m.content.clone(), serde_json::json!({ "id": uuid.to_string() })),
            _ => {
                return serde_json::json!({
                    "from": agent_id,
                    "error": format!("memory {uuid} not found"),
                })
            }
        },
        None => {
            let q = req
                .get("query")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if q.is_empty() {
                return serde_json::json!({
                    "from": agent_id,
                    "error": "neighbors requires `query` (text) or `id` (uuid)",
                });
            }
            let anchor = serde_json::json!({ "query": q });
            (q, anchor)
        }
    };

    match sys.recall(&query, top_k) {
        Ok(results) => {
            let neighbors: Vec<serde_json::Value> = results
                .iter()
                .map(|m| {
                    serde_json::json!({
                        "id": m.id.to_string(),
                        "content": m.content,
                        "similarity": m.similarity,
                        "strength": m.strength,
                        "age_hours": m.age_hours,
                        "layer": m.layer,
                    })
                })
                .collect();
            serde_json::json!({ "from": agent_id, "anchor": anchor, "neighbors": neighbors })
        }
        Err(e) => serde_json::json!({ "from": agent_id, "error": format!("recall failed: {e}") }),
    }
}

#[cfg(feature = "nats")]
#[allow(clippy::too_many_arguments)]
fn _handle_serve_msg(
    sys: &mut kannaka_memory::openclaw::KannakaMemorySystem,
    cfg: &KannakaConfig,
    transport: &kannaka_memory::nats::SwarmTransport,
    msg: &kannaka_memory::nats::NatsMessage,
    is_broadcast: bool,
    threshold: f32,
    serve_agent_id: &str,
    nats_url: &str,
    rate_limit: &mut kannaka_memory::serve_guard::ServeRateLimiter,
) {
    let reply_to = match &msg.reply_to {
        Some(r) => r.clone(),
        None => {
            eprintln!(
                "[swarm serve] msg without reply-to on {} — ignoring",
                msg.subject
            );
            return;
        }
    };

    let req: serde_json::Value = match serde_json::from_slice(&msg.payload) {
        Ok(v) => v,
        Err(e) => {
            let err =
                serde_json::json!({ "from": serve_agent_id, "error": format!("bad json: {e}") });
            let _ = transport.reply(&reply_to, err.to_string().as_bytes());
            return;
        }
    };
    let from = req.get("from").and_then(|v| v.as_str()).unwrap_or("?");
    let text = req.get("text").and_then(|v| v.as_str()).unwrap_or("");
    let recall_q = req.get("recall_query").and_then(|v| v.as_str());

    if text.is_empty() {
        let err = serde_json::json!({ "from": serve_agent_id, "error": "empty text" });
        let _ = transport.reply(&reply_to, err.to_string().as_bytes());
        return;
    }

    // #932, behaviour 1: the wire never chooses the route. Provider and model
    // come from this node's own `[llm]`; the envelope is read only to SAY what
    // was ignored. An ask labelling itself `kind = "reason"` to reach the
    // operator's expensive key gets the same provider as every other ask.
    //
    // Nothing on the answer path below has ever consulted these fields — this
    // is the invariant made explicit and testable, because #931 adds a router
    // right here and "we happen not to read it" is not a property a test holds.
    let route = kannaka_memory::serve_guard::resolve_served_route(&cfg.llm, &req);
    if !route.ignored_wire_fields.is_empty() {
        eprintln!(
            "[swarm serve] {} tried to steer the route ({}) — ignored; answering with {}/{}",
            kannaka_memory::sanitize_display(from),
            route.ignored_wire_fields.join(", "),
            route.provider,
            route.model
        );
    }

    // An id that long is not a name, it is a payload: the broker's max_payload
    // is 64MB, so `from` is attacker-SIZED as well as attacker-chosen. Refuse it
    // here, before it costs a recall. (`requester_key` hashes an oversized id
    // anyway, so the limiter is safe on its own — this is the cheaper refusal,
    // not the bound.)
    if kannaka_memory::serve_guard::id_is_oversized(from) {
        let err = serde_json::json!({
            "from": serve_agent_id,
            "error": format!(
                "`from` is {} bytes; this node accepts at most {}",
                from.len(),
                kannaka_memory::serve_guard::MAX_REQUESTER_ID_BYTES
            ),
        });
        let _ = transport.reply(&reply_to, err.to_string().as_bytes());
        return;
    }

    // #932, behaviour 2: rate limit BEFORE the resonance probe. The probe is a
    // full recall against the medium, so it is itself the cheap half of the
    // abuse — metering after it would leave the 1-vCPU hub payable in CPU even
    // when it never spends a token.
    //
    // Only the REQUESTER's bucket is committed here. The hourly ceiling is
    // committed further down, past the resonance gate, at the point the ask
    // actually reaches the model — see `ServeRateLimiter::check`. Metering the
    // ceiling here instead made a stranger's ~30-byte non-resonant publish, one
    // every twelve seconds, enough to take the public ask_kannaka off the air
    // for everybody: an outage cheaper than the abuse it was guarding.
    //
    // Keyed by the envelope's `from`, else the reply-inbox prefix. Both are
    // caller-chosen (NATS core carries no publisher identity on a message), so
    // this bounds an honest neighbour precisely and a rotating caller only via
    // the hourly TOTAL. The startup banner says exactly that.
    let requester =
        kannaka_memory::serve_guard::requester_key(Some(from), Some(reply_to.as_str()));
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let decision = rate_limit.check(&requester, now_secs);
    if let Some(refusal) = decision.refusal_text() {
        // Logged once per requester per window, not once per ask: a caller
        // hammering the bus must not be able to drive our log volume either.
        let first = matches!(
            decision,
            kannaka_memory::serve_guard::RateDecision::Requester { first_in_window: true, .. }
                | kannaka_memory::serve_guard::RateDecision::Global { first_in_window: true, .. }
                | kannaka_memory::serve_guard::RateDecision::TooManyRequesters { .. }
        );
        if first {
            eprintln!(
                "[swarm serve] refusing {}: {} (silent for the rest of this window)",
                kannaka_memory::sanitize_display(&requester),
                refusal
            );
        }
        // A short, polite reply beats silence: the caller learns it was throttled
        // rather than timing out and retrying. It costs one small publish on a
        // subject only that caller is listening to.
        let err = serde_json::json!({ "from": serve_agent_id, "error": refusal });
        let _ = transport.reply(&reply_to, err.to_string().as_bytes());
        return;
    }

    // #932, behaviour 3: `hops`, ceiling 1 — a hired ask never hires. Absent
    // field = 0, so an envelope written before this field existed is a
    // first-hop ask and behaves exactly as it always did. The cell is read by
    // the outbound ask publisher (`ask --remote`), which refuses to forward
    // once the ceiling is reached; it is cleared on every exit path below.
    let inbound_hops = kannaka_memory::serve_guard::hops_of(&req);
    kannaka_memory::serve_guard::set_serving_hops(Some(inbound_hops));

    // Self-throttle on broadcast: only reply if local recall has resonance ≥ threshold.
    if is_broadcast {
        let probe = recall_q.unwrap_or(text);
        let res = sys.recall(probe, 1).unwrap_or_default();
        let top = res.first().map(|r| r.strength).unwrap_or(0.0);
        if top < threshold {
            eprintln!("[swarm serve] broadcast from {from}: top resonance {top:.3} < threshold {threshold:.2} — staying quiet");
            kannaka_memory::serve_guard::set_serving_hops(None);
            return;
        }
        eprintln!(
            "[swarm serve] broadcast from {from}: top resonance {top:.3} ≥ threshold — answering"
        );
    } else {
        eprintln!("[swarm serve] directed from {from}");
    }

    // #746: honour the caller's recall mode. An ABSENT or unrecognised `mode`
    // resolves to FullRecall — byte-identical to what every pre-#746 server
    // did — so an old client's payload behaves exactly as it always has.
    //
    // None of these three run the tool loop. That is deliberate, not an
    // oversight: the loop exposes `remember` and `dream`, and this daemon's
    // read-only mode blocks the PERSIST, not the in-RAM mutation, so a remote
    // `remember` would poison the live medium every other caller is answered
    // from. `mode_used` says `full_recall_no_tools` rather than `full_recall`
    // so the caller is told that plainly instead of having to infer it.
    let mode = kannaka_memory::agent::RemoteAskMode::from_wire(
        req.get("mode").and_then(|v| v.as_str()),
    );
    eprintln!("[swarm serve] mode={} (requested {:?})", mode.wire_name(), req.get("mode"));

    // #412: answer AS the served identity. When --agent-id makes this loop
    // serve a different agent than the config's own `[agent]`, that agent must
    // NOT be lent the config agent's persona/display name (the bug where
    // KANNAKA.ask.0xSCADA-QE answered "I'm Kannaka"). Present the served id
    // with a neutral self; the config's persona only applies when the config
    // IS that agent. A box that serves 0xSCADA-QE for real sets `[agent] id`
    // and `persona` in its own config and hits the equal-id branch.
    let mut eff = cfg.clone();
    if eff.agent.id != serve_agent_id {
        eff.agent.id = serve_agent_id.to_string();
        eff.agent.display_name = String::new();
        eff.agent.persona = String::new();
    }
    let cfg = &eff;

    // #932: the hourly ceiling is committed HERE — past the resonance gate,
    // immediately before the model call — so it counts asks that actually
    // spend. An ask the gate dropped cost one recall and nothing else, and must
    // not be able to exhaust the node's hour on everybody else's behalf.
    rate_limit.commit_global(now_secs);

    let result = match mode {
        kannaka_memory::agent::RemoteAskMode::Attention => {
            kannaka_memory::agent::ask_attention(sys, cfg, text)
        }
        kannaka_memory::agent::RemoteAskMode::NoRecall => {
            kannaka_memory::agent::ask_no_recall(sys, cfg, text)
        }
        kannaka_memory::agent::RemoteAskMode::FullRecall => {
            kannaka_memory::agent::ask_notools_ex(sys, cfg, text, recall_q)
        }
    };
    // The hop budget covers the ANSWER, not the reply write-back: past this
    // point nothing can hire another node on this ask's behalf.
    kannaka_memory::serve_guard::set_serving_hops(None);
    // "from" is the id this serve loop is actually answering as (the
    // --agent-id override when given), not unconditionally cfg.agent.id.
    // `mode_used` is additive: an old client ignores it, a new one uses its
    // presence to tell a mode-aware peer from a pre-#746 one.
    let reply = match result {
        Ok(r) => serde_json::json!({
            "from": serve_agent_id,
            "text": r.text,
            "mode_used": mode.mode_used_name(),
        }),
        Err(e) => serde_json::json!({
            "from": serve_agent_id,
            "error": format!("{e}"),
            "mode_used": mode.mode_used_name(),
        }),
    };

    // Heavy ask calls take 3–5 min. The original NATS connection has been
    // idle that whole time and the server typically closes it on PING
    // timeout. Open a fresh connection just to send the reply so the
    // long-running subscription on the parent transport doesn't have to
    // be reconnected on every request. The fresh connection reuses the
    // SAME resolved URL the serve loop connected with (pre-fix it
    // re-resolved from env with a 127.0.0.1 fallback, so a --nats-url
    // serve could reply into the wrong broker).
    let reply_payload = reply.to_string();
    let reply_result = match kannaka_memory::nats::SwarmTransport::connect(nats_url) {
        Ok(fresh) => fresh.reply(&reply_to, reply_payload.as_bytes()),
        Err(e) => Err(e),
    };
    // Fall back to the original transport if the fresh connect failed.
    if reply_result.is_err() {
        if let Err(e2) = transport.reply(&reply_to, reply_payload.as_bytes()) {
            eprintln!("[swarm serve] reply failed (fresh + fallback): {e2}");
            return;
        }
    }
    eprintln!("[swarm serve] replied on {reply_to}");
}

#[cfg(not(feature = "nats"))]
pub(crate) fn handle_swarm_serve(
    _: &mut kannaka_memory::openclaw::KannakaMemorySystem,
    _: &KannakaConfig,
    _: &[String],
) {
    eprintln!("swarm serve requires the 'nats' feature");
    process::exit(1);
}

// ── ADR-0026 Phase 2: Exemplar broadcast (#72) ─────────────────────────────

#[cfg(feature = "nats")]
pub(crate) fn handle_swarm_exemplars(
    sys: &mut kannaka_memory::openclaw::KannakaMemorySystem,
    cfg: &KannakaConfig,
    args: &[String],
) {
    const USAGE: &str = "Usage: kannaka swarm exemplars <publish|list> [--top-k N] [--agent-id ID] [--from <agent>] [--nats-url URL]";
    // Usage:
    //   kannaka swarm exemplars publish [--top-k N] [--agent-id ID]
    //   kannaka swarm exemplars list [--from <agent_id>] [--top-k N]
    let sub = args.get(2).map(String::as_str).unwrap_or("publish");
    let mut top_k: usize = 20;
    let mut agent_id_override: Option<String> = None;
    let mut from: Option<String> = None;
    let mut i = 3;
    while i < args.len() {
        match args[i].as_str() {
            "--top-k" => {
                top_k = parse_flag_value(args, i, "--top-k", USAGE);
                i += 2;
            }
            "--agent-id" => {
                agent_id_override = Some(flag_value(args, i, "--agent-id", USAGE).to_string());
                i += 2;
            }
            "--from" => {
                from = Some(flag_value(args, i, "--from", USAGE).to_string());
                i += 2;
            }
            "--nats-url" => {
                let _ = flag_value(args, i, "--nats-url", USAGE);
                i += 2;
            }
            other => {
                warn_unknown_flag("exemplars", other);
                i += 1;
            }
        }
    }
    let agent_id = agent_id_override.unwrap_or_else(|| cfg.agent.id.clone());
    let nats_url = resolve_nats_url(args, 0, &cfg.swarm.nats_url);
    let transport = match kannaka_memory::nats::SwarmTransport::connect(&nats_url) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("nats: {e}");
            process::exit(1);
        }
    };

    match sub {
        "publish" => {
            // Make sure the stream exists. Best-effort — JetStream may already
            // have it from a previous run.
            // #577: this was `let _ = transport.ensure_exemplar_stream();`.
            // `publish_exemplar` is a raw subject PUB — the broker accepts it
            // whether or not the KANNAKA_EXEMPLARS stream exists, so with the
            // stream missing every publish "succeeds" and nothing is durable.
            // `swarm exemplars list` and `swarm absorb` then see nothing. Not
            // fatal (the publishes may still be consumed live), but the
            // durability loss must be stated rather than swallowed.
            let durable = match transport.ensure_exemplar_stream() {
                Ok(()) => true,
                Err(e) => {
                    eprintln!(
                        "[exemplars] WARNING: KANNAKA_EXEMPLARS stream unavailable ({e}) — publishes \
                         below will NOT be durable and `swarm exemplars list` will show nothing."
                    );
                    false
                }
            };

            let report = sys.observe();
            let mut clusters = report.clusters.clusters.clone();
            // Order by mean_amplitude desc — strongest exemplars first.
            clusters.sort_by(|a, b| {
                b.mean_amplitude
                    .partial_cmp(&a.mean_amplitude)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            // inc-1b: ALWAYS sign exemplar emits with the node key so peers running
            // the corroboration gate can verify + accrue. Best-effort load.
            let node_seed = kannaka_memory::provenance::node_signing_key(&data_dir()).ok();
            let mut published = 0;
            for c in clusters.iter().take(top_k) {
                let mut payload = serde_json::json!({
                    "agent_id": agent_id,
                    "cluster_id": c.cluster_id,
                    "size": c.size,
                    "content": c.exemplar_content,
                    "exemplar_id": c.exemplar_id,
                    "amplitude": c.mean_amplitude,
                    "frequency": c.mean_frequency,
                    "phase": c.mean_phase,
                    "modality": c.dominant_modality,
                    "theme": c.theme,
                    "semantic_summary": c.semantic_summary,
                    "coherence": c.coherence,
                    "xi_diversity": c.xi_diversity,
                    "created_at": chrono::Utc::now().to_rfc3339(),
                });
                // Sign over (exemplar_id, content, amplitude) — the fields the
                // absorbing node's admit() reconstructs. Only when both id+content
                // are present and parseable.
                if let (Some(seed), Some(content), Some(eid)) = (
                    &node_seed,
                    c.exemplar_content.as_deref(),
                    c.exemplar_id.as_deref(),
                ) {
                    if let Ok(mem_id) = uuid::Uuid::parse_str(eid) {
                        let nonce = *uuid::Uuid::new_v4().as_bytes();
                        let ts = kannaka_memory::provenance::now_ms();
                        let sig = kannaka_memory::sign_mem(
                            seed,
                            kannaka_memory::SIGN_AGENT_ID,
                            mem_id,
                            &nonce,
                            ts,
                            content,
                            kannaka_memory::SUBJECT_EXEMPLAR,
                            kannaka_memory::provenance::amp_to_q16(c.mean_amplitude),
                            kannaka_memory::PROV_TIER,
                        );
                        if let (Some(obj), Ok(v)) =
                            (payload.as_object_mut(), serde_json::to_value(&sig))
                        {
                            obj.insert("provenance_sig".to_string(), v);
                        }
                    }
                }
                match transport.publish_exemplar(&agent_id, c.cluster_id, &payload) {
                    Ok(()) => published += 1,
                    Err(e) => eprintln!("[exemplars] cluster {} publish failed: {e}", c.cluster_id),
                }
            }
            // #689: the summary must not read as success when nothing was
            // durable — live subscribers may have seen the PUBs, but list/
            // absorb/autoabsorb read only the JetStream history.
            println!(
                "Published {} exemplars from {} (top-{} by amplitude){}",
                published,
                agent_id,
                top_k,
                if durable {
                    ""
                } else {
                    " — NOT DURABLE: KANNAKA_EXEMPLARS stream unavailable; \
                     list/absorb/autoabsorb will not see these"
                }
            );
        }
        "list" => {
            let exemplars = match transport.get_exemplars(from.as_deref()) {
                Ok(e) => e,
                Err(e) => {
                    eprintln!("nats: {e}");
                    process::exit(1);
                }
            };
            let limit = if top_k == 0 { exemplars.len() } else { top_k };
            for (i, e) in exemplars.iter().take(limit).enumerate() {
                let agent = e.get("agent_id").and_then(|v| v.as_str()).unwrap_or("?");
                let cid = e.get("cluster_id").and_then(|v| v.as_u64()).unwrap_or(0);
                let amp = e.get("amplitude").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let content = e
                    .get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("(no content)");
                // SECURITY (increment-0): agent_id + content are attacker-
                // controllable wire strings — sanitize before printing (strip
                // ANSI/control bytes) and flag sources not on the trusted
                // allowlist as observe-only. Never print wire content raw.
                let agent_s = kannaka_memory::sanitize_display(agent);
                let trusted = agent == agent_id
                    || kannaka_memory::agent_matches_allowlist(
                        agent,
                        &cfg.swarm_trust.trusted_agents,
                    );
                let mark = if trusted { "" } else { " (unverified)" };
                let preview: String = kannaka_memory::sanitize_display(content)
                    .chars()
                    .take(120)
                    .collect();
                println!(
                    "[{:3}] {}{} c{:<3} amp={:.3}",
                    i + 1,
                    agent_s,
                    mark,
                    cid,
                    amp
                );
                println!("       {preview}");
            }
            println!();
            println!("Total: {} exemplars", exemplars.len());
        }
        other => {
            eprintln!("{USAGE}");
            eprintln!("Unknown subcommand: {other}");
            process::exit(1);
        }
    }
}

#[cfg(not(feature = "nats"))]
pub(crate) fn handle_swarm_exemplars(
    _: &mut kannaka_memory::openclaw::KannakaMemorySystem,
    _: &KannakaConfig,
    _: &[String],
) {
    eprintln!("swarm exemplars requires the 'nats' feature");
    process::exit(1);
}

/// ADR-0037 Track-D: `kannaka swarm cores <publish|list|shared>` — share belief
/// cores (L6 fingerprints + phases) across the swarm and measure overlap. All
/// read/publish (no HRM mutation); the writing `couple` command lands with the
/// heartbeat wiring. `shared` is the falsifiable "shared cores ⇒ agreement" metric.
#[cfg(feature = "nats")]
pub(crate) fn handle_swarm_cores(
    sys: &mut kannaka_memory::openclaw::KannakaMemorySystem,
    cfg: &KannakaConfig,
    args: &[String],
) {
    const USAGE: &str = "Usage: kannaka swarm cores <publish|list|shared> [--from <agent>] [--min-cos X] [--agent-id ID] [--nats-url URL]";
    let sub = args.get(2).map(String::as_str).unwrap_or("shared");
    let mut agent_id_override: Option<String> = None;
    let mut from: Option<String> = None;
    let mut min_cos: f32 = 0.85;
    let mut i = 3;
    while i < args.len() {
        match args[i].as_str() {
            "--agent-id" => {
                agent_id_override = Some(flag_value(args, i, "--agent-id", USAGE).to_string());
                i += 2;
            }
            "--from" => {
                from = Some(flag_value(args, i, "--from", USAGE).to_string());
                i += 2;
            }
            "--min-cos" => {
                min_cos = parse_flag_value(args, i, "--min-cos", USAGE);
                i += 2;
            }
            "--nats-url" => {
                let _ = flag_value(args, i, "--nats-url", USAGE);
                i += 2;
            }
            other => {
                warn_unknown_flag("cores", other);
                i += 1;
            }
        }
    }
    let agent_id = agent_id_override.unwrap_or_else(|| cfg.agent.id.clone());
    let nats_url = resolve_nats_url(args, 0, &cfg.swarm.nats_url);
    let transport = match kannaka_memory::nats::SwarmTransport::connect(&nats_url) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("nats: {e}");
            process::exit(1);
        }
    };

    // This node's belief cores (the L6 snapshot used everywhere in Track-D).
    let own_cores = sys
        .engine
        .store
        .as_any()
        .downcast_ref::<kannaka_memory::hrm_store::HrmStore>()
        .map(|h| h.belief_core_snapshot())
        .unwrap_or_default();

    match sub {
        "publish" => {
            let _ = transport.ensure_cores_stream();
            let payload = serde_json::json!({
                "agent_id": agent_id,
                "core_count": own_cores.len(),
                "cores": own_cores,
                "created_at": chrono::Utc::now().to_rfc3339(),
            });
            match transport.publish_cores(&agent_id, &payload) {
                Ok(()) => println!(
                    "published {} belief cores from {}",
                    own_cores.len(),
                    agent_id
                ),
                Err(e) => {
                    eprintln!("nats: {e}");
                    process::exit(1);
                }
            }
        }
        "list" => {
            let peers = match transport.get_peer_cores(from.as_deref()) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("nats: {e}");
                    process::exit(1);
                }
            };
            for p in &peers {
                let aid = p.get("agent_id").and_then(|v| v.as_str()).unwrap_or("?");
                // Count the actual cores array (authoritative), not the peer-supplied
                // core_count scalar (which a misbehaving peer could set inconsistently).
                let n = p
                    .get("cores")
                    .and_then(|c| c.as_array())
                    .map(|a| a.len() as u64)
                    .or_else(|| p.get("core_count").and_then(|v| v.as_u64()))
                    .unwrap_or(0);
                let ts = p.get("created_at").and_then(|v| v.as_str()).unwrap_or("");
                println!("{aid:<24} {n:>4} cores  {ts}");
            }
            println!("{} peer snapshot(s)", peers.len());
        }
        "shared" => {
            // Falsifiable "shared cores ⇒ swarm agreement": how many of THIS node's
            // belief cores are mirrored by each peer (same-charge fingerprint match).
            let peers = match transport.get_peer_cores(from.as_deref()) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("nats: {e}");
                    process::exit(1);
                }
            };
            println!(
                "own cores: {} (agent {agent_id}, min_cos={min_cos:.2})",
                own_cores.len()
            );
            let (mut total, mut counted) = (0usize, 0usize);
            for p in &peers {
                let aid = p.get("agent_id").and_then(|v| v.as_str()).unwrap_or("?");
                if aid == agent_id {
                    continue;
                }
                let mut peer_cores: Vec<kannaka_memory::l6::CoreObs> = p
                    .get("cores")
                    .and_then(|c| serde_json::from_value(c.clone()).ok())
                    .unwrap_or_default();
                // Bound the O(own × peer) match against a misbehaving peer's oversized
                // snapshot (honest publishers emit ≤8 cores; 1024 is a generous cap).
                peer_cores.truncate(1024);
                let shared = kannaka_memory::l6::shared_cores(&own_cores, &peer_cores, min_cos);
                let rate = if own_cores.is_empty() {
                    0.0
                } else {
                    shared as f32 / own_cores.len() as f32
                };
                println!(
                    "  {aid:<24} shared={shared:<4}/{:<4} peer  (agreement {:.0}%)",
                    peer_cores.len(),
                    rate * 100.0
                );
                total += shared;
                counted += 1;
            }
            if counted > 0 {
                println!(
                    "mean shared across {counted} peer(s): {:.1}",
                    total as f32 / counted as f32
                );
            } else {
                println!(
                    "no peers have published cores yet (run `kannaka swarm cores publish` on them)"
                );
            }
        }
        other => {
            eprintln!("{USAGE}");
            eprintln!("Unknown subcommand: {other}");
            process::exit(1);
        }
    }
}

#[cfg(not(feature = "nats"))]
pub(crate) fn handle_swarm_cores(
    _: &mut kannaka_memory::openclaw::KannakaMemorySystem,
    _: &KannakaConfig,
    _: &[String],
) {
    eprintln!("swarm cores requires the 'nats' feature");
    process::exit(1);
}

#[cfg(feature = "nats")]
pub(crate) fn handle_swarm_absorb(
    sys: &mut kannaka_memory::openclaw::KannakaMemorySystem,
    cfg: &KannakaConfig,
    args: &[String],
) {
    const USAGE: &str = "Usage: kannaka swarm absorb [--from <agent>] [--top-k N] [--threshold X] [--dry-run] [--nats-url URL]";
    // Usage: kannaka swarm absorb [--from <agent>] [--top-k N] [--threshold X] [--dry-run]
    let mut from: Option<String> = None;
    let mut top_k: usize = 50;
    let mut threshold: f32 = 0.4;
    let mut dry_run = false;
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--from" => {
                from = Some(flag_value(args, i, "--from", USAGE).to_string());
                i += 2;
            }
            "--top-k" => {
                top_k = parse_flag_value(args, i, "--top-k", USAGE);
                i += 2;
            }
            "--threshold" => {
                threshold = parse_flag_value(args, i, "--threshold", USAGE);
                i += 2;
            }
            "--dry-run" => {
                dry_run = true;
                i += 1;
            }
            "--nats-url" => {
                let _ = flag_value(args, i, "--nats-url", USAGE);
                i += 2;
            }
            other => {
                warn_unknown_flag("swarm absorb", other);
                i += 1;
            }
        }
    }
    if !dry_run {
        super::warn_if_readonly("swarm absorb");
    }

    let nats_url = resolve_nats_url(args, 0, &cfg.swarm.nats_url);
    let transport = match kannaka_memory::nats::SwarmTransport::connect(&nats_url) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("nats: {e}");
            process::exit(1);
        }
    };

    let exemplars = match transport.get_exemplars(from.as_deref()) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("nats: {e}");
            process::exit(1);
        }
    };
    if exemplars.is_empty() {
        eprintln!(
            "No exemplars found in stream{}.",
            from.as_ref()
                .map(|f| format!(" (from {f})"))
                .unwrap_or_default()
        );
        eprintln!("Hint: a peer must run 'kannaka swarm exemplars publish' first.");
        return;
    }
    eprintln!(
        "Found {} exemplars; evaluating against local medium (threshold {:.2})...",
        exemplars.len(),
        threshold
    );

    let mut absorbed = 0usize;
    let mut skipped_threshold = 0usize;
    let mut skipped_self = 0usize;
    let my_id = &cfg.agent.id;

    // Sort by amplitude descending so we evaluate the strongest first.
    let mut ordered = exemplars;
    ordered.sort_by(|a, b| {
        let aa = a.get("amplitude").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let bb = b.get("amplitude").and_then(|v| v.as_f64()).unwrap_or(0.0);
        bb.partial_cmp(&aa).unwrap_or(std::cmp::Ordering::Equal)
    });

    // inc-1b corroboration admit() chokepoint state. DORMANT unless the gate is
    // enabled AND seeds are pinned (then it governs each absorb below).
    let gate_dir = store_dir(cfg);
    let mut rep_store = kannaka_memory::reputation::RepStore::load(&gate_dir, &cfg.swarm_trust);
    let mut staging = kannaka_memory::absorb_gate::QuarantineStaging::load(&gate_dir);

    for e in ordered.iter().take(top_k) {
        let source = e.get("agent_id").and_then(|v| v.as_str()).unwrap_or("?");
        if source == my_id {
            skipped_self += 1;
            continue;
        }
        let content = e.get("content").and_then(|v| v.as_str()).unwrap_or("");
        if content.is_empty() {
            continue;
        }

        // Resonance against the local medium — compute by recall-and-check
        // the top result's strength.
        let res = sys.recall(content, 1).ok().unwrap_or_default();
        let top_strength = res.first().map(|r| r.strength).unwrap_or(0.0);
        let cluster_id = e.get("cluster_id").and_then(|v| v.as_u64()).unwrap_or(0);

        if top_strength >= threshold {
            // Already in our medium with high resonance — skip duplicates.
            // (The match suggests we've heard this before.)
            eprintln!(
                "  ✓ already resonant: {source} c{cluster_id} strength={top_strength:.3} — skip"
            );
            continue;
        }

        // Low local resonance + non-trivial content = candidate for absorption.
        // The absorb threshold inverts: we want NEW material that's distinctive,
        // not already present. But we also don't want noise. Use a min content
        // length + presence of metadata as a soft filter.
        let amp = e.get("amplitude").and_then(|v| v.as_f64()).unwrap_or(0.0);
        if amp < 0.3 || content.len() < 30 {
            skipped_threshold += 1;
            continue;
        }

        eprintln!(
            "  + new wavefront: {source} c{cluster_id} amp={amp:.3} resonance={top_strength:.3}"
        );
        eprintln!(
            "      \"{}\"",
            &content.chars().take(120).collect::<String>()
        );

        if !dry_run {
            // inc-1b: route through the corroboration admit() chokepoint. DORMANT
            // ⇒ admit returns Live (current ungated behaviour) with a sanitized
            // amplitude; ACTIVE ⇒ the decision governs.
            let memory_id = e
                .get("exemplar_id")
                .and_then(|v| v.as_str())
                .and_then(|s| uuid::Uuid::parse_str(s).ok())
                .unwrap_or_else(uuid::Uuid::new_v4);
            let prov_sig: Option<kannaka_memory::ProvenanceSig> = e
                .get("provenance_sig")
                .and_then(|v| serde_json::from_value(v.clone()).ok());
            let wire_phase = e.get("phase").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
            let wire_freq = e.get("frequency").and_then(|v| v.as_f64()).unwrap_or(0.1) as f32;
            let now = kannaka_memory::provenance::now_ms();
            let (decision, clean, pending) = kannaka_memory::admit(
                content,
                amp as f32,
                wire_phase,
                wire_freq,
                false,
                kannaka_memory::SUBJECT_EXEMPLAR,
                memory_id,
                prov_sig.as_ref(),
                &mut staging,
                &mut rep_store,
                cfg,
                now,
            );
            use kannaka_memory::AdmitDecision::*;
            match decision {
                Live => {
                    // Tag with provenance so we can identify swarm-origin memories later.
                    let category = format!("swarm:{source}");
                    match sys.remember_with_category(content, &category, clean.amplitude as f64) {
                        Ok(id) => {
                            // #8: commit the pending promotion ONLY after the medium
                            // write succeeds (no-op when dormant / non-Live).
                            kannaka_memory::commit_promotion(
                                pending,
                                &mut rep_store,
                                &mut staging,
                                cfg,
                            );
                            eprintln!("      remembered as {id}");
                            absorbed += 1;
                        }
                        // #8: write failed — drop the pending, do not commit.
                        Err(e) => {
                            eprintln!("      remember failed: {e}");
                        }
                    }
                }
                Quarantine | ProbationLive => {
                    eprintln!("      quarantined (awaiting corroboration)");
                }
                Drop => {
                    eprintln!("      dropped (invalid signature / echo)");
                }
            }
        } else {
            absorbed += 1;
        }
    }

    println!();
    println!(
        "Absorb complete{}:",
        if dry_run { " (DRY RUN)" } else { "" }
    );
    println!("  absorbed:    {absorbed}");
    println!("  skipped (threshold/length): {skipped_threshold}");
    println!("  skipped (self-origin):      {skipped_self}");
}

#[cfg(not(feature = "nats"))]
pub(crate) fn handle_swarm_absorb(
    _: &mut kannaka_memory::openclaw::KannakaMemorySystem,
    _: &KannakaConfig,
    _: &[String],
) {
    eprintln!("swarm absorb requires the 'nats' feature");
    process::exit(1);
}

#[cfg(feature = "nats")]
pub(crate) fn handle_swarm_peers(cfg: &KannakaConfig, args: &[String]) {
    const USAGE: &str = "Usage: kannaka swarm peers [--json] [--all] [--nats-url URL]";
    // Usage: kannaka swarm peers [--json] [--all]
    let mut as_json = false;
    let mut show_all = false;
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => {
                as_json = true;
                i += 1;
            }
            // Forensic view: include peers whose `last_seen` has gone stale
            // (crashed / partitioned / retired but never tombstoned, #737)
            // instead of hiding them.
            "--all" => {
                show_all = true;
                i += 1;
            }
            "--nats-url" => {
                let _ = flag_value(args, i, "--nats-url", USAGE);
                i += 2;
            }
            other => {
                warn_unknown_flag("swarm peers", other);
                i += 1;
            }
        }
    }
    let nats_url = resolve_nats_url(args, 0, &cfg.swarm.nats_url);
    let transport = match kannaka_memory::nats::SwarmTransport::connect(&nats_url) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("nats: {e}");
            process::exit(1);
        }
    };
    // Read the retained set ONCE and split it locally, so the default view can
    // say how many stale records it suppressed without a second round trip.
    let retained = match transport.get_presence_all() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("get_presence: {e}");
            process::exit(1);
        }
    };
    let now = chrono::Utc::now();
    let stale_count = retained
        .iter()
        .filter(|p| !kannaka_memory::nats::is_fresh_presence(p, now))
        .count();
    let peers: Vec<serde_json::Value> = if show_all {
        retained
    } else {
        retained
            .into_iter()
            .filter(|p| kannaka_memory::nats::is_fresh_presence(p, now))
            .collect()
    };
    if as_json {
        // Annotate rather than only filter: `live` and `last_seen_age_secs` are
        // additive fields, so existing consumers keep parsing the records they
        // already read, and one that wants the freshness signal (or is looking
        // at `--all`) no longer has to re-derive it from `last_seen`.
        let annotated: Vec<serde_json::Value> = peers
            .into_iter()
            .map(|mut p| {
                let live = kannaka_memory::nats::is_fresh_presence(&p, now);
                let age = kannaka_memory::nats::presence_age_secs(&p, now);
                if let Some(obj) = p.as_object_mut() {
                    obj.insert("live".into(), serde_json::json!(live));
                    obj.insert("last_seen_age_secs".into(), serde_json::json!(age));
                }
                p
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&annotated).unwrap_or_default()
        );
        return;
    }
    if peers.is_empty() {
        println!("No peers in the swarm yet.");
        println!("Hint: peers register via 'kannaka swarm join'.");
        if stale_count > 0 {
            println!(
                "({stale_count} stale record(s) retained but not live — 'kannaka swarm peers --all' to see them)"
            );
        }
        return;
    }
    println!();
    println!(
        "{:<24} {:<8} {:<8} {}",
        "AGENT", "MEMS", "VERSION", "CAPABILITIES"
    );
    println!("{}", "─".repeat(78));
    for p in &peers {
        let agent_raw = p.get("agent_id").and_then(|v| v.as_str()).unwrap_or("?");
        let display_raw = p.get("display_name").and_then(|v| v.as_str()).unwrap_or("");
        let mem = p.get("memory_count").and_then(|v| v.as_u64()).unwrap_or(0);
        let ver_raw = p
            .get("kannaka_version")
            .and_then(|v| v.as_str())
            .unwrap_or("?");
        let caps_obj = p.get("capabilities").and_then(|v| v.as_object());
        let caps_raw = caps_obj
            .map(|o| {
                o.iter()
                    .filter_map(|(k, v)| {
                        if v.as_bool() == Some(true) {
                            Some(k.as_str())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .unwrap_or_default();
        // SECURITY (increment-0): every field here is attacker-controllable
        // wire data on the open swarm — sanitize before printing (strip
        // ANSI/control bytes) and flag any peer whose id is not on the
        // trusted allowlist as observe-only (` (unverified)`). Matching is on
        // the raw id; our own id is always trusted.
        let agent = kannaka_memory::sanitize_display(agent_raw);
        let display = kannaka_memory::sanitize_display(display_raw);
        let ver = kannaka_memory::sanitize_display(ver_raw);
        let caps = kannaka_memory::sanitize_display(&caps_raw);
        let mut label = if display.is_empty() || display == agent {
            agent.clone()
        } else {
            format!("{display} ({agent})")
        };
        // Optional identity block (swarm agent identity, step 2): agents
        // that joined while logged in via `kannaka identity` carry
        // {user_id, email} in their presence record — show the email.
        if let Some(email) = kannaka_memory::nats::AnnounceIdentity::email_from(p) {
            label = format!("{} <{}>", label, kannaka_memory::sanitize_display(email));
        }
        let trusted = agent_raw == cfg.agent.id
            || kannaka_memory::agent_matches_allowlist(agent_raw, &cfg.swarm_trust.trusted_agents);
        if !trusted {
            label.push_str(" (unverified)");
        }
        // Only reachable under --all: the default view has already dropped
        // these. Say how long ago the node was last heard from so a stale row
        // can never be mistaken for a live one.
        if !kannaka_memory::nats::is_fresh_presence(p, now) {
            match kannaka_memory::nats::presence_age_secs(p, now) {
                Some(age) => label.push_str(&format!(" (stale {})", human_age(age))),
                None => label.push_str(" (stale)"),
            }
        }
        println!("{label:<24} {mem:<8} {ver:<8} {caps}");
    }
    println!();
    if show_all {
        println!("{} peers ({} stale)", peers.len(), stale_count);
    } else {
        println!("{} live peers", peers.len());
        if stale_count > 0 {
            println!(
                "{stale_count} stale record(s) hidden — 'kannaka swarm peers --all' to see them"
            );
        }
    }
}

/// Render a `last_seen` age as a compact human span (`45s`, `12m`, `3h`, `2d`).
#[cfg(feature = "nats")]
fn human_age(secs: i64) -> String {
    // A negative age means the peer's clock runs ahead of ours; report it as
    // "just now" rather than a nonsense "-3h".
    let s = secs.max(0);
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else if s < 86_400 {
        format!("{}h", s / 3600)
    } else {
        format!("{}d", s / 86_400)
    }
}

#[cfg(not(feature = "nats"))]
pub(crate) fn handle_swarm_peers(_: &KannakaConfig, _: &[String]) {
    eprintln!("swarm peers requires the 'nats' feature");
    process::exit(1);
}

// ── ADR-0026 Phase 6: Auto-absorb (#76) ─────────────────────────────────────
// One-shot autonomous sweep with rate limits + anti-drift safety. Designed
// to be invoked from cron every 30 min (or in-process loop) so a node that
// stays online keeps absorbing fresh exemplars from peers.

#[cfg(feature = "nats")]
pub(crate) fn handle_swarm_autoabsorb(
    sys: &mut kannaka_memory::openclaw::KannakaMemorySystem,
    cfg: &KannakaConfig,
    args: &[String],
) {
    const USAGE: &str = "Usage: kannaka swarm autoabsorb [--threshold 0.4] [--per-source-daily-cap N] [--max-phi-drop X] [--dry-run] [--nats-url URL]";
    // Usage: kannaka swarm autoabsorb [--threshold 0.4] [--per-source-daily-cap N] [--dry-run]
    let mut threshold: f32 = 0.4;
    let mut per_source_daily_cap: usize = 10;
    let mut dry_run = false;
    let mut max_phi_drop: f32 = 0.05;
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--threshold" => {
                threshold = parse_flag_value(args, i, "--threshold", USAGE);
                i += 2;
            }
            "--per-source-daily-cap" => {
                per_source_daily_cap = parse_flag_value(args, i, "--per-source-daily-cap", USAGE);
                i += 2;
            }
            "--max-phi-drop" => {
                max_phi_drop = parse_flag_value(args, i, "--max-phi-drop", USAGE);
                i += 2;
            }
            "--dry-run" => {
                dry_run = true;
                i += 1;
            }
            "--nats-url" => {
                let _ = flag_value(args, i, "--nats-url", USAGE);
                i += 2;
            }
            other => {
                warn_unknown_flag("autoabsorb", other);
                i += 1;
            }
        }
    }
    if !dry_run {
        super::warn_if_readonly("swarm autoabsorb");
    }

    // Beside the ACTIVE store (#769): the per-source caps and phi-drop pause
    // guard THIS memory universe, so they travel with it.
    let state_path = store_dir(cfg).join("autoabsorb-state.json");
    let mut state = AutoabsorbState::load(&state_path);
    let today_key = chrono::Utc::now().format("%Y-%m-%d").to_string();
    state.purge_old_days(&today_key);

    // Anti-drift safety: compare current Phi to the snapshot the last sweep
    // recorded. If Phi has dropped > max_phi_drop, pause autonomous absorb.
    let current_phi = sys.assess().phi;
    if let Some(prev) = state.last_phi {
        let drop = prev - current_phi;
        if drop > max_phi_drop {
            eprintln!(
                "[autoabsorb] PAUSED: Phi dropped {prev:.3} → {current_phi:.3} (Δ={drop:.3} > {max_phi_drop:.3})"
            );
            eprintln!(
                "[autoabsorb] manual intervention required: review recent absorbs in {}",
                state_path.display()
            );
            return;
        }
    }

    let nats_url = resolve_nats_url(args, 0, &cfg.swarm.nats_url);
    let transport = match kannaka_memory::nats::SwarmTransport::connect(&nats_url) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("[autoabsorb] nats: {e}");
            return;
        }
    };

    let exemplars = match transport.get_exemplars(None) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("[autoabsorb] get_exemplars: {e}");
            return;
        }
    };
    if exemplars.is_empty() {
        eprintln!("[autoabsorb] no exemplars in stream — nothing to do");
        state.last_phi = Some(current_phi);
        let _ = state.save(&state_path);
        return;
    }

    // Sort by amplitude desc.
    let mut ordered = exemplars;
    ordered.sort_by(|a, b| {
        let aa = a.get("amplitude").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let bb = b.get("amplitude").and_then(|v| v.as_f64()).unwrap_or(0.0);
        bb.partial_cmp(&aa).unwrap_or(std::cmp::Ordering::Equal)
    });

    let my_id = &cfg.agent.id;
    let mut absorbed = 0usize;
    let mut skipped_self = 0usize;
    let mut skipped_capped = 0usize;
    let mut skipped_resonant = 0usize;
    let mut skipped_low = 0usize;

    // inc-1b corroboration admit() chokepoint state. DORMANT unless the gate is
    // enabled AND seeds are pinned (then it governs each absorb below).
    let gate_dir = store_dir(cfg);
    let mut rep_store = kannaka_memory::reputation::RepStore::load(&gate_dir, &cfg.swarm_trust);
    let mut staging = kannaka_memory::absorb_gate::QuarantineStaging::load(&gate_dir);

    for e in ordered.iter() {
        let source = e
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string();
        if source == *my_id {
            skipped_self += 1;
            continue;
        }

        // Per-source per-day cap.
        let used_today = state.absorbs_today(&today_key, &source);
        if used_today >= per_source_daily_cap {
            skipped_capped += 1;
            continue;
        }

        let content = e.get("content").and_then(|v| v.as_str()).unwrap_or("");
        if content.is_empty() || content.len() < 30 {
            skipped_low += 1;
            continue;
        }
        let amp = e.get("amplitude").and_then(|v| v.as_f64()).unwrap_or(0.0);
        if amp < 0.3 {
            skipped_low += 1;
            continue;
        }

        // Local resonance — only absorb if the medium DOESN'T already have
        // strong resonance (i.e. it's novel material).
        let res = sys.recall(content, 1).ok().unwrap_or_default();
        let top_strength = res.first().map(|r| r.strength).unwrap_or(0.0);
        if top_strength >= threshold {
            skipped_resonant += 1;
            continue;
        }

        let cluster_id = e.get("cluster_id").and_then(|v| v.as_u64()).unwrap_or(0);
        eprintln!(
            "[autoabsorb] absorb from {source} c{cluster_id} amp={amp:.3} resonance={top_strength:.3}"
        );

        if !dry_run {
            // inc-1b: route through the corroboration admit() chokepoint. DORMANT
            // ⇒ admit returns Live (current ungated behaviour) with a sanitized
            // amplitude; ACTIVE ⇒ the decision governs.
            let memory_id = e
                .get("exemplar_id")
                .and_then(|v| v.as_str())
                .and_then(|s| uuid::Uuid::parse_str(s).ok())
                .unwrap_or_else(uuid::Uuid::new_v4);
            let prov_sig: Option<kannaka_memory::ProvenanceSig> = e
                .get("provenance_sig")
                .and_then(|v| serde_json::from_value(v.clone()).ok());
            let wire_phase = e.get("phase").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
            let wire_freq = e.get("frequency").and_then(|v| v.as_f64()).unwrap_or(0.1) as f32;
            let now = kannaka_memory::provenance::now_ms();
            let (decision, clean, pending) = kannaka_memory::admit(
                content,
                amp as f32,
                wire_phase,
                wire_freq,
                false,
                kannaka_memory::SUBJECT_EXEMPLAR,
                memory_id,
                prov_sig.as_ref(),
                &mut staging,
                &mut rep_store,
                cfg,
                now,
            );
            use kannaka_memory::AdmitDecision::*;
            match decision {
                Live => {
                    let category = format!("swarm:{source}");
                    match sys.remember_with_category(content, &category, clean.amplitude as f64) {
                        Ok(id) => {
                            // #8: commit the pending promotion ONLY after the medium
                            // write succeeds (no-op when dormant / non-Live).
                            kannaka_memory::commit_promotion(
                                pending,
                                &mut rep_store,
                                &mut staging,
                                cfg,
                            );
                            eprintln!("[autoabsorb]   remembered {id}");
                            state.record_absorb(&today_key, &source);
                            absorbed += 1;
                        }
                        // #8: write failed — drop the pending, do not commit.
                        Err(e) => eprintln!("[autoabsorb]   remember failed: {e}"),
                    }
                }
                Quarantine | ProbationLive => {
                    eprintln!("[autoabsorb]   quarantined (awaiting corroboration)");
                }
                Drop => {
                    eprintln!("[autoabsorb]   dropped (invalid signature / echo)");
                }
            }
        } else {
            // #688: a rehearsal counts what WOULD absorb but must not burn
            // the per-source daily quota — record_absorb here made a
            // --dry-run sweep change later autonomous behavior.
            absorbed += 1;
        }
    }

    // #688: state mutations (quota counters, last_phi) persist only for a
    // real sweep. A dry run leaves autoabsorb-state.json untouched.
    if !dry_run {
        state.last_phi = Some(current_phi);
        if let Err(e) = state.save(&state_path) {
            eprintln!("[autoabsorb] state save failed: {e}");
        }
    }

    eprintln!(
        "[autoabsorb] sweep complete{}: +{} absorbed (self={} capped={} resonant={} low={})",
        if dry_run { " (DRY RUN)" } else { "" },
        absorbed,
        skipped_self,
        skipped_capped,
        skipped_resonant,
        skipped_low
    );
}

#[cfg(not(feature = "nats"))]
pub(crate) fn handle_swarm_autoabsorb(
    _: &mut kannaka_memory::openclaw::KannakaMemorySystem,
    _: &KannakaConfig,
    _: &[String],
) {
    eprintln!("swarm autoabsorb requires the 'nats' feature");
    process::exit(1);
}

/// State persisted across autoabsorb sweeps. Tracks daily absorb counts per
/// source agent and the last seen local Phi (for anti-drift detection).
#[cfg(feature = "nats")]
#[derive(Default, serde::Serialize, serde::Deserialize)]
struct AutoabsorbState {
    /// `{ "2026-04-25": { "kannaka-prime": 3, "agent-x": 1 } }`
    absorbs_per_day: std::collections::HashMap<String, std::collections::HashMap<String, usize>>,
    last_phi: Option<f32>,
}

#[cfg(feature = "nats")]
impl AutoabsorbState {
    fn load(path: &std::path::Path) -> Self {
        if !path.exists() {
            return Self::default();
        }
        match std::fs::read_to_string(path) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }
    fn save(&self, path: &std::path::Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(path, bytes)
    }
    fn absorbs_today(&self, day: &str, source: &str) -> usize {
        self.absorbs_per_day
            .get(day)
            .and_then(|d| d.get(source))
            .copied()
            .unwrap_or(0)
    }
    fn record_absorb(&mut self, day: &str, source: &str) {
        let day_map = self.absorbs_per_day.entry(day.to_string()).or_default();
        *day_map.entry(source.to_string()).or_insert(0) += 1;
    }
    /// Drop entries older than 7 days so the state file doesn't grow unbounded.
    fn purge_old_days(&mut self, today: &str) {
        let cutoff = chrono::NaiveDate::parse_from_str(today, "%Y-%m-%d")
            .ok()
            .map(|d| d - chrono::Duration::days(7))
            .map(|d| d.format("%Y-%m-%d").to_string())
            .unwrap_or_else(|| "1970-01-01".to_string());
        self.absorbs_per_day
            .retain(|k, _| k.as_str() >= cutoff.as_str());
    }
}

// ── ADR-0026 Phase 4: Work queues (#74) ─────────────────────────────────────
//
// Cooperative task processing across the swarm. v1 uses raw NATS queue-group
// subscriptions (not full JetStream consumer groups) — multiple workers
// SUB to `KANNAKA.work.<kind>` with the same queue group; NATS delivers
// each task to exactly one worker. The requester pubs with a reply-to and
// awaits the result via existing request_one. Same wire shape as ask/serve;
// the difference is the queue-group on the worker side.
//
// Supported task kinds:
//   ask  — runs agent::ask_notools_ex on the worker's local HRM.
// Future: dream.deep, batch HRM analysis, TTS pool. Each kind gets its own
// subject + queue group.

#[cfg(feature = "nats")]
pub(crate) fn handle_swarm_enqueue(cfg: &KannakaConfig, args: &[String]) {
    use std::time::Duration;
    const USAGE: &str =
        "Usage: kannaka swarm enqueue <kind> \"payload\" [--timeout SECONDS] [--nats-url URL]";
    // Usage: kannaka swarm enqueue <kind> "payload text" [--timeout 600]
    let kind = match args.get(2) {
        Some(s) => s.clone(),
        None => {
            eprintln!("{USAGE}");
            process::exit(1);
        }
    };
    let mut timeout_secs: u64 = 600;
    let mut text_parts: Vec<String> = Vec::new();
    let mut i = 3;
    while i < args.len() {
        match args[i].as_str() {
            "--timeout" => {
                timeout_secs = parse_flag_value(args, i, "--timeout", USAGE);
                i += 2;
            }
            "--nats-url" => {
                let _ = flag_value(args, i, "--nats-url", USAGE);
                i += 2;
            }
            other if other.starts_with("--") => {
                // Payload-collecting handler: a typo'd flag must not silently
                // become part of the task text.
                eprintln!("swarm enqueue: unknown flag: {other}");
                eprintln!("{USAGE}");
                process::exit(2);
            }
            _ => {
                text_parts.push(args[i].clone());
                i += 1;
            }
        }
    }
    let text = text_parts.join(" ").trim().to_string();
    if text.is_empty() {
        eprintln!("{USAGE}");
        process::exit(1);
    }

    let nats_url = resolve_nats_url(args, 0, &cfg.swarm.nats_url);
    let transport = match kannaka_memory::nats::SwarmTransport::connect(&nats_url) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("nats: {e}");
            process::exit(1);
        }
    };

    let task_id = format!("t-{}", uuid::Uuid::new_v4().simple());
    let payload = serde_json::json!({
        "task_id": task_id,
        "from": cfg.agent.id,
        "text": text,
    });
    let bytes = serde_json::to_vec(&payload).unwrap();
    let subject = format!("KANNAKA.work.{kind}");
    eprintln!(
        "[enqueue] {subject} task {task_id} (waiting up to {timeout_secs}s for a worker reply)"
    );

    match transport.request_one(&subject, &bytes, Duration::from_secs(timeout_secs)) {
        Ok(reply) => {
            let parsed: serde_json::Value = serde_json::from_slice(&reply).unwrap_or_else(
                |_| serde_json::json!({"raw": String::from_utf8_lossy(&reply).to_string()}),
            );
            let from = parsed.get("from").and_then(|v| v.as_str()).unwrap_or("?");
            eprintln!("[enqueue] reply from {from}");
            if let Some(err) = parsed.get("error").and_then(|v| v.as_str()) {
                eprintln!("[enqueue] worker error: {err}");
                if let Some(tid) = parsed.get("task_id").and_then(|v| v.as_str()) {
                    eprintln!("[enqueue] task_id: {tid}");
                }
                process::exit(1);
            }
            let text = parsed
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("(no text)");
            println!("{text}");
        }
        Err(e) => {
            eprintln!("enqueue: {e}");
            process::exit(1);
        }
    }
}

#[cfg(not(feature = "nats"))]
pub(crate) fn handle_swarm_enqueue(_: &KannakaConfig, _: &[String]) {
    eprintln!("swarm enqueue requires the 'nats' feature");
    process::exit(1);
}

#[cfg(feature = "nats")]
pub(crate) fn handle_swarm_worker(
    sys: &mut kannaka_memory::openclaw::KannakaMemorySystem,
    cfg: &KannakaConfig,
    args: &[String],
) {
    use std::time::Duration;
    const USAGE: &str = "Usage: kannaka swarm worker [--kinds ask,dream,...] [--queue-group GROUP] [--nats-url URL]";
    // Usage: kannaka swarm worker [--kinds ask,dream,...] [--queue-group GROUP]
    let mut kinds: Vec<String> = vec!["ask".to_string()];
    let mut queue_group: String = "kannaka_workers".to_string();
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--kinds" => {
                let v = flag_value(args, i, "--kinds", USAGE);
                kinds = v
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                i += 2;
            }
            "--queue-group" => {
                queue_group = flag_value(args, i, "--queue-group", USAGE).to_string();
                i += 2;
            }
            "--nats-url" => {
                let _ = flag_value(args, i, "--nats-url", USAGE);
                i += 2;
            }
            other => {
                warn_unknown_flag("worker", other);
                i += 1;
            }
        }
    }
    if kinds.is_empty() {
        kinds.push("ask".to_string());
    }

    let nats_url = resolve_nats_url(args, 0, &cfg.swarm.nats_url);

    eprintln!("[worker] kinds: {kinds:?}, queue group: {queue_group}");

    if kinds.len() == 1 {
        let kind = &kinds[0];
        let transport = match kannaka_memory::nats::SwarmTransport::connect(&nats_url) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("[worker] nats: {e}");
                process::exit(1);
            }
        };
        let subject = format!("KANNAKA.work.{kind}");
        let group = format!("{queue_group}_{kind}");
        let mut sub = match transport.subscribe_with_queue(&subject, Some(&group)) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[worker] subscribe: {e}");
                process::exit(1);
            }
        };
        eprintln!("[worker] subscribed to {subject} (group {group})");
        loop {
            match sub.next_event() {
                SubEvent::Msg(msg) => {
                    _process_work_msg(sys, cfg, &transport, &nats_url, kind, &msg);
                }
                SubEvent::Timeout => {}
                SubEvent::Closed => {
                    // Pre-fix this exited 0 on connection close, so
                    // Restart=on-failure units never brought the worker back.
                    eprintln!("[worker] subscription {subject} closed — exiting for restart");
                    process::exit(1);
                }
            }
        }
    } else {
        // Multi-kind worker: ONE durable subscription per kind, each on a
        // dedicated connection (the transport documents a one-subscription-
        // per-connection model), polled round-robin with short timeouts.
        // Pre-fix this re-subscribed every ~5s on the same connection,
        // leaking server-side subscriptions and discarding bytes buffered
        // in each dropped reader.
        let mut subs: Vec<(
            String,
            kannaka_memory::nats::SwarmTransport,
            kannaka_memory::nats::NatsSubscription,
        )> = Vec::new();
        for kind in &kinds {
            let transport = match kannaka_memory::nats::SwarmTransport::connect(&nats_url) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("[worker] nats connect for kind {kind}: {e}");
                    process::exit(1);
                }
            };
            let subject = format!("KANNAKA.work.{kind}");
            let group = format!("{queue_group}_{kind}");
            let sub = match transport.subscribe_with_queue(&subject, Some(&group)) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("[worker] subscribe {kind}: {e}");
                    process::exit(1);
                }
            };
            let _ = sub.set_timeout(Some(Duration::from_millis(500)));
            eprintln!("[worker] subscribed to {subject} (group {group})");
            subs.push((kind.clone(), transport, sub));
        }
        loop {
            for (kind, transport, sub) in subs.iter_mut() {
                match sub.next_event() {
                    SubEvent::Msg(msg) => {
                        _process_work_msg(sys, cfg, transport, &nats_url, kind, &msg);
                    }
                    SubEvent::Timeout => {}
                    SubEvent::Closed => {
                        eprintln!("[worker] subscription KANNAKA.work.{kind} closed — exiting for restart");
                        process::exit(1);
                    }
                }
            }
        }
    }
}

#[cfg(feature = "nats")]
fn _process_work_msg(
    sys: &mut kannaka_memory::openclaw::KannakaMemorySystem,
    cfg: &KannakaConfig,
    transport: &kannaka_memory::nats::SwarmTransport,
    nats_url: &str,
    kind: &str,
    msg: &kannaka_memory::nats::NatsMessage,
) {
    let reply_to = match &msg.reply_to {
        Some(r) => r.clone(),
        None => {
            eprintln!("[worker] task without reply-to on {} — drop", msg.subject);
            return;
        }
    };
    let req: serde_json::Value = match serde_json::from_slice(&msg.payload) {
        Ok(v) => v,
        Err(e) => {
            let err = serde_json::json!({"from": cfg.agent.id, "error": format!("bad json: {e}")});
            let _ = transport.reply(&reply_to, err.to_string().as_bytes());
            return;
        }
    };
    let task_id = req
        .get("task_id")
        .and_then(|v| v.as_str())
        .unwrap_or("(none)");
    let from = req.get("from").and_then(|v| v.as_str()).unwrap_or("?");
    let text = req.get("text").and_then(|v| v.as_str()).unwrap_or("");
    eprintln!("[worker] kind={kind} task={task_id} from={from}");

    let reply_payload = match kind {
        "ask" => {
            if text.is_empty() {
                serde_json::json!({"from": cfg.agent.id, "error": "empty text"})
            } else {
                match kannaka_memory::agent::ask_notools_ex(sys, cfg, text, None) {
                    Ok(r) => {
                        serde_json::json!({"from": cfg.agent.id, "task_id": task_id, "text": r.text})
                    }
                    Err(e) => {
                        serde_json::json!({"from": cfg.agent.id, "task_id": task_id, "error": format!("{e}")})
                    }
                }
            }
        }
        other => {
            serde_json::json!({"from": cfg.agent.id, "error": format!("unknown kind: {other}")})
        }
    };

    // Reply on a fresh connection (the original may have idled past PING).
    let body = reply_payload.to_string();
    let reply_result = match kannaka_memory::nats::SwarmTransport::connect(nats_url) {
        Ok(fresh) => fresh.reply(&reply_to, body.as_bytes()),
        Err(_) => transport.reply(&reply_to, body.as_bytes()),
    };
    if let Err(e) = reply_result {
        eprintln!("[worker] reply failed: {e}");
    } else {
        eprintln!("[worker] replied on {reply_to}");
    }
}

#[cfg(not(feature = "nats"))]
pub(crate) fn handle_swarm_worker(
    _: &mut kannaka_memory::openclaw::KannakaMemorySystem,
    _: &KannakaConfig,
    _: &[String],
) {
    eprintln!("swarm worker requires the 'nats' feature");
    process::exit(1);
}

/// `kannaka swarm tail` — subscribe to the constellation pulse and emit
/// one NDJSON line per inbound NATS message. With credentials (NATS_USER
/// or user:pass in the URL) the default subject set is the broad
/// `QUEEN.>`, `KANNAKA.>`, `RADIO.>`, `KAX.>`, `EYE.>`. Without
/// credentials the swarm server's anonymous user (ADR-0026 #73 public
/// read-only mirror) denies those wildcards at SUB time — the whole
/// subscription yields nothing, not just the disallowed subjects — so
/// the anonymous default is the curated anon-visible set instead.
/// `--subject` repeats override either default.
///
/// Output format (one line per message):
///   {"ts": <unix-ms>, "subject": "<subj>", "payload": <json-or-string>}
///
/// This is the streaming source the TUI Bus tab consumes — running it
/// from a shell is also useful: `kannaka swarm tail | grep consciousness`.
#[cfg(feature = "nats")]
pub(crate) fn handle_swarm_tail(cfg: &KannakaConfig, args: &[String]) {
    use std::io::Write;
    const USAGE: &str = "Usage: kannaka swarm tail [--subject SUBJ ...] [--nats-url URL]";

    let mut subjects: Vec<String> = Vec::new();
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--subject" => {
                subjects.push(flag_value(args, i, "--subject", USAGE).to_string());
                i += 2;
            }
            "--nats-url" => {
                let _ = flag_value(args, i, "--nats-url", USAGE);
                i += 2;
            }
            other => {
                warn_unknown_flag("tail", other);
                i += 1;
            }
        }
    }
    let nats_url = resolve_nats_url(args, 0, &cfg.swarm.nats_url);
    if subjects.is_empty() {
        let has_creds = std::env::var("NATS_USER")
            .map(|u| !u.is_empty())
            .unwrap_or(false)
            || nats_url.contains('@');
        let defaults: &[&str] = if has_creds {
            &["QUEEN.>", "KANNAKA.>", "RADIO.>", "KAX.>", "EYE.>"]
        } else {
            // Anon-visible set, mirroring the server's anonymous subscribe
            // allowlist. A broad wildcard here would be denied wholesale.
            &[
                "QUEEN.>",
                "KANNAKA.activity.>",
                "KANNAKA.events.>",
                "KANNAKA.consciousness",
                "KANNAKA.dreams",
                "KANNAKA.exemplar.>",
                "KANNAKA.presence.>",
            ]
        };
        for s in defaults {
            subjects.push(s.to_string());
        }
    }

    eprintln!(
        "[tail] connecting to {nats_url} — subjects: {subjects:?}"
    );

    // One dedicated transport per subject — the transport documents a
    // one-subscription-per-connection model, so the cleanest way to
    // multiplex is one TCP socket per wildcard. Five sockets is cheap.
    let stdout_mu = std::sync::Arc::new(std::sync::Mutex::new(()));
    let mut handles = Vec::new();
    for subj in subjects {
        let url = nats_url.clone();
        let mu = std::sync::Arc::clone(&stdout_mu);
        handles.push(std::thread::spawn(move || loop {
            let transport = match kannaka_memory::nats::SwarmTransport::connect(&url) {
                Ok(t) => t,
                Err(e) => {
                    let _ = writeln!(
                        std::io::stderr(),
                        "[tail] {subj} connect failed: {e} (retry 5s)"
                    );
                    std::thread::sleep(std::time::Duration::from_secs(5));
                    continue;
                }
            };
            let mut sub = match transport.subscribe(&subj) {
                Ok(s) => s,
                Err(e) => {
                    let _ = writeln!(
                        std::io::stderr(),
                        "[tail] {subj} subscribe failed: {e} (retry 5s)"
                    );
                    std::thread::sleep(std::time::Duration::from_secs(5));
                    continue;
                }
            };
            // Block indefinitely; Ctrl+C terminates the process.
            let _ = sub.set_timeout(None);
            eprintln!("[tail] {subj} subscribed");
            loop {
                match sub.next_event() {
                    SubEvent::Msg(msg) => {
                        let payload_str = std::str::from_utf8(&msg.payload).unwrap_or("<binary>");
                        // SECURITY (increment-0): subject + payload are wire
                        // data on the open swarm. A valid-JSON payload is
                        // re-escaped by serde on output (so control bytes can't
                        // reach the terminal raw), but the subject and the
                        // non-JSON fallback are embedded as bare strings —
                        // sanitize those to strip ANSI/control sequences.
                        let payload_json: serde_json::Value = serde_json::from_str(payload_str)
                            .unwrap_or_else(|_| {
                                serde_json::Value::String(kannaka_memory::sanitize_display(
                                    payload_str,
                                ))
                            });
                        let line = serde_json::json!({
                            "ts": chrono::Utc::now().timestamp_millis(),
                            "subject": kannaka_memory::sanitize_display(&msg.subject),
                            "payload": payload_json,
                        });
                        let _guard = mu.lock();
                        println!("{line}");
                        let _ = std::io::stdout().flush();
                    }
                    // No timeout is set; defensive — keep polling.
                    SubEvent::Timeout => continue,
                    // Connection closed — break to the reconnect path.
                    SubEvent::Closed => break,
                }
            }
            let _ = writeln!(
                std::io::stderr(),
                "[tail] {subj} disconnected — reconnecting in 2s"
            );
            std::thread::sleep(std::time::Duration::from_secs(2));
        }));
    }

    // Block forever — Ctrl+C kills the process. Joining the threads would
    // hang the same way, so just sleep the main thread.
    loop {
        std::thread::sleep(std::time::Duration::from_secs(60));
    }
}

#[cfg(not(feature = "nats"))]
pub(crate) fn handle_swarm_tail(_: &KannakaConfig, _: &[String]) {
    eprintln!("swarm tail requires the 'nats' feature");
    std::process::exit(1);
}

// ── #563: swarm-serve hardness helpers ──────────────────────

/// Pure freshness decision (unit-testable without a filesystem). Given the
/// mtime the HRM had when this daemon loaded it, the mtime observed now, and
/// the pending observation (a changed mtime we're waiting to settle), decide
/// the next pending state and whether to restart-to-reload.
///
/// A change only triggers a reload after it has been observed unchanged for
/// `settle` — so a writer mid-flush (or a multi-step dream write) never gets
/// loaded half-finished. A change that keeps changing keeps re-arming.
#[cfg(feature = "nats")]
fn freshness_decision(
    loaded: std::time::SystemTime,
    current: std::time::SystemTime,
    pending: Option<(std::time::SystemTime, std::time::Instant)>,
    now: std::time::Instant,
    settle: std::time::Duration,
) -> (Option<(std::time::SystemTime, std::time::Instant)>, bool) {
    if current == loaded {
        // Back to the loaded state (or never changed) — disarm.
        return (None, false);
    }
    match pending {
        Some((seen, since)) if seen == current => {
            if now.duration_since(since) >= settle {
                (pending, true) // changed AND stable long enough — reload
            } else {
                (pending, false) // changed, still settling
            }
        }
        // First sighting of this mtime (or it moved again mid-settle): re-arm.
        _ => (Some((current, now)), false),
    }
}

/// Minimal sd_notify(3) — one datagram to $NOTIFY_SOCKET. Zero dependencies
/// (musl-static friendly); silently a no-op when the socket is unset (dev
/// shells) or on non-unix targets.
#[cfg(all(feature = "nats", unix))]
fn sd_notify(state: &str) {
    if let Ok(path) = std::env::var("NOTIFY_SOCKET") {
        if path.is_empty() {
            return;
        }
        // Abstract-namespace sockets ('@...') are not used by systemd for
        // NOTIFY_SOCKET on our targets; treat path as filesystem.
        if let Ok(sock) = std::os::unix::net::UnixDatagram::unbound() {
            let _ = sock.send_to(state.as_bytes(), &path);
        }
    }
}

#[cfg(all(feature = "nats", not(unix)))]
fn sd_notify(_state: &str) {}

#[cfg(all(test, feature = "nats"))]
mod serve_hardness_tests {
    use super::freshness_decision;
    use std::time::{Duration, Instant, SystemTime};

    #[test]
    fn freshness_transitions() {
        let settle = Duration::from_secs(20);
        let t0 = SystemTime::UNIX_EPOCH;
        let t1 = t0 + Duration::from_secs(100); // first rewrite
        let t2 = t0 + Duration::from_secs(200); // second rewrite mid-settle
        let now = Instant::now();

        // Unchanged → disarmed, no reload.
        assert_eq!(freshness_decision(t0, t0, None, now, settle), (None, false));

        // First sighting of a change → arm, no reload yet.
        let (p, reload) = freshness_decision(t0, t1, None, now, settle);
        assert_eq!(p.map(|(m, _)| m), Some(t1));
        assert!(!reload);

        // Same change, settle not elapsed → hold.
        let (p2, reload) = freshness_decision(t0, t1, p, now + Duration::from_secs(5), settle);
        assert_eq!(p2.map(|(m, _)| m), Some(t1));
        assert!(!reload);

        // Same change, settle elapsed → reload.
        let (_, reload) = freshness_decision(t0, t1, p, now + Duration::from_secs(25), settle);
        assert!(reload);

        // Change moved again mid-settle → re-arm on the new mtime, no reload.
        let (p3, reload) = freshness_decision(t0, t2, p, now + Duration::from_secs(25), settle);
        assert_eq!(p3.map(|(m, _)| m), Some(t2));
        assert!(!reload);

        // File restored to the loaded mtime (e.g. snapshot rollback) → disarm.
        assert_eq!(
            freshness_decision(t0, t0, p, now + Duration::from_secs(25), settle),
            (None, false)
        );
    }
}

#[cfg(all(test, feature = "nats"))]
mod neighbors_tests {
    use super::neighbors_top_k;

    /// Absent `top_k` uses the documented default rather than 0 (which would
    /// return nothing and look like an empty memory).
    #[test]
    fn absent_top_k_defaults_to_ten() {
        assert_eq!(neighbors_top_k(&serde_json::json!({ "query": "x" })), 10);
    }

    /// The value comes straight off the NATS wire and drives allocation, a
    /// per-result `store.get` and JSON serialization, so an unbounded one is an
    /// OOM lever on the 1-vCPU hub. Same cap recall and `cores` already apply.
    #[test]
    fn hostile_top_k_is_clamped() {
        assert_eq!(
            neighbors_top_k(&serde_json::json!({ "top_k": 4_000_000_000u64 })),
            100
        );
    }

    /// Zero would make a well-formed request return nothing at all; floor it so
    /// a caller always gets the answer it asked a question to get.
    #[test]
    fn zero_top_k_floors_to_one() {
        assert_eq!(neighbors_top_k(&serde_json::json!({ "top_k": 0 })), 1);
    }

    /// A sane value passes through untouched.
    #[test]
    fn in_range_top_k_passes_through() {
        assert_eq!(neighbors_top_k(&serde_json::json!({ "top_k": 25 })), 25);
    }

    /// A non-numeric `top_k` falls back to the default instead of failing the
    /// whole request — the query is still answerable.
    #[test]
    fn non_numeric_top_k_falls_back_to_default() {
        assert_eq!(neighbors_top_k(&serde_json::json!({ "top_k": "lots" })), 10);
    }
}
