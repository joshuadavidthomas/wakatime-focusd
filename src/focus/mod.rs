//! Focus detection module.
//!
//! Provides the FocusEvent model and FocusProvider trait for backend implementations.

pub mod hyprland_ipc;

use std::time::Instant;
use thiserror::Error;

/// A normalized focus event from any backend.
#[derive(Debug, Clone)]
pub struct FocusEvent {
    /// Timestamp when the focus change was detected.
    #[allow(dead_code)]
    pub ts: Instant,

    /// Backend identifier (e.g., "hyprland-ipc").
    pub backend: &'static str,

    /// Window identifier (backend-specific, e.g., "0xabc123" from activewindowv2).
    pub window_id: Option<String>,

    /// Application class/app_id (primary identifier for the app).
    pub app_class: String,

    /// Window title (optional, may contain sensitive info).
    pub title: Option<String>,
}

impl FocusEvent {
    /// Create a new focus event with the current timestamp.
    pub fn new(
        backend: &'static str,
        window_id: Option<String>,
        app_class: String,
        title: Option<String>,
    ) -> Self {
        Self {
            ts: Instant::now(),
            backend,
            window_id,
            app_class,
            title,
        }
    }

    /// Returns true if this represents an empty/no-focus state.
    pub fn is_empty(&self) -> bool {
        self.app_class.is_empty()
    }
}

/// Errors that can occur in focus detection.
#[derive(Error, Debug)]
pub enum FocusError {
    #[error("Socket connection failed: {0}")]
    ConnectionFailed(String),

    #[error("Socket disconnected")]
    Disconnected,

    #[error("Failed to read from socket: {0}")]
    ReadError(String),

    #[error("Environment variable not set: {0}")]
    EnvVarNotSet(String),

    #[error("Socket path not found: {0}")]
    SocketNotFound(String),

    #[error("Parse error: {0}")]
    #[allow(dead_code)]
    ParseError(String),
}
