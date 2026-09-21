#!/bin/bash
# roll-node.sh vX.Y.Z [--stage-only]
#
# Roll one fleet node to a released kannaka binary. Reconstructed from the
# 0.16.3 / 0.16.4 playbook (the original lived in a session scratchpad):
#   - download to a temp file; sha256 against the published sidecar, BEFORE
#     replacing anything, after asserting both are non-empty (a check against
#     an empty sidecar passes vacuously);
#   - refuse up front (exit 2) when kannaka units are active and sudo needs a
#     password — the 0.16.3 groundwave roll "succeeded" on disk while the
#     service kept executing kannaka.previous;
#   - move-aside, never write-over (a busy binary must be mv'd);
#   - restart each unit that execs the binary, then verify /proc/<MainPID>/exe
#     and FAIL on *.previous / *.bak — a version on disk is not a version running;
#   - --stage-only replaces the binary and prints the restarts owed, for boxes
#     where systemd will re-exec on its own (Restart=always + ExecStart by path)
#     after a plain `kill <MainPID>` as the unit's user.
set -u
VER="${1:?usage: roll-node.sh vX.Y.Z [--stage-only]}"; STAGE_ONLY="${2:-}"
ARCH=$(uname -m); case "$ARCH" in aarch64) A=linux-aarch64;; x86_64) A=linux-x86_64;; *) echo "unsupported arch $ARCH"; exit 1;; esac
BASE="https://github.com/kannaka-labs/kannaka-memory/releases/download/$VER"
say() { echo "[$(hostname) $(date -u +%H:%M:%S)] $*"; }
die() { say "ABORT: $*"; exit 1; }

