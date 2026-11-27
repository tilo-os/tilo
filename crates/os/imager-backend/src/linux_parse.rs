//! Pure parsers for the Linux adapter (`sfdisk --json`, `lsblk -J`, `/proc/self/mountinfo`,
//! `/sys/class/power_supply`), free of I/O so they are tested on every development host.

use block_devices::{BlockDevice, sfdisk};
use serde_json::Value;

use super::platform::{Disk, Partition};

/// `sfdisk --json <disk>` for a GPT disk; `sector_bytes` is used when the dump predates the
/// `sectorsize` field.
pub fn disk_from_sfdisk(json: &str, size_bytes: u64, sector_bytes: u64) -> Result<Disk, String> {
    let table = sfdisk::parse(json).map_err(|error| error.to_string())?;
    if table.label != "gpt" {
        return Err("not-gpt".into());
    }
    if table.unit.as_deref().is_some_and(|unit| unit != "sectors") {
        return Err("sfdisk reported a unit other than sectors".into());
    }
    let sector = table.sector_bytes.unwrap_or(sector_bytes);
    let mut partitions = Vec::new();
    for part in &table.partitions {
        let start = part.start.ok_or("partition lacks start")?;
        let size = part.size.ok_or("partition lacks size")?;
        if size == 0 {
            return Err("zero-size partition".into());
        }
        let last_lba = start
            .checked_add(size.saturating_sub(1))
            .ok_or("partition extent overflows")?;
        partitions.push(Partition {
            number: sfdisk::node_number(&part.node).ok_or("partition node has no number")?,
            guid: part.uuid.clone(),
            type_guid: part.kind.clone(),
            first_lba: start,
            last_lba,
        });
    }
    if table.id.is_empty() {
        return Err("sfdisk JSON lacks the disk GUID".into());
    }
    Ok(Disk {
        id: table.device,
        guid: table.id,
        size_bytes,
        sector_bytes: sector,
        first_usable_lba: table.first_usable_lba.ok_or("sfdisk JSON lacks firstlba")?,
        last_usable_lba: table.last_usable_lba.ok_or("sfdisk JSON lacks lastlba")?,
        partitions,
    })
}

/// What `lsblk -J -b -o NAME,PATH,TYPE,SIZE,LOG-SEC,RM,PTUUID,PTTYPE,PARTUUID,PARTTYPE,PARTN` says
/// about one disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LsblkDisk {
    pub name: String,
    pub path: String,
    pub size_bytes: u64,
    pub sector_bytes: u64,
    pub removable: bool,
    pub pttype: Option<String>,
    pub ptuuid: Option<String>,
    pub partitions: Vec<(String, u32, String, String)>,
}

pub fn lsblk_disk(json: &str) -> Result<LsblkDisk, String> {
    let value: Value = serde_json::from_str(json).map_err(|e| format!("lsblk JSON: {e}"))?;
    let devices = block_devices::lsblk::parse(&value).ok_or("lsblk JSON has no devices")?;
    let [disk] = devices.as_slice() else {
        return Err("lsblk returned more than one device".into());
    };
    if disk.kind != "disk" {
        return Err("disk-unsupported: the system device is not a whole disk".into());
    }
    let mut partitions = Vec::new();
    for child in disk.children.iter().filter(|child| child.kind == "part") {
        partitions.push((
            present(&child.name).ok_or("partition lacks a name")?,
            child.partn.ok_or("partition lacks PARTN")?,
            child
                .partuuid
                .clone()
                .ok_or("partition lacks PARTUUID")?
                .to_ascii_lowercase(),
            child
                .parttype
                .clone()
                .ok_or("partition lacks PARTTYPE")?
                .to_ascii_lowercase(),
        ));
    }
    Ok(LsblkDisk {
        name: present(&disk.name).ok_or("disk lacks a name")?,
        path: disk.path.clone().unwrap_or_default(),
        size_bytes: (disk.size != 0)
            .then_some(disk.size)
            .ok_or("disk lacks SIZE")?,
        sector_bytes: disk.log_sec.ok_or("disk lacks LOG-SEC")?,
        removable: disk.removable,
        pttype: disk.pttype.clone(),
        ptuuid: disk.ptuuid.clone().map(|s| s.to_ascii_lowercase()),
        partitions,
    })
}

/// A column that lsblk reports as an empty string reads as absent.
fn present(text: &str) -> Option<String> {
    (!text.is_empty()).then(|| text.to_owned())
}

