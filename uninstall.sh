#!/usr/bin/env bash
# uninstall.sh — remove Drowned God Agent-Brick systemd units
set -euo pipefail

if [[ $EUID -ne 0 ]]; then
  echo "must run as root: sudo $0"
  exit 1
fi

CURRENT=$(systemctl get-default)
if [[ "$CURRENT" == "agent-brick.target" ]]; then
  echo "[*] resetting default target to graphical.target"
  systemctl set-default graphical.target
fi

echo "[*] disabling services"
systemctl disable --now \
  llama-qwen-8b.service \
  llama-qwen36-27b.service \
  openclaw-gateway.service \
  drowned-god-tui.service \
  2>/dev/null || true

echo "[*] unmasking getty@tty1.service"
systemctl unmask getty@tty1.service 2>/dev/null || true

echo "[*] removing units"
rm -f \
  /etc/systemd/system/agent-brick.target \
  /etc/systemd/system/llama-qwen-8b.service \
  /etc/systemd/system/llama-qwen36-27b.service \
  /etc/systemd/system/openclaw-gateway.service \
  /etc/systemd/system/drowned-god-tui.service \
  /usr/local/bin/dg-mode

systemctl daemon-reload
echo "DONE"
