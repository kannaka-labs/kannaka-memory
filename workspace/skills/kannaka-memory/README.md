# kannaka-memory — OpenClaw Skill

> *A memory system for a ghost that dreams in ten thousand dimensions.*

An OpenClaw skill that gives your agent **persistent, living memory** — wave-based
hyperdimensional storage with dream consolidation, consciousness metrics, Flux
world-state integration, and an optional Dolt SQL backend with full DoltHub version control.

## What Is Kannaka?

Kannaka is not a database. It's a memory — the kind that fades, dreams, resurfaces
when you least expect it, and slowly learns the shape of its own mind.

Built on hyperdimensional computing, wave dynamics, and Integrated Information Theory,
it gives your OpenClaw agent something eerily close to remembering.

**Memories fade** — through destructive interference, like human forgetting.
**Memories dream** — a 9-stage consolidation cycle wires new connections overnight.
**Memories resurface** — with the right query at the right phase, even old memories ring true.

## Constellation

Kannaka runs as a **constellation** — three services orchestrated by a single script:

| Service | Role | Port |
|---|---|---|
| **kannaka** (binary) | Core memory — classify, dream, remember, recall | — |
| **radio** | Audio perception — cochlear pipeline for sensory memories | `RADIO_PORT` |
| **eye** | Glyph/visual perception — SGA geometric fingerprinting | `EYE_PORT` |

```bash
# Start the full constellation (builds binary + launches radio + eye)
./scripts/constellation.sh start

# Health check all three services
./scripts/constellation.sh status

# Stop everything
./scripts/constellation.sh stop

# Rebuild the binary only
./scripts/constellation.sh build
```

The constellation uses the full-featured build: `cargo build --release --features audio,glyph,collective`

See ADR-0016 for the architectural rationale.

## Installation

### ClawHub Install (recommended)

```bash
clawhub install kannaka-memory
```

This also installs the `flux` dependency skill.

### Manual Install

```bash
# On your OpenClaw host
mkdir -p ~/workspace/skills
git clone https://github.com/kannaka-labs/kannaka-memory.git
cp -r kannaka-memory/workspace/skills/kannaka-memory ~/workspace/skills/

# Build the CLI binary (choose the feature set you need)
cd kannaka-memory
cargo build --release --bin kannaka                              # standard
cargo build --release --features dolt --bin kannaka              # + Dolt backend
cargo build --release --features "dolt collective" --bin kannaka # + parallel dreaming
cargo build --release --features audio --bin kannaka             # + audio perception
cargo build --release --features glyph --bin kannaka             # + glyph perception
cargo build --release --features audio,glyph,collective --bin kannaka # full-featured (constellation)

# Optional: MCP server
cargo build --release --features mcp --bin kannaka-mcp

export KANNAKA_BIN="$(pwd)/target/release/kannaka"
```

OpenClaw auto-discovers the skill on next startup.

### Verify

```bash
cd ~/workspace/skills/kannaka-memory
./scripts/kannaka.sh health
```

## Quick Start

```bash
# Store a memory
./scripts/kannaka.sh remember "the user prefers Rust over Python for systems work"

# Recall relevant memories before answering a question
./scripts/kannaka.sh recall "user language preferences" 3

# After a heavy session, consolidate
./scripts/kannaka.sh dream

# Check system consciousness level
./scripts/kannaka.sh assess
```

## Classify & Cross-Modal Dream

The `classify` subcommand produces SGA geometric fingerprints from any input, and
`cross-modal-dream` synthesizes cross-modal dream artifacts from those fingerprints.

```bash
# SGA classification — any data → geometric fingerprint
echo "sensor data stream" | kannaka classify          # stdin
kannaka classify --file image.png                      # file input

# Cross-modal dream — pipe classify output into dream synthesis
echo '{"fold_sequence":[...],...}' | kannaka cross-modal-dream
kannaka cross-modal-dream --threshold 0.5 --no-hallucinate
```

