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
//! On a normal desktop session the symmetric key is held by the freedesktop
//! Secret Service through `secret-tool`; it is not stored beside the
//! ciphertext. Headless/test environments without Secret Service use an
//! owner-only local key and can report that weaker custody to their caller.
//!
//! Persistence is best-effort: every IO or crypto error is logged via `tracing` and
//! swallowed, so a read-only or missing config dir never crashes an app.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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
    custody: SecretCustody,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SecretCustody {
    SecretService,
    LocalFile,
}

impl SecretVault {
    /// Open the vault at the default location (`$XDG_CONFIG_HOME/qos/accounts/vault`). An empty
    /// vault when nothing is stored or the config root can't be resolved — never an error.
    pub fn open() -> Self {
        match crate::config_root() {
            Some(root) => {
                let dir = crate::vault_dir_in(&root);
                let custody = if secret_service_available() {
                    SecretCustody::SecretService
                } else {
                    SecretCustody::LocalFile
                };
                let entries = load_entries(&dir, custody);
                Self {
                    dir,
                    entries,
                    custody,
                }
            }
            None => Self {
                dir: PathBuf::from("accounts/vault"),
                entries: BTreeMap::new(),
                custody: SecretCustody::LocalFile,
            },
        }
    }

    /// Open the vault under an explicit config root (`root/accounts/vault`). The seam tests and
    /// callers that want a non-default location use.
    pub fn open_in(config_root: &Path) -> Self {
        let dir = crate::vault_dir_in(config_root);
        let custody = SecretCustody::LocalFile;
        let entries = load_entries(&dir, custody);
        Self {
            dir,
            entries,
            custody,
        }
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
        atomic_file::write(&self.dir.join("vault.enc"), &blob)
            .map_err(|error| format!("could not write credential vault: {error}"))
    }
}

