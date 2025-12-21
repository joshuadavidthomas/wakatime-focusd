//! Focus detection module.
//!
//! Provides the FocusEvent model for backend implementations.

pub mod hyprland_ipc;

use thiserror::Error;

/// Focus detection backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusBackend {
    HyprlandIpc,
}

impl FocusBackend {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::HyprlandIpc => "hyprland-ipc",
        }
    }
}

/// A normalized focus event from any backend.
#[derive(Debug, Clone)]
pub struct FocusEvent {
    /// Backend that produced this event.
    pub backend: FocusBackend,

    /// Window identifier (backend-specific, e.g., "0xabc123" from activewindowv2).
    pub window_id: Option<String>,

    /// Application class/app_id (primary identifier for the app).
    pub app_class: String,

    /// Window title (optional, may contain sensitive info).
    pub title: Option<String>,
}

impl FocusEvent {
    /// Create a new focus event.
    pub fn new(
        backend: FocusBackend,
        window_id: Option<String>,
        app_class: String,
        title: Option<String>,
    ) -> Self {
        Self {
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
}
