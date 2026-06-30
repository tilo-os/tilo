//! Live system probing: turn `lsblk -J` + `smartctl -j` into a
//! [`Topology`]. Every call is best-effort and side-effect free — if a
//! tool is missing or returns nonsense we degrade gracefully (an empty
//! result lets the caller fall back to the curated sample).
//!
//! This module is only exercised in the live binary; `--shots` never
//! calls it, so the screenshot is fully deterministic offline.

use std::process::Command;

use serde_json::Value;

use crate::model::{
    AttrSeverity, Drive, DriveKind, Partition, SmartAttr, SmartInfo, SmartStatus,
    SmartStatusOpt, Topology,
};

/// Probe the machine. Returns `None` when `lsblk` is unavailable or
/// produced no usable disks.
pub fn probe() -> Option<Topology> {
    let out = Command::new("lsblk")
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
        let rota = dev.get("rota").and_then(|v| v.as_bool()).unwrap_or(false);
        let tran = str_field(dev, "tran");
        let removable = dev.get("rm").and_then(|v| v.as_bool()).unwrap_or(false);
        let kind = DriveKind::from_lsblk(rota, tran.as_deref());

        let mut partitions = Vec::new();
        if let Some(children) = dev.get("children").and_then(|v| v.as_array()) {
            collect_parts(children, &mut partitions);
        }

        let smart = smart_for(&name, kind);

        drives.push(Drive {
            name: name.clone(),
            model: str_field(dev, "model").unwrap_or_else(|| name.clone()),
            serial: str_field(dev, "serial"),
            size,
            kind,
            tran,
            removable,
            partitions,
            smart,
        });
    }

    if drives.is_empty() {
        None
    } else {
        Some(Topology { drives })
    }
}

/// Flatten partitions (and nested children, e.g. LUKS/LVM) into a list.
fn collect_parts(children: &[Value], out: &mut Vec<Partition>) {
    for c in children {
        let ty = c.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let size = c.get("size").and_then(as_u64).unwrap_or(0);
        if ty == "part" || ty == "crypt" || ty == "lvm" {
            out.push(Partition {
                name: str_field(c, "name").unwrap_or_default(),
                size,
                fstype: str_field(c, "fstype"),
                label: str_field(c, "label"),
                mountpoint: str_field(c, "mountpoint"),
                used: c.get("fsused").and_then(as_u64),
                uuid: str_field(c, "uuid"),
            });
        }
        if let Some(nested) = c.get("children").and_then(|v| v.as_array()) {
            collect_parts(nested, out);
        }
    }
}

/// Run `smartctl -j -a /dev/<name>` and distil the summary. Returns
/// `None` if smartctl is missing or yields no parseable JSON.
fn smart_for(name: &str, kind: DriveKind) -> Option<SmartInfo> {
    let dev = format!("/dev/{name}");
    let out = Command::new("smartctl")
        .args(["-j", "-a", &dev])
        .output()
        .ok()?;
    // smartctl exits non-zero for warning bitmasks; parse anyway.
    let json: Value = serde_json::from_slice(&out.stdout).ok()?;

    let passed = json
        .pointer("/smart_status/passed")
        .and_then(|v| v.as_bool());
    let temp = json.pointer("/temperature/current").and_then(|v| v.as_i64());
    let poh = json
        .pointer("/power_on_time/hours")
        .and_then(|v| v.as_i64());
    let cycles = json.pointer("/power_cycle_count").and_then(|v| v.as_i64());
    let pct_used = json
        .pointer("/nvme_smart_health_information_log/percentage_used")
        .and_then(|v| v.as_i64());

    let mut attrs = Vec::new();
    let mut reallocated = None;

    // ATA attribute table.
    if let Some(table) = json
        .pointer("/ata_smart_attributes/table")
        .and_then(|v| v.as_array())
    {
        for a in table {
            let id = a.get("id").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
            let aname = a
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .replace('_', " ");
            let raw = a
                .pointer("/raw/string")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .or_else(|| a.pointer("/raw/value").and_then(|v| v.as_i64()).map(|n| n.to_string()))
                .unwrap_or_default();
            let when_failed = a.get("when_failed").and_then(|v| v.as_str()).unwrap_or("-");
            let sev = if when_failed != "-" && !when_failed.is_empty() {
                AttrSeverity::Crit
            } else if (id == 5 || id == 197) && raw.split_whitespace().next().and_then(|s| s.parse::<i64>().ok()).unwrap_or(0) > 0 {
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
        if let Some(spare) = log.get("available_spare").and_then(|v| v.as_i64()) {
            attrs.push(SmartAttr {
                id: 5,
                name: "Available Spare".into(),
                value: format!("{spare} %"),
                severity: if spare < 10 { AttrSeverity::Warn } else { AttrSeverity::Ok },
            });
        }
        if let Some(pu) = pct_used {
            attrs.push(SmartAttr {
                id: 9,
                name: "Percentage Used".into(),
                value: format!("{pu} %"),
                severity: if pu > 90 { AttrSeverity::Warn } else { AttrSeverity::Ok },
            });
        }
        if let Some(me) = log.get("media_errors").and_then(|v| v.as_i64()) {
            attrs.push(SmartAttr {
                id: 1,
                name: "Media Errors".into(),
                value: me.to_string(),
                severity: if me > 0 { AttrSeverity::Warn } else { AttrSeverity::Ok },
            });
        }
    }

    let written_gb = json
        .pointer("/nvme_smart_health_information_log/data_units_written")
        .and_then(|v| v.as_i64())
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
        None => {
            if kind == DriveKind::Usb {
                SmartStatus::Unknown
            } else {
                SmartStatus::Unknown
            }
        }
    };

    if passed.is_none() && attrs.is_empty() && temp.is_none() && poh.is_none() {
        return Some(SmartInfo {
            status: SmartStatusOpt(SmartStatus::Unknown),
            ..Default::default()
        });
    }

    Some(SmartInfo {
        status: SmartStatusOpt(status),
        temp_c: temp,
        power_on_hours: poh,
        power_cycles: cycles,
        reallocated,
        pct_used,
        written_gb,
        attrs,
    })
}

fn str_field(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

/// lsblk emits sizes as JSON numbers with `-b`, but older versions emit
/// strings — accept both.
fn as_u64(v: &Value) -> Option<u64> {
    if let Some(n) = v.as_u64() {
        return Some(n);
    }
    v.as_str().and_then(|s| s.parse::<u64>().ok())
}
