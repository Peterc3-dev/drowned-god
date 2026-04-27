# Drowned God Agent-Brick

Headless agent OS profile + Alienware-mothership cockpit TUI for raz-gpd4.

Strip the laptop down to bare-bones, let the local LLM have all the resources,
drive everything from a phone via Telegram/web while the rig itself runs a
phosphor-green submersible cockpit on tty1.

## What's here

```
drowned-god/
  systemd/
    agent-brick.target         # custom isolation target (replaces graphical.target)
    llama-qwen-8b.service      # default: Qwen3-8B + Qwen3-1.7B spec-dec on :8080
    llama-qwen36-27b.service   # opt-in: Qwen3.6-27B (no spec dec — SSM wall)
    openclaw-gateway.service   # Telegram + tools + local-llm provider
    drowned-god-tui.service    # cockpit TUI on tty1
  scripts/
    dg-mode                    # switch helper: brick | dev | status
  tui/                         # Rust+ratatui cockpit
    src/main.rs
    src/palette.rs             # phosphor sub-aqua palette
    src/corners.rs             # rotating ASCII corner ornaments
    src/telemetry.rs           # llama / FLM / sysinfo / VRAM polling
  install.sh                   # sudo: install systemd units + dg-mode
  uninstall.sh                 # sudo: revert
```

## Install

```bash
# build the cockpit binary
cd ~/projects/drowned-god/tui && cargo build --release

# install the systemd units + dg-mode helper
sudo ~/projects/drowned-god/install.sh
```

## Usage

```bash
dg-mode status     # see current mode + service health
dg-mode brick      # isolate to agent-brick (kills GUI session — confirms first)
dg-mode dev        # back to graphical.target
```

To boot directly into brick mode every time:
```bash
sudo systemctl set-default agent-brick.target
```
To revert:
```bash
sudo systemctl set-default graphical.target
```

## What you gain in brick mode

- ~3 GB of system RAM freed (KDE+Plasma+browsers→0)
- iGPU driver still loaded; `llama-server` runs unchanged
- Network up; SSH up; Tailscale up
- Telegram bot stays online via openclaw → @QwenLo_Bot
- Direct cockpit on tty1; remote access via SSH or Tailscale

## What you lose

- No GUI on the device (intentional)
- All debugging via SSH or the cockpit TUI
- Browser-based dev tools require a remote browser pointed at this rig

## Web access from anywhere

The rig's `:8080` (llama-server, OpenAI-compatible) is reachable on:
- LAN: `http://raz-gpd4:8080`
- Tailscale: `http://100.77.212.27:8080`
- Funnel (public): `tailscale funnel 8080` then the funnel URL

Drive it from a phone browser, OpenAI client, or the existing Telegram bot.

## Cockpit controls (TUI)

- `↑ ↓` — move model selector
- `enter` — send chat
- `esc` / `q` — exit
- More controls land as panes wire up (TODO: tool-call streaming, memory recall, model switch)

## Troubleshooting

- TUI crashes: `journalctl -u drowned-god-tui -e`
- llama-server: `journalctl -u llama-qwen-8b -f`
- openclaw: `journalctl -u openclaw-gateway -f`

## Aesthetic notes

Phosphor green on abyss, sonar cyan accents, amber warnings, salt-rust idle.
Rotating corner anchors at 4 fps (Sierra/LucasArts cockpit feel). Sonar pulse
in the footer. All gauges live-update from llama-server `/v1/models` plus
`rocm-smi` for VRAM and `sysinfo` for everything else.
