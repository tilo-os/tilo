//! In-memory rooms and directories the channel service's tests drive.
use super::*;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};

/// An in-memory room: every endpoint publishes to all the others while
/// the room is up, and loses what it publishes while it is down.
#[derive(Default)]
pub struct Room {
    pub queues: Mutex<HashMap<usize, Vec<InboundChannelGovernance>>>,
    pub down: AtomicBool,
}

pub struct Endpoint {
    pub room: Arc<Room>,
    pub id: usize,
    pub room_id: RoomId,
}

impl Endpoint {
    pub fn join(room: &Arc<Room>, room_id: RoomId, id: usize) -> Self {
        room.queues.lock().unwrap().entry(id).or_default();
        Self {
            room: Arc::clone(room),
            id,
            room_id,
        }
    }
}

impl GovernanceTransport for Endpoint {
    fn publish(&self, message: NeighbourMsg) -> bool {
        if self.room.down.load(Ordering::SeqCst) {
            return false;
        }
        let room = self.room_id;
        let inbound = match message {
            NeighbourMsg::MlsGovernance { frame, .. } => {
                InboundChannelGovernance::Frame { room, frame }
            }
            NeighbourMsg::MlsAccessRequest { request, .. } => {
                InboundChannelGovernance::AccessRequest {
                    room,
                    request,
                    route: None,
                }
            }
            NeighbourMsg::MlsRoutedAccessRequest { request, route, .. } => {
                InboundChannelGovernance::AccessRequest {
                    room,
                    request,
                    route: Some(route),
                }
            }
            _ => return false,
        };
        for (id, queue) in self.room.queues.lock().unwrap().iter_mut() {
            if *id != self.id {
                queue.push(inbound.clone());
            }
        }
        true
    }

    fn take_inbound(&self) -> Vec<InboundChannelGovernance> {
        std::mem::take(self.room.queues.lock().unwrap().entry(self.id).or_default())
    }
}

#[derive(Clone, Default)]
pub struct Policy {
    pub allowed: Arc<Mutex<BTreeSet<Ed25519PublicKey>>>,
    /// Devices allowed only until a wall-clock second: a grant that
    /// expires.
    pub until: Arc<Mutex<BTreeMap<Ed25519PublicKey, u64>>>,
    /// How the directory announces changes; `None` announces none, and
    /// groups keep their one-second re-check.
    pub announces: Arc<Mutex<Option<Announcements>>>,
    /// The instants groups asked to be announced at, in order; the
    /// test announces them itself when it moves the wall clock there.
    pub announce_at: Arc<Mutex<Vec<u64>>>,
    /// Devices kept out of one channel alone: a Read Deny on its record.
    pub withheld: Arc<Mutex<HashMap<ChannelId, BTreeSet<Ed25519PublicKey>>>>,
    /// Devices kept out of every record of an app: a Read Deny on the
    /// app. A channel whose route a device does not know may be any
    /// app's, so they are kept out of it too (a production directory
    /// keeps everyone out of a channel whose record it cannot name).
    pub withheld_apps: Arc<Mutex<HashMap<String, BTreeSet<Ed25519PublicKey>>>>,
    /// Channels whose record no grant names: without a route the
    /// directory cannot tell who reads them.
    pub unnamed: Arc<Mutex<std::collections::HashSet<ChannelId>>>,
}

/// The epoch a test [`Directory`] reports as its
/// [`ChannelDirectory::policy_epoch`].
#[derive(Clone)]
pub enum Announcements {
    /// [`tilo_identity_host::policy_change`]'s, as production reports.
    Process,
    /// The test's own, bumped by hand.
    Own(Arc<std::sync::atomic::AtomicU64>),
}

pub struct Directory {
    pub governor: Ed25519PublicKey,
    pub policy: Policy,
}

impl ChannelDirectory for Directory {
    fn governor_of(&self, _: RoomId) -> Option<Ed25519PublicKey> {
        Some(self.governor)
    }
    fn allowed_devices(
        &self,
        _: RoomId,
        channel: ChannelId,
        route: Option<&ChannelRoute>,
        now: u64,
    ) -> Option<BTreeSet<Ed25519PublicKey>> {
        if route.is_none() && self.policy.unnamed.lock().unwrap().contains(&channel) {
            return None;
        }
        let mut allowed = self.policy.allowed.lock().unwrap().clone();
        allowed.extend(
            self.policy
                .until
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, until)| now < **until)
                .map(|(device, _)| *device),
        );
        if let Some(withheld) = self.policy.withheld.lock().unwrap().get(&channel) {
            allowed.retain(|device| !withheld.contains(device));
        }
        for (app, withheld) in self.policy.withheld_apps.lock().unwrap().iter() {
            if route.is_none_or(|route| &route.app == app) {
                allowed.retain(|device| !withheld.contains(device));
            }
        }
        Some(allowed)
    }
    fn policy_epoch(&self) -> Option<u64> {
        match &*self.policy.announces.lock().unwrap() {
            None => None,
            Some(Announcements::Process) => Some(tilo_identity_host::policy_change::epoch()),
            Some(Announcements::Own(epoch)) => Some(epoch.load(Ordering::SeqCst)),
        }
    }
    fn next_policy_change(&self, _: RoomId, now: u64) -> Option<u64> {
        self.policy
            .until
            .lock()
            .unwrap()
            .values()
            .copied()
            .filter(|until| *until > now)
            .min()
    }
    fn announce_at(&self, at: u64) {
        self.policy.announce_at.lock().unwrap().push(at);
    }
}

pub fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

pub fn public(seed: u8) -> Ed25519PublicKey {
    Ed25519PublicKey::from_bytes(key(seed).verifying_key().to_bytes())
}
