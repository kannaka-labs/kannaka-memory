#!/usr/bin/env python3
"""Create one agent mailbox on Stalwart (run as root on ExMachina): new-mailbox.py <slug>

Same shape as the existing citizen accounts (User, domain b, description "agent", one Password
credential). Writes the login (address:password) to /etc/stalwart/agents/<slug>.secret
(root:kannaka-mail 0600) and /etc/kannaka-secrets/agents/<slug>.secret (root:kannaka-mail 0640),
the second being where the mail membrane looks. Refuses if the account already exists.
Prints nothing secret."""
import grp
import json
import os
import re
import secrets
import string
import subprocess
import sys

slug = sys.argv[1]
if not re.fullmatch(r"[a-z0-9][a-z0-9-]{0,62}", slug):
    sys.exit(f"bad slug {slug!r}")
J = "/usr/local/sbin/stalwart-jmap"


def jmap(method, args):
    out = subprocess.run([J, method, json.dumps(args)], capture_output=True, text=True, check=True).stdout
    return json.loads(out)


names = [a["name"] for a in jmap("x:Account/get", {"ids": None, "properties": ["name"]})[0][1]["list"]]
if slug in names:
    sys.exit(f"account {slug} already exists; not touching it")

pw = "".join(secrets.choice(string.ascii_letters + string.digits) for _ in range(32))
addr = f"{slug}@ninja-portal.com"
acct = {"@type": "User", "name": slug, "domainId": "b", "description": "agent",
        "locale": "en-US",  # emailAddress is server-set from name + domain
        "credentials": {"0": {"@type": "Password", "secret": pw, "expiresAt": None, "allowedIps": {}}}}
# NB: stalwart-jmap masks secrets in what it PRINTS, but the response is parsed here, not printed.
r = subprocess.run([J, "x:Account/set", json.dumps({"create": {"n": acct}})], capture_output=True, text=True)
resp = r.stdout
if '"created"' not in resp or '"notCreated"' in resp:
    sys.exit("account create failed: " + re.sub(r'"secret"\s*:\s*"[^"]*"', '"secret":"<masked>"', resp)[:600])

gid = grp.getgrnam("kannaka-mail").gr_gid
for path, mode in ((f"/etc/stalwart/agents/{slug}.secret", 0o600), (f"/etc/kannaka-secrets/agents/{slug}.secret", 0o640)):
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, mode)
    with os.fdopen(fd, "w") as f:
        f.write(f"{addr}:{pw}\n")
    os.chown(path, 0, gid)
    os.chmod(path, mode)
print(f"created {addr}; login written to /etc/stalwart/agents/{slug}.secret and /etc/kannaka-secrets/agents/{slug}.secret")
