//! wakatime-focusd binary entry point.

mod service;

use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use axoupdater::AxoUpdater;
use axoupdater::UpdateRequest;
use clap::Parser;
use clap::Subcommand;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tracing::error;
use tracing::info;
use tracing::warn;
use tracing_subscriber::EnvFilter;
use wakatime_focusd::EventLoopOutcome;
use wakatime_focusd::api::ApiSender;
use wakatime_focusd::backend::Backend;
use wakatime_focusd::backend::FocusSource;
use wakatime_focusd::config::Config;
use wakatime_focusd::config::SenderBackend;
use wakatime_focusd::idle::IdleMonitor;
use wakatime_focusd::wakatime::HeartbeatSender;
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

    /// Update to the latest release.
    ///
    /// Checks for a newer version and installs it. Only works when installed
    /// via the shell installer (cargo-dist). Builds from source or
    /// cargo-binstall should use their original install method to update.
    Update,
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
    ///
    /// If --config or --backend are provided, the corresponding flags are
    /// embedded in the `ExecStart` line of the generated unit file so the
    /// daemon uses them when started by systemd.
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
                    return service::install(*now, *force, args.config.as_deref(), args.backend);
                }
                ServiceAction::Uninstall => return service::uninstall(),
                ServiceAction::Status => {
                    service::status();
                    return Ok(());
                }
            },
            Command::Oneshot { count } => return cmd_oneshot(&args, *count).await,
            Command::Update => return cmd_update().await,
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
    let cli_overrides = CliOverrides {
        config_path: args.config,
        backend: args.backend,
        dry_run: args.dry_run,
    };
    run_daemon(backend, config, cli_overrides, args.print_events).await
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

