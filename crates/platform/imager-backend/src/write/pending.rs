//! Bind pending migration plans to native target identity before destructive I/O.
use super::{attest_target_descriptor, io, revalidate_target};
use crate::media_model::TargetDrive;
use std::os::unix::fs::{FileTypeExt, MetadataExt};

/// Revalidate a reviewed device before and after checking its independent sources.
///
/// # Errors
/// Refuses changed device numbers, non-block targets, or failed source checks.
pub fn validate_pending_source_disks(
    target: &TargetDrive,
    expected_number: u64,
    validate_sources: impl FnOnce(u64) -> io::Result<()>,
) -> io::Result<()> {
    revalidate_target(target)?;
    let held = std::fs::symlink_metadata(target.dev_path())?;
    if !held.file_type().is_block_device() || held.rdev() != expected_number {
        return Err(io::Error::other("Pending raw media identity changed"));
    }
    validate_sources(held.rdev())?;
    revalidate_target(target)
}

/// Bind the opened descriptor to the pending reviewed device and source plans.
///
/// # Errors
/// Refuses replaced descriptors, changed device numbers, or failed source checks.
pub fn validate_pending_descriptor(
    target: &TargetDrive,
    device: &std::fs::File,
    expected_number: u64,
    validate_sources: impl FnOnce(u64) -> io::Result<()>,
) -> io::Result<()> {
    attest_target_descriptor(target, device)?;
    let held = device.metadata()?;
    if !held.file_type().is_block_device() || held.rdev() != expected_number {
        return Err(io::Error::other(
            "Opened pending raw media identity changed",
        ));
    }
    validate_sources(held.rdev())
}
