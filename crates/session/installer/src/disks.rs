//! Target-disk and partition discovery.
//!
//! We list block devices with:
//!
//! ```text
//! lsblk -dnpo NAME,SIZE,MODEL,TYPE
//! ```
//!
//! * `-d` — no partitions / holders (whole devices only).
//! * `-n` — no header row.
//! * `-p` — full device path (`/dev/sda`, not `sda`).
//! * `-o NAME,SIZE,MODEL,TYPE` — the four columns we care about.
//!
//! Only rows whose `TYPE` is exactly `disk` are kept (filters out `rom`,
//! `loop`, etc.). The `MODEL` column may be empty (multiple spaces) or
//! contain spaces — so the parser treats the first token as `NAME`, the
//! second as `SIZE`, the **last** as `TYPE`, and everything in between as
//! `MODEL`. That keeps us robust to multi-word model strings without
//! depending on fixed column widths.

use std::process::Command;

use serde_json::Value;

/// GPT type used for an ordinary Linux data/filesystem partition.
pub const LINUX_FILESYSTEM_GUID: &str = "0fc63daf-8483-4772-8e79-3d69d8477de4";
/// GPT type used for an EFI System Partition.
pub const EFI_SYSTEM_GUID: &str = "c12a7328-f81f-11d2-ba4b-00a0c93ec93b";
/// Minimum prepared root accepted by the guided alongside flow (32 GiB).
pub const MIN_ALONGSIDE_BYTES: u64 = 32 * 1024 * 1024 * 1024;
/// Minimum ESP accepted for an alongside install (2 GiB).
///
/// qOS keeps several signed UKIs for rollback, staged updates, and repair.
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

/// A non-destructive plan that appends qOS's two partitions inside one exact
/// unallocated GPT extent. It never represents space reclaimed by resizing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuidedAlongsidePlan {
    pub free_start_bytes: u64,
    pub free_size_bytes: u64,
    pub efi_start_bytes: u64,
    pub efi_size_bytes: u64,
    pub root_start_bytes: u64,
    pub root_size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
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
#[derive(Debug, Clone, PartialEq, Eq)]
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
                "More than one prepared 2 GiB+ EFI partition is eligible; leave exactly one unmounted qOS EFI destination."
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
                    "{} is too small for qOS boot images; a prepared EFI partition must be at least 2 GiB.",
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
        self.identity_ready()
            && !self.read_only
            && self.alongside_root().is_some()
            && self.efi_partition().is_some()
    }

    /// Whether discovery found storage normally managed by Windows. This is a
    /// preparation hint, not proof that a bootable Windows installation is
    /// present: an NTFS data partition is enough reason to avoid implying qOS
    /// can safely clear hibernation state or resize it.
    pub fn has_windows_storage(&self) -> bool {
        self.partitions.iter().any(|partition| {
            matches!(
                partition.filesystem.to_ascii_lowercase().as_str(),
                "ntfs" | "ntfs3" | "bitlocker"
            ) || partition.label.to_ascii_lowercase().contains("windows")
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
    /// ESP plus a >=32 GiB root, in on-disk order. Callers must require an
    /// explicit choice when this returns more than one plan.
    pub fn guided_alongside_plans(&self) -> Vec<GuidedAlongsidePlan> {
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
            let start = align_up(cursor, PARTITION_ALIGNMENT_BYTES);
            let end = align_down(
                occupied.start_bytes.min(disk_end),
                PARTITION_ALIGNMENT_BYTES,
            );
            if end > start {
                let Some(size) = end.checked_sub(start) else {
                    return Vec::new();
                };
                if size >= MIN_ALONGSIDE_ESP_BYTES + MIN_ALONGSIDE_BYTES {
                    let Some(root_start) = start.checked_add(MIN_ALONGSIDE_ESP_BYTES) else {
                        return Vec::new();
                    };
                    let Some(root_size) = size.checked_sub(MIN_ALONGSIDE_ESP_BYTES) else {
                        return Vec::new();
                    };
                    candidates.push(GuidedAlongsidePlan {
                        free_start_bytes: start,
                        free_size_bytes: size,
                        efi_start_bytes: start,
                        efi_size_bytes: MIN_ALONGSIDE_ESP_BYTES,
                        root_start_bytes: root_start,
                        root_size_bytes: root_size,
                    });
                }
            }
            cursor = cursor.max(occupied.start_bytes.saturating_add(occupied.size_bytes));
        }
        candidates
    }

    /// Convenience for flows that can proceed only when discovery is
    /// unambiguous. Multiple safe extents are never silently resolved.
    pub fn guided_alongside_plan(&self) -> Option<GuidedAlongsidePlan> {
        exactly_one_owned(self.guided_alongside_plans().into_iter())
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

#[allow(
    clippy::arithmetic_side_effects,
    reason = "the fixed one-MiB alignment is nonzero; saturation handles addition overflow"
)]
fn align_up(value: u64, alignment: u64) -> u64 {
    value.saturating_add(alignment - 1) / alignment * alignment
}

#[allow(
    clippy::arithmetic_side_effects,
    reason = "the fixed one-MiB alignment is nonzero and division only reduces the value"
)]
fn align_down(value: u64, alignment: u64) -> u64 {
    value / alignment * alignment
}

