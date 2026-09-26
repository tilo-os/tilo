//! Pure parsers for the Linux adapter (`sfdisk --json`, `lsblk -J`,
//! `/proc/self/mountinfo`, `/sys/class/power_supply`). Kept free of I/O so
//! they are tested on every development host.

use serde_json::Value;

use super::platform::{Disk, Partition};

fn as_u64(v: &Value) -> Option<u64> {
    v.as_u64().or_else(|| v.as_str()?.trim().parse().ok())
}

fn as_bool(v: &Value) -> Option<bool> {
    v.as_bool().or_else(|| match v.as_str()?.trim() {
        "1" => Some(true),
        "0" => Some(false),
        _ => None,
    })
}

/// Trailing partition number of a device node (`/dev/nvme0n1p3` → 3).
pub fn node_number(node: &str) -> Option<u32> {
    let digits: String = node
        .chars()
        .rev()
        .take_while(char::is_ascii_digit)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    digits.parse().ok()
}

/// `sfdisk --json <disk>` for a GPT disk. `sector_bytes` is used when the
/// dump predates the `sectorsize` field.
pub fn disk_from_sfdisk(json: &str, size_bytes: u64, sector_bytes: u64) -> Result<Disk, String> {
    let root: Value = serde_json::from_str(json).map_err(|e| format!("sfdisk JSON: {e}"))?;
    let table = root
        .get("partitiontable")
        .ok_or("sfdisk JSON has no partition table")?;
    if table.get("label").and_then(Value::as_str) != Some("gpt") {
        return Err("not-gpt".into());
    }
    if table
        .get("unit")
        .and_then(Value::as_str)
        .is_some_and(|u| u != "sectors")
    {
        return Err("sfdisk reported a unit other than sectors".into());
    }
    let sector = table
        .get("sectorsize")
        .and_then(as_u64)
        .unwrap_or(sector_bytes);
    let field = |k: &str| {
        table
            .get(k)
            .and_then(as_u64)
            .ok_or(format!("sfdisk JSON lacks {k}"))
    };
    let mut partitions = Vec::new();
    for p in table
        .get("partitions")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        let text = |k: &str| {
            p.get(k)
                .and_then(Value::as_str)
                .map(str::to_ascii_lowercase)
                .ok_or(format!("sfdisk partition lacks {k}"))
        };
        let node = p.get("node").and_then(Value::as_str).unwrap_or_default();
        let start = p
            .get("start")
            .and_then(as_u64)
            .ok_or("partition lacks start")?;
        let size = p
            .get("size")
            .and_then(as_u64)
            .ok_or("partition lacks size")?;
        if size == 0 {
            return Err("zero-size partition".into());
        }
        partitions.push(Partition {
            number: node_number(node).ok_or("partition node has no number")?,
            guid: text("uuid")?,
            type_guid: text("type")?,
            first_lba: start,
            last_lba: start + size - 1,
        });
    }
    Ok(Disk {
        id: table
            .get("device")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .into(),
        guid: table
            .get("id")
            .and_then(Value::as_str)
            .ok_or("sfdisk JSON lacks the disk GUID")?
            .to_ascii_lowercase(),
        size_bytes,
        sector_bytes: sector,
        first_usable_lba: field("firstlba")?,
        last_usable_lba: field("lastlba")?,
        partitions,
    })
}

/// What `lsblk -J -b -o NAME,PATH,TYPE,SIZE,LOG-SEC,RM,PTUUID,PTTYPE,PARTUUID,PARTTYPE,PARTN <disk>`
/// says about one disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LsblkDisk {
    pub name: String,
    pub path: String,
    pub size_bytes: u64,
    pub sector_bytes: u64,
    pub removable: bool,
    pub pttype: Option<String>,
    pub ptuuid: Option<String>,
    /// (kernel name, partition number, PARTUUID, PARTTYPE)
    pub partitions: Vec<(String, u32, String, String)>,
}

