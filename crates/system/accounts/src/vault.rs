//! The encrypted **secret vault** — credentials at rest, never in the plain
//! [`crate::AccountStore`] JSON.
//!
//! Every credential an [`crate::Account`] needs is a long-lived bearer secret, sealed with
//! **XChaCha20-Poly1305** (192-bit-nonce AEAD, audited `RustCrypto` stack) under a 256-bit key,
//! stored as `vault.enc` under `$XDG_CONFIG_HOME/qos/accounts/vault/`. A fresh random nonce
//! is drawn per write, so re-saving an unchanged vault still produces distinct ciphertext and
//! no nonce is ever reused — the same pattern the authenticator (totp) vault uses.
//!
//! # Key custody
//!
//! The symmetric key lives in `vault.key` next to the ciphertext, created with the OS CSPRNG
//! on first save and written `0600` on unix — protecting the blob from a backup, a sync, or
//! another local user, while keeping the vault recoverable by backing up the config directory.
//!
//! **TODO (hardening):** the key is a local random file today. [`load_or_create_key`] is the
//! single seam for stronger custody: when the compositor exposes a keyring/TPM-sealed
//! device-key capability over its Unix-socket channel, only that function changes to request
//! an AEAD seal/open from the device key instead of a local file, keeping the on-disk format
//! identical. Until then a local reader with config-directory access sees both key and
//! ciphertext.
//!
//! Persistence is best-effort: every IO or crypto error is logged via `tracing` and
//! swallowed, so a read-only or missing config dir never crashes an app.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::AccountId;

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

/// Magic + format byte prefixing every sealed vault, so a stray file or a
/// format bump is detected before bytes are handed to the AEAD.
const SEAL_MAGIC: &[u8; 4] = b"QASV"; // qOS Accounts Secret Vault
const SEAL_FORMAT: u8 = 1;

/// XChaCha20-Poly1305 nonce width (192-bit / 24-byte extended nonce).
const NONCE_LEN: usize = 24;
/// Symmetric key width (256-bit).
const KEY_LEN: usize = 32;

/// The encrypted, per-[`AccountId`] credential store. Held in memory after
/// [`SecretVault::open`]; mutations are staged and written by [`SecretVault::save`].
#[derive(Debug)]
pub struct SecretVault {
    /// The vault directory (`.../qos/accounts/vault`).
    dir: PathBuf,
    /// Secrets keyed by the account id string.
    entries: BTreeMap<String, Secret>,
}

impl SecretVault {
    /// Open the vault at the default location (`$XDG_CONFIG_HOME/qos/accounts/vault`). An empty
    /// vault when nothing is stored or the config root can't be resolved — never an error.
    pub fn open() -> Self {
        match crate::config_root() {
            Some(root) => Self::open_in(&root),
            None => Self {
                dir: PathBuf::from("accounts/vault"),
                entries: BTreeMap::new(),
            },
        }
    }

    /// Open the vault under an explicit config root (`root/accounts/vault`). The seam tests and
    /// callers that want a non-default location use.
    pub fn open_in(config_root: &Path) -> Self {
        let dir = crate::vault_dir_in(config_root);
        let entries = load_entries(&dir);
        Self { dir, entries }
    }

    /// Store (or replace) `secret` for `id`. In-memory until [`Self::save`].
    pub fn put(&mut self, id: &AccountId, secret: &Secret) {
        self.entries.insert(id.as_str().to_string(), secret.clone());
    }

    /// Fetch the secret for `id`, if the vault holds one.
    pub fn get(&self, id: &AccountId) -> Option<Secret> {
        self.entries.get(id.as_str()).cloned()
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
        if let Err(e) = std::fs::create_dir_all(&self.dir) {
            tracing::warn!(error = %e, "accounts: could not create vault dir");
            return;
        }
        let Some(key) = load_or_create_key(&self.dir) else {
            tracing::warn!("accounts: no vault key available; secrets kept in memory only");
            return;
        };
        let plaintext = match serde_json::to_vec(&self.entries) {
            Ok(bytes) => Zeroizing::new(bytes),
            Err(e) => {
                tracing::warn!(error = %e, "accounts: vault serialize failed");
                return;
            }
        };
        let Some(blob) = seal(&key, &plaintext) else {
            tracing::warn!("accounts: vault encryption failed");
            return;
        };
        if let Err(e) = atomic_file::write(&self.dir.join("vault.enc"), &blob) {
            tracing::warn!(error = %e, "accounts: vault write failed");
        }
    }
}

/// Read + decrypt the vault entries under `dir`. Empty on anything that isn't a well-formed,
/// authenticated blob.
fn load_entries(dir: &Path) -> BTreeMap<String, Secret> {
    let enc = dir.join("vault.enc");
    if !enc.exists() {
        return BTreeMap::new();
    }
    match (load_or_create_key(dir), std::fs::read(&enc)) {
        (Some(key), Ok(blob)) => {
            if let Some(plaintext) = open(&key, &blob) {
                serde_json::from_slice(&plaintext).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "accounts: decrypted vault parse failed; starting empty");
            BTreeMap::new()
        })
            } else {
                tracing::warn!("accounts: vault decrypt failed; starting empty");
                BTreeMap::new()
            }
        }
        _ => BTreeMap::new(),
    }
}

