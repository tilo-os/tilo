//! Signed, two-party proposal to bring a whole document into a room.
//!
//! A member of a governed room proposes that a document, with its whole
//! history, become a new record there. Its device signs the proposal; the
//! room's owner accepts or declines it with a signed decision. The room's
//! governor creates the record only for an accepted proposal, and only
//! with the rights an empty creation needs, so the pair adds consent to
//! record creation and never widens it.

use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    CanonicalError, Ed25519PublicKey, Signature, VerifyError, canonical_bytes, sign_event,
    verify_event,
};

pub const DOCUMENT_IMPORT_VERSION: u16 = 1;
/// The largest document a proposal can carry, in saved bytes.
pub const MAX_IMPORT_DOCUMENT_BYTES: u64 = 160 * 1024;
/// How long an owner has to answer, at most.
pub const MAX_IMPORT_LIFETIME_SECS: u64 = 60 * 60;
const MAX_APP_BYTES: usize = 128;
const MAX_RECORD_BYTES: usize = 1024;
const MAX_TITLE_BYTES: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentImportProposalV1 {
    pub version: u16,
    /// The room the document would join.
    pub room: String,
    pub proposal_id: [u8; 32],
    /// The proposing device's key.
    pub proposer: Ed25519PublicKey,
    pub app: String,
    /// The record id the document would have there.
    pub record: String,
    /// What the person moving it calls it, for the owner's sheet.
    pub title: String,
    /// SHA-256 of the saved document the proposal carries.
    pub document_digest: [u8; 32],
    pub document_len: u64,
    pub issued_unix: u64,
    pub expires_unix: u64,
    pub signature: Signature,
}

/// What a proposal names, before it is signed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentImportTerms {
    pub room: String,
    pub proposal_id: [u8; 32],
    pub app: String,
    pub record: String,
    pub title: String,
    pub document: Vec<u8>,
    pub issued_unix: u64,
    pub expires_unix: u64,
}

impl DocumentImportProposalV1 {
    pub fn new(
        terms: &DocumentImportTerms,
        device_key: &SigningKey,
    ) -> Result<Self, DocumentImportError> {
        let mut proposal = Self {
            version: DOCUMENT_IMPORT_VERSION,
            room: terms.room.clone(),
            proposal_id: terms.proposal_id,
            proposer: Ed25519PublicKey::from_bytes(device_key.verifying_key().to_bytes()),
            app: terms.app.clone(),
            record: terms.record.clone(),
            title: terms.title.clone(),
            document_digest: Sha256::digest(&terms.document).into(),
            document_len: u64::try_from(terms.document.len())
                .map_err(|_| DocumentImportError::OutOfBounds)?,
            issued_unix: terms.issued_unix,
            expires_unix: terms.expires_unix,
            signature: Signature::from_bytes([0; 64]),
        };
        proposal.check_bounds()?;
        proposal.signature = sign_event(&proposal.signing_bytes()?, device_key)?;
        Ok(proposal)
    }

    /// Canonical bytes covered by the proposer's signature.
    pub fn signing_bytes(&self) -> Result<Vec<u8>, CanonicalError> {
        canonical_bytes(&(
            b"tilo.document-import.proposal.v1".as_slice(),
            self.version,
            &self.room,
            self.proposal_id,
            self.proposer,
            &self.app,
            &self.record,
            &self.title,
            self.document_digest,
            self.document_len,
            self.issued_unix,
            self.expires_unix,
        ))
    }

    /// Digest binding a decision to this complete proposal.
    pub fn digest(&self) -> Result<[u8; 32], CanonicalError> {
        Ok(Sha256::digest(canonical_bytes(self)?).into())
    }

    fn check_bounds(&self) -> Result<(), DocumentImportError> {
        if self.version != DOCUMENT_IMPORT_VERSION {
            return Err(DocumentImportError::UnsupportedVersion);
        }
        let label = |value: &str, max: usize| {
            !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
        };
        if !label(&self.room, 64)
            || !label(&self.app, MAX_APP_BYTES)
            || !label(&self.record, MAX_RECORD_BYTES)
            || self.title.len() > MAX_TITLE_BYTES
            || self.title.chars().any(char::is_control)
            || self.document_len == 0
            || self.document_len > MAX_IMPORT_DOCUMENT_BYTES
            || self
                .expires_unix
                .checked_sub(self.issued_unix)
                .is_none_or(|lifetime| lifetime == 0 || lifetime > MAX_IMPORT_LIFETIME_SECS)
        {
            return Err(DocumentImportError::OutOfBounds);
        }
        Ok(())
    }

    /// Verify version, bounds, expiry and the proposer's signature.
    pub fn verify(&self, now_unix: u64) -> Result<(), DocumentImportError> {
        self.check_bounds()?;
        if now_unix >= self.expires_unix {
            return Err(DocumentImportError::Expired);
        }
        verify_event(&self.signing_bytes()?, &self.signature, &self.proposer)?;
        Ok(())
    }

