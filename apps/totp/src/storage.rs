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
//! `vault.key.dpapi` file. Legacy private keys migrate only after a
//! verified DPAPI restore; DPAPI failure never falls back to plaintext.
//!
//! Sealing and key custody are shared with the accounts vault through the
//! `secret-vault` crate. Under every custody a key that exists but cannot be
//! read is never replaced, no key is minted while `vault.enc` exists without
//! one, and a new local key is published with an exclusive create, so
//! concurrent first launches converge on the one key that reached disk.
//!
//! # Migration
//!
//! An older build wrote plaintext `vault.json`. On first load that file is
//! parsed, re-sealed into `vault.enc`, and then deleted, so an upgrade
//! transparently encrypts an existing vault without losing accounts.
//!
//! Persistence failures never crash the app: the vault remains usable in
//! memory for the session and the `--shots` renderer.  Unlike the old
//! fire-and-forget API, [`save`] reports whether the encrypted write became
//! durable so the UI can state that boundary honestly and offer an explicit,
//! bounded retry.

use std::path::{Path, PathBuf};

use secret_vault::{Custody as KeyCustody, VaultFormat, VaultKey};
use serde::{Deserialize, Serialize};
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

/// Resolve `$XDG_DATA_HOME/tilo-authenticator`, then Windows `LocalAppData` or
/// `$HOME/.local/share/...`. Returns `None` when no user directory is available.
pub fn data_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| {
            #[cfg(windows)]
            {
                std::env::var_os("LOCALAPPDATA")
                    .map(PathBuf::from)
                    .filter(|path| path.is_absolute())
            }
            #[cfg(not(windows))]
            {
                None
            }
        })
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
    Some(base.join("tilo-authenticator"))
}

/// Path of the encrypted vault (`.../tilo-authenticator/vault.enc`). This is
/// the real vault file — its presence is what "is there a saved vault?"
/// checks against.
pub fn vault_path() -> Option<PathBuf> {
    data_dir().map(|d| d.join("vault.enc"))
}

