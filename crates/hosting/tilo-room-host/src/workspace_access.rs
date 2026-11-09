//! The one answer to "may this device act in this workspace now?".
//!
//! Every relay plane that admits a device into a workspace — subscribe, the
//! access audit, state delivery, replay, remote actions, the browser live
//! plane, content discovery, compute and environments — asks [`decide`] with
//! its [`WorkspaceOperation`]. The rule is written once:
//!
//! 1. The certificate is current relay-wide ([`RelayTrust::cert`]): not
//!    expired, not revoked, admitted by the live trust policy.
//! 2. An agent device acts only inside its live delegation.
//! 3. A host this workspace is attached to is admitted on the planes that carry
//!    its changes (subscribe, state, publish, replay), listed or not.
//! 4. An exact member device — within any pending replacement's ceiling — or
//!    a device of a user root the workspace's signed authority admits is admitted.
//! 5. On a same-owner home host ([`AdmissionMode::SameOwner`]) the relay
//!    owner's own devices are admitted, and enrolled where the plane enrols.
//! 6. Anything else is not a member.
//!
//! An agent's grant (a key the person certified for a service that acts for
//! an agent they approved) is admitted on none of these planes: it acts in
//! workspaces through the person's runtime, which checks it, and asks this
//! host only its own operations, which no workspace decides.
//!
//! A public workspace is no exception to the rule: whoever visits it is a
//! member of it with the role its owner gave them, and every device certified
//! by that member's root is that member's device, admitted to the channels its
//! records take part in like any other.
//!
//! And one plane admits a device to a workspace's governance itself:
//! [`WorkspaceOperation::DelegateMembership`], a delegated agent of this relay's own
//! user joining, as a governed member, a workspace this relay governs. Agents are
//! collaborators: each has its own device key, a certificate and a
//! delegation its person's root signed, and the governor records it on the
//! workspace's roster bound to that delegation, so every member admits its
//! changes as its own, at its person's role and never above Editor, only
//! within the delegation, and only until the governor withdraws it. The
//! rule, after steps 1 to 3: admit iff the workspace's admission is governance's,
//! this relay governs it, and the device is a registered agent of this
//! relay's own user whose delegation (step 2) is live and names this workspace. A
//! device that is no agent, an agent of another user, and any workspace this
//! relay does not govern are refused; nothing is admitted by membership,
//! enrolment or attachment. Being a governed member is what admits the agent
//! to the workspace on the other planes and lets the workspace's participants admit it
//! to the channels of its records.
//!
//! The workspace's admission mode is one value, so a governed workspace can never be
//! left auto-enrolling by a writer that updated one flag and not the other.

use std::sync::RwLock;

use tilo_user_identity::RevocationList;

use crate::auth::AuthenticatedPeer;
use crate::peer::AgentRegistry;
use crate::policy::TrustPolicy;
use crate::workspace::WorkspaceRuntime;
use tilo_lock::RwLockRecover as _;

/// How a workspace admits devices beyond its exact membership.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum AdmissionMode {
    /// Only exact member devices and admitted user roots.
    #[default]
    Explicit,
    /// Same-owner home host: the relay owner's devices are admitted, and
    /// enrolled by the planes that enrol.
    SameOwner,
    /// Native governance owns admission; nothing is ever auto-enrolled, and
    /// no later binding or autobind may relax it.
    Governed,
}

impl AdmissionMode {
    /// The mode a workspace takes when a binding or autobind asks for `requested`:
    /// governance, once set, is never relaxed.
    #[must_use]
    pub(crate) const fn rebind(self, requested: Self) -> Self {
        match self {
            Self::Governed => Self::Governed,
            Self::Explicit | Self::SameOwner => requested,
        }
    }
}

/// The plane asking. Planes differ only in the three properties below.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkspaceOperation {
    /// A subscribe, and the periodic audit of a live subscription.
    Subscribe,
    /// Holding the workspace's state: being sent a state frame, answered a sync
    /// request, or keeping a source's frames flowing.
    State,
    /// Taking a device's own frames into the workspace: a change or commit,
    /// presence, record content, a compaction, or joining the live plane.
    Publish,
    /// Replaying retained state to a subscriber.
    Replay,
    /// A browser page's session in the workspace's live plane.
    Live,
    /// A device asking one of the workspace's channels for access: the plane
    /// its own key package arrives on (see the module documentation). The
    /// channel's group admits the devices its record's readers name.
    ChannelAccess,
    /// Discovering the workspace's content endpoints.
    ContentDiscovery,
    Compute,
    /// Opening or attaching to an environment in the workspace.
    Environment,
    /// Originating or receiving a remote (directed) action.
    RemoteAction,
    /// A delegated agent of this relay's user becoming a governed member of
    /// a workspace this relay governs (see the module documentation).
    #[cfg(any(test, feature = "agents"))]
    DelegateMembership,
}

