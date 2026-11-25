//! The enrolment: a boot key asks the person's root to certify it.
//!
//! The request travels to a device that keeps the root as the `certifyKey`
//! device action, through the same ask every action only that root signs
//! takes: the person answers it there. The sibling half never leaves the
//! enrolling device in the clear: the root answers the certificate and the
//! person's secret-sync public key, and the enrolling device seals the half
//! to it itself.

use ed25519_dalek::{Signature, Signer as _, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{PROTOCOL_VERSION, SlotId, digest};

const ENROL_PROOF_CONTEXT: &str = "tilo-unlock enrol proof v2";

/// A boot key's request to be certified by the person's root, as `certifyKey`
/// carries it: the key, the slot it unlocks, and the boot key's proof that the
/// caller has it.
///
/// The caller generated the boot key pair and both halves and enrolled the
/// derived key into `slot`; the sibling half stays with it until it seals the
/// half to the person's secret-sync key the certificate's answer names.
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
    /// own boot chain to be accepted, and the person's root signs that
    /// acceptance with the certificate.
    pub unattended_chain: Option<[u8; 32]>,
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
    ) -> Result<Self, RequestError> {
        let mut request = Self {
            version: PROTOCOL_VERSION,
            boot_public_key: boot_key.verifying_key().to_bytes(),
            slot,
            attestation_key,
            unattended_chain,
            proof: [0; 64],
        };
        request.proof = boot_key.sign(&request.proof_digest()?).to_bytes();
        Ok(request)
    }

    /// The digest the boot key signs.
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

/// What an enrolled boot key keeps: the root's certificate, and the person's
/// devices its sealed half is published to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnrolResponse {
    /// The root's certificate for the boot key, with no capabilities: what
    /// early boot presents in [`crate::UnlockHello`].
    pub cert: tilo_user_identity::DeviceCert,
    /// The person's devices that hold the sibling half.
    pub siblings: Vec<SiblingContact>,
}
