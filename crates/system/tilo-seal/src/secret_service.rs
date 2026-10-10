//! The wire encryption of the freedesktop Secret Service API
//! (`org.freedesktop.Secret.Session`).
//!
//! A client and the service agree on a session algorithm before any secret
//! crosses the bus. Besides `plain`, which this crate does not implement
//! because it has nothing to compute, libsecret speaks
//! [`DH_ALGORITHM`]:
//!
//! - key agreement: Diffie-Hellman in the 1024-bit Oakley group 2 (RFC 2409,
//!   generator 2); public values travel as big-endian byte strings;
//! - key derivation: `HKDF-SHA256(ikm = shared secret padded to 128 bytes,
//!   salt = none, info = empty)`, 128 bits;
//! - encryption: AES-128-CBC with PKCS#7 padding and a fresh 16-byte IV per
//!   secret, carried in the secret's `parameters`.
//!
//! This is a transport format fixed by the specification, not a construction
//! Tilo chose. CBC is unauthenticated, and the 1024-bit group is weak by
//! modern standards; the bus it protects is the person's private session bus,
//! and at-rest storage uses [`crate::envelope`] instead. The modular
//! exponentiation is not constant-time; the private exponent lives for one
//! session and is never reused.

use aes::Aes128;
use cbc::cipher::{BlockModeDecrypt, BlockModeEncrypt, KeyIvInit, block_padding::Pkcs7};
use hkdf::Hkdf;
use num_bigint::BigUint;
use sha2::Sha256;
use zeroize::{Zeroize, Zeroizing};

use crate::error::{Error, Result};
use crate::keys::{SymmetricKey, fill_random};

/// The session algorithm identifier libsecret negotiates.
pub const DH_ALGORITHM: &str = "dh-ietf1024-sha256-aes128-cbc-pkcs7";
/// Length of a CBC initialisation vector, the secret's `parameters`.
pub const IV_LEN: usize = 16;
/// Length of a public value and of the padded shared secret.
pub const GROUP_LEN: usize = 128;
const KEY_LEN: usize = 16;
/// The largest ciphertext [`TransportKey::decrypt`] accepts. Secrets are
/// credentials, not files.
pub const MAX_SECRET_LEN: usize = 1 << 20;

/// The at-rest key of one person's store on one device.
///
/// `HKDF-SHA256(ikm = device secret, salt = the person's random salt, info =
/// label ‖ 0 ‖ account)`. The device secret ties the store to this machine:
/// a copied home directory opens nowhere else. The salt, kept in the
/// person's own data directory, separates people who share the machine; the
/// account names the person again so one salt file never serves two.
///
/// # Errors
///
/// [`Error::InvalidKey`] for a device secret shorter than 16 bytes, which
/// cannot be a key.
pub fn store_key(
    device_secret: &[u8],
    user_salt: &[u8; 32],
    account: &[u8],
) -> Result<SymmetricKey> {
    if device_secret.len() < 16 {
        return Err(Error::InvalidKey);
    }
    let mut info = crate::Label::SecretService.as_str().as_bytes().to_vec();
    info.push(0);
    info.extend_from_slice(account);
    let mut out = Zeroizing::new([0_u8; 32]);
    Hkdf::<Sha256>::new(Some(user_salt), device_secret)
        .expand(&info, out.as_mut())
        .map_err(|_| Error::InvalidKey)?;
    Ok(SymmetricKey::from_bytes(*out))
}

/// RFC 2409, section 6.2: the second Oakley group's 1024-bit safe prime.
const PRIME_HEX: &str = "FFFFFFFFFFFFFFFFC90FDAA22168C234C4C6628B80DC1CD129024E088A67CC74\
020BBEA63B139B22514A08798E3404DDEF9519B3CD3A431B302B0A6DF25F1437\
4FE1356D6D51C245E485B576625E7EC6F44C42E9A637ED6B0BFF5CB6F406B7ED\
EE386BFB5A899FA5AE9F24117C4B1FE649286651ECE65381FFFFFFFFFFFFFFFF";

fn prime() -> BigUint {
    BigUint::parse_bytes(PRIME_HEX.as_bytes(), 16).unwrap_or_default()
}

/// Big-endian `value`, left-padded with zeros to [`GROUP_LEN`].
fn padded(value: &BigUint) -> Zeroizing<Vec<u8>> {
    let bytes = Zeroizing::new(value.to_bytes_be());
    let mut out = Zeroizing::new(vec![0_u8; GROUP_LEN.saturating_sub(bytes.len())]);
    out.extend_from_slice(&bytes);
    out
}

