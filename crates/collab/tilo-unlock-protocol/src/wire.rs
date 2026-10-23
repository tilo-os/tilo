//! Frames of the boot-time exchange and the request signature.

use ed25519_dalek::{Signature, Signer as _, SigningKey};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tilo_seal::{Label, PublicKey, SecretKey};

use crate::attest::Quote;
use crate::cert::BootKeyCert;
use crate::half::{HALF_LEN, Half};
use crate::{BootId, MAX_FRAME_BYTES, PROTOCOL_VERSION, SlotId, digest};

const REQUEST_CONTEXT: &str = "tilo-unlock request v1";
const RELEASE_VERSION: u8 = 1;

/// First frame: early boot names the enrolment it asks for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnlockHello {
    /// [`PROTOCOL_VERSION`].
    pub version: u16,
    /// The boot key certificate issued at enrolment.
    pub boot_cert: BootKeyCert,
}

/// A sibling's single-use challenge.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Challenge {
    /// [`PROTOCOL_VERSION`].
    pub version: u16,
    /// The answering sibling's fleet key; the request is bound to it.
    pub sibling_public_key: [u8; 32],
    /// Fresh random nonce, answerable once.
    pub nonce: [u8; 32],
    /// After this the sibling refuses the answer.
    pub expires_at_ns: u64,
}

/// Early boot's answer to a [`Challenge`], signed by the boot key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnlockRequest {
    /// [`PROTOCOL_VERSION`].
    pub version: u16,
    /// This boot's random id.
    pub boot_id: BootId,
    /// The slot to unlock; must equal the certificate's.
    pub slot: SlotId,
    /// X25519 public key drawn for this request alone; the half is released
    /// sealed to it.
    pub ephemeral_public: [u8; 32],
    /// The challenge nonce being answered.
    pub nonce: [u8; 32],
    /// A TPM quote over [`crate::attest::qualifying_data`], when the device
    /// has a TPM. Required for auto-approval, optional otherwise.
    pub attestation: Option<Quote>,
    /// Boot key signature over [`UnlockRequest::digest`].
    #[serde(with = "serde_bytes")]
    pub signature: [u8; 64],
}

#[derive(Serialize)]
struct RequestView<'a> {
    version: u16,
    cert: [u8; 32],
    sibling_public_key: &'a [u8; 32],
    nonce: &'a [u8; 32],
    boot_id: &'a BootId,
    slot: &'a SlotId,
    ephemeral_public: &'a [u8; 32],
    attestation: &'a Option<Quote>,
}

impl UnlockRequest {
    /// Answer `challenge` for `cert`, signing with the boot key.
    ///
    /// # Errors
    ///
    /// [`RefusalReason::Malformed`] when the request cannot be encoded.
    pub fn sign(
        boot_key: &SigningKey,
        cert: &BootKeyCert,
        challenge: &Challenge,
        boot_id: BootId,
        ephemeral_public: &PublicKey,
        attestation: Option<Quote>,
    ) -> Result<Self, RefusalReason> {
        let mut request = Self {
            version: PROTOCOL_VERSION,
            boot_id,
            slot: cert.slot,
            ephemeral_public: ephemeral_public.to_bytes(),
            nonce: challenge.nonce,
            attestation,
            signature: [0; 64],
        };
        let digest = request.digest(cert, &challenge.sibling_public_key)?;
        request.signature = boot_key.sign(&digest).to_bytes();
        Ok(request)
    }

