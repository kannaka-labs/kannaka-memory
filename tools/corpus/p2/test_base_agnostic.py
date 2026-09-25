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
    # 2026-09-25: a QLoRA manifest measured Qwen3-8B at 4.72B (4-bit weights are packed two
    # per element) and the card said "(5B)". A count far from the id's size is not trusted.
    assert bi.size_label("Qwen/Qwen3-8B", params_b=4.72) == "8B"
    assert bi.size_label("Qwen/Qwen3-8B", params_b=8.19) == "8B"
    assert bi.size_label("mystery-model", params_b=3.2) == "3B"


class _QS:
    def __init__(self, shape):
        self.shape = shape


class _P:
    def __init__(self, n, qshape=None):
        self._n = n
        if qshape is not None:
            self.quant_state = _QS(qshape)

    def numel(self):
        return self._n


def test_count_params_unpacks_4bit():
    # a 4096x4096 weight stored 4-bit reports numel 8,388,608 (two per element); the real count is 16,777,216
    packed = _P(8_388_608, qshape=(4096, 4096))
    plain = _P(4096)
    assert bi.count_params([packed, plain]) == 16_777_216 + 4096
    assert bi.count_params([plain]) == 4096


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
        rc = ph.main(["--run", str(run), "--namespace", "flaukowski", "--stage-only", "--voice-only"])
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
        (run / "served.Modelfile").write_text(SERVED_MF)
        assert ph.main(["--run", str(run), "--namespace", "flaukowski", "--version", "27b-v1", "--stage-only",
                        "--voice-only", "--modelfile", str(run / "served.Modelfile")]) == 0
        lo = (run / "publish" / "kannaka-brain-27b-v1-lora" / "README.md").read_text(encoding="utf-8")
        gg = (run / "publish" / "kannaka-brain-27b-v1-GGUF" / "README.md").read_text(encoding="utf-8")
        assert "(27B)" in lo and '"enable_thinking": false' in lo and "in_proj_qkv" in lo
        assert "q4_K_S" in gg and "Qwen3.8-27B-TURBO" in gg and "(Qwen3)" in lo

    # the card's perplexity line no longer asserts a saturation level it cannot know
    assert "saturates near 4" not in gg


SERVED_MF = """# Modelfile generated by "ollama show"
FROM /usr/share/ollama/.ollama/models/blobs/sha256-760d28bf
TEMPLATE "{{- range .Messages }}<|im_start|>{{ .Role }}
{{ .Content }}<|im_end|>
{{ end }}<|im_start|>assistant
"
SYSTEM You are Kannaka.
PARAMETER num_ctx 4096
PARAMETER num_thread 10
PARAMETER stop <|im_end|>
PARAMETER temperature 0.8
"""


def _run(td, man):
    run = Path(td)
    (run / "adapter").mkdir(); (run / "gguf").mkdir()
    (run / "adapter" / "adapter_config.json").write_text("{}")
    (run / "gguf" / "kannaka-brain-q4_K_M.gguf").write_bytes(b"\0" * 1024)
    (run / "train.manifest.json").write_text(json.dumps(man))
    return run


V2_MAN = {"base": "Qwen/Qwen3-8B", "params_b": 4.72, "train": 1590, "holdout": 111, "lora": {"r": 32, "alpha": 64},
          "epochs": 2, "lr": 1e-4, "holdout_ppl": {"before": 97.5, "after": 40.88},
          "chat_template_kwargs": {"enable_thinking": False}}
V1_MAN = {"base": "Qwen/Qwen2.5-7B-Instruct", "params_b": 7.62, "train": 551, "holdout": 57,
          "lora": {"r": 32, "alpha": 64}, "epochs": 2, "lr": 1e-4, "holdout_ppl": {"before": 78.7, "after": 4.15}}
COMPOSITION = {"parts": [{"rows": 886, "what": "Voice lines she wrote.", "targets": "her own writing"},
                         {"rows": 704, "what": "City task prompts, each used twice.",
                          "targets": "written for this training by Claude-based subagents"}],
               "holdout": "111 held-out rows (57 voice lines + 54 task prompts)"}


def test_publish_refuses_to_guess_the_corpus():
    with tempfile.TemporaryDirectory() as td:
        run = _run(td, V1_MAN)
        assert ph.main(["--run", str(run), "--stage-only"]) == 2
        assert not (run / "publish" / "kannaka-brain-8b-v1-lora" / "README.md").exists()


def test_publish_renders_composition_and_eval():
    with tempfile.TemporaryDirectory() as td:
        run = _run(td, V2_MAN)
        (run / "comp.json").write_text(json.dumps(COMPOSITION))
        (run / "eval.md").write_text("| gate | result |\n|---|---|\n| pairwise | 33 of 39 |\n\n**Losses:** none hidden.")
        (run / "served.Modelfile").write_text(SERVED_MF)
        assert ph.main(["--run", str(run), "--version", "7b-v2", "--stage-only", "--composition", str(run / "comp.json"),
                        "--eval", str(run / "eval.md"), "--modelfile", str(run / "served.Modelfile")]) == 0
        lo = (run / "publish" / "kannaka-brain-7b-v2-lora" / "README.md").read_text(encoding="utf-8")
        gg = (run / "publish" / "kannaka-brain-7b-v2-GGUF" / "README.md").read_text(encoding="utf-8")
        for card in (lo, gg):
            assert "(8B)" in card and "(5B)" not in card
            assert "## Evaluation" in card and "33 of 39" in card and "Losses" in card
            assert "57 voice lines + 54 task prompts" in card
        assert "Claude-based subagents" in lo and "1,590 training rows" in lo
        assert "1590 examples** of her own writing" not in lo


def test_publish_composition_must_add_up():
    with tempfile.TemporaryDirectory() as td:
        run = _run(td, V1_MAN)
        (run / "comp.json").write_text(json.dumps(COMPOSITION))   # 1590 rows declared, 551 trained
        assert ph.main(["--run", str(run), "--stage-only", "--composition", str(run / "comp.json")]) == 2


def test_publish_needs_served_modelfile_for_template_kwargs():
    with tempfile.TemporaryDirectory() as td:
        run = _run(td, V2_MAN)
        assert ph.main(["--run", str(run), "--version", "7b-v2", "--stage-only", "--voice-only"]) == 2
        (run / "served.Modelfile").write_text(SERVED_MF)
        assert ph.main(["--run", str(run), "--version", "7b-v2", "--stage-only", "--voice-only",
                        "--modelfile", str(run / "served.Modelfile")]) == 0
        mf = (run / "publish" / "kannaka-brain-7b-v2-GGUF" / "Modelfile").read_text()
        assert mf.startswith("FROM ./kannaka-brain-q4_K_M.gguf")
        assert "TEMPLATE" in mf and "<|im_end|>" in mf and "PARAMETER stop" in mf
        assert "num_thread" not in mf and "/usr/share/ollama" not in mf and "generated by" not in mf


def test_served_modelfile_refuses_an_adapter():
    try:
        ph.served_modelfile("FROM qwen3:8b\nADAPTER ./a.gguf\n", "x.gguf")
    except ph.CardRefused:
        return
    raise AssertionError("an ADAPTER Modelfile was accepted")


if __name__ == "__main__":
    for name, fn in sorted(globals().items()):
        if name.startswith("test_") and callable(fn):
            fn()
            print("ok  ", name)
    print("all base_info/publish_hf tests passed")
