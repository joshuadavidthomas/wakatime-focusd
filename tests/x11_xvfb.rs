//! Integration tests for the X11 backend against a virtual X11 display (Xvfb).
//!
//! Starts a headless Xvfb server, creates X windows with `WM_CLASS` and
//! `_NET_WM_NAME` properties, sets `_NET_ACTIVE_WINDOW` on the root window,
//! and verifies `FocusEvent` output from the real X11 backend.

use std::env;
use std::process::Child;
use std::process::Command;
use std::time::Duration;

use serial_test::serial;
use wakatime_focusd::backend::Backend;
use wakatime_focusd::backend::{
    self,
};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::AtomEnum;
use x11rb::protocol::xproto::ConnectionExt;
use x11rb::protocol::xproto::PropMode;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as WrapperConnectionExt;

const TEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Find a free display number and start Xvfb on it.
///
/// Returns `(Child, display_string)` — the `Child` must be kept alive for the
/// duration of the test. The display string is e.g. `:99`.
fn start_xvfb() -> (Child, String) {
    // Try display numbers starting from 99 to avoid conflicts
    for display_num in 99..120 {
        let display = format!(":{display_num}");
        let lock_file = format!("/tmp/.X{display_num}-lock");

        // Skip if lock file exists (display in use)
        if std::path::Path::new(&lock_file).exists() {
            continue;
        }

        let child = Command::new("Xvfb")
            .arg(&display)
            .arg("-screen")
            .arg("0")
            .arg("1024x768x24")
            .arg("-nolisten")
            .arg("tcp")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();

        if let Ok(child) = child {
            // Give Xvfb time to start
            std::thread::sleep(Duration::from_millis(500));
            return (child, display);
        }
    }
    panic!("Could not find a free display number for Xvfb");
}

/// Helper that connects to the test X display and provides utilities for
/// creating windows and setting properties.
struct XTestHelper {
    conn: RustConnection,
    root: u32,
    net_active_window: u32,
    net_wm_name: u32,
    utf8_string: u32,
}

impl XTestHelper {
    fn connect(display: &str) -> Self {
        let (conn, screen_num) =
            RustConnection::connect(Some(display)).expect("Failed to connect to Xvfb");
        let screen = &conn.setup().roots[screen_num];
        let root = screen.root;

        let net_active_window = conn
            .intern_atom(false, b"_NET_ACTIVE_WINDOW")
            .unwrap()
            .reply()
            .unwrap()
            .atom;
        let net_wm_name = conn
            .intern_atom(false, b"_NET_WM_NAME")
            .unwrap()
            .reply()
            .unwrap()
            .atom;
        let utf8_string = conn
            .intern_atom(false, b"UTF8_STRING")
            .unwrap()
            .reply()
            .unwrap()
            .atom;

        Self {
            conn,
            root,
            net_active_window,
            net_wm_name,
            utf8_string,
        }
    }

    /// Create an X window with the given `WM_CLASS` and `_NET_WM_NAME`.
    fn create_window(&self, instance: &str, class: &str, title: &str) -> u32 {
        let wid = self.conn.generate_id().unwrap();
        self.conn
            .create_window(
                x11rb::COPY_DEPTH_FROM_PARENT,
                wid,
                self.root,
                0,
                0,
                100,
                100,
                0,
                x11rb::protocol::xproto::WindowClass::INPUT_OUTPUT,
                0,
                &x11rb::protocol::xproto::CreateWindowAux::default(),
            )
            .unwrap();

        // Set WM_CLASS: "instance\0class\0"
        let wm_class_value = format!("{instance}\0{class}\0");
        self.conn
            .change_property8(
                PropMode::REPLACE,
                wid,
                AtomEnum::WM_CLASS,
                AtomEnum::STRING,
                wm_class_value.as_bytes(),
            )
            .unwrap();

        // Set _NET_WM_NAME (UTF-8 title)
        self.conn
            .change_property8(
                PropMode::REPLACE,
                wid,
                self.net_wm_name,
                self.utf8_string,
                title.as_bytes(),
            )
            .unwrap();

        self.conn.flush().unwrap();
        wid
    }

