//! Sway IPC focus detection backend.
//!
//! Connects to Sway's IPC socket and subscribes to window focus events.
//! Uses the i3-compatible IPC binary protocol over the `SWAYSOCK` Unix socket.

use std::env;
use std::path::PathBuf;
use std::time::Duration;

use futures_util::future::BoxFuture;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;
use tracing::debug;
use tracing::info;
use tracing::trace;
use tracing::warn;

use super::FocusError;
use super::FocusEvent;
use super::FocusSource;

/// IPC magic string for the i3/sway protocol.
const IPC_MAGIC: &[u8] = b"i3-ipc";

/// IPC message type: subscribe to events.
const IPC_SUBSCRIBE: u32 = 2;

/// IPC event type: window event (high bit set for events).
const IPC_EVENT_WINDOW: u32 = 0x8000_0003;

/// Header size: 6 (magic) + 4 (length) + 4 (type).
const IPC_HEADER_SIZE: usize = 14;

/// Sway focus source implementation.
pub struct SwaySource {
    stream: Option<UnixStream>,
    backoff: Duration,
}

impl SwaySource {
    /// Create a new Sway focus source and subscribe to window events.
    pub async fn connect() -> Result<Self, FocusError> {
        let socket_path = get_socket_path()?;
        info!("Connecting to Sway socket: {}", socket_path.display());

        let stream = UnixStream::connect(&socket_path)
            .await
            .map_err(|e| FocusError::ConnectionFailed(e.to_string()))?;

        let mut source = Self {
            stream: Some(stream),
            backoff: Duration::from_millis(250),
        };

        source.subscribe().await?;
        info!("Connected to Sway IPC and subscribed to window events");

        Ok(source)
    }

    /// Get diagnostic information about the Sway environment.
    pub fn get_diagnostics() -> Vec<String> {
        let mut diags = Vec::new();

        match env::var("SWAYSOCK") {
            Ok(v) => {
                diags.push(format!("SWAYSOCK={v}"));
                let path = PathBuf::from(&v);
                if path.exists() {
                    diags.push(format!("Socket path: {} (exists)", path.display()));
                } else {
                    diags.push(format!("Socket path: {} (NOT FOUND)", path.display()));
                }
            }
            Err(_) => {
                diags.push("SWAYSOCK: NOT SET".to_string());
            }
        }

        diags
    }

    /// Send a subscribe message for window events.
    async fn subscribe(&mut self) -> Result<(), FocusError> {
        let payload = b"[\"window\"]";
        self.send_message(IPC_SUBSCRIBE, payload).await?;

        // Read the subscribe reply
        let (_msg_type, reply) = self.read_message().await?;
        let reply_str = String::from_utf8_lossy(&reply);
        trace!("Subscribe reply: {reply_str}");

        if !reply_str.contains("\"success\"") || !reply_str.contains("true") {
            return Err(FocusError::ConnectionFailed(format!(
                "Subscribe failed: {reply_str}"
            )));
        }

        Ok(())
    }

    /// Send an IPC message with the i3/sway binary protocol.
    async fn send_message(&mut self, msg_type: u32, payload: &[u8]) -> Result<(), FocusError> {
        let Some(stream) = &mut self.stream else {
            return Err(FocusError::ConnectionFailed("Not connected".to_string()));
        };

        let mut header = Vec::with_capacity(IPC_HEADER_SIZE + payload.len());
        header.extend_from_slice(IPC_MAGIC);
        #[allow(clippy::cast_possible_truncation)]
        header.extend_from_slice(&(payload.len() as u32).to_ne_bytes());
        header.extend_from_slice(&msg_type.to_ne_bytes());
        header.extend_from_slice(payload);

        stream
            .write_all(&header)
            .await
            .map_err(|e| FocusError::ConnectionFailed(e.to_string()))?;

        Ok(())
    }

