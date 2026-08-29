//! Live system probing: turn `lsblk -J` + `smartctl -j` into a
//! [`Topology`]. Every call is best-effort and side-effect free — if a
//! tool is missing or returns nonsense we degrade gracefully (`None`),
//! and the caller surfaces an honest empty state rather than inventing
//! devices.
//!
//! Parsing is split from execution: [`parse_lsblk`] and [`parse_smart`]
//! are pure functions over `serde_json::Value`, so the enumeration and
//! SMART-distillation logic is unit-tested against captured fixtures with
//! no root, no system bus and no real hardware. `probe()` runs the tools
//! and feeds their JSON through those parsers.

use std::path::Path;
use std::process::Command;

use serde_json::Value;

use crate::model::{
    AttrSeverity, Drive, DriveKind, Partition, SmartAttr, SmartInfo, SmartStatus, SmartStatusOpt,
    Topology,
};

/// Resolve a system tool to an absolute path, preferring the canonical
/// system locations over whatever a user's `PATH` surfaces first. This
/// matters because a Homebrew/Nix/linuxbrew `lsblk` earlier on `PATH` can
/// silently omit `FSTYPE`/`LABEL`/`UUID` (no udev access), which would make
/// every partition read as an unlabelled blob. Falls back to the bare name
/// so a non-standard install still works.
fn tool(name: &str) -> String {
    for dir in ["/usr/bin", "/bin", "/usr/sbin", "/sbin"] {
        let p = format!("{dir}/{name}");
        if Path::new(&p).exists() {
            return p;
        }
    }
    name.to_string()
}

