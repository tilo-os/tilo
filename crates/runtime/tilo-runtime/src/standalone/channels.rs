//! This device's side of every encrypted channel the profile takes part in.
//!
//! In the profile's own workspace, which the workspace host this runtime runs hosts,
//! the host takes part in the channels as this device's one peer and asks
//! this service for their keys ([`DeviceChannelKeys`]), so its apps get
//! their route keys without any network. In a workspace
//! hosted elsewhere it is one participant among the workspace's member devices:
//! it asks them for access over the workspace's relay session, follows and
//! checks their commits against the workspace's governance, and commits itself
//! when the group falls behind the membership. While nobody who can admit
//! it is online, channels this device already reads keep their keys and a
//! new request waits, sent again until a participant answers.
use super::joined::Joined;
use channel_crypto::VaultKey;
use collab_types::Ed25519PublicKey;
use std::{
    collections::BTreeSet,
    io,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
use tilo_action_broker::{
    channel_encryption::{
        ChannelDirectory, ChannelEncryption, ChannelEncryptionConfig, ChannelKeyAccess,
        ChannelKeyProvider, ChannelsDir, GovernancePump,
    },
    collab_core::{BrokerChannels, BrokerGates},
};
use tilo_desktop_model::WorkspaceId;
use tilo_identity_host::provisioning::IdentityStorage;

/// Sealed channel state, and where a Linux profile keeps the key that seals
/// it (owner-only).
const CHANNEL_STATE: &str = "channels";
const CHANNEL_STATE_KEY: &str = "channel-state.key";
/// How long a first write to a route's channel waits for its group to be
/// created and admit this device: far more than the wait before this device
/// creates the group itself
/// (`tilo_action_broker::channel_encryption::founding_wait`) and the work a
/// first write does beside it, so a fresh workspace's first write is not
/// refused for keys that are about to arrive; keys that have not arrived in
/// half a minute are not about to.
const OPEN_WAIT: Duration = Duration::from_secs(30);
/// A first write must outlast the wait before the group is created here.
const _: () = assert!(
    OPEN_WAIT.as_secs() > tilo_action_broker::channel_encryption::founding_wait().as_secs()
);

/// The running channel service; stops when dropped.
pub(super) struct Channels {
    pub(super) keys: ChannelKeyProvider,
    _pump: GovernancePump,
}

/// Open this device's channel state in `profile`, whose workspace `profile_workspace`
/// names the state's key, and run its governance over the joined workspaces'
/// sessions.
pub(super) fn open(
    profile: &Path,
    profile_workspace: WorkspaceId,
    storage: IdentityStorage,
    device_key: ed25519_dalek::SigningKey,
    joined: Arc<Joined>,
    broker: Arc<Mutex<BrokerChannels>>,
) -> io::Result<Channels> {
    let service = ChannelEncryption::open(
        ChannelEncryptionConfig {
            state: Arc::new(ChannelsDir::new(profile.join(CHANNEL_STATE))),
            vault: vault_key(profile, profile_workspace, storage)?,
            signer: Arc::new(device_key),
            role: tilo_action_broker::channel_encryption::Role::Participant,
        },
        Box::new(Directory {
            joined: Arc::clone(&joined),
        }),
        Arc::new(BrokerGates(broker)),
    )?;
    let park = joined.governance_wake();
    let poke = Arc::clone(&park);
    let access = ChannelKeyAccess::with_wake(service, Arc::new(move || poke.poke()));
    let keys = joined.channel_keys();
    keys.set(Some(access.clone()));
    let pump = GovernancePump::spawn("tilo-runtime-channels", park, Some(access), joined, || {})?;
    Ok(Channels { keys, _pump: pump })
}

struct Directory {
    joined: Arc<Joined>,
}

impl ChannelDirectory for Directory {
    /// The member devices the workspace's governance lets read the channel's
    /// record.
    fn allowed_devices(
        &self,
        workspace: WorkspaceId,
        channel: collab_types::ChannelId,
        route: Option<&collab_types::ChannelRoute>,
        now: u64,
    ) -> Option<BTreeSet<Ed25519PublicKey>> {
        self.joined.channel_readers(workspace, channel, route, now)
    }

    /// The scope the record on `route` seals under, as its governance decides
    /// it: the workspace's, or the record's own when its readers are narrowed.
    fn unit_scope(
        &self,
        workspace: WorkspaceId,
        route: &collab_types::ChannelRoute,
        now: u64,
    ) -> Option<collab_types::ChannelId> {
        self.joined.unit_scope(workspace, route, now)
    }
}

/// The workspace host this runtime runs holds no channel keys of its own: it
/// takes part in a workspace's channels as this device's one peer, asking this
/// device's channel service for them ([`tilo_room_host::workspaces::ChannelKeys`]),
/// so the host and the device never found two groups under one leaf.
pub(super) struct DeviceChannelKeys {
    keys: ChannelKeyProvider,
}

impl DeviceChannelKeys {
    /// This device's channel service as the workspace host this runtime runs
    /// asks for it: the keys it opens and seals records with, and the sink
    /// the channel governance its members' sessions carry is handed to, since
    /// the service runs here, on this same device.
    pub(super) fn device(
        keys: ChannelKeyProvider,
        joined: &Arc<Joined>,
    ) -> tilo_room_host::workspaces::DeviceChannels {
        tilo_room_host::workspaces::DeviceChannels {
            keys: Arc::new(Self { keys }),
            sink: Some(Arc::new(DeviceChannelSink {
                joined: Arc::clone(joined),
            })),
        }
    }
}

/// Where the workspace host this runtime runs hands the channel governance its
/// members send: this device's channel service, which runs in this process.
struct DeviceChannelSink {
    joined: Arc<Joined>,
}

impl tilo_room_host::workspaces::ChannelSink for DeviceChannelSink {
    fn take(&self, messages: Vec<neighbour_wire::NeighbourMsg>) {
        self.joined.take_channel_governance(messages);
    }

    /// Nothing to note: this device's channel service reads the workspace's
    /// governance itself, through the session that holds it.
    fn settled(&self, _workspace: &str, _state: tilo_peer_state::StatePeer) {}
}

impl tilo_room_host::workspaces::ChannelKeys for DeviceChannelKeys {
    fn channel_keys<'a>(
        &'a self,
        workspace: &'a str,
        route: &'a collab_types::ChannelRoute,
        create: bool,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Option<Result<channel_crypto::ChannelContentKeys, String>>,
                > + Send
                + 'a,
        >,
    > {
        let keys = self.keys.clone();
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

/// The key sealing `workspace`'s channel state in `profile`, created once: in the
/// login Keychain on macOS, else in an owner-only file.
fn vault_key(
    profile: &Path,
    workspace: WorkspaceId,
    storage: IdentityStorage,
) -> io::Result<VaultKey> {
    let file = profile.join(CHANNEL_STATE_KEY);
    #[cfg(target_os = "macos")]
    if storage == IdentityStorage::Platform {
        return keychain::vault_key(&file, workspace).map(VaultKey::new);
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
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid channel state key")),
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

/// The channel-state key as a Keychain generic password. A key file an
/// earlier build wrote moves in once: stored, read back, then removed. A
/// Keychain that refuses or fails fails the start; nothing falls back to a
/// file.
#[cfg(target_os = "macos")]
mod keychain {
    use super::{WorkspaceId, random_key, read_key_file};
    use std::{io, path::Path};
    use tilo_secrets::{PutOptions, SecretStore};

    const SECRETS_APP: &str = "so.tilo.runtime";

    pub(super) fn vault_key(file: &Path, workspace: WorkspaceId) -> io::Result<[u8; 32]> {
        let store = SecretStore::for_app(SECRETS_APP);
        let name = format!("channel-state-v1.{workspace}");
        let stored = get(&store, &name)?;
        let legacy = read_key_file(file)?;
        let key = match (stored, legacy) {
            (Some(stored), Some(legacy)) if stored != legacy => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "the login Keychain and {} hold different channel state keys",
                        file.display()
                    ),
                ));
            }
            (Some(stored), _) => stored,
            (None, legacy) => {
                let key = legacy.unwrap_or_else(random_key);
                store
                    .put_blocking(
                        &name,
                        &key,
                        PutOptions {
                            label: Some("Tilo channel state key".into()),
                        },
                    )
                    .map_err(|error| keychain_error(&error))?;
                if get(&store, &name)? != Some(key) {
                    return Err(io::Error::other(
                        "the login Keychain did not return the stored channel state key",
                    ));
                }
                key
            }
        };
        if legacy.is_some() {
            std::fs::remove_file(file)?;
        }
        Ok(key)
    }

    fn get(store: &SecretStore, name: &str) -> io::Result<Option<[u8; 32]>> {
        store
            .get_blocking(name)
            .map_err(|error| keychain_error(&error))?
            .map(|secret| {
                secret.expose().try_into().map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidData, "invalid channel state key")
                })
            })
            .transpose()
    }

    fn keychain_error(error: &tilo_secrets::SecretError) -> io::Error {
        let kind = match error {
            tilo_secrets::SecretError::Denied => io::ErrorKind::PermissionDenied,
            _ => io::ErrorKind::Other,
        };
        io::Error::new(kind, format!("login Keychain, channel state key: {error}"))
    }
}

#[cfg(test)]
#[path = "channels_tests.rs"]
mod tests;
