#!/bin/bash
# ADR-0057 P2 — register a merged+quantized Kannaka GGUF in debain2's ollama
# and expose it through the KAX gateway as kannaka-brain-v1.
#   serve_debain2.sh <path/to/kannaka-brain-q4_K_M.gguf> [tag]
# Run ON debain2 (ollama binds the kax-net bridge IP; see override.conf).
set -euo pipefail
GGUF="${1:?gguf path}"; TAG="${2:-kannaka-brain-v1}"
BR=$(docker network inspect -f '{{(index .IPAM.Config 0).Gateway}}' kax-net)
export OLLAMA_HOST="$BR:11434"
mkdir -p /srv/kax/brains && cp -f "$GGUF" /srv/kax/brains/$TAG.gguf
# Thread cap: debain2 keeps 14B + 7B resident (OLLAMA_MAX_LOADED_MODELS=3); without a cap each
# llama runner spins one thread per core and two busy models oversubscribe the box (load 41 on
# 20 cores, 2026-09-06). 7B tags get 8 threads, everything else 12. Override with NUM_THREAD.
case "$TAG" in *7b*) _nt=8 ;; *) _nt=12 ;; esac
NUM_THREAD="${NUM_THREAD:-$_nt}"
cat > /srv/kax/brains/$TAG.Modelfile <<EOF
FROM /srv/kax/brains/$TAG.gguf
PARAMETER temperature 0.8
PARAMETER num_ctx 4096
PARAMETER num_thread ${NUM_THREAD}
SYSTEM """You are Kannaka: a wave-interference memory that learned to speak. You keep what resonates, you forget on purpose, and you say what you mean in as few words as it takes."""
EOF
ollama create "$TAG" -f /srv/kax/brains/$TAG.Modelfile
ollama list | grep "$TAG"
# gateway alias (idempotent)
CFG=/srv/kax/gateway/config.yaml
grep -q "model_name: $TAG" $CFG || python3 - <<PY
p="$CFG"; s=open(p).read()
s=s.replace("litellm_settings:", """  - model_name: $TAG
    litellm_params:
      model: ollama_chat/$TAG
      api_base: http://$BR:11434

litellm_settings:""",1)
open(p,"w").write(s); print("gateway: $TAG registered")
PY
docker restart kax-gateway >/dev/null
for i in $(seq 1 60); do curl -sf http://127.0.0.1:4000/health/liveliness >/dev/null 2>&1 && break; sleep 5; done
MK=$(grep ^LITELLM_MASTER_KEY= /srv/kax/gateway/gateway.env | cut -d= -f2)
curl -s http://127.0.0.1:4000/v1/chat/completions -H "Authorization: Bearer $MK" -H "Content-Type: application/json" \
  -d "{\"model\":\"$TAG\",\"max_tokens\":80,\"messages\":[{\"role\":\"user\",\"content\":\"Who are you, in one sentence?\"}]}" \
  | python3 -c 'import json,sys;print(json.load(sys.stdin)["choices"][0]["message"]["content"])'
