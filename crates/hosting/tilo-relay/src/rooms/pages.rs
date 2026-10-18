//! The rooms as pages reach them over HTTPS (`/rooms`, `/join`; see
//! [`tilo_relay_protocol::rooms`]).
//!
//! `/rooms` is room administration with the authorization every door has
//! ([`Rooms::authorize`]): the
//! session's device is one of the relay's own user, or a member's the
//! room's governance lets do what it asks. `/join` redeems an invitation for
//! a device with no session yet. Unlike the workspace-join protocol, a
//! page's verified redemption is not admitted at once: it waits, in memory,
//! until someone the room lets invite admits or declines it. A relay restart forgets waiting
//! requests; the person asks again.

use tilo_action_policy::roles::governance;
use tilo_peer_state::join::{
    JoinChallenge, JoinChallengeRequest, JoinProof, MemberRole, VerifiedRedemption,
};
use tilo_relay_protocol::rooms::{
    Inviter, JoinReply, JoinRequest, JoinStep, MAX_JOIN_REQUESTS, PendingInvite, PreAdmission,
    Room, RoomMember, RoomNotice, RoomRole, RoomsReply, RoomsRequest,
};
use tilo_user_identity::DeviceCert;

use super::authority::RoomOp;
use super::{
    CancelInvitationRequest, CollaborationError, GovernedRoom, IssueInvitationRequest,
    IssuedInvitation, RemoveMemberRequest, Rooms, SetMemberRoleRequest, system_time,
};
use crate::host_authority::HostOperation;
use tilo_lock::MutexRecover as _;

/// How long a declined request still answers its device's status.
const DECIDED_LIFETIME_NS: u64 = 10 * 60 * 1_000_000_000;

/// One pre-admission to record: the owner's signed grant and, when the
/// invitation is for one person only, that person.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreAdmitRequest {
    /// Caller-chosen idempotency key: the same request again is a retry.
    pub request_id: [u8; 32],
    /// The owner's signed grant.
    pub grant: PreAdmission,
    /// The only person who may redeem the invitation, if restricted.
    pub recipient_user_public_key: Option<[u8; 32]>,
    /// How the invitation names who issued it.
    pub inviter_name: Option<String>,
}

/// A pre-admitted redemption waiting for its room's next admission pass,
/// and where its answer goes.
pub(super) struct QueuedAdmission {
    proof: JoinProof,
    proof_digest: [u8; 32],
    name: String,
    token: Vec<u8>,
    answer: tokio::sync::oneshot::Sender<Result<JoinReply, CollaborationError>>,
}

/// A verified redemption waiting for the room's owner.
#[derive(Debug)]
pub(super) struct PageJoin {
    room: String,
    request: JoinRequest,
    proof_digest: [u8; 32],
    redemption: VerifiedRedemption,
    declined_at_ns: Option<u64>,
}

/// The page joins of every room, waiting or recently declined.
#[derive(Debug, Default)]
pub(super) struct PageJoins(Vec<PageJoin>);

impl PageJoins {
    fn prune(&mut self, now: u64) {
        self.0.retain(|join| {
            join.declined_at_ns
                .is_none_or(|at| now.saturating_sub(at) < DECIDED_LIFETIME_NS)
        });
    }

    fn find(&self, room: &str, request_id: [u8; 32]) -> Option<&PageJoin> {
        self.0
            .iter()
            .find(|join| join.room == room && join.request.request_id == request_id)
    }
}

pub(super) const fn member_role(role: RoomRole) -> MemberRole {
    match role {
        RoomRole::Viewer => MemberRole::Viewer,
        RoomRole::Commenter => MemberRole::Commenter,
        RoomRole::Editor => MemberRole::Editor,
        RoomRole::Moderator => MemberRole::Moderator,
    }
}

const fn room_role(role: MemberRole) -> RoomRole {
    match role {
        MemberRole::Viewer => RoomRole::Viewer,
        MemberRole::Commenter => RoomRole::Commenter,
        MemberRole::Editor => RoomRole::Editor,
        MemberRole::Moderator => RoomRole::Moderator,
    }
}

