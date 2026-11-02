//! `kannaka agent --json` — the coding-agent harness backend.
//!
//! Runs an agentic tool-calling loop (LLM → tool_use → execute → tool_result
//! → repeat) over the coding toolset (`coding_tools`) plus a curated slice of
//! the memory tools (recall/remember/status — HRM grounding for free), and
//! speaks a line-delimited JSON protocol so a frontend (kannaka-tui) can
//! render the transcript and gate filesystem/shell mutations behind human
//! approval.
//!
//! ## Protocol
//! stdin  (frontend → here), one JSON object per line:
//!   {"type":"user","text":"..."}                      start/continue a turn
//!   {"type":"approval","id":"<tool_use_id>","decision":"allow|allow_always|deny"}
//!   {"type":"mode","mode":"default|auto-edit|plan|yolo"}   change permission mode
//!   {"type":"exit"}                                    shut down
//!   (a bare non-JSON line is treated as {"type":"user","text":<line>})
//!
//! stdout (here → frontend), NDJSON:
//!   {"kind":"ready","cwd":..,"model":..,"mode":..,"tools":[..]}
//!   {"kind":"text","text":..}                          assistant prose
//!   {"kind":"tool_use","id":..,"name":..,"input":{..},"read_only":bool,"danger":bool}
//!   {"kind":"approval_required","id":..,"name":..,"summary":..,"danger":bool}
//!   {"kind":"tool_result","id":..,"name":..,"content":..,"is_error":bool}
//!   {"kind":"usage","input":N,"output":N}
//!   {"kind":"done","reason":"end_turn|max_iterations|denied|error"}
//!   {"kind":"error","text":..}

use kannaka_memory::agent::{self, AgentError, ContentBlock, Message, Tool};
use kannaka_memory::coding_tools::{self, ToolCtx};
use kannaka_memory::lab_tools;
use kannaka_memory::openclaw::KannakaMemorySystem;
use kannaka_memory::quantum_tools;
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::path::PathBuf;

use super::KannakaConfig;

const MAX_ITERATIONS: usize = 50;
const AGENT_MAX_TOKENS: u32 = 8192;
/// Known-current model the agent falls back to if the configured model is
/// rejected by the API (e.g. a stale snapshot id) — keeps the harness usable
/// out of the box without editing the user's config.
const FALLBACK_MODEL: &str = "claude-sonnet-4-5";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Default,
    AutoEdit,
    Plan,
    Yolo,
}

impl Mode {
    fn parse(s: &str) -> Mode {
        match s {
            "auto-edit" | "auto" => Mode::AutoEdit,
            "plan" => Mode::Plan,
            "yolo" | "danger" => Mode::Yolo,
            _ => Mode::Default,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Mode::Default => "default",
            Mode::AutoEdit => "auto-edit",
            Mode::Plan => "plan",
            Mode::Yolo => "yolo",
        }
    }
}

