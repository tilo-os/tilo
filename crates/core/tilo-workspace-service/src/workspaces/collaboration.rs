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
use tilo_peer_state::join::{JoinBootstrap, JoinCompleteRequest};
use tilo_peer_state::join_answer::{JoinWorkspaces, WorkspaceTxn};
use tilo_peer_state::join_log::{Administered, Governed, Signer};
pub use tilo_peer_state::join_log::{JoinError as CollaborationError, Redeeming};
use tilo_server_protocol::workspaces::{
    CancelInvitationRequest, IssueInvitationRequest, IssuedInvitation, PendingInvitation,
    RemoveMemberRequest, SetMemberRoleRequest,
};
use tilo_user_identity::{AgentScope, DeviceCert};

use tilo_action_policy::roles::governance;
use tilo_action_policy::{Grantee, Role};

use super::authority::WorkspaceOp;
use super::{GovernedWorkspace, WorkspaceDocumentEntry, Workspaces, system_time};
use tilo_lock::MutexRecover as _;

/// The name an invitation carries for a person: what the workspace shows them
/// by, where they chose one an invitation may sign.
pub(super) fn carried(name: &str) -> Option<String> {
    let name = name.trim();
    (!name.is_empty() && name.chars().count() <= tilo_peer_state::join::MAX_INVITER_NAME_CHARS)
        .then(|| name.to_owned())
}

/// The root key `person`, a membership change names, spells.
fn root(person: &str) -> Result<[u8; 32], String> {
    hex::decode(person)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| "a member is named by their user root key".to_owned())
}

