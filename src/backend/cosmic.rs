//! COSMIC desktop focus detection via the `zcosmic-toplevel-info` protocol.
//!
//! This backend is for the COSMIC desktop environment (default on Pop!_OS).
//! It uses the `zcosmic_toplevel_info_v1` protocol, which extends
//! `ext-foreign-toplevel-list-v1` with state tracking including the
//! `Activated` flag for focus detection.
//!
//! The Wayland event loop is synchronous, so it runs in a dedicated thread
//! via `spawn_blocking` and sends focus events through an `mpsc` channel.

use std::collections::HashMap;
use std::env;

use async_trait::async_trait;
use cosmic_protocols::toplevel_info::v1::client::zcosmic_toplevel_handle_v1;
use cosmic_protocols::toplevel_info::v1::client::zcosmic_toplevel_handle_v1::ZcosmicToplevelHandleV1;
use cosmic_protocols::toplevel_info::v1::client::zcosmic_toplevel_info_v1;
use cosmic_protocols::toplevel_info::v1::client::zcosmic_toplevel_info_v1::ZcosmicToplevelInfoV1;
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
use wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1;
use wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1;
use wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_list_v1;
use wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1;

use super::FocusError;
use super::FocusEvent;
use super::FocusSource;

/// The `Activated` state value in the COSMIC toplevel protocol.
const STATE_ACTIVATED: u32 = 2;

/// COSMIC focus source using `zcosmic-toplevel-info` + `ext-foreign-toplevel-list`.
///
/// Runs a blocking Wayland event loop in a dedicated thread and receives
/// focus events via a channel.
pub struct CosmicSource {
    rx: mpsc::Receiver<FocusEvent>,
}

impl CosmicSource {
    /// Connect to the Wayland display and bind the COSMIC toplevel protocols.
    #[allow(clippy::unused_async)]
    pub async fn connect() -> Result<Self, FocusError> {
        if env::var("WAYLAND_DISPLAY").is_err() {
            return Err(FocusError::EnvVarNotSet("WAYLAND_DISPLAY".to_string()));
        }

        verify_protocols()?;

        let (tx, rx) = mpsc::channel(32);

        tokio::task::spawn_blocking(move || {
            if let Err(e) = run_event_loop(&tx) {
                warn!("COSMIC event loop exited: {e}");
            }
        });

        info!("Connected to COSMIC desktop via zcosmic-toplevel-info");
        Ok(Self { rx })
    }

    /// Diagnostic information about the COSMIC environment.
    pub fn get_diagnostics() -> Vec<String> {
        let mut diags = Vec::new();

        match env::var("XDG_CURRENT_DESKTOP") {
            Ok(v) => diags.push(format!("XDG_CURRENT_DESKTOP={v}")),
            Err(_) => diags.push("XDG_CURRENT_DESKTOP: NOT SET".to_string()),
        }

        match env::var("WAYLAND_DISPLAY") {
            Ok(v) => diags.push(format!("WAYLAND_DISPLAY={v}")),
            Err(_) => diags.push("WAYLAND_DISPLAY: NOT SET".to_string()),
        }

        match env::var("XDG_SESSION_TYPE") {
            Ok(v) => diags.push(format!("XDG_SESSION_TYPE={v}")),
            Err(_) => diags.push("XDG_SESSION_TYPE: NOT SET".to_string()),
        }

        diags
    }
}

#[async_trait]
impl FocusSource for CosmicSource {
    async fn next_event(&mut self) -> Result<FocusEvent, FocusError> {
        self.rx
            .recv()
            .await
            .ok_or_else(|| FocusError::ConnectionFailed("COSMIC event loop closed".to_string()))
    }
}

/// Per-toplevel pending state accumulated between `Done` events.
#[derive(Debug, Clone, Default)]
struct WindowData {
    app_id: String,
    title: String,
    activated: bool,
}

/// Window tracking state, extracted for testability.
///
/// Tracks per-toplevel data and focus state. Used by `ToplevelState` at
/// runtime and directly in unit tests.
struct WindowTracker {
    /// Pending state for each toplevel, keyed by cosmic handle protocol ID.
    windows: HashMap<u32, WindowData>,
    /// The ID of the currently focused toplevel (after `Done`).
    focused_id: Option<u32>,
}

