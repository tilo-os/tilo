//! The encrypted **secret vault** — credentials at rest, never in the plain
//! [`crate::AccountStore`] JSON.
//!
//! Every credential an [`crate::Account`] needs is a long-lived bearer secret, sealed with
//! **XChaCha20-Poly1305** (192-bit-nonce AEAD, audited `RustCrypto` stack) under a 256-bit key,
//! stored as `vault.enc` under `$XDG_CONFIG_HOME/tilo/accounts/vault/`. A fresh random nonce
//! is drawn per write, so re-saving an unchanged vault still produces distinct ciphertext and
//! no nonce is ever reused. Sealing and key custody live in the `secret-vault` crate, shared
//! with the authenticator; this module owns only the account vault's format identity.
//!
//! # Key custody
//!
//! Windows protects keys with current-user DPAPI and private file ACLs, including
//! explicit-root stores. Existing private plaintext keys migrate only after
//! authenticated restoration; unavailable protected keys never fall back.
//! On a Linux desktop session the symmetric key is held by the freedesktop
//! Secret Service through `secret-tool`; it is not stored beside the
//! ciphertext. Headless/test environments without Secret Service use an
//! owner-only local key and can report that weaker custody to their caller.
//!
//! Failed loads are recorded for callers and prevent saves until recovery and
//! reopen. Saves revalidate existing ciphertext before replacement; a missing
//! key never authorizes re-keying an existing vault. The convenience `save`
//! method logs errors, while `try_save` returns them to the caller.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use secret_vault::{VaultFormat, VaultKey};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::AccountId;

pub use secret_vault::Custody as SecretCustody;

/// One account's secret material. Kept out of the plaintext account store entirely — it
/// only ever lives in the sealed vault (or in memory while an app is connecting).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Secret {
    /// The password / app-password for password-auth accounts.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub password: String,
    /// An `OAuth2` token bundle (serialised by the owning type) for accounts that sign in instead
    /// of using a password. Unused by current consumers but reserved so the vault shape doesn't churn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

impl Secret {
    /// A password-only secret.
    pub fn password(password: impl Into<String>) -> Self {
        Self {
            password: password.into(),
            token: None,
        }
    }

    /// Whether this secret carries nothing worth persisting.
    pub fn is_empty(&self) -> bool {
        self.password.is_empty() && self.token.is_none()
    }
}

/// The persisted identity of the account vault. Every value here is part of
/// the on-disk format and of where its key is held.
const FORMAT: VaultFormat = VaultFormat {
    name: "accounts",
    // Tilo Accounts Secret Vault.
    magic: b"QASV",
    secret_service_label: "Tilo account credentials",
    secret_service_attributes: &[("application", "Tilo"), ("purpose", "account-vault")],
    dpapi_entropy: b"Tilo Accounts vault key v1",
};

/// The encrypted, per-[`AccountId`] credential store. Held in memory after
/// [`SecretVault::open`]; mutations are staged and written by [`SecretVault::save`].
#[derive(Debug)]
pub struct SecretVault {
    /// The vault directory (`.../tilo/accounts/vault`).
    dir: PathBuf,
    /// Set where there is no native config directory (a browser page): the
    /// vault lives in the shared config location's storage instead, its key
    /// in the platform's secret store.
    page: Option<tilo_files::Dir>,
    /// Secrets keyed by the account id string.
    entries: BTreeMap<String, Secret>,
    custody: SecretCustody,
    load_error: Option<String>,
}

pub(crate) fn explicit_root_custody() -> SecretCustody {
    #[cfg(windows)]
    {
        SecretCustody::WindowsDpapi
    }
    #[cfg(not(windows))]
    {
        SecretCustody::LocalFile
    }
}
impl SecretVault {
    /// Open the vault at the default location (`$XDG_CONFIG_HOME/tilo/accounts/vault`). An empty
    /// vault when nothing is stored. Load failures yield an empty view with
    /// [`Self::recovery_error`] set and writes disabled until recovery and reopen.
    pub fn open() -> Self {
        match crate::config_root() {
            Some(root) => {
                let dir = crate::vault_dir_in(&root);
                Self::open_at(dir, SecretCustody::for_session())
            }
            None => match tilo_files::Dir::open(tilo_paths::AppPaths::for_app("tilo").config()) {
                Ok(page) => Self::open_page(page),
                Err(error) => Self {
                    dir: PathBuf::from("accounts/vault"),
                    page: None,
                    entries: BTreeMap::new(),
                    custody: explicit_root_custody(),
                    load_error: Some(format!("user config directory unavailable: {error}")),
                },
            },
        }
    }

