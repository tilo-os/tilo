//! Live target enumeration: turn `lsblk -J -b` into a list of
//! [`TargetDrive`]s.
//!
//! Best-effort and side-effect free — if the platform probe is missing or
//! returns nonsense we degrade to `None`, letting the caller retain its last
//! trustworthy snapshot. A successful probe with no usable drives is
//! deliberately `Some(Vec::new())`: that distinction lets Rescan remove an
//! unplugged drive instead of leaving a stale destructive target selected.
//! This module is only exercised in the live binary; `--shots` never calls it,
//! so the screenshot stays deterministic offline.

use std::path::Path;
#[cfg(any(target_os = "macos", target_os = "windows"))]
use std::process::Command;

use serde_json::Value;

use block_devices::BlockDevice;

use crate::model::{Source, SourceIdentity, SourceKind, TargetDrive, TargetKind, TargetPartition};

const PROBE_OUTPUT_LIMIT: usize = 1024 * 1024;
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Enumerate candidate write targets. Returns `None` when the platform probe
/// is unavailable or malformed, and `Some(Vec::new())` when it succeeded but
/// found no usable disks.
pub fn probe() -> Option<Vec<TargetDrive>> {
    probe_platform()
}

/// Exactly the columns [`parse_lsblk`] reads.
#[cfg(target_os = "linux")]
const LSBLK_COLUMNS: &str = "NAME,MODEL,SERIAL,WWN,SIZE,TYPE,ROTA,TRAN,RM,FSTYPE,LABEL,MOUNTPOINT";

#[cfg(target_os = "linux")]
fn probe_platform() -> Option<Vec<TargetDrive>> {
    let devices = block_devices::Lsblk::new(LSBLK_COLUMNS)
        .output_limit(PROBE_OUTPUT_LIMIT)
        .timeout(PROBE_TIMEOUT)
        .list()?;
    let drives = targets_from(&devices);
    #[cfg(feature = "flatpak-media-writer")]
    if std::env::var("TILO_IMAGER_FLATPAK").as_deref() == Ok("1") {
        return harden_flatpak_inventory(drives);
    }
    Some(drives)
}

/// Reconcile the sandbox-visible sysfs inventory with the host's `UDisks2`
/// view. Mountpoints in a Flatpak's own namespace cannot prove that a USB disk
/// is not the host's root disk, so failure to obtain the host-side safety
/// properties fails closed and hides that candidate.
#[cfg(all(target_os = "linux", feature = "flatpak-media-writer"))]
fn harden_flatpak_inventory(drives: Vec<TargetDrive>) -> Option<Vec<TargetDrive>> {
    let udisks = block_devices::udisks::Udisks::system().ok()?;
    let mut safe = Vec::new();
    for mut drive in drives {
        let Ok(proxy) = udisks.block(&drive.name) else {
            continue;
        };
        let host_size = proxy.get_property::<u64>("Size").ok();
        let read_only = proxy.get_property::<bool>("ReadOnly").ok();
        let hint_system = proxy.get_property::<bool>("HintSystem").ok();
        let root_usage = proxy.get_property::<String>("IdUsage").ok();
        let mut host_system_mount = false;
        let mut complete_partition_evidence = true;
        if root_usage.as_deref() == Some("filesystem") {
            if let Ok(mounts) = udisks.mount_points(&drive.name) {
                host_system_mount |= mounts.iter().any(|bytes| host_mount_is_system(bytes));
            } else {
                complete_partition_evidence = false;
            }
        } else if root_usage.is_none() {
            complete_partition_evidence = false;
        }
        for partition in &drive.partitions {
            let Ok(block) = udisks.block(&partition.name) else {
                complete_partition_evidence = false;
                break;
            };
            let Ok(partition_system) = block.get_property::<bool>("HintSystem") else {
                complete_partition_evidence = false;
                break;
            };
            host_system_mount |= partition_system;
            let Ok(usage) = block.get_property::<String>("IdUsage") else {
                complete_partition_evidence = false;
                break;
            };
            if usage == "filesystem" {
                let Ok(mounts) = udisks.mount_points(&partition.name) else {
                    complete_partition_evidence = false;
                    break;
                };
                host_system_mount |= mounts.iter().any(|bytes| host_mount_is_system(bytes));
            }
        }
        let Some(host_system) = reconcile_flatpak_host_safety(
            drive.size,
            host_size,
            read_only,
            hint_system,
            complete_partition_evidence.then_some(host_system_mount),
        ) else {
            continue;
        };
        drive.is_system |= host_system;
        safe.push(drive);
    }
    Some(safe)
}

