//! Signed, two-party workspace ownership transfer protocol.

use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    CanonicalError, Ed25519PublicKey, Signature, VerifyError, canonical_bytes, sign_event,
    verify_event,
};

pub const OWNERSHIP_TRANSFER_VERSION: u16 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnershipTransferProposalV1 {
    pub version: u16,
    pub workspace: String,
    pub proposal_id: [u8; 32],
    pub current_owner: Ed25519PublicKey,
    pub recipient: Ed25519PublicKey,
    pub expires_unix: u64,
    pub signature: Signature,
}

impl OwnershipTransferProposalV1 {
    /// Create and sign an exact-recipient proposal.
    ///
    /// # Errors
    ///
    /// Returns an error if canonical encoding fails.
    pub fn new(
        workspace: String,
        proposal_id: [u8; 32],
        current_owner: Ed25519PublicKey,
        recipient: Ed25519PublicKey,
        expires_unix: u64,
        owner_key: &SigningKey,
    ) -> Result<Self, CanonicalError> {
        let mut proposal = Self {
            version: OWNERSHIP_TRANSFER_VERSION,
            workspace,
            proposal_id,
            current_owner,
            recipient,
            expires_unix,
            signature: Signature::from_bytes([0; 64]),
        };
        proposal.signature = sign_event(&proposal.signing_bytes()?, owner_key)?;
        Ok(proposal)
    }

    /// Canonical bytes covered by the owner's signature.
    ///
    /// # Errors
    ///
    /// Returns an error if canonical encoding fails.
    pub fn signing_bytes(&self) -> Result<Vec<u8>, CanonicalError> {
        canonical_bytes(&(
            b"tilo.ownership-transfer.proposal.v1".as_slice(),
            self.version,
            &self.workspace,
            self.proposal_id,
            self.current_owner,
            self.recipient,
            self.expires_unix,
        ))
    }

    /// Digest binding acceptance and cancellation to this complete proposal.
    ///
    /// # Errors
    ///
    /// Returns an error if canonical encoding fails.
    pub fn digest(&self) -> Result<[u8; 32], CanonicalError> {
        let bytes = canonical_bytes(self)?;
        Ok(Sha256::digest(bytes).into())
    }

