//! The relay's part in the encrypted groups of the workspaces it governs.
//!
//! A relay whose device may own workspaces takes part in every group of the
//! workspaces it governs, with the one leaf it has in every other group
//! ([`WorkspaceKeys`]). What its members send for a group reaches it
//! through the workspace host's [`KeySink`], and what it settles is
//! retained by the workspace as its own governance, so whoever joins the
//! workspace later reads the group's history from it, and handed to the
//! workspace's direct link, so a member device linked there hears this
//! device's access requests and commits as it hears every other member's
//! ([`Workspaces::publish_governance`]). Each workspace hands the
//! service its governance as the workspace settles
//! ([`KeySink::settled`]), so the service decides a record's group from
//! the same state the host does, at the same moment.
//!
//! A relay whose device may not own workspaces runs no key service: its
//! workspaces retain what their members' groups are made of, and this device
//! reads none of it.

use std::{
    collections::{BTreeSet, HashMap},
    io,
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, UNIX_EPOCH},
};

use collab_types::Ed25519PublicKey;
use ed25519_dalek::SigningKey;
use key_group::VaultKey;
use neighbour_wire::NeighbourMsg;
use tilo_action_broker::workspace_keys::{
    GovernancePark, GovernancePump, GovernanceTransport, InboundGovernance, KeyDir, KeyDirectory,
    KeyProvider, KeysConfig, NoGates, SharedKeys, WorkspaceKeys,
};
use tilo_desktop_model::WorkspaceId;
use tilo_peer_state::StatePeer;
use tilo_room_host::workspaces::{
    DeviceKeyGroups, DeviceKeys, GroupKeys, GroupKeysAnswerFuture, KeySink, Workspaces,
};
use tokio::task::JoinHandle;

/// How long a first write to a record's group waits for its group: half a
/// minute, which is far more than the wait before this device creates the
/// group itself (`workspace_keys::founding_wait`) and the work a start
/// does beside it — creating the workspace, its listing and its settings, and
/// writing the genesis of each app the operator seeded. A fresh start's first
/// seed waits for all of that, so a wait as short as the group's own is
/// refused for keys that are about to arrive; keys that have not arrived in
/// half a minute are not about to.
const OPEN_WAIT: Duration = Duration::from_secs(30);
/// A first write must outlast the wait before the group is created here, or a
/// fresh workspace's first seed is refused for keys that do arrive.
const _: () =
    assert!(OPEN_WAIT.as_secs() > tilo_action_broker::workspace_keys::founding_wait().as_secs());
/// Where the shared key service keeps its state under the state root.
const STATE: &str = "groups";
/// Where the 0.1 releases kept each workspace's channel state under the state
/// root: their keys open what those releases sealed.
const RELEASED: &str = "channels";

/// The relay's key service: this device's keys and the thread settling the
/// groups of the workspaces it governs. Stops when dropped.
pub(crate) struct KeyService {
    device: DeviceKeys,
    /// The service itself, to forget the workspaces deleted here.
    access: SharedKeys,
    link: Arc<Link>,
    _pump: GovernancePump,
    following: Mutex<Vec<JoinHandle<()>>>,
}

