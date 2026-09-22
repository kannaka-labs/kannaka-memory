#!/bin/bash
# host-metrics.sh — publish host health + cron liveness to Flux every 10 min.
#
# Born from the 2026-07-04 incident: the disk filled over weeks, two prune
# crons failed silently for weeks, and the first signal anyone saw was a
# public bad gateway. This makes the leading indicators visible in Flux
# (namespace entity <FLUX_NAMESPACE>/host-<HOST_NAME>) so the curve is
# watchable days before it becomes an outage.
#
# Config (env or /home/opc/.kannaka-flux.env sourced by the cron wrapper):
#   FLUX_URL, FLUX_TOKEN, FLUX_NAMESPACE  — Flux ingestion (see flux contract)
#   HOST_NAME                             — short host label (no IPs — Flux reads are public)
#   CRON_LOGS                             — space-separated list of log files that
#                                           a healthy cron touches at least daily
#   HRM_PATH                              — default ~/.kannaka/kannaka.hrm
#
# Install: install -m755 ops/services/host-metrics.sh ~/bin/
# Cron:    3,13,23,33,43,53 * * * * <wrapper that sources flux env> ~/bin/host-metrics.sh
set -u

HOST_NAME="${HOST_NAME:-$(hostname -s)}"
HRM="${HRM_PATH:-$HOME/.kannaka/kannaka.hrm}"
NS="${FLUX_NAMESPACE:-pure-jade}"
STALE_HOURS="${CRON_STALE_HOURS:-26}"

disk_pct=$(df --output=pcent / | tail -1 | tr -dc 0-9)
hrm_mb=0; [ -f "$HRM" ] && hrm_mb=$(du -m "$HRM" | cut -f1)
load1=$(cut -d" " -f1 /proc/loadavg)
mem_avail_mb=$(awk '/MemAvailable/ {print int($2/1024)}' /proc/meminfo)
orphan_tmps=$(find "$(dirname "$HRM")" -maxdepth 1 -name "kannaka.hrm.tmp.*" 2>/dev/null | wc -l)

stale=""
for f in ${CRON_LOGS:-}; do
  if [ ! -f "$f" ] || [ -n "$(find "$f" -mmin +$((STALE_HOURS * 60)) 2>/dev/null)" ]; then
    stale="$stale$(basename "$f") "
  fi
done
stale="${stale% }"
[ -n "$stale" ] && logger -t host-metrics "STALE CRON LOGS on $HOST_NAME: $stale"

now="$(date -u +%FT%TZ)"
now_ms="$(date +%s%3N)"
payload=$(cat <<JSON
{"stream":"constellation","source":"host-metrics","timestamp":$now_ms,"payload":{"entity_id":"$NS/host-$HOST_NAME","properties":{"type":"host","disk_pct":$disk_pct,"hrm_mb":$hrm_mb,"load1":$load1,"mem_avail_mb":$mem_avail_mb,"orphan_hrm_tmps":$orphan_tmps,"stale_crons":"$stale","checked_at":"$now"}}}
JSON
)

# The result is INSPECTED, not discarded. `>/dev/null || true` made an
# outage, a revoked FLUX_TOKEN and a healthy run indistinguishable -- no
# output, exit 0, nothing in the log -- on the one job whose silence means
# everything it monitors (stale_crons included) goes unwatched as well.
flux_code=$(curl -s -o /dev/null -w '%{http_code}' --max-time 15 \
  -X POST "${FLUX_URL:-https://api.flux-universe.com}/api/events" \
  -H "Authorization: Bearer ${FLUX_TOKEN:-}" \
  -H "Content-Type: application/json" \
  -d "$payload" 2>/dev/null) || flux_code="000"

case "$flux_code" in
  2*) ;;
  # A failed POST still must not abort the run or spam cron mail every tick,
  # so the failure is one line plus an exit status -- enough to have a
  # consequence somewhere, not enough to become noise.
  *)  echo "$(date -Is) host-metrics: flux POST failed (http $flux_code)" >&2
      exit 1 ;;
esac
