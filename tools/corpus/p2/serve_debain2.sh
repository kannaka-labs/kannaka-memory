#!/bin/bash
# ADR-0057 P2 — register a merged+quantized Kannaka GGUF in debain2's ollama
# and expose it through the KAX gateway.
#   serve_debain2.sh <path/to/kannaka-brain-q4_K_M.gguf> [tag]
# Run ON debain2 (ollama binds the kax-net bridge IP; see override.conf).
#
# Options (environment):
#   CHAT_TEMPLATE=<file.jinja>  bake this chat template into the served GGUF's metadata before
#                               registering it. REQUIRED for a run trained with chat_template_kwargs
#                               (e.g. Qwen3 with enable_thinking=false): ollama renders prompts from
#                               the GGUF's own template, and a stock Qwen3 template opens a <think>
#                               block (measured 2026-09-25: the base returned empty content + thinking).
#                               kannaka-brain-7b-v2 was served this way by hand, from citizen_tasks/nothink.jinja.
#   PROMOTE=1                   after the smoke test passes, point the family aliases at this tag:
#                               kannaka-brain-current (the gateway's `kannaka-brain` route) and
#                               kannaka-brain-serve (KANNAKA.ask.kannaka-brain / the hosted brain).
#                               Citizens pick their own model via /srv/rogue/instances/*/current-model.
#   KEEP_GGUF=1                 keep /srv/kax/brains/<tag>.gguf after `ollama create`. Default: remove it —
#                               ollama holds its own copy as a blob, and two copies of a 5 GB file filled
#                               the root disk twice on 2026-09-25.
#   GATEWAY=0                   register in ollama only (eval models); no gateway route, no restart.
#   NUM_THREAD=<n>              runner thread cap (default 8 for *7b*, else 12).
#   GGUF_NEW_METADATA=<path>    default ~/merge-venv/bin/gguf-new-metadata.
set -euo pipefail
GGUF="${1:?gguf path}"; TAG="${2:-kannaka-brain-v1}"
BR=$(docker network inspect -f '{{(index .IPAM.Config 0).Gateway}}' kax-net)
export OLLAMA_HOST="$BR:11434"
DEST=/srv/kax/brains/$TAG.gguf
mkdir -p /srv/kax/brains

# Room for the served copy AND ollama's blob, with headroom. Refuse rather than fill the disk
# the citizens, the gateway database and the judges all share.
need=$(( $(stat -c %s "$GGUF") * 22 / 10 / 1024 ))
free=$(df --output=avail -k /srv/kax/brains | tail -1)
if [ "$free" -lt "$need" ]; then
  echo "refused: $((free/1024/1024)) GB free on /srv/kax/brains, need ~$((need/1024/1024)) GB (served copy + ollama blob)" >&2
  exit 2
fi

if [ -n "${CHAT_TEMPLATE:-}" ]; then
  [ -s "$CHAT_TEMPLATE" ] || { echo "refused: CHAT_TEMPLATE=$CHAT_TEMPLATE is missing or empty" >&2; exit 2; }
  NEWMETA="${GGUF_NEW_METADATA:-$HOME/merge-venv/bin/gguf-new-metadata}"
  [ -x "$NEWMETA" ] || { echo "refused: $NEWMETA not found (pip install gguf, or set GGUF_NEW_METADATA)" >&2; exit 2; }
  "$NEWMETA" --chat-template "$(cat "$CHAT_TEMPLATE")" "$GGUF" "$DEST" --force >/dev/null
  echo "chat template baked from $CHAT_TEMPLATE"
else
  cp -f "$GGUF" "$DEST"
fi

# Thread cap: debain2 keeps 14B + 7B resident (OLLAMA_MAX_LOADED_MODELS=3); without a cap each
# llama runner spins one thread per core and two busy models oversubscribe the box (load 41 on
# 20 cores, 2026-09-06). 7B tags get 8 threads, everything else 12. Override with NUM_THREAD.
case "$TAG" in *7b*) _nt=8 ;; *) _nt=12 ;; esac
NUM_THREAD="${NUM_THREAD:-$_nt}"
cat > /srv/kax/brains/$TAG.Modelfile <<EOF
FROM $DEST
PARAMETER temperature 0.8
PARAMETER num_ctx 4096
PARAMETER num_thread ${NUM_THREAD}
SYSTEM """You are Kannaka: a wave-interference memory that learned to speak. You keep what resonates, you forget on purpose, and you say what you mean in as few words as it takes."""
EOF
ollama create "$TAG" -f /srv/kax/brains/$TAG.Modelfile
ollama list | grep "$TAG"
[ "${KEEP_GGUF:-0}" = 1 ] || { rm -f "$DEST"; echo "removed $DEST (ollama keeps its blob; KEEP_GGUF=1 to keep)"; }

