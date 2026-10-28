//! The lock follows the person across their own devices. Each device tells
//! the others, in its fabric presence, whether it is locked, when the person
//! last locked or unlocked it themselves and when they last used it
//! ([`OwnerSession`]). From those:
//!
//! - Locking a device locks the person's other devices, unless they used one
//!   after that.
//! - Unlocking a device unlocks the others on the same network that locked
//!   only because it did, if they were in use when they locked; one left idle
//!   or away stays locked.
//! - A device does not lock for idleness while the person uses another of
//!   their devices on the same network: its idle time counts from their last
//!   input there.
//!
//! The idle timer's lock (and the lock before an idle sleep) stays on its
//! device: it neither spreads nor is undone by another device, so a device set
//! never to lock on its own is not locked by another one going idle. A lock
//! followed from another device does not spread further either. Unlocking always takes the person's credential on one of
//! their devices.

use super::device_fabric::{DevicePresence, OWNER_SESSION_ACTIVE_STEP_MS, OwnerSession};

/// What this device does to follow the person's other devices.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Follow {
    Lock,
    Unlock,
}

/// This device's side of [`OwnerSession`]: what it publishes and what it has
/// already followed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SessionFollow {
    /// When the person last locked or unlocked this device themselves.
    changed_unix_ms: u64,
    /// The latest change on another device this device has acted on, so an
    /// old lock elsewhere is not followed twice.
    acted_unix_ms: u64,
    /// Locked because another device was locked at this time, and whether
    /// this device was in use then.
    followed: Option<Followed>,
    /// Whether the lock this device has, if any, is the person's own here
    /// and not the idle timer's or another device's.
    locked_by_person: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Followed {
    since_unix_ms: u64,
    in_use: bool,
}

impl SessionFollow {
    /// The person locked or unlocked this device themselves.
    pub fn changed_by_person(&mut self, now_unix_ms: u64) {
        self.changed_unix_ms = now_unix_ms;
        self.followed = None;
        self.locked_by_person = true;
    }

    /// The idle timer locked this device: no other device's unlock undoes it,
    /// and no other device follows it.
    pub fn locked_by_idle(&mut self) {
        self.followed = None;
        self.locked_by_person = false;
    }

    /// What this device publishes, given its lock and the person's last input.
    /// A lock that is not the person's own here publishes no change for the
    /// others to follow.
    #[must_use]
    pub fn presence(&self, locked: bool, input_unix_ms: u64) -> OwnerSession {
        OwnerSession {
            locked,
            changed_unix_ms: if locked && !self.locked_by_person {
                0
            } else {
                self.changed_unix_ms
            },
            active_unix_ms: input_unix_ms.saturating_sub(
                input_unix_ms
                    .checked_rem(OWNER_SESSION_ACTIVE_STEP_MS)
                    .unwrap_or(0),
            ),
        }
    }

    /// Follow the person's other devices' sessions, each with whether it
    /// shares a network with this one: lock after a newer lock elsewhere this
    /// device was not used since, unlock when a device nearby is unlocked
    /// after the lock it followed and this one was in use when it locked.
    pub fn decide(
        &mut self,
        locked: bool,
        input_unix_ms: u64,
        in_use: bool,
        others: impl IntoIterator<Item = (OwnerSession, bool)>,
    ) -> Option<Follow> {
        let others = others.into_iter().collect::<Vec<_>>();
        let latest = |want_locked: bool, nearby_only: bool| {
            others
                .iter()
                .filter(|(session, nearby)| {
                    session.locked == want_locked && (*nearby || !nearby_only)
                })
                .map(|(session, _)| session.changed_unix_ms)
                .max()
        };
        if !locked {
            let locked_at = latest(true, false)?;
            if locked_at
                <= self
                    .changed_unix_ms
                    .max(self.acted_unix_ms)
                    .max(input_unix_ms)
            {
                return None;
            }
            self.acted_unix_ms = locked_at;
            self.followed = Some(Followed {
                since_unix_ms: locked_at,
                in_use,
            });
            self.locked_by_person = false;
            return Some(Follow::Lock);
        }
        let followed = self.followed?;
        let unlocked_at = latest(false, true)?;
        if unlocked_at <= followed.since_unix_ms {
            return None;
        }
        self.acted_unix_ms = unlocked_at;
        self.followed = None;
        followed.in_use.then_some(Follow::Unlock)
    }
}

/// The person's latest input on another of their devices that shares a
/// network with this one and is unlocked; idle time here counts from it.
#[must_use]
pub fn latest_use_nearby<'a>(
    own: &DevicePresence,
    others: impl IntoIterator<Item = &'a DevicePresence>,
) -> Option<u64> {
    others
        .into_iter()
        .filter(|other| other.online && !other.session.locked && shares_network(own, other))
        .map(|other| other.session.active_unix_ms)
        .max()
}

