//! The relay's part in the encrypted channels of the workspaces it governs.
//!
//! A relay whose device may own workspaces takes part in every channel of the
//! workspaces it governs, with the one leaf it has in every other channel
//! ([`ChannelEncryption`]). What its members send for a channel reaches it
//! through the workspace host's [`ChannelSink`], and what it settles is
//! retained by the workspace as its own governance, so whoever joins the
//! workspace later reads the group's history from it, and handed to the
//! workspace's direct link, so a member device linked there hears this
//! device's access requests and commits as it hears every other member's
//! ([`Workspaces::publish_channel_governance`]). Each workspace hands the
//! service its governance as the workspace settles
//! ([`ChannelSink::settled`]), so the service decides a record's channel from
//! the same state the host does, at the same moment.
//!
//! A relay whose device may not own workspaces runs no channel service: its
//! workspaces retain what their members' channels are made of, and this device
//! reads none of it.

use std::{
    collections::{BTreeSet, HashMap},
    io,
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, UNIX_EPOCH},
};

use channel_crypto::VaultKey;
use collab_types::{ChannelId, ChannelRoute, Ed25519PublicKey};
use ed25519_dalek::SigningKey;
use neighbour_wire::NeighbourMsg;
use tilo_action_broker::channel_encryption::{
    ChannelDirectory, ChannelEncryption, ChannelEncryptionConfig, ChannelKeyAccess,
    ChannelKeyProvider, ChannelsDir, GovernancePark, GovernancePump, GovernanceTransport,
    InboundChannelGovernance, NoGates,
};
use tilo_desktop_model::WorkspaceId;
use tilo_peer_state::StatePeer;
use tilo_room_host::workspaces::{
    ChannelKeys, ChannelKeysAnswerFuture, ChannelSink, DeviceChannels, DeviceKeys, Workspaces,
};
use tokio::task::JoinHandle;

/// How long a first write to a record's channel waits for its group.
const OPEN_WAIT: Duration = Duration::from_secs(5);
/// Where the shared channel service keeps its state under the state root.
const STATE: &str = "channels";
/// Where the workspace host keeps the workspaces this relay governs.
const ROOMS: &str = "rooms";

/// The relay's channel service: this device's keys and the thread settling the
/// groups of the workspaces it governs. Stops when dropped.
pub(crate) struct Channels {
    device: DeviceKeys,
    link: Arc<Link>,
    _pump: GovernancePump,
    following: Mutex<Vec<JoinHandle<()>>>,
}

impl Channels {
    /// Open the relay's channel state under `state_root`, taking part as
    /// `device`.
    ///
    /// # Errors
    ///
    /// The sealed state cannot be read or created, or the thread settling it
    /// cannot start.
    pub(crate) fn open(state_root: &Path, device: SigningKey) -> io::Result<Self> {
        let root = state_root.join(STATE);
        adopt_earlier_state(&state_root.join(ROOMS), &root)?;
        let directory = Directory {
            states: Arc::new(Mutex::new(HashMap::new())),
        };
        let service = ChannelEncryption::open(
            ChannelEncryptionConfig {
                root: root.clone(),
                peers: Arc::new(ChannelsDir::new(&root)),
                vault: vault_key(&device),
                device_key: device,
            },
            Box::new(directory.clone()),
            Arc::new(NoGates),
        )?;
        let link = Link::new(directory);
        let park = Arc::clone(&link.park);
        let poke = Arc::clone(&park);
        let access = ChannelKeyAccess::with_wake(service, Arc::new(move || poke.poke()));
        let keys = ChannelKeyProvider::default();
        keys.set(Some(access.clone()));
        let pump = GovernancePump::spawn(
            "tilo-relay-channels",
            park,
            Some(access),
            Arc::clone(&link) as Arc<dyn GovernanceTransport + Send + Sync>,
            || {},
        )?;
        Ok(Self {
            device: Arc::new(Keys(keys)),
            link,
            _pump: pump,
            following: Mutex::new(Vec::new()),
        })
    }

    /// The device's channel service as the workspace host asks for it: its
    /// keys, and the sink the channel governance its sessions hear is handed
    /// to.
    pub(crate) fn device(&self) -> DeviceChannels {
        DeviceChannels {
            keys: Arc::clone(&self.device),
            sink: Some(Arc::clone(&self.link) as Arc<dyn ChannelSink>),
        }
    }