#[cfg(all(target_os = "linux", feature = "flatpak-media-writer"))]
fn host_mount_is_system(bytes: &[u8]) -> bool {
    let mount = bytes.strip_suffix(&[0]).unwrap_or(bytes);
    matches!(mount, b"/" | b"/boot" | b"/boot/efi") || mount.starts_with(b"/var")
}

/// Return the host's system-disk classification only when every safety fact
/// was obtained. Any missing property, partition-query error, read-only
/// target, or cross-namespace size disagreement rejects the candidate.
#[cfg(all(target_os = "linux", feature = "flatpak-media-writer"))]
fn reconcile_flatpak_host_safety(
    expected_size: u64,
    host_size: Option<u64>,
    read_only: Option<bool>,
    hint_system: Option<bool>,
    system_mount: Option<bool>,
) -> Option<bool> {
    let host_size = host_size?;
    let read_only = read_only?;
    let hint_system = hint_system?;
    let system_mount = system_mount?;
    (!read_only && host_size == expected_size).then_some(hint_system || system_mount)
}

/// macOS: enumerate external physical media only. Internal disks are omitted
/// because raw imaging the running APFS container is never a valid media-
/// creation operation; the live Tilo installer handles internal installation.
#[cfg(target_os = "macos")]
fn probe_platform() -> Option<Vec<TargetDrive>> {
    let plist = bounded_command::run(
        "diskutil",
        &["list", "-plist", "external", "physical"],
        None,
        PROBE_OUTPUT_LIMIT,
        PROBE_TIMEOUT,
    )
    .ok()?;
    if !plist.status.success() {
        return None;
    }
    let json = plist_to_json(&plist.stdout)?;
    parse_diskutil(&json)
}

#[cfg(target_os = "macos")]
fn plist_to_json(plist: &[u8]) -> Option<Value> {
    let mut command = Command::new("plutil");
    command.args(["-convert", "json", "-o", "-", "--", "-"]);
    let out =
        bounded_command::run_prepared(command, Some(plist), PROBE_OUTPUT_LIMIT, PROBE_TIMEOUT)
            .ok()?;
    out.status
        .success()
        .then(|| serde_json::from_slice(&out.stdout).ok())
        .flatten()
}

