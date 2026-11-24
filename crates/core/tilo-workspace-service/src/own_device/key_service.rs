//! The device's part in the encrypted groups of the workspaces its own service holds.
//!
//! The device takes part in every group of those workspaces with the one leaf it has in every
//! other group ([`WorkspaceKeys`]). What the workspaces' other devices send for a group reaches it
//! through the service's [`KeySink`]: a governed workspace's members' links, and the link of a
//! member copy to the device serving it. What it settles goes back the same way: retained by a
//! workspace the service governs as its own governance, so whoever joins it later reads the
//! group's history from it, and handed to the workspace's direct link
//! ([`Workspaces::publish_governance`]); sent on the member link of a member copy
//! ([`Workspaces::send_member`]). Who takes part in a group is read from the workspace's replica
//! as it stands ([`Workspaces::read_now`]), so the service decides a record's group from the same
//! state the workspace service does, at the same moment.

use std::{
    collections::BTreeSet,
    io,
    path::Path,
    sync::{Arc, Mutex},
    time::UNIX_EPOCH,
};

use collab_types::Ed25519PublicKey;
use collab_types::signer::DeviceSigner;
use key_group::VaultKey;
use neighbour_wire::NeighbourMsg;
use tilo_desktop_model::WorkspaceId;
use tilo_lock::MutexRecover as _;
use tilo_peer_state::StatePeer;
use tilo_workspace_keys::{
    GovernancePark, GovernancePump, GovernanceTransport, InboundGovernance, KeyDir, KeyDirectory,
    KeyProvider, KeysConfig, SealCheckRegistry, SharedKeys, WorkspaceKeys,
};
use tokio::task::JoinHandle;

use crate::workspaces::{DeviceKeyGroups, Held, KeySink, WeakWorkspaces, Workspaces};

/// The device's key service: its keys and the thread settling the groups of the workspaces its
/// own service holds. Stops when dropped.
pub struct KeyService {
    keys: KeyProvider,
    /// The service itself, to forget the workspaces deleted here.
    access: SharedKeys,
    link: Arc<Link>,
    _pump: GovernancePump,
    following: Mutex<Vec<JoinHandle<()>>>,
}

impl KeyService {
    /// Open the device's group state under `state`, sealed with `vault`, taking part as `signer`
    /// and checking what a group's members seal with `seal_checks`. `name` names the thread that
    /// settles the groups.
    ///
    /// # Errors
    ///
    /// The sealed state cannot be read or created, or the thread settling it cannot start.
    pub fn open(
        name: &str,
        state: &Path,
        vault: VaultKey,
        signer: Arc<dyn DeviceSigner>,
        seal_checks: Arc<dyn SealCheckRegistry>,
    ) -> io::Result<Self> {
        let directory = Directory {
            workspaces: Arc::default(),
            founders: Arc::new(Mutex::new(BTreeSet::from([signer.device_public_key()]))),
        };
        let service = WorkspaceKeys::open(
            KeysConfig {
                state: Arc::new(KeyDir::new(state.to_path_buf())),
                vault,
                signer,
                role: tilo_workspace_keys::KeyRole::Committer,
            },
            Box::new(directory.clone()),
            seal_checks,
        )?;
        let link = Arc::new(Link {
            directory,
            inbound: Mutex::default(),
            publish: Mutex::default(),
            park: GovernancePark::new(),
        });
        let park = Arc::clone(&link.park);
        let poke = Arc::clone(&park);
        let access = SharedKeys::with_wake(service, Arc::new(move || poke.poke()));
        let keys = KeyProvider::default();
        keys.set(Some(access.clone()));
        let pump = GovernancePump::spawn(
            name,
            park,
            Some(access.clone()),
            Arc::clone(&link) as Arc<dyn GovernanceTransport + Send + Sync>,
            || {},
        )?;
        Ok(Self {
            keys,
            access,
            link,
            _pump: pump,
            following: Mutex::new(Vec::new()),
        })
    }