    /// Retain what this service settles for `workspaces`, and read who may
    /// read their channels from the governance each one notes as it settles
    /// ([`ChannelSink::settled`]).
    pub(crate) fn attach(&self, workspaces: &Workspaces) {
        let (publish, mut outgoing) = tokio::sync::mpsc::unbounded_channel();
        *lock(&self.link.publish) = Some(publish);
        let writing = workspaces.clone();
        let publisher = tokio::spawn(async move {
            while let Some(message) = outgoing.recv().await {
                let Some(workspace) = channel_workspace(&message) else {
                    continue;
                };
                let workspace = workspace.to_owned();
                if let Err(error) = writing
                    .publish_channel_governance(&workspace, vec![message])
                    .await
                {
                    tracing::warn!(%workspace, %error, "the relay's channel governance was not retained");
                }
            }
        });
        lock(&self.following).push(publisher);
        self.link.park.poke();
    }
}

impl Drop for Channels {
    fn drop(&mut self) {
        let following = std::mem::take(&mut *lock(&self.following));
        for task in following {
            task.abort();
        }
    }
}

/// The device's channel keys, as the workspace host asks for them.
struct Keys(ChannelKeyProvider);

impl ChannelKeys for Keys {
    fn channel_keys<'a>(
        &'a self,
        workspace: &'a str,
        route: &'a ChannelRoute,
        create: bool,
    ) -> ChannelKeysAnswerFuture<'a> {
        let keys = self.0.clone();
        let (workspace, route) = (workspace.to_owned(), route.clone());
        Box::pin(async move {
            if keys.get().is_none() {
                return Some(Err("this device's channel service is not running".into()));
            }
            keys.channel_keys(&workspace, &route, create, OPEN_WAIT)
                .await
        })
    }
}

impl ChannelSink for Link {
    fn take(&self, messages: Vec<NeighbourMsg>) {
        let mut inbound = lock(&self.inbound);
        inbound.extend(
            messages
                .into_iter()
                .filter_map(InboundChannelGovernance::from_message),
        );
        drop(inbound);
        self.park.poke();
    }

    fn settled(&self, workspace: &str, state: StatePeer) {
        self.directory.note(workspace, state);
        self.park.poke();
    }
}

/// The workspace transport the relay's channel governance travels on: what its
/// members sent, waiting for the next pass, and what the pass settles, on its
/// way to the workspace's store.
struct Link {
    /// Who may read the channels of the workspaces this relay governs, as each
    /// noted its governance when it settled.
    directory: Directory,
    /// Governance the workspace host's sessions carried here, waiting for the
    /// next pass.
    inbound: Mutex<Vec<InboundChannelGovernance>>,
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

    fn take_inbound(&self) -> Vec<InboundChannelGovernance> {
        std::mem::take(&mut *lock(&self.inbound))
    }
}

/// Who may read each channel of the workspaces the relay governs, as their
/// governance last stood.
#[derive(Clone)]
struct Directory {
    /// Each workspace's state as it last settled, by workspace UUID: a handle
    /// to the same document the workspace holds. Shared, so the pump reads it
    /// on its own thread without reaching for the workspace.
    states: Arc<Mutex<HashMap<String, StatePeer>>>,
}

impl Directory {
    /// `workspace`'s state, as the workspace noted it when it settled.
    fn note(&self, workspace: &str, state: StatePeer) {
        lock(&self.states).insert(workspace.to_owned(), state);
    }
}

impl ChannelDirectory for Directory {
    fn allowed_devices(
        &self,
        workspace: WorkspaceId,
        channel: ChannelId,
        route: Option<&ChannelRoute>,
        now: u64,
    ) -> Option<BTreeSet<Ed25519PublicKey>> {
        let at = UNIX_EPOCH.checked_add(Duration::from_secs(now))?;
        let state = lock(&self.states).get(&workspace.to_string())?.clone();
        // Governance that cannot be read says nothing of who reads it.
        let readers = state
            .governance()
            .channel_readers(at, channel, route)
            .ok()
            .flatten()?;
        Some(
            readers
                .into_iter()
                .map(Ed25519PublicKey::from_bytes)
                .collect(),
        )
    }
    fn unit_scope(
        &self,
        workspace: WorkspaceId,
        route: &ChannelRoute,
        now: u64,
    ) -> Option<ChannelId> {
        let at = UNIX_EPOCH.checked_add(Duration::from_secs(now))?;
        let state = lock(&self.states).get(&workspace.to_string())?.clone();
        state.governance().unit_scope(at, route).ok()
    }
}

