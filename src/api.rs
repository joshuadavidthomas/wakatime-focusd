//! `WakaTime` API heartbeat sender.
//!
//! Sends heartbeats directly to the `WakaTime` API via HTTP POST.
//! Heartbeats are buffered and sent in batches via the bulk endpoint.
//! Failed sends are persisted to an offline queue and replayed later.

use std::io::Write;
use std::path::PathBuf;
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
use serde::Deserialize;
use serde::Serialize;
use tracing::debug;
use tracing::error;
use tracing::info;
use tracing::trace;
use tracing::warn;

use crate::api_key;
use crate::config::Config;
use crate::domain::Heartbeat;

/// Trait for sending heartbeats to `WakaTime`.
pub trait HeartbeatSender: Send {
    /// Send a heartbeat. Returns `Ok(())` on success.
    ///
    /// Implementations may buffer the heartbeat for later delivery (see
    /// [`flush`](Self::flush)).
    fn send_heartbeat<'a>(&'a self, heartbeat: &'a Heartbeat) -> BoxFuture<'a, Result<()>>;

    /// Flush any buffered heartbeats.
    ///
    /// Called by the event loop on periodic ticks and before shutdown/reload.
    /// The default implementation is a no-op (for senders that deliver
    /// immediately, e.g., test mocks).
    fn flush(&self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async { Ok(()) })
    }
}

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

/// Maximum offline queue file size in bytes (10 MB).
const QUEUE_MAX_SIZE: u64 = 10 * 1024 * 1024;

/// Maximum number of queued batches to drain per flush cycle.
const QUEUE_DRAIN_LIMIT: usize = 10;

/// Offline queue file name.
const QUEUE_FILE: &str = "queue.jsonl";

/// JSON payload for a heartbeat (fully owned for buffering and offline queue).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct HeartbeatPayload {
    entity: String,
    #[serde(rename = "type")]
    entity_type: String,
    category: String,
    time: f64,
    plugin: String,
}

impl HeartbeatPayload {
    fn from_heartbeat(heartbeat: &Heartbeat) -> Result<Self> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("System time before UNIX epoch")?;

        Ok(Self {
            entity: heartbeat.entity.as_str().to_string(),
            entity_type: "app".to_string(),
            category: heartbeat.category.as_str().to_string(),
            time: now.as_secs_f64(),
            plugin: PLUGIN_ID.to_string(),
        })
    }
}

