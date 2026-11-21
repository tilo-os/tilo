//! Invitations, their redemption and member administration of a governed
//! workspace on this server.
//!
//! What a governor records between the steps of an exchange is the shared
//! [`JoinLog`], the same on every host that governs workspaces; this module
//! owns who may ask what of this server's workspaces, and the durability barrier
//! before each answer.

use collab_types::workspace_grant::WorkspaceGrant;
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use tilo_peer_state::join::{JoinBootstrap, Role};
use tilo_peer_state::join_answer::{JoinWorkspaces, WorkspaceTxn};
use tilo_peer_state::join_log::{Administered, Governed, Signer};
pub use tilo_peer_state::join_log::{JoinError as CollaborationError, Redeeming};
use tilo_server_session::workspaces::{
    CancelInvitationRequest, IssueInvitationRequest, IssuedInvitation, PendingInvitation,
    RemoveMemberRequest, SetMemberRoleRequest,
};
use tilo_user_identity::{AgentScope, DeviceCert};
use tilo_workspace_agent::WorkspaceAgents;

use tilo_action_policy::roles::governance;

use super::authority::WorkspaceOp;
use super::{GovernedWorkspace, WorkspaceDocumentEntry, Workspaces, system_time};
use crate::service_authority::ServiceOperation;
use tilo_lock::MutexRecover as _;

/// The name an invitation carries for a person: what the workspace shows them
/// by, where they chose one an invitation may sign.
pub(super) fn carried(name: &str) -> Option<String> {
    let name = name.trim();
    (!name.is_empty() && name.chars().count() <= tilo_peer_state::join::MAX_INVITER_NAME_CHARS)
        .then(|| name.to_owned())
}

/// Where [`Workspaces::add_member`] puts a person.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemberPlace {
    /// A member at this role, governing nothing.
    Role(Role),
    /// An editor holding every governance action.
    Administrator,
    /// An administrator the workspace is kept for: they make and unmake its
    /// administrators and owners.
    Owner,
}

impl MemberPlace {
    /// The place `text` names: a role, `administrator` or `owner`.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "administrator" => Some(Self::Administrator),
            "owner" => Some(Self::Owner),
            role => Role::parse(role)
                .filter(|role| role.assignable())
                .map(Self::Role),
        }
    }
}

/// Hold a person in a workspace by their key ([`Workspaces::add_member`]).
#[derive(Clone, Debug)]
pub struct AddMemberRequest {
    /// Caller-chosen idempotency key.
    pub request_id: [u8; 32],
    /// The person's user root.
    pub user_public_key: [u8; 32],
    pub place: MemberPlace,
    /// How the workspace names them until they name themselves.
    pub name: Option<String>,
}

pub(super) use tilo_peer_state::join_log::JoinLog as CollaborationState;

/// A change to who is in a workspace.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(super) enum Membership {
    Remove,
    Ban,
    Unban,
    /// No changes until this Unix millisecond; reading stays allowed.
    Restrict(u64),
    Unrestrict,
}

impl Membership {
    /// This change of the person with the root key `person`, as the
    /// workspace's policy decides it.
    pub(super) fn change(self, person: [u8; 32]) -> tilo_action_policy::Change {
        use tilo_action_policy::Change;
        let person = hex::encode(person);
        match self {
            Self::Remove => Change::RemoveMember { person },
            Self::Ban => Change::Ban { person },
            Self::Unban => Change::Unban { person },
            Self::Restrict(until_ms) => Change::Restrict { person, until_ms },
            Self::Unrestrict => Change::Unrestrict { person },
        }
    }
}

/// A committed governance change.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GovernanceCompletion {
    /// The workspace generation that carries it.
    pub generation: u64,
}

/// One waiting for its workspace's next write: whether it staged a change of
/// the workspace's pre-admissions as well as of its record, and where the
/// outcome goes.
pub(super) struct Written {
    admissions: bool,
    answer: tokio::sync::oneshot::Sender<Result<(), CollaborationError>>,
}

impl GovernedWorkspace {
    /// The workspace as its join log reads it: committed now, answered by
    /// `answering`, the device that answers its joins here.
    pub(super) fn governed<'a>(&'a self, answering: &'a DeviceCert) -> Governed<'a> {
        governed(&self.record, self.peer().into(), answering)
    }

    /// The workspace as its join log reads it, from the state the caller
    /// holds: what a decision reads is what its commit is measured against.
    pub(super) fn governed_from<'a>(
        &'a self,
        peer: &'a tilo_peer_state::StatePeer,
        answering: &'a DeviceCert,
    ) -> Governed<'a> {
        governed(&self.record, peer.into(), answering)
    }

    /// Keep the people `removed` out of the workspace's public entry for the
    /// day: moderation took their membership away. `by` is the person who
    /// asked, when a person asks: someone who leaves is not kept out of a
    /// workspace they may join again.
    pub(super) fn keep_out_removed(
        &mut self,
        removed: &std::collections::BTreeSet<[u8; 32]>,
        by: Option<[u8; 32]>,
        now_ns: u64,
    ) -> Result<(), String> {
        for person in removed {
            if Some(*person) != by {
                self.removals.remove(*person, now_ns)?;
            }
        }
        Ok(())
    }
}

/// `record` and `peer`, a workspace's committed state, as its join log reads
/// it, answered by `answering`: the device that answers this workspace's
/// joins here, which need not be the device that governs it.
fn governed<'a>(
    record: &'a WorkspaceDocumentEntry,
    peer: tilo_peer_state::join_log::PeerRef<'a>,
    answering: &'a DeviceCert,
) -> Governed<'a> {
    Governed {
        peer,
        host: answering,
        workspace_id: record.workspace_id,
        generation: record.generation,
        // A newcomer admits the workspace's governance alone: one message
        // holding its complete committed document. What the record retains of
        // its own publications builds on that document, so it reaches the
        // newcomer through the workspace's sync, not through this answer.
        governance_messages: None,
    }
}

fn random<const N: usize>() -> Result<[u8; N], CollaborationError> {
    let mut bytes = [0; N];
    getrandom::fill(&mut bytes).map_err(|error| CollaborationError::new(error.to_string()))?;
    Ok(bytes)
}

impl Workspaces {
    /// The heads of `workspace`'s governance as this server holds it now: what
    /// a change refused against an access read from an earlier state waits
    /// for ([`crate::workspace::access_deciding`]).
    pub(crate) async fn governance_heads(&self, workspace: &str) -> Option<Vec<[u8; 32]>> {
        let governed = self.workspace(workspace).await.ok()?;
        let heads = governed.lock().await.peer().governance().heads();
        Some(heads)
    }

    /// Issue an invitation to `workspace` for `administrator`: a device of this
    /// server's own user, or of a member the workspace lets invite with the
    /// invitation's role.
    pub async fn issue(
        &self,
        workspace: &str,
        administrator: &DeviceCert,
        request: IssueInvitationRequest,
    ) -> Result<IssuedInvitation, CollaborationError> {
        self.issue_at(workspace, administrator, request, false, None)
            .await
    }

    /// [`Self::issue`] of an invitation that, when `makes_administrator`,
    /// also makes whoever redeems it an administrator: an Editor's
    /// invitation, for someone who may make administrators
    /// ([`tilo_action_policy::WorkspacePolicy::check_administrator_invite`]).
    /// It names them as whom it is from, and every member admitting its
    /// holder checks again that they may.
    pub async fn issue_as(
        &self,
        workspace: &str,
        administrator: &DeviceCert,
        request: IssueInvitationRequest,
        makes_administrator: bool,
    ) -> Result<IssuedInvitation, CollaborationError> {
        self.issue_at(workspace, administrator, request, makes_administrator, None)
            .await
    }

