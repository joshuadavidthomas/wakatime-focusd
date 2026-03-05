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
- [x] **`cargo-dist` updater** — enable the built-in self-updater so users can run `wakatime-focusd update` to fetch the latest release without re-running the install script.
- [x] **CLI snapshot testing** — use [trycmd](https://github.com/assert-rs/trycmd) or [snapbox](https://github.com/assert-rs/snapbox) to snapshot-test `--help` output and command behavior. Catches unintentional CLI changes (flag renames, default changes, subcommand reorganization) that `cargo-semver-checks` can't detect since it only covers library API surfaces.

### Tier 1.5 — Direct WakaTime API integration

Replaced wakatime-cli with direct HTTP API integration. Heartbeats are sent to the WakaTime API (or compatible servers like Wakapi) with batching and an offline queue. The `HeartbeatSender` trait remains the seam between the pipeline and the sender; the full pipeline (focus sources → builder → throttle) was untouched.

- [x] **API key reading** — hand-parsed INI reader for `~/.wakatime.cfg` (`[settings]` section, `api_key` and `api_url` fields). `$WAKATIME_API_KEY` env var takes priority. No new dependencies.
- [x] **`api_url` config field** — supports self-hosted Wakapi instances. Resolution priority: daemon config → `~/.wakatime.cfg` → default (`https://api.wakatime.com/api`).
- [x] **`ApiSender`** — `HeartbeatSender` impl that POSTs JSON heartbeats to `/v1/users/current/heartbeats` with Basic auth. Single `reqwest::Client` for connection pooling. Error handling: 401 (clear auth message), 429 (logs `Retry-After`), 4xx/5xx (rate-limited logging). Dry-run logs the JSON payload.
- [x] **Heartbeat batching** — `send_heartbeat()` buffers heartbeats in memory. Auto-flush at threshold (10) or on periodic timer tick. Bulk endpoint (`/heartbeats.bulk`, max 25/request) for multi-heartbeat flushes, single endpoint for 1. Flush on shutdown, reload, and source error.
- [x] **Offline queue** — failed flushes persist to `~/.local/share/wakatime-focusd/queue.jsonl` (JSONL, one batch per line). Drained oldest-first (up to 10 batches) after each successful flush and on periodic ticks. Queue bounded at 10 MB; corrupt entries skipped.
- [x] **Removed CLI sender** — deleted `wakatime.rs`, `WakaTimeClient`, `find_wakatime_cli()`, `which` dependency, `SenderBackend` enum, `sender` and `wakatime_cli_path` config fields. `HeartbeatSender` trait moved to `api.rs`. README updated: requirements list API key (not wakatime-cli), added Wakapi mention, updated troubleshooting.

### Tier 2 — Valuable additions

- [ ] **Project detection** — detect the project from the focused window (e.g., parse terminal title for cwd, read IDE window title for project name) and include in heartbeat. This is the single most impactful missing feature — without it, all app time goes into an "unassigned" bucket in WakaTime.
- [ ] **Metrics / status socket** — expose internal state (last entity, heartbeat count, error count, idle state, uptime) via a Unix socket or `sd_notify` STATUS string.

### Tier 3 — Bigger lifts

- [ ] **macOS support** — use `NSWorkspace` notifications for active app tracking + launchd instead of systemd. Would require abstracting the idle monitor and service management.
- [ ] **Packaging** — AUR package, Nix flake, Fedora COPR, `.deb`/`.rpm` via `cargo-deb`/`cargo-generate-rpm`. The `service install` command partially replaces this, but distribution packages handle updates and discoverability.
- [ ] **Plugin system for backends** — if more backends keep coming, a trait-object plugin architecture (or even dynamic loading) could keep the binary size in check. Not needed yet with 8 backends, but worth considering.
- [ ] **Window-level time tracking** — track individual windows, not just app classes. Combined with title tracking, this could provide per-tab or per-document time breakdowns.
