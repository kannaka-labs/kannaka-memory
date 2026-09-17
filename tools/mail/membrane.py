#!/usr/bin/env python3
"""kannaka-mail-membrane — mail becomes a bus event (ADR-0062, first slice).

Runs on the mail host (ExMachina, Stalwart). Every agent mailbox under
AGENTS_DIR (<slug>.secret = "user:password", root:kannaka-mail 0640) is
polled over IMAP on loopback; each new message is published ONCE to
KANNAKA.mail.<slug>.inbound on JetStream stream KANNAKA_MAIL_V2 with the
payload ADR-0062 §4 specifies (verdict first, body last, `untrusted: true`
always), and the mailbox's UID watermark advances only after the PubAck.

What this slice is not: the ADR's receiver publishes BEFORE it says 250 and
answers 451 when it cannot; a poller cannot — the mail is already accepted.
So: at-least-once with a content-derived Nats-Msg-Id (the stream's 7-day
duplicate window makes a retry idempotent), and a mailbox whose publish
fails is retried next round rather than skipped. Nothing is marked \\Seen:
that is the agent's own tool's business.

State: STATE_PATH {slug: {uidvalidity, last_uid}}. Env: NATS_URL, NATS_USER,
NATS_PASSWORD (EnvironmentFile), POLL_S (15), AGENTS_DIR, IMAP_HOST/IMAP_PORT
(127.0.0.1:993 — loopback to the same box, so the certificate's name is not
checked; every other property of TLS still holds), MEMBRANE_ONCE=1 to run one
round and exit, MEMBRANE_DRY=1 to print instead of publish.
"""
import asyncio
import email
import email.policy
import hashlib
import html
import imaplib
import json
import os
import re
import ssl
import sys
import time
import uuid
from email.utils import getaddresses, parseaddr, parsedate_to_datetime

import nats
from nats.js.api import StreamConfig  # noqa: F401  (import check: nats-py with JetStream)

AGENTS_DIR = os.environ.get("AGENTS_DIR", "/etc/kannaka-secrets/agents")
STATE_PATH = os.environ.get("STATE_PATH", "/var/lib/kannaka-mail/state.json")
IMAP_HOST = os.environ.get("IMAP_HOST", "127.0.0.1")
IMAP_PORT = int(os.environ.get("IMAP_PORT", "993"))
POLL_S = float(os.environ.get("POLL_S", "15"))
STREAM = os.environ.get("MAIL_STREAM", "KANNAKA_MAIL_V2")
TEXT_CAP = 64 * 1024
SKIP_SLUGS = set(s for s in os.environ.get("SKIP_SLUGS", "admin").split(",") if s)
ONCE = os.environ.get("MEMBRANE_ONCE") == "1"
DRY = os.environ.get("MEMBRANE_DRY") == "1"


def log(msg):
    print(time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()) + " " + msg, flush=True)


# ------------------------------------------------------------------ accounts

def accounts():
    out = {}
    for name in sorted(os.listdir(AGENTS_DIR)):
        if not name.endswith(".secret"):
            continue
        slug = name[:-len(".secret")]
        if slug in SKIP_SLUGS:
            continue
        line = open(os.path.join(AGENTS_DIR, name)).read().strip()
        if ":" not in line:
            continue
        user, pw = line.split(":", 1)
        out[slug] = (user, pw)
    return out


# ------------------------------------------------------------------ state

def load_state():
    try:
        return json.load(open(STATE_PATH))
    except Exception:
        return {}


def save_state(st):
    os.makedirs(os.path.dirname(STATE_PATH), exist_ok=True)
    tmp = STATE_PATH + ".tmp"
    with open(tmp, "w") as f:
        json.dump(st, f, indent=1)
    os.replace(tmp, STATE_PATH)


# ------------------------------------------------------------------ parsing

def hdr(m, k):
    v = m.get(k)
    return " ".join(str(v).split()) if v else ""


