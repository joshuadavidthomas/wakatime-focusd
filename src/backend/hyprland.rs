//! Hyprland IPC socket2 focus detection backend.
//!
//! Connects to Hyprland's socket2 event stream and parses activewindow/activewindowv2 events.

use super::{FocusError, FocusEvent, FocusSource};
use async_trait::async_trait;
use std::env;
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::UnixStream;
use tracing::{debug, info, trace, warn};

/// Hyprland focus source implementation.
pub struct HyprlandSource {
    reader: Option<BufReader<UnixStream>>,
    state: FocusState,
    backoff: Duration,
}

impl HyprlandSource {
    /// Create a new Hyprland focus source.
    pub async fn connect() -> Result<Self, FocusError> {
        let socket_path = get_socket2_path()?;
        info!("Connecting to Hyprland socket2: {}", socket_path.display());

        let stream = UnixStream::connect(&socket_path)
            .await
            .map_err(|e| FocusError::ConnectionFailed(e.to_string()))?;

        info!("Connected to Hyprland socket2");

        Ok(Self {
            reader: Some(BufReader::new(stream)),
            state: FocusState::default(),
            backoff: Duration::from_millis(250),
        })
    }

    /// Get diagnostic information about the Hyprland environment.
    pub fn get_diagnostics() -> Vec<String> {
        let mut diags = Vec::new();

        match env::var("XDG_RUNTIME_DIR") {
            Ok(v) => diags.push(format!("XDG_RUNTIME_DIR={}", v)),
            Err(_) => diags.push("XDG_RUNTIME_DIR: NOT SET".to_string()),
        }

        match env::var("HYPRLAND_INSTANCE_SIGNATURE") {
            Ok(v) => diags.push(format!("HYPRLAND_INSTANCE_SIGNATURE={}", v)),
            Err(_) => diags.push("HYPRLAND_INSTANCE_SIGNATURE: NOT SET".to_string()),
        }

        if let Ok(path) = get_socket2_path() {
            diags.push(format!("Socket2 path: {} (exists)", path.display()));
        } else {
            diags.push("Socket2 path: NOT FOUND".to_string());
        }

        diags
    }

    /// Attempt to reconnect to the Hyprland socket.
    async fn reconnect(&mut self) -> Result<(), FocusError> {
        const MAX_BACKOFF: Duration = Duration::from_secs(5);

        warn!(
            "Socket2 connection lost. Retrying in {:?}...",
            self.backoff
        );

        tokio::time::sleep(self.backoff).await;

        // Exponential backoff with cap
        self.backoff = std::cmp::min(self.backoff * 2, MAX_BACKOFF);

        let socket_path = get_socket2_path()?;
        let stream = UnixStream::connect(&socket_path)
            .await
            .map_err(|e| FocusError::ConnectionFailed(e.to_string()))?;

        info!("Reconnected to Hyprland socket2");
        self.reader = Some(BufReader::new(stream));
        self.backoff = Duration::from_millis(250); // Reset backoff on success

        Ok(())
    }
}

#[async_trait]
impl FocusSource for HyprlandSource {
    async fn next_event(&mut self) -> Result<FocusEvent, FocusError> {
        loop {
            let reader = match &mut self.reader {
                Some(r) => r,
                None => {
                    self.reconnect().await?;
                    continue;
                }
            };

            let mut line = String::new();
            match reader.read_line(&mut line).await {
                Ok(0) => {
                    // EOF - socket closed
                    warn!("Socket2 stream ended (EOF)");
                    self.reader = None;
                    self.reconnect().await?;
                    continue;
                }
                Ok(_) => {
                    trace!("Received line: {}", line.trim());
                    let event = parse_event_line(&line);

                    if let Some(focus_event) = self.state.update(event) {
                        debug!(
                            "Focus changed: class={}, title={:?}, window_id={:?}",
                            focus_event.app_class, focus_event.title, focus_event.window_id
                        );
                        return Ok(focus_event);
                    }
                    // No event produced, read next line
                }
                Err(e) => {
                    warn!("Read error: {}", e);
                    self.reader = None;
                    self.reconnect().await?;
                    continue;
                }
            }
        }
    }
}

