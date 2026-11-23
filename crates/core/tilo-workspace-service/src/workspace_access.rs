//! The one answer to "may this device act in this workspace now?".
//!
//! Every server protocol that admits a device into a workspace — subscribe, the
//! access audit, state delivery, replay, remote actions, the browser live
//! protocol, content discovery, compute and environments — asks [`decide`] with
//! its [`WorkspaceOperation`]. The rule is written once:
//!
//! 1. The certificate is current server-wide ([`ServerTrust::cert`]): not
//!    expired, not revoked, not blocked by the operator, admitted by the live
//!    trust policy.
//! 2. An agent device acts only inside its live delegation.
//! 3. A host this workspace is attached to is admitted on the protocols that carry
//!    its changes (subscribe, state, publish, replay), listed or not.
//! 4. An exact member device — within any pending replacement's ceiling — or
//!    a device of a user root the workspace's signed authority admits is admitted.
//! 5. In a workspace this server governs, a device of a person its governance
//!    has as a member ([`person_device`]: a browser tab, a new computer of
//!    theirs) is enrolled when it subscribes: governance records it under its
//!    person, as the entry does for a further device of a member, so it holds
//!    the part its person holds. Membership follows the person, not one
//!    device key.
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
//! - it is an exact member device here: it redeemed an invitation for its
//!   person, as any new device of theirs does, so the workspace's roster
//!   holds it under its person;
//! - in a governed workspace, its person is still a member;
//! - and the protocol is one an agent works on: subscribe, state, publish,
//!   replay, channel access, remote actions, the live session and content
//!   discovery. Never compute, environments or governed membership, and
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
//! And one protocol admits a device to a workspace's governance itself:
//! [`WorkspaceOperation::DelegateMembership`], a delegated agent joining, as a
//! governed member, a workspace this server governs. Agents are members: each
//! has its own device key, a certificate and a delegation its person's root
//! signed, and the governor records it on the workspace's roster bound to that
//! delegation, so every member admits its changes as its own, at its person's
//! role and never above Editor, only within the delegation, and only until the
//! governor withdraws it. The rule, after steps 1 to 3: admit iff the
//! workspace's admission is governance's, this server governs it, and the
//! device is a registered agent whose live delegation (step 2) names this
//! workspace and the person the agent acts for. The agent's approver is any
//! device of that person, the host's own person included and never singled
//! out. A device that is no agent, and any workspace this server does not
//! govern, are refused; nothing is admitted by membership or attachment. Being a governed member is what admits the agent
//! to the workspace on the other protocols and lets the workspace's key holders admit it
//! to the channels of its records.
//!
//! The workspace's admission mode is one value, so a governed workspace can never be
//! left auto-enrolling by a writer that updated one flag and not the other.

use std::sync::RwLock;

use tilo_user_identity::RevocationList;

use crate::auth::AuthenticatedPeer;
use crate::registry::AgentRegistry;
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
    /// A delegated agent becoming a governed member of a workspace this
    /// server governs (see the module documentation).
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

