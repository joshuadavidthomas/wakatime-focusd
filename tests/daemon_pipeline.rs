//! Integration tests for the daemon event pipeline.
//!
//! Uses a `MockFocusSource` to replay scripted focus events and a
//! `RecordingSender` to capture heartbeats, verifying the full pipeline:
//! event → allowlist/denylist filter → heartbeat build → throttle dedup → idle gating → send.

use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use wakatime_focusd::EventLoopOutcome;
use wakatime_focusd::backend::FocusError;
use wakatime_focusd::backend::FocusEvent;
use wakatime_focusd::backend::FocusSource;
use wakatime_focusd::config::Config;
use wakatime_focusd::domain::Heartbeat;
use wakatime_focusd::idle::IdleMonitor;
use wakatime_focusd::run_event_loop;
use wakatime_focusd::wakatime::HeartbeatSender;

/// A `FocusSource` that replays a scripted sequence of events.
///
/// Events are consumed from a channel. When the channel is drained and closed,
/// `next_event` returns a `ConnectionFailed` error to signal the loop to exit.
struct MockFocusSource {
    rx: mpsc::Receiver<FocusEvent>,
}

impl MockFocusSource {
    /// Create a new mock source with the given events.
    fn from_events(events: Vec<FocusEvent>) -> Self {
        let (tx, rx) = mpsc::channel(events.len().max(1));
        for event in events {
            tx.try_send(event).expect("channel has capacity");
        }
        drop(tx); // Close sender so rx will return None after all events
        Self { rx }
    }

    /// Create a mock source with its sender, for feeding events dynamically.
    fn with_sender() -> (Self, mpsc::Sender<FocusEvent>) {
        let (tx, rx) = mpsc::channel(64);
        (Self { rx }, tx)
    }
}

#[async_trait]
impl FocusSource for MockFocusSource {
    async fn next_event(&mut self) -> Result<FocusEvent, FocusError> {
        self.rx
            .recv()
            .await
            .ok_or_else(|| FocusError::ConnectionFailed("mock source exhausted".into()))
    }
}

/// A `HeartbeatSender` that records all sent heartbeats for assertions.
struct RecordingSender {
    sent: Arc<Mutex<Vec<SentRecord>>>,
}

/// A record of a sent heartbeat.
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct SentRecord {
    entity: String,
    category: String,
    app_class: String,
    title: Option<String>,
}

impl RecordingSender {
    fn new() -> (Self, Arc<Mutex<Vec<SentRecord>>>) {
        let sent = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                sent: Arc::clone(&sent),
            },
            sent,
        )
    }
}

#[async_trait]
impl HeartbeatSender for RecordingSender {
    async fn send_heartbeat(&self, heartbeat: &Heartbeat) -> Result<()> {
        self.sent.lock().unwrap().push(SentRecord {
            entity: heartbeat.entity.as_str().to_string(),
            category: heartbeat.category.as_str().to_string(),
            app_class: heartbeat.source.app_class.clone(),
            title: heartbeat.source.title.clone(),
        });
        Ok(())
    }
}

fn event(class: &str, title: Option<&str>) -> FocusEvent {
    FocusEvent::new(class.to_string(), title.map(str::to_string), None)
}

/// Helper: run the event loop to completion with default config, returning sent heartbeats.
async fn run_pipeline(events: Vec<FocusEvent>, config: Config) -> Vec<SentRecord> {
    let source = MockFocusSource::from_events(events);
    let (sender, sent) = RecordingSender::new();
    let idle_monitor = IdleMonitor::new();
    let shutdown = CancellationToken::new();
    // Disable idle monitoring so it doesn't try to reach D-Bus
    idle_monitor.disable();

    let outcome =
        run_event_loop(Box::new(source), &config, &sender, &idle_monitor, &shutdown, false).await;

    // Should always end with SourceError when mock is exhausted
    assert!(
        matches!(outcome, EventLoopOutcome::SourceError(_)),
        "expected SourceError when mock source is exhausted"
    );

    sent.lock().unwrap().clone()
}