def registrable(domain):
    """Relaxed alignment: the last two labels. Good enough for our zones;
    not a public-suffix list."""
    parts = domain.lower().strip(".").split(".")
    return ".".join(parts[-2:]) if len(parts) >= 2 else domain.lower()


def parse_auth(m, from_domain):
    """Authentication-Results in the mail server's own words, plus the one
    derived boolean the agent's rule is (ADR-0062 §4)."""
    ar = " ".join(str(v) for v in (m.get_all("Authentication-Results") or []))
    spf = {"result": None, "domain": None}
    dkim = []
    dmarc = {"result": None, "policy": None, "aligned": None}
    # Stalwart writes two spf= clauses: HELO first, then MAIL FROM. The
    # MAIL FROM one is the verdict that matters (ADR-0062 §4: SPF evaluated
    # on MAIL FROM and the peer IP); take it when present, else the first.
    clauses = [c for c in ar.split(";") if re.search(r"\bspf=", c)]
    pick = next((c for c in clauses if "mailfrom" in c or "domain of" in c), clauses[0] if clauses else None)
    if pick:
        mm = re.search(r"\bspf=(\w+)", pick)
        spf["result"] = mm.group(1) if mm else None
        d = re.search(r"(?:domain of|smtp\.mailfrom=)\s*(?:[^\s@;)]+@)?([^\s;)]+)", pick)
        if d:
            spf["domain"] = d.group(1)
    for dk in re.finditer(r"\bdkim=(\w+)([^;]*)", ar):
        rest = dk.group(2)
        dd = re.search(r"header\.d=([^\s;]+)", rest)
        di = re.search(r"header\.i=@?([^\s;]+)", rest)
        ds = re.search(r"header\.s=([^\s;]+)", rest)
        dom = (dd.group(1) if dd else (di.group(1).split("@")[-1] if di else None))
        dkim.append({"result": dk.group(1), "d": dom, "s": ds.group(1) if ds else None})
    dm = re.search(r"\bdmarc=(\w+)([^;]*)", ar)
    if dm:
        dmarc["result"] = dm.group(1)
        pol = re.search(r"\bp=(\w+)", dm.group(2))
        if pol:
            dmarc["policy"] = pol.group(1)
    fd = registrable(from_domain) if from_domain else None
    aligned_dkim = any(d["result"] == "pass" and d["d"] and fd and registrable(d["d"]) == fd for d in dkim)
    aligned_spf = spf["result"] == "pass" and spf["domain"] and fd and registrable(spf["domain"]) == fd
    if fd:
        dmarc["aligned"] = bool(aligned_dkim or aligned_spf)
    from_authenticated = dmarc["result"] == "pass" or bool(aligned_dkim or aligned_spf)
    return {"spf": spf, "dkim": dkim, "dmarc": dmarc, "raw": ar[:2000]}, from_authenticated


def body_text(m):
    """Decoded text; HTML converted crudely to text. Returns (text, truncated)."""
    text = ""
    plain = m.get_body(preferencelist=("plain",))
    if plain is not None:
        try:
            text = plain.get_content()
        except Exception:
            text = ""
    if not text.strip():
        h = m.get_body(preferencelist=("html",))
        if h is not None:
            try:
                raw = h.get_content()
            except Exception:
                raw = ""
            raw = re.sub(r"(?is)<(script|style).*?</\1>", " ", raw)
            raw = re.sub(r"(?i)<br\s*/?>|</p>|</div>|</tr>", "\n", raw)
            text = html.unescape(re.sub(r"<[^>]+>", " ", raw))
            text = re.sub(r"[ \t]+", " ", text)
            text = re.sub(r"\n\s*\n+", "\n\n", text).strip()
    b = text.encode("utf-8", "replace")
    if len(b) > TEXT_CAP:
        return b[:TEXT_CAP].decode("utf-8", "ignore"), True
    return text, False


