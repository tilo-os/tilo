//! Sealed local secret vaults and the custody of their key.
//!
//! A vault is one file, `vault.enc`, sealed as a `tilo-seal` at-rest
//! envelope (XChaCha20-Poly1305, versioned authenticated header, random
//! 192-bit nonce) under a 256-bit key. The envelope is bound to the vault's
//! [`VaultFormat`] identity, so a vault never opens as another kind of vault.
//! What the plaintext means is the caller's business; this crate only
//! seals it and keeps the key.
//!
//! Each consumer describes its on-disk identity with a [`VaultFormat`]: the
//! magic bytes, the secret-store attributes and the DPAPI entropy. Changing
//! any of those strands every existing vault of that format.
//!
//! # Key custody
//!
//! - [`Custody::System`]: the platform's secret store holds the key apart
//!   from the ciphertext, through `tilo-secrets` — the Secret Service on
//!   Linux (the item `secret-tool` used to write), the Keychain on macOS. An
//!   owner-only `vault.key` left by local custody migrates only after a
//!   verified round-trip, then is removed.
//! - [`Custody::LocalFile`]: an owner-only `vault.key` beside the vault, for
//!   headless and test environments without Secret Service.
//! - `Custody::WindowsDpapi`: current-user DPAPI with a private
//!   `vault.key.dpapi`; a DPAPI failure never falls back to plaintext.
//!
//! Every custody refuses to replace a key it cannot read, and refuses to mint
//! a key when `vault.enc` already exists without one: a fresh key would make
//! the existing vault permanently undecryptable. New local keys are published
//! with an exclusive create, so concurrent creators all converge on the one
//! key that reached disk.

use std::io::Read as _;
use std::path::Path;

use tilo_seal::Label;
use tilo_seal::envelope::{self, Context};
use tilo_secrets::{PutOptions, SecretStore};
use zeroize::Zeroizing;

#[cfg(windows)]
mod windows;

/// Symmetric key width (256-bit).
pub const KEY_LEN: usize = 32;

/// Name of the sealed vault inside its directory.
pub const VAULT_FILE: &str = "vault.enc";
/// Name of the owner-only local key inside the vault directory.
pub const KEY_FILE: &str = "vault.key";

/// A vault key: wiped from memory on drop, compared in constant time and
/// never printed by `Debug`.
pub type VaultKey = tilo_seal::SymmetricKey;

/// Copy exactly [`KEY_LEN`] bytes into a [`VaultKey`].
fn key_from_bytes(bytes: &[u8]) -> Option<VaultKey> {
    VaultKey::from_slice(bytes).ok()
}

/// Where a vault's key is held.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Custody {
    /// The platform's secret store through `tilo-secrets`: the Secret
    /// Service on Linux, the Keychain on macOS.
    System,
    /// An owner-only key file beside the vault.
    LocalFile,
    /// Current-user DPAPI protecting a private key file.
    #[cfg(windows)]
    WindowsDpapi,
}

impl Custody {
    /// A short user-facing name for where the key is held.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::System => "System keyring",
            Self::LocalFile => "Local encrypted storage",
            #[cfg(windows)]
            Self::WindowsDpapi => "Windows user protection (DPAPI)",
        }
    }

    /// The strongest custody this session offers: DPAPI on Windows,
    /// otherwise the platform's secret store when it answers, else a local
    /// key.
    pub fn for_session() -> Self {
        #[cfg(windows)]
        {
            Self::WindowsDpapi
        }
        #[cfg(not(windows))]
        {
            if system_store_available() {
                Self::System
            } else {
                Self::LocalFile
            }
        }
    }
}

/// The on-disk identity of one kind of vault. Every field is part of the
/// persisted format: changing one makes existing vaults of this kind unreadable.
#[derive(Debug)]
pub struct VaultFormat {
    /// Short name identifying the vault in log messages.
    pub name: &'static str,
    /// Four bytes prefixing every sealed blob, so a stray file is rejected
    /// before it reaches the AEAD.
    pub magic: &'static [u8; 4],
    /// Label shown by keyring managers for the Secret Service item.
    pub secret_service_label: &'static str,
    /// Attribute pairs identifying the key in the platform's secret store:
    /// `application` names the `tilo-secrets` store and `purpose` the key in
    /// it (the Secret Service item carries exactly these attributes).
    pub secret_service_attributes: &'static [(&'static str, &'static str)],
    /// Optional entropy mixed into DPAPI protection of the key.
    pub dpapi_entropy: &'static [u8],
}

impl VaultFormat {
    /// The bytes every envelope of this format is bound to: the magic and
    /// the name, so neither a renamed nor a re-magicked format opens it.
    fn identity(&self) -> Vec<u8> {
        let mut identity = Vec::with_capacity(self.magic.len().saturating_add(self.name.len()));
        identity.extend_from_slice(self.magic);
        identity.extend_from_slice(self.name.as_bytes());
        identity
    }

