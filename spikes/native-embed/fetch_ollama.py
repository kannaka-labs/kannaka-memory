# Fetches reference vectors from a local Ollama (`POST /api/embed`, one text
# per request, exactly as kannaka's OllamaEncoder calls it) and records timing.
#   python fetch_ollama.py [url]        -> data/ollama_ref.json      (Ollama's default placement)
#   python fetch_ollama.py [url] cpu    -> data/ollama_ref_cpu.json  (options.num_gpu = 0)
import json, sys, time, urllib.request

url = (sys.argv[1] if len(sys.argv) > 1 else "http://localhost:11434").rstrip("/")
cpu = "cpu" in sys.argv[2:]
out = "data/ollama_ref_cpu.json" if cpu else "data/ollama_ref.json"
sents = json.load(open("data/sentences.json", encoding="utf-8"))


def embed(text):
    req = urllib.request.Request(
        url + "/api/embed",
        data=json.dumps({"model": "all-minilm", "input": text, **({"options": {"num_gpu": 0}} if cpu else {})}).encode(),
        headers={"Content-Type": "application/json"},
    )
    return json.load(urllib.request.urlopen(req, timeout=120))


# Unload first: a runner loaded with num_gpu=0 is reused by a later default
# request, which would silently turn the "default" run into a second CPU run.
urllib.request.urlopen(urllib.request.Request(
    url + "/api/embed", data=json.dumps({"model": "all-minilm", "input": "x", "keep_alive": 0}).encode(),
    headers={"Content-Type": "application/json"}), timeout=120).read()
time.sleep(4)
t0 = time.perf_counter()
first = embed(sents[0])
cold = time.perf_counter() - t0
vecs, counts, times = [], [], []
for s in sents:
    t = time.perf_counter()
    r = embed(s)
    times.append(time.perf_counter() - t)
    vecs.append(r["embeddings"][0])
    counts.append(r.get("prompt_eval_count"))
times.sort()
json.dump({"vectors": vecs, "prompt_eval_count": counts}, open(out, "w"))
print(f"n={len(vecs)} first-call(cold)={cold*1000:.0f} ms  warm median={times[len(times)//2]*1000:.1f} ms  "
      f"mean={sum(times)/len(times)*1000:.1f} ms  total={sum(times):.2f} s")
