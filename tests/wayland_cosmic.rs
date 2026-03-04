//! Integration tests for the COSMIC backend against a mock Wayland compositor.
//!
//! Starts a minimal Wayland server that advertises both
//! `ext_foreign_toplevel_list_v1` and `zcosmic_toplevel_info_v1`, creates
//! toplevel handles with focus state, and verifies `FocusEvent` output from
//! the real backend.

use std::collections::HashMap;
use std::env;
use std::os::fd::AsFd;
use std::os::fd::AsRawFd;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;

use cosmic_protocols::toplevel_info::v1::server::zcosmic_toplevel_handle_v1;
use cosmic_protocols::toplevel_info::v1::server::zcosmic_toplevel_handle_v1::ZcosmicToplevelHandleV1;
use cosmic_protocols::toplevel_info::v1::server::zcosmic_toplevel_info_v1;
use cosmic_protocols::toplevel_info::v1::server::zcosmic_toplevel_info_v1::ZcosmicToplevelInfoV1;
use serial_test::serial;
use wakatime_focusd::backend::Backend;
use wakatime_focusd::backend::{
    self,
};
use wayland_protocols::ext::foreign_toplevel_list::v1::server::ext_foreign_toplevel_handle_v1;
use wayland_protocols::ext::foreign_toplevel_list::v1::server::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1;
use wayland_protocols::ext::foreign_toplevel_list::v1::server::ext_foreign_toplevel_list_v1;
use wayland_protocols::ext::foreign_toplevel_list::v1::server::ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1;
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

struct ToplevelDesc {
    app_id: String,
    title: String,
    activated: bool,
}

struct MockCompositor {
    toplevels_to_send: Vec<ToplevelDesc>,
    /// Maps ext handle object IDs to their descriptors, used when
    /// `get_cosmic_toplevel` arrives to send the right events.
    ext_to_desc: HashMap<u32, usize>,
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

// When client binds ext-foreign-toplevel-list: send Toplevel events
impl GlobalDispatch<ExtForeignToplevelListV1, ()> for MockCompositor {
    fn bind(
        state: &mut Self,
        dh: &DisplayHandle,
        client: &Client,
        resource: New<ExtForeignToplevelListV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        let list = data_init.init(resource, ());

        for (idx, _desc) in state.toplevels_to_send.iter().enumerate() {
            let ext_handle: ExtForeignToplevelHandleV1 = client
                .create_resource::<ExtForeignToplevelHandleV1, (), MockCompositor>(
                    dh,
                    list.version(),
                    (),
                )
                .unwrap();

            // Track which desc index this ext handle maps to.
            state.ext_to_desc.insert(ext_handle.id().protocol_id(), idx);

            list.toplevel(&ext_handle);
        }
    }
}

impl Dispatch<ExtForeignToplevelListV1, ()> for MockCompositor {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &ExtForeignToplevelListV1,
        _request: ext_foreign_toplevel_list_v1::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
    }
}

impl Dispatch<ExtForeignToplevelHandleV1, ()> for MockCompositor {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &ExtForeignToplevelHandleV1,
        _request: ext_foreign_toplevel_handle_v1::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
    }
}

