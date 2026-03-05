# wakatime-focusd Review & Improvement Plan

## The Good

1. **Architecture is genuinely excellent.** The pipeline design — `FocusSource → HeartbeatBuilder → HeartbeatThrottle → HeartbeatSender` — is clean, decoupled, and testable. The trait-based dependency injection (`FocusSource`, `HeartbeatSender`) lets integration tests run the full pipeline without a desktop, D-Bus, or wakatime-cli.

2. **Backend breadth is impressive.** 8 backends covering essentially every Linux desktop environment, each using native protocols (IPC sockets, D-Bus, Wayland protocols, X11 EWMH). The auto-detection cascade is well-ordered and sensible.

3. **Test quality is high.** The `daemon_pipeline.rs` integration tests cover the full pipeline including throttle dedup, idle gating, idle transitions, periodic timer, allowlist/denylist, category rules, and title strategies. Per-backend tests spin up real protocol servers (Wayland server, Xvfb, D-Bus launch).

4. **Operational polish is unusual for a personal project.** Self-installing systemd service, `config init` with documented template, `config dump` for debugging, `oneshot` mode, exponential backoff reconnection, rate-limited error logging, graceful signal handling. The CHANGELOG is well-maintained. The README is thorough with troubleshooting guidance.

5. **Code quality is clean.** Clippy pedantic is enabled, formatting is enforced, the module structure is flat and navigable, domain types use newtypes (`Entity`), and error handling is sensible (anyhow for the binary, thiserror for the library error type).

## Areas for Improvement

### `service.rs` doesn't pass through CLI flags

The generated systemd unit hardcodes `ExecStart={binary_path}` with no arguments. If a user has `--config /custom/path.toml` or `--backend sway`, they have to manually edit the unit file. `service install` should accept and embed these flags, or at minimum pass through the `--config` flag.

### No config file watching / reload

Changing config requires `systemctl --user restart wakatime-focusd`. A `SIGHUP` handler to reload config (or inotify on the config file) would be a nice daemon affordance. Not urgent, but it's a standard daemon pattern.

### `zbus::Proxy` is recreated on every idle poll

In `idle.rs`, `get_idle_hint()` creates a new `zbus::Proxy` on every call (every 10s). Proxies are lightweight in zbus 5, but caching it alongside the connection would be cleaner and slightly more efficient.

### No installation path beyond building from source

The only way to install is `git clone` + `cargo build --release` + manual copy to `~/.local/bin/`. There's no crates.io publish, no prebuilt binaries attached to GitHub releases (the release workflow builds them but discoverability is low), and no distro packages. This is the biggest barrier to adoption.

### No health/status endpoint

There's no way to query the daemon's internal state (current backend, last heartbeat entity/time, idle state, error count) without reading journal logs. A small Unix socket or `sd_notify` integration would help debugging.

### The `async_trait` crate is unnecessary on edition 2024

Rust 1.75+ supports `async fn` in traits natively. You're on edition 2024 — you can drop `async_trait` from both `FocusSource` and `HeartbeatSender`. This removes a dependency and the hidden `Box<dyn Future>` allocations.

### No `Display` impl for `Category`

`Category::as_str()` exists but there's no `Display` impl, so you can't use `{}` formatting directly. Minor, but it's the idiomatic Rust pattern.

### `wakatime-cli` output is captured but not logged on success

`stdout` and `stderr` are piped but only stderr is logged on failure. On success, stdout is silently discarded. If wakatime-cli ever emits useful info (rate limit warnings, etc.), it would be lost.

### Build CI only targets x86_64-unknown-linux-gnu

No cross-compilation for aarch64 (Raspberry Pi, Asahi Linux). Given this is a Linux-only tool, aarch64 is the main missing target.

### No `--version` output includes git commit hash

The version is logged at startup, but `wakatime-focusd --version` doesn't show the git commit hash. For a daemon, knowing the exact build is valuable for bug reports.

### Allowlist/denylist use exact string matching, but category rules use regex

This inconsistency could confuse users. Consider supporting regex in allowlist/denylist too, or at least documenting the difference prominently.

## Roadmap

### Tier 1 — Low-hanging fruit

- [x] **`service install --config <path>`** — embed config path in the generated unit file
- [x] **Drop `async_trait`** — use native async trait syntax (edition 2024)
- [x] **aarch64-unknown-linux-gnu build** — add to CI matrix
- [x] **`SIGHUP` config reload** — standard daemon pattern
- [x] **`Display` for `Category`** and `Entity`
- [x] **`cargo-dist`** — generates the install.sh script, GitHub Release CI, platform matrix builds, and `cargo-binstall` metadata in one shot. Replaces the hand-rolled release workflow and solves install.sh, cross-compilation, and binary distribution all at once.
- [ ] **`cargo-dist` updater** — enable the built-in self-updater so users can run `wakatime-focusd update` to fetch the latest release without re-running the install script.
- [ ] **CLI snapshot testing** — use [trycmd](https://github.com/assert-rs/trycmd) or [snapbox](https://github.com/assert-rs/snapbox) to snapshot-test `--help` output and command behavior. Catches unintentional CLI changes (flag renames, default changes, subcommand reorganization) that `cargo-semver-checks` can't detect since it only covers library API surfaces.

### Tier 2 — Valuable additions

- [ ] **Project detection** — detect the project from the focused window (e.g., parse terminal title for cwd, read IDE window title for project name) and pass `--project` to wakatime-cli. This is the single most impactful missing feature — without it, all app time goes into an "unassigned" bucket in WakaTime.
- [ ] **Heartbeat batching** — wakatime-cli supports `--extra-heartbeats` via stdin (JSON array). Batching reduces process spawns, especially during rapid focus switching.
- [ ] **Offline queue** — if wakatime-cli fails (network down), queue heartbeats to a local file and replay them later. wakatime-cli has its own offline queue, but catching spawn failures (binary missing, permissions) at the daemon level would be more resilient.
- [ ] **Metrics / status socket** — expose internal state (last entity, heartbeat count, error count, idle state, uptime) via a Unix socket or `sd_notify` STATUS string.

### Tier 3 — Bigger lifts

- [ ] **macOS support** — use `NSWorkspace` notifications for active app tracking + launchd instead of systemd. Would require abstracting the idle monitor and service management.
- [ ] **Direct API integration** — bypass wakatime-cli entirely and POST heartbeats to the WakaTime API directly (the API is well-documented). Eliminates the process-spawn overhead and the wakatime-cli dependency.
- [ ] **Packaging** — AUR package, Nix flake, Fedora COPR, `.deb`/`.rpm` via `cargo-deb`/`cargo-generate-rpm`. The `service install` command partially replaces this, but distribution packages handle updates and discoverability.
- [ ] **Plugin system for backends** — if more backends keep coming, a trait-object plugin architecture (or even dynamic loading) could keep the binary size in check. Not needed yet with 8 backends, but worth considering.
- [ ] **Window-level time tracking** — track individual windows, not just app classes. Combined with title tracking, this could provide per-tab or per-document time breakdowns.
