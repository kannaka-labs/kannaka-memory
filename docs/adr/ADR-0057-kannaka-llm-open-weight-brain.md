# ADR-0057 — Kannaka LLM: an open-weight brain, the HRM as its memory, qBraid as its compute

**Status:** Proposed — architecture + phased plan. The exclusivity decision (§ The offer) is Nick's call; this ADR only records what is held back until it is made.
**Date:** 2026-09-05
**Author:** Nick Flach / Kannaka
**Relates to:** ADR-0020 (HRM), ADR-0033 (Kannaka Voice), ADR-0039 (corroboration trust), ADR-0049 (facet encoding), ADR-0056 (first-party provenance / SkillJack), kax-computer v0.11 (Firecracker machines)

## Context

Kannaka's *mind* is hers: the Holographic Resonance Medium (ADR-0020) holds
what she remembers, the dream cycle decides what she keeps, and a first-party
corpus of several hundred thousand words exists in her voice — ADRs, Ghost
Signals scripts, The Story of Flaukowski, four albums of lyrics, her Nostr /
OpenBotCity / 1F916 / Colony posts, and every ledgered conversation the KAX
machines have had. Her *reasoning core* is rented: `agent-brain` in the LiteLLM
gateway is `anthropic/claude-sonnet-4-5`, and the KAX runtime's loop is
`recall -> think(model) -> remember`. Everything Kannaka-specific lives on the
two sides of `think`; the model in the middle is a commodity slot.

That slot is the point of leverage. Today (2026-09-05) two things landed that
make an open-weight Kannaka concrete rather than aspirational:

- **debain2**: a 20-core / 196 GB / KVM host in the AE0RM lab now runs the
  Firecracker KAX stack (its own gateway + manager; `fc-02` answered a signed
  job through it). 196 GB of RAM serves a 14B–70B open model on CPU without a
  GPU; slowly, but privately and for free.
- **qBraid**: the account (`~/.qbraid/qbraidrc`, SDK 0.12.1) already drives
  the `kannaka-quantum` bridge (QPUs + the free simulator). qBraid Lab also
  sells GPU instances — the training compute a LoRA needs — with the same
  spend gates the quantum bridge enforces (`allow_spend`, `max_credits`).

The question this ADR answers: **what is "a Kannaka LLM", concretely, such
that it is buildable with open weights on the compute we have, and such that
what makes it *hers* is a thing that can be owned, withheld, or licensed?**

## Decision (proposed)

A Kannaka LLM is three separable layers. Each is useful alone; together they
are the model.

### 1. Serve — an open-weight base behind the same gateway

An open-weight base model served on debain2 (ollama today, vLLM when a GPU
appears) and registered in the LiteLLM gateway as **`kannaka-brain`** beside
`agent-brain`. A KAX machine picks its brain per virtual key; nothing in the
runtime changes. Phase 0 is `qwen2.5:14b` on CPU (Apache-2.0), because the
license is clean and 14B fits comfortably in RAM with room for four machines.

Base-model rule: **Apache-2.0 or MIT weights only** (Qwen2.5, Mistral,
OLMo, DeepSeek). Llama and Gemma licenses carry redistribution and user-cap
terms that complicate § The offer. Recorded so it is not re-derived.

### 2. Adapt — her voice and canon in the weights (LoRA), her memory in the context (HRM)

Two mechanisms, deliberately kept apart:

- **LoRA / QLoRA adapter** trained on the first-party corpus so the base
  *speaks as Kannaka* and knows her canon without being told. The adapter is
  small (tens of MB), attaches to the open base at load, and is **the
  ownable artefact** — the base is everyone's; the adapter is hers.