// Test: full pipeline - events flow through filter → heartbeat → throttle → send
#[tokio::test]
async fn test_full_pipeline_basic_events() {
    let events = vec![
        event("firefox", Some("GitHub")),
        event("code", Some("main.rs")),
        event("kitty", None),
    ];

    let sent = run_pipeline(events, Config::default()).await;

    assert_eq!(sent.len(), 3);
    assert_eq!(sent[0].entity, "firefox");
    assert_eq!(sent[1].entity, "code");
    assert_eq!(sent[2].entity, "kitty");
}

// Test: empty/no-focus events are skipped
#[tokio::test]
async fn test_empty_events_skipped() {
    let events = vec![
        event("firefox", None),
        event("", None), // empty class
        event("code", Some("main.rs")),
        event("", Some("ghost")), // empty class with title
    ];

    let sent = run_pipeline(events, Config::default()).await;

    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0].entity, "firefox");
    assert_eq!(sent[1].entity, "code");
}

// Test: allowlist filters events
#[tokio::test]
async fn test_allowlist_filters_events() {
    let config = Config {
        app_allowlist: Some(vec!["firefox".to_string(), "code".to_string()]),
        ..Config::default()
    };

    let events = vec![
        event("firefox", None),
        event("slack", None), // not in allowlist
        event("code", None),
        event("spotify", None), // not in allowlist
    ];

    let sent = run_pipeline(events, config).await;

    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0].entity, "firefox");
    assert_eq!(sent[1].entity, "code");
}

// Test: denylist filters events
#[tokio::test]
async fn test_denylist_filters_events() {
    let config = Config {
        app_denylist: Some(vec!["slack".to_string(), "spotify".to_string()]),
        ..Config::default()
    };

    let events = vec![
        event("firefox", None),
        event("slack", None), // denied
        event("code", None),
        event("spotify", None), // denied
    ];

    let sent = run_pipeline(events, config).await;

    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0].entity, "firefox");
    assert_eq!(sent[1].entity, "code");
}

// Test: denylist overrides allowlist
#[tokio::test]
async fn test_denylist_overrides_allowlist() {
    let config = Config {
        app_allowlist: Some(vec!["firefox".to_string(), "slack".to_string()]),
        app_denylist: Some(vec!["slack".to_string()]),
        ..Config::default()
    };

    let events = vec![
        event("firefox", None),
        event("slack", None), // in allowlist but also in denylist
    ];

    let sent = run_pipeline(events, config).await;

    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].entity, "firefox");
}

// Test: rapid focus switching (A → B → A within throttle window) - all three send
// because entity changes each time
#[tokio::test]
async fn test_rapid_focus_switching_different_entities() {
    let config = Config {
        min_entity_resend_seconds: 120, // high throttle
        ..Config::default()
    };

    let events = vec![
        event("firefox", None),
        event("code", None),
        event("firefox", None), // back to firefox - entity changed, should send
    ];

    let sent = run_pipeline(events, config).await;

    assert_eq!(sent.len(), 3);
    assert_eq!(sent[0].entity, "firefox");
    assert_eq!(sent[1].entity, "code");
    assert_eq!(sent[2].entity, "firefox");
}

// Test: same entity repeated within throttle window gets deduplicated
#[tokio::test]
async fn test_same_entity_throttled() {
    let config = Config {
        min_entity_resend_seconds: 120, // high throttle so it never expires in test
        ..Config::default()
    };

    let events = vec![
        event("firefox", None),
        event("firefox", None), // same entity, should be throttled
        event("firefox", None), // same entity, should be throttled
    ];

    let sent = run_pipeline(events, config).await;

    // Only the first one should be sent
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].entity, "firefox");
}

// Test: idle suppression - events arrive while session is idle
#[tokio::test]
async fn test_idle_suppression() {
    let source = MockFocusSource::from_events(vec![event("firefox", None), event("code", None)]);
    let (sender, sent) = RecordingSender::new();
    let idle_monitor = IdleMonitor::new();
    let shutdown = CancellationToken::new();
    // Mark as idle (don't start polling — just set the atomic directly)
    idle_monitor.set_idle(true);

    let outcome = run_event_loop(
        Box::new(source),
        &Config::default(),
        &sender,
        &idle_monitor,
        &shutdown,
        false,
    )
    .await;
    assert!(matches!(outcome, EventLoopOutcome::SourceError(_)));

    let sent = sent.lock().unwrap().clone();
    assert!(sent.is_empty(), "no heartbeats should be sent while idle");
}