pub use tilo_peer_state::workspace_membership::MemberPlace;

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
            self.authorize(
                administrator,
                Some(&governed),
                WorkspaceOp::admit(request.role, makes_administrator),
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
            WorkspaceOp::Govern(governance::ADD_MEMBER),
            tilo_clock::unix_now_ns(),
        )?;
        let mut state = workspace.collaboration().clone();
        if state.cancel(&request)? {
            workspace.commit_state(state)?;
        }
        Ok(())
    }

    /// The signed invitation `workspace` keeps under the secret a short link
    /// carries ([`tilo_peer_state::join::native::bearer_text`]), while it may
    /// still be redeemed: what this host answers whoever follows the link,
    /// before they are anyone it knows. `None` for a secret that names
    /// nothing here, or an invitation spent, lapsed or taken back.
    pub async fn kept_invitation(&self, workspace: &str, secret: &str) -> Option<Vec<u8>> {
        let bearer = tilo_peer_state::join::native::parse_bearer(secret)?;
        let workspace = self.workspace(workspace).await.ok()?;
        let workspace = workspace.lock().await;
        workspace
            .collaboration()
            .invitation_by_bearer(&bearer, tilo_clock::unix_now_ns())
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
            WorkspaceOp::Govern(governance::ADD_MEMBER),
            now,
        )?;
        Ok(workspace.collaboration().pending(now))
    }

    /// Enrol `device`, a further device of a member of `workspace`, under its person. Returns
    /// whether the member list changed.
    ///
    /// # Errors
    ///
    /// The device is no device of a member's own, or the workspace is not held here.
    pub async fn enter(
        &self,
        workspace: &str,
        device: &DeviceCert,
    ) -> Result<bool, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        self.check_now(device, Some(workspace), WorkspaceOp::Enter)
            .await?;
        self.admit_entering(workspace, device, now).await
    }

    /// An invitation for `device`'s own person to redeem `workspace` with, as a device that
    /// entered it does: at their own role, an Editor's at most, so it hands them nothing the
    /// member list does not. Asking for it is no invitation of anyone, so it needs no grant to
    /// invite.
    ///
    /// # Errors
    ///
    /// The person is no member there, or the invitation cannot be recorded.
    pub(super) async fn own_invitation(
        &self,
        workspace: &str,
        device: &DeviceCert,
        (request_id, expires_at_ns): ([u8; 32], u64),
        address: &iroh_base::EndpointAddr,
    ) -> Result<IssuedInvitation, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        let issued = {
            let governed = self.workspace(workspace).await?;
            let mut governed = governed.lock().await;
            let person = hex::encode(device.user_public_key);
            let policy = governed
                .peer()
                .governance()
                .governed_policy()
                .map_err(CollaborationError::new)?;
            let role = match policy.active_role(&person) {
                Some(role @ (Role::Viewer | Role::Commenter)) => role,
                Some(_) => Role::Editor,
                None => {
                    return Err(CollaborationError::new(
                        tilo_action_policy::Refusal::NotAMember(person).to_string(),
                    ));
                }
            };
            let request = IssueInvitationRequest {
                request_id,
                role,
                recipient_user_public_key: Some(device.user_public_key),
                invitee_name: None,
                expires_at_ns,
                inviter_name: self.member_name(&governed, device),
            };
            self.stage_issue(&mut governed, device, request, false, address, now)?
        };
        self.written(workspace, false).await?;
        Ok(issued)
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
                let admitted = candidate.governance_mut().admit_member_device(
                    device,
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
            .decides_for(workspace.workspace_id(), device.device_public_key)
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
            scope: scope.workspace_name(),
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

    /// Make `agent` a member of `workspace` acting for `person`, written for the member `by`
    /// ([`tilo_action_policy::Change::add_agent`]): what every place a turn or this server
    /// makes an agent calls. `by` is the person, or an agent acting for them, whose right
    /// decides, narrowed by this server's own: a member adds their own agents, and this device
    /// signs the change as a member write for them. An agent the workspace holds already is
    /// no change.
    ///
    /// # Errors
    /// The workspace refuses the change, or its governance cannot be updated.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the workspace stays locked from the check through the signed write"
    )]
    pub async fn add_agent(
        &self,
        workspace: &str,
        by: tilo_action_policy::MemberId,
        agent: &str,
        person: &str,
    ) -> Result<(), CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        let workspace = self.workspace(workspace).await?;
        let mut workspace = workspace.lock().await;
        let host = &self.inner.host;
        let member = workspace
            .peer()
            .permissions(host.cert.device_public_key, system_time(now))?;
        if member.policy.agents.contains_key(agent) {
            return Ok(());
        }
        let change = tilo_action_policy::Change::add_agent(agent, person, Some(by));
        let signer = hex::encode(host.cert.user_public_key);
        member
            .policy
            .check(
                &signer,
                &member.policy.person_on_device(&signer, false),
                &change,
                &tilo_action_policy::Context::at_time(system_time(now)),
                false,
            )
            .map_err(|refusal| CollaborationError::new(refusal.to_string()))?;
        let request_id = *blake3::hash(format!("add-agent:{agent}").as_bytes()).as_bytes();
        let key = host.key.clone();
        self.administer(
            &mut workspace,
            request_id,
            request_digest(&("member-list", &change))?,
            |peer| {
                peer.governance_mut()
                    .change_member_list(change, &key, system_time(now))
            },
            now,
        )
        .await
        .map(drop)
    }

    /// Make `change`, a custom role's or an agent's, to `workspace`'s member list as
    /// `caller`, whom the workspace must allow it. An agent change this server signs for a
    /// caller of another person names them as whom it is made for.
    pub(super) async fn edit_member_list(
        &self,
        workspace: &str,
        caller: &DeviceCert,
        request_id: [u8; 32],
        mut change: tilo_action_policy::Change,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        use tilo_action_policy::Change;
        match &mut change {
            Change::AddAgent { by, .. } | Change::RemoveAgent { by, .. } => {
                if caller.user_public_key != self.inner.host.cert.user_public_key {
                    *by = Some(tilo_action_policy::MemberId::person(hex::encode(
                        caller.user_public_key,
                    )));
                }
            }
            change if change.is_custom_role_change() => {}
            _ => {
                return Err(CollaborationError::new(
                    "expected a custom role or agent change",
                ));
            }
        }
        let now = tilo_clock::unix_now_ns();
        let workspace = self.workspace(workspace).await?;
        let mut workspace = workspace.lock().await;
        self.authorize(caller, Some(&workspace), WorkspaceOp::Change(&change), now)?;
        let key = self.inner.host.key.clone();
        self.administer(
            &mut workspace,
            request_id,
            request_digest(&("custom-role", &change))?,
            |peer| {
                peer.governance_mut()
                    .change_member_list(change, &key, system_time(now))
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
            if tilo_action_policy::roles::is_private_record(app, document) {
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

    /// Write workspace grants (workspace settings such as a role's Deny on a kind), keyed by
    /// their ids, as `administrator` decided.
    ///
    /// # Errors
    ///
    /// Refuses a device whose person the workspace does not let grant, a workspace
    /// this server's person does not administer ([`Workspaces::administers`]), and
    /// grants for another workspace.
    pub async fn put_workspace_grants(
        &self,
        workspace: &str,
        administrator: &DeviceCert,
        request_id: [u8; 32],
        grants: Vec<tilo_action_policy::Grant>,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        let workspace = self.workspace(workspace).await?;
        let mut workspace = workspace.lock().await;
        self.authorize(
            administrator,
            Some(&workspace),
            WorkspaceOp::Govern(governance::GRANT),
            now,
        )?;
        if !self.administers(&workspace) {
            return Err(CollaborationError::new(
                "this server's person does not administer the workspace",
            ));
        }
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
    /// does not administer ([`Workspaces::administers`]) is refused.
    pub async fn remove_workspace_grants(
        &self,
        workspace: &str,
        administrator: &DeviceCert,
        request_id: [u8; 32],
        ids: Vec<tilo_action_policy::GrantId>,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        let workspace = self.workspace(workspace).await?;
        let mut workspace = workspace.lock().await;
        self.authorize(
            administrator,
            Some(&workspace),
            WorkspaceOp::Govern(governance::REVOKE),
            now,
        )?;
        if !self.administers(&workspace) {
            return Err(CollaborationError::new(
                "this server's person does not administer the workspace",
            ));
        }
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

    /// Stop the agent `agent` in `workspace`, as the person this host acts for:
    /// an answer denying it everything there
    /// ([`tilo_action_policy::agent_answers::stop_grant`]), which its certificate
    /// and its earlier answers cannot lift, until [`Self::let_agent_go_on`].
    ///
    /// # Errors
    ///
    /// Why the workspace did not take the answer, or does not hold this device
    /// as a member.
    pub async fn answer_stop(&self, workspace: &str, agent: &str) -> Result<(), String> {
        let person = Grantee::person(hex::encode(self.inner.host.cert.user_public_key));
        let stop = tilo_action_policy::agent_answers::stop_grant(workspace, agent, person);
        self.commit_own_change(
            workspace,
            tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::Member {
                change: tilo_action_policy::Change::Grant(stop),
            },
            None,
        )
        .await
    }

    /// Let the agent `agent` its person stopped go on in `workspace`: the
    /// answer [`Self::stop_agent`] wrote is taken back. A workspace holding no
    /// such answer is unchanged.
    ///
    /// # Errors
    ///
    /// Why the workspace did not take the revocation, or does not hold this
    /// device as a member.
    pub async fn answer_go_on(&self, workspace: &str, agent: &str) -> Result<(), String> {
        let revoke = tilo_action_policy::Change::Revoke(
            tilo_action_policy::agent_answers::stop_grant_id(agent),
        );
        self.commit_own_change(
            workspace,
            tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::Member {
                change: revoke,
            },
            None,
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

    /// Remove the member `request.user_public_key` and every device of theirs
    /// from `workspace`. The server stops admitting them before this returns.
    pub async fn remove_member(
        &self,
        workspace: &str,
        administrator: &DeviceCert,
        request: RemoveMemberRequest,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        let person = request.user_public_key;
        let done = self
            .change_membership(workspace, administrator, request, Membership::Remove)
            .await;
        if done.is_ok() {
            self.unlist_for(&person, workspace).await;
        }
        done
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
        let named = workspace.to_owned();
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
        drop(workspace);
        for person in people {
            self.unlist_for(person, &named).await;
        }
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
    /// user governs its workspaces, and any other member by the workspace's grants on
    /// the action (the Moderator role holds them).
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
        Ok(completion)
    }

    /// Make `change`, a membership action, as the workspace's moderation bot `bot`: the same
    /// restrict, unrestrict, ban and unban a person with moderation authority makes, decided
    /// under the grants the workspace gave the bot when it turned its moderation agent on
    /// ([`tilo_action_policy::bot::moderation_grants`]) and recorded as the bot's, so history
    /// and every device name it like any other decision. Its grants reach every member but the
    /// owner and the workspace's administrators, as anyone's grants on those actions do.
    ///
    /// # Errors
    /// The bot's grants do not allow the change, the member is out of its reach, or the change
    /// could not commit.
    pub async fn moderate_member(
        &self,
        workspace: &str,
        bot: &str,
        change: tilo_action_policy::Change,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        let workspace = self.workspace(workspace).await?;
        let mut workspace = workspace.lock().await;
        let policy = workspace
            .peer()
            .governance()
            .governed_policy()
            .map_err(CollaborationError::new)?;
        let authority = tilo_action_policy::Authority::person(
            policy.party_of(tilo_action_policy::Grantee::bot(bot)),
        );
        // A bot is no member: its id is the one its grants name, decided at each governance
        // action's own node.
        let actor = format!("bot:{bot}");
        let cx = tilo_action_policy::Context::at_time(system_time(now));
        policy
            .check(&actor, &authority, &change, &cx, false)
            .map_err(|refusal| CollaborationError::new(refusal.to_string()))?;
        let by = tilo_action_policy::Grantee::bot(bot);
        let target = change
            .person()
            .map(root)
            .transpose()
            .map_err(CollaborationError::new)?;
        let identity = workspace.workspace().to_owned();
        let person = change.person().map(str::to_owned);
        let key = self.inner.host.key.clone();
        let digest = request_digest(&(&by, &change))?;
        let completion = self
            .administer(
                &mut workspace,
                digest,
                digest,
                |peer| match &change {
                    tilo_action_policy::Change::Ban { person } => peer
                        .governance_mut()
                        .ban_member_by(root(person)?, by.clone(), &key, system_time(now)),
                    tilo_action_policy::Change::Unban { person } => peer
                        .governance_mut()
                        .unban_member(root(person)?, &key, system_time(now)),
                    tilo_action_policy::Change::Restrict { person, until_ms } => {
                        peer.governance_mut().put_governed_grants(
                            &[tilo_action_policy::roles::restriction_grant(
                                &identity,
                                person,
                                *until_ms,
                                by.clone(),
                            )],
                            &key,
                            system_time(now),
                        )
                    }
                    tilo_action_policy::Change::Unrestrict { person } => {
                        root(person)?;
                        peer.governance_mut().remove_governed_grants(
                            &[tilo_action_policy::roles::restriction_grant_id(person)],
                            &key,
                            system_time(now),
                        )
                    }
                    _ => Err(format!("`{}` is not a moderation action", change.action())),
                },
                now,
            )
            .await?;
        match &change {
            tilo_action_policy::Change::Ban { .. } => {
                if let Some(target) = target {
                    let mut state = workspace.collaboration().clone();
                    state.withdraw_invitations(&std::collections::BTreeSet::from([target]));
                    workspace.commit_state(state)?;
                    workspace
                        .keep_out_removed(&std::collections::BTreeSet::from([target]), None, now)
                        .map_err(CollaborationError::new)?;
                }
            }
            tilo_action_policy::Change::Unban { .. } => {
                if let Some(target) = target {
                    workspace
                        .removals
                        .clear(target)
                        .map_err(CollaborationError::new)?;
                }
            }
            _ => {}
        }
        tracing::info!(
            workspace = %workspace.workspace(),
            %bot,
            person = person.as_deref().unwrap_or_default(),
            action = change.action(),
            "the moderation bot committed a membership change"
        );
        drop(workspace);
        Ok(completion)
    }

    /// Commit `command`, a membership change this host's own device makes, in `workspace` as the
    /// workspace's governor, with `receipt`, what the workspace's history says of it, in the
    /// same change. The host writes the workspace's history, so its history has one
    /// writer, and the device's own replica only ever receives what is written here. The
    /// workspace's policy decides the change against the host's grants, as it decides any
    /// member's.
    ///
    /// Answers the workspace's generation that carries the change, and the
    /// changes it added to the workspace's document, for the device's replica
    /// to take in at once (none when the workspace carried it already).
    ///
    /// # Errors
    /// Refuses a change the workspace's rules do not let this host make, and a change that
    /// could not commit.
    pub async fn commit_own_membership(
        &self,
        workspace: &str,
        command: tilo_peer_state::workspace_membership::WorkspaceMembershipCommand,
        receipt: Option<&tilo_desktop_model::WorkspaceEffectReceipt>,
    ) -> Result<(GovernanceCompletion, Vec<Vec<u8>>), CollaborationError> {
        let device = &self.inner.host.cert;
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
        if let Some(receipt) = receipt {
            changed
                .workspace_mut()
                .apply_own_effect_receipt(device, receipt, system_time(now))
                .map_err(CollaborationError::new)?;
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
        drop(workspace);
        Ok((GovernanceCompletion { generation }, added))
    }

    /// Write `change` into `workspace`'s own state as `device`, this host's
    /// own device, and publish it to the workspace's members. A lease or a
    /// schedule's progress belongs there: it is the workspace's state
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
    /// The workspace is not kept here.
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

    /// Whether a device of `workspace` kept the work of the lease `lease`
    /// done ([`tilo_peer_state::Done`]).
    ///
    /// # Errors
    /// The workspace is not kept here.
    pub async fn lease_done(
        &self,
        workspace: &str,
        lease: &str,
    ) -> Result<Option<serde_json::Value>, CollaborationError> {
        Ok(self
            .workspace(workspace)
            .await?
            .lock()
            .await
            .peer()
            .workspace()
            .done(lease)
            .map(|done| done.answer))
    }

    /// What the devices of `workspace` kept of the work done under every lease whose id starts
    /// with `prefix`, by lease id, each the answer of the device that did it first.
    ///
    /// # Errors
    /// The workspace is not kept here.
    pub async fn leases_done_under(
        &self,
        workspace: &str,
        prefix: &str,
    ) -> Result<std::collections::BTreeMap<String, serde_json::Value>, CollaborationError> {
        Ok(self
            .workspace(workspace)
            .await?
            .lock()
            .await
            .peer()
            .workspace()
            .done_under(prefix)
            .into_iter()
            .map(|(id, done)| (id, done.answer))
            .collect())
    }

    /// What the device that last looked kept of `trigger` of the automation
    /// `automation` in `workspace`, the trigger marked `mark`
    /// ([`tilo_peer_state::trigger_mark`]); `None` when no device kept any.
    ///
    /// # Errors
    /// The workspace is not kept here.
    pub async fn trigger_progress(
        &self,
        workspace: &str,
        automation: &str,
        trigger: usize,
        mark: &str,
    ) -> Result<Option<serde_json::Value>, CollaborationError> {
        Ok(self
            .workspace(workspace)
            .await?
            .lock()
            .await
            .peer()
            .workspace()
            .trigger_progress(automation, trigger, mark))
    }

    /// The latest fire time a device of `workspace` answered for the
    /// schedule at `trigger` of the automation `automation`, marked `mark`
    /// ([`tilo_peer_state::trigger_mark`]); `None` when none has.
    ///
    /// # Errors
    /// The workspace is not kept here.
    pub async fn schedule_answered(
        &self,
        workspace: &str,
        automation: &str,
        trigger: usize,
        mark: &str,
    ) -> Result<Option<i64>, CollaborationError> {
        Ok(self
            .workspace(workspace)
            .await?
            .lock()
            .await
            .peer()
            .workspace()
            .schedule_answered(automation, trigger, mark))
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

    /// Change the trust `workspace` keeps for packages — adding or replacing an
    /// attestor, removing one, or refusing a publisher — as a member the
    /// workspace lets install apps. Every member reads
    /// the lists ([`Self::package_trust`]) and verifies against them.
    ///
    /// # Errors
    /// Refuses another command, a malformed row, and a member the workspace
    /// does not let install apps.
    pub async fn change_package_trust(
        &self,
        workspace: &str,
        member: &DeviceCert,
        request_id: [u8; 32],
        command: tilo_peer_state::workspace_membership::WorkspaceMembershipCommand,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        use tilo_peer_state::workspace_membership::WorkspaceMembershipCommand as Command;
        if !matches!(
            command,
            Command::AddAttestor(_)
                | Command::RemoveAttestor { .. }
                | Command::RevokePublisher { .. }
        ) {
            return Err(CollaborationError::new(
                "only a trust list change governs the packages a workspace trusts",
            ));
        }
        self.govern_as_member(
            workspace,
            member,
            request_id,
            command,
            governance::INSTALL_APP,
            "package.trust",
        )
        .await
    }

    /// Change the registries `workspace` fetches apps from, as a member the
    /// workspace lets install apps. Every member reads
    /// the list ([`Self::registries`]) and fetches from it.
    ///
    /// # Errors
    /// Refuses another command, a malformed row, and a member the workspace
    /// does not let install apps.
    pub async fn change_package_registries(
        &self,
        workspace: &str,
        member: &DeviceCert,
        request_id: [u8; 32],
        command: tilo_peer_state::workspace_membership::WorkspaceMembershipCommand,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        use tilo_peer_state::workspace_membership::WorkspaceMembershipCommand as Command;
        if !matches!(
            command,
            Command::AddRegistry(_)
                | Command::RemoveRegistry { .. }
                | Command::SetRegistryEnabled { .. }
                | Command::SetRegistryChannel { .. }
        ) {
            return Err(CollaborationError::new(
                "only a registry change governs where a workspace fetches apps",
            ));
        }
        self.govern_as_member(
            workspace,
            member,
            request_id,
            command,
            governance::INSTALL_APP,
            "package.registries",
        )
        .await
    }

    /// The trust `workspace` keeps for packages: the attestors it accepts and
    /// the publisher keys it refuses, as its tree records them now.
    pub async fn package_trust(
        &self,
        workspace: &str,
    ) -> Result<
        (
            Vec<tilo_peer_state::TrustAttestor>,
            Vec<tilo_peer_state::RevokedPublisher>,
        ),
        CollaborationError,
    > {
        let workspace = self.workspace(workspace).await?;
        let locked = workspace.lock().await;
        let peer = locked.peer();
        Ok((
            peer.governance().trusted_attestors(),
            peer.governance().revoked_publishers(),
        ))
    }

    /// The releases of `app` `workspace` ran before the one it runs, the most recent first.
    pub async fn earlier_app_versions_of(
        &self,
        workspace: &str,
        app: &str,
    ) -> Result<Vec<tilo_peer_state::workspace_membership::AppRelease>, CollaborationError> {
        Ok(self
            .workspace(workspace)
            .await?
            .lock()
            .await
            .peer()
            .governance()
            .earlier_app_versions(app))
    }

    /// The registries `workspace` fetches apps from, as its tree records
    /// them now.
    pub async fn registries(
        &self,
        workspace: &str,
    ) -> Result<Vec<tilo_peer_state::PackageRegistry>, CollaborationError> {
        let mut rows = self
            .workspace(workspace)
            .await?
            .lock()
            .await
            .peer()
            .governance()
            .package_registries();
        Self::repair_official_pin(&mut rows);
        Ok(rows)
    }

    /// Restore the official registry's pin on its row when the row carries none (an emptied or
    /// placeholder key): the image's official key is what a new workspace records, so a row that
    /// lost the pin reads as that, never as trust in nobody. The row stays the workspace's to
    /// change; only the reading is whole.
    fn repair_official_pin(rows: &mut [tilo_peer_state::PackageRegistry]) {
        let seeded = super::default_apps::registries();
        let Some(official) = seeded.registries.first() else {
            return;
        };
        for row in rows {
            if row.name == official.name
                && (row.owner_pubkey_hex.is_empty()
                    || !row.owner_pubkey_hex.bytes().any(|byte| byte != b'0'))
            {
                row.owner_pubkey_hex.clone_from(&official.owner_pubkey_hex);
            }
        }
    }

    /// Change what the call in `channel` of `workspace` allows, as `member`, whom the workspace
    /// must grant `call`: a governed change every member's live session holds the call to.
    ///
    /// # Errors
    /// Refuses a malformed channel, and a member the workspace does not let moderate calls.
    pub async fn moderate_call(
        &self,
        workspace: &str,
        member: &DeviceCert,
        request_id: [u8; 32],
        channel: &str,
        change: collab_types::calls::CallChange,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        let done = self
            .govern_as_member(
                workspace,
                member,
                request_id,
                tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::Call {
                    channel: channel.to_owned(),
                    change,
                },
                governance::CALL,
                "call",
            )
            .await;
        // The live origin holds every call track to the call's rule: it reads the rule again now.
        if done.is_ok() {
            crate::authority_epoch::bump();
            self.found_call_keys(workspace, channel);
        }
        done
    }

    /// Found the key group of the generation `channel`'s call in `workspace` is in now, which
    /// seals its media from now on: the members it admits are handed its keys at once, rather
    /// than after the wait a device that cannot found a group gives one that can.
    fn found_call_keys(&self, workspace: &str, channel: &str) {
        let workspaces = self.clone();
        let (workspace, channel) = (workspace.to_owned(), channel.to_owned());
        tokio::spawn(async move {
            let Ok(rule) = workspaces.call_rule(&workspace, &channel).await else {
                return;
            };
            let route = tilo_peer_state::document_frame::DocumentRoute {
                app_id: collab_types::scope::MEDIA_APP.to_owned(),
                cluster_id: workspace,
                document: collab_types::calls::media_document(&channel, rule.generation),
            };
            if let Some(Err(error)) = workspaces.own_route_keys(&route, true).await {
                tracing::debug!(%error, channel, "the call's keys could not be founded here");
            }
        });
    }

    /// What the call in `channel` of `workspace` allows, as the workspace records it now.
    pub async fn call_rule(
        &self,
        workspace: &str,
        channel: &str,
    ) -> Result<collab_types::calls::CallRule, CollaborationError> {
        Ok(self
            .workspace(workspace)
            .await?
            .lock()
            .await
            .peer()
            .governance()
            .call(channel))
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
}

/// What a change committed while a workspace is held keeps with it: the
/// members' records an admission writes ([`super::admitting`]), answering
/// whose it kept.
pub(super) type Keeping<'a> = &'a dyn Fn(
    &mut tilo_peer_state::StatePeer,
    (&SigningKey, std::time::SystemTime),
) -> Result<Vec<[u8; 32]>, String>;

/// A workspace of this server while a governor decides, under its lock: whether
/// its log changed unwritten, and whether a governance change was
/// committed that its members have not been sent yet.
pub(super) struct HeldWorkspace<'a> {
    key: &'a SigningKey,
    /// The device answering this workspace's joins: this server's own, which
    /// need not be the device that governs the workspace.
    answering: &'a DeviceCert,
    workspace: &'a mut GovernedWorkspace,
    /// The workspace's state, held for the decision: a member's write reaches
    /// the replica over the link it syncs on, without this workspace's lock.
    replica: &'a mut tilo_peer_state::StatePeer,
    /// What a change committed here keeps along.
    keeping: Option<Keeping<'a>>,
    staged: bool,
    pub(super) committed: bool,
    /// Whose memberships the committed changes kept.
    pub(super) kept: Vec<[u8; 32]>,
}

impl<'a> HeldWorkspace<'a> {
    pub(super) const fn new(
        key: &'a SigningKey,
        answering: &'a DeviceCert,
        workspace: &'a mut GovernedWorkspace,
        replica: &'a mut tilo_peer_state::StatePeer,
        keeping: Option<Keeping<'a>>,
    ) -> Self {
        Self {
            key,
            answering,
            workspace,
            replica,
            keeping,
            staged: false,
            committed: false,
            kept: Vec::new(),
        }
    }

    /// Commit `peer` as `change`, with what it keeps along written in it,
    /// and `log`'s answer as the workspace's join log.
    fn commit(
        &mut self,
        mut peer: tilo_peer_state::StatePeer,
        now: u64,
        change: super::Change,
        log: &mut dyn FnMut(&Governed<'_>) -> Result<CollaborationState, CollaborationError>,
    ) -> Result<(), CollaborationError> {
        let kept = match self.keeping {
            Some(keep) => keep(&mut peer, (self.key, system_time(now)))?,
            None => Vec::new(),
        };
        self.workspace.commit_held(
            self.replica,
            peer,
            (self.key, system_time(now)),
            change,
            |record, peer| {
                log(&governed(record, peer.into(), self.answering))
                    .map_err(|error| error.to_string())
            },
        )?;
        // The commit wrote the whole record, the log staged before
        // included.
        self.staged = false;
        self.committed = true;
        self.kept.extend(kept);
        Ok(())
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
        self.commit(peer, now, super::Change::Other, log)
    }

    fn commit_admission(
        &mut self,
        peer: tilo_peer_state::StatePeer,
        now: u64,
        log: &mut dyn FnMut(&Governed<'_>) -> Result<CollaborationState, CollaborationError>,
    ) -> Result<(), CollaborationError> {
        self.commit(peer, now, super::Change::Admission, log)
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
                None,
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

    async fn admit_completed(
        &self,
        remote_endpoint: [u8; 32],
        request: &JoinCompleteRequest,
        now: u64,
    ) -> Option<Result<JoinBootstrap, CollaborationError>> {
        Some(
            self.admit_answered(remote_endpoint, request.proof.clone(), now)
                .await,
        )
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
