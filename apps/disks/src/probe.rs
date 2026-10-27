//! This machine's storage, as the session describes it.
//!
//! Disks runs in a sandbox with a near-empty `/dev`, no udev database, none of
//! the host's programs and no system bus: it cannot list the machine's disks
//! itself, and it must not be widened until it can. The session's system
//! information service lists them instead (`tilo_system_info::storage`:
//! `lsblk` for the device tree, `UDisks2` for each drive's health) and answers
//! [`Client::storage`](tilo_system_info::client::Client::storage) over the
//! connection the launcher handed Disks, which the image grants `system-info`
//! without a prompt. Reading changes nothing; operations on a device stay
//! `UDisks2` calls behind polkit ([`crate::ops`]).
//!
//! [`topology_from`] is the one parser from the service's answer to the
//! [`Topology`] the app draws, and is tested against storage captured from
//! real machines.

use tilo_system_info::protocol::{DriveHealth, Refusal, Storage, StorageDevice};

use crate::model::{
    AttrSeverity, Drive, DriveKind, Encryption, Partition, SmartAttr, SmartInfo, SmartStatus,
    SmartStatusOpt, Topology,
};

/// The app this is, when it dials the session itself (an unconfined run).
const APP_ID: &str = "so.tilo.disks";

/// Ask the session for this machine's storage.
///
/// # Errors
/// What the person is told when the storage cannot be read. The caller shows
/// an honest empty state with it and never substitutes devices.
pub fn probe() -> Result<Topology, String> {
    let client = tilo_system_info::connection(APP_ID).ok_or_else(unreadable)?;
    let storage = client.storage().map_err(|refusal| match refusal {
        Refusal::NotGranted { .. } | Refusal::Declined => {
            "Disks isn't allowed to see this device's storage".to_owned()
        }
        _ => unreadable(),
    })?;
    Ok(topology_from(&storage))
}

fn unreadable() -> String {
    "Storage devices can't be read right now".to_owned()
}

/// The service's storage as the drives Disks shows: every disk with a size,
/// its partitions (an encrypted one with what it holds once unlocked), and
/// its health. Physical drives first, virtual ones (loop) after. A zram
/// device is compressed memory, not storage, and is left out.
pub fn topology_from(storage: &Storage) -> Topology {
    let mut drives: Vec<Drive> = storage
        .devices
        .iter()
        .filter(|device| device.kind == "disk" && device.size > 0)
        .map(drive_from)
        .filter(|drive| drive.kind != DriveKind::Zram)
        .collect();
    // Physical media first, virtual devices after, so the default selection
    // lands on a real disk. Stable, so kernel enumeration order is kept
    // within each group.
    drives.sort_by_key(|drive| drive.kind.is_virtual());
    Topology { drives }
}

fn drive_from(device: &StorageDevice) -> Drive {
    let name = device.name.clone();
    let kind = DriveKind::from_lsblk(&name, device.rotational, device.transport.as_deref());
    let mut partitions: Vec<Partition> = device
        .children
        .iter()
        .filter(|child| child.kind == "part")
        .map(partition_from)
        .collect();
    // A device formatted (or encrypted) whole, with no partition table: a USB
    // stick as one exfat volume, a zram swap area. It is one span covering the
    // device rather than a blank, unallocated disk.
    if partitions.is_empty() && device.fstype.is_some() {
        partitions.push(partition_from(device));
    }
    Drive {
        model: device.model.clone().unwrap_or_else(|| name.clone()),
        serial: device.serial.clone(),
        size: device.size,
        kind,
        tran: device.transport.clone(),
        // A virtual device is never physically removable, whatever the
        // kernel's flag says; it is not offered for ejecting.
        removable: device.removable && !kind.is_virtual(),
        partitions,
        smart: Some(smart_from(device.health.as_ref(), kind)),
        name,
    }
}

/// The encryption scheme `fstype` names, if it is an encrypted container.
fn encryption_scheme(device: &StorageDevice) -> Option<String> {
    match device.fstype.as_deref()? {
        "crypto_LUKS" => Some(match device.fs_version.as_deref() {
            Some(version) if !version.is_empty() => format!("LUKS{version}"),
            _ => "LUKS".to_owned(),
        }),
        "BitLocker" => Some("BitLocker".to_owned()),
        _ => None,
    }
}

