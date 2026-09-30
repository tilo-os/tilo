//! Legacy allocating policy data, never owner provenance or resource qualification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrivatePolicyBasis {
    revision: String,
    expiries: Vec<u64>,
}
impl PrivatePolicyBasis {
    /// Preserves the historical strict capability/expiry checks and revision.
    /// This legacy output carries no allocation, owner, caller or clock witness.
    ///
    /// # Errors
    ///
    /// Returns an error if room governance is missing or malformed.
    pub fn from_peer(peer: &crate::StatePeer) -> Result<Self, String> {
        let basis = peer.legacy_private_policy_basis()?;
        Ok(Self {
            revision: basis.revision,
            expiries: basis.expiries,
        })
    }
    pub fn revision(&self) -> &str {
        &self.revision
    }
    pub fn next_expiry_after(&self, unix_seconds: u64) -> Option<u64> {
        self.expiries.iter().copied().find(|at| *at > unix_seconds)
    }
}

/// The canonical owner of `peer`'s room.
///
/// # Errors
///
/// Returns an error if room governance is missing or malformed.
pub fn canonical_owner(peer: &crate::StatePeer) -> Result<String, String> {
    peer.legacy_canonical_owner()
}
/// Opaque policy identity. Deliberately excludes aggregate generation, room
/// heads, document coverage and runtime task/output progress.
///
/// # Errors
///
/// Returns an error if room governance is missing or malformed.
pub fn governance_revision(peer: &crate::StatePeer) -> Result<String, String> {
    peer.legacy_governance_revision()
}