    /// Create a window with only `WM_NAME` (Latin-1), no `_NET_WM_NAME`.
    fn create_window_legacy_title(&self, instance: &str, class: &str, title: &str) -> u32 {
        let wid = self.conn.generate_id().unwrap();
        self.conn
            .create_window(
                x11rb::COPY_DEPTH_FROM_PARENT,
                wid,
                self.root,
                0,
                0,
                100,
                100,
                0,
                x11rb::protocol::xproto::WindowClass::INPUT_OUTPUT,
                0,
                &x11rb::protocol::xproto::CreateWindowAux::default(),
            )
            .unwrap();

        // Set WM_CLASS
        let wm_class_value = format!("{instance}\0{class}\0");
        self.conn
            .change_property8(
                PropMode::REPLACE,
                wid,
                AtomEnum::WM_CLASS,
                AtomEnum::STRING,
                wm_class_value.as_bytes(),
            )
            .unwrap();

        // Set WM_NAME (Latin-1 / STRING type) only — no _NET_WM_NAME
        self.conn
            .change_property8(
                PropMode::REPLACE,
                wid,
                AtomEnum::WM_NAME,
                AtomEnum::STRING,
                title.as_bytes(),
            )
            .unwrap();

        self.conn.flush().unwrap();
        wid
    }

    /// Set `_NET_ACTIVE_WINDOW` on the root window to the given window ID,
    /// which triggers a `PropertyNotify` event that the X11 backend listens for.
    fn set_active_window(&self, window_id: u32) {
        self.conn
            .change_property32(
                PropMode::REPLACE,
                self.root,
                self.net_active_window,
                AtomEnum::WINDOW,
                &[window_id],
            )
            .unwrap();
        self.conn.flush().unwrap();
    }

    /// Destroy a window.
    fn destroy_window(&self, window_id: u32) {
        self.conn.destroy_window(window_id).unwrap();
        self.conn.flush().unwrap();
    }
}

/// Guard that stops Xvfb and cleans up env vars on drop.
struct XvfbGuard {
    child: Child,
}

impl Drop for XvfbGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn setup() -> (XvfbGuard, String) {
    let (child, display) = start_xvfb();

    unsafe {
        env::set_var("DISPLAY", &display);
        // Clear Wayland env vars so auto-detect falls through to X11
        env::remove_var("HYPRLAND_INSTANCE_SIGNATURE");
        env::remove_var("HYPRLAND_CMD");
        env::remove_var("SWAYSOCK");
        env::remove_var("NIRI_SOCKET");
        env::remove_var("XDG_CURRENT_DESKTOP");
    }

    (XvfbGuard { child }, display)
}

#[tokio::test]
#[serial]
async fn basic_focus_event() {
    let (_guard, display) = setup();
    let helper = XTestHelper::connect(&display);

    let win = helper.create_window("firefox", "Firefox", "Mozilla Firefox");

    // Connect the backend (it reads the initial active window)
    // Set active window before connecting so the backend picks it up
    helper.set_active_window(win);

    let mut source = backend::connect(Backend::X11).await.unwrap();

    let event = tokio::time::timeout(TEST_TIMEOUT, source.next_event())
        .await
        .expect("timed out")
        .unwrap();

    assert_eq!(event.app_class, "Firefox");
    assert_eq!(event.title, Some("Mozilla Firefox".to_string()));
    assert_eq!(event.window_id, Some(format!("0x{win:x}")));
}

#[tokio::test]
#[serial]
async fn switch_focus_between_windows() {
    let (_guard, display) = setup();
    let helper = XTestHelper::connect(&display);

    let win_a = helper.create_window("firefox", "Firefox", "Tab 1");
    let win_b = helper.create_window("code", "Code", "main.rs");

    // Start with window A focused
    helper.set_active_window(win_a);

    let mut source = backend::connect(Backend::X11).await.unwrap();

    let event = tokio::time::timeout(TEST_TIMEOUT, source.next_event())
        .await
        .expect("timed out")
        .unwrap();
    assert_eq!(event.app_class, "Firefox");
    assert_eq!(event.title, Some("Tab 1".to_string()));

    // Small delay to ensure backend is in the event loop before we change properties
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Switch focus to window B
    helper.set_active_window(win_b);

    let event = tokio::time::timeout(TEST_TIMEOUT, source.next_event())
        .await
        .expect("timed out")
        .unwrap();
    assert_eq!(event.app_class, "Code");
    assert_eq!(event.title, Some("main.rs".to_string()));
    assert_eq!(event.window_id, Some(format!("0x{win_b:x}")));
}

