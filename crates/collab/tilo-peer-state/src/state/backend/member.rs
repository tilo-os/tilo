//! One current-member algorithm over actual admitted room data.
use super::{
    BoundedNative, CertificateLookupError, GovernedIdentity, LegacyNative, PeerCore, Role, Storage,
    SystemTime,
};
use crate::room::key_hex::KeyHex;
use crate::state::bounded::{BoundedPeerError, BoundedPeerInvalid, PeerLimits};
use std::collections::BTreeMap;
use tilo_action_policy::{Authority, Bound, Grant, RoomPolicy, Subject};
use tilo_workspace_join::delegation::Delegate;
mod policy;

#[derive(Debug)]
pub(in crate::state) enum Error {
    Certificate(CertificateLookupError),
    GovernanceMissing,
    Absent,
    LegacyJson(String),
    Decode(crate::resources::PeerResourceError),
    CertificateMismatch,
    RevocationRoot,
    Revocation(tilo_user_identity::RevocationError),
    RoomRevoked,
    DelegateInactive(crate::state::delegates::DelegateInactive),
    NoMembership,
    Read,
    Capacity,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Certificate(e) => e.fmt(f),
            Self::LegacyJson(e) => f.write_str(e),
            Self::Revocation(e) => e.fmt(f),
            Self::GovernanceMissing => f.write_str("room governance missing"),
            Self::Absent => f.write_str("device is absent from governed roster"),
            Self::CertificateMismatch => f.write_str("governed certificate mismatch"),
            Self::RevocationRoot => f.write_str("revocation root mismatch"),
            Self::RoomRevoked => f.write_str("device revoked by room"),
            Self::DelegateInactive(reason) => reason.fmt(f),
            Self::NoMembership => f.write_str("person has no room membership"),
            Self::Read => f.write_str("governed membership read failed"),
            Self::Decode(e) => write!(f, "governed membership decode refusal: {e:?}"),
            Self::Capacity => f.write_str("governed membership exceeds its limit"),
        }
    }
}
trait Policy {
    fn owner(&self) -> Option<&str>;
    fn members(&self) -> &BTreeMap<String, Role>;
    fn grants(&self) -> &[Grant];
    fn identity(&self) -> &GovernedIdentity;
    /// The role `person` acts with in `room`: none for a stranger or a
    /// blocked member.
    fn role(&self, room: &str, person: &str) -> Option<Role> {
        if self.owner() == Some(person) {
            return Some(Role::Owner);
        }
        let role = self.members().get(person).copied()?;
        let subject = Subject::Person(person.to_owned());
        let node = tilo_action_policy::TreePath::room(room);
        let blocked = self.grants().iter().any(|grant| {
            grant.subject == subject
                && grant.node == node
                && grant.answer == tilo_action_policy::Answer::Deny
                && grant.kinds == tilo_action_policy::KindSet::ALL
        });
        (!blocked).then_some(role)
    }
}
fn key_text(key: &[u8; 32]) -> KeyHex {
    KeyHex::new(key)
}
/// A current member's role, person and policy, and whether their device is
/// held to the foreign-device bound in this room.
struct Current<P> {
    role: Role,
    person: KeyHex,
    policy: P,
    foreign: bool,
    /// The bound on a delegate's device.
    delegate: Option<Bound>,
}
fn current<'peer, S: Storage, P: Policy>(
    peer: &'peer PeerCore<S>,
    source: &[u8; 32],
    now: SystemTime,
    collect: impl FnOnce(&'peer S::Room) -> Result<P, Error>,
) -> Result<Current<P>, Error> {
    let cert = peer
        .certificate_typed(source, now)
        .map_err(Error::Certificate)?;
    let room = peer.room_doc.as_ref().ok_or(Error::GovernanceMissing)?;
    let policy = collect(room)?;
    let identity = policy.identity();
    if identity.device_cert != *cert {
        return Err(Error::CertificateMismatch);
    }
    let mut revoked = false;
    for entry in &identity.revocations {
        if entry.user_public_key != cert.user_public_key {
            return Err(Error::RevocationRoot);
        }
        // The historical temporary union only served is_revoked(source).
        // Verify EVERY row, in the same order, including duplicate targets and
        // rows after a matching target. No cloned revocation map is necessary.
        tilo_user_identity::verify_revocation_entry(entry).map_err(Error::Revocation)?;
        revoked |= &entry.revoked_device_public_key == source;
    }
    if revoked {
        return Err(Error::RoomRevoked);
    }
    identity
        .delegate_acting(&peer.room, crate::state::now_ns(now))
        .map_err(Error::DelegateInactive)?;
    let person = key_text(&cert.user_public_key);
    let role = policy
        .role(&peer.room, person.as_str())
        .ok_or(Error::NoMembership)?;
    let foreign = foreign(cert, &peer.authority);
    // A delegate acts with its person's authority bounded by its own: an
    // agent by the agent preset, a bot by its derived grant in the room's
    // policy. Neither governs the room, and neither rises above an Editor.
    let delegate = identity
        .delegate
        .as_ref()
        .map(|row| match &row.delegation.delegate {
            Delegate::Agent { agent_id } => {
                tilo_action_policy::roles::agent_delegation(&peer.room, agent_id, person.as_str())
            }
            Delegate::Bot { installation } => tilo_action_policy::bot::bound(installation),
        });
    let role = if delegate.is_some() {
        crate::state::delegates::agent_role(role)
    } else {
        role
    };
    Ok(Current {
        role,
        person,
        policy,
        foreign,
        delegate,
    })
}
/// Whether `cert`'s device is held to the foreign-device bound in the room
/// `governor` governs: a device that is not the person's own, unless it is the
/// governing device itself.
pub(in crate::state) fn foreign(
    cert: &tilo_user_identity::DeviceCert,
    governor: &tilo_user_identity::DeviceCert,
) -> bool {
    cert.capabilities.foreign && cert.device_public_key != governor.device_public_key
}
struct LegacyPolicy<'a> {
    data: &'a crate::cached_document::PolicyData,
    identity: &'a GovernedIdentity,
}
impl Policy for LegacyPolicy<'_> {
    fn owner(&self) -> Option<&str> {
        self.data.owner.as_deref()
    }
    fn members(&self) -> &BTreeMap<String, Role> {
        &self.data.members
    }
    fn grants(&self) -> &[Grant] {
        &self.data.grants
    }
    fn identity(&self) -> &GovernedIdentity {
        self.identity
    }
}
pub(in crate::state) fn legacy(
    peer: &PeerCore<LegacyNative>,
    source: &[u8; 32],
    now: SystemTime,
) -> Result<(Role, Option<Bound>), Error> {
    current(peer, source, now, |room| {
        let data = room.policy();
        let key = key_text(source);
        // Decoded once per document state, not on every lookup.
        let identity = room
            .identities()
            .get(key.as_str())
            .ok_or(Error::Absent)?
            .as_ref()
            .map_err(|error| Error::LegacyJson(error.clone()))?;
        Ok(LegacyPolicy { data, identity })
    })
    .map(|current| (current.role, current.delegate))
}
/// Current member data; no owner/transport credential.
pub struct PeerMember<'peer> {
    role: Role,
    person: KeyHex,
    policy: policy::NativePolicy,
    foreign: bool,
    delegate: Option<Bound>,
    room: &'peer str,
}
impl PeerMember<'_> {
    /// Verified current member root; data rather than a caller credential.
    pub fn user_root(&self) -> [u8; 32] {
        self.policy.identity().device_cert.user_public_key
    }
    pub fn role(&self) -> Role {
        self.role
    }
    pub fn person(&self) -> &str {
        self.person.as_str()
    }
    /// The room's grants as admitted.
    pub fn grants(&self) -> &[Grant] {
        self.policy.grants()
    }
    /// Whether the member's device is held to the foreign-device bound.
    pub fn foreign(&self) -> bool {
        self.foreign
    }
    /// The room's authority as admitted.
    pub fn room_policy(&self) -> RoomPolicy {
        RoomPolicy {
            room: self.room.to_owned(),
            owner: self.policy.owner().map(str::to_owned),
            members: self.policy.members().clone(),
            grants: self.policy.grants().to_vec(),
        }
    }
    /// The member's authority over `policy` (from [`Self::room_policy`]),
    /// held to `foreign_bound` on a foreign device.
    pub fn authority<'a>(
        &'a self,
        policy: &'a RoomPolicy,
        foreign_bound: &'a [Grant],
    ) -> Authority<'a> {
        let authority = policy.person_on_device(self.person.as_str(), self.foreign, foreign_bound);
        match &self.delegate {
            Some(bound) => authority.bounded_by(bound.party(&policy.grants)),
            None => authority,
        }
    }
    /// The bound on the member when its device is a delegate: an agent or
    /// an installed bot.
    pub fn delegate(&self) -> Option<&Bound> {
        self.delegate.as_ref()
    }
}
pub(in crate::state) fn bounded(
    peer: &PeerCore<BoundedNative>,
    source: [u8; 32],
    now: SystemTime,
    limits: PeerLimits,
) -> Result<PeerMember<'_>, BoundedPeerError> {
    let current = current(peer, &source, now, |room| {
        policy::collect(&room.read(), &source, limits)
    })
    .map_err(|error| match error {
        Error::Capacity => BoundedPeerError::Capacity,
        Error::Decode(e) => BoundedPeerError::Decode(e),
        Error::Certificate(_) | Error::CertificateMismatch => {
            BoundedPeerError::Invalid(BoundedPeerInvalid::Certificate)
        }
        _ => BoundedPeerError::Invalid(BoundedPeerInvalid::Governance),
    })?;
    Ok(PeerMember {
        role: current.role,
        person: current.person,
        policy: current.policy,
        foreign: current.foreign,
        delegate: current.delegate,
        room: &peer.room,
    })
}
#[cfg(test)]
mod tests;
