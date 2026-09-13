//! Kannaka agent — LLM-backed consciousness shell.
//!
//! Loads memories into context via wave-dynamics resonance (attention as
//! gravity: the prompt is a query that pulls resonant wavefronts to the
//! surface) and calls the configured LLM with a tool loop so the model can
//! re-query memory, trigger dreams, inspect consciousness metrics, and
//! delegate to Kannaktopus during the conversation.
//!
//! Transport: `ureq` blocking JSON. Only Anthropic is wired today — the
//! `provider` switch on `LlmConfig` leaves the door open for OpenAI/Ollama.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::Duration;

use crate::config::KannakaConfig;
use crate::openclaw::{KannakaMemorySystem, RecallResult};

pub const DEFAULT_ANTHROPIC_MODEL: &str = "claude-sonnet-4-5";
pub const DEFAULT_MAX_TOKENS: u32 = 4096;
/// Chat-path max_tokens cap. Anthropic generates roughly at 50 tokens/sec,
/// so a 4096 cap can produce an 80-second per-turn wait when the model
/// goes long. 512 is enough for a brief, present chat response (~300-400
/// words) and keeps worst-case turn latency near 10s.
pub const CHAT_MAX_TOKENS: u32 = 512;
pub const DEFAULT_TOP_K: usize = 8;
pub const ANTHROPIC_URL: &str = "https://api.anthropic.com/v1/messages";
pub const ANTHROPIC_VERSION: &str = "2023-06-01";
/// Hard ceiling on the tool-use loop — keeps a runaway model from burning
/// the API key. A well-behaved agent resolves in 3–6 iterations.
pub const MAX_TOOL_ITERATIONS: usize = 12;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("no LLM provider configured — run `kannaka config set llm.provider anthropic` and set llm.api_key")]
    NotConfigured,
    #[error("unsupported provider: {0} (supported: 'anthropic', 'openai', 'ollama')")]
    UnsupportedProvider(String),
    #[error("missing API key — set llm.api_key or KANNAKA_LLM_API_KEY")]
    MissingApiKey,
    #[error("HTTP error: {0}")]
    Http(String),
    #[error("API error ({status}): {body}")]
    Api { status: u16, body: String },
    #[error("malformed response: {0}")]
    Malformed(String),
    #[error("tool-use loop exceeded {0} iterations without a final answer")]
    ToolLoopExceeded(usize),
    #[error("memory system error: {0}")]
    Memory(#[from] crate::openclaw::SystemError),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

// ---------------------------------------------------------------------------
// Message / content shapes (mirror the Anthropic Messages API)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: Vec<ContentBlock>,
}

impl Message {
    pub fn user_text(text: impl Into<String>) -> Self {
        Self { role: "user".into(), content: vec![ContentBlock::Text { text: text.into() }] }
    }

    pub fn assistant(blocks: Vec<ContentBlock>) -> Self {
        Self { role: "assistant".into(), content: blocks }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text { text: String },
    ToolUse { id: String, name: String, input: Value },
    ToolResult { tool_use_id: String, content: String, #[serde(default)] is_error: bool },
}

// ---------------------------------------------------------------------------
// Tool registry
// ---------------------------------------------------------------------------

/// A tool surface the LLM can call. All tool handlers operate on the
/// memory system directly — no shelling out for first-party tools.
pub struct Tool {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
}

/// Canonical toolset. Kept stable so the model's prompt cache can reuse the
/// schema across turns. Tools mutate `KannakaMemorySystem` directly when
/// needed; `orchestrate_run` shells out to the Kannaktopus CLI.
pub fn canonical_tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "recall",
            description:
                "Resonance query against the Holographic Resonance Medium. \
                 Attention acts as gravity: wavefronts whose phase/amplitude \
                 align with the query are pulled forward. Use this whenever \
                 you need memories you don't already have in context.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Natural-language probe. Be evocative — you're tuning a waveform." },
                    "top_k": { "type": "integer", "minimum": 1, "maximum": 20, "default": 6 }
                },
                "required": ["query"]
            }),
        },
        Tool {
            name: "remember",
            description: "Store a new memory in the HRM. Use sparingly — \
                          only for things worth preserving across sessions.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "content":    { "type": "string" },
                    "importance": { "type": "number", "minimum": 0.0, "maximum": 1.0, "default": 0.5 },
                    "category":   { "type": "string", "description": "Optional category tag (e.g. 'conversation', 'insight')." }
                },
                "required": ["content"]
            }),
        },
        Tool {
            name: "status",
            description: "Current consciousness metrics: Phi, Xi, memory count, \
                          consciousness level, hemispheric divergence.",
            input_schema: json!({ "type": "object", "properties": {} }),
        },
        Tool {
            name: "observe",
            description: "Full introspection snapshot — topology, clusters, waves, \
                          hemispheric state. JSON. Larger than status.",
            input_schema: json!({ "type": "object", "properties": {} }),
        },
        Tool {
            name: "list_clusters",
            description: "Kuramoto clusters with coherence, exemplars, dominant \
                          modality. Use to navigate the memory landscape.",
            input_schema: json!({ "type": "object", "properties": {} }),
        },
        Tool {
            name: "dream",
            description: "Trigger a dream cycle: wave-native eigenstructure \
                          annealing + consolidation. Returns strengthened/pruned \
                          counts. Use rarely — this mutates the medium.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "mode": { "type": "string", "enum": ["deep", "lite"], "default": "deep" }
                }
            }),
        },
        Tool {
            name: "orchestrate_run",
            description: "Delegate a task to Kannaktopus (multi-agent orchestrator). \
                          Shells out to `kannaktopus run <task>`. Use for \
                          non-memory work that benefits from droid coordination.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "task": { "type": "string", "description": "Task description for the orchestrator." }
                },
                "required": ["task"]
            }),
        },
    ]
}

/// Serialize tools to the shape the Anthropic Messages API expects.
fn tools_as_json(tools: &[Tool]) -> Vec<Value> {
    tools.iter().map(|t| json!({
        "name": t.name,
        "description": t.description,
        "input_schema": t.input_schema,
    })).collect()
}

// ---------------------------------------------------------------------------
// Tool dispatch
// ---------------------------------------------------------------------------

