//! Heartbeat throttling state machine.
//!
//! Implements WakaTime's throttling rules:
//! - Send immediately on focus/entity change
//! - Send again if >= min_resend_seconds since last send for same entity

use std::time::{Duration, Instant};
use tracing::debug;

/// Decision from the throttle check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThrottleDecision {
    /// Send the heartbeat.
    Send,
    /// Skip this heartbeat (throttled).
    Skip,
}

/// Heartbeat throttle state machine.
#[derive(Debug)]
pub struct HeartbeatThrottle {
    /// Last entity that was sent.
    last_sent_entity: Option<String>,

    /// When the last heartbeat was sent.
    last_sent_at: Option<Instant>,

    /// Minimum seconds before resending for same entity.
    min_resend_seconds: u64,
}

impl HeartbeatThrottle {
    /// Create a new throttle with the given minimum resend interval.
    pub fn new(min_resend_seconds: u64) -> Self {
        Self {
            last_sent_entity: None,
            last_sent_at: None,
            min_resend_seconds,
        }
    }

    /// Check if a heartbeat should be sent for the given entity.
    ///
    /// Returns `Send` if:
    /// - This is a different entity than last sent
    /// - Enough time has passed since last send for same entity
    ///
    /// Returns `Skip` if:
    /// - Same entity and not enough time has passed
    pub fn should_send(&self, entity: &str) -> ThrottleDecision {
        // New entity or first heartbeat - always send
        let Some(ref last_entity) = self.last_sent_entity else {
            debug!("First heartbeat for entity: {}", entity);
            return ThrottleDecision::Send;
        };

        if entity != last_entity {
            debug!(
                "Entity changed: {} -> {}, sending heartbeat",
                last_entity, entity
            );
            return ThrottleDecision::Send;
        }

        // Same entity - check time
        let Some(last_time) = self.last_sent_at else {
            return ThrottleDecision::Send;
        };

        let elapsed = last_time.elapsed();
        let threshold = Duration::from_secs(self.min_resend_seconds);

        if elapsed >= threshold {
            debug!(
                "Same entity '{}', elapsed {:?} >= threshold {:?}, sending",
                entity, elapsed, threshold
            );
            ThrottleDecision::Send
        } else {
            debug!(
                "Throttled: same entity '{}', elapsed {:?} < threshold {:?}",
                entity, elapsed, threshold
            );
            ThrottleDecision::Skip
        }
    }

    /// Record that a heartbeat was sent for the given entity.
    pub fn record_sent(&mut self, entity: &str) {
        self.last_sent_entity = Some(entity.to_string());
        self.last_sent_at = Some(Instant::now());
    }

    /// Get the last sent entity, if any.
    pub fn last_entity(&self) -> Option<&str> {
        self.last_sent_entity.as_deref()
    }

    /// Get time since last heartbeat was sent, if any.
    #[allow(dead_code)]
    pub fn time_since_last_send(&self) -> Option<Duration> {
        self.last_sent_at.map(|t| t.elapsed())
    }

    /// Reset the throttle state.
    #[allow(dead_code)]
    pub fn reset(&mut self) {
        self.last_sent_entity = None;
        self.last_sent_at = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;

    #[test]
    fn test_first_heartbeat_always_sends() {
        let throttle = HeartbeatThrottle::new(120);
        assert_eq!(throttle.should_send("firefox"), ThrottleDecision::Send);
    }

    #[test]
    fn test_different_entity_sends() {
        let mut throttle = HeartbeatThrottle::new(120);

        // First heartbeat
        assert_eq!(throttle.should_send("firefox"), ThrottleDecision::Send);
        throttle.record_sent("firefox");

        // Different entity - should send immediately
        assert_eq!(throttle.should_send("code"), ThrottleDecision::Send);
    }

    #[test]
    fn test_same_entity_throttled() {
        let mut throttle = HeartbeatThrottle::new(120);

        assert_eq!(throttle.should_send("firefox"), ThrottleDecision::Send);
        throttle.record_sent("firefox");

        // Same entity immediately after - should skip
        assert_eq!(throttle.should_send("firefox"), ThrottleDecision::Skip);
    }

    #[test]
    fn test_same_entity_after_timeout() {
        // Use a very short timeout for testing
        let mut throttle = HeartbeatThrottle::new(0);

        assert_eq!(throttle.should_send("firefox"), ThrottleDecision::Send);
        throttle.record_sent("firefox");

        // With 0 second threshold, should send immediately
        sleep(Duration::from_millis(10));
        assert_eq!(throttle.should_send("firefox"), ThrottleDecision::Send);
    }

    #[test]
    fn test_entity_change_sequence() {
        let mut throttle = HeartbeatThrottle::new(120);

        // firefox -> code -> firefox
        assert_eq!(throttle.should_send("firefox"), ThrottleDecision::Send);
        throttle.record_sent("firefox");

        assert_eq!(throttle.should_send("code"), ThrottleDecision::Send);
        throttle.record_sent("code");

        // Back to firefox - should send because entity changed
        assert_eq!(throttle.should_send("firefox"), ThrottleDecision::Send);
    }

    #[test]
    fn test_reset() {
        let mut throttle = HeartbeatThrottle::new(120);

        throttle.record_sent("firefox");
        assert!(throttle.last_entity().is_some());

        throttle.reset();
        assert!(throttle.last_entity().is_none());
        assert!(throttle.time_since_last_send().is_none());
    }

    #[test]
    fn test_time_since_last_send() {
        let mut throttle = HeartbeatThrottle::new(120);

        assert!(throttle.time_since_last_send().is_none());

        throttle.record_sent("firefox");
        sleep(Duration::from_millis(10));

        let elapsed = throttle.time_since_last_send().unwrap();
        assert!(elapsed >= Duration::from_millis(10));
    }
}
