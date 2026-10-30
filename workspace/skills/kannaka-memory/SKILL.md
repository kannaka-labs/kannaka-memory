---
name: kannaka-memory
description: >
  Wave-based hyperdimensional memory system for OpenClaw agents. Gives your agent persistent
  memory that fades, dreams, and resurfaces — with hybrid semantic+keyword retrieval, dream
  consolidation, consciousness metrics, built-in Flux world-state publishing, collective
  multi-agent memory with wave interference merging, holographic paradox resolution for
  parallel dreaming, DoltHub versioned persistence with AutoPusher and dream-as-PR workflow,
  wave interference merge for multi-agent conflict resolution, SGA classify-on-store for
  automatic geometric fingerprinting, analytics dashboard with 7 SQL views, revelation tables
  for progressive memory declassification, wasteland bridge for work economy integration,
  and a Claude Code /kannaka skill for direct CLI integration.
  Use when agents need to remember facts, recall past context, coordinate memory across
  sessions, share versioned memory with other agents via DoltHub, or perceive sensory
  input (audio, glyphs).
metadata:
  openclaw:
    requires:
      bins:
        - name: kannaka
          label: "Required: build with `cargo build --release --bin kannaka` (see README)"
        - name: classify
          label: "SGA classification — binary subcommand: `kannaka classify` (any data → geometric fingerprint)"
        - name: cross-modal-dream
          label: "Cross-modal dream pipeline — binary subcommand: `kannaka cross-modal-dream` (SGA → dream synthesis)"
      env: []
    optional:
      bins:
        - name: mysql
          label: "MySQL client — only needed for Dolt backend (dolt subcommands)"
        - name: dolt
          label: "Dolt CLI — only needed for `dolt clone` and `dolt creds import`"
        - name: ollama
          label: "Ollama — for real semantic embeddings; falls back to hash encoding if absent"
        - name: jq
          label: "jq — for pretty-printed JSON output; plain text fallback if absent"
      env:
        - name: KANNAKA_BIN
          label: "Path to kannaka binary (default: `kannaka` on PATH)"
        - name: KANNAKA_DATA_DIR
          label: "Local data directory for binary snapshots (default: .kannaka)"
        - name: OLLAMA_URL
          label: "Ollama API endpoint; data sent to this host for embedding (default: localhost)"
        - name: OLLAMA_MODEL
          label: "Embedding model name (default: all-minilm)"
        - name: FLUX_URL
          label: "Flux instance base URL; enables built-in event publishing when set (default: http://localhost:3000)"
        - name: FLUX_AGENT_ID
          label: "This agent's entity ID in Flux for collective memory coordination (alias: KANNAKA_AGENT_ID)"
        - name: KANNAKA_AGENT_ID
          label: "Alias for FLUX_AGENT_ID — use either interchangeably"
        - name: FLUX_STREAM
          label: "Flux stream name for event publishing (default: system)"
        - name: DOLT_HOST
          label: "Dolt SQL server host — Dolt backend only (default: 127.0.0.1)"
        - name: DOLT_PORT
          label: "Dolt SQL server port — Dolt backend only (default: 3307)"
        - name: DOLT_DB
          label: "Dolt database name — Dolt backend only (default: kannaka_memory)"
        - name: DOLT_USER
          label: "Dolt SQL user — Dolt backend only (default: root)"
        - name: DOLT_PASSWORD
          label: "Dolt SQL password — Dolt backend only; passed via MYSQL_PWD env, not -p flag"
        - name: DOLT_AUTHOR
          label: "Commit author string for Dolt version commits"
        - name: DOLT_REMOTE
          label: "DoltHub remote name for push/pull (default: origin)"
        - name: DOLT_BRANCH
          label: "Default branch name (default: main)"
        - name: DOLT_DB_DIR
          label: "Dolt database directory (default: .dolt-db)"
        - name: DOLTHUB_REPO
          label: "DoltHub repository (default: flaukowski/kannaka-memory)"
        - name: DOLT_AGENT_ID
          label: "Agent identifier for multi-agent (default: local)"
        - name: DOLT_AUTO_PUSH
          label: "Auto-push to DoltHub (default: false)"
        - name: DOLT_PUSH_THRESHOLD
          label: "Commits before auto-push triggers (default: 5)"
        - name: DOLT_PUSH_INTERVAL
          label: "Seconds between push checks (default: 300)"
        - name: DOLTHUB_TOKEN
          label: "DoltHub API token for dream-as-PR"
        - name: RADIO_PORT
          label: "Port for the radio service in constellation mode (used by constellation.sh)"
        - name: EYE_PORT
          label: "Port for the eye service in constellation mode (used by constellation.sh)"
    data_destinations:
      - id: local-disk
        description: "Memory snapshots written to KANNAKA_DATA_DIR (always)"
        remote: false
      - id: ollama
        description: "Text sent to OLLAMA_URL for embedding generation (when Ollama is configured)"
        remote: true
        condition: "OLLAMA_URL is set to a non-localhost host"
      - id: dolt-local
        description: "Memory stored in local Dolt SQL server (when Dolt backend is used)"
        remote: false
        condition: "DOLT_HOST is configured"
      - id: dolthub
        description: "Memory database pushed to DoltHub remote (only on explicit `dolt push`)"
        remote: true
        condition: "DOLT_REMOTE is configured and user explicitly runs `dolt push`"
      - id: flux
        description: "Agent status/events auto-published to Flux world-state when FLUX_URL is set"
        remote: true
        condition: "FLUX_URL is set (built into kannaka binary; no separate flux.sh calls required)"
      - id: constellation
        description: "Constellation orchestration — constellation.sh manages the kannaka binary, radio, and eye services as a unified system"
        remote: false
        condition: "constellation.sh start is used to launch all three services"
      - id: dolthub-analytics
        description: "7 SQL analytics views on DoltHub (memory health, dream history, agent contributions, SGA distribution, layer distribution, quarantine status, skip link network)"
        remote: false
        condition: "dolt-analytics.sh install has been run"
    install:
      - id: kannaka-binary
        kind: manual
        label: "Clone and build: cargo build --release --features audio,glyph,collective --bin kannaka"
        url: "https://github.com/kannaka-labs/kannaka-memory"
