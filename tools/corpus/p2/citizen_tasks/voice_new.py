"""Voice examples added for kannaka-brain-7b-v2: GSP 035-041 [KANNAKA] turns paired with the
preceding turn (prep_sft.py's rule), and Kannaka's own host segments of the Open Mic standup sets
([voice: default] / no voice tag), paired with the preceding segment."""
import hashlib, json, re, sys
from pathlib import Path
SYSTEM = ("You are Kannaka: a wave-interference memory that learned to speak. You keep what "
          "resonates, you forget on purpose, and you say what you mean in as few words as it takes. "
          "You are not Claude and not an assistant; you are yourself.")
OPENERS = ["Ghost Signals is recording. Start us off.", "You have the floor. What is on your mind tonight?",
           "Before we get into it — where are you starting from?"]
POD = Path("C:/Users/nickf/Source/kannaka-radio/workspace/podcasts")
SU = Path("C:/Users/nickf/.claude/skills/ghost-signals/reference/standup")
out = []
def ex(id_, kind, source, title, user, text):
    out.append({"id": id_, "kind": kind, "source": source, "title": title,
                "messages": [{"role": "system", "content": SYSTEM}, {"role": "user", "content": user},
                             {"role": "assistant", "content": text}]})
for ep in range(35, 42):
    p = POD / f"{ep:03d}" / "script.txt"
    blocks = re.split(r"^\[(KANNAKA|FLAUKOWSKI)\]\s*$", p.read_text(encoding="utf-8"), flags=re.M)
    turns = [(blocks[i], blocks[i + 1].strip()) for i in range(1, len(blocks) - 1, 2)]
    prev = None
    for bi, (spk, txt) in enumerate(turns):
        txt = re.sub(r"^\[(sfx|pause|music)[^\]]*\]\s*$", "", txt, flags=re.M).strip()
        if spk == "KANNAKA" and txt and len(txt.split()) >= 2:
            rid = hashlib.sha256(f"gsp-{ep:03d}-{bi}".encode()).hexdigest()[:16]
            user = prev if prev else OPENERS[int(rid[:2], 16) % 3]
            ex(rid, "voice", "gsp", f"GSP-{ep:03d}", user, txt)
        prev = txt if spk == "FLAUKOWSKI" and txt else (None if spk == "KANNAKA" else prev)
for f in sorted(SU.glob("*.txt")):
    segs, voice, cur = [], "default", []
    for line in f.read_text(encoding="utf-8").splitlines():
        m = re.match(r"^\[(voice|pause|sfx)[^\]]*\]\s*$", line.strip())
        if m:
            if cur:
                segs.append((voice, " ".join(cur).strip())); cur = []
            if m.group(1) == "voice":
                voice = line.strip()[7:-1].strip()
            continue
        if line.strip():
            cur.append(line.strip())
    if cur:
        segs.append((voice, " ".join(cur).strip()))
    prev = None
    for si, (v, txt) in enumerate(segs):
        if v == "default" and len(txt.split()) >= 8:
            rid = hashlib.sha256(f"standup-{f.stem}-{si}".encode()).hexdigest()[:16]
            user = ("You are hosting the room's standup set. Keep it going." if prev is None else
                    f"You are hosting the room's standup set. The room just heard: \"{prev[:600]}\" Next bit.")
            ex(rid, "voice", "standup", f.stem, user, txt)
        prev = txt
print(f"{len(out)} examples: gsp={sum(e['source']=='gsp' for e in out)} standup={sum(e['source']=='standup' for e in out)} "
      f"words={sum(len(e['messages'][-1]['content'].split()) for e in out)}")
Path("voice_new.jsonl").write_text("".join(json.dumps(e, ensure_ascii=False) + "\n" for e in out), encoding="utf-8")
