//! Domain types for `WakaTime` heartbeats.

use std::fmt;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

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
    #[must_use]
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

impl fmt::Display for Category {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
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
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Entity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
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

    /// Unix timestamp (seconds) when the event occurred.
    pub time: f64,
}

impl Heartbeat {
    /// Create a new heartbeat, capturing the current time.
    #[must_use]
    pub fn new(entity: Entity, category: Category, source: FocusEvent) -> Self {
        let time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before UNIX epoch")
            .as_secs_f64();

        Self {
            entity,
            category,
            source,
            time,
        }
    }
}