/// One side's half of a key agreement: a private exponent and the public
/// value to send. Consumed by [`Handshake::finish`].
pub struct Handshake {
    private: BigUint,
    public: Vec<u8>,
}

impl Handshake {
    /// Draw a private exponent and compute the public value.
    ///
    /// # Errors
    ///
    /// [`Error::Randomness`] when the operating system supplies none.
    pub fn new() -> Result<Self> {
        let mut seed = Zeroizing::new([0_u8; GROUP_LEN]);
        fill_random(seed.as_mut())?;
        let p = prime();
        let two = BigUint::from(2_u8);
        // 2 <= x <= p - 2.
        let private = BigUint::from_bytes_be(seed.as_ref()) % (&p - BigUint::from(3_u8)) + &two;
        let public = padded(&two.modpow(&private, &p)).to_vec();
        Ok(Self { private, public })
    }

    /// A handshake with a chosen exponent, for known-answer tests.
    #[cfg(test)]
    fn from_exponent(bytes: &[u8]) -> Self {
        let private = BigUint::from_bytes_be(bytes);
        let public = padded(&BigUint::from(2_u8).modpow(&private, &prime())).to_vec();
        Self { private, public }
    }

    /// The public value to send, [`GROUP_LEN`] big-endian bytes.
    #[must_use]
    pub fn public(&self) -> &[u8] {
        &self.public
    }

    /// Combine with the peer's public value into the session key.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidKey`] for a peer value outside `2..p-1`, which would
    /// force the shared secret to a known value.
    pub fn finish(self, peer_public: &[u8]) -> Result<TransportKey> {
        let p = prime();
        let peer = BigUint::from_bytes_be(peer_public);
        if peer_public.len() > GROUP_LEN || peer < BigUint::from(2_u8) || peer >= &p - 1_u8 {
            return Err(Error::InvalidKey);
        }
        let shared = padded(&peer.modpow(&self.private, &p));
        let mut key = Zeroizing::new([0_u8; KEY_LEN]);
        Hkdf::<Sha256>::new(None, &shared)
            .expand(&[], key.as_mut())
            .map_err(|_| Error::InvalidKey)?;
        Ok(TransportKey(key))
    }
}

impl Drop for Handshake {
    fn drop(&mut self) {
        // `BigUint` cannot zeroize its limbs; replacing it drops them, and
        // the exponent is single-use in any case.
        self.private = BigUint::default();
        self.public.zeroize();
    }
}

/// The AES-128 key one session derived.
pub struct TransportKey(Zeroizing<[u8; KEY_LEN]>);

impl TransportKey {
    /// Encrypt `plaintext` under a fresh IV.
    ///
    /// # Errors
    ///
    /// [`Error::Randomness`] when no IV can be drawn.
    pub fn encrypt(&self, plaintext: &[u8]) -> Result<([u8; IV_LEN], Vec<u8>)> {
        let mut iv = [0_u8; IV_LEN];
        fill_random(&mut iv)?;
        let encryptor = cbc::Encryptor::<Aes128>::new_from_slices(self.0.as_ref(), &iv)
            .map_err(|_| Error::Seal)?;
        Ok((iv, encryptor.encrypt_padded_vec::<Pkcs7>(plaintext)))
    }

    /// Decrypt a secret the peer encrypted.
    ///
    /// # Errors
    ///
    /// [`Error::Malformed`] for a wrong IV length, an oversized or
    /// misaligned ciphertext or bad padding.
    pub fn decrypt(&self, iv: &[u8], ciphertext: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
        if iv.len() != IV_LEN {
            return Err(Error::Malformed("iv length"));
        }
        if ciphertext.len() > MAX_SECRET_LEN {
            return Err(Error::Malformed("secret too long"));
        }
        let decryptor = cbc::Decryptor::<Aes128>::new_from_slices(self.0.as_ref(), iv)
            .map_err(|_| Error::Seal)?;
        decryptor
            .decrypt_padded_vec::<Pkcs7>(ciphertext)
            .map(Zeroizing::new)
            .map_err(|_| Error::Malformed("padding"))
    }