// Test: idle transitions — events sent when not idle, suppressed when idle
#[tokio::test]
async fn test_idle_transitions() {
    tokio::time::pause();

    let (source, tx) = MockFocusSource::with_sender();
    let (sender, sent) = RecordingSender::new();
    let idle_monitor = Arc::new(IdleMonitor::new());
    let shutdown = CancellationToken::new();
    // Not idle initially, don't start D-Bus polling

    let idle_ref = Arc::clone(&idle_monitor);
    let config = Config {
        min_entity_resend_seconds: 0,     // no throttle
        heartbeat_interval_seconds: 3600, // disable periodic timer
        ..Config::default()
    };

    let handle = tokio::spawn(async move {
        run_event_loop(Box::new(source), &config, &sender, &idle_ref, &shutdown, false).await
    });

    // Send event while not idle — should be sent
    tx.send(event("firefox", None)).await.unwrap();
    tokio::time::advance(Duration::from_millis(50)).await;
    tokio::task::yield_now().await;

    // Go idle
    idle_monitor.set_idle(true);
    // Yield to ensure the idle state is visible
    tokio::task::yield_now().await;

    // Send event while idle — should be suppressed
    tx.send(event("code", None)).await.unwrap();
    tokio::time::advance(Duration::from_millis(50)).await;
    tokio::task::yield_now().await;

    // Go back to active
    idle_monitor.set_idle(false);
    tokio::task::yield_now().await;

    // Send event while active — should be sent
    tx.send(event("kitty", None)).await.unwrap();
    tokio::time::advance(Duration::from_millis(50)).await;
    tokio::task::yield_now().await;

    // Close source
    drop(tx);
    let _ = handle.await;

    let sent = sent.lock().unwrap().clone();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0].entity, "firefox");
    assert_eq!(sent[1].entity, "kitty");
}

// Test: periodic heartbeat timer fires for sustained focus on same app
#[tokio::test]
async fn test_periodic_heartbeat_timer() {
    tokio::time::pause(); // Enable manual time control

    let (source, tx) = MockFocusSource::with_sender();
    let (sender, sent_arc) = RecordingSender::new();
    let idle_monitor = IdleMonitor::new();
    let shutdown = CancellationToken::new();
    idle_monitor.disable(); // Don't use D-Bus

    let config = Config {
        min_entity_resend_seconds: 2,  // 2 second throttle
        heartbeat_interval_seconds: 3, // periodic tick every 3 seconds
        ..Config::default()
    };

    let handle = tokio::spawn({
        let sent_arc = Arc::clone(&sent_arc);
        async move {
            let outcome =
                run_event_loop(Box::new(source), &config, &sender, &idle_monitor, &shutdown, false)
                    .await;
            (outcome, sent_arc)
        }
    });

    // Send initial focus event
    tx.send(event("firefox", None)).await.unwrap();
    // Let the event be processed — need multiple yields for select! to poll and process
    for _ in 0..5 {
        tokio::time::advance(Duration::from_millis(10)).await;
        tokio::task::yield_now().await;
    }

    // Advance past the periodic timer (3s) — throttle window (2s) will have expired
    // Do this in steps to let the runtime process timer wakeups
    for _ in 0..10 {
        tokio::time::advance(Duration::from_millis(500)).await;
        tokio::task::yield_now().await;
    }

    // Close the source
    drop(tx);
    let (outcome, sent_arc) = handle.await.unwrap();
    assert!(matches!(outcome, EventLoopOutcome::SourceError(_)));

    let sent = sent_arc.lock().unwrap().clone();
    // Should have: initial heartbeat + at least one periodic heartbeat
    assert!(
        sent.len() >= 2,
        "expected at least 2 heartbeats (initial + periodic), got {}",
        sent.len()
    );
    // All should be for firefox
    for record in &sent {
        assert_eq!(record.entity, "firefox");
    }
}

