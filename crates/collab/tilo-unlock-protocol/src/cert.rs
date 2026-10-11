//! Boot key certificates and the enrolment exchange that issues them.

use ed25519_dalek::{Signature, Signer as _, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::half::Half;
use crate::{PROTOCOL_VERSION, SlotId, digest};

const BOOT_CERT_CONTEXT: &str = "tilo-unlock boot key certificate v1";
const ENROL_PROOF_CONTEXT: &str = "tilo-unlock enrol proof v1";

/// The person's identity certifies one boot-only key for one device and one
/// LUKS slot.
///
/// The boot key never signs anything but [`crate::UnlockRequest`]s and the
/// [`EnrolRequest`] proof, so a leaked boot partition exposes a key that
/// can ask, never one that can act as the device.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BootKeyCert {
    /// [`PROTOCOL_VERSION`] at issuance.
    pub version: u16,
    /// The person's root public key, which signs this certificate.
    pub user_public_key: [u8; 32],
    /// The fleet key of the enrolled device (its `DeviceCert` subject).
    /// Removing that device from the fleet revokes this certificate.
    pub device_public_key: [u8; 32],
    /// The boot-only Ed25519 key early boot signs requests with.
    pub boot_public_key: [u8; 32],
    /// The one slot this key may ask a sibling half for.
    pub slot: SlotId,
    /// The device's TPM attestation key (SEC1 P-256) when it has a TPM.
    /// Only a device with one can be auto-approved.
    pub attestation_key: Option<Vec<u8>>,
    /// Nanoseconds since the Unix epoch.
    pub issued_at_ns: u64,
    /// Root signature over [`BootKeyCert::signing_digest`].
    #[serde(with = "serde_bytes")]
    pub signature: [u8; 64],
}

#[derive(Serialize)]
struct BootKeyCertView<'a> {
    version: u16,
    user_public_key: &'a [u8; 32],
    device_public_key: &'a [u8; 32],
    boot_public_key: &'a [u8; 32],
    slot: &'a SlotId,
    attestation_key: &'a Option<Vec<u8>>,
    issued_at_ns: u64,
}

/// Why a boot key certificate is not acceptable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum BootKeyCertError {
    /// Another protocol version.
    #[error("unsupported boot key certificate version")]
    UnsupportedVersion,
    /// The signing key is not the root named by the certificate.
    #[error("the signing key is not the certificate's root")]
    WrongSigner,
    /// The root or boot public key is not a valid Ed25519 point.
    #[error("invalid public key")]
    InvalidKey,
    /// The signature does not verify.
    #[error("boot key certificate signature did not verify")]
    BadSignature,
    /// The certificate could not be encoded.
    #[error("boot key certificate could not be encoded")]
    Encoding,
}

impl BootKeyCert {
    fn view(&self) -> BootKeyCertView<'_> {
        BootKeyCertView {
            version: self.version,
            user_public_key: &self.user_public_key,
            device_public_key: &self.device_public_key,
            boot_public_key: &self.boot_public_key,
            slot: &self.slot,
            attestation_key: &self.attestation_key,
            issued_at_ns: self.issued_at_ns,
        }
    }

    /// The digest the root signs, which also names the enrolment: sealed
    /// halves and released halves are bound to it.
    ///
    /// # Errors
    ///
    /// [`BootKeyCertError::Encoding`] if the view cannot be encoded.
    pub fn signing_digest(&self) -> Result<[u8; 32], BootKeyCertError> {
        digest(BOOT_CERT_CONTEXT, &self.view()).map_err(|_| BootKeyCertError::Encoding)
    }

    /// Sign with the person's root key, filling `signature`.
    ///
    /// # Errors
    ///
    /// [`BootKeyCertError::WrongSigner`] when `root` is not
    /// `user_public_key`, [`BootKeyCertError::Encoding`] otherwise.
    pub fn sign(&mut self, root: &SigningKey) -> Result<(), BootKeyCertError> {
        if root.verifying_key().to_bytes() != self.user_public_key {
            return Err(BootKeyCertError::WrongSigner);
        }
        self.signature = root.sign(&self.signing_digest()?).to_bytes();
        Ok(())
    }

    /// Verify the version, both keys and the root signature. Whether the
    /// device is still in the fleet is the verifier's own check.
    ///
    /// # Errors
    ///
    /// As [`BootKeyCertError`].
    pub fn verify(&self) -> Result<(), BootKeyCertError> {
        if self.version != PROTOCOL_VERSION {
            return Err(BootKeyCertError::UnsupportedVersion);
        }
        VerifyingKey::from_bytes(&self.boot_public_key)
            .map_err(|_| BootKeyCertError::InvalidKey)?;
        let root = VerifyingKey::from_bytes(&self.user_public_key)
            .map_err(|_| BootKeyCertError::InvalidKey)?;
        root.verify_strict(
            &self.signing_digest()?,
            &Signature::from_bytes(&self.signature),
        )
        .map_err(|_| BootKeyCertError::BadSignature)
    }

    /// The boot key as a verifying key.
    ///
    /// # Errors
    ///
    /// [`BootKeyCertError::InvalidKey`] for a key off the curve.
    pub fn boot_key(&self) -> Result<VerifyingKey, BootKeyCertError> {
        VerifyingKey::from_bytes(&self.boot_public_key).map_err(|_| BootKeyCertError::InvalidKey)
    }
}

