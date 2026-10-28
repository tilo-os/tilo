//! Pure data model for the Disks app.
//!
//! Everything here is plain data + serde, parsed from the probe's JSON.
//! Rendering and probing live in sibling modules — this file knows nothing
//! about vello, taffy or `lsblk`.

use serde::{Deserialize, Serialize};

/// Broad class of a block device, used both for the left-list badge and
/// for choosing the right icon glyph.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DriveKind {
    Nvme,
    Ssd,
    Hdd,
    Usb,
    /// Compressed RAM-backed swap device (`zram*`).
    Zram,
    /// Loopback / device-mapper / MD / other virtual block device.
    Virtual,
    Other,
}

impl DriveKind {
    /// Short name of the kind, as it is written on the drive's box.
    pub fn tag(&self) -> &'static str {
        match self {
            DriveKind::Nvme => "NVMe",
            DriveKind::Ssd => "SSD",
            DriveKind::Hdd => "HDD",
            DriveKind::Usb => "USB",
            DriveKind::Zram => "zram",
            DriveKind::Virtual => "Virtual",
            DriveKind::Other => "Disk",
        }
    }

    /// Longer human label for the detail header.
    pub fn long(&self) -> &'static str {
        match self {
            DriveKind::Nvme => "NVMe solid state drive",
            DriveKind::Ssd => "Solid state drive",
            DriveKind::Hdd => "Hard disk drive",
            DriveKind::Usb => "Removable USB drive",
            DriveKind::Zram => "Compressed RAM disk",
            DriveKind::Virtual => "Virtual block device",
            DriveKind::Other => "Block device",
        }
    }

    /// Whether this is a memory- or software-backed device (zram, loop, dm,
    /// md…) rather than physical storage media. Such devices legitimately
    /// expose no SMART data and no serial, so the UI degrades accordingly
    /// instead of implying they are disks that could be ejected or benchmarked.
    pub fn is_virtual(&self) -> bool {
        matches!(self, DriveKind::Zram | DriveKind::Virtual)
    }

    /// Derive the kind from a device's kernel name plus `lsblk`'s `rota` +
    /// `tran` fields. Virtual devices are recognised by their kernel-name
    /// prefix (they carry no transport), so a `zram` swap device never
    /// masquerades as an "SSD" just because it is non-rotational.
    pub fn from_lsblk(name: &str, rota: bool, tran: Option<&str>) -> Self {
        if name.starts_with("zram") {
            return DriveKind::Zram;
        }
        if name.starts_with("loop")
            || name.starts_with("dm-")
            || name.starts_with("md")
            || name.starts_with("ram")
            || name.starts_with("nbd")
            || name.starts_with("sr")
        {
            return DriveKind::Virtual;
        }
        match tran {
            Some("nvme") => DriveKind::Nvme,
            Some("usb") => DriveKind::Usb,
            _ => {
                if rota {
                    DriveKind::Hdd
                } else {
                    DriveKind::Ssd
                }
            }
        }
    }
}

/// Overall SMART verdict.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SmartStatus {
    Passed,
    Warning,
    Failing,
    Unknown,
}

impl SmartStatus {
    pub fn label(&self) -> &'static str {
        match self {
            SmartStatus::Passed => "Healthy",
            SmartStatus::Warning => "Needs attention",
            SmartStatus::Failing => "Failing",
            SmartStatus::Unknown => "Unknown",
        }
    }
}

/// Severity of a single SMART attribute row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AttrSeverity {
    Ok,
    Warn,
    Crit,
}

/// One row in the SMART attribute table.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SmartAttr {
    /// The ATA attribute number; `None` for a reading with no number, such as
    /// one from the `NVMe` health log.
    pub id: Option<u16>,
    pub name: String,
    pub value: String,
    pub severity: AttrSeverity,
}