pub fn lsblk_disk(json: &str) -> Result<LsblkDisk, String> {
    let root: Value = serde_json::from_str(json).map_err(|e| format!("lsblk JSON: {e}"))?;
    let devices = root
        .get("blockdevices")
        .and_then(Value::as_array)
        .ok_or("lsblk JSON has no devices")?;
    let [disk] = devices.as_slice() else {
        return Err("lsblk returned more than one device".into());
    };
    let text = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    if text(disk, "type").as_deref() != Some("disk") {
        return Err("disk-unsupported: the system device is not a whole disk".into());
    }
    let mut partitions = Vec::new();
    for child in disk
        .get("children")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        if text(child, "type").as_deref() != Some("part") {
            continue;
        }
        partitions.push((
            text(child, "name").ok_or("partition lacks a name")?,
            child
                .get("partn")
                .and_then(as_u64)
                .and_then(|n| u32::try_from(n).ok())
                .ok_or("partition lacks PARTN")?,
            text(child, "partuuid")
                .ok_or("partition lacks PARTUUID")?
                .to_ascii_lowercase(),
            text(child, "parttype")
                .ok_or("partition lacks PARTTYPE")?
                .to_ascii_lowercase(),
        ));
    }
    Ok(LsblkDisk {
        name: text(disk, "name").ok_or("disk lacks a name")?,
        path: text(disk, "path").unwrap_or_default(),
        size_bytes: disk.get("size").and_then(as_u64).ok_or("disk lacks SIZE")?,
        sector_bytes: disk
            .get("log-sec")
            .and_then(as_u64)
            .ok_or("disk lacks LOG-SEC")?,
        removable: disk.get("rm").and_then(as_bool).unwrap_or(false),
        pttype: text(disk, "pttype"),
        ptuuid: text(disk, "ptuuid").map(|s| s.to_ascii_lowercase()),
        partitions,
    })
}

/// Build a [`Disk`] from lsblk plus sysfs `start`/`size` (always 512-byte
/// units, whatever the logical sector size). Usable bounds assume the
/// standard 128-entry GPT, as every mainstream partitioner writes; the
/// elevated helper re-reads the real header with `sfdisk`.
pub fn disk_from_lsblk(
    l: &LsblkDisk,
    sysfs_extent: &dyn Fn(&str) -> Option<(u64, u64)>,
) -> Result<Disk, String> {
    if l.pttype.as_deref() != Some("gpt") {
        return Err("not-gpt".into());
    }
    let s = l.sector_bytes;
    if !(512..=4096).contains(&s) || !s.is_power_of_two() || !l.size_bytes.is_multiple_of(s) {
        return Err("unsupported sector size".into());
    }
    let per = s / 512;
    let mut partitions = Vec::new();
    for (name, number, guid, kind) in &l.partitions {
        let (start, size) = sysfs_extent(name).ok_or("partition extent unreadable")?;
        if start % per != 0 || size % per != 0 || size == 0 {
            return Err("partition not aligned to the logical sector".into());
        }
        partitions.push(Partition {
            number: *number,
            guid: guid.clone(),
            type_guid: kind.clone(),
            first_lba: start / per,
            last_lba: (start + size) / per - 1,
        });
    }
    let total = l.size_bytes / s;
    // Header + 16 KiB entry array at each end.
    let array = 16384 / s;
    Ok(Disk {
        id: l.path.clone(),
        guid: l.ptuuid.clone().ok_or("disk has no GPT GUID")?,
        size_bytes: l.size_bytes,
        sector_bytes: s,
        first_usable_lba: 2 + array,
        last_usable_lba: total - 2 - array,
        partitions,
    })
}

/// A mount's source device, from `/proc/self/mountinfo`: the `/dev/...`
/// source when there is one, else the `major:minor` field when not
/// anonymous (btrfs and composefs report anonymous `0:N`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MountSource {
    Path(String),
    Number(u32, u32),
}

pub fn mount_source(mountinfo: &str, mountpoint: &str) -> Option<MountSource> {
    let mut found = None;
    for line in mountinfo.lines() {
        let fields: Vec<&str> = line.split(' ').collect();
        let Some(dash) = fields.iter().position(|f| *f == "-") else {
            continue;
        };
        if fields.get(4) != Some(&mountpoint) {
            continue;
        }
        let source = fields.get(dash + 2).copied().unwrap_or_default();
        let Some((major, minor)) = fields.get(2).and_then(|f| f.split_once(':')) else {
            continue;
        };
        let (Ok(major), Ok(minor)) = (major.parse::<u32>(), minor.parse::<u32>()) else {
            continue;
        };
        // Later lines shadow earlier mounts at the same point.
        found = if source.starts_with("/dev/") {
            Some(MountSource::Path(source.to_string()))
        } else if major != 0 {
            Some(MountSource::Number(major, minor))
        } else {
            None
        };
    }
    found
}

