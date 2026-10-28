//! When the shovel asks the identity service for its routes' channel keys:
//! the signals that ask, the rounds they start, and the backoff between
//! rounds while nothing tells a waiting route about a change.

use crate::time::Duration;
use std::pin::Pin;

use super::super::route_keys::RouteKeys;

/// How soon a route waiting for keys is asked about again after anything
/// changed, and how often every route is asked when the broker does not say
/// when keys change (`channelKeysChanged`).
const CHANNEL_KEY_REFRESH: Duration = Duration::from_secs(1);

/// The longest a route waiting for keys goes unasked while nothing changes,
/// when nothing tells it about a change: an identity service that refused or
/// could not take its question, or a removal whose rejoin it asks for.
/// Admission, a channel service starting after the app and a steward this
/// device learns of later are all pushed (see `KeySchedule`).
const CHANNEL_KEY_WAIT_LONGEST: Duration = Duration::from_secs(30);

/// What makes an endpoint ask the identity service about a route's keys.
pub(super) enum KeySignal {
    /// The route's channel changed, or ciphertext arrived its keys can't
    /// open: ask for the route's keys now.
    Changed(String),
    /// The broker does not say when keys change: ask on a timer.
    Unpushed,
}

/// Which routes one round of key questions asks about. Only routes that ask
/// the identity service at all are ever asked.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct KeyRound {
    /// Every such route.
    pub(super) all: bool,
    /// Routes holding no keys yet.
    pub(super) waiting: bool,
    /// These routes, by channel key.
    pub(super) routes: std::collections::BTreeSet<String>,
}

impl KeyRound {
    pub(super) fn asks(&self, route: &str, keys: &RouteKeys) -> bool {
        keys.asks_identity_service()
            && (self.all || (self.waiting && keys.awaiting_keys()) || self.routes.contains(route))
    }
}

/// When an endpoint asks the identity service for its routes' keys.
///
/// A route holding keys is asked only when told to: the broker resolves the
/// route's pending `channelKeysChanged` the moment its channel's keys or
/// this device's access change, ciphertext arrives its keys can't open, or
/// the broker refuses an edit sealed under them (`send_document_frame`). An
/// endpoint whose routes all hold keys therefore never wakes for them.
///
/// Every channel change the identity service would answer differently is a
/// commit the channel's steward makes within its reconcile interval (about a
/// second) of the policy change: a key rotation, an invite, a removal, a
/// role change that ends reading, a device's enrolment or revocation. The
/// endpoint asks the moment that commit reaches this device, so a removed or
/// revoked route stops holding its keys within that bound plus the commit's
/// delivery to this device. Independently, the broker refuses content sealed
/// under a superseded epoch on every path, and the identity service checks
/// this device's trust and the person's role on every signature, so a stale
/// key never publishes past the commit.
///
/// A route the channel service answered as waiting or unavailable is told
/// as well when that answer may change: the service follows a member's
/// access request to the steward's welcome, re-checks a room with no known
/// steward whenever the workspace tree announces a change of its authority
/// or governance, and retries a channel it failed to open on its own
/// backoff, resolving the route's `channelKeysChanged` each time. So it is
/// not asked on a timer either.
///
/// A route waiting for keys is asked on a timer only when nothing will tell
/// it: its last question got no answer from the channel service (the
/// identity service refused it or could not be reached, or it was never
/// answered yet), or the service answered that this device was removed (a
/// rejoin is asked for again on each question). It is asked after
/// [`CHANNEL_KEY_REFRESH`], then backing off to [`CHANNEL_KEY_WAIT_LONGEST`]
/// while nothing changes. An endpoint whose broker pushes nothing asks for
/// every route every [`CHANNEL_KEY_REFRESH`], as before.
pub(super) struct KeySchedule {
    /// Whether the broker says when keys change. Assumed until a route's
    /// `channelKeysChanged` is refused.
    pushed: bool,
    /// Routes told to ask since the last round.
    pub(super) marked: std::collections::BTreeSet<String>,
    /// The timer's next round, and the wait it was armed with.
    pub(super) timer: Pin<Box<tokio::time::Sleep>>,
    wait: Duration,
}

