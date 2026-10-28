//! Parse the read-only PowerShell observation used on Windows.
//!
//! The parser is platform-neutral so fixtures exercise it on every host. It
//! never invents a component: an empty array stays absent, and a device
//! without a signed-driver row stays without a driver.

use std::collections::BTreeMap;

use serde_json::Value;

use super::{
    RawBattery, RawDevice, RawDisk, RawDisplay, RawGraphics, RawIdentity, RawNetwork, RawReport,
    RawSoundCard,
};

pub(super) fn parse(json: &str) -> Result<RawReport, String> {
    let root: Value = serde_json::from_str(json)
        .map_err(|_| "Windows hardware observation is not JSON".to_string())?;
    let drivers = drivers(&root);
    let mut raw = RawReport {
        identity: identity(&root),
        kernel: text(&root, "osVersion"),
        ..RawReport::default()
    };
    graphics(&root, &drivers, &mut raw.graphics);
    displays(&root, &mut raw.displays);
    network(&root, &drivers, &mut raw.wifi, &mut raw.ethernet);
    devices(&root, &drivers, &mut raw);
    battery(&root, &mut raw);
    firmware(&root, &mut raw);
    disks(&root, &mut raw.disks);
    input(&root, &mut raw.inputs);
    Ok(raw)
}

fn identity(root: &Value) -> RawIdentity {
    let board = text(root, "boardProduct");
    let variant = board
        .clone()
        .or_else(|| text(root, "boardVersion"))
        .or_else(|| text(root, "sku"));
    let firmware = [
        text(root, "biosVendor"),
        text(root, "biosVersion"),
        text(root, "biosDate"),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    RawIdentity {
        manufacturer: text(root, "manufacturer"),
        model: text(root, "model"),
        variant,
        board,
        sku: text(root, "sku"),
        firmware: (!firmware.is_empty()).then(|| firmware.join(" ")),
    }
}

fn graphics(root: &Value, drivers: &BTreeMap<String, String>, out: &mut Vec<RawGraphics>) {
    for (index, value) in array(root, "graphics").iter().enumerate() {
        let Some(name) = text(value, "name") else {
            continue;
        };
        let id = text(value, "deviceId");
        let driver = text(value, "vendor")
            .zip(text(value, "driverVersion"))
            .map(|(vendor, version)| format!("{vendor} {version}"))
            .or_else(|| {
                id.as_deref()
                    .and_then(|id| drivers.get(&id.to_ascii_uppercase()))
                    .cloned()
            });
        let lower = name.to_ascii_lowercase();
        out.push(RawGraphics {
            name,
            driver,
            vendor: text(value, "vendor"),
            device: id,
            primary: index == 0,
            virtual_device: lower.contains("remote")
                || lower.contains("basic display")
                || lower.contains("virtual"),
        });
    }
}

fn displays(root: &Value, out: &mut Vec<RawDisplay>) {
    for (index, value) in array(root, "monitors").iter().enumerate() {
        let Some(name) = text(value, "name") else {
            continue;
        };
        let active = value
            .get("active")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        out.push(RawDisplay {
            name,
            internal: (index == 0).then_some(true),
            connected: true,
            enabled: active,
        });
    }
}

fn network(
    root: &Value,
    drivers: &BTreeMap<String, String>,
    wifi: &mut Vec<RawNetwork>,
    ethernet: &mut Vec<RawNetwork>,
) {
    for value in array(root, "network") {
        let Some(name) = text(value, "name") else {
            continue;
        };
        let kind = text(value, "type").unwrap_or_default().to_ascii_lowercase();
        let lower = name.to_ascii_lowercase();
        let wireless =
            kind.contains("wireless") || kind.contains("802.11") || lower.contains("wi-fi");
        let wired =
            kind.contains("ethernet") || kind.contains("802.3") || lower.contains("ethernet");
        if !wireless && !wired {
            continue;
        }
        let driver =
            text(value, "id").and_then(|id| drivers.get(&id.to_ascii_uppercase()).cloned());
        let adapter = RawNetwork {
            name,
            driver,
            firmware: None,
        };
        if wireless {
            wifi.push(adapter);
        } else {
            ethernet.push(adapter);
        }
    }
}

fn devices(root: &Value, drivers: &BTreeMap<String, String>, raw: &mut RawReport) {
    let mut audio = BTreeMap::<String, RawSoundCard>::new();
    for value in array(root, "devices") {
        let Some(name) = text(value, "name") else {
            continue;
        };
        let class = text(value, "class").unwrap_or_default();
        let lower = name.to_ascii_lowercase();
        let healthy = text(value, "status").map(|status| status.eq_ignore_ascii_case("ok"));
        let driver =
            text(value, "id").and_then(|id| drivers.get(&id.to_ascii_uppercase()).cloned());
        let device = RawDevice {
            name: name.clone(),
            driver,
            vendor: None,
            product: None,
            healthy,
        };
        if class.eq_ignore_ascii_case("bluetooth") || lower.contains("bluetooth") {
            if !raw
                .bluetooth
                .iter()
                .any(|existing| existing.name == device.name)
            {
                raw.bluetooth.push(device.clone());
            }
        } else if class.eq_ignore_ascii_case("camera")
            || lower.contains("camera")
            || lower.contains("webcam")
        {
            if !raw
                .cameras
                .iter()
                .any(|existing| existing.name == device.name)
            {
                raw.cameras.push(device.clone());
            }
        } else if lower.contains("fingerprint") || lower.contains("biometric") {
            if !raw
                .fingerprint
                .iter()
                .any(|existing| existing.name == device.name)
            {
                raw.fingerprint.push(device.clone());
            }
        } else if class.eq_ignore_ascii_case("audioendpoint")
            || lower.contains("speaker")
            || lower.contains("headphone")
            || lower.contains("microphone")
            || lower.contains("headset")
            || lower.contains("audio")
        {
            let playback = usize::from(
                lower.contains("speaker")
                    || lower.contains("headphone")
                    || lower.contains("headset")
                    || lower.contains("audio"),
            );
            let capture = usize::from(
                lower.contains("microphone")
                    || lower.contains("headset")
                    || lower.contains("audio"),
            );
            audio.entry(name.clone()).or_insert_with(|| RawSoundCard {
                name,
                playback,
                capture,
                driver: device.driver,
            });
        }
        if class.eq_ignore_ascii_case("keyboard")
            || class.eq_ignore_ascii_case("mouse")
            || lower.contains("keyboard")
            || lower.contains("touchpad")
            || lower.contains("mouse")
        {
            raw.inputs.push(device.name);
        }
    }
    raw.sound_cards.extend(audio.into_values());
}

fn battery(root: &Value, raw: &mut RawReport) {
    let Some(value) = array(root, "battery").first() else {
        return;
    };
    let status = value
        .get("status")
        .and_then(Value::as_u64)
        .map(battery_status);
    raw.ac_online = matches!(
        status.as_deref(),
        Some(
            "Fully charged"
                | "Charging"
                | "Charging and high"
                | "Charging and low"
                | "Charging and critical"
        )
    )
    .then_some(true);
    raw.battery = Some(RawBattery {
        name: text(value, "name").unwrap_or_else(|| "Battery".into()),
        capacity: value
            .get("capacity")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok()),
        status,
        health: None,
    });
}

