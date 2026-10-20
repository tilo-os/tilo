//! Read-only Linux observation from sysfs and procfs.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::{
    RawBattery, RawDevice, RawDisk, RawDisplay, RawGraphics, RawIdentity, RawNetwork, RawReport,
    RawSoundCard, SupportReport,
};

pub(super) fn probe() -> SupportReport {
    probe_root(Path::new("/"))
}

pub(super) fn probe_root(root: &Path) -> SupportReport {
    let sys = root.join("sys");
    let proc = root.join("proc");
    let mut raw = RawReport {
        identity: identity(&sys),
        kernel: read_trim(&proc.join("sys/kernel/osrelease")),
        architecture: Some(std::env::consts::ARCH.to_string()),
        ..RawReport::default()
    };
    graphics(&sys, &mut raw.graphics);
    displays(&sys, &mut raw.displays);
    network(&sys, &mut raw.wifi, &mut raw.ethernet);
    bluetooth(&sys, &mut raw.bluetooth);
    sound(&sys, &mut raw.sound_cards);
    cameras(&sys, &mut raw.cameras);
    fingerprint(&sys, &mut raw.fingerprint);
    power(&sys, &mut raw);
    firmware(&sys, &mut raw);
    storage(&sys, &mut raw.disks);
    input(&sys, &mut raw.inputs);
    raw.into_report()
}

fn identity(sys: &Path) -> RawIdentity {
    let dmi = sys.join("class/dmi/id");
    let get = |name: &str| usable(read_trim(&dmi.join(name)));
    let board_name = get("board_name");
    let product_version = get("product_version");
    let sku = get("product_sku");
    let variant = board_name
        .clone()
        .or(product_version)
        .or_else(|| sku.clone());
    let firmware = [get("bios_vendor"), get("bios_version"), get("bios_date")]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    RawIdentity {
        manufacturer: get("sys_vendor"),
        model: get("product_name"),
        variant,
        board: board_name,
        sku,
        firmware: (!firmware.is_empty()).then(|| firmware.join(" ")),
    }
}

fn graphics(sys: &Path, out: &mut Vec<RawGraphics>) {
    for card in entries(&sys.join("class/drm")) {
        let Some(name) = card.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !is_card(name) {
            continue;
        }
        let device = card.join("device");
        let driver = driver_of(&device).or_else(|| uevent(&device, "DRIVER"));
        let vendor = read_trim(&device.join("vendor"));
        let id = read_trim(&device.join("device"));
        let primary = read_trim(&device.join("boot_vga")).as_deref() == Some("1");
        let virtual_device = matches!(driver.as_deref(), Some("vkms" | "simpledrm"));
        out.push(RawGraphics {
            name: graphics_name(vendor.as_deref(), id.as_deref(), driver.as_deref(), name),
            driver,
            vendor,
            device: id,
            primary,
            virtual_device,
        });
    }
    if !out.iter().any(|graphics| graphics.primary)
        && let Some(first) = out.first_mut()
    {
        first.primary = true;
    }
}

fn displays(sys: &Path, out: &mut Vec<RawDisplay>) {
    for connector in entries(&sys.join("class/drm")) {
        let Some(name) = connector.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !name.starts_with("card") || !name.contains('-') || name.contains("Writeback") {
            continue;
        }
        let Some(status) = read_trim(&connector.join("status")) else {
            continue;
        };
        if status != "connected" && status != "disconnected" {
            continue;
        }
        let connected = status == "connected";
        let enabled = connected
            && read_trim(&connector.join("enabled")).is_none_or(|enabled| enabled != "disabled");
        out.push(RawDisplay {
            name: name
                .split_once('-')
                .map_or_else(|| name.to_string(), |(_, connector)| connector.to_string()),
            internal: Some(internal_connector(name)),
            connected,
            enabled,
        });
    }
}

fn network(sys: &Path, wifi: &mut Vec<RawNetwork>, ethernet: &mut Vec<RawNetwork>) {
    for interface in entries(&sys.join("class/net")) {
        let Some(name) = interface.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name == "lo" {
            continue;
        }
        let uevent = interface.join("uevent");
        let devtype = uevent_value(&uevent, "DEVTYPE");
        let wireless = interface.join("wireless").exists()
            || interface.join("phy80211").exists()
            || devtype.as_deref() == Some("wlan");
        if devtype.as_deref() == Some("wwan") {
            continue;
        }
        let device = interface.join("device");
        let adapter = RawNetwork {
            name: name.to_string(),
            driver: driver_of(&device).or_else(|| uevent_value(&uevent, "DRIVER")),
            firmware: read_trim(&device.join("firmware/version"))
                .or_else(|| read_trim(&device.join("version"))),
        };
        if wireless {
            wifi.push(adapter);
        } else {
            ethernet.push(adapter);
        }
    }
}

