//! Invitations, their redemption and member administration of a governed
//! room, with the request idempotency each exchange needs.
//!
//! The cryptographic and governed-document contract lives in
//! `tilo_peer_state::join`; this module owns what the room authority records
//! between the steps of an exchange and the durability barrier before each
//! answer.

use std::fmt;

use serde::{Deserialize, Serialize};
use tilo_delivery_protocol::workspace_join::WorkspaceJoinService;
use tilo_peer_state::join::{
    InviteGrant, Inviter, JoinBootstrap, JoinChallenge, JoinChallengeRequest,
    JoinChallengeResponse, JoinCompleteRequest, JoinInspection, JoinProof, JoinTransportRequest,
    JoinTransportResponse, MemberRole, MembershipChallenge, MembershipChallengeRequest,
    MembershipChallengeResponse, MembershipCompleteRequest, VerifiedJoin, VerifiedRedemption,
    decode_and_verify_invitation, issue_invitation, issue_invitation_from, verify_join_proof,
    verify_membership_proof,
};
use tilo_relay_session::rooms::{
    CancelInvitationRequest, IssueInvitationRequest, IssuedInvitation, PendingInvitation,
    RemoveMemberRequest, SetMemberRoleRequest,
};
use tilo_user_identity::{DeviceCert, RevocationList};

use tilo_peer_state::StatePeer;

use super::{GovernedRoom, RoomRecord, Rooms, system_time};

/// A change to who is in a room.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
enum Membership {
    Remove,
    Ban,
    Unban,
    /// No changes until this Unix millisecond; reading stays allowed.
    Restrict(u64),
    Unrestrict,
}

/// Pending challenges of one kind a room keeps at once.
const MAX_PENDING_CHALLENGES: usize = 4096;
/// How long a join or membership challenge may be answered.
const CHALLENGE_LIFETIME_NS: u64 = 60_000_000_000;
/// The longest an invitation may stay redeemable.
const MAX_INVITATION_LIFETIME_NS: u64 = 30 * 24 * 60 * 60 * 1_000_000_000;

/// Delegate one application's workspace authorship, with an idempotency key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantAppAuthorshipRequest {
    /// Caller-chosen request identifier.
    pub request_id: [u8; 32],
    /// The exact registered application identifier.
    pub app: String,
}

/// A committed governance change.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GovernanceCompletion {
    /// The room generation that carries it.
    pub generation: u64,
}

/// Why the room authority refused or failed a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollaborationError(String);

impl CollaborationError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for CollaborationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl std::error::Error for CollaborationError {}

