//! What a room's governor records between the steps of a join, on every
//! host that governs rooms: a relay, a cloud host or the device that made
//! the room. One ledger, so a room is joined the same way wherever it is
//! hosted.
//!
//! The cryptographic and governed-document contract is [`crate::join`]'s
//! and [`crate::governance`]'s. The ledger keeps the invitations a room
//! issued, the challenges it handed out, the receipts of completed joins and
//! of administration requests, with the request idempotency each exchange
//! needs. It never writes: the host stages a change here, makes the room
//! durable with it, and only then answers.
//!
//! Nothing here draws randomness or reads a clock; the host passes nonces
//! and `now`, so the ledger runs wherever the room engine does.

use std::collections::BTreeSet;
use std::fmt;

use web_time::SystemTime;

use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use tilo_user_identity::{DeviceCert, RevocationList};

use crate::StatePeer;
use crate::join::invitations::{
    CancelInvitationRequest, IssueInvitationRequest, IssuedInvitation, PendingInvitation,
};
use crate::join::{
    InviteGrant, Inviter, JoinBootstrap, JoinChallenge, JoinChallengeRequest, JoinProof,
    MemberRole, MembershipChallenge, MembershipChallengeRequest, MembershipCompleteRequest,
    TransportHint, VerifiedJoin, VerifiedRedemption, decode_and_verify_invitation,
    issue_invitation, issue_invitation_from, verify_join_proof, verify_membership_proof,
};

/// Pending challenges of one kind a room keeps at once.
const MAX_PENDING_CHALLENGES: usize = 4096;
/// How long a join or membership challenge may be answered.
pub const CHALLENGE_LIFETIME_NS: u64 = 60_000_000_000;
/// The longest an invitation may stay redeemable.
const MAX_INVITATION_LIFETIME_NS: u64 = 30 * 24 * 60 * 60 * 1_000_000_000;

/// Why the governor refused or failed a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinError {
    message: String,
    /// What a workspace join answer carries as its code: 403 for a
    /// refusal, [`crate::join::JOIN_PROOF_LAPSED`] for a proof this host
    /// never admitted and no longer can.
    code: u16,
}

impl JoinError {
    /// A refusal.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: 403,
        }
    }

    /// A Complete whose proof this host never admitted and no longer can:
    /// it holds neither the challenge it answers nor a completion of its
    /// request.
    fn lapsed(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: crate::join::JOIN_PROOF_LAPSED,
        }
    }

    /// A Complete this host admitted before, whose device takes the room's
    /// current state through the membership exchange instead
    /// ([`crate::join::JOIN_COMPLETED_REFRESH`]).
    fn refresh() -> Self {
        Self {
            message:
                "join request ID already completed; refresh membership for the current workspace"
                    .into(),
            code: crate::join::JOIN_COMPLETED_REFRESH,
        }
    }

    /// The code a workspace join answer carries for this refusal.
    #[must_use]
    pub const fn code(&self) -> u16 {
        self.code
    }
}

impl fmt::Display for JoinError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.message.fmt(formatter)
    }
}

impl std::error::Error for JoinError {}

impl From<String> for JoinError {
    fn from(message: String) -> Self {
        Self::new(message)
    }
}

/// The governed room a ledger answers for, as committed now.
#[derive(Clone, Copy)]
pub struct Governed<'a> {
    /// The room's admitted state.
    pub peer: &'a StatePeer,
    /// The room's workspace id on the wire
    /// ([`crate::join::room_workspace_id`]).
    pub workspace_id: u64,
    /// The room's committed generation.
    pub generation: u64,
    /// The signed governance a joining member admits, oldest first.
    pub governance_messages: &'a dyn GovernanceMessages,
}

/// Where a governor keeps the signed governance a joining member admits.
pub trait GovernanceMessages {
    /// That governance, oldest first.
    fn governance_messages(&self) -> Vec<Vec<u8>>;
}

impl<F: Fn() -> Vec<Vec<u8>>> GovernanceMessages for F {
    fn governance_messages(&self) -> Vec<Vec<u8>> {
        self()
    }
}

