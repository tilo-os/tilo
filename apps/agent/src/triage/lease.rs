//! One triage session per room, however many team devices run the agent.
//!
//! Every team device keeps the triage agent running where it may operate it
//! (`shell::triage_runtime`), so two devices signed in as the team would
//! otherwise both answer each report. The lease is the agent's own room
//! presence: a session that triages a room says so in its presence status in
//! that room, `triage-lease:<device>:<since>`, and restates it every
//! [`Timing::renew`]. A presence status is short, so the device is its first
//! eight bytes, as hex.
//!
//! - A session that has just started first watches the room for
//!   [`Timing::observe`]. If a live claim from another device is there, it
//!   yields and ends; the desktop tries again later.
//! - Otherwise it claims, and triages only once its claim has stood for
//!   [`Timing::settle`] without an earlier one appearing.
//! - The earliest live claim holds the room. Between claims made in the same
//!   moment, the lowest device id holds it. A session that sees a claim ahead
//!   of its own yields.
//! - A claim is live while it was restated within [`Timing::ttl`]. When the
//!   holder's device goes away its claim expires, and the next session a
//!   desktop starts takes the room over.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Where a claim's text starts.
pub const PREFIX: &str = "triage-lease:";

/// How the lease is timed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timing {
    /// How long a claim stays live without being restated.
    pub ttl: Duration,
    /// How often the holder restates its claim.
    pub renew: Duration,
    /// How long a new session watches before it claims.
    pub observe: Duration,
    /// How long a claim must stand before its session triages.
    pub settle: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            ttl: Duration::from_secs(20),
            renew: Duration::from_secs(5),
            observe: Duration::from_secs(12),
            settle: Duration::from_secs(3),
        }
    }
}

/// How much of a device id a claim names, in hex digits.
pub const DEVICE_DIGITS: usize = 16;

/// One session's claim on the room its presence is in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Claim {
    /// The claiming device, as lowercase hex ([`DEVICE_DIGITS`] of it).
    pub device: String,
    /// When it claimed, in Unix milliseconds.
    pub since: u64,
}

impl Claim {
    /// The presence status that states this claim.
    #[must_use]
    pub fn text(&self) -> String {
        format!("{PREFIX}{}:{}", self.device, self.since)
    }

    /// A claim stated by `text`, if it is one.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let (device, since) = text.strip_prefix(PREFIX)?.split_once(':')?;
        if device.is_empty() || !device.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        Some(Self {
            device: device.to_string(),
            since: since.parse().ok()?,
        })
    }

    /// Whether this claim is ahead of `other`: earlier, or as early from a
    /// lower device.
    fn ahead_of(&self, other: &Self) -> bool {
        (self.since, &self.device) < (other.since, &other.device)
    }
}

/// What the session should do now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Watch, and do not triage yet.
    Wait,
    /// Triage: this session holds the room.
    Hold,
    /// Another device holds the room: end this session.
    Yield,
}

/// A session's side of the lease on one room.
#[derive(Debug)]
pub struct Lease {
    device: String,
    timing: Timing,
    started: Instant,
    claim: Option<(Claim, Instant)>,
    stated: Option<Instant>,
}

/// Now, in Unix milliseconds.
#[must_use]
pub fn unix_ms(now: SystemTime) -> u64 {
    now.duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| u64::try_from(elapsed.as_millis()).ok())
        .unwrap_or(0)
}

impl Lease {
    #[must_use]
    pub fn new(device: &str, timing: Timing, now: Instant) -> Self {
        Self {
            device: device.chars().take(DEVICE_DIGITS).collect(),
            timing,
            started: now,
            claim: None,
            stated: None,
        }
    }

    /// Decide from the claims heard in the room, each with how long ago it
    /// was last heard. This session's own claim, if heard back, is ignored.
    pub fn decide(&mut self, heard: &[(Claim, Duration)], now: Instant, now_ms: u64) -> Verdict {
        let rivals: Vec<&Claim> = heard
            .iter()
            .filter(|(claim, age)| claim.device != self.device && *age <= self.timing.ttl)
            .map(|(claim, _)| claim)
            .collect();
        match &self.claim {
            None => {
                if !rivals.is_empty() {
                    return Verdict::Yield;
                }
                if now.duration_since(self.started) < self.timing.observe {
                    return Verdict::Wait;
                }
                self.claim = Some((
                    Claim {
                        device: self.device.clone(),
                        since: now_ms,
                    },
                    now,
                ));
                Verdict::Wait
            }
            Some((mine, claimed)) => {
                if rivals.iter().any(|rival| rival.ahead_of(mine)) {
                    return Verdict::Yield;
                }
                if now.duration_since(*claimed) < self.timing.settle {
                    Verdict::Wait
                } else {
                    Verdict::Hold
                }
            }
        }
    }

