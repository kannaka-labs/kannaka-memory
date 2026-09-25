#!/bin/bash
# Run every judgment on the GPU judge pod through an ssh tunnel, then release the pod.
cd ~/kb2
until [ -f judge-pod.alias ]; do
  pgrep -f "[j]udge_pod.py" > /dev/null || { echo "judge pod gone before READY"; exit 1; }
  sleep 15
done
ALIAS=$(cat judge-pod.alias)
setsid -f ssh -o BatchMode=yes -o ServerAliveInterval=30 -o ExitOnForwardFailure=yes -N -L 11499:127.0.0.1:11434 "$ALIAS" < /dev/null > tunnel.log 2>&1
sleep 8
curl -s http://127.0.0.1:11499/api/tags | python3 -c "import json,sys; print('pod models:', [m['name'] for m in json.load(sys.stdin)['models']])"
echo dummy > ~/kb2/dummy.key
# grade judge (ii): generation on debain2's tmpfs ollama (CPU, the served weights), judging on the pod
python3 p2/ab_judge.py --mode grade --holdout /home/nick/kannaka-p2-runner/sft/holdout.jsonl \
  --arms kannaka-brain-7b-v1 kannaka-brain-7b-v2 --n 30 --judge qwen2.5:14b --ollama http://127.0.0.1:11499 \
  --gateway http://127.0.0.1:11445/v1 --gateway-key-file /home/nick/kb2/dummy.key \
  --out /home/nick/kb2/eval/grade-v1-v2-qwen14.json > eval/grade-v1-v2-qwen14.log 2>&1 &
GRADE=$!
G=/home/nick/kb2/eval/gen-v1-v2.json
T=/home/nick/kb2/heldout_tasks.jsonl
for J in qwen2.5:14b hf.co/unsloth/gemma-4-26B-A4B-it-GGUF:UD-Q4_K_M; do
  tag=$( [ "$J" = "qwen2.5:14b" ] && echo qwen14 || echo gemma4 )
  for run in "cal-gold-v1:__gold__ kannaka-brain-7b-v1" "cal-v1-foreign:kannaka-brain-7b-v1 __foreign__" "gate-v1-v2:kannaka-brain-7b-v1 kannaka-brain-7b-v2"; do
    name=${run%%:*}; arms=${run#*:}
    python3 p2/task_judge.py --tasks $T --arms $arms --replies-from $G --gen-url http://127.0.0.1:11445/v1 \
      --judge "$J" --judge-api ollama --judge-url http://127.0.0.1:11499 --out eval/$name-$tag.json > eval/$name-$tag.log 2>&1
    echo "$name-$tag: $(grep -o '"win_share[^,]*' eval/$name-$tag.log | tr '\n' ' ') consistency $(grep -o '"position_consistency": [0-9.]*' eval/$name-$tag.log)"
  done
done
wait $GRADE
tail -12 eval/grade-v1-v2-qwen14.log
touch ~/kb2/judge-pod.STOP
echo JUDGE_ALL_DONE
