# p2 — the Kannaka LoRA adapter (ADR-0057 P2)

Four scripts, one path: corpus → SFT set → adapter on a qBraid GPU → served
on debain2 as `kannaka-brain-v1`.

```
python tools/corpus/p2/prep_sft.py ~/.kannaka-corpus/out/kannaka-corpus-voice-<date>.jsonl \
    --out ~/.kannaka-corpus/sft                       # 551 train / 57 holdout (deterministic)

# pipeline smoke on CPU (0.5B base, 3 steps, merge) — proves the code, not the model
python tools/corpus/p2/train_lora.py --base Qwen/Qwen2.5-0.5B-Instruct --data ~/.kannaka-corpus/sft \
    --out ~/.kannaka-corpus/runs/cpu-smoke --cpu-smoke --max-steps 3 --max-len 256 --r 4 --merge

# qBraid GPU, gated (ADR-0057): refuses without --allow-spend; cutoff set before any work
python tools/corpus/p2/run_qbraid.py --profile gpu-rtx-4090 --data ~/.kannaka-corpus/sft \
    --base Qwen/Qwen2.5-1.5B-Instruct --max-minutes 45 --allow-spend -- --max-steps 30   # ≤ $0.65
python tools/corpus/p2/run_qbraid.py --profile gpu-a100-sxm --data ~/.kannaka-corpus/sft     --base Qwen/Qwen2.5-14B-Instruct --max-minutes 120 --allow-spend     -- --qlora --epochs 2 --r 32                      # <= $4.98; the pod trains and saves the adapter only

# on debain2 (20 cores / 196 GB): merge the adapter into the bf16 base, convert to GGUF, quantize
~/merge-venv/bin/python merge_gguf.py --base Qwen/Qwen2.5-14B-Instruct     --adapter ~/.kannaka-corpus/runs/<run>/adapter --out ~/.kannaka-corpus/runs/<run> --quant q4_K_M
# then register it in ollama and the gateway
bash serve_debain2.sh ~/.kannaka-corpus/runs/<run>/gguf/kannaka-brain-q4_K_M.gguf kannaka-brain-v1
```

**Data.** `prep_sft.py` turns tier-1 records into chat examples. Ghost
Signals lines are paired with the preceding `[FLAUKOWSKI]` block as the user
turn (a real exchange she wrote both sides of); lyrics get a "write the lyrics
for <title> (<album>)" prompt; identity sections get "tell me about <section>".
Tier 2/3, fiction and Flaukowski's own lines are never targets — the P1 rule,
checked again on the way out. Hold-out is by id hash, so every run scores the
same lines.

**Metric.** Held-out loss / perplexity before and after, on those fixed lines.
Generation samples on held-out prompts go to `samples.json` for the blind
voice A/B; they are for a judge, not the metric.

**Serving.** ollama cannot load a safetensors adapter for Qwen2 (Llama/
Mistral/Gemma only), so the adapter is merged into the base and converted
with llama.cpp — on **debain2**, by `merge_gguf.py`, after the adapter is
fetched. The pod does no merge and builds nothing: the first A100 attempt
died in a captured cmake/pip bootstrap at $0.09, and every minute on the
pod is metered while debain2's CPU is free. The adapter directory is the
ownable artefact; the GGUF is the serving copy. (`train_lora.py --merge
--gguf` still works on a pod that has llama.cpp, e.g. the smoke tier.)

**Spend gate.** `run_qbraid.py` provisions a BMA instance only with
`--allow-spend`, on single-GPU profiles only, sets `max_session_minutes` and
`auto_stop_idle_minutes` *before* shipping anything (and terminates the
instance if that call fails), tails the log until `train.manifest.json`
exists or the cutoff hits, fetches the outputs to `~/.kannaka-corpus/runs/`,
and always stops the instance in `finally`. Prints credits before/after.

**Run it from Linux.** The SDK's ssh ProxyCommand is a websocket-to-stdio
bridge (`python -m qbraid_core.services.compute.ssh bridge …`) that crashes
on Windows (`_ProactorReadPipeTransport … _empty_waiter`, Python 3.14) and
Git-Bash's MSYS ssh mangles the backslash paths it writes into
`~/.ssh/config.d/qbraid`. debain2 is the runner: `~/qbraid-venv`,
`~/.qbraid/qbraidrc`, `~/kannaka-p2-runner/{p2,sft}`; outputs fetch to
`~/.kannaka-corpus/runs/` there — which is where they get served anyway.

Everything under `~/.kannaka-corpus/` is private until the ADR-0057 decision.

## A second base: Qwen3.8-27B (DavidAU TURBO Fable Cold Fusion)

