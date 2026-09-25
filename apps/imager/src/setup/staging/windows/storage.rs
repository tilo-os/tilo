//! Pure decoding of the PowerShell Storage-cmdlet snapshot (`Get-Disk`,
//! `Get-Partition`, `Get-PartitionSupportedSize`, the Shell BitLocker
//! property) and the disk-kind policy derived from it.

use serde_json::Value;

use super::super::platform::{Disk, Partition};
use super::layout::normalize_guid;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PartitionStyle {
    Gpt,
    Mbr,
    Raw,
    Unknown,
}

impl PartitionStyle {
    /// `Get-Disk` emits the formatted name (`GPT`) when cast to string and the
    /// CIM integer otherwise; accept both.
    fn from_json(value: Option<&Value>) -> Self {
        match value {
            Some(Value::Number(n)) => match n.as_u64() {
                Some(0) => Self::Raw,
                Some(1) => Self::Mbr,
                Some(2) => Self::Gpt,
                _ => Self::Unknown,
            },
            Some(Value::String(s)) => match s.trim().to_ascii_uppercase().as_str() {
                "GPT" => Self::Gpt,
                "MBR" => Self::Mbr,
                "RAW" => Self::Raw,
                _ => Self::Unknown,
            },
            _ => Self::Unknown,
        }
    }
}

/// `MSFT_Disk.BusType`, normalised to a lowercase name without spaces.
pub fn bus_type_name(value: Option<&Value>) -> String {
    const NAMES: [&str; 20] = [
        "unknown",
        "scsi",
        "atapi",
        "ata",
        "1394",
        "ssa",
        "fibrechannel",
        "usb",
        "raid",
        "iscsi",
        "sas",
        "sata",
        "sd",
        "mmc",
        "virtual",
        "filebackedvirtual",
        "spaces",
        "nvme",
        "scm",
        "ufs",
    ];
    match value {
        Some(Value::Number(n)) => n
            .as_u64()
            .and_then(|code| usize::try_from(code).ok())
            .and_then(|code| NAMES.get(code))
            .map_or_else(|| "unknown".to_string(), |name| (*name).to_string()),
        Some(Value::String(s)) => {
            let name: String = s
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect::<String>()
                .to_ascii_lowercase();
            if name == "storagespaces" {
                "spaces".into()
            } else {
                name
            }
        }
        _ => "unknown".into(),
    }
}

/// Buses whose disk the firmware itself boots from and that Windows exposes
/// as one plain physical disk. Everything else — RAID/RST, Storage Spaces,
/// removable media, VHD(X) native boot, network disks — fails closed.
const SUPPORTED_BUSES: [&str; 7] = ["nvme", "sata", "ata", "sas", "scsi", "virtual", "ufs"];

/// GPT types that mean the disk is not a plain basic disk.
const LDM_METADATA_TYPE: &str = "5808c8aa-7e8f-42e0-85d2-e1e90434cfb3";
const LDM_DATA_TYPE: &str = "af9b60a0-1431-4f62-bc68-3311714a69ad";
const STORAGE_SPACES_TYPE: &str = "e75caf8f-f680-4cee-afa3-b001e56efc2d";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotDisk {
    pub number: u32,
    pub guid: Option<String>,
    pub size: u64,
    pub logical_sector: u64,
    pub style: PartitionStyle,
    pub bus: String,
    pub is_boot: bool,
    pub is_system: bool,
    pub is_clustered: bool,
    pub is_read_only: bool,
    pub is_offline: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotPartition {
    pub number: u32,
    pub offset: u64,
    pub size: u64,
    pub guid: Option<String>,
    pub gpt_type: Option<String>,
    pub drive_letter: Option<char>,
    /// Holds the running Windows (`C:`).
    pub is_boot: bool,
    /// The ESP Windows booted from.
    pub is_system: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotSystem {
    pub number: u32,
    pub offset: u64,
    pub size: u64,
    pub guid: Option<String>,
    /// `Get-PartitionSupportedSize`; `None` when the cmdlet failed.
    pub size_min: Option<u64>,
    pub size_max: Option<u64>,
}

/// One consistent PowerShell view of the system disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub disk: SnapshotDisk,
    pub system: SnapshotSystem,
    pub partitions: Vec<SnapshotPartition>,
    /// Shell `System.Volume.BitLockerProtection` of the system volume.
    pub bitlocker_protection: Option<u64>,
}

fn u64_field(v: &Value, key: &str) -> Option<u64> {
    let field = v.get(key)?;
    field
        .as_u64()
        .or_else(|| field.as_str().and_then(|s| s.trim().parse().ok()))
}

fn u32_field(v: &Value, key: &str) -> Option<u32> {
    u64_field(v, key).and_then(|n| u32::try_from(n).ok())
}

fn bool_field(v: &Value, key: &str) -> Option<bool> {
    match v.get(key)? {
        Value::Bool(b) => Some(*b),
        Value::String(s) if s.eq_ignore_ascii_case("true") => Some(true),
        Value::String(s) if s.eq_ignore_ascii_case("false") => Some(false),
        _ => None,
    }
}

fn guid_field(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).and_then(normalize_guid)
}

