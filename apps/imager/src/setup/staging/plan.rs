//! Compatibility facade for the portable staging planner.

pub use imager_backend::plan::{
    Basis, BootEntry, Extent, FirmwareFacts, HostOs, Plan, PlanError, Requirements, Shrink,
    StagedPartition, TargetBounds, compute, minimum_linux_btrfs_bytes, minimum_system_volume_bytes,
    system_volume_reserve, target_bounds, with_loader_path,
};

#[cfg(test)]
use super::platform::Disk;
#[cfg(all(test, feature = "installation"))]
use super::platform::SystemVolume;

#[cfg(test)]
pub(crate) mod tests {
    use super::super::platform::Partition;
    use super::*;

    #[cfg(feature = "installation")]
    const MIB: u64 = 1 << 20;
    #[cfg(feature = "installation")]
    const GIB: u64 = 1 << 30;
    #[cfg(feature = "installation")]
    const BASIC: &str = "ebd0a0a2-b9e5-4433-87c0-68b6b72699c7";

    pub(crate) fn firmware() -> FirmwareFacts {
        FirmwareFacts {
            uefi: true,
            secure_boot: false,
            boot_current: Some("0001".into()),
            boot_order: vec!["0001".into(), "0000".into()],
            boot_next: None,
        }
    }

    #[cfg(feature = "installation")]
    pub(crate) fn part(number: u32, guid: &str, first_lba: u64, last_lba: u64) -> Partition {
        Partition {
            number,
            guid: guid.into(),
            type_guid: BASIC.into(),
            first_lba,
            last_lba,
        }
    }

    pub(crate) fn disk(size_bytes: u64, sector: u64, partitions: Vec<Partition>) -> Disk {
        let total = size_bytes / sector;
        let tail = if sector == 512 { 34 } else { 6 };
        Disk {
            id: "/dev/test".into(),
            guid: "11111111-2222-4333-8444-555555555555".into(),
            size_bytes,
            sector_bytes: sector,
            first_usable_lba: tail,
            last_usable_lba: total - tail,
            partitions,
        }
    }

    #[cfg(feature = "installation")]
    pub(crate) const C_GUID: &str = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";

    /// ESP 100 MiB, MSR 16 MiB, C: to `c_end_gib`, optional WinRE right after C:.
    #[cfg(feature = "installation")]
    pub(crate) fn windows_basis(
        disk_gib: u64,
        c_end_gib: u64,
        used_gib: u64,
        winre: bool,
    ) -> Basis {
        let s = 512;
        let mib = MIB / s;
        let mut parts = vec![
            part(
                1,
                "e0000000-0000-4000-8000-000000000001",
                mib,
                101 * mib - 1,
            ),
            part(
                2,
                "e0000000-0000-4000-8000-000000000002",
                101 * mib,
                117 * mib - 1,
            ),
        ];
        let c_first = 117 * mib;
        let c_last = c_end_gib * GIB / s - 1;
        parts.push(part(3, C_GUID, c_first, c_last));
        if winre {
            parts.push(part(
                4,
                "e0000000-0000-4000-8000-000000000004",
                c_last + 1,
                c_last + 700 * mib,
            ));
        }
        let size = (c_last - c_first + 1) * s;
        Basis {
            disk: disk(disk_gib * GIB, s, parts),
            system_volume: Some(SystemVolume {
                partition_guid: C_GUID.into(),
                size_bytes: size,
                supported_min_bytes: used_gib * GIB + GIB,
                used_bytes: used_gib * GIB,
            }),
            linux_btrfs_volume: None,
            firmware: firmware(),
        }
    }

    pub(crate) fn linux_basis(sector: u64, disk_bytes: u64, parts: Vec<Partition>) -> Basis {
        Basis {
            disk: disk(disk_bytes, sector, parts),
            system_volume: None,
            linux_btrfs_volume: None,
            firmware: firmware(),
        }
    }
}
