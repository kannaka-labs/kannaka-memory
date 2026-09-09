#!/usr/bin/env python3
"""ADR-0057 — refresh the code-graph corpus and index.

The graphs the Archivist reads are only as true as their last build. This walks the whole corpus:
discover repos, update the clones, re-extract only what moved, and swap in a new index atomically
so a live reader never sees a half-built file.

  refresh_corpus.py --root ~/graphify-corpus            # the nightly job
  refresh_corpus.py --root ... --only kannaka --dry-run # plan one slice, touch nothing

Stages
  1 discover  both accounts' and both organisations' non-fork repos from the GitHub API (token at
              --token-file), plus the pinned fork list; writes source-repos.tsv. A repo that has
              vanished is left on disk and reported, never silently deleted. A repo that moved
              from an account into an organisation (kannaka-labs 2026-09-07, spacechild-labs
              2026-09-09) is recognised by name and its checkout and graph are renamed to the new
              owner, so it is neither cloned twice nor counted as vanished.
  2 sync      clone what is new, `fetch --depth 1` + `reset --hard` what exists. Shallow clones
              stay shallow; a repo whose default branch was renamed is re-pointed.
  3 extract   re-run graphify ONLY where the checkout's HEAD differs from the graph's
              built_at_commit, or the graph was built by a different graphify version than the
              one installed (stamped as built_with), or no graph exists. This is the whole
              point: a no-op refresh costs one fetch per repo and no CPU. Every extraction is
              `graphify extract --force` — a full re-scan. graphify's own incremental path reuses
              the previous graph.json wholesale when no file changed, which on 2026-09-08 made a
              forced refresh after a graphify upgrade a 1-second no-op per repo.
  4 index     build to <index>.new, fsync, then os.replace() onto the live path. Readers holding
              the old file keep reading it; the next query opens the new one. A failed build
              leaves the previous index exactly where it was.

Exit 0 even when individual repos fail: a corpus refresh must not be all-or-nothing. Failures are
counted in the manifest and printed; exit 1 only if the index could not be built at all.
"""
from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import time
import urllib.error
import urllib.request
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

ACCOUNTS = ("NickFlach", "flaukowski")
ORGS = ("kannaka-labs", "spacechild-labs")
API = "https://api.github.com"


def log(msg: str) -> None:
    print(f"[{time.strftime('%H:%M:%S')}] [refresh] {msg}", flush=True)


def run(cmd: list[str], cwd: Path | None = None, timeout: int = 600) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, cwd=str(cwd) if cwd else None, capture_output=True, text=True,
                          timeout=timeout, check=False,
                          env={**os.environ, "GIT_TERMINAL_PROMPT": "0"})


# ---------------------------------------------------------------- 1. discover

def api_get(url: str, token: str | None) -> list[dict]:
    """One page of the GitHub API, with the token when we have one."""
    req = urllib.request.Request(url, headers={
        "Accept": "application/vnd.github+json",
        "User-Agent": "kannaka-graph-refresh",
        **({"Authorization": f"Bearer {token}"} if token else {}),
    })
    with urllib.request.urlopen(req, timeout=60) as r:
        return json.load(r)


def authenticated_login(token: str | None) -> str | None:
    if not token:
        return None
    try:
        return (api_get(f"{API}/user", token) or {}).get("login")  # type: ignore[union-attr]
    except Exception:  # noqa: BLE001 - no token, no network: fall back to public listings
        return None