/// Builds a [`Disk`] from lsblk plus sysfs `start`/`size` (always 512-byte units, whatever the
/// logical sector size). Bounds assume the standard 128-entry GPT; the elevated helper re-reads the
/// real header with `sfdisk`.
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
        if !start.is_multiple_of(per) || !size.is_multiple_of(per) || size == 0 {
            return Err("partition not aligned to the logical sector".into());
        }
        partitions.push(Partition {
            number: *number,
            guid: guid.clone(),
            type_guid: kind.clone(),
            first_lba: start.checked_div(per).ok_or("zero sector ratio")?,
            last_lba: start
                .checked_add(size)
                .and_then(|end| end.checked_div(per))
                .and_then(|end| end.checked_sub(1))
                .ok_or("partition extent overflows")?,
        });
    }
    let total = l.size_bytes.checked_div(s).ok_or("zero sector size")?;
    let array = 16384_u64.checked_div(s).ok_or("zero sector size")?;
    Ok(Disk {
        id: l.path.clone(),
        guid: l.ptuuid.clone().ok_or("disk has no GPT GUID")?,
        size_bytes: l.size_bytes,
        sector_bytes: s,
        first_usable_lba: array.saturating_add(2),
        last_usable_lba: total
            .checked_sub(2)
            .and_then(|total| total.checked_sub(array))
            .ok_or("disk too small for GPT")?,
        partitions,
    })
}

/// A mount's source device: the `/dev/...` source when there is one, else the `major:minor` field
/// when it is not anonymous (btrfs and composefs report `0:N`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MountSource {
    Path(String),
    Number(u32, u32),
}

/// One line of `/proc/self/mountinfo`, split into the fields its readers use.
#[derive(Clone, Copy, Debug)]
pub struct MountEntry<'a> {
    pub mount_id: &'a str,
    /// The `major:minor` device number.
    pub device: &'a str,
    pub mountpoint: &'a str,
    pub options: &'a str,
    pub filesystem: &'a str,
    pub source: &'a str,
}

/// Splits `/proc/self/mountinfo` into its entries, skipping malformed lines.
pub fn mount_entries(mountinfo: &str) -> impl Iterator<Item = MountEntry<'_>> {
    mountinfo.lines().filter_map(|line| {
        let (left, right) = line.split_once(" - ")?;
        let mut left = left.split_whitespace();
        let mount_id = left.next()?;
        let _parent = left.next()?;
        let device = left.next()?;
        let _root = left.next()?;
        let mountpoint = left.next()?;
        let options = left.next()?;
        let mut right = right.split_whitespace();
        let filesystem = right.next()?;
        let source = right.next().unwrap_or_default();
        Some(MountEntry {
            mount_id,
            device,
            mountpoint,
            options,
            filesystem,
            source,
        })
    })
}

pub fn mount_source(mountinfo: &str, mountpoint: &str) -> Option<MountSource> {
    let mut found = None;
    for entry in mount_entries(mountinfo) {
        if entry.mountpoint != mountpoint {
            continue;
        }
        let Some((major, minor)) = entry.device.split_once(':') else {
            continue;
        };
        let (Ok(major), Ok(minor)) = (major.parse::<u32>(), minor.parse::<u32>()) else {
            continue;
        };
        found = if entry.source.starts_with("/dev/") {
            Some(MountSource::Path(entry.source.to_string()))
        } else if major != 0 {
            Some(MountSource::Number(major, minor))
        } else {
            None
        };
    }
    found
}

/// Whole disks under a device, from `lsblk -J -s -o NAME,TYPE <dev>`; `Err` when a RAID sits in
/// between.
pub fn backing_disks(json: &str) -> Result<Vec<String>, String> {
    let value: Value = serde_json::from_str(json).map_err(|e| format!("lsblk JSON: {e}"))?;
    let devices = block_devices::lsblk::parse(&value).unwrap_or_default();
    let mut out = Vec::new();
    for device in devices.iter().flat_map(BlockDevice::descendants) {
        if device.kind.starts_with("raid") || device.kind == "mpath" {
            return Err("disk-unsupported: the system volume is on RAID".into());
        }
        if device.kind == "disk" {
            let name = present(&device.name).ok_or("device lacks a name")?;
            if !out.contains(&name) {
                out.push(name);
            }
        }
    }
    Ok(out)
}

#[derive(Clone, Debug, Default)]
pub struct PowerSupply {
    pub kind: String,
    pub online: Option<bool>,
    pub status: Option<String>,
    pub capacity: Option<u64>,
}

/// On battery below 50 %: no external supply online and a discharging battery under half charge.
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

pub fn efivar_payload(file: &[u8]) -> Result<Vec<u8>, String> {
    file.get(4..)
        .map(<[u8]>::to_vec)
        .ok_or_else(|| "efivarfs file shorter than its attributes".into())
}