/// The `lsblk` argument vector. Kept as a constant so the call site and
/// any docs/tests reference exactly one source of truth.
pub const LSBLK_ARGS: [&str; 5] = [
    "-J",
    "-b",
    "-p",
    "--output",
    "NAME,SIZE,START,MODEL,TYPE,FSTYPE,LABEL,MOUNTPOINTS,PARTTYPE,PARTUUID,PTTYPE,PTUUID,LOG-SEC,RM,RO,WWN,SERIAL,MAJ:MIN",
];

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
        });
    }
    disks
}

fn parse_lsblk_json(json: &Value) -> Vec<Disk> {
    let Some(devices) = json.get("blockdevices").and_then(Value::as_array) else {
        return Vec::new();
    };
    devices
        .iter()
        .filter(|d| field(d, "type") == "disk")
        .filter_map(|d| {
            let name = field(d, "name");
            let size_bytes = number(d, "size");
            if !is_installable_device_name(&name) || size_bytes == 0 {
                return None;
            }
            let partitions: Vec<DiskPartition> = d
                .get("children")
                .and_then(Value::as_array)
                .map(|children| children.iter().filter_map(parse_partition).collect())
                .unwrap_or_default();
            // A normal encrypted Fedora/qOS root is not mounted directly from
            // the partition: lsblk reports disk -> part -> crypt (and often
            // -> lvm) before the node carrying `/` or `/sysroot`. Keep only
            // direct partitions in the install plan, but inspect the complete
            // holder tree when deciding whether the whole disk is the running
            // system. Otherwise the UI offers a disk that the privileged
            // backend correctly rejects only after the user has reviewed it.
            let is_system = device_tree_has_live_system_mount(d);
            Some(Disk {
                name,
                size: format_bytes(size_bytes),
                model: field(d, "model"),
                size_bytes,
                partition_table: field(d, "pttype"),
                partition_table_uuid: field(d, "ptuuid"),
                logical_sector_size: number(d, "log-sec"),
                major_minor: field(d, "maj:min"),
                wwn: field(d, "wwn"),
                serial: field(d, "serial"),
                removable: boolean(d, "rm"),
                read_only: boolean(d, "ro"),
                is_system,
                partitions,
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

fn parse_partition(value: &Value) -> Option<DiskPartition> {
    if field(value, "type") != "part" {
        return None;
    }
    Some(DiskPartition {
        name: field(value, "name"),
        // util-linux documents START in 512-byte sectors even when the device
        // has a different logical sector size.
        start_bytes: number(value, "start").saturating_mul(512),
        size_bytes: number(value, "size"),
        part_uuid: field(value, "partuuid"),
        filesystem: field(value, "fstype"),
        label: field(value, "label"),
        mountpoint: mountpoints(value),
        part_type: field(value, "parttype"),
    })
}

/// `MOUNTPOINTS` is an array in lsblk JSON. A single `MOUNTPOINT` is not
/// sufficient here: on a Btrfs system one partition commonly backs `/`,
/// `/var`, and `/home`, and lsblk may choose any one of them for the singular
/// column. Keep a newline-separated representation so existing empty/mounted
/// checks remain simple while running-system detection sees every mount.
fn mountpoints(value: &Value) -> String {
    if let Some(points) = value.get("mountpoints").and_then(Value::as_array) {
        return points
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|point| !point.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
    }
    // Compatibility with older fixtures and callers that still provide the
    // singular field.
    field(value, "mountpoint")
}

fn device_tree_has_live_system_mount(value: &Value) -> bool {
    is_live_system_mount(&mountpoints(value))
        || value
            .get("children")
            .and_then(Value::as_array)
            .is_some_and(|children| children.iter().any(device_tree_has_live_system_mount))
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

fn field(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn number(value: &Value, key: &str) -> u64 {
    value
        .get(key)
        .and_then(|v| v.as_u64().or_else(|| v.as_str()?.parse().ok()))
        .unwrap_or(0)
}

fn boolean(value: &Value, key: &str) -> bool {
    value
        .get(key)
        .and_then(|v| v.as_bool().or_else(|| Some(v.as_u64()? != 0)))
        .unwrap_or(false)
}

fn format_bytes(bytes: u64) -> String {
    const GIB: u64 = 1024 * 1024 * 1024;
    if bytes >= GIB {
        let tenths = bytes.saturating_mul(10) / GIB;
        format!("{}.{} GiB", tenths / 10, tenths % 10)
    } else {
        format!("{} MiB", bytes / (1024 * 1024))
    }
}

/// Run `lsblk` and parse its output. Returns an empty vec when `lsblk`
/// is missing or fails — the UI surfaces that as "No disks found".
pub fn read_disks() -> Vec<Disk> {
    match Command::new("lsblk").args(LSBLK_ARGS).output() {
        Ok(out) if out.status.success() => parse_lsblk(&String::from_utf8_lossy(&out.stdout)),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lsblk_arguments_use_the_portable_output_form() {
        assert_eq!(LSBLK_ARGS[3], "--output");
        assert!(!LSBLK_ARGS[4].starts_with('='));
    }

    #[test]
    fn memory_and_loop_backed_pseudo_disks_are_never_targets() {
        for name in ["/dev/zram0", "/dev/ram0", "/dev/loop0"] {
            assert!(!is_installable_device_name(name), "accepted {name}");
        }
        for name in ["/dev/sda", "/dev/nvme0n1", "/dev/vdb", "/dev/mmcblk0"] {
            assert!(is_installable_device_name(name), "rejected {name}");
        }
    }

    #[test]
    fn every_mountpoint_marks_a_multimounted_linux_system_disk_unavailable() {
        let fixture = r#"{"blockdevices":[{"name":"/dev/nvme0n1","size":1000000000000,"model":"NVMe","type":"disk","rm":false,"ro":false,"serial":"SYSTEM-DISK","maj:min":"259:0","children":[{"name":"/dev/nvme0n1p3","size":900000000000,"start":2048,"type":"part","fstype":"btrfs","mountpoints":["/var/home","/var","/"],"parttype":"0fc63daf-8483-4772-8e79-3d69d8477de4","partuuid":"00000000-0000-0000-0000-000000000003"}]}]}"#;
        let disks = parse_lsblk(fixture);

        assert_eq!(disks.len(), 1);
        assert_eq!(disks[0].partitions[0].mountpoint, "/var/home\n/var\n/");
        assert!(
            disks[0].is_system,
            "the root mount must not be hidden by another mount of the same partition"
        );
    }

    #[test]
    fn encrypted_and_lvm_descendants_mark_the_running_disk_unavailable() {
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

    #[test]
    fn nested_non_system_mount_does_not_hide_an_installable_data_disk() {
        let fixture = format!(
            r#"{{"blockdevices":[{{"name":"/dev/sdb","size":1000000000000,"model":"Data","type":"disk","rm":false,"ro":false,"serial":"DATA-DISK","maj:min":"8:16","children":[{{"name":"/dev/sdb1","size":900000000000,"start":2048,"type":"part","fstype":"crypto_LUKS","mountpoints":[null],"parttype":"{LINUX_FILESYSTEM_GUID}","partuuid":"00000000-0000-0000-0000-000000000004","children":[{{"name":"/dev/mapper/data","size":900000000000,"type":"crypt","fstype":"ext4","mountpoints":["/run/media/qos/data"]}}]}}]}}]}}"#
        );
        let disks = parse_lsblk(&fixture);

        assert_eq!(disks.len(), 1);
        assert!(!disks[0].is_system);
    }

    #[test]
    fn live_media_mounts_mark_the_installer_device_unavailable() {
        for mountpoint in [
            "/run/initramfs/live",
            "/run/initramfs/live/rootfs",
            "/run/rootfsbase",
            "/run/rootfsbase/subdir",
            "/sysroot",
            "/sysroot/ostree/deploy/qos",
        ] {
            assert!(is_live_system_mount(mountpoint), "missed {mountpoint}");
        }
        assert!(!is_live_system_mount("/run/media/qos/data"));
    }

    #[test]
    fn parses_multiword_model() {
        let out = "/dev/sda 238.5G Samsung SSD 860 EVO disk\n";
        let disks = parse_lsblk(out);
        assert_eq!(disks.len(), 1);
        assert_eq!(disks[0].name, "/dev/sda");
        assert_eq!(disks[0].size, "238.5G");
        assert_eq!(disks[0].model, "Samsung SSD 860 EVO");
    }

    #[test]
    fn parses_empty_model() {
        // No model column: only NAME, SIZE, TYPE present.
        let out = "/dev/vda 64G disk\n";
        let disks = parse_lsblk(out);
        assert_eq!(disks.len(), 1);
        assert_eq!(disks[0].name, "/dev/vda");
        assert_eq!(disks[0].size, "64G");
        assert_eq!(disks[0].model, "");
    }

    #[test]
    fn filters_non_disk_types() {
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

    #[test]
    fn skips_blank_and_malformed_lines() {
        let out = "\n/dev/sda 238.5G Samsung disk\n\ngarbage\n/dev/sdb\n/dev/sdc 1T  disk\n";
        let disks = parse_lsblk(out);
        // "garbage" (1 token) and "/dev/sdb" (1 token) are dropped;
        // /dev/sda and /dev/sdc (empty model) survive.
        let names: Vec<&str> = disks.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, vec!["/dev/sda", "/dev/sdc"]);
        assert_eq!(disks[1].model, "");
    }

    #[test]
    fn empty_output_yields_no_disks() {
        assert!(parse_lsblk("").is_empty());
        assert!(parse_lsblk("\n\n   \n").is_empty());
    }

    #[test]
    fn label_with_and_without_model() {
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
            partitions: Vec::new(),
        };
        assert_eq!(without.label(), "/dev/vda  —  64G");
    }

    #[test]
    fn json_discovery_finds_safe_alongside_root_and_efi() {
        let fixture = format!(
            r#"{{"blockdevices":[{{"name":"/dev/nvme0n1","size":1000000000000,"model":"NVMe","type":"disk","rm":false,"ro":false,"wwn":"eui.0011223344556677","serial":"QOS-NVME","maj:min":"259:0","children":[{{"name":"/dev/nvme0n1p1","size":536870912,"type":"part","fstype":"vfat","label":"Windows EFI","mountpoint":null,"parttype":"{EFI_SYSTEM_GUID}","partuuid":"00000000-0000-0000-0000-000000000001"}},{{"name":"/dev/nvme0n1p2","size":500000000000,"type":"part","fstype":"ntfs","label":"Windows","mountpoint":null,"parttype":"basic","partuuid":"00000000-0000-0000-0000-000000000002"}},{{"name":"/dev/nvme0n1p3","size":68719476736,"type":"part","fstype":null,"label":null,"mountpoint":null,"parttype":"{LINUX_FILESYSTEM_GUID}","partuuid":"00000000-0000-0000-0000-000000000003"}},{{"name":"/dev/nvme0n1p4","size":2147483648,"type":"part","fstype":"vfat","label":"qOS EFI","mountpoint":null,"parttype":"{EFI_SYSTEM_GUID}","partuuid":"00000000-0000-0000-0000-000000000004"}}]}}]}}"#
        );
        let disks = parse_lsblk(&fixture);
        assert_eq!(disks.len(), 1);
        assert!(disks[0].identity_ready());
        assert_eq!(disks[0].major_minor, "259:0");
        assert_eq!(disks[0].wwn, "eui.0011223344556677");
        assert_eq!(disks[0].serial, "QOS-NVME");
        assert!(disks[0].can_install_alongside());
        assert_eq!(disks[0].alongside_root().unwrap().name, "/dev/nvme0n1p3");
        assert_eq!(
            disks[0].alongside_root().unwrap().part_uuid,
            "00000000-0000-0000-0000-000000000003"
        );
        assert_eq!(disks[0].efi_partition().unwrap().name, "/dev/nvme0n1p4");
    }

    #[test]
    fn alongside_rejects_small_or_mounted_efi_partitions() {
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

    #[test]
    fn prepared_efi_issue_explains_discovery_time_failures() {
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

    #[test]
    fn linux_disk_keeps_existing_filesystems_out_of_the_qos_candidates() {
        let fixture = format!(
            r#"{{"blockdevices":[{{"name":"/dev/sda","size":500000000000,"model":"Linux disk","type":"disk","rm":false,"ro":false,"serial":"QOS-SATA","maj:min":"8:0","children":[{{"name":"/dev/sda1","size":2147483648,"type":"part","fstype":"vfat","label":"qOS EFI","mountpoint":null,"parttype":"{EFI_SYSTEM_GUID}","partuuid":"00000000-0000-0000-0000-000000000001"}},{{"name":"/dev/sda2","size":200000000000,"type":"part","fstype":"ext4","label":"Existing Linux","mountpoint":null,"parttype":"{LINUX_FILESYSTEM_GUID}","partuuid":"00000000-0000-0000-0000-000000000002"}},{{"name":"/dev/sda3","size":68719476736,"type":"part","fstype":null,"label":null,"mountpoint":null,"parttype":"{LINUX_FILESYSTEM_GUID}","partuuid":"00000000-0000-0000-0000-000000000003"}}]}}]}}"#
        );
        let disks = parse_lsblk(&fixture);
        assert!(disks[0].can_install_alongside());
        assert_eq!(disks[0].alongside_root().unwrap().name, "/dev/sda3");
        assert_eq!(disks[0].efi_partition().unwrap().name, "/dev/sda1");
        assert_eq!(disks[0].partitions[1].filesystem, "ext4");
    }

    #[test]
    fn windows_preparation_hint_recognizes_ntfs_and_bitlocker_storage() {
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

    #[test]
    fn alongside_rejects_partition_with_existing_signature() {
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

    #[test]
    fn alongside_refuses_ambiguous_root_or_efi_candidates() {
        let fixture = format!(
            r#"{{"blockdevices":[{{"name":"/dev/nvme0n1","size":1000000000000,"model":"NVMe","type":"disk","rm":false,"ro":false,"maj:min":"259:0","children":[{{"name":"/dev/nvme0n1p1","size":2147483648,"type":"part","fstype":"vfat","label":"qOS EFI A","mountpoint":null,"parttype":"{EFI_SYSTEM_GUID}","partuuid":"00000000-0000-0000-0000-000000000001"}},{{"name":"/dev/nvme0n1p2","size":2147483648,"type":"part","fstype":"vfat","label":"qOS EFI B","mountpoint":null,"parttype":"{EFI_SYSTEM_GUID}","partuuid":"00000000-0000-0000-0000-000000000002"}},{{"name":"/dev/nvme0n1p3","size":68719476736,"type":"part","fstype":null,"label":null,"mountpoint":null,"parttype":"{LINUX_FILESYSTEM_GUID}","partuuid":"00000000-0000-0000-0000-000000000003"}},{{"name":"/dev/nvme0n1p4","size":68719476736,"type":"part","fstype":null,"label":null,"mountpoint":null,"parttype":"{LINUX_FILESYSTEM_GUID}","partuuid":"00000000-0000-0000-0000-000000000004"}}]}}]}}"#
        );
        let disks = parse_lsblk(&fixture);
        assert_eq!(disks.len(), 1);
        assert!(disks[0].alongside_root().is_none());
        assert!(disks[0].efi_partition().is_none());
        assert!(!disks[0].can_install_alongside());
    }

    #[test]
    fn destructive_targets_require_live_identity_fields() {
        let fixture = r#"{"blockdevices":[{"name":"/dev/vda","size":68719476736,"model":"VM","type":"disk","rm":false,"ro":false}]}"#;
        let disks = parse_lsblk(fixture);
        assert_eq!(disks.len(), 1);
        assert!(!disks[0].identity_ready());
        assert!(!disks[0].can_install_alongside());
    }

    #[test]
    fn alongside_partitions_require_gpt_partuuid() {
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
    }

    #[test]
    fn guided_plan_uses_one_aligned_gpt_free_extent_without_resizing() {
        let gib = 1024_u64 * 1024 * 1024;
        let fixture = format!(
            r#"{{"blockdevices":[{{"name":"/dev/nvme0n1","size":{},"start":null,"model":"NVMe","type":"disk","pttype":"gpt","ptuuid":"aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee","log-sec":512,"rm":false,"ro":false,"wwn":"eui.0011223344556677","serial":"QOS-NVME","maj:min":"259:0","children":[{{"name":"/dev/nvme0n1p1","size":{},"start":2048,"type":"part","fstype":"ntfs","label":"Windows","mountpoint":null,"parttype":"basic","partuuid":"00000000-0000-0000-0000-000000000001"}}]}}]}}"#,
            200 * gib,
            100 * gib - PARTITION_ALIGNMENT_BYTES,
        );
        let disk = parse_lsblk(&fixture).remove(0);
        let plan = disk.guided_alongside_plan().expect("one safe free extent");
        assert_eq!(plan.free_start_bytes, 100 * gib);
        assert_eq!(plan.efi_start_bytes, 100 * gib);
        assert_eq!(plan.efi_size_bytes, 2 * gib);
        assert_eq!(plan.root_start_bytes, 102 * gib);
        assert_eq!(plan.root_size_bytes, 98 * gib - PARTITION_ALIGNMENT_BYTES);
        assert_eq!(disk.occupied_extents().unwrap().len(), 1);
    }

    #[test]
    fn guided_plan_rejects_non_gpt_and_ambiguous_free_extents() {
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
}
