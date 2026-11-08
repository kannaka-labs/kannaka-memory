#!/usr/bin/env python3
"""Reference verifier for `kannaka digest` (schema kannaka-digest/v1, #1156).

Standard library only, so checking Kannaka's published digests needs no Rust.
The byte layout is specified in docs/digest-v1.md; this file is the second,
independent implementation of it, and tests/digest_v1_vectors.json is checked
by both.

  verify_digest.py ids-root DIGEST.json
      Recompute ids_root from the digest's published "ids" list and compare.
  verify_digest.py reconcile D1.json D2.json EVENTS.ndjson
      Check D2.ids == D1.ids + remembered - forgotten, using memory events
      for the digest's agent. Each line: {"subject": ..., "payload": {...}}
      (or "data" instead of "payload"); the subject's last token is the kind.
  verify_digest.py vectors VECTORS.json
      Check the golden vectors.
"""
import hashlib, json, sys, uuid

SCHEMA = "kannaka-digest/v1"
LEAF, NODE, COUNT, ID_LEAF = b"\x00", b"\x01", b"\x02", b"\x03"


def h(*parts):
    return hashlib.sha256(b"".join(parts)).digest()


def tree(leaves):
    if not leaves:
        return bytes(32)
    level = list(leaves)
    while len(level) > 1:
        nxt = [h(NODE, level[i], level[i + 1]) for i in range(0, len(level) - 1, 2)]
        if len(level) % 2:
            nxt.append(level[-1])  # odd node is promoted, never duplicated
        level = nxt
    return level[0]


def commit(count, root):
    return h(COUNT, count.to_bytes(8, "little"), root)


def canon(i):
    """Canonical lowercase hyphenated form, so case/format never fakes a gap."""
    return str(uuid.UUID(i))


def ids_root(ids):
    ids = sorted({uuid.UUID(i).bytes for i in ids})
    return commit(len(ids), tree([h(ID_LEAF, i) for i in ids])).hex()


def digest_leaves(pairs):
    """pairs: iterable of (uuid str, content_sha256 hex). Returns the hashed fields."""
    rows = sorted((uuid.UUID(i).bytes, bytes.fromhex(c)) for i, c in dict(pairs).items())
    leaves = [h(LEAF, i, c) for i, c in rows]
    n = len(rows).to_bytes(8, "little")
    return {
        "count": len(rows),
        "set_hash": h(SCHEMA.encode() + b"\n", n, *leaves).hex(),
        "merkle_root": commit(len(rows), tree(leaves)).hex(),
        "ids_root": ids_root(str(uuid.UUID(bytes=i)) for i, _ in rows),
    }


def main(argv):
    cmd = argv[1] if len(argv) > 1 else ""
    if cmd == "ids-root":
        d = json.load(open(argv[2], encoding="utf-8"))
        got = ids_root(d["ids"])
        ok = got == d["ids_root"] and len(set(d["ids"])) == d["count"]
        print(("OK" if ok else "MISMATCH"), got)
        return 0 if ok else 1
    if cmd == "reconcile":
        d1, d2 = json.load(open(argv[2], encoding="utf-8")), json.load(open(argv[3], encoding="utf-8"))
        for name, d in (("D1", d1), ("D2", d2)):
            if ids_root(d["ids"]) != d["ids_root"]:
                print(f"{name}: published ids do not match ids_root")
                return 1
        if d1.get("agent_id") != d2.get("agent_id"):
            print("D1 and D2 are for different agents")
            return 2
        added, removed = set(), set()
        for line in open(argv[4], encoding="utf-8"):
            if not line.strip():
                continue
            ev = json.loads(line)
            p = ev.get("payload") or ev.get("data") or {}
            if d2.get("agent_id") and p.get("agent_id") != d2["agent_id"]:
                continue
            # The lane also carries .recall events (memory_ids, plural);
            # only remember/forget move the set.
            kind = ev["subject"].rsplit(".", 1)[-1]
            if kind == "remember":
                added.add(canon(p["memory_id"]))
            elif kind == "forget":
                removed.add(canon(p["memory_id"]))
        expected = ({canon(i) for i in d1["ids"]} | added) - removed
        actual = {canon(i) for i in d2["ids"]}
        unlogged_new = sorted(actual - expected)
        unlogged_gone = sorted(expected - actual)
        print(json.dumps({"balanced": not unlogged_new and not unlogged_gone,
                          "in_d2_without_remember": unlogged_new,
                          "missing_from_d2_without_forget": unlogged_gone}, indent=2))
        return 0 if not unlogged_new and not unlogged_gone else 1
    if cmd == "vectors":
        bad = 0
        for v in json.load(open(argv[2], encoding="utf-8"))["vectors"]:
            pairs = [(m["id"], hashlib.sha256(m["content"].encode()).hexdigest()) for m in v["memories"]]
            got = digest_leaves(pairs)
            for k in ("count", "set_hash", "merkle_root", "ids_root"):
                if got[k] != v[k]:
                    bad += 1
                    print(f"{v['name']}: {k} expected {v[k]} got {got[k]}")
        print("OK" if not bad else f"{bad} mismatch(es)")
        return 1 if bad else 0
    print(__doc__)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
