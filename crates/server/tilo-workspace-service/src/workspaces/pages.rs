//! The workspaces as pages reach them (`/workspaces`, `/invitation`; see
//! [`tilo_server_protocol::workspaces`]), and the redemptions of the devices
//! that enter them on their sessions ([`tilo_server_protocol::entering`]).
//!
//! `/workspaces` is workspace administration with the authorization every action API has
//! ([`Workspaces::authorize`]): the
//! session's device is one of the server's own user, or a member's the
//! workspace's governance lets do what it asks. `/invitation` says what an
//! invitation offers. A redemption is the invitation's own approval of whoever
//! made it, so the device's verified redemption is admitted at once, as at
//! every other action API, or through the queue under the owner's pre-admission
//! token. The
//! addresses left at the entry of a workspace that takes people by invitation
//! are kept with the workspace ([`super::wait_list`]) and listed on their own
//! ([`WorkspacesRequest::WaitList`]).

use std::collections::BTreeSet;

use tilo_action_policy::roles::governance;
use tilo_peer_state::join::VerifiedRedemption;
use tilo_server_protocol::workspaces::{
    DocumentAccess, Inviter, JoinReply, PendingInvite, PreAdmission, Role, ServerEntry,
    WaitListEntry, Workspace, WorkspaceHolder, WorkspaceNotice, WorkspacesReply, WorkspacesRequest,
};
use tilo_user_identity::DeviceCert;

use super::authority::WorkspaceOp;
use super::collaboration::carried;
use super::hosts::holders_of;
use super::{
    CancelInvitationRequest, CollaborationError, GovernedWorkspace, IssueInvitationRequest,
    IssuedInvitation, WorkspaceSummary, Workspaces, system_time,
};
use crate::service_authority::ServiceOperation;
use tilo_lock::MutexRecover as _;
use tilo_server_protocol::WorkspaceId;

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

/// What the card that opens an invitation says of the workspace itself: its
/// name, how many people it has, and the name it shows each of them by.
struct Showing {
    name: String,
    members: usize,
    names: std::collections::BTreeMap<[u8; 32], String>,
}

/// An invitation a device redeems on the connection that carries its entry,
/// on which its host checked the device holds its certificate's key: the
/// device signed keying material exported from that connection
/// ([`tilo_server_protocol::entering`]).
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
        tilo_peer_state::join_log::invitation_workspace(&self.invitation, now)
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
    /// still holds redeemable, at the device answering for it.
    fn redeem(
        &self,
        workspace: &GovernedWorkspace,
        answering: &DeviceCert,
        now: u64,
    ) -> Result<VerifiedRedemption, CollaborationError> {
        workspace.collaboration().redeem_on_connection(
            &workspace.governed(answering),
            &self.invitation,
            &self.cert,
            self.request_id,
            now,
        )
    }
}

/// `workspace` as a page sees it, with `summary` its people as its
/// governance keeps them, its owner among them, `holders` the holders of it,
/// and `reader` the person the page's device acts for.
fn page_workspace(
    summary: WorkspaceSummary,
    holders: Vec<WorkspaceHolder>,
    reader: [u8; 32],
) -> Workspace {
    let members: Vec<_> = summary
        .members
        .into_iter()
        .map(|mut member| {
            let root = member.root();
            // A person the workspace has not named stays without a name: their device's label
            // names the device, never them.
            // The page shows its reader's own row as theirs, and reads from it
            // what they may do here.
            if root == Some(reader) {
                member = member.claiming_self();
            }
            member
        })
        .collect();
    Workspace {
        workspace: summary.workspace,
        workspace_id: summary.workspace_id,
        name: summary.name,
        generation: summary.generation,
        members,
        holders,
    }
}

fn valid_name(name: &str) -> Result<String, CollaborationError> {
    tilo_user_identity::display_name(name)
        .map(str::to_owned)
        .map_err(|error| CollaborationError::new(error.reason()))
}

/// The name a device joins under: a blank one is none given, so its person stays without a name
/// until they give one; any other must be a display name.
fn joined_name(name: &str) -> Result<String, CollaborationError> {
    if name.trim().is_empty() {
        return Ok(String::new());
    }
    valid_name(name)
}