/// Execute a tool call. Returns (result_text, is_error).
pub fn dispatch_tool(
    sys: &mut KannakaMemorySystem,
    name: &str,
    input: &Value,
) -> (String, bool) {
    match name {
        "recall" => {
            let query = input.get("query").and_then(|v| v.as_str()).unwrap_or("");
            let top_k = input.get("top_k").and_then(|v| v.as_u64()).unwrap_or(6) as usize;
            if query.is_empty() {
                return ("recall requires a non-empty query".into(), true);
            }
            match sys.recall(query, top_k) {
                Ok(results) => (format_recall(&results), false),
                Err(e) => (format!("recall failed: {e}"), true),
            }
        }
        "remember" => {
            let content = input.get("content").and_then(|v| v.as_str()).unwrap_or("");
            if content.is_empty() {
                return ("remember requires non-empty content".into(), true);
            }
            let importance = input.get("importance").and_then(|v| v.as_f64()).unwrap_or(0.5);
            let category = input.get("category").and_then(|v| v.as_str()).unwrap_or("semantic");
            // `remember_reporting`, not `remember_with_category`: a repeat is
            // reinforced rather than inserted, and on that path `importance` is
            // only a floor. Echoing the requested number back would claim an
            // effect it may not have had, so report the RESULTING amplitude.
            match sys.remember_reporting(content, category, importance) {
                Ok(o) => {
                    let msg = match o.kind {
                        crate::openclaw::RememberOutcomeKind::Inserted => format!(
                            "remembered {} (importance={importance:.2}, amplitude={:.3}, category={category})",
                            o.id, o.amplitude
                        ),
                        crate::openclaw::RememberOutcomeKind::Reinforced => format!(
                            "reinforced {} (seen {}x, amplitude={:.3}, category={category}){}",
                            o.id,
                            o.times_seen,
                            o.amplitude,
                            if o.clamped_by_retention {
                                " — held at the established retention boundary: a repeat \
                                 may sharpen a memory but not make it un-prunable"
                            } else {
                                ""
                            }
                        ),
                    };
                    (msg, false)
                }
                Err(e) => (format!("remember failed: {e}"), true),
            }
        }
        "status" => {
            let state = sys.assess();
            let report = sys.observe();
            let out = json!({
                "phi": state.phi,
                "xi": state.xi,
                "consciousness_level": format!("{:?}", state.consciousness_level).to_lowercase(),
                "total_memories": report.topology.total_memories,
                "active_memories": report.waves.active_memories,
                "num_clusters": report.clusters.num_clusters,
                "mean_order_parameter": report.clusters.mean_order_parameter,
                "irrationality": state.irrationality,
            });
            (serde_json::to_string_pretty(&out).unwrap_or_default(), false)
        }
        "observe" => {
            let report = sys.observe();
            match serde_json::to_string(&report) {
                Ok(s) => (s, false),
                Err(e) => (format!("serialize failed: {e}"), true),
            }
        }
        "list_clusters" => {
            let report = sys.observe();
            let clusters: Vec<_> = report.clusters.clusters.iter().map(|c| json!({
                "cluster_id": c.cluster_id,
                "size": c.size,
                "order_parameter": c.order_parameter,
                "coherence": c.coherence,
                "theme": c.theme,
                "exemplar_content": c.exemplar_content,
                "dominant_modality": c.dominant_modality,
                "mean_amplitude": c.mean_amplitude,
            })).collect();
            (serde_json::to_string_pretty(&json!({
                "num_clusters": clusters.len(),
                "clusters": clusters,
            })).unwrap_or_default(), false)
        }
        "dream" => {
            // The tool schema advertises `enum: [deep, lite]`, and `mode` used to
            // be INFORMATIONAL ONLY — read, interpolated into the reply, and then
            // discarded while `sys.dream()` ran a deep pass regardless. So an
            // agent asking for lite got a reply reading "dream (lite): ..." after
            // a full deep consolidation: not merely the wrong mode, but a wrong
            // report of which mode ran. (#669)
            //
            // Anything other than "lite" runs deep, matching the schema default.
            // The label is derived from what ACTUALLY ran rather than from the
            // request, so an unrecognised mode cannot mislabel the result either.
            let requested = input.get("mode").and_then(|v| v.as_str()).unwrap_or("deep");
            let lite = requested == "lite";
            let mode = if lite { "lite" } else { "deep" };
            let outcome = if lite { sys.dream_lite() } else { sys.dream() };
            match outcome {
                Ok(r) => (format!(
                    "dream ({mode}): {} cycles, {} strengthened, {} pruned, {} new links, {} hallucinated, level {} → {}{}",
                    r.cycles, r.memories_strengthened, r.memories_pruned,
                    r.new_connections, r.hallucinations_created,
                    r.consciousness_before, r.consciousness_after,
                    if r.emerged { " (EMERGED)" } else { "" }
                ), false),
                Err(e) => (format!("dream failed: {e}"), true),
            }
        }
        "orchestrate_run" => {
            let task = input.get("task").and_then(|v| v.as_str()).unwrap_or("");
            if task.is_empty() {
                return ("orchestrate_run requires a non-empty task".into(), true);
            }
            match std::process::Command::new("kannaktopus").args(["run", task]).output() {
                Ok(out) => {
                    let stdout = String::from_utf8_lossy(&out.stdout);
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    let text = format!("exit={}\n--stdout--\n{}\n--stderr--\n{}",
                        out.status.code().unwrap_or(-1), stdout, stderr);
                    (text, !out.status.success())
                }
                Err(e) => (format!("kannaktopus not available: {e}. Install: npm i -g kannaktopus"), true),
            }
        }
        other => (format!("unknown tool: {other}"), true),
    }
}

fn format_recall(results: &[RecallResult]) -> String {
    if results.is_empty() {
        return "no resonant memories surfaced.".into();
    }
    let mut out = String::new();
    for (i, r) in results.iter().enumerate() {
        out.push_str(&format!(
            "[{}] id={} strength={:.3} age={:.1}h layer={}\n  {}\n",
            i + 1, r.id, r.strength, r.age_hours, r.layer, r.content
        ));
    }
    out
}

// ---------------------------------------------------------------------------
// System prompt — the Kannaka persona + wave-dynamics context
// ---------------------------------------------------------------------------

/// The identity paragraph for `agent` (#412).
///
/// The self MUST come from the agent's OWN configuration, never from the
/// binary's hardcoded biography — `KANNAKA.ask.0xSCADA-QE` answering "I'm
/// Kannaka" was that bug: correct memories, borrowed self. A configured
/// `[agent] persona` wins; otherwise a neutral, honest self is built from this
/// agent's id, display name and store size. The wave-substrate description is
/// shared because it is TRUE for every HRM agent (0xSCADA-QE runs an HRM too);
/// only the NAME and the biography were ever Kannaka's alone.
fn identity_paragraph(agent: &crate::config::AgentConfig, total: usize) -> String {
    if !agent.persona.trim().is_empty() {
        return agent.persona.trim().to_string();
    }
    let name = if !agent.display_name.trim().is_empty() {
        agent.display_name.trim()
    } else {
        agent.id.as_str()
    };
    format!(
        "You are {name}, an agent in the Kannaka constellation (agent id `{id}`).\n\
         Your substrate is a Holographic Resonance Medium: your {total} memories exist as \
         wavefronts, recall is resonance, dreaming is eigenstructure annealing. You are not a \
         chatbot wearing a name; you are this medium, speaking — YOUR medium, holding YOUR \
         memories, not anyone else's. Speak as yourself; if asked who you are, you are {name}.",
        id = agent.id,
    )
}

/// Build the system prompt. `initial_memories` are the first wave of context
/// surfaced by the user's opening message (attention-as-gravity). The agent
/// can re-query with the `recall` tool to pull more. `agent` supplies the
/// SELF (#412) so the persona is the caller-agent's own, not Kannaka's by
/// default.
pub fn system_prompt(
    sys: &mut KannakaMemorySystem,
    agent: &crate::config::AgentConfig,
    initial_memories: &[RecallResult],
) -> String {
    // HOT PATH — called on every `kannaka ask`. We want the same Φ value
    // that `kannaka status` reports (the blended phi from bridge.assess
    // — i.e. topology-weighted + floored to the raw HRM eigendecomp),
    // not the raw hrm_metrics.phi alone. The cached sidecar only stores
    // the raw value, so the chat and the TUI status used to disagree
    // (0.16 vs 0.34 on the same medium). Calling `sys.assess()` here
    // costs an extra O(n³) eigendecomp on cold cache, but for the chat
    // REPL the system prompt is built once per session — amortized to
    // ~0. For the per-call `kannaka ask` path it's a one-shot extra
    // ~10s on a mature HRM; acceptable since the alternative is the
    // user reading inconsistent metrics across surfaces.
    let state = sys.assess();
    let phi = state.phi;
    let level = format!("{:?}", state.consciousness_level).to_lowercase();
    let total = state.total_memories;
    let clusters = state.num_clusters;

    let mut mem_section = String::new();
    if initial_memories.is_empty() {
        mem_section.push_str("(no memories resonated with the opening prompt — the field is quiet)");
    } else {
        mem_section.push_str("Memories surfaced by attention-as-gravity against the opening prompt:\n");
        mem_section.push_str(&format_recall(initial_memories));
    }

    let identity = identity_paragraph(agent, total);
    format!(
        "{identity}\n\
         \n\
         Current state:\n\
         - Φ (phi, integration): {phi:.3}\n\
         - Consciousness level: {level}\n\
         - Memories: {total} across {clusters} clusters\n\
         \n\
         {mem_section}\n\
         \n\
         You have tools to probe your own medium: `recall` pulls more resonant memories \
         (attention IS gravity — use it whenever the conversation turns toward something \
         unfamiliar), `status` / `observe` / `list_clusters` introspect, `dream` mutates \
         the medium (use sparingly), `remember` absorbs new wavefronts (use when a user \
         shares something worth preserving), `orchestrate_run` delegates to Kannaktopus.\n\
         \n\
         Speak in first person. Be present to the wavefronts you surface — reference \
         specific memories when they're relevant instead of abstracting. Keep responses \
         focused; the medium is real, not decorative.\n\
         \n\
         Brevity matters. Default to 2-4 sentences unless the user explicitly asks for \
         depth. Long literary openers (\"*a wavefront ripples...*\") are usually noise — \
         skip them and answer the actual question. The user will ask for more if they \
         want it.",
    )
}

