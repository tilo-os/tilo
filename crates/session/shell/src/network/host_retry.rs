//! When the network actor next asks a workspace host for a room.
//!
//! A host that cannot be reached, or that drops a session, is asked again
//! after an exponentially growing, jittered delay, capped so a host that comes
//! back is found within a bounded time. A host that refuses the room for good
//! — it does not carry it, or this device is not a member — is not asked
//! again until something that could change its answer changes here: the
//! host's configuration, an invitation to redeem, or the room's membership.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use iroh::EndpointId;
use tilo_relay_protocol::transport::Refusal;

use super::actor::WorkspaceId;

/// How many base delays the longest wait between attempts is.
const CAP_FACTOR: u32 = 24;

/// Why a room is not being asked for at a host right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Wait {
    /// Asked again once `until` has passed.
    Until(Instant),
    /// Not asked again until [`HostRetries::reconsider_room`] or
    /// [`HostRetries::reconsider_host`] says something changed.
    Parked,
}

#[derive(Clone, Copy, Debug)]
struct Retry {
    failures: u32,
    wait: Wait,
}

/// Each `(room, host)` pair's back-off.
#[derive(Debug)]
pub(super) struct HostRetries {
    base: Duration,
    retries: HashMap<(WorkspaceId, EndpointId), Retry>,
}

impl HostRetries {
    /// Back-off starting at `base` (the actor's retry cadence) and capped at
    /// [`CAP_FACTOR`] times it.
    pub(super) fn new(base: Duration) -> Self {
        Self {
            base,
            retries: HashMap::new(),
        }
    }

    /// Whether `host` may be asked for `ws` at `now`.
    pub(super) fn due(&self, ws: WorkspaceId, host: EndpointId, now: Instant) -> bool {
        match self.retries.get(&(ws, host)).map(|retry| retry.wait) {
            None => true,
            Some(Wait::Until(until)) => now >= until,
            Some(Wait::Parked) => false,
        }
    }

    /// The host subscribed this device: the next failure starts over.
    pub(super) fn succeeded(&mut self, ws: WorkspaceId, host: EndpointId) {
        self.retries.remove(&(ws, host));
    }

    /// The connection failed or closed. `entropy` spreads the retry across
    /// the upper half of the delay, so devices that lost a host together do
    /// not all come back in the same instant. Returns the wait, or `None`
    /// when the pair stays parked by an earlier final refusal.
    pub(super) fn failed(
        &mut self,
        ws: WorkspaceId,
        host: EndpointId,
        now: Instant,
        entropy: u64,
    ) -> Option<Duration> {
        let retry = self.retries.entry((ws, host)).or_insert(Retry {
            failures: 0,
            wait: Wait::Until(now),
        });
        if retry.wait == Wait::Parked {
            return None;
        }
        retry.failures = retry.failures.saturating_add(1);
        let delay = jittered(backoff(self.base, retry.failures), entropy);
        retry.wait = Wait::Until(now.checked_add(delay).unwrap_or(now));
        Some(delay)
    }

    /// The host refused the room. A final refusal parks the pair and
    /// returns `true`; a passing one changes nothing here, as the session's
    /// close that follows every refusal counts as the failure.
    pub(super) fn refused(&mut self, ws: WorkspaceId, host: EndpointId, refusal: Refusal) -> bool {
        if !refusal.is_final() {
            return false;
        }
        let retry = self.retries.entry((ws, host)).or_insert(Retry {
            failures: 0,
            wait: Wait::Parked,
        });
        retry.failures = retry.failures.saturating_add(1);
        retry.wait = Wait::Parked;
        true
    }

    /// Something about `ws` changed that a host may answer differently: ask
    /// every host again, at once.
    pub(super) fn reconsider_room(&mut self, ws: WorkspaceId) {
        self.retries.retain(|(room, _), _| *room != ws);
    }

    /// Something about `host` changed: ask it again for every room, at once.
    pub(super) fn reconsider_host(&mut self, host: EndpointId) {
        self.retries.retain(|(_, known), _| *known != host);
    }

    /// Whether `host` refused `ws` for good and is not being asked.
    #[cfg(test)]
    pub(super) fn parked(&self, ws: WorkspaceId, host: EndpointId) -> bool {
        self.retries
            .get(&(ws, host))
            .is_some_and(|retry| retry.wait == Wait::Parked)
    }
}

/// `base` doubled for every failure after the first, capped.
fn backoff(base: Duration, failures: u32) -> Duration {
    let cap = base.saturating_mul(CAP_FACTOR);
    let doublings = failures.saturating_sub(1).min(16);
    base.saturating_mul(1_u32 << doublings).min(cap)
}

