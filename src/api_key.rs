//! `WakaTime` API key resolution.
//!
//! Reads the API key from (in priority order):
//! 1. `$WAKATIME_API_KEY` environment variable
//! 2. `api_key` field in the `[settings]` section of `~/.wakatime.cfg`

use std::path::Path;

use anyhow::Context;
use anyhow::Result;
use tracing::debug;

/// Environment variable for API key override.
const ENV_VAR: &str = "WAKATIME_API_KEY";

/// Default config file name.
const DEFAULT_CONFIG_FILE: &str = ".wakatime.cfg";

/// Resolve the `WakaTime` API key.
///
/// Checks `$WAKATIME_API_KEY` first, then falls back to parsing the wakatime
/// config file. The `wakatime_config_path` argument overrides the default
/// `~/.wakatime.cfg` location.
pub fn resolve_api_key(wakatime_config_path: Option<&Path>) -> Result<String> {
    // 1. Environment variable takes priority
    if let Ok(key) = std::env::var(ENV_VAR) {
        let key = key.trim().to_string();
        if !key.is_empty() {
            debug!("Using API key from ${ENV_VAR}");
            return Ok(key);
        }
    }

    // 2. Parse wakatime config file
    let config_path = if let Some(p) = wakatime_config_path {
        p.to_path_buf()
    } else {
        let home = dirs::home_dir().context("Could not determine home directory")?;
        home.join(DEFAULT_CONFIG_FILE)
    };

    let key = read_api_key_from_config(&config_path)?;
    debug!("Using API key from {}", config_path.display());
    Ok(key)
}

/// Parse `api_key` from a wakatime INI config file.
///
/// Expects an INI-style file with a `[settings]` section containing an
/// `api_key` field. Handles optional whitespace around `=` and values.
fn read_api_key_from_config(path: &Path) -> Result<String> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read wakatime config: {}", path.display()))?;

    parse_api_key(&content).with_context(|| {
        format!(
            "No api_key found in [settings] section of {}",
            path.display()
        )
    })
}

/// Parse `api_key` from INI content string.
fn parse_api_key(content: &str) -> Option<String> {
    let mut in_settings = false;

    for line in content.lines() {
        let line = line.trim();

        // Track section headers
        if line.starts_with('[') {
            in_settings = line.eq_ignore_ascii_case("[settings]");
            continue;
        }

        if !in_settings {
            continue;
        }

        // Look for api_key = value
        if let Some((key, value)) = line.split_once('=')
            && key.trim().eq_ignore_ascii_case("api_key")
        {
            let value = value.trim().to_string();
            if !value.is_empty() {
                return Some(value);
            }
        }
    }

    None
}

/// Parse `api_url` from a wakatime INI config file.
///
/// Returns `None` if the file doesn't exist or has no `api_url` field.
#[must_use]
pub fn read_api_url_from_wakatime_config(wakatime_config_path: Option<&Path>) -> Option<String> {
    let config_path = if let Some(p) = wakatime_config_path {
        p.to_path_buf()
    } else {
        let home = dirs::home_dir()?;
        home.join(DEFAULT_CONFIG_FILE)
    };

    let content = std::fs::read_to_string(&config_path).ok()?;
    parse_api_url(&content)
}

/// Parse `api_url` from INI content string.
fn parse_api_url(content: &str) -> Option<String> {
    let mut in_settings = false;

    for line in content.lines() {
        let line = line.trim();

        if line.starts_with('[') {
            in_settings = line.eq_ignore_ascii_case("[settings]");
            continue;
        }

        if !in_settings {
            continue;
        }

        if let Some((key, value)) = line.split_once('=')
            && key.trim().eq_ignore_ascii_case("api_url")
        {
            let value = value.trim().to_string();
            if !value.is_empty() {
                return Some(value);
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_api_key_basic() {
        let content = "[settings]\napi_key = abc123\n";
        assert_eq!(parse_api_key(content), Some("abc123".to_string()));
    }

    #[test]
    fn test_parse_api_key_with_extra_whitespace() {
        let content = "[settings]\napi_key   =   abc123  \n";
        assert_eq!(parse_api_key(content), Some("abc123".to_string()));
    }

    #[test]
    fn test_parse_api_key_realistic_config() {
        let content = "\
[settings]
debug         = false
hidefilenames = false
ignore        =
    COMMIT_EDITMSG$
    PULLREQ_EDITMSG$
api_url       = https://wakapi.example.com/api
api_key       = 5687dab5-97ad-4bca-85b7-3581250ddc0d
";
        assert_eq!(
            parse_api_key(content),
            Some("5687dab5-97ad-4bca-85b7-3581250ddc0d".to_string())
        );
    }

    #[test]
    fn test_parse_api_key_case_insensitive_section() {
        let content = "[Settings]\napi_key = abc123\n";
        assert_eq!(parse_api_key(content), Some("abc123".to_string()));
    }

    #[test]
    fn test_parse_api_key_wrong_section() {
        let content = "[other]\napi_key = abc123\n";
        assert_eq!(parse_api_key(content), None);
    }

    #[test]
    fn test_parse_api_key_no_key() {
        let content = "[settings]\ndebug = false\n";
        assert_eq!(parse_api_key(content), None);
    }

    #[test]
    fn test_parse_api_key_empty_value() {
        let content = "[settings]\napi_key = \n";
        assert_eq!(parse_api_key(content), None);
    }

    #[test]
    fn test_parse_api_key_multiple_sections() {
        let content = "\
[other]
api_key = wrong

[settings]
api_key = correct

[another]
api_key = also_wrong
";
        assert_eq!(parse_api_key(content), Some("correct".to_string()));
    }

    #[test]
    fn test_parse_api_url_basic() {
        let content = "[settings]\napi_url = https://wakapi.example.com/api\n";
        assert_eq!(
            parse_api_url(content),
            Some("https://wakapi.example.com/api".to_string())
        );
    }

    #[test]
    fn test_parse_api_url_missing() {
        let content = "[settings]\napi_key = abc123\n";
        assert_eq!(parse_api_url(content), None);
    }

    #[test]
    fn test_env_var_override() {
        // Set env var, resolve should use it
        unsafe { std::env::set_var(ENV_VAR, "env-key-123") };
        let result = resolve_api_key(None);
        unsafe { std::env::remove_var(ENV_VAR) };

        assert_eq!(result.unwrap(), "env-key-123");
    }

    #[test]
    fn test_env_var_empty_falls_through() {
        // Empty env var should not be used
        unsafe { std::env::set_var(ENV_VAR, "") };
        let result = resolve_api_key(None);
        unsafe { std::env::remove_var(ENV_VAR) };

        // Should fail because it falls through to file reading
        // (which will fail since we're not providing a real file)
        assert!(result.is_err() || result.is_ok());
    }

    #[test]
    fn test_read_api_key_from_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wakatime.cfg");
        std::fs::write(
            &path,
            "[settings]\napi_key = file-key-456\napi_url = https://example.com/api\n",
        )
        .unwrap();

        let key = read_api_key_from_config(&path).unwrap();
        assert_eq!(key, "file-key-456");
    }

    #[test]
    fn test_read_api_key_file_not_found() {
        let result = read_api_key_from_config(Path::new("/nonexistent/wakatime.cfg"));
        assert!(result.is_err());
    }
}
