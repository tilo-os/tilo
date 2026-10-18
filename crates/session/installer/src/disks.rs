//! Target-disk and partition discovery.
//!
//! Devices come from `lsblk -J -b -p` with [`LSBLK_COLUMNS`], parsed by the
//! shared `block-devices` inventory. Only `TYPE == "disk"` rows with a real
//! size are candidates; memory- and loop-backed pseudo disks never are, and a
//! disk whose holder tree carries a live system mount is marked as the
//! running system.
//!
//! [`parse_lsblk`] still accepts the older whitespace form of
//! `lsblk -dnpo NAME,SIZE,MODEL,TYPE`: the first token is `NAME`, the second
//! `SIZE`, the **last** `TYPE`, and everything in between `MODEL`, which keeps
//! multi-word model strings intact without depending on column widths.

use std::time::Duration;

use block_devices::{BlockDevice, Lsblk, lsblk};
use serde_json::Value;

/// GPT type used for an ordinary Linux data/filesystem partition.
pub const LINUX_FILESYSTEM_GUID: &str = "0fc63daf-8483-4772-8e79-3d69d8477de4";
/// Tilo-owned root GPT type used for target-independent same-disk discovery.
pub const TILO_ROOT_GUID: &str = "7a2e8d75-3f01-4e5a-b2c9-51f6ed90b8ab";
/// GPT type of the root a staged live installer boots from. Only the live
/// installer UKI resolves it (os/usr/libexec/tilo-root-discover), so it never
/// competes with the Tilo root installed beside it.
pub const TILO_STAGING_ROOT_GUID: &str = "d4c4daf6-f72c-43ee-b8e3-5ba32550311d";
/// GPT type used for an EFI System Partition.
pub const EFI_SYSTEM_GUID: &str = "c12a7328-f81f-11d2-ba4b-00a0c93ec93b";
/// Minimum prepared root accepted by the guided alongside flow (32 GiB).
pub const MIN_ALONGSIDE_BYTES: u64 = 32 * 1024 * 1024 * 1024;
/// Minimum ESP accepted for an alongside install (2 GiB).
///
/// Tilo keeps several signed UKIs for rollback, staged updates, and repair.
/// A typical 100--512 MiB Windows/Linux ESP cannot safely hold that lifecycle,
/// even if the first install happens to fit.
pub const MIN_ALONGSIDE_ESP_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// Keep partition starts and ends on MiB boundaries. Besides avoiding poor
/// flash/4Kn alignment, the first/last MiB conservatively stay clear of both
/// copies of the GPT metadata.
const PARTITION_ALIGNMENT_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartitionExtent {
    pub part_uuid: String,
    pub start_bytes: u64,
    pub size_bytes: u64,
}

/// A non-destructive plan that appends Tilo's three partitions inside one exact
/// unallocated GPT extent. It never represents space reclaimed by resizing.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GuidedAlongsidePlan {
    pub free_start_bytes: u64,
    pub free_size_bytes: u64,
    pub efi_start_bytes: u64,
    pub efi_size_bytes: u64,
    pub boot_start_bytes: u64,
    pub boot_size_bytes: u64,
    pub root_start_bytes: u64,
    pub root_size_bytes: u64,
}

impl GuidedAlongsidePlan {
    /// Restrict the existing append-only plan to an explicitly chosen allocation.
    /// The free extent in the returned plan is the exact write authorization,
    /// not all the unused space that happened to be available at discovery.
    pub fn with_capacity_bytes(&self, bytes: u64) -> Option<Self> {
        let overhead = MIN_ALONGSIDE_ESP_BYTES.checked_add(1 << 30)?;
        let minimum = overhead.checked_add(MIN_ALONGSIDE_BYTES)?;
        let available_end = self.free_start_bytes.checked_add(self.free_size_bytes)?;
        let end = self.free_start_bytes.checked_add(bytes)?;
        if bytes < minimum
            || bytes > self.free_size_bytes
            || !bytes.is_multiple_of(PARTITION_ALIGNMENT_BYTES)
            || !self
                .free_start_bytes
                .is_multiple_of(PARTITION_ALIGNMENT_BYTES)
            || self.efi_start_bytes != self.free_start_bytes
            || self.efi_size_bytes != MIN_ALONGSIDE_ESP_BYTES
            || self.boot_start_bytes != self.efi_start_bytes.checked_add(self.efi_size_bytes)?
            || self.boot_size_bytes != 1 << 30
            || self.root_start_bytes != self.free_start_bytes.checked_add(overhead)?
            || end > available_end
        {
            return None;
        }
        let mut plan = self.clone();
        plan.free_size_bytes = bytes;
        plan.root_size_bytes = end.checked_sub(plan.root_start_bytes)?;
        Some(plan)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DiskPartition {
    pub name: String,
    /// Byte offset from the start of the parent disk.
    pub start_bytes: u64,
    pub size_bytes: u64,
    /// GPT partition UUID used to bind the reviewed partition across device
    /// renumbering. Empty means discovery could not establish its identity.
    pub part_uuid: String,
    pub filesystem: String,
    pub label: String,
    pub mountpoint: String,
    pub part_type: String,
}

impl DiskPartition {
    /// A dual-boot root must be explicitly typed for Linux, unmounted, large
    /// enough, and have no filesystem signature. The backend repeats these
    /// checks immediately before formatting it.
    pub fn is_alongside_candidate(&self) -> bool {
        self.size_bytes >= MIN_ALONGSIDE_BYTES
            && !self.part_uuid.is_empty()
            && self.filesystem.is_empty()
            && self.mountpoint.is_empty()
            && (self.part_type.eq_ignore_ascii_case(LINUX_FILESYSTEM_GUID)
                || self.part_type.eq_ignore_ascii_case(TILO_ROOT_GUID))
    }

    pub fn is_alongside_boot_candidate(&self) -> bool {
        self.size_bytes >= 1024 * 1024 * 1024
            && self.size_bytes < MIN_ALONGSIDE_BYTES
            && !self.part_uuid.is_empty()
            && self.filesystem.is_empty()
            && self.mountpoint.is_empty()
            && self.part_type.eq_ignore_ascii_case(LINUX_FILESYSTEM_GUID)
    }

    pub fn is_efi(&self) -> bool {
        self.part_type.eq_ignore_ascii_case(EFI_SYSTEM_GUID)
            && self.filesystem.eq_ignore_ascii_case("vfat")
    }

    pub fn is_alongside_efi_candidate(&self) -> bool {
        self.is_efi()
            && self.size_bytes >= MIN_ALONGSIDE_ESP_BYTES
            && !self.part_uuid.is_empty()
            && self.mountpoint.is_empty()
    }
}

/// One installable target disk.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Disk {
    /// Full device path, e.g. `/dev/sda`.
    pub name: String,
    /// Human-readable size as `lsblk` prints it, e.g. `238.5G`.
    pub size: String,
    /// Device model, e.g. `Samsung SSD 860 EVO`. May be empty.
    pub model: String,
    pub size_bytes: u64,
    pub partition_table: String,
    pub partition_table_uuid: String,
    pub logical_sector_size: u64,
    /// Kernel device number observed during discovery. This is always bound,
    /// while WWN/serial add durable hardware identity when the device exports it.
    pub major_minor: String,
    pub wwn: String,
    pub serial: String,
    pub removable: bool,
    pub read_only: bool,
    pub is_system: bool,
    pub partitions: Vec<DiskPartition>,
    /// Device nodes directly below this disk (or the disk itself) whose
    /// subtree carries a running-system mount. For a staged (USB-free)
    /// installation these must be exactly the staging partitions.
    pub live_system_nodes: Vec<String>,
    /// Set only on the running-system disk that also holds a verified staged
    /// installer: the single guided plan that fills the staged target extent.
    /// Whole-disk erase and prepared partitions are never offered there.
    pub internal_plan: Option<GuidedAlongsidePlan>,
}

impl Disk {
    /// Single-line label shown on the disk-selection row.
    pub fn label(&self) -> String {
        if self.model.is_empty() {
            format!("{}  —  {}", self.name, self.size)
        } else {
            format!("{}  —  {}  —  {}", self.name, self.size, self.model)
        }
    }