impl From<String> for CollaborationError {
    fn from(message: String) -> Self {
        Self(message)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(super) struct CollaborationState {
    invitations: Vec<InvitationRecord>,
    join_challenges: Vec<JoinChallengeRecord>,
    join_completions: Vec<JoinCompletionRecord>,
    membership_challenges: Vec<MembershipChallengeRecord>,
    administration: Vec<AdministrationRecord>,
}

impl CollaborationState {
    /// Drop what the room keeps about how `people` joined: their
    /// invitations, join receipts and pending membership challenges. An
    /// invitation whose records are gone is refused as not issued here, so
    /// it still cannot be redeemed again.
    fn forget(&mut self, people: &std::collections::BTreeSet<[u8; 32]>) {
        let theirs = |device: &DeviceCert| people.contains(&device.user_public_key);
        let redeemed: std::collections::BTreeSet<[u8; 32]> = self
            .join_completions
            .iter()
            .filter(|completion| theirs(&completion.bootstrap.device_cert))
            .map(|completion| completion.bootstrap.invite_id)
            .collect();
        self.join_completions
            .retain(|completion| !theirs(&completion.bootstrap.device_cert));
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
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct InvitationRecord {
    request_id: [u8; 32],
    request_digest: [u8; 32],
    issued: IssuedInvitation,
    grant: InviteGrant,
    cancellation: Option<CancellationRecord>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct CancellationRecord {
    request_id: [u8; 32],
    request_digest: [u8; 32],
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct JoinChallengeRecord {
    remote_endpoint: [u8; 32],
    request_digest: [u8; 32],
    device_cert: DeviceCert,
    challenge: JoinChallenge,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct JoinCompletionRecord {
    remote_endpoint: [u8; 32],
    request_id: [u8; 32],
    proof_digest: [u8; 32],
    /// The delivered bootstrap with the room's state it carried left out and
    /// its governance messages replaced by one digest entry (see
    /// [`JoinCompletionRecord::receipt`]).
    bootstrap: JoinBootstrap,
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
    ) -> Result<Self, CollaborationError> {
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
    fn replay(&self, room: &GovernedRoom) -> Result<JoinBootstrap, CollaborationError> {
        let delivered = self
            .delivered_digest()
            .ok_or_else(|| CollaborationError::new("join completion receipt is malformed"))?;
        let rebuilt = JoinBootstrap {
            certified_roster: room.peer().certificates(),
            revocations: room.peer().revocations(),
            records: room.peer().rows(),
            governance_messages: room.record.governance_messages(),
            ..self.bootstrap.clone()
        };
        if bootstrap_digest(&rebuilt)?.as_slice() != delivered {
            return Err(CollaborationError::new(
                "join request ID already completed; refresh membership for the current workspace",
            ));
        }
        Ok(rebuilt)
    }
}

fn bootstrap_digest(bootstrap: &JoinBootstrap) -> Result<[u8; 32], CollaborationError> {
    let hasher = postcard::to_io(
        bootstrap,
        blake3::Hasher::new_derive_key("tilo join completion receipt v1"),
    )
    .map_err(|error| CollaborationError::new(error.to_string()))?;
    Ok(*hasher.finalize().as_bytes())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct MembershipChallengeRecord {
    remote_endpoint: [u8; 32],
    request_digest: [u8; 32],
    device_cert: DeviceCert,
    challenge: MembershipChallenge,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct AdministrationRecord {
    request_id: [u8; 32],
    request_digest: [u8; 32],
    generation: u64,
}

fn current_bootstrap(
    record: &RoomRecord,
    peer: &StatePeer,
    invite_id: [u8; 32],
    device_cert: DeviceCert,
    role: MemberRole,
) -> JoinBootstrap {
    JoinBootstrap {
        version: 1,
        invite_id,
        device_cert,
        role,
        room: peer.room().to_owned(),
        workspace_id: record.workspace_id,
        authority: peer.authority().clone(),
        certified_roster: peer.certificates(),
        revocations: peer.revocations(),
        records: peer.rows(),
        governance_messages: record.governance_messages(),
        generation: record.generation,
    }
}

impl Rooms {
    /// Issue an invitation to `room` for `administrator`, a device of this
    /// relay's own user.
    ///
    /// # Errors
    ///
    /// Returns why the invitation was refused or could not be recorded.
    pub async fn issue(
        &self,
        room: &str,
        administrator: &DeviceCert,
        request: IssueInvitationRequest,
    ) -> Result<IssuedInvitation, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        self.authorize(administrator, now, "invitation.issue")?;
        let digest = request_digest(&(
            &request.role,
            &request.recipient_user_public_key,
            &request.invitee_name,
            request.expires_at_ns,
            &request.inviter_name,
        ))?;
        let room = self.room(room).await?;
        let mut room = room.lock().await;
        if let Some(record) = room
            .collaboration()
            .invitations
            .iter()
            .find(|record| record.request_id == request.request_id)
        {
            if record.request_digest != digest {
                return Err(CollaborationError::new(
                    "invitation request ID already names a different request",
                ));
            }
            return Ok(record.issued.clone());
        }
        validate_issue_request(&request, now)?;
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).map_err(|error| CollaborationError::new(error.to_string()))?;
        let authority = room.peer().authority().clone();
        let grant = InviteGrant {
            workspace_id: room.workspace_id(),
            room: room.room().to_owned(),
            authority: authority.clone(),
            issuer: authority,
            delegation: None,
            role: request.role,
            nonce,
            recipient_user_public_key: request.recipient_user_public_key,
            invitee_name: request.invitee_name,
            expires_at_ns: request.expires_at_ns,
            transport: vec![self.transport_hint(&room)?],
        };
        let signed_invitation = match request.inviter_name {
            Some(name) => issue_invitation_from(
                grant.clone(),
                Inviter {
                    user_public_key: administrator.user_public_key,
                    name,
                },
                &self.inner.host.key,
            )?,
            None => issue_invitation(grant.clone(), &self.inner.host.key)?,
        };
        let issued = IssuedInvitation {
            invite_id: *blake3::hash(&signed_invitation).as_bytes(),
            signed_invitation,
        };
        let mut state = room.collaboration().clone();
        state.invitations.push(InvitationRecord {
            request_id: request.request_id,
            request_digest: digest,
            issued: issued.clone(),
            grant,
            cancellation: None,
        });
        room.commit_state(state)?;
        tracing::info!(
            room = %room.room(),
            invite_id = %hex::encode(issued.invite_id),
            role = ?request.role,
            outcome = "issued",
            "workspace invitation issued"
        );
        Ok(issued)
    }

    /// Cancel an unredeemed invitation of `room`.
    ///
    /// # Errors
    ///
    /// Returns why the cancellation was refused or could not be recorded.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the room stays locked until its decision is committed, published and answered"
    )]
    pub async fn cancel(
        &self,
        room: &str,
        administrator: &DeviceCert,
        request: CancelInvitationRequest,
    ) -> Result<(), CollaborationError> {
        self.authorize(
            administrator,
            tilo_clock::unix_now_ns(),
            "invitation.cancel",
        )?;
        let digest = request_digest(&request)?;
        let room = self.room(room).await?;
        let mut room = room.lock().await;
        let mut state = room.collaboration().clone();
        if let Some(prior) = state
            .invitations
            .iter()
            .filter_map(|record| record.cancellation.as_ref())
            .find(|cancelled| cancelled.request_id == request.request_id)
        {
            if prior.request_digest != digest {
                return Err(CollaborationError::new(
                    "invitation cancellation request ID already names a different request",
                ));
            }
            return Ok(());
        }
        let record = state
            .invitations
            .iter_mut()
            .find(|record| record.issued.invite_id == request.invite_id)
            .ok_or_else(|| CollaborationError::new("invitation does not exist"))?;
        if record.cancellation.is_some() {
            return Err(CollaborationError::new(
                "invitation was already cancelled by a different request",
            ));
        }
        record.cancellation = Some(CancellationRecord {
            request_id: request.request_id,
            request_digest: digest,
        });
        room.commit_state(state)?;
        Ok(())
    }

    /// The invitations of `room` still redeemable.
    ///
    /// # Errors
    ///
    /// Returns why the listing was refused.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the room stays locked until its decision is committed, published and answered"
    )]
    pub async fn pending(
        &self,
        room: &str,
        administrator: &DeviceCert,
    ) -> Result<Vec<PendingInvitation>, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        self.authorize(administrator, now, "invitation.pending")?;
        let room = self.room(room).await?;
        let room = room.lock().await;
        let redeemed = |invite_id: [u8; 32]| {
            room.collaboration()
                .join_completions
                .iter()
                .any(|completion| completion.bootstrap.invite_id == invite_id)
        };
        let mut invitations = room
            .collaboration()
            .invitations
            .iter()
            .filter(|record| {
                record.cancellation.is_none()
                    && record.grant.expires_at_ns > now
                    && !redeemed(record.issued.invite_id)
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
        Ok(invitations)
    }

    /// Admit `device`, a device of this relay's own user, to `room` as its
    /// owner. Returns whether governance changed.
    ///
    /// # Errors
    ///
    /// Returns why the device was refused or the change could not commit.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the room stays locked until its decision is committed, published and answered"
    )]
    pub async fn enter(&self, room: &str, device: &DeviceCert) -> Result<bool, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        self.authorize(device, now, "room.enter")?;
        let room = self.room(room).await?;
        let mut room = room.lock().await;
        let mut candidate = room.peer().clone();
        if !candidate.admit_certificate(device.clone(), &self.inner.host.key, system_time(now))? {
            return Ok(false);
        }
        let state = room.collaboration().clone();
        room.commit(candidate, &self.inner.host.key, system_time(now), |_, _| {
            Ok(state)
        })?;
        self.settle(&mut room).await?;
        Ok(true)
    }

    /// Change the role of the member `request.user_public_key` in `room`.
    ///
    /// # Errors
    ///
    /// Returns why the change was refused or could not commit.
    pub async fn set_member_role(
        &self,
        room: &str,
        administrator: &DeviceCert,
        request: SetMemberRoleRequest,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        self.authorize(administrator, now, "member.role.set")?;
        let room = self.room(room).await?;
        let mut room = room.lock().await;
        let key = self.inner.host.key.clone();
        self.administer(
            &mut room,
            request.request_id,
            request_digest(&request)?,
            |peer| {
                peer.set_member_role(
                    request.user_public_key,
                    request.role,
                    &key,
                    system_time(now),
                )
            },
            now,
        )
        .await
    }

    /// Write room grants the governor decided (room settings such as a
    /// role's Deny on a kind), keyed by their ids.
    ///
    /// # Errors
    ///
    /// Refuses non-administrators and grants for another room.
    pub async fn put_room_grants(
        &self,
        room: &str,
        administrator: &DeviceCert,
        request_id: [u8; 32],
        grants: Vec<tilo_action_policy::Grant>,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        self.authorize(administrator, now, "room.grants.put")?;
        let room = self.room(room).await?;
        let mut room = room.lock().await;
        let key = self.inner.host.key.clone();
        self.administer(
            &mut room,
            request_id,
            request_digest(&grants)?,
            |peer| peer.put_governed_grants(&grants, &key, system_time(now)),
            now,
        )
        .await
    }

    /// Make `user_public_key` an administrator of `room`: an editor holding
    /// every governance action, as the room's Owner role.
    ///
    /// # Errors
    ///
    /// Refuses non-administrators, the structural owner and people the
    /// room never admitted.
    pub async fn set_member_administrator(
        &self,
        room: &str,
        administrator: &DeviceCert,
        request_id: [u8; 32],
        user_public_key: [u8; 32],
    ) -> Result<GovernanceCompletion, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        self.authorize(administrator, now, "member.administrator.set")?;
        let room = self.room(room).await?;
        let mut room = room.lock().await;
        let key = self.inner.host.key.clone();
        self.administer(
            &mut room,
            request_id,
            request_digest(&("administrator", user_public_key))?,
            |peer| peer.set_member_administrator(user_public_key, &key, system_time(now)),
            now,
        )
        .await
    }

