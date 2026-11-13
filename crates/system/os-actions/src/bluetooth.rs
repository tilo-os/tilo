//! Bluetooth helpers over the shared `BlueZ` client; new code calls [`crate::bluez`]
//! directly.

use std::time::Duration;

use tilo_bluez::DeviceInfo;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BtDeviceInfo {
    pub name: String,
    /// The handle `bluetoothctl connect <mac>` takes; empty only in hand-built fixtures.
    pub mac: String,
    /// From `bluetoothctl info <mac>`, `0` when the device reports none.
    pub battery: u8,
    pub connected: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NearbyBtDevice {
    pub name: String,
    pub address: String,
    /// Signal during discovery, dBm; `None` for a device `BlueZ` knows but has not seen now.
    pub rssi: Option<i16>,
    pub paired: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BluetoothHandoffTarget {
    Paired,
    Pairable,
}

fn unique_device_by_address<'a>(
    devices: &'a [DeviceInfo],
    address: &str,
) -> Result<Option<&'a DeviceInfo>, String> {
    let mut matches = devices
        .iter()
        .filter(|device| device.address.eq_ignore_ascii_case(address));
    let first = matches.next();
    if matches.next().is_some() {
        return Err(format!(
            "Bluetooth address {address} is ambiguous across local adapters"
        ));
    }
    Ok(first)
}

