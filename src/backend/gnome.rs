//! GNOME Shell focus detection backend.
//!
//! Uses the `org.gnome.Shell.Introspect` D-Bus interface to detect window focus
//! changes. Listens for `WindowsChanged` signals and queries `GetWindows()` to
//! find the currently focused window.

use std::collections::HashMap;
use std::env;

use futures_util::future::BoxFuture;
use tracing::debug;
use tracing::info;
use tracing::trace;
use zbus::Connection;
use zbus::proxy;
use zbus::zvariant::OwnedValue;

use super::FocusError;
use super::FocusEvent;
use super::FocusSource;

/// D-Bus proxy for `org.gnome.Shell.Introspect`.
#[proxy(
    interface = "org.gnome.Shell.Introspect",
    default_service = "org.gnome.Shell.Introspect",
    default_path = "/org/gnome/Shell/Introspect"
)]
trait ShellIntrospect {
    /// Get all windows with their properties.
    #[zbus(name = "GetWindows")]
    fn get_windows(&self) -> zbus::Result<HashMap<u64, HashMap<String, OwnedValue>>>;

    /// Signal emitted when windows change (including focus changes).
    #[zbus(signal)]
    fn windows_changed();
}

/// GNOME Shell focus source implementation.
pub struct GnomeSource {
    proxy: ShellIntrospectProxy<'static>,
    last_focused_class: Option<String>,
}

impl GnomeSource {
    /// Create a new GNOME Shell focus source.
    pub async fn connect() -> Result<Self, FocusError> {
        let connection = Connection::session()
            .await
            .map_err(|e| FocusError::ConnectionFailed(format!("D-Bus session connect: {e}")))?;

        let proxy = ShellIntrospectProxy::new(&connection)
            .await
            .map_err(|e| FocusError::ConnectionFailed(format!("Introspect proxy: {e}")))?;

        // Verify we can access the interface
        proxy
            .get_windows()
            .await
            .map_err(|e| FocusError::ConnectionFailed(format!("GetWindows failed: {e}")))?;

        info!("Connected to GNOME Shell Introspect D-Bus interface");

        Ok(Self {
            proxy,
            last_focused_class: None,
        })
    }

    /// Get diagnostic information about the GNOME environment.
    pub fn get_diagnostics() -> Vec<String> {
        let mut diags = Vec::new();

        match env::var("XDG_CURRENT_DESKTOP") {
            Ok(v) => diags.push(format!("XDG_CURRENT_DESKTOP={v}")),
            Err(_) => diags.push("XDG_CURRENT_DESKTOP: NOT SET".to_string()),
        }

        match env::var("GNOME_DESKTOP_SESSION_ID") {
            Ok(v) => diags.push(format!("GNOME_DESKTOP_SESSION_ID={v}")),
            Err(_) => diags.push("GNOME_DESKTOP_SESSION_ID: NOT SET".to_string()),
        }

        diags
    }

    /// Query `GetWindows()` and find the focused window.
    async fn get_focused_window(&self) -> Result<Option<FocusEvent>, FocusError> {
        let windows = self
            .proxy
            .get_windows()
            .await
            .map_err(|e| FocusError::ConnectionFailed(format!("GetWindows failed: {e}")))?;

        Ok(find_focused_window(&windows))
    }
}

impl FocusSource for GnomeSource {
    fn next_event(&mut self) -> BoxFuture<'_, Result<FocusEvent, FocusError>> {
        Box::pin(async move {
            // Get initial focus
            if let Some(event) = self.get_focused_window().await?
                && self.last_focused_class.as_deref() != Some(&event.app_class)
            {
                self.last_focused_class = Some(event.app_class.clone());
                debug!(
                    "Initial focus: class={}, title={:?}",
                    event.app_class, event.title
                );
                return Ok(event);
            }

            // Listen for WindowsChanged signals
            let mut stream = self
                .proxy
                .receive_windows_changed()
                .await
                .map_err(|e| FocusError::ConnectionFailed(format!("Signal subscribe: {e}")))?;

            loop {
                use futures_util::StreamExt;
                let signal = stream.next().await;

                if signal.is_none() {
                    return Err(FocusError::ConnectionFailed(
                        "WindowsChanged signal stream ended".to_string(),
                    ));
                }

                trace!("WindowsChanged signal received");

                if let Some(event) = self.get_focused_window().await? {
                    // Only emit if focus actually changed
                    if self.last_focused_class.as_deref() != Some(&event.app_class) {
                        self.last_focused_class = Some(event.app_class.clone());
                        debug!(
                            "Focus changed: class={}, title={:?}, window_id={:?}",
                            event.app_class, event.title, event.window_id
                        );
                        return Ok(event);
                    }
                } else if self.last_focused_class.is_some() {
                    // Focus went to nothing (e.g., desktop)
                    self.last_focused_class = None;
                    trace!("Focus cleared (no focused window)");
                }
            }
        })
    }
}