    /// The digest the boot key signs: the certificate, the answering
    /// sibling, the challenge nonce, the boot id, the slot, the ephemeral key
    /// and the attestation. The released half is bound to it as well.
    ///
    /// # Errors
    ///
    /// [`RefusalReason::Malformed`] when the request cannot be encoded.
    pub fn digest(
        &self,
        cert: &BootKeyCert,
        sibling_public_key: &[u8; 32],
    ) -> Result<[u8; 32], RefusalReason> {
        let view = RequestView {
            version: self.version,
            cert: cert
                .signing_digest()
                .map_err(|_| RefusalReason::Malformed)?,
            sibling_public_key,
            nonce: &self.nonce,
            boot_id: &self.boot_id,
            slot: &self.slot,
            ephemeral_public: &self.ephemeral_public,
            attestation: &self.attestation,
        };
        digest(REQUEST_CONTEXT, &view).map_err(|_| RefusalReason::Malformed)
    }

    /// Check this request against the certificate (already verified by the
    /// caller) and the challenge the sibling issued on this stream, at
    /// `now_ns`. Returns the request digest.
    ///
    /// Whether the challenge was already answered, the boot id already
    /// served, or the device removed is the sibling's state to check.
    ///
    /// # Errors
    ///
    /// The [`RefusalReason`] to answer with.
    pub fn verify(
        &self,
        cert: &BootKeyCert,
        challenge: &Challenge,
        now_ns: u64,
    ) -> Result<[u8; 32], RefusalReason> {
        if self.version != PROTOCOL_VERSION {
            return Err(RefusalReason::UnsupportedVersion);
        }
        if self.slot != cert.slot {
            return Err(RefusalReason::WrongSlot);
        }
        if self.nonce != challenge.nonce {
            return Err(RefusalReason::UnknownChallenge);
        }
        if now_ns >= challenge.expires_at_ns {
            return Err(RefusalReason::ChallengeExpired);
        }
        let digest = self.digest(cert, &challenge.sibling_public_key)?;
        cert.boot_key()
            .map_err(|_| RefusalReason::BadCertificate)?
            .verify_strict(&digest, &Signature::from_bytes(&self.signature))
            .map_err(|_| RefusalReason::BadSignature)?;
        Ok(digest)
    }
}

/// The sibling half, sealed to the request's ephemeral key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Release {
    /// Format version (1).
    pub version: u8,
    /// `tilo_seal::SealedBox` wire bytes, label `UnlockRelease`, the request
    /// digest as associated data.
    #[serde(with = "serde_bytes")]
    pub sealed: Vec<u8>,
}

/// Seal `half` to the request's ephemeral key, bound to its digest.
pub fn seal_release(
    ephemeral_public: &[u8; 32],
    request_digest: &[u8; 32],
    half: &Half,
) -> Result<Release, tilo_seal::Error> {
    let sealed = tilo_seal::hpke::seal_to_bytes(
        &PublicKey::from_bytes(*ephemeral_public),
        Label::UnlockRelease,
        request_digest,
        half.expose(),
    )?;
    Ok(Release {
        version: RELEASE_VERSION,
        sealed,
    })
}

/// Open a [`Release`] with the request's ephemeral secret.
pub fn open_release(
    ephemeral_secret: &SecretKey,
    request_digest: &[u8; 32],
    release: &Release,
) -> Result<Half, tilo_seal::Error> {
    if release.version != RELEASE_VERSION {
        return Err(tilo_seal::Error::UnsupportedVersion(release.version));
    }
    let plain = tilo_seal::hpke::open_bytes(
        ephemeral_secret,
        Label::UnlockRelease,
        request_digest,
        &release.sealed,
    )?;
    let bytes = <[u8; HALF_LEN]>::try_from(plain.as_slice())
        .map_err(|_| tilo_seal::Error::Malformed("sibling half length"))?;
    Ok(Half::from_bytes(bytes))
}

/// Frames early boot sends.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientFrame {
    /// Opens the exchange.
    Hello(UnlockHello),
    /// Answers the challenge.
    Request(UnlockRequest),
}

