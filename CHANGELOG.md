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

- Support for multiple desktop environments via `--backend` flag and `backend` config option (`auto`, `hyprland`, `sway`, `gnome`, `kde`, `niri`, `x11`)
- Automatic desktop environment detection — the daemon now picks the right backend without manual configuration
- Sway backend — tracks focused windows via Sway's IPC socket, with support for both native Wayland (`app_id`) and XWayland (`window_properties.class`) windows
- X11 backend — tracks focused windows on any EWMH-compliant X11 window manager (i3, bspwm, awesome, openbox, etc.) via `_NET_ACTIVE_WINDOW`
- GNOME Shell backend — tracks focused windows via the `org.gnome.Shell.Introspect` D-Bus interface
- KDE Plasma backend — tracks focused windows via a KWin script that monitors `workspace.windowActivated` and reports back over D-Bus
- Niri backend — tracks focused windows via Niri's JSON IPC socket

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

- **Internal**: Refactored module structure from `mod.rs` pattern to named modules

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

[unreleased]: https://github.com/joshuadavidthomas/wakatime-focusd/compare/v0.1.3...HEAD
[0.1.0]: https://github.com/joshuadavidthomas/wakatime-focusd/releases/tag/v0.1.0
[0.1.1]: https://github.com/joshuadavidthomas/wakatime-focusd/releases/tag/v0.1.1
[0.1.2]: https://github.com/joshuadavidthomas/wakatime-focusd/releases/tag/v0.1.2
[0.1.3]: https://github.com/joshuadavidthomas/wakatime-focusd/releases/tag/v0.1.3
