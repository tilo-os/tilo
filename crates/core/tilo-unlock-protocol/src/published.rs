//! What a device publishes about disk unlock in its own entry of the person's
//! devices: its enrolments, the auto-approvals its person's root signed, and
//! its answers to boots. Every device of the person reads these; the device
//! whose entry keeps them alone writes them.

use std::collections::{BTreeMap, BTreeSet};

use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tilo_user_identity::{
    DeviceCapabilities, DeviceCert, device_cert_signing_digest, verify_device_cert,
};

use crate::{BootId, SealedSiblingHalf, SlotId};

/// Auto-approvals one device may publish: one per device of the person.
pub const MAX_AUTO_APPROVALS: usize = 64;

/// Why what a device publishes about disk unlock is not acceptable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum PublishedError {
    /// More than its bound.
    #[error("what a device says of itself exceeds its bound")]
    TooLarge,
    /// Written by another device than the one whose entry keeps it.
    #[error("a value was authored by a different device")]
    ForeignAuthorship,
    /// One thing said twice.
    #[error("a device says one thing twice")]
    DuplicateItem,
    /// An enrolment or setting that does not verify.
    #[error("invalid disk-unlock enrolment or setting")]
    InvalidUnlockEnrolment,
}

/// Enrolments one device may publish (one per LUKS slot it unlocks this way).
pub const MAX_UNLOCK_ENROLMENTS: usize = 8;
/// Answers one device keeps at once, oldest dropped first.
pub const MAX_UNLOCK_DECISIONS: usize = 64;
const AUTO_APPROVAL_CONTEXT: &str = "tilo-unlock auto-approval v1";
/// Boot chains one auto-approval may accept (current and next generation,
/// with room for a rollback target).
pub const MAX_ACCEPTED_BOOT_CHAINS: usize = 4;

/// A device's enrolment for one key of its disk, kept in its own part of the person's store.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnlockEnrolment {
    /// The root's certificate for the boot key, which grants nothing.
    pub cert: DeviceCert,
    /// The device whose disk this boot key unlocks: the one that asked for the
    /// enrolment, which is not always the one that keeps it.
    pub device_public_key: [u8; 32],
    pub sealed_half: SealedSiblingHalf,
    /// The volume and key slot this enrolment unlocks; the request must ask
    /// for exactly this one.
    pub slot: SlotId,
    /// The device's TPM attestation key, when it has one: what an
    /// auto-approval's quote verifies under.
    pub attestation_key: Option<Vec<u8>>,
    /// The device whose entry of the person's devices keeps it, which alone
    /// writes it there.
    pub kept_by: [u8; 32],
}

impl UnlockEnrolment {
    /// Authored by the device that keeps it, root-certified, bounded.
    ///
    /// # Errors
    ///
    /// [`PublishedError::InvalidUnlockEnrolment`] otherwise.
    pub fn validate(&self, owner: &[u8; 32]) -> Result<(), PublishedError> {
        if &self.kept_by != owner
            || verify_device_cert(&self.cert).is_err()
            || self.cert.capabilities != DeviceCapabilities::nothing()
            || self.sealed_half.sealed.len() > 256
        {
            return Err(PublishedError::InvalidUnlockEnrolment);
        }
        Ok(())
    }

    /// The device whose disk this boot key unlocks.
    #[must_use]
    pub fn device(&self) -> [u8; 32] {
        self.device_public_key
    }

    /// The certificate digest that names this enrolment.
    #[must_use]
    pub fn id(&self) -> Option<[u8; 32]> {
        device_cert_signing_digest(&self.cert).ok()
    }
}

/// The owner's per-device auto-approval, signed by the person's root so no
/// device can turn it on for itself. `accepted_boot_chains` empty means off.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoApproval {
    pub user_public_key: [u8; 32],
    pub device_public_key: [u8; 32],
    /// PCR 4 + 7 composites ([`attest::pcr_composite`]) the owner accepts.
    pub accepted_boot_chains: Vec<[u8; 32]>,
    pub issued_at_ns: u64,
    pub signature: Vec<u8>,
}

#[derive(Serialize)]
struct AutoApprovalView<'a> {
    user_public_key: &'a [u8; 32],
    device_public_key: &'a [u8; 32],
    accepted_boot_chains: &'a [[u8; 32]],
    issued_at_ns: u64,
}

impl AutoApproval {
    /// An unsigned setting; [`Self::digest`] is what the root signs.
    #[must_use]
    pub fn unsigned(
        user_public_key: [u8; 32],
        device_public_key: [u8; 32],
        accepted_boot_chains: Vec<[u8; 32]>,
        issued_at_ns: u64,
    ) -> Self {
        Self {
            user_public_key,
            device_public_key,
            accepted_boot_chains,
            issued_at_ns,
            signature: Vec::new(),
        }
    }

    /// The digest the person's root signs.
    #[must_use]
    pub fn digest(&self) -> Option<[u8; 32]> {
        let bytes = postcard::to_allocvec(&AutoApprovalView {
            user_public_key: &self.user_public_key,
            device_public_key: &self.device_public_key,
            accepted_boot_chains: &self.accepted_boot_chains,
            issued_at_ns: self.issued_at_ns,
        })
        .ok()?;
        Some(blake3::derive_key(AUTO_APPROVAL_CONTEXT, &bytes))
    }

