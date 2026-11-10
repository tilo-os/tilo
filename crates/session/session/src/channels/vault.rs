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
pub(crate) struct MlsVault {
    state: Arc<dyn ChannelState>,
    master_key: Zeroizing<[u8; 32]>,
}

impl MlsVault {
    /// The vault `state` seals, its master key kept by the platform secret store under `locator`
    /// and created on this device's first use.
    ///
    /// # Errors
    /// The secret store is absent or locked, or holds a key this vault cannot read.
    pub(crate) fn open(state: Arc<dyn ChannelState>, locator: &str) -> Result<Self, String> {
        let existed = state.open_vault()?;
        let master_key = match secret_service_load(locator) {
            Ok(bytes) => bytes,
            Err(error) if existed => return Err(error),
            Err(_) => {
                let mut bytes = Zeroizing::new([0_u8; 32]);
                rand_core::Rng::fill_bytes(&mut UnwrapErr(SysRng), bytes.as_mut());
                secret_service_store(locator, bytes.as_ref())?;
                let verified = secret_service_load(locator)?;
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
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn open_for_session() -> Result<Self, String> {
        let state = tilo_paths::env_dir("XDG_STATE_HOME").ok_or_else(|| {
            "the launcher did not provide XDG_STATE_HOME for MLS state".to_owned()
        })?;
        let root = state.join("tilo/collaboration/mls");
        let locator = master_key_locator(&root);
        Self::open(
            Arc::new(tilo_channels::ChannelsDir::new(root.join("channels")).with_vault(root)),
            &locator,
        )
    }

    /// Where this device keeps its sealed channel state.
    pub(crate) fn channel_state(&self) -> Arc<dyn ChannelState> {
        Arc::clone(&self.state)
    }

    /// The key sealing this device's channel state, derived from the master key under its own
    /// domain so it seals nothing else.
    pub(crate) fn channel_state_key(&self) -> channel_crypto::VaultKey {
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

/// Where the secret store keeps a vault's master key, named for the directory it seals: the one
/// earlier builds used, so a device that kept its sealed channel state has it opened here, never
/// replaced.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn master_key_locator(root: &std::path::Path) -> String {
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

fn secret_service_load(locator: &str) -> Result<Zeroizing<[u8; 32]>, String> {
    let stored = mls_secrets()
        .get_blocking(locator)
        .map_err(|error| format!("MLS key lookup unavailable: {error}"))?;
    let bytes = if let Some(secret) = stored {
        Zeroizing::new(secret.expose().to_vec())
    } else {
        let legacy =
            legacy_secret_tool_load(locator)?.ok_or_else(|| "MLS key unavailable".to_owned())?;
        secret_service_store(locator, &legacy)?;
        legacy
    };
    let key: [u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| "stored MLS key has invalid length".to_owned())?;
    Ok(Zeroizing::new(key))
}

/// A key stored by earlier builds through `secret-tool` under their own attributes; `None` when
/// there is none (or no `secret-tool` ). The caller moves it into the platform store.
#[cfg(not(target_arch = "wasm32"))]
fn legacy_secret_tool_load(locator: &str) -> Result<Option<Zeroizing<Vec<u8>>>, String> {
    let Ok(output) = bounded_command::run(
        "secret-tool",
        &["lookup", "tilo-purpose", "mls", "tilo-mls-key", locator],
        None,
        64 * 1024,
        std::time::Duration::from_secs(10),
    ) else {
        return Ok(None);
    };
    if !output.status.success() {
        return Ok(None);
    }
    let stdout = Zeroizing::new(output.stdout);
    let decoded = Zeroizing::new(
        hex::decode(String::from_utf8_lossy(stdout.as_slice()).trim())
            .map_err(|_| "the Secret Service returned a malformed MLS key".to_owned())?,
    );
    Ok(Some(decoded))
}

#[cfg(target_arch = "wasm32")]
fn legacy_secret_tool_load(_locator: &str) -> Result<Option<Zeroizing<Vec<u8>>>, String> {
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// `registry.postcard` as builds that kept lifecycle records wrote it,
    /// holding one record of each retired kind. Generated from those types; the
    /// old shell decodes the same file.
    fn registry_before_the_authority_was_dropped() -> Vec<u8> {
        let text = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../fixtures/registry_with_retired_records.postcard.hex"
        ));
        hex::decode(text.trim()).expect("the fixture is hex")
    }

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
            Arc::new(tilo_channels::ChannelsDir::new(root.join("channels")).with_vault(root)),
            locator,
            secrets,
        )
    }

    #[test]
    fn registry_from_previous_layout_still_opens() {
        let dir = tempfile::tempdir().expect("a directory");
        let root = dir.path().join("mls");
        fs::create_dir_all(&root).expect("the vault root");
        fs::write(
            root.join("registry.postcard"),
            registry_before_the_authority_was_dropped(),
        )
        .expect("the registry");
        let (state, locator, _secrets) = seeded(&root, [6; 32]);

        let vault = MlsVault::open(state, &locator).expect("the vault opens");

        assert_eq!(
            hex::encode(vault.channel_state_key_bytes()),
            "9e51a75e84b0a7930f1865b7c7a8e92e60cab50b0b054df9b72029b7604a1ae5",
            "the derived key is the one earlier builds sealed channel state with"
        );
        assert_eq!(
            fs::read(root.join("registry.postcard")).expect("the registry"),
            [0_u8; 4],
            "the retired records are gone (four empty maps) and the marker stays"
        );
        let reopened =
            MlsVault::open(vault.channel_state(), &locator).expect("the vault opens again");
        assert_eq!(
            reopened.channel_state_key_bytes(),
            vault.channel_state_key_bytes(),
            "and the master key is kept"
        );
    }

    #[test]
    fn a_vault_whose_key_is_gone_refuses_a_new_one() {
        let dir = tempfile::tempdir().expect("a directory");
        let root = dir.path().join("mls");
        fs::create_dir_all(&root).expect("the vault root");
        fs::write(root.join("registry.postcard"), [1, 2, 3]).expect("the marker");
        let _secrets = tilo_secrets::scope_thread(tilo_secrets::memory_backend());

        let state: Arc<dyn ChannelState> =
            Arc::new(tilo_channels::ChannelsDir::new(root.join("channels")).with_vault(&root));

        assert!(
            MlsVault::open(state, &master_key_locator(&root)).is_err(),
            "a new key would open none of the state sealed under the lost one"
        );
    }

    #[test]
    fn leftover_snapshot_files_are_removed_on_open() {
        let dir = tempfile::tempdir().expect("a directory");
        let root = dir.path().join("mls");
        fs::create_dir_all(root.join("authority")).expect("the retired ledger");
        fs::write(root.join("authority/authority.postcard"), [1, 2, 3]).expect("the ledger");
        fs::write(root.join("snapshot-aa.op"), [5]).expect("a compaction envelope");
        fs::write(root.join("group-11.mls"), [8]).expect("a group");
        fs::write(root.join("key-package-22.mls"), [9]).expect("a key package");
        fs::create_dir_all(root.join("channels")).expect("the channel state");
        fs::write(root.join("channels/group.sealed"), [6]).expect("sealed channel state");
        fs::write(root.join("snapshot-notes.txt"), [7]).expect("a person's file");
        let (state, locator, _secrets) = seeded(&root, [6; 32]);

        MlsVault::open(state, &locator).expect("the vault opens");

        assert!(!root.join("snapshot-aa.op").exists());
        assert!(!root.join("group-11.mls").exists());
        assert!(!root.join("key-package-22.mls").exists());
        assert!(!root.join("authority").exists());
        assert!(
            root.join("channels/group.sealed").exists(),
            "sealed channel state is untouched"
        );
        assert!(root.join("snapshot-notes.txt").exists());
    }
}
