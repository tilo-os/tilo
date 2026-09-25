//! Readers for formats Tilo sealed before this crate existed.
//!
//! Each Tilo format written before consolidation combined the same few
//! primitives in its own way: a raw X25519 agreement hashed with BLAKE3 into
//! a key, then ChaCha20-Poly1305 (12-byte nonce) or XChaCha20-Poly1305
//! (24-byte nonce) with format-specific associated data. The functions here
//! expose exactly those primitives so each format's reader can reproduce its
//! old layout byte for byte, while the raw AEAD and KEM crates stay a
//! dependency of this crate alone.
//!
//! Nothing new may be *stored* with these. The one sealing function,
//! [`seal_chacha20poly1305_for_transition`], exists only so a service can
//! answer a peer that still speaks the previous enrollment wire version.

use chacha20poly1305::aead::{Aead as _, KeyInit as _, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Nonce, XChaCha20Poly1305, XNonce};
use zeroize::Zeroizing;

use crate::error::{Error, Result};
use crate::keys::{KEY_LEN, PublicKey, SecretKey, SymmetricKey, fill_random};

/// Length of a ChaCha20-Poly1305 nonce.
pub const CHACHA_NONCE_LEN: usize = 12;
/// Length of an XChaCha20-Poly1305 nonce.
pub const XCHACHA_NONCE_LEN: usize = 24;

/// The key derivation every pre-consolidation X25519 format used:
/// `BLAKE3(domain || X25519(secret, peer) || context[0] || context[1] ...)`.
///
/// # Errors
///
/// [`Error::InvalidKey`] when the agreement yields the all-zero value
/// (a low-order peer key), which no honest sender ever produced.
pub fn blake3_x25519_key(
    secret: &SecretKey,
    peer: &PublicKey,
    domain: &[u8],
    context: &[&[u8]],
) -> Result<SymmetricKey> {
    let shared = secret.diffie_hellman(peer)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(domain);
    hasher.update(shared.as_ref());
    for part in context {
        hasher.update(part);
    }
    let digest = hasher.finalize();
    let mut key = Zeroizing::new([0_u8; KEY_LEN]);
    key.copy_from_slice(digest.as_bytes());
    Ok(SymmetricKey::from_bytes(*key))
}

/// Open a ChaCha20-Poly1305 ciphertext (12-byte nonce) from an old format.
///
/// # Errors
///
/// [`Error::Authentication`] when the key, nonce, AAD or bytes differ.
pub fn open_chacha20poly1305(
    key: &SymmetricKey,
    nonce: &[u8; CHACHA_NONCE_LEN],
    aad: &[u8],
    ciphertext: &[u8],
) -> Result<Zeroizing<Vec<u8>>> {
    ChaCha20Poly1305::new_from_slice(key.expose_secret())
        .map_err(|_| Error::InvalidKey)?
        .decrypt(
            <&Nonce>::from(nonce),
            Payload {
                msg: ciphertext,
                aad,
            },
        )
        .map(Zeroizing::new)
        .map_err(|_| Error::Authentication)
}

/// Open an XChaCha20-Poly1305 ciphertext (24-byte nonce) from an old format.
///
/// # Errors
///
/// [`Error::Authentication`] when the key, nonce, AAD or bytes differ.
pub fn open_xchacha20poly1305(
    key: &SymmetricKey,
    nonce: &[u8; XCHACHA_NONCE_LEN],
    aad: &[u8],
    ciphertext: &[u8],
) -> Result<Zeroizing<Vec<u8>>> {
    XChaCha20Poly1305::new_from_slice(key.expose_secret())
        .map_err(|_| Error::InvalidKey)?
        .decrypt(
            <&XNonce>::from(nonce),
            Payload {
                msg: ciphertext,
                aad,
            },
        )
        .map(Zeroizing::new)
        .map_err(|_| Error::Authentication)
}

/// Seal with ChaCha20-Poly1305 and a fresh random nonce, for answering a
/// peer that only speaks a previous wire version during a protocol
/// transition. Never use this for anything stored.
///
/// # Errors
///
/// [`Error::Randomness`] when no nonce can be drawn.
pub fn seal_chacha20poly1305_for_transition(
    key: &SymmetricKey,
    aad: &[u8],
    plaintext: &[u8],
) -> Result<([u8; CHACHA_NONCE_LEN], Vec<u8>)> {
    let mut nonce = [0_u8; CHACHA_NONCE_LEN];
    fill_random(&mut nonce)?;
    let ciphertext = ChaCha20Poly1305::new_from_slice(key.expose_secret())
        .map_err(|_| Error::InvalidKey)?
        .encrypt(
            <&Nonce>::from(&nonce),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| Error::Seal)?;
    Ok((nonce, ciphertext))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blake3_key_is_order_sensitive_and_symmetric_in_the_agreement() {
        let a = SecretKey::from_bytes([0x11; 32]);
        let b = SecretKey::from_bytes([0x22; 32]);
        let ka = blake3_x25519_key(&a, &b.public_key().unwrap(), b"d", &[b"x", b"y"]).unwrap();
        let kb = blake3_x25519_key(&b, &a.public_key().unwrap(), b"d", &[b"x", b"y"]).unwrap();
        assert_eq!(ka, kb);
        let swapped = blake3_x25519_key(&a, &b.public_key().unwrap(), b"d", &[b"y", b"x"]).unwrap();
        assert_ne!(ka, swapped);
    }

    #[test]
    fn transition_seal_opens_with_the_legacy_reader() {
        let key = SymmetricKey::from_bytes([9; 32]);
        let (nonce, ct) = seal_chacha20poly1305_for_transition(&key, b"aad", b"pt").unwrap();
        assert_eq!(
            open_chacha20poly1305(&key, &nonce, b"aad", &ct)
                .unwrap()
                .as_slice(),
            b"pt"
        );
        assert_eq!(
            open_chacha20poly1305(&key, &nonce, b"other", &ct),
            Err(Error::Authentication)
        );
    }
}
