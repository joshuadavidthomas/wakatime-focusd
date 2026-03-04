//! wakatime-focusd binary entry point.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use clap::Parser;
use tracing::error;
use tracing::info;
use tracing::warn;
use tracing_subscriber::EnvFilter;
use wakatime_focusd::EventLoopOutcome;
use wakatime_focusd::backend::Backend;
use wakatime_focusd::backend::FocusSource;
use wakatime_focusd::config::Config;
use wakatime_focusd::idle::IdleMonitor;
use wakatime_focusd::wakatime::WakaTimeClient;

/// `WakaTime` focus daemon.
///
/// Tracks currently focused desktop application and sends heartbeats to `WakaTime`.
#[derive(Parser, Debug)]
#[command(name = "wakatime-focusd")]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Path to config file.
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Backend to use for focus detection.
    #[arg(short, long, default_value = "auto")]
    backend: Backend,

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

    // Load config
    let mut config =
        Config::load_or_default(args.config.as_deref()).context("Failed to load configuration")?;

    if args.dry_run {
        config.dry_run = true;
    }

    // CLI --backend flag overrides config file
    if args.backend != Backend::Auto {
        config.backend = args.backend;
    }

    // Resolve the backend (auto-detect if needed)
    let backend = config
        .backend
        .resolve()
        .context("Backend detection failed")?;
    info!("Using backend: {backend}");

    // Show diagnostics
    for diag in wakatime_focusd::backend::diagnostics(backend) {
        tracing::debug!("{}", diag);
    }

    info!("Configuration loaded (dry_run={})", config.dry_run);

    // Oneshot mode
    if args.oneshot {
        return run_oneshot(backend, args.oneshot_count, args.print_events).await;
    }

    // Normal daemon mode
    run_daemon(backend, config, args.print_events).await
}

/// Initialize logging with the specified level.
fn init_logging(level: &str) -> Result<()> {
    let filter = EnvFilter::try_new(format!("wakatime_focusd={level}"))
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
async fn run_oneshot(backend: Backend, count: usize, print_events: bool) -> Result<()> {
    info!("Running in oneshot mode, capturing {} events", count);

    let source = wakatime_focusd::backend::connect(backend).await?;
    run_oneshot_with_source(source, count, print_events).await
}

/// Run oneshot mode with an injected `FocusSource`.
async fn run_oneshot_with_source(
    mut source: Box<dyn FocusSource>,
    count: usize,
    print_events: bool,
) -> Result<()> {
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
async fn run_daemon(backend: Backend, config: Config, print_events: bool) -> Result<()> {
    let wakatime_client =
        WakaTimeClient::from_config(&config).context("Failed to initialize WakaTime client")?;

    let idle_monitor = Arc::new(IdleMonitor::new());
    idle_monitor
        .clone()
        .start_polling(Duration::from_secs(config.idle_check_interval_seconds));

    info!("Daemon started, waiting for focus events...");

    loop {
        let source = wakatime_focusd::backend::connect(backend).await?;
        let outcome = wakatime_focusd::run_event_loop(
            source,
            &config,
            &wakatime_client,
            &idle_monitor,
            print_events,
        )
        .await;

        match outcome {
            EventLoopOutcome::SourceError(e) => {
                error!("Focus event error: {}, reconnecting...", e);
            }
            EventLoopOutcome::Finished => return Ok(()),
        }
    }
}