    /// Remove the member `request.user_public_key` and every device of theirs
    /// from `room`. The relay stops admitting them before this returns.
    ///
    /// # Errors
    ///
    /// Returns why the removal was refused or could not commit.
    pub async fn remove_member(
        &self,
        room: &str,
        administrator: &DeviceCert,
        request: RemoveMemberRequest,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        self.change_membership(room, administrator, request, Membership::Remove)
            .await
    }

    /// Remove the members `people` from `room` in one governance change and
    /// forget how they joined. A public room prunes guests who left this
    /// way: every join costs time in proportion to the room's members, so a
    /// room that kept every visitor would slow with each one, and one change
    /// per guest would hold the room as long again. A pruned guest joins
    /// again through a new invitation.
    ///
    /// # Errors
    ///
    /// Returns why the removal was refused or could not commit.
    pub async fn prune_members(
        &self,
        room: &str,
        administrator: &DeviceCert,
        request_id: [u8; 32],
        people: &std::collections::BTreeSet<[u8; 32]>,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        let room = self.room(room).await?;
        let mut room = room.lock().await;
        for person in people {
            let change = tilo_action_policy::Change::RemoveMember {
                person: hex::encode(person),
            };
            self.authorize_member_change(&room, administrator, &change, now)?;
        }
        let key = self.inner.host.key.clone();
        let completion = self
            .administer(
                &mut room,
                request_id,
                request_digest(people)?,
                |peer| {
                    let mut changed = false;
                    for person in people {
                        changed |= peer.remove_member(*person, &key, system_time(now))?;
                    }
                    Ok(changed)
                },
                now,
            )
            .await?;
        let mut state = room.collaboration().clone();
        state.forget(people);
        room.commit_state(state)?;
        tracing::info!(
            room = %room.room(),
            people = people.len(),
            "idle members pruned"
        );
        Ok(completion)
    }

    /// Remove `request.user_public_key` from `room`, when a member, and keep
    /// them out until [`Self::unban_member`]: a removal plus their
    /// `block:<person>` Deny.
    ///
    /// # Errors
    ///
    /// Returns why the ban was refused or could not commit.
    pub async fn ban_member(
        &self,
        room: &str,
        administrator: &DeviceCert,
        request: RemoveMemberRequest,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        self.change_membership(room, administrator, request, Membership::Ban)
            .await
    }

    /// Let the banned `request.user_public_key` be admitted to `room` again.
    ///
    /// # Errors
    ///
    /// Returns why the unban was refused or could not commit.
    pub async fn unban_member(
        &self,
        room: &str,
        administrator: &DeviceCert,
        request: RemoveMemberRequest,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        self.change_membership(room, administrator, request, Membership::Unban)
            .await
    }

