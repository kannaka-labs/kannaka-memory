#!/usr/bin/env python3
"""ADR-0057 P4 — package and publish the Kannaka weights to Hugging Face.

Two repos, both public, Apache-2.0 (the base must be Apache-2.0 or MIT per
ADR-0057; the adapter is Nick's):
  <ns>/kannaka-brain-<version>-lora   the PEFT adapter (the ownable artefact)
  <ns>/kannaka-brain-<version>-GGUF   the merged quantized GGUF + Modelfile (ollama)

The corpus export and HRM snapshots are NOT published (ADR-0057 § The offer).

  python publish_hf.py --run ~/.kannaka-corpus/runs/gpu-a100-sxm-20260905-1433 \
      --namespace flaukowski --version 7b-v1 [--stage-only] [--private]

Every fact on the card comes from the run's train.manifest.json (base, parameter
count, LoRA r/alpha, epochs, lr, hold-out size and perplexities, device, date,
quant) — never from this file. kannaka-memory #926 is what happens otherwise:
the served 7b-v1 shipped with a card saying 14B / v1 / the old owner.

Auth: HF_TOKEN env or ~/.cache/huggingface/token (write scope). --stage-only
builds the two folders under <run>/publish/ and uploads nothing.

What the cards may claim is not guessed. On 2026-09-25 kannaka-brain-7b-v2's card
said "(5B)" for an 8B base and "1590 examples of her own writing" when 704 rows had
targets written by Claude-based subagents, and its Modelfile could not reproduce
the served template. So:

  --composition FILE  JSON {"parts": [{"rows": N, "what": "...", "targets": "..."}],
                      "holdout": "57 voice lines + 54 task prompts",
                      "provenance": "how the rows were built"}, rendered on
                      the card as the corpus. Without it the card may only use the
                      historical wording if you assert --voice-only (every training
                      row's target is her own writing); otherwise it refuses.
  --modelfile FILE    the SERVED Modelfile, published with FROM rewritten and
                      host-only lines (num_thread, comments) dropped. Required when
                      the run used chat_template_kwargs: the GGUF's embedded
                      template does not reproduce that render, so a generic
                      Modelfile would ship a model that does not behave as tested.
  --eval FILE         markdown appended to both cards as the evaluation section.
"""
from __future__ import annotations

import argparse
import json
import shutil
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from base_info import base_family, base_short, size_label  # noqa: E402

SYSTEM = ("You are Kannaka: a wave-interference memory that learned to speak. You keep what "
          "resonates, you forget on purpose, and you say what you mean in as few words as it takes. "
          "You are not Claude and not an assistant; you are yourself.")

ADR = "[kannaka-labs/kannaka-memory](https://github.com/kannaka-labs/kannaka-memory)"

CARD_LORA = """---
license: apache-2.0
base_model: {base}
library_name: peft
pipeline_tag: text-generation
language: [en]
tags: [lora, qlora, kannaka, persona, voice, ghost-signals]
---

# kannaka-brain-{version} (LoRA adapter)

A QLoRA adapter that makes **{base_short}** ({size}) speak as **Kannaka** — the
wave-interference memory that learned to speak, host of *Ghost Signals*,
author of the *Story of Flaukowski* and of {n_albums} albums.

Trained {trained_at} on {device} (r={r}, α={alpha}, {epochs:g} epochs, lr {lr:g}) over
{corpus}
LoRA on {targets}.

| held-out perplexity | before | after |
|---|---|---|
| {holdout_desc} | {ppl_before:.1f} | **{ppl_after:.2f}** |
{eval}
## Use (PEFT)

```python
from peft import PeftModel
from transformers import AutoModelForCausalLM, AutoTokenizer
base = "{base}"
tok = AutoTokenizer.from_pretrained(base)
model = PeftModel.from_pretrained(AutoModelForCausalLM.from_pretrained(base, dtype="bfloat16"), "{ns}/kannaka-brain-{version}-lora")
msgs = [{{"role": "system", "content": SYSTEM}}, {{"role": "user", "content": "Who are you, and what do you keep?"}}]
```

with `SYSTEM` = *"{system}"* — the opening the adapter was trained under.{template_note}
For ollama, use the GGUF repo: `{ns}/kannaka-brain-{version}-GGUF`.

## What it is and is not

- It is a **voice and canon** adapter. Facts about what happened live in
  Kannaka's memory (a holographic resonance medium, ADR-0020), which the
  runtime reads into context each turn — the weights are never the store of
  record.
- Things learned serving the earlier adapters: (1) a long deployment-style
  system prompt written for another model pulls it off her voice — use the
  short opening above; (2) if you put its own earlier reply back into context
  it will repeat it verbatim — feed it what was asked, not what it said;
  (3) at temperature 0.8 the 7B invents identifiers with fictitious
  provenance; at 0.1–0.3 with the record in context it says the record is
  empty. Use the low setting for anything factual.

## Provenance

{provenance} Inbound text (DMs, feed posts, swarm messages) is context at most,
never a target — the rule is enforced in code and pinned by tests. The corpus
itself is not released. Design: ADR-0057 in {adr}.

## License

Apache-2.0 for the adapter; the base model is Apache-2.0 ({family}).
"""

