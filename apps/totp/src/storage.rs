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
use std::time::Duration;

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::model::{Account, Vault};

#[derive(Debug, Serialize, Deserialize, Default)]
struct VaultFile {
    version: u32,
    accounts: Vec<Account>,
}

const VAULT_VERSION: u32 = 1;

/// Magic + format byte prefixing every sealed vault, so a stray file or a
/// format bump is detected before we hand bytes to the AEAD.
const SEAL_MAGIC: &[u8; 4] = b"QOAV";
const SEAL_FORMAT: u8 = 1;

/// XChaCha20-Poly1305 nonce width (192-bit / 24-byte extended nonce).
const NONCE_LEN: usize = 24;
/// Symmetric key width (256-bit).
const KEY_LEN: usize = 32;

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KeyCustody {
    SecretService,
    LocalFile,
}

/// Resolve the data directory: `$XDG_DATA_HOME/tilo-authenticator`
/// (falling back to `$HOME/.local/share/...`). Returns `None` when no
/// home directory is discoverable.
pub fn data_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
    Some(base.join("tilo-authenticator"))
}

/// Path of the encrypted vault (`.../tilo-authenticator/vault.enc`). This is
/// the real vault file — its presence is what "is there a saved vault?"
/// checks against.
pub fn vault_path() -> Option<PathBuf> {
    data_dir().map(|d| d.join("vault.enc"))
}

/// Path of the symmetric key file.
fn key_path_in(dir: &Path) -> PathBuf {
    dir.join("vault.key")
}

/// Path of the legacy plaintext vault, migrated + removed on first load.
fn legacy_plaintext_path_in(dir: &Path) -> PathBuf {
    dir.join("vault.json")
}

