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
use hkdf::hmac::{Hmac, KeyInit, Mac};
use num_bigint::BigUint;
use sha2::Sha256;
use std::ops::{Add, Rem, Sub};

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

/// The at-rest key of one person's store on any device, from the person's
/// own key alone.
///
/// `HKDF-SHA256(ikm = user secret, salt = the person's random salt, info =
/// label ‖ 0 ‖ user identity)`.
///
/// [`store_key`] ties the vault to the machine: losing the device key loses
/// the store even when the person's own key is intact. This derivation is
/// the other way round — the person's key keeps the store, and a machine
/// that cannot reach it keeps the vault shut. The user identity is the
/// public half, which separates a person's stores in a shared data home
/// without naming them; the salt stays in the person's data directory, so
/// the same key with another salt opens nothing.
///
/// # Errors
///
/// [`Error::InvalidKey`] for a user secret shorter than 16 bytes and for an
/// empty or over-long user identity, neither of which can be key material.
pub fn user_store_key(
    user_secret: &[u8],
    user_salt: &[u8; 32],
    user_identity: &[u8],
) -> Result<SymmetricKey> {
    if user_secret.len() < 16 || user_identity.is_empty() || user_identity.len() > 64 {
        return Err(Error::InvalidKey);
    }
    let mut info = crate::Label::SecretService.as_str().as_bytes().to_vec();
    info.push(0);
    info.extend_from_slice(user_identity);
    let mut out = Zeroizing::new([0_u8; 32]);
    Hkdf::<Sha256>::new(Some(user_salt), user_secret)
        .expand(&info, out.as_mut())
        .map_err(|_| Error::InvalidKey)?;
    Ok(SymmetricKey::from_bytes(*out))
}

/// Length of a [`CallerKey`] proof.
pub const CALLER_PROOF_LEN: usize = 32;

/// What a process that holds the device identity proves it with, to the
/// person's Secret Service, when the service cannot read its executable.
///
/// A process that loads the device key makes itself non-dumpable, and the
/// kernel then refuses its `/proc/<pid>/exe` to every other process of the
/// user, the service included. Such a process answers the service's
/// challenge instead: `HMAC-SHA256(key, challenge ‖ caller ‖ program)`, each
/// field preceded by its length as a big-endian `u64`, where `caller` is the
/// process's unique bus name and `program` the executable it names, and
/// `key = HKDF-SHA256(ikm = device secret, salt = none, info =`
/// [`crate::Label::StoreCallerProof`]`)`.
///
/// The proof grants nothing its holder does not already have: the device
/// secret, together with the person's salt in their own data directory, is
/// exactly what derives [`store_key`], so whoever can prove can already open
/// the store's file. The key is used for nothing else.
#[derive(Clone)]
pub struct CallerKey(Zeroizing<[u8; 32]>);

impl CallerKey {
    /// The caller key of `device_secret`.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidKey`] for a device secret shorter than 16 bytes, which
    /// cannot be a key.
    pub fn derive(device_secret: &[u8]) -> Result<Self> {
        if device_secret.len() < 16 {
            return Err(Error::InvalidKey);
        }
        let mut out = Zeroizing::new([0_u8; 32]);
        Hkdf::<Sha256>::new(None, device_secret)
            .expand(crate::Label::StoreCallerProof.as_bytes(), out.as_mut())
            .map_err(|_| Error::InvalidKey)?;
        Ok(Self(out))
    }

    /// The proof that answers `challenge` for the bus name `caller` running
    /// `program`.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidKey`] if HMAC refuses the key, which it never does
    /// for 32 bytes.
    pub fn prove(
        &self,
        challenge: &[u8],
        caller: &str,
        program: &[u8],
    ) -> Result<[u8; CALLER_PROOF_LEN]> {
        Ok(self
            .mac(challenge, caller, program)?
            .finalize()
            .into_bytes()
            .into())
    }

    /// Whether `proof` answers `challenge` for `caller` running `program`.
    /// The comparison takes the same time however much of it matches.
    #[must_use]
    pub fn verify(&self, challenge: &[u8], caller: &str, program: &[u8], proof: &[u8]) -> bool {
        self.mac(challenge, caller, program)
            .is_ok_and(|mac| mac.verify_slice(proof).is_ok())
    }

    fn mac(&self, challenge: &[u8], caller: &str, program: &[u8]) -> Result<Hmac<Sha256>> {
        let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(self.0.as_ref())
            .map_err(|_| Error::InvalidKey)?;
        for field in [challenge, caller.as_bytes(), program] {
            mac.update(&u64::try_from(field.len()).unwrap_or(u64::MAX).to_be_bytes());
            mac.update(field);
        }
        Ok(mac)
    }
}