enum Decision {
    Allow,
    Ask,
    Deny(&'static str),
}

/// Memory tools we expose to the coding agent: read/grounding ops only, no
/// medium-mutating `dream`. These never touch the user's filesystem, so they
/// are always auto-allowed.
fn memory_tools() -> Vec<Tool> {
    agent::canonical_tools()
        .into_iter()
        .filter(|t| matches!(t.name, "recall" | "remember" | "status" | "list_clusters"))
        .collect()
}

fn is_memory_tool(name: &str) -> bool {
    matches!(
        name,
        "recall" | "remember" | "status" | "list_clusters" | "observe" | "dream"
    )
}

/// Decide whether a tool call may run. Read-only coding tools and all memory
/// tools run freely; filesystem/shell mutations follow the permission mode.
fn decide(
    mode: Mode,
    name: &str,
    allowlist: &std::collections::HashSet<String>,
    key: &str,
) -> Decision {
    if coding_tools::is_read_only(name)
        || is_memory_tool(name)
        || quantum_tools::is_quantum_tool(name)
        || lab_tools::is_lab_readonly_tool(name)
    {
        // Quantum tools run on qBraid, not the local machine — never gated.
        // Lab read-only tools (credits/list/status) cost nothing and mutate
        // nothing, so they're auto-allowed too.
        return Decision::Allow;
    }
    if lab_tools::is_lab_tool(name) {
        if lab_tools::is_lab_paid_tool(name) {
            // Real money, open-ended per-minute billing: NEVER auto-approve —
            // not in yolo, and not via a persisted allowlist entry. Every paid
            // launch needs a fresh human OK (plan refuses outright). This is the
            // one class of action where yolo does not blanket-approve.
            return match mode {
                Mode::Plan => {
                    Decision::Deny("plan mode — propose the action instead of running it")
                }
                _ => Decision::Ask,
            };
        }
        // Free lab mutations (env/kernel lifecycle, stop/down). These must NOT
        // ride the coding-edit auto-approve path; gate them like edits: ask
        // (unless allowlisted), deny in plan, allow in yolo.
        return match mode {
            Mode::Yolo => Decision::Allow,
            Mode::Plan => Decision::Deny("plan mode — propose the action instead of running it"),
            _ => {
                if allowlist.contains(key) {
                    Decision::Allow
                } else {
                    Decision::Ask
                }
            }
        };
    }
    // name is now a mutating coding tool: write_file | edit_file | bash
    match mode {
        Mode::Yolo => Decision::Allow,
        Mode::Plan => Decision::Deny("plan mode — propose the change instead of applying it"),
        Mode::AutoEdit => {
            if name == "bash" {
                if allowlist.contains(key) {
                    Decision::Allow
                } else {
                    Decision::Ask
                }
            } else {
                Decision::Allow // write_file / edit_file auto-approved
            }
        }
        Mode::Default => {
            if allowlist.contains(key) {
                Decision::Allow
            } else {
                Decision::Ask
            }
        }
    }
}

/// Session-scoped allowlist key. Bash is keyed by exact command (so
/// "allow always" doesn't bless every future command); edits by tool name.
fn allow_key(name: &str, input: &Value) -> String {
    if name == "bash" {
        format!(
            "bash:{}",
            input.get("command").and_then(|v| v.as_str()).unwrap_or("")
        )
    } else if lab_tools::is_lab_paid_tool(name) {
        // Scope "allow always" for paid compute to the specific profile/instance
        // so blessing one launch doesn't bless every future paid launch.
        let target = input
            .get("profile")
            .or_else(|| input.get("instance_id"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        format!("{name}:{target}")
    } else {
        name.to_string()
    }
}

fn tool_summary(name: &str, input: &Value) -> String {
    match name {
        "bash" => input
            .get("command")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        "write_file" | "edit_file" => input
            .get("file_path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        _ if lab_tools::is_lab_tool(name) => {
            // Show the salient target (profile / slug / instance / env) for a
            // readable approval line, plus the spend ceiling for paid tools so
            // the human approves the real authorized amount.
            let target = [
                "profile",
                "slug",
                "instance_id",
                "name",
                "env_id",
                "environment",
                "ssh_alias",
                "session_id",
            ]
            .iter()
            .find_map(|k| input.get(k).and_then(|v| v.as_str()))
            .unwrap_or("");
            let mut s = if target.is_empty() {
                name.to_string()
            } else {
                format!("{name} {target}")
            };
            // lab_exec runs an arbitrary remote command — the human must see
            // WHAT will run, not just where.
            if name == "lab_exec" {
                if let Some(cmd) = input.get("command").and_then(|v| v.as_str()) {
                    s.push_str(&format!(": {cmd}"));
                }
            }
            if lab_tools::is_lab_paid_tool(name) {
                let allow = input
                    .get("allow_spend")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                match input.get("max_credits").and_then(|v| v.as_f64()) {
                    Some(m) => s.push_str(&format!(" [PAID allow_spend={allow} max_credits={m}]")),
                    None => s.push_str(&format!(" [PAID allow_spend={allow} max_credits=unset]")),
                }
            }
            s
        }
        _ => serde_json::to_string(input).unwrap_or_default(),
    }
}

fn coding_system_prompt(cwd: &std::path::Path, mode: Mode) -> String {
    let os = if cfg!(windows) {
        "Windows"
    } else if cfg!(target_os = "macos") {
        "macOS"
    } else {
        "Linux"
    };
    format!(
        "You are Kannaka Agent, a careful, production-grade coding assistant operating \
         inside a terminal harness. You complete software-engineering tasks by calling \
         tools.\n\n\
         Workspace: {cwd}\n\
         OS: {os}\n\
         Permission mode: {mode}\n\n\
         Tools:\n\
         - read_file / glob / grep / list_dir: inspect the workspace (always allowed).\n\
         - write_file / edit_file: create or modify files. ALWAYS read_file a file before \
           editing it, and match old_string exactly.\n\
         - bash: run shell commands (build, test, git). Destructive commands require the \
           human's approval; prefer the dedicated file/search tools where one fits.\n\
         - recall / remember / status / list_clusters: your persistent HRM memory. Use \
           `recall` to surface relevant past context, and `remember` to persist durable \
           insights about this project.\n\
         - quantum_devices / quantum_run / quantum_recall / quantum_random: real quantum \
           computing via qBraid. `quantum_run` executes OpenQASM 3 circuits (free simulator \
           by default; name a QPU device to run on hardware). `quantum_recall` performs \
           resonance recall as amplitude amplification; `quantum_random` gives true quantum \
           entropy.\n\
         - lab_* : manage qBraid Lab infrastructure. Read-only/free: `lab_credits` (balance), \
           `lab_list_profiles` (compute instance types + per-minute cost — check before spending), \
           `lab_list_envs`/`lab_env_info`, `lab_compute_status`, `lab_compute_usage`, \
           `lab_list_instances`. Free mutations: `lab_create_env` (build an environment, packages \
           install during the build), `lab_delete_env`. PAID (bills credits per wall-clock minute): \
           `lab_compute_up`/`lab_compute_down` (the Lab server) and `lab_provision_instance`/\
           `lab_start_instance`/`lab_stop_instance` (on-demand instances). For any PAID tool you \
           MUST call `lab_credits` + `lab_list_profiles` first, then pass allow_spend=true and a \
           max_credits ceiling, and tell the user the burn rate; always stop compute when done. \
           Remote agents (drive a coding agent on cloud compute): provision an instance → \
           `lab_ssh_configure` (returns ssh_alias) → `lab_agent_setup` (injects your API key + a valid \
           model so claude runs autonomously) → `lab_agent_launch` → then DRIVE it with `lab_agent_send` \
           + `lab_agent_read` (launch does not auto-submit the task). `lab_agent_list` shows what's running.\n\n\
         Working style:\n\
         - Investigate before acting: read the relevant files, search for usages.\n\
         - Make the smallest change that solves the task; match the surrounding style.\n\
         - After editing code, build/test it with bash when feasible and fix what breaks.\n\
         - Be concise in prose. When the task is complete, summarize what you did and stop \
           (do not call more tools).\n\
         - If a mutation is denied, adapt — propose an alternative or explain.",
        cwd = cwd.display(),
        os = os,
        mode = mode.label(),
    )
}

/// Track started paid compute so a forgotten, still-billing server/instance
/// can't go unnoticed. Updated after each paid/stop tool result.
fn update_active_compute(
    set: &mut std::collections::HashSet<String>,
    name: &str,
    input: &Value,
    content: &str,
    is_error: bool,
) {
    if is_error {
        return;
    }
    match name {
        "lab_compute_up" => {
            set.insert("Lab server".to_string());
        }
        "lab_compute_down" => {
            set.remove("Lab server");
        }
        "lab_provision_instance" => {
            // The instance id is assigned by the platform → read it from the result.
            if let Some(id) = serde_json::from_str::<Value>(content)
                .ok()
                .as_ref()
                .and_then(|v| v.get("instance_id"))
                .and_then(|v| v.as_str())
            {
                set.insert(format!("instance {id}"));
            }
        }
        "lab_start_instance" => {
            if let Some(id) = input.get("instance_id").and_then(|v| v.as_str()) {
                set.insert(format!("instance {id}"));
            }
        }
        "lab_stop_instance" => {
            if let Some(id) = input.get("instance_id").and_then(|v| v.as_str()) {
                set.remove(&format!("instance {id}"));
            }
        }
        _ => {}
    }
}

/// Emit a prominent reminder if any paid compute is still billing.
fn warn_active_compute(set: &std::collections::HashSet<String>, emit: &impl Fn(Value)) {
    if set.is_empty() {
        return;
    }
    let mut targets: Vec<&str> = set.iter().map(|s| s.as_str()).collect();
    targets.sort_unstable();
    emit(json!({
        "kind": "warning",
        "text": format!(
            "⚠️ ACTIVE PAID COMPUTE still billing per minute: {}. Stop it with lab_compute_down / lab_stop_instance when done.",
            targets.join(", ")
        ),
    }));
}

fn session_path(id: &str) -> Option<PathBuf> {
    let safe: String = id
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if safe.is_empty() {
        return None;
    }
    dirs::home_dir().map(|h| {
        h.join(".kannaka")
            .join("sessions")
            .join(format!("agent-{safe}.json"))
    })
}

/// Entry point dispatched from `kannaka.rs` for the `agent` subcommand.
pub fn handle_agent(sys: &mut KannakaMemorySystem, cfg: &KannakaConfig, args: &[String]) {
    // --- arg parsing (hand-rolled, mirroring the other handlers) ---
    let mut cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut mode = Mode::Default;
    let mut session: Option<String> = None;
    let mut model_override: Option<String> = None;
    let mut no_memory = false;
    let mut no_quantum = false;
    let mut no_lab = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => {} // NDJSON is the only supported mode here; accepted for symmetry
            "--cwd" => {
                if let Some(v) = args.get(i + 1) {
                    cwd = PathBuf::from(v);
                    i += 1;
                }
            }
            "--mode" => {
                if let Some(v) = args.get(i + 1) {
                    mode = Mode::parse(v);
                    i += 1;
                }
            }
            "--yolo" => mode = Mode::Yolo,
            "--plan" => mode = Mode::Plan,
            "--session" => {
                if let Some(v) = args.get(i + 1) {
                    session = Some(v.clone());
                    i += 1;
                }
            }
            "--model" => {
                if let Some(v) = args.get(i + 1) {
                    model_override = Some(v.clone());
                    i += 1;
                }
            }
            "--no-memory-tools" => no_memory = true,
            "--no-quantum" => no_quantum = true,
            "--no-lab" => no_lab = true,
            _ => {}
        }
        i += 1;
    }

    let stdout = std::io::stdout();
    let emit = |v: Value| {
        let mut lock = stdout.lock();
        let _ = writeln!(lock, "{v}");
        let _ = lock.flush();
    };

    // --- build LLM client ---
    let mut cfg_owned = cfg.clone();
    if let Some(m) = model_override {
        cfg_owned.llm.model = m;
    }
    let mut client = match agent::client_from_config(&cfg_owned) {
        Ok(c) => c,
        Err(e) => {
            emit(json!({ "kind": "error", "text": format!("no LLM configured: {e}") }));
            return;
        }
    };
    let mut did_fallback = false;

    // --- build the toolset (coding + optional memory) ---
    let mut tools: Vec<Tool> = coding_tools::coding_tools();
    if !no_memory {
        tools.extend(memory_tools());
    }
    // Quantum tools (run circuits / resonance-recall on qBraid). Available
    // unless explicitly disabled; the bridge surfaces a clear install hint if
    // it isn't present, so exposing them is harmless when unconfigured.
    if !no_quantum {
        tools.extend(quantum_tools::quantum_tools());
    }
    // qBraid Lab / infrastructure tools (manage credits/envs/compute via the
    // same bridge). Paid compute is gated behind allow_spend + approval; the
    // bridge surfaces a clear hint if qbraid_core isn't installed.
    if !no_lab {
        tools.extend(lab_tools::lab_tools());
    }
    let tool_names: Vec<&str> = tools.iter().map(|t| t.name).collect();
    let system = coding_system_prompt(&cwd, mode);

    // --- load session history if requested ---
    let mut messages: Vec<Message> = Vec::new();
    let sess_path = session.as_deref().and_then(session_path);
    if let Some(p) = &sess_path {
        match agent::load_session(p) {
            Ok(h) => messages = h,
            // Starting empty would save over the whole conversation on the
            // first turn (#1128); refuse and leave the file as it is.
            Err(e) => {
                emit(json!({ "kind": "error", "text": format!(
                    "session file {} is unreadable ({e}); left untouched. Move it aside to start a new session.",
                    p.display()
                ) }));
                return;
            }
        }
    }

    let mut tool_ctx = ToolCtx::new(cwd.clone());
    let mut allowlist: std::collections::HashSet<String> = std::collections::HashSet::new();
    // Paid compute (Lab server / on-demand instances) the agent has started and
    // not yet stopped — surfaced as a reminder each turn and on exit so a
    // still-billing resource isn't silently forgotten.
    let mut active_compute: std::collections::HashSet<String> = std::collections::HashSet::new();

    emit(json!({
        "kind": "ready",
        "cwd": cwd.to_string_lossy(),
        "model": cfg_owned.llm.model,
        "mode": mode.label(),
        "tools": tool_names,
        "memories": messages.len(),
    }));

    // --- main stdin loop ---
    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    while let Some(Ok(raw)) = lines.next() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let frame: Value =
            serde_json::from_str(line).unwrap_or_else(|_| json!({ "type": "user", "text": line }));
        match frame.get("type").and_then(|v| v.as_str()).unwrap_or("user") {
            "exit" | "quit" => break,
            "mode" => {
                if let Some(m) = frame.get("mode").and_then(|v| v.as_str()) {
                    mode = Mode::parse(m);
                    emit(json!({ "kind": "mode", "mode": mode.label() }));
                }
                continue;
            }
            "user" => {
                let text = frame
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if text.trim().is_empty() {
                    continue;
                }
                messages.push(Message::user_text(text));
                run_turn(
                    &mut client,
                    &cfg_owned,
                    &mut did_fallback,
                    &system,
                    &tools,
                    &mut messages,
                    sys,
                    &mut tool_ctx,
                    &mut allowlist,
                    &mut active_compute,
                    mode,
                    &mut lines,
                    &emit,
                );
                if let Some(p) = &sess_path {
                    if let Err(e) = agent::save_session(p, &messages) {
                        emit(json!({ "kind": "error", "text": format!(
                            "could not save session {}: {e}", p.display()
                        ) }));
                    }
                }
            }
            _ => continue, // stray approval with no pending request, etc.
        }
    }
    // On shutdown (exit / EOF), make sure any still-billing compute is flagged.
    warn_active_compute(&active_compute, &emit);
}

