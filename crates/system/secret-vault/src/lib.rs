//! Sealed local secret vaults and the custody of their key.
//!
//! A vault is one file, `vault.enc`, sealed with **XChaCha20-Poly1305**
//! (192-bit-nonce AEAD, audited `RustCrypto` stack) under a 256-bit key:
//! `magic(4) || format(1) || nonce(24) || ciphertext+tag`. A fresh random
//! nonce is drawn per write, so re-saving unchanged plaintext still produces
//! distinct ciphertext and no nonce is ever reused. What the plaintext means
//! is the caller's business; this crate only seals it and keeps the key.
//!
//! Each consumer describes its on-disk identity with a [`VaultFormat`]: the
//! magic bytes, the Secret Service attributes and the DPAPI entropy. Changing
//! any of those strands every existing vault of that format.
//!
//! # Key custody
//!
//! - [`Custody::SecretService`]: the freedesktop Secret Service holds the key
//!   through `secret-tool`, apart from the ciphertext. An owner-only
//!   `vault.key` left by local custody migrates only after a verified
//!   Secret Service round-trip, then is removed.
//! - [`Custody::LocalFile`]: an owner-only `vault.key` beside the vault, for
//!   headless and test environments without Secret Service.
//! - `Custody::WindowsDpapi`: current-user DPAPI with a private
//!   `vault.key.dpapi`. Legacy private keys migrate only after a verified
//!   restore; a DPAPI failure never falls back to plaintext.
//!
//! Every custody refuses to replace a key it cannot read, and refuses to mint
//! a key when `vault.enc` already exists without one: a fresh key would make
//! the existing vault permanently undecryptable. New local keys are published
//! with an exclusive create, so concurrent creators all converge on the one
//! key that reached disk.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

use std::io::Read as _;
use std::path::Path;
use std::time::Duration;

use rand_core::{OsRng, RngCore as _};
use zeroize::Zeroizing;

#[cfg(windows)]
mod windows;

/// Symmetric key width (256-bit).
pub const KEY_LEN: usize = 32;
/// XChaCha20-Poly1305 nonce width (192-bit / 24-byte extended nonce).
const NONCE_LEN: usize = 24;
/// Format byte following the magic, so a format bump is detected before
/// bytes are handed to the AEAD.
const SEAL_FORMAT: u8 = 1;

/// Name of the sealed vault inside its directory.
pub const VAULT_FILE: &str = "vault.enc";
/// Name of the owner-only local key inside the vault directory.
pub const KEY_FILE: &str = "vault.key";

/// A vault key, wiped from memory on drop.
pub type VaultKey = Zeroizing<[u8; KEY_LEN]>;

/// Where a vault's key is held.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Custody {
    /// The freedesktop Secret Service, through `secret-tool`.
    SecretService,
    /// An owner-only key file beside the vault.
    LocalFile,
    /// Current-user DPAPI protecting a private key file.
    #[cfg(windows)]
    WindowsDpapi,
}