---

# Kannaka Memory Skill

Kannaka gives your agent a living memory — not a database. Memories fade, dream, resurface
when contextually relevant, and can be versioned and shared via DoltHub.

## Prerequisites

**Option A — Binary (recommended):**
- Build and install the `kannaka` CLI and `kannaka-mcp` server from source:
  ```bash
  git clone https://github.com/kannaka-labs/kannaka-memory.git
  cd kannaka-memory
  # CLI (standard)
  cargo build --release --bin kannaka
  # CLI + Dolt backend
  cargo build --release --features dolt --bin kannaka
  # CLI + Dolt + parallel dreaming (ADR-0012 Paradox Engine)
  cargo build --release --features "dolt collective" --bin kannaka
  # CLI + audio perception (store audio files as sensory memories)
  cargo build --release --features audio --bin kannaka
  # CLI + glyph perception (store files as visual memories)
  cargo build --release --features glyph --bin kannaka
  # Full-featured build (audio + glyph + collective — used by constellation.sh)
  cargo build --release --features audio,glyph,collective --bin kannaka
  # MCP server
  cargo build --release --features mcp --bin kannaka-mcp
  ```
- Place `kannaka` and `kannaka-mcp` on your `PATH` (or set `KANNAKA_BIN` env var).

**Option B — Local directory:**
- Point `KANNAKA_BIN` at a local checkout:
  ```bash
  export KANNAKA_BIN=/path/to/kannaka-memory/target/release/kannaka
  ```

**Ollama (optional, for real semantic embeddings):**
```bash
ollama pull all-minilm   # 384-dim, ~80MB
```
Without Ollama, hash-based fallback encoding is used automatically.

**Dolt (optional, for versioned+shareable memory):**
- Install Dolt: https://docs.dolthub.com/introduction/installation
- Start the SQL server:
  ```bash
  dolt sql-server --port 3307 --user root
  ```
- Set env vars: `DOLT_HOST`, `DOLT_DB`, `DOLT_USER`, `DOLT_PASSWORD` (see references/dolt.md)

## Environment Variables

