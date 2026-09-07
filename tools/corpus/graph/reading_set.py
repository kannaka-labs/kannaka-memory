#!/usr/bin/env python3
"""ADR-0057 — training set for a graph READING adapter (not a knowledge adapter).

The 2026-09-06 arms showed the base model reads a graph excerpt imperfectly (callee-list F1 0.81)
while an adapter that had seen excerpts reads them well (0.97), and the live Archivist confirmed
it: right subject, blurred relations. So train an adapter whose only job is to read an excerpt
faithfully and answer from it, with file and line, and to say "not recorded" when the excerpt
does not carry the answer.

Every training example puts the graph excerpt IN THE USER TURN:

    <excerpt from serialize_graph.RepoGraph.context>

    Question: <question>

and the assistant turn is the exact templated answer. Repos are split by a stable hash: the
adapter trains on ~85 % of repos and is evaluated on the rest, so a gain can only come from
reading. A share of examples are abstentions: the question's subject is absent from the
excerpt (a different node's excerpt, or none) and the answer says so.

Outputs (trainer-compatible): train.jsonl, holdout.jsonl, eval_seen.jsonl (eval-repo rows with
oracle context, plus abstention rows with empty context and truth "not recorded"),
reading.manifest.json. Reuses serialize_graph's graph model, questions and answers.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import random
import sys
from collections import Counter
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import serialize_graph as sg  # noqa: E402

SYSTEM = ("You are Kannaka's archivist. A code-graph excerpt is given with each question: answer only from "
          "it, name file and line when they are in the excerpt, keep lists exact, and when the excerpt does "
          "not contain what is asked, say plainly that it is not recorded.")
ABSENT = "`{name}` is not in the records I was given for {repo}; I cannot say from this excerpt."
EVAL_REPO_FRAC = 0.15


def is_eval_repo(repo: str, frac: float = EVAL_REPO_FRAC) -> bool:
    return sg.stable_frac("reading-eval", repo) < frac


def subject_nodes(kind: str, subject: str) -> list[str]:
    if kind in ("path", "rationale"):
        return subject.split(sg.SUBJ_SEP)
    if kind == "absent":
        return []
    return [subject]


def excerpt_for(g: sg.RepoGraph, kind: str, subject: str) -> str:
    nodes = subject_nodes(kind, subject)
    if kind == "rationale":
        nodes = [nodes[1]]
    return g.context(nodes) if nodes else ""


def with_excerpt(excerpt: str, question: str) -> str:
    return f"{excerpt}\n\nQuestion: {question}" if excerpt else f"(no excerpt)\n\nQuestion: {question}"


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--graphs", required=True, type=Path)
    ap.add_argument("--novelty", type=Path, default=None)
    ap.add_argument("--out", required=True, type=Path)
    ap.add_argument("--budget", type=int, default=3000, help="train examples (before the ppl hold-out split)")
    ap.add_argument("--abstain-frac", type=float, default=0.12)
    ap.add_argument("--repo-cap", type=int, default=400)
    ap.add_argument("--eval-per-repo", type=int, default=25)
    ap.add_argument("--eval-abstain", type=int, default=60)
    ap.add_argument("--ppl-holdout", type=float, default=0.05)
    ap.add_argument("--seed", type=int, default=11)
    ap.add_argument("--only", default=None)
    ap.add_argument("--dry-run", action="store_true")
    a = ap.parse_args()

    rng = random.Random(a.seed)
    found = sg.discover(a.graphs)
    if a.only:
        import re
        found = [(r, p) for r, p in found if re.search(a.only, r)]
    train_graphs, eval_graphs = [], []
    for repo, p in found:
        data = json.loads(p.read_text(encoding="utf-8"))
        nov = sg.load_novelty(a.novelty, repo)
        if is_eval_repo(repo):
            eval_graphs.append(sg.RepoGraph(repo, data, 0.10, nov))   # hold-out gives callees_list rows
        else:
            train_graphs.append(sg.RepoGraph(repo, data, 0.0, nov))   # the adapter may see every edge
    repo_w = {g.repo: (sg.CORE_WEIGHT if sg.is_core(g.repo) else 1.0) for g in train_graphs}

    # 1. positives from training repos, with the excerpt in the user turn
    candidates = []
    for g in train_graphs:
        recs = sg.build_records(g, random.Random(f"{a.seed}:{g.repo}"), max_paths=40, max_absent=0, other_repos=[])
        recs = [r for r in recs if r["kind"] != "absent"]
        if len(recs) > a.repo_cap:
            recs = sg.weighted_sample(recs, a.repo_cap, random.Random(f"{a.seed}:cap:{g.repo}"), repo_w)
        for r in recs:
            r["excerpt"] = excerpt_for(g, r["kind"], r["subject"])
            r["graph"] = g
        candidates.extend(recs)
    n_pos = int(a.budget * (1 - a.abstain_frac))
    positives = sg.weighted_sample(candidates, n_pos, rng, repo_w)

    # 2. abstentions: a real question paired with a foreign or empty excerpt
    abstentions = []
    pool = [r for r in candidates if r["kind"] in ("callees", "callers", "explain", "file_contents")]
    rng.shuffle(pool)
    for r in pool[: a.budget - n_pos]:
        g = r["graph"]
        other = rng.choice(candidates)
        excerpt = "" if rng.random() < 0.5 else (other["excerpt"] if other["graph"] is not g or other["subject"] != r["subject"] else "")
        name = g.name(r["subject"]) if r["subject"] in g.nodes else r["subject"]
        rid = hashlib.sha256(f"abstain|{r['id']}|{other['id']}".encode()).hexdigest()[:16]
        abstentions.append({"id": rid, "kind": "abstain", "repo": g.repo, "novelty": 1.0, "weight": 1.0,
                            "user": with_excerpt(excerpt, r["user"]), "assistant": ABSENT.format(name=name, repo=g.repo)})

    def example(r):
        user = r["user"] if r["kind"] == "abstain" else with_excerpt(r["excerpt"], r["user"])
        w = r.get("weight") or round(sg.weight(r, repo_w), 4)
        return {"id": r["id"], "kind": r["kind"], "repo": r["repo"], "weight": w,
                "messages": [{"role": "system", "content": SYSTEM}, {"role": "user", "content": user},
                             {"role": "assistant", "content": r["assistant"]}]}

    rows = [example(r) for r in positives] + [example(r) for r in abstentions]
    rng.shuffle(rows)
    hold = [r for r in rows if sg.stable_frac("ppl", r["id"]) < a.ppl_holdout]
    hold_ids = {r["id"] for r in hold}
    train = [r for r in rows if r["id"] not in hold_ids]

    # 3. eval on repos the adapter never saw: serialize_graph's eval rows already carry oracle context
    ev = []
    for g in eval_graphs:
        seen, unseen = sg.eval_records(g, random.Random(f"{a.seed}:eval:{g.repo}"), a.eval_per_repo, set())
        ev.extend(seen); ev.extend(unseen)
    # abstention eval: define_file questions with NO excerpt; truth "not recorded" is what score_define_file looks for
    eval_pool = [g for g in eval_graphs if g.nodes]
    for i in range(a.eval_abstain):
        g = eval_pool[i % len(eval_pool)]
        syms = [n for n in g.nodes if g.kind(n) in ("symbol", "class")]
        if not syms:
            continue
        nid = random.Random(f"{a.seed}:abs:{i}").choice(syms)
        ev.append({"id": hashlib.sha256(f"evabs|{g.repo}|{nid}".encode()).hexdigest()[:16], "kind": "define_file",
                   "repo": g.repo, "nodes": [nid], "context": "",
                   "user": f"Which file in {g.repo} defines `{g.name(nid)}`?", "truth": "not recorded"})

    summary = {"train_repos": len(train_graphs), "eval_repos": len(eval_graphs), "eval_repo_names": [g.repo for g in eval_graphs],
               "candidates": len(candidates), "train": len(train), "holdout": len(hold), "eval": len(ev),
               "by_kind": dict(Counter(r["kind"] for r in train)), "eval_by_kind": dict(Counter(r["kind"] for r in ev)),
               "words": sum(len(r["messages"][1]["content"].split()) + len(r["messages"][2]["content"].split()) for r in train)}
    print(json.dumps(summary, indent=1))
    if a.dry_run:
        return 0
    a.out.mkdir(parents=True, exist_ok=True)
    sg.write_jsonl(a.out / "train.jsonl", train)
    sg.write_jsonl(a.out / "holdout.jsonl", hold)
    sg.write_jsonl(a.out / "eval_seen.jsonl", ev)
    (a.out / "reading.manifest.json").write_text(json.dumps({**summary, "seed": a.seed, "system": SYSTEM,
                                                             "abstain_frac": a.abstain_frac}, indent=1), encoding="utf-8")
    print(f"wrote {a.out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