    /// Open the vault under an explicit config root. Windows uses the same
    /// DPAPI custody as the default store; other hosts use a private local key.
    pub fn open_in(config_root: &Path) -> Self {
        let dir = crate::vault_dir_in(config_root);
        let custody = explicit_root_custody();
        Self::open_at(dir, custody)
    }

    fn open_page(page: tilo_files::Dir) -> Self {
        let (entries, load_error) = match load_page_entries(&page) {
            Ok(entries) => (entries, None),
            Err(error) => {
                tracing::warn!(%error, "accounts: vault unavailable; writes disabled until reopen");
                (BTreeMap::new(), Some(error))
            }
        };
        Self {
            dir: PathBuf::from("accounts/vault"),
            page: Some(page),
            entries,
            custody: SecretCustody::System,
            load_error,
        }
    }

    fn open_at(dir: PathBuf, custody: SecretCustody) -> Self {
        let (entries, load_error) = match load_entries(&dir, custody) {
            Ok(entries) => (entries, None),
            Err(error) => {
                tracing::warn!(%error, "accounts: vault unavailable; writes disabled until reopen");
                (BTreeMap::new(), Some(error))
            }
        };
        Self {
            dir,
            page: None,
            entries,
            custody,
            load_error,
        }
    }

    /// A failed initial load requires recovery and a fresh open before saving.
    pub fn recovery_error(&self) -> Option<&str> {
        self.load_error.as_deref()
    }

    pub fn custody(&self) -> SecretCustody {
        self.custody
    }

    /// Store (or replace) `secret` for `id`. In-memory until [`Self::save`].
    pub fn put(&mut self, id: &AccountId, secret: &Secret) {
        self.entries.insert(id.as_str().to_string(), secret.clone());
    }

    /// Fetch the secret for `id`, if the vault holds one.
    pub fn get(&self, id: &AccountId) -> Option<Secret> {
        self.entries.get(id.as_str()).cloned()
    }
    pub(crate) fn contains(&self, id: &AccountId) -> bool {
        self.entries.contains_key(id.as_str())
    }

    /// Internal snapshot used by the typed stores layered over this encrypted
    /// container. Kept crate-private so applications cannot enumerate account
    /// credentials by bypassing their broker.
    pub(crate) fn entries_with_prefix(&self, prefix: &str) -> Vec<(String, Secret)> {
        self.entries
            .iter()
            .filter(|(id, _)| id.starts_with(prefix))
            .map(|(id, secret)| (id.clone(), secret.clone()))
            .collect()
    }

    /// Remove the secret for `id`, returning whether one existed. In-memory until [`Self::save`].
    pub fn remove(&mut self, id: &AccountId) -> bool {
        self.entries.remove(id.as_str()).is_some()
    }

    /// Whether the vault currently holds no secrets.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Seal + write the vault. Best-effort: logs and returns on any error so a read-only config
    /// dir never crashes the caller.
    pub fn save(&self) {
        if let Err(e) = self.try_save() {
            tracing::warn!(error = %e, "accounts: vault write failed");
        }
    }

    /// Seal and atomically persist the vault, returning failures to callers
    /// that must not publish account metadata unless credential custody
    /// succeeded first.
    ///
    /// # Errors
    ///
    /// Returns a descriptive error when the vault directory or key cannot be
    /// prepared, serialization or encryption fails, or the sealed file cannot
    /// be atomically persisted.
    pub fn try_save(&self) -> Result<(), String> {
        if let Some(error) = &self.load_error {
            return Err(format!(
                "vault recovery required; restore access and reopen: {error}"
            ));
        }
        if let Some(page) = &self.page {
            return self.save_page(page);
        }
        // Revalidate the on-disk vault before replacing it, including failures
        // that appeared after this instance was opened.
        load_entries(&self.dir, self.custody)?;
        std::fs::create_dir_all(&self.dir)
            .map_err(|error| format!("could not create vault directory: {error}"))?;
        let key = load_or_create_key(&self.dir, self.custody)
            .ok_or_else(|| "no vault key is available".to_string())?;
        let plaintext = Zeroizing::new(
            serde_json::to_vec(&self.entries)
                .map_err(|error| format!("could not serialize credential vault: {error}"))?,
        );
        let blob = seal(&key, &plaintext)
            .ok_or_else(|| "credential vault encryption failed".to_string())?;
        tilo_files::native::atomic::write(&self.dir.join("vault.enc"), &blob)
            .map_err(|error| format!("could not write credential vault: {error}"))
    }
}