/// Path of the legacy plaintext vault, migrated + removed on first load.
fn legacy_plaintext_path_in(dir: &Path) -> PathBuf {
    dir.join("vault.json")
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
/// Returns storage, key-custody, integrity, or migration failures.
pub fn load_checked() -> std::io::Result<Vault> {
    let dir = data_dir().ok_or_else(|| std::io::Error::other("no user data directory"))?;
    load_at_checked(&dir, key_custody())
}

/// Persist the vault, sealed. `false` means changes remain in memory only.
pub fn save(vault: &Vault) -> bool {
    #[cfg(test)]
    if FORCE_SAVE_FAILURE.with(std::cell::Cell::get) {
        return false;
    }
    data_dir().is_some_and(|dir| save_at_with_custody(&dir, vault, key_custody()))
}

/// Load (and, if needed, migrate) the vault stored under `dir`.
#[cfg(test)]
fn load_at(dir: &Path) -> Vault {
    load_at_with_custody(dir, KeyCustody::LocalFile)
}

#[cfg(test)]
fn load_at_with_custody(dir: &Path, custody: KeyCustody) -> Vault {
    load_at_checked(dir, custody).unwrap_or_default()
}

fn load_at_checked(dir: &Path, custody: KeyCustody) -> std::io::Result<Vault> {
    let enc = dir.join("vault.enc");
    match std::fs::read(&enc) {
        Ok(blob) => {
            let key = load_or_create_key(dir, custody)
                .ok_or_else(|| std::io::Error::other("vault key unavailable"))?;
            let plaintext = open(&key, &blob)
                .ok_or_else(|| std::io::Error::other("vault integrity check failed"))?;
            return parse_vault(&plaintext);
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    // Transparent upgrade: an older build left a plaintext vault. Parse it,
    // re-seal it, and remove the cleartext original.
    let legacy = legacy_plaintext_path_in(dir);
    match std::fs::read(&legacy) {
        Ok(bytes) => {
            let plaintext = Zeroizing::new(bytes);
            let vault = parse_vault(&plaintext)?;
            if !save_at_with_custody(dir, &vault, custody) {
                return Err(std::io::Error::other("vault migration was not saved"));
            }
            std::fs::remove_file(&legacy)?;
            Ok(vault)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vault::new()),
        Err(error) => Err(error),
    }
}

/// Seal + write the vault under `dir`.
#[cfg(test)]
fn save_at(dir: &Path, vault: &Vault) {
    save_at_with_custody(dir, vault, KeyCustody::LocalFile);
}

fn save_at_with_custody(dir: &Path, vault: &Vault, custody: KeyCustody) -> bool {
    if let Err(e) = std::fs::create_dir_all(dir) {
        tracing::warn!(error = %e, "authenticator: could not create data dir");
        return false;
    }
    let Some(key) = load_or_create_key(dir, custody) else {
        tracing::warn!("authenticator: no vault key available; changes kept in memory only");
        return false;
    };
    // Loading an inaccessible vault yields an empty in-memory view. Do not
    // let a subsequent edit turn that recovery situation into data loss.
    match std::fs::read(dir.join("vault.enc")) {
        Ok(previous) if open(&key, &previous).is_none() => {
            tracing::warn!("authenticator: refusing to replace an unreadable existing vault");
            return false;
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
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
    if let Err(e) = atomic_file::write(&dir.join("vault.enc"), &blob) {
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
    vault_path().is_none_or(|p| !p.exists())
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

    #[cfg(windows)]
    #[test]
    fn windows_live_custody_protects_key_and_restores_vault() {
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

    #[cfg(windows)]
    #[test]
    fn windows_missing_key_cannot_replace_an_existing_vault() {
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

    #[cfg(windows)]
    #[test]
    fn windows_corrupt_ciphertext_is_preserved_on_save() {
        let dir = temp_dir();
        assert!(save_at_with_custody(&dir, &Vault::new(), key_custody()));
        let ciphertext = b"corrupt existing vault";
        std::fs::write(dir.join("vault.enc"), ciphertext).unwrap();
        assert!(load_at_checked(&dir, key_custody()).is_err());
        assert!(!save_at_with_custody(&dir, &Vault::new(), key_custody()));
        assert_eq!(std::fs::read(dir.join("vault.enc")).unwrap(), ciphertext);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_corrupt_protected_key_is_never_replaced() {
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

    #[cfg(windows)]
    #[test]
    fn windows_private_legacy_key_migrates_only_after_verified_restore() {
        let dir = temp_dir();
        private_fs::directory(&dir).unwrap();
        private_fs::create_atomic(&dir.join("vault.key"), &[42; KEY_LEN]).unwrap();
        let mut vault = Vault::new();
        vault.upsert(demo_accounts().remove(0));
        assert!(save_at_with_custody(&dir, &vault, KeyCustody::LocalFile));
        assert_eq!(
            load_at_with_custody(&dir, key_custody()).ordered(),
            vault.ordered()
        );
        assert!(!dir.join("vault.key").exists());
        assert!(private_fs::read(&dir.join("vault.key.dpapi")).is_ok());
        assert_eq!(
            load_at_with_custody(&dir, key_custody()).ordered(),
            vault.ordered()
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_interrupted_migration_removes_verified_plaintext_on_reopen() {
        let dir = temp_dir();
        private_fs::directory(&dir).unwrap();
        let legacy_key = [42; KEY_LEN];
        private_fs::create_atomic(&dir.join("vault.key"), &legacy_key).unwrap();
        let mut vault = Vault::new();
        vault.upsert(demo_accounts().remove(0));
        assert!(save_at_with_custody(&dir, &vault, KeyCustody::LocalFile));
        assert_eq!(
            load_at_checked(&dir, key_custody()).unwrap().ordered(),
            vault.ordered()
        );
        assert!(!dir.join("vault.key").exists());

        // Reconstruct the exact durable state after DPAPI publication but before
        // plaintext removal. The protected key and ciphertext remain unchanged.
        let ciphertext = std::fs::read(dir.join("vault.enc")).unwrap();
        let protected_key = std::fs::read(dir.join("vault.key.dpapi")).unwrap();
        private_fs::create_atomic(&dir.join("vault.key"), &legacy_key).unwrap();
        assert_eq!(
            load_at_checked(&dir, key_custody()).unwrap().ordered(),
            vault.ordered()
        );
        assert!(
            !dir.join("vault.key").exists(),
            "reopen must finish interrupted key migration"
        );
        assert_eq!(std::fs::read(dir.join("vault.enc")).unwrap(), ciphertext);
        assert_eq!(
            std::fs::read(dir.join("vault.key.dpapi")).unwrap(),
            protected_key
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_interrupted_migration_preserves_conflicting_legacy_material() {
        for legacy in [vec![7; KEY_LEN], b"malformed key".to_vec()] {
            let dir = temp_dir();
            assert!(save_at_with_custody(&dir, &Vault::new(), key_custody()));
            let encrypted = std::fs::read(dir.join("vault.enc")).unwrap();
            let protected = std::fs::read(dir.join("vault.key.dpapi")).unwrap();
            private_fs::create_atomic(&dir.join("vault.key"), &legacy).unwrap();
            assert!(load_at_checked(&dir, key_custody()).is_err());
            assert!(!save_at_with_custody(&dir, &Vault::new(), key_custody()));
            assert_eq!(std::fs::read(dir.join("vault.key")).unwrap(), legacy);
            assert_eq!(std::fs::read(dir.join("vault.enc")).unwrap(), encrypted);
            assert_eq!(
                std::fs::read(dir.join("vault.key.dpapi")).unwrap(),
                protected
            );
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_interrupted_migration_preserves_keys_if_ciphertext_is_corrupt() {
        let dir = temp_dir();
        let key = load_or_create_key(&dir, key_custody()).unwrap();
        private_fs::create_atomic(&dir.join("vault.key"), key.expose_secret()).unwrap();
        private_fs::create_atomic(&dir.join("vault.enc"), b"damaged vault").unwrap();
        assert!(load_at_checked(&dir, key_custody()).is_err());
        assert_eq!(
            std::fs::read(dir.join("vault.key")).unwrap(),
            key.expose_secret()
        );
        assert_eq!(
            std::fs::read(dir.join("vault.enc")).unwrap(),
            b"damaged vault"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_interrupted_migration_retries_after_plaintext_removal_denied() {
        use std::os::windows::fs::OpenOptionsExt;

        let dir = temp_dir();
        assert!(save_at_with_custody(&dir, &Vault::new(), key_custody()));
        let key = load_or_create_key(&dir, key_custody()).unwrap();
        private_fs::create_atomic(&dir.join("vault.key"), key.expose_secret()).unwrap();
        let protected = std::fs::read(dir.join("vault.key.dpapi")).unwrap();
        let encrypted = std::fs::read(dir.join("vault.enc")).unwrap();
        // Allow reads but deny deletion while this owned handle is alive.
        let held = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(dir.join("vault.key"))
            .unwrap();
        assert!(load_at_checked(&dir, key_custody()).is_err());
        assert!(dir.join("vault.key").exists());
        drop(held);
        assert!(load_at_checked(&dir, key_custody()).is_ok());
        assert!(!dir.join("vault.key").exists());
        assert_eq!(
            std::fs::read(dir.join("vault.key.dpapi")).unwrap(),
            protected
        );
        assert_eq!(std::fs::read(dir.join("vault.enc")).unwrap(), encrypted);
        std::fs::remove_dir_all(dir).unwrap();
    }

    // Written by the implementation that predates the shared `secret-vault` crate. The
    // on-disk format must stay readable: these bytes are what existing users have.
    const LEGACY_KEY: &str = "c9df09f1db44ccd3ec5a2faf50e7276ab216492f4393fbc0657f858f3b67669b";
    const LEGACY_VAULT: &str = concat!(
        "514f4156017b7647917454de11136c24a35c28e8d85166a880e87309ecb8f2e2",
        "6af9ab277aecb0f6a67d3ae878ab760bf4bf002c28e9b1cda6c780f93f6ba8a1",
        "3771a9a4cc5210ad27018a38eaa18c2f61311ac7f365573f4890791127695dc3",
        "9a1ce65d3f47d20d6520d04abdf6cd0993ea3eed144532af1dae01bd37b0856d",
        "8207e484e6e8015ed7bb5292e07b5e2fcbef7bccd2c281c1e44727ea345ada46",
        "2a1b89ef7da0c581d5ddad68ee5097d0d09a0b7562a2a53c58d32cb3e4953d49",
        "a3a371acdb427828864bb4af50d3a36485f8e3ac5f1c1cf830c32c0c936b78c6",
        "7ea2012d5b4af54ef4e08945731798b32c308252a59d3c5446e3bab592bb0fa7",
        "38d580",
    );
    // `seal(&[0x42; KEY_LEN], b"tilo authenticator known answer")` from the same implementation.
    const LEGACY_BLOB: &str = concat!(
        "514f4156013167e6c9c0727220a09abf8529ab69ffaccaf6fe2925e6884e9ae1",
        "cc009de167689f2b086d0378def6c8be215b59618a75daff64de0e8b59a38973",
        "d1a668b1559972f956191d6f",
    );

    fn unhex(hex: &str) -> Vec<u8> {
        hex.as_bytes()
            .chunks_exact(2)
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect()
    }

    fn legacy_fixture_vault() -> Vault {
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

    #[test]
    fn known_answer_blob_from_the_previous_implementation_opens() {
        let blob = unhex(LEGACY_BLOB);
        assert_eq!(&blob[..5], b"QOAV\x01");
        assert_eq!(
            open(&VaultKey::from_bytes([0x42; KEY_LEN]), &blob)
                .unwrap()
                .as_slice(),
            b"tilo authenticator known answer"
        );
        assert!(open(&VaultKey::from_bytes([0x43; KEY_LEN]), &blob).is_none());
    }

    #[test]
    fn vault_written_by_the_previous_implementation_loads_and_resaves() {
        let dir = temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        private_fs::create_atomic(&dir.join("vault.key"), &unhex(LEGACY_KEY)).unwrap();
        std::fs::write(dir.join("vault.enc"), unhex(LEGACY_VAULT)).unwrap();
        let expected = legacy_fixture_vault().ordered();

        let loaded = load_at_checked(&dir, KeyCustody::LocalFile).unwrap();
        assert_eq!(loaded.ordered(), expected);
        assert!(save_at_with_custody(&dir, &loaded, KeyCustody::LocalFile));
        assert!(
            std::fs::read(dir.join("vault.enc"))
                .unwrap()
                .starts_with(b"TSEAL"),
            "re-saving writes the current tilo-seal envelope"
        );
        assert_eq!(
            load_at_checked(&dir, KeyCustody::LocalFile)
                .unwrap()
                .ordered(),
            expected
        );
        assert_eq!(
            std::fs::read(dir.join("vault.key")).unwrap(),
            unhex(LEGACY_KEY),
            "reopening must keep the existing key"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_key_is_never_replaced_and_its_vault_survives() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir();
        let vault = legacy_fixture_vault();
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

    #[test]
    fn missing_key_never_rekeys_an_existing_vault() {
        let dir = temp_dir();
        assert!(save_at_with_custody(
            &dir,
            &legacy_fixture_vault(),
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

    #[test]
    fn simultaneous_first_saves_share_one_key() {
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
                        save_at_with_custody(&dir, &legacy_fixture_vault(), KeyCustody::LocalFile);
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
                legacy_fixture_vault().ordered()
            );
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn vault_file_round_trips_through_json() {
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

    #[test]
    fn seal_open_round_trips_and_rejects_tampering() {
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

    #[test]
    fn fresh_writes_are_never_plaintext_on_disk() {
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

    #[test]
    fn save_then_load_round_trips_the_vault() {
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

    #[cfg(unix)]
    #[test]
    fn key_file_is_owner_only() {
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

    #[test]
    fn invalid_legacy_vault_is_reported_and_preserved() {
        let dir = temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let original = b"not a vault document";
        std::fs::write(dir.join("vault.json"), original).unwrap();
        assert!(load_at_checked(&dir, KeyCustody::LocalFile).is_err());
        assert_eq!(std::fs::read(dir.join("vault.json")).unwrap(), original);
        assert!(!dir.join("vault.enc").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn plaintext_vault_is_migrated_and_removed() {
        let dir = temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        // Simulate an older build's plaintext vault.
        let mut v = Vault::new();
        for a in demo_accounts() {
            v.upsert(a);
        }
        let file = VaultFile {
            version: VAULT_VERSION,
            accounts: v.ordered(),
        };
        std::fs::write(dir.join("vault.json"), serde_json::to_vec(&file).unwrap()).unwrap();

        let loaded = load_at(&dir);
        assert_eq!(loaded.len(), v.len(), "migration preserves accounts");
        assert!(dir.join("vault.enc").exists(), "vault is re-sealed");
        assert!(
            !dir.join("vault.json").exists(),
            "plaintext original is removed"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
