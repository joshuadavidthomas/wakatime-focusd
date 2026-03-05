//! Configuration loading and defaults for wakatime-focusd.

use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use anyhow::Result;
use serde::Deserialize;
use serde::Serialize;

use crate::backend::Backend;
use crate::domain::Category;

/// Which sender backend to use for delivering heartbeats.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SenderBackend {
    /// Send heartbeats directly to the `WakaTime` API (default).
    #[default]
    Api,
    /// Send heartbeats by spawning wakatime-cli (legacy).
    Cli,
}

/// Title handling strategy when `track_titles` is enabled.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TitleStrategy {
    /// Ignore window titles entirely (default).
    #[default]
    Ignore,
    /// Append title to class: "Class — Title".
    Append,
}

/// Category rule for pattern-based category assignment.
///
/// Patterns are case-insensitive regexes that match **anywhere** in the
/// `app_class` string (substring match). Use `^...$` anchors for exact
/// matching, e.g. `"^code$"` matches only `"code"`, not `"unicode-input"`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CategoryRule {
    /// Regex pattern to match `app_class` (case-insensitive, substring match).
    pub pattern: String,
    /// Category to assign when pattern matches.
    pub category: Category,
}

/// Main configuration for wakatime-focusd.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Which backend to use for focus detection (default: auto).
    pub backend: Backend,

    /// Interval between heartbeats in seconds (default: 120).
    pub heartbeat_interval_seconds: u64,

    /// Minimum seconds before resending heartbeat for same entity (default: 120).
    pub min_entity_resend_seconds: u64,

    /// Whether to include window titles in tracking (default: false).
    pub track_titles: bool,

    /// How to handle titles when `track_titles` is true.
    pub title_strategy: TitleStrategy,

    /// Default category for apps that don't match any rule (default: "coding").
    pub default_category: Category,

    /// Category rules evaluated in order (first match wins).
    pub category_rules: Vec<CategoryRule>,

    /// Optional allowlist of app classes to track.
    /// If set, only these classes generate heartbeats.
    pub app_allowlist: Option<Vec<String>>,

    /// Optional denylist of app classes to exclude.
    /// Always excluded even if in allowlist.
    pub app_denylist: Option<Vec<String>>,

    /// How to send heartbeats: "api" (direct HTTP) or "cli" (wakatime-cli).
    pub sender: SenderBackend,

    /// `WakaTime` API base URL (default: <https://api.wakatime.com/api>).
    /// Also read from `api_url` in `~/.wakatime.cfg` if not set here.
    pub api_url: Option<String>,

    /// Path to wakatime-cli binary (only used when sender = "cli").
    /// If unset, searches PATH and ~/.wakatime/wakatime-cli*.
    pub wakatime_cli_path: Option<PathBuf>,

    /// Path to wakatime config file (`~/.wakatime.cfg`).
    /// Used to read the API key (sender = "api") or forwarded to
    /// wakatime-cli --config (sender = "cli").
    pub wakatime_config_path: Option<PathBuf>,

    /// Dry run mode: log commands instead of executing.
    pub dry_run: bool,

    /// Idle check interval in seconds (default: 10).
    pub idle_check_interval_seconds: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            backend: Backend::default(),
            heartbeat_interval_seconds: 120,
            min_entity_resend_seconds: 120,
            track_titles: false,
            title_strategy: TitleStrategy::default(),
            default_category: Category::default(),
            category_rules: Vec::new(),
            app_allowlist: None,
            app_denylist: None,
            sender: SenderBackend::default(),
            api_url: None,
            wakatime_cli_path: None,
            wakatime_config_path: None,
            dry_run: false,
            idle_check_interval_seconds: 10,
        }
    }
}