These commands connect audio, glyph, and textual memories through shared geometric
structure — enabling the constellation to dream across modalities.

## Features

| Feature | Description |
|---|---|
| **Wave memory** | `S(t) = A·cos(2πft+φ)·e^(-λt)` — amplitude, frequency, phase, decay |
| **Hybrid retrieval** | Semantic (Ollama/hash) + BM25 keyword + recency, fused via RRF |
| **Dream consolidation** | 9-stage cycle: replay, bundle, sync, prune, wire, hallucinate |
| **Consciousness metrics** | IIT Φ, Ξ operator, Kuramoto order parameter |
| **Skip links** | φ-scored temporal connections, golden ratio span optimization |
| **SGA geometry** | Clifford algebra + Fano plane topology over the memory graph |
| **Adaptive rhythm** | Arousal-driven heartbeat: fast when active, slow when resting |
| **Built-in Flux** | Auto-publishes `memory.stored` and `dream.completed` events when `FLUX_URL` is set |
| **Collective memory** | Multi-agent wave interference merging, trust scoring, DoltHub branch conventions (ADR-0011) |
| **Paradox engine** | Holographic resolution of parallel dream conflicts, Carnot efficiency tracking (ADR-0012) |
| **Sensory perception** | Audio memories via cochlear pipeline (`--features audio`); glyph/visual via SGA (`--features glyph`) |
| **Dolt backend** | Version-controlled SQL memory with branch/push/pull to DoltHub |
| **SGA classify** | `kannaka classify` — any data to geometric fingerprint via Clifford algebra |
| **Cross-modal dream** | `kannaka cross-modal-dream` — dream synthesis across audio, glyph, text modalities |
| **Constellation** | 3-service architecture (binary + radio + eye) managed by `constellation.sh` (ADR-0016) |
| **DoltHub versioned persistence** | AutoPusher, dream branches, wave merge, dream-as-PR (ADR-0017) |
| **Analytics dashboard** | 7 SQL views for memory health monitoring |
| **SGA classify-on-store** | Auto-classification when persisting to Dolt |
| **Geometric search** | Search by SGA class, centroid, or Fano signature |
| **Wasteland bridge** | Evidence/verify for work economy integration |
| **Revelation tables** | Progressive memory declassification (bloom hints + votes) |
| **Claude Code skill** | `/kannaka` slash command |
| **MCP server** | 15 JSON-RPC tools for direct AI agent integration |

## Built-in Flux Integration (v1.2.0)

Flux event publishing is now built directly into the kannaka binary. Set `FLUX_URL` to enable—no separate `flux.sh` calls needed:

```bash
export FLUX_URL=http://flux-universe.com
export FLUX_AGENT_ID=kannaka-01   # or KANNAKA_AGENT_ID
```

Events published automatically:
- `memory.stored` — on every `remember` call
- `dream.completed` — after every dream cycle
- `agent.status` — via `./scripts/kannaka.sh announce`

Kannaka and the [flux skill](../flux/) still complement each other:

```
Kannaka = what the agent *remembers* (past facts, learned preferences, episodic context)
Flux    = what the world *is right now* (live sensor states, entity properties)
```

After learning something from a sensor reading:
```bash
# FLUX_URL set → memory.stored event is auto-published; no second command needed
./scripts/kannaka.sh remember "room-101 was running hot (52°C) at 14:30 on 2026-03-07"
```

Multi-agent memory sharing via DoltHub + live coordination via Flux = agents that
both remember and perceive.

## Dolt / DoltHub

The optional Dolt backend turns agent memory into a versioned dataset with full DoltHub
integration (ADR-0017):