/// What a same-owner workspace does with one of its owner's devices it does not
/// list yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OwnerDevice {
    /// Admit it and persist it as a member.
    Enrol,
    Admit,
    /// Refuse: this plane serves only devices already enrolled.
    Refuse,
}

impl WorkspaceOperation {
    /// Whether this plane carries a workspace's changes.
    const fn carries_changes(self) -> bool {
        matches!(
            self,
            Self::Subscribe | Self::State | Self::Publish | Self::Replay | Self::ChannelAccess
        )
    }

    const fn owner_device(self) -> OwnerDevice {
        match self {
            Self::Subscribe | Self::Compute => OwnerDevice::Enrol,
            Self::ContentDiscovery | Self::Environment | Self::Live => OwnerDevice::Admit,
            Self::State
            | Self::Publish
            | Self::Replay
            | Self::RemoteAction
            | Self::ChannelAccess => OwnerDevice::Refuse,
            #[cfg(any(test, feature = "agents"))]
            Self::DelegateMembership => OwnerDevice::Refuse,
        }
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

/// Whether `user` is the person this host runs for, or is admitted by a
/// workspace it serves: who the host queues a device certificate for, beside
/// the owner's own devices.
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
        if workspace.lock().await.admits_person(&user, &own_user) {
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
    /// Governed membership as an agent asked for by a device that is no
    /// delegated agent of this relay's user, or for a workspace this relay does
    /// not govern.
    #[cfg(any(test, feature = "agents"))]
    NotADelegatedAgent,
}

impl DenyReason {
    /// The refusal as wire text.
    pub const fn reason(self) -> &'static str {
        match self {
            Self::CertExpired => "device certificate expired",
            Self::Revoked => "authenticated device was revoked",
            Self::NotTrusted => "collaborator was removed from signed workspace authority",
            Self::AgentWithdrawn => "the agent was withdrawn",
            Self::AgentExpired => "the agent's delegation expired",
            Self::AgentOutOfScope => "the agent is not delegated this room",
            Self::NotAMember => "device not in workspace membership",
            #[cfg(any(test, feature = "agents"))]
            Self::NotADelegatedAgent => {
                "only a delegated agent of this relay's user joins a room it governs as an agent"
            }
        }
    }
}

/// Relay-wide certificate validity and agent delegation: one copy.
#[derive(Clone, Copy)]
pub struct RelayTrust<'a> {
    pub revocations: &'a RwLock<RevocationList>,
    /// Live handshake trust policy.
    pub policy: &'a RwLock<TrustPolicy>,
    /// The user this relay chains to.
    pub own_user: &'a [u8; 32],
    /// Agent delegations this relay was told about.
    pub agents: &'a AgentRegistry,
}

impl RelayTrust<'_> {
    /// Whether `device` is an agent this relay was told about, delegated or
    /// withdrawn. An agent is a device of the relay's own person, yet no
    /// person: what only the host's person may do (its compute, its workspaces'
    /// administration) is never its to do. A poisoned registry counts every
    /// device as an agent, so it refuses rather than admits.
    pub fn is_agent(&self, device: &[u8; 32]) -> bool {
        !self
            .agents
            .read()
            .is_ok_and(|agents| !agents.contains_key(device))
    }

    /// Whether `peer`'s certificate is current relay-wide at `now_ns`. A
    /// poisoned list or policy refuses.
    pub fn cert(&self, peer: &AuthenticatedPeer, now_ns: u64) -> Result<(), DenyReason> {
        if peer
            .cert
            .expires_at_ns
            .is_some_and(|expiry| expiry <= now_ns)
        {
            return Err(DenyReason::CertExpired);
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
        workspace_id: u64,
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
        workspace_id: u64,
        now_ns: u64,
    ) -> Option<tilo_relay_protocol::delegation::Delegation> {
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
    pub(crate) fn principal(
        &self,
        peer: &AuthenticatedPeer,
        workspace_id: u64,
        now_ns: u64,
    ) -> Result<(), DenyReason> {
        self.cert(peer, now_ns)?;
        self.agent(&peer.device_public_key, workspace_id, now_ns)
    }
}

/// The workspace facts [`decide`] reads.
#[derive(Clone, Copy)]
pub(crate) struct WorkspaceFacts<'a> {
    runtime: &'a WorkspaceRuntime,
    /// Member devices just read from disk, in place of the runtime's set.
    listed: Option<&'a [[u8; 32]]>,
}

