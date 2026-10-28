use anyhow::{Result, anyhow};
use hmac::{KeyInit, Mac as _};
use zeroize::Zeroizing;

pub const KEY_LEN: usize = 32;
const SYSTEM_VOLUME_DOMAIN: &[u8] = b"tilo-system-volume-v1";

type HmacSha256 = hmac::Hmac<sha2::Sha256>;

/// The root of a person's personal scope.
#[derive(Clone)]
pub struct UserKey(Zeroizing<[u8; KEY_LEN]>);

impl UserKey {
    pub fn generate() -> Result<Self> {
        let mut bytes = Zeroizing::new([0_u8; KEY_LEN]);
        getrandom::fill(bytes.as_mut()).map_err(|e| anyhow!("reading OS randomness: {e}"))?;
        Ok(Self(bytes))
    }

    pub fn from_bytes(bytes: [u8; KEY_LEN]) -> Self {
        Self(Zeroizing::new(bytes))
    }

    pub fn from_slice(bytes: &[u8]) -> Option<Self> {
        <[u8; KEY_LEN]>::try_from(bytes).ok().map(Self::from_bytes)
    }

    pub fn expose(&self) -> &[u8; KEY_LEN] {
        &self.0
    }

    /// The passphrase of this person's system-volume keyslot.
    pub fn system_volume_passphrase(&self) -> Zeroizing<String> {
        Zeroizing::new(hex::encode(self.derive(SYSTEM_VOLUME_DOMAIN).as_slice()))
    }

    pub(crate) fn derive(&self, domain: &[u8]) -> Zeroizing<[u8; 32]> {
        hmac_sha256(self.0.as_slice(), domain)
    }

    pub(crate) fn as_symmetric(&self) -> tilo_seal::SymmetricKey {
        tilo_seal::SymmetricKey::from_bytes(*self.0)
    }
}

#[allow(
    clippy::expect_used,
    reason = "HMAC accepts any key length; new_from_slice cannot fail for SHA-256"
)]
pub(crate) fn hmac_sha256(key: &[u8], message: &[u8]) -> Zeroizing<[u8; 32]> {
    let mut mac =
        <HmacSha256 as KeyInit>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(message);
    Zeroizing::new(mac.finalize().into_bytes().into())
}

impl std::fmt::Debug for UserKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("UserKey(..)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_slot_passphrase_is_lowercase_hex_of_the_contract_hmac() {
        let key = UserKey::from_bytes([7; KEY_LEN]);
        let passphrase = key.system_volume_passphrase();
        assert_eq!(passphrase.len(), 64);
        assert!(
            passphrase
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
        );
        assert_eq!(
            *passphrase,
            hex::encode(hmac_sha256(&[7; KEY_LEN], b"tilo-system-volume-v1").as_slice())
        );
        assert_ne!(
            *passphrase,
            *UserKey::from_bytes([8; KEY_LEN]).system_volume_passphrase()
        );
    }

    #[test]
    fn hmac_matches_rfc_4231_case_2() {
        assert_eq!(
            hex::encode(hmac_sha256(b"Jefe", b"what do ya want for nothing?").as_slice()),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }
}
