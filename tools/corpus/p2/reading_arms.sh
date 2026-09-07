#!/usr/bin/env bash
# ADR-0057 graph READING adapter — one pod session: train on data/real, then score base vs adapter
# with and without the excerpt on repos the adapter never saw (data/eval_seen.jsonl carries the
# oracle context and the abstention rows). Writes out/arms.json and out/JOB_DONE for run_qbraid --job.
# Env: BASE, N_PER_KIND, EPOCHS, R, LR, MAXLEN, BATCH, ACCUM.
set -u
cd "$(dirname "$0")"
BASE="${BASE:-Qwen/Qwen2.5-7B-Instruct}"
N_PER_KIND="${N_PER_KIND:-60}"
EPOCHS="${EPOCHS:-1}"; R="${R:-16}"; LR="${LR:-1e-4}"; MAXLEN="${MAXLEN:-1024}"; BATCH="${BATCH:-2}"; ACCUM="${ACCUM:-8}"
T0=$(date +%s)
mkdir -p out
log() { echo "[$(date -u +%H:%M:%S)] [reading] $*"; }
elapsed_min() { echo $(( ($(date +%s) - T0) / 60 )); }
summary() {
  python3 - "$1" "$(elapsed_min)" <<'PY'
import json, sys, os
status, minutes = sys.argv[1], int(sys.argv[2])
d = {"status": status, "elapsed_min": minutes, "base": os.environ.get("BASE")}
if os.path.exists("out/real/train.manifest.json"):
    d["real"] = json.load(open("out/real/train.manifest.json"))
if os.path.exists("out/eval/results.json"):
    d["eval"] = json.load(open("out/eval/results.json"))
json.dump(d, open("out/arms.json", "w"), indent=1)
print(json.dumps({k: v for k, v in d.items() if k in ("status", "elapsed_min")}))
PY
}
finish() { summary "$1"; touch out/JOB_DONE; log "JOB_DONE ($1) after $(elapsed_min) min"; }
trap 'finish interrupted' INT TERM

log "train reading adapter ($BASE, epochs=$EPOCHS r=$R maxlen=$MAXLEN)"
if ! python3 train_lora.py --base "$BASE" --data data/real --out out/real --qlora --epochs "$EPOCHS" --lr "$LR" \
      --r "$R" --max-len "$MAXLEN" --batch "$BATCH" --grad-accum "$ACCUM" --eval-samples 6; then
  log "training FAILED"; finish train-failed; exit 1
fi
rm -rf out/real/ckpt
log "trained in $(elapsed_min) min; eval on unseen repos"
python3 eval_graph.py --base "$BASE" --adapter real=out/real/adapter \
  --evals data/eval_seen.jsonl --arms base_ctx real_ctx base_alone real_alone \
  --n-per-kind "$N_PER_KIND" --out out/eval || log "eval FAILED"
finish complete