impl<'a> WorkspaceFacts<'a> {
    pub(crate) const fn current(runtime: &'a WorkspaceRuntime) -> Self {
        Self {
            runtime,
            listed: None,
        }
    }

    /// The workspace with member devices read from disk by a caller that may not
    /// reload the runtime's set; the replacement ceiling still bounds them.
    pub(crate) const fn with_listed(runtime: &'a WorkspaceRuntime, listed: &'a [[u8; 32]]) -> Self {
        Self {
            runtime,
            listed: Some(listed),
        }
    }

    fn within_ceiling(&self, device: &[u8; 32]) -> bool {
        self.runtime
            .membership_ceiling
            .as_ref()
            .is_none_or(|ceiling| ceiling.contains(device))
    }

    /// Whether governance owns this workspace's admission and this relay's own
    /// workspace authority governs it.
    #[cfg(any(test, feature = "agents"))]
    fn governed_here(&self) -> bool {
        self.runtime.admission == AdmissionMode::Governed
            && self.runtime.workspace_authority.is_some()
    }

    fn lists(&self, device: &[u8; 32]) -> bool {
        let listed = self.listed.map_or_else(
            || self.runtime.membership.contains(device),
            |listed| listed.contains(device),
        );
        listed && self.within_ceiling(device)
    }
}

/// Whether the workspace lists `peer` for `op`: steps 4 to 6 of the rule, with
/// no certificate, agent, cross-user, seat or same-owner question. Only for
/// a recipient check that has no relay trust at hand; everything else asks
/// [`decide`].
pub(crate) fn listed(
    workspace: WorkspaceFacts<'_>,
    peer: &AuthenticatedPeer,
    op: WorkspaceOperation,
) -> bool {
    (op.carries_changes() && workspace.runtime.attached(&peer.device_public_key))
        || workspace.lists(&peer.device_public_key)
}

/// The one admission decision for every plane, at the current time.
pub fn decide(
    workspace: &WorkspaceRuntime,
    trust: &RelayTrust<'_>,
    peer: &AuthenticatedPeer,
    op: WorkspaceOperation,
) -> Decision {
    decide_in(
        WorkspaceFacts::current(workspace),
        trust,
        peer,
        op,
        tilo_clock::unix_now_ns(),
    )
}

/// [`decide`] over explicit workspace facts at `now_ns`.
pub(crate) fn decide_in(
    workspace: WorkspaceFacts<'_>,
    trust: &RelayTrust<'_>,
    peer: &AuthenticatedPeer,
    op: WorkspaceOperation,
    now_ns: u64,
) -> Decision {
    if let Err(reason) = trust.principal(peer, workspace.runtime.store.workspace_id(), now_ns) {
        return Decision::Deny(reason);
    }
    if peer.cert.capabilities.agent.is_some() {
        return Decision::Deny(DenyReason::NotAMember);
    }
    let own_user = &peer.user_public_key == trust.own_user;
    #[cfg(any(test, feature = "agents"))]
    if op == WorkspaceOperation::DelegateMembership {
        // Never by membership, enrolment or attachment: only this relay's
        // user's own agent, inside its delegation, in a workspace governed here.
        let delegated = own_user
            && trust
                .delegation(
                    &peer.device_public_key,
                    workspace.runtime.store.workspace_id(),
                    now_ns,
                )
                .is_some_and(|delegation| delegation.user_public_key == peer.user_public_key);
        return if delegated && workspace.governed_here() {
            Decision::Admit
        } else {
            Decision::Deny(DenyReason::NotADelegatedAgent)
        };
    }
    if listed(workspace, peer, op) {
        return Decision::Admit;
    }
    if workspace.runtime.admission == AdmissionMode::SameOwner
        && own_user
        && workspace.within_ceiling(&peer.device_public_key)
    {
        match op.owner_device() {
            OwnerDevice::Enrol => return Decision::AdmitAndEnrol,
            OwnerDevice::Admit => return Decision::Admit,
            OwnerDevice::Refuse => {}
        }
    }
    Decision::Deny(DenyReason::NotAMember)
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