impl Custody {
    /// The strongest custody this session offers: DPAPI on Windows,
    /// otherwise Secret Service when `secret-tool` answers, else a local key.
    pub fn for_session() -> Self {
        #[cfg(windows)]
        {
            Self::WindowsDpapi
        }
        #[cfg(not(windows))]
        {
            if secret_service_available() {
                Self::SecretService
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
    /// Attribute pairs identifying the Secret Service item.
    pub secret_service_attributes: &'static [(&'static str, &'static str)],
    /// Optional entropy mixed into DPAPI protection of the key.
    pub dpapi_entropy: &'static [u8],
}

impl VaultFormat {
    /// Seal `plaintext` under `key`. `None` if the CSPRNG or the cipher fails.
    pub fn seal(&self, key: &[u8; KEY_LEN], plaintext: &[u8]) -> Option<Vec<u8>> {
        use chacha20poly1305::aead::Aead;
        use chacha20poly1305::{Key, KeyInit, XChaCha20Poly1305, XNonce};

        let mut nonce_bytes = [0u8; NONCE_LEN];
        OsRng.try_fill_bytes(&mut nonce_bytes).ok()?;
        let cipher = XChaCha20Poly1305::new(Key::from_slice(key));
        let ciphertext = cipher
            .encrypt(XNonce::from_slice(&nonce_bytes), plaintext)
            .ok()?;

        let mut out = Vec::with_capacity(
            self.magic
                .len()
                .saturating_add(1)
                .saturating_add(NONCE_LEN)
                .saturating_add(ciphertext.len()),
        );
        out.extend_from_slice(self.magic);
        out.push(SEAL_FORMAT);
        out.extend_from_slice(&nonce_bytes);
        out.extend_from_slice(&ciphertext);
        Some(out)
    }

    /// Open a blob produced by [`Self::seal`], returning the zeroized
    /// plaintext. `None` on any framing mismatch or authentication failure —
    /// a tampered or truncated file never yields bytes.
    pub fn open(&self, key: &[u8; KEY_LEN], blob: &[u8]) -> Option<Zeroizing<Vec<u8>>> {
        use chacha20poly1305::aead::Aead;
        use chacha20poly1305::{Key, KeyInit, XChaCha20Poly1305, XNonce};

        let rest = blob.strip_prefix(self.magic.as_slice())?;
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

    /// Load the key for the vault in `dir`, creating one on first use.
    ///
    /// `None` when the key can neither be read nor safely created: an
    /// existing key that cannot be read or is malformed is never replaced,
    /// and no key is minted while `vault.enc` exists without one.
    pub fn load_or_create_key(&self, dir: &Path, custody: Custody) -> Option<VaultKey> {
        match custody {
            Custody::SecretService => self.secret_service_key(dir),
            Custody::LocalFile => self.local_file_key(dir),
            #[cfg(windows)]
            Custody::WindowsDpapi => match windows::load_or_create_key(self, dir) {
                Ok(key) => Some(key),
                Err(error) => {
                    tracing::warn!(vault = self.name, %error, "Windows protected key unavailable; refusing fallback");
                    None
                }
            },
        }
    }

    fn secret_service_key(&self, dir: &Path) -> Option<VaultKey> {
        match self.lookup_secret_service_key() {
            Ok(Some(key)) => return Some(key),
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(vault = self.name, %error, "Secret Service lookup failed; refusing local fallback");
                return None;
            }
        }

        let local_path = dir.join(KEY_FILE);
        let (key, migrated) = match read_key_bytes(&local_path) {
            Ok(bytes) if bytes.len() == KEY_LEN => {
                let mut key = Zeroizing::new([0u8; KEY_LEN]);
                key.copy_from_slice(&bytes);
                (key, true)
            }
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
                let mut key = Zeroizing::new([0u8; KEY_LEN]);
                OsRng.try_fill_bytes(key.as_mut_slice()).ok()?;
                (key, false)
            }
            Err(error) => {
                tracing::warn!(vault = self.name, %error, "local vault key could not be read for migration");
                return None;
            }
        };

        if let Err(error) = self.store_secret_service_key(&key) {
            tracing::warn!(vault = self.name, %error, "Secret Service key creation failed");
            return None;
        }
        match self.lookup_secret_service_key() {
            Ok(Some(stored)) if stored.as_ref() == key.as_ref() => {
                if migrated && let Err(error) = std::fs::remove_file(&local_path) {
                    tracing::warn!(vault = self.name, %error, "migrated vault key but could not remove local copy");
                }
                Some(key)
            }
            _ => {
                tracing::warn!(
                    vault = self.name,
                    "Secret Service did not return the key after storing it"
                );
                None
            }
        }
    }

    fn secret_service_args(&self, command: &'static str, label: Option<&str>) -> Vec<String> {
        let mut args = vec![command.to_string()];
        if let Some(label) = label {
            args.push(format!("--label={label}"));
        }
        for (attribute, value) in self.secret_service_attributes {
            args.push((*attribute).to_string());
            args.push((*value).to_string());
        }
        args
    }

    fn lookup_secret_service_key(&self) -> Result<Option<VaultKey>, String> {
        let output = bounded_command::run_os(
            "secret-tool",
            &self.secret_service_args("lookup", None),
            None,
            256,
            Duration::from_secs(5),
        )
        .map_err(|error| format!("Secret Service lookup failed: {error}"))?;
        if !output.status.success() {
            return if output.stderr.is_empty() {
                Ok(None)
            } else {
                Err("Secret Service refused or could not retrieve the vault key".to_string())
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

    fn store_secret_service_key(&self, key: &[u8; KEY_LEN]) -> Result<(), String> {
        let encoded = Zeroizing::new(encode_key(key));
        let output = bounded_command::run_os(
            "secret-tool",
            &self.secret_service_args("store", Some(self.secret_service_label)),
            Some(encoded.as_bytes()),
            256,
            Duration::from_secs(5),
        )
        .map_err(|error| format!("Secret Service store failed: {error}"))?;
        if output.status.success() {
            Ok(())
        } else {
            Err("Secret Service refused or could not store the vault key".to_string())
        }
    }

    fn local_file_key(&self, dir: &Path) -> Option<VaultKey> {
        let path = dir.join(KEY_FILE);
        match read_key_bytes(&path) {
            Ok(bytes) if bytes.len() == KEY_LEN => {
                let mut key = Zeroizing::new([0u8; KEY_LEN]);
                key.copy_from_slice(&bytes);
                Some(key)
            }
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
                let mut key = Zeroizing::new([0u8; KEY_LEN]);
                if OsRng.try_fill_bytes(key.as_mut_slice()).is_err() {
                    return None;
                }
                if let Err(error) = std::fs::create_dir_all(dir) {
                    tracing::warn!(vault = self.name, %error, "could not create vault dir for key");
                    return None;
                }
                if private_fs::create_atomic(&path, key.as_slice()).is_err() {
                    tracing::warn!(vault = self.name, "vault key write failed");
                    return None;
                }
                // Another process may have won the exclusive publication. Always
                // reload its key instead of encrypting with our discarded candidate.
                let bytes = read_key_bytes(&path).ok()?;
                if bytes.len() != KEY_LEN {
                    return None;
                }
                key.copy_from_slice(&bytes);
                Some(key)
            }
            Err(error) => {
                tracing::warn!(vault = self.name, %error, "vault key cannot be read; refusing replacement");
                None
            }
        }
    }
}

/// Whether `secret-tool` is installed and answers.
#[cfg(not(windows))]
pub fn secret_service_available() -> bool {
    bounded_command::run(
        "secret-tool",
        &["--version"],
        None,
        256,
        Duration::from_secs(2),
    )
    .is_ok_and(|output| output.status.success())
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
    use std::fmt::Write as _;
    let mut encoded = String::with_capacity(KEY_LEN * 2);
    for byte in key {
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

fn decode_key(encoded: &str) -> Result<VaultKey, String> {
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

#[cfg(test)]
mod tests;
