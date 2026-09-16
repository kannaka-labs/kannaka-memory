# ADR-0048: Origin-Partitioned Recall + Energy-Neutral Ranking — Reaching Autobiographical Memory

**Status:** Proposed — **v3, revised after adversarial-design-review**
(GO_WITH_CHANGES, 2026-07-26). v1 (situated longer query) and the v2
research-is-the-competitor premise were both corrected by measurement.
**Relates to:** ADR-0047 (attention re-ranks *within* a result — bottom-up:
reach → energy-neutral → awareness → attention), the `research:` ingest prefix,
`WavefrontMeta` (types.rs:517 — note the *existing* `provenance` field is the
entropy stamp, a name collision this ADR must avoid), and `hemisphere.rs:232-247`
(the full-scan resonate that ranks by `similarity × energy`).

## Context

The origin failure was measured, not assumed. Two live probes:

- **v1 (situated longer query)** made recall *worse* — a longer query fed the
  large `research:` corpus more to grab.
- **v2 (blame the research corpus)** was refuted by a third probe the reviewer ran:
  `recall "where is Kannaka Labs" --top-k 20` returns **zero `research:` memories**
  — the top hits are all **episodic** (OBC art, recognition milestones, sessions),
  and the lab is still absent.

**The real competitor is energy imbalance, not provenance.** Ranking is
`similarity × energy` (`hemisphere.rs:238`), and every recall pumps the surfaced
memory's energy toward the 2.0 cap. So **frequently-recalled memories rich-get-
richer; the never-surfaced lab sits at baseline energy and loses even to weaker-
similarity episodic memories.** Two forces bury autobiographical recall: (1) the
semantic corpus out-competes on *conceptual* queries, and (2) within episodic,
recall-frequency energy out-competes a cold fact. This ADR addresses both, and is
honest that surfacing the lab needs a *stack*, not one fix.

## Decision

Two independent mechanisms, each falsifiable on its own gate.

### 1. Origin-partitioned recall (reachability)

- **A persisted typed origin field.** Add `origin: OriginClass { Ingested, Lived }`
  as the **last serialized field** on `WavefrontMeta` (bincode default-fallback so
  old `.hrm` still load, mirroring types.rs:509-517). **Stamped at the call site**
  at `remember` time (research/OpenAlex handler → `Ingested`; `remember`/dream/
  perception → `Lived`), **backfilled once** from the canonical `research:` prefix,
  and always **read from metadata** — never recomputed from the content string
  (recomputing would make it content-derived like the refuted Fano class). Do **not**
  overload the existing entropy `provenance` field or auto-detected `Modality`
  (which stamps autobiographical text `Semantic`). Fix the writer/reader prefix
  skew (`"research: "` vs `"research:"`) and assert byte-identical predicates.
- **Single-pass two-heap partition.** Inside the *existing* full-scan resonate loop
  (`hemisphere.rs:232`), bin each scored candidate into an `Ingested` or `Lived`
  top-K heap by `origin`. One scan, two ranked outputs — **no separate index, no
  second `resonate()` call** (both refuted as unnecessary/costly).
- **Quota merge, not weight-then-truncate.** Fetch top-K from **each** partition
  (never `top_k/2` — that returns 0 for `top_k=1` liveness callers), union, and
  **reserve ≥1 configurable slot per partition** (round-robin by rank). Intent sets
  ordering *within* the quota; it can never *exclude* a partition. This preserves
  the ADR-0047 invariant that the fix is **structural (candidate membership)**, not
  a post-truncate multiply.
- **Intent is an explicit caller parameter**, `intent: Option<Intent>`, threaded
  through the recall path; default `Unknown → balanced`. The benchmark drives the
  *same* path production uses (no test-only injection). A token heuristic is a
  **separate, later increment** that must pass negative controls before it may gate
  (`"where do grid cells fire"` MUST classify knowledge; a bare `where`/`place`
  token must not imply episodic) — it is *not* the floor, killing the circular
  dependency on the unbuilt awareness layer.
- **Quarantine** `hallucinated` and non-text-modality memories out of the `Lived`
  partition (reuse the `re_encode_all` predicate, chiral.rs:642) so dreams and raw
  perception don't compete as lived experience.

