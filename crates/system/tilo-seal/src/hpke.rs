//! Public-key sealing: RFC 9180 HPKE, base mode,
//! `DHKEM(X25519, HKDF-SHA256)` / `HKDF-SHA256` / `ChaCha20-Poly1305`.
//!
//! The [`Label`] is the HPKE `info` string, so a box sealed for one purpose
//! never opens for another. Optional associated data is authenticated but
//! not encrypted; the opener must present the same bytes.
//!
//! Wire layout of a [`SealedBox`] (version 1):
//!
//! ```text
//! 0      version = 0x01
//! 1..33  enc — the sender's ephemeral X25519 public key
//! 33..   HPKE ciphertext (plaintext length + 16-byte tag)
//! ```
//!
//! Base mode does not authenticate the sender: anyone holding the public
//! key can seal to it. Callers that need sender authenticity sign the box
//! or deliver it over an authenticated channel.

use hpke_rs::hpke_types::{AeadAlgorithm, KdfAlgorithm, KemAlgorithm};
use hpke_rs::{Hpke, HpkePrivateKey, HpkePublicKey, Mode};
use hpke_rs_rust_crypto::HpkeRustCrypto;
use zeroize::Zeroizing;

use crate::error::{Error, Result};
use crate::keys::{KEY_LEN, PublicKey, SecretKey};
use crate::label::Label;

/// Version byte of the only sealed-box layout.
pub const SEALED_BOX_VERSION: u8 = 1;
const ENC_END: usize = 1 + KEY_LEN;
const TAG_LEN: usize = 16;

fn suite() -> Hpke<HpkeRustCrypto> {
    Hpke::new(
        Mode::Base,
        KemAlgorithm::DhKem25519,
        KdfAlgorithm::HkdfSha256,
        AeadAlgorithm::ChaCha20Poly1305,
    )
}

/// A value sealed to one X25519 public key.
#[derive(Clone, PartialEq, Eq)]
pub struct SealedBox {
    enc: [u8; KEY_LEN],
    ciphertext: Vec<u8>,
}

impl SealedBox {
    /// The versioned wire encoding.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(ENC_END.saturating_add(self.ciphertext.len()));
        out.push(SEALED_BOX_VERSION);
        out.extend_from_slice(&self.enc);
        out.extend_from_slice(&self.ciphertext);
        out
    }

    /// Parse the wire encoding produced by [`Self::to_bytes`].
    ///
    /// # Errors
    ///
    /// [`Error::UnsupportedVersion`] for another version byte, and
    /// [`Error::Malformed`] when the value is too short to hold a tag.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let (&version, rest) = bytes
            .split_first()
            .ok_or(Error::Malformed("empty sealed box"))?;
        if version != SEALED_BOX_VERSION {
            return Err(Error::UnsupportedVersion(version));
        }
        if rest.len() < KEY_LEN.saturating_add(TAG_LEN) {
            return Err(Error::Malformed("sealed box too short"));
        }
        let (enc, ciphertext) = rest.split_at(KEY_LEN);
        Ok(Self {
            enc: <[u8; KEY_LEN]>::try_from(enc).map_err(|_| Error::Malformed("enc"))?,
            ciphertext: ciphertext.to_vec(),
        })
    }
}

impl std::fmt::Debug for SealedBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SealedBox")
            .field("ciphertext_len", &self.ciphertext.len())
            .finish_non_exhaustive()
    }
}

/// Seal `plaintext` to `recipient` for `label`, authenticating `aad`.
///
/// # Errors
///
/// [`Error::InvalidKey`] for a degenerate recipient key,
/// [`Error::Randomness`] when no randomness is available.
pub fn seal(
    recipient: &PublicKey,
    label: Label,
    aad: &[u8],
    plaintext: &[u8],
) -> Result<SealedBox> {
    let pk = HpkePublicKey::new(recipient.as_bytes().to_vec());
    let (enc, ciphertext) = suite()
        .seal(&pk, label.as_bytes(), aad, plaintext, None, None, None)
        .map_err(|error| match error {
            hpke_rs::HpkeError::InsufficientRandomness => Error::Randomness,
            hpke_rs::HpkeError::CryptoError(_) | hpke_rs::HpkeError::InvalidInput => {
                Error::InvalidKey
            }
            _ => Error::Seal,
        })?;
    Ok(SealedBox {
        enc: <[u8; KEY_LEN]>::try_from(enc.as_slice()).map_err(|_| Error::Seal)?,
        ciphertext,
    })
}

/// Open a box sealed by [`seal`] with the recipient's private key. The
/// `label` and `aad` must equal the sealer's.
///
/// # Errors
///
/// [`Error::Authentication`] when the key, label, AAD or bytes differ.
pub fn open(
    recipient: &SecretKey,
    label: Label,
    aad: &[u8],
    sealed: &SealedBox,
) -> Result<Zeroizing<Vec<u8>>> {
    let sk = HpkePrivateKey::new(recipient.expose_secret().to_vec());
    suite()
        .open(
            &sealed.enc,
            &sk,
            label.as_bytes(),
            aad,
            &sealed.ciphertext,
            None,
            None,
            None,
        )
        .map(Zeroizing::new)
        .map_err(|_| Error::Authentication)
}