/// Whether two devices were last seen on a common IPv4 subnet.
#[must_use]
pub fn shares_network(own: &DevicePresence, other: &DevicePresence) -> bool {
    own.wake_routes
        .iter()
        .filter_map(|route| route.subnet)
        .any(|subnet| {
            other
                .wake_routes
                .iter()
                .any(|route| route.subnet == Some(subnet))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A device on the same network.
    fn session(locked: bool, changed_unix_ms: u64) -> (OwnerSession, bool) {
        let session = OwnerSession {
            locked,
            changed_unix_ms,
            active_unix_ms: 0,
        };
        (session, true)
    }

    #[test]
    fn a_lock_elsewhere_locks_a_device_not_used_since_once() {
        let mut follow = SessionFollow::default();
        let laptop = session(true, 2_000);
        assert_eq!(
            follow.decide(false, 1_000, true, [laptop]),
            Some(Follow::Lock)
        );
        // Unlocked here by the person afterwards: the same lock is not followed again.
        follow.changed_by_person(3_000);
        assert_eq!(follow.decide(false, 1_000, true, [laptop]), None);
    }

    #[test]
    fn a_device_used_after_the_lock_elsewhere_stays_unlocked() {
        let mut follow = SessionFollow::default();
        assert_eq!(
            follow.decide(false, 2_500, true, [session(true, 2_000)]),
            None
        );
    }

    #[test]
    fn an_unlock_elsewhere_undoes_only_a_followed_lock_of_a_device_in_use() {
        let mut follow = SessionFollow::default();
        assert_eq!(
            follow.decide(false, 1_000, true, [session(true, 2_000)]),
            Some(Follow::Lock)
        );
        assert_eq!(
            follow.decide(true, 1_000, true, [session(true, 2_000)]),
            None
        );
        assert_eq!(
            follow.decide(true, 1_000, true, [session(false, 3_000)]),
            Some(Follow::Unlock)
        );

        // Idle when it followed the lock: the unlock elsewhere leaves it locked.
        let mut idle = SessionFollow::default();
        idle.decide(false, 1_000, false, [session(true, 2_000)]);
        assert_eq!(
            idle.decide(true, 1_000, false, [session(false, 3_000)]),
            None
        );

        // Locked by its own idle timer or by the person here: never unlocked from elsewhere.
        let mut own = SessionFollow::default();
        own.locked_by_idle();
        assert_eq!(own.decide(true, 1_000, true, [session(false, 3_000)]), None);
        own.changed_by_person(1_500);
        assert_eq!(own.decide(true, 1_000, true, [session(false, 3_000)]), None);
    }

    #[test]
    fn an_unlock_on_another_network_leaves_a_followed_lock() {
        let mut follow = SessionFollow::default();
        let (away_lock, _) = session(true, 2_000);
        assert_eq!(
            follow.decide(false, 1_000, true, [(away_lock, false)]),
            Some(Follow::Lock)
        );
        let (away_unlock, _) = session(false, 3_000);
        assert_eq!(
            follow.decide(true, 1_000, true, [(away_unlock, false)]),
            None
        );
    }

    #[test]
    fn an_older_lock_elsewhere_does_not_relock_after_an_unlock_was_followed() {
        let mut follow = SessionFollow::default();
        let earlier = session(true, 1_500);
        follow.decide(false, 1_000, true, [session(true, 2_000), earlier]);
        assert_eq!(
            follow.decide(true, 1_000, true, [session(false, 3_000), earlier]),
            Some(Follow::Unlock)
        );
        assert_eq!(
            follow.decide(false, 1_000, true, [session(false, 3_000), earlier]),
            None
        );
    }

    /// What `device` publishes, as seen on the same network.
    fn published(device: &SessionFollow, locked: bool, input_unix_ms: u64) -> (OwnerSession, bool) {
        (device.presence(locked, input_unix_ms), true)
    }

    #[test]
    fn an_idle_lock_elsewhere_never_locks_a_device_set_never_to_lock() {
        // The person unlocks the desktop after last touching the laptop, then
        // leaves both; the desktop's idle timer locks it (or its idle sleep
        // does, which counts as the idle timer's lock).
        let mut desktop = SessionFollow::default();
        desktop.changed_by_person(2_000);
        desktop.locked_by_idle();
        let mut laptop = SessionFollow::default();
        assert_eq!(
            laptop.decide(false, 1_000, false, [published(&desktop, true, 2_000)]),
            None,
            "the desktop's idle lock is not followed"
        );

        // The person's own lock there still locks the laptop.
        desktop.changed_by_person(3_000);
        assert_eq!(
            laptop.decide(false, 1_000, false, [published(&desktop, true, 3_000)]),
            Some(Follow::Lock)
        );
    }

    #[test]
    fn a_followed_lock_does_not_spread_further() {
        let mut desktop = SessionFollow::default();
        desktop.changed_by_person(500);
        assert_eq!(
            desktop.decide(false, 400, false, [session(true, 2_000)]),
            Some(Follow::Lock)
        );
        // The tablet, last used before the desktop's last unlock, sees only
        // the desktop: its lock is not the person's, nothing to follow.
        let mut tablet = SessionFollow::default();
        assert_eq!(
            tablet.decide(false, 300, false, [published(&desktop, true, 400)]),
            None
        );
    }

    #[test]
    fn the_published_input_moves_in_steps() {
        let follow = SessionFollow::default();
        let first = follow.presence(false, 20_001);
        assert_eq!(first, follow.presence(false, 29_999));
        assert_eq!(first.active_unix_ms, 20_000);
    }
}