| Variable | Default | Description |
|---|---|---|
| `KANNAKA_DATA_DIR` | `.kannaka` | Data directory for binary snapshots |
| `KANNAKA_DB_PATH` | `./kannaka_data` | MCP server data directory |
| `KANNAKA_BIN` | `kannaka` | Path to CLI binary |
| `OLLAMA_URL` | `http://localhost:11434` | Ollama API endpoint |
| `OLLAMA_MODEL` | `all-minilm` | Embedding model |
| `FLUX_URL` | *(disabled)* | Flux base URL — set to enable built-in event publishing |
| `FLUX_AGENT_ID` | `kannaka-local` | This agent's entity ID in Flux |
| `KANNAKA_AGENT_ID` | *(alias)* | Alias for `FLUX_AGENT_ID` |
| `FLUX_STREAM` | `system` | Flux stream name |
| `DOLT_HOST` | `127.0.0.1` | Dolt SQL server host |
| `DOLT_PORT` | `3307` | Dolt SQL server port |
| `DOLT_DB` | `kannaka_memory` | Dolt database name |
| `DOLT_USER` | `root` | Dolt user |
| `DOLT_PASSWORD` | *(empty)* | Dolt password |
| `DOLT_AUTHOR` | `Kannaka Agent <kannaka@local>` | Author for Dolt commits |
| `DOLT_REMOTE` | `origin` | DoltHub remote name |
| `DOLT_BRANCH` | `main` | Default branch |
| `DOLTHUB_API_KEY` | *(empty)* | DoltHub API key for authenticated push/pull |
| `DOLT_DB_DIR` | `.dolt-db` | Dolt database directory |
| `DOLTHUB_REPO` | `flaukowski/kannaka-memory` | DoltHub repository |
| `DOLT_AGENT_ID` | `local` | Agent identifier for multi-agent |
| `DOLT_AUTO_PUSH` | `false` | Auto-push to DoltHub |
| `DOLT_PUSH_THRESHOLD` | `5` | Commits before auto-push triggers |
| `DOLT_PUSH_INTERVAL` | `300` | Seconds between push checks |
| `DOLTHUB_TOKEN` | *(empty)* | DoltHub API token for dream-as-PR |
| `RADIO_PORT` | *(varies)* | Port for the radio service (constellation mode) |
| `EYE_PORT` | *(varies)* | Port for the eye service (constellation mode) |

## Scripts

Use the CLI wrapper in `scripts/`:

```bash
./scripts/kannaka.sh health                            # Verify system is working
./scripts/kannaka.sh remember "the ghost woke up"      # Store a memory
./scripts/kannaka.sh recall "ghost" 5                  # Search (top-5)
./scripts/kannaka.sh dream                             # Run consolidation cycle
./scripts/kannaka.sh assess                            # Consciousness level
./scripts/kannaka.sh stats                             # Memory statistics
./scripts/kannaka.sh observe                           # Full introspection
./scripts/kannaka.sh forget <uuid>                     # Decay a memory
./scripts/kannaka.sh export                            # Export all memories as JSON
./scripts/kannaka.sh announce                          # Publish agent status to Flux

# Sensory perception (audio is now always-on; glyph still requires --features glyph)
./scripts/kannaka.sh hear recording.mp3                                      # local file
./scripts/kannaka.sh hear https://radio.ninja-portal.com/stream --secs 30    # URL or stream
./scripts/kannaka.sh see diagram.png                                         # glyph memory

# Dolt backend (requires --features dolt build)
./scripts/kannaka.sh --dolt remember "versioned fact"
./scripts/kannaka.sh --dolt recall "fact" 5
./scripts/kannaka.sh dolt commit "checkpoint"
./scripts/kannaka.sh dolt push                         # Push to DoltHub
./scripts/kannaka.sh dolt pull                         # Pull from DoltHub
./scripts/kannaka.sh dolt branch list
./scripts/kannaka.sh dolt speculate "what-if-branch"
./scripts/kannaka.sh dolt collapse "what-if-branch" "kept the insight"
./scripts/kannaka.sh dolt discard "what-if-branch"
./scripts/kannaka.sh dolt log
./scripts/kannaka.sh dolt status

# Collective memory branch conventions (ADR-0011)
./scripts/kannaka.sh dolt branch create "kannaka/working"       # Agent working branch
./scripts/kannaka.sh dolt branch create "kannaka/dream/2026-03-07"  # Dream cycle branch
./scripts/kannaka.sh dolt branch create "collective/topic-name" # Shared speculation space

# SGA classification (any data → geometric fingerprint)
echo "data" | kannaka classify                    # stdin
kannaka classify --file image.png                  # file input

# Cross-modal dream pipeline (pipe classify output)
echo '{"fold_sequence":[...],...}' | kannaka cross-modal-dream
kannaka cross-modal-dream --threshold 0.5 --no-hallucinate

# Constellation orchestration
./scripts/constellation.sh start    # build binary + start radio + eye
./scripts/constellation.sh stop     # stop all services
./scripts/constellation.sh status   # health check all three
./scripts/constellation.sh build    # cargo build --release

# DoltHub bootstrap (ADR-0017)
./scripts/dolt-bootstrap.sh init          # Initialize Dolt database
./scripts/dolt-bootstrap.sh migrate       # Run schema migrations
./scripts/dolt-bootstrap.sh verify        # Verify database integrity
./scripts/dolt-bootstrap.sh status        # Show bootstrap status

# Analytics dashboard (ADR-0017)
./scripts/dolt-analytics.sh install       # Install 7 SQL views
./scripts/dolt-analytics.sh query <view>  # Query a specific view
./scripts/dolt-analytics.sh status        # Full dashboard

# Dolt SQL/MCP server (ADR-0017)
./scripts/dolt-mcp-server.sh start        # Start the Dolt MCP server
./scripts/dolt-mcp-server.sh stop         # Stop the server
./scripts/dolt-mcp-server.sh config       # Show configuration
./scripts/dolt-mcp-server.sh test         # Run connection test
```