pub fn efivar_file(payload: &[u8]) -> Vec<u8> {
    let mut out = 0x0000_0007_u32.to_le_bytes().to_vec();
    out.extend_from_slice(payload);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The same captured dump the table reader's tests read.
    const SFDISK: &str = include_str!("../../block-devices/tests/fixtures/sfdisk.json");

    tilo_test_env::scenario! {
        fn linux_tool_output_parses_exactly_and_refuses_overflow;

        phase sfdisk_partition_extent_overflow_refuses_instead_of_wrapping;
        {
            let mut dump: serde_json::Value = serde_json::from_str(SFDISK).unwrap();
            dump["partitiontable"]["partitions"][0]["start"] = u64::MAX.into();
            dump["partitiontable"]["partitions"][0]["size"] = 2.into();
            assert!(
                disk_from_sfdisk(&dump.to_string(), 1_000_215_216 * 512, 512).is_err(),
                "an extent that overflows is refused instead of wrapping"
            );
            dump["partitiontable"]["partitions"][0]["size"] = 1.into();
            assert_eq!(
                disk_from_sfdisk(&dump.to_string(), 1_000_215_216 * 512, 512)
                    .unwrap()
                    .partitions[0]
                    .last_lba,
                u64::MAX
            );
        }

        phase linux_disk_inventory_parsers_normalise_units_and_backing_devices;
        {
            {
                let d = disk_from_sfdisk(SFDISK, 1_000_215_216 * 512, 4096).unwrap();
                assert_eq!(d.guid, "a1b2c3d4-0000-4000-8000-00000000aaaa");
                assert_eq!(d.sector_bytes, 512, "the dump's sectorsize wins");
                assert_eq!(d.first_usable_lba, 34);
                assert_eq!(d.partitions.len(), 2);
                assert_eq!(d.partitions[0].number, 1);
                assert_eq!(d.partitions[0].type_guid, super::super::ESP_TYPE);
                assert_eq!(d.partitions[1].first_lba, 1_230_848);
                assert_eq!(d.partitions[1].last_lba, 1_230_848 + 2_097_152 - 1);
                assert_eq!(
                    disk_from_sfdisk(&SFDISK.replace("\"gpt\"", "\"dos\""), 0, 512).unwrap_err(),
                    "not-gpt"
                );
            }

            {
                let json = r#"{"blockdevices":[{"name":"sda","path":"/dev/sda","type":"disk","size":1099511627776,
                "log-sec":4096,"rm":"0","ptuuid":"A1B2C3D4-0000-4000-8000-00000000AAAA","pttype":"gpt",
                "partuuid":null,"parttype":null,"partn":null,
                "children":[{"name":"sda1","path":"/dev/sda1","type":"part","size":"536870912","log-sec":4096,
                "rm":"0","ptuuid":"x","pttype":"gpt","partuuid":"0F0E0D0C-0000-4000-8000-000000000001",
                "parttype":"C12A7328-F81F-11D2-BA4B-00A0C93EC93B","partn":1}]}]}"#;
                let l = lsblk_disk(json).unwrap();
                assert_eq!(l.name, "sda");
                assert_eq!(l.path, "/dev/sda");
                assert_eq!(l.partitions[0].0, "sda1");
                assert_eq!(l.partitions[0].1, 1, "the partition's PARTN");
                assert!(!l.removable);
                // sysfs: start 2048 (512-byte units) = LBA 256 on 4Kn.
                let d =
                    disk_from_lsblk(&l, &|name| (name == "sda1").then_some((2048, 1_048_576))).unwrap();
                assert_eq!(d.sector_bytes, 4096);
                assert_eq!(d.partitions[0].first_lba, 256);
                assert_eq!(d.partitions[0].last_lba, 256 + 131_072 - 1);
                assert_eq!(d.first_usable_lba, 6);
                assert_eq!(d.last_usable_lba, 1_099_511_627_776 / 4096 - 6);
                assert_eq!(
                    disk_from_lsblk(&l, &|_| Some((u64::MAX - 7, 8))).unwrap_err(),
                    "partition extent overflows"
                );
                let tiny = LsblkDisk {
                    size_bytes: 4096,
                    ..l.clone()
                };
                assert_eq!(
                    disk_from_lsblk(&tiny, &|_| Some((0, 8))).unwrap_err(),
                    "disk too small for GPT"
                );
                let dos = LsblkDisk {
                    pttype: Some("dos".into()),
                    ..l
                };
                assert!(disk_from_lsblk(&dos, &|_| None).is_err());
            }

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

        phase linux_power_and_efivar_facts_parse_exactly;
        {
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

            {
                let file = efivar_file(&[1, 0]);
                assert_eq!(file, vec![7, 0, 0, 0, 1, 0]);
                assert_eq!(efivar_payload(&file).unwrap(), vec![1, 0]);
                assert!(efivar_payload(&[7, 0]).is_err());
            }
        }
    }
}
