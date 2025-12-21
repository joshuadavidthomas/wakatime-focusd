//! wakatime-focusd - Hyprland-first systemd user daemon for WakaTime app heartbeats.
//!
//! Tracks the currently focused desktop application and sends heartbeats to WakaTime
//! using wakatime-cli.

mod config;
mod focus;
mod idle;
mod throttle;
mod wakatime;

use crate::config::Config;
use crate::focus::hyprland_ipc;
use crate::focus::FocusEvent;
use crate::idle::IdleMonitor;
use crate::throttle::{HeartbeatThrottle, ThrottleDecision};
use crate::wakatime::WakaTimeClient;

use anyhow::{Context, Result};
use clap::Parser;
use std::env;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};
use tracing_subscriber::EnvFilter;

/// WakaTime focus daemon for Hyprland.
///
/// Tracks the currently focused desktop application and sends heartbeats to WakaTime.
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

    info!(
        "wakatime-focusd v{} starting",
        env!("CARGO_PKG_VERSION")
    );

    // Check environment
    let hyprland_available = env::var("HYPRLAND_INSTANCE_SIGNATURE").is_ok()
        && env::var("XDG_RUNTIME_DIR").is_ok();
    if !hyprland_available {
        error!("Hyprland environment not detected.");
        error!("Required environment variables:");
        for diag in hyprland_ipc::get_diagnostics() {
            error!("  {}", diag);
        }
        error!("");
        error!("If running as a systemd user service, ensure these variables are available.");
        error!("See: dbus-update-activation-environment --systemd HYPRLAND_INSTANCE_SIGNATURE XDG_RUNTIME_DIR");
        anyhow::bail!("Hyprland environment not available");
    }

    // Show diagnostics
    for diag in hyprland_ipc::get_diagnostics() {
        debug!("{}", diag);
    }

    // Load config
    let mut config = Config::load_or_default(args.config.as_deref())
        .context("Failed to load configuration")?;

    // Override dry_run if specified on command line
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

    let (tx, mut rx) = mpsc::channel::<FocusEvent>(32);

    // Spawn focus stream
    tokio::spawn(async move {
        if let Err(e) = hyprland_ipc::run_focus_stream(tx).await {
            error!("Focus stream error: {}", e);
        }
    });

    // Capture events
    let mut captured = 0;
    while captured < count {
        match tokio::time::timeout(Duration::from_secs(30), rx.recv()).await {
            Ok(Some(event)) => {
                captured += 1;
                if print_events {
                    println!(
                        "[{}] {} | class={} title={:?} window_id={:?}",
                        captured,
                        event.backend.as_str(),
                        event.app_class,
                        event.title,
                        event.window_id
                    );
                } else {
                    info!(
                        "Event {}/{}: class={}, title={:?}",
                        captured, count, event.app_class, event.title
                    );
                }
            }
            Ok(None) => {
                warn!("Focus stream ended unexpectedly");
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

/// Run the daemon event loop.
async fn run_daemon(config: Config, print_events: bool) -> Result<()> {
    // Initialize WakaTime client
    let wakatime_client = WakaTimeClient::from_config(&config)
        .context("Failed to initialize WakaTime client")?;

    // Initialize idle monitor
    let idle_monitor = Arc::new(IdleMonitor::new());
    idle_monitor.clone().start_polling(Duration::from_secs(config.idle_check_interval_seconds));

    // Initialize throttle
    let mut throttle = HeartbeatThrottle::new(config.min_entity_resend_seconds);

    // Create focus event channel
    let (tx, mut rx) = mpsc::channel::<FocusEvent>(32);

    // Spawn focus stream with reconnection handling
    tokio::spawn(async move {
        loop {
            if let Err(e) = hyprland_ipc::run_focus_stream(tx.clone()).await {
                error!("Focus stream error: {}", e);
                // run_focus_stream handles reconnection internally
            }
        }
    });

    info!("Daemon started, waiting for focus events...");

    // Main event loop
    loop {
        tokio::select! {
            // Handle focus events
            event = rx.recv() => {
                match event {
                    Some(focus_event) => {
                        handle_focus_event(
                            &focus_event,
                            &config,
                            &idle_monitor,
                            &mut throttle,
                            &wakatime_client,
                            print_events,
                        ).await;
                    }
                    None => {
                        error!("Focus event channel closed");
                        break;
                    }
                }
            }

            // Periodic heartbeat check (in case no focus changes but time elapsed)
            _ = tokio::time::sleep(Duration::from_secs(config.heartbeat_interval_seconds)) => {
                // Check if we should send a periodic heartbeat for the current entity
                if let Some(entity) = throttle.last_entity() {
                    if throttle.should_send(entity) == ThrottleDecision::Send {
                        // Check idle state
                        if idle_monitor.is_idle() {
                            debug!("Skipping periodic heartbeat: session is idle");
                            continue;
                        }

                        let entity = entity.to_string();
                        debug!("Sending periodic heartbeat for: {}", entity);
                        throttle.record_sent(&entity);

                        if let Err(e) = wakatime_client.send_heartbeat(&entity).await {
                            warn!("Failed to send periodic heartbeat: {}", e);
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

/// Handle a focus event.
async fn handle_focus_event(
    event: &FocusEvent,
    config: &Config,
    idle_monitor: &IdleMonitor,
    throttle: &mut HeartbeatThrottle,
    wakatime_client: &WakaTimeClient,
    print_events: bool,
) {
    // Print event if requested
    if print_events {
        println!(
            "[FOCUS] {} | class={} title={:?} window_id={:?}",
            event.backend.as_str(), event.app_class, event.title, event.window_id
        );
    }

    // Skip empty focus (no focused window)
    if event.is_empty() {
        debug!("Ignoring empty focus event");
        return;
    }

    // Check allowlist/denylist
    if !config.is_app_allowed(&event.app_class) {
        debug!("App '{}' not allowed by filter", event.app_class);
        return;
    }

    // Build entity string
    let entity = config.build_entity(&event.app_class, event.title.as_deref());

    // Check idle state
    if idle_monitor.is_idle() {
        debug!("Skipping heartbeat: session is idle");
        return;
    }

    // Check throttle
    match throttle.should_send(&entity) {
        ThrottleDecision::Send => {
            debug!("Sending heartbeat for: {}", entity);
            throttle.record_sent(&entity);

            if let Err(e) = wakatime_client.send_heartbeat(&entity).await {
                warn!("Failed to send heartbeat: {}", e);
            }
        }
        ThrottleDecision::Skip => {
            debug!("Throttled heartbeat for: {}", entity);
        }
    }
}
