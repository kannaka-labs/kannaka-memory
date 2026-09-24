#!/bin/bash
# Turn on outbound mail for ninja-portal.com through Resend (ADR-0062 outbound half).
# Oracle blocks outbound :25, so Stalwart cannot deliver to remote MX itself; Resend is the relay.
#
#   printf '%s' 're_XXXX' | sudo stalwart-enable-resend            # enable
#   sudo stalwart-enable-resend --status                            # show route + strategy
#   sudo stalwart-enable-resend --disable                           # back to direct MX (sends nothing)
#
# The key goes to /etc/stalwart/resend.key (root:stalwart 0640) and is referenced by the route as
# {"@type":"File","filePath":...}; it never enters Stalwart's config or any command line.
set -euo pipefail
J=/usr/local/sbin/stalwart-jmap
KEY=/etc/stalwart/resend.key
LOCAL_IF="is_local_domain(rcpt_domain)"   # one argument in v0.16 (two in older docs)

route_id() { $J "x:MtaRoute/get" '{"ids":null}' | python3 -c 'import json,sys; r=json.load(sys.stdin)[0][1]["list"]; print(next((x["id"] for x in r if x.get("name")=="resend"),""))'; }
strategy_ids() { $J "x:MtaOutboundStrategy/get" '{"ids":null}' | python3 -c 'import json,sys; print(" ".join(x["id"] for x in json.load(sys.stdin)[0][1]["list"]))'; }

case "${1:-}" in
  --status)
    $J "x:MtaRoute/get" '{"ids":null}' | grep -E '"name"|"address"|"port"|"@type"' ; $J "x:MtaOutboundStrategy/get" '{"ids":null}'; exit 0 ;;
  --disable)
    for s in $(strategy_ids); do $J "x:MtaOutboundStrategy/set" "{\"update\":{\"$s\":{\"route\":{\"match\":{\"0\":{\"if\":\"$LOCAL_IF\",\"then\":\"'local'\"}},\"else\":\"'mx'\"}}}}" >/dev/null; done
    systemctl restart stalwart
    echo "outbound strategy back to direct MX (which Oracle blocks: remote mail will queue)"; exit 0 ;;
esac

key=$(cat)
[[ "$key" =~ ^re_[A-Za-z0-9_]{16,}$ ]] || { echo "refusing: stdin is not a Resend API key (re_...)" >&2; exit 2; }
umask 027
printf '%s' "$key" > "$KEY"; chown root:stalwart "$KEY"; chmod 640 "$KEY"; unset key
echo "key stored: $KEY ($(stat -c '%a %U:%G' "$KEY"))"

rid=$(route_id)
ROUTE='{"@type":"Relay","name":"resend","description":"Resend SMTP relay (Oracle blocks outbound 25)","address":"smtp.resend.com","port":465,"protocol":"smtp","implicitTls":true,"allowInvalidCerts":false,"authUsername":"resend","authSecret":{"@type":"File","filePath":"'"$KEY"'"}}'
if [ -z "$rid" ]; then
  out=$($J "x:MtaRoute/set" "{\"create\":{\"r\":$ROUTE}}")
else
  out=$($J "x:MtaRoute/set" "{\"update\":{\"$rid\":${ROUTE/\"name\":\"resend\",/}}}")   # name is read-only after create
fi
echo "$out" | grep -q '"notCreated"\|"notUpdated"' && { echo "route rejected:"; echo "$out"; exit 3; }
echo "route 'resend' ready"

EXPR="{\"match\":{\"0\":{\"if\":\"$LOCAL_IF\",\"then\":\"'local'\"}},\"else\":\"'resend'\"}"
ids=$(strategy_ids)
if [ -z "$ids" ]; then
  out=$($J "x:MtaOutboundStrategy/set" "{\"create\":{\"s\":{\"route\":$EXPR}}}")
else
  for s in $ids; do out=$($J "x:MtaOutboundStrategy/set" "{\"update\":{\"$s\":{\"route\":$EXPR}}}"); done
fi
echo "$out" | grep -q '"notCreated"\|"notUpdated"' && { echo "strategy rejected (route created, strategy unchanged):"; echo "$out"; exit 4; }
# The running queue keeps the strategy it loaded at start: on 2026-09-24 a letter queued after
# the strategy was set still went straight to the recipient's MX (blocked :25). Restart to load it.
systemctl restart stalwart; sleep 5; systemctl is-active --quiet stalwart || { echo "stalwart did not come back up"; exit 5; }
echo "outbound: local domains -> local, everything else -> resend. Test: send from an agent mailbox to an outside address."
