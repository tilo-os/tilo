//! Signature-verified enrollment union, not authority.
use super::{BoundedPeerError, BoundedPeerInvalid, RevocationList, backend};
use tilo_user_identity::RevocationEntry;

/// A verified enrollment union. This does not bind its roots to an admitted
/// roster or prove a caller/owner/transport identity; restore still performs
/// those root and pinned-authority checks.
pub struct PeerRevocations {
    inner: backend::revocations::BoundedRevocations,
}
impl PeerRevocations {
    pub fn list(&self) -> &RevocationList {
        self.inner.list()
    }
    /// Verify every signature and form the earliest-entry union. No roster
    /// filter, authority or domain is inferred.
    ///
    /// # Errors
    ///
    /// Returns a [`BoundedPeerError`] if a signature does not verify.
    pub fn verified(entries: &[RevocationEntry]) -> Result<Self, BoundedPeerError> {
        let inner = backend::revocations::BoundedRevocations::verified(entries)
            .map_err(|_| BoundedPeerError::Invalid(BoundedPeerInvalid::Revocation))?;
        Ok(Self { inner })
    }
}
#[cfg(test)]
mod tests;
