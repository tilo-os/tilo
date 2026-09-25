//! Versioned, signed clear metadata around opaque MLS governance bytes.
//!
//! Relays validate this frame and target its opaque Welcome/commit payloads;
//! they never receive group keys or decrypt application data.

use std::collections::BTreeSet;

use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use thiserror::Error;

use crate::{
    CanonicalError, ChannelId, Ed25519PublicKey, Signature, VerifyError, canonical_bytes,
    sign_event, verify_event,
};

/// Initial governance wire format. Unknown versions must be rejected.
pub const MLS_GOVERNANCE_VERSION: u16 = 1;
/// Prevent a signed frame from becoming an unbounded allocation vector.
pub const MAX_MLS_CONTROL_BYTES: usize = 16 * 1024 * 1024;

/// Opaque control material plus the identity it must affect.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum MlsGovernanceAction {
    /// A Welcome is targeted only to `recipient`; `existing_member_commit`
    /// is fanned out to pre-existing endpoints other than the issuer.
    Add {
        recipient: Ed25519PublicKey,
        welcome: Vec<u8>,
        existing_member_commit: Option<Vec<u8>>,
    },
    /// A removal commit is delivered to every pre-change endpoint,
    /// including the removed endpoint, except the issuer that already merged.
    Remove {
        removed: Ed25519PublicKey,
        commit: Vec<u8>,
    },
    /// Permanently closes the plaintext migration gate after the encrypted
    /// snapshot named by this digest has a durable broker acknowledgement.
    EnableEncryption { encrypted_snapshot_digest: [u8; 32] },
}

/// Signed, replay-linked governance frame.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MlsGovernanceFrameV1 {
    pub version: u16,
    /// Authority-issued idempotency key, unique within this channel.
    pub transaction_id: [u8; 32],
    pub channel: ChannelId,
    /// Revision of the authoritative channel-membership record this change
    /// was checked against.
    pub membership_revision: u64,
    /// Hash of the complete preceding frame, or all-zero for the first.
    pub previous_frame_digest: [u8; 32],
    /// MLS epoch after the represented commit (or current epoch for enable).
    pub epoch: u64,
    pub action: MlsGovernanceAction,
    pub signed_by: Ed25519PublicKey,
    pub signature: Signature,
}

#[derive(Serialize)]
struct SigningView<'a> {
    domain: &'static [u8],
    version: u16,
    transaction_id: [u8; 32],
    channel: ChannelId,
    membership_revision: u64,
    previous_frame_digest: [u8; 32],
    epoch: u64,
    action: &'a MlsGovernanceAction,
    signed_by: Ed25519PublicKey,
}

#[derive(Clone, Copy)]
enum SigningDomainV1 {
    CurrentTilo,
    LegacyQos,
}

impl SigningDomainV1 {
    const fn bytes(self) -> &'static [u8] {
        match self {
            Self::CurrentTilo => b"tilo.mls-governance.v1",
            Self::LegacyQos => b"qos.mls-governance.v1",
        }
    }
}

/// Live authoritative membership inputs required to accept a frame.
pub struct MlsGovernanceContext<'a> {
    pub channel: ChannelId,
    pub membership_revision: u64,
    pub previous_frame_digest: [u8; 32],
    pub expected_epoch: u64,
    pub governors: &'a BTreeSet<Ed25519PublicKey>,
    /// Membership after the authoritative workspace/channel mutation.
    pub desired_members: &'a BTreeSet<Ed25519PublicKey>,
    /// Endpoints present in MLS before this frame.
    pub current_mls_members: &'a BTreeSet<Ed25519PublicKey>,
    /// Set only after the broker durably stores the encrypted snapshot.
    pub durable_snapshot_ack: Option<[u8; 32]>,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum MlsGovernanceError {
    #[error("unsupported MLS governance version {0}")]
    Version(u16),
    #[error("governance frame does not match the live channel/revision/epoch/head")]
    StaleOrMisrouted,
    #[error("governance signer is not currently authorised to govern")]
    Unauthorized,
    #[error("governance frame signature is invalid: {0}")]
    Signature(String),
    #[error("governance action contradicts authoritative membership")]
    MembershipMismatch,
    #[error("required Welcome or commit is missing or oversized")]
    InvalidControlPayload,
    #[error("encrypted snapshot lacks the matching durable acknowledgement")]
    SnapshotNotDurable,
    #[error("canonical encoding failed: {0}")]
    Canonical(String),
}

impl MlsGovernanceFrameV1 {
    /// Create and sign a V1 frame.
    ///
    /// # Errors
    ///
    /// Returns [`CanonicalError`] if the signing view cannot be encoded.
    pub fn signed(
        channel: ChannelId,
        transaction_id: [u8; 32],
        membership_revision: u64,
        previous_frame_digest: [u8; 32],
        epoch: u64,
        action: MlsGovernanceAction,
        signer: &SigningKey,
    ) -> Result<Self, CanonicalError> {
        let signed_by = Ed25519PublicKey::from_bytes(signer.verifying_key().to_bytes());
        let mut frame = Self {
            version: MLS_GOVERNANCE_VERSION,
            transaction_id,
            channel,
            membership_revision,
            previous_frame_digest,
            epoch,
            action,
            signed_by,
            signature: Signature::from_bytes([0; 64]),
        };
        frame.signature = sign_event(&frame.signing_view(SigningDomainV1::CurrentTilo), signer)?;
        Ok(frame)
    }

