//! Focus detection backends.
//!
//! This module provides a generic abstraction for detecting window focus changes
//! across different window managers and desktop environments.

mod gnome;
mod hyprland;
mod kde;
mod niri;
mod sway;
mod x11;

use std::env;
use std::fmt;

use async_trait::async_trait;
use clap::ValueEnum;
use gnome::GnomeSource;
use hyprland::HyprlandSource;
use kde::KdeSource;
use niri::NiriSource;
use serde::Deserialize;
use serde::Serialize;
use sway::SwaySource;
use thiserror::Error;
use tracing::info;
use x11::X11Source;

/// Backend-agnostic focus event.
#[derive(Debug, Clone)]
pub struct FocusEvent {
    /// Application `class/app_id` (primary identifier for the app).
    pub app_class: String,

    /// Window title (optional, may contain sensitive info).
    pub title: Option<String>,

    /// Window identifier (backend-specific).
    pub window_id: Option<String>,
}

impl FocusEvent {
    /// Create a new focus event.
    #[must_use] 
    pub fn new(app_class: String, title: Option<String>, window_id: Option<String>) -> Self {
        Self {
            app_class,
            title,
            window_id,
        }
    }

    /// Returns true if this represents an empty/no-focus state.
    #[must_use] 
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

/// Available backend types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    /// Auto-detect the running desktop environment.
    #[default]
    Auto,
    /// Hyprland compositor.
    Hyprland,
    /// Sway compositor (i3-compatible IPC).
    Sway,
    /// GNOME Shell (Mutter).
    Gnome,
    /// KDE Plasma (`KWin`).
    Kde,
    /// Niri compositor.
    Niri,
    /// Generic X11 (fallback for any X11 window manager).
    X11,
}

impl fmt::Display for Backend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Auto => write!(f, "auto"),
            Self::Hyprland => write!(f, "hyprland"),
            Self::Sway => write!(f, "sway"),
            Self::Gnome => write!(f, "gnome"),
            Self::Kde => write!(f, "kde"),
            Self::Niri => write!(f, "niri"),
            Self::X11 => write!(f, "x11"),
        }
    }
}

impl Backend {
    /// Resolve `Auto` to a concrete backend by checking the environment.
    ///
    /// Detection order prefers Wayland-native backends over the X11 fallback:
    /// Hyprland → Sway → Niri → KDE → GNOME → X11
    pub fn detect() -> Result<Self, FocusError> {
        if env::var("HYPRLAND_INSTANCE_SIGNATURE").is_ok() || env::var("HYPRLAND_CMD").is_ok() {
            info!("Detected Hyprland environment");
            return Ok(Self::Hyprland);
        }

        if env::var("SWAYSOCK").is_ok() {
            info!("Detected Sway environment");
            return Ok(Self::Sway);
        }

        if env::var("NIRI_SOCKET").is_ok() {
            info!("Detected Niri environment");
            return Ok(Self::Niri);
        }

        if let Ok(desktop) = env::var("XDG_CURRENT_DESKTOP") {
            let desktop_upper = desktop.to_uppercase();
            if desktop_upper.contains("KDE") {
                info!("Detected KDE environment (XDG_CURRENT_DESKTOP={desktop})");
                return Ok(Self::Kde);
            }
            if desktop_upper.contains("GNOME") {
                info!("Detected GNOME environment (XDG_CURRENT_DESKTOP={desktop})");
                return Ok(Self::Gnome);
            }
        }

        if env::var("DISPLAY").is_ok() {
            info!("Detected X11 environment (fallback)");
            return Ok(Self::X11);
        }

        Err(FocusError::NoBackendDetected)
    }

    /// Resolve this backend, running auto-detection if needed.
    pub fn resolve(self) -> Result<Self, FocusError> {
        match self {
            Self::Auto => Self::detect(),
            concrete => Ok(concrete),
        }
    }
}

/// Connect to the appropriate backend and return a boxed `FocusSource`.
pub async fn connect(backend: Backend) -> Result<Box<dyn FocusSource>, FocusError> {
    let resolved = backend.resolve()?;

    match resolved {
        Backend::Hyprland => {
            let source = HyprlandSource::connect().await?;
            Ok(Box::new(source))
        }
        Backend::Sway => {
            let source = SwaySource::connect().await?;
            Ok(Box::new(source))
        }
        Backend::X11 => {
            let source = X11Source::connect().await?;
            Ok(Box::new(source))
        }
        Backend::Gnome => {
            let source = GnomeSource::connect().await?;
            Ok(Box::new(source))
        }
        Backend::Kde => {
            let source = KdeSource::connect().await?;
            Ok(Box::new(source))
        }
        Backend::Niri => {
            let source = NiriSource::connect().await?;
            Ok(Box::new(source))
        }
        Backend::Auto => unreachable!("Auto should have been resolved"),
    }
}

/// Get diagnostic information for the given backend.
#[must_use] 
pub fn diagnostics(backend: Backend) -> Vec<String> {
    match backend {
        Backend::Auto => {
            let mut diags = vec!["Backend: auto-detect".to_string()];
            match Backend::detect() {
                Ok(detected) => {
                    diags.push(format!("Detected: {detected}"));
                    diags.extend(diagnostics(detected));
                }
                Err(e) => diags.push(format!("Detection failed: {e}")),
            }
            diags
        }
        Backend::Hyprland => HyprlandSource::get_diagnostics(),
        Backend::Sway => SwaySource::get_diagnostics(),
        Backend::Gnome => GnomeSource::get_diagnostics(),
        Backend::Kde => KdeSource::get_diagnostics(),
        Backend::Niri => NiriSource::get_diagnostics(),
        Backend::X11 => X11Source::get_diagnostics(),
    }
}

/// Errors that can occur in focus detection.
#[derive(Error, Debug)]
pub enum FocusError {
    #[error("Connection failed: {0}")]
    ConnectionFailed(String),

    #[error("Environment variable not set: {0}")]
    EnvVarNotSet(String),

    #[error("Socket path not found: {0}")]
    SocketNotFound(String),

    #[error("Could not detect a supported desktop environment")]
    NoBackendDetected,
}
