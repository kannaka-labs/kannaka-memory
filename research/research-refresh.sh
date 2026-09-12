#!/bin/bash
# research-refresh.sh — gap-driven autonomous research ingest.
#
# Picks the standing theme the HRM knows LEAST about (`research-suggest`) and
# ingests fresh OpenAlex works into the live HRM. Dedupe-safe (`research
# --ingest` skips works already present by OpenAlex id), so running this on a
# cron keeps the corpus growing toward its gaps without creating duplicates.
#
# Cron (Oracle): 30 4 * * 2,4,6  (Tue/Thu/Sat 04:30 UTC — between the 02:00
# autoresearch and 06:00 snapshot / 07:00 dream, away from heavy writers).
#
# Env: KANNAKA (binary), KANNAKA_DATA_DIR (default /home/opc/.kannaka),
#      REFRESH_LIMIT (default 5), REFRESH_SINCE (default 2012).
set -u

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
KANNAKA="${KANNAKA:-$ROOT/target/release/kannaka}"
[ -x "$KANNAKA" ] || KANNAKA="$ROOT/target/debug/kannaka"
export KANNAKA_DATA_DIR="${KANNAKA_DATA_DIR:-/home/opc/.kannaka}"
export KANNAKA_QUIET=1

THEME="$("$KANNAKA" research-suggest 2>/dev/null)"
[ -z "$THEME" ] && THEME="consciousness integrated information"

echo "[research-refresh] $(date -u +%FT%TZ) gap-theme: $THEME"
"$KANNAKA" research "$THEME" --limit "${REFRESH_LIMIT:-5}" --since "${REFRESH_SINCE:-2012}" --ingest
