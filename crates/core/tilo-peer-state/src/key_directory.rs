//! The access policy of a device's workspaces, as their state stands ([`KeyDirectory`]).

use std::collections::BTreeSet;

use collab_types::{Ed25519PublicKey, NamedReaders, signer::MaybeSendSync};
use tilo_clock::time::{SystemTime, UNIX_EPOCH};
use tilo_desktop_model::WorkspaceId;
use tilo_workspace_keys::{KeyDirectory, policy_instant};

use crate::StatePeer;

type Devices = BTreeSet<Ed25519PublicKey>;

/// The keys a follower of a workspace also reads its groups as: its pinned device, own key, person.
pub type Follower = ([u8; 32], [u8; 32], [u8; 32]);

/// What reads a workspace's groups at an instant.
pub type Readers<'a> = &'a dyn Fn(SystemTime) -> Result<BTreeSet<[u8; 32]>, String>;

/// The workspaces a key directory reads.
pub trait KeyStates: MaybeSendSync {
    /// What `read` makes of `workspace`'s state as this device has it now; `None` when it has it
    /// not.
    fn with_key_state<R>(
        &self,
        workspace: WorkspaceId,
        read: impl FnOnce(&StatePeer, Option<Follower>, Readers<'_>) -> R,
    ) -> Option<R>;
    fn founds(&self, workspace: WorkspaceId) -> bool;
    /// Whether this device reads `workspace`'s state here now.
    fn reads(&self, workspace: WorkspaceId) -> bool {
        self.with_key_state(workspace, |_, _, _| ()).is_some()
    }
    fn policy_epoch(&self) -> Option<u64> {
        None
    }
    fn next_policy_change(&self, workspace: WorkspaceId, now: u64) -> Option<u64> {
        let _ = (workspace, now);
        None
    }
}

/// The access policy of the workspaces a device holds ([`KeyDirectory`]).
pub struct StateDirectory<S: KeyStates> {
    pub states: S,
}

fn devices(readers: BTreeSet<[u8; 32]>) -> Devices {
    readers
        .into_iter()
        .map(Ed25519PublicKey::from_bytes)
        .collect()
}

impl<S: KeyStates> KeyDirectory for StateDirectory<S> {
    fn requires_document_keys(
        &self,
        workspace: WorkspaceId,
        app: &str,
        document: &str,
    ) -> Option<bool> {
        self.states.with_key_state(workspace, |peer, _, _| {
            peer.requires_document_keys(app, document, tilo_clock::wall())
                .ok()
        })?
    }

    fn document_devices(
        &self,
        workspace: WorkspaceId,
        app: &str,
        document: &str,
        now: u64,
    ) -> Option<Devices> {
        let at = UNIX_EPOCH.checked_add(policy_instant(now))?;
        let readers = self.states.with_key_state(workspace, |peer, _, _| {
            peer.document_key_holders(app, document, at).ok()
        })??;
        Some(devices(readers))
    }

    fn allowed_devices(&self, workspace: WorkspaceId, now: u64) -> Option<Devices> {
        let at = UNIX_EPOCH.checked_add(policy_instant(now))?;
        // A workspace whose governance cannot be read says nothing of who reads it.
        let readers = self
            .states
            .with_key_state(workspace, |_, follower, readers| {
                let mut readers = readers(at).ok()?;
                if let Some((host, device, _)) = follower {
                    readers.insert(host);
                    readers.insert(device);
                }
                Some(readers)
            })??;
        Some(devices(readers))
    }

    fn reader_devices(
        &self,
        workspace: WorkspaceId,
        named: &NamedReaders,
        now: u64,
    ) -> Option<Devices> {
        let at = UNIX_EPOCH.checked_add(policy_instant(now))?;
        let readers = self
            .states
            .with_key_state(workspace, |peer, follower, _| {
                let governance = peer.governance();
                let mut readers = governance.reader_key_holders(named, at).ok()?;
                if let Some((_, device, user)) = follower
                    && governance.reads_named(named, &user, at)
                {
                    readers.insert(device);
                }
                Some(readers)
            })??;
        Some(devices(readers))
    }

    fn founds(&self, workspace: WorkspaceId) -> bool {
        self.states.founds(workspace)
    }

    fn reads(&self, workspace: WorkspaceId) -> bool {
        self.states.reads(workspace)
    }

    fn policy_epoch(&self) -> Option<u64> {
        self.states.policy_epoch()
    }

    fn next_policy_change(&self, workspace: WorkspaceId, now: u64) -> Option<u64> {
        self.states.next_policy_change(workspace, now)
    }
}