fn valid_bluetooth_address(address: &str) -> bool {
    address.len() == 17
        && address.split(':').count() == 6
        && address
            .split(':')
            .all(|octet| octet.len() == 2 && octet.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

/// Checks an accessory against this device's own `BlueZ` inventory, discovering once if needed; a
/// peer-provided name or object path is never evidence.
pub fn inspect_bluetooth_handoff_target(
    address: &str,
    discovery_window: Duration,
) -> Result<BluetoothHandoffTarget, String> {
    if !valid_bluetooth_address(address) {
        return Err("invalid Bluetooth address".into());
    }
    let known = crate::bluez::devices()?;
    if unique_device_by_address(&known, address)?.is_some_and(|device| device.paired) {
        return Ok(BluetoothHandoffTarget::Paired);
    }

    let discovered = crate::bluez::scan_for(discovery_window)?;
    let device = unique_device_by_address(&discovered, address)?
        .filter(|device| device.rssi.is_some())
        .ok_or_else(|| format!("Bluetooth device {address} was not discovered locally"))?;
    Ok(if device.paired {
        BluetoothHandoffTarget::Paired
    } else {
        BluetoothHandoffTarget::Pairable
    })
}

/// Pairs, trusts and connects one local object for an approved handoff address; callers hold their own
/// one-shot local authorization.
pub fn pair_bluetooth_handoff(address: &str) -> Result<(), String> {
    if !valid_bluetooth_address(address) {
        return Err("invalid Bluetooth address".into());
    }
    let devices = crate::bluez::devices()?;
    let device = unique_device_by_address(&devices, address)?
        .ok_or_else(|| format!("Bluetooth device {address} is no longer known to BlueZ"))?;
    connected_pair_outcome(crate::bluez::pair(&device.path)?)
}

fn connected_pair_outcome(outcome: tilo_bluez::PairOutcome) -> Result<(), String> {
    match outcome {
        tilo_bluez::PairOutcome::PairedAndConnected => Ok(()),
        tilo_bluez::PairOutcome::PairedNotConnected { reason } => Err(format!(
            "Bluetooth pairing completed but the handoff connection failed: {reason}"
        )),
    }
}

pub fn nearby_bluetooth() -> Result<Vec<NearbyBtDevice>, String> {
    let mut devices: Vec<NearbyBtDevice> = crate::bluez::devices()?
        .into_iter()
        .map(|device| NearbyBtDevice {
            name: device.name,
            address: device.address,
            rssi: device.rssi,
            paired: device.paired,
        })
        .collect();
    devices.sort_by(|a, b| b.rssi.cmp(&a.rssi).then_with(|| a.name.cmp(&b.name)));
    Ok(devices)
}

pub fn scan_bluetooth(on: bool) -> Result<(), String> {
    if on {
        crate::bluez::start_scan()
    } else {
        crate::bluez::stop_scan()
    }
}

pub fn scanning_bluetooth() -> Result<bool, String> {
    crate::bluez::is_scanning()
}

/// Forgets the device at `address`: bond, keys and trust go, so it must be paired again.
pub fn forget_bluetooth(address: &str) -> Result<(), String> {
    if !valid_bluetooth_address(address) {
        return Err("invalid Bluetooth address".into());
    }
    let devices = crate::bluez::devices()?;
    let device = unique_device_by_address(&devices, address)?
        .ok_or_else(|| format!("Bluetooth device {address} is not known to BlueZ"))?;
    crate::bluez::forget(&device.path)
}

pub fn set_bluetooth(enabled: bool) -> Result<(), String> {
    crate::bluez::set_powered(enabled)
}

pub fn bt_connect(mac: &str) -> Result<(), String> {
    let device = crate::bluez::devices()?
        .into_iter()
        .find(|d| d.address.eq_ignore_ascii_case(mac))
        .ok_or_else(|| format!("Bluetooth device {mac} is not known to BlueZ"))?;
    crate::bluez::connect(&device.path)
}

pub fn bt_disconnect(mac: &str) -> Result<(), String> {
    let device = crate::bluez::devices()?
        .into_iter()
        .find(|d| d.address.eq_ignore_ascii_case(mac))
        .ok_or_else(|| format!("Bluetooth device {mac} is not known to BlueZ"))?;
    crate::bluez::disconnect(&device.path)
}

pub fn get_bluetooth() -> Result<bool, String> {
    crate::bluez::is_powered()
}

#[cfg(test)]
fn parse_bluetoothctl_power(stdout: &str) -> Result<bool, String> {
    for line in stdout.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("Powered:") {
            return Ok(rest.trim() == "yes");
        }
    }
    Err("bluetoothctl show: no Powered: line".to_string())
}

/// Two shell-outs: `bluetoothctl devices Paired`, then `info` per MAC for its connected state.
pub fn list_bt_devices() -> Result<Vec<BtDeviceInfo>, String> {
    Ok(crate::bluez::paired_devices()?
        .into_iter()
        .map(|d| BtDeviceInfo {
            name: d.name,
            mac: d.address,
            battery: d.battery.unwrap_or(0),
            connected: d.connected,
        })
        .collect())
}

#[cfg(test)]
fn parse_bluetoothctl_devices(stdout: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in stdout.lines() {
        let Some(rest) = line.strip_prefix("Device ") else {
            continue;
        };
        let mut parts = rest.splitn(2, ' ');
        let mac = match parts.next() {
            Some(m) => m.to_string(),
            None => continue,
        };
        let name = parts.next().unwrap_or("").trim().to_string();
        if !mac.is_empty() {
            out.push((mac, name));
        }
    }
    out
}

#[cfg(test)]
fn parse_bluetoothctl_info(stdout: &str) -> (bool, u8) {
    let mut connected = false;
    let mut battery = 0u8;
    for line in stdout.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("Connected:") {
            connected = rest.trim() == "yes";
        } else if let Some(rest) = t.strip_prefix("Battery Percentage:") {
            // Format: `Battery Percentage: 0x50 (80)`; the decimal in parens is the value.
            if let Some(open) = rest.find('(') {
                let after = &rest[open.saturating_add(1)..];
                if let Some(close) = after.find(')')
                    && let Ok(n) = after[..close].trim().parse::<u8>()
                {
                    battery = n;
                }
            }
        }
    }
    (connected, battery)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bluetoothctl_power_devices_and_info_parse_or_fail() {
        {
            let s = "Controller AA:BB\n\tName: Foo\n\tPowered: yes\n\tDiscoverable: no";
            assert!(parse_bluetoothctl_power(s).unwrap());
        }
        {
            let s = "\tPowered: no";
            assert!(!parse_bluetoothctl_power(s).unwrap());
        }
        {
            assert!(parse_bluetoothctl_power("no Powered line").is_err());
        }
        {
            let s = "\
    Device AA:BB:CC:DD:EE:01 AirPods Pro\n\
    Device AA:BB:CC:DD:EE:02 Magic Mouse\n\
    ";
            let v = parse_bluetoothctl_devices(s);
            assert_eq!(v.len(), 2);
            assert_eq!(v[0].0, "AA:BB:CC:DD:EE:01");
            assert_eq!(v[0].1, "AirPods Pro");
            assert_eq!(v[1].1, "Magic Mouse");
        }
        {
            let s = "\
    Device AA:BB:CC:DD:EE:01\n\
    \tName: AirPods Pro\n\
    \tConnected: yes\n\
    \tBattery Percentage: 0x50 (80)\n\
    ";
            let (connected, battery) = parse_bluetoothctl_info(s);
            assert!(connected);
            assert_eq!(battery, 80);
        }
        {
            let s = "\tConnected: no\n";
            let (connected, battery) = parse_bluetoothctl_info(s);
            assert!(!connected);
            assert_eq!(battery, 0);
        }
    }

    #[test]
    fn forgetting_a_device_needs_a_real_address() {
        assert!(forget_bluetooth("not-an-address").is_err());
        assert!(forget_bluetooth("/org/bluez/hci0/dev_AA_BB").is_err());
    }

    fn device(path: &str, address: &str, paired: bool) -> DeviceInfo {
        DeviceInfo {
            path: path.into(),
            address: address.into(),
            paired,
            ..DeviceInfo::default()
        }
    }

    #[test]
    fn handoff_addresses_resolve_uniquely_and_pairing_must_complete() {
        {
            let devices = vec![device(
                "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF",
                "AA:BB:CC:DD:EE:FF",
                false,
            )];
            assert_eq!(
                unique_device_by_address(&devices, "aa:bb:cc:dd:ee:ff")
                    .unwrap()
                    .map(|device| device.path.as_str()),
                Some("/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF")
            );
        }
        {
            let devices = vec![
                device("/org/bluez/hci0/dev_AA", "AA:BB:CC:DD:EE:FF", true),
                device("/org/bluez/hci1/dev_AA", "AA:BB:CC:DD:EE:FF", false),
            ];
            assert!(
                unique_device_by_address(&devices, "AA:BB:CC:DD:EE:FF")
                    .unwrap_err()
                    .contains("ambiguous")
            );
        }
        {
            assert!(valid_bluetooth_address("AA:BB:CC:DD:EE:FF"));
            assert!(valid_bluetooth_address("aa:bb:cc:dd:ee:ff"));
            assert!(!valid_bluetooth_address("/org/bluez/hci0/dev_AA"));
            assert!(!valid_bluetooth_address("AA:BB:CC:DD:EE"));
            assert!(!valid_bluetooth_address("GG:BB:CC:DD:EE:FF"));
        }
        {
            assert_eq!(
                connected_pair_outcome(tilo_bluez::PairOutcome::PairedAndConnected),
                Ok(())
            );
            assert!(
                connected_pair_outcome(tilo_bluez::PairOutcome::PairedNotConnected {
                    reason: "accessory stayed busy".into(),
                })
                .unwrap_err()
                .contains("accessory stayed busy")
            );
        }
    }
}
