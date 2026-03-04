//! Integration tests for the Niri backend with a fake Unix socket server.
//!
//! Spins up a fake socket server that speaks the Niri JSON-lines protocol,
//! connects the real `NiriSource` backend, and verifies `FocusEvent` output.

use std::env;
use std::time::Duration;

use serial_test::serial;
use tempfile::TempDir;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixListener;
use tokio::net::UnixStream;
use wakatime_focusd::backend::Backend;
use wakatime_focusd::backend::FocusError;
use wakatime_focusd::backend::{self};

const TEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Read a single line (up to `\n`) from a stream, one byte at a time.
async fn read_line_from(stream: &mut UnixStream) -> String {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    while stream.read_exact(&mut byte).await.is_ok() {
        buf.push(byte[0]);
        if byte[0] == b'\n' {
            break;
        }
    }
    String::from_utf8(buf).unwrap()
}

/// Read and respond to the `EventStream` handshake.
async fn handle_handshake(stream: &mut UnixStream) {
    let line = read_line_from(stream).await;
    assert_eq!(line.trim(), "\"EventStream\"");
    stream
        .write_all(b"{\"Ok\":\"EventStream\"}\n")
        .await
        .unwrap();
}

/// Create a temporary Niri socket environment.
fn setup() -> (TempDir, UnixListener) {
    let tmp = TempDir::new().unwrap();
    let socket_path = tmp.path().join("niri.sock");
    let listener = UnixListener::bind(&socket_path).unwrap();

    unsafe {
        env::set_var("NIRI_SOCKET", &socket_path);
    }

    (tmp, listener)
}

#[tokio::test]
#[serial]
async fn basic_focus_event() {
    let (_tmp, listener) = setup();

    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        handle_handshake(&mut stream).await;

        // Register a window, then focus it.
        stream
            .write_all(
                br#"{"WindowOpenedOrChanged":{"window":{"id":1,"title":"Terminal","app_id":"foot"}}}"#,
            )
            .await
            .unwrap();
        stream.write_all(b"\n").await.unwrap();

        stream
            .write_all(br#"{"WindowFocusChanged":{"id":1}}"#)
            .await
            .unwrap();
        stream.write_all(b"\n").await.unwrap();
    });

    let mut source = backend::connect(Backend::Niri).await.unwrap();
    let event = tokio::time::timeout(TEST_TIMEOUT, source.next_event())
        .await
        .expect("timed out")
        .unwrap();

    assert_eq!(event.app_class, "foot");
    assert_eq!(event.title, Some("Terminal".to_string()));
    assert_eq!(event.window_id, Some("1".to_string()));
}

#[tokio::test]
#[serial]
async fn cache_coherence_focus_before_window_info() {
    let (_tmp, listener) = setup();

    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        handle_handshake(&mut stream).await;

        // Focus on unknown window — silently consumed (no event produced).
        stream
            .write_all(br#"{"WindowFocusChanged":{"id":42}}"#)
            .await
            .unwrap();
        stream.write_all(b"\n").await.unwrap();

        // Window info arrives — cached, no event.
        stream
            .write_all(
                br#"{"WindowOpenedOrChanged":{"window":{"id":42,"title":"GitHub","app_id":"firefox"}}}"#,
            )
            .await
            .unwrap();
        stream.write_all(b"\n").await.unwrap();

        // Focus again — now the window is known, event produced.
        stream
            .write_all(br#"{"WindowFocusChanged":{"id":42}}"#)
            .await
            .unwrap();
        stream.write_all(b"\n").await.unwrap();
    });

    let mut source = backend::connect(Backend::Niri).await.unwrap();
    let event = tokio::time::timeout(TEST_TIMEOUT, source.next_event())
        .await
        .expect("timed out")
        .unwrap();

    assert_eq!(event.app_class, "firefox");
    assert_eq!(event.title, Some("GitHub".to_string()));
    assert_eq!(event.window_id, Some("42".to_string()));
}