fn bluetooth(sys: &Path, out: &mut Vec<RawDevice>) {
    for adapter in entries(&sys.join("class/bluetooth")) {
        let Some(name) = adapter.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !name.starts_with("hci") {
            continue;
        }
        let device = adapter.join("device");
        out.push(RawDevice {
            name: format!("Bluetooth adapter {name}"),
            driver: driver_of(&device).or_else(|| uevent(&device, "DRIVER")),
            vendor: read_trim(&device.join("idVendor")),
            product: read_trim(&device.join("idProduct")),
            healthy: None,
        });
    }
}

fn sound(sys: &Path, out: &mut Vec<RawSoundCard>) {
    let mut cards = BTreeMap::<u32, RawSoundCard>::new();
    for entry in entries(&sys.join("class/sound")) {
        let Some(name) = entry.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if let Some(index) = name
            .strip_prefix("card")
            .and_then(|index| index.parse::<u32>().ok())
        {
            let card = cards.entry(index).or_insert_with(|| RawSoundCard {
                name: format!("Sound card {index}"),
                playback: 0,
                capture: 0,
                driver: None,
            });
            if let Some(id) = read_trim(&entry.join("id")) {
                card.name = id;
            }
            card.driver = driver_of(&entry.join("device"))
                .or_else(|| uevent(&entry.join("device"), "DRIVER"));
        } else if let Some((index, direction)) = pcm(&name) {
            let card = cards.entry(index).or_insert_with(|| RawSoundCard {
                name: format!("Sound card {index}"),
                playback: 0,
                capture: 0,
                driver: None,
            });
            match direction {
                'p' => card.playback += 1,
                'c' => card.capture += 1,
                _ => {}
            }
        }
    }
    out.extend(cards.into_values());
}

fn cameras(sys: &Path, out: &mut Vec<RawDevice>) {
    let mut seen = std::collections::BTreeSet::new();
    for camera in entries(&sys.join("class/video4linux")) {
        let Some(fallback) = camera.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let name = read_trim(&camera.join("name")).unwrap_or_else(|| fallback.to_string());
        if !seen.insert(name.clone()) {
            continue;
        }
        let device = camera.join("device");
        out.push(RawDevice {
            name,
            driver: driver_of(&device).or_else(|| uevent(&device, "DRIVER")),
            vendor: read_trim(&device.join("../idVendor")),
            product: read_trim(&device.join("../idProduct")),
            healthy: None,
        });
    }
}

fn fingerprint(sys: &Path, out: &mut Vec<RawDevice>) {
    let mut seen = std::collections::BTreeSet::new();
    for device in entries(&sys.join("bus/usb/devices")) {
        let product = read_trim(&device.join("product"));
        let manufacturer = read_trim(&device.join("manufacturer"));
        let description = format!(
            "{} {}",
            product.as_deref().unwrap_or_default(),
            manufacturer.as_deref().unwrap_or_default()
        )
        .to_ascii_lowercase();
        if !description.contains("fingerprint") && !description.contains("biometric") {
            continue;
        }
        let name = product
            .or(manufacturer)
            .unwrap_or_else(|| "Fingerprint reader".into());
        if !seen.insert(name.clone()) {
            continue;
        }
        out.push(RawDevice {
            name,
            driver: driver_of(&device).or_else(|| uevent(&device, "DRIVER")),
            vendor: read_trim(&device.join("idVendor")),
            product: read_trim(&device.join("idProduct")),
            healthy: None,
        });
    }
    for input in entries(&sys.join("class/input")) {
        let Some(name) = read_trim(&input.join("name")) else {
            continue;
        };
        if !name.to_ascii_lowercase().contains("fingerprint") || !seen.insert(name.clone()) {
            continue;
        }
        out.push(RawDevice {
            name,
            driver: None,
            vendor: None,
            product: None,
            healthy: None,
        });
    }
}

fn power(sys: &Path, raw: &mut RawReport) {
    for supply in entries(&sys.join("class/power_supply")) {
        let Some(kind) = read_trim(&supply.join("type")) else {
            continue;
        };
        match kind.as_str() {
            "Battery" if raw.battery.is_none() => {
                let full = read_u64(&supply.join("energy_full"))
                    .or_else(|| read_u64(&supply.join("charge_full")));
                let design = read_u64(&supply.join("energy_full_design"))
                    .or_else(|| read_u64(&supply.join("charge_full_design")));
                let health = full.zip(design).and_then(|(full, design)| {
                    (design > 0).then(|| (full * 100 / design).min(100) as u32)
                });
                raw.battery = Some(RawBattery {
                    name: read_trim(&supply.join("model_name")).unwrap_or_else(|| "Battery".into()),
                    capacity: read_u32(&supply.join("capacity")),
                    status: read_trim(&supply.join("status")),
                    health,
                });
            }
            "Mains" | "USB" => {
                let online = read_trim(&supply.join("online")).as_deref() == Some("1");
                raw.ac_online = Some(raw.ac_online.unwrap_or(false) || online);
            }
            _ => {}
        }
    }
    raw.suspend_states = read_trim(&sys.join("power/state"))
        .map(|states| states.split_whitespace().map(str::to_string).collect())
        .unwrap_or_default();
}