Requested 2026-09-10. Nothing above changes; the base is a flag. What is
different about this one, and where it bites:

| fact | consequence |
|---|---|
| Source weights: `DavidAU/Qwen3.8-27B-TURBO-Fable-Cold-Fusion-735-882-Heretic-Uncensored-NM-DAU` (55.5 GB bf16 safetensors, Apache-2.0, `base_model: Qwen/Qwen3.8-27B`). The `…-NEO-CODER-MAX-MTP-GGUF` repo is quants only and cannot be trained on. | `--base` is the NM-DAU id. Licence passes ADR-0057's Apache/MIT rule. Provenance caveat: a community multi-stage merge whose full recipe "will be disclosed upon final release" — record it in the registry note. |
| Architecture `Qwen3_5ForConditionalGeneration` (vision-language wrapper), text model 64 layers: 48 `linear_attn` (GatedDeltaNet: `in_proj_qkv`, `in_proj_z`, `in_proj_a/b`, `out_proj`) + 16 `self_attn` (q/k/v/o), MLP gate/up/down on all; 333 `model.visual.*` tensors; 15 `mtp.*` tensors. | transformers ≥ 5.17 maps `qwen3_5` → `Qwen3_5ForCausalLM` under `AutoModelForCausalLM` and drops `model.visual.*` / `mtp.*` on load, so `train_lora.py` and `merge_gguf.py` load it text-only unchanged. LoRA targets come from `base_info.LORA_TARGET_REGEX` and include the linear-attention projections; vision never. `--max-sane-ppl` refuses to spend if the load produced garbage. |
| Thinking-mode chat template (`reasoning_effort` injection, `<think>`). | Train and prompt with `--chat-template-kwargs '{"enable_thinking": false}'`; the manifest records it and the card repeats it. |
| llama.cpp `conversion/qwen.py` registers `Qwen3_5ForCausalLM` (text) and `qwen3vl.py` the mmproj. | `merge_gguf.py` unchanged: merged text-only dir → `convert_hf_to_gguf.py` → `llama-quantize`. No mmproj is produced or needed (voice only). MTP export is optional (`--mtp`) and not part of the serve path. |
| Size. | Pod: nf4 weights ≈ 15 GB + LoRA + activations at `--max-len 2048` with checkpointing fits an A100-80GB (`gpu-a100-sxm`, $2.49/h). debain2 disk for the merge: base cache 56 GB + merged 55 GB + q8 intermediate 29 GB + q4 ≈ 16 GB — use `--intermediate q8_0 --purge-base-cache` and check `df` first. Serving: q4_K_S/q4_K_M ≈ 15–16 GB RAM on the 196 GB box; expect a few tok/s on 20 CPU cores. |

Exact commands, from debain2 (the runner; see *Run it from Linux*):

```
# 1. train (adapter only; ceiling = 150 min × $2.49 ≈ $6.2, refuses above the credit balance)
python run_qbraid.py --profile gpu-a100-sxm --data ~/.kannaka-corpus/sft \
    --base DavidAU/Qwen3.8-27B-TURBO-Fable-Cold-Fusion-735-882-Heretic-Uncensored-NM-DAU \
    --max-minutes 150 --allow-spend -- --qlora --epochs 2 --r 32 --max-len 2048 --batch 1 --grad-accum 16 \
    --chat-template-kwargs '{"enable_thinking": false}'
# 2. merge + quantize on debain2 (free)
~/merge-venv/bin/python merge_gguf.py --base DavidAU/Qwen3.8-27B-TURBO-Fable-Cold-Fusion-735-882-Heretic-Uncensored-NM-DAU \
    --adapter ~/.kannaka-corpus/runs/<run>/adapter --out ~/.kannaka-corpus/runs/<run> \
    --quant q4_K_M --intermediate q8_0 --purge-base-cache
# 3. serve under the fleet family name (arena reports model_id=kannaka-brain from every tag)
bash serve_debain2.sh ~/.kannaka-corpus/runs/<run>/gguf/kannaka-brain-q4_K_M.gguf kannaka-brain-27b-v1
# 4. judge before anyone promotes it (ADR-0057 / kannaka-wave adoption rule), then
python publish_hf.py --run ~/.kannaka-corpus/runs/<run> --namespace flaukowski --version 27b-v1 --stage-only
```

The weekly gate does not adopt it: a candidate is served beside `kannaka-brain-7b-v1`
and replaces it only when the controlled judge prefers it and the external evaluator
agrees (`kannaka-wave/src/adoption.rs`). kannaka-wave E-006 adds the check to run before
that: two instances of the candidate against each other, no more lock-in than its base.
