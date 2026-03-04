//! Niri compositor focus detection backend.
//!
//! Connects to Niri's IPC socket and subscribes to the event stream.
//! Tracks `WindowFocusChanged` and `WindowOpenedOrChanged` events to detect
//! the currently focused window's `app_id` and title.

use std::collections::HashMap;
use std::env;
use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
use tokio::net::UnixStream;
use tracing::debug;
use tracing::info;
use tracing::trace;
use tracing::warn;

use super::FocusError;
use super::FocusEvent;
use super::FocusSource;

/// Environment variable for the Niri socket path.
const SOCKET_PATH_ENV: &str = "NIRI_SOCKET";

/// Niri focus source implementation.
pub struct NiriSource {
    reader: Option<BufReader<UnixStream>>,
    /// Track known windows so we can resolve focus IDs to app info.
    windows: HashMap<u64, WindowInfo>,
    backoff: Duration,
}

/// Cached window information.
#[derive(Debug, Clone)]
struct WindowInfo {
    app_id: Option<String>,
    title: Option<String>,
}

impl NiriSource {
    /// Create a new Niri focus source.
    pub async fn connect() -> Result<Self, FocusError> {
        let socket_path = get_socket_path()?;
        info!("Connecting to Niri socket: {}", socket_path.display());

        let reader = connect_and_subscribe(&socket_path).await?;

        info!("Connected to Niri IPC and subscribed to event stream");

        Ok(Self {
            reader: Some(reader),
            windows: HashMap::new(),
            backoff: Duration::from_millis(250),
        })
    }

    /// Get diagnostic information about the Niri environment.
    pub fn get_diagnostics() -> Vec<String> {
        let mut diags = Vec::new();

        match env::var(SOCKET_PATH_ENV) {
            Ok(v) => {
                diags.push(format!("{SOCKET_PATH_ENV}={v}"));
                let path = PathBuf::from(&v);
                if path.exists() {
                    diags.push(format!("Socket path: {} (exists)", path.display()));
                } else {
                    diags.push(format!("Socket path: {} (NOT FOUND)", path.display()));
                }
            }
            Err(_) => {
                diags.push(format!("{SOCKET_PATH_ENV}: NOT SET"));
            }
        }

        diags
    }

    /// Attempt to reconnect to the Niri socket.
    async fn reconnect(&mut self) -> Result<(), FocusError> {
        const MAX_BACKOFF: Duration = Duration::from_secs(5);

        warn!(
            "Niri IPC connection lost. Retrying in {:?}...",
            self.backoff
        );
        tokio::time::sleep(self.backoff).await;
        self.backoff = std::cmp::min(self.backoff * 2, MAX_BACKOFF);

        let socket_path = get_socket_path()?;
        let reader = connect_and_subscribe(&socket_path).await?;

        self.reader = Some(reader);
        self.backoff = Duration::from_millis(250);

        info!("Reconnected to Niri IPC");
        Ok(())
    }
}

#[async_trait]
impl FocusSource for NiriSource {
    async fn next_event(&mut self) -> Result<FocusEvent, FocusError> {
        loop {
            if self.reader.is_none() {
                self.reconnect().await?;
            }

            let Some(reader) = &mut self.reader else {
                continue;
            };

            let mut line = String::new();
            match reader.read_line(&mut line).await {
                Ok(0) => {
                    warn!("Niri IPC stream ended (EOF)");
                    self.reader = None;
                }
                Ok(_) => {
                    trace!("Niri event: {}", line.trim());

                    match serde_json::from_str::<NiriEvent>(line.trim()) {
                        Ok(event) => {
                            if let Some(focus_event) = self.handle_event(event) {
                                return Ok(focus_event);
                            }
                        }
                        Err(e) => {
                            trace!("Ignoring unparseable event: {e}");
                        }
                    }
                }
                Err(e) => {
                    warn!("Niri read error: {e}");
                    self.reader = None;
                }
            }
        }
    }
}