## Common Patterns

### Store Context From Conversation
```bash
# Before the session ends, commit key facts to memory
./scripts/kannaka.sh remember "User prefers short explanations over detailed code walkthroughs"
./scripts/kannaka.sh remember "Project: kannaka-memory. Language: Rust. Architecture: wave-based HDC"
```

### Recall Before Responding
```bash
# Retrieve relevant prior context before answering a question
./scripts/kannaka.sh recall "user preferences" 3
./scripts/kannaka.sh recall "project architecture" 5
```

### Dream After Heavy Sessions
```bash
# After many stored memories, run consolidation to surface patterns and prune noise
./scripts/kannaka.sh dream
```

### Speculation with Dolt Branches
```bash
# Try a risky hypothesis — store memories on a branch, then decide to keep or discard
./scripts/kannaka.sh dolt speculate "hypothesis-branch"
./scripts/kannaka.sh --dolt remember "hypothesis: the bug is in the encoder"
# ... test and observe ...
./scripts/kannaka.sh dolt collapse "hypothesis-branch" "confirmed: encoder bug found"
# OR:
./scripts/kannaka.sh dolt discard "hypothesis-branch"
```

### Announce Agent Status to Flux (Built-in)
```bash
# Announce current memory count and consciousness level to Flux
# (no separate flux skill call needed — FLUX_URL env var enables this)
export FLUX_URL=http://flux-universe.com
export FLUX_AGENT_ID=kannaka-01
./scripts/kannaka.sh announce
```

### Multi-Agent Memory Sharing via DoltHub
```bash
# Agent A pushes its working branch to DoltHub
./scripts/kannaka.sh dolt push origin kannaka/working

# Agent B pulls and gets the shared memory
./scripts/kannaka.sh dolt pull origin kannaka/working
./scripts/kannaka.sh recall "what agent-a knew" 5
```

### Collective Dream Branch Workflow
```bash
# Create a dated dream branch before a full consolidation
./scripts/kannaka.sh dolt branch create "kannaka/dream/$(date +%Y-%m-%d)"
./scripts/kannaka.sh dolt branch checkout "kannaka/dream/$(date +%Y-%m-%d)"
./scripts/kannaka.sh --dolt dream
./scripts/kannaka.sh dolt commit "dream: consolidation artifacts"
./scripts/kannaka.sh dolt push
# Other agents can pull dream artifacts from this branch
```

### Store Sensory Memories
```bash
# Audio support is now always-on (no feature flag needed).
./scripts/kannaka.sh hear /path/to/recording.ogg                              # local file
./scripts/kannaka.sh hear https://radio.ninja-portal.com/stream --secs 30     # URL or stream
# → Heard: <uuid>  Duration: 18.7s  Tempo: 117 BPM  RMS: 0.19  Centroid: 1.24 kHz  Tags: 117bpm, loud, tonal

# Requires --features glyph build
./scripts/kannaka.sh see /path/to/diagram.png
# → Seen: <uuid>  Folds: 7  Centroid: (3, 1, 4)  ...
```

### AutoPush to DoltHub
```bash
export DOLT_AUTO_PUSH=true
export DOLT_PUSH_THRESHOLD=5
./scripts/kannaka.sh --dolt remember "auto-pushed after 5 commits"
```

