//! In-memory workspaces and directories the channel service's tests drive.
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use collab_types::{ChannelId, ChannelRoute, Ed25519PublicKey};
use ed25519_dalek::SigningKey;
use neighbour_wire::NeighbourMsg;
use tilo_desktop_model::WorkspaceId;

use crate::encryption::{ChannelDirectory, GovernanceTransport, InboundChannelGovernance};

/// An in-memory workspace: every endpoint publishes to all the others while
/// the workspace is up, and loses what it publishes while it is down.
#[derive(Default)]
pub struct Workspace {
    pub queues: Mutex<HashMap<usize, Vec<InboundChannelGovernance>>>,
    pub down: AtomicBool,
}

pub struct Endpoint {
    pub workspace: Arc<Workspace>,
    pub id: usize,
    pub workspace_id: WorkspaceId,
}

impl Endpoint {
    pub fn join(workspace: &Arc<Workspace>, workspace_id: WorkspaceId, id: usize) -> Self {
        workspace.queues.lock().unwrap().entry(id).or_default();
        Self {
            workspace: Arc::clone(workspace),
            id,
            workspace_id,
        }
    }
}

impl GovernanceTransport for Endpoint {
    fn publish(&self, message: NeighbourMsg) -> bool {
        if self.workspace.down.load(Ordering::SeqCst) {
            return false;
        }
        let workspace = self.workspace_id;
        let inbound = match message {
            NeighbourMsg::MlsGovernance { frame, .. } => {
                InboundChannelGovernance::Frame { workspace, frame }
            }
            NeighbourMsg::MlsAccessRequest { request, .. } => {
                InboundChannelGovernance::AccessRequest {
                    workspace,
                    request,
                    route: None,
                }
            }
            NeighbourMsg::MlsRoutedAccessRequest { request, route, .. } => {
                InboundChannelGovernance::AccessRequest {
                    workspace,
                    request,
                    route: Some(route),
                }
            }
            _ => return false,
        };
        for (id, queue) in self.workspace.queues.lock().unwrap().iter_mut() {
            if *id != self.id {
                queue.push(inbound.clone());
            }
        }
        true
    }

    fn take_inbound(&self) -> Vec<InboundChannelGovernance> {
        std::mem::take(
            self.workspace
                .queues
                .lock()
                .unwrap()
                .entry(self.id)
                .or_default(),
        )
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
    /// The workspace's history has not reached the device yet
    /// ([`ChannelDirectory::history_held`]).
    pub history_unheld: Arc<std::sync::atomic::AtomicBool>,
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
    pub policy: Policy,
}

impl ChannelDirectory for Directory {
    fn allowed_devices(
        &self,
        _: WorkspaceId,
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
    /// Each channel of these tests is a reader set of its own: the fixture
    /// narrows every record, so its keys never leave the devices its policy
    /// names for it.
    fn unit_scope(
        &self,
        workspace: WorkspaceId,
        route: &ChannelRoute,
        _: u64,
    ) -> Option<ChannelId> {
        Some(route.channel(&workspace.to_string()))
    }
    fn history_held(&self, _: WorkspaceId) -> bool {
        !self.policy.history_unheld.load(Ordering::SeqCst)
    }
    fn policy_epoch(&self) -> Option<u64> {
        match &*self.policy.announces.lock().unwrap() {
            None => None,
            Some(Announcements::Process) => Some(tilo_identity_host::policy_change::epoch()),
            Some(Announcements::Own(epoch)) => Some(epoch.load(Ordering::SeqCst)),
        }
    }
    fn next_policy_change(&self, _: WorkspaceId, now: u64) -> Option<u64> {
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

impl Policy {
    /// A policy whose channels are read by the devices `seeds` name.
    pub fn readers(seeds: impl IntoIterator<Item = u8>) -> Self {
        let policy = Self::default();
        for seed in seeds {
            policy.allowed.lock().unwrap().insert(public(seed));
        }
        policy
    }
}

pub fn public(seed: u8) -> Ed25519PublicKey {
    Ed25519PublicKey::from_bytes(key(seed).verifying_key().to_bytes())
}
