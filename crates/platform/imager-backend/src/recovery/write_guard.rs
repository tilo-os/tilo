//! Conservative filesystem-to-write-range admission. Unknown mappings fail closed.
use super::hardware as destination;
use crate::plan::Plan;
use std::{io, path::Path};

#[cfg(target_os = "linux")]
pub fn validate(source: &Path, destination: &Path, plan: &Plan) -> io::Result<()> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let meta = std::fs::metadata(&plan.basis.disk.id)?;
    if !meta.file_type().is_block_device() {
        return Err(io::Error::other(
            "Write target has no physical block mapping",
        ));
    }
    let disk = std::fs::canonicalize(format!(
        "/sys/dev/block/{}:{}",
        libc::major(meta.rdev()),
        libc::minor(meta.rdev())
    ))?;
    if disk.join("partition").exists() || std::fs::read_dir(disk.join("slaves"))?.next().is_some() {
        return Err(io::Error::other(
            "Write target physical mapping is unsupported",
        ));
    }
    let hardware = destination::hardware(&disk)
        .ok_or_else(|| io::Error::other("Write target hardware identity is unavailable"))?;
    for path in [
        source,
        destination
            .parent()
            .ok_or_else(|| io::Error::other("Recovery directory missing"))?,
    ] {
        let physical = destination::physical(path)
            .ok_or_else(|| io::Error::other("Recovery write-range mapping is unverified"))?;
        if physical.independent_of(&hardware) {
            continue;
        }
        if !physical.same_device(&hardware) {
            return Err(io::Error::other(
                "Physical storage independence is unverified",
            ));
        }
        // Same physical disk is not independent, but an unchanged partition
        // outside every write range may be retained on the alongside path.
        partition_outside(path, plan, &disk)?;
    }
    Ok(())
}
#[cfg(target_os = "linux")]
fn partition_outside(path: &Path, plan: &Plan, disk: &Path) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    let dev = std::fs::metadata(path)?.dev();
    let sys = std::fs::canonicalize(format!(
        "/sys/dev/block/{}:{}",
        libc::major(dev),
        libc::minor(dev)
    ))?;
    if !sys.join("partition").exists() {
        return Err(io::Error::other(
            "Recovery is on the write target without a retained partition boundary",
        ));
    }
    if sys.parent() != Some(disk) {
        return Err(io::Error::other(
            "Shared hardware has an unverified namespace mapping",
        ));
    }
    let number = |name| -> io::Result<u64> {
        std::fs::read_to_string(sys.join(name))?
            .trim()
            .parse()
            .map_err(io::Error::other)
    };
    let first = number("start")?
        .checked_mul(512)
        .ok_or_else(|| io::Error::other("Partition range overflow"))?;
    let end = first
        .checked_add(
            number("size")?
                .checked_mul(512)
                .ok_or_else(|| io::Error::other("Partition range overflow"))?,
        )
        .ok_or_else(|| io::Error::other("Partition range overflow"))?;
    let sector = plan.basis.disk.sector_bytes;
    for (a, b) in [
        (plan.target_extent.first_lba, plan.target_extent.last_lba),
        (plan.staging_esp.first_lba, plan.staging_esp.last_lba),
        (plan.staging_root.first_lba, plan.staging_root.last_lba),
    ] {
        let start = a
            .checked_mul(sector)
            .ok_or_else(|| io::Error::other("Write range overflow"))?;
        let stop = b
            .checked_add(1)
            .and_then(|v| v.checked_mul(sector))
            .ok_or_else(|| io::Error::other("Write range overflow"))?;
        if first < stop && start < end {
            return Err(io::Error::other(
                "Recovery source or destination overlaps actual installation write extents",
            ));
        }
    }
    if let Some(shrink) = &plan.shrink {
        if plan.basis.disk.partitions.iter().any(|p| {
            p.guid == shrink.partition_guid && p.first_lba.checked_mul(sector) == Some(first)
        }) {
            return Err(io::Error::other(
                "Recovery must not depend on a filesystem being resized",
            ));
        }
    }
    Ok(())
}
#[cfg(not(target_os = "linux"))]
pub fn validate(_: &Path, _: &Path, _: &Plan) -> io::Result<()> {
    Err(io::Error::other(
        "Native recovery write-range mapping is unavailable",
    ))
}
