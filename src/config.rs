//! Configuration loading and defaults for wakatime-focusd.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Title handling strategy when track_titles is enabled.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TitleStrategy {
    /// Ignore window titles entirely (default).
    #[default]
    Ignore,
    /// Append title to class: "Class — Title".
    Append,
}

/// Main configuration for wakatime-focusd.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Interval between heartbeats in seconds (default: 120).
    pub heartbeat_interval_seconds: u64,

    /// Minimum seconds before resending heartbeat for same entity (default: 120).
    pub min_entity_resend_seconds: u64,

    /// Whether to include window titles in tracking (default: false).
    pub track_titles: bool,

    /// How to handle titles when track_titles is true.
    pub title_strategy: TitleStrategy,

    /// WakaTime category for heartbeats (default: "coding").
    pub category: String,

    /// Optional allowlist of app classes to track.
    /// If set, only these classes generate heartbeats.
    pub app_allowlist: Option<Vec<String>>,

    /// Optional denylist of app classes to exclude.
    /// Always excluded even if in allowlist.
    pub app_denylist: Option<Vec<String>>,

    /// Path to wakatime-cli binary.
    /// If unset, searches PATH and ~/.wakatime/wakatime-cli*.
    pub wakatime_cli_path: Option<PathBuf>,

    /// Path to wakatime config file.
    /// Forwarded to wakatime-cli --config.
    pub wakatime_config_path: Option<PathBuf>,

    /// Dry run mode: log commands instead of executing.
    pub dry_run: bool,

    /// Idle check interval in seconds (default: 10).
    pub idle_check_interval_seconds: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            heartbeat_interval_seconds: 120,
            min_entity_resend_seconds: 120,
            track_titles: false,
            title_strategy: TitleStrategy::default(),
            category: "coding".to_string(),
            app_allowlist: None,
            app_denylist: None,
            wakatime_cli_path: None,
            wakatime_config_path: None,
            dry_run: false,
            idle_check_interval_seconds: 10,
        }
    }
}

impl Config {
    /// Load configuration from a file path.
    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read config file: {}", path.display()))?;
        let config: Config = toml::from_str(&content)
            .with_context(|| format!("Failed to parse config file: {}", path.display()))?;
        Ok(config)
    }

    /// Load configuration from the default path, or return defaults if not found.
    pub fn load_or_default(path: Option<&Path>) -> Result<Self> {
        if let Some(p) = path {
            return Self::load(p);
        }

        // Try default config path
        if let Some(config_dir) = dirs::config_dir() {
            let default_path = config_dir.join("wakatime-focusd").join("config.toml");
            if default_path.exists() {
                return Self::load(&default_path);
            }
        }

        Ok(Self::default())
    }

    /// Check if an app class is allowed based on allowlist/denylist.
    pub fn is_app_allowed(&self, app_class: &str) -> bool {
        // Denylist takes precedence
        if let Some(ref denylist) = self.app_denylist {
            if denylist.iter().any(|d| d == app_class) {
                return false;
            }
        }

        // If allowlist is set, app must be in it
        if let Some(ref allowlist) = self.app_allowlist {
            return allowlist.iter().any(|a| a == app_class);
        }

        // No allowlist means all apps are allowed (unless denylisted)
        true
    }

    /// Build the entity string from app class and optional title.
    pub fn build_entity(&self, app_class: &str, title: Option<&str>) -> String {
        if self.track_titles {
            match self.title_strategy {
                TitleStrategy::Ignore => app_class.to_string(),
                TitleStrategy::Append => {
                    if let Some(t) = title {
                        if !t.is_empty() {
                            return format!("{} — {}", app_class, t);
                        }
                    }
                    app_class.to_string()
                }
            }
        } else {
            app_class.to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.heartbeat_interval_seconds, 120);
        assert_eq!(config.min_entity_resend_seconds, 120);
        assert!(!config.track_titles);
        assert_eq!(config.category, "coding");
        assert!(!config.dry_run);
    }

    #[test]
    fn test_app_filtering() {
        let mut config = Config::default();

        // No filters - all allowed
        assert!(config.is_app_allowed("firefox"));
        assert!(config.is_app_allowed("code"));

        // With denylist
        config.app_denylist = Some(vec!["slack".to_string()]);
        assert!(config.is_app_allowed("firefox"));
        assert!(!config.is_app_allowed("slack"));

        // With allowlist
        config.app_allowlist = Some(vec!["code".to_string(), "firefox".to_string()]);
        assert!(config.is_app_allowed("firefox"));
        assert!(config.is_app_allowed("code"));
        assert!(!config.is_app_allowed("chromium"));
        assert!(!config.is_app_allowed("slack")); // Still denied

        // Denylist overrides allowlist
        config.app_denylist = Some(vec!["firefox".to_string()]);
        assert!(!config.is_app_allowed("firefox"));
        assert!(config.is_app_allowed("code"));
    }

    #[test]
    fn test_build_entity() {
        let mut config = Config::default();

        // track_titles = false
        assert_eq!(config.build_entity("code", Some("main.rs")), "code");
        assert_eq!(config.build_entity("code", None), "code");

        // track_titles = true, strategy = ignore
        config.track_titles = true;
        config.title_strategy = TitleStrategy::Ignore;
        assert_eq!(config.build_entity("code", Some("main.rs")), "code");

        // track_titles = true, strategy = append
        config.title_strategy = TitleStrategy::Append;
        assert_eq!(
            config.build_entity("code", Some("main.rs")),
            "code — main.rs"
        );
        assert_eq!(config.build_entity("code", None), "code");
        assert_eq!(config.build_entity("code", Some("")), "code");
    }

    #[test]
    fn test_parse_toml() {
        let toml_str = r#"
            heartbeat_interval_seconds = 60
            min_entity_resend_seconds = 60
            track_titles = true
            title_strategy = "append"
            category = "browsing"
            app_denylist = ["slack", "discord"]
            dry_run = true
        "#;

        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.heartbeat_interval_seconds, 60);
        assert!(config.track_titles);
        assert_eq!(config.title_strategy, TitleStrategy::Append);
        assert_eq!(config.category, "browsing");
        assert!(config.dry_run);
        assert_eq!(
            config.app_denylist,
            Some(vec!["slack".to_string(), "discord".to_string()])
        );
    }
}
