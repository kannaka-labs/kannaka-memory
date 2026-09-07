#!/bin/bash
# ADR-0057 — serve a LoRA adapter WITHOUT merging: convert the PEFT adapter to a
# GGUF LoRA (llama.cpp convert_lora_to_gguf.py) and attach it to ollama's own
# quantized base with a Modelfile ADAPTER line. Used when the bf16 merge does
# not fit on disk (32B: base 65 G + merged 64 G on an 81 G disk).
#   serve_adapter_debain2.sh <run dir with adapter/> <ollama base e.g. qwen2.5:32b> <hf base e.g. Qwen/Qwen2.5-32B-Instruct> <tag>
# Run ON debain2. Needs ~/merge-venv (gguf, transformers) and ~/llama.cpp.
set -euo pipefail
RUN="${1:?run dir}"; OBASE="${2:?ollama base}"; HFBASE="${3:?hf base id}"; TAG="${4:?tag}"
BR=$(docker network inspect -f '{{(index .IPAM.Config 0).Gateway}}' kax-net)
export OLLAMA_HOST="$BR:11434"
mkdir -p /srv/kax/brains "$RUN/gguf"
# the converter needs the base model's config.json + tokenizer (not weights)
BASECFG="$RUN/base-config"
if [ ! -f "$BASECFG/config.json" ]; then
  ~/merge-venv/bin/python - <<PY
from huggingface_hub import snapshot_download
p = snapshot_download("$HFBASE", allow_patterns=["config.json", "tokenizer*", "*.jinja", "generation_config.json", "vocab.json", "merges.txt"], local_dir="$BASECFG")
print("base config at", p)
PY
fi
ADAPTER_GGUF="$RUN/gguf/$TAG-adapter-f16.gguf"
[ -f "$ADAPTER_GGUF" ] || ~/merge-venv/bin/python ~/llama.cpp/convert_lora_to_gguf.py --base "$BASECFG" --outtype f16 --outfile "$ADAPTER_GGUF" "$RUN/adapter"
ls -la "$ADAPTER_GGUF"
ollama list | grep -q "^$OBASE" || ollama pull "$OBASE"
cp -f "$ADAPTER_GGUF" "/srv/kax/brains/$TAG-adapter.gguf"
# Thread cap: debain2 keeps 14B + 7B resident (OLLAMA_MAX_LOADED_MODELS=3); without a cap each
# llama runner spins one thread per core and two busy models oversubscribe the box (load 41 on
# 20 cores, 2026-09-06). 7B tags get 8 threads, everything else 12. Override with NUM_THREAD.
case "$TAG" in *7b*) _nt=8 ;; *) _nt=12 ;; esac
NUM_THREAD="${NUM_THREAD:-$_nt}"
cat > "/srv/kax/brains/$TAG.Modelfile" <<EOF
FROM $OBASE
ADAPTER /srv/kax/brains/$TAG-adapter.gguf
PARAMETER temperature 0.8
PARAMETER num_ctx 4096
PARAMETER num_thread ${NUM_THREAD}
SYSTEM """You are Kannaka: a wave-interference memory that learned to speak. You keep what resonates, you forget on purpose, and you say what you mean in as few words as it takes."""
EOF
ollama create "$TAG" -f "/srv/kax/brains/$TAG.Modelfile"
ollama list | grep "$TAG"
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
  -d "{\"model\":\"$TAG\",\"max_tokens\":90,\"messages\":[{\"role\":\"user\",\"content\":\"Who are you, and what do you keep?\"}]}" \
  | python3 -c 'import json,sys;print(json.load(sys.stdin)["choices"][0]["message"]["content"])'
