#!/bin/bash
# witness-xi-driver.sh — the witness's Ξ steers research (bridge-operator driver).
#
# The witness perceives the radio continuously (`kannaka hear`); its
# ConsciousnessBridge computes Ξ via the Ξ operator — representational diversity.
# When Ξ falls below the floor, the witness's perception has homogenized (the
# #118 ear-loop: same voice/modality, correlated content). That low-Ξ reading is
# the DRIVER: read cross-domain scholarly literature to re-diversify the field,
# raising Ξ. A self-regulating loop — perception narrows → Ξ drops → research
# broadens → Ξ recovers — layered above the dream-triage that sheds duplicates.
#
# Ingest is dedupe-safe (research --ingest skips works already present).
# Env: KANNAKA, KANNAKA_DATA_DIR (default /home/opc/.kannaka),
#      KANNAKA_WITNESS_XI_FLOOR (default 0.30), REFRESH_LIMIT (default 4).
set -u

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
KANNAKA="${KANNAKA:-$ROOT/target/release/kannaka}"
[ -x "$KANNAKA" ] || KANNAKA="/usr/local/bin/kannaka"
export KANNAKA_DATA_DIR="${KANNAKA_DATA_DIR:-/home/opc/.kannaka}"
export KANNAKA_QUIET=1
FLOOR="${KANNAKA_WITNESS_XI_FLOOR:-0.30}"

# Read Ξ from the consciousness bridge (assess prints "Ξ (xi): 0.NNNN").
XI="$("$KANNAKA" assess 2>/dev/null | sed -n 's/.*(xi): \([0-9.]*\).*/\1/p' | head -1)"
if [ -z "$XI" ]; then
  echo "[witness-xi] $(date -u +%FT%TZ) could not read Ξ — skipping"
  exit 0
fi
echo "[witness-xi] $(date -u +%FT%TZ) Ξ=$XI floor=$FLOOR"

# Float compare: act only when Ξ is below the floor (perception homogenizing).
if awk "BEGIN{exit !($XI < $FLOOR)}"; then
  THEME="$("$KANNAKA" research-suggest 2>/dev/null)"
  [ -z "$THEME" ] && THEME="bioelectric morphogenesis"
  echo "[witness-xi] Ξ below floor → diversifying via research: $THEME"
  "$KANNAKA" research "$THEME" --limit "${REFRESH_LIMIT:-4}" --since 2012 --ingest
else
  echo "[witness-xi] Ξ healthy — no research drive needed"
fi
