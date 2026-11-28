//! What one of the person's devices sends another of theirs alone, over the live link: their
//! keyboard and pointer, driving it.
//!
//! Each device's daemon relays a scope of its own in the person's own workspace
//! ([`collab_types::scope::Scope::device`]). This session holds no key, so it opens a link on its
//! daemon's socket toward that scope ([`Toward::Device`]) and the daemon joins it as the device:
//! the scope of the device driven at that device's relay, dialled straight from this device's
//! daemon, or this device's own on the relay its daemon runs. No third device stands between
//! them, and the person's own workspace needs no device serving it. What crosses is sealed by the
//! daemons under that workspace's keys, which only the person's devices hold; each relay admits
//! only the devices whose certificates the person's root signed and which they have not taken
//! back, and lets only the device driven read it. What a message says is its sender's affair:
//! this carries bytes.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tilo_live::link::Toward;
use tilo_live::link_call::Call;
use tilo_live::live::Track;
use tilo_lock::MutexRecover as _;

use super::Shared;

/// The way to the person's other devices, and from them to this one.
#[derive(Clone)]
pub(crate) struct OwnDevices {
    shared: Arc<Shared>,
    state: Arc<Mutex<State>>,
}

impl std::fmt::Debug for OwnDevices {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OwnDevices").finish_non_exhaustive()
    }
}

#[derive(Default)]
struct State {
    /// The links this device publishes to each device it sends to, by that device.
    to: BTreeMap<[u8; 32], Sending>,
    /// The link into this device's own scope: what the devices driving it send.
    here: Receiving,
    /// The devices this device's own scope is read for, as last told.
    heard_from: BTreeSet<[u8; 32]>,
}

enum Sending {
    /// The link toward the other device's scope is opening: what was sent meanwhile goes as it
    /// opens.
    Opening(VecDeque<Vec<u8>>),
    Open(Arc<Call>),
}

/// How long this device's own scope waits, after it was not joined, before a pass joins it
/// again while the devices it hears are the same.
const JOIN_AGAIN: Duration = Duration::from_secs(30);

#[derive(Default)]
enum Receiving {
    #[default]
    Closed,
    Opening,
    Open(Arc<Call>),
    /// It was not joined at `since`: joined again once other devices are to be heard, or
    /// [`JOIN_AGAIN`] later.
    Missed {
        since: Instant,
    },
}

impl OwnDevices {
    pub(super) fn new(shared: Arc<Shared>) -> Self {
        Self {
            shared,
            state: Arc::default(),
        }
    }

    /// Send `body` to the person's device `to`, after what was sent to it before: `false` when it
    /// cannot go, because this session has no link to its daemon yet or the link toward the
    /// other device ended. The first message to a device opens the link, and goes once it opens.
    pub(crate) fn send(&self, to: [u8; 32], body: Vec<u8>) -> bool {
        if to == self.shared.device {
            return false;
        }
        let mut state = self.state.lock_recover();
        match state.to.get_mut(&to) {
            Some(Sending::Opening(waiting)) => {
                waiting.push_back(body);
                return true;
            }
            Some(Sending::Open(call)) if !call.ended() => {
                if call.publish(Track::Input, 0, &body).is_ok() {
                    return true;
                }
                tracing::debug!(to = %hex::encode(to),
                    "the link to the person's other device ended: it is opened again");
            }
            Some(Sending::Open(_)) | None => {}
        }
        let Some(runtime) = self.shared.runtime.get().cloned() else {
            state.to.remove(&to);
            return false;
        };
        state
            .to
            .insert(to, Sending::Opening(VecDeque::from([body])));
        drop(state);
        let (shared, sent) = (Arc::clone(&self.shared), Arc::clone(&self.state));
        runtime.spawn(async move {
            let opened = open(&shared, to).await;
            let mut state = sent.lock_recover();
            let Some(Sending::Opening(waiting)) = state.to.remove(&to) else {
                return;
            };
            let Some(call) = opened else {
                tracing::info!(to = %hex::encode(to),
                    dropped = waiting.len(),
                    "the person's other device was not reached: what was sent to it is dropped");
                return;
            };
            for body in &waiting {
                if call.publish(Track::Input, 0, body).is_err() {
                    return;
                }
            }
            state.to.insert(to, Sending::Open(call));
        });
        true
    }

