#!/usr/bin/env bash
# ADR-0057 code-graph experiment — one pod session, three stages, results survive a cutoff.
#   stage 1  train the REAL adapter on data/real, then eval base_* + real_* arms   (out/eval_stage1)
#   stage 2  if time remains: train the SCRAMBLED control on data/scr, eval all six arms (out/eval)
#   done     write out/arms.json and the out/JOB_DONE marker run_qbraid.py --job waits for
# Env: BASE (HF id), N_PER_KIND (eval questions per kind), STAGE2_LIMIT_MIN (skip the control if
# stage 1 took longer than this), EPOCHS, R, LR, MAXLEN, BATCH, ACCUM.
set -u
cd "$(dirname "$0")"
BASE="${BASE:-Qwen/Qwen2.5-7B-Instruct}"
N_PER_KIND="${N_PER_KIND:-60}"
STAGE2_LIMIT_MIN="${STAGE2_LIMIT_MIN:-55}"
EPOCHS="${EPOCHS:-2}"; R="${R:-32}"; LR="${LR:-1e-4}"; MAXLEN="${MAXLEN:-1024}"; BATCH="${BATCH:-4}"; ACCUM="${ACCUM:-4}"
T0=$(date +%s)
mkdir -p out
log() { echo "[$(date -u +%H:%M:%S)] [arms] $*"; }
elapsed_min() { echo $(( ($(date +%s) - T0) / 60 )); }
train() {  # train <data-subdir> <out-subdir>
  python3 train_lora.py --base "$BASE" --data "data/$1" --out "out/$2" --qlora --epochs "$EPOCHS" --lr "$LR" \
    --r "$R" --max-len "$MAXLEN" --batch "$BATCH" --grad-accum "$ACCUM" --eval-samples 6
}
summary() {  # summary <status>
  python3 - "$1" "$(elapsed_min)" <<'PY'
import json, sys, os
status, minutes = sys.argv[1], int(sys.argv[2])
d = {"status": status, "elapsed_min": minutes, "base": os.environ.get("BASE")}
for k in ("real", "scr"):
    p = f"out/{k}/train.manifest.json"
    if os.path.exists(p):
        d[k] = json.load(open(p))
for k in ("eval_stage1", "eval"):
    p = f"out/{k}/results.json"
    if os.path.exists(p):
        d[k] = json.load(open(p))
json.dump(d, open("out/arms.json", "w"), indent=1)
print(json.dumps({k: v for k, v in d.items() if k in ("status", "elapsed_min")}))
PY
}
finish() { summary "$1"; touch out/JOB_DONE; log "JOB_DONE ($1) after $(elapsed_min) min"; }
trap 'finish interrupted' INT TERM

log "stage 1: train real ($BASE, epochs=$EPOCHS r=$R)"
if ! train real real; then log "real training FAILED"; finish real-failed; exit 1; fi
log "stage 1 trained in $(elapsed_min) min; eval base + real"
python3 eval_graph.py --base "$BASE" --adapter real=out/real/adapter \
  --evals data/eval_seen.jsonl data/eval_unseen.jsonl \
  --arms base_alone base_ctx real_alone real_ctx --n-per-kind "$N_PER_KIND" --out out/eval_stage1 \
  || log "stage-1 eval FAILED (continuing)"
summary stage1
if [ "$(elapsed_min)" -gt "$STAGE2_LIMIT_MIN" ]; then
  log "stage 1 took $(elapsed_min) min > $STAGE2_LIMIT_MIN; skipping the scrambled control"
  finish stage1-only; exit 0
fi
log "stage 2: train scrambled control"
if ! train scr scr; then log "scrambled training FAILED"; finish scr-failed; exit 1; fi
log "stage 2 trained; eval all six arms"
python3 eval_graph.py --base "$BASE" --adapter real=out/real/adapter --adapter scr=out/scr/adapter \
  --evals data/eval_seen.jsonl data/eval_unseen.jsonl \
  --arms base_alone base_ctx real_alone real_ctx scr_alone scr_ctx --n-per-kind "$N_PER_KIND" --out out/eval \
  || log "final eval FAILED"
finish complete