    /// Seal `plaintext` under `key`. `None` if the CSPRNG or the cipher fails.
    pub fn seal(&self, key: &VaultKey, plaintext: &[u8]) -> Option<Vec<u8>> {
        let identity = self.identity();
        envelope::seal(key, Context::new(Label::SecretVault, &identity), plaintext).ok()
    }

    /// Open a blob produced by [`Self::seal`], returning the zeroized
    /// plaintext. `None` on any framing mismatch or authentication failure: a
    /// tampered or truncated file, or a vault of another format, never yields
    /// bytes.
    pub fn open(&self, key: &VaultKey, blob: &[u8]) -> Option<Zeroizing<Vec<u8>>> {
        let identity = self.identity();
        envelope::open(key, Context::new(Label::SecretVault, &identity), blob).ok()
    }

    /// Load the key for the vault in `dir`, creating one on first use.
    ///
    /// `None` when the key can neither be read nor safely created: an
    /// existing key that cannot be read or is malformed is never replaced,
    /// and no key is minted while `vault.enc` exists without one.
    pub fn load_or_create_key(&self, dir: &Path, custody: Custody) -> Option<VaultKey> {
        match custody {
            Custody::System => self.system_key(dir, &self.system_store()),
            Custody::LocalFile => self.local_file_key(dir),
            #[cfg(windows)]
            Custody::WindowsDpapi => match windows::load_or_create_key(self, dir) {
                Ok(key) => key_from_bytes(key.as_slice()),
                Err(error) => {
                    tracing::warn!(vault = self.name, %error, "Windows protected key unavailable; refusing fallback");
                    None
                }
            },
        }
    }

    /// The key of a vault that has no native directory (one in the page's
    /// storage on the web): the platform's secret store only, with no local
    /// key to migrate. A key is created only when `vault_exists` is false.
    pub fn load_or_create_system_key(&self, vault_exists: bool) -> Option<VaultKey> {
        self.system_only_key(vault_exists, &self.system_store())
    }

    /// [`Self::load_or_create_system_key`] in `store`.
    pub fn load_or_create_system_key_in(
        &self,
        vault_exists: bool,
        store: &SecretStore,
    ) -> Option<VaultKey> {
        self.system_only_key(vault_exists, store)
    }

    fn system_only_key(&self, vault_exists: bool, store: &SecretStore) -> Option<VaultKey> {
        match self.lookup_system_key(store) {
            Ok(Some(key)) => return Some(key),
            Ok(None) if vault_exists => {
                tracing::warn!(
                    vault = self.name,
                    "existing vault key is missing; refusing replacement"
                );
                return None;
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(vault = self.name, %error, "secret store lookup failed");
                return None;
            }
        }
        let key = VaultKey::generate().ok()?;
        if let Err(error) = self.store_system_key(store, &key) {
            tracing::warn!(vault = self.name, %error, "secret store key creation failed");
            return None;
        }
        match self.lookup_system_key(store) {
            Ok(Some(stored)) if stored == key => Some(key),
            _ => {
                tracing::warn!(
                    vault = self.name,
                    "the secret store did not return the key after storing it"
                );
                None
            }
        }
    }

    /// [`Self::load_or_create_key`] with [`Custody::System`], in `store`
    /// rather than the platform's. For tests and hosts with their own store.
    pub fn load_or_create_key_in(&self, dir: &Path, store: &SecretStore) -> Option<VaultKey> {
        self.system_key(dir, store)
    }

    /// The store and key naming this format's vault key: the `application`
    /// and `purpose` attributes, else the format's name.
    fn system_item(&self) -> (&'static str, &'static str) {
        let attribute = |name: &str| {
            self.secret_service_attributes
                .iter()
                .find(|(attribute, _)| *attribute == name)
                .map(|(_, value)| *value)
        };
        (
            attribute("application").unwrap_or("Tilo"),
            attribute("purpose").unwrap_or(self.name),
        )
    }

    fn system_store(&self) -> SecretStore {
        SecretStore::for_app(self.system_item().0)
    }

    fn system_key(&self, dir: &Path, store: &SecretStore) -> Option<VaultKey> {
        match self.lookup_system_key(store) {
            Ok(Some(key)) => return Some(key),
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(vault = self.name, %error, "secret store lookup failed; refusing local fallback");
                return None;
            }
        }

        let local_path = dir.join(KEY_FILE);
        let (key, migrated) = match read_key_bytes(&local_path) {
            Ok(bytes) if bytes.len() == KEY_LEN => (key_from_bytes(&bytes)?, true),
            Ok(_) => {
                tracing::warn!(
                    vault = self.name,
                    "local vault key is malformed; not replacing it"
                );
                return None;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if vault_exists(dir) {
                    tracing::warn!(
                        vault = self.name,
                        "existing vault key is missing; refusing replacement"
                    );
                    return None;
                }
                (VaultKey::generate().ok()?, false)
            }
            Err(error) => {
                tracing::warn!(vault = self.name, %error, "local vault key could not be read for migration");
                return None;
            }
        };

