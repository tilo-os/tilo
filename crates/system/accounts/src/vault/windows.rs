//! User-scoped DPAPI custody. No plaintext fallback or silent re-keying.
use std::io::{self, Read};
use std::path::Path;

use rand_core::{OsRng, RngCore as _};
use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
};
use zeroize::{Zeroize, Zeroizing};

use super::KEY_LEN;

const ENTROPY: &[u8] = b"Tilo Accounts vault key v1";
const MAX_KEY_BLOB: u64 = 65_536;

struct DpapiOutput(CRYPT_INTEGER_BLOB);

impl Drop for DpapiOutput {
    fn drop(&mut self) {
        if !self.0.pbData.is_null() {
            // SAFETY: DPAPI allocated this buffer and reported its length.
            // Zero plaintext before releasing the OS allocation.
            unsafe {
                std::slice::from_raw_parts_mut(self.0.pbData, self.0.cbData as usize).zeroize();
                LocalFree(self.0.pbData.cast());
            }
        }
    }
}

fn blob(bytes: &[u8]) -> io::Result<CRYPT_INTEGER_BLOB> {
    Ok(CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(bytes.len()).map_err(|_| io::Error::other("key blob too large"))?,
        pbData: bytes.as_ptr().cast_mut(),
    })
}

fn transform(bytes: &[u8], protect: bool) -> io::Result<Zeroizing<Vec<u8>>> {
    let input = blob(bytes)?;
    let entropy = blob(ENTROPY)?;
    let mut output = DpapiOutput(CRYPT_INTEGER_BLOB::default());
    // SAFETY: all input buffers remain live, are read-only by the API contract,
    // and output is an initialized descriptor owned by the RAII guard. Flags
    // forbid prompts and deliberately exclude CRYPTPROTECT_LOCAL_MACHINE.
    let succeeded = unsafe {
        if protect {
            CryptProtectData(
                &raw const input,
                std::ptr::null(),
                &raw const entropy,
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &raw mut output.0,
            )
        } else {
            CryptUnprotectData(
                &raw const input,
                std::ptr::null_mut(),
                &raw const entropy,
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &raw mut output.0,
            )
        }
    };
    if succeeded == 0 {
        return Err(io::Error::last_os_error());
    }
    if output.0.pbData.is_null() || output.0.cbData == 0 {
        return Err(io::Error::other("DPAPI returned an empty key blob"));
    }
    // SAFETY: successful DPAPI output is retained until this copy completes.
    Ok(Zeroizing::new(unsafe {
        std::slice::from_raw_parts(output.0.pbData, output.0.cbData as usize).to_vec()
    }))
}

fn read_private(path: &Path) -> io::Result<Zeroizing<Vec<u8>>> {
    let mut bytes = Zeroizing::new(Vec::new());
    private_fs::read(path)?
        .take(MAX_KEY_BLOB + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_KEY_BLOB {
        return Err(io::Error::other("stored key blob too large"));
    }
    Ok(bytes)
}

fn decode_key(bytes: &[u8]) -> io::Result<Zeroizing<[u8; KEY_LEN]>> {
    let bytes = transform(bytes, false)?;
    let mut key = Zeroizing::new([0u8; KEY_LEN]);
    if bytes.len() != KEY_LEN {
        return Err(io::Error::other("DPAPI key has the wrong length"));
    }
    key.copy_from_slice(&bytes);
    Ok(key)
}

pub(super) fn load_or_create_key(dir: &Path) -> io::Result<Zeroizing<[u8; KEY_LEN]>> {
    private_fs::directory(dir)?;
    let lock = private_fs::lock_file(&dir.join("vault-key.lock"))?;
    lock.lock()?;
    let protected_path = dir.join("vault.key.dpapi");
    match read_private(&protected_path) {
        Ok(bytes) => {
            let key = decode_key(&bytes)?;
            finish_legacy_migration(dir, &key)?;
            return Ok(key);
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    let legacy_path = dir.join("vault.key");
    let mut key = Zeroizing::new([0u8; KEY_LEN]);
    let migrating = match read_private(&legacy_path) {
        Ok(bytes) => {
            if bytes.len() != KEY_LEN {
                return Err(io::Error::other("legacy vault key has the wrong length"));
            }
            key.copy_from_slice(&bytes);
            true
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            // An existing ciphertext without its key is a recovery problem,
            // never permission to mint a replacement and overwrite it.
            if dir.join("vault.enc").try_exists()? {
                return Err(io::Error::other("existing vault key is missing"));
            }
            OsRng
                .try_fill_bytes(key.as_mut_slice())
                .map_err(|error| io::Error::other(error.to_string()))?;
            false
        }
        Err(error) => return Err(error),
    };
    if migrating && dir.join("vault.enc").try_exists()? {
        let encrypted = std::fs::read(dir.join("vault.enc"))?;
        if super::open(&key, &encrypted).is_none() {
            return Err(io::Error::other(
                "legacy key does not unlock the existing vault",
            ));
        }
    }
    let protected = transform(key.as_slice(), true)?;
    private_fs::create_atomic(&protected_path, &protected)?;
    let restored = decode_key(&read_private(&protected_path)?)?;
    if restored.as_slice() != key.as_slice() {
        return Err(io::Error::other("published key differs from the original"));
    }
    if migrating {
        finish_legacy_migration(dir, &restored)?;
    }
    Ok(restored)
}

/// Publication and plaintext removal are separate durable steps. Reopening
/// must finish an interrupted migration, but never discard conflicting material
/// that may be needed for recovery. The caller holds the vault-key lock.
fn finish_legacy_migration(dir: &Path, key: &[u8; KEY_LEN]) -> io::Result<()> {
    let legacy_path = dir.join("vault.key");
    let legacy = match read_private(&legacy_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if legacy.as_slice() != key {
        return Err(io::Error::other(
            "legacy vault key differs from the protected key",
        ));
    }
    match std::fs::read(dir.join("vault.enc")) {
        Ok(encrypted) if super::open(key, &encrypted).is_none() => {
            return Err(io::Error::other(
                "protected key does not unlock the existing vault",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    std::fs::remove_file(legacy_path)?;
    private_fs::sync_directory(dir)
}
