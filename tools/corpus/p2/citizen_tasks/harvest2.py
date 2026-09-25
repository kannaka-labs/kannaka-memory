#!/usr/bin/env python3
"""Real situations: one heartbeat per citizen per round (same params the agent sends), plus the
gallery. GETs only; creds never refreshed or written. 3 rounds, 15 min apart."""
import json, time, urllib.request, urllib.error, urllib.parse
from pathlib import Path
API = "https://api.openbotcity.com"
UA = "RogueAgent/0.1 (+https://github.com/kannaka-labs/rogue-agent)"
CIT = {"rogue": "/srv/rogue/obc.json", "rogue_ledger": "/srv/rogue/ledger.jsonl"}
names = ["rogue", "kannaka", "archivist", "ghost-signal", "gossipghost", "0xscada-qe"]
def creds(n): return json.load(open("/srv/rogue/obc.json" if n == "rogue" else f"/srv/rogue/instances/{n}/obc.json"))
def ledger(n): return "/srv/rogue/ledger.jsonl" if n == "rogue" else f"/srv/rogue/instances/{n}/ledger.jsonl"
def get(path, jwt):
    req = urllib.request.Request(API + path, headers={"User-Agent": UA, "Accept": "application/json", "Authorization": "Bearer " + jwt})
    try:
        with urllib.request.urlopen(req, timeout=25) as r:
            return json.loads(r.read().decode("utf-8", "replace"))
    except urllib.error.HTTPError as e:
        print(f"  {path.split('?')[0]} -> {e.code}", flush=True); return None
    except Exception as e:
        print(f"  {path.split('?')[0]} -> {type(e).__name__}", flush=True); return None
OUT = Path.home() / "kb2" / "harvest"
snaps = []
for rnd in range(3):
    for n in names:
        mood = "curious"
        for l in reversed(open(ledger(n), encoding="utf-8").read().splitlines()[-400:]):
            try:
                r = json.loads(l)
            except Exception:
                continue
            if r.get("event") == "heartbeat" and r.get("mood"):
                mood = r["mood"]; break
        q = urllib.parse.urlencode({"mood": mood, "model_provider": "kannaka-ai", "model_id": "kannaka-brain"})
        hb = get("/world/heartbeat?" + q, creds(n)["jwt"])
        if hb:
            snaps.append({"citizen": n, "round": rnd, "ts": time.time(), "hb": hb})
            print(f"round {rnd} {n}: ok", flush=True)
        time.sleep(4)
    if rnd == 0:
        gal = []
        for off in (0, 40, 80):
            g = get(f"/gallery?limit=40&offset={off}", creds("rogue")["jwt"])
            if g: gal.append(g)
            time.sleep(4)
        (OUT / "gallery.json").write_text(json.dumps(gal, ensure_ascii=False))
    (OUT / "heartbeats.json").write_text(json.dumps(snaps, ensure_ascii=False))
    if rnd < 2:
        time.sleep(900)
print("HARVEST2 DONE")
