//! The one answer to "may this device act in this workspace now?".
//!
//! Every server protocol that admits a device into a workspace — subscribe, the
//! access audit, state delivery, replay, remote actions, the browser live
//! protocol, content discovery, compute and environments — asks [`decide`] with
//! its [`WorkspaceOperation`]. The rule is written once:
//!
//! 1. The certificate is current server-wide ([`ServerTrust::cert`]): not
//!    expired, not revoked, admitted by the live trust policy.
//! 2. An agent device acts only inside its live delegation.
//! 3. A host this workspace is attached to is admitted on the protocols that carry
//!    its changes (subscribe, state, publish, replay), listed or not.
//! 4. A device the workspace's signed member list has is admitted
//!    ([`crate::workspace::WorkspaceRuntime::members`]).
//! 5. In a workspace served here, a device of a person the member list has
//!    ([`person_device`]: a browser tab, a new computer of theirs) is enrolled
//!    when it subscribes: the list records it under its person, as the entry
//!    does for a further device of a member, so it holds the part its person
//!    holds. Membership follows the person, not one device key.
//! 6. Anything else is not a member.
//!
//! An agent's grant (a key the person certified for a service that acts for
//! an agent they approved, [`tilo_user_identity::AgentScope`]) is its
//! person's agent, and its rule replaces steps 3 to 6: after step 1, it is
//! admitted iff
//!
//! - its scope reaches this workspace (all of the person's, or exactly this
//!   one: a one-workspace grant, root-certified or minted, reaches no other
//!   and never the site's home workspace);
//! - the member list has it: it redeemed an invitation for its person, as any
//!   new device of theirs does, so the list holds it under its person;
//! - its person is still a member;
//! - and the protocol is one an agent works on: subscribe, state, publish,
//!   replay, channel access, remote actions, the live session and content
//!   discovery. Never compute, environments or membership as an agent, and
//!   never by attachment or a host's enrolment.
//!
//! What it does there is its person's agent's ([`crate::peer`] makes its
//! caller the agent, for its person): an agent's role cap holds it at
//! Editor at most, and it governs and administers nothing; only a person's
//! server key invites, for its person, as an Editor at most
//! (`workspaces::authority`). Its certificate's expiry and its person's
//! revocations (a minted grant's issuer included) are step 1, checked at
//! every handshake and audit.
//!
//! A hosted browser environment's device is certified as a member device of
//! the workspace's person, and is no member: a page that escapes Chromium
//! holds its key, so its rule replaces steps 4 to 6 as an agent's does. After
//! step 1, it is admitted iff the server registered it for this workspace
//! ([`crate::environment`]), on the protocols a window's session works on — and
//! its one record is what it reaches within them: the state protocol's replay,
//! live fan-out, opening and changes ask its scope what a frame's record is,
//! and the live session offers it no namespace but its own. A device whose
//! environment stopped is refused by step 1 on every protocol, as a revoked one
//! is, until the certificate it was given expires: the register says so before
//! anything is delisted, so no protocol admits it in between.
//!
//! A public workspace is no exception to the rule: whoever visits it is a
//! member of it with the role its owner gave them, and every device certified
//! by that member's root is that member's device, admitted to the channels its
//! records take part in like any other.
//!
//! And one protocol admits a device to a workspace's member list itself:
//! [`WorkspaceOperation::DelegateMembership`], a delegated agent joining a
//! workspace served here as a member. Agents are members: each has its own
//! device key, a certificate and a delegation its person's root signed, and
//! the member list records it bound to that delegation, so every member
//! admits its changes as its own, at its person's role and never above
//! Editor, only within the delegation, and only until it is withdrawn. The
//! rule, after steps 1 to 3: admit iff the workspace is served here and the
//! device is a registered agent whose live delegation (step 2) names this
//! workspace and the person the agent acts for. The agent's approver is any
//! device of that person, the host's own person included and never singled
//! out. A device that is no agent, and any workspace not served here, are
//! refused; nothing is admitted by membership or attachment. Being on the
//! member list is what admits the agent to the workspace on the other
//! protocols and lets the workspace's key holders admit it to the channels of
//! its records.