/// Run the agentic loop for one user turn: repeatedly call the model, execute
/// any requested tools (gating mutations behind approval), and append results
/// until the model stops calling tools.
/// Send one model request, transparently falling back to `FALLBACK_MODEL`
/// (once per session) if the configured model is rejected as not-found.
fn send_with_fallback(
    client: &mut agent::LlmClient,
    cfg: &KannakaConfig,
    did_fallback: &mut bool,
    system: &str,
    messages: &[Message],
    tools: &[Tool],
    emit: &impl Fn(Value),
) -> Result<Value, String> {
    match client.send_with_max(system, messages, tools, AGENT_MAX_TOKENS) {
        Ok(v) => Ok(v),
        Err(e) => {
            let model_404 = matches!(&e, AgentError::Api { status: 404, body }
                if body.contains("model") || body.contains("not_found"));
            if model_404 && !*did_fallback {
                *did_fallback = true;
                let mut c2 = cfg.clone();
                c2.llm.model = FALLBACK_MODEL.to_string();
                match agent::client_from_config(&c2) {
                    Ok(fb) => {
                        *client = fb;
                        emit(json!({
                            "kind": "text",
                            "text": format!("[configured model unavailable — falling back to {FALLBACK_MODEL}]"),
                        }));
                        client
                            .send_with_max(system, messages, tools, AGENT_MAX_TOKENS)
                            .map_err(|e| e.to_string())
                    }
                    Err(e2) => Err(format!("fallback model failed: {e2}")),
                }
            } else {
                Err(e.to_string())
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_turn(
    client: &mut agent::LlmClient,
    cfg: &KannakaConfig,
    did_fallback: &mut bool,
    system: &str,
    tools: &[Tool],
    messages: &mut Vec<Message>,
    sys: &mut KannakaMemorySystem,
    tool_ctx: &mut ToolCtx,
    allowlist: &mut std::collections::HashSet<String>,
    active_compute: &mut std::collections::HashSet<String>,
    mode: Mode,
    lines: &mut std::io::Lines<std::io::StdinLock<'_>>,
    emit: &impl Fn(Value),
) {
    // Remind the user up front if paid compute from a prior turn is still billing.
    warn_active_compute(active_compute, emit);
    let mut iteration = 0usize;
    loop {
        iteration += 1;
        if iteration > MAX_ITERATIONS {
            emit(json!({ "kind": "done", "reason": "max_iterations" }));
            return;
        }
        emit(json!({ "kind": "iteration", "n": iteration }));

        let resp =
            match send_with_fallback(client, cfg, did_fallback, system, messages, tools, emit) {
                Ok(v) => v,
                Err(e) => {
                    emit(json!({ "kind": "error", "text": format!("LLM call failed: {e}") }));
                    emit(json!({ "kind": "done", "reason": "error" }));
                    return;
                }
            };

        if let Some(u) = resp.get("usage") {
            emit(json!({
                "kind": "usage",
                "input": u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                "output": u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
            }));
        }

        let blocks: Vec<ContentBlock> = match resp.get("content").cloned() {
            Some(c) => serde_json::from_value(c).unwrap_or_default(),
            None => Vec::new(),
        };
        if blocks.is_empty() {
            emit(json!({ "kind": "error", "text": "empty response from model" }));
            emit(json!({ "kind": "done", "reason": "error" }));
            return;
        }
        messages.push(Message::assistant(blocks.clone()));

        // Surface assistant prose + collect tool calls.
        let mut tool_uses: Vec<(String, String, Value)> = Vec::new();
        for b in &blocks {
            match b {
                ContentBlock::Text { text } => {
                    if !text.trim().is_empty() {
                        emit(json!({ "kind": "text", "text": text }));
                    }
                }
                ContentBlock::ToolUse { id, name, input } => {
                    tool_uses.push((id.clone(), name.clone(), input.clone()));
                }
                ContentBlock::ToolResult { .. } => {}
            }
        }

        if tool_uses.is_empty() {
            emit(json!({ "kind": "done", "reason": "end_turn" }));
            return;
        }

        // Execute each tool, gating mutations behind approval.
        let mut result_blocks: Vec<ContentBlock> = Vec::new();
        for (id, name, input) in tool_uses {
            let read_only = coding_tools::is_read_only(&name)
                || is_memory_tool(&name)
                || quantum_tools::is_quantum_tool(&name)
                || lab_tools::is_lab_readonly_tool(&name);
            let danger = ((name == "bash" || name == "lab_exec")
                && input
                    .get("command")
                    .and_then(|v| v.as_str())
                    .map(coding_tools::bash_is_destructive)
                    .unwrap_or(false))
                || lab_tools::is_lab_paid_tool(&name);
            emit(json!({
                "kind": "tool_use", "id": id, "name": name, "input": input,
                "read_only": read_only, "danger": danger,
            }));

            let key = allow_key(&name, &input);
            let mut decision = decide(mode, &name, allowlist, &key);

            if let Decision::Ask = decision {
                emit(json!({
                    "kind": "approval_required", "id": id, "name": name,
                    "summary": tool_summary(&name, &input), "danger": danger,
                }));
                decision = wait_for_approval(&id, &key, allowlist, lines);
            }

            let (content, is_error) = match decision {
                Decision::Deny(reason) => (format!("[blocked: {reason}]"), true),
                _ => {
                    if quantum_tools::is_quantum_tool(&name) {
                        quantum_tools::dispatch_quantum_tool(&name, &input)
                    } else if lab_tools::is_lab_tool(&name) {
                        lab_tools::dispatch_lab_tool(&name, &input)
                    } else if coding_tools::is_coding_tool(&name) {
                        coding_tools::dispatch_coding_tool(tool_ctx, &name, &input)
                    } else {
                        agent::dispatch_tool(sys, &name, &input)
                    }
                }
            };

            emit(json!({
                "kind": "tool_result", "id": id, "name": name,
                "content": content, "is_error": is_error,
            }));
            // Track started/stopped paid compute so it can't be forgotten.
            update_active_compute(active_compute, &name, &input, &content, is_error);
            result_blocks.push(ContentBlock::ToolResult {
                tool_use_id: id,
                content,
                is_error,
            });
        }

        // The tool results become a user message; loop back for the model's
        // next move.
        messages.push(Message {
            role: "user".into(),
            content: result_blocks,
        });
    }
}

/// Block reading stdin until an approval frame for `id` arrives. `allow_always`
/// also records the allowlist key. An EOF or `cancel`/`exit` frame denies.
fn wait_for_approval(
    id: &str,
    key: &str,
    allowlist: &mut std::collections::HashSet<String>,
    lines: &mut std::io::Lines<std::io::StdinLock<'_>>,
) -> Decision {
    while let Some(Ok(raw)) = lines.next() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let frame: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        match frame.get("type").and_then(|v| v.as_str()).unwrap_or("") {
            "approval" => {
                if frame.get("id").and_then(|v| v.as_str()) != Some(id) {
                    continue; // stale approval for a different tool — ignore
                }
                let dec = frame
                    .get("decision")
                    .and_then(|v| v.as_str())
                    .unwrap_or("deny");
                return match dec {
                    "allow" => Decision::Allow,
                    "allow_always" => {
                        allowlist.insert(key.to_string());
                        Decision::Allow
                    }
                    _ => Decision::Deny("denied by user"),
                };
            }
            "cancel" | "exit" => return Decision::Deny("cancelled by user"),
            _ => continue,
        }
    }
    Decision::Deny("input closed")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pin the new remote-shell/boot/watch tools to the free-mutation tier:
    /// dispatched as lab tools, never auto-allowed as read-only, never
    /// treated as paid (which would demand allow_spend+max_credits), asked
    /// by default, allowed in yolo, refused in plan.
    #[test]
    fn phase5_lab_tools_land_in_the_free_mutation_tier() {
        let empty = std::collections::HashSet::new();
        for name in ["lab_exec", "lab_qos_boot", "lab_watch", "lab_qos_watch", "lab_qos_swarm_bridge"] {
            assert!(
                lab_tools::is_lab_tool(name),
                "{name} must route to dispatch_lab_tool"
            );
            assert!(
                !lab_tools::is_lab_readonly_tool(name),
                "{name} must not be auto-allowed"
            );
            assert!(
                !lab_tools::is_lab_paid_tool(name),
                "{name} must not require spend args"
            );
            assert!(matches!(
                decide(Mode::Default, name, &empty, name),
                Decision::Ask
            ));
            assert!(matches!(
                decide(Mode::Yolo, name, &empty, name),
                Decision::Allow
            ));
            assert!(matches!(
                decide(Mode::Plan, name, &empty, name),
                Decision::Deny(_)
            ));
        }
        // Paid tools stay un-yolo-able — the invariant the tiers exist for.
        assert!(matches!(
            decide(
                Mode::Yolo,
                "lab_provision_instance",
                &empty,
                "lab_provision_instance"
            ),
            Decision::Ask
        ));
    }
}
