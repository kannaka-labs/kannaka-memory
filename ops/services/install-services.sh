#!/usr/bin/env bash
# install-services.sh — wire the binary-provided daemons as systemd units.
#
# Tier 1 (the kannaka binary) must already be installed (install.sh). This sets
# up Tier 2 daemons. Today it installs the attention beam; extend the SERVICES
# list as more units are version-controlled here.
#
#   bash ops/services/install-services.sh            # install + enable
#   bash ops/services/install-services.sh --eye      # also clone+enable kannaka-eye
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
KANNAKA_BIN="${KANNAKA_BIN:-$HOME/.local/bin/kannaka}"

if [ ! -x "$KANNAKA_BIN" ]; then
  echo "kannaka binary not found at $KANNAKA_BIN — run install.sh first." >&2
  exit 1
fi

SERVICES=(kannaka-attention.service)

for svc in "${SERVICES[@]}"; do
  echo "→ installing $svc"
  sudo install -m644 "$HERE/$svc" "/etc/systemd/system/$svc"
done
sudo systemctl daemon-reload
for svc in "${SERVICES[@]}"; do
  sudo systemctl enable --now "$svc"
  echo "  $svc: $(systemctl is-active "$svc")"
done

if [ "${1:-}" = "--eye" ]; then
  echo "→ kannaka-eye (sibling Node service — the glyph producer)"
  [ -d "$HOME/kannaka-eye" ] || git clone -q https://github.com/kannaka-labs/kannaka-eye.git "$HOME/kannaka-eye"
  ( cd "$HOME/kannaka-eye" && git pull -q )
  bash "$HOME/kannaka-eye/ops/run-eye.sh" >/dev/null 2>&1 &  # quick smoke; real run is the unit
  sudo install -m644 "$HOME/kannaka-eye/ops/kannaka-eye.service" /etc/systemd/system/kannaka-eye.service
  sudo systemctl daemon-reload && sudo systemctl enable --now kannaka-eye
  echo "  kannaka-eye: $(systemctl is-active kannaka-eye)"
  echo "  add the feeder cron: * * * * * \$HOME/kannaka-eye/ops/eye-feeder.sh"
fi

echo "✓ services installed"
