//! Signature-verified enrollment union with retained backing, not authority.
use super::*;
use tilo_user_identity::RevocationEntry;

/// Retains the verified enrollment union's allocation credit. This does not bind
/// its roots to an admitted roster or prove a caller/owner/transport identity.
/// The actual restore still performs those root and pinned-authority checks.
pub struct PeerRevocations {
    inner: backend::revocations::BoundedRevocations,
}
impl PeerRevocations {
    /// Borrow the original verified union without extracting its owned backing.
    pub fn list(&self) -> &RevocationList {
        self.inner.list()
    }
}
impl PeerWork<'_> {
    /// Verify every original signature and form the earliest-entry union using
    /// this exact turn. Input rows remain caller-owned; output credit stays with
    /// the returned holder. No roster filter, authority or domain is inferred.
    pub fn verified_revocations(
        &self,
        entries: &[RevocationEntry],
    ) -> Result<PeerRevocations, BoundedPeerError> {
        let inner = backend::revocations::BoundedRevocations::verified(entries, self.turn)
            .map_err(|error| match error {
                backend::revocations::RestoreRevocationsError::Resource(error) => {
                    BoundedPeerError::Resource(error)
                }
                _ => BoundedPeerError::Invalid(BoundedPeerInvalid::Revocation),
            })?;
        Ok(PeerRevocations { inner })
    }
}
#[cfg(test)]
mod tests;
