//! Generic X11 focus detection backend.
//!
//! Monitors `_NET_ACTIVE_WINDOW` property changes on the root window to detect
//! focus changes. Works with any EWMH-compliant X11 window manager (i3, bspwm,
//! awesome, openbox, etc.).

use std::env;

use async_trait::async_trait;
use tokio::sync::mpsc;
use tracing::debug;
use tracing::info;
use tracing::trace;
use tracing::warn;
use x11rb::connection::Connection;
use x11rb::properties::WmClass;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::Atom;
use x11rb::protocol::xproto::AtomEnum;
use x11rb::protocol::xproto::ChangeWindowAttributesAux;
use x11rb::protocol::xproto::ConnectionExt;
use x11rb::protocol::xproto::EventMask;
use x11rb::protocol::xproto::Window;
use x11rb::rust_connection::RustConnection;

use super::FocusError;
use super::FocusEvent;
use super::FocusSource;

/// X11 focus source implementation.
///
/// Runs the blocking X11 event loop in a dedicated thread and receives
/// focus events via a channel.
pub struct X11Source {
    rx: mpsc::Receiver<FocusEvent>,
}

impl X11Source {
    /// Create a new X11 focus source.
    #[allow(clippy::unused_async)]
    pub async fn connect() -> Result<Self, FocusError> {
        // Verify DISPLAY is set before spawning
        if env::var("DISPLAY").is_err() {
            return Err(FocusError::EnvVarNotSet("DISPLAY".to_string()));
        }

        let (tx, rx) = mpsc::channel(32);

        // Spawn the blocking X11 event loop in a dedicated thread
        tokio::task::spawn_blocking(move || {
            if let Err(e) = run_x11_event_loop(&tx) {
                warn!("X11 event loop exited: {e}");
            }
        });

        info!("Connected to X11 display");
        Ok(Self { rx })
    }

    /// Get diagnostic information about the X11 environment.
    pub fn get_diagnostics() -> Vec<String> {
        let mut diags = Vec::new();

        match env::var("DISPLAY") {
            Ok(v) => diags.push(format!("DISPLAY={v}")),
            Err(_) => diags.push("DISPLAY: NOT SET".to_string()),
        }

        diags
    }
}

#[async_trait]
impl FocusSource for X11Source {
    async fn next_event(&mut self) -> Result<FocusEvent, FocusError> {
        self.rx
            .recv()
            .await
            .ok_or_else(|| FocusError::ConnectionFailed("X11 event loop closed".to_string()))
    }
}

/// Run the blocking X11 event loop, sending focus events to the channel.
fn run_x11_event_loop(tx: &mpsc::Sender<FocusEvent>) -> Result<(), FocusError> {
    let (conn, screen_num) = x11rb::connect(None)
        .map_err(|e| FocusError::ConnectionFailed(format!("X11 connect failed: {e}")))?;

    let screen = &conn.setup().roots[screen_num];
    let root = screen.root;

    // Intern the _NET_ACTIVE_WINDOW atom
    let net_active_window = intern_atom(&conn, b"_NET_ACTIVE_WINDOW")?;
    let net_wm_name = intern_atom(&conn, b"_NET_WM_NAME")?;
    let utf8_string = intern_atom(&conn, b"UTF8_STRING")?;

    // Subscribe to property changes on the root window
    let attrs = ChangeWindowAttributesAux::default().event_mask(EventMask::PROPERTY_CHANGE);
    conn.change_window_attributes(root, &attrs)
        .map_err(|e| FocusError::ConnectionFailed(format!("Failed to set event mask: {e}")))?;
    conn.flush()
        .map_err(|e| FocusError::ConnectionFailed(format!("Failed to flush: {e}")))?;

    // Read the initial active window
    if let Some(event) =
        read_active_window(&conn, root, net_active_window, net_wm_name, utf8_string)
    {
        debug!(
            "Initial focus: class={}, title={:?}",
            event.app_class, event.title
        );
        if tx.blocking_send(event).is_err() {
            return Ok(());
        }
    }

    // Event loop
    loop {
        let event = conn
            .wait_for_event()
            .map_err(|e| FocusError::ConnectionFailed(format!("X11 event error: {e}")))?;

        if let Event::PropertyNotify(prop_event) = event
            && prop_event.window == root
            && prop_event.atom == net_active_window
        {
            trace!("_NET_ACTIVE_WINDOW changed");

            if let Some(focus_event) =
                read_active_window(&conn, root, net_active_window, net_wm_name, utf8_string)
            {
                debug!(
                    "Focus changed: class={}, title={:?}, window_id={:?}",
                    focus_event.app_class, focus_event.title, focus_event.window_id
                );
                if tx.blocking_send(focus_event).is_err() {
                    // Receiver dropped, shut down
                    return Ok(());
                }
            }
        }
    }
}

