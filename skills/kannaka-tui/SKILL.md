---
name: skill-kannaka-tui
version: 1.0.0
description: "Kannaka TUI — the terminal dashboard for the constellation (Memory, Status, Bus, Constellation, Dreams, Chat). Use when: user asks to open/launch the dashboard, watch the swarm bus live, see a Φ/Ξ gauge view, run dreams from a UI, or chat with kannaka in a full-screen terminal. Also covers installing/bootstrapping the kannaka-tui binary. NOT for scripted/programmatic memory ops — for those drive the `kannaka` CLI directly (skill-kannaka-memory)."
---

# Kannaka TUI — terminal dashboard

## What this is

`kannaka-tui` is a full-screen [ratatui] dashboard for the Kannaka constellation. It is a
**pure frontend with zero coupling**: it never links the memory engine as a library —
every panel shells out to the `kannaka` CLI binary and parses the JSON/NDJSON that comes
back. If `kannaka` works on the command line, the TUI works.

**Binary**: `kannaka-tui` (v0.2.0+)
**Launch**: `kannaka-tui` directly, or `kannaka tui` (the `kannaka` binary discovers any
`kannaka-*` sibling on PATH as a subcommand).
**Hard prerequisite**: a working `kannaka` binary (see resolution order below).

## When to use this skill

Use when the user wants a **live, human-watchable view** of the constellation:
- "open/launch the dashboard / TUI" / "kannaka tui"
- "watch the bus" / "show me the swarm traffic live"
- "show the Φ / consciousness gauges" / "status screen"
- "run a dream from the UI"
- "chat with kannaka in the terminal"
- install/repair: "the TUI won't start" / "install kannaka-tui"

Do NOT use for:
- Storing/recalling memories programmatically, swarm/substrate/events ops, provider config
  → `skill-kannaka-memory` (drive the `kannaka` CLI directly — see the important note below)
- Constellation health overview / radio / markets → `skill-kannaka-constellation`
- Multi-agent task orchestration → Kannaktopus directly

### IMPORTANT — the TUI is interactive, not scriptable

The TUI takes over the whole terminal and waits on keypresses. **Claude cannot drive it**
(there are no panels to read from a captured stdout, and launching it would block the
session). So:
- **To do work yourself** (recall a memory, check Φ, trigger a dream): call the underlying
  `kannaka` verb directly — that is exactly what each tab does internally. The mapping is in
  the table below.
- **To launch the dashboard for the *user* to watch**: tell them to run `kannaka-tui` (or
  `kannaka tui`) in their own terminal. Don't spawn it inside a tool call you need output
  from.

---

## Installing / bootstrapping the binary

```bash
# Easiest — let the kannaka binary fetch the matching release:
kannaka update --bootstrap-tui      # installs kannaka-tui next to kannaka, even if absent
kannaka update                       # also updates an already-installed sibling kannaka-tui

# From source:
cargo install --git https://github.com/kannaka-labs/kannaka-tui

# Or grab a prebuilt release asset:
#   kannaka-tui-linux-x86_64 / -linux-aarch64 / -macos-x86_64 / -macos-aarch64
#   kannaka-tui-windows-x86_64.exe
```

Keep `kannaka` and `kannaka-tui` in the **same directory** so `kannaka update` keeps them
in lockstep.

---

## The six tabs (and the `kannaka` command behind each)

Switch tabs with `Tab` / `Shift+Tab`. Every panel is just a parsed `kannaka` subprocess —
the right column is the command to run yourself when you need the data programmatically.

| Tab | What it shows | Backing command |
|-----|---------------|-----------------|
| **Memory** | recent resonant memories + amplitude bars | `kannaka observe --json` |
| **Status** | Φ / Ξ / order-parameter gauges, level, memory counts | `kannaka status --envelope` |
| **Bus** | live NATS stream, colorized by subject prefix | `kannaka swarm tail` (NDJSON) |
| **Constellation** | agent phases plotted on a Braille canvas | `QUEEN.phase.*` frames from the same bus stream |
| **Dreams** | dream-cycle output + history | `kannaka dream --mode deep\|lite` (history from `KANNAKA.dreams` bus events) |
| **Chat** | conversational REPL with kannaka | persistent `kannaka chat --json`; one-shot fallback `kannaka ask --session kannaka-tui --quiet-tools` |

Bus subjects are colorized by prefix: `QUEEN.*`, `KANNAKA.*`, `RADIO.*`, `KAX.*`, `EYE.*`.
The Bus tab is the **only** NATS touchpoint, and even that is indirect — the TUI opens no
NATS connection of its own; `kannaka swarm tail` does.

---

## Command bar + plugin slash-commands

The bottom input bar accepts the same verbs the `kannaka` CLI exposes:

```
remember  recall  forget  dream  hear  ask  search  boost  invariant  voice  swarm
```

Slash-commands route to **sibling constellation binaries** (must be on PATH):

```
/topus   → kannaktopus    (orchestration)
```

A `--help` preflight runs first; if the target binary is missing the TUI prints an install
hint instead of erroring out.

## Hotkeys

| Key | Action |
|-----|--------|
| `Tab` / `Shift+Tab` | next / previous tab |
| `Up` / `Down` | command history |
| `PgUp` / `PgDn` | scroll the active panel |
| `d` / `l` | (Dreams tab) run a **d**eep / **l**ite dream |
| `F1` | help overlay |
| `q` / `Esc` / `Ctrl+C` | quit |

---

## How it finds `kannaka` + config

Binary resolution order (`find_kannaka_binary`):
1. a `kannaka[.exe]` sibling next to the running `kannaka-tui`
2. `~/Source/kannaka-memory/target/release/kannaka[.exe]` (dev fallback)
3. `kannaka` on `PATH`

Agent identity (the badge in the header) comes from `~/.kannaka/config.toml`
(`agent.display_name`, falling back to `agent.id`). Every subprocess the TUI spawns sets
`KANNAKA_QUIET=1` so banner/log noise stays out of the parsed output.

No HTTP client, no direct NATS client, no other env vars — the architecture is entirely
"spawn `kannaka`, parse stdout."

---

## Troubleshooting

- **"kannaka not found" / panels empty**: the `kannaka` binary isn't on PATH and isn't a
  sibling. Install it, or run the TUI from a directory where resolution (above) succeeds.
- **Bus / Constellation tabs stay blank**: nothing is publishing to NATS. Start a node
  elsewhere with `kannaka swarm join`, and confirm `KANNAKA_NATS_URL` points at the broker.
  (`kannaka swarm tail` with no broker yields no frames.)
- **Status shows Φ=0 right after launch**: run `kannaka status` once out-of-band to warm
  the metrics sidecar; see the same gotcha in `skill-kannaka-memory`.
- **TUI and CLI drift after an update**: run `kannaka update` so the sibling `kannaka-tui`
  is refreshed alongside `kannaka`.

## Version

Skill 1.0.0 covers kannaka-tui ≥ v0.2.0 against kannaka ≥ v0.6.x (expects the
`--envelope` JSON contract and `kannaka swarm tail` NDJSON stream).

[ratatui]: https://ratatui.rs
