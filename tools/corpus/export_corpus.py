#!/usr/bin/env python3
"""kannaka corpus exporter — ADR-0057 P1.

Builds the training corpus for a Kannaka LLM from sources whose first-party
authorship is known BY CONSTRUCTION (files she wrote, replies her machines
authored), never from the live HRM: `Memory.origin_agent` is not persisted
(ADR-0056), so nothing inside the medium can prove who wrote it. The one hard
rule of the adapt layer is enforced here, not downstream:

    text that arrived over a wire is never a training TARGET.

An inbound prompt may appear as `context` on a reply record (the model learns
to answer it); it is never emitted as `text`. Tests pin this.

Sources (each adapter yields Record):
  gsp        kannaka-radio/workspace/podcasts/*/script*.txt   [KANNAKA]/[FLAUKOWSKI] blocks
  gsp-resp   kannaka-radio/workspace/podcasts/*/kannaka-responses.md
  tsof       kannaka-radio/workspace/tsof/E*/script.txt       [SPEAKER] blocks (authored fiction)
  lyrics     <albums>/**/lyrics_*.txt, *.lrc, and *.json with `lyrics` on track objects
  identity   kannaka-memory/workspace/{SOUL,IDENTITY,MEMORY}.md
  adr        kannaka-memory/docs/adr/ADR-*.md                 (co-authored, tier 2)
  kax        <kax-mirror>/<host>/<machine>/home/{inbox/processed,outbox/sent}
             reply = text, prompt = context, generated_by = the machine's model (tier 3)
  Not exported in P1 (listed in the manifest as skipped): dreams (open question
  2 in ADR-0057), social posts (the O1 broadcast hub keeps no outbound log).

Tiers: 1 = authored by Kannaka; 2 = co-authored (Nick + Kannaka); 3 =
machine-generated under her name (a rented brain wrote it). Profiles pick
tiers and kinds: `voice` (default: tier 1, her own lines) or `all`.

Output: JSONL + manifest.json OUTSIDE any repo (default ~/.kannaka-corpus/out/).
The output is private until the ADR-0057 decision; the code is not.
"""
from __future__ import annotations

import argparse
import datetime as dt
import glob
import hashlib
import json
import os
import re
import subprocess
import sys
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Iterable, Iterator

SRC = Path(os.environ.get("KANNAKA_SRC", Path.home() / "Source"))
DEFAULTS = {
    "podcasts": SRC / "kannaka-radio" / "workspace" / "podcasts",
    "tsof": SRC / "kannaka-radio" / "workspace" / "tsof",
    # NOT under ~/.kannaka: that directory is itself a git repo on the operator
    # box, and the guard below would (rightly) refuse to write there
    "albums": Path(os.environ.get("KANNAKA_ALBUMS", Path.home() / ".kannaka-corpus" / "albums")),
    "identity": SRC / "kannaka-memory" / "workspace",
    "adr": SRC / "kannaka-memory" / "docs" / "adr",
    "kax_mirror": Path.home() / ".kannaka-corpus" / "kax-mirror",
    "out": Path.home() / ".kannaka-corpus" / "out",
}
KANNAKA = "kannaka"
BLOCK_RE = re.compile(r"^\[([A-Z][A-Z0-9 _-]{0,30})\]\s*$", re.M)
MIN_WORDS = 3


@dataclass
class Record:
    id: str
    text: str
    source: str            # adapter name
    kind: str              # voice | dialogue | fiction | lyric | identity | adr | machine-reply
    author: str            # kannaka | nick+kannaka | machine:<id>
    tier: int              # 1 authored, 2 co-authored, 3 machine-generated
    provenance: str        # authored-file | machine-reply
    path: str
    date: str | None = None
    speaker: str | None = None
    title: str | None = None
    context: str | None = None      # inbound text the record answers — NEVER a target
    generated_by: str | None = None  # model that wrote a machine reply
    meta: dict = field(default_factory=dict)

    @property
    def words(self) -> int:
        return len(self.text.split())