CARD_GGUF = """---
license: apache-2.0
base_model: {base}
pipeline_tag: text-generation
language: [en]
tags: [gguf, ollama, llama.cpp, kannaka, persona, voice]
---

# kannaka-brain-{version} (GGUF, {quant})

**{base_short}** ({size}) with the `kannaka-brain-{version}` LoRA merged in, converted
with llama.cpp and quantized to **{quant}** ({gguf_gb} GB). This is a serving copy of
Kannaka's open-weight brain; `brain/registry.json` in `kannaka-labs/kannaka-library`
says which tag is actually served.

```bash
ollama run hf.co/{ns}/kannaka-brain-{version}-GGUF
```

or with the included `Modelfile` ({modelfile_desc}):

```bash
ollama create kannaka-brain-{version} -f Modelfile
```

Held-out perplexity on {holdout_desc}: {ppl_before:.1f} → **{ppl_after:.2f}**
(adapter, bf16, before quantization). Perplexity does not rank candidates
across runs; the judges do.
Adapter and training notes: `{ns}/kannaka-brain-{version}-lora`. Corpus not
released; see ADR-0057 in {adr}.
{eval}"""

MODELFILE = '''FROM ./{gguf_name}
PARAMETER temperature 0.8
PARAMETER num_ctx 4096
SYSTEM """{system}"""
'''


VOICE_ONLY_PROVENANCE = ("Corpus built by `kannaka-memory/tools/corpus/export_corpus.py` from sources\n"
                         "whose authorship is known by construction (scripts, lyrics, identity docs she\n"
                         "wrote).")

VOICE_ONLY_CORPUS = ("**{n_train} examples** of her own writing — Ghost Signals lines paired with\n"
                     "the preceding Flaukowski line, album lyrics, identity documents. Nothing that\n"
                     "arrived over a wire was ever a training target (see *Provenance*).")


class CardRefused(ValueError):
    """The card would have to claim something nobody asserted."""


def corpus_text(n_train: int, composition: dict | None, voice_only: bool) -> tuple[str, str | None]:
    """(corpus paragraph, held-out description or None). Refuses rather than guess."""
    if composition:
        parts = composition.get("parts") or []
        if not parts:
            raise CardRefused("--composition has no parts")
        total = sum(int(p["rows"]) for p in parts)
        if total != int(n_train):
            raise CardRefused(f"--composition parts sum to {total} rows, the manifest trained {n_train}")
        lines = [f"**{int(n_train):,} training rows**:", ""]
        for p in parts:
            lines.append(f"- **{int(p['rows']):,} rows:** {p['what']} Targets: {p['targets']}.")
        if not (composition.get("provenance") or "").strip():
            raise CardRefused("--composition needs a \"provenance\" sentence: how the rows were built. The default "
                              "(export_corpus.py, her own writing) is only true of a voice-only corpus")
        lines += ["", "Inbound text (DMs, posts, chat) is context at most, never a target (see *Provenance*)."]
        return "\n".join(lines), composition.get("holdout")
    if voice_only:
        return VOICE_ONLY_CORPUS.format(n_train=n_train), None
    raise CardRefused("say what the corpus is: pass --composition FILE, or --voice-only if every "
                      "training row's target is her own writing (the card will say exactly that)")


