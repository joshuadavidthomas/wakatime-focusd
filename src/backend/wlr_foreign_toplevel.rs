//! Generic Wayland focus detection via the `wlr-foreign-toplevel-management` protocol.
//!
//! This backend works with any compositor that implements
//! `wlr-foreign-toplevel-management-unstable-v1`, including River, Wayfire,
//! labwc, dwl, and others. It monitors toplevel `State` events for the
//! `Activated` flag to detect focus changes.
//!
//! The Wayland event loop is synchronous, so it runs in a dedicated thread
//! via `spawn_blocking` and sends focus events through an `mpsc` channel.

use std::collections::HashMap;
use std::env;

use async_trait::async_trait;
use tokio::sync::mpsc;
use tracing::debug;
use tracing::info;
use tracing::trace;
use tracing::warn;
use wayland_client::Connection;
use wayland_client::Dispatch;
use wayland_client::Proxy;
use wayland_client::QueueHandle;
use wayland_client::globals::GlobalListContents;
use wayland_client::globals::registry_queue_init;
use wayland_client::protocol::wl_registry;
use wayland_protocols_wlr::foreign_toplevel::v1::client::zwlr_foreign_toplevel_handle_v1;
use wayland_protocols_wlr::foreign_toplevel::v1::client::zwlr_foreign_toplevel_handle_v1::ZwlrForeignToplevelHandleV1;
use wayland_protocols_wlr::foreign_toplevel::v1::client::zwlr_foreign_toplevel_manager_v1;
use wayland_protocols_wlr::foreign_toplevel::v1::client::zwlr_foreign_toplevel_manager_v1::ZwlrForeignToplevelManagerV1;

use super::FocusError;
use super::FocusEvent;
use super::FocusSource;

/// The `Activated` state value in the wlr-foreign-toplevel protocol.
const STATE_ACTIVATED: u32 = 2;

/// Wayland focus source using `wlr-foreign-toplevel-management`.
///
/// Runs a blocking Wayland event loop in a dedicated thread and receives
/// focus events via a channel.
pub struct WlrForeignToplevelSource {
    rx: mpsc::Receiver<FocusEvent>,
}

impl WlrForeignToplevelSource {
    /// Connect to the Wayland display and bind the foreign-toplevel-manager.
    #[allow(clippy::unused_async)]
    pub async fn connect() -> Result<Self, FocusError> {
        if env::var("WAYLAND_DISPLAY").is_err() {
            return Err(FocusError::EnvVarNotSet("WAYLAND_DISPLAY".to_string()));
        }

        // Verify we can connect and the protocol is available before spawning
        // the long-lived thread.
        verify_protocol()?;

        let (tx, rx) = mpsc::channel(32);

        tokio::task::spawn_blocking(move || {
            if let Err(e) = run_event_loop(&tx) {
                warn!("wlr-foreign-toplevel event loop exited: {e}");
            }
        });

        info!("Connected to Wayland display via wlr-foreign-toplevel-management");
        Ok(Self { rx })
    }

    /// Diagnostic information about the Wayland environment.
    pub fn get_diagnostics() -> Vec<String> {
        let mut diags = Vec::new();

        match env::var("WAYLAND_DISPLAY") {
            Ok(v) => diags.push(format!("WAYLAND_DISPLAY={v}")),
            Err(_) => diags.push("WAYLAND_DISPLAY: NOT SET".to_string()),
        }

        match env::var("XDG_CURRENT_DESKTOP") {
            Ok(v) => diags.push(format!("XDG_CURRENT_DESKTOP={v}")),
            Err(_) => diags.push("XDG_CURRENT_DESKTOP: NOT SET".to_string()),
        }

        diags
    }
}

#[async_trait]
impl FocusSource for WlrForeignToplevelSource {
    async fn next_event(&mut self) -> Result<FocusEvent, FocusError> {
        self.rx.recv().await.ok_or_else(|| {
            FocusError::ConnectionFailed("wlr-foreign-toplevel event loop closed".to_string())
        })
    }
}

/// Per-toplevel pending state accumulated between `Done` events.
#[derive(Debug, Clone, Default)]
struct WindowData {
    app_id: String,
    title: String,
    activated: bool,
}

