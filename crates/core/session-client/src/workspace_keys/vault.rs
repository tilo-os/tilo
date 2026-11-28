//! This device's MLS vault: the master key the platform secret store keeps for collaboration, and
//! the group state it seals.
//! The vault hands the group encryption service one key derived from the
//! master key and the store where sealed group state lives. MLS group membership follows the
//! workspace's access policy through that service's members; nothing here writes membership.

use std::sync::Arc;

use getrandom::{SysRng, rand_core::UnwrapErr};
use sha2::{Digest as _, Sha256};
use zeroize::Zeroizing;

use tilo_workspace_keys::KeyStorage;

/// This device's vault: the master key the platform secret store keeps for collaboration, and the
/// store it seals. Only ciphertext is written to it.
pub struct MlsVault {
    state: Arc<dyn KeyStorage>,
    master_key: Zeroizing<[u8; 32]>,
}

impl MlsVault {
    /// The vault `state` seals, its master key kept by the platform secret store under `locator`
    /// and created on this device's first use.
    ///
    /// # Errors
    /// The secret store is absent or locked, or holds a key this vault cannot read.
    pub fn open(state: Arc<dyn KeyStorage>, locator: &str) -> Result<Self, String> {
        let kept = state.holds_key_state()?;
        let master_key = match secret_service_load(locator)? {
            Some(key) => key,
            None if kept => {
                return Err(
                    "this device holds group state whose master key the secret store no longer \
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
    /// ownership.
    ///
    /// # Errors
    /// The launcher named no state directory, or the vault material cannot be opened.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn open_for_session() -> Result<Self, String> {
        let state = tilo_paths::env_dir("XDG_STATE_HOME").ok_or_else(|| {
            "the launcher did not provide XDG_STATE_HOME for MLS state".to_owned()
        })?;
        let root = state.join("tilo/collaboration/mls");
        Self::open(
            Arc::new(tilo_workspace_keys::KeyDir::new(root.join("groups"))),
            &master_key_locator(&root),
        )
    }

    /// Where this device keeps its sealed group state.
    pub fn group_state(&self) -> Arc<dyn KeyStorage> {
        Arc::clone(&self.state)
    }

    /// The key sealing this device's group state, derived from the master key under its own
    /// domain so it seals nothing else.
    pub fn group_state_key(&self) -> key_group::VaultKey {
        key_group::VaultKey::new(self.group_state_key_bytes())
    }

    /// The bytes of [`Self::group_state_key`]. Every device's sealed group state depends on
    /// this derivation staying byte-identical.
    fn group_state_key_bytes(&self) -> [u8; 32] {
        let mut hash = Sha256::new();
        hash.update(b"tilo.group-state-key.v1");
        hash.update(self.master_key.as_slice());
        hash.finalize().into()
    }
}

/// Where the secret store keeps a vault's master key, named for the directory it seals: the one
/// earlier builds used, so a device that kept its sealed group state has it opened here, never
/// replaced.
#[cfg(not(target_arch = "wasm32"))]
pub fn master_key_locator(root: &std::path::Path) -> String {
    format!(
        "mls-master-{}",
        hex::encode(Sha256::digest(root.as_os_str().as_encoded_bytes()))
    )
}

/// The groups a desktop sealed under its own master key, before this device's daemon kept them,
/// as the desktop hands them over once ([`carried`]).
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct CarriedKeys {
    /// The key the groups' state was sealed with, in hex: derived from the desktop's master key
    /// under the group-state domain, so it opens that state and nothing else.
    pub sealed_with: String,
    /// Each file of the groups' directory by its name, its bytes in hex.
    pub groups: std::collections::BTreeMap<String, String>,
}

/// The groups the session started on `state` (its `XDG_STATE_HOME`) kept itself, with the key they
/// were sealed with: `None` when it kept none.
///
/// # Errors
/// The secret store does not answer, it lost the master key the groups were sealed with, or the
/// groups cannot be read.
#[cfg(not(target_arch = "wasm32"))]
pub fn carried(state: &std::path::Path) -> Result<Option<CarriedKeys>, String> {
    let root = state.join("tilo/collaboration/mls");
    let groups = root.join("groups");
    let mut files = std::collections::BTreeMap::new();
    match std::fs::read_dir(&groups) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry.map_err(|error| format!("{}: {error}", groups.display()))?;
                if !entry
                    .file_type()
                    .map_err(|error| error.to_string())?
                    .is_file()
                {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().into_owned();
                let bytes = std::fs::read(entry.path())
                    .map_err(|error| format!("{}: {error}", entry.path().display()))?;
                files.insert(name, hex::encode(bytes));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("{}: {error}", groups.display())),
    }
    let state: Arc<dyn KeyStorage> = Arc::new(tilo_workspace_keys::KeyDir::new(groups));
    let Some(master_key) = secret_service_load(&master_key_locator(&root))? else {
        if state.holds_key_state()? {
            return Err(
                "this desktop holds group state whose master key the secret store no longer keeps"
                    .to_owned(),
            );
        }
        return Ok(None);
    };
    let vault = MlsVault { state, master_key };
    Ok(Some(CarriedKeys {
        sealed_with: hex::encode(vault.group_state_key_bytes()),
        groups: files,
    }))
}

/// Forget what [`carried`] read under `state` once this device's daemon keeps it: the groups'
/// directory and the master key they were sealed with, so they are carried once.
///
/// # Errors
/// The directory or the key cannot be removed.
#[cfg(not(target_arch = "wasm32"))]
pub fn forget_carried(state: &std::path::Path) -> Result<(), String> {
    let root = state.join("tilo/collaboration/mls");
    match std::fs::remove_dir_all(&root) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("{}: {error}", root.display())),
    }
    mls_secrets()
        .delete_blocking(&master_key_locator(&root))
        .map_err(|error| format!("the desktop's MLS key: {error}"))
}

