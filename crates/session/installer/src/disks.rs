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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskPartition {
    pub name: String,
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

    pub fn can_install_alongside(&self) -> bool {
        self.identity_ready()
            && !self.read_only
            && self.alongside_root().is_some()
            && self.efi_partition().is_some()
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

/// The `lsblk` argument vector. Kept as a constant so the call site and
/// any docs/tests reference exactly one source of truth.
pub const LSBLK_ARGS: [&str; 5] = [
    "-J",
    "-b",
    "-p",
    "--output",
    "NAME,SIZE,MODEL,TYPE,FSTYPE,LABEL,MOUNTPOINT,PARTTYPE,PARTUUID,RM,RO,WWN,SERIAL,MAJ:MIN",
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
            let is_system = partitions.iter().any(|p: &DiskPartition| {
                matches!(p.mountpoint.as_str(), "/" | "/boot" | "/boot/efi")
            });
            Some(Disk {
                name,
                size: format_bytes(size_bytes),
                model: field(d, "model"),
                size_bytes,
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
        size_bytes: number(value, "size"),
        part_uuid: field(value, "partuuid"),
        filesystem: field(value, "fstype"),
        label: field(value, "label"),
        mountpoint: field(value, "mountpoint"),
        part_type: field(value, "parttype"),
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
    fn alongside_rejects_partition_with_existing_signature() {
        let p = DiskPartition {
            name: "/dev/sda3".into(),
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
}
