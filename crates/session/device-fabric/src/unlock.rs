//! The sibling side of unlocking an encrypted disk after a reboot.
//! Every device of the person holds the sibling halves of the others, as
//! [`UnlockEnrolment`]s sealed to the person's root-derived secret-sync key, so
//! only a device that holds the root can open one. Early boot of an enrolled device
//! asks over [`tilo_unlock_protocol::UNLOCK_ALPN`]; [`UnlockSibling`] answers.
//! A sibling releases a half only when the boot key is signed by the person's root
//! and matches an enrolment, the request answers a challenge this sibling issued,
//! this boot was not served before and the device is within its rate limit, and the
//! owner approved `device.unlock` or turned on auto-approval for exactly this
//! device. The half then travels sealed to the request's ephemeral key, and the
//! owner's answer is a fact every one of their devices keeps
//! ([`UnlockDecisionRecord`]), so a boot that lost its stream hears it again.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use tilo_seal::SecretKey;
use tilo_unlock_protocol::{
    APPROVAL_TIMEOUT_NS, BootId, BootKeyCert, CHALLENGE_LIFETIME_NS, Challenge, PROTOCOL_VERSION,
    RefusalReason, SealedSiblingHalf, SiblingFrame, SlotId, UnlockHello, UnlockRequest,
    approval_code, attest, open_sibling_half, seal_release,
};
use tilo_user_identity::{CertifiedFleet, DeviceStanding};

use crate::FabricError;
use tilo_network_actor::fabric::DeviceId;
use tilo_network_actor::fabric::FabricStamp;

/// The typed action the owner approves a pending unlock with.
pub const DEVICE_UNLOCK_ACTION: &str = "device.unlock";
/// Enrolments one device may publish (one per LUKS slot it unlocks this way).
pub const MAX_UNLOCK_ENROLMENTS: usize = 8;
/// Requests one device may start within [`RATE_WINDOW_NS`].
pub const MAX_REQUESTS_PER_WINDOW: usize = 5;
/// The rate-limit window.
pub const RATE_WINDOW_NS: u64 = 15 * 60 * 1_000_000_000;
/// Requests waiting for the owner at once, across all devices.
pub const MAX_PENDING: usize = 16;
/// Answers one device keeps at once, oldest dropped first.
pub const MAX_UNLOCK_DECISIONS: usize = 64;
/// How long the owner's answer stands for the boot that asked.
pub const DECISION_LIFETIME_NS: u64 = APPROVAL_TIMEOUT_NS;
/// Boots remembered as served, oldest dropped first.
const MAX_SERVED_BOOTS: usize = 1024;
/// Challenges outstanding at once, oldest dropped first.
const MAX_OPEN_CHALLENGES: usize = 256;
const AUTO_APPROVAL_CONTEXT: &str = "tilo-unlock auto-approval v1";
/// Boot chains one auto-approval may accept (current and next generation,
/// with room for a rollback target).
pub const MAX_ACCEPTED_BOOT_CHAINS: usize = 4;

/// A device's enrolment for one slot, published in its own fabric snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnlockEnrolment {
    pub cert: BootKeyCert,
    pub sealed_half: SealedSiblingHalf,
    pub stamp: FabricStamp,
}

impl UnlockEnrolment {
    /// Authored by the enrolled device itself, root-certified, bounded.
    pub fn validate(&self, owner: &DeviceId) -> Result<(), FabricError> {
        if &self.stamp.device != owner || owner.as_str() != hex::encode(self.cert.device_public_key)
        {
            return Err(FabricError::ForeignAuthorship);
        }
        if self.cert.verify().is_err() || self.sealed_half.sealed.len() > 256 {
            return Err(FabricError::InvalidUnlockEnrolment);
        }
        Ok(())
    }

    /// The certificate digest that names this enrolment.
    #[must_use]
    pub fn id(&self) -> Option<[u8; 32]> {
        self.cert.signing_digest().ok()
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

    #[cfg(test)]
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

    pub fn validate(&self) -> Result<(), FabricError> {
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
            Err(FabricError::InvalidUnlockEnrolment)
        }
    }
}

/// What the owner answered one boot's request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DecisionOutcome {
    /// The half is released to this boot, sealed to each request's own key.
    Approve,
    /// The boot is refused, and nobody is asked again while the answer holds.
    Deny,
}