impl WindowTracker {
    fn new() -> Self {
        Self {
            windows: HashMap::new(),
            focused_id: None,
        }
    }

    /// Process a committed toplevel state update. Returns a `FocusEvent` if
    /// focus changed.
    fn commit(&mut self, id: u32) -> Option<FocusEvent> {
        let window = self.windows.get(&id)?;

        if window.activated {
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

/// Shared state for Wayland dispatch.
struct ToplevelState {
    tracker: WindowTracker,
    /// The bound `zcosmic_toplevel_info_v1` proxy, used to create cosmic
    /// handles via `get_cosmic_toplevel`.
    cosmic_info: ZcosmicToplevelInfoV1,
    tx: mpsc::Sender<FocusEvent>,
}

impl ToplevelState {
    fn new(cosmic_info: ZcosmicToplevelInfoV1, tx: mpsc::Sender<FocusEvent>) -> Self {
        Self {
            tracker: WindowTracker::new(),
            cosmic_info,
            tx,
        }
    }
}

/// Connect to the Wayland display and verify both protocol globals exist.
fn verify_protocols() -> Result<(), FocusError> {
    let conn = Connection::connect_to_env()
        .map_err(|e| FocusError::ConnectionFailed(format!("Wayland connect: {e}")))?;

    let (globals, _event_queue) = registry_queue_init::<ProbeState>(&conn)
        .map_err(|e| FocusError::ConnectionFailed(format!("Registry init: {e}")))?;

    let global_list = globals.contents().clone_list();

    let has_ext_list = global_list
        .iter()
        .any(|g| g.interface == ExtForeignToplevelListV1::interface().name);

    let has_cosmic_info = global_list
        .iter()
        .any(|g| g.interface == ZcosmicToplevelInfoV1::interface().name);

    if !has_ext_list {
        return Err(FocusError::ConnectionFailed(
            "Compositor does not support ext-foreign-toplevel-list-v1".to_string(),
        ));
    }

    if !has_cosmic_info {
        return Err(FocusError::ConnectionFailed(
            "Compositor does not support zcosmic-toplevel-info-v1".to_string(),
        ));
    }

    Ok(())
}

/// Minimal state used only to probe for the protocol globals.
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

    let _ext_list: ExtForeignToplevelListV1 = globals.bind(&qh, 1..=1, ()).map_err(|e| {
        FocusError::ConnectionFailed(format!("Bind ext-foreign-toplevel-list: {e}"))
    })?;

    let cosmic_info: ZcosmicToplevelInfoV1 = globals
        .bind(&qh, 2..=3, ())
        .map_err(|e| FocusError::ConnectionFailed(format!("Bind zcosmic-toplevel-info: {e}")))?;

    let mut state = ToplevelState::new(cosmic_info, tx.clone());

    event_queue
        .roundtrip(&mut state)
        .map_err(|e| FocusError::ConnectionFailed(format!("Initial roundtrip: {e}")))?;

    loop {
        event_queue
            .blocking_dispatch(&mut state)
            .map_err(|e| FocusError::ConnectionFailed(format!("Dispatch error: {e}")))?;
    }
}

// Dispatch: ext-foreign-toplevel-list (creates ext handles, we upgrade to cosmic handles)
impl Dispatch<ExtForeignToplevelListV1, ()> for ToplevelState {
    fn event(
        state: &mut Self,
        _proxy: &ExtForeignToplevelListV1,
        event: ext_foreign_toplevel_list_v1::Event,
        _data: &(),
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            ext_foreign_toplevel_list_v1::Event::Toplevel { toplevel } => {
                let cosmic_handle = state.cosmic_info.get_cosmic_toplevel(&toplevel, qh, ());
                let id = cosmic_handle.id().protocol_id();
                trace!("New COSMIC toplevel: {id}");
                state.tracker.windows.insert(id, WindowData::default());
            }
            ext_foreign_toplevel_list_v1::Event::Finished => {
                warn!("Foreign toplevel list finished (compositor shutting down?)");
            }
            _ => {}
        }
    }

    wayland_client::event_created_child!(Self, ExtForeignToplevelListV1, [
        ext_foreign_toplevel_list_v1::EVT_TOPLEVEL_OPCODE => (ExtForeignToplevelHandleV1, ()),
    ]);
}

// Dispatch: ext-foreign-toplevel-handle (no-op — we use cosmic handle events instead)
impl Dispatch<ExtForeignToplevelHandleV1, ()> for ToplevelState {
    fn event(
        _state: &mut Self,
        _proxy: &ExtForeignToplevelHandleV1,
        _event: ext_foreign_toplevel_handle_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

// Dispatch: zcosmic-toplevel-info (manager-level events)
impl Dispatch<ZcosmicToplevelInfoV1, ()> for ToplevelState {
    fn event(
        _state: &mut Self,
        _proxy: &ZcosmicToplevelInfoV1,
        event: zcosmic_toplevel_info_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let zcosmic_toplevel_info_v1::Event::Finished = event {
            warn!("COSMIC toplevel info finished (compositor shutting down?)");
        }
    }

    wayland_client::event_created_child!(Self, ZcosmicToplevelInfoV1, [
        zcosmic_toplevel_info_v1::EVT_TOPLEVEL_OPCODE => (ZcosmicToplevelHandleV1, ()),
    ]);
}

// Dispatch: per-toplevel cosmic handle events
impl Dispatch<ZcosmicToplevelHandleV1, ()> for ToplevelState {
    fn event(
        state: &mut Self,
        proxy: &ZcosmicToplevelHandleV1,
        event: zcosmic_toplevel_handle_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        let id = proxy.id().protocol_id();

        match event {
            zcosmic_toplevel_handle_v1::Event::Title { title } => {
                trace!("Title changed for {id}: {title}");
                if let Some(w) = state.tracker.windows.get_mut(&id) {
                    w.title = title;
                }
            }
            zcosmic_toplevel_handle_v1::Event::AppId { app_id } => {
                trace!("AppId changed for {id}: {app_id}");
                if let Some(w) = state.tracker.windows.get_mut(&id) {
                    w.app_id = app_id;
                }
            }
            zcosmic_toplevel_handle_v1::Event::State { state: raw_state } => {
                let activated = parse_states(&raw_state).any(|s| s == STATE_ACTIVATED);
                trace!("State changed for {id}: activated={activated}");
                if let Some(w) = state.tracker.windows.get_mut(&id) {
                    w.activated = activated;
                }
            }
            zcosmic_toplevel_handle_v1::Event::Done => {
                trace!("Done for {id}");
                if let Some(event) = state.tracker.commit(id)
                    && state.tx.blocking_send(event).is_err()
                {
                    warn!("Receiver dropped, stopping event loop");
                }
            }
            zcosmic_toplevel_handle_v1::Event::Closed => {
                trace!("Closed: {id}");
                state.tracker.remove(id);
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

    fn make_tracker() -> WindowTracker {
        WindowTracker::new()
    }

    #[test]
    fn commit_activated_produces_focus_event() {
        let mut tracker = make_tracker();
        tracker.windows.insert(
            1,
            WindowData {
                app_id: "firefox".to_string(),
                title: "Mozilla Firefox".to_string(),
                activated: true,
            },
        );

        let event = tracker.commit(1).expect("should produce event");
        assert_eq!(event.app_class, "firefox");
        assert_eq!(event.title, Some("Mozilla Firefox".to_string()));
        assert_eq!(event.window_id, Some("1".to_string()));
        assert_eq!(tracker.focused_id, Some(1));
    }

    #[test]
    fn commit_not_activated_no_event() {
        let mut tracker = make_tracker();
        tracker.windows.insert(
            1,
            WindowData {
                app_id: "firefox".to_string(),
                title: "Mozilla Firefox".to_string(),
                activated: false,
            },
        );

        assert!(tracker.commit(1).is_none());
        assert_eq!(tracker.focused_id, None);
    }

    #[test]
    fn commit_same_focus_no_duplicate() {
        let mut tracker = make_tracker();
        tracker.windows.insert(
            1,
            WindowData {
                app_id: "firefox".to_string(),
                title: "Tab".to_string(),
                activated: true,
            },
        );

        tracker
            .commit(1)
            .expect("first commit should produce event");
        assert!(tracker.commit(1).is_none(), "second commit should be None");
    }

    #[test]
    fn commit_focus_switch() {
        let mut tracker = make_tracker();
        tracker.windows.insert(
            1,
            WindowData {
                app_id: "firefox".to_string(),
                title: String::new(),
                activated: true,
            },
        );
        tracker.windows.insert(
            2,
            WindowData {
                app_id: "alacritty".to_string(),
                title: "~".to_string(),
                activated: false,
            },
        );

        tracker.commit(1).expect("firefox gets focus");

        tracker.windows.get_mut(&1).unwrap().activated = false;
        tracker.windows.get_mut(&2).unwrap().activated = true;

        assert!(
            tracker.commit(1).is_none(),
            "deactivation produces no event"
        );
        assert_eq!(tracker.focused_id, None);

        let event = tracker.commit(2).expect("alacritty gets focus");
        assert_eq!(event.app_class, "alacritty");
        assert_eq!(tracker.focused_id, Some(2));
    }

    #[test]
    fn commit_empty_app_id_no_event() {
        let mut tracker = make_tracker();
        tracker.windows.insert(
            1,
            WindowData {
                app_id: String::new(),
                title: "untitled".to_string(),
                activated: true,
            },
        );

        assert!(tracker.commit(1).is_none());
    }

    #[test]
    fn commit_empty_title_becomes_none() {
        let mut tracker = make_tracker();
        tracker.windows.insert(
            1,
            WindowData {
                app_id: "foot".to_string(),
                title: String::new(),
                activated: true,
            },
        );

        let event = tracker.commit(1).expect("should produce event");
        assert_eq!(event.title, None);
    }

    #[test]
    fn commit_unknown_id_returns_none() {
        let mut tracker = make_tracker();
        assert!(tracker.commit(42).is_none());
    }

    #[test]
    fn remove_clears_focus_if_focused() {
        let mut tracker = make_tracker();
        tracker.windows.insert(
            1,
            WindowData {
                app_id: "firefox".to_string(),
                title: String::new(),
                activated: true,
            },
        );
        tracker.commit(1);
        assert_eq!(tracker.focused_id, Some(1));

        tracker.remove(1);
        assert_eq!(tracker.focused_id, None);
        assert!(!tracker.windows.contains_key(&1));
    }

    #[test]
    fn remove_other_window_keeps_focus() {
        let mut tracker = make_tracker();
        tracker.windows.insert(
            1,
            WindowData {
                app_id: "firefox".to_string(),
                title: String::new(),
                activated: true,
            },
        );
        tracker.windows.insert(
            2,
            WindowData {
                app_id: "alacritty".to_string(),
                title: String::new(),
                activated: false,
            },
        );
        tracker.commit(1);

        tracker.remove(2);
        assert_eq!(tracker.focused_id, Some(1));
    }

    #[test]
    fn parse_states_empty() {
        let states: Vec<u32> = parse_states(&[]).collect();
        assert!(states.is_empty());
    }

    #[test]
    fn parse_states_activated() {
        let raw = 2u32.to_le_bytes();
        let states: Vec<u32> = parse_states(&raw).collect();
        assert_eq!(states, vec![2]);
    }

    #[test]
    fn parse_states_multiple() {
        let mut raw = Vec::new();
        raw.extend_from_slice(&0u32.to_le_bytes());
        raw.extend_from_slice(&2u32.to_le_bytes());
        raw.extend_from_slice(&3u32.to_le_bytes());

        let states: Vec<u32> = parse_states(&raw).collect();
        assert_eq!(states, vec![0, 2, 3]);
    }

    #[test]
    fn parse_states_trailing_bytes_ignored() {
        let mut raw = Vec::new();
        raw.extend_from_slice(&1u32.to_le_bytes());
        raw.extend_from_slice(&[0xFF, 0xFF]);

        let states: Vec<u32> = parse_states(&raw).collect();
        assert_eq!(states, vec![1]);
    }

    #[test]
    fn diagnostics_not_empty() {
        let diags = CosmicSource::get_diagnostics();
        assert!(diags.len() >= 3);
        assert!(diags[0].starts_with("XDG_CURRENT_DESKTOP"));
        assert!(diags[1].starts_with("WAYLAND_DISPLAY"));
        assert!(diags[2].starts_with("XDG_SESSION_TYPE"));
    }
}