/// Self-monitoring summary for a drive. All numeric fields are optional
/// because USB sticks / virtual disks frequently expose none of them.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SmartInfo {
    pub status: SmartStatusOpt,
    pub temp_c: Option<i64>,
    pub power_on_hours: Option<i64>,
    pub power_cycles: Option<i64>,
    pub reallocated: Option<i64>,
    /// `NVMe` wear indicator (0-100, "percentage used").
    pub pct_used: Option<i64>,
    /// Total host writes in GB (`NVMe` "Data Units Written" derived).
    pub written_gb: Option<i64>,
    pub attrs: Vec<SmartAttr>,
    /// Human explanation shown when there is no usable telemetry — e.g. the
    /// device genuinely reports no SMART, or reading it needs elevated
    /// privileges. Keeps the "Unknown" state honest about *why*.
    #[serde(default)]
    pub detail: Option<String>,
}

/// Newtype wrapper so `SmartInfo` can derive `Default` (`SmartStatus` has
/// no natural default otherwise).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SmartStatusOpt(pub SmartStatus);
impl Default for SmartStatusOpt {
    fn default() -> Self {
        SmartStatusOpt(SmartStatus::Unknown)
    }
}

/// Semantic role of a partition, used to pick its colour + legend label.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PartRole {
    Efi,
    Boot,
    Root,
    Home,
    Swap,
    Data,
    Free,
}

impl PartRole {
    pub fn legend(&self) -> &'static str {
        match self {
            PartRole::Efi => "EFI System",
            PartRole::Boot => "Boot",
            PartRole::Root => "Root",
            PartRole::Home => "Home",
            PartRole::Swap => "Swap",
            PartRole::Data => "Data",
            PartRole::Free => "Unallocated",
        }
    }
}

/// A single partition on a drive.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Partition {
    pub name: String,
    pub size: u64,
    pub fstype: Option<String>,
    pub label: Option<String>,
    pub mountpoint: Option<String>,
    /// Bytes used on the filesystem, if mounted + measurable.
    pub used: Option<u64>,
    pub uuid: Option<String>,
    /// How the partition is encrypted, when it is. Once unlocked, the fields
    /// above describe the filesystem inside it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encryption: Option<Encryption>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part_type: Option<String>,
    /// What its type is called (`Microsoft reserved`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part_type_name: Option<String>,
}

/// The EFI system partition's type GUID.
const ESP_TYPE: &str = "c12a7328-f81f-11d2-ba4b-00a0c93ec93b";
/// The extended boot loader partition's type GUID (`/boot`).
const XBOOTLDR_TYPE: &str = "bc13c2ff-59e6-4262-a352-b275fd6f7172";

/// An encrypted partition's container.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Encryption {
    /// `LUKS2`, `LUKS1`, `BitLocker`.
    pub scheme: String,
    /// The unlocked mapping's kernel name (`dm-1`), which is where its
    /// filesystem is mounted from; `None` while it is locked.
    pub unlocked_as: Option<String>,
}

impl Partition {
    /// Classify by mountpoint / fstype / label.
    pub fn role(&self) -> PartRole {
        let mp = self.mountpoint.as_deref().unwrap_or("");
        let fs = self.fstype.as_deref().unwrap_or("").to_ascii_lowercase();
        let label = self.label.as_deref().unwrap_or("").to_ascii_lowercase();
        if fs == "swap" || fs == "linux-swap" || label.contains("swap") {
            return PartRole::Swap;
        }
        let part_type = self.part_type.as_deref().unwrap_or("").to_ascii_lowercase();
        if part_type == ESP_TYPE
            || mp == "/boot/efi"
            || mp.contains("efi")
            || (fs == "vfat" && label.contains("efi"))
        {
            return PartRole::Efi;
        }
        if part_type == XBOOTLDR_TYPE
            || mp == "/boot"
            || label == "boot"
            || label.ends_with("_boot")
            || label.ends_with("-boot")
        {
            return PartRole::Boot;
        }
        if mp == "/" || mp == "/sysroot" {
            return PartRole::Root;
        }
        // On atomic Fedora / Tilo (bootc) the home volume lives at /var/home.
        if mp == "/home" || mp == "/var/home" {
            return PartRole::Home;
        }
        PartRole::Data
    }

    /// Fraction of the filesystem in use, if known.
    pub fn used_frac(&self) -> Option<f64> {
        let used = self.used? as f64;
        if self.size == 0 {
            return None;
        }
        Some((used / self.size as f64).clamp(0.0, 1.0))
    }

