//! One current-member algorithm over actual admitted room data.
use super::{
    BoundedNative, CapOverride, CertificateLookupError, GovernedIdentity, LegacyNative, PeerCore,
    Principal, Role, Scope, Storage, Subject, SystemTime, now_secs, room,
};
use crate::room::key_hex::KeyHex;
use crate::state::bounded::{BoundedPeerError, BoundedPeerInvalid, PeerLimits};
mod policy;

#[derive(Debug)]
pub(in crate::state) enum Error {
    Certificate(CertificateLookupError),
    GovernanceMissing,
    Absent,
    LegacyJson(serde_json::Error),
    Decode(crate::resources::PeerResourceError),
    CertificateMismatch,
    RevocationRoot,
    Revocation(tilo_user_identity::RevocationError),
    RoomRevoked,
    NoMembership,
    Read,
    Capacity,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Certificate(e) => e.fmt(f),
            Self::LegacyJson(e) => e.fmt(f),
            Self::Revocation(e) => e.fmt(f),
            Self::GovernanceMissing => f.write_str("room governance missing"),
            Self::Absent => f.write_str("device is absent from governed roster"),
            Self::CertificateMismatch => f.write_str("governed certificate mismatch"),
            Self::RevocationRoot => f.write_str("revocation root mismatch"),
            Self::RoomRevoked => f.write_str("device revoked by room"),
            Self::NoMembership => f.write_str("person has no room membership"),
            Self::Read => f.write_str("governed membership read failed"),
            Self::Decode(e) => write!(f, "governed membership decode refusal: {e:?}"),
            Self::Capacity => f.write_str("governed membership exceeds its limit"),
        }
    }
}
trait Policy {
    fn owner(&self) -> Option<&str>;
    fn caps(&self) -> &[CapOverride];
    fn identity(&self) -> &GovernedIdentity;
    fn role(&self, person: &str, now: u64) -> Option<Role> {
        room::role_of(self.owner(), self.caps(), person, now)
    }
}
fn key_text(key: &[u8; 32]) -> KeyHex {
    KeyHex::new(key)
}
fn current<'peer, S: Storage, P: Policy>(
    peer: &'peer PeerCore<S>,
    source: &[u8; 32],
    now: SystemTime,
    collect: impl FnOnce(&'peer S::Room) -> Result<P, Error>,
) -> Result<(Role, KeyHex, P), Error> {
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
    let person = key_text(&cert.user_public_key);
    let mut role = policy
        .role(person.as_str(), now_secs(now))
        .ok_or(Error::NoMembership)?;
    if capped(cert, &peer.authority) {
        role =
            collab_types::foreign_device::foreign_device_role(role, policy.caps(), now_secs(now));
    }
    Ok((role, person, policy))
}
/// Whether `cert`'s device is held to the foreign-device cap in the room
/// `governor` governs.
fn capped(
    cert: &tilo_user_identity::DeviceCert,
    governor: &tilo_user_identity::DeviceCert,
) -> bool {
    collab_types::foreign_device::cap_applies(
        cert.capabilities.foreign,
        cert.device_public_key == governor.device_public_key,
    )
}
/// Adds the foreign-device restriction to the capabilities `cert`'s device is
/// decided under in the room `governor` governs; otherwise the capabilities
/// are left as they are.
pub(in crate::state) fn restrict_foreign_device(
    cert: &tilo_user_identity::DeviceCert,
    governor: &tilo_user_identity::DeviceCert,
    person: &str,
    caps: &mut Vec<CapOverride>,
    now: SystemTime,
) {
    if capped(cert, governor) {
        let restriction =
            collab_types::foreign_device::foreign_device_restriction(person, caps, now_secs(now));
        caps.push(restriction);
    }
}
struct LegacyPolicy<'a> {
    data: &'a crate::cached_document::PolicyData,
    identity: GovernedIdentity,
}
impl Policy for LegacyPolicy<'_> {
    fn owner(&self) -> Option<&str> {
        self.data.owner.as_deref()
    }
    fn caps(&self) -> &[CapOverride] {
        &self.data.caps
    }
    fn identity(&self) -> &GovernedIdentity {
        &self.identity
    }
}
pub(in crate::state) fn legacy(
    peer: &PeerCore<LegacyNative>,
    source: &[u8; 32],
    now: SystemTime,
) -> Result<Role, Error> {
    current(peer, source, now, |room| {
        let data = room.policy();
        let key = key_text(source);
        let bytes = data.identities.get(key.as_str()).ok_or(Error::Absent)?;
        let identity = serde_json::from_slice(bytes).map_err(Error::LegacyJson)?;
        Ok(LegacyPolicy { data, identity })
    })
    .map(|(role, _, _)| role)
}
/// Current member data; no owner/transport credential.
pub struct PeerMember<'peer> {
    role: Role,
    person: KeyHex,
    policy: policy::NativePolicy,
    _peer: &'peer PeerCore<BoundedNative>,
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
    pub fn capabilities(&self) -> &[CapOverride] {
        self.policy.caps()
    }
}
pub(in crate::state) fn bounded(
    peer: &PeerCore<BoundedNative>,
    source: [u8; 32],
    now: SystemTime,
    limits: PeerLimits,
) -> Result<PeerMember<'_>, BoundedPeerError> {
    let (role, person, mut policy) = current(peer, &source, now, |room| {
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
    let cert = policy.identity().device_cert.clone();
    restrict_foreign_device(
        &cert,
        &peer.authority,
        person.as_str(),
        policy.caps_mut(),
        now,
    );
    Ok(PeerMember {
        role,
        person,
        policy,
        _peer: peer,
    })
}
#[cfg(test)]
mod tests;
