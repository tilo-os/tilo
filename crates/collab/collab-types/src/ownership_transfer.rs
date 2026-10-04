//! Signed, two-party workspace ownership transfer protocol.
//!
//! Each step is signed as a person by one of their devices: the device key
//! signs, and the record carries the certificate the person's root issued for
//! that key ([`DelegatedSignature`]). The root itself never has to be on the
//! device that offers, accepts or cancels. A verifier accepts a step only
//! through [`verify_delegated`]: the certificate chains to the named person,
//! is valid at the time checked, was not revoked by that person's root, and
//! grants [`Authority::OwnWorkspaces`] — handing a workspace over, cancelling
//! the offer and taking ownership all act as a workspace owner, so a device
//! certified only to host may do none of them.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tilo_user_identity::{
    Authority, ChainError, ChainPolicy, DelegatedSignature, IdentitySigner, RevocationList,
    verify_delegated,
};

use crate::{CanonicalError, Ed25519PublicKey, canonical_bytes};

pub const OWNERSHIP_TRANSFER_VERSION: u16 = 2;

const PROPOSAL_DOMAIN: &str = "tilo.ownership-transfer.proposal.v2";
const ACCEPTANCE_DOMAIN: &str = "tilo.ownership-transfer.acceptance.v2";
const CANCELLATION_DOMAIN: &str = "tilo.ownership-transfer.cancellation.v2";

const NANOS_PER_SECOND: u64 = 1_000_000_000;

/// Check `signature` over `bytes` in `domain` as `person` at `at_unix`.
fn verify_step(
    signature: &DelegatedSignature,
    domain: &str,
    bytes: &[u8],
    person: &Ed25519PublicKey,
    revocations: &RevocationList,
    at_unix: u64,
) -> Result<(), OwnershipTransferError> {
    verify_delegated(
        signature,
        domain,
        bytes,
        &ChainPolicy {
            user_public_key: person.as_bytes(),
            authority: Authority::OwnWorkspaces,
            revocations,
            now_ns: at_unix.saturating_mul(NANOS_PER_SECOND),
        },
    )?;
    Ok(())
}