/// Parse the JSON form of `diskutil list -plist external physical`.
pub fn parse_diskutil(json: &Value) -> Option<Vec<TargetDrive>> {
    let disks = json.get("AllDisksAndPartitions")?.as_array()?;
    let drives = disks
        .iter()
        .filter_map(|disk| {
            let name = str_field(disk, "DeviceIdentifier")?;
            let size = disk.get("Size").and_then(as_u64).unwrap_or(0);
            if size == 0 {
                return None;
            }
            let partitions = disk
                .get("Partitions")
                .and_then(Value::as_array)
                .map(|parts| {
                    parts
                        .iter()
                        .filter_map(|p| {
                            Some(TargetPartition {
                                name: str_field(p, "DeviceIdentifier")?,
                                size: p.get("Size").and_then(as_u64).unwrap_or(0),
                                fstype: str_field(p, "Content"),
                                label: str_field(p, "VolumeName"),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            Some(TargetDrive {
                name,
                model: str_field(disk, "MediaName").unwrap_or_else(|| "External drive".into()),
                stable_id: str_field(disk, "DeviceTreePath")
                    .or_else(|| str_field(disk, "MediaUUID")),
                size,
                kind: TargetKind::Usb,
                tran: Some("usb".into()),
                removable: true,
                is_system: false,
                partitions,
            })
        })
        .collect::<Vec<_>>();
    Some(drives)
}

#[cfg(target_os = "windows")]
fn probe_platform() -> Option<Vec<TargetDrive>> {
    // Keep disk identity and safety fields in the same snapshot as the volume
    // details shown on the destructive review screen. A partition without a
    // mountable volume still appears by number and size instead of making a
    // populated drive look empty.
    let script = r#"$disks = @(Get-Disk | ForEach-Object {
  $disk = $_
  $partitions = @(Get-Partition -DiskNumber $disk.Number -ErrorAction SilentlyContinue | ForEach-Object {
    $partition = $_
    $volume = $partition | Get-Volume -ErrorAction SilentlyContinue | Select-Object -First 1
    $name = if ($partition.DriveLetter) { "$($partition.DriveLetter):" } else { "Partition $($partition.PartitionNumber)" }
    [PSCustomObject]@{Name=$name;Size=$partition.Size;FileSystem=$volume.FileSystem;Label=$volume.FileSystemLabel}
  })
  [PSCustomObject]@{Number=$disk.Number;FriendlyName=$disk.FriendlyName;UniqueId=$disk.UniqueId;Size=$disk.Size;BusType=$disk.BusType;IsBoot=$disk.IsBoot;IsSystem=$disk.IsSystem;IsReadOnly=$disk.IsReadOnly;Partitions=$partitions}
})
$disks | ConvertTo-Json -Compress -Depth 4"#;
    let out = bounded_command::run(
        "powershell.exe",
        &["-NoProfile", "-NonInteractive", "-Command", script],
        None,
        PROBE_OUTPUT_LIMIT,
        PROBE_TIMEOUT,
    )
    .ok()?;
    if !out.status.success() {
        return None;
    }
    let json: Value = serde_json::from_slice(&out.stdout).ok()?;
    parse_windows_disks(&json)
}

/// Parse PowerShell `Get-Disk` JSON. Kept platform-neutral for fixture tests.
pub fn parse_windows_disks(json: &Value) -> Option<Vec<TargetDrive>> {
    let one;
    let (disks, inventory_is_array) = if let Some(items) = json.as_array() {
        (items, true)
    } else {
        one = vec![json.clone()];
        (&one, false)
    };
    let drives = disks
        .iter()
        .filter_map(|disk| {
            let number = disk.get("Number")?.as_u64()?;
            let size = disk.get("Size").and_then(as_u64).unwrap_or(0);
            let read_only = disk
                .get("IsReadOnly")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if size == 0 || read_only {
                return None;
            }
            let bus = str_field(disk, "BusType").unwrap_or_default();
            let is_system = disk.get("IsBoot").and_then(Value::as_bool).unwrap_or(false)
                || disk
                    .get("IsSystem")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
            let removable = matches!(bus.to_ascii_lowercase().as_str(), "usb" | "sd" | "mmc");
            let partitions = windows_partitions(disk);
            Some(TargetDrive {
                name: format!("PhysicalDrive{number}"),
                model: str_field(disk, "FriendlyName").unwrap_or_else(|| format!("Disk {number}")),
                stable_id: str_field(disk, "UniqueId"),
                size,
                kind: TargetKind::from_lsblk(false, Some(&bus.to_ascii_lowercase()), removable),
                tran: Some(bus.to_ascii_lowercase()),
                removable,
                is_system,
                partitions,
            })
        })
        .collect::<Vec<_>>();
    // `Get-Disk | ConvertTo-Json` emits `[]` when the inventory is
    // authoritatively empty. A singleton object remains accepted for older
    // PowerShell producers, but a malformed singleton must still be a probe
    // failure rather than an empty snapshot.
    (inventory_is_array || !drives.is_empty()).then_some(drives)
}

fn windows_partitions(disk: &Value) -> Vec<TargetPartition> {
    let Some(value) = disk.get("Partitions") else {
        return Vec::new();
    };
    let mut partitions = Vec::new();
    let mut push = |partition: &Value| {
        let Some(name) = str_field(partition, "Name") else {
            return;
        };
        partitions.push(TargetPartition {
            name,
            size: partition.get("Size").and_then(as_u64).unwrap_or(0),
            fstype: str_field(partition, "FileSystem"),
            label: str_field(partition, "Label"),
        });
    };
    if let Some(items) = value.as_array() {
        items.iter().for_each(&mut push);
    } else if value.is_object() {
        // Accept singleton objects too, so fixtures and older PowerShell JSON
        // producers do not need to preserve a one-element array wrapper.
        push(value);
    }
    partitions
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn probe_platform() -> Option<Vec<TargetDrive>> {
    None
}

/// Parse the `lsblk -J` JSON document into target drives. Split out from
/// [`probe`] so it can be unit-tested against a fixture without a real
/// `lsblk`.
pub fn parse_lsblk(json: &Value) -> Option<Vec<TargetDrive>> {
    Some(targets_from(&block_devices::lsblk::parse(json)?))
}

fn targets_from(devices: &[BlockDevice]) -> Vec<TargetDrive> {
    let mut drives = Vec::new();
    for dev in devices {
        if dev.kind != "disk" {
            continue;
        }
        if dev.size == 0 {
            continue; // skip empty loop / nbd shells
        }
        if dev.name.is_empty() {
            continue;
        }
        let kind = TargetKind::from_lsblk(dev.rotational, dev.tran.as_deref(), dev.removable);

        let mut partitions = Vec::new();
        let mut is_system = false;
        collect_parts(&dev.children, &mut partitions, &mut is_system);

        drives.push(TargetDrive {
            name: dev.name.clone(),
            model: dev.model.clone().unwrap_or_else(|| dev.name.clone()),
            stable_id: dev.wwn.clone().or_else(|| dev.serial.clone()),
            size: dev.size,
            kind,
            tran: dev.tran.clone(),
            removable: dev.removable,
            is_system,
            partitions,
        });
    }
    drives
}

/// Flatten partitions; flag the drive as the system disk if any child is
/// mounted at `/`, `/boot` or `/boot/efi` (writing there destroys the
/// running OS).
fn collect_parts(children: &[BlockDevice], out: &mut Vec<TargetPartition>, is_system: &mut bool) {
    for c in children {
        if let Some(mp) = c.mountpoint.as_deref()
            && (mp == "/" || mp == "/boot" || mp == "/boot/efi" || mp.starts_with("/var"))
        {
            *is_system = true;
        }
        if matches!(c.kind.as_str(), "part" | "crypt" | "lvm") {
            out.push(TargetPartition {
                name: c.name.clone(),
                size: c.size,
                fstype: c.fstype.clone(),
                label: c.label.clone(),
            });
        }
        collect_parts(&c.children, out, is_system);
    }
}

// ---------------------------------------------------------------------------
// Source inspection — turn a picked file path into a populated `Source`.
// ---------------------------------------------------------------------------

/// Inspect a local image file the user picked (via the portal) and build a
/// [`Source`] from one opened file: its on-disk size, identity and, for an
/// ISO, the ISO-9660 volume label read straight from the file. Returns `None`
/// if the path does not exist or cannot be read.
pub fn inspect_source(path: &Path) -> Option<Source> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // A selected path may be a FIFO (or replaced with one). Reach the
        // held-file type check without waiting for a writer to open it.
        options.custom_flags(libc::O_NONBLOCK);
    }
    let mut file = options.open(path).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() {
        return None;
    }
    let size = meta.len();
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("image")
        .to_string();
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    // A real volume label beats the file name when we can read one.
    let label = read_iso_label_file(&mut file)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            path.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("image")
                .to_string()
        });

    // Local files have no authenticated release-catalog provenance. Even a
    // Tilo-looking filename or volume label is therefore write-only.
    let looks_like_image = matches!(ext.as_str(), "iso" | "img" | "raw" | "bin" | "dd");
    if !looks_like_image {
        return None;
    }

    Some(Source {
        kind: SourceKind::IsoFile,
        path: path.to_string_lossy().to_string(),
        name,
        size,
        label,
        installable: false,
        identity: SourceIdentity::from_file(&file).ok()?,
        expected_sha256: None,
    })
}

/// Read the ISO-9660 Primary Volume Descriptor's volume identifier. The PVD
/// lives at byte 0x8000 (logical sector 16); its 5-byte standard identifier is
/// `CD001` at offset 1, and the 32-byte, space-padded volume id sits at offset
/// 40 (file offset 0x8028). Best-effort: any short/mismatched file yields
/// `None` and the caller falls back to the file name.
pub fn read_iso_label(path: &Path) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    read_iso_label_file(&mut file)
}

