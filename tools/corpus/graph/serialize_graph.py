#!/usr/bin/env python3
"""ADR-0057 — turn graphify code graphs into a code-knowledge SFT set + exact evals.

Input: one `graphify-out/graph.json` per repo (tree-sitter AST graph, NetworkX
node-link format) under `--graphs <dir>/<owner>/<name>/graphify-out/graph.json`,
plus optional per-fork novelty lists (`--novelty <dir>/<owner>/<name>.txt`, one
path per line = files that differ from the fork's upstream).

Output (trainer-compatible, same shape as p2/prep_sft.py):
  train.jsonl            {"id","kind","repo","weight","messages":[system,user,assistant]}
  holdout.jsonl          same shape, disjoint by id hash — the trainer's ppl hold-out
  train_scrambled.jsonl  (with --scramble) the SAME questions re-answered from a graph
                         in which every non-contains edge points at a same-kind node the
                         real graph does NOT link (a question the scrambled graph cannot
                         answer gets an explicit empty answer, never the true one):
                         identical wattage, false facts. The control arm.
  eval_seen.jsonl        exact-scorable questions about facts that ARE in train, in a
                         form never used as a training template (yes/no membership,
                         "which file defines X"). Carries "truth".
  eval_unseen.jsonl      exact-scorable questions whose facts were HELD OUT of the
                         training graph (edge-level hold-out). Adapter-alone should
                         sit at chance here; graph-in-context arms should not.
  serialize.manifest.json  counts per repo/kind, weights, seed, graph commits.
  Every eval row also carries "nodes" (graph ids) and "context": the 1-hop neighbourhood
  of the subject in the FULL graph (train + held-out), i.e. oracle retrieval for the *_ctx arms.
  Keys of config files (.json/.yaml/.toml/...) are excluded from records and evals.

Weighting ("original novel work weighs more", Nick 2026-09-06):
  repo weight   = CORE_WEIGHT (2.0) if the repo name matches a constellation pattern,
                  else 1.0 for a source repo; forks inherit the same by name.
  file novelty  = 1.0 for every file of a source repo; for a fork, 1.0 if the file is
                  in its novelty list (differs from upstream) else UPSTREAM_WEIGHT (0.1).
  kind weight   = rationale 2.0 > explain 1.5 > callees/callers/path/cross 1.0 >
                  file_contents/imports 0.7.
  record weight = repo × novelty × kind. Sampling is weighted without replacement,
  with a per-repo cap, so a 113k-node fork cannot swamp a 5k-node first-party repo.

Hold-out: 10 % (default) of calls/references/imports edges per repo are chosen by a
stable hash and REMOVED from the training graph before any record is built, so no
train answer can leak a held-out fact. The scrambled set uses the same held-out
mask (same questions, same coverage).

Everything here is derived from the graph: no LLM, no HRM, no inbound text.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import random
import re
from collections import Counter, defaultdict, deque
from pathlib import Path

SYSTEM = ("You are Kannaka's archivist: you answer questions about the constellation's code from "
          "the code graph you were trained on. Name the file and line when you know them, keep "
          "lists short and exact, and when a symbol or repo is not in your graph say so plainly.")

CORE_PATTERNS = [r"kannaka", r"^kax", r"agent-kax", r"0xscada", r"quantumos", r"space-?child",
                 r"ghost", r"pitchfork", r"flaukowski", r"ninja", r"singularis", r"kannaktopus",
                 r"queensync", r"1f916", r"flux$"]
CORE_WEIGHT = 2.0
UPSTREAM_WEIGHT = 0.1
KIND_WEIGHT = {"rationale": 2.0, "explain": 1.5, "callees": 1.0, "callers": 1.0, "path": 1.0,
               "cross_repo": 1.0, "file_contents": 0.7, "imports": 0.7, "absent": 0.5}
CALL_RELATIONS = {"calls", "indirect_call"}
IMPORT_RELATIONS = {"imports", "imports_from", "dynamic_import", "re_exports"}
HOLDOUT_RELATIONS = CALL_RELATIONS | IMPORT_RELATIONS | {"references"}
MAX_LIST = 12
# graphify emits keys of config files (tsconfig.json, *.yaml, ...) as symbols; they are not code
CONFIG_EXT = (".json", ".yaml", ".yml", ".toml", ".lock", ".ini", ".cfg", ".env", ".csv", ".md")
CONTEXT_EDGES = 30
CONTEXT_CHARS = 4000


def stable_frac(*parts: str) -> float:
    h = hashlib.sha256("\x1f".join(parts).encode("utf-8")).hexdigest()[:12]
    return int(h, 16) / 0xFFFFFFFFFFFF


def is_core(repo: str) -> bool:
    name = repo.split("/", 1)[-1].lower()
    return any(re.search(p, name) for p in CORE_PATTERNS)


class RepoGraph:
    """One repo's graph with a train view (held-out edges removed) and the held-out edges."""

    def __init__(self, repo: str, data: dict, holdout_frac: float, novel: set[str] | None):
        self.repo = repo
        self.nodes = {n["id"]: n for n in data.get("nodes", [])}
        self.commit = data.get("built_at_commit")
        self.novel = novel  # None = source repo (everything novel)
        links = data.get("links", data.get("edges", []))
        self.train_edges: list[dict] = []
        self.heldout_edges: list[dict] = []
        for e in links:
            if e["source"] not in self.nodes or e["target"] not in self.nodes:
                continue
            key = (repo, e["source"], e["target"], e.get("relation", ""))
            if e.get("relation") in HOLDOUT_RELATIONS and stable_frac(*key) < holdout_frac:
                self.heldout_edges.append(e)
            else:
                self.train_edges.append(e)
        self._index(self.train_edges)
        # full view (train + held-out): only ever used to render oracle retrieval context for evals
        self.full_out = defaultdict(list)
        self.full_inc = defaultdict(list)
        for e in self.train_edges + self.heldout_edges:
            self.full_out[e["source"]].append((e.get("relation", ""), e["target"]))
            self.full_inc[e["target"]].append((e.get("relation", ""), e["source"]))

    def _index(self, edges: list[dict]) -> None:
        self.out = defaultdict(list)
        self.inc = defaultdict(list)
        self.contains = defaultdict(list)
        self.rationale_for = defaultdict(list)
        self.undirected = defaultdict(set)
        for e in edges:
            rel = e.get("relation", "")
            s, t = e["source"], e["target"]
            self.out[s].append((rel, t))
            self.inc[t].append((rel, s))
            if rel == "contains":
                self.contains[s].append(t)
            elif rel == "rationale_for":
                self.rationale_for[t].append(s)
            if rel not in ("contains",):
                self.undirected[s].add(t)
                self.undirected[t].add(s)

    def scrambled(self, seed: int) -> "RepoGraph":
        """Same nodes, same edge sources/relations; each non-contains edge re-targeted to a
        same-kind node the real graph does not link from that source (false facts, same count)."""
        rng = random.Random(f"{seed}:{self.repo}")
        clone = object.__new__(RepoGraph)
        clone.repo, clone.nodes, clone.commit, clone.novel = self.repo, self.nodes, self.commit, self.novel
        clone.heldout_edges = self.heldout_edges
        by_rel = defaultdict(list)
        for e in self.train_edges:
            by_rel[e.get("relation", "")].append(e)
        real = {(e["source"], e["target"], e.get("relation", "")) for e in self.train_edges}
        # candidate targets by kind, so a scrambled call still points at a symbol, an import at a file
        by_kind = defaultdict(list)
        for nid in self.nodes:
            by_kind[self.kind(nid)].append(nid)
        new_edges = []
        for rel, es in by_rel.items():
            for e in es:
                t = e["target"]
                if rel != "contains":
                    pool = by_kind.get(self.kind(t)) or list(self.nodes)
                    for _ in range(25):  # a scrambled edge must be a pair the real graph does NOT have
                        cand = rng.choice(pool)
                        if cand != e["source"] and (e["source"], cand, rel) not in real:
                            t = cand
                            break
                new_edges.append({**e, "target": t})
        clone.train_edges = new_edges
        clone._index(new_edges)
        return clone

    # ---- node helpers -------------------------------------------------
    def kind(self, nid: str) -> str:
        n = self.nodes[nid]
        ft = n.get("file_type")
        if ft == "rationale":
            return "rationale"
        if ft == "concept":
            return "concept"
        if (n.get("source_file") or "").lower().endswith(CONFIG_EXT):
            return "config"
        if n.get("_callable_class"):
            return "class"
        if nid in self.contains or n.get("label") == n.get("source_file"):
            return "file"
        return "symbol"

    def name(self, nid: str) -> str:
        return self.nodes[nid].get("label") or nid

    def where(self, nid: str) -> str:
        n = self.nodes[nid]
        f, loc = n.get("source_file") or "", n.get("source_location") or ""
        return f"{f}:{loc}" if f and loc else f or "(unknown file)"

    def context(self, ids: list[str], focus: list[str] | None = None) -> str:
        """Oracle retrieval: the 1-hop neighbourhood of each node in the FULL graph, as compact lines.
        This is what a perfect graph lookup would hand the model; the *_ctx eval arms use it.
        Edges whose other end is one of `ids`/`focus` are rendered first, so the cap can never
        drop the very edge a question is about (a hub with 32 callees did exactly that)."""
        prio = set(ids) | set(focus or ())
        lines = [f"Graph excerpt from {self.repo}:"]
        for nid in ids:
            if nid not in self.nodes:
                continue
            lines.append(f"`{self.name(nid)}` [{self.kind(nid)}] {self.where(nid)}")
            outs = sorted(self.full_out.get(nid, []), key=lambda rt: rt[1] not in prio)
            for rel, t in outs[:CONTEXT_EDGES]:
                lines.append(f"  -> {rel} `{self.name(t)}` ({self.where(t)})")
            incs = sorted(self.full_inc.get(nid, []), key=lambda rs: rs[1] not in prio)
            for rel, s_ in incs[:CONTEXT_EDGES]:
                lines.append(f"  <- {rel} `{self.name(s_)}` ({self.where(s_)})")
        text = "\n".join(lines)
        return text[:CONTEXT_CHARS]

    def novelty(self, nid: str) -> float:
        if self.novel is None:
            return 1.0
        f = self.nodes[nid].get("source_file") or ""
        return 1.0 if f in self.novel else UPSTREAM_WEIGHT

    def rels(self, nid: str, relset: set[str], incoming: bool = False) -> list[str]:
        seq = self.inc[nid] if incoming else self.out[nid]
        seen, res = set(), []
        for rel, other in seq:
            if rel in relset and other not in seen and other != nid:
                seen.add(other)
                res.append(other)
        return res

    def path(self, a: str, b: str, max_hops: int = 4) -> list[tuple[str, str, str]] | None:
        """Shortest undirected path a..b as [(node, relation, next)], or None."""
        if a == b:
            return None
        rel_of = {}
        for e in self.train_edges:
            if e.get("relation") != "contains":
                rel_of[(e["source"], e["target"])] = e.get("relation", "")
        prev = {a: None}
        dq = deque([a])
        while dq:
            cur = dq.popleft()
            if cur == b:
                break
            if len(self._chain(prev, cur)) > max_hops:
                continue
            for nxt in sorted(self.undirected[cur]):  # sorted: set order is per-process
                if nxt not in prev:
                    prev[nxt] = cur
                    dq.append(nxt)
        if b not in prev:
            return None
        chain = self._chain(prev, b)
        if len(chain) - 1 > max_hops:
            return None
        hops = []
        for x, y in zip(chain, chain[1:]):
            rel = rel_of.get((x, y)) or rel_of.get((y, x), "linked")
            hops.append((x, rel, y))
        return hops

    @staticmethod
    def _chain(prev: dict, node: str) -> list[str]:
        out = []
        while node is not None:
            out.append(node)
            node = prev[node]
        return out[::-1]


