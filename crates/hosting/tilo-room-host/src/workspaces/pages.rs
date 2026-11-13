//! The workspaces as pages reach them (`/rooms`, `/invitation`; see
//! [`tilo_relay_protocol::workspaces`]), and the redemptions of the devices
//! that enter them on their sessions ([`tilo_relay_protocol::entering`]).
//!
//! `/rooms` is workspace administration with the authorization every door has
//! ([`Workspaces::authorize`]): the
//! session's device is one of the relay's own user, or a member's the
//! workspace's governance lets do what it asks. `/invitation` says what an
//! invitation offers. A redemption is the invitation's own approval of whoever
//! made it, so the device's verified redemption is admitted at once, as at
//! every other door, or through the queue under the owner's pre-admission
//! token. The
//! addresses left at the door of a workspace that takes people by invitation
//! are kept with the workspace ([`super::wait_list`]) and listed on their own
//! ([`WorkspacesRequest::WaitList`]).

use tilo_action_policy::roles::governance;
use tilo_peer_state::join::{MemberRole, VerifiedRedemption};
use tilo_relay_protocol::workspaces::{
    Inviter, JoinReply, PendingInvite, PreAdmission, WaitListEntry, Workspace, WorkspaceMember,
    WorkspaceNotice, WorkspaceRole, WorkspacesReply, WorkspacesRequest,
};
use tilo_user_identity::DeviceCert;

use super::authority::WorkspaceOp;
use super::{
    CancelInvitationRequest, CollaborationError, GovernedWorkspace, IssueInvitationRequest,
    IssuedInvitation, RemoveMemberRequest, SetMemberRoleRequest, WorkspaceSummary, Workspaces,
    system_time,
};
use crate::host_authority::HostOperation;
use tilo_lock::MutexRecover as _;
use tilo_relay_protocol::WorkspaceId;

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

/// A pre-admitted redemption waiting for its workspace's next admission pass,
/// and where its answer goes.
pub(super) struct QueuedAdmission {
    presented: Presented,
    proof_digest: [u8; 32],
    name: String,
    token: Vec<u8>,
    answer: tokio::sync::oneshot::Sender<Result<JoinReply, CollaborationError>>,
}

/// An invitation a device redeems on the connection that carries its entry,
/// on which its host checked the device holds its certificate's key: the
/// device signed keying material exported from that connection
/// ([`tilo_relay_protocol::entering`]).
pub(crate) struct Presented {
    pub invitation: Vec<u8>,
    pub cert: DeviceCert,
    pub request_id: [u8; 32],
}

impl Presented {
    /// The device redeeming.
    const fn device(&self) -> [u8; 32] {
        self.cert.device_public_key
    }

    /// The workspace the invitation names, read without trusting it: the
    /// redemption is then verified against what that workspace issued.
    fn workspace(&self, now: u64) -> Result<WorkspaceId, CollaborationError> {
        tilo_peer_state::join_ledger::invitation_workspace(&self.invitation, now)
    }

    /// What the workspace's join receipt keeps of the request, so the same
    /// request again is answered rather than redeemed twice.
    fn digest(&self) -> Result<[u8; 32], CollaborationError> {
        super::collaboration::request_digest(&(
            "tilo/redeemed-on-connection/v1",
            &self.invitation,
            self.cert.device_public_key,
            self.request_id,
        ))
    }

    /// The verified redemption, for an invitation `workspace` issued and
    /// still holds redeemable.
    fn redeem(
        &self,
        workspace: &GovernedWorkspace,
        now: u64,
    ) -> Result<VerifiedRedemption, CollaborationError> {
        workspace.collaboration().redeem_on_connection(
            &workspace.governed(),
            &self.invitation,
            &self.cert,
            self.request_id,
            now,
        )
    }
}

pub(super) const fn member_role(role: WorkspaceRole) -> MemberRole {
    match role {
        WorkspaceRole::Viewer => MemberRole::Viewer,
        WorkspaceRole::Commenter => MemberRole::Commenter,
        WorkspaceRole::Editor => MemberRole::Editor,
        WorkspaceRole::Moderator => MemberRole::Moderator,
    }
}

const fn room_role(role: MemberRole) -> WorkspaceRole {
    match role {
        MemberRole::Viewer => WorkspaceRole::Viewer,
        MemberRole::Commenter => WorkspaceRole::Commenter,
        MemberRole::Editor => WorkspaceRole::Editor,
        MemberRole::Moderator => WorkspaceRole::Moderator,
    }
}