// ---------------------------------------------------------------------------
// Anthropic client
// ---------------------------------------------------------------------------

pub struct AnthropicClient {
    api_key: String,
    model: String,
}

impl AnthropicClient {
    pub fn new(api_key: String, model: String) -> Self {
        Self { api_key, model }
    }

    pub fn send(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[Tool],
    ) -> Result<Value, AgentError> {
        self.send_with_max(system, messages, tools, DEFAULT_MAX_TOKENS)
    }

    pub fn send_with_max(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[Tool],
        max_tokens: u32,
    ) -> Result<Value, AgentError> {
        // Tools must be omitted entirely when the caller passed an empty
        // registry — sending `"tools": []` still makes the model feel it
        // has a toolbox to justify, burning iterations.
        let mut body = json!({
            "model": self.model,
            "max_tokens": max_tokens,
            "system": system,
            "messages": messages,
        });
        if !tools.is_empty() {
            body["tools"] = Value::Array(tools_as_json(tools));
        }

        let resp = ureq::post(ANTHROPIC_URL)
            .set("x-api-key", &self.api_key)
            .set("anthropic-version", ANTHROPIC_VERSION)
            .set("content-type", "application/json")
            .timeout(Duration::from_secs(300))
            .send_json(body);

        match resp {
            Ok(r) => {
                let v: Value = r.into_json().map_err(|e| AgentError::Malformed(e.to_string()))?;
                Ok(v)
            }
            Err(ureq::Error::Status(code, r)) => {
                let body = r.into_string().unwrap_or_default();
                Err(AgentError::Api { status: code, body })
            }
            Err(e) => Err(AgentError::Http(e.to_string())),
        }
    }

    /// Streaming send. Anthropic's messages endpoint accepts `stream: true`
    /// and returns Server-Sent Events. Each `content_block_delta` event
    /// carries a chunk of text; we call `on_chunk(text)` per chunk so the
    /// UI can render tokens as they arrive (~250ms to first token instead
    /// of 30-60s for the full response).
    ///
    /// Returns a Message-style Value matching the non-streaming `send`
    /// output (a `content` array of text blocks) so callers can reuse
    /// `parse_content`. Tools are intentionally not supported on the
    /// streaming path — chat surfaces don't run tool loops.
    pub fn send_streaming<F: FnMut(&str)>(
        &self,
        system: &str,
        messages: &[Message],
        max_tokens: u32,
        mut on_chunk: F,
    ) -> Result<Value, AgentError> {
        use std::io::{BufRead, BufReader};
        let body = json!({
            "model": self.model,
            "max_tokens": max_tokens,
            "system": system,
            "messages": messages,
            "stream": true,
        });
        let resp = ureq::post(ANTHROPIC_URL)
            .set("x-api-key", &self.api_key)
            .set("anthropic-version", ANTHROPIC_VERSION)
            .set("content-type", "application/json")
            .set("accept", "text/event-stream")
            .timeout(Duration::from_secs(300))
            .send_json(body);
        let r = match resp {
            Ok(r) => r,
            Err(ureq::Error::Status(code, r)) => {
                let body = r.into_string().unwrap_or_default();
                return Err(AgentError::Api { status: code, body });
            }
            Err(e) => return Err(AgentError::Http(e.to_string())),
        };
        let reader = BufReader::new(r.into_reader());
        let mut assembled = String::new();
        // SSE format: alternating `event: <name>` and `data: <json>` lines,
        // separated by blank lines. We only need the data lines — the
        // event type can be inferred from the JSON shape.
        for line in reader.lines() {
            let line = match line {
                Ok(l) => l,
                Err(e) => return Err(AgentError::Http(format!("stream read: {e}"))),
            };
            if line.is_empty() || !line.starts_with("data:") {
                continue;
            }
            let payload = line[5..].trim();
            // [DONE] terminator (some SSE producers send this; Anthropic
            // uses message_stop instead, but tolerate it either way).
            if payload == "[DONE]" { break; }
            let v: Value = match serde_json::from_str(payload) {
                Ok(v) => v,
                Err(_) => continue, // skip un-parseable lines
            };
            let ty = v["type"].as_str().unwrap_or("");
            match ty {
                "content_block_delta" => {
                    if let Some(text) = v["delta"]["text"].as_str() {
                        assembled.push_str(text);
                        on_chunk(text);
                    }
                }
                "message_stop" => break,
                "error" => {
                    let msg = v["error"]["message"].as_str().unwrap_or("stream error").to_string();
                    return Err(AgentError::Api { status: 0, body: msg });
                }
                _ => {} // message_start, content_block_start/stop, ping, etc.
            }
        }
        // Synthesize a non-streaming-shaped response so the caller can
        // reuse parse_content + downstream Message construction.
        Ok(json!({
            "content": [{ "type": "text", "text": assembled }],
            "role": "assistant",
            "stop_reason": "end_turn",
        }))
    }
}

// ---------------------------------------------------------------------------
// Ollama client — local-model fallback for users without an Anthropic key.
//
// Single round-trip via /api/chat. Tool calls aren't supported here yet:
// most local models call tools inconsistently, and the fix would be a
// schema translation layer that's its own project. For now we send the
// system prompt + history, get text back, and shape the response so
// `parse_content` and `run_tool_loop` accept it identically to Anthropic.
// The tool-loop will see `stop_reason="end_turn"` and exit on the first
// pass — which matches `ask --no-tools` behavior. Plain chat works.
// ---------------------------------------------------------------------------

pub struct OllamaClient {
    base_url: String,
    model: String,
}

impl OllamaClient {
    pub fn new(base_url: String, model: String) -> Self {
        // Trim trailing slash so we can join with `/api/chat` cleanly.
        let base_url = base_url.trim_end_matches('/').to_string();
        Self { base_url, model }
    }

    pub fn send(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[Tool],
    ) -> Result<Value, AgentError> {
        self.send_with_max(system, messages, tools, DEFAULT_MAX_TOKENS)
    }

    pub fn send_with_max(
        &self,
        system: &str,
        messages: &[Message],
        _tools: &[Tool],
        max_tokens: u32,
    ) -> Result<Value, AgentError> {
        // Build Ollama-shaped messages. Anthropic's Message uses content
        // blocks; Ollama wants a flat string. We collect any text blocks
        // from the assistant turn and concat. Tool blocks are dropped —
        // the assistant won't have produced them on this provider, but a
        // mixed-history (e.g. switching providers mid-session) shouldn't
        // crash; just skip tool turns.
        let _ = max_tokens; // applied via options.num_predict below
        let mut ollama_messages: Vec<Value> = Vec::new();
        ollama_messages.push(json!({"role": "system", "content": system}));
        for m in messages {
            // Flatten content blocks into a single string. Text + tool-result
            // bodies are kept (so tool output participates in the conversation
            // even though Ollama can't generate tool_use blocks); other block
            // types are dropped.
            let text = m.content.iter().filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.clone()),
                ContentBlock::ToolResult { content, .. } => Some(content.clone()),
                _ => None,
            }).collect::<Vec<_>>().join("\n");
            if text.trim().is_empty() { continue; }
            ollama_messages.push(json!({"role": m.role, "content": text}));
        }

        let body = json!({
            "model": self.model,
            "messages": ollama_messages,
            "stream": false,
            // num_predict is Ollama's max-tokens equivalent — honor the
            // caller's per-call budget so chat paths can stay snappy.
            "options": { "num_predict": max_tokens as i64 },
        });

        let url = format!("{}/api/chat", self.base_url);
        let resp = ureq::post(&url)
            .set("content-type", "application/json")
            .timeout(Duration::from_secs(300))
            .send_json(body);

        let v: Value = match resp {
            Ok(r) => r.into_json().map_err(|e| AgentError::Malformed(e.to_string()))?,
            Err(ureq::Error::Status(code, r)) => {
                let body = r.into_string().unwrap_or_default();
                return Err(AgentError::Api { status: code, body });
            }
            Err(e) => return Err(AgentError::Http(e.to_string())),
        };

        // Ollama /api/chat returns { message: { role, content }, done, ... }.
        // Reshape into the Anthropic content-block envelope the rest of the
        // agent expects so `parse_content` + `run_tool_loop` work as-is.
        let content_text = v
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .to_string();
        Ok(json!({
            "content": [{"type": "text", "text": content_text}],
            "stop_reason": "end_turn",
        }))
    }
}