#[tokio::test]
#[serial]
async fn window_destroyed_between_focus_and_read() {
    let (_guard, display) = setup();
    let helper = XTestHelper::connect(&display);

    let win_ephemeral = helper.create_window("popup", "Popup", "Gone soon");
    let win_stable = helper.create_window("kitty", "kitty", "Terminal");

    // Start with no active window
    helper.set_active_window(0);

    let mut source = backend::connect(Backend::X11).await.unwrap();

    // Small delay to let backend enter event loop
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Destroy the window, then set it as active — simulates a race where
    // the window is destroyed between the PropertyNotify and the property read.
    helper.destroy_window(win_ephemeral);
    // Small delay to ensure destroy is processed
    tokio::time::sleep(Duration::from_millis(50)).await;
    helper.set_active_window(win_ephemeral);

    // Then set a valid window — the backend should skip the destroyed one
    tokio::time::sleep(Duration::from_millis(100)).await;
    helper.set_active_window(win_stable);

    let event = tokio::time::timeout(TEST_TIMEOUT, source.next_event())
        .await
        .expect("timed out")
        .unwrap();
    assert_eq!(event.app_class, "kitty");
    assert_eq!(event.title, Some("Terminal".to_string()));
}

#[tokio::test]
#[serial]
async fn net_wm_name_preferred_over_wm_name() {
    let (_guard, display) = setup();
    let helper = XTestHelper::connect(&display);

    // Create a window with both _NET_WM_NAME and WM_NAME
    let wid = helper.conn.generate_id().unwrap();
    helper
        .conn
        .create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            wid,
            helper.root,
            0,
            0,
            100,
            100,
            0,
            x11rb::protocol::xproto::WindowClass::INPUT_OUTPUT,
            0,
            &x11rb::protocol::xproto::CreateWindowAux::default(),
        )
        .unwrap();

    // WM_CLASS
    helper
        .conn
        .change_property8(
            PropMode::REPLACE,
            wid,
            AtomEnum::WM_CLASS,
            AtomEnum::STRING,
            b"vim\0Vim\0",
        )
        .unwrap();

    // _NET_WM_NAME (UTF-8) — this should be preferred
    helper
        .conn
        .change_property8(
            PropMode::REPLACE,
            wid,
            helper.net_wm_name,
            helper.utf8_string,
            "UTF-8 Title ✓".as_bytes(),
        )
        .unwrap();

    // WM_NAME (Latin-1) — fallback, should NOT be used
    helper
        .conn
        .change_property8(
            PropMode::REPLACE,
            wid,
            AtomEnum::WM_NAME,
            AtomEnum::STRING,
            b"Latin1 Title",
        )
        .unwrap();

    helper.conn.flush().unwrap();
    helper.set_active_window(wid);

    let mut source = backend::connect(Backend::X11).await.unwrap();

    let event = tokio::time::timeout(TEST_TIMEOUT, source.next_event())
        .await
        .expect("timed out")
        .unwrap();
    assert_eq!(event.app_class, "Vim");
    assert_eq!(event.title, Some("UTF-8 Title ✓".to_string()));
}

#[tokio::test]
#[serial]
async fn wm_name_fallback_when_no_net_wm_name() {
    let (_guard, display) = setup();
    let helper = XTestHelper::connect(&display);

    let win = helper.create_window_legacy_title("xterm", "XTerm", "Legacy Terminal");
    helper.set_active_window(win);

    let mut source = backend::connect(Backend::X11).await.unwrap();

    let event = tokio::time::timeout(TEST_TIMEOUT, source.next_event())
        .await
        .expect("timed out")
        .unwrap();
    assert_eq!(event.app_class, "XTerm");
    assert_eq!(event.title, Some("Legacy Terminal".to_string()));
}

#[tokio::test]
#[serial]
async fn no_active_window_emits_nothing() {
    let (_guard, display) = setup();
    let helper = XTestHelper::connect(&display);

    let win = helper.create_window("kitty", "kitty", "Terminal");

    // Set active window to 0 (no focus)
    helper.set_active_window(0);

    let mut source = backend::connect(Backend::X11).await.unwrap();

    // The backend should not emit an event for window ID 0.
    // Give it some time, then set a real window to verify the backend is working.
    tokio::time::sleep(Duration::from_millis(300)).await;

    helper.set_active_window(win);

    let event = tokio::time::timeout(TEST_TIMEOUT, source.next_event())
        .await
        .expect("timed out")
        .unwrap();

    // The first event we get should be for the real window, not for ID 0
    assert_eq!(event.app_class, "kitty");
    assert_eq!(event.title, Some("Terminal".to_string()));
}
