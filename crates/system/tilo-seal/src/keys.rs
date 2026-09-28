//! Key types. Secret halves are zeroized on drop, compare in constant
//! time, and never print their bytes.

use hpke_rs_crypto::HpkeCrypto as _;
use hpke_rs_crypto::types::KemAlgorithm;
use hpke_rs_rust_crypto::HpkeRustCrypto;
use subtle::ConstantTimeEq as _;
use zeroize::Zeroizing;

use crate::error::{Error, Result};

/// Length of every symmetric key and X25519 key handled by this crate.
pub const KEY_LEN: usize = 32;

/// Fill `out` from the operating system CSPRNG.
///
/// # Errors
///
/// [`Error::Randomness`] when the OS cannot supply randomness.
pub fn fill_random(out: &mut [u8]) -> Result<()> {
    getrandom::fill(out).map_err(|_| Error::Randomness)
}

/// A 256-bit symmetric key (content key, vault key, key-encryption key).
#[derive(Clone)]
pub struct SymmetricKey(Zeroizing<[u8; KEY_LEN]>);

impl SymmetricKey {
    /// Wrap existing key bytes (from a secret service, a KDF, an unwrap).
    pub fn from_bytes(bytes: [u8; KEY_LEN]) -> Self {
        Self(Zeroizing::new(bytes))
    }

    /// Copy key bytes out of a slice of exactly [`KEY_LEN`] bytes.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidKey`] for any other length.
    pub fn from_slice(bytes: &[u8]) -> Result<Self> {
        let mut key = Zeroizing::new([0_u8; KEY_LEN]);
        if bytes.len() != KEY_LEN {
            return Err(Error::InvalidKey);
        }
        key.copy_from_slice(bytes);
        Ok(Self(key))
    }

    /// Draw a fresh key from the OS CSPRNG.
    ///
    /// # Errors
    ///
    /// [`Error::Randomness`] when the OS cannot supply randomness.
    pub fn generate() -> Result<Self> {
        let mut key = Zeroizing::new([0_u8; KEY_LEN]);
        fill_random(key.as_mut())?;
        Ok(Self(key))
    }

    /// The raw key bytes, for handing to a secret store. Keep the borrow
    /// short; never log it.
    pub fn expose_secret(&self) -> &[u8; KEY_LEN] {
        &self.0
    }

    /// A short public identifier for this key: the first 8 bytes of
    /// HKDF-SHA256 output under a fixed label. Stored in envelope headers
    /// so opening with the wrong key fails fast and distinguishably from
    /// tampering. It reveals nothing a trial decryption would not.
    pub fn key_id(&self) -> KeyId {
        let hk = hkdf::Hkdf::<sha2::Sha256>::new(None, self.0.as_ref());
        let mut id = [0_u8; KEY_ID_LEN];
        // Expanding 8 bytes from SHA-256 HKDF cannot exceed the 255*32 limit.
        if hk.expand(b"tilo-seal/v1/key-id", &mut id).is_err() {
            return KeyId([0; KEY_ID_LEN]);
        }
        KeyId(id)
    }
}

impl PartialEq for SymmetricKey {
    fn eq(&self, other: &Self) -> bool {
        self.0.as_ref().ct_eq(other.0.as_ref()).into()
    }
}

impl Eq for SymmetricKey {}

impl std::fmt::Debug for SymmetricKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("SymmetricKey").field(&self.key_id()).finish()
    }
}

/// Length of a [`KeyId`].
pub const KEY_ID_LEN: usize = 8;

/// Public identifier of a [`SymmetricKey`].
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyId(pub [u8; KEY_ID_LEN]);

impl std::fmt::Debug for KeyId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "KeyId(")?;
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        write!(f, ")")
    }
}

/// An X25519 public key that sealed data can be addressed to.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct PublicKey([u8; KEY_LEN]);