# every installed kannaka binary this node runs from
BINS=(); for b in /usr/local/bin/kannaka "$HOME/.local/bin/kannaka"; do [ -x "$b" ] && BINS+=("$b"); done
[ ${#BINS[@]} -gt 0 ] || die "no kannaka binary installed here"
# units whose MainPID execs one of them (not a description match — the 0.16.3 awk filter caught grid-* on skywave)
UNITS=(); for u in $(systemctl list-units --type=service --state=running --no-legend 2>/dev/null | awk '{print $1}'); do
  pid=$(systemctl show -p MainPID --value "$u" 2>/dev/null); exe=$(readlink "/proc/$pid/exe" 2>/dev/null | sed 's/ (deleted)$//')
  for b in "${BINS[@]}"; do [ "$exe" = "$b" ] && UNITS+=("$u"); done
done
say "binaries: ${BINS[*]}"; say "units on them: ${UNITS[*]:-none}"

# An EMPTY unit set is not success. The filter above only sees RUNNING units, so
# a node whose services are stopped looks identical to one that has none — and
# the verdict at the end is `FAIL=0`, trivially true over zero units. On the
# v0.16.9 roll O3 printed "ROLLED" having restarted nothing, because its units
# had been stopped minutes earlier for store maintenance. Say which case it is.
INACTIVE=()
if [ ${#UNITS[@]} -eq 0 ]; then
  ALL=$(systemctl list-units --type=service --all --no-legend 2>/dev/null | awk '{print $1}' | grep -iE "kannaka|grid-mind|gossipghost")
  for u in $ALL; do
    st=$(systemctl is-active "$u" 2>/dev/null)
    [ "$st" != "active" ] && INACTIVE+=("$u:$st")
  done
fi

SUDO=""; if sudo -n true 2>/dev/null; then SUDO="sudo"; fi
if [ -z "$SUDO" ] && [ ${#UNITS[@]} -gt 0 ] && [ "$STAGE_ONLY" != "--stage-only" ]; then
  say "sudo needs a password and ${#UNITS[@]} unit(s) are active — refusing to roll blind (exit 2)."
  say "run with --stage-only, then 'kill <MainPID>' as the unit's user for: ${UNITS[*]}"; exit 2
fi
for b in "${BINS[@]}"; do [ -w "$b" ] || [ -n "$SUDO" ] || die "cannot write $b without sudo"; done

say "== download + verify $VER $A =="
T=$(mktemp -d); curl -fsSL --retry 3 -o "$T/kannaka" "$BASE/kannaka-$A" || die "download failed"
curl -fsSL --retry 3 -o "$T/kannaka.sha256" "$BASE/kannaka-$A.sha256" || die "sidecar download failed"
[ -s "$T/kannaka" ] && [ -s "$T/kannaka.sha256" ] || die "empty download or empty sidecar — refusing a vacuous check"
WANT=$(awk '{print $1}' "$T/kannaka.sha256"); HAVE=$(sha256sum "$T/kannaka" | awk '{print $1}')
[ "$WANT" = "$HAVE" ] || die "sha256 mismatch: sidecar $WANT vs download $HAVE"
chmod +x "$T/kannaka"; NEWV=$("$T/kannaka" --version 2>/dev/null | head -1)
echo "$NEWV" | grep -q "${VER#v}" || die "downloaded binary reports '$NEWV', expected ${VER#v}"
say "verified: $NEWV sha $HAVE"

say "== replace (move-aside, never write-over) =="
for b in "${BINS[@]}"; do
  # capture fully, THEN take a line: `--version | head -1` closes the pipe under
  # the binary and it dies with a Rust "Broken pipe" panic (cosmetic, but it
  # littered the O3 roll log and is the cmd|head EPIPE trap in memory)
  OLDV=$("$b" --version 2>/dev/null); OLD=$(printf '%s\n' "$OLDV" | sed -n '1p' | cut -d' ' -f2)
  $SUDO mv -f "$b" "$b.previous" && $SUDO cp "$T/kannaka" "$b" && $SUDO chmod 755 "$b" && $SUDO chown "$(stat -c %U:%G "$b.previous")" "$b" 2>/dev/null
  command -v restorecon >/dev/null && $SUDO restorecon "$b" 2>/dev/null
  NEWV=$("$b" --version 2>/dev/null); say "  $b: $OLD -> $(printf '%s
' "$NEWV" | sed -n '1p' | cut -d' ' -f2)"
done

if [ "$STAGE_ONLY" = "--stage-only" ]; then
  say "== staged only; restarts owed: =="
  for u in "${UNITS[@]}"; do say "  $u MainPID=$(systemctl show -p MainPID --value "$u")  (kill it as its user; Restart=always re-execs on the new binary)"; done
  exit 0
fi

say "== restart units one at a time, verify the PROCESS =="
FAIL=0
for u in "${UNITS[@]}"; do
  $SUDO systemctl restart "$u"; sleep 6
  pid=$(systemctl show -p MainPID --value "$u"); exe=$(readlink "/proc/$pid/exe" 2>/dev/null)
  st=$(systemctl is-active "$u")
  case "$exe" in *.previous*|*.bak*|"") say "  !! $u: $st exe=${exe:-none} — STILL ON THE OLD BINARY"; FAIL=$((FAIL+1));;
    *) say "  ok $u: $st exe=$exe";; esac
done
say "== on disk: =="; for b in "${BINS[@]}"; do V=$("$b" --version 2>/dev/null); say "  $b $(printf '%s
' "$V" | sed -n '1p')"; done
if [ "$FAIL" != "0" ]; then
  die "$FAIL unit(s) not on the new binary"
fi
if [ ${#UNITS[@]} -eq 0 ]; then
  if [ ${#INACTIVE[@]} -gt 0 ]; then
    say "!! binary replaced, but NOTHING WAS RESTARTED: no kannaka unit is running here."
    say "!! these exist and are not active: ${INACTIVE[*]}"
    say "!! they will come up on $VER when started — but this node is NOT serving now."
    say "== STAGED ONLY (no running units) $VER =="
    exit 3
  fi
  say "== binary replaced; this node runs no kannaka units =="
  exit 0
fi
say "== ROLLED $VER =="
