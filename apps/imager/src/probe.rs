//! Live target enumeration: turn `lsblk -J -b` into a list of
//! [`TargetDrive`]s.
//!
//! Best-effort and side-effect free — if `lsblk` is missing or returns
//! nonsense we degrade to `None`, letting the caller fall back to the
//! curated sample so the UI is always populated. This module is only
//! exercised in the live binary; `--shots` never calls it, so the
//! screenshot stays deterministic offline.

use std::path::Path;
use std::process::Command;

use serde_json::Value;

use crate::model::{Source, SourceKind, TargetDrive, TargetKind, TargetPartition};

/// Enumerate candidate write targets. Returns `None` when `lsblk` is
/// unavailable or produced no usable disks.
pub fn probe() -> Option<Vec<TargetDrive>> {
    let out = Command::new("lsblk")
        .args([
            "-J",
            "-b",
            "-o",
            "NAME,MODEL,SIZE,TYPE,ROTA,TRAN,RM,FSTYPE,LABEL,MOUNTPOINT",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let json: Value = serde_json::from_slice(&out.stdout).ok()?;
    parse_lsblk(&json)
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
        if let Some(mp) = str_field(c, "mountpoint") {
            if mp == "/" || mp == "/boot" || mp == "/boot/efi" || mp.starts_with("/var") {
                *is_system = true;
            }
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
    let hay = format!("{} {}", name.to_ascii_lowercase(), label.to_ascii_lowercase());
    let is_qos = hay.contains("qos");
    let kind = if is_qos { SourceKind::QosImage } else { SourceKind::IsoFile };

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
    let label = String::from_utf8_lossy(raw).trim().trim_matches('\0').trim().to_string();
    if label.is_empty() { None } else { Some(label) }
}

fn str_field(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
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
        Some("1") | Some("true") => Some(true),
        Some("0") | Some("false") => Some(false),
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
        // Pad up to the PVD sector, then write: type(1) + "CD001" + version(1)
        // + 32 bytes system id + 32 bytes volume id (space-padded).
        f.seek(SeekFrom::Start(0x8000)).unwrap();
        let mut pvd = vec![0x01u8];
        pvd.extend_from_slice(b"CD001");
        pvd.push(0x01);
        pvd.extend_from_slice(&[b' '; 32]); // system identifier
        let mut vol = [b' '; 32];
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
        assert_eq!(read_iso_label(iso.path()).as_deref(), Some("QOS_1_0_X86_64"));
    }

    #[test]
    fn inspect_source_uses_volume_label_and_marks_qos_installable() {
        let iso = iso_with_label("QOS_1_0");
        let src = inspect_source(iso.path()).expect("inspect");
        assert_eq!(src.kind, SourceKind::QosImage);
        assert!(src.installable, "a qOS image can be pre-configured");
        assert_eq!(src.label, "QOS_1_0");
        assert!(src.size > 0);
        assert!(src.name.ends_with(".iso"));
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
}
