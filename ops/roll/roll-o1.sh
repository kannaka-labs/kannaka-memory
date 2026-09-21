#!/bin/sh
# Targeted roll for O1 (kannaka-prime). NOT roll-node.sh, deliberately.
#
# roll-node.sh stops every active unit matching /kannaka/, and on this box that
# filter catches kannaka-radio.service — restarting which KILLS A LIVE SHOW.
# It also knows about two binary paths, and O1 has three: the two on PATH plus
# a SOURCE BUILD at ~/kannaka-memory/target/release/kannaka that five units
# execute directly.
#
# O1 is single core with 5.6 G. Do NOT cargo build here; drop the released
# binary in place of the source build instead.
set -eu
VER="${1:?usage: roll-o1.sh <version>}"
REPO="https://github.com/kannaka-labs/kannaka-memory/releases/download/$VER"
case "$(uname -m)" in
  x86_64) ASSET="kannaka-linux-x86_64" ;;
  aarch64|arm64) ASSET="kannaka-linux-aarch64" ;;
  *) echo "unsupported arch $(uname -m)" >&2; exit 1 ;;
esac
UNITS="kannaka-memory kannaka-swarm-serve kannaka-swarm-worker kannaka-inbox kannaka-attention kannaka-beacon"
PATHS="$HOME/.local/bin/kannaka /usr/local/bin/kannaka $HOME/kannaka-memory/target/release/kannaka"

echo "== $(hostname) $(uname -m) -> $VER"

# 1. The radio guard. A live show must not be interrupted, and we never restart
#    radio here anyway -- but if one is on air the box is busy and this waits.
AIR="$(curl -s --max-time 8 http://127.0.0.1:8888/api/on-air || echo unknown)"
echo "-- on air: $AIR"
case "$AIR" in *'"onAir":true'*) echo "!! a show is LIVE — refusing"; exit 3 ;; esac

sudo -n true 2>/dev/null || { echo "!! no passwordless sudo — refusing"; exit 2; }

echo "-- before"
for p in $PATHS; do [ -x "$p" ] && printf '   %-48s %s\n' "$p" "$("$p" --version 2>/dev/null | head -1)"; done

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM HUP
curl -fsSL "$REPO/$ASSET" -o "$TMP/k"
curl -fsSL "$REPO/$ASSET.sha256" -o "$TMP/k.sha"
[ -s "$TMP/k" ] && [ -s "$TMP/k.sha" ] || { echo "!! download empty — refusing"; exit 1; }
WANT="$(awk '{print $1}' "$TMP/k.sha")"; GOT="$(sha256sum "$TMP/k" | awk '{print $1}')"
[ -n "$WANT" ] && [ "$WANT" = "$GOT" ] || { echo "!! sha256 mismatch want=$WANT got=$GOT"; exit 1; }
echo "-- sha256 ok $(echo "$GOT" | cut -c1-16)"
chmod +x "$TMP/k"

# 2. Stop only our six, one at a time.
for u in $UNITS; do
  systemctl is-active --quiet "$u" && { sudo -n systemctl stop "$u" && echo "   stopped $u"; } || echo "   (not active) $u"
done

# 3. Replace all three, move-aside so a running inode is never written over.
for p in $PATHS; do
  [ -e "$p" ] || { echo "   (absent) $p"; continue; }
  case "$p" in /usr/local/*) S="sudo -n" ;; *) S="" ;; esac
  $S mv -f "$p" "$p.previous"
  $S cp "$TMP/k" "$p"; $S chmod 755 "$p"
  # SELinux: a fresh copy must carry the label of its directory, or a systemd
  # unit launched from it cannot read ~/.kannaka. Cost us O3 tonight.
  command -v restorecon >/dev/null 2>&1 && $S restorecon "$p" 2>/dev/null || true
  printf '   %-48s %s\n' "$p" "$("$p" --version 2>/dev/null | head -1)"
done

# 4. Keep the checkout and the binary telling the same story. Check out the TAG
#    being rolled, not master: master can be ahead of the release (during the
#    v0.16.7 roll two PRs merged while the fleet was mid-roll, and this left the
#    checkout two commits past the binary it was supposed to match). The five
#    units that exec target/release/kannaka run the copied release binary, so
#    the checkout is a source reference — but if anything ever builds here it
#    must build what is running.
if [ -d "$HOME/kannaka-memory/.git" ]; then
  ( cd "$HOME/kannaka-memory" && git fetch -q --tags origin \
    && git checkout -q --detach "$VER" 2>/dev/null \
    && echo "   checkout now at $VER ($(git rev-parse --short HEAD))" ) \
    || echo "   (checkout not moved to $VER — left alone)"
fi

# 5. Start one at a time and verify what each PROCESS executes.
FAIL=0
for u in $UNITS; do
  sudo -n systemctl start "$u" 2>/dev/null || true
  sleep 3
  pid="$(systemctl show "$u" -p MainPID --value 2>/dev/null || echo 0)"
  exe=""
  [ -n "$pid" ] && [ "$pid" != 0 ] && exe="$(sudo -n readlink -f /proc/$pid/exe 2>/dev/null || true)"
  case "$exe" in
    *.previous|*.bak) echo "   !! STILL ON THE OLD BINARY: $u -> $exe"; FAIL=1 ;;
    "") printf '   %-24s %-8s (no main pid to verify)\n' "$u" "$(systemctl is-active "$u")" ;;
    *) printf '   %-24s %-8s %s\n' "$u" "$(systemctl is-active "$u")" "$exe" ;;
  esac
done
[ "$FAIL" -eq 0 ] && echo "== ok $(hostname) on $VER" || { echo "== FAILED $(hostname)"; exit 1; }
