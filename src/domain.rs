//! Domain types for `WakaTime` heartbeats.

use serde::Deserialize;
use serde::Serialize;

use crate::backend::FocusEvent;

/// `WakaTime` activity category.
///
/// See: <https://wakatime.com/developers#heartbeats>
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    #[default]
    Coding,
    Building,
    Indexing,
    Debugging,
    Browsing,
    RunningTests,
    WritingTests,
    ManualTesting,
    WritingDocs,
    CodeReviewing,
    Communicating,
    Notes,
    Researching,
    Learning,
    Designing,
    AiCoding,
}

impl Category {
    /// Get the category as a string for `WakaTime` API.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Coding => "coding",
            Self::Building => "building",
            Self::Indexing => "indexing",
            Self::Debugging => "debugging",
            Self::Browsing => "browsing",
            Self::RunningTests => "running tests",
            Self::WritingTests => "writing tests",
            Self::ManualTesting => "manual testing",
            Self::WritingDocs => "writing docs",
            Self::CodeReviewing => "code reviewing",
            Self::Communicating => "communicating",
            Self::Notes => "notes",
            Self::Researching => "researching",
            Self::Learning => "learning",
            Self::Designing => "designing",
            Self::AiCoding => "ai coding",
        }
    }
}

/// Entity sent to `WakaTime` (newtype for type safety).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Entity(String);

impl Entity {
    /// Create a new entity.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Get the entity as a string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for Entity {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for Entity {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

/// Complete heartbeat ready to send to `WakaTime`.
#[derive(Debug, Clone)]
pub struct Heartbeat {
    /// The entity (app name or app + title).
    pub entity: Entity,

    /// The activity category.
    pub category: Category,

    /// The source focus event (for provenance).
    pub source: FocusEvent,
}

impl Heartbeat {
    /// Create a new heartbeat.
    pub fn new(entity: Entity, category: Category, source: FocusEvent) -> Self {
        Self {
            entity,
            category,
            source,
        }
    }
}