/// Whole disks under a device, from `lsblk -J -s -o NAME,TYPE <dev>`
/// (inverse tree). `Err` when a RAID sits in between.
pub fn backing_disks(json: &str) -> Result<Vec<String>, String> {
    fn walk(v: &Value, out: &mut Vec<String>) -> Result<(), String> {
        let kind = v.get("type").and_then(Value::as_str).unwrap_or_default();
        if kind.starts_with("raid") || kind == "mpath" {
            return Err("disk-unsupported: the system volume is on RAID".into());
        }
        if kind == "disk" {
            let name = v
                .get("name")
                .and_then(Value::as_str)
                .ok_or("device lacks a name")?;
            if !out.iter().any(|n| n == name) {
                out.push(name.into());
            }
        }
        for child in v
            .get("children")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            walk(child, out)?;
        }
        Ok(())
    }
    let root: Value = serde_json::from_str(json).map_err(|e| format!("lsblk JSON: {e}"))?;
    let mut out = Vec::new();
    for dev in root
        .get("blockdevices")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        walk(dev, &mut out)?;
    }
    Ok(out)
}

/// One `/sys/class/power_supply/*` entry.
#[derive(Clone, Debug, Default)]
pub struct PowerSupply {
    pub kind: String,
    pub online: Option<bool>,
    pub status: Option<String>,
    pub capacity: Option<u64>,
}

/// On battery below 50 %: no external supply online and a discharging
/// battery under half charge.
pub fn on_low_battery(supplies: &[PowerSupply]) -> bool {
    let external = supplies
        .iter()
        .any(|s| s.kind != "Battery" && s.online == Some(true));
    !external
        && supplies.iter().any(|s| {
            s.kind == "Battery"
                && s.status.as_deref() == Some("Discharging")
                && s.capacity.is_some_and(|c| c < 50)
        })
}

/// Strip the 4-byte attribute prefix of an efivarfs file.
pub fn efivar_payload(file: &[u8]) -> Result<Vec<u8>, String> {
    if file.len() < 4 {
        return Err("efivarfs file shorter than its attributes".into());
    }
    Ok(file[4..].to_vec())
}