impl Workspaces {
    /// Answer one `/workspaces` request of `device`, the session's certified
    /// device.
    pub async fn page_workspaces(
        &self,
        device: &DeviceCert,
        request: WorkspacesRequest,
    ) -> WorkspacesReply {
        self.page_workspaces_inner(device, request)
            .await
            .unwrap_or_else(|error| WorkspacesReply::Refused(error.to_string()))
    }

    async fn page_document_access(
        &self,
        workspace: &str,
        device: &DeviceCert,
        now: u64,
    ) -> Result<DocumentAccess, CollaborationError> {
        let state = self.workspace(workspace).await?;
        let workspace = state.lock().await;
        let peer = workspace.peer();
        let at = system_time(now);
        let member = peer.permissions(device.device_public_key, at)?;
        member
            .policy
            .check_action(
                &member.authority(),
                governance::GRANT,
                &tilo_action_policy::Context::at_time(at),
                false,
            )
            .map_err(|refusal| CollaborationError::new(refusal.to_string()))?;
        let documents: BTreeSet<_> = peer
            .records()
            .into_iter()
            .filter_map(|(app, document)| {
                let document = collab_types::document_access_target(&app, &document)?.to_owned();
                (!tilo_action_policy::roles::is_private_record(&app, &document)
                    && peer
                        .check_document_read(device.device_public_key, &app, &document, at)
                        .is_ok())
                .then_some((app, document))
            })
            .collect();
        drop(peer);
        drop(workspace);
        let documents: Vec<_> = documents.into_iter().collect();
        let documents = collab_types::document_access_scopes(&member.policy, &documents, []);
        let rules = collab_types::document_access_rules(&member.policy, &documents);
        Ok(DocumentAccess { documents, rules })
    }

