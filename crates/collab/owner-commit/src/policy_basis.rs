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
    pub fn from_peer(peer: &tilo_peer_state::StatePeer) -> Result<Self, String> {
        let (revision, expiries) = tilo_peer_state::legacy_policy::private_policy_basis(peer)?;
        Ok(Self { revision, expiries })
    }
    pub fn revision(&self) -> &str {
        &self.revision
    }
    pub fn next_expiry_after(&self, unix_seconds: u64) -> Option<u64> {
        self.expiries.iter().copied().find(|at| *at > unix_seconds)
    }
}

pub use tilo_peer_state::legacy_policy::{canonical_owner, governance_revision};
