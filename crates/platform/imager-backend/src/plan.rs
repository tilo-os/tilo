//! Pure placement: host facts + bundle sizes + chosen Tilo size → one reviewed
//! plan, or a refusal. No I/O. The helper recomputes `basis` from a fresh probe
//! and refuses when it differs (`plan-stale`).

use serde::{Deserialize, Serialize};

use super::platform::{Disk, LinuxBtrfsVolume, SystemVolume};
use super::{ALIGN_BYTES, MIN_TARGET_BYTES};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FirmwareFacts {
    pub uefi: bool,
    pub secure_boot: bool,
    pub boot_current: Option<String>,
    pub boot_order: Vec<String>,
    pub boot_next: Option<String>,
}

/// Exactly the facts a plan was computed from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Basis {
    pub disk: Disk,
    pub system_volume: Option<SystemVolume>,
    /// Separate from Windows facts; omitted None preserves legacy plan digests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linux_btrfs_volume: Option<LinuxBtrfsVolume>,
    pub firmware: FirmwareFacts,
}

pub use staging_handoff_model::{Extent, HostOs};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Shrink {
    pub partition_guid: String,
    pub from_bytes: u64,
    pub to_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StagedPartition {
    pub first_lba: u64,
    pub last_lba: u64,
    /// Pre-generated on Linux; `None` on Windows until creation records it.
    pub partition_guid: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BootEntry {
    pub description: String,
    /// Backslash-separated path inside the staging ESP.
    pub loader_path: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    pub basis: Basis,
    pub shrink: Option<Shrink>,
    pub target_extent: Extent,
    pub staging_esp: StagedPartition,
    pub staging_root: StagedPartition,
    pub boot_entry: BootEntry,
}

/// Bundle-derived sizes the plan must fit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Requirements {
    pub staging_esp_bytes: u64,
    pub staging_root_bytes: u64,
    pub target_bytes: u64,
}

/// Bounds the Review stepper may offer for the Tilo size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TargetBounds {
    pub min_bytes: u64,
    pub max_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlanError {
    /// Not enough space even at the maximum safe shrink; bytes short.
    InsufficientSpace {
        needed_bytes: u64,
        available_bytes: u64,
    },
    /// The disk or partition map is outside what this planner supports.
    Unsupported(String),
}

impl Plan {
    /// Canonical JSON (serde field order, no whitespace) and its SHA-256.
    ///
    /// # Errors
    /// Returns an error if the plan cannot be serialized.
    pub fn digest(&self) -> Result<String, serde_json::Error> {
        use sha2::{Digest, Sha256};
        let bytes = serde_json::to_vec(self)?;
        Ok(hex::encode(Sha256::digest(bytes)))
    }
}

impl Basis {
    /// Whether the host can choose a new partition's GUID at creation.
    ///
    /// Only Windows reports `system_volume`. Linux Btrfs facts are separate:
    /// Windows `New-Partition` cannot set a GUID; Linux `sfdisk` can.
    pub fn sets_partition_guids(&self) -> bool {
        self.system_volume.is_none()
    }

    /// The host, as written into the handoff.
    pub fn host_os(&self) -> HostOs {
        if self.system_volume.is_some() {
            HostOs::Windows
        } else {
            HostOs::Linux
        }
    }
}

impl StagedPartition {
    pub fn extent(&self) -> Extent {
        Extent {
            first_lba: self.first_lba,
            last_lba: self.last_lba,
        }
    }
}

/// Free space the system volume must keep after a shrink:
/// `max(20 GiB, 15 % of used)`.
pub fn system_volume_reserve(used_bytes: u64) -> u64 {
    (20_u64 << 30).max(
        (used_bytes / 100)
            .saturating_mul(15)
            .saturating_add((used_bytes % 100).saturating_mul(15) / 100),
    )
}

/// Smallest size the system volume may be shrunk to (1 MiB aligned, up).
pub fn minimum_system_volume_bytes(volume: &SystemVolume) -> u64 {
    let floor = volume.supported_min_bytes.max(
        volume
            .used_bytes
            .saturating_add(system_volume_reserve(volume.used_bytes)),
    );
    floor.div_ceil(ALIGN_BYTES).saturating_mul(ALIGN_BYTES)
}

/// Supported Btrfs minimum plus explicit retained headroom, rounded upward.
/// Overflow refuses the plan instead of manufacturing a smaller bound.
pub fn minimum_linux_btrfs_bytes(volume: &LinuxBtrfsVolume) -> Result<u64, PlanError> {
    volume
        .supported_min_bytes
        .checked_add(volume.reserve_bytes)
        .and_then(|bytes| bytes.checked_add(ALIGN_BYTES.saturating_sub(1)))
        .map(|bytes| (bytes / ALIGN_BYTES).saturating_mul(ALIGN_BYTES))
        .ok_or_else(|| PlanError::Unsupported("Btrfs shrink bound overflow".into()))
}

/// Validated sector geometry of the basis disk.
#[derive(Clone, Copy, Debug)]
struct Geometry {
    sector: u64,
    /// LBAs per [`ALIGN_BYTES`].
    align: u64,
    first_usable: u64,
    last_usable: u64,
}

impl Geometry {
    fn of(disk: &Disk) -> Result<Self, PlanError> {
        let unsupported = |why: &str| Err(PlanError::Unsupported(why.into()));
        let sector = disk.sector_bytes;
        if !(512..=4096).contains(&sector) || !sector.is_power_of_two() {
            return unsupported("unsupported logical sector size");
        }
        if disk.size_bytes == 0 || !disk.size_bytes.is_multiple_of(sector) {
            return unsupported("disk size is not a whole number of sectors");
        }
        let total = disk
            .size_bytes
            .checked_div(sector)
            .ok_or_else(|| PlanError::Unsupported("zero logical sector size".into()))?;
        if disk.first_usable_lba < 2
            || disk.first_usable_lba > disk.last_usable_lba
            || disk.last_usable_lba >= total
        {
            return unsupported("GPT usable bounds are inconsistent with the disk size");
        }
        let geometry = Self {
            sector,
            align: ALIGN_BYTES
                .checked_div(sector)
                .ok_or_else(|| PlanError::Unsupported("zero logical sector size".into()))?,
            first_usable: disk.first_usable_lba,
            last_usable: disk.last_usable_lba,
        };
        let mut sorted: Vec<_> = disk.partitions.iter().collect();
        sorted.sort_by_key(|p| p.first_lba);
        let mut previous_last: Option<u64> = None;
        let mut guids = std::collections::HashSet::new();
        for partition in sorted {
            if partition.first_lba > partition.last_lba
                || !(geometry.first_usable..=geometry.last_usable).contains(&partition.first_lba)
                || !(geometry.first_usable..=geometry.last_usable).contains(&partition.last_lba)
            {
                return unsupported("a partition lies outside the GPT usable bounds");
            }
            if previous_last.is_some_and(|last| partition.first_lba <= last) {
                return unsupported("partitions overlap");
            }
            if !guids.insert(partition.guid.to_ascii_lowercase()) {
                return unsupported("duplicate partition GUID");
            }
            previous_last = Some(partition.last_lba);
        }
        Ok(geometry)
    }

    fn up(self, lba: u64) -> u64 {
        lba.div_ceil(self.align).saturating_mul(self.align)
    }

    fn down(self, lba: u64) -> u64 {
        lba.checked_div(self.align)
            .unwrap_or_default()
            .saturating_mul(self.align)
    }

    /// Bytes rounded up to whole alignment units, as LBAs.
    fn lbas(self, bytes: u64) -> u64 {
        bytes.div_ceil(ALIGN_BYTES).saturating_mul(self.align)
    }

    fn bytes(self, lbas: u64) -> u64 {
        lbas.saturating_mul(self.sector)
    }
}

/// Reject requirement totals that cannot be represented in bytes. Geometry's
/// own LBAs are bounded by the validated disk size and cannot overflow.
fn checked_requirement_sum(g: Geometry, values: &[u64]) -> Result<u64, PlanError> {
    values
        .iter()
        .try_fold(0_u64, |sum, value| sum.checked_add(*value))
        .filter(|sum| sum.checked_mul(g.sector).is_some())
        .ok_or_else(|| PlanError::Unsupported("layout size overflow".into()))
}

/// An aligned, clearance-respecting run of free LBAs `[start, end)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Span {
    start: u64,
    end: u64,
}