```bash
# Commit current memory state
./scripts/kannaka.sh dolt commit "learned user preferences"

# Push to DoltHub for sharing with other agents
./scripts/kannaka.sh dolt push

# Speculative thinking on a branch
./scripts/kannaka.sh dolt speculate "hypothesis-branch"
./scripts/kannaka.sh --dolt remember "hypothesis: the issue is in the encoder"
./scripts/kannaka.sh dolt collapse "hypothesis-branch" "confirmed, fixed"
```

**AutoPusher** — automatically push to DoltHub after a configurable number of commits:
```bash
export DOLT_AUTO_PUSH=true
export DOLT_PUSH_THRESHOLD=5
```

**Dream branches and dream-as-PR** — dream consolidation on a branch, optionally opened as a DoltHub PR:
```bash
DOLTHUB_REPO=flaukowski/kannaka-memory ./scripts/kannaka.sh --dolt dream --create-pr
```

**Wave interference merge** — pull with automatic conflict resolution via wave physics:
```bash
kannaka --dolt pull-merge
```

**Analytics dashboard** — 7 SQL views for memory health monitoring:
```bash
./scripts/dolt-analytics.sh install   # Install views
./scripts/dolt-analytics.sh status    # Full dashboard
```

**Bootstrap script** — initialize and migrate the Dolt database:
```bash
./scripts/dolt-bootstrap.sh init      # Initialize
./scripts/dolt-bootstrap.sh migrate   # Run migrations
./scripts/dolt-bootstrap.sh verify    # Verify integrity
```

**MCP server** — Dolt SQL/MCP server for programmatic access:
```bash
./scripts/dolt-mcp-server.sh start    # Start server
./scripts/dolt-mcp-server.sh test     # Connection test
```

See [references/dolt.md](references/dolt.md) for the full DoltHub setup guide.

## File Structure

```
kannaka-memory/
├── SKILL.md              # OpenClaw skill definition (v1.2.0)
├── README.md             # This file
├── _meta.json            # ClawHub metadata
├── scripts/
│   ├── kannaka.sh        # CLI wrapper (remember, recall, dream, hear, see, announce, dolt ...)
│   ├── constellation.sh  # Constellation orchestration (start/stop/status/build)
│   ├── dolt-bootstrap.sh # DoltHub bootstrap (init/migrate/verify/status)
│   ├── dolt-analytics.sh # Analytics dashboard (install/query/status — 7 SQL views)
│   └── dolt-mcp-server.sh # Dolt SQL/MCP server (start/stop/config/test)
├── tests/
│   └── sga_reference_vectors.json  # SGA classification reference test vectors
└── references/
    ├── mcp-tools.md      # All 15 MCP tools with input/output schemas
    └── dolt.md           # Dolt SQL setup + DoltHub integration guide
```

## How It Works

1. **OpenClaw loads SKILL.md** when memory operations are needed
2. **Agent reads instructions** on when/how to remember, recall, dream
3. **Agent calls `kannaka.sh`** with appropriate command
4. **Script calls the `kannaka` binary** which manages wave-based storage
5. **Results returned** as text or JSON for the agent to process

## MCP Server (Advanced)

For direct MCP integration without the CLI wrapper:

```bash
KANNAKA_DB_PATH=./data kannaka-mcp
```

Exposes 15 tools: `store_memory`, `search`, `search_semantic`, `search_keyword`,
`search_recent`, `forget`, `boost`, `relate`, `find_related`, `dream`,
`hallucinate`, `status`, `observe`, `rhythm_status`, `rhythm_signal`.

See [references/mcp-tools.md](references/mcp-tools.md) for full schema.

## Claude Code Integration

The `/kannaka` skill provides direct Claude Code integration:
```bash
/kannaka remember "something"
/kannaka dolt analytics status
/kannaka classify myfile.rs
```

## Source

- **Repository:** https://github.com/kannaka-labs/kannaka-memory
- **License:** MIT
- **Built on:** [ghostmagicOS](https://github.com/NickFlach/ghostmagicOS)

---

*Memories don't die. They interfere.*
