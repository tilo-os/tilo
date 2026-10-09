//! The one answer to "may this device act in this room now?".
//!
//! Every relay plane that admits a device into a room — subscribe, the
//! access audit, state delivery, replay, remote actions, the browser live
//! plane, content discovery, compute and environments — asks [`decide`] with
//! its [`RoomOperation`]. The rule is written once:
//!
//! 1. The certificate is current relay-wide ([`RelayTrust::cert`]): not
//!    expired, not revoked, admitted by the live trust policy.
//! 2. An agent device acts only inside its live delegation.
//! 3. A host-capable certificate of another user never federates a room.
//! 4. A same-user peer relay this room federates with is admitted on the
//!    planes that carry federation (subscribe, state, replay).
//! 5. An exact member device — within any pending replacement's ceiling — or
//!    a device of a user root the room's signed authority admits is admitted.
//! 6. On a same-owner home host ([`AdmissionMode::SameOwner`]) the relay
//!    owner's own devices are admitted, and enrolled where the plane enrols.
//! 7. Anything else is not a member.
//!
//! The room's admission mode is one value, so a governed room can never be
//! left auto-enrolling by a writer that updated one flag and not the other.

use std::sync::RwLock;

use collab_types::caps::{Verb, role_caps};
use tilo_user_identity::RevocationList;

use crate::auth::{AuthenticatedPeer, PeerRole};
use crate::peer::AgentRegistry;
use crate::policy::TrustPolicy;
use crate::workspace::WorkspaceRuntime;

/// How a room admits devices beyond its exact membership.
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
    /// The mode a room takes when a binding or autobind asks for `requested`:
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
pub(crate) enum RoomOperation {
    /// A subscribe, and the periodic audit of a live subscription.
    Subscribe,
    /// Admitting, fanning out or answering a state frame.
    State,
    /// Replaying retained state to a subscriber.
    Replay,
    /// A browser live-plane grant or its observation.
    #[cfg_attr(
        not(feature = "web"),
        allow(
            dead_code,
            reason = "only the browser plane asks, and it is behind `web`"
        )
    )]
    Live,
    /// Discovering the room's content endpoints.
    ContentDiscovery,
    /// Attaching to the room's compute plane.
    Compute,
    /// Opening or attaching to an environment in the room.
    Environment,
    /// Originating or receiving a remote (directed) action.
    RemoteAction,
}

/// What a same-owner room does with one of its owner's devices it does not
/// list yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OwnerDevice {
    /// Admit it and persist it as a member.
    Enrol,
    /// Admit it without enrolling it.
    Admit,
    /// Refuse: this plane serves only devices already enrolled.
    Refuse,
}

impl RoomOperation {
    /// Whether this plane carries federation between same-user relays.
    const fn federates(self) -> bool {
        matches!(self, Self::Subscribe | Self::State | Self::Replay)
    }

    /// Whether a user root the room's authority admits stands in for an exact
    /// device on this plane. Compute admits exact devices only.
    const fn admits_user_roots(self) -> bool {
        !matches!(self, Self::Compute)
    }

    const fn owner_device(self) -> OwnerDevice {
        match self {
            Self::Subscribe | Self::Compute => OwnerDevice::Enrol,
            Self::Live | Self::ContentDiscovery | Self::Environment => OwnerDevice::Admit,
            Self::State | Self::Replay | Self::RemoteAction => OwnerDevice::Refuse,
        }
    }
}

/// The admission decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Decision {
    /// Admitted now.
    Admit,
    /// Admitted once the device is persisted as a member; a caller that
    /// cannot enrol treats it as not yet a member.
    AdmitAndEnrol,
    /// Refused.
    Deny(DenyReason),
}

impl Decision {
    /// Admitted without any further step.
    pub(crate) fn admitted(self) -> bool {
        self == Self::Admit
    }
}

/// Why [`decide`] refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DenyReason {
    /// The certificate's expiry has passed.
    CertExpired,
    /// The device is on the revocation list.
    Revoked,
    /// The live trust policy no longer admits the certificate's user.
    NotTrusted,
    /// A host-capable certificate of another user.
    CrossUserFederation,
    /// The agent was withdrawn.
    AgentWithdrawn,
    /// The agent's delegation expired.
    AgentExpired,
    /// The agent is not delegated this room.
    AgentOutOfScope,
    /// Not a member of the room.
    NotAMember,
}

impl DenyReason {
    /// The refusal as wire text.
    pub(crate) const fn reason(self) -> &'static str {
        match self {
            Self::CertExpired => "device certificate expired",
            Self::Revoked => "authenticated device was revoked",
            Self::NotTrusted => "collaborator was removed from signed workspace authority",
            Self::CrossUserFederation => "cross-user certificates cannot federate relay rooms",
            Self::AgentWithdrawn => "the agent was withdrawn",
            Self::AgentExpired => "the agent's delegation expired",
            Self::AgentOutOfScope => "the agent is not delegated this room",
            Self::NotAMember => "device not in workspace membership",
        }
    }
}