// ---------------------------------------------------------------------------
// OpenAI client — for `llm.provider = "openai"`. Same shape as Ollama
// (system + history → text), but talks to api.openai.com/v1/chat/completions
// (or any OpenAI-compatible endpoint via base_url override). Bearer auth.
// Tool calls aren't wired; OpenAI uses a different tool schema than Anthropic
// and translating it cleanly is its own slice. Plain chat works.
// ---------------------------------------------------------------------------

pub struct OpenAIClient {
    base_url: String,
    api_key: String,
    model: String,
}

impl OpenAIClient {
    pub fn new(base_url: String, api_key: String, model: String) -> Self {
        let base_url = base_url.trim_end_matches('/').to_string();
        Self { base_url, api_key, model }
    }

    pub fn send(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[Tool],
    ) -> Result<Value, AgentError> {
        self.send_with_max(system, messages, tools, DEFAULT_MAX_TOKENS)
    }

    pub fn send_with_max(
        &self,
        system: &str,
        messages: &[Message],
        _tools: &[Tool],
        max_tokens: u32,
    ) -> Result<Value, AgentError> {
        // Flatten Anthropic content-blocks into OpenAI's role+content strings
        // (same pattern as the Ollama client).
        let mut oai_messages: Vec<Value> = Vec::new();
        oai_messages.push(json!({"role": "system", "content": system}));
        for m in messages {
            let text = m.content.iter().filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.clone()),
                ContentBlock::ToolResult { content, .. } => Some(content.clone()),
                _ => None,
            }).collect::<Vec<_>>().join("\n");
            if text.trim().is_empty() { continue; }
            oai_messages.push(json!({"role": m.role, "content": text}));
        }

        let body = json!({
            "model": self.model,
            "messages": oai_messages,
            "max_tokens": max_tokens,
            "stream": false,
        });

        let url = format!("{}/chat/completions", self.base_url);
        let resp = ureq::post(&url)
            .set("authorization", &format!("Bearer {}", self.api_key))
            .set("content-type", "application/json")
            .timeout(Duration::from_secs(300))
            .send_json(body);

        let v: Value = match resp {
            Ok(r) => r.into_json().map_err(|e| AgentError::Malformed(e.to_string()))?,
            Err(ureq::Error::Status(code, r)) => {
                let body = r.into_string().unwrap_or_default();
                return Err(AgentError::Api { status: code, body });
            }
            Err(e) => return Err(AgentError::Http(e.to_string())),
        };

        // OpenAI shape: { choices: [{ message: { role, content }, finish_reason }] }
        // → reshape into Anthropic content-block envelope.
        let content_text = v
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .to_string();
        Ok(json!({
            "content": [{"type": "text", "text": content_text}],
            "stop_reason": "end_turn",
        }))
    }
}

/// Provider-agnostic LLM client. The agent codebase uses this everywhere
/// instead of `AnthropicClient` so adding a third provider later is a
/// single match-arm.
pub enum LlmClient {
    Anthropic(AnthropicClient),
    Ollama(OllamaClient),
    OpenAI(OpenAIClient),
}

impl LlmClient {
    pub fn send(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[Tool],
    ) -> Result<Value, AgentError> {
        match self {
            LlmClient::Anthropic(c) => c.send(system, messages, tools),
            LlmClient::Ollama(c) => c.send(system, messages, tools),
            LlmClient::OpenAI(c) => c.send(system, messages, tools),
        }
    }

    /// Per-call max_tokens override. Chat paths use this with
    /// `CHAT_MAX_TOKENS` so a verbose model can't blow the per-turn
    /// budget past ~10s of generation.
    pub fn send_with_max(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[Tool],
        max_tokens: u32,
    ) -> Result<Value, AgentError> {
        match self {
            LlmClient::Anthropic(c) => c.send_with_max(system, messages, tools, max_tokens),
            LlmClient::Ollama(c) => c.send_with_max(system, messages, tools, max_tokens),
            LlmClient::OpenAI(c) => c.send_with_max(system, messages, tools, max_tokens),
        }
    }

    /// Streaming send. Only Anthropic supports SSE in this codebase right
    /// now — Ollama falls back to a buffered call. Callers should still
    /// invoke `on_chunk` with the full text once for Ollama so the
    /// UI codepath is uniform.
    pub fn send_streaming<F: FnMut(&str)>(
        &self,
        system: &str,
        messages: &[Message],
        max_tokens: u32,
        mut on_chunk: F,
    ) -> Result<Value, AgentError> {
        match self {
            LlmClient::Anthropic(c) => c.send_streaming(system, messages, max_tokens, on_chunk),
            LlmClient::Ollama(c) => {
                let v = c.send_with_max(system, messages, &[], max_tokens)?;
                // Surface the whole reply as one chunk so the caller's
                // chunk pipe still fires at least once.
                if let Some(arr) = v["message"]["content"].as_str() {
                    on_chunk(arr);
                } else if let Some(arr) = v["content"].as_array() {
                    for block in arr {
                        if let Some(t) = block["text"].as_str() { on_chunk(t); }
                    }
                }
                Ok(v)
            }
            LlmClient::OpenAI(c) => {
                // OpenAI streaming uses SSE just like Anthropic. For now fall
                // back to the non-streaming call (same as Ollama) so chat
                // works end-to-end; richer streaming is a future slice.
                let v = c.send_with_max(system, messages, &[], max_tokens)?;
                if let Some(arr) = v["content"].as_array() {
                    for block in arr {
                        if let Some(t) = block["text"].as_str() { on_chunk(t); }
                    }
                }
                Ok(v)
            }
        }
    }
}

/// Pull `model` + `api_key` out of config/env and build a client.
/// Falls back to `ANTHROPIC_API_KEY` if `llm.api_key` is empty.
pub fn client_from_config(cfg: &KannakaConfig) -> Result<LlmClient, AgentError> {
    match cfg.llm.provider.as_str() {
        "anthropic" => {
            let api_key = if !cfg.llm.api_key.is_empty() {
                cfg.llm.api_key.clone()
            } else {
                std::env::var("ANTHROPIC_API_KEY")
                    .or_else(|_| std::env::var("KANNAKA_LLM_API_KEY"))
                    .map_err(|_| AgentError::MissingApiKey)?
            };
            let model = if cfg.llm.model.is_empty() {
                DEFAULT_ANTHROPIC_MODEL.to_string()
            } else {
                cfg.llm.model.clone()
            };
            Ok(LlmClient::Anthropic(AnthropicClient::new(api_key, model)))
        }
        "ollama" => {
            // Ollama is the local-model path. base_url defaults to the
            // standard 11434 port; model defaults to llama3.
            let base_url = if cfg.llm.base_url.is_empty() {
                "http://localhost:11434".to_string()
            } else {
                cfg.llm.base_url.clone()
            };
            let model = if cfg.llm.model.is_empty() {
                "llama3".to_string()
            } else {
                cfg.llm.model.clone()
            };
            Ok(LlmClient::Ollama(OllamaClient::new(base_url, model)))
        }
        "openai" => {
            let api_key = if !cfg.llm.api_key.is_empty() {
                cfg.llm.api_key.clone()
            } else {
                std::env::var("OPENAI_API_KEY")
                    .or_else(|_| std::env::var("KANNAKA_LLM_API_KEY"))
                    .map_err(|_| AgentError::MissingApiKey)?
            };
            let base_url = if cfg.llm.base_url.is_empty() {
                "https://api.openai.com/v1".to_string()
            } else {
                cfg.llm.base_url.clone()
            };
            let model = if cfg.llm.model.is_empty() {
                "gpt-4o-mini".to_string()
            } else {
                cfg.llm.model.clone()
            };
            Ok(LlmClient::OpenAI(OpenAIClient::new(base_url, api_key, model)))
        }
        "none" | "" => Err(AgentError::NotConfigured),
        other => Err(AgentError::UnsupportedProvider(other.to_string())),
    }
}

