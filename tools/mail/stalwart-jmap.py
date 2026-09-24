#!/usr/bin/env python3
"""Tiny Stalwart JMAP admin helper (run as root on ExMachina).
   jmap.py '<method>' '<json args>'   -> prints the response with anything secret-looking masked
   jmap.py --session                  -> prints capability names and the admin account id
Credentials: /etc/stalwart/admin.secret = 'user:password'."""
import base64, json, re, ssl, sys, urllib.request

URL = "http://127.0.0.1:8080"
user, pw = open("/etc/stalwart/admin.secret").read().strip().split(":", 1)
AUTH = "Basic " + base64.b64encode(f"{user}:{pw}".encode()).decode()


def call(path, body=None):
    req = urllib.request.Request(URL + path, data=json.dumps(body).encode() if body is not None else None,
                                 headers={"Authorization": AUTH, "Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=20) as r:
        return json.loads(r.read())


def mask(o):
    s = json.dumps(o, indent=1)
    s = re.sub(r'("(?:secret|password|privateKey|apiKey|token)"\s*:\s*)"[^"]*"', r'\1"<masked>"', s, flags=re.I)
    return s


sess = call("/jmap/session")
acct = next(iter(sess.get("primaryAccounts", {}).values()), None) or next(iter(sess.get("accounts", {})), None)
if sys.argv[1] == "--session":
    print("capabilities:", sorted(sess.get("capabilities", {})))
    print("account:", acct)
    sys.exit(0)
method, args = sys.argv[1], json.loads(sys.argv[2])
args.setdefault("accountId", acct)
caps = [c for c in sess.get("capabilities", {}) if c.startswith("urn:ietf:params:jmap:core") or "stalwart" in c]
resp = call("/jmap", {"using": caps, "methodCalls": [[method, args, "c0"]]})
print(mask(resp.get("methodResponses", resp)))