/// `NV|BS|RT` attributes followed by the payload, written in one `write()`.
pub fn efivar_file(payload: &[u8]) -> Vec<u8> {
    let mut out = 0x0000_0007_u32.to_le_bytes().to_vec();
    out.extend_from_slice(payload);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SFDISK: &str = r#"{
   "partitiontable": {
      "label": "gpt",
      "id": "A1B2C3D4-0000-4000-8000-00000000AAAA",
      "device": "/dev/nvme0n1",
      "unit": "sectors",
      "firstlba": 34,
      "lastlba": 1000215182,
      "sectorsize": 512,
      "partitions": [
         {"node": "/dev/nvme0n1p1", "start": 2048, "size": 1228800, "type": "C12A7328-F81F-11D2-BA4B-00A0C93EC93B", "uuid": "0F0E0D0C-0000-4000-8000-000000000001", "name": "EFI System Partition"},
         {"node": "/dev/nvme0n1p2", "start": 1230848, "size": 2097152, "type": "BC13C2FF-59E6-4262-A352-B275FD6F7172", "uuid": "0F0E0D0C-0000-4000-8000-000000000002"}
      ]
   }
}"#;

    /// sfdisk, lsblk/sysfs, mountinfo and LUKS/LVM backing disks.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn linux_disk_inventory_parsers_normalise_units_and_backing_devices() {
        // Phase `sfdisk_dump_becomes_a_lowercase_disk`.
        {
            let d = disk_from_sfdisk(SFDISK, 1000215216 * 512, 4096).unwrap();
            assert_eq!(d.guid, "a1b2c3d4-0000-4000-8000-00000000aaaa");
            assert_eq!(d.sector_bytes, 512, "the dump's sectorsize wins");
            assert_eq!(d.first_usable_lba, 34);
            assert_eq!(d.partitions.len(), 2);
            assert_eq!(d.partitions[0].number, 1);
            assert_eq!(d.partitions[0].type_guid, super::super::ESP_TYPE);
            assert_eq!(d.partitions[1].first_lba, 1230848);
            assert_eq!(d.partitions[1].last_lba, 1230848 + 2097152 - 1);
            assert_eq!(
                disk_from_sfdisk(&SFDISK.replace("\"gpt\"", "\"dos\""), 0, 512).unwrap_err(),
                "not-gpt"
            );
        }

        // Phase `lsblk_plus_sysfs_handles_4kn_512_byte_units`.
        {
            let json = r#"{"blockdevices":[{"name":"sda","path":"/dev/sda","type":"disk","size":1099511627776,
                "log-sec":4096,"rm":"0","ptuuid":"A1B2C3D4-0000-4000-8000-00000000AAAA","pttype":"gpt",
                "partuuid":null,"parttype":null,"partn":null,
                "children":[{"name":"sda1","path":"/dev/sda1","type":"part","size":"536870912","log-sec":4096,
                "rm":"0","ptuuid":"x","pttype":"gpt","partuuid":"0F0E0D0C-0000-4000-8000-000000000001",
                "parttype":"C12A7328-F81F-11D2-BA4B-00A0C93EC93B","partn":1}]}]}"#;
            let l = lsblk_disk(json).unwrap();
            assert!(!l.removable);
            // sysfs: start 2048 (512-byte units) = LBA 256 on 4Kn.
            let d =
                disk_from_lsblk(&l, &|name| (name == "sda1").then_some((2048, 1_048_576))).unwrap();
            assert_eq!(d.sector_bytes, 4096);
            assert_eq!(d.partitions[0].first_lba, 256);
            assert_eq!(d.partitions[0].last_lba, 256 + 131_072 - 1);
            assert_eq!(d.first_usable_lba, 6);
            assert_eq!(d.last_usable_lba, 1099511627776 / 4096 - 6);
            let dos = LsblkDisk {
                pttype: Some("dos".into()),
                ..l
            };
            assert!(disk_from_lsblk(&dos, &|_| None).is_err());
        }

        // Phase `mountinfo_prefers_device_sources_and_skips_anonymous`.
        {
            let info = "\
    22 1 0:21 / / rw,relatime shared:1 - overlay composefs ro\n\
    23 22 259:3 / /sysroot ro shared:2 - xfs /dev/nvme0n1p3 rw\n\
    24 22 0:40 / /var rw shared:3 - btrfs /dev/mapper/luks-abc rw\n\
    25 22 259:1 / /boot/efi rw shared:4 - vfat systemd-1 rw\n";
            assert_eq!(mount_source(info, "/"), None);
            assert_eq!(
                mount_source(info, "/sysroot"),
                Some(MountSource::Path("/dev/nvme0n1p3".into()))
            );
            assert_eq!(
                mount_source(info, "/var"),
                Some(MountSource::Path("/dev/mapper/luks-abc".into()))
            );
            assert_eq!(
                mount_source(info, "/boot/efi"),
                Some(MountSource::Number(259, 1))
            );
        }

        // Phase `backing_disks_through_luks_and_lvm`.
        {
            let lvm_two = r#"{"blockdevices":[{"name":"vg-root","type":"lvm","children":[
                {"name":"sda2","type":"part","children":[{"name":"sda","type":"disk"}]},
                {"name":"sdb1","type":"part","children":[{"name":"sdb","type":"disk"}]}]}]}"#;
            assert_eq!(backing_disks(lvm_two).unwrap(), vec!["sda", "sdb"]);
            let luks = r#"{"blockdevices":[{"name":"luks-x","type":"crypt","children":[
                {"name":"nvme0n1p3","type":"part","children":[{"name":"nvme0n1","type":"disk"}]}]}]}"#;
            assert_eq!(backing_disks(luks).unwrap(), vec!["nvme0n1"]);
            let raid = r#"{"blockdevices":[{"name":"md0","type":"raid1","children":[]}]}"#;
            assert!(backing_disks(raid).is_err());
        }
    }

    /// Battery rule and efivar attributes.
    #[test]
    fn linux_power_and_efivar_facts_parse_exactly() {
        // Phase `battery_rule_needs_discharging_below_half_and_no_ac`.
        {
            let bat = |status: &str, capacity| PowerSupply {
                kind: "Battery".into(),
                status: Some(status.into()),
                capacity: Some(capacity),
                ..PowerSupply::default()
            };
            let ac = |online| PowerSupply {
                kind: "Mains".into(),
                online: Some(online),
                ..PowerSupply::default()
            };
            assert!(on_low_battery(&[bat("Discharging", 49), ac(false)]));
            assert!(!on_low_battery(&[bat("Discharging", 50)]));
            assert!(!on_low_battery(&[bat("Discharging", 10), ac(true)]));
            assert!(!on_low_battery(&[bat("Charging", 10)]));
            assert!(!on_low_battery(&[]));
        }

        // Phase `efivar_files_carry_nv_bs_rt_attributes`.
        {
            let file = efivar_file(&[1, 0]);
            assert_eq!(file, vec![7, 0, 0, 0, 1, 0]);
            assert_eq!(efivar_payload(&file).unwrap(), vec![1, 0]);
            assert!(efivar_payload(&[7, 0]).is_err());
            assert_eq!(node_number("/dev/nvme0n1p12"), Some(12));
            assert_eq!(node_number("/dev/sda"), None);
        }
    }
}