/// One partition. An encrypted one, once unlocked, shows the filesystem its
/// mapping holds (an ext4 home at `/var/home`); while locked it shows only
/// that it is encrypted. Either way it takes its own place on the disk once,
/// never again for the mapping inside it.
fn partition_from(device: &StorageDevice) -> Partition {
    let size = device.size;
    let encryption = encryption_scheme(device).map(|scheme| {
        let unlocked = device.children.iter().find(|child| child.kind == "crypt");
        (
            Encryption {
                scheme,
                unlocked_as: unlocked.map(|mapping| {
                    mapping
                        .kernel_name
                        .clone()
                        .unwrap_or_else(|| mapping.name.clone())
                }),
            },
            unlocked,
        )
    });
    let (encryption, holds) = match encryption {
        Some((encryption, unlocked)) => (Some(encryption), unlocked),
        None => (None, Some(device)),
    };
    let mountpoint = holds
        .and_then(|fs| fs.mountpoint.clone())
        .filter(|mount| mount != "null");
    Partition {
        name: device.name.clone(),
        size,
        fstype: holds.and_then(|fs| fs.fstype.clone()),
        label: holds
            .and_then(|fs| fs.label.clone())
            .or_else(|| device.label.clone()),
        mountpoint,
        // Usage over the partition's size is a whole-pool figure (one member
        // of a multi-device btrfs, LVM or RAID), not this partition's share,
        // so it is dropped rather than drawn as a bogus full bar.
        used: holds
            .and_then(|fs| fs.fs_used)
            .filter(|&used| size > 0 && used <= size),
        uuid: device.uuid.clone(),
        encryption,
        part_type: device.part_type.clone(),
        part_type_name: device.part_type_name.clone(),
    }
}

/// What a drive's health says, as Disks shows it. A drive that reports
/// nothing is an honest `Unknown` that says why.
pub fn smart_from(health: Option<&DriveHealth>, kind: DriveKind) -> SmartInfo {
    let unknown = |detail: &str| SmartInfo {
        status: SmartStatusOpt(SmartStatus::Unknown),
        detail: Some(detail.to_owned()),
        ..SmartInfo::default()
    };
    if kind.is_virtual() {
        return unknown("Virtual devices have no health data.");
    }
    let Some(health) = health.filter(|health| health.supported) else {
        return unknown(if kind == DriveKind::Usb {
            "USB adapters rarely pass the drive's health through."
        } else {
            "This drive doesn't report its health."
        });
    };
    if health.updated.is_none() {
        return unknown("The drive's health hasn't been read yet. Rescan in a few minutes.");
    }

    let attrs = health_attrs(health);
    let status = if health.failing {
        SmartStatus::Failing
    } else if attrs.iter().any(|attr| attr.severity != AttrSeverity::Ok) {
        SmartStatus::Warning
    } else {
        SmartStatus::Passed
    };
    let signed = |n: u64| i64::try_from(n).ok();
    SmartInfo {
        status: SmartStatusOpt(status),
        temp_c: health.temperature_c.map(i64::from),
        power_on_hours: health.power_on_hours.and_then(signed),
        power_cycles: health.power_cycles.and_then(signed),
        reallocated: health.bad_sectors.and_then(signed),
        pct_used: health.percent_used.map(i64::from),
        written_gb: health
            .bytes_written
            .map(|bytes| bytes / 1_000_000_000)
            .and_then(signed),
        attrs,
        detail: None,
    }
}

/// The readings that have no tile of their own, each with what it means.
fn health_attrs(health: &DriveHealth) -> Vec<SmartAttr> {
    let mut attrs = Vec::new();
    let mut row = |name: &str, value: String, severity: AttrSeverity| {
        attrs.push(SmartAttr {
            id: None,
            name: name.to_owned(),
            value,
            severity,
        });
    };
    for warning in &health.warnings {
        row(
            "Critical warning",
            warning_name(warning).to_owned(),
            AttrSeverity::Crit,
        );
    }
    if let Some(spare) = health.available_spare {
        let threshold = health.spare_threshold.unwrap_or(10);
        let severity = if spare <= threshold {
            AttrSeverity::Warn
        } else {
            AttrSeverity::Ok
        };
        row("Available spare", format!("{spare} %"), severity);
    }
    if let Some(used) = health.percent_used {
        let severity = if used > 90 {
            AttrSeverity::Warn
        } else {
            AttrSeverity::Ok
        };
        row("Percentage used", format!("{used} %"), severity);
    }
    if let Some(errors) = health.media_errors {
        let severity = if errors > 0 {
            AttrSeverity::Warn
        } else {
            AttrSeverity::Ok
        };
        row("Media and data errors", errors.to_string(), severity);
    }
    if let Some(shutdowns) = health.unsafe_shutdowns {
        row("Unsafe shutdowns", shutdowns.to_string(), AttrSeverity::Ok);
    }
    if let Some(failing) = health.failing_attributes {
        let severity = if failing > 0 {
            AttrSeverity::Crit
        } else {
            AttrSeverity::Ok
        };
        row("Attributes past threshold", failing.to_string(), severity);
    }
    if let Some(test) = health.self_test.as_deref() {
        let severity = match test {
            "success" | "inprogress" | "aborted" | "interrupted" => AttrSeverity::Ok,
            _ => AttrSeverity::Warn,
        };
        row("Last self-test", self_test_name(test).to_owned(), severity);
    }
    attrs
}

