//! Invitations, their redemption and member administration of a workspace this
//! server hosts.
//!
//! What a host records between the steps of an exchange is the shared
//! [`JoinLog`], the same on every device that hosts workspaces; this module
//! owns who may ask what of this server's workspaces, and the durability barrier
//! before each answer.

use collab_types::workspace_grant::WorkspaceGrant;
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use tilo_peer_state::join::{JoinBootstrap, JoinCompleteRequest};
use tilo_peer_state::join_answer::{JoinWorkspaces, WorkspaceTxn};
use tilo_peer_state::join_log::{Administered, Listed, Signer};
pub use tilo_peer_state::join_log::{JoinError as CollaborationError, Redeeming};
use tilo_server_protocol::workspaces::{
    CancelInvitationRequest, IssueInvitationRequest, IssuedInvitation, PendingInvitation,
    RemoveMemberRequest, SetMemberRoleRequest,
};
use tilo_user_identity::DeviceCert;

use tilo_action_policy::Role;
use tilo_action_policy::roles::governance;
use tilo_member_types::{Actor, MemberId};
use tilo_peer_state::ActionStamp;

use super::authority::WorkspaceOp;
use super::{HostedWorkspace, WorkspaceDocumentEntry, Workspaces, system_time};
use tilo_lock::MutexRecover as _;

/// Why a read of a workspace's own state is refused: this host keeps no such workspace.
fn not_held() -> CollaborationError {
    CollaborationError::new("this server does not hold that workspace")
}

/// The person `person` (a root key in hex) as the host's actor: who a
/// governance change names as the one who decided it. `None` for a key that is
/// no person.
pub(super) fn actor_of(person: &str) -> Option<Actor> {
    tilo_failure::ignored!(
        "a change decided by a key that is no person names no decider, and still applies",
        tilo_member_types::PersonId::parse(person).ok()
    )
    .map(|person| Actor::Member(MemberId::person(person)))
}

/// The stamp of the governance action `verb`, decided by `by`: what every
/// governance change of it names, so a watcher of the workspace tells a ban
/// from a removal.
pub(super) fn stamped(verb: &'static str, by: Option<Actor>) -> Option<ActionStamp> {
    by.map(|by| ActionStamp::new(verb, by))
}

/// The name an invitation carries for a person: what the workspace shows them
/// by, where they chose one an invitation may sign.
pub(super) fn carried(name: &str) -> Option<String> {
    let name = name.trim();
    (!name.is_empty() && name.chars().count() <= tilo_peer_state::join::MAX_INVITER_NAME_CHARS)
        .then(|| name.to_owned())
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

impl HostedWorkspace {
    /// The workspace as its join log reads it: committed now, answered by
    /// `answering`, the device that answers its joins here.
    pub(super) fn hosted<'a>(&'a self, answering: &'a DeviceCert) -> Listed<'a> {
        hosted(&self.record, self.peer().into(), answering)
    }

    /// The workspace as its join log reads it, from the state the caller
    /// holds: what a decision reads is what its commit is measured against.
    pub(super) fn logged_from<'a>(
        &'a self,
        peer: &'a tilo_peer_state::StatePeer,
        answering: &'a DeviceCert,
    ) -> Listed<'a> {
        hosted(&self.record, peer.into(), answering)
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
/// joins here, which need not be the device that hosts it.
fn hosted<'a>(
    record: &'a WorkspaceDocumentEntry,
    peer: tilo_peer_state::join_log::PeerRef<'a>,
    answering: &'a DeviceCert,
) -> Listed<'a> {
    let generation = peer.governance().generation();
    Listed {
        peer,
        host: answering,
        workspace_id: record.workspace_id,
        generation,
        // A newcomer admits the workspace's governance alone: one message
        // holding its complete committed document. What members' devices
        // wrote of its records builds on that document, so it reaches the
        // newcomer through the workspace's sync, not through this answer.
        governance_messages: None,
    }
}

