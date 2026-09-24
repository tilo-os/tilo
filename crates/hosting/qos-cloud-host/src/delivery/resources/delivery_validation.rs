//! Retained delivery semantics against an actual peer; not an owner credential.
use super::HeldCloudState;
use crate::delivery::validation::{self, Admissions, CheckError};
use qos_peer_state::{BoundedPeerError, BoundedStatePeer, PeerWork};
use qos_relay::delivery_v3::commit::MAX_CONSUMERS;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeliveryStateFailure {
    Capacity,
    Denied,
}
impl HeldCloudState {
    pub(crate) fn validate_delivery(
        &self,
        peer: &BoundedStatePeer,
        work: &PeerWork<'_>,
    ) -> Result<(), DeliveryStateFailure> {
        let state = self.state();
        let Some(delivery) = validation::selected(state).map_err(denied)? else {
            return Ok(());
        };
        if delivery.ledger.consumers.len() > MAX_CONSUMERS {
            return Err(DeliveryStateFailure::Denied);
        }
        let scratch = delivery
            .ledger
            .consumers
            .len()
            .checked_mul(size_of::<super::ConsumerNode>())
            .ok_or(DeliveryStateFailure::Capacity)?;
        {
            // The duplicate-consumer set dies inside structure before this
            // reservation. Release it before allocating the policy projection.
            let _scratch = work.reserve_output(scratch).map_err(peer_failure)?;
            validation::structure(delivery).map_err(denied)?;
        }
        validation::barrier(state, delivery).map_err(denied)?;
        let read = peer.read(work).map_err(peer_failure)?;
        validation::selected_v5(read.admission_v5_selected()).map_err(denied)?;
        let basis = read.private_policy_basis().map_err(peer_failure)?;
        validation::revision(delivery, basis.revision()).map_err(denied)?;
        validation::replay(state, delivery, Admissions::Bounded(&read)).map_err(denied)
    }
}
fn denied(_: CheckError) -> DeliveryStateFailure {
    DeliveryStateFailure::Denied
}
fn peer_failure(error: BoundedPeerError) -> DeliveryStateFailure {
    use automerge::resource_limits::NativeResourceError as Native;
    use qos_peer_state::resources::PeerResourceError;
    match error {
        BoundedPeerError::Resource(
            Native::NormalizedBytes
            | Native::Operations
            | Native::Collection
            | Native::ValueBytes
            | Native::WorkBytes,
        )
        | BoundedPeerError::Decode(PeerResourceError::Capacity) => DeliveryStateFailure::Capacity,
        _ => DeliveryStateFailure::Denied,
    }
}
