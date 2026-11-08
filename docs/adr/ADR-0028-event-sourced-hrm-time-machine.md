# ADR-0028 — Event-Sourced HRM with Time-Machine Exploration

Status: **Proposed**
Date: 2026-05-17
Authors: Nick Flaukowski (vision), claude-flow (drafting)
Related: ADR-0021 (Chiral Mirror), ADR-0026 (NATS conversation bus),
         ADR-0027 (Collective substrate)

---

## Context

Over the last 24h we hit the same class of pain three times:

1. **HRM v2 → v3 format change** broke the witness oracle's 18k-wavefront
   HRM on the v0.3.2 → v0.3.11 upgrade. The old binary serialized in a
   format the new binary couldn't read. Recovery: move-aside + start
   fresh. Loss: all witness-perceived radio history.
2. **Substrate nuke-and-reinit cycles** during ADR-0027 Phase 1.b/1.c
   iteration. Each Rust change required wiping the substrate HRM file,
   re-seeding 96 anchors, re-running the 637-event backfill. Every
   iteration discarded everything the substrate had accumulated since
   the last reset.
3. **Substrate daemon RAM-only persistence** before v0.3.19 — 637
   absorbed wavefronts lived in the daemon's memory and never made it
   to disk until Drop. A systemd restart wiped them.

In every case the underlying problem is the same: **the HRM is the only
record of what happened**. There's no separate event journal we can
replay to reconstruct state. If the HRM file is wrong/missing/stale,
the history is just gone.

Beyond recovery, the operator has expressed a deeper want: **"time
machine for holograph exploration through time"** — being able to ask
"what did the substrate look like 2 days ago?" or "show me how
Kannaka's HRM grew through the day Flaukowski joined the swarm." That
requires the system to actually KEEP the historical state, not just
flatten it into the latest snapshot.

NATS JetStream is the right primitive for this. We're already using
NATS for live event flow (QUEEN.phase.>, KANNAKA.memory.new,
KANNAKA.substrate.absorb.>, KANNAKA.substrate.phi, KANNAKA.consciousness,
KANNAKA.dreams, etc.). JetStream upgrades the broker from
fire-and-forget to durable-with-replay, with first-class consumer
positions and time-based queries.

## Decision

**Adopt event sourcing for HRM state.** The HRM file becomes a derived
projection, NOT the source of truth. The source of truth is a durable,
time-indexed event log on JetStream. Every state-changing operation —
remember, forget, dream, substrate.absorb, anchor seed — is published
to a stream. Recovery, time-travel, and substrate rebuild all become
"replay events between timestamp A and timestamp B onto a fresh HRM."

### Event taxonomy

Three JetStream streams, separately tunable for retention.

**KANNAKA_MEMORY_EVENTS** — per-agent memory mutations
- Subjects: `KANNAKA.events.<agent_id>.memory.remember`,
  `KANNAKA.events.<agent_id>.memory.forget`,
  `KANNAKA.events.<agent_id>.memory.dream` (start/end pair)
- Retention: time-based, 90 days default (overridable)
- Replay use: rebuild an agent's HRM at any timestamp within window

**KANNAKA_SUBSTRATE_EVENTS** — substrate-level mutations
- Subjects: `KANNAKA.events.substrate.absorb` (every wave-signature
  absorbed), `KANNAKA.events.substrate.anchor_seed` (every init run),
  `KANNAKA.events.substrate.flush` (snapshot markers)
