//! This device's side of every encrypted channel the profile takes part in.
//!
//! The runtime stewards the channels of its own room: it governs that room
//! and is its only reader, so its apps get their route keys without any
//! network. In a joined room the room's governing device stewards; this
//! device asks it for access over the room's relay session. While that
//! device is offline, channels this device already reads keep their keys
//! and a new request waits, redelivered until the steward answers; no host
//! ever holds a channel key.
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
        ChannelDirectory, ChannelEncryption, ChannelEncryptionConfig, ChannelKeyAccess,
        ChannelKeyProvider, GovernanceTransport,
    },
    collab_core::BrokerChannels,
};
use tilo_desktop_model::RoomId;
use tilo_identity_host::provisioning::IdentityStorage;

/// Sealed channel state, and where a Linux profile keeps the key that seals
/// it (owner-only).
const CHANNEL_STATE: &str = "channels";
const CHANNEL_STATE_KEY: &str = "channel-state.key";
/// How often governance is pumped while a channel is live: steward
/// reconciliation and redelivery run on deadlines, as on the desktop's
/// drainer. With no channel the pump parks until woken.
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
        let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        *pending = true;
        drop(pending);
        self.ready.notify_one();
    }

    /// Sleep until poked, or for at most `timeout` when there is one.
    fn wait(&self, timeout: Option<Duration>) {
        let pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
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

/// Open this device's channel state in `profile` and run its governance
/// over the joined rooms' sessions.
pub(super) fn open(
    profile: &Path,
    own_room: RoomId,
    storage: IdentityStorage,
    device_key: ed25519_dalek::SigningKey,
    joined: Arc<Joined>,
    broker: Arc<Mutex<BrokerChannels>>,
) -> io::Result<Channels> {
    let device = Ed25519PublicKey::from_bytes(device_key.verifying_key().to_bytes());
    let service = ChannelEncryption::open(
        ChannelEncryptionConfig {
            root: profile.join(CHANNEL_STATE),
            vault: vault_key(profile, own_room, storage)?,
            device_key,
        },
        Box::new(Directory {
            own_room,
            device,
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
    own_room: RoomId,
    device: Ed25519PublicKey,
    joined: Arc<Joined>,
}

impl ChannelDirectory for Directory {
    fn governs(&self, room: RoomId) -> bool {
        room == self.own_room
    }

    fn steward_of(&self, room: RoomId) -> Option<Ed25519PublicKey> {
        self.joined.steward_of(room)
    }

    /// Only the steward asks; this device stewards only its own room, which
    /// it alone reads.
    fn allowed_devices(&self, room: RoomId, _now: u64) -> BTreeSet<Ed25519PublicKey> {
        if room == self.own_room {
            BTreeSet::from([self.device])
        } else {
            BTreeSet::new()
        }
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