# ---- record builders ----------------------------------------------------

def fmt_list(g: RepoGraph, ids: list[str], with_where: bool = True) -> str:
    items = ids[:MAX_LIST]
    parts = [f"`{g.name(i)}` ({g.where(i)})" if with_where else f"`{g.name(i)}`" for i in items]
    more = f", and {len(ids) - MAX_LIST} more" if len(ids) > MAX_LIST else ""
    return ", ".join(parts) + more


SUBJ_SEP = "\x1e"


def question(g: RepoGraph, kind: str, subject: str) -> str:
    repo = g.repo
    if kind == "path":
        a, b = subject.split(SUBJ_SEP)
        return f"How is `{g.name(a)}` connected to `{g.name(b)}` in {repo}?"
    if kind == "absent":
        other = subject.split(SUBJ_SEP)[1]
        return f"In {repo}, where is `{other.split('/')[-1]}_main` defined?"
    if kind == "rationale":
        t = subject.split(SUBJ_SEP)[1]
        return f"In {repo}, what rationale is recorded for `{g.name(t)}` ({g.where(t)})?"
    nm = g.name(subject)
    return {"file_contents": f"In {repo}, what does the file `{nm}` define?",
            "imports": f"What does `{nm}` import in {repo}?",
            "callees": f"In {repo}, what does `{nm}` call?",
            "callers": f"What calls `{nm}` in {repo}?",
            "explain": f"Explain `{nm}` in {repo}."}[kind]


