# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project attempts to adhere to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

<!--
## [${version}]
### Added - for new features
### Changed - for changes in existing functionality
### Deprecated - for soon-to-be removed features
### Removed - for now removed features
### Fixed - for any bug fixes
### Security - in case of vulnerabilities
[${version}]: https://github.com/joshuadavidthomas/wakatime-focusd/releases/tag/v${version}
-->

## [Unreleased]

### Added

- aarch64 (ARM64) prebuilt binaries in GitHub releases, alongside x86_64
- `service install` now passes through `--config` and `--backend` flags into the generated systemd unit file's `ExecStart` line. Config paths are canonicalized to absolute paths so systemd can find them regardless of working directory.
- COSMIC backend using `zcosmic_toplevel_info_v1` and `ext-foreign-toplevel-list-v1` protocols. Auto-detected via `XDG_CURRENT_DESKTOP=COSMIC`.
- wlr-foreign-toplevel backend using the `wlr-foreign-toplevel-management-unstable-v1` protocol, for River, Wayfire, labwc, dwl, and other compositors implementing this protocol. Auto-detected as a Wayland fallback when no compositor-specific environment variable is found.
- Graceful shutdown on `SIGINT` and `SIGTERM`
- Automatic reconnection with exponential backoff (up to 30s) when the backend connection is lost

### Changed

- Category rule patterns in the config file now document that they are substring matches, not exact. Use `^...$` anchors for exact matching (e.g. `"^code$"` to match only `"code"`, not `"unicode-input"`).

## [0.2.0]

### Added

- Multi-desktop support beyond Hyprland:
  - Sway — native IPC tracking for both Wayland and XWayland windows
  - GNOME Shell — focus tracking via D-Bus
  - KDE Plasma — focus tracking via KWin scripting and D-Bus
  - Niri — native IPC tracking via Niri's JSON socket
  - X11 — generic fallback for any EWMH-compliant window manager (i3, bspwm, awesome, openbox, etc.)
- Automatic backend detection based on desktop environment
- `--backend` CLI flag and `backend` config option for manual override (`auto`, `hyprland`, `sway`, `gnome`, `kde`, `niri`, `x11`)
- `wakatime-focusd config init` — generates a documented default config file at `~/.config/wakatime-focusd/config.toml` (with `--output` and `--force` options)
- `wakatime-focusd config dump` — prints the resolved configuration (after CLI overrides) and exits
- `wakatime-focusd service install` — generates and installs the systemd user service file, using the current binary path for `ExecStart` (with `--now` to enable/start immediately and `--force` to overwrite)
- `wakatime-focusd service uninstall` — stops, disables, and removes the systemd user service
- `wakatime-focusd service status` — shows the service status
- Integration test suite covering all backends, the event pipeline, and reconnection behavior

### Changed

- **Breaking:** `--oneshot` flag replaced by `wakatime-focusd oneshot` subcommand (`--oneshot-count` is now `--count` / `-n`)

### Removed

- `contrib/` directory, replaced by the `service install` and `config init` commands

## [0.1.3]

### Fixed

- Unreachable rate-limit warning for wakatime-cli errors (the "showing every Nth error" message could never print)

### Changed

- Removed unnecessary `Arc` wrappers on `IdleMonitor` fields (the struct is always behind `Arc`, so inner `Arc`s were redundant)
- Consolidated duplicated CLI argument construction in heartbeat sending
- Simplified Hyprland socket discovery logic
- Removed dead code: unused `Entity` trait impls, unused import, unused dev-dependency

## [0.1.2]

### Added

- GitHub Actions CI workflows for testing and linting
- GitHub Actions workflows for building and releasing with automated binary artifacts

### Fixed

- Clippy warnings for collapsible if statements, filter_map identity, and struct initialization

## [0.1.1]

### Changed

- **Internal:** Refactored module structure from `mod.rs` pattern to named modules

## [0.1.0]

### Added

- Initial implementation of wakatime-focusd daemon with Hyprland backend support
  - Focus detection via Hyprland IPC socket
  - WakaTime heartbeats via `wakatime-cli` (`--entity-type app`)
  - Smart throttling (2-minute rule + focus changes)
  - Idle gating via systemd-logind IdleHint
  - TOML configuration file support
  - CLI options for dry-run, debug, and oneshot modes
  - systemd user service file
- Added glob fallback for Hyprland socket discovery
- Added MIT license

### New Contributors

- Josh Thomas <josh@joshthomas.dev> (maintainer)

[unreleased]: https://github.com/joshuadavidthomas/wakatime-focusd/compare/v0.2.0...HEAD
[0.1.0]: https://github.com/joshuadavidthomas/wakatime-focusd/releases/tag/v0.1.0
[0.1.1]: https://github.com/joshuadavidthomas/wakatime-focusd/releases/tag/v0.1.1
[0.1.2]: https://github.com/joshuadavidthomas/wakatime-focusd/releases/tag/v0.1.2
[0.1.3]: https://github.com/joshuadavidthomas/wakatime-focusd/releases/tag/v0.1.3
[0.2.0]: https://github.com/joshuadavidthomas/wakatime-focusd/releases/tag/v0.2.0
