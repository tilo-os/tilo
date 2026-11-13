//! Live target enumeration: turns `lsblk -J -b` into [`TargetDrive`]s.
//!
//! Degrades to `None` when the platform probe is missing or malformed, so the caller keeps its last
//! snapshot; a successful probe with no drives is `Some(Vec::new())`, which lets Rescan drop an
//! unplugged drive.

use std::path::Path;
#[cfg(any(target_os = "macos", target_os = "windows"))]
use std::process::Command;

use serde_json::Value;

use block_devices::BlockDevice;

use crate::media_model::{
    Source, SourceIdentity, SourceKind, TargetDrive, TargetKind, TargetPartition,
};

const PROBE_OUTPUT_LIMIT: usize = 1024 * 1024;
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// `None` when the probe is unavailable or malformed; `Some(Vec::new())` when it found no usable
/// disks.
pub fn probe() -> Option<Vec<TargetDrive>> {
    probe_platform()
}

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

/// Mountpoints in a Flatpak's namespace cannot prove that a USB disk is not the host's root disk,
/// so a missing host-side fact hides the candidate.
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
            if let Ok(mounts) = udisks
                .filesystem(&drive.name)
                .and_then(|filesystem| filesystem.get_property::<Vec<Vec<u8>>>("MountPoints"))
            {
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
                let Ok(mounts) = udisks
                    .filesystem(&partition.name)
                    .and_then(|filesystem| filesystem.get_property::<Vec<Vec<u8>>>("MountPoints"))
                else {
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

/// Any missing property, partition-query error, read-only target or cross-namespace size
/// disagreement rejects the candidate.
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

/// Internal disks are omitted: raw imaging the running APFS container is never media creation, and
/// the live installer handles internal installation.
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
    // A partition without a mountable volume still appears by number and size, so a populated
    // drive never looks empty.
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
    // `Get-Disk | ConvertTo-Json` emits `[]` when the inventory is empty. A singleton object is
    // still accepted, but a malformed one is a probe failure rather than an empty snapshot.
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
        push(value);
    }
    partitions
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn probe_platform() -> Option<Vec<TargetDrive>> {
    None
}

/// Split out from [`probe`], so it can be unit-tested against a fixture without a real `lsblk`.
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

/// Flags the drive as the system disk if any child is mounted at `/`, `/boot` or `/boot/efi`.
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

/// One opened file: its on-disk size, identity and, for an ISO, the volume label read from the
/// file. `None` if the path does not exist or cannot be read.
pub fn inspect_source(path: &Path) -> Option<Source> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // The path may be a FIFO: reach the held-file type check without waiting for a writer.
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

    let label = read_iso_label_file(&mut file)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            path.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("image")
                .to_string()
        });

    // Local files have no release-catalog provenance, so even a Tilo-looking name is write-only.
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

/// Reads the ISO-9660 Primary Volume Descriptor's volume id: `CD001` at offset 1 of logical
/// sector 16, the 32-byte space-padded id at offset 40. A short or mismatched file yields `None`.
pub fn read_iso_label(path: &Path) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    read_iso_label_file(&mut file)
}