/// Read the vault key from `dir`, generating + persisting a fresh random one the first time.
/// `None` only when the key can neither be read nor created. The single point of key
/// custody — see module docs for the TPM/keyring seam that will replace it.
fn load_or_create_key(dir: &Path) -> Option<Zeroizing<[u8; KEY_LEN]>> {
    let path = dir.join("vault.key");
    match std::fs::read(&path) {
        Ok(bytes) if bytes.len() == KEY_LEN => {
            let mut key = Zeroizing::new([0u8; KEY_LEN]);
            key.copy_from_slice(&bytes);
            Some(key)
        }
        // A file exists but isn't a well-formed key: refuse to clobber it, since regenerating
        // would strand any vault it already sealed.
        Ok(_) => {
            tracing::warn!("accounts: vault key file is malformed; not overwriting");
            None
        }
        Err(_) => {
            let mut key = Zeroizing::new([0u8; KEY_LEN]);
            if getrandom::getrandom(key.as_mut_slice()).is_err() {
                return None;
            }
            if let Err(e) = std::fs::create_dir_all(dir) {
                tracing::warn!(error = %e, "accounts: could not create vault dir for key");
                return None;
            }
            if write_key_file(&path, &key).is_err() {
                tracing::warn!("accounts: vault key write failed");
                return None;
            }
            Some(key)
        }
    }
}

/// Seal `plaintext` under `key`: `MAGIC || format || nonce(24) || AEAD`. `None` if the
/// CSPRNG or the cipher fails.
fn seal(key: &[u8; KEY_LEN], plaintext: &[u8]) -> Option<Vec<u8>> {
    use chacha20poly1305::aead::Aead;
    use chacha20poly1305::{Key, KeyInit, XChaCha20Poly1305, XNonce};

    let mut nonce_bytes = [0u8; NONCE_LEN];
    getrandom::getrandom(&mut nonce_bytes).ok()?;
    let cipher = XChaCha20Poly1305::new(Key::from_slice(key));
    let ciphertext = cipher
        .encrypt(XNonce::from_slice(&nonce_bytes), plaintext)
        .ok()?;

    let mut out = Vec::with_capacity(SEAL_MAGIC.len() + 1 + NONCE_LEN + ciphertext.len());
    out.extend_from_slice(SEAL_MAGIC);
    out.push(SEAL_FORMAT);
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ciphertext);
    Some(out)
}

/// Open a blob produced by [`seal`], returning the zeroized plaintext. `None` on any framing
/// mismatch or authentication failure — a tampered or truncated file never yields bytes.
fn open(key: &[u8; KEY_LEN], blob: &[u8]) -> Option<Zeroizing<Vec<u8>>> {
    use chacha20poly1305::aead::Aead;
    use chacha20poly1305::{Key, KeyInit, XChaCha20Poly1305, XNonce};

    let rest = blob.strip_prefix(SEAL_MAGIC)?;
    let (&format, rest) = rest.split_first()?;
    if format != SEAL_FORMAT || rest.len() < NONCE_LEN {
        return None;
    }
    let (nonce_bytes, ciphertext) = rest.split_at(NONCE_LEN);
    let cipher = XChaCha20Poly1305::new(Key::from_slice(key));
    cipher
        .decrypt(XNonce::from_slice(nonce_bytes), ciphertext)
        .ok()
        .map(Zeroizing::new)
}

/// Write the key file atomically, restricted to `0600` before it is ever visible at its
/// final path.
fn write_key_file(path: &Path, key: &[u8; KEY_LEN]) -> std::io::Result<()> {
    atomic_file::write_with_mode(path, key, 0o600)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root() -> PathBuf {
        std::env::temp_dir().join(format!("qos-accounts-vault-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn seal_open_round_trips_and_rejects_tampering() {
        let key = [7u8; KEY_LEN];
        let plaintext = b"secret vault bytes";
        let blob = seal(&key, plaintext).expect("seal");
        // The sealed blob is not the plaintext.
        assert!(!blob.windows(plaintext.len()).any(|w| w == plaintext));
        // It carries the magic header.
        assert_eq!(&blob[..4], SEAL_MAGIC.as_slice());
        // Round-trips under the same key.
        assert_eq!(open(&key, &blob).unwrap().as_slice(), plaintext);
        // A different key can't open it.
        assert!(open(&[9u8; KEY_LEN], &blob).is_none());
        // A single flipped ciphertext byte fails authentication.
        let mut tampered = blob;
        let last = tampered.len() - 1;
        tampered[last] ^= 0x01;
        assert!(open(&key, &tampered).is_none());
    }

    #[test]
    fn vault_put_save_load_round_trips() {
        let root = temp_root();
        let id = AccountId::from("ada@example.com");
        {
            let mut v = SecretVault::open_in(&root);
            v.put(&id, &Secret::password("hunter2"));
            v.save();
        }
        // A fresh open decrypts the persisted secret.
        let v = SecretVault::open_in(&root);
        assert_eq!(v.get(&id).unwrap().password, "hunter2");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn secret_never_appears_in_the_sealed_file() {
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

    #[test]
    fn wrong_key_cannot_decrypt_a_persisted_vault() {
        let root = temp_root();
        let id = AccountId::from("x");
        {
            let mut v = SecretVault::open_in(&root);
            v.put(&id, &Secret::password("pw"));
            v.save();
        }
        // Corrupt the key file: the sealed vault can no longer be opened, and it degrades to empty
        // rather than surfacing garbage.
        let key_path = crate::vault_dir_in(&root).join("vault.key");
        std::fs::write(&key_path, [0u8; KEY_LEN]).unwrap();
        let v = SecretVault::open_in(&root);
        assert!(
            v.get(&id).is_none(),
            "a wrong key must not decrypt the vault"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[cfg(unix)]
    #[test]
    fn key_file_is_owner_only() {
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