/// Relay-wide certificate validity and agent delegation: one copy.
#[derive(Clone, Copy)]
pub(crate) struct RelayTrust<'a> {
    /// Live revocation list.
    pub(crate) revocations: &'a RwLock<RevocationList>,
    /// Live handshake trust policy.
    pub(crate) policy: &'a RwLock<TrustPolicy>,
    /// The user this relay chains to.
    pub(crate) own_user: &'a [u8; 32],
    /// Agent delegations this relay was told about.
    pub(crate) agents: &'a AgentRegistry,
}

impl RelayTrust<'_> {
    /// Whether `peer`'s certificate is current relay-wide at `now_ns`. A
    /// poisoned list or policy refuses.
    ///
    /// # Errors
    ///
    /// The first reason the certificate is not current.
    pub(crate) fn cert(&self, peer: &AuthenticatedPeer, now_ns: u64) -> Result<(), DenyReason> {
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
            .is_ok_and(|list| !list.is_revoked(&peer.device_public_key))
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
        let agents = self
            .agents
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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

    /// Certificate and agent delegation together: everything about `peer`
    /// that does not depend on the room's membership.
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

/// The room facts [`decide`] reads.
#[derive(Clone, Copy)]
pub(crate) struct Room<'a> {
    runtime: &'a WorkspaceRuntime,
    /// Member devices just read from disk, in place of the runtime's set.
    listed: Option<&'a [[u8; 32]]>,
}

impl<'a> Room<'a> {
    /// The room as its runtime holds it.
    pub(crate) const fn current(runtime: &'a WorkspaceRuntime) -> Self {
        Self {
            runtime,
            listed: None,
        }
    }

    /// The room with member devices read from disk by a caller that may not
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

    fn lists(&self, device: &[u8; 32]) -> bool {
        let listed = self.listed.map_or_else(
            || self.runtime.membership.contains(device),
            |listed| listed.contains(device),
        );
        listed && self.within_ceiling(device)
    }
}

/// Whether the room lists `peer` for `op`: steps 4 and 5 of the rule, with
/// no certificate, agent, cross-user or same-owner question. Only for a
/// recipient check that has no relay trust at hand; everything else asks
/// [`decide`].
pub(crate) fn listed(room: Room<'_>, peer: &AuthenticatedPeer, op: RoomOperation) -> bool {
    (op.federates()
        && peer.role == PeerRole::PeerRelay
        && room
            .runtime
            .known_peer_relays
            .contains(&peer.device_public_key))
        || room.lists(&peer.device_public_key)
        || (op.admits_user_roots() && room.runtime.admitted_users.contains(&peer.user_public_key))
}

/// The one admission decision for every plane, at the current time.
pub(crate) fn decide(
    room: &WorkspaceRuntime,
    trust: &RelayTrust<'_>,
    peer: &AuthenticatedPeer,
    op: RoomOperation,
) -> Decision {
    decide_in(
        Room::current(room),
        trust,
        peer,
        op,
        tilo_clock::unix_now_ns(),
    )
}

/// [`decide`] over explicit room facts at `now_ns`.
pub(crate) fn decide_in(
    room: Room<'_>,
    trust: &RelayTrust<'_>,
    peer: &AuthenticatedPeer,
    op: RoomOperation,
    now_ns: u64,
) -> Decision {
    if let Err(reason) = trust.principal(peer, room.runtime.store.workspace_id(), now_ns) {
        return Decision::Deny(reason);
    }
    let own_user = &peer.user_public_key == trust.own_user;
    if peer.role == PeerRole::PeerRelay && !own_user {
        return Decision::Deny(DenyReason::CrossUserFederation);
    }
    if listed(room, peer, op) {
        return Decision::Admit;
    }
    if room.runtime.admission == AdmissionMode::SameOwner
        && own_user
        && room.within_ceiling(&peer.device_public_key)
    {
        match op.owner_device() {
            OwnerDevice::Enrol => return Decision::AdmitAndEnrol,
            OwnerDevice::Admit => return Decision::Admit,
            OwnerDevice::Refuse => {}
        }
    }
    Decision::Deny(DenyReason::NotAMember)
}

/// Whether `role` carries at least `verb` in a governed room.
pub(crate) fn role_grants(role: collab_types::Role, verb: Verb) -> bool {
    role_caps(role).is_some_and(|ceiling| ceiling.implies(verb))
}

#[cfg(test)]
#[path = "room_access_tests.rs"]
#[allow(
    clippy::significant_drop_tightening,
    reason = "each test keeps its room runtime until its last assertion"
)]
pub(crate) mod tests;
