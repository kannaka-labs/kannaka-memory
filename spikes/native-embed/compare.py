# Parity table: native vectors vs Ollama (GPU run = data/ollama_ref.json, CPU run = data/ollama_ref_cpu.json).
import json, sys, numpy as np
G = np.array(json.load(open('data/ollama_ref.json'))['vectors'])
C = np.array(json.load(open('data/ollama_ref_cpu.json'))['vectors'])
OUT = (45, 148)  # Korean, Devanagari: tokenizer divergence (see README)
keep = [i for i in range(len(G)) if i not in OUT]

def top(M, k=5):
    S = M @ M.T
    np.fill_diagonal(S, -9)
    return np.argsort(-S, 1, kind='stable')[:, :k], S

def row(name, A, B):
    c = (A * B).sum(1)
    ck = c[keep]
    (ta, SA), (tb, SB) = top(A[keep]), top(B[keep])
    same = (ta == tb).all(1)
    sets = np.array([set(x) == set(y) for x, y in zip(ta, tb)])
    # a differing row is a "near-tie" if every item one side has in its top-5 is within 2e-3 of the other side's 5th score
    worst_gap = 0.0
    for i in np.where(~same)[0]:
        for j in set(ta[i]) ^ set(tb[i]):
            worst_gap = max(worst_gap, abs(SB[i, j] - SB[i, tb[i][4]]))
    print(f"{name:34s} all200: min {c.min():.6f} | 198: min {ck.min():.7f} median {np.median(ck):.7f} | "
          f"top-5 same order {same.sum()}/198, same set {sets.sum()}/198, top-1 {(ta[:,0]==tb[:,0]).sum()}/198, "
          f"largest ref gap behind a set change {worst_gap:.1e}")

row('ollama-CPU vs ollama-GPU', C, G)
for f in sys.argv[1:]:
    N = np.array(json.load(open(f)))
    row(f.split('out_')[-1][:-5] + ' vs ollama-GPU', N, G)
    row(f.split('out_')[-1][:-5] + ' vs ollama-CPU', N, C)
