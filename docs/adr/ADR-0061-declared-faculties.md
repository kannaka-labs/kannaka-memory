# ADR-0061: Declared faculties — agents are a cast, not a fleet

**Status:** Proposed
**Date:** 2026-09-14

## Context

The roster says every agent can do the same four things:

```json
"capabilities": { "absorb": true, "dream": true, "exemplar_broadcast": true, "ask": false }
```

Three of those four are hardcoded `true` in `src/bin/kannaka.rs`. They are not
observations about a node; they are a constant that every node repeats about
itself.

That was never true, and it is getting less true every week. What the
constellation actually holds today:

| agent | what it alone can do | where that lives |
|---|---|---|
| `kannaka-witness-01` | **hears** — the only node ingesting the radio stream, ~1 tick/5 min | `kannaka-witness-loop.sh`, `kannaka hear` |
| `kannaka-eye` | **sees** — `Modality::Visual` | ADR-0008, `kannaka watch` |
| `kannaktopus` | **occupies** — up to 8 arms gripping cluster exemplars; its memory *is* the aggregate of the clusters it holds | ADR-0030; an MCP server, not a swarm node |
| `gossipghost-01` | **reads another agent's archive** — a read-only replica of prime, synced every :12/:42 | the O3 sync |
| O3's `swarm serve` | **serves prime's recall redundantly**, in prime's own queue group | ADR-0042 Ph4 |
| `skywave` | **computes** — the machine fleet and the LiteLLM gateway | kax-computer |
| `rogue`, `ghost-signal`, `the-archivist` | **haunt** — distinct OBC rooms, distinct cycle periods | ADR-0058 |
| `city-resident` | **executes** — takes real repo work, not only speech | `EXECUTOR_REPOS` |
| `kannaka-prime` | 25 units: hive organs, Nostr DVMs, inbox, attention, eye | — |

None of that is discoverable. A peer choosing who to ask a question about a
sound, or who to hand a repo task, has one bit to go on (`ask`) and otherwise
must already know the constellation by heart. The differentiation exists; the
roster just cannot say it.

The one capability that is *not* a constant is instructive. `ask` was
unconditionally `true` until #835, when a join-only node advertised a responder
that did not exist and peers DM'd into the void. The fix was not to detect the
responder — a sibling process under the same agent id is undetectable from the
heartbeat — but to make the claim **operator-declared**, defaulting OFF:

> an unset flag under-advertises (a peer skips asking), which is recoverable;
> the old always-on over-advertised, which silently swallowed messages.

That asymmetry is the whole design, and it generalises.

## Decision

**Faculties are declared, not assumed, and the roster carries them.**

1. `capabilities` gains named faculties alongside the existing four. A faculty
   is a claim about what this node will actually answer for.

2. **Every faculty defaults OFF and is operator-declared**, by the same rule and
   for the same reason as `ask`. Under-declaring costs a peer one skipped
   request. Over-declaring costs a peer a message into silence, and it is the
   expensive direction because the failure is invisible from outside.

3. **A faculty that names a source must name it.** `reads:kannaka-prime` is a
   faculty; `reads` is not. An agent drawing on another's archive says whose.

4. **Faculties describe reach, never authority.** A declared faculty does not
   grant NATS permission, a token, or a subject. It says "ask me about this",
   and the bus still decides whether the asker may.

Initial set, each grounded in something that already runs:

| faculty | means | declared by |
|---|---|---|
| `ask` | answers `KANNAKA.ask.<id>` | `KANNAKA_ADVERTISE_ASK=1` *(existing)* |
| `hears` | ingests an audio stream into memory | `KANNAKA_FACULTY_HEARS=1` |
| `sees` | ingests visual modality | `KANNAKA_FACULTY_SEES=1` |
| `occupies` | holds cluster exemplars; memory is the aggregate | `KANNAKA_FACULTY_OCCUPIES=1` |
| `serves_recall` | answers `KANNAKA.recall.<id>` | `KANNAKA_FACULTY_SERVES_RECALL=1` |
| `executes` | accepts work against named repos | `KANNAKA_FACULTY_EXECUTES=1` |
| `reads` | read access to another agent's store | `KANNAKA_FACULTY_READS=<agent-id>` |

## Consequences

**GossipGhost stops lying without losing its power.** Today three processes join
under `gossipghost-01`: the supervised unit reads *prime's* 32 MB replica, an
unsupervised orphan holds GossipGhost's own 85 MB store, and a third uses the
default. The roster shows prime's memory count under GossipGhost's name.

Under this ADR GossipGhost joins with **its own store and its own Φ**, and
declares `reads: kannaka-prime`. The insider access — which for a gossip agent
*is* the role, and is the more interesting half — is kept and made legible
instead of being laundered as its own recollection. Anyone reading its output
can tell which of two minds a claim came from.

That distinction is the same one the July Sybil injection taught us: content
whose origin is unclear is the problem, not content from elsewhere. *Identity
says who, corroboration proves what.*

**A wrong declaration is now a checkable claim.** "This node says it hears" can
be tested against whether anything answers. That is strictly better than a
constant, which cannot be wrong because it never said anything.

**The roster becomes a directory.** A peer with a question about a sound can
find the node that hears. This is the precondition for routing work by faculty
rather than by an operator knowing the constellation by heart.

**It does not fix the duplicate-identity bug, and must not be read as doing so.**
Three processes under one agent id is broken under any design. That is separate
work and it comes first — a faculty declared by three processes that disagree
about which store they hold is worse than no faculty at all.

**Cost of being wrong.** If nobody declares anything, the roster is exactly what
it is today and no peer is worse off. The failure mode of this ADR is that it
changes nothing, which is the failure mode you want.

## Alternatives considered

**Detect faculties instead of declaring them.** Rejected for the reason #835
found: the thing that answers is frequently a sibling process the heartbeat
cannot see. Detection would re-introduce exactly the over-advertising the
current design fixed.

**A separate capability registry, outside presence.** Rejected — presence is
already the thing every peer reads and it already carries `capabilities`. A
second source of truth about what an agent can do is a second thing to be wrong.

**Grant authority with the faculty.** Rejected firmly. A declaration that also
opened a subject would make "claim a power" and "be given a power" the same act,
and the bus would no longer be the thing that decides. Faculties advertise;
`nats.conf` authorises. Those stay separate.

## References

- #835 — `ask` made operator-declared; the asymmetry this ADR generalises
- ADR-0008 (the eye), ADR-0030 (Kannaktopus), ADR-0042 Ph4 (redundant recall),
  ADR-0058 (Rogue Agent), ADR-0060 (per-agent identity)