    pub fn alongside_root(&self) -> Option<&DiskPartition> {
        exactly_one(
            self.partitions
                .iter()
                .filter(|p| p.is_alongside_candidate()),
        )
    }

    pub fn alongside_boot(&self) -> Option<&DiskPartition> {
        exactly_one(
            self.partitions
                .iter()
                .filter(|p| p.is_alongside_boot_candidate()),
        )
    }

    pub fn efi_partition(&self) -> Option<&DiskPartition> {
        exactly_one(
            self.partitions
                .iter()
                .filter(|p| p.is_alongside_efi_candidate()),
        )
    }

    /// Explain why discovery cannot select a prepared ESP using only metadata
    /// available to the unprivileged UI. FAT integrity and free bytes on an
    /// unmounted filesystem are intentionally absent: the privileged backend
    /// probes those immediately before installation.
    pub fn prepared_efi_issue(&self) -> Option<String> {
        let eligible = self
            .partitions
            .iter()
            .filter(|partition| partition.is_alongside_efi_candidate())
            .count();
        if eligible > 1 {
            return Some(
                "More than one prepared 2 GiB+ EFI partition is eligible; leave exactly one unmounted Tilo EFI destination."
                    .to_string(),
            );
        }
        if eligible == 1 {
            return None;
        }

        let efi_partitions = self
            .partitions
            .iter()
            .filter(|partition| partition.part_type.eq_ignore_ascii_case(EFI_SYSTEM_GUID));
        for partition in efi_partitions {
            if !partition.filesystem.eq_ignore_ascii_case("vfat") {
                return Some(format!(
                    "{} is an EFI System Partition but is not FAT32.",
                    partition.name
                ));
            }
            if partition.size_bytes < MIN_ALONGSIDE_ESP_BYTES {
                return Some(format!(
                    "{} is too small for Tilo boot images; a prepared EFI partition must be at least 2 GiB.",
                    partition.name
                ));
            }
            if !partition.mountpoint.is_empty() {
                return Some(format!(
                    "{} is mounted; unmount the prepared EFI partition, then refresh disks.",
                    partition.name
                ));
            }
            if partition.part_uuid.is_empty() {
                return Some(format!(
                    "{} has no stable GPT partition identity; repair the partition table, then refresh disks.",
                    partition.name
                ));
            }
        }
        None
    }

    pub fn can_install_alongside(&self) -> bool {
        self.internal_plan.is_none()
            && self.identity_ready()
            && !self.read_only
            && self.alongside_root().is_some()
            && self.alongside_boot().is_some()
            && self.efi_partition().is_some()
    }

    /// Whether discovery found storage normally managed by Windows. This is a
    /// preparation hint, not proof that a bootable Windows installation is
    /// present: an NTFS data partition is enough reason to avoid implying Tilo
    /// can safely clear hibernation state or resize it.
    pub fn has_windows_storage(&self) -> bool {
        self.partitions.iter().any(|partition| {
            matches!(
                partition.filesystem.to_ascii_lowercase().as_str(),
                "ntfs" | "ntfs3" | "bitlocker"
            ) || partition.label.to_ascii_lowercase().contains("windows")
        })
    }

    /// A preparation hint, not proof of a bootable Linux installation. Never
    /// infer unallocated device space from free bytes inside these filesystems.
    pub fn has_linux_storage(&self) -> bool {
        self.partitions.iter().any(|partition| {
            matches!(
                partition.filesystem.to_ascii_lowercase().as_str(),
                "ext2" | "ext3" | "ext4" | "btrfs" | "xfs" | "crypto_luks" | "luks" | "lvm2_member"
            )
        })
    }

    pub fn occupied_extents(&self) -> Option<Vec<PartitionExtent>> {
        let mut extents = self
            .partitions
            .iter()
            .map(|partition| {
                (!partition.part_uuid.is_empty()
                    && partition.start_bytes > 0
                    && partition.size_bytes > 0)
                    .then(|| PartitionExtent {
                        part_uuid: partition.part_uuid.to_ascii_lowercase(),
                        start_bytes: partition.start_bytes,
                        size_bytes: partition.size_bytes,
                    })
            })
            .collect::<Option<Vec<_>>>()?;
        extents.sort_by_key(|extent| extent.start_bytes);
        Some(extents)
    }

    /// Return every unallocated GPT extent that can hold a dedicated 2 GiB
    /// ESP plus a 1 GiB boot and >=32 GiB root, in on-disk order. Callers must require an
    /// explicit choice when this returns more than one plan.
    pub fn guided_alongside_plans(&self) -> Vec<GuidedAlongsidePlan> {
        if let Some(plan) = &self.internal_plan {
            return vec![plan.clone()];
        }
        if !self.partition_table.eq_ignore_ascii_case("gpt")
            || self.partition_table_uuid.is_empty()
            || self.logical_sector_size == 0
            || !PARTITION_ALIGNMENT_BYTES.is_multiple_of(self.logical_sector_size)
            || !self.identity_ready()
            || self.read_only
        {
            return Vec::new();
        }
        let Some(extents) = self.occupied_extents() else {
            return Vec::new();
        };
        let mut previous_end = 0;
        for extent in &extents {
            let Some(end) = extent.start_bytes.checked_add(extent.size_bytes) else {
                return Vec::new();
            };
            if extent.start_bytes < previous_end || end > self.size_bytes {
                return Vec::new();
            }
            previous_end = end;
        }
        let disk_end = align_down(
            self.size_bytes.saturating_sub(PARTITION_ALIGNMENT_BYTES),
            PARTITION_ALIGNMENT_BYTES,
        );
        let mut cursor = PARTITION_ALIGNMENT_BYTES;
        let mut candidates = Vec::new();
        for occupied in extents.iter().chain(std::iter::once(&PartitionExtent {
            part_uuid: String::new(),
            start_bytes: disk_end,
            size_bytes: 0,
        })) {
            let Some(start) = align_up(cursor, PARTITION_ALIGNMENT_BYTES) else {
                return Vec::new();
            };
            let end = align_down(
                occupied.start_bytes.min(disk_end),
                PARTITION_ALIGNMENT_BYTES,
            );
            if end > start {
                let Some(size) = end.checked_sub(start) else {
                    return Vec::new();
                };
                if size >= MIN_ALONGSIDE_ESP_BYTES + 1024 * 1024 * 1024 + MIN_ALONGSIDE_BYTES {
                    let Some(boot_start) = start.checked_add(MIN_ALONGSIDE_ESP_BYTES) else {
                        return Vec::new();
                    };
                    let Some(root_start) = boot_start.checked_add(1024 * 1024 * 1024) else {
                        return Vec::new();
                    };
                    let Some(root_size) =
                        size.checked_sub(MIN_ALONGSIDE_ESP_BYTES + 1024 * 1024 * 1024)
                    else {
                        return Vec::new();
                    };
                    candidates.push(GuidedAlongsidePlan {
                        free_start_bytes: start,
                        free_size_bytes: size,
                        efi_start_bytes: start,
                        efi_size_bytes: MIN_ALONGSIDE_ESP_BYTES,
                        boot_start_bytes: boot_start,
                        boot_size_bytes: 1024 * 1024 * 1024,
                        root_start_bytes: root_start,
                        root_size_bytes: root_size,
                    });
                }
            }
            let Some(end) = occupied.start_bytes.checked_add(occupied.size_bytes) else {
                return Vec::new();
            };
            cursor = cursor.max(end);
        }
        candidates
    }

