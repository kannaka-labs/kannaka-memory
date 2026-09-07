"""graph_index tests — two synthetic graphify graphs, no real corpus, no network.

Run: python tools/corpus/graph/test_graph_index.py
Pins: identifiers pulled from a question; exact label hits render a neighbourhood with file:line;
file names resolve to file nodes with their symbols; a repo named in the question wins ties;
config keys never surface; an unknown identifier yields an empty excerpt and CLI exit 3.
"""
import json
import os
import shutil
import sqlite3
import subprocess
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, os.path.dirname(__file__))
import graph_index as gi  # noqa: E402


def node(nid, label, f, loc="L1", **kw):
    return {"id": nid, "label": label, "file_type": "code", "source_file": f, "source_location": loc, **kw}


def edge(s, t, rel, conf="EXTRACTED"):
    return {"source": s, "target": t, "relation": rel, "confidence": conf}


def graph(repo_tag):
    nodes = [node("src_main", "src/main.rs", "src/main.rs"), node("main", "main", "src/main.rs", "L3"),
             node("parse", f"parse_args_{repo_tag}", "src/main.rs", "L20"), node("run", "run_loop", "src/main.rs", "L40"),
             node("fmt", "format_row()", "src/util.rs", "L5"), node("tsconfig", "tsconfig.json", "tsconfig.json"),
             node("isolated", "isolatedModules", "tsconfig.json", "L4"),
             {"id": "r1", "label": "WHY: parse before run.", "file_type": "rationale", "source_file": "src/main.rs", "source_location": "L19"}]
    links = [edge("src_main", "main", "contains"), edge("src_main", "parse", "contains"), edge("src_main", "run", "contains"),
             edge("main", "parse", "calls"), edge("main", "run", "calls"), edge("run", "fmt", "calls"),
             edge("r1", "parse", "rationale_for"), edge("tsconfig", "isolated", "contains"),
             edge("main", "fmt", "references", conf="INFERRED")]
    return {"directed": False, "nodes": nodes, "links": links, "built_at_commit": repo_tag}


def setup(tmp: Path):
    for repo, tag in (("NickFlach/kannaka-thing", "a"), ("NickFlach/other-thing", "b")):
        d = tmp / "graphs" / repo / "graphify-out"
        d.mkdir(parents=True)
        (d / "graph.json").write_text(json.dumps(graph(tag)), encoding="utf-8")


def test_identifiers():
    ids = gi.identifiers("In kannaka-thing, what does `run_loop` call? Also format_row() and src/main.rs and EmlTree.")
    assert ids[0] == "run_loop"
    assert "format_row()" in ids or "format_row" in ids
    assert "src/main.rs" in ids
    assert "EmlTree" in ids
    assert gi.identifiers("what does the code do") == []


def main():
    td = tempfile.mkdtemp()  # not TemporaryDirectory: Windows keeps the sqlite handle briefly
    try:
        tmp = Path(td)
        setup(tmp)
        out = tmp / "idx.sqlite"
        total = gi.build(tmp / "graphs", out)
        assert total["repos"] == 2 and total["nodes"] == 16, total
        db = sqlite3.connect(str(out))

        # exact symbol hit, with neighbourhood and file:line
        ex = gi.lookup(db, "In NickFlach/kannaka-thing, what does `run_loop` call?")
        assert "`run_loop` [symbol] in NickFlach/kannaka-thing at src/main.rs:L40" in ex, ex
        assert "-> calls `format_row()` (src/util.rs:L5)" in ex, ex
        assert "<- calls `main`" in ex, ex
        # the repo named in the question is preferred; the other repo's twin is second
        assert ex.index("kannaka-thing") < ex.index("other-thing") if "other-thing" in ex else True

        # a file name resolves to the file node and lists what it defines
        fx = gi.lookup(db, "what is in src/main.rs of other-thing?")
        assert "`src/main.rs` [file] in NickFlach/other-thing" in fx, fx
        assert "defines" in fx and "parse_args_b" in fx, fx

        # trailing () and case are normalised
        assert "format_row" in gi.lookup(db, "who calls FORMAT_ROW in kannaka-thing")

        # config keys never surface; unknown identifiers give nothing
        assert gi.lookup(db, "where is `isolatedModules` set?") == ""
        assert gi.lookup(db, "tell me about `no_such_symbol_here`") == ""

        # rationale reachable through the symbol it annotates
        rx = gi.lookup(db, "why does `parse_args_a` exist?")
        assert "rationale_for" in rx and "WHY: parse before run." in rx, rx

        # max_chars is honoured
        short = gi.lookup(db, "`run_loop` and `main` and `format_row`", max_chars=120)
        assert len(short) <= 121, len(short)
        db.close()

        # resolve: the typed component query KannakaHDL asks (facts, not invented scores)
        db2 = sqlite3.connect(str(out))
        hits = gi.resolve(db2, "run_loop")
        assert hits and hits[0]["label"] == "run_loop"
        assert {h["repo"] for h in hits} == {"NickFlach/kannaka-thing", "NickFlach/other-thing"}
        assert hits[0]["id"].startswith("NickFlach/") and "src/main.rs:L40" in hits[0]["id"]
        assert hits[0]["in_degree"] == 1 and hits[0]["out_degree"] == 1
        # material narrows to one repo, tolerantly (bare name or full name)
        assert len(gi.resolve(db2, "run_loop", repo="kannaka-thing")) == 1
        assert len(gi.resolve(db2, "run_loop", repo="NickFlach/kannaka-thing")) == 1
        assert gi.resolve(db2, "run_loop", repo="no-such-repo") == []
        # type narrows by kind; a file is not a symbol
        assert gi.resolve(db2, "src/main.rs", kind="symbol") == []
        assert gi.resolve(db2, "src/main.rs", kind="file")[0]["kind"] == "file"
        # confidence is carried so a caller can weigh EXTRACTED against INFERRED
        assert gi.has_confidence(db2)
        fmt = gi.resolve(db2, "format_row", repo="kannaka-thing")[0]
        assert fmt["extracted"] == 1 and fmt["inferred"] == 1, fmt
        # config keys are never components
        assert gi.resolve(db2, "isolatedModules") == []
        db2.close()

        # CLI: exit 3 when nothing is recorded, 0 with text otherwise
        here = os.path.join(os.path.dirname(__file__), "graph_index.py")
        r = subprocess.run([sys.executable, here, "query", "--index", str(out), "`nothing_like_this`"], capture_output=True, text=True)
        assert r.returncode == 3 and r.stdout == "", (r.returncode, r.stdout)
        r = subprocess.run([sys.executable, here, "query", "--index", str(out), "what calls `format_row`?"], capture_output=True, text=True)
        assert r.returncode == 0 and "format_row" in r.stdout
        r = subprocess.run([sys.executable, here, "stats", "--index", str(out)], capture_output=True, text=True)
        assert json.loads(r.stdout)["repos"] == 2
        r = subprocess.run([sys.executable, here, "resolve", "--index", str(out), "--class", "run_loop"],
                           capture_output=True, text=True)
        env = json.loads(r.stdout)
        assert r.returncode == 0 and env["schema_version"] == "code-graph-resolve/1" and env["confidence"] is True
        assert env["data"][0]["label"] == "run_loop"
        r = subprocess.run([sys.executable, here, "resolve", "--index", str(out), "--class", "nope_not_here"],
                           capture_output=True, text=True)
        assert r.returncode == 3 and json.loads(r.stdout)["data"] == []
    finally:
        shutil.rmtree(td, ignore_errors=True)
    test_identifiers()
    print("all graph_index tests passed")


if __name__ == "__main__":
    main()
