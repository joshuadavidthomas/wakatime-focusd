//! WakaTime CLI invocation.
//!
//! Builds and spawns wakatime-cli commands for sending heartbeats.

use crate::config::Config;
use anyhow::{Context, Result};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};
use tokio::process::Command;
use tracing::{debug, error, info, trace, warn};

/// Plugin identifier for WakaTime.
const PLUGIN_NAME: &str = "wakatime-focusd";

/// Get the current version from Cargo.toml.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

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

    /// Category for heartbeats.
    category: String,

    /// Dry run mode.
    dry_run: bool,

    /// Last successful send time (for health checks).
    last_success: Option<Instant>,

    /// Count of consecutive failures.
    consecutive_failures: u32,
}

impl WakaTimeClient {
    /// Create a new WakaTime client from config.
    pub fn from_config(config: &Config) -> Result<Self> {
        let cli_path = find_wakatime_cli(config.wakatime_cli_path.as_ref())?;
        info!("Using wakatime-cli: {}", cli_path.display());

        Ok(Self {
            cli_path,
            config_path: config.wakatime_config_path.clone(),
            category: config.category.clone(),
            dry_run: config.dry_run,
            last_success: None,
            consecutive_failures: 0,
        })
    }

    /// Send a heartbeat for the given entity.
    ///
    /// This spawns wakatime-cli asynchronously and does not block.
    pub async fn send_heartbeat(&mut self, entity: &str) -> Result<()> {
        let args = self.build_args(entity);

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
            self.last_success = Some(Instant::now());
            self.consecutive_failures = 0;
            Ok(())
        } else {
            self.consecutive_failures += 1;
            let stderr = String::from_utf8_lossy(&result.stderr);

            // Rate-limit error logging
            let count = ERROR_LOG_COUNT.fetch_add(1, Ordering::Relaxed);
            if count < 5 || count % ERROR_LOG_RATE_LIMIT == 0 {
                error!(
                    "wakatime-cli failed (exit code {:?}): {}",
                    result.status.code(),
                    stderr.trim()
                );
                if count == 5 {
                    warn!("Rate-limiting error logs (showing every {}th error)", ERROR_LOG_RATE_LIMIT);
                }
            }

            anyhow::bail!(
                "wakatime-cli exited with code {:?}",
                result.status.code()
            )
        }
    }

    /// Build command line arguments for wakatime-cli.
    fn build_args(&self, entity: &str) -> Vec<String> {
        let mut args = vec![
            "--entity-type".to_string(),
            "app".to_string(),
            "--entity".to_string(),
            entity.to_string(),
            "--plugin".to_string(),
            format!("{}/{}", PLUGIN_NAME, version()),
            "--category".to_string(),
            self.category.clone(),
        ];

        if let Some(ref config_path) = self.config_path {
            args.push("--config".to_string());
            args.push(config_path.display().to_string());
        }

        args
    }

    /// Get time since last successful heartbeat.
    #[allow(dead_code)]
    pub fn time_since_last_success(&self) -> Option<Duration> {
        self.last_success.map(|t| t.elapsed())
    }

    /// Get count of consecutive failures.
    #[allow(dead_code)]
    pub fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures
    }

    /// Check if client appears healthy.
    #[allow(dead_code)]
    pub fn is_healthy(&self) -> bool {
        self.consecutive_failures < 10
    }
}

/// Find the wakatime-cli binary.
fn find_wakatime_cli(configured_path: Option<&PathBuf>) -> Result<PathBuf> {
    // Use configured path if provided
    if let Some(path) = configured_path {
        if path.exists() {
            return Ok(path.clone());
        }
        anyhow::bail!("Configured wakatime-cli path does not exist: {}", path.display());
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
                if name_str.starts_with("wakatime-cli") && !name_str.ends_with(".zip") {
                    if let Ok(meta) = entry.metadata() {
                        if meta.is_file() {
                            return Ok(entry.path());
                        }
                    }
                }
            }
        }
    }

    anyhow::bail!(
        "wakatime-cli not found. Install it or set wakatime_cli_path in config. \
         See https://wakatime.com/terminal"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version() {
        let v = version();
        assert!(!v.is_empty());
        assert!(v.contains('.')); // Should be semver like "0.1.0"
    }

    #[test]
    fn test_build_args() {
        let client = WakaTimeClient {
            cli_path: PathBuf::from("/usr/bin/wakatime-cli"),
            config_path: None,
            category: "coding".to_string(),
            dry_run: false,
            last_success: None,
            consecutive_failures: 0,
        };

        let args = client.build_args("firefox");
        assert!(args.contains(&"--entity-type".to_string()));
        assert!(args.contains(&"app".to_string()));
        assert!(args.contains(&"--entity".to_string()));
        assert!(args.contains(&"firefox".to_string()));
        assert!(args.contains(&"--plugin".to_string()));
        assert!(args.contains(&"--category".to_string()));
        assert!(args.contains(&"coding".to_string()));
    }

    #[test]
    fn test_build_args_with_config() {
        let client = WakaTimeClient {
            cli_path: PathBuf::from("/usr/bin/wakatime-cli"),
            config_path: Some(PathBuf::from("/home/user/.wakatime.cfg")),
            category: "browsing".to_string(),
            dry_run: false,
            last_success: None,
            consecutive_failures: 0,
        };

        let args = client.build_args("chromium");
        assert!(args.contains(&"--config".to_string()));
        assert!(args.contains(&"/home/user/.wakatime.cfg".to_string()));
        assert!(args.contains(&"browsing".to_string()));
    }

    #[test]
    fn test_health_tracking() {
        let mut client = WakaTimeClient {
            cli_path: PathBuf::from("/usr/bin/wakatime-cli"),
            config_path: None,
            category: "coding".to_string(),
            dry_run: false,
            last_success: None,
            consecutive_failures: 0,
        };

        assert!(client.is_healthy());
        assert_eq!(client.consecutive_failures(), 0);

        // Simulate failures
        for _ in 0..9 {
            client.consecutive_failures += 1;
        }
        assert!(client.is_healthy());

        client.consecutive_failures += 1;
        assert!(!client.is_healthy());
    }
}
