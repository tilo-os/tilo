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
//! 5. A device the room's spectator root certified only watches: it is
//!    admitted on the read planes ([`RoomOperation::reads`]) and refused on
//!    every other, even where the room lists it, and it subscribes only
//!    while one of the room's spectator seats is free ([`Spectators`]). In
//!    a public room, one it holds a seat in, it may also take the channels'
//!    content keys ([`RoomOperation::ChannelReadKeys`]).
//! 6. An exact member device — within any pending replacement's ceiling — or
//!    a device of a user root the room's signed authority admits is admitted.
//! 7. On a same-owner home host ([`AdmissionMode::SameOwner`]) the relay
//!    owner's own devices are admitted, and enrolled where the plane enrols.
//! 8. Anything else is not a member.
//!
//! Spectators are the visitors of a public room who only watch it: a page
//! embedding the room. They are not members one by one; they all act as one
//! person, a member of the room whose root the host keeps and whose short-
//! lived certificates carry no capabilities. Their devices come and go
//! with page loads, so the room names the spectator root and a number of
//! seats instead of each device. A spectator reads what any subscriber is
//! sent: the room's retained state, its live plane and its presence. It
//! never writes: no change, commit, presence, record content, compaction,
//! live or voice frame and no action ([`RoomOperation::Publish`] and every
//! plane that is no read). Its devices are on no channel's roster, and
//! without a write it cannot ask a channel's steward for access either. What
//! members seal into a channel it opens only in a public room
//! ([`Spectators::public`]): anyone may join that room as a guest and be
//! admitted to its channels, so showing a visitor who only watches what a
//! guest would read keeps nothing secret that was secret before. There the
//! steward hands the spectator person, not each of its devices, the
//! channels' content keys ([`RoomOperation::ChannelReadKeys`]): enough to
//! open, and of no use to write, since the relay takes nothing from it. A
//! record a Read Deny withholds from anyone is never opened to spectators.
//! A room that is not public keeps everything its members sealed from them.
//!
//! One plane is not a room membership at all:
//! [`RoomOperation::StewardedChannelKeys`], the relay's own application
//! endpoint (its room actions) taking the content keys of the channels the
//! relay stewards. The relay's device is the steward of every channel of a
//! room it governs: it is already a member of each channel's group and holds
//! its keys. Its room actions are an application endpoint of that same device
//! and key, so they cannot be admitted as a second member of the group; the
//! device hands them the keys it holds, the way a device's session service
//! hands its apps their keys ([`channel_crypto::ChannelContentKeys`]). The
//! rule, after steps 1 and 3: the keys go to the device that governs this
//! room on this relay, in a room whose admission governance owns, and to no
//! other device, ever. Every other device is admitted to a channel by its
//! steward over the channel itself.
//!
//! And one plane admits a device to a room's governance itself:
//! [`RoomOperation::DelegateMembership`], a delegated agent of this relay's own
//! user joining, as a governed member, a room this relay governs. Agents are
//! collaborators: each has its own device key, a certificate and a
//! delegation its person's root signed, and the governor records it on the
//! room's roster bound to that delegation, so every member admits its
//! changes as its own, at its person's role and never above Editor, only
//! within the delegation, and only until the governor withdraws it. The
//! rule, after steps 1 to 3: admit iff the room's admission is governance's,
//! this relay governs it, and the device is a registered agent of this
//! relay's own user whose delegation (step 2) is live and names this room. A
//! device that is no agent, an agent of another user, and any room this
//! relay does not govern are refused; nothing is admitted by membership,
//! enrolment or federation. Being a governed member is what admits the agent
//! to the room on the other planes and lets the room's steward admit it to
//! the channels of its records.
//!
//! The room's admission mode is one value, so a governed room can never be
//! left auto-enrolling by a writer that updated one flag and not the other.

use std::sync::RwLock;

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