/// Read + decrypt the vault entries under `dir`. Empty on anything that isn't a well-formed,
/// authenticated blob.
fn load_entries(dir: &Path, custody: SecretCustody) -> BTreeMap<String, Secret> {
    let enc = dir.join("vault.enc");
    if !enc.exists() {
        return BTreeMap::new();
    }
    match (load_or_create_key(dir, custody), std::fs::read(&enc)) {
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
fn load_or_create_key(dir: &Path, custody: SecretCustody) -> Option<Zeroizing<[u8; KEY_LEN]>> {
    match custody {
        SecretCustody::SecretService => secret_service_key(dir),
        SecretCustody::LocalFile => local_file_key(dir),
    }
}

fn secret_service_available() -> bool {
    Command::new("secret-tool")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn secret_service_key(dir: &Path) -> Option<Zeroizing<[u8; KEY_LEN]>> {
    match lookup_secret_service_key() {
        Ok(Some(key)) => return Some(key),
        Ok(None) => {}
        Err(error) => {
            tracing::warn!(%error, "accounts: Secret Service lookup failed; refusing local fallback");
            return None;
        }
    }

    let local_path = dir.join("vault.key");
    let (key, migrated) = match std::fs::read(&local_path) {
        Ok(bytes) if bytes.len() == KEY_LEN => {
            let mut key = Zeroizing::new([0u8; KEY_LEN]);
            key.copy_from_slice(&bytes);
            (key, true)
        }
        Ok(_) => {
            tracing::warn!("accounts: local vault key is malformed; not replacing it");
            return None;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut key = Zeroizing::new([0u8; KEY_LEN]);
            getrandom::getrandom(key.as_mut_slice()).ok()?;
            (key, false)
        }
        Err(error) => {
            tracing::warn!(%error, "accounts: local vault key could not be read for migration");
            return None;
        }
    };

    if let Err(error) = store_secret_service_key(&key) {
        tracing::warn!(%error, "accounts: Secret Service key creation failed");
        return None;
    }
    match lookup_secret_service_key() {
        Ok(Some(stored)) if stored.as_ref() == key.as_ref() => {
            if migrated && let Err(error) = std::fs::remove_file(&local_path) {
                tracing::warn!(%error, "accounts: migrated vault key but could not remove local copy");
            }
            Some(key)
        }
        _ => {
            tracing::warn!("accounts: Secret Service did not return the key after storing it");
            None
        }
    }
}

fn lookup_secret_service_key() -> Result<Option<Zeroizing<[u8; KEY_LEN]>>, String> {
    let output = Command::new("secret-tool")
        .args(["lookup", "application", "qOS", "purpose", "account-vault"])
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("secret-tool could not start: {error}"))?;
    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return if error.is_empty() {
            Ok(None)
        } else {
            Err(error)
        };
    }
    let encoded = String::from_utf8(output.stdout)
        .map_err(|_| "Secret Service returned non-UTF-8 key material".to_string())?;
    if encoded.trim().is_empty() {
        Ok(None)
    } else {
        decode_key(encoded.trim()).map(Some)
    }
}

fn store_secret_service_key(key: &[u8; KEY_LEN]) -> Result<(), String> {
    let mut child = Command::new("secret-tool")
        .args([
            "store",
            "--label=qOS account credentials",
            "application",
            "qOS",
            "purpose",
            "account-vault",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("secret-tool could not start: {error}"))?;
    child
        .stdin
        .take()
        .ok_or_else(|| "secret-tool stdin unavailable".to_string())?
        .write_all(encode_key(key).as_bytes())
        .map_err(|error| format!("could not send key to Secret Service: {error}"))?;
    let output = child
        .wait_with_output()
        .map_err(|error| format!("secret-tool wait failed: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

fn encode_key(key: &[u8; KEY_LEN]) -> String {
    use std::fmt::Write as _;
    let mut encoded = String::with_capacity(KEY_LEN * 2);
    for byte in key {
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

fn decode_key(encoded: &str) -> Result<Zeroizing<[u8; KEY_LEN]>, String> {
    if encoded.len() != KEY_LEN * 2 {
        return Err("Secret Service key has the wrong length".into());
    }
    let mut key = Zeroizing::new([0u8; KEY_LEN]);
    for (byte, pair) in key.iter_mut().zip(encoded.as_bytes().chunks_exact(2)) {
        // `chunks_exact(2)` guarantees `pair` is exactly two bytes; the pattern expresses
        // that instead of indexing into it.
        let &[hi, lo] = pair else {
            return Err("Secret Service key is not hexadecimal".to_string());
        };
        let high = hex_nibble(hi)?;
        let low = hex_nibble(lo)?;
        *byte = high
            .checked_mul(16)
            .and_then(|value| value.checked_add(low))
            .ok_or_else(|| "Secret Service key is not hexadecimal".to_string())?;
    }
    Ok(key)
}

fn hex_nibble(byte: u8) -> Result<u8, String> {
    char::from(byte)
        .to_digit(16)
        .and_then(|value| u8::try_from(value).ok())
        .ok_or_else(|| "Secret Service key is not hexadecimal".into())
}

fn local_file_key(dir: &Path) -> Option<Zeroizing<[u8; KEY_LEN]>> {
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

    let mut out = Vec::with_capacity(
        SEAL_MAGIC
            .len()
            .saturating_add(1)
            .saturating_add(NONCE_LEN)
            .saturating_add(ciphertext.len()),
    );
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
    fn secret_service_key_encoding_round_trips() {
        let key = [0xa5; KEY_LEN];
        assert_eq!(decode_key(&encode_key(&key)).unwrap().as_ref(), &key);
        assert!(decode_key("too-short").is_err());
    }

    #[test]
    fn vault_put_save_load_round_trips() {
        let root = temp_root();
        let id = AccountId::from("ada@example.com");
        {
            let mut v = SecretVault::open_in(&root);
            assert_eq!(v.custody(), SecretCustody::LocalFile);
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
