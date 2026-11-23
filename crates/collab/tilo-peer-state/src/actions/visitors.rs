//! How often a visitor may ask: someone signing with a key no certificate in the workspace
//! names, so nothing about them but the network they ask from tells them apart. Their calls
//! are counted per client address, as a keyed hash of it: the device never keeps an address,
//! and a visitor making a new key for every call gets no further.

use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;

const MINUTE_MS: u64 = 60_000;
const DAY_MS: u64 = 24 * 60 * MINUTE_MS;
const SHORT_WINDOW_MS: u64 = 10 * MINUTE_MS;

/// Calls one client address may make in any ten minutes, and in any day. A person leaves an
/// address, maybe again after a typo, confirms it and perhaps takes it off; a household or an
/// office shares an address. A script asking for more gets no further from one network.
pub const VISITOR_CALLS_PER_TEN_MINUTES: usize = 10;

/// See [`VISITOR_CALLS_PER_TEN_MINUTES`].
pub const VISITOR_CALLS_PER_DAY: usize = 50;

/// Client addresses kept at once; the quietest go first past this.
const KEPT_ADDRESSES: usize = 16_384;

/// The calls of visitors, per keyed hash of the network they ask from.
pub(super) struct VisitorWindows {
    key: [u8; 32],
    calls: HashMap<[u8; 32], VecDeque<u64>>,
}

impl VisitorWindows {
    pub(super) fn new(key: [u8; 32]) -> Self {
        Self {
            key,
            calls: HashMap::new(),
        }
    }

    /// The keyed hash `client` is counted by: an IPv6 address by its /64, which one
    /// subscriber usually holds whole, so moving inside it does not dodge the count. A call
    /// from no address the device can tell (one through a relay) is counted with every other
    /// such call.
    fn network(&self, client: Option<IpAddr>) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new_keyed(&self.key);
        match client {
            Some(IpAddr::V4(v4)) => hasher.update(&v4.octets()),
            Some(IpAddr::V6(v6)) => hasher.update(v6.octets().get(..8).unwrap_or_default()),
            None => hasher.update(b"no address"),
        };
        *hasher.finalize().as_bytes()
    }

    /// Count a call from `client` at `now_ms`, or say in how many seconds the next one is
    /// taken.
    pub(super) fn admit(&mut self, client: Option<IpAddr>, now_ms: u64) -> Result<(), u64> {
        let network = self.network(client);
        if self.calls.len() >= KEPT_ADDRESSES && !self.calls.contains_key(&network) {
            self.calls.retain(|_, times| {
                times
                    .back()
                    .is_some_and(|last| now_ms.saturating_sub(*last) < SHORT_WINDOW_MS)
            });
        }
        let times = self.calls.entry(network).or_default();
        while times
            .front()
            .is_some_and(|first| now_ms.saturating_sub(*first) >= DAY_MS)
        {
            times.pop_front();
        }
        let first_recent = times
            .iter()
            .copied()
            .find(|at| now_ms.saturating_sub(*at) < SHORT_WINDOW_MS);
        let recent = times
            .iter()
            .filter(|at| now_ms.saturating_sub(**at) < SHORT_WINDOW_MS)
            .count();
        let wait_ms = if recent >= VISITOR_CALLS_PER_TEN_MINUTES {
            first_recent.map(|first| first.saturating_add(SHORT_WINDOW_MS).saturating_sub(now_ms))
        } else if times.len() >= VISITOR_CALLS_PER_DAY {
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
mod tests {
    use super::*;

    fn ip(text: &str) -> Option<IpAddr> {
        text.parse().ok()
    }

    tilo_test_env::scenario! {
        fn a_network_is_counted_by_a_keyed_hash_and_its_windows;

        phase ipv6_addresses_share_their_slash_64;
        {
            let windows = VisitorWindows::new([7; 32]);
            assert_eq!(
                windows.network(ip("2001:db8:1:2::1")),
                windows.network(ip("2001:db8:1:2:ffff::9"))
            );
            assert_ne!(
                windows.network(ip("2001:db8:1:2::1")),
                windows.network(ip("2001:db8:1:3::1"))
            );
            assert_ne!(windows.network(ip("192.0.2.1")), windows.network(ip("192.0.2.2")));
            assert_ne!(
                VisitorWindows::new([8; 32]).network(ip("192.0.2.1")),
                windows.network(ip("192.0.2.1")),
                "another device's key counts the same address apart"
            );
        }

        phase ten_calls_per_ten_minutes_and_fifty_a_day;
        {
            let mut windows = VisitorWindows::new([7; 32]);
            for minute in 0..10 {
                windows.admit(ip("192.0.2.1"), minute * 30_000).unwrap();
            }
            assert_eq!(
                windows.admit(ip("192.0.2.1"), 5 * MINUTE_MS),
                Err(300),
                "the oldest call leaves the window at minute ten"
            );
            windows.admit(ip("192.0.2.2"), 5 * MINUTE_MS).unwrap();
            let mut day = VisitorWindows::new([7; 32]);
            for index in 0..50 {
                day.admit(ip("192.0.2.1"), index * 11 * MINUTE_MS).unwrap();
            }
            assert!(day.admit(ip("192.0.2.1"), 50 * 11 * MINUTE_MS).is_err());
            assert!(
                day.admit(ip("192.0.2.1"), 24 * 60 * MINUTE_MS + 1).is_ok(),
                "a day after the first, one more is taken"
            );
        }
    }
}