impl SecretVault {
    fn save_page(&self, page: &tilo_files::Dir) -> Result<(), String> {
        load_page_entries(page)?;
        let key = FORMAT
            .load_or_create_system_key(page_vault_exists(page))
            .ok_or_else(|| "no vault key is available".to_string())?;
        let plaintext = Zeroizing::new(
            serde_json::to_vec(&self.entries)
                .map_err(|error| format!("could not serialize credential vault: {error}"))?,
        );
        let blob = seal(&key, &plaintext)
            .ok_or_else(|| "credential vault encryption failed".to_string())?;
        tilo_files::blocking::write_atomic(page, &page_vault(), &blob)
            .map_err(|error| format!("could not write credential vault: {error}"))
    }
}

fn page_vault() -> tilo_files::VPath {
    tilo_files::VPath::new("accounts/vault/vault.enc").unwrap_or_else(|_| tilo_files::VPath::root())
}

fn page_vault_exists(page: &tilo_files::Dir) -> bool {
    tilo_files::blocking::metadata(page, &page_vault()).is_ok()
}

/// As [`load_entries`], for a vault in page storage.
fn load_page_entries(page: &tilo_files::Dir) -> Result<BTreeMap<String, Secret>, String> {
    let blob = match tilo_files::blocking::read(page, &page_vault()) {
        Ok(blob) => blob,
        Err(tilo_files::FsError::NotFound) => return Ok(BTreeMap::new()),
        Err(error) => return Err(format!("could not read credential vault: {error}")),
    };
    let key = FORMAT
        .load_or_create_system_key(true)
        .ok_or_else(|| "credential vault key unavailable".to_string())?;
    let plaintext =
        open(&key, &blob).ok_or_else(|| "credential vault integrity check failed".to_string())?;
    serde_json::from_slice(&plaintext)
        .map_err(|_| "decrypted credential vault is invalid".to_string())
}

/// Read and authenticate existing entries, distinguishing absence from damage.
fn load_entries(dir: &Path, custody: SecretCustody) -> Result<BTreeMap<String, Secret>, String> {
    let blob = match std::fs::read(dir.join("vault.enc")) {
        Ok(blob) => blob,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => return Err(format!("could not read credential vault: {error}")),
    };
    let key = load_or_create_key(dir, custody)
        .ok_or_else(|| "credential vault key unavailable".to_string())?;
    let plaintext =
        open(&key, &blob).ok_or_else(|| "credential vault integrity check failed".to_string())?;
    serde_json::from_slice(&plaintext)
        .map_err(|_| "decrypted credential vault is invalid".to_string())
}

/// Read the vault key from `dir`, generating + persisting a fresh random one the first time.
/// `None` only when the key can neither be read nor created. The single point of key
/// custody — see module docs.
fn load_or_create_key(dir: &Path, custody: SecretCustody) -> Option<VaultKey> {
    FORMAT.load_or_create_key(dir, custody)
}

fn seal(key: &VaultKey, plaintext: &[u8]) -> Option<Vec<u8>> {
    FORMAT.seal(key, plaintext)
}

fn open(key: &VaultKey, blob: &[u8]) -> Option<Zeroizing<Vec<u8>>> {
    FORMAT.open(key, blob)
}

#[cfg(test)]
mod tests {
    use super::*;
    use secret_vault::KEY_LEN;
    #[cfg(windows)]
    use std::time::Duration;

    fn local_file_key(dir: &Path) -> Option<VaultKey> {
        load_or_create_key(dir, SecretCustody::LocalFile)
    }

    fn read_key_bytes(path: &Path) -> std::io::Result<Zeroizing<Vec<u8>>> {
        use std::io::Read as _;
        let mut bytes = Zeroizing::new(Vec::new());
        tilo_files::native::private::read(path)?.read_to_end(&mut bytes)?;
        Ok(bytes)
    }

