//! Who is joining: the client address (behind trusted proxies only), a
//! keyed hash of it (no raw address is ever kept), and the per-address
//! join windows of a public room.

use hmac::{Hmac, KeyInit, Mac};
use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;

const MINUTE_MS: u64 = 60_000;
pub const DAY_MS: u64 = 24 * 60 * MINUTE_MS;
const SHORT_WINDOW_MS: u64 = 10 * MINUTE_MS;
/// How long an admitted visitor's seat is held for them before the relay's
/// count must show them: a page opens its session within seconds.
const SEAT_HOLD_MS: u64 = 30_000;

/// Address ranges whose `X-Forwarded-For` is believed: the cluster's own
/// ingress, never the open internet.
pub use tilo_relay_protocol::proxies::TrustedProxies;

/// The visitor's address: the peer, unless the peer is a trusted proxy,
/// in which case the rightmost `X-Forwarded-For` entry that is not itself
/// a trusted proxy. Entries further left are the visitor's own text.
pub fn client_ip(peer: IpAddr, forwarded_for: Option<&str>, trusted: &TrustedProxies) -> IpAddr {
    if !trusted.contains(peer) {
        return peer;
    }
    forwarded_for
        .into_iter()
        .flat_map(|header| header.rsplit(','))
        .filter_map(|entry| entry.trim().parse::<IpAddr>().ok())
        .find(|ip| !trusted.contains(*ip))
        .unwrap_or(peer)
}

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

/// How many joins one address may make: in any 10 minutes and in any day.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JoinRates {
    pub per_ten_minutes: usize,
    pub per_day: usize,
}

impl JoinRates {
    /// Guest joins, which mint a person each: 5 per 10 minutes and 30 per
    /// day per address unless the operator sets otherwise.
    pub const GUESTS: Self = Self {
        per_ten_minutes: 5,
        per_day: 30,
    };

    /// Spectator certificates, which mint nobody and last an hour: 30 per
    /// 10 minutes per address, enough for a visitor reloading the homepage,
    /// and no daily cap.
    pub const SPECTATORS: Self = Self {
        per_ten_minutes: 30,
        per_day: usize::MAX,
    };
}

/// Sliding windows of joins per address hash.
pub struct JoinLimits {
    short_max: usize,
    day_max: usize,
    joins: HashMap<String, VecDeque<u64>>,
}

impl Default for JoinLimits {
    fn default() -> Self {
        Self::new(JoinRates::GUESTS)
    }
}

impl JoinLimits {
    pub fn new(rates: JoinRates) -> Self {
        Self {
            short_max: rates.per_ten_minutes,
            day_max: rates.per_day,
            joins: HashMap::new(),
        }
    }

    /// Windows already holding `joins`, `(address hash, when)` in any order:
    /// the joins of the last day kept on the persistent volume, so a restart
    /// of the service does not give every address its windows back.
    pub fn restored<'a>(rates: JoinRates, joins: impl IntoIterator<Item = (&'a str, u64)>) -> Self {
        let mut limits = Self::new(rates);
        for (ip_hash, at_ms) in joins {
            limits
                .joins
                .entry(ip_hash.to_owned())
                .or_default()
                .push_back(at_ms);
        }
        for times in limits.joins.values_mut() {
            times.make_contiguous().sort_unstable();
        }
        limits
    }

    /// Takes back the join [`Self::check_and_record`] recorded for `ip_hash`
    /// at `at_ms`, for a visitor who got no answer.
    pub fn forget(&mut self, ip_hash: &str, at_ms: u64) {
        if let Some(times) = self.joins.get_mut(ip_hash)
            && let Some(at) = times.iter().rposition(|time| *time == at_ms)
        {
            times.remove(at);
        }
    }

    /// Records a join from `ip_hash` at `now_ms`, or says in how many
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

