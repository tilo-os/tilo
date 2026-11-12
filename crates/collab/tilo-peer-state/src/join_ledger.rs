//! What a workspace's governor records between the steps of a join, on every
//! host that governs workspaces: a relay, a cloud host or the device that made
//! the workspace. One ledger, so a workspace is joined the same way wherever it is
//! hosted.
//!
//! The cryptographic and governed-document contract is [`crate::join`]'s
//! and [`crate::governance`]'s. The ledger keeps the invitations a workspace
//! issued, the challenges it handed out, the receipts of completed joins and
//! of administration requests, with the request idempotency each exchange
//! needs. It never writes: the host stages a change here, makes the workspace
//! durable with it, and only then answers.
//!
//! Nothing here draws randomness or reads a clock; the host passes nonces
//! and `now`, so the ledger runs wherever the workspace engine does.

use crate::replica::PeerGuard;
use std::collections::BTreeSet;
use std::fmt;
use std::ops::Deref;

use web_time::SystemTime;

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
    issue_invitation, issue_invitation_from, verify_connection_redemption, verify_join_proof,
    verify_membership_proof,
};

/// Whether a device joining under this certificate names its person as it
/// comes in, or goes by a name of its own.
///
/// A device of the person's own says what they are called here
/// ([`crate::PeerGovernanceMut::name_person`]). A key made to act for them —
/// an agent's grant, a server key a backend holds, a key minted for one
/// workspace — names itself: its label is the key's ("Jane Doe's agent"),
/// and a key never names the person it acts for.
fn names_its_person(cert: &DeviceCert) -> bool {
    cert.capabilities.agent.is_none() && !cert.capabilities.stand_in
}

/// Pending challenges of one kind a workspace keeps at once.
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

impl From<crate::state::MembershipError> for JoinError {
    /// A joining device the workspace does not admit is refused in the words its
    /// membership refusal already has.
    fn from(error: crate::state::MembershipError) -> Self {
        Self::new(error.to_string())
    }
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

    /// A Complete this host admitted before, whose device takes the workspace's
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

/// The workspace's admitted state, as a ledger reads it: handed over by the
/// device that holds it, or held out of the engine's lock for one decision.
pub enum PeerRef<'a> {
    /// A replica the caller holds.
    Borrowed(&'a StatePeer),
    /// A replica behind the engine's lock, held while the ledger reads it.
    Held(PeerGuard<'a>),
}

impl Deref for PeerRef<'_> {
    type Target = StatePeer;

    fn deref(&self) -> &StatePeer {
        match self {
            Self::Borrowed(peer) => peer,
            Self::Held(peer) => peer,
        }
    }
}

impl<'a> From<&'a StatePeer> for PeerRef<'a> {
    fn from(peer: &'a StatePeer) -> Self {
        Self::Borrowed(peer)
    }
}

impl<'a> From<PeerGuard<'a>> for PeerRef<'a> {
    fn from(peer: PeerGuard<'a>) -> Self {
        Self::Held(peer)
    }
}

/// The governed workspace a ledger answers for, as committed now.
pub struct Governed<'a> {
    /// The workspace's admitted state.
    pub peer: PeerRef<'a>,
    /// The device answering the join: a device of the workspace's owner.
    pub host: &'a DeviceCert,
    /// The workspace's relay shard on the wire
    /// ([`crate::join::room_workspace_id`]).
    pub workspace_id: u64,
    /// The workspace's committed generation.
    pub generation: u64,
    /// Where a governor keeps the signed governance a joining member admits,
    /// oldest first, when it keeps them apart from the replica; a governor
    /// without one answers with its replica's own workspace document.
    pub governance_messages: Option<&'a dyn GovernanceMessages>,
}

/// Where a governor keeps the signed governance a joining member admits.
pub trait GovernanceMessages {
    /// That governance, oldest first.
    fn governance_messages(&self) -> Result<Vec<Vec<u8>>, String>;
}

impl<F: Fn() -> Result<Vec<Vec<u8>>, String>> GovernanceMessages for F {
    fn governance_messages(&self) -> Result<Vec<Vec<u8>>, String> {
        self()
    }
}

