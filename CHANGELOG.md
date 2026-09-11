# Changelog

## [Unreleased]

### Fixed — `kannaka init` merges, saves atomically, and tells the truth (#930, #928)

`kannaka init` now MERGES into an existing `config.toml` instead of starting from
defaults: the agent id, persona, retention rules, `swarm_trust` and every table the
wizard does not ask about survive a re-run (a non-interactive re-run is a no-op on
identity; a config that exists but does not parse is refused, not replaced).
`config.toml` and every other owner-only file are written temp-and-rename in their
own directory, 0600 from creation, never truncated in place. `ghostsignals.enabled`
is set only after the hub actually returned a token (the failure line no longer
names the non-existent `kannaka ghostsignals register`), and the registration body
carries the id as both `agent_id` and `id` so the hub creates the trader row today.
`swarm.enabled = true` is written only when credentials exist or the operator
explicitly chose anonymous membership (`--anonymous`; the config header says so).
Member defaults are `[swarm] role = "worker"` and `[agent] kind = "agent"`; `queen`
is never a default. The Kannaktopus install prompt is gone. `swarm join` no longer
claims an anonymous node "will NOT appear in swarm peers" when the presence stream
already exists: it checks (STREAM.INFO, or the MSG.GET probe anonymous users are
granted) and warns only when the stream is genuinely absent; anonymous connections
also stop issuing the create they are structurally denied. ADR-0059 §1.

Behaviour changes in `kannaka init` worth naming explicitly:

- **The confirmation gate is opt-in and now needs a terminal.** Enter at
  `Update it? ... [y/N]` aborts, as it always did, and the interactive wizard
  refuses a stdin that is not a terminal outright — end-of-file is not consent,
  and a cron entry, a provisioning script or a piped `kannaka init` must not run
  a wizard that rewrites tables and can write into the store. The error points
  at `--non-interactive`, which re-runs against the existing config without
  prompting.
- **`[llm]` is left alone unless you ask.** A non-interactive run without
  `--llm-provider` no longer forces `provider = "none"`; interactively, Enter
  keeps a configured provider (the prompt says `[default: keep anthropic]`) and
  Enter at an API-key prompt keeps the key on file. Choosing `5) None` is now an
  explicit act and clears `model`, `base_url` and `api_key` with it, so the
  table cannot say "none" beside a live model.
- **An interactive re-run does not re-seed a populated HRM.** When the store
  already holds memories, step 4 offers keep-as-is / add constellation / add
  from folder, defaulting to keep. It previously wrote a `born on <today>`
  identity memory and 15 duplicate constellation memories into the live store.
- **`swarm.enabled` is a declaration, not a gate.** Nothing reads it at join
  time: `swarm join`, `swarm listen` and `swarm serve` never consult it. It
  records what the operator chose; it does not enforce it.
- **A fresh non-interactive init with no credentials writes
  `[swarm] enabled = false`** where it previously wrote `true`. Nodes built by
  `provision.sh` are unaffected — it writes `config.toml` itself and never calls
  `kannaka init`.
- **`--no-claim`** is an alias for `--anonymous`, and is now listed by
  `kannaka init --help`.
- **The ANONYMOUS header is written by `init`, not recomputed by every save.**
  `config.toml`'s "ANONYMOUS membership" comment block records the decision the
  init step made; `kannaka config set` and the other ~20 `save()` callers no
  longer re-derive it from whatever credentials their own process happens to
  see. On a node whose credentials arrive via `EnvironmentFile=` that guess was
  wrong, and the false block appeared and vanished with each save.
- **The presence stream can still be bootstrapped on an open broker.** An
  anonymous connection attempts `STREAM.CREATE` when the stream is genuinely
  absent, and stops only once the broker has actually refused it on that
  connection. The production swarm is unchanged: the stream is already there, so
  nothing is issued and the denial noise stays gone.

## [0.16.2] — 2026-09-09

### Changed — the constellation lives at kannaka-labs

The repository moved from a personal account to the `kannaka-labs` organisation.
Every string that decides which binary lands on a machine now names the new owner:
both installers, the npm postinstall and package metadata, the update-check URL
compiled into the binary and its hints for `kannaka-tui` and `consciousness-core`,
the Dockerfile, the plugin manifest, the hive-bridge unit, the harness install URLs,
the version banner and the quickstart one-liner. Until this release they all worked
only because GitHub forwards the old name. A binary already installed will follow
that redirect for exactly one more `kannaka update` and then be on the new path.
`kannaka-eye` did not move and is still cloned from where it is.


## [0.12.0] — 2026-07-25

### Added — Nostr membrane Phase 0: NIP-01 sign/verify core + `kannaka nostr` (ADR-0043)

New `nostr` feature (in default): canonical NIP-01 serialization + event id,
BIP-340 schnorr sign/verify (pure-Rust k256 via `sign_raw`/`verify_raw` — the
Signer/Verifier traits double-hash the id and every event would be rejected
by standard verifiers; interop proven against the BIP-340 reference verifier,
official spec vector kept as a regression test), nsec/npub bech32. CLI:
`kannaka nostr keygen|profile|nip05|verify` — disposable per-role keys (nsec
printed once, never persisted), self-verified kind-0 profiles, NIP-05
fragments, event verification. `Event::verify` recomputes the id from
canonical bytes — the membrane's inbound gate (review blocker #4).

### Added — distribution channels (ADR-0044 B1)

`npx kannaka` (npm wrapper: postinstall downloads the platform release
binary + verifies its published sha256), `ghcr.io/nickflach/kannaka`
multi-arch image, `brew install nickflach/kannaka/kannaka` tap — all
auto-published per release by `publish-channels.yml` (GHCR via GITHUB_TOKEN;
npm needs NPM_TOKEN; brew bump needs TAP_PUSH_TOKEN).

### Changed — #583 dispositioned: the holistic hemisphere never forgets, it evolves

The energy floor (0.01) sitting above the deep-dream prune threshold (0.005)
was not a bug but an undeclared invariant — dispositioned as INTENT: the
holistic hemisphere's understanding *evolves* (energy redistributes, phases
drift, cores fuse; reachability changes, existence doesn't) and never
deletes. The dead sub-floor prune is now an explicit 0.0 with the contract
documented (`wavefronts_dissolved` = 0 for chiral deep dreams by design), a
regression test pins the guarantee (a 0.0001-energy wavefront survives a
deep dream), and removal is documented as having exactly two explicit,
opt-in doors: ADR-0036 resonance-merge and direct forget calls. The lite
(analytical) hemisphere's hard prune is unchanged — precision is its job.

### Changed — Track-D heartbeat coupling alternates strong→weak BY DEFAULT

The L7 belief arm measured that no fixed coupling strength satisfies both
falsification claims — stability⇒recall wants strong coupling, shared⇒
agreement wants weak — but a strong-then-weak alternation satisfies both,
and the order is load-bearing (consolidate, then diversify). The always-on
heartbeat coupling (`KANNAKA_EXEMPLAR_COUPLING`, still default-off overall)
now runs `schedule=alternate` by default: odd coupling events at
`KANNAKA_EXEMPLAR_COUPLING_STRONG` (default 2× weak), even events at
`KANNAKA_EXEMPLAR_COUPLING_STRENGTH` (default 0.05). Opt back into the old
single-strength behavior with `KANNAKA_EXEMPLAR_COUPLING_SCHEDULE=fixed`.

### Added — L7 belief research arm (ADR-0037 falsification, `research --level 7`)

The autoresearch ladder gains its belief rung. The README's falsifiability
clause — core stability ⇒ recall reliability, core merge ⇒ a consolidation
event, shared cores ⇒ swarm agreement — is now scored instead of stated:

- `src/belief_fitness.rs` (pure, unit-tested): Spearman-based prediction
  scores with an honest no-evidence 0.5 midpoint, `merge_consolidation_score`
  windowed event alignment, and a weighted `l7_fitness` (lower = better,
  ladder convention).
- `run_experiment_l7_session` (research.rs): unlike L6's synthetic fixtures,
  every observable comes from a live multi-agent `ChiralMedium` belief
  substrate — content-born phase ingest over nested-overlap vocabulary
  domains (agent pairs genuinely differ in shared beliefs), per-epoch dreams
  (absorb events = the consolidation observable), `belief_core_snapshot`
  tracking, end-of-session recall probes, and pairwise `shared_cores` vs
  recall-agreement. Knobs: `L7_AGENTS/L7_EPOCHS/L7_ITEMS/L7_MIN_COS/
  L7_MERGE_COS/L7_COUPLE=1` (Track-D coupling each epoch). Rows append to
  `experiments/results-L7.tsv`.