/// Shared state for Wayland dispatch.
struct ToplevelState {
    /// Pending state for each toplevel handle, committed on `Done`.
    windows: HashMap<u32, WindowData>,
    /// The ID of the currently focused toplevel (after `Done`).
    focused_id: Option<u32>,
    tx: mpsc::Sender<FocusEvent>,
}

impl ToplevelState {
    fn new(tx: mpsc::Sender<FocusEvent>) -> Self {
        Self {
            windows: HashMap::new(),
            focused_id: None,
            tx,
        }
    }

    /// Process a committed toplevel state update. Returns a `FocusEvent` if
    /// focus changed.
    fn commit(&mut self, id: u32) -> Option<FocusEvent> {
        let window = self.windows.get(&id)?;

        if window.activated {
            // Already focused on this window — no change.
            if self.focused_id == Some(id) {
                return None;
            }
            self.focused_id = Some(id);

            let app_class = &window.app_id;
            if app_class.is_empty() {
                return None;
            }

            let title = if window.title.is_empty() {
                None
            } else {
                Some(window.title.clone())
            };

            debug!(
                "Focus changed: class={}, title={:?}, window_id={id}",
                app_class, title
            );

            Some(FocusEvent::new(
                app_class.clone(),
                title,
                Some(id.to_string()),
            ))
        } else if self.focused_id == Some(id) {
            // This window lost focus.
            self.focused_id = None;
            None
        } else {
            None
        }
    }

    /// Remove a toplevel from tracking.
    fn remove(&mut self, id: u32) {
        self.windows.remove(&id);
        if self.focused_id == Some(id) {
            self.focused_id = None;
        }
    }
}

/// Connect to the Wayland display and verify the protocol global exists.
fn verify_protocol() -> Result<(), FocusError> {
    let conn = Connection::connect_to_env()
        .map_err(|e| FocusError::ConnectionFailed(format!("Wayland connect: {e}")))?;

    // Use a temporary probe state to enumerate globals.
    let (globals, _event_queue) = registry_queue_init::<ProbeState>(&conn)
        .map_err(|e| FocusError::ConnectionFailed(format!("Registry init: {e}")))?;

    let has_manager = globals
        .contents()
        .clone_list()
        .iter()
        .any(|g| g.interface == ZwlrForeignToplevelManagerV1::interface().name);

    if !has_manager {
        return Err(FocusError::ConnectionFailed(
            "Compositor does not support wlr-foreign-toplevel-management".to_string(),
        ));
    }

    Ok(())
}