impl KeyService {
    /// Open the relay's group state under `state_root`, taking part as
    /// `device`.
    ///
    /// # Errors
    ///
    /// The sealed state cannot be read or created, or the thread settling it
    /// cannot start.
    pub(crate) fn open(state_root: &Path, device: SigningKey) -> io::Result<Self> {
        let root = state_root.join(STATE);
        let directory = Directory {
            states: Arc::new(Mutex::new(HashMap::new())),
            device: device.verifying_key().to_bytes(),
        };
        let service = WorkspaceKeys::open(
            KeysConfig {
                state: Arc::new(
                    KeyDir::new(&root)
                        .with_released(state_root.join(RELEASED), released_vault_key(&device)),
                ),
                vault: vault_key(&device),
                signer: Arc::new(device),
                role: tilo_action_broker::workspace_keys::KeyRole::Committer,
            },
            Box::new(directory.clone()),
            Arc::new(NoGates),
        )?;
        let link = Link::new(directory);
        let park = Arc::clone(&link.park);
        let poke = Arc::clone(&park);
        let access = SharedKeys::with_wake(service, Arc::new(move || poke.poke()));
        let keys = KeyProvider::default();
        keys.set(Some(access.clone()));
        let kept = access.clone();
        let pump = GovernancePump::spawn(
            "tilo-relay-groups",
            park,
            Some(access),
            Arc::clone(&link) as Arc<dyn GovernanceTransport + Send + Sync>,
            || {},
        )?;
        Ok(Self {
            device: Arc::new(Keys(keys)),
            access: kept,
            link,
            _pump: pump,
            following: Mutex::new(Vec::new()),
        })
    }

    /// The device's key service as the workspace host asks for it: its
    /// keys, and the sink the group governance its sessions hear is handed
    /// to.
    pub(crate) fn device(&self) -> DeviceKeyGroups {
        DeviceKeyGroups {
            keys: Arc::clone(&self.device),
            sink: Some(Arc::clone(&self.link) as Arc<dyn KeySink>),
        }
    }

    /// Retain what this service settles for `workspaces`, and read who may
    /// read their groups from the governance each one notes as it settles
    /// ([`KeySink::settled`]).
    pub(crate) fn attach(&self, workspaces: &Workspaces) {
        let (publish, mut outgoing) = tokio::sync::mpsc::unbounded_channel();
        *lock(&self.link.publish) = Some(publish);
        let writing = workspaces.clone();
        let publisher = tokio::spawn(async move {
            while let Some(message) = outgoing.recv().await {
                let Some(workspace) = message.governance_workspace() else {
                    continue;
                };
                let workspace = workspace.to_owned();
                if let Err(error) = writing.publish_governance(&workspace, vec![message]).await {
                    tracing::warn!(%workspace, %error, "the relay's group governance was not retained");
                }
            }
        });
        lock(&self.following).push(publisher);
        // A workspace deleted here takes its groups with it.
        let mut deletions = workspaces.deletions();
        let (access, directory) = (self.access.clone(), self.link.directory.clone());
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
                lock(&directory.states).remove(&workspace);
                let Some(id) = WorkspaceId::parse(&workspace) else {
                    continue;
                };
                if let Err(error) = access.forget_workspace(id) {
                    tracing::warn!(%workspace, %error, "a deleted workspace's groups stay");
                }
            }
        });
        lock(&self.following).push(forgetting);
        self.link.park.poke();
    }
}

impl Drop for KeyService {
    fn drop(&mut self) {
        let following = std::mem::take(&mut *lock(&self.following));
        for task in following {
            task.abort();
        }
    }
}

/// The device's group keys, as the workspace host asks for them.
struct Keys(KeyProvider);

impl GroupKeys for Keys {
    fn group_keys<'a>(&'a self, workspace: &'a str, create: bool) -> GroupKeysAnswerFuture<'a> {
        let keys = self.0.clone();
        Box::pin(async move {
            if keys.get().is_none() {
                return Some(Err("this device's key service is not running".into()));
            }
            keys.group_keys(workspace, create, OPEN_WAIT).await
        })
    }
}

impl KeySink for Link {
    fn take(&self, messages: Vec<NeighbourMsg>) {
        let mut inbound = lock(&self.inbound);
        inbound.extend(
            messages
                .into_iter()
                .filter_map(InboundGovernance::from_message),
        );
        drop(inbound);
        self.park.poke();
    }

    fn settled(&self, workspace: &str, state: StatePeer) {
        self.directory.note(workspace, state);
        self.park.poke();
    }
}