/// The seats of a capped room: the visitors the relay last counted online,
/// and a seat held for each admission answered since that the count may not
/// show yet. The relay counts every couple of seconds and a page takes a few
/// more to open its session, so without held seats every visitor of a burst
/// saw the room below its cap and was admitted; with them the visitor past
/// the cap is told at once that the room is full.
pub struct Seats {
    cap: usize,
    online: u32,
    /// When each held seat was taken, oldest first.
    held: VecDeque<u64>,
}

impl Seats {
    pub const fn new(cap: usize) -> Self {
        Self {
            cap,
            online: 0,
            held: VecDeque::new(),
        }
    }

    /// The visitors the relay last counted.
    pub const fn online(&self) -> u32 {
        self.online
    }

    /// Holds a seat at `now_ms`, or `false` when the room is full.
    pub fn take(&mut self, now_ms: u64) -> bool {
        while self
            .held
            .front()
            .is_some_and(|taken| now_ms.saturating_sub(*taken) >= SEAT_HOLD_MS)
        {
            self.held.pop_front();
        }
        let online = usize::try_from(self.online).unwrap_or(usize::MAX);
        if online.saturating_add(self.held.len()) >= self.cap {
            return false;
        }
        self.held.push_back(now_ms);
        true
    }

    /// Gives back a seat held for an admission that was not answered.
    pub fn release(&mut self) {
        self.held.pop_back();
    }

    /// The relay counted `online`: as many held seats as the count grew by
    /// are in it now. Seats of visitors who left meanwhile hide some of that
    /// growth, which only keeps seats held until they expire.
    pub fn counted(&mut self, online: u32) {
        for _ in 0..online.saturating_sub(self.online) {
            if self.held.pop_front().is_none() {
                break;
            }
        }
        self.online = online;
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "in a test this is the assertion")]
mod tests {
    use super::*;

    #[test]
    fn a_burst_past_the_cap_is_refused_before_the_count_shows_it() {
        let mut seats = Seats::new(3);
        seats.counted(1);
        assert!(seats.take(0) && seats.take(0));
        assert!(!seats.take(0), "two held seats and one online fill three");
        seats.release();
        assert!(seats.take(0), "a released seat is free again");
        // Both admitted visitors came online: their seats count once.
        seats.counted(3);
        assert!(!seats.take(1000));
        seats.counted(2);
        assert!(seats.take(1000));
    }

    #[test]
    fn a_seat_nobody_used_frees_after_its_hold() {
        let mut seats = Seats::new(1);
        assert!(seats.take(0));
        assert!(!seats.take(SEAT_HOLD_MS - 1));
        assert!(seats.take(SEAT_HOLD_MS));
    }
    use std::net::IpAddr;

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    #[test]
    fn forwarded_for_is_trusted_only_from_listed_proxies() {
        let trusted: TrustedProxies = "10.42.0.0/16".parse().unwrap();
        let header = Some("203.0.113.9, 10.42.1.1");
        assert_eq!(
            client_ip(ip("10.42.0.7"), header, &trusted),
            ip("203.0.113.9")
        );
    }

    #[test]
    fn forwarded_for_is_ignored_from_untrusted_peers() {
        let trusted: TrustedProxies = "10.42.0.0/16".parse().unwrap();
        assert_eq!(
            client_ip(ip("198.51.100.4"), Some("203.0.113.9"), &trusted),
            ip("198.51.100.4")
        );
        let nobody: TrustedProxies = "".parse().unwrap();
        assert_eq!(
            client_ip(ip("10.42.0.7"), Some("203.0.113.9"), &nobody),
            ip("10.42.0.7")
        );
    }

    #[test]
    fn a_visitor_cannot_prepend_a_fake_address_behind_the_proxy() {
        // The proxy appends the real client; anything left of it is the visitor's own text.
        let trusted: TrustedProxies = "10.42.0.0/16".parse().unwrap();
        assert_eq!(
            client_ip(ip("10.42.0.7"), Some("1.2.3.4, 203.0.113.9"), &trusted),
            ip("203.0.113.9")
        );
    }