use std::sync::RwLock;

use tilo_user_identity::RevocationList;

use crate::auth::AuthenticatedPeer;
use crate::peer::AgentRegistry;
use crate::policy::TrustPolicy;
use crate::workspace::WorkspaceRuntime;
use tilo_lock::RwLockRecover as _;
use tilo_server_protocol::WorkspaceId;

/// The protocol asking. Protocols differ only in the three properties below.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkspaceOperation {
    /// A subscribe, and the periodic audit of a live subscription.
    Subscribe,
    /// Holding the workspace's state: being sent a state frame, answered a sync
    /// request, or keeping a source's frames flowing.
    State,
    /// Taking a device's own frames into the workspace: a change or commit,
    /// presence, record content, a compaction, or joining the live session.
    Publish,
    /// Replaying retained state to a subscriber.
    Replay,
    /// A browser page's session in the workspace's live session.
    Live,
    /// A device asking one of the workspace's channels for access: the protocol
    /// its own key package arrives on (see the module documentation). The
    /// channel's group admits the devices its record's readers name.
    KeyAccess,
    /// Discovering the workspace's content endpoints.
    ContentDiscovery,
    Compute,
    /// Opening or attaching to an environment in the workspace.
    Environment,
    /// Originating or receiving a remote (directed) action.
    RemoteAction,
    /// A delegated agent becoming a member of a workspace served here (see
    /// the module documentation).
    #[cfg(any(test, feature = "agents"))]
    DelegateMembership,
}

impl WorkspaceOperation {
    /// Whether this protocol carries a workspace's changes.
    const fn carries_changes(self) -> bool {
        matches!(
            self,
            Self::Subscribe | Self::State | Self::Publish | Self::Replay | Self::KeyAccess
        )
    }
}

/// The admission decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Admitted now.
    Admit,
    /// Admitted once the device is persisted as a member; a caller that
    /// cannot enrol treats it as not yet a member.
    AdmitAndEnrol,
    /// Refused.
    Deny(DenyReason),
}

/// Whether `user` is the person this host runs for, or a member of a workspace
/// it serves: who the host queues a device certificate for, beside the owner's
/// own devices.
pub async fn admits_person(
    workspaces: &crate::peer::WorkspaceRegistry,
    own_user: [u8; 32],
    user: [u8; 32],
) -> bool {
    if user == own_user {
        return true;
    }
    let served: Vec<_> = workspaces.lock().await.values().cloned().collect();
    for workspace in served {
        if workspace.lock().await.admits_person(&user) {
            return true;
        }
    }
    false
}

impl Decision {
    /// Admitted without any further step.
    pub fn admitted(self) -> bool {
        self == Self::Admit
    }
}

/// Why [`decide`] refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DenyReason {
    /// The certificate's expiry has passed.
    CertExpired,
    /// The device is on the revocation list.
    Revoked,
    /// The live trust policy no longer admits the certificate's user.
    NotTrusted,
    /// The agent was withdrawn.
    AgentWithdrawn,
    /// The agent's delegation expired.
    AgentExpired,
    /// The agent is not delegated this workspace.
    AgentOutOfScope,
    /// Not a member of the workspace.
    NotAMember,
    /// A hosted browser environment's device, outside the one workspace and
    /// the one record the server confines it to.
    OutOfEnvironmentScope,
    /// A hosted browser environment's device whose environment stopped: it
    /// reaches nothing, and no protocol admits it, until its certificate expires.
    EnvironmentStopped,
    /// Membership as an agent asked for by a device that is no delegated
    /// agent of its person, or for a workspace not served here.
    #[cfg(any(test, feature = "agents"))]
    NotADelegatedAgent,
}

