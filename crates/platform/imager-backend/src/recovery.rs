//! Native recovery admission and physical-storage validation.
pub mod capacity;
pub mod hardware;
pub mod write_guard;
use crate::plan::{self, Plan, PlanError, Requirements};

/// Recompute a retained plan and require the reviewed placement to remain unchanged.
///
/// # Errors
/// Refuses unsupported geometry, insufficient capacity or a plan that differs from recomputation.
pub fn admit_retained(plan: &Plan, requirements: Requirements) -> Result<(), PlanError> {
    let mut expected = plan::compute(&plan.basis, requirements)?;
    // UUIDs are minted once per review, not placement inputs.
    expected
        .staging_esp
        .partition_guid
        .clone_from(&plan.staging_esp.partition_guid);
    expected
        .staging_root
        .partition_guid
        .clone_from(&plan.staging_root.partition_guid);
    if expected != *plan {
        return Err(PlanError::Unsupported(
            "Storage changed after review; review capacity again. The original system has not been removed.".into(),
        ));
    }
    Ok(())
}
