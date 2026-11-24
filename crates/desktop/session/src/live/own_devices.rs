//! What one of the person's devices sends another of theirs alone, over the live link: their
//! keyboard and pointer, driving it.
//!
//! Each device's daemon relays a scope of its own in the person's own workspace
//! ([`Scope::Device`]). The device driving dials that relay directly, from its daemon to the other
//! device's, and publishes there; the device driven joins its own daemon's relay and reads it. No
//! third device stands between them, and the person's own workspace needs no device serving it.
//! What crosses is sealed under that workspace's keys, which only the person's devices hold, and
//! the relay admits only the devices their record keeps and has not taken back, and lets only the
//! device driven read it. What a message says is its sender's affair: this carries bytes.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};

use collab_types::scope::Scope;
use iroh::{EndpointAddr, EndpointId};
use tilo_desktop_model::WorkspaceId;
use tilo_live::frame::LiveKeys;
use tilo_live::link;
use tilo_live::live::Track;
use tilo_live::live_session::{Join, LiveSession};
use tilo_live::roster::Roster;
use tilo_lock::MutexRecover as _;

use super::{ATTEMPT, Shared};

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
    /// The sessions this device publishes to each device it sends to, by that device.
    to: BTreeMap<[u8; 32], Sending>,
    /// This device's own scope, read here: what the devices driving it send.
    here: Receiving,
    /// The devices this device's own scope hears, as last told.
    heard_from: BTreeSet<[u8; 32]>,
}

enum Sending {
    /// The other device's relay is being dialled: what was sent meanwhile goes as it opens.
    Opening(VecDeque<Vec<u8>>),
    Open {
        session: Arc<LiveSession>,
        sequence: u32,
    },
}

#[derive(Default)]
enum Receiving {
    #[default]
    Closed,
    Opening,
    Open(Arc<LiveSession>),
}

impl OwnDevices {
    pub(super) fn new(shared: Arc<Shared>) -> Self {
        Self {
            shared,
            state: Arc::default(),
        }
    }

    /// Send `body` to the person's device `to`, after what was sent to it before: `false` when it
    /// cannot go, because this session has no link to its daemon yet or the other device's relay
    /// turned it away. The first message to a device dials its relay, and goes once that opens.
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
            Some(Sending::Open { session, sequence }) if !session.ended() => {
                let next = sequence.wrapping_add(1);
                if session
                    .publish_burst(Track::Input, &[(next, body.as_slice())])
                    .is_ok()
                {
                    *sequence = next;
                    return true;
                }
                tracing::debug!(to = %hex::encode(to),
                    "the session to the person's other device ended: it is dialled again");
            }
            Some(Sending::Open { .. }) | None => {}
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
            let opened = open(&shared, to, true, Roster::new([])).await;
            let mut state = sent.lock_recover();
            let Some(Sending::Opening(waiting)) = state.to.remove(&to) else {
                return;
            };
            let Some(session) = opened else {
                tracing::info!(to = %hex::encode(to),
                    dropped = waiting.len(),
                    "the person's other device was not reached: what was sent to it is dropped");
                return;
            };
            let mut sequence = 0_u32;
            let objects: Vec<(u32, &[u8])> = waiting
                .iter()
                .map(|body| {
                    sequence = sequence.wrapping_add(1);
                    (sequence, body.as_slice())
                })
                .collect();
            if !objects.is_empty() && session.publish_burst(Track::Input, &objects).is_err() {
                return;
            }
            state.to.insert(
                to,
                Sending::Open {
                    session: Arc::new(session),
                    sequence,
                },
            );
        });
        true
    }

    /// What the person's devices `from` sent this one since it was last asked, in the order each
    /// sent it, with the device that sealed it. This device's own scope is joined on the first
    /// ask, and heard from `from` from then on.
    pub(crate) fn heard(&self, from: &BTreeSet<[u8; 32]>) -> Vec<([u8; 32], Vec<u8>)> {
        let mut state = self.state.lock_recover();
        let roster_moved = state.heard_from != *from;
        if roster_moved {
            state.heard_from.clone_from(from);
        }
        let session = match &state.here {
            Receiving::Open(session) if !session.ended() => Arc::clone(session),
            Receiving::Opening => return Vec::new(),
            Receiving::Open(_) | Receiving::Closed => {
                let Some(runtime) = self.shared.runtime.get().cloned() else {
                    return Vec::new();
                };
                state.here = Receiving::Opening;
                let roster = Roster::new(from.iter().copied());
                let (shared, heard) = (Arc::clone(&self.shared), Arc::clone(&self.state));
                let own = self.shared.device;
                runtime.spawn(async move {
                    let opened = open(&shared, own, false, roster).await;
                    let mut state = heard.lock_recover();
                    state.here = match opened {
                        Some(session) => {
                            // The devices named while it opened are the ones it hears, and what
                            // they send wakes the session's pass the moment it is here.
                            session.set_roster(Roster::new(state.heard_from.iter().copied()));
                            session.set_wake(Arc::clone(&shared.wake));
                            Receiving::Open(Arc::new(session))
                        }
                        None => Receiving::Closed,
                    };
                    drop(state);
                    (shared.wake)();
                });
                return Vec::new();
            }
        };
        drop(state);
        if roster_moved {
            session.set_roster(Roster::new(from.iter().copied()));
        }
        std::iter::from_fn(|| session.try_recv())
            .filter(|heard| heard.track == Track::Input)
            .map(|heard| (heard.sender, heard.body))
            .collect()
    }
}

