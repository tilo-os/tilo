//! This device's MLS vault: the master key the system secret store keeps for
//! collaboration, and the state root it seals.
//!
//! The vault hands the channel encryption service
//! (`crate::shell::channel_encryption`) one key derived from the master key
//! and the directory where sealed channel state lives. MLS group membership
//! follows the room's access policy through that service's steward; nothing
//! here writes membership.

use getrandom::{SysRng, rand_core::UnwrapErr};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use collab_types::ChannelId;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use thiserror::Error;
use zeroize::Zeroizing;

#[derive(Debug, Error)]
pub enum MlsVaultError {
    #[error("MLS vault provider unavailable: {0}")]
    VaultUnavailable(String),
    #[error("MLS vault persistence failed: {0}")]
    Persistence(String),
}

/// `registry.postcard` in the layout earlier builds wrote. Its presence marks
/// a vault whose master key must already be in the secret store, so the file
/// stays. Its entries named group, key-package and snapshot-receipt records of
/// a retired MLS lifecycle authority; nothing reads them, and
/// [`SecretServiceMlsVault::open`] clears them.
///
/// Postcard is not self-describing: the field order and wire types here must
/// not change, or an existing file stops decoding and the vault stops opening.
/// The 32-byte keys were transparent handle newtypes, which encode the same.
#[derive(Default, Serialize, Deserialize)]
struct SecretServiceRegistry {
    groups: BTreeMap<[u8; 32], RetiredGroupRecord>,
    key_packages: BTreeMap<[u8; 32], String>,
    snapshots: BTreeMap<[u8; 32], RetiredSnapshotRecord>,
    latest_snapshot: BTreeMap<[u8; 16], [u8; 32]>,
}

impl SecretServiceRegistry {
    fn is_empty(&self) -> bool {
        self.groups.is_empty()
            && self.key_packages.is_empty()
            && self.snapshots.is_empty()
            && self.latest_snapshot.is_empty()
    }
}

#[derive(Serialize, Deserialize)]
struct RetiredGroupRecord {
    state_file: String,
    signer_locator: String,
}

#[derive(Serialize, Deserialize)]
struct RetiredSnapshotRecord {
    channel: ChannelId,
    digest: [u8; 32],
    envelope_file: String,
}

/// Production vault backed by the system secret store (the freedesktop
/// Secret Service on Linux, the Keychain on macOS, or the sealed file store
/// a headless machine names with `TILO_SECRETS_DIR`). Only ciphertext is
/// written to disk.
pub struct SecretServiceMlsVault {
    root: PathBuf,
    master_key: Zeroizing<[u8; 32]>,
}

impl SecretServiceMlsVault {
    /// Open at the session-owned state root selected by the launcher. No home
    /// directory fallback is accepted because that would weaken key custody.
    ///
    /// # Errors
    ///
    /// Returns an error when session state or vault material cannot be opened.
    pub fn open_for_session() -> Result<Self, MlsVaultError> {
        let state = tilo_paths::env_dir("XDG_STATE_HOME").ok_or_else(|| {
            MlsVaultError::VaultUnavailable(
                "launcher did not provide XDG_STATE_HOME for MLS state".into(),
            )
        })?;
        Self::open(state.join("tilo/collaboration/mls"))
    }

    /// Where this device keeps its sealed channel group and member state.
    pub(crate) fn channel_state_root(&self) -> PathBuf {
        self.root.join("channels")
    }

    /// Key sealing this device's channel state, derived from the Secret
    /// Service master key under its own domain so it seals nothing else.
    #[cfg(not(test))]
    pub(crate) fn channel_state_key(&self) -> channel_crypto::VaultKey {
        channel_crypto::VaultKey::new(self.channel_state_key_bytes())
    }

    /// The bytes of [`Self::channel_state_key`]. Every device's sealed
    /// channel state depends on this derivation staying byte-identical.
    fn channel_state_key_bytes(&self) -> [u8; 32] {
        let mut hash = Sha256::new();
        hash.update(b"tilo.channel-state-key.v1");
        hash.update(self.master_key.as_slice());
        hash.finalize().into()
    }

