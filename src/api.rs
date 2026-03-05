//! Direct `WakaTime` API heartbeat sender.
//!
//! Sends heartbeats via HTTP POST instead of spawning wakatime-cli.

use std::sync::atomic::AtomicU32;
use std::sync::atomic::Ordering;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use anyhow::Context;
use anyhow::Result;
use futures_util::future::BoxFuture;
use reqwest::Client;
use reqwest::StatusCode;
use serde::Serialize;
use tracing::debug;
use tracing::error;
use tracing::info;
use tracing::trace;
use tracing::warn;

use crate::api_key;
use crate::config::Config;
use crate::domain::Heartbeat;
use crate::wakatime::HeartbeatSender;

/// Default `WakaTime` API base URL.
const DEFAULT_API_URL: &str = "https://api.wakatime.com/api";

/// Log every Nth error after an initial burst of 5.
const ERROR_LOG_RATE_LIMIT: u32 = 10;

/// Plugin identifier sent with each heartbeat.
const PLUGIN_ID: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"));

/// JSON payload for a single heartbeat POST.
#[derive(Debug, Serialize)]
struct HeartbeatPayload<'a> {
    entity: &'a str,
    #[serde(rename = "type")]
    entity_type: &'static str,
    category: &'a str,
    time: f64,
    plugin: &'static str,
}

/// Direct `WakaTime` API sender.
#[derive(Debug)]
pub struct ApiSender {
    /// HTTP client (reused for connection pooling).
    client: Client,

    /// Full heartbeat endpoint URL.
    heartbeat_url: String,

    /// API key for authentication.
    api_key: String,

    /// Dry run mode.
    dry_run: bool,

    /// Per-instance error log counter for rate limiting.
    error_log_count: AtomicU32,
}

impl ApiSender {
    /// Create a new API sender from config.
    ///
    /// Resolves the API key (from env var or `~/.wakatime.cfg`) and the API
    /// base URL (from daemon config, `~/.wakatime.cfg`, or the default).
    pub fn from_config(config: &Config) -> Result<Self> {
        let api_key = api_key::resolve_api_key(config.wakatime_config_path.as_deref())
            .context("Failed to resolve WakaTime API key")?;

        let base_url = Self::resolve_api_url(config);
        let base_url = base_url.trim_end_matches('/');
        let heartbeat_url = format!("{base_url}/v1/users/current/heartbeats");

        let client = Client::builder()
            .user_agent(PLUGIN_ID)
            .build()
            .context("Failed to build HTTP client")?;

        info!("Using WakaTime API: {heartbeat_url}");

        Ok(Self {
            client,
            heartbeat_url,
            api_key,
            dry_run: config.dry_run,
            error_log_count: AtomicU32::new(0),
        })
    }

    /// Resolve the API base URL from config sources.
    ///
    /// Priority: daemon config `api_url` > `~/.wakatime.cfg` `api_url` > default.
    fn resolve_api_url(config: &Config) -> String {
        if let Some(ref url) = config.api_url {
            return url.clone();
        }

        if let Some(url) =
            api_key::read_api_url_from_wakatime_config(config.wakatime_config_path.as_deref())
        {
            return url;
        }

        DEFAULT_API_URL.to_string()
    }

    /// Send a heartbeat to the `WakaTime` API.
    pub async fn send_heartbeat(&self, heartbeat: &Heartbeat) -> Result<()> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("System time before UNIX epoch")?;

        let payload = HeartbeatPayload {
            entity: heartbeat.entity.as_str(),
            entity_type: "app",
            category: heartbeat.category.as_str(),
            time: now.as_secs_f64(),
            plugin: PLUGIN_ID,
        };

        if self.dry_run {
            info!(
                "[DRY RUN] Would POST to {}: {}",
                self.heartbeat_url,
                serde_json::to_string(&payload).unwrap_or_default()
            );
            return Ok(());
        }

        debug!(
            "Sending heartbeat to API: entity={} category={}",
            heartbeat.entity, heartbeat.category
        );

        let response = self
            .client
            .post(&self.heartbeat_url)
            .basic_auth(&self.api_key, None::<&str>)
            .json(&payload)
            .send()
            .await
            .context("Failed to send heartbeat request")?;

        let status = response.status();