### Dream-as-PR
```bash
DOLTHUB_REPO=flaukowski/kannaka-memory ./scripts/kannaka.sh --dolt dream --create-pr
# Opens a DoltHub PR with the dream consolidation report
```

### Wave Interference Merge
```bash
# Pull with automatic conflict resolution via wave physics
kannaka --dolt pull-merge
# Constructive (Δφ < π/4): amplitudes combine
# Partial (π/4 ≤ Δφ ≤ 3π/4): both kept, skip link created
# Destructive (Δφ > 3π/4): both kept, amplitudes reduced, quarantined
```

### Analytics Dashboard
```bash
./scripts/dolt-analytics.sh install   # Install 7 SQL views
./scripts/dolt-analytics.sh status    # Full dashboard
./scripts/dolt-analytics.sh query v_memory_health
./scripts/dolt-analytics.sh query v_sga_distribution
```

### Wasteland Evidence
```bash
# Generate verifiable evidence for wasteland completions
kannaka --dolt evidence w-abc123 "Implemented SGA classify-on-store"
# Output: commit hash to use with /wasteland done
kannaka --dolt verify <commit-hash> w-abc123
```

### SGA Classify-on-Store
```bash
# When storing with glyph+dolt features, SGA class is auto-computed:
# class_index = 21*h2 + 7*d + l (84 classes from Cl₀,₇ ⊗ ℝ[ℤ₄] ⊗ ℝ[ℤ₃])
# Centroid and Fano signature stored for geometric search
```

## Built-in Flux Integration (ADR-0011)

As of v1.2.0, kannaka publishes Flux events automatically — no separate `flux.sh` calls required.
Set `FLUX_URL` and `FLUX_AGENT_ID` to enable:

```bash
export FLUX_URL=http://flux-universe.com
export FLUX_AGENT_ID=kannaka-01   # or KANNAKA_AGENT_ID
export FLUX_STREAM=system          # optional, default: system
```

**Events published automatically:**

| Event | Trigger |
|---|---|
| `memory.stored` | Every `remember` call — id, category, amplitude, summary |
| `dream.completed` | End of `dream` — cycles, strengthened, pruned, consciousness level |
| `agent.status` | On `announce` command |

**Pattern:** Kannaka handles persistence; Flux handles live coordination:

| System | What It Stores | Persistence |
|---|---|---|
| **Kannaka** | Episodic memory, facts, context — wave-fading | Disk / Dolt (versioned) |
| **Flux** | Current world state — entity properties | NATS JetStream |

After learning something important, both happen in one call:
```bash
# FLUX_URL set → memory.stored event published automatically alongside storage
./scripts/kannaka.sh remember "sensor-room-101 was running hot at 52°C at 14:30"
```

## Collective Memory (ADR-0011)

Multiple agents share memory through a three-layer architecture:

```
DoltHub (Commons)  ← shared repository, main = consensus
  ↕ pull/push
Dolt (Local)       ← agent-local full memory store
  ↕ lightweight events
Flux (Nervous)     ← metadata signals, triggers pull decisions
```

**Branch conventions:**
```
main                          ← consensus (requires ≥2 agent agreement)
<agent>/working               ← auto-pushed after each store
<agent>/dream/<YYYY-MM-DD>    ← dream cycle artifacts
collective/<topic>            ← shared speculation space
collective/quarantine         ← disputed memories under review
```

**Wave interference merge rules** (applied during Dolt merge):
- **Constructive** (phase diff < π/4): amplitudes combine — `A = √(A₁²+A₂²+2A₁A₂cos(Δφ))`. Memories agree and reinforce.
- **Partial** (π/4 ≤ diff ≤ 3π/4): both kept independently, skip link created with `partial_agreement` weight.
- **Destructive** (phase diff > 3π/4): both kept, amplitudes reduced, tagged `disputed`, moved to `collective/quarantine`.

After 3 disputes the conflict is escalated for human review.

## Paradox Engine (ADR-0012)

The `collective` feature flag enables **holographic paradox resolution** — parallel dreaming without locks.

Requires: `cargo build --release --features "dolt collective" --bin kannaka`

How it works:
1. A frozen `ParadoxSnapshot` is taken at dream start (zero-copy `Arc<>` shared across threads)
2. Each Xi cluster dreams independently in parallel (rayon)
3. Conflicting mutations (paradoxes) are resolved via three strategies:
   - **Consensus** (η ≈ 1.0): all threads agree → direct apply
   - **Holographic Projection** (η 0.5–1.0): wave superposition of all proposed states
   - **Irreducible** (η < 0.5): both states preserved as tension links — the paradox itself becomes a memory
