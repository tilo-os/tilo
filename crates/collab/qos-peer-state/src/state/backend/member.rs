//! One current-member algorithm over actual admitted room data.
use super::*;
use crate::state::bounded::{BoundedPeerError, BoundedPeerInvalid, PeerLimits};
use automerge::resource_limits::{NativeReservation, NativeResourceError, NativeWorkTurn};
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
    Resource(NativeResourceError),
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
            Self::Resource(e) => write!(f, "governed membership resource refusal: {e:?}"),
        }
    }
}
trait Policy {
    fn owner(&self) -> Option<&str>;
    fn caps(&self) -> &[CapOverride];
    fn identity(&self) -> &GovernedIdentity;
    fn role(
        &self,
        person: &str,
        now: u64,
        _turn: Option<&NativeWorkTurn>,
    ) -> Result<Option<Role>, Error> {
        Ok(room::role_of(self.owner(), self.caps(), person, now))
    }
}
fn key_text(key: &[u8; 32]) -> [u8; 64] {
    let mut text = [0; 64];
    hex::encode_to_slice(key, &mut text).expect("fixed hex buffer");
    text
}
fn current<'peer, S: Storage, P: Policy>(
    peer: &'peer PeerCore<S>,
    source: &[u8; 32],
    now: SystemTime,
    turn: Option<&NativeWorkTurn>,
    collect: impl FnOnce(&'peer S::Room) -> Result<P, Error>,
) -> Result<(Role, [u8; 64], P), Error> {
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
    let role = policy
        .role(
            std::str::from_utf8(&person).expect("hex person"),
            now_secs(now),
            turn,
        )?
        .ok_or(Error::NoMembership)?;
    Ok((role, person, policy))
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
    current(peer, source, now, None, |room| {
        let data = room.policy();
        let key = key_text(source);
        let key = std::str::from_utf8(&key).expect("hex key");
        let bytes = data.identities.get(key).ok_or(Error::Absent)?;
        let identity = serde_json::from_slice(bytes).map_err(Error::LegacyJson)?;
        Ok(LegacyPolicy { data, identity })
    })
    .map(|(role, _, _)| role)
}
/// Current data with retained policy backing; no owner/transport credential.
/// Borrowed capabilities must be copied under the caller's own output credit.
pub struct PeerMember<'peer, 'work> {
    role: Role,
    person: [u8; 64],
    policy: policy::NativePolicy,
    _peer: &'peer PeerCore<BoundedNative>,
    _turn: &'work NativeWorkTurn,
}
impl PeerMember<'_, '_> {
    /// Verified current member root; data rather than a caller credential.
    pub fn user_root(&self) -> [u8; 32] {
        self.policy.identity().device_cert.user_public_key
    }
    pub fn role(&self) -> Role {
        self.role
    }
    pub fn person(&self) -> &str {
        std::str::from_utf8(&self.person).expect("hex person")
    }
    pub fn capabilities(&self) -> &[CapOverride] {
        self.policy.caps()
    }
}
pub(in crate::state) fn bounded<'peer, 'work>(
    peer: &'peer PeerCore<BoundedNative>,
    source: [u8; 32],
    now: SystemTime,
    turn: &'work NativeWorkTurn,
    limits: PeerLimits,
) -> Result<PeerMember<'peer, 'work>, BoundedPeerError> {
    turn.reserve(0).map_err(BoundedPeerError::Resource)?;
    let (role, person, policy) = current(peer, &source, now, Some(turn), |room| {
        let read = room.read(turn).map_err(Error::Resource)?;
        policy::collect(&read, &source, turn, limits)
    })
    .map_err(|error| match error {
        Error::Resource(e) => BoundedPeerError::Resource(e),
        Error::Decode(e) => BoundedPeerError::Decode(e),
        Error::Certificate(_) | Error::CertificateMismatch => {
            BoundedPeerError::Invalid(BoundedPeerInvalid::Certificate)
        }
        _ => BoundedPeerError::Invalid(BoundedPeerInvalid::Governance),
    })?;
    Ok(PeerMember {
        role,
        person,
        policy,
        _peer: peer,
        _turn: turn,
    })
}
#[cfg(test)]
mod tests;