/// An `NVMe` critical warning as a person reads it.
fn warning_name(warning: &str) -> &str {
    match warning {
        "spare" => "Spare capacity is low",
        "temperature" => "Temperature is out of range",
        "degraded" => "Reliability is degraded",
        "readonly" => "The drive is read-only",
        "volatile_mem" => "Its backup memory failed",
        "pmr_readonly" => "Its persistent memory is read-only",
        other => other,
    }
}

/// A self-test outcome as a person reads it.
fn self_test_name(status: &str) -> &str {
    match status {
        "success" => "Passed",
        "aborted" | "interrupted" => "Stopped",
        "inprogress" => "Running",
        "fatal" | "fatal_error" | "error_fatal" => "Failed",
        "unknown" | "error_unknown" => "Failed for an unknown reason",
        "electrical" | "error_electrical" => "Failed an electrical test",
        "servo" | "error_servo" => "Failed a servo test",
        "read" | "error_read" => "Failed a read test",
        "handling" | "error_handling" => "Damaged in handling",
        other => other,
    }
}

/// Storage captured from real machines, read the way the session and the app
/// read it: the screenshots' `laptop` scenario and the tests.
pub(crate) mod captured {
    use std::collections::BTreeMap;

    use tilo_system_info::protocol::{DriveHealth, Storage};

    use crate::model::Topology;

    /// Storage the way the session builds it from `lsblk` output and the
    /// drives' health (by kernel name): the production path, from the
    /// captured text on.
    pub(crate) fn served(lsblk: &str, health: &BTreeMap<String, DriveHealth>) -> Option<Storage> {
        tilo_system_info::storage::from_lsblk(lsblk.as_bytes(), &mut |device| {
            health.get(device).cloned()
        })
    }

    /// The Yoga laptop (2026-10-22): one Samsung `NVMe` drive holding Windows,
    /// a LUKS2 home and a LUKS2 root with ext4 inside, and a zram swap.
    /// Serial numbers and UUIDs are replaced; sizes and layout are its own.
    pub(crate) fn yoga() -> Option<Topology> {
        let health: BTreeMap<String, DriveHealth> =
            serde_json::from_str(include_str!("../tests/fixtures/yoga-health.json")).ok()?;
        let storage = served(include_str!("../tests/fixtures/yoga-lsblk.json"), &health)?;
        Some(super::topology_from(&storage))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    use crate::model::PartRole;

    /// Storage as the session serves it, across the wire as Disks receives it.
    fn served(lsblk: &str, health: &BTreeMap<String, DriveHealth>) -> Storage {
        let storage = captured::served(lsblk, health).expect("an lsblk document");
        serde_json::from_slice(&serde_json::to_vec(&storage).unwrap()).unwrap()
    }

    fn yoga() -> Topology {
        let health: BTreeMap<String, DriveHealth> =
            serde_json::from_str(include_str!("../tests/fixtures/yoga-health.json")).unwrap();
        let topology = topology_from(&served(
            include_str!("../tests/fixtures/yoga-lsblk.json"),
            &health,
        ));
        assert_eq!(
            serde_json::to_value(&topology.drives).unwrap(),
            serde_json::to_value(&captured::yoga().unwrap().drives).unwrap(),
            "the screenshots' laptop is the one the tests read"
        );
        topology
    }

    /// Disks lists the laptop's disks and partitions: the Yoga's `NVMe` drive
    /// with its eight partitions, the two LUKS2 partitions showing the ext4
    /// filesystems they hold (root at /sysroot, home at /var/home), and its
    /// health.
    #[test]
    fn disks_lists_the_laptops_disks_and_partitions() {
        let topology = yoga();
        let names: Vec<&str> = topology.drives.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["nvme0n1"], "the drive; zram is memory, not a drive");

        let nvme = &topology.drives[0];
        assert_eq!(nvme.kind, DriveKind::Nvme);
        assert_eq!(nvme.model, "SAMSUNG MZAL81T0HDLB-00BL2");
        assert_eq!(nvme.size, 1_024_209_543_168);
        let partitions: Vec<&str> = nvme.partitions.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            partitions,
            [
                "nvme0n1p1",
                "nvme0n1p2",
                "nvme0n1p3",
                "nvme0n1p4",
                "nvme0n1p5",
                "nvme0n1p6",
                "nvme0n1p7",
                "nvme0n1p8"
            ],
            "every partition once; an unlocked mapping is not a partition of its own"
        );
        assert!(
            nvme.allocated() <= nvme.size,
            "the partitions fit on the disk: {} of {}",
            nvme.allocated(),
            nvme.size
        );

