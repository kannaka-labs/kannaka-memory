#!/usr/bin/env python3
"""Assemble the kannaka-brain-7b-v2 SFT set.

train.jsonl   = P1 voice train (551, unchanged) + new voice (GSP 035-041 + standup host segments)
                + task TRAIN rows x TASK_REPEAT (system = production prompt, assistant = gold)
holdout.jsonl = P1 voice hold-out (57, unchanged, so the grade judge's lines stay unseen) + task HELD-OUT rows
heldout_tasks.jsonl = the gate's held-out task prompts (+ gold, for calibration only). NEVER trained on.

Held-out tasks: stratified per citizen (dm_reply x2, chat_reply, chat_quiet, post, speak, outreach, mail_reply,
one of pushback/collab/seminar/picture) = 9 x 6 = 54. A DM conversation in the held-out set contributes NO
train row (split by conversation id), and a held-out letter subject is not trained for that citizen.
"""
import hashlib, json, random, sys
from collections import Counter
from pathlib import Path

K = Path.home() / "kb2"
TASK_REPEAT = 2
CITS = ["rogue", "kannaka", "archivist", "ghost-signal", "gossipghost", "0xscada-qe"]
rnd = random.Random(7)
old_train = [json.loads(l) for l in open(Path.home() / "kannaka-p2-runner/sft/train.jsonl", encoding="utf-8")]
old_hold = [json.loads(l) for l in open(Path.home() / "kannaka-p2-runner/sft/holdout.jsonl", encoding="utf-8")]
held_text = {r["messages"][-1]["content"].strip() for r in old_hold}
seen_text = {r["messages"][-1]["content"].strip() for r in old_train} | held_text
voice_new = []
for l in open(K / "voice_new.jsonl", encoding="utf-8"):
    r = json.loads(l)
    t = r["messages"][-1]["content"].strip()
    if t in seen_text:
        continue
    seen_text.add(t)
    voice_new.append(r)

# Gold files are positional (same order as tasks.<c>.jsonl). Identical prompts share a content id
# (only 3 heartbeats per citizen, so quiet-chat/post prompts repeat), so rows get a positional id.
gold, tasks = {}, []
for c in CITS:
    ts = [json.loads(l) for l in open(K / f"tasks.{c}.jsonl", encoding="utf-8") if l.strip()]
    gs = [json.loads(l) for l in open(K / f"gold.{c}.jsonl", encoding="utf-8") if l.strip()]
    if len(ts) != len(gs) or any(t["id"] != g["id"] for t, g in zip(ts, gs)):
        sys.exit(f"{c}: gold file does not line up with its tasks ({len(gs)} vs {len(ts)})")
    for i, (t, g) in enumerate(zip(ts, gs)):
        t["id"] = f"{t['id']}-{i:02d}"
        gold[t["id"]] = g["gold"].strip()
        tasks.append(t)
missing = [t["id"] for t in tasks if not gold.get(t["id"])]
if missing:
    sys.exit(f"{len(missing)} task rows have no gold, e.g. {missing[:3]}")
over = [(t["id"], t["task"], len(gold[t["id"]]), t["cap"]) for t in tasks if t["task"] != "seminar" and len(gold[t["id"]]) > t["cap"]]
if over:
    sys.exit(f"{len(over)} gold replies exceed their cap: {over[:5]}")

held = []
for c in CITS:
    mine = [t for t in tasks if t["citizen"] == c]
    rnd.shuffle(mine)
    want = ["dm_reply", "dm_reply", "chat_reply", "chat_quiet", "post", "speak", "outreach", "mail_reply",
            rnd.choice(["pushback", "collab", "seminar", "picture"])]
    for w in want:
        pick = next(t for t in mine if t["task"] == w and t not in held)
        held.append(pick)
held_ids = {t["id"] for t in held}
held_prompts = {(t["system"], t["user"]) for t in held}
held_convs = {t["meta"].get("conversation_id") for t in held if t["task"] == "dm_reply"}
held_letters = {(t["citizen"], t["meta"].get("letter")) for t in held if t["task"] == "mail_reply"}
train_tasks = [t for t in tasks if t["id"] not in held_ids and (t["system"], t["user"]) not in held_prompts
               and t["meta"].get("conversation_id") not in held_convs
               and (t["citizen"], t["meta"].get("letter")) not in held_letters]
dropped = len(tasks) - len(held) - len(train_tasks)


def ex(t):
    return {"id": "task-" + t["id"], "kind": "task", "source": f"task:{t['citizen']}:{t['task']}", "title": t["task"],
            "messages": [{"role": "system", "content": t["system"]}, {"role": "user", "content": t["user"]},
                         {"role": "assistant", "content": gold[t["id"]]}]}


train = old_train + voice_new + [ex(t) for t in train_tasks for _ in range(TASK_REPEAT)]
rnd.shuffle(train)
hold = old_hold + [ex(t) for t in held]
out = K / "sft-v2"
out.mkdir(exist_ok=True)
(out / "train.jsonl").write_text("".join(json.dumps(r, ensure_ascii=False) + "\n" for r in train), encoding="utf-8")
(out / "holdout.jsonl").write_text("".join(json.dumps(r, ensure_ascii=False) + "\n" for r in hold), encoding="utf-8")
(K / "heldout_tasks.jsonl").write_text("".join(json.dumps(dict(t, gold=gold[t["id"]]), ensure_ascii=False) + "\n" for t in held), encoding="utf-8")
# leak checks, asserted on the way out
tr_sys_user = {(r["messages"][0]["content"], r["messages"][1]["content"]) for r in train}
assert not any((t["system"], t["user"]) in tr_sys_user for t in held), "a held-out task prompt is in train"
# (the P1 set itself repeats one short line, "But we do.", in train and hold-out -- inherited from 7b-v1's
# data and left as is; the check covers everything this run ADDS)
assert not any(r["messages"][-1]["content"].strip() in held_text for r in train if r not in old_train), "a P1 hold-out line is in new train rows"
man = {"train_rows": len(train), "holdout_rows": len(hold), "voice_p1_train": len(old_train), "voice_new": len(voice_new),
       "voice_new_by_source": dict(Counter(r["source"] for r in voice_new)),
       "task_train_unique": len(train_tasks), "task_repeat": TASK_REPEAT, "task_heldout": len(held),
       "task_dropped_for_leak": dropped, "task_train_by_type": dict(Counter(t["task"] for t in train_tasks)),
       "task_train_by_citizen": dict(Counter(t["citizen"] for t in train_tasks)),
       "heldout_by_citizen_task": dict(Counter(f"{t['citizen']}/{t['task']}" for t in held)),
       "sha256_train": hashlib.sha256((out / "train.jsonl").read_bytes()).hexdigest(),
       "sha256_heldout_tasks": hashlib.sha256((K / "heldout_tasks.jsonl").read_bytes()).hexdigest()}
(out / "prep.manifest.json").write_text(json.dumps(man, indent=1), encoding="utf-8")
print(json.dumps(man, indent=1))