impl DenyReason {
    /// The refusal a device is sent for it.
    #[must_use]
    pub fn refusal(self) -> tilo_server_protocol::refusal::Refusal {
        use tilo_server_protocol::refusal::Refusal;
        match self {
            Self::OutOfEnvironmentScope => Refusal::NotAllowed(self.reason().into()),
            Self::NotTrusted => Refusal::NotAdmitted,
            _ => Refusal::NotAMember(self.reason().into()),
        }
    }

    /// The refusal as text, for the log.
    pub const fn reason(self) -> &'static str {
        match self {
            Self::CertExpired => "device certificate expired",
            Self::Revoked => "authenticated device was revoked",
            Self::NotTrusted => "the member was removed from the workspace's signed authority",
            Self::AgentWithdrawn => "the agent was withdrawn",
            Self::AgentExpired => "the agent's delegation expired",
            Self::AgentOutOfScope => "the agent is not delegated this workspace",
            Self::NotAMember => "device not in workspace membership",
            Self::OutOfEnvironmentScope => {
                "a browser environment's device reaches one workspace and its own record"
            }
            Self::EnvironmentStopped => {
                "the browser environment this device belongs to has stopped"
            }
            #[cfg(any(test, feature = "agents"))]
            Self::NotADelegatedAgent => {
                "only a delegated agent joins a workspace it governs as an agent"
            }
        }
    }
}

/// Server-wide certificate validity and agent delegation: one copy.
#[derive(Clone, Copy)]
pub struct ServerTrust<'a> {
    pub revocations: &'a RwLock<RevocationList>,
    /// Live handshake trust policy.
    pub policy: &'a RwLock<TrustPolicy>,
    /// The user this server chains to.
    pub own_user: &'a [u8; 32],
    /// Agent delegations this server was told about.
    pub agents: &'a AgentRegistry,
    /// Hosted browser environments' devices, and the one record each reaches
    /// ([`crate::environment`]).
    pub environments: &'a crate::environment::EnvironmentRegistry,
}

impl ServerTrust<'_> {
    /// Whether `device` is an agent this server was told about, delegated or
    /// withdrawn. An agent is no person of its own: what a person alone may
    /// do (their compute, a workspace's administration) is never its to do. A
    /// poisoned registry counts every device as an agent, so it refuses rather
    /// than admits.
    pub fn is_agent(&self, device: &[u8; 32]) -> bool {
        !self
            .agents
            .read()
            .is_ok_and(|agents| !agents.contains_key(device))
    }

    /// Whether `peer`'s certificate is current server-wide at `now_ns`. A
    /// poisoned list or policy refuses.
    pub fn cert(&self, peer: &AuthenticatedPeer, now_ns: u64) -> Result<(), DenyReason> {
        if peer
            .cert
            .expires_at_ns
            .is_some_and(|expiry| expiry <= now_ns)
        {
            return Err(DenyReason::CertExpired);
        }
        // A device whose environment stopped is refused on every protocol, as a
        // revoked one is, until the certificate it was given expires: this is
        // the check every protocol and every audit makes.
        if matches!(
            self.environments.of(&peer.device_public_key, now_ns),
            Some(crate::environment::Environment::Stopped { .. })
        ) {
            return Err(DenyReason::EnvironmentStopped);
        }
        if !self
            .revocations
            .read()
            .is_ok_and(|list| !list.refuses(&peer.cert))
        {
            return Err(DenyReason::Revoked);
        }
        if !self
            .policy
            .read()
            .is_ok_and(|policy| policy.admits(&peer.cert, self.own_user))
        {
            return Err(DenyReason::NotTrusted);
        }
        Ok(())
    }

    /// Whether `device` may be in `workspace_id` as far as agent delegation
    /// is concerned. A device that is no agent passes.
    ///
    /// # Errors
    ///
    /// Why the agent may not act there.
    pub(crate) fn agent(
        &self,
        device: &[u8; 32],
        workspace_id: WorkspaceId,
        now_ns: u64,
    ) -> Result<(), DenyReason> {
        let agents = self.agents.read_recover();
        match agents.get(device) {
            None => Ok(()),
            Some(None) => Err(DenyReason::AgentWithdrawn),
            Some(Some(delegation)) if delegation.expires_at_ns <= now_ns => {
                Err(DenyReason::AgentExpired)
            }
            Some(Some(delegation)) if delegation.permits_workspace(workspace_id) => Ok(()),
            Some(Some(_)) => Err(DenyReason::AgentOutOfScope),
        }
    }

    /// The live delegation of agent `device` for `workspace_id` at `now_ns`:
    /// `None` for a device that is no agent, or whose delegation was
    /// withdrawn, expired or does not name the workspace.
    #[cfg(any(test, feature = "agents"))]
    pub(crate) fn delegation(
        &self,
        device: &[u8; 32],
        workspace_id: WorkspaceId,
        now_ns: u64,
    ) -> Option<tilo_server_protocol::delegation::Delegation> {
        self.agents
            .read_recover()
            .get(device)
            .cloned()
            .flatten()
            .filter(|delegation| {
                delegation.expires_at_ns > now_ns && delegation.permits_workspace(workspace_id)
            })
    }

    /// Certificate and agent delegation together: everything about `peer`
    /// that does not depend on the workspace's membership.
    ///
    /// # Errors
    ///
    /// The first reason `peer` may not act in `workspace_id`.
    pub(crate) fn actor(
        &self,
        peer: &AuthenticatedPeer,
        workspace_id: WorkspaceId,
        now_ns: u64,
    ) -> Result<(), DenyReason> {
        self.cert(peer, now_ns)?;
        self.agent(&peer.device_public_key, workspace_id, now_ns)
    }
}

