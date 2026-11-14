//! In-memory workspaces and directories the channel service's tests drive.
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use collab_types::Ed25519PublicKey;
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
                InboundChannelGovernance::AccessRequest { workspace, request }
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
    /// The workspace's governance is not known here: the directory names no
    /// one.
    pub unknown: Arc<std::sync::atomic::AtomicBool>,
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
    /// This device created the workspace, so it founds its group
    /// ([`ChannelDirectory::founds`]).
    pub founds: Arc<AtomicBool>,
}

impl Directory {
    /// The directory of the device that created the workspace.
    pub fn new(policy: Policy) -> Self {
        Self {
            policy,
            founds: Arc::new(AtomicBool::new(true)),
        }
    }
}

impl ChannelDirectory for Directory {
    fn allowed_devices(&self, _: WorkspaceId, now: u64) -> Option<BTreeSet<Ed25519PublicKey>> {
        if self.policy.unknown.load(Ordering::SeqCst) {
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
        Some(allowed)
    }
    fn founds(&self, _: WorkspaceId) -> bool {
        self.founds.load(Ordering::SeqCst)
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