/// Frames a sibling sends.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SiblingFrame {
    /// Answer this to go on.
    Challenge(Challenge),
    /// Waiting for the owner. The same code is shown in the approval prompt
    /// so the person can tell this boot from another.
    Pending {
        /// Six digits derived from the request digest.
        approval_code: String,
    },
    /// The owner approved (or the device is auto-approved and attested).
    Released(Release),
    /// The owner declined, or nobody decided in time.
    Denied,
    /// The sibling refused before asking anyone.
    Refused(RefusalReason),
}

/// Why a sibling refused. Every refusal is in the owner's activity history.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Error, Serialize, Deserialize)]
pub enum RefusalReason {
    /// Another protocol version.
    #[error("unsupported protocol version")]
    UnsupportedVersion,
    /// A frame did not decode or was out of order.
    #[error("malformed frame")]
    Malformed,
    /// The boot key certificate does not verify, or is not the person's.
    #[error("boot key certificate is not valid")]
    BadCertificate,
    /// No sibling half is held for this certificate.
    #[error("no enrolment for this boot key")]
    NotEnrolled,
    /// The device was removed from the person's fleet.
    #[error("device was removed from the fleet")]
    Revoked,
    /// The request names another slot than the certificate.
    #[error("wrong slot")]
    WrongSlot,
    /// The answer is not for the challenge issued on this stream, or that
    /// challenge was already answered.
    #[error("unknown or reused challenge")]
    UnknownChallenge,
    #[error("challenge expired")]
    ChallengeExpired,
    /// The boot key signature does not verify.
    #[error("bad request signature")]
    BadSignature,
    /// A half was already released to this boot.
    #[error("this boot was already unlocked")]
    BootAlreadyServed,
    /// Too many requests from this device; ask again later.
    #[error("too many unlock requests")]
    RateLimited,
    /// This sibling cannot open halves (it does not hold the person's root).
    #[error("this device cannot release halves")]
    NotACustodian,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum FrameError {
    /// Longer than [`MAX_FRAME_BYTES`].
    #[error("frame too large")]
    TooLarge,
    /// Not a frame of the expected type.
    #[error("malformed frame")]
    Malformed,
}

/// Encode one frame: a big-endian `u32` length, then the postcard body.
///
/// # Errors
///
/// [`FrameError::TooLarge`] past [`MAX_FRAME_BYTES`].
pub fn encode_frame<T: Serialize>(value: &T) -> Result<Vec<u8>, FrameError> {
    let body = postcard::to_allocvec(value).map_err(|_| FrameError::Malformed)?;
    if body.len() > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge);
    }
    let len = u32::try_from(body.len()).map_err(|_| FrameError::TooLarge)?;
    let mut out = Vec::with_capacity(body.len().saturating_add(4));
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

/// The body length announced by a frame's 4-byte prefix, checked against
/// [`MAX_FRAME_BYTES`] before anything is read.
///
/// # Errors
///
/// [`FrameError::TooLarge`] past the bound.
pub fn frame_len(prefix: [u8; 4]) -> Result<usize, FrameError> {
    let len = usize::try_from(u32::from_be_bytes(prefix)).map_err(|_| FrameError::TooLarge)?;
    if len > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge);
    }
    Ok(len)
}

/// Decode one frame body (without its length prefix).
///
/// # Errors
///
/// [`FrameError`] for an oversized or undecodable body, or trailing bytes.
pub fn decode_frame<T: DeserializeOwned>(body: &[u8]) -> Result<T, FrameError> {
    if body.len() > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge);
    }
    let (value, rest) = postcard::take_from_bytes(body).map_err(|_| FrameError::Malformed)?;
    if !rest.is_empty() {
        return Err(FrameError::Malformed);
    }
    Ok(value)
}

/// The six-digit code shown both at the boot screen and in the approval
/// prompt for one request.
#[must_use]
pub fn approval_code(request_digest: &[u8; 32]) -> String {
    let code = blake3::derive_key("tilo-unlock approval code v1", request_digest);
    let value = u32::from_le_bytes([code[0], code[1], code[2], code[3]]) % 1_000_000;
    format!("{value:06}")
}