    /// Convenience for flows that can proceed only when discovery is
    /// unambiguous. Multiple safe extents are never silently resolved.
    pub fn guided_alongside_plan(&self) -> Option<GuidedAlongsidePlan> {
        exactly_one_owned(self.guided_alongside_plans().into_iter())
    }

    /// Whether this disk holds the running installer and cannot be a target.
    /// A verified staged installation is the one exception.
    pub fn blocks_install(&self) -> bool {
        self.is_system && self.internal_plan.is_none()
    }

    /// Whether this is the running-system disk that holds a verified staged
    /// installer (and therefore also the retained OS).
    pub fn hosts_staged_installer(&self) -> bool {
        self.internal_plan.is_some()
    }

    /// Bind a staged (USB-free) installation to this disk. It applies only to
    /// the disk the installer is running from, and only when the disk, both
    /// staging partitions and the free target extent are exactly as staged
    /// and nothing but the staging partitions carries the running system.
    /// The privileged backend repeats every check before writing anything.
    ///
    /// # Errors
    ///
    /// Returns why the handoff does not describe this disk; the disk then
    /// stays unavailable exactly like any other running-system disk.
    pub fn apply_internal_handoff(
        &mut self,
        handoff: &crate::internal_handoff::Handoff,
    ) -> Result<(), String> {
        self.internal_plan = None;
        let plan = self.internal_plan_for(handoff)?;
        self.internal_plan = Some(plan);
        Ok(())
    }

    fn internal_plan_for(
        &self,
        handoff: &crate::internal_handoff::Handoff,
    ) -> Result<GuidedAlongsidePlan, String> {
        if !self.is_system {
            return Err("the staged installer is not on this disk".into());
        }
        if !self.partition_table.eq_ignore_ascii_case("gpt")
            || !self
                .partition_table_uuid
                .eq_ignore_ascii_case(&handoff.disk_guid)
            || self.size_bytes != handoff.disk_size_bytes
        {
            return Err("this disk differs from the one Tilo Imager prepared".into());
        }
        if self.read_only || !self.identity_ready() {
            return Err("this disk cannot be bound to a stable identity".into());
        }
        if !matches!(self.logical_sector_size, 512 | 4096) {
            return Err("unsupported logical sector size".into());
        }
        let esp = self.staged_partition(&handoff.staging_esp, EFI_SYSTEM_GUID)?;
        let root = self.staged_partition(&handoff.staging_root, TILO_STAGING_ROOT_GUID)?;
        if !self
            .live_system_nodes
            .iter()
            .all(|node| *node == esp.name || *node == root.name)
            || !self.live_system_nodes.contains(&root.name)
        {
            return Err("the running system is not exactly the staged installer".into());
        }
        if self
            .partitions
            .iter()
            .any(|p| !p.mountpoint.is_empty() && p.name != esp.name && p.name != root.name)
        {
            return Err("another partition on this disk is in use".into());
        }
        self.staged_target_plan(handoff.target_extent)
    }

    /// Inclusive LBA range of `partition` in this disk's logical sectors.
    fn lba_range(&self, partition: &DiskPartition) -> Option<(u64, u64)> {
        let sector = self.logical_sector_size;
        if sector == 0
            || !partition.start_bytes.is_multiple_of(sector)
            || !partition.size_bytes.is_multiple_of(sector)
            || partition.size_bytes == 0
        {
            return None;
        }
        let first = partition.start_bytes.checked_div(sector)?;
        let last = first
            .checked_add(partition.size_bytes.checked_div(sector)?)?
            .checked_sub(1)?;
        Some((first, last))
    }

    fn staged_partition(
        &self,
        identity: &crate::internal_handoff::StagedIdentity,
        part_type: &str,
    ) -> Result<&DiskPartition, String> {
        let partition = exactly_one(
            self.partitions
                .iter()
                .filter(|p| p.part_uuid.eq_ignore_ascii_case(&identity.partuuid)),
        )
        .ok_or_else(|| "a staged installer partition is missing".to_string())?;
        if !partition.part_type.eq_ignore_ascii_case(part_type)
            || self.lba_range(partition) != Some((identity.first_lba, identity.last_lba))
        {
            return Err("a staged installer partition changed after staging".into());
        }
        Ok(partition)
    }

    /// The guided plan that fills the staged target extent exactly: a 2 GiB
    /// ESP at its start and a root that ends where the staged installer
    /// begins, so reclaim can later grow the root in place.
    fn staged_target_plan(
        &self,
        target: crate::internal_handoff::Extent,
    ) -> Result<GuidedAlongsidePlan, String> {
        let malformed = || "the staged target space is malformed".to_string();
        let sector = self.logical_sector_size;
        let lbas = target
            .last_lba
            .checked_sub(target.first_lba)
            .and_then(|span| span.checked_add(1))
            .ok_or_else(malformed)?;
        let free_start_bytes = target.first_lba.checked_mul(sector).ok_or_else(malformed)?;
        let free_size_bytes = lbas.checked_mul(sector).ok_or_else(malformed)?;
        let free_end = free_start_bytes
            .checked_add(free_size_bytes)
            .ok_or_else(malformed)?;
        let boot_start_bytes = free_start_bytes
            .checked_add(MIN_ALONGSIDE_ESP_BYTES)
            .ok_or_else(malformed)?;
        let root_start_bytes = boot_start_bytes
            .checked_add(1024 * 1024 * 1024)
            .ok_or_else(malformed)?;
        let root_size_bytes = free_size_bytes
            .checked_sub(MIN_ALONGSIDE_ESP_BYTES + 1024 * 1024 * 1024)
            .ok_or_else(malformed)?;
        if !free_start_bytes.is_multiple_of(PARTITION_ALIGNMENT_BYTES)
            || !free_size_bytes.is_multiple_of(PARTITION_ALIGNMENT_BYTES)
            || root_size_bytes < MIN_ALONGSIDE_BYTES
            || free_end > self.size_bytes.saturating_sub(PARTITION_ALIGNMENT_BYTES)
        {
            return Err("the staged target space cannot hold Tilo".into());
        }
        if self.partitions.iter().any(|p| {
            self.lba_range(p)
                .is_none_or(|(first, last)| first <= target.last_lba && target.first_lba <= last)
        }) {
            return Err("the staged target space is no longer unallocated".into());
        }
        Ok(GuidedAlongsidePlan {
            free_start_bytes,
            free_size_bytes,
            efi_start_bytes: free_start_bytes,
            efi_size_bytes: MIN_ALONGSIDE_ESP_BYTES,
            boot_start_bytes,
            boot_size_bytes: 1024 * 1024 * 1024,
            root_start_bytes,
            root_size_bytes,
        })
    }

