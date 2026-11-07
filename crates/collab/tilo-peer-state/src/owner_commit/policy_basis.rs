//! Legacy allocating policy data, never owner provenance or resource qualification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrivatePolicyBasis {
    revision: String,
    expiries: Vec<u64>,
}
impl PrivatePolicyBasis {
    /// Preserves the historical strict capability/expiry checks and revision.
    /// This legacy output carries no allocation, owner, caller or clock witness.
    pub fn from_peer(peer: &crate::StatePeer) -> Result<Self, String> {
        let (revision, expiries) = crate::legacy_policy::private_policy_basis(peer)?;
        Ok(Self { revision, expiries })
    }
    pub fn revision(&self) -> &str {
        &self.revision
    }
}

pub use crate::legacy_policy::{canonical_owner, governance_revision};