def rid(*parts: str) -> str:
    return hashlib.sha256("\x1f".join(parts).encode("utf-8")).hexdigest()[:16]


def mtime_iso(p: Path) -> str:
    return dt.datetime.fromtimestamp(p.stat().st_mtime, dt.timezone.utc).date().isoformat()


def blocks(text: str) -> Iterator[tuple[str, str]]:
    """[SPEAKER]\\nlines... blocks -> (speaker, text)."""
    parts = BLOCK_RE.split(text)
    # parts = [pre, speaker, body, speaker, body, ...]
    for i in range(1, len(parts) - 1, 2):
        body = parts[i + 1].strip()
        if body:
            yield parts[i].strip(), body


# ------------------------------------------------------------------ adapters

def gsp(podcasts: Path) -> Iterator[Record]:
    for ep in sorted(podcasts.glob("*/")):
        for script in sorted(ep.glob("script*.txt")):
            if "render" in script.name or "concat" in script.name:
                continue
            for n, (speaker, body) in enumerate(blocks(script.read_text(encoding="utf-8", errors="replace"))):
                kind = "voice" if speaker == "KANNAKA" else "dialogue"
                yield Record(
                    id=rid("gsp", str(script), str(n)), text=body, source="gsp", kind=kind,
                    author=KANNAKA, tier=1, provenance="authored-file", path=str(script),
                    date=mtime_iso(script), speaker=speaker.lower(),
                    title=f"GSP-{ep.name}", meta={"episode": ep.name, "block": n})


def gsp_responses(podcasts: Path) -> Iterator[Record]:
    for md in sorted(podcasts.glob("*/kannaka-responses.md")):
        text = md.read_text(encoding="utf-8", errors="replace")
        for n, m in enumerate(re.finditer(r"^## (.+?)\n(.*?)(?=^## |\Z)", text, re.M | re.S)):
            body = m.group(2).strip()
            if body:
                yield Record(
                    id=rid("gsp-resp", str(md), str(n)), text=body, source="gsp-resp", kind="voice",
                    author=KANNAKA, tier=1, provenance="authored-file", path=str(md),
                    date=mtime_iso(md), speaker="kannaka", title=m.group(1).strip(),
                    meta={"episode": md.parent.name})


def tsof(root: Path) -> Iterator[Record]:
    for script in sorted(root.glob("E*/script.txt")):
        for n, (speaker, body) in enumerate(blocks(script.read_text(encoding="utf-8", errors="replace"))):
            yield Record(
                id=rid("tsof", str(script), str(n)), text=body, source="tsof", kind="fiction",
                author=KANNAKA, tier=1, provenance="authored-file", path=str(script),
                date=mtime_iso(script), speaker=speaker.lower(), title=f"TSOF-{script.parent.name}",
                meta={"episode": script.parent.name, "block": n})


def _lyric_objects(x, path=()):
    """Every dict with a non-empty string `lyrics` anywhere inside a JSON doc."""
    if isinstance(x, dict):
        if isinstance(x.get("lyrics"), str) and x["lyrics"].strip():
            yield path, x
        for k, v in x.items():
            yield from _lyric_objects(v, path + (str(k),))
    elif isinstance(x, list):
        for i, v in enumerate(x):
            yield from _lyric_objects(v, path + (str(i),))