    /// What the partition is called: its label, else what its type is
    /// called ("Microsoft reserved"), else its filesystem, else its device.
    pub fn display_label(&self) -> String {
        [&self.label, &self.part_type_name, &self.fstype]
            .into_iter()
            .flatten()
            .find(|name| !name.is_empty())
            .cloned()
            .unwrap_or_else(|| self.name.clone())
    }

    /// Why this partition must not be formatted or unmounted, when it serves
    /// the running system: its root, its home folders, or (on the drive the
    /// system runs from, `system_drive`) what starts it.
    pub fn system_use(&self, system_drive: bool) -> Option<&'static str> {
        match self.role() {
            PartRole::Root => Some(HOLDS_SYSTEM),
            PartRole::Home => Some("Holds the home folders"),
            PartRole::Efi | PartRole::Boot if system_drive => Some("Starts this device"),
            _ => None,
        }
    }

    /// Where the filesystem is mounted, as a person reads it: the kernel's
    /// `[SWAP]` marker is "Swap". `None` when it is not mounted.
    pub fn mount_display(&self) -> Option<String> {
        self.mountpoint.as_deref().map(|mount| {
            if mount.eq_ignore_ascii_case("[swap]") {
                "Swap".to_owned()
            } else {
                mount.to_owned()
            }
        })
    }
}

/// A whole block device with its partitions and SMART summary.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Drive {
    pub name: String,
    pub model: String,
    pub serial: Option<String>,
    pub size: u64,
    pub kind: DriveKind,
    pub tran: Option<String>,
    pub removable: bool,
    pub partitions: Vec<Partition>,
    pub smart: Option<SmartInfo>,
}

/// Why the drive the system runs from cannot be formatted.
pub const HOLDS_SYSTEM: &str = "Holds the running system";

impl Drive {
    /// Whether the running system or the home folders live on this drive.
    pub fn is_system_drive(&self) -> bool {
        self.partitions
            .iter()
            .any(|part| matches!(part.role(), PartRole::Root | PartRole::Home))
    }

    /// Why the whole drive must not be formatted, when it holds the running
    /// system.
    pub fn system_use(&self) -> Option<&'static str> {
        self.is_system_drive().then_some(HOLDS_SYSTEM)
    }

    /// Why `part` must not be formatted or unmounted, when it serves the
    /// running system.
    pub fn partition_use(&self, part: &Partition) -> Option<&'static str> {
        part.system_use(self.is_system_drive())
    }

    /// Total bytes claimed by partitions.
    pub fn allocated(&self) -> u64 {
        self.partitions.iter().map(|p| p.size).sum()
    }

    /// Unallocated bytes at the tail of the device (never negative).
    pub fn free_tail(&self) -> u64 {
        self.size.saturating_sub(self.allocated())
    }

    /// SMART status, defaulting to Unknown when no data is present.
    pub fn smart_status(&self) -> SmartStatus {
        self.smart
            .as_ref()
            .map_or(SmartStatus::Unknown, |s| s.status.0)
    }
}

/// The whole machine's storage picture.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Topology {
    pub drives: Vec<Drive>,
}

impl Topology {
    pub fn total_size(&self) -> u64 {
        self.drives.iter().map(|d| d.size).sum()
    }

    /// A curated, fully-populated sample machine for screenshots and tests:
    /// an `NVMe` with
    /// EFI + root + home + swap, a SATA SSD, a spinning HDD that needs
    /// attention, and a USB stick.
    pub fn sample() -> Self {
        Topology {
            drives: vec![
                Self::sample_nvme(),
                Self::sample_ssd(),
                Self::sample_hdd(),
                Self::sample_usb(),
            ],
        }
    }

