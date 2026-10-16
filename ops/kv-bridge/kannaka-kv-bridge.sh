#!/bin/bash
# kannaka-kv-bridge — ADR-0042 Phase 2: mirror live bus state onto JetStream KV
# so any organ (Command Center MCP) reads it instantly, no request-reply.
#   consciousness/state   <- KANNAKA.consciousness (last-value)
#   roster/<agent_id>     <- QUEEN.phase.<agent_id> (bucket TTL 5m expires the departed)
#
# Populates via kannaka_internal, which is ALLOWED to publish $KV.* (KV puts
# are not in its ADR-0042 1b deny list). The buckets themselves are created by
# `writer` (the only identity with $JS.API.STREAM.CREATE):
#   nats --user writer ... kv add consciousness --history=1
#   nats --user writer ... kv add roster --history=1 --ttl=5m
#
# Deploy: install -m755 to /home/opc/, install the sibling .service unit,
# `systemctl enable --now kannaka-kv-bridge`. Source of truth: this repo.
set -a; . /home/opc/.kannaka-nats.env; set +a
# Credentials reach nats through the exported NATS_USER / NATS_PASSWORD (its
# environment), never --user/--password: argv is readable by every user on
# the box through `ps` and /proc/<pid>/cmdline.
NATS=/usr/local/bin/nats

roster_loop() {
  $NATS sub "QUEEN.phase.>" --raw 2>/dev/null \
    | while IFS= read -r msg; do
        [ -z "$msg" ] && continue
        id=$(printf "%s" "$msg" | python3 -c "import sys,json;print(json.load(sys.stdin).get('agent_id',''))" 2>/dev/null)
        [ -z "$id" ] && continue
        printf "%s" "$msg" | $NATS kv put roster "$id" >/dev/null 2>&1
      done
}

consciousness_loop() {
  $NATS sub "KANNAKA.consciousness" --raw 2>/dev/null \
    | while IFS= read -r msg; do
        [ -z "$msg" ] && continue
        printf "%s" "$msg" | $NATS kv put consciousness state >/dev/null 2>&1
      done
}

roster_loop &
consciousness_loop &
wait -n   # if either loop dies, exit -> systemd Restart=always revives both
