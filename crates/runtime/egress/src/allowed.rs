//! The addresses currently admitted, and how long for.
//!
//! The kernel's sets carry each address with a timeout, so an address stops
//! being reachable on its own when its answer has aged out. This mirror
//! decides what to tell the kernel when another answer names an address:
//! nothing (it is in the set for long enough already), add it, or refresh
//! its timeout. Pure, so the arithmetic is tested without a kernel.

use std::collections::HashMap;
use std::net::IpAddr;
use std::time::{Duration, Instant};

/// The shortest time an answered address stays reachable. Resolvers hand out
/// TTLs of seconds (or zero); a client that resolved and then connects a
/// moment later must still find the address admitted.
pub const MIN_TTL: Duration = Duration::from_secs(60);

/// The longest. An address a name pointed at yesterday should not stay
/// reachable for a day because its record said so.
pub const MAX_TTL: Duration = Duration::from_secs(4 * 60 * 60);

/// A refresh that would extend an entry by less than this is skipped: the
/// common case is many lookups of one name inside its TTL.
pub const REFRESH_SLACK: Duration = Duration::from_secs(15);

/// A record TTL in seconds, clamped to [`MIN_TTL`]..=[`MAX_TTL`].
pub fn clamp_ttl(ttl_secs: u32) -> Duration {
    Duration::from_secs(u64::from(ttl_secs)).clamp(MIN_TTL, MAX_TTL)
}

/// What an answer asks of the kernel for one address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// Not in the set (or aged out): add with this timeout.
    Add(Duration),
    /// In the set but expiring sooner than the new answer says: replace
    /// with this timeout.
    Refresh(Duration),
}

/// The addresses admitted, with when each ages out of the kernel's set.
#[derive(Debug, Default, Clone)]
pub struct AllowedAddrs {
    until: HashMap<IpAddr, Instant>,
}

impl AllowedAddrs {
    /// Record that `addr` was answered with `ttl_secs` at `now`; what the
    /// kernel must be told, if anything.
    pub fn admit(&mut self, addr: IpAddr, ttl_secs: u32, now: Instant) -> Option<Change> {
        let timeout = clamp_ttl(ttl_secs);
        let until = now.checked_add(timeout)?;
        match self.until.get(&addr).copied() {
            Some(current) if current > now => {
                let gain = until.checked_duration_since(current)?;
                if gain < REFRESH_SLACK {
                    return None;
                }
                self.until.insert(addr, until);
                Some(Change::Refresh(timeout))
            }
            _ => {
                self.until.insert(addr, until);
                Some(Change::Add(timeout))
            }
        }
    }

    /// Undo [`Self::admit`] for addresses the kernel did not take.
    pub fn forget(&mut self, addrs: impl IntoIterator<Item = IpAddr>) {
        for addr in addrs {
            self.until.remove(&addr);
        }
    }

    /// Whether `addr` is admitted at `now`.
    pub fn contains(&self, addr: IpAddr, now: Instant) -> bool {
        self.until.get(&addr).is_some_and(|until| *until > now)
    }

    /// Drop entries that have aged out by `now`, so the mirror does not grow
    /// without bound over a long session.
    pub fn prune(&mut self, now: Instant) {
        self.until.retain(|_, until| *until > now);
    }

    /// Forget everything: the kernel's sets were flushed.
    pub fn clear(&mut self) {
        self.until.clear();
    }

    pub fn len(&self) -> usize {
        self.until.len()
    }

    pub fn is_empty(&self) -> bool {
        self.until.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn ttls_are_clamped_to_a_minute_and_four_hours() {
        assert_eq!(clamp_ttl(0), MIN_TTL);
        assert_eq!(clamp_ttl(59), MIN_TTL);
        assert_eq!(clamp_ttl(300), Duration::from_secs(300));
        assert_eq!(clamp_ttl(86_400), MAX_TTL);
        assert_eq!(clamp_ttl(u32::MAX), MAX_TTL);
    }

    #[test]
    fn a_new_address_is_added_and_a_repeat_inside_its_ttl_is_not() {
        let t0 = Instant::now();
        let mut set = AllowedAddrs::default();
        assert_eq!(
            set.admit(ip("140.82.112.3"), 300, t0),
            Some(Change::Add(Duration::from_secs(300)))
        );
        assert!(set.contains(ip("140.82.112.3"), t0));
        // The same answer a few seconds later extends the entry by less than
        // the slack: the kernel is left alone.
        assert_eq!(
            set.admit(ip("140.82.112.3"), 300, t0 + Duration::from_secs(5)),
            None
        );
        // Much later, but before it ages out: a refresh.
        assert_eq!(
            set.admit(ip("140.82.112.3"), 300, t0 + Duration::from_secs(200)),
            Some(Change::Refresh(Duration::from_secs(300)))
        );
        // A shorter TTL never shortens what the kernel holds.
        assert_eq!(
            set.admit(ip("140.82.112.3"), 0, t0 + Duration::from_secs(201)),
            None
        );
        // After it aged out it is new again.
        let later = t0 + Duration::from_secs(1_000);
        assert!(!set.contains(ip("140.82.112.3"), later));
        assert_eq!(
            set.admit(ip("140.82.112.3"), 30, later),
            Some(Change::Add(MIN_TTL))
        );
    }

    #[test]
    fn pruning_and_clearing_forget_entries() {
        let t0 = Instant::now();
        let mut set = AllowedAddrs::default();
        set.admit(ip("192.0.2.1"), 60, t0);
        set.admit(ip("2001:db8::1"), 3_600, t0);
        set.prune(t0 + Duration::from_secs(120));
        assert_eq!(set.len(), 1);
        assert!(set.contains(ip("2001:db8::1"), t0 + Duration::from_secs(120)));
        set.forget([ip("2001:db8::1")]);
        assert!(set.is_empty());
        set.admit(ip("192.0.2.1"), 60, t0);
        set.clear();
        assert!(set.is_empty());
    }
}
