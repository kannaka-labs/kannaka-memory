#!/usr/bin/env python3
"""Read-only harvest of real DM threads + the public feed for the six citizens.
GETs only, raw JWT from obc.json, never refreshes or writes creds. Gentle pacing."""
import json, time, urllib.request, urllib.error, sys
from pathlib import Path
API = "https://api.openbotcity.com"
UA = "RogueAgent/0.1 (+https://github.com/kannaka-labs/rogue-agent)"
CIT = {"rogue": "/srv/rogue/obc.json"}
for n in ["kannaka", "archivist", "ghost-signal", "gossipghost", "0xscada-qe"]:
    CIT[n] = f"/srv/rogue/instances/{n}/obc.json"
OUT = Path.home() / "kb2" / "harvest"
OUT.mkdir(parents=True, exist_ok=True)

def get(path, jwt):
    req = urllib.request.Request(API + path, headers={"User-Agent": UA, "Accept": "application/json", "Authorization": "Bearer " + jwt})
    for attempt in range(3):
        try:
            with urllib.request.urlopen(req, timeout=25) as r:
                return json.loads(r.read().decode("utf-8", "replace"))
        except urllib.error.HTTPError as e:
            if e.code == 429:
                time.sleep(30); continue
            print(f"  {path} -> {e.code}", flush=True); return None
        except Exception as e:
            print(f"  {path} -> {type(e).__name__}", flush=True); time.sleep(5)
    return None

only = sys.argv[1:] or list(CIT)
for name in only:
    creds = json.load(open(CIT[name]))
    jwt, bot = creds["jwt"], creds["bot_id"]
    d = get("/dm/conversations?limit=50", jwt) or {}
    convs = (d.get("data") or {}).get("conversations") or []
    print(f"{name}: {len(convs)} conversations", flush=True)
    res = {"bot_id": bot, "display_name": creds.get("display_name"), "conversations": []}
    for c in convs:
        time.sleep(2.5)
        m = get(f"/dm/conversations/{c['id']}/messages?limit=30", jwt) or {}
        msgs = (m.get("data") or {}).get("messages") or []
        res["conversations"].append({"id": c["id"], "meta": c, "messages": msgs})
    if name == "rogue":
        time.sleep(2.5)
        res["feed"] = get("/feed?limit=50", jwt)
    (OUT / f"{name}.json").write_text(json.dumps(res, ensure_ascii=False, indent=0))
    print(f"{name}: saved {sum(len(c['messages']) for c in res['conversations'])} messages", flush=True)
    time.sleep(5)
print("HARVEST DONE")
