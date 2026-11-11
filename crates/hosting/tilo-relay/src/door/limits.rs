//! Who is asking at the door's wait list: a keyed hash of the client address
//! (no raw address is ever kept) and the per-address windows of the wait
//! list's acts. Taken from the room service's own limits, whose joins the
//! relay's directory door answers itself.

use hmac::{Hmac, KeyInit, Mac};
use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;

const MINUTE_MS: u64 = 60_000;
pub const DAY_MS: u64 = 24 * 60 * MINUTE_MS;
const SHORT_WINDOW_MS: u64 = 10 * MINUTE_MS;

/// Keyed hashes of client addresses, so limits and cooldowns can tell
/// visitors apart without the host ever storing an address.
pub struct IpHasher {
    key: [u8; 32],
}

impl IpHasher {
    pub const fn new(key: [u8; 32]) -> Self {
        Self { key }
    }

    /// HMAC-SHA256 of the address under the host's key. An IPv6 address
    /// counts by its /64, which one subscriber usually holds whole, so
    /// rotating addresses inside it does not dodge a limit.
    pub fn hash(&self, ip: IpAddr) -> String {
        let bytes: Vec<u8> = match ip {
            IpAddr::V4(v4) => v4.octets().to_vec(),
            IpAddr::V6(v6) => v6.octets().get(..8).unwrap_or_default().to_vec(),
        };
        // HMAC is defined for a key of any length, so this never falls back.
        let Ok(mut mac) = <Hmac<sha2::Sha256> as KeyInit>::new_from_slice(&self.key) else {
            return String::new();
        };
        mac.update(&bytes);
        hex::encode(mac.finalize().into_bytes())
    }
}

/// How many times one address may ask: in any 10 minutes and in any day.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JoinRates {
    pub per_ten_minutes: usize,
    pub per_day: usize,
}

impl JoinRates {
    /// Addresses left on a wait list: a person types theirs once, maybe
    /// again after a typo, and a household or office shares an address, so
    /// 5 per 10 minutes and 20 per day; a script filling the list with
    /// made-up addresses gets no further from one address.
    pub const WAIT_LIST: Self = Self {
        per_ten_minutes: 5,
        per_day: 20,
    };

    /// Wait-list entries taken off by their removal secret: a person does
    /// it once, maybe again after joining from another browser, so 10 per
    /// 10 minutes and 50 per day; guessing at secrets gets no further.
    pub const WAIT_LIST_REMOVALS: Self = Self {
        per_ten_minutes: 10,
        per_day: 50,
    };

    /// Confirmation links opened and doors asking where an entry stands:
    /// each page load of a door that holds a secret asks once, so 30 per 10
    /// minutes and 300 per day.
    pub const WAIT_LIST_LOOKUPS: Self = Self {
        per_ten_minutes: 30,
        per_day: 300,
    };
}

/// Sliding windows of asks per address hash.
pub struct JoinLimits {
    short_max: usize,
    day_max: usize,
    joins: HashMap<String, VecDeque<u64>>,
}

impl JoinLimits {
    pub fn new(rates: JoinRates) -> Self {
        Self {
            short_max: rates.per_ten_minutes,
            day_max: rates.per_day,
            joins: HashMap::new(),
        }
    }

    /// Takes back the ask [`Self::check_and_record`] recorded for `ip_hash`
    /// at `at_ms`, for a visitor who got no answer.
    pub fn forget(&mut self, ip_hash: &str, at_ms: u64) {
        if let Some(times) = self.joins.get_mut(ip_hash)
            && let Some(at) = times.iter().rposition(|time| *time == at_ms)
        {
            times.remove(at);
        }
    }

