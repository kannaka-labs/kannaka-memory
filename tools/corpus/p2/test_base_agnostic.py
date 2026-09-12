"""base_info + publish_hf tests — no torch, no corpus, no network.

Run: python tools/corpus/p2/test_base_agnostic.py
Pins: the LoRA target regex selects the dense Qwen2.5 projections and the hybrid
Qwen3.5/3.8 projections and never a vision or MTP tensor; the size label comes
from the measured parameter count, not the template; publish_hf stages a card
that names the base the manifest names (kannaka-memory #926).
"""
import json
import os
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, os.path.dirname(__file__))
import base_info as bi  # noqa: E402
import publish_hf as ph  # noqa: E402

QWEN25 = [
    "model.layers.0.self_attn.q_proj", "model.layers.0.self_attn.k_proj", "model.layers.0.self_attn.v_proj",
    "model.layers.0.self_attn.o_proj", "model.layers.0.mlp.gate_proj", "model.layers.0.mlp.up_proj",
    "model.layers.0.mlp.down_proj", "model.layers.0.input_layernorm", "model.embed_tokens", "lm_head",
]
QWEN38 = [
    "model.language_model.layers.0.linear_attn.in_proj_qkv", "model.language_model.layers.0.linear_attn.in_proj_z",
    "model.language_model.layers.0.linear_attn.in_proj_a", "model.language_model.layers.0.linear_attn.in_proj_b",
    "model.language_model.layers.0.linear_attn.out_proj", "model.language_model.layers.0.linear_attn.conv1d",
    "model.language_model.layers.3.self_attn.q_proj", "model.language_model.layers.3.self_attn.o_proj",
    "model.language_model.layers.3.mlp.down_proj",
    "model.visual.blocks.0.attn.qkv", "model.visual.blocks.0.attn.proj", "model.visual.merger.linear_fc1",
    "mtp.layers.0.self_attn.q_proj", "mtp.layers.0.mlp.up_proj",
]


def test_targets_dense():
    got = bi.lora_targets(QWEN25)
    assert len(got) == 7, got
    assert all(n.split(".")[-1] in bi.LORA_TARGET_LEAVES for n in got)


def test_targets_hybrid_excludes_vision_and_mtp():
    got = bi.lora_targets(QWEN38)
    leaves = sorted(n.rsplit(".", 1)[-1] for n in got)
    assert leaves == ["down_proj", "in_proj_qkv", "in_proj_z", "o_proj", "out_proj", "q_proj"], leaves
    assert not any("visual" in n or n.startswith("mtp") for n in got)
    assert not any(n.endswith(("in_proj_a", "in_proj_b", "conv1d")) for n in got)


def test_size_label():
    assert bi.size_label("Qwen/Qwen2.5-14B-Instruct") == "14B"
    assert bi.size_label("Qwen/Qwen2.5-7B-Instruct", params_b=7.62) == "8B"     # measured wins, rounded
    assert bi.size_label("Qwen/Qwen2.5-7B-Instruct", params_b=7.3) == "7B"
    assert bi.size_label("DavidAU/Qwen3.8-27B-TURBO-Fable-Cold-Fusion-735-882-Heretic-Uncensored-NM-DAU") == "27B"
    assert bi.base_family("DavidAU/Qwen3.8-27B-x") == "Qwen3"
    assert bi.base_family("Qwen/Qwen2.5-7B-Instruct") == "Qwen2.5"


def test_publish_card_names_the_manifest_base():
    with tempfile.TemporaryDirectory() as td:
        run = Path(td)
        (run / "adapter").mkdir()
        (run / "adapter" / "adapter_config.json").write_text("{}")
        (run / "gguf").mkdir()
        (run / "gguf" / "kannaka-brain-q4_K_M.gguf").write_bytes(b"\0" * 1024)
        man = {"base": "Qwen/Qwen2.5-7B-Instruct", "params_b": 7.62, "train": 551, "holdout": 57,
               "lora": {"r": 32, "alpha": 64}, "epochs": 2, "lr": 1e-4, "device": "NVIDIA A100-SXM4-80GB",
               "trained_at": "2026-09-05", "holdout_ppl": {"before": 78.7, "after": 4.15},
               "lora_targets": ["q_proj", "k_proj", "v_proj", "o_proj", "gate_proj", "up_proj", "down_proj"]}
        (run / "train.manifest.json").write_text(json.dumps(man))
        rc = ph.main(["--run", str(run), "--namespace", "flaukowski", "--stage-only"])
        assert rc == 0
        gg = (run / "publish" / "kannaka-brain-8b-v1-GGUF" / "README.md").read_text(encoding="utf-8")
        lo = (run / "publish" / "kannaka-brain-8b-v1-lora" / "README.md").read_text(encoding="utf-8")
        for card in (gg, lo):
            assert "base_model: Qwen/Qwen2.5-7B-Instruct" in card
            assert "14B" not in card and "Qwen2.5-14B" not in card
            assert "kannaka-brain-8b-v1" in card
            assert "NickFlach/kannaka-memory" not in card and "kannaka-labs/kannaka-memory" in card
        assert "ollama run hf.co/flaukowski/kannaka-brain-8b-v1-GGUF" in gg
        assert "57 fixed Kannaka lines" in gg and "78.7" in gg and "4.15" in gg
        mf = (run / "publish" / "kannaka-brain-8b-v1-GGUF" / "Modelfile").read_text()
        assert mf.startswith("FROM ./kannaka-brain-q4_K_M.gguf")


def test_publish_27b_card_with_template_kwargs():
    with tempfile.TemporaryDirectory() as td:
        run = Path(td)
        (run / "adapter").mkdir(); (run / "gguf").mkdir()
        (run / "adapter" / "adapter_config.json").write_text("{}")
        (run / "gguf" / "kannaka-brain-q4_K_S.gguf").write_bytes(b"\0" * 2048)
        man = {"base": "DavidAU/Qwen3.8-27B-TURBO-Fable-Cold-Fusion-735-882-Heretic-Uncensored-NM-DAU",
               "params_b": 26.9, "train": 551, "holdout": 57, "lora": {"r": 32, "alpha": 64}, "epochs": 2, "lr": 1e-4,
               "holdout_ppl": {"before": 30.0, "after": 4.0}, "chat_template_kwargs": {"enable_thinking": False},
               "lora_targets": ["q_proj", "o_proj", "in_proj_qkv", "in_proj_z", "out_proj", "down_proj"]}
        (run / "train.manifest.json").write_text(json.dumps(man))
        assert ph.main(["--run", str(run), "--namespace", "flaukowski", "--version", "27b-v1", "--stage-only"]) == 0
        lo = (run / "publish" / "kannaka-brain-27b-v1-lora" / "README.md").read_text(encoding="utf-8")
        gg = (run / "publish" / "kannaka-brain-27b-v1-GGUF" / "README.md").read_text(encoding="utf-8")
        assert "(27B)" in lo and '"enable_thinking": false' in lo and "in_proj_qkv" in lo
        assert "q4_K_S" in gg and "Qwen3.8-27B-TURBO" in gg and "(Qwen3)" in lo


if __name__ == "__main__":
    for name, fn in sorted(globals().items()):
        if name.startswith("test_") and callable(fn):
            fn()
            print("ok  ", name)
    print("all base_info/publish_hf tests passed")