/// Which boot's request an answer is about: its enrolment (the boot
/// certificate's digest), the boot's id, and the slot it asks for.
pub type DecisionKey = ([u8; 32], BootId, SlotId);

/// The owner's answer to one boot's request, kept in the answering device's
/// own fabric snapshot: it is that device's fact, and their other devices
/// carry it to answer the same boot the same way. A boot whose stream went
/// away before the answer reached it therefore hears the answer instead of
/// asking again, and a boot whose handler died leaves no question behind.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnlockDecisionRecord {
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

impl UnlockDecisionRecord {
    /// The boot this answers.
    #[must_use]
    pub fn key(&self) -> DecisionKey {
        (self.boot_cert_digest, self.boot_id, self.slot)
    }

    /// Written by the device whose snapshot carries it, and in force when the
    /// answer was made.
    pub fn validate(&self, owner: &DeviceId) -> Result<(), FabricError> {
        if owner.as_str() != hex::encode(self.decided_by)
            || self.valid_until_ns <= self.decided_at_ns
        {
            return Err(FabricError::ForeignAuthorship);
        }
        Ok(())
    }
}

/// The answer in force for the boot `digest`, `boot_id` asks for `slot`, if
/// the person's devices kept one.
#[must_use]
pub fn decision_for<'a>(
    decisions: &'a BTreeMap<DecisionKey, UnlockDecisionRecord>,
    digest: &[u8; 32],
    boot_id: BootId,
    slot: SlotId,
    now_ns: u64,
) -> Option<&'a UnlockDecisionRecord> {
    decisions
        .get(&(*digest, boot_id, slot))
        .filter(|record| record.valid_until_ns > now_ns)
}

/// What one device publishes about disk unlock in its fabric snapshot.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnlockFabricState {
    /// This device's own enrolments, one per slot.
    pub enrolments: Vec<UnlockEnrolment>,
    /// Root-signed auto-approval settings the owner made on this device,
    /// for any device of the person.
    pub auto_approvals: Vec<AutoApproval>,
    /// This device's answers to boots that asked it, kept while they hold.
    pub decisions: Vec<UnlockDecisionRecord>,
}

impl UnlockFabricState {
    pub fn validate(&self, owner: &DeviceId) -> Result<(), FabricError> {
        if self.enrolments.len() > MAX_UNLOCK_ENROLMENTS
            || self.auto_approvals.len() > crate::MAX_DEVICES
            || self.decisions.len() > MAX_UNLOCK_DECISIONS
        {
            return Err(FabricError::SnapshotTooLarge);
        }
        let mut slots = BTreeSet::new();
        for enrolment in &self.enrolments {
            enrolment.validate(owner)?;
            if !slots.insert(enrolment.cert.slot) {
                return Err(FabricError::DuplicateItem);
            }
        }
        let mut devices = BTreeSet::new();
        for approval in &self.auto_approvals {
            approval.validate()?;
            if !devices.insert(approval.device_public_key) {
                return Err(FabricError::DuplicateItem);
            }
        }
        let mut answered = BTreeSet::new();
        for decision in &self.decisions {
            decision.validate(owner)?;
            if !answered.insert(decision.key()) {
                return Err(FabricError::DuplicateItem);
            }
        }
        Ok(())
    }
}

/// What the person's fabric says about disk unlock: the enrolments and
/// auto-approvals it carries, and the answers to boots that are still in
/// force.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MergedUnlock {
    pub enrolments: BTreeMap<[u8; 32], UnlockEnrolment>,
    pub auto_approvals: BTreeMap<[u8; 32], AutoApproval>,
    pub decisions: BTreeMap<DecisionKey, UnlockDecisionRecord>,
}