/// Minimal state used only to probe for the protocol global.
struct ProbeState;

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for ProbeState {
    fn event(
        _state: &mut Self,
        _proxy: &wl_registry::WlRegistry,
        _event: wl_registry::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for ToplevelState {
    fn event(
        _state: &mut Self,
        _proxy: &wl_registry::WlRegistry,
        _event: wl_registry::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

/// Run the blocking Wayland event loop.
fn run_event_loop(tx: &mpsc::Sender<FocusEvent>) -> Result<(), FocusError> {
    let conn = Connection::connect_to_env()
        .map_err(|e| FocusError::ConnectionFailed(format!("Wayland connect: {e}")))?;

    let (globals, mut event_queue) = registry_queue_init::<ToplevelState>(&conn)
        .map_err(|e| FocusError::ConnectionFailed(format!("Registry init: {e}")))?;

    let qh = event_queue.handle();

    let _manager: ZwlrForeignToplevelManagerV1 = globals
        .bind(&qh, 1..=3, ())
        .map_err(|e| FocusError::ConnectionFailed(format!("Bind foreign-toplevel: {e}")))?;

    let mut state = ToplevelState::new(tx.clone());

    // Initial roundtrip to receive existing toplevels.
    event_queue
        .roundtrip(&mut state)
        .map_err(|e| FocusError::ConnectionFailed(format!("Initial roundtrip: {e}")))?;

    loop {
        event_queue
            .blocking_dispatch(&mut state)
            .map_err(|e| FocusError::ConnectionFailed(format!("Dispatch error: {e}")))?;
    }
}

// Dispatch: manager events
impl Dispatch<ZwlrForeignToplevelManagerV1, ()> for ToplevelState {
    fn event(
        state: &mut Self,
        _proxy: &ZwlrForeignToplevelManagerV1,
        event: zwlr_foreign_toplevel_manager_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_foreign_toplevel_manager_v1::Event::Toplevel { toplevel } => {
                let id = toplevel.id().protocol_id();
                trace!("New toplevel: {id}");
                state.windows.insert(id, WindowData::default());
            }
            zwlr_foreign_toplevel_manager_v1::Event::Finished => {
                warn!("Foreign toplevel manager finished (compositor shutting down?)");
            }
            _ => {}
        }
    }

    wayland_client::event_created_child!(Self, ZwlrForeignToplevelManagerV1, [
        zwlr_foreign_toplevel_manager_v1::EVT_TOPLEVEL_OPCODE => (ZwlrForeignToplevelHandleV1, ()),
    ]);
}

// Dispatch: per-toplevel handle events
impl Dispatch<ZwlrForeignToplevelHandleV1, ()> for ToplevelState {
    fn event(
        state: &mut Self,
        proxy: &ZwlrForeignToplevelHandleV1,
        event: zwlr_foreign_toplevel_handle_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        let id = proxy.id().protocol_id();

        match event {
            zwlr_foreign_toplevel_handle_v1::Event::Title { title } => {
                trace!("Title changed for {id}: {title}");
                if let Some(w) = state.windows.get_mut(&id) {
                    w.title = title;
                }
            }
            zwlr_foreign_toplevel_handle_v1::Event::AppId { app_id } => {
                trace!("AppId changed for {id}: {app_id}");
                if let Some(w) = state.windows.get_mut(&id) {
                    w.app_id = app_id;
                }
            }
            zwlr_foreign_toplevel_handle_v1::Event::State { state: raw_state } => {
                let activated = parse_states(&raw_state).any(|s| s == STATE_ACTIVATED);
                trace!("State changed for {id}: activated={activated}");
                if let Some(w) = state.windows.get_mut(&id) {
                    w.activated = activated;
                }
            }
            zwlr_foreign_toplevel_handle_v1::Event::Done => {
                trace!("Done for {id}");
                if let Some(event) = state.commit(id)
                    && state.tx.blocking_send(event).is_err()
                {
                    warn!("Receiver dropped, stopping event loop");
                }
            }
            zwlr_foreign_toplevel_handle_v1::Event::Closed => {
                trace!("Closed: {id}");
                state.remove(id);
            }
            _ => {}
        }
    }
}

/// Parse the LE-encoded state array into `u32` values.
fn parse_states(raw: &[u8]) -> impl Iterator<Item = u32> + '_ {
    raw.chunks_exact(4)
        .map(|chunk| u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_state() -> (ToplevelState, mpsc::Receiver<FocusEvent>) {
        let (tx, rx) = mpsc::channel(16);
        (ToplevelState::new(tx), rx)
    }

    #[test]
    fn commit_activated_produces_focus_event() {
        let (mut state, _rx) = make_state();
        state.windows.insert(
            1,
            WindowData {
                app_id: "firefox".to_string(),
                title: "Mozilla Firefox".to_string(),
                activated: true,
            },
        );

        let event = state.commit(1).expect("should produce event");
        assert_eq!(event.app_class, "firefox");
        assert_eq!(event.title, Some("Mozilla Firefox".to_string()));
        assert_eq!(event.window_id, Some("1".to_string()));
        assert_eq!(state.focused_id, Some(1));
    }

    #[test]
    fn commit_not_activated_no_event() {
        let (mut state, _rx) = make_state();
        state.windows.insert(
            1,
            WindowData {
                app_id: "firefox".to_string(),
                title: "Mozilla Firefox".to_string(),
                activated: false,
            },
        );

        assert!(state.commit(1).is_none());
        assert_eq!(state.focused_id, None);
    }

    #[test]
    fn commit_same_focus_no_duplicate() {
        let (mut state, _rx) = make_state();
        state.windows.insert(
            1,
            WindowData {
                app_id: "firefox".to_string(),
                title: "Tab".to_string(),
                activated: true,
            },
        );

        state.commit(1).expect("first commit should produce event");
        assert!(state.commit(1).is_none(), "second commit should be None");
    }

    #[test]
    fn commit_focus_switch() {
        let (mut state, _rx) = make_state();
        state.windows.insert(
            1,
            WindowData {
                app_id: "firefox".to_string(),
                title: String::new(),
                activated: true,
            },
        );
        state.windows.insert(
            2,
            WindowData {
                app_id: "alacritty".to_string(),
                title: "~".to_string(),
                activated: false,
            },
        );

        state.commit(1).expect("firefox gets focus");

        // Firefox loses focus, alacritty gains it.
        state.windows.get_mut(&1).unwrap().activated = false;
        state.windows.get_mut(&2).unwrap().activated = true;

        assert!(state.commit(1).is_none(), "deactivation produces no event");
        assert_eq!(state.focused_id, None);

        let event = state.commit(2).expect("alacritty gets focus");
        assert_eq!(event.app_class, "alacritty");
        assert_eq!(state.focused_id, Some(2));
    }

    #[test]
    fn commit_empty_app_id_no_event() {
        let (mut state, _rx) = make_state();
        state.windows.insert(
            1,
            WindowData {
                app_id: String::new(),
                title: "untitled".to_string(),
                activated: true,
            },
        );

        assert!(state.commit(1).is_none());
    }

    #[test]
    fn commit_empty_title_becomes_none() {
        let (mut state, _rx) = make_state();
        state.windows.insert(
            1,
            WindowData {
                app_id: "foot".to_string(),
                title: String::new(),
                activated: true,
            },
        );

        let event = state.commit(1).expect("should produce event");
        assert_eq!(event.title, None);
    }

    #[test]
    fn commit_unknown_id_returns_none() {
        let (mut state, _rx) = make_state();
        assert!(state.commit(42).is_none());
    }

    #[test]
    fn remove_clears_focus_if_focused() {
        let (mut state, _rx) = make_state();
        state.windows.insert(
            1,
            WindowData {
                app_id: "firefox".to_string(),
                title: String::new(),
                activated: true,
            },
        );
        state.commit(1);
        assert_eq!(state.focused_id, Some(1));

        state.remove(1);
        assert_eq!(state.focused_id, None);
        assert!(!state.windows.contains_key(&1));
    }

    #[test]
    fn remove_other_window_keeps_focus() {
        let (mut state, _rx) = make_state();
        state.windows.insert(
            1,
            WindowData {
                app_id: "firefox".to_string(),
                title: String::new(),
                activated: true,
            },
        );
        state.windows.insert(
            2,
            WindowData {
                app_id: "alacritty".to_string(),
                title: String::new(),
                activated: false,
            },
        );
        state.commit(1);

        state.remove(2);
        assert_eq!(state.focused_id, Some(1));
    }

    #[test]
    fn parse_states_empty() {
        let states: Vec<u32> = parse_states(&[]).collect();
        assert!(states.is_empty());
    }

    #[test]
    fn parse_states_activated() {
        // Activated = 2, encoded as LE u32
        let raw = 2u32.to_le_bytes();
        let states: Vec<u32> = parse_states(&raw).collect();
        assert_eq!(states, vec![2]);
    }

    #[test]
    fn parse_states_multiple() {
        // Maximized (0), Activated (2), Fullscreen (3)
        let mut raw = Vec::new();
        raw.extend_from_slice(&0u32.to_le_bytes());
        raw.extend_from_slice(&2u32.to_le_bytes());
        raw.extend_from_slice(&3u32.to_le_bytes());

        let states: Vec<u32> = parse_states(&raw).collect();
        assert_eq!(states, vec![0, 2, 3]);
    }

    #[test]
    fn parse_states_trailing_bytes_ignored() {
        // 4 bytes + 2 trailing — chunks_exact ignores the remainder
        let mut raw = Vec::new();
        raw.extend_from_slice(&1u32.to_le_bytes());
        raw.extend_from_slice(&[0xFF, 0xFF]);

        let states: Vec<u32> = parse_states(&raw).collect();
        assert_eq!(states, vec![1]);
    }

    #[test]
    fn diagnostics_not_empty() {
        let diags = WlrForeignToplevelSource::get_diagnostics();
        assert!(diags.len() >= 2);
        assert!(diags[0].starts_with("WAYLAND_DISPLAY"));
        assert!(diags[1].starts_with("XDG_CURRENT_DESKTOP"));
    }
}
