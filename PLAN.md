# wakatime-focusd Review & Improvement Plan

## Issues

### 1. D-Bus connection created on every idle poll

**File:** `src/idle.rs:97-99`

`poll_idle_state()` calls `Connection::system().await` on every invocation (default: every 10 seconds). This creates a new D-Bus connection each time instead of reusing one. Store the connection alongside `session_path` in the struct:

```rust
pub struct IdleMonitor {
    idle_hint: AtomicBool,
    session_path: RwLock<Option<String>>,
    connection: RwLock<Option<Connection>>,  // add this
    enabled: AtomicBool,
}
```

Initialize it in `init()` and reuse in `poll_idle_state()`.

### 2. No graceful shutdown / signal handling

**File:** `src/main.rs:266-282`

The daemon loop has no `SIGTERM`/`SIGINT` handling. When systemd stops the service, `tokio::main` is abruptly interrupted. Consider adding `tokio::signal::ctrl_c()` or `tokio::signal::unix::signal(SignalKind::terminate())` to the `run_daemon` loop, allowing a clean exit and optionally flushing a final heartbeat.

### 3. Reconnect loop has no delay between backend connect failures

**File:** `src/main.rs:274-281`

If `backend::connect()` fails repeatedly (e.g., socket doesn't exist yet after compositor restart), this loop hammers `connect()` with no backoff:

```rust
loop {
    let source = wakatime_focusd::backend::connect(backend).await?;  // fails with ?
    ...
}
```

The `?` propagates immediately to `main`, crashing the daemon. If you want reconnection resilience at this level (not just inside backends), add a backoff loop or `tokio::time::sleep` before retrying.

### 4. Idle polling task never exits

**File:** `src/idle.rs:125-143`

`start_polling` spawns a task with an infinite `loop`. It can only exit if `init()` fails. There's no way to stop it (no cancellation token, no shutdown signal). The `Arc<IdleMonitor>` keeps it alive forever. Consider accepting a `CancellationToken` or `tokio::sync::watch` for clean shutdown.

### 5. Global mutable state for error rate limiting

**File:** `src/wakatime.rs:30`

`static ERROR_LOG_COUNT: AtomicU32` is process-global and never resets. This means:

- After `u32::MAX` errors it wraps (minor)
- In tests, the counter leaks between test cases
- The rate limiting logic is untestable in isolation

Consider moving this counter into `WakaTimeClient` as a field instead.

### 6. Category regex patterns match substrings

**File:** `src/heartbeat.rs`

`Regex::is_match` matches anywhere in the string. A pattern like `"code"` would match `app_class = "unicode-input"`. Consider anchoring patterns or documenting this behavior prominently. Users might expect exact matches.

## Minor / Nits

- **`FocusEvent` ordering in Hyprland** (`src/backend/hyprland.rs`): `activewindowv2` can arrive *after* `activewindow`, meaning the address attached to a focus event may be from the *previous* window switch. This is a known Hyprland IPC ordering quirk — the current behavior (stale address) is probably fine since `window_id` isn't used downstream, but worth a comment.

- **`service.rs` uses blocking `std::process::Command`** (`src/service.rs:47-59`): Fine today since service subcommands exit before the async runtime starts, but if this code is ever called from an async context it would block the executor. A comment noting this assumption would help.

- **`Heartbeat` cloned unnecessarily for periodic resend** (`src/lib.rs:69`, `src/lib.rs:82`): The periodic timer path clones `last_heartbeat.source` to rebuild a heartbeat that's structurally identical to the one already stored. Consider having the periodic path just re-send the stored `Heartbeat` directly instead of rebuilding it.

- **No `reconnect_delay` between backend reconnects in the outer daemon loop** (`src/main.rs`): The inner backends (Hyprland, Sway) have exponential backoff, but the outer `run_daemon` loop doesn't — if the inner loop returns `SourceError`, it immediately tries `connect()` again.