    /// Stop `request.user_public_key` changing anything in `room` until
    /// `until_ms` (Unix milliseconds): the room's expiring
    /// `restrict:<person>` Deny, which lapses by itself and leaves reading
    /// alone. A newer restriction replaces an older one.
    ///
    /// # Errors
    ///
    /// Returns why the restriction was refused or could not commit.
    pub async fn restrict_member(
        &self,
        room: &str,
        administrator: &DeviceCert,
        request: RemoveMemberRequest,
        until_ms: u64,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        self.change_membership(room, administrator, request, Membership::Restrict(until_ms))
            .await
    }

    /// Lift `request.user_public_key`'s restriction in `room` before it
    /// lapses.
    ///
    /// # Errors
    ///
    /// Returns why lifting was refused or could not commit.
    pub async fn unrestrict_member(
        &self,
        room: &str,
        administrator: &DeviceCert,
        request: RemoveMemberRequest,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        self.change_membership(room, administrator, request, Membership::Unrestrict)
            .await
    }

    /// Remove, ban or unban, decided by who asks: a device of the relay's
    /// user governs its rooms, and any other member by the room's policy (a
    /// Moderator reaches members ranked below Moderator).
    async fn change_membership(
        &self,
        room: &str,
        administrator: &DeviceCert,
        request: RemoveMemberRequest,
        membership: Membership,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        let room = self.room(room).await?;
        let mut room = room.lock().await;
        let person = hex::encode(request.user_public_key);
        let change = match membership {
            Membership::Remove => tilo_action_policy::Change::RemoveMember {
                person: person.clone(),
            },
            Membership::Ban => tilo_action_policy::Change::Ban {
                person: person.clone(),
            },
            Membership::Unban => tilo_action_policy::Change::Unban {
                person: person.clone(),
            },
            Membership::Restrict(until_ms) => tilo_action_policy::Change::Restrict {
                person: person.clone(),
                until_ms,
            },
            Membership::Unrestrict => tilo_action_policy::Change::Unrestrict {
                person: person.clone(),
            },
        };
        let room_id = room.room().to_owned();
        let by = self.authorize_member_change(&room, administrator, &change, now)?;
        let key = self.inner.host.key.clone();
        let target = request.user_public_key;
        let completion = self
            .administer(
                &mut room,
                request.request_id,
                request_digest(&(membership, &request))?,
                |peer| match membership {
                    Membership::Remove => peer.remove_member(target, &key, system_time(now)),
                    Membership::Ban => peer.ban_member(target, &by, &key, system_time(now)),
                    Membership::Unban => peer.unban_member(target, &key, system_time(now)),
                    Membership::Restrict(until_ms) => peer.put_governed_grants(
                        &[tilo_action_policy::roles::restriction_grant(
                            &room_id,
                            &person,
                            until_ms,
                            tilo_action_policy::Subject::Person(by.clone()),
                        )],
                        &key,
                        system_time(now),
                    ),
                    Membership::Unrestrict => peer.remove_governed_grants(
                        &[tilo_action_policy::roles::restriction_grant_id(&person)],
                        &key,
                        system_time(now),
                    ),
                },
                now,
            )
            .await?;
        tracing::info!(
            room = %room.room(),
            %person,
            outcome = ?membership,
            "workspace membership change committed"
        );
        Ok(completion)
    }

    /// The person deciding `change`, when `device` may make it in `room`:
    /// the owner for a device of the relay's user, otherwise the member whose
    /// device it is, held to the room's policy.
    fn authorize_member_change(
        &self,
        room: &GovernedRoom,
        device: &DeviceCert,
        change: &tilo_action_policy::Change,
        now: u64,
    ) -> Result<String, CollaborationError> {
        let owner = hex::encode(room.peer().authority().user_public_key);
        if self.authorize(device, now, change.action()).is_ok() {
            if let tilo_action_policy::Change::RemoveMember { person }
            | tilo_action_policy::Change::Ban { person }
            | tilo_action_policy::Change::Restrict { person, .. } = change
                && *person == owner
            {
                return Err(CollaborationError::new("the owner cannot be removed"));
            }
            return Ok(owner);
        }
        if self.revocations().is_revoked(&device.device_public_key) {
            return Err(CollaborationError::new("device revoked"));
        }
        let member = room
            .peer()
            .permissions(device.device_public_key, system_time(now))
            .map_err(CollaborationError::new)?;
        let bound = tilo_action_policy::roles::foreign_device_bound();
        let authority = member.authority(&bound);
        let cx = tilo_action_policy::Context::at_time(system_time(now));
        member
            .policy
            .check(&member.person, &authority, change, &cx, false)
            .map_err(|refusal| CollaborationError::new(refusal.to_string()))?;
        Ok(member.person.clone())
    }

