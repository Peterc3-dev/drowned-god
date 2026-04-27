#!/usr/bin/env bash
# install.sh — install Drowned God Agent-Brick systemd units
# Run once with sudo. Reversible: see uninstall.sh
set -euo pipefail

if [[ $EUID -ne 0 ]]; then
  echo "must run as root: sudo $0"
  exit 1
fi

REPO=/home/raz/projects/drowned-god

echo "[*] installing systemd units to /etc/systemd/system/"
install -m 644 "$REPO/systemd/agent-brick.target"        /etc/systemd/system/
install -m 644 "$REPO/systemd/llama-qwen-8b.service"     /etc/systemd/system/
install -m 644 "$REPO/systemd/llama-qwen36-27b.service"  /etc/systemd/system/
install -m 644 "$REPO/systemd/openclaw-gateway.service"  /etc/systemd/system/
install -m 644 "$REPO/systemd/drowned-god-tui.service"   /etc/systemd/system/

echo "[*] installing dg-mode helper to /usr/local/bin/"
install -m 755 "$REPO/scripts/dg-mode" /usr/local/bin/dg-mode

echo "[*] masking getty@tty1.service so the cockpit owns tty1 cleanly"
systemctl mask getty@tty1.service

echo "[*] reloading systemd"
systemctl daemon-reload

echo "[*] enabling default agent-brick services (start at boot if you isolate the target)"
systemctl enable llama-qwen-8b.service openclaw-gateway.service drowned-god-tui.service

echo
echo "DONE. Try:"
echo "  dg-mode status     # check current mode"
echo "  dg-mode brick      # switch to brick mode (warns first; kills GUI)"
echo "  dg-mode dev        # switch back to graphical"
echo
echo "To boot DIRECTLY into brick mode every time:"
echo "  sudo systemctl set-default agent-brick.target"
echo
echo "To revert that:"
echo "  sudo systemctl set-default graphical.target"