impl std::fmt::Debug for CallerKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CallerKey(..)")
    }
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
        // Function-call syntax: `p` is a 1024-bit constant, so these
        // big-integer operations cannot underflow or overflow.
        let span = Sub::sub(&p, BigUint::from(3_u8));
        let private = Add::add(Rem::rem(BigUint::from_bytes_be(seed.as_ref()), span), &two);
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
        // libsecret sends the value as a signed big-endian integer, so a
        // value with its top bit set arrives with a leading zero byte:
        // leading zeros carry no information and are not counted.
        let digits = peer_public.iter().skip_while(|byte| **byte == 0).count();
        let peer = BigUint::from_bytes_be(peer_public);
        if digits > GROUP_LEN || peer < BigUint::from(2_u8) || peer >= Sub::sub(&p, 1_u8) {
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
        // Decrypt in place in a wiped buffer: bad padding must not leave the
        // earlier, correctly decrypted secret to the allocator.
        let mut plaintext = Zeroizing::new(ciphertext.to_vec());
        let length = decryptor
            .decrypt_padded::<Pkcs7>(plaintext.as_mut_slice())
            .map_err(|_| Error::Malformed("padding"))?
            .len();
        plaintext.truncate(length);
        Ok(plaintext)
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

    /// Computed independently with Python's `hmac` and `hashlib`: HKDF-SHA256
    /// of `[7; 32]` with an all-zero salt, then HMAC-SHA256 over the
    /// length-prefixed fields.
    #[test]
    fn a_caller_proof_matches_an_independent_computation() {
        let key = CallerKey::derive(&[7; 32]).unwrap();
        let proof = key.prove(&[9; 32], ":1.42", b"/usr/bin/shell").unwrap();
        assert_eq!(
            hex::encode(proof),
            "57ce696f68420fb60ceab4713cfc659f39b2d97e01b5f7ee2e377a4f9740d943"
        );
        assert!(key.verify(&[9; 32], ":1.42", b"/usr/bin/shell", &proof));
    }

    #[test]
    fn a_caller_proof_binds_the_device_the_challenge_the_caller_and_the_program() {
        let key = CallerKey::derive(&[7; 32]).unwrap();
        let proof = key.prove(&[9; 32], ":1.42", b"/usr/bin/shell").unwrap();
        let other_device = CallerKey::derive(&[8; 32]).unwrap();
        assert!(!other_device.verify(&[9; 32], ":1.42", b"/usr/bin/shell", &proof));
        assert!(!key.verify(&[10; 32], ":1.42", b"/usr/bin/shell", &proof));
        assert!(!key.verify(&[9; 32], ":1.43", b"/usr/bin/shell", &proof));
        assert!(!key.verify(&[9; 32], ":1.42", b"/usr/bin/sh", &proof));
        // Field boundaries are part of the proof: moving bytes between the
        // caller and the program is another proof.
        assert!(!key.verify(&[9; 32], ":1.42/", b"usr/bin/shell", &proof));
        assert!(!key.verify(&[9; 32], ":1.42", b"/usr/bin/shell", &proof[..31]));
        assert!(CallerKey::derive(&[7; 15]).is_err());
        // The key is not the store's key under another name.
        let salt = [0_u8; 32];
        let store = store_key(&[7; 32], &salt, b"").unwrap();
        assert_ne!(store.expose_secret(), &*key.0);
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
    fn user_store_keys_bind_the_person_the_salt_and_the_identity() {
        let base = user_store_key(&[1; 32], &[2; 32], &[3; 32]).unwrap();
        assert_eq!(
            base.expose_secret(),
            user_store_key(&[1; 32], &[2; 32], &[3; 32])
                .unwrap()
                .expose_secret()
        );
        for other in [
            user_store_key(&[9; 32], &[2; 32], &[3; 32]).unwrap(),
            user_store_key(&[1; 32], &[9; 32], &[3; 32]).unwrap(),
            user_store_key(&[1; 32], &[2; 32], &[9; 32]).unwrap(),
        ] {
            assert_ne!(base.expose_secret(), other.expose_secret());
        }
        // The same person and salt on another device key: the store is the
        // same, which is the whole point of keying it by the person.
        assert_ne!(base.expose_secret(), &[0_u8; 32]);
        assert!(user_store_key(&[1; 8], &[2; 32], &[3; 32]).is_err());
        assert!(user_store_key(&[1; 32], &[2; 32], &[]).is_err());
        assert!(user_store_key(&[1; 32], &[2; 32], &[3; 65]).is_err());
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

    /// Real libsecret (gcrypt's signed format) prefixes a zero byte when the
    /// top bit is set, giving 129 bytes. That came out as "invalid public
    /// value" in about half of all sessions.
    #[test]
    fn a_peer_value_with_leading_zero_bytes_gives_the_same_key() {
        let peer = Handshake::from_exponent(&[0x22; GROUP_LEN]);
        let plain = Handshake::from_exponent(&[0x11; GROUP_LEN])
            .finish(peer.public())
            .unwrap();
        for zeros in 1..4 {
            let mut padded = vec![0_u8; zeros];
            padded.extend_from_slice(peer.public());
            let key = Handshake::from_exponent(&[0x11; GROUP_LEN])
                .finish(&padded)
                .unwrap();
            assert_eq!(key.bytes(), plain.bytes(), "{zeros} leading zero bytes");
        }
        // Zeros do not hide a value that is too large.
        let mut oversized = vec![0_u8; 2];
        oversized.extend_from_slice(&[0xff; GROUP_LEN + 1]);
        assert!(Handshake::new().unwrap().finish(&oversized).is_err());
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
