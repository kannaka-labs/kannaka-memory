#!/bin/bash
# kannaka-witness-loop.sh — continuous radio-stream perception loop for a
# witness Kannaka node (e.g. Oracle-2 / 0xscada-node2).
#
# The witness "blooms" its HRM by hearing the live radio stream on a slow tick
# and participating in the swarm's Kuramoto phase coupling. It is a read-mostly
# observer of the constellation, not a primary writer.
#
# History: v0.3.12 turned `swarm join` into a long-running daemon. The loop kept
# the original announce+hear+sleep architecture by calling `swarm join --once`.
# That re-emitted a queen.event.join on EVERY tick, which the radio announced
# on air every ~5 min. Fixed 2026-06-16: join ONCE at startup, then `swarm sync`
# each tick (phase coupling without a join event).
set -u
ENV_FILE="${WITNESS_ENV_FILE:-/etc/kannaka-witness.env}"
KANNAKA="${KANNAKA_BIN:-/usr/local/bin/kannaka}"
STREAM_URL="${WITNESS_STREAM_URL:-https://radio.ninja-portal.com/stream}"
TICK_SLEEP="${WITNESS_TICK_SLEEP:-300}"

[ -f "$ENV_FILE" ] && { set -a; . "$ENV_FILE"; set +a; }

trap 'echo "[witness] shutting down"; exit 0' SIGTERM SIGINT

echo "[witness] starting — agent_id=${KANNAKA_AGENT_ID:-?} tick=${TICK_SLEEP}s"

# The true session start, stamped ONCE. `swarm sync` publishes the presence
# heartbeat and reads this so `joined_at` is the loop's start, not the tick
# (#587) — a one-shot process cannot know it otherwise.
export KANNAKA_SESSION_JOINED_AT
KANNAKA_SESSION_JOINED_AT=$(date -u +%FT%TZ)

# One real join announcement at startup.
"$KANNAKA" swarm join --once >/dev/null 2>&1 || true

while :; do
  TS=$(date -u +%FT%TZ)
  HEAR_OUT=$("$KANNAKA" hear "$STREAM_URL" 2>&1)
  TAGS=$(echo "$HEAR_OUT" | grep -E "Tags:" | sed "s/^.*Tags: //")
  if [ -n "$TAGS" ]; then
    echo "$TS heard: $TAGS"
  else
    echo "$TS hear failed; tail: $(echo "$HEAR_OUT" | tail -3 | tr "\n" " | ")"
  fi
  # Phase-sync + presence heartbeat, no re-announce. `swarm sync` couples
  # phases (Kuramoto) and republishes KANNAKA.presence. Until 0.16.7 it only
  # wrote QUEEN.phase, and this comment claimed presence — so the witness's
  # roster row went stale minutes after startup while the ticks kept going.
  "$KANNAKA" swarm sync >/dev/null 2>&1 || true
  sleep "$TICK_SLEEP"
done