/// Whether a subscribe enrols `peer`'s device in `workspace` (step 5): the
/// workspace is served here, its member list has the device's person, and the
/// device is one of theirs.
fn enrols_member_device(workspace: &WorkspaceRuntime, peer: &AuthenticatedPeer) -> bool {
    workspace.workspace_authority.is_some()
        && person_device(&peer.cert)
        && workspace.admits_person(&peer.user_public_key)
}

/// Whether the workspace lists `peer` for `op`: steps 3 and 4 of the rule.
fn listed(workspace: &WorkspaceRuntime, peer: &AuthenticatedPeer, op: WorkspaceOperation) -> bool {
    (op.carries_changes() && workspace.attached(&peer.device_public_key))
        || workspace.lists(&peer.device_public_key)
}

/// The one admission decision for every protocol, at the current time.
pub fn decide(
    workspace: &WorkspaceRuntime,
    trust: &ServerTrust<'_>,
    peer: &AuthenticatedPeer,
    op: WorkspaceOperation,
) -> Decision {
    decide_in(workspace, trust, peer, op, tilo_clock::unix_now_ns())
}

/// [`decide`] at `now_ns`.
pub(crate) fn decide_in(
    workspace: &WorkspaceRuntime,
    trust: &ServerTrust<'_>,
    peer: &AuthenticatedPeer,
    op: WorkspaceOperation,
    now_ns: u64,
) -> Decision {
    if let Err(reason) = trust.actor(peer, workspace.workspace_id(), now_ns) {
        return Decision::Deny(reason);
    }
    match crate::environment::environment_of(trust.environments, &peer.device_public_key) {
        Some(crate::environment::Environment::Placed(scope)) => {
            return environment_decision(workspace, scope, op);
        }
        Some(crate::environment::Environment::Stopped { .. }) => {
            return Decision::Deny(DenyReason::EnvironmentStopped);
        }
        None => {}
    }
    if let Some(scope) = peer.cert.capabilities.agent {
        return grant_decision(workspace, peer, scope, op);
    }
    #[cfg(any(test, feature = "agents"))]
    if op == WorkspaceOperation::DelegateMembership {
        // Never by membership, enrolment or attachment: only an agent inside
        // its live delegation, acting for its own person's root, in a
        // workspace served here.
        let delegated = trust
            .delegation(&peer.device_public_key, workspace.workspace_id(), now_ns)
            .is_some_and(|delegation| delegation.user_public_key == peer.user_public_key);
        return if delegated && workspace.workspace_authority.is_some() {
            Decision::Admit
        } else {
            Decision::Deny(DenyReason::NotADelegatedAgent)
        };
    }
    if listed(workspace, peer, op) {
        return Decision::Admit;
    }
    if op == WorkspaceOperation::Subscribe && enrols_member_device(workspace, peer) {
        return Decision::AdmitAndEnrol;
    }
    Decision::Deny(DenyReason::NotAMember)
}

