//! Integration tests for the Sway backend with a fake Unix socket server.
//!
//! Spins up a fake socket server that speaks the i3-compatible IPC binary
//! protocol, connects the real `SwaySource` backend, and verifies `FocusEvent`
//! output.

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
use wakatime_focusd::backend::{
    self,
};

const TEST_TIMEOUT: Duration = Duration::from_secs(5);

const IPC_MAGIC: &[u8] = b"i3-ipc";
const IPC_SUBSCRIBE: u32 = 2;
const IPC_EVENT_WINDOW: u32 = 0x8000_0003;
const IPC_HEADER_SIZE: usize = 14;

fn build_ipc_message(msg_type: u32, payload: &[u8]) -> Vec<u8> {
    let mut msg = Vec::with_capacity(IPC_HEADER_SIZE + payload.len());
    msg.extend_from_slice(IPC_MAGIC);
    #[allow(clippy::cast_possible_truncation)]
    msg.extend_from_slice(&(payload.len() as u32).to_ne_bytes());
    msg.extend_from_slice(&msg_type.to_ne_bytes());
    msg.extend_from_slice(payload);
    msg
}

async fn read_ipc_message(stream: &mut UnixStream) -> (u32, Vec<u8>) {
    let mut header = [0u8; IPC_HEADER_SIZE];
    stream.read_exact(&mut header).await.unwrap();
    assert_eq!(&header[..6], IPC_MAGIC);
    let payload_len = u32::from_ne_bytes(header[6..10].try_into().unwrap()) as usize;
    let msg_type = u32::from_ne_bytes(header[10..14].try_into().unwrap());
    let mut payload = vec![0u8; payload_len];
    stream.read_exact(&mut payload).await.unwrap();
    (msg_type, payload)
}

/// Read and respond to the subscribe handshake.
async fn handle_subscribe(stream: &mut UnixStream) {
    let (msg_type, _payload) = read_ipc_message(stream).await;
    assert_eq!(msg_type, IPC_SUBSCRIBE);
    let reply = build_ipc_message(IPC_SUBSCRIBE, br#"{"success":true}"#);
    stream.write_all(&reply).await.unwrap();
}

fn wayland_focus_event(id: u64, app_id: &str, name: &str) -> Vec<u8> {
    let json = format!(
        r#"{{"change":"focus","container":{{"id":{id},"name":"{name}","app_id":"{app_id}"}}}}"#,
    );
    build_ipc_message(IPC_EVENT_WINDOW, json.as_bytes())
}

fn xwayland_focus_event(id: u64, wm_class: &str, name: &str) -> Vec<u8> {
    let json = format!(
        r#"{{"change":"focus","container":{{"id":{id},"name":"{name}","app_id":null,"window_properties":{{"class":"{wm_class}"}}}}}}"#,
    );
    build_ipc_message(IPC_EVENT_WINDOW, json.as_bytes())
}

/// Create a temporary Sway socket environment.
fn setup() -> (TempDir, UnixListener) {
    let tmp = TempDir::new().unwrap();
    let socket_path = tmp.path().join("sway.sock");
    let listener = UnixListener::bind(&socket_path).unwrap();

    unsafe {
        env::set_var("SWAYSOCK", &socket_path);
    }

    (tmp, listener)
}

#[tokio::test]
#[serial]
async fn basic_wayland_focus_event() {
    let (_tmp, listener) = setup();

    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        handle_subscribe(&mut stream).await;
        let event = wayland_focus_event(42, "foot", "~/projects");
        stream.write_all(&event).await.unwrap();
    });

    let mut source = backend::connect(Backend::Sway).await.unwrap();
    let event = tokio::time::timeout(TEST_TIMEOUT, source.next_event())
        .await
        .expect("timed out")
        .unwrap();

    assert_eq!(event.app_class, "foot");
    assert_eq!(event.title, Some("~/projects".to_string()));
    assert_eq!(event.window_id, Some("42".to_string()));
}

#[tokio::test]
#[serial]
async fn xwayland_class_fallback() {
    let (_tmp, listener) = setup();

    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        handle_subscribe(&mut stream).await;
        let event = xwayland_focus_event(7, "Firefox", "Mozilla Firefox");
        stream.write_all(&event).await.unwrap();
    });

    let mut source = backend::connect(Backend::Sway).await.unwrap();
    let event = tokio::time::timeout(TEST_TIMEOUT, source.next_event())
        .await
        .expect("timed out")
        .unwrap();

    assert_eq!(event.app_class, "Firefox");
    assert_eq!(event.title, Some("Mozilla Firefox".to_string()));
    assert_eq!(event.window_id, Some("7".to_string()));
}

#[tokio::test]
#[serial]
async fn reconnection_after_eof() {
    let (_tmp, listener) = setup();

    tokio::spawn(async move {
        // First connection: subscribe, send event, close.
        let (mut stream, _) = listener.accept().await.unwrap();
        handle_subscribe(&mut stream).await;
        let event = wayland_focus_event(1, "foot", "term");
        stream.write_all(&event).await.unwrap();
        drop(stream);

        // Second connection after backend reconnects (must re-subscribe).
        let (mut stream, _) = listener.accept().await.unwrap();
        handle_subscribe(&mut stream).await;
        let event = wayland_focus_event(2, "code", "main.rs");
        stream.write_all(&event).await.unwrap();
    });

    let mut source = backend::connect(Backend::Sway).await.unwrap();

    let event = tokio::time::timeout(TEST_TIMEOUT, source.next_event())
        .await
        .expect("timed out")
        .unwrap();
    assert_eq!(event.app_class, "foot");

    let event = tokio::time::timeout(TEST_TIMEOUT, source.next_event())
        .await
        .expect("reconnection timed out")
        .unwrap();
    assert_eq!(event.app_class, "code");
    assert_eq!(event.title, Some("main.rs".to_string()));
}

#[tokio::test]
#[serial]
async fn subscribe_failure() {
    let (_tmp, listener) = setup();

    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        // Read the subscribe request
        let (msg_type, _payload) = read_ipc_message(&mut stream).await;
        assert_eq!(msg_type, IPC_SUBSCRIBE);
        // Reply with failure
        let reply = build_ipc_message(IPC_SUBSCRIBE, br#"{"success":false}"#);
        stream.write_all(&reply).await.unwrap();
    });

    let result = backend::connect(Backend::Sway).await;
    match result {
        Err(FocusError::ConnectionFailed(msg)) => {
            assert!(msg.contains("Subscribe failed"), "unexpected error: {msg}");
        }
        Ok(_) => panic!("Expected error, but connect succeeded"),
        Err(e) => panic!("Expected ConnectionFailed, got: {e:?}"),
    }
}