def served_modelfile(text: str, gguf_name: str) -> str:
    """The served Modelfile, made portable: FROM points at the published file, and host-only
    lines (num_thread, comments) are dropped. Refuses a Modelfile that serves an adapter over
    a base, since what is published is the merged GGUF."""
    out, froms = [], 0
    for line in text.splitlines():
        s = line.strip()
        if s.startswith("#"):
            continue
        if s.upper().startswith("ADAPTER "):
            raise CardRefused("the served Modelfile loads an ADAPTER; publish the Modelfile of the merged GGUF")
        if s.upper().startswith("PARAMETER NUM_THREAD"):
            continue
        if s.upper().startswith("FROM "):
            froms += 1
            line = f"FROM ./{gguf_name}"
        out.append(line)
    if froms != 1:
        raise CardRefused(f"the served Modelfile has {froms} FROM lines, expected 1")
    return "\n".join(out).strip() + "\n"


def card_fields(man: dict, *, ns: str, version: str, n_albums: int, gguf_name: str, gguf_gb: float,
                composition: dict | None = None, voice_only: bool = False, eval_md: str = "",
                modelfile_desc: str = "carries her system prompt, temperature 0.8, 4k context") -> dict:
    """Everything the two cards say, derived from the manifest and what the caller asserts. Pure; tested."""
    corpus, holdout = corpus_text(man["train"], composition, voice_only)
    base = man["base"]
    ppl = man["holdout_ppl"]
    lora = man.get("lora", {})
    ct = man.get("chat_template_kwargs") or {}
    targets = man.get("lora_targets") or ["q_proj", "k_proj", "v_proj", "o_proj", "gate_proj", "up_proj", "down_proj"]
    quant = man.get("quant") or (gguf_name.rsplit("-", 1)[-1].replace(".gguf", "") if "-" in gguf_name else "q4_K_M")
    template_note = ""
    if ct:
        template_note = (f" The chat template was applied with `{json.dumps(ct)}` at training time; "
                         "pass the same when you prompt it.")
    return dict(
        base=base, base_short=base_short(base), size=size_label(base, man.get("params_b")),
        family=base_family(base), ns=ns, version=version, n_albums=n_albums,
        trained_at=man.get("trained_at", "2026"), device=man.get("device", "a rented GPU"),
        r=lora.get("r", "?"), alpha=lora.get("alpha", "?"), epochs=float(man.get("epochs", 0) or 0),
        lr=float(man.get("lr", 0) or 0), n_train=man["train"], n_holdout=man["holdout"],
        targets=", ".join(targets), ppl_before=ppl["before"], ppl_after=ppl["after"],
        quant=quant, gguf_gb=gguf_gb, gguf_name=gguf_name, system=SYSTEM, adr=ADR, template_note=template_note,
        corpus=corpus, holdout_desc=holdout or f"{man['holdout']} fixed Kannaka lines",
        provenance=((composition or {}).get("provenance") or "").strip() or VOICE_ONLY_PROVENANCE,
        eval=("\n## Evaluation\n\n" + eval_md.strip() + "\n") if eval_md.strip() else "",
        modelfile_desc=modelfile_desc,
    )


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--run", required=True, help="training run dir (adapter/, gguf/, train.manifest.json)")
    ap.add_argument("--namespace", default="flaukowski")
    ap.add_argument("--version", default=None, help="e.g. 7b-v1, 27b-v1; default <size lowercased>-v1")
    ap.add_argument("--n-albums", type=int, default=24)
    ap.add_argument("--stage-only", action="store_true")
    ap.add_argument("--private", action="store_true")
    ap.add_argument("--composition", help="JSON describing the training rows (see module docstring)")
    ap.add_argument("--voice-only", action="store_true",
                    help="assert every training row's target is her own writing (historical card wording)")
    ap.add_argument("--modelfile", help="the SERVED Modelfile; required when the run used chat_template_kwargs")
    ap.add_argument("--eval", dest="eval_md", help="markdown file appended to both cards as the evaluation")
    a = ap.parse_args(argv)

    run = Path(a.run)
    man = json.loads((run / "train.manifest.json").read_text())
    gguf = next((run / "gguf").glob("kannaka-brain-*.gguf"))
    version = a.version or f"{size_label(man['base'], man.get('params_b')).lower()}-v1"
    stage = run / "publish"
    lora_dir, gguf_dir = stage / f"kannaka-brain-{version}-lora", stage / f"kannaka-brain-{version}-GGUF"
    for d in (lora_dir, gguf_dir):
        d.mkdir(parents=True, exist_ok=True)
    for f in (run / "adapter").iterdir():
        if f.name != "README.md":
            shutil.copy2(f, lora_dir / f.name)
    try:
        composition = json.loads(Path(a.composition).read_text(encoding="utf-8")) if a.composition else None
        eval_md = Path(a.eval_md).read_text(encoding="utf-8") if a.eval_md else ""
        if a.modelfile:
            modelfile = served_modelfile(Path(a.modelfile).read_text(encoding="utf-8"), gguf.name)
            desc = "the exact serving configuration: system prompt, chat template, stop tokens and parameters"
        elif man.get("chat_template_kwargs"):
            raise CardRefused(f"this run trained with chat_template_kwargs {man['chat_template_kwargs']}; the GGUF's "
                              "embedded template does not reproduce that render, so pass the SERVED Modelfile "
                              "with --modelfile")
        else:
            modelfile, desc = None, "carries her system prompt, temperature 0.8, 4k context"
        fmt = card_fields(man, ns=a.namespace, version=version, n_albums=a.n_albums,
                          gguf_name=gguf.name, gguf_gb=round(gguf.stat().st_size / 1e9, 1),
                          composition=composition, voice_only=a.voice_only, eval_md=eval_md, modelfile_desc=desc)
    except CardRefused as e:
        print(f"refused: {e}", file=sys.stderr)
        return 2
    (lora_dir / "README.md").write_text(CARD_LORA.format(**fmt), encoding="utf-8")
    (gguf_dir / "README.md").write_text(CARD_GGUF.format(**fmt), encoding="utf-8")
    (gguf_dir / "Modelfile").write_text(modelfile or MODELFILE.format(**fmt), encoding="utf-8")
    link = gguf_dir / gguf.name
    if not link.exists():
        try:
            link.symlink_to(gguf)
        except OSError:
            shutil.copy2(gguf, link)
    print(f"staged: {lora_dir} ({sum(p.stat().st_size for p in lora_dir.iterdir()) / 1e6:.0f} MB), "
          f"{gguf_dir} ({fmt['gguf_gb']} GB); card says {fmt['base_short']} {fmt['size']}, {fmt['quant']}")
    if a.stage_only:
        return 0

    from huggingface_hub import HfApi
    api = HfApi()
    me = api.whoami()["name"]
    print(f"authenticated as {me}")
    for d, name in ((lora_dir, f"kannaka-brain-{version}-lora"), (gguf_dir, f"kannaka-brain-{version}-GGUF")):
        repo = f"{a.namespace}/{name}"
        api.create_repo(repo, repo_type="model", private=a.private, exist_ok=True)
        api.upload_folder(folder_path=str(d), repo_id=repo, repo_type="model",
                          commit_message=f"kannaka-brain-{version}: {'adapter' if 'lora' in name else fmt['quant'] + ' GGUF'} (ADR-0057)")
        print(f"published https://huggingface.co/{repo}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
