//! Heartbeat throttling state machine.
//!
//! Implements WakaTime's throttling rules:
//! - Send immediately on focus/entity change
//! - Send again if >= min_resend_seconds since last send for same entity

use crate::domain::{Entity, Heartbeat};
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
    /// Last heartbeat that was sent.
    last_sent: Option<SentHeartbeat>,

    /// Minimum seconds before resending for same entity.
    min_resend_seconds: u64,
}

/// A heartbeat that was successfully sent.
#[derive(Debug)]
struct SentHeartbeat {
    /// The complete heartbeat that was sent.
    heartbeat: Heartbeat,
    /// When it was sent.
    sent_at: Instant,
}

impl HeartbeatThrottle {
    /// Create a new throttle with the given minimum resend interval.
    pub fn new(min_resend_seconds: u64) -> Self {
        Self {
            last_sent: None,
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
    pub fn should_send(&self, entity: &Entity) -> ThrottleDecision {
        // First heartbeat - always send
        let Some(ref last_sent) = self.last_sent else {
            debug!("First heartbeat for entity: {}", entity.as_str());
            return ThrottleDecision::Send;
        };

        // Check entity change
        if &last_sent.heartbeat.entity != entity {
            debug!(
                "Entity changed: {} -> {}, sending heartbeat",
                last_sent.heartbeat.entity.as_str(),
                entity.as_str()
            );
            return ThrottleDecision::Send;
        }

        // Same entity - check time
        let elapsed = last_sent.sent_at.elapsed();
        let threshold = Duration::from_secs(self.min_resend_seconds);

        if elapsed >= threshold {
            debug!(
                "Same entity '{}', elapsed {:?} >= threshold {:?}, sending",
                entity.as_str(),
                elapsed,
                threshold
            );
            ThrottleDecision::Send
        } else {
            debug!(
                "Throttled: same entity '{}', elapsed {:?} < threshold {:?}",
                entity.as_str(),
                elapsed,
                threshold
            );
            ThrottleDecision::Skip
        }
    }

    /// Record that a heartbeat was sent.
    pub fn record_sent(&mut self, heartbeat: Heartbeat) {
        self.last_sent = Some(SentHeartbeat {
            heartbeat,
            sent_at: Instant::now(),
        });
    }

    /// Get the last sent heartbeat, if any.
    pub fn last_heartbeat(&self) -> Option<&Heartbeat> {
        self.last_sent.as_ref().map(|s| &s.heartbeat)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::FocusEvent;
    use crate::domain::{Category, Heartbeat};
    use std::thread::sleep;

    fn test_heartbeat(app_class: &str) -> Heartbeat {
        Heartbeat::new(
            Entity::new(app_class),
            Category::Coding,
            FocusEvent::new(app_class.to_string(), None, None),
        )
    }

    #[test]
    fn test_first_heartbeat_always_sends() {
        let throttle = HeartbeatThrottle::new(120);
        let heartbeat = test_heartbeat("firefox");
        assert_eq!(
            throttle.should_send(&heartbeat.entity),
            ThrottleDecision::Send
        );
    }

    #[test]
    fn test_different_entity_sends() {
        let mut throttle = HeartbeatThrottle::new(120);

        // First heartbeat
        let firefox = test_heartbeat("firefox");
        assert_eq!(
            throttle.should_send(&firefox.entity),
            ThrottleDecision::Send
        );
        throttle.record_sent(firefox);

        // Different entity - should send immediately
        let code = test_heartbeat("code");
        assert_eq!(throttle.should_send(&code.entity), ThrottleDecision::Send);
    }

    #[test]
    fn test_same_entity_throttled() {
        let mut throttle = HeartbeatThrottle::new(120);
        let firefox = test_heartbeat("firefox");

        assert_eq!(
            throttle.should_send(&firefox.entity),
            ThrottleDecision::Send
        );
        throttle.record_sent(firefox);

        // Same entity immediately after - should skip
        let another_firefox = test_heartbeat("firefox");
        assert_eq!(
            throttle.should_send(&another_firefox.entity),
            ThrottleDecision::Skip
        );
    }

    #[test]
    fn test_same_entity_after_timeout() {
        // Use a very short timeout for testing
        let mut throttle = HeartbeatThrottle::new(0);
        let firefox = test_heartbeat("firefox");

        assert_eq!(
            throttle.should_send(&firefox.entity),
            ThrottleDecision::Send
        );
        throttle.record_sent(firefox);

        // With 0 second threshold, should send immediately
        sleep(Duration::from_millis(10));
        let another_firefox = test_heartbeat("firefox");
        assert_eq!(
            throttle.should_send(&another_firefox.entity),
            ThrottleDecision::Send
        );
    }

    #[test]
    fn test_entity_change_sequence() {
        let mut throttle = HeartbeatThrottle::new(120);

        // firefox -> code -> firefox
        let firefox1 = test_heartbeat("firefox");
        assert_eq!(
            throttle.should_send(&firefox1.entity),
            ThrottleDecision::Send
        );
        throttle.record_sent(firefox1);

        let code = test_heartbeat("code");
        assert_eq!(throttle.should_send(&code.entity), ThrottleDecision::Send);
        throttle.record_sent(code);

        // Back to firefox - should send because entity changed
        let firefox2 = test_heartbeat("firefox");
        assert_eq!(
            throttle.should_send(&firefox2.entity),
            ThrottleDecision::Send
        );
    }

    #[test]
    fn test_last_heartbeat() {
        let mut throttle = HeartbeatThrottle::new(120);
        assert!(throttle.last_heartbeat().is_none());

        let heartbeat = test_heartbeat("firefox");
        throttle.record_sent(heartbeat.clone());

        let last = throttle.last_heartbeat().unwrap();
        assert_eq!(last.entity.as_str(), "firefox");
    }
}
