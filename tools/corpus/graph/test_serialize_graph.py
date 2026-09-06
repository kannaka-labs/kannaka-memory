"""serialize_graph tests — two synthetic graphify graphs, no real corpus.

Run: python tools/corpus/graph/test_serialize_graph.py
Pins: trainer record shape; held-out edges never appear in any train answer;
the scrambled twin has the same questions and different answers; upstream files
in a fork weigh less than novel files; core repos weigh double; determinism.
"""
import json
import os
import random
import subprocess
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, os.path.dirname(__file__))
import serialize_graph as sg  # noqa: E402


def node(nid, label, f, loc="L1", **kw):
    return {"id": nid, "label": label, "file_type": "code", "source_file": f, "source_location": loc, **kw}


def edge(s, t, rel, **kw):
    return {"source": s, "target": t, "relation": rel, "confidence": "EXTRACTED", "source_file": "x", **kw}


def graph_a():
    nodes = [node("src_main", "src/main.rs", "src/main.rs"), node("main", "main", "src/main.rs", "L3"),
             node("parse", "parse_args", "src/main.rs", "L20"), node("run", "run_loop", "src/main.rs", "L40"),
             node("src_util", "src/util.rs", "src/util.rs"), node("fmt", "format_row", "src/util.rs", "L5"),
             node("log", "log_line", "src/util.rs", "L9"), node("cfg", "Config", "src/cfg.rs", "L2", _callable=True, _callable_class=True),
             {"id": "r1", "label": "WHY: parse before run so flags gate the loop.", "file_type": "rationale",
              "source_file": "src/main.rs", "source_location": "L19"}]
    links = [edge("src_main", "main", "contains"), edge("src_main", "parse", "contains"), edge("src_main", "run", "contains"),
             edge("src_util", "fmt", "contains"), edge("src_util", "log", "contains"),
             edge("main", "parse", "calls"), edge("main", "run", "calls"), edge("run", "fmt", "calls"),
             edge("run", "log", "calls"), edge("fmt", "log", "calls"), edge("parse", "cfg", "references"),
             edge("src_main", "src_util", "imports_from"), edge("r1", "parse", "rationale_for")]
    for i in range(30):  # bulk so a 10% hold-out is non-empty
        nodes.append(node(f"h{i}", f"helper_{i}", "src/util.rs", f"L{100 + i}"))
        links.append(edge("src_util", f"h{i}", "contains"))
        links.append(edge("run", f"h{i}", "calls"))
    return {"directed": False, "nodes": nodes, "links": links, "built_at_commit": "abc"}


def graph_b():
    nodes = [node("lib", "lib.py", "lib.py"), node("f", "run_loop", "lib.py", "L2"), node("g", "emit", "lib.py", "L8"),
             node("v", "vendor.py", "vendor/vendor.py"), node("vf", "vendored_fn", "vendor/vendor.py", "L1")]
    links = [edge("lib", "f", "contains"), edge("lib", "g", "contains"), edge("f", "g", "calls"),
             edge("v", "vf", "contains"), edge("vf", "g", "calls")]
    return {"directed": False, "nodes": nodes, "links": links, "built_at_commit": "def"}


def setup(tmp: Path):
    for repo, data in (("NickFlach/kannaka-thing", graph_a()), ("NickFlach/plainfork", graph_b())):
        d = tmp / "graphs" / repo / "graphify-out"
        d.mkdir(parents=True)
        (d / "graph.json").write_text(json.dumps(data))
    nov = tmp / "novelty" / "NickFlach"
    nov.mkdir(parents=True)
    (nov / "plainfork.txt").write_text("lib.py\n")  # vendor/vendor.py is upstream


def run(tmp: Path, out: str, *extra):
    cmd = [sys.executable, os.path.join(os.path.dirname(__file__), "serialize_graph.py"), "--graphs", str(tmp / "graphs"),
           "--novelty", str(tmp / "novelty"), "--out", str(tmp / out), "--budget", "500", "--holdout", "0.25",
           "--paths-per-repo", "5", "--absent-per-repo", "1", "--eval-per-repo", "5", *extra]
    subprocess.run(cmd, check=True, capture_output=True)
    return {p.stem: [json.loads(l) for l in (tmp / out / p.name).read_text(encoding="utf-8").splitlines()]
            for p in (tmp / out).glob("*.jsonl")}


def test_shape_and_weights(tmp):
    res = run(tmp, "o1", "--scramble")
    train = res["train"]
    assert train, "no train records"
    for r in train:
        assert [m["role"] for m in r["messages"]] == ["system", "user", "assistant"]
        assert r["id"] and r["kind"] and "weight" in r
    kinds = {r["kind"] for r in train}
    assert {"callees", "callers", "explain", "file_contents", "rationale"} <= kinds, kinds
    # upstream file in the fork weighs less than the novel file; core repo weighs double
    w = {r["messages"][1]["content"]: r["weight"] for r in train}
    novel = [v for k, v in w.items() if "plainfork" in k and "`run_loop` call" in k]
    upstream = [v for k, v in w.items() if "plainfork" in k and "`vendored_fn` call" in k]
    assert novel and upstream and upstream[0] < novel[0], (novel, upstream)
    core = [v for k, v in w.items() if "kannaka-thing" in k and "`main` call" in k]
    assert core and abs(core[0] - 2.0 * novel[0]) < 1e-6, (core, novel)


def test_holdout_never_leaks(tmp):
    res = run(tmp, "o2")
    train_text = "\n".join(r["messages"][2]["content"] for r in res["train"] + res["holdout"])
    unseen = res["eval_unseen"]
    assert unseen, "no unseen evals (hold-out empty)"
    yes = [u for u in unseen if u["kind"] == "yn_unseen" and u["truth"] == "yes"]
    assert yes
    for u in yes:
        # "does `A` call `B`" — B must not be listed as a callee of A anywhere in train
        a, b = u["user"].split("`")[1], u["user"].split("`")[3]
        for line in train_text.splitlines():
            if line.startswith(f"`{a}`") and " calls " in line:
                assert f"`{b}`" not in line.split(" calls ", 1)[1], (a, b, line)
    lists = [u for u in unseen if u["kind"] == "callees_list"]
    for u in lists:
        assert set(u["held_out"]) <= set(u["truth"])


def test_scramble_and_determinism(tmp):
    r1 = run(tmp, "o3", "--scramble")
    r2 = run(tmp, "o4", "--scramble")
    assert [r["id"] for r in r1["train"]] == [r["id"] for r in r2["train"]], "not deterministic"
    scr = {r["id"]: r for r in r1["train_scrambled"]}
    assert set(scr) == {r["id"] for r in r1["train"]}
    changed = 0
    for r in r1["train"]:
        s = scr[r["id"]]
        assert s["messages"][1] == r["messages"][1], "scramble changed a question"
        changed += s["messages"][2] != r["messages"][2]
    assert changed >= len(r1["train"]) // 4, f"scramble changed only {changed}/{len(r1['train'])}"


def test_is_core():
    assert sg.is_core("NickFlach/kannaka-memory") and sg.is_core("NickFlach/Agent-Kax") and sg.is_core("NickFlach/0xSCADA")
    assert not sg.is_core("NickFlach/llmfit") and not sg.is_core("NickFlach/voicebox")


def main():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        setup(tmp)
        for t in (test_shape_and_weights, test_holdout_never_leaks, test_scramble_and_determinism):
            t(tmp)
            print("ok", t.__name__)
        test_is_core()
        print("ok test_is_core")
    print("all serialize_graph tests passed")


if __name__ == "__main__":
    main()