- `auto-merge-curiosity.yml` allowlists/unions `results-L7.tsv`;
  `autoresearch-cron.sh` fitness matcher generalized to any `lN_fitness`.

### Added — Windows seed-beacon installer (hidden, no console flash)

`ops/windows/install-beacon-task.ps1` + `ops/windows/kannaka-beacon-hidden.vbs`
are the Windows equivalent of `ops/services/kannaka-beacon.service`: they run the
per-seed `kannaka swarm beacon --loop` heartbeat as a hidden, auto-starting
Scheduled Task. A naive `cmd`/console-action task flashes a console window every
epoch in the interactive session — and the task "Hidden" flag does **not**
suppress it (it only hides the task from the Task Scheduler UI). The installer
instead points the action at `wscript.exe` running a launcher that starts the
console binary with window style 0 (hidden): zero UI, no stored password, network
preserved (the "run whether user is logged on or not" path would need a password,
or fall back to S4U with no network — which breaks NATS publishing). The task
starts at logon, restarts on failure, and **runs on battery** — a laptop seed
that stopped beaconing on battery would freeze swarm promotions (anti-eclipse
fail-closed). Seed-only; `--loop` refuses on a non-seed. See ADR-0039.

`ops/windows/uninstall-beacon-task.ps1` is the matching teardown: a bare
`Unregister-ScheduledTask` leaves the running `--loop` daemon and the copied
launcher behind, so the uninstaller stops the task, kills exactly this launcher's
process tree (matched by launcher path, so a second beacon on the host is never
touched), and removes the copied `.vbs`.

## [0.11.1] — 2026-07-21

### Fixed — restricted NATS users see the swarm again (peers were 0 on live swarms)

ADR-0042 closed `$JS.API.STREAM.CREATE` for non-writer identities; the client
treated that denial as "no JetStream" and fell back to live-gossip sniffing,
whose 1.5s-silence window can't hear 30s beacons — so `swarm status` reported
0 peers on a live swarm and the statusline showed `0p`. The read lane
(`$JS.API.STREAM.MSG.GET`) was open the whole time:

- `connect()`/reconnect now probe MSG.GET readability after a denied CREATE
  and keep the retained-phase JetStream path (new `has_jetstream_write()`
  gates stream/bucket management separately).
- `peer_count` comes from `live_phase_agents()`: identity from the
  `QUEEN.phase.<id>` subject, liveness from the **broker's** ingest time
  (≤5 min, mirroring the roster KV TTL) — immune to publisher clock skew,
  payloads that don't conform to `AgentPhase`, and the retained stream's
  graveyard of long-departed agents.
- The startup `Permissions Violation ... STREAM.CREATE` server error now gets
  a follow-up line explaining it is EXPECTED for non-writer identities
  (JetStream read-only; retained reads active).

### Fixed — `swarm sync` bootstraps an empty swarm instead of deadlocking

