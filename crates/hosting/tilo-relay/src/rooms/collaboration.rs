//! Invitations, their redemption and member administration of a governed
//! room on this relay.
//!
//! What a governor records between the steps of an exchange is the shared
//! [`JoinLedger`], the same on every host that governs rooms; this module
//! owns who may ask what of this relay's rooms, and the durability barrier
//! before each answer.

use serde::{Deserialize, Serialize};
use tilo_delivery_protocol::workspace_join::WorkspaceJoinService;
use tilo_peer_state::join::{
    JoinBootstrap, JoinChallengeRequest, JoinChallengeResponse, JoinCompleteRequest,
    JoinInspection, JoinProof, JoinTransportRequest, JoinTransportResponse,
    MembershipChallengeRequest, MembershipChallengeResponse, MembershipCompleteRequest,
    VerifiedJoin, VerifiedRedemption,
};
use tilo_peer_state::join_ledger::{Administered, Governed, Signer};
pub use tilo_peer_state::join_ledger::{JoinError as CollaborationError, Redeeming};
use tilo_relay_session::rooms::{
    CancelInvitationRequest, GrantAppAuthorshipRequest, IssueInvitationRequest, IssuedInvitation,
    PendingInvitation, RemoveMemberRequest, SetMemberRoleRequest,
};
use tilo_user_identity::DeviceCert;

use tilo_action_policy::roles::governance;

use super::authority::RoomOp;
use super::{GovernedRoom, RoomRecord, Rooms, system_time};
use crate::host_authority::HostOperation;
use tilo_lock::MutexRecover as _;

pub(super) use tilo_peer_state::join_ledger::JoinLedger as CollaborationState;

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

/// A committed governance change.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GovernanceCompletion {
    /// The room generation that carries it.
    pub generation: u64,
}

/// One waiting for its room's next write: whether it staged a change of
/// the room's pre-admissions as well as of its record, and where the
/// outcome goes.
pub(super) struct Written {
    admissions: bool,
    answer: tokio::sync::oneshot::Sender<Result<(), CollaborationError>>,
}

impl tilo_peer_state::join_ledger::GovernanceMessages for RoomRecord {
    fn governance_messages(&self) -> Vec<Vec<u8>> {
        RoomRecord::governance_messages(self)
    }
}

impl GovernedRoom {
    /// The room as its join ledger reads it: committed now.
    pub(super) fn governed(&self) -> Governed<'_> {
        governed(&self.record, self.peer())
    }
}

/// `record` and `peer`, a room's committed state, as its join ledger reads it.
fn governed<'a>(record: &'a RoomRecord, peer: &'a tilo_peer_state::StatePeer) -> Governed<'a> {
    Governed {
        peer,
        workspace_id: record.workspace_id,
        generation: record.generation,
        governance_messages: record,
    }
}

fn random<const N: usize>() -> Result<[u8; N], CollaborationError> {
    let mut bytes = [0; N];
    getrandom::fill(&mut bytes).map_err(|error| CollaborationError::new(error.to_string()))?;
    Ok(bytes)
}

