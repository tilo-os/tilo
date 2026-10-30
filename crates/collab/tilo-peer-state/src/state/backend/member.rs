//! One current-member algorithm over actual admitted room data.
use super::{CertificateLookupError, GovernedIdentity, PeerCore, Role, SystemTime};
use crate::room::key_hex::KeyHex;
use std::collections::BTreeMap;
use tilo_action_policy::{Bound, Grant, Subject};
use tilo_workspace_join::delegation::Delegate;

#[derive(Debug)]
pub(in crate::state) enum Error {
    Certificate(CertificateLookupError),
    GovernanceMissing,
    Absent,
    LegacyJson(String),
    CertificateMismatch,
    RevocationRoot,
    Revocation(tilo_user_identity::RevocationError),
    RoomRevoked,
    DelegateInactive(crate::state::delegates::DelegateInactive),
    NoMembership,
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
/// A current member's role, and the bound on it when its device is a
/// delegate.
fn current<'peer, P: Policy>(
    peer: &'peer PeerCore,
    source: &[u8; 32],
    now: SystemTime,
    collect: impl FnOnce(&'peer crate::cached_document::CachedDocument) -> Result<P, Error>,
) -> Result<(Role, Option<Bound>), Error> {
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
    Ok((role, delegate))
}
/// Whether `cert`'s device is held to the foreign-device bound: a device
/// that is not the person's own, unless the replica was made on it or joined
/// through it (`origin`).
pub(in crate::state) fn foreign(cert: &tilo_user_identity::DeviceCert, origin: &[u8; 32]) -> bool {
    cert.capabilities.foreign && cert.device_public_key != *origin
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
    peer: &PeerCore,
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
}
#[cfg(test)]
mod tests;