        match status {
            s if s.is_success() => {
                trace!("Heartbeat accepted ({})", s);
                Ok(())
            }
            StatusCode::UNAUTHORIZED => {
                // Log auth errors clearly — don't rate-limit these since
                // they indicate a configuration problem, not transient failure
                error!(
                    "WakaTime API authentication failed (401). \
                     Check your API key in ~/.wakatime.cfg or $WAKATIME_API_KEY"
                );
                anyhow::bail!("WakaTime API authentication failed")
            }
            StatusCode::TOO_MANY_REQUESTS => {
                let retry_after = response
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("unknown");
                warn!(
                    "WakaTime API rate limited (429). Retry-After: {retry_after}"
                );
                anyhow::bail!("Rate limited by WakaTime API")
            }
            _ => {
                let body = response.text().await.unwrap_or_default();
                let count = self.error_log_count.fetch_add(1, Ordering::Relaxed);
                if count < 5 || count.is_multiple_of(ERROR_LOG_RATE_LIMIT) {
                    error!("WakaTime API error ({status}): {body}");
                    if count == 5 {
                        warn!(
                            "Rate-limiting error logs (showing every {}th error)",
                            ERROR_LOG_RATE_LIMIT
                        );
                    }
                }
                anyhow::bail!("WakaTime API returned {status}")
            }
        }
    }
}

impl HeartbeatSender for ApiSender {
    fn send_heartbeat<'a>(&'a self, heartbeat: &'a Heartbeat) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move { self.send_heartbeat(heartbeat).await })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_heartbeat_payload_serialization() {
        let payload = HeartbeatPayload {
            entity: "firefox",
            entity_type: "app",
            category: "browsing",
            time: 1_700_000_000.123,
            plugin: "wakatime-focusd/0.3.0",
        };

        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(json["entity"], "firefox");
        assert_eq!(json["type"], "app");
        assert_eq!(json["category"], "browsing");
        assert_eq!(json["plugin"], "wakatime-focusd/0.3.0");
        // time should be a float
        assert!(json["time"].is_f64());
    }

    #[test]
    fn test_resolve_api_url_from_config() {
        let config = Config {
            api_url: Some("https://custom.example.com/api".to_string()),
            ..Config::default()
        };
        assert_eq!(
            ApiSender::resolve_api_url(&config),
            "https://custom.example.com/api"
        );
    }

    #[test]
    fn test_resolve_api_url_default() {
        // Point to a nonexistent config file so we don't read ~/.wakatime.cfg
        let config = Config {
            wakatime_config_path: Some("/nonexistent/wakatime.cfg".into()),
            ..Config::default()
        };
        let url = ApiSender::resolve_api_url(&config);
        assert_eq!(url, DEFAULT_API_URL);
    }

    #[test]
    fn test_resolve_api_url_from_wakatime_cfg() {
        let dir = tempfile::tempdir().unwrap();
        let cfg_path = dir.path().join("wakatime.cfg");
        std::fs::write(
            &cfg_path,
            "[settings]\napi_key = test\napi_url = https://wakapi.example.com/api\n",
        )
        .unwrap();

        let config = Config {
            wakatime_config_path: Some(cfg_path),
            ..Config::default()
        };
        assert_eq!(
            ApiSender::resolve_api_url(&config),
            "https://wakapi.example.com/api"
        );
    }

    #[test]
    fn test_resolve_api_url_config_overrides_wakatime_cfg() {
        let dir = tempfile::tempdir().unwrap();
        let cfg_path = dir.path().join("wakatime.cfg");
        std::fs::write(
            &cfg_path,
            "[settings]\napi_key = test\napi_url = https://from-cfg.example.com/api\n",
        )
        .unwrap();

        let config = Config {
            api_url: Some("https://from-config.example.com/api".to_string()),
            wakatime_config_path: Some(cfg_path),
            ..Config::default()
        };
        // Daemon config takes priority over wakatime.cfg
        assert_eq!(
            ApiSender::resolve_api_url(&config),
            "https://from-config.example.com/api"
        );
    }

    #[test]
    fn test_heartbeat_url_trailing_slash_handled() {
        // Verify that trailing slashes in the base URL don't cause double slashes
        let base = "https://api.wakatime.com/api/";
        let base = base.trim_end_matches('/');
        let url = format!("{base}/v1/users/current/heartbeats");
        assert_eq!(
            url,
            "https://api.wakatime.com/api/v1/users/current/heartbeats"
        );
    }
}