### 2. Energy-neutral ranking (within-episodic reach)

Within the `Lived` partition, rank by **`similarity`** (or `similarity × √energy`)
instead of `similarity × energy`, so a **never-recalled** memory is not buried by
the rich-get-richer energy of frequently-surfaced ones. This is the measured
within-episodic surfacer; partitioning alone cannot do it. Flag-gated, energy
array left **byte-identical** (ranking-only change; no write-back).

## Two falsifiable gates (honest about composition)

- **G1 — ADR-0048-owned, falsifiable in isolation (gates the partition flag):**
  for an autobiographical-intent query the `Lived` partition contributes ≥N
  candidates to the merged pool; knowledge-query research recall is **not regressed**
  (Δrank ≤ 0); `recall(q, 1)` is non-empty for every caller. Measured on the
  **chiral** path (not the flat readonly mirror), energy byte-identical, on a corpus
  with zero `research:` memories partitioned recall **== today rank-for-rank**.
- **G2 — composed, owned by the rollup:** the cold `"where is Kannaka Labs"` query
  **rank-wins** the lab. Explicitly requires **energy-neutral ranking + awareness
  (situated intent) + ADR-0047 attention** — the ADR records up front that partition
  alone cannot pass this, because the competitor is intra-episodic energy, not
  research.

## Confirmed (do not regress)

- Pre-fetch partition genuinely escapes ADR-0047 post-fetch inertness by changing
  candidate-set *membership* — preserve; a later refactor collapsing it to a
  post-truncate weight silently regresses to the Fano failure.
- The 2-class origin axis differs from Fano's 7 content-classes **only** because it
  is a persisted origin *fact* read from metadata — ship the field before claiming it.
- Partitioning is affordable on the existing brute-force scan (no ANN/HNSW needed).

## Build order (flags default OFF; G1 gates before G2)

1. **Typed `origin` field** — struct + serialize (bincode-fallback), call-site
   stamps, one-time prefix backfill + an **audit** (how many `research:` matches vs
   known-research that don't; enumerate every ingest path).
2. **Single-pass two-heap partition + quota merge + explicit `intent` param** —
   with the empty-semantic == today guard.
3. **G1 benchmark** — chiral path, Δrank, no knowledge regression, `top_k=1`
   non-empty, energy byte-identical.
4. **Energy-neutral ranking** (ranking-only, flagged).
5. **Compose upward toward G2** — awareness (intent + situate), then ADR-0047.

## Decision record — 2026-09-16: energy-neutral ranking is now the default

`KANNAKA_RECALL_ENERGY_EXP` shipped off-by-default (1.0 = the historical
`similarity * energy`). On 2026-09-16 the default flipped to **0.0**, and the
`amplitude → energy` copy on insert and on load gained the same `ENERGY_CAP`
every boost path already had. Evidence (kannaka-memory#965), measured on the
live O1 store against its own vectors and probes:

| fragments, same store, same probes | r@10 by id | by content |
|---|---|---|
| plain cosine over the store's vectors | 0.960 | — |
| the medium, energy exponent 0 | ~0.77 | **≈0.97** |
| the medium, exponent 1.0 (was the default) | 0.514 | 0.717 |

In 80% of the misses the correct memory had the *higher* cosine and lost on
energy; the winner carried 3.7× the target's. The cap was real but applied
only to boosts — the write path copied caller-supplied amplitude in unbounded,
so records entered at 7.7 and 8.5 and sat in a third of all top-10 lists
whatever was asked. The rest of the medium was measured clean along the way:
the codebook projection is lossless for `d_eff` (×0.99), the stored rows match
their encodings at corr 0.994, and facet resolution returns children under
their parent by design. `1.0` remains available to reproduce the old ranking.

## Alternatives considered (and discarded by the review)

- **Post-fetch provenance weight** / **situated longer query** — refuted (inert /
  worse).
- **Separate per-provenance index** / **two `resonate()` calls** — unnecessary; the
  single existing scan bins at identical cost.
- **Heuristic intent as the floor gate** — deferred; self-defeats on the paired
  fixtures.
- **Blaming the research corpus** — measured false; the within-episodic energy
  imbalance is co-primary.