/// The enrolments, auto-approvals and answers across the person's fabric,
/// keeping only those of the fleet's root and of devices it has not revoked.
/// The newest auto-approval per device wins, and the newest answer per boot,
/// an approval over a denial made at the same moment.
#[must_use]
pub fn merged_unlock_state<'a>(
    snapshots: impl IntoIterator<Item = &'a UnlockFabricState>,
    fleet: &CertifiedFleet,
    now_ns: u64,
) -> MergedUnlock {
    let mut merged = MergedUnlock::default();
    let standing = |device: &[u8; 32]| fleet.standing(device, now_ns);
    for state in snapshots {
        for enrolment in &state.enrolments {
            if enrolment.cert.user_public_key == *fleet.user_public_key()
                && standing(&enrolment.cert.device_public_key) == DeviceStanding::Certified
                && let Some(id) = enrolment.id()
            {
                merged.enrolments.insert(id, enrolment.clone());
            }
        }
        for approval in &state.auto_approvals {
            if approval.user_public_key != *fleet.user_public_key()
                || standing(&approval.device_public_key) != DeviceStanding::Certified
            {
                continue;
            }
            let newer = merged
                .auto_approvals
                .get(&approval.device_public_key)
                .is_none_or(|current| approval.issued_at_ns > current.issued_at_ns);
            if newer {
                merged
                    .auto_approvals
                    .insert(approval.device_public_key, approval.clone());
            }
        }
        for decision in &state.decisions {
            if standing(&decision.decided_by) != DeviceStanding::Certified
                || decision.valid_until_ns <= now_ns
            {
                continue;
            }
            let newer = merged.decisions.get(&decision.key()).is_none_or(|current| {
                (
                    decision.decided_at_ns,
                    decision.outcome == DecisionOutcome::Approve,
                ) >= (
                    current.decided_at_ns,
                    current.outcome == DecisionOutcome::Approve,
                )
            });
            if newer {
                merged.decisions.insert(decision.key(), decision.clone());
            }
        }
    }
    merged
}

/// What a sibling knows when it answers: the person's fleet, the enrolments,
/// auto-approvals and answers the fabric carries, and the secret-sync key
/// when this device holds the root.
pub struct UnlockView<'a> {
    pub fleet: &'a CertifiedFleet,
    pub enrolments: &'a BTreeMap<[u8; 32], UnlockEnrolment>,
    pub auto_approvals: &'a BTreeMap<[u8; 32], AutoApproval>,
    /// The owner's answers still in force, from every device of theirs.
    pub decisions: &'a BTreeMap<DecisionKey, UnlockDecisionRecord>,
    /// Whether this device holds the person's root, so it can release.
    pub custodian: bool,
    /// The secret-sync key, loaded only where a half is released.
    pub secret_sync: Option<&'a SecretKey>,
}

/// A request waiting for the owner's decision on `device.unlock`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingUnlock {
    pub request_digest: [u8; 32],
    pub approval_code: String,
    pub enrolment: [u8; 32],
    pub device_public_key: [u8; 32],
    pub slot: SlotId,
    pub boot_id: BootId,
    pub ephemeral_public: [u8; 32],
    pub expires_at_ns: u64,
}

/// The owner's answer to a pending `device.unlock`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnlockDecision {
    Approve { approver: [u8; 32] },
    Deny { approver: [u8; 32] },
}

/// What the owner's answer means for the boot waiting on it: the frame to
/// send it, and the answer to keep for the boot's next attempt — none where
/// the answer releases nothing (the half could not be opened, the device was
/// removed, or nobody decided in time).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decided {
    pub frame: SiblingFrame,
    pub record: Option<UnlockDecisionRecord>,
}

#[derive(Clone, Debug)]
struct OpenChallenge {
    enrolment: [u8; 32],
    challenge: Challenge,
}

/// The sibling's state: outstanding challenges, served boots, rate limits
/// and requests waiting for the owner.
#[derive(Debug)]
pub struct UnlockSibling {
    sibling_public_key: [u8; 32],
    challenges: BTreeMap<[u8; 32], OpenChallenge>,
    challenge_order: VecDeque<[u8; 32]>,
    served: BTreeSet<([u8; 32], BootId)>,
    served_order: VecDeque<([u8; 32], BootId)>,
    attempts: BTreeMap<[u8; 32], VecDeque<u64>>,
    pending: BTreeMap<[u8; 32], PendingUnlock>,
}

impl UnlockSibling {
    #[must_use]
    pub fn new(sibling_public_key: [u8; 32]) -> Self {
        Self {
            sibling_public_key,
            challenges: BTreeMap::new(),
            challenge_order: VecDeque::new(),
            served: BTreeSet::new(),
            served_order: VecDeque::new(),
            attempts: BTreeMap::new(),
            pending: BTreeMap::new(),
        }
    }

