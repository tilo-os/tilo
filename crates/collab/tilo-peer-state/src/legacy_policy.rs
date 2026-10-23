//! The legacy allocating private policy reads of an admitted room, one narrow
//! function each, for callers above the admission core. They carry no owner
//! provenance or resource qualification.
use crate::StatePeer;

/// The governance revision and the ascending capability expiries of `peer`'s
/// room, with the historical strict capability and expiry checks.
pub fn private_policy_basis(peer: &StatePeer) -> Result<(String, Vec<u64>), String> {
    let basis = peer.legacy_private_policy_basis()?;
    Ok((basis.revision, basis.expiries))
}

pub fn canonical_owner(peer: &StatePeer) -> Result<String, String> {
    peer.legacy_canonical_owner()
}

/// Opaque policy identity. Deliberately excludes aggregate generation, room
/// heads, document coverage and runtime task/output progress.
pub fn governance_revision(peer: &StatePeer) -> Result<String, String> {
    peer.legacy_governance_revision()
}