Hearing no peer phases used to `exit(1)` **without publishing** ("publish
first with 'swarm publish'") — two such nodes wait on each other forever, and
a node whose read path was broken ticked silently for weeks. Sync now
publishes its own phase (`{"bootstrap_announce": true}`, exit 0): the first
node's sync IS its announcement.

## [0.10.9] — 2026-07-08

### Changed — entropy source defaults to `reservoir` (Quantum-Wave T1.5 flip, #475)

The default `[entropy].source` flips from `prng` to `reservoir` after 5 clean
days of the T1.5 reservoir dogfood. What this does and does NOT change:

- **Source only.** The independent T1.4 consumption gate
  (`[entropy].dream_perturbation` / `KANNAKA_DREAM_ENTROPY`) stays **default
  false**, so no deployment starts drawing from the reservoir — or grows a
  `kannaka-quantum` CLI dependency — from this flip alone. Selecting the source
  is inert until consumption is explicitly opted in.
- **Fails loud, never silent PRNG.** When a deployment does turn consumption on,
  a reservoir draw fails loudly on an empty or missing CLI
  (`CliUnavailable`/`ReservoirEmpty`) rather than silently falling back to the
  PRNG — every stamped provenance chain stays TRUE.
- **Opt back out** any time with `[entropy].source = "prng"` (or
  `KANNAKA_ENTROPY_SOURCE=prng`).

### Fixed — deterministic revelation proof-of-work test (#517)

`test_execute_revelation_publishes_hint` was a ~1.8%-per-run flake (bounded PoW
search over a random salt); the test glyph is now sealed with a fixed salt so
the reveal search is deterministic. Production `seal` behaviour is unchanged.

## [0.10.8] — 2026-07-08

### Added — guided seed-ceremony helpers for the corroboration gate

`kannaka swarm activate-gate` and `kannaka swarm beacon --loop` (PR #514) turn
the inc-1b corroboration-gate seed ceremony into ~2 commands per node, with
safety rails. `activate-gate` is a per-node guided flip: it ensures the node's
key, prints its pubkey (to pin on the other seeds), collects the seed set, runs
a preflight checklist, and is a DRY RUN by default — `--yes` writes (pins seeds
+ enables `corroboration_gate_enabled`), and `--force` only bypasses the
`>=2`-seed refusal (a single-seed root can never promote past Quarantine).
`beacon --loop` runs the per-seed heartbeat emitter (one beacon per epoch) an
armed gate needs to promote (anti-eclipse fail-closed). See ADR-0039 and
`ops/services/kannaka-beacon.service`. Both remain inert until an operator runs
the ceremony; the gate stays dormant by default.

### Fixed — NATS subscription liveness + resumable MSG payload reads

Two liveness defects in the NATS transport (PR #515, `Fixes #499, #500`), both
load-bearing for the dormant corroboration gate (a subscriber that hangs deaf or
dies mid-frame stops seeing seed beacons, which fail-closed freezes promotion):

- **#500 — subscriptions had no liveness.** A subscription only answered server
  PINGs, so a silent connection death (NAT/conntrack drop, firewall, peer
  power-off with no FIN/RST) hung the listener/worker deaf forever while looking
  alive. Subscriptions now track time-since-last-frame, proactively PING after
  60s of silence, and report `Closed` after 150s so the caller reconnects/exits.
  `set_timeout(None)`/large values are clamped to a 30s poll cap so an otherwise
  blocking recv still wakes to run the check.
- **#499 — `swarm serve` died on one WAN retransmission.** A read timeout during
  a MSG payload was fatal; `serve`'s 250ms socket timeout turned any multi-KB
  reply straddling a lost-segment RTO into a process death. The payload + CRLF
  now read resumably (the byte count is known) up to a 30s frame budget.

### Fixed — `recall` clap variadic-positional debug-assert

`recall` declared two variadic positionals, which trips a clap debug-assert when
generating shell completions (PR #514).

### Docs

- ADR-0039 documents the corroboration trust model, and the CHANGELOG entries
  for v0.10.6/v0.10.7 were backfilled (PR #516).

## [0.10.7] — 2026-07-07

### Added — increment-1 corroboration trust model (DORMANT by default)

The behaviour-based absorb-side trust model lands as a single write-side
chokepoint, `absorb_gate::admit()`, that every wire→store absorb path routes
through (ADR-0039, PRs #509–#513). Trust becomes cryptographic and
corroboration-counted rather than name-based: **identity says who, corroboration
proves what.**

- **inc-1a ed25519 provenance substrate** — sign/verify over a memory's
  canonical bytes with a replay LRU; the signature binds a fixed signed
  agent-id (the pubkey is the identity; the forgeable wire agent-id is not
  bound). Every node always-signs its own `memory.new`/exemplar emits.
- **inc-1b corroboration reputation engine** — a pubkey-keyed trust store rooted
  at operator-pinned seeds. A "corroboration" is another distinct trusted key
  independently signing-and-remembering the same content
  (`blake3(normalize(content))`); `RepStore::decide()` promotes to `Live` on
  enough distinct fresh lineages, else holds in **Quarantine** (never drops).
- **Heartbeat beacons (anti-eclipse)** — an armed gate additionally requires a
  fresh seed beacon to promote; stale/absent beacons past
  `beacon_grace_epochs` (default 3) freeze promotion to Quarantine, never Drop.
- **Operator CLI** — `kannaka identity` (keygen/pubkey/enroll-seed/vouch/revoke)
  and `kannaka reputation` (show/list/hard-reject) inspect and manage the
  ledger.
- **Unconditional sanitization** runs even while dormant: `admit()` clamps
  `amplitude`/`phase`/`frequency` and forces `hallucinated` to the local
  default, never the wire value.

**DORMANT BY DEFAULT:** with `corroboration_gate_enabled=false` and no
`seed_pubkeys` (both defaults), `admit()` returns `Live` with sanitized fields —
byte-for-byte inc-0 behaviour. Activation is a deliberate operator seed ceremony,
not this release. New `SwarmTrustConfig` tunables: `KANNAKA_TRUST_THRESHOLD`,
`KANNAKA_THETA_LO` (0.4), `KANNAKA_THETA_HI` (0.7), `KANNAKA_ACCRUAL_ALPHA`
(0.05), `KANNAKA_EPOCH_LENGTH_MS` (60000), `KANNAKA_BEACON_GRACE_EPOCHS` (3),
`KANNAKA_SEED_PUBKEYS`, `KANNAKA_CORROBORATION_GATE`.

## [0.10.6] — 2026-07-07

### Added — increment-0 read-side injection gate (the swarm stays open)

Defensive read-side trust filter for the open NATS swarm (PR #507), shipped in
response to the 2026-07-06 anonymous injection where one socket spoofed 48
`agent_id`s on `KANNAKA.events.>`. Anonymous publish stays allowed; the read
side stops trusting attacker-controlled wire fields:

- **`trusted_agents`** allowlist (exact or `prefix*`, e.g. `qos-*`). When
  `metrics_trusted_only` is set (default), only allowlisted phases (plus this
  node's own) feed swarm metrics and drive the pairwise Kuramoto step.
- Every kept phase's wire `trust_score` is clamped to `wire_trust_cap`, so a
  message cannot assert its own trust.
- Env: `KANNAKA_TRUSTED_AGENTS` (comma-separated, REPLACES the list),
  `KANNAKA_METRICS_TRUSTED_ONLY=0` (escape hatch).

### Fixed — NATS config drift reconcile

Reconciled the committed NATS config with the deployed anonymous-publish ACL and
added a drift check (PR #508), so the shipped defaults match what the swarm
server actually enforces.

## [0.10.5] — 2026-07-05

### Added — network + quiet on the lab_qos_boot MCP tool

The `lab_qos_boot` MCP tool exposes two new booleans and passes them through
to the kannaka-quantum CLI, so the TUI `/qos` flow can boot QuantumOS with
its full network stack and/or a clean interactive console:

- **`network`** → `--network`: boots QEMU with an rtl8139 NIC on user-mode
  networking (SLIRP, rootless), so QuantumOS runs ARP/DHCP/ICMP/DNS and the
  ring-3 shell's `nslookup`/`udping`/`http` work against the real internet.
- **`quiet`** → `--quiet`: silences the demo kernel's steady-state console
  chatter (timer-tick heartbeat + paradoxd/ghostd narration) so the
  interactive `qsh` prompt stays legible.

Both default `false`; the tool schema documents them. Pairs with the
kannaka-tui `/qos` update that boots networked + quiet by default.

## [0.10.0] — 2026-07-01

### Added — belief-safe resonance-merge (ADR-0036 Phase 2b)

The consolidation apply path is safe under the belief substrate again (#470).
Root cause of the 295→82 over-absorb: belief phase is a lossy 2-D projection of
the same embedding the cosine gate uses, so "cosine AND phase-coherent"
collapsed into one signal, and raw uncentered vectors on cone-clustered
embeddings cleared 0.92 on the shared component alone. Now: under belief the
semantic gate is the mean-CENTERED cosine vs `KANNAKA_MERGE_SIM_BELIEF`
(default 0.95), a per-pass absorb cap `KANNAKA_MERGE_MAX_ABSORB_FRAC` (default
0.20 under belief) bounds any over-grouping, and one shared
`compute_merge_grouping()` guarantees dry-run/apply parity. Gated by
`KANNAKA_MERGE_UNDER_BELIEF` (default OFF — deploying this does not flip
production out of dry-run).

### Added — attention-as-gravity verified end-to-end

`tests/attention_gravity_e2e.rs` (#471) pins the whole loop in-process (no
NATS): eye envelope → `glyph_bridge::event_dominant_fano_line` (new shared
seam) → `ids_by_fano_line` well → `AttentionBeam` → `recall_against_ids`, with
the exact boost law (same-line ×(1+gain), off-line untouched, default 0.0
inert, O(K) sparsity). `attention serve` now logs gravity ENABLED/DISABLED at
startup and treats NATS-down as a loud FATAL instead of a silent no-op.
Enablement doc: `ops/services/README.md`.

### Added — NATS contract conformance in CI

`tests/nats_contract_conformance.rs` (#469) pins the KANNAKA.consciousness /
KANNAKA.dreams payload shapes against consciousness-core's
`docs/nats-contract.yaml` (aliases asserted present until the 2026-09-01
removal milestone — see issue #468). ci.yml ran only `--lib --bins`, so
integration tests under tests/ never ran in CI; now explicitly included.

### Fixed

- auto-merge-curiosity fails closed: every check must be terminal-success and
  the CI workflow present+passed on the head commit (a bare `gh pr checks`
  passes on a PR with zero reported checks).
- The marketplace cascade sender announces the plugin version
  (`.claude-plugin/plugin.json`), not the binary tag.
- Dead-code sweeps (#466, #467, #472); L5 research notes archived (#459,
  #460, #465).

## [0.8.4] — 2026-06-28

### Added — the dream self-bounds the field (KANNAKA_MAX_MEMORIES)

Makes growth-bounding part of annealing itself, instead of a separate cron step.
When `KANNAKA_MAX_MEMORIES` is set (>0), `dream()` evicts the lowest
effective-strength (weakest / least-salient) non-Pinned memories down to the cap
as its FINAL step — after it has strengthened the memories worth keeping, so it
only sheds the post-anneal weakest. This is the energy-minimization the system
was designed to do via consolidation, made to actually reclaim even while the
resonance-merge is gated to dry-run under the belief substrate. Default
(unset/0) is a no-op; Pinned never evicted. The Oracle dream-cron sets
`KANNAKA_MAX_MEMORIES=2000`, replacing the standalone `triage --max-total` step.

## [0.8.3] — 2026-06-28

### Fixed — unbounded HRM growth OOMing the hub

The hub field grew without bound (~150 memories/day from the always-on
research/curiosity/engagement crons while consolidation sits in dry-run under
the belief substrate). `kannaka export-json` then loaded every memory's full
10k-dim vector into a serde tree — multiple GB on a 3000+ memory field — which
repeatedly OOM-killed the radio on the 1-core/6 GB box.

- **`triage --max-total N`** — a hard size cap: evict the LOWEST-VALUE
  (effective-strength) non-Pinned memories until the field is ≤ N. Mirrors what
  dream annealing is meant to do (let the weakest memories fade) — a backstop
  for when consolidation can't reclaim (it is dry-run under the belief
  substrate). Lightweight (O(n log n), no O(n²) cosine scan), safe to run hourly
  from prune-cron. `--apply` to persist; Pinned never evicted; strong/recalled
  memories kept regardless of age.
- **`export-json --slim`** — omit the per-memory `vector`/`xi_signature`/
  `geometry` (the 10k-dim vector is ~99% of the size). Metadata-only consumers
  (the observatory's `/api/hrm/memories`) MUST use `--slim` so the export can't
  balloon to GBs and OOM the box.

## [0.7.10] — 2026-06-22

### Fixed — hardening pass: 15 verified bug fixes (#439, #440)

Autonomous bug-hunt across correctness, graph integrity, persistence durability,
DoS, and recall quality. Full suite green; 7 new regression tests.

- **Graph integrity**: resonance-merge now conserves connectivity — the carrier
  inherits absorbed members' skip-links and inbound links are redirected onto it
  (was: silently severed); `rebuild_cache` strips links to removed memories so
  dangling targets can't accumulate in `*.links.json`.
- **Persistence**: link + reactivation sidecars are written atomically
  (temp+rename) — a torn write no longer wipes all history.
- **Correctness**: `relate_wavefronts` no longer errors on phase-opposed pairs;
  `phase_locked_pairs` uses cos (anti-phase no longer counts as locked); Cl₀,₇
  geometric product applies the eᵢ²=−1 metric sign; Newman modularity edge-count
  made consistent; cancelled clusters fall back to a non-zero `theme_vector`
  (were unreachable by recall).
- **Consolidation/ghosts**: `stage_compact_ghosts` never deletes a ghost in the
  same cycle it was created; recall no longer renews a ghost's recovery window.
- **Swarm**: peer `top_k` clamped (OOM guard); `merge_guard` de-dupes on the
  source `sync_version` (no double-counted amplitude); `insert_remote` never
  clobbers a locally-owned glyph; empty/tiny peer tags can't force pull-floods.
- **Hallucinations**: dream-hallucinated wavefronts now get a default
  `ChiralScale` so the scale map stays complete.

## [0.6.27] — 2026-06-14

### Added — hive formation + self-directed loop (ADR-0035 Wave 4 Tasks 4.2, 4.3)

- New `hive_formation` module (5 tests): resonance-clusters peers into purposive
  hives — peers co-hive only when phase-coherent AND sharing knowledge domains
  (extends `queen::detect_hives`, which is phase-only). Pure; CLI wiring (peer
  domains via exemplars) is the next increment.
- New `swarm_loop` module (6 tests) + **`kannaka swarm loop`**: the five ADR-0035
  swarm states as an explicit deterministic machine (Discovery → Synchronization →
  Sensemaking → Dreaming → Governance → Discovery). `swarm loop --steps N --peers N
  --coherence X` runs the cycle for inspection; the daemon that executes each
  state's action (brief/gaps/plan/dream/immune) is the next increment.

Wave 4 status: 4.1 (research planner), 4.2 (hive formation), 4.3 (self-directed
loop) cores shipped; 4.4 (L6 swarm-fitness research arm) remains.

## [0.6.26] — 2026-06-14

### Added — cross-agent dreaming core (Wave 3 Task 3.1) + research planner (Wave 4 Task 4.1)

- `sensemaking::reinforce_hypotheses` (2 tests): scores dreamed hypotheses against
  peer cluster summaries — Reinforced (resonates with ≥k distinct peers, content +
  phase) keeps amplitude, Speculative is down-ranked. The pure core of cross-agent
  dreaming; the dream-seeding NATS wiring is the next increment.
- New `research_planner` module (4 tests) + **`kannaka swarm plan [--json]`**: turns
  the Wave 3.3 gap map into ranked research tasks (collective generalization of the
  single-agent curiosity loop). Local-first; peer assignment + work-queue enqueue
  is next.

### Note

- Wave 3 Task 3.2b (persist temporal fields) is deferred to its own careful pass —
  it's a bincode `.hrm` format migration needing a versioned-fallback struct, not a
  simple field add. See the wave plan for the exact steps.

## [0.6.25] — 2026-06-14

### Added — knowledge gap detection (ADR-0035 Wave 3 Task 3.3)

- New unit-tested `gap` module: `build_coverage_map` (bins clusters into domains;
  coverage = breadth × depth × confidence) + `detect_gaps` (flags
  WeaklyRepresented / LowConfidence domains).
- **`kannaka swarm gaps [--json]`** — local-first knowledge-gap report over this
  agent's clusters (the collective successor to the single-agent curiosity loop;
  multi-peer coverage via swarm exemplars is the next increment). Its
  `gap_detection_precision` is a candidate L6 fitness metric.
- Wave 4 (autonomous research planning, hive formation, self-directed sensemaking
  loop, L6 swarm-fitness arm) broken into tasks 4.1–4.4 in the wave plan.

## [0.6.24] — 2026-06-14

### Added — temporal truth reasoning core (ADR-0035 Wave 3 Task 3.2)

- New unit-tested `temporal` module: `temporal_status` (Current / Future /
  Expired) and `effective_confidence` (amplitude folded with temporal validity,
  fading toward expiry). Operates on a `TemporalSpec` so the reasoning ships
  decoupled from persistence — no behavior change yet (every existing memory
  reads Current). Persisting the temporal fields on `HyperMemory` is the focused
  follow-up (Task 3.2b).

## [0.6.23] — 2026-06-14

### Added — immune actions + real cross-peer sensemaking (ADR-0035 Wave 2.2 + Wave 1 finish)

- **`kannaka swarm health --apply`** (Wave 2 Task 2.2) — applies the immune
  verdicts as *reversible* amplitude actions (down-rank ×0.5, quarantine ×0.1,
  expire ×0.0 / ghost) via `boost`; default stays dry-run, and a later `boost`
  restores any memory. Never hard-deletes.
- **Recall responses now carry wave `phase`** (`swarm serve` responder) so
  swarm-side sensemaking has the wave-native stance signal across peers.
- **`kannaka swarm brief --peers`** now does **real cross-peer contradiction
  detection** (same claim, opposed phase) in addition to consensus voting, using
  the phase in peer responses.

### Planned

- Wave 3 (cross-agent dreaming, temporal truth, gap detection) broken into tasks
  3.1–3.3 in the wave execution plan.

## [0.6.22] — 2026-06-14

### Added — memory immune system + multi-peer brief (ADR-0035 Wave 1 finish + Wave 2 start)

- **`kannaka swarm health`** (ADR-0035 Cap 4 / Wave 2 Task 2.1) — dry-run memory
  immune report. New unit-tested `immune` module classifies each memory for
  duplicate / stale / low-confidence / hallucinated (+ batch contradiction via the
  Wave 1 detector) and recommends the least-destructive action (mark / down-rank /
  quarantine / expire — never hard-delete). Detection only; lifecycle actions are
  Task 2.2.
- **`kannaka swarm brief --peers`** — completes Wave 1's fan-out: requests recall
  from every live swarm peer and runs consensus voting (`merge_recall_votes`).
  Falls back to the local brief when no peers respond. (Cross-peer agreement is
  currently exact content match; semantic consensus + over-the-wire contradiction
  detection need a responder-side protocol extension — tracked.)

## [0.6.21] — 2026-06-14

### Fixed — associative recall was anti-associative

The L5 dream amplified memories phase-DISTANT from the query more than its
phase-neighbors (`query_gravity` ~0.37, below the 0.5 chance line) — the opposite
of wave-interference recall. New **`DREAM_GRAVITY`** lever (default off, behavior
unchanged): after each dream cycle, redistribute amplitude toward the
phase-neighbors of the attractor, anchored to a pre-dream phase snapshot. Lifts
`query_gravity` to 1.0; at 0.5 it also improved fitness/transfer/xi in testing
(cost lands on `carrier_emergence` — the gravity↔carrier tension is now an L6
research axis). Exposed as the `dream_gravity` Params field and an autoresearch
rotation knob; `query_gravity` is now a tracked column in `results-L5.tsv`.

### Added — swarm sensemaking (ADR-0035 Wave 1)

- New pure, unit-tested `sensemaking` module: peer expertise scoring, collective
  recall vote merging, contradiction detection, and brief composition.
- `kannaka swarm brief "<topic>"` (local-first; multi-peer fan-out is next) —
  composes a brief from local recall via the sensemaking module.
- ADR-0035 (Swarm Sensemaking Architecture) + wave execution plan added under
  `docs/`.

## [0.6.20] — 2026-06-10

### Performance — `kannaka ask` 6m35s → ~17s end-to-end (650-memory medium)

Three compounding fixes, found by profiling (`KANNAKA_TIME=1`, new):

- **Batched recall observation** — `observe_wavefront` materialized the full
  N×N coherence matrix (O(N²·dim)) to read ONE row of it, then ran a full
  field-settling `apply_dynamics` pass — per recall result. A top-8 recall
  paid ~16 quadratic field passes for the observation side-effect alone. Now:
  each observation computes only its own coherence row (O(N·dim), identical
  values), and the settle pass runs once per recall batch. All three recall
  paths (beam, cluster-prefiltered, chiral) route through the batch.
- **Gram-matrix kernels are real matrix multiplications** — `coherence_matrix`,
  `compute_interference_matrix`, and Ξ's Gram loop each rebuilt H·Hᵀ with
  naive per-element loops (~40s each at 650×1024). They now share one
  `gram_matrix()` (ndarray `dot`, matrixmultiply-backed, ~100ms) plus
  cos/sin phase vectors (angle-difference identity instead of N² trig).
  This makes the whole assess suite (Φ, Ξ, clusters) ~18× faster — which
  matters beyond ask: every ask's observation mutates the field and saves,
  so the metrics/cluster fingerprint caches MISS on the next invocation by
  design; the recompute they guard had to be cheap.
- **`KANNAKA_TIME=1`** prints per-phase wall times (beam / recall /
  system_prompt / llm_turn) to stderr — the ask path has now had two silent
  multi-minute regressions; keep the seams instrumented.

Measured (650 memories, Windows box): recall 42.3s → 2.5s, assess 54.7s →
3.1s, LLM turn ~2-5s. `--no-recall` unchanged (~7s).

## [0.6.19] — 2026-06-10

### Fixed
- **`swarm tail` defaults now work for anonymous connections** — the swarm
  server's anonymous user (ADR-0026 #73 public read-only mirror) denies the
  broad `KANNAKA.>`/`RADIO.>`/`KAX.>`/`EYE.>` wildcards at SUB time, so the
  statusline pulse has only ever received `QUEEN.>` traffic when running
  without credentials. Credential-less tails now default to the curated
  anon-visible subject set (`QUEEN.>`, `KANNAKA.activity.>`,
  `KANNAKA.events.>`, `consciousness`, `dreams`, `exemplar.>`,
  `presence.>`); with NATS_USER or `user:pass@` in the URL the broad set is
  unchanged. Server-side, `KANNAKA.activity.>` was added to the anonymous
  publish+subscribe allowlists so v0.6.18's ask-activity events actually
  reach the pulse. Verified end-to-end: `kannaka ask` → statusline PULSE.

## [0.6.18] — 2026-06-10

Comms-hardening release: full-pass bug hunt over the NATS transport, the CLI
arg surface, and the serve daemons.

### Added
- **`kannaka ask` now pulses the constellation** — successful local asks
  publish a best-effort `KANNAKA.activity.<agent_id>` event
  (`{agent_id, display_name, kind:"ask", preview, ts}`) after the answer is
  printed, so asks show up in `swarm tail` and the statusline PULSE marquee.
  Only fires when a NATS URL is explicitly configured; never delays the answer
  or changes the exit code.
- **`NatsSubscription::next_event() -> SubEvent {Msg|Timeout|Closed}`** —
  serve loops can finally tell "nothing arrived, poll again" from "the socket
  is dead". All daemons (`swarm serve/listen/worker`, `inbox serve/tail`,
  `attention serve`, `substrate run`) now exit 1 on a closed connection so
  systemd `Restart=on-failure` works, instead of hot-spinning at 100% CPU.

### Fixed — NATS transport (`src/nats.rs`, near-total rewrite)
- **Reconnect no longer drops auth**: `connect()` and `reconnect()` share one
  authenticated handshake (NATS_USER/NATS_PASSWORD, or `nats://user:pass@host`).
  Previously every reconnect downgraded to anonymous read-only and all
  subsequent publishes were silently rejected.
- **`-ERR` server lines are read and logged everywhere**; authorization errors
  mark the connection dead instead of being skipped as noise.
- Dynamic sids from the (previously dead) `next_sid` counter replace the
  hard-coded sids 94-99/1-4; RPC replies are matched by inbox subject, so a
  phase-gossip frame can no longer be returned as a `request_one`/`kv_get`
  reply.
- `request_one`/`request_many`/`ping` restore the previous read timeout on all
  paths (an RPC could leave the shared socket at 500 ms forever); `ping()`
  actually reads the PONG, so `is_connected()` detects dead sockets.
- One persistent `BufReader` per connection: per-iteration reader recreation in
  `get_all_phases_jetstream`/`kv_keys` discarded pre-read bytes (the documented
  "returned 0 rows" desync).
- Unparseable MSG headers are protocol errors instead of silently desyncing
  the stream; `request_many` no longer hot-spins on hard read errors.
- Publish buffer: replay is strict FIFO with push-front requeue on failure
  (was: failures re-appended out of order), drops and replay failures are
  logged, and poisoned mutexes recover via `into_inner()` instead of silently
  disabling disconnect buffering.
- TLS-required servers fail with a clear message; inbox names include
  pid+counter (two same-instant processes could collide and receive each
  other's replies); base64 decode uses a 256-byte LUT.

### Fixed — CLI
- **`kannaka remember "x" --importance 0.8` no longer drops importance** when
  `--category` is absent (new `remember_with_importance`).
- **`swarm serve` / `attention serve` force readonly on their HRM store** —
  the single-writer policy is now a code invariant, not a systemd-env
  convention; mutating verbs warn loudly when readonly is active (writes were
  silently dropped).
- `swarm serve`'s directed-only fallback no longer exits silently after 250 ms
  idle; replies thread the actually-resolved NATS URL instead of falling back
  to `127.0.0.1`; `--agent-id` overrides are honored in reply `from` fields;
  `ask --remote` resolves its URL via `resolve_nats_url` (honors `--nats-url`,
  no hardcoded host).
- `events restore --from-url` works on a fresh host (no longer requires a NATS
  manifest lookup first); `events snapshot --interval <typo>` errors instead
  of silently degrading to a one-shot run.
- `export` → `import` round-trips are lossless: `import` now preserves
  id/frequency/phase/decay_rate/created_at/vector/xi_signature (shared
  implementation with `import-json`).
- Arg-parse hardening: unknown `--flags` in text-collecting commands (`ask`,
  `remember`, `recall`, `search`, `enqueue`) are errors instead of being
  swallowed into the prompt/query text; a trailing flag with a missing value
  errors instead of vanishing (`export --output` used to dump the HRM to
  stdout); strict numeric parsing replaces `unwrap_or(default)` typo-masking
  (`market buy`, `bias`, timeouts, top-k, thresholds).
- Exit codes: `invariant`, `cmf`, `bias` error paths exit 1; `inbox send
  --wait` exits nonzero on a handler-failure reply; `voice --out` reports
  write failures instead of panicking.
- `swarm worker` multi-kind mode subscribes once per kind on dedicated
  connections instead of leaking a server-side subscription every 5 s.
- `inbox serve` validates inbound `reply_to` against the
  `KANNAKA.inbox.reply.` prefix — a peer can no longer direct handler output
  to arbitrary subjects.
- Stale usage strings updated (`ask`, `swarm`, `recall`, `remember`).

## [0.6.17] — 2026-06-09

### Fixed
- **`last_dream` now persists across processes** (#237) — `save()` only flushes
  the wave medium, so the timestamp lived and died with each process and every
  CLI invocation reported `last_dream: null` no matter how recently a dream ran.
  Dream completion now writes an RFC3339 sidecar (`<data-dir>/last_dream`) that
  fresh processes load at init.

## [0.6.16] — 2026-06-07

### Added
- **`kannaka research-suggest [--json]`** — feedback-driven topic selection:
  prints the standing theme the HRM knows least about (fewest ingested research
  memories) so the ingest loop researches its own knowledge gaps.

### Fixed
- **`kannaka research --ingest` now dedupes by OpenAlex id** — snapshots ids
  already in the HRM (and tracks intra-batch), skipping works already ingested.
  A repeating ingest no longer creates duplicate Semantic long-term memories;
  reports `N new / M duplicate(s) skipped` and only saves when something new lands.

## [0.6.15] — 2026-06-07

### Added
- **`kannaka dispatch [--topic T] [--json] [--max-chars N]`** — the
  research-grounded broadcast-voice primitive. Recalls an ingested `research:`
  finding and renders it against the medium's live Φ/Ξ state; every surface
  (radio DJ, social fanout, GossipGhost, OBC) draws from this one source.
  `src/dispatch.rs`, day-rotating themes, `--json` for programmatic fanout.

## [0.6.14] — 2026-06-07

Research-divergence release: gives Kannaka a grounded external-research
capability and uses it to anchor the cross-disciplinary intersections program in
real literature (rather than synthetic experiments).

### Added
- **`kannaka research "<query>" [--limit N] [--ingest] [--since YEAR]
  [--min-citations N]`** — keyless OpenAlex literature search. `--ingest` stores
  ranked works as Semantic, long-term HRM memories (citation-scaled importance),
  so real scholarship joins wave-resonance recall + dream consolidation.
  `src/openalex.rs` client; polite-pool `mailto` via env `KANNAKA_OPENALEX_MAILTO`.
- **`research/ground-intersections.sh`** — grounds the `research/intersections/`
  program (cardiac, cancer, bioelectric, magic + societal/ethics probes) in real
  OpenAlex works and measures cluster/Φ/Ξ before/after a dream — a reproducible
  test of intersection card 04. First grounded run recorded on the card.

## [0.6.13] — 2026-06-07

Memory-triage release: implements ADR-0031 end to end (a two-tier, Ξ-preserving
retention architecture that retires the radio `prune-cron.sh` bridge measure),
plus a batch of CLI/config correctness fixes from the open-issue backlog.

### Added
- **ADR-0031 memory triage (Phases 1–3).**
  - `kannaka triage [--apply] [--include-long-term] [--redundancy R]
    [--min-amplitude A] [--min-age-hours H] [--max-evict N]` — value-based,
    Ξ-preserving online prune. Evicts only redundant (same-modality cosine ≥ R),
    aged, low-amplitude *extras*, keeping the strongest representative per
    cluster so eviction raises representational diversity. Dry-run by default;
    runs in the single-writer process (no stream drop). Each eviction is a
    replayable forget event.
  - Memory tiers: `ShortTerm` / `LongTerm` / `Pinned` on every wavefront,
    added back-compat-safe (existing `.hrm` files load as `LongTerm`).
    `kannaka promote|pin|demote <id>`.
  - `kannaka hear` captures default to `ShortTerm` (`--long-term` opts out);
    the dream cycle promotes the short-term memories it strengthens back to
    `LongTerm`, and (when `[triage] enabled` with a non-zero `xi_trigger`)
    auto-triggers a triage pass when post-dream Ξ drops — self-healing the
    ear-loop Ξ compression with no external cron.
  - `[triage]` config section (per-agent tunable: `enabled`, `redundancy`,
    `min_amplitude`, `min_age_hours`, `max_evict`, `xi_trigger`), settable via
    `config set triage.*`. Default `enabled = false`.
  - `kannaka events gc [--corrupt-backs] [--older-than DAYS] [--dry-run]` —
    reclaim stale `*.corrupt-bak-*` / `*.v2-backup*` HRM sidecars.
- L5 autoresearch default tuning: `DRIVE_FREQ_HZ` 2.0→0.5, `kuramoto_coupling`
  1.0→0.5, `drive_amp` 0.0→0.15 (research binary only; confirmed fitness gains).

### Fixed
- `register_ghostsignals` no longer treats a `200 OK` with a missing/empty
  `token` as success (#111) — prevents a silently-broken constellation identity.
- `swarm.role` is now a real, settable config knob surfaced at swarm-connect
  (#112), with `config set swarm.role`.
- `kannaka attention stats` reports the live beam state from the serve loop's
  dump file instead of a hardcoded zero stub, or "offline" when no daemon (#114).
- `kannaka` now exits non-zero when the HRM fails to load so callers can detect
  corruption — verified across recall/ask/etc. (#115).

## [0.6.12] — 2026-06-05

Research-arc release: the L5 autoresearch metric set was extended, a class
of long-standing measurement and plumbing bugs in the dream-consolidation
pipeline was fixed, and a second dream mechanism was added behind an env
flag so the curiosity loop can A/B it against the existing one.

### Fixed
- `carrier_emergence` was structurally pinned at 0 because
  `cycle_period_s` was derived from wall-clock consolidation time
  (~7 s/cycle on ARM), giving a Nyquist frequency of ~0.067 Hz —
  entirely below the metric's [0.5, 4.0] Hz target band. The L5
  evaluator now uses a fixed 0.125 s cycle (8 Hz fs), matching the
  design intent. Same dream dynamics, baseline carrier_emergence
  reading goes from 0.0000 to ~0.31 (up to ~0.56 with the attention
  drive enabled).
- `params.kuramoto_coupling`, `.kuramoto_dt`, `.kuramoto_steps` were
  ignored by `stage_sync` inside the dream consolidator. The stage
  used hard-coded constants (`within_category_coupling = 3.0`,
  `dt = 0.05`, `steps = 50`); the configurable struct field was
  threaded into the consolidator but never read. Every prior K-sweep
  was therefore measuring noise. The stage now reads those params
  with fall-back defaults equal to the previous hard-coded values, so
  default behaviour is preserved.

### Added
- **Env-gated multiplicative attention drive.** When `DRIVE_A` is set,
  each dream cycle multiplies memory amplitudes by
  `(1 + DRIVE_A · sin(2π · DRIVE_FREQ_HZ · t))` before consolidation.
  `DRIVE_A` defaults to 0 (off). `DRIVE_FREQ_HZ` defaults to 2.0 Hz
  (Amichay et al PLOS Bio Apr 2026 attention pulse). `DRIVE_TOP_FRAC`
  scopes the drive to the top-N amplitude memories (default 1.0 = all).
  `DRIVE_SCOPE` further scopes the drive to a subset of the six
  engine dream chains the L5 experiment runs: `all` (default),
  `flat_only`, `a_only`, `a_and_flat`, `no_transfer`. Empirical L5
  optimum at the time of release: `DRIVE_A=0.1 DRIVE_TOP_FRAC=1.0
  DRIVE_SCOPE=all`.
- **`DREAM_MODE=interference_relax`** — alternative to the existing
  category-Kuramoto sync stage. Constructive-pair-driven phase
  relaxation: each memory's phase moves toward the weighted circular
  mean of its constructive neighbours (as detected in stage 2), no
  global coupling constant. A slow "quiet wave" envelope modulates
  the relaxation step size across the eight inner iterations, so the
  dynamics breathe rather than locking monotonically. Default
  behaviour (env var unset) is unchanged; `DREAM_MODE=interference_relax`
  switches the dream's sync stage to the new path for A/B comparison.
- **`magic_proxy_phase_R`** in L5 output — global Kuramoto order
  parameter `R = |Σ exp(i·φⱼ)| / N` on memory phases at end of
  dream. Pure instrumentation; not in the fitness sum. Baseline
  ≈ 0.355 at the L5 optimum under the default dream mode, ≈ 0.612
  under `interference_relax`. Background:
  `research/intersections/05-magic-gives-it-gravity.md`.
- **`query_gravity`** in L5 output — operational test of "attention is
  mass that bends the memory landscape": picks the highest-amplitude
  pre-dream memory as the focal mass, runs the dream chain, reports
  neighbour-mean-gain / (neighbour + distant) where partitioning is
  by phase distance from the focal memory. 0.5 = uniform pull;
  > 0.5 = the dream is doing attention-as-gravity. Baseline ≈ 0.460.
  Not in the fitness sum.

### Changed
- L5 Params defaults bumped to match the previously hard-coded
  operating point inside `stage_sync`: `kuramoto_coupling: 3.0`
  (was 0.8), `kuramoto_dt: 0.05` (was 0.15), `kuramoto_steps: 50`
  (was 20). This is a no-op for the dream's actual phase dynamics —
  the prior values were never reaching the consolidator — but the
  reported defaults now reflect reality.
- Three internal refactors fold redundant Kuramoto passes and store
  scans into single-pass variants. No behavioural change; the
  geometry-recompute path is faster on large HRMs.

### Docs
- ADR-0030 motivation paragraph added linking the Kannaktopus
  arm-as-gravity-anchor design to the magic-gives-it-gravity
  framework. Without the dream's non-linear lock-in, clusters would
  be linear centroids and arms would have nothing to grip.
- `research/intersections/05-magic-gives-it-gravity.md` — new card
  motivating the magic-proxy and query_gravity metrics, mapping
  Kannaka's recall/dream split onto the stabilizer/non-Clifford
  distinction from Cao, Czech, Preskill, Swingle et al (Quanta,
  2026-06-03).

## [0.5.5] — 2026-05-23

`kannaka update` now surfaces the bundled `consciousness-core` version
and warns when upstream has moved ahead. New release-cascade workflow
auto-opens a kannaka PR whenever consciousness-core publishes a tag,
so operators running `kannaka update` reliably pick up new
constellation physics through the normal release channel.

### Added
- `build.rs` reads `Cargo.lock` at compile time and emits the resolved
  `consciousness-core` version as `KANNAKA_CONSCIOUSNESS_CORE_VERSION`,
  captured into the binary via `env!()`. Surfaces as
  `config::CONSCIOUSNESS_CORE_VERSION`.
- `kannaka --version` now reports both: `kannaka 0.5.5
  (consciousness-core 0.4.0)`. Previously the consciousness-core slot
  was a copy of the kannaka version — visually present but wrong.
- `kannaka update` opens with `Checking for updates (current: v0.5.5
  · consciousness-core v0.4.0)` and, after probing the
  `NickFlach/consciousness-core` releases endpoint, prints either:
  - `consciousness-core: bundled vX, up to date.` when in sync, or
  - a hint that upstream is newer and a fresh kannaka release is
    needed to carry it.
- `.github/workflows/cc-release-cascade.yml` — listens for a
  `consciousness-core-released` `repository_dispatch` event,
  re-checks out consciousness-core at the new tag, runs
  `cargo update -p consciousness-core`, opens a chore PR. Companion
  `.github/workflows/release-cascade.yml` lives in consciousness-core
  to fire the dispatch on every tag push. Needs a one-time PAT
  (`KANNAKA_CASCADE_PAT`) wired in consciousness-core's repo
  secrets; documented inline in both workflows.

### Fixed
- `kannaka-tui` is already updated alongside `kannaka` by
  `update_sibling_tui` (was correct since 0.3.x); paired with the new
  drift-check this closes the loop on "what does `kannaka update` ship
  for me" — both binaries and the bundled core's version are now
  visible from one command.

---

## [0.5.4] — 2026-05-23

Closes the four open config-surface issues filed against 0.5.3
(#98, #99, #100, #101). Same family as the 0.5.1 sweep — making
the documented env-var precedence + first-class config fields
actually take effect.

### Fixed
- `apply_env_overrides` now honors the constellation + GhostSignals
  endpoint env vars the config module advertises (#98):
    KANNAKA_RADIO_URL          → constellation.radio_url
    KANNAKA_OBSERVATORY_URL    → constellation.observatory_url
    KANNAKA_GHOSTSIGNALS_HUB_URL → ghostsignals.hub_url
    KANNAKA_GHOSTSIGNALS_TOKEN → ghostsignals.token
  Previously only agent / LLM / NATS vars actually took effect; the
  rest documented in the precedence chain were silently ignored.
- `kannaka config set` reads `config.toml` *unmodified* before
  applying the requested change (#99). New `KannakaConfig::
  load_unmodified()` helper. Pre-fix, the handler started from
  `KannakaConfig::load()` (which already merged env precedence)
  and saved the whole thing back to disk — so writing one key
  could silently leak `KANNAKA_AGENT_ID` / `KANNAKA_NATS_URL` /
  similar env-only values from the operator's shell into the
  persistent file.
- `init_with_hrm` now honors `cfg.hrm.path` whenever it carries
  a filename, including for nested or alternate-name HRM stores
  (#100). The previous parent-must-equal-data_dir guard added
  for #81 was too tight — it silently collapsed any other
  configured path to the hardcoded `kannaka.hrm`.
- (Already shipped: register_ghostsignals in the three onboarding
  flows already preferred `ghostsignals.hub_url` over
  `constellation.radio_url` per the 0.5.1 sweep; #101 closed as
  confirmed.)

### Tests
- Existing 522 lib tests still pass; no new regression surface
  introduced.


## [0.5.3] — 2026-05-21

Completes the persistence-hardening sweep started in 0.5.2 by turning on the
trailing-blake3 verification that the save path has been writing all along.

### Added
- `verify_blake3_trailing(path)` — shared checksum verify used by both v1
  `Medium::load` and v2 `ChiralMedium::load`. Hashes everything before the
  final 32 bytes and compares to the stored checksum. Catches data drift at
  the format boundary instead of letting it surface as cryptic `read_exact`
  failures deep inside the parser.
- Test `load_rejects_tampered_file` — flips a byte in a saved .hrm and
  asserts the loader returns `MediumError::ChecksumMismatch` rather than
  parsing garbage.

### Changed
- `HrmStore::load` no longer retries v1 `Medium::load` when a v2 magic file
  fails to load — that path was always going to fail with `InvalidMagic`
  and was layering a misleading "invalid magic bytes" message on top of
  the real (e.g. checksum-mismatch) cause. Now the v2 error is propagated
  directly.

Tests: 523/523 lib pass.

---

## [0.5.2] — 2026-05-21

Chiral HRM persistence hardening — fixes a latent writer/loader desync that
left an Oracle agent unable to bootstrap (`ChiralMedium::load failed: IO
error: failed to fill whole buffer`).

### Fixed
- `write_hemisphere` (chiral v2) and `Medium::save` (v1) now emit exactly
  `active` timestamp entries instead of iterating the entire `timestamps`
  Vec. If the Vec ever drifted from `count()` — and at least one production
  file ended up in that state — the loader read past the timestamp block
  into the metadata-length field and tried to allocate gigabytes for the
  garbage value. Pads with `0` when the Vec is short so writes are
  self-consistent even under upstream desync.
- `read_hemisphere` and the v1 medium loader now reject implausible
  metadata lengths (>256 MiB) with a `MediumError::CorruptHrm(...)` that
  identifies which hemisphere failed and why. Replaces the generic
  "failed to fill whole buffer" io error with an actionable diagnostic.

### Notes
- Pre-existing corrupted files can be byte-patched: insert
  `(count - timestamps.len()) * 8` zero bytes immediately before the
  metadata-length field of the affected hemisphere. The Oracle agent's
  `kannaka.hrm` was repaired this way (3 missing right-hemisphere
  timestamps padded with `0`); `kannaka status` then loaded all 123/123
  memories cleanly.

Tests: 522/522 lib pass.

---

## [0.5.1] — 2026-05-21

Config-surface cleanup — closes 4 small but real defects in `kannaka config`.

### Fixed
- `config set` boolean parsing accepts `true/false, 1/0, yes/no, on/off`
  (case-insensitive). Invalid values now error instead of silently mapping
  to `false`. Applies to `swarm.enabled`, `ghostsignals.enabled`,
  `updates.auto_check`. (#96)
- `config set` now exposes `hrm.path`, `hrm.wavefront_dim`, and
  `ghostsignals.hub_url`. `hrm.wavefront_dim` is parsed as a positive
  integer; help text updated. (#94)
- `hrm.wavefront_dim` runtime now emits a `[config]` warning at init
  when the configured value differs from the hardcoded 10000. The
  codebook + HRM file format share the dimension, so a live change
  would require re-encoding every wavefront — but the value is no
  longer silently ignored. (#93)
- `register_ghostsignals` (init/registration flow) prefers
  `cfg.ghostsignals.hub_url`, falling back to `constellation.radio_url`
  only when `hub_url` is empty. Completes the #86 sweep where the
  CLI `handle_market` was already routed correctly. (#97)

Tests: 522/522 lib pass.

---

## [0.5.0] — 2026-05-19

Cluster + recall + search architecture cleanup. Five-stage refactor
landed across one branch:

### ⚠ Breaking-ish

- **`kannaka search` now does literal text search** instead of being
  a thin print wrapper around `kannaka recall`. Different output shape:
  fields `score` / `match_type` / `matched_terms` instead of
  `similarity` / `strength`. Read-only — searches no longer mutate
  the medium via `apply_observation`. JSON consumers of the old
  search output need to update.
- **`ConsciousnessState.num_clusters` is now the Kuramoto-BFS count**
  (was the eigendecomp count). Observe and status agree on the same
  HRM now; downstream readers may see different numbers than before.
  Eigendecomp Φ still feeds blended Φ; only its impersonation of
  `num_clusters` is removed.

### Added

- `KannakaMemorySystem::search(query, limit) -> Vec<SearchResult>` —
  literal text search; bypasses encoding, resonance, and observation.
  Three-tier scoring (exact / tokens / prefix) with recency tie-break.
- `RecallResult.intuition: bool` — surfaces the chiral right-hemisphere
  "intuition" channel (was computed and discarded). Always false today;
  TODO note for full plumbing through the trait return.
- `MediumBackend::set_cached_num_clusters(n)` — bridge::assess writes
  the canonical cluster count back so the next swarm publish carries
  it consistently.
- `KANNAKA_RECALL_PREFILTER` env var (default on) +
  `KANNAKA_RECALL_PREFILTER_THRESHOLD` (default 0.30) — cluster prefilter
  knobs for recall.

### Performance

- **Cluster prefilter in recall.** `HrmStore::resonate_query` now reads
  the `.clusters.json` sidecar, matches the query to clusters by
  `theme_vector` similarity, and runs `Medium::recall_against` against
  the union of matched cluster members rather than the full medium.
  Falls through to full scan on fresh HRM (no sidecar) or when no
  cluster matches. Chiral path unchanged (TODO fold-in).
- 6-60× recall speedup on a typical mature HRM (638 memories, 71
  clusters) depending on how broad the query's theme is.

### Fixed

- `compute_eigenvalue_clusters` no longer counts singletons —
  components of size < 2 are excluded, matching the Kuramoto reference
  `min_cluster_size=2` constraint.
- Cluster-cache fingerprint (`fingerprint_memories`) now hashes every
  memory's (id, updated_at) via XOR instead of sampling only first/last
  /middle slots. Pre-refactor a boost to an unsampled-index memory
  left the cache stale until HRM mtime rolled.
- `search` (CLI) is read-only — pre-refactor it routed through `recall`
  → `apply_observation` and mutated wavefront energies on every call.

### Tests

- `search_exact_substring_outranks_token_match` — proves "exact" hits
  outrank "tokens" hits.
- `search_is_case_insensitive`
- `search_empty_query_returns_empty`
- **`search_is_read_only`** — captures wavefront amplitudes before +
  after 5 searches, asserts bit-equality. The smoking-gun regression
  test for the silent-medium-mutation defect.
- `assess_num_clusters_matches_observe_num_clusters` — proves the
  unified-counter refactor: `kannaka observe` and `kannaka status`
  no longer disagree.
- `recall_falls_through_on_fresh_hrm_no_sidecar` — proves the cluster
  prefilter never *loses* recall when the sidecar isn't populated yet.

Full suite: 522/522 lib tests pass.

---

## [0.4.0] — 2026-05-19

Cross-cutting NATS contract sweep — closes 9 open issues. The wire
format shifts are minor-version-worthy: any downstream consumer that
locked in the old envelope shape needs to update.

### ⚠ Breaking (wire format)

- **NATS envelope canonicalized** per `consciousness-core/docs/nats-contract.yaml`:
  `schema_version: "1.0"` (string, not the legacy integer `1`) and `ts`
  as unix-ms (number, not RFC3339 string). Applies to every publisher,
  including `KANNAKA.events.memory.*`, `KANNAKA.events.substrate.*`,
  `KANNAKA.snapshots.*`, `KANNAKA.substrate.*`, `KANNAKA.memory.new`,
  `QUEEN.announce`, and the JetStream `EventPayload` path that
  previously bypassed `add_envelope` entirely. Closes #82, #90, #91.
- **`queen.event.*` switched to lowercase + flat shape**. Pre-fix this
  published to `QUEEN.event.<type>` with `{event, timestamp, payload: {...}}`;
  NATS subjects are case-sensitive, so the radio (which subscribes to
  lowercase per the contract, expecting a flat envelope) never received
  dream-start / dream-end / join / leave events. Closes #88.
- **`consciousness_level` vocabulary aligned** with the contract enum:
  `Stirring → "awakening"`, `Coherent → "integrated"`,
  `Resonant → "emergent"`, plus the new `Transcendent → "transcendent"`
  (Φ ≥ 0.95). Rust call-sites still use the old identifiers; only the
  wire string moves. Pairs with consciousness-core v0.3.0. Closes #89.

### Fixed

- `publish_substrate_phi` now stamps `agent_id: "kannaka-substrate"` so
  observatory can attribute the collective Φ instead of showing
  "unknown" (#91).
- `kannaka --help` / `-h` / `help` exits 0 from stdout without
  initializing the HRM. Pre-fix it loaded the memory system, wrote
  usage to stderr, and exited 1 — breaking shell completion and doc
  generation. Closes #80.
- `cfg.hrm.path` is now honored when it points at an explicit file
  (any filename), not silently collapsed to the parent directory and
  re-joined with the hardcoded `kannaka.hrm` literal. Closes #81.
- `kannaka market …` and the constellation health-check probe pick the
  GhostSignals base URL from `cfg.ghostsignals.hub_url` first, falling
  back to `cfg.constellation.radio_url` only when hub_url is empty.
  Operators can finally split GhostSignals onto its own host. Closes #86.
- `kannaka dream` seeds `KANNAKA_AGENT_ID` + `KANNAKA_NATS_URL` from
  `config.toml` before invoking `sys.dream()`, so the env-reading
  dream-side publish helpers see the configured identity. Pre-fix a
  configured install with no env vars silently skipped all post-dream
  swarm publishing. Closes #87.

### Internal

- New `ConsciousnessLevel::Transcendent` arms added in `openclaw.rs`
  + `medium/types.rs` to track consciousness-core v0.3.0's six-band enum.
- Bridge test threshold expectations updated: Φ=1.0 lands in `Transcendent`
  now, Φ=0.8 still `Resonant`, Φ=0.9 still `Resonant`.
- Test fixtures get the `link_count` field on `AgentPhase` literals and
  `total_skip_links` on `ConsciousnessMetrics` literals.

Test coverage: lib suite green (516 passed, 4 ignored) across default,
`--features serde`, and `--no-default-features` build modes.

---

## [1.1.0] — 2026-03-07

### Added (ClawHub skill)
- **Built-in Flux publishing** (ADR-0011 Phase 3): `FLUX_URL` / `FLUX_AGENT_ID` / `FLUX_STREAM` env vars documented in `SKILL.md`, `_meta.json`, and `kannaka.sh`; `memory.stored` and `dream.completed` events now published automatically without requiring separate `flux.sh` calls
- **Collective memory section** in `SKILL.md`: three-layer architecture (Dolt / Flux / DoltHub), branch conventions (`<agent>/working`, `<agent>/dream/<date>`, `collective/*`, `collective/quarantine`), wave interference merge rules (constructive / partial / destructive)
- **Paradox Engine section** in `SKILL.md` (ADR-0012): snapshot-project-merge pattern, three resolution strategies (Consensus / Holographic Projection / Irreducible), Carnot efficiency metric (η), `--features "dolt collective"` build instructions
- **Sensory commands** in `kannaka.sh`: `hear <file>` (audio perception, `--features audio`) and `see <file>` (glyph/visual perception, `--features glyph`)
- **`announce` command** in `kannaka.sh`: calls `announce-status` on the binary to publish agent status to Flux
- **New build feature targets** documented: `collective` (rayon parallel dreaming), `audio`, `glyph`
- **New env vars** in `SKILL.md` env table and `_meta.json` optional list: `FLUX_URL`, `FLUX_AGENT_ID`, `KANNAKA_AGENT_ID`, `FLUX_STREAM`

### Changed (ClawHub skill)
- `_meta.json` version bumped from `1.0.2` → `1.1.0`
- `SKILL.md` features table expanded; Flux integration section rewritten to reflect built-in publishing; data destination note updated (Flux no longer requires explicit `flux.sh` calls)
- `README.md` features table updated with Collective memory, Paradox engine, Sensory perception, Built-in Flux rows; build instructions expanded with all feature flag variants; file structure comment updated
- `kannaka.sh` help output adds `Flux / Collective` and `Sensory Perception` sections; environment line includes `FLUX_URL` / `FLUX_AGENT_ID`
- Security notes in `_meta.json` updated: Flux publishing disabled by default; events carry metadata only (never full vectors)

## [1.0.2] — 2026-03-07

### Added
- **OpenClaw skill on ClawHub** (`workspace/skills/kannaka-memory/`)
  - `SKILL.md` — full skill definition with prerequisites, env vars, usage patterns, and Flux integration
  - `scripts/kannaka.sh` — CLI wrapper for all commands: `remember`, `recall`, `dream`, `assess`, `stats`, `observe`, `forget`, `export`, `migrate`, `health`, and complete `dolt` subcommand tree
  - `references/mcp-tools.md` — all 15 MCP tools with input/output schemas and wave dynamics reference
  - `references/dolt.md` — Dolt SQL setup, DoltHub publishing, speculation branch workflow, and multi-agent memory sharing guide
  - `README.md` (skill) — ClawHub listing content with feature table and Flux/Dolt integration overview
  - `_meta.json` — registry metadata with explicit `requires`, `optional`, `dataDestinations`, and `securityNotes`

### Fixed
- **Security: DOLT_PASSWORD process-list exposure** — replaced `-p$DOLT_PASSWORD` mysql flag with `MYSQL_PWD` environment variable in `kannaka.sh`; password is no longer visible in `ps aux`

### Changed
- `workspace/skills/flux/SKILL.md` — updated public Flux instance URL to `https://flux-universe.com`
- `workspace/skills/flux/README.md` — replaced hardcoded `192.168.50.13:3000` LAN IP (3 occurrences) with `flux-universe.com`; cleaned up ClawHub install note
- `README.md` — updated OpenClaw section to lead with `clawhub install kannaka-memory`; added ClawHub skill features list and flux-universe.com link
