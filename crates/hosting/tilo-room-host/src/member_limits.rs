//! Token buckets per person (user root) for a public host: a guest who
//! sends too much gets refusals, never a disconnect, and nobody else
//! notices. The relay's own user (its host device, and the bots a
//! host-held root delegated) is never limited; callers skip it.
//!
//! The relay has no blob upload path: content travels as record content
//! routed point to point, so the content budget meters those bytes.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;
use tilo_lock::MutexRecover as _;

pub use tilo_relay_protocol::config::MemberLimitsConfig;

/// Buckets kept before idle, full ones are forgotten.
const KEPT: usize = 4096;

/// Which budget refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Limited {
    Changes,
    /// Too many record content bytes this hour.
    Content,
}

impl std::fmt::Display for Limited {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Changes => tilo_relay_protocol::refusal::TOO_MANY_CHANGES,
            Self::Content => tilo_relay_protocol::refusal::TOO_MUCH_CONTENT,
        })
    }
}

#[derive(Clone, Copy)]
struct Bucket {
    tokens: f64,
    at: Instant,
}

/// A bucket's size and refill rate per second.
#[derive(Clone, Copy)]
struct Rate {
    capacity: f64,
    per_second: f64,
}

impl Bucket {
    fn take(&mut self, amount: f64, rate: Rate, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.at).as_secs_f64();
        self.tokens = elapsed
            .mul_add(rate.per_second, self.tokens)
            .min(rate.capacity);
        self.at = now;
        if self.tokens >= amount {
            self.tokens -= amount;
            true
        } else {
            false
        }
    }

    fn full(&self, rate: Rate, now: Instant) -> bool {
        now.saturating_duration_since(self.at)
            .as_secs_f64()
            .mul_add(rate.per_second, self.tokens)
            >= rate.capacity
    }
}

/// The change and content budgets of every person a public host saw.
pub struct MemberLimits {
    changes: Rate,
    content: Rate,
    people: Mutex<HashMap<[u8; 32], (Bucket, Bucket)>>,
}

impl MemberLimits {
    /// Budgets as `config` sets them, every person starting full.
    #[must_use]
    pub fn new(config: MemberLimitsConfig) -> Self {
        #[expect(
            clippy::cast_precision_loss,
            reason = "byte budgets are far below 2^52"
        )]
        let content = config.content_bytes_per_hour as f64;
        Self {
            changes: Rate {
                capacity: f64::from(config.burst),
                per_second: f64::from(config.changes_per_window)
                    / f64::from(config.window_secs.max(1)),
            },
            content: Rate {
                capacity: content,
                per_second: content / 3600.0,
            },
            people: Mutex::default(),
        }
    }

    fn with_buckets<R>(
        &self,
        user: &[u8; 32],
        now: Instant,
        f: impl FnOnce(&mut (Bucket, Bucket)) -> R,
    ) -> R {
        let (changes, content) = (self.changes, self.content);
        let mut people = self.people.lock_recover();
        if people.len() >= KEPT && !people.contains_key(user) {
            // A full bucket remembers nothing a new one would not.
            people.retain(|_, (a, b)| !(a.full(changes, now) && b.full(content, now)));
        }
        f(people.entry(*user).or_insert((
            Bucket {
                tokens: changes.capacity,
                at: now,
            },
            Bucket {
                tokens: content.capacity,
                at: now,
            },
        )))
    }

    /// Spend one change of `user`'s budget.
    ///
    /// # Errors
    ///
    /// [`Limited::Changes`] when the budget is spent.
    pub fn admit_change(&self, user: &[u8; 32], now: Instant) -> Result<(), Limited> {
        let rate = self.changes;
        self.with_buckets(user, now, |(changes, _)| changes.take(1.0, rate, now))
            .then_some(())
            .ok_or(Limited::Changes)
    }

    /// Spend `bytes` of `user`'s content budget.
    ///
    /// # Errors
    ///
    /// [`Limited::Content`] when the budget does not cover them.
    pub fn admit_content(&self, user: &[u8; 32], bytes: u64, now: Instant) -> Result<(), Limited> {
        let rate = self.content;
        #[expect(
            clippy::cast_precision_loss,
            reason = "byte budgets are far below 2^52"
        )]
        let amount = bytes as f64;
        self.with_buckets(user, now, |(_, content)| content.take(amount, rate, now))
            .then_some(())
            .ok_or(Limited::Content)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "in a test this is the assertion")]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn a_burst_of_forty_passes_then_changes_refill_at_two_per_second() {
        let limits = MemberLimits::new(MemberLimitsConfig::default());
        let (user, start) = ([1; 32], Instant::now());
        for _ in 0..40 {
            limits.admit_change(&user, start).unwrap();
        }
        assert_eq!(limits.admit_change(&user, start), Err(Limited::Changes));
        // 20 per 10 s: one more change after half a second.
        limits
            .admit_change(&user, start + Duration::from_millis(500))
            .unwrap();
        assert_eq!(
            limits.admit_change(&user, start + Duration::from_millis(500)),
            Err(Limited::Changes)
        );
    }

    #[test]
    fn people_have_their_own_budgets() {
        let limits = MemberLimits::new(MemberLimitsConfig::default());
        let now = Instant::now();
        for _ in 0..40 {
            limits.admit_change(&[1; 32], now).unwrap();
        }
        limits.admit_change(&[2; 32], now).unwrap();
    }

    #[test]
    fn eight_mebibytes_of_content_per_hour() {
        let limits = MemberLimits::new(MemberLimitsConfig::default());
        let (user, start) = ([1; 32], Instant::now());
        limits.admit_content(&user, 8 * 1024 * 1024, start).unwrap();
        assert_eq!(limits.admit_content(&user, 1, start), Err(Limited::Content));
        limits
            .admit_content(&user, 1024 * 1024, start + Duration::from_secs(450))
            .unwrap();
    }
}
