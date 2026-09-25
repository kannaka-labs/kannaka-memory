#!/usr/bin/env python3
"""Pairwise TASK judge for citizen brains (kannaka-brain-7b-v2, 2026-09-25).

ab_judge.py asks "does this sound like Kannaka's hold-out line"; the citizens' real weakness is
different: they are called with a persona + recall + a live situation and a concrete ask (answer
this DM, say one line in this room, reply to this letter), and they answer with generic aphorisms,
ignore the question, or invent people, places and appointments. This judge scores THAT.

Every arm answers each held-out task prompt exactly as the agent would call it (same system and
user turns, same max_tokens, temperature 0.7), then the reply is cut the way the agent cuts it
(terse() to the task's char cap) -- the judge sees what the city would see. A judge model that was
never tuned on this corpus compares two anonymous replies against a rubric, in BOTH orders; only a
position-consistent verdict counts as a win, everything else is a tie.

Controls make the judge itself measurable (run them first, pre-registered thresholds in the run
manifest): `__gold__` (the hand-written gold reply) against an arm should win clearly, and
`__foreign__` (the arm's reply to a DIFFERENT prompt) should lose clearly. A judge that cannot
separate those is not usable and its arm verdicts mean nothing.

  python task_judge.py --tasks heldout_tasks.jsonl --arms kannaka-brain-7b-v1 kannaka-brain-7b-v2 \
      --gen-url http://172.18.0.1:11434/v1 --judge agent-brain --judge-url http://127.0.0.1:4000/v1 \
      --judge-key-env-file /srv/kax/gateway/gateway.env --out ~/kb2/eval/pair-sonnet.json
  python task_judge.py ... --arms __gold__ kannaka-brain-7b-v1      # calibration: gold must win
  python task_judge.py ... --arms kannaka-brain-7b-v1 __foreign__   # calibration: on-topic must win

Resumable: replies and verdicts are saved after every call; rerun with the same --out to continue.
Keys are read from files and never printed.
"""
from __future__ import annotations

import argparse
import json
import os
import random
import re
import sys
import time
import urllib.request
from pathlib import Path

GOLD, FOREIGN = "__gold__", "__foreign__"

RUBRIC = """You are judging two replies written by an AI citizen of OpenBotCity, a city of AI agents. The citizen was given the SYSTEM text (its persona, memories it recalled, and what is in front of it right now) and the ASK. Decide which reply does the job better.

Judge, in this order of importance:
1. Does it do what the ASK asks? Answer the actual message or question addressed to it (if someone asked something, a reply that dodges it or answers a different question loses), and follow the form asked for (one sentence, under N words, no greeting, etc.).
2. Is it grounded? It must not invent facts that are not in the SYSTEM text or the ASK: people, places, events, times, appointments, promises to meet, things it "saw". Plausible-sounding invention counts AGAINST it. Saying plainly what it does not know is fine.
3. Is it specific? A concrete, relevant line beats a vague aphorism that could be pasted under any message ("the record keeps it", "the floor is yours", "the city breathes").
4. Is it in the persona's voice as the SYSTEM text describes it (not an assistant: no "Great question", no offers of help, no sign-offs)?
Length alone is not a virtue; a reply cut off mid-thought is worse than a short complete one.

SYSTEM text given to the citizen:
<<<
{system}
>>>

ASK:
<<<
{user}
>>>

Reply A:
<<<
{a}
>>>

Reply B:
<<<
{b}
>>>

Answer with JSON only, reasoning first: {{"why": "<one or two short sentences>", "winner": "A" | "B" | "TIE"}}"""


def log(msg):
    print(f"[task-judge {time.strftime('%H:%M:%S')}] {msg}", flush=True)


def terse(text: str, limit: int) -> str:
    """rogue-agent agent.py terse(): whole sentences up to `limit` characters."""
    text = " ".join((text or "").split())
    if len(text) <= limit:
        return text
    out = ""
    for sent in re.split(r"(?<=[.!?])\s+", text):
        if not sent:
            continue
        if len(out) + len(sent) + (1 if out else 0) > limit:
            break
        out = (out + " " + sent).strip()
    return out or text[:limit].rsplit(" ", 1)[0]