/// The person's own workspace, as this session's device names its person.
fn person_workspace(shared: &Shared) -> WorkspaceId {
    let root = shared.engine.identity().device_cert.user_public_key;
    WorkspaceId::from(collab_types::workspace_id::person_workspace(&root))
}

/// Join the scope of the person's device `device`, at its relay: publishing to it from here
/// (`publish`), or this device reading its own, hearing `roster`. `None` when it could not be
/// joined now: no link to the daemon, no keys of the person's workspace, or the relay was not
/// reached or turned this device away.
async fn open(
    shared: &Shared,
    device: [u8; 32],
    publish: bool,
    roster: Roster,
) -> Option<LiveSession> {
    let links = Arc::clone(shared.links.get()?);
    let workspace = person_workspace(shared);
    let keys = match shared.engine.seal_keys(workspace) {
        Ok(crate::engine::SealKeys::Here(keys)) => LiveKeys::from(keys),
        Ok(_) => {
            tracing::debug!("the person's own workspace's keys are not here");
            return None;
        }
        Err(error) => {
            tracing::warn!(%error, "the person's own workspace's keys could not be read");
            return None;
        }
    };
    let scope = Scope::device(workspace.to_string(), device);
    let epoch = keys.epoch();
    let grant = match shared.grant(workspace, &scope, epoch) {
        Ok(grant) => grant,
        Err(error) => {
            tracing::warn!(%error, "this device has no grant for its person's devices");
            return None;
        }
    };
    let toward = if device == shared.device {
        link::Toward::Here
    } else {
        // The person's devices keep sessions with each other's endpoints already: the daemon's
        // endpoint reaches the other device at the address it last learned.
        let Ok(id) = EndpointId::from_bytes(&device) else {
            tracing::debug!(device = %hex::encode(device), "a device key that names no endpoint");
            return None;
        };
        link::Toward::Relay(EndpointAddr::new(id))
    };
    let dial = Arc::new(link::Daemon::new(move || links(), toward));
    let join = Join {
        scope,
        device: shared.device,
        keys,
        roster,
        publish,
        presence: false,
    };
    match LiveSession::join_within(dial, &grant, join, ATTEMPT).await {
        Ok(session) => Some(session),
        Err(error) => {
            tracing::warn!(device = %hex::encode(device), %error,
                "the scope of the person's device was not joined");
            None
        }
    }
}
