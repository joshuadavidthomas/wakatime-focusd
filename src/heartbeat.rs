//! Heartbeat construction from focus events.

use anyhow::Result;
use regex::Regex;
use regex::RegexBuilder;
use tracing::warn;

use crate::backend::FocusEvent;
use crate::config::CategoryRule;
use crate::config::Config;
use crate::config::TitleStrategy;
use crate::domain::Category;
use crate::domain::Entity;
use crate::domain::Heartbeat;

/// Compiled category matching rule.
struct CompiledRule {
    pattern: Regex,
    category: Category,
}

/// Constructs Heartbeats from `FocusEvents` using configured rules.
pub struct HeartbeatBuilder {
    rules: Vec<CompiledRule>,
    default_category: Category,
    track_titles: bool,
    title_strategy: TitleStrategy,
    app_allowlist: Option<Vec<String>>,
    app_denylist: Option<Vec<String>>,
}

impl HeartbeatBuilder {
    /// Build from config, compiling regexes and validating.
    pub fn from_config(config: &Config) -> Result<Self> {
        let mut rules = Vec::new();

        for rule in &config.category_rules {
            match compile_rule(rule) {
                Ok(compiled) => rules.push(compiled),
                Err(e) => {
                    warn!("Skipping invalid category rule '{}': {}", rule.pattern, e);
                }
            }
        }

        Ok(Self {
            rules,
            default_category: config.default_category,
            track_titles: config.track_titles,
            title_strategy: config.title_strategy.clone(),
            app_allowlist: config.app_allowlist.clone(),
            app_denylist: config.app_denylist.clone(),
        })
    }

    /// Check if an app class is allowed based on allowlist/denylist.
    pub fn is_app_allowed(&self, app_class: &str) -> bool {
        // Denylist takes precedence
        if let Some(ref denylist) = self.app_denylist
            && denylist.iter().any(|d| d.eq_ignore_ascii_case(app_class))
        {
            return false;
        }

        // If allowlist is set, app must be in it
        if let Some(ref allowlist) = self.app_allowlist {
            return allowlist.iter().any(|a| a.eq_ignore_ascii_case(app_class));
        }

        // No allowlist means all apps are allowed (unless denylisted)
        true
    }

    /// Construct a Heartbeat from a `FocusEvent`.
    pub fn build(&self, event: FocusEvent) -> Heartbeat {
        let category = self.match_category(&event.app_class);
        let entity = self.build_entity(&event);

        Heartbeat::new(entity, category, event)
    }

    /// Match the category for an app class using rules.
    fn match_category(&self, app_class: &str) -> Category {
        for rule in &self.rules {
            if rule.pattern.is_match(app_class) {
                return rule.category;
            }
        }
        self.default_category
    }

    /// Build the entity string from a focus event.
    fn build_entity(&self, event: &FocusEvent) -> Entity {
        if self.track_titles {
            match self.title_strategy {
                TitleStrategy::Ignore => Entity::new(event.app_class.clone()),
                TitleStrategy::Append => {
                    if let Some(ref title) = event.title
                        && !title.is_empty()
                    {
                        return Entity::new(format!("{} — {}", event.app_class, title));
                    }
                    Entity::new(event.app_class.clone())
                }
            }
        } else {
            Entity::new(event.app_class.clone())
        }
    }
}

