#!/usr/bin/env python3
"""ADR-0057 — a queryable index over the constellation's code graphs, for retrieval in context.

The code-knowledge experiment (2026-09-06) showed that a model with the right graph excerpt in
context answers code questions almost perfectly, while the same facts trained into an adapter
are barely queryable. This module is the retrieval side: it folds every per-repo graphify graph
into one SQLite file and answers a plain question with a compact excerpt, the same shape the
eval arms used (`serialize_graph.RepoGraph.context`).

  build   graph_index.py build --graphs <dir> --out graph-index.sqlite
  query   graph_index.py query --index graph-index.sqlite "what does sendRow call in Agent-Kax?"

Lookup is deliberately dumb and exact: identifiers are pulled out of the question (backticked
spans, snake_case / CamelCase / dotted tokens, file names, repo names), matched against node
labels (exact first, then prefix), and each hit's 1-hop neighbourhood is rendered. No embeddings.
When nothing matches, the answer is an empty string, so a caller can say "not recorded" honestly
instead of improvising. Zero LLM, zero network.
"""
from __future__ import annotations

import argparse
import json
import re
import sqlite3
import sys
from pathlib import Path

CONFIG_EXT = (".json", ".yaml", ".yml", ".toml", ".lock", ".ini", ".cfg", ".env", ".csv", ".md")
SCHEMA = """
CREATE TABLE IF NOT EXISTS repos(repo TEXT PRIMARY KEY, nodes INTEGER, edges INTEGER, commit_sha TEXT);
CREATE TABLE IF NOT EXISTS nodes(repo TEXT, id TEXT, label TEXT, norm TEXT, kind TEXT, file TEXT, loc TEXT,
                                 PRIMARY KEY(repo, id));
CREATE INDEX IF NOT EXISTS nodes_norm ON nodes(norm);
CREATE INDEX IF NOT EXISTS nodes_file ON nodes(repo, file);
CREATE TABLE IF NOT EXISTS edges(repo TEXT, src TEXT, dst TEXT, rel TEXT, conf TEXT, loc TEXT);
CREATE INDEX IF NOT EXISTS edges_src ON edges(repo, src);
CREATE INDEX IF NOT EXISTS edges_dst ON edges(repo, dst);
"""


def norm(label: str) -> str:
    return re.sub(r"\(\)$", "", (label or "").strip()).lower()


def _kind(n: dict, contains_src: set[str]) -> str:
    ft = n.get("file_type")
    if ft == "rationale":
        return "rationale"
    if ft == "concept":
        return "concept"
    if (n.get("source_file") or "").lower().endswith(CONFIG_EXT):
        return "config"
    if n.get("_callable_class"):
        return "class"
    if n["id"] in contains_src or n.get("label") == n.get("source_file"):
        return "file"
    return "symbol"


# ---------------------------------------------------------------- build

def build(graphs_dir: Path, out: Path) -> dict:
    if out.exists():
        out.unlink()
    db = sqlite3.connect(str(out))
    db.executescript(SCHEMA)
    total = {"repos": 0, "nodes": 0, "edges": 0}
    for p in sorted(graphs_dir.glob("*/*/graphify-out/graph.json")):
        repo = "/".join(p.parts[-4:-2])
        g = json.loads(p.read_text(encoding="utf-8"))
        nodes = g.get("nodes", [])
        links = g.get("links", g.get("edges", []))
        contains_src = {e["source"] for e in links if e.get("relation") == "contains"}
        rows = [(repo, n["id"], n.get("label") or n["id"], norm(n.get("label") or n["id"]), _kind(n, contains_src),
                 n.get("source_file") or "", n.get("source_location") or "") for n in nodes]
        db.executemany("INSERT OR REPLACE INTO nodes VALUES (?,?,?,?,?,?,?)", rows)
        erows = [(repo, e["source"], e["target"], e.get("relation") or "", e.get("confidence") or "",
                  e.get("source_location") or "")
                 for e in links if e.get("relation") != "contains"]
        db.executemany("INSERT INTO edges VALUES (?,?,?,?,?,?)", erows)
        db.execute("INSERT OR REPLACE INTO repos VALUES (?,?,?,?)", (repo, len(rows), len(erows), g.get("built_at_commit")))
        total["repos"] += 1
        total["nodes"] += len(rows)
        total["edges"] += len(erows)
    db.commit()
    db.execute("VACUUM")
    db.close()
    return total


# ---------------------------------------------------------------- query