    fn signing_view(&self, domain: SigningDomainV1) -> SigningView<'_> {
        SigningView {
            domain: domain.bytes(),
            version: self.version,
            transaction_id: self.transaction_id,
            channel: self.channel,
            membership_revision: self.membership_revision,
            previous_frame_digest: self.previous_frame_digest,
            epoch: self.epoch,
            action: &self.action,
            signed_by: self.signed_by,
        }
    }

    /// SHA-256 link value covering the signature as well as all metadata.
    ///
    /// # Errors
    ///
    /// Returns [`CanonicalError`] if the complete frame cannot be encoded.
    pub fn digest(&self) -> Result<[u8; 32], CanonicalError> {
        Ok(Sha256::digest(canonical_bytes(self)?).into())
    }

    /// Verify the frame's detached signature without applying live policy.
    ///
    /// # Errors
    ///
    /// Returns [`VerifyError`] for malformed keys, encoding failures, or a
    /// signature mismatch.
    pub fn verify_signature(&self) -> Result<(), VerifyError> {
        self.verify_signature_for_domain(SigningDomainV1::CurrentTilo)
    }

    /// Verify a frame while reopening a persisted governance journal.
    ///
    /// This compatibility boundary accepts only the current Tilo V1 signing
    /// domain and the original qOS V1 domain. Live validation remains bound
    /// to [`Self::verify_signature`] and therefore accepts current Tilo
    /// signatures only.
    ///
    /// # Errors
    ///
    /// Returns [`MlsGovernanceError::Version`] for an unsupported embedded
    /// frame version, or [`MlsGovernanceError::Signature`] for malformed
    /// keys, encoding failures, or a signature that matches neither known V1
    /// journal domain.
    pub fn verify_journal_signature(&self) -> Result<(), MlsGovernanceError> {
        if self.version != MLS_GOVERNANCE_VERSION {
            return Err(MlsGovernanceError::Version(self.version));
        }
        let verification = match self.verify_signature() {
            Err(VerifyError::BadSignature) => {
                self.verify_signature_for_domain(SigningDomainV1::LegacyQos)
            }
            result => result,
        };
        verification.map_err(|error| MlsGovernanceError::Signature(error.to_string()))
    }

    fn verify_signature_for_domain(&self, domain: SigningDomainV1) -> Result<(), VerifyError> {
        verify_event(&self.signing_view(domain), &self.signature, &self.signed_by)
    }

    /// Validate signature, replay link, live revision/capability, membership
    /// transition, payload bounds, and migration acknowledgement.
    ///
    /// # Errors
    ///
    /// Returns [`MlsGovernanceError`] when any cryptographic or live-policy
    /// precondition fails.
    pub fn validate(&self, context: &MlsGovernanceContext<'_>) -> Result<(), MlsGovernanceError> {
        if self.version != MLS_GOVERNANCE_VERSION {
            return Err(MlsGovernanceError::Version(self.version));
        }
        let route_matches = self.channel == context.channel;
        let revision_matches = self.membership_revision == context.membership_revision;
        let head_matches = self.previous_frame_digest == context.previous_frame_digest;
        let epoch_matches = self.epoch == context.expected_epoch;
        if !(route_matches && revision_matches && head_matches && epoch_matches) {
            return Err(MlsGovernanceError::StaleOrMisrouted);
        }
        if !context.governors.contains(&self.signed_by) {
            return Err(MlsGovernanceError::Unauthorized);
        }
        self.verify_signature()
            .map_err(|error: VerifyError| MlsGovernanceError::Signature(error.to_string()))?;
        match &self.action {
            MlsGovernanceAction::Add {
                recipient,
                welcome,
                existing_member_commit,
            } => {
                if !context.desired_members.contains(recipient)
                    || context.current_mls_members.contains(recipient)
                {
                    return Err(MlsGovernanceError::MembershipMismatch);
                }
                let needs_commit = context.current_mls_members.len() > 1;
                if welcome.is_empty()
                    || welcome.len() > MAX_MLS_CONTROL_BYTES
                    || needs_commit != existing_member_commit.is_some()
                    || existing_member_commit
                        .as_ref()
                        .is_some_and(|v| v.is_empty() || v.len() > MAX_MLS_CONTROL_BYTES)
                {
                    return Err(MlsGovernanceError::InvalidControlPayload);
                }
            }
            MlsGovernanceAction::Remove { removed, commit } => {
                if !context.current_mls_members.contains(removed)
                    || context.desired_members.contains(removed)
                {
                    return Err(MlsGovernanceError::MembershipMismatch);
                }
                if commit.is_empty() || commit.len() > MAX_MLS_CONTROL_BYTES {
                    return Err(MlsGovernanceError::InvalidControlPayload);
                }
            }
            MlsGovernanceAction::EnableEncryption {
                encrypted_snapshot_digest,
            } => {
                if context.current_mls_members != context.desired_members {
                    return Err(MlsGovernanceError::MembershipMismatch);
                }
                if context.durable_snapshot_ack != Some(*encrypted_snapshot_digest) {
                    return Err(MlsGovernanceError::SnapshotNotDurable);
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generate_keypair;

    #[test]
    fn journal_verification_rejects_correctly_signed_non_v1_frame() {
        let (signer, _) = generate_keypair();
        let mut frame = MlsGovernanceFrameV1::signed(
            ChannelId::new(),
            [0x11; 32],
            1,
            [0; 32],
            0,
            MlsGovernanceAction::EnableEncryption {
                encrypted_snapshot_digest: [0x22; 32],
            },
            &signer,
        )
        .unwrap();
        frame.version = MLS_GOVERNANCE_VERSION + 1;
        frame.signature =
            sign_event(&frame.signing_view(SigningDomainV1::CurrentTilo), &signer).unwrap();

        assert_eq!(
            frame.verify_journal_signature(),
            Err(MlsGovernanceError::Version(MLS_GOVERNANCE_VERSION + 1))
        );
    }
}
