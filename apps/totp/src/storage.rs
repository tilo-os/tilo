//! On-disk persistence for the vault, encrypted at rest.
//!
//! TOTP secrets are long-lived bearer credentials, so the vault is never
//! written as plaintext. It is sealed with **XChaCha20-Poly1305** (a
//! 192-bit-nonce AEAD from the audited `RustCrypto` stack) under a 256-bit
//! key, and stored as `vault.enc` under the user data dir. A fresh random
//! nonce is drawn for every write, so re-saving an unchanged vault still
//! produces distinct ciphertext and no nonce is ever reused.
//!
//! # Key custody
//!
//! In a normal desktop session the symmetric key is held by the freedesktop
//! Secret Service through `secret-tool`, separate from the ciphertext. An
//! existing owner-only `vault.key` is migrated only after a verified Secret
//! Service round-trip, then removed. Headless/test environments without the
//! client use the explicit owner-only local fallback so recovery and tests do
//! not silently lose an authenticator vault.
//! Windows uses user-scoped DPAPI with a private, atomically published
//! `vault.key.dpapi` file; DPAPI failure never falls back to plaintext.
//!
//! Sealing and key custody are shared with the accounts vault through the
//! `secret-vault` crate. Under every custody a key that exists but cannot be
//! read is never replaced, no key is minted while `vault.enc` exists without
//! one, and a new local key is published with an exclusive create, so
//! concurrent first launches converge on the one key that reached disk.
//!
//! Persistence failures never crash the app: the vault remains usable in
//! memory for the session and the `--shots` renderer.  Unlike the old
//! fire-and-forget API, [`save`] reports whether the encrypted write became
//! durable so the UI can state that boundary honestly and offer an explicit,
//! bounded retry.

use std::path::{Path, PathBuf};

use secret_vault::{Custody as KeyCustody, VaultFormat, VaultKey};
use serde::{Deserialize, Serialize};
use tilo_files::{Dir, FsError, Location, VPath, blocking};
use zeroize::Zeroizing;

use crate::model::{Account, Vault};

#[derive(Debug, Serialize, Deserialize, Default)]
struct VaultFile {
    version: u32,
    accounts: Vec<Account>,
}

const VAULT_VERSION: u32 = 1;

/// The persisted identity of the authenticator vault. Every value here is
/// part of the on-disk format and of where its key is held.
const FORMAT: VaultFormat = VaultFormat {
    name: "authenticator",
    magic: b"QOAV",
    secret_service_label: "Tilo authenticator vault",
    secret_service_attributes: &[("application", "Tilo"), ("purpose", "totp-vault")],
    dpapi_entropy: b"Tilo Authenticator vault key v1",
};