impl PublicKey {
    /// Wrap 32 public-key bytes.
    pub const fn from_bytes(bytes: [u8; KEY_LEN]) -> Self {
        Self(bytes)
    }

    /// Parse a 32-byte slice.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidKey`] for any other length.
    pub fn from_slice(bytes: &[u8]) -> Result<Self> {
        <[u8; KEY_LEN]>::try_from(bytes)
            .map(Self)
            .map_err(|_| Error::InvalidKey)
    }

    /// The encoded key.
    pub const fn to_bytes(self) -> [u8; KEY_LEN] {
        self.0
    }

    /// Borrow the encoded key.
    pub const fn as_bytes(&self) -> &[u8; KEY_LEN] {
        &self.0
    }
}

impl std::fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PublicKey(")?;
        for byte in self.0.iter().take(4) {
            write!(f, "{byte:02x}")?;
        }
        write!(f, "..)")
    }
}

/// An X25519 private key that sealed data can be opened with.
#[derive(Clone)]
pub struct SecretKey(Zeroizing<[u8; KEY_LEN]>);

impl SecretKey {
    /// Draw a fresh private key from the OS CSPRNG.
    ///
    /// # Errors
    ///
    /// [`Error::Randomness`] when the OS cannot supply randomness.
    pub fn generate() -> Result<Self> {
        let mut key = Zeroizing::new([0_u8; KEY_LEN]);
        fill_random(key.as_mut())?;
        Ok(Self(key))
    }

    /// Wrap stored private-key bytes (any 32 bytes are a valid X25519
    /// scalar; clamping happens inside the primitive).
    pub fn from_bytes(bytes: [u8; KEY_LEN]) -> Self {
        Self(Zeroizing::new(bytes))
    }

    /// Copy private-key bytes out of a slice of exactly [`KEY_LEN`] bytes.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidKey`] for any other length.
    pub fn from_slice(bytes: &[u8]) -> Result<Self> {
        SymmetricKey::from_slice(bytes).map(|key| Self(key.0))
    }

    /// The raw private key, for persisting in a sealed store.
    pub fn expose_secret(&self) -> &[u8; KEY_LEN] {
        &self.0
    }

    /// The matching public key.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidKey`] if the primitive rejects the scalar (it accepts
    /// every 32-byte value today).
    pub fn public_key(&self) -> Result<PublicKey> {
        let public = HpkeRustCrypto::secret_to_public(KemAlgorithm::DhKem25519, self.0.as_ref())
            .map_err(|_| Error::InvalidKey)?;
        PublicKey::from_slice(&public)
    }
}

impl PartialEq for SecretKey {
    fn eq(&self, other: &Self) -> bool {
        self.0.as_ref().ct_eq(other.0.as_ref()).into()
    }
}

impl Eq for SecretKey {}

impl std::fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.public_key() {
            Ok(public) => f.debug_tuple("SecretKey").field(&public).finish(),
            Err(_) => f.write_str("SecretKey(..)"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{SecretKey, SymmetricKey};

    #[test]
    fn keys_match_rfc7748_and_never_debug_secret_bytes() {
        // Phase `debug_output_never_contains_secret_bytes`.
        {
            let key = SymmetricKey::from_bytes([0xab; 32]);
            let rendered = format!("{key:?}");
            assert!(!rendered.contains("abab"), "{rendered}");
            let secret = SecretKey::from_bytes([0xcd; 32]);
            let rendered = format!("{secret:?}");
            assert!(!rendered.contains("cdcd"), "{rendered}");
        }
        // Phase `x25519_public_key_matches_rfc7748_vector`.
        {
            // RFC 7748 section 6.1, Alice.
            let sk: [u8; 32] =
                hex::decode("77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a")
                    .unwrap()
                    .try_into()
                    .unwrap();
            let pk = SecretKey::from_bytes(sk).public_key().unwrap();
            assert_eq!(
                hex::encode(pk.to_bytes()),
                "8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a"
            );
        }
    }
}