/// The platform secret store: the Secret Service on Linux, the login Keychain on macOS, or the
/// sealed file store a headless machine names with `TILO_SECRETS_DIR` .
fn mls_secrets() -> tilo_secrets::SecretStore {
    tilo_secrets::SecretStore::for_app(tilo_workspace_keys::VAULT_SECRET_APP)
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
    ) -> (Arc<dyn KeyStorage>, String, tilo_secrets::SecretsScope) {
        let secrets = tilo_secrets::scope_thread(tilo_secrets::memory_backend());
        let locator = master_key_locator(root);
        secret_service_store(&locator, &key).expect("the master key");
        (
            Arc::new(tilo_workspace_keys::KeyDir::new(root.join("groups"))),
            locator,
            secrets,
        )
    }

    tilo_test_env::scenario! {
        fn a_vault_opens_only_on_the_master_key_it_kept;

        phase a_vault_opens_on_the_key_it_stored_and_keeps_it;
        {
            let dir = tempfile::tempdir().expect("a directory");
            let root = dir.path().join("mls");
            let (state, locator, _secrets) = seeded(&root, [6; 32]);

            let vault = MlsVault::open(Arc::clone(&state), &locator)
                .expect("the vault opens on the master key it kept");

            assert_eq!(
                hex::encode(vault.group_state_key_bytes()),
                "8a488a4bdd1c84405a60bbbce6dd1fd05d98114eb33e5908862e25f48618d79d",
                "the derived key is the one group state is sealed with"
            );
            let reopened = MlsVault::open(state, &locator).expect("the vault opens again");
            assert_eq!(
                reopened.group_state_key_bytes(),
                vault.group_state_key_bytes(),
                "and the master key is kept"
            );
        }

        phase a_vault_whose_state_is_kept_refuses_a_new_key;
        {
            let dir = tempfile::tempdir().expect("a directory");
            let root = dir.path().join("mls");
            let workspace = root
                .join("groups")
                .join("302d59e0-f586-42e1-a59e-3aa613f0f866");
            fs::create_dir_all(workspace.join("groups")).expect("the group state");
            fs::write(workspace.join("groups/0b6f.peer"), [1]).expect("a peer");
            let _secrets = tilo_secrets::scope_thread(tilo_secrets::memory_backend());

            let state: Arc<dyn KeyStorage> =
                Arc::new(tilo_workspace_keys::KeyDir::new(root.join("groups")));

            assert!(
                MlsVault::open(state, &master_key_locator(&root)).is_err(),
                "a new key would open none of the state sealed under the lost one"
            );
        }
    }
}
