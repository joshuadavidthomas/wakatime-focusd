//! KDE Plasma (`KWin`) focus detection backend.
//!
//! Loads a `KWin` script that monitors `workspace.windowActivated` and sends
//! focus change notifications back via D-Bus. This is the standard approach
//! since `KWin` doesn't expose active-window-changed signals directly.

use std::env;
use std::path::PathBuf;

use futures_util::future::BoxFuture;
use tokio::sync::mpsc;
use tracing::debug;
use tracing::info;
use tracing::warn;
use zbus::Connection;
use zbus::connection::Builder;
use zbus::proxy;
use zbus::zvariant::ObjectPath;

use super::FocusError;
use super::FocusEvent;
use super::FocusSource;

/// D-Bus service name for receiving `KWin` script callbacks.
const DBUS_SERVICE_NAME: &str = "org.wakatime.focusd";

/// D-Bus object path for receiving `KWin` script callbacks.
const DBUS_OBJECT_PATH: &str = "/org/wakatime/focusd";

/// Name used when loading the `KWin` script.
const KWIN_SCRIPT_NAME: &str = "wakatime-focusd";

/// `KWin` script content that monitors focus changes and calls back via D-Bus.
const KWIN_SCRIPT: &str = r#"
function notifyFocusChange(window) {
    if (window) {
        callDBus(
            "org.wakatime.focusd",
            "/org/wakatime/focusd",
            "org.wakatime.focusd.FocusReceiver",
            "FocusChanged",
            window.resourceClass || "",
            window.caption || "",
            window.internalId ? window.internalId.toString() : ""
        );
    }
}

workspace.windowActivated.connect(notifyFocusChange);

if (workspace.activeWindow) {
    notifyFocusChange(workspace.activeWindow);
}
"#;

/// D-Bus proxy for the `KWin` scripting interface.
#[proxy(
    interface = "org.kde.kwin.Scripting",
    default_service = "org.kde.KWin",
    default_path = "/Scripting"
)]
trait KWinScripting {
    fn load_script(&self, path: String, name: String) -> zbus::Result<i32>;
    fn unload_script(&self, name: String) -> zbus::Result<bool>;
}

/// D-Bus proxy for a loaded `KWin` script instance.
#[proxy(interface = "org.kde.kwin.Script", default_service = "org.kde.KWin")]
trait KWinScript {
    fn run(&self) -> zbus::Result<()>;
}

/// D-Bus object that receives focus change callbacks from the `KWin` script.
struct FocusReceiver {
    tx: mpsc::Sender<FocusEvent>,
}

#[zbus::interface(name = "org.wakatime.focusd.FocusReceiver")]
impl FocusReceiver {
    async fn focus_changed(&self, class: String, title: String, window_id: String) {
        let title = if title.is_empty() { None } else { Some(title) };
        let window_id = if window_id.is_empty() {
            None
        } else {
            Some(window_id)
        };
        let event = FocusEvent::new(class, title, window_id);
        if self.tx.send(event).await.is_err() {
            warn!("Focus event channel closed");
        }
    }
}

/// KDE Plasma focus source implementation.
pub struct KdeSource {
    rx: mpsc::Receiver<FocusEvent>,
    // Hold connection to keep the D-Bus service alive.
    // Not read directly but must stay alive for the D-Bus service.
    #[allow(dead_code)]
    connection: Connection,
    script_path: PathBuf,
}

