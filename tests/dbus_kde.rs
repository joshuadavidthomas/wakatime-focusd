//! Integration tests for the KDE backend with a private D-Bus session bus.
//!
//! Starts a private `dbus-daemon`, registers mock `KWin` D-Bus services
//! (`org.kde.kwin.Scripting` + script runner), connects the real `KdeSource`,
//! and calls `FocusChanged` on the daemon's `org.wakatime.focusd` receiver to
//! simulate `KWin` script callbacks.

use std::env;
use std::sync::Arc;
use std::time::Duration;

use dbus_launch::BusType;
use serial_test::serial;
use tokio::sync::Mutex;
use wakatime_focusd::backend::Backend;
use wakatime_focusd::backend::{self};
use zbus::Connection;
use zbus::connection::Builder;
use zbus::proxy;

const TEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Shared state tracking what the mock `KWin` scripting service received.
#[derive(Debug, Clone, Default)]
struct ScriptingState {
    loaded_scripts: Arc<Mutex<Vec<(String, String)>>>,
    next_script_id: Arc<Mutex<i32>>,
    unloaded: Arc<Mutex<Vec<String>>>,
    ran: Arc<Mutex<Vec<i32>>>,
}

/// Mock implementation of `org.kde.kwin.Scripting` at `/Scripting`.
struct MockKWinScripting {
    state: ScriptingState,
}

#[zbus::interface(name = "org.kde.kwin.Scripting")]
impl MockKWinScripting {
    async fn load_script(&self, path: String, name: String) -> zbus::fdo::Result<i32> {
        let mut loaded = self.state.loaded_scripts.lock().await;
        loaded.push((path, name));
        let mut id = self.state.next_script_id.lock().await;
        *id += 1;
        Ok(*id)
    }

    async fn unload_script(&self, name: String) -> zbus::fdo::Result<bool> {
        let mut unloaded = self.state.unloaded.lock().await;
        unloaded.push(name);
        Ok(true)
    }
}

/// Mock implementation of `org.kde.kwin.Script` at `/Scripting/Script{id}`.
struct MockKWinScript {
    id: i32,
    state: ScriptingState,
}

#[zbus::interface(name = "org.kde.kwin.Script")]
impl MockKWinScript {
    async fn run(&self) -> zbus::fdo::Result<()> {
        let mut ran = self.state.ran.lock().await;
        ran.push(self.id);
        Ok(())
    }
}

/// Proxy to call `FocusChanged` on the daemon's `org.wakatime.focusd` service.
#[proxy(
    interface = "org.wakatime.focusd.FocusReceiver",
    default_service = "org.wakatime.focusd",
    default_path = "/org/wakatime/focusd"
)]
trait FocusReceiverClient {
    fn focus_changed(&self, class: &str, title: &str, window_id: &str) -> zbus::Result<()>;
}

/// Start a private dbus-daemon and set `DBUS_SESSION_BUS_ADDRESS`.
async fn launch_private_bus() -> dbus_launch::Daemon {
    tokio::task::spawn_blocking(|| {
        dbus_launch::Launcher::daemon()
            .bus_type(BusType::Session)
            .launch()
            .expect("failed to launch dbus-daemon")
    })
    .await
    .expect("spawn_blocking panicked")
}

/// Register the mock `KWin` scripting service. Returns the connection (keep alive)
/// and the scripting state for verification.
async fn setup_mock_kwin(address: &str) -> (Connection, ScriptingState) {
    let state = ScriptingState {
        next_script_id: Arc::new(Mutex::new(0)),
        ..Default::default()
    };

    // The KDE backend loads script, gets ID, then calls Run on /Scripting/Script{id}.
    // Since next_script_id starts at 0 and increments before returning, the first
    // call returns 1. We need to pre-register the script object at /Scripting/Script1.
    let conn = Builder::address(address)
        .expect("invalid address")
        .name("org.kde.KWin")
        .expect("invalid service name")
        .serve_at(
            "/Scripting",
            MockKWinScripting {
                state: state.clone(),
            },
        )
        .expect("serve_at Scripting failed")
        .serve_at(
            "/Scripting/Script1",
            MockKWinScript {
                id: 1,
                state: state.clone(),
            },
        )
        .expect("serve_at Script1 failed")
        .build()
        .await
        .expect("failed to build connection");

    (conn, state)
}

/// Create a client connection to the private bus for calling `FocusChanged`.
async fn create_client(address: &str) -> FocusReceiverClientProxy<'static> {
    let conn = Builder::address(address)
        .expect("invalid address")
        .build()
        .await
        .expect("failed to build client connection");

    FocusReceiverClientProxy::new(&conn)
        .await
        .expect("failed to create proxy")
}