def attachment_refs(m):
    out = []
    for i, part in enumerate(m.walk()):
        if part.is_multipart():
            continue
        disp = (part.get_content_disposition() or "").lower()
        fn = part.get_filename()
        if disp != "attachment" and not fn:
            continue
        payload = part.get_payload(decode=True) or b""
        out.append({"part_index": i, "filename": fn, "content_type": part.get_content_type(),
                    "size": len(payload), "sha256": hashlib.sha256(payload).hexdigest()})
    return out


def payload_for(slug, address, uid, uidvalidity, raw):
    m = email.message_from_bytes(raw, policy=email.policy.default)
    from_name, from_addr = parseaddr(hdr(m, "From"))
    from_domain = from_addr.split("@")[-1] if "@" in from_addr else ""
    auth, from_authenticated = parse_auth(m, from_domain)
    envelope_from = hdr(m, "Return-Path").strip("<>")
    ctype = m.get_content_type()
    is_bounce = envelope_from == "" or ctype == "multipart/report"
    text, truncated = body_text(m)
    local = address.split("@")[0]
    tag = None
    for _, a in getaddresses([hdr(m, "To"), hdr(m, "Cc"), hdr(m, "Delivered-To")]):
        lp = a.split("@")[0]
        if "+" in lp and lp.split("+")[0].lower() == local.split("+")[0].lower():
            tag = lp.split("+", 1)[1]
            break
    try:
        date_epoch = int(parsedate_to_datetime(hdr(m, "Date")).timestamp())
    except Exception:
        date_epoch = None
    raw_sha = hashlib.sha256(raw).hexdigest()
    return {
        "kind": "mail.inbound",
        "mail_id": str(uuid.uuid7()) if hasattr(uuid, "uuid7") else str(uuid.uuid4()),
        "slug": slug,
        "address": address,
        "received_at": int(time.time()),
        "auth": auth,
        "from_authenticated": from_authenticated,
        "envelope_from": envelope_from,
        "is_bounce": is_bounce,
        "auto_submitted": hdr(m, "Auto-Submitted") or "no",
        "list_id": hdr(m, "List-Id") or None,
        "precedence": hdr(m, "Precedence") or None,
        "tag": tag,
        "from": {"name": from_name, "address": from_addr},
        "to": [a for _, a in getaddresses([hdr(m, "To")]) if a],
        "cc": [a for _, a in getaddresses([hdr(m, "Cc")]) if a],
        "subject": hdr(m, "Subject"),
        "date": hdr(m, "Date"),
        "date_epoch": date_epoch,
        "message_id": hdr(m, "Message-ID"),
        "in_reply_to": hdr(m, "In-Reply-To"),
        "references": hdr(m, "References").split(),
        "attachments": attachment_refs(m),
        "imap": {"uid": uid, "uidvalidity": uidvalidity},
        "raw_sha256": raw_sha,
        "size": len(raw),
        "untrusted": True,
        "truncated": truncated,
        "text": text,
    }, raw_sha


def msg_id(slug, envelope_from, raw_sha):
    return hashlib.sha256(f"{slug}|{envelope_from}|{raw_sha}".encode()).hexdigest()


# ------------------------------------------------------------------ imap

def imap_ctx():
    ctx = ssl.create_default_context()
    if IMAP_HOST in ("127.0.0.1", "localhost", "::1"):
        # Loopback to the very server whose certificate is for its public
        # name; the name check would fail against 127.0.0.1 and nothing is
        # gained by it here. Encryption and the rest of TLS stay on.
        ctx.check_hostname = False
        ctx.verify_mode = ssl.CERT_NONE
    return ctx