/// A workspace's governance is the workspace's own committed document, as one
/// message: a joining device admits governance alone, and what a host retains
/// of its record publications builds on a document the newcomer does not hold.
impl GovernanceMessages for crate::StatePeer {
    fn governance_messages(&self) -> Result<Vec<Vec<u8>>, String> {
        postcard::to_stdvec(&self.workspace_message())
            .map(|bytes| vec![bytes])
            .map_err(|error| format!("workspace governance does not encode: {error}"))
    }
}

impl Governed<'_> {
    /// The bootstrap a device admitted as `role` with `invite_id` receives.
    pub fn bootstrap(
        &self,
        invite_id: [u8; 32],
        device_cert: DeviceCert,
        role: MemberRole,
    ) -> Result<JoinBootstrap, JoinError> {
        self.filled(Self::admission(self, invite_id, device_cert, role))
    }

    /// What a bootstrap says of the one device it admits, with the workspace's
    /// state it carries (roster, revocations, records, governance) left out:
    /// as large as the workspace, so built only for a device it is sent to.
    fn admission(
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
            room: self.peer.workspace_id().to_owned(),
            workspace_id: self.workspace_id,
            host: self.host.clone(),
            certified_roster: Vec::new(),
            revocations: Vec::new(),
            records: Vec::new(),
            governance_messages: Vec::new(),
            generation: self.generation,
        }
    }

    /// `admitted`, one device's admission, with the workspace's state as it
    /// stands now: the bootstrap that device receives.
    ///
    /// # Errors
    ///
    /// The workspace's governance does not encode.
    pub fn filled(&self, admitted: JoinBootstrap) -> Result<JoinBootstrap, JoinError> {
        let messages = match self.governance_messages {
            Some(messages) => messages.governance_messages(),
            None => self.peer.governance_messages(),
        }
        .map_err(JoinError::new)?;
        Ok(JoinBootstrap {
            certified_roster: self.peer.certificates(),
            revocations: self.peer.revocations(),
            records: self.peer.rows(),
            governance_messages: messages,
            ..admitted
        })
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
    pub key: &'a dyn collab_types::signer::DeviceSigner,
    pub nonce: [u8; 16],
}

/// A verified redemption to admit, and what its receipt answers for.
pub struct Redeeming<'a> {
    pub remote_endpoint: [u8; 32],
    pub request_id: [u8; 32],
    pub proof_digest: [u8; 32],
    pub redemption: &'a VerifiedRedemption,
    /// How the person names themself as they join: the name the workspace
    /// shows for them ([`crate::PeerGovernanceMut::name_person`]).
    /// `None` keeps the name their invitation and certificate give.
    pub name: Option<&'a str>,
}

