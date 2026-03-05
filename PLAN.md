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
- [x] **`cargo-dist` updater** — enable the built-in self-updater so users can run `wakatime-focusd update` to fetch the latest release without re-running the install script.
- [x] **CLI snapshot testing** — use [trycmd](https://github.com/assert-rs/trycmd) or [snapbox](https://github.com/assert-rs/snapbox) to snapshot-test `--help` output and command behavior. Catches unintentional CLI changes (flag renames, default changes, subcommand reorganization) that `cargo-semver-checks` can't detect since it only covers library API surfaces.

### Tier 1.5 — Direct WakaTime API integration

Bypass wakatime-cli entirely and POST heartbeats to the WakaTime API directly. This supersedes the previously planned heartbeat batching and offline queue items (which were designed around CLI workarounds) and eliminates the wakatime-cli external dependency.

**Why now:** The daemon only sends `--entity-type app` heartbeats — it doesn't use wakatime-cli's file analysis, language detection, or git integration. The actual API surface needed is a single JSON POST with ~6 fields and bearer auth. The `HeartbeatSender` trait is the perfect seam; the entire pipeline (focus sources → builder → throttle) stays untouched.

**Implementation plan:**

#### Phase 1: API key reading + config

- [ ] **Read API key from `~/.wakatime.cfg`** — parse the INI file (`[settings]` section, `api_key` key). Use a lightweight approach (hand-parse or `configparser` crate). Support `$WAKATIME_API_KEY` env var as override.
- [ ] **Add `sender` config field** — `sender = "api"` (new default) or `sender = "cli"` (legacy). Keeps `WakaTimeClient` as a fallback during transition. Add `api_url` field (default: `https://api.wakatime.com/api/v1`) for self-hosted WakaTime instances.
- [ ] **Update config template** — document the new fields, comment out `wakatime_cli_path` in the template since it's only relevant for `sender = "cli"`.

#### Phase 2: Basic `ApiSender`

- [ ] **Add `reqwest` dependency** — with `rustls-tls` feature (avoids OpenSSL linking, consistent across distros).
- [ ] **Implement `ApiSender`** — new `HeartbeatSender` impl that POSTs to `POST /api/v1/users/current/heartbeats`. JSON body: `{ "entity", "type": "app", "category", "time", "plugin" }`. Auth: `Authorization: Basic base64(api_key)`. Reuse a single `reqwest::Client` (connection pooling).
- [ ] **Handle API errors** — 401 (bad key, log once and clearly), 429 (rate limit, respect `Retry-After` header), 5xx (backoff). Reuse the existing rate-limited error logging pattern (`AtomicU32` counter).
- [ ] **Dry-run support** — log the JSON payload instead of sending, matching existing `WakaTimeClient` dry-run behavior.
- [ ] **Wire into `main.rs`** — construct `ApiSender` or `WakaTimeClient` based on `config.sender`. Handle reload (reconstruct sender on `SIGHUP`).

#### Phase 3: Heartbeat batching

- [ ] **Buffer heartbeats internally** — `ApiSender` collects heartbeats in an internal buffer instead of POSTing immediately. The `send_heartbeat` trait method adds to the buffer.
- [ ] **Flush on timer or threshold** — flush the buffer via `POST /api/v1/users/current/heartbeats.bulk` (max 25 per request per API docs) either when the buffer hits a threshold (e.g., 10) or on a configurable timer (e.g., 30s). A background flush task runs alongside the event loop.
- [ ] **Flush on shutdown** — drain the buffer on graceful shutdown (SIGTERM/SIGINT) so no heartbeats are lost.

#### Phase 4: Offline queue

- [ ] **Persist on failure** — when a flush fails (network down, 5xx), append the batch to a local file (`~/.local/share/wakatime-focusd/queue.jsonl`, one JSON array per line).
- [ ] **Drain on reconnect** — after a successful flush, check for queued batches and replay them. Process oldest first, respect rate limits.
- [ ] **Bound the queue** — cap the queue file size (e.g., 10MB / ~50k heartbeats) to prevent unbounded disk growth. Drop oldest entries when full.

#### Phase 5: Cleanup

- [ ] **Deprecate CLI sender** — mark `sender = "cli"` as deprecated in docs/config template. Keep it functional but recommend migration.
- [ ] **Remove `which` dependency** — only needed for wakatime-cli discovery. Can be made optional behind a feature flag or removed once CLI sender is fully deprecated.
- [ ] **Update README** — installation no longer requires wakatime-cli. Just needs an API key in `~/.wakatime.cfg` or `$WAKATIME_API_KEY`.

### Tier 2 — Valuable additions

- [ ] **Project detection** — detect the project from the focused window (e.g., parse terminal title for cwd, read IDE window title for project name) and include in heartbeat. This is the single most impactful missing feature — without it, all app time goes into an "unassigned" bucket in WakaTime.
- [ ] **Metrics / status socket** — expose internal state (last entity, heartbeat count, error count, idle state, uptime) via a Unix socket or `sd_notify` STATUS string.

### Tier 3 — Bigger lifts

- [ ] **macOS support** — use `NSWorkspace` notifications for active app tracking + launchd instead of systemd. Would require abstracting the idle monitor and service management.
- [ ] **Packaging** — AUR package, Nix flake, Fedora COPR, `.deb`/`.rpm` via `cargo-deb`/`cargo-generate-rpm`. The `service install` command partially replaces this, but distribution packages handle updates and discoverability.
- [ ] **Plugin system for backends** — if more backends keep coming, a trait-object plugin architecture (or even dynamic loading) could keep the binary size in check. Not needed yet with 8 backends, but worth considering.
- [ ] **Window-level time tracking** — track individual windows, not just app classes. Combined with title tracking, this could provide per-tab or per-document time breakdowns.