fn read_iso_label_file(f: &mut std::fs::File) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    f.seek(SeekFrom::Start(0x8000)).ok()?;
    let mut hdr = [0u8; 72];
    f.read_exact(&mut hdr).ok()?;
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
    #[cfg(unix)]
    use tilo_test_env::SpawnUnderEnvLock as _;

    #[test]
    #[cfg(all(target_os = "linux", feature = "flatpak-media-writer"))]
    fn flatpak_host_reconciliation_and_mount_classification_fail_closed() {
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

        {
            assert!(host_mount_is_system(b"/\0"));
            assert!(host_mount_is_system(b"/var/home\0"));
            assert!(!host_mount_is_system(b"/run/media/alice/USB\0"));
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn linux_lsblk_inventory_parses_and_may_authoritatively_be_empty() {
        {
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
            assert!(drives[1].is_system);
        }

        {
            let input: Value = serde_json::from_str(include_str!(
                "../../../system/block-devices/tests/fixtures/imager.json"
            ))
            .unwrap();
            let expected: Value = serde_json::from_str(include_str!(
                "../../../../apps/imager/tests/fixtures/lsblk-targets.json"
            ))
            .unwrap();
            assert_eq!(serde_json::to_value(parse_lsblk(&input)).unwrap(), expected);
        }

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

    #[test]
    fn nested_system_mounts_and_stable_identity_survive_inventory_projection() {
        for mount in ["/", "/boot", "/boot/efi", "/var", "/var/home"] {
            let inventory = serde_json::json!({"blockdevices": [{
                "name": "sda", "type": "disk", "size": 64_000_000_000_u64,
                "wwn": "wwn-disk", "serial": "serial-disk", "tran": "usb", "rm": true,
                "children": [{"name": "sda1", "type": "part", "size": 60_000_000_000_u64,
                    "children": [{"name": "crypt-root", "type": "crypt", "size": 59_000_000_000_u64,
                        "children": [{"name": "root", "type": "lvm", "size": 58_000_000_000_u64,
                            "mountpoint": mount, "fstype": "ext4", "label": "system"}]}]}]
            }]});
            let drives = parse_lsblk(&inventory).expect("controlled nested inventory");
            assert_eq!(drives.len(), 1);
            let drive = &drives[0];
            assert!(
                drive.is_system,
                "nested mount {mount} protects the whole disk"
            );
            assert_eq!(drive.stable_id.as_deref(), Some("wwn-disk"));
            assert_eq!(drive.partitions.len(), 3);
            assert_eq!(drive.partitions[2].label.as_deref(), Some("system"));
        }
        let inventory = serde_json::json!({"blockdevices": [{
            "name": "sdb", "type": "disk", "size": 32_000_000_000_u64,
            "serial": "serial-only", "tran": "usb", "rm": true,
            "children": [{"name": "sdb1", "type": "part", "size": 31_000_000_000_u64,
                "mountpoint": "/run/media/person/backup", "fstype": "ext4"}]
        }]});
        let drives = parse_lsblk(&inventory).expect("controlled removable inventory");
        assert_eq!(drives.len(), 1);
        assert!(!drives[0].is_system);
        assert_eq!(drives[0].stable_id.as_deref(), Some("serial-only"));
    }

    /// Builds a file whose ISO-9660 PVD carries `label`, so the reader parses a real volume
    /// descriptor.
    fn iso_with_label(label: &str) -> tempfile::NamedTempFile {
        use std::io::{Seek, SeekFrom, Write};
        let mut f = tempfile::Builder::new().suffix(".iso").tempfile().unwrap();
        // Pad to the PVD sector, then the real header layout: type(1), "CD001"(5), version(1),
        // unused(1), system id(32), volume id(32) at offset 40 (file offset 0x8028).
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

    #[test]
    fn inspect_source_classifies_local_images_without_trusting_names() {
        {
            let iso = iso_with_label("TILO_1_0_X86_64");
            assert_eq!(
                read_iso_label(iso.path()).as_deref(),
                Some("TILO_1_0_X86_64")
            );
        }

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

        {
            let f = tempfile::Builder::new().suffix(".iso").tempfile().unwrap();
            let src = inspect_source(f.path()).expect("inspect");
            assert_eq!(src.kind, SourceKind::IsoFile);
            assert!(!src.installable);
        }

        {
            let f = tempfile::Builder::new().suffix(".txt").tempfile().unwrap();
            assert!(inspect_source(f.path()).is_none());
            assert!(inspect_source(std::path::Path::new("/no/such/file.iso")).is_none());
        }

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

        // Isolate a blocking open in a subprocess, so a timeout can kill and reap it before the
        // FIFO is removed.
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "probe::tests::inspect_source_rejects_fifo_without_waiting_for_a_writer",
            ])
            .env(CHILD_FIFO, &fifo)
            .stdout(std::fs::File::create(root.path().join("child.stdout")).unwrap())
            .spawn_locked()
            .unwrap();
        let status =
            bounded_command::wait_or_kill(&mut child, tilo_test_env::hang_bound()).unwrap();
        assert!(
            status.is_some(),
            "source inspection blocked on a FIFO with no writer"
        );
        assert!(
            status.unwrap().success(),
            "FIFO inspection must reject the source"
        );
        let stdout = std::fs::read_to_string(root.path().join("child.stdout")).unwrap();
        // libtest exits 0 when `--exact` selects nothing, so require that the child ran.
        assert!(
            stdout.contains("test result: ok. 1 passed"),
            "the child ran no test: {stdout}"
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn macos_and_windows_inventories_parse_and_may_authoritatively_be_empty() {
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

        {
            let empty = serde_json::json!({"AllDisksAndPartitions": []});
            assert_eq!(parse_diskutil(&empty).unwrap().len(), 0);
            assert!(parse_diskutil(&serde_json::json!({})).is_none());
        }

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
            assert_eq!(drives[1].partition_names(), ["TRANSFER"]);
        }

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
            assert_eq!(drives[0].partition_names(), ["CAMERA"]);
        }

        {
            assert_eq!(
                parse_windows_disks(&serde_json::json!([])).unwrap().len(),
                0
            );
            assert!(parse_windows_disks(&serde_json::json!({})).is_none());
        }
    }
}