        let home = &nvme.partitions[3];
        assert_eq!(home.fstype.as_deref(), Some("ext4"));
        assert_eq!(home.label.as_deref(), Some("home"));
        assert_eq!(home.mountpoint.as_deref(), Some("/var/home"));
        assert_eq!(home.role(), PartRole::Home);
        assert!(home.used.is_some_and(|used| used > 0 && used <= home.size));
        let encryption = home.encryption.as_ref().expect("home is encrypted");
        assert_eq!(encryption.scheme, "LUKS2");
        assert_eq!(encryption.unlocked_as.as_deref(), Some("dm-1"));

        let root = &nvme.partitions[6];
        assert_eq!(root.fstype.as_deref(), Some("ext4"));
        assert_eq!(root.mountpoint.as_deref(), Some("/sysroot"));
        assert_eq!(root.role(), PartRole::Root);
        assert_eq!(
            root.encryption.as_ref().map(|e| e.scheme.as_str()),
            Some("LUKS2")
        );

        let esp = &nvme.partitions[4];
        assert_eq!(esp.label.as_deref(), Some("TILO-EFI"));
        assert!(esp.encryption.is_none());
        let windows = &nvme.partitions[2];
        assert_eq!(windows.fstype.as_deref(), Some("ntfs"));
        assert_eq!(windows.mountpoint, None);

        let smart = nvme.smart.as_ref().expect("the drive's health");
        assert_eq!(smart.status.0, SmartStatus::Passed);
        assert_eq!(smart.temp_c, Some(43));
        assert_eq!(smart.power_on_hours, Some(2294));
        assert_eq!(smart.power_cycles, Some(376));
        assert_eq!(smart.pct_used, Some(5));
        assert_eq!(smart.written_gb, Some(44_492));
        assert!(smart.detail.is_none());