/// The workspace transport the relay's group governance travels on: what its
/// members sent, waiting for the next pass, and what the pass settles, on its
/// way to the workspace's store.
struct Link {
    /// Who may read the groups of the workspaces this relay governs, as each
    /// noted its governance when it settled.
    directory: Directory,
    /// Governance the workspace host's sessions carried here, waiting for the
    /// next pass.
    inbound: Mutex<Vec<InboundGovernance>>,
    /// What the service settles, once the workspaces it settles it for are
    /// known.
    publish: Mutex<Option<tokio::sync::mpsc::UnboundedSender<NeighbourMsg>>>,
    park: Arc<GovernancePark>,
}

impl Link {
    fn new(directory: Directory) -> Arc<Self> {
        Arc::new(Self {
            directory,
            inbound: Mutex::default(),
            publish: Mutex::default(),
            park: GovernancePark::new(),
        })
    }
}

impl GovernanceTransport for Link {
    fn publish(&self, message: NeighbourMsg) -> bool {
        let publish = lock(&self.publish).clone();
        publish.is_some_and(|publish| publish.send(message).is_ok())
    }

    fn take_inbound(&self) -> Vec<InboundGovernance> {
        std::mem::take(&mut *lock(&self.inbound))
    }
}

/// Who may read each group of the workspaces the relay governs, as their
/// governance last stood.
#[derive(Clone)]
struct Directory {
    /// Each workspace's state as it last settled, by workspace UUID: a handle
    /// to the same document the workspace holds. Shared, so the pump reads it
    /// on its own thread without reaching for the workspace.
    states: Arc<Mutex<HashMap<String, StatePeer>>>,
    /// The relay's own device key.
    device: [u8; 32],
}

impl Directory {
    /// `workspace`'s state, as the workspace noted it when it settled.
    fn note(&self, workspace: &str, state: StatePeer) {
        lock(&self.states).insert(workspace.to_owned(), state);
    }
}

impl KeyDirectory for Directory {
    fn allowed_devices(
        &self,
        workspace: WorkspaceId,
        now: u64,
    ) -> Option<BTreeSet<Ed25519PublicKey>> {
        let at = UNIX_EPOCH.checked_add(tilo_action_broker::workspace_keys::policy_instant(now))?;
        let state = lock(&self.states).get(&workspace.to_string())?.clone();
        // Governance that cannot be read says nothing of who reads it.
        let readers = state.governance().key_holders(at).ok()?;
        Some(
            readers
                .into_iter()
                .map(Ed25519PublicKey::from_bytes)
                .collect(),
        )
    }
    fn founds(&self, workspace: WorkspaceId) -> bool {
        lock(&self.states)
            .get(&workspace.to_string())
            .is_some_and(|state| state.governance().founder() == Some(self.device))
    }
}

/// The key sealing the relay's group state: derived from the device key the
/// relay signs with.
fn vault_key(device: &SigningKey) -> VaultKey {
    VaultKey::new(blake3::derive_key(
        "tilo-relay workspace key vault v1",
        &device.to_bytes(),
    ))
}

/// The key the 0.1 releases sealed the relay's channel state under, from the
/// same device key.
fn released_vault_key(device: &SigningKey) -> VaultKey {
    VaultKey::new(blake3::derive_key(
        "tilo-relay governed room channel vault v1",
        &device.to_bytes(),
    ))
}

/// A lock a panic elsewhere cannot poison away: what these hold is read from
/// the workspaces' governance.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::{KeyDirectory, KeyService, STATE, SigningKey};

    #[test]
    fn a_relay_that_may_own_workspaces_takes_part_in_their_groups() {
        let state = tempfile::tempdir().unwrap();
        let device = SigningKey::from_bytes(&[7; 32]);
        let groups = KeyService::open(state.path(), device).unwrap();
        assert!(state.path().join(STATE).is_dir());
        let device = groups.device();
        assert!(device.sink.is_some());
        assert!(
            groups
                .link
                .directory
                .allowed_devices(tilo_desktop_model::WorkspaceId::new(), 0)
                .is_none()
        );
    }
}