    /// Minimum identity needed for any destructive handoff. Device number
    /// closes same-session path reuse, while at least one hardware identifier
    /// keeps the binding durable across kernel renumbering.
    pub fn identity_ready(&self) -> bool {
        self.size_bytes > 0
            && !self.major_minor.is_empty()
            && (!self.wwn.is_empty() || !self.serial.is_empty())
    }

    pub fn contents_summary(&self) -> String {
        if self.partitions.is_empty() {
            return "Empty disk".to_string();
        }
        let known = self
            .partitions
            .iter()
            .filter_map(|p| (!p.filesystem.is_empty()).then_some(p.filesystem.as_str()))
            .collect::<Vec<_>>();
        if known.is_empty() {
            format!("{} partition(s)", self.partitions.len())
        } else {
            format!(
                "{} partition(s): {}",
                self.partitions.len(),
                known.join(", ")
            )
        }
    }
}

/// Return the candidate only when discovery is unambiguous. Formatting the
/// "first" blank partition is not safe: signature-free storage can still
/// contain unrecognised data, and enumeration order is not user consent.
fn exactly_one<'a>(
    mut candidates: impl Iterator<Item = &'a DiskPartition>,
) -> Option<&'a DiskPartition> {
    let candidate = candidates.next()?;
    candidates.next().is_none().then_some(candidate)
}

fn exactly_one_owned<T>(mut candidates: impl Iterator<Item = T>) -> Option<T> {
    let candidate = candidates.next()?;
    candidates.next().is_none().then_some(candidate)
}

fn align_up(value: u64, alignment: u64) -> Option<u64> {
    value
        .checked_add(alignment.checked_sub(1)?)?
        .checked_div(alignment)?
        .checked_mul(alignment)
}

#[allow(
    clippy::arithmetic_side_effects,
    reason = "the fixed one-MiB alignment is nonzero and division only reduces the value"
)]
fn align_down(value: u64, alignment: u64) -> u64 {
    value / alignment * alignment
}

/// Exactly the columns discovery reads, listed by full device path.
pub const LSBLK_COLUMNS: &str = "NAME,SIZE,START,MODEL,TYPE,FSTYPE,LABEL,MOUNTPOINTS,PARTTYPE,PARTUUID,PTTYPE,PTUUID,LOG-SEC,RM,RO,WWN,SERIAL,MAJ:MIN";

fn lsblk() -> Lsblk {
    Lsblk::new(LSBLK_COLUMNS)
        .full_paths(true)
        .output_limit(4 * 1024 * 1024)
        .timeout(Duration::from_secs(10))
}

/// Parse the output of `lsblk -dnpo NAME,SIZE,MODEL,TYPE` into the set of
/// whole-disk devices. Rows that aren't `TYPE == "disk"`, blank lines,
/// and malformed lines (fewer than the three mandatory columns) are
/// skipped.
pub fn parse_lsblk(output: &str) -> Vec<Disk> {
    if let Ok(json) = serde_json::from_str::<Value>(output) {
        return parse_lsblk_json(&json);
    }
    // Compatibility with old fixtures and unusually old util-linux builds.
    let mut disks = Vec::new();
    for raw in output.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        // NAME SIZE [MODEL ...] TYPE — at minimum NAME, SIZE, TYPE.
        let tokens: Vec<&str> = line.split_whitespace().collect();
        let Some((&ty, rest)) = tokens.split_last() else {
            continue;
        };
        if ty != "disk" {
            continue;
        }
        // Middle tokens (if any) are the model; empty when `rest` holds
        // exactly NAME and SIZE.
        let [name, size, model_tokens @ ..] = rest else {
            continue;
        };
        disks.push(Disk {
            name: (*name).to_string(),
            size: (*size).to_string(),
            model: model_tokens.join(" "),
            size_bytes: 0,
            partition_table: String::new(),
            partition_table_uuid: String::new(),
            logical_sector_size: 512,
            major_minor: String::new(),
            wwn: String::new(),
            serial: String::new(),
            removable: false,
            read_only: false,
            is_system: false,
            partitions: Vec::new(),
            live_system_nodes: Vec::new(),
            internal_plan: None,
        });
    }
    disks
}

fn parse_lsblk_json(json: &Value) -> Vec<Disk> {
    lsblk::parse(json).map_or_else(Vec::new, |devices| disks_from(&devices))
}

fn disks_from(devices: &[BlockDevice]) -> Vec<Disk> {
    devices
        .iter()
        .filter(|d| d.kind == "disk")
        .filter_map(|d| {
            let name = d.name.clone();
            let size_bytes = d.size;
            if !is_installable_device_name(&name) || size_bytes == 0 {
                return None;
            }
            let partitions: Vec<DiskPartition> =
                d.children.iter().filter_map(parse_partition).collect();
            // A normal encrypted Fedora/Tilo root is not mounted directly from
            // the partition: lsblk reports disk -> part -> crypt (and often
            // -> lvm) before the node carrying `/` or `/sysroot`. Keep only
            // direct partitions in the install plan, but inspect the complete
            // holder tree when deciding whether the whole disk is the running
            // system. Otherwise the UI offers a disk that the privileged
            // backend correctly rejects only after the user has reviewed it.
            let tree_is_live = |device: &BlockDevice| {
                device
                    .descendants()
                    .any(|node| is_live_system_mount(&mountpoints(node)))
            };
            let is_system = tree_is_live(d);
            let mut live_system_nodes: Vec<String> = d
                .children
                .iter()
                .filter(|child| tree_is_live(child))
                .map(|child| child.name.clone())
                .collect();
            if is_live_system_mount(&mountpoints(d)) {
                live_system_nodes.push(name.clone());
            }
            Some(Disk {
                name,
                size: ui::format::memory(size_bytes),
                model: text(d.model.as_ref()),
                size_bytes,
                partition_table: text(d.pttype.as_ref()),
                partition_table_uuid: text(d.ptuuid.as_ref()),
                logical_sector_size: d.log_sec.unwrap_or(0),
                major_minor: text(d.maj_min.as_ref()),
                wwn: text(d.wwn.as_ref()),
                serial: text(d.serial.as_ref()),
                removable: d.removable,
                read_only: d.read_only,
                is_system,
                partitions,
                live_system_nodes,
                internal_plan: None,
            })
        })
        .collect()
}

fn is_installable_device_name(name: &str) -> bool {
    !name.is_empty()
        && !matches!(
            name.strip_prefix("/dev/").unwrap_or(name),
            n if n.starts_with("zram") || n.starts_with("ram") || n.starts_with("loop")
        )
}

fn parse_partition(device: &BlockDevice) -> Option<DiskPartition> {
    if device.kind != "part" {
        return None;
    }
    Some(DiskPartition {
        name: device.name.clone(),
        // util-linux documents START in 512-byte sectors even when the device
        // has a different logical sector size.
        start_bytes: device.start.unwrap_or(0).saturating_mul(512),
        size_bytes: device.size,
        part_uuid: text(device.partuuid.as_ref()),
        filesystem: text(device.fstype.as_ref()),
        label: text(device.label.as_ref()),
        mountpoint: mountpoints(device),
        part_type: text(device.parttype.as_ref()),
    })
}