    /// Answer a hello with a fresh challenge, or refuse. `nonce` must come
    /// from the OS CSPRNG.
    pub fn hello(
        &mut self,
        hello: &UnlockHello,
        view: &UnlockView<'_>,
        nonce: [u8; 32],
        now_ns: u64,
    ) -> SiblingFrame {
        match self.admit(hello, view, now_ns) {
            Ok(enrolment) => {
                let challenge = Challenge {
                    version: PROTOCOL_VERSION,
                    sibling_public_key: self.sibling_public_key,
                    nonce,
                    expires_at_ns: now_ns.saturating_add(CHALLENGE_LIFETIME_NS),
                };
                if self.challenge_order.len() >= MAX_OPEN_CHALLENGES
                    && let Some(oldest) = self.challenge_order.pop_front()
                {
                    self.challenges.remove(&oldest);
                }
                self.challenge_order.push_back(nonce);
                self.challenges.insert(
                    nonce,
                    OpenChallenge {
                        enrolment,
                        challenge: challenge.clone(),
                    },
                );
                SiblingFrame::Challenge(challenge)
            }
            Err(reason) => refuse(reason),
        }
    }

    fn admit(
        &mut self,
        hello: &UnlockHello,
        view: &UnlockView<'_>,
        now_ns: u64,
    ) -> Result<[u8; 32], RefusalReason> {
        if hello.version != PROTOCOL_VERSION {
            return Err(RefusalReason::UnsupportedVersion);
        }
        let cert = &hello.boot_cert;
        if cert.user_public_key != *view.fleet.user_public_key() || cert.verify().is_err() {
            return Err(RefusalReason::BadCertificate);
        }
        match view.fleet.standing(&cert.device_public_key, now_ns) {
            DeviceStanding::Certified => {}
            DeviceStanding::Revoked(_) => return Err(RefusalReason::Revoked),
            DeviceStanding::Uncertified => return Err(RefusalReason::BadCertificate),
        }
        let id = cert
            .signing_digest()
            .map_err(|_| RefusalReason::BadCertificate)?;
        let enrolment = view.enrolments.get(&id).ok_or(RefusalReason::NotEnrolled)?;
        if enrolment.cert != *cert {
            return Err(RefusalReason::NotEnrolled);
        }
        self.charge(cert.device_public_key, now_ns)?;
        if !view.custodian {
            return Err(RefusalReason::NotACustodian);
        }
        Ok(id)
    }

    /// Count one attempt against the device's rate limit.
    fn charge(&mut self, device: [u8; 32], now_ns: u64) -> Result<(), RefusalReason> {
        let attempts = self.attempts.entry(device).or_default();
        while attempts
            .front()
            .is_some_and(|at| now_ns.saturating_sub(*at) >= RATE_WINDOW_NS)
        {
            attempts.pop_front();
        }
        if attempts.len() >= MAX_REQUESTS_PER_WINDOW {
            return Err(RefusalReason::RateLimited);
        }
        attempts.push_back(now_ns);
        Ok(())
    }

    /// Handle the signed answer to `challenge_nonce` (the challenge issued on
    /// this stream). Returns `Pending` (the owner is asked through
    /// `device.unlock`), `Released` (the owner allowed this boot, or
    /// auto-approved it), `Denied` (the owner's answer to this boot is in
    /// force), or `Refused`.
    pub fn request(
        &mut self,
        request: &UnlockRequest,
        hello: &UnlockHello,
        challenge_nonce: &[u8; 32],
        view: &UnlockView<'_>,
        now_ns: u64,
    ) -> SiblingFrame {
        let cert = &hello.boot_cert;
        let device = cert.device_public_key;
        let (pending, enrolment) =
            match self.check_request(request, cert, challenge_nonce, view, now_ns) {
                Ok(checked) => checked,
                Err(reason) => return refuse(reason),
            };
        // The person already answered this boot: it hears that answer again,
        // with a half sealed to this request's own key where they allowed
        // it, instead of asking them a second time.
        if let Some(frame) = self.answered(&pending, &enrolment, view, now_ns) {
            return frame;
        }
        if self
            .served
            .contains(&(pending.device_public_key, pending.boot_id))
        {
            return refuse(RefusalReason::BootAlreadyServed);
        }
        self.forget_unanswered(now_ns);
        if self.pending.len() >= MAX_PENDING {
            return refuse(RefusalReason::RateLimited);
        }
        if auto_approved(request, cert, self.sibling_public_key, view).is_some() {
            let frame = release(&pending, &enrolment, view);
            if matches!(frame, SiblingFrame::Released(_)) {
                self.mark_served(device, request.boot_id);
            }
            return frame;
        }
        let code = pending.approval_code.clone();
        self.pending.insert(pending.request_digest, pending);
        SiblingFrame::Pending {
            approval_code: code,
        }
    }

