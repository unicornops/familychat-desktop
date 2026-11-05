//! Retry backoff for the sync supervisor.
//!
//! Copied from Zmail's `zmail-sync` (`backoff.rs`, unicornops/zmail,
//! GPL-3.0-only like this project), without its connection-uptime helper.

use std::time::Duration;

/// Exponential backoff: each failure doubles the delay up to a cap, and a
/// success starts over from the base delay.
#[derive(Debug, Clone)]
pub(crate) struct Backoff {
    base: Duration,
    max: Duration,
    next: Duration,
}

impl Backoff {
    pub(crate) const fn new(base: Duration, max: Duration) -> Self {
        Self {
            base,
            max,
            next: base,
        }
    }

    /// Returns the delay to wait now and doubles the following one.
    pub(crate) fn next_delay(&mut self) -> Duration {
        let delay = self.next;
        self.next = self.next.saturating_mul(2).min(self.max);
        delay
    }

    /// Starts over from the base delay.
    pub(crate) fn reset(&mut self) {
        self.next = self.base;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delays_double_up_to_the_cap_and_reset() {
        let mut backoff = Backoff::new(Duration::from_secs(1), Duration::from_secs(5));
        let delays = (0..5).map(|_| backoff.next_delay()).collect::<Vec<_>>();
        assert_eq!(
            delays,
            [1, 2, 4, 5, 5].map(Duration::from_secs),
            "the delay doubles and stops at the cap"
        );
        backoff.reset();
        assert_eq!(backoff.next_delay(), Duration::from_secs(1));
    }
}