impl Config {
    /// Return the default config file content with comments.
    ///
    /// Optional fields are commented out so the file is safe to write as-is.
    #[must_use]
    pub fn template() -> &'static str {
        r#"# wakatime-focusd configuration
# Location: ~/.config/wakatime-focusd/config.toml

# Backend for focus detection (default: "auto")
# Options: auto, hyprland, sway, gnome, kde, niri, cosmic, wlr-foreign-toplevel, x11
# "auto" detects your desktop environment automatically.
# backend = "auto"

# Heartbeat interval in seconds (default: 120)
# How often to send heartbeats for the same focused app.
heartbeat_interval_seconds = 120

# Minimum seconds before resending heartbeat for the same entity (default: 120)
# Usually the same as heartbeat_interval_seconds.
min_entity_resend_seconds = 120

# Whether to include window titles in tracking (default: false)
# WARNING: Titles may contain sensitive information (file paths, URLs, etc.)
track_titles = false

# How to handle titles when track_titles is true (default: "ignore")
# Options: "ignore" | "append"
# "append" creates entities like "Class — Title" (high cardinality warning)
title_strategy = "ignore"

# Default category for heartbeats when no rule matches (default: "coding")
# Valid options: coding, building, indexing, debugging, browsing, running tests,
# writing tests, manual testing, writing docs, code reviewing, communicating,
# notes, researching, learning, designing, ai coding
# See: https://wakatime.com/developers#heartbeats
default_category = "coding"

# Category rules - first match wins (case-insensitive regex, substring match).
# Patterns match anywhere in the app class. Use ^...$ anchors for exact matches,
# e.g. "^code$" matches only "code", not "unicode-input".
# [[category_rules]]
# pattern = "firefox|chromium|brave|zen-browser"
# category = "browsing"
#
# [[category_rules]]
# pattern = "thunderbird|evolution|geary"
# category = "communicating"
#
# [[category_rules]]
# pattern = "slack|discord|element"
# category = "communicating"
#
# [[category_rules]]
# pattern = "figma|inkscape|gimp"
# category = "designing"

# Optional: Only track these app classes (empty = track all)
# app_allowlist = ["code", "codium", "nvim", "vim", "emacs"]

# Optional: Never track these app classes
# app_denylist = ["slack", "discord", "spotify"]

# How to send heartbeats (default: "api")
# Options: "api" (direct HTTP, recommended) | "cli" (spawn wakatime-cli, legacy)
# sender = "api"

# WakaTime API base URL (optional)
# Default: https://api.wakatime.com/api
# Also read from api_url in ~/.wakatime.cfg if not set here.
# For self-hosted Wakapi: use your instance URL (e.g. "https://wakapi.example.com/api")
# api_url = "https://api.wakatime.com/api"

# Path to wakatime config file (optional, default: ~/.wakatime.cfg)
# Used to read the API key and api_url.
# The API key can also be set via the $WAKATIME_API_KEY environment variable.
# wakatime_config_path = "/home/user/.wakatime.cfg"

# Path to wakatime-cli binary (only used when sender = "cli")
# If not set, searches PATH and ~/.wakatime/
# wakatime_cli_path = "/usr/bin/wakatime-cli"

# Idle check interval in seconds (default: 10)
# How often to poll systemd-logind for idle state.
idle_check_interval_seconds = 10

# Dry run mode: log commands instead of executing (default: false)
dry_run = false
"#
    }

    /// Serialize the resolved config to TOML.
    pub fn dump(&self) -> Result<String> {
        toml::to_string_pretty(self).context("Failed to serialize config")
    }

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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.backend, Backend::Auto);
        assert_eq!(config.heartbeat_interval_seconds, 120);
        assert_eq!(config.min_entity_resend_seconds, 120);
        assert!(!config.track_titles);
        assert_eq!(config.default_category, Category::Coding);
        assert!(config.category_rules.is_empty());
        assert!(!config.dry_run);
    }

    #[test]
    fn test_parse_toml_with_backend() {
        let toml_str = r#"
            backend = "sway"
            heartbeat_interval_seconds = 60
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.backend, Backend::Sway);
    }

    #[test]
    fn test_parse_toml_with_category_rules() {
        let toml_str = r#"
            heartbeat_interval_seconds = 60
            track_titles = true
            title_strategy = "append"
            default_category = "browsing"
            app_denylist = ["spotify"]
            dry_run = true

            [[category_rules]]
            pattern = "firefox|chromium"
            category = "browsing"

            [[category_rules]]
            pattern = "slack|discord"
            category = "communicating"
        "#;

        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.heartbeat_interval_seconds, 60);
        assert!(config.track_titles);
        assert_eq!(config.title_strategy, TitleStrategy::Append);
        assert_eq!(config.default_category, Category::Browsing);
        assert_eq!(config.category_rules.len(), 2);
        assert_eq!(config.category_rules[0].pattern, "firefox|chromium");
        assert_eq!(config.category_rules[0].category, Category::Browsing);
        assert_eq!(config.category_rules[1].pattern, "slack|discord");
        assert_eq!(config.category_rules[1].category, Category::Communicating);
        assert!(config.dry_run);
        assert_eq!(config.app_denylist, Some(vec!["spotify".to_string()]));
    }

    #[test]
    fn test_template_is_valid_toml() {
        let config: Config = toml::from_str(Config::template()).unwrap();
        // Template should parse to defaults since optional fields are commented out
        assert_eq!(config.heartbeat_interval_seconds, 120);
        assert!(!config.dry_run);
    }

    #[test]
    fn test_dump_roundtrips() {
        let config = Config::default();
        let dumped = config.dump().unwrap();
        let reloaded: Config = toml::from_str(&dumped).unwrap();
        assert_eq!(
            reloaded.heartbeat_interval_seconds,
            config.heartbeat_interval_seconds
        );
        assert_eq!(reloaded.backend, config.backend);
        assert_eq!(reloaded.dry_run, config.dry_run);
        assert_eq!(reloaded.track_titles, config.track_titles);
    }

    #[test]
    fn test_dump_preserves_overrides() {
        let config = Config {
            backend: Backend::Sway,
            dry_run: true,
            heartbeat_interval_seconds: 60,
            ..Config::default()
        };
        let dumped = config.dump().unwrap();
        let reloaded: Config = toml::from_str(&dumped).unwrap();
        assert_eq!(reloaded.backend, Backend::Sway);
        assert!(reloaded.dry_run);
        assert_eq!(reloaded.heartbeat_interval_seconds, 60);
    }
}