impl Span {
    fn len(self) -> u64 {
        self.end.saturating_sub(self.start)
    }
}

/// Aligned free spans between partitions, keeping [`ALIGN_BYTES`] clear of
/// every existing partition.
fn free_spans(disk: &Disk, g: Geometry) -> Vec<Span> {
    let mut sorted: Vec<_> = disk.partitions.iter().collect();
    sorted.sort_by_key(|p| p.first_lba);
    let mut spans = Vec::new();
    let mut lower = g.first_usable;
    for index in 0..=sorted.len() {
        let upper_excl = match sorted.get(index) {
            Some(next) => next.first_lba.saturating_sub(g.align),
            None => g.last_usable.saturating_add(1),
        };
        let span = Span {
            start: g.up(lower),
            end: g.down(upper_excl),
        };
        if span.end > span.start {
            spans.push(span);
        }
        if let Some(partition) = sorted.get(index) {
            lower = partition.last_lba.saturating_add(1).saturating_add(g.align);
        }
    }
    spans
}

/// Where the layout may go on this basis, before sizes are chosen.
#[derive(Clone, Debug)]
enum Capacity {
    /// Linux: the largest existing free span.
    Free { span: Span },
    /// A span after one eligible Windows or Linux Btrfs volume, ending before
    /// its next partition. The bound comes from the actual filesystem minimum.
    Shrinkable {
        end: u64,
        /// First LBA usable without shrinking.
        current_start: u64,
        /// First LBA usable at the maximum safe shrink.
        lowest_start: u64,
        volume_first_lba: u64,
        partition_guid: String,
        from_bytes: u64,
        minimum_bytes: u64,
    },
}

