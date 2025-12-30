//! WakaTime CLI invocation.
//!
//! Builds and spawns wakatime-cli commands for sending heartbeats.

use crate::config::Config;
use crate::domain::Heartbeat;
use anyhow::{Context, Result};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU32, Ordering};
use tokio::process::Command;
use tracing::{debug, error, info, trace, warn};

/// Rate limiter for error logging.
static ERROR_LOG_COUNT: AtomicU32 = AtomicU32::new(0);
const ERROR_LOG_RATE_LIMIT: u32 = 10; // Log every Nth error after initial burst

/// WakaTime CLI client.
#[derive(Debug)]
pub struct WakaTimeClient {
    /// Path to wakatime-cli binary.
    cli_path: PathBuf,

    /// Optional config file path.
    config_path: Option<PathBuf>,

    /// Dry run mode.
    dry_run: bool,
}

impl WakaTimeClient {
    /// Create a new WakaTime client from config.
    pub fn from_config(config: &Config) -> Result<Self> {
        let cli_path = find_wakatime_cli(config.wakatime_cli_path.as_ref())?;
        info!("Using wakatime-cli: {}", cli_path.display());

        Ok(Self {
            cli_path,
            config_path: config.wakatime_config_path.clone(),
            dry_run: config.dry_run,
        })
    }

    /// Send a heartbeat.
    ///
    /// This spawns wakatime-cli asynchronously and does not block.
    pub async fn send_heartbeat(&self, heartbeat: &Heartbeat) -> Result<()> {
        let mut args = vec![
            "--entity-type",
            "app",
            "--entity",
            heartbeat.entity.as_str(),
            "--plugin",
            concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION")),
            "--category",
            heartbeat.category.as_str(),
        ];

        if let Some(ref config_path) = self.config_path {
            args.push("--config");
            args.push(config_path.to_str().unwrap_or(""));
        }

        if self.dry_run {
            info!(
                "[DRY RUN] Would execute: {} {}",
                self.cli_path.display(),
                args.join(" ")
            );
            return Ok(());
        }

        debug!(
            "Sending heartbeat: {} {}",
            self.cli_path.display(),
            args.join(" ")
        );

        let result = Command::new(&self.cli_path)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("Failed to spawn wakatime-cli")?
            .wait_with_output()
            .await
            .context("Failed to wait for wakatime-cli")?;

        if result.status.success() {
            trace!("wakatime-cli succeeded");
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&result.stderr);

            // Rate-limit error logging
            let count = ERROR_LOG_COUNT.fetch_add(1, Ordering::Relaxed);
            if count < 5 || count.is_multiple_of(ERROR_LOG_RATE_LIMIT) {
                error!(
                    "wakatime-cli failed (exit code {:?}): {}",
                    result.status.code(),
                    stderr.trim()
                );
                if count == 5 {
                    warn!(
                        "Rate-limiting error logs (showing every {}th error)",
                        ERROR_LOG_RATE_LIMIT
                    );
                }
            }

            anyhow::bail!("wakatime-cli exited with code {:?}", result.status.code())
        }
    }
}

/// Find the wakatime-cli binary.
fn find_wakatime_cli(configured_path: Option<&PathBuf>) -> Result<PathBuf> {
    // Use configured path if provided
    if let Some(path) = configured_path {
        if path.exists() {
            return Ok(path.clone());
        }
        anyhow::bail!(
            "Configured wakatime-cli path does not exist: {}",
            path.display()
        );
    }

    // Search PATH
    if let Ok(path) = which::which("wakatime-cli") {
        return Ok(path);
    }

    // Check ~/.wakatime/
    if let Some(home) = dirs::home_dir() {
        let wakatime_dir = home.join(".wakatime");

        // Try exact name first
        let exact = wakatime_dir.join("wakatime-cli");
        if exact.exists() {
            return Ok(exact);
        }

        // Try with platform suffix (e.g., wakatime-cli-linux-amd64)
        if let Ok(entries) = std::fs::read_dir(&wakatime_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if name_str.starts_with("wakatime-cli")
                    && !name_str.ends_with(".zip")
                    && let Ok(meta) = entry.metadata()
                    && meta.is_file()
                {
                    return Ok(entry.path());
                }
            }
        }
    }

    anyhow::bail!(
        "wakatime-cli not found. Install it or set wakatime_cli_path in config. \
         See https://wakatime.com/terminal"
    )
}
