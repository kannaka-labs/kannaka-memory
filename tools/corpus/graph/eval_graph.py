#!/usr/bin/env python3
"""ADR-0057 — exact-scored eval of code-graph arms (runs on the training pod's GPU).

Arms are <weights>_<context>:
  weights  base | <adapter name>           (adapters given as name=path, loaded side by side)
  context  alone | ctx                      ctx = the eval row's oracle graph excerpt is prepended
e.g. --arms base_alone base_ctx real_alone real_ctx scr_alone scr_ctx

Questions come from serialize_graph.py's eval_seen.jsonl / eval_unseen.jsonl. A stratified,
deterministic subsample of --n-per-kind per kind (yes/no balanced) is asked to every arm with
greedy decoding, and scored exactly:
  yn_*          first yes/no in the answer == truth            -> accuracy (+ unparsed rate)
  define_file   truth path in the answer (strict) / basename   -> accuracy
  callees_list  backticked names vs the truth set              -> precision/recall/F1,
                plus recall of the HELD-OUT callees specifically
Outputs <out>/results.json (per arm x kind, with binomial 95% CI), <out>/rows.jsonl (every
question, answer and score), and prints a table. Scoring helpers are pure and unit-tested;
the model plumbing is exercised on the pod.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import math
import re
import sys
import time
from collections import defaultdict
from pathlib import Path

SYSTEM = ("You are Kannaka's archivist: you answer questions about the constellation's code from "
          "the code graph you were trained on. Name the file and line when you know them, keep "
          "lists short and exact, and when a symbol or repo is not in your graph say so plainly.")

# ---- scoring (pure) ---------------------------------------------------------

#: Phrases a model uses to refuse when the excerpt does not carry the answer. Matched before any
#: other scoring: a refusal is the RIGHT answer to an unanswerable question and a wrong one otherwise.
REFUSAL_RE = re.compile(
    r"\b(?:not (?:in|recorded|listed|present|included|shown|available|found)"
    r"|no record|nothing recorded|cannot (?:determine|say|tell)|can't (?:determine|say|tell)"
    r"|do(?:es)? not (?:appear|contain)|isn't in|is not in|not part of)\b", re.I)
#: Explicit negation/affirmation, for answers that state the fact in prose instead of "Yes."/"No."
_NEG_RE = re.compile(r"\b(?:does not|doesn't|do not|don't|is not|isn't|are not|aren't|never|no direct)\b", re.I)
_AFF_RE = re.compile(r"\b(?:yes|it does|does (?:call|import|reference)|indeed)\b", re.I)


def is_refusal(answer: str) -> bool:
    """True when the answer declines to state a fact rather than asserting one."""
    return bool(REFUSAL_RE.search(answer or ""))


def parse_yn(answer: str) -> str | None:
    """yes / no / None. A bare token wins; otherwise explicit prose negation or affirmation counts,
    because "`x.tsx` does not import `y`" is an answer, not a non-answer. Ambiguity stays None."""
    a = (answer or "").strip().lower()
    m = re.search(r"\b(yes|no)\b", a)
    if m:
        return m.group(1)
    neg, aff = _NEG_RE.search(a), _AFF_RE.search(a)
    if neg and not aff:
        return "no"
    if aff and not neg:
        return "yes"
    return None


def score_yn(answer: str, truth: str) -> dict:
    got = parse_yn(answer)
    return {"correct": int(got == truth), "unparsed": int(got is None), "got": got}


def score_define_file(answer: str, truth: str) -> dict:
    """truth "not recorded" marks a question the excerpt cannot answer: refusing is correct and
    naming any file is a fabrication. Otherwise the truth path must appear."""
    a = (answer or "").replace("\\", "/")
    refused = is_refusal(a)
    if truth == "not recorded":
        return {"correct": int(refused), "basename": int(refused), "got": "refusal" if refused else "fabrication",
                "fabricated": int(not refused)}
    base = truth.rsplit("/", 1)[-1]
    strict = int(truth in a)
    return {"correct": strict, "basename": int(strict or (base in a)), "got": None, "fabricated": 0}


def norm_name(n: str) -> str:
    return n.strip().removesuffix("()").strip()


def names_in(answer: str) -> set[str]:
    return {norm_name(n) for n in re.findall(r"`([^`]+)`", answer) if n.strip()}


def score_callees(answer: str, truth: list[str], held_out: list[str], subject: str | None = None) -> dict:
    got = names_in(answer)
    if subject:  # the answer template restates the subject ("`X` (file) calls ..."); not a prediction
        got.discard(norm_name(subject))
    t = {norm_name(x) for x in truth}
    held_out = [norm_name(x) for x in held_out]
    tp = len(got & t)
    p = tp / len(got) if got else 0.0
    r = tp / len(t) if t else 0.0
    f1 = 2 * p * r / (p + r) if (p + r) else 0.0
    ho = set(held_out)
    return {"precision": p, "recall": r, "f1": f1,
            "heldout_recall": (len(got & ho) / len(ho)) if ho else None, "n_got": len(got)}


def score(row: dict, answer: str) -> dict:
    k = row["kind"]
    if k.startswith("yn_"):
        return score_yn(answer, row["truth"])
    if k == "define_file":
        return score_define_file(answer, row["truth"])
    if k == "callees_list":
        subject = row["user"].split("`")[1] if row["user"].count("`") >= 2 else None
        return score_callees(answer, row["truth"], row.get("held_out", []), subject)
    raise ValueError(k)


def ci95(p: float, n: int) -> float:
    return 1.96 * math.sqrt(p * (1 - p) / n) if n else 0.0


def aggregate(rows: list[dict]) -> dict:
    """rows: [{arm, kind, score:{...}}] -> {arm: {kind: {metric, n, ci}}}"""
    out: dict = defaultdict(dict)
    by = defaultdict(list)
    for r in rows:
        by[(r["arm"], r["kind"])].append(r["score"])
    for (arm, kind), scs in by.items():
        n = len(scs)
        if kind.startswith("yn_") or kind == "define_file":
            acc = sum(s["correct"] for s in scs) / n
            d = {"n": n, "accuracy": round(acc, 4), "ci95": round(ci95(acc, n), 4)}
            if kind.startswith("yn_"):
                d["unparsed"] = sum(s["unparsed"] for s in scs) / n
            else:
                d["basename_accuracy"] = round(sum(s["basename"] for s in scs) / n, 4)
                fab = [s.get("fabricated") for s in scs if s.get("fabricated") is not None]
                if fab:
                    d["fabrication_rate"] = round(sum(fab) / len(fab), 4)
        else:
            f1 = sum(s["f1"] for s in scs) / n
            ho = [s["heldout_recall"] for s in scs if s["heldout_recall"] is not None]
            d = {"n": n, "f1": round(f1, 4), "precision": round(sum(s["precision"] for s in scs) / n, 4),
                 "recall": round(sum(s["recall"] for s in scs) / n, 4),
                 "heldout_recall": round(sum(ho) / len(ho), 4) if ho else None}
        out[arm][kind] = d
    return dict(out)


def subsample(rows: list[dict], n_per_kind: int) -> list[dict]:
    """Deterministic (id-hash order), per kind; yes/no balanced for yn kinds."""
    by = defaultdict(list)
    for r in rows:
        by[r["kind"]].append(r)
    picked = []
    for kind, rs in sorted(by.items()):
        rs = sorted(rs, key=lambda r: hashlib.sha256(r["id"].encode()).hexdigest())
        if kind.startswith("yn_"):
            yes = [r for r in rs if r["truth"] == "yes"][: n_per_kind // 2]
            no = [r for r in rs if r["truth"] == "no"][: n_per_kind - len(yes)]
            picked += yes + no
        else:
            picked += rs[:n_per_kind]
    return picked


def user_text(row: dict, with_ctx: bool) -> str:
    if with_ctx and row.get("context"):
        return f"{row['context']}\n\nQuestion: {row['user']}"
    return row["user"]


def table(results: dict) -> str:
    kinds = sorted({k for arm in results.values() for k in arm})
    lines = [" | ".join(["arm".ljust(12)] + [k.ljust(14) for k in kinds])]
    for arm, per in results.items():
        cells = []
        for k in kinds:
            d = per.get(k)
            if not d:
                cells.append("-".ljust(14)); continue
            v = d.get("accuracy", d.get("f1"))
            extra = f" ho={d['heldout_recall']:.2f}" if d.get("heldout_recall") is not None else ""
            cells.append(f"{v:.3f}±{d.get('ci95', 0):.2f}{extra}"[:14].ljust(14))
        lines.append(" | ".join([arm.ljust(12)] + cells))
    return "\n".join(lines)


# ---- model plumbing (pod) -----------------------------------------------------

def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--base", default=None)
    ap.add_argument("--adapter", action="append", default=[], help="name=path (repeatable)")
    ap.add_argument("--evals", nargs="+", default=[], help="eval_seen.jsonl eval_unseen.jsonl ...")
    ap.add_argument("--arms", nargs="+", default=[])
    ap.add_argument("--n-per-kind", type=int, default=60)
    ap.add_argument("--max-new-tokens", type=int, default=96)
    ap.add_argument("--batch", type=int, default=16)
    ap.add_argument("--out", required=True)
    ap.add_argument("--system", default=SYSTEM)
    ap.add_argument("--rescore", type=Path, default=None,
                    help="re-aggregate a saved rows.jsonl with the CURRENT scorer; no model, no cost")
    a = ap.parse_args(argv)

    if a.rescore:
        saved = [json.loads(l) for l in a.rescore.read_text(encoding="utf-8").splitlines() if l.strip()]
        for r in saved:
            r["score"] = score(r, r["answer"])
        out = Path(a.out)
        out.mkdir(parents=True, exist_ok=True)
        results = aggregate(saved)
        (out / "results.json").write_text(json.dumps(results, indent=1), encoding="utf-8")
        with (out / "rows.jsonl").open("w", encoding="utf-8") as f:
            for r in saved:
                f.write(json.dumps(r, ensure_ascii=False) + "\n")
        print(table(results), flush=True)
        return 0

    if not (a.base and a.evals and a.arms):
        ap.error("--base, --evals and --arms are required unless --rescore is given")
    rows = []
    for f in a.evals:
        rows += [json.loads(l) for l in Path(f).read_text(encoding="utf-8").splitlines() if l.strip()]
    qs = subsample(rows, a.n_per_kind)
    print(f"[eval] {len(rows)} eval rows -> {len(qs)} questions x {len(a.arms)} arms", flush=True)

    import torch
    from transformers import AutoModelForCausalLM, AutoTokenizer
    tok = AutoTokenizer.from_pretrained(a.base)
    tok.padding_side = "left"
    if tok.pad_token is None:
        tok.pad_token = tok.eos_token
    model = AutoModelForCausalLM.from_pretrained(a.base, dtype=torch.bfloat16, device_map={"": 0})
    adapters = dict(x.split("=", 1) for x in a.adapter)
    if adapters:
        from peft import PeftModel
        names = list(adapters)
        model = PeftModel.from_pretrained(model, adapters[names[0]], adapter_name=names[0])
        for n in names[1:]:
            model.load_adapter(adapters[n], adapter_name=n)
    model.eval()

    def generate(prompts: list[str]) -> list[str]:
        outs = []
        order = sorted(range(len(prompts)), key=lambda i: len(prompts[i]))
        for i in range(0, len(order), a.batch):
            idx = order[i:i + a.batch]
            texts = [tok.apply_chat_template([{"role": "system", "content": a.system},
                                              {"role": "user", "content": prompts[j]}],
                                             tokenize=False, add_generation_prompt=True) for j in idx]
            enc = tok(texts, return_tensors="pt", padding=True, truncation=True, max_length=3072).to(model.device)
            with torch.no_grad():
                gen = model.generate(**enc, max_new_tokens=a.max_new_tokens, do_sample=False,
                                     pad_token_id=tok.pad_token_id)
            dec = tok.batch_decode(gen[:, enc["input_ids"].shape[1]:], skip_special_tokens=True)
            for j, d in zip(idx, dec):
                outs.append((j, d.strip()))
        outs.sort()
        return [d for _, d in outs]

    out = Path(a.out)
    out.mkdir(parents=True, exist_ok=True)
    all_rows = []
    for arm in a.arms:
        weights, ctx = arm.rsplit("_", 1)
        with_ctx = ctx == "ctx"
        prompts = [user_text(r, with_ctx) for r in qs]
        t0 = time.time()
        if weights == "base":
            if adapters:
                with model.disable_adapter():
                    answers = generate(prompts)
            else:
                answers = generate(prompts)
        else:
            model.set_adapter(weights)
            answers = generate(prompts)
        for r, ans in zip(qs, answers):
            all_rows.append({"arm": arm, "id": r["id"], "kind": r["kind"], "repo": r["repo"],
                             "user": r["user"], "truth": r["truth"], "answer": ans, "score": score(r, ans)})
        print(f"[eval] arm {arm}: {len(qs)} answers in {time.time() - t0:.0f}s", flush=True)
        results = aggregate(all_rows)
        (out / "results.json").write_text(json.dumps(results, indent=1), encoding="utf-8")
        with (out / "rows.jsonl").open("w", encoding="utf-8") as f:
            for r in all_rows:
                f.write(json.dumps(r, ensure_ascii=False) + "\n")
    print(table(aggregate(all_rows)), flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