fn random<const N: usize>() -> Result<[u8; N], CollaborationError> {
    let mut bytes = [0; N];
    getrandom::fill(&mut bytes).map_err(|error| CollaborationError::new(error.to_string()))?;
    Ok(bytes)
}

/// Where an invitation says its workspace answers the one who redeems it.
pub(super) enum InvitedAt {
    /// This device, at its address.
    Here(iroh_base::EndpointAddr),
    /// Another device that holds the workspace, at the address it signed for it.
    There(Box<tilo_peer_state::join::TransportHint>),
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
            InvitedAt::Here(self.invitation_addr().await),
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
        at: InvitedAt,
    ) -> Result<IssuedInvitation, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        let issued = {
            let hosted = self.workspace(workspace).await?;
            let mut hosted = hosted.lock().await;
            self.authorize(
                administrator,
                Some(&hosted),
                WorkspaceOp::admit(request.role, makes_administrator),
                now,
            )?;
            // An invitation says who it is from. A caller that names nobody
            // (an API key, the command line) invites under the name the
            // workspace already shows for them, so the person it reaches
            // sees who asked them in, not "someone".
            if request.inviter_name.is_none() {
                request.inviter_name = self.member_name(&hosted, administrator);
            }
            if let Some(entry) = wait_list_entry
                && !hosted.wait_list.has(entry, now)
            {
                return Err(CollaborationError::new(
                    "that address is no longer on the wait list",
                ));
            }
            let transport = match at {
                InvitedAt::Here(address) => self.invitation_hint(&hosted, &address)?,
                InvitedAt::There(hint) => *hint,
            };
            let issued = self.stage_issue_through(
                &mut hosted,
                administrator,
                (request, makes_administrator),
                vec![transport],
                now,
            )?;
            if let Some(entry) = wait_list_entry {
                hosted
                    .wait_list
                    .invited(entry, issued.invite_id, now)
                    .map_err(CollaborationError::new)?;
            }
            issued
        };
        self.written(workspace, false).await?;
        Ok(issued)
    }

    /// The invitation a member's own device enters `workspace` with: for its
    /// own person, at their role, an Editor's at most, and asking for no grant
    /// to invite. It names only their own person and hands them nothing the
    /// member list does not, so a Viewer or Commenter entering is answered the
    /// same as an Editor.
    ///
    /// # Errors
    ///
    /// The workspace is not hosted here, its policy does not read, its device
    /// is no member, or the invitation cannot be issued.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the workspace stays locked from reading the role through issuing the invitation"
    )]
    pub(super) async fn own_invitation(
        &self,
        workspace: &str,
        device: &DeviceCert,
        (request_id, expires_at_ns): ([u8; 32], u64),
        address: &iroh_base::EndpointAddr,
    ) -> Result<IssuedInvitation, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        let issued = {
            let hosted = self.workspace(workspace).await?;
            let mut hosted = hosted.lock().await;
            let person = hex::encode(device.user_public_key);
            let policy = hosted
                .peer()
                .governance()
                .member_policy()
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
                inviter_name: self.member_name(&hosted, device),
            };
            self.stage_issue(&mut hosted, device, request, false, address, now)?
        };
        self.written(workspace, false).await?;
        Ok(issued)
    }

    /// The name the hosted workspace shows for the person `device` belongs
    /// to, where they chose one an invitation can carry.
    fn member_name(&self, hosted: &HostedWorkspace, device: &DeviceCert) -> Option<String> {
        self.member_name_by_root(hosted, device.user_public_key)
    }

    /// The name the hosted workspace shows for the person whose root is
    /// `root`, where they chose one an invitation can carry.
    pub(super) fn member_name_by_root(
        &self,
        hosted: &HostedWorkspace,
        root: [u8; 32],
    ) -> Option<String> {
        self.people(hosted)
            .members
            .into_iter()
            .find(|member| member.root() == Some(root) && !member.removed)
            .and_then(|member| carried(&member.name))
    }

    /// Record an invitation `request` asks of `workspace`, unwritten, naming this device at
    /// `address` ([`Self::invitation_addr`]).
    pub(super) fn stage_issue(
        &self,
        workspace: &mut HostedWorkspace,
        administrator: &DeviceCert,
        request: IssueInvitationRequest,
        makes_administrator: bool,
        address: &iroh_base::EndpointAddr,
        now: u64,
    ) -> Result<IssuedInvitation, CollaborationError> {
        let transport = vec![self.invitation_hint(workspace, address)?];
        self.stage_issue_through(
            workspace,
            administrator,
            (request, makes_administrator),
            transport,
            now,
        )
    }

    /// [`Self::stage_issue`], naming `transport` as where the workspace answers whoever redeems
    /// it.
    fn stage_issue_through(
        &self,
        workspace: &mut HostedWorkspace,
        administrator: &DeviceCert,
        (request, makes_administrator): (IssueInvitationRequest, bool),
        transport: Vec<tilo_peer_state::join::TransportHint>,
        now: u64,
    ) -> Result<IssuedInvitation, CollaborationError> {
        let role = request.role;
        let mut log = workspace.collaboration().clone();
        let issued = log.issue_as(
            &workspace.hosted(self.device_cert()),
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

    /// Have `device`, a further device of a person `workspace` has in it, list itself there
    /// under them: it signs its own row with `signer` on the member list held here
    /// ([`tilo_peer_state::own_admission::sign_further_device_row`]), and the workspace takes
    /// the row in as every member does ([`Self::take_own_row`]); this host signs nothing.
    /// Answers the row's changes: none when the device was listed already and signed nothing.
    ///
    /// A device the workspace met and took off its member list enters no more by asking: it is
    /// told it was removed, and comes back only where the workspace is handed to it again, by
    /// a fresh ticket or an invitation.
    ///
    /// # Errors
    ///
    /// The workspace is not held here, removed the device or keeps it as a revoked key, does not
    /// let the device in, or `signer` does not sign.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the workspace stays locked until the device's row is committed and settled"
    )]
    pub async fn enter(
        &self,
        workspace: &str,
        (device, signer): (&DeviceCert, &dyn collab_types::signer::DeviceSigner),
    ) -> Result<Vec<Vec<u8>>, CollaborationError> {
        use tilo_peer_state::join_log::GovernanceMessages as _;
        let now = tilo_clock::unix_now_ns();
        self.inner
            .host
            .environments
            .refuse_joining(&device.device_public_key, now)?;
        let held = self.workspace(workspace).await?;
        let mut hosted = held.lock().await;
        // A key whose grant this workspace records as revoked never enters it
        // again, whichever API server it comes through.
        refuse_revoked(&hosted, device)?;
        if hosted.peer().governance().removed_from_member_list(device) {
            return Err(CollaborationError::new(
                tilo_peer_state::REMOVED_DEVICE_REFUSED,
            ));
        }
        super::listing::refuse_unlisted_person(&hosted, device).map_err(CollaborationError::new)?;
        // What the device signed into the workspace before this copy had it (its session's
        // redemption, sent to the host that handed the copy over) comes first.
        let _turn =
            tilo_peer_state::own_admission::signing_turn(workspace, device.device_public_key).await;
        let own = self.own_admissions(hosted.workspace_id(), &device.device_public_key);
        let governance = hosted
            .peer()
            .governance_messages()
            .map_err(CollaborationError::new)?;
        let row = tilo_peer_state::own_admission::sign_further_device_row(
            workspace,
            (&governance, &own),
            device,
            signer,
            system_time(now),
        )
        .await
        .map_err(CollaborationError::new)?;
        if !row.is_empty() {
            self.take_own_row(&mut hosted, device, &row, now)
                .await
                .map_err(CollaborationError::new)?;
        } else if !self
            .decides_for(hosted.workspace_id(), device.device_public_key)
            .await
        {
            // Listed already, but an earlier entry can have been cut off between
            // committing its row and settling it: settle what governance holds
            // before answering, so the device's changes are decided against it.
            self.settle(&mut hosted).await?;
        }
        Ok(row)
    }

    /// Take `device`, a device `workspace` removed, back onto its member list, signed by this
    /// device, which keeps its person's root: `servers add` naming a server the workspace took
    /// off is the deliberate admission that brings it back, and only that root makes it. The
    /// change carries the root's mark of this device ([`super::person_workspace::devices::root_mark`])
    /// for every receiver to check. A device the workspace lists, or never met, changes nothing.
    ///
    /// # Errors
    ///
    /// The workspace is not held here, it removed `device` and this device does not keep its
    /// person's root, or the workspace refused the change.
    pub(super) async fn readmit_removed(
        &self,
        workspace: &str,
        device: &DeviceCert,
    ) -> Result<(), String> {
        let removed = {
            let held = self
                .workspace(workspace)
                .await
                .map_err(|error| error.to_string())?;
            let governed = held.lock().await;
            governed
                .peer()
                .governance()
                .removed_from_member_list(device)
        };
        if !removed {
            return Ok(());
        }
        let Some(root) =
            super::person_workspace::devices::root_mark(self, &self.inner.host.cert).await
        else {
            return Err(
                "The workspace removed this server. Only the device that keeps your \
                        identity adds it back."
                    .into(),
            );
        };
        self.name_host(workspace, |governance, key, now| {
            governance.readmit_device(device, root, key, now)
        })
        .await
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
        workspace.keep(candidate, &self.inner.host.key, at, |_, _| Ok(state))?;
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
        let by = self.authorize_role_change(&workspace, administrator, &request, now)?;
        let key = self.inner.host.key.clone();
        let completion = self
            .administer_as(
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
                stamped(governance::SET_ROLE, actor_of(&by)),
                now,
            )
            .await;
        drop(workspace);
        completion
    }

    /// Make `change`, a page's role edit, to `workspace`'s members as `caller`, whom the
    /// workspace must allow it: one member's role ([`Self::set_member_role`]), or a custom
    /// role or its assignment ([`Self::edit_member_list`]).
    pub(super) async fn edit_roles(
        &self,
        workspace: &str,
        caller: &DeviceCert,
        request_id: [u8; 32],
        change: tilo_action_policy::Change,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        match change {
            tilo_action_policy::Change::SetRole { person, role } => {
                let user_public_key = hex::decode(&person)
                    .ok()
                    .and_then(|bytes| bytes.try_into().ok())
                    .ok_or_else(|| CollaborationError::new("the change names no person"))?;
                self.set_member_role(
                    workspace,
                    caller,
                    SetMemberRoleRequest {
                        request_id,
                        user_public_key,
                        role,
                    },
                )
                .await
            }
            change => {
                self.edit_member_list(workspace, caller, request_id, change)
                    .await
            }
        }
    }

    /// Make `change`, a custom role's, to `workspace`'s member list as `caller`, whom the
    /// workspace must allow it.
    pub(super) async fn edit_member_list(
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
        let by = self.authorize(caller, Some(&workspace), WorkspaceOp::Change(&change), now)?;
        let key = self.inner.host.key.clone();
        // Giving a member a role of the workspace's own changes that member; defining a role
        // changes nobody.
        let stamp = matches!(
            change,
            tilo_action_policy::Change::SetMemberCustomRole { .. }
        )
        .then(|| stamped(governance::SET_ROLE, actor_of(&by)))
        .flatten();
        let completion = self
            .administer_as(
                &mut workspace,
                request_id,
                request_digest(&("custom-role", &change))?,
                |peer| {
                    peer.governance_mut()
                        .change_member_list(change, &key, system_time(now))
                },
                stamp,
                now,
            )
            .await;
        drop(workspace);
        completion
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
        let policy = workspace.peer().governance().member_policy()?;
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
                    .put_member_grants(&grants, &key, system_time(now))
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
        let completion = self
            .administer(
                &mut workspace,
                request_id,
                request_digest(&("workspace.grants.remove", &ids))?,
                |peer| {
                    peer.governance_mut()
                        .remove_member_grants(&ids, &key, system_time(now))
                },
                now,
            )
            .await?;
        drop(workspace);
        // Taking back someone's administrator grant unmakes them an administrator here, so the
        // server controls it let them reach go with it, unless another served workspace keeps
        // them an administrator.
        for id in &ids {
            if let Some(person) = tilo_action_policy::roles::administrator_named(id)
                && let Err(error) = self.release_administrator_controls(person).await
            {
                tracing::warn!(%error, "an unmade administrator's server controls were not taken back");
            }
        }
        Ok(completion)
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
        let person = hex::encode(self.inner.host.cert.user_public_key);
        let stop = tilo_action_policy::agent_answers::stop_grant(workspace, agent, &person);
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
        let person = hex::encode(self.inner.host.cert.user_public_key);
        let revoke = tilo_action_policy::Change::Revoke(
            tilo_action_policy::agent_answers::stop_grant_id(agent, &person),
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
        let named = workspace.to_owned();
        let workspace = self.workspace(workspace).await?;
        let mut workspace = workspace.lock().await;
        let by = self.authorize(
            administrator,
            Some(&workspace),
            WorkspaceOp::MakeAdministrator,
            now,
        )?;
        let key = self.inner.host.key.clone();
        // Someone new to the workspace is added; a member is given a new role.
        let verb = if workspace
            .peer()
            .governance()
            .member_policy()
            .map_err(CollaborationError::new)?
            .members
            .contains_key(&hex::encode(user_public_key))
        {
            governance::SET_ROLE
        } else {
            governance::ADD_MEMBER
        };
        let completion = self
            .administer_as(
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
                stamped(verb, actor_of(&by)),
                now,
            )
            .await?;
        drop(workspace);
        // The new administrator reaches the machines of every server serving the workspace, by
        // a grant naming them there.
        if let Err(error) = self.refresh_server_controls(&named).await {
            tracing::warn!(%error, "a new administrator was not granted the served machines' controls");
        }
        Ok(completion)
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
            // A member who left the workspace is no administrator of it, and their server
            // controls go with that place.
            if let Err(error) = self
                .release_administrator_controls(&hex::encode(person))
                .await
            {
                tracing::warn!(%error, "a removed administrator's server controls were not taken back");
            }
        }
        done
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
    /// user administers its workspaces, and any other member by the
    /// workspace's grants on the action (the Moderator role has them).
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
        // What a watcher of the workspace's members reads the change as: a ban
        // is no removal and an unban no admission.
        let stamp = match membership {
            Membership::Remove => stamped(governance::REMOVE_MEMBER, actor_of(&by)),
            Membership::Ban => stamped(governance::BAN, actor_of(&by)),
            Membership::Unban => stamped(governance::UNBAN, actor_of(&by)),
            Membership::Restrict(_) | Membership::Unrestrict => None,
        };
        let completion = self
            .administer_as(
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
                    Membership::Restrict(until_ms) => peer.governance_mut().put_member_grants(
                        &[tilo_action_policy::roles::restriction_grant(
                            &identity,
                            &person,
                            until_ms,
                            tilo_action_policy::Grantee::person(by.clone()),
                        )],
                        &key,
                        system_time(now),
                    ),
                    Membership::Unrestrict => peer.governance_mut().remove_member_grants(
                        &[tilo_action_policy::roles::restriction_grant_id(&person)],
                        &key,
                        system_time(now),
                    ),
                },
                stamp,
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

    /// Commit `command`, a membership change this host's own device makes, in
    /// `workspace` as its host, with `receipt`, what the workspace's history
    /// says of it, in the same change. The host writes the workspace's history,
    /// so its history has one writer, and the device's own replica only ever
    /// receives what is written here. The workspace's policy decides the change
    /// against the host's grants, as it decides any member's.
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
        receipt: Option<&tilo_actions::receipt::WorkspaceEffectReceipt>,
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
                    generation: workspace.peer().governance().generation(),
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
        workspace.keep(changed, &key, system_time(now), |record, _| {
            let mut state = record.collaboration.clone();
            state.withdraw_invitations(&removed);
            Ok(state)
        })?;
        let generation = workspace.peer().governance().generation();
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
    /// change against `device`'s grants as any member's write; the host's
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
        let (answer, changed) = workspace.change(
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
        let peer = self.state(workspace).await.ok_or_else(not_held)?;
        Ok(peer
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
        let peer = self.state(workspace).await.ok_or_else(not_held)?;
        Ok(peer.workspace().done(lease).map(|done| done.answer))
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
        let peer = self.state(workspace).await.ok_or_else(not_held)?;
        Ok(peer
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
    /// lets install apps. The choice is a workspace change every member
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
                "only an install or uninstall changes an app version",
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
                "only a trust list change decides the packages a workspace trusts",
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
                "only a registry change decides where a workspace fetches apps",
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
        let trust = (
            peer.governance().trusted_attestors(),
            peer.governance().revoked_publishers(),
        );
        drop(peer);
        drop(locked);
        Ok(trust)
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
    /// must grant `call`: a workspace change every member's live session follows the call to.
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
        workspace: &mut HostedWorkspace,
        request_id: [u8; 32],
        digest: [u8; 32],
        change: impl FnOnce(&mut tilo_peer_state::StatePeer) -> Result<bool, String>,
        now: u64,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        self.administer_as(workspace, request_id, digest, change, None, now)
            .await
    }

    /// [`Self::administer`], the governance changes `change` makes named as
    /// the action `stamp` names, by whom it names: what a watcher of the
    /// workspace's members reads a ban, a removal and an unban apart by. The
    /// change is otherwise the same, in the same order and admitted the same.
    pub(super) async fn administer_as(
        &self,
        workspace: &mut HostedWorkspace,
        request_id: [u8; 32],
        digest: [u8; 32],
        change: impl FnOnce(&mut tilo_peer_state::StatePeer) -> Result<bool, String>,
        stamp: Option<ActionStamp>,
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
            if let Some(stamp) = stamp {
                candidate.stamp_action(stamp.verb, stamp.by);
            }
            change(&mut candidate)?;
            // The action names the change that moves the rows, and no commit
            // after it: a base made, a name dropped or a grant reconciled with
            // it is bookkeeping the change itself is not.
            candidate.clear_action_stamp();
            let generation = candidate.governance().generation();
            let mut state = workspace.collaboration().clone();
            state.record_administration(request_id, digest, generation);
            let kept = workspace.keep_held(
                &mut replica,
                candidate,
                (&self.inner.host.key, system_time(now)),
                |_, _| Ok(state),
            );
            drop(replica);
            kept?;
            generation
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

/// A workspace of this server while the host decides, under its lock: whether
/// its log changed unwritten, and whether a governance change was
/// committed that its members have not been sent yet.
pub(super) struct HeldWorkspace<'a> {
    key: &'a SigningKey,
    /// The device answering this workspace's joins: this server's own, which
    /// need not be the device that hosts the workspace.
    answering: &'a DeviceCert,
    workspace: &'a mut HostedWorkspace,
    /// The workspace's state, held for the decision: a member's write reaches
    /// the replica over the link it syncs on, without this workspace's lock.
    replica: &'a mut tilo_peer_state::StatePeer,
    staged: bool,
    pub(super) committed: bool,
    /// The rows devices signed for themselves that the commit took in, by device, for the
    /// settle to carry ([`tilo_peer_state::join_answer::WorkspaceTxn::carry_own_row`]).
    own_rows: Vec<([u8; 32], Vec<Vec<u8>>)>,
}

impl<'a> HeldWorkspace<'a> {
    pub(super) const fn new(
        key: &'a SigningKey,
        answering: &'a DeviceCert,
        workspace: &'a mut HostedWorkspace,
        replica: &'a mut tilo_peer_state::StatePeer,
    ) -> Self {
        Self {
            key,
            answering,
            workspace,
            replica,
            staged: false,
            committed: false,
            own_rows: Vec::new(),
        }
    }

    /// Keep `peer`, with what it keeps along written in it, and `log`'s
    /// answer as the workspace's join log.
    fn keep(
        &mut self,
        peer: tilo_peer_state::StatePeer,
        now: u64,
        log: &mut dyn FnMut(&Listed<'_>) -> Result<CollaborationState, CollaborationError>,
    ) -> Result<(), CollaborationError> {
        self.workspace.keep_held(
            self.replica,
            peer,
            (self.key, system_time(now)),
            |record, peer| {
                log(&hosted(record, peer.into(), self.answering)).map_err(|error| error.to_string())
            },
        )?;
        // The commit wrote the whole record, the log staged before
        // included.
        self.staged = false;
        self.committed = true;
        Ok(())
    }
}

impl WorkspaceTxn for HeldWorkspace<'_> {
    fn workspace(&self) -> &str {
        self.workspace.workspace()
    }

    fn hosted(&self) -> Listed<'_> {
        self.workspace.logged_from(self.replica, self.answering)
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
        log: &mut dyn FnMut(&Listed<'_>) -> Result<CollaborationState, CollaborationError>,
    ) -> Result<(), CollaborationError> {
        self.keep(peer, now, log)
    }

    fn commit_admission(
        &mut self,
        peer: tilo_peer_state::StatePeer,
        now: u64,
        log: &mut dyn FnMut(&Listed<'_>) -> Result<CollaborationState, CollaborationError>,
    ) -> Result<(), CollaborationError> {
        self.keep(peer, now, log)
    }

    fn carry_own_row(&mut self, device: [u8; 32], changes: Vec<Vec<u8>>) {
        self.own_rows.push((device, changes));
    }
}

/// This server's workspaces, to the host that answers their workspace-join
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
        let (answer, staged, committed, own_rows) = {
            // A member's write reaches this workspace's replica over the link it
            // syncs on, without this workspace's lock: holding the replica for
            // the decision is what keeps the state a change is built from the
            // one it is committed over.
            let shared = locked.peer.shared();
            let mut replica = shared.lock();
            let mut held = HeldWorkspace::new(
                &self.inner.host.key,
                self.device_cert(),
                &mut locked,
                &mut replica,
            );
            let answer = decide(&mut held);
            (answer, held.staged, held.committed, held.own_rows)
        };
        if committed {
            for (device, changes) in own_rows {
                self.carry_own_admission(&locked, device, changes)?;
            }
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
            self.admit_answered(
                remote_endpoint,
                request.proof.clone(),
                request.admission.clone(),
                now,
            )
            .await,
        )
    }

    async fn readmit_invited(
        &self,
        workspace: &str,
        device: &DeviceCert,
        signed_invitation: &[u8],
        now: u64,
    ) -> Result<(), CollaborationError> {
        let issuer = tilo_peer_state::join_log::invitation_issuer(signed_invitation, now)?;
        // A device that did not issue the invitation, or keeps no root, takes no removed device
        // back: the challenge then refuses it as removed.
        if issuer != self.inner.host.cert.device_public_key
            || super::person_workspace::devices::root_mark(self, &self.inner.host.cert)
                .await
                .is_none()
        {
            return Ok(());
        }
        self.readmit_removed(workspace, device)
            .await
            .map_err(CollaborationError::new)
    }
}

impl HostedWorkspace {
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
    workspace: &HostedWorkspace,
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
pub(super) fn revoked_keys(workspace: &HostedWorkspace) -> Vec<[u8; 32]> {
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