        let reserved = &nvme.partitions[1];
        assert_eq!(
            reserved.display_label(),
            "Microsoft reserved",
            "a partition with no label is named by its type"
        );
    }

    /// zram is compressed memory: it is not listed among the drives, while a
    /// loop device (a disk image) still is, after the real drives.
    #[test]
    fn zram_is_not_a_drive() {
        let lsblk = r#"{"blockdevices":[
            {"name":"zram0","kname":"zram0","size":8589934592,"type":"disk","fstype":"swap",
             "mountpoint":"[SWAP]"},
            {"name":"loop0","kname":"loop0","size":1073741824,"type":"disk","fstype":"ext4"},
            {"name":"sda","kname":"sda","size":500107862016,"type":"disk","tran":"sata"}
        ]}"#;
        let names: Vec<String> = topology_from(&served(lsblk, &BTreeMap::new()))
            .drives
            .into_iter()
            .map(|drive| drive.name)
            .collect();
        assert_eq!(names, ["sda", "loop0"]);
    }

    /// A locked LUKS partition says it is encrypted and nothing it cannot
    /// know; a disk encrypted whole is one encrypted span.
    #[test]
    fn a_locked_partition_is_encrypted_and_shows_nothing_inside() {
        let lsblk = r#"{"blockdevices":[
            {"name":"sda","kname":"sda","size":500107862016,"type":"disk","rota":false,"tran":"usb","rm":true,
             "children":[{"name":"sda1","kname":"sda1","size":500105249280,"type":"part",
                          "fstype":"crypto_LUKS","fsver":"2","uuid":"0b0e"}]},
            {"name":"sdb","kname":"sdb","size":64000000000,"type":"disk","rota":false,"tran":"usb","rm":true,
             "fstype":"crypto_LUKS","fsver":"1",
             "children":[{"name":"stick","kname":"dm-3","size":63998000000,"type":"crypt",
                          "fstype":"exfat","label":"STICK","mountpoint":"/run/media/u/STICK","fsused":1000}]}
        ]}"#;
        let topology = topology_from(&served(lsblk, &BTreeMap::new()));
        let locked = &topology.drives[0].partitions[0];
        assert_eq!(
            locked.encryption,
            Some(Encryption {
                scheme: "LUKS2".into(),
                unlocked_as: None
            })
        );
        assert_eq!(
            locked.fstype, None,
            "a locked container hides its filesystem"
        );
        assert_eq!(locked.mountpoint, None);
        assert_eq!(locked.used, None);

        let whole = &topology.drives[1];
        assert_eq!(whole.partitions.len(), 1);
        let span = &whole.partitions[0];
        assert_eq!(span.size, whole.size);
        assert_eq!(span.fstype.as_deref(), Some("exfat"));
        assert_eq!(span.mountpoint.as_deref(), Some("/run/media/u/STICK"));
        assert_eq!(
            span.encryption
                .as_ref()
                .and_then(|e| e.unlocked_as.as_deref()),
            Some("dm-3")
        );
        let smart = whole.smart.as_ref().unwrap();
        assert_eq!(smart.status.0, SmartStatus::Unknown);
        assert!(smart.detail.as_deref().is_some_and(|d| d.contains("USB")));
    }

    /// The shapes `lsblk` prints elsewhere still read: pool usage over a
    /// member is dropped, zero-size devices and non-disks are skipped, and
    /// the captured inventory gives the drive list it always gave.
    #[test]
    fn other_machines_storage_reads_as_it_always_did() {
        let pool = r#"{"blockdevices":[{"name":"nvme1n1","size":2000398934016,"type":"disk",
            "rota":false,"tran":"nvme","children":[{"name":"nvme1n1p3","size":1998694907904,
            "type":"part","fstype":"btrfs","label":"pool","mountpoint":"/var/home",
            "fsused":2623247085568,"uuid":"2f0d"}]}]}"#;
        let p = &topology_from(&served(pool, &BTreeMap::new())).drives[0].partitions[0];
        assert_eq!(
            p.used, None,
            "pool-wide usage over the partition size is dropped"
        );

        let empty = r#"{"blockdevices":[{"name":"loop0","size":0,"type":"disk"},
            {"name":"sr0","size":0,"type":"rom"}]}"#;
        assert!(
            topology_from(&served(empty, &BTreeMap::new()))
                .drives
                .is_empty()
        );

        // Real `lsblk` output, and the drive list Disks derived from it before
        // the session described storage. Health is not part of that capture.
        let mut drives = topology_from(&served(
            include_str!("../../../crates/system/block-devices/tests/fixtures/disks.json"),
            &BTreeMap::new(),
        ))
        .drives;
        for drive in &mut drives {
            drive.smart = None;
        }
        // zram is no longer listed: it is memory, not a drive.
        let mut expected: Vec<serde_json::Value> =
            serde_json::from_str(include_str!("../tests/fixtures/lsblk-drives.json")).unwrap();
        expected.retain(|drive| drive["kind"] != "Zram");
        assert_eq!(
            serde_json::to_value(drives).unwrap(),
            serde_json::Value::from(expected)
        );
    }

    /// Health the drive reports becomes the verdict: a critical warning
    /// fails it, low spare needs attention, and a drive that reports nothing
    /// is unknown with the reason.
    #[test]
    fn a_drives_own_report_decides_its_health() {
        let read = DriveHealth {
            supported: true,
            updated: Some(1),
            ..DriveHealth::default()
        };
        let failing = DriveHealth {
            failing: true,
            warnings: vec!["spare".into()],
            ..read.clone()
        };
        let smart = smart_from(Some(&failing), DriveKind::Nvme);
        assert_eq!(smart.status.0, SmartStatus::Failing);
        assert!(
            smart
                .attrs
                .iter()
                .any(|a| a.value == "Spare capacity is low" && a.severity == AttrSeverity::Crit)
        );

        let low_spare = DriveHealth {
            available_spare: Some(8),
            spare_threshold: Some(10),
            ..read.clone()
        };
        assert_eq!(
            smart_from(Some(&low_spare), DriveKind::Nvme).status.0,
            SmartStatus::Warning
        );

        let unread = DriveHealth {
            updated: None,
            ..read
        };
        let smart = smart_from(Some(&unread), DriveKind::Nvme);
        assert_eq!(smart.status.0, SmartStatus::Unknown);
        assert!(smart.detail.is_some());
        assert_eq!(
            smart_from(None, DriveKind::Ssd).status.0,
            SmartStatus::Unknown
        );
        assert!(
            smart_from(None, DriveKind::Zram)
                .detail
                .is_some_and(|d| d.contains("Virtual"))
        );
    }
}