impl KdeSource {
    /// Create a new KDE focus source.
    pub async fn connect() -> Result<Self, FocusError> {
        let (tx, rx) = mpsc::channel(32);

        // Register our D-Bus service to receive callbacks
        let connection = Builder::session()
            .map_err(|e| FocusError::ConnectionFailed(format!("D-Bus builder: {e}")))?
            .name(DBUS_SERVICE_NAME)
            .map_err(|e| FocusError::ConnectionFailed(format!("D-Bus name: {e}")))?
            .serve_at(DBUS_OBJECT_PATH, FocusReceiver { tx })
            .map_err(|e| FocusError::ConnectionFailed(format!("D-Bus serve: {e}")))?
            .build()
            .await
            .map_err(|e| FocusError::ConnectionFailed(format!("D-Bus connect: {e}")))?;

        // Write the KWin script to a temp file
        let script_path = write_kwin_script()?;

        // Load and run the script
        let scripting_proxy = KWinScriptingProxy::new(&connection)
            .await
            .map_err(|e| FocusError::ConnectionFailed(format!("KWin scripting proxy: {e}")))?;

        // Unload any previous instance
        let _ = scripting_proxy
            .unload_script(KWIN_SCRIPT_NAME.to_string())
            .await;

        let script_id = scripting_proxy
            .load_script(
                script_path.display().to_string(),
                KWIN_SCRIPT_NAME.to_string(),
            )
            .await
            .map_err(|e| FocusError::ConnectionFailed(format!("Load script: {e}")))?;

        debug!("Loaded KWin script with ID {script_id}");

        let script_object_path = format!("/Scripting/Script{script_id}");
        let script_proxy = KWinScriptProxy::builder(&connection)
            .path(
                ObjectPath::try_from(script_object_path.as_str())
                    .map_err(|e| FocusError::ConnectionFailed(format!("Script path: {e}")))?,
            )
            .map_err(|e| FocusError::ConnectionFailed(format!("Script proxy builder: {e}")))?
            .build()
            .await
            .map_err(|e| FocusError::ConnectionFailed(format!("Script proxy: {e}")))?;

        script_proxy
            .run()
            .await
            .map_err(|e| FocusError::ConnectionFailed(format!("Run script: {e}")))?;

        info!("KWin focus monitoring script loaded and running");

        Ok(Self {
            rx,
            connection,
            script_path,
        })
    }

    /// Get diagnostic information about the KDE environment.
    pub fn get_diagnostics() -> Vec<String> {
        let mut diags = Vec::new();

        match env::var("XDG_CURRENT_DESKTOP") {
            Ok(v) => diags.push(format!("XDG_CURRENT_DESKTOP={v}")),
            Err(_) => diags.push("XDG_CURRENT_DESKTOP: NOT SET".to_string()),
        }

        match env::var("KDE_SESSION_VERSION") {
            Ok(v) => diags.push(format!("KDE_SESSION_VERSION={v}")),
            Err(_) => diags.push("KDE_SESSION_VERSION: NOT SET".to_string()),
        }

        diags
    }
}

impl Drop for KdeSource {
    fn drop(&mut self) {
        // Clean up the temp script file
        if self.script_path.exists() {
            let _ = std::fs::remove_file(&self.script_path);
        }
    }
}

impl FocusSource for KdeSource {
    fn next_event(&mut self) -> BoxFuture<'_, Result<FocusEvent, FocusError>> {
        Box::pin(async move {
            loop {
                let event = self.rx.recv().await.ok_or_else(|| {
                    FocusError::ConnectionFailed("KWin callback channel closed".to_string())
                })?;

                // Skip empty focus events
                if event.is_empty() {
                    continue;
                }

                debug!(
                    "Focus changed: class={}, title={:?}, window_id={:?}",
                    event.app_class, event.title, event.window_id
                );

                return Ok(event);
            }
        })
    }
}

/// Write the `KWin` script to a temporary file.
fn write_kwin_script() -> Result<PathBuf, FocusError> {
    let dir = env::temp_dir().join("wakatime-focusd");
    std::fs::create_dir_all(&dir)
        .map_err(|e| FocusError::ConnectionFailed(format!("Create script dir: {e}")))?;

    let path = dir.join("focus-monitor.js");
    std::fs::write(&path, KWIN_SCRIPT)
        .map_err(|e| FocusError::ConnectionFailed(format!("Write script: {e}")))?;

    debug!("Wrote KWin script to {}", path.display());
    Ok(path)
}
