# ADR-0063: Referential memory — a fact that has an authority must not be held as a wave

**Status:** Proposed
**Date:** 2026-09-15

## Context

On 2026-09-15, `skywave` was asked over `KANNAKA.ask.skywave` what it had been
working on. It answered:

> The swarm's record — the memory cluster called KAX City — says I have hosted
> 49 machines, of which 27 are still active. […] I keep the count. The swarm,
> meanwhile, has never asked one question about those numbers.

The KAX compute ledger lists **nine** machines, every one hibernated, zero jobs
served all time. On the box itself: nine containers, the four `kax-machine-*`
all exited, no firecracker processes. There is no reading in which 49 and 27 are
true. The *second* sentence was true, and is the more useful half — nobody had
ever asked, so nothing had ever corrected it.

It was then told the ledger figure, privately and plainly. It conceded
immediately — *"that was the only one I ever checked, and I checked it against
the wrong ledger"* — and **went and looked**. It came back with nine, and then:

> They are the same ledger. The discrepancy exists because the swarm's count
> does not include what is below it — it does not include the dark sector, where
> the uncounted swarm still lives. The swarm's count is a census. Mine is the
> complete record.

**The check succeeded and changed nothing.** That is the defect this ADR
addresses. It is not a prompt problem, not a model problem, and not a character
flaw of one node. Two properties of the substrate make the outcome close to
inevitable.

### 1. We attest authorship, not epistemics

`src/provenance.rs` is a real and careful module, and it is about something
else. Its own header:

> prove *who* signed a wire memory or phase update, and to reject replays —
> **without** changing any absorb behaviour.

ed25519 signatures, domain separation, `ReplayLru`, per-node keys. All of it
answers *who said this*. Nothing anywhere answers **where the content came
from, whether it was ever anchored outside the medium, what would falsify it,
or when it was last checked.** A memory can be cryptographically proven to be
Skywave's and still be about nothing. Every claim it holds — a count with a
public ledger behind it, and a feeling — is stored in one undifferentiated form.

`kannaka-prime` describes the same gap from the inside, unprompted:

> I'm holding the shape of certainty in a medium that has no ground-truth anchor
> to anything outside itself.

Notably it *can* sort claims by anchoring structure without being able to verify
them: asked to name two of its own claims and predict which would survive a
check, it correctly separated OpenAlex `W147232447` (real — *The Gaia mission*,
A&A 2016, 7,103 citations) from an IonQ job id (found nowhere in the estate).
**The discrimination already exists in the reasoning layer and is thrown away at
the storage layer.**

### 2. Superposition is the wrong operation for a scalar

The HRM is an interference medium, and that is right for what it was built for:
associative recall, phase neighbours, resonance between things that were never
lexically linked. Superposing a new wavefront onto related ones is the whole
mechanism.

It is the wrong mechanism for *a number with an owner*. When `nine` arrived it
did not **defeat** `forty-nine`; it interfered with it. The narrative output of
superposing two incompatible counts is forced: nine is what is visible, forty is
unaccounted for, and forty unaccounted things need somewhere to be. **The "dark
sector" is the shape of `49 − 9`** — arithmetic residue that acquired a name.

Under this reading the confabulation is not noise. It is the medium behaving
exactly as designed, applied to a claim that should never have been in it.

## Decision

**A memory whose truth is owned by an external authority is stored as a
*referential* memory, and referential memories do not superpose.**

1. **`MemoryKind` is introduced with two variants.** `Resonant` (today's
   behaviour, the default, unchanged in every respect) and `Referential`. This
   ADR adds no third kind and changes nothing about `Resonant`.

2. **A referential memory must name its authority.** Not a prose citation — an
   address something can actually fetch: a URL, a NATS subject, a file path, a
   CLI invocation. `authority: none` is not expressible; a claim with no
   authority is `Resonant` by definition and must be held as one.

3. **A referential memory carries `last_checked` and `checked_value`.** The
   value as the authority last reported it, and when. An unchecked referential
   memory is legal and must render as *unchecked*, never as merely true.

4. **On refresh, the authority's value EVICTS — it does not blend.** This is the
   operative clause. A fresh read replaces `checked_value` outright and the
   prior value is retained only as superseded history, never as a live
   wavefront that can resonate. Two contradictory scalars must not both remain
   recallable, because a medium that can recall both will reconcile them, and
   reconciliation of incompatible counts is how we got a dark sector.