/// The workspace a channel governance message names.
fn channel_workspace(message: &NeighbourMsg) -> Option<&str> {
    match message {
        NeighbourMsg::MlsGovernance { workspace, .. }
        | NeighbourMsg::MlsAccessRequest { workspace, .. } => Some(workspace),
        NeighbourMsg::MlsRoutedAccessRequest { room, .. } => Some(room),
        _ => None,
    }
}

/// The key sealing the relay's channel state: derived from the device key the
/// relay already signs with, which is what sealed the groups it kept before the
/// shared channel service, so those open still.
fn vault_key(device: &SigningKey) -> VaultKey {
    VaultKey::new(blake3::derive_key(
        "tilo-relay governed room channel vault v1",
        &device.to_bytes(),
    ))
}

/// Move the channel state an earlier relay kept per workspace, beside the
/// workspaces it governs (`<rooms>/<workspace>.channels`), into the shared
/// service's layout, so the groups those workspaces already have are read with
/// the key they were sealed with and their records stay readable.
fn adopt_earlier_state(state_root: &Path, root: &Path) -> io::Result<()> {
    let Ok(entries) = std::fs::read_dir(state_root) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some(STATE) {
            continue;
        }
        let Some(workspace) = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .and_then(|workspace| uuid::Uuid::parse_str(workspace).ok())
        else {
            continue;
        };
        let target = root.join(workspace.hyphenated().to_string()).join(STATE);
        std::fs::create_dir_all(&target)?;
        for file in std::fs::read_dir(&path)?.flatten() {
            let from = file.path();
            if !matches!(
                from.extension().and_then(|extension| extension.to_str()),
                Some("peer" | "route")
            ) {
                continue;
            }
            let Some(name) = from.file_name() else {
                continue;
            };
            std::fs::rename(&from, target.join(name))?;
        }
        // What it held moved; a file it did not is left where it is.
        let _ = std::fs::remove_dir(&path);
    }
    Ok(())
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
    use super::{ChannelDirectory, Channels, STATE, SigningKey, adopt_earlier_state};

    #[test]
    fn the_channel_state_an_earlier_relay_kept_moves_into_the_shared_service() {
        let state = tempfile::tempdir().unwrap();
        let workspace = uuid::Uuid::new_v4();
        let rooms = state.path().join(super::ROOMS);
        std::fs::create_dir_all(&rooms).unwrap();
        let earlier = rooms.join(format!("{workspace}.channels"));
        std::fs::create_dir_all(&earlier).unwrap();
        std::fs::write(earlier.join("11.peer"), b"sealed").unwrap();
        std::fs::write(earlier.join("11.route"), b"route").unwrap();
        std::fs::write(earlier.join("notes"), b"kept").unwrap();
        let root = state.path().join(STATE);
        adopt_earlier_state(&rooms, &root).unwrap();
        let moved = root.join(workspace.to_string()).join(STATE);
        assert_eq!(std::fs::read(moved.join("11.peer")).unwrap(), b"sealed");
        assert_eq!(std::fs::read(moved.join("11.route")).unwrap(), b"route");
        assert!(!earlier.join("11.peer").exists());
        assert_eq!(std::fs::read(earlier.join("notes")).unwrap(), b"kept");
    }

    #[test]
    fn a_relay_that_may_own_workspaces_takes_part_in_their_channels() {
        let state = tempfile::tempdir().unwrap();
        let device = SigningKey::from_bytes(&[7; 32]);
        let channels = Channels::open(state.path(), device).unwrap();
        assert!(state.path().join(STATE).is_dir());
        let device = channels.device();
        assert!(device.sink.is_some());
        assert!(
            channels
                .link
                .directory
                .allowed_devices(
                    tilo_desktop_model::WorkspaceId::new(),
                    collab_types::ChannelId(uuid::Uuid::new_v4()),
                    None,
                    0,
                )
                .is_none()
        );
    }
}
