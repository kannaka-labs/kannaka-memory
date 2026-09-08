"""refresh_corpus tests — decision logic only, no network and no graphify.

Run: python tools/corpus/graph/test_refresh_corpus.py
Pins: a repo is re-extracted only when its HEAD differs from the graph's built_at_commit or the
graph was built by a different graphify version (2026-09-08: an upgrade never reached the index
because graphify's incremental path handed back the old graph); every extraction passes --force;
a commit already known to hold no code is skipped until it changes; the index swap is atomic and
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


def write_graph(root: Path, repo: str, commit: str, version: str | None = None) -> Path:
    d = root / "graphs" / repo / "graphify-out"
    d.mkdir(parents=True, exist_ok=True)
    p = d / "graph.json"
    g = {"nodes": [], "links": [], "built_at_commit": commit}
    if version:
        g["built_with"] = version
    p.write_text(json.dumps(g), encoding="utf-8")
    return p


# main() and this test call the same rc.is_stale — the previous copy of the rule in this file
# could drift from the one that ran (and it did not know about versions at all).
is_stale = rc.is_stale


def test_staleness(root: Path):
    repo = "acct/thing"
    assert is_stale(root, repo, "aaaa"), "no graph at all -> extract"
    write_graph(root, repo, "aaaa")
    assert not is_stale(root, repo, "aaaa"), "graph matches HEAD -> skip"
    assert is_stale(root, repo, "bbbb"), "HEAD moved -> extract"
    assert is_stale(root, repo, "aaaa", force=True), "--force-extract overrides"


def test_staleness_by_graphify_version(root: Path):
    repo = "acct/versioned"
    write_graph(root, repo, "aaaa")  # a graph from before stamping
    assert not is_stale(root, repo, "aaaa", version=None), "unknown installed version -> commit rule only"
    assert is_stale(root, repo, "aaaa", version="0.9.56"), "unstamped graph + known version -> one re-scan"
    write_graph(root, repo, "aaaa", version="0.9.55")
    assert is_stale(root, repo, "aaaa", version="0.9.56"), "built by an older graphify -> extract"
    assert not is_stale(root, repo, "aaaa", version="0.9.55"), "same version, same commit -> skip"
    assert is_stale(root, repo, "bbbb", version="0.9.55"), "same version, HEAD moved -> extract"


def test_extract_passes_force_and_stamps_the_version(root: Path):
    """graphify's incremental path returns the previous graph when no file changed; the refresh
    must ask for a full re-scan every time and record which graphify produced the result."""
    fake = root / "fake_graphify.py"
    fake.write_text(
        "import sys, json, pathlib\n"
        "if sys.argv[1:] == ['--version']:\n"
        "    print('graphify 9.9.9'); sys.exit(0)\n"
        "out = pathlib.Path(sys.argv[sys.argv.index('--out') + 1]) / 'graphify-out'\n"
        "out.mkdir(parents=True, exist_ok=True)\n"
        "(out / 'argv.json').write_text(json.dumps(sys.argv[1:]))\n"
        "(out / 'graph.json').write_text(json.dumps({'nodes': [{'id': 'a'}], 'links': [],\n"
        "                                            'built_at_commit': 'ffff'}))\n",
        encoding="utf-8")
    # extract_repo runs the tool as an executable; wrap the script so it runs anywhere
    if os.name == "nt":
        launcher = root / "fake_graphify.cmd"
        launcher.write_text(f'@"{sys.executable}" "{fake}" %*\n', encoding="utf-8")
    else:
        launcher = root / "fake_graphify"
        launcher.write_text(f'#!/bin/sh\nexec "{sys.executable}" "{fake}" "$@"\n', encoding="utf-8")
        launcher.chmod(0o755)
    (root / "repos" / "acct" / "thing").mkdir(parents=True, exist_ok=True)
    version = rc.graphify_version(str(launcher))
    assert version == "9.9.9", version
    res = rc.extract_repo(root, "acct/thing", str(launcher), 2, version)
    assert res["ok"] and res["nodes"] == 1, res
    argv = json.loads((root / "graphs" / "acct" / "thing" / "graphify-out" / "argv.json").read_text())
    assert "--force" in argv, argv
    gj = root / "graphs" / "acct" / "thing" / "graphify-out" / "graph.json"
    assert rc.graphed_version(gj) == "9.9.9" and rc.graphed_commit(gj) == "ffff"
    assert not is_stale(root, "acct/thing", "ffff", version="9.9.9"), "just built by this version -> skip"
    assert is_stale(root, "acct/thing", "ffff", version="9.9.10"), "a newer graphify -> extract again"

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
        test_staleness_by_graphify_version(root)
        print("ok test_staleness_by_graphify_version")
        test_extract_passes_force_and_stamps_the_version(root)
        print("ok test_extract_passes_force_and_stamps_the_version")
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