/// Connect the KDE backend and wait for it to be ready. Returns the source
/// and a client proxy to send `FocusChanged` calls.
async fn connect_kde(
    address: &str,
) -> (
    Box<dyn wakatime_focusd::backend::FocusSource>,
    FocusReceiverClientProxy<'static>,
) {
    let source = backend::connect(Backend::Kde).await.unwrap();

    // Small delay to let the D-Bus service fully register
    tokio::time::sleep(Duration::from_millis(50)).await;

    let client = create_client(address).await;
    (source, client)
}

#[tokio::test]
#[serial]
async fn mock_accepts_load_script_and_run() {
    let daemon = launch_private_bus().await;
    unsafe { env::set_var("DBUS_SESSION_BUS_ADDRESS", daemon.address()) };

    let (_kwin_conn, state) = setup_mock_kwin(daemon.address()).await;

    let _source = backend::connect(Backend::Kde).await.unwrap();

    // Verify the backend loaded and ran a script
    let loaded = state.loaded_scripts.lock().await;
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].1, "wakatime-focusd");

    let ran = state.ran.lock().await;
    assert_eq!(ran.len(), 1);
    assert_eq!(ran[0], 1);

    // Verify unload was called first (cleanup of previous instance)
    let unloaded = state.unloaded.lock().await;
    assert!(
        !unloaded.is_empty(),
        "Expected unload of previous script instance"
    );
    assert_eq!(unloaded[0], "wakatime-focusd");
}

#[tokio::test]
#[serial]
async fn focus_changed_events_flow_through() {
    let daemon = launch_private_bus().await;
    unsafe { env::set_var("DBUS_SESSION_BUS_ADDRESS", daemon.address()) };

    let (_kwin_conn, _state) = setup_mock_kwin(daemon.address()).await;
    let (mut source, client) = connect_kde(daemon.address()).await;

    client
        .focus_changed("kitty", "~/projects", "abc-123")
        .await
        .unwrap();

    let event = tokio::time::timeout(TEST_TIMEOUT, source.next_event())
        .await
        .expect("timed out")
        .unwrap();

    assert_eq!(event.app_class, "kitty");
    assert_eq!(event.title, Some("~/projects".to_string()));
    assert_eq!(event.window_id, Some("abc-123".to_string()));
}

#[tokio::test]
#[serial]
async fn multiple_focus_changes_in_sequence() {
    let daemon = launch_private_bus().await;
    unsafe { env::set_var("DBUS_SESSION_BUS_ADDRESS", daemon.address()) };

    let (_kwin_conn, _state) = setup_mock_kwin(daemon.address()).await;
    let (mut source, client) = connect_kde(daemon.address()).await;

    let apps = [
        ("Firefox", "GitHub", "win-1"),
        ("kitty", "~/projects", "win-2"),
        ("Slack", "#general", "win-3"),
        ("code", "main.rs", "win-4"),
    ];

    for (class, title, id) in &apps {
        client.focus_changed(class, title, id).await.unwrap();

        let event = tokio::time::timeout(TEST_TIMEOUT, source.next_event())
            .await
            .expect("timed out")
            .unwrap();

        assert_eq!(event.app_class, *class);
        assert_eq!(event.title, Some(title.to_string()));
        assert_eq!(event.window_id, Some(id.to_string()));
    }
}

#[tokio::test]
#[serial]
async fn empty_class_events_are_filtered() {
    let daemon = launch_private_bus().await;
    unsafe { env::set_var("DBUS_SESSION_BUS_ADDRESS", daemon.address()) };

    let (_kwin_conn, _state) = setup_mock_kwin(daemon.address()).await;
    let (mut source, client) = connect_kde(daemon.address()).await;

    // Send an empty class event (should be filtered)
    client.focus_changed("", "", "").await.unwrap();

    // Then send a real event
    client
        .focus_changed("Firefox", "GitHub", "win-1")
        .await
        .unwrap();

    // The first event we get should be Firefox, not the empty one
    let event = tokio::time::timeout(TEST_TIMEOUT, source.next_event())
        .await
        .expect("timed out")
        .unwrap();

    assert_eq!(event.app_class, "Firefox");
    assert_eq!(event.title, Some("GitHub".to_string()));
}

#[tokio::test]
#[serial]
async fn script_unload_called_before_load() {
    let daemon = launch_private_bus().await;
    unsafe { env::set_var("DBUS_SESSION_BUS_ADDRESS", daemon.address()) };

    let (_kwin_conn, state) = setup_mock_kwin(daemon.address()).await;

    let _source = backend::connect(Backend::Kde).await.unwrap();

    // The backend should unload any previous script before loading a new one.
    // Unload is called before load, so unloaded should have at least one entry.
    let unloaded = state.unloaded.lock().await;
    let loaded = state.loaded_scripts.lock().await;

    assert!(!unloaded.is_empty(), "Expected unload call");
    assert!(!loaded.is_empty(), "Expected load call");
    assert_eq!(unloaded[0], "wakatime-focusd");
    assert_eq!(loaded[0].1, "wakatime-focusd");
}
