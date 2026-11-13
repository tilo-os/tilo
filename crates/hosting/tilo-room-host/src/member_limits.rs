//! Token buckets per person (user root) for a public host: a guest who
//! sends too much gets refusals, never a disconnect, and nobody else
//! notices. The relay's own user (its host device, and the bots a
//! host-held root delegated) is never limited; callers skip it. A person
//! changing what is theirs ([`Standing::Owner`]) spends a budget of its own,
//! far larger than a guest's, so pacing guests never paces an owner's
//! backend.
//!
//! The relay has no blob upload path: content travels as record content
//! routed point to point, so the content budget meters those bytes.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tilo_lock::MutexRecover as _;

pub use tilo_relay_protocol::config::MemberLimitsConfig;

/// Buckets kept before idle, full ones are forgotten.
const KEPT: usize = 4096;

/// Which budget refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Limited {
    /// Too many changes: the budget admits the next one `retry_after` from
    /// now.
    Changes { retry_after: Duration },
    /// Too many record content bytes this hour.
    Content,
}

impl std::fmt::Display for Limited {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Changes { .. } => tilo_relay_protocol::refusal::TOO_MANY_CHANGES,
            Self::Content => tilo_relay_protocol::refusal::TOO_MUCH_CONTENT,
        })
    }
}

/// Whose a change is, as the change budget sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Standing {
    /// A person acting where they are a guest.
    Guest,
    /// A person changing a workspace of their own, or acting through their
    /// server key.
    Owner,
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
    /// Spend `amount`, or say how long until the bucket holds it.
    fn take(&mut self, amount: f64, rate: Rate, now: Instant) -> Result<(), Duration> {
        let elapsed = now.saturating_duration_since(self.at).as_secs_f64();
        self.tokens = elapsed
            .mul_add(rate.per_second, self.tokens)
            .min(rate.capacity);
        self.at = now;
        if self.tokens >= amount {
            self.tokens -= amount;
            Ok(())
        } else {
            let missing = (amount - self.tokens) / rate.per_second.max(f64::MIN_POSITIVE);
            Err(Duration::try_from_secs_f64(missing).unwrap_or(Duration::MAX))
        }
    }

    fn full(&self, rate: Rate, now: Instant) -> bool {
        now.saturating_duration_since(self.at)
            .as_secs_f64()
            .mul_add(rate.per_second, self.tokens)
            >= rate.capacity
    }
}

/// A person's buckets: guest changes, owner changes, content.
type Buckets = (Bucket, Bucket, Bucket);

/// The change and content budgets of every person a public host saw.
pub struct MemberLimits {
    changes: Rate,
    owner_changes: Rate,
    content: Rate,
    people: Mutex<HashMap<[u8; 32], Buckets>>,
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
        let window = f64::from(config.window_secs.max(1));
        Self {
            changes: Rate {
                capacity: f64::from(config.burst),
                per_second: f64::from(config.changes_per_window) / window,
            },
            owner_changes: Rate {
                capacity: f64::from(config.owner_burst),
                per_second: f64::from(config.owner_changes_per_window) / window,
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
        f: impl FnOnce(&mut Buckets) -> R,
    ) -> R {
        let (changes, owner, content) = (self.changes, self.owner_changes, self.content);
        let mut people = self.people.lock_recover();
        if people.len() >= KEPT && !people.contains_key(user) {
            // A full bucket remembers nothing a new one would not.
            people.retain(|_, (a, b, c)| {
                !(a.full(changes, now) && b.full(owner, now) && c.full(content, now))
            });
        }
        let full = |rate: Rate| Bucket {
            tokens: rate.capacity,
            at: now,
        };
        f(people
            .entry(*user)
            .or_insert_with(|| (full(changes), full(owner), full(content))))
    }

    /// Spend one change of `user`'s budget for their `standing`.
    ///
    /// # Errors
    ///
    /// [`Limited::Changes`] when the budget is spent, with when it admits
    /// the next change.
    pub fn admit_change(
        &self,
        user: &[u8; 32],
        standing: Standing,
        now: Instant,
    ) -> Result<(), Limited> {
        let rate = match standing {
            Standing::Guest => self.changes,
            Standing::Owner => self.owner_changes,
        };
        self.with_buckets(user, now, |(guest, owner, _)| {
            match standing {
                Standing::Guest => guest,
                Standing::Owner => owner,
            }
            .take(1.0, rate, now)
        })
        .map_err(|retry_after| Limited::Changes { retry_after })
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
        self.with_buckets(user, now, |(_, _, content)| content.take(amount, rate, now))
            .map_err(|_| Limited::Content)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "in a test this is the assertion")]
mod tests {
    use super::*;
    use std::time::Duration;

    const GUEST: Standing = Standing::Guest;

    #[test]
    fn a_burst_of_forty_passes_then_changes_refill_at_two_per_second() {
        let limits = MemberLimits::new(MemberLimitsConfig::default());
        let (user, start) = ([1; 32], tilo_clock::now());
        for _ in 0..40 {
            limits.admit_change(&user, GUEST, start).unwrap();
        }
        // 20 per 10 s: one more change after half a second, which the
        // refusal says.
        assert_eq!(
            limits.admit_change(&user, GUEST, start),
            Err(Limited::Changes {
                retry_after: Duration::from_millis(500)
            })
        );
        let later = start + Duration::from_millis(500);
        limits.admit_change(&user, GUEST, later).unwrap();
        assert!(matches!(
            limits.admit_change(&user, GUEST, later),
            Err(Limited::Changes { .. })
        ));
    }

    #[test]
    fn people_have_their_own_budgets() {
        let limits = MemberLimits::new(MemberLimitsConfig::default());
        let now = tilo_clock::now();
        for _ in 0..40 {
            limits.admit_change(&[1; 32], GUEST, now).unwrap();
        }
        limits.admit_change(&[2; 32], GUEST, now).unwrap();
    }

    #[test]
    fn an_owner_is_never_paced_by_the_guest_budget() {
        let limits = MemberLimits::new(MemberLimitsConfig::default());
        let (user, now) = ([1; 32], tilo_clock::now());
        for _ in 0..40 {
            limits.admit_change(&user, GUEST, now).unwrap();
        }
        assert!(limits.admit_change(&user, GUEST, now).is_err());
        // The same person making their own workspaces: a thousand at once.
        for _ in 0..1000 {
            limits.admit_change(&user, Standing::Owner, now).unwrap();
        }
        assert!(limits.admit_change(&user, Standing::Owner, now).is_err());
    }

    #[test]
    fn eight_mebibytes_of_content_per_hour() {
        let limits = MemberLimits::new(MemberLimitsConfig::default());
        let (user, start) = ([1; 32], tilo_clock::now());
        limits.admit_content(&user, 8 * 1024 * 1024, start).unwrap();
        assert_eq!(limits.admit_content(&user, 1, start), Err(Limited::Content));
        limits
            .admit_content(&user, 1024 * 1024, start + Duration::from_secs(450))
            .unwrap();
    }
}
