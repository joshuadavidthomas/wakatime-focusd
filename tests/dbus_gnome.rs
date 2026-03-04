//! Integration tests for the GNOME backend with a private D-Bus session bus.
//!
//! Starts a private `dbus-daemon`, registers a mock `org.gnome.Shell.Introspect`
//! service, connects the real `GnomeSource` backend, and verifies `FocusEvent`
//! output.

use std::collections::HashMap;
use std::env;
use std::sync::Arc;
use std::time::Duration;

use dbus_launch::BusType;
use serial_test::serial;
use tokio::sync::Mutex;
use wakatime_focusd::backend::Backend;
use wakatime_focusd::backend::FocusError;
use wakatime_focusd::backend::{
    self,
};
use zbus::Connection;
use zbus::connection::Builder;
use zbus::object_server::InterfaceRef;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::OwnedValue;
use zbus::zvariant::Value;

const TEST_TIMEOUT: Duration = Duration::from_secs(5);
const SERVICE_NAME: &str = "org.gnome.Shell.Introspect";
const OBJECT_PATH: &str = "/org/gnome/Shell/Introspect";

type WindowMap = HashMap<u64, HashMap<String, OwnedValue>>;

/// Shared state for the mock GNOME Shell Introspect service.
#[derive(Debug, Clone)]
struct MockState {
    windows: Arc<Mutex<WindowMap>>,
}

