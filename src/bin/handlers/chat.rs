//! `kannaka chat` REPL — interactive chat loop with attention-driven
//! resonance and slash commands for direct medium ops.
//!
//! Extracted from `bin/kannaka.rs` in v0.3.26 following the pattern
//! documented in `handlers/substrate.rs`.

use super::KannakaConfig;

/// The kannaka chat REPL. HRM is loaded ONCE at process startup (the
/// expensive bit — 15s for a mature medium), then every turn reuses
/// the in-memory `sys`. Compared to shelling out `kannaka ask` per
/// turn (which reloads HRM every time), this drops per-turn latency
/// from ~30s to ~3-5s — the difference is exactly the cold-start
/// load. The system prompt is built once from current state and
/// stays stable for the session.
///
/// `--json` mode: line-delimited JSON I/O for embedding into a parent
/// process (the TUI, in particular). Each stdin line is one user
/// message (or starts with `/` for a slash command); each response is
/// one JSON line: `{"text":"...","kind":"chat"|"slash"|"error"}`. No
/// prompts ("you> ") are written, no welcome banner — pure protocol.
pub(crate) fn handle_chat(
    sys: &mut kannaka_memory::openclaw::KannakaMemorySystem,
    cfg: &KannakaConfig,
    args: &[String],
) {
    use std::io::{BufRead, Write};
    let json_mode = args.iter().any(|a| a == "--json");

    let system = kannaka_memory::agent::system_prompt(
        sys,
        &cfg.agent,
        &[],
        kannaka_memory::agent::PromptArm::Baseline,
    );
    let mut history: Vec<kannaka_memory::agent::Message> = Vec::new();

    if !json_mode {
        eprintln!("\nkannaka chat — type /help for commands, Ctrl+D or /quit to exit");
        eprintln!("HRM loaded ({} memories). All turns reuse the loaded medium.",
            sys.all_memories().map(|m| m.len()).unwrap_or(0));
    } else {
        // Banner on stderr so the parent knows the REPL is alive without
        // it polluting the stdout NDJSON stream.
        eprintln!("{}", serde_json::json!({
            "kind": "ready",
            "memories": sys.all_memories().map(|m| m.len()).unwrap_or(0),
        }));
    }

    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();

    let emit = |kind: &str, text: &str| {
        if json_mode {
            let line = serde_json::json!({ "kind": kind, "text": text }).to_string();
            println!("{line}");
        } else {
            match kind {
                "chat" => println!("\nkannaka> {text}"),
                _ => println!("{text}"),
            }
        }
        let _ = std::io::stdout().flush();
    };

    loop {
        if !json_mode {
            print!("\nyou> ");
            let _ = stdout.flush();
        }
        let mut line = String::new();
        match stdin.lock().read_line(&mut line) {
            Ok(0) => { eprintln!(); break; } // EOF
            Ok(_) => {}
            Err(e) => { eprintln!("read error: {e}"); break; }
        }
        let msg = line.trim();
        if msg.is_empty() { continue; }
        if msg == "exit" || msg == "quit" { break; }

        // ── Slash commands ────────────────────────────────────
        // Everything works through chat per the v0.3.x direction.
        // Slash-prefixed lines bypass the LLM and act directly on
        // the loaded medium. Useful for the operator-style verbs
        // (remember/forget/observe/dream) that don't need a model.
        if let Some(stripped) = msg.strip_prefix('/') {
            let mut parts = stripped.splitn(2, char::is_whitespace);
            let cmd = parts.next().unwrap_or("");
            let rest = parts.next().unwrap_or("").trim();
            let (kind, text): (&str, String) = match cmd {
                "help" | "h" | "?" => ("slash", concat!(
                    "commands:\n",
                    "  /remember <text>    absorb a new memory\n",
                    "  /recall <query>     full-recall, surfaces top-k resonant memories\n",
                    "  /forget <uuid>      remove a memory by id\n",
                    "  /status             show consciousness metrics (cached)\n",
                    "  /observe            dump medium state (json)\n",
                    "  /count              memory count\n",
                    "  /history clear      drop conversation history (medium untouched)\n",
                    "  /quit               exit (Ctrl+D also works)\n",
                    "any non-slash line is a chat turn — attention-driven resonance auto-runs.",
                ).to_string()),
                "quit" | "q" => break,
                "history" if rest == "clear" => {
                    history.clear();
                    let n = sys.all_memories().map(|m| m.len()).unwrap_or(0);
                    ("slash", format!("[history cleared — {n} memories still loaded]"))
                }
                "count" => {
                    let n = sys.all_memories().map(|m| m.len()).unwrap_or(0);
                    ("slash", format!("memories: {n}"))
                }
                "status" => {
                    let n = sys.all_memories().map(|m| m.len()).unwrap_or(0);
                    let t = if let Some(m) = sys.engine.store.try_cached_consciousness_metrics() {
                        format!("Φ={:.3}  Ξ={:.3}  order={:.3}  clusters={}  memories={}",
                            m.phi, m.xi, m.order, m.num_clusters, n)
                    } else {
                        format!("[no cached metrics — run `kannaka status` once in another shell to seed the sidecar. memories={n}]")
                    };
                    ("slash", t)
                }
                "remember" if !rest.is_empty() => match sys.with_write_origin(
                    kannaka_memory::remember_events::VIA_CHAT,
                    |s| s.remember(rest),
                ) {
                    Ok(id) => ("slash", format!("[absorbed {id}]")),
                    Err(e) => ("error", format!("remember error: {e}")),
                },
                "recall" if !rest.is_empty() => match sys.recall(rest, kannaka_memory::agent::DEFAULT_TOP_K) {
                    Ok(results) if results.is_empty() => ("slash", format!("[no resonance for '{rest}']")),
                    Ok(results) => {
                        let mut lines = Vec::with_capacity(results.len());
                        for (i, r) in results.iter().enumerate() {
                            let preview = if r.content.len() > 120 {
                                format!("{}…", &r.content[..r.content.floor_char_boundary(120)])
                            } else { r.content.clone() };
                            lines.push(format!("  {}. [{:.3}] {}", i + 1, r.strength, preview));
                        }
                        ("slash", lines.join("\n"))
                    }
                    Err(e) => ("error", format!("recall error: {e}")),
                },
                "forget" if !rest.is_empty() => match uuid::Uuid::parse_str(rest) {
                    // #1157: refuse while another writer holds the store (its
                    // next save would bring the row back after the forget
                    // event said it was gone), save at once instead of at
                    // exit, and say whether the forget event went out.
                    Ok(id) => match super::try_acquire_write_lock() {
                        None => (
                            "error",
                            "forget refused: another process holds the HRM write lock \
                             (a swarm join writer or a dream); its next save would undo the delete"
                                .to_string(),
                        ),
                        Some(_lock) => match sys.with_forget_origin(
                            kannaka_memory::remember_events::VIA_CHAT,
                            kannaka_memory::forget_events::REASON_OPERATOR,
                            |s| {
                                let removed = s.forget(&id)?;
                                if removed {
                                    s.save()?;
                                }
                                Ok::<bool, kannaka_memory::openclaw::SystemError>(removed)
                            },
                        ) {
                            Ok(true) => (
                                "slash",
                                format!("[forgot {id}] {}", sys.take_forget_outcome().summary_line()),
                            ),
                            Ok(false) => ("error", format!("forget: no memory {id}")),
                            Err(e) => ("error", format!("forget error: {e}")),
                        },
                    },
                    Err(_) => ("error", "forget: invalid uuid".to_string()),
                },
                "observe" => {
                    let report = sys.observe();
                    match serde_json::to_string_pretty(&report) {
                        Ok(s) => ("slash", s),
                        Err(e) => ("error", format!("observe serialize error: {e}")),
                    }
                }
                other => ("error", format!("unknown command: /{other} — type /help")),
            };
            emit(kind, &text);
            continue;
        }

        // ── Normal chat turn ──────────────────────────────────
        // Stream tokens as they arrive. In --json mode each chunk is
        // emitted as `{"kind":"chunk","text":"..."}` and the full reply
        // is closed with `{"kind":"chat","text":"..."}` (which carries
        // the final assembled text so a consumer that ignored chunks
        // still gets the whole response). In human mode chunks print
        // directly to stdout under the `kannaka> ` prefix.
        if !json_mode {
            print!("\nkannaka> ");
            let _ = std::io::stdout().flush();
        }
        let result = {
            let json_mode_for_cb = json_mode;
            kannaka_memory::agent::chat_turn_streaming(
                sys, cfg, &mut history, &system, msg,
                |chunk: &str| {
                    if json_mode_for_cb {
                        let line = serde_json::json!({ "kind": "chunk", "text": chunk }).to_string();
                        println!("{line}");
                        let _ = std::io::stdout().flush();
                    } else {
                        print!("{chunk}");
                        let _ = std::io::stdout().flush();
                    }
                },
            )
        };
        match result {
            Ok(turn) => {
                if json_mode {
                    // Final assembled-text frame so consumers that
                    // skipped chunks still get the whole reply.
                    emit("chat", &turn.text);
                } else {
                    println!(); // newline after the streamed reply
                }
            }
            Err(e) => {
                emit("error", &format!("agent error: {e}"));
                history.pop();
            }
        }
    }
}
