//! Direct `WakaTime` API heartbeat sender.
//!
//! Sends heartbeats via HTTP POST instead of spawning wakatime-cli.
//! Heartbeats are buffered and sent in batches via the bulk endpoint.

use std::sync::atomic::AtomicU32;
use std::sync::atomic::Ordering;
use std::sync::Mutex;
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

/// Flush the buffer when it reaches this many heartbeats.
const BATCH_THRESHOLD: usize = 10;

/// Maximum heartbeats per bulk API request (`WakaTime` API limit).
const MAX_BULK_SIZE: usize = 25;

/// JSON payload for a heartbeat (owned, for buffering).
#[derive(Debug, Clone, Serialize)]
struct HeartbeatPayload {
    entity: String,
    #[serde(rename = "type")]
    entity_type: &'static str,
    category: String,
    time: f64,
    plugin: &'static str,
}

impl HeartbeatPayload {
    fn from_heartbeat(heartbeat: &Heartbeat) -> Result<Self> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("System time before UNIX epoch")?;

        Ok(Self {
            entity: heartbeat.entity.as_str().to_string(),
            entity_type: "app",
            category: heartbeat.category.as_str().to_string(),
            time: now.as_secs_f64(),
            plugin: PLUGIN_ID,
        })
    }
}

/// Direct `WakaTime` API sender with heartbeat batching.
///
/// Heartbeats are buffered in memory and flushed to the bulk API endpoint
/// when the buffer reaches [`BATCH_THRESHOLD`] or when [`flush`](Self::flush)
/// is called explicitly (on periodic ticks and shutdown).
#[derive(Debug)]
pub struct ApiSender {
    /// HTTP client (reused for connection pooling).
    client: Client,

    /// Full single-heartbeat endpoint URL (used when buffer has 1 item).
    heartbeat_url: String,

    /// Full bulk heartbeat endpoint URL.
    bulk_url: String,

    /// API key for authentication.
    api_key: String,

    /// Dry run mode.
    dry_run: bool,

    /// Buffered heartbeats waiting to be flushed.
    buffer: Mutex<Vec<HeartbeatPayload>>,

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
        let bulk_url = format!("{base_url}/v1/users/current/heartbeats.bulk");

        let client = Client::builder()
            .user_agent(PLUGIN_ID)
            .build()
            .context("Failed to build HTTP client")?;

        info!("Using WakaTime API: {heartbeat_url}");

        Ok(Self {
            client,
            heartbeat_url,
            bulk_url,
            api_key,
            dry_run: config.dry_run,
            buffer: Mutex::new(Vec::new()),
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

    /// Buffer a heartbeat. Triggers a flush if the buffer reaches the threshold.
    async fn buffer_heartbeat(&self, heartbeat: &Heartbeat) -> Result<()> {
        let payload = HeartbeatPayload::from_heartbeat(heartbeat)?;

        if self.dry_run {
            info!(
                "[DRY RUN] Would send heartbeat: {}",
                serde_json::to_string(&payload).unwrap_or_default()
            );
            return Ok(());
        }

        debug!(
            "Buffering heartbeat: entity={} category={}",
            heartbeat.entity, heartbeat.category
        );

        let should_flush = {
            let mut buffer = self.buffer.lock().expect("buffer lock poisoned");
            buffer.push(payload);
            buffer.len() >= BATCH_THRESHOLD
        };

        if should_flush {
            debug!("Buffer reached threshold ({}), flushing", BATCH_THRESHOLD);
            if let Err(e) = self.flush_buffer().await {
                warn!("Failed to flush heartbeat buffer: {e}");
            }
        }

        Ok(())
    }

    /// Flush all buffered heartbeats to the API.
    async fn flush_buffer(&self) -> Result<()> {
        let payloads = {
            let mut buffer = self.buffer.lock().expect("buffer lock poisoned");
            std::mem::take(&mut *buffer)
        };

        if payloads.is_empty() {
            return Ok(());
        }

        debug!("Flushing {} buffered heartbeat(s)", payloads.len());

        if payloads.len() == 1 {
            return self.post_single(&payloads[0]).await;
        }

        // Chunk into groups of MAX_BULK_SIZE per API limit
        for chunk in payloads.chunks(MAX_BULK_SIZE) {
            self.post_bulk(chunk).await?;
        }

        Ok(())
    }

    /// POST a single heartbeat.
    async fn post_single(&self, payload: &HeartbeatPayload) -> Result<()> {
        let response = self
            .client
            .post(&self.heartbeat_url)
            .basic_auth(&self.api_key, None::<&str>)
            .json(payload)
            .send()
            .await
            .context("Failed to send heartbeat request")?;

        self.handle_response(response).await
    }

    /// POST a batch of heartbeats to the bulk endpoint.
    async fn post_bulk(&self, payloads: &[HeartbeatPayload]) -> Result<()> {
        debug!("Sending bulk request with {} heartbeat(s)", payloads.len());

        let response = self
            .client
            .post(&self.bulk_url)
            .basic_auth(&self.api_key, None::<&str>)
            .json(payloads)
            .send()
            .await
            .context("Failed to send bulk heartbeat request")?;

        self.handle_response(response).await
    }

    /// Handle an API response, mapping status codes to results.
    async fn handle_response(&self, response: reqwest::Response) -> Result<()> {
        let status = response.status();

        match status {
            s if s.is_success() => {
                trace!("Heartbeat(s) accepted ({})", s);
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
                warn!("WakaTime API rate limited (429). Retry-After: {retry_after}");
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
        Box::pin(async move { self.buffer_heartbeat(heartbeat).await })
    }

    fn flush(&self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move { self.flush_buffer().await })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_heartbeat_payload_serialization() {
        let payload = HeartbeatPayload {
            entity: "firefox".to_string(),
            entity_type: "app",
            category: "browsing".to_string(),
            time: 1_700_000_000.123,
            plugin: "wakatime-focusd/0.3.0",
        };

        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(json["entity"], "firefox");
        assert_eq!(json["type"], "app");
        assert_eq!(json["category"], "browsing");
        assert_eq!(json["plugin"], "wakatime-focusd/0.3.0");
        assert!(json["time"].is_f64());
    }

    #[test]
    fn test_bulk_payload_serialization() {
        let payloads = vec![
            HeartbeatPayload {
                entity: "firefox".to_string(),
                entity_type: "app",
                category: "browsing".to_string(),
                time: 1_700_000_000.0,
                plugin: PLUGIN_ID,
            },
            HeartbeatPayload {
                entity: "code".to_string(),
                entity_type: "app",
                category: "coding".to_string(),
                time: 1_700_000_001.0,
                plugin: PLUGIN_ID,
            },
        ];

        let json = serde_json::to_value(&payloads).unwrap();
        assert!(json.is_array());
        assert_eq!(json.as_array().unwrap().len(), 2);
        assert_eq!(json[0]["entity"], "firefox");
        assert_eq!(json[1]["entity"], "code");
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
        assert_eq!(
            ApiSender::resolve_api_url(&config),
            "https://from-config.example.com/api"
        );
    }

    #[test]
    fn test_heartbeat_url_trailing_slash_handled() {
        let base = "https://api.wakatime.com/api/";
        let base = base.trim_end_matches('/');
        let url = format!("{base}/v1/users/current/heartbeats");
        assert_eq!(
            url,
            "https://api.wakatime.com/api/v1/users/current/heartbeats"
        );
    }
}
