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

## Prerequisites

Two things must land before a consumer may act on a faculty. Neither is optional
and neither is inside this ADR.

1. **Per-agent NATS credentials (ADR-0060).** `KANNAKA.presence.<agent_id>` is
   writable by anyone who can reach the bus, and NATS attaches no publisher
   identity even on an authenticated connection. Until publish on that subject
   is restricted to the agent it names, a faculty is **other-assertable**: any
   node can write any faculty into any agent's record, and the record is a
   single last-writer-wins cell, so it can also erase one. Faculties may be
   *rendered* before this lands. They may not be *routed on*.

2. **The duplicate-identity work.** Three processes currently publish as
   `gossipghost-01` from three different stores. A faculty declared by processes
   that disagree about what they hold is worse than no faculty.

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

5. **Encoding is key-encoded and boolean-valued** — `"reads:kannaka-prime": true`,
   never `"reads": "kannaka-prime"`. `kannaka swarm peers` drops any capability
   whose value is not boolean `true`, so the string form would render as nothing
   at all. It also makes a bare `reads` structurally impossible to express, which
   is point 3 enforced by the wire format rather than by discipline.

6. **A faculty declares what would falsify it.** The first draft claimed "a wrong
   declaration is now a checkable claim". That is only true of `ask` and
   `serves_recall`, which have a subject a peer can probe; the other five have no
   probe and are unfalsifiable from outside. So each faculty carries its evidence
   kind — a probe subject where one exists, and an explicit `null` where none
   does. An unprobeable faculty is still worth declaring, but it must not be
   dressed as verifiable.

### Consumer rules

These are part of the decision, not implementation detail. A faculty is only as
safe as what a reader does with it.

- **Absent means UNKNOWN, never false.** Filter on a faculty only when at least
  one live peer declares it; keep undeclared peers in the fallback path. Reading
  absence as denial silently narrows a consumer that today asks everyone, which
  converts the safe default (point 2) into a failure.
- **A faculty may only NARROW the set of agents you would already have asked,
  never widen it.** Implemented as an intersection, never a lookup. This keeps a
  forged faculty from being able to *attract* work — the most it can do is make a
  node skip someone.
- **Faculties fail closed even though presence display fails open.** Honour a
  faculty only when the record's age parses AND is inside the freshness window.
- **A publisher states its cadence.** The witness — the flagship `hears` node —
  beacons at roughly the freshness window, so it would be invisible as a
  directory entry by construction. The payload carries the heartbeat interval and
  freshness is judged against that, not against a constant.

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

**A wrong declaration is a checkable claim where a probe exists, and an honest
one where it does not.** "This node says it answers asks" is testable against
`KANNAKA.ask.<id>`. `hears` is not testable from outside — the first draft
claimed otherwise and was wrong. What improves either way is that a constant
cannot be wrong because it never said anything, whereas a declaration can be
contradicted by its own operator, its logs, or a peer who tries.

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

## Rejected: attesting faculties from store history

A natural extension was proposed and attacked before any code was written: let a
faculty be *attested* by the agent's own store — "`hears` is backed by N audio
memories spanning M days" — so that wiping the store implicitly wipes the
faculty, and longevity is rewarded.

It does not survive. A four-lens adversarial review raised 38 findings, of which
33 survived an adversarial verification pass, and **every blocker landed on this
extension rather than on the declared half**. Recorded here so it is not
re-proposed from intuition:

- **The evidence does not distinguish the faculty from talk about it.** "N audio
  memories" is a keyword classification over *text*, so `kannaka-prime` already
  attests `hears` without ever having heard anything.
- **On a replica it launders another agent's corpus into the reader's name, with
  numbers attached.** Today's roster showing prime's `memory_count` under
  GossipGhost's name is a visible wrong number an operator can suspect.
  Attestation promotes it to a credential that carries corroboration.
- **The central claim is false for a node live in the swarm today.** That replica
  is re-synced every :12/:42 and its divergence is discarded, so the faculty is
  continuously re-attested from someone else's data and *can never decay*.
  "Wiping the store wipes the faculty" cannot hold where the operator cannot
  wipe the store.
- **It is deleted hourly on the only node it would be true for.** The witness
  caps `audio:` at 200 with a TTL, because that prune is what keeps it healthy.
  The retention policy destroys the evidence.
- **The longevity incentive points against the system's own health.** Φ on the
  witness rose from 0.26 to 0.50 *by deleting 97% of the store*. Rewarding
  accumulation fights the forgetting the substrate depends on.
- Snapshots, `.bak` files and `export`/`import` make the implicit revocation a
  30-second undo, and `import-json` sets both modality and `created_at` from an
  operator-chosen file, so the span is fabricable outright.

**What is kept from the idea.** Reading one's own store history is a good thing
to show *an operator who is deciding what to declare* — `kannaka init` and
`config set` may print "this store holds 41 days of hearing" locally. It stays
off the wire. Declaration is the switch; nothing is dressed as proof.

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

## Review

Attacked before implementation with the `adversarial-design-review` method
(4 lenses: security/capability, distributed state/protocol, attestation
integrity, CI/test integrity), each finding then passed to a separate agent
whose job was to refute it. 38 raised, 33 survived, 5 blockers — all on the
rejected extension above.

Guardrails the review asked not to be regressed, recorded so a later reviewer
does not "fix" them: declaration over detection; default OFF; naming the source
in `reads:<agent-id>`; refusing to grant authority alongside the declaration;
and the statement that this does not fix duplicate identity.

## References

- #835 — `ask` made operator-declared; the asymmetry this ADR generalises
- ADR-0008 (the eye), ADR-0030 (Kannaktopus), ADR-0042 Ph4 (redundant recall),
  ADR-0058 (Rogue Agent), ADR-0060 (per-agent identity)