/// Probe the machine. Returns `None` when `lsblk` is unavailable or
/// produced no usable disks — the caller must NOT substitute fabricated
/// devices; an honest empty state is the correct fallback.
pub fn probe() -> Option<Topology> {
    let out = Command::new(tool("lsblk"))
        .args([
            "-J",
            "-b",
            "-o",
            "NAME,MODEL,SERIAL,SIZE,TYPE,ROTA,TRAN,RM,FSTYPE,LABEL,MOUNTPOINT,FSUSED,UUID",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let json: Value = serde_json::from_slice(&out.stdout).ok()?;

    let mut drives = parse_lsblk(&json);
    if drives.is_empty() {
        return None;
    }
    // Attach best-effort SMART for each physical device.
    for d in &mut drives {
        d.smart = smart_for(&d.name, d.kind);
    }
    Some(Topology { drives })
}

/// Pure transform: `lsblk -J -b …` JSON → the drive list (without SMART,
/// which needs a separate privileged tool). Kept separate so enumeration,
/// classification and the usage-sanity rules are testable offline.
pub fn parse_lsblk(json: &Value) -> Vec<Drive> {
    let Some(devs) = json.get("blockdevices").and_then(|v| v.as_array()) else {
        return Vec::new();
    };

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
        let rota = dev
            .get("rota")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let tran = str_field(dev, "tran");
        let removable = dev
            .get("rm")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let kind = DriveKind::from_lsblk(&name, rota, tran.as_deref());

        let mut partitions = Vec::new();
        if let Some(children) = dev.get("children").and_then(|v| v.as_array()) {
            collect_parts(children, &mut partitions);
        }
        // Whole-disk filesystem (no partition table): a device formatted
        // directly — a USB stick as one big exfat volume, a zram swap area.
        // lsblk reports its fstype/mountpoint on the disk row itself, so
        // represent it as a single full-span "partition" instead of showing
        // the whole device as blank/unallocated.
        if partitions.is_empty()
            && let Some(fstype) = str_field(dev, "fstype")
        {
            let used = dev.get("fsused").and_then(as_u64).filter(|&u| u <= size);
            partitions.push(Partition {
                name: name.clone(),
                size,
                fstype: Some(fstype),
                label: str_field(dev, "label"),
                mountpoint: str_field(dev, "mountpoint"),
                used,
                uuid: str_field(dev, "uuid"),
            });
        }

        drives.push(Drive {
            model: str_field(dev, "model").unwrap_or_else(|| name.clone()),
            serial: str_field(dev, "serial"),
            size,
            kind,
            tran,
            // Virtual devices (zram/loop/dm) are never physically removable
            // even if the kernel's `rm` flag is odd; don't offer to eject them.
            removable: removable && !kind.is_virtual(),
            partitions,
            smart: None,
            name,
        });
    }
    // Physical media first, virtual devices (zram/loop/dm) after — so the
    // default selection lands on a real disk, not the swap zram. Stable, so
    // kernel enumeration order is preserved within each group.
    drives.sort_by_key(|d| d.kind.is_virtual());
    drives
}

/// Flatten partitions (and nested children, e.g. LUKS/LVM) into a list.
fn collect_parts(children: &[Value], out: &mut Vec<Partition>) {
    for c in children {
        let ty = c.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let size = c.get("size").and_then(as_u64).unwrap_or(0);
        if ty == "part" || ty == "crypt" || ty == "lvm" {
            // `fsused` can exceed the partition size for a device that is one
            // member of a multi-device filesystem (btrfs pool, LVM, RAID),
            // where usage is a whole-pool figure. That's not a per-partition
            // fraction, so drop it rather than paint a bogus 100%-full bar.
            let used = c
                .get("fsused")
                .and_then(as_u64)
                .filter(|&u| size > 0 && u <= size);
            out.push(Partition {
                name: str_field(c, "name").unwrap_or_default(),
                size,
                fstype: str_field(c, "fstype"),
                label: str_field(c, "label"),
                mountpoint: str_field(c, "mountpoint").filter(|m| m != "null"),
                used,
                uuid: str_field(c, "uuid"),
            });
        }
        if let Some(nested) = c.get("children").and_then(|v| v.as_array()) {
            collect_parts(nested, out);
        }
    }
}

/// Run `smartctl -j -a /dev/<name>` and distil the summary. Returns `None`
/// only if smartctl cannot be spawned at all (not installed); otherwise a
/// [`SmartInfo`] whose `detail` explains any missing telemetry honestly.
fn smart_for(name: &str, kind: DriveKind) -> Option<SmartInfo> {
    // Memory/software devices never carry SMART; don't shell out or imply
    // they might.
    if kind.is_virtual() {
        return Some(SmartInfo {
            status: SmartStatusOpt(SmartStatus::Unknown),
            detail: Some("Virtual devices have no SMART telemetry.".into()),
            ..Default::default()
        });
    }
    let dev = format!("/dev/{name}");
    let out = Command::new(tool("smartctl"))
        .args(["-j", "-a", &dev])
        .output()
        .ok()?;
    // smartctl exits non-zero for warning bitmasks *and* for permission
    // errors, but always emits JSON — parse it regardless of exit code.
    let json: Value = serde_json::from_slice(&out.stdout).ok()?;
    Some(parse_smart(&json, kind))
}

/// Pure transform: `smartctl -j -a` JSON → a [`SmartInfo`] summary. When the
/// payload carries only an error (e.g. "Permission denied" when not root),
/// the result is an honest `Unknown` with a `detail` saying *why*, rather
/// than the misleading "device exposes no SMART".
pub fn parse_smart(json: &Value, kind: DriveKind) -> SmartInfo {
    let passed = json
        .pointer("/smart_status/passed")
        .and_then(serde_json::Value::as_bool);
    let temp = json
        .pointer("/temperature/current")
        .and_then(serde_json::Value::as_i64);
    let poh = json
        .pointer("/power_on_time/hours")
        .and_then(serde_json::Value::as_i64);
    let cycles = json
        .pointer("/power_cycle_count")
        .and_then(serde_json::Value::as_i64);
    let pct_used = json
        .pointer("/nvme_smart_health_information_log/percentage_used")
        .and_then(serde_json::Value::as_i64);

    let mut attrs = Vec::new();
    let mut reallocated = None;

    // ATA attribute table.
    if let Some(table) = json
        .pointer("/ata_smart_attributes/table")
        .and_then(|v| v.as_array())
    {
        for a in table {
            let id = a.get("id").and_then(serde_json::Value::as_u64).unwrap_or(0) as u16;
            let aname = a
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .replace('_', " ");
            let raw = a
                .pointer("/raw/string")
                .and_then(|v| v.as_str())
                .map(std::string::ToString::to_string)
                .or_else(|| {
                    a.pointer("/raw/value")
                        .and_then(serde_json::Value::as_i64)
                        .map(|n| n.to_string())
                })
                .unwrap_or_default();
            let when_failed = a.get("when_failed").and_then(|v| v.as_str()).unwrap_or("-");
            let sev = if when_failed != "-" && !when_failed.is_empty() {
                AttrSeverity::Crit
            } else if (id == 5 || id == 197)
                && raw
                    .split_whitespace()
                    .next()
                    .and_then(|s| s.parse::<i64>().ok())
                    .unwrap_or(0)
                    > 0
            {
                AttrSeverity::Warn
            } else {
                AttrSeverity::Ok
            };
            if id == 5 {
                reallocated = raw
                    .split_whitespace()
                    .next()
                    .and_then(|s| s.parse::<i64>().ok());
            }
            attrs.push(SmartAttr {
                id,
                name: aname,
                value: raw,
                severity: sev,
            });
        }
    }

    // NVMe health log → a few synthetic attribute rows.
    if let Some(log) = json.pointer("/nvme_smart_health_information_log") {
        if let Some(spare) = log
            .get("available_spare")
            .and_then(serde_json::Value::as_i64)
        {
            attrs.push(SmartAttr {
                id: 5,
                name: "Available Spare".into(),
                value: format!("{spare} %"),
                severity: if spare < 10 {
                    AttrSeverity::Warn
                } else {
                    AttrSeverity::Ok
                },
            });
        }
        if let Some(pu) = pct_used {
            attrs.push(SmartAttr {
                id: 9,
                name: "Percentage Used".into(),
                value: format!("{pu} %"),
                severity: if pu > 90 {
                    AttrSeverity::Warn
                } else {
                    AttrSeverity::Ok
                },
            });
        }
        if let Some(me) = log.get("media_errors").and_then(serde_json::Value::as_i64) {
            attrs.push(SmartAttr {
                id: 1,
                name: "Media Errors".into(),
                value: me.to_string(),
                severity: if me > 0 {
                    AttrSeverity::Warn
                } else {
                    AttrSeverity::Ok
                },
            });
        }
    }

    let written_gb = json
        .pointer("/nvme_smart_health_information_log/data_units_written")
        .and_then(serde_json::Value::as_i64)
        // NVMe data units are 512000-byte blocks → GB.
        .map(|u| u * 512_000 / 1_000_000_000);

    let status = match passed {
        Some(true) => {
            if attrs.iter().any(|a| a.severity != AttrSeverity::Ok) {
                SmartStatus::Warning
            } else {
                SmartStatus::Passed
            }
        }
        Some(false) => SmartStatus::Failing,
        None => SmartStatus::Unknown,
    };

    // No usable telemetry at all → explain why, distinguishing a hard access
    // error (needs privilege / unsupported bridge) from a device that simply
    // reports nothing.
    if passed.is_none() && attrs.is_empty() && temp.is_none() && poh.is_none() {
        let detail = Some(smart_unavailable_reason(json, kind));
        return SmartInfo {
            status: SmartStatusOpt(SmartStatus::Unknown),
            detail,
            ..Default::default()
        };
    }

    SmartInfo {
        status: SmartStatusOpt(status),
        temp_c: temp,
        power_on_hours: poh,
        power_cycles: cycles,
        reallocated,
        pct_used,
        written_gb,
        attrs,
        detail: None,
    }
}

/// Read smartctl's own diagnostic messages to explain why no data came back.
fn smart_unavailable_reason(json: &Value, kind: DriveKind) -> String {
    let msgs = json
        .pointer("/smartctl/messages")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m.get("string").and_then(|s| s.as_str()))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default()
        .to_ascii_lowercase();

    if msgs.contains("permission denied") || msgs.contains("requires") || msgs.contains("root") {
        "SMART needs administrator access — on qOS it is read by the system service.".into()
    } else if msgs.contains("unable to detect device type")
        || msgs.contains("unknown usb bridge")
        || msgs.contains("please specify device type")
    {
        "This bridge does not pass SMART through — try a direct connection.".into()
    } else if kind == DriveKind::Usb {
        "USB mass-storage bridges rarely pass through SMART telemetry.".into()
    } else {
        "This device does not report SMART telemetry.".into()
    }
}