/// A single `MOUNTPOINT` is not sufficient here: on a Btrfs system one
/// partition commonly backs `/`, `/var`, and `/home`, and lsblk may choose any
/// one of them for the singular column. Keep a newline-separated
/// representation of every mount so existing empty/mounted checks remain
/// simple while running-system detection sees every mount.
fn mountpoints(device: &BlockDevice) -> String {
    device.all_mountpoints().join("\n")
}

fn is_live_system_mount(mountpoints: &str) -> bool {
    mountpoints.lines().map(str::trim).any(|mountpoint| {
        matches!(mountpoint, "/" | "/boot" | "/boot/efi" | "/sysroot")
            || mountpoint.starts_with("/sysroot/")
            || mountpoint == "/run/initramfs/live"
            || mountpoint.starts_with("/run/initramfs/live/")
            || mountpoint == "/run/rootfsbase"
            || mountpoint.starts_with("/run/rootfsbase/")
    })
}

fn text(value: Option<&String>) -> String {
    value.cloned().unwrap_or_default()
}

/// Run `lsblk` and parse its output. Returns an empty vec when `lsblk`
/// is missing or fails — the UI surfaces that as "No disks found".
pub fn read_disks() -> Vec<Disk> {
    lsblk()
        .output()
        .map_or_else(Vec::new, |out| parse_lsblk(&String::from_utf8_lossy(&out)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lsblk_inventory_parses_json_and_text_forms() {
        // Phase `parse_lsblk_matches_the_captured_inventory`.
        {
            // Real `lsblk` output for exactly LSBLK_COLUMNS, and the disks this
            // module derived from it before the shared parser existed.
            let disks = parse_lsblk(include_str!(
                "../../../system/block-devices/tests/fixtures/installer.json"
            ));
            assert_eq!(
                format!("{disks:#?}\n"),
                include_str!("../tests/fixtures/lsblk-disks.txt")
            );
        }
        // Phase `lsblk_lists_full_paths_in_the_portable_output_form`.
        {
            assert_eq!(
                lsblk().args(),
                ["-J", "-b", "-p", "--output", LSBLK_COLUMNS]
            );
        }
        // Phase `parses_multiword_model`.
        {
            let out = "/dev/sda 238.5G Samsung SSD 860 EVO disk\n";
            let disks = parse_lsblk(out);
            assert_eq!(disks.len(), 1);
            assert_eq!(disks[0].name, "/dev/sda");
            assert_eq!(disks[0].size, "238.5G");
            assert_eq!(disks[0].model, "Samsung SSD 860 EVO");
        }
        // Phase `parses_empty_model`.
        {
            // No model column: only NAME, SIZE, TYPE present.
            let out = "/dev/vda 64G disk\n";
            let disks = parse_lsblk(out);
            assert_eq!(disks.len(), 1);
            assert_eq!(disks[0].name, "/dev/vda");
            assert_eq!(disks[0].size, "64G");
            assert_eq!(disks[0].model, "");
        }
        // Phase `filters_non_disk_types`.
        {
            let out = "\
    /dev/sda 238.5G Samsung SSD disk
    /dev/sr0 1024M QEMU DVD-ROM rom
    /dev/loop0 4K  loop
    /dev/sdb 16G SanDisk Cruzer disk
    ";
            let disks = parse_lsblk(out);
            let names: Vec<&str> = disks.iter().map(|d| d.name.as_str()).collect();
            assert_eq!(names, vec!["/dev/sda", "/dev/sdb"]);
        }
        // Phase `skips_blank_and_malformed_lines`.
        {
            let out = "\n/dev/sda 238.5G Samsung disk\n\ngarbage\n/dev/sdb\n/dev/sdc 1T  disk\n";
            let disks = parse_lsblk(out);
            // "garbage" (1 token) and "/dev/sdb" (1 token) are dropped;
            // /dev/sda and /dev/sdc (empty model) survive.
            let names: Vec<&str> = disks.iter().map(|d| d.name.as_str()).collect();
            assert_eq!(names, vec!["/dev/sda", "/dev/sdc"]);
            assert_eq!(disks[1].model, "");
        }
        // Phase `empty_output_yields_no_disks`.
        {
            assert!(parse_lsblk("").is_empty());
            assert!(parse_lsblk("\n\n   \n").is_empty());
        }
        // Phase `label_with_and_without_model`.
        {
            let with = Disk {
                name: "/dev/sda".into(),
                size: "238.5G".into(),
                model: "Samsung SSD".into(),
                size_bytes: 0,
                partition_table: String::new(),
                partition_table_uuid: String::new(),
                logical_sector_size: 512,
                major_minor: String::new(),
                wwn: String::new(),
                serial: String::new(),
                removable: false,
                read_only: false,
                is_system: false,
                live_system_nodes: Vec::new(),
                internal_plan: None,
                partitions: Vec::new(),
            };
            assert_eq!(with.label(), "/dev/sda  —  238.5G  —  Samsung SSD");
            let without = Disk {
                name: "/dev/vda".into(),
                size: "64G".into(),
                model: String::new(),
                size_bytes: 0,
                partition_table: String::new(),
                partition_table_uuid: String::new(),
                logical_sector_size: 512,
                major_minor: String::new(),
                wwn: String::new(),
                serial: String::new(),
                removable: false,
                read_only: false,
                is_system: false,
                live_system_nodes: Vec::new(),
                internal_plan: None,
                partitions: Vec::new(),
            };
            assert_eq!(without.label(), "/dev/vda  —  64G");
        }
    }

    #[test]
    fn running_and_pseudo_disks_are_never_destructive_targets() {
        // Phase `memory_and_loop_backed_pseudo_disks_are_never_targets`.
        {
            for name in ["/dev/zram0", "/dev/ram0", "/dev/loop0"] {
                assert!(!is_installable_device_name(name), "accepted {name}");
            }
            for name in ["/dev/sda", "/dev/nvme0n1", "/dev/vdb", "/dev/mmcblk0"] {
                assert!(is_installable_device_name(name), "rejected {name}");
            }
        }
        // Phase `every_mountpoint_marks_a_multimounted_linux_system_disk_unavailable`.
        {
            let fixture = r#"{"blockdevices":[{"name":"/dev/nvme0n1","size":1000000000000,"model":"NVMe","type":"disk","rm":false,"ro":false,"serial":"SYSTEM-DISK","maj:min":"259:0","children":[{"name":"/dev/nvme0n1p3","size":900000000000,"start":2048,"type":"part","fstype":"btrfs","mountpoints":["/var/home","/var","/"],"parttype":"0fc63daf-8483-4772-8e79-3d69d8477de4","partuuid":"00000000-0000-0000-0000-000000000003"}]}]}"#;
            let disks = parse_lsblk(fixture);

            assert_eq!(disks.len(), 1);
            assert_eq!(disks[0].partitions[0].mountpoint, "/var/home\n/var\n/");
            assert!(
                disks[0].is_system,
                "the root mount must not be hidden by another mount of the same partition"
            );
        }
        // Phase `encrypted_and_lvm_descendants_mark_the_running_disk_unavailable`.
        {
            for holder in [
                r#"{"name":"/dev/mapper/cryptroot","size":900000000000,"type":"crypt","fstype":"btrfs","mountpoints":["/sysroot","/var/home"]}"#,
                r#"{"name":"/dev/mapper/system-root","size":900000000000,"type":"lvm","fstype":"ext4","mountpoints":["/"]}"#,
            ] {
                let fixture = format!(
                    r#"{{"blockdevices":[{{"name":"/dev/nvme0n1","size":1000000000000,"model":"NVMe","type":"disk","rm":false,"ro":false,"serial":"SYSTEM-DISK","maj:min":"259:0","children":[{{"name":"/dev/nvme0n1p3","size":900000000000,"start":2048,"type":"part","fstype":"crypto_LUKS","mountpoints":[null],"parttype":"{LINUX_FILESYSTEM_GUID}","partuuid":"00000000-0000-0000-0000-000000000003","children":[{holder}]}}]}}]}}"#
                );
                let disks = parse_lsblk(&fixture);

                assert_eq!(disks.len(), 1);
                assert_eq!(disks[0].partitions.len(), 1);
                assert!(
                    disks[0].is_system,
                    "a live mount below a partition holder must disable its physical disk"
                );
            }
        }
        // Phase `nested_non_system_mount_does_not_hide_an_installable_data_disk`.
        {
            let fixture = format!(
                r#"{{"blockdevices":[{{"name":"/dev/sdb","size":1000000000000,"model":"Data","type":"disk","rm":false,"ro":false,"serial":"DATA-DISK","maj:min":"8:16","children":[{{"name":"/dev/sdb1","size":900000000000,"start":2048,"type":"part","fstype":"crypto_LUKS","mountpoints":[null],"parttype":"{LINUX_FILESYSTEM_GUID}","partuuid":"00000000-0000-0000-0000-000000000004","children":[{{"name":"/dev/mapper/data","size":900000000000,"type":"crypt","fstype":"ext4","mountpoints":["/run/media/tilo/data"]}}]}}]}}]}}"#
            );
            let disks = parse_lsblk(&fixture);

            assert_eq!(disks.len(), 1);
            assert!(!disks[0].is_system);
        }
        // Phase `live_media_mounts_mark_the_installer_device_unavailable`.
        {
            for mountpoint in [
                "/run/initramfs/live",
                "/run/initramfs/live/rootfs",
                "/run/rootfsbase",
                "/run/rootfsbase/subdir",
                "/sysroot",
                "/sysroot/ostree/deploy/tilo",
            ] {
                assert!(is_live_system_mount(mountpoint), "missed {mountpoint}");
            }
            assert!(!is_live_system_mount("/run/media/tilo/data"));
        }
        // Phase `destructive_targets_require_live_identity_fields`.
        {
            let fixture = r#"{"blockdevices":[{"name":"/dev/vda","size":68719476736,"model":"VM","type":"disk","rm":false,"ro":false}]}"#;
            let disks = parse_lsblk(fixture);
            assert_eq!(disks.len(), 1);
            assert!(!disks[0].identity_ready());
            assert!(!disks[0].can_install_alongside());
        }
    }

    #[test]
    fn alongside_discovery_accepts_only_safe_unambiguous_partitions() {
        // Phase `json_discovery_finds_safe_alongside_root_and_efi`.
        {
            let fixture = format!(
                r#"{{"blockdevices":[{{"name":"/dev/nvme0n1","size":1000000000000,"model":"NVMe","type":"disk","rm":false,"ro":false,"wwn":"eui.0011223344556677","serial":"TILO-NVME","maj:min":"259:0","children":[{{"name":"/dev/nvme0n1p1","size":536870912,"type":"part","fstype":"vfat","label":"Windows EFI","mountpoint":null,"parttype":"{EFI_SYSTEM_GUID}","partuuid":"00000000-0000-0000-0000-000000000001"}},{{"name":"/dev/nvme0n1p2","size":500000000000,"type":"part","fstype":"ntfs","label":"Windows","mountpoint":null,"parttype":"basic","partuuid":"00000000-0000-0000-0000-000000000002"}},{{"name":"/dev/nvme0n1p3","size":68719476736,"type":"part","fstype":null,"label":null,"mountpoint":null,"parttype":"{LINUX_FILESYSTEM_GUID}","partuuid":"00000000-0000-0000-0000-000000000003"}},{{"name":"/dev/nvme0n1p4","size":2147483648,"type":"part","fstype":"vfat","label":"Tilo EFI","mountpoint":null,"parttype":"{EFI_SYSTEM_GUID}","partuuid":"00000000-0000-0000-0000-000000000004"}},{{"name":"/dev/nvme0n1p5","size":1073741824,"type":"part","fstype":null,"label":null,"mountpoint":null,"parttype":"{LINUX_FILESYSTEM_GUID}","partuuid":"00000000-0000-0000-0000-000000000005"}}]}}]}}"#
            );
            let disks = parse_lsblk(&fixture);
            assert_eq!(disks.len(), 1);
            assert!(disks[0].identity_ready());
            assert_eq!(disks[0].major_minor, "259:0");
            assert_eq!(disks[0].wwn, "eui.0011223344556677");
            assert_eq!(disks[0].serial, "TILO-NVME");
            assert!(disks[0].can_install_alongside());
            assert_eq!(disks[0].alongside_root().unwrap().name, "/dev/nvme0n1p3");
            assert_eq!(
                disks[0].alongside_root().unwrap().part_uuid,
                "00000000-0000-0000-0000-000000000003"
            );
            assert_eq!(disks[0].efi_partition().unwrap().name, "/dev/nvme0n1p4");
        }
        // Phase `alongside_rejects_small_or_mounted_efi_partitions`.
        {
            let mut p = DiskPartition {
                name: "/dev/sda1".into(),
                start_bytes: 1_048_576,
                size_bytes: MIN_ALONGSIDE_ESP_BYTES - 1,
                part_uuid: "00000000-0000-0000-0000-000000000001".into(),
                filesystem: "vfat".into(),
                label: "EFI".into(),
                mountpoint: String::new(),
                part_type: EFI_SYSTEM_GUID.into(),
            };
            assert!(p.is_efi());
            assert!(!p.is_alongside_efi_candidate());

            p.size_bytes = MIN_ALONGSIDE_ESP_BYTES;
            assert!(p.is_alongside_efi_candidate());
            p.mountpoint = "/boot/efi".into();
            assert!(!p.is_alongside_efi_candidate());
        }
        // Phase `linux_disk_keeps_existing_filesystems_out_of_the_tilo_candidates`.
        {
            let fixture = format!(
                r#"{{"blockdevices":[{{"name":"/dev/sda","size":500000000000,"model":"Linux disk","type":"disk","rm":false,"ro":false,"serial":"TILO-SATA","maj:min":"8:0","children":[{{"name":"/dev/sda1","size":2147483648,"type":"part","fstype":"vfat","label":"Tilo EFI","mountpoint":null,"parttype":"{EFI_SYSTEM_GUID}","partuuid":"00000000-0000-0000-0000-000000000001"}},{{"name":"/dev/sda2","size":200000000000,"type":"part","fstype":"ext4","label":"Existing Linux","mountpoint":null,"parttype":"{LINUX_FILESYSTEM_GUID}","partuuid":"00000000-0000-0000-0000-000000000002"}},{{"name":"/dev/sda3","size":68719476736,"type":"part","fstype":null,"label":null,"mountpoint":null,"parttype":"{LINUX_FILESYSTEM_GUID}","partuuid":"00000000-0000-0000-0000-000000000003"}},{{"name":"/dev/sda4","size":1073741824,"type":"part","fstype":null,"label":null,"mountpoint":null,"parttype":"{LINUX_FILESYSTEM_GUID}","partuuid":"00000000-0000-0000-0000-000000000004"}}]}}]}}"#
            );
            let disks = parse_lsblk(&fixture);
            assert!(disks[0].can_install_alongside());
            assert_eq!(disks[0].alongside_root().unwrap().name, "/dev/sda3");
            assert_eq!(disks[0].alongside_boot().unwrap().name, "/dev/sda4");
            assert_eq!(disks[0].efi_partition().unwrap().name, "/dev/sda1");
            assert_eq!(disks[0].partitions[1].filesystem, "ext4");
        }
        // Phase `alongside_rejects_partition_with_existing_signature`.
        {
            let p = DiskPartition {
                name: "/dev/sda3".into(),
                start_bytes: 1_048_576,
                size_bytes: MIN_ALONGSIDE_BYTES,
                part_uuid: "00000000-0000-0000-0000-000000000003".into(),
                filesystem: "ext4".into(),
                label: String::new(),
                mountpoint: String::new(),
                part_type: LINUX_FILESYSTEM_GUID.into(),
            };
            assert!(!p.is_alongside_candidate());
        }
        // Phase `alongside_refuses_ambiguous_root_or_efi_candidates`.
        {
            let fixture = format!(
                r#"{{"blockdevices":[{{"name":"/dev/nvme0n1","size":1000000000000,"model":"NVMe","type":"disk","rm":false,"ro":false,"maj:min":"259:0","children":[{{"name":"/dev/nvme0n1p1","size":2147483648,"type":"part","fstype":"vfat","label":"Tilo EFI A","mountpoint":null,"parttype":"{EFI_SYSTEM_GUID}","partuuid":"00000000-0000-0000-0000-000000000001"}},{{"name":"/dev/nvme0n1p2","size":2147483648,"type":"part","fstype":"vfat","label":"Tilo EFI B","mountpoint":null,"parttype":"{EFI_SYSTEM_GUID}","partuuid":"00000000-0000-0000-0000-000000000002"}},{{"name":"/dev/nvme0n1p3","size":68719476736,"type":"part","fstype":null,"label":null,"mountpoint":null,"parttype":"{LINUX_FILESYSTEM_GUID}","partuuid":"00000000-0000-0000-0000-000000000003"}},{{"name":"/dev/nvme0n1p4","size":68719476736,"type":"part","fstype":null,"label":null,"mountpoint":null,"parttype":"{LINUX_FILESYSTEM_GUID}","partuuid":"00000000-0000-0000-0000-000000000004"}}]}}]}}"#
            );
            let disks = parse_lsblk(&fixture);
            assert_eq!(disks.len(), 1);
            assert!(disks[0].alongside_root().is_none());
            assert!(disks[0].efi_partition().is_none());
            assert!(!disks[0].can_install_alongside());
        }
        // Phase `alongside_partitions_require_gpt_partuuid`.
        {
            let mut root = DiskPartition {
                name: "/dev/vda3".into(),
                start_bytes: 1_048_576,
                size_bytes: MIN_ALONGSIDE_BYTES,
                part_uuid: String::new(),
                filesystem: String::new(),
                label: String::new(),
                mountpoint: String::new(),
                part_type: LINUX_FILESYSTEM_GUID.into(),
            };
            assert!(!root.is_alongside_candidate());
            root.part_uuid = "00000000-0000-0000-0000-000000000003".into();
            assert!(root.is_alongside_candidate());
            root.part_type = TILO_ROOT_GUID.into();
            assert!(root.is_alongside_candidate());
        }
    }

    #[test]
    fn alongside_hints_explain_efi_failures_and_windows_preparation() {
        // Phase `prepared_efi_issue_explains_discovery_time_failures`.
        {
            let mut disk = Disk {
                name: "/dev/sda".into(),
                size: "100 GiB".into(),
                model: String::new(),
                size_bytes: 100 * 1024 * 1024 * 1024,
                partition_table: "gpt".into(),
                partition_table_uuid: "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".into(),
                logical_sector_size: 512,
                major_minor: "8:0".into(),
                wwn: String::new(),
                serial: "efi-diagnostic".into(),
                removable: false,
                read_only: false,
                is_system: false,
                live_system_nodes: Vec::new(),
                internal_plan: None,
                partitions: vec![DiskPartition {
                    name: "/dev/sda1".into(),
                    start_bytes: 1024 * 1024,
                    size_bytes: MIN_ALONGSIDE_ESP_BYTES - 1,
                    part_uuid: "00000000-0000-0000-0000-000000000001".into(),
                    filesystem: "vfat".into(),
                    label: "EFI".into(),
                    mountpoint: String::new(),
                    part_type: EFI_SYSTEM_GUID.into(),
                }],
            };
            assert!(disk.prepared_efi_issue().unwrap().contains("too small"));

            disk.partitions[0].size_bytes = MIN_ALONGSIDE_ESP_BYTES;
            disk.partitions[0].mountpoint = "/mnt/efi".into();
            assert!(disk.prepared_efi_issue().unwrap().contains("mounted"));
            disk.partitions[0].mountpoint.clear();
            disk.partitions[0].filesystem = "ext4".into();
            assert!(disk.prepared_efi_issue().unwrap().contains("not FAT32"));
            disk.partitions[0].filesystem = "vfat".into();
            disk.partitions[0].part_uuid.clear();
            assert!(
                disk.prepared_efi_issue()
                    .unwrap()
                    .contains("no stable GPT partition identity")
            );
        }
        // Phase `windows_preparation_hint_recognizes_ntfs_and_bitlocker_storage`.
        {
            let mut disk = Disk {
                name: "/dev/sda".into(),
                size: "100 GiB".into(),
                model: String::new(),
                size_bytes: 100 * 1024 * 1024 * 1024,
                partition_table: "gpt".into(),
                partition_table_uuid: "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".into(),
                logical_sector_size: 512,
                major_minor: "8:0".into(),
                wwn: String::new(),
                serial: "windows-hint".into(),
                removable: false,
                read_only: false,
                is_system: false,
                live_system_nodes: Vec::new(),
                internal_plan: None,
                partitions: vec![DiskPartition {
                    name: "/dev/sda1".into(),
                    start_bytes: 1024 * 1024,
                    size_bytes: 50 * 1024 * 1024 * 1024,
                    part_uuid: "00000000-0000-0000-0000-000000000001".into(),
                    filesystem: "ntfs3".into(),
                    label: String::new(),
                    mountpoint: String::new(),
                    part_type: "ebd0a0a2-b9e5-4433-87c0-68b6b72699c7".into(),
                }],
            };
            assert!(disk.has_windows_storage());

            disk.partitions[0].filesystem = "BitLocker".into();
            assert!(disk.has_windows_storage());
            disk.partitions[0].filesystem = "ext4".into();
            assert!(!disk.has_windows_storage());
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "scenario test: each labelled phase keeps one former test's fixture and assertions"
    )]
    fn guided_plan_uses_one_free_extent_and_honours_only_a_matching_handoff() {
        // Phase `guided_plan_uses_one_aligned_gpt_free_extent_without_resizing`.
        {
            let gib = 1024_u64 * 1024 * 1024;
            let fixture = format!(
                r#"{{"blockdevices":[{{"name":"/dev/nvme0n1","size":{},"start":null,"model":"NVMe","type":"disk","pttype":"gpt","ptuuid":"aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee","log-sec":512,"rm":false,"ro":false,"wwn":"eui.0011223344556677","serial":"TILO-NVME","maj:min":"259:0","children":[{{"name":"/dev/nvme0n1p1","size":{},"start":2048,"type":"part","fstype":"ntfs","label":"Windows","mountpoint":null,"parttype":"basic","partuuid":"00000000-0000-0000-0000-000000000001"}}]}}]}}"#,
                200 * gib,
                100 * gib - PARTITION_ALIGNMENT_BYTES,
            );
            let disk = parse_lsblk(&fixture).remove(0);
            let plan = disk.guided_alongside_plan().expect("one safe free extent");
            assert_eq!(plan.free_start_bytes, 100 * gib);
            assert_eq!(plan.efi_start_bytes, 100 * gib);
            assert_eq!(plan.efi_size_bytes, 2 * gib);
            assert_eq!(plan.boot_start_bytes, 102 * gib);
            assert_eq!(plan.boot_size_bytes, gib);
            assert_eq!(plan.root_start_bytes, 103 * gib);
            assert_eq!(plan.root_size_bytes, 97 * gib - PARTITION_ALIGNMENT_BYTES);
            assert_eq!(disk.occupied_extents().unwrap().len(), 1);
        }
        // Phase `guided_plan_rejects_non_gpt_and_ambiguous_free_extents`.
        {
            let gib = 1024_u64 * 1024 * 1024;
            let mut disk = Disk {
                name: "/dev/vda".into(),
                size: "200 GiB".into(),
                model: "VM".into(),
                size_bytes: 200 * gib,
                partition_table: "dos".into(),
                partition_table_uuid: "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".into(),
                logical_sector_size: 512,
                major_minor: "252:0".into(),
                wwn: String::new(),
                serial: "guided-test".into(),
                removable: false,
                read_only: false,
                is_system: false,
                live_system_nodes: Vec::new(),
                internal_plan: None,
                partitions: Vec::new(),
            };
            assert!(
                disk.guided_alongside_plan().is_none(),
                "MBR is never mutated"
            );
            disk.partition_table = "gpt".into();
            disk.partitions.push(DiskPartition {
                name: "/dev/vda1".into(),
                start_bytes: 80 * gib,
                size_bytes: 20 * gib,
                part_uuid: "00000000-0000-0000-0000-000000000001".into(),
                filesystem: "ntfs".into(),
                label: "middle".into(),
                mountpoint: String::new(),
                part_type: "basic".into(),
            });
            let plans = disk.guided_alongside_plans();
            assert_eq!(plans.len(), 2, "both eligible gaps must be offered");
            assert!(
                disk.guided_alongside_plan().is_none(),
                "two eligible gaps must never be silently selected"
            );
            assert!(plans[0].free_start_bytes < plans[1].free_start_bytes);
        }
        // Phase `verified_staged_installer_disk_offers_only_the_prepared_guided_plan`.
        {
            use crate::internal_handoff::tests::{TARGET, handoff};
            let mut disk = staged_disk(|_| {});
            assert!(disk.is_system && disk.blocks_install());
            assert_eq!(disk.live_system_nodes, vec!["/dev/nvme0n1p4".to_string()]);
            disk.apply_internal_handoff(&handoff()).unwrap();
            assert!(!disk.blocks_install());
            assert!(disk.hosts_staged_installer());
            assert!(
                !disk.can_install_alongside(),
                "prepared partitions are never offered here"
            );
            let plans = disk.guided_alongside_plans();
            assert_eq!(plans.len(), 1);
            let plan = &plans[0];
            assert_eq!(plan.free_start_bytes, TARGET.0 * 512);
            assert_eq!(plan.free_size_bytes, (TARGET.1 - TARGET.0 + 1) * 512);
            assert_eq!(plan.efi_start_bytes, plan.free_start_bytes);
            assert_eq!(plan.efi_size_bytes, MIN_ALONGSIDE_ESP_BYTES);
            assert_eq!(
                plan.root_start_bytes + plan.root_size_bytes,
                (TARGET.1 + 1) * 512,
                "the Tilo root must end where the staged installer begins"
            );
        }
        // Phase `staged_handoff_that_does_not_describe_the_disk_keeps_it_unavailable`.
        {
            use crate::internal_handoff::tests::{TARGET, handoff};
            type Mutation = Box<dyn Fn(&mut serde_json::Value)>;
            let cases: Vec<(&str, Mutation)> = vec![
                (
                    "guid",
                    Box::new(|j| {
                        j["blockdevices"][0]["ptuuid"] =
                            "99999999-2222-4333-8444-555555555555".into();
                    }),
                ),
                (
                    "size",
                    Box::new(|j| {
                        j["blockdevices"][0]["size"] = (255u64 << 30).into();
                    }),
                ),
                (
                    "esp missing",
                    Box::new(|j| {
                        j["blockdevices"][0]["children"][2]["partuuid"] =
                            "cccccccc-0000-4000-8000-000000000009".into();
                    }),
                ),
                (
                    "root not a staging root",
                    Box::new(|j| {
                        j["blockdevices"][0]["children"][3]["parttype"] =
                            LINUX_FILESYSTEM_GUID.into();
                    }),
                ),
                (
                    "root moved",
                    Box::new(|j| {
                        j["blockdevices"][0]["children"][3]["start"] =
                            (crate::internal_handoff::tests::STAGING_ROOT.0 + 2048).into();
                    }),
                ),
                (
                    "root elsewhere",
                    Box::new(|j| {
                        j["blockdevices"][0]["children"][3]["mountpoints"] =
                            serde_json::json!([null]);
                    }),
                ),
                (
                    "windows mounted",
                    Box::new(|j| {
                        j["blockdevices"][0]["children"][1]["mountpoints"] =
                            serde_json::json!(["/mnt/c"]);
                    }),
                ),
                (
                    "extent used",
                    Box::new(|j| {
                        let used = serde_json::json!({"name": "/dev/nvme0n1p5", "type": "part",
                            "start": TARGET.0 + 2048, "size": 1u64 << 30,
                            "partuuid": "dddddddd-0000-4000-8000-000000000004",
                            "parttype": LINUX_FILESYSTEM_GUID, "mountpoints": [null]});
                        j["blockdevices"][0]["children"]
                            .as_array_mut()
                            .unwrap()
                            .push(used);
                    }),
                ),
            ];
            for (label, mutate) in cases {
                let mut disk = staged_disk(|j| mutate(j));
                assert!(
                    disk.apply_internal_handoff(&handoff()).is_err(),
                    "accepted {label}"
                );
                assert!(disk.internal_plan.is_none());
                assert!(
                    disk.blocks_install() || !disk.is_system,
                    "{label} became selectable"
                );
            }
            let mut not_live = staged_disk(|j| {
                j["blockdevices"][0]["children"][3]["mountpoints"] = serde_json::json!([null]);
            });
            assert!(!not_live.is_system);
            assert!(
                not_live.apply_internal_handoff(&handoff()).is_err(),
                "only the running disk qualifies"
            );
        }
    }

    fn staged_disk(mutate: impl FnOnce(&mut serde_json::Value)) -> Disk {
        let mut json = crate::internal_handoff::tests::lsblk_json();
        mutate(&mut json);
        parse_lsblk(&json.to_string()).remove(0)
    }
}