/// Find the focused window from a `GetWindows()` response.
fn find_focused_window(windows: &HashMap<u64, HashMap<String, OwnedValue>>) -> Option<FocusEvent> {
    for (window_id, props) in windows {
        let has_focus = props
            .get("has-focus")
            .and_then(|v| <bool>::try_from(v.clone()).ok())
            .unwrap_or(false);

        if !has_focus {
            continue;
        }

        // Prefer wm-class, fall back to app-id
        let app_class = props
            .get("wm-class")
            .and_then(|v| <String>::try_from(v.clone()).ok())
            .filter(|s| !s.is_empty())
            .or_else(|| {
                props
                    .get("app-id")
                    .and_then(|v| <String>::try_from(v.clone()).ok())
                    .filter(|s| !s.is_empty())
            })?;

        let title = props
            .get("title")
            .and_then(|v| <String>::try_from(v.clone()).ok())
            .filter(|s| !s.is_empty());

        return Some(FocusEvent::new(
            app_class,
            title,
            Some(window_id.to_string()),
        ));
    }

    None
}

#[cfg(test)]
mod tests {
    use zbus::zvariant::Value;

    use super::*;

    fn make_window(
        has_focus: bool,
        wm_class: &str,
        title: &str,
        app_id: Option<&str>,
    ) -> HashMap<String, OwnedValue> {
        let mut props = HashMap::new();
        props.insert(
            "has-focus".to_string(),
            Value::from(has_focus).try_into().unwrap(),
        );
        props.insert(
            "wm-class".to_string(),
            Value::from(wm_class).try_into().unwrap(),
        );
        props.insert("title".to_string(), Value::from(title).try_into().unwrap());
        if let Some(id) = app_id {
            props.insert("app-id".to_string(), Value::from(id).try_into().unwrap());
        }
        props
    }

    #[test]
    fn test_find_focused_window() {
        let mut windows = HashMap::new();
        windows.insert(1, make_window(false, "Firefox", "Google", None));
        windows.insert(2, make_window(true, "kitty", "~/projects", None));
        windows.insert(3, make_window(false, "Slack", "general", None));

        let event = find_focused_window(&windows).expect("Should find focused window");
        assert_eq!(event.app_class, "kitty");
        assert_eq!(event.title, Some("~/projects".to_string()));
        assert_eq!(event.window_id, Some("2".to_string()));
    }

    #[test]
    fn test_find_focused_no_focus() {
        let mut windows = HashMap::new();
        windows.insert(1, make_window(false, "Firefox", "Google", None));
        windows.insert(2, make_window(false, "kitty", "~/projects", None));

        assert!(find_focused_window(&windows).is_none());
    }

    #[test]
    fn test_find_focused_empty_class_falls_back_to_app_id() {
        let mut windows = HashMap::new();
        windows.insert(
            1,
            make_window(true, "", "Nautilus", Some("org.gnome.Nautilus")),
        );

        let event = find_focused_window(&windows).expect("Should fall back to app-id");
        assert_eq!(event.app_class, "org.gnome.Nautilus");
    }

    #[test]
    fn test_find_focused_empty_title() {
        let mut windows = HashMap::new();
        windows.insert(1, make_window(true, "dmenu", "", None));

        let event = find_focused_window(&windows).expect("Should parse with empty title");
        assert_eq!(event.app_class, "dmenu");
        assert_eq!(event.title, None);
    }

    #[test]
    fn test_find_focused_no_windows() {
        let windows = HashMap::new();
        assert!(find_focused_window(&windows).is_none());
    }
}