    /// A setting `root` signs: what the person's root makes, and what a test
    /// stands in for it with.
    #[must_use]
    pub fn signed(
        root: &ed25519_dalek::SigningKey,
        device_public_key: [u8; 32],
        accepted_boot_chains: Vec<[u8; 32]>,
        issued_at_ns: u64,
    ) -> Option<Self> {
        if accepted_boot_chains.len() > MAX_ACCEPTED_BOOT_CHAINS {
            return None;
        }
        let mut approval = Self::unsigned(
            root.verifying_key().to_bytes(),
            device_public_key,
            accepted_boot_chains,
            issued_at_ns,
        );
        approval.signature = ed25519_dalek::Signer::sign(root, &approval.digest()?)
            .to_bytes()
            .to_vec();
        Some(approval)
    }

    /// Signed by the person's root it names, and bounded.
    ///
    /// # Errors
    ///
    /// [`PublishedError::InvalidUnlockEnrolment`] otherwise.
    pub fn validate(&self) -> Result<(), PublishedError> {
        let valid = self.accepted_boot_chains.len() <= MAX_ACCEPTED_BOOT_CHAINS
            && <[u8; 64]>::try_from(self.signature.as_slice())
                .ok()
                .zip(VerifyingKey::from_bytes(&self.user_public_key).ok())
                .zip(self.digest())
                .is_some_and(|((signature, root), digest)| {
                    root.verify_strict(&digest, &Signature::from_bytes(&signature))
                        .is_ok()
                });
        if valid {
            Ok(())
        } else {
            Err(PublishedError::InvalidUnlockEnrolment)
        }
    }
}

/// What the owner answered one boot's request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DecisionOutcome {
    /// The half is released to this boot, sealed to each request's own key.
    Approve,
    /// The boot is refused, and nobody is asked again while the answer stands.
    Deny,
}

/// Which boot's request an answer is about: its enrolment (the boot
/// certificate's digest), the boot's id, and the slot it asks for.
pub type DecisionKey = ([u8; 32], BootId, SlotId);

/// The owner's answer to one boot's request, kept in the answering device's
/// own part of the person's store: it is that device's fact, and their other devices
/// carry it to answer the same boot the same way. A boot whose stream went
/// away before the answer reached it therefore hears the answer instead of
/// asking again, and a boot whose handler died leaves no question behind.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnlockDecisionEntry {
    /// The enrolment that asked: the boot certificate's digest.
    pub boot_cert_digest: [u8; 32],
    pub boot_id: BootId,
    pub slot: SlotId,
    pub outcome: DecisionOutcome,
    /// The device of the person whose owner answered.
    pub decided_by: [u8; 32],
    pub decided_at_ns: u64,
    /// After this the request is put to the person again.
    pub valid_until_ns: u64,
}

impl UnlockDecisionEntry {
    /// The boot this answers.
    #[must_use]
    pub fn key(&self) -> DecisionKey {
        (self.boot_cert_digest, self.boot_id, self.slot)
    }

    /// Written by the device whose part of the store carries it, and in force when the
    /// answer was made.
    ///
    /// # Errors
    ///
    /// [`PublishedError::ForeignAuthorship`] otherwise.
    pub fn validate(&self, owner: &[u8; 32]) -> Result<(), PublishedError> {
        if owner != &self.decided_by
            || self.valid_until_ns <= self.decided_at_ns
        {
            return Err(PublishedError::ForeignAuthorship);
        }
        Ok(())
    }
}

/// The answer in force for the boot `digest`, `boot_id` asks for `slot`, if
/// the person's devices kept one.
#[must_use]
pub fn decision_for<'a>(
    decisions: &'a BTreeMap<DecisionKey, UnlockDecisionEntry>,
    digest: &[u8; 32],
    boot_id: BootId,
    slot: SlotId,
    now_ns: u64,
) -> Option<&'a UnlockDecisionEntry> {
    decisions
        .get(&(*digest, boot_id, slot))
        .filter(|record| record.valid_until_ns > now_ns)
}

/// What one device keeps about disk unlock in its part of the person's store.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnlockFabricState {
    /// This device's own enrolments, one per slot.
    pub enrolments: Vec<UnlockEnrolment>,
    /// Root-signed auto-approval settings the owner made on this device,
    /// for any device of the person.
    pub auto_approvals: Vec<AutoApproval>,
    /// This device's answers to boots that asked it, kept while they stand.
    pub decisions: Vec<UnlockDecisionEntry>,
}

impl UnlockFabricState {
    /// Bounded, and all of it written by `owner`, whose entry keeps it.
    ///
    /// # Errors
    ///
    /// The first way it is not.
    pub fn validate(&self, owner: &[u8; 32]) -> Result<(), PublishedError> {
        if self.enrolments.len() > MAX_UNLOCK_ENROLMENTS
            || self.auto_approvals.len() > MAX_AUTO_APPROVALS
            || self.decisions.len() > MAX_UNLOCK_DECISIONS
        {
            return Err(PublishedError::TooLarge);
        }
        let mut slots = BTreeSet::new();
        for enrolment in &self.enrolments {
            enrolment.validate(owner)?;
            if !slots.insert(enrolment.slot) {
                return Err(PublishedError::DuplicateItem);
            }
        }
        let mut devices = BTreeSet::new();
        for approval in &self.auto_approvals {
            approval.validate()?;
            if !devices.insert(approval.device_public_key) {
                return Err(PublishedError::DuplicateItem);
            }
        }
        let mut answered = BTreeSet::new();
        for decision in &self.decisions {
            decision.validate(owner)?;
            if !answered.insert(decision.key()) {
                return Err(PublishedError::DuplicateItem);
            }
        }
        Ok(())
    }
}