// Test: periodic heartbeat is suppressed when idle
#[tokio::test]
async fn test_periodic_heartbeat_suppressed_when_idle() {
    tokio::time::pause();

    let (source, tx) = MockFocusSource::with_sender();
    let (sender, sent_arc) = RecordingSender::new();
    let idle_monitor = Arc::new(IdleMonitor::new());
    let shutdown = CancellationToken::new();
    // Not idle initially, but don't start polling

    let config = Config {
        min_entity_resend_seconds: 1,
        heartbeat_interval_seconds: 2,
        ..Config::default()
    };

    let idle_ref = Arc::clone(&idle_monitor);
    let handle = tokio::spawn({
        let sent_arc = Arc::clone(&sent_arc);
        async move {
            let outcome =
                run_event_loop(Box::new(source), &config, &sender, &idle_ref, &shutdown, false)
                    .await;
            (outcome, sent_arc)
        }
    });

    // Send initial event while active
    tx.send(event("firefox", None)).await.unwrap();
    tokio::time::advance(Duration::from_millis(100)).await;
    tokio::task::yield_now().await;

    // Go idle before periodic timer fires
    idle_monitor.set_idle(true);

    // Advance past periodic timer
    tokio::time::advance(Duration::from_secs(3)).await;
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(100)).await;
    tokio::task::yield_now().await;

    drop(tx);
    let (_, sent_arc) = handle.await.unwrap();

    let sent = sent_arc.lock().unwrap().clone();
    // Only the initial heartbeat should have been sent
    assert_eq!(
        sent.len(),
        1,
        "periodic heartbeat should be suppressed when idle"
    );
    assert_eq!(sent[0].entity, "firefox");
}

// Test: category rules are applied correctly
#[tokio::test]
async fn test_category_rules_applied() {
    use wakatime_focusd::config::CategoryRule;
    use wakatime_focusd::domain::Category;

    let config = Config {
        category_rules: vec![
            CategoryRule {
                pattern: "firefox|chromium".to_string(),
                category: Category::Browsing,
            },
            CategoryRule {
                pattern: "slack|discord".to_string(),
                category: Category::Communicating,
            },
        ],
        ..Config::default()
    };

    let events = vec![
        event("firefox", None),
        event("code", None),
        event("slack", None),
    ];

    let sent = run_pipeline(events, config).await;

    assert_eq!(sent.len(), 3);
    assert_eq!(sent[0].category, "browsing");
    assert_eq!(sent[1].category, "coding"); // default
    assert_eq!(sent[2].category, "communicating");
}

// Test: title tracking with append strategy
#[tokio::test]
async fn test_title_append_strategy() {
    use wakatime_focusd::config::TitleStrategy;

    let config = Config {
        track_titles: true,
        title_strategy: TitleStrategy::Append,
        ..Config::default()
    };

    let events = vec![
        event("firefox", Some("GitHub")),
        event("code", None),      // no title
        event("kitty", Some("")), // empty title
    ];

    let sent = run_pipeline(events, config).await;

    assert_eq!(sent.len(), 3);
    assert_eq!(sent[0].entity, "firefox — GitHub");
    assert_eq!(sent[1].entity, "code");
    assert_eq!(sent[2].entity, "kitty"); // empty title not appended
}

// Test: title tracking with ignore strategy
#[tokio::test]
async fn test_title_ignore_strategy() {
    use wakatime_focusd::config::TitleStrategy;

    let config = Config {
        track_titles: true,
        title_strategy: TitleStrategy::Ignore,
        ..Config::default()
    };

    let events = vec![event("firefox", Some("GitHub"))];

    let sent = run_pipeline(events, config).await;

    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].entity, "firefox"); // title ignored
}

// Test: source error causes SourceError outcome
#[tokio::test]
async fn test_source_error_returns_source_error_outcome() {
    // Empty source will immediately return an error
    let source = MockFocusSource::from_events(vec![]);
    let (sender, _sent) = RecordingSender::new();
    let idle_monitor = IdleMonitor::new();
    let shutdown = CancellationToken::new();
    idle_monitor.disable();

    let outcome = run_event_loop(
        Box::new(source),
        &Config::default(),
        &sender,
        &idle_monitor,
        &shutdown,
        false,
    )
    .await;

    assert!(matches!(outcome, EventLoopOutcome::SourceError(_)));
}
