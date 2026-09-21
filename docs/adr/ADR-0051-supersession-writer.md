# ADR-0051: The Supersession Writer — Recording That One Fact Replaced Another

**Status:** Proposed — **v2, revised after adversarial-design-review**
(GO_WITH_CHANGES, 2026-07-26, `wf_229ce7ea-f88`: 5 lenses + 2 skeptics + synthesis;
58 raw findings → 19 distinct defects). The staged shape survived every lens.
**Four factual claims v1 made about the code were refuted and are corrected below.**
**Relates to:** ADR-0050 (temporal/confirmation-weighted recall — this ADR unblocks
its inert half), ADR-0049 (facet encoding — the dependency, and the source of this
ADR's biggest structural blocker), ADR-0035 Cap 8 (the fields being written),
ADR-0036 (dream consolidation — the conflict, worse than v1 stated),
`sensemaking::detect_contradictions` (measured and rejected for this job).
**Blocked on:** issue #630 (`HrmStore::insert` loses memories on chiral stores) for
any import/wire-sync path.

## Context

ADR-0050 shipped temporal ranking and closed the ranking question: a superseded fact
can be demoted without being evicted, measured, off by default. It also moved the
blocker rather than removing it. Outside tests, the only writer of
`effective_at` / `observed_at` / `expires_at` is `HrmStore::set_temporal`
(`hrm_store.rs:1873`), reachable from one place —
`kannaka remember --effective/--observed/--expires` (`kannaka.rs:1557`). On the live
HRM `expires_at` is `None` everywhere, so **the supersession half of ADR-0050 is
completely inert.** Something has to write supersession down. That is this ADR.

## The obvious reuse does not work, and we measured it

`sensemaking::detect_contradictions` (Cap 10, used by `immune::classify_batch`) finds
"same subject, opposed stance": high similarity AND **phase gap near π**. It cannot do
this job. `content_born_phase` is deliberately content-**smooth** — its doc comment
says it maps "identical content to identical phase" — and a superseded fact differs
from its replacement by a *single value token*. **The pair that most needs detecting
is the pair that looks most alike.**

Probe (`chiral::tests::supersession_pairs_are_not_phase_opposed`, kept as a permanent
regression pin):

| pair | phase gap |
|---|---|
| `"...channel is twelve"` vs `"...channel is twentyseven"` (supersession) | **0.6341 rad** |
| `"...channel is twelve"` vs `"mycelium spreads beneath the forest floor"` (unrelated) | **0.5415 rad** |
| opposed threshold (`π/2`) | 1.5708 rad |

Supersession is nowhere near opposed, and **phase does not even order these
correctly** — the supersession pair scores a *larger* gap than unrelated content.
Non-monotonic, so no threshold on phase gap can work.

The review sharpened this and corrected one popular derivation. With belief phase OFF
(the default, `chiral.rs:35-39`) every stored phase is literally `0.0`, so
`detect_contradictions`' gap is 0 for *every* pair — the conclusion is stronger than
the phase-gap argument, and should be stated that way. **v2 correction:** the probe
test as committed asserts only `supersession < π/2`; it *prints* the unrelated gap
without asserting it, so the non-monotonicity this ADR calls the stronger finding is
narrated rather than guarded. It must gain
`assert!(supersession >= unrelated)`, plus a case going through `ChiralMedium::store`
with `KANNAKA_BELIEF_PHASE=1`, plus one re-read after `dream` (which rephases with the
uncentered function, `chiral.rs:1177-1185`).

This ADR does **not** retire `detect_contradictions` — it rejects reuse for this job.

## Decision

A **staged writer**, each stage earning the next by measurement. Detection rides on
facets, not phase, and not on whole compound memories.

### Stage 0 — explicit declaration. No judgement, ships first.

`kannaka remember "<text>" --supersedes <id>`, **node-local**.

**The stamp is derived, never read from the clock:**
`expires_at = replacement.observed_at.unwrap_or(replacement.created_at)`.

v1 said `expires_at = now`, which was wrong three ways at once. Backfilling three known
versions in one session would stamp v1 and v2 milliseconds apart, destroying the fact
that v1 died years before v2; re-running a declaration would move the expiry forward,
since `set_temporal` overwrites unconditionally with no journal; and a Stage-2 writer
calling `Utc::now()` inside dream would persist a wall clock into `WavefrontMeta`,
breaking the #521 byte-identical-dream contract. The L8 fixture already had the right
semantics and this ADR should have copied them: version *v* expires exactly when
version *v+1* was observed — **that is what supersession is.**

Skip any target already expired at or before that value (idempotent re-runs). Support
`--observed <iso>` alongside `--supersedes` so backfill can state the real instant.

**~~Usable today by the swarm and nostr membrane.~~ REFUTED — Stage 0 is node-local.**
`handlers/swarm.rs:1025` absorbs peer content via `remember_with_category`, minting a
**fresh local uuid** and discarding the peer's id, while the actual cross-node join key
is `blake3(normalize(content))` (`absorb_gate.rs:188-193`). There is no wire path for a
post-hoc metadata update — `KANNAKA.memory.new` is emitted once at creation — and
`swarm sync` is Kuramoto phase reconciliation only, so two nodes that disagree never
converge. Any swarm-replicated supersession is a **blocking prerequisite**, not a
consequence: it would need a content-hash-keyed, signed subject routed through
`admit()`.

### Stage 1 — propose, do not apply.

A detector emits supersession *candidates* and writes nothing. Candidate rule, revised:

> two comparable units whose **subject+attribute** match but whose **value** differs,
> where the newer unit's `observed_at.unwrap_or(created_at)` is strictly later, the
> normalized value spans are **mutually exclusive** (neither substring nor superset of
> the other), and their `effective_at` intervals are **not disjoint**.

Four amendments over v1, each from a refutation:

- **`created_at` fallback in the rule text, not the implementation.** v1 required
  "the newer facet's `observed_at` is later", but `observed_at` is `None` across the
  entire live corpus and nothing ingested over the swarm ever acquires one. `None` vs
  `None` — neither is later — so the v1 rule emits **zero candidates in production**
  while the mandated fully-stamped fixture measures a state production never reaches.
  This is the ADR-0047 inertness shape for the third time. The ranking side already
  solved it: `hemisphere.rs:119` is `observed_at.unwrap_or(created_at)`.
- **Equal timestamps emit nothing.** Never guess direction.
- **Disjoint `effective_at` intervals are a timeline, not a supersession.**
- **Mutual exclusivity** stops terse-stored-later from expiring a more informative
  earlier elaboration.

**Comparable unit = facet *or* whole atomic memory.** v1 keyed Stage 1 solely on
facets, which the review showed is unbuildable as written: ADR-0049 decomposes only
compound, ≥2-clause lived content, but the canonical supersession shape — and every
family in the L8 corpus — is **single-clause**, so it never becomes a facet at all.
Facets remain the right unit for compound content and the hard dependency stands for
that case; they are an additional source of comparable units, not the only one.

**No (subject, attribute, value) structure exists yet.** ADR-0049 commits only to
`parent_id` / `is_facet` / `decomposed` and produces *text*; similarity is whole-vector
cosine over a whitespace bag-of-words encoder, and no API scores a sub-span. An
implementer handed the v1 rule has nothing to key on and will silently fall back to
whole-facet cosine. Before Stage 1 is designed, ADR-0049 must either emit persisted
normalized S/A/V slots (three more trailing fields, deterministic extractor, own
fallback struct) **or** Stage 1 must be restated as a lexical operation over normalized
strings with resonance demoted to a pre-filter. ADR-0049's facet-quality gate must also
be amended to **retain** short attribute-value assertions rather than drop them as
low-word-count, as a named test case.

### Stage 2 — auto-apply behind an off-by-default flag, only if Stage 1 measures well.

Byte-identical default, pre-registered gates, same discipline as ADR-0048/0050.
Gate on **cardinality**: maintain a per-(subject, attribute) observed-value set and
refuse to auto-apply for any key ever seen with ≥2 concurrently-current values; an
unseen key defaults to "not known to be single-valued" → propose only.

## The constellation is the unit of temporal truth

**The single biggest structural finding, and it inverts v1's "Stage 0 ships safely
now".** Once ADR-0049 lands, the stamp and the reader sit on opposite records:

- `temporal_weight` is evaluated on `self.metadata[i]` — the **scanned** row, i.e. the
  facet (`hemisphere.rs:402`). A **parent** stamp is invisible to ranking.
- `swarm brief` does `store.get(&r.id)` on the already-resolved id, i.e. the **parent**
  (`kannaka.rs:3633-3637`). A **facet** stamp is invisible to the brief.
- `set_temporal` keys on exactly one id with no facet walk.

Every stamp reaches exactly one of the two consumers, and the wrong one in each
direction. **Stage 0 goes inert the day facets ship.**

Therefore: `set_temporal` resolves a facet up to its parent and **fans the write to the
parent and every child sharing that `parent_id`**, through one shared helper so no
caller can stamp half a constellation. `resolve_facets` carries the most-restrictive
temporal spec of parent+facet onto the resolved result. Facets inherit the parent's
`created_at`/`observed_at`/`effective_at` verbatim — never the backfill instant — with
`facet.created_at == parent.created_at` pinned in the round-trip fixture.

## Prerequisites — these land before the first stamp exists

**P1. A stamp must be clearable. It currently is not.**
v1 claimed `set_temporal` can clear a stamp and called that "the safety margin that
makes Stage 2 thinkable at all". **False.** `hrm_store.rs:1885-1888` is
`if exp.is_some() { meta.expires_at = exp; }` — `None` means *leave untouched*,
contractually, and the same guard repeats at all four write surfaces. The sole call
site is itself wrapped in an `is_some()` check, `parse_ts` exits(2) on anything not
RFC3339 so no clear sentinel is expressible, and there is no `kannaka temporal` verb,
so `set_temporal` is unreachable for any pre-existing id. Required: a tri-state
signature (outer `None` = leave, `Some(None)` = clear) or a sibling `clear_temporal`,
keeping the current signature as a wrapper; plus a `kannaka temporal <id>` verb.
Test: stamp → save → reload → clear → save → reload → `expires_at == None`.

**P2. The write needs a commit point and mutual exclusion.**
The new memory is flushed to disk *before* `set_temporal` runs, and `set_temporal` only
`mark_dirty()`s; the id is printed and blocking NATS work happens before `impl Drop`
saves. A kill in that window leaves the new fact live, the old un-expired, exit 0.
Separately, the `remember` arm takes **no write lock** while every other writer does,
and `swarm join` holds a RAM snapshot it never re-reads and flushes every 30s — which
rewrites the whole file from stale state. The `flock` is `#[cfg(unix)]`, so **on the
Windows seed box there is no lock at all.** Required: resolve and stamp before the
print and before any network I/O; one dirty window so a single atomic
`save_medium` rename commits the whole supersession; take the write lock in the
remember arm and hard-fail when the daemon holds it; make `swarm join` reload on mtime
change or route `--supersedes` as a request the daemon consumes.

**P3. Stage 0 fails silently in four ways.** `set_temporal` returns `bool` and the call
site discards it; a non-HRM backend drops the flags with no message; `warn_if_readonly`
prints to stderr and still exits 0; and a cache-only match reports `found = true`
although `sync_cache_to_medium` writes back energy only. Required: return an enum
(`AuthoritativeWrite` / `CacheOnly` / `NotFound`); `--supersedes` exits 2 on anything
else; reject self-supersession; hard-fail under `KANNAKA_READONLY=1`.

**P4. `swarm brief` hard-drops expired memories, on the default config.**
`kannaka.rs:3630-3639` filters on `is_current` before building consensus, with no env
gate — while `KANNAKA_RECALL_TEMPORAL_EXP` defaults to 0.0 and skips the temporal
factor entirely. **Net on a stock install: `--supersedes X` produces zero ranking
demotion and total exclusion of X from the brief.** The inverse of what ADR-0050
designed. Convert the filter to a demotion (keep the item, scale confidence by the
floor, tag `"current": false` in `--json`) before turning ADR-0050 on anywhere.

## The dream interaction — worse than v1 said, and v1 described it wrongly

v1 said merge would blur two versions into "a memory that asserts both values weakly".
**That is factually wrong.** ADR-0036's `vec_rep = normalize(Σ vec_i)` was never
implemented. `apply_consolidation` picks a carrier by
`argmax energy * exp(-0.001 * age_days)`, mutates only energy and tier, and
**hard-removes every non-carrier**. Nothing temporal is read anywhere in the pass and
`usable[]` exempts only `Tier::Pinned`. So an old, high-energy, *expired* fact can win
the carrier vote, keep its `expires_at`, and the current replacement is **deleted
outright**. A blur would at least read as low confidence; a deletion does not.

Preconditions are stiffer than four lenses assumed — `ConsolidateOpts::default().mode`
is `DryRun`, production is force-downgraded to dry-run under belief unless
`KANNAKA_MERGE_UNDER_BELIEF=1`, and the cosine ≥ 0.92 gate is rarely met by short
facts under the BoW encoder — so this is a major, not a blocker. Fix regardless: add
the temporal triple to `Snap`, split any group whose members disagree on
`expires_at.is_some()`, and make carrier selection lexicographic on
`(is_current, eff_strength)`.

**The eviction paths are blind too.** `lowest_value_overflow_ids` filters only on
`Tier::Pinned` and `triage_select` retains the *highest-amplitude* member at cosine ≥
0.95 — which is the old accessed fact, not the current one. Give both the exemption
Pinned has, skip the redundancy check when exactly one member is expired, and prefer
`(is_current, amplitude)` for retention, with a bounded
`KANNAKA_EXPIRED_RETENTION_DAYS` escape hatch.

## Measurement (L9), pre-registered and frozen before any detector code

- **G1 precision** on **ordered** candidates `(expired_id, replacement_id)` — v1 scored
  unordered pairs, so a detector that always expires the wrong member would post 1.0
  on every gate and Stage 2 would then expire every *current* fact in the corpus. A
  true positive requires pair **and** direction. Add a **direction-accuracy gate at
  exactly 1.0**: an inverted call is strictly worse than no call.
- **G2 recall**, weighted below precision — but a **hard floor**, stated as a number,
  not merely deweighted.
- **G3 negative controls, each scored separately** (pooling lets easy controls mask
  hard ones): multi-valued attributes (the L8 vocabulary already contains one —
  `["apiary","hive","frames"]`), scope-disjoint facts, numeric/unit/date format
  variants, disjoint `effective_at` intervals, reversed elaboration, and negation
  (which shares the value token, so a value-difference test never fires — a separate
  candidate reason with independently measured precision).
- **G4 zero candidates on a clean corpus** — adversarial, not benign: shared subjects,
  shared attributes, shared value tokens, function-word-only differences.
- **G5 liveness — new, hard PASS/FAIL outside fitness, per seed.** v1 had no liveness
  metric, and a detector at threshold 1.0 passes G1 (0 FP / 0 calls), G3 and G4 while
  posting the **best printable fitness** — three of four gates green for a dead
  detector, with the gradient pointing at threshold → 1.0. Every one of the ≥10 seeds
  must yield ≥1 true positive; not an OR across runs, which is how L8's own liveness
  check was satisfiable by a single candidate in ten corpora.
- **G6 constellation survival** — stamp the fixture, run a full `dream` with
  `KANNAKA_CONSOLIDATE=on`, `KANNAKA_MERGE_UNDER_BELIEF=1` and `KANNAKA_MAX_MEMORIES`
  forcing the size cap, and assert every superseded id still resolves to its original
  content, at 1.0. **Assert inside the arm that the merge actually applied** — else the
  dry-run downgrade makes dream a no-op and the arm passes for nothing.

**Three arms the harness asserts must come out NOT_SUPPORTED**, failing the run if they
pass: threshold = 1.0; a **timestamp-shuffled control** (identical corpus, `observed_at`
permuted — if precision does not collapse to chance, the detector is reading text
overlap, not time); and the **unstamped arm** (every `observed_at = None`, ordering from
`created_at` only) which is the actual live-corpus condition.

**Statistics.** Gate G1 on **MIN over seeds** plus an absolute zero-false-positive
count, not the mean: with ~240 ordered true pairs, mean precision 0.97 is ~7 wrongly
expired true memories, and one catastrophic seed hides behind nine clean ones. Emit one
TSV row per seed. Adopt L8's `0.5 = no evidence` convention so 0/0 precision fails
rather than scores. Note honestly that L8's "10 seeds" are rotations of a 16-template
table sharing 11 of 12 families between consecutive seeds — grow the tables or say so.

**Fixture hygiene.** Stamp the whole fixture — L8's first run failed every gate because
unstamped distractors were silently dated to `now` — but **pair it with the unstamped
arm above**, or it hides the inertness rather than exposing it. Stamp **by returned
Uuid, never by content match** (L8 keys on `m.content == text`, and nothing dedups
identical content), with a pre-gate self-check that aborts if versions are
indistinguishable. Keep failed-fixture rows in `results-L9.tsv` the way `results-L8.tsv`
kept v1-unaged vs v2-aged; the delta is what makes the lesson legible. Define truth on
the **transitive reduction** — a true positive is `(older, nearest-newer)` only, and
Stage 2 writes `expires_at = min(observed_at over all newer candidates)` so the value is
order-independent. Include a 4-version family so this is exercised at all.

## Consequences and remaining risks

**Wrongly expiring a true memory is currently unrecoverable** (P1), **invisible** on the
default ranking config and **fatal on the brief path** (P4), and can be made
**permanent by deletion** (dream merge, eviction). That is why precision dominates
recall and why Stage 2 is the last thing built, not the first.

**Not addressed here:** auto-refreshing `observed_at` on non-contradicting
re-assertion — the "confirmation" half of the original question, cheap and probably
worth doing before any of this; graded/partial disagreement; and swarm-replicated
supersession, now an explicit blocking prerequisite rather than a footnote.

**Also folded in wherever convenient:** wire sanitization must extend `CleanFields` and
add the temporal triple to `canonical_mem` behind a **new domain tag** (old signatures
must fail, not silently verify a zeroed triple) — and must land *together with* any
change that lets `insert` carry temporal fields (#630), never after, or a peer gains
ungated control of `expires_at` on our store. The stale
"MUST stay the LAST serialized fields" comment on `WavefrontMeta` is now wrong —
`provenance` is the actual tail — and should become one ordered append-only invariant
list before two ADRs edit that struct.

## Revised build order

0. **ADR text** — this document (done).
1. **Enabling PR:** P1 + P2 + P3 together. One coherent change to the write path;
   splitting it ships a half-safe writer.
2. **Stage 0 PR:** `--supersedes` with the derived stamp, plus its `cargo test` gates.
3. **Parallel, independent:** issue #630 and the export/import temporal round trip.
4. **Before `KANNAKA_RECALL_TEMPORAL_EXP` is enabled anywhere:** P4, the dedup fix
   (compute dedup at `temporal_exp = 0.0` so demotion can never widen the swarm
   re-absorption window), and the eviction exemptions.
5. **Before Stage 2 is thinkable:** the dream temporal exclusion + G6.
6. **ADR-0049 amendments:** constellation-as-unit, S/A/V slots or the lexical
   restatement, retain short assertions.
7. **L9 pre-registration, frozen**, then Stage 1, then Stage 2 gated on its numbers.

---

## Addendum 2026-09-21 — the gate was satisfied, and the flag measured negative

Step 4 above — *"Before `KANNAKA_RECALL_TEMPORAL_EXP` is enabled anywhere"* — is now met. The
dedup fix and the eviction exemptions landed in #992 as M9/M8/M3. This records what happened when
the flag was then actually turned on, because the answer is **do not enable it**, and that is worth
more in the ADR than in a benchmark file alone.

### The flag could not have been measured before today

`temporal_weight` decays `0.5^(age / half_life)` from `observed_at` to *now*, then clamps the
result **up** to `TEMPORAL_SUPERSEDED_FLOOR`. That clamp binds at exactly two half-lives — 360 days
at the default 180. On any store whose contents are older than that, **every candidate returns the
floor**: the temporal factor becomes a constant multiplier, which cannot reorder anything. It does
not error, and nothing reports it.

That is a property of the design, not of the benchmark. Any deployment ingesting historical data —
or simply one that sits a year without writes — silently loses temporal ranking. Lowering
`KANNAKA_RECALL_TEMPORAL_FLOOR` does not help: it only moves the constant.

`recall --at` (#994) makes recency scoreable as of a chosen instant, which is also the question an
agent actually asks ("what did we use last March?"). It exposed a second defect in the same code:
`age_days = (now - confirmed).max(0)` clamped a negative age to zero, so a memory observed *after*
the scoring instant read as maximally fresh — asked about February, the system ranked a value
adopted in May first. Now floored, matching what `effective_at` already does for facts not yet in
force.

### Measurement (kannaka-bench `temporal2-*`, longmemeval_s, k=15, n=30, MiniLM)

Three arms, identical questions and stores, scored as of each question's own date:

| arm | hit@15 | recall@15 | evid@15 | MRR |
|---|---|---|---|---|
| flag off | 1.000 | 0.950 | 0.848 | **0.9183** |
| `exp=1.0`, half-life 180d (**shipped default**) | 1.000 | 0.950 | 0.848 | **0.9028** |
| `exp=1.0`, half-life 3000d | 1.000 | 0.950 | 0.848 | **0.9186** |

Retrieval quality is identical across all three. The flag finds nothing new and loses nothing; it
only reorders, and only **2 of 30 questions changed at all**:

```
knowledge-update    1.000 -> 0.500   WORSE
multi-session       0.091 -> 0.125   better
```

**The one category the feature exists for is the one it hurt.** Knowledge-update *is* the
superseded-fact case; enabling the flag pushed the gold answer from rank 1 to rank 2 on one of the
five. That is a single question — not evidence the flag harms the category, but not evidence it
helps, and the opposite of the intended effect.

The 3000-day arm is a consistency check rather than a candidate: once ages are days instead of
years, that half-life makes every weight ≈1.0, so it should be indistinguishable from "off" — and
is. Together with the 180-day arm differing under deterministic scoring on identical stores, that
is what establishes the measurement actually exercised the factor.

### Decision

**The Phase 3 gate stays closed.** `KANNAKA_RECALL_TEMPORAL_EXP` remains 0.0 by default and is not
enabled anywhere. M9/M8/M3 stay in regardless — each was written to be correct whether the flag is
on or off, so none of them depended on the flip to be worth having.

What this does **not** settle: `longmemeval_s` has five knowledge-update questions and nothing
stamped `--supersedes`, so only a handful of rankings can move at all. A corpus built to exercise
supersession directly — pairs where the stamped-current fact and its retired predecessor are both
retrievable — would test the mechanism rather than test whether it perturbs an unrelated benchmark.
That corpus is the precondition for reopening this, not another run of this one.
