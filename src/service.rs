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
use wakatime_focusd::backend::Backend;

const SERVICE_NAME: &str = "wakatime-focusd.service";

/// Return the path to the systemd user service file.
fn service_file_path() -> Result<PathBuf> {
    let config_dir = dirs::config_dir().context("Could not determine config directory")?;
    Ok(config_dir.join("systemd/user").join(SERVICE_NAME))
}

/// Build the `ExecStart` value from the binary path and optional CLI flags.
fn build_exec_start(binary_path: &Path, config_path: Option<&Path>, backend: Backend) -> String {
    let mut parts = vec![binary_path.display().to_string()];

    if let Some(cfg) = config_path {
        parts.push(format!("--config {}", cfg.display()));
    }

    if backend != Backend::Auto {
        parts.push(format!("--backend {backend}"));
    }

    parts.join(" ")
}

/// Generate the systemd service unit file contents.
fn generate_service_unit(
    binary_path: &Path,
    config_path: Option<&Path>,
    backend: Backend,
) -> String {
    let exec_start = build_exec_start(binary_path, config_path, backend);
    format!(
        "\
[Unit]
Description=WakaTime focus tracking daemon
Documentation=https://github.com/joshuadavidthomas/wakatime-focusd
After=graphical-session.target
PartOf=graphical-session.target

[Service]
Type=simple
ExecStart={exec_start}
Restart=on-failure
RestartSec=2
Environment=RUST_LOG=info
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=graphical-session.target
"
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
///
/// When `config_path` or a non-`Auto` `backend` is provided, the
/// corresponding CLI flags are embedded in the `ExecStart` line of the
/// generated unit file so the daemon picks them up when started by systemd.
pub fn install(now: bool, force: bool, config_path: Option<&Path>, backend: Backend) -> Result<()> {
    let binary_path = std::env::current_exe().context("Could not determine binary path")?;
    let binary_path = binary_path
        .canonicalize()
        .context("Could not resolve binary path")?;

    // Canonicalize the config path so the unit file contains an absolute path
    // that won't break when systemd starts the daemon from a different cwd.
    let config_path = config_path
        .map(|p| {
            p.canonicalize()
                .with_context(|| format!("Could not resolve config path {}", p.display()))
        })
        .transpose()?;

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

    let unit = generate_service_unit(&binary_path, config_path.as_deref(), backend);
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

#[cfg(test)]
mod tests {
    use std::path::Path;

    use wakatime_focusd::backend::Backend;

    use super::*;

    #[test]
    fn exec_start_bare_binary() {
        let result = build_exec_start(Path::new("/usr/bin/wakatime-focusd"), None, Backend::Auto);
        assert_eq!(result, "/usr/bin/wakatime-focusd");
    }

    #[test]
    fn exec_start_with_config() {
        let result = build_exec_start(
            Path::new("/usr/bin/wakatime-focusd"),
            Some(Path::new("/home/user/.config/wakatime-focusd/custom.toml")),
            Backend::Auto,
        );
        assert_eq!(
            result,
            "/usr/bin/wakatime-focusd --config /home/user/.config/wakatime-focusd/custom.toml"
        );
    }

    #[test]
    fn exec_start_with_backend() {
        let result = build_exec_start(Path::new("/usr/bin/wakatime-focusd"), None, Backend::Sway);
        assert_eq!(result, "/usr/bin/wakatime-focusd --backend sway");
    }

    #[test]
    fn exec_start_with_config_and_backend() {
        let result = build_exec_start(
            Path::new("/usr/bin/wakatime-focusd"),
            Some(Path::new("/etc/wakatime.toml")),
            Backend::Hyprland,
        );
        assert_eq!(
            result,
            "/usr/bin/wakatime-focusd --config /etc/wakatime.toml --backend hyprland"
        );
    }

    #[test]
    fn unit_file_contains_exec_start_with_flags() {
        let unit = generate_service_unit(
            Path::new("/usr/bin/wakatime-focusd"),
            Some(Path::new("/home/user/config.toml")),
            Backend::Gnome,
        );
        assert!(unit.contains(
            "ExecStart=/usr/bin/wakatime-focusd --config /home/user/config.toml --backend gnome"
        ));
    }

    #[test]
    fn unit_file_bare_exec_start_when_defaults() {
        let unit =
            generate_service_unit(Path::new("/usr/bin/wakatime-focusd"), None, Backend::Auto);
        assert!(unit.contains("ExecStart=/usr/bin/wakatime-focusd\n"));
    }
}