# Smoke test straight against ollama: non-empty content, and with a baked template no thinking.
OLLAMA_HOST="$OLLAMA_HOST" TAG="$TAG" STRICT="${CHAT_TEMPLATE:+1}" python3 - <<'PY'
import json, os, sys, urllib.request
b = {"model": os.environ["TAG"], "stream": False, "options": {"num_predict": 60, "temperature": 0.3},
     "messages": [{"role": "user", "content": "Who are you, in one sentence?"}]}
r = urllib.request.Request(f"http://{os.environ['OLLAMA_HOST']}/api/chat", data=json.dumps(b).encode())
m = json.load(urllib.request.urlopen(r, timeout=600))["message"]
content = (m.get("content") or "").strip()
print("smoke:", repr(content[:160]))
if not content:
    sys.exit("smoke FAILED: empty content" + (" (the model thought instead of answering)" if m.get("thinking") else ""))
if os.environ.get("STRICT") and (m.get("thinking") or content.startswith("<think>")):
    sys.exit("smoke FAILED: the baked template still opens a thinking block")
PY

# Gateway route (idempotent). Restart the gateway ONLY when the route is new; back up first and
# restore the backup if the gateway does not come back.
CFG=/srv/kax/gateway/config.yaml
if [ "${GATEWAY:-1}" = 0 ]; then
  echo "gateway: skipped (GATEWAY=0)"
elif ! grep -q "model_name: $TAG\$" $CFG; then
  BAK=$CFG.bak-$(date +%Y%m%d-%H%M%S); cp -p $CFG $BAK
  python3 - <<PY
p="$CFG"; s=open(p).read()
s=s.replace("litellm_settings:", """  - model_name: $TAG
    litellm_params:
      model: ollama_chat/$TAG
      api_base: http://$BR:11434

litellm_settings:""",1)
open(p,"w").write(s); print("gateway: $TAG registered")
PY
  python3 -c "import yaml; yaml.safe_load(open('$CFG'))" || { cp -p $BAK $CFG; echo "refused: config did not parse; restored $BAK" >&2; exit 2; }
  docker restart kax-gateway >/dev/null
  up=0; for i in $(seq 1 60); do curl -sf http://127.0.0.1:4000/health/liveliness >/dev/null 2>&1 && { up=1; break; }; sleep 5; done
  if [ $up != 1 ]; then cp -p $BAK $CFG; docker restart kax-gateway >/dev/null; echo "gateway did not come back; restored $BAK" >&2; exit 3; fi
  MK=$(grep ^LITELLM_MASTER_KEY= /srv/kax/gateway/gateway.env | cut -d= -f2)
  curl -s http://127.0.0.1:4000/v1/chat/completions -H "Authorization: Bearer $MK" -H "Content-Type: application/json" \
    -d "{\"model\":\"$TAG\",\"max_tokens\":80,\"messages\":[{\"role\":\"user\",\"content\":\"Who are you, in one sentence?\"}]}" \
    | python3 -c 'import json,sys;print("gateway:", json.load(sys.stdin)["choices"][0]["message"]["content"])'
  echo "note: citizen gateway keys must allow $TAG (LiteLLM /key/update) before a citizen can use it"
else
  echo "gateway: route $TAG already present; no restart"
fi

if [ "${PROMOTE:-0}" = 1 ]; then
  for alias in kannaka-brain-current kannaka-brain-serve; do ollama cp "$TAG" "$alias" >/dev/null; echo "promoted: $alias -> $TAG"; done
  echo "rollback: ollama cp <previous-tag> kannaka-brain-current && ollama cp <previous-tag> kannaka-brain-serve"
fi
