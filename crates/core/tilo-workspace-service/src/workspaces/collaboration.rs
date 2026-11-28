//! Invitations, their redemption and member administration of a workspace this
//! server hosts.
//!
//! What a host records between the steps of an exchange is the shared
//! [`JoinLog`], the same on every device that hosts workspaces; this module
//! owns who may ask what of this server's workspaces, and the durability barrier
//! before each answer.

use collab_types::signer::DeviceSigner;
use collab_types::workspace_grant::WorkspaceGrant;
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use tilo_peer_state::join::{JoinBootstrap, JoinCompleteRequest};
use tilo_peer_state::join_answer::{JoinWorkspaces, WorkspaceTxn};
use tilo_peer_state::join_log::{Administered, Listed, Signer};
pub use tilo_peer_state::join_log::{JoinError as CollaborationError, Redeeming};
use tilo_server_protocol::workspaces::{
    CancelInvitationRequest, IssueInvitationRequest, IssuedInvitation, PendingInvitation,
};
use tilo_user_identity::DeviceCert;

use tilo_action_policy::Role;
use tilo_action_policy::roles::governance;
use tilo_peer_state::workspace_membership::{SettingsChange, WorkspaceMembershipCommand};

use super::authority::WorkspaceOp;
use super::{HostedWorkspace, WorkspaceDocumentEntry, Workspaces, system_time};
use tilo_lock::MutexRecover as _;

/// Why a read of a workspace's own state is refused: this device has no copy of it.
pub(super) fn not_here() -> CollaborationError {
    CollaborationError::new("this device has no copy of that workspace and hasn't joined it")
}

/// Who asks [`Workspaces::govern`] for a change, under which request, and the changes their
/// device signed for it, once it has.
pub struct SignedAsk<'a> {
    pub device: &'a DeviceCert,
    pub request_id: [u8; 32],
    pub signed: Option<Vec<Vec<u8>>>,
}

impl<'a> SignedAsk<'a> {
    /// `device` asking under `request_id`, with nothing signed yet.
    pub const fn here(device: &'a DeviceCert, request_id: [u8; 32]) -> Self {
        Self {
            device,
            request_id,
            signed: None,
        }
    }
}

/// What a change asked of this host came to: `T`, or what the asking device signs the change on
/// ([`Workspaces::govern`]), nothing written yet.
#[derive(Debug)]
pub enum Signing<T> {
    Done(T),
    Sign(tilo_actions::remote::SignFacts),
}

/// What [`Workspaces::govern`] came to.
pub type DeviceChange = Signing<GovernanceCompletion>;

impl<T> Signing<T> {
    /// What the change came to: a device that is not this host's own signs it through the
    /// workspace's actions, which answer what it signs on.
    ///
    /// # Errors
    /// The change is another device's to sign.
    pub fn done(self) -> Result<T, CollaborationError> {
        match self {
            Self::Done(done) => Ok(done),
            Self::Sign(_) => Err(CollaborationError::new(
                "the change is the asking device's to sign, through the workspace's actions",
            )),
        }
    }

    /// What `answer` makes of the change once it is made.
    pub fn map<U>(self, answer: impl FnOnce(T) -> U) -> Signing<U> {
        match self {
            Self::Done(done) => Signing::Done(answer(done)),
            Self::Sign(facts) => Signing::Sign(facts),
        }
    }
}

