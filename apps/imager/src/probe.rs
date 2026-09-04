//! Live target enumeration: turn `lsblk -J -b` into a list of
//! [`TargetDrive`]s.
//!
//! Best-effort and side-effect free — if `lsblk` is missing or returns
//! nonsense we degrade to `None`, letting the caller fall back to the
//! curated sample so the UI is always populated. This module is only
//! exercised in the live binary; `--shots` never calls it, so the
//! screenshot stays deterministic offline.

#[cfg(target_os = "macos")]
use std::io::Write;
use std::path::Path;
use std::process::Command;
#[cfg(target_os = "macos")]
use std::process::Stdio;

use serde_json::Value;

use crate::model::{Source, SourceIdentity, SourceKind, TargetDrive, TargetKind, TargetPartition};

#[cfg(not(target_os = "linux"))]
pub fn pick_image_native() -> ui::PortalChannel {
    let (tx, rx) = std::sync::mpsc::channel();
    let _ = std::thread::Builder::new()
        .name("imager-native-picker".into())
        .spawn(move || {
            let outcome = native_image_path().map_or_else(
                |error| ui::PortalOutcome::Failed(error.to_string()),
                |path| path.map_or(ui::PortalOutcome::Cancelled, ui::PortalOutcome::Opened),
            );
            let _ = tx.send(outcome);
        });
    rx
}

#[cfg(target_os = "macos")]
fn native_image_path() -> std::io::Result<Option<std::path::PathBuf>> {
    let script = r#"try
set picked to choose file with prompt "Choose a qOS or disk image"
POSIX path of picked
on error number -128
return ""
end try"#;
    let out = Command::new("osascript").args(["-e", script]).output()?;
    if !out.status.success() {
        return Err(std::io::Error::other("macOS file dialog failed"));
    }
    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok((!path.is_empty()).then(|| std::path::PathBuf::from(path)))
}

#[cfg(target_os = "windows")]
fn native_image_path() -> std::io::Result<Option<std::path::PathBuf>> {
    let script = r#"Add-Type -AssemblyName System.Windows.Forms; $d=New-Object System.Windows.Forms.OpenFileDialog; $d.Title='Choose a qOS or disk image'; $d.Filter='Disk images (*.iso;*.img;*.raw)|*.iso;*.img;*.raw|All files (*.*)|*.*'; if($d.ShowDialog() -eq 'OK'){[Console]::Out.Write($d.FileName)}"#;
    let out = Command::new("powershell.exe")
        .args(["-NoProfile", "-STA", "-Command", script])
        .output()?;
    if !out.status.success() {
        return Err(std::io::Error::other("Windows file dialog failed"));
    }
    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok((!path.is_empty()).then(|| std::path::PathBuf::from(path)))
}

/// Enumerate candidate write targets. Returns `None` when `lsblk` is
/// unavailable or produced no usable disks.
pub fn probe() -> Option<Vec<TargetDrive>> {
    probe_platform()
}