    /// [`Self::issue_as`] of an invitation issued for the wait-list entry
    /// `wait_list_entry`, where one is named: the entry records the
    /// invitation, so what the entry mails to it carries this link, and it
    /// leaves the list by itself once someone joins with it.
    pub async fn issue_at(
        &self,
        workspace: &str,
        administrator: &DeviceCert,
        request: IssueInvitationRequest,
        makes_administrator: bool,
        wait_list_entry: Option<[u8; 32]>,
    ) -> Result<IssuedInvitation, CollaborationError> {
        self.issue_from(
            workspace,
            administrator,
            request,
            makes_administrator,
            wait_list_entry,
            self.invitation_addr().await,
        )
        .await
    }

    pub(super) async fn issue_from(
        &self,
        workspace: &str,
        administrator: &DeviceCert,
        mut request: IssueInvitationRequest,
        makes_administrator: bool,
        wait_list_entry: Option<[u8; 32]>,
        address: iroh_base::EndpointAddr,
    ) -> Result<IssuedInvitation, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        let issued = {
            let governed = self.workspace(workspace).await?;
            let mut governed = governed.lock().await;
            if makes_administrator {
                self.ensure_keeper(&mut governed, administrator.user_public_key, now)
                    .await?;
            }
            self.authorize(
                administrator,
                Some(&governed),
                WorkspaceOp::invite(request.role, makes_administrator),
                now,
            )?;
            // An invitation says who it is from. A caller that names nobody
            // (an API key, the command line) invites under the name the
            // workspace already shows for them, so the person it reaches
            // sees who asked them in, not "someone".
            if request.inviter_name.is_none() {
                request.inviter_name = self.member_name(&governed, administrator);
            }
            if let Some(entry) = wait_list_entry
                && !governed.wait_list.has(entry, now)
            {
                return Err(CollaborationError::new(
                    "that address is no longer on the wait list",
                ));
            }
            let issued = self.stage_issue(
                &mut governed,
                administrator,
                request,
                makes_administrator,
                &address,
                now,
            )?;
            if let Some(entry) = wait_list_entry {
                governed
                    .wait_list
                    .invited(entry, issued.invite_id, now)
                    .map_err(CollaborationError::new)?;
            }
            issued
        };
        self.written(workspace, false).await?;
        Ok(issued)
    }

    /// The name `governed` shows for the person `device` belongs to, where
    /// they chose one an invitation can carry.
    fn member_name(&self, governed: &GovernedWorkspace, device: &DeviceCert) -> Option<String> {
        self.member_name_by_root(governed, device.user_public_key)
    }

    /// The name `governed` shows for the person whose root is `root`, where
    /// they chose one an invitation can carry.
    pub(super) fn member_name_by_root(
        &self,
        governed: &GovernedWorkspace,
        root: [u8; 32],
    ) -> Option<String> {
        self.people(governed)
            .members
            .into_iter()
            .find(|member| member.root() == Some(root) && !member.removed)
            .and_then(|member| carried(&member.name))
    }

    /// Record an invitation `request` asks of `workspace`, unwritten, naming this device at
    /// `address` ([`Self::invitation_addr`]).
    pub(super) fn stage_issue(
        &self,
        workspace: &mut GovernedWorkspace,
        administrator: &DeviceCert,
        request: IssueInvitationRequest,
        makes_administrator: bool,
        address: &iroh_base::EndpointAddr,
        now: u64,
    ) -> Result<IssuedInvitation, CollaborationError> {
        let role = request.role;
        let transport = vec![self.invitation_hint(workspace, address)?];
        let mut log = workspace.collaboration().clone();
        let issued = log.issue_as(
            &workspace.governed(self.serving_certificate()),
            administrator,
            request,
            makes_administrator,
            transport,
            Signer {
                key: &self.inner.host.key,
                nonce: random()?,
            },
            now,
        )?;
        if log != *workspace.collaboration() {
            workspace.record.collaboration = log;
            tracing::info!(
                workspace = %workspace.workspace(),
                invite_id = %hex::encode(issued.invite_id),
                ?role,
                makes_administrator,
                outcome = "issued",
                "workspace invitation issued"
            );
        }
        Ok(issued)
    }

    /// Cancel an unredeemed invitation of `workspace`.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the workspace stays locked until its decision is committed, published and answered"
    )]
    pub async fn cancel(
        &self,
        workspace: &str,
        administrator: &DeviceCert,
        request: CancelInvitationRequest,
    ) -> Result<(), CollaborationError> {
        let workspace = self.workspace(workspace).await?;
        let mut workspace = workspace.lock().await;
        self.authorize(
            administrator,
            Some(&workspace),
            WorkspaceOp::Govern(governance::INVITE),
            tilo_clock::unix_now_ns(),
        )?;
        let mut state = workspace.collaboration().clone();
        if state.cancel(&request)? {
            workspace.commit_state(state)?;
        }
        Ok(())
    }

    /// The invitations of `workspace` still redeemable.
    pub async fn pending(
        &self,
        workspace: &str,
        administrator: &DeviceCert,
    ) -> Result<Vec<PendingInvitation>, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        let workspace = self.workspace(workspace).await?;
        let workspace = workspace.lock().await;
        self.authorize(
            administrator,
            Some(&workspace),
            WorkspaceOp::Govern(governance::INVITE),
            now,
        )?;
        Ok(workspace.collaboration().pending(now))
    }

    /// Admit `device`, a device of this server's own user, to `workspace` as its
    /// owner. Returns whether governance changed.
    pub async fn enter(
        &self,
        workspace: &str,
        device: &DeviceCert,
    ) -> Result<bool, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        self.authorize(
            device,
            None,
            WorkspaceOp::Host(ServiceOperation::EnterWorkspace),
            now,
        )?;
        self.admit_entering(workspace, device, now).await
    }

    /// Admit `device`, whose entry was decided, to `workspace`; a key keeps
    /// the grant it entered under. Returns whether governance changed.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the workspace stays locked until its decision is committed, published and answered"
    )]
    pub(crate) async fn admit_entering(
        &self,
        workspace: &str,
        device: &DeviceCert,
        now: u64,
    ) -> Result<bool, CollaborationError> {
        let workspace = self.workspace(workspace).await?;
        let mut workspace = workspace.lock().await;
        // A key whose grant this workspace records as revoked never enters it
        // again, whichever API server it comes through.
        refuse_revoked(&workspace, device)?;
        let state = workspace.collaboration().clone();
        let admitted = workspace.change_held(
            &self.inner.host.key,
            system_time(now),
            |_, candidate| {
                let admitted = candidate.admit_certificate(
                    device.clone(),
                    &self.inner.host.key,
                    system_time(now),
                )?;
                Ok((admitted, admitted))
            },
            |_, _| Ok(state),
        )?;
        if admitted {
            self.settle(&mut workspace).await?;
        } else if !self
            .inner
            .host
            .access
            .projects_access(workspace.workspace_id(), device.device_public_key)
            .await
        {
            // Admitted already, but an earlier entry can have been cut off
            // between committing the admission and settling it (its page
            // closed meanwhile): the server would then decide the device's
            // changes against device_access that leave it out. Settle what
            // governance holds before answering.
            self.settle(&mut workspace).await?;
        }
        // The workspace keeps the grant of every key it admitted, so every
        // holder and every API server reads the same grant from the workspace
        // itself, whichever way the key came in.
        if let Some(scope) = device.capabilities.agent {
            self.list_grant(&mut workspace, device, scope, None, now)
                .await?;
        }
        Ok(admitted)
    }

    /// Refuse `device` when `workspace` keeps it as a revoked key, saying so:
    /// it is invited into it no more, and its holder tells its person to
    /// approve another.
    ///
    /// A workspace not held here refuses nothing.
    ///
    /// # Errors
    /// The key is revoked there.
    pub(super) async fn refuse_revoked_key(
        &self,
        workspace: &str,
        device: &DeviceCert,
    ) -> Result<(), CollaborationError> {
        let held = self.inner.workspaces.lock().await.get(workspace).cloned();
        match held {
            Some(held) => refuse_revoked(&*held.lock().await, device),
            None => Ok(()),
        }
    }

    /// Keep the grant `cert` was admitted under in `workspace`'s governance,
    /// reaching `scope`, authored by this host: the workspace's holders and
    /// the API servers that read them see it from then on. It is named `name`,
    /// what the key joined as (the name its person approved it under), where
    /// its join says one; else as it is listed already, else as its
    /// certificate is labelled.
    pub(super) async fn list_grant(
        &self,
        workspace: &mut GovernedWorkspace,
        cert: &DeviceCert,
        scope: AgentScope,
        name: Option<&str>,
        now: u64,
    ) -> Result<(), CollaborationError> {
        let at = system_time(now);
        let grant = WorkspaceGrant {
            id: WorkspaceGrant::id_of(&cert.device_public_key),
            person: hex::encode(cert.user_public_key),
            device: hex::encode(cert.device_public_key),
            name: cert.device_label.clone(),
            scope: match scope {
                AgentScope::AllWorkspaces => None,
                one @ AgentScope::Workspace(_) => Some(one.name()),
            },
            expires_ms: cert.expires_at_ns.map(|expiry| expiry / 1_000_000),
            // A key that another grant minted names it, so the key that made
            // it lists it wherever the workspace is held.
            parent: cert
                .issuer
                .as_deref()
                .map(|issuer| WorkspaceGrant::id_of(&issuer.device_public_key)),
            created_ms: cert.issued_at_ns / 1_000_000,
            revoked_ms: None,
        };
        // Recording the grant of a key this workspace revoked does not bring
        // it back: its person's revocation stands. A name it is listed under
        // stays, unless its join names it now.
        let held = workspace
            .peer()
            .governance()
            .workspace_grants()
            .into_iter()
            .find(|held| held.id == grant.id);
        let named = name.map(str::trim).filter(|name| !name.is_empty());
        let grant = WorkspaceGrant {
            revoked_ms: held.as_ref().and_then(|held| held.revoked_ms),
            name: match (named, held) {
                (Some(named), _) => named.to_owned(),
                (None, Some(held)) => held.name,
                (None, None) => grant.name.clone(),
            },
            ..grant
        };
        let mut candidate = workspace.peer().clone();
        if candidate
            .governance_mut()
            .author_workspace_grant(&grant, &self.inner.host.key, at)?
        {
            let state = workspace.collaboration().clone();
            workspace.commit(candidate, &self.inner.host.key, at, |_, _| Ok(state))?;
            self.settle(workspace).await?;
        }
        Ok(())
    }

    /// Every grant `workspace`'s governance holds, as the policy its holders
    /// decide by keeps them.
    ///
    /// # Errors
    ///
    /// The workspace's policy is not here.
    pub async fn workspace_grants(
        &self,
        workspace: &str,
    ) -> Result<Vec<tilo_action_policy::Grant>, CollaborationError> {
        self.policy_of(workspace)
            .await
            .map(|policy| policy.grants)
            .ok_or_else(|| CollaborationError::new("the workspace's policy is not here"))
    }

    /// The grants `workspace` records of `person`'s, as its governance keeps
    /// them: each key it admitted, when it admitted it, and whether its person
    /// revoked it.
    pub async fn grants_of(
        &self,
        workspace: &str,
        person: &[u8; 32],
    ) -> Result<Vec<WorkspaceGrant>, CollaborationError> {
        let workspace = self.workspace(workspace).await?;
        let workspace = workspace.lock().await;
        let person = hex::encode(person);
        Ok(workspace
            .peer()
            .governance()
            .workspace_grants()
            .into_iter()
            .filter(|grant| grant.person == person)
            .collect())
    }

    /// Mark the grant `id` of `workspace` revoked at `now_ms`, authored by
    /// this host: every holder and API server that reads the workspace refuses
    /// the key from then on. Answers whether it changed the workspace.
    pub async fn revoke_grant(
        &self,
        workspace: &str,
        id: &str,
        now_ms: u64,
    ) -> Result<bool, CollaborationError> {
        let workspace = self.workspace(workspace).await?;
        let mut workspace = workspace.lock().await;
        let at = system_time(now_ms.saturating_mul(1_000_000));
        let mut candidate = workspace.peer().clone();
        if !candidate.governance_mut().author_workspace_grant_revoked(
            id,
            now_ms,
            &self.inner.host.key,
            at,
        )? {
            return Ok(false);
        }
        let state = workspace.collaboration().clone();
        workspace.commit(candidate, &self.inner.host.key, at, |_, _| Ok(state))?;
        self.settle(&mut workspace).await?;
        drop(workspace);
        Ok(true)
    }

    pub async fn set_member_role(
        &self,
        workspace: &str,
        administrator: &DeviceCert,
        request: SetMemberRoleRequest,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        let workspace = self.workspace(workspace).await?;
        let mut workspace = workspace.lock().await;
        self.ensure_keeper(&mut workspace, administrator.user_public_key, now)
            .await?;
        self.authorize_role_change(&workspace, administrator, &request, now)?;
        let key = self.inner.host.key.clone();
        self.administer(
            &mut workspace,
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

    pub(super) async fn edit_roles(
        &self,
        workspace: &str,
        caller: &DeviceCert,
        request_id: [u8; 32],
        change: tilo_action_policy::Change,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        if !change.is_custom_role_change() {
            return Err(CollaborationError::new("expected a custom role change"));
        }
        let now = tilo_clock::unix_now_ns();
        let workspace = self.workspace(workspace).await?;
        let mut workspace = workspace.lock().await;
        self.ensure_keeper(&mut workspace, caller.user_public_key, now)
            .await?;
        self.authorize(caller, Some(&workspace), WorkspaceOp::Change(&change), now)?;
        let key = self.inner.host.key.clone();
        self.administer(
            &mut workspace,
            request_id,
            request_digest(&("custom-role", &change))?,
            |peer| {
                peer.governance_mut()
                    .change_custom_role(change, &key, system_time(now))
            },
            now,
        )
        .await
    }

    pub(super) async fn set_document_access(
        &self,
        workspace: &str,
        caller: &DeviceCert,
        request_id: [u8; 32],
        (recipient, app, document): (&str, &str, &str),
        access: &str,
        now: u64,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        let workspace = self.workspace(workspace).await?;
        let mut workspace = workspace.lock().await;
        self.ensure_keeper(&mut workspace, caller.user_public_key, now)
            .await?;
        let at = system_time(now);
        let document = if app.is_empty() {
            document
        } else {
            let document =
                collab_types::document_access_target(app, document).ok_or_else(|| {
                    CollaborationError::new("this document has no workspace access rules")
                })?;
            workspace
                .peer()
                .check_document_read(caller.device_public_key, app, document, at)?;
            if workspace.peer().own_resource(app, document).is_some() {
                return Err(CollaborationError::new("this document belongs to a person"));
            }
            document
        };
        let policy = workspace.peer().governance().governed_policy()?;
        let actor = hex::encode(caller.user_public_key);
        let changes = collab_types::document_access_changes(
            &policy,
            &actor,
            (recipient, app, document),
            access,
        )
        .map_err(|refusal| CollaborationError::new(refusal.to_string()))?;
        for change in &changes {
            self.authorize(caller, Some(&workspace), WorkspaceOp::Change(change), now)?;
        }
        let key = self.inner.host.key.clone();
        let completion = self
            .administer(
                &mut workspace,
                request_id,
                request_digest(&("document-access", recipient, app, document, access))?,
                |peer| {
                    peer.governance_mut()
                        .change_document_access(changes, &key, at)
                },
                now,
            )
            .await;
        drop(workspace);
        completion
    }

    /// Hold the person `request.user_public_key` in `workspace` at
    /// `request.place`, whether or not a device of theirs has joined: what an
    /// administrator does when they know the person's key rather than handing
    /// them an invitation. A member already is moved to the place; their
    /// entry lets them in from then on, and a key of theirs reaches the
    /// workspace before any device of theirs has joined.
    ///
    /// # Errors
    ///
    /// Why `administrator`'s person may not give that place, a name that is not
    /// a display name, the workspace's own owner, or governance that cannot be
    /// updated.
    pub async fn add_member(
        &self,
        workspace: &str,
        administrator: &DeviceCert,
        request: AddMemberRequest,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        let workspace = self.workspace(workspace).await?;
        let mut workspace = workspace.lock().await;
        self.ensure_keeper(&mut workspace, administrator.user_public_key, now)
            .await?;
        let person = request.user_public_key;
        let (role, makes_administrator, makes_owner) = match request.place {
            MemberPlace::Role(role) => (role, false, false),
            MemberPlace::Administrator => (Role::Editor, true, false),
            MemberPlace::Owner => (Role::Editor, true, true),
        };
        let op = WorkspaceOp::Place(person, request.place);
        self.authorize(administrator, Some(&workspace), op, now)?;
        let key = self.inner.host.key.clone();
        let name = request.name.as_deref();
        let done = self
            .administer(
                &mut workspace,
                request.request_id,
                request_digest(&("member", person, &request.place, name))?,
                |peer| {
                    let placed = peer.governance_mut().declare_member(
                        person,
                        role,
                        makes_administrator,
                        name,
                        &key,
                        system_time(now),
                    )?;
                    let kept = makes_owner
                        && peer
                            .governance_mut()
                            .set_keeper(person, &key, system_time(now))?;
                    Ok(placed || kept)
                },
                now,
            )
            .await;
        drop(workspace);
        done
    }

    /// Write workspace grants the governor decided (workspace settings such as a
    /// role's Deny on a kind), keyed by their ids.
    ///
    /// # Errors
    ///
    /// Refuses non-administrators, a workspace this server's person does not
    /// govern, and grants for another workspace.
    pub async fn put_workspace_grants(
        &self,
        workspace: &str,
        administrator: &DeviceCert,
        request_id: [u8; 32],
        grants: Vec<tilo_action_policy::Grant>,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        self.authorize(
            administrator,
            None,
            WorkspaceOp::Host(ServiceOperation::PutWorkspaceGrants),
            now,
        )?;
        if !self.governs(workspace) {
            return Err(CollaborationError::new(
                "this server's person does not govern the workspace",
            ));
        }
        let workspace = self.workspace(workspace).await?;
        let mut workspace = workspace.lock().await;
        let key = self.inner.host.key.clone();
        self.administer(
            &mut workspace,
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

    /// Take back the workspace grants `ids`, as the workspace's host decided; a grant
    /// the workspace does not hold is no change.
    ///
    /// # Errors
    ///
    /// Why the grants were not taken back; a workspace this server's person
    /// does not govern is refused.
    pub async fn remove_workspace_grants(
        &self,
        workspace: &str,
        administrator: &DeviceCert,
        request_id: [u8; 32],
        ids: Vec<tilo_action_policy::GrantId>,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        self.authorize(
            administrator,
            None,
            WorkspaceOp::Host(ServiceOperation::PutWorkspaceGrants),
            now,
        )?;
        if !self.governs(workspace) {
            return Err(CollaborationError::new(
                "this server's person does not govern the workspace",
            ));
        }
        let workspace = self.workspace(workspace).await?;
        let mut workspace = workspace.lock().await;
        let key = self.inner.host.key.clone();
        self.administer(
            &mut workspace,
            request_id,
            request_digest(&("workspace.grants.remove", &ids))?,
            |peer| {
                peer.governance_mut()
                    .remove_governed_grants(&ids, &key, system_time(now))
            },
            now,
        )
        .await
    }

    /// Make `user_public_key` an administrator of `workspace`: an editor holding
    /// every governance action, still bounded by their role
    /// ([`tilo_action_policy::roles::administrator_grant`]). `name`, when the
    /// declaring request carries one, is how the workspace names them from
    /// then on: its member list and the invitations they make say it before
    /// any device of theirs has named them.
    ///
    /// # Errors
    ///
    /// Refuses non-administrators, the structural owner, a name that is not a
    /// display name, and governance that cannot be updated.
    pub async fn set_member_administrator(
        &self,
        workspace: &str,
        administrator: &DeviceCert,
        request_id: [u8; 32],
        user_public_key: [u8; 32],
        name: Option<&str>,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        let workspace = self.workspace(workspace).await?;
        let mut workspace = workspace.lock().await;
        self.ensure_keeper(&mut workspace, administrator.user_public_key, now)
            .await?;
        self.authorize(
            administrator,
            Some(&workspace),
            WorkspaceOp::MakeAdministrator,
            now,
        )?;
        let key = self.inner.host.key.clone();
        self.administer(
            &mut workspace,
            request_id,
            request_digest(&("administrator", user_public_key, name))?,
            |peer| {
                peer.governance_mut().set_member_administrator(
                    user_public_key,
                    name,
                    &key,
                    system_time(now),
                )
            },
            now,
        )
        .await
    }

    /// Name `user_public_key`, a member, as whom `workspace` is kept for
    /// ([`tilo_action_policy::roles::keeper_grant`]): they make and unmake its
    /// administrators as its owner, this host, does. The host's own decision:
    /// only its person's devices ask it.
    ///
    /// # Errors
    ///
    /// Refuses anyone but the host's person, a person who is no member, and
    /// governance that cannot be updated.
    pub async fn set_member_keeper(
        &self,
        workspace: &str,
        administrator: &DeviceCert,
        request_id: [u8; 32],
        user_public_key: [u8; 32],
    ) -> Result<GovernanceCompletion, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        self.authorize(
            administrator,
            None,
            WorkspaceOp::Host(ServiceOperation::SetAdministrator),
            now,
        )?;
        let workspace = self.workspace(workspace).await?;
        let mut workspace = workspace.lock().await;
        let key = self.inner.host.key.clone();
        self.administer(
            &mut workspace,
            request_id,
            request_digest(&("keeper", user_public_key))?,
            |peer| {
                peer.governance_mut()
                    .set_keeper(user_public_key, &key, system_time(now))
            },
            now,
        )
        .await
    }

    /// Remove the member `request.user_public_key` and every device of theirs
    /// from `workspace`. The server stops admitting them before this returns.
    pub async fn remove_member(
        &self,
        workspace: &str,
        administrator: &DeviceCert,
        request: RemoveMemberRequest,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        self.change_membership(workspace, administrator, request, Membership::Remove)
            .await
    }

    /// Remove the members `people` from `workspace` in one governance change and
    /// forget how they joined. A public workspace prunes guests who left this
    /// way: every join costs time in proportion to the workspace's members, so a
    /// workspace that kept every visitor would slow with each one, and one change
    /// per guest would hold the workspace as long again. A pruned guest joins
    /// again through a new invitation.
    pub async fn prune_members(
        &self,
        workspace: &str,
        administrator: &DeviceCert,
        request_id: [u8; 32],
        people: &std::collections::BTreeSet<[u8; 32]>,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        let workspace = self.workspace(workspace).await?;
        let mut workspace = workspace.lock().await;
        for person in people {
            let change = tilo_action_policy::Change::RemoveMember {
                person: hex::encode(person),
            };
            self.authorize(
                administrator,
                Some(&workspace),
                WorkspaceOp::Change(&change),
                now,
            )?;
        }
        let key = self.inner.host.key.clone();
        let completion = self
            .administer(
                &mut workspace,
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
        let mut state = workspace.collaboration().clone();
        state.forget(people);
        workspace.commit_state(state)?;
        for person in people {
            workspace
                .removals
                .clear(*person)
                .map_err(CollaborationError::new)?;
        }
        tracing::info!(
            workspace = %workspace.workspace(),
            people = people.len(),
            "idle members pruned"
        );
        let identity = workspace.workspace().to_owned();
        drop(workspace);
        self.retire_kept_agents(&identity, people).await;
        Ok(completion)
    }

    /// Remove `request.user_public_key` from `workspace`, when a member, and keep
    /// them out until [`Self::unban_member`]: a removal plus their
    /// `block:<person>` Deny.
    pub async fn ban_member(
        &self,
        workspace: &str,
        administrator: &DeviceCert,
        request: RemoveMemberRequest,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        self.change_membership(workspace, administrator, request, Membership::Ban)
            .await
    }

    /// Let the banned `request.user_public_key` be admitted to `workspace` again.
    pub async fn unban_member(
        &self,
        workspace: &str,
        administrator: &DeviceCert,
        request: RemoveMemberRequest,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        self.change_membership(workspace, administrator, request, Membership::Unban)
            .await
    }

    /// Stop `request.user_public_key` changing anything in `workspace` until
    /// `until_ms` (Unix milliseconds): the workspace's expiring
    /// `restrict:<person>` Deny, which lapses by itself and leaves reading
    /// alone. A newer restriction replaces an older one.
    pub async fn restrict_member(
        &self,
        workspace: &str,
        administrator: &DeviceCert,
        request: RemoveMemberRequest,
        until_ms: u64,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        self.change_membership(
            workspace,
            administrator,
            request,
            Membership::Restrict(until_ms),
        )
        .await
    }

    /// Lift `request.user_public_key`'s restriction in `workspace` before it
    /// lapses.
    pub async fn unrestrict_member(
        &self,
        workspace: &str,
        administrator: &DeviceCert,
        request: RemoveMemberRequest,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        self.change_membership(workspace, administrator, request, Membership::Unrestrict)
            .await
    }

    /// Remove, ban or unban, decided by who asks: a device of the server's
    /// user governs its workspaces, and any other member by the workspace's policy (a
    /// Moderator reaches members ranked below Moderator).
    async fn change_membership(
        &self,
        workspace: &str,
        administrator: &DeviceCert,
        request: RemoveMemberRequest,
        membership: Membership,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        let workspace = self.workspace(workspace).await?;
        let mut workspace = workspace.lock().await;
        self.ensure_keeper(&mut workspace, administrator.user_public_key, now)
            .await?;
        let person = hex::encode(request.user_public_key);
        let change = membership.change(request.user_public_key);
        let identity = workspace.workspace().to_owned();
        let by = self.authorize(
            administrator,
            Some(&workspace),
            WorkspaceOp::Change(&change),
            now,
        )?;
        let key = self.inner.host.key.clone();
        let target = request.user_public_key;
        let completion = self
            .administer(
                &mut workspace,
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
                            &identity,
                            &person,
                            until_ms,
                            tilo_action_policy::Grantee::person(by.clone()),
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
            // A removal takes back every way in the workspace handed them, not
            // only their membership: an invitation issued to them before, still
            // unredeemed, would otherwise admit them again at once.
            let mut state = workspace.collaboration().clone();
            state.withdraw_invitations(&std::collections::BTreeSet::from([target]));
            workspace.commit_state(state)?;
            workspace
                .keep_out_removed(
                    &std::collections::BTreeSet::from([target]),
                    Some(administrator.user_public_key),
                    now,
                )
                .map_err(CollaborationError::new)?;
        }
        if matches!(membership, Membership::Unban) {
            workspace
                .removals
                .clear(target)
                .map_err(CollaborationError::new)?;
        }
        tracing::info!(
            workspace = %workspace.workspace(),
            %person,
            outcome = ?membership,
            "workspace membership change committed"
        );
        drop(workspace);
        if matches!(membership, Membership::Remove | Membership::Ban) {
            self.retire_kept_agents(&identity, &std::collections::BTreeSet::from([target]))
                .await;
        }
        Ok(completion)
    }

    /// End the agents `people` kept in `workspace`: an agent acts for its keeper,
    /// so one whose keeper stops being a member does not stay live. Assistant's list of
    /// the workspace's agents says what `retire-agent` makes it say, written by the
    /// workspace's governance as it removes them rather than by a keeper who may
    /// act here no more.
    ///
    /// A list this device cannot read or end leaves the removal done: the
    /// keeper's devices are no longer admitted, so their agent gets no turn.
    pub(super) async fn retire_kept_agents(
        &self,
        workspace: &str,
        people: &std::collections::BTreeSet<[u8; 32]>,
    ) {
        if people.is_empty() {
            return;
        }
        let list = collab_types::workspace_document_id(tilo_workspace_agent::KIND);
        let app = tilo_workspace_agent::OWNER;
        let keepers: Vec<String> = people.iter().map(hex::encode).collect();
        let retired = self
            .author_workspace_document(workspace, self.governor(), (app, &list), move |doc| {
                let mut ended: usize = 0;
                for keeper in &keepers {
                    ended = ended.saturating_add(
                        WorkspaceAgents::retire_kept_in(doc, keeper)
                            .map_err(|error| error.to_string())?,
                    );
                }
                Ok(ended)
            })
            .await;
        match retired {
            Ok(0) => {}
            Ok(ended) => tracing::info!(
                %workspace,
                ended,
                "a removed keeper's agents were ended"
            ),
            Err(error) => {
                tracing::error!(%workspace, %error, "a removed keeper's agents were not ended");
            }
        }
    }

    /// Commit `command`, a membership change `device` makes itself, in
    /// `workspace` on its behalf, as the workspace's governor: for the device whose
    /// own host this is. That device and this host sign with one key, so
    /// the workspace's history has one writer under it, this host, and the
    /// device's own replica only ever receives what is written here. The
    /// workspace's policy decides the change against `device`'s grants, as it
    /// decides any member's.
    ///
    /// Answers the workspace's generation that carries the change, and the
    /// changes it added to the workspace's document, for the device's replica
    /// to take in at once (none when the workspace carried it already).
    ///
    /// # Errors
    /// Refuses a device other than this host's own, a change the workspace's
    /// rules do not let `device` make, and a change that could not commit.
    pub async fn commit_own_membership(
        &self,
        workspace: &str,
        device: &DeviceCert,
        command: tilo_peer_state::workspace_membership::WorkspaceMembershipCommand,
    ) -> Result<(GovernanceCompletion, Vec<Vec<u8>>), CollaborationError> {
        if device.device_public_key != self.inner.host.cert.device_public_key {
            return Err(CollaborationError::new(
                "a membership change is committed here only for this host's own device",
            ));
        }
        command.validate()?;
        let now = tilo_clock::unix_now_ns();
        let workspace = self.workspace(workspace).await?;
        let mut workspace = workspace.lock().await;
        let key = self.inner.host.key.clone();
        let mut changed = workspace.peer().clone();
        if !changed.workspace_mut().apply_workspace_membership_command(
            device,
            &command,
            &key,
            system_time(now),
        )? {
            return Ok((
                GovernanceCompletion {
                    generation: workspace.record.generation,
                },
                Vec::new(),
            ));
        }
        let before = workspace.peer().workspace_heads().unwrap_or_default();
        // A member removed takes every way in the workspace handed them with
        // them, as when a member's own device sends the removal.
        let removed: std::collections::BTreeSet<_> =
            super::member_roots(&workspace.peer(), system_time(now))
                .difference(&super::member_roots(&changed, system_time(now)))
                .copied()
                .collect();
        let generation = workspace.commit(changed, &key, system_time(now), |record, _| {
            let mut state = record.collaboration.clone();
            state.withdraw_invitations(&removed);
            Ok(state)
        })?;
        workspace
            .keep_out_removed(
                &removed,
                Some(device.user_public_key),
                tilo_clock::unix_now_ns(),
            )
            .map_err(CollaborationError::new)?;
        self.settle(&mut workspace).await?;
        let added = workspace.peer().workspace_changes_since(&before);
        let identity = workspace.workspace().to_owned();
        drop(workspace);
        self.retire_kept_agents(&identity, &removed).await;
        Ok((GovernanceCompletion { generation }, added))
    }

    /// Write `change` into `workspace`'s own state as `device`, this host's
    /// own device, and publish it to the workspace's members. A lease or a
    /// bot's schedule progress belongs there: it is the workspace's state
    /// whichever key holder holds the workspace, so a group of desktops with
    /// no server keeps it as a hosted workspace does. The receiver decides the
    /// change against `device`'s grants as any member's write; a governor's
    /// device may make any undoable change.
    ///
    /// Answers what `change` answered. A change that wrote nothing is not
    /// published.
    ///
    /// # Errors
    /// Refuses a device other than this host's own, a change the workspace's
    /// rules do not let `device` make, and a change that could not commit.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the workspace stays locked until its change is committed and published"
    )]
    pub async fn author_own_workspace_state<R>(
        &self,
        workspace: &str,
        device: &DeviceCert,
        change: impl FnOnce(&mut tilo_peer_state::StatePeer) -> Result<R, String>,
    ) -> Result<R, CollaborationError> {
        if device.device_public_key != self.inner.host.cert.device_public_key {
            return Err(CollaborationError::new(
                "workspace state is written here only by this host's own device",
            ));
        }
        let now = system_time(tilo_clock::unix_now_ns());
        let workspace = self.workspace(workspace).await?;
        let mut workspace = workspace.lock().await;
        let key = self.inner.host.key.clone();
        let (answer, changed) = workspace.change_held(
            &key,
            now,
            |_, candidate| {
                let before = candidate.workspace_heads().unwrap_or_default();
                let answer = change(candidate)?;
                let changed = candidate.workspace_heads().unwrap_or_default() != before;
                Ok(((answer, changed), changed))
            },
            |record, _| Ok(record.collaboration.clone()),
        )?;
        if changed {
            self.settle(&mut workspace).await?;
        }
        Ok(answer)
    }

    /// The device holding `workspace`'s lease `lease` now, if any: the one
    /// whose claim wins it in the workspace's own state, as every device that
    /// holds the workspace reads it.
    ///
    /// # Errors
    /// The workspace is not held here.
    pub async fn lease_holder(
        &self,
        workspace: &str,
        lease: &str,
    ) -> Result<Option<[u8; 32]>, CollaborationError> {
        let now = system_time(tilo_clock::unix_now_ns());
        Ok(self
            .workspace(workspace)
            .await?
            .lock()
            .await
            .peer()
            .workspace()
            .lease(lease, now)
            .holder
            .map(|claim| claim.device))
    }

    /// Install a bot as a governed member of `workspace`, update its program,
    /// renew its delegation or uninstall it
    /// ([`WorkspaceMembershipCommand::InstallBot`],
    /// [`WorkspaceMembershipCommand::RenewBot`], `UninstallBot`), as the
    /// workspace's administrator. Every member reads and enforces the result, so it is
    /// a governed change rather than this host's own state.
    ///
    /// # Errors
    /// Refuses non-administrators and anything else; the peer refuses an
    /// install or renewal its rules do not admit.
    ///
    /// [`WorkspaceMembershipCommand::InstallBot`]: tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::InstallBot
    /// [`WorkspaceMembershipCommand::RenewBot`]: tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::RenewBot
    pub async fn govern_bot(
        &self,
        workspace: &str,
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
            WorkspaceOp::Host(ServiceOperation::GovernBot),
            now,
        )?;
        command.validate()?;
        // The command names itself: the same install or renewal sent again
        // is the same administration request.
        let digest = request_digest(&("bot.govern", &command))?;
        let workspace = self.workspace(workspace).await?;
        let mut workspace = workspace.lock().await;
        let key = self.inner.host.key.clone();
        workspace
            .peer()
            .clone()
            .workspace_mut()
            .apply_workspace_membership_command(administrator, &command, &key, system_time(now))?;
        self.administer(
            &mut workspace,
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

    /// Ask `workspace`'s host to run a bot from the directory, or take the
    /// request back ([`WorkspaceMembershipCommand::RequestBot`],
    /// [`WorkspaceMembershipCommand::WithdrawBotRequest`]), as a member the
    /// workspace lets install (or uninstall) bots: any such member of a workspace this
    /// host governs, not only its administrators. The request is a governed
    /// change every member reads; [`Self::bot_requests`] is what the host
    /// follows.
    ///
    /// # Errors
    /// Refuses another command, a malformed request, and a member the workspace
    /// does not let install (or uninstall) bots.
    ///
    /// [`WorkspaceMembershipCommand::RequestBot`]: tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::RequestBot
    /// [`WorkspaceMembershipCommand::WithdrawBotRequest`]: tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::WithdrawBotRequest
    pub async fn ask_for_bot(
        &self,
        workspace: &str,
        member: &DeviceCert,
        request_id: [u8; 32],
        command: tilo_peer_state::workspace_membership::WorkspaceMembershipCommand,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        use tilo_peer_state::workspace_membership::WorkspaceMembershipCommand as Command;
        let check = match &command {
            Command::RequestBot(_) => governance::INSTALL_BOT,
            Command::WithdrawBotRequest { .. } => governance::UNINSTALL_BOT,
            _ => {
                return Err(CollaborationError::new(
                    "only asking for a bot or taking the request back asks for a bot",
                ));
            }
        };
        self.govern_as_member(workspace, member, request_id, command, check, "bot.request")
            .await
    }

    /// Choose the version of an app `workspace` runs for every member, or take
    /// the choice back ([`WorkspaceMembershipCommand::InstallApp`],
    /// [`WorkspaceMembershipCommand::UninstallApp`]), as a member the workspace
    /// lets install apps. The choice is a governed change every member
    /// reads ([`Self::app_versions`]); each is to run it, and a
    /// change made with another version waits until its author has.
    ///
    /// # Errors
    /// Refuses another command, a malformed release, and a member the workspace
    /// does not let install apps.
    ///
    /// [`WorkspaceMembershipCommand::InstallApp`]: tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::InstallApp
    /// [`WorkspaceMembershipCommand::UninstallApp`]: tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::UninstallApp
    pub async fn govern_app(
        &self,
        workspace: &str,
        member: &DeviceCert,
        request_id: [u8; 32],
        command: tilo_peer_state::workspace_membership::WorkspaceMembershipCommand,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        use tilo_peer_state::workspace_membership::WorkspaceMembershipCommand as Command;
        if !matches!(
            command,
            Command::InstallApp(_) | Command::UninstallApp { .. }
        ) {
            return Err(CollaborationError::new(
                "only installing or uninstalling an app version governs an app",
            ));
        }
        self.govern_as_member(
            workspace,
            member,
            request_id,
            command,
            governance::INSTALL_APP,
            "app.version",
        )
        .await
    }

    /// The app versions `workspace` runs, as its governance records them now.
    pub async fn app_versions(
        &self,
        workspace: &str,
    ) -> Result<Vec<tilo_peer_state::InstalledApp>, CollaborationError> {
        Ok(self
            .workspace(workspace)
            .await?
            .lock()
            .await
            .peer()
            .governance()
            .app_versions())
    }

    /// Apply `command` to `workspace` as `member`, whom the workspace must allow the
    /// governance action `check`, and publish it; `tag` names the request
    /// kind in its digest.
    async fn govern_as_member(
        &self,
        workspace: &str,
        member: &DeviceCert,
        request_id: [u8; 32],
        command: tilo_peer_state::workspace_membership::WorkspaceMembershipCommand,
        check: &'static str,
        tag: &'static str,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        command.validate()?;
        let now = tilo_clock::unix_now_ns();
        let workspace = self.workspace(workspace).await?;
        let mut workspace = workspace.lock().await;
        self.authorize(member, Some(&workspace), WorkspaceOp::Govern(check), now)?;
        let key = self.inner.host.key.clone();
        workspace
            .peer()
            .clone()
            .workspace_mut()
            .apply_workspace_membership_command(member, &command, &key, system_time(now))?;
        self.administer(
            &mut workspace,
            request_id,
            request_digest(&(tag, &command))?,
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

    /// The bots `workspace` asks its host to run, as its governance records
    /// them now.
    pub async fn bot_requests(
        &self,
        workspace: &str,
    ) -> Result<Vec<tilo_peer_state::RequestedBot>, CollaborationError> {
        Ok(self
            .workspace(workspace)
            .await?
            .lock()
            .await
            .peer()
            .governance()
            .bot_requests())
    }

    /// Installation `id` of `workspace` as its governance records it now: the
    /// live row, else the last withdrawn one.
    pub async fn installed_bot(
        &self,
        workspace: &str,
        id: &str,
    ) -> Result<Option<tilo_peer_state::InstalledBot>, CollaborationError> {
        let now = system_time(tilo_clock::unix_now_ns());
        Ok(self
            .workspace(workspace)
            .await?
            .lock()
            .await
            .peer()
            .governance()
            .installation(id, now))
    }

    pub(super) async fn administer(
        &self,
        workspace: &mut GovernedWorkspace,
        request_id: [u8; 32],
        digest: [u8; 32],
        change: impl FnOnce(&mut tilo_peer_state::StatePeer) -> Result<bool, String>,
        now: u64,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        if let Administered::Done(generation) =
            workspace.collaboration().administered(request_id, digest)?
        {
            return Ok(GovernanceCompletion { generation });
        }
        // Staged from the replica held until the change commits: a member's
        // write its link takes in meanwhile waits, rather than landing between
        // the copy the change was made on and the commit, which then refused
        // the change for dropping that write.
        let generation = {
            let shared = workspace.peer.shared();
            let mut replica = shared.lock();
            let mut candidate = replica.clone();
            change(&mut candidate)?;
            let mut state = workspace.collaboration().clone();
            state.record_administration(
                request_id,
                digest,
                workspace.record.generation.saturating_add(1),
            );
            let committed = workspace.commit_held(
                &mut replica,
                candidate,
                (&self.inner.host.key, system_time(now)),
                super::Change::Other,
                |_, _| Ok(state),
            );
            drop(replica);
            committed?
        };
        self.settle(workspace).await?;
        Ok(GovernanceCompletion { generation })
    }

    /// Wait until `workspace`'s next write, which takes the workspace once for every
    /// change staged in it before: a crowd joining wrote the whole workspace once
    /// per challenge, one after another under its lock.
    /// With `admissions`, the write saves the workspace's pre-admissions too.
    pub(super) async fn written(
        &self,
        workspace: &str,
        admissions: bool,
    ) -> Result<(), CollaborationError> {
        let (answer, wait) = tokio::sync::oneshot::channel();
        if self.queue_write(workspace, Written { admissions, answer }) {
            // Its own task: a request that goes away mid-write must not cut
            // off the answers of everyone else waiting on it.
            let workspaces = self.clone();
            let workspace = workspace.to_owned();
            tokio::spawn(async move { workspaces.write_queued(&workspace).await });
        }
        wait.await
            .map_err(|_| CollaborationError::new("the workspace stopped writing"))?
    }

    /// Queue `written` for `workspace`'s next write. True when it is the first
    /// since the workspace last took them, so its sender starts the write.
    fn queue_write(&self, workspace: &str, written: Written) -> bool {
        let mut queued = self.inner.queued_writes.lock_recover();
        let waiting = queued.entry(workspace.to_owned()).or_default();
        waiting.push(written);
        let first = waiting.len() == 1;
        drop(queued);
        first
    }

    async fn write_queued(&self, identity: &str) {
        let take = || {
            self.inner
                .queued_writes
                .lock_recover()
                .remove(identity)
                .unwrap_or_default()
        };
        let (result, waiting) = match self.workspace(identity).await {
            Err(error) => (Err(error), take()),
            Ok(workspace) => {
                let mut workspace = workspace.lock().await;
                // Taken under the workspace: whoever queued staged their change
                // before.
                let waiting = take();
                let mut result = workspace.persist();
                if result.is_ok() && waiting.iter().any(|written| written.admissions) {
                    result = workspace.admissions.save();
                }
                drop(workspace);
                (result.map_err(CollaborationError::new), waiting)
            }
        };
        for written in waiting {
            let _ = written.answer.send(result.clone());
        }
    }

    /// Admit the verified `joins` to `workspace` in one governance change: the
    /// shared admission ([`tilo_peer_state::join_answer::admit`]), published before anyone
    /// hears their answer. People arriving together cost the workspace one write
    /// and one publication instead of one each, and each of those grows
    /// with the workspace.
    ///
    /// Answers each join in order. Fails as a whole when the change cannot
    /// be committed or published.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the replica stays held until the admission is committed over it"
    )]
    pub(super) async fn admit_redemptions(
        &self,
        workspace: &mut GovernedWorkspace,
        joins: &[Redeeming<'_>],
        now: u64,
    ) -> Result<Vec<Result<JoinBootstrap, CollaborationError>>, CollaborationError> {
        let key = &self.inner.host.key;
        let deciding = crate::timing::start();
        let (admitted, committed) = {
            let shared = workspace.peer.shared();
            let mut replica = shared.lock();
            let mut held =
                HeldWorkspace::new(key, self.serving_certificate(), workspace, &mut replica);

            let admitted = tilo_peer_state::join_answer::admit(&mut held, joins, key, now);
            (admitted, held.committed)
        };
        crate::timing::event("join.admit.decide", deciding, joins.len() as u64);
        if committed {
            let settling = crate::timing::start();
            self.settle(workspace).await?;
            crate::timing::event("join.admit.settle", settling, joins.len() as u64);
            // The workspace keeps the grant of every key it admitted, so every
            // holder and every API server reads the same grant from the
            // workspace itself, whichever way the key came in.
            if let Ok(answers) = &admitted {
                for (join, answer) in joins.iter().zip(answers) {
                    let certificate = join.redemption.certificate();
                    if answer.is_ok()
                        && let Some(scope) = certificate.capabilities.agent
                    {
                        self.list_grant(workspace, certificate, scope, join.name, now)
                            .await?;
                    }
                }
            }
            // Someone invited from the wait list is in now: their entry
            // leaves it.
            workspace.forget_joined_wait_list();
            // The person whose own workspace this is administers it from
            // their first device on.
            let owner = self
                .own()
                .and_then(|own| own.owner_of(workspace.workspace()))
                .filter(|owner| {
                    joins
                        .iter()
                        .any(|join| join.redemption.certificate().user_public_key == *owner)
                });
            if let Some(owner) = owner {
                self.make_administrator(workspace, owner, now).await?;
            }
        }
        admitted
    }

    /// Name `person` as whom `workspace` is kept for when it is a workspace of
    /// their own here and the workspace does not say so yet: a workspace
    /// made before its governance named the person it is kept for gains the
    /// grant the first time they govern its administrators. Nothing for
    /// anyone else, nor while they have no device there.
    pub(super) async fn ensure_keeper(
        &self,
        workspace: &mut GovernedWorkspace,
        person: [u8; 32],
        now: u64,
    ) -> Result<(), CollaborationError> {
        let theirs = self
            .own()
            .and_then(|own| own.owner_of(workspace.workspace()))
            .is_some_and(|owner| owner == person);
        if !theirs {
            return Ok(());
        }
        let kept = workspace
            .peer()
            .governance()
            .governed_policy()
            .map_err(CollaborationError::new)?;
        let root = hex::encode(person);
        if kept.is_keeper(&root) || !kept.is_member(&root) {
            return Ok(());
        }
        self.make_administrator(workspace, person, now).await
    }

    /// Make `person`, admitted to the workspace of their own `workspace`, its
    /// administrator; nothing while they have no device there.
    pub(super) async fn administer_own(
        &self,
        workspace: &str,
        person: [u8; 32],
        now: u64,
    ) -> Result<(), CollaborationError> {
        let held = self.workspace(workspace).await?;
        let mut held = held.lock().await;
        let members = held
            .peer()
            .governance()
            .governed_members(system_time(now))
            .map_err(CollaborationError::new)?
            .members;
        if members.iter().any(|member| member.root() == Some(person)) {
            self.make_administrator(&mut held, person, now).await?;
        }
        drop(held);
        Ok(())
    }

    /// Make `person`, the one the workspace of their own `workspace` is kept for here, its
    /// administrator as a device of theirs joins it before any of theirs has entered it: the
    /// person a workspace of this host's own belongs to administers it from their first device on.
    pub(super) async fn own_person_administers(
        &self,
        workspace: &mut GovernedWorkspace,
        person: [u8; 32],
        now: u64,
    ) -> Result<(), CollaborationError> {
        self.make_administrator(workspace, person, now).await
    }

    /// Make the admitted `person` an administrator of `workspace`, held: an
    /// Editor with every governance and moderation action, and the person
    /// the workspace is kept for, who makes and unmakes its administrators
    /// as its owner, this host, does. Writes nothing when they are both.
    async fn make_administrator(
        &self,
        workspace: &mut GovernedWorkspace,
        person: [u8; 32],
        now: u64,
    ) -> Result<(), CollaborationError> {
        let key = &self.inner.host.key;
        let mut candidate = workspace.peer().clone();
        let administrator = candidate.governance_mut().set_member_administrator(
            person,
            None,
            key,
            system_time(now),
        )?;
        let kept = candidate
            .governance_mut()
            .set_keeper(person, key, system_time(now))?;
        if !(administrator || kept) {
            return Ok(());
        }
        let state = workspace.collaboration().clone();
        workspace.commit(candidate, key, system_time(now), |_, _| Ok(state))?;
        self.settle(workspace).await?;
        Ok(())
    }
}

/// A workspace of this server while a governor decides, under its lock: whether
/// its log changed unwritten, and whether a governance change was
/// committed that its members have not been sent yet.
struct HeldWorkspace<'a> {
    key: &'a SigningKey,
    /// The device answering this workspace's joins: this server's own, which
    /// need not be the device that governs the workspace.
    answering: &'a DeviceCert,
    workspace: &'a mut GovernedWorkspace,
    /// The workspace's state, held for the decision: a member's write reaches
    /// the replica over the link it syncs on, without this workspace's lock.
    replica: &'a mut tilo_peer_state::StatePeer,
    staged: bool,
    committed: bool,
}

impl<'a> HeldWorkspace<'a> {
    const fn new(
        key: &'a SigningKey,
        answering: &'a DeviceCert,
        workspace: &'a mut GovernedWorkspace,
        replica: &'a mut tilo_peer_state::StatePeer,
    ) -> Self {
        Self {
            key,
            answering,
            workspace,
            replica,
            staged: false,
            committed: false,
        }
    }
}

impl WorkspaceTxn for HeldWorkspace<'_> {
    fn workspace(&self) -> &str {
        self.workspace.workspace()
    }

    fn governed(&self) -> Governed<'_> {
        self.workspace.governed_from(self.replica, self.answering)
    }

    fn log(&self) -> &CollaborationState {
        self.workspace.collaboration()
    }

    fn stage_log(&mut self, log: CollaborationState) {
        if *self.workspace.collaboration() != log {
            self.workspace.record.collaboration = log;
            self.staged = true;
        }
    }

    fn commit_governance(
        &mut self,
        peer: tilo_peer_state::StatePeer,
        now: u64,
        log: &mut dyn FnMut(&Governed<'_>) -> Result<CollaborationState, CollaborationError>,
    ) -> Result<(), CollaborationError> {
        self.workspace.commit_held(
            self.replica,
            peer,
            (self.key, system_time(now)),
            super::Change::Other,
            |record, peer| {
                log(&governed(record, peer.into(), self.answering))
                    .map_err(|error| error.to_string())
            },
        )?;
        // The commit wrote the whole record, the log staged before
        // included.
        self.staged = false;
        self.committed = true;
        Ok(())
    }

    fn commit_admission(
        &mut self,
        peer: tilo_peer_state::StatePeer,
        now: u64,
        log: &mut dyn FnMut(&Governed<'_>) -> Result<CollaborationState, CollaborationError>,
    ) -> Result<(), CollaborationError> {
        self.workspace.commit_held(
            self.replica,
            peer,
            (self.key, system_time(now)),
            super::Change::Admission,
            |record, peer| {
                log(&governed(record, peer.into(), self.answering))
                    .map_err(|error| error.to_string())
            },
        )?;
        self.staged = false;
        self.committed = true;
        Ok(())
    }
}

/// This server's workspaces, to the governor that answers their workspace-join
/// exchange: a change is staged in the workspace's record and written once for
/// everyone who staged one meanwhile (`Workspaces::written`); a governance
/// change is committed under the workspace's lock and published before the
/// lock is released.
impl JoinWorkspaces for Workspaces {
    fn signer(&self) -> &dyn collab_types::signer::DeviceSigner {
        &self.inner.host.key
    }

    fn nonce(&self) -> Result<[u8; 32], CollaborationError> {
        random()
    }

    async fn with_workspace<T: Send>(
        &self,
        workspace: &str,
        decide: impl FnOnce(&mut dyn WorkspaceTxn) -> Result<T, CollaborationError> + Send,
    ) -> Result<T, CollaborationError> {
        let held = self.workspace(workspace).await?;
        let mut locked = held.lock().await;
        let (answer, staged, committed) = {
            // A member's write reaches this workspace's replica over the link it
            // syncs on, without this workspace's lock: holding the replica for
            // the decision is what keeps the state a change is built from the
            // one it is committed over.
            let shared = locked.peer.shared();
            let mut replica = shared.lock();
            let mut held = HeldWorkspace::new(
                &self.inner.host.key,
                self.serving_certificate(),
                &mut locked,
                &mut replica,
            );
            let answer = decide(&mut held);
            (answer, held.staged, held.committed)
        };
        if committed {
            self.settle(&mut locked).await?;
        }
        drop(locked);
        if staged {
            self.written(workspace, false).await?;
        }
        answer
    }
}

impl GovernedWorkspace {
    /// Whether a device redeemed the invitation `invite_id`.
    pub(super) fn redeemed(&self, invite_id: [u8; 32]) -> bool {
        self.collaboration().redeemed(invite_id)
    }

    /// The certified device admitted by the committed request receipt.
    pub(super) fn admitted_device(&self, request_id: [u8; 32]) -> Option<[u8; 32]> {
        self.collaboration().admitted_device(request_id)
    }
}

pub(super) use tilo_peer_state::join_log::request_digest;

/// A refusal of `device` when `workspace` keeps it as a revoked key.
fn refuse_revoked(
    workspace: &GovernedWorkspace,
    device: &DeviceCert,
) -> Result<(), CollaborationError> {
    let revoked_ms = workspace
        .peer()
        .governance()
        .workspace_grants()
        .into_iter()
        .find(|grant| grant.device == hex::encode(device.device_public_key))
        .and_then(|grant| grant.revoked_ms);
    revoked_ms.map_or(Ok(()), |revoked_ms| {
        Err(CollaborationError::new(format!(
            "key_revoked: this key was revoked at {revoked_ms}"
        )))
    })
}

/// The devices `workspace`'s governance keeps as keys its person revoked.
pub(super) fn revoked_keys(workspace: &GovernedWorkspace) -> Vec<[u8; 32]> {
    workspace
        .peer()
        .governance()
        .workspace_grants()
        .into_iter()
        .filter(|grant| grant.revoked_ms.is_some())
        .filter_map(|grant| {
            let mut key = [0; 32];
            match hex::decode_to_slice(&grant.device, &mut key) {
                Ok(()) => Some(key),
                Err(error) => {
                    tracing::warn!(
                        device = %grant.device,
                        %error,
                        "a grant's device is not a key"
                    );
                    None
                }
            }
        })
        .collect()
}
