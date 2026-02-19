//! wakatime-focusd - Systemd user daemon for WakaTime app heartbeats.
//!
//! Tracks currently focused desktop application and sends heartbeats to WakaTime
//! using wakatime-cli.

mod backend;
mod config;
mod domain;
mod heartbeat;
mod idle;
mod throttle;
mod wakatime;

use crate::backend::{FocusEvent, FocusSource, HyprlandSource};
use crate::config::Config;
use crate::heartbeat::HeartbeatBuilder;
use crate::idle::IdleMonitor;
use crate::throttle::{HeartbeatThrottle, ThrottleDecision};
use crate::wakatime::WakaTimeClient;

use anyhow::{Context, Result};
use clap::Parser;
use std::env;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, error, info, warn};
use tracing_subscriber::EnvFilter;

/// WakaTime focus daemon for Hyprland.
///
/// Tracks currently focused desktop application and sends heartbeats to WakaTime.
#[derive(Parser, Debug)]
#[command(name = "wakatime-focusd")]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Path to config file.
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Enable dry-run mode (don't actually send heartbeats).
    #[arg(long)]
    dry_run: bool,

    /// Log level (trace, debug, info, warn, error).
    #[arg(long, default_value = "info")]
    log_level: String,

    /// Print normalized focus events to stdout.
    #[arg(long)]
    print_events: bool,

    /// Run in oneshot mode: connect, print a few events, then exit.
    #[arg(long)]
    oneshot: bool,

    /// Number of events to capture in oneshot mode.
    #[arg(long, default_value = "5")]
    oneshot_count: usize,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Initialize logging
    init_logging(&args.log_level)?;

    info!("wakatime-focusd v{} starting", env!("CARGO_PKG_VERSION"));

    // Check environment
    let hyprland_available = env::var("XDG_RUNTIME_DIR").is_ok();
    if !hyprland_available {
        error!("Hyprland environment not detected.");
        error!("Required environment variables:");
        for diag in HyprlandSource::get_diagnostics() {
            error!("  {}", diag);
        }
        error!("");
        error!("If running as a systemd user service, ensure these variables are available.");
        error!("See: dbus-update-activation-environment --systemd XDG_RUNTIME_DIR");
        anyhow::bail!("Hyprland environment not available");
    }

    // Show diagnostics
    for diag in HyprlandSource::get_diagnostics() {
        debug!("{}", diag);
    }

    // Load config
    let mut config =
        Config::load_or_default(args.config.as_deref()).context("Failed to load configuration")?;

    if args.dry_run {
        config.dry_run = true;
    }

    info!("Configuration loaded (dry_run={})", config.dry_run);

    // Oneshot mode
    if args.oneshot {
        return run_oneshot(args.oneshot_count, args.print_events).await;
    }

    // Normal daemon mode
    run_daemon(config, args.print_events).await
}

/// Initialize logging with the specified level.
fn init_logging(level: &str) -> Result<()> {
    let filter = EnvFilter::try_new(format!("wakatime_focusd={}", level))
        .or_else(|_| EnvFilter::try_new("info"))
        .context("Invalid log level")?;

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_thread_ids(false)
        .init();

    Ok(())
}

/// Run in oneshot mode: capture a few events and exit.
async fn run_oneshot(count: usize, print_events: bool) -> Result<()> {
    info!("Running in oneshot mode, capturing {} events", count);

    let mut source = HyprlandSource::connect().await?;

    // Capture events
    let mut captured = 0;
    while captured < count {
        match tokio::time::timeout(Duration::from_secs(30), source.next_event()).await {
            Ok(Ok(event)) => {
                captured += 1;
                if print_events {
                    println!(
                        "[{}] | class={} title={:?} window_id={:?}",
                        captured, event.app_class, event.title, event.window_id
                    );
                } else {
                    info!(
                        "Event {}/{}: class={}, title={:?}",
                        captured, count, event.app_class, event.title
                    );
                }
            }
            Ok(Err(e)) => {
                error!("Focus event error: {}", e);
                break;
            }
            Err(_) => {
                warn!("Timeout waiting for focus events");
                break;
            }
        }
    }

    info!("Oneshot mode complete, captured {} events", captured);
    Ok(())
}

/// Run daemon event loop.
async fn run_daemon(config: Config, print_events: bool) -> Result<()> {
    // Initialize components
    let wakatime_client =
        WakaTimeClient::from_config(&config).context("Failed to initialize WakaTime client")?;

    let idle_monitor = Arc::new(IdleMonitor::new());
    idle_monitor
        .clone()
        .start_polling(Duration::from_secs(config.idle_check_interval_seconds));

    let mut throttle = HeartbeatThrottle::new(config.min_entity_resend_seconds);
    let heartbeat_builder = HeartbeatBuilder::from_config(&config)?;
    let mut periodic_timer =
        tokio::time::interval(Duration::from_secs(config.heartbeat_interval_seconds));
    periodic_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    info!("Daemon started, waiting for focus events...");

    loop {
        let mut source = HyprlandSource::connect().await?;

        loop {
            tokio::select! {
                // Handle focus events
                event = source.next_event() => {
                    match event {
                        Ok(focus_event) => {
                            handle_focus_event(
                                &focus_event,
                                &heartbeat_builder,
                                &idle_monitor,
                                &mut throttle,
                                &wakatime_client,
                                print_events,
                            ).await;
                        }
                        Err(e) => {
                            error!("Focus event error: {}", e);
                            break; // Reconnect
                        }
                    }
                }

                // Periodic heartbeat check (in case no focus changes but time elapsed)
                _ = periodic_timer.tick() => {
                    if let Some(last_heartbeat) = throttle.last_heartbeat()
                        && throttle.should_send(&last_heartbeat.entity) == ThrottleDecision::Send
                    {
                        if idle_monitor.is_idle() {
                            debug!("Skipping periodic heartbeat: session is idle");
                            continue;
                        }

                        let periodic_heartbeat =
                            heartbeat_builder.build(last_heartbeat.source.clone());
                        debug!(
                            "Sending periodic heartbeat for: {}",
                            periodic_heartbeat.entity.as_str()
                        );
                        match wakatime_client.send_heartbeat(&periodic_heartbeat).await {
                            Ok(()) => throttle.record_sent(periodic_heartbeat),
                            Err(e) => warn!("Failed to send periodic heartbeat: {}", e),
                        }
                    }
                }
            }
        }
    }
}

/// Handle a focus event.
async fn handle_focus_event(
    event: &FocusEvent,
    heartbeat_builder: &HeartbeatBuilder,
    idle_monitor: &IdleMonitor,
    throttle: &mut HeartbeatThrottle,
    wakatime_client: &WakaTimeClient,
    print_events: bool,
) {
    // Print event if requested
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
            debug!("Sending heartbeat for: {}", heartbeat.entity.as_str());
            if let Err(e) = wakatime_client.send_heartbeat(&heartbeat).await {
                warn!("Failed to send heartbeat: {}", e);
            } else {
                throttle.record_sent(heartbeat);
            }
        }
        ThrottleDecision::Skip => {
            debug!("Throttled heartbeat for: {}", heartbeat.entity.as_str());
        }
    }
}
