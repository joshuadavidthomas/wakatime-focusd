# wakatime-focusd

A systemd user daemon that tracks focused desktop applications and sends heartbeats to [WakaTime](https://wakatime.com).

## Features

The daemon monitors your desktop's focused window and sends heartbeats to WakaTime whenever focus changes or after a configurable timeout (default: 2 minutes). Window class names become tracked entity, allowing WakaTime to show which applications you spend time in.

Heartbeats are gated by systemd-logind's `IdleHint`, so no activity is recorded when your session is idle or locked. The daemon runs as a systemd user service with automatic restart on failure.

Currently supports Hyprland via its IPC socket. Additional backends are planned.

## Requirements

- [wakatime-cli](https://wakatime.com/terminal) installed and configured with API key
- systemd (for user service and idle detection)
- A supported window manager (see [Backend Setup](#backend-setup))
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

### Backend Setup

#### Hyprland

The Hyprland backend requires environment variables to be available to systemd user service. Add this to your Hyprland config (`~/.config/hypr/hyprland.conf`):

```
exec-once = dbus-update-activation-environment --systemd WAYLAND_DISPLAY XDG_CURRENT_DESKTOP HYPRLAND_INSTANCE_SIGNATURE XDG_RUNTIME_DIR
```

Without this, the service will fail to start because it cannot locate the Hyprland IPC socket.

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

### Running as a service (recommended)

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

### Running interactively

```bash
# Normal mode
wakatime-focusd

# Debug mode with event printing
wakatime-focusd --dry-run --print-events --log-level debug

# Oneshot mode (capture a few events and exit)
wakatime-focusd --oneshot --print-events
```

### CLI Options

```
Options:
  -c, --config <CONFIG>          Path to config file
      --dry-run                  Enable dry-run mode (don't send heartbeats)
      --log-level <LOG_LEVEL>    Log level [default: info]
      --print-events             Print normalized focus events to stdout
      --oneshot                  Run in oneshot mode: capture events then exit
      --oneshot-count <COUNT>    Number of events to capture [default: 5]
  -h, --help                     Print help
  -V, --version                  Print version
```

## Troubleshooting

### Service fails to start

1. Check if Hyprland backend environment is available:
   ```bash
   systemctl --user show-environment | grep HYPRLAND
   ```

2. If not, add to your Hyprland config:
   ```
   exec-once = dbus-update-activation-environment --systemd HYPRLAND_INSTANCE_SIGNATURE XDG_RUNTIME_DIR
   ```

3. Restart Hyprland or manually run the command.

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

## Architecture

```
src/
├── main.rs              # CLI parsing, logging, event loop
├── config.rs            # TOML configuration loading
├── backend/
│   ├── mod.rs           # FocusSource trait and FocusEvent model
│   └── hyprland.rs     # Hyprland socket2 backend
├── domain.rs            # Entity, Category, Heartbeat types
├── heartbeat.rs         # Heartbeat building logic
├── idle.rs             # Idle detection interface (systemd-logind DBus backend)
├── throttle.rs          # Heartbeat throttle state machine
└── wakatime.rs         # wakatime-cli invocation
```

## Roadmap

- [ ] Additional backends (wlr-foreign-toplevel, X11, KDE, GNOME)
- [ ] Configurable allowlist/denylist patterns (regex)
- [ ] Metrics/stats endpoint

## License

MIT
