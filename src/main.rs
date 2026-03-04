//! wakatime-focusd binary entry point.

mod service;

use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use clap::Parser;
use clap::Subcommand;
use tokio_util::sync::CancellationToken;
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
    #[command(subcommand)]
    command: Option<Command>,

    /// Path to config file.
    #[arg(short, long, global = true)]
    config: Option<PathBuf>,

    /// Backend to use for focus detection.
    #[arg(short, long, default_value = "auto", global = true)]
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
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Manage configuration.
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },

    /// Manage the systemd user service.
    Service {
        #[command(subcommand)]
        action: ServiceAction,
    },

    /// Capture a few focus events and exit (for debugging).
    Oneshot {
        /// Number of events to capture.
        #[arg(short = 'n', long = "count", default_value = "5")]
        count: usize,
    },
}

#[derive(Subcommand, Debug)]
enum ConfigAction {
    /// Create a default config file with documentation.
    Init {
        /// Write to this path instead of the default location.
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Overwrite an existing config file.
        #[arg(long)]
        force: bool,
    },

    /// Print the resolved configuration and exit.
    Dump,
}

#[derive(Subcommand, Debug)]
enum ServiceAction {
    /// Install the systemd user service.
    ///
    /// Generates a service unit file pointing to the current binary and writes
    /// it to ~/.config/systemd/user/. Runs `systemctl --user daemon-reload`
    /// after installation.
    Install {
        /// Enable and start the service immediately after installing.
        #[arg(long)]
        now: bool,

        /// Overwrite an existing service file.
        #[arg(long)]
        force: bool,
    },

    /// Uninstall the systemd user service.
    ///
    /// Stops and disables the service, removes the unit file, and runs
    /// `systemctl --user daemon-reload`.
    Uninstall,

    /// Show the service status.
    Status,
}

/// Return the default config file path.
fn default_config_path() -> Result<PathBuf> {
    let config_dir = dirs::config_dir().context("Could not determine config directory")?;
    Ok(config_dir.join("wakatime-focusd").join("config.toml"))
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Handle subcommands that don't need the full daemon setup.
    if let Some(command) = &args.command {
        match command {
            Command::Config { action } => match action {
                ConfigAction::Init { output, force } => {
                    return cmd_init(output.as_deref(), *force);
                }
                ConfigAction::Dump => return cmd_dump_config(&args),
            },
            Command::Service { action } => match action {
                ServiceAction::Install { now, force } => {
                    return service::install(*now, *force);
                }
                ServiceAction::Uninstall => return service::uninstall(),
                ServiceAction::Status => {
                    service::status();
                    return Ok(());
                }
            },
            Command::Oneshot { count } => return cmd_oneshot(&args, *count).await,
        }
    }

    // Initialize logging
    init_logging(&args.log_level)?;

    info!("wakatime-focusd v{} starting", env!("CARGO_PKG_VERSION"));

    // Load and resolve config
    let mut config = load_config(&args)?;

    if args.dry_run {
        config.dry_run = true;
    }

    let backend = config
        .backend
        .resolve()
        .context("Backend detection failed")?;
    info!("Using backend: {backend}");

    for diag in wakatime_focusd::backend::diagnostics(backend) {
        tracing::debug!("{}", diag);
    }

    info!("Configuration loaded (dry_run={})", config.dry_run);

    // Normal daemon mode
    run_daemon(backend, config, args.print_events).await
}

/// Load config and apply CLI overrides.
fn load_config(args: &Args) -> Result<Config> {
    let mut config =
        Config::load_or_default(args.config.as_deref()).context("Failed to load configuration")?;

    if args.backend != Backend::Auto {
        config.backend = args.backend;
    }

    Ok(config)
}

/// `init` — create a default config file.
fn cmd_init(output: Option<&Path>, force: bool) -> Result<()> {
    // `--output -` writes to stdout
    if output == Some(Path::new("-")) {
        print!("{}", Config::template());
        return Ok(());
    }

    let path = match output {
        Some(p) => p.to_path_buf(),
        None => default_config_path()?,
    };

    if path.exists() && !force {
        anyhow::bail!(
            "Config file already exists at {}\nUse --force to overwrite.",
            path.display()
        );
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directory {}", parent.display()))?;
    }

    fs::write(&path, Config::template())
        .with_context(|| format!("Failed to write config to {}", path.display()))?;

    println!("Config written to {}", path.display());
    Ok(())
}

/// `dump-config` — print resolved config and exit.
fn cmd_dump_config(args: &Args) -> Result<()> {
    let config = load_config(args)?;
    println!("{}", config.dump()?);
    Ok(())
}

/// `oneshot` — capture a few events and exit.
async fn cmd_oneshot(args: &Args, count: usize) -> Result<()> {
    init_logging(&args.log_level)?;

    let config = load_config(args)?;
    let backend = config
        .backend
        .resolve()
        .context("Backend detection failed")?;
    info!("Using backend: {backend}");

    run_oneshot(backend, count, args.print_events).await
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
    let shutdown = CancellationToken::new();
    idle_monitor.clone().start_polling(
        Duration::from_secs(config.idle_check_interval_seconds),
        shutdown.clone(),
    );

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
                // TODO: add exponential backoff here to avoid tight reconnect
                // loops (individual backends have their own backoff, but a
                // compositor restart can cause rapid SourceError cycles).
                error!("Focus event error: {}, reconnecting...", e);
            }
            EventLoopOutcome::Finished => return Ok(()),
        }
    }
}
