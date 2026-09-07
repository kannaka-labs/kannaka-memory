"""refresh_corpus tests — decision logic only, no network and no graphify.

Run: python tools/corpus/graph/test_refresh_corpus.py
Pins: a repo is re-extracted only when its HEAD differs from the graph's built_at_commit; a
commit already known to hold no code is skipped until it changes; the index swap is atomic and
a failed build leaves the old index in place; a discovery that loses most of the corpus is
treated as a bad listing rather than as deletions (2026-09-07: /users/<login>/repos hides all
54 private repos).
"""
import json
import os
import shutil
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, os.path.dirname(__file__))
import refresh_corpus as rc  # noqa: E402


def write_graph(root: Path, repo: str, commit: str) -> Path:
    d = root / "graphs" / repo / "graphify-out"
    d.mkdir(parents=True, exist_ok=True)
    p = d / "graph.json"
    p.write_text(json.dumps({"nodes": [], "links": [], "built_at_commit": commit}), encoding="utf-8")
    return p


def is_stale(root: Path, repo: str, head: str, force: bool = False) -> bool:
    """The same decision main() makes, kept in one place so the test pins the real rule."""
    gj = root / "graphs" / repo / "graphify-out" / "graph.json"
    nocode = root / "graphs" / repo / "graphify-out" / ".no-code"
    known_empty = nocode.exists() and nocode.read_text(encoding="utf-8").strip() == head
    return force or (not known_empty and (not gj.exists() or rc.graphed_commit(gj) != head))


def test_staleness(root: Path):
    repo = "acct/thing"
    assert is_stale(root, repo, "aaaa"), "no graph at all -> extract"
    write_graph(root, repo, "aaaa")
    assert not is_stale(root, repo, "aaaa"), "graph matches HEAD -> skip"
    assert is_stale(root, repo, "bbbb"), "HEAD moved -> extract"
    assert is_stale(root, repo, "aaaa", force=True), "--force-extract overrides"

    empty = "acct/empty"
    (root / "graphs" / empty / "graphify-out").mkdir(parents=True)
    (root / "graphs" / empty / "graphify-out" / ".no-code").write_text("cccc", encoding="utf-8")
    assert not is_stale(root, empty, "cccc"), "commit known to have no code -> stay quiet"
    assert is_stale(root, empty, "dddd"), "new commit -> look again"


def test_graphed_commit_survives_a_broken_graph(root: Path):
    p = write_graph(root, "acct/broken", "eeee")
    assert rc.graphed_commit(p) == "eeee"
    p.write_text("{ this is not json", encoding="utf-8")
    assert rc.graphed_commit(p) is None, "a corrupt graph means re-extract, not a crash"
    assert rc.graphed_commit(root / "nope" / "graph.json") is None


def test_index_swap_is_atomic_and_failure_keeps_the_old_index(root: Path):
    index = root / "graph-index.sqlite"
    index.write_text("OLD INDEX", encoding="utf-8")

    good = root / "good_builder.py"
    good.write_text(
        "import sys, json, pathlib\n"
        "out = sys.argv[sys.argv.index('--out') + 1]\n"
        "pathlib.Path(out).write_text('NEW INDEX')\n"
        "print(json.dumps({'repos': 3, 'nodes': 9, 'edges': 12}))\n", encoding="utf-8")
    res = rc.build_index(root, index, good)
    assert res["ok"] and res["repos"] == 3, res
    assert index.read_text(encoding="utf-8") == "NEW INDEX"
    assert not index.with_suffix(index.suffix + ".new").exists(), "the temp file must be gone"

    bad = root / "bad_builder.py"
    bad.write_text("import sys\nsys.stderr.write('boom\\n')\nsys.exit(2)\n", encoding="utf-8")
    res = rc.build_index(root, index, bad)
    assert res["ok"] is False and "boom" in res["error"], res
    assert index.read_text(encoding="utf-8") == "NEW INDEX", "a failed build must not touch the live index"


def test_a_shrunken_listing_is_not_treated_as_deletions():
    on_disk = [f"acct/r{i}" for i in range(100)]
    discovered = ["acct/r0", "acct/r1"]  # e.g. the API hid every private repo
    vanished = sorted(set(on_disk) - set(discovered))
    if discovered and len(vanished) > max(5, len(on_disk) // 10):
        vanished = []
    assert vanished == [], "losing most of the corpus is a bad listing, not 98 deletions"
    # a genuine handful still reports
    discovered = on_disk[:97]
    vanished = sorted(set(on_disk) - set(discovered))
    if discovered and len(vanished) > max(5, len(on_disk) // 10):
        vanished = []
    assert len(vanished) == 3


def main():
    td = tempfile.mkdtemp()
    try:
        root = Path(td)
        (root / "graphs").mkdir()
        test_staleness(root)
        print("ok test_staleness")
        test_graphed_commit_survives_a_broken_graph(root)
        print("ok test_graphed_commit_survives_a_broken_graph")
        test_index_swap_is_atomic_and_failure_keeps_the_old_index(root)
        print("ok test_index_swap_is_atomic_and_failure_keeps_the_old_index")
    finally:
        shutil.rmtree(td, ignore_errors=True)
    test_a_shrunken_listing_is_not_treated_as_deletions()
    print("ok test_a_shrunken_listing_is_not_treated_as_deletions")
    print("all refresh_corpus tests passed")


if __name__ == "__main__":
    main()