    /// The sample `NVMe` drive: EFI + root + home + swap, all healthy (part
    /// of [`Self::sample`]).
    fn sample_nvme() -> Drive {
        Drive {
            name: "nvme0n1".into(),
            model: "Samsung SSD 990 PRO 1TB".into(),
            serial: Some("S6Z1NJ0T934217X".into()),
            size: 1_000_204_886_016,
            kind: DriveKind::Nvme,
            tran: Some("nvme".into()),
            removable: false,
            partitions: vec![
                Partition {
                    name: "nvme0n1p1".into(),
                    size: 536_870_912,
                    fstype: Some("vfat".into()),
                    label: Some("EFI System".into()),
                    mountpoint: Some("/boot/efi".into()),
                    used: Some(96_468_992),
                    uuid: Some("9C33-6Bled".into()),
                    encryption: None,
                    part_type: None,
                    part_type_name: None,
                },
                Partition {
                    name: "nvme0n1p2".into(),
                    size: 214_748_364_800,
                    fstype: Some("ext4".into()),
                    label: Some("system".into()),
                    mountpoint: Some("/".into()),
                    used: Some(142_300_000_000),
                    uuid: Some("a1b2c3d4-...-root".into()),
                    encryption: None,
                    part_type: None,
                    part_type_name: None,
                },
                Partition {
                    name: "nvme0n1p3".into(),
                    size: 750_000_000_000,
                    fstype: Some("ext4".into()),
                    label: Some("home".into()),
                    mountpoint: Some("/home".into()),
                    used: Some(410_900_000_000),
                    uuid: Some("e5f6a7b8-...-home".into()),
                    encryption: None,
                    part_type: None,
                    part_type_name: None,
                },
                Partition {
                    name: "nvme0n1p4".into(),
                    size: 17_179_869_184,
                    fstype: Some("swap".into()),
                    label: Some("swap".into()),
                    mountpoint: Some("[SWAP]".into()),
                    used: Some(1_073_741_824),
                    uuid: Some("c0ffee00-...-swap".into()),
                    encryption: None,
                    part_type: None,
                    part_type_name: None,
                },
            ],
            smart: Some(SmartInfo {
                status: SmartStatusOpt(SmartStatus::Passed),
                temp_c: Some(41),
                power_on_hours: Some(2196),
                power_cycles: Some(412),
                reallocated: Some(0),
                pct_used: Some(3),
                written_gb: Some(48_210),
                attrs: vec![
                    health_row("Available spare", "100 %", AttrSeverity::Ok),
                    health_row("Percentage used", "3 %", AttrSeverity::Ok),
                    health_row("Media and data errors", "0", AttrSeverity::Ok),
                    health_row("Unsafe shutdowns", "14", AttrSeverity::Ok),
                ],
                detail: None,
            }),
        }
    }

    /// The sample SATA SSD: one healthy backup partition (part of
    /// [`Self::sample`]).
    fn sample_ssd() -> Drive {
        Drive {
            name: "sda".into(),
            model: "Crucial MX500 500GB".into(),
            serial: Some("2138E6A0F1B2".into()),
            size: 500_107_862_016,
            kind: DriveKind::Ssd,
            tran: Some("sata".into()),
            removable: false,
            partitions: vec![Partition {
                name: "sda1".into(),
                size: 500_105_249_280,
                fstype: Some("ntfs".into()),
                label: Some("Backup".into()),
                mountpoint: Some("/mnt/backup".into()),
                used: Some(213_400_000_000),
                uuid: Some("7E5A9C2B5A9C1F00".into()),
                encryption: None,
                part_type: None,
                part_type_name: None,
            }],
            smart: Some(SmartInfo {
                status: SmartStatusOpt(SmartStatus::Passed),
                temp_c: Some(35),
                power_on_hours: Some(11_860),
                power_cycles: Some(983),
                reallocated: Some(0),
                pct_used: None,
                written_gb: Some(31_540),
                attrs: vec![
                    attr(5, "Reallocated sectors", "0", AttrSeverity::Ok),
                    attr(9, "Power-on hours", "11860", AttrSeverity::Ok),
                    attr(173, "Wear leveling count", "94", AttrSeverity::Ok),
                    attr(194, "Temperature", "35 °C", AttrSeverity::Ok),
                    attr(199, "UDMA CRC errors", "0", AttrSeverity::Ok),
                ],
                detail: None,
            }),
        }
    }