    /// Read an IPC message, returning (type, payload).
    async fn read_message(&mut self) -> Result<(u32, Vec<u8>), FocusError> {
        let Some(stream) = &mut self.stream else {
            return Err(FocusError::ConnectionFailed("Not connected".to_string()));
        };

        let mut header = [0u8; IPC_HEADER_SIZE];
        stream
            .read_exact(&mut header)
            .await
            .map_err(|e| FocusError::ConnectionFailed(e.to_string()))?;

        // Verify magic
        if &header[..6] != IPC_MAGIC {
            return Err(FocusError::ConnectionFailed(
                "Invalid IPC magic in response".to_string(),
            ));
        }

        let payload_len = u32::from_ne_bytes(header[6..10].try_into().unwrap()) as usize;
        let msg_type = u32::from_ne_bytes(header[10..14].try_into().unwrap());

        let mut payload = vec![0u8; payload_len];
        stream
            .read_exact(&mut payload)
            .await
            .map_err(|e| FocusError::ConnectionFailed(e.to_string()))?;

        Ok((msg_type, payload))
    }

    /// Attempt to reconnect to the Sway socket.
    async fn reconnect(&mut self) -> Result<(), FocusError> {
        const MAX_BACKOFF: Duration = Duration::from_secs(5);

        warn!(
            "Sway IPC connection lost. Retrying in {:?}...",
            self.backoff
        );
        tokio::time::sleep(self.backoff).await;
        self.backoff = std::cmp::min(self.backoff * 2, MAX_BACKOFF);

        let socket_path = get_socket_path()?;
        let stream = UnixStream::connect(&socket_path)
            .await
            .map_err(|e| FocusError::ConnectionFailed(e.to_string()))?;

        self.stream = Some(stream);
        self.subscribe().await?;

        info!("Reconnected to Sway IPC");
        self.backoff = Duration::from_millis(250);

        Ok(())
    }
}

impl FocusSource for SwaySource {
    fn next_event(&mut self) -> BoxFuture<'_, Result<FocusEvent, FocusError>> {
        Box::pin(async move {
            loop {
                if self.stream.is_none() {
                    self.reconnect().await?;
                }

                match self.read_message().await {
                    Ok((msg_type, payload)) => {
                        if msg_type != IPC_EVENT_WINDOW {
                            trace!("Ignoring non-window event type: {msg_type:#x}");
                            continue;
                        }

                        let payload_str = String::from_utf8_lossy(&payload);
                        trace!("Window event: {payload_str}");

                        if let Some(event) = parse_window_event(&payload_str) {
                            debug!(
                                "Focus changed: class={}, title={:?}, window_id={:?}",
                                event.app_class, event.title, event.window_id
                            );
                            return Ok(event);
                        }
                    }
                    Err(e) => {
                        warn!("Read error: {e}");
                        self.stream = None;
                        self.reconnect().await?;
                    }
                }
            }
        })
    }
}

/// Get the Sway IPC socket path from `SWAYSOCK`.
fn get_socket_path() -> Result<PathBuf, FocusError> {
    let path =
        env::var("SWAYSOCK").map_err(|_| FocusError::EnvVarNotSet("SWAYSOCK".to_string()))?;

    let path = PathBuf::from(path);

    if !path.exists() {
        return Err(FocusError::SocketNotFound(path.display().to_string()));
    }

    Ok(path)
}