impl Governed<'_> {
    /// The bootstrap a device admitted as `role` with `invite_id` receives.
    #[must_use]
    pub fn bootstrap(
        &self,
        invite_id: [u8; 32],
        device_cert: DeviceCert,
        role: MemberRole,
    ) -> JoinBootstrap {
        JoinBootstrap {
            version: 1,
            invite_id,
            device_cert,
            role,
            room: self.peer.room().to_owned(),
            workspace_id: self.workspace_id,
            authority: self.peer.authority().clone(),
            certified_roster: self.peer.certificates(),
            revocations: self.peer.revocations(),
            records: self.peer.rows(),
            governance_messages: self.governance_messages.governance_messages(),
            generation: self.generation,
        }
    }

    fn revocations(&self) -> Result<RevocationList, JoinError> {
        let mut revocations = RevocationList::new();
        for entry in self.peer.revocations() {
            revocations
                .insert(entry)
                .map_err(|error| JoinError::new(error.to_string()))?;
        }
        Ok(revocations)
    }
}

/// The governor's key signing an invitation, and the nonce that makes this
/// invitation unique.
#[derive(Clone, Copy)]
pub struct Signer<'a> {
    pub key: &'a SigningKey,
    pub nonce: [u8; 16],
}

/// A verified redemption to admit, and what its receipt answers for.
pub struct Redeeming<'a> {
    pub remote_endpoint: [u8; 32],
    pub request_id: [u8; 32],
    pub proof_digest: [u8; 32],
    pub redemption: &'a VerifiedRedemption,
}

