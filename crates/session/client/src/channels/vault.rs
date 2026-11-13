//! This device's MLS vault: the master key the platform secret store keeps for collaboration, and
//! the channel state it seals.
//! The vault hands the channel encryption service ([`super::Channels`]) one key derived from the
//! master key and the store where sealed channel state lives. MLS group membership follows the
//! workspace's access policy through that service's participants; nothing here writes membership.

use std::sync::Arc;

use getrandom::{SysRng, rand_core::UnwrapErr};
use sha2::{Digest as _, Sha256};
use zeroize::Zeroizing;

use tilo_channels::ChannelState;

/// This device's vault: the master key the platform secret store keeps for collaboration, and the
/// store it seals. Only ciphertext is written to it.
pub struct MlsVault {
    state: Arc<dyn ChannelState>,
    master_key: Zeroizing<[u8; 32]>,
}

impl MlsVault {
    /// The vault `state` seals, its master key kept by the platform secret store under `locator`
    /// and created on this device's first use.
    ///
    /// # Errors
    /// The secret store is absent or locked, or holds a key this vault cannot read.
    pub fn open(state: Arc<dyn ChannelState>, locator: &str) -> Result<Self, String> {
        let kept = state.holds_channel_state()?;
        let master_key = match secret_service_load(locator)? {
            Some(key) => key,
            None if kept => {
                return Err(
                    "this device holds channel state whose master key the secret store no longer \
                     keeps; a new key would open none of it"
                        .to_owned(),
                );
            }
            None => {
                let mut bytes = Zeroizing::new([0_u8; 32]);
                rand_core::Rng::fill_bytes(&mut UnwrapErr(SysRng), bytes.as_mut());
                secret_service_store(locator, bytes.as_ref())?;
                let verified = secret_service_load(locator)?
                    .ok_or_else(|| "the secret store did not return the new MLS key".to_owned())?;
                if verified.as_slice() != bytes.as_slice() {
                    return Err(
                        "the secret store did not return the newly stored MLS key".to_owned()
                    );
                }
                verified
            }
        };
        Ok(Self { state, master_key })
    }

    /// Open the vault of the session the launcher started: its state root under the session's own
    /// state directory. No home directory fallback is accepted because that would weaken key
    /// custody.
    ///
    /// # Errors
    /// The launcher named no state directory, or the vault material cannot be opened.
    /// `device` is the device key this session runs on: channel state made for another device is
    /// not opened as this one's ([`claim_for_device`]).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn open_for_session(device: &[u8; 32]) -> Result<Self, String> {
        let state = tilo_paths::env_dir("XDG_STATE_HOME").ok_or_else(|| {
            "the launcher did not provide XDG_STATE_HOME for MLS state".to_owned()
        })?;
        Self::open_at(&state.join("tilo/collaboration/mls"), device)
    }

    /// The vault at `root`, for the device `device` names.
    #[cfg(not(target_arch = "wasm32"))]
    fn open_at(root: &std::path::Path, device: &[u8; 32]) -> Result<Self, String> {
        if claim_for_device(root, device)? {
            tracing::info!(
                "the channel state here was made for the device this machine was copied from; \
                 this device joins its workspaces' channels as itself"
            );
        }
        let locator = master_key_locator(root);
        Self::open(
            Arc::new(tilo_channels::ChannelsDir::new(root.join("channels"))),
            &locator,
        )
    }

    /// Where this device keeps its sealed channel state.
    pub fn channel_state(&self) -> Arc<dyn ChannelState> {
        Arc::clone(&self.state)
    }

    /// The key sealing this device's channel state, derived from the master key under its own
    /// domain so it seals nothing else.
    pub fn channel_state_key(&self) -> channel_crypto::VaultKey {
        channel_crypto::VaultKey::new(self.channel_state_key_bytes())
    }

    /// The bytes of [`Self::channel_state_key`]. Every device's sealed channel state depends on
    /// this derivation staying byte-identical.
    fn channel_state_key_bytes(&self) -> [u8; 32] {
        let mut hash = Sha256::new();
        hash.update(b"tilo.channel-state-key.v1");
        hash.update(self.master_key.as_slice());
        hash.finalize().into()
    }
}

/// The file beside the channel state naming the device it was made for.
#[cfg(not(target_arch = "wasm32"))]
const DEVICE_FILE: &str = "device";

/// Make the channel state under `root` the state of `device`, the device key this session runs on:
/// state another device made is dropped. Every group in it names that device as its member, so no
/// re-seal makes it this one's; kept, a machine copied from another would open its groups as the
/// device it was copied from, and read nothing either of them writes after the first change of
/// keys. Dropped, this device joins its workspaces' channels as itself, admitted by their
/// participants, as a relay does when its machine is re-keyed. The master key stays: it seals
/// nothing that names a device. State from before the file named its device is taken as this
/// device's. Answers whether state was dropped.
#[cfg(not(target_arch = "wasm32"))]
fn claim_for_device(root: &std::path::Path, device: &[u8; 32]) -> Result<bool, String> {
    let named = root.join(DEVICE_FILE);
    let held = match std::fs::read(&named) {
        Ok(held) => Some(held),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(format!("{}: {error}", named.display())),
    };
    if held.as_deref() == Some(device.as_slice()) {
        return Ok(false);
    }
    let dropped = held.is_some();
    if dropped {
        let channels = root.join("channels");
        match std::fs::remove_dir_all(&channels) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("{}: {error}", channels.display())),
        }
    }
    std::fs::create_dir_all(root).map_err(|error| format!("{}: {error}", root.display()))?;
    let staged = root.join(".device.new");
    std::fs::write(&staged, device)
        .and_then(|()| std::fs::rename(&staged, &named))
        .map_err(|error| format!("{}: {error}", named.display()))?;
    Ok(dropped)
}