fn str_field(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(std::string::ToString::to_string)
}

/// lsblk emits sizes as JSON numbers with `-b`, but older versions emit
/// strings — accept both.
fn as_u64(v: &Value) -> Option<u64> {
    if let Some(n) = v.as_u64() {
        return Some(n);
    }
    v.as_str().and_then(|s| s.parse::<u64>().ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_lsblk_enumerates_disks_and_partitions() {
        let j = json!({
            "blockdevices": [
                {
                    "name": "nvme0n1", "model": "WD SN560  ", "serial": "23352V ",
                    "size": 1024209543168u64, "type": "disk", "rota": false,
                    "tran": "nvme", "rm": false,
                    "children": [
                        { "name": "nvme0n1p1", "size": 629145600u64, "type": "part",
                          "fstype": "vfat", "label": "EFI", "mountpoint": "/boot/efi",
                          "fsused": 13754368u64, "uuid": "BDDD-7686" }
                    ]
                }
            ]
        });
        let drives = parse_lsblk(&j);
        assert_eq!(drives.len(), 1);
        let d = &drives[0];
        assert_eq!(d.name, "nvme0n1");
        assert_eq!(d.kind, DriveKind::Nvme);
        // Model/serial are trimmed of lsblk's trailing padding.
        assert_eq!(d.model, "WD SN560");
        assert_eq!(d.serial.as_deref(), Some("23352V"));
        assert_eq!(d.partitions.len(), 1);
        assert_eq!(d.partitions[0].fstype.as_deref(), Some("vfat"));
        assert_eq!(d.partitions[0].used, Some(13754368));
    }

    #[test]
    fn parse_lsblk_drops_pool_usage_exceeding_partition_size() {
        // A multi-device btrfs member: fsused (whole-pool) > this partition.
        let j = json!({
            "blockdevices": [{
                "name": "nvme1n1", "size": 2000398934016u64, "type": "disk",
                "rota": false, "tran": "nvme", "rm": false,
                "children": [{
                    "name": "nvme1n1p3", "size": 1998694907904u64, "type": "part",
                    "fstype": "btrfs", "label": "pool", "mountpoint": "/var/home",
                    "fsused": 2623247085568u64, "uuid": "2f0d"
                }]
            }]
        });
        let d = &parse_lsblk(&j)[0];
        let p = &d.partitions[0];
        assert_eq!(p.fstype.as_deref(), Some("btrfs"));
        assert_eq!(
            p.used, None,
            "pool-wide usage over the partition size is dropped, not clamped to 100%"
        );
        assert_eq!(p.used_frac(), None);
    }

    #[test]
    fn parse_lsblk_classifies_zram_as_virtual_not_ssd() {
        let j = json!({
            "blockdevices": [{
                "name": "zram0", "model": null, "serial": null,
                "size": 16402874368u64, "type": "disk", "rota": false,
                "tran": null, "rm": false, "mountpoint": "[SWAP]"
            }]
        });
        let d = &parse_lsblk(&j)[0];
        assert_eq!(d.kind, DriveKind::Zram, "zram must not read as an SSD");
        assert!(!d.removable, "a zram device is never ejectable");
        // No model → fall back to the kernel name rather than a blank header.
        assert_eq!(d.model, "zram0");
    }

    #[test]
    fn parse_lsblk_represents_a_whole_disk_filesystem() {
        // A USB stick formatted directly (no partition table): lsblk carries
        // the fstype/mount on the disk row. It must show as a real filesystem
        // spanning the device, not as a blank/unallocated disk.
        let j = json!({
            "blockdevices": [{
                "name": "sdc", "model": "SanDisk Ultra", "size": 30752000000u64,
                "type": "disk", "rota": false, "tran": "usb", "rm": true,
                "fstype": "exfat", "label": "ULTRA", "mountpoint": "/run/media/u/ULTRA",
                "fsused": 8240000000u64, "uuid": "64ED-1A2B"
            }]
        });
        let d = &parse_lsblk(&j)[0];
        assert_eq!(d.kind, DriveKind::Usb);
        assert_eq!(
            d.partitions.len(),
            1,
            "the whole-disk filesystem is one span"
        );
        let p = &d.partitions[0];
        assert_eq!(p.fstype.as_deref(), Some("exfat"));
        assert_eq!(p.mountpoint.as_deref(), Some("/run/media/u/ULTRA"));
        assert_eq!(p.size, d.size, "the span covers the whole device");
        assert_eq!(d.free_tail(), 0, "no unallocated tail for a whole-disk fs");
    }

    #[test]
    fn parse_lsblk_skips_zero_size_and_non_disks() {
        let j = json!({
            "blockdevices": [
                { "name": "loop0", "size": 0u64, "type": "disk" },
                { "name": "sr0", "size": 0u64, "type": "rom" }
            ]
        });
        assert!(parse_lsblk(&j).is_empty());
    }

    #[test]
    fn parse_smart_reports_permission_error_honestly() {
        // Exactly what non-root smartctl emits: JSON with only an error msg.
        let j = json!({
            "smartctl": {
                "exit_status": 2,
                "messages": [{ "string": "Smartctl open device: /dev/nvme1n1 failed: Permission denied", "severity": "error" }]
            }
        });
        let s = parse_smart(&j, DriveKind::Nvme);
        assert_eq!(s.status.0, SmartStatus::Unknown);
        let detail = s.detail.expect("permission failure carries an explanation");
        assert!(
            detail.contains("administrator access"),
            "explains it is a privilege issue, not absent SMART: {detail}"
        );
    }

    #[test]
    fn parse_smart_distils_a_passing_nvme_log() {
        let j = json!({
            "smart_status": { "passed": true },
            "temperature": { "current": 41 },
            "power_on_time": { "hours": 2196 },
            "power_cycle_count": 412,
            "nvme_smart_health_information_log": {
                "percentage_used": 3,
                "available_spare": 100,
                "media_errors": 0,
                "data_units_written": 94000000u64
            }
        });
        let s = parse_smart(&j, DriveKind::Nvme);
        assert_eq!(s.status.0, SmartStatus::Passed);
        assert_eq!(s.temp_c, Some(41));
        assert_eq!(s.power_on_hours, Some(2196));
        assert_eq!(s.pct_used, Some(3));
        assert!(
            s.detail.is_none(),
            "real data carries no 'unavailable' note"
        );
        assert!(s.attrs.iter().any(|a| a.name == "Percentage Used"));
    }

    #[test]
    fn parse_smart_flags_a_failing_ata_attribute() {
        let j = json!({
            "smart_status": { "passed": true },
            "ata_smart_attributes": { "table": [
                { "id": 5, "name": "Reallocated_Sector_Ct", "when_failed": "-",
                  "raw": { "value": 8, "string": "8" } },
                { "id": 197, "name": "Current_Pending_Sector", "when_failed": "-",
                  "raw": { "value": 2, "string": "2" } }
            ]}
        });
        let s = parse_smart(&j, DriveKind::Hdd);
        // Passing overall, but non-zero reallocations pull it to Warning.
        assert_eq!(s.status.0, SmartStatus::Warning);
        assert_eq!(s.reallocated, Some(8));
        assert!(
            s.attrs
                .iter()
                .any(|a| a.id == 5 && a.severity == AttrSeverity::Warn)
        );
    }

    #[test]
    fn probe_returns_none_or_real_devices_never_fabricates() {
        // On CI/hardware without lsblk this is None; where lsblk exists every
        // returned drive is real (non-empty name, non-zero size). Either way,
        // the parser never invents devices.
        if let Some(topo) = probe() {
            assert!(!topo.drives.is_empty());
            for d in &topo.drives {
                assert!(!d.name.is_empty(), "every probed drive has a kernel name");
                assert!(d.size > 0, "every probed drive has a real size");
            }
        }
    }
}