/// A point in `[delay / 2, delay]` chosen by `entropy`.
fn jittered(delay: Duration, entropy: u64) -> Duration {
    let half = delay.checked_div(2).unwrap_or_default();
    let span = u128::from(u64::try_from(half.as_nanos()).unwrap_or(u64::MAX));
    // Scaled rather than wrapped, so the whole range of `entropy` spans it.
    let offset = u128::from(entropy)
        .saturating_mul(span)
        .checked_div(u128::from(u64::MAX))
        .unwrap_or_default();
    half.saturating_add(Duration::from_nanos(
        u64::try_from(offset).unwrap_or(u64::MAX),
    ))
}

/// Fresh entropy for [`HostRetries::failed`].
pub(super) fn entropy() -> u64 {
    let mut bytes = [0_u8; 8];
    // A failed draw only removes the spread, never the back-off itself.
    let _ = getrandom::fill(&mut bytes);
    u64::from_le_bytes(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(byte: u8) -> EndpointId {
        iroh::SecretKey::from_bytes(&[byte; 32]).public()
    }

    const BASE: Duration = Duration::from_secs(5);

    #[test]
    fn failures_back_off_exponentially_up_to_a_cap() {
        let mut retries = HostRetries::new(BASE);
        let now = Instant::now();
        // The top of every jitter range: the undithered delay.
        let waits: Vec<_> = (0..10)
            .map(|_| retries.failed(1, host(1), now, u64::MAX).unwrap())
            .collect();
        let at_most = |wait: Duration, delay: Duration| wait <= delay && wait >= delay / 2;
        assert!(at_most(waits[0], BASE));
        assert!(at_most(waits[1], BASE * 2));
        assert!(at_most(waits[2], BASE * 4));
        assert!(at_most(waits[3], BASE * 8));
        for wait in &waits[5..] {
            assert!(at_most(*wait, BASE * CAP_FACTOR), "capped: {wait:?}");
        }
        assert!(
            waits.windows(2).all(|pair| pair[1] >= pair[0] / 2),
            "never collapses back to the first delay while still failing: {waits:?}"
        );
    }

    #[test]
    fn jitter_stays_in_the_upper_half_of_the_delay() {
        let delay = Duration::from_secs(8);
        for entropy in [0, 1, 7, u64::MAX / 3, u64::MAX] {
            let wait = jittered(delay, entropy);
            assert!(wait >= delay / 2 && wait <= delay, "{wait:?}");
        }
        assert_ne!(jittered(delay, 0), jittered(delay, u64::MAX / 2));
        assert_eq!(jittered(delay, u64::MAX), delay);
        assert_eq!(jittered(delay, 0), delay / 2);
    }

    #[test]
    fn a_failed_host_is_asked_again_only_once_its_wait_has_passed() {
        let mut retries = HostRetries::new(BASE);
        let now = Instant::now();
        assert!(retries.due(1, host(1), now), "a host never tried is due");
        let wait = retries.failed(1, host(1), now, 0).unwrap();
        assert!(!retries.due(1, host(1), now));
        assert!(retries.due(1, host(1), now + wait));
        assert!(
            retries.due(2, host(1), now),
            "another room is not held back"
        );
        assert!(retries.due(1, host(2), now), "nor another host");

        retries.succeeded(1, host(1));
        assert_eq!(
            retries.failed(1, host(1), now, u64::MAX),
            Some(BASE),
            "a success starts the back-off over"
        );
    }

    #[test]
    fn a_final_refusal_parks_the_room_until_something_changes() {
        let mut retries = HostRetries::new(BASE);
        let now = Instant::now();
        for refusal in [Refusal::NotHosted, Refusal::NotMember] {
            assert!(retries.refused(1, host(1), refusal));
            assert!(retries.parked(1, host(1)));
            let much_later = now + BASE * CAP_FACTOR * 100;
            assert!(
                !retries.due(1, host(1), much_later),
                "{refusal:?} stays parked"
            );
            assert_eq!(
                retries.failed(1, host(1), much_later, 0),
                None,
                "the session closing after the refusal does not unpark it"
            );

            retries.reconsider_room(1);
            assert!(
                retries.due(1, host(1), now),
                "a membership change asks again"
            );
            retries.refused(1, host(1), refusal);
            retries.reconsider_host(host(1));
            assert!(retries.due(1, host(1), now), "a host change asks again");
        }
    }

    #[test]
    fn a_passing_refusal_backs_off_instead_of_parking() {
        let mut retries = HostRetries::new(BASE);
        let now = Instant::now();
        assert!(!retries.refused(1, host(1), Refusal::Busy));
        // The close that follows the refusal.
        let wait = retries.failed(1, host(1), now, 0).unwrap();
        assert!(!retries.parked(1, host(1)));
        assert!(!retries.due(1, host(1), now));
        assert!(retries.due(1, host(1), now + wait));
    }
}