#[tokio::test]
#[serial]
async fn window_closed_removes_from_cache() {
    let (_tmp, listener) = setup();

    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        handle_handshake(&mut stream).await;

        // Register window 10.
        stream
            .write_all(
                br#"{"WindowOpenedOrChanged":{"window":{"id":10,"title":"term","app_id":"foot"}}}"#,
            )
            .await
            .unwrap();
        stream.write_all(b"\n").await.unwrap();

        // Close window 10.
        stream
            .write_all(br#"{"WindowClosed":{"id":10}}"#)
            .await
            .unwrap();
        stream.write_all(b"\n").await.unwrap();

        // Focus on closed window — should produce nothing (removed from cache).
        stream
            .write_all(br#"{"WindowFocusChanged":{"id":10}}"#)
            .await
            .unwrap();
        stream.write_all(b"\n").await.unwrap();

        // Register and focus a different window — this should produce the event.
        stream
            .write_all(
                br#"{"WindowOpenedOrChanged":{"window":{"id":20,"title":"main.rs","app_id":"code"}}}"#,
            )
            .await
            .unwrap();
        stream.write_all(b"\n").await.unwrap();

        stream
            .write_all(br#"{"WindowFocusChanged":{"id":20}}"#)
            .await
            .unwrap();
        stream.write_all(b"\n").await.unwrap();
    });

    let mut source = backend::connect(Backend::Niri).await.unwrap();
    let event = tokio::time::timeout(TEST_TIMEOUT, source.next_event())
        .await
        .expect("timed out")
        .unwrap();

    // Window 10 was closed, so the first event should be for window 20.
    assert_eq!(event.app_class, "code");
    assert_eq!(event.title, Some("main.rs".to_string()));
    assert_eq!(event.window_id, Some("20".to_string()));
}

#[tokio::test]
#[serial]
async fn reconnection_after_eof() {
    let (_tmp, listener) = setup();

    tokio::spawn(async move {
        // First connection: handshake, send event, close.
        let (mut stream, _) = listener.accept().await.unwrap();
        handle_handshake(&mut stream).await;
        stream
            .write_all(
                br#"{"WindowOpenedOrChanged":{"window":{"id":1,"title":"tab1","app_id":"firefox"}}}"#,
            )
            .await
            .unwrap();
        stream.write_all(b"\n").await.unwrap();
        stream
            .write_all(br#"{"WindowFocusChanged":{"id":1}}"#)
            .await
            .unwrap();
        stream.write_all(b"\n").await.unwrap();
        // Give the client time to read before closing.
        tokio::time::sleep(Duration::from_millis(50)).await;
        drop(stream);

        // Second connection after backend reconnects (must re-handshake).
        let (mut stream, _) = listener.accept().await.unwrap();
        handle_handshake(&mut stream).await;
        stream
            .write_all(
                br#"{"WindowOpenedOrChanged":{"window":{"id":2,"title":"main.rs","app_id":"code"}}}"#,
            )
            .await
            .unwrap();
        stream.write_all(b"\n").await.unwrap();
        stream
            .write_all(br#"{"WindowFocusChanged":{"id":2}}"#)
            .await
            .unwrap();
        stream.write_all(b"\n").await.unwrap();
    });

    let mut source = backend::connect(Backend::Niri).await.unwrap();

    let event = tokio::time::timeout(TEST_TIMEOUT, source.next_event())
        .await
        .expect("timed out")
        .unwrap();
    assert_eq!(event.app_class, "firefox");

    // After EOF, backend reconnects and reads from the new connection.
    // The window cache is preserved across reconnects, but window 2 is new.
    let event = tokio::time::timeout(TEST_TIMEOUT, source.next_event())
        .await
        .expect("reconnection timed out")
        .unwrap();
    assert_eq!(event.app_class, "code");
    assert_eq!(event.title, Some("main.rs".to_string()));
}

#[tokio::test]
#[serial]
async fn handshake_failure() {
    let (_tmp, listener) = setup();

    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        // Read the EventStream request
        let _line = read_line_from(&mut stream).await;
        // Reply with non-Ok response
        stream
            .write_all(br#"{"Err":"UnsupportedRequest"}"#)
            .await
            .unwrap();
        stream.write_all(b"\n").await.unwrap();
    });

    let result = backend::connect(Backend::Niri).await;
    match result {
        Err(FocusError::ConnectionFailed(msg)) => {
            assert!(
                msg.contains("EventStream subscribe failed"),
                "unexpected error: {msg}"
            );
        }
        Ok(_) => panic!("Expected error, but connect succeeded"),
        Err(e) => panic!("Expected ConnectionFailed, got: {e:?}"),
    }
}
