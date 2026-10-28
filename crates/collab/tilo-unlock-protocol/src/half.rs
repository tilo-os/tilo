//! The two halves of a slot key and the sealed form siblings hold.

use serde::{Deserialize, Serialize};
use tilo_seal::{Label, PublicKey, SecretKey};
use zeroize::Zeroizing;

use crate::cert::BootKeyCert;

/// Length of each half.
pub const HALF_LEN: usize = 32;
/// Version byte of [`SealedSiblingHalf`].
const SEALED_HALF_VERSION: u8 = 1;
const SLOT_KEY_CONTEXT: &str = "tilo-unlock slot key v1";

/// One half of a slot key. Wiped on drop; `Debug` never prints it.
#[derive(Clone, PartialEq, Eq)]
pub struct Half(Zeroizing<[u8; HALF_LEN]>);

impl Serialize for Half {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_bytes(self.expose())
    }
}

impl<'de> Deserialize<'de> for Half {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let bytes = Zeroizing::new(serde_bytes::ByteBuf::deserialize(d)?.into_vec());
        <[u8; HALF_LEN]>::try_from(bytes.as_slice())
            .map(Self::from_bytes)
            .map_err(|_| serde::de::Error::invalid_length(bytes.len(), &"32 bytes"))
    }
}

impl Half {
    /// Wrap half bytes.
    #[must_use]
    pub fn from_bytes(bytes: [u8; HALF_LEN]) -> Self {
        Self(Zeroizing::new(bytes))
    }

    /// Draw a fresh half from the OS CSPRNG.
    ///
    /// # Errors
    ///
    /// When the OS cannot supply randomness.
    pub fn generate() -> Result<Self, tilo_seal::Error> {
        let mut bytes = Zeroizing::new([0_u8; HALF_LEN]);
        tilo_seal::fill_random(bytes.as_mut())?;
        Ok(Self(bytes))
    }

    /// The raw bytes. Keep the borrow short; never log it.
    #[must_use]
    pub fn expose(&self) -> &[u8; HALF_LEN] {
        &self.0
    }
}

impl std::fmt::Debug for Half {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Half(..)")
    }
}

/// The key a slot opens with, as the LUKS passphrase: lowercase hex of
/// `blake3::derive_key(SLOT_KEY_CONTEXT, device_half || sibling_half ||
/// volume_uuid)`. Neither half alone determines it.
#[must_use]
pub fn derive_slot_key(
    device_half: &Half,
    sibling_half: &Half,
    volume_uuid: &[u8; 16],
) -> Zeroizing<String> {
    let mut material = Zeroizing::new([0_u8; 2 * HALF_LEN + 16]);
    let (device, rest) = material.split_at_mut(HALF_LEN);
    let (sibling, volume) = rest.split_at_mut(HALF_LEN);
    device.copy_from_slice(device_half.expose());
    sibling.copy_from_slice(sibling_half.expose());
    volume.copy_from_slice(volume_uuid);
    let key = Zeroizing::new(blake3::derive_key(SLOT_KEY_CONTEXT, material.as_ref()));
    let mut out = Zeroizing::new(String::with_capacity(2 * HALF_LEN));
    for byte in key.iter() {
        let _ = std::fmt::Write::write_fmt(&mut *out, format_args!("{byte:02x}"));
    }
    out
}

/// A sibling half sealed to the person's secret-sync key and bound to one
/// boot key certificate. This is what every sibling stores and replicates;
/// it opens only on a device that holds the person's root.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SealedSiblingHalf {
    /// Format version (1).
    pub version: u8,
    /// `tilo_seal::SealedBox` wire bytes, label `UnlockSiblingHalf`, the
    /// certificate's signing digest as associated data.
    #[serde(with = "serde_bytes")]
    pub sealed: Vec<u8>,
}

/// Seal `half` to the person's secret-sync public key for `cert`.
///
/// # Errors
///
/// When the certificate cannot be encoded or sealing fails.
pub fn seal_sibling_half(
    secret_sync: &PublicKey,
    cert: &BootKeyCert,
    half: &Half,
) -> Result<SealedSiblingHalf, tilo_seal::Error> {
    let aad = cert
        .signing_digest()
        .map_err(|_| tilo_seal::Error::Malformed("boot key certificate"))?;
    let sealed =
        tilo_seal::hpke::seal_to_bytes(secret_sync, Label::UnlockSiblingHalf, &aad, half.expose())?;
    Ok(SealedSiblingHalf {
        version: SEALED_HALF_VERSION,
        sealed,
    })
}

/// Open a sealed sibling half with the person's secret-sync key. Fails for
/// any other certificate than the one it was sealed for.
pub fn open_sibling_half(
    secret_sync: &SecretKey,
    cert: &BootKeyCert,
    sealed: &SealedSiblingHalf,
) -> Result<Half, tilo_seal::Error> {
    if sealed.version != SEALED_HALF_VERSION {
        return Err(tilo_seal::Error::UnsupportedVersion(sealed.version));
    }
    let aad = cert
        .signing_digest()
        .map_err(|_| tilo_seal::Error::Malformed("boot key certificate"))?;
    let plain =
        tilo_seal::hpke::open_bytes(secret_sync, Label::UnlockSiblingHalf, &aad, &sealed.sealed)?;
    let bytes = <[u8; HALF_LEN]>::try_from(plain.as_slice())
        .map_err(|_| tilo_seal::Error::Malformed("sibling half length"))?;
    Ok(Half::from_bytes(bytes))
}
