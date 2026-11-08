#!/bin/bash
# prime-hrm-guard.sh — hourly backstop against the 2026-07-04 disk-full incident
# on the primary Oracle box (ninjaportal).
#
# What happened: kannaka-radio's ghost DJ "hears herself" (`kannaka hear` after
# every voice segment in voice-dj.js / live-broadcast.js), so audio:heard
# memories accumulated in the prime HRM with no shedding — 37 MB → 372 MB in
# ~6 weeks. When the root disk hit 100%, HRM atomic saves failed mid-rename and
# stranded six full-size kannaka.hrm.tmp.* copies (~2.3 GB), slamming the disk
# shut: SQLite writes failed, radio/observatory died, nginx served 502.
#
# This guard (cron, hourly):
#   1. clears orphaned atomic-save temp files
#   2. prunes audio:* perception memories when the HRM exceeds SIZE_MB
#      (engine-level [triage] auto-shedding is the first line of defense;
#      this is the belt-and-suspenders backstop)
#   3. logs a loud warning when the root disk crosses DISK_WARN_PCT
#
# Install: install -m755 ops/oracle/prime-hrm-guard.sh ~/bin/
# Cron:    37 * * * * /home/opc/bin/prime-hrm-guard.sh >> /tmp/prime-hrm-guard.log 2>&1
set -u
KANNAKA="${KANNAKA_BIN:-$HOME/kannaka-memory/target/release/kannaka}"
HRM="$HOME/.kannaka/kannaka.hrm"
SIZE_MB="${PRIME_PRUNE_SIZE_MB:-100}"
DISK_WARN_PCT="${DISK_WARN_PCT:-85}"
TS="$(date -u +%FT%TZ)"

exec 9>/tmp/prime-hrm-guard.lock
flock -n 9 || { echo "[hrm-guard $TS] another run in progress — skip"; exit 0; }

# 1. orphaned save temps older than 6h (an active save never lives that long)
find "$HOME/.kannaka" -maxdepth 1 -name "kannaka.hrm.tmp.*" -mmin +360 -print -delete 2>/dev/null | sed "s/^/[hrm-guard $TS] removed orphan: /"

# 2. size-triggered audio prune
hrm_mb=0; [ -f "$HRM" ] && hrm_mb=$(du -m "$HRM" | cut -f1)
echo "[hrm-guard $TS] hrm_mb=$hrm_mb limit=$SIZE_MB"
if [ "$hrm_mb" -ge "$SIZE_MB" ]; then
  sudo systemctl stop kannaka-radio kannaka-memory
  sleep 1
  cp "$HRM" "${HRM}.pre-prune-$(date +%s)"
  # Log the "forgotten=N not_found=M" count and the forget-event summary
  # (since #1157 the summary is the last line, so `tail -1` dropped the count).
  "$KANNAKA" prune-prefix "audio:heard" "audio:/tmp/" "audio:/var/tmp/" 2>&1 | grep '^\[prune-prefix\]'
  sudo systemctl start kannaka-memory kannaka-radio
  echo "[hrm-guard $TS] pruned — hrm now $(du -m "$HRM" | cut -f1) MB"
  ls -1t "${HRM}".pre-prune-* 2>/dev/null | tail -n +3 | xargs -r rm -f
fi

# 3. disk headroom warning
pct=$(df --output=pcent / | tail -1 | tr -dc 0-9)
if [ "$pct" -ge "$DISK_WARN_PCT" ]; then
  echo "[hrm-guard $TS] WARNING: root disk at ${pct}% — investigate before it fills"
  logger -t hrm-guard "root disk at ${pct}%, HRM ${hrm_mb}MB — see /tmp/prime-hrm-guard.log"
fi
