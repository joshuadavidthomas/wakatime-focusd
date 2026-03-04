# wakatime-focusd

A systemd user daemon that tracks focused desktop applications and sends heartbeats to [WakaTime](https://wakatime.com).

## Supported Desktops

wakatime-focusd auto-detects your desktop environment. No manual configuration is needed in most cases.

| Desktop | How it works |
|---------|-------------|
| **Hyprland** | IPC socket events |
| **Sway** | IPC socket events (Wayland + XWayland windows) |
| **Niri** | JSON IPC socket events |
| **GNOME Shell** | D-Bus (`org.gnome.Shell.Introspect`) |
| **KDE Plasma** | KWin script + D-Bus |
| **X11** (any WM) | `_NET_ACTIVE_WINDOW` — works with i3, bspwm, awesome, openbox, etc. |

Auto-detection checks Wayland-native compositors first, then falls back to X11.

## How It Works

The daemon monitors your desktop's focused window and sends heartbeats to WakaTime whenever focus changes or after a configurable timeout (default: 2 minutes). Window class names become the tracked entity, allowing WakaTime to show which applications you spend time in.

Heartbeats are gated by systemd-logind's `IdleHint`, so no activity is recorded when your session is idle or locked. The daemon runs as a systemd user service with automatic restart on failure.

## Requirements

- [wakatime-cli](https://wakatime.com/terminal) installed and configured with your API key
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

> **Important:** Your desktop environment's variables must be visible to systemd user services. Most Wayland compositors and display managers handle this, but if the service can't detect your backend, add this to your compositor's startup:
>
> ```bash
> dbus-update-activation-environment --systemd \
>   WAYLAND_DISPLAY XDG_CURRENT_DESKTOP XDG_RUNTIME_DIR \
>   DISPLAY SWAYSOCK HYPRLAND_INSTANCE_SIGNATURE NIRI_SOCKET
> ```
>
> Only the variables relevant to your setup matter — you don't need all of them.

## Configuration

Generate a default config file:

```bash
wakatime-focusd init
```

This writes a fully documented config to `~/.config/wakatime-focusd/config.toml`. To write to a different location, use `--output <path>`.

The config file is optional — the daemon works out of the box with sensible defaults. Here's what you can customize:

```toml
# Backend for focus detection (default: "auto")
# Options: auto, hyprland, sway, gnome, kde, niri, x11
# "auto" detects your desktop environment automatically.
backend = "auto"

# Heartbeat interval in seconds (default: 120)
heartbeat_interval_seconds = 120

# Minimum seconds before resending for same entity (default: 120)
min_entity_resend_seconds = 120

# Whether to include window titles in tracking (default: false)
# WARNING: Titles may contain sensitive information (file paths, URLs, etc.)
track_titles = false

# How to handle titles when track_titles is true (default: "ignore")
# Options: "ignore" | "append"
# "append" creates entities like "Class — Title" (high cardinality warning)
title_strategy = "ignore"

# Default category for heartbeats when no rule matches (default: "coding")
# Valid options: coding, building, indexing, debugging, browsing, running tests,
# writing tests, manual testing, writing docs, code reviewing, communicating,
# notes, researching, learning, designing, ai coding
# See: https://wakatime.com/developers#heartbeats
default_category = "coding"

# Category rules - first match wins (case-insensitive regex)
# [[category_rules]]
# pattern = "firefox|chromium|brave|zen-browser"
# category = "browsing"
#
# [[category_rules]]
# pattern = "thunderbird|evolution|geary"
# category = "communicating"
#
# [[category_rules]]
# pattern = "slack|discord|element"
# category = "communicating"
#
# [[category_rules]]
# pattern = "figma|inkscape|gimp"
# category = "designing"

# Optional: Only track these app classes (empty = track all)
# app_allowlist = ["code", "codium", "nvim", "vim", "emacs"]

# Optional: Never track these app classes
# app_denylist = ["slack", "discord", "spotify"]

# Path to wakatime-cli binary (optional)
# If not set, searches PATH and ~/.wakatime/
# wakatime_cli_path = "/usr/bin/wakatime-cli"

# Path to wakatime config file (optional)
# Forwarded to wakatime-cli --config
# wakatime_config_path = "/home/user/.wakatime.cfg"

# Idle check interval in seconds (default: 10)
# How often to poll systemd-logind for idle state.
idle_check_interval_seconds = 10

# Dry run mode: log commands instead of executing (default: false)
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

### CLI Options

```
wakatime-focusd [OPTIONS] [COMMAND]

Commands:
  init                       Create a default config file with documentation

Options:
  -c, --config <PATH>        Path to TOML config file
  -b, --backend <BACKEND>    Backend override [default: auto]
                              [values: auto, hyprland, sway, gnome, kde, niri, x11]
      --dump-config           Print the resolved configuration and exit
      --dry-run               Log commands instead of sending heartbeats
      --log-level <LEVEL>     Log level [default: info]
                              [values: trace, debug, info, warn, error]
      --print-events          Print focus events to stdout
      --oneshot               Capture N events then exit (for debugging)
      --oneshot-count <N>     Number of events in oneshot mode [default: 5]
```

Use `--dump-config` to see the effective configuration after applying CLI overrides:

```bash
wakatime-focusd --backend sway --dry-run --dump-config
```

## Troubleshooting

### Service fails to start

1. Check that your desktop's environment variables are visible to systemd:
   ```bash
   systemctl --user show-environment
   ```
   Look for the variable your backend needs (e.g., `SWAYSOCK` for Sway, `DISPLAY` for X11, `HYPRLAND_INSTANCE_SIGNATURE` for Hyprland).

2. If variables are missing, export them to systemd from your compositor startup. See the note in [Installation](#installing-the-systemd-service).

3. Test detection manually:
   ```bash
   wakatime-focusd --oneshot --log-level debug
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

1. Check if `dry_run` is enabled in your config
2. Check if the app is in `app_denylist` or not in `app_allowlist`
3. Check idle state: `loginctl show-session --property=IdleHint`
4. Check logs: `journalctl --user -u wakatime-focusd -f`

## License

wakatime-focusd is licensed under the MIT license. See the [`LICENSE`](LICENSE) file for more information.