impl Capacity {
    fn of(basis: &Basis, g: Geometry) -> Result<Self, PlanError> {
        let (partition_guid, size_bytes, minimum) =
            match (&basis.system_volume, &basis.linux_btrfs_volume) {
                (Some(_), Some(_)) => {
                    return Err(PlanError::Unsupported(
                        "Windows and Linux shrink facts cannot coexist".into(),
                    ));
                }
                (Some(volume), None) => (
                    &volume.partition_guid,
                    volume.size_bytes,
                    minimum_system_volume_bytes(volume),
                ),
                (None, Some(volume)) => {
                    if volume.device_id == 0
                        || volume.mount_id == 0
                        || volume.sector_bytes != g.sector
                        || [volume.filesystem_bytes, volume.kernel_partition_bytes]
                            .iter()
                            .any(|bytes| *bytes != volume.size_bytes)
                        || volume.supported_min_bytes == 0
                        || volume.reserve_bytes == 0
                        || uuid::Uuid::parse_str(&volume.filesystem_uuid)
                            .ok()
                            .is_none_or(|uuid| uuid.is_nil())
                        || uuid::Uuid::parse_str(&volume.partition_guid)
                            .ok()
                            .is_none_or(|uuid| uuid.is_nil())
                    {
                        return Err(PlanError::Unsupported("invalid direct Btrfs facts".into()));
                    }
                    (
                        &volume.partition_guid,
                        volume.size_bytes,
                        minimum_linux_btrfs_bytes(volume)?,
                    )
                }
                (None, None) => {
                    let span = free_spans(&basis.disk, g)
                        .into_iter()
                        .max_by(|a, b| a.len().cmp(&b.len()).then(b.start.cmp(&a.start)))
                        .unwrap_or(Span { start: 0, end: 0 });
                    return Ok(Self::Free { span });
                }
            };
        let c = basis
            .disk
            .partitions
            .iter()
            .find(|p| super::same_guid(&p.guid, partition_guid))
            .ok_or_else(|| {
                PlanError::Unsupported("the shrink volume is not on the system disk".into())
            })?;
        if g.bytes(c.last_lba.saturating_sub(c.first_lba).saturating_add(1)) != size_bytes {
            return Err(PlanError::Unsupported(
                "the shrink volume size disagrees with the partition map".into(),
            ));
        }
        if let Some(volume) = &basis.linux_btrfs_volume
            && (volume.first_lba != c.first_lba
                || !super::same_guid(&c.type_guid, "0fc63daf-8483-4772-8e79-3d69d8477de4")
                || minimum > size_bytes)
        {
            return Err(PlanError::Unsupported(
                "direct Btrfs geometry or shrink bound disagrees with GPT".into(),
            ));
        }
        let next_first = basis
            .disk
            .partitions
            .iter()
            .filter(|p| p.first_lba > c.last_lba)
            .map(|p| p.first_lba)
            .min();
        let upper_excl = match next_first {
            Some(first) => first.saturating_sub(g.align),
            None => g.last_usable.saturating_add(1),
        };
        let end = g.down(upper_excl);
        let current_start = g.up(c.last_lba.saturating_add(1).saturating_add(g.align));
        let lowest = g.up(c
            .first_lba
            .saturating_add(
                minimum
                    .checked_div(g.sector)
                    .ok_or_else(|| PlanError::Unsupported("zero logical sector size".into()))?,
            )
            .saturating_add(g.align));
        Ok(Self::Shrinkable {
            end,
            current_start,
            lowest_start: lowest.min(current_start),
            volume_first_lba: c.first_lba,
            partition_guid: partition_guid.clone(),
            from_bytes: size_bytes,
            minimum_bytes: minimum,
        })
    }

    /// The largest number of LBAs the layout may occupy.
    fn max_lbas(&self) -> u64 {
        match self {
            Self::Free { span } => span.len(),
            Self::Shrinkable {
                end, lowest_start, ..
            } => end.saturating_sub(*lowest_start),
        }
    }
}

/// Place staging + target on `basis`. Pure apart from GUID generation; see
/// the design spec §2 for rules.
///
/// Layout: `target | staging ESP | staging root`, contiguous and 1 MiB
/// aligned. Linux without shrink facts uses the largest existing free span.
/// Windows and eligible Linux Btrfs layouts pin it against the end of the
/// space after the retained volume and shrink by exactly the shortfall.
/// Partition GUIDs are pre-generated only when [`Basis::sets_partition_guids`]. The loader path defaults to the
/// fallback path for this build's architecture; callers holding the bundle's
/// `boot_binding.efi_path` apply it with [`with_loader_path`].
pub fn compute(basis: &Basis, requirements: Requirements) -> Result<Plan, PlanError> {
    let g = Geometry::of(&basis.disk)?;
    if requirements.target_bytes < MIN_TARGET_BYTES {
        return Err(PlanError::Unsupported(
            "the Tilo size is below the installer minimum".into(),
        ));
    }
    if requirements.staging_esp_bytes == 0 || requirements.staging_root_bytes == 0 {
        return Err(PlanError::Unsupported(
            "staging sizes must be non-zero".into(),
        ));
    }
    let target = g.lbas(requirements.target_bytes);
    let esp = g.lbas(requirements.staging_esp_bytes);
    let root = g.lbas(requirements.staging_root_bytes);
    let need = checked_requirement_sum(g, &[target, esp, root])?;
    let capacity = Capacity::of(basis, g)?;
    let available_bytes = g.bytes(capacity.max_lbas());
    let insufficient = || PlanError::InsufficientSpace {
        needed_bytes: g.bytes(need),
        available_bytes,
    };
    if need > capacity.max_lbas() {
        return Err(insufficient());
    }
    let (start, shrink) = match capacity {
        Capacity::Free { span } => (span.start, None),
        Capacity::Shrinkable {
            end,
            current_start,
            volume_first_lba,
            partition_guid,
            from_bytes,
            minimum_bytes,
            ..
        } => {
            let start = end.saturating_sub(need);
            let shrink = if start >= current_start {
                None
            } else {
                let to_bytes = (g.bytes(
                    start
                        .checked_sub(g.align)
                        .and_then(|start| start.checked_sub(volume_first_lba))
                        .ok_or_else(|| {
                            PlanError::Unsupported("shrink geometry underflow".into())
                        })?,
                ) / ALIGN_BYTES)
                    .saturating_mul(ALIGN_BYTES);
                if to_bytes < minimum_bytes || to_bytes >= from_bytes {
                    return Err(insufficient());
                }
                Some(Shrink {
                    partition_guid,
                    from_bytes,
                    to_bytes,
                })
            };
            (start, shrink)
        }
    };
    let guid = || basis.sets_partition_guids().then(super::random_guid);
    let loader_path = format!(
        "\\EFI\\BOOT\\{}",
        match std::env::consts::ARCH {
            "aarch64" => "BOOTAA64.EFI",
            _ => "BOOTX64.EFI",
        }
    );
    Ok(Plan {
        basis: basis.clone(),
        shrink,
        target_extent: Extent {
            first_lba: start,
            last_lba: start.saturating_add(target).saturating_sub(1),
        },
        staging_esp: StagedPartition {
            first_lba: start.saturating_add(target),
            last_lba: start
                .saturating_add(target)
                .saturating_add(esp)
                .saturating_sub(1),
            partition_guid: guid(),
        },
        staging_root: StagedPartition {
            first_lba: start.saturating_add(target).saturating_add(esp),
            last_lba: start.saturating_add(need).saturating_sub(1),
            partition_guid: guid(),
        },
        boot_entry: BootEntry {
            description: super::BOOT_ENTRY_DESCRIPTION.into(),
            loader_path,
        },
    })
}