/// Where the secret store keeps a vault's master key, named for the directory it seals: the one
/// earlier builds used, so a device that kept its sealed channel state has it opened here, never
/// replaced.
#[cfg(not(target_arch = "wasm32"))]
pub fn master_key_locator(root: &std::path::Path) -> String {
    format!(
        "mls-master-{}",
        hex::encode(Sha256::digest(root.as_os_str().as_encoded_bytes()))
    )
}

/// The platform secret store: the Secret Service on Linux, the login Keychain on macOS, or the
/// sealed file store a headless machine names with `TILO_SECRETS_DIR` .
fn mls_secrets() -> tilo_secrets::SecretStore {
    tilo_secrets::SecretStore::for_app(tilo_channels::VAULT_SECRET_APP)
}

fn secret_service_store(locator: &str, secret: &[u8]) -> Result<(), String> {
    let options = tilo_secrets::PutOptions {
        label: Some("Tilo MLS key".into()),
    };
    mls_secrets()
        .put_blocking(locator, secret, options)
        .map_err(|error| format!("MLS key storage refused: {error}"))
}

/// The vault key `locator` names, or `None` when the store holds none.
fn secret_service_load(locator: &str) -> Result<Option<Zeroizing<[u8; 32]>>, String> {
    let Some(secret) = mls_secrets()
        .get_blocking(locator)
        .map_err(|error| format!("MLS key lookup unavailable: {error}"))?
    else {
        return Ok(None);
    };
    let bytes = Zeroizing::new(secret.expose().to_vec());
    let key: [u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| "stored MLS key has invalid length".to_owned())?;
    Ok(Some(Zeroizing::new(key)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Keep this thread's secrets in memory and give the vault at `root` a known master key, so
    /// `open` neither reaches the user's secret store nor generates a fresh key.
    fn seeded(
        root: &std::path::Path,
        key: [u8; 32],
    ) -> (Arc<dyn ChannelState>, String, tilo_secrets::SecretsScope) {
        let secrets = tilo_secrets::scope_thread(tilo_secrets::memory_backend());
        let locator = master_key_locator(root);
        secret_service_store(&locator, &key).expect("the master key");
        (
            Arc::new(tilo_channels::ChannelsDir::new(root.join("channels"))),
            locator,
            secrets,
        )
    }

    #[test]
    fn a_vault_opens_on_the_key_it_stored_and_keeps_it() {
        let dir = tempfile::tempdir().expect("a directory");
        let root = dir.path().join("mls");
        let (state, locator, _secrets) = seeded(&root, [6; 32]);

        let vault = MlsVault::open(Arc::clone(&state), &locator)
            .expect("the vault opens on the master key it kept");

        assert_eq!(
            hex::encode(vault.channel_state_key_bytes()),
            "9e51a75e84b0a7930f1865b7c7a8e92e60cab50b0b054df9b72029b7604a1ae5",
            "the derived key is the one channel state is sealed with"
        );
        let reopened = MlsVault::open(state, &locator).expect("the vault opens again");
        assert_eq!(
            reopened.channel_state_key_bytes(),
            vault.channel_state_key_bytes(),
            "and the master key is kept"
        );
    }

    #[test]
    fn a_vault_whose_state_is_kept_refuses_a_new_key() {
        let dir = tempfile::tempdir().expect("a directory");
        let root = dir.path().join("mls");
        let workspace = root
            .join("channels")
            .join("302d59e0-f586-42e1-a59e-3aa613f0f866");
        fs::create_dir_all(workspace.join("channels")).expect("the channel state");
        fs::write(workspace.join("channels/0b6f.peer"), [1]).expect("a peer");
        let _secrets = tilo_secrets::scope_thread(tilo_secrets::memory_backend());

        let state: Arc<dyn ChannelState> =
            Arc::new(tilo_channels::ChannelsDir::new(root.join("channels")));

        assert!(
            MlsVault::open(state, &master_key_locator(&root)).is_err(),
            "a new key would open none of the state sealed under the lost one"
        );
    }

    /// A machine copied from another carries the channel state made for the source's device: the
    /// copy, re-keyed, drops it and joins as itself, and keeps what it makes from then on.
    #[test]
    fn channel_state_made_for_another_device_is_not_opened_as_this_ones() {
        let dir = tempfile::tempdir().expect("a directory");
        let root = dir.path().join("mls");
        let (_, _, _secrets) = seeded(&root, [6; 32]);
        let (source, copy) = ([1_u8; 32], [2_u8; 32]);

        MlsVault::open_at(&root, &source).expect("the source's vault opens");
        fs::create_dir_all(root.join("channels")).expect("the channel state");
        fs::write(root.join("channels/group.sealed"), [6]).expect("the source's group");
        MlsVault::open_at(&root, &source).expect("the source's vault opens again");
        assert!(
            root.join("channels/group.sealed").exists(),
            "the source keeps its own state"
        );

        let vault = MlsVault::open_at(&root, &copy).expect("the copy's vault opens");
        assert!(
            !root.join("channels/group.sealed").exists(),
            "the copy does not open the source's groups as its own"
        );
        fs::create_dir_all(root.join("channels")).expect("the copy's channel state");
        fs::write(root.join("channels/group.sealed"), [7]).expect("the copy's group");
        let reopened = MlsVault::open_at(&root, &copy).expect("the copy's vault opens again");
        assert!(
            root.join("channels/group.sealed").exists(),
            "and keeps what it made itself"
        );
        assert_eq!(
            reopened.channel_state_key_bytes(),
            vault.channel_state_key_bytes(),
            "the master key stays"
        );
    }
}