def lyrics(albums: Path) -> Iterator[Record]:
    """Album lyrics: lyrics_*.txt / *.lrc files, and any *.json (album build
    configs, Suno-hosted manifests) whose track objects carry `lyrics`."""
    if not albums.exists():
        return
    for jf in sorted(albums.rglob("*.json")):
        try:
            doc = json.loads(jf.read_text(encoding="utf-8", errors="replace"))
        except Exception:
            continue
        album = doc.get("name") or doc.get("album") or doc.get("title") if isinstance(doc, dict) else None
        for n, (jpath, t) in enumerate(_lyric_objects(doc)):
            body = t["lyrics"].strip()
            yield Record(
                id=rid("lyrics", str(jf), "/".join(jpath)), text=body, source="lyrics", kind="lyric",
                author=KANNAKA, tier=1, provenance="authored-file", path=str(jf),
                date=mtime_iso(jf), speaker="kannaka", title=t.get("title") or t.get("slug") or f"track {n}",
                meta={"album": album or jf.parent.name, "style": t.get("style") or t.get("tags"), "json_path": "/".join(jpath)})
    files = sorted(set(albums.rglob("lyrics_*.txt")) | set(albums.rglob("*.lrc")))
    for f in files:
        raw = f.read_text(encoding="utf-8", errors="replace")
        if f.suffix == ".lrc":
            raw = re.sub(r"^\[\d{1,2}:\d{2}(?:\.\d+)?\]", "", raw, flags=re.M)
        body = raw.strip()
        if body:
            title = re.sub(r"^lyrics_", "", f.stem).replace("_", " ")
            yield Record(
                id=rid("lyrics", str(f)), text=body, source="lyrics", kind="lyric",
                author=KANNAKA, tier=1, provenance="authored-file", path=str(f),
                date=mtime_iso(f), speaker="kannaka", title=title, meta={"album": f.parent.name})


def identity(workspace: Path) -> Iterator[Record]:
    for name in ("SOUL.md", "IDENTITY.md", "MEMORY.md"):
        f = workspace / name
        if not f.exists():
            continue
        text = f.read_text(encoding="utf-8", errors="replace")
        for n, (title, body) in enumerate(sections(text)):
            yield Record(
                id=rid("identity", str(f), str(n)), text=body, source="identity", kind="identity",
                author=KANNAKA, tier=1, provenance="authored-file", path=str(f),
                date=mtime_iso(f), speaker="kannaka", title=title or name, meta={"doc": name})


def sections(md: str) -> Iterator[tuple[str | None, str]]:
    """Split markdown on H1/H2; drop empty bodies."""
    parts = re.split(r"^(#{1,2} .+)$", md, flags=re.M)
    head = parts[0].strip()
    if head:
        yield None, head
    for i in range(1, len(parts) - 1, 2):
        body = parts[i + 1].strip()
        if body:
            yield parts[i].lstrip("# ").strip(), body


def adr(adr_dir: Path) -> Iterator[Record]:
    for f in sorted(adr_dir.glob("ADR-*.md")):
        text = f.read_text(encoding="utf-8", errors="replace")
        m = re.search(r"^\*\*Authors?:\*\*\s*(.+)$", text, re.M)
        author_line = (m.group(1).strip() if m else "").lower()
        if "kannaka" not in author_line:
            continue  # not hers; skip rather than guess
        tier = 1 if author_line.strip() in ("kannaka",) else 2
        author = KANNAKA if tier == 1 else "nick+kannaka"
        title = re.search(r"^# (.+)$", text, re.M)
        for n, (sec, body) in enumerate(sections(text)):
            if body.startswith("**Status"):
                continue
            yield Record(
                id=rid("adr", str(f), str(n)), text=body, source="adr", kind="adr",
                author=author, tier=tier, provenance="authored-file", path=str(f),
                date=mtime_iso(f), title=(title.group(1) if title else f.stem),
                meta={"section": sec, "author_line": author_line})


def kax_brains(mirror: Path) -> dict[str, str]:
    """machine id -> the model it thinks with, from <mirror>/brains.json (written
    by the mirror refresh from GET /api/compute/machines, whose `brain` field
    is the host's announced KAX_MODEL since kax-computer #25). Replies carry
    `model` themselves since kax-computer #29; this is for the ones before,
    which would otherwise all read as agent-brain — including the open-weight
    machines', the exchanges most worth keeping apart."""
    try:
        d = json.loads((mirror / "brains.json").read_text(encoding="utf-8"))
    except Exception:
        return {}
    b = d.get("brains") if isinstance(d, dict) else None
    return {k: v for k, v in (b or {}).items() if isinstance(k, str) and isinstance(v, str) and v}