/// What a workspace's governor keeps between the steps of its joins. Its bytes
/// are part of the host's workspace record (postcard on a relay): field order is
/// the format, append only.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinLedger {
    pub invitations: Vec<InvitationRecord>,
    pub join_challenges: Vec<JoinChallengeRecord>,
    pub join_completions: Vec<JoinCompletionRecord>,
    pub membership_challenges: Vec<MembershipChallengeRecord>,
    pub administration: Vec<AdministrationRecord>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvitationRecord {
    pub request_id: [u8; 32],
    pub request_digest: [u8; 32],
    pub issued: IssuedInvitation,
    pub grant: InviteGrant,
    pub cancellation: Option<CancellationRecord>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancellationRecord {
    pub request_id: [u8; 32],
    pub request_digest: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinChallengeRecord {
    pub remote_endpoint: [u8; 32],
    pub request_digest: [u8; 32],
    pub device_cert: DeviceCert,
    pub challenge: JoinChallenge,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinCompletionRecord {
    pub remote_endpoint: [u8; 32],
    pub request_id: [u8; 32],
    pub proof_digest: [u8; 32],
    /// The delivered bootstrap with the workspace's state it carried left out
    /// (see [`JoinCompletionRecord::receipt`]).
    pub bootstrap: JoinBootstrap,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MembershipChallengeRecord {
    pub remote_endpoint: [u8; 32],
    pub request_digest: [u8; 32],
    pub device_cert: DeviceCert,
    pub challenge: MembershipChallenge,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdministrationRecord {
    pub request_id: [u8; 32],
    pub request_digest: [u8; 32],
    pub generation: u64,
}

impl JoinCompletionRecord {
    /// Durable receipt for a delivered join reply: the joiner's own fields
    /// and the generation it was delivered at. The roster, revocations, rows
    /// and governance messages are the workspace's state at delivery; a replay
    /// rebuilds them from the workspace while it is still at that generation,
    /// which every governance change moves on. Keeping them here made every
    /// receipt as large as the workspace, and building them only to hash them
    /// made every admission cost the whole workspace, for a reply the joiner
    /// mostly never asks again.
    fn receipt(
        remote_endpoint: [u8; 32],
        request_id: [u8; 32],
        proof_digest: [u8; 32],
        admitted: JoinBootstrap,
    ) -> Self {
        Self {
            remote_endpoint,
            request_id,
            proof_digest,
            bootstrap: admitted,
        }
    }

    /// The reply this completion delivered, rebuilt from the workspace and
    /// answered only while its governance is unchanged. Once the workspace has
    /// moved on, the member refreshes through the membership challenge.
    fn replay(&self, workspace: &Governed<'_>) -> Result<JoinBootstrap, JoinError> {
        if workspace.generation != self.bootstrap.generation {
            return Err(JoinError::refresh());
        }
        workspace.filled(self.bootstrap.clone())
    }
}

/// The idempotency digest of a request.
pub fn request_digest(value: &impl Serialize) -> Result<[u8; 32], JoinError> {
    postcard::to_stdvec(value)
        .map(|bytes| *blake3::hash(&bytes).as_bytes())
        .map_err(|error| JoinError::new(error.to_string()))
}

/// The workspace an invitation names, read without trusting it: the workspace it
/// names is then verified against what that workspace issued.
pub fn invitation_workspace(signed_invitation: &[u8], now_ns: u64) -> Result<String, JoinError> {
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

    /// Drop what the workspace keeps about how `people` joined: their
    /// invitations, join receipts and pending membership challenges. An
    /// invitation whose records are gone is refused as not issued here, so
    /// it still cannot be redeemed again.
    pub fn forget(&mut self, people: &BTreeSet<[u8; 32]>) {
        self.withdraw_invitations(people);
        self.join_completions.retain(|completion| {
            !people.contains(&completion.bootstrap.device_cert.user_public_key)
        });
    }

    /// Take back every way in the workspace handed `people`: the invitations
    /// they redeemed or that are addressed to them, and their pending
    /// membership challenges. Their join receipts stay, so a removed
    /// person asking after their request hears it declined, and a replay
    /// of it is still refused by the workspace's governance.
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

    /// Record an invitation `request` asks of `workspace`, signed by `signer` (the
    /// workspace's governor) and reached through `transport`.
    pub fn issue(
        &mut self,
        workspace: &Governed<'_>,
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
        let grant = InviteGrant {
            workspace_id: workspace.workspace_id,
            room: workspace.peer.workspace_id().to_owned(),
            issuer: workspace.host.clone(),
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

    /// The person who issued the invitation `invite_id`, as the ledger
    /// recorded them.
    #[must_use]
    pub fn issuer_of(&self, invite_id: [u8; 32]) -> Option<[u8; 32]> {
        self.invitations
            .iter()
            .find(|record| record.issued.invite_id == invite_id)
            .map(|record| record.grant.issuer.user_public_key)
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

    /// The signed invitation still redeemable at `now` that names only
    /// `person`, as the workspace issued it: what a workspace that takes
    /// people by invitation answers its door with when the person it
    /// invited asks. `None` while no such invitation stands.
    ///
    /// The invitation the workspace's own inviter made is the whole answer:
    /// its role and lifetime are the inviter's decision, and a door handing
    /// it back mints nothing.
    #[must_use]
    pub fn invitation_for(&self, person: [u8; 32], now: u64) -> Option<IssuedInvitation> {
        self.invitations
            .iter()
            .filter(|record| {
                record.cancellation.is_none()
                    && record.grant.expires_at_ns > now
                    && record.grant.recipient_user_public_key == Some(person)
                    && !self.redeemed(record.issued.invite_id)
            })
            .max_by_key(|record| record.grant.expires_at_ns)
            .map(|record| record.issued.clone())
    }

    /// The invitation `signed_invitation`, verified against what this workspace
    /// issued and has not cancelled.
    pub fn inspect(
        &self,
        workspace: &Governed<'_>,
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
            &workspace.revocations()?,
            now,
        )?)
    }

    /// Record a challenge for `request` from the device at
    /// `remote_endpoint`, unwritten. The same request asked again while its
    /// challenge is open answers that challenge.
    pub fn challenge(
        &mut self,
        workspace: &Governed<'_>,
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
        let verified = self.inspect(workspace, &request.signed_invitation, now)?;
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

    /// The answer to a Complete this workspace admitted before: `None` when it
    /// never completed request `request_id`.
    ///
    /// # Errors
    ///
    /// The request completed with a different proof or from another
    /// endpoint, its device is no longer a member, or the workspace moved on
    /// or the certificate it admitted ran out since
    /// ([`crate::join::JOIN_COMPLETED_REFRESH`]).
    pub fn completed(
        &self,
        workspace: &Governed<'_>,
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
        if let Err(error) = workspace.peer.permissions(device, system_time(now)) {
            // The certificate this request was admitted with ran out since.
            // The device is the member it admitted, and proves it again
            // with its renewal through the membership exchange, which puts
            // the renewal in the workspace. Refused here, a retained proof would
            // be sent again and refused for good.
            if workspace
                .peer
                .certificate_expired(&device, system_time(now))
            {
                return Err(JoinError::refresh());
            }
            return Err(JoinError::new(error.to_string()));
        }
        prior.replay(workspace).map(Some)
    }

    /// Verify `proof` against the challenge this workspace issued to the device
    /// at `remote_endpoint`, for an invitation still redeemable.
    ///
    /// # Errors
    ///
    /// The challenge was not issued here or lapsed
    /// ([`crate::join::JOIN_PROOF_LAPSED`]), the proof is not the endpoint's
    /// or does not verify, or the invitation is no longer redeemable.
    pub fn redeem(
        &self,
        workspace: &Governed<'_>,
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
            &workspace.revocations()?,
            now,
        )?;
        self.redeemable(redemption.join().invite_id())?;
        Ok(redemption)
    }

    /// Verify the redemption of `signed_invitation` by the device `cert`
    /// certifies, whose host checked it holds its key on the connection that
    /// carries the redemption ([`verify_connection_redemption`]): no
    /// challenge of this workspace's stands in between. The invitation must
    /// be this workspace's and still redeemable.
    ///
    /// # Errors
    ///
    /// The certificate or the invitation does not verify, the invitation is
    /// another workspace's, or it was spent or cancelled.
    pub fn redeem_on_connection(
        &self,
        workspace: &Governed<'_>,
        signed_invitation: &[u8],
        cert: &DeviceCert,
        request_id: [u8; 32],
        now: u64,
    ) -> Result<VerifiedRedemption, JoinError> {
        let redemption = verify_connection_redemption(
            signed_invitation,
            cert,
            request_id,
            &workspace.revocations()?,
            now,
        )?;
        if redemption.join().grant().room != workspace.peer.workspace_id()
            || redemption.join().grant().workspace_id != workspace.workspace_id
        {
            return Err(JoinError::new("invitation is not for this workspace"));
        }
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
    pub fn redeemable(&self, invite_id: [u8; 32]) -> Result<(), JoinError> {
        if self.issued(invite_id)?.cancellation.is_some() {
            return Err(JoinError::new("invitation was cancelled"));
        }
        Ok(())
    }

    /// Admit the verified `joins` into `candidate`, the workspace's next state,
    /// signed by the governor's `key`. Each is checked on its own and
    /// refused alone: a refused admission leaves the others' candidate as it
    /// was. Answers each join's refusal, in order.
    pub fn admit(
        &self,
        candidate: &mut StatePeer,
        joins: &[Redeeming<'_>],
        key: &dyn collab_types::signer::DeviceSigner,
        now: u64,
    ) -> Vec<Option<JoinError>> {
        joins
            .iter()
            .map(|join| {
                let redemption = join.redemption;
                self.redeemable(redemption.join().invite_id())
                    .and_then(|()| {
                        let mut next = candidate.clone();
                        next.governance_mut().admit_named_member(
                            redemption,
                            join.name
                                .filter(|_| names_its_person(redemption.certificate())),
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

    /// Stage the outcome of [`Self::admit`] once `workspace` is the committed
    /// state that admitted them: the receipt of each admitted join, which
    /// answers for it from now on, with the challenge it proved and the
    /// invitation it spent let go. Answers each join's admission or refusal,
    /// in order: its bootstrap without the workspace's state, which
    /// [`Governed::filled`] adds for a device the bootstrap is sent to.
    pub fn complete(
        &mut self,
        workspace: &Governed<'_>,
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
            // workspace carry each past join.
            let invite_id = join.redemption.join().invite_id();
            self.join_challenges.retain(|record| {
                record.challenge.request_id != join.request_id
                    || record.remote_endpoint != join.remote_endpoint
            });
            self.invitations
                .retain(|record| record.issued.invite_id != invite_id);
            let admitted = workspace.admission(
                invite_id,
                join.redemption.certificate().clone(),
                join.redemption.join().grant().role,
            );
            self.join_completions.push(JoinCompletionRecord::receipt(
                join.remote_endpoint,
                join.request_id,
                join.proof_digest,
                admitted.clone(),
            ));
            delivered.push(Ok(admitted));
        }
        Ok(delivered)
    }

    /// Record a membership challenge for `request` from the device at
    /// `remote_endpoint`, unwritten. The same request asked again while its
    /// challenge is open answers that challenge.
    pub fn membership_challenge(
        &mut self,
        workspace: &Governed<'_>,
        remote_endpoint: [u8; 32],
        request: MembershipChallengeRequest,
        nonce: [u8; 32],
        now: u64,
    ) -> Result<MembershipChallenge, JoinError> {
        if remote_endpoint != request.device_cert.device_public_key
            || request.workspace_id != workspace.workspace_id
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
        let challenge = workspace.peer.governance().membership_challenge(
            workspace.workspace_id,
            workspace.host,
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
    /// the governor puts in place of the one the workspace holds
    /// ([`crate::PeerGovernanceMut::renew_member_certificate`]) before it
    /// answers [`Self::membership_complete`]. `None` when the workspace holds the
    /// certificate the member proved with.
    ///
    /// # Errors
    ///
    /// As [`Self::membership_complete`], or the certificate does not renew
    /// the one the workspace holds ([`crate::PeerGovernance::renewal_of`]).
    pub fn membership_renewal(
        &self,
        workspace: &Governed<'_>,
        remote_endpoint: [u8; 32],
        request: &MembershipCompleteRequest,
        now: u64,
    ) -> Result<Option<DeviceCert>, JoinError> {
        let (issued, _) = self.proven_membership(workspace, remote_endpoint, request, now)?;
        Ok(workspace
            .peer
            .governance()
            .renewal_of(&issued.device_cert, system_time(now))?
            .map(|_| issued.device_cert.clone()))
    }

    /// The challenge `request` proves, checked against the endpoint that
    /// sent it and the workspace's revocations.
    fn proven_membership(
        &self,
        workspace: &Governed<'_>,
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
            &workspace.revocations()?,
            now,
        )?;
        Ok((issued, membership))
    }

    /// The workspace's current bootstrap for a member that proved its membership
    /// against a challenge this workspace issued.
    pub fn membership_complete(
        &self,
        workspace: &Governed<'_>,
        remote_endpoint: [u8; 32],
        request: &MembershipCompleteRequest,
        now: u64,
    ) -> Result<JoinBootstrap, JoinError> {
        let (issued, membership) =
            self.proven_membership(workspace, remote_endpoint, request, now)?;
        let role = workspace
            .peer
            .governance()
            .member_role(&membership, system_time(now))?;
        let invite_id = self
            .join_completions
            .iter()
            .find(|completion| {
                completion.bootstrap.device_cert.device_public_key == remote_endpoint
            })
            .map_or([0; 32], |completion| completion.bootstrap.invite_id);
        workspace.bootstrap(invite_id, issued.device_cert.clone(), role)
    }

    /// Whether administration request `request_id` was committed before.
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