/// The plane asking. Planes differ only in the four properties below.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RoomOperation {
    /// A subscribe, and the periodic audit of a live subscription.
    Subscribe,
    /// Holding the room's state: being sent a state frame, answered a sync
    /// request, or keeping a source's frames flowing.
    State,
    /// Taking a device's own frames into the room: a change or commit,
    /// presence, record content, a compaction, or joining the live plane.
    Publish,
    /// Replaying retained state to a subscriber.
    Replay,
    /// A browser live-plane grant or its observation.
    #[cfg(any(test, feature = "web"))]
    Live,
    /// A browser live-plane grant that only receives, or its observation:
    /// the members' cursors and previews, without publishing any.
    #[cfg(any(test, feature = "web"))]
    Watch,
    /// A spectator taking the content keys of one of the room's channels, to
    /// open what members sealed there without being the channel's member
    /// (see the module documentation).
    #[cfg(any(test, feature = "web"))]
    ChannelReadKeys,
    /// Discovering the room's content endpoints.
    ContentDiscovery,
    /// Attaching to the room's compute plane.
    Compute,
    /// Opening or attaching to an environment in the room.
    Environment,
    /// Originating or receiving a remote (directed) action.
    RemoteAction,
    /// The relay's own room actions taking the content keys of a channel the
    /// relay stewards (see the module documentation).
    #[cfg(any(test, feature = "agents"))]
    StewardedChannelKeys,
    /// A delegated agent of this relay's user becoming a governed member of
    /// a room this relay governs (see the module documentation).
    #[cfg(any(test, feature = "agents"))]
    DelegateMembership,
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
        matches!(
            self,
            Self::Subscribe | Self::State | Self::Publish | Self::Replay
        )
    }

    /// Whether this plane only reads the room: the planes a spectator is
    /// admitted on.
    pub(crate) const fn reads(self) -> bool {
        match self {
            Self::Subscribe | Self::State | Self::Replay => true,
            #[cfg(any(test, feature = "web"))]
            Self::Watch => true,
            _ => false,
        }
    }

    /// Whether a user root the room's authority admits stands in for an exact
    /// device on this plane. Compute admits exact devices only.
    const fn admits_user_roots(self) -> bool {
        !matches!(self, Self::Compute)
    }

    const fn owner_device(self) -> OwnerDevice {
        match self {
            Self::Subscribe | Self::Compute => OwnerDevice::Enrol,
            #[cfg(any(test, feature = "web"))]
            Self::Live | Self::Watch => OwnerDevice::Admit,
            #[cfg(any(test, feature = "web"))]
            Self::ChannelReadKeys => OwnerDevice::Refuse,
            Self::ContentDiscovery | Self::Environment => OwnerDevice::Admit,
            Self::State | Self::Publish | Self::Replay | Self::RemoteAction => OwnerDevice::Refuse,
            #[cfg(any(test, feature = "agents"))]
            Self::StewardedChannelKeys | Self::DelegateMembership => OwnerDevice::Refuse,
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
    /// A spectator, who only reads the room, asked to write or act in it.
    WatchOnly,
    /// A spectator subscribing while every spectator seat is taken.
    SpectatorsFull,
    /// Channel keys asked for by a spectator of a room that is not public,
    /// or of one it holds no seat in.
    #[cfg(any(test, feature = "web"))]
    SealedFromSpectators,
    /// Channel keys asked for outside the channel by a device that is no
    /// spectator: a member is admitted to the channel by its steward.
    #[cfg(any(test, feature = "web"))]
    OverTheChannel,
    /// Channel keys asked for by a device other than the one governing and
    /// stewarding the room on this relay.
    #[cfg(any(test, feature = "agents"))]
    NotTheSteward,
    /// Governed membership as an agent asked for by a device that is no
    /// delegated agent of this relay's user, or for a room this relay does
    /// not govern.
    #[cfg(any(test, feature = "agents"))]
    NotADelegatedAgent,
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
            Self::WatchOnly => "a spectator only watches this room",
            Self::SpectatorsFull => "too many people are watching this room",
            #[cfg(any(test, feature = "web"))]
            Self::SealedFromSpectators => {
                "this room keeps what its members write from those who only watch"
            }
            #[cfg(any(test, feature = "web"))]
            Self::OverTheChannel => {
                "a member takes a channel's keys from its steward over the channel"
            }
            #[cfg(any(test, feature = "agents"))]
            Self::NotTheSteward => {
                "only the device that stewards this room's channels holds their keys"
            }
            #[cfg(any(test, feature = "agents"))]
            Self::NotADelegatedAgent => {
                "only a delegated agent of this relay's user joins a room it governs as an agent"
            }
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
    /// Whether `device` is an agent this relay was told about, delegated or
    /// withdrawn. An agent is a device of the relay's own person, yet no
    /// person: what only the host's person may do (its compute, its rooms'
    /// administration) is never its to do. A poisoned registry counts every
    /// device as an agent, so it refuses rather than admits.
    pub(crate) fn is_agent(&self, device: &[u8; 32]) -> bool {
        !self
            .agents
            .read()
            .is_ok_and(|agents| !agents.contains_key(device))
    }

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

    /// The live delegation of agent `device` for `workspace_id` at `now_ns`:
    /// `None` for a device that is no agent, or whose delegation was
    /// withdrawn, expired or does not name the room.
    #[cfg(any(test, feature = "agents"))]
    pub(crate) fn delegation(
        &self,
        device: &[u8; 32],
        workspace_id: u64,
        now_ns: u64,
    ) -> Option<tilo_relay_protocol::delegation::Delegation> {
        self.agents
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(device)
            .cloned()
            .flatten()
            .filter(|delegation| {
                delegation.expires_at_ns > now_ns && delegation.permits_workspace(workspace_id)
            })
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

/// Who may watch a room without being one of its members: every device the
/// spectator root certified, up to `seats` subscribed at once. Set by the
/// room's host for that room only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Spectators {
    /// The spectator person's root: what signs each watching device's
    /// certificate.
    pub root: [u8; 32],
    /// Spectator devices subscribed to the room at once.
    pub seats: usize,
    /// Anyone may join the room as a guest, and so read what its members
    /// seal: spectators may then take the channels' content keys too. A
    /// room that is not public never hands them any.
    pub public: bool,
}

/// Whether `peer` only watches `runtime`'s room: its certificate is the
/// room's spectator root's (rule step 5). A change or commit asks [`decide`]
/// with [`RoomOperation::Publish`], which refuses such a device. Frames that
/// ride on a live subscription without a decision of their own (presence,
/// record content, a compaction, joining the live plane, voice) ask this:
/// a spectator's subscription carries none of its own.
pub(crate) fn watches_only(runtime: &WorkspaceRuntime, peer: &AuthenticatedPeer) -> bool {
    runtime
        .spectators
        .is_some_and(|spectators| spectators.root == peer.user_public_key)
}

/// Whether `peer` may hold a spectator seat in `runtime`'s room: it is no
/// spectator, it holds one already (a live subscription), or one is free.
/// Asked again, under the room's lock, as the subscription is registered,
/// so two spectators subscribing at once cannot both take the last seat.
pub(crate) fn seated(runtime: &WorkspaceRuntime, peer: &AuthenticatedPeer) -> bool {
    let Some(spectators) = runtime.spectators.filter(|_| watches_only(runtime, peer)) else {
        return true;
    };
    runtime.state_sessions.contains_key(&peer.device_public_key)
        || seated_spectators(runtime) < spectators.seats
}

/// The spectators holding a seat in `runtime`'s room: their live
/// subscriptions. A page refused for want of a seat holds none, so it is
/// not counted as watching.
pub(crate) fn seated_spectators(runtime: &WorkspaceRuntime) -> usize {
    runtime.spectators.map_or(0, |spectators| {
        runtime
            .state_sessions
            .values()
            .filter(|session| session.peer.user_public_key == spectators.root)
            .count()
    })
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

    /// Whether `device` governs this room on this relay, and so stewards its
    /// channels: governance owns the room's admission and the relay's room
    /// authority, bound to this room, names `device` as its governor.
    #[cfg(any(test, feature = "agents"))]
    fn governed_by(&self, device: &[u8; 32]) -> bool {
        self.runtime.admission == AdmissionMode::Governed
            && self
                .runtime
                .room_authority
                .as_ref()
                .is_some_and(|authority| authority.governor() == device)
    }

    /// Whether governance owns this room's admission and this relay's own
    /// room authority governs it.
    #[cfg(any(test, feature = "agents"))]
    fn governed_here(&self) -> bool {
        self.runtime.admission == AdmissionMode::Governed && self.runtime.room_authority.is_some()
    }

    fn lists(&self, device: &[u8; 32]) -> bool {
        let listed = self.listed.map_or_else(
            || self.runtime.membership.contains(device),
            |listed| listed.contains(device),
        );
        listed && self.within_ceiling(device)
    }
}

/// Whether the room lists `peer` for `op`: steps 4 to 6 of the rule, with
/// no certificate, agent, cross-user, seat or same-owner question. Only for
/// a recipient check that has no relay trust at hand; everything else asks
/// [`decide`].
pub(crate) fn listed(room: Room<'_>, peer: &AuthenticatedPeer, op: RoomOperation) -> bool {
    if watches_only(room.runtime, peer) {
        return op.reads();
    }
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
    #[cfg(any(test, feature = "agents"))]
    if op == RoomOperation::StewardedChannelKeys {
        // Never by membership, enrolment or federation: only the steward.
        return if room.governed_by(&peer.device_public_key) {
            Decision::Admit
        } else {
            Decision::Deny(DenyReason::NotTheSteward)
        };
    }
    #[cfg(any(test, feature = "agents"))]
    if op == RoomOperation::DelegateMembership {
        // Never by membership, enrolment or federation: only this relay's
        // user's own agent, inside its delegation, in a room governed here.
        let delegated = own_user
            && trust
                .delegation(
                    &peer.device_public_key,
                    room.runtime.store.workspace_id(),
                    now_ns,
                )
                .is_some_and(|delegation| delegation.user_public_key == peer.user_public_key);
        return if delegated && room.governed_here() {
            Decision::Admit
        } else {
            Decision::Deny(DenyReason::NotADelegatedAgent)
        };
    }
    #[cfg(any(test, feature = "web"))]
    if op == RoomOperation::ChannelReadKeys {
        // Never by membership: members are admitted to a channel by its
        // steward. A spectator, in a public room, while it holds a seat.
        return match room
            .runtime
            .spectators
            .filter(|_| watches_only(room.runtime, peer))
        {
            None => Decision::Deny(DenyReason::OverTheChannel),
            Some(spectators)
                if spectators.public
                    && room
                        .runtime
                        .state_sessions
                        .contains_key(&peer.device_public_key) =>
            {
                Decision::Admit
            }
            Some(_) => Decision::Deny(DenyReason::SealedFromSpectators),
        };
    }
    if watches_only(room.runtime, peer) {
        // Before membership: the device the spectator person joined with
        // is listed, and must not write either.
        return if !op.reads() {
            Decision::Deny(DenyReason::WatchOnly)
        } else if op == RoomOperation::Subscribe && !seated(room.runtime, peer) {
            Decision::Deny(DenyReason::SpectatorsFull)
        } else {
            Decision::Admit
        };
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

/// Whether `role`'s preset lets its members make undoable changes across a
/// governed room ([`tilo_action_policy::roles::role_rules`]): the roles that
/// write records.
pub(crate) fn role_writes(role: collab_types::Role) -> bool {
    matches!(role, collab_types::Role::Owner | collab_types::Role::Editor)
}

#[cfg(test)]
#[path = "room_access_tests.rs"]
#[allow(
    clippy::significant_drop_tightening,
    reason = "each test keeps its room runtime until its last assertion"
)]
pub(crate) mod tests;