def discover(token: str | None) -> tuple[list[str], dict[str, str]]:
    """(non-fork repos, {repo: default_branch}) across both accounts and both organisations.

    /users/<login>/repos lists only PUBLIC repos even with a token, so the account that owns the
    token is read from /user/repos instead — otherwise every private repo looks like it vanished
    and a new private repo is never discovered (2026-09-07: 54 of them). An organisation's
    repos come from /orgs/<org>/repos, which lists private ones for a member's token; the
    affiliation=owner listing never includes them, which is why the 26 repos moved to
    kannaka-labs read as "29 on disk missing from discovery" on 2026-09-08."""
    me = authenticated_login(token)
    repos: list[str] = []
    branches: dict[str, str] = {}
    seen_any = False
    for account in ACCOUNTS + ORGS:
        if account in ORGS:
            url = f"{API}/orgs/{account}/repos?per_page=100&type=all"
        else:
            url = (f"{API}/user/repos?per_page=100&affiliation=owner"
                   if me and account.lower() == me.lower()
                   else f"{API}/users/{account}/repos?per_page=100&type=owner")
        page = 1
        while True:
            try:
                rows = api_get(f"{url}&page={page}", token)
            except urllib.error.HTTPError as e:
                log(f"discover {account} page {page}: HTTP {e.code} — keeping the existing list")
                return [], {}
            if not rows:
                break
            seen_any = True
            for r in rows:
                if r.get("fork") or r.get("size", 0) == 0:
                    continue
                if (r.get("owner") or {}).get("login", "").lower() != account.lower():
                    continue  # /user/repos can carry org repos too
                repos.append(r["full_name"])
                branches[r["full_name"]] = r.get("default_branch") or "main"
            page += 1
            if len(rows) < 100:
                break
    if not seen_any:
        return [], {}
    return sorted(set(repos)), branches


def migrate_moved(root: Path, discovered: list[str]) -> list[tuple[str, str]]:
    """A repo discovered under an organisation whose checkout still sits under a personal
    account (same name, exactly one candidate) is the same repository after a transfer: rename
    its checkout and its graph directory to the new owner and repoint origin. Without this the
    org copy would be cloned fresh beside the old one, both would be extracted, and the index
    would carry every moved repo twice. Two same-named checkouts under different accounts are
    ambiguous and are left alone with a log line."""
    moves: list[tuple[str, str]] = []
    for full in discovered:
        owner, _, name = full.partition("/")
        if owner not in ORGS or (root / "repos" / owner / name).exists():
            continue
        candidates = [f"{acct}/{name}" for acct in ACCOUNTS if (root / "repos" / acct / name / ".git").exists()]
        if len(candidates) != 1:
            if candidates:
                log(f"  {full}: {len(candidates)} same-named checkouts ({', '.join(candidates)}) — not migrating")
            continue
        old = candidates[0]
        for kind in ("repos", "graphs"):
            src, dst = root / kind / old, root / kind / full
            if src.exists():
                dst.parent.mkdir(parents=True, exist_ok=True)
                os.rename(src, dst)
        r = run(["git", "remote", "set-url", "origin", f"https://github.com/{full}.git"], cwd=root / "repos" / full, timeout=60)
        log(f"  moved {old} -> {full}" + ("" if r.returncode == 0 else f" (remote not repointed: {(r.stderr or '').strip()[:80]})"))
        moves.append((old, full))
    return moves


# ---------------------------------------------------------------- 2. sync

def head_of(path: Path) -> str | None:
    r = run(["git", "rev-parse", "HEAD"], cwd=path, timeout=60)
    return r.stdout.strip() if r.returncode == 0 else None


def graphed_commit(graph_json: Path) -> str | None:
    try:
        return json.loads(graph_json.read_text(encoding="utf-8")).get("built_at_commit")
    except Exception:  # noqa: BLE001 - a missing or broken graph simply means "re-extract"
        return None


def graphed_version(graph_json: Path) -> str | None:
    """The graphify version that built this graph (our `built_with` stamp), or None for a graph
    from before stamping — which is treated as stale once, so it gets one full re-scan."""
    try:
        return json.loads(graph_json.read_text(encoding="utf-8")).get("built_with")
    except Exception:  # noqa: BLE001
        return None


def graphify_version(graphify: str) -> str | None:
    """`graphify --version` -> "0.9.56". graphify records its version nowhere in its output, so
    the refresh has to ask and stamp it itself."""
    try:
        r = run([graphify, "--version"], timeout=60)
    except (OSError, subprocess.SubprocessError):
        return None
    out = (r.stdout or r.stderr or "").strip().split()
    return out[-1] if r.returncode == 0 and out else None


