//! Linux upstream OSCrypt v10/v11 only; never guesses a vendor key or drops host binding.
//!
//! Format and KDF:
//! <https://github.com/chromium/chromium/blob/134.0.6998.35/components/os_crypt/sync/os_crypt_linux.cc>
//! Brand lookup: key_storage_linux.cc and key_storage_libsecret.cc at the same tag.
//! Schema 24 host binding:
//! <https://chromium.googlesource.com/chromium/src/+/main/net/extras/sqlite/sqlite_persistent_cookie_store.cc>
use super::{ChromiumDecryptor, SourceError, SourceGrant};
use aes::{
    Aes128,
    cipher::{Array, BlockCipherDecrypt, KeyInit},
};
use sha2::{Digest, Sha256};
use std::{fs::File, os::unix::fs::MetadataExt, sync::Mutex};
pub use tilo_secrets::owner_migration::ChromiumBrand;
use tilo_secrets::owner_migration::ChromiumKey;
use zeroize::{Zeroize, Zeroizing};

const MAX_ENCRYPTED_COOKIE: usize = 32 * 1024;

/// Private native capability for one explicitly selected profile and upstream brand.
/// Unknown vendor/password stores and prefixes are refused, not reinterpreted.
pub struct ChromiumLinux {
    profile: File,
    identity: (u32, u64, u64),
    brand: ChromiumBrand,
    v11: Mutex<Option<Zeroizing<[u8; 16]>>>,
}
impl ChromiumLinux {
    /// The brand must come from the user's recognized source selection. No keyring
    /// is contacted until an actual selected v11 cookie needs its existing password.
    ///
    /// # Errors
    /// Refuses non-Chromium, foreign/elevated users, and unsafe profile custody.
    pub fn for_grant(grant: &SourceGrant, brand: ChromiumBrand) -> Result<Self, SourceError> {
        if brand == ChromiumBrand::Slack452178 {
            return Err(SourceError::UnsupportedEncryption);
        }
        Self::for_application(grant, brand)
    }
    pub(super) fn for_application(
        grant: &SourceGrant,
        brand: ChromiumBrand,
    ) -> Result<Self, SourceError> {
        let facts = grant.profile.metadata().map_err(super::unavailable)?;
        let owner = rustix::process::getuid().as_raw();
        if grant.provider != browser_continuity::Provider::Chromium
            || owner == 0
            || facts.uid() != owner
            || rustix::process::geteuid().as_raw() != owner
        {
            return Err(SourceError::Unavailable);
        }
        // Reuses the narrow profile capability's original-owner admission.
        ChromiumKey::new(&grant.profile, brand).map_err(|_| SourceError::Unavailable)?;
        Ok(Self {
            profile: grant.profile.try_clone().map_err(super::unavailable)?,
            identity: (owner, facts.dev(), facts.ino()),
            brand,
            v11: Mutex::new(None),
        })
    }
    fn existing_v11(&self) -> Result<Zeroizing<[u8; 16]>, SourceError> {
        let mut cached = self.v11.lock().map_err(|_| SourceError::Unavailable)?;
        if cached.is_none() {
            let capability = ChromiumKey::new(&self.profile, self.brand)
                .map_err(|_| SourceError::UnsupportedEncryption)?;
            let password = capability
                .read_blocking()
                .map_err(|_| SourceError::UnsupportedEncryption)?
                .ok_or(SourceError::UnsupportedEncryption)?;
            *cached = Some(derive(password.expose()));
        }
        cached
            .as_ref()
            .map(|key| Zeroizing::new(**key))
            .ok_or(SourceError::UnsupportedEncryption)
    }
}
impl ChromiumDecryptor for ChromiumLinux {
    fn approved_profile(&self) -> (u32, u64, u64) {
        self.identity
    }
    fn decrypt(
        &self,
        host_key: &str,
        schema: i64,
        encrypted: &[u8],
    ) -> Result<Zeroizing<String>, SourceError> {
        let facts = self.profile.metadata().map_err(super::unavailable)?;
        if (facts.uid(), facts.dev(), facts.ino()) != self.identity
            || facts.mode() & 0o022 != 0
            || rustix::process::getuid().as_raw() != self.identity.0
            || rustix::process::geteuid().as_raw() != self.identity.0
        {
            return Err(SourceError::Unavailable);
        }
        let (prefix, ciphertext) = encrypted
            .split_at_checked(3)
            .ok_or(SourceError::UnsupportedEncryption)?;
        if !matches!(schema, 23 | 24) {
            return Err(SourceError::UnsupportedSchema);
        }
        if ciphertext.is_empty()
            || ciphertext.len() > MAX_ENCRYPTED_COOKIE
            || !ciphertext.len().is_multiple_of(16)
        {
            return Err(SourceError::UnsupportedEncryption);
        }
        let key = match prefix {
            b"v10" => derive(b"peanuts"),
            b"v11" => self.existing_v11()?,
            _ => return Err(SourceError::UnsupportedEncryption),
        };
        decode(&key, ciphertext, host_key, schema)
    }
}
fn derive(password: &[u8]) -> Zeroizing<[u8; 16]> {
    let mut key = Zeroizing::new([0; 16]);
    // Compatibility KDF defined by Linux OSCrypt, not a new password-storage policy.
    pbkdf2::pbkdf2_hmac::<sha1::Sha1>(password, b"saltysalt", 1, key.as_mut_slice());
    key
}
fn decode(
    key: &[u8; 16],
    ciphertext: &[u8],
    host_key: &str,
    schema: i64,
) -> Result<Zeroizing<String>, SourceError> {
    if !matches!(schema, 23 | 24) {
        return Err(SourceError::UnsupportedSchema);
    }
    if ciphertext.is_empty()
        || ciphertext.len() > MAX_ENCRYPTED_COOKIE
        || !ciphertext.len().is_multiple_of(16)
    {
        return Err(SourceError::UnsupportedEncryption);
    }
    let cipher = Aes128::new_from_slice(key).map_err(|_| SourceError::UnsupportedEncryption)?;
    let mut previous = [b' '; 16];
    let mut plaintext = Zeroizing::new(Vec::with_capacity(ciphertext.len()));
    for bytes in ciphertext.chunks_exact(16) {
        let original: [u8; 16] = bytes
            .try_into()
            .map_err(|_| SourceError::UnsupportedEncryption)?;
        let mut block = Array::from(original);
        cipher.decrypt_block(&mut block);
        for (byte, previous) in block.iter_mut().zip(previous.iter()) {
            *byte ^= previous
        }
        plaintext.extend_from_slice(&block);
        block.as_mut_slice().zeroize();
        previous = original;
    }
    let padding = usize::from(*plaintext.last().ok_or(SourceError::UnsupportedEncryption)?);
    if !(1..=16).contains(&padding) {
        return Err(SourceError::UnsupportedEncryption);
    }
    let length = plaintext
        .len()
        .checked_sub(padding)
        .ok_or(SourceError::UnsupportedEncryption)?;
    if !plaintext
        .get(length..)
        .ok_or(SourceError::UnsupportedEncryption)?
        .iter()
        .all(|byte| usize::from(*byte) == padding)
    {
        return Err(SourceError::UnsupportedEncryption);
    }
    let unpadded = plaintext
        .get(..length)
        .ok_or(SourceError::UnsupportedEncryption)?;
    let value = if schema == 24 {
        let (hash, value) = unpadded
            .split_at_checked(32)
            .ok_or(SourceError::UnsupportedEncryption)?;
        if hash != Sha256::digest(host_key.as_bytes()).as_slice() {
            return Err(SourceError::UnsupportedEncryption);
        }
        value
    } else {
        unpadded
    };
    let text = std::str::from_utf8(value).map_err(|_| SourceError::InvalidCookies)?;
    Ok(Zeroizing::new(text.to_owned()))
}

#[cfg(test)]
mod tests;