/// What `tilo-disk-encrypt enrol --sibling` hands the sibling service on the
/// enrolling device (the typed `device.unlock.enrol` action).
///
/// The caller generated the boot key pair and both halves, enrolled the
/// derived key into `slot`, and keeps only the device half afterwards. The
/// service certifies the boot key with the person's identity, seals the
/// sibling half to the person's secret-sync key and replicates it to every
/// sibling.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnrolRequest {
    /// [`PROTOCOL_VERSION`].
    pub version: u16,
    /// The boot-only public key to certify.
    pub boot_public_key: [u8; 32],
    /// The slot the derived key was enrolled into.
    pub slot: SlotId,
    /// The device's TPM attestation key (SEC1 P-256), if it has a TPM.
    pub attestation_key: Option<Vec<u8>>,
    /// The sibling half. Wiped on drop.
    pub sibling_half: Half,
    /// Boot key signature over [`EnrolRequest::proof_digest`]: proof that the
    /// caller holds the key it asks to have certified.
    #[serde(with = "serde_bytes")]
    pub proof: [u8; 64],
}

#[derive(Serialize)]
struct EnrolProofView<'a> {
    version: u16,
    boot_public_key: &'a [u8; 32],
    slot: &'a SlotId,
    attestation_key: &'a Option<Vec<u8>>,
    half_commitment: [u8; 32],
}

impl EnrolRequest {
    /// Build a request and sign its proof with the boot key.
    ///
    /// # Errors
    ///
    /// [`BootKeyCertError::Encoding`] if the view cannot be encoded.
    pub fn new(
        boot_key: &SigningKey,
        slot: SlotId,
        attestation_key: Option<Vec<u8>>,
        sibling_half: Half,
    ) -> Result<Self, BootKeyCertError> {
        let mut request = Self {
            version: PROTOCOL_VERSION,
            boot_public_key: boot_key.verifying_key().to_bytes(),
            slot,
            attestation_key,
            sibling_half,
            proof: [0; 64],
        };
        request.proof = boot_key.sign(&request.proof_digest()?).to_bytes();
        Ok(request)
    }

    /// The digest the boot key signs. It commits to the half without
    /// exposing it.
    ///
    /// # Errors
    ///
    /// [`BootKeyCertError::Encoding`] if the view cannot be encoded.
    pub fn proof_digest(&self) -> Result<[u8; 32], BootKeyCertError> {
        let view = EnrolProofView {
            version: self.version,
            boot_public_key: &self.boot_public_key,
            slot: &self.slot,
            attestation_key: &self.attestation_key,
            half_commitment: blake3::derive_key(
                "tilo-unlock enrol half commitment v1",
                self.sibling_half.expose(),
            ),
        };
        digest(ENROL_PROOF_CONTEXT, &view).map_err(|_| BootKeyCertError::Encoding)
    }

    /// Check the version and the boot key's proof of possession.
    ///
    /// # Errors
    ///
    /// As [`BootKeyCertError`].
    pub fn verify(&self) -> Result<(), BootKeyCertError> {
        if self.version != PROTOCOL_VERSION {
            return Err(BootKeyCertError::UnsupportedVersion);
        }
        let key = VerifyingKey::from_bytes(&self.boot_public_key)
            .map_err(|_| BootKeyCertError::InvalidKey)?;
        key.verify_strict(&self.proof_digest()?, &Signature::from_bytes(&self.proof))
            .map_err(|_| BootKeyCertError::BadSignature)
    }
}

/// A sibling early boot can ask, as recorded at enrolment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SiblingContact {
    /// The sibling's fleet key; it answers challenges as this device.
    pub device_public_key: [u8; 32],
    /// Its label in the fleet, for the boot screen.
    pub label: String,
    /// Its peer-network address (a serialized endpoint address), opaque to
    /// this protocol. Empty when the sibling has not published one; early
    /// boot then finds it by `device_public_key` through the relay.
    pub endpoint: String,
}

/// The service's answer to an [`EnrolRequest`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnrolResponse {
    /// The certificate early boot presents in [`crate::UnlockHello`].
    pub cert: BootKeyCert,
    /// The person's devices that hold the sibling half.
    pub siblings: Vec<SiblingContact>,
}

/// The service's answer on the enrolment socket.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EnrolReply {
    /// Enrolled; the half now replicates to the siblings listed.
    Enrolled(Box<EnrolResponse>),
    /// Not enrolled, with a reason for the person.
    Refused(String),
}