fn read_iso_label_file(f: &mut std::fs::File) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    f.seek(SeekFrom::Start(0x8000)).ok()?;
    let mut hdr = [0u8; 72];
    f.read_exact(&mut hdr).ok()?;
    // Bytes 1..6 must be the "CD001" magic for a valid PVD.
    if &hdr[1..6] != b"CD001" {
        return None;
    }
    let raw = &hdr[40..72];
    let label = String::from_utf8_lossy(raw)
        .trim()
        .trim_matches('\0')
        .trim()
        .to_string();
    if label.is_empty() { None } else { Some(label) }
}

fn str_field(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .map(std::string::ToString::to_string)
}

/// Platform inventories emit sizes as JSON numbers or numeric strings.
fn as_u64(v: &Value) -> Option<u64> {
    if let Some(n) = v.as_u64() {
        return Some(n);
    }
    v.as_str().and_then(|s| s.parse::<u64>().ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// Flatpak host reconciliation.
    #[test]
    #[cfg(all(target_os = "linux", feature = "flatpak-media-writer"))]
    fn flatpak_host_reconciliation_and_mount_classification_fail_closed() {
        // Phase `flatpak_host_reconciliation_rejects_every_unknown_or_mismatch`.
        {
            let complete = (Some(32_000), Some(false), Some(false), Some(false));
            assert_eq!(
                reconcile_flatpak_host_safety(
                    32_000, complete.0, complete.1, complete.2, complete.3
                ),
                Some(false)
            );
            assert_eq!(
                reconcile_flatpak_host_safety(
                    32_000,
                    Some(32_000),
                    Some(false),
                    Some(true),
                    Some(false)
                ),
                Some(true)
            );
            assert_eq!(
                reconcile_flatpak_host_safety(
                    32_000,
                    Some(32_000),
                    Some(false),
                    Some(false),
                    Some(true)
                ),
                Some(true)
            );
            assert_eq!(
                reconcile_flatpak_host_safety(
                    32_000,
                    Some(31_999),
                    Some(false),
                    Some(false),
                    Some(false)
                ),
                None
            );
            assert_eq!(
                reconcile_flatpak_host_safety(
                    32_000,
                    Some(32_000),
                    Some(true),
                    Some(false),
                    Some(false)
                ),
                None
            );
            for missing in 0..4 {
                let mut facts = complete;
                match missing {
                    0 => facts.0 = None,
                    1 => facts.1 = None,
                    2 => facts.2 = None,
                    _ => facts.3 = None,
                }
                assert_eq!(
                    reconcile_flatpak_host_safety(32_000, facts.0, facts.1, facts.2, facts.3),
                    None,
                    "missing host safety fact {missing} must reject the drive"
                );
            }
        }

        // Phase `host_mount_classification_uses_host_paths_and_tolerates_nul`.
        {
            assert!(host_mount_is_system(b"/\0"));
            assert!(host_mount_is_system(b"/var/home\0"));
            assert!(!host_mount_is_system(b"/run/media/alice/USB\0"));
        }
    }

    /// Linux lsblk parsing.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn linux_lsblk_inventory_parses_and_may_authoritatively_be_empty() {
        // Phase `parses_usb_and_nvme_from_fixture`.
        {
            // A trimmed `lsblk -J -b` document: one USB stick + one NVMe.
            let json: Value = serde_json::from_str(
                r#"{
                  "blockdevices": [
                    {"name":"sdb","model":"SanDisk Ultra","size":30752000000,"type":"disk",
                     "rota":false,"tran":"usb","rm":true,
                     "children":[{"name":"sdb1","size":30745000000,"type":"part","fstype":"exfat","label":"ULTRA","mountpoint":null}]},
                    {"name":"nvme0n1","model":"Samsung 990","size":1000204886016,"type":"disk",
                     "rota":false,"tran":"nvme","rm":false,
                     "children":[
                       {"name":"nvme0n1p1","size":536870912,"type":"part","fstype":"vfat","label":"EFI","mountpoint":"/boot/efi"},
                       {"name":"nvme0n1p2","size":999000000000,"type":"part","fstype":"btrfs","label":"root","mountpoint":"/"}
                     ]}
                  ]
                }"#,
            )
            .unwrap();
            let drives = parse_lsblk(&json).unwrap();
            assert_eq!(drives.len(), 2);
            assert_eq!(drives[0].kind, TargetKind::Usb);
            assert!(drives[0].removable);
            assert!(!drives[0].is_system);
            assert_eq!(drives[1].kind, TargetKind::Nvme);
            // The NVMe is mounted at / and /boot/efi → flagged as the system disk.
            assert!(drives[1].is_system);
        }

        // Phase `parse_lsblk_matches_the_captured_inventory`.
        {
            // Real `lsblk` output for exactly the Linux probe's columns, and the
            // targets this module derived from it before the shared parser existed.
            let input: Value = serde_json::from_str(include_str!(
                "../../../crates/system/block-devices/tests/fixtures/imager.json"
            ))
            .unwrap();
            let expected: Value =
                serde_json::from_str(include_str!("../tests/fixtures/lsblk-targets.json")).unwrap();
            assert_eq!(serde_json::to_value(parse_lsblk(&input)).unwrap(), expected);
        }

        // Phase `successful_linux_probe_can_authoritatively_return_no_drives`.
        {
            let json: Value = serde_json::from_str(
                r#"{"blockdevices":[
                     {"name":"loop0","size":0,"type":"loop"},
                     {"name":"sr0","size":0,"type":"rom"}
                ]}"#,
            )
            .unwrap();
            assert_eq!(parse_lsblk(&json).unwrap().len(), 0);
            assert!(parse_lsblk(&serde_json::json!({})).is_none());
        }

        // Phase `accepts_string_flags_from_older_lsblk`.
        {
            let json: Value = serde_json::from_str(
                r#"{"blockdevices":[
                     {"name":"sdc","model":"Old Stick","size":"8000000000","type":"disk",
                      "rota":"0","tran":"usb","rm":"1"}
                ]}"#,
            )
            .unwrap();
            let drives = parse_lsblk(&json).unwrap();
            assert_eq!(drives.len(), 1);
            assert_eq!(drives[0].size, 8_000_000_000);
            assert!(drives[0].removable);
        }
    }

    /// Build a file whose ISO-9660 PVD carries `label`, so the real reader
    /// parses a real (if minimal) volume descriptor — no external ISO needed.
    fn iso_with_label(label: &str) -> tempfile::NamedTempFile {
        use std::io::{Seek, SeekFrom, Write};
        let mut f = tempfile::Builder::new().suffix(".iso").tempfile().unwrap();
        // Pad up to the PVD sector, then write the real ISO-9660 header layout:
        // type(1) + "CD001"(5) + version(1) + unused(1) + system id(32) +
        // volume id(32). The volume id must land at PVD offset 40 (file
        // offset 0x8028) for `read_iso_label` to find it.
        f.seek(SeekFrom::Start(0x8000)).unwrap();
        let mut pvd = vec![0x01u8]; // offset 0: descriptor type
        pvd.extend_from_slice(b"CD001"); // offset 1..6: standard identifier
        pvd.push(0x01); // offset 6: version
        pvd.push(0x00); // offset 7: unused
        pvd.extend_from_slice(&[b' '; 32]); // offset 8..40: system identifier
        let mut vol = [b' '; 32]; // offset 40..72: volume identifier
        let b = label.as_bytes();
        vol[..b.len()].copy_from_slice(b);
        pvd.extend_from_slice(&vol);
        f.write_all(&pvd).unwrap();
        f.flush().unwrap();
        f
    }

    /// Source inspection of ISO labels, foreign and non-image files.
    #[test]
    fn inspect_source_classifies_local_images_without_trusting_names() {
        // Phase `reads_real_iso9660_volume_label`.
        {
            let iso = iso_with_label("TILO_1_0_X86_64");
            assert_eq!(
                read_iso_label(iso.path()).as_deref(),
                Some("TILO_1_0_X86_64")
            );
        }

        // Phase `tilo_looking_local_source_remains_unverified_and_write_only`.
        {
            let iso = iso_with_label("TILO_1_0");
            let src = inspect_source(iso.path()).expect("inspect");
            assert_eq!(src.kind, SourceKind::IsoFile);
            assert!(!src.installable);
            assert!(src.expected_sha256.is_none());
            assert_eq!(src.label, "TILO_1_0");
            assert!(src.size > 0);
            assert!(
                std::path::Path::new(&src.name)
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("iso"))
            );
        }

        // Phase `inspect_source_treats_foreign_iso_as_write_only`.
        {
            // No CD001 magic, but a `.iso` extension → still a valid image source,
            // just not installable, and it falls back to the file stem for a label.
            let f = tempfile::Builder::new().suffix(".iso").tempfile().unwrap();
            let src = inspect_source(f.path()).expect("inspect");
            assert_eq!(src.kind, SourceKind::IsoFile);
            assert!(!src.installable);
        }

        // Phase `inspect_source_rejects_non_image_files`.
        {
            let f = tempfile::Builder::new().suffix(".txt").tempfile().unwrap();
            assert!(inspect_source(f.path()).is_none());
            assert!(inspect_source(std::path::Path::new("/no/such/file.iso")).is_none());
        }

        // Phase `tilo_filename_does_not_turn_non_image_into_official_media`.
        {
            let f = tempfile::Builder::new()
                .prefix("tilo-official-")
                .suffix(".txt")
                .tempfile()
                .unwrap();
            assert!(inspect_source(f.path()).is_none());
        }
    }

    #[cfg(unix)]
    #[test]
    fn inspect_source_rejects_fifo_without_waiting_for_a_writer() {
        use std::os::unix::ffi::OsStrExt;
        use ui::time::Duration;

        const CHILD_FIFO: &str = "TILO_IMAGER_TEST_INSPECTION_FIFO";
        if let Some(path) = std::env::var_os(CHILD_FIFO) {
            assert!(inspect_source(Path::new(&path)).is_none());
            return;
        }

        let root = tempfile::tempdir().unwrap();
        let fifo = root.path().join("selected.img");
        let native = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: the path is NUL-terminated and remains live through the call.
        assert_eq!(unsafe { libc::mkfifo(native.as_ptr(), 0o600) }, 0);

        // Isolate a regressed blocking open in a test subprocess, so timeout
        // cleanup can always kill and reap it before removing the private FIFO.
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "probe::tests::inspect_source_rejects_fifo_without_waiting_for_a_writer",
            ])
            .env(CHILD_FIFO, &fifo)
            .stdout(std::fs::File::create(root.path().join("child.stdout")).unwrap())
            .spawn()
            .unwrap();
        let deadline = tilo_test_env::hang_deadline();
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if ui::time::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                _ => break None,
            }
        };
        if status.is_none() {
            let _ = child.kill();
            child.wait().unwrap();
        }
        assert!(
            status.is_some(),
            "source inspection blocked on a FIFO with no writer"
        );
        assert!(
            status.unwrap().success(),
            "FIFO inspection must reject the source"
        );
        let stdout = std::fs::read_to_string(root.path().join("child.stdout")).unwrap();
        // libtest exits 0 when `--exact` selects nothing; require the child ran.
        assert!(
            stdout.contains("test result: ok. 1 passed"),
            "the child ran no test: {stdout}"
        );
    }

    /// diskutil and Get-Disk parsing.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn macos_and_windows_inventories_parse_and_may_authoritatively_be_empty() {
        // Phase `parses_macos_external_diskutil_inventory`.
        {
            let json: Value = serde_json::from_str(
                r#"{
              "AllDisksAndPartitions":[{"DeviceIdentifier":"disk4","Size":64000000000,
                "MediaName":"SanDisk USB","Partitions":[{"DeviceIdentifier":"disk4s1",
                "Size":63900000000,"Content":"ExFAT","VolumeName":"TRANSFER"}]}]}
            "#,
            )
            .unwrap();
            let drives = parse_diskutil(&json).unwrap();
            assert_eq!(drives[0].name, "disk4");
            assert!(drives[0].removable);
            assert_eq!(drives[0].partitions[0].label.as_deref(), Some("TRANSFER"));
        }

        // Phase `successful_macos_probe_can_authoritatively_return_no_external_drives`.
        {
            let empty = serde_json::json!({"AllDisksAndPartitions": []});
            assert_eq!(parse_diskutil(&empty).unwrap().len(), 0);
            assert!(parse_diskutil(&serde_json::json!({})).is_none());
        }

        // Phase `parses_windows_get_disk_and_flags_system_disk`.
        {
            let json: Value = serde_json::from_str(r#"[
              {"Number":0,"FriendlyName":"Internal NVMe","Size":1000000000000,"BusType":"NVMe","IsBoot":true,"IsSystem":true,"IsReadOnly":false},
              {"Number":2,"FriendlyName":"USB Stick","UniqueId":"usb-stick-serial","Size":32000000000,"BusType":"USB","IsBoot":false,"IsSystem":false,"IsReadOnly":false,
               "Partitions":[
                 {"Name":"E:","Size":16000000000,"FileSystem":"exFAT","Label":"TRANSFER"},
                 {"Name":"Partition 2","Size":"15900000000","FileSystem":null,"Label":null}
               ]},
              {"Number":3,"FriendlyName":"Locked card","Size":16000000000,"BusType":"SD","IsBoot":false,"IsSystem":false,"IsReadOnly":true}
            ]"#).unwrap();
            let drives = parse_windows_disks(&json).unwrap();
            assert_eq!(drives.len(), 2, "read-only disks are not write targets");
            assert!(drives[0].is_system);
            assert_eq!(drives[1].name, "PhysicalDrive2");
            assert_eq!(drives[1].stable_id.as_deref(), Some("usb-stick-serial"));
            assert!(drives[1].removable);
            assert_eq!(drives[1].partitions.len(), 2);
            assert_eq!(drives[1].partitions[0].name, "E:");
            assert_eq!(drives[1].partitions[0].fstype.as_deref(), Some("exFAT"));
            assert_eq!(drives[1].partitions[0].label.as_deref(), Some("TRANSFER"));
            assert_eq!(drives[1].partitions[1].size, 15_900_000_000);
            assert_eq!(
                drives[1].contents_summary(),
                "2 partition(s): TRANSFER, data"
            );
        }

        // Phase `parses_windows_single_partition_object_from_older_json_producer`.
        {
            let json: Value = serde_json::from_str(
                r#"{"Number":7,"FriendlyName":"Card reader","UniqueId":"reader-7","Size":64000000000,"BusType":"SD","IsBoot":false,"IsSystem":false,"IsReadOnly":false,
                    "Partitions":{"Name":"F:","Size":63900000000,"FileSystem":"FAT32","Label":"CAMERA"}}"#,
            )
            .unwrap();

            let drives = parse_windows_disks(&json).unwrap();
            assert_eq!(drives.len(), 1);
            assert_eq!(drives[0].name, "PhysicalDrive7");
            assert_eq!(drives[0].stable_id.as_deref(), Some("reader-7"));
            assert_eq!(drives[0].partitions.len(), 1);
            assert_eq!(drives[0].partitions[0].name, "F:");
            assert_eq!(drives[0].contents_summary(), "1 partition(s): CAMERA");
        }

        // Phase `successful_windows_probe_can_authoritatively_return_no_drives`.
        {
            assert_eq!(
                parse_windows_disks(&serde_json::json!([])).unwrap().len(),
                0
            );
            assert!(parse_windows_disks(&serde_json::json!({})).is_none());
        }
    }
}