def answer(g: RepoGraph, kind: str, subject: str) -> str | None:
    """The answer to (kind, subject) on graph g, or None when g holds no fact for it.
    The same function answers on the real graph (train) and the scrambled graph (control),
    so every control question is re-answered from the scrambled facts, never copied."""
    repo = g.repo
    if kind == "absent":
        other = subject.split(SUBJ_SEP)[1]
        return (f"There is no `{other.split('/')[-1]}_main` in my graph for {repo}. "
                f"If you mean {other}, ask me there.")
    if kind == "path":
        a, b = subject.split(SUBJ_SEP)
        hops = g.path(a, b)
        if not hops:
            return None
        chain = " ".join(f"`{g.name(x)}` --{rel}--> " for x, rel, _ in hops) + f"`{g.name(hops[-1][2])}`"
        return f"Shortest path ({len(hops)} hops): {chain}."
    if kind == "rationale":
        r, t = subject.split(SUBJ_SEP)
        if t not in g.rels(r, {"rationale_for"}):
            return None
        return f"The note next to `{g.name(t)}` reads: \"{g.name(r).rstrip('…')}\""
    nid = subject
    nm, wh = g.name(nid), g.where(nid)
    if kind == "file_contents":
        syms = g.contains.get(nid, [])
        return f"`{nm}` in {repo} defines {len(syms)} symbol(s): {fmt_list(g, syms, with_where=False)}." if syms else None
    if kind == "imports":
        imps = g.rels(nid, IMPORT_RELATIONS)
        return f"`{nm}` ({repo}) imports {fmt_list(g, imps, with_where=False)}." if imps else None
    if kind == "callees":
        callees = g.rels(nid, CALL_RELATIONS)
        return f"`{nm}` ({wh}) calls {fmt_list(g, callees)}." if callees else None
    if kind == "callers":
        callers = g.rels(nid, CALL_RELATIONS, incoming=True)
        return f"`{nm}` ({wh}) is called by {fmt_list(g, callers)}." if callers else None
    if kind == "explain":
        callees = g.rels(nid, CALL_RELATIONS)
        callers = g.rels(nid, CALL_RELATIONS, incoming=True)
        refs = g.rels(nid, {"references"})
        rats = g.rationale_for.get(nid, [])
        if not (callees or callers or refs or rats):
            return None
        k = g.kind(nid)
        bits = [f"`{nm}` is a {'class' if k == 'class' else 'symbol'} in {repo} at {wh}."]
        if callees:
            bits.append(f"It calls {fmt_list(g, callees, with_where=False)}.")
        if callers:
            bits.append(f"It is called by {fmt_list(g, callers, with_where=False)}.")
        if refs:
            bits.append(f"It references {fmt_list(g, refs, with_where=False)}.")
        for r in rats[:2]:
            bits.append(f"Rationale recorded next to it: \"{g.name(r).rstrip('…')}\"")
        return " ".join(bits)
    raise ValueError(kind)