#[cfg(target_os = "linux")]
fn probe_platform() -> Option<Vec<TargetDrive>> {
    let out = Command::new("lsblk")
        .args([
            "-J",
            "-b",
            "-o",
            "NAME,MODEL,SERIAL,WWN,SIZE,TYPE,ROTA,TRAN,RM,FSTYPE,LABEL,MOUNTPOINT",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let json: Value = serde_json::from_slice(&out.stdout).ok()?;
    parse_lsblk(&json)
}

/// macOS: enumerate external physical media only. Internal disks are omitted
/// because raw imaging the running APFS container is never a valid media-
/// creation operation; the live qOS installer handles internal installation.
#[cfg(target_os = "macos")]
fn probe_platform() -> Option<Vec<TargetDrive>> {
    let plist = Command::new("diskutil")
        .args(["list", "-plist", "external", "physical"])
        .output()
        .ok()?;
    if !plist.status.success() {
        return None;
    }
    let json = plist_to_json(&plist.stdout)?;
    parse_diskutil(&json)
}

#[cfg(target_os = "macos")]
fn plist_to_json(plist: &[u8]) -> Option<Value> {
    let mut child = Command::new("plutil")
        .args(["-convert", "json", "-o", "-", "--", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .ok()?;
    child.stdin.take()?.write_all(plist).ok()?;
    let out = child.wait_with_output().ok()?;
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
    (!drives.is_empty()).then_some(drives)
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
    let out = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .output()
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
    let disks = if let Some(items) = json.as_array() {
        items
    } else {
        one = vec![json.clone()];
        &one
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
    (!drives.is_empty()).then_some(drives)
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
    let devs = json.get("blockdevices")?.as_array()?;
    let mut drives = Vec::new();
    for dev in devs {
        if dev.get("type").and_then(|v| v.as_str()) != Some("disk") {
            continue;
        }
        let size = dev.get("size").and_then(as_u64).unwrap_or(0);
        if size == 0 {
            continue; // skip empty loop / nbd shells
        }
        let name = str_field(dev, "name").unwrap_or_default();
        if name.is_empty() {
            continue;
        }
        let rota = dev.get("rota").and_then(as_bool).unwrap_or(false);
        let tran = str_field(dev, "tran");
        let removable = dev.get("rm").and_then(as_bool).unwrap_or(false);
        let kind = TargetKind::from_lsblk(rota, tran.as_deref(), removable);

        let mut partitions = Vec::new();
        let mut is_system = false;
        if let Some(children) = dev.get("children").and_then(|v| v.as_array()) {
            collect_parts(children, &mut partitions, &mut is_system);
        }

        drives.push(TargetDrive {
            name: name.clone(),
            model: str_field(dev, "model").unwrap_or_else(|| name.clone()),
            stable_id: str_field(dev, "wwn").or_else(|| str_field(dev, "serial")),
            size,
            kind,
            tran,
            removable,
            is_system,
            partitions,
        });
    }

    if drives.is_empty() {
        None
    } else {
        Some(drives)
    }
}

/// Flatten partitions; flag the drive as the system disk if any child is
/// mounted at `/`, `/boot` or `/boot/efi` (writing there destroys the
/// running OS).
fn collect_parts(children: &[Value], out: &mut Vec<TargetPartition>, is_system: &mut bool) {
    for c in children {
        let ty = c.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let size = c.get("size").and_then(as_u64).unwrap_or(0);
        if let Some(mp) = str_field(c, "mountpoint")
            && (mp == "/" || mp == "/boot" || mp == "/boot/efi" || mp.starts_with("/var"))
        {
            *is_system = true;
        }
        if ty == "part" || ty == "crypt" || ty == "lvm" {
            out.push(TargetPartition {
                name: str_field(c, "name").unwrap_or_default(),
                size,
                fstype: str_field(c, "fstype"),
                label: str_field(c, "label"),
            });
        }
        if let Some(nested) = c.get("children").and_then(|v| v.as_array()) {
            collect_parts(nested, out, is_system);
        }
    }
}

// ---------------------------------------------------------------------------
// Source inspection — turn a picked file path into a populated `Source`.
// ---------------------------------------------------------------------------

/// Inspect a local image file the user picked (via the portal) and build a
/// [`Source`] from real metadata: the on-disk size (via `stat`) and, for an
/// ISO, the ISO-9660 volume label read straight from the file. Returns `None`
/// if the path does not exist or cannot be read.
pub fn inspect_source(path: &Path) -> Option<Source> {
    let meta = std::fs::metadata(path).ok()?;
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
    let label = read_iso_label(path)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            path.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("image")
                .to_string()
        });

    // A qOS image (by name or volume label) can drive a full pre-configured
    // install; anything else is written as-is.
    let hay = format!(
        "{} {}",
        name.to_ascii_lowercase(),
        label.to_ascii_lowercase()
    );
    let is_qos = hay.contains("qos");
    let kind = if is_qos {
        SourceKind::QosImage
    } else {
        SourceKind::IsoFile
    };

    // Only bless real disk-image extensions; a stray `.txt` is not an image.
    let looks_like_image = matches!(ext.as_str(), "iso" | "img" | "raw" | "bin" | "dd") || is_qos;
    if !looks_like_image {
        return None;
    }

    Some(Source {
        kind,
        path: path.to_string_lossy().to_string(),
        name,
        size,
        label,
        installable: is_qos,
        identity: SourceIdentity::from_metadata(&meta),
        expected_sha256: None,
    })
}

/// Read the ISO-9660 Primary Volume Descriptor's volume identifier. The PVD
/// lives at byte 0x8000 (logical sector 16); its 5-byte standard identifier is
/// `CD001` at offset 1, and the 32-byte, space-padded volume id sits at offset
/// 40 (file offset 0x8028). Best-effort: any short/mismatched file yields
/// `None` and the caller falls back to the file name.
pub fn read_iso_label(path: &Path) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
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

/// `lsblk -b` emits sizes as JSON numbers, but older versions emit
/// strings — accept both.
fn as_u64(v: &Value) -> Option<u64> {
    if let Some(n) = v.as_u64() {
        return Some(n);
    }
    v.as_str().and_then(|s| s.parse::<u64>().ok())
}

/// Older `lsblk` emits the `rm`/`rota` flags as `"0"`/`"1"` strings; newer
/// as booleans. Accept both.
fn as_bool(v: &Value) -> Option<bool> {
    if let Some(b) = v.as_bool() {
        return Some(b);
    }
    match v.as_str() {
        Some("1" | "true") => Some(true),
        Some("0" | "false") => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_usb_and_nvme_from_fixture() {
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

    #[test]
    fn skips_zero_size_and_non_disk() {
        let json: Value = serde_json::from_str(
            r#"{"blockdevices":[
                 {"name":"loop0","size":0,"type":"loop"},
                 {"name":"sr0","size":0,"type":"rom"}
            ]}"#,
        )
        .unwrap();
        assert!(parse_lsblk(&json).is_none());
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

    #[test]
    fn reads_real_iso9660_volume_label() {
        let iso = iso_with_label("QOS_1_0_X86_64");
        assert_eq!(
            read_iso_label(iso.path()).as_deref(),
            Some("QOS_1_0_X86_64")
        );
    }

    #[test]
    fn inspect_source_uses_volume_label_and_marks_qos_installable() {
        let iso = iso_with_label("QOS_1_0");
        let src = inspect_source(iso.path()).expect("inspect");
        assert_eq!(src.kind, SourceKind::QosImage);
        assert!(src.installable, "a qOS image can be pre-configured");
        assert_eq!(src.label, "QOS_1_0");
        assert!(src.size > 0);
        assert!(
            std::path::Path::new(&src.name)
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("iso"))
        );
    }

    #[test]
    fn inspect_source_treats_foreign_iso_as_write_only() {
        // No CD001 magic, but a `.iso` extension → still a valid image source,
        // just not installable, and it falls back to the file stem for a label.
        let f = tempfile::Builder::new().suffix(".iso").tempfile().unwrap();
        let src = inspect_source(f.path()).expect("inspect");
        assert_eq!(src.kind, SourceKind::IsoFile);
        assert!(!src.installable);
    }

    #[test]
    fn inspect_source_rejects_non_image_files() {
        let f = tempfile::Builder::new().suffix(".txt").tempfile().unwrap();
        assert!(inspect_source(f.path()).is_none());
        assert!(inspect_source(std::path::Path::new("/no/such/file.iso")).is_none());
    }

    #[test]
    fn accepts_string_flags_from_older_lsblk() {
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

    #[test]
    fn parses_macos_external_diskutil_inventory() {
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

    #[test]
    fn parses_windows_get_disk_and_flags_system_disk() {
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

    #[test]
    fn parses_windows_single_partition_object_from_older_json_producer() {
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
}