/// What a room's governor keeps between the steps of its joins. Its bytes
/// are part of the host's room record (postcard on a relay): field order is
/// the format, append only.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinLedger {
    invitations: Vec<InvitationRecord>,
    join_challenges: Vec<JoinChallengeRecord>,
    join_completions: Vec<JoinCompletionRecord>,
    membership_challenges: Vec<MembershipChallengeRecord>,
    administration: Vec<AdministrationRecord>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct InvitationRecord {
    request_id: [u8; 32],
    request_digest: [u8; 32],
    issued: IssuedInvitation,
    grant: InviteGrant,
    cancellation: Option<CancellationRecord>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct CancellationRecord {
    request_id: [u8; 32],
    request_digest: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct JoinChallengeRecord {
    remote_endpoint: [u8; 32],
    request_digest: [u8; 32],
    device_cert: DeviceCert,
    challenge: JoinChallenge,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct JoinCompletionRecord {
    remote_endpoint: [u8; 32],
    request_id: [u8; 32],
    proof_digest: [u8; 32],
    /// The delivered bootstrap with the room's state it carried left out and
    /// its governance messages replaced by one digest entry (see
    /// [`JoinCompletionRecord::receipt`]).
    bootstrap: JoinBootstrap,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct MembershipChallengeRecord {
    remote_endpoint: [u8; 32],
    request_digest: [u8; 32],
    device_cert: DeviceCert,
    challenge: MembershipChallenge,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct AdministrationRecord {
    request_id: [u8; 32],
    request_digest: [u8; 32],
    generation: u64,
}

/// Marks the single governance entry of a completion receipt. An encoded
/// neighbour message starts with its small postcard variant index instead.
const JOIN_RECEIPT_TAG: &[u8] = b"tilo.join-receipt.v1\0";

impl JoinCompletionRecord {
    /// Durable receipt for a delivered join reply: the joiner's own fields
    /// and one digest over the whole reply. The roster, revocations, rows
    /// and governance messages are the room's state at delivery; a replay
    /// rebuilds them from the room and answers only while the digest still
    /// matches. Keeping them here made every receipt as large as the room,
    /// and the room record grow with the square of its members.
    fn receipt(
        remote_endpoint: [u8; 32],
        request_id: [u8; 32],
        proof_digest: [u8; 32],
        delivered: &JoinBootstrap,
    ) -> Result<Self, JoinError> {
        let mut marker = JOIN_RECEIPT_TAG.to_vec();
        marker.extend_from_slice(&bootstrap_digest(delivered)?);
        Ok(Self {
            remote_endpoint,
            request_id,
            proof_digest,
            bootstrap: JoinBootstrap {
                certified_roster: Vec::new(),
                revocations: Vec::new(),
                records: Vec::new(),
                governance_messages: vec![marker],
                ..delivered.clone()
            },
        })
    }

    fn delivered_digest(&self) -> Option<&[u8]> {
        match self.bootstrap.governance_messages.as_slice() {
            [only] => only
                .strip_prefix(JOIN_RECEIPT_TAG)
                .filter(|digest| digest.len() == 32),
            _ => None,
        }
    }

    /// The exact reply this completion delivered, rebuilt from the room and
    /// answered only while it is unchanged. Once the room has moved on, the
    /// member refreshes through the membership challenge.
    fn replay(&self, room: &Governed<'_>) -> Result<JoinBootstrap, JoinError> {
        let delivered = self
            .delivered_digest()
            .ok_or_else(|| JoinError::new("join completion receipt is malformed"))?;
        let rebuilt = JoinBootstrap {
            certified_roster: room.peer.certificates(),
            revocations: room.peer.revocations(),
            records: room.peer.rows(),
            governance_messages: room.governance_messages.governance_messages(),
            ..self.bootstrap.clone()
        };
        if bootstrap_digest(&rebuilt)?.as_slice() != delivered {
            return Err(JoinError::refresh());
        }
        Ok(rebuilt)
    }
}

fn bootstrap_digest(bootstrap: &JoinBootstrap) -> Result<[u8; 32], JoinError> {
    let hasher = postcard::to_io(
        bootstrap,
        blake3::Hasher::new_derive_key("tilo join completion receipt v1"),
    )
    .map_err(|error| JoinError::new(error.to_string()))?;
    Ok(*hasher.finalize().as_bytes())
}

/// The idempotency digest of a request.
///
/// # Errors
///
/// The request cannot be encoded.
pub fn request_digest(value: &impl Serialize) -> Result<[u8; 32], JoinError> {
    postcard::to_stdvec(value)
        .map(|bytes| *blake3::hash(&bytes).as_bytes())
        .map_err(|error| JoinError::new(error.to_string()))
}

/// The room an invitation names, read without trusting it: the room it
/// names is then verified against what that room issued.
///
/// # Errors
///
/// The invitation does not decode or verify.
pub fn invitation_room(signed_invitation: &[u8], now_ns: u64) -> Result<String, JoinError> {
    let join =
        decode_and_verify_invitation(signed_invitation, None, &RevocationList::new(), now_ns)?;
    Ok(join.grant().room.clone())
}

/// What happened to an administration request asked before.
pub enum Administered {
    /// The same request was committed in this generation.
    Done(u64),
    /// Not asked before.
    New,
}

fn system_time(now_ns: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH
        .checked_add(web_time::Duration::from_nanos(now_ns))
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

impl JoinLedger {
    /// Whether the ledger keeps nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }

    /// Whether a device redeemed the invitation `invite_id`.
    #[must_use]
    pub fn redeemed(&self, invite_id: [u8; 32]) -> bool {
        self.join_completions
            .iter()
            .any(|completion| completion.bootstrap.invite_id == invite_id)
    }

    /// The certified device admitted by the committed request receipt.
    #[must_use]
    pub fn admitted_device(&self, request_id: [u8; 32]) -> Option<[u8; 32]> {
        self.join_completions
            .iter()
            .find(|completion| completion.request_id == request_id)
            .map(|completion| completion.bootstrap.device_cert.device_public_key)
    }

    /// Drop what the room keeps about how `people` joined: their
    /// invitations, join receipts and pending membership challenges. An
    /// invitation whose records are gone is refused as not issued here, so
    /// it still cannot be redeemed again.
    pub fn forget(&mut self, people: &BTreeSet<[u8; 32]>) {
        self.withdraw_invitations(people);
        self.join_completions.retain(|completion| {
            !people.contains(&completion.bootstrap.device_cert.user_public_key)
        });
    }

    /// Take back every way in the room handed `people`: the invitations
    /// they redeemed or that are addressed to them, and their pending
    /// membership challenges. Their join receipts stay, so a removed
    /// person asking after their request hears it declined, and a replay
    /// of it is still refused by the room's governance.
    pub fn withdraw_invitations(&mut self, people: &BTreeSet<[u8; 32]>) {
        let theirs = |device: &DeviceCert| people.contains(&device.user_public_key);
        let redeemed: BTreeSet<[u8; 32]> = self
            .join_completions
            .iter()
            .filter(|completion| theirs(&completion.bootstrap.device_cert))
            .map(|completion| completion.bootstrap.invite_id)
            .collect();
        self.invitations.retain(|record| {
            !redeemed.contains(&record.issued.invite_id)
                && !record
                    .grant
                    .recipient_user_public_key
                    .is_some_and(|recipient| people.contains(&recipient))
        });
        self.membership_challenges
            .retain(|record| !theirs(&record.device_cert));
    }

    /// Record an invitation `request` asks of `room`, signed by `signer` (the
    /// room's governor) and reached through `transport`.
    ///
    /// # Errors
    ///
    /// The request ID names a different request, the request is invalid or
    /// the invitation cannot be signed.
    pub fn issue(
        &mut self,
        room: &Governed<'_>,
        administrator: &DeviceCert,
        request: IssueInvitationRequest,
        transport: Vec<TransportHint>,
        signer: Signer<'_>,
        now: u64,
    ) -> Result<IssuedInvitation, JoinError> {
        let Signer { key, nonce } = signer;
        let digest = request_digest(&(
            &request.role,
            &request.recipient_user_public_key,
            &request.invitee_name,
            request.expires_at_ns,
            &request.inviter_name,
        ))?;
        if let Some(record) = self
            .invitations
            .iter()
            .find(|record| record.request_id == request.request_id)
        {
            if record.request_digest != digest {
                return Err(JoinError::new(
                    "invitation request ID already names a different request",
                ));
            }
            return Ok(record.issued.clone());
        }
        validate_issue_request(&request, now)?;
        let authority = room.peer.authority().clone();
        let issuer = DeviceCert::clone(&authority);
        let grant = InviteGrant {
            workspace_id: room.workspace_id,
            room: room.peer.room().to_owned(),
            authority,
            issuer,
            delegation: None,
            role: request.role,
            nonce,
            recipient_user_public_key: request.recipient_user_public_key,
            invitee_name: request.invitee_name,
            expires_at_ns: request.expires_at_ns,
            transport,
        };
        let signed_invitation = match request.inviter_name {
            Some(name) => issue_invitation_from(
                grant.clone(),
                Inviter {
                    user_public_key: administrator.user_public_key,
                    name,
                },
                key,
            )?,
            None => issue_invitation(grant.clone(), key)?,
        };
        let answer = IssuedInvitation {
            invite_id: *blake3::hash(&signed_invitation).as_bytes(),
            signed_invitation,
        };
        self.invitations.push(InvitationRecord {
            request_id: request.request_id,
            request_digest: digest,
            issued: answer.clone(),
            grant,
            cancellation: None,
        });
        Ok(answer)
    }

    /// Cancel an unredeemed invitation. Answers whether the ledger changed;
    /// the same cancellation asked again changes nothing.
    ///
    /// # Errors
    ///
    /// The request ID names a different request, or the invitation does not
    /// exist or was cancelled by another request.
    pub fn cancel(&mut self, request: &CancelInvitationRequest) -> Result<bool, JoinError> {
        let digest = request_digest(request)?;
        if let Some(prior) = self
            .invitations
            .iter()
            .filter_map(|record| record.cancellation.as_ref())
            .find(|cancelled| cancelled.request_id == request.request_id)
        {
            if prior.request_digest != digest {
                return Err(JoinError::new(
                    "invitation cancellation request ID already names a different request",
                ));
            }
            return Ok(false);
        }
        let record = self
            .invitations
            .iter_mut()
            .find(|record| record.issued.invite_id == request.invite_id)
            .ok_or_else(|| JoinError::new("invitation does not exist"))?;
        if record.cancellation.is_some() {
            return Err(JoinError::new(
                "invitation was already cancelled by a different request",
            ));
        }
        record.cancellation = Some(CancellationRecord {
            request_id: request.request_id,
            request_digest: digest,
        });
        Ok(true)
    }

    /// The invitations still redeemable at `now`, by invite id.
    #[must_use]
    pub fn pending(&self, now: u64) -> Vec<PendingInvitation> {
        let mut invitations = self
            .invitations
            .iter()
            .filter(|record| {
                record.cancellation.is_none()
                    && record.grant.expires_at_ns > now
                    && !self.redeemed(record.issued.invite_id)
            })
            .map(|record| PendingInvitation {
                invite_id: record.issued.invite_id,
                role: record.grant.role,
                recipient_user_public_key: record.grant.recipient_user_public_key,
                invitee_name: record.grant.invitee_name.clone(),
                expires_at_ns: record.grant.expires_at_ns,
            })
            .collect::<Vec<_>>();
        invitations.sort_by_key(|invitation| invitation.invite_id);
        invitations
    }

    /// The invitation `signed_invitation`, verified against what this room
    /// issued and has not cancelled.
    ///
    /// # Errors
    ///
    /// The room did not issue it, cancelled it, or it does not verify.
    pub fn inspect(
        &self,
        room: &Governed<'_>,
        signed_invitation: &[u8],
        now: u64,
    ) -> Result<VerifiedJoin, JoinError> {
        let invite_id = *blake3::hash(signed_invitation).as_bytes();
        let record = self.issued(invite_id)?;
        if record.issued.signed_invitation != signed_invitation {
            return Err(JoinError::new("invitation bytes do not match issuance"));
        }
        if record.cancellation.is_some() {
            return Err(JoinError::new("invitation was cancelled"));
        }
        Ok(decode_and_verify_invitation(
            signed_invitation,
            None,
            &room.revocations()?,
            now,
        )?)
    }

    /// Record a challenge for `request` from the device at
    /// `remote_endpoint`, unwritten. The same request asked again while its
    /// challenge is open answers that challenge.
    pub fn challenge(
        &mut self,
        room: &Governed<'_>,
        remote_endpoint: [u8; 32],
        request: JoinChallengeRequest,
        nonce: [u8; 32],
        now: u64,
    ) -> Result<JoinChallenge, JoinError> {
        if remote_endpoint != request.device_cert.device_public_key {
            return Err(JoinError::new(
                "join certificate does not match authenticated transport endpoint",
            ));
        }
        let digest = request_digest(&(remote_endpoint, &request))?;
        if let Some(prior) = self.join_challenges.iter().find(|prior| {
            prior.challenge.request_id == request.request_id && prior.challenge.expires_at_ns > now
        }) {
            if prior.request_digest != digest {
                return Err(JoinError::new(
                    "join request ID already names a different challenge",
                ));
            }
            return Ok(prior.challenge.clone());
        }
        let verified = self.inspect(room, &request.signed_invitation, now)?;
        if verified
            .grant()
            .recipient_user_public_key
            .is_some_and(|recipient| recipient != request.device_cert.user_public_key)
        {
            return Err(JoinError::new(
                "invitation recipient does not match joining identity",
            ));
        }
        let expires_at_ns = verified
            .grant()
            .expires_at_ns
            .min(now.saturating_add(CHALLENGE_LIFETIME_NS));
        let challenge = JoinChallenge::new(
            &verified,
            &request.device_cert,
            request.request_id,
            nonce,
            expires_at_ns,
            now,
        )?;
        self.join_challenges
            .retain(|record| record.challenge.expires_at_ns > now);
        if self.join_challenges.len() >= MAX_PENDING_CHALLENGES {
            return Err(JoinError::new("pending join challenge limit reached"));
        }
        self.join_challenges.push(JoinChallengeRecord {
            remote_endpoint,
            request_digest: digest,
            device_cert: request.device_cert,
            challenge: challenge.clone(),
        });
        Ok(challenge)
    }

    /// The answer to a Complete this room admitted before: `None` when it
    /// never completed request `request_id`.
    ///
    /// # Errors
    ///
    /// The request completed with a different proof or from another
    /// endpoint, its device is no longer a member, or the room moved on
    /// or the certificate it admitted ran out since
    /// ([`crate::join::JOIN_COMPLETED_REFRESH`]).
    pub fn completed(
        &self,
        room: &Governed<'_>,
        remote_endpoint: [u8; 32],
        request_id: [u8; 32],
        proof_digest: [u8; 32],
        now: u64,
    ) -> Result<Option<JoinBootstrap>, JoinError> {
        let Some(prior) = self
            .join_completions
            .iter()
            .find(|prior| prior.request_id == request_id)
        else {
            return Ok(None);
        };
        if prior.remote_endpoint != remote_endpoint || prior.proof_digest != proof_digest {
            return Err(JoinError::new(
                "join request ID already completed with different proof",
            ));
        }
        let device = prior.bootstrap.device_cert.device_public_key;
        if let Err(error) = room.peer.permissions(device, system_time(now)) {
            // The certificate this request was admitted with ran out since.
            // The device is the member it admitted, and proves it again
            // with its renewal through the membership exchange, which puts
            // the renewal in the room. Refused here, a retained proof would
            // be sent again and refused for good.
            if room.peer.certificate_expired(&device, system_time(now)) {
                return Err(JoinError::refresh());
            }
            return Err(error.into());
        }
        prior.replay(room).map(Some)
    }

    /// Verify `proof` against the challenge this room issued to the device
    /// at `remote_endpoint`, for an invitation still redeemable.
    ///
    /// # Errors
    ///
    /// The challenge was not issued here or lapsed
    /// ([`crate::join::JOIN_PROOF_LAPSED`]), the proof is not the endpoint's
    /// or does not verify, or the invitation is no longer redeemable.
    pub fn redeem(
        &self,
        room: &Governed<'_>,
        remote_endpoint: [u8; 32],
        proof: &JoinProof,
        now: u64,
    ) -> Result<VerifiedRedemption, JoinError> {
        // A completed request is answered before it is redeemed again, so
        // a proof whose challenge is gone or past was never admitted here
        // and never will be: the joining device needs a new challenge.
        let challenge = self
            .join_challenges
            .iter()
            .find(|prior| prior.challenge == proof.challenge)
            .ok_or_else(|| JoinError::lapsed("join challenge was not issued by this host"))?;
        if challenge.challenge.expires_at_ns <= now {
            return Err(JoinError::lapsed(
                "join challenge expired before the join completed",
            ));
        }
        if challenge.remote_endpoint != remote_endpoint
            || challenge.device_cert.device_public_key != remote_endpoint
        {
            return Err(JoinError::new(
                "join proof does not match authenticated transport endpoint",
            ));
        }
        let redemption = verify_join_proof(
            proof,
            &challenge.challenge,
            &challenge.device_cert,
            &room.revocations()?,
            now,
        )?;
        self.redeemable(redemption.join().invite_id())?;
        Ok(redemption)
    }

    /// The record of invitation `invite_id`, kept until it is redeemed.
    fn issued(&self, invite_id: [u8; 32]) -> Result<&InvitationRecord, JoinError> {
        self.invitations
            .iter()
            .find(|record| record.issued.invite_id == invite_id)
            .ok_or_else(|| {
                JoinError::new(if self.redeemed(invite_id) {
                    "invitation was already redeemed"
                } else {
                    "invitation was not issued by this host"
                })
            })
    }

    /// Whether invitation `invite_id` may still be redeemed.
    ///
    /// # Errors
    ///
    /// It was not issued here, was redeemed or was cancelled.
    pub fn redeemable(&self, invite_id: [u8; 32]) -> Result<(), JoinError> {
        if self.issued(invite_id)?.cancellation.is_some() {
            return Err(JoinError::new("invitation was cancelled"));
        }
        Ok(())
    }

    /// Admit the verified `joins` into `candidate`, the room's next state,
    /// signed by the governor's `key`. Each is checked on its own and
    /// refused alone: a refused admission leaves the others' candidate as it
    /// was. Answers each join's refusal, in order.
    pub fn admit(
        &self,
        candidate: &mut StatePeer,
        joins: &[Redeeming<'_>],
        key: &SigningKey,
        now: u64,
    ) -> Vec<Option<JoinError>> {
        joins
            .iter()
            .map(|join| {
                let redemption = join.redemption;
                self.redeemable(redemption.join().invite_id())
                    .and_then(|()| {
                        let mut next = candidate.clone();
                        next.governance_mut().admit_invited_member(
                            redemption,
                            key,
                            system_time(now),
                        )?;
                        *candidate = next;
                        Ok(())
                    })
                    .err()
            })
            .collect()
    }

    /// Stage the outcome of [`Self::admit`] once `room` is the committed
    /// state that admitted them: the receipt of each admitted join, which
    /// answers for it from now on, with the challenge it proved and the
    /// invitation it spent let go. Answers each join's bootstrap or refusal,
    /// in order.
    ///
    /// # Errors
    ///
    /// A receipt cannot be encoded.
    pub fn complete(
        &mut self,
        room: &Governed<'_>,
        joins: &[Redeeming<'_>],
        refusals: &[Option<JoinError>],
    ) -> Result<Vec<Result<JoinBootstrap, JoinError>>, JoinError> {
        let mut delivered = Vec::with_capacity(joins.len());
        for (join, refused) in joins.iter().zip(refusals) {
            if let Some(refused) = refused {
                delivered.push(Err(refused.clone()));
                continue;
            }
            // The challenge it proved and the invitation it spent admit
            // nobody else, and keeping them made every later write of the
            // room carry each past join.
            let invite_id = join.redemption.join().invite_id();
            self.join_challenges.retain(|record| {
                record.challenge.request_id != join.request_id
                    || record.remote_endpoint != join.remote_endpoint
            });
            self.invitations
                .retain(|record| record.issued.invite_id != invite_id);
            let bootstrap = room.bootstrap(
                invite_id,
                join.redemption.certificate().clone(),
                join.redemption.join().grant().role,
            );
            self.join_completions.push(JoinCompletionRecord::receipt(
                join.remote_endpoint,
                join.request_id,
                join.proof_digest,
                &bootstrap,
            )?);
            delivered.push(Ok(bootstrap));
        }
        Ok(delivered)
    }

    /// Record a membership challenge for `request` from the device at
    /// `remote_endpoint`, unwritten. The same request asked again while its
    /// challenge is open answers that challenge.
    pub fn membership_challenge(
        &mut self,
        room: &Governed<'_>,
        remote_endpoint: [u8; 32],
        request: MembershipChallengeRequest,
        nonce: [u8; 32],
        now: u64,
    ) -> Result<MembershipChallenge, JoinError> {
        if remote_endpoint != request.device_cert.device_public_key
            || request.workspace_id != room.workspace_id
        {
            return Err(JoinError::new(
                "membership request does not match authenticated workspace endpoint",
            ));
        }
        let digest = request_digest(&(remote_endpoint, &request))?;
        if let Some(prior) = self.membership_challenges.iter().find(|prior| {
            prior.challenge.request_id == request.request_id && prior.challenge.expires_at_ns > now
        }) {
            if prior.request_digest != digest {
                return Err(JoinError::new(
                    "membership request ID already names a different challenge",
                ));
            }
            return Ok(prior.challenge.clone());
        }
        let challenge = room.peer.governance().membership_challenge(
            room.workspace_id,
            &request.device_cert,
            request.request_id,
            nonce,
            now.saturating_add(CHALLENGE_LIFETIME_NS),
            system_time(now),
        )?;
        self.membership_challenges
            .retain(|record| record.challenge.expires_at_ns > now);
        if self.membership_challenges.len() >= MAX_PENDING_CHALLENGES {
            return Err(JoinError::new("pending membership challenge limit reached"));
        }
        self.membership_challenges.push(MembershipChallengeRecord {
            remote_endpoint,
            request_digest: digest,
            device_cert: request.device_cert,
            challenge: challenge.clone(),
        });
        Ok(challenge)
    }

    /// The renewed certificate a member proved its membership with, which
    /// the governor puts in place of the one the room holds
    /// ([`crate::PeerGovernanceMut::renew_member_certificate`]) before it
    /// answers [`Self::membership_complete`]. `None` when the room holds the
    /// certificate the member proved with.
    ///
    /// # Errors
    ///
    /// As [`Self::membership_complete`], or the certificate does not renew
    /// the one the room holds ([`crate::PeerGovernance::renewal_of`]).
    pub fn membership_renewal(
        &self,
        room: &Governed<'_>,
        remote_endpoint: [u8; 32],
        request: &MembershipCompleteRequest,
        now: u64,
    ) -> Result<Option<DeviceCert>, JoinError> {
        let (issued, _) = self.proven_membership(room, remote_endpoint, request, now)?;
        Ok(room
            .peer
            .governance()
            .renewal_of(&issued.device_cert, system_time(now))?
            .map(|_| issued.device_cert.clone()))
    }

    /// The challenge `request` proves, checked against the endpoint that
    /// sent it and the room's revocations.
    fn proven_membership(
        &self,
        room: &Governed<'_>,
        remote_endpoint: [u8; 32],
        request: &MembershipCompleteRequest,
        now: u64,
    ) -> Result<(&MembershipChallengeRecord, crate::join::VerifiedMembership), JoinError> {
        let issued = self
            .membership_challenges
            .iter()
            .find(|prior| prior.challenge == request.proof.challenge)
            .ok_or_else(|| JoinError::new("membership challenge was not issued by this host"))?;
        if issued.remote_endpoint != remote_endpoint
            || issued.device_cert.device_public_key != remote_endpoint
        {
            return Err(JoinError::new(
                "membership proof does not match authenticated transport endpoint",
            ));
        }
        let membership = verify_membership_proof(
            &request.proof,
            &issued.challenge,
            &issued.device_cert,
            &room.revocations()?,
            now,
        )?;
        Ok((issued, membership))
    }

    /// The room's current bootstrap for a member that proved its membership
    /// against a challenge this room issued.
    pub fn membership_complete(
        &self,
        room: &Governed<'_>,
        remote_endpoint: [u8; 32],
        request: &MembershipCompleteRequest,
        now: u64,
    ) -> Result<JoinBootstrap, JoinError> {
        let (issued, membership) = self.proven_membership(room, remote_endpoint, request, now)?;
        let role = room
            .peer
            .governance()
            .verify_member_refresh(&membership, system_time(now))?;
        let invite_id = self
            .join_completions
            .iter()
            .find(|completion| {
                completion.bootstrap.device_cert.device_public_key == remote_endpoint
            })
            .map_or([0; 32], |completion| completion.bootstrap.invite_id);
        Ok(room.bootstrap(invite_id, issued.device_cert.clone(), role))
    }

    /// Whether administration request `request_id` was committed before.
    ///
    /// # Errors
    ///
    /// The request ID names a different operation.
    pub fn administered(
        &self,
        request_id: [u8; 32],
        digest: [u8; 32],
    ) -> Result<Administered, JoinError> {
        match self
            .administration
            .iter()
            .find(|prior| prior.request_id == request_id)
        {
            Some(prior) if prior.request_digest != digest => Err(JoinError::new(
                "administration request ID already names a different operation",
            )),
            Some(prior) => Ok(Administered::Done(prior.generation)),
            None => Ok(Administered::New),
        }
    }

    /// Record that administration request `request_id` commits in
    /// `generation`, unwritten.
    pub fn record_administration(
        &mut self,
        request_id: [u8; 32],
        digest: [u8; 32],
        generation: u64,
    ) {
        self.administration.push(AdministrationRecord {
            request_id,
            request_digest: digest,
            generation,
        });
    }
}

fn validate_issue_request(request: &IssueInvitationRequest, now: u64) -> Result<(), JoinError> {
    if request.request_id == [0; 32] {
        return Err(JoinError::new("invitation request ID cannot be zero"));
    }
    if request.expires_at_ns <= now
        || request.expires_at_ns > now.saturating_add(MAX_INVITATION_LIFETIME_NS)
    {
        return Err(JoinError::new("invitation lifetime is invalid"));
    }
    if let Some(name) = request.invitee_name.as_deref()
        && let Err(error) = tilo_user_identity::display_name(name)
    {
        return Err(JoinError::new(format!("invitee name: {error}")));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