    /// The device's keys, as whatever seals and opens its records asks for them.
    #[must_use]
    pub fn keys(&self) -> KeyProvider {
        self.keys.clone()
    }

    /// The device's key service as its workspace service asks for it: its keys, and the sink the
    /// group governance the service's links carry is handed to.
    #[must_use]
    pub fn groups(&self) -> DeviceKeyGroups {
        super::key_groups(
            self.keys.clone(),
            None,
            Some(Arc::clone(&self.link) as Arc<dyn KeySink>),
            None,
        )
    }

    /// Send what this service settles for `workspaces`, the service it was handed to, and forget
    /// the groups of the workspaces deleted there. Run on the runtime the service is served on.
    pub fn attach(&self, workspaces: &Workspaces) {
        self.link
            .directory
            .founders
            .lock_recover()
            .insert(workspaces.host_device());
        *self.link.directory.workspaces.lock_recover() = Some(workspaces.downgrade());
        let (publish, mut outgoing) = tokio::sync::mpsc::unbounded_channel();
        *self.link.publish.lock_recover() = Some(publish);
        let writing = workspaces.clone();
        let publisher = tokio::spawn(async move {
            while let Some(message) = outgoing.recv().await {
                let Some(workspace) = message.governance_workspace() else {
                    continue;
                };
                let workspace = workspace.to_owned();
                if let Err(error) = writing.publish_governance(&workspace, vec![message]).await {
                    tracing::warn!(%workspace, %error, "the device's group governance was not retained");
                }
            }
        });
        self.following.lock_recover().push(publisher);
        // A workspace deleted here takes its groups with it.
        let mut deletions = workspaces.deletions();
        let access = self.access.clone();
        let forgetting = tokio::spawn(async move {
            loop {
                let workspace = match deletions.recv().await {
                    Ok((workspace, _)) => workspace,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(missed)) => {
                        tracing::error!(missed, "deleted workspaces' groups were not forgotten");
                        continue;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                };
                let Some(id) = WorkspaceId::parse(&workspace) else {
                    continue;
                };
                if let Err(error) = access.forget_workspace(id) {
                    tracing::warn!(%workspace, %error, "a deleted workspace's groups stay");
                }
            }
        });
        self.following.lock_recover().push(forgetting);
        self.link.park.poke();
    }
}

impl Drop for KeyService {
    fn drop(&mut self) {
        let following = std::mem::take(&mut *self.following.lock_recover());
        for task in following {
            task.abort();
        }
    }
}

/// The transport the device's group governance travels on: what the service's links carried
/// here, waiting for the next pass, and what the pass settles, on its way to the workspace.
struct Link {
    /// Who may read the groups of the workspaces the service holds.
    directory: Directory,
    /// Governance the service's links carried here, waiting for the next pass.
    inbound: Mutex<Vec<InboundGovernance>>,
    /// What the service settles for a workspace it governs, once the service is open.
    publish: Mutex<Option<tokio::sync::mpsc::UnboundedSender<NeighbourMsg>>>,
    park: Arc<GovernancePark>,
}

impl KeySink for Link {
    fn take(&self, messages: Vec<NeighbourMsg>) {
        let mut inbound = self.inbound.lock_recover();
        inbound.extend(
            messages
                .into_iter()
                .filter_map(InboundGovernance::from_message),
        );
        drop(inbound);
        self.park.poke();
    }

    /// The pass after a workspace settles reads its replica as it now stands.
    fn settled(&self, _workspace: &str, _state: StatePeer) {
        self.park.poke();
    }
}

impl GovernanceTransport for Link {
    /// A member copy's governance goes to the device serving it while the copy's link is up, and
    /// is kept for a later pass while it is not; a governed workspace retains it.
    fn publish(&self, message: NeighbourMsg) -> bool {
        let Some(workspace) = message.governance_workspace().map(str::to_owned) else {
            return false;
        };
        let Some(workspaces) = self.directory.workspaces() else {
            return false;
        };
        if workspaces
            .read_now(&workspace, |_| ())
            .map(|(held, ())| held)
            == Some(Held::MemberCopy)
        {
            return WorkspaceId::parse(&workspace)
                .is_some_and(|id| workspaces.send_member(id, message));
        }
        let publish = self.publish.lock_recover().clone();
        publish.is_some_and(|publish| publish.send(message).is_ok())
    }