/// `[string]$partition.DriveLetter` is `"C"`, `""`, or a NUL character when
/// no letter is assigned.
fn drive_letter(v: &Value) -> Option<char> {
    let text = v.get("DriveLetter")?.as_str()?;
    let mut chars = text.chars().filter(|c| *c != '\0' && !c.is_whitespace());
    let letter = chars.next()?;
    (chars.next().is_none() && letter.is_ascii_alphabetic()).then(|| letter.to_ascii_uppercase())
}

fn items(value: Option<&Value>) -> Vec<&Value> {
    match value {
        Some(Value::Array(items)) => items.iter().collect(),
        Some(item @ Value::Object(_)) => vec![item],
        _ => Vec::new(),
    }
}

/// Decode the snapshot script's JSON. Missing identity fields are errors;
/// only the shrink bounds and the BitLocker property may be absent.
pub fn parse_snapshot(json: &Value) -> Result<Snapshot, String> {
    let disk = json.get("Disk").ok_or("snapshot has no Disk")?;
    let system = json.get("System").ok_or("snapshot has no System")?;
    let need = |what: &str| format!("snapshot is missing {what}");
    let disk = SnapshotDisk {
        number: u32_field(disk, "Number").ok_or_else(|| need("Disk.Number"))?,
        guid: guid_field(disk, "Guid"),
        size: u64_field(disk, "Size").ok_or_else(|| need("Disk.Size"))?,
        logical_sector: u64_field(disk, "LogicalSectorSize")
            .ok_or_else(|| need("Disk.LogicalSectorSize"))?,
        style: PartitionStyle::from_json(disk.get("PartitionStyle")),
        bus: bus_type_name(disk.get("BusType")),
        is_boot: bool_field(disk, "IsBoot").ok_or_else(|| need("Disk.IsBoot"))?,
        is_system: bool_field(disk, "IsSystem").unwrap_or(false),
        is_clustered: bool_field(disk, "IsClustered").unwrap_or(false),
        is_read_only: bool_field(disk, "IsReadOnly").unwrap_or(false),
        is_offline: bool_field(disk, "IsOffline").unwrap_or(false),
    };
    let system = SnapshotSystem {
        number: u32_field(system, "PartitionNumber")
            .ok_or_else(|| need("System.PartitionNumber"))?,
        offset: u64_field(system, "Offset").ok_or_else(|| need("System.Offset"))?,
        size: u64_field(system, "Size").ok_or_else(|| need("System.Size"))?,
        guid: guid_field(system, "Guid"),
        size_min: u64_field(system, "SizeMin"),
        size_max: u64_field(system, "SizeMax"),
    };
    let mut partitions = Vec::new();
    for item in items(json.get("Partitions")) {
        partitions.push(SnapshotPartition {
            number: u32_field(item, "PartitionNumber").ok_or_else(|| need("PartitionNumber"))?,
            offset: u64_field(item, "Offset").ok_or_else(|| need("Partition.Offset"))?,
            size: u64_field(item, "Size").ok_or_else(|| need("Partition.Size"))?,
            guid: guid_field(item, "Guid"),
            gpt_type: guid_field(item, "GptType"),
            drive_letter: drive_letter(item),
            is_boot: bool_field(item, "IsBoot").unwrap_or(false),
            is_system: bool_field(item, "IsSystem").unwrap_or(false),
        });
    }
    let bitlocker_protection = u64_field(json, "BitLockerProtection");
    Ok(Snapshot {
        disk,
        system,
        partitions,
        bitlocker_protection,
    })
}

/// Why this disk cannot host staging, or `None` when it is a plain basic GPT
/// boot disk. `NotGpt` is reported separately by the caller.
pub fn unsupported_reason(snapshot: &Snapshot) -> Option<String> {
    let disk = &snapshot.disk;
    if !disk.is_boot {
        return Some("the Windows volume is not on the boot disk".into());
    }
    if !SUPPORTED_BUSES.contains(&disk.bus.as_str()) {
        return Some(format!("disks on the {} bus are not supported", disk.bus));
    }
    if disk.is_clustered || disk.is_read_only || disk.is_offline {
        return Some("the system disk is clustered, read-only or offline".into());
    }
    let special = snapshot.partitions.iter().find_map(|p| {
        p.gpt_type
            .as_deref()
            .filter(|t| [LDM_METADATA_TYPE, LDM_DATA_TYPE, STORAGE_SPACES_TYPE].contains(t))
    });
    if let Some(kind) = special {
        return Some(if kind == STORAGE_SPACES_TYPE {
            "the system disk belongs to a Storage Spaces pool".into()
        } else {
            "the system disk is a dynamic disk".into()
        });
    }
    None
}

