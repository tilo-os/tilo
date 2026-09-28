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
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tilo_action_broker::{
    channel_encryption::{
        ChannelDirectory, ChannelEncryption, ChannelEncryptionConfig, ChannelKeyAccess,
        ChannelKeyProvider,
    },
    collab_core::BrokerChannels,
};
use tilo_desktop_model::RoomId;

/// Sealed channel state, and the key that seals it (owner-only; a platform
/// keychain is not used yet).
const CHANNEL_STATE: &str = "channels";
const CHANNEL_STATE_KEY: &str = "channel-state.key";
const PUMP_INTERVAL: Duration = Duration::from_millis(100);

/// The running channel service; stops when dropped.
pub(super) struct Channels {
    pub(super) keys: ChannelKeyProvider,
    stop: Arc<AtomicBool>,
}

impl Drop for Channels {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Open this device's channel state in `profile` and run its governance
/// over the joined rooms' sessions.
pub(super) fn open(
    profile: &Path,
    own_room: RoomId,
    device_key: ed25519_dalek::SigningKey,
    joined: Arc<Joined>,
    broker: Arc<Mutex<BrokerChannels>>,
) -> io::Result<Channels> {
    let device = Ed25519PublicKey::from_bytes(device_key.verifying_key().to_bytes());
    let service = ChannelEncryption::open(
        ChannelEncryptionConfig {
            root: profile.join(CHANNEL_STATE),
            vault: vault_key(&profile.join(CHANNEL_STATE_KEY))?,
            device_key,
        },
        Box::new(Directory {
            own_room,
            device,
            joined: Arc::clone(&joined),
        }),
        broker,
    )?;
    let access = ChannelKeyAccess::new(service);
    let keys = ChannelKeyProvider::default();
    keys.set(Some(access.clone()));
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = Arc::clone(&stop);
    std::thread::Builder::new()
        .name("tilo-runtime-channels".into())
        .spawn(move || {
            while !stopped.load(Ordering::Relaxed) {
                access.pump(&*joined);
                std::thread::sleep(PUMP_INTERVAL);
            }
        })?;
    Ok(Channels { keys, stop })
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

/// The key sealing the channel state, created once in an owner-only file.
fn vault_key(path: &Path) -> io::Result<VaultKey> {
    match std::fs::read(path) {
        Ok(bytes) => {
            let key: [u8; 32] = bytes.try_into().map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "invalid channel state key")
            })?;
            Ok(VaultKey::new(key))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut key = [0; 32];
            rand_core::Rng::fill_bytes(
                &mut getrandom::rand_core::UnwrapErr(getrandom::SysRng),
                &mut key,
            );
            atomic_file::write_with_mode(path, &key, 0o600)?;
            Ok(VaultKey::new(key))
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
#[path = "channels_tests.rs"]
mod tests;