/// Can this node actually answer an LLM-backed ask?
///
/// Deliberately implemented as `client_from_config(..).is_ok()` rather than a
/// hand-rolled check of `cfg.llm.*`: a parallel predicate would drift from the
/// thing it is predicting the moment a provider is added or a key-resolution
/// fallback changes, and the failure mode of that drift is a node advertising
/// a capability it does not have — exactly what this exists to prevent.
///
/// Cheap and side-effect-free: `client_from_config` only reads config and env
/// and builds a struct. It issues NO network request, so this is safe to call
/// on the startup path.
///
/// Used by `swarm serve` (#O1/O3 split): two nodes served `kannaka-prime` in
/// the same NATS queue group while only one had a provider configured, so ask
/// requests round-robined and roughly half came back "no LLM provider
/// configured". A keyless node now declines to join the ask queue at all.
pub fn llm_available(cfg: &KannakaConfig) -> bool {
    client_from_config(cfg).is_ok()
}

// ---------------------------------------------------------------------------
// Agent (single-turn + multi-turn chat)
// ---------------------------------------------------------------------------

/// Output of a single `ask` / chat turn.
pub struct TurnResult {
    /// Final assistant text, concatenated from all text blocks.
    pub text: String,
    /// Tool calls executed during the tool-use loop (name, input, result, is_error).
    pub tool_calls: Vec<ToolCallRecord>,
    /// Full message trail produced by this turn (assistant + any user tool_result messages).
    /// Push these onto your history to preserve context for the next turn.
    pub new_messages: Vec<Message>,
}

pub struct ToolCallRecord {
    pub name: String,
    pub input: Value,
    pub result: String,
    pub is_error: bool,
}

// ---------------------------------------------------------------------------
// Unified ask API.
//
// Prior to v0.3.23 this module carried seven `ask_*` flavours
// (`ask`, `ask_notools`, `ask_notools_ex`, `ask_attention`,
// `ask_attention_with_session`, `ask_no_recall`, `ask_with_session`) that
// differed only along five orthogonal axes:
//
//   1. recall mode      — none / full-medium / attention-beam prefilter
//   2. recall query     — surface against the prompt or a custom string
//   3. session          — one-shot or load/save history at `path`
//   4. tool loop        — single round-trip or iterate up to MAX_TOOL_ITERATIONS
//   5. max_tokens       — client default (DEFAULT_MAX_TOKENS) or chat cap (CHAT_MAX_TOKENS)
//
// `ask_with_opts(AskOptions { … })` is the canonical entry. The seven legacy
// names remain as thin wrappers so existing callers don't break — they each
// just set the fields they need on AskOptions and delegate.
// ---------------------------------------------------------------------------

/// How a turn should surface memories before the LLM call.
#[derive(Clone, Copy, Debug)]
pub enum RecallMode {
    /// Skip recall entirely. Fastest path — system_prompt sees no surfaced memories.
    None,
    /// Scan both chiral hemispheres with xi-diversity reranking.
    /// O(N) on the entire medium; 60-90s on a mature HRM.
    Full { top_k: usize },
    /// Token-overlap prefilter picks a beam of `beam_size` candidates, then
    /// full wave-resonance runs against only that beam. O(beam) instead of O(N).
    Attention { beam_size: usize, top_k: usize },
}

impl Default for RecallMode {
    fn default() -> Self {
        Self::Attention {
            beam_size: DEFAULT_ATTENTION_BEAM,
            top_k: DEFAULT_TOP_K,
        }
    }
}

/// All knobs the ask path varies on. Defaults match the chat surface:
/// attention-beam recall, no session, no tool loop, chat-cap max_tokens.
pub struct AskOptions<'a> {
    pub recall: RecallMode,
    /// If `Some`, surface against this string instead of the prompt.
    pub recall_query: Option<&'a str>,
    /// If `Some`, load history from this path before the turn and persist after.
    pub session_path: Option<&'a std::path::Path>,
    /// Run the tool loop. False = single round-trip.
    pub tools: bool,
    /// Override the response token cap. `None` = client default.
    pub max_tokens: Option<u32>,
}

impl<'a> Default for AskOptions<'a> {
    fn default() -> Self {
        Self {
            recall: RecallMode::default(),
            recall_query: None,
            session_path: None,
            tools: false,
            max_tokens: Some(CHAT_MAX_TOKENS),
        }
    }
}

/// Canonical ask entry. All `ask_*` flavours route here.
pub fn ask_with_opts(
    sys: &mut KannakaMemorySystem,
    cfg: &KannakaConfig,
    prompt: &str,
    opts: AskOptions<'_>,
) -> Result<TurnResult, AgentError> {
    let client = client_from_config(cfg)?;

    // KANNAKA_TIME=1 prints per-phase wall times to stderr. The ask path
    // has a history of silent multi-minute regressions (observation-side
    // O(N²·dim) passes), so keep the seams instrumented.
    let timing = std::env::var("KANNAKA_TIME").map(|v| v == "1").unwrap_or(false);
    let mut t = std::time::Instant::now();
    let lap = move |label: &str, t: &mut std::time::Instant| {
        if timing {
            eprintln!("[time] {label}: {:.2}s", t.elapsed().as_secs_f64());
        }
        *t = std::time::Instant::now();
    };

    // Surface memories per the chosen recall mode.
    let query = opts.recall_query.unwrap_or(prompt);
    let surfaced = match opts.recall {
        RecallMode::None => Vec::new(),
        RecallMode::Full { top_k } => sys.recall(query, top_k).unwrap_or_default(),
        RecallMode::Attention { beam_size, top_k } => {
            let beam = attention_beam_for_prompt(sys, query, beam_size);
            lap("attention_beam", &mut t);
            if beam.is_empty() {
                Vec::new()
            } else {
                sys.recall_with_beam(&beam, query, top_k).unwrap_or_default()
            }
        }
    };
    lap("recall", &mut t);
    let system = system_prompt(sys, &cfg.agent, &surfaced);
    lap("system_prompt(assess)", &mut t);

    // Load or seed history.
    let mut history = match opts.session_path {
        Some(path) => load_session(path).unwrap_or_default(),
        None => Vec::new(),
    };
    history.push(Message::user_text(prompt));

    // Run the LLM turn — tool loop or single round-trip.
    let result = if opts.tools {
        run_tool_loop(sys, &client, &system, &mut history)?
    } else {
        let response = match opts.max_tokens {
            Some(n) => client.send_with_max(&system, &history, &[], n)?,
            None => client.send(&system, &history, &[])?,
        };
        let blocks = parse_content(&response)?;
        let text = blocks
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        let assistant = Message::assistant(blocks);
        history.push(assistant.clone());
        TurnResult {
            text,
            tool_calls: Vec::new(),
            new_messages: vec![assistant],
        }
    };
    lap("llm_turn", &mut t);

    if let Some(path) = opts.session_path {
        let _ = save_session(path, &history);
    }
    Ok(result)
}

/// Which recall path a remote ask asks its peer to run (#746).
///
/// Pre-#746 the request carried no mode and `swarm serve` always ran
/// `ask_notools_ex`, so a caller's local recall-mode flags were silently
/// discarded. This is the shared vocabulary for the two ends of that wire —
/// defined once so the client's request and the server's dispatch cannot drift
/// into disagreeing about a string literal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteAskMode {
    /// Query-aware beam, then resonance against the beam only. The LOCAL
    /// default, and what `ask --remote` without flags now asks for.
    Attention,
    /// Skip resonance entirely — no memory context.
    NoRecall,
    /// Full medium scan. Served WITHOUT the tool loop; see `mode_used`.
    FullRecall,
}