/// Whether `user` is the person this host runs for, or is admitted by a
/// workspace it serves: who the host queues a device certificate for, beside
/// the owner's own devices.
pub async fn admits_person(
    workspaces: &crate::registry::WorkspaceRegistry,
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
    /// The host's operator blocks the device, or the person whose device it is.
    Blocked,
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
    /// Governed membership as an agent asked for by a device that is no
    /// delegated agent of its person, or for a workspace this server does
    /// not govern.
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
            Self::Blocked => Refusal::NotAllowed(self.reason().into()),
            Self::NotTrusted => Refusal::NotAdmitted,
            _ => Refusal::NotAMember(self.reason().into()),
        }
    }

    /// The refusal as text, for the log.
    pub const fn reason(self) -> &'static str {
        match self {
            Self::CertExpired => "device certificate expired",
            Self::Revoked => "authenticated device was revoked",
            Self::Blocked => "this host refuses this device",
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
    /// The devices and people the operator blocks, read from the record of
    /// their own workspace ([`crate::workspaces::blocked`]). No membership or
    /// entry rule bypasses it.
    pub blocked: &'a RwLock<crate::workspaces::blocked::BlockedKeys>,
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
        // A device is revoked by its root's signed entry, or by the mark the
        // record of its person's devices carries, which reaches every host that
        // holds the record as the change does. A poisoned list refuses.
        if !self
            .revocations
            .read()
            .is_ok_and(|list| !list.refuses(&peer.cert))
            || !self
                .blocked
                .read()
                .is_ok_and(|keys| !keys.revokes(&peer.cert))
        {
            return Err(DenyReason::Revoked);
        }
        // The operator's explicit block stands ahead of the policy that gates
        // who connects here: a workspace's membership admits a member, and this
        // refuses a key its membership holds. A poisoned list refuses.
        if !self
            .blocked
            .read()
            .is_ok_and(|blocked| !blocked.blocks(&peer.device_public_key, &peer.user_public_key))
        {
            return Err(DenyReason::Blocked);
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

/// The workspace facts [`decide`] reads, from the host's runtime for it or from
/// the replica its links admit into.
///
/// The rule is one; where it reads its facts is not. A connection the host
/// keeps a runtime for (`crate::workspace::WorkspaceRuntime`) decides from
/// that, as every session always has. A device's link is admitted before it
/// listens, from a synchronous callback, so it decides from the workspace's
/// replica, which its links share and which holds the same governance: its
/// members by their roots, the devices it lists now, and the hosts keeping it.
#[derive(Clone, Copy)]
pub(crate) enum WorkspaceFacts<'a> {
    /// The host's own runtime for the workspace, as its sessions read it.
    Runtime(RuntimeFacts<'a>),
    /// What a workspace's link holds of it, as it admits into it.
    #[cfg(not(target_arch = "wasm32"))]
    Link(LinkFacts<'a>),
}

/// What the host's runtime knows of a workspace.
#[derive(Clone, Copy)]
pub(crate) struct RuntimeFacts<'a> {
    runtime: &'a WorkspaceRuntime,
    /// Member devices just read from disk, in place of the runtime's set.
    listed: Option<&'a [[u8; 32]]>,
}

/// What a workspace's replica knows of it.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Copy)]
pub(crate) struct LinkFacts<'a> {
    peer: &'a tilo_peer_state::StatePeer,
    workspace_id: WorkspaceId,
    /// Whether this host keeps `workspace_id` for `person`, one of their own
    /// ([`WorkspaceAuthority::keeps_for`](crate::workspaces::WorkspaceAuthority::keeps_for)).
    keeps_for: &'a dyn Fn(&[u8; 32]) -> bool,
    now: web_time::SystemTime,
}

impl<'a> WorkspaceFacts<'a> {
    /// The workspace with member devices read from disk by a caller that may not
    /// reload the runtime's set.
    #[cfg(test)]
    pub(crate) const fn with_listed(runtime: &'a WorkspaceRuntime, listed: &'a [[u8; 32]]) -> Self {
        Self::Runtime(RuntimeFacts {
            runtime,
            listed: Some(listed),
        })
    }

    pub(crate) const fn current(runtime: &'a WorkspaceRuntime) -> Self {
        Self::Runtime(RuntimeFacts {
            runtime,
            listed: None,
        })
    }

    /// The facts `peer`'s replica holds of `workspace_id`, at `now`:
    /// [`decide`] for a device dialling the workspace's link, whose admission
    /// runs before any runtime lock can be waited for.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn of_link(
        peer: &'a tilo_peer_state::StatePeer,
        workspace_id: WorkspaceId,
        keeps_for: &'a dyn Fn(&[u8; 32]) -> bool,
        now: web_time::SystemTime,
    ) -> Self {
        Self::Link(LinkFacts {
            peer,
            workspace_id,
            keeps_for,
            now,
        })
    }

    pub(crate) fn workspace_id(&self) -> WorkspaceId {
        match self {
            Self::Runtime(facts) => facts.runtime.workspace_id(),
            #[cfg(not(target_arch = "wasm32"))]
            Self::Link(facts) => facts.workspace_id,
        }
    }

    /// Whether `device` is a host this workspace is attached to: a device that
    /// keeps the workspace and may carry another device's frames to it.
    pub(crate) fn attached(&self, device: &[u8; 32]) -> bool {
        match self {
            Self::Runtime(facts) => facts.runtime.attached(device),
            #[cfg(not(target_arch = "wasm32"))]
            Self::Link(facts) => facts
                .peer
                .governance()
                .keeping_devices()
                .iter()
                .any(|keeping| keeping.host.device_public_key == *device),
        }
    }

    /// Whether this server's own workspace authority governs the workspace.
    #[cfg(any(test, feature = "agents"))]
    fn governed_here(&self) -> bool {
        self.host_decides_membership()
    }

    /// Whether a subscribe enrols `peer`'s device here (step 5): this server
    /// governs the workspace and its member list has the device's person. A
    /// workspace of a person's own admits a device of theirs by their root
    /// alone, before any of theirs has entered it: the host knows whose it is,
    /// and names them its member as the device joins.
    fn enrols_member_device(&self, peer: &AuthenticatedPeer) -> bool {
        self.host_decides_membership()
            && person_device(&peer.cert)
            && self.person_member(&peer.user_public_key)
    }

    /// Whether this server's own workspace authority governs the workspace here.
    fn host_decides_membership(&self) -> bool {
        match self {
            Self::Runtime(facts) => facts.runtime.workspace_authority.is_some(),
            #[cfg(not(target_arch = "wasm32"))]
            Self::Link(facts) => facts.peer.has_governance(),
        }
    }

    /// Whether `person` is one of the workspace's members by their root, or the
    /// person this host keeps it for.
    fn person_member(&self, person: &[u8; 32]) -> bool {
        match self {
            Self::Runtime(facts) => {
                facts.runtime.members.member_user_roots.contains(person)
                    || facts.keeps_for(person)
            }
            #[cfg(not(target_arch = "wasm32"))]
            Self::Link(facts) => {
                !facts.peer.has_governance()
                    || facts
                        .peer
                        .governance()
                        .governed_access(facts.now)
                        .is_ok_and(|access| access.member_user_roots.contains(person))
                    || (facts.keeps_for)(person)
            }
        }
    }

    fn lists(&self, device: &[u8; 32]) -> bool {
        match self {
            Self::Runtime(facts) => facts.listed.map_or_else(
                || facts.runtime.members.member_devices.contains(device),
                |listed| listed.contains(device),
            ),
            // The devices its governance lists now, or, for a workspace that
            // carries no governance, the roster of certificates it holds.
            #[cfg(not(target_arch = "wasm32"))]
            Self::Link(facts) => match facts.peer.governance().governed_access(facts.now) {
                Ok(access) => access.member_devices.contains(device),
                Err(_) => facts
                    .peer
                    .certificates()
                    .iter()
                    .any(|certificate| certificate.device_public_key == *device),
            },
        }
    }
}

