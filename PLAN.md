# Multi-Backend Support Plan

Add support for multiple desktop environment backends beyond the current Hyprland-only implementation.

## Branches

Each phase is implemented on its own branch, merged to `main` before starting the next.

### 1. `backend-architecture` — Dynamic backend selection ✅

Refactor `main.rs` to use `Box<dyn FocusSource>` instead of hardcoding `HyprlandSource`.

- [x] Add `Backend` enum to config (`auto`, `hyprland`, `sway`, `gnome`, `kde`, `x11`, `niri`)
- [x] Add `--backend` CLI flag (defaults to `auto`)
- [x] Implement auto-detection: check env vars / D-Bus to pick the right backend
- [x] Wire up `connect_backend()` factory function in `src/backend.rs`
- [x] Remove Hyprland-specific env checks from `main.rs`
- [x] Update diagnostics to be backend-aware
- [x] Update `FocusError` variants for backend-generic errors

### 2. `backend-sway` — Sway backend ✅

Sway/i3-compatible IPC via `SWAYSOCK` Unix socket.

- [x] Detect via `SWAYSOCK` env var
- [x] Subscribe to `window::focus` events via raw IPC binary protocol
- [x] Parse JSON event payloads for `app_id` (Wayland) and `window_properties.class` (XWayland)
- [x] Extract `name` (title) and `id` (window id)
- [x] Handle reconnection with backoff (same pattern as Hyprland)
- [x] Add tests for event parsing

### 3. `backend-x11` — Generic X11 backend ✅

Covers all X11 window managers (i3, bspwm, awesome, openbox, etc.) via `_NET_ACTIVE_WINDOW`.

- [x] Add `x11rb` dependency (blocking with `spawn_blocking`)
- [x] Detect via `DISPLAY` env var (and absence of Wayland indicators)
- [x] Listen for `_NET_ACTIVE_WINDOW` property change events on the root window
- [x] Read `WM_CLASS`, `_NET_WM_NAME` / `WM_NAME`, and window ID from the active window
- [x] Handle window destruction between detection and property read
- [x] Add tests for property parsing

### 4. `backend-gnome` — GNOME Shell backend ✅

D-Bus interface to GNOME Shell's window tracking.

- [x] Detect via `XDG_CURRENT_DESKTOP=GNOME`
- [x] Use D-Bus (`zbus`) with `org.gnome.Shell.Introspect` interface
- [x] Subscribe to `WindowsChanged` signal for event-driven detection
- [x] Extract `wm-class` (with `app-id` fallback), `title` from focused window
- [x] Deduplicate focus events to only emit on actual changes
- [x] Add tests for response parsing

### 5. `backend-kde` — KDE Plasma backend ✅

D-Bus interface to KWin's scripting/window tracking.

- [x] Detect via `XDG_CURRENT_DESKTOP=KDE`
- [x] Use D-Bus (`zbus`) to interact with `org.kde.KWin`
- [x] Use KWin scripting interface to register a focus-change callback
- [x] Script calls back via D-Bus with `resourceClass`, `caption`, `internalId`
- [x] Clean up temp script file on drop

### 6. `backend-niri` — Niri backend ✅

Niri compositor IPC via `NIRI_SOCKET` Unix socket.

- [x] Detect via `NIRI_SOCKET` env var
- [x] Connect to Niri's IPC socket and subscribe to EventStream
- [x] Track `WindowOpenedOrChanged` to build window cache
- [x] Resolve `WindowFocusChanged` IDs to `app_id` and `title` from cache
- [x] Handle `WindowClosed` to keep cache clean
- [x] Handle reconnection with backoff
- [x] Add tests for event parsing and window cache logic

## Integration Testing

Full-fidelity integration tests for all backends and the daemon event loop, runnable in CI without real desktop environments.

### 7. `integration-test-infra` — Mock FocusSource + daemon loop tests ✅

Refactor `run_daemon`/`run_oneshot` to accept a `Box<dyn FocusSource>` (or factory) so the full event pipeline can be tested with scripted events.

- [x] Refactor `run_daemon` and `run_oneshot` to accept injected source
- [x] Create `MockFocusSource` that replays a scripted sequence of `FocusEvent`s
- [x] Test full pipeline: event → allowlist/denylist filter → heartbeat build → throttle dedup → idle gating → send
- [x] Test periodic heartbeat timer fires for sustained focus on same app
- [x] Test rapid focus switching (A → B → A within throttle window)
- [x] Test idle suppression (events arrive while session is idle)
- [x] Test empty/no-focus events are skipped

