//! When the broker re-checks what it already admitted.
//!
//! Admitted native context subscribers, membership surfaces and the devices
//! a channel steward lets read were re-checked once a second, forever, so an
//! expiring grant or certificate was enforced within a second. Now a re-check
//! runs when one of its inputs announces a change
//! ([`tilo_identity_host::policy_change`]) or when the earliest expiry the
//! last check read comes due, which [`PolicySchedule::checked`]'s caller
//! registers with [`announce_at`]. Nothing runs
//! while neither happens.
//!
//! Announcements coalesce: a re-check runs at once for the first change after
//! a quiet [`RECHECK_INTERVAL`], and at most once per interval while changes
//! keep coming, so a busy desktop costs no more re-checks than the clock did
//! and no change waits longer than it did.
//!
//! A check whose inputs are not all announced (a host that does not announce,
//! a timer that cannot follow clock changes, identity files that cannot be
//! watched while the check reads the device fleet, a check that failed)
//! records itself as clocked, and the next runs [`RECHECK_INTERVAL`] later,
//! as before.

use std::time::{Duration, Instant};

/// The longest a change waits for its re-check, and the period of a clocked
/// one.
pub const RECHECK_INTERVAL: Duration = Duration::from_secs(1);

/// The last re-check and what the next waits for.
#[derive(Clone, Copy, Debug, Default)]
pub struct PolicySchedule {
    last: Option<Instant>,
    /// The announcement epoch read just before the last check read its
    /// inputs: any later change has a different one.
    epoch: u64,
    /// The last check read an input nothing announces.
    clocked: bool,
}

impl PolicySchedule {
    /// Record a check that ran at `at`, `epoch` read before it read anything.
    pub const fn checked(&mut self, at: Instant, epoch: u64, clocked: bool) {
        self.last = Some(at);
        self.epoch = epoch;
        self.clocked = clocked;
    }

    /// The epoch the last check read.
    pub const fn epoch(&self) -> u64 {
        self.epoch
    }

    /// When the next check is due, given the announcement epoch now: at once
    /// before the first, [`RECHECK_INTERVAL`] after the last while clocked
    /// or once a change arrived (but not before `now`), and `None` while
    /// nothing has changed since: an announcement or an expiry wakes the
    /// owner then.
    pub fn due(&self, now: Instant, epoch: u64) -> Option<Instant> {
        let Some(last) = self.last else {
            return Some(now);
        };
        (self.clocked || epoch != self.epoch).then(|| {
            last.checked_add(RECHECK_INTERVAL)
                .map_or(now, |next| next.max(now))
        })
    }

    /// Whether the owner waits for an announcement: checked, not clocked,
    /// and nothing changed since.
    pub const fn waits_for_change(&self, epoch: u64) -> bool {
        self.last.is_some() && !self.clocked && epoch == self.epoch
    }
}

/// The wall-clock instant (Unix nanoseconds) at which a policy clock that
/// reads `policy_now_ns` while the wall clock reads `wall_now_ns` reaches
/// `policy_at_ns`. The broker's policy clock never runs behind the wall clock
/// but may run ahead of it after the wall clock was set back.
pub const fn wall_instant(policy_at_ns: u64, policy_now_ns: u64, wall_now_ns: u64) -> u64 {
    wall_now_ns.saturating_add(policy_at_ns.saturating_sub(policy_now_ns))
}

/// Have the process's policy clock announce a change when the wall clock
/// reaches `unix_ns` ([`tilo_identity_host::policy_change::wake_at`]).
pub fn announce_at(unix_ns: u64) {
    #[cfg(test)]
    ANNOUNCE_AT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(unix_ns);
    tilo_identity_host::policy_change::wake_at(unix_ns);
}

/// Every instant [`announce_at`] was asked for in this process, in order:
/// a test that holds the wall clock sees what the broker registered, and
/// announces it itself when it moves the clock there.
#[cfg(test)]
static ANNOUNCE_AT: std::sync::Mutex<Vec<u64>> = std::sync::Mutex::new(Vec::new());

/// See [`ANNOUNCE_AT`].
#[cfg(test)]
pub fn announced_at() -> Vec<u64> {
    ANNOUNCE_AT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// The first whole Unix second at or after `at_ns`: when a check that reads
/// the clock in whole seconds first sees an instant `at_ns` as passed.
pub const fn second_at_or_after(at_ns: u64) -> u64 {
    at_ns.div_ceil(1_000_000_000)
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn secs(seconds: u64) -> Duration {
        Duration::from_secs(seconds)
    }

    fn after(start: Instant, offset: Duration) -> Instant {
        start.checked_add(offset).unwrap()
    }

    #[test]
    fn the_first_check_is_due_at_once() {
        let now = Instant::now();
        assert_eq!(PolicySchedule::default().due(now, 7), Some(now));
    }

    #[test]
    fn nothing_is_due_while_nothing_changed() {
        let start = Instant::now();
        let mut schedule = PolicySchedule::default();
        schedule.checked(start, 3, false);
        assert_eq!(schedule.due(after(start, secs(3600)), 3), None);
        assert!(schedule.waits_for_change(3));
    }

    #[test]
    fn a_change_after_a_quiet_interval_is_checked_at_once() {
        let start = Instant::now();
        let mut schedule = PolicySchedule::default();
        schedule.checked(start, 3, false);
        let later = after(start, secs(30));
        assert_eq!(schedule.due(later, 4), Some(later));
        assert!(!schedule.waits_for_change(4));
    }

    #[test]
    fn changes_in_quick_succession_wait_out_the_interval() {
        let start = Instant::now();
        let mut schedule = PolicySchedule::default();
        schedule.checked(start, 3, false);
        let soon = after(start, Duration::from_millis(200));
        assert_eq!(schedule.due(soon, 4), Some(after(start, RECHECK_INTERVAL)));
    }

    #[test]
    fn a_clocked_check_recurs_every_interval_without_a_change() {
        let start = Instant::now();
        let mut schedule = PolicySchedule::default();
        schedule.checked(start, 3, true);
        assert_eq!(schedule.due(start, 3), Some(after(start, RECHECK_INTERVAL)));
        assert!(!schedule.waits_for_change(3));
    }

    #[test]
    fn policy_instants_map_onto_the_wall_clock() {
        // Policy and wall agree: the instant is the same.
        assert_eq!(wall_instant(150, 100, 100), 150);
        // The wall clock was set back by 40: it reaches the instant later.
        assert_eq!(wall_instant(150, 100, 60), 110);
        // Already passed: now.
        assert_eq!(wall_instant(90, 100, 100), 100);
    }

    #[test]
    fn instants_round_up_to_the_second_a_check_sees_them_in() {
        assert_eq!(second_at_or_after(5_000_000_000), 5);
        assert_eq!(second_at_or_after(5_000_000_001), 6);
        assert_eq!(second_at_or_after(0), 0);
    }
}