impl NiriSource {
    /// Process a Niri event and optionally produce a `FocusEvent`.
    fn handle_event(&mut self, event: NiriEvent) -> Option<FocusEvent> {
        match event {
            NiriEvent::WindowOpenedOrChanged { window } => {
                self.windows.insert(
                    window.id,
                    WindowInfo {
                        app_id: window.app_id,
                        title: window.title,
                    },
                );
                None
            }
            NiriEvent::WindowClosed { id } => {
                self.windows.remove(&id);
                None
            }
            NiriEvent::WindowFocusChanged { id } => {
                let id = id?;
                let info = self.windows.get(&id)?;
                let app_id = info.app_id.as_ref().filter(|s| !s.is_empty())?;

                let title = info.title.clone().filter(|s| !s.is_empty());

                debug!(
                    "Focus changed: class={}, title={:?}, window_id={id}",
                    app_id, title
                );

                Some(FocusEvent::new(
                    app_id.clone(),
                    title,
                    Some(id.to_string()),
                ))
            }
        }
    }
}

/// Get the Niri IPC socket path.
fn get_socket_path() -> Result<PathBuf, FocusError> {
    let path = env::var(SOCKET_PATH_ENV)
        .map_err(|_| FocusError::EnvVarNotSet(SOCKET_PATH_ENV.to_string()))?;

    let path = PathBuf::from(path);

    if !path.exists() {
        return Err(FocusError::SocketNotFound(path.display().to_string()));
    }

    Ok(path)
}

/// Connect to the Niri socket and subscribe to the event stream.
async fn connect_and_subscribe(
    socket_path: &PathBuf,
) -> Result<BufReader<UnixStream>, FocusError> {
    let mut stream = UnixStream::connect(socket_path)
        .await
        .map_err(|e| FocusError::ConnectionFailed(e.to_string()))?;

    // Send EventStream request
    let request = "\"EventStream\"\n";
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|e| FocusError::ConnectionFailed(format!("Write request: {e}")))?;

    let mut reader = BufReader::new(stream);

    // Read the initial reply
    let mut reply_line = String::new();
    reader
        .read_line(&mut reply_line)
        .await
        .map_err(|e| FocusError::ConnectionFailed(format!("Read reply: {e}")))?;

    trace!("EventStream reply: {}", reply_line.trim());

    if !reply_line.contains("Ok") {
        return Err(FocusError::ConnectionFailed(format!(
            "EventStream subscribe failed: {}",
            reply_line.trim()
        )));
    }

    Ok(reader)
}

/// Niri IPC event (subset we care about).
///
/// Variant names match the Niri IPC JSON protocol.
#[derive(Debug, Deserialize)]
#[allow(clippy::enum_variant_names)]
enum NiriEvent {
    WindowOpenedOrChanged {
        window: NiriWindow,
    },
    WindowClosed {
        id: u64,
    },
    WindowFocusChanged {
        id: Option<u64>,
    },
}

/// Window data from Niri IPC.
#[derive(Debug, Deserialize)]
struct NiriWindow {
    id: u64,
    title: Option<String>,
    app_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source_with_windows(windows: Vec<(u64, &str, &str)>) -> NiriSource {
        let mut s = NiriSource {
            reader: None,
            windows: HashMap::new(),
            backoff: Duration::from_millis(250),
        };
        for (id, app_id, title) in windows {
            s.windows.insert(
                id,
                WindowInfo {
                    app_id: Some(app_id.to_string()),
                    title: Some(title.to_string()),
                },
            );
        }
        s
    }

    #[test]
    fn test_parse_window_focus_changed() {
        let json = r#"{"WindowFocusChanged":{"id":42}}"#;
        let event: NiriEvent = serde_json::from_str(json).unwrap();
        assert!(matches!(
            event,
            NiriEvent::WindowFocusChanged { id: Some(42) }
        ));
    }