/// Read the currently active window and build a `FocusEvent`.
fn read_active_window(
    conn: &RustConnection,
    root: Window,
    net_active_window: Atom,
    net_wm_name: Atom,
    utf8_string: Atom,
) -> Option<FocusEvent> {
    // Get the active window ID
    let reply = conn
        .get_property(false, root, net_active_window, AtomEnum::WINDOW, 0, 1)
        .ok()?
        .reply()
        .ok()?;

    let window_id = reply.value32()?.next()?;

    // Window ID 0 or None means no focused window
    if window_id == 0 {
        return None;
    }

    let app_class = read_wm_class(conn, window_id);
    let title = read_window_title(conn, window_id, net_wm_name, utf8_string);

    // Skip if we couldn't get a class (window may have been destroyed)
    let app_class = app_class?;
    if app_class.is_empty() {
        return None;
    }

    Some(FocusEvent::new(
        app_class,
        title,
        Some(format!("0x{window_id:x}")),
    ))
}

/// Read `WM_CLASS` from a window, returning the class (second) component.
fn read_wm_class(conn: &RustConnection, window: Window) -> Option<String> {
    let wm_class = WmClass::get(conn, window).ok()?.reply().ok()??;
    let class = std::str::from_utf8(wm_class.class()).ok()?;
    if class.is_empty() {
        None
    } else {
        Some(class.to_string())
    }
}

/// Read the window title, preferring `_NET_WM_NAME` (UTF-8) over `WM_NAME`.
fn read_window_title(
    conn: &RustConnection,
    window: Window,
    net_wm_name: Atom,
    utf8_string: Atom,
) -> Option<String> {
    // Try _NET_WM_NAME first (UTF-8)
    if let Some(title) = get_string_property(conn, window, net_wm_name, utf8_string)
        && !title.is_empty()
    {
        return Some(title);
    }

    // Fall back to WM_NAME (Latin-1)
    let reply = conn
        .get_property(
            false,
            window,
            AtomEnum::WM_NAME,
            AtomEnum::STRING,
            0,
            u32::MAX,
        )
        .ok()?
        .reply()
        .ok()?;

    let title = String::from_utf8_lossy(&reply.value).to_string();
    if title.is_empty() { None } else { Some(title) }
}

/// Get a UTF-8 string property from a window.
fn get_string_property(
    conn: &RustConnection,
    window: Window,
    property: Atom,
    prop_type: Atom,
) -> Option<String> {
    let reply = conn
        .get_property(false, window, property, prop_type, 0, u32::MAX)
        .ok()?
        .reply()
        .ok()?;

    if reply.value.is_empty() {
        return None;
    }

    Some(String::from_utf8_lossy(&reply.value).to_string())
}

/// Intern an X11 atom by name.
fn intern_atom(conn: &RustConnection, name: &[u8]) -> Result<Atom, FocusError> {
    conn.intern_atom(false, name)
        .map_err(|e| FocusError::ConnectionFailed(format!("Failed to intern atom: {e}")))?
        .reply()
        .map(|r| r.atom)
        .map_err(|e| FocusError::ConnectionFailed(format!("Atom reply error: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_diagnostics_reports_display() {
        let diags = X11Source::get_diagnostics();
        assert!(!diags.is_empty());
        assert!(diags[0].starts_with("DISPLAY"));
    }
}