/// Fallback [`Disk`] when the layout IOCTL is unavailable: usable bounds are
/// derived for the standard 128-entry array (identical to what Windows itself
/// writes), so the planner's 1 MiB margins still hold.
pub fn disk_from_snapshot(snapshot: &Snapshot) -> Result<Disk, String> {
    let disk = &snapshot.disk;
    if disk.style != PartitionStyle::Gpt {
        return Err("the system disk is not GPT".into());
    }
    let sector = disk.logical_sector;
    if !((512..=65536).contains(&sector) && sector.is_power_of_two()) {
        return Err(format!("unsupported logical sector size {sector}"));
    }
    let guid = disk.guid.clone().ok_or("the GPT disk has no disk GUID")?;
    let total = disk.size.checked_div(sector).ok_or("disk size overflow")?;
    let entry_sectors = (128u64 * 128).div_ceil(sector);
    let first_usable_lba = entry_sectors.checked_add(2).ok_or("lba overflow")?;
    let last_usable_lba = total
        .checked_sub(entry_sectors)
        .and_then(|lba| lba.checked_sub(2))
        .ok_or("disk too small for a GPT")?;
    let mut partitions = Vec::with_capacity(snapshot.partitions.len());
    for p in &snapshot.partitions {
        let (Some(guid), Some(type_guid)) = (p.guid.clone(), p.gpt_type.clone()) else {
            return Err(format!("partition {} has no GPT identity", p.number));
        };
        if p.offset.checked_rem(sector) != Some(0) || p.size.checked_rem(sector) != Some(0) {
            return Err(format!("partition {} is not sector aligned", p.number));
        }
        let first_lba = p.offset.checked_div(sector).ok_or("lba overflow")?;
        let last_lba = p
            .offset
            .checked_add(p.size)
            .and_then(|end| end.checked_div(sector))
            .and_then(|end| end.checked_sub(1))
            .ok_or("partition extent overflow")?;
        partitions.push(Partition {
            number: p.number,
            guid,
            type_guid,
            first_lba,
            last_lba,
        });
    }
    partitions.sort_by_key(|p| p.first_lba);
    Ok(Disk {
        id: disk.number.to_string(),
        guid,
        size_bytes: disk.size,
        sector_bytes: sector,
        first_usable_lba,
        last_usable_lba,
        partitions,
    })
}

/// Require the Storage-cmdlet view to describe exactly the disk the native
/// layout IOCTL returned. Two independent readers that disagree mean the
/// partition map is in flux; nothing is planned against it.
pub fn cross_check(disk: &Disk, snapshot: &Snapshot) -> Result<(), String> {
    let from_snapshot = disk_from_snapshot(snapshot)?;
    if from_snapshot.id != disk.id
        || from_snapshot.guid != disk.guid
        || from_snapshot.size_bytes != disk.size_bytes
        || from_snapshot.sector_bytes != disk.sector_bytes
    {
        return Err("Get-Disk and the disk layout disagree about the system disk".into());
    }
    if from_snapshot.partitions != disk.partitions {
        return Err("Get-Partition and the disk layout disagree about the partition map".into());
    }
    let system = disk
        .partitions
        .iter()
        .find(|p| Some(&p.guid) == snapshot.system.guid.as_ref())
        .ok_or("the Windows volume's partition is not on the system disk")?;
    let offset = system.first_lba.checked_mul(disk.sector_bytes);
    if system.number != snapshot.system.number || offset != Some(snapshot.system.offset) {
        return Err("the Windows volume's partition moved while probing".into());
    }
    Ok(())
}

/// The partition holding the running Windows and the ESP it booted from; the
/// adapter refuses to write, resize (other than C:) or delete either.
pub fn protected_partitions(snapshot: &Snapshot) -> Vec<String> {
    snapshot
        .partitions
        .iter()
        .filter(|p| p.is_boot || p.is_system || p.drive_letter.is_some())
        .filter_map(|p| p.guid.clone())
        .chain(snapshot.system.guid.clone())
        .collect()
}

/// Shell `System.Volume.BitLockerProtection` values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BitlockerState {
    /// Not encrypted, not encryptable, or encrypted with protection off
    /// (suspended, or device encryption still waiting for activation).
    Unprotected,
    /// Encrypted with active protectors: firmware changes need a suspend.
    Protected,
    /// Encryption or decryption in progress: the volume cannot be resized.
    Converting,
    /// Locked (never true for the running Windows volume, but fail closed).
    Locked,
    /// The property was unavailable or had an undocumented value. Treated as
    /// protected; the elevated remedy re-reads the authoritative status.
    Unknown,
}

pub fn bitlocker_state(value: Option<u64>) -> BitlockerState {
    match value {
        Some(0 | 2 | 5 | 8) => BitlockerState::Unprotected,
        Some(1) => BitlockerState::Protected,
        Some(3 | 4) => BitlockerState::Converting,
        Some(6) => BitlockerState::Locked,
        _ => BitlockerState::Unknown,
    }
}