    /// The sample spinning HDD, flagged with a real attention-needing SMART
    /// signal — reallocated + pending sectors (part of [`Self::sample`]).
    fn sample_hdd() -> Drive {
        Drive {
            name: "sdb".into(),
            model: "Seagate BarraCuda 2TB".into(),
            serial: Some("ZFL2K8WP".into()),
            size: 2_000_398_934_016,
            kind: DriveKind::Hdd,
            tran: Some("sata".into()),
            removable: false,
            partitions: vec![Partition {
                name: "sdb1".into(),
                size: 2_000_396_836_864,
                fstype: Some("ext4".into()),
                label: Some("Media".into()),
                mountpoint: Some("/mnt/media".into()),
                used: Some(1_461_000_000_000),
                uuid: Some("d34db33f-...-media".into()),
                encryption: None,
                part_type: None,
                part_type_name: None,
            }],
            smart: Some(SmartInfo {
                status: SmartStatusOpt(SmartStatus::Warning),
                temp_c: Some(39),
                power_on_hours: Some(28_714),
                power_cycles: Some(1_531),
                reallocated: Some(8),
                pct_used: None,
                written_gb: None,
                attrs: vec![
                    attr(5, "Reallocated sectors", "8", AttrSeverity::Warn),
                    attr(197, "Current pending sectors", "2", AttrSeverity::Warn),
                    attr(198, "Offline uncorrectable", "0", AttrSeverity::Ok),
                    attr(9, "Power-on hours", "28714", AttrSeverity::Ok),
                    attr(194, "Temperature", "39 °C", AttrSeverity::Ok),
                    attr(7, "Seek error rate", "0", AttrSeverity::Ok),
                ],
                detail: None,
            }),
        }
    }

    /// The sample USB stick — removable, with SMART honestly reported as
    /// unavailable through the bridge rather than fabricated (part of
    /// [`Self::sample`]).
    fn sample_usb() -> Drive {
        Drive {
            name: "sdc".into(),
            model: "SanDisk Ultra USB 3.0".into(),
            serial: Some("4C530001230531119".into()),
            size: 30_752_000_000,
            kind: DriveKind::Usb,
            tran: Some("usb".into()),
            removable: true,
            partitions: vec![Partition {
                name: "sdc1".into(),
                size: 30_745_000_000,
                fstype: Some("exfat".into()),
                label: Some("Ultra".into()),
                mountpoint: Some("/run/media/tilo/Ultra".into()),
                used: Some(8_240_000_000),
                uuid: Some("64ED-1A2B".into()),
                encryption: None,
                part_type: None,
                part_type_name: None,
            }],
            smart: Some(SmartInfo {
                status: SmartStatusOpt(SmartStatus::Unknown),
                temp_c: None,
                power_on_hours: None,
                power_cycles: None,
                reallocated: None,
                pct_used: None,
                written_gb: None,
                attrs: vec![],
                detail: Some(
                    "USB mass-storage bridges rarely pass through SMART telemetry.".into(),
                ),
            }),
        }
    }
}

fn attr(id: u16, name: &str, value: &str, severity: AttrSeverity) -> SmartAttr {
    SmartAttr {
        id: Some(id),
        name: name.into(),
        value: value.into(),
        severity,
    }
}

/// A health reading with no attribute number (the `NVMe` health log).
fn health_row(name: &str, value: &str, severity: AttrSeverity) -> SmartAttr {
    SmartAttr {
        id: None,
        name: name.into(),
        value: value.into(),
        severity,
    }
}

