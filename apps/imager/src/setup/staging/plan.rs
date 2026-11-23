//! Compatibility facade for the portable staging planner.

pub use imager_backend::plan::{
    Basis, BootEntry, Extent, FirmwareFacts, HostOs, Plan, PlanError, Requirements, Shrink,
    StagedPartition, TargetBounds, compute, minimum_linux_btrfs_bytes, minimum_system_volume_bytes,
    system_volume_reserve, target_bounds, with_loader_path,
};

#[cfg(test)]
pub(crate) use imager_backend::plan::tests;
