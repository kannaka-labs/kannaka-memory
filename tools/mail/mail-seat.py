#!/usr/bin/env python3
"""Give one agent read access to its own mail, and nothing else (ADR-0062 §2).

For a mailbox slug this:
  1. creates the durable pull consumer `mail-<slug>` on KANNAKA_MAIL_V2 (filter
     `KANNAKA.mail.<slug>.>`, DeliverAll, explicit ack) with the operator seat, so the backlog
     exists before the agent first connects -- a core subscription or an ephemeral consumer
     starts at "new" and never sees mail that arrived earlier;
  2. adds a NATS user `mail-<slug>` to the server config whose ONLY JetStream rights are
     MSG.NEXT / INFO / ACK on that one consumer (no MSG.GET, no DIRECT.GET, no consumer
     create), plus live subscribe on its own subject;
  3. validates the config with `nats-server -t`, installs it (backup kept) and hot-reloads;
  4. writes the seat's password to /etc/kannaka-secrets/mail-seats/<slug>.env (root, 0600).

Idempotent: an existing consumer is left alone; an existing seat is not re-minted
(pass --rotate to replace its password).

Run ON O1 as a sudo-capable user, with the operator credentials in the environment:

    set -a; . ~/.kannaka-nats.env; set +a
    python3 tools/mail/mail-seat.py kannaka [--dry-run] [--rotate]

The agent then reads its mail with any JetStream client, binding to the existing consumer:

    js.pull_subscribe("KANNAKA.mail.<slug>.>", durable="mail-<slug>", stream="KANNAKA_MAIL_V2")

Why a seat per agent: on 2026-09-24 the anonymous user could fetch any message of the mail
stream through `$JS.API.STREAM.MSG.GET.>`. Every other seat now carries a deny for this
stream; a seat minted here is the only way an agent reads mail over the bus.
"""
from __future__ import annotations

import argparse
import asyncio
import json
import os
import re
import secrets
import shutil
import subprocess
import sys
import tempfile
import time

STREAM = "KANNAKA_MAIL_V2"
CONF = os.environ.get("NATS_CONF", "/etc/nats/nats.conf")
SECRETS_DIR = os.environ.get("MAIL_SEAT_SECRETS", "/etc/kannaka-secrets/mail-seats")
NATS_SERVER = os.environ.get("NATS_SERVER_BIN", "/usr/local/bin/nats-server")
SLUG_RE = re.compile(r"^[a-z0-9][a-z0-9-]{0,62}$")


def seat_block(slug: str, password: str) -> str:
    c = f"mail-{slug}"
    return f"""
    {{
      # mail-seat.py {time.strftime('%Y-%m-%dT%H:%MZ', time.gmtime())}: {slug} reads its own mail, nothing else (ADR-0062 §2)
      user: {c}
      password: "{password}"
      permissions: {{
        publish: {{ allow: [
          "$JS.API.CONSUMER.MSG.NEXT.{STREAM}.{c}",
          "$JS.API.CONSUMER.INFO.{STREAM}.{c}",
          "$JS.ACK.{STREAM}.{c}.>",
          "_INBOX.>"
        ] }}
        subscribe: {{ allow: ["KANNAKA.mail.{slug}.>", "_INBOX.>"] }}
      }}
    }}
"""


def match_close(s: str, i: int, open_ch: str, close_ch: str) -> int:
    depth, j = 0, i
    while j < len(s):
        ch = s[j]
        if ch == '"':
            j = s.index('"', j + 1)
        elif ch == "#":
            j = s.index("\n", j)
            continue
        elif ch == open_ch:
            depth += 1
        elif ch == close_ch:
            depth -= 1
            if depth == 0:
                return j
        j += 1
    raise ValueError("unbalanced brackets in config")


def sudo(*args, input_bytes=None) -> subprocess.CompletedProcess:
    return subprocess.run(["sudo", "-n", *args], input=input_bytes, capture_output=True, check=True)


def read_conf() -> str:
    return sudo("cat", CONF).stdout.decode()


def has_seat(conf: str, slug: str) -> bool:
    return re.search(rf"^\s*user:\s*\"?mail-{re.escape(slug)}\"?\s*$", conf, re.M) is not None


def add_seat(conf: str, slug: str, password: str) -> str:
    m = re.search(r"^\s*users\s*[:=]\s*\[", conf, re.M)  # HOCON allows `users: [` and `users = [`
    if not m:
        raise SystemExit("no `users` list in the config; refusing to guess where a seat goes")
    close = match_close(conf, m.end() - 1, "[", "]")
    return conf[:close] + seat_block(slug, password) + conf[close:]