impl Rooms {
    /// Issue an invitation to `room` for `administrator`: a device of this
    /// relay's own user, or of a member the room lets invite with the
    /// invitation's role.
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
        let issued = {
            let governed = self.room(room).await?;
            let mut governed = governed.lock().await;
            self.authorize(
                administrator,
                Some(&governed),
                RoomOp::Invite(request.role.into()),
                now,
            )?;
            self.stage_issue(&mut governed, administrator, request, now)?
        };
        self.written(room, false).await?;
        Ok(issued)
    }

    /// Record an invitation `request` asks of `room`, unwritten.
    pub(super) fn stage_issue(
        &self,
        room: &mut GovernedRoom,
        administrator: &DeviceCert,
        request: IssueInvitationRequest,
        now: u64,
    ) -> Result<IssuedInvitation, CollaborationError> {
        let role = request.role;
        let transport = vec![self.transport_hint(room)?];
        let mut ledger = room.collaboration().clone();
        let issued = ledger.issue(
            &room.governed(),
            administrator,
            request,
            transport,
            Signer {
                key: &self.inner.host.key,
                nonce: random()?,
            },
            now,
        )?;
        if ledger != *room.collaboration() {
            room.record.collaboration = ledger;
            tracing::info!(
                room = %room.room(),
                invite_id = %hex::encode(issued.invite_id),
                ?role,
                outcome = "issued",
                "workspace invitation issued"
            );
        }
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
        let room = self.room(room).await?;
        let mut room = room.lock().await;
        self.authorize(
            administrator,
            Some(&room),
            RoomOp::Govern(governance::INVITE),
            tilo_clock::unix_now_ns(),
        )?;
        let mut state = room.collaboration().clone();
        if state.cancel(&request)? {
            room.commit_state(state)?;
        }
        Ok(())
    }

    /// The invitations of `room` still redeemable.
    ///
    /// # Errors
    ///
    /// Returns why the listing was refused.
    pub async fn pending(
        &self,
        room: &str,
        administrator: &DeviceCert,
    ) -> Result<Vec<PendingInvitation>, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        let room = self.room(room).await?;
        let room = room.lock().await;
        self.authorize(
            administrator,
            Some(&room),
            RoomOp::Govern(governance::INVITE),
            now,
        )?;
        Ok(room.collaboration().pending(now))
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
        self.authorize(device, None, RoomOp::Host(HostOperation::EnterRoom), now)?;
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
        let room = self.room(room).await?;
        let mut room = room.lock().await;
        self.authorize_role_change(&room, administrator, &request, now)?;
        let key = self.inner.host.key.clone();
        self.administer(
            &mut room,
            request.request_id,
            request_digest(&request)?,
            |peer| {
                peer.governance_mut().set_member_role(
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
        self.authorize(
            administrator,
            None,
            RoomOp::Host(HostOperation::PutRoomGrants),
            now,
        )?;
        let room = self.room(room).await?;
        let mut room = room.lock().await;
        let key = self.inner.host.key.clone();
        self.administer(
            &mut room,
            request_id,
            request_digest(&grants)?,
            |peer| {
                peer.governance_mut()
                    .put_governed_grants(&grants, &key, system_time(now))
            },
            now,
        )
        .await
    }

    /// Make the room's app authorship what its host configured: every app
    /// in `apps` authors the room's content
    /// ([`tilo_action_policy::roles::app_authorship`]), so a member whose
    /// role lets them change the room can do so through it, and every app in
    /// `withdrawn` (one the host made an author before and names no longer)
    /// stops. A room already that way is left as it is, so the host settles
    /// this at every start without a governance change each time. Whether a
    /// member may write stays their role's: a viewer still writes nothing.
    ///
    /// Answers whether anything changed.
    ///
    /// # Errors
    ///
    /// Refuses non-administrators, invalid application ids and changes that
    /// could not commit.
    pub async fn settle_app_authorship(
        &self,
        room: &str,
        administrator: &DeviceCert,
        apps: &std::collections::BTreeSet<String>,
        withdrawn: &std::collections::BTreeSet<String>,
    ) -> Result<bool, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        self.authorize(
            administrator,
            None,
            RoomOp::Host(HostOperation::PutRoomGrants),
            now,
        )?;
        for app in apps.iter().chain(withdrawn) {
            tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::GrantAppAuthorship {
                app: app.clone(),
            }
            .validate()?;
        }
        let state = self.room(room).await?;
        let mut governed = state.lock().await;
        let granted_by =
            tilo_action_policy::Subject::Person(hex::encode(administrator.user_public_key));
        let held = governed
            .peer()
            .permissions(administrator.device_public_key, system_time(now))?
            .policy
            .grants;
        let grants: Vec<tilo_action_policy::Grant> = apps
            .iter()
            .map(|app| tilo_action_policy::roles::app_authorship(room, app, granted_by.clone()))
            .filter(|grant| !held.contains(grant))
            .collect();
        let revoked: Vec<tilo_action_policy::GrantId> = withdrawn
            .iter()
            .filter(|app| !apps.contains(*app))
            .map(|app| tilo_action_policy::roles::app_authorship(room, app, granted_by.clone()).id)
            .filter(|id| held.iter().any(|grant| &grant.id == id))
            .collect();
        if grants.is_empty() && revoked.is_empty() {
            return Ok(false);
        }
        let digest = request_digest(&("app.authorship.settle", &grants, &revoked))?;
        // One request per change from this generation: settling the same
        // configuration again later finds nothing to change and asks nothing.
        let request_id = request_digest(&(digest, governed.record.generation))?;
        let key = self.inner.host.key.clone();
        self.administer(
            &mut governed,
            request_id,
            digest,
            |peer| {
                let added = !grants.is_empty()
                    && peer.governance_mut().put_governed_grants(
                        &grants,
                        &key,
                        system_time(now),
                    )?;
                let removed = !revoked.is_empty()
                    && peer.governance_mut().remove_governed_grants(
                        &revoked,
                        &key,
                        system_time(now),
                    )?;
                Ok(added || removed)
            },
            now,
        )
        .await?;
        drop(governed);
        tracing::info!(
            %room,
            authors = ?apps,
            withdrawn = revoked.len(),
            "room app authorship settled"
        );
        Ok(true)
    }

    /// Make `user_public_key` an administrator of `room`: an editor holding
    /// every governance action, still bounded by their role
    /// ([`tilo_action_policy::roles::administrator_grant`]).
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
        self.authorize(
            administrator,
            None,
            RoomOp::Host(HostOperation::SetAdministrator),
            now,
        )?;
        let room = self.room(room).await?;
        let mut room = room.lock().await;
        let key = self.inner.host.key.clone();
        self.administer(
            &mut room,
            request_id,
            request_digest(&("administrator", user_public_key))?,
            |peer| {
                peer.governance_mut().set_member_administrator(
                    user_public_key,
                    &key,
                    system_time(now),
                )
            },
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
            self.authorize(administrator, Some(&room), RoomOp::Change(&change), now)?;
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
                        changed |=
                            peer.governance_mut()
                                .remove_member(*person, &key, system_time(now))?;
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
        let by = self.authorize(administrator, Some(&room), RoomOp::Change(&change), now)?;
        let key = self.inner.host.key.clone();
        let target = request.user_public_key;
        let completion = self
            .administer(
                &mut room,
                request.request_id,
                request_digest(&(membership, &request))?,
                |peer| match membership {
                    Membership::Remove => {
                        peer.governance_mut()
                            .remove_member(target, &key, system_time(now))
                    }
                    Membership::Ban => {
                        peer.governance_mut()
                            .ban_member(target, &by, &key, system_time(now))
                    }
                    Membership::Unban => {
                        peer.governance_mut()
                            .unban_member(target, &key, system_time(now))
                    }
                    Membership::Restrict(until_ms) => peer.governance_mut().put_governed_grants(
                        &[tilo_action_policy::roles::restriction_grant(
                            &room_id,
                            &person,
                            until_ms,
                            tilo_action_policy::Subject::Person(by.clone()),
                        )],
                        &key,
                        system_time(now),
                    ),
                    Membership::Unrestrict => peer.governance_mut().remove_governed_grants(
                        &[tilo_action_policy::roles::restriction_grant_id(&person)],
                        &key,
                        system_time(now),
                    ),
                },
                now,
            )
            .await?;
        if matches!(membership, Membership::Remove | Membership::Ban) {
            // A removal takes back every way in the room handed them, not
            // only their seat: an invitation issued to them before, still
            // unredeemed, would otherwise admit them again at once.
            let mut state = room.collaboration().clone();
            state.withdraw_invitations(&std::collections::BTreeSet::from([target]));
            room.commit_state(state)?;
        }
        tracing::info!(
            room = %room.room(),
            %person,
            outcome = ?membership,
            "workspace membership change committed"
        );
        Ok(completion)
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
        self.authorize(
            administrator,
            None,
            RoomOp::Host(HostOperation::GrantAppAuthorship),
            now,
        )?;
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
        room.peer()
            .clone()
            .workspace_mut()
            .apply_workspace_membership_command(administrator, &command, &key, system_time(now))?;
        self.administer(
            &mut room,
            request.request_id,
            request_digest(&("app.authorship.grant", &request))?,
            |peer| {
                peer.workspace_mut().apply_workspace_membership_command(
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

    /// Install a bot as a governed member of `room`, update its program,
    /// renew its delegation or uninstall it
    /// ([`WorkspaceMembershipCommand::InstallBot`],
    /// [`WorkspaceMembershipCommand::RenewBot`], `UninstallBot`), as the
    /// room's administrator. Every member reads and enforces the result, so it is
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
        if !matches!(
            command,
            Command::InstallBot(_) | Command::RenewBot(_) | Command::UninstallBot { .. }
        ) {
            return Err(CollaborationError::new(
                "only installing, renewing or uninstalling a bot governs a bot",
            ));
        }
        let now = tilo_clock::unix_now_ns();
        self.authorize(
            administrator,
            None,
            RoomOp::Host(HostOperation::GovernBot),
            now,
        )?;
        command.validate()?;
        // The command names itself: the same install or renewal sent again
        // is the same administration request.
        let digest = request_digest(&("bot.govern", &command))?;
        let room = self.room(room).await?;
        let mut room = room.lock().await;
        let key = self.inner.host.key.clone();
        room.peer()
            .clone()
            .workspace_mut()
            .apply_workspace_membership_command(administrator, &command, &key, system_time(now))?;
        self.administer(
            &mut room,
            digest,
            digest,
            |peer| {
                peer.workspace_mut().apply_workspace_membership_command(
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

    /// Ask `room`'s host to run a bot from the directory, or take the
    /// request back ([`WorkspaceMembershipCommand::RequestBot`],
    /// [`WorkspaceMembershipCommand::WithdrawBotRequest`]), as a member the
    /// room lets install (or uninstall) bots: any such member of a room this
    /// host governs, not only its administrators. The request is a governed
    /// change every member reads; [`Self::bot_requests`] is what the host
    /// follows.
    ///
    /// # Errors
    /// Refuses another command, a malformed request, and a member the room
    /// does not let install (or uninstall) bots.
    ///
    /// [`WorkspaceMembershipCommand::RequestBot`]: tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::RequestBot
    /// [`WorkspaceMembershipCommand::WithdrawBotRequest`]: tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::WithdrawBotRequest
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the room remains locked through the committed and published administration decision"
    )]
    pub async fn ask_for_bot(
        &self,
        room: &str,
        member: &DeviceCert,
        request_id: [u8; 32],
        command: tilo_peer_state::workspace_membership::WorkspaceMembershipCommand,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        use tilo_peer_state::workspace_membership::WorkspaceMembershipCommand as Command;
        let gate = match &command {
            Command::RequestBot(_) => governance::INSTALL_BOT,
            Command::WithdrawBotRequest { .. } => governance::UNINSTALL_BOT,
            _ => {
                return Err(CollaborationError::new(
                    "only asking for a bot or taking the request back asks for a bot",
                ));
            }
        };
        command.validate()?;
        let now = tilo_clock::unix_now_ns();
        let room = self.room(room).await?;
        let mut room = room.lock().await;
        self.authorize(member, Some(&room), RoomOp::Govern(gate), now)?;
        let key = self.inner.host.key.clone();
        room.peer()
            .clone()
            .workspace_mut()
            .apply_workspace_membership_command(member, &command, &key, system_time(now))?;
        self.administer(
            &mut room,
            request_id,
            request_digest(&("bot.request", &command))?,
            |peer| {
                peer.workspace_mut().apply_workspace_membership_command(
                    member,
                    &command,
                    &key,
                    system_time(now),
                )
            },
            now,
        )
        .await
    }

    /// The bots `room` asks its host to run, as its governance records
    /// them now.
    ///
    /// # Errors
    /// The room is not governed here.
    pub async fn bot_requests(
        &self,
        room: &str,
    ) -> Result<Vec<tilo_peer_state::RequestedBot>, CollaborationError> {
        Ok(self
            .room(room)
            .await?
            .lock()
            .await
            .peer()
            .governance()
            .bot_requests())
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
            .governance()
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
        let changed = candidate.workspace_mut().apply_member_effect_receipt(
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
        if let Administered::Done(generation) =
            room.collaboration().administered(request_id, digest)?
        {
            return Ok(GovernanceCompletion { generation });
        }
        let mut candidate = room.peer().clone();
        change(&mut candidate)?;
        let mut state = room.collaboration().clone();
        state.record_administration(request_id, digest, room.record.generation.saturating_add(1));
        let generation =
            room.commit(candidate, &self.inner.host.key, system_time(now), |_, _| {
                Ok(state)
            })?;
        self.settle(room).await?;
        Ok(GovernanceCompletion { generation })
    }

    pub(super) fn inspect(
        room: &GovernedRoom,
        signed_invitation: &[u8],
        now: u64,
    ) -> Result<VerifiedJoin, CollaborationError> {
        room.collaboration()
            .inspect(&room.governed(), signed_invitation, now)
    }

    /// Issue the device at `remote_endpoint` a challenge for `request`,
    /// answered once the room has written it.
    pub(super) async fn challenge(
        &self,
        remote_endpoint: [u8; 32],
        request: JoinChallengeRequest,
        now: u64,
    ) -> Result<JoinChallengeResponse, CollaborationError> {
        let room_id = invitation_room(&request.signed_invitation)?;
        let room = self.room(&room_id).await?;
        let challenge =
            Self::stage_challenge(&mut *room.lock().await, remote_endpoint, request, now)?;
        self.written(&room_id, false).await?;
        Ok(JoinChallengeResponse { challenge })
    }

    /// Wait until `room`'s next write, which takes the room once for every
    /// change staged in it before: a crowd joining wrote the whole room once
    /// per challenge, one after another under its lock.
    /// With `admissions`, the write saves the room's pre-admissions too.
    pub(super) async fn written(
        &self,
        room: &str,
        admissions: bool,
    ) -> Result<(), CollaborationError> {
        let (answer, wait) = tokio::sync::oneshot::channel();
        if self.queue_write(room, Written { admissions, answer }) {
            // Its own task: a request that goes away mid-write must not cut
            // off the answers of everyone else waiting on it.
            let rooms = self.clone();
            let room = room.to_owned();
            tokio::spawn(async move { rooms.write_queued(&room).await });
        }
        wait.await
            .map_err(|_| CollaborationError::new("the room stopped writing"))?
    }

    /// Queue `written` for `room`'s next write. True when it is the first
    /// since the room last took them, so its sender starts the write.
    fn queue_write(&self, room: &str, written: Written) -> bool {
        let mut queued = self.inner.queued_writes.lock_recover();
        let waiting = queued.entry(room.to_owned()).or_default();
        waiting.push(written);
        let first = waiting.len() == 1;
        drop(queued);
        first
    }

    async fn write_queued(&self, room_id: &str) {
        let take = || {
            self.inner
                .queued_writes
                .lock_recover()
                .remove(room_id)
                .unwrap_or_default()
        };
        let (result, waiting) = match self.room(room_id).await {
            Err(error) => (Err(error), take()),
            Ok(room) => {
                let mut room = room.lock().await;
                // Taken under the room: whoever queued staged their change
                // before.
                let waiting = take();
                let mut result = room.persist();
                if result.is_ok() && waiting.iter().any(|written| written.admissions) {
                    result = room.admissions.save();
                }
                drop(room);
                (result.map_err(CollaborationError::new), waiting)
            }
        };
        for written in waiting {
            let _ = written.answer.send(result.clone());
        }
    }

    /// Record a challenge for `request` in `room`, unwritten.
    fn stage_challenge(
        room: &mut GovernedRoom,
        remote_endpoint: [u8; 32],
        request: JoinChallengeRequest,
        now: u64,
    ) -> Result<tilo_peer_state::join::JoinChallenge, CollaborationError> {
        let mut ledger = room.collaboration().clone();
        let challenge =
            ledger.challenge(&room.governed(), remote_endpoint, request, random()?, now)?;
        room.record.collaboration = ledger;
        Ok(challenge)
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
        if let Some(prior) = room.collaboration().completed(
            &room.governed(),
            remote_endpoint,
            request_id,
            proof_digest,
            now,
        )? {
            return Ok(prior);
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
        room.collaboration()
            .redeem(&room.governed(), remote_endpoint, proof, now)
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
        let join = Redeeming {
            remote_endpoint,
            request_id,
            proof_digest,
            redemption,
        };
        self.admit_redemptions(room, &[join], now)
            .await?
            .pop()
            .unwrap_or_else(|| Err(CollaborationError::new("join bootstrap was not built")))
    }

    /// Admit the verified `joins` to `room` in one governance change. Each
    /// is checked on its own and refused alone; everyone admitted is then
    /// committed, recorded and published together, in order. People arriving
    /// together cost the room one write and one publication instead of one
    /// each, and each of those grows with the room.
    ///
    /// Answers each join in order. Fails as a whole when the change cannot
    /// be committed or published.
    pub(super) async fn admit_redemptions(
        &self,
        room: &mut GovernedRoom,
        joins: &[Redeeming<'_>],
        now: u64,
    ) -> Result<Vec<Result<JoinBootstrap, CollaborationError>>, CollaborationError> {
        let mut candidate = room.peer().clone();
        let refusals = room
            .collaboration()
            .admit(&mut candidate, joins, &self.inner.host.key, now);
        for (join, refused) in joins.iter().zip(&refusals) {
            if let Some(error) = refused {
                tracing::warn!(
                    room = %room.room(),
                    invite_id = %hex::encode(join.redemption.join().invite_id()),
                    person = %hex::encode(join.redemption.certificate().user_public_key),
                    outcome = "replay_rejected",
                    reason = %error,
                    "workspace invitation redemption rejected"
                );
            }
        }
        if refusals.iter().all(Option::is_some) {
            return Ok(refusals.into_iter().flatten().map(Err).collect());
        }
        let mut ledger = room.collaboration().clone();
        let mut delivered = Vec::new();
        room.commit(
            candidate,
            &self.inner.host.key,
            system_time(now),
            |record, peer| {
                delivered = ledger
                    .complete(&governed(record, peer), joins, &refusals)
                    .map_err(|error| error.to_string())?;
                Ok(ledger)
            },
        )?;
        self.settle(room).await?;
        for bootstrap in delivered.iter().flatten() {
            tracing::info!(
                room = %bootstrap.room,
                invite_id = %hex::encode(bootstrap.invite_id),
                person = %hex::encode(bootstrap.device_cert.user_public_key),
                device = %hex::encode(bootstrap.device_cert.device_public_key),
                role = ?bootstrap.role,
                outcome = "redeemed",
                "workspace invitation redemption committed"
            );
        }
        Ok(delivered)
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
        let mut state = room.collaboration().clone();
        let challenge = state.membership_challenge(
            &room.governed(),
            remote_endpoint,
            request,
            random()?,
            now,
        )?;
        if state != *room.collaboration() {
            room.commit_state(state)?;
        }
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
        room.collaboration()
            .membership_complete(&room.governed(), remote_endpoint, &request, now)
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
        self.collaboration().redeemed(invite_id)
    }

    /// The certified device admitted by the committed request receipt.
    pub(super) fn admitted_device(&self, request_id: [u8; 32]) -> Option<[u8; 32]> {
        self.collaboration().admitted_device(request_id)
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
                code: error.code(),
                message: error.to_string(),
            })
    }
}

/// The room an invitation names, read without trusting it: the room it
/// names is then verified against what that room issued.
pub(super) fn invitation_room(signed_invitation: &[u8]) -> Result<String, CollaborationError> {
    tilo_peer_state::join_ledger::invitation_room(signed_invitation, tilo_clock::unix_now_ns())
}

pub(super) use tilo_peer_state::join_ledger::request_digest;