    /// Commit application authorship under the current owner's authority.
    ///
    /// # Errors
    /// Refuses non-administrators, stale admission, invalid applications and
    /// conflicting retries; returns only after governance is durable and published.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the room remains locked through the committed and published administration decision"
    )]
    pub async fn grant_app_authorship(
        &self,
        room: &str,
        administrator: &DeviceCert,
        request: GrantAppAuthorshipRequest,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        self.authorize(administrator, now, "app.authorship.grant")?;
        if request.request_id == [0; 32] {
            return Err(CollaborationError::new(
                "request identifier must be nonzero",
            ));
        }
        let command =
            tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::GrantAppAuthorship {
                app: request.app.clone(),
            };
        command.validate()?;
        let room = self.room(room).await?;
        let mut room = room.lock().await;
        // Validate retries against today's exact certificate, admission,
        // administration and governor custody through the same typed command.
        let key = self.inner.host.key.clone();
        room.peer().clone().apply_workspace_membership_command(
            administrator,
            &command,
            &key,
            system_time(now),
        )?;
        self.administer(
            &mut room,
            request.request_id,
            request_digest(&("app.authorship.grant", &request))?,
            |peer| {
                peer.apply_workspace_membership_command(
                    administrator,
                    &command,
                    &key,
                    system_time(now),
                )
            },
            now,
        )
        .await
    }

    /// Install a bot as a governed member of `room`, update its program, or
    /// renew its delegation ([`WorkspaceMembershipCommand::InstallBot`],
    /// [`WorkspaceMembershipCommand::RenewBot`]), as the room's
    /// administrator. Every member reads and enforces the result, so it is
    /// a governed change rather than this host's own state.
    ///
    /// # Errors
    /// Refuses non-administrators and anything else; the peer refuses an
    /// install or renewal its rules do not admit.
    ///
    /// [`WorkspaceMembershipCommand::InstallBot`]: tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::InstallBot
    /// [`WorkspaceMembershipCommand::RenewBot`]: tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::RenewBot
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the room remains locked through the committed and published administration decision"
    )]
    pub async fn govern_bot(
        &self,
        room: &str,
        administrator: &DeviceCert,
        command: tilo_peer_state::workspace_membership::WorkspaceMembershipCommand,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        use tilo_peer_state::workspace_membership::WorkspaceMembershipCommand as Command;
        if !matches!(command, Command::InstallBot(_) | Command::RenewBot(_)) {
            return Err(CollaborationError::new(
                "only installing or renewing a bot governs a bot",
            ));
        }
        let now = tilo_clock::unix_now_ns();
        self.authorize(administrator, now, "bot.govern")?;
        command.validate()?;
        // The command names itself: the same install or renewal sent again
        // is the same administration request.
        let digest = request_digest(&("bot.govern", &command))?;
        let room = self.room(room).await?;
        let mut room = room.lock().await;
        let key = self.inner.host.key.clone();
        room.peer().clone().apply_workspace_membership_command(
            administrator,
            &command,
            &key,
            system_time(now),
        )?;
        self.administer(
            &mut room,
            digest,
            digest,
            |peer| {
                peer.apply_workspace_membership_command(
                    administrator,
                    &command,
                    &key,
                    system_time(now),
                )
            },
            now,
        )
        .await
    }

    /// Installation `id` of `room` as its governance records it now: the
    /// live row, else the last withdrawn one.
    ///
    /// # Errors
    /// The room is not governed here.
    pub async fn installed_bot(
        &self,
        room: &str,
        id: &str,
    ) -> Result<Option<tilo_peer_state::InstalledBot>, CollaborationError> {
        let now = system_time(tilo_clock::unix_now_ns());
        Ok(self
            .room(room)
            .await?
            .lock()
            .await
            .peer()
            .installation(id, now))
    }

    /// Persist and publish an admitted person's own bounded terminal receipt.
    /// All room administration commands keep their separate owner-only checks.
    ///
    /// # Errors
    /// Rejects stale admission, impersonation, malformed or conflicting receipts,
    /// and failed durable publication.
    pub async fn record_member_effect_receipt(
        &self,
        room: &str,
        caller: &DeviceCert,
        invocation: uuid::Uuid,
        redacted: &[u8],
    ) -> Result<GovernanceCompletion, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        tilo_user_identity::verify_device_cert_at(caller, now)
            .map_err(|error| CollaborationError::new(error.to_string()))?;
        if self.revocations().is_revoked(&caller.device_public_key) {
            return Err(CollaborationError::new("device revoked"));
        }
        let room = self.room(room).await?;
        let mut room = room.lock().await;
        let mut candidate = room.peer().clone();
        let changed = candidate.apply_member_effect_receipt(
            caller,
            invocation,
            redacted,
            &self.inner.host.key,
            system_time(now),
        )?;
        let collaboration = room.collaboration().clone();
        let generation = if changed {
            room.commit(candidate, &self.inner.host.key, system_time(now), |_, _| {
                Ok(collaboration)
            })?
        } else {
            room.record.generation
        };
        self.settle(&mut room).await?;
        Ok(GovernanceCompletion { generation })
    }

    async fn administer(
        &self,
        room: &mut GovernedRoom,
        request_id: [u8; 32],
        digest: [u8; 32],
        change: impl FnOnce(&mut tilo_peer_state::StatePeer) -> Result<bool, String>,
        now: u64,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        if let Some(prior) = room
            .collaboration()
            .administration
            .iter()
            .find(|prior| prior.request_id == request_id)
        {
            if prior.request_digest != digest {
                return Err(CollaborationError::new(
                    "administration request ID already names a different operation",
                ));
            }
            return Ok(GovernanceCompletion {
                generation: prior.generation,
            });
        }
        let mut candidate = room.peer().clone();
        change(&mut candidate)?;
        let mut state = room.collaboration().clone();
        state.administration.push(AdministrationRecord {
            request_id,
            request_digest: digest,
            generation: room.record.generation.saturating_add(1),
        });
        let generation =
            room.commit(candidate, &self.inner.host.key, system_time(now), |_, _| {
                Ok(state)
            })?;
        self.settle(room).await?;
        Ok(GovernanceCompletion { generation })
    }

    /// Only devices of this relay's own user administer its rooms.
    pub(super) fn authorize(
        &self,
        device: &DeviceCert,
        now: u64,
        operation: &'static str,
    ) -> Result<(), CollaborationError> {
        let host = &self.inner.host.cert;
        // An agent is a device of the relay's user too, but it governs
        // nothing: it joins rooms only through its delegation.
        #[cfg(feature = "agents")]
        let agent = self
            .inner
            .host
            .agents
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(&device.device_public_key);
        #[cfg(not(feature = "agents"))]
        let agent = false;
        let result = if device.user_public_key != host.user_public_key {
            Err(CollaborationError::new(
                "room administration requires a device of the relay's user",
            ))
        } else if agent {
            Err(CollaborationError::new(
                "room administration requires a person's device, not an agent's",
            ))
        } else if self.revocations().is_revoked(&device.device_public_key) {
            Err(CollaborationError::new("device revoked"))
        } else {
            tilo_user_identity::verify_device_cert_at(device, now)
                .map_err(|error| CollaborationError::new(error.to_string()))
        };
        if let Err(reason) = &result {
            tracing::warn!(
                device = %hex::encode(device.device_public_key),
                operation,
                %reason,
                "room administration denied"
            );
        }
        result
    }

    pub(super) fn inspect(
        room: &GovernedRoom,
        signed_invitation: &[u8],
        now: u64,
    ) -> Result<VerifiedJoin, CollaborationError> {
        let invite_id = *blake3::hash(signed_invitation).as_bytes();
        let record = Self::issued(room, invite_id)?;
        if record.issued.signed_invitation != signed_invitation {
            return Err(CollaborationError::new(
                "invitation bytes do not match issuance",
            ));
        }
        if record.cancellation.is_some() {
            return Err(CollaborationError::new("invitation was cancelled"));
        }
        Ok(decode_and_verify_invitation(
            signed_invitation,
            None,
            &revocations(room)?,
            now,
        )?)
    }

    #[expect(
        clippy::significant_drop_tightening,
        reason = "the room stays locked until its decision is committed, published and answered"
    )]
    pub(super) async fn challenge(
        &self,
        remote_endpoint: [u8; 32],
        request: JoinChallengeRequest,
        now: u64,
    ) -> Result<JoinChallengeResponse, CollaborationError> {
        if remote_endpoint != request.device_cert.device_public_key {
            return Err(CollaborationError::new(
                "join certificate does not match authenticated transport endpoint",
            ));
        }
        let room = self
            .room(&invitation_room(&request.signed_invitation)?)
            .await?;
        let mut room = room.lock().await;
        let digest = request_digest(&(remote_endpoint, &request))?;
        if let Some(prior) = room.collaboration().join_challenges.iter().find(|prior| {
            prior.challenge.request_id == request.request_id && prior.challenge.expires_at_ns > now
        }) {
            if prior.request_digest != digest {
                return Err(CollaborationError::new(
                    "join request ID already names a different challenge",
                ));
            }
            return Ok(JoinChallengeResponse {
                challenge: prior.challenge.clone(),
            });
        }
        let verified = Self::inspect(&room, &request.signed_invitation, now)?;
        if verified
            .grant()
            .recipient_user_public_key
            .is_some_and(|recipient| recipient != request.device_cert.user_public_key)
        {
            return Err(CollaborationError::new(
                "invitation recipient does not match joining identity",
            ));
        }
        let mut nonce = [0; 32];
        getrandom::fill(&mut nonce).map_err(|error| CollaborationError::new(error.to_string()))?;
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
        let mut state = room.collaboration().clone();
        state
            .join_challenges
            .retain(|record| record.challenge.expires_at_ns > now);
        if state.join_challenges.len() >= MAX_PENDING_CHALLENGES {
            return Err(CollaborationError::new(
                "pending join challenge limit reached",
            ));
        }
        state.join_challenges.push(JoinChallengeRecord {
            remote_endpoint,
            request_digest: digest,
            device_cert: request.device_cert,
            challenge: challenge.clone(),
        });
        room.commit_state(state)?;
        Ok(JoinChallengeResponse { challenge })
    }

    #[expect(
        clippy::significant_drop_tightening,
        reason = "one locked admission: verify the proof, admit the device, record its receipt and publish, in order"
    )]
    async fn complete(
        &self,
        remote_endpoint: [u8; 32],
        request: JoinCompleteRequest,
        now: u64,
    ) -> Result<JoinBootstrap, CollaborationError> {
        let room = self.room(&request.proof.challenge.room).await?;
        let mut room = room.lock().await;
        let request_id = request.proof.challenge.request_id;
        let proof_digest = request_digest(&request.proof)?;
        if let Some(prior) = room
            .collaboration()
            .join_completions
            .iter()
            .find(|prior| prior.request_id == request_id)
        {
            if prior.remote_endpoint != remote_endpoint || prior.proof_digest != proof_digest {
                return Err(CollaborationError::new(
                    "join request ID already completed with different proof",
                ));
            }
            room.peer().permissions(
                prior.bootstrap.device_cert.device_public_key,
                system_time(now),
            )?;
            return prior.replay(&room);
        }
        let redemption = Self::redeem(&room, remote_endpoint, &request.proof, now)?;
        self.admit_redemption(
            &mut room,
            remote_endpoint,
            request_id,
            proof_digest,
            &redemption,
            now,
        )
        .await
    }

    /// Verify `proof` against the challenge `room` issued to the device at
    /// `remote_endpoint`, for an invitation still redeemable.
    pub(super) fn redeem(
        room: &GovernedRoom,
        remote_endpoint: [u8; 32],
        proof: &JoinProof,
        now: u64,
    ) -> Result<VerifiedRedemption, CollaborationError> {
        let challenge = room
            .collaboration()
            .join_challenges
            .iter()
            .find(|prior| prior.challenge == proof.challenge)
            .ok_or_else(|| CollaborationError::new("join challenge was not issued by this host"))?;
        if challenge.remote_endpoint != remote_endpoint
            || challenge.device_cert.device_public_key != remote_endpoint
        {
            return Err(CollaborationError::new(
                "join proof does not match authenticated transport endpoint",
            ));
        }
        let redemption = verify_join_proof(
            proof,
            &challenge.challenge,
            &challenge.device_cert,
            &revocations(room)?,
            now,
        )?;
        Self::redeemable(room, redemption.join().invite_id())?;
        Ok(redemption)
    }

    /// The record of invitation `invite_id`, which `room` keeps until the
    /// invitation is redeemed.
    fn issued(
        room: &GovernedRoom,
        invite_id: [u8; 32],
    ) -> Result<&InvitationRecord, CollaborationError> {
        let state = room.collaboration();
        state
            .invitations
            .iter()
            .find(|record| record.issued.invite_id == invite_id)
            .ok_or_else(|| {
                CollaborationError::new(if room.redeemed(invite_id) {
                    "invitation was already redeemed"
                } else {
                    "invitation was not issued by this host"
                })
            })
    }

    fn redeemable(room: &GovernedRoom, invite_id: [u8; 32]) -> Result<(), CollaborationError> {
        if Self::issued(room, invite_id)?.cancellation.is_some() {
            return Err(CollaborationError::new("invitation was cancelled"));
        }
        Ok(())
    }

    /// Admit the verified `redemption` to `room`: commit its governance,
    /// record its receipt and publish, in order.
    pub(super) async fn admit_redemption(
        &self,
        room: &mut GovernedRoom,
        remote_endpoint: [u8; 32],
        request_id: [u8; 32],
        proof_digest: [u8; 32],
        redemption: &VerifiedRedemption,
        now: u64,
    ) -> Result<JoinBootstrap, CollaborationError> {
        Self::redeemable(room, redemption.join().invite_id())?;
        let mut candidate = room.peer().clone();
        if let Err(error) =
            candidate.admit_invited_member(redemption, &self.inner.host.key, system_time(now))
        {
            tracing::warn!(
                room = %room.room(),
                invite_id = %hex::encode(redemption.join().invite_id()),
                person = %hex::encode(redemption.certificate().user_public_key),
                outcome = "replay_rejected",
                reason = %error,
                "workspace invitation redemption rejected"
            );
            return Err(CollaborationError::new(error));
        }
        let mut state = room.collaboration().clone();
        // The receipt answers for this join from now on. The challenge it
        // proved and the invitation it spent admit nobody else, and keeping
        // them made every later write of the room carry each past join.
        let invite_id = redemption.join().invite_id();
        state.join_challenges.retain(|record| {
            record.challenge.request_id != request_id || record.remote_endpoint != remote_endpoint
        });
        state
            .invitations
            .retain(|record| record.issued.invite_id != invite_id);
        let mut delivered = None;
        room.commit(
            candidate,
            &self.inner.host.key,
            system_time(now),
            |record, peer| {
                let bootstrap = current_bootstrap(
                    record,
                    peer,
                    redemption.join().invite_id(),
                    redemption.certificate().clone(),
                    redemption.join().grant().role,
                );
                state.join_completions.push(
                    JoinCompletionRecord::receipt(
                        remote_endpoint,
                        request_id,
                        proof_digest,
                        &bootstrap,
                    )
                    .map_err(|error| error.to_string())?,
                );
                delivered = Some(bootstrap);
                Ok(state)
            },
        )?;
        let bootstrap =
            delivered.ok_or_else(|| CollaborationError::new("join bootstrap was not built"))?;
        self.settle(room).await?;
        tracing::info!(
            room = %bootstrap.room,
            invite_id = %hex::encode(bootstrap.invite_id),
            person = %hex::encode(bootstrap.device_cert.user_public_key),
            device = %hex::encode(bootstrap.device_cert.device_public_key),
            role = ?bootstrap.role,
            outcome = "redeemed",
            "workspace invitation redemption committed"
        );
        Ok(bootstrap)
    }

    #[expect(
        clippy::significant_drop_tightening,
        reason = "the room stays locked until its decision is committed, published and answered"
    )]
    async fn membership_challenge(
        &self,
        remote_endpoint: [u8; 32],
        request: MembershipChallengeRequest,
        now: u64,
    ) -> Result<MembershipChallengeResponse, CollaborationError> {
        let room = self.room(&request.room).await?;
        let mut room = room.lock().await;
        if remote_endpoint != request.device_cert.device_public_key
            || request.workspace_id != room.workspace_id()
        {
            return Err(CollaborationError::new(
                "membership request does not match authenticated workspace endpoint",
            ));
        }
        let digest = request_digest(&(remote_endpoint, &request))?;
        if let Some(prior) = room
            .collaboration()
            .membership_challenges
            .iter()
            .find(|prior| {
                prior.challenge.request_id == request.request_id
                    && prior.challenge.expires_at_ns > now
            })
        {
            if prior.request_digest != digest {
                return Err(CollaborationError::new(
                    "membership request ID already names a different challenge",
                ));
            }
            return Ok(MembershipChallengeResponse {
                challenge: prior.challenge.clone(),
            });
        }
        let mut nonce = [0; 32];
        getrandom::fill(&mut nonce).map_err(|error| CollaborationError::new(error.to_string()))?;
        let challenge = room.peer().membership_challenge(
            room.workspace_id(),
            &request.device_cert,
            request.request_id,
            nonce,
            now.saturating_add(CHALLENGE_LIFETIME_NS),
            system_time(now),
        )?;
        let mut state = room.collaboration().clone();
        state
            .membership_challenges
            .retain(|record| record.challenge.expires_at_ns > now);
        if state.membership_challenges.len() >= MAX_PENDING_CHALLENGES {
            return Err(CollaborationError::new(
                "pending membership challenge limit reached",
            ));
        }
        state.membership_challenges.push(MembershipChallengeRecord {
            remote_endpoint,
            request_digest: digest,
            device_cert: request.device_cert,
            challenge: challenge.clone(),
        });
        room.commit_state(state)?;
        Ok(MembershipChallengeResponse { challenge })
    }

    async fn membership_complete(
        &self,
        remote_endpoint: [u8; 32],
        request: MembershipCompleteRequest,
        now: u64,
    ) -> Result<JoinBootstrap, CollaborationError> {
        let room = self.room(&request.proof.challenge.room).await?;
        let room = room.lock().await;
        let issued = room
            .collaboration()
            .membership_challenges
            .iter()
            .find(|prior| prior.challenge == request.proof.challenge)
            .ok_or_else(|| {
                CollaborationError::new("membership challenge was not issued by this host")
            })?;
        if issued.remote_endpoint != remote_endpoint
            || issued.device_cert.device_public_key != remote_endpoint
        {
            return Err(CollaborationError::new(
                "membership proof does not match authenticated transport endpoint",
            ));
        }
        let membership = verify_membership_proof(
            &request.proof,
            &issued.challenge,
            &issued.device_cert,
            &revocations(&room)?,
            now,
        )?;
        let role = room
            .peer()
            .verify_member_refresh(&membership, system_time(now))?;
        let invite_id = room
            .collaboration()
            .join_completions
            .iter()
            .find(|completion| {
                completion.bootstrap.device_cert.device_public_key == remote_endpoint
            })
            .map_or([0; 32], |completion| completion.bootstrap.invite_id);
        Ok(current_bootstrap(
            &room.record,
            room.peer(),
            invite_id,
            issued.device_cert.clone(),
            role,
        ))
    }

    async fn handle_join(
        &self,
        remote_endpoint: [u8; 32],
        request: JoinTransportRequest,
    ) -> Result<JoinTransportResponse, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        Ok(match request {
            JoinTransportRequest::Inspect { signed_invitation } => {
                let room = self.room(&invitation_room(&signed_invitation)?).await?;
                let join = Self::inspect(&*room.lock().await, &signed_invitation, now)?;
                JoinTransportResponse::Inspection(JoinInspection {
                    invite_id: join.invite_id(),
                    grant: join.grant().clone(),
                })
            }
            JoinTransportRequest::Challenge(request) => JoinTransportResponse::Challenge(
                self.challenge(remote_endpoint, request, now).await?,
            ),
            JoinTransportRequest::Complete(request) => {
                JoinTransportResponse::Complete(self.complete(remote_endpoint, request, now).await?)
            }
            JoinTransportRequest::MembershipChallenge(request) => {
                JoinTransportResponse::MembershipChallenge(
                    self.membership_challenge(remote_endpoint, request, now)
                        .await?,
                )
            }
            JoinTransportRequest::MembershipComplete(request) => {
                JoinTransportResponse::MembershipComplete(
                    self.membership_complete(remote_endpoint, request, now)
                        .await?,
                )
            }
        })
    }
}