/// Get the path to Hyprland's socket2.
fn get_socket2_path() -> Result<PathBuf, FocusError> {
    let xdg_runtime_dir = env::var("XDG_RUNTIME_DIR")
        .map_err(|_| FocusError::EnvVarNotSet("XDG_RUNTIME_DIR".to_string()))?;

    let hyprland_sig = env::var("HYPRLAND_INSTANCE_SIGNATURE")
        .map_err(|_| FocusError::EnvVarNotSet("HYPRLAND_INSTANCE_SIGNATURE".to_string()))?;

    let socket_path = PathBuf::from(&xdg_runtime_dir)
        .join("hypr")
        .join(&hyprland_sig)
        .join(".socket2.sock");

    if !socket_path.exists() {
        return Err(FocusError::SocketNotFound(format!(
            "{}",
            socket_path.display()
        )));
    }

    Ok(socket_path)
}

/// Parsed IPC event from socket2.
#[derive(Debug, Clone)]
enum HyprlandEvent {
    /// activewindow>>WINDOWCLASS,WINDOWTITLE
    ActiveWindow { class: String, title: String },
    /// activewindowv2>>WINDOWADDRESS
    ActiveWindowV2 { address: String },
    /// Other events we don't care about.
    Other,
}

/// Parse a single line from the socket2 stream.
///
/// Format: EVENT>>DATA\n
fn parse_event_line(line: &str) -> HyprlandEvent {
    let line = line.trim_end();

    // Split on first ">>" to get event name and data
    let Some((event_name, data)) = line.split_once(">>") else {
        trace!("Ignoring malformed line (no >>): {}", line);
        return HyprlandEvent::Other;
    };

    match event_name {
        "activewindow" => {
            // Data format: WINDOWCLASS,WINDOWTITLE
            // IMPORTANT: Title can contain commas, so split on FIRST comma only
            let (class, title) = if let Some((c, t)) = data.split_once(',') {
                (c.to_string(), t.to_string())
            } else {
                // No comma means just class, no title
                (data.to_string(), String::new())
            };

            HyprlandEvent::ActiveWindow { class, title }
        }
        "activewindowv2" => {
            // Data format: WINDOWADDRESS (e.g., "0xabc123" or just the hex part)
            HyprlandEvent::ActiveWindowV2 {
                address: data.to_string(),
            }
        }
        _ => {
            trace!("Ignoring event: {}", event_name);
            HyprlandEvent::Other
        }
    }
}

/// State tracker for building complete focus events.
///
/// We may receive activewindow and activewindowv2 in sequence,
/// so we need to correlate them.
#[derive(Debug, Default)]
struct FocusState {
    current_class: Option<String>,
    current_title: Option<String>,
    current_address: Option<String>,
}