fn page_room(room: &GovernedRoom) -> Room {
    let summary = room.summary();
    let certificates = room.peer().certificates();
    let owner = room.peer().authority().user_public_key;
    let mut members: Vec<_> = summary
        .members
        .into_iter()
        .map(|member| RoomMember {
            name: member.display_name.clone().or_else(|| {
                certificates
                    .iter()
                    .find(|cert| cert.user_public_key == member.user_public_key)
                    .map(|cert| cert.device_label.clone())
                    .filter(|label| !label.is_empty())
            }),
            user_public_key: member.user_public_key,
            role: room_role(member.role),
            owner: member.user_public_key == owner,
            removed: member.removed,
        })
        .collect();
    // The structural owner has no invitation admission receipt, so
    // governed_members() lists only invited people. Include the owner from
    // the room's pinned authority, never from the caller's asserted root.
    if !members.iter().any(|member| member.user_public_key == owner) {
        members.insert(
            0,
            RoomMember {
                user_public_key: owner,
                name: Some(room.peer().authority().device_label.clone()),
                role: RoomRole::Editor,
                owner: true,
                removed: false,
            },
        );
    }
    Room {
        room: summary.room,
        workspace_id: summary.workspace_id,
        name: summary.name,
        generation: summary.generation,
        members,
    }
}

fn valid_name(name: &str) -> Result<String, CollaborationError> {
    tilo_user_identity::display_name(name)
        .map(str::to_owned)
        .map_err(|error| CollaborationError::new(error.reason()))
}

impl Rooms {
    /// Answer one `/rooms` request of `device`, the session's certified
    /// device.
    pub async fn page_rooms(&self, device: &DeviceCert, request: RoomsRequest) -> RoomsReply {
        self.page_rooms_inner(device, request)
            .await
            .unwrap_or_else(|error| RoomsReply::Refused(error.to_string()))
    }