    #[test]
    fn vault_key_is_created_once_and_owner_only() {
        // Phase `simultaneous_vault_creators_use_the_one_persisted_private_key`.
        {
            let root = temp_root();
            let gate = std::sync::Arc::new(std::sync::Barrier::new(4));
            // Spawn every participant before joining: interleaving spawn/join
            // would deadlock the barrier.
            let mut workers = Vec::with_capacity(4);
            for _ in 0..4 {
                let root = root.clone();
                let gate = std::sync::Arc::clone(&gate);
                workers.push(std::thread::spawn(move || {
                    gate.wait();
                    local_file_key(&root).unwrap()
                }));
            }
            let keys: Vec<_> = workers
                .into_iter()
                .map(|worker| worker.join().unwrap())
                .collect();
            let persisted = read_key_bytes(&root.join("vault.key")).unwrap();
            assert!(
                keys.iter()
                    .all(|key| key.expose_secret().as_slice() == persisted.as_slice())
            );
            assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
            std::fs::remove_dir_all(&root).unwrap();
        }

        // Phase `key_file_is_owner_only`.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let root = temp_root();
            let mut v = SecretVault::open_in(&root);
            v.put(&AccountId::from("a"), &Secret::password("pw"));
            v.save();
            let mode = std::fs::metadata(crate::vault_dir_in(&root).join("vault.key"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "vault key must be owner-only");
            std::fs::remove_dir_all(&root).ok();
        }
    }

