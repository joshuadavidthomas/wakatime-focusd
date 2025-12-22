# wakatime-focusd

A systemd user daemon that tracks focused desktop applications and sends heartbeats to [WakaTime](https://wakatime.com).

## Features

The daemon monitors your desktop's focused window and sends heartbeats to WakaTime whenever focus changes or after a configurable timeout (default: 2 minutes). Window class names become tracked entity, allowing WakaTime to show which applications you spend time in.

Heartbeats are gated by systemd-logind's `IdleHint`, so no activity is recorded when your session is idle or locked. The daemon runs as a systemd user service with automatic restart on failure.

Currently supports Hyprland via its IPC socket. Additional backends are planned.

## Requirements

- [wakatime-cli](https://wakatime.com/terminal) installed and configured with API key
- systemd (for user service and idle detection)
- Rust toolchain (for building from source)

## Installation

### Building from source

```bash
git clone https://github.com/joshuadavidthomas/wakatime-focusd
cd wakatime-focusd
cargo build --release
cp target/release/wakatime-focusd ~/.local/bin/
```

### Installing the systemd service

```bash
mkdir -p ~/.config/systemd/user
cp contrib/wakatime-focusd.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now wakatime-focusd.service
```

## Configuration

Create `~/.config/wakatime-focusd/config.toml`:

```toml
# Heartbeat interval in seconds (default: 120)
heartbeat_interval_seconds = 120

# Minimum seconds before resending for same entity (default: 120)
min_entity_resend_seconds = 120

# Include window titles (default: false, may contain sensitive info)
track_titles = false

# Title strategy: "ignore" or "append" (default: "ignore")
title_strategy = "ignore"

# Default category for heartbeats when no rule matches (default: "coding")
# Options: coding, browsing, designing, debugging, communicating, etc.
default_category = "coding"

# Only track these apps (optional, empty = track all)
# app_allowlist = ["code", "nvim", "emacs"]

# Never track these apps (optional)
# app_denylist = ["slack", "discord"]

# Path to wakatime-cli (optional, searches PATH by default)
# wakatime_cli_path = "/usr/bin/wakatime-cli"

# Idle check interval in seconds (default: 10)
idle_check_interval_seconds = 10

# Dry run mode (default: false)
dry_run = false
```

See `contrib/config.toml` for a fully documented example.

## Usage

```bash
# Start the service
systemctl --user start wakatime-focusd

# Check status
systemctl --user status wakatime-focusd

# View logs
journalctl --user -u wakatime-focusd -f

# Stop the service
systemctl --user stop wakatime-focusd
```

## Troubleshooting

### Service fails to start

1. Check if systemd environment has required variables:
   ```bash
   systemctl --user show-environment | grep XDG_RUNTIME_DIR
   ```

2. Verify Hyprland socket exists:
   ```bash
   ls -la $XDG_RUNTIME_DIR/hypr/*/.socket2.sock 2>/dev/null
   ```

3. If running multiple Hyprland instances, verify which instance is active:
   ```bash
   systemctl --user show-environment | grep HYPRLAND_INSTANCE_SIGNATURE
   ```

### wakatime-cli not found

Ensure `wakatime-cli` is installed:
```bash
# Check if it's in PATH
which wakatime-cli

# Or install it
pip install wakatime

# Or download from https://wakatime.com/terminal
```

Set `wakatime_cli_path` in config if it's in a non-standard location.

### No heartbeats being sent

1. Check if dry_run is enabled
2. Check if app is in denylist or not in allowlist
3. Check idle state: `loginctl show-session --property=IdleHint`
4. Check logs: `journalctl --user -u wakatime-focusd -f`

## Roadmap

- [ ] Additional backends (wlr-foreign-toplevel, X11, KDE, GNOME)
- [ ] Configurable allowlist/denylist patterns (regex)
- [ ] Metrics/stats endpoint

## License

MIT