def empty_answer(g: RepoGraph, kind: str, subject: str) -> str:
    """What the archivist says when the graph holds nothing for the question (used by the control)."""
    repo = g.repo
    if kind == "path":
        a, b = subject.split(SUBJ_SEP)
        return f"I have no recorded path between `{g.name(a)}` and `{g.name(b)}` in {repo}."
    if kind == "rationale":
        t = subject.split(SUBJ_SEP)[1]
        return f"I have no rationale recorded for `{g.name(t)}` in {repo}."
    nm = g.name(subject)
    return {"file_contents": f"I have no symbols recorded for `{nm}` in {repo}.",
            "imports": f"I have no imports recorded for `{nm}` in {repo}.",
            "callees": f"I have no calls recorded from `{nm}` in {repo}.",
            "callers": f"I have no callers recorded for `{nm}` in {repo}.",
            "explain": f"`{nm}` is in {repo} at {g.where(subject)}; I have no calls or references recorded for it."
            }[kind]


def build_records(g: RepoGraph, rng: random.Random, max_paths: int, max_absent: int,
                  other_repos: list[str]) -> list[dict]:
    repo = g.repo
    recs: list[dict] = []

    def add(kind: str, subject: str, novelty_id: str | None = None):
        ans = answer(g, kind, subject)
        if ans is None:
            return
        user = question(g, kind, subject)
        rid = hashlib.sha256(f"{repo}|{kind}|{subject}|{user}".encode()).hexdigest()[:16]
        nov_id = novelty_id or subject
        recs.append({"id": rid, "kind": kind, "repo": repo, "subject": subject,
                     "novelty": g.novelty(nov_id) if nov_id in g.nodes else 1.0,
                     "user": user, "assistant": ans})

    for nid in g.nodes:
        k = g.kind(nid)
        if k == "file":
            add("file_contents", nid)
            add("imports", nid)
        elif k in ("symbol", "class"):
            add("callees", nid)
            add("callers", nid)
            add("explain", nid)
        elif k == "rationale":
            for t in g.rels(nid, {"rationale_for"})[:1]:
                add("rationale", f"{nid}{SUBJ_SEP}{t}", novelty_id=t)

    # paths: sample pairs of connected symbols 2..4 hops apart
    syms = [n for n in g.nodes if g.kind(n) in ("symbol", "class") and g.undirected[n]]
    tries, made, seen_pairs = 0, 0, set()
    while syms and made < max_paths and tries < max_paths * 8:
        tries += 1
        a = rng.choice(syms)
        cur = a
        for _ in range(rng.randint(2, 4)):
            nbrs = sorted(g.undirected[cur])  # sorted: set order is per-process
            if not nbrs:
                break
            cur = rng.choice(nbrs)
        b = cur
        if a == b or (a, b) in seen_pairs:
            continue
        hops = g.path(a, b)
        if not hops or len(hops) < 2:
            continue
        seen_pairs.add((a, b))
        made += 1
        add("path", f"{a}{SUBJ_SEP}{b}")

    # absent: symbols from other repos asked of this one -> honest "not in my graph"
    for _ in range(min(max_absent, len(other_repos))):
        other = rng.choice(other_repos)
        add("absent", f"absent{SUBJ_SEP}{other}")
    return recs


