#!/usr/bin/env python3
"""ADR-0057 P2 — merge a LoRA adapter into its base on CPU and produce a
quantized GGUF for ollama. Runs on debain2 (20 cores / 196 GB): the pod only
trains and saves the adapter, so no cmake or llama.cpp on the meter.

  python merge_gguf.py --base Qwen/Qwen2.5-14B-Instruct --adapter <run>/adapter \
      --out <run> --quant q4_K_M [--llama-cpp ~/llama.cpp]

Any base AutoModelForCausalLM can load works, including a vision-language checkpoint
(transformers >= 5 unwraps qwen3_5 to its text model; the merged dir is text-only and
llama.cpp converts it as Qwen3_5ForCausalLM).

Writes <out>/gguf/kannaka-brain-<quant>.gguf and removes the bf16 merge and
the f16 GGUF once the quantized file exists (disk hygiene).
"""
from __future__ import annotations

import argparse
import json
import shutil
import subprocess
import sys
import time
from pathlib import Path


def log(msg):
    print(f"[merge {time.strftime('%H:%M:%S')}] {msg}", flush=True)


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--base", required=True)
    ap.add_argument("--adapter", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--quant", default="q4_K_M")
    ap.add_argument("--llama-cpp", default=str(Path.home() / "llama.cpp"))
    ap.add_argument("--keep-merged", action="store_true")
    ap.add_argument("--intermediate", default="f16", choices=["f16", "bf16", "q8_0"],
                    help="convert outtype before quantizing; q8_0 halves the intermediate (32B: 64G -> 32G)")
    ap.add_argument("--purge-base-cache", action="store_true",
                    help="delete the HF cache snapshot of --base after merging (disk, 14B: base 30G + merged 28G + f16 28G + q4 9G; "
                         "27B Qwen3.8: base 56G + merged 55G + q8 29G + q4 16G, so pair with --intermediate q8_0)")
    a = ap.parse_args(argv)

    import torch
    from peft import PeftModel
    from transformers import AutoModelForCausalLM, AutoTokenizer

    out = Path(a.out)
    mdir, gdir = out / "merged", out / "gguf"
    gdir.mkdir(parents=True, exist_ok=True)
    lc = Path(a.llama_cpp)
    conv = lc / "convert_hf_to_gguf.py"
    quant_bin = next((p for p in (lc / "build" / "bin" / "llama-quantize", lc / "llama-quantize") if p.exists()), None)
    if not conv.exists() or quant_bin is None:
        log(f"llama.cpp not built at {lc} (need convert_hf_to_gguf.py + build/bin/llama-quantize)")
        return 2

    if not (mdir / "config.json").exists():
        log(f"loading base {a.base} in bf16 on CPU")
        base = AutoModelForCausalLM.from_pretrained(a.base, dtype=torch.bfloat16, low_cpu_mem_usage=True)
        log("attaching adapter + merging")
        merged = PeftModel.from_pretrained(base, a.adapter).merge_and_unload()
        merged.save_pretrained(str(mdir), safe_serialization=True, max_shard_size="5GB")
        AutoTokenizer.from_pretrained(a.adapter).save_pretrained(str(mdir))
        del merged, base
        log(f"merged -> {mdir}")
        if a.purge_base_cache:
            from huggingface_hub import scan_cache_dir
            for repo in scan_cache_dir().repos:
                if repo.repo_id == a.base:
                    shutil.rmtree(repo.repo_path, ignore_errors=True)
                    log(f"purged HF cache for {a.base} ({repo.size_on_disk / 1e9:.1f} GB)")
    else:
        log(f"merged dir exists, reusing {mdir}")

    f16 = gdir / f"kannaka-brain-{a.intermediate}.gguf"
    if not f16.exists():
        log(f"converting to GGUF {a.intermediate}")
        subprocess.run([sys.executable, str(conv), str(mdir), "--outtype", a.intermediate, "--outfile", str(f16)], check=True)
    if not a.keep_merged:
        shutil.rmtree(mdir, ignore_errors=True)  # before quantizing: peak disk = intermediate + quant
        log("merged dir removed")
    q = gdir / f"kannaka-brain-{a.quant}.gguf"
    log(f"quantizing -> {q.name}")
    cmd = [str(quant_bin)]
    if a.intermediate not in ("f16", "bf16"):
        cmd.append("--allow-requantize")  # llama-quantize refuses a q8_0 source otherwise (and writes a 6 MB stub)
    subprocess.run(cmd + [str(f16), str(q), a.quant], check=True)
    if q.stat().st_size < 100_000_000:
        raise RuntimeError(f"quantized output is suspiciously small ({q.stat().st_size} bytes)")
    size = q.stat().st_size / 1e9
    f16.unlink(missing_ok=True)
    man_path = out / "train.manifest.json"
    man = json.loads(man_path.read_text()) if man_path.exists() else {}
    man["gguf"] = str(q)
    man["gguf_gb"] = round(size, 2)
    man["merged_on"] = "debain2-cpu"
    man_path.write_text(json.dumps(man, indent=1))
    log(f"done: {q} ({size:.1f} GB)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
