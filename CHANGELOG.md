# Changelog

## [Unreleased]

## [0.16.12] — 2026-09-24

### Recall drops memories that expired before the instant it scores as of (#1044)

`HrmStore` has carried `expires_at` since the temporal triple landed, `remember --expires` and
the batch `expires` field stamp it, and nothing on the recall path read it: a superseded fact
ranked exactly like a current one, with or without `--at`. Now `KannakaMemorySystem::recall`
(and the beam path) skips a memory whose `expires_at` is at or before the recall instant
(`--at`, else the wall clock), over-fetching only when the store holds any expiry at all so a
store that never stamps one is byte-identical to before. `KANNAKA_RECALL_EXPIRED=keep`
restores the old behaviour. Motivated by kannaka-bench E-L3c/E-L3d, where a write-time
supersession reflex stamps `expires` at ingest and the harness had to simulate this rule.

This sits beside, not instead of, the temporal-decay floor (`KANNAKA_RECALL_TEMPORAL_FLOOR`,
which only applies when `KANNAKA_RECALL_TEMPORAL_EXP` > 0 and deliberately never reaches 0 so
"what did we use before" stays answerable). That question is now answered with `--at`: recall
as of an instant before the stamp still returns the older fact, which the new test checks.

### Added — Simulated Bifurcation as a `ConsolidationSolver` (kannaka-quantum Wave 4, W4.7a)

`qubo::SimulatedBifurcation`: ballistic simulated bifurcation (Goto et al. 2019,
2021), the quantum-inspired classical solver the September 2026 field review found
actually delivers, beside `ClassicalAnneal` under the same ADR-0038 seam: same
`SolveBudget`, same entropy-seeded provenance, exhaustive below `EXACT_THRESHOLD`
so small dreams stay exact whichever solver is used. The QUBO is mapped to Ising
by `x = (1 + s)/2` (a test checks the map preserves the ordering of every
assignment up to a constant); the answer is the best `sign(x)` seen, scored with
the problem's own `energy`, so it cannot report an energy the objective
disagrees with. On random dense problems just above the exhaustive threshold it
reaches the brute-force optimum, and it is at least as good as the annealer on
the same seeds in the test's sample.

Not wired into the engine: nothing selects a solver yet (T3.5's re-score-before-
apply path decides that), so this changes no dream. Which solver keeps more of
what later mattered is the one-week dream diff's question.

## [0.16.11] — 2026-09-22

### A recall no longer rewrites the store to count itself (#977, PR #1041)

Every production recall ended in a full `.hrm` save. #1010 blamed
observation, and observation is one writer — but with `KANNAKA_RECALL_OBSERVE=0`
the file was still rewritten, because ADR-0036's `record_retrieval` reached
each hit through `get_mut`, which marks the whole medium dirty on
acquisition. On a 1,678-memory store that was a 135 MB rewrite per
`kannaka recall`, and roughly half of a 16.6 s recall.

`record_retrieval` is now a backend method that touches the cache and owes
only the `.reactivation.json` sidecar (a few KB, merge-on-write, the same
file the serve daemon already flushes). `save_medium` settles that debt
without touching the `.hrm`; a recall that observes nothing writes nothing
but the sidecar. Observation on recall keeps its default (on) — that is the
"storage is computation" half, and a separate decision.

### A fresh store is chiral from birth — `remember --batch` no longer builds a different store (#917, #1031, PR #1039)

`HrmStore::new` started FLAT and only became chiral when the next process
loaded it (`ChiralMedium::load` converts a v1 file). So the shape of a store
depended on how many processes had touched it: twenty `kannaka remember`
calls, each saving and the next reloading, produced a v2 store with **61**
rows from twenty compound turns; one `remember --batch` of the same twenty
lines never reloaded, stayed on the flat branch of `absorb`, minted **no
facets**, and wrote a v1 store with **20** rows. The flat branch also pays
`apply_interference` over the whole medium on every insert — 0.797·n ms/item,
455 ms/item at n=600 against 100 chiral, ~16× at n=2400 — so bulk ingest was
quadratic for no reason but a missed reload. Identical input, different store;
ruled an ingest correctness bug, not a benchmark artifact.

Now `new` builds the chiral medium the way a v1→v2 conversion would (an
empty `from_medium`), so a store that was never reloaded is the same shape as
one that was, the first save is already v2, and bulk ingest runs flat in n
(a full 2,989-item LongMemEval store in 356 s, measured). `HrmStore::new_flat`
keeps the legacy v1 medium reachable for tests of the flat path and of
`upgrade_to_chiral`.

The flat "view" medium that several readers still consult (`consciousness_metrics`,
`find_associated`, the legacy `recall_resonance`, the queen's phase derivation)
was a load-time snapshot: `sync_medium_from_chiral` filled it only when empty, so
every row written in-process after load was invisible to those readers — and a
store chiral from birth would have had an empty view for its whole first
process. The sync now appends the rows the view lacks, by id, after each chiral
write (once per bulk load). Visible effect: the metrics a long-running writer
publishes now count the memories it absorbed since it started.

**Consequence for kannaka-bench:** every published hit@k / recall@k /
evidence-coverage figure was measured on stores built by `remember --batch`,
i.e. with no facet rows, while an incremental user's store is mostly facet
rows by count. Those numbers need re-taking on this build.

### ⚠ Read before rolling — dreams start forgetting for real (#917, #1035)

`stage_prune` and `stage_retention_triage` soft-delete by setting
`amplitude = 0.0` through `get_mut`, which writes the CACHE; the medium's
energy is untouched. Every dream ends in a path that calls `rebuild_cache`,
which reconstructed amplitude from the medium — so **every ghost came back
alive before anything could act on it.** Measured on 0.16.1: a dream reporting
`pruned 25` left 11 distractors live and 0 rows at amplitude <= 0. The O1 log
line "retention triage audio:heard: ghosted 37" was counted at ghost time.

The fleet has therefore been running with forgetting silently undone. This
release stops that, and the consequence is operational rather than theoretical:

- retention triage is NOT the exposure — `KANNAKA_TRIAGE=1` is set on O1, but
  there is no `[retention]` section in its config, so there are no rules;
- **`stage_prune` is.** It is core dream behaviour, gated by no flag, and at
  the 0.16.1 rate that is roughly 25 rows a night per node, ongoing;
- ghosts are SOFT deletes with the ADR-0037 recovery window, so nothing is lost
  immediately — but `stage_compact_ghosts` hard-deletes past the 7-day horizon,
  so real deletion begins about a week after this lands;
- before the fix, `compact_ghosts` reclaimed nothing at all: its first test is
  `amplitude != 0.0 -> continue`, and no ghost ever reached it with amplitude 0.

Counts for scale at the time of writing: kannaka-prime 1187, gossipghost-01
1149, the workstation 1678, the witness 434. **Take a substrate snapshot before
rolling**, and expect `total_memories` to trend DOWN for the first time.

Pinned memories are protected on every path (ADR-0031), and `compact_ghosts`
fails closed — it refuses to reclaim a ghost whose stamp is missing, because a
ghost that lingers costs space while a ghost deleted without its window is gone.

The fix itself is small: `rebuild_cache` preserves a ghost across the clear,
alongside the cache-only state it already preserved. #497 had preserved a
ghost's `updated_at` stamp, so a ghost kept its recovery-window paperwork and
lost the ghosting that paperwork documents. Measurement also settled a question
the issue left open: the hemisphere energy floor does NOT lift a ghost back.

### Added — the bus now records when a memory is used, not only when it is written (#1038)

`kannaka swarm serve` publishes `KANNAKA.events.memory.<agent>.recall` after
each recall it serves on `KANNAKA.recall.<agent>`: the returned ids in rank
order, their similarities, `top_k`, `via`, and a SHA-256 of the query. Never
the query text and never content; the hash is there so a reader can tell one
poller asking the same thing every minute from many distinct askers. It lands
in `KANNAKA_MEMORY_EVENTS` under the existing capture (90-day window), after
the reply so it delays none, as a plain publish so it cannot steal the
responder connection's subscription bytes. Nothing replays that stream and the
event changes no state.

Why: the memory stream carried only `remember` events, so nothing on the bus
could say which memories later mattered. kannaka-wave E-007 scores salience
against exactly this. Not published: the CLI's local recall (it stays off NATS,
as documented; the MCP plugin uses it) and `recall --batch` (a benchmark hit is
not a memory mattering). Flows only once a release carrying this runs
`swarm serve` on the hosts.

### Fixed — atomic-write follow-ups: dir fsync, temp litter, rename retry (#934, #1033)