/// Parse a window event JSON payload into a `FocusEvent`.
///
/// Only produces events for `"change": "focus"`. The app class is taken from
/// `app_id` for native Wayland windows, falling back to `window_properties.class`
/// for `XWayland` windows.
fn parse_window_event(json: &str) -> Option<FocusEvent> {
    let parsed: serde_json::Value = serde_json::from_str(json).ok()?;

    let change = parsed.get("change")?.as_str()?;
    if change != "focus" {
        trace!("Ignoring window event with change={change}");
        return None;
    }

    let container = parsed.get("container")?;

    // app_id for native Wayland, window_properties.class for XWayland
    let app_class = container
        .get("app_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            container
                .get("window_properties")
                .and_then(|wp| wp.get("class"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
        })?;

    let title = container
        .get("name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from);

    let window_id = container
        .get("id")
        .and_then(serde_json::Value::as_u64)
        .map(|id| id.to_string());

    Some(FocusEvent::new(app_class.to_string(), title, window_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_focus_event_wayland() {
        let json = r#"{
            "change": "focus",
            "container": {
                "id": 42,
                "name": "main.rs — wakatime-focusd",
                "app_id": "foot",
                "window_properties": null
            }
        }"#;

        let event = parse_window_event(json).expect("Should parse focus event");
        assert_eq!(event.app_class, "foot");
        assert_eq!(event.title, Some("main.rs — wakatime-focusd".to_string()));
        assert_eq!(event.window_id, Some("42".to_string()));
    }

    #[test]
    fn test_parse_focus_event_xwayland() {
        let json = r#"{
            "change": "focus",
            "container": {
                "id": 7,
                "name": "Mozilla Firefox",
                "app_id": null,
                "window_properties": {
                    "class": "Firefox",
                    "instance": "Navigator",
                    "title": "Mozilla Firefox",
                    "transient_for": null
                }
            }
        }"#;

        let event = parse_window_event(json).expect("Should parse XWayland focus event");
        assert_eq!(event.app_class, "Firefox");
        assert_eq!(event.title, Some("Mozilla Firefox".to_string()));
        assert_eq!(event.window_id, Some("7".to_string()));
    }

    #[test]
    fn test_parse_focus_event_no_title() {
        let json = r#"{
            "change": "focus",
            "container": {
                "id": 1,
                "name": "",
                "app_id": "dmenu"
            }
        }"#;

        let event = parse_window_event(json).expect("Should parse event without title");
        assert_eq!(event.app_class, "dmenu");
        assert_eq!(event.title, None);
    }

    #[test]
    fn test_parse_non_focus_event() {
        let json = r#"{
            "change": "close",
            "container": {
                "id": 42,
                "name": "closing window",
                "app_id": "foot"
            }
        }"#;

        assert!(parse_window_event(json).is_none());
    }

    #[test]
    fn test_parse_title_event_ignored() {
        let json = r#"{
            "change": "title",
            "container": {
                "id": 42,
                "name": "new title",
                "app_id": "foot"
            }
        }"#;

        assert!(parse_window_event(json).is_none());
    }

    #[test]
    fn test_parse_no_app_id_or_class() {
        let json = r#"{
            "change": "focus",
            "container": {
                "id": 1,
                "name": "mystery",
                "app_id": null,
                "window_properties": null
            }
        }"#;

        assert!(parse_window_event(json).is_none());
    }

    #[test]
    fn test_parse_empty_app_id_falls_back_to_class() {
        let json = r#"{
            "change": "focus",
            "container": {
                "id": 3,
                "name": "Steam",
                "app_id": "",
                "window_properties": {
                    "class": "steam",
                    "instance": "steam",
                    "title": "Steam",
                    "transient_for": null
                }
            }
        }"#;

        let event = parse_window_event(json).expect("Should fall back to class");
        assert_eq!(event.app_class, "steam");
    }

    #[test]
    fn test_parse_no_window_id() {
        let json = r#"{
            "change": "focus",
            "container": {
                "name": "terminal",
                "app_id": "alacritty"
            }
        }"#;

        let event = parse_window_event(json).expect("Should parse without window id");
        assert_eq!(event.app_class, "alacritty");
        assert_eq!(event.window_id, None);
    }

    #[test]
    fn test_parse_malformed_json() {
        assert!(parse_window_event("not json").is_none());
        assert!(parse_window_event("{}").is_none());
        assert!(parse_window_event("").is_none());
    }
}
