//! The enrolment exchange: a boot key asks the person's root to certify it.

use ed25519_dalek::{Signature, Signer as _, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::half::Half;
use crate::{PROTOCOL_VERSION, SlotId, digest};

const ENROL_PROOF_CONTEXT: &str = "tilo-unlock enrol proof v1";

/// What `tilo-disk-encrypt enrol --sibling` hands the service on the
/// enrolling device (the typed `device.unlock.enrol` action).
///
/// The caller generated the boot key pair and both halves, enrolled the
/// derived key into `slot`, and keeps only the device half afterwards. The
/// service certifies the boot key with the person's identity, seals the
/// sibling half to the person's secret-sync key and replicates it to every
/// device of theirs.
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
    /// The PCR 4 + 7 composite this device measured now, when the owner is
    /// enrolling it to unlock unattended: the enrolled device asks for its
    /// own boot chain to be accepted, and the session holding the root signs
    /// that acceptance for the person.
    pub unattended_chain: Option<[u8; 32]>,
    /// The half the other devices will hold. Wiped on drop.
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
    unattended_chain: &'a Option<[u8; 32]>,
    half_commitment: [u8; 32],
}

/// Why a boot key's request to be certified is not acceptable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum RequestError {
    /// Another protocol version.
    #[error("unsupported enrolment version")]
    UnsupportedVersion,
    /// The boot public key is not a valid Ed25519 point.
    #[error("invalid public key")]
    InvalidKey,
    /// The proof does not verify.
    #[error("enrolment proof did not verify")]
    BadSignature,
    /// The request could not be encoded.
    #[error("enrolment request could not be encoded")]
    Encoding,
}

impl EnrolRequest {
    /// Build a request and sign its proof with the boot key.
    ///
    /// # Errors
    ///
    /// [`RequestError::Encoding`] if the view cannot be encoded.
    pub fn new(
        boot_key: &SigningKey,
        slot: SlotId,
        attestation_key: Option<Vec<u8>>,
        unattended_chain: Option<[u8; 32]>,
        sibling_half: Half,
    ) -> Result<Self, RequestError> {
        let mut request = Self {
            version: PROTOCOL_VERSION,
            boot_public_key: boot_key.verifying_key().to_bytes(),
            slot,
            attestation_key,
            unattended_chain,
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
    /// [`RequestError::Encoding`] if the view cannot be encoded.
    pub fn proof_digest(&self) -> Result<[u8; 32], RequestError> {
        let view = EnrolProofView {
            version: self.version,
            boot_public_key: &self.boot_public_key,
            slot: &self.slot,
            attestation_key: &self.attestation_key,
            unattended_chain: &self.unattended_chain,
            half_commitment: blake3::derive_key(
                "tilo-unlock enrol half commitment v1",
                self.sibling_half.expose(),
            ),
        };
        digest(ENROL_PROOF_CONTEXT, &view).map_err(|_| RequestError::Encoding)
    }

    /// Check the version and the boot key's proof of possession.
    ///
    /// # Errors
    ///
    /// As [`RequestError`].
    pub fn verify(&self) -> Result<(), RequestError> {
        if self.version != PROTOCOL_VERSION {
            return Err(RequestError::UnsupportedVersion);
        }
        let key = VerifyingKey::from_bytes(&self.boot_public_key)
            .map_err(|_| RequestError::InvalidKey)?;
        key.verify_strict(&self.proof_digest()?, &Signature::from_bytes(&self.proof))
            .map_err(|_| RequestError::BadSignature)
    }
}

/// A device early boot can ask, as recorded at enrolment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SiblingContact {
    /// The device's fleet key; it answers challenges as this device.
    pub device_public_key: [u8; 32],
    /// Its label in the person's devices, for the boot screen.
    pub label: String,
    /// Its peer-network address (a serialized endpoint address), opaque to
    /// this protocol. Empty when the device has not published one; early
    /// boot then finds it by `device_public_key` through the server.
    pub endpoint: String,
}

/// The service's answer to an [`EnrolRequest`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnrolResponse {
    /// The root's certificate for the boot key, with no capabilities: what
    /// early boot presents in [`crate::UnlockHello`].
    pub cert: tilo_user_identity::DeviceCert,
    /// The person's devices that hold the sibling half.
    pub siblings: Vec<SiblingContact>,
}

/// The service's answer on the enrolment socket, and to another of the
/// person's devices that asks over [`crate::ENROL_ALPN`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EnrolReply {
    /// Enrolled; the half now replicates to the devices listed.
    Enrolled(Box<EnrolResponse>),
    /// Not enrolled, with a reason for the person.
    Refused(String),
    /// This device does not hold the person's root, so it cannot certify the
    /// key: another of their devices must. The asking device moves on to
    /// another.
    NotKeyHolder,
    /// The person refused the enrolment, or nobody answered in time.
    Denied,
}
