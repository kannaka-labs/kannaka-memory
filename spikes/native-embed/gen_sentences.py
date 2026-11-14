# Builds data/sentences.json (200 probe texts) from data/base.txt:
# short/plain/technical/unicode lines, medium paragraphs, and texts that cross
# Ollama's 256-token window.
import json, random, unicodedata

random.seed(7)
out = [l.replace("\\n", "\n") for l in open("data/base.txt", encoding="utf-8").read().split("\n") if l.strip()]
# one genuinely decomposed (NFD) string, since editors tend to recompose
out.append(unicodedata.normalize("NFD", "Café crème brûlée, decomposed"))
plain = [s for s in out if s.isascii() and len(s.split()) > 5]
for _ in range(22):  # medium paragraphs
    out.append(" ".join(random.sample(plain, random.randint(3, 8))))
for n in (14, 18, 20, 22, 24, 26, 30, 40, 60, 90):  # long, natural text
    out.append(" ".join(random.sample(plain, min(n, len(plain)))))
for n in (250, 253, 254, 255, 256, 257, 300):  # exact lengths around the window
    out.append(" ".join(["cat"] * n))
out.append("word " * 600)
out.append("Zażółć gęślą jaźń. " * 60)
out.append("今日は天気がいいですね。" * 40)
seen, uniq = set(), []
for s in out:
    if s not in seen:
        seen.add(s)
        uniq.append(s)
uniq = uniq[:200]
json.dump(uniq, open("data/sentences.json", "w", encoding="utf-8"), ensure_ascii=False, indent=0)
print(len(uniq))