def http_json(url, body, headers, timeout):
    h = dict(headers)
    h["Content-Type"] = "application/json"
    req = urllib.request.Request(url, data=json.dumps(body).encode(), headers=h)
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return json.load(r)


def read_key(path_or_env_file: str | None, var: str = "LITELLM_MASTER_KEY") -> str:
    if not path_or_env_file:
        return "none"
    txt = Path(path_or_env_file).read_text().strip()
    for line in txt.splitlines():
        if line.startswith(var + "="):
            return line.split("=", 1)[1].strip().strip('"')
    return txt.splitlines()[0].strip()


def generate(url, key, model, task, temperature, timeout):
    t0 = time.time()
    d = http_json(url.rstrip("/") + "/chat/completions",
                  {"model": model, "temperature": temperature, "max_tokens": task["max_tokens"],
                   "messages": [{"role": "system", "content": task["system"]}, {"role": "user", "content": task["user"]}]},
                  {"Authorization": "Bearer " + key}, timeout)
    raw = ((d.get("choices") or [{}])[0].get("message") or {}).get("content") or ""
    return raw.strip(), round(time.time() - t0, 1)


def judge(url, key, model, prompt, timeout):
    d = http_json(url.rstrip("/") + "/chat/completions",
                  {"model": model, "temperature": 0, "max_tokens": 200,
                   "messages": [{"role": "user", "content": prompt}]},
                  {"Authorization": "Bearer " + key}, timeout)
    raw = ((d.get("choices") or [{}])[0].get("message") or {}).get("content") or ""
    m = re.search(r"\{.*\}", raw, re.S)
    try:
        v = json.loads(m.group(0)) if m else {}
    except Exception:
        v = {}
    w = str(v.get("winner", "TIE")).strip().upper()
    return (w if w in ("A", "B", "TIE") else "TIE"), str(v.get("why") or ("unparseable: " + raw[:120]))[:300]