/// Tilo sizes the Review stepper may offer for `basis`, given staging sizes
/// (`requirements.target_bytes` is ignored). Both bounds are 1 MiB aligned,
/// and `compute` accepts every aligned size between them.
pub fn target_bounds(basis: &Basis, requirements: Requirements) -> Result<TargetBounds, PlanError> {
    let g = Geometry::of(&basis.disk)?;
    let staging = checked_requirement_sum(
        g,
        &[
            g.lbas(requirements.staging_esp_bytes),
            g.lbas(requirements.staging_root_bytes),
        ],
    )?;
    let capacity = Capacity::of(basis, g)?;
    let max_target = capacity.max_lbas().saturating_sub(staging);
    let min_target = g.lbas(MIN_TARGET_BYTES);
    if max_target < min_target {
        return Err(PlanError::InsufficientSpace {
            needed_bytes: g.bytes(checked_requirement_sum(g, &[min_target, staging])?),
            available_bytes: g.bytes(capacity.max_lbas()),
        });
    }
    Ok(TargetBounds {
        min_bytes: g.bytes(min_target),
        max_bytes: g.bytes(max_target),
    })
}

/// Rewrite the loader path from the bundle's `boot_binding.efi_path`
/// (`/EFI/BOOT/BOOTX64.EFI`) into the plan's backslash form.
pub fn with_loader_path(mut plan: Plan, efi_path: &str) -> Plan {
    plan.boot_entry.loader_path = efi_path.replace('/', "\\");
    plan
}

#[cfg(test)]
pub(crate) mod tests {
    use super::super::platform::Partition;
    use super::*;