/// `update` — check for and install the latest release.
async fn cmd_update() -> Result<()> {
    let mut updater = AxoUpdater::new_for("wakatime-focusd");
    updater.configure_version_specifier(UpdateRequest::Latest);

    if let Ok(token) = std::env::var("GITHUB_TOKEN") {
        updater.set_github_token(&token);
    }

    match updater.load_receipt() {
        Ok(_) => {}
        Err(e) => {
            // Match on the specific error types for a friendly message
            let msg = e.to_string();
            if msg.contains("Unable to load receipt") || msg.contains("Unable to read") {
                eprintln!("No install receipt found for wakatime-focusd.");
                eprintln!();
                eprintln!("Self-update only works when installed via the shell installer:");
                eprintln!(
                    "  curl --proto '=https' --tlsv1.2 -LsSf \
                     https://github.com/joshuadavidthomas/wakatime-focusd/releases/latest/download/wakatime-focusd-installer.sh | sh"
                );
                eprintln!();
                eprintln!(
                    "If you installed via cargo or built from source, use that method to update."
                );
                std::process::exit(1);
            }
            return Err(e).context("Failed to load install receipt");
        }
    }

    eprintln!("Checking for updates...");

    match updater.run().await {
        Ok(Some(result)) => {
            eprintln!("Updated wakatime-focusd to {}!", result.new_version);
            eprintln!();
            eprintln!("If running as a systemd service, restart it to use the new version:");
            eprintln!("  systemctl --user restart wakatime-focusd");
        }
        Ok(None) => {
            eprintln!("Already up to date (v{}).", env!("CARGO_PKG_VERSION"));
        }
        Err(e) => {
            return Err(e).context("Update failed");
        }
    }

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

/// CLI overrides that need to be reapplied when config is reloaded.
struct CliOverrides {
    config_path: Option<PathBuf>,
    backend: Backend,
    dry_run: bool,
}

/// Install signal handlers and return a [`CancellationToken`] that is
/// cancelled on `SIGINT` or `SIGTERM`.
fn setup_shutdown_signal(shutdown: CancellationToken) {
    tokio::spawn(async move {
        #[cfg(unix)]
        {
            use tokio::signal::unix::SignalKind;
            use tokio::signal::unix::signal;

            let mut sigterm =
                signal(SignalKind::terminate()).expect("failed to register SIGTERM handler");
            let mut sigint =
                signal(SignalKind::interrupt()).expect("failed to register SIGINT handler");

            tokio::select! {
                _ = sigterm.recv() => info!("Received SIGTERM"),
                _ = sigint.recv() => info!("Received SIGINT"),
            }
        }

        #[cfg(not(unix))]
        {
            tokio::signal::ctrl_c()
                .await
                .expect("failed to register ctrl-c handler");
            info!("Received ctrl-c");
        }

        shutdown.cancel();
    });
}

/// Install a `SIGHUP` handler that notifies the reload signal.
///
/// Each `SIGHUP` wakes the event loop so it can reload the configuration
/// file from disk and apply changes without a full restart.
#[cfg(unix)]
fn setup_reload_signal(reload: Arc<Notify>) {
    use tokio::signal::unix::SignalKind;
    use tokio::signal::unix::signal;

    tokio::spawn(async move {
        let mut sighup = signal(SignalKind::hangup()).expect("failed to register SIGHUP handler");
        loop {
            sighup.recv().await;
            info!("Received SIGHUP, requesting config reload");
            reload.notify_one();
        }
    });
}

/// Initial delay before retrying a failed backend connection.
const RECONNECT_INITIAL_BACKOFF: Duration = Duration::from_millis(500);

/// Maximum delay between backend reconnection attempts.
const RECONNECT_MAX_BACKOFF: Duration = Duration::from_secs(30);

/// Reload the configuration from disk, applying CLI overrides.
fn reload_config(overrides: &CliOverrides) -> Result<Config> {
    let mut config = Config::load_or_default(overrides.config_path.as_deref())
        .context("Failed to reload configuration")?;

    if overrides.backend != Backend::Auto {
        config.backend = overrides.backend;
    }
    if overrides.dry_run {
        config.dry_run = true;
    }

    Ok(config)
}

/// Create the appropriate heartbeat sender based on config.
fn create_sender(config: &Config) -> Result<Box<dyn HeartbeatSender + Sync>> {
    match config.sender {
        SenderBackend::Api => {
            let sender = ApiSender::from_config(config)?;
            Ok(Box::new(sender))
        }
        SenderBackend::Cli => {
            let sender = WakaTimeClient::from_config(config)?;
            Ok(Box::new(sender))
        }
    }
}

/// Run daemon event loop.
async fn run_daemon(
    backend: Backend,
    initial_config: Config,
    cli_overrides: CliOverrides,
    print_events: bool,
) -> Result<()> {
    let mut config = initial_config;
    let mut sender: Box<dyn HeartbeatSender + Sync> =
        create_sender(&config).context("Failed to initialize heartbeat sender")?;

    let shutdown = CancellationToken::new();
    setup_shutdown_signal(shutdown.clone());

    let reload_signal = Arc::new(Notify::new());
    #[cfg(unix)]
    setup_reload_signal(Arc::clone(&reload_signal));

    let mut idle_monitor = Arc::new(IdleMonitor::new());
    let mut idle_shutdown = CancellationToken::new();
    idle_monitor.clone().start_polling(
        Duration::from_secs(config.idle_check_interval_seconds),
        idle_shutdown.clone(),
    );

    info!("Daemon started, waiting for focus events...");

    let mut backoff = RECONNECT_INITIAL_BACKOFF;

    loop {
        let source = match wakatime_focusd::backend::connect(backend).await {
            Ok(source) => {
                backoff = RECONNECT_INITIAL_BACKOFF;
                source
            }
            Err(e) => {
                error!(
                    "Failed to connect to backend: {}. Retrying in {:?}...",
                    e, backoff
                );
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(RECONNECT_MAX_BACKOFF);
                continue;
            }
        };

        let outcome = wakatime_focusd::run_event_loop(
            source,
            &config,
            &*sender,
            &idle_monitor,
            &shutdown,
            &reload_signal,
            print_events,
        )
        .await;

        match outcome {
            EventLoopOutcome::SourceError(e) => {
                error!("Focus event error: {}. Reconnecting in {:?}...", e, backoff);
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(RECONNECT_MAX_BACKOFF);
            }
            EventLoopOutcome::Reload => {
                info!("Reloading configuration...");
                match reload_config(&cli_overrides) {
                    Ok(new_config) => {
                        if new_config.backend != config.backend {
                            warn!(
                                "Backend change ({} -> {}) requires a restart and will be ignored",
                                config.backend, new_config.backend,
                            );
                        }

                        match create_sender(&new_config) {
                            Ok(new_sender) => sender = new_sender,
                            Err(e) => {
                                error!(
                                    "Failed to initialize heartbeat sender after reload: {e}. \
                                     Keeping current configuration."
                                );
                                continue;
                            }
                        }

                        // Restart idle polling with potentially new interval
                        idle_shutdown.cancel();
                        idle_monitor = Arc::new(IdleMonitor::new());
                        idle_shutdown = CancellationToken::new();
                        idle_monitor.clone().start_polling(
                            Duration::from_secs(new_config.idle_check_interval_seconds),
                            idle_shutdown.clone(),
                        );

                        config = new_config;
                        backoff = RECONNECT_INITIAL_BACKOFF;
                        info!("Configuration reloaded successfully");
                    }
                    Err(e) => {
                        error!(
                            "Failed to reload configuration: {e}. Keeping current configuration."
                        );
                    }
                }
            }
            EventLoopOutcome::Finished | EventLoopOutcome::Shutdown => {
                idle_shutdown.cancel();
                info!("Daemon shutting down");
                return Ok(());
            }
        }
    }
}
