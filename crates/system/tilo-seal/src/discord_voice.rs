//! Discord voice's transport encryption, as its voice gateway fixes it: the
//! `*_rtpsize` AEAD modes over RTP.

// The two ciphers come from different RustCrypto generations (this
// crate's AES-GCM is the one `web` shares), so each uses its own traits.
use aes_gcm::Aes256Gcm;
use aes_gcm::aead::{Aead as AesAead, KeyInit as AesKeyInit, Payload as AesPayload};
use chacha20poly1305::XChaCha20Poly1305;
use chacha20poly1305::aead::{Aead as _, KeyInit as _, Payload};

use crate::error::{Error, Result};

pub const AES_GCM: &str = "aead_aes256_gcm_rtpsize";
pub const XCHACHA: &str = "aead_xchacha20_poly1305_rtpsize";

/// The mode to select from the ones a voice server offers: AES-256-GCM when
/// offered, else XChaCha20-Poly1305, else none this module speaks.
#[must_use]
pub fn choose_mode<'a>(offered: impl IntoIterator<Item = &'a str>) -> Option<&'static str> {
    let mut best = None;
    for mode in offered {
        if mode == AES_GCM {
            return Some(AES_GCM);
        }
        if mode == XCHACHA {
            best = Some(XCHACHA);
        }
    }
    best
}

pub enum TransportCipher {
    Aes(Box<Aes256Gcm>),
    XChaCha(Box<XChaCha20Poly1305>),
}

impl std::fmt::Debug for TransportCipher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Aes(_) => "TransportCipher(aes256_gcm, <redacted>)",
            Self::XChaCha(_) => "TransportCipher(xchacha20_poly1305, <redacted>)",
        })
    }
}

impl TransportCipher {
    /// The cipher for `mode` under `key`.
    ///
    /// # Errors
    ///
    /// [`Error::UnsupportedAlgorithm`] for another mode,
    /// [`Error::InvalidKey`] for a key that is not 32 bytes.
    pub fn new(mode: &str, key: &[u8]) -> Result<Self> {
        match mode {
            AES_GCM => <Aes256Gcm as AesKeyInit>::new_from_slice(key)
                .map(|cipher| Self::Aes(Box::new(cipher)))
                .map_err(|_| Error::InvalidKey),
            XCHACHA => XChaCha20Poly1305::new_from_slice(key)
                .map(|cipher| Self::XChaCha(Box::new(cipher)))
                .map_err(|_| Error::InvalidKey),
            _ => Err(Error::UnsupportedAlgorithm(0)),
        }
    }

    /// `body` sealed under `counter` with `aad`, its 16-byte tag appended.
    ///
    /// # Errors
    ///
    /// [`Error::Seal`] if the primitive refuses.
    pub fn seal(&self, counter: u32, aad: &[u8], body: &[u8]) -> Result<Vec<u8>> {
        match self {
            Self::Aes(cipher) => AesAead::encrypt(
                cipher.as_ref(),
                &short_nonce(counter).into(),
                AesPayload { msg: body, aad },
            )
            .map_err(|_| Error::Seal),
            Self::XChaCha(cipher) => cipher
                .encrypt(&long_nonce(counter).into(), Payload { msg: body, aad })
                .map_err(|_| Error::Seal),
        }
    }

    /// What [`Self::seal`] sealed under `counter` with `aad`.
    ///
    /// # Errors
    ///
    /// [`Error::Authentication`] for another key, counter or associated
    /// data, or modified bytes.
    pub fn open(&self, counter: u32, aad: &[u8], sealed: &[u8]) -> Result<Vec<u8>> {
        match self {
            Self::Aes(cipher) => AesAead::decrypt(
                cipher.as_ref(),
                &short_nonce(counter).into(),
                AesPayload { msg: sealed, aad },
            )
            .map_err(|_| Error::Authentication),
            Self::XChaCha(cipher) => cipher
                .decrypt(&long_nonce(counter).into(), Payload { msg: sealed, aad })
                .map_err(|_| Error::Authentication),
        }
    }
}

fn short_nonce(counter: u32) -> [u8; 12] {
    let [a, b, c, d] = counter.to_be_bytes();
    [a, b, c, d, 0, 0, 0, 0, 0, 0, 0, 0]
}

fn long_nonce(counter: u32) -> [u8; 24] {
    let [a, b, c, d] = counter.to_be_bytes();
    let mut nonce = [0_u8; 24];
    for (slot, byte) in nonce.iter_mut().zip([a, b, c, d]) {
        *slot = byte;
    }
    nonce
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "in a test this is the assertion")]

    use super::*;

    #[test]
    fn both_modes_round_trip_and_bind_counter_and_header() {
        for mode in [AES_GCM, XCHACHA] {
            let cipher = TransportCipher::new(mode, &[7; 32]).unwrap();
            let sealed = cipher.seal(5, b"header", b"opus").unwrap();
            assert_eq!(sealed.len(), 4 + 16);
            assert_eq!(cipher.open(5, b"header", &sealed).unwrap(), b"opus");
            assert_eq!(
                cipher.open(6, b"header", &sealed),
                Err(Error::Authentication)
            );
            assert_eq!(
                cipher.open(5, b"other", &sealed),
                Err(Error::Authentication)
            );
        }
    }

    #[test]
    fn modes_and_keys_outside_the_protocol_are_refused() {
        assert!(TransportCipher::new("xsalsa20_poly1305", &[0; 32]).is_err());
        assert!(TransportCipher::new(AES_GCM, &[0; 16]).is_err());
    }

    #[test]
    fn aes_is_preferred_and_xchacha_is_the_fallback() {
        assert_eq!(choose_mode([XCHACHA, AES_GCM]), Some(AES_GCM));
        assert_eq!(choose_mode(["xsalsa20_poly1305", XCHACHA]), Some(XCHACHA));
        assert_eq!(choose_mode(["xsalsa20_poly1305"]), None);
    }
}