def stamp_version(graph_json: Path, version: str | None) -> None:
    """Write `built_with` into graph.json beside graphify's own built_at_commit, atomically."""
    if not version:
        return
    try:
        g = json.loads(graph_json.read_text(encoding="utf-8"))
    except Exception:  # noqa: BLE001 - not our graph to fix
        return
    g["built_with"] = version
    tmp = graph_json.with_suffix(".json.tmp")
    tmp.write_text(json.dumps(g), encoding="utf-8")
    os.replace(tmp, graph_json)


def is_stale(root: Path, repo: str, head: str, version: str | None = None, force: bool = False) -> bool:
    """The one rule main() uses: extract when forced, when there is no graph, when the checkout
    moved past the graph's commit, or when the installed graphify is not the one that built it.
    A commit already known to hold no code stays quiet until it changes."""
    if force:
        return True
    out = root / "graphs" / repo / "graphify-out"
    nocode = out / ".no-code"
    if nocode.exists() and nocode.read_text(encoding="utf-8").strip() == head:
        return False
    gj = out / "graph.json"
    if not gj.exists() or graphed_commit(gj) != head:
        return True
    return bool(version) and graphed_version(gj) != version


def sync_repo(root: Path, repo: str, branch: str) -> dict:
    """Clone or fast-forward one repo. Returns {repo, action, head, error}."""
    dest = root / "repos" / repo
    if not (dest / ".git").exists():
        dest.parent.mkdir(parents=True, exist_ok=True)
        r = run(["git", "clone", "--depth", "1", "--no-tags", "--quiet",
                 f"https://github.com/{repo}.git", str(dest)], timeout=900)
        if r.returncode != 0:
            return {"repo": repo, "action": "clone-failed", "error": r.stderr.strip()[:200]}
        return {"repo": repo, "action": "cloned", "head": head_of(dest)}
    before = head_of(dest)
    r = run(["git", "fetch", "--depth", "1", "--no-tags", "--quiet", "origin", branch], cwd=dest, timeout=900)
    if r.returncode != 0:  # the default branch may have been renamed
        r = run(["git", "fetch", "--depth", "1", "--no-tags", "--quiet", "origin", "HEAD"], cwd=dest, timeout=900)
        if r.returncode != 0:
            return {"repo": repo, "action": "fetch-failed", "head": before, "error": r.stderr.strip()[:200]}
    if run(["git", "reset", "--hard", "--quiet", "FETCH_HEAD"], cwd=dest, timeout=300).returncode != 0:
        return {"repo": repo, "action": "reset-failed", "head": before}
    after = head_of(dest)
    return {"repo": repo, "action": "updated" if after != before else "unchanged", "head": after}


# ---------------------------------------------------------------- 3. extract

def extract_repo(root: Path, repo: str, graphify: str, workers: int, version: str | None = None) -> dict:
    src = root / "repos" / repo
    out = root / "graphs" / repo
    out.mkdir(parents=True, exist_ok=True)
    t0 = time.time()
    # --force: a full re-scan every time we decide to extract. Without it graphify's incremental
    # path keeps the previous graph.json when no file changed, so a re-extract after a graphify
    # upgrade returned the old graph in one second and the upgrade never reached the index.
    r = run([graphify, "extract", str(src), "--code-only", "--force", "--max-workers", str(workers),
             "--out", str(out)], timeout=3600)
    gj = out / "graphify-out" / "graph.json"
    if r.returncode != 0 or not gj.exists():
        tail = (r.stdout or r.stderr or "").strip().splitlines()[-1:] or [""]
        err = tail[0][:160]
        # A repo with no source at all (LICENSE only, a .zip, a lone shell script) fails every
        # single night and teaches the reader to skip the failure list. Remember the commit we
        # already know has no code, and stay quiet until it changes.
        if "found 0 code" in (r.stdout or "") or "not classified" in (r.stdout or ""):
            head = head_of(src)
            if head:
                (out / "graphify-out").mkdir(parents=True, exist_ok=True)
                (out / "graphify-out" / ".no-code").write_text(head, encoding="utf-8")
            return {"repo": repo, "ok": True, "no_code": True, "seconds": round(time.time() - t0, 1),
                    "nodes": 0, "edges": 0}
        return {"repo": repo, "ok": False, "seconds": round(time.time() - t0, 1), "error": err}
    stamp_version(gj, version)
    try:
        g = json.loads(gj.read_text(encoding="utf-8"))
        nodes, edges = len(g.get("nodes", [])), len(g.get("links", g.get("edges", [])))
    except Exception:  # noqa: BLE001
        nodes = edges = -1
    return {"repo": repo, "ok": True, "seconds": round(time.time() - t0, 1), "nodes": nodes, "edges": edges}