/// Direct `WakaTime` API sender with heartbeat batching and offline queue.
///
/// Heartbeats are buffered in memory and flushed to the bulk API endpoint
/// when the buffer reaches [`BATCH_THRESHOLD`] or when [`flush`](Self::flush)
/// is called explicitly (on periodic ticks and shutdown).
///
/// If a flush fails (network down, server error), the batch is persisted to
/// an offline queue file and replayed on the next successful flush.
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

    /// Path to the offline queue file.
    queue_path: Option<PathBuf>,

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

        let queue_path = dirs::data_dir().map(|d| d.join("wakatime-focusd").join(QUEUE_FILE));

        info!("Using WakaTime API: {heartbeat_url}");
        if let Some(ref qp) = queue_path {
            debug!("Offline queue path: {}", qp.display());
        }

        Ok(Self {
            client,
            heartbeat_url,
            bulk_url,
            api_key,
            dry_run: config.dry_run,
            buffer: Mutex::new(Vec::new()),
            queue_path,
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
    ///
    /// On success, also drains any queued offline heartbeats.
    /// On failure, persists the batch to the offline queue.
    async fn flush_buffer(&self) -> Result<()> {
        let payloads = {
            let mut buffer = self.buffer.lock().expect("buffer lock poisoned");
            std::mem::take(&mut *buffer)
        };

        if payloads.is_empty() {
            // No new heartbeats, but still try to drain offline queue
            self.drain_queue().await;
            return Ok(());
        }

        debug!("Flushing {} buffered heartbeat(s)", payloads.len());

        match self.send_payloads(&payloads).await {
            Ok(()) => {
                self.drain_queue().await;
                Ok(())
            }
            Err(e) => {
                self.persist_to_queue(&payloads);
                Err(e)
            }
        }
    }

    /// Send a slice of payloads, using single or bulk endpoint as appropriate.
    async fn send_payloads(&self, payloads: &[HeartbeatPayload]) -> Result<()> {
        if payloads.len() == 1 {
            return self.post_single(&payloads[0]).await;
        }

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

    /// Persist a failed batch to the offline queue file.
    ///
    /// Appends the batch as a single JSON line. Skips if the queue file
    /// exceeds [`QUEUE_MAX_SIZE`] to prevent unbounded disk growth.
    fn persist_to_queue(&self, payloads: &[HeartbeatPayload]) {
        let Some(ref queue_path) = self.queue_path else {
            warn!("No offline queue path available, dropping {} heartbeat(s)", payloads.len());
            return;
        };

        // Check file size before appending
        if let Ok(metadata) = std::fs::metadata(queue_path)
            && metadata.len() >= QUEUE_MAX_SIZE
        {
            warn!(
                "Offline queue is full ({} bytes), dropping {} heartbeat(s)",
                metadata.len(),
                payloads.len()
            );
            return;
        }

        // Ensure parent directory exists
        if let Some(parent) = queue_path.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            error!("Failed to create offline queue directory: {e}");
            return;
        }

        let line = match serde_json::to_string(payloads) {
            Ok(l) => l,
            Err(e) => {
                error!("Failed to serialize heartbeats for offline queue: {e}");
                return;
            }
        };

        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(queue_path)
        {
            Ok(mut file) => {
                if let Err(e) = writeln!(file, "{line}") {
                    error!("Failed to write to offline queue: {e}");
                } else {
                    info!(
                        "Queued {} heartbeat(s) to offline queue",
                        payloads.len()
                    );
                }
            }
            Err(e) => error!("Failed to open offline queue: {e}"),
        }
    }

    /// Drain queued batches from the offline queue file.
    ///
    /// Sends up to [`QUEUE_DRAIN_LIMIT`] batches, oldest first. On failure,
    /// stops and rewrites the queue file with the remaining batches. On full
    /// drain, removes the queue file.
    async fn drain_queue(&self) {
        let Some(ref queue_path) = self.queue_path else {
            return;
        };

        let content = match std::fs::read_to_string(queue_path) {
            Ok(c) if !c.is_empty() => c,
            _ => return,
        };

        let lines: Vec<String> = content.lines().map(String::from).collect();
        if lines.is_empty() {
            return;
        }

        info!("Draining offline queue ({} batch(es))", lines.len());

        let mut drained = 0;

        for line in &lines {
            if drained >= QUEUE_DRAIN_LIMIT {
                debug!(
                    "Reached drain limit ({}), deferring remaining batches",
                    QUEUE_DRAIN_LIMIT
                );
                break;
            }

            let batch: Vec<HeartbeatPayload> = match serde_json::from_str(line) {
                Ok(b) => b,
                Err(e) => {
                    warn!("Skipping corrupt queue entry: {e}");
                    drained += 1;
                    continue;
                }
            };

            if let Err(e) = self.send_payloads(&batch).await {
                warn!("Failed to drain queued batch: {e}. Will retry later.");
                break;
            }

            debug!("Drained queued batch ({} heartbeat(s))", batch.len());
            drained += 1;
        }

        if drained == 0 {
            return;
        }

        if drained >= lines.len() {
            // All batches drained, remove queue file
            if let Err(e) = std::fs::remove_file(queue_path) {
                warn!("Failed to remove empty queue file: {e}");
            } else {
                info!("Offline queue fully drained");
            }
        } else {
            // Rewrite with remaining batches
            let remaining = lines[drained..].iter().fold(
                String::new(),
                |mut acc, l| {
                    acc.push_str(l);
                    acc.push('\n');
                    acc
                },
            );
            if let Err(e) = std::fs::write(queue_path, remaining) {
                error!("Failed to rewrite offline queue: {e}");
            } else {
                debug!(
                    "Offline queue trimmed: {} batch(es) remaining",
                    lines.len() - drained
                );
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
            entity_type: "app".to_string(),
            category: "browsing".to_string(),
            time: 1_700_000_000.123,
            plugin: "wakatime-focusd/0.3.0".to_string(),
        };

        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(json["entity"], "firefox");
        assert_eq!(json["type"], "app");
        assert_eq!(json["category"], "browsing");
        assert_eq!(json["plugin"], "wakatime-focusd/0.3.0");
        assert!(json["time"].is_f64());
    }

    #[test]
    fn test_heartbeat_payload_roundtrip() {
        let payload = HeartbeatPayload {
            entity: "firefox".to_string(),
            entity_type: "app".to_string(),
            category: "browsing".to_string(),
            time: 1_700_000_000.123,
            plugin: PLUGIN_ID.to_string(),
        };

        let json = serde_json::to_string(&payload).unwrap();
        let deserialized: HeartbeatPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.entity, "firefox");
        assert_eq!(deserialized.entity_type, "app");
        assert_eq!(deserialized.category, "browsing");
    }

    #[test]
    fn test_bulk_payload_serialization() {
        let payloads = vec![
            HeartbeatPayload {
                entity: "firefox".to_string(),
                entity_type: "app".to_string(),
                category: "browsing".to_string(),
                time: 1_700_000_000.0,
                plugin: PLUGIN_ID.to_string(),
            },
            HeartbeatPayload {
                entity: "code".to_string(),
                entity_type: "app".to_string(),
                category: "coding".to_string(),
                time: 1_700_000_001.0,
                plugin: PLUGIN_ID.to_string(),
            },
        ];

        let json = serde_json::to_value(&payloads).unwrap();
        assert!(json.is_array());
        assert_eq!(json.as_array().unwrap().len(), 2);
        assert_eq!(json[0]["entity"], "firefox");
        assert_eq!(json[1]["entity"], "code");
    }

    #[test]
    fn test_bulk_payload_roundtrip() {
        let payloads = vec![
            HeartbeatPayload {
                entity: "firefox".to_string(),
                entity_type: "app".to_string(),
                category: "browsing".to_string(),
                time: 1_700_000_000.0,
                plugin: PLUGIN_ID.to_string(),
            },
            HeartbeatPayload {
                entity: "code".to_string(),
                entity_type: "app".to_string(),
                category: "coding".to_string(),
                time: 1_700_000_001.0,
                plugin: PLUGIN_ID.to_string(),
            },
        ];

        let json_line = serde_json::to_string(&payloads).unwrap();
        let deserialized: Vec<HeartbeatPayload> = serde_json::from_str(&json_line).unwrap();
        assert_eq!(deserialized.len(), 2);
        assert_eq!(deserialized[0].entity, "firefox");
        assert_eq!(deserialized[1].entity, "code");
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

    #[test]
    fn test_persist_to_queue_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let queue_path = dir.path().join("queue.jsonl");

        let sender = ApiSender {
            client: Client::new(),
            heartbeat_url: String::new(),
            bulk_url: String::new(),
            api_key: String::new(),
            dry_run: false,
            buffer: Mutex::new(Vec::new()),
            queue_path: Some(queue_path.clone()),
            error_log_count: AtomicU32::new(0),
        };

        let payloads = vec![
            HeartbeatPayload {
                entity: "firefox".to_string(),
                entity_type: "app".to_string(),
                category: "browsing".to_string(),
                time: 1_700_000_000.0,
                plugin: PLUGIN_ID.to_string(),
            },
            HeartbeatPayload {
                entity: "code".to_string(),
                entity_type: "app".to_string(),
                category: "coding".to_string(),
                time: 1_700_000_001.0,
                plugin: PLUGIN_ID.to_string(),
            },
        ];

        // Persist
        sender.persist_to_queue(&payloads);
        assert!(queue_path.exists());

        // Read back
        let content = std::fs::read_to_string(&queue_path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 1);

        let batch: Vec<HeartbeatPayload> = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(batch.len(), 2);
        assert_eq!(batch[0].entity, "firefox");
        assert_eq!(batch[1].entity, "code");
    }

    #[test]
    fn test_persist_multiple_batches() {
        let dir = tempfile::tempdir().unwrap();
        let queue_path = dir.path().join("queue.jsonl");

        let sender = ApiSender {
            client: Client::new(),
            heartbeat_url: String::new(),
            bulk_url: String::new(),
            api_key: String::new(),
            dry_run: false,
            buffer: Mutex::new(Vec::new()),
            queue_path: Some(queue_path.clone()),
            error_log_count: AtomicU32::new(0),
        };

        // Persist two separate batches
        sender.persist_to_queue(&[HeartbeatPayload {
            entity: "batch1".to_string(),
            entity_type: "app".to_string(),
            category: "coding".to_string(),
            time: 1.0,
            plugin: PLUGIN_ID.to_string(),
        }]);
        sender.persist_to_queue(&[HeartbeatPayload {
            entity: "batch2".to_string(),
            entity_type: "app".to_string(),
            category: "coding".to_string(),
            time: 2.0,
            plugin: PLUGIN_ID.to_string(),
        }]);

        let content = std::fs::read_to_string(&queue_path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2);

        let batch1: Vec<HeartbeatPayload> = serde_json::from_str(lines[0]).unwrap();
        let batch2: Vec<HeartbeatPayload> = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(batch1[0].entity, "batch1");
        assert_eq!(batch2[0].entity, "batch2");
    }

    #[test]
    fn test_persist_respects_max_size() {
        let dir = tempfile::tempdir().unwrap();
        let queue_path = dir.path().join("queue.jsonl");

        // Pre-fill the queue file to just under the limit
        #[allow(clippy::cast_possible_truncation)]
        let big_content = "x".repeat(QUEUE_MAX_SIZE as usize);
        std::fs::write(&queue_path, &big_content).unwrap();

        let sender = ApiSender {
            client: Client::new(),
            heartbeat_url: String::new(),
            bulk_url: String::new(),
            api_key: String::new(),
            dry_run: false,
            buffer: Mutex::new(Vec::new()),
            queue_path: Some(queue_path.clone()),
            error_log_count: AtomicU32::new(0),
        };

        // This should be dropped because the file is already at max size
        sender.persist_to_queue(&[HeartbeatPayload {
            entity: "should-be-dropped".to_string(),
            entity_type: "app".to_string(),
            category: "coding".to_string(),
            time: 1.0,
            plugin: PLUGIN_ID.to_string(),
        }]);

        // File should still just contain the original content
        let content = std::fs::read_to_string(&queue_path).unwrap();
        assert_eq!(content, big_content);
    }

    #[test]
    fn test_persist_no_queue_path() {
        let sender = ApiSender {
            client: Client::new(),
            heartbeat_url: String::new(),
            bulk_url: String::new(),
            api_key: String::new(),
            dry_run: false,
            buffer: Mutex::new(Vec::new()),
            queue_path: None,
            error_log_count: AtomicU32::new(0),
        };

        // Should not panic
        sender.persist_to_queue(&[HeartbeatPayload {
            entity: "test".to_string(),
            entity_type: "app".to_string(),
            category: "coding".to_string(),
            time: 1.0,
            plugin: PLUGIN_ID.to_string(),
        }]);
    }
}