// This is thread-local so storage failure injection remains deterministic even
// when the test harness runs unrelated authenticator tests in parallel.  It is
// intentionally compiled out of production builds.
#[cfg(test)]
thread_local! {
    static FORCE_SAVE_FAILURE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub(crate) fn force_save_failure_for_test(fail: bool) {
    FORCE_SAVE_FAILURE.with(|forced| forced.set(fail));
}

/// `tilo-authenticator` in [`tilo_paths::legacy_data_home`], where existing
/// vaults live. Returns `None` when no user directory is available.
pub fn data_dir() -> Option<PathBuf> {
    Some(tilo_paths::legacy_data_home()?.join("tilo-authenticator"))
}

/// Path of the encrypted vault (`.../tilo-authenticator/vault.enc`). This is
/// the real vault file — its presence is what "is there a saved vault?"
/// checks against.
pub fn vault_path() -> Option<PathBuf> {
    data_dir().map(|d| d.join("vault.enc"))
}

const VAULT_FILE: &str = "vault.enc";
/// The legacy plaintext vault, migrated + removed on first load.
const LEGACY_FILE: &str = "vault.json";

/// The files below `dir`. Key custody needs the directory's path itself.
fn files(dir: &Path) -> std::io::Result<Dir> {
    Dir::open(Location::Native(dir.to_path_buf())).map_err(std::io::Error::other)
}

fn name(file: &str) -> VPath {
    VPath::new(file).unwrap_or_else(|_| VPath::root())
}

/// Load the vault from disk. Returns an empty vault when nothing is stored
/// (or when it can't be read/decrypted) — never an error to the caller.
pub fn load() -> Vault {
    load_checked().unwrap_or_else(|error| {
        tracing::warn!(%error, "authenticator: vault needs recovery");
        Vault::new()
    })
}

/// Load the real vault without confusing a recovery failure with a fresh install.
///
/// # Errors
/// Returns storage, key-custody or integrity failures.
pub fn load_checked() -> std::io::Result<Vault> {
    match data_dir() {
        Some(dir) => load_at_checked(&dir, key_custody()),
        None => load_in(&page_files()?, &Keys::System),
    }
}

/// Where the vault lives without a native data directory: the app's data
/// location, which on the web is the page's storage.
fn page_files() -> std::io::Result<Dir> {
    Dir::open(tilo_paths::AppPaths::for_app("tilo-authenticator").data())
        .map_err(std::io::Error::other)
}

/// Where the vault key comes from.
enum Keys<'a> {
    /// Custody beside a native vault directory, migrating a local key.
    Native(&'a Path, KeyCustody),
    /// The platform's secret store alone (the web's).
    System,
}

impl Keys<'_> {
    fn load(&self, store: &Dir) -> Option<VaultKey> {
        match self {
            Self::Native(dir, custody) => load_or_create_key(dir, *custody),
            Self::System => FORMAT
                .load_or_create_system_key(blocking::metadata(store, &name(VAULT_FILE)).is_ok()),
        }
    }
}

/// Persist the vault, sealed. `false` means changes remain in memory only.
pub fn save(vault: &Vault) -> bool {
    #[cfg(test)]
    if FORCE_SAVE_FAILURE.with(std::cell::Cell::get) {
        return false;
    }
    match data_dir() {
        Some(dir) => save_at_with_custody(&dir, vault, key_custody()),
        None => page_files().is_ok_and(|store| save_in(&store, vault, &Keys::System)),
    }
}

/// Load the vault stored under `dir`.
#[cfg(test)]
fn load_at(dir: &Path) -> Vault {
    load_at_with_custody(dir, KeyCustody::LocalFile)
}

#[cfg(test)]
fn load_at_with_custody(dir: &Path, custody: KeyCustody) -> Vault {
    load_at_checked(dir, custody).unwrap_or_default()
}

fn load_at_checked(dir: &Path, custody: KeyCustody) -> std::io::Result<Vault> {
    load_in(&files(dir)?, &Keys::Native(dir, custody))
}

fn load_in(store: &Dir, keys: &Keys<'_>) -> std::io::Result<Vault> {
    let store = store.clone();
    match blocking::read(&store, &name(VAULT_FILE)) {
        Ok(blob) => {
            let key = keys
                .load(&store)
                .ok_or_else(|| std::io::Error::other("vault key unavailable"))?;
            let plaintext = open(&key, &blob)
                .ok_or_else(|| std::io::Error::other("vault integrity check failed"))?;
            return parse_vault(&plaintext);
        }
        Err(FsError::NotFound) => {}
        Err(error) => return Err(std::io::Error::other(error)),
    }

    // Transparent upgrade: an older build left a plaintext vault. Parse it,
    // re-seal it, and remove the cleartext original.
    match blocking::read(&store, &name(LEGACY_FILE)) {
        Ok(bytes) => {
            let plaintext = Zeroizing::new(bytes);
            let vault = parse_vault(&plaintext)?;
            if !save_in(&store, &vault, keys) {
                return Err(std::io::Error::other("vault migration was not saved"));
            }
            blocking::remove(&store, &name(LEGACY_FILE)).map_err(std::io::Error::other)?;
            Ok(vault)
        }
        Err(FsError::NotFound) => Ok(Vault::new()),
        Err(error) => Err(std::io::Error::other(error)),
    }
}

