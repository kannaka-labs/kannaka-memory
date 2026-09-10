"""Base-agnostic helpers shared by train_lora.py, merge_gguf.py and publish_hf.py.

Pure Python, no torch: importable by the tests and by the publish step on a
machine that has neither. Everything the pipeline used to assume about the
base (Qwen2.5-14B, q/k/v/o/gate/up/down, "14B" in the card) lives here and is
derived from the base id or the run manifest instead.
"""
from __future__ import annotations

import re

# One regex for every base the pipeline has trained or plans to train:
#   Qwen2.5 (dense):     model.layers.N.self_attn.q_proj ... mlp.down_proj
#   Qwen3.5 / 3.8 (hybrid, VL-wrapped): model.language_model.layers.N.linear_attn.in_proj_qkv,
#                        .in_proj_z, .out_proj on 3 of 4 layers; self_attn.q/k/v/o on the 4th;
#                        mlp.gate/up/down on all. in_proj_a / in_proj_b are num_v_heads wide
#                        (a few KB) and are left alone.
# Anything under model.visual.* (vision blocks use attn.qkv / attn.proj) and mtp.* never matches,
# so a LoRA on a VL checkpoint touches the language model only.
LORA_TARGET_LEAVES = ("q_proj", "k_proj", "v_proj", "o_proj", "gate_proj", "up_proj", "down_proj",
                      "in_proj_qkv", "in_proj_z", "out_proj")
# Anchored on the language model path: `model.layers.N` (dense) or `model.language_model.layers.N`
# (VL-wrapped), optionally under PEFT's `base_model.model.` wrapper. `mtp.layers.N` and
# `model.visual.blocks.N` never match.
LORA_TARGET_REGEX = (r"^(base_model\.model\.)?model\.(language_model\.)?layers\.\d+\.(self_attn|linear_attn|mlp)\.("
                     + "|".join(LORA_TARGET_LEAVES) + r")$")


def lora_targets(module_names) -> list[str]:
    """The module names LORA_TARGET_REGEX selects, for logging and tests."""
    rx = re.compile(LORA_TARGET_REGEX)
    return [n for n in module_names if rx.match(n)]


def size_label(base: str, params_b: float | None = None) -> str:
    """'7B', '14B', '27B' — from the measured parameter count when the manifest has one,
    else from the base id. The card must never say 14B because the template did."""
    if params_b:
        return f"{round(params_b):d}B"
    m = re.search(r"(\d+(?:\.\d+)?)\s*[bB](?![a-zA-Z])", base)
    return f"{m.group(1)}B" if m else "?B"


def base_short(base: str) -> str:
    """'Qwen2.5-14B-Instruct' from 'Qwen/Qwen2.5-14B-Instruct'; the DavidAU id keeps its full tail."""
    return base.split("/", 1)[1] if "/" in base else base


def base_family(base: str) -> str:
    """The licence-bearing family named in the card's licence line."""
    b = base.lower()
    if "qwen3" in b:
        return "Qwen3"
    if "qwen2" in b:
        return "Qwen2.5"
    return base_short(base).split("-")[0]