impl GovernedRoom {
    /// Whether a device redeemed the invitation `invite_id`.
    pub(super) fn redeemed(&self, invite_id: [u8; 32]) -> bool {
        self.collaboration()
            .join_completions
            .iter()
            .any(|completion| completion.bootstrap.invite_id == invite_id)
    }

    /// The certified device admitted by the committed request receipt.
    pub(super) fn admitted_device(&self, request_id: [u8; 32]) -> Option<[u8; 32]> {
        self.collaboration()
            .join_completions
            .iter()
            .find(|completion| completion.request_id == request_id)
            .map(|completion| completion.bootstrap.device_cert.device_public_key)
    }
}

/// The workspace-join service of the rooms this relay governs.
#[derive(Clone, Debug)]
pub struct RoomJoinService {
    rooms: Rooms,
}

impl RoomJoinService {
    pub(super) const fn new(rooms: Rooms) -> Self {
        Self { rooms }
    }
}

impl WorkspaceJoinService for RoomJoinService {
    async fn handle(
        &self,
        remote_endpoint: [u8; 32],
        request: JoinTransportRequest,
    ) -> JoinTransportResponse {
        self.rooms
            .handle_join(remote_endpoint, request)
            .await
            .unwrap_or_else(|error| JoinTransportResponse::Error {
                code: 403,
                message: error.to_string(),
            })
    }
}