    #[test]
    fn test_parse_window_focus_changed_none() {
        let json = r#"{"WindowFocusChanged":{"id":null}}"#;
        let event: NiriEvent = serde_json::from_str(json).unwrap();
        assert!(matches!(
            event,
            NiriEvent::WindowFocusChanged { id: None }
        ));
    }

    #[test]
    fn test_parse_window_opened_or_changed() {
        let json = r#"{"WindowOpenedOrChanged":{"window":{"id":1,"title":"Terminal","app_id":"foot"}}}"#;
        let event: NiriEvent = serde_json::from_str(json).unwrap();
        match event {
            NiriEvent::WindowOpenedOrChanged { window } => {
                assert_eq!(window.id, 1);
                assert_eq!(window.app_id, Some("foot".to_string()));
                assert_eq!(window.title, Some("Terminal".to_string()));
            }
            _ => panic!("Expected WindowOpenedOrChanged"),
        }
    }

    #[test]
    fn test_parse_window_closed() {
        let json = r#"{"WindowClosed":{"id":7}}"#;
        let event: NiriEvent = serde_json::from_str(json).unwrap();
        assert!(matches!(event, NiriEvent::WindowClosed { id: 7 }));
    }

    #[test]
    fn test_handle_focus_with_known_window() {
        let mut source = source_with_windows(vec![(42, "firefox", "Mozilla Firefox")]);

        let event = NiriEvent::WindowFocusChanged { id: Some(42) };
        let focus = source.handle_event(event).expect("Should produce event");
        assert_eq!(focus.app_class, "firefox");
        assert_eq!(focus.title, Some("Mozilla Firefox".to_string()));
        assert_eq!(focus.window_id, Some("42".to_string()));
    }

    #[test]
    fn test_handle_focus_unknown_window() {
        let mut source = source_with_windows(vec![]);

        let event = NiriEvent::WindowFocusChanged { id: Some(99) };
        assert!(source.handle_event(event).is_none());
    }

    #[test]
    fn test_handle_focus_none() {
        let mut source = source_with_windows(vec![(1, "foot", "term")]);

        let event = NiriEvent::WindowFocusChanged { id: None };
        assert!(source.handle_event(event).is_none());
    }

    #[test]
    fn test_handle_window_opened_updates_cache() {
        let mut source = source_with_windows(vec![]);

        let event = NiriEvent::WindowOpenedOrChanged {
            window: NiriWindow {
                id: 5,
                app_id: Some("alacritty".to_string()),
                title: Some("~".to_string()),
            },
        };
        assert!(source.handle_event(event).is_none());

        // Now focus should work
        let event = NiriEvent::WindowFocusChanged { id: Some(5) };
        let focus = source.handle_event(event).expect("Should produce event");
        assert_eq!(focus.app_class, "alacritty");
    }

    #[test]
    fn test_handle_window_closed_removes_from_cache() {
        let mut source = source_with_windows(vec![(3, "foot", "term")]);

        let event = NiriEvent::WindowClosed { id: 3 };
        assert!(source.handle_event(event).is_none());

        // Focus on closed window should produce nothing
        let event = NiriEvent::WindowFocusChanged { id: Some(3) };
        assert!(source.handle_event(event).is_none());
    }

    #[test]
    fn test_handle_focus_empty_app_id() {
        let mut source = NiriSource {
            reader: None,
            windows: HashMap::new(),
            backoff: Duration::from_millis(250),
        };
        source.windows.insert(
            1,
            WindowInfo {
                app_id: Some(String::new()),
                title: Some("untitled".to_string()),
            },
        );

        let event = NiriEvent::WindowFocusChanged { id: Some(1) };
        assert!(source.handle_event(event).is_none());
    }

    #[test]
    fn test_parse_unknown_event_ignored() {
        let json = r#"{"WorkspaceActivated":{"id":1,"focused":true}}"#;
        assert!(serde_json::from_str::<NiriEvent>(json).is_err());
    }
}