    fn take_inbound(&self) -> Vec<InboundGovernance> {
        std::mem::take(&mut *self.inbound.lock_recover())
    }
}

/// Who may read each group of the workspaces the service holds, as their replicas stand.
#[derive(Clone)]
struct Directory {
    /// The service's workspaces, once it is open, held without keeping them alive: they hold
    /// this directory's link as their key sink, and through them the device's endpoint, whose
    /// socket a stopped server would otherwise never let go of.
    workspaces: Arc<Mutex<Option<WeakWorkspaces>>>,
    /// The devices a workspace this device founds was founded by: the device, and its service.
    founders: Arc<Mutex<BTreeSet<[u8; 32]>>>,
}

impl Directory {
    fn workspaces(&self) -> Option<Workspaces> {
        self.workspaces.lock_recover().as_ref()?.upgrade()
    }

    /// What `read` makes of `workspace`'s replica as it stands, and how the service holds it.
    fn read<R>(
        &self,
        workspace: WorkspaceId,
        read: impl FnOnce(&StatePeer) -> R,
    ) -> Option<(Held, R)> {
        self.workspaces()?.read_now(&workspace.to_string(), read)
    }

    fn state<R>(&self, workspace: WorkspaceId, read: impl FnOnce(&StatePeer) -> R) -> Option<R> {
        self.read(workspace, read).map(|(_, read)| read)
    }
}

impl KeyDirectory for Directory {
    fn requires_document_keys(
        &self,
        workspace: WorkspaceId,
        app: &str,
        document: &str,
    ) -> Option<bool> {
        self.state(workspace, |state| {
            state
                .requires_document_keys(app, document, tilo_clock::wall())
                .ok()
        })?
    }

    fn document_devices(
        &self,
        workspace: WorkspaceId,
        app: &str,
        document: &str,
        now: u64,
    ) -> Option<BTreeSet<Ed25519PublicKey>> {
        let at = UNIX_EPOCH.checked_add(tilo_workspace_keys::policy_instant(now))?;
        let readers = self.state(workspace, |state| {
            state.document_key_holders(app, document, at).ok()
        })??;
        Some(
            readers
                .into_iter()
                .map(Ed25519PublicKey::from_bytes)
                .collect(),
        )
    }

    fn allowed_devices(
        &self,
        workspace: WorkspaceId,
        now: u64,
    ) -> Option<BTreeSet<Ed25519PublicKey>> {
        let at = UNIX_EPOCH.checked_add(tilo_workspace_keys::policy_instant(now))?;
        // Governance that cannot be read says nothing of who reads it.
        let readers = self.state(workspace, |state| state.governance().key_holders(at).ok())??;
        Some(
            readers
                .into_iter()
                .map(Ed25519PublicKey::from_bytes)
                .collect(),
        )
    }

    fn reader_devices(
        &self,
        workspace: WorkspaceId,
        named: &collab_types::NamedReaders,
        now: u64,
    ) -> Option<BTreeSet<Ed25519PublicKey>> {
        let at = UNIX_EPOCH.checked_add(tilo_workspace_keys::policy_instant(now))?;
        let readers = self.state(workspace, |state| {
            state.governance().reader_key_holders(named, at).ok()
        })??;
        Some(
            readers
                .into_iter()
                .map(Ed25519PublicKey::from_bytes)
                .collect(),
        )
    }

    /// A workspace the service governs whose founding device is this device or its service: a
    /// member copy's groups are its serving device's to found.
    fn founds(&self, workspace: WorkspaceId) -> bool {
        let Some((Held::Governs, founder)) =
            self.read(workspace, |state| state.governance().founder())
        else {
            return false;
        };
        founder.is_some_and(|founder| self.founders.lock_recover().contains(&founder))
    }
}