/// Compact duration: hours → "X yr Y d" / "Y d Z h" / "Z h".
pub fn fmt_hours(hours: i64) -> String {
    if hours <= 0 {
        return "0 h".to_string();
    }
    let days = hours / 24;
    let years = days / 365;
    let rem_days = days % 365;
    let rem_hours = hours % 24;
    if years > 0 {
        format!("{years} yr {rem_days} d")
    } else if days > 0 {
        format!("{days} d {rem_hours} h")
    } else {
        format!("{hours} h")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sample topology: drive mix, partition classes, free tail, clamped usage and JSON round-trip.
    #[test]
    fn sample_topology_geometry_and_serialisation() {
        // --- phase: sample_has_four_drives_with_mixed_kinds ---
        {
            let t = Topology::sample();
            assert_eq!(t.drives.len(), 4);
            let kinds: Vec<_> = t.drives.iter().map(|d| d.kind).collect();
            assert!(kinds.contains(&DriveKind::Nvme));
            assert!(kinds.contains(&DriveKind::Ssd));
            assert!(kinds.contains(&DriveKind::Hdd));
            assert!(kinds.contains(&DriveKind::Usb));
        }

        // --- phase: nvme_partitions_classify_correctly ---
        {
            let t = Topology::sample();
            let nvme = &t.drives[0];
            let roles: Vec<_> = nvme.partitions.iter().map(super::Partition::role).collect();
            assert_eq!(roles[0], PartRole::Efi);
            assert_eq!(roles[1], PartRole::Root);
            assert_eq!(roles[2], PartRole::Home);
            assert_eq!(roles[3], PartRole::Swap);
        }

        // --- phase: free_tail_is_non_negative_and_correct ---
        {
            let t = Topology::sample();
            let nvme = &t.drives[0];
            assert!(nvme.free_tail() > 0, "sample NVMe leaves unallocated tail");
            assert_eq!(nvme.allocated() + nvme.free_tail(), nvme.size);
        }

        // --- phase: used_frac_clamped ---
        {
            let p = Partition {
                name: "x".into(),
                size: 100,
                fstype: None,
                label: None,
                mountpoint: None,
                used: Some(150),
                uuid: None,
                encryption: None,
                part_type: None,
                part_type_name: None,
            };
            assert_eq!(p.used_frac(), Some(1.0));
        }

        // --- phase: round_trips_through_json ---
        {
            let t = Topology::sample();
            let json = serde_json::to_string(&t).unwrap();
            let back: Topology = serde_json::from_str(&json).unwrap();
            assert_eq!(back.drives.len(), t.drives.len());
            assert_eq!(back.drives[0].name, "nvme0n1");
        }
    }

    /// Mountpoint roles and lsblk device kinds classify atomic Fedora, media and virtual devices.
    #[test]
    fn lsblk_classification_of_roles_and_kinds() {
        // --- phase: role_recognises_atomic_fedora_mountpoints ---
        {
            let part = |mp: &str, fs: &str| Partition {
                name: "p".into(),
                size: 100,
                fstype: Some(fs.into()),
                label: None,
                mountpoint: Some(mp.into()),
                used: None,
                uuid: None,
                encryption: None,
                part_type: None,
                part_type_name: None,
            };
            // bootc systems: home at /var/home, root sometimes surfaced as /sysroot.
            assert_eq!(part("/var/home", "btrfs").role(), PartRole::Home);
            assert_eq!(part("/home", "ext4").role(), PartRole::Home);
            assert_eq!(part("/sysroot", "btrfs").role(), PartRole::Root);
            assert_eq!(part("/boot/efi", "vfat").role(), PartRole::Efi);
            assert_eq!(part("[SWAP]", "swap").role(), PartRole::Swap);
            assert_eq!(part("/mnt/data", "xfs").role(), PartRole::Data);
        }

        // --- phase: kind_from_lsblk_covers_media_and_virtual_devices ---
        {
            assert_eq!(
                DriveKind::from_lsblk("nvme0n1", false, Some("nvme")),
                DriveKind::Nvme
            );
            assert_eq!(
                DriveKind::from_lsblk("sda", false, Some("sata")),
                DriveKind::Ssd
            );
            assert_eq!(
                DriveKind::from_lsblk("sdb", true, Some("sata")),
                DriveKind::Hdd
            );
            assert_eq!(
                DriveKind::from_lsblk("sdc", false, Some("usb")),
                DriveKind::Usb
            );
            // A zram swap device is non-rotational but must NOT read as an SSD.
            assert_eq!(DriveKind::from_lsblk("zram0", false, None), DriveKind::Zram);
            assert!(DriveKind::from_lsblk("zram0", false, None).is_virtual());
            assert_eq!(
                DriveKind::from_lsblk("loop0", false, None),
                DriveKind::Virtual
            );
            assert_eq!(
                DriveKind::from_lsblk("dm-0", false, None),
                DriveKind::Virtual
            );
            assert!(!DriveKind::from_lsblk("nvme0n1", false, Some("nvme")).is_virtual());
        }
    }
}