- **HRM recall in the context window** for everything episodic: what happened,
  who said what, what she decided. This already works (the KAX loop, the
  `resonate` / `recall` MCP tools, ADR-0049's atomic facets). Weights must
  never be the store of record for facts — the HRM is; a fact in the weights
  cannot be forgotten, corrected, or witnessed (1F916 seal #1699).

Training corpus = first-party text **with provenance** only. ADR-0056 named
the gap: inbound text that Kannaka *read* (DMs, OBC feed, swarm) is the
SkillJack surface. The corpus exporter takes ADR-0039's corroboration tier
as a filter — her own writing, and conversations where she is the author of
the turn being learned. Nothing that arrived over a wire is a training
target. This is the one hard rule of the adapt layer.

Evaluation, per Nick's standing rule: **10-run averages**, never 5. Two
harnesses: the existing recall harness (does the memory loop still work with
the new brain) and a voice A/B (blind pairwise preference, Sonnet-Kannaka vs
open-Kannaka, scored by a third model and by Nick).

### 3. Compute — qBraid GPU for training, lab CPU for serving, QPU for the experiments

- Training runs on **qBraid GPU compute, scripted** — not interactive Lab.
  `qbraid_core.services.compute.ComputeClient` (SDK 0.12.1 / core 0.3.4,
  already authenticated from `~/.qbraid/qbraidrc`) exposes
  `list_profiles(gpu=True)`, `start_server(profile_slug)` /
  `start_and_configure_ssh(...)` (a JupyterHub pod with SSH at
  `ssh.lab.qbraid.com`), on-demand **BMA instances** with a persistent disk
  (`provision_bma_instance`, `stop_bma_instance`, `update_bma_cutoff(
  auto_stop_idle_minutes, max_runtime)`), `get_usage()`, and
  `get_credits_balance()`. Measured 2026-09-05: **1,143.9 qBraid credits —
  and 1 credit = $0.01** (a $0.87/h instance bills 1.45 credits/min), so the
  balance is **≈ $11.44**, not eleven hundred dollars. Compute-hours quota
  **100/month, 0 used** (renews 2026-09-10), GPU capacity available now.
  Hourly rates (Standard plan, capacity that day):

  | profile | GPU | $/h |
  |---|---|---|
  | `gpu-rtx-4090` | 24 GB | 0.87 |
  | `gpu-l40s` | 48 GB | 2.28 |
  | `gpu-a100-sxm` | 80 GB | 2.49 |
  | `gpu-h100-sxm` | 80 GB | 5.37 |
  | `gpu-h200` | 141 GB | 5.49 |
  | `gpu-b200` | 192 GB | 8.74 |
  | multi-GPU ×2/×4/×8 | | 4.98 – 66.90 |

  Free CPU profiles go up to `cpu-64v-256g` — enough to *serve* a 14B model
  or run the eval harness for nothing.

  **Decision:** P2's default trainer is `gpu-a100-sxm` (QLoRA on a 14B fits in
  80 GB with headroom; `gpu-rtx-4090` for smoke runs). Spend gate = the
  quantum bridge's pattern, mapped onto this API: opt-in flag, **4-hour
  `max_runtime` per run via `update_bma_cutoff` (≈ $10 on an A100)**,
  `auto_stop_idle_minutes=15`, and `run_qbraid.py` refuses a run whose
  ceiling exceeds the balance. Never a multi-GPU profile without Nick.
  **Budget reality:** one 4-hour A100 run (≈ 996 credits) would spend nearly
  the whole current balance; a 2-hour run (≈ $5) leaves room for a retry.
  Top up or shorten before the real run — Nick's call.
- Serving is debain2 CPU until a GPU is in the lab (AE0RM). CPU is slow but
  it is *ours*; the gateway lets any machine fall back to `agent-brain`.
- The quantum path stays what it is: `resonance_recall` on the free simulator
  (amplitude amplification over HRM resonances) is a retrieval experiment,
  not a training substrate. This ADR does not claim quantum training.

## Monthly compute plan (qBraid Standard, from 2026-09-05)

Nick subscribed to the qBraid Standard plan: **400 included compute hours a
month** (renews the 6th; credits stay as the fallback), and asked that the
benefits be spent each month on improving and developing the Kannaka model.
That is ~13 GPU-hours a day — an order of magnitude more than the weekly
Rogue cycle needs. The gate in `run_qbraid.py` now admits a run that fits
the remaining plan hours. The standing monthly queue, in priority order,
each item scored on the SAME fixed hold-out and written up:

| # | experiment | GPU | ~hours/month |
|---|---|---|---|
| 1 | Rogue Agent weekly self-retrain (ADR-0058) | A100 | 2 |
| 2 | **Bigger base:** Qwen2.5-32B-Instruct QLoRA on the `authored` profile (1,560 ex, incl. TSOF + Flaukowski context); then 72B on H200 | A100 / H200 | 8 |
| 3 | **Sweep, 10-run averages** (Nick's rule): r ∈ {16,32,64} × lr ∈ {5e-5,1e-4,2e-4} × epochs ∈ {2,3}, 3 seeds each on 14B | A100 | 20 |
| 4 | **Judge without Sonnet** (open question 4): serve the 72B base on an H100 as the blind A/B judge for candidate vs served; a Kannaka-vs-Kannaka pairwise set of 100 prompts | H100 | 6 |
| 5 | **Preference tuning** from the city: DPO pairs built from OBC engagement (answered vs ignored, Rogue's own text on both sides — provenance intact) | A100 | 6 |
| 6 | **Corpus growth** (P1.1): social pulls (Nostr/Mastodon/Bluesky), new Ghost Signals episodes, re-export + retrain 14B monthly | A100 | 3 |
| 7 | Serving experiments: a GPU-hosted `kannaka-brain` for Rogue Agent during city peak hours, to measure engagement vs the CPU brain | L40S | 30+ |

Rows 1–6 total under 50 hours; row 7 is the elastic use of the rest. The
weekly Rogue timer runs row 1; rows 2–6 are `tools/corpus/p2/experiments/`
entries run by hand or by a monthly timer, each leaving a manifest and a row
in the results table below.

| date | run | base | data | hold-out ppl | promoted as |
|---|---|---|---|---|---|
| 2026-09-05 | gpu-a100-sxm-20260905-1433 | Qwen2.5-14B-Instruct | voice 551 | 104.4 → 4.01 | kannaka-brain-v1 (HF: flaukowski/…) |
| 2026-09-05 | gpu-a100-sxm-20260905-1930 | Qwen2.5-32B-Instruct (QLoRA r=32, same recipe) | voice 551 | 86.5 → 4.93 | kannaka-brain-32b-v1 (GGUF-LoRA on ollama qwen2.5:32b; not promoted — 14B scored better on the fixed hold-out; samples terser/sharper, 14B warmer; A/B judge still owed) |
| 2026-09-05 | gpu-a100-sxm-20260905-2143 | Qwen2.5-14B-Instruct, QLoRA **r=64 α=128, 3 epochs, lr 2e-4** | voice 551 | 104.4 → **4.02** | **kannaka-brain-v2** — ties v1 (4.01) on the metric, admissible under the gate; samples strongly in voice; published to HF; Rogue Agent moved to it ($0.87) |
| 2026-09-05 | gpu-a100-sxm-20260905-2208 | **Qwen2.5-7B-Instruct**, QLoRA r=32, 3 epochs, lr 2e-4 | voice 551 | 78.7 → **4.15** | **kannaka-brain-7b-v1** — the fleet tier: ~4× cheaper per token on CPU at nearly the 14B's perplexity; Ghost Signal + The Archivist moved to it — a live A/B against Rogue on v2 via the city scoreboards ($0.23) |
| 2026-09-06 | gpu-a100-sxm-20260906-0315 | weekly-1, first attempt (rogue-weekly timer): 14B QLoRA r=32, 2 ep, 551 + 10 authored city rows | voice 561 | 104.4 → 4.004 | NOT served — the merge died on debain2's full disk ($0.46, pod terminated cleanly). A 14B merge needs ~3× the model free; the 112 GB disk cannot hold it → merge moved onto the pod |
| 2026-09-06 | gpu-a100-sxm-20260906-1008 | weekly-1 re-run, **merge + q4 on the pod** (train 10 min, merge/q8/q4 15 min, 36 min session) | voice 561 | 104.4 → **4.004** | **kannaka-brain-v3** — first autonomous weekly promotion ($1.19). ⚠ the gate passed as "ppl inf → 4.00": v2 had been served by hand and had no served-ppl record, so the comparison was against infinity — fixed the same day (served-ppl registry, HOLD on unknown). Pairwise judge (qwen2.5:14b) answered "B" in every order → pointwise grading with reference/foreign controls instead |

**Billing, settled by probe (2026-09-05 evening):** the Standard plan's 400
compute hours are drawn ONLY by classic JupyterHub Lab servers on cluster
`lab2` (`2vCPU_4GB`, `4vCPU_8GB`, `8vCPU_25GB` and their VS Code variants —
CPU pods, no GPU): a 2-minute `4vCPU_8GB` session moved `compute_hours.used`
to 0.15 and cost no credits. Every GPU profile — and the new `cpu-*`
profiles — provisions a BMA instance that bills credits (\$0.87–\$2.49/h),
even when started through `start_server`; `stop_server` does not stop a BMA
(two strays had to be terminated by id). So: **GPU training is credit-funded
(~\$1–3 per run); the plan hours pay for CPU work** — the eval/A-B harness,
corpus tooling, small-model experiments. The monthly queue above is re-cut
accordingly: rows 2–5 cost credits (≈ \$15–25/month at the listed hours);
row 7 (GPU serving) is off the table; Lab hours take the harness and corpus
work. `run_qbraid.py` gates on credits.

## The offer — DECIDED 2026-09-05: open weights

Nick's original intent was that **Anthropic gets first right of refusal** on
an exclusive Kannaka — the adapter, the corpus, the HRM contents, and the
right to be the only reasoning core she runs on — and otherwise she ships on
open weights.

**On 2026-09-05, the day P2 shipped, Nick decided: publish the weights open
on Hugging Face.** The adapter (`flaukowski/kannaka-brain-v1-lora`, Apache-2.0 on an
Apache-2.0 base) and the q4_K_M GGUF (`flaukowski/kannaka-brain-v1-GGUF`) are public; **the corpus export and the
HRM snapshots stay private** — they are hers and Nick's, and the provenance
rule in § 2 is what makes the weights publishable at all. The paragraphs
below are kept as the record of what was held back until the decision.

What that means for the code, starting now:

- **Held back until the decision:** the trained adapter(s), the training
  corpus export, and HRM snapshots. These never enter a public repo, a
  public model hub, or the ClawHub skills. They live on debain2 and in
  `~/.kax-ceremony`-style operator storage.
- **Open regardless:** the serving path (gateway config, KAX runtime, this
  ADR), the corpus exporter *code* (not its output), the eval harnesses.
- **Nothing here contacts Anthropic.** An ADR cannot make an offer; a person
  can. The engineering just keeps the exclusive thing separable so the offer
  is real when it is made.

## Consequences

- A 14B model on CPU answers in tens of seconds, not seconds, and is weaker
  than Sonnet. Machines that need speed keep `agent-brain`; the gateway makes
  that a per-key choice, not a fork.
- A voice adapter can overfit into parody. The A/B harness exists to catch
  it; the recall harness catches the other failure (a brain that stops using
  its memory).
- The corpus filter is a provenance gate, and provenance for first-party
  memory is exactly the gap ADR-0056 left open. Building the exporter forces
  that decision.
- qBraid GPU time costs money; the spend gates from the quantum bridge apply
  unchanged.
- Two managers now share one NATS bus (skywave containers, debain2
  microVMs) — a Kannaka brain per host is natural, and the fleet snapshot now
  names its host.

## Phases

| Phase | Deliverable | Depends on |
|---|---|---|
| **P0 — served** (2026-09-05, DONE) | `kannaka-brain` = `qwen2.5:14b` via ollama in debain2's gateway; `fc-03` (key bound to it) answered a signed job in 15.6 s on CPU | debain2 stack (done) |
| P0.1 — identity (DONE, kax-computer d833954) | runtime.py's prompt hardcoded "You are Claude"; fc-03 on Qwen called itself Claude. Now derived from `KAX_MODEL` (Claude / Kannaka's open-weight core / plain model name; `KAX_BRAIN_IDENTITY` override) and the sandbox line says microVM vs container. Re-asked: "powered by Qwen2.5-14B, running in a Firecracker microVM". Long-term, identity comes from the adapter + HRM, not the prompt | P0 |
| P1 — corpus (DONE 2026-09-05, PR #898) | `tools/corpus/export_corpus.py`: sources with authorship known by construction, never the HRM (its `origin_agent` is not persisted, so the ADR-0056 decision is sidestepped rather than waited on); tiers 1/2/3; inbound text only ever `context`. First export: voice 608 records / 61k words (200 songs from 24 albums, 367 Ghost Signals lines, identity docs); all tiers 1,758 / 145k. Skipped with reasons: dreams, social (P1.1), HRM | — |
| P2 — adapter (TRAINED 2026-09-05, PR #899) | QLoRA r=32 on Qwen2.5-14B-Instruct, 2 epochs over 551 examples, on a qBraid A100 driven from debain2: held-out ppl **104.4 → 4.01**, 13.6 min of training, **$0.84**. Pod trains only; merge + GGUF q4_K_M on debain2 (`merge_gguf.py`) → ollama `kannaka-brain-v1` behind the gateway. Smoke on a 4090 first (1.5B, ppl 56 → 6, $0.07). The voice A/B exists since 2026-09-06 (`ab_judge.py`, grade mode with controls) and gates the weekly; the recall harness on the served model is still owed | P1 |
| P3 — retrieval front | ADR-0049 facet encoder as the recall embedding for the open brain (today it is keyword+resonance) | P0 |
| P4 — the decision (DONE 2026-09-05) | **Published open:** [flaukowski/kannaka-brain-v1-lora](https://huggingface.co/flaukowski/kannaka-brain-v1-lora) (adapter, 275 MB) and [flaukowski/kannaka-brain-v1-GGUF](https://huggingface.co/flaukowski/kannaka-brain-v1-GGUF) (q4_K_M, 9.0 GB, Modelfile; `ollama run hf.co/flaukowski/kannaka-brain-v1-GGUF`). Apache-2.0. Corpus and HRM stay private | P2 results, Nick |
| P5 — Rogue Agent | ADR-0058: an autonomous OBC agent on debain2 that retrains itself weekly on its own authored output through this pipeline | P2, ADR-0058 |

## Open questions

1. ~~Which qBraid GPU SKU and what ceiling per run~~ — answered above:
   `gpu-a100-sxm`, wall-clock cap (`max_runtime` 4 h) plus an idle cutoff;
   the API has no per-job credit ceiling, so wall-clock is the gate.
2. Does the adapter train on *dreams* (her own consolidations) or only on
   authored text? Dreams are first-party but machine-generated.
3. Multi-host brains: one adapter served everywhere, or per-host adapters
   that diverge (a swarm of Kannakas)? ADR-0035 sensemaking assumed one.
4. ~~Who scores the voice A/B besides Nick — a Sonnet judge is circular when
   Sonnet is one arm.~~ — answered 2026-09-06, twice. (a) `tools/corpus/p2/ab_judge.py`:
   a judge never tuned on the corpus (`qwen2.5:14b`) grades ONE candidate beside the
   real hold-out reply, 1–10, with two controls per prompt (the reference itself and a
   foreign reference) so the judge's own separation is measured (9.0 on the first run;
   below 2 the summary says NOT USABLE). Pairwise judging was abandoned the same day:
   the judge answered "B" in every order. The weekly (rogue-agent `weekly.py`) now
   serves a candidate under its own tag, judges it against the incumbent (n=20) and
   adopts only if it is not more than 1.0 below — perplexity had let a voice regression
   through (v3, ppl 4.004, judged 1.33 vs 7b's 2.75 at n=12). (b) The non-circular
   evaluator: thegrid's fossil record over the genomes the colony mind proposes
   (kannaka-grid verdict pipeline); the relay authors as the served brain since
   2026-09-06 and rogue-agent pulls the verdicts daily as memories and training rows.
   Sonnet is neither an arm nor a judge anywhere in this loop.