    /// The shape of a request that passed every check, before the owner is
    /// asked anything.
    fn check_request(
        &mut self,
        request: &UnlockRequest,
        cert: &BootKeyCert,
        challenge_nonce: &[u8; 32],
        view: &UnlockView<'_>,
        now_ns: u64,
    ) -> Result<(PendingUnlock, UnlockEnrolment), RefusalReason> {
        if request.nonce != *challenge_nonce {
            return Err(RefusalReason::UnknownChallenge);
        }
        // Single use: whatever happens next, this challenge is spent.
        let open = self
            .challenges
            .remove(challenge_nonce)
            .ok_or(RefusalReason::UnknownChallenge)?;
        self.challenge_order
            .retain(|nonce| nonce != challenge_nonce);
        let digest = request.verify(cert, &open.challenge, now_ns)?;
        // The fleet or the enrolments may have changed since the hello.
        if let DeviceStanding::Revoked(_) = view.fleet.standing(&cert.device_public_key, now_ns) {
            return Err(RefusalReason::Revoked);
        }
        let enrolment = view
            .enrolments
            .get(&open.enrolment)
            .filter(|enrolment| enrolment.cert == *cert)
            .ok_or(RefusalReason::NotEnrolled)?
            .clone();
        Ok((
            PendingUnlock {
                request_digest: digest,
                approval_code: approval_code(&digest),
                enrolment: open.enrolment,
                device_public_key: cert.device_public_key,
                slot: request.slot,
                boot_id: request.boot_id,
                ephemeral_public: request.ephemeral_public,
                expires_at_ns: now_ns.saturating_add(APPROVAL_TIMEOUT_NS),
            },
            enrolment,
        ))
    }

    /// The owner's answer to this boot, while it holds: a denial as it stands,
    /// an approval as a half sealed to this request's own key. A boot that
    /// reconnects after its stream went away is not put to the person again.
    fn answered(
        &mut self,
        pending: &PendingUnlock,
        enrolment: &UnlockEnrolment,
        view: &UnlockView<'_>,
        now_ns: u64,
    ) -> Option<SiblingFrame> {
        let record = decision_for(
            view.decisions,
            &pending.enrolment,
            pending.boot_id,
            pending.slot,
            now_ns,
        )?;
        match record.outcome {
            DecisionOutcome::Deny => Some(SiblingFrame::Denied),
            DecisionOutcome::Approve => {
                let frame = release(pending, enrolment, view);
                if matches!(frame, SiblingFrame::Released(_)) {
                    self.mark_served(pending.device_public_key, pending.boot_id);
                }
                Some(frame)
            }
        }
    }

    /// Let go of the questions nobody can answer any more: a request whose
    /// answer window passed while its handler was gone is no longer pending,
    /// so it holds no place another boot would be refused for.
    fn forget_unanswered(&mut self, now_ns: u64) {
        self.pending
            .retain(|_, pending| pending.expires_at_ns > now_ns);
    }

    /// The boot waiting on `request_digest` went away: nobody can receive a
    /// decision any more, and the person is asked no further.
    pub fn withdraw(&mut self, request_digest: &[u8; 32]) {
        self.pending.remove(request_digest);
    }

    /// Requests waiting for the owner, by request digest.
    pub fn pending(&self) -> impl Iterator<Item = &PendingUnlock> {
        self.pending.values()
    }