_IDENT = re.compile(r"`([^`]{2,80})`")
_CODEISH = re.compile(r"\b(?:[A-Za-z_][A-Za-z0-9_]*(?:\.[A-Za-z_][A-Za-z0-9_]*)+|[A-Za-z0-9]+_[A-Za-z0-9_]+|[A-Z][a-z0-9]+(?:[A-Z][a-z0-9]+)+|[a-z][a-zA-Z0-9]*[A-Z][a-zA-Z0-9]*|[\w./-]+\.(?:py|rs|ts|tsx|js|mjs|sh|go|c|h|toml|yaml|yml|sql))\b")
STOP = {"what", "does", "call", "calls", "where", "which", "file", "defines", "define", "the", "in", "of", "and",
        "for", "how", "is", "are", "to", "a", "an", "it", "this", "that", "list", "everything", "repo", "code"}


def identifiers(question: str) -> list[str]:
    seen: list[str] = []
    for m in _IDENT.findall(question):
        m = m.strip()
        if m and m not in seen:
            seen.append(m)
    for m in _CODEISH.findall(question):
        if m.lower() not in STOP and m not in seen and len(m) >= 3:
            seen.append(m)
    return seen[:8]


def repo_hints(db: sqlite3.Connection, question: str) -> list[str]:
    q = question.lower()
    hits = []
    for (repo,) in db.execute("SELECT repo FROM repos"):
        name = repo.split("/", 1)[-1].lower()
        if name in q or repo.lower() in q:
            hits.append(repo)
    return hits


def _name(db, repo, nid):
    r = db.execute("SELECT label, file, loc FROM nodes WHERE repo=? AND id=?", (repo, nid)).fetchone()
    if not r:
        return f"`{nid}`"
    label, f, loc = r
    where = f"{f}:{loc}" if f and loc else f
    return f"`{label}`" + (f" ({where})" if where else "")


def render(db: sqlite3.Connection, repo: str, nid: str, max_edges: int = 12) -> str:
    r = db.execute("SELECT label, kind, file, loc FROM nodes WHERE repo=? AND id=?", (repo, nid)).fetchone()
    if not r:
        return ""
    label, kind, f, loc = r
    where = f"{f}:{loc}" if f and loc else f or "(unknown file)"
    lines = [f"`{label}` [{kind}] in {repo} at {where}"]
    outs = db.execute("SELECT rel, dst FROM edges WHERE repo=? AND src=? LIMIT ?", (repo, nid, max_edges)).fetchall()
    for rel, dst in outs:
        lines.append(f"  -> {rel} {_name(db, repo, dst)}")
    ins = db.execute("SELECT rel, src FROM edges WHERE repo=? AND dst=? LIMIT ?", (repo, nid, max_edges)).fetchall()
    for rel, src in ins:
        lines.append(f"  <- {rel} {_name(db, repo, src)}")
    if kind == "file":
        syms = db.execute("SELECT label FROM nodes WHERE repo=? AND file=? AND kind IN ('symbol','class') LIMIT 20",
                          (repo, f)).fetchall()
        if syms:
            lines.append("  defines " + ", ".join(f"`{s[0]}`" for s in syms))
    return "\n".join(lines)


def lookup(db: sqlite3.Connection, question: str, max_nodes: int = 4, max_chars: int = 1800) -> str:
    idents = identifiers(question)
    if not idents:
        return ""
    prefer = repo_hints(db, question)
    chosen: list[tuple[str, str]] = []
    for ident in idents:
        n = norm(ident)
        base = n.rsplit("/", 1)[-1]
        rows = db.execute(
            "SELECT repo, id, kind FROM nodes WHERE norm=? OR norm=? OR (kind='file' AND (file=? OR file LIKE ?)) "
            "ORDER BY CASE kind WHEN 'file' THEN 1 WHEN 'class' THEN 2 WHEN 'symbol' THEN 3 ELSE 9 END LIMIT 40",
            (n, base, ident, f"%/{base}")).fetchall()
        if not rows and len(n) >= 5:
            rows = db.execute("SELECT repo, id, kind FROM nodes WHERE norm LIKE ? AND kind IN ('symbol','class','file') "
                              "ORDER BY length(norm) LIMIT 12", (n + "%",)).fetchall()
        rows = [r for r in rows if r[2] != "config"]
        if prefer:
            rows.sort(key=lambda r: r[0] not in prefer)
        for repo, nid, _k in rows[:2]:
            if (repo, nid) not in chosen:
                chosen.append((repo, nid))
        if len(chosen) >= max_nodes:
            break
    parts = []
    used = 0
    for repo, nid in chosen[:max_nodes]:
        block = render(db, repo, nid)
        if not block:
            continue
        if used + len(block) > max_chars:
            block = block[: max(0, max_chars - used)]
        parts.append(block)
        used += len(block) + 1
        if used >= max_chars:
            break
    return "\n".join(parts).strip()