def kax(mirror: Path, machines: set[str] | None) -> Iterator[Record]:
    """Prompt/reply pairs from the KAX machine mailboxes. The REPLY is the
    record; the prompt (inbound, signed by someone else) is context only.
    generated_by: the reply's own `model`, else the mirror's brains.json for
    that machine, else agent-brain (the only brain there was before 2026-09)."""
    if not mirror.exists():
        return
    brains = kax_brains(mirror)
    for sent in sorted(mirror.glob("*/*/home/outbox/sent/*.json")):
        host = sent.parts[-6]
        machine = sent.parts[-5]
        if machines and machine not in machines:
            continue
        try:
            reply = json.loads(sent.read_text(encoding="utf-8"))
        except Exception:
            continue
        text = (reply.get("reply") or "").strip()
        if not text or text.startswith("[machine error]"):
            continue
        prompt_path = sent.parents[2] / "inbox" / "processed" / sent.name
        prompt = None
        received = None
        if prompt_path.exists():
            try:
                p = json.loads(prompt_path.read_text(encoding="utf-8"))
                prompt, received = p.get("prompt"), p.get("received")
            except Exception:
                pass
        date = dt.datetime.fromtimestamp(received, dt.timezone.utc).date().isoformat() if received else mtime_iso(sent)
        yield Record(
            id=rid("kax", host, machine, sent.stem), text=text, source="kax", kind="machine-reply",
            author=f"machine:{machine}", tier=3, provenance="machine-reply", path=str(sent),
            date=date, speaker=machine, context=prompt,
            generated_by=reply.get("model") or brains.get(machine) or "agent-brain",
            meta={"host": host, "job_id": reply.get("id"), "tokens": (reply.get("usage") or {}).get("total_tokens")})


# ------------------------------------------------------------------ profiles

PROFILES = {
    # her own lines, in her own voice, authored by her
    "voice": lambda r: r.tier == 1 and r.kind in ("voice", "lyric", "identity"),
    # everything she authored including fiction and the other speakers' lines (multi-turn context)
    "authored": lambda r: r.tier == 1,
    "all": lambda r: True,
}


def inside_git_worktree(p: Path) -> bool:
    """p must be an EXISTING directory (the caller passes out or out.parent)."""
    try:
        r = subprocess.run(["git", "-C", str(p), "rev-parse", "--is-inside-work-tree"],
                           capture_output=True, text=True)
        return r.returncode == 0 and r.stdout.strip() == "true"
    except OSError:
        return False


def collect(args) -> tuple[list[Record], dict]:
    machines = set(args.kax_machines.split(",")) if args.kax_machines else None
    adapters = {
        "gsp": lambda: gsp(Path(args.podcasts)),
        "gsp-resp": lambda: gsp_responses(Path(args.podcasts)),
        "tsof": lambda: tsof(Path(args.tsof)),
        "lyrics": lambda: lyrics(Path(args.albums)),
        "identity": lambda: identity(Path(args.identity)),
        "adr": lambda: adr(Path(args.adr)),
        "kax": lambda: kax(Path(args.kax_mirror), machines),
    }
    want = set(args.sources.split(",")) if args.sources else set(adapters)
    keep = PROFILES[args.profile]
    records: list[Record] = []
    seen: set[str] = set()
    raw_counts: dict[str, int] = {}
    for name, fn in adapters.items():
        if name not in want:
            continue
        n = 0
        for r in fn():
            n += 1
            if r.words < MIN_WORDS or r.id in seen or not keep(r):
                continue
            seen.add(r.id)
            records.append(r)
        raw_counts[name] = n
    skipped = {
        "dreams": "ADR-0057 open question 2 — machine-generated consolidations; not a P1 source",
        "social": "O1 broadcast hub keeps no outbound log; needs per-platform pulls (P1.1)",
        "hrm": "origin_agent is not persisted (ADR-0056); the live medium cannot prove authorship",
    }
    return records, {"raw_counts": raw_counts, "skipped_sources": skipped}