impl MockState {
    fn new() -> Self {
        Self {
            windows: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

/// Mock implementation of `org.gnome.Shell.Introspect`.
struct MockIntrospect {
    state: MockState,
}

#[zbus::interface(name = "org.gnome.Shell.Introspect")]
impl MockIntrospect {
    #[zbus(name = "GetWindows")]
    async fn get_windows(&self) -> zbus::fdo::Result<WindowMap> {
        let windows = self.state.windows.lock().await;
        Ok(windows.clone())
    }

    #[zbus(signal)]
    async fn windows_changed(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

fn make_window(
    has_focus: bool,
    wm_class: &str,
    title: &str,
    app_id: Option<&str>,
) -> HashMap<String, OwnedValue> {
    let mut props = HashMap::new();
    props.insert(
        "has-focus".to_string(),
        Value::from(has_focus).try_into().unwrap(),
    );
    props.insert(
        "wm-class".to_string(),
        Value::from(wm_class).try_into().unwrap(),
    );
    props.insert("title".to_string(), Value::from(title).try_into().unwrap());
    if let Some(id) = app_id {
        props.insert("app-id".to_string(), Value::from(id).try_into().unwrap());
    }
    props
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

/// Register the mock Introspect service on the given bus and return the
/// connection (must be kept alive) and a handle for emitting signals.
async fn setup_mock_service(
    address: &str,
    state: &MockState,
) -> (Connection, InterfaceRef<MockIntrospect>) {
    let conn = Builder::address(address)
        .expect("invalid address")
        .name(SERVICE_NAME)
        .expect("invalid service name")
        .serve_at(
            OBJECT_PATH,
            MockIntrospect {
                state: state.clone(),
            },
        )
        .expect("serve_at failed")
        .build()
        .await
        .expect("failed to build connection");

    let iface_ref: InterfaceRef<MockIntrospect> = conn
        .object_server()
        .interface(OBJECT_PATH)
        .await
        .expect("interface not found");

    (conn, iface_ref)
}

async fn emit_windows_changed(iface_ref: &InterfaceRef<MockIntrospect>) {
    MockIntrospect::windows_changed(iface_ref.signal_emitter())
        .await
        .expect("failed to emit WindowsChanged");
}

#[tokio::test]
#[serial]
async fn get_windows_returns_focused_window() {
    let daemon = launch_private_bus().await;
    unsafe { env::set_var("DBUS_SESSION_BUS_ADDRESS", daemon.address()) };

    let state = MockState::new();
    {
        let mut windows = state.windows.lock().await;
        windows.insert(1, make_window(false, "Firefox", "Google", None));
        windows.insert(2, make_window(true, "kitty", "~/projects", None));
    }

    let (_conn, _iface_ref) = setup_mock_service(daemon.address(), &state).await;

    let mut source = backend::connect(Backend::Gnome).await.unwrap();
    let event = tokio::time::timeout(TEST_TIMEOUT, source.next_event())
        .await
        .expect("timed out")
        .unwrap();

    assert_eq!(event.app_class, "kitty");
    assert_eq!(event.title, Some("~/projects".to_string()));
    assert_eq!(event.window_id, Some("2".to_string()));
}

#[tokio::test]
#[serial]
async fn windows_changed_signal_triggers_focus_update() {
    let daemon = launch_private_bus().await;
    unsafe { env::set_var("DBUS_SESSION_BUS_ADDRESS", daemon.address()) };

    let state = MockState::new();
    {
        let mut windows = state.windows.lock().await;
        windows.insert(1, make_window(true, "kitty", "~/projects", None));
    }

    let (_conn, iface_ref) = setup_mock_service(daemon.address(), &state).await;

    let mut source = backend::connect(Backend::Gnome).await.unwrap();

    // Consume the initial focus event
    let event = tokio::time::timeout(TEST_TIMEOUT, source.next_event())
        .await
        .expect("timed out on initial event")
        .unwrap();
    assert_eq!(event.app_class, "kitty");

    // Change focus to Firefox
    {
        let mut windows = state.windows.lock().await;
        windows.insert(1, make_window(false, "kitty", "~/projects", None));
        windows.insert(2, make_window(true, "Firefox", "GitHub", None));
    }
    emit_windows_changed(&iface_ref).await;

    let event = tokio::time::timeout(TEST_TIMEOUT, source.next_event())
        .await
        .expect("timed out on focus change")
        .unwrap();

    assert_eq!(event.app_class, "Firefox");
    assert_eq!(event.title, Some("GitHub".to_string()));
    assert_eq!(event.window_id, Some("2".to_string()));
}

#[tokio::test]
#[serial]
async fn focus_dedup_unchanged_window() {
    let daemon = launch_private_bus().await;
    unsafe { env::set_var("DBUS_SESSION_BUS_ADDRESS", daemon.address()) };

    let state = MockState::new();
    {
        let mut windows = state.windows.lock().await;
        windows.insert(1, make_window(true, "kitty", "~/projects", None));
    }

    let (_conn, iface_ref) = setup_mock_service(daemon.address(), &state).await;

    let mut source = backend::connect(Backend::Gnome).await.unwrap();

    // Consume initial event
    let event = tokio::time::timeout(TEST_TIMEOUT, source.next_event())
        .await
        .expect("timed out")
        .unwrap();
    assert_eq!(event.app_class, "kitty");

    // Emit signal but don't change the focused window — should be deduped
    emit_windows_changed(&iface_ref).await;

    // Emit signal again with actual focus change to verify the source didn't
    // get stuck on the dedup
    {
        let mut windows = state.windows.lock().await;
        windows.insert(1, make_window(false, "kitty", "~/projects", None));
        windows.insert(2, make_window(true, "code", "main.rs", None));
    }
    emit_windows_changed(&iface_ref).await;

    let event = tokio::time::timeout(TEST_TIMEOUT, source.next_event())
        .await
        .expect("timed out waiting for real change")
        .unwrap();

    assert_eq!(event.app_class, "code");
    assert_eq!(event.title, Some("main.rs".to_string()));
}

#[tokio::test]
#[serial]
async fn multiple_windows_focus_switches() {
    let daemon = launch_private_bus().await;
    unsafe { env::set_var("DBUS_SESSION_BUS_ADDRESS", daemon.address()) };

    let state = MockState::new();
    {
        let mut windows = state.windows.lock().await;
        windows.insert(1, make_window(true, "Firefox", "Google", None));
        windows.insert(2, make_window(false, "kitty", "~/projects", None));
        windows.insert(3, make_window(false, "Slack", "general", None));
    }

    let (_conn, iface_ref) = setup_mock_service(daemon.address(), &state).await;

    let mut source = backend::connect(Backend::Gnome).await.unwrap();

    // Initial: Firefox focused
    let event = tokio::time::timeout(TEST_TIMEOUT, source.next_event())
        .await
        .expect("timed out")
        .unwrap();
    assert_eq!(event.app_class, "Firefox");

    // Switch to kitty
    {
        let mut windows = state.windows.lock().await;
        windows.insert(1, make_window(false, "Firefox", "Google", None));
        windows.insert(2, make_window(true, "kitty", "~/projects", None));
    }
    emit_windows_changed(&iface_ref).await;

    let event = tokio::time::timeout(TEST_TIMEOUT, source.next_event())
        .await
        .expect("timed out")
        .unwrap();
    assert_eq!(event.app_class, "kitty");

    // Switch to Slack
    {
        let mut windows = state.windows.lock().await;
        windows.insert(2, make_window(false, "kitty", "~/projects", None));
        windows.insert(3, make_window(true, "Slack", "#general", None));
    }
    emit_windows_changed(&iface_ref).await;

    let event = tokio::time::timeout(TEST_TIMEOUT, source.next_event())
        .await
        .expect("timed out")
        .unwrap();
    assert_eq!(event.app_class, "Slack");
    assert_eq!(event.title, Some("#general".to_string()));
}

#[tokio::test]
#[serial]
async fn empty_wm_class_falls_back_to_app_id() {
    let daemon = launch_private_bus().await;
    unsafe { env::set_var("DBUS_SESSION_BUS_ADDRESS", daemon.address()) };

    let state = MockState::new();
    {
        let mut windows = state.windows.lock().await;
        windows.insert(
            1,
            make_window(true, "", "Nautilus", Some("org.gnome.Nautilus")),
        );
    }

    let (_conn, _iface_ref) = setup_mock_service(daemon.address(), &state).await;

    let mut source = backend::connect(Backend::Gnome).await.unwrap();
    let event = tokio::time::timeout(TEST_TIMEOUT, source.next_event())
        .await
        .expect("timed out")
        .unwrap();

    assert_eq!(event.app_class, "org.gnome.Nautilus");
    assert_eq!(event.title, Some("Nautilus".to_string()));
}

#[tokio::test]
#[serial]
async fn connection_failure_when_service_unavailable() {
    let daemon = launch_private_bus().await;
    unsafe { env::set_var("DBUS_SESSION_BUS_ADDRESS", daemon.address()) };

    // Don't register any service — GnomeSource::connect() should fail
    // because GetWindows() verification call will fail.
    let result = backend::connect(Backend::Gnome).await;

    match result {
        Err(FocusError::ConnectionFailed(msg)) => {
            assert!(
                msg.contains("GetWindows"),
                "unexpected error message: {msg}"
            );
        }
        Ok(_) => panic!("Expected connection failure, but connect succeeded"),
        Err(e) => panic!("Expected ConnectionFailed, got: {e:?}"),
    }
}