    /// What the person's devices `from` sent this one since it was last asked, in the order each
    /// sent it, with the device that sealed it. This device's own scope is joined on the first
    /// ask; its daemon hears every device of the person there, and what a device not in `from`
    /// sent is left out here.
    pub(crate) fn heard(&self, from: &BTreeSet<[u8; 32]>) -> Vec<([u8; 32], Vec<u8>)> {
        let mut state = self.state.lock_recover();
        let heard_moved = state.heard_from != *from;
        if heard_moved {
            state.heard_from.clone_from(from);
        }
        let call = match &state.here {
            Receiving::Open(call) if !call.ended() => Arc::clone(call),
            Receiving::Opening => return Vec::new(),
            Receiving::Missed { since } if !heard_moved && since.elapsed() < JOIN_AGAIN => {
                return Vec::new();
            }
            Receiving::Open(_) | Receiving::Closed | Receiving::Missed { .. } => {
                let Some(runtime) = self.shared.runtime.get().cloned() else {
                    return Vec::new();
                };
                state.here = Receiving::Opening;
                let (shared, heard) = (Arc::clone(&self.shared), Arc::clone(&self.state));
                let own = self.shared.device;
                runtime.spawn(async move {
                    let opened = open(&shared, own).await;
                    let mut state = heard.lock_recover();
                    let Some(call) = opened else {
                        // Nothing came to hear: the session's pass is not woken for it, or each
                        // pass would join again at once and wake the next.
                        state.here = Receiving::Missed {
                            since: Instant::now(),
                        };
                        return;
                    };
                    // What the devices send wakes the session's pass the moment it is here.
                    call.set_wake(Arc::clone(&shared.wake));
                    state.here = Receiving::Open(call);
                    drop(state);
                    (shared.wake)();
                });
                return Vec::new();
            }
        };
        drop(state);
        std::iter::from_fn(|| call.try_recv())
            .filter(|heard| heard.track == Track::Input && from.contains(&heard.sender))
            .map(|heard| (heard.sender, heard.body))
            .collect()
    }
}

/// Open the link toward the scope of the person's device `device`, which this device's daemon
/// joins as this device. `None` when it could not be opened now: no link to the daemon yet, or
/// the daemon could not join the scope (the person's own workspace's keys are not there yet, or
/// the other device was not reached or turned this one away).
async fn open(shared: &Shared, device: [u8; 32]) -> Option<Arc<Call>> {
    let links = Arc::clone(shared.links.get()?);
    match Call::open_toward(move || links(), Toward::Device { device }, shared.device).await {
        Ok(call) => Some(call),
        Err(error) => {
            tracing::warn!(device = %hex::encode(device), %error,
                "the scope of the person's device was not joined");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "in a test this is the assertion")]

    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// A scope that does not join wakes no pass, and is not joined again by the passes after it
    /// while the devices it hears stay the same: a session that cannot join its own scope
    /// would otherwise wake itself to join again, round after round.
    #[test]
    fn a_scope_that_did_not_join_is_not_joined_again_by_every_pass() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let wakes = Arc::new(AtomicUsize::new(0));
        let woken = Arc::clone(&wakes);
        let live = super::super::Live::new(
            [9; 32],
            Arc::new(move || {
                woken.fetch_add(1, Ordering::SeqCst);
            }),
        );
        live.shared.runtime.set(runtime.handle().clone()).unwrap();
        let own = live.own_devices();
        let missed = || matches!(own.state.lock_recover().here, Receiving::Missed { .. });
        let one = BTreeSet::from([[1_u8; 32]]);
        assert!(own.heard(&one).is_empty());
        let deadline = Instant::now() + Duration::from_secs(10);
        while !missed() {
            assert!(Instant::now() < deadline, "the join never ended");
            std::thread::yield_now();
        }
        for _ in 0..100 {
            assert!(own.heard(&one).is_empty());
            assert!(missed(), "a pass joined the scope again");
        }
        assert_eq!(
            wakes.load(Ordering::SeqCst),
            0,
            "a join that failed woke the session"
        );
        // Another device to hear joins it again.
        assert!(
            own.heard(&BTreeSet::from([[1_u8; 32], [2_u8; 32]]))
                .is_empty()
        );
        assert!(!missed(), "another device to hear did not join again");
    }
}