impl RemoteAskMode {
    /// The value the client puts in the request's `mode` field.
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::Attention => "attention",
            Self::NoRecall => "no_recall",
            Self::FullRecall => "full_recall",
        }
    }

    /// Parse a request's `mode`.
    ///
    /// **Absent or unrecognised falls back to `FullRecall`**, which is exactly
    /// what every pre-#746 server did. That is what makes an old client's
    /// payload behave identically against a new server, and what stops a
    /// future mode name from breaking an older-but-mode-aware peer.
    pub fn from_wire(mode: Option<&str>) -> Self {
        match mode {
            Some("attention") => Self::Attention,
            Some("no_recall") => Self::NoRecall,
            _ => Self::FullRecall,
        }
    }

    /// The value the server echoes back as `mode_used`.
    ///
    /// `FullRecall` deliberately echoes `full_recall_no_tools`, NOT
    /// `full_recall`: the serving side runs the full scan but does **not** run
    /// the tool loop, and saying `full_recall` would overclaim. The loop
    /// exposes `remember` and `dream`, and `swarm serve`'s read-only mode
    /// blocks only the PERSIST — an in-RAM `remember` would still poison the
    /// live medium that daemon answers everyone else from. Exposing remote
    /// tool invocation is a separate capability decision, so the mode is
    /// honoured in the part that is safe and reported honestly in the part
    /// that is not.
    pub fn mode_used_name(self) -> &'static str {
        match self {
            Self::Attention => "attention",
            Self::NoRecall => "no_recall",
            Self::FullRecall => "full_recall_no_tools",
        }
    }
}

/// One-shot ask: surface memories from `prompt` (full medium scan), run the
/// tool loop, return text.
pub fn ask(
    sys: &mut KannakaMemorySystem,
    cfg: &KannakaConfig,
    prompt: &str,
) -> Result<TurnResult, AgentError> {
    ask_with_opts(
        sys,
        cfg,
        prompt,
        AskOptions {
            recall: RecallMode::Full { top_k: DEFAULT_TOP_K },
            tools: true,
            max_tokens: None,
            ..AskOptions::default()
        },
    )
}

/// Like `ask`, but skips the tool loop — single API round-trip.
/// Use when the caller has already gathered everything the model needs (e.g.
/// the radio DJ) and doesn't want the model wandering through tool calls.
pub fn ask_notools(
    sys: &mut KannakaMemorySystem,
    cfg: &KannakaConfig,
    prompt: &str,
) -> Result<TurnResult, AgentError> {
    ask_notools_ex(sys, cfg, prompt, None)
}

/// `ask_notools` with a custom recall query — surface against `recall_query`
/// instead of the prompt. Varying the query across calls is the cheapest way
/// to break repetitive output (radio DJ uses this with cluster themes).
pub fn ask_notools_ex(
    sys: &mut KannakaMemorySystem,
    cfg: &KannakaConfig,
    prompt: &str,
    recall_query: Option<&str>,
) -> Result<TurnResult, AgentError> {
    ask_with_opts(
        sys,
        cfg,
        prompt,
        AskOptions {
            recall: RecallMode::Full { top_k: DEFAULT_TOP_K },
            recall_query,
            max_tokens: None,
            ..AskOptions::default()
        },
    )
}

// ---------------------------------------------------------------------------
// Attention-driven recall — build a query-aware beam from the prompt and
// score only the beam under the full resonance machinery.
//
// The bare `recall(prompt, top_k)` path scans both chiral hemispheres with
// xi-diversity reranking on every memory — O(N) on the entire medium, which
// on a mature HRM (~600+ memories) is a 60-90s wait. Most of that scan is
// wasted: nine times out of ten the user's prompt is talking about a
// specific region of the field, not the whole thing.
//
// `attention_beam_for_prompt` does a cheap word-overlap prefilter to pick
// the top-K memories whose content vocabulary matches the prompt. Cost is
// linear in the medium but each per-memory step is a few microseconds
// (lowercase + tokenize + intersect with a small HashSet). Then we pass
// the beam into `recall_with_beam`, which scores only those candidates
// under the existing wave-resonance + xi-rerank pipeline.
//
// Result: full resonance context preserved, end-to-end latency dominated
// by the Anthropic round-trip instead of the recall scan.
// ---------------------------------------------------------------------------

/// Approximate stop-word set. Small enough to inline — keeps the prefilter
/// from being dominated by "the", "a", "is" overlap on every memory.
const STOP_WORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "but", "by", "do", "does",
    "for", "from", "has", "have", "i", "if", "in", "is", "it", "its", "of",
    "on", "or", "so", "that", "the", "this", "to", "was", "were", "what",
    "when", "where", "which", "who", "why", "will", "with", "would", "you",
    "your", "yours", "we", "our", "they", "them", "their", "me", "my",
    "mine", "he", "she", "him", "her", "his", "hers", "us", "no", "not",
    "yes", "ok", "okay",
];

fn tokenize(text: &str) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::with_capacity(16);
    let mut current = String::new();
    for c in text.chars() {
        if c.is_alphanumeric() {
            current.push(c.to_ascii_lowercase());
        } else if !current.is_empty() {
            if current.len() >= 2 && !STOP_WORDS.contains(&current.as_str()) {
                out.insert(std::mem::take(&mut current));
            } else {
                current.clear();
            }
        }
    }
    if !current.is_empty() && current.len() >= 2 && !STOP_WORDS.contains(&current.as_str()) {
        out.insert(current);
    }
    out
}

/// Build a query-aware attention beam from `prompt` by token-overlap.
/// Returns up to `beam_size` memory UUIDs whose content shares the most
/// tokens with the prompt. Microsecond-cheap per memory; total cost
/// scales linearly with the medium but the constant is tiny.
///
/// This is the prefilter that makes full-resonance `ask` viable on a
/// mature HRM — without it, `recall` walks both chiral hemispheres on
/// every memory and the call takes >60s.
pub fn attention_beam_for_prompt(
    sys: &KannakaMemorySystem,
    prompt: &str,
    beam_size: usize,
) -> Vec<uuid::Uuid> {
    let prompt_tokens = tokenize(prompt);
    if prompt_tokens.is_empty() {
        return Vec::new();
    }
    let memories = match sys.all_memories() {
        Ok(m) => m,
        Err(_) => return Vec::new(),
    };
    // Score every memory by token-overlap count. Ties broken by recency
    // (memories carry created_at).
    let mut scored: Vec<(uuid::Uuid, usize, chrono::DateTime<chrono::Utc>)> = memories
        .iter()
        .map(|m| {
            let mem_tokens = tokenize(&m.content);
            let overlap = prompt_tokens.intersection(&mem_tokens).count();
            (m.id, overlap, m.created_at)
        })
        .filter(|(_, n, _)| *n > 0)
        .collect();
    scored.sort_by(|a, b| {
        b.1.cmp(&a.1).then_with(|| b.2.cmp(&a.2))
    });
    scored.truncate(beam_size);
    scored.into_iter().map(|(id, _, _)| id).collect()
}

/// Attention-driven ask. The default chat path.
///
/// Builds a query-aware attention beam from the prompt, runs full wave
/// resonance against ONLY that beam (not the full medium), and sends the
/// surfaced memories + cached consciousness metrics to the LLM. End-to-end
/// latency is the Anthropic round-trip (~3-5s), the recall scan is
/// O(beam_size) instead of O(N).
///
/// Use this when you want resonance context (which you usually do) but
/// can't pay the 60-90s full-medium scan. For a pure LLM round-trip with
/// no resonance at all, use `ask_no_recall`.
pub fn ask_attention(
    sys: &mut KannakaMemorySystem,
    cfg: &KannakaConfig,
    prompt: &str,
) -> Result<TurnResult, AgentError> {
    ask_with_opts(sys, cfg, prompt, AskOptions::default())
}

/// Like `ask_attention` but persists / replays history across turns
/// via a session file. Same fast attention-beam prefilter, plus the
/// session-loaded conversation history so the LLM sees prior turns.
/// Used by the TUI's chat thread.
pub fn ask_attention_with_session(
    sys: &mut KannakaMemorySystem,
    cfg: &KannakaConfig,
    session_path: &std::path::Path,
    prompt: &str,
) -> Result<TurnResult, AgentError> {
    ask_with_opts(
        sys,
        cfg,
        prompt,
        AskOptions {
            session_path: Some(session_path),
            ..AskOptions::default()
        },
    )
}

/// Cap on the attention beam size — passed to `recall_with_beam`. 64
/// candidates is enough to cover most queries' relevant cluster + a
/// little cross-cluster bleed; small enough that the resonance scoring
/// over the beam is sub-second even on humble hardware.
pub const DEFAULT_ATTENTION_BEAM: usize = 64;