impl KeySchedule {
    pub(super) fn new() -> Self {
        Self {
            pushed: true,
            marked: std::collections::BTreeSet::new(),
            timer: Box::pin(tokio::time::sleep(CHANNEL_KEY_REFRESH)),
            wait: CHANNEL_KEY_REFRESH,
        }
    }

    /// Whether the timer starts rounds: never while the broker pushes
    /// changes and no route waits for keys that nothing will tell it about
    /// (`unpushed_wait`, see [`RouteKeys::asks_on_timer`]).
    pub(super) fn timed(&self, unpushed_wait: bool) -> bool {
        !self.pushed || unpushed_wait
    }

    pub(super) fn note(&mut self, signal: KeySignal) {
        match signal {
            KeySignal::Changed(route) => {
                self.marked.insert(route);
                self.restart();
            }
            KeySignal::Unpushed if self.pushed => {
                self.pushed = false;
                self.restart();
            }
            KeySignal::Unpushed => {}
        }
    }

    /// The round the timer starts now that it fired; arms the next one.
    pub(super) fn timer_round(&mut self) -> KeyRound {
        if self.pushed {
            self.wait = self.wait.saturating_mul(2).min(CHANNEL_KEY_WAIT_LONGEST);
        }
        self.arm();
        KeyRound {
            all: !self.pushed,
            waiting: true,
            routes: std::mem::take(&mut self.marked),
        }
    }

    /// A round for the routes told to ask since the last one, if any.
    pub(super) fn marked_round(&mut self) -> Option<KeyRound> {
        (!self.marked.is_empty()).then(|| KeyRound {
            all: false,
            waiting: false,
            routes: std::mem::take(&mut self.marked),
        })
    }

    /// The round a route that just joined starts: it learns its keys, or
    /// that it waits, before its first edit.
    pub(super) fn join_round(&mut self) -> KeyRound {
        self.restart();
        KeyRound {
            all: !self.pushed,
            waiting: true,
            routes: std::mem::take(&mut self.marked),
        }
    }

    /// Something changed (a route joined, keys or access moved): a route
    /// still waiting is asked again soon.
    pub(super) fn restart(&mut self) {
        self.wait = CHANNEL_KEY_REFRESH;
        self.arm();
    }

    fn arm(&mut self) {
        self.timer.set(tokio::time::sleep(self.wait));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The timer only covers routes still waiting for keys while the broker
    /// pushes changes, backing off while nothing changes; a change asks at
    /// once and starts the backoff over. Without pushes, every route is
    /// asked every second.
    #[test]
    fn key_schedule_polls_only_what_is_not_pushed() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let _entered = runtime.enter();
        let mut schedule = KeySchedule::new();
        assert!(
            !schedule.timed(false),
            "routes holding keys are never polled while changes are pushed"
        );
        assert!(schedule.timed(true), "a waiting route is polled");
        let mut waits = vec![schedule.wait];
        for _ in 0..6 {
            let round = schedule.timer_round();
            assert!(round.waiting && !round.all, "{round:?}");
            waits.push(schedule.wait);
        }
        assert_eq!(
            waits,
            [1, 2, 4, 8, 16, 30, 30].map(Duration::from_secs),
            "waiting routes back off to the longest wait"
        );

        schedule.note(KeySignal::Changed("route".into()));
        assert_eq!(schedule.wait, CHANNEL_KEY_REFRESH, "a change starts over");
        let round = schedule.marked_round().expect("the changed route asks now");
        assert!(!round.all && !round.waiting);
        assert_eq!(round.routes.into_iter().collect::<Vec<_>>(), ["route"]);
        assert_eq!(schedule.marked_round(), None, "asked once");

        schedule.note(KeySignal::Unpushed);
        assert!(
            schedule.timed(false),
            "without pushes, every route is polled"
        );
        for _ in 0..3 {
            assert!(schedule.timer_round().all);
            assert_eq!(schedule.wait, CHANNEL_KEY_REFRESH, "every second");
        }
    }
}