    /// Whether `document` is exactly the document this proposal names.
    #[must_use]
    pub fn carries(&self, document: &[u8]) -> bool {
        u64::try_from(document.len()).ok() == Some(self.document_len)
            && <[u8; 32]>::from(Sha256::digest(document)) == self.document_digest
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentImportDecisionV1 {
    pub version: u16,
    pub proposal_digest: [u8; 32],
    /// The deciding device's key.
    pub decider: Ed25519PublicKey,
    pub accepted: bool,
    pub decided_unix: u64,
    pub signature: Signature,
}

impl DocumentImportDecisionV1 {
    /// Sign a decision on one exact proposal.
    pub fn new(
        proposal: &DocumentImportProposalV1,
        accepted: bool,
        decided_unix: u64,
        device_key: &SigningKey,
    ) -> Result<Self, DocumentImportError> {
        let mut decision = Self {
            version: DOCUMENT_IMPORT_VERSION,
            proposal_digest: proposal.digest()?,
            decider: Ed25519PublicKey::from_bytes(device_key.verifying_key().to_bytes()),
            accepted,
            decided_unix,
            signature: Signature::from_bytes([0; 64]),
        };
        decision.signature = sign_event(&decision.signing_bytes()?, device_key)?;
        Ok(decision)
    }

    /// Canonical bytes covered by the decider's signature.
    pub fn signing_bytes(&self) -> Result<Vec<u8>, CanonicalError> {
        canonical_bytes(&(
            b"tilo.document-import.decision.v1".as_slice(),
            self.version,
            self.proposal_digest,
            self.decider,
            self.accepted,
            self.decided_unix,
        ))
    }

    /// Verify that this decision answers exactly `proposal` and is signed
    /// by its decider. An acceptance must come before the proposal expires;
    /// a decline may come at any time. Whether the decider may decide is
    /// the room's to say.
    pub fn verify(
        &self,
        proposal: &DocumentImportProposalV1,
        now_unix: u64,
    ) -> Result<(), DocumentImportError> {
        if self.version != DOCUMENT_IMPORT_VERSION {
            return Err(DocumentImportError::UnsupportedVersion);
        }
        if self.proposal_digest != proposal.digest()? {
            return Err(DocumentImportError::WrongProposal);
        }
        if self.accepted {
            proposal.verify(now_unix)?;
            if self.decided_unix > now_unix || self.decided_unix >= proposal.expires_unix {
                return Err(DocumentImportError::Expired);
            }
        } else {
            proposal.check_bounds()?;
            verify_event(
                &proposal.signing_bytes()?,
                &proposal.signature,
                &proposal.proposer,
            )?;
        }
        verify_event(&self.signing_bytes()?, &self.signature, &self.decider)?;
        Ok(())
    }
}

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum DocumentImportError {
    #[error("unsupported document-import version")]
    UnsupportedVersion,
    #[error("document-import proposal is out of bounds")]
    OutOfBounds,
    #[error("document-import proposal expired")]
    Expired,
    #[error("decision does not answer this exact proposal")]
    WrongProposal,
    #[error(transparent)]
    Canonical(#[from] CanonicalError),
    #[error(transparent)]
    Verify(#[from] VerifyError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generate_keypair;

    fn terms(document: &[u8]) -> DocumentImportTerms {
        DocumentImportTerms {
            room: "3f2b1c00-0000-4000-8000-00000000000b".into(),
            proposal_id: [5; 32],
            app: "so.tilo.notes".into(),
            record: "uuid:0f2b1c00-0000-4000-8000-000000000001".into(),
            title: "Groceries".into(),
            document: document.to_vec(),
            issued_unix: 10,
            expires_unix: 100,
        }
    }

    #[test]
    fn a_decision_answers_one_exact_signed_proposal() {
        let (member_key, _) = generate_keypair();
        let (owner_key, _) = generate_keypair();
        let proposal = DocumentImportProposalV1::new(&terms(b"doc"), &member_key).unwrap();
        proposal.verify(50).unwrap();
        assert!(proposal.carries(b"doc"));
        assert!(!proposal.carries(b"dog"));
        let accepted = DocumentImportDecisionV1::new(&proposal, true, 20, &owner_key).unwrap();
        accepted.verify(&proposal, 30).unwrap();
        let declined = DocumentImportDecisionV1::new(&proposal, false, 200, &owner_key).unwrap();
        declined.verify(&proposal, 300).unwrap();

        // An acceptance after expiry, for another proposal, or tampered fails.
        assert_eq!(
            accepted.verify(&proposal, 100),
            Err(DocumentImportError::Expired)
        );
        let late = DocumentImportDecisionV1::new(&proposal, true, 100, &owner_key).unwrap();
        assert!(late.verify(&proposal, 99).is_err());
        let mut other = proposal.clone();
        other.record = "uuid:0f2b1c00-0000-4000-8000-000000000002".into();
        assert!(accepted.verify(&other, 30).is_err());
        let mut forged = accepted;
        forged.accepted = false;
        assert!(forged.verify(&proposal, 30).is_err());
        let mut tampered = proposal;
        tampered.title = "Something else".into();
        assert!(tampered.verify(50).is_err());
    }

    #[test]
    fn a_proposal_stays_within_its_bounds() {
        let (key, _) = generate_keypair();
        let too_large = vec![0; usize::try_from(MAX_IMPORT_DOCUMENT_BYTES).unwrap() + 1];
        assert_eq!(
            DocumentImportProposalV1::new(&terms(&too_large), &key),
            Err(DocumentImportError::OutOfBounds)
        );
        assert!(DocumentImportProposalV1::new(&terms(b""), &key).is_err());
        let mut forever = terms(b"doc");
        forever.expires_unix = forever.issued_unix + MAX_IMPORT_LIFETIME_SECS + 1;
        assert!(DocumentImportProposalV1::new(&forever, &key).is_err());
    }
}
