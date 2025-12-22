//! Configuration loading and defaults for wakatime-focusd.

use crate::domain::Category;
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

/// Category rule for pattern-based category assignment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CategoryRule {
    /// Regex pattern to match app_class (case-insensitive).
    pub pattern: String,
    /// Category to assign when pattern matches.
    pub category: Category,
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
            default_category: Category::default(),
            category_rules: Vec::new(),
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
        assert_eq!(config.default_category, Category::Coding);
        assert!(config.category_rules.is_empty());
        assert!(!config.dry_run);
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
        assert_eq!(
            config.app_denylist,
            Some(vec!["spotify".to_string()])
        );
    }
}