//! wakatime-focusd - Systemd user daemon for `WakaTime` app heartbeats.
//!
//! Tracks currently focused desktop application and sends heartbeats to `WakaTime`
//! using wakatime-cli.

pub mod backend;
pub mod config;
pub mod domain;
pub mod heartbeat;
pub mod idle;
pub mod throttle;
pub mod wakatime;

use std::time::Duration;

use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tracing::debug;
use tracing::info;
use tracing::warn;

use crate::backend::FocusSource;
use crate::config::Config;
use crate::heartbeat::HeartbeatBuilder;
use crate::idle::IdleMonitor;
use crate::throttle::HeartbeatThrottle;
use crate::throttle::ThrottleDecision;

/// Outcome of an event loop iteration (used for reconnection logic).
pub enum EventLoopOutcome {
    /// The source returned an error; the caller should reconnect.
    SourceError(crate::backend::FocusError),
    /// The loop completed normally (e.g., source was exhausted).
    Finished,
    /// Shutdown was requested via the cancellation token.
    Shutdown,
    /// A configuration reload was requested (e.g., via `SIGHUP`).
    Reload,
}

/// Core event loop, decoupled from backend connection for testability.
///
/// Accepts injected dependencies so integration tests can provide mock
/// implementations without needing a real desktop environment, wakatime-cli
/// binary, or D-Bus session.
pub async fn run_event_loop(
    mut source: Box<dyn FocusSource>,
    config: &Config,
    sender: &(dyn wakatime::HeartbeatSender + Sync),
    idle_monitor: &IdleMonitor,
    shutdown: &CancellationToken,
    reload: &Notify,
    print_events: bool,
) -> EventLoopOutcome {
    let mut throttle = HeartbeatThrottle::new(config.min_entity_resend_seconds);
    let heartbeat_builder = HeartbeatBuilder::from_config(config);
    let mut periodic_timer =
        tokio::time::interval(Duration::from_secs(config.heartbeat_interval_seconds));
    periodic_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            () = shutdown.cancelled() => {
                info!("Shutdown signal received, exiting event loop");
                return EventLoopOutcome::Shutdown;
            }

            () = reload.notified() => {
                info!("Reload signal received, exiting event loop for config reload");
                return EventLoopOutcome::Reload;
            }

            event = source.next_event() => {
                match event {
                    Ok(focus_event) => {
                        handle_focus_event(
                            &focus_event,
                            &heartbeat_builder,
                            idle_monitor,
                            &mut throttle,
                            sender,
                            print_events,
                        ).await;
                    }
                    Err(e) => {
                        return EventLoopOutcome::SourceError(e);
                    }
                }
            }

            _ = periodic_timer.tick() => {
                if let Some(last_heartbeat) = throttle.last_heartbeat()
                    && throttle.should_send(&last_heartbeat.entity) == ThrottleDecision::Send
                {
                    if idle_monitor.is_idle() {
                        debug!("Skipping periodic heartbeat: session is idle");
                        continue;
                    }

                    // Re-send the same heartbeat rather than rebuilding from
                    // the source event — entity and category haven't changed.
                    let periodic_heartbeat = last_heartbeat.clone();
                    debug!(
                        "Sending periodic heartbeat for: {}",
                        periodic_heartbeat.entity
                    );
                    match sender.send_heartbeat(&periodic_heartbeat).await {
                        Ok(()) => throttle.record_sent(periodic_heartbeat),
                        Err(e) => warn!("Failed to send periodic heartbeat: {}", e),
                    }
                }
            }
        }
    }
}

/// Handle a focus event through the full pipeline.
async fn handle_focus_event(
    event: &backend::FocusEvent,
    heartbeat_builder: &HeartbeatBuilder,
    idle_monitor: &IdleMonitor,
    throttle: &mut HeartbeatThrottle,
    sender: &(dyn wakatime::HeartbeatSender + Sync),
    print_events: bool,
) {
    if print_events {
        println!(
            "[FOCUS] | class={} title={:?} window_id={:?}",
            event.app_class, event.title, event.window_id
        );
    }

    // Skip empty focus (no focused window)
    if event.is_empty() {
        debug!("Ignoring empty focus event");
        return;
    }

    // Check allowlist/denylist
    if !heartbeat_builder.is_app_allowed(&event.app_class) {
        debug!("App '{}' not allowed by filter", event.app_class);
        return;
    }

    // Build heartbeat
    let heartbeat = heartbeat_builder.build(event.clone());

    // Check idle state
    if idle_monitor.is_idle() {
        debug!("Skipping heartbeat: session is idle");
        return;
    }

    // Check throttle
    match throttle.should_send(&heartbeat.entity) {
        ThrottleDecision::Send => {
            debug!("Sending heartbeat for: {}", heartbeat.entity);
            if let Err(e) = sender.send_heartbeat(&heartbeat).await {
                warn!("Failed to send heartbeat: {}", e);
            } else {
                throttle.record_sent(heartbeat);
            }
        }
        ThrottleDecision::Skip => {
            debug!("Throttled heartbeat for: {}", heartbeat.entity);
        }
    }
}