    /// Open the vault, creating its master key in the secret store if this is
    /// the first use. There is intentionally no file-key fallback.
    ///
    /// # Errors
    ///
    /// Returns unavailable if the secret store is absent/locked, or persistence
    /// errors for an invalid registry.
    pub fn open(root: PathBuf) -> Result<Self, MlsVaultError> {
        fs::create_dir_all(&root).map_err(io_error)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).map_err(io_error)?;
        }
        let locator = master_key_locator(&root);
        let registry_path = root.join("registry.postcard");
        let master_key = match secret_service_load(&locator) {
            Ok(bytes) => bytes,
            Err(error) if registry_path.exists() => return Err(error),
            Err(_) => {
                let mut bytes = Zeroizing::new([0_u8; 32]);
                rand_core::Rng::fill_bytes(&mut UnwrapErr(SysRng), bytes.as_mut());
                secret_service_store(&locator, bytes.as_ref())?;
                let verified = secret_service_load(&locator)?;
                if verified.as_slice() != bytes.as_slice() {
                    return Err(MlsVaultError::VaultUnavailable(
                        "the secret store did not return the newly stored MLS key".into(),
                    ));
                }
                verified
            }
        };
        if registry_path.exists() {
            let registry: SecretServiceRegistry =
                postcard::from_bytes(&fs::read(&registry_path).map_err(io_error)?)
                    .map_err(|error| MlsVaultError::Persistence(error.to_string()))?;
            if !registry.is_empty() {
                clear_retired_registry(&registry_path);
            }
        }
        remove_retired_files(&root);
        Ok(Self { root, master_key })
    }
}

/// Where the secret store keeps the master key of the vault at `root`.
fn master_key_locator(root: &Path) -> String {
    format!(
        "mls-master-{}",
        hex::encode(Sha256::digest(root.as_os_str().as_encoded_bytes()))
    )
}

/// Rewrite `registry.postcard` without the retired records, keeping the file
/// as the marker [`SecretServiceMlsVault::open`] checks. Best-effort: the
/// records are unread, so failing to drop them costs only their bytes.
fn clear_retired_registry(path: &Path) {
    let cleared = postcard::to_allocvec(&SecretServiceRegistry::default())
        .map_err(|error| MlsVaultError::Persistence(error.to_string()))
        .and_then(|bytes| atomic_write(path, &bytes));
    if let Err(error) = cleared {
        tracing::warn!(%error, "retired MLS registry records could not be cleared");
    }
}

/// Remove what the retired MLS lifecycle authority and its snapshot receipts
/// left in `root`: the authority's ledger directory, every compaction
/// envelope copied to `snapshot-*.op`, and the `group-*.mls` and
/// `key-package-*.mls` files its registry named. Nothing reads them. Sealed
/// channel state lives under `channels/` and is untouched. Best-effort, so a
/// file that cannot be removed never keeps the vault from opening.
fn remove_retired_files(root: &Path) {
    let authority = root.join("authority");
    if fs::symlink_metadata(&authority).is_ok()
        && let Err(error) = fs::remove_dir_all(&authority)
    {
        tracing::warn!(%error, "retired MLS authority ledger could not be removed");
    }
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if is_retired_file(&path)
            && entry.file_type().is_ok_and(|kind| kind.is_file())
            && let Err(error) = fs::remove_file(&path)
        {
            tracing::warn!(%error, "retired MLS vault file could not be removed");
        }
    }
}

/// A top-level file only the retired authority wrote: `snapshot-*.op`,
/// `group-*.mls` or `key-package-*.mls`.
fn is_retired_file(path: &Path) -> bool {
    let (Some(stem), Some(extension)) = (
        path.file_stem().and_then(|stem| stem.to_str()),
        path.extension().and_then(|extension| extension.to_str()),
    ) else {
        return false;
    };
    match extension {
        "op" => stem.starts_with("snapshot-"),
        "mls" => stem.starts_with("group-") || stem.starts_with("key-package-"),
        _ => false,
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "I/O errors are consumed by map_err at the persistence boundary"
)]
fn io_error(error: std::io::Error) -> MlsVaultError {
    MlsVaultError::Persistence(error.to_string())
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), MlsVaultError> {
    tilo_files::native::atomic::write_with_mode(path, bytes, 0o600).map_err(io_error)
}

/// The secret-store namespace holding the MLS vault master keys.
const MLS_SECRETS_APP: &str = "so.tilo.collaboration.mls";

/// The system secret store: the Secret Service on Linux, the login Keychain
/// on macOS.
fn mls_secrets() -> tilo_secrets::SecretStore {
    tilo_secrets::SecretStore::for_app(MLS_SECRETS_APP)
}

fn secret_service_store(locator: &str, secret: &[u8]) -> Result<(), MlsVaultError> {
    let options = tilo_secrets::PutOptions {
        label: Some("Tilo MLS key".into()),
    };
    mls_secrets()
        .put_blocking(locator, secret, options)
        .map_err(|error| {
            MlsVaultError::VaultUnavailable(format!("MLS key storage refused: {error}"))
        })
}

