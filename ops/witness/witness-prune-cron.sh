#!/bin/bash
# witness-prune-cron.sh — keep the witness HRM from growing unbounded.
#
# Run hourly from cron. When the HRM crosses the SIZE threshold (or the memory
# count crosses THRESHOLD) it backs up the HRM, prunes audio perception
# memories by content prefix, and restarts the witness loop (so `kannaka hear`
# doesn't race the prune on the HRM lock).
#
# Prefixes pruned:
#   audio:heard   — current descriptive perception memories (ADR: hear stores
#                   "audio:heard <tempo> <tags> | ...")
#   audio:/tmp/   — legacy path-only memories from before the descriptive change
#   audio:/var/tmp/
#
# v2 (2026-07-04, incident postmortem): the previous version parsed the memory
# count out of `kannaka stats` human output. That failed SILENTLY twice —
# first a corrupted on-host heredoc (2026-06-17), then the stats output format
# changed ("Total memories: N") and hourly stats runs overlapped on the HRM
# lock, so the parse returned 0 → "below threshold" every hour while the HRM
# grew to 453 MB. Lessons baked in here:
#   1. flock guard — hourly runs can never overlap
#   2. PRIMARY trigger is HRM file SIZE (cannot lie, costs nothing)
#   3. count comes from status-cache.json (no HRM load, no lock contention)
#   4. below-threshold runs spawn no kannaka process at all
#   5. orphaned atomic-save temp files are cleaned (they strand when a save
#      fails on a full disk and then accelerate the fill)
set -u

KANNAKA="${KANNAKA_BIN:-/usr/local/bin/kannaka}"
SERVICE="${WITNESS_SERVICE:-kannaka-witness}"
THRESHOLD="${WITNESS_PRUNE_THRESHOLD:-4000}"
SIZE_HARD_MB="${WITNESS_PRUNE_SIZE_MB:-100}"
HRM="${KANNAKA_HRM:-$HOME/.kannaka/kannaka.hrm}"
CACHE="$HOME/.kannaka/status-cache.json"
TS="$(date -u +%FT%TZ)"

exec 9>/tmp/witness-prune.lock
flock -n 9 || { echo "[witness-prune $TS] another run in progress — skip"; exit 0; }

# Orphaned atomic-save temps older than 6h (an active save never lives that long).
find "$(dirname "$HRM")" -maxdepth 1 -name "kannaka.hrm.tmp.*" -mmin +360 -delete 2>/dev/null

hrm_mb=0
[ -f "$HRM" ] && hrm_mb=$(du -m "$HRM" | cut -f1)
total=0
[ -f "$CACHE" ] && total=$(grep -oE '"total_memories": *[0-9]+' "$CACHE" | grep -oE "[0-9]+" | head -1)
total="${total:-0}"
echo "[witness-prune $TS] hrm_mb=$hrm_mb size_hard=$SIZE_HARD_MB memories=$total threshold=$THRESHOLD"

if [ "$hrm_mb" -lt "$SIZE_HARD_MB" ] && [ "$total" -lt "$THRESHOLD" ]; then
  echo "[witness-prune $TS] below both thresholds — no action"
  exit 0
fi

# Stop the loop so its `kannaka hear` doesn't race the prune on the HRM lock,
# and kill any straggler stats/dream still holding the old giant HRM.
sudo systemctl stop "$SERVICE"
sleep 1
pkill -f "$KANNAKA (stats|dream)" 2>/dev/null
sleep 1

# Backup before pruning (same pattern as the primary box).
cp "$HRM" "${HRM}.pre-prune-$(date +%s)" 2>/dev/null || true

for prefix in "audio:heard" "audio:/tmp/" "audio:/var/tmp/"; do
  echo "[witness-prune $TS] prune-prefix \"$prefix\""
  # Count line + forget-event summary (#1157 made the summary the last line).
  "$KANNAKA" prune-prefix "$prefix" 2>&1 | grep '^\[prune-prefix\]'
done

sudo systemctl start "$SERVICE"
hrm_mb_after=$(du -m "$HRM" | cut -f1)
echo "[witness-prune $TS] done — hrm now ${hrm_mb_after} MB"

# Retention: keep only the 3 most recent pre-prune backups (they can be huge).
ls -1t "${HRM}".pre-prune-* 2>/dev/null | tail -n +4 | xargs -r rm -f
