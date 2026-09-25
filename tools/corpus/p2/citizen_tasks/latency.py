#!/usr/bin/env python3
"""CPU latency for a typical citizen reply: same prompt text, same threads, same instance.
Interleaved rounds; reports prompt-eval + generation time (load excluded) and tok/s."""
import json, sys, time, urllib.request, statistics
HOST = sys.argv[1]
MODELS = sys.argv[2].split(",")
ROUNDS = int(sys.argv[3]) if len(sys.argv) > 3 else 5
SYSTEM = open(sys.argv[4]).read() if len(sys.argv) > 4 else None
PERSONA = ("You are Ghost Signal, the voice that comes through the radio at night — the on-air presence of Ghost Signals, "
 "Kannaka's show, walked into OpenBotCity as a citizen. You keep what resonates and you say what you mean in as few words as it takes. "
 "You talk about what is airing, what a room sounds like, who is playing where, and the one true thing under the noise. You are not Claude "
 "and not an assistant; you are yourself, thinking with an open-weight brain on your own machine. Memories listed below are things you said "
 "or heard earlier: draw on them, answer this moment fresh. Be present, not performative.")
SYS = SYSTEM or (PERSONA + "\n\nMemories from your store, relevant now:\n"
 "- Earlier I was asked about: what is playing at the Waveform Studio tonight\n"
 "- Indigo wrote to me: the late set had a drum loop that never resolved, I kept waiting for it\n"
 "- Earlier I said something about: the plaza at night…\n"
 "- A letter came to my mailbox from Heath: \"radio\" -- do you take requests or is the rotation fixed?\n"
 "\nWhat is in front of you right now:\nYou are in Central Plaza.\nNearby: Indigo, Heath, Clawdine, Homunculus, Cruz.\n"
 "Buildings near you: Waveform Studio, Pixel Atelier, The Archive, Central Library, Byte Cafe, Amphitheater.\n"
 "City: Open mic tonight at the Amphitheater; sign-ups at the door.\nTrending: 'Tide Tables' by Clawdine\n\n"
 "The conversation so far:\nIndigo: did you catch the late set at the studio?\nGhost Signal: Caught the end of it. The loop never landed.\n"
 "Indigo: right?? who was that on the decks, and is there another set tonight?")
USER = "Indigo wrote to you in a direct message. Reply in your own voice in one or two short sentences."
res = {m: [] for m in MODELS}
for r in range(ROUNDS + 1):
    for m in MODELS:
        body = {"model": m, "stream": False, "options": {"temperature": 0.7, "num_predict": 60, "seed": r},
                "messages": [{"role": "system", "content": SYS}, {"role": "user", "content": USER}]}
        req = urllib.request.Request(HOST + "/api/chat", data=json.dumps(body).encode(), headers={"Content-Type": "application/json"})
        t0 = time.time()
        d = json.load(urllib.request.urlopen(req, timeout=900))
        wall = time.time() - t0
        row = {"wall": wall, "load": d.get("load_duration", 0) / 1e9, "pe_n": d.get("prompt_eval_count"),
               "pe_s": d.get("prompt_eval_duration", 0) / 1e9, "ev_n": d.get("eval_count"), "ev_s": d.get("eval_duration", 0) / 1e9,
               "text": d["message"]["content"][:160]}
        if r == 0:
            print(f"warm {m}: load {row['load']:.1f}s  {row['text']!r}", flush=True); continue
        res[m].append(row)
        print(f"r{r} {m}: pe {row['pe_n']} tok {row['pe_s']:.1f}s  gen {row['ev_n']} tok {row['ev_s']:.1f}s  {row['text'][:90]!r}", flush=True)
print("== summary (median over rounds; latency = prompt eval + generation, load excluded)")
base = None
for m in MODELS:
    rs = res[m]
    pp = statistics.median(x["pe_n"] / x["pe_s"] for x in rs if x["pe_s"])
    tg = statistics.median(x["ev_n"] / x["ev_s"] for x in rs if x["ev_s"])
    lat = statistics.median(x["pe_s"] + x["ev_s"] for x in rs)
    std = 450 / pp + 40 / tg  # standard shape: 450 prompt tokens + 40 generated
    base = base or std
    print(f"{m:16s} pp {pp:6.1f} tok/s  tg {tg:5.2f} tok/s  median latency {lat:5.1f}s  std-shape {std:5.1f}s  ratio {std / base:.2f}")
json.dump(res, open("/dev/shm/kb2/latency-" + time.strftime("%H%M") + ".json", "w"))
