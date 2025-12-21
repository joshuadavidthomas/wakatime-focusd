//! Idle detection via systemd-logind DBus interface.
//!
//! Polls IdleHint property from the current session.

use anyhow::{Context, Result};
use std::env;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::{debug, error, info, trace, warn};
use zbus::Connection;

/// DBus interface for login1 Manager.
const LOGIND_SERVICE: &str = "org.freedesktop.login1";
const LOGIND_PATH: &str = "/org/freedesktop/login1";
const MANAGER_INTERFACE: &str = "org.freedesktop.login1.Manager";
const SESSION_INTERFACE: &str = "org.freedesktop.login1.Session";
const PROPERTIES_INTERFACE: &str = "org.freedesktop.DBus.Properties";

/// Idle monitor that polls systemd-logind for idle state.
pub struct IdleMonitor {
    /// Cached idle state.
    idle_hint: Arc<AtomicBool>,

    /// Session object path in DBus.
    session_path: Arc<RwLock<Option<String>>>,

    /// Whether idle monitoring is available/enabled.
    enabled: Arc<AtomicBool>,
}

impl IdleMonitor {
    /// Create a new idle monitor.
    pub fn new() -> Self {
        Self {
            idle_hint: Arc::new(AtomicBool::new(false)),
            session_path: Arc::new(RwLock::new(None)),
            enabled: Arc::new(AtomicBool::new(true)),
        }
    }

    /// Check if the session is currently idle.
    ///
    /// Returns the cached idle state. Use `poll_idle_state` to refresh.
    /// Returns false if idle monitoring is disabled.
    pub fn is_idle(&self) -> bool {
        if !self.enabled.load(Ordering::Relaxed) {
            return false; // If disabled, assume not idle
        }
        self.idle_hint.load(Ordering::Relaxed)
    }

    /// Initialize the monitor by resolving the session path.
    pub async fn init(&self) -> Result<()> {
        let conn = Connection::system()
            .await
            .context("Failed to connect to system DBus")?;

        let session_path = resolve_session_path(&conn).await?;
        info!("Resolved session path: {}", session_path);

        *self.session_path.write().await = Some(session_path);
        Ok(())
    }

    /// Poll the current idle state from DBus.
    ///
    /// This updates the cached idle_hint value.
    pub async fn poll_idle_state(&self) -> Result<bool> {
        let session_path = self.session_path.read().await;
        let Some(ref path) = *session_path else {
            return Ok(false); // Not initialized
        };

        let conn = Connection::system()
            .await
            .context("Failed to connect to system DBus")?;

        let idle = get_idle_hint(&conn, path).await?;
        let prev = self.idle_hint.swap(idle, Ordering::Relaxed);

        if idle != prev {
            debug!("Idle state changed: {} -> {}", prev, idle);
        } else {
            trace!("Idle state: {}", idle);
        }

        Ok(idle)
    }

    /// Disable idle monitoring (fallback mode).
    pub fn disable(&self) {
        warn!("Disabling idle monitoring");
        self.enabled.store(false, Ordering::Relaxed);
    }

    /// Start background polling task.
    ///
    /// Polls idle state at the specified interval and updates the cache.
    pub fn start_polling(self: Arc<Self>, interval: Duration) {
        let monitor = self.clone();

        tokio::spawn(async move {
            // Try to initialize
            if let Err(e) = monitor.init().await {
                error!("Failed to initialize idle monitor: {}. Disabling idle gating.", e);
                monitor.disable();
                return;
            }

            info!("Idle monitor started, polling every {:?}", interval);

            loop {
                if let Err(e) = monitor.poll_idle_state().await {
                    warn!("Failed to poll idle state: {}", e);
                    // Don't disable on transient errors, just log
                }

                tokio::time::sleep(interval).await;
            }
        });
    }
}

impl Default for IdleMonitor {
    fn default() -> Self {
        Self::new()
    }
}

/// Resolve the session object path for the current session.
async fn resolve_session_path(conn: &Connection) -> Result<String> {
    // First try XDG_SESSION_ID if available
    if let Ok(session_id) = env::var("XDG_SESSION_ID") {
        debug!("Using XDG_SESSION_ID: {}", session_id);
        return get_session_by_id(conn, &session_id).await;
    }

    // Fall back to getting sessions for current user
    debug!("XDG_SESSION_ID not set, trying to find current session");

    // Try "self" or "auto" session
    let self_path = format!("{}/session/self", LOGIND_PATH);

    // Check if self path works by trying to read a property
    if check_session_exists(conn, &self_path).await {
        return Ok(self_path);
    }

    // Try auto
    let auto_path = format!("{}/session/auto", LOGIND_PATH);
    if check_session_exists(conn, &auto_path).await {
        return Ok(auto_path);
    }

    anyhow::bail!("Could not resolve session path. Set XDG_SESSION_ID or ensure logind session is available.")
}

/// Get session object path by session ID via Manager.GetSession.
async fn get_session_by_id(conn: &Connection, session_id: &str) -> Result<String> {
    let proxy = zbus::Proxy::new(
        conn,
        LOGIND_SERVICE,
        LOGIND_PATH,
        MANAGER_INTERFACE,
    )
    .await
    .context("Failed to create Manager proxy")?;

    let path: zbus::zvariant::OwnedObjectPath = proxy
        .call("GetSession", &(session_id,))
        .await
        .context("GetSession call failed")?;

    Ok(path.to_string())
}

/// Check if a session path exists by trying to get IdleHint.
async fn check_session_exists(conn: &Connection, path: &str) -> bool {
    get_idle_hint(conn, path).await.is_ok()
}

/// Get the IdleHint property from a session.
async fn get_idle_hint(conn: &Connection, session_path: &str) -> Result<bool> {
    let proxy = zbus::Proxy::new(
        conn,
        LOGIND_SERVICE,
        session_path,
        PROPERTIES_INTERFACE,
    )
    .await
    .context("Failed to create Properties proxy")?;

    let value: zbus::zvariant::OwnedValue = proxy
        .call("Get", &(SESSION_INTERFACE, "IdleHint"))
        .await
        .context("Failed to get IdleHint property")?;

    let idle: bool = value
        .downcast_ref::<bool>()
        .map_err(|_| anyhow::anyhow!("IdleHint is not a boolean"))?;

    Ok(idle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_idle_monitor_default_state() {
        let monitor = IdleMonitor::new();
        assert!(!monitor.is_idle());
    }

    #[test]
    fn test_idle_monitor_disable() {
        let monitor = IdleMonitor::new();
        // Set idle hint to true
        monitor.idle_hint.store(true, Ordering::Relaxed);
        assert!(monitor.is_idle());

        // After disable, is_idle returns false regardless of hint
        monitor.disable();
        assert!(!monitor.is_idle());
    }

    #[test]
    fn test_idle_hint_atomic() {
        let monitor = IdleMonitor::new();
        monitor.idle_hint.store(true, Ordering::Relaxed);
        assert!(monitor.is_idle());
        monitor.idle_hint.store(false, Ordering::Relaxed);
        assert!(!monitor.is_idle());
    }
}
