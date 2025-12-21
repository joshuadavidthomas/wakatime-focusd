//! Idle detection module.
//!
//! Provides idle state tracking via systemd-logind DBus interface.

pub mod logind;

pub use logind::IdleMonitor;