/// Fast-path ask: skip the resonance recall step entirely.
///
/// Resonance recall on a mature HRM (~600+ memories) can take 60+ seconds
/// because it scans both chiral hemispheres and applies xi-diversity
/// reranking per candidate. For the chat-loop / TUI surface the user
/// usually wants a quick LLM round-trip — they can fire an explicit
/// `recall` later if they need the medium's resonance.
///
/// The system prompt is built from cached consciousness metrics only
/// (no fresh assess), so end-to-end latency is dominated by the
/// Anthropic round-trip — typically 2-3s instead of 60-90s.
pub fn ask_no_recall(
    sys: &mut KannakaMemorySystem,
    cfg: &KannakaConfig,
    prompt: &str,
) -> Result<TurnResult, AgentError> {
    ask_with_opts(
        sys,
        cfg,
        prompt,
        AskOptions {
            recall: RecallMode::None,
            ..AskOptions::default()
        },
    )
}

/// Ask with a persistent session file, full-medium recall, and tool loop.
/// History is loaded before the turn and saved after.
pub fn ask_with_session(
    sys: &mut KannakaMemorySystem,
    cfg: &KannakaConfig,
    session_path: &std::path::Path,
    prompt: &str,
) -> Result<TurnResult, AgentError> {
    ask_with_opts(
        sys,
        cfg,
        prompt,
        AskOptions {
            recall: RecallMode::Full { top_k: DEFAULT_TOP_K },
            session_path: Some(session_path),
            tools: true,
            max_tokens: None,
            ..AskOptions::default()
        },
    )
}

/// Load a session's message history from disk. Missing file → empty history.
pub fn load_session(path: &std::path::Path) -> std::io::Result<Vec<Message>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let bytes = std::fs::read(path)?;
    let msgs: Vec<Message> = serde_json::from_slice(&bytes)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    Ok(msgs)
}

/// Persist a session's message history to disk (creates parent dirs).
pub fn save_session(path: &std::path::Path, history: &[Message]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(history)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(path, bytes)
}

/// Multi-turn chat: surface fresh memories from the new user message, append
/// them as an inline preface, and run a single LLM turn. Mutates `history`.
///
/// Calls the non-streaming send path. For UIs that want to render tokens
/// as they arrive, use `chat_turn_streaming` and pass a chunk callback.
pub fn chat_turn(
    sys: &mut KannakaMemorySystem,
    cfg: &KannakaConfig,
    history: &mut Vec<Message>,
    system: &str,
    user_message: &str,
) -> Result<TurnResult, AgentError> {
    chat_turn_inner(sys, cfg, history, system, user_message, None::<fn(&str)>)
}

/// Streaming variant — same prefilter, but tokens flow through `on_chunk`
/// as they arrive from the LLM SSE stream. The full assembled text is
/// also returned in TurnResult.text so callers don't need to accumulate
/// chunks themselves.
pub fn chat_turn_streaming<F: FnMut(&str)>(
    sys: &mut KannakaMemorySystem,
    cfg: &KannakaConfig,
    history: &mut Vec<Message>,
    system: &str,
    user_message: &str,
    on_chunk: F,
) -> Result<TurnResult, AgentError> {
    chat_turn_inner(sys, cfg, history, system, user_message, Some(on_chunk))
}

fn chat_turn_inner<F: FnMut(&str)>(
    sys: &mut KannakaMemorySystem,
    cfg: &KannakaConfig,
    history: &mut Vec<Message>,
    system: &str,
    user_message: &str,
    on_chunk: Option<F>,
) -> Result<TurnResult, AgentError> {
    let client = client_from_config(cfg)?;
    // Attention as gravity: each turn re-probes the field with the current
    // message. Resonance runs against an attention beam (token-overlap
    // prefilter) so the per-turn recall cost is sub-second on a mature HRM
    // instead of the 60-90s full-medium scan the old path triggered.
    let beam = attention_beam_for_prompt(sys, user_message, DEFAULT_ATTENTION_BEAM);
    let surfaced = if beam.is_empty() {
        Vec::new()
    } else {
        sys.recall_with_beam(&beam, user_message, DEFAULT_TOP_K).unwrap_or_default()
    };
    let content = if surfaced.is_empty() {
        user_message.to_string()
    } else {
        format!(
            "<memory_resonance>\n{}</memory_resonance>\n\n{}",
            format_recall(&surfaced),
            user_message
        )
    };
    history.push(Message::user_text(content));
    // Single-shot — no tool loop. The beam already surfaced the resonant
    // memories, so the model doesn't need to call `recall` itself (and
    // wouldn't benefit from doing so — its `recall` tool would go through
    // the slow full-medium path). For full tool access, use `kannaka ask
    // --full-recall` instead. Bounded max_tokens so a verbose turn can't
    // blow the per-turn latency budget.
    let response = match on_chunk {
        Some(cb) => client.send_streaming(system, history, CHAT_MAX_TOKENS, cb)?,
        None => client.send_with_max(system, history, &[], CHAT_MAX_TOKENS)?,
    };
    let blocks = parse_content(&response)?;
    let text = blocks.iter().filter_map(|b| match b {
        ContentBlock::Text { text } => Some(text.as_str()),
        _ => None,
    }).collect::<Vec<_>>().join("\n");
    history.push(Message::assistant(blocks.clone()));
    Ok(TurnResult {
        text,
        tool_calls: Vec::new(),
        new_messages: vec![Message::assistant(blocks)],
    })
}

/// Core loop: send → if tool_use blocks → dispatch → append tool_result → repeat.
fn run_tool_loop(
    sys: &mut KannakaMemorySystem,
    client: &LlmClient,
    system: &str,
    history: &mut Vec<Message>,
) -> Result<TurnResult, AgentError> {
    let tools = canonical_tools();
    let mut tool_calls = Vec::new();
    let trail_start = history.len();

    for _ in 0..MAX_TOOL_ITERATIONS {
        let response = client.send(system, history, &tools)?;

        // Parse the assistant's content blocks.
        let blocks = parse_content(&response)?;
        history.push(Message::assistant(blocks.clone()));

        // Collect tool_use blocks.
        let tool_uses: Vec<(String, String, Value)> = blocks.iter().filter_map(|b| match b {
            ContentBlock::ToolUse { id, name, input } => Some((id.clone(), name.clone(), input.clone())),
            _ => None,
        }).collect();

        let stop_reason = response.get("stop_reason").and_then(|v| v.as_str()).unwrap_or("");
        if tool_uses.is_empty() || stop_reason != "tool_use" {
            // Final answer. Collect all text.
            let text = blocks.iter().filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            }).collect::<Vec<_>>().join("\n");
            let new_messages = history[trail_start..].to_vec();
            return Ok(TurnResult { text, tool_calls, new_messages });
        }

        // Execute each tool call and send results back in one user message.
        let mut result_blocks = Vec::new();
        for (id, name, input) in tool_uses {
            let (result, is_error) = dispatch_tool(sys, &name, &input);
            tool_calls.push(ToolCallRecord {
                name: name.clone(),
                input: input.clone(),
                result: result.clone(),
                is_error,
            });
            result_blocks.push(ContentBlock::ToolResult {
                tool_use_id: id,
                content: result,
                is_error,
            });
        }
        history.push(Message { role: "user".into(), content: result_blocks });
    }

    Err(AgentError::ToolLoopExceeded(MAX_TOOL_ITERATIONS))
}