# ---------------------------------------------------------------- 4. index

def build_index(root: Path, index: Path, builder: Path) -> dict:
    """Build beside the live index, then replace it in one atomic step."""
    tmp = index.with_suffix(index.suffix + ".new")
    if tmp.exists():
        tmp.unlink()
    r = run([sys.executable, str(builder), "build", "--graphs", str(root / "graphs"), "--out", str(tmp)],
            timeout=3600)
    if r.returncode != 0 or not tmp.exists():
        return {"ok": False, "error": (r.stderr or r.stdout).strip()[-200:]}
    try:
        stats = json.loads(r.stdout.strip().splitlines()[-1])
    except Exception:  # noqa: BLE001
        stats = {}
    os.replace(tmp, index)  # atomic on the same filesystem; open readers keep the old inode
    # Durability, not atomicity: os.replace is atomic either way, but the rename itself only
    # survives a power loss once the directory entry is on disk. Best-effort — a read-mode
    # fsync raises EBADF on Windows, and directory fds do not exist there at all.
    try:
        dfd = os.open(str(index.parent), os.O_RDONLY)
        try:
            os.fsync(dfd)
        finally:
            os.close(dfd)
    except (OSError, AttributeError):
        pass
    return {"ok": True, **stats, "bytes": index.stat().st_size}


# ---------------------------------------------------------------- main

