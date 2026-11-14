//! This device's side of every encrypted group the profile takes part in.
//!
//! In the profile's own workspace, which the workspace host this runtime runs hosts,
//! the host takes part in the groups as this device's one peer and asks
//! this service for their keys ([`DeviceGroupKeys`]), so its apps get
//! their route keys without any network. In a workspace
//! hosted elsewhere it is one participant among the workspace's member devices:
//! it asks them for access over the workspace's relay session, follows and
//! checks their commits against the workspace's governance, and commits itself
//! when the group falls behind the membership. While nobody who can admit
//! it is online, groups this device already reads keep their keys and a
//! new request waits, sent again until a participant answers.
use super::joined::Joined;
use collab_types::Ed25519PublicKey;
use key_group::VaultKey;
use std::{
    collections::BTreeSet,
    io,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
use tilo_action_broker::{
    collab_core::{BrokerChannels, BrokerGates},
    workspace_keys::{
        GovernancePump, KeyDir, KeyDirectory, KeyProvider, KeysConfig, SharedKeys, WorkspaceKeys,
    },
};
use tilo_desktop_model::WorkspaceId;
use tilo_identity_host::provisioning::IdentityStorage;

/// Sealed group state, and where a Linux profile keeps the key that seals
/// it (owner-only).
const GROUP_STATE: &str = "groups";
const GROUP_STATE_KEY: &str = "group-state.key";
/// How long a first write to a route's group waits for its group to be
/// created and admit this device: far more than the wait before this device
/// creates the group itself
/// (`tilo_action_broker::workspace_keys::founding_wait`) and the work a
/// first write does beside it, so a fresh workspace's first write is not
/// refused for keys that are about to arrive; keys that have not arrived in
/// half a minute are not about to.
const OPEN_WAIT: Duration = Duration::from_secs(30);
/// A first write must outlast the wait before the group is created here.
const _: () =
    assert!(OPEN_WAIT.as_secs() > tilo_action_broker::workspace_keys::founding_wait().as_secs());

/// The running key service; stops when dropped.
pub(super) struct KeyService {
    pub(super) keys: KeyProvider,
    _pump: GovernancePump,
}

/// Open this device's group state in `profile`, whose workspace `profile_workspace`
/// names the state's key, and run its governance over the joined workspaces'
/// sessions.
pub(super) fn open(
    profile: &Path,
    profile_workspace: WorkspaceId,
    storage: IdentityStorage,
    device_key: ed25519_dalek::SigningKey,
    joined: Arc<Joined>,
    broker: Arc<Mutex<BrokerChannels>>,
) -> io::Result<KeyService> {
    let service = WorkspaceKeys::open(
        KeysConfig {
            state: Arc::new(KeyDir::new(profile.join(GROUP_STATE))),
            vault: vault_key(profile, profile_workspace, storage)?,
            signer: Arc::new(device_key),
            role: tilo_action_broker::workspace_keys::KeyRole::Committer,
        },
        Box::new(Directory {
            joined: Arc::clone(&joined),
        }),
        Arc::new(BrokerGates(broker)),
    )?;
    let park = joined.governance_wake();
    let poke = Arc::clone(&park);
    let access = SharedKeys::with_wake(service, Arc::new(move || poke.poke()));
    let keys = joined.group_keys();
    keys.set(Some(access.clone()));
    let pump = GovernancePump::spawn("tilo-runtime-groups", park, Some(access), joined, || {})?;
    Ok(KeyService { keys, _pump: pump })
}

struct Directory {
    joined: Arc<Joined>,
}

impl KeyDirectory for Directory {
    fn allowed_devices(
        &self,
        workspace: WorkspaceId,
        now: u64,
    ) -> Option<BTreeSet<Ed25519PublicKey>> {
        self.joined.key_holders(workspace, now)
    }

    fn founds(&self, workspace: WorkspaceId) -> bool {
        self.joined.founded_here(workspace)
    }
}

/// The workspace host this runtime runs holds no group keys of its own: it
/// takes part in a workspace's groups as this device's one peer, asking this
/// device's key service for them ([`tilo_room_host::workspaces::GroupKeys`]),
/// so the host and the device never found two groups under one leaf.
pub(super) struct DeviceGroupKeys {
    keys: KeyProvider,
}

impl DeviceGroupKeys {
    /// This device's key service as the workspace host this runtime runs
    /// asks for it: the keys it opens and seals records with, and the sink
    /// the group governance its members' sessions carry is handed to, since
    /// the service runs here, on this same device.
    pub(super) fn device(
        keys: KeyProvider,
        joined: &Arc<Joined>,
    ) -> tilo_room_host::workspaces::DeviceKeyGroups {
        tilo_room_host::workspaces::DeviceKeyGroups {
            keys: Arc::new(Self { keys }),
            sink: Some(Arc::new(DeviceKeySink {
                joined: Arc::clone(joined),
            })),
        }
    }
}

/// Where the workspace host this runtime runs hands the group governance its
/// members send: this device's key service, which runs in this process.
struct DeviceKeySink {
    joined: Arc<Joined>,
}

impl tilo_room_host::workspaces::KeySink for DeviceKeySink {
    fn take(&self, messages: Vec<neighbour_wire::NeighbourMsg>) {
        self.joined.take_governance(messages);
    }

    /// Nothing to note: this device's key service reads the workspace's
    /// governance itself, through the session that holds it.
    fn settled(&self, _workspace: &str, _state: tilo_peer_state::StatePeer) {}
}

impl tilo_room_host::workspaces::GroupKeys for DeviceGroupKeys {
    fn group_keys<'a>(
        &'a self,
        workspace: &'a str,
        create: bool,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Option<Result<key_group::ContentKeys, String>>>
                + Send
                + 'a,
        >,
    > {
        let keys = self.keys.clone();
        Box::pin(async move {
            if keys.get().is_none() {
                return Some(Err("this device's key service is not running".into()));
            }
            keys.group_keys(workspace, create, OPEN_WAIT).await
        })
    }
}

/// The key sealing `workspace`'s group state in `profile`, created once: in the
/// login Keychain on macOS, else in an owner-only file.
fn vault_key(
    profile: &Path,
    workspace: WorkspaceId,
    storage: IdentityStorage,
) -> io::Result<VaultKey> {
    let file = profile.join(GROUP_STATE_KEY);
    #[cfg(target_os = "macos")]
    if storage == IdentityStorage::Platform {
        return keychain::vault_key(workspace).map(VaultKey::new);
    }
    let _ = (workspace, storage);
    if let Some(key) = read_key_file(&file)? {
        return Ok(VaultKey::new(key));
    }
    let key = random_key();
    tilo_files::native::atomic::write_with_mode(&file, &key, 0o600)?;
    Ok(VaultKey::new(key))
}

fn read_key_file(path: &Path) -> io::Result<Option<[u8; 32]>> {
    match std::fs::read(path) {
        Ok(bytes) => bytes
            .try_into()
            .map(Some)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid group state key")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn random_key() -> [u8; 32] {
    let mut key = [0; 32];
    rand_core::Rng::fill_bytes(
        &mut getrandom::rand_core::UnwrapErr(getrandom::SysRng),
        &mut key,
    );
    key
}

/// The group-state key as a Keychain generic password, created on first
/// use. A Keychain that refuses or fails fails the start; nothing falls back
/// to a file.
#[cfg(target_os = "macos")]
mod keychain {
    use super::{WorkspaceId, random_key};
    use std::io;
    use tilo_secrets::{PutOptions, SecretStore};

    const SECRETS_APP: &str = "so.tilo.runtime";

    pub(super) fn vault_key(workspace: WorkspaceId) -> io::Result<[u8; 32]> {
        let store = SecretStore::for_app(SECRETS_APP);
        let name = format!("group-state-v1.{workspace}");
        if let Some(key) = get(&store, &name)? {
            return Ok(key);
        }
        let key = random_key();
        store
            .put_blocking(
                &name,
                &key,
                PutOptions {
                    label: Some("Tilo group state key".into()),
                },
            )
            .map_err(|error| keychain_error(&error))?;
        if get(&store, &name)? != Some(key) {
            return Err(io::Error::other(
                "the login Keychain did not return the stored group state key",
            ));
        }
        Ok(key)
    }

    fn get(store: &SecretStore, name: &str) -> io::Result<Option<[u8; 32]>> {
        store
            .get_blocking(name)
            .map_err(|error| keychain_error(&error))?
            .map(|secret| {
                secret.expose().try_into().map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidData, "invalid group state key")
                })
            })
            .transpose()
    }

    fn keychain_error(error: &tilo_secrets::SecretError) -> io::Error {
        let kind = match error {
            tilo_secrets::SecretError::Denied => io::ErrorKind::PermissionDenied,
            _ => io::ErrorKind::Other,
        };
        io::Error::new(kind, format!("login Keychain, group state key: {error}"))
    }
}

#[cfg(test)]
#[path = "workspace_keys_tests.rs"]
mod tests;