fn firmware(sys: &Path, raw: &mut RawReport) {
    raw.uefi = Some(sys.join("firmware/efi").is_dir());
    raw.acpi_tables = sys
        .join("firmware/acpi/tables")
        .read_dir()
        .ok()
        .map(|entries| {
            entries
                .flatten()
                .filter(|entry| entry.path().is_file())
                .count()
        });
    raw.secure_boot = secure_boot(sys);
    raw.tpm = entries(&sys.join("class/tpm")).into_iter().find_map(|tpm| {
        let version = read_trim(&tpm.join("tpm_version_major"))?;
        Some(format!("TPM {version}.0"))
    });
}

fn secure_boot(sys: &Path) -> Option<bool> {
    let efivars = sys.join("firmware/efi/efivars");
    let entry = entries(&efivars).into_iter().find(|entry| {
        entry
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("SecureBoot-"))
    })?;
    let bytes = std::fs::read(entry).ok()?;
    bytes.get(4).map(|value| *value == 1)
}

fn storage(sys: &Path, out: &mut Vec<RawDisk>) {
    for disk in entries(&sys.join("class/block")) {
        let Some(name) = disk.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if disk.join("partition").exists()
            || ["loop", "ram", "zram", "dm-", "md", "sr"]
                .iter()
                .any(|prefix| name.starts_with(*prefix))
        {
            continue;
        }
        let device = disk.join("device");
        if !device.exists() {
            continue;
        }
        let driver = driver_of(&device)
            .or_else(|| driver_of(&device.join("device")))
            .or_else(|| driver_of(&device.join("../device")))
            .or_else(|| uevent(&device, "DRIVER"));
        out.push(RawDisk {
            name: name.to_string(),
            model: read_trim(&device.join("model")).or_else(|| read_trim(&device.join("../model"))),
            size_bytes: read_u64(&disk.join("size")).map(|sectors| sectors * 512),
            driver,
        });
    }
}

fn input(sys: &Path, out: &mut Vec<String>) {
    for device in entries(&sys.join("class/input")) {
        let Some(name) = read_trim(&device.join("name")) else {
            continue;
        };
        out.push(name);
    }
}

fn pcm(name: &str) -> Option<(u32, char)> {
    let rest = name.strip_prefix("pcmC")?;
    let (index, rest) = rest.split_once('D')?;
    let index = index.parse().ok()?;
    let direction = rest.chars().last()?;
    Some((index, direction))
}

fn is_card(name: &str) -> bool {
    name.strip_prefix("card")
        .is_some_and(|number| !number.is_empty() && number.chars().all(|c| c.is_ascii_digit()))
}

fn internal_connector(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.contains("edp")
        || lower.contains("lvds")
        || lower.contains("dsi")
        || lower.contains("dpi")
}

fn graphics_name(
    vendor: Option<&str>,
    device: Option<&str>,
    driver: Option<&str>,
    fallback: &str,
) -> String {
    let vendor = vendor.map(|vendor| match vendor {
        "0x10de" => "NVIDIA",
        "0x1002" => "AMD",
        "0x8086" => "Intel",
        "0x1af4" => "virtio",
        "0x1b36" => "QEMU",
        "0x5143" => "Qualcomm",
        "0x13b5" => "ARM",
        _ => "Display",
    });
    match (vendor, device) {
        (Some(vendor), Some(device)) => format!("{vendor} graphics ({device})"),
        (Some(vendor), None) => format!("{vendor} graphics"),
        (None, _) => driver.map_or_else(
            || format!("Display adapter {fallback}"),
            |driver| format!("{driver} display adapter"),
        ),
    }
}

fn driver_of(device: &Path) -> Option<String> {
    let target = std::fs::read_link(device.join("driver")).ok()?;
    target
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_string)
}

fn uevent(device: &Path, key: &str) -> Option<String> {
    uevent_value(&device.join("uevent"), key)
}

fn uevent_value(path: &Path, key: &str) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    text.lines().find_map(|line| {
        let (name, value) = line.split_once('=')?;
        (name == key).then(|| value.trim().to_string())
    })
}

fn usable(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let value = value.trim().to_string();
        let lower = value.to_ascii_lowercase();
        (!value.is_empty()
            && !matches!(
                lower.as_str(),
                "none"
                    | "not specified"
                    | "to be filled by o.e.m."
                    | "system version"
                    | "default string"
            ))
        .then_some(value)
    })
}

fn read_trim(path: &Path) -> Option<String> {
    let value = std::fs::read_to_string(path).ok()?;
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn read_u64(path: &Path) -> Option<u64> {
    read_trim(path)?.parse().ok()
}

fn read_u32(path: &Path) -> Option<u32> {
    read_trim(path)?.parse().ok()
}

fn entries(dir: &Path) -> Vec<PathBuf> {
    let mut entries = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    entries.sort();
    entries
}
