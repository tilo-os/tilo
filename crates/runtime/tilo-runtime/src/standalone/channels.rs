//! This device's side of every encrypted channel the profile takes part in.
//!
//! In the profile's own room, which the room host this runtime runs hosts,
//! the host takes part in the channels as this device's one peer and asks
//! this service for their keys ([`DeviceChannelKeys`]), so its apps get
//! their route keys without any network. In a room
//! hosted elsewhere it is one participant among the room's member devices:
//! it asks them for access over the room's relay session, follows and
//! checks their commits against the room's governance, and commits itself
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
    sync::{
        Arc, Condvar, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tilo_action_broker::{
    channel_encryption::{
        ChannelAccess, ChannelDirectory, ChannelEncryption, ChannelEncryptionConfig,
        ChannelKeyAccess, ChannelKeyProvider, GovernanceTransport,
    },
    collab_core::BrokerChannels,
};
use tilo_desktop_model::RoomId;
use tilo_identity_host::provisioning::IdentityStorage;
use tilo_lock::MutexRecover as _;

/// Sealed channel state, and where a Linux profile keeps the key that seals
/// it (owner-only).
const CHANNEL_STATE: &str = "channels";
const CHANNEL_STATE_KEY: &str = "channel-state.key";
/// How often governance is pumped while a channel is live: reconciliation
/// and requests run on deadlines, as on the desktop's drainer. With no channel the pump parks until woken.
const LIVE_INTERVAL: Duration = Duration::from_secs(1);

/// The running channel service; stops when dropped.
pub(super) struct Channels {
    pub(super) keys: ChannelKeyProvider,
    _pump: Pump,
}

/// Wakes the governance pump: a frame arrived, an endpoint opened a channel,
/// or the service stops. A poke that arrives before the pump waits is kept,
/// so a frame queued just before it parks is not left behind.
#[derive(Default)]
pub(super) struct PumpWake {
    pending: Mutex<bool>,
    ready: Condvar,
}

impl PumpWake {
    pub(super) fn poke(&self) {
        let mut pending = self.pending.lock_recover();
        *pending = true;
        drop(pending);
        self.ready.notify_one();
    }

    /// Sleep until poked, or for at most `timeout` when there is one.
    fn wait(&self, timeout: Option<Duration>) {
        let pending = self.pending.lock_recover();
        let mut pending = match timeout {
            Some(timeout) => {
                self.ready
                    .wait_timeout_while(pending, timeout, |pending| !*pending)
                    .unwrap_or_else(PoisonError::into_inner)
                    .0
            }
            None => self
                .ready
                .wait_while(pending, |pending| !*pending)
                .unwrap_or_else(PoisonError::into_inner),
        };
        *pending = false;
    }
}

/// The `tilo-runtime-channels` thread; stops when dropped.
struct Pump {
    wake: Arc<PumpWake>,
    stop: Arc<AtomicBool>,
}

impl Drop for Pump {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.wake.poke();
    }
}

/// Run `access`'s governance over `transport` on its own thread: once per
/// [`LIVE_INTERVAL`] while a channel is live, otherwise only when `wake` is
/// poked.
fn spawn_pump(
    access: ChannelKeyAccess,
    transport: Arc<dyn GovernanceTransport + Send + Sync>,
    wake: Arc<PumpWake>,
) -> io::Result<Pump> {
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = Arc::clone(&stop);
    let woken = Arc::clone(&wake);
    std::thread::Builder::new()
        .name("tilo-runtime-channels".into())
        .spawn(move || {
            while !stopped.load(Ordering::Relaxed) {
                access.pump(&*transport);
                let timeout = access.has_live_channels().then_some(LIVE_INTERVAL);
                woken.wait(timeout);
            }
        })?;
    Ok(Pump { wake, stop })
}