/// The room an invitation names, read without trusting it: the room it
/// names is then verified against what that room issued.
pub(super) fn invitation_room(signed_invitation: &[u8]) -> Result<String, CollaborationError> {
    let join = decode_and_verify_invitation(
        signed_invitation,
        None,
        &RevocationList::new(),
        tilo_clock::unix_now_ns(),
    )?;
    Ok(join.grant().room.clone())
}

fn validate_issue_request(
    request: &IssueInvitationRequest,
    now: u64,
) -> Result<(), CollaborationError> {
    if request.request_id == [0; 32] {
        return Err(CollaborationError::new(
            "invitation request ID cannot be zero",
        ));
    }
    if request.expires_at_ns <= now
        || request.expires_at_ns > now.saturating_add(MAX_INVITATION_LIFETIME_NS)
    {
        return Err(CollaborationError::new("invitation lifetime is invalid"));
    }
    if let Some(name) = request.invitee_name.as_deref()
        && (name.trim().is_empty() || name.len() > 128 || name.chars().any(char::is_control))
    {
        return Err(CollaborationError::new("invitee name is invalid"));
    }
    Ok(())
}

pub(super) fn request_digest(value: &impl Serialize) -> Result<[u8; 32], CollaborationError> {
    postcard::to_stdvec(value)
        .map(|bytes| *blake3::hash(&bytes).as_bytes())
        .map_err(|error| CollaborationError::new(error.to_string()))
}

fn revocations(room: &GovernedRoom) -> Result<RevocationList, CollaborationError> {
    let mut revocations = RevocationList::new();
    for entry in room.peer().revocations() {
        revocations
            .insert(entry)
            .map_err(|error| CollaborationError::new(error.to_string()))?;
    }
    Ok(revocations)
}

#[cfg(test)]
mod receipt_tests {
    use super::*;

    #[test]
    fn a_join_receipt_keeps_none_of_the_rooms_state_and_replays_while_it_is_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let (key, cert) = crate::rooms::tests::governor();
        let room = GovernedRoom::create(
            dir.path(),
            "Demo",
            &cert,
            &key,
            std::time::SystemTime::now(),
        )
        .unwrap();
        let delivered =
            current_bootstrap(&room.record, room.peer(), [9; 32], cert, MemberRole::Editor);
        assert!(!delivered.certified_roster.is_empty());
        let receipt = JoinCompletionRecord::receipt([1; 32], [2; 32], [3; 32], &delivered).unwrap();
        assert!(receipt.bootstrap.certified_roster.is_empty());
        assert!(receipt.bootstrap.records.is_empty());
        assert_eq!(receipt.replay(&room).unwrap(), delivered);
    }
}
