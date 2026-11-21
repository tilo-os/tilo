//! This device's side of every encrypted group the profile takes part in.
//!
//! The device's key service runs beside its own workspace service
//! ([`KeyService`]): in the profile's own workspace, which that service governs, it is the
//! group's founder and the service asks it for the keys the apps' records seal with, so they get
//! their route keys without any network. In a workspace another device serves it is one key
//! holder among the workspace's devices: what it asks for and commits travels on the member
//! copy's link to the serving device, and what the groups' other devices send arrives on it. While
//! nobody who can admit it is online, groups this device already reads keep their keys and a new
//! request waits, sent again until a key holder answers.
use key_group::VaultKey;
use std::{
    io,
    path::Path,
    sync::{Arc, Mutex},
};
use tilo_desktop_model::WorkspaceId;
use tilo_identity::provisioning::IdentityStorage;
use tilo_workspace_service::apps::collab_core::{BrokerChannels, BrokerSealChecks};
use tilo_workspace_service::own_device::KeyService;

/// Sealed group state, and where a Linux profile keeps the key that seals
/// it (owner-only).
const GROUP_STATE: &str = "groups";
const GROUP_STATE_KEY: &str = "group-state.key";

/// Open this device's group state in `profile`, whose workspace `profile_workspace`
/// names the state's key, checking what the apps' channels seal against `broker`.
pub(super) fn open(
    profile: &Path,
    profile_workspace: WorkspaceId,
    storage: IdentityStorage,
    device_key: ed25519_dalek::SigningKey,
    broker: Arc<Mutex<BrokerChannels>>,
) -> io::Result<KeyService> {
    KeyService::open(
        "tilo-runtime-groups",
        &profile.join(GROUP_STATE),
        vault_key(profile, profile_workspace, storage)?,
        Arc::new(device_key),
        Arc::new(BrokerSealChecks(broker)),
    )
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
