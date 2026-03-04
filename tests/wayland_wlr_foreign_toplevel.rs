//! Integration tests for the wlr-foreign-toplevel backend against a mock
//! Wayland compositor.
//!
//! Starts a minimal Wayland server that advertises
//! `zwlr_foreign_toplevel_manager_v1`, creates toplevel handles with focus
//! state, and verifies `FocusEvent` output from the real backend.

use std::env;
use std::os::fd::AsFd;
use std::os::fd::AsRawFd;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;

use serial_test::serial;
use wakatime_focusd::backend::Backend;
use wakatime_focusd::backend::{
    self,
};
use wayland_protocols_wlr::foreign_toplevel::v1::server::zwlr_foreign_toplevel_handle_v1;
use wayland_protocols_wlr::foreign_toplevel::v1::server::zwlr_foreign_toplevel_handle_v1::ZwlrForeignToplevelHandleV1;
use wayland_protocols_wlr::foreign_toplevel::v1::server::zwlr_foreign_toplevel_manager_v1;
use wayland_protocols_wlr::foreign_toplevel::v1::server::zwlr_foreign_toplevel_manager_v1::ZwlrForeignToplevelManagerV1;
use wayland_server::Client;
use wayland_server::DataInit;
use wayland_server::Dispatch;
use wayland_server::DisplayHandle;
use wayland_server::GlobalDispatch;
use wayland_server::ListeningSocket;
use wayland_server::New;
use wayland_server::Resource;
use wayland_server::protocol::wl_registry;

const TEST_TIMEOUT: Duration = Duration::from_secs(5);

// Toplevel descriptor: what to send when the manager is bound.
struct ToplevelDesc {
    app_id: String,
    title: String,
    activated: bool,
}

// Mock compositor state
struct MockCompositor {
    toplevels_to_send: Vec<ToplevelDesc>,
}

// Required: registry dispatch
impl Dispatch<wl_registry::WlRegistry, ()> for MockCompositor {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &wl_registry::WlRegistry,
        _request: wl_registry::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
    }
}

// When the client binds the manager: create toplevels and send events immediately
impl GlobalDispatch<ZwlrForeignToplevelManagerV1, ()> for MockCompositor {
    fn bind(
        state: &mut Self,
        dh: &DisplayHandle,
        client: &Client,
        resource: New<ZwlrForeignToplevelManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        let manager = data_init.init(resource, ());

        for desc in &state.toplevels_to_send {
            let handle: ZwlrForeignToplevelHandleV1 = client
                .create_resource::<ZwlrForeignToplevelHandleV1, (), MockCompositor>(
                    dh,
                    manager.version(),
                    (),
                )
                .unwrap();

            manager.toplevel(&handle);
            handle.title(desc.title.clone());
            handle.app_id(desc.app_id.clone());

            if desc.activated {
                handle.state(activated_state_bytes());
            } else {
                handle.state(Vec::new());
            }

            handle.done();
        }
    }
}

impl Dispatch<ZwlrForeignToplevelManagerV1, ()> for MockCompositor {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &ZwlrForeignToplevelManagerV1,
        _request: zwlr_foreign_toplevel_manager_v1::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
    }
}

impl Dispatch<ZwlrForeignToplevelHandleV1, ()> for MockCompositor {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &ZwlrForeignToplevelHandleV1,
        _request: zwlr_foreign_toplevel_handle_v1::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
    }
}

/// Encode Activated state (2) as LE u32 bytes.
fn activated_state_bytes() -> Vec<u8> {
    2u32.to_le_bytes().to_vec()
}

/// Clear all compositor-specific env vars and set up a mock Wayland display.
fn setup(
    toplevels: Vec<ToplevelDesc>,
) -> (
    tempfile::TempDir,
    ListeningSocket,
    wayland_server::Display<MockCompositor>,
    MockCompositor,
) {
    let tmp = tempfile::TempDir::new().unwrap();

    // SAFETY: tests run serially so env var mutation is safe.
    unsafe {
        for var in [
            "HYPRLAND_INSTANCE_SIGNATURE",
            "HYPRLAND_CMD",
            "SWAYSOCK",
            "NIRI_SOCKET",
            "XDG_CURRENT_DESKTOP",
        ] {
            env::remove_var(var);
        }

        env::set_var("XDG_RUNTIME_DIR", tmp.path());
        env::set_var("WAYLAND_DISPLAY", "wayland-test-focusd");
    }

    let socket = ListeningSocket::bind("wayland-test-focusd").unwrap();
    let display = wayland_server::Display::<MockCompositor>::new().unwrap();
    let comp = MockCompositor {
        toplevels_to_send: toplevels,
    };

    (tmp, socket, display, comp)
}

/// Run the mock server dispatch loop.
fn run_server(
    socket: &ListeningSocket,
    mut display: wayland_server::Display<MockCompositor>,
    mut comp: MockCompositor,
    stop: &Arc<AtomicBool>,
) {
    let mut dh = display.handle();
    let _global = dh.create_global::<MockCompositor, ZwlrForeignToplevelManagerV1, ()>(3, ());

    while !stop.load(Ordering::Relaxed) {
        if let Some(stream) = socket.accept().unwrap() {
            dh.insert_client(stream, Arc::new(())).unwrap();
        }

        let fd = display.backend().poll_fd().as_fd().as_raw_fd();
        let mut pollfd = [libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        }];
        unsafe {
            libc::poll(pollfd.as_mut_ptr(), 1, 50);
        }

        display.dispatch_clients(&mut comp).unwrap();
        display.flush_clients().unwrap();
    }
}