    #[cfg(windows)]
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn windows_key_custody_never_leaks_rotates_or_falls_back() {
        // Phase `insecure_windows_vault_key_does_not_rotate_or_overwrite_the_vault`.
        {
            let root = temp_root();
            let mut vault =
                SecretVault::open_at(crate::vault_dir_in(&root), SecretCustody::LocalFile);
            vault.put(&AccountId::from("protected"), &Secret::password("secret"));
            vault.try_save().unwrap();
            let directory = crate::vault_dir_in(&root);
            let key_path = directory.join("vault.key");
            let sealed_path = directory.join("vault.enc");
            let key_before = std::fs::read(&key_path).unwrap();
            let sealed_before = std::fs::read(&sealed_path).unwrap();
            let result = bounded_command::run(
                "icacls.exe",
                &[key_path.to_str().unwrap(), "/grant", "*S-1-1-0:(R)"],
                None,
                4096,
                Duration::from_secs(5),
            )
            .unwrap();
            assert!(result.status.success(), "{result:?}");
            assert!(local_file_key(&directory).is_none());
            assert!(vault.try_save().is_err());
            assert_eq!(std::fs::read(&key_path).unwrap(), key_before);
            assert_eq!(std::fs::read(&sealed_path).unwrap(), sealed_before);
            std::fs::remove_dir_all(&root).unwrap();
        }

        // Phase `windows_dpapi_secret_store_does_not_publish_plaintext_key`.
        {
            let root = temp_root();
            let mut item = crate::SecretItem::login("Owned fixture", "user", "fixture secret");
            let mut store = crate::SecretStore::open_in(&root);
            store.put(&mut item).unwrap();
            let dir = crate::vault_dir_in(&root);
            assert!(!dir.join("vault.key").exists());
            assert!(tilo_files::native::private::read(&dir.join("vault.key.dpapi")).is_ok());
            let reopened = crate::SecretStore::open_in(&root);
            assert!(reopened.recovery_error().is_none());
            assert_eq!(
                reopened.reveal_password(&item.id).unwrap().as_str(),
                "fixture secret"
            );
            std::fs::remove_dir_all(root).unwrap();
        }

        // Phase `windows_dpapi_insecure_protected_key_cannot_replace_existing_vault`.
        {
            let root = temp_root();
            let mut vault = SecretVault::open_in(&root);
            vault.put(&AccountId::from("protected"), &Secret::password("fixture"));
            vault.try_save().unwrap();
            let dir = crate::vault_dir_in(&root);
            let key_path = dir.join("vault.key.dpapi");
            let key_before = std::fs::read(&key_path).unwrap();
            let encrypted = std::fs::read(dir.join("vault.enc")).unwrap();
            let result = bounded_command::run(
                "icacls.exe",
                &[key_path.to_str().unwrap(), "/grant", "*S-1-1-0:(R)"],
                None,
                4096,
                Duration::from_secs(5),
            )
            .unwrap();
            assert!(result.status.success(), "{result:?}");
            assert!(SecretVault::open_in(&root).recovery_error().is_some());
            assert!(vault.try_save().is_err());
            assert!(!dir.join("vault.key").exists());
            assert_eq!(std::fs::read(key_path).unwrap(), key_before);
            assert_eq!(std::fs::read(dir.join("vault.enc")).unwrap(), encrypted);
            std::fs::remove_dir_all(root).unwrap();
        }

        // Phase `windows_dpapi_corrupt_protected_key_never_falls_back_to_a_plaintext_key`.
        {
            let root = temp_root();
            let dir = crate::vault_dir_in(&root);
            let mut vault = SecretVault::open_in(&root);
            vault.put(&AccountId::from("original"), &Secret::password("fixture"));
            vault.try_save().unwrap();
            let encrypted = std::fs::read(dir.join("vault.enc")).unwrap();
            let stray_key = [7; KEY_LEN];
            tilo_files::native::private::create_atomic(&dir.join("vault.key"), &stray_key).unwrap();
            tilo_files::native::private::write_atomic(
                &dir.join("vault.key.dpapi"),
                b"corrupt protected key",
            )
            .unwrap();
            let failed = SecretVault::open_in(&root);
            assert!(failed.recovery_error().is_some());
            assert!(failed.try_save().is_err());
            assert_eq!(
                std::fs::read(dir.join("vault.key")).unwrap(),
                stray_key.as_slice()
            );
            assert_eq!(
                std::fs::read(dir.join("vault.key.dpapi")).unwrap(),
                b"corrupt protected key"
            );
            assert_eq!(std::fs::read(dir.join("vault.enc")).unwrap(), encrypted);
            std::fs::remove_dir_all(root).unwrap();
        }

        // Phase `windows_dpapi_concurrent_creation_returns_one_persisted_key`.
        {
            let root = temp_root();
            let gate = std::sync::Arc::new(std::sync::Barrier::new(4));
            let mut workers = Vec::new();
            for _ in 0..4 {
                let root = root.clone();
                let gate = gate.clone();
                workers.push(std::thread::spawn(move || {
                    gate.wait();
                    load_or_create_key(&root, SecretCustody::WindowsDpapi).unwrap()
                }));
            }
            let keys: Vec<_> = workers
                .into_iter()
                .map(|worker| worker.join().unwrap())
                .collect();
            let persisted = load_or_create_key(&root, SecretCustody::WindowsDpapi).unwrap();
            assert!(keys.iter().all(|key| *key == persisted));
            assert!(!root.join("vault.key").exists());
            assert!(tilo_files::native::private::read(&root.join("vault.key.dpapi")).is_ok());
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn a_vault_that_cannot_be_opened_is_never_overwritten_or_misread() {
        // Phase `missing_key_never_rekeys_an_existing_vault`.
        {
            let root = temp_root();
            let mut vault = SecretVault::open_in(&root);
            vault.put(
                &AccountId::from("existing"),
                &Secret::password("preserve me"),
            );
            vault.try_save().unwrap();
            let dir = crate::vault_dir_in(&root);
            let before = std::fs::read(dir.join("vault.enc")).unwrap();
            std::fs::remove_file(dir.join(active_key_filename())).unwrap();
            let reopened = SecretVault::open_in(&root);
            assert!(reopened.try_save().is_err());
            assert!(!dir.join(active_key_filename()).exists());
            assert_eq!(std::fs::read(dir.join("vault.enc")).unwrap(), before);
            std::fs::remove_dir_all(root).unwrap();
        }

        // Phase `failed_open_cannot_overwrite_vault_after_key_is_restored`.
        {
            let root = temp_root();
            let mut vault = SecretVault::open_in(&root);
            let id = AccountId::from("existing");
            vault.put(&id, &Secret::password("preserve me"));
            vault.try_save().unwrap();
            let dir = crate::vault_dir_in(&root);
            let before = std::fs::read(dir.join("vault.enc")).unwrap();
            let key = std::fs::read(dir.join(active_key_filename())).unwrap();
            std::fs::write(dir.join(active_key_filename()), [0u8; KEY_LEN]).unwrap();
            let mut failed = SecretVault::open_in(&root);
            std::fs::write(dir.join(active_key_filename()), key).unwrap();
            failed.put(&AccountId::from("new"), &Secret::password("new value"));
            assert!(failed.try_save().is_err());
            assert_eq!(std::fs::read(dir.join("vault.enc")).unwrap(), before);
            assert_eq!(
                SecretVault::open_in(&root).get(&id).unwrap().password,
                "preserve me"
            );
            std::fs::remove_dir_all(root).unwrap();
        }

        // Phase `save_preserves_ciphertext_corrupted_after_open`.
        {
            let root = temp_root();
            let mut vault = SecretVault::open_in(&root);
            vault.put(
                &AccountId::from("existing"),
                &Secret::password("preserve me"),
            );
            vault.try_save().unwrap();
            let enc = crate::vault_dir_in(&root).join("vault.enc");
            std::fs::write(&enc, b"corrupt existing vault").unwrap();
            assert!(vault.try_save().is_err());
            assert_eq!(std::fs::read(&enc).unwrap(), b"corrupt existing vault");
            std::fs::remove_dir_all(root).unwrap();
        }

        // Phase `authenticated_but_invalid_document_is_preserved`.
        {
            let root = temp_root();
            let dir = crate::vault_dir_in(&root);
            let key = local_file_key(&dir).unwrap();
            let blob = seal(&key, b"not valid JSON").unwrap();
            std::fs::write(dir.join("vault.enc"), &blob).unwrap();
            let vault = SecretVault::open_in(&root);
            assert!(vault.recovery_error().is_some());
            assert!(vault.try_save().is_err());
            assert_eq!(std::fs::read(dir.join("vault.enc")).unwrap(), blob);
            std::fs::remove_dir_all(root).unwrap();
        }

        // Phase `wrong_key_cannot_decrypt_a_persisted_vault`.
        {
            let root = temp_root();
            let id = AccountId::from("x");
            {
                let mut v = SecretVault::open_in(&root);
                v.put(&id, &Secret::password("pw"));
                v.save();
            }
            // Corrupt the key file: the sealed vault can no longer be opened, and it degrades to empty
            // rather than surfacing garbage.
            let key_path = crate::vault_dir_in(&root).join(active_key_filename());
            std::fs::write(&key_path, [0u8; KEY_LEN]).unwrap();
            let v = SecretVault::open_in(&root);
            assert!(
                v.get(&id).is_none(),
                "a wrong key must not decrypt the vault"
            );
            std::fs::remove_dir_all(&root).ok();
        }
    }

    fn active_key_filename() -> &'static str {
        if cfg!(windows) {
            "vault.key.dpapi"
        } else {
            "vault.key"
        }
    }
    fn temp_root() -> PathBuf {
        std::env::temp_dir().join(format!("tilo-accounts-vault-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn sealed_vault_round_trips_and_never_exposes_the_secret() {
        // Phase `seal_open_round_trips_and_rejects_tampering`.
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

        // Phase `vault_put_save_load_round_trips`.
        {
            let root = temp_root();
            let id = AccountId::from("ada@example.com");
            {
                let mut v = SecretVault::open_in(&root);
                #[cfg(not(windows))]
                assert_eq!(v.custody(), SecretCustody::LocalFile);
                #[cfg(windows)]
                assert_eq!(v.custody(), SecretCustody::WindowsDpapi);
                v.put(&id, &Secret::password("hunter2"));
                v.save();
            }
            // A fresh open decrypts the persisted secret.
            let v = SecretVault::open_in(&root);
            assert_eq!(v.get(&id).unwrap().password, "hunter2");
            std::fs::remove_dir_all(&root).ok();
        }

        // Phase `secret_never_appears_in_the_sealed_file`.
        {
            let root = temp_root();
            let mut v = SecretVault::open_in(&root);
            v.put(&AccountId::from("a"), &Secret::password("s3cr3t-value"));
            v.save();
            let enc = crate::vault_dir_in(&root).join("vault.enc");
            let raw = std::fs::read(&enc).unwrap();
            assert!(
                !raw.windows(b"s3cr3t-value".len())
                    .any(|w| w == b"s3cr3t-value"),
                "password must not be recoverable from the sealed file"
            );
            std::fs::remove_dir_all(&root).ok();
        }
    }
}