    const MIB: u64 = 1 << 20;
    const GIB: u64 = 1 << 30;
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
        let total = size_bytes.checked_div(sector).expect("nonzero sector");
        let tail = if sector == 512 { 34 } else { 6 };
        Disk {
            id: "/dev/test".into(),
            guid: "11111111-2222-4333-8444-555555555555".into(),
            size_bytes,
            sector_bytes: sector,
            first_usable_lba: tail,
            last_usable_lba: total.checked_sub(tail).expect("disk fits GPT"),
            partitions,
        }
    }

    pub(crate) const C_GUID: &str = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";

    /// ESP 100 MiB, MSR 16 MiB, C: to `c_end_gib`, optional `WinRE` right after C:.
    pub(crate) fn windows_basis(
        disk_gib: u64,
        c_end_gib: u64,
        used_gib: u64,
        winre: bool,
    ) -> Basis {
        let s = 512;
        let mib = MIB.checked_div(s).expect("nonzero sector");
        let mut parts = vec![
            part(
                1,
                "e0000000-0000-4000-8000-000000000001",
                mib,
                101_u64.checked_mul(mib).unwrap().checked_sub(1).unwrap(),
            ),
            part(
                2,
                "e0000000-0000-4000-8000-000000000002",
                101_u64.checked_mul(mib).unwrap(),
                117_u64.checked_mul(mib).unwrap().checked_sub(1).unwrap(),
            ),
        ];
        let c_first = 117_u64.checked_mul(mib).unwrap();
        let c_last = c_end_gib
            .checked_mul(GIB)
            .unwrap()
            .checked_div(s)
            .unwrap()
            .checked_sub(1)
            .unwrap();
        parts.push(part(3, C_GUID, c_first, c_last));
        if winre {
            parts.push(part(
                4,
                "e0000000-0000-4000-8000-000000000004",
                c_last.checked_add(1).unwrap(),
                c_last
                    .checked_add(700_u64.checked_mul(mib).unwrap())
                    .unwrap(),
            ));
        }
        let size = c_last
            .checked_sub(c_first)
            .unwrap()
            .checked_add(1)
            .unwrap()
            .checked_mul(s)
            .unwrap();
        Basis {
            disk: disk(disk_gib.checked_mul(GIB).unwrap(), s, parts),
            system_volume: Some(SystemVolume {
                partition_guid: C_GUID.into(),
                size_bytes: size,
                supported_min_bytes: used_gib.checked_mul(GIB).unwrap().checked_add(GIB).unwrap(),
                used_bytes: used_gib.checked_mul(GIB).unwrap(),
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

    fn req(target: u64) -> Requirements {
        Requirements {
            staging_esp_bytes: 600 * MIB,
            staging_root_bytes: 40 * GIB,
            target_bytes: target,
        }
    }

    /// Every invariant of §2, asserted against any computed plan.
    fn assert_layout(plan: &Plan, requirements: Requirements) {
        let disk = &plan.basis.disk;
        let sector = disk.sector_bytes;
        let align = MIB.checked_div(sector).unwrap();
        let target = plan.target_extent;
        let esp = plan.staging_esp.extent();
        let root = plan.staging_root.extent();
        for extent in [target, esp, root] {
            assert!(
                extent.first_lba.is_multiple_of(align),
                "{extent:?} start aligned"
            );
            assert!(
                extent.lbas().is_multiple_of(align),
                "{extent:?} size aligned"
            );
            assert!(
                extent.first_lba >= disk.first_usable_lba
                    && extent.last_lba <= disk.last_usable_lba
            );
        }
        assert_eq!(
            target.last_lba.checked_add(1).unwrap(),
            esp.first_lba,
            "ESP follows target"
        );
        assert_eq!(
            esp.last_lba.checked_add(1).unwrap(),
            root.first_lba,
            "root follows ESP"
        );
        assert!(target.lbas().checked_mul(sector).unwrap() >= requirements.target_bytes);
        assert!(esp.lbas().checked_mul(sector).unwrap() >= requirements.staging_esp_bytes);
        assert!(root.lbas().checked_mul(sector).unwrap() >= requirements.staging_root_bytes);
        for partition in &disk.partitions {
            let mut last = partition.last_lba;
            if let Some(shrink) = &plan.shrink
                && super::super::same_guid(&partition.guid, &shrink.partition_guid)
            {
                last = partition
                    .first_lba
                    .checked_add(shrink.to_bytes.checked_div(sector).unwrap())
                    .unwrap()
                    .checked_sub(1)
                    .unwrap();
            }
            let clear = last.checked_add(align).unwrap() < target.first_lba
                || partition.first_lba > root.last_lba.checked_add(align).unwrap();
            assert!(clear, "{partition:?} is within 1 MiB of the layout");
        }
    }

    #[test]
    fn overflowing_layout_requirements_are_rejected() {
        let basis = linux_basis(512, 200 * GIB, vec![]);
        for requirements in [
            Requirements {
                staging_esp_bytes: u64::MAX,
                staging_root_bytes: 1,
                target_bytes: MIN_TARGET_BYTES,
            },
            Requirements {
                staging_esp_bytes: GIB,
                staging_root_bytes: u64::MAX,
                target_bytes: MIN_TARGET_BYTES,
            },
            Requirements {
                staging_esp_bytes: GIB,
                staging_root_bytes: GIB,
                target_bytes: u64::MAX,
            },
        ] {
            assert!(matches!(
                compute(&basis, requirements),
                Err(PlanError::Unsupported(_))
            ));
        }
        assert!(matches!(
            target_bounds(
                &basis,
                Requirements {
                    staging_esp_bytes: u64::MAX,
                    staging_root_bytes: GIB,
                    target_bytes: 0,
                }
            ),
            Err(PlanError::Unsupported(_))
        ));
    }

    /// Linux extent selection, exact fit, alignment and empty disks.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn linux_plan_uses_one_aligned_free_extent() {
        {
            let s = 512;
            let mib = MIB / s;
            let parts = vec![
                part(
                    1,
                    "a0000000-0000-4000-8000-000000000001",
                    mib,
                    601 * mib - 1,
                ),
                // 50 GiB gap, then a partition, then a ~140 GiB tail.
                part(
                    2,
                    "a0000000-0000-4000-8000-000000000002",
                    601 * mib + 50 * GIB / s,
                    601 * mib + 60 * GIB / s - 1,
                ),
            ];
            let basis = linux_basis(s, 200 * GIB, parts);
            let r = req(64 * GIB);
            let plan = compute(&basis, r).unwrap();
            assert_layout(&plan, r);
            assert!(plan.shrink.is_none());
            assert_eq!(
                plan.target_extent.first_lba,
                601 * mib + 60 * GIB / s + mib,
                "starts exactly 1 MiB after the last partition"
            );
            assert!(plan.staging_esp.partition_guid.is_some());
            assert!(plan.staging_root.partition_guid.is_some());
            assert_ne!(
                plan.staging_esp.partition_guid,
                plan.staging_root.partition_guid
            );
        }

        {
            let s = 512;
            let mib = MIB / s;
            let gap = 70 * GIB / s;
            let parts = vec![
                part(1, "a0000000-0000-4000-8000-000000000001", mib, 2 * mib - 1),
                part(
                    2,
                    "a0000000-0000-4000-8000-000000000002",
                    2 * mib + gap,
                    3 * mib + gap - 1,
                ),
                part(
                    3,
                    "a0000000-0000-4000-8000-000000000003",
                    3 * mib + 2 * gap,
                    4 * mib + 2 * gap - 1,
                ),
            ];
            let end = 4 * mib + 2 * gap;
            let basis = linux_basis(s, (end + 34 + mib / 2) * s, parts);
            let err = compute(&basis, req(64 * GIB)).unwrap_err();
            let PlanError::InsufficientSpace {
                needed_bytes,
                available_bytes,
            } = err
            else {
                panic!("{err:?}")
            };
            assert_eq!(needed_bytes, 64 * GIB + 600 * MIB + 40 * GIB);
            assert_eq!(
                available_bytes,
                70 * GIB - 2 * MIB,
                "clearance on both sides"
            );
        }

        {
            let s = 512;
            let mib = MIB / s;
            let r = req(64 * GIB);
            let need = (64 * GIB + 600 * MIB + 40 * GIB) / s;
            // Partition 1 at [1 MiB, 2 MiB); 1 MiB clearance; the layout; 1 MiB
            // clearance; partition 2.
            let make = |short: u64| {
                let second = 3 * mib + need + mib - short;
                linux_basis(
                    s,
                    (second + 2 * mib) * s,
                    vec![
                        part(1, "a0000000-0000-4000-8000-000000000001", mib, 2 * mib - 1),
                        part(
                            2,
                            "a0000000-0000-4000-8000-000000000002",
                            second,
                            second + mib - 1,
                        ),
                    ],
                )
            };
            let plan = compute(&make(0), r).unwrap();
            assert_layout(&plan, r);
            assert_eq!(plan.target_extent.first_lba, 3 * mib);
            assert_eq!(plan.staging_root.last_lba + 1, 3 * mib + need);
            // One sector short is already a whole MiB short after alignment.
            assert!(matches!(
                compute(&make(1), r),
                Err(PlanError::InsufficientSpace { .. })
            ));
        }

        {
            let s = 512;
            let mib = MIB / s;
            let parts = vec![
                part(1, "a0000000-0000-4000-8000-000000000001", 34, 1_000_003),
                part(
                    2,
                    "a0000000-0000-4000-8000-000000000002",
                    300 * GIB / s + 17,
                    300 * GIB / s + 5000,
                ),
            ];
            let basis = linux_basis(s, 400 * GIB, parts);
            let r = req(64 * GIB);
            let plan = compute(&basis, r).unwrap();
            assert_layout(&plan, r);
            // The largest span is the one between the partitions; it starts
            // 1 MiB after partition 1, rounded up.
            let expected = (1_000_003 + 1 + mib).div_ceil(mib) * mib;
            assert_eq!(plan.target_extent.first_lba, expected);
        }

        // Phase `linux_4kn_sectors_align_to_256_lbas`.
        {
            let s = 4096;
            let lbas_per_mib = MIB / s;
            assert_eq!(lbas_per_mib, 256);
            let parts = vec![part(
                1,
                "a0000000-0000-4000-8000-000000000001",
                256,
                256 * 601 - 1,
            )];
            let basis = linux_basis(s, 256 * GIB, parts);
            let r = req(64 * GIB);
            let plan = compute(&basis, r).unwrap();
            assert_layout(&plan, r);
            assert_eq!(plan.target_extent.first_lba, 256 * 602);
            assert_eq!(plan.target_extent.lbas(), 64 * GIB / s);
            assert_eq!(plan.staging_esp.extent().lbas(), 600 * 256);
        }

        {
            let basis = linux_basis(512, 128 * GIB, Vec::new());
            let r = req(64 * GIB);
            let plan = compute(&basis, r).unwrap();
            assert_layout(&plan, r);
            assert_eq!(plan.target_extent.first_lba, 2048);
        }
    }

    /// Size rounding, minimums, reserve, bounds and full disks.
    #[test]
    fn plan_sizes_reserves_and_bounds_are_exact() {
        {
            let basis = linux_basis(512, 128 * GIB, Vec::new());
            let r = Requirements {
                staging_esp_bytes: 600 * MIB + 512,
                staging_root_bytes: 40 * GIB + 1,
                target_bytes: 35 * GIB + 3,
            };
            let plan = compute(&basis, r).unwrap();
            assert_layout(&plan, r);
            assert_eq!(plan.staging_esp.extent().lbas() * 512, 601 * MIB);
            assert_eq!(plan.staging_root.extent().lbas() * 512, 40 * GIB + MIB);
            assert_eq!(plan.target_extent.lbas() * 512, 35 * GIB + MIB);
        }

        {
            let basis = linux_basis(512, 256 * GIB, Vec::new());
            assert!(matches!(
                compute(&basis, req(35 * GIB - MIB)),
                Err(PlanError::Unsupported(_))
            ));
        }

        // Phase `reserve_is_the_larger_of_20_gib_and_15_percent_of_used`.
        {
            assert_eq!(system_volume_reserve(10 * GIB), 20 * GIB);
            assert_eq!(system_volume_reserve(200 * GIB), 30 * GIB);
            assert_eq!(system_volume_reserve(0), 20 * GIB);
        }

        {
            for basis in [
                windows_basis(512, 400, 100, true),
                windows_basis(512, 400, 100, false),
                linux_basis(512, 300 * GIB, Vec::new()),
                linux_basis(4096, 300 * GIB, Vec::new()),
            ] {
                let bounds = target_bounds(&basis, req(0)).unwrap();
                assert_eq!(bounds.min_bytes, MIN_TARGET_BYTES);
                assert_eq!(bounds.max_bytes % MIB, 0);
                let r = req(bounds.max_bytes);
                let plan = compute(&basis, r).unwrap();
                assert_layout(&plan, r);
                assert!(matches!(
                    compute(&basis, req(bounds.max_bytes + MIB)),
                    Err(PlanError::InsufficientSpace { .. })
                ));
                assert!(compute(&basis, req(bounds.min_bytes)).is_ok());
            }
        }

        {
            let s = 512;
            let basis = linux_basis(
                s,
                128 * GIB,
                vec![part(
                    1,
                    "a0000000-0000-4000-8000-000000000001",
                    2048,
                    128 * GIB / s - 2048,
                )],
            );
            assert!(matches!(
                compute(&basis, req(35 * GIB)),
                Err(PlanError::InsufficientSpace {
                    available_bytes: 0,
                    ..
                })
            ));
        }
    }

    /// Partition map consistency, system volume, digest and loader path.
    #[test]
    fn plans_require_consistent_maps_and_bind_digest_and_loader() {
        {
            let s = 512;
            let overlap = linux_basis(
                s,
                64 * GIB,
                vec![
                    part(1, "a0000000-0000-4000-8000-000000000001", 2048, 10_000),
                    part(2, "a0000000-0000-4000-8000-000000000002", 10_000, 20_000),
                ],
            );
            assert!(matches!(
                compute(&overlap, req(35 * GIB)),
                Err(PlanError::Unsupported(_))
            ));
            let mut odd = linux_basis(512, 64 * GIB, Vec::new());
            odd.disk.sector_bytes = 520;
            assert!(matches!(
                compute(&odd, req(35 * GIB)),
                Err(PlanError::Unsupported(_))
            ));
            let mut outside = linux_basis(s, 64 * GIB, Vec::new());
            outside.disk.partitions = vec![part(1, "a0000000-0000-4000-8000-000000000001", 1, 100)];
            assert!(matches!(
                compute(&outside, req(35 * GIB)),
                Err(PlanError::Unsupported(_))
            ));
            let mut duplicate = linux_basis(s, 64 * GIB, Vec::new());
            duplicate.disk.partitions = vec![
                part(1, "a0000000-0000-4000-8000-000000000001", 2048, 4095),
                part(2, "A0000000-0000-4000-8000-000000000001", 8192, 9000),
            ];
            assert!(matches!(
                compute(&duplicate, req(35 * GIB)),
                Err(PlanError::Unsupported(_))
            ));
        }

        {
            let mut basis = windows_basis(512, 400, 100, true);
            basis.system_volume.as_mut().unwrap().size_bytes += MIB;
            assert!(matches!(
                compute(&basis, req(64 * GIB)),
                Err(PlanError::Unsupported(_))
            ));
            basis.system_volume.as_mut().unwrap().partition_guid =
                "dddddddd-dddd-4ddd-8ddd-dddddddddddd".into();
            assert!(matches!(
                compute(&basis, req(64 * GIB)),
                Err(PlanError::Unsupported(_))
            ));
        }

        {
            let basis = windows_basis(512, 400, 100, true);
            let plan = compute(&basis, req(64 * GIB)).unwrap();
            let mut other = plan.clone();
            other.target_extent.last_lba -= 1;
            assert_ne!(plan.digest().unwrap(), other.digest().unwrap());
            let round: Plan = serde_json::from_slice(&serde_json::to_vec(&plan).unwrap()).unwrap();
            assert_eq!(round.digest().unwrap(), plan.digest().unwrap());
        }

        {
            let basis = linux_basis(512, 128 * GIB, Vec::new());
            let plan = with_loader_path(
                compute(&basis, req(35 * GIB)).unwrap(),
                "/EFI/BOOT/BOOTX64.EFI",
            );
            assert_eq!(plan.boot_entry.loader_path, "\\EFI\\BOOT\\BOOTX64.EFI");
            assert_eq!(plan.boot_entry.description, "Tilo installer");
            assert_eq!(plan.basis.host_os(), HostOs::Linux);
        }
    }

    fn btrfs_basis() -> Basis {
        let mut basis = windows_basis(512, 400, 100, true);
        let volume = basis.system_volume.take().unwrap();
        let partition = basis
            .disk
            .partitions
            .iter_mut()
            .find(|p| p.guid == C_GUID)
            .unwrap();
        partition.type_guid = "0fc63daf-8483-4772-8e79-3d69d8477de4".into();
        basis.linux_btrfs_volume = Some(LinuxBtrfsVolume {
            partition_guid: volume.partition_guid,
            filesystem_uuid: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb".into(),
            device_id: 1,
            mount_id: 37,
            first_lba: partition.first_lba,
            sector_bytes: basis.disk.sector_bytes,
            size_bytes: volume.size_bytes,
            filesystem_bytes: volume.size_bytes,
            kernel_partition_bytes: volume.size_bytes,
            supported_min_bytes: 250 * GIB,
            reserve_bytes: 20 * GIB,
        });
        basis
    }

    #[test]
    fn linux_btrfs_plans_use_supported_minimum_and_keep_linux_guids() {
        let basis = btrfs_basis();
        let bounds = target_bounds(&basis, req(0)).unwrap();
        let plan = compute(&basis, req(bounds.max_bytes)).unwrap();
        assert_layout(&plan, req(bounds.max_bytes));
        let shrink = plan.shrink.as_ref().unwrap();
        assert_eq!(shrink.to_bytes, 270 * GIB);
        assert_eq!(shrink.partition_guid, C_GUID);
        assert_eq!(plan.basis.host_os(), HostOs::Linux);
        assert!(plan.staging_esp.partition_guid.is_some());
        assert!(plan.staging_root.partition_guid.is_some());
        assert!(matches!(
            compute(&basis, req(bounds.max_bytes + MIB)),
            Err(PlanError::InsufficientSpace { .. })
        ));
        let mut changed = basis;
        changed
            .linux_btrfs_volume
            .as_mut()
            .unwrap()
            .supported_min_bytes += GIB;
        assert_eq!(
            target_bounds(&changed, req(0)).unwrap().max_bytes,
            bounds.max_bytes - GIB
        );
        let mut rebound = plan.clone();
        rebound.basis = changed;
        assert_ne!(rebound.digest().unwrap(), plan.digest().unwrap());
    }

    #[test]
    fn linux_btrfs_plans_refuse_inconsistent_and_indirect_facts() {
        let basis = btrfs_basis();
        let mutations: [fn(&mut LinuxBtrfsVolume); 9] = [
            |v| v.mount_id = 0,
            |v| v.device_id = 0,
            |v| v.first_lba += 1,
            |v| v.sector_bytes = 4096,
            |v| v.filesystem_uuid = "invalid".into(),
            |v| v.filesystem_bytes -= MIB,
            |v| v.kernel_partition_bytes -= MIB,
            |v| v.supported_min_bytes = u64::MAX,
            |v| v.partition_guid = "dddddddd-dddd-4ddd-8ddd-dddddddddddd".into(),
        ];
        for mutate in mutations {
            let mut changed = basis.clone();
            mutate(changed.linux_btrfs_volume.as_mut().unwrap());
            assert!(matches!(
                compute(&changed, req(64 * GIB)),
                Err(PlanError::Unsupported(_))
            ));
            assert!(matches!(
                target_bounds(&changed, req(0)),
                Err(PlanError::Unsupported(_))
            ));
        }
        let mut mixed = basis;
        mixed.system_volume = windows_basis(512, 400, 100, true).system_volume;
        assert!(matches!(
            compute(&mixed, req(64 * GIB)),
            Err(PlanError::Unsupported(_))
        ));
    }

    #[test]
    fn legacy_basis_serialization_preserves_absent_linux_facts() {
        let basis = windows_basis(512, 400, 100, true);
        let value = serde_json::to_value(&basis).unwrap();
        assert!(value.get("linux_btrfs_volume").is_none());
        let decoded: Basis = serde_json::from_value(value).unwrap();
        assert_eq!(decoded, basis);
        assert_eq!(decoded.host_os(), HostOs::Windows);
        assert!(!decoded.sets_partition_guids());
    }

    /// Windows shrink planning.
    #[test]
    fn windows_plan_shrinks_c_only_by_the_safe_shortfall() {
        {
            // 512 GiB disk, C: ends at 400 GiB, WinRE right after; tail free.
            let basis = windows_basis(512, 400, 100, true);
            let r = req(64 * GIB);
            let plan = compute(&basis, r).unwrap();
            assert_layout(&plan, r);
            let shrink = plan
                .shrink
                .clone()
                .expect("C: must shrink: WinRE blocks the tail");
            let volume = basis.system_volume.as_ref().unwrap();
            assert_eq!(shrink.from_bytes, volume.size_bytes);
            let need = 64 * GIB + 600 * MIB + 40 * GIB;
            // Nothing usable after C: before WinRE, so C: gives up need + 2 MiB
            // clearance (one each side of the layout).
            assert_eq!(shrink.from_bytes - shrink.to_bytes, need + 2 * MIB);
            assert_eq!(shrink.to_bytes % MIB, 0);
            let winre = &basis.disk.partitions[3];
            assert_eq!(plan.staging_root.last_lba + 1 + MIB / 512, winre.first_lba);
            assert!(
                plan.staging_esp.partition_guid.is_none(),
                "Windows cannot set GUIDs"
            );
            assert!(plan.staging_root.partition_guid.is_none());
            assert_eq!(plan.basis.host_os(), HostOs::Windows);
        }

        {
            // C: ends at 400 GiB; no WinRE; a 112 GiB free tail covers everything.
            let basis = windows_basis(512, 400, 100, false);
            let r = req(64 * GIB);
            let plan = compute(&basis, r).unwrap();
            assert_layout(&plan, r);
            assert!(plan.shrink.is_none());
            assert_eq!(
                plan.staging_root.last_lba,
                (basis.disk.last_usable_lba + 1) / 2048 * 2048 - 1
            );
        }

        {
            // 20 GiB free after C:, WinRE after that.
            let s = 512;
            let mib = MIB / s;
            let mut basis = windows_basis(512, 400, 100, false);
            let winre_first = 420 * GIB / s;
            basis.disk.partitions.push(part(
                4,
                "e0000000-0000-4000-8000-000000000004",
                winre_first,
                winre_first + 700 * mib - 1,
            ));
            let r = req(64 * GIB);
            let plan = compute(&basis, r).unwrap();
            assert_layout(&plan, r);
            let shrink = plan.shrink.unwrap();
            let need = 64 * GIB + 600 * MIB + 40 * GIB;
            assert_eq!(
                shrink.from_bytes - shrink.to_bytes,
                need + 2 * MIB - 20 * GIB
            );
        }

        {
            // C: ~400 GiB with 300 GiB used: reserve 45 GiB, so C: keeps 345 GiB
            // and only ~55 GiB can be carved: less than the ~104.6 GiB needed.
            let basis = windows_basis(512, 400, 300, true);
            let err = compute(&basis, req(64 * GIB)).unwrap_err();
            let PlanError::InsufficientSpace {
                available_bytes, ..
            } = err
            else {
                panic!("{err:?}")
            };
            let volume = basis.system_volume.as_ref().unwrap();
            assert_eq!(minimum_system_volume_bytes(volume), 345 * GIB);
            assert!(available_bytes < 64 * GIB + 600 * MIB + 40 * GIB);
            assert!(available_bytes > 50 * GIB);
        }

        {
            let mut basis = windows_basis(512, 400, 50, true);
            // Unmovable files: the supported minimum dominates the free rule.
            basis.system_volume.as_mut().unwrap().supported_min_bytes = 350 * GIB;
            assert!(matches!(
                compute(&basis, req(64 * GIB)),
                Err(PlanError::InsufficientSpace { .. })
            ));
            let bounds = target_bounds(&basis, req(0));
            assert!(matches!(bounds, Err(PlanError::InsufficientSpace { .. })));
        }
    }
}
