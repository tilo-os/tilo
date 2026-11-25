//! What a workspace's host records between the steps of a join, on every
//! host that serves workspaces: a server, a cloud host or the device that made
//! the workspace. One log, so a workspace is joined the same way wherever it is
//! hosted.
//!
//! The cryptographic and member-list contract is [`crate::join`]'s
//! and [`crate::governance`]'s. The log keeps the invitations a workspace
//! issued, the challenges it handed out, the receipts of completed joins and
//! of administration requests, with the request idempotency each exchange
//! needs. It never writes: the host stages a change here, makes the workspace
//! durable with it, and only then answers.
//!
//! Nothing here draws randomness or reads a clock; the host passes nonces
//! and `now`, so the log runs wherever the workspace engine does.

use crate::replica::PeerGuard;
use std::collections::BTreeSet;
use std::fmt;
use std::ops::Deref;

use web_time::SystemTime;

use serde::{Deserialize, Serialize};
use tilo_user_identity::{DeviceCert, RevocationList};

use crate::StatePeer;
use crate::join::WorkspaceId;
use crate::join::invitations::{
    CancelInvitationRequest, IssueInvitationRequest, IssuedInvitation, PendingInvitation,
};
use crate::join::{
    InviteGrant, Inviter, JoinBootstrap, JoinChallenge, JoinChallengeRequest, JoinProof,
    MembershipChallenge, MembershipChallengeRequest, MembershipCompleteRequest, Role,
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
pub(crate) fn names_its_person(cert: &DeviceCert) -> bool {
    cert.capabilities.agent.is_none() && !cert.capabilities.stand_in
}

/// Pending challenges of one kind a workspace keeps at once.
const MAX_PENDING_CHALLENGES: usize = 4096;
/// How long a join or membership challenge may be answered.
pub const CHALLENGE_LIFETIME_NS: u64 = 60_000_000_000;
/// The longest an invitation may stay redeemable.
const MAX_INVITATION_LIFETIME_NS: u64 = 30 * 24 * 60 * 60 * 1_000_000_000;

/// Why the host refused or failed a request.
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

    /// A refusal carrying `code`, as another answered it ([`Self::code`]).
    pub fn coded(message: impl Into<String>, code: u16) -> Self {
        Self {
            message: message.into(),
            code,
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

/// The workspace's admitted state, as a log reads it: handed over by the
/// device that holds it, or held out of the engine's lock for one decision.
pub enum PeerRef<'a> {
    /// A replica the caller holds.
    Borrowed(&'a StatePeer),
    /// A replica behind the engine's lock, held while the log reads it.
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

/// The workspace a log answers for, as committed now.
pub struct Listed<'a> {
    /// The workspace's admitted state.
    pub peer: PeerRef<'a>,
    /// The device answering the join: a key holder of the workspace.
    pub host: &'a DeviceCert,
    /// The workspace the log answers for.
    pub workspace_id: WorkspaceId,
    /// The workspace's committed generation.
    pub generation: u64,
    /// Where a host keeps the signed governance a joining member admits,
    /// oldest first, when it keeps it apart from the workspace document; a host
    /// without one answers with the document itself.
    pub governance_messages: Option<&'a dyn GovernanceMessages>,
}

/// Where a host keeps the signed governance a joining member admits.
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

impl Listed<'_> {
    /// The bootstrap a device admitted as `role` with `invite_id` receives.
    pub fn bootstrap(
        &self,
        invite_id: [u8; 32],
        device_cert: DeviceCert,
        role: Role,
    ) -> Result<JoinBootstrap, JoinError> {
        self.filled(Self::admission(self, invite_id, device_cert, role))
    }

    /// What a bootstrap says of the one device it admits, with the workspace's
    /// state it carries (roster, revocations, records, governance) left out:
    /// as large as the workspace, so built only for a device it is sent to.
    fn admission(&self, invite_id: [u8; 32], device_cert: DeviceCert, role: Role) -> JoinBootstrap {
        JoinBootstrap {
            version: 1,
            invite_id,
            device_cert,
            role,
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
        let messages = self.governance()?;
        Ok(JoinBootstrap {
            certified_roster: self.peer.certificates(),
            revocations: self.peer.revocations(),
            records: self.peer.rows(),
            governance_messages: messages,
            ..admitted
        })
    }

    /// The workspace's governance as it stands now, as the messages a device
    /// takes it in by: what a joining device writes its own admission on top
    /// of ([`crate::own_admission`]), and what a bootstrap carries.
    ///
    /// # Errors
    ///
    /// The workspace's governance does not encode.
    pub fn governance(&self) -> Result<Vec<Vec<u8>>, JoinError> {
        match self.governance_messages {
            Some(messages) => messages.governance_messages(),
            None => self.peer.governance_messages(),
        }
        .map_err(JoinError::new)
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

/// The host's key signing an invitation, and the nonce that makes this
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
    /// The redeeming device's own admission, as the workspace changes it
    /// signed ([`crate::own_admission::sign_own_admission`]), the name it
    /// chose among them: the answering device passes them on unchanged.
    pub admission: &'a [Vec<u8>],
}

/// What a workspace's host keeps between the steps of its joins. Its bytes
/// are part of the host's workspace record (postcard on a server): field order is
/// the format, append only.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinLog {
    pub invitations: Vec<InvitationEntry>,
    pub join_challenges: Vec<JoinChallengeRecord>,
    pub join_completions: Vec<JoinCompletionRecord>,
    pub membership_challenges: Vec<MembershipChallengeRecord>,
    pub administration: Vec<AdministrationRecord>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvitationEntry {
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
    fn replay(&self, workspace: &Listed<'_>) -> Result<JoinBootstrap, JoinError> {
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
pub fn invitation_workspace(
    signed_invitation: &[u8],
    now_ns: u64,
) -> Result<WorkspaceId, JoinError> {
    let join =
        decode_and_verify_invitation(signed_invitation, None, &RevocationList::new(), now_ns)?;
    Ok(join.grant().workspace_id)
}

/// The device key that issued `signed_invitation`, read once its signature
/// holds: whose grant decides what the invitation admits.
pub fn invitation_issuer(signed_invitation: &[u8], now_ns: u64) -> Result<[u8; 32], JoinError> {
    let join =
        decode_and_verify_invitation(signed_invitation, None, &RevocationList::new(), now_ns)?;
    Ok(join.grant().issuer.device_public_key)
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

/// What a workspace's join log says of an invitation: whether it still keeps
/// it, and whether someone took it back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Offer {
    /// Issued here, and nobody took it back.
    Kept,
    /// Issued here, then taken back.
    Withdrawn,
    /// One someone already redeemed.
    Redeemed,
    /// One this workspace never issued.
    Unissued,
}

impl Offer {
    /// What a log says of an invitation it keeps (`kept`, and whether it was
    /// taken back) and one it redeemed (`redeemed`).
    #[must_use]
    pub const fn of(kept: Option<bool>, redeemed: bool) -> Self {
        match kept {
            Some(false) => Self::Kept,
            Some(true) => Self::Withdrawn,
            None if redeemed => Self::Redeemed,
            None => Self::Unissued,
        }
    }
}

/// The refusal an invitation someone already redeemed reads.
const REDEEMED: &str = "invitation was already redeemed";
/// The refusal an invitation this workspace never issued reads.
const UNISSUED: &str = "invitation was not issued by this host";
/// The refusal an invitation someone took back reads.
const WITHDRAWN: &str = "invitation was cancelled";

/// The invitation `signed_invitation` offers as `offer` and `revocations`
/// leave it: what its holder may read, or the refusal a workspace not
/// honouring it answers with.
///
/// # Errors
///
/// The invitation was taken back, redeemed, never issued here, or its own
/// signed layout does not verify.
pub fn honourable(
    offer: Offer,
    revocations: &RevocationList,
    signed_invitation: &[u8],
    now: u64,
) -> Result<VerifiedJoin, JoinError> {
    match offer {
        Offer::Kept => Ok(decode_and_verify_invitation(
            signed_invitation,
            None,
            revocations,
            now,
        )?),
        Offer::Withdrawn => Err(JoinError::new(WITHDRAWN)),
        Offer::Redeemed => Err(JoinError::new(REDEEMED)),
        Offer::Unissued => Err(JoinError::new(UNISSUED)),
    }
}

impl JoinLog {
    /// Whether the log keeps nothing.
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

    /// Record the completion of an entry admission: `device_cert` came in
    /// under the entry `entry` names, at `role`, answered at the endpoint its
    /// own key is, for `request_id`. No invitation was spent, so the
    /// completion names the entry's digest where an invitation's id goes, and
    /// the same request asked again hears this answer.
    ///
    /// # Errors
    ///
    /// The request does not encode.
    pub fn completed_entry(
        &mut self,
        hosted: &Listed<'_>,
        device_cert: &DeviceCert,
        request_id: [u8; 32],
        entry: [u8; 32],
        role: Role,
    ) -> Result<JoinBootstrap, JoinError> {
        let admitted = hosted.admission(entry, device_cert.clone(), role);
        let digest = request_digest(&(device_cert.device_public_key, request_id))?;
        self.join_completions.push(JoinCompletionRecord::receipt(
            device_cert.device_public_key,
            request_id,
            digest,
            admitted.clone(),
        ));
        Ok(admitted)
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
    /// workspace's host) and reached through `transport`.
    pub fn issue(
        &mut self,
        workspace: &Listed<'_>,
        administrator: &DeviceCert,
        request: IssueInvitationRequest,
        transport: Vec<TransportHint>,
        signer: Signer<'_>,
        now: u64,
    ) -> Result<IssuedInvitation, JoinError> {
        self.issue_as(
            workspace,
            administrator,
            request,
            false,
            transport,
            signer,
            now,
        )
    }

    /// [`Self::issue`] of an invitation that, when `makes_administrator`,
    /// also makes its holder an administrator: an Editor's invitation in the
    /// layout that says so ([`crate::join::issue_administrator_invitation`]),
    /// naming `administrator`'s person as whom it is from. Whoever admits the
    /// holder checks that person may make administrators. The invitation
    /// names the device that signs it, so only a device the workspace lets
    /// invite issues one: a holder that is no member never speaks for
    /// another person.
    ///
    /// # Errors
    ///
    /// As [`Self::issue`], or `signer`'s device may not invite with the
    /// request's role.
    pub fn issue_as(
        &mut self,
        workspace: &Listed<'_>,
        administrator: &DeviceCert,
        request: IssueInvitationRequest,
        makes_administrator: bool,
        transport: Vec<TransportHint>,
        signer: Signer<'_>,
        now: u64,
    ) -> Result<IssuedInvitation, JoinError> {
        let Signer { key, nonce } = signer;
        if makes_administrator && request.role != Role::Editor {
            return Err(JoinError::new("an administrator joins as an Editor"));
        }
        let at = system_time(now);
        workspace.peer.require_member_writer(key, at)?;
        if makes_administrator {
            workspace.peer.require_administrator_invitation(key, at)?;
        } else {
            workspace
                .peer
                .require_invitation_authority(key, request.role, at)?;
        }
        let terms = (
            &request.role,
            &request.recipient_user_public_key,
            &request.invitee_name,
            request.expires_at_ns,
            &request.inviter_name,
        );
        // An administrator's request is another request than the same
        // terms without it; every other request keeps the digest it had.
        let digest = if makes_administrator {
            request_digest(&(terms, "administrator"))?
        } else {
            request_digest(&terms)?
        };
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
            issuer: workspace.host.clone(),
            role: request.role,
            nonce,
            recipient_user_public_key: request.recipient_user_public_key,
            invitee_name: request.invitee_name,
            expires_at_ns: request.expires_at_ns,
            transport,
        };
        let signed_invitation = match request.inviter_name {
            _ if makes_administrator => crate::join::issue_administrator_invitation(
                grant.clone(),
                // The person who asked, under the name they give: whom it is
                // from is what an admitting member checks, and that is the
                // key, never a name. Where no name is known they are signed
                // unnamed, and a surface showing the invitation names them
                // its own way, from their profile or in plain words. A key of
                // theirs is no name of theirs to write here. Only the device
                // that issues the invitation for the workspace itself asks
                // for nobody: the issuer asking for itself is nobody's name.
                // Any other device is a person's, also one of the person the
                // issuer belongs to: their own key asking names them.
                (administrator.device_public_key != key.device_public_key()).then(|| Inviter {
                    user_public_key: administrator.user_public_key,
                    name: request.inviter_name.clone().unwrap_or_default(),
                }),
                key,
            )?,
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
        self.invitations.push(InvitationEntry {
            request_id: request.request_id,
            request_digest: digest,
            issued: answer.clone(),
            grant,
            cancellation: None,
        });
        Ok(answer)
    }

    /// Cancel an unredeemed invitation. Answers whether the log changed;
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

    /// The person who issued the invitation `invite_id`, as the log
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
                administrator: crate::join::makes_administrator(&record.issued.signed_invitation),
            })
            .collect::<Vec<_>>();
        invitations.sort_by_key(|invitation| invitation.invite_id);
        invitations
    }

    /// Invitation `invite_id` as it was signed, and when it lapses, while it
    /// may still be redeemed at `now`: what its issuer hands out again
    /// rather than making another.
    #[must_use]
    pub fn open_invitation(
        &self,
        invite_id: [u8; 32],
        now: u64,
    ) -> Option<(IssuedInvitation, u64)> {
        self.invitations
            .iter()
            .find(|record| {
                record.issued.invite_id == invite_id
                    && record.cancellation.is_none()
                    && record.grant.expires_at_ns > now
                    && !self.redeemed(invite_id)
            })
            .map(|record| (record.issued.clone(), record.grant.expires_at_ns))
    }

    /// The signed invitation a web link's secret names (its bearer,
    /// [`crate::join::native::bearer_text`]), while it may still be redeemed
    /// at `now`: what the workspace answers whoever follows the link, before
    /// they are anyone it knows. Holding the secret is holding the
    /// invitation; redeeming it still checks for whom it was issued.
    #[must_use]
    pub fn invitation_by_bearer(&self, bearer: &[u8; 32], now: u64) -> Option<Vec<u8>> {
        let record = self
            .invitations
            .iter()
            .find(|record| crate::join::native::bears(&record.issued.signed_invitation, bearer))?;
        self.open_invitation(record.issued.invite_id, now)
            .map(|(issued, _)| issued.signed_invitation)
    }

    /// Open the exact invitation named by a native link for the dialling device.
    pub fn resolve_invitation(
        &self,
        workspace: &Listed<'_>,
        reference: &crate::join::native::InvitationRef,
        device: &DeviceCert,
        remote_endpoint: [u8; 32],
        now: u64,
    ) -> Result<Vec<u8>, JoinError> {
        let unavailable = || JoinError::new("the invitation is no longer available to this device");
        if reference.workspace != workspace.workspace_id
            || remote_endpoint != device.device_public_key
        {
            return Err(unavailable());
        }
        let (issued, _) = self
            .open_invitation(reference.invite_id, now)
            .ok_or_else(unavailable)?;
        if !reference.matches(&issued.signed_invitation) {
            return Err(unavailable());
        }
        let revocations = workspace.revocations()?;
        let invitation = decode_and_verify_invitation(
            &issued.signed_invitation,
            Some(device.user_public_key),
            &revocations,
            now,
        )
        .map_err(|_| unavailable())?;
        if invitation.grant().workspace_id != workspace.workspace_id
            || invitation.grant().issuer.device_public_key != workspace.host.device_public_key
            || revocations.refuses(device)
            || tilo_user_identity::verify_device_cert_at(device, now).is_err()
        {
            return Err(unavailable());
        }
        Ok(issued.signed_invitation)
    }

    /// The signed invitation still redeemable at `now` that names only
    /// `person`, as the workspace issued it: what a workspace that takes
    /// people by invitation answers its entry with when the person it
    /// invited asks. `None` while no such invitation stands.
    ///
    /// The invitation the workspace's own inviter made is the whole answer:
    /// its role and lifetime are the inviter's decision, and an action API handing
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

    /// What this log says of the invitation `signed_invitation`: whether it
    /// keeps it, and whether someone took it back.
    #[must_use]
    pub fn offer(&self, signed_invitation: &[u8]) -> Offer {
        let invite_id = *blake3::hash(signed_invitation).as_bytes();
        let kept = match self
            .invitations
            .iter()
            .find(|entry| entry.issued.invite_id == invite_id)
        {
            Some(entry) if entry.issued.signed_invitation != signed_invitation => None,
            Some(entry) => Some(entry.cancellation.is_some()),
            None => None,
        };
        Offer::of(kept, self.redeemed(invite_id))
    }

    /// The invitation `signed_invitation`, verified against what this workspace
    /// issued and has not cancelled.
    pub fn inspect(
        &self,
        workspace: &Listed<'_>,
        signed_invitation: &[u8],
        now: u64,
    ) -> Result<VerifiedJoin, JoinError> {
        honourable(
            self.offer(signed_invitation),
            &workspace.revocations()?,
            signed_invitation,
            now,
        )
    }

    /// Record a challenge for `request` from the device at
    /// `remote_endpoint`, unwritten. The same request asked again while its
    /// challenge is open answers that challenge.
    pub fn challenge(
        &mut self,
        workspace: &Listed<'_>,
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
        workspace: &Listed<'_>,
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
        workspace: &Listed<'_>,
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
        workspace: &Listed<'_>,
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
        if redemption.join().grant().workspace_id != workspace.workspace_id {
            return Err(JoinError::new("invitation is not for this workspace"));
        }
        self.redeemable(redemption.join().invite_id())?;
        Ok(redemption)
    }

    /// The record of invitation `invite_id`, kept until it is redeemed.
    fn issued(&self, invite_id: [u8; 32]) -> Result<&InvitationEntry, JoinError> {
        self.invitations
            .iter()
            .find(|record| record.issued.invite_id == invite_id)
            .ok_or_else(|| {
                JoinError::new(if self.redeemed(invite_id) {
                    REDEEMED
                } else {
                    UNISSUED
                })
            })
    }

    /// Whether invitation `invite_id` may still be redeemed.
    pub fn redeemable(&self, invite_id: [u8; 32]) -> Result<(), JoinError> {
        if self.issued(invite_id)?.cancellation.is_some() {
            return Err(JoinError::new(WITHDRAWN));
        }
        Ok(())
    }

    /// Admit the verified `joins` into `candidate`, the workspace's next state:
    /// each by the admission its device signed itself, taken in as that
    /// device's ([`StatePeer::admit_own_redemption`]). Each is checked on its
    /// own and refused alone: a refused admission leaves the others'
    /// candidate as it was. Answers each join's refusal, in order.
    pub fn admit(
        &self,
        candidate: &mut StatePeer,
        joins: &[Redeeming<'_>],
        now: u64,
    ) -> Vec<Option<JoinError>> {
        joins
            .iter()
            .map(|join| {
                self.redeemable(join.redemption.join().invite_id())
                    .and_then(|()| {
                        candidate
                            .admit_own_redemption(
                                join.redemption,
                                join.admission.to_vec(),
                                system_time(now),
                            )
                            .map_err(JoinError::new)
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
    /// [`Listed::filled`] adds for a device the bootstrap is sent to.
    pub fn complete(
        &mut self,
        workspace: &Listed<'_>,
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
    /// `remote_endpoint`, unwritten: the endpoint is the device's own key, or
    /// one it signed a live delegation for ([`membership_endpoint`]). The same
    /// request asked again while its challenge is open answers that challenge.
    pub fn membership_challenge(
        &mut self,
        workspace: &Listed<'_>,
        remote_endpoint: [u8; 32],
        request: MembershipChallengeRequest,
        nonce: [u8; 32],
        now: u64,
    ) -> Result<MembershipChallenge, JoinError> {
        if request.workspace_id != workspace.workspace_id {
            return Err(JoinError::new(
                "membership request does not match authenticated workspace endpoint",
            ));
        }
        membership_endpoint(&request, remote_endpoint, now)?;
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

    /// The challenge `request` proves, checked against the endpoint that
    /// sent it and the workspace's revocations.
    fn proven_membership(
        &self,
        workspace: &Listed<'_>,
        remote_endpoint: [u8; 32],
        request: &MembershipCompleteRequest,
        now: u64,
    ) -> Result<(&MembershipChallengeRecord, crate::join::VerifiedMembership), JoinError> {
        let issued = self
            .membership_challenges
            .iter()
            .find(|prior| prior.challenge == request.proof.challenge)
            .ok_or_else(|| JoinError::new("membership challenge was not issued by this host"))?;
        if issued.remote_endpoint != remote_endpoint {
            // The challenge was issued to the endpoint that proved it spoke
            // for the device when it asked ([`membership_endpoint`]); the
            // proof must come back from that same endpoint.
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

    /// The device that proved `request`, checked as
    /// [`Self::membership_complete`] checks it: what a further device's own
    /// row is taken in as before it is answered.
    ///
    /// # Errors
    ///
    /// As [`Self::membership_complete`].
    pub fn membership_device(
        &self,
        workspace: &Listed<'_>,
        remote_endpoint: [u8; 32],
        request: &MembershipCompleteRequest,
        now: u64,
    ) -> Result<DeviceCert, JoinError> {
        let (issued, _) = self.proven_membership(workspace, remote_endpoint, request, now)?;
        Ok(issued.device_cert.clone())
    }

    /// The workspace's current bootstrap for a member that proved its membership
    /// against a challenge this workspace issued.
    pub fn membership_complete(
        &self,
        workspace: &Listed<'_>,
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

/// Whether the endpoint that sent a membership request may speak for the
/// device the request certifies: its own key, or one that device signed a
/// delegation for, in force at `now` and naming exactly this endpoint.
fn membership_endpoint(
    request: &MembershipChallengeRequest,
    remote_endpoint: [u8; 32],
    now: u64,
) -> Result<(), JoinError> {
    let device = request.device_cert.device_public_key;
    if remote_endpoint == device {
        return Ok(());
    }
    let refused =
        || JoinError::new("membership request does not match authenticated workspace endpoint");
    let delegation = request.delegation.as_ref().ok_or_else(refused)?;
    if delegation.device_public_key != device {
        return Err(refused());
    }
    tilo_user_identity::verify_endpoint_delegation(delegation, &remote_endpoint, now)
        .map_err(|_| refused())
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