    /// Apply the owner's decision. Returns the frame for the waiting boot and
    /// the answer to keep, or `None` when no such request is pending (already
    /// decided, withdrawn, or never made).
    pub fn decide(
        &mut self,
        request_digest: &[u8; 32],
        decision: UnlockDecision,
        view: &UnlockView<'_>,
        now_ns: u64,
    ) -> Option<Decided> {
        let pending = self.pending.remove(request_digest)?;
        if pending.expires_at_ns <= now_ns {
            return Some(Decided {
                frame: SiblingFrame::Denied,
                record: None,
            });
        }
        let approver = match decision {
            UnlockDecision::Deny { approver } => {
                return Some(Decided {
                    frame: SiblingFrame::Denied,
                    record: Some(decision_record(
                        &pending,
                        DecisionOutcome::Deny,
                        approver,
                        now_ns,
                    )),
                });
            }
            UnlockDecision::Approve { approver } => approver,
        };
        let still_enrolled = !matches!(
            view.fleet.standing(&pending.device_public_key, now_ns),
            DeviceStanding::Revoked(_)
        ) && view.enrolments.contains_key(&pending.enrolment);
        let frame = if still_enrolled {
            view.enrolments.get(&pending.enrolment).map_or(
                SiblingFrame::Refused(RefusalReason::NotEnrolled),
                |enrolment| release(&pending, enrolment, view),
            )
        } else {
            SiblingFrame::Refused(RefusalReason::Revoked)
        };
        let record = match &frame {
            SiblingFrame::Released(_) => {
                self.mark_served(pending.device_public_key, pending.boot_id);
                Some(decision_record(
                    &pending,
                    DecisionOutcome::Approve,
                    approver,
                    now_ns,
                ))
            }
            _ => None,
        };
        Some(Decided { frame, record })
    }

    /// The device left the fleet: forget everything about it. The fabric
    /// drops its enrolments (and with them the sealed halves) separately.
    pub fn remove_device(&mut self, device_public_key: [u8; 32]) {
        self.pending
            .retain(|_, pending| pending.device_public_key != device_public_key);
        self.attempts.remove(&device_public_key);
    }

    fn mark_served(&mut self, device: [u8; 32], boot_id: BootId) {
        if self.served_order.len() >= MAX_SERVED_BOOTS
            && let Some(oldest) = self.served_order.pop_front()
        {
            self.served.remove(&oldest);
        }
        self.served_order.push_back((device, boot_id));
        self.served.insert((device, boot_id));
    }
}

/// The answer to a request nobody will be asked about.
fn refuse(reason: RefusalReason) -> SiblingFrame {
    SiblingFrame::Refused(reason)
}

/// The accepted boot chain when the owner auto-approves this device and the
/// request's TPM quote proves that chain for this exact request.
fn auto_approved(
    request: &UnlockRequest,
    cert: &BootKeyCert,
    sibling_public_key: [u8; 32],
    view: &UnlockView<'_>,
) -> Option<[u8; 32]> {
    let policy = view.auto_approvals.get(&cert.device_public_key)?;
    if policy.user_public_key != *view.fleet.user_public_key()
        || policy.device_public_key != cert.device_public_key
        || policy.validate().is_err()
    {
        return None;
    }
    let key = cert.attestation_key.as_deref()?;
    let quote = request.attestation.as_ref()?;
    let challenge = Challenge {
        version: PROTOCOL_VERSION,
        sibling_public_key,
        nonce: request.nonce,
        expires_at_ns: 0,
    };
    let nonce = attest::qualifying_data(
        cert,
        &challenge,
        &request.boot_id,
        &request.ephemeral_public,
    )
    .ok()?;
    let chain = attest::verify_quote(key, quote, &nonce).ok()?;
    policy
        .accepted_boot_chains
        .contains(&chain)
        .then_some(chain)
}

/// The answer to keep for `pending`, in force for [`DECISION_LIFETIME_NS`].
fn decision_record(
    pending: &PendingUnlock,
    outcome: DecisionOutcome,
    decided_by: [u8; 32],
    now_ns: u64,
) -> UnlockDecisionRecord {
    UnlockDecisionRecord {
        boot_cert_digest: pending.enrolment,
        boot_id: pending.boot_id,
        slot: pending.slot,
        outcome,
        decided_by,
        decided_at_ns: now_ns,
        valid_until_ns: now_ns.saturating_add(DECISION_LIFETIME_NS),
    }
}

/// Open the sibling half with the secret-sync key and seal it to the
/// request's ephemeral key.
fn release(
    pending: &PendingUnlock,
    enrolment: &UnlockEnrolment,
    view: &UnlockView<'_>,
) -> SiblingFrame {
    let Some(secret_sync) = view.secret_sync else {
        return SiblingFrame::Refused(RefusalReason::NotACustodian);
    };
    open_sibling_half(secret_sync, &enrolment.cert, &enrolment.sealed_half)
        .and_then(|half| seal_release(&pending.ephemeral_public, &pending.request_digest, &half))
        .map_or(
            SiblingFrame::Refused(RefusalReason::NotACustodian),
            SiblingFrame::Released,
        )
}

#[cfg(test)]
#[path = "unlock_tests.rs"]
mod tests;