/// `workspace` as a page sees it, with `summary` its people as
/// [`Workspaces::people`] names them, `certificates` the devices its
/// governance records and `owner` the person whose workspace it is.
fn page_room(summary: WorkspaceSummary, certificates: &[DeviceCert], owner: [u8; 32]) -> Workspace {
    let members: Vec<_> = summary
        .members
        .into_iter()
        .map(|member| WorkspaceMember {
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
    Workspace {
        workspace: summary.workspace,
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

impl Workspaces {
    /// Answer one `/rooms` request of `device`, the session's certified
    /// device.
    pub async fn page_rooms(
        &self,
        device: &DeviceCert,
        request: WorkspacesRequest,
    ) -> WorkspacesReply {
        self.page_rooms_inner(device, request)
            .await
            .unwrap_or_else(|error| WorkspacesReply::Refused(error.to_string()))
    }

    async fn page_rooms_inner(
        &self,
        device: &DeviceCert,
        request: WorkspacesRequest,
    ) -> Result<WorkspacesReply, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        Ok(match request {
            WorkspacesRequest::List => self.page_list(device, now).await?,
            WorkspacesRequest::Create { name } => self.page_create(device, &name, now).await?,
            WorkspacesRequest::Place { workspace, name } => {
                self.page_place(device, &workspace, &name, now).await?
            }
            WorkspacesRequest::Enter { workspace } => {
                self.enter(&workspace, device).await?;
                self.page_room(&workspace).await?
            }
            request @ (WorkspacesRequest::Invite { .. }
            | WorkspacesRequest::Cancel { .. }
            | WorkspacesRequest::Pending { .. }
            | WorkspacesRequest::WaitList { .. }
            | WorkspacesRequest::Unlist { .. }
            | WorkspacesRequest::MailInvite { .. }) => {
                self.page_invitations(device, request, now).await?
            }
            request @ (WorkspacesRequest::SetRole { .. }
            | WorkspacesRequest::Remove { .. }
            | WorkspacesRequest::Ban { .. }
            | WorkspacesRequest::Unban { .. }) => self.page_membership(device, request).await?,
            WorkspacesRequest::PreAdmit {
                request_id,
                grant,
                inviter_name,
            } => {
                self.page_pre_admit(device, request_id, grant, inviter_name)
                    .await?
            }
            WorkspacesRequest::Notices { workspace } => {
                self.page_notices(&workspace, device, now).await?
            }
            WorkspacesRequest::Permits { workspace, node } => {
                WorkspacesReply::Permits(self.page_permits(&workspace, device, &node).await?)
            }
            request @ (WorkspacesRequest::Share { .. } | WorkspacesRequest::Unshare { .. }) => {
                self.page_share(device, request).await?
            }
            WorkspacesRequest::Name { workspace, name } => {
                self.name_self(&workspace, device, &name, now).await?
            }
            // Retired: a conversation is an ordinary record of the workspace,
            // so a host names no pair of people.
            WorkspacesRequest::RetiredPair {} => {
                WorkspacesReply::Refused("a conversation is a record of the workspace".into())
            }
            // Retired: an invitation admits by itself, so nobody waits for
            // the owner to let them in.
            WorkspacesRequest::RetiredJoinRequests {}
            | WorkspacesRequest::RetiredAdmit {}
            | WorkspacesRequest::RetiredDecline {} => {
                WorkspacesReply::Refused("an invitation admits by itself".into())
            }
        })
    }

    /// A page's invitations to a workspace, its wait list, and what it does
    /// with them.
    async fn page_invitations(
        &self,
        device: &DeviceCert,
        request: WorkspacesRequest,
        now: u64,
    ) -> Result<WorkspacesReply, CollaborationError> {
        Ok(match request {
            WorkspacesRequest::Invite {
                workspace,
                request_id,
                role,
                expires_at_ns,
                inviter_name,
                wait_list_entry,
                recipient,
            } => {
                let request = IssueInvitationRequest {
                    request_id,
                    role: member_role(role),
                    recipient_user_public_key: recipient,
                    invitee_name: None,
                    expires_at_ns,
                    inviter_name,
                };
                self.page_invitation(&workspace, device, request, wait_list_entry)
                    .await?
            }
            WorkspacesRequest::Cancel {
                workspace,
                request_id,
                invite_id,
            } => {
                self.cancel(
                    &workspace,
                    device,
                    CancelInvitationRequest {
                        request_id,
                        invite_id,
                    },
                )
                .await?;
                self.page_pending(&workspace, device).await?
            }
            WorkspacesRequest::Pending { workspace } => {
                self.page_pending(&workspace, device).await?
            }
            WorkspacesRequest::WaitList { workspace } => {
                WorkspacesReply::WaitList(self.wait_list(&workspace, device, now).await?)
            }
            WorkspacesRequest::Unlist {
                workspace,
                request_id,
            } => {
                self.unlist(&workspace, device, request_id, now).await?;
                WorkspacesReply::WaitList(self.wait_list(&workspace, device, now).await?)
            }
            WorkspacesRequest::MailInvite {
                workspace,
                wait_list_entry,
                link,
            } => {
                self.mail_invite(&workspace, device, wait_list_entry, link, now)
                    .await?;
                WorkspacesReply::WaitList(self.wait_list(&workspace, device, now).await?)
            }
            _ => return Err(CollaborationError::new("not an invitation")),
        })
    }

    /// The workspace's admission notices, for a device that may invite.
    async fn page_notices(
        &self,
        workspace: &str,
        device: &DeviceCert,
        now: u64,
    ) -> Result<WorkspacesReply, CollaborationError> {
        let workspace = self.workspace(workspace).await?;
        let workspace = workspace.lock().await;
        self.authorize(
            device,
            Some(&workspace),
            WorkspaceOp::Govern(governance::INVITE),
            now,
        )?;
        Ok(WorkspacesReply::Notices(workspace.admissions.notices()))
    }

    /// A page's share of one record or item with the workspace, or its end.
    async fn page_share(
        &self,
        device: &DeviceCert,
        request: WorkspacesRequest,
    ) -> Result<WorkspacesReply, CollaborationError> {
        Ok(WorkspacesReply::Shared(match request {
            WorkspacesRequest::Share {
                workspace,
                share,
                node,
                access,
            } => {
                let node = node
                    .parse()
                    .map_err(|_| CollaborationError::new("not a node of the room's tree"))?;
                let access = access
                    .parse()
                    .map_err(|_| CollaborationError::new("a share gives `view` or `edit`"))?;
                let grants = self
                    .share_resource(&workspace, device, &share, &node, access)
                    .await?;
                // Who may do what changed in the workspace: its operators read
                // that here, as they read who was admitted.
                tracing::info!(%workspace, %share, %node, access = access.as_str(), grants, "a share grants the room");
                grants
            }
            WorkspacesRequest::Unshare { workspace, share } => {
                let revoked = self.unshare_resource(&workspace, device, &share).await?;
                tracing::info!(%workspace, %share, revoked, "a share ended");
                revoked
            }
            _ => return Err(CollaborationError::new("not a share")),
        }))
    }

    /// Name `device`'s person `name` in `workspace`: the name the workspace shows for
    /// them from now on. Only a person names themself, on a device the
    /// workspace admitted as theirs. A name they already go by writes nothing.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the workspace stays locked from the check until the name is committed"
    )]
    pub(crate) async fn name_self(
        &self,
        workspace: &str,
        device: &DeviceCert,
        name: &str,
        now: u64,
    ) -> Result<WorkspacesReply, CollaborationError> {
        let name = valid_name(name)?;
        let state = self.workspace(workspace).await?;
        let mut workspace = state.lock().await;
        let at = system_time(now);
        let member = workspace.peer().permissions(device.device_public_key, at)?;
        if member.foreign
            || member.delegate.is_some()
            || member.person != hex::encode(device.user_public_key)
        {
            return Err(CollaborationError::new("only a person names themself"));
        }
        let key = self.inner.host.key.clone();
        let mut candidate = workspace.peer().clone();
        if !candidate
            .governance_mut()
            .name_person(device.user_public_key, &name, &key, at)
            .map_err(CollaborationError::new)?
        {
            return Ok(WorkspacesReply::Named);
        }
        let ledger = workspace.collaboration().clone();
        workspace
            .commit(candidate, &key, at, |_, _| Ok(ledger))
            .map_err(CollaborationError::new)?;
        self.settle(&mut workspace).await?;
        Ok(WorkspacesReply::Named)
    }

    /// Whether `device`'s person may make an undoable change at `node` of
    /// `workspace`, decided as their writes there are.
    async fn page_permits(
        &self,
        workspace: &str,
        device: &DeviceCert,
        node: &str,
    ) -> Result<bool, CollaborationError> {
        let node: tilo_action_policy::TreePath = node
            .parse()
            .map_err(|_| CollaborationError::new("not a node of the room's tree"))?;
        let state = self.workspace(workspace).await?;
        let governed = state.lock().await;
        if node.workspace_id() != Some(governed.workspace()) {
            return Err(CollaborationError::new("the node is not in this room"));
        }
        let now = system_time(tilo_clock::unix_now_ns());
        let member = governed.peer().permissions(device.device_public_key, now)?;
        drop(governed);
        Ok(member.allows(&node, tilo_action_policy::ActionKind::Undoable, now))
    }

    /// A page's change to who is in a workspace and with which role.
    async fn page_membership(
        &self,
        device: &DeviceCert,
        request: WorkspacesRequest,
    ) -> Result<WorkspacesReply, CollaborationError> {
        Ok(match request {
            WorkspacesRequest::SetRole {
                workspace,
                request_id,
                user_public_key,
                role,
            } => {
                self.set_member_role(
                    &workspace,
                    device,
                    SetMemberRoleRequest {
                        request_id,
                        user_public_key,
                        role: member_role(role),
                    },
                )
                .await?;
                self.page_room(&workspace).await?
            }
            WorkspacesRequest::Remove {
                workspace,
                request_id,
                user_public_key,
            } => {
                self.page_remove(&workspace, device, request_id, user_public_key)
                    .await?
            }
            WorkspacesRequest::Ban {
                workspace,
                request_id,
                user_public_key,
            } => {
                self.page_ban(&workspace, device, request_id, user_public_key, true)
                    .await?
            }
            WorkspacesRequest::Unban {
                workspace,
                request_id,
                user_public_key,
            } => {
                self.page_ban(&workspace, device, request_id, user_public_key, false)
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
    ) -> Result<WorkspacesReply, CollaborationError> {
        let request = PreAdmitRequest {
            request_id,
            grant,
            recipient_user_public_key: None,
            inviter_name,
        };
        let issued = self.pre_admit(device, request).await?;
        Ok(WorkspacesReply::Invitation {
            invite_id: issued.invite_id,
            signed_invitation: issued.signed_invitation,
        })
    }

    /// Ban (`ban`) or unban a person, as the workspace's policy lets `device`.
    async fn page_ban(
        &self,
        workspace: &str,
        device: &DeviceCert,
        request_id: [u8; 32],
        user_public_key: [u8; 32],
        ban: bool,
    ) -> Result<WorkspacesReply, CollaborationError> {
        let request = RemoveMemberRequest {
            request_id,
            user_public_key,
        };
        if ban {
            self.ban_member(workspace, device, request).await?;
        } else {
            self.unban_member(workspace, device, request).await?;
        }
        self.page_room(workspace).await
    }

    async fn page_remove(
        &self,
        workspace: &str,
        device: &DeviceCert,
        request_id: [u8; 32],
        user_public_key: [u8; 32],
    ) -> Result<WorkspacesReply, CollaborationError> {
        if user_public_key == self.inner.host.cert.user_public_key {
            return Err(CollaborationError::new("the owner cannot be removed"));
        }
        self.remove_member(
            workspace,
            device,
            RemoveMemberRequest {
                request_id,
                user_public_key,
            },
        )
        .await?;
        self.page_room(workspace).await
    }

    /// Issue the single-use invitation of the owner's grant and record the
    /// grant with it. With a recipient, only that person can redeem it.
    ///
    /// # Errors
    ///
    /// Refuses non-administrators, grants the workspace's owner did not sign,
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
        self.authorize(
            device,
            None,
            WorkspaceOp::Host(HostOperation::PreAdmit),
            now,
        )?;
        grant.verify()?;
        if grant.owner_user_public_key != self.inner.host.cert.user_public_key {
            return Err(CollaborationError::new(
                "only the room's owner pre-admits people",
            ));
        }
        let identity = grant.workspace.clone();
        let issued = {
            let workspace = self.workspace(&identity).await?;
            let mut workspace = workspace.lock().await;
            let issued = self.stage_issue(
                &mut workspace,
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
            workspace.admissions.record(
                issued.invite_id,
                grant,
                now,
                self.inner.host.max_pre_admissions,
            )?;
            issued
        };
        // The invitation and its grant are written with whatever else the
        // workspace staged meanwhile: a crowd's pre-admissions took the workspace for
        // two whole writes each, one after another.
        self.written(&identity, true).await?;
        Ok(issued)
    }

    async fn page_create(
        &self,
        device: &DeviceCert,
        name: &str,
        now: u64,
    ) -> Result<WorkspacesReply, CollaborationError> {
        self.authorize(
            device,
            None,
            WorkspaceOp::Host(HostOperation::CreateWorkspace),
            now,
        )?;
        let created = self.create(name).await?;
        self.page_room(&created.workspace).await
    }

    /// Govern `workspace` here unless this host does, created as `name`, and
    /// admit `device` to it as its owner.
    async fn page_place(
        &self,
        device: &DeviceCert,
        workspace: &str,
        name: &str,
        now: u64,
    ) -> Result<WorkspacesReply, CollaborationError> {
        self.authorize(
            device,
            None,
            WorkspaceOp::Host(HostOperation::CreateWorkspace),
            now,
        )?;
        if !self.inner.workspaces.lock().await.contains_key(workspace)
            && let Err(error) = self.create_workspace(name, Some(workspace)).await
            // Another device of the person placed it meanwhile.
            && !self.inner.workspaces.lock().await.contains_key(workspace)
        {
            return Err(error.into());
        }
        self.enter(workspace, device).await?;
        self.page_room(workspace).await
    }

    async fn page_list(
        &self,
        device: &DeviceCert,
        now: u64,
    ) -> Result<WorkspacesReply, CollaborationError> {
        self.authorize(
            device,
            None,
            WorkspaceOp::Host(HostOperation::ListWorkspaces),
            now,
        )?;
        let workspaces = self
            .inner
            .workspaces
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let mut listed = Vec::with_capacity(workspaces.len());
        for workspace in workspaces {
            let workspace = workspace.lock().await;
            if !self.lists_to(device, workspace.workspace()) {
                continue;
            }
            let summary = self.people(workspace.workspace(), &workspace);
            let certificates = workspace.peer().certificates();
            let owner = workspace.peer().owner();
            drop(workspace);
            listed.push(page_room(summary, &certificates, owner));
        }
        Ok(WorkspacesReply::Workspaces(listed))
    }

    /// Issue the invitation `request` asks; for the wait-list entry
    /// `wait_list_entry`, noted on it, so it shows as invited and leaves the
    /// list once someone joins with it.
    async fn page_invitation(
        &self,
        workspace: &str,
        device: &DeviceCert,
        request: IssueInvitationRequest,
        wait_list_entry: Option<[u8; 32]>,
    ) -> Result<WorkspacesReply, CollaborationError> {
        let Some(entry) = wait_list_entry else {
            let issued = self.issue(workspace, device, request).await?;
            return Ok(WorkspacesReply::Invitation {
                invite_id: issued.invite_id,
                signed_invitation: issued.signed_invitation,
            });
        };
        let now = tilo_clock::unix_now_ns();
        let issued = {
            let governed = self.workspace(workspace).await?;
            let mut governed = governed.lock().await;
            self.authorize(
                device,
                Some(&governed),
                WorkspaceOp::Invite(request.role.into()),
                now,
            )?;
            if !governed.wait_list.has(entry, now) {
                return Err(CollaborationError::new(
                    "that address is no longer on the wait list",
                ));
            }
            let issued = self.stage_issue(&mut governed, device, request, now)?;
            governed
                .wait_list
                .invited(entry, issued.invite_id, now)
                .map_err(CollaborationError::new)?;
            issued
        };
        self.written(workspace, false).await?;
        Ok(WorkspacesReply::Invitation {
            invite_id: issued.invite_id,
            signed_invitation: issued.signed_invitation,
        })
    }

    async fn page_room(&self, workspace: &str) -> Result<WorkspacesReply, CollaborationError> {
        let workspace = self.workspace(workspace).await?;
        let workspace = workspace.lock().await;
        let summary = self.people(workspace.workspace(), &workspace);
        let certificates = workspace.peer().certificates();
        let owner = workspace.peer().owner();
        drop(workspace);
        Ok(WorkspacesReply::Workspace(page_room(
            summary,
            &certificates,
            owner,
        )))
    }

    async fn page_pending(
        &self,
        workspace: &str,
        device: &DeviceCert,
    ) -> Result<WorkspacesReply, CollaborationError> {
        Ok(WorkspacesReply::Pending(
            self.pending(workspace, device)
                .await?
                .into_iter()
                .map(|invitation| PendingInvite {
                    invite_id: invitation.invite_id,
                    role: room_role(invitation.role),
                    expires_at_ns: invitation.expires_at_ns,
                    recipient: invitation.recipient_user_public_key,
                    invitee_name: invitation.invitee_name,
                })
                .collect(),
        ))
    }

    /// The addresses left at `workspace`'s door, oldest first, for someone the
    /// workspace lets invite.
    async fn wait_list(
        &self,
        workspace: &str,
        device: &DeviceCert,
        now: u64,
    ) -> Result<Vec<WaitListEntry>, CollaborationError> {
        let governed = self.workspace(workspace).await?;
        let mut governed = governed.lock().await;
        self.authorize(
            device,
            Some(&governed),
            WorkspaceOp::Govern(governance::INVITE),
            now,
        )?;
        // Whoever joined by a way that did not take them off already.
        governed.forget_joined_wait_list();
        Ok(governed.wait_list.entries(now))
    }

    /// Take the address `request_id` off `workspace`'s wait list, for someone the
    /// workspace lets invite.
    async fn unlist(
        &self,
        workspace: &str,
        device: &DeviceCert,
        request_id: [u8; 32],
        now: u64,
    ) -> Result<(), CollaborationError> {
        let governed = self.workspace(workspace).await?;
        let mut governed = governed.lock().await;
        self.authorize(
            device,
            Some(&governed),
            WorkspaceOp::Govern(governance::INVITE),
            now,
        )?;
        if !governed
            .wait_list
            .remove(request_id, now)
            .map_err(CollaborationError::new)?
        {
            return Err(CollaborationError::new("no such address on the wait list"));
        }
        drop(governed);
        tracing::info!(%workspace, "an address was taken off the wait list");
        Ok(())
    }

    /// Mail the invitation `link` to the confirmed wait-list entry
    /// `request_id` of `workspace`, for someone the workspace lets invite, once the
    /// relay sends mail ([`Workspaces::mail_wait_list`]).
    async fn mail_invite(
        &self,
        workspace: &str,
        device: &DeviceCert,
        request_id: [u8; 32],
        link: String,
        now: u64,
    ) -> Result<(), CollaborationError> {
        let Some((door, mail)) = self.inner.door.mail.get() else {
            return Err(CollaborationError::new("this door sends no mail"));
        };
        if !tilo_relay_protocol::workspaces::invitation_on_door(door, &link) {
            return Err(CollaborationError::new(
                "only an invitation on this door is mailed",
            ));
        }
        let governed = self.workspace(workspace).await?;
        let mut governed = governed.lock().await;
        self.authorize(
            device,
            Some(&governed),
            WorkspaceOp::Govern(governance::INVITE),
            now,
        )?;
        let (to, removal) = governed
            .wait_list
            .mail_invite(request_id, now)
            .map_err(CollaborationError::new)?
            .map_err(CollaborationError::new)?;
        drop(governed);
        let (queued, accepted) = tokio::sync::oneshot::channel();
        let sent = mail
            .send(super::WaitListMail {
                queued,
                to,
                link,
                removal,
            })
            .is_ok();
        let accepted = if sent {
            match tokio::time::timeout(std::time::Duration::from_secs(10), accepted).await {
                Ok(Ok(queued)) => Some(queued),
                Ok(Err(_)) | Err(_) => None,
            }
        } else {
            Some(false)
        };
        if accepted == Some(false) {
            use sha2::Digest as _;
            let mut governed = self.workspace(workspace).await?.lock_owned().await;
            governed
                .wait_list
                .mail_failed(
                    sha2::Sha256::digest(removal).into(),
                    tilo_clock::unix_now_ns(),
                )
                .map_err(CollaborationError::new)?;
            drop(governed);
            return Err(CollaborationError::new(
                "the door did not accept the invitation mail",
            ));
        }
        if accepted.is_none() {
            // Acceptance can precede a lost acknowledgement. Keep the
            // reservation so an uncertain outcome cannot duplicate mail.
            return Err(CollaborationError::new(
                "invitation mail acceptance is uncertain; it may already be queued",
            ));
        }
        tracing::info!(%workspace, "an invitation was queued for an address on the wait list");
        Ok(())
    }

    /// Keep the request of someone who asked at `workspace`'s door without an
    /// invitation, to be invited at `contact` (an address
    /// [`tilo_relay_protocol::workspaces::contact_address`] accepts), while the
    /// workspace keeps fewer than `max`. Whoever may invite sees it on the workspace's
    /// wait list until it leaves it, once confirmed; whoever holds the
    /// secret whose SHA-256 is `secrets.removal` takes it off
    /// ([`Self::leave_wait_list`]), and the one whose SHA-256 is
    /// `secrets.confirm` confirms it ([`Self::confirm_wait_list`]).
    ///
    /// # Errors
    ///
    /// Not an address, the workspace is unknown, or its wait list could not be
    /// written.
    pub async fn wait(
        &self,
        workspace: &str,
        contact: &str,
        secrets: super::WaitListSecrets,
        language: &str,
        max: usize,
    ) -> Result<super::WaitListed, CollaborationError> {
        let contact = tilo_relay_protocol::workspaces::contact_address(contact)
            .map_err(CollaborationError::new)?;
        let governed = self.workspace(workspace).await?;
        let mut governed = governed.lock().await;
        let kept = governed
            .wait_list
            .keep(&contact, secrets, language, tilo_clock::unix_now_ns(), max)
            .map_err(CollaborationError::new)?;
        drop(governed);
        tracing::info!(%workspace, outcome = ?kept, "a request was left at the door");
        Ok(kept)
    }

    /// Confirm the request whose confirmation secret has the SHA-256
    /// `confirm` on `workspace`'s wait list: whether one had it.
    ///
    /// # Errors
    ///
    /// The workspace is unknown, or its wait list could not be written.
    pub async fn confirm_wait_list(
        &self,
        workspace: &str,
        confirm: [u8; 32],
    ) -> Result<bool, CollaborationError> {
        let governed = self.workspace(workspace).await?;
        let mut governed = governed.lock().await;
        let confirmed = governed
            .wait_list
            .confirm(confirm, tilo_clock::unix_now_ns())
            .map_err(CollaborationError::new)?;
        drop(governed);
        if confirmed {
            tracing::info!(%workspace, "an address on the wait list was confirmed");
        }
        Ok(confirmed)
    }

    /// Where the request whose removal secret has the SHA-256 `removal`
    /// stands on `workspace`'s wait list, for whoever left it.
    ///
    /// # Errors
    ///
    /// The workspace is unknown.
    pub async fn wait_list_standing(
        &self,
        workspace: &str,
        removal: [u8; 32],
    ) -> Result<tilo_relay_protocol::operator::WaitListStanding, CollaborationError> {
        let governed = self.workspace(workspace).await?;
        let mut governed = governed.lock().await;
        governed.forget_joined_wait_list();
        Ok(governed
            .wait_list
            .standing(removal, tilo_clock::unix_now_ns()))
    }

    /// Forget the addresses on `workspace`'s wait list that were not confirmed
    /// in time.
    ///
    /// # Errors
    ///
    /// The workspace is unknown, or its wait list could not be written.
    pub async fn expire_wait_list(&self, workspace: &str) -> Result<(), CollaborationError> {
        let governed = self.workspace(workspace).await?;
        let mut governed = governed.lock().await;
        let expired = governed
            .wait_list
            .expire(tilo_clock::unix_now_ns())
            .map_err(CollaborationError::new)?;
        drop(governed);
        if expired > 0 {
            tracing::info!(%workspace, expired, "unconfirmed addresses left the wait list");
        }
        Ok(())
    }

    /// Take the request whose removal secret has the SHA-256 `removal` off
    /// `workspace`'s wait list, for whoever left it. Answers alike whether or not
    /// one had that secret, so the answer tells nobody what is listed; where
    /// to tell the address it left comes back only to whoever runs the workspace,
    /// once the address had been confirmed.
    ///
    /// # Errors
    ///
    /// The workspace is unknown, or its wait list could not be written.
    pub async fn leave_wait_list(
        &self,
        workspace: &str,
        removal: [u8; 32],
    ) -> Result<Option<super::Mailbox>, CollaborationError> {
        self.leave_wait_list_for_operator(workspace, removal)
            .await
            .map(|(told, _)| told)
    }

    /// Remove an entry for the private door operator, returning its stable
    /// contact digest for cancelling mail even when it was not confirmed.
    /// Neither this digest nor the mailbox belongs in a public response.
    ///
    /// # Errors
    /// The workspace is unknown, or its wait list could not be written.
    pub async fn leave_wait_list_for_operator(
        &self,
        workspace: &str,
        removal: [u8; 32],
    ) -> Result<(Option<super::Mailbox>, Option<[u8; 32]>), CollaborationError> {
        let governed = self.workspace(workspace).await?;
        let mut governed = governed.lock().await;
        let now = tilo_clock::unix_now_ns();
        let contact = governed.wait_list.removal_contact(removal, now);
        let told = governed
            .wait_list
            .leave(removal, now)
            .map_err(CollaborationError::new)?;
        drop(governed);
        if contact.is_some() {
            tracing::info!(%workspace, "someone took their address off the wait list");
        }
        Ok((told, contact))
    }

    /// What `invitation` offers: its workspace, the part it grants, who sent
    /// it and how many people are there; or why this host would not honour
    /// it.
    pub async fn inspect_invitation(&self, invitation: &[u8]) -> JoinReply {
        self.inspected(invitation)
            .await
            .unwrap_or_else(|error| JoinReply::Refused(error.to_string()))
    }

    async fn inspected(&self, invitation: &[u8]) -> Result<JoinReply, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        let workspace = self
            .workspace(
                &tilo_peer_state::join_ledger::invitation_workspace(invitation, now)?.to_string(),
            )
            .await?;
        let workspace = workspace.lock().await;
        let join = workspace
            .collaboration()
            .inspect(&workspace.governed(), invitation, now)?;
        Ok(JoinReply::Invitation {
            workspace: workspace.workspace().to_owned(),
            workspace_id: workspace.workspace_id(),
            name: workspace.record.name.clone(),
            role: room_role(join.grant().role),
            expires_at_ns: join.grant().expires_at_ns,
            inviter: join.inviter().map(|inviter| Inviter {
                user_public_key: inviter.user_public_key,
                name: inviter.name.clone(),
            }),
            members: self.people(workspace.workspace(), &workspace).people(),
        })
    }

    /// Admit the device `cert` certifies with `invitation`, under the request
    /// `request_id`, shown to the workspace's people as `name`, with the
    /// owner's pre-admission `token` where it was issued under one. The
    /// caller vouches that the device proved it holds its key on the
    /// connection that carries the redemption: the host's session entry
    /// ([`tilo_relay_protocol::entering`]) checks that proof before it gets
    /// here.
    pub async fn admit_proven(
        &self,
        invitation: Vec<u8>,
        cert: DeviceCert,
        request_id: [u8; 32],
        name: &str,
        token: Option<&[u8]>,
    ) -> JoinReply {
        self.redeem_on_connection(
            Presented {
                invitation,
                cert,
                request_id,
            },
            name,
            token,
        )
        .await
    }

    /// Admit the device `presented` names with its invitation, then answer
    /// once the governance that admitted it is retained
    /// ([`Self::retained_for`]): what a session entering with an invitation,
    /// or through a door, is answered.
    pub(crate) async fn redeem_on_connection(
        &self,
        presented: Presented,
        name: &str,
        token: Option<&[u8]>,
    ) -> JoinReply {
        let now = tilo_clock::unix_now_ns();
        self.presented(presented, name, token, now)
            .await
            .unwrap_or_else(|error| JoinReply::Refused(error.to_string()))
    }

    async fn presented(
        &self,
        presented: Presented,
        name: &str,
        token: Option<&[u8]>,
        now: u64,
    ) -> Result<JoinReply, CollaborationError> {
        let proving = crate::timing::start();
        let reply = self
            .prove(presented, &valid_name(name)?, token, now)
            .await?;
        crate::timing::event("join.prove.admitted", proving, 0);
        let retaining = crate::timing::start();
        self.retained_for(&reply).await;
        crate::timing::event("join.prove.retained", retaining, 0);
        crate::timing::event("join.prove", proving, 0);
        Ok(reply)
    }

    /// Wait, off the workspace's lock, until the governance that admitted the
    /// device `reply` answers is retained. The page subscribes as soon as it
    /// hears it is in: a replay that began before that governance was written
    /// would tell it the workspace without it, and its channels would wait for
    /// the catch-up to learn it may read them.
    pub(super) async fn retained_for(&self, reply: &JoinReply) {
        if let JoinReply::Admitted { workspace_id, .. } = reply {
            self.inner.host.records.settled(*workspace_id).await;
        }
    }

    #[expect(
        clippy::significant_drop_tightening,
        reason = "the workspace stays locked while the request is admitted"
    )]
    async fn prove(
        &self,
        presented: Presented,
        name: &str,
        token: Option<&[u8]>,
        now: u64,
    ) -> Result<JoinReply, CollaborationError> {
        if let Some(token) = token {
            return self.prove_pre_admitted(presented, name, token, now).await;
        }
        let identity = presented.workspace(now)?;
        let request_id = presented.request_id;
        let device = presented.device();
        let proof_digest = presented.digest()?;
        let governed = self.workspace(&identity.to_string()).await?;
        let mut workspace = governed.lock().await;
        // A page that asked again under the same request — a reload, a lost
        // answer — hears what became of it, not that its invitation is spent.
        if workspace.admitted_device(request_id) == Some(device) {
            return Self::admitted(&workspace, device, now);
        }
        let redemption = presented.redeem(&workspace, now)?;
        let invite_id = redemption.join().invite_id();
        if workspace.redeemed(invite_id) {
            return Err(CollaborationError::new("invitation was already redeemed"));
        }
        // An invitation is the approval of whoever made it: a page that holds
        // one is let in by its issuer's own grant, as at every other door, and
        // never waits for someone in the workspace to answer for it.
        let issuer = redemption.join().grant().issuer.clone();
        self.authorize(
            &issuer,
            Some(&workspace),
            WorkspaceOp::Invite(redemption.join().grant().role.into()),
            now,
        )?;
        let join = super::collaboration::Redeeming {
            remote_endpoint: device,
            request_id,
            proof_digest,
            redemption: &redemption,
            name: Some(name),
        };
        self.admit_redemptions(&mut workspace, &[join], now)
            .await?
            .pop()
            .unwrap_or_else(|| Err(CollaborationError::new("join bootstrap was not built")))?;
        tracing::info!(
            workspace = %identity,
            invite_id = %hex::encode(invite_id),
            person = %hex::encode(device),
            outcome = "admitted",
            "workspace join admitted with its invitation"
        );
        Self::admitted(&workspace, device, now)
    }

    /// What a device the workspace just admitted hears: the workspace it is in
    /// and the part it holds there.
    pub(super) fn admitted(
        workspace: &GovernedWorkspace,
        device: [u8; 32],
        now: u64,
    ) -> Result<JoinReply, CollaborationError> {
        let role = workspace
            .peer()
            .permissions(device, system_time(now))
            .map_err(|error| CollaborationError::new(error.to_string()))?;
        Ok(JoinReply::Admitted {
            workspace: workspace.workspace().to_owned(),
            workspace_id: workspace.workspace_id(),
            name: workspace.record.name.clone(),
            role: match role.role {
                collab_types::role::Role::Moderator => WorkspaceRole::Moderator,
                collab_types::role::Role::Commenter => WorkspaceRole::Commenter,
                collab_types::role::Role::Viewer => WorkspaceRole::Viewer,
                collab_types::role::Role::Owner | collab_types::role::Role::Editor => {
                    WorkspaceRole::Editor
                }
            },
        })
    }

    /// Admit the redeeming device at once under the owner's pre-admission
    /// for its invitation, spend the grant and tell the owner.
    ///
    /// A public workspace admits a crowd this way, and each admission is a write
    /// and a publication of the workspace that grow with it. So redemptions only
    /// queue here; the first since the workspace last took them starts a pass
    /// that takes the workspace once and admits all of them in one change.
    async fn prove_pre_admitted(
        &self,
        presented: Presented,
        name: &str,
        token: &[u8],
        now: u64,
    ) -> Result<JoinReply, CollaborationError> {
        let workspace = presented.workspace(now)?;
        let proof_digest = presented.digest()?;
        let (answer, answered) = tokio::sync::oneshot::channel();
        let first = self.queue_admission(
            &workspace.to_string(),
            QueuedAdmission {
                presented,
                proof_digest,
                name: name.to_owned(),
                token: token.to_vec(),
                answer,
            },
        );
        if first {
            // Its own task: a page that goes away mid-pass must not cut off
            // the answers of everyone else in it.
            let workspaces = self.clone();
            tokio::spawn(async move { workspaces.admit_queued(&workspace.to_string()).await });
        }
        answered
            .await
            .map_err(|_| CollaborationError::new("the room stopped admitting"))?
    }

    /// Queue `join` for `workspace`'s next admission pass. True when it is the
    /// first since the workspace last took them, so its sender starts the pass.
    fn queue_admission(&self, workspace: &str, join: QueuedAdmission) -> bool {
        let mut queued = self.inner.queued_admissions.lock_recover();
        let waiting = queued.entry(workspace.to_owned()).or_default();
        waiting.push(join);
        let first = waiting.len() == 1;
        drop(queued);
        first
    }

    /// Admit every pre-admitted redemption queued for `workspace`.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the workspace stays locked until its admissions are committed, published and answered"
    )]
    async fn admit_queued(&self, identity: &str) {
        let locking = crate::timing::start();
        let workspace = self.workspace(identity).await;
        let mut locked = match &workspace {
            Ok(workspace) => Some(workspace.lock().await),
            Err(_) => None,
        };
        let queued = self
            .inner
            .queued_admissions
            .lock_recover()
            .remove(identity)
            .unwrap_or_default();
        crate::timing::event("join.queued.locked", locking, queued.len() as u64);
        let admitting = crate::timing::start();
        let answers = match (&workspace, locked.as_mut()) {
            (Ok(_), Some(workspace)) => self.admit_pre_admitted(workspace, &queued).await,
            (Err(error), _) => vec![Err(error.clone()); queued.len()],
            (Ok(_), None) => Vec::new(),
        };
        crate::timing::event("join.queued.admit", admitting, queued.len() as u64);
        for (queued, answer) in queued.into_iter().zip(answers) {
            let _ = queued.answer.send(answer);
        }
    }

    /// Check each queued redemption alone, then admit those that hold in one
    /// change and spend their grants in one write. Answers each in order.
    async fn admit_pre_admitted(
        &self,
        workspace: &mut GovernedWorkspace,
        queued: &[QueuedAdmission],
    ) -> Vec<Result<JoinReply, CollaborationError>> {
        let now = tilo_clock::unix_now_ns();
        let mut answers: Vec<Option<Result<JoinReply, CollaborationError>>> =
            Vec::with_capacity(queued.len());
        let mut admitting = Vec::new();
        let mut invites = std::collections::BTreeSet::new();
        for (index, join) in queued.iter().enumerate() {
            match self.check_pre_admitted(workspace, join, &mut invites, now) {
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
                    name: queued.get(*index).map(|join| join.name.as_str()),
                },
            )
            .collect();
        let admitted = if joins.is_empty() {
            Ok(Vec::new())
        } else {
            self.admit_redemptions(workspace, &joins, now).await
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
                            WorkspaceNotice::PreAdmitted {
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
                            workspace = %workspace.workspace(),
                            invite_id = %hex::encode(invite_id),
                            person = %hex::encode(certificate.user_public_key),
                            outcome = "pre-admitted",
                            "workspace join admitted under the owner's pre-admission"
                        );
                        JoinReply::Admitted {
                            workspace: workspace.workspace().to_owned(),
                            workspace_id: workspace.workspace_id(),
                            name: workspace.record.name.clone(),
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
            && let Err(error) = workspace.admissions.spend(spent)
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
        workspace: &GovernedWorkspace,
        join: &QueuedAdmission,
        invites: &mut std::collections::BTreeSet<[u8; 32]>,
        now: u64,
    ) -> Result<
        (VerifiedRedemption, String, WorkspaceRole),
        Box<Result<JoinReply, CollaborationError>>,
    > {
        let redemption = join
            .presented
            .redeem(workspace, now)
            .map_err(|error| Box::new(Err(error)))?;
        let invite_id = redemption.join().invite_id();
        if workspace.redeemed(invite_id) || !invites.insert(invite_id) {
            return Err(Box::new(Err(CollaborationError::new(
                "invitation was already redeemed",
            ))));
        }
        let role = room_role(redemption.join().grant().role);
        let grant = workspace
            .admissions
            .grant_for(invite_id, &join.token, now)
            .map_err(|error| Box::new(Err(CollaborationError::from(error))))?;
        self.pre_admission_signer(workspace, grant)
            .map_err(|error| Box::new(Err(CollaborationError::from(error))))?;
        if grant.role != role || grant.workspace != workspace.workspace() {
            return Err(Box::new(Err(CollaborationError::new(
                "the pre-admission is for another room or role",
            ))));
        }
        Ok((redemption, grant.label.clone(), role))
    }
}