/// Open this device's channel state in `profile`, whose room `profile_room`
/// names the state's key, and run its governance over the joined rooms'
/// sessions.
pub(super) fn open(
    profile: &Path,
    profile_room: RoomId,
    storage: IdentityStorage,
    device_key: ed25519_dalek::SigningKey,
    joined: Arc<Joined>,
    broker: Arc<Mutex<BrokerChannels>>,
) -> io::Result<Channels> {
    let service = ChannelEncryption::open(
        ChannelEncryptionConfig {
            root: profile.join(CHANNEL_STATE),
            vault: vault_key(profile, profile_room, storage)?,
            device_key,
        },
        Box::new(Directory {
            joined: Arc::clone(&joined),
        }),
        broker,
    )?;
    let wake = joined.governance_wake();
    let poke = Arc::clone(&wake);
    let access = ChannelKeyAccess::with_wake(service, Arc::new(move || poke.poke()));
    let keys = joined.channel_keys();
    keys.set(Some(access.clone()));
    let pump = spawn_pump(access, joined, wake)?;
    Ok(Channels { keys, _pump: pump })
}

struct Directory {
    joined: Arc<Joined>,
}

impl ChannelDirectory for Directory {
    fn governor_of(&self, room: RoomId) -> Option<Ed25519PublicKey> {
        self.joined.governor_of(room)
    }

    /// The member devices the room's governance lets read the channel's
    /// record.
    fn allowed_devices(
        &self,
        room: RoomId,
        channel: collab_types::ChannelId,
        route: Option<&collab_types::ChannelRoute>,
        now: u64,
    ) -> Option<BTreeSet<Ed25519PublicKey>> {
        self.joined.channel_readers(room, channel, route, now)
    }
}

/// The room host this runtime runs holds no channel keys of its own: it
/// takes part in a room's channels as this device's one peer, asking this
/// device's channel service for them ([`tilo_room_host::rooms::ChannelKeys`]),
/// so the host and the device never found two groups under one leaf.
pub(super) struct DeviceChannelKeys {
    keys: ChannelKeyProvider,
}

impl DeviceChannelKeys {
    pub(super) fn handle(keys: ChannelKeyProvider) -> tilo_room_host::rooms::DeviceKeys {
        Arc::new(Self { keys })
    }
}

impl tilo_room_host::rooms::ChannelKeys for DeviceChannelKeys {
    fn channel_keys<'a>(
        &'a self,
        room: &'a str,
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
        let (room, route) = (room.to_owned(), route.clone());
        Box::pin(async move {
            // Asked off the caller's task: the service takes its lock and
            // reads its state on that thread, never on the host's.
            let asked = tokio::task::spawn_blocking(move || {
                let Some(access) = keys.get() else {
                    return Some(Err("this device's channel service is not running".into()));
                };
                let room = RoomId::parse(&room)?;
                if create {
                    match access.access_record(room, &route).0 {
                        ChannelAccess::Ready(keys) => Some(Ok(keys)),
                        ChannelAccess::Waiting | ChannelAccess::Removed => {
                            Some(Err("the channel's keys are not ready".into()))
                        }
                        ChannelAccess::Unavailable | ChannelAccess::NoSecretStore => None,
                    }
                } else {
                    access.record_keys(room, &route)
                }
            })
            .await;
            asked.unwrap_or_else(|error| Some(Err(error.to_string())))
        })
    }
}

/// The key sealing `room`'s channel state in `profile`, created once: in the
/// login Keychain on macOS, else in an owner-only file.
fn vault_key(profile: &Path, room: RoomId, storage: IdentityStorage) -> io::Result<VaultKey> {
    let file = profile.join(CHANNEL_STATE_KEY);
    #[cfg(target_os = "macos")]
    if storage == IdentityStorage::Platform {
        return keychain::vault_key(&file, room).map(VaultKey::new);
    }
    let _ = (room, storage);
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
    use super::{RoomId, random_key, read_key_file};
    use std::{io, path::Path};
    use tilo_secrets::{PutOptions, SecretStore};

    const SECRETS_APP: &str = "so.tilo.runtime";

    pub(super) fn vault_key(file: &Path, room: RoomId) -> io::Result<[u8; 32]> {
        let store = SecretStore::for_app(SECRETS_APP);
        let name = format!("channel-state-v1.{room}");
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