/// Load the vault from disk. Returns an empty vault when nothing is stored
/// (or when it can't be read/decrypted) — never an error to the caller.
pub fn load() -> Vault {
    match data_dir() {
        Some(dir) => load_at_with_custody(&dir, key_custody()),
        None => Vault::new(),
    }
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

fn load_at_with_custody(dir: &Path, custody: KeyCustody) -> Vault {
    let enc = dir.join("vault.enc");
    if enc.exists() {
        return match (load_or_create_key(dir, custody), std::fs::read(&enc)) {
            (Some(key), Ok(blob)) => {
                if let Some(plaintext) = open(&key, &blob) {
                    parse_vault(&plaintext)
                } else {
                    tracing::warn!("authenticator: vault decrypt failed; starting empty");
                    Vault::new()
                }
            }
            _ => Vault::new(),
        };
    }

    // Transparent upgrade: an older build left a plaintext vault. Parse it,
    // re-seal it, and remove the cleartext original.
    let legacy = legacy_plaintext_path_in(dir);
    if let Ok(text) = std::fs::read_to_string(&legacy)
        && let Ok(file) = serde_json::from_str::<VaultFile>(&text)
    {
        let mut v = Vault::new();
        v.accounts = file.accounts;
        v.renumber();
        save_at_with_custody(dir, &v, custody);
        if enc.exists()
            && let Err(e) = std::fs::remove_file(&legacy)
        {
            tracing::warn!(error = %e, "authenticator: could not remove migrated plaintext vault");
        }
        return v;
    }

    Vault::new()
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

fn parse_vault(plaintext: &[u8]) -> Vault {
    match serde_json::from_slice::<VaultFile>(plaintext) {
        Ok(file) => {
            let mut v = Vault::new();
            v.accounts = file.accounts;
            v.renumber();
            v
        }
        Err(e) => {
            tracing::warn!(error = %e, "authenticator: decrypted vault parse failed; starting empty");
            Vault::new()
        }
    }
}

/// Read the vault key from `dir`, generating + persisting a fresh random
/// one the first time. Returns `None` only when the key can neither be read
/// nor created (a read-only or malformed data dir), in which case the
/// caller degrades to in-memory-only.
///
/// This is the single point of key custody — see the module docs for the
/// keyring seam that will replace the local key file.
fn load_or_create_key(dir: &Path, custody: KeyCustody) -> Option<Zeroizing<[u8; KEY_LEN]>> {
    match custody {
        KeyCustody::SecretService => secret_service_key(dir),
        KeyCustody::LocalFile => local_file_key(dir),
    }
}

fn key_custody() -> KeyCustody {
    let available = bounded_command::run(
        "secret-tool",
        &["--version"],
        None,
        256,
        Duration::from_secs(2),
    )
    .is_ok_and(|output| output.status.success());
    if available {
        KeyCustody::SecretService
    } else {
        KeyCustody::LocalFile
    }
}

fn secret_service_key(dir: &Path) -> Option<Zeroizing<[u8; KEY_LEN]>> {
    match lookup_secret_service_key() {
        Ok(Some(key)) => return Some(key),
        Ok(None) => {}
        Err(error) => {
            tracing::warn!(%error, "authenticator: Secret Service lookup failed; refusing local fallback");
            return None;
        }
    }

    let local_path = key_path_in(dir);
    let (key, migrated) = match std::fs::read(&local_path) {
        Ok(bytes) if bytes.len() == KEY_LEN => {
            let mut key = Zeroizing::new([0u8; KEY_LEN]);
            key.copy_from_slice(&bytes);
            (key, true)
        }
        Ok(_) => {
            tracing::warn!("authenticator: local vault key is malformed; not replacing it");
            return None;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut key = Zeroizing::new([0u8; KEY_LEN]);
            getrandom::getrandom(key.as_mut_slice()).ok()?;
            (key, false)
        }
        Err(error) => {
            tracing::warn!(%error, "authenticator: local vault key could not be read for migration");
            return None;
        }
    };

    if let Err(error) = store_secret_service_key(&key) {
        tracing::warn!(%error, "authenticator: Secret Service key creation failed");
        return None;
    }
    match lookup_secret_service_key() {
        Ok(Some(stored)) if stored.as_ref() == key.as_ref() => {
            if migrated && let Err(error) = std::fs::remove_file(&local_path) {
                tracing::warn!(%error, "authenticator: migrated vault key but could not remove local copy");
            }
            Some(key)
        }
        _ => {
            tracing::warn!("authenticator: Secret Service did not return the stored key");
            None
        }
    }
}

fn lookup_secret_service_key() -> Result<Option<Zeroizing<[u8; KEY_LEN]>>, String> {
    let output = bounded_command::run(
        "secret-tool",
        &["lookup", "application", "Tilo", "purpose", "totp-vault"],
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

fn store_secret_service_key(key: &[u8; KEY_LEN]) -> Result<(), String> {
    let encoded = Zeroizing::new(encode_key(key));
    let output = bounded_command::run(
        "secret-tool",
        &[
            "store",
            "--label=Tilo authenticator vault",
            "application",
            "Tilo",
            "purpose",
            "totp-vault",
        ],
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
        let &[hi, lo] = pair else {
            return Err("Secret Service key is not hexadecimal".into());
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
    let path = key_path_in(dir);
    match std::fs::read(&path) {
        Ok(bytes) if bytes.len() == KEY_LEN => {
            let mut key = Zeroizing::new([0u8; KEY_LEN]);
            key.copy_from_slice(&bytes);
            Some(key)
        }
        // A file exists but isn't a well-formed key: refuse to clobber it,
        // since regenerating would strand any vault it already sealed.
        Ok(_) => {
            tracing::warn!("authenticator: vault key file is malformed; not overwriting");
            None
        }
        Err(_) => {
            let mut key = Zeroizing::new([0u8; KEY_LEN]);
            if getrandom::getrandom(key.as_mut_slice()).is_err() {
                return None;
            }
            if let Err(e) = std::fs::create_dir_all(dir) {
                tracing::warn!(error = %e, "authenticator: could not create data dir for key");
                return None;
            }
            if write_key_file(&path, &key).is_err() {
                tracing::warn!("authenticator: vault key write failed");
                return None;
            }
            Some(key)
        }
    }
}

/// Seal `plaintext` under `key`: `MAGIC || format || nonce(24) || AEAD`.
/// Returns `None` if the CSPRNG or the cipher fails.
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

/// Open a blob produced by [`seal`], returning the plaintext (zeroized on
/// drop). `None` on any framing mismatch or authentication failure — a
/// tampered or truncated file never yields bytes.
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

/// Write the key file atomically, restricted to `0600` before it is ever
/// visible at its final path.
fn write_key_file(path: &Path, key: &[u8; KEY_LEN]) -> std::io::Result<()> {
    atomic_file::write_with_mode(path, key, 0o600)
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

    fn temp_dir() -> PathBuf {
        std::env::temp_dir().join(format!("tilo-totp-store-{}", uuid::Uuid::new_v4()))
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
    fn secret_service_key_encoding_round_trips_and_rejects_bad_material() {
        let key = [0xa5; KEY_LEN];
        assert_eq!(decode_key(&encode_key(&key)).unwrap().as_ref(), &key);
        assert!(decode_key("short").is_err());
        assert!(decode_key(&"z".repeat(KEY_LEN * 2)).is_err());
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