def has_confidence(db: sqlite3.Connection) -> bool:
    """False for an index built before edges carried confidence — report nothing rather than
    inventing a certainty the file cannot support."""
    return any(r[1] == "conf" for r in db.execute("PRAGMA table_info(edges)"))


def resolve(db: sqlite3.Connection, name: str, kind: str | None = None, repo: str | None = None,
            limit: int = 8) -> list[dict]:
    """Components matching a typed query, as facts.

    `name` matches a node label exactly after normalisation (case, trailing "()"), then by
    prefix. `repo` matches the full name or the bare name, tolerantly — the same rule the crystal
    registry uses for class names. Config-file keys are never components.
    """
    conf = has_confidence(db)
    n = norm(name)
    rows = db.execute(
        "SELECT repo, id, label, kind, file, loc FROM nodes WHERE (norm=? OR norm=?) AND kind != 'config'",
        (n, n.rsplit("/", 1)[-1])).fetchall()
    if not rows and len(n) >= 5:
        rows = db.execute(
            "SELECT repo, id, label, kind, file, loc FROM nodes WHERE norm LIKE ? AND kind != 'config' "
            "ORDER BY length(norm) LIMIT 40", (n + "%",)).fetchall()
    if kind:
        rows = [r for r in rows if r[3] == kind]
    if repo:
        want = repo.strip().lower()
        rows = [r for r in rows if r[0].lower() == want or r[0].split("/", 1)[-1].lower() == want]
    out = []
    for repo_name, nid, label, k, f, loc in rows[: max(limit, 1) * 4]:
        ins = db.execute("SELECT COUNT(*) FROM edges WHERE repo=? AND dst=?", (repo_name, nid)).fetchone()[0]
        outs = db.execute("SELECT COUNT(*) FROM edges WHERE repo=? AND src=?", (repo_name, nid)).fetchone()[0]
        extracted = inferred = None
        if conf:
            extracted = db.execute(
                "SELECT COUNT(*) FROM edges WHERE repo=? AND (src=? OR dst=?) AND conf='EXTRACTED'",
                (repo_name, nid, nid)).fetchone()[0]
            inferred = db.execute(
                "SELECT COUNT(*) FROM edges WHERE repo=? AND (src=? OR dst=?) AND conf='INFERRED'",
                (repo_name, nid, nid)).fetchone()[0]
        out.append({
            "id": f"{repo_name}@{f}:{loc}#{label}" if f else f"{repo_name}#{label}",
            "label": label, "kind": k, "repo": repo_name, "file": f, "loc": loc,
            "in_degree": ins, "out_degree": outs, "extracted": extracted, "inferred": inferred,
        })
    out.sort(key=lambda d: (-d["in_degree"], d["label"]))
    return out[:limit]


def open_index(path: Path) -> sqlite3.Connection:
    db = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
    return db


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    b = sub.add_parser("build")
    b.add_argument("--graphs", required=True, type=Path)
    b.add_argument("--out", required=True, type=Path)
    q = sub.add_parser("query")
    q.add_argument("--index", required=True, type=Path)
    q.add_argument("--max-chars", type=int, default=1800)
    q.add_argument("--max-nodes", type=int, default=4)
    q.add_argument("question")
    s = sub.add_parser("stats")
    s.add_argument("--index", required=True, type=Path)
    rs = sub.add_parser("resolve", help="typed component query -> JSON (the KannakaHDL contract)")
    rs.add_argument("--index", required=True, type=Path)
    rs.add_argument("--class", dest="klass", required=True, help="component name (a node label)")
    rs.add_argument("--type", default=None, help="symbol | class | file | rationale | concept")
    rs.add_argument("--material", default=None, help="repo, full name or bare name")
    rs.add_argument("--limit", type=int, default=8)
    a = ap.parse_args(argv)
    if a.cmd == "build":
        print(json.dumps(build(a.graphs, a.out)))
        return 0
    db = open_index(a.index)
    if a.cmd == "resolve":
        data = resolve(db, a.klass, a.type, a.material, a.limit)
        print(json.dumps({"schema_version": "code-graph-resolve/1", "confidence": has_confidence(db),
                          "data": data}))
        return 0 if data else 3  # 3 = nothing recorded, same as query
    if a.cmd == "stats":
        n, e, r = db.execute("SELECT (SELECT COUNT(*) FROM nodes), (SELECT COUNT(*) FROM edges), (SELECT COUNT(*) FROM repos)").fetchone()
        print(json.dumps({"repos": r, "nodes": n, "edges": e}))
        return 0
    out = lookup(db, a.question, max_nodes=a.max_nodes, max_chars=a.max_chars)
    sys.stdout.write(out + ("\n" if out else ""))
    return 0 if out else 3  # 3 = nothing recorded (a caller can branch on it)


if __name__ == "__main__":
    raise SystemExit(main())