def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--root", required=True, type=Path)
    ap.add_argument("--index", default=None, type=Path, help="default <root>/graph-index.sqlite")
    ap.add_argument("--graphify", default=str(Path.home() / ".local/bin/graphify"))
    ap.add_argument("--builder", default=None, type=Path, help="graph_index.py (default beside this file)")
    ap.add_argument("--token-file", default=str(Path.home() / ".github-token"))
    ap.add_argument("--only", default=None, help="regex on owner/name")
    ap.add_argument("--jobs", type=int, default=4, help="repos extracted in parallel")
    ap.add_argument("--workers", type=int, default=4, help="AST workers per repo")
    ap.add_argument("--skip-pull", action="store_true")
    ap.add_argument("--force-extract", action="store_true", help="re-extract even when HEAD is unchanged")
    ap.add_argument("--dry-run", action="store_true")
    a = ap.parse_args(argv)

    root: Path = a.root.expanduser()
    index: Path = (a.index or root / "graph-index.sqlite").expanduser()
    builder: Path = (a.builder or Path(__file__).parent / "graph_index.py").expanduser()
    token = None
    tf = Path(a.token_file).expanduser()
    if tf.exists():
        token = tf.read_text(encoding="utf-8").strip() or None
    started = time.time()

    # 1. discover
    discovered, branches = discover(token)
    migrated = migrate_moved(root, discovered) if discovered and not a.dry_run else []
    if migrated:
        log(f"{len(migrated)} checkout(s) followed their repository into an organisation")
    on_disk = sorted("/".join(p.parts[-2:]) for p in (root / "repos").glob("*/*") if (p / ".git").exists())
    forks_file = root / "fork-repos.tsv"
    forks = [ln.split("\t")[0].strip() for ln in forks_file.read_text(encoding="utf-8").splitlines()
             if ln.strip()] if forks_file.exists() else []
    wanted = sorted(set(discovered) | set(forks) | set(on_disk)) if discovered else sorted(set(on_disk))
    vanished = sorted(set(on_disk) - set(discovered) - set(forks)) if discovered else []
    if discovered and len(vanished) > max(5, len(on_disk) // 10):
        log(f"discovery returned {len(discovered)} repos but {len(vanished)} on disk are missing from it — "
            "treating that as a bad listing (auth/scope?), not as deletions")
        vanished = []
    if a.only:
        wanted = [r for r in wanted if re.search(a.only, r)]
    new = [r for r in wanted if r not in on_disk]
    log(f"{len(wanted)} repos ({len(new)} new, {len(vanished)} no longer listed upstream)")
    if vanished:
        log(f"  kept on disk, not deleted: {', '.join(vanished[:6])}{' …' if len(vanished) > 6 else ''}")

    # 2. sync
    synced: list[dict] = []
    if a.skip_pull:
        synced = [{"repo": r, "action": "skipped", "head": head_of(root / "repos" / r)} for r in wanted]
    elif a.dry_run:
        log(f"dry-run: would clone {len(new)} and fetch {len(wanted) - len(new)}")
        synced = [{"repo": r, "action": "dry", "head": head_of(root / "repos" / r)} for r in wanted]
    else:
        with ThreadPoolExecutor(max_workers=8) as ex:  # network-bound
            synced = list(ex.map(lambda r: sync_repo(root, r, branches.get(r, "main")), wanted))
        acts = {}
        for s in synced:
            acts[s["action"]] = acts.get(s["action"], 0) + 1
        log("sync: " + ", ".join(f"{k} {v}" for k, v in sorted(acts.items())))

    # 3. extract what moved — or what was built by a different graphify
    version = graphify_version(a.graphify)
    log(f"graphify {version or 'version unknown'} at {a.graphify}")
    stale = [s["repo"] for s in synced
             if s.get("head") and is_stale(root, s["repo"], s["head"], version, a.force_extract)]
    log(f"{len(stale)} repos to re-extract" + (f": {', '.join(stale[:8])}{' …' if len(stale) > 8 else ''}" if stale else ""))
    extracted: list[dict] = []
    if stale and not a.dry_run:
        if not shutil.which(a.graphify) and not Path(a.graphify).exists():
            log(f"graphify not found at {a.graphify}; skipping extraction")
        else:
            with ThreadPoolExecutor(max_workers=a.jobs) as ex:
                for res in ex.map(lambda r: extract_repo(root, r, a.graphify, a.workers, version), stale):
                    extracted.append(res)
                    log(("  none " if res.get("no_code") else "  ok   " if res["ok"] else "  FAIL ") + res["repo"] +
                        (f" {res.get('nodes')}n/{res.get('edges')}e {res['seconds']}s" if res["ok"]
                         else f" {res.get('error', '')}"))

    # 4. index
    idx = {"ok": None}
    if a.dry_run:
        log("dry-run: would rebuild the index")
    elif any(e["ok"] and not e.get("no_code") for e in extracted) or not index.exists() or a.force_extract or migrated:
        log("rebuilding the index")
        idx = build_index(root, index, builder)
        log(f"index: {idx}" if idx["ok"] else f"index FAILED: {idx.get('error')}")
    else:
        log("nothing moved; index left alone")

    manifest = {
        "at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "seconds": round(time.time() - started, 1),
        "repos_wanted": len(wanted), "new": new, "vanished": vanished,
        "sync": {s["action"]: sum(1 for x in synced if x["action"] == s["action"]) for s in synced},
        "re_extracted": [e["repo"] for e in extracted if e["ok"] and not e.get("no_code")],
        "no_code": [e["repo"] for e in extracted if e.get("no_code")],
        "extract_failures": [{"repo": e["repo"], "error": e.get("error")} for e in extracted if not e["ok"]],
        "index": idx,
    }
    if not a.dry_run:
        (root / "refresh.manifest.json").write_text(json.dumps(manifest, indent=1), encoding="utf-8")
    log(f"done in {manifest['seconds']}s: {len(manifest['re_extracted'])} re-extracted, "
        f"{len(manifest['extract_failures'])} failed")
    return 0 if idx["ok"] is not False else 1


if __name__ == "__main__":
    raise SystemExit(main())