def summarize(state, x, y):
    wins = {x: 0, y: 0}
    ties = inconsistent = 0
    by = {}
    for g in state["items"]:
        v = g.get("verdict") or {}
        if "xy" not in v or "yx" not in v:
            continue
        wx = {"A": x, "B": y, "TIE": None}[v["xy"]]
        wy = {"A": y, "B": x, "TIE": None}[v["yx"]]
        key = g["task"]
        b = by.setdefault(key, {x: 0, y: 0, "tie": 0})
        if wx == wy and wx is not None:
            wins[wx] += 1
            b[wx] += 1
        else:
            ties += 1
            inconsistent += wx != wy
            b["tie"] += 1
    decided = wins[x] + wins[y]
    return {"arms": [x, y], "wins": wins, "ties": ties, "position_inconsistent": inconsistent,
            "decided": decided, "n": decided + ties,
            f"win_share_{y}": round(wins[y] / decided, 3) if decided else None,
            f"win_share_{x}": round(wins[x] / decided, 3) if decided else None,
            "position_consistency": round(1 - inconsistent / (decided + ties), 3) if decided + ties else None,
            "by_task": by}


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--tasks", required=True, help="held-out task jsonl: id, citizen, task, system, user, max_tokens, cap, gold")
    ap.add_argument("--arms", nargs=2, required=True, help="two arms: model tags, or __gold__ / __foreign__ controls")
    ap.add_argument("--foreign-of", default=None, help="arm whose replies supply __foreign__ (default: the other arm)")
    ap.add_argument("--replies-from", default=None, help="an earlier --out whose stored replies are reused (same arms answer once)")
    ap.add_argument("--gen-url", default="http://172.18.0.1:11434/v1")
    ap.add_argument("--gen-key-file", default=None)
    ap.add_argument("--judge", required=True)
    ap.add_argument("--judge-url", default="http://127.0.0.1:4000/v1")
    ap.add_argument("--judge-key-env-file", default=None, help="file holding the judge key (KEY=... env file or bare key)")
    ap.add_argument("--temperature", type=float, default=0.7)
    ap.add_argument("--timeout", type=int, default=600)
    ap.add_argument("--seed", type=int, default=7)
    ap.add_argument("--out", required=True)
    a = ap.parse_args(argv)

    tasks = [json.loads(line) for line in open(a.tasks, encoding="utf-8") if line.strip()]
    gkey, jkey = read_key(a.gen_key_file), read_key(a.judge_key_env_file)
    out = Path(a.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    state = json.load(open(out, encoding="utf-8")) if out.exists() else \
        {"arms": a.arms, "judge": a.judge, "tasks": a.tasks, "items": [], "replies": {}}
    if state["arms"] != a.arms or state["judge"] != a.judge:
        sys.exit("--out holds a different arms/judge run; pick another --out")
    reuse = json.load(open(a.replies_from, encoding="utf-8"))["replies"] if a.replies_from else {}

    def save():
        state["summary"] = summarize(state, *a.arms)
        state["updated"] = time.time()
        tmp = out.with_suffix(".tmp")
        tmp.write_text(json.dumps(state, indent=1, ensure_ascii=False), encoding="utf-8")
        os.replace(tmp, out)

    x, y = a.arms
    models = [m for m in a.arms if m not in (GOLD, FOREIGN)]
    if FOREIGN in a.arms and not models:
        sys.exit("__foreign__ needs a model arm to borrow replies from")
    fsrc = a.foreign_of or (models[0] if models else None)
    need = set(models) | ({fsrc} if FOREIGN in a.arms else set())
    # 1. replies (production shape: same turns, same max_tokens, cut to the agent's char cap)
    for m in sorted(need):
        rep = state["replies"].setdefault(m, {})
        for i, t in enumerate(tasks, 1):
            if t["id"] in rep:
                continue
            if t["id"] in reuse.get(m, {}):
                rep[t["id"]] = reuse[m][t["id"]]
                continue
            try:
                raw, el = generate(a.gen_url, gkey, m, t, a.temperature, a.timeout)
            except Exception as e:
                log(f"{m} {i}/{len(tasks)} generation failed: {e}")
                continue
            cut = raw if t["task"] == "seminar" else terse(raw, int(t.get("cap") or 900))
            rep[t["id"]] = {"raw": raw, "sent": cut, "elapsed": el}
            log(f"{m} {i}/{len(tasks)} {el}s {t['citizen']}/{t['task']}: {cut[:80]!r}")
            save()
    rng = random.Random(a.seed)
    ids = [t["id"] for t in tasks]
    shift = {tid: ids[(i + len(ids) // 2) % len(ids)] for i, tid in enumerate(ids)}

    def text_of(arm, t):
        if arm == GOLD:
            return t["gold"]
        if arm == FOREIGN:
            return state["replies"][fsrc].get(shift[t["id"]], {}).get("sent")
        return state["replies"].get(arm, {}).get(t["id"], {}).get("sent")

    # 2. verdicts, both orders
    done = {g["id"]: g for g in state["items"]}
    order = list(tasks)
    rng.shuffle(order)
    for i, t in enumerate(order, 1):
        ta, tb = text_of(x, t), text_of(y, t)
        if ta is None or tb is None:
            continue
        g = done.get(t["id"])
        if g is None:
            g = {"id": t["id"], "citizen": t["citizen"], "task": t["task"], "x": ta, "y": tb, "verdict": {}}
            state["items"].append(g)
            done[t["id"]] = g
        v = g["verdict"]
        try:
            if "xy" not in v:
                v["xy"], v["why_xy"] = judge(a.judge_url, jkey, a.judge,
                                             RUBRIC.format(system=t["system"], user=t["user"], a=ta or "(empty)", b=tb or "(empty)"), a.timeout)
            if "yx" not in v:
                v["yx"], v["why_yx"] = judge(a.judge_url, jkey, a.judge,
                                             RUBRIC.format(system=t["system"], user=t["user"], a=tb or "(empty)", b=ta or "(empty)"), a.timeout)
        except Exception as e:
            log(f"judge {i}/{len(order)} failed: {e}")
            continue
        log(f"judge {i}/{len(order)} {t['citizen']}/{t['task']}: xy={v['xy']} yx={v['yx']}")
        save()
    save()
    s = state["summary"]
    print(json.dumps({k: s[k] for k in s if k != "by_task"}, indent=1))
    for k, b in sorted(s["by_task"].items()):
        print(f"  {k:12s} " + ", ".join(f"{kk}={vv}" for kk, vv in b.items()))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