/// Sign `bytes` in `domain` with `signer`, which must speak for `person`.
fn sign_step(
    signer: &dyn IdentitySigner,
    person: &Ed25519PublicKey,
    domain: &str,
    bytes: &[u8],
) -> Result<DelegatedSignature, OwnershipTransferError> {
    if &signer.user_public_key() != person.as_bytes() {
        return Err(OwnershipTransferError::WrongSigner);
    }
    Ok(signer.sign_as_person(domain, bytes)?)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnershipTransferProposalV1 {
    pub version: u16,
    pub workspace: String,
    pub proposal_id: [u8; 32],
    pub current_owner: Ed25519PublicKey,
    pub recipient: Ed25519PublicKey,
    pub expires_unix: u64,
    pub signature: DelegatedSignature,
}

impl OwnershipTransferProposalV1 {
    /// Create an exact-recipient proposal, signed as `current_owner` by
    /// `signer`.
    ///
    /// # Errors
    ///
    /// Returns an error if `signer` speaks for someone other than
    /// `current_owner`, cannot sign, or canonical encoding fails.
    pub fn new(
        workspace: String,
        proposal_id: [u8; 32],
        current_owner: Ed25519PublicKey,
        recipient: Ed25519PublicKey,
        expires_unix: u64,
        signer: &dyn IdentitySigner,
    ) -> Result<Self, OwnershipTransferError> {
        let mut proposal = Self {
            version: OWNERSHIP_TRANSFER_VERSION,
            workspace,
            proposal_id,
            current_owner,
            recipient,
            expires_unix,
            signature: placeholder_signature(),
        };
        proposal.signature = sign_step(
            signer,
            &current_owner,
            PROPOSAL_DOMAIN,
            &proposal.signing_bytes()?,
        )?;
        Ok(proposal)
    }

    /// Canonical bytes covered by the owner's signature.
    ///
    /// # Errors
    ///
    /// Returns an error if canonical encoding fails.
    pub fn signing_bytes(&self) -> Result<Vec<u8>, CanonicalError> {
        canonical_bytes(&(
            PROPOSAL_DOMAIN.as_bytes(),
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

    /// Verify version, distinct recipient, expiry, and the owner's signature
    /// at `now_unix` against the owner's `revocations`.
    ///
    /// # Errors
    ///
    /// Returns the precise malformed, expired, or signature-chain failure.
    pub fn verify(
        &self,
        now_unix: u64,
        revocations: &RevocationList,
    ) -> Result<(), OwnershipTransferError> {
        if self.version != OWNERSHIP_TRANSFER_VERSION {
            return Err(OwnershipTransferError::UnsupportedVersion);
        }
        if self.current_owner == self.recipient {
            return Err(OwnershipTransferError::SameOwner);
        }
        if now_unix >= self.expires_unix {
            return Err(OwnershipTransferError::Expired);
        }
        verify_step(
            &self.signature,
            PROPOSAL_DOMAIN,
            &self.signing_bytes()?,
            &self.current_owner,
            revocations,
            now_unix,
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnershipTransferAcceptanceV1 {
    pub version: u16,
    pub proposal_digest: [u8; 32],
    pub recipient: Ed25519PublicKey,
    pub accepted_unix: u64,
    pub signature: DelegatedSignature,
}

impl OwnershipTransferAcceptanceV1 {
    /// Accept one exact proposal, signed as its named recipient by `signer`.
    ///
    /// # Errors
    ///
    /// Returns an error if `signer` speaks for someone other than the
    /// recipient, cannot sign, or canonical encoding fails.
    pub fn new(
        proposal: &OwnershipTransferProposalV1,
        accepted_unix: u64,
        signer: &dyn IdentitySigner,
    ) -> Result<Self, OwnershipTransferError> {
        let mut acceptance = Self {
            version: OWNERSHIP_TRANSFER_VERSION,
            proposal_digest: proposal.digest()?,
            recipient: proposal.recipient,
            accepted_unix,
            signature: placeholder_signature(),
        };
        acceptance.signature = sign_step(
            signer,
            &proposal.recipient,
            ACCEPTANCE_DOMAIN,
            &acceptance.signing_bytes()?,
        )?;
        Ok(acceptance)
    }

    /// Canonical bytes covered by the recipient's signature.
    ///
    /// # Errors
    ///
    /// Returns an error if canonical encoding fails.
    pub fn signing_bytes(&self) -> Result<Vec<u8>, CanonicalError> {
        canonical_bytes(&(
            ACCEPTANCE_DOMAIN.as_bytes(),
            self.version,
            self.proposal_digest,
            self.recipient,
            self.accepted_unix,
        ))
    }

    /// Verify the exact proposal, recipient, time bounds, and both signature
    /// chains at `now_unix`.
    ///
    /// # Errors
    ///
    /// Returns the precise proposal, expiry, encoding, or signature-chain
    /// failure.
    pub fn verify(
        &self,
        proposal: &OwnershipTransferProposalV1,
        now_unix: u64,
        revocations: &RevocationList,
    ) -> Result<(), OwnershipTransferError> {
        proposal.verify(now_unix, revocations)?;
        if self.version != OWNERSHIP_TRANSFER_VERSION {
            return Err(OwnershipTransferError::UnsupportedVersion);
        }
        if self.proposal_digest != proposal.digest()? || self.recipient != proposal.recipient {
            return Err(OwnershipTransferError::WrongProposalOrRecipient);
        }
        if self.accepted_unix > now_unix || self.accepted_unix >= proposal.expires_unix {
            return Err(OwnershipTransferError::Expired);
        }
        verify_step(
            &self.signature,
            ACCEPTANCE_DOMAIN,
            &self.signing_bytes()?,
            &self.recipient,
            revocations,
            now_unix,
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnershipTransferCancellationV1 {
    pub version: u16,
    pub proposal_digest: [u8; 32],
    pub current_owner: Ed25519PublicKey,
    pub cancelled_unix: u64,
    pub signature: DelegatedSignature,
}

impl OwnershipTransferCancellationV1 {
    /// Cancel one exact proposal, signed as its current owner by `signer`.
    ///
    /// # Errors
    ///
    /// Returns an error if `signer` speaks for someone other than the
    /// current owner, cannot sign, or canonical encoding fails.
    pub fn new(
        proposal: &OwnershipTransferProposalV1,
        cancelled_unix: u64,
        signer: &dyn IdentitySigner,
    ) -> Result<Self, OwnershipTransferError> {
        let mut cancellation = Self {
            version: OWNERSHIP_TRANSFER_VERSION,
            proposal_digest: proposal.digest()?,
            current_owner: proposal.current_owner,
            cancelled_unix,
            signature: placeholder_signature(),
        };
        cancellation.signature = sign_step(
            signer,
            &proposal.current_owner,
            CANCELLATION_DOMAIN,
            &cancellation.signing_bytes()?,
        )?;
        Ok(cancellation)
    }

    /// Canonical bytes covered by the owner's cancellation signature.
    ///
    /// # Errors
    ///
    /// Returns an error if canonical encoding fails.
    pub fn signing_bytes(&self) -> Result<Vec<u8>, CanonicalError> {
        canonical_bytes(&(
            CANCELLATION_DOMAIN.as_bytes(),
            self.version,
            self.proposal_digest,
            self.current_owner,
            self.cancelled_unix,
        ))
    }

    /// Verify the cancellation against the exact pending proposal, with the
    /// owner's signature chain checked at `now_unix`.
    ///
    /// # Errors
    ///
    /// Returns an error for mismatched records, encoding, or the signature
    /// chain.
    pub fn verify(
        &self,
        proposal: &OwnershipTransferProposalV1,
        now_unix: u64,
        revocations: &RevocationList,
    ) -> Result<(), OwnershipTransferError> {
        if self.version != OWNERSHIP_TRANSFER_VERSION
            || self.current_owner != proposal.current_owner
            || self.proposal_digest != proposal.digest()?
        {
            return Err(OwnershipTransferError::WrongProposalOrRecipient);
        }
        verify_step(
            &self.signature,
            CANCELLATION_DOMAIN,
            &self.signing_bytes()?,
            &self.current_owner,
            revocations,
            now_unix,
        )
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
        revocations: &RevocationList,
    ) -> Result<Self, OwnershipTransferError> {
        let audit = Self {
            version: OWNERSHIP_TRANSFER_VERSION,
            proposal,
            acceptance,
            committed_unix,
        };
        audit.verify(revocations)?;
        Ok(audit)
    }

    /// Canonical bytes retained as the immutable governance audit record.
    ///
    /// # Errors
    ///
    /// Returns an error if canonical encoding fails.
    pub fn signing_bytes(&self) -> Result<Vec<u8>, CanonicalError> {
        canonical_bytes(&(
            b"tilo.ownership-transfer.commit.v2".as_slice(),
            self.version,
            &self.proposal,
            &self.acceptance,
            self.committed_unix,
        ))
    }

    /// Verify the outgoing owner's exact-recipient authorization and the
    /// incoming owner's acceptance, both chains as of `committed_unix`, with
    /// the revocations known now. Their successful verification is the
    /// commit rule; cancellation and acceptance ordering is resolved by the
    /// durable workspace state transaction.
    ///
    /// # Errors
    ///
    /// Returns the precise acceptance, encoding, or signature-chain failure.
    pub fn verify(&self, revocations: &RevocationList) -> Result<(), OwnershipTransferError> {
        if self.version != OWNERSHIP_TRANSFER_VERSION {
            return Err(OwnershipTransferError::UnsupportedVersion);
        }
        self.acceptance
            .verify(&self.proposal, self.committed_unix, revocations)?;
        Ok(())
    }
}

/// A signature slot before signing; never verifies.
fn placeholder_signature() -> DelegatedSignature {
    DelegatedSignature {
        cert: tilo_user_identity::DeviceCert {
            user_public_key: [0; 32],
            device_public_key: [0; 32],
            device_label: String::new(),
            capabilities: tilo_user_identity::DeviceCapabilities::member(),
            issued_at_ns: 0,
            expires_at_ns: None,
            signature: [0; 64],
        },
        signature: [0; 64],
    }
}

/// A fresh device key certified by `root` with `capabilities`, ready to sign
/// ownership steps as that person. For tests and rendered fixtures, which
/// hold a root key in place of an enrolled device.
///
/// # Panics
///
/// Never in practice: the certificate is built for the key it pairs with.
#[doc(hidden)]
#[must_use]
#[allow(
    clippy::missing_panics_doc,
    clippy::expect_used,
    reason = "the certificate is minted for exactly this key"
)]
pub fn fixture_device_signer(
    root: &ed25519_dalek::SigningKey,
    capabilities: tilo_user_identity::DeviceCapabilities,
) -> tilo_user_identity::DeviceSigner {
    let (key, device) = tilo_user_identity::DeviceIdentity::generate("Fixture device", 1);
    let mut cert = tilo_user_identity::DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: device.public_key,
        device_label: device.label,
        capabilities,
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    tilo_user_identity::sign_device_cert(&mut cert, root).expect("root signs its own certificate");
    tilo_user_identity::DeviceSigner::new(key, cert).expect("the certificate names this key")
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
    #[error("this device signs for another person than the transfer names")]
    WrongSigner,
    #[error(transparent)]
    Canonical(#[from] CanonicalError),
    #[error("ownership-transfer signature: {0}")]
    Chain(#[from] ChainError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_user_identity::{
        DeviceCapabilities, DeviceSigner, RevocationEntry, RevocationReason, UserIdentity,
        sign_revocation_entry,
    };

    struct Person {
        root: ed25519_dalek::SigningKey,
        key: Ed25519PublicKey,
    }

    fn person() -> Person {
        let (root, user) = UserIdentity::generate("person", 1);
        Person {
            root,
            key: Ed25519PublicKey::from_bytes(user.public_key),
        }
    }

    /// A device the person enrolled without giving it their root.
    fn joined(person: &Person) -> DeviceSigner {
        fixture_device_signer(&person.root, DeviceCapabilities::joined())
    }

    fn propose(
        owner: &Person,
        recipient: &Person,
        signer: &DeviceSigner,
    ) -> OwnershipTransferProposalV1 {
        OwnershipTransferProposalV1::new(
            "workspace".into(),
            [3; 32],
            owner.key,
            recipient.key,
            20,
            signer,
        )
        .unwrap()
    }

    fn revoke(person: &Person, signer: &DeviceSigner) -> RevocationList {
        let mut entry = RevocationEntry {
            user_public_key: *person.key.as_bytes(),
            revoked_device_public_key: signer.cert().device_public_key,
            revoked_at_ns: 1,
            reason: RevocationReason::Lost,
            signature: [0; 64],
        };
        sign_revocation_entry(&mut entry, &person.root).unwrap();
        let mut list = RevocationList::new();
        list.insert(entry).unwrap();
        list
    }

    #[test]
    fn devices_without_the_root_hand_over_and_accept_ownership() {
        let none = RevocationList::new();
        let owner = person();
        let recipient = person();
        let proposal = propose(&owner, &recipient, &joined(&owner));
        proposal.verify(10, &none).unwrap();
        let acceptance =
            OwnershipTransferAcceptanceV1::new(&proposal, 10, &joined(&recipient)).unwrap();
        let audit = OwnershipTransferAuditV1::new(proposal.clone(), acceptance, 11, &none).unwrap();
        audit.verify(&none).unwrap();
        let cancellation =
            OwnershipTransferCancellationV1::new(&proposal, 12, &joined(&owner)).unwrap();
        cancellation.verify(&proposal, 12, &none).unwrap();
    }

    #[test]
    fn other_recipients_expiry_and_tampering_fail_closed() {
        let none = RevocationList::new();
        let owner = person();
        let recipient = person();
        let attacker = person();
        let proposal = propose(&owner, &recipient, &joined(&owner));
        assert_eq!(
            proposal.verify(20, &none),
            Err(OwnershipTransferError::Expired)
        );

        // Another person's device cannot sign as the recipient...
        assert_eq!(
            OwnershipTransferAcceptanceV1::new(&proposal, 10, &joined(&attacker)).unwrap_err(),
            OwnershipTransferError::WrongSigner
        );
        // ...nor pass off its own certificate for the recipient's.
        let mut forged =
            OwnershipTransferAcceptanceV1::new(&proposal, 10, &joined(&recipient)).unwrap();
        forged.signature = joined(&attacker)
            .sign_as_person(ACCEPTANCE_DOMAIN, &forged.signing_bytes().unwrap())
            .unwrap();
        assert_eq!(
            forged.verify(&proposal, 11, &none),
            Err(OwnershipTransferError::Chain(ChainError::OtherIdentity))
        );

        let mut accepted =
            OwnershipTransferAcceptanceV1::new(&proposal, 10, &joined(&recipient)).unwrap();
        accepted.proposal_digest[0] ^= 1;
        assert!(accepted.verify(&proposal, 11, &none).is_err());

        let mut tampered = proposal.clone();
        tampered.expires_unix = 30;
        assert_eq!(
            tampered.verify(10, &none),
            Err(OwnershipTransferError::Chain(ChainError::BadSignature))
        );

        // A signature over the proposal bytes cannot be replayed as a
        // cancellation: each step signs in its own domain.
        let mut replayed =
            OwnershipTransferCancellationV1::new(&proposal, 12, &joined(&owner)).unwrap();
        replayed.signature = proposal.signature.clone();
        assert!(replayed.verify(&proposal, 12, &none).is_err());

        let acceptance =
            OwnershipTransferAcceptanceV1::new(&proposal, 10, &joined(&recipient)).unwrap();
        let mut audit = OwnershipTransferAuditV1::new(proposal, acceptance, 11, &none).unwrap();
        audit.acceptance.signature.signature = [0; 64];
        assert!(audit.verify(&none).is_err());
    }

    #[test]
    fn host_only_and_revoked_devices_cannot_act_as_owner() {
        let none = RevocationList::new();
        let owner = person();
        let recipient = person();

        let host = fixture_device_signer(&owner.root, DeviceCapabilities::host());
        let proposal = propose(&owner, &recipient, &host);
        assert_eq!(
            proposal.verify(10, &none),
            Err(OwnershipTransferError::Chain(ChainError::NotPermitted(
                Authority::OwnWorkspaces
            )))
        );

        let device = joined(&owner);
        let proposal = propose(&owner, &recipient, &device);
        let revoked = revoke(&owner, &device);
        assert_eq!(
            proposal.verify(10, &revoked),
            Err(OwnershipTransferError::Chain(ChainError::Revoked))
        );

        // A revoked recipient device's acceptance is refused, and an audit
        // re-verified after the revocation is learned no longer holds.
        let fine = propose(&owner, &recipient, &joined(&owner));
        let recipient_device = joined(&recipient);
        let acceptance = OwnershipTransferAcceptanceV1::new(&fine, 10, &recipient_device).unwrap();
        let audit = OwnershipTransferAuditV1::new(fine, acceptance, 11, &none).unwrap();
        assert_eq!(
            audit.verify(&revoke(&recipient, &recipient_device)),
            Err(OwnershipTransferError::Chain(ChainError::Revoked))
        );
    }

    #[test]
    fn records_round_trip_through_their_wire_encodings() {
        let owner = person();
        let recipient = person();
        let proposal = propose(&owner, &recipient, &joined(&owner));
        let bytes = canonical_bytes(&proposal).unwrap();
        let back: OwnershipTransferProposalV1 = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(back, proposal);
        let json = serde_json::to_string(&proposal).unwrap();
        assert_eq!(
            serde_json::from_str::<OwnershipTransferProposalV1>(&json).unwrap(),
            proposal
        );
        back.verify(10, &RevocationList::new()).unwrap();
    }
}