    /// The claim to state now, when this session has one and it is due:
    /// just made, or not restated for [`Timing::renew`].
    pub fn due(&mut self, now: Instant) -> Option<String> {
        let (claim, _) = self.claim.as_ref()?;
        if self
            .stated
            .is_some_and(|stated| now.duration_since(stated) < self.timing.renew)
        {
            return None;
        }
        self.stated = Some(now);
        Some(claim.text())
    }

    /// Whether this session has claimed the room.
    #[must_use]
    pub fn claimed(&self) -> bool {
        self.claim.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TIMING: Timing = Timing {
        ttl: Duration::from_secs(20),
        renew: Duration::from_secs(5),
        observe: Duration::from_secs(12),
        settle: Duration::from_secs(3),
    };

    /// Two devices, each with a session: `a` (the lower id) and `b`. What
    /// each heard from the other is what the other last stated.
    struct Pair {
        a: Lease,
        b: Lease,
        said_a: Option<(Claim, Instant)>,
        said_b: Option<(Claim, Instant)>,
    }

    impl Pair {
        fn step(&mut self, now: Instant, now_ms: u64) -> (Verdict, Verdict) {
            let heard = |said: &Option<(Claim, Instant)>| {
                said.iter()
                    .map(|(claim, at)| (claim.clone(), now.duration_since(*at)))
                    .collect::<Vec<_>>()
            };
            let va = self.a.decide(&heard(&self.said_b), now, now_ms);
            let vb = self.b.decide(&heard(&self.said_a), now, now_ms);
            if va != Verdict::Yield
                && let Some(text) = self.a.due(now)
            {
                self.said_a = Some((Claim::parse(&text).unwrap(), now));
            }
            if vb != Verdict::Yield
                && let Some(text) = self.b.due(now)
            {
                self.said_b = Some((Claim::parse(&text).unwrap(), now));
            }
            (va, vb)
        }
    }

    #[test]
    fn two_devices_starting_together_leave_one_holder_and_the_other_takes_over_later() {
        let t0 = Instant::now();
        let mut pair = Pair {
            a: Lease::new("0aaa", TIMING, t0),
            b: Lease::new("0bbb", TIMING, t0),
            said_a: None,
            said_b: None,
        };
        // Both watch, both claim in the same instant; the lower device holds.
        let mut last = (Verdict::Wait, Verdict::Wait);
        for second in 0..=20 {
            last = pair.step(t0 + Duration::from_secs(second), 1_000);
            if last.1 == Verdict::Yield {
                break;
            }
        }
        assert_eq!(last.1, Verdict::Yield, "the higher device yields");
        let (va, _) = pair.step(t0 + Duration::from_secs(21), 1_000);
        assert_eq!(va, Verdict::Hold, "the lower device holds");

        // A fresh session on b while a holds: it yields without claiming.
        let t1 = t0 + Duration::from_secs(22);
        pair.b = Lease::new("0bbb", TIMING, t1);
        pair.said_b = None;
        let (_, vb) = pair.step(t1, 22_000);
        assert_eq!(vb, Verdict::Yield);
        assert!(!pair.b.claimed());

        // a's device goes away: its claim expires, and b's next session
        // takes the room over.
        let (claim, at) = pair.said_a.clone().unwrap();
        let t2 = at + TIMING.ttl + Duration::from_secs(1);
        let mut b = Lease::new("0bbb", TIMING, t2);
        let heard = vec![(claim, t2.duration_since(at))];
        assert_eq!(b.decide(&heard, t2, 60_000), Verdict::Wait);
        let t3 = t2 + TIMING.observe;
        assert_eq!(b.decide(&heard, t3, 72_000), Verdict::Wait, "claims");
        assert!(b.claimed());
        assert_eq!(
            b.decide(&heard, t3 + TIMING.settle, 75_000),
            Verdict::Hold,
            "the expired holder no longer counts"
        );
    }

    #[test]
    fn a_claim_round_trips_and_a_later_rival_is_behind() {
        let claim = Claim {
            device: "ab12".into(),
            since: 42,
        };
        assert_eq!(Claim::parse(&claim.text()), Some(claim.clone()));
        let long = Claim {
            device: "f".repeat(DEVICE_DIGITS),
            since: u64::MAX,
        };
        assert!(
            long.text().chars().count() <= 96,
            "a claim fits a presence status"
        );
        assert_eq!(Claim::parse("Sorting reports"), None);
        let later = Claim {
            since: 43,
            ..claim.clone()
        };
        assert!(claim.ahead_of(&later));
        assert!(
            Claim {
                device: "0001".into(),
                since: claim.since,
            }
            .ahead_of(&claim),
            "ties go to the lower device"
        );
    }
}
