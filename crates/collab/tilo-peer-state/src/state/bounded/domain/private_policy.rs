//! Retained exact private policy data, not a caller or current-owner credential.
use super::{BoundedPeerError, PeerRead, backend};
/// Paid revision/expiry output from the actual bounded current peer. No owned
/// backing can be extracted or cloned away from its original reservation.
/// ```
/// use tilo_peer_state::PeerPolicyBasis;
/// fn data(value: &PeerPolicyBasis) -> (&str, Option<u64>) {
///     (value.revision(), value.next_expiry_after(0))
/// }
/// ```
/// ```compile_fail
/// use tilo_peer_state::PeerPolicyBasis;
/// fn duplicate(value: PeerPolicyBasis) -> PeerPolicyBasis { value.clone() }
/// ```
pub struct PeerPolicyBasis {
    value: crate::room::private_policy::NativeBasis,
}
impl PeerPolicyBasis {
    pub fn revision(&self) -> &str {
        self.value.revision()
    }
    pub fn next_expiry_after(&self, unix_seconds: u64) -> Option<u64> {
        self.value.next_expiry_after(unix_seconds)
    }
}
impl PeerRead<'_> {
    /// Retains the same strict persistent capability checks and exact revision
    /// as the legacy private basis. The parent still binds caller, lock, clock,
    /// generation and publication lineage. This does not authorize an action.
    ///
    /// # Errors
    ///
    /// Returns a [`BoundedPeerError`] if a persistent capability is malformed or a limit
    /// is reached.
    pub fn private_policy_basis(&self) -> Result<PeerPolicyBasis, BoundedPeerError> {
        Ok(PeerPolicyBasis {
            value: backend::private_policy::bounded(&self.peer.core, self.limits)?,
        })
    }
}
#[cfg(test)]
mod tests;