impl FocusState {
    /// Update state and return a FocusEvent if we have enough info.
    fn update(&mut self, event: HyprlandEvent) -> Option<FocusEvent> {
        match event {
            HyprlandEvent::ActiveWindow { class, title } => {
                // activewindow comes before activewindowv2, so store and wait
                // But also emit immediately since we have the essential info
                self.current_class = Some(class.clone());
                self.current_title = Some(title.clone());

                // Create event with whatever address we have (might be stale)
                if class.is_empty() {
                    // Empty class means no focus (e.g., switching to empty workspace)
                    None
                } else {
                    Some(FocusEvent::new(
                        class,
                        if title.is_empty() { None } else { Some(title) },
                        self.current_address.clone(),
                    ))
                }
            }
            HyprlandEvent::ActiveWindowV2 { address } => {
                // Update address for future events
                self.current_address = if address.is_empty() {
                    None
                } else {
                    Some(address)
                };
                // Don't emit here; activewindow already emitted
                None
            }
            HyprlandEvent::Other => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_activewindow_simple() {
        let line = "activewindow>>firefox,Mozilla Firefox";
        match parse_event_line(line) {
            HyprlandEvent::ActiveWindow { class, title } => {
                assert_eq!(class, "firefox");
                assert_eq!(title, "Mozilla Firefox");
            }
            _ => panic!("Expected ActiveWindow event"),
        }
    }

    #[test]
    fn test_parse_activewindow_comma_in_title() {
        // Title contains commas - must split on first comma only
        let line = "activewindow>>code,main.rs - wakatime-focusd, Pair Programming";
        match parse_event_line(line) {
            HyprlandEvent::ActiveWindow { class, title } => {
                assert_eq!(class, "code");
                assert_eq!(title, "main.rs - wakatime-focusd, Pair Programming");
            }
            _ => panic!("Expected ActiveWindow event"),
        }
    }

    #[test]
    fn test_parse_activewindow_no_title() {
        let line = "activewindow>>kitty,";
        match parse_event_line(line) {
            HyprlandEvent::ActiveWindow { class, title } => {
                assert_eq!(class, "kitty");
                assert_eq!(title, "");
            }
            _ => panic!("Expected ActiveWindow event"),
        }
    }

    #[test]
    fn test_parse_activewindow_no_comma() {
        let line = "activewindow>>dmenu";
        match parse_event_line(line) {
            HyprlandEvent::ActiveWindow { class, title } => {
                assert_eq!(class, "dmenu");
                assert_eq!(title, "");
            }
            _ => panic!("Expected ActiveWindow event"),
        }
    }

    #[test]
    fn test_parse_activewindow_empty() {
        // Empty class means no focused window
        let line = "activewindow>>,";
        match parse_event_line(line) {
            HyprlandEvent::ActiveWindow { class, title } => {
                assert_eq!(class, "");
                assert_eq!(title, "");
            }
            _ => panic!("Expected ActiveWindow event"),
        }
    }

    #[test]
    fn test_parse_activewindowv2() {
        let line = "activewindowv2>>0x55a1b2c3d4e5";
        match parse_event_line(line) {
            HyprlandEvent::ActiveWindowV2 { address } => {
                assert_eq!(address, "0x55a1b2c3d4e5");
            }
            _ => panic!("Expected ActiveWindowV2 event"),
        }
    }

    #[test]
    fn test_parse_activewindowv2_empty() {
        let line = "activewindowv2>>";
        match parse_event_line(line) {
            HyprlandEvent::ActiveWindowV2 { address } => {
                assert_eq!(address, "");
            }
            _ => panic!("Expected ActiveWindowV2 event"),
        }
    }

    #[test]
    fn test_parse_other_events() {
        assert!(matches!(
            parse_event_line("workspace>>1"),
            HyprlandEvent::Other
        ));
        assert!(matches!(
            parse_event_line("openwindow>>0x123,1,kitty,kitty"),
            HyprlandEvent::Other
        ));
        assert!(matches!(
            parse_event_line("closewindow>>0x123"),
            HyprlandEvent::Other
        ));
    }

    #[test]
    fn test_parse_malformed_line() {
        assert!(matches!(
            parse_event_line("no separator"),
            HyprlandEvent::Other
        ));
        assert!(matches!(parse_event_line(""), HyprlandEvent::Other));
    }

    #[test]
    fn test_parse_with_trailing_newline() {
        let line = "activewindow>>firefox,Title\n";
        match parse_event_line(line) {
            HyprlandEvent::ActiveWindow { class, title } => {
                assert_eq!(class, "firefox");
                assert_eq!(title, "Title");
            }
            _ => panic!("Expected ActiveWindow event"),
        }
    }

    #[test]
    fn test_focus_state_activewindow() {
        let mut state = FocusState::default();

        let event = HyprlandEvent::ActiveWindow {
            class: "firefox".to_string(),
            title: "Mozilla Firefox".to_string(),
        };

        let focus = state.update(event).expect("Should produce focus event");
        assert_eq!(focus.app_class, "firefox");
        assert_eq!(focus.title, Some("Mozilla Firefox".to_string()));
    }

    #[test]
    fn test_focus_state_empty_class() {
        let mut state = FocusState::default();

        let event = HyprlandEvent::ActiveWindow {
            class: "".to_string(),
            title: "".to_string(),
        };

        assert!(
            state.update(event).is_none(),
            "Empty class should not produce event"
        );
    }

    #[test]
    fn test_focus_state_v2_updates_address() {
        let mut state = FocusState::default();

        // First, get a v2 event
        let v2_event = HyprlandEvent::ActiveWindowV2 {
            address: "0xabc123".to_string(),
        };
        assert!(state.update(v2_event).is_none());

        // Then an activewindow event should include the address
        let event = HyprlandEvent::ActiveWindow {
            class: "code".to_string(),
            title: "main.rs".to_string(),
        };
        let focus = state.update(event).expect("Should produce focus event");
        assert_eq!(focus.window_id, Some("0xabc123".to_string()));
    }
}
