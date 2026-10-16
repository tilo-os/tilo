//! The wrap a browser computes with `WebCrypto`: AES-256-GCM under a key
//! derived with HKDF-SHA256, so native code opens what a page sealed and the
//! page unwraps straight into a non-extractable `CryptoKey`.
//!
//! - key: `HKDF-SHA256(ikm, salt, info = label)`, 256 bits;
//! - nonce: 96 random bits, stored beside the ciphertext;
//! - associated data: the [`Context`] (label and record, each
//!   length-prefixed, as for [`crate::envelope`]) with no header.
//!
//! In `WebCrypto` terms: `deriveKey({name: 'HKDF', hash: 'SHA-256', salt,
//! info}, ikm, {name: 'AES-GCM', length: 256})`, then `wrapKey`/`unwrapKey`
//! or `encrypt`/`decrypt` with `{name: 'AES-GCM', iv: nonce,
//! additionalData}`.

use aes_gcm::aead::{Aead as _, KeyInit as _, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use zeroize::Zeroizing;

use crate::envelope::Context;
use crate::error::{Error, Result};
use crate::keys::{SymmetricKey, fill_random};

/// AES-GCM nonce length.
pub const NONCE_LEN: usize = 12;
const KEY_LEN: usize = 32;
/// Shortest accepted input keying material.
pub const MIN_IKM_LEN: usize = 16;

/// The wrapping key for `ikm` and `salt` under `context`'s label.
///
/// # Errors
///
/// [`Error::InvalidKey`] for keying material shorter than [`MIN_IKM_LEN`].
pub fn derive_key(ikm: &[u8], salt: &[u8], context: Context<'_>) -> Result<SymmetricKey> {
    if ikm.len() < MIN_IKM_LEN {
        return Err(Error::InvalidKey);
    }
    let mut out = Zeroizing::new([0_u8; KEY_LEN]);
    hkdf::Hkdf::<sha2::Sha256>::new(Some(salt), ikm)
        .expand(context.label().as_str().as_bytes(), out.as_mut())
        .map_err(|_| Error::InvalidKey)?;
    Ok(SymmetricKey::from_bytes(*out))
}

fn cipher(key: &SymmetricKey) -> Result<Aes256Gcm> {
    Aes256Gcm::new_from_slice(key.expose_secret()).map_err(|_| Error::InvalidKey)
}

/// Seal `plaintext` under `key` for `context` with a fresh nonce. Returns
/// the nonce and the ciphertext (with its 16-byte tag).
///
/// # Errors
///
/// [`Error::Randomness`] when no nonce can be drawn.
pub fn seal(
    key: &SymmetricKey,
    context: Context<'_>,
    plaintext: &[u8],
) -> Result<([u8; NONCE_LEN], Vec<u8>)> {
    let mut nonce = [0_u8; NONCE_LEN];
    fill_random(&mut nonce)?;
    let aad = context.aad(&[])?;
    let ciphertext = cipher(key)?
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad: &aad,
            },
        )
        .map_err(|_| Error::Seal)?;
    Ok((nonce, ciphertext))
}

/// Open what [`seal`] (or a page's `wrapKey`/`encrypt`) produced.
///
/// # Errors
///
/// [`Error::Malformed`] for a nonce of the wrong length,
/// [`Error::Authentication`] for another key, context, or modified bytes.
pub fn open(
    key: &SymmetricKey,
    context: Context<'_>,
    nonce: &[u8],
    ciphertext: &[u8],
) -> Result<Zeroizing<Vec<u8>>> {
    if nonce.len() != NONCE_LEN {
        return Err(Error::Malformed("nonce length"));
    }
    let aad = context.aad(&[])?;
    cipher(key)?
        .decrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: ciphertext,
                aad: &aad,
            },
        )
        .map(Zeroizing::new)
        .map_err(|_| Error::Authentication)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Label;

    const RECORD: &[u8] = b"record";

    fn context(record: &[u8]) -> Context<'_> {
        Context::new(Label::UserRootWrap, record)
    }

    #[test]
    fn round_trips_and_rejects_another_key_context_or_bytes() {
        let key = derive_key(&[7; 32], b"salt", context(RECORD)).unwrap();
        let (nonce, sealed) = seal(&key, context(RECORD), b"secret").unwrap();
        assert_eq!(
            open(&key, context(RECORD), &nonce, &sealed)
                .unwrap()
                .as_slice(),
            b"secret"
        );
        let other = derive_key(&[7; 32], b"pepper", context(RECORD)).unwrap();
        assert_eq!(
            open(&other, context(RECORD), &nonce, &sealed),
            Err(Error::Authentication)
        );
        assert_eq!(
            open(&key, context(b"elsewhere"), &nonce, &sealed),
            Err(Error::Authentication)
        );
        let mut flipped = sealed.clone();
        flipped[0] ^= 1;
        assert_eq!(
            open(&key, context(RECORD), &nonce, &flipped),
            Err(Error::Authentication)
        );
        assert_eq!(
            open(&key, context(RECORD), &nonce[..8], &sealed),
            Err(Error::Malformed("nonce length"))
        );
        assert_eq!(
            derive_key(&[7; 8], b"salt", context(RECORD)).err(),
            Some(Error::InvalidKey)
        );
    }

    /// `WebCrypto`'s HKDF-SHA256 → AES-256-GCM `encrypt` of `"tilo"` for
    /// these inputs (computed with Node's `crypto.subtle`).
    #[test]
    fn matches_webcrypto() {
        let key = derive_key(&[1; 32], &[2; 32], context(RECORD)).unwrap();
        let sealed = hex::decode(WEBCRYPTO_CIPHERTEXT).unwrap();
        assert_eq!(
            open(&key, context(RECORD), &[3; NONCE_LEN], &sealed)
                .unwrap()
                .as_slice(),
            b"tilo"
        );
    }

    const WEBCRYPTO_CIPHERTEXT: &str = "baaa88e89ac22253d6ef8e0ae783f3242fd0c3cd";
}
