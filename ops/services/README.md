# Constellation services — how the install works

There are **two tiers**, and they answer "where do kannaka-attention and
kannaka-eye live in the install":

## Tier 1 — the kannaka binary (`install.sh` / GitHub Releases)

`install.sh` downloads the `kannaka` binary into `~/.local/bin`. That single
binary already contains:

- the HRM engine, `remember`/`recall`/`dream`/`hear`/`see`/`classify`
- **`kannaka attention serve`** — the sparse-attention beam. The
  **`kannaka-attention` crate is a path dependency compiled into the binary**
  (`Cargo.toml: kannaka-attention = { path = "../kannaka-attention" }`), so
  there is nothing separate to install — installing kannaka installs attention.
- the glyph-gravity recall path (the `glyph` feature is now default).

So: **kannaka-attention is already "in the install."** What's left is choosing
to *run* it as a daemon — that's a systemd unit, not another download.

## Tier 2 — constellation services (per-host daemons)

These are long-running roles a node opts into. Each is the SAME binary invoked
with a different subcommand, wired as a systemd unit:

| Service | Command | Unit |
|---|---|---|
| Attention beam | `kannaka attention serve` | `kannaka-attention.service` (here) |
| Remote recall | `kannaka swarm serve` | `kannaka-swarm-serve.service` |
| Substrate | `kannaka substrate run` | `kannaka-substrate.service` |
| Inbox | `kannaka inbox serve` | `kannaka-inbox.service` |

`kannaka-eye` is the exception: it's a **separate Node service** (not part of
the binary — it has a WebGL UI and its own glyph emitter). It's deployed from
its own repo (`kannaka-labs/kannaka-eye`, see that repo's `ops/`), and it's the
*producer* whose glyph events `attention serve` consumes. Eye → glyph →
`KANNAKA.attention.eye` → attention beam → instant recall.

## Install the attention daemon

```bash
# binary first (if not already):  curl -sSf https://install.ninja-portal.com/kannaka | sh
bash ops/services/install-services.sh          # installs + enables kannaka-attention
```

Or by hand:

```bash
sudo install -m644 ops/services/kannaka-attention.service /etc/systemd/system/
sudo systemctl daemon-reload && sudo systemctl enable --now kannaka-attention
```

Gravity is on at gain 0.5 via the unit's `KANNAKA_GLYPH_GRAVITY`; set it to 0 to
disable without code changes. On a build node, point `ExecStart` at the cargo
target binary instead of `~/.local/bin/kannaka`.

## Attention-as-gravity: enabling `KANNAKA_GLYPH_GRAVITY`

`KANNAKA_GLYPH_GRAVITY=<gain>` turns "folded information acts as gravity" on.
It is read at runtime from the environment and gates **two** behaviours, both
keyed on a memory's dominant **Fano line** (0..6, the argmax of the 7-line glyph
signature — `glyph_bridge::fano_line_of`):

1. **Beam pull** (`kannaka attention serve`): each incoming eye glyph on line L
   pulls the same-line memories (`Medium::ids_by_fano_line(L)`) into the
   attention beam, so that perception's whole neighbourhood is "in attention".
2. **Recall boost** (any recall path — `Medium::recall_against` /
   `ChiralMedium::recall`): candidates whose Fano line matches the query's are
   scaled by `(1 + gain)`, so same-line memories gravitate to the top.

Key facts:

- **Default is `0.0` = fully inert** — byte-for-byte the pre-glyph behaviour.
  Nothing changes until a service opts in, so it is safe to ship on by default
  in code and enable per-host.
- **Set it on every process that should feel gravity**, not just the beam:
  - `kannaka-attention.service` (beam pull + recall) — set here already.
  - `kannaka-swarm-serve.service` (remote recall) — set it there too, or
    swarm-routed recalls won't get the boost.
- **Typical value `0.5`** (same-line resonance ×1.5). Higher values pull harder;
  `0` disables without a rebuild.
- **Requires the `glyph` feature** (default-on) and a **producer**: kannaka-eye
  must be publishing `KANNAKA.attention.eye`, else the beam never warms and
  gravity has nothing to pull. If NATS is down, `attention serve` logs a loud
  `FATAL: NATS unavailable … attention-as-gravity OFFLINE` and exits for
  supervisor restart (`Restart=always`).

### Producer prerequisite (the eye feeder)

Gravity is a *consumer*-side switch; it does nothing without a producer. The
`kannaka-eye` service (its own repo) must be running with its feeder cron
POSTing now-playing / observed content to `/api/process`, which publishes a
glyph to `KANNAKA.attention.eye`. No eye → no glyphs → the beam never warms.

### Beam export

`attention serve` writes the live beam to the path in
`KANNAKA_ATTENTION_BEAM_FILE` (the unit sets `%h/.kannaka/attention-beam.json`;
default `/tmp/kannaka-attention-beam.json`, or `C:\Users\Public\...` on Windows).
The observatory polls this file to render the beam.

### Verifying it's live

1. **Startup line** — `journalctl -u kannaka-attention` shows exactly one of:
   - `glyph-gravity ENABLED (KANNAKA_GLYPH_GRAVITY=0.5) …`, or
   - `glyph-gravity DISABLED (KANNAKA_GLYPH_GRAVITY unset/0) …`
   so a quiet beam can't be mistaken for a disabled loop.
2. **Per-glyph pulls** — with gravity on and the eye publishing, the log emits
   `glyph-gravity: line <L> pulled <N> same-line memories into beam`.
3. **Beam file** — `jq .stats $KANNAKA_ATTENTION_BEAM_FILE` shows `beam_size` /
   `observations` climbing above 0 as glyphs arrive.
4. **NATS down** is loud, not silent: `FATAL: NATS unavailable … attention-as-
   gravity OFFLINE`, then a supervisor restart.

The full loop is regression-tested end to end — eye envelope → `event_dominant_
fano_line` → `ids_by_fano_line` well → `AttentionBeam` → `recall_against_ids`
same-line boost — in `tests/attention_gravity_e2e.rs`.
