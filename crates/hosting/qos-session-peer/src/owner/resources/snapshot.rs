//! Actual owner-root binding for opaque snapshot sidecars, not pin authority.
use super::*;
use crate::store::{HostWorkFailure, HostWorkInventory, SnapshotDirectory, SnapshotReservation};
fn failure(error: HostWorkFailure) -> OwnerDataFailure {
    OwnerDataFailure::new(
        match error {
            HostWorkFailure::Capacity => Failure::Capacity(Capacity::Disk),
            HostWorkFailure::Conflict => Failure::CurrentDenied,
            HostWorkFailure::Unavailable => Failure::Unavailable,
        },
        error,
    )
}
impl SessionOwner {
    pub(crate) fn snapshot_directory(
        &self,
        inventory: &HostWorkInventory,
    ) -> DataResult<SnapshotDirectory> {
        self.require_delivery()
            .map_err(|error| self.classify_data_error(error))?;
        if !self
            .resources
            .as_ref()
            .is_some_and(|r| inventory.same_pool(&r.budget))
        {
            return Err(OwnerDataFailure::new(
                Failure::Unavailable,
                anyhow::anyhow!("snapshot requires original attached host inventory"),
            ));
        }
        self.store
            .snapshot_directory(inventory, self.workspace_id)
            .map_err(failure)
    }
    pub(crate) fn validate_snapshot_reservation(
        &self,
        reservation: &SnapshotReservation,
    ) -> DataResult<()> {
        self.require_delivery()
            .map_err(|error| self.classify_data_error(error))?;
        if !self
            .resources
            .as_ref()
            .is_some_and(|r| reservation.same_pool(&r.budget))
            || !self
                .store
                .matches_snapshot_reservation(reservation, self.workspace_id)
                .map_err(failure)?
        {
            return Err(OwnerDataFailure::new(
                Failure::CurrentDenied,
                anyhow::anyhow!("snapshot reservation does not match actual owner root and pool"),
            ));
        }
        Ok(())
    }
}