        if let Err(error) = self.store_system_key(store, &key) {
            tracing::warn!(vault = self.name, %error, "secret store key creation failed");
            return None;
        }
        match self.lookup_system_key(store) {
            Ok(Some(stored)) if stored == key => {
                if migrated && let Err(error) = std::fs::remove_file(&local_path) {
                    tracing::warn!(vault = self.name, %error, "migrated vault key but could not remove local copy");
                }
                Some(key)
            }
            _ => {
                tracing::warn!(
                    vault = self.name,
                    "the secret store did not return the key after storing it"
                );
                None
            }
        }
    }

    fn lookup_system_key(&self, store: &SecretStore) -> Result<Option<VaultKey>, String> {
        let stored = store
            .get_blocking(self.system_item().1)
            .map_err(|error| format!("secret store lookup failed: {error}"))?;
        let Some(stored) = stored else {
            return Ok(None);
        };
        let encoded = std::str::from_utf8(stored.expose())
            .map_err(|_| "the secret store returned non-UTF-8 key material".to_string())?;
        if encoded.trim().is_empty() {
            Ok(None)
        } else {
            decode_key(encoded.trim()).map(Some)
        }
    }

    fn store_system_key(&self, store: &SecretStore, key: &VaultKey) -> Result<(), String> {
        // Stored as lower-case hex, as `secret-tool` stored it, so keys
        // written before and after this move read the same.
        let encoded = Zeroizing::new(encode_key(key.expose_secret()));
        store
            .put_blocking(
                self.system_item().1,
                encoded.as_bytes(),
                PutOptions {
                    label: Some(self.secret_service_label.to_owned()),
                },
            )
            .map_err(|error| format!("secret store refused the vault key: {error}"))
    }

    fn local_file_key(&self, dir: &Path) -> Option<VaultKey> {
        let path = dir.join(KEY_FILE);
        match read_key_bytes(&path) {
            Ok(bytes) if bytes.len() == KEY_LEN => key_from_bytes(&bytes),
            // A file exists but isn't a well-formed key: refuse to clobber it,
            // since regenerating would strand any vault it already sealed.
            Ok(_) => {
                tracing::warn!(
                    vault = self.name,
                    "vault key file is malformed; not overwriting"
                );
                None
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if vault_exists(dir) {
                    tracing::warn!(
                        vault = self.name,
                        "existing vault key is missing; refusing replacement"
                    );
                    return None;
                }
                let key = VaultKey::generate().ok()?;
                if let Err(error) = std::fs::create_dir_all(dir) {
                    tracing::warn!(vault = self.name, %error, "could not create vault dir for key");
                    return None;
                }
                if private_fs::create_atomic(&path, key.expose_secret()).is_err() {
                    tracing::warn!(vault = self.name, "vault key write failed");
                    return None;
                }
                // Another process may have won the exclusive publication. Always
                // reload its key instead of encrypting with our discarded candidate.
                drop(key);
                let bytes = read_key_bytes(&path).ok()?;
                if bytes.len() != KEY_LEN {
                    return None;
                }
                key_from_bytes(&bytes)
            }
            Err(error) => {
                tracing::warn!(vault = self.name, %error, "vault key cannot be read; refusing replacement");
                None
            }
        }
    }
}

/// Whether the platform's secret store answers on this session, and
/// synchronously (the browser's store is async-only, which this crate's
/// blocking API cannot use).
#[cfg(not(windows))]
pub fn system_store_available() -> bool {
    tilo_secrets::availability(tilo_secrets::Feature::Blocking)
        == tilo_secrets::Availability::Available
        && SecretStore::for_app("Tilo").probe_blocking().is_ok()
}

/// A vault whose presence cannot be ruled out counts as existing, so an I/O
/// error never authorizes minting a replacement key.
fn vault_exists(dir: &Path) -> bool {
    dir.join(VAULT_FILE).try_exists().unwrap_or(true)
}

/// Keep private-key validation tied to the handle that supplies the bytes.
/// One byte past the key width is enough to recognise a malformed key.
fn read_key_bytes(path: &Path) -> std::io::Result<Zeroizing<Vec<u8>>> {
    let mut bytes = Zeroizing::new(Vec::new());
    private_fs::read(path)?.take(33).read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn encode_key(key: &[u8; KEY_LEN]) -> String {
    // One exactly-sized allocation, so no partial copy of the key is left
    // behind in a freed buffer; the caller wraps the result in `Zeroizing`.
    hex::encode(key)
}

fn decode_key(encoded: &str) -> Result<VaultKey, String> {
    if encoded.len() != KEY_LEN * 2 {
        return Err("stored vault key has the wrong length".into());
    }
    // Decode straight into the zeroizing buffer; no intermediate `Vec` holds
    // key bytes.
    let mut key = Zeroizing::new([0u8; KEY_LEN]);
    hex::decode_to_slice(encoded, key.as_mut_slice())
        .map_err(|_| "stored vault key is not hexadecimal".to_string())?;
    key_from_bytes(key.as_slice()).ok_or_else(|| "stored vault key has the wrong length".into())
}

#[cfg(test)]
mod tests;