    #[test]
    fn a_chain_through_a_cdn_and_the_cluster_proxy_names_the_visitor() {
        // Visitor -> CDN edge -> load balancer -> ingress pod: the ingress
        // appends the CDN edge, which is trusted too, so the walk from the
        // right skips it and stops at the visitor.
        let trusted: TrustedProxies = "10.42.0.0/16, 173.245.48.0/20, 2400:cb00::/32"
            .parse()
            .unwrap();
        assert_eq!(
            client_ip(
                ip("10.42.0.7"),
                Some("203.0.113.9, 173.245.48.20"),
                &trusted
            ),
            ip("203.0.113.9")
        );
        assert_eq!(
            client_ip(
                ip("10.42.0.7"),
                Some("2001:db8:5::1, 2400:cb00:1::7"),
                &trusted
            ),
            ip("2001:db8:5::1")
        );
        // A chain of nothing but proxies falls back to the peer.
        assert_eq!(
            client_ip(ip("10.42.0.7"), Some("173.245.48.20"), &trusted),
            ip("10.42.0.7")
        );
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
    fn five_joins_per_ten_minutes_and_thirty_per_day() {
        let mut limits = JoinLimits::default();
        for minute in 0..5 {
            limits.check_and_record("h", minute * 60_000).unwrap();
        }
        let retry = limits.check_and_record("h", 5 * 60_000).unwrap_err();
        assert_eq!(retry, 300, "the oldest join leaves the window at minute 10");
        limits.check_and_record("other", 5 * 60_000).unwrap();
        let mut day = JoinLimits::default();
        for index in 0..30 {
            day.check_and_record("h", index * 11 * 60_000).unwrap();
        }
        assert!(day.check_and_record("h", 30 * 11 * 60_000).is_err());
    }

    #[test]
    fn restored_windows_refuse_as_the_ones_before_a_restart_did() {
        let kept: Vec<(&str, u64)> = [4, 0, 2, 1, 3]
            .into_iter()
            .map(|minute| ("h", minute * 60_000))
            .collect();
        let mut before = JoinLimits::default();
        let mut in_order = kept.clone();
        in_order.sort_unstable_by_key(|(_, at)| *at);
        for (hash, at) in in_order {
            before.check_and_record(hash, at).unwrap();
        }
        let mut after = JoinLimits::restored(JoinRates::GUESTS, kept);
        let now = 5 * 60_000;
        assert_eq!(after.check_and_record("h", now), Err(300));
        assert_eq!(
            after.check_and_record("h", now),
            before.check_and_record("h", now)
        );
        after.check_and_record("other", now).unwrap();
    }

    #[test]
    fn a_forgotten_join_frees_its_place_in_the_window() {
        let mut limits = JoinLimits::default();
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
    fn spectators_get_thirty_per_ten_minutes_and_no_daily_cap() {
        let mut limits = JoinLimits::new(JoinRates::SPECTATORS);
        for second in 0..30 {
            limits.check_and_record("h", second * 1_000).unwrap();
        }
        assert_eq!(limits.check_and_record("h", 30_000).unwrap_err(), 570);
        let mut day = JoinLimits::new(JoinRates::SPECTATORS);
        for index in 0..100 {
            day.check_and_record("h", index * 11 * 60_000).unwrap();
        }
    }

    #[test]
    fn cidrs_are_validated() {
        assert!("10.0.0.0/33".parse::<TrustedProxies>().is_err());
        assert!("fd00::/129".parse::<TrustedProxies>().is_err());
        assert!(
            "10.0.0.1"
                .parse::<TrustedProxies>()
                .unwrap()
                .contains(ip("10.0.0.1"))
        );
        let wide: TrustedProxies = "0.0.0.0/0, fd00::/8".parse().unwrap();
        assert!(wide.contains(ip("8.8.8.8")));
        assert!(wide.contains(ip("fd12::1")));
        assert!(!wide.contains(ip("2001:db8::1")));
    }
}