/// What `ask` comes to as `device` signs it with `signer`, in one call: asked first, a host that
/// is not that device answers what to sign on; the device signs the change it asked for on it
/// ([`tilo_peer_state::asked_signing::signed_on`]) and asks again with that signature. What a
/// device holding its own key in this process does; the first ask writes nothing.
///
/// # Errors
///
/// As `ask`, or the device could not sign the change the host named.
pub async fn signed_through<'d, T, Asked>(
    (device, signer): (&'d DeviceCert, &dyn DeviceSigner),
    request_id: [u8; 32],
    ask: impl Fn(SignedAsk<'d>) -> Asked,
) -> Result<T, CollaborationError>
where
    Asked: std::future::Future<Output = Result<Signing<T>, CollaborationError>>,
{
    let facts = match ask(SignedAsk::here(device, request_id)).await? {
        Signing::Done(done) => return Ok(done),
        Signing::Sign(facts) => facts,
    };
    let command = serde_json::from_value(facts.change.clone())
        .map_err(|error| CollaborationError::new(error.to_string()))?;
    let changes = tilo_peer_state::asked_signing::signed_on(
        &facts,
        &command,
        (device, signer),
        system_time(tilo_clock::unix_now_ns()),
    )
    .await?;
    ask(SignedAsk {
        device,
        request_id,
        signed: Some(changes),
    })
    .await?
    .done()
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
        self.issue_as(workspace, administrator, request, false)
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
        self.issue_from(
            workspace,
            administrator,
            request,
            makes_administrator,
            None,
            InvitedAt::Here(self.invitation_addr().await),
        )
        .await
    }

    /// [`Self::issue_as`] of an invitation issued for the wait-list entry `wait_list_entry`, where
    /// one is named: the entry records the invitation, so what it mails carries this link, and it
    /// leaves the list once someone joins with it.
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
    /// change carries the root's mark of this device ([`Workspaces::root_mark`])
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
        let Some(root) = self.root_mark() else {
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
        let peer = self.state(workspace).await.ok_or_else(not_here)?;
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
        let peer = self.state(workspace).await.ok_or_else(not_here)?;
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
        let peer = self.state(workspace).await.ok_or_else(not_here)?;
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
    /// them now ([`Self::registries_read`]).
    pub async fn registries(
        &self,
        workspace: &str,
    ) -> Result<Vec<tilo_peer_state::PackageRegistry>, CollaborationError> {
        let recorded = self
            .workspace(workspace)
            .await?
            .lock()
            .await
            .peer()
            .governance()
            .package_registries();
        Ok(Self::registries_read(recorded))
    }

    /// The registries a workspace that records `recorded` fetches apps from. A workspace that
    /// names none (one made where no registry was seeded, or moved from such a device) starts
    /// from the official store, as a device that names none does: otherwise nothing it runs
    /// could ever be updated. A workspace that wants no store keeps the row, disabled.
    pub(crate) fn registries_read(
        mut recorded: Vec<tilo_peer_state::PackageRegistry>,
    ) -> Vec<tilo_peer_state::PackageRegistry> {
        if recorded.is_empty() {
            return super::default_apps::registry_rows(&crate::service::RegistriesFile::seeded());
        }
        Self::repair_official_pin(&mut recorded);
        recorded
    }

    /// Restore the official registry's pin on its row when the row carries none (an emptied or
    /// placeholder key): the image's official key is what a new workspace records, so a row that
    /// lost the pin reads as that, never as trust in nobody. The row stays the workspace's to
    /// change; only the reading is whole.
    fn repair_official_pin(rows: &mut [tilo_peer_state::PackageRegistry]) {
        let seeded = crate::service::RegistriesFile::seeded();
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

    /// Make `command` in `workspace` as `ask.device`, whom the workspace must allow what the
    /// command asks ([`asked_check`]): a change the device signs itself, never this host.
    /// Without `ask.signed`, nothing is written and the answer is what the device signs it on
    /// ([`DeviceChange::Sign`]); with it, those changes are taken in as the device's own, judged
    /// by the device's place in the workspace as every copy judges them. A round trip refused or
    /// never finished leaves the workspace as it was.
    ///
    /// What this host keeps of a change goes with it, under the same lock: a person it removes
    /// takes every way in the workspace handed them with them, and one it admits or unbans is no
    /// longer kept out. After it, a removed administrator's server controls go, a new one's come,
    /// a directory hears of changed settings and a moderated call's rule is read again.
    ///
    /// A device of this host's own signs here at once, as any device of the person's does.
    ///
    /// # Errors
    /// The workspace does not allow the change of the device, or refuses what it signed.
    pub async fn govern(
        &self,
        workspace: &str,
        ask: SignedAsk<'_>,
        command: &WorkspaceMembershipCommand,
    ) -> Result<DeviceChange, CollaborationError> {
        command.validate()?;
        // A key its person's root revoked signs nothing here, in a workspace whose member list
        // carries the revocation as in one this host only keeps.
        if let Some(super::Refusal::Revoked { reason }) =
            self.revoked(&tilo_actions::remote::Caller::authenticated(
                ask.device.user_public_key,
                ask.device.device_public_key,
            ))
        {
            return Err(CollaborationError::new(reason));
        }
        let now = tilo_clock::unix_now_ns();
        let held = self.workspace(workspace).await?;
        let mut hosted = held.lock().await;
        if let Some(check) = asked_check(command) {
            self.authorize_signed(ask.device, &hosted, check, now)?;
        }
        let signed = if let Some(signed) = ask.signed {
            signed
        } else {
            let facts = Self::sign_facts(&hosted, command)?;
            if ask.device.device_public_key != self.inner.host.cert.device_public_key {
                return Ok(DeviceChange::Sign(facts));
            }
            drop(hosted);
            let signed = self.sign_here(&facts, command).await?;
            hosted = held.lock().await;
            signed
        };
        let members =
            |hosted: &HostedWorkspace| super::member_roots(&hosted.peer(), system_time(now));
        let before = members(&hosted);
        // A signed change is taken in, not written by this host's own commit: it reaches the
        // workspace's devices only as this host passes it on. It goes under this host's name,
        // since the device that signed it asked for it through an action, not its link, and its
        // own copy takes it from here as every other does.
        let carried = signed.clone();
        let completion = self
            .administer(
                &mut hosted,
                ask.request_id,
                request_digest(&("signed", command))?,
                |peer| peer.take_signed(ask.device, signed, system_time(now)),
                now,
            )
            .await?;
        self.carry_signed(hosted.workspace_id(), &self.inner.host.cert, carried)
            .map_err(CollaborationError::new)?;
        let removed: std::collections::BTreeSet<_> =
            before.difference(&members(&hosted)).copied().collect();
        if !removed.is_empty() {
            let mut state = hosted.collaboration().clone();
            state.withdraw_invitations(&removed);
            hosted.commit_state(state)?;
            hosted
                .keep_out_removed(&removed, Some(ask.device.user_public_key), now)
                .map_err(CollaborationError::new)?;
        }
        // Holding a person again is a deliberate admission, and lifts a plain removal at once; a
        // ban's block stays on the policy until it is lifted, which lifts this too.
        let admitted = match command {
            WorkspaceMembershipCommand::Place { person, .. } => Some(*person),
            WorkspaceMembershipCommand::Member {
                change: tilo_action_policy::Change::Unban { person },
            } => {
                let mut key = [0; 32];
                hex::decode_to_slice(person, &mut key)
                    .map_err(|_| CollaborationError::new("an unban names a person's key"))?;
                Some(key)
            }
            _ => None,
        };
        if let Some(person) = admitted {
            hosted
                .removals
                .clear(person)
                .map_err(CollaborationError::new)?;
        }
        drop(hosted);
        Box::pin(self.after_change(workspace, command, &removed)).await;
        Ok(DeviceChange::Done(completion))
    }

    /// What follows `command` once `workspace` took it, outside its lock: `removed` leave the
    /// people's lists of it and their server controls, a new administrator reaches the served
    /// machines, a directory hears of the settings, and a call's live origin reads its rule again.
    async fn after_change(
        &self,
        workspace: &str,
        command: &WorkspaceMembershipCommand,
        removed: &std::collections::BTreeSet<[u8; 32]>,
    ) {
        let mut unmade: Vec<String> = Vec::new();
        for person in removed {
            self.unlist_for(person, workspace).await;
            unmade.push(hex::encode(person));
        }
        match command {
            WorkspaceMembershipCommand::Place {
                place: MemberPlace::Administrator,
                ..
            } => {
                if let Err(error) = self.refresh_server_controls(workspace).await {
                    tracing::warn!(%error, "a new administrator was not granted the served machines' controls");
                }
            }
            WorkspaceMembershipCommand::Revoke { grant } => {
                unmade.extend(
                    tilo_action_policy::roles::administrator_named(grant).map(str::to_owned),
                );
            }
            WorkspaceMembershipCommand::Settings(_) => self.inner.directory.announce(workspace),
            WorkspaceMembershipCommand::Call { channel, .. } => {
                crate::authority_epoch::bump();
                self.found_call_keys(workspace, channel);
            }
            _ => {}
        }
        for person in unmade {
            if let Err(error) = self.release_administrator_controls(&person).await {
                tracing::warn!(%error, "an unmade administrator's server controls were not taken back");
            }
        }
    }

    /// Make `command` in `workspace` as `device` under `request_id` ([`Self::govern`]): a change
    /// this host's own device signs here at once; another device's is refused, since it signs its
    /// own through the workspace's actions.
    ///
    /// # Errors
    /// As [`Self::govern`], or `device` is not this host's own.
    pub async fn change(
        &self,
        workspace: &str,
        (device, request_id): (&DeviceCert, [u8; 32]),
        command: impl Into<WorkspaceMembershipCommand>,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        self.govern(
            workspace,
            SignedAsk::here(device, request_id),
            &command.into(),
        )
        .await?
        .done()
    }

    /// What a device signs `command` on in `hosted`: the workspace's member list, and the change.
    fn sign_facts(
        hosted: &HostedWorkspace,
        command: &WorkspaceMembershipCommand,
    ) -> Result<tilo_actions::remote::SignFacts, CollaborationError> {
        use tilo_peer_state::join_log::GovernanceMessages as _;
        Ok(tilo_actions::remote::SignFacts {
            workspace: hosted.workspace_id().to_string(),
            governance: hosted
                .peer()
                .governance_messages()?
                .iter()
                .map(hex::encode)
                .collect(),
            change: serde_json::to_value(command)
                .map_err(|error| CollaborationError::new(error.to_string()))?,
        })
    }

    /// `command` signed on `facts` by this host's own device, as any device of its person's signs
    /// it.
    async fn sign_here(
        &self,
        facts: &tilo_actions::remote::SignFacts,
        command: &WorkspaceMembershipCommand,
    ) -> Result<Vec<Vec<u8>>, CollaborationError> {
        Ok(tilo_peer_state::asked_signing::signed_on(
            facts,
            command,
            (&self.inner.host.cert, &self.inner.host.key),
            system_time(tilo_clock::unix_now_ns()),
        )
        .await?)
    }

    pub(super) async fn administer(
        &self,
        workspace: &mut HostedWorkspace,
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
        if issuer != self.inner.host.cert.device_public_key || !self.serves_at_root() {
            return Ok(());
        }
        self.readmit_removed(workspace, device)
            .await
            .map_err(CollaborationError::new)
    }

    fn entry_answered(
        &self,
        workspace: &str,
        device: &DeviceCert,
        request_id: [u8; 32],
        now: u64,
    ) -> bool {
        self.entry_answer_stands(workspace, device, request_id, now)
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

/// What the workspace must allow the device asking `command` of this host, before it is asked to
/// sign it: `None` for a change only its signer's own place decides, as every copy decides it.
pub(super) fn asked_check(command: &WorkspaceMembershipCommand) -> Option<WorkspaceOp<'_>> {
    use WorkspaceMembershipCommand as Command;
    Some(match command {
        Command::Member { change } => WorkspaceOp::Change(change),
        Command::Place { person, place, .. } => WorkspaceOp::Place(*person, *place),
        Command::InstallApp(_)
        | Command::UninstallApp { .. }
        | Command::AddAttestor(_)
        | Command::RemoveAttestor { .. }
        | Command::RevokePublisher { .. }
        | Command::AddRegistry(_)
        | Command::RemoveRegistry { .. }
        | Command::SetRegistryEnabled { .. }
        | Command::SetRegistryChannel { .. } => WorkspaceOp::Govern(governance::INSTALL_APP),
        Command::Call { .. } => WorkspaceOp::Govern(governance::CALL),
        Command::Settings(SettingsChange::Rename { .. }) => WorkspaceOp::Govern(governance::RENAME),
        Command::Settings(_) => WorkspaceOp::Govern(governance::SETTINGS),
        Command::Grant { .. } | Command::Reading { reads: true, .. } => {
            WorkspaceOp::Govern(governance::GRANT)
        }
        Command::Reading { reads: false, .. } => WorkspaceOp::Govern(governance::REVOKE),
        _ => return None,
    })
}