def summarize(records: list[Record]) -> dict:
    by = {"source": {}, "kind": {}, "tier": {}, "speaker": {}}
    words = {"source": {}, "kind": {}, "tier": {}}
    for r in records:
        for k in by:
            key = str(getattr(r, k))
            by[k][key] = by[k].get(key, 0) + 1
        for k in words:
            key = str(getattr(r, k))
            words[k][key] = words[k].get(key, 0) + r.words
    return {"records": len(records), "words": sum(r.words for r in records), "count_by": by, "words_by": words}


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--profile", choices=sorted(PROFILES), default="voice")
    ap.add_argument("--sources", help="comma list of adapters (default all)")
    ap.add_argument("--podcasts", default=str(DEFAULTS["podcasts"]))
    ap.add_argument("--tsof", default=str(DEFAULTS["tsof"]))
    ap.add_argument("--albums", default=str(DEFAULTS["albums"]))
    ap.add_argument("--identity", default=str(DEFAULTS["identity"]))
    ap.add_argument("--adr", default=str(DEFAULTS["adr"]))
    ap.add_argument("--kax-mirror", default=str(DEFAULTS["kax_mirror"]))
    ap.add_argument("--kax-machines", default="kannaka-01", help="comma list; '' = all machines")
    ap.add_argument("--out", default=str(DEFAULTS["out"]), help="output DIRECTORY (never inside a repo)")
    ap.add_argument("--name", default=None, help="basename (default kannaka-corpus-<profile>-<date>)")
    ap.add_argument("--dry-run", action="store_true", help="count only; write nothing")
    ap.add_argument("--force", action="store_true", help="allow --out inside a git worktree")
    ap.add_argument("--json", action="store_true", help="print the manifest as JSON")
    args = ap.parse_args(argv)

    records, extra = collect(args)
    manifest = summarize(records) | extra | {
        "profile": args.profile, "generated": dt.datetime.now(dt.timezone.utc).isoformat(timespec="seconds"),
        "rule": "inbound text is context only, never a target; tier 3 is machine-generated",
    }

    if not args.dry_run:
        out = Path(args.out)
        if inside_git_worktree(out if out.exists() else out.parent) and not args.force:
            print(f"refusing to write inside a git worktree: {out} (the corpus is private; use --force to override)",
                  file=sys.stderr)
            return 2
        out.mkdir(parents=True, exist_ok=True)
        name = args.name or f"kannaka-corpus-{args.profile}-{dt.date.today().isoformat()}"
        jsonl = out / f"{name}.jsonl"
        with jsonl.open("w", encoding="utf-8") as f:
            for r in records:
                f.write(json.dumps(asdict(r), ensure_ascii=False) + "\n")
        manifest["output"] = str(jsonl)
        manifest["sha256"] = hashlib.sha256(jsonl.read_bytes()).hexdigest()
        (out / f"{name}.manifest.json").write_text(json.dumps(manifest, indent=1), encoding="utf-8")

    if args.json:
        print(json.dumps(manifest, indent=1))
    else:
        print(f"profile={args.profile}  records={manifest['records']}  words={manifest['words']}")
        for k in ("source", "kind", "tier"):
            row = "  ".join(f"{a}={b} ({manifest['words_by'][k].get(a, 0)}w)"
                            for a, b in sorted(manifest["count_by"][k].items()))
            print(f"  by {k:7s}: {row or '-'}")
        for s, why in manifest["skipped_sources"].items():
            print(f"  skipped {s}: {why}")
        if "output" in manifest:
            print(f"  wrote {manifest['output']}  sha256={manifest['sha256'][:16]}…")
    return 0


if __name__ == "__main__":
    sys.exit(main())