### 8. `integration-test-sockets` — Fake socket servers for Hyprland, Sway, Niri

Spin up fake Unix socket servers that speak each protocol, connect the real backend, verify `FocusEvent` output.

- [ ] **Hyprland**: Fake server writes `activewindow>>` and `activewindowv2>>` lines
  - [ ] Basic focus event round-trip
  - [ ] Reconnection after EOF (server closes socket, backend reconnects)
  - [ ] Reconnection after read error
  - [ ] Malformed lines are skipped without crashing
  - [ ] Empty class (no focus) is skipped
- [ ] **Sway**: Fake server speaks i3-ipc binary protocol
  - [ ] Respond to subscribe handshake (`["window"]` → `{"success": true}`)
  - [ ] Send window focus events with i3-ipc framing
  - [ ] Wayland `app_id` events
  - [ ] XWayland `window_properties.class` fallback events
  - [ ] Reconnection after EOF
  - [ ] Subscribe failure response
- [ ] **Niri**: Fake server speaks JSON lines protocol
  - [ ] Respond to `"EventStream"` handshake with `Ok` reply
  - [ ] Send `WindowOpenedOrChanged` followed by `WindowFocusChanged`
  - [ ] Window cache coherence: focus on unknown window ID produces nothing until window info arrives
  - [ ] `WindowClosed` removes from cache
  - [ ] Reconnection after EOF
  - [ ] Handshake failure response

### 9. `integration-test-dbus` — D-Bus integration tests for GNOME and KDE

Run a private D-Bus session bus in tests. Mock the compositor-side D-Bus services.

- [ ] **GNOME**: Mock `org.gnome.Shell.Introspect` service
  - [ ] `GetWindows()` returns windows with `has-focus`, `wm-class`, `title`, `app-id`
  - [ ] Emit `WindowsChanged` signal, verify source picks up focus change
  - [ ] Focus dedup: signal fires but focused window unchanged → no event emitted
  - [ ] Multiple windows, focus switches between them
  - [ ] Empty `wm-class` falls back to `app-id`
  - [ ] Connection failure when service is unavailable
- [ ] **KDE**: Mock `org.kde.kwin.Scripting` service + direct `FocusReceiver` calls
  - [ ] Mock accepts `loadScript(path, name)` → returns script ID
  - [ ] Mock accepts `run()` on `/Scripting/Script{id}`
  - [ ] Directly call `FocusChanged` on `org.wakatime.focusd` → verify event received
  - [ ] Multiple focus changes in sequence
  - [ ] Empty class events are filtered
  - [ ] Script unload on reconnect/restart
  - [ ] Note: KWin JS script execution is untestable outside KDE — this tests everything on the Rust side of the boundary

### 10. `integration-test-x11` — Xvfb-based X11 backend tests

Run against a virtual X11 display (Xvfb) with no window manager. Manually set X properties to simulate focus changes.

- [ ] Set up Xvfb display in test (or CI provides `xvfb-run`)
- [ ] Create X windows with `WM_CLASS` and `_NET_WM_NAME` properties
- [ ] Set `_NET_ACTIVE_WINDOW` on root window to trigger `PropertyNotify`
- [ ] Verify `FocusEvent` with correct class, title, and window ID
- [ ] Switch focus between multiple windows
- [ ] Window destroyed between focus change and property read → graceful skip
- [ ] `_NET_WM_NAME` (UTF-8) preferred over `WM_NAME` (Latin-1) fallback
- [ ] No active window (ID 0) → no event emitted

## Backend Detection Order (`auto` mode)

When `--backend auto` (the default), detect in this order:

1. **Hyprland** — `HYPRLAND_INSTANCE_SIGNATURE` is set
2. **Sway** — `SWAYSOCK` is set
3. **Niri** — `NIRI_SOCKET` is set
4. **KDE** — `XDG_CURRENT_DESKTOP` contains `KDE`
5. **GNOME** — `XDG_CURRENT_DESKTOP` contains `GNOME`
6. **X11** — `DISPLAY` is set (fallback for any X11 WM)

Wayland-native backends are preferred over the generic X11 fallback.

## Shared Patterns

All backends follow these conventions:

- Implement `FocusSource` trait (`async fn next_event()`)
- Provide a `connect()` async constructor
- Handle reconnection internally with exponential backoff (250ms → 5s cap)
- Provide `get_diagnostics() -> Vec<String>` for debug output
- Live in `src/backend/<name>.rs`
- Include unit tests for event/message parsing
