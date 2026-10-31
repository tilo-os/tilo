//! This device's MLS vault: the master key the platform secret store keeps
//! for collaboration, and the state root it seals.
//!
//! The vault hands the channel encryption service ([`super::Channels`]) one
//! key derived from the master key and the directory where sealed channel
//! state lives. MLS group membership follows the room's access policy through
//! that service's participants; nothing here writes membership.
//!
//! The layout is the one earlier sessions wrote, byte for byte: a device that
//! kept its sealed channel state has it opened here, never replaced.

use getrandom::{SysRng, rand_core::UnwrapErr};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use collab_types::ChannelId;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use zeroize::Zeroizing;

/// `registry.postcard` in the layout earlier builds wrote. Its presence marks
/// a vault whose master key must already be in the secret store, so the file
/// stays. Its entries named group, key-package and snapshot-receipt records of
/// a retired MLS lifecycle authority; nothing reads them, and
/// [`MlsVault::open`] clears them.
///
/// Postcard is not self-describing: the field order and wire types here must
/// not change, or an existing file stops decoding and the vault stops opening.
/// The 32-byte keys were transparent handle newtypes, which encode the same.
#[derive(Default, Serialize, Deserialize)]
struct RetiredRegistry {
    groups: BTreeMap<[u8; 32], RetiredGroupRecord>,
    key_packages: BTreeMap<[u8; 32], String>,
    snapshots: BTreeMap<[u8; 32], RetiredSnapshotRecord>,
    latest_snapshot: BTreeMap<[u8; 16], [u8; 32]>,
}

impl RetiredRegistry {
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

/// This device's vault: the master key the platform secret store keeps for
/// collaboration, and the state root it seals. Only ciphertext is written to
/// disk.
pub(crate) struct MlsVault {
    root: PathBuf,
    master_key: Zeroizing<[u8; 32]>,
}

impl MlsVault {
    /// Open at the session-owned state root selected by the launcher. No home
    /// directory fallback is accepted because that would weaken key custody.
    ///
    /// # Errors
    /// Session state or vault material cannot be opened.
    pub(crate) fn open_for_session() -> Result<Self, String> {
        let state = tilo_paths::env_dir("XDG_STATE_HOME").ok_or_else(|| {
            "the launcher did not provide XDG_STATE_HOME for MLS state".to_owned()
        })?;
        Self::open(state.join("tilo/collaboration/mls"))
    }

    /// Where this device keeps its sealed channel group and member state.
    pub(crate) fn channel_state_root(&self) -> PathBuf {
        self.root.join("channels")
    }

    /// The key sealing this device's channel state, derived from the master
    /// key under its own domain so it seals nothing else.
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
    /// The secret store is absent or locked, or the registry is invalid.
    pub(crate) fn open(root: PathBuf) -> Result<Self, String> {
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
                    return Err(
                        "the secret store did not return the newly stored MLS key".to_owned()
                    );
                }
                verified
            }
        };
        if registry_path.exists() {
            let registry: RetiredRegistry =
                postcard::from_bytes(&fs::read(&registry_path).map_err(io_error)?)
                    .map_err(|error| error.to_string())?;
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
/// as the marker [`MlsVault::open`] checks. Best-effort: the records are
/// unread, so failing to drop them costs only their bytes.
fn clear_retired_registry(path: &Path) {
    let cleared = postcard::to_allocvec(&RetiredRegistry::default())
        .map_err(|error| error.to_string())
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
fn io_error(error: std::io::Error) -> String {
    error.to_string()
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    tilo_files::native::atomic::write_with_mode(path, bytes, 0o600).map_err(io_error)
}

/// The secret-store namespace holding the MLS vault master keys.
const MLS_SECRETS_APP: &str = "so.tilo.collaboration.mls";

/// The platform secret store: the Secret Service on Linux, the login Keychain
/// on macOS, or the sealed file store a headless machine names with
/// `TILO_SECRETS_DIR`.
fn mls_secrets() -> tilo_secrets::SecretStore {
    tilo_secrets::SecretStore::for_app(MLS_SECRETS_APP)
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

/// A key stored by earlier builds through `secret-tool` under their own
/// attributes; `None` when there is none (or no `secret-tool`). The caller
/// moves it into the platform store.
fn legacy_secret_tool_load(locator: &str) -> Result<Option<Zeroizing<Vec<u8>>>, String> {
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
        hex::decode(String::from_utf8_lossy(stdout.as_slice()).trim())
            .map_err(|_| "the Secret Service returned a malformed MLS key".to_owned())?,
    );
    Ok(Some(decoded))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `registry.postcard` as builds that kept lifecycle records wrote it,
    /// holding one record of each retired kind. Generated from those types.
    fn registry_before_the_authority_was_dropped() -> Vec<u8> {
        let text = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/registry_with_retired_records.postcard.hex"
        ));
        hex::decode(text.trim()).expect("the fixture is hex")
    }

    /// Keep this thread's secrets in memory and give the vault at `root` a
    /// known master key, so `open` neither reaches the user's secret store
    /// nor generates a fresh key.
    fn seed_master_key(root: &Path, key: [u8; 32]) -> tilo_secrets::SecretsScope {
        let scope = tilo_secrets::scope_thread(tilo_secrets::memory_backend());
        secret_service_store(&master_key_locator(root), &key).expect("the master key");
        scope
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
        let _secrets = seed_master_key(&root, [6; 32]);

        let vault = MlsVault::open(root.clone()).expect("the vault opens");

        assert_eq!(
            hex::encode(vault.channel_state_key_bytes()),
            "9e51a75e84b0a7930f1865b7c7a8e92e60cab50b0b054df9b72029b7604a1ae5",
            "the derived key is the one earlier builds sealed channel state with"
        );
        assert_eq!(vault.channel_state_root(), root.join("channels"));
        let cleared: RetiredRegistry =
            postcard::from_bytes(&fs::read(root.join("registry.postcard")).expect("the registry"))
                .expect("it decodes");
        assert!(cleared.is_empty(), "the retired records are gone");
        let reopened = MlsVault::open(root).expect("the vault opens again");
        assert_eq!(
            reopened.channel_state_key_bytes(),
            vault.channel_state_key_bytes(),
            "and the master key is kept"
        );
    }

    #[test]
    fn leftover_snapshot_files_are_removed_on_open() {
        let dir = tempfile::tempdir().expect("a directory");
        let root = dir.path().join("mls");
        fs::create_dir_all(root.join("authority")).expect("the retired ledger");
        fs::write(root.join("authority/authority.postcard"), [1, 2, 3]).expect("the ledger");
        fs::write(
            root.join("authority/00000000-0000-0000-0000-000000000000.governance"),
            [4],
        )
        .expect("the ledger's entries");
        fs::write(root.join("snapshot-aa.op"), [5]).expect("a compaction envelope");
        fs::write(root.join("group-11.mls"), [8]).expect("a group");
        fs::write(root.join("key-package-22.mls"), [9]).expect("a key package");
        fs::create_dir_all(root.join("channels")).expect("the channel state");
        fs::write(root.join("channels/group.sealed"), [6]).expect("sealed channel state");
        fs::write(root.join("snapshot-notes.txt"), [7]).expect("a person's file");
        let _secrets = seed_master_key(&root, [6; 32]);

        MlsVault::open(root.clone()).expect("the vault opens");

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