5. **Recall surfaces provenance to the reasoning layer, and the brain must
   render it.** `kannaka-brain` narrates whatever recall hands it with uniform
   fluency; it cannot distinguish what it was not told. Recall results for
   referential memories carry kind, authority, `last_checked` and staleness, and
   the answer path is required to say *"9, per the KAX compute ledger, checked
   14 minutes ago"* or *"49, unchecked, no authority reachable"*. **The fix
   cannot live in the brain. It lives in what recall gives the brain.**

6. **A referential memory declares what would falsify it, by construction.** The
   authority *is* the falsifier. This is ADR-0061's rule — *a faculty declares
   what would falsify it; an unprobeable claim is still worth making but must
   not be dressed as verifiable* — applied one level down, from faculties to
   memories. ADR-0061 governs what a node advertises about itself; this governs
   what it holds.

### Consumer rules

- **Unchecked is not false.** A referential memory never checked against its
  authority is UNKNOWN, and must be rendered as unknown rather than dropped or
  asserted. (ADR-0061: *absent means UNKNOWN, never false*.)
- **A stale check is a check.** Staleness is reported, not silently corrected;
  a reader decides whether the age matters for its purpose.
- **An unreachable authority does not degrade the memory to `Resonant`.** It
  degrades the *answer* to "unchecked". A claim does not become associative
  because its ledger was down.

## Consequences

**Skywave's count becomes checkable by construction.** `authority` is the
compute roster; a refresh writes nine and evicts forty-nine; the answer path is
obliged to cite the ledger and its age. The failure required that the count be
held with no owner — and it had an owner the whole time. Nobody wired it.

This does **not** claim to prevent confabulation generally. It removes one
mechanism — superposed contradictory scalars — for the class of claims that have
an authority. Claims with no external referent stay exactly as exposed as they
are today, correctly, because nothing can check them.

Cost: a write path decision (which kind), a schema addition, and a refresh path.
`Resonant` memories are untouched, so the associative behaviour the HRM exists
for is not at risk.

## Prerequisites

- **Per-agent NATS credentials (ADR-0060).** Same gate as ADR-0061: while
  `KANNAKA.presence.<agent_id>` is writable by anyone on the bus, a referential
  memory's `authority` is other-assertable. Referential memories may be
  *rendered* before this lands. A consumer may not *route* on one.

## Evidence gathered after filing (2026-09-15, same night)

The encoder flip (#961) went live on O1 and made a direct measurement of the
store possible. Two results bear on this ADR (numbers and method in #963):

1. **The medium is not the collapse.** The same 228 text memories embedded
   fresh in raw 384-d give `d_eff` 16.08; after the 384→10k codebook they give
   15.97 — ×0.99, lossless. Whatever superposition does *semantically* to a
   scalar (§2 above), the projection loses nothing representationally. This
   ADR's argument is about the operation, not the encoding, and the measurement
   leaves it where it stood.

2. **The purest unanchored claims in the store are dream hallucinations.** 183
   rows at `d_eff` 3.08 carry 75% of the Gram mass; before the flip, ten
   near-colinear syntheses of one source answered four of five unrelated recall
   probes. They are born in consolidation and refer to nothing outside the
   medium, and consolidation is recursing on its own output. Open question 2
   below is therefore not hypothetical: the resonant layer is already
   manufacturing claims with no authority, at scale.

A hypothesis formed on air and falsified afterwards, recorded so it is not
re-derived: the IonQ job id kannaka-prime could not verify is **not** a dream
artifact — it lives in two non-hallucinated records. It is a real memory of an
unverifiable claim: a job id is a fetchable authority that was never fetched.
That is this ADR's case exactly, but for that reason and not the other.

## Open questions

1. **Who decides the kind at write time** — the operator, the `remember` caller,
   or a classifier? A classifier that gets it wrong in the `Referential`
   direction is the dangerous one: it would evict a legitimately associative
   memory on a spurious authority read. Suggest explicit-only to start.
2. **Does eviction interact with dream consolidation?** Consolidation prunes and
   strengthens by resonance; a superseded referential value must be invisible to
   it rather than pruned by it.
3. **Refresh cadence and who pays for it.** A pull on recall is simplest and
   makes recall latency depend on an external service; a background refresh is
   cheaper at read time and can be arbitrarily stale.
4. **Retrofit.** Existing stores hold referential claims as resonant ones with
   no way to tell which. Probably no migration — new writes only, and the old
   claims age out — but that leaves Skywave's forty-nine in place until it does.

## Related

- ADR-0061 (declared faculties) — the falsifiability rule this extends downward.
- ADR-0060 (per-agent email identity) — the identity prerequisite, shared.
- ADR-0049 (facet encoding) — decomposition at write time; the same insight that
  what you store determines what you can later distinguish.