    async fn page_workspaces_inner(
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
                self.page_workspace(&workspace, device).await?
            }
            request @ (WorkspacesRequest::Invite { .. }
            | WorkspacesRequest::Cancel { .. }
            | WorkspacesRequest::Pending { .. }
            | WorkspacesRequest::WaitList { .. }
            | WorkspacesRequest::Unlist { .. }
            | WorkspacesRequest::MailInvite { .. }) => {
                self.page_invitations(device, request, now).await?
            }
            WorkspacesRequest::EditRoles {
                workspace,
                request_id,
                change,
            } => {
                self.edit_member_list(&workspace, device, request_id, change)
                    .await?;
                self.page_roles(&workspace, device, now).await?
            }
            WorkspacesRequest::Roles { workspace } => {
                self.page_roles(&workspace, device, now).await?
            }
            WorkspacesRequest::DocumentAccess { workspace } => WorkspacesReply::DocumentAccess(
                self.page_document_access(&workspace, device, now).await?,
            ),
            WorkspacesRequest::SetDocumentAccess {
                workspace,
                request_id,
                app,
                document,
                recipient,
                access,
            } => {
                self.set_document_access(
                    &workspace,
                    device,
                    request_id,
                    (&recipient, &app, &document),
                    &access,
                    now,
                )
                .await?;
                self.page_workspace(&workspace, device).await?
            }
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
            WorkspacesRequest::Rename { workspace, name } => {
                self.page_rename(device, &workspace, &name).await?
            }
            request @ (WorkspacesRequest::Servers { .. }
            | WorkspacesRequest::AddServer { .. }
            | WorkspacesRequest::RemoveServer { .. }) => self.page_servers(device, request).await?,
            WorkspacesRequest::Delete { workspace } => self.page_delete(device, &workspace).await?,
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
                administrator,
            } => {
                let request = IssueInvitationRequest {
                    request_id,
                    role,
                    recipient_user_public_key: recipient,
                    invitee_name: None,
                    expires_at_ns,
                    inviter_name,
                };
                self.page_invitation(&workspace, device, request, administrator, wait_list_entry)
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
                    .map_err(|_| CollaborationError::new("not a node of the workspace's tree"))?;
                let access = access
                    .parse()
                    .map_err(|_| CollaborationError::new("a share gives `view` or `edit`"))?;
                let grants = self
                    .share_resource(&workspace, device, &share, &node, access)
                    .await?;
                // Who may do what changed in the workspace: its operators read
                // that here, as they read who was admitted.
                tracing::info!(%workspace, %share, %node, access = access.as_str(), grants, "a share grants the workspace");
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

    /// A page's rename of `workspace` to `name`, as its device's person: the
    /// workspace's owner or a moderator, as the shell's own menu offers it. The
    /// workspace's own governance is where the name lives, so the host writes
    /// it there (its records follow) and answers with the workspace as it is
    /// after.
    async fn page_rename(
        &self,
        device: &DeviceCert,
        workspace: &str,
        name: &str,
    ) -> Result<WorkspacesReply, CollaborationError> {
        {
            let state = self.workspace(workspace).await?;
            let mut governed = state.lock().await;
            let now = tilo_clock::unix_now_ns();
            self.authorize(
                device,
                Some(&governed),
                WorkspaceOp::Govern(governance::RENAME),
                now,
            )?;
            self.rename_governed(&mut governed, name)
                .map_err(CollaborationError::new)?;
            self.settle(&mut governed).await?;
        }
        self.page_workspace(workspace, device).await
    }

    /// Name `device`'s person `name`, asked in `workspace`: the name they go
    /// by from now on, there and in every other workspace kept here, in its
    /// member list and on the invitations they make, and the one their profile
    /// keeps for every device of theirs. Only a person names themself, and only
    /// in a workspace they are a member of: from any of their devices or keys —
    /// one the workspace admitted as theirs, or a key of theirs it holds them by
    /// their root, which is what a declared owner's key is before any device of
    /// theirs arrived. A name they already go by writes nothing.
    pub(crate) async fn name_self(
        &self,
        workspace: &str,
        device: &DeviceCert,
        name: &str,
        now: u64,
    ) -> Result<WorkspacesReply, CollaborationError> {
        let name = valid_name(name)?;
        let person = hex::encode(device.user_public_key);
        let own_key = device.capabilities.own_key_in(workspace);
        let state = self.workspace(workspace).await?;
        let mut workspace = state.lock().await;
        let at = system_time(now);
        // An agent's grant, a stand-in and a bot act as members of their own:
        // they never name the person they act for.
        let asking = workspace.peer().permissions(device.device_public_key, at);
        let theirs = !device.capabilities.stand_in
            && match asking {
                Ok(member) => {
                    !member.acting.delegated() && member.person_id() == Some(person.as_str())
                }
                Err(_) => own_key,
            };
        let names_themself = theirs
            && workspace
                .peer()
                .governance()
                .governed_members(at)
                .map_err(CollaborationError::new)?
                .members
                .iter()
                .any(|member| member.root() == Some(device.user_public_key) && !member.removed);
        if !names_themself {
            return Err(CollaborationError::new(
                "only a person names themself, in a workspace they are a member of",
            ));
        }
        let key = self.inner.host.key.clone();
        let mut candidate = workspace.peer().clone();
        if candidate
            .governance_mut()
            .name_person(device.user_public_key, &name, &key, at)
            .map_err(CollaborationError::new)?
        {
            let log = workspace.collaboration().clone();
            workspace
                .commit(candidate, &key, at, |_, _| Ok(log))
                .map_err(CollaborationError::new)?;
            self.settle(&mut workspace).await?;
        }
        drop(workspace);
        // The name is the person's, not this workspace's: kept with their profile, which every
        // device of theirs reads, and theirs in every other workspace kept here.
        self.name_person(device.user_public_key, &name, now).await?;
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
            .map_err(|_| CollaborationError::new("not a node of the workspace's tree"))?;
        let state = self.workspace(workspace).await?;
        let governed = state.lock().await;
        if node.workspace_id() != Some(governed.workspace()) {
            return Err(CollaborationError::new("the node is not in this workspace"));
        }
        let now = system_time(tilo_clock::unix_now_ns());
        let member = governed.peer().permissions(device.device_public_key, now)?;
        drop(governed);
        Ok(member.allows(&node, tilo_action_policy::ActionKind::Undoable, now))
    }

    /// A page listing the servers keeping a workspace, having another keep it
    /// as well by its site, and letting one go: each answers the servers as
    /// they are then, the one the workspace was made on first.
    async fn page_servers(
        &self,
        device: &DeviceCert,
        request: WorkspacesRequest,
    ) -> Result<WorkspacesReply, CollaborationError> {
        let workspace = match &request {
            WorkspacesRequest::Servers { workspace }
            | WorkspacesRequest::AddServer { workspace, .. }
            | WorkspacesRequest::RemoveServer { workspace, .. } => workspace.clone(),
            _ => return Err(CollaborationError::new("not a servers request")),
        };
        // Which servers keep a workspace is its owner's choice, as the
        // workspace's own `server.*` actions decide it.
        let chooses = {
            let governed = self.workspace(&workspace).await?;
            let mut governed = governed.lock().await;
            self.hand_kept_workspace_to(&mut governed, device.user_public_key)
                .await;
            Self::chooses_servers(&governed, device)
        };
        if !chooses {
            return Err(CollaborationError::new(
                "only the workspace's owner chooses the servers that keep it",
            ));
        }
        match request {
            WorkspacesRequest::AddServer { host, .. } => {
                self.add_server(&workspace, &host, tilo_peer_state::CopyRole::KeyHolder)
                    .await
                    .map_err(CollaborationError::new)?;
            }
            WorkspacesRequest::RemoveServer { server, .. } => {
                self.remove_server(&workspace, &server)
                    .await
                    .map_err(CollaborationError::new)?;
            }
            _ => {}
        }
        let servers = self
            .keeping_devices(&workspace)
            .await
            .map_err(CollaborationError::new)?;
        Ok(WorkspacesReply::Servers(
            servers
                .into_iter()
                .map(|server| ServerEntry {
                    device: server.device,
                    person: server.person,
                    label: server.label,
                    here: server.here,
                })
                .collect(),
        ))
    }

    /// A page's deletion of `workspace`, a workspace of the calling device's
    /// person with nobody else in it: final, with everything this server keeps
    /// of it. Whose a workspace is, the same check answers as for making one,
    /// and the server's own person deletes any it governs.
    async fn page_delete(
        &self,
        device: &DeviceCert,
        workspace: &str,
    ) -> Result<WorkspacesReply, CollaborationError> {
        let person = device.user_public_key;
        let theirs =
            person == self.inner.host.cert.user_public_key || self.owns(&person, workspace);
        if !theirs || !self.lists_to(device, workspace) {
            return Err(CollaborationError::new(
                tilo_server_session::workspaces::Refusal::NotYourOwn.to_reason(),
            ));
        }
        {
            let governed = self.workspace(workspace).await?;
            let governed = governed.lock().await;
            if self.people(&governed).people() > 1 {
                return Err(CollaborationError::new("other people are in the workspace"));
            }
        }
        self.delete(workspace)
            .await
            .map_err(CollaborationError::new)?;
        Ok(WorkspacesReply::Deleted)
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
        grant.verify()?;
        let identity = grant.workspace.clone();
        let address = self.invitation_addr().await;
        let issued = {
            let workspace = self.workspace(&identity).await?;
            let mut workspace = workspace.lock().await;
            self.authorize(
                device,
                Some(&workspace),
                WorkspaceOp::Invite(grant.role),
                now,
            )?;
            if grant.owner_user_public_key != workspace.peer().owner() {
                return Err(CollaborationError::new(
                    "only the workspace's owner pre-admits people",
                ));
            }
            let issued = self.stage_issue(
                &mut workspace,
                device,
                IssueInvitationRequest {
                    request_id,
                    role: grant.role,
                    recipient_user_public_key,
                    invitee_name: None,
                    expires_at_ns: grant.expires_at_ns,
                    inviter_name,
                },
                false,
                &address,
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
            WorkspaceOp::Host(ServiceOperation::CreateWorkspace),
            now,
        )?;
        let created = self.create(name).await?;
        self.page_workspace(&created.workspace, device).await
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
            WorkspaceOp::Host(ServiceOperation::CreateWorkspace),
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
        self.page_workspace(workspace, device).await
    }

    async fn page_list(
        &self,
        device: &DeviceCert,
        now: u64,
    ) -> Result<WorkspacesReply, CollaborationError> {
        self.authorize(
            device,
            None,
            WorkspaceOp::Host(ServiceOperation::ListWorkspaces),
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
            let summary = self.people(&workspace);
            let holders = holders_of(&workspace.peer().governance().keeping_devices());
            drop(workspace);
            listed.push(page_workspace(summary, holders, device.user_public_key));
        }
        Ok(WorkspacesReply::Workspaces(listed))
    }

    /// Issue the invitation `request` asks, as a page asks it.
    async fn page_invitation(
        &self,
        workspace: &str,
        device: &DeviceCert,
        request: IssueInvitationRequest,
        administrator: bool,
        wait_list_entry: Option<[u8; 32]>,
    ) -> Result<WorkspacesReply, CollaborationError> {
        let issued = self
            .issue_for(workspace, device, request, administrator, wait_list_entry)
            .await?;
        Ok(WorkspacesReply::Invitation {
            invite_id: issued.invite_id,
            signed_invitation: issued.signed_invitation,
        })
    }

    /// Issue the invitation `request` asks; for the wait-list entry
    /// `wait_list_entry`, noted on it, so it shows as invited and leaves the
    /// list once someone joins with it.
    pub(super) async fn issue_for(
        &self,
        workspace: &str,
        device: &DeviceCert,
        request: IssueInvitationRequest,
        administrator: bool,
        wait_list_entry: Option<[u8; 32]>,
    ) -> Result<IssuedInvitation, CollaborationError> {
        let Some(entry) = wait_list_entry else {
            return self
                .issue_as(workspace, device, request, administrator)
                .await;
        };
        let now = tilo_clock::unix_now_ns();
        let address = self.invitation_addr().await;
        let issued = {
            let governed = self.workspace(workspace).await?;
            let mut governed = governed.lock().await;
            self.authorize(
                device,
                Some(&governed),
                WorkspaceOp::invite(request.role, administrator),
                now,
            )?;
            if !governed.wait_list.has(entry, now) {
                return Err(CollaborationError::new(
                    "that address is no longer on the wait list",
                ));
            }
            let issued =
                self.stage_issue(&mut governed, device, request, administrator, &address, now)?;
            governed
                .wait_list
                .invited(entry, issued.invite_id, now)
                .map_err(CollaborationError::new)?;
            issued
        };
        self.written(workspace, false).await?;
        Ok(issued)
    }

    async fn page_roles(
        &self,
        workspace: &str,
        device: &DeviceCert,
        now: u64,
    ) -> Result<WorkspacesReply, CollaborationError> {
        use tilo_server_protocol::workspaces::WorkspaceRoles;
        let workspace = self.workspace(workspace).await?;
        let workspace = workspace.lock().await;
        let member = workspace
            .peer()
            .permissions(device.device_public_key, system_time(now))?;
        drop(workspace);
        Ok(WorkspacesReply::Roles(WorkspaceRoles {
            custom_roles: member.policy.custom_roles.clone(),
            assigned_roles: member.policy.assigned_roles.clone(),
        }))
    }

    async fn page_workspace(
        &self,
        workspace: &str,
        device: &DeviceCert,
    ) -> Result<WorkspacesReply, CollaborationError> {
        let workspace = self.workspace(workspace).await?;
        let workspace = workspace.lock().await;
        let summary = self.people(&workspace);
        let holders = holders_of(&workspace.peer().governance().keeping_devices());
        drop(workspace);
        Ok(WorkspacesReply::Workspace(page_workspace(
            summary,
            holders,
            device.user_public_key,
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
                    role: invitation.role,
                    expires_at_ns: invitation.expires_at_ns,
                    recipient: invitation.recipient_user_public_key,
                    invitee_name: invitation.invitee_name,
                    administrator: invitation.administrator,
                })
                .collect(),
        ))
    }

    /// The addresses left at `workspace`'s entry, oldest first, for someone the
    /// workspace lets invite.
    pub(super) async fn wait_list(
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
    pub(super) async fn unlist(
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
    /// server sends mail ([`Workspaces::mail_wait_list`]).
    pub(super) async fn mail_invite(
        &self,
        workspace: &str,
        device: &DeviceCert,
        request_id: [u8; 32],
        link: String,
        now: u64,
    ) -> Result<(), CollaborationError> {
        let Some((entry_url, mail)) = self.inner.entry.mail.get() else {
            return Err(CollaborationError::new("this entry sends no mail"));
        };
        if !tilo_server_protocol::workspaces::invitation_on_entry(entry_url, &link) {
            return Err(CollaborationError::new(
                "only an invitation on this entry is mailed",
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
        // The invitation says who asked the person in, by the name the workspace shows them by.
        let inviter = self
            .member_name_by_root(&governed, device.user_public_key)
            .unwrap_or_default();
        drop(governed);
        let (queued, accepted) = tokio::sync::oneshot::channel();
        let sent = mail
            .send(super::WaitListMail {
                queued,
                to,
                link,
                inviter,
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
                "the entry did not accept the invitation mail",
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

    /// Keep the request of someone who asked at `workspace`'s entry without an
    /// invitation, to be invited at `contact` (an address
    /// [`tilo_server_protocol::workspaces::contact_address`] accepts), while the
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
        let contact = tilo_server_protocol::workspaces::contact_address(contact)
            .map_err(CollaborationError::new)?;
        let governed = self.workspace(workspace).await?;
        let mut governed = governed.lock().await;
        let kept = governed
            .wait_list
            .keep(&contact, secrets, language, tilo_clock::unix_now_ns(), max)
            .map_err(CollaborationError::new)?;
        drop(governed);
        tracing::info!(%workspace, outcome = ?kept, "a request was left at the entry");
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
    pub async fn wait_list_state(
        &self,
        workspace: &str,
        removal: [u8; 32],
    ) -> Result<tilo_server_protocol::operator::WaitListStatus, CollaborationError> {
        let governed = self.workspace(workspace).await?;
        let mut governed = governed.lock().await;
        governed.forget_joined_wait_list();
        Ok(governed.wait_list.state(removal, tilo_clock::unix_now_ns()))
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

    /// Remove an entry for the private entry operator, returning its stable
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
        let workspace_id = tilo_peer_state::join_log::invitation_workspace(invitation, now)?;
        // Read without the workspace's lock: the card answers at once, however
        // long the workspace stays taken by what it is in the middle of.
        let reading = self
            .reading(workspace_id)
            .await
            .ok_or_else(|| CollaborationError::new("this server does not hold that workspace"))?;
        let issued = reading.invitations.now();
        let invite_id = *blake3::hash(invitation).as_bytes();
        let join = tilo_peer_state::join_log::honourable(
            issued.offer(invite_id),
            &self.revocations(),
            invitation,
            now,
        )?;
        let showing = self.showing(&reading);
        Ok(JoinReply::Invitation {
            workspace: reading.workspace.clone(),
            workspace_id,
            name: showing.name,
            role: join.grant().role,
            expires_at_ns: join.grant().expires_at_ns,
            inviter: join.inviter().map(|inviter| Inviter {
                user_public_key: inviter.user_public_key,
                // An invitation signed before the workspace knew a name for
                // who asked (a link made early, before they named themselves)
                // is answered under the name the workspace shows them now:
                // the card that opens it names a person, never "someone".
                name: if inviter.name.trim().is_empty() {
                    showing
                        .names
                        .get(&inviter.user_public_key)
                        .cloned()
                        .unwrap_or_default()
                } else {
                    inviter.name.clone()
                },
            }),
            members: showing.members,
            administrator: join.administrator(),
            owner: self.owner_invitation(join.invite_id(), &reading.workspace),
            wait_list: issued.mailed(invite_id),
        })
    }

    /// What the card that opens an invitation says of the workspace itself:
    /// its name, how many people it has, and the name it shows each of them
    /// by, as its replica holds them now.
    fn showing(&self, reading: &super::readings::Reading) -> Showing {
        reading.replica.read(|peer| {
            let governance = peer.governance();
            let name = governance.workspace_name().unwrap_or_default();
            let listed = governance
                .governed_members(tilo_clock::wall())
                .unwrap_or_default()
                .members;
            let own = self.own().is_some();
            let mine = hex::encode(self.serving_certificate().user_public_key);
            let mut showing = Showing {
                name,
                members: 0,
                names: std::collections::BTreeMap::new(),
            };
            for member in listed {
                if member.removed || (own && member.id.person_id() == Some(mine.as_str())) {
                    continue;
                }
                showing.members = showing.members.saturating_add(1);
                if let (Some(root), Some(name)) = (member.root(), carried(&member.name)) {
                    showing.names.insert(root, name);
                }
            }
            showing
        })
    }

    /// Admit the device `cert` certifies with `invitation`, under the request
    /// `request_id`, shown to the workspace's people as `name`, with the
    /// owner's pre-admission `token` where it was issued under one. The
    /// caller vouches that the device proved it holds its key on the
    /// connection that carries the redemption: the host's session entry
    /// ([`tilo_server_protocol::entering`]) checks that proof before it gets
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

    /// Admit the device `presented` names with its invitation: what a session
    /// entering with an invitation, or through an entry, is answered.
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
        let (invitation, person) = (presented.invitation.clone(), presented.cert.user_public_key);
        let cert = presented.cert.clone();
        let reply = self
            .prove(presented, &joined_name(name)?, token, now)
            .await?;
        if let JoinReply::Admitted { workspace, .. } = &reply {
            self.accepted(workspace, &invitation, person).await;
            // A key that came in with an invitation is listed as every key
            // the workspace admits is, by the name it joined as, so its
            // person finds it and can revoke it there.
            if let Some(scope) = cert.capabilities.agent {
                let governed = self.workspace(workspace).await?;
                let mut governed = governed.lock().await;
                let named = (!name.is_empty()).then_some(name);
                self.list_grant(&mut governed, &cert, scope, named, now)
                    .await?;
            }
        }
        crate::timing::event("join.prove.admitted", proving, 0);
        crate::timing::event("join.prove", proving, 0);
        Ok(reply)
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
        let redemption = presented.redeem(&workspace, self.serving_certificate(), now)?;
        let invite_id = redemption.join().invite_id();
        if workspace.redeemed(invite_id) {
            return Err(CollaborationError::new("invitation was already redeemed"));
        }
        // An invitation is the approval of whoever made it: a page that holds
        // one is let in by its issuer's own grant, as at every other entry, and
        // never waits for someone in the workspace to answer for it.
        let issuer = redemption.join().grant().issuer.clone();
        self.authorize(
            &issuer,
            Some(&workspace),
            WorkspaceOp::Invite(redemption.join().grant().role),
            now,
        )?;
        let join = super::collaboration::Redeeming {
            remote_endpoint: device,
            request_id,
            proof_digest,
            redemption: &redemption,
            // A device that names no person arrives as it is: the name the workspace already
            // knows the person by stands, and one it does not know yet stays unnamed rather than
            // taking a device's label for a person's name.
            name: (!name.trim().is_empty()).then_some(name),
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
            .role(&device, system_time(now))
            .map_err(|error| CollaborationError::new(error.to_string()))?;
        Ok(JoinReply::Admitted {
            workspace: workspace.workspace().to_owned(),
            workspace_id: workspace.workspace_id(),
            name: workspace.name(),
            role: match role {
                collab_types::role::Role::Moderator => Role::Moderator,
                collab_types::role::Role::Commenter => Role::Commenter,
                collab_types::role::Role::Viewer => Role::Viewer,
                collab_types::role::Role::Owner | collab_types::role::Role::Editor => Role::Editor,
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
            .map_err(|_| CollaborationError::new("the workspace stopped admitting"))?
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
                            name: workspace.name(),
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
    ) -> Result<(VerifiedRedemption, String, Role), Box<Result<JoinReply, CollaborationError>>>
    {
        let redemption = join
            .presented
            .redeem(workspace, self.serving_certificate(), now)
            .map_err(|error| Box::new(Err(error)))?;
        let invite_id = redemption.join().invite_id();
        if workspace.redeemed(invite_id) || !invites.insert(invite_id) {
            return Err(Box::new(Err(CollaborationError::new(
                "invitation was already redeemed",
            ))));
        }
        let role = redemption.join().grant().role;
        let grant = workspace
            .admissions
            .grant_for(invite_id, &join.token, now)
            .map_err(|error| Box::new(Err(CollaborationError::from(error))))?;
        self.pre_admission_signer(workspace, grant)
            .map_err(|error| Box::new(Err(CollaborationError::from(error))))?;
        if grant.role != role || grant.workspace != workspace.workspace() {
            return Err(Box::new(Err(CollaborationError::new(
                "the pre-admission is for another workspace or role",
            ))));
        }
        Ok((redemption, grant.label.clone(), role))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use collab_types::Person;
    use ed25519_dalek::SigningKey;
    use tilo_peer_state::Member;

    /// A workspace of one person the page has no name for: the row stays without a name, never
    /// what their device or a key of theirs is called.
    #[test]
    fn a_person_without_a_name_is_named_by_no_device_or_key_on_a_page() {
        let person = SigningKey::from_bytes(&rand::random());
        let root = person.verifying_key().to_bytes();
        let member = Member::of_person(&Person::new(hex::encode(root), ""), Some(Role::Owner));
        let summary = WorkspaceSummary {
            workspace: "team".to_owned(),
            workspace_id: WorkspaceId::from_u128(1),
            name: "Team".to_owned(),
            generation: 1,
            members: vec![member],
            devices: std::collections::BTreeMap::new(),
        };
        let page = page_workspace(summary, Vec::new(), [0; 32]);
        assert_eq!(page.members[0].name, "", "an unnamed person stays unnamed");
    }
}