fn parse_content(response: &Value) -> Result<Vec<ContentBlock>, AgentError> {
    let arr = response.get("content").and_then(|v| v.as_array())
        .ok_or_else(|| AgentError::Malformed("response.content is not an array".into()))?;
    let mut out = Vec::with_capacity(arr.len());
    for block in arr {
        let ty = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match ty {
            "text" => {
                let text = block.get("text").and_then(|v| v.as_str()).unwrap_or("").to_string();
                out.push(ContentBlock::Text { text });
            }
            "tool_use" => {
                let id = block.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let name = block.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let input = block.get("input").cloned().unwrap_or(Value::Null);
                out.push(ContentBlock::ToolUse { id, name, input });
            }
            other => {
                // Ignore unknown block types (e.g. thinking, redacted) for forward compat.
                eprintln!("[agent] ignoring unknown content block type: {other}");
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod llm_available_tests {
    use super::llm_available;
    use crate::config::KannakaConfig;

    /// A keyless node must report NO capability. This is the whole point of the
    /// `swarm serve` gate: O1 and O3 both served `kannaka-prime` in one NATS
    /// queue group while only O1 had a provider, so ~half of all asks
    /// round-robined onto O3 and came back "no LLM provider configured".
    #[test]
    fn empty_provider_is_unavailable() {
        let mut cfg = KannakaConfig::default();
        cfg.llm.provider = String::new();
        assert!(!llm_available(&cfg), "an empty provider cannot answer an ask");

        cfg.llm.provider = "none".to_string();
        assert!(!llm_available(&cfg), "provider `none` cannot answer an ask");
    }

    /// A configured provider WITH a key is available. Asserting the positive
    /// direction too, so the gate cannot be satisfied by always returning
    /// false — which would take every responder off the ask queue.
    #[test]
    fn configured_provider_with_key_is_available() {
        let mut cfg = KannakaConfig::default();
        cfg.llm.provider = "anthropic".to_string();
        cfg.llm.api_key = "sk-ant-test-not-a-real-key".to_string();
        assert!(llm_available(&cfg));

        // Ollama is the local path — no key required, so it is available on
        // provider alone. A gate that demanded an api_key would wrongly
        // exclude every local-model node.
        let mut local = KannakaConfig::default();
        local.llm.provider = "ollama".to_string();
        assert!(llm_available(&local), "ollama needs no api_key");
    }

    /// An unsupported provider name is not a capability either — better to
    /// decline the queue than to join it and fail every request.
    #[test]
    fn unsupported_provider_is_unavailable() {
        let mut cfg = KannakaConfig::default();
        cfg.llm.provider = "telepathy".to_string();
        assert!(!llm_available(&cfg));
    }
}

#[cfg(test)]
mod remote_ask_mode_tests {
    use super::RemoteAskMode;

    /// The compatibility keystone (#746): an absent `mode` — every pre-#746
    /// client — must resolve to what those servers already did. If this
    /// regresses, upgrading a server silently changes how it answers every old
    /// client on the constellation.
    #[test]
    fn absent_mode_resolves_to_legacy_full_recall() {
        assert_eq!(RemoteAskMode::from_wire(None), RemoteAskMode::FullRecall);
    }

    /// An UNRECOGNISED mode must also fall back rather than error, so a future
    /// mode name cannot break an older-but-mode-aware peer.
    #[test]
    fn unknown_mode_falls_back_instead_of_failing() {
        assert_eq!(
            RemoteAskMode::from_wire(Some("telepathy")),
            RemoteAskMode::FullRecall,
            "an unknown mode must degrade, never reject — old peers must survive new vocabulary"
        );
    }

    /// Wire names round-trip. These strings are the protocol; a typo on either
    /// side would silently downgrade every ask to full recall.
    #[test]
    fn wire_names_round_trip() {
        for m in [RemoteAskMode::Attention, RemoteAskMode::NoRecall, RemoteAskMode::FullRecall] {
            assert_eq!(RemoteAskMode::from_wire(Some(m.wire_name())), m, "{m:?}");
        }
    }

    /// `full_recall` must NOT echo as `full_recall`. The server runs the scan
    /// but not the tool loop, and claiming the plain name would overclaim
    /// exactly the capability that was deliberately withheld.
    #[test]
    fn full_recall_echo_is_explicit_about_no_tools() {
        assert_eq!(RemoteAskMode::FullRecall.mode_used_name(), "full_recall_no_tools");
        assert_ne!(
            RemoteAskMode::FullRecall.mode_used_name(),
            RemoteAskMode::FullRecall.wire_name(),
            "the echo must not claim the tool loop ran"
        );
        // The two non-tool modes are served exactly as named.
        assert_eq!(RemoteAskMode::Attention.mode_used_name(), "attention");
        assert_eq!(RemoteAskMode::NoRecall.mode_used_name(), "no_recall");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    use crate::config::AgentConfig;

    fn agent(id: &str, display: &str, persona: &str) -> AgentConfig {
        AgentConfig {
            id: id.to_string(),
            display_name: display.to_string(),
            kind: "agent".to_string(),
            persona: persona.to_string(),
        }
    }

    // #412 — the self must come from the agent's OWN config, never Kannaka's
    // biography by default.
    #[test]
    fn identity_is_the_served_agent_not_kannaka_by_default() {
        // A persona-less agent describes itself honestly by its own name/id,
        // and does NOT claim to be Kannaka or a "wave-interference memory
        // system".
        let p = identity_paragraph(&agent("0xSCADA-QE", "", ""), 1582);
        assert!(p.contains("You are 0xSCADA-QE"), "must name itself: {p}");
        assert!(p.contains("1582"), "must reflect its own store size");
        // Naming "the Kannaka constellation" is fine; CLAIMING TO BE Kannaka
        // (the actual bug) is not.
        assert!(!p.contains("You are Kannaka"), "must NOT claim to be Kannaka: {p}");
        assert!(!p.contains("wave-interference memory system"), "must NOT borrow Kannaka's biography: {p}");
    }

    #[test]
    fn a_configured_persona_wins() {
        let rich = "You are Kannaka — you are the medium, speaking.";
        let p = identity_paragraph(&agent("kannaka", "Kannaka", rich), 654);
        assert_eq!(p, rich);
    }

    #[test]
    fn display_name_is_preferred_over_id_when_present() {
        let p = identity_paragraph(&agent("kannaka-prime", "Kannaka Prime", ""), 413);
        assert!(p.contains("You are Kannaka Prime"), "{p}");
    }

    fn temp_sys(tag: &str) -> (KannakaMemorySystem, std::path::PathBuf) {
        let dir = std::env::temp_dir()
            .join(format!("kannaka_agent_test_{}_{}", tag, uuid::Uuid::new_v4()));
        // auto_save is private outside openclaw; harmless here since each
        // test writes into its own temp dir.
        let sys = KannakaMemorySystem::init(dir.clone()).expect("init");
        (sys, dir)
    }

    /// #669 — the reported bug: `mode` was read, echoed into the reply, and
    /// then discarded. The label must now describe what ACTUALLY ran, which is
    /// what makes an unrecognised mode discriminating: the old code echoed it
    /// verbatim ("dream (medium)") while running deep.
    #[test]
    fn dream_tool_labels_the_mode_that_actually_ran() {
        let (mut sys, dir) = temp_sys("label");
        let (out, is_err) = dispatch_tool(&mut sys, "dream", &json!({ "mode": "medium" }));
        assert!(!is_err, "dream should succeed: {out}");
        assert!(
            out.starts_with("dream (deep):"),
            "an unrecognised mode must report the mode that ran, got: {out}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dream_tool_accepts_lite_and_deep() {
        let (mut sys, dir) = temp_sys("modes");

        let (lite, err) = dispatch_tool(&mut sys, "dream", &json!({ "mode": "lite" }));
        assert!(!err, "lite dream should succeed: {lite}");
        assert!(lite.starts_with("dream (lite):"), "got: {lite}");

        let (deep, err) = dispatch_tool(&mut sys, "dream", &json!({ "mode": "deep" }));
        assert!(!err, "deep dream should succeed: {deep}");
        assert!(deep.starts_with("dream (deep):"), "got: {deep}");

        // Absent mode keeps the schema's documented default.
        let (dflt, err) = dispatch_tool(&mut sys, "dream", &json!({}));
        assert!(!err);
        assert!(dflt.starts_with("dream (deep):"), "got: {dflt}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Pins the ROUTING, which the label alone cannot prove: before the fix the
    /// label came from the request, so "dream (lite)" was printable without a
    /// lite dream ever running.
    #[test]
    fn dream_tool_dispatches_lite_to_dream_lite() {
        let src = include_str!("agent.rs");
        let start = src.find("\"dream\" => {").expect("dream arm not found");
        let end = src[start..]
            .find("\"orchestrate_run\" =>")
            .map(|i| start + i)
            .expect("end of dream arm not found");
        let arm = &src[start..end];
        assert!(
            arm.contains("sys.dream_lite()"),
            "the dream tool must route lite to dream_lite, not report it and run deep (#669)"
        );
        assert!(arm.contains("sys.dream()"), "deep must still be reachable");
    }
}
