//! The rooms as pages reach them over HTTPS (`/rooms`, `/join`; see
//! [`tilo_relay_protocol::rooms`]).
//!
//! `/rooms` is room administration with the authorization of
//! [`ROOM_ADMIN_ALPN`](super::ROOM_ADMIN_ALPN): the session's device must be
//! one of the relay's own user. `/join` redeems an invitation for a device
//! with no session yet. Unlike the workspace-join protocol, a page's
//! verified redemption is not admitted at once: it waits, in memory, until
//! the room's owner admits or declines it. A relay restart forgets waiting
//! requests; the person asks again.

use tilo_peer_state::join::{
    JoinChallenge, JoinChallengeRequest, JoinProof, MemberRole, VerifiedRedemption,
};
use tilo_relay_protocol::rooms::{
    Inviter, JoinReply, JoinRequest, JoinStep, MAX_JOIN_NAME_CHARS, MAX_JOIN_REQUESTS,
    PendingInvite, PreAdmission, Room, RoomMember, RoomNotice, RoomRole, RoomsReply, RoomsRequest,
};
use tilo_user_identity::DeviceCert;

use super::{
    CancelInvitationRequest, CollaborationError, GovernedRoom, IssueInvitationRequest,
    RemoveMemberRequest, Rooms, SetMemberRoleRequest, system_time,
};

/// How long a declined request still answers its device's status.
const DECIDED_LIFETIME_NS: u64 = 10 * 60 * 1_000_000_000;

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

const fn member_role(role: RoomRole) -> MemberRole {
    match role {
        RoomRole::Viewer => MemberRole::Viewer,
        RoomRole::Commenter => MemberRole::Commenter,
        RoomRole::Editor => MemberRole::Editor,
    }
}

const fn room_role(role: MemberRole) -> RoomRole {
    match role {
        MemberRole::Viewer => RoomRole::Viewer,
        MemberRole::Commenter => RoomRole::Commenter,
        MemberRole::Editor => RoomRole::Editor,
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
    let name = name.trim();
    if name.is_empty()
        || name.chars().count() > MAX_JOIN_NAME_CHARS
        || name.chars().any(char::is_control)
    {
        return Err(CollaborationError::new("name is invalid"));
    }
    Ok(name.to_owned())
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
                self.authorize(device, now, "join.requests")?;
                self.room(&room).await?;
                RoomsReply::Requests(self.waiting(&room, now))
            }
            RoomsRequest::Admit { room, request_id } => {
                self.authorize(device, now, "join.admit")?;
                self.admit_page(&room, request_id, now).await?;
                self.page_room(&room).await?
            }
            RoomsRequest::Decline { room, request_id } => {
                self.authorize(device, now, "join.decline")?;
                self.decline_page(&room, request_id, now).await?;
                RoomsReply::Requests(self.waiting(&room, now))
            }
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
            RoomsRequest::PreAdmit {
                request_id,
                grant,
                inviter_name,
            } => {
                self.pre_admit(device, request_id, grant, inviter_name, now)
                    .await?
            }
            RoomsRequest::Notices { room } => {
                self.authorize(device, now, "room.notices")?;
                let room = self.room(&room).await?;
                let notices = room.lock().await.admissions.notices();
                RoomsReply::Notices(notices)
            }
        })
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

    /// Issue the single-use invitation of the owner's `grant` and record
    /// the grant with it.
    async fn pre_admit(
        &self,
        device: &DeviceCert,
        request_id: [u8; 32],
        grant: PreAdmission,
        inviter_name: Option<String>,
        now: u64,
    ) -> Result<RoomsReply, CollaborationError> {
        self.authorize(device, now, "invitation.pre-admit")?;
        grant.verify()?;
        if grant.owner_user_public_key != self.inner.host.cert.user_public_key {
            return Err(CollaborationError::new(
                "only the room's owner pre-admits people",
            ));
        }
        let issued = self
            .issue(
                &grant.room,
                device,
                IssueInvitationRequest {
                    request_id,
                    role: member_role(grant.role),
                    recipient_user_public_key: None,
                    invitee_name: None,
                    expires_at_ns: grant.expires_at_ns,
                    inviter_name,
                },
            )
            .await?;
        let room = self.room(&grant.room).await?;
        room.lock()
            .await
            .admissions
            .record(issued.invite_id, grant)?;
        Ok(RoomsReply::Invitation {
            invite_id: issued.invite_id,
            signed_invitation: issued.signed_invitation,
        })
    }

    async fn page_create(
        &self,
        device: &DeviceCert,
        name: &str,
        now: u64,
    ) -> Result<RoomsReply, CollaborationError> {
        self.authorize(device, now, "room.create")?;
        let created = self.create(name).await?;
        self.page_room(&created.room).await
    }

    async fn page_list(
        &self,
        device: &DeviceCert,
        now: u64,
    ) -> Result<RoomsReply, CollaborationError> {
        self.authorize(device, now, "room.list")?;
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
        self.inner
            .page_joins
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
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

    async fn decline_page(
        &self,
        room: &str,
        request_id: [u8; 32],
        now: u64,
    ) -> Result<(), CollaborationError> {
        let governed = self.room(room).await?;
        // Serialize with admission, which holds this lock until governance
        // publication completes and removes the waiting request.
        let decision = governed.lock().await;
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
    async fn admit_page(
        &self,
        room_id: &str,
        request_id: [u8; 32],
        now: u64,
    ) -> Result<(), CollaborationError> {
        let room = self.room(room_id).await?;
        let mut room = room.lock().await;
        // A retry after a lost HTTPS reply observes the committed receipt.
        // Never re-admit a member who has since lost governed access.
        if let Some(device) = room.admitted_device(request_id) {
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
        let room_id = proof.challenge.room.clone();
        let request_id = proof.challenge.request_id;
        let device = proof.challenge.device_public_key;
        let proof_digest = super::collaboration::request_digest(&proof)?;
        let room = self.room(&room_id).await?;
        let mut room = room.lock().await;
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
        if let Some(token) = token {
            return self
                .admit_pre_admitted(&mut room, &redemption, proof_digest, name, token, now)
                .await;
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
    async fn admit_pre_admitted(
        &self,
        room: &mut GovernedRoom,
        redemption: &VerifiedRedemption,
        proof_digest: [u8; 32],
        name: &str,
        token: &[u8],
        now: u64,
    ) -> Result<JoinReply, CollaborationError> {
        let invite_id = redemption.join().invite_id();
        let role = room_role(redemption.join().grant().role);
        let label = {
            let grant = room.admissions.grant_for(invite_id, token, now)?;
            if grant.role != role || grant.room != room.room() {
                return Err(CollaborationError::new(
                    "the pre-admission is for another room or role",
                ));
            }
            grant.label.clone()
        };
        let certificate = redemption.certificate();
        self.admit_redemption(
            room,
            certificate.device_public_key,
            redemption.request_id(),
            proof_digest,
            redemption,
            now,
        )
        .await?;
        room.admissions.spend(
            invite_id,
            RoomNotice::PreAdmitted {
                user_public_key: certificate.user_public_key,
                name: name.to_owned(),
                label,
                role,
                at_ns: now,
            },
        )?;
        tracing::info!(
            room = %room.room(),
            invite_id = %hex::encode(invite_id),
            person = %hex::encode(certificate.user_public_key),
            outcome = "pre-admitted",
            "workspace join admitted under the owner's pre-admission"
        );
        Ok(JoinReply::Admitted {
            room: room.room().to_owned(),
            workspace_id: room.workspace_id(),
            name: room.record.name.clone(),
            role,
        })
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