impl RuntimeFacts<'_> {
    fn keeps_for(&self, person: &[u8; 32]) -> bool {
        self.runtime
            .workspace_authority
            .as_ref()
            .is_some_and(|authority| authority.keeps_for(person))
    }
}

/// Whether the workspace lists `peer` for `op`: steps 4 to 6 of the rule, with
/// no certificate, agent, cross-user or local-person question. Only for
/// a recipient check that has no server trust at hand; everything else asks
/// [`decide`].
pub(crate) fn listed(
    workspace: WorkspaceFacts<'_>,
    peer: &AuthenticatedPeer,
    op: WorkspaceOperation,
) -> bool {
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
    trust: &ServerTrust<'_>,
    peer: &AuthenticatedPeer,
    op: WorkspaceOperation,
    now_ns: u64,
) -> Decision {
    if let Err(reason) = trust.actor(peer, workspace.workspace_id(), now_ns) {
        // The operator's handshake policy gates who connects to this host. A workspace's own
        // signed membership admits its members, as its entry does: a device of a person it holds
        // reaches the workspace on that rule, even where the operator's policy would not admit
        // it. Every other refusal — an expired or revoked certificate, a blocked key, an agent
        // outside its delegation, a stopped environment — still denies: in particular a block is
        // never read as the policy's refusal and bypassed.
        if reason != DenyReason::NotTrusted || !person_device(&peer.cert) {
            return Decision::Deny(reason);
        }
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
        // workspace governed here.
        let delegated = trust
            .delegation(&peer.device_public_key, workspace.workspace_id(), now_ns)
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
    if op == WorkspaceOperation::Subscribe && workspace.enrols_member_device(peer) {
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
    workspace: WorkspaceFacts<'_>,
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
    workspace: WorkspaceFacts<'_>,
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
    let person_member = workspace.person_member(&peer.user_public_key);
    if reaches && agents_protocol && person_member && workspace.lists(&peer.device_public_key) {
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