#[cfg(test)]
fn save_at(dir: &Path, vault: &Vault) {
    save_at_with_custody(dir, vault, KeyCustody::LocalFile);
}

fn save_at_with_custody(dir: &Path, vault: &Vault, custody: KeyCustody) -> bool {
    match files(dir) {
        Ok(store) => save_in(&store, vault, &Keys::Native(dir, custody)),
        Err(e) => {
            tracing::warn!(error = %e, "authenticator: no data dir");
            false
        }
    }
}

fn save_in(store: &Dir, vault: &Vault, keys: &Keys<'_>) -> bool {
    let store = store.clone();
    if let Err(e) = blocking::create_dir_all(&store, &VPath::root()) {
        tracing::warn!(error = %e, "authenticator: could not create data dir");
        return false;
    }
    let Some(key) = keys.load(&store) else {
        tracing::warn!("authenticator: no vault key available; changes kept in memory only");
        return false;
    };
    // Loading an inaccessible vault yields an empty in-memory view. Do not
    // let a subsequent edit turn that recovery situation into data loss.
    match blocking::read(&store, &name(VAULT_FILE)) {
        Ok(previous) if open(&key, &previous).is_none() => {
            tracing::warn!("authenticator: refusing to replace an unreadable existing vault");
            return false;
        }
        Ok(_) | Err(FsError::NotFound) => {}
        Err(error) => {
            tracing::warn!(%error, "authenticator: existing vault cannot be checked");
            return false;
        }
    }
    let file = VaultFile {
        version: VAULT_VERSION,
        accounts: vault.ordered(),
    };
    let plaintext = match serde_json::to_vec(&file) {
        Ok(bytes) => Zeroizing::new(bytes),
        Err(e) => {
            tracing::warn!(error = %e, "authenticator: vault serialize failed");
            return false;
        }
    };
    let Some(blob) = seal(&key, &plaintext) else {
        tracing::warn!("authenticator: vault encryption failed");
        return false;
    };
    if let Err(e) = blocking::write_atomic(&store, &name(VAULT_FILE), &blob) {
        tracing::warn!(error = %e, "authenticator: vault write failed");
        false
    } else {
        true
    }
}

fn parse_vault(plaintext: &[u8]) -> std::io::Result<Vault> {
    let file = serde_json::from_slice::<VaultFile>(plaintext)
        .map_err(|_| std::io::Error::other("vault document is invalid"))?;
    if file.version != VAULT_VERSION {
        return Err(std::io::Error::other("unsupported vault version"));
    }
    let mut vault = Vault::new();
    vault.accounts = file.accounts;
    vault.renumber();
    Ok(vault)
}

/// Read the vault key from `dir`, generating + persisting a fresh random
/// one the first time. Returns `None` when the key can neither be read nor
/// safely created, in which case the caller degrades to in-memory-only: an
/// existing key that cannot be read is never replaced, and no key is minted
/// while `vault.enc` exists without one.
fn load_or_create_key(dir: &Path, custody: KeyCustody) -> Option<VaultKey> {
    FORMAT.load_or_create_key(dir, custody)
}

fn key_custody() -> KeyCustody {
    KeyCustody::for_session()
}

fn seal(key: &VaultKey, plaintext: &[u8]) -> Option<Vec<u8>> {
    FORMAT.seal(key, plaintext)
}

fn open(key: &VaultKey, blob: &[u8]) -> Option<Zeroizing<Vec<u8>>> {
    FORMAT.open(key, blob)
}

