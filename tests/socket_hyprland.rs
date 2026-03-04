//! Integration tests for the Hyprland backend with a fake Unix socket server.
//!
//! Spins up a fake socket server that writes `activewindow>>` and
//! `activewindowv2>>` lines, connects the real `HyprlandSource` backend,
//! and verifies `FocusEvent` output.

use std::env;
use std::time::Duration;

use serial_test::serial;
use tempfile::TempDir;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixListener;
use wakatime_focusd::backend::Backend;
use wakatime_focusd::backend::{
    self,
};

const TEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Create a temporary Hyprland socket environment.
///
/// Builds the directory structure `$XDG_RUNTIME_DIR/hypr/$sig/.socket2.sock`
/// and sets the env vars so `HyprlandSource::connect()` finds the socket.
///
/// Returns `(TempDir, UnixListener)` — the `TempDir` must be kept alive.
fn setup() -> (TempDir, UnixListener) {
    let tmp = TempDir::new().unwrap();
    let sig = "test_instance";
    let socket_dir = tmp.path().join("hypr").join(sig);
    std::fs::create_dir_all(&socket_dir).unwrap();
    let socket_path = socket_dir.join(".socket2.sock");
    let listener = UnixListener::bind(&socket_path).unwrap();

    unsafe {
        env::set_var("XDG_RUNTIME_DIR", tmp.path());
        env::set_var("HYPRLAND_INSTANCE_SIGNATURE", sig);
    }

    (tmp, listener)
}

#[tokio::test]
#[serial]
async fn basic_focus_event() {
    let (_tmp, listener) = setup();

    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        stream
            .write_all(b"activewindowv2>>0xabc123\n")
            .await
            .unwrap();
        stream
            .write_all(b"activewindow>>firefox,Mozilla Firefox\n")
            .await
            .unwrap();
    });

    let mut source = backend::connect(Backend::Hyprland).await.unwrap();
    let event = tokio::time::timeout(TEST_TIMEOUT, source.next_event())
        .await
        .expect("timed out")
        .unwrap();

    assert_eq!(event.app_class, "firefox");
    assert_eq!(event.title, Some("Mozilla Firefox".to_string()));
    assert_eq!(event.window_id, Some("0xabc123".to_string()));
}

#[tokio::test]
#[serial]
async fn reconnection_after_eof() {
    let (_tmp, listener) = setup();

    tokio::spawn(async move {
        // First connection: send one event, then close.
        let (mut stream, _) = listener.accept().await.unwrap();
        stream
            .write_all(b"activewindow>>firefox,Tab 1\n")
            .await
            .unwrap();
        drop(stream);

        // Second connection after backend reconnects.
        let (mut stream, _) = listener.accept().await.unwrap();
        stream
            .write_all(b"activewindow>>code,main.rs\n")
            .await
            .unwrap();
    });

    let mut source = backend::connect(Backend::Hyprland).await.unwrap();

    let event = tokio::time::timeout(TEST_TIMEOUT, source.next_event())
        .await
        .expect("timed out")
        .unwrap();
    assert_eq!(event.app_class, "firefox");

    // Backend detects EOF, reconnects (250ms backoff), reads from new connection.
    let event = tokio::time::timeout(TEST_TIMEOUT, source.next_event())
        .await
        .expect("reconnection timed out")
        .unwrap();
    assert_eq!(event.app_class, "code");
    assert_eq!(event.title, Some("main.rs".to_string()));
}

// Read errors follow the same reconnection code path as EOF
// (self.reader = None → reconnect), so the EOF test above covers both.

#[tokio::test]
#[serial]
async fn malformed_lines_skipped() {
    let (_tmp, listener) = setup();

    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        // No ">>" separator
        stream.write_all(b"garbage_no_separator\n").await.unwrap();
        // Other event type (not activewindow)
        stream.write_all(b"workspace>>1\n").await.unwrap();
        // Empty line
        stream.write_all(b"\n").await.unwrap();
        // Valid event — should be the only one returned
        stream
            .write_all(b"activewindow>>firefox,After Garbage\n")
            .await
            .unwrap();
    });

    let mut source = backend::connect(Backend::Hyprland).await.unwrap();
    let event = tokio::time::timeout(TEST_TIMEOUT, source.next_event())
        .await
        .expect("timed out")
        .unwrap();

    assert_eq!(event.app_class, "firefox");
    assert_eq!(event.title, Some("After Garbage".to_string()));
}

#[tokio::test]
#[serial]
async fn empty_class_skipped() {
    let (_tmp, listener) = setup();

    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        // Empty class (no focused window)
        stream.write_all(b"activewindow>>,\n").await.unwrap();
        // Valid event after the empty one
        stream
            .write_all(b"activewindow>>kitty,Terminal\n")
            .await
            .unwrap();
    });

    let mut source = backend::connect(Backend::Hyprland).await.unwrap();
    let event = tokio::time::timeout(TEST_TIMEOUT, source.next_event())
        .await
        .expect("timed out")
        .unwrap();

    assert_eq!(event.app_class, "kitty");
    assert_eq!(event.title, Some("Terminal".to_string()));
}
