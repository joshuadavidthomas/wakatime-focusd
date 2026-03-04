//! Systemd user service management (install / uninstall / status).
//!
//! All functions in this module use blocking I/O (`std::process::Command`,
//! `std::fs`). This is fine because they are only called from CLI subcommands
//! that return before the async runtime's event loop starts. Do not call these
//! from an async context without `spawn_blocking`.

use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

use anyhow::Context;
use anyhow::Result;

const SERVICE_NAME: &str = "wakatime-focusd.service";

/// Return the path to the systemd user service file.
fn service_file_path() -> Result<PathBuf> {
    let config_dir = dirs::config_dir().context("Could not determine config directory")?;
    Ok(config_dir.join("systemd/user").join(SERVICE_NAME))
}

/// Generate the systemd service unit file contents.
fn generate_service_unit(binary_path: &Path) -> String {
    format!(
        "\
[Unit]
Description=WakaTime focus tracking daemon
Documentation=https://github.com/joshuadavidthomas/wakatime-focusd
After=graphical-session.target
PartOf=graphical-session.target

[Service]
Type=simple
ExecStart={binary_path}
Restart=on-failure
RestartSec=2
Environment=RUST_LOG=info
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=graphical-session.target
",
        binary_path = binary_path.display()
    )
}

/// Run a `systemctl --user` command, printing its output.
fn systemctl(args: &[&str]) -> Result<bool> {
    let output = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .context("Failed to run systemctl")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if !stdout.is_empty() {
        print!("{stdout}");
    }
    if !stderr.is_empty() {
        eprint!("{stderr}");
    }

    Ok(output.status.success())
}

/// Install the systemd user service.
pub fn install(now: bool, force: bool) -> Result<()> {
    let binary_path = std::env::current_exe().context("Could not determine binary path")?;
    let binary_path = binary_path
        .canonicalize()
        .context("Could not resolve binary path")?;
    let service_path = service_file_path()?;

    if service_path.exists() && !force {
        anyhow::bail!(
            "Service file already exists at {}\nUse --force to overwrite.",
            service_path.display()
        );
    }

    if let Some(parent) = service_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directory {}", parent.display()))?;
    }

    let unit = generate_service_unit(&binary_path);
    fs::write(&service_path, &unit)
        .with_context(|| format!("Failed to write service file to {}", service_path.display()))?;
    println!("Service file written to {}", service_path.display());

    systemctl(&["daemon-reload"]).context("Failed to reload systemd")?;
    println!("Systemd daemon reloaded.");

    if now {
        println!("Enabling and starting service...");
        systemctl(&["enable", "--now", SERVICE_NAME])
            .context("Failed to enable and start service")?;
        println!("Service enabled and started.");
    } else {
        println!(
            "\nTo enable and start the service:\n  systemctl --user enable --now {SERVICE_NAME}"
        );
    }

    Ok(())
}

/// Stop, disable, and remove the systemd user service.
pub fn uninstall() -> Result<()> {
    let service_path = service_file_path()?;

    // Stop and disable (best-effort — may already be stopped/disabled).
    let _ = systemctl(&["stop", SERVICE_NAME]);
    let _ = systemctl(&["disable", SERVICE_NAME]);

    if service_path.exists() {
        fs::remove_file(&service_path).with_context(|| {
            format!(
                "Failed to remove service file at {}",
                service_path.display()
            )
        })?;
        println!("Removed {}", service_path.display());
    } else {
        println!("No service file found at {}", service_path.display());
    }

    systemctl(&["daemon-reload"]).context("Failed to reload systemd")?;
    println!("Systemd daemon reloaded.");
    println!("Service uninstalled.");

    Ok(())
}

/// Show the service status.
pub fn status() {
    // `systemctl status` returns non-zero for inactive/failed services,
    // which is expected — we just want to show the output.
    let _ = systemctl(&["status", SERVICE_NAME]);
}