fn battery_status(value: u64) -> String {
    match value {
        3 => "Fully charged",
        4 => "Low",
        5 => "Critical",
        6 => "Charging",
        7 => "Charging and high",
        8 => "Charging and low",
        9 => "Charging and critical",
        11 => "Partially charged",
        _ => "Unknown",
    }
    .into()
}

fn firmware(root: &Value, raw: &mut RawReport) {
    raw.uefi = text(root, "firmwareType").map(|kind| kind.eq_ignore_ascii_case("uefi"));
    raw.secure_boot = root.get("secureBoot").and_then(Value::as_bool);
    raw.tpm = array(root, "tpm")
        .first()
        .and_then(|tpm| text(tpm, "spec"))
        .map(|spec| format!("TPM {spec}"));
}

fn disks(root: &Value, out: &mut Vec<RawDisk>) {
    for value in array(root, "disks") {
        let Some(name) = text(value, "name") else {
            continue;
        };
        out.push(RawDisk {
            name,
            model: None,
            size_bytes: value.get("size").and_then(as_u64),
            driver: None,
        });
    }
}

fn input(root: &Value, out: &mut Vec<String>) {
    for value in array(root, "inputs") {
        if let Some(name) = text(value, "name") {
            out.push(name);
        }
    }
}

fn drivers(root: &Value) -> BTreeMap<String, String> {
    let mut drivers = BTreeMap::new();
    for value in array(root, "drivers") {
        let Some(id) = text(value, "deviceId") else {
            continue;
        };
        let provider = text(value, "provider").unwrap_or_default();
        let version = text(value, "version").unwrap_or_default();
        let label = [provider, version]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        if !label.is_empty() {
            drivers.insert(id.to_ascii_uppercase(), label);
        }
    }
    drivers
}

fn array<'a>(root: &'a Value, key: &str) -> &'a [Value] {
    root.get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

fn text(value: &Value, key: &str) -> Option<String> {
    let value = value.get(key)?;
    let text = value.as_str()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

fn as_u64(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str()?.trim().parse().ok())
}
