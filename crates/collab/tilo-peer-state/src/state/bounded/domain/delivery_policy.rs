//! Same-peer current route checks feeding an inline delivery data projection.
use super::{BoundedPeerError, DeviceCert, PeerRead, SystemTime, backend};
pub use backend::delivery_policy::{PeerDeliveryPolicy, PeerDeliveryPolicyError};
impl<'peer> PeerRead<'peer> {
    /// Computes the historical delivery digest with every actual route read
    /// check. Parent retains current authenticated host/session, lock and clock.
    /// Arguments are host then device; neither becomes caller authority here.
    pub fn delivery_policy(
        &self,
        workspace: u64,
        host: &DeviceCert,
        device: &DeviceCert,
        now: SystemTime,
    ) -> Result<PeerDeliveryPolicy<'peer>, PeerDeliveryPolicyError> {
        backend::delivery_policy::bounded(
            &self.peer.core,
            self.limits,
            workspace,
            host,
            device,
            || {
                use crate::room::delivery_policy::Error;
                for route in self.peer.core.documents.keys() {
                    #[cfg(test)]
                    ROUTE_CHECKS.with(|n| n.set(n.get().saturating_add(1)));
                    match self.document_read_allowed(
                        device.device_public_key,
                        &route.app,
                        &route.record,
                        now,
                    ) {
                        Ok(true) => (),
                        Ok(false) | Err(BoundedPeerError::Invalid(_)) => {
                            return Err(Error::CurrentDenied);
                        }
                        Err(BoundedPeerError::Capacity) => return Err(Error::Capacity),
                        Err(BoundedPeerError::Decode(error)) => return Err(Error::Decode(error)),
                    }
                }
                Ok(())
            },
        )
    }
}
#[cfg(test)]
thread_local! {static ROUTE_CHECKS:std::cell::Cell<usize>=const{std::cell::Cell::new(0)};}
#[cfg(test)]
mod tests;