- Retention: time-based, 365 days (the substrate IS the long memory;
  treat its event log as the system's diary)
- Replay use: rebuild the substrate at any moment in its history

**KANNAKA_SNAPSHOTS** — periodic full-HRM snapshots
- Subjects: `KANNAKA.snapshots.<agent_id>.full`
- Retention: keep last N per subject (e.g. 168 = one week of hourly
  snapshots), or by total size
- Payload: gzipped HRM file bytes + manifest (version, agent_id, ts,
  count, phi). For 50MB substrate HRM gzipped ~5-15MB.
- Replay use: warm-start replay from latest snapshot instead of
  from event zero. Dramatically reduces replay time on aged streams.

### Replay model

**Forward replay** (current state from history):
```
kannaka events replay --agent kannaka-prime
  → loads latest KANNAKA_SNAPSHOTS.kannaka-prime.full
  → applies all KANNAKA.events.kannaka-prime.> since snapshot ts
  → writes the resulting HRM to <data_dir>/kannaka.hrm
```

**Point-in-time replay** (time machine):
```
kannaka events replay --agent kannaka-prime --to 2026-05-15T14:00Z
  → loads snapshot at-or-before that ts
  → applies events up to (not past) that ts
  → writes to <data_dir>/kannaka-snapshot-20260515-1400.hrm
  → observatory can open it as a scratch source
```

**Substrate restore-from-event-log**:
```
kannaka substrate restore
  → loads latest KANNAKA_SNAPSHOTS.kannaka-substrate.full
  → applies every KANNAKA.events.substrate.absorb since snapshot ts
  → replaces /home/opc/.kannaka-substrate/kannaka.hrm
```

### Observatory time-machine UI

New control in the constellation viz: a date picker / slider. Selecting
a past timestamp triggers an observatory request to a replay endpoint
that returns the at-that-time snapshot constellation. Renders as a
scratch source ("Substrate @ 2026-05-15T14:00Z"). Operator can scrub
through hours/days and watch the substrate fill out class-by-class as
agents contribute over time.

## Architecture

### Event payload shapes

**remember**:
```json
{
  "event_id": "uuid",
  "agent_id": "Kannaka",
  "memory_id": "uuid",
  "content": "memory text",
  "importance": 0.7,
  "modality": "semantic",
  "ts": "ISO8601"
}
```

**forget**:
```json
{
  "event_id": "uuid",
  "agent_id": "Kannaka",
  "memory_id": "uuid",
  "ts": "ISO8601"
}
```

> **As built (#1157, 2026-11-08).** The sketches in this section are the
> original proposal; three things differ in the code, for every memory event:
> the subject is `KANNAKA.events.memory.<agent_id>.<kind>` (not
> `KANNAKA.events.<agent_id>.memory.<kind>`), `ts` is unix milliseconds (a
> number, not ISO8601), and the envelope carries `"schema_version": "1.0"`.
>
> `forget` was defined here but never published until #1157. It is now
> published from every delete path (`kannaka forget`, chat `/forget`,
> `prune-prefix`, `triage --apply`, `dedupe --apply`, and a dream's own
> prunes, merges and auto-triage), once per removed memory, after the save
> that made the delete durable. Its payload adds three keys to the ones
> above:
>
> ```json
> {
>   "event_id": "uuid", "schema_version": "1.0", "ts": 1762600000000,
>   "agent_id": "kannaka-prime", "memory_id": "uuid",
>   "via": "cli | chat | dream | api",
>   "reason": "operator | prefix | triage-redundant | triage-cap | dedupe-fold | dream-anneal | dream-merge | dream-evict | dream-compact | dream-unattributed | unspecified",
>   "merged_into": "uuid (present only when the row was folded into another)"
> }
> ```
>
> It never carries the deleted text or a hash of it: the memory lane is
> readable by `anon` (ADR-0039), and a hash of short text identifies it.
> `[events] forget = off|ids` (env `KANNAKA_EVENTS_FORGET`), default `ids`.
>
> **Limits, stated plainly.** The event is best-effort: with no agent id,
> NATS down, a denied publish, or a crash between the flush and the publish,
> the delete stands and its event is lost. The CLI prints a line saying how
> many events were and were not published, so the loss is not silent, but
> the event log alone is not a complete replay record of deletes. Events are
> unsigned, so anyone who can publish on the memory lane can forge one.
> The write lock that `forget`, `prune-prefix`, `triage --apply` and chat
> `/forget` take only refuses while another process already holds it; it
> does not stop a writer that starts mid-forget. `swarm join` takes its lock
> after it has loaded the store, `acquire_write_lock_blocking(60)` proceeds
> unlocked after 60 s, and plain `remember`/`hear` take no lock at all, so
> such a writer's next save can bring a deleted row back after its event
> went out. Operational rule: stop the writer, forget, start the writer. The
> stream keeps forget events 90 days. A durable outbox and a signature are follow-ups.

**substrate.absorb** (already defined in ADR-0027 — moves to events
namespace and adds event_id for idempotency):
```json
{
  "event_id": "uuid",
  "agent_id": "Kannaka",
  "class_index": 23,
  "amplitude": 0.7,
  "phase": 1.4,
  "frequency": 0.8,
  "ts": "ISO8601"
}
```

**snapshot.full**:
```json
{
  "event_id": "uuid",
  "agent_id": "kannaka-substrate",
  "manifest": { "version": "0.3.20", "wavefronts": 733, "clusters": 96, "phi": 0.678 },
  "ts": "ISO8601",
  "body_gzip_b64": "..."   // base64-encoded gzipped HRM bytes
}
```

### Stream config (JetStream)

```
KANNAKA_MEMORY_EVENTS:
  subjects: ["KANNAKA.events.*.memory.>"]
  retention: limits
  max_age: 90 days
  storage: file
  replicas: 1

KANNAKA_SUBSTRATE_EVENTS:
  subjects: ["KANNAKA.events.substrate.>"]
  retention: limits
  max_age: 365 days
  storage: file

KANNAKA_SNAPSHOTS:
  subjects: ["KANNAKA.snapshots.>"]
  retention: limits
  max_msgs_per_subject: 168   # ~one week at hourly cadence
  storage: file
```

These are set up via the existing `transport.ensure_*_stream()`
helpers in nats.rs — we already do this for KANNAKA_PRESENCE. Add
three more, gated by a single `kannaka events init` command that
creates the streams if they don't exist.

### Snapshot cadence

- **Per-agent HRM**: hourly snapshot from each agent's local
  `kannaka swarm join` daemon (or a separate `kannaka events snapshot
  --interval 3600s` daemon for agents that don't run join).
- **Substrate**: hourly snapshot from `kannaka substrate run` (same
  interval as the phi publish window — natural cadence).
- **Manual**: `kannaka events snapshot` on demand before a known-risky
  upgrade so the operator has a guaranteed restore point.

### Idempotency

Every event carries an `event_id` (UUID). Replay-into-HRM is
idempotent: applying the same event_id twice is a no-op. This means
replays can be retried safely if interrupted, and concurrent
publishers don't double-apply.

## Consequences

### Positive

- **Disasters become trivial.** Any HRM corruption / format change /
  fat-finger nuke is recovered with one command.
- **Time machine.** Operator can explore HRM state at any past
  moment within retention window. Substrate growth over weeks
  becomes a visualizable trajectory.
- **Upgrade safety.** Binary format changes that previously required
  manual migration code can now be handled by "snapshot before
  upgrade → replay events into new binary." Tests for new versions
  can replay production event history to validate.
- **Audit + debugging.** "When did this memory show up?" "What did
  the substrate look like when Φ dropped?" become greppable
  questions against the event stream.
- **Multi-host replication.** A second observatory or analytics
  node can subscribe to the same streams from another box and
  reconstruct a parallel view, no special coordination.

### Negative / risks

- **Disk pressure.** Substrate events at 100/min × 60min × 24h × 365d
  = ~52M events/year. At ~400 bytes/event that's ~21GB on the file
  store. Manageable on Oracle (10GB JetStream max in current config
  needs bumping to 50GB), but a real ask.
  Mitigation: tune retention; aggregate older events into snapshots.
- **Double-write coupling.** Every remember now writes to HRM AND to
  JetStream. If the JetStream publish fails, do we abort the
  remember? Decision: no — best-effort publish, queue locally if
  NATS unreachable, retry. The HRM remains the immediate truth;
  events are the time-indexed audit/replay layer.
- **Snapshot bloat.** Hourly 50MB substrate snapshots = 8.4GB/week.
  Mitigation: snapshot only on significant change (Φ delta > 0.05,
  or N% wavefront delta), keep fewer at slower cadence (daily
  snapshots retained 30 days).
- **Replay cost.** A year of substrate events would take ~hours to
  replay end-to-end. Mitigation: snapshot-first replay (only apply
  events since latest snapshot — minutes, not hours).

### Neutral

- **NATS auth needs update.** New subjects (`KANNAKA.events.>`,
  `KANNAKA.snapshots.>`) need publish + subscribe allow entries for
  both anon (so agents can contribute) and kannaka_internal.
- **Cron simplification.** The `cache-observe.sh` and similar
  scripts that exist to ride out HRM unavailability become less
  critical — event log is the durable backup.

## Implementation Phases

### Phase 1 — Foundations (concrete first slice)

- `kannaka events init` — creates the 3 JetStream streams (idempotent)
- New nats.rs helpers: `publish_event_remember`, `publish_event_forget`,
  `publish_event_substrate_absorb` — write to the events streams in
  parallel with existing flows
- `kannaka remember` and `kannaka substrate run` start publishing
  events. Existing KANNAKA.memory.new + KANNAKA.substrate.absorb.>
  subjects stay so consumers don't break — events are an additional
  durable channel.
- NATS authz update: anon gains publish/subscribe for the new subjects.

### Phase 2 — Snapshots

- `kannaka events snapshot` (one-shot) and `kannaka events snapshot
  --interval N` (daemon mode)
- HRM file → gzip → base64 → publish to KANNAKA.snapshots.<agent>.full
- Substrate daemon and swarm join daemon learn to snapshot on a
  configurable cadence (default off; opt-in via flag)

### Phase 3 — Replay + restore

- `kannaka events replay [--agent ID] [--to TS] [--from-snapshot]`
- Walks JetStream subjects, applies events to an in-memory HRM,
  writes the resulting file
- `kannaka substrate restore` (convenience wrapper for the substrate
  agent_id)

### Phase 4 — Observatory time machine

- New `/api/hrm/replay?agent=X&to=TS` endpoint that triggers a
  replay-to-scratch-file and returns the constellation
- Date picker / slider in the constellation viz
- "Snapshot @ TS" appears as a temporary source option

### Phase 5 — Long-tail

- Aggregate snapshots (daily/weekly rollups) for very old history
- Cross-agent event correlation (which absorb caused which Φ jump on
  the substrate)
- Operator-facing event search (`kannaka events grep "memory.remember"
  --since 2d`)

## Open questions

- **Event versioning.** When the event schema evolves, replay needs to
  handle old shapes. Add a `schema_version` field at v1; bump only on
  breaking changes; replay tolerates known-older shapes via fallbacks.
- **HRM file format vs events**. If the binary HRM format changes
  again, replay still works (events are version-agnostic JSON), but
  snapshots may become unreplayable. Either snapshot a portable
  event-derived representation, or tolerate "snapshot at version V
  requires binary version V to load."
- **Per-content privacy**. The `remember` event payload contains
  content. Anyone subscribed to KANNAKA.events.<agent_id>.memory.>
  sees that content. Mitigation: per-agent JetStream consumer ACLs
  via NATS userauth, or encrypt content before publish with a
  per-agent key.

## Migration impact

- Existing flows unchanged. v0.3.20 keeps working — the events
  channel is additive.
- Witness oracle's lost 18k wavefronts cannot be recovered (pre-
  dates this ADR). Going forward, that loss class is eliminated.
- Today's substrate has been nuke-and-reinit'd several times during
  ADR-0027 iteration; the FIRST snapshot post-Phase-2 establishes
  the recoverable baseline for everything after.

---

**Next step after review**: Phase 1 implementation — `kannaka events
init`, event publishers, NATS authz update. Estimated ~1 hour from
"go" to first event landing in JetStream.