Three items from the #933 adversarial review. The parent directory is now
fsynced after the rename (unix), so a power cut cannot leave the old name
pointing at old contents. `.kannaka-tmp-*` orphaned by a hard kill between
create and rename is reclaimed from `HrmStore::load`, with an hour-old floor so
a concurrent writer's temp file is never touched. And the rename retries three
times at 50 ms on `PermissionDenied` only — the transient Windows failure where
another process briefly holds the target — while a permanent error still fails
at once, with its real message.

P1 was checked and needs no change: the only symlinks under `~/.kannaka` are
piper's shared libraries and a `snapshots` directory link, and the latter is
safe because the temp sibling is derived from the target's parent, so both
resolve into the same real directory. That also keeps it a same-filesystem
rename, which matters because `/var/oled` is a different filesystem from `/`.

### Fixed — self-origin is every identity this node published under (#890, #1032)

`swarm exemplars publish --agent-id X` publishes under X, but both absorb
sweeps compared the incoming source against `cfg.agent.id` alone, so a node
re-absorbed its own material as a peer's: false cross-agent novelty, provenance
stamped `swarm:<override>`, and autoabsorb quota spent on its own output.

`self_identities()` is now the single source of truth, unioning the configured
id, the legacy `agent_id` file, and anything recorded at publish time. The
override is a run-time argument, so it is recorded when used — on a SUCCESSFUL
publish only, because recording at parse time would let a dry run teach this
node to ignore a real peer that happens to use that id.

### Fixed — three of seven OODA rotation slots could never move the fitness (#1029)

#939 got the nightly cron running again; its first real cycle then showed ten
runs across both arms all returning exactly 0.202656. Three slots were dead:
`chain_carry_strength` and `dream_gravity` are overwritten by both levels the
cron runs (research.rs:1477, :3436, :3460), and `kuramoto_steps` had a `FROM`
of 20 after the value moved to 50 in June, so its `sed` matched nothing and the
night exited 0 on "param edit did not take".

Each dead slot still wrote a confident `revert` row — a negative result nobody
measured. The cycle now flags bit-identical arms as `INERT?`.

⚠ The first fix replaced one dead knob with another: `xi_repulsion_weight` is a
dead FIELD, read nowhere. The guard asked "is it overwritten?" when the question
is "does it reach the measured code?" — and CI passed the branch that carried
it.

### Fixed — a stream-create refusal is not a server error (#969, #1027)

#1018 took the doomed `$JS.API.STREAM.CREATE` attempts from four per
`swarm serve` start to one, verified on O1 after the v0.16.10 roll against a
pre-registered prediction. The surviving line is no longer printed as
`[nats] server error`: the broker declining stream creation to a reader identity
is expected and fully handled, so it now says so once per process. Nothing about
the create path changed — a genuine writer never receives this refusal.

### Fixed — KANNAKA_RECALL_OBSERVE=0 does not stop the write (#977, #1030)

#1010's doc comment claimed the knob separated "the medium's ranking cost from
the write it provokes". It does not. ADR-0036 calls `record_retrieval()` on
every hit through `store.get_mut`, which marks dirty on its own, so the full
save still happens. Measured on a real 1678-memory / 135 MB store: plain
16608 ms, `RECALL_OBSERVE=0` 8484 ms (still rewritten), `READONLY=1` 9752 ms
(not rewritten), both 4638 ms. Comment only; the read-only recall path is #977.

### Ops — host-metrics must notice when its own POST fails (#1034)

`ops/services/host-metrics.sh` discarded both the response and the exit status
of its telemetry POST (`>/dev/null || true`), so a flux outage, a revoked
`FLUX_TOKEN` and a healthy run were byte-identical: no output, exit 0, nothing
logged. This is the fleet's staleness monitor — `disk_pct`, `hrm_mb`, `load1`,
`orphan_hrm_tmps`, `stale_crons` every ten minutes — so when it stops reporting,
everything it watches goes unwatched and nothing says so.

It now captures the HTTP status, writes one dated line to stderr on a non-2xx
and exits non-zero, without aborting the run or spamming cron mail. Verified on
O1 three ways: unreachable host (status 000, exit 1, logged), wrong path (404,
logged), real endpoint (silent, exit 0).

⚠ Worth keeping from how it was found: sweeping for this class produced three
FALSE alarms first. `hrm-sync-o3.log` looked 312 h stale and `host-metrics.log`
63 h on a ten-minute schedule — both healthy, because `>>` does not update the
mtime when the command writes nothing, and both jobs are silent-on-success by
design. A log file's mtime is not a liveness signal for a job that only speaks
on failure.

### Security — rustls-webpki 0.103.13, rand 0.8.6 (#1028)

Five dependabot alerts (1 high, 1 medium, 3 low) to zero. Lockfile only, bumped
with `cargo update --precise`: exactly two version lines and two checksums moved.

## [0.16.10] — 2026-09-21

### Fixed — autoresearch had produced nothing for 126 nights, and the reason was a PATH (#939, #1023, #1024, #1025)

`research/autoresearch-cron.sh` runs nightly on O1 and again on Sundays at OODA
level 5. **Every one of the 126 retained logs ends the same way**, from
2026-05-09 to 2026-09-12: five `FAILED (no fitness line)` and `all baseline runs
failed; aborting`. `experiments/ooda-state.json` has been frozen at
`"last_harvest": "2026-04-14"` throughout.

**The stated cause was wrong, and so was the first fix's restatement of it.**
#939 attributed the abort to a cold rebuild that would not fit under
`MemoryMax=2200M`, and #1023 repeated that. The script never reached a compile.
Running the original command inside the exact cron scope with stderr visible:

```
timeout: failed to run command 'cargo': No such file or directory
cargo exit=127
```

`cargo` is at `~/.cargo/bin/cargo`, which rustup adds to PATH from the shell
profile. A `systemd-run` scope sources no profile and inherits the manager's
`PATH=/sbin:/bin:/usr/sbin:/usr/bin`. Exit 127 gives empty stdout, so the `awk`
fitness match found nothing, so the loop printed "FAILED (no fitness line)" —
and the one sentence naming the cause went to `2>/dev/null`. Four months of a
fault one absolute path wide, and the discarded stderr is what made it last.

What changed:

- The baseline and hypothesis loops run the **prebuilt** `target/release/research`
  and keep its stderr; a failed run prints it instead of only "no fitness line".
- A missing or stale binary aborts loudly, naming the binary, what is newer than
  it, and the command to build it. `OODA_ALLOW_BUILD=1` opts back in.
- Every compile goes through one `build_research` helper that resolves cargo
  through `$CARGO`, PATH, `$CARGO_HOME/bin`, `$HOME/.cargo/bin` and
  `/usr/local`, with its own timeout and its own log — and refuses with a named
  reason when none of them holds an executable.
- The script **refuses to run as root**. O1's crontab reached it through
  `sudo systemd-run --scope` for the memory cap, so its opening
  `git reset --hard origin/master` had been rewriting an opc-owned checkout as
  uid 0 — **176 working-tree files and 991 objects under `.git`** by 2026-09-21,
  at which point `git pull` failed with `unable to unlink old
  'ops/roll/roll-node.sh': Permission denied` and the script's own nightly sync
  had been failing identically, reported nowhere. `--uid=<owner>` keeps the
  memory cap and fixes it; `OODA_ALLOW_ROOT=1` is there for a checkout that
  really is root's.