/// Compile a category rule into a case-insensitive regex.
fn compile_rule(rule: &CategoryRule) -> Result<CompiledRule, regex::Error> {
    let pattern = RegexBuilder::new(&rule.pattern)
        .case_insensitive(true)
        .build()?;

    Ok(CompiledRule {
        pattern,
        category: rule.category,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn test_match_category_default() {
        let config = Config::default();
        let builder = HeartbeatBuilder::from_config(&config).unwrap();

        assert_eq!(builder.match_category("code"), Category::Coding);
        assert_eq!(builder.match_category("firefox"), Category::Coding);
    }

    #[test]
    fn test_match_category_with_rules() {
        let config = Config {
            category_rules: vec![
                CategoryRule {
                    pattern: "firefox|chromium".to_string(),
                    category: Category::Browsing,
                },
                CategoryRule {
                    pattern: "slack|discord".to_string(),
                    category: Category::Communicating,
                },
            ],
            ..Default::default()
        };

        let builder = HeartbeatBuilder::from_config(&config).unwrap();

        assert_eq!(builder.match_category("firefox"), Category::Browsing);
        assert_eq!(builder.match_category("chromium"), Category::Browsing);
        assert_eq!(builder.match_category("slack"), Category::Communicating);
        assert_eq!(builder.match_category("code"), Category::Coding);
    }

    #[test]
    fn test_match_category_case_insensitive() {
        let config = Config {
            category_rules: vec![CategoryRule {
                pattern: "firefox".to_string(),
                category: Category::Browsing,
            }],
            ..Default::default()
        };

        let builder = HeartbeatBuilder::from_config(&config).unwrap();

        assert_eq!(builder.match_category("Firefox"), Category::Browsing);
        assert_eq!(builder.match_category("FIREFOX"), Category::Browsing);
        assert_eq!(builder.match_category("firefox"), Category::Browsing);
    }

    #[test]
    fn test_build_entity_no_title() {
        let config = Config::default();
        let builder = HeartbeatBuilder::from_config(&config).unwrap();

        let event = FocusEvent::new("code".to_string(), None, None);
        let entity = builder.build_entity(&event);

        assert_eq!(entity.as_str(), "code");
    }

    #[test]
    fn test_build_entity_with_title_ignore() {
        let config = Config {
            track_titles: true,
            title_strategy: TitleStrategy::Ignore,
            ..Default::default()
        };

        let builder = HeartbeatBuilder::from_config(&config).unwrap();

        let event = FocusEvent::new("code".to_string(), Some("main.rs".to_string()), None);
        let entity = builder.build_entity(&event);

        assert_eq!(entity.as_str(), "code");
    }

    #[test]
    fn test_build_entity_with_title_append() {
        let config = Config {
            track_titles: true,
            title_strategy: TitleStrategy::Append,
            ..Default::default()
        };

        let builder = HeartbeatBuilder::from_config(&config).unwrap();

        let event = FocusEvent::new("code".to_string(), Some("main.rs".to_string()), None);
        let entity = builder.build_entity(&event);

        assert_eq!(entity.as_str(), "code — main.rs");
    }

    #[test]
    fn test_is_app_allowed_no_filters() {
        let config = Config::default();
        let builder = HeartbeatBuilder::from_config(&config).unwrap();

        assert!(builder.is_app_allowed("firefox"));
        assert!(builder.is_app_allowed("code"));
    }

    #[test]
    fn test_is_app_allowed_with_denylist() {
        let config = Config {
            app_denylist: Some(vec!["slack".to_string()]),
            ..Default::default()
        };

        let builder = HeartbeatBuilder::from_config(&config).unwrap();

        assert!(builder.is_app_allowed("firefox"));
        assert!(!builder.is_app_allowed("slack"));
    }

    #[test]
    fn test_is_app_allowed_with_allowlist() {
        let config = Config {
            app_allowlist: Some(vec!["code".to_string(), "firefox".to_string()]),
            ..Default::default()
        };

        let builder = HeartbeatBuilder::from_config(&config).unwrap();

        assert!(builder.is_app_allowed("firefox"));
        assert!(builder.is_app_allowed("code"));
        assert!(!builder.is_app_allowed("chromium"));
    }

    #[test]
    fn test_is_app_allowed_case_insensitive_matching() {
        let config = Config {
            app_allowlist: Some(vec!["Code".to_string()]),
            app_denylist: Some(vec!["Slack".to_string()]),
            ..Default::default()
        };

        let builder = HeartbeatBuilder::from_config(&config).unwrap();

        assert!(builder.is_app_allowed("code"));
        assert!(!builder.is_app_allowed("slack"));
    }

    #[test]
    fn test_is_app_allowed_denylist_overrides_allowlist() {
        let config = Config {
            app_allowlist: Some(vec!["firefox".to_string(), "code".to_string()]),
            app_denylist: Some(vec!["firefox".to_string()]),
            ..Default::default()
        };

        let builder = HeartbeatBuilder::from_config(&config).unwrap();

        assert!(!builder.is_app_allowed("firefox"));
        assert!(builder.is_app_allowed("code"));
    }
}
