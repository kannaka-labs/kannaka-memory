# Kannaka NATS Identity & Synapse Map (ADR-0042)

The nervous-system wiring: which organ publishes/subscribes what, and (target
end-state) which account it lives in and which synapses (exports/imports) cross
between accounts. Phase 1a ships the identities in a flat `authorization` block;
1c splits them into accounts per the "Account" column.

## Identities (Phase 1b — COMPLETE 2026-07-19, all daemons migrated, 3-node cluster)

**Every constellation daemon now runs on its scoped identity — `kannaka_internal`
is retired from all daemons** (verified via `/proc/<pid>/environ`; zero publish
violations from any scoped user over a 40s+ census). `kannaka_internal` survives
only as (a) the CLI default (`.kannaka-nats.env` — recall/status/tail need broad
subscribe) and (b) the kv-bridge's `$KV.*` publisher.

Reflected in each node's live `/etc/nats/nats.conf` (literal passwords on the
boxes; `oracle-cluster.conf.template` in `ops/nats-cluster/` is the redacted shape).

| Identity | Daemon(s) | PUBLISH (scoped, as deployed) | Account (1c target) |
|---|---|---|---|
| `writer` | kannaka-memory single writer (`run-swarm.sh`) | `KANNAKA.>`, `QUEEN.>`, `queen.event.>`, `EYE.>`, `$JS.API.>`, `_INBOX.>` (the only memory/JS mutator) | INTERNAL |
| `serve` | swarm-serve, swarm-worker, inbox | `KANNAKA.recall.>`, `inbox.audit`, `inbox.reply.>`, `KANNAKA.skills.>`, `KANNAKA.events.memory.*.recall`, `_INBOX.>` | INTERNAL |
| `radio` | kannaka-radio | `RADIO.>`, `attention.ear`, `reactions`, `consciousness`, `$JS.API.STREAM.MSG.GET/INFO.>`, `_INBOX.>` | INTERNAL |
| `presence` | kannaka-presence (ADR-0013) | `KANNAKA.events.obc.>`, `presence.>`, `_INBOX.>` | INTERNAL |
| `responder` | kannaka-responder (ADR-0014) | `events.obc.responder_escalation`, `recall.>` (req), `_INBOX.>` | INTERNAL |
| `eye` | kannaka-eye | `attention.eye`, `attention.ear`, `EYE.>`, `exemplar.>`, `_INBOX.>` | INTERNAL |
| `attention` | kannaka-attention | `attention.beam`, `_INBOX.>` | INTERNAL |
| `ui_bridge` | kannaka-ui-bridge | `UI.>`, `hrm.>`, `js.>`, `ooda.>`, `radio.>`, `swarm.>`, `dream.>`, `$JS.API.>` (stream-admin denied), `_INBOX.>` | INTERNAL |
| `beacon` | kannaka-beacon (seeds) | `KANNAKA.events.beacon`, `_INBOX.>` | INTERNAL |
| `kannaktopus` | Kannaktopus daemons (off-box) | `QUEEN.phase.>`, `announce`, `queen.event.>`, `reactions`, `KANNAKTOPUS.>` | INTERNAL |
| `queen_agent` | owned swarm members off-box (`kannaka swarm join` on laptops: 0xSCADA-QE, Flaukowski, …) | open memory lane + `QUEEN.*`, `ask.>`, `inbox.>`, **`$KV.QUEEN_AGENTS.>`** (KV roster — the one write anon must never get), `$JS.API.STREAM.MSG.GET.>`; **denied** `work.>`/JS-admin/private lanes | INTERNAL |
| `anon` | open swarm peers, observatory reads, Command Center MCP | open memory lane ONLY (see below); **denied** `ask.>`/`work.>`/`inbox.>`/JS-admin | PUBLIC |

All identities `subscribe` broadly within their reach; the security invariant is
on **publish** (who can mutate what).

## The single-writer invariant (why this ADR exists)
Only `writer` may publish `KANNAKA.memory.>` / `snapshots.>` / `$JS.API.>`.
Every other organ is read-or-emit-only on its own lane. A buggy or hostile
reader physically cannot corrupt the shared HRM — [[oracle-hrm-single-writer]]
enforced at the transport, not by `KANNAKA_READONLY=1` convention.

## The open memory lane (preserved) vs the control lane (closed)
`anon` KEEPS publish on the memory-sharing lane (absorb-gated client-side, per
`nats-server.conf`): `events.>`, `activity.>`, `memory.new`, `consciousness`,
`substrate.*`, `presence.>`, `QUEEN.*`, `snapshots.>`.
`anon` LOSES publish on the control lane (the injection surface): `ask.>`,
`work.>`, `inbox.>`, `$JS.API.STREAM.CREATE/UPDATE/DELETE`, `$JS.API.CONSUMER.>`.

## Synapses (accounts split, Phase 1c)
When `PUBLIC` and `INTERNAL` become separate accounts, every currently-shared
subject needs a declared export/import (an isolated account sees nothing of
another by default):

- `INTERNAL exports` → `PUBLIC imports`: `KANNAKA.consciousness`,
  `KANNAKA.dreams`, `KANNAKA.exemplar.>`, `KANNAKA.cores.>`, `KANNAKA.snapshots.>`
  (the public-safe read stream — what open peers are allowed to observe).
- `PUBLIC exports` → `INTERNAL imports`: `KANNAKA.events.>`, `KANNAKA.memory.new`,
  `KANNAKA.substrate.absorb.>`, `KANNAKA.presence.>`, `QUEEN.phase.>`
  (the open memory-sharing lane, absorb-gated on ingest).
- Everything not exported is invisible across the synapse — the callosum made
  explicit. This map is the authority for that export/import block.

## Credential injection
Passwords are env placeholders in `nats-accounts.conf`
(`$NATS_WRITER_PASS`, `$NATS_SERVE_PASS`, …). Deploy injects them from
`/home/opc/.kannaka-nats.env` (extended with the per-organ vars). Each daemon's
unit/env sets `NATS_USER=<identity>` + its matching `NATS_PASSWORD`. Rotation =
change env + `nats-server --signal reload` (no JetStream drop).

`queen_agent` (`$NATS_QUEEN_AGENT_PASS`) is the off-box case: its credential
lives on each agent's machine, either as
`nats_url = "nats://queen_agent:<pass>@swarm.ninja-portal.com:4222"` in that
agent's `~/.kannaka/config.toml` or as `NATS_USER`/`NATS_PASSWORD` env (env
wins). One shared credential for all owned members; rotate it like the others.