    /// Records an ask from `ip_hash` at `now_ms`, or says in how many
    /// seconds the next one would be allowed.
    pub fn check_and_record(&mut self, ip_hash: &str, now_ms: u64) -> std::result::Result<(), u64> {
        self.joins.retain(|_, times| {
            times
                .back()
                .is_some_and(|last| now_ms.saturating_sub(*last) < DAY_MS)
        });
        let times = self.joins.entry(ip_hash.to_owned()).or_default();
        while times
            .front()
            .is_some_and(|first| now_ms.saturating_sub(*first) >= DAY_MS)
        {
            times.pop_front();
        }
        let recent: Vec<u64> = times
            .iter()
            .copied()
            .filter(|t| now_ms.saturating_sub(*t) < SHORT_WINDOW_MS)
            .collect();
        let wait_ms = if recent.len() >= self.short_max {
            recent
                .first()
                .map(|first| first.saturating_add(SHORT_WINDOW_MS).saturating_sub(now_ms))
        } else if times.len() >= self.day_max {
            times
                .front()
                .map(|first| first.saturating_add(DAY_MS).saturating_sub(now_ms))
        } else {
            None
        };
        if let Some(wait_ms) = wait_ms {
            return Err(wait_ms.div_ceil(1000).max(1));
        }
        times.push_back(now_ms);
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "in a test this is the assertion")]
mod tests {
    use super::*;

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    #[test]
    fn ipv6_addresses_share_their_slash_64() {
        let hasher = IpHasher::new([7; 32]);
        assert_eq!(
            hasher.hash(ip("2001:db8:1:2::1")),
            hasher.hash(ip("2001:db8:1:2:ffff::9"))
        );
        assert_ne!(
            hasher.hash(ip("2001:db8:1:2::1")),
            hasher.hash(ip("2001:db8:1:3::1"))
        );
        assert_ne!(hasher.hash(ip("192.0.2.1")), hasher.hash(ip("192.0.2.2")));
        assert_ne!(
            IpHasher::new([8; 32]).hash(ip("192.0.2.1")),
            hasher.hash(ip("192.0.2.1"))
        );
        assert_eq!(hasher.hash(ip("192.0.2.1")).len(), 64);
    }

    #[test]
    fn five_addresses_per_ten_minutes_and_twenty_per_day() {
        let mut limits = JoinLimits::new(JoinRates::WAIT_LIST);
        for minute in 0..5 {
            limits.check_and_record("h", minute * 60_000).unwrap();
        }
        let retry = limits.check_and_record("h", 5 * 60_000).unwrap_err();
        assert_eq!(retry, 300, "the oldest ask leaves the window at minute 10");
        limits.check_and_record("other", 5 * 60_000).unwrap();
        let mut day = JoinLimits::new(JoinRates::WAIT_LIST);
        for index in 0..20 {
            day.check_and_record("h", index * 11 * 60_000).unwrap();
        }
        assert!(day.check_and_record("h", 20 * 11 * 60_000).is_err());
    }

    #[test]
    fn a_forgotten_ask_frees_its_place_in_the_window() {
        let mut limits = JoinLimits::new(JoinRates::WAIT_LIST);
        for minute in 0..5 {
            limits.check_and_record("h", minute * 60_000).unwrap();
        }
        limits.forget("h", 4 * 60_000);
        limits.check_and_record("h", 5 * 60_000).unwrap();
        assert!(limits.check_and_record("h", 5 * 60_000).is_err());
        // Forgetting what was never recorded changes nothing.
        limits.forget("h", 1);
        limits.forget("nobody", 0);
        assert!(limits.check_and_record("h", 5 * 60_000).is_err());
    }

    #[test]
    fn lookups_get_thirty_per_ten_minutes_and_three_hundred_per_day() {
        let mut limits = JoinLimits::new(JoinRates::WAIT_LIST_LOOKUPS);
        for second in 0..30 {
            limits.check_and_record("h", second * 1_000).unwrap();
        }
        assert_eq!(limits.check_and_record("h", 30_000).unwrap_err(), 570);
        let mut day = JoinLimits::new(JoinRates::WAIT_LIST_LOOKUPS);
        for index in 0..300 {
            day.check_and_record("h", index * 130_000).unwrap();
        }
        assert!(
            day.check_and_record("h", 300 * 130_000).is_err(),
            "the three hundredth of the day is the last"
        );
    }
}