// When client binds zcosmic-toplevel-info: nothing to send yet (v2 mode)
impl GlobalDispatch<ZcosmicToplevelInfoV1, ()> for MockCompositor {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<ZcosmicToplevelInfoV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

// Handle get_cosmic_toplevel requests: create the cosmic handle and send events
impl Dispatch<ZcosmicToplevelInfoV1, ()> for MockCompositor {
    fn request(
        state: &mut Self,
        _client: &Client,
        _resource: &ZcosmicToplevelInfoV1,
        request: zcosmic_toplevel_info_v1::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if let zcosmic_toplevel_info_v1::Request::GetCosmicToplevel {
            cosmic_toplevel,
            foreign_toplevel,
        } = request
        {
            let handle = data_init.init(cosmic_toplevel, ());

            // Look up which toplevel desc this ext handle corresponds to.
            let ext_id = foreign_toplevel.id().protocol_id();
            if let Some(&idx) = state.ext_to_desc.get(&ext_id) {
                let desc = &state.toplevels_to_send[idx];

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
}

impl Dispatch<ZcosmicToplevelHandleV1, ()> for MockCompositor {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &ZcosmicToplevelHandleV1,
        _request: zcosmic_toplevel_handle_v1::Request,
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
    socket_name: &str,
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
        ] {
            env::remove_var(var);
        }

        env::set_var("XDG_CURRENT_DESKTOP", "COSMIC");
        env::set_var("XDG_RUNTIME_DIR", tmp.path());
        env::set_var("WAYLAND_DISPLAY", socket_name);
    }

    let socket = ListeningSocket::bind(socket_name).unwrap();
    let display = wayland_server::Display::<MockCompositor>::new().unwrap();
    let comp = MockCompositor {
        toplevels_to_send: toplevels,
        ext_to_desc: HashMap::new(),
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
    let _ext_list_global = dh.create_global::<MockCompositor, ExtForeignToplevelListV1, ()>(1, ());
    let _cosmic_info_global = dh.create_global::<MockCompositor, ZcosmicToplevelInfoV1, ()>(2, ());

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

    let (_tmp, socket, display, comp) = setup(toplevels, "wayland-cosmic-test-1");
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();

    let server = tokio::task::spawn_blocking(move || {
        run_server(&socket, display, comp, &stop2);
    });

    let mut source = backend::connect(Backend::Cosmic).await.unwrap();

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

    let (_tmp, socket, display, comp) = setup(toplevels, "wayland-cosmic-test-2");
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();

    let server = tokio::task::spawn_blocking(move || {
        run_server(&socket, display, comp, &stop2);
    });

    let mut source = backend::connect(Backend::Cosmic).await.unwrap();

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

    let (_tmp, socket, display, comp) = setup(toplevels, "wayland-cosmic-test-3");
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();

    let server = tokio::task::spawn_blocking(move || {
        run_server(&socket, display, comp, &stop2);
    });

    let mut source = backend::connect(Backend::Cosmic).await.unwrap();

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
        ] {
            env::remove_var(var);
        }

        env::set_var("XDG_CURRENT_DESKTOP", "COSMIC");
        env::set_var("XDG_RUNTIME_DIR", tmp.path());
        env::set_var("WAYLAND_DISPLAY", "wayland-cosmic-test-noproto");
    }

    let socket = ListeningSocket::bind("wayland-cosmic-test-noproto").unwrap();
    let mut display = wayland_server::Display::<MockCompositor>::new().unwrap();
    let mut comp = MockCompositor {
        toplevels_to_send: Vec::new(),
        ext_to_desc: HashMap::new(),
    };

    // Server with NO globals registered — protocols unavailable
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();

    let server = tokio::task::spawn_blocking(move || {
        let mut dh = display.handle();
        // Deliberately NOT creating any globals

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

    let result = backend::connect(Backend::Cosmic).await;
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

    let (_tmp, socket, display, comp) = setup(toplevels, "wayland-cosmic-test-5");
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();

    let server = tokio::task::spawn_blocking(move || {
        run_server(&socket, display, comp, &stop2);
    });

    let mut source = backend::connect(Backend::Cosmic).await.unwrap();

    // Empty app_id should be filtered out — no event produced
    let result = tokio::time::timeout(Duration::from_millis(500), source.next_event()).await;
    assert!(result.is_err(), "expected timeout for empty app_id");

    stop.store(true, Ordering::Relaxed);
    let _ = server.await;
}

#[tokio::test]
#[serial]
async fn detection_picks_cosmic_from_xdg_current_desktop() {
    // SAFETY: tests run serially.
    unsafe {
        for var in [
            "HYPRLAND_INSTANCE_SIGNATURE",
            "HYPRLAND_CMD",
            "SWAYSOCK",
            "NIRI_SOCKET",
        ] {
            env::remove_var(var);
        }

        env::set_var("XDG_CURRENT_DESKTOP", "COSMIC");
        env::set_var("WAYLAND_DISPLAY", "wayland-0");
    }

    let detected = Backend::detect().unwrap();
    assert_eq!(detected, Backend::Cosmic);
}