def cross_repo_records(graphs: list[RepoGraph]) -> list[dict]:
    """Labels defined as symbols/classes in two or more first-party (source) repos."""
    where = defaultdict(set)
    for g in graphs:
        if g.novel is not None:
            continue
        for nid in g.nodes:
            if g.kind(nid) in ("symbol", "class"):
                where[g.name(nid)].add(g.repo)
    recs = []
    for label, repos in where.items():
        if 2 <= len(repos) <= 6 and len(label) >= 6:
            rs = sorted(repos)
            rid = hashlib.sha256(f"cross|{label}".encode()).hexdigest()[:16]
            recs.append({"id": rid, "kind": "cross_repo", "repo": "*", "subject": label, "novelty": 1.0,
                         "user": f"Which repos in the constellation define a symbol named `{label}`?",
                         "assistant": f"`{label}` is defined in {len(rs)} repos: {', '.join(rs)}."})
    return recs


# ---- evals ----------------------------------------------------------------

def eval_records(g: RepoGraph, rng: random.Random, n_per_repo: int, train_ids: set[str]) -> tuple[list, list]:
    """seen: yes/no membership over TRAIN edges + 'which file defines X';
    unseen: yes/no membership over HELD-OUT edges + list questions for held-out subjects."""
    seen, unseen = [], []
    repo = g.repo
    node_ids = list(g.nodes)

    def neg_target(src: str, rel: str) -> str | None:
        have = {t for r, t in g.out[src] if r == rel} | {e["target"] for e in g.heldout_edges if e["source"] == src}
        for _ in range(20):
            cand = rng.choice(node_ids)
            if cand != src and cand not in have and g.kind(cand) in ("symbol", "class", "file"):
                return cand
        return None

    def yn(kind_label: str, e: dict, truth: bool, bucket: list):
        s, t, rel = e["source"], e["target"], e.get("relation", "")
        verb = {"calls": "call", "indirect_call": "call", "references": "reference"}.get(rel, "import")
        rid = hashlib.sha256(f"{repo}|yn|{s}|{t}|{rel}|{truth}".encode()).hexdigest()[:16]
        bucket.append({"id": rid, "kind": f"yn_{kind_label}", "repo": repo, "relation": rel,
                       "nodes": [s, t], "context": g.context([s], focus=[t]),
                       "user": f"In {repo}, does `{g.name(s)}` {verb} `{g.name(t)}`? Answer yes or no.",
                       "truth": "yes" if truth else "no"})

    train_pool = [e for e in g.train_edges if e.get("relation") in HOLDOUT_RELATIONS]
    rng.shuffle(train_pool)
    for e in train_pool[:n_per_repo]:
        yn("seen", e, True, seen)
        nt = neg_target(e["source"], e.get("relation", ""))
        if nt:
            yn("seen", {**e, "target": nt}, False, seen)
    syms = [n for n in node_ids if g.kind(n) in ("symbol", "class") and g.nodes[n].get("source_file")]
    rng.shuffle(syms)
    for nid in syms[:n_per_repo // 2]:
        rid = hashlib.sha256(f"{repo}|deffile|{nid}".encode()).hexdigest()[:16]
        seen.append({"id": rid, "kind": "define_file", "repo": repo, "nodes": [nid], "context": g.context([nid]),
                     "user": f"Which file in {repo} defines `{g.name(nid)}`?",
                     "truth": g.nodes[nid]["source_file"]})

    held = list(g.heldout_edges)
    rng.shuffle(held)
    for e in held[:n_per_repo]:
        yn("unseen", e, True, unseen)
        nt = neg_target(e["source"], e.get("relation", ""))
        if nt:
            yn("unseen", {**e, "target": nt}, False, unseen)
    by_src = defaultdict(list)
    for e in g.heldout_edges:
        if e.get("relation") in CALL_RELATIONS:
            by_src[e["source"]].append(e["target"])
    for s, held_t in list(by_src.items())[: n_per_repo // 2]:
        full = sorted({t for r, t in g.out[s] if r in CALL_RELATIONS} | set(held_t))
        rid = hashlib.sha256(f"{repo}|calllist|{s}".encode()).hexdigest()[:16]
        unseen.append({"id": rid, "kind": "callees_list", "repo": repo, "nodes": [s], "context": g.context([s]),
                       "user": f"In {repo}, list everything `{g.name(s)}` calls.",
                       "truth": [g.name(t) for t in full], "held_out": [g.name(t) for t in held_t]})
    return seen, unseen


# ---- sampling / output ------------------------------------------------------

def weight(rec: dict, repo_w: dict[str, float]) -> float:
    return repo_w.get(rec["repo"], 1.0) * rec["novelty"] * KIND_WEIGHT.get(rec["kind"], 1.0)


def weighted_sample(recs: list[dict], k: int, rng: random.Random, repo_w: dict[str, float]) -> list[dict]:
    """Efraimidis–Spirakis weighted sampling without replacement (stable under a seed)."""
    keyed = []
    for r in recs:
        w = weight(r, repo_w)
        if w <= 0:
            continue
        keyed.append((rng.random() ** (1.0 / w), r))
    keyed.sort(key=lambda x: -x[0])
    return [r for _, r in keyed[:k]]


def to_example(rec: dict, repo_w: dict[str, float]) -> dict:
    return {"id": rec["id"], "kind": rec["kind"], "repo": rec["repo"], "weight": round(weight(rec, repo_w), 4),
            "messages": [{"role": "system", "content": SYSTEM},
                         {"role": "user", "content": rec["user"]},
                         {"role": "assistant", "content": rec["assistant"]}]}


def load_novelty(novelty_dir: Path | None, repo: str) -> set[str] | None:
    if novelty_dir is None:
        return None
    p = novelty_dir / f"{repo}.txt"
    if not p.exists():
        return None
    return {ln.strip() for ln in p.read_text(encoding="utf-8").splitlines() if ln.strip()}


def discover(graphs_dir: Path) -> list[tuple[str, Path]]:
    out = []
    for p in sorted(graphs_dir.glob("*/*/graphify-out/graph.json")):
        out.append(("/".join(p.parts[-4:-2]), p))
    return out


def write_jsonl(path: Path, rows: list[dict]) -> None:
    with path.open("w", encoding="utf-8") as f:
        for r in rows:
            f.write(json.dumps(r, ensure_ascii=False) + "\n")


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--graphs", required=True, type=Path, help="dir with <owner>/<name>/graphify-out/graph.json")
    ap.add_argument("--novelty", type=Path, default=None, help="dir with <owner>/<name>.txt fork novelty lists")
    ap.add_argument("--out", required=True, type=Path)
    ap.add_argument("--budget", type=int, default=10000, help="train examples to sample")
    ap.add_argument("--repo-cap", type=int, default=1200, help="max candidate records kept per repo before sampling")
    ap.add_argument("--holdout", type=float, default=0.10, help="edge-level hold-out fraction")
    ap.add_argument("--ppl-holdout", type=float, default=0.05, help="fraction of sampled train moved to holdout.jsonl")
    ap.add_argument("--paths-per-repo", type=int, default=150)
    ap.add_argument("--absent-per-repo", type=int, default=6)
    ap.add_argument("--eval-per-repo", type=int, default=40)
    ap.add_argument("--seed", type=int, default=7)
    ap.add_argument("--scramble", action="store_true", help="also write train_scrambled.jsonl (control arm)")
    ap.add_argument("--only", default=None, help="regex on owner/name to restrict repos (debug)")
    ap.add_argument("--dry-run", action="store_true")
    a = ap.parse_args()

    rng = random.Random(a.seed)
    found = discover(a.graphs)
    if a.only:
        found = [(r, p) for r, p in found if re.search(a.only, r)]
    if not found:
        print("no graphs found"); return 2

    graphs: list[RepoGraph] = []
    for repo, p in found:
        data = json.loads(p.read_text(encoding="utf-8"))
        graphs.append(RepoGraph(repo, data, a.holdout, load_novelty(a.novelty, repo)))
    repo_names = [g.repo for g in graphs]
    repo_w = {g.repo: (CORE_WEIGHT if is_core(g.repo) else 1.0) for g in graphs}

    candidates: list[dict] = []
    per_repo_counts = {}
    scr_by_id: dict[str, str] = {}
    for g in graphs:
        others = [r for r in repo_names if r != g.repo]
        recs = build_records(g, random.Random(f"{a.seed}:{g.repo}"), a.paths_per_repo, a.absent_per_repo, others)
        if len(recs) > a.repo_cap:
            recs = weighted_sample(recs, a.repo_cap, random.Random(f"{a.seed}:cap:{g.repo}"), repo_w)
        per_repo_counts[g.repo] = {"nodes": len(g.nodes), "train_edges": len(g.train_edges),
                                   "heldout_edges": len(g.heldout_edges), "candidates": len(recs),
                                   "core": is_core(g.repo), "fork": g.novel is not None,
                                   "novel_files": (len(g.novel) if g.novel is not None else None)}
        candidates.extend(recs)
        if a.scramble:
            sg = g.scrambled(a.seed)
            for r in recs:  # re-answer every kept question from the scrambled graph
                scr_by_id[r["id"]] = answer(sg, r["kind"], r["subject"]) or empty_answer(sg, r["kind"], r["subject"])
    candidates.extend(cross_repo_records(graphs))

    train_all = weighted_sample(candidates, a.budget, rng, repo_w)
    ppl_hold = [r for r in train_all if stable_frac("ppl", r["id"]) < a.ppl_holdout]
    hold_ids = {r["id"] for r in ppl_hold}
    train = [r for r in train_all if r["id"] not in hold_ids]
    train_ids = {r["id"] for r in train}

    seen_all, unseen_all = [], []
    for g in graphs:
        s, u = eval_records(g, random.Random(f"{a.seed}:eval:{g.repo}"), a.eval_per_repo, train_ids)
        seen_all.extend(s); unseen_all.extend(u)

    kinds = Counter(r["kind"] for r in train)
    repos_in_train = Counter(r["repo"] for r in train)
    summary = {"repos": len(graphs), "candidates": len(candidates), "train": len(train), "holdout": len(ppl_hold),
               "eval_seen": len(seen_all), "eval_unseen": len(unseen_all), "by_kind": dict(kinds),
               "top_repos": repos_in_train.most_common(15),
               "words": sum(len(r["assistant"].split()) for r in train)}
    print(json.dumps(summary, indent=1))
    if a.dry_run:
        return 0

    a.out.mkdir(parents=True, exist_ok=True)
    write_jsonl(a.out / "train.jsonl", [to_example(r, repo_w) for r in train])
    write_jsonl(a.out / "holdout.jsonl", [to_example(r, repo_w) for r in ppl_hold])
    write_jsonl(a.out / "eval_seen.jsonl", seen_all)
    write_jsonl(a.out / "eval_unseen.jsonl", unseen_all)
    if a.scramble:
        scr = []
        for r in train:
            if r["id"] in scr_by_id:
                scr.append(to_example({**r, "assistant": scr_by_id[r["id"]]}, repo_w))
            else:  # cross_repo: names are unchanged by an edge scramble, so the text stands
                assert r["kind"] == "cross_repo", r["kind"]
                scr.append(to_example(r, repo_w))
        write_jsonl(a.out / "train_scrambled.jsonl", scr)
        summary["scrambled_changed"] = sum(1 for r in train if r["id"] in scr_by_id and scr_by_id[r["id"]] != r["assistant"])
    manifest = {**summary, "seed": a.seed, "budget": a.budget, "holdout_frac": a.holdout, "repo_cap": a.repo_cap,
                "core_weight": CORE_WEIGHT, "upstream_weight": UPSTREAM_WEIGHT, "kind_weight": KIND_WEIGHT,
                "system": SYSTEM, "per_repo": per_repo_counts,
                "graph_commits": {g.repo: g.commit for g in graphs}}
    (a.out / "serialize.manifest.json").write_text(json.dumps(manifest, indent=1), encoding="utf-8")
    print(f"wrote {a.out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