    async fn page_rooms_inner(
        &self,
        device: &DeviceCert,
        request: RoomsRequest,
    ) -> Result<RoomsReply, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        Ok(match request {
            RoomsRequest::List => self.page_list(device, now).await?,
            RoomsRequest::Create { name } => self.page_create(device, &name, now).await?,
            RoomsRequest::Enter { room } => {
                self.enter(&room, device).await?;
                self.page_room(&room).await?
            }
            RoomsRequest::Invite {
                room,
                request_id,
                role,
                expires_at_ns,
                inviter_name,
            } => {
                let request = IssueInvitationRequest {
                    request_id,
                    role: member_role(role),
                    recipient_user_public_key: None,
                    invitee_name: None,
                    expires_at_ns,
                    inviter_name,
                };
                self.page_invitation(&room, device, request).await?
            }
            RoomsRequest::Cancel {
                room,
                request_id,
                invite_id,
            } => {
                self.cancel(
                    &room,
                    device,
                    CancelInvitationRequest {
                        request_id,
                        invite_id,
                    },
                )
                .await?;
                self.page_pending(&room, device).await?
            }
            RoomsRequest::Pending { room } => self.page_pending(&room, device).await?,
            RoomsRequest::Requests { room } => {
                RoomsReply::Requests(self.join_requests(&room, device, now).await?)
            }
            RoomsRequest::Admit { room, request_id } => {
                self.admit_page(&room, device, request_id, now).await?;
                self.page_room(&room).await?
            }
            RoomsRequest::Decline { room, request_id } => {
                self.decline_page(&room, device, request_id, now).await?;
                RoomsReply::Requests(self.waiting(&room, now))
            }
            request @ (RoomsRequest::SetRole { .. }
            | RoomsRequest::Remove { .. }
            | RoomsRequest::Ban { .. }
            | RoomsRequest::Unban { .. }) => self.page_membership(device, request).await?,
            RoomsRequest::PreAdmit {
                request_id,
                grant,
                inviter_name,
            } => {
                self.page_pre_admit(device, request_id, grant, inviter_name)
                    .await?
            }
            RoomsRequest::Notices { room } => {
                let room = self.room(&room).await?;
                let room = room.lock().await;
                self.authorize(device, Some(&room), RoomOp::Govern(governance::INVITE), now)?;
                RoomsReply::Notices(room.admissions.notices())
            }
            RoomsRequest::Permits { room, node } => {
                RoomsReply::Permits(self.page_permits(&room, device, &node).await?)
            }
        })
    }

    /// Whether `device`'s person may make an undoable change at `node` of
    /// `room`, decided as their writes there are.
    async fn page_permits(
        &self,
        room: &str,
        device: &DeviceCert,
        node: &str,
    ) -> Result<bool, CollaborationError> {
        let node: tilo_action_policy::TreePath = node
            .parse()
            .map_err(|_| CollaborationError::new("not a node of the room's tree"))?;
        let state = self.room(room).await?;
        let governed = state.lock().await;
        if node.room_id() != Some(governed.room()) {
            return Err(CollaborationError::new("the node is not in this room"));
        }
        let now = system_time(tilo_clock::unix_now_ns());
        let member = governed
            .peer()
            .permissions(device.device_public_key, now)
            .map_err(CollaborationError::new)?;
        drop(governed);
        Ok(member.allows(&node, tilo_action_policy::ActionKind::Undoable, now))
    }

    /// A page's change to who is in a room and with which role.
    async fn page_membership(
        &self,
        device: &DeviceCert,
        request: RoomsRequest,
    ) -> Result<RoomsReply, CollaborationError> {
        Ok(match request {
            RoomsRequest::SetRole {
                room,
                request_id,
                user_public_key,
                role,
            } => {
                self.set_member_role(
                    &room,
                    device,
                    SetMemberRoleRequest {
                        request_id,
                        user_public_key,
                        role: member_role(role),
                    },
                )
                .await?;
                self.page_room(&room).await?
            }
            RoomsRequest::Remove {
                room,
                request_id,
                user_public_key,
            } => {
                self.page_remove(&room, device, request_id, user_public_key)
                    .await?
            }
            RoomsRequest::Ban {
                room,
                request_id,
                user_public_key,
            } => {
                self.page_ban(&room, device, request_id, user_public_key, true)
                    .await?
            }
            RoomsRequest::Unban {
                room,
                request_id,
                user_public_key,
            } => {
                self.page_ban(&room, device, request_id, user_public_key, false)
                    .await?
            }
            _ => return Err(CollaborationError::new("not a membership request")),
        })
    }

    /// A page's pre-admission: an invitation anyone holding the token may
    /// redeem.
    async fn page_pre_admit(
        &self,
        device: &DeviceCert,
        request_id: [u8; 32],
        grant: PreAdmission,
        inviter_name: Option<String>,
    ) -> Result<RoomsReply, CollaborationError> {
        let request = PreAdmitRequest {
            request_id,
            grant,
            recipient_user_public_key: None,
            inviter_name,
        };
        let issued = self.pre_admit(device, request).await?;
        Ok(RoomsReply::Invitation {
            invite_id: issued.invite_id,
            signed_invitation: issued.signed_invitation,
        })
    }

    /// Ban (`ban`) or unban a person, as the room's policy lets `device`.
    async fn page_ban(
        &self,
        room: &str,
        device: &DeviceCert,
        request_id: [u8; 32],
        user_public_key: [u8; 32],
        ban: bool,
    ) -> Result<RoomsReply, CollaborationError> {
        let request = RemoveMemberRequest {
            request_id,
            user_public_key,
        };
        if ban {
            self.ban_member(room, device, request).await?;
        } else {
            self.unban_member(room, device, request).await?;
        }
        self.page_room(room).await
    }

    async fn page_remove(
        &self,
        room: &str,
        device: &DeviceCert,
        request_id: [u8; 32],
        user_public_key: [u8; 32],
    ) -> Result<RoomsReply, CollaborationError> {
        if user_public_key == self.inner.host.cert.user_public_key {
            return Err(CollaborationError::new("the owner cannot be removed"));
        }
        self.remove_member(
            room,
            device,
            RemoveMemberRequest {
                request_id,
                user_public_key,
            },
        )
        .await?;
        self.page_room(room).await
    }

    /// Issue the single-use invitation of the owner's grant and record the
    /// grant with it. With a recipient, only that person can redeem it.
    ///
    /// # Errors
    ///
    /// Refuses non-administrators, grants the room's owner did not sign,
    /// and a full grant table.
    pub async fn pre_admit(
        &self,
        device: &DeviceCert,
        request: PreAdmitRequest,
    ) -> Result<IssuedInvitation, CollaborationError> {
        let PreAdmitRequest {
            request_id,
            grant,
            recipient_user_public_key,
            inviter_name,
        } = request;
        let now = tilo_clock::unix_now_ns();
        self.authorize(device, None, RoomOp::Host(HostOperation::PreAdmit), now)?;
        grant.verify()?;
        if grant.owner_user_public_key != self.inner.host.cert.user_public_key {
            return Err(CollaborationError::new(
                "only the room's owner pre-admits people",
            ));
        }
        let room_id = grant.room.clone();
        let issued = {
            let room = self.room(&room_id).await?;
            let mut room = room.lock().await;
            let issued = self.stage_issue(
                &mut room,
                device,
                IssueInvitationRequest {
                    request_id,
                    role: member_role(grant.role),
                    recipient_user_public_key,
                    invitee_name: None,
                    expires_at_ns: grant.expires_at_ns,
                    inviter_name,
                },
                now,
            )?;
            room.admissions.record(
                issued.invite_id,
                grant,
                now,
                self.inner.host.max_pre_admissions,
            )?;
            issued
        };
        // The invitation and its grant are written with whatever else the
        // room staged meanwhile: a crowd's pre-admissions took the room for
        // two whole writes each, one after another.
        self.written(&room_id, true).await?;
        Ok(issued)
    }

    async fn page_create(
        &self,
        device: &DeviceCert,
        name: &str,
        now: u64,
    ) -> Result<RoomsReply, CollaborationError> {
        self.authorize(device, None, RoomOp::Host(HostOperation::CreateRoom), now)?;
        let created = self.create(name).await?;
        self.page_room(&created.room).await
    }

    async fn page_list(
        &self,
        device: &DeviceCert,
        now: u64,
    ) -> Result<RoomsReply, CollaborationError> {
        self.authorize(device, None, RoomOp::Host(HostOperation::ListRooms), now)?;
        let rooms = self
            .inner
            .rooms
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let mut listed = Vec::with_capacity(rooms.len());
        for room in rooms {
            listed.push(page_room(&*room.lock().await));
        }
        Ok(RoomsReply::Rooms(listed))
    }

    async fn page_invitation(
        &self,
        room: &str,
        device: &DeviceCert,
        request: IssueInvitationRequest,
    ) -> Result<RoomsReply, CollaborationError> {
        let issued = self.issue(room, device, request).await?;
        Ok(RoomsReply::Invitation {
            invite_id: issued.invite_id,
            signed_invitation: issued.signed_invitation,
        })
    }

    async fn page_room(&self, room: &str) -> Result<RoomsReply, CollaborationError> {
        let room = self.room(room).await?;
        Ok(RoomsReply::Room(page_room(&*room.lock().await)))
    }

    async fn page_pending(
        &self,
        room: &str,
        device: &DeviceCert,
    ) -> Result<RoomsReply, CollaborationError> {
        Ok(RoomsReply::Pending(
            self.pending(room, device)
                .await?
                .into_iter()
                .map(|invitation| PendingInvite {
                    invite_id: invitation.invite_id,
                    role: room_role(invitation.role),
                    expires_at_ns: invitation.expires_at_ns,
                })
                .collect(),
        ))
    }

    fn page_joins(&self) -> std::sync::MutexGuard<'_, PageJoins> {
        self.inner.page_joins.lock_recover()
    }

    /// The join requests waiting in `room`, for `device`: someone the room
    /// lets invite.
    ///
    /// # Errors
    ///
    /// Why the device may not see them.
    pub(super) async fn join_requests(
        &self,
        room: &str,
        device: &DeviceCert,
        now: u64,
    ) -> Result<Vec<JoinRequest>, CollaborationError> {
        {
            let governed = self.room(room).await?;
            let governed = governed.lock().await;
            self.authorize(
                device,
                Some(&governed),
                RoomOp::Govern(governance::INVITE),
                now,
            )?;
        }
        Ok(self.waiting(room, now))
    }

    fn waiting(&self, room: &str, now: u64) -> Vec<JoinRequest> {
        let mut joins = self.page_joins();
        joins.prune(now);
        joins
            .0
            .iter()
            .filter(|join| join.room == room && join.declined_at_ns.is_none())
            .map(|join| join.request.clone())
            .collect()
    }

    pub(super) async fn decline_page(
        &self,
        room: &str,
        device: &DeviceCert,
        request_id: [u8; 32],
        now: u64,
    ) -> Result<(), CollaborationError> {
        let governed = self.room(room).await?;
        // Serialize with admission, which holds this lock until governance
        // publication completes and removes the waiting request.
        let decision = governed.lock().await;
        self.authorize(
            device,
            Some(&decision),
            RoomOp::Govern(governance::INVITE),
            now,
        )?;
        let mut joins = self.page_joins();
        let join = joins
            .0
            .iter_mut()
            .find(|join| join.room == room && join.request.request_id == request_id)
            .ok_or_else(|| CollaborationError::new("no such join request"))?;
        join.declined_at_ns.get_or_insert(now);
        let person = join.request.user_public_key;
        drop(joins);
        drop(decision);
        tracing::info!(
            %room,
            person = %hex::encode(person),
            outcome = "declined",
            "workspace join request declined"
        );
        Ok(())
    }

    #[expect(
        clippy::significant_drop_tightening,
        reason = "the room stays locked until its decision is committed, published and answered"
    )]
    pub(super) async fn admit_page(
        &self,
        room_id: &str,
        administrator: &DeviceCert,
        request_id: [u8; 32],
        now: u64,
    ) -> Result<(), CollaborationError> {
        let room = self.room(room_id).await?;
        let mut room = room.lock().await;
        // A retry after a lost HTTPS reply observes the committed receipt.
        // Never re-admit a member who has since lost governed access.
        if let Some(device) = room.admitted_device(request_id) {
            self.authorize(
                administrator,
                Some(&room),
                RoomOp::Govern(governance::INVITE),
                now,
            )?;
            room.peer().permissions(device, system_time(now))?;
            return Ok(());
        }
        let (device, proof_digest, redemption) = {
            let joins = self.page_joins();
            let join = joins
                .find(room_id, request_id)
                .filter(|join| join.declined_at_ns.is_none())
                .ok_or_else(|| CollaborationError::new("no such join request"))?;
            (
                join.request.device_public_key,
                join.proof_digest,
                join.redemption.clone(),
            )
        };
        // Admitting someone hands out the role their invitation carries:
        // the invite action, and never a role above the admitter's own.
        self.authorize(
            administrator,
            Some(&room),
            RoomOp::Invite(redemption.join().grant().role.into()),
            now,
        )?;
        self.admit_redemption(
            &mut room,
            device,
            request_id,
            proof_digest,
            &redemption,
            now,
        )
        .await?;
        self.page_joins()
            .0
            .retain(|join| !(join.room == room_id && join.request.request_id == request_id));
        Ok(())
    }

    /// Answer one `/join` step of a device with no session.
    pub async fn page_join(&self, step: JoinStep) -> JoinReply {
        self.page_join_inner(step)
            .await
            .unwrap_or_else(|error| JoinReply::Refused(error.to_string()))
    }

    async fn page_join_inner(&self, step: JoinStep) -> Result<JoinReply, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        match step {
            JoinStep::Inspect { invitation } => {
                let room = self
                    .room(&super::collaboration::invitation_room(&invitation)?)
                    .await?;
                let room = room.lock().await;
                let join = Self::inspect(&room, &invitation, now)?;
                Ok(JoinReply::Invitation {
                    room: room.room().to_owned(),
                    workspace_id: room.workspace_id(),
                    name: room.record.name.clone(),
                    role: room_role(join.grant().role),
                    expires_at_ns: join.grant().expires_at_ns,
                    inviter: join.inviter().map(|inviter| Inviter {
                        user_public_key: inviter.user_public_key,
                        name: inviter.name.clone(),
                    }),
                })
            }
            JoinStep::Ask {
                invitation,
                device_cert,
                request_id,
            } => {
                let remote = device_cert.device_public_key;
                let challenge = self
                    .challenge(
                        remote,
                        JoinChallengeRequest {
                            signed_invitation: invitation,
                            device_cert,
                            request_id,
                        },
                        now,
                    )
                    .await?
                    .challenge;
                Ok(JoinReply::Challenge {
                    challenge: postcard::to_stdvec(&challenge)
                        .map_err(|error| CollaborationError::new(error.to_string()))?,
                })
            }
            JoinStep::Prove {
                challenge,
                signature,
                name,
            } => {
                let challenge: JoinChallenge = postcard::from_bytes(&challenge)
                    .map_err(|_| CollaborationError::new("join challenge is malformed"))?;
                self.prove(
                    JoinProof {
                        challenge,
                        signature,
                    },
                    &valid_name(&name)?,
                    None,
                    now,
                )
                .await
            }
            JoinStep::ProvePreAdmitted {
                challenge,
                signature,
                name,
                token,
            } => {
                let challenge: JoinChallenge = postcard::from_bytes(&challenge)
                    .map_err(|_| CollaborationError::new("join challenge is malformed"))?;
                self.prove(
                    JoinProof {
                        challenge,
                        signature,
                    },
                    &valid_name(&name)?,
                    Some(&token),
                    now,
                )
                .await
            }
            JoinStep::Status {
                request_id,
                device_public_key,
            } => self.status(request_id, device_public_key, now).await,
        }
    }

    #[expect(
        clippy::significant_drop_tightening,
        reason = "the room stays locked while the request is recorded"
    )]
    async fn prove(
        &self,
        proof: JoinProof,
        name: &str,
        token: Option<&[u8]>,
        now: u64,
    ) -> Result<JoinReply, CollaborationError> {
        if let Some(token) = token {
            return self.prove_pre_admitted(proof, name, token).await;
        }
        let room_id = proof.challenge.room.clone();
        let request_id = proof.challenge.request_id;
        let device = proof.challenge.device_public_key;
        let proof_digest = super::collaboration::request_digest(&proof)?;
        let room = self.room(&room_id).await?;
        let room = room.lock().await;
        if let Some(prior) = self.page_joins().find(&room_id, request_id) {
            if prior.proof_digest != proof_digest {
                return Err(CollaborationError::new(
                    "join request ID already names a different proof",
                ));
            }
            return Ok(match prior.declined_at_ns {
                Some(_) => JoinReply::Declined,
                None => JoinReply::Waiting,
            });
        }
        let redemption = Self::redeem(&room, device, &proof, now)?;
        let invite_id = redemption.join().invite_id();
        if room.redeemed(invite_id) {
            return Err(CollaborationError::new("invitation was already redeemed"));
        }
        let mut joins = self.page_joins();
        joins.prune(now);
        if joins.0.iter().any(|join| {
            join.declined_at_ns.is_none() && join.redemption.join().invite_id() == invite_id
        }) {
            return Err(CollaborationError::new(
                "someone else is already waiting with this invitation",
            ));
        }
        if joins.0.iter().filter(|join| join.room == room_id).count() >= MAX_JOIN_REQUESTS {
            return Err(CollaborationError::new(
                "too many people are waiting to join",
            ));
        }
        let certificate = redemption.certificate();
        let request = JoinRequest {
            request_id,
            user_public_key: certificate.user_public_key,
            device_public_key: certificate.device_public_key,
            name: name.to_owned(),
            role: room_role(redemption.join().grant().role),
            requested_at_ns: now,
        };
        tracing::info!(
            room = %room_id,
            invite_id = %hex::encode(invite_id),
            person = %hex::encode(request.user_public_key),
            outcome = "waiting",
            "workspace join request waits for the owner"
        );
        joins.0.push(PageJoin {
            room: room_id,
            request,
            proof_digest,
            redemption,
            declined_at_ns: None,
        });
        Ok(JoinReply::Waiting)
    }

    /// Admit the redeeming device at once under the owner's pre-admission
    /// for its invitation, spend the grant and tell the owner.
    ///
    /// A public room admits a crowd this way, and each admission is a write
    /// and a publication of the room that grow with it. So redemptions only
    /// queue here; the first since the room last took them starts a pass
    /// that takes the room once and admits all of them in one change.
    async fn prove_pre_admitted(
        &self,
        proof: JoinProof,
        name: &str,
        token: &[u8],
    ) -> Result<JoinReply, CollaborationError> {
        let room = proof.challenge.room.clone();
        let proof_digest = super::collaboration::request_digest(&proof)?;
        let (answer, answered) = tokio::sync::oneshot::channel();
        let first = self.queue_admission(
            &room,
            QueuedAdmission {
                proof,
                proof_digest,
                name: name.to_owned(),
                token: token.to_vec(),
                answer,
            },
        );
        if first {
            // Its own task: a page that goes away mid-pass must not cut off
            // the answers of everyone else in it.
            let rooms = self.clone();
            tokio::spawn(async move { rooms.admit_queued(&room).await });
        }
        answered
            .await
            .map_err(|_| CollaborationError::new("the room stopped admitting"))?
    }

    /// Queue `join` for `room`'s next admission pass. True when it is the
    /// first since the room last took them, so its sender starts the pass.
    fn queue_admission(&self, room: &str, join: QueuedAdmission) -> bool {
        let mut queued = self.inner.queued_admissions.lock_recover();
        let waiting = queued.entry(room.to_owned()).or_default();
        waiting.push(join);
        let first = waiting.len() == 1;
        drop(queued);
        first
    }

    /// Admit every pre-admitted redemption queued for `room`.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the room stays locked until its admissions are committed, published and answered"
    )]
    async fn admit_queued(&self, room_id: &str) {
        let room = self.room(room_id).await;
        let mut locked = match &room {
            Ok(room) => Some(room.lock().await),
            Err(_) => None,
        };
        let queued = self
            .inner
            .queued_admissions
            .lock_recover()
            .remove(room_id)
            .unwrap_or_default();
        let answers = match (&room, locked.as_mut()) {
            (Ok(_), Some(room)) => self.admit_pre_admitted(room, &queued).await,
            (Err(error), _) => vec![Err(error.clone()); queued.len()],
            (Ok(_), None) => Vec::new(),
        };
        for (queued, answer) in queued.into_iter().zip(answers) {
            let _ = queued.answer.send(answer);
        }
    }

    /// Check each queued redemption alone, then admit those that hold in one
    /// change and spend their grants in one write. Answers each in order.
    async fn admit_pre_admitted(
        &self,
        room: &mut GovernedRoom,
        queued: &[QueuedAdmission],
    ) -> Vec<Result<JoinReply, CollaborationError>> {
        let now = tilo_clock::unix_now_ns();
        let mut answers: Vec<Option<Result<JoinReply, CollaborationError>>> =
            Vec::with_capacity(queued.len());
        let mut admitting = Vec::new();
        let mut invites = std::collections::BTreeSet::new();
        for (index, join) in queued.iter().enumerate() {
            match self.check_pre_admitted(room, join, &mut invites, now) {
                Ok(checked) => {
                    admitting.push((index, checked));
                    answers.push(None);
                }
                Err(answer) => answers.push(Some(*answer)),
            }
        }
        let joins: Vec<_> = admitting
            .iter()
            .map(
                |(index, (redemption, _, _))| super::collaboration::Redeeming {
                    remote_endpoint: redemption.certificate().device_public_key,
                    request_id: redemption.request_id(),
                    proof_digest: queued.get(*index).map_or([0; 32], |join| join.proof_digest),
                    redemption,
                },
            )
            .collect();
        let admitted = if joins.is_empty() {
            Ok(Vec::new())
        } else {
            self.admit_redemptions(room, &joins, now).await
        };
        drop(joins);
        let mut spent = Vec::new();
        match admitted {
            Ok(admitted) => {
                for ((index, (redemption, label, role)), admitted) in
                    admitting.into_iter().zip(admitted)
                {
                    let answer = admitted.map(|_| {
                        let certificate = redemption.certificate();
                        let invite_id = redemption.join().invite_id();
                        spent.push((
                            invite_id,
                            RoomNotice::PreAdmitted {
                                user_public_key: certificate.user_public_key,
                                name: queued
                                    .get(index)
                                    .map(|join| join.name.clone())
                                    .unwrap_or_default(),
                                label,
                                role,
                                at_ns: now,
                            },
                        ));
                        tracing::info!(
                            room = %room.room(),
                            invite_id = %hex::encode(invite_id),
                            person = %hex::encode(certificate.user_public_key),
                            outcome = "pre-admitted",
                            "workspace join admitted under the owner's pre-admission"
                        );
                        JoinReply::Admitted {
                            room: room.room().to_owned(),
                            workspace_id: room.workspace_id(),
                            name: room.record.name.clone(),
                            role,
                        }
                    });
                    if let Some(slot) = answers.get_mut(index) {
                        *slot = Some(answer);
                    }
                }
            }
            Err(error) => {
                for (index, _) in admitting {
                    if let Some(slot) = answers.get_mut(index) {
                        *slot = Some(Err(error.clone()));
                    }
                }
            }
        }
        if !spent.is_empty()
            && let Err(error) = room.admissions.spend(spent)
        {
            let error = CollaborationError::from(error);
            for answer in answers.iter_mut().flatten() {
                if answer.is_ok() {
                    *answer = Err(error.clone());
                }
            }
        }
        answers
            .into_iter()
            .map(|answer| {
                answer.unwrap_or_else(|| Err(CollaborationError::new("join was not answered")))
            })
            .collect()
    }

    /// What admits one queued redemption under its pre-admission (the
    /// verified redemption, the grant's label and role), or its answer.
    /// `invites` holds the invitations already admitted in this pass.
    fn check_pre_admitted(
        &self,
        room: &GovernedRoom,
        join: &QueuedAdmission,
        invites: &mut std::collections::BTreeSet<[u8; 32]>,
        now: u64,
    ) -> Result<(VerifiedRedemption, String, RoomRole), Box<Result<JoinReply, CollaborationError>>>
    {
        let request_id = join.proof.challenge.request_id;
        if let Some(prior) = self.page_joins().find(room.room(), request_id) {
            if prior.proof_digest != join.proof_digest {
                return Err(Box::new(Err(CollaborationError::new(
                    "join request ID already names a different proof",
                ))));
            }
            return Err(Box::new(Ok(match prior.declined_at_ns {
                Some(_) => JoinReply::Declined,
                None => JoinReply::Waiting,
            })));
        }
        let device = join.proof.challenge.device_public_key;
        let redemption =
            Self::redeem(room, device, &join.proof, now).map_err(|error| Box::new(Err(error)))?;
        let invite_id = redemption.join().invite_id();
        if room.redeemed(invite_id) || !invites.insert(invite_id) {
            return Err(Box::new(Err(CollaborationError::new(
                "invitation was already redeemed",
            ))));
        }
        let role = room_role(redemption.join().grant().role);
        let grant = room
            .admissions
            .grant_for(invite_id, &join.token, now)
            .map_err(|error| Box::new(Err(CollaborationError::from(error))))?;
        self.pre_admission_signer(room.room(), grant)
            .map_err(|error| Box::new(Err(CollaborationError::from(error))))?;
        if grant.role != role || grant.room != room.room() {
            return Err(Box::new(Err(CollaborationError::new(
                "the pre-admission is for another room or role",
            ))));
        }
        Ok((redemption, grant.label.clone(), role))
    }

    async fn status(
        &self,
        request_id: [u8; 32],
        device: [u8; 32],
        now: u64,
    ) -> Result<JoinReply, CollaborationError> {
        {
            let mut joins = self.page_joins();
            joins.prune(now);
            if let Some(join) = joins.0.iter().find(|join| {
                join.request.request_id == request_id && join.request.device_public_key == device
            }) {
                return Ok(match join.declined_at_ns {
                    Some(_) => JoinReply::Declined,
                    None => JoinReply::Waiting,
                });
            }
        }
        let rooms = self
            .inner
            .rooms
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for room in rooms {
            let room = room.lock().await;
            if room.admitted_device(request_id) != Some(device) {
                continue;
            }
            let role = match room.peer().permissions(device, system_time(now)) {
                Ok(member) => match member.role {
                    collab_types::role::Role::Owner | collab_types::role::Role::Editor => {
                        RoomRole::Editor
                    }
                    collab_types::role::Role::Moderator => RoomRole::Moderator,
                    collab_types::role::Role::Commenter => RoomRole::Commenter,
                    collab_types::role::Role::Viewer => RoomRole::Viewer,
                },
                Err(_) => return Ok(JoinReply::Declined),
            };
            return Ok(JoinReply::Admitted {
                room: room.room().to_owned(),
                workspace_id: room.workspace_id(),
                name: room.record.name.clone(),
                role,
            });
        }
        Err(CollaborationError::new("no such join request"))
    }
}
