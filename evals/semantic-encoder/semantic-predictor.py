#!/usr/bin/env python3
"""Semantic-encoder predictor: embed the frozen corpus + probe queries with an
Ollama model, rank by cosine, score recall@10/MRR against pinned UUIDs.
Pure-encoder cosine was validated against the production medium in the ablation
(reproduced all production hits), so this predicts in-medium behavior.
"""
import json, sys, urllib.request

MODEL = sys.argv[1] if len(sys.argv) > 1 else "all-minilm"
SC = r"C:\Users\nickf\AppData\Local\Temp\claude\C--Windows-System32\b2c5fcd4-7a81-4d6f-aade-0f1192b9a2b1\scratchpad"
REPO = r"C:\Users\nickf\Source\kannaka-memory"


def embed(texts):
    out = []
    B = 64
    for i in range(0, len(texts), B):
        req = urllib.request.Request(
            "http://127.0.0.1:11434/api/embed",
            data=json.dumps({"model": MODEL, "input": texts[i:i+B]}).encode(),
            headers={"Content-Type": "application/json"})
        with urllib.request.urlopen(req, timeout=600) as r:
            out.extend(json.load(r)["embeddings"])
        print(f"  embedded {min(i+B,len(texts))}/{len(texts)}", file=sys.stderr)
    return out


def norm(v):
    s = sum(x*x for x in v) ** 0.5
    return [x/s for x in v] if s else v


def score(name, probes, expected, doc_ids, doc_vecs):
    qv = [norm(v) for v in embed([p["query"] for p in probes])]
    hits, per, mrr_sum, rec_sum = 0, {}, 0.0, 0.0
    for p, q in zip(probes, qv):
        sims = sorted(((sum(a*b for a, b in zip(q, d)), i) for i, d in enumerate(doc_vecs)), reverse=True)
        top10 = [doc_ids[i] for _, i in sims[:10]]
        rel = set(expected[p["id"]])
        rank = next((i+1 for i, x in enumerate(top10) if x in rel), 0)
        h = sum(1 for x in top10 if x in rel)
        rec_sum += h/len(rel); mrr_sum += (1.0/rank if rank else 0.0)
        if rank: hits += 1; per[p["id"]] = rank
    n = len(probes)
    print(f"{name}: recall@10={rec_sum/n:.4f}  mrr={mrr_sum/n:.4f}  hits={hits}/{n}")
    print(f"  ranks: {json.dumps(per)}")
    return {"recall_at_10": round(rec_sum/n, 4), "mrr": round(mrr_sum/n, 4), "hits": hits, "n": n, "ranks": per}


mem = json.load(open(SC + r"\zo-export\export-slim.json", encoding="utf-8"))
doc_ids = [m["id"] for m in mem]
print(f"embedding {len(mem)} memories with {MODEL}...", file=sys.stderr)
doc_vecs = [norm(v) for v in embed([m["content"] for m in mem])]

p50 = json.load(open(REPO + r"\evals\recall-paraphrase-regression\environment\probes.json", encoding="utf-8"))
e50 = json.load(open(REPO + r"\evals\recall-paraphrase-regression\tests\expected.json", encoding="utf-8"))
p33 = json.load(open(REPO + r"\evals\zero-overlap-anomaly\environment\probes.json", encoding="utf-8"))
e33 = json.load(open(REPO + r"\evals\zero-overlap-anomaly\tests\expected.json", encoding="utf-8"))

res = {"model": MODEL,
       "paraphrase_50": score("paraphrase-50", p50, e50, doc_ids, doc_vecs),
       "zero_overlap_33": score("zero-overlap-33", p33, e33, doc_ids, doc_vecs)}
json.dump(res, open(SC + rf"\predictor-{MODEL.replace(':','_').replace('/','_')}.json", "w"), indent=1)