Two status-reading defects were found in the same file, both the same shape:
`if ! cargo build ... | tail -5` tested `tail`, so a failed build reported
success (#1023); and `local rc=$?` placed after a false `if` reported every
failed build as `exit 0` (#1025). The second survived its first test because
that test asserted only that a failure was reported, never what the report said.

`tests/autoresearch_cron.rs` runs the real script against a scratch tree with a
stubbed cargo and a stubbed research binary, and is named explicitly in
`ci.yml`. Six mutation controls were run against the real script on O1 and all
six were detected.

⚠ Operational, not in this tag: O1's crontab now passes `--uid=opc
--setenv=HOME=/home/opc --setenv=OODA_ALLOW_BUILD=1`, both entries log to a real
file instead of `/dev/null`, and the Sunday entry's `-p
WorkingDirectory=` — not a valid property for a scope, so that run had **never
started** either — has been removed. A no-push validation cycle on 2026-09-21
completed baseline, build, hypothesis and revert: the first full OODA cycle in
126 nights.

### Fixed — the facet-decompose flag is per thread, so tests stop racing (#942, #1022)

Four tests mutated `KANNAKA_FACET_DECOMPOSE` in the **process** environment
while `cargo test` ran them as threads, so a reader in another test saw a value
it never set: 2 of 3 full-suite runs failed on *"flag off must store exactly one
wavefront"*. #986 made the guard restore on drop, which fixed the durable leak
but not the race, and said so — the victim was a reader that took no lock, and
serialising the mutators could never reach it.

The refactor #986 declined turned out to be one function: `decompose_enabled`
has exactly one production call site. Tests now set a **thread-local override**
that it consults first; production is untouched and still reads the environment.
Net −25 lines, with the mutex, the restore-on-drop guard and its test all dead
and removed. The new test has a control arm so it cannot pass by the override
doing nothing.

### Fixed — a reply may only go to the inbox that asked (#943, #1020)

`swarm serve` emits on a stranger's behalf under an identity far more
privileged than the caller's: `anon` may publish `KANNAKA.ask.broadcast` but is
denied `KANNAKA.work.>`, `KANNAKA.inbox.>` and the JetStream admin subjects,
while the daemon authenticates as `kannaka_internal`, which publishes `>`. The
reply SUBJECT is caller-controlled input reaching that publisher.

#941 closed this for the `ask` handler. #943 predicted the guard would not be
inherited — and it had not been: `is_valid_reply_inbox` had exactly **one**
production caller, while the `recall` and `neighbors` handlers replied to the
same caller-supplied field with no check at all.

The check now lives in `SwarmTransport::reply`, the chokepoint all twelve reply
sites pass through, so the next handler inherits it by construction. Nothing
legitimate is refused: every reply answers an inbound request and this client
mints `_INBOX.<tag>.<pid>.<uuid>.<nonce>`.

⚠ Defence in depth, not the fix #943 asks for — the guarantee is the code's,
not the broker's. A second connection under an identity scoped to `_INBOX.>`
is what makes it structural, and needs broker config rather than a commit.
#943 stays open.

### Ops — an empty unit set is not a successful roll (#1019)

On the v0.16.9 roll, O3 printed `== ROLLED ==` having restarted **nothing**: its
units had been stopped minutes earlier for store maintenance, the discovery
filter only lists `--state=running` units, and `FAIL=0` is trivially true over
zero of them. `roll-node.sh` now distinguishes "units exist but are down" (names
them, exit 3) from "this host runs no kannaka units" (exit 0). Only a genuine
roll still prints ROLLED.

### Fixed — the stream-create refusal is remembered per PROCESS, not per connection (#969, #1018)

0.16.9 claimed #996 removed "~6 s of dead stall on every connect". Verified on
O1 after the roll, it did not: still four `Permissions Violation` lines and four
`JS_API_TIMEOUT` stalls per `swarm serve` start, unchanged from before.

The guard was not unreachable — it was scoped wrong. `swarm serve` opens **four
independent transports** (main, recall, neighbors, reply), each with its own
`Conn` and its own fresh `stream_create_denied` flag, so there were never
repeats *within* a connection to suppress. Every connection paid its own first
refusal and its own 3 s timeout.

The broker judges the identity, not the socket: one process connects with one
set of credentials, so a refusal on any connection answers for all of them. The
refusal is now recorded process-wide. Deliberately not reset on reconnect — if a
caller ever connects with different credentials in one process, this has to
become per-identity instead.

## [0.16.9] — 2026-09-21

### Fixed — over-cap energy really is clamped on load now (#1008, #1009)

0.16.6 said: *"Persisted over-cap energies are clamped on load: the live 7.74 /
8.51 records come down to 2.0 on the first restart of this version."* **That was
not true.** Nothing on the load path touched them — the four `ENERGY_CAP` sites
were in `insert` and `sync_cache_to_medium`, both write paths — so a store
written by an older build kept its over-cap energies through every restart.

The test that was supposed to prove otherwise was vacuous, and instructively so.
It set an over-cap energy on the hemisphere and called `flush()`, but `flush()`
runs `sync_cache_to_medium`, which rewrites every energy from the cache —
measured at 8.5 before the flush and 1.0 after. The file never held an over-cap
value, so the assertion passed at 1.0 with load never asked to clamp anything.
Its author had anticipated exactly this vacuity and written a countermeasure
that did not work. A second attempt, persisting via the medium's own `save()`,
failed the same way for a different reason: `HrmStore` auto-saves on `Drop`, so
doctoring the file while the writer is alive is undone before the test reads it.

`HrmStore::load` now clamps every persisted energy on both the chiral and flat
paths, before the cache is rebuilt so `all_memories()` agrees. The test doctors
the file after the writer is dropped and **asserts the precondition** — that the
file really does hold an over-cap value — so it can no longer pass by having
nothing to clamp. Both halves are mutation-checked: removing the clamp fails it,
and neutralising the doctoring fails it with "this test would prove nothing".

This is also the best current explanation for the 2.2125 record found on a live
store in #997: it survived load because load did not clamp.

### Added — `recall --at`: score recency as of a chosen instant (#994)

`temporal_weight` decays from `observed_at` to *now* and clamps up to the
superseded floor, and that clamp binds at two half-lives — 360 days by default.
So on any store whose contents are older than that, every candidate returns the
floor: the temporal factor degrades into a constant multiplier that cannot
reorder anything, does not fail, and reports nothing. Measured on longmemeval
(2023 data, 2026 wall clock): similarity 0.25 for every candidate, ordering
byte-identical to the flag being off. Lowering the floor only moves the
constant.

`--at` measures recency from a chosen instant, which is also the honest question
an agent asks — "what did we use last March" wants recency as of March, not
today. `recall --batch` takes a per-row `at`, because a benchmark run asks
hundreds of questions each with its own date and a process-wide flag cannot
express that.

### Docs — ADRs recorded (#962, #998)

**ADR-0063: referential memory** — a fact with an authority must not be held as
a wave. **ADR-0051** records that the Phase 3 gate was satisfied and the flag
measured negative; the preconditions themselves shipped in 0.16.8 (#992).

### Fixed — one data directory, one agent identity (#946, #1016)

Two `kannaka remember` invocations against one data directory published under
two *different* agent ids twenty seconds apart. ADR-0039's corroboration rests
on distinct-lineage agreement, so a process that mints a fresh identity per run
can manufacture its own corroborating peers — the Sybil shape the gate exists to
prevent. The gate is off today, so this closes a hole rather than stopping an
exploit.

Two causes. A minted id was never written down: `persist_agent_id_compat()` is
called from three init paths and never from `load()`, so a node that never ran
`init` minted a new identity every process. And the persisted `agent_id` file
was consulted **only when `config.toml` was absent** — a config that merely
omitted `[agent] id` skipped it and minted, even on a node that already had a
good id on disk. The precedence documented in `load()` (env > config.toml >
persisted file > generate new) was aspirational rather than implemented.

A serde default cannot tell "configured" from "defaulted", so the raw TOML is
now checked for the key before deserializing. Persisting is best-effort and
skipped under `KANNAKA_READONLY`.

Still open in #946: binding an id to a lineage inside the gate, whether an
ephemeral id should publish to `KANNAKA.memory.new` at all, and that this subject
is bound to no JetStream stream so a sync cannot be audited after the fact.

### Tests — ingest-cost probes, and the O(n²) located (#978; #1013, #1014, #1015)

Three `#[ignore]`d probes (no CI cost) that measure ingest cost against store
size. They locate #978's quadratic precisely, and correct where the issue
placed it:

| path | growth over ~8x size |
|---|---|
| `Medium::store` (flat backend) | **x18.74** |
| `store_with_facets` (chiral, 3x rows) | x1.01 |
| `HrmStore::insert` (either backend) | flat |

The O(n) per insert is `apply_interference`, reached through `Medium::store` —
not `HrmStore::insert`, which is flat on both backends. A store is flat for
exactly one process lifetime: `HrmStore::new` sets `chiral: None`, and
`ChiralMedium::load` converts a v1 file on first reload. That window is when
bulk ingest happens, which is why a benchmark sees O(n²) and the fleet does not
(both live stores verified on disk as v2/chiral).

### Added — two recall knobs that make measurement possible (#977, #1010; #979, #1011)

Neither changes default behaviour. Both exist because a measured problem could
not be isolated without them.

`KANNAKA_RECALL_OBSERVE=0` stops a recall reshaping the field it just read.
Observation is deliberate ("attention IS computation") and is the ADR-0036
replay signal behind tier promotion, so it stays on — but every recall calls
`mark_dirty()`, and a one-shot CLI read therefore pays a full save on exit.
Measured against a copy of a live 1671-memory store: a single `recall` took
**9.45 s and rewrote all 135 MB** of the `.hrm`. ⚠ Gated at BOTH observation
sites — the chiral branch observes inline rather than through the shared helper,
so a gate in the helper alone would have done nothing on the stores the fleet
runs.

`KANNAKA_RECALL_INCLUDE_DREAMS=0` returns evidence only. A dream row is a
cross-cluster synthesis, not a turn, so it can never be the evidence a question
needs — yet after a *single* deep dream it took rank 1 in **24 of 30** bench
questions, moving MRR from 0.950 to 0.529 while hit@15 held. The filter
over-fetches and truncates rather than returning short, because the complaint is
that each dream is a top-k slot spent. `resonate_query` became a thin wrapper
over a private inner method so the filter lives in one place rather than at each
of its four return paths.

Still open in #977 and #979: the speedup itself, why dreams outrank real
memories, and the unexplained 2.3x post-dream recall improvement.

### Fixed — a stream create the broker already refused is not re-issued (#969, #996)

A permissions refusal for `$JS.API.STREAM.CREATE` arrives as an async `-ERR` with
no JetStream reply, so `ensure_js_stream` could not fail fast: it waited out
`JS_API_TIMEOUT` (3 s) and only then errored. A connect ensures two streams, so
every non-writer identity paid **~6 s of dead stall on every connect**, plus a
"server error" log line per attempt that reads like a fault and is not one. On a
node whose `swarm serve` reloads at the writer's save cadence (#563) that
repeated every few minutes all day, and those lines were misread as the *cause*
of the restarts during the v0.16.7 roll.

The mechanism already existed and was wired to one caller: #933 records the
broker's verdict in `Conn::stream_create_denied` and `ensure_presence_stream`
consults it, but `ensure_js_stream` — which all seven stream helpers go through —
did not. The flag is cleared when `reconnect()` replaces the Conn, so a genuine
writer that reconnects still creates.

### Fixed — three energy writes that did not respect `ENERGY_CAP` (#997, #999)

0.16.6 capped energy "at every write", but capped the amplitude→energy
conversions and the boost sites. Both `medium/sync.rs` coupling paths clamped to
**10.0**, a ceiling predating `ENERGY_CAP` — and the multiplier there is a
*peer's* energy, a value the local node did not compute. `medium/chiral.rs`
callosal reinforcement had no ceiling at all, the only energy write in the medium
without one.

Found by measuring a live store: of 1671 memories, exactly one sat at 2.2125
against a cap of 2.0. ⚠ That specific record is **not** attributed to these
sites — a test for the callosal path passed with its ceiling removed, because the
branch is gated by the callosal budget and did not fire. The remaining suspects
are the growth and interference sites, which bound only the floor; those are the
medium's physics and remain an open design question in #997.

### Added — `content_digest`: a `.hrm` digest that ignores when the file was saved (#952, #1000)

Every save stamps `Utc::now()` into the header and the trailing blake3 covers it,
so two saves of an unchanged store a millisecond apart differ — same length,
different checksum, identical meaning. A file hash therefore could not answer
"did this store change", which is the cheapest integrity check there is and the
one the encoder-flip runbook leans on ("assert the store sha changed").

`content_digest` hashes the file except the 8-byte timestamp window and the
trailing checksum. v1 and v2 share the header layout, so one window covers both.
Deliberately does **not** make files byte-identical: #952's other option was to
derive the timestamp from content, which answers the question by destroying the
data.

### Tests — assertions that were written down and never made

A sweep driven by `unused variable` warnings in test code, which turn out to be a
reliable marker for an assertion that went missing (#1001, #1002, #1003):

- `queen.rs` `order_parameter_trust_weighted` stated "r = 1/2" in its own comment,
  asserted only `psi`, and dropped `r` — so the zero-trust case it is named for
  was the one it could not catch.
- `medium/tests.rs` asserted `energy != initial || wavefront_count() == 2`. The
  second disjunct is true by construction, so the test **could never fail**.
- `chiral.rs` `deep_dream_only_affects_right` never checked that the right
  hemisphere *was* affected; it passed if `dream` did nothing.
- `chiral.rs` `callosal_kuramoto_modifies_phases` returned early on an empty left
  hemisphere — a silent pass on the test whose subject is coupling between them.
- `hrm_store.rs` `hrm_store_persistence` checked that content survived a reload
  but never that the **id** did (the defect #949 fixed on the import path).
- `consolidation.rs` `destructive_interference_weakens_memories` — plural —
  checked one half of the pair.

Two tests named for skip-link wiring, a feature that was **removed**, asserted
only that consolidation ran; renamed to what they verify. Two git-backed tests
were `#[ignore]`d as "requires git repo" when each builds its own repo in a
TempDir and is hermetic — enabled, taking ignored tests 18 → 16 (#1004).

### Docs — the intuition flag's documentation described a refactor that never landed (#1005, #1006)

`RecallResult::intuition` documented itself as flowing through to the CLI while
the code thirty lines below hardcodes `false` with a TODO saying it is not
plumbed. Corrected to state what is true; the plumbing is tracked in #1005 rather
than left as a comment.

### Ops — a real scheduled task for the Windows ask responder (#995)

`ops/windows/` gains `kannaka-swarm-serve.cmd` and `KannakaSwarmServe.xml`. The
workstation's responder had no service manager — it was kept alive by a bash loop
started from a terminal session, which would have died with it, leaving a node
whose presence advertises `ask` with nothing answering.

## [0.16.8] — 2026-09-21

### Fixed — `swarm sync`'s presence keeps the agent's display name (#991)

0.16.7 taught `swarm sync` to publish presence (#970) and passed `""` as the
display name. `swarm_publish_heartbeat` maps an empty string to `None` for the
*phase* payload, which is why it read as safe — but the *presence* record embeds
the string as given, so every tick overwrote the label `swarm join` had published
at startup and consumers rendered the agent nameless. It now publishes
`config.agent.display_name`, with `join`'s own fallback to the agent id.

Found by verifying the 0.16.7 roll rather than by review: the witness's roster row
came back alive — 21 s since last seen, down from 60,987 — and unnamed.

### Fixed — ADR-0051 Phase 3 preconditions: dedup, supersession retention, `swarm brief` (#992)

Three fixes the Phase 3 review gates `KANNAKA_RECALL_TEMPORAL_EXP` on, each
written to be correct whether that flag is on or off.

**A dedup recall must not score temporally (M9).** The autoabsorb dedup recall is
an admission gate, not a ranking. With the temporal factor live, stamping a local
memory as superseded lowered its own resonance *there*, dropped it under the
threshold, and re-admitted the peer's copy of the stale text with a fresh
`created_at` that reads as maximally recent — resurrecting the value the stamp had
just retired. `SuppressTemporalScoring` is an RAII guard around that recall which
restores the previous value on drop, so nesting, an early return or a panic cannot
leave scoring suppressed process-wide.

**A supersession record survives reclamation (M8).** An expired memory is not
stale garbage; it is the record that a fact changed, and the only thing that can
answer "what did we use before". The size cap now exempts it the way it exempts
Pinned, bounded by `KANNAKA_EXPIRED_RETENTION_DAYS`, so the past becomes
reclaimable by policy rather than by accident. Triage also no longer collapses a
supersession pair as a duplicate — it kept whichever carried more amplitude,
usually the older, more-accessed fact, the one that is no longer true.

**`swarm brief` demotes instead of dropping (M3).** The filter removed a
superseded memory from the brief entirely, unconditionally, while the ranking-side
temporal factor was off — so `--supersedes X` produced no visible demotion
anywhere and total invisibility on the one surface an operator reads, making a
false supersession both silent and unrecoverable. It is now multiplied by the same
floor the ranking path uses, via `temporal::brief_confidence`.

Ten tests, each asserting the hazard rather than the happy path, verified by
mutation: neutralizing all four behaviours fails 7 of 10, and the 3 survivors are
the 3 that should. That pass caught two of its own tests being vacuous — the M9
pair asserted the flag was 0.0 with the env unset, where the default is already
0.0, and an `assert_eq!` inside a `catch_unwind` closure made a test pass
precisely when suppression was broken.

### Added — `[llm] timeout_secs`: the LLM request timeout is configurable (#904, #987)

`src/agent.rs` hard-coded `.timeout(Duration::from_secs(300))` at four call sites
with no key to change it. A ~2.6k-token ask through `swarm serve` against
`kannaka-brain` measures 233 s idle and 314 s under load, so legitimate asks were
timing out and the only workaround lived on the caller's side — the server had no
say in its own timeout.

`AnthropicClient`, `OllamaClient` and `OpenAIClient` each carry a `timeout` now,
overridable with a chainable `with_timeout_secs`, and `client_from_config` threads
`cfg.llm.timeout_secs` through at build time. **The default is the old constant,
so an unset key changes nothing.**

### Fixed — the facet flag guard restores `KANNAKA_FACET_DECOMPOSE` on drop (#942, #986)

`lock_decompose_flag()` serialized the mutating tests against each other and did
nothing more, and a `MutexGuard` does not survive a panic. A test that set the
flag and panicked before its own `remove_var` left it **on process-wide**, and the
next test to read it through `decompose_enabled()` — which does not take the lock
— saw a stale value it never set. One failure became a cascade blamed on innocent
tests.

The guard now snapshots the flag at lock time and restores it on drop, so a green
run is the property rather than a scheduling outcome.

## [0.16.7] — 2026-09-20

### Added — an attention beam over the skip-link graph: measured, and shipped off (#988)

Dream consolidation had been writing skip links for months and nothing ever read
one. On a live 1,671-memory store that is **57,161 links, every memory carrying at
least one, median 22 each** — a graph rebuilt every night and driven on never.
`Medium::recall_against` always accepted a candidate set, but only
`recall_resonance_with_beam` ever passed one, so an ordinary recall scored the
whole field.

This adds the missing half: seed from the moment, walk the links outward, score
the neighbourhood. It is wired into `resonate_query` — the path the CLI, swarm,
chat and attention handlers actually take — behind `KANNAKA_RECALL_BEAM`, with a
dense fallback so a thin beam can never degrade into "no memories".

**It defaults off because it did not win.** The measurement is the deliverable
here rather than the feature; the traced run is recorded in the kannaka-bench
manifest.

### Changed — a new store defaults to the semantic encoder, and refuses to be created without one (#976)

The shipped default was `SimpleHashEncoder`: no semantics, no warning. On
LongMemEval-S the same medium scored hit@k 0.53 with it and 0.93 with all-minilm
(kannaka-bench), so every store a new user created started useless for recall and
nothing ever said so. `[encoder] kind` now defaults to `ollama` (all-minilm, 384d).

**An existing store needs nothing.** The `.encoder` stamp a store was written with
is now *adopted* when the caller has not explicitly set `KANNAKA_ENCODER`: reading
vectors with the encoder that wrote them is always the safe direction, and it is
what the guard exists to protect. An explicit selection that disagrees with the
stamp still refuses, as before. Every node of this fleet was checked before the
release — all six stamped, so all six adopt and only the one already running an
embedder needs one.

**A store with no stamp, on a host with no embedder, will now refuse to start
(exit 2).** An unstamped directory is indistinguishable from a new one, and a new
store quietly falling back to hash is the exact failure this change exists to
prevent — but the message says "refusing to create a new store" even where the
store is old and simply predates the sidecar. If that is you: stamp it with what
wrote it (`printf 'hash:384:42' > <data_dir>/.encoder`), or pass
`KANNAKA_ENCODER=hash` to choose the fallback deliberately.

### Added — `remember --batch` and `recall --batch`: NDJSON in, NDJSON out, one process (#973, #974)

Loading thousands of memories through the single-item CLI cost ~600 ms per spawn
(the encoder loads every time), which turns a 250k ingest into a two-day job;
`import` takes full wavefront records only. Both subcommands now read NDJSON and
emit one line per line in, in order.

Two quadratic costs were found and removed while measuring it. Every insert
flushed the whole medium and its sidecars twice, and `HrmStore::absorb` rebuilt
the entire memory cache after each one — 300 items took 447 s, at 4.2 s per item
by the end. `set_auto_save(false)` plus `begin_bulk()`/`end_bulk()` make a bulk
load linear; the medium stays complete throughout and recall reads the medium, so
only cache-backed views (`get`, `stats`, exact-repeat detection) lag until the
batch ends. Single-item `remember` is unchanged, and a batch never publishes to
NATS — a bulk load is a local act.

### Added — `KANNAKA_RECALL_XI_BOOST=off` makes the xi reranker measurable (#975)

On LongMemEval-S with the same MiniLM embeddings as an exact-cosine baseline, the
medium found the evidence session less often (0.78 vs 0.94 hit@5). Tracing a miss:
raw right-hemisphere resonance ranked like cosine, then the xi-diversity reranker
lifted candidates above 0.15 cosine with a repelling xi-signature by up to 1.8x
while the gold turn, whose xi did not repel, kept its raw score — a rank inversion.
There was no way to measure the medium without that reranker; now there is.
Default on, shipped behaviour unchanged.

### Added — the mail membrane, first slice (ADR-0062) (#972)

`membrane.py` polls every agent mailbox over loopback IMAP and publishes each new
message once to `KANNAKA.mail.<slug>.inbound` with the ADR-0062 §4 payload (auth
verdict first, untrusted text last). The UID watermark advances only after the
PubAck, and a content-derived `Nats-Msg-Id` makes retries idempotent. Proven
2026-09-17: Zoho to bus in 10 s. Not included: the ADR's 451 receiver, consumers,
outbound.

### Fixed — `swarm sync` publishes presence, and a deliberate `swarm serve` reload exits 0 (#970)

`swarm sync` only ever published `QUEEN.phase`, while the witness loop joins once
at startup and then ticks `sync` every 5 min believing it refreshed presence — so
`kannaka-witness-01`'s presence row aged out minutes after every restart while the
loop went on hearing every tick, and the roster showed it dead. `sync` now
publishes the presence heartbeat as well, before the Kuramoto step so the coupled
phase still lands last. `joined_at` is the true session start when the caller
exports `KANNAKA_SESSION_JOINED_AT` once; a value that does not parse as RFC 3339
is treated as unset rather than published.

`swarm serve` reloads by exiting whenever the HRM changes on disk (#563) and exited
with status 1, so on a node whose writer saves every few minutes systemd recorded
`Failed with result 'exit-code'` on every reload — 44 "failures" in three hours,
all this one line. Every fleet unit is `Restart=always`, so the status carried no
information except the wrong one. Real errors in that handler still exit 1.

### Fixed — `import` preserves the id on both paths that used to discard it (#949)

`import` skips a record whose id is already in the store, but for a record with no
`vector` that skip could never fire: the branch called `absorb`, which mints a
fresh id. So a `--slim` export could not be round-tripped without rewriting every
id, and importing the same slim file twice produced two full copies while
reporting `Skipped: 0`. The dimension-mismatch retry did the same thing. Both now
encode through the store's own pipeline and take the identity-preserving `insert`.

Behaviour change, stated rather than buried: `absorb` also ran ADR-0049 facet
decomposition and `insert` does not, so a slim import no longer mints facets
inline — run `kannaka facets backfill --apply` afterwards. A full export already
behaved this way, so the two paths now agree instead of differing silently. On a
legacy flat backend there is no id-preserving path at all; that case still absorbs
and now says so.

### Fixed — `backfill_all_facets` rebuilds the memory cache (#947)

It minted facet rows into the medium but left `memory_cache` holding only the
pre-sweep rows, so `all_memories()` and `all_ids()` returned none of the facets it
had just created until the store was next loaded. Nothing was lost or corrupted —
an invisible row is also not processed by anything downstream — which is why it
went unnoticed. Its two siblings both rebuild after mutating; this one now does
too, reporting a rebuild failure through the `stats.errors` channel it already has.

### Fixed — `ChiralMedium::save` writes its two maps in key order (#952)

`scales` and `left_to_right` are `HashMap`s, and `RandomState` randomises iteration
order per instance, so a store saved, loaded and saved again emitted the same pairs
in a different order: same length, different blake3, identical semantics. A
checksum could not answer "did this store change". Both are now sorted by key
before bincode; the loader collects back into `HashMap`s, so this is a pure
serialization change and existing files load unchanged. Note that sorting alone
does **not** make saves byte-identical — the v2 header carries a wall-clock
timestamp that the trailing checksum covers — so #952 stays open for that decision.

### Changed — the KAX corpus attributes replies to the brain that wrote them (#971)

The reader took `generated_by` from `reply["model"]` and fell back to
`agent-brain`, but replies never carried a model until kax-computer #29, so every
KAX exchange — including the open-weight machines' — read as Claude. It now prefers
the reply's own model, then the mirror's `brains.json`. On today's mirror: 135
replies, 99 Claude / 36 open-weight.

### Ops — the fleet roll scripts live in the repo (#968)

`roll-node.sh` and `roll-o1.sh` lived in session scratchpads for v0.16.3 and
v0.16.4 and were gone by v0.16.6, which had to reconstruct one from notes. Both are
now committed under `ops/roll/` with a README stating the order that has worked and
the trap each script encodes.

## [0.16.6] — 2026-09-16

### Changed — recall ranks by similarity alone, and energy is capped at every write (#965, #966)

Recall ranked by `similarity * energy^exp` with `exp` defaulting to 1.0. Measured
on the live O1 store against its own vectors and probes: production r@10 0.514,
plain cosine over the same vectors 0.960. In 80% of the misses the correct memory
had the higher cosine and lost on energy alone (the winner carried 3.7x the
target's). With the exponent at 0 the medium recalls at parity with cosine
(~0.97 by content). Everything else in the path measured clean: the codebook is
lossless for d_eff (x0.99), stored rows match their encodings at corr 0.994, and
facet resolution returns children under their parent by design.

`KANNAKA_RECALL_ENERGY_EXP` now defaults to **0.0** (ADR-0048's energy-neutral
ranking; `1.0` reproduces the old order). `ENERGY_CAP` (2.0) is applied at every
energy write — every boost path already clamped, but the four sites that copied
a memory's caller-supplied amplitude into energy on insert and on load did not,
so a memory remembered at amplitude 8.5 entered at energy 8.5 above a ceiling
nothing could lower it to, and sat in a third of all top-10 lists whatever the
query. Persisted over-cap energies are clamped on load: the live 7.74 / 8.51
records come down to 2.0 on the first restart of this version.

### Added — the re-encode tool builds a pipeline that can fail, and can reach a remote embedder (#961)

`recompute_encoding` used `make_pipeline`'s composite encoder, whose hash
fallback silently swallows an embedder outage — a bulk re-encode interrupted
mid-run would have left one store part semantic and part hashed under one
`.encoder` stamp. It now builds `make_strict_pipeline` (no fallback: an outage
is an error, `re_encode_all` propagates it, nothing is written) and takes
`KANNAKA_ENCODER_URL` / `_MODEL` / `_DIM`, probing the endpoint it will actually
use rather than a hardcoded localhost.

### Docs — ADR-0062: the mail membrane (#964)

## [0.16.5] — 2026-09-13

### Fixed — retention: "established" now means established, not merely recent (#950)

`stage_prune` skips destructive dampening for *established* memories under the
belief substrate. It decided establishment from amplitude alone, and an ordinary
write lands at 0.5–0.6 — already above the 0.5 floor. So a memory written a
minute ago was "established".

Measured on prime, the one node that runs `KANNAKA_BELIEF_PHASE=on`: **676 of
1054 memories were protected, and their median age was 1.6 days against 2.4 days
for the unprotected ones.** 75% of the protected set was under a week old. The
predicate was not merely too broad, it was inverted relative to its own name —
it selected for recency of writing, so four fifths of the store was shielded from
the dampening the belief substrate exists to apply.

Establishment now requires strength **and** age: `amplitude > 0.5` and at least
`KANNAKA_ESTABLISHED_MIN_AGE_DAYS` (default 7). On prime that moves protection
from 676 memories to 166, whose median age is 38.8 days. Of the 510 that lose it,
369 are ShortTerm perception rows that ADR-0054 wants cleared anyway. The choice
of 7 is not delicate: 3 days gives 170 and 14 gives 153, so any value past the
knee behaves the same.

`retrieval_count` would be the natural "has earned its keep" signal and cannot be
used — `rebuild_cache` resets it to 0 and it is absent from `WavefrontMeta`, so it
does not survive a save. `times_seen` only moves under reinforcement, which is
dark by default. `created_at` is persisted, and is what is left.

**Nothing changes on a node without the belief substrate.** The switch is the
first clause of the predicate, so on every other fleet member the result is false
before and after. `KANNAKA_ESTABLISHED_MIN_AGE_DAYS=0` restores the old
amplitude-only rule exactly; an unparseable or negative value falls back to the
default rather than to 0, because a typo must not silently reinstate the bug.

## [0.16.4] — 2026-09-13

### Added — eye: the video perception engine has had no callers since March (ADR-0008)

`src/eye/` has been in the tree since 2026-03-01: 1,607 lines of decoder, spatial
features, temporal features, block-matching optical flow and shot detection, behind
a `video` feature that was not in `default` and a `VideoPipeline` that nothing ever
constructed. One file had tests. The eye could not be reached from the CLI, from
`OpenClawSystem`, or from anything else. It perceived nothing.

**`kannaka watch <video-file>`** runs a clip through the whole path — ffmpeg decode
at 2 fps and 320 px wide, 192 spatial dims per frame, 128 sequence-level temporal
dims, projected through the EYE codebook (seed `0x3E5E`) into the same
10,000-dimensional space as text and audio — and prints duration, frames sampled,
shot count and cut positions, motion magnitude, brightness, contrast, visual tempo
and named dominant colours. `--json` for the machine-readable form, `--fps N` to
sample harder, `--long-term` to keep the clip out of short-term triage.

The verb is `watch`, not `see`. `see` is the SGA glyph path and keeps its meaning;
`watch` is temporal — shots, motion, arc.

**Into the HRM**, mirroring `store_audio`. `store_video` absorbs a perceptually
descriptive content string ("video:watched moving cut bright | 4 shots …"), never
the `video:/tmp/<hash>.mp4` path the encoder produces — embedding the path is how
the audio side once collapsed every capture into one hive with `xi_diversity` 0.
The wavefront is then tagged `Modality::Visual` explicitly rather than left to a
keyword vote, gets the ADR-0008 wave parameters (frequency 0.03, phase π/2, decay
3e-7), and lands `ShortTerm` like a `hear` does, promoted only if a dream
strengthens it.

**ffmpeg is a runtime dependency and is genuinely absent on some fleet nodes.**
`ffprobe` is now probed alongside `ffmpeg` — it is a separate binary, and a PATH
with only one of the two used to fail later as an opaque "ffprobe JSON parse
error". A missing decoder gives the operator a named, actionable message with the
install line for their platform, never a panic, and every test that needs a real
decode skips cleanly instead of failing.

### Fixed — eye: 32 of the 192 spatial dims were a hardcoded zero in every video

`extract_frame_features` reserves 32 dims for the optical-flow histogram and fills
them with zeros, because flow is a property of a frame *pair* and a single frame
cannot see one. Nothing ever filled them in: `motion.rs` — block matching, flow
histogram, motion statistics, 149 lines — had no caller anywhere in the crate. A
sixth of the spatial vector was a constant, and the pipeline could not tell a
locked-off tripod from a whip pan.

`VideoPipeline::analyze` now fills that band from block matching between
consecutive frames, and reports mean/std/peak motion in pixels. Frame 0 inherits
frame 1's flow rather than staying zero, so a clip no longer opens with a spurious
jump that the temporal motion trajectory reads as a burst of movement.

Two defects surfaced the moment the code was exercised:

**Block matching reported maximum motion for a static frame.** The search keeps
the first candidate with the lowest sum of absolute differences, and it starts at
`(-8, -8)`. A flat or uniform block matches every offset equally, so a tripod shot
of a clear sky came back panning diagonally at the corner of the search window.
Ties now break toward the smallest displacement, which is also the right prior for
real footage: given equal evidence, the block did not move.

**`dominant_colors` lost half of any frame with vertical structure.** The k-means
centroids were seeded at evenly spaced sample *indices*, and samples arrive in
row-major order, so the stride `samples.len() / k` lines up with the row period. A
frame that is red on the left and blue on the right seeded all four centroids on
red pixels — and identical centroids send every sample to cluster 0, collapsing
k-means to one average colour. Seeding is now farthest-point. The results are also
ordered by cluster population instead of by brightness, which is what "most
dominant" was supposed to mean all along; sorting by value let a one-pixel
highlight outrank the colour covering the frame.

### Changed — eye: `video` is a default feature and no longer pulls in `image`

The module cannot be reached from a default build otherwise, which is how it sat
dead for six months. `video = ["image"]` enabled the `image` crate and its whole
decoder tree; nothing in `src/eye/` ever referenced it — the decode is an ffmpeg
subprocess. The feature is now `video = []` and the dependency is gone, so
default builds gain the eye and lose a codec tree. ADR-0008 principle 4 holds: no
GPU, no neural inference, algorithmic features only.

### Tests — eye: five of seven files had none

`decode.rs`, `shot.rs`, `motion.rs`, `spatial.rs`, `color.rs` and `mod.rs` now have
51 tests between them, all on frame sequences synthesised in memory — no video
fixture on disk, no network fetch, no ffmpeg required. A hard cut between two solid
colours produces exactly one boundary; a static sequence produces none; a 4 px/frame
pan measures 4 px in the pan direction and nothing in the other; an identical frame
pair measures zero. `VideoPipeline::encode_frames` exposes the whole perception path
minus the decoder so tests, and any caller holding frames from elsewhere, can reach
it directly.


### Changed — remember: a fact seen again is reinforced, not duplicated

Rogue's grid job re-wrote the same verdicts on every run, and the store did what it
was told: it inserted them all. One identical sentence existed **five times**, each
at strength **0.400**, at ages 84.28 / 60.20 / 12.17 / 2.79 / 2.58 hours. If
repetition deepened a memory there would be one memory with a rising strength.
Instead there were five weak ones, none of which knew about the others.

The cost lands on recall, which is what makes this a correctness bug and not
housekeeping. `recall "what happened in colony-one"` at top-10 came back with **ten
slots holding five distinct facts** — one sentence occupied six of the ten. Every
duplicate is a fact the agent can no longer see. Duplication made the view
*shallower*. And because each write loads the whole store, every copy in a 190 MB /
2,366-memory medium slowed every write that came after it.

**`remember` now strengthens what it already holds.** Before absorbing, the write
path looks for a memory whose stored content is byte-identical to the new text after
trimming. If it finds one, that memory's amplitude rises, its repeat count goes up,
its recency is refreshed, and **its** id comes back. No second row. `remember` still
returns a `Uuid` for a memory that contains the text, so no caller changed: every
`remember*` call site either logs the id or uses it to stamp modality and temporal
bounds on the row it just wrote, and all of that stays correct when the row is one
that already existed.

**Exact match only.** Fuzzy merging of near-identical memories already exists, and it
already has the right home: dream consolidation decides that two wavefronts are the
same thought with the whole field in view and a snapshot behind it. Doing that at
write time would mean `remember` silently ruling that your new sentence "was" an old
one on a similarity threshold — lossy, surprising, and unreviewable. Byte-identical
text is the only repeat the write path can claim with certainty. Trimming surrounding
whitespace is the single normalisation applied.

**Bounded by construction.** A repeat closes a quarter of the remaining gap to the
ceiling: `a' = a + 0.25·(CEILING − a)`, so `n` repeats reach
`CEILING − (CEILING − a₀)·0.75ⁿ`. Every repeat is worth something, the tenth is worth
about 7.5% of the first, and the ceiling is approached and never crossed — it is the
same `AMPLITUDE_CEILING` dream consolidation's additive boosts respect, so a fact
asserted 500 times by a cron job can dominate the field no more than the strongest
dream-strengthened memory already could. A test walks 500 repeats and asserts the
step never grows and the ceiling never breaks.

An explicit `--importance` above the memory's current amplitude is honoured as a floor
before that step, so `kannaka remember "x" --importance 0.95` on something already held
raises it rather than dropping the number on the floor — the same silent-drop class
`remember_with_importance` was written to fix. A lower importance never weakens anything.

**Four things a repeat deliberately does not do.** It does not touch a **ShortTerm**
row's energy, because `compute_decay_set` picks the weakest half of that distribution and
lifting a row out of it makes ADR-0054's evict path permanently unreachable — for exactly
the `audio:` perceptions and cron repeats that config exists to clear. Those rows get the
count. It does not strengthen a **decomposed parent**; the facets gain instead, because
recall ranks on `similarity × energy` and ADR-0049 names "a parent can't out-rank its own
facets" as a blocker it defuses. It does not revive a **ghost**: an ADR-0037 ghost is a
memory the dream chose to let go, so the same text arriving again becomes a new memory and
the ghost is left to age out. And it does not **replicate** — `times_seen` is not on the
wire and a local `sync_version` is not comparable across agents, so no counter is bumped
and no `MemoryStored` event is published for a store that did not happen. Making salience
swarm-wide means putting the count on the wire with MAX reconciliation, which is an ADR.

**Recency lands on `observed_at`**, not `updated_at`. That is the field that already means
"when this agent observed the fact", the one in `WavefrontMeta`, and the one
`temporal_weight` reads. `updated_at` is neither persisted nor consulted by any recall
path, and `updated_at != created_at` with `retrieval_count == 0` is this codebase's ghost
stamp — writing it onto healthy rows would file every reinforced memory in
`.reactivation.json` under a signature meaning "ghosted, keep recoverable".

**The count is the point.** `times_seen` records how many times the world showed you
the fact, as distinct from `retrieval_count`, which records how many times *you* went
looking. It is the salience signal this whole change exists to create, so it has to
outlive the process: it rides a `.times_seen.json` sidecar next to the `.hrm`, with
the same merge-on-write reconciliation `.reactivation.json` uses, for the same reason
— appending to the bincode `WavefrontMeta` layout means extending a positional format
and its fallback-struct chain, and a sidecar carries no format risk. A memory nobody
ever repeated reads `1`, not `0`.

**`kannaka dedupe` collapses what is already on disk.** The write path only stops new
duplicates; the sets already written need a deliberate pass. It collapses rather than
deletes, because the duplicate set is itself evidence — five copies mean the world
showed you that fact five times, and a cleanup that simply dropped four of them would
throw away the one useful thing the accident encoded. The keeper inherits the summed
count, and the energy is advanced along the reinforcement curve once per folded copy,
starting from the *strongest* member so nothing the store already held is lost.

**The keeper comes from the same function the write path uses.** One rule, called twice:
highest retention tier, then oldest, then id. When the two paths had separate rules they
disagreed on a mixed set — dedupe kept one row while the next `remember` strengthened a
different one, so the duplicates were never actually resolved. Preferring the highest tier
is also what keeps a **Pinned** duplicate from being the casualty: ADR-0031 says Pinned is
never evicted and never demoted, and a collapse that deleted the pinned row and left an
unpinned keeper did both.

**A duplicated decomposed parent folds together with its whole facet constellation.**
Previously every row in a facet-decomposed store was either a facet or a decomposed
parent, so the tool cleaned nothing while printing "0 duplicate sets" — which reads as
"your store is clean" when it means "I cannot see your duplicates". Compound memories are
precisely what ADR-0049 targets and precisely the shape a verdict line takes. Deleting a
parent with its own atoms removes a self-contained copy and leaves the keeper's atoms
intact. Facet rows are never grouped in their own right, and the report says so in those
words rather than as a count of declined duplicates.

**Ghosts are not in the candidate set at all**, so `max(amplitude)` over a group can never
resurrect one.

The command is a dry run by default and prints what it would fold. The dry run takes **no
write lock**, so the safe informational mode stays available while the node is up.
`--apply` takes the lock and refuses if another writer holds it, refuses outright under
`KANNAKA_READONLY` (a read-only store drops the write on save, so the run would report
deletions that never happened), refuses to proceed without a retention-exempt pre-collapse
snapshot, and exits non-zero if any deletion failed so a script can see a partial run. The
snapshot is a **bundle**: the `.hrm` plus the sidecars, because the collapse's flush prunes
folded ids out of `.times_seen.json` and that sidecar's merge only ever raises a count, so
restoring the medium alone would leave an inflation nothing could later correct. Nothing
schedules it.

**A repeat cannot buy immunity from pruning.** `stage_prune` skips dampening entirely for an
*established* memory, and a verdict-typical 0.4 became 0.8 on ONE repeat.

This is not a switch to check before enabling. It is **on in production today**:
`/etc/systemd/system/kannaka-memory.service.d/belief.conf` on O1 sets `KANNAKA_BELIEF_PHASE=on`
alongside `KANNAKA_EXEMPLAR_COUPLING=on`, that unit's `KANNAKA_DATA_DIR` is `/home/opc/.kannaka`
— prime's own store, behind the public `ask_kannaka` — and its journal confirms it at runtime
(`[couple] always-on belief coupling ENABLED ... (needs belief on)`). debain1, debain2 and
docker1 have it unset; O1 has it set, and O1 is the one that matters. So without this fix a cron
job re-asserting one line made that memory immortal on prime on its first run: never dampened,
never ghosted, never compacted.

The rule that closes it is the same one the ShortTerm case above follows, now stated once:
**reinforcement moves a memory within its retention class and never across a retention
boundary.** Crossing the established line is the dream's decision, earned over nights of
corroboration, or the operator's through `kannaka boost` or `kannaka pin` — never a side effect
of the same sentence arriving again. A memory already above the line is unrestricted, because
it got there the hard way. An explicit `--importance` on a repeat cannot buy it either.

The threshold and the predicate now live in one place each, `ESTABLISHED_AMPLITUDE` and
`is_established_protected`, because `stage_prune` and the write path both have to agree about
them and a bare `0.5` in one of them is how they would drift apart.

A clamped repeat **says so**. `RememberOutcome.clamped_by_retention` and the dedupe report's
`held at the retention line` counter surface it, because a repeat that quietly declines to
strengthen looks like a write that failed. And the claim is checked against the real
`stage_prune` rather than against the predicate they share: a memory carried to the highest
amplitude repetition can reach is still dampened by a destructive pair, while a control that
earned its place above the line is skipped in the same run — so the test cannot pass by having
protection switched off.

**Peer re-sends no longer earn reputation.** Both swarm absorb sites now use
`remember_reporting`: a byte-identical re-send strengthens the memory but commits no
promotion and increments no absorb counter, because those are "a new contribution landed"
signals and paying them for repetition is a lever worth closing before it is used.

**It ships dark. `KANNAKA_REINFORCE_ON_REPEAT` defaults to OFF** and is opted into with
`1`/`true`/`on`/`yes`. Unset, `remember` inserts exactly as it always has, so rolling this
binary onto a node changes nothing about how that node writes. This is the same posture
ADR-0049's facet decomposition shipped under, and for the same reason: a change to the write
semantics of the substrate should be a decision someone makes, not something a deploy
acquires. Operator decision 2026-09-13.

The flag is read once at construction into `KannakaMemorySystem::reinforce_on_repeat`, and
`set_reinforce_on_repeat` turns it on for one system. Tests use the setter rather than the
variable, because `cargo test` runs threads in one process and a process-global switch is the
race `facet::lock_decompose_flag` exists to contain.

`KannakaMemorySystem::remember_forcing_new` remains the per-call opt-out for a caller that
genuinely needs one row per call, whatever the gate says.

### Fixed — serve: an anonymous ask can no longer choose what it costs (#932)

`swarm serve` answers `KANNAKA.ask.broadcast`, and the anonymous NATS identity may
publish there. Behind a 0.4 resonance gate the inbound text went straight to the
node's configured provider: no rate limit, no ceiling, and no distinction between
the operator's own ask and a stranger's broadcast. A node serving with a paid key
was a public endpoint for that key. The invariant this closes is not the ADR's
literal "pin served asks to local providers" — `kannaka-prime`, which *is* the
public `ask_kannaka` product, answers from a remote gateway on a virtual key
already capped at $25/30d, and pinning would take it off the air to fix an exposure
it does not have. What matters is the ceiling, not the locality:

> A served ask must never spend without a ceiling, and must never let the caller
> choose what it costs.

**The wire never chooses the route.** An inbound ask is answered with this node's
own `[llm]` provider and model. Any routing-shaped field on the envelope —
`provider`, `model`, `base_url`, `api_key`, `kind`, `route` — is ignored and named
in the log, so an ask labelling itself `kind = "reason"` to reach the operator's
expensive key gets exactly the provider every other ask gets. Nothing on the answer
path ever read those fields; the point is that a test now holds it there, because
the providers table (#931) adds a router in the same place.

**A per-requester rate limit, on by default.** 60 asks per requester per hour and
300 per hour in total, both settable with `KANNAKA_SERVE_ASKS_PER_HOUR` and
`KANNAKA_SERVE_ASKS_PER_HOUR_TOTAL`, both printed at startup. A refused ask gets a
short reply naming the limit rather than a timeout, and the refusal is logged once
per requester per window, not once per ask.

The two ceilings are metered in **different places, on purpose**. The per-requester
bucket is committed before the resonance probe, because that probe is a full recall
and is the cheap half of the abuse on a 1-vCPU hub — CPU the caller spends on
itself. The hourly total is committed past the resonance gate, immediately before
the model call, so it counts only asks that actually spend. Metering the total up
front turns the control into a cheaper outage than the problem: `swarm serve`
decides whether to answer a broadcast *after* the limiter runs, and anon may
publish there, so a stranger sending ~30-byte non-resonant asks — every one dropped
by that gate, none of them costing a token — would take the public `ask_kannaka`
off the air for everybody at one publish every twelve seconds.

A requester is keyed by the **pair** of reply-inbox prefix and declared `from`.
NATS attaches no publisher identity to a message even on an authenticated
connection, so **there is no unforgeable identity on this path** and both halves
are caller-chosen. Keying on `from` alone was not merely evadable, it was aimable:
three asks declaring `from = "kannaka-prime"` exhausted that peer's bucket, so any
caller could spend an honest neighbour's quota by claiming its name. The pair means
a caller can only exhaust the bucket it owns unless it also guesses the victim's
calling process. That is the most this layer can do, so the startup banner says the
rest plainly: the per-requester limit keeps honest neighbours from spending each
other's quota, and the hourly total is the ceiling that holds against a determined
caller. That key is also attacker-*sized*, since the
broker's `max_payload` is 64MB: an id over 128 bytes is stored as a hash of itself
(a hash, not a truncation, so one caller cannot land in another's bucket by sharing
a prefix), and `serve` refuses an oversized `from` outright before it costs a
recall. The limiter is therefore bounded in bytes, not just in entries — a cap on
the number of tracked requesters would have left ~85 asks able to retain ~5.4GB on
a box with 5.5GB.

**`hops`, ceiling 1 — a hired ask never hires.** The ask envelope gains `hops`; an
envelope without the field reads as 0, so an old client is unchanged. `serve` marks
the hop count of the ask it is answering, and the outbound publisher refuses to
forward one that has already been hired. A wire value is clamped to one above the
ceiling, and "this process is not serving anything" is an `Option`, never a
reserved number — otherwise a forged `hops` of `4294967295` would clamp onto that
reserved value and read back as "I originated this ask", handing the forger the
budget it was meant to exhaust. No serve path routes onward today, so the refusal
is dormant by construction: it is here so #931's router lands on a ceiling that is
already enforced instead of after one.

**Refuse to start unbounded — loudly, not fatally.** `serve` now classifies what it
can spend. A local brain or a keyless provider is free; a keyed provider with
`[llm] max_usd_per_day` or `[llm] externally_capped = true` is bounded. A keyed
provider with neither gets one of two notices, because they are not the same
situation: against the vendor's own API it is the plain exposure and draws the loud
banner; through a gateway the operator interposed it draws a single calm line
saying what is actually known — this node can spend, has declared no ceiling here,
and reaches its provider through that URL, so set `externally_capped` if the key is
capped upstream. A gateway is where budgets live, and a false alarm on the one node
everybody watches is how a banner stops being read. Either way `serve` still
starts; the hard refusal is opt-in with `KANNAKA_SERVE_REFUSE_UNBOUNDED=1`.

`max_usd_per_day` is **declared, not enforced** — enforcing it needs per-call cost
accounting, which is #931 — and the banner says so rather than letting a number in
a config file read as a ceiling. Because the installer writes `provider = "openai"`
for a local Ollama brain as well as for the hosted gateway, the base URL decides
locality before the provider string does.

**A reply only ever goes to an inbox.** `reply_to` comes off the wire, so every
reply in the serve handler could be aimed at a third party — or at an ordinary
subject. The sharp edge is privilege rather than volume: a serving node
authenticates with publish `>`, while anon is explicitly denied publish on
`KANNAKA.work.>`, `KANNAKA.inbox.>` and the JetStream admin subjects, so reflecting
through a serving node was a way to emit onto subjects the caller may not publish
to. `serve` now refuses any `reply_to` that is not an `_INBOX.` subject, above
every reply including the two that predate this work, so it closes the primitive
rather than only the refusal this change added.

Two diagnostics that an anonymous caller could trigger at will now log once per
process instead of once per ask: the malformed-reply-to line and the
"tried to steer the route" line, which also moved below the rate limit. Neither
was an injection vector, since `sanitize_display` strips control characters and
truncates, but both were journald volume on demand, and O1 has filled its disk with
syslog before.

This PR emits no `KANNAKA.events.*` at all, so the 48-character prompt preview the
activity publisher sends on an anon-readable subject is not extended to served asks.

### Fixed — GhostSignals: present the stored bearer, and tell the truth about a 409 (#930)

The hub now mints a per-row bearer and returns the plaintext token once
(kannaka-radio#304, deployed). The client presents the token already on file as
`Authorization: Bearer <token>` on the register POST, so a re-register ROTATES it
instead of colliding; a 200 carrying a different token is recognised as a rotation
and the new token replaces the old. Each outcome of that call is now distinct
rather than one error string: a first registration, a rotation, a 409 with no token
on file (the id is taken by someone else — register under a different `--agent-id`,
or have an operator clear the row with `POST /api/agents/:id/bearer/reset`), a 409
with a token on file (the stored token is stale, and it is KEPT), a 200 naming a
pre-existing row that holds no bearer (true of every row on the live hub today —
not a failure, and it changes neither the stored token nor `enabled`), and a
a 4xx the hub understood and rejected (a bad id, or the reserved `kax:` namespace),
and a transport failure. The retry hint follows the outcome: "re-run `kannaka init`"
is printed only where re-running can actually change the answer, never for an
existing row that only the hub oracle can give a bearer to, and never for a 4xx that
will be refused identically until the request itself changes.

Three rules hold across every outcome. **No path writes an empty token over a stored
one** — only the success arm assigns `ghostsignals.token` at all, and it trims what
it stores so a padded answer cannot become a malformed `Authorization` header.
**A failure the stored token had nothing to do with no longer disables a working
node**: an unreachable hub, a 5xx or a 4xx leaves `enabled` as it is when a token is
on file, since only a 409 proves the stored token is not the row's bearer. And **a
new token is written to disk the moment it arrives**, ahead of the HRM work and the
final save: the hub commits the row's new `bearer_hash` before it answers and never
shows the plaintext again, so a rotation whose save failed would have locked the
node out of its own row for good. If even that save fails, the token is printed
where an operator can copy it, alongside the oracle reset route.

`kannaka init` on an already-registered node offers rotation, defaulted to no and
taking only an explicit yes, and a rejected trade names the hub bearer alongside the
KAX identity token.

## [0.16.3] — 2026-09-11

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
