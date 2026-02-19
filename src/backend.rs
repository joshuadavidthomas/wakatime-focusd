//! Focus detection backends.
//!
//! This module provides a generic abstraction for detecting window focus changes
//! across different window managers and desktop environments.

mod hyprland;

use async_trait::async_trait;
pub use hyprland::HyprlandSource;
use thiserror::Error;

/// Backend-agnostic focus event.
#[derive(Debug, Clone)]
pub struct FocusEvent {
    /// Application class/app_id (primary identifier for the app).
    pub app_class: String,

    /// Window title (optional, may contain sensitive info).
    pub title: Option<String>,

    /// Window identifier (backend-specific).
    pub window_id: Option<String>,
}

impl FocusEvent {
    /// Create a new focus event.
    pub fn new(app_class: String, title: Option<String>, window_id: Option<String>) -> Self {
        Self {
            app_class,
            title,
            window_id,
        }
    }

    /// Returns true if this represents an empty/no-focus state.
    pub fn is_empty(&self) -> bool {
        self.app_class.is_empty()
    }
}

/// Trait for focus event sources.
#[async_trait]
pub trait FocusSource: Send {
    /// Get the next focus event.
    ///
    /// This method blocks until a focus event occurs or an error happens.
    /// Implementations should handle reconnection internally.
    async fn next_event(&mut self) -> Result<FocusEvent, FocusError>;
}

/// Errors that can occur in focus detection.
#[derive(Error, Debug)]
pub enum FocusError {
    #[error("Socket connection failed: {0}")]
    ConnectionFailed(String),

    #[error("Environment variable not set: {0}")]
    EnvVarNotSet(String),

    #[error("Socket path not found: {0}")]
    SocketNotFound(String),
}