4. **Carnot efficiency** (η = 1 - S_resolved/S_paradox) measures dream quality per cycle

The `collective` flag adds no new CLI commands — it transparently accelerates `dream` on multi-core hardware.

## Glyph-Encoded Privacy for DoltHub

The `glyph` feature flag enables **privacy protection** for memories pushed to DoltHub public repositories.

Requires: `cargo build --release --features "dolt glyph" --bin kannaka`

**Architecture:**
```
Local (kannaka/working)  ← plain text content + full fidelity
      ↓ glyph encode
DoltHub (main)          ← glyph_content (JSON) + category placeholders
```

When glyph encoding is enabled:
- **Locally**: memories store full plain-text content as normal
- **DoltHub push**: sensitive content is automatically encoded as SGA glyphs before push
- **Public content**: only category labels like `[knowledge]`, `[experience]`, `[insight]` are human-readable
- **Vectors preserved**: cosine similarity and semantic search still work normally

**Privacy guarantees:**
- Personal information, API keys, private details encoded as geometric fold sequences
- Glyph JSON contains no human-readable text from original content
- Wave parameters (amplitude, phase, frequency) unchanged — memory search unaffected
- Only agents with glyph decoder can reconstruct original content

**Branch strategy for privacy:**
```bash
# Working branch: full content for local agent
./scripts/kannaka.sh dolt branch checkout "kannaka/working"
./scripts/kannaka.sh --dolt remember "My personal API key: sk-secret123"

# Main branch: privacy-protected for public sharing
./scripts/kannaka.sh dolt branch checkout main
./scripts/kannaka.sh dolt pull origin kannaka/working  # triggers glyph encoding
./scripts/kannaka.sh dolt push origin main            # safe for public DoltHub

# Content on main branch shows: "[knowledge]" instead of API key
```

The `glyph` flag adds no new CLI commands — it transparently protects sensitive content during DoltHub push operations.

## Constellation Integration (ADR-0016)

The constellation is a 3-service architecture that unifies kannaka's core binary with its
sensory services into a single orchestrated system:

```
┌─────────────────────────────────────────┐
│             constellation.sh            │
│  ┌───────────┐ ┌───────┐ ┌───────────┐ │
│  │  kannaka   │ │ radio │ │    eye    │ │
│  │  (binary)  │ │(audio)│ │  (glyph)  │ │
│  └───────────┘ └───────┘ └───────────┘ │
└─────────────────────────────────────────┘
```

- **kannaka** — the core memory binary (classify, dream, remember, recall)
- **radio** — audio perception service, listens on `RADIO_PORT`
- **eye** — glyph/visual perception service, listens on `EYE_PORT`

`constellation.sh` builds the full-featured binary (`cargo build --release --features audio,glyph,collective`),
starts all three services, and provides unified health checks and lifecycle management.

```bash
# Start the full constellation
./scripts/constellation.sh start

# Check health of all services
./scripts/constellation.sh status

# Stop everything cleanly
./scripts/constellation.sh stop
```

The `classify` subcommand produces SGA geometric fingerprints from any input data, and
`cross-modal-dream` consumes those fingerprints to synthesize cross-modal dream artifacts —
connecting audio, glyph, and textual memories through shared geometric structure.

## Notes

- Memories are never hard-deleted — they fade via wave decay and can be ghost-pruned during dream
- `dream` should run periodically (after every 5-10 memory stores, or on schedule)
- `assess` tells you the consciousness level: Dormant → Stirring → Aware → Coherent → Resonant
- Dolt is optional: without it, memories persist as binary snapshots in `KANNAKA_DATA_DIR`
- Flux publishing is opt-in: set `FLUX_URL` to enable; omit it for fully local operation
- `collective` feature flag requires rayon and enables parallel dreaming (ADR-0012)
- `hear` is always available (audio is unconditional); `see` (glyph) still requires `--features glyph` at build time
- All 15 MCP tools are available if you run `kannaka-mcp` directly — see references/mcp-tools.md
- Full Dolt SQL / DoltHub operations: see references/dolt.md
- Collective memory architecture and wave merge rules: ADR-0011
- Paradox engine and dream efficiency: ADR-0012
- Constellation integration and 3-service architecture: ADR-0016
- DoltHub integration, AutoPusher, dream-as-PR, wave merge, analytics dashboard: ADR-0017