fn secret_service_load(locator: &str) -> Result<Zeroizing<[u8; 32]>, MlsVaultError> {
    let stored = mls_secrets().get_blocking(locator).map_err(|error| {
        MlsVaultError::VaultUnavailable(format!("MLS key lookup unavailable: {error}"))
    })?;
    let bytes = if let Some(secret) = stored {
        Zeroizing::new(secret.expose().to_vec())
    } else {
        let legacy = legacy_secret_tool_load(locator)?
            .ok_or_else(|| MlsVaultError::VaultUnavailable("MLS key unavailable".into()))?;
        secret_service_store(locator, &legacy)?;
        legacy
    };
    let key: [u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| MlsVaultError::VaultUnavailable("stored MLS key has invalid length".into()))?;
    Ok(Zeroizing::new(key))
}

/// A key stored by earlier builds through `secret-tool` under their own
/// attributes; `None` when there is none (or no `secret-tool`). The caller
/// moves it into the system store.
fn legacy_secret_tool_load(locator: &str) -> Result<Option<Zeroizing<Vec<u8>>>, MlsVaultError> {
    let Ok(output) = bounded_command::run(
        "secret-tool",
        &["lookup", "tilo-purpose", "mls", "tilo-mls-key", locator],
        None,
        64 * 1024,
        Duration::from_secs(10),
    ) else {
        return Ok(None);
    };
    if !output.status.success() {
        return Ok(None);
    }
    let stdout = Zeroizing::new(output.stdout);
    let decoded = Zeroizing::new(
        hex::decode(String::from_utf8_lossy(stdout.as_slice()).trim()).map_err(|_| {
            MlsVaultError::VaultUnavailable("Secret Service returned malformed MLS key".into())
        })?,
    );
    Ok(Some(decoded))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `registry.postcard` as the build before #3693 wrote it, holding one
    /// record of each retired kind. Generated from that build's types.
    fn registry_before_til_182() -> Vec<u8> {
        let text = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/mls_registry_before_til_182.postcard.hex"
        ));
        hex::decode(text.trim()).unwrap()
    }

    /// Keep this thread's secrets in memory and give the vault at `root` a
    /// known master key, so `open` neither reaches the user's secret store
    /// nor generates a fresh key.
    fn seed_master_key(root: &Path, key: [u8; 32]) -> tilo_secrets::SecretsScope {
        let scope = tilo_secrets::scope_thread(tilo_secrets::memory_backend());
        secret_service_store(&master_key_locator(root), &key).unwrap();
        scope
    }

    #[test]
    fn registry_from_previous_layout_still_opens() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("mls");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("registry.postcard"), registry_before_til_182()).unwrap();
        let _secrets = seed_master_key(&root, [6; 32]);

        let vault = SecretServiceMlsVault::open(root.clone()).unwrap();

        // The derivation every device's sealed channel state depends on.
        assert_eq!(
            hex::encode(vault.channel_state_key_bytes()),
            "9e51a75e84b0a7930f1865b7c7a8e92e60cab50b0b054df9b72029b7604a1ae5"
        );
        assert_eq!(vault.channel_state_root(), root.join("channels"));
        let cleared: SecretServiceRegistry =
            postcard::from_bytes(&fs::read(root.join("registry.postcard")).unwrap()).unwrap();
        assert!(cleared.is_empty());
        let reopened = SecretServiceMlsVault::open(root).unwrap();
        assert_eq!(
            reopened.channel_state_key_bytes(),
            vault.channel_state_key_bytes()
        );
    }

    #[test]
    fn leftover_snapshot_files_are_removed_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("mls");
        fs::create_dir_all(root.join("authority")).unwrap();
        fs::write(root.join("authority/authority.postcard"), [1, 2, 3]).unwrap();
        fs::write(
            root.join("authority/00000000-0000-0000-0000-000000000000.governance"),
            [4],
        )
        .unwrap();
        fs::write(root.join("snapshot-aa.op"), [5]).unwrap();
        fs::write(root.join("group-11.mls"), [8]).unwrap();
        fs::write(root.join("key-package-22.mls"), [9]).unwrap();
        fs::create_dir_all(root.join("channels")).unwrap();
        fs::write(root.join("channels/group.sealed"), [6]).unwrap();
        fs::write(root.join("snapshot-notes.txt"), [7]).unwrap();
        let _secrets = seed_master_key(&root, [6; 32]);

        SecretServiceMlsVault::open(root.clone()).unwrap();

        assert!(!root.join("snapshot-aa.op").exists());
        assert!(!root.join("group-11.mls").exists());
        assert!(!root.join("key-package-22.mls").exists());
        assert!(!root.join("authority").exists());
        assert!(root.join("channels/group.sealed").exists());
        assert!(root.join("snapshot-notes.txt").exists());
    }
}