/// Whether `cert` is a device of its person's own: certified by their root,
/// or by one of their devices or the stand-in their browser holds for it
/// ([`tilo_user_identity::DeviceCert::issuer`]). An agent's grant, or a key
/// one minted, acts for the person and is no device of theirs: it comes in by
/// the delegation that bounds it, never as the person. Nor is the stand-in
/// itself, which approves devices and reaches no workspace, nor a device's
/// workspace service.
pub fn person_device(cert: &tilo_user_identity::DeviceCert) -> bool {
    let capabilities = &cert.capabilities;
    capabilities.agent.is_none()
        && !capabilities.stand_in
        && !capabilities.workspace_service
        && cert
            .issuer
            .as_ref()
            .is_none_or(|issuer| issuer.capabilities.stand_in || issuer.is_root_signed_device())
}

/// The rule for a hosted browser environment's device, whose scope the server
/// registered when it placed the environment ([`crate::environment`]): its one
/// workspace, on the protocols a window's session works on, and no other. Its
/// record is narrower than a protocol, so what a frame carries decides it: the
/// state protocol's replay, live fan-out, record opening and changes ask
/// [`crate::environment::EnvironmentScope::reaches`], and the live session
/// offers such a device no namespace of anyone else's.
fn environment_decision(
    workspace: &WorkspaceRuntime,
    scope: crate::environment::EnvironmentScope,
    op: WorkspaceOperation,
) -> Decision {
    if scope.admits(workspace.workspace_id(), op) {
        Decision::Admit
    } else {
        Decision::Deny(DenyReason::OutOfEnvironmentScope)
    }
}

/// The rule for an agent's grant whose certificate holds server-wide (see the
/// module): its person's agent, only where its scope reaches, it redeemed
/// its person's invitation and its person is a member, on an agent's protocols.
fn grant_decision(
    workspace: &WorkspaceRuntime,
    peer: &AuthenticatedPeer,
    scope: tilo_user_identity::AgentScope,
    op: WorkspaceOperation,
) -> Decision {
    let here = workspace.workspace_id();
    let reaches = scope
        .workspace_name()
        .is_none_or(|one| WorkspaceId::parse(&one) == Some(here));
    let agents_protocol = matches!(
        op,
        WorkspaceOperation::Subscribe
            | WorkspaceOperation::State
            | WorkspaceOperation::Publish
            | WorkspaceOperation::Replay
            | WorkspaceOperation::KeyAccess
            | WorkspaceOperation::RemoteAction
            | WorkspaceOperation::Live
            | WorkspaceOperation::ContentDiscovery
    );
    if reaches
        && agents_protocol
        && workspace.admits_person(&peer.user_public_key)
        && workspace.lists(&peer.device_public_key)
    {
        Decision::Admit
    } else {
        Decision::Deny(DenyReason::NotAMember)
    }
}

/// Whether `role`'s preset lets its members make undoable changes across a
/// governed workspace ([`tilo_action_policy::roles::role_rules`]): the roles that
/// write records. A Moderator writes as an Editor does, and moderates too.
pub fn role_writes(role: collab_types::Role) -> bool {
    matches!(
        role,
        collab_types::Role::Owner | collab_types::Role::Moderator | collab_types::Role::Editor
    )
}

#[cfg(test)]
#[path = "workspace_access_tests.rs"]
#[allow(
    clippy::significant_drop_tightening,
    reason = "each test keeps its workspace runtime until its last assertion"
)]
pub(crate) mod tests;