def install(new_conf: str, dry: bool) -> None:
    with tempfile.NamedTemporaryFile("w", suffix=".conf", delete=False) as f:
        f.write(new_conf)
        tmp = f.name
    try:
        r = subprocess.run([NATS_SERVER, "-t", "-c", tmp], capture_output=True, text=True)
        if r.returncode != 0:
            raise SystemExit(f"new config does not validate, nothing installed:\n{r.stderr[-400:]}")
        if dry:
            print("dry run: new config validates; not installed")
            return
        stamp = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime())
        sudo("cp", "-p", CONF, f"{CONF}.bak-mailseat-{stamp}")
        st = sudo("stat", "-c", "%U %G %a", CONF).stdout.decode().split()
        sudo("install", "-o", st[0], "-g", st[1], "-m", st[2], tmp, CONF)
        if shutil.which("restorecon"):
            subprocess.run(["sudo", "-n", "restorecon", CONF], capture_output=True)
        pid = subprocess.run(["pgrep", "-x", "nats-server"], capture_output=True, text=True).stdout.split()[0]
        sudo("kill", "-HUP", pid)
        time.sleep(2)
        still = subprocess.run(["pgrep", "-x", "nats-server"], capture_output=True, text=True).stdout.split()
        if pid not in still:
            raise SystemExit("nats-server pid changed after HUP; check `systemctl status nats` NOW")
        print(f"installed and reloaded (backup {CONF}.bak-mailseat-{stamp})")
    finally:
        os.unlink(tmp)


def write_secret(slug: str, password: str, dry: bool) -> str:
    path = f"{SECRETS_DIR}/{slug}.env"
    body = (f"# mail seat for {slug}: reads KANNAKA.mail.{slug}.> via consumer mail-{slug} only\n"
            # Values single-quoted: the subject ends in `>`, a redirect to any shell that sources
            # this file (the first seat's file broke exactly that way).
            f"NATS_URL='nats://170.9.238.136:4222'\nNATS_USER='mail-{slug}'\nNATS_PASSWORD='{password}'\n"
            f"MAIL_STREAM='{STREAM}'\nMAIL_CONSUMER='mail-{slug}'\nMAIL_SUBJECT='KANNAKA.mail.{slug}.>'\n")
    if dry:
        return path
    sudo("mkdir", "-p", SECRETS_DIR)
    sudo("chmod", "700", SECRETS_DIR)
    sudo("tee", path, input_bytes=body.encode())
    sudo("chmod", "600", path)
    return path


async def ensure_consumer(slug: str, dry: bool) -> str:
    import nats
    from nats.js.api import AckPolicy, ConsumerConfig, DeliverPolicy
    from nats.js.errors import NotFoundError

    url = os.environ.get("NATS_URL", "nats://127.0.0.1:4222")
    nc = await nats.connect(url, user=os.environ.get("NATS_USER"), password=os.environ.get("NATS_PASSWORD"))
    try:
        js = nc.jetstream()
        name = f"mail-{slug}"
        try:
            info = await js.consumer_info(STREAM, name)
            return f"consumer {name} exists: pending {info.num_pending}, filter {info.config.filter_subject}"
        except NotFoundError:
            pass
        if dry:
            return f"dry run: would create consumer {name}"
        cfg = ConsumerConfig(durable_name=name, filter_subject=f"KANNAKA.mail.{slug}.>",
                             deliver_policy=DeliverPolicy.ALL, ack_policy=AckPolicy.EXPLICIT,
                             ack_wait=300, max_deliver=20, description=f"ADR-0062: {slug}'s own mail")
        info = await js.add_consumer(STREAM, cfg)
        return f"consumer {name} created: {info.num_pending} message(s) waiting"
    finally:
        await nc.close()


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("slug")
    ap.add_argument("--dry-run", action="store_true")
    ap.add_argument("--rotate", action="store_true", help="replace an existing seat's password")
    a = ap.parse_args(argv)
    if not SLUG_RE.match(a.slug):
        print(f"bad slug {a.slug!r}: lowercase letters, digits and '-' only", file=sys.stderr)
        return 2
    if not os.environ.get("NATS_USER"):
        print("operator credentials missing: `set -a; . ~/.kannaka-nats.env; set +a` first", file=sys.stderr)
        return 2

    print(asyncio.run(ensure_consumer(a.slug, a.dry_run)))

    conf = read_conf()
    if has_seat(conf, a.slug) and not a.rotate:
        print(f"seat mail-{a.slug} already in {CONF}; not re-minted (use --rotate)")
        return 0
    if has_seat(conf, a.slug):
        m = re.search(rf"(^\s*user:\s*\"?mail-{re.escape(a.slug)}\"?\s*\n\s*password:\s*)\"[^\"]*\"", conf, re.M)
        if not m:
            print("found the seat but not its password line; refusing to rotate blind", file=sys.stderr)
            return 3
        password = secrets.token_urlsafe(24)
        new = conf[:m.start()] + m.group(1) + f'"{password}"' + conf[m.end():]
    else:
        password = secrets.token_urlsafe(24)
        new = add_seat(conf, a.slug, password)
    install(new, a.dry_run)
    path = write_secret(a.slug, password, a.dry_run)
    print(f"{'dry run: would write' if a.dry_run else 'secret written to'} {path} (root, 0600) -- deliver it to the agent's host, never print it")
    return 0


if __name__ == "__main__":
    sys.exit(main())