#[tokio::test]
#[serial]
async fn basic_focus_event() {
    let toplevels = vec![ToplevelDesc {
        app_id: "firefox".to_string(),
        title: "Mozilla Firefox".to_string(),
        activated: true,
    }];

    let (_tmp, socket, display, comp) = setup(toplevels);
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();

    let server = tokio::task::spawn_blocking(move || {
        run_server(&socket, display, comp, &stop2);
    });

    let mut source = backend::connect(Backend::WlrForeignToplevel).await.unwrap();

    let event = tokio::time::timeout(TEST_TIMEOUT, source.next_event())
        .await
        .expect("timed out waiting for event")
        .expect("next_event failed");

    assert_eq!(event.app_class, "firefox");
    assert_eq!(event.title, Some("Mozilla Firefox".to_string()));

    stop.store(true, Ordering::Relaxed);
    let _ = server.await;
}

#[tokio::test]
#[serial]
async fn unfocused_window_produces_no_event() {
    let toplevels = vec![ToplevelDesc {
        app_id: "firefox".to_string(),
        title: "Mozilla Firefox".to_string(),
        activated: false,
    }];

    let (_tmp, socket, display, comp) = setup(toplevels);
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();

    let server = tokio::task::spawn_blocking(move || {
        run_server(&socket, display, comp, &stop2);
    });

    let mut source = backend::connect(Backend::WlrForeignToplevel).await.unwrap();

    let result = tokio::time::timeout(Duration::from_millis(500), source.next_event()).await;
    assert!(result.is_err(), "expected timeout, got an event");

    stop.store(true, Ordering::Relaxed);
    let _ = server.await;
}

#[tokio::test]
#[serial]
async fn multiple_toplevels_only_focused_emits_event() {
    let toplevels = vec![
        ToplevelDesc {
            app_id: "alacritty".to_string(),
            title: "~".to_string(),
            activated: false,
        },
        ToplevelDesc {
            app_id: "firefox".to_string(),
            title: "Home".to_string(),
            activated: true,
        },
        ToplevelDesc {
            app_id: "nautilus".to_string(),
            title: "Files".to_string(),
            activated: false,
        },
    ];

    let (_tmp, socket, display, comp) = setup(toplevels);
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();

    let server = tokio::task::spawn_blocking(move || {
        run_server(&socket, display, comp, &stop2);
    });

    let mut source = backend::connect(Backend::WlrForeignToplevel).await.unwrap();

    let event = tokio::time::timeout(TEST_TIMEOUT, source.next_event())
        .await
        .expect("timed out waiting for event")
        .expect("next_event failed");

    assert_eq!(event.app_class, "firefox");
    assert_eq!(event.title, Some("Home".to_string()));

    stop.store(true, Ordering::Relaxed);
    let _ = server.await;
}

#[tokio::test]
#[serial]
async fn protocol_not_available_returns_error() {
    let tmp = tempfile::TempDir::new().unwrap();

    // SAFETY: tests run serially.
    unsafe {
        for var in [
            "HYPRLAND_INSTANCE_SIGNATURE",
            "HYPRLAND_CMD",
            "SWAYSOCK",
            "NIRI_SOCKET",
            "XDG_CURRENT_DESKTOP",
        ] {
            env::remove_var(var);
        }

        env::set_var("XDG_RUNTIME_DIR", tmp.path());
        env::set_var("WAYLAND_DISPLAY", "wayland-test-noproto");
    }

    let socket = ListeningSocket::bind("wayland-test-noproto").unwrap();
    let mut display = wayland_server::Display::<MockCompositor>::new().unwrap();
    let mut comp = MockCompositor {
        toplevels_to_send: Vec::new(),
    };

    // Server with NO global registered — protocol unavailable
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();

    let server = tokio::task::spawn_blocking(move || {
        let mut dh = display.handle();
        // Deliberately NOT creating the global

        while !stop2.load(Ordering::Relaxed) {
            if let Some(stream) = socket.accept().unwrap() {
                dh.insert_client(stream, Arc::new(())).unwrap();
            }

            let fd = display.backend().poll_fd().as_fd().as_raw_fd();
            let mut pollfd = [libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            }];
            unsafe {
                libc::poll(pollfd.as_mut_ptr(), 1, 50);
            }

            display.dispatch_clients(&mut comp).unwrap();
            display.flush_clients().unwrap();
        }
    });

    let result = backend::connect(Backend::WlrForeignToplevel).await;
    assert!(result.is_err());

    let err = match result {
        Ok(_) => panic!("expected error, got Ok"),
        Err(e) => format!("{e}"),
    };
    assert!(err.contains("does not support"), "unexpected error: {err}");

    stop.store(true, Ordering::Relaxed);
    let _ = server.await;
}

#[tokio::test]
#[serial]
async fn empty_app_id_filtered() {
    let toplevels = vec![ToplevelDesc {
        app_id: String::new(),
        title: "untitled".to_string(),
        activated: true,
    }];

    let (_tmp, socket, display, comp) = setup(toplevels);
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();

    let server = tokio::task::spawn_blocking(move || {
        run_server(&socket, display, comp, &stop2);
    });

    let mut source = backend::connect(Backend::WlrForeignToplevel).await.unwrap();

    // Empty app_id should be filtered out — no event produced
    let result = tokio::time::timeout(Duration::from_millis(500), source.next_event()).await;
    assert!(result.is_err(), "expected timeout for empty app_id");

    stop.store(true, Ordering::Relaxed);
    let _ = server.await;
}
