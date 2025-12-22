# wakatime-focusd

A systemd user daemon that tracks focused desktop applications and sends heartbeats to [WakaTime](https://wakatime.com).

## Features

- **Hyprland IPC integration**: Detects focused windows via Hyprland's socket2 event stream (currently supported backend)
- **WakaTime heartbeats**: Sends `--entity-type app` heartbeats using `wakatime-cli`
- **Smart throttling**: Follows WakaTime's 2-minute rule - only sends when focus changes or after timeout
- **Idle detection**: Skips heartbeats when session is idle (via systemd-logind `IdleHint`)
- **systemd integration**: Runs as a user service with automatic restart

## Requirements

- [Hyprland](https://hyprland.org/) window manager (currently supported backend)
- [wakatime-cli](https://wakatime.com/terminal) installed and configured with API key
- systemd (for user service and idle detection)
- Rust toolchain (for building)

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

### Hyprland Backend Setup (Required)

The Hyprland backend requires Hyprland environment variables to be available to the systemd user service. Add this to your Hyprland config (`~/.config/hypr/hyprland.conf`):

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

## How It Works

1. **Focus Detection**: Currently connects to Hyprland's `.socket2.sock` and listens for `activewindow>>` events
2. **Event Parsing**: Extracts window class and title from events (handling commas in titles correctly)
3. **Throttling**: Only sends heartbeats on focus change or after 2 minutes for the same app
4. **Idle Gating**: Polls systemd-logind's `IdleHint` to skip heartbeats when idle/locked
5. **Heartbeat**: Invokes `wakatime-cli --entity-type app --entity <CLASS> --category <CAT>`

## Manual Testing

1. **Test focus detection**:
   ```bash
   wakatime-focusd --dry-run --print-events --log-level debug
   ```
   Switch windows and verify events are logged.

2. **Test throttling**:
   - Focus on one app, verify only 1 heartbeat
   - Wait 2+ minutes, verify second heartbeat
   - Switch apps, verify immediate heartbeat

3. **Test idle gating**:
   - Lock screen or trigger idle
   - Verify no heartbeats while `IdleHint=true`

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

## Future Plans

- [ ] Additional backends (wlr-foreign-toplevel, X11, KDE, GNOME)
- [ ] Configurable allowlist/denylist patterns (regex)
- [ ] Metrics/stats endpoint

## License

MIT