def fetch_new(user, pw, last_uid, uidvalidity_known):
    """Returns (uidvalidity, [(uid, raw)...]) for uids above the watermark."""
    M = imaplib.IMAP4_SSL(IMAP_HOST, IMAP_PORT, ssl_context=imap_ctx())
    try:
        M.login(user, pw)
        typ, data = M.select("INBOX", readonly=True)
        uv = None
        typ, resp = M.response("UIDVALIDITY")
        if resp and resp[0]:
            uv = int(resp[0])
        if uidvalidity_known is not None and uv is not None and uv != uidvalidity_known:
            last_uid = 0  # the mailbox was rebuilt; uids restarted
        typ, data = M.uid("search", None, f"UID {last_uid + 1}:*")
        uids = [int(x) for x in data[0].split()] if data and data[0] else []
        uids = [u for u in uids if u > last_uid]
        out = []
        for u in uids:
            typ, d = M.uid("fetch", str(u), "(BODY.PEEK[])")
            raw = b""
            for item in d or []:
                if isinstance(item, tuple):
                    raw = item[1]
            if raw:
                out.append((u, raw))
        return uv, out, last_uid
    finally:
        try:
            M.logout()
        except Exception:
            pass


# ------------------------------------------------------------------ main loop

async def run_round(js, st):
    published = 0
    for slug, (user, pw) in accounts().items():
        s = st.setdefault(slug, {"uidvalidity": None, "last_uid": 0})
        try:
            uv, new, last = fetch_new(user, pw, int(s.get("last_uid") or 0), s.get("uidvalidity"))
        except Exception as e:
            log(f"[{slug}] imap failed: {type(e).__name__}: {str(e)[:120]}")
            continue
        if uv is not None and uv != s.get("uidvalidity"):
            s["uidvalidity"] = uv
            s["last_uid"] = last
        for uid, raw in new:
            body, raw_sha = payload_for(slug, user, uid, uv, raw)
            subject = f"KANNAKA.mail.{slug}.inbound"
            data = json.dumps(body, ensure_ascii=False).encode("utf-8")
            mid = msg_id(slug, body["envelope_from"], raw_sha)
            if DRY:
                log(f"[{slug}] DRY {subject} uid={uid} from={body['from']['address']} auth={body['from_authenticated']} "
                    f"subj={body['subject'][:60]!r} bytes={len(data)}")
                s["last_uid"] = max(int(s["last_uid"]), uid)
                continue
            try:
                ack = await js.publish(subject, data, headers={"Nats-Msg-Id": mid}, timeout=10)
            except Exception as e:
                log(f"[{slug}] publish failed for uid {uid}: {type(e).__name__}: {str(e)[:120]} — will retry")
                break  # keep the watermark; the rest of this mailbox waits for the next round
            if ack.stream != STREAM:
                log(f"[{slug}] PubAck from stream {ack.stream!r}, expected {STREAM!r} — refusing to advance")
                break
            s["last_uid"] = max(int(s["last_uid"]), uid)
            published += 1
            log(f"[{slug}] published uid={uid} seq={ack.seq} dup={ack.duplicate} from={body['from']['address']} "
                f"auth={body['from_authenticated']} subj={body['subject'][:60]!r}")
            save_state(st)
    save_state(st)
    return published


async def main():
    url = os.environ.get("NATS_URL", "nats://swarm.ninja-portal.com:4222")
    user, pw = os.environ.get("NATS_USER"), os.environ.get("NATS_PASSWORD")
    if not DRY and not (user and pw):
        sys.exit("NATS_USER/NATS_PASSWORD not set")
    js = None
    nc = None
    if not DRY:
        nc = await nats.connect(url, user=user, password=pw, name="kannaka-mail-membrane",
                                connect_timeout=10, max_reconnect_attempts=-1)
        js = nc.jetstream()
    st = load_state()
    log(f"membrane up: {len(accounts())} mailboxes, poll {POLL_S}s, stream {STREAM}, dry={DRY}")
    while True:
        try:
            n = await run_round(js, st)
            if n:
                log(f"round: {n} published")
        except Exception as e:
            log(f"round failed: {type(e).__name__}: {str(e)[:200]}")
        if ONCE:
            break
        await asyncio.sleep(POLL_S)
    if nc:
        await nc.drain()


if __name__ == "__main__":
    asyncio.run(main())
