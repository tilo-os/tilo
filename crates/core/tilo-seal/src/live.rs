//! Live frames: one stream's frames sealed under a key every session of its
//! sender derives alike, so no counter can be the nonce.

use chacha20poly1305::aead::{Aead as _, KeyInit as _, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};

use crate::error::{Error, Result};
use crate::keys::{SymmetricKey, fill_random};

/// Bytes of the random nonce a sealed frame starts with.
pub const NONCE_LEN: usize = 24;
/// Bytes sealing adds to a body: the nonce and the authentication tag.
pub const OVERHEAD: usize = NONCE_LEN + 16;

pub struct FrameCipher(Box<XChaCha20Poly1305>);

impl std::fmt::Debug for FrameCipher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FrameCipher(xchacha20_poly1305, <redacted>)")
    }
}

impl FrameCipher {
    /// The cipher under `key`.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidKey`] if the primitive refuses the key.
    pub fn new(key: &SymmetricKey) -> Result<Self> {
        XChaCha20Poly1305::new_from_slice(key.expose_secret())
            .map(|cipher| Self(Box::new(cipher)))
            .map_err(|_| Error::InvalidKey)
    }

    /// `body` sealed with `aad` under a fresh random nonce, which leads the
    /// result.
    ///
    /// # Errors
    ///
    /// [`Error::Randomness`] when no nonce can be drawn, [`Error::Seal`] if
    /// the primitive refuses.
    pub fn seal(&self, aad: &[u8], body: &[u8]) -> Result<Vec<u8>> {
        let mut nonce = [0_u8; NONCE_LEN];
        fill_random(&mut nonce)?;
        let sealed = self
            .0
            .encrypt(<&XNonce>::from(&nonce), Payload { msg: body, aad })
            .map_err(|_| Error::Seal)?;
        let mut frame = Vec::with_capacity(NONCE_LEN.saturating_add(sealed.len()));
        frame.extend_from_slice(&nonce);
        frame.extend_from_slice(&sealed);
        Ok(frame)
    }

    /// What [`Self::seal`] sealed with `aad`.
    ///
    /// # Errors
    ///
    /// [`Error::Malformed`] for bytes shorter than a nonce and tag,
    /// [`Error::Authentication`] for another key or associated data, or
    /// modified bytes.
    pub fn open(&self, aad: &[u8], sealed: &[u8]) -> Result<Vec<u8>> {
        let (nonce, sealed) = sealed
            .split_first_chunk::<NONCE_LEN>()
            .filter(|(_, rest)| rest.len() >= OVERHEAD - NONCE_LEN)
            .ok_or(Error::Malformed("shorter than a sealed live frame"))?;
        self.0
            .decrypt(<&XNonce>::from(nonce), Payload { msg: sealed, aad })
            .map_err(|_| Error::Authentication)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_open_under_their_key_and_aad_with_fresh_nonces() {
        let key = SymmetricKey::from_bytes([9; 32]);
        let cipher = FrameCipher::new(&key).unwrap();
        let one = cipher.seal(b"aad", b"body").unwrap();
        let two = cipher.seal(b"aad", b"body").unwrap();
        assert_eq!(one.len(), b"body".len() + OVERHEAD);
        assert_ne!(one[..NONCE_LEN], two[..NONCE_LEN]);
        assert_eq!(cipher.open(b"aad", &one).unwrap(), b"body");
        assert_eq!(cipher.open(b"other", &one), Err(Error::Authentication));
        let other = FrameCipher::new(&SymmetricKey::from_bytes([8; 32])).unwrap();
        assert_eq!(other.open(b"aad", &one), Err(Error::Authentication));
        let mut changed = one.clone();
        changed[0] ^= 1;
        assert_eq!(cipher.open(b"aad", &changed), Err(Error::Authentication));
        assert!(matches!(
            cipher.open(b"aad", &one[..OVERHEAD - 1]),
            Err(Error::Malformed(_))
        ));
    }
}