    /// The raw key, for known-answer tests.
    #[cfg(test)]
    fn bytes(&self) -> [u8; KEY_LEN] {
        *self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_sides_derive_one_key_and_round_trip_a_secret() {
        let client = Handshake::new().unwrap();
        let server = Handshake::new().unwrap();
        let (client_public, server_public) = (client.public().to_vec(), server.public().to_vec());
        assert_eq!(client_public.len(), GROUP_LEN);
        let client_key = client.finish(&server_public).unwrap();
        let server_key = server.finish(&client_public).unwrap();
        assert_eq!(client_key.bytes(), server_key.bytes());

        let (iv, ciphertext) = client_key.encrypt(b"hunter2").unwrap();
        assert_ne!(ciphertext.as_slice(), b"hunter2");
        assert_eq!(
            server_key.decrypt(&iv, &ciphertext).unwrap().as_slice(),
            b"hunter2"
        );
        // A fresh IV per secret.
        assert_ne!(iv, client_key.encrypt(b"hunter2").unwrap().0);
    }

    /// Computed independently: Python big-integer `pow`, HMAC-SHA256 HKDF
    /// and `openssl enc -aes-128-cbc`, from the exponents 0x11 * 128 and
    /// 0x37 * 128.
    #[test]
    fn matches_an_independent_computation() {
        let ours = Handshake::from_exponent(&[0x11; GROUP_LEN]);
        assert_eq!(
            hex::encode(ours.public()),
            "0801943b23ea3af0b48c1afc4c5905de5251679f7f2aaf9254cd826086462ec90f8595dbb1cee62f0225c662916a099e43698a80ac9ec968782ffb8dd6e9c676cbf4319edd89affca201cad7950b593db43f862fde4ccce9aa4e93a6eba285ef2a7192dfeaeab61c0bbaa7eb162bfdf0083f6bf2f73be0c7e69d20d2f2a76fc5"
        );
        let theirs = hex::decode("e4af64616727ce78fa42cbfda8352b05db0718395f008d098999cdea978230a6d92254a0bd36175c59bddeb330fb6f084a692369cc83ce57ec3fc93281720eecf9e20a9182e615fdb4aba38667a299e2e5c8759bc6c574001aa84635f11c970f8ecf90414a7ad3760987eb49bd80614733fa19e054083518f9d4f9809b8d6734").unwrap();
        let key = ours.finish(&theirs).unwrap();
        assert_eq!(hex::encode(key.bytes()), "a5c3c3609b0026f1c19cc56ce57a944f");
        let iv: Vec<u8> = (0..16).collect();
        let plain = key
            .decrypt(
                &iv,
                &hex::decode("75840a54ae2387af16a350fba793d6af").unwrap(),
            )
            .unwrap();
        assert_eq!(plain.as_slice(), b"hunter2 tilo");
    }

    #[test]
    fn store_keys_bind_device_person_and_account() {
        let base = store_key(&[1; 32], &[2; 32], b"1000").unwrap();
        assert_eq!(
            base.expose_secret(),
            store_key(&[1; 32], &[2; 32], b"1000")
                .unwrap()
                .expose_secret()
        );
        for other in [
            store_key(&[9; 32], &[2; 32], b"1000").unwrap(),
            store_key(&[1; 32], &[9; 32], b"1000").unwrap(),
            store_key(&[1; 32], &[2; 32], b"1001").unwrap(),
        ] {
            assert_ne!(base.expose_secret(), other.expose_secret());
        }
        assert!(store_key(&[1; 8], &[2; 32], b"1000").is_err());
    }

    #[test]
    fn degenerate_peer_values_are_refused() {
        let p = prime();
        for bad in [
            BigUint::from(0_u8),
            BigUint::from(1_u8),
            &p - 1_u8,
            p.clone(),
            &p + 5_u8,
        ] {
            let bytes = bad.to_bytes_be();
            assert_eq!(
                Handshake::new().unwrap().finish(&bytes).err(),
                Some(Error::InvalidKey),
                "{bad}"
            );
        }
        assert!(Handshake::new().unwrap().finish(&[]).is_err());
    }

    #[test]
    fn tampered_or_misshapen_ciphertext_is_refused() {
        let a = Handshake::new().unwrap();
        let b = Handshake::new().unwrap();
        let (a_public, b_public) = (a.public().to_vec(), b.public().to_vec());
        let key = a.finish(&b_public).unwrap();
        let other = b.finish(&a_public).unwrap();
        let (iv, ciphertext) = key.encrypt(b"secret").unwrap();
        assert!(other.decrypt(&iv[..8], &ciphertext).is_err());
        assert!(
            other
                .decrypt(&iv, &ciphertext[..ciphertext.len() - 1])
                .is_err()
        );
        assert!(other.decrypt(&iv, &[]).is_err());
    }
}
