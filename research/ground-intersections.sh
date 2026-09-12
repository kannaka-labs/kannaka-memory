#!/bin/bash
# ─────────────────────────────────────────────────────────────────────────────
# ground-intersections.sh — anchor the research/intersections program in REAL
# scholarly literature (OpenAlex) and test the card-04 clustering hypothesis.
#
# The intersections README says it plainly: "the work moves better when anchored
# in real signal", and card 04 predicts that absorbing cross-domain literature
# should DIVERSIFY the HRM's cluster topology. This harness operationalises both:
# for each standing intersection it pulls ranked OpenAlex works and ingests them
# as Semantic memories, measuring cluster/Φ/Ξ before and after so card 04 gets a
# real, reproducible test instead of a synthetic one.
#
# Usage:
#   research/ground-intersections.sh [--limit N] [--since YEAR] [--dry-run]
#
# Defaults to an ISOLATED data dir (KANNAKA_DATA_DIR=$PWD/.ground-intersections)
# so it never contends with a live single-writer constellation HRM. Point
# KANNAKA_DATA_DIR elsewhere to ground a specific store (ensure no writer is live).
# ─────────────────────────────────────────────────────────────────────────────
set -u

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
KANNAKA="${KANNAKA:-$ROOT/target/release/kannaka}"
[ -x "$KANNAKA" ] || KANNAKA="$ROOT/target/debug/kannaka"
export KANNAKA_DATA_DIR="${KANNAKA_DATA_DIR:-$ROOT/.ground-intersections}"
export KANNAKA_QUIET=1

LIMIT=8
SINCE=2010
DRY=0
while [ $# -gt 0 ]; do
  case "$1" in
    --limit) LIMIT="$2"; shift 2 ;;
    --since) SINCE="$2"; shift 2 ;;
    --dry-run) DRY=1; shift ;;
    *) echo "ground-intersections: unknown arg: $1" >&2; shift ;;
  esac
done

# Theme → OpenAlex query. One representative query per standing intersection
# (README cards 01-05) plus a societal-contribution probe (the 2026-06-07 pivot
# toward broader societal exploration).
declare -a THEMES=(
  "01-cardiac|heart rate variability Kuramoto synchronization cardiorespiratory coupling"
  "02-cancer|cancer gap junction bioelectric decoupling oscillation coherence"
  "03-bioelectric|bioelectric pattern memory morphogenesis Levin voltage gradient"
  "05-magic|magic state non-stabilizer resource quantum computation"
  "societal|collective intelligence emergence multi-agent cooperation"
  "ethics|artificial consciousness moral status machine sentience"
)

echo "=== ground-intersections — $(date -u +%FT%TZ) ==="
echo "bin=$KANNAKA  data_dir=$KANNAKA_DATA_DIR  limit=$LIMIT since=$SINCE dry_run=$DRY"

assess_line() { "$KANNAKA" assess 2>/dev/null | grep -iE "phi|xi|cluster|order" | tr '\n' ' '; echo; }

echo "--- BEFORE ---"
BEFORE="$(assess_line)"
echo "$BEFORE"

for entry in "${THEMES[@]}"; do
  tag="${entry%%|*}"; query="${entry#*|}"
  echo "--- [$tag] $query ---"
  if [ "$DRY" -eq 1 ]; then
    "$KANNAKA" research "$query" --limit "$LIMIT" --since "$SINCE" 2>&1 | grep -E '^\s+[0-9]+\.' | head -"$LIMIT"
  else
    "$KANNAKA" research "$query" --limit "$LIMIT" --since "$SINCE" --ingest 2>&1 | grep -E 'ingested|no works'
  fi
done

if [ "$DRY" -eq 1 ]; then
  echo "=== dry-run complete (nothing ingested) ==="
  exit 0
fi

# Cluster topology emerges during consolidation, not raw absorption — so the
# card-04 test only makes sense after a dream pass over the freshly-ingested
# cross-domain corpus.
echo "--- DREAM (consolidate the ingested corpus) ---"
"$KANNAKA" dream 2>&1 | grep -iE "strengthened|pruned|promoted|triage|cluster" | head

echo "--- AFTER ---"
AFTER="$(assess_line)"
echo "$AFTER"

echo "=== card-04 read: did cross-domain ingestion diversify cluster topology? ==="
echo "before: $BEFORE"
echo "after:  $AFTER"
echo "(compare num_clusters / Ξ — a rise supports the card-04 prediction)"