    /// Verify signer, version, distinct recipient, and expiry.
    ///
    /// # Errors
    ///
    /// Returns the precise malformed, expired, or signature failure.
    pub fn verify(&self, now_unix: u64) -> Result<(), OwnershipTransferError> {
        if self.version != OWNERSHIP_TRANSFER_VERSION {
            return Err(OwnershipTransferError::UnsupportedVersion);
        }
        if self.current_owner == self.recipient {
            return Err(OwnershipTransferError::SameOwner);
        }
        if now_unix >= self.expires_unix {
            return Err(OwnershipTransferError::Expired);
        }
        verify_event(&self.signing_bytes()?, &self.signature, &self.current_owner)?;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnershipTransferAcceptanceV1 {
    pub version: u16,
    pub proposal_digest: [u8; 32],
    pub recipient: Ed25519PublicKey,
    pub accepted_unix: u64,
    pub signature: Signature,
}

impl OwnershipTransferAcceptanceV1 {
    /// Sign acceptance of one exact proposal as its named recipient.
    ///
    /// # Errors
    ///
    /// Returns an error if canonical encoding fails.
    pub fn new(
        proposal: &OwnershipTransferProposalV1,
        accepted_unix: u64,
        recipient_key: &SigningKey,
    ) -> Result<Self, CanonicalError> {
        let mut acceptance = Self {
            version: OWNERSHIP_TRANSFER_VERSION,
            proposal_digest: proposal.digest()?,
            recipient: proposal.recipient,
            accepted_unix,
            signature: Signature::from_bytes([0; 64]),
        };
        acceptance.signature = sign_event(&acceptance.signing_bytes()?, recipient_key)?;
        Ok(acceptance)
    }

    /// Canonical bytes covered by the recipient's signature.
    ///
    /// # Errors
    ///
    /// Returns an error if canonical encoding fails.
    pub fn signing_bytes(&self) -> Result<Vec<u8>, CanonicalError> {
        canonical_bytes(&(
            b"tilo.ownership-transfer.acceptance.v1".as_slice(),
            self.version,
            self.proposal_digest,
            self.recipient,
            self.accepted_unix,
        ))
    }

    /// Verify exact proposal, recipient, time bounds, and signature.
    ///
    /// # Errors
    ///
    /// Returns the precise proposal, expiry, encoding, or signature failure.
    pub fn verify(
        &self,
        proposal: &OwnershipTransferProposalV1,
        now_unix: u64,
    ) -> Result<(), OwnershipTransferError> {
        proposal.verify(now_unix)?;
        if self.version != OWNERSHIP_TRANSFER_VERSION {
            return Err(OwnershipTransferError::UnsupportedVersion);
        }
        if self.proposal_digest != proposal.digest()? || self.recipient != proposal.recipient {
            return Err(OwnershipTransferError::WrongProposalOrRecipient);
        }
        if self.accepted_unix > now_unix || self.accepted_unix >= proposal.expires_unix {
            return Err(OwnershipTransferError::Expired);
        }
        verify_event(&self.signing_bytes()?, &self.signature, &self.recipient)?;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnershipTransferCancellationV1 {
    pub version: u16,
    pub proposal_digest: [u8; 32],
    pub current_owner: Ed25519PublicKey,
    pub cancelled_unix: u64,
    pub signature: Signature,
}

impl OwnershipTransferCancellationV1 {
    /// Sign cancellation of one exact proposal as its current owner.
    ///
    /// # Errors
    ///
    /// Returns an error if canonical encoding fails.
    pub fn new(
        proposal: &OwnershipTransferProposalV1,
        cancelled_unix: u64,
        owner_key: &SigningKey,
    ) -> Result<Self, CanonicalError> {
        let mut cancellation = Self {
            version: OWNERSHIP_TRANSFER_VERSION,
            proposal_digest: proposal.digest()?,
            current_owner: proposal.current_owner,
            cancelled_unix,
            signature: Signature::from_bytes([0; 64]),
        };
        cancellation.signature = sign_event(&cancellation.signing_bytes()?, owner_key)?;
        Ok(cancellation)
    }

    /// Canonical bytes covered by the owner's cancellation signature.
    ///
    /// # Errors
    ///
    /// Returns an error if canonical encoding fails.
    pub fn signing_bytes(&self) -> Result<Vec<u8>, CanonicalError> {
        canonical_bytes(&(
            b"tilo.ownership-transfer.cancellation.v1".as_slice(),
            self.version,
            self.proposal_digest,
            self.current_owner,
            self.cancelled_unix,
        ))
    }

    /// Verify the cancellation against the exact pending proposal.
    ///
    /// # Errors
    ///
    /// Returns an error for mismatched records, encoding, or signature.
    pub fn verify(
        &self,
        proposal: &OwnershipTransferProposalV1,
    ) -> Result<(), OwnershipTransferError> {
        if self.version != OWNERSHIP_TRANSFER_VERSION
            || self.current_owner != proposal.current_owner
            || self.proposal_digest != proposal.digest()?
        {
            return Err(OwnershipTransferError::WrongProposalOrRecipient);
        }
        verify_event(&self.signing_bytes()?, &self.signature, &self.current_owner)?;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnershipTransferAuditV1 {
    pub version: u16,
    pub proposal: OwnershipTransferProposalV1,
    pub acceptance: OwnershipTransferAcceptanceV1,
    pub committed_unix: u64,
}

impl OwnershipTransferAuditV1 {
    /// Assemble the completed audit after the exact recipient accepts.
    /// The owner's proposal signature is an explicit authorization for this
    /// automatic commit before expiry; the recipient signature binds the
    /// resulting commit to the same proposal.
    ///
    /// # Errors
    ///
    /// Returns an error if either signed record fails verification.
    pub fn new(
        proposal: OwnershipTransferProposalV1,
        acceptance: OwnershipTransferAcceptanceV1,
        committed_unix: u64,
    ) -> Result<Self, OwnershipTransferError> {
        let audit = Self {
            version: OWNERSHIP_TRANSFER_VERSION,
            proposal,
            acceptance,
            committed_unix,
        };
        audit.verify()?;
        Ok(audit)
    }

    /// Canonical bytes retained as the immutable governance audit record.
    ///
    /// # Errors
    ///
    /// Returns an error if canonical encoding fails.
    pub fn signing_bytes(&self) -> Result<Vec<u8>, CanonicalError> {
        canonical_bytes(&(
            b"tilo.ownership-transfer.commit.v1".as_slice(),
            self.version,
            &self.proposal,
            &self.acceptance,
            self.committed_unix,
        ))
    }

    /// Verify the outgoing owner's exact-recipient authorization and the
    /// incoming owner's acceptance. Their successful verification is the
    /// commit rule; cancellation and acceptance ordering is resolved by the
    /// durable workspace state transaction.
    ///
    /// # Errors
    ///
    /// Returns the precise acceptance, encoding, or signature failure.
    pub fn verify(&self) -> Result<(), OwnershipTransferError> {
        if self.version != OWNERSHIP_TRANSFER_VERSION {
            return Err(OwnershipTransferError::UnsupportedVersion);
        }
        self.acceptance
            .verify(&self.proposal, self.committed_unix)?;
        Ok(())
    }
}

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum OwnershipTransferError {
    #[error("unsupported ownership-transfer version")]
    UnsupportedVersion,
    #[error("ownership transfer cannot target the current owner")]
    SameOwner,
    #[error("ownership-transfer proposal expired")]
    Expired,
    #[error("acceptance or cancellation does not match the exact proposal and recipient")]
    WrongProposalOrRecipient,
    #[error(transparent)]
    Canonical(#[from] CanonicalError),
    #[error(transparent)]
    Verify(#[from] VerifyError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generate_keypair;

    /// Owner authorisation plus exact recipient acceptance commits; another recipient, expiry and
    /// tampering fail closed.
    #[test]
    fn ownership_transfer_commits_only_for_the_exact_recipient() {
        // Phase: owner_authorization_and_exact_recipient_acceptance_commit.
        {
            let (owner_key, owner) = generate_keypair();
            let (recipient_key, recipient) = generate_keypair();
            let proposal = OwnershipTransferProposalV1::new(
                "workspace".into(),
                [3; 32],
                owner,
                recipient,
                20,
                &owner_key,
            )
            .unwrap();
            let acceptance =
                OwnershipTransferAcceptanceV1::new(&proposal, 10, &recipient_key).unwrap();
            let audit = OwnershipTransferAuditV1::new(proposal, acceptance, 11).unwrap();
            audit.verify().unwrap();
        }

        // Phase: other_recipient_expiry_and_tamper_fail_closed.
        {
            let (owner_key, owner) = generate_keypair();
            let (recipient_key, recipient) = generate_keypair();
            let (attacker_key, _) = generate_keypair();
            let proposal = OwnershipTransferProposalV1::new(
                "workspace".into(),
                [4; 32],
                owner,
                recipient,
                20,
                &owner_key,
            )
            .unwrap();
            assert!(proposal.verify(20).is_err());
            let forged = OwnershipTransferAcceptanceV1::new(&proposal, 10, &attacker_key).unwrap();
            assert!(forged.verify(&proposal, 11).is_err());
            let mut accepted =
                OwnershipTransferAcceptanceV1::new(&proposal, 10, &recipient_key).unwrap();
            accepted.proposal_digest[0] ^= 1;
            assert!(accepted.verify(&proposal, 11).is_err());
            let acceptance =
                OwnershipTransferAcceptanceV1::new(&proposal, 10, &recipient_key).unwrap();
            let mut audit = OwnershipTransferAuditV1::new(proposal, acceptance, 11).unwrap();
            audit.acceptance.signature = Signature::from_bytes([0; 64]);
            assert!(audit.verify().is_err());
        }
    }
}