/// Convenience: [`seal`] then [`SealedBox::to_bytes`].
///
/// # Errors
///
/// As [`seal`].
pub fn seal_to_bytes(
    recipient: &PublicKey,
    label: Label,
    aad: &[u8],
    plaintext: &[u8],
) -> Result<Vec<u8>> {
    seal(recipient, label, aad, plaintext).map(|sealed| sealed.to_bytes())
}

/// Convenience: [`SealedBox::from_bytes`] then [`open`].
///
/// # Errors
///
/// As [`SealedBox::from_bytes`] and [`open`].
pub fn open_bytes(
    recipient: &SecretKey,
    label: Label,
    aad: &[u8],
    sealed: &[u8],
) -> Result<Zeroizing<Vec<u8>>> {
    open(recipient, label, aad, &SealedBox::from_bytes(sealed)?)
}

/// Seal the same plaintext separately to each recipient (per-device
/// envelopes). Every box uses a fresh ephemeral key; boxes are returned in
/// recipient order, paired with the key they are addressed to.
pub fn seal_for_each<'a>(
    recipients: impl IntoIterator<Item = &'a PublicKey>,
    label: Label,
    aad: &[u8],
    plaintext: &[u8],
) -> Result<Vec<(PublicKey, SealedBox)>> {
    recipients
        .into_iter()
        .map(|recipient| seal(recipient, label, aad, plaintext).map(|sealed| (*recipient, sealed)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hpke_boxes_bind_label_aad_and_recipient_and_match_rfc9180() {
        {
            let alice = SecretKey::generate().unwrap();
            let bob = SecretKey::generate().unwrap();
            let sealed = seal(
                &alice.public_key().unwrap(),
                Label::RelayEnrollOffer,
                b"aad",
                b"hello",
            )
            .unwrap();
            let bytes = sealed.to_bytes();
            assert_eq!(bytes.first(), Some(&SEALED_BOX_VERSION));
            assert_eq!(
                &open_bytes(&alice, Label::RelayEnrollOffer, b"aad", &bytes)
                    .unwrap()
                    .as_slice(),
                b"hello"
            );
            assert_eq!(
                open(&alice, Label::DeviceFabricSecret, b"aad", &sealed),
                Err(Error::Authentication)
            );
            assert_eq!(
                open(&alice, Label::RelayEnrollOffer, b"other", &sealed),
                Err(Error::Authentication)
            );
            assert_eq!(
                open(&bob, Label::RelayEnrollOffer, b"aad", &sealed),
                Err(Error::Authentication)
            );
            let mut tampered = bytes.clone();
            if let Some(last) = tampered.last_mut() {
                *last ^= 1;
            }
            assert_eq!(
                open_bytes(&alice, Label::RelayEnrollOffer, b"aad", &tampered),
                Err(Error::Authentication)
            );
            assert!(matches!(
                SealedBox::from_bytes(&[2, 0, 0]),
                Err(Error::UnsupportedVersion(2))
            ));
            assert!(SealedBox::from_bytes(&bytes[..20]).is_err());
        }
        {
            assert!(seal(&PublicKey::from_bytes([0; 32]), Label::KeyWrap, b"", b"x").is_err());
        }
        {
            let keys: Vec<SecretKey> = (0..3).map(|_| SecretKey::generate().unwrap()).collect();
            let publics: Vec<PublicKey> = keys.iter().map(|k| k.public_key().unwrap()).collect();
            let boxes =
                seal_for_each(&publics, Label::DeviceFabricSecret, b"ws", b"secret").unwrap();
            assert_eq!(boxes.len(), 3);
            for (key, (public, sealed)) in keys.iter().zip(&boxes) {
                assert_eq!(*public, key.public_key().unwrap());
                assert_eq!(
                    open(key, Label::DeviceFabricSecret, b"ws", sealed)
                        .unwrap()
                        .as_slice(),
                    b"secret"
                );
            }
            assert!(open(&keys[0], Label::DeviceFabricSecret, b"ws", &boxes[1].1).is_err());
        }
        // Phase `open_matches_rfc9180_test_vector`.
        // RFC 9180 appendix A.2.1 (base mode, X25519, HKDF-SHA256,
        // ChaCha20-Poly1305), sequence number 0.
        {
            let sk_r =
                hex::decode("8057991eef8f1f1af18f4a9491d16a1ce333f695d4db8e38da75975c4478e0fb")
                    .unwrap();
            let enc =
                hex::decode("1afa08d3dec047a643885163f1180476fa7ddb54c6a8029ea33f95796bf2ac4a")
                    .unwrap();
            let info = hex::decode("4f6465206f6e2061204772656369616e2055726e").unwrap();
            let aad = hex::decode("436f756e742d30").unwrap();
            let ct = hex::decode(
                "1c5250d8034ec2b784ba2cfd69dbdb8af406cfe3ff938e131f0def8c8b60b4db21993c62ce81883d2dd1b51a28",
            )
            .unwrap();
            let pt = suite()
                .open(
                    &enc,
                    &HpkePrivateKey::new(sk_r),
                    &info,
                    &aad,
                    &ct,
                    None,
                    None,
                    None,
                )
                .unwrap();
            assert_eq!(
                hex::encode(pt),
                "4265617574792069732074727574682c20747275746820626561757479"
            );
        }
    }
}