/// Fresh-install check: true when no vault has been persisted yet, so the
/// app shows the empty state rather than fabricating sample accounts.
pub fn is_first_run() -> bool {
    let store = match data_dir() {
        Some(dir) => files(&dir),
        None => page_files(),
    };
    store.is_ok_and(|store| blocking::metadata(&store, &name(VAULT_FILE)).is_err())
}

/// Test/inspection helper: deserialize an account list from a JSON blob.
#[doc(hidden)]
pub fn accounts_from_json(json: &str) -> Option<Vec<Account>> {
    serde_json::from_str::<VaultFile>(json)
        .ok()
        .map(|f| f.accounts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::demo_accounts;
    use secret_vault::KEY_LEN;

    fn temp_dir() -> PathBuf {
        std::env::temp_dir().join(format!("tilo-totp-store-{}", uuid::Uuid::new_v4()))
    }

    /// Windows DPAPI custody protects the key and never replaces existing or corrupt material.
    #[cfg(windows)]
    #[test]
    fn windows_key_custody_protects_and_never_replaces_material() {
        // Case `windows_live_custody_protects_key_and_restores_vault`.
        {
            let dir = temp_dir();
            let mut vault = Vault::new();
            vault.upsert(demo_accounts().remove(0));
            assert!(save_at_with_custody(&dir, &vault, key_custody()));
            assert!(
                !dir.join("vault.key").exists(),
                "Windows must not persist a plaintext key"
            );
            assert!(dir.join("vault.key.dpapi").is_file());
            assert_eq!(
                load_at_with_custody(&dir, key_custody()).ordered(),
                vault.ordered()
            );
            std::fs::remove_dir_all(dir).unwrap();
        }

        // Case `windows_missing_key_cannot_replace_an_existing_vault`.
        {
            let dir = temp_dir();
            std::fs::create_dir_all(&dir).unwrap();
            let previous = b"existing inaccessible vault";
            std::fs::write(dir.join("vault.enc"), previous).unwrap();
            assert!(!save_at_with_custody(&dir, &Vault::new(), key_custody()));
            assert_eq!(std::fs::read(dir.join("vault.enc")).unwrap(), previous);
            assert!(!dir.join("vault.key").exists());
            assert!(!dir.join("vault.key.dpapi").exists());
            std::fs::remove_dir_all(dir).unwrap();
        }

        // Case `windows_corrupt_ciphertext_is_preserved_on_save`.
        {
            let dir = temp_dir();
            assert!(save_at_with_custody(&dir, &Vault::new(), key_custody()));
            let ciphertext = b"corrupt existing vault";
            std::fs::write(dir.join("vault.enc"), ciphertext).unwrap();
            assert!(load_at_checked(&dir, key_custody()).is_err());
            assert!(!save_at_with_custody(&dir, &Vault::new(), key_custody()));
            assert_eq!(std::fs::read(dir.join("vault.enc")).unwrap(), ciphertext);
            std::fs::remove_dir_all(dir).unwrap();
        }

        // Case `windows_corrupt_protected_key_is_never_replaced`.
        {
            let dir = temp_dir();
            assert!(save_at_with_custody(&dir, &Vault::new(), key_custody()));
            let ciphertext = std::fs::read(dir.join("vault.enc")).unwrap();
            private_fs::write_atomic(&dir.join("vault.key.dpapi"), b"corrupt DPAPI key").unwrap();
            assert!(!save_at_with_custody(&dir, &Vault::new(), key_custody()));
            assert_eq!(std::fs::read(dir.join("vault.enc")).unwrap(), ciphertext);
            assert_eq!(
                std::fs::read(dir.join("vault.key.dpapi")).unwrap(),
                b"corrupt DPAPI key"
            );
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    fn fixture_vault() -> Vault {
        let mut vault = Vault::new();
        let mut account = Account::blank(uuid::Uuid::from_u128(
            0x0123_4567_89ab_cdef_0123_4567_89ab_cdef,
        ));
        account.issuer = "Fixture Issuer".into();
        account.label = "ada@example.com".into();
        account.secret = "JBSWY3DPEHPK3PXP".into();
        vault.upsert(account);
        vault
    }

    /// On Unix, the key file is owner-only and an unreadable key is never replaced.
    #[cfg(unix)]
    #[test]
    fn unix_key_file_is_owner_only_and_never_replaced() {
        // Case `unreadable_key_is_never_replaced_and_its_vault_survives`.
        {
            use std::os::unix::fs::PermissionsExt;
            let dir = temp_dir();
            let vault = fixture_vault();
            assert!(save_at_with_custody(&dir, &vault, KeyCustody::LocalFile));
            let key_path = dir.join("vault.key");
            let key = std::fs::read(&key_path).unwrap();
            let sealed = std::fs::read(dir.join("vault.enc")).unwrap();

            // A directory in the key's place fails every read, whatever the
            // caller's privileges (a mode-000 file is still readable by root).
            std::fs::remove_file(&key_path).unwrap();
            std::fs::create_dir(&key_path).unwrap();
            assert!(load_at_checked(&dir, KeyCustody::LocalFile).is_err());
            assert!(!save_at_with_custody(
                &dir,
                &Vault::new(),
                KeyCustody::LocalFile
            ));
            assert!(key_path.is_dir(), "the unreadable key must not be replaced");
            assert_eq!(std::fs::read_dir(&key_path).unwrap().count(), 0);
            std::fs::remove_dir(&key_path).unwrap();
            std::fs::write(&key_path, &key).unwrap();
            std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600)).unwrap();

            assert_eq!(std::fs::read(&key_path).unwrap(), key);
            assert_eq!(std::fs::read(dir.join("vault.enc")).unwrap(), sealed);
            assert_eq!(
                load_at_checked(&dir, KeyCustody::LocalFile)
                    .unwrap()
                    .ordered(),
                vault.ordered()
            );
            std::fs::remove_dir_all(dir).unwrap();
        }

        // Case `key_file_is_owner_only`.
        {
            use std::os::unix::fs::PermissionsExt;
            let dir = temp_dir();
            let v = Vault::new();
            save_at(&dir, &v);
            let mode = std::fs::metadata(dir.join("vault.key"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "vault key must be owner-only");
            std::fs::remove_dir_all(&dir).ok();
        }
    }

    /// A missing key never rekeys an existing vault, and racing first saves share a key.
    #[test]
    fn key_custody_never_rekeys_and_first_saves_share_one_key() {
        // Case `missing_key_never_rekeys_an_existing_vault`.
        {
            let dir = temp_dir();
            assert!(save_at_with_custody(
                &dir,
                &fixture_vault(),
                KeyCustody::LocalFile
            ));
            let sealed = std::fs::read(dir.join("vault.enc")).unwrap();
            std::fs::remove_file(dir.join("vault.key")).unwrap();

            assert!(load_at_checked(&dir, KeyCustody::LocalFile).is_err());
            assert!(!save_at_with_custody(
                &dir,
                &Vault::new(),
                KeyCustody::LocalFile
            ));
            assert!(
                !dir.join("vault.key").exists(),
                "no replacement key is minted"
            );
            assert_eq!(std::fs::read(dir.join("vault.enc")).unwrap(), sealed);
            std::fs::remove_dir_all(dir).unwrap();
        }

        // Case `simultaneous_first_saves_share_one_key`.
        {
            for _ in 0..8 {
                let dir = temp_dir();
                let gate = std::sync::Arc::new(std::sync::Barrier::new(4));
                // Spawn every participant before joining: interleaving spawn/join
                // would deadlock the barrier.
                let mut workers = Vec::with_capacity(4);
                for _ in 0..4 {
                    let dir = dir.clone();
                    let gate = std::sync::Arc::clone(&gate);
                    workers.push(std::thread::spawn(move || {
                        gate.wait();
                        let key = load_or_create_key(&dir, KeyCustody::LocalFile).unwrap();
                        let saved =
                            save_at_with_custody(&dir, &fixture_vault(), KeyCustody::LocalFile);
                        (key, saved)
                    }));
                }
                let results: Vec<_> = workers
                    .into_iter()
                    .map(|worker| worker.join().unwrap())
                    .collect();
                let persisted = std::fs::read(dir.join("vault.key")).unwrap();
                assert!(results.iter().all(|(key, saved)| {
                    *saved && key.expose_secret().as_slice() == persisted.as_slice()
                }));
                assert_eq!(
                    load_at_checked(&dir, KeyCustody::LocalFile)
                        .unwrap()
                        .ordered(),
                    fixture_vault().ordered()
                );
                std::fs::remove_dir_all(dir).unwrap();
            }
        }
    }

    /// Vault JSON, sealing and save/load round-trips; fresh writes are never plaintext.
    #[test]
    fn vault_files_round_trip_sealed_and_never_plaintext() {
        // Case `vault_file_round_trips_through_json`.
        {
            let mut v = Vault::new();
            for a in demo_accounts() {
                v.upsert(a);
            }
            let file = VaultFile {
                version: VAULT_VERSION,
                accounts: v.ordered(),
            };
            let json = serde_json::to_string(&file).unwrap();
            let back = accounts_from_json(&json).unwrap();
            assert_eq!(back.len(), v.len());
            assert_eq!(back[0].issuer, v.ordered()[0].issuer);
        }

        // Case `seal_open_round_trips_and_rejects_tampering`.
        {
            let key = VaultKey::from_bytes([7u8; KEY_LEN]);
            let plaintext = b"secret vault bytes";
            let blob = seal(&key, plaintext).expect("seal");
            // The sealed blob is not the plaintext.
            assert!(!blob.windows(plaintext.len()).any(|w| w == plaintext));
            // It is a versioned `tilo-seal` envelope bound to this vault format.
            assert!(blob.starts_with(b"TSEAL"));
            // Round-trips under the same key.
            assert_eq!(open(&key, &blob).unwrap().as_slice(), plaintext);
            // A different key can't open it.
            assert!(open(&VaultKey::from_bytes([9u8; KEY_LEN]), &blob).is_none());
            // A single flipped ciphertext byte fails authentication.
            let mut tampered = blob;
            let last = tampered.len() - 1;
            tampered[last] ^= 0x01;
            assert!(open(&key, &tampered).is_none());
        }

        // Case `fresh_writes_are_never_plaintext_on_disk`.
        {
            let dir = temp_dir();
            let mut v = Vault::new();
            for a in demo_accounts() {
                v.upsert(a);
            }
            save_at(&dir, &v);
            let enc = dir.join("vault.enc");
            assert!(enc.exists(), "an encrypted vault is written");
            assert!(
                !dir.join("vault.json").exists(),
                "no plaintext vault is written"
            );
            let raw = std::fs::read(&enc).unwrap();
            // The base32 secret must not appear anywhere in the ciphertext.
            let secret = v.ordered()[0].secret.clone().into_bytes();
            assert!(
                !raw.windows(secret.len()).any(|w| w == secret.as_slice()),
                "secret must not be recoverable from the sealed file"
            );
            std::fs::remove_dir_all(&dir).ok();
        }

        // Case `save_then_load_round_trips_the_vault`.
        {
            let dir = temp_dir();
            let mut v = Vault::new();
            for a in demo_accounts() {
                v.upsert(a);
            }
            save_at(&dir, &v);
            let back = load_at(&dir);
            assert_eq!(back.len(), v.len());
            assert_eq!(back.ordered()[0].issuer, v.ordered()[0].issuer);
            assert_eq!(back.ordered()[0].secret, v.ordered()[0].secret);
            std::fs::remove_dir_all(&dir).ok();
        }
    }
}
