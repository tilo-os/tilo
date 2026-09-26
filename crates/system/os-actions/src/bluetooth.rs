//! Compatibility-shaped Bluetooth helpers over the shared `BlueZ` D-Bus client.
//! New code should prefer [`crate::bluez`] and object paths directly.

use std::time::Duration;

use tilo_bluez::DeviceInfo;

/// One Bluetooth device entry returned by [`list_bt_devices`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BtDeviceInfo {
    pub name: String,
    /// The device's MAC address — the handle `bluetoothctl connect <mac>`
    /// takes. Empty only for hand-built fixtures that don't need it.
    pub mac: String,
    /// Reported via `bluetoothctl info <mac>` (Battery Percentage), or
    /// `0` if the device doesn't expose one.
    pub battery: u8,
    pub connected: bool,
}

/// Local evidence available for a cross-device accessory handoff.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BluetoothHandoffTarget {
    /// This adapter already owns a bond and may connect without creating trust.
    Paired,
    /// The exact address was discovered locally but still requires local user
    /// authorization before pairing can create a bond.
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

/// Verify an accessory against this device's own `BlueZ` inventory, performing
/// one bounded discovery when it is not already known.
///
/// A peer-provided name or object path is never accepted as evidence. Address
/// ambiguity across adapters fails closed because `BlueZ` object paths, unlike
/// Bluetooth addresses, are the locally authoritative operation handles.
///
/// # Errors
///
/// Returns an error when `BlueZ` is unavailable, discovery fails, the address
/// is not observed, or more than one local adapter reports the address.
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

/// Resolve an approved handoff address back to one local `BlueZ` object and
/// pair, trust, then connect it using the compositor-owned pairing agent.
///
/// Callers must hold their own one-shot local authorization; this helper does
/// not interpret a remote handoff request as consent.
///
/// # Errors
///
/// Returns an error when the address is absent or ambiguous in the fresh local
/// inventory, or `BlueZ` rejects pairing/trusting.
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

/// Toggle the Bluetooth controller via `bluetoothctl power on|off`.
///
/// # Errors
///
/// Returns an error message if the `BlueZ` D-Bus connection is unavailable or the call fails.
pub fn set_bluetooth(enabled: bool) -> Result<(), String> {
    crate::bluez::set_powered(enabled)
}

/// Connect a paired device by MAC via `bluetoothctl connect <mac>`.
///
/// # Errors
///
/// Returns an error message if `mac` does not match a device known to `BlueZ`, or the connect
/// call fails.
pub fn bt_connect(mac: &str) -> Result<(), String> {
    let device = crate::bluez::devices()?
        .into_iter()
        .find(|d| d.address.eq_ignore_ascii_case(mac))
        .ok_or_else(|| format!("Bluetooth device {mac} is not known to BlueZ"))?;
    crate::bluez::connect(&device.path)
}

/// Disconnect a device by MAC via `bluetoothctl disconnect <mac>`.
///
/// # Errors
///
/// Returns an error message if `mac` does not match a device known to `BlueZ`, or the
/// disconnect call fails.
pub fn bt_disconnect(mac: &str) -> Result<(), String> {
    let device = crate::bluez::devices()?
        .into_iter()
        .find(|d| d.address.eq_ignore_ascii_case(mac))
        .ok_or_else(|| format!("Bluetooth device {mac} is not known to BlueZ"))?;
    crate::bluez::disconnect(&device.path)
}

/// Read the Bluetooth controller state by grepping `bluetoothctl show`
/// for `Powered:`.
///
/// # Errors
///
/// Returns an error message if the `BlueZ` D-Bus connection is unavailable or the call fails.
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

/// List paired Bluetooth devices and their connected state. Two-step
/// shell-out: `bluetoothctl devices Paired` lists name + MAC, then for
/// each MAC we call `bluetoothctl info <mac>` to extract `Connected:`
/// and `Battery Percentage:`.
///
/// # Errors
///
/// Returns an error message if the `BlueZ` D-Bus connection is unavailable or the call fails.
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

/// Parse `bluetoothctl devices Paired` output:
/// `Device AA:BB:CC:DD:EE:FF Some Name`
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

/// Parse the relevant lines of `bluetoothctl info <mac>`. Returns
/// `(connected, battery)`; battery defaults to `0` when no
/// `Battery Percentage:` line is present.
#[cfg(test)]
fn parse_bluetoothctl_info(stdout: &str) -> (bool, u8) {
    let mut connected = false;
    let mut battery = 0u8;
    for line in stdout.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("Connected:") {
            connected = rest.trim() == "yes";
        } else if let Some(rest) = t.strip_prefix("Battery Percentage:") {
            // Format: `Battery Percentage: 0x50 (80)` — the decimal in
            // parens is what we want.
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
        // Phase `parse_bluetoothctl_power_yes`.
        {
            let s = "Controller AA:BB\n\tName: Foo\n\tPowered: yes\n\tDiscoverable: no";
            assert!(parse_bluetoothctl_power(s).unwrap());
        }
        // Phase `parse_bluetoothctl_power_no`.
        {
            let s = "\tPowered: no";
            assert!(!parse_bluetoothctl_power(s).unwrap());
        }
        // Phase `parse_bluetoothctl_power_missing`.
        {
            assert!(parse_bluetoothctl_power("no Powered line").is_err());
        }
        // Phase `parse_bluetoothctl_devices_basic`.
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
        // Phase `parse_bluetoothctl_info_extracts_battery_and_connected`.
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
        // Phase `parse_bluetoothctl_info_no_battery_defaults_zero`.
        {
            let s = "\tConnected: no\n";
            let (connected, battery) = parse_bluetoothctl_info(s);
            assert!(!connected);
            assert_eq!(battery, 0);
        }
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
        // Phase `handoff_address_resolution_is_case_insensitive_and_unique`.
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
        // Phase `handoff_address_resolution_rejects_cross_adapter_ambiguity`.
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
        // Phase `handoff_address_validation_rejects_paths_and_partial_addresses`.
        {
            assert!(valid_bluetooth_address("AA:BB:CC:DD:EE:FF"));
            assert!(valid_bluetooth_address("aa:bb:cc:dd:ee:ff"));
            assert!(!valid_bluetooth_address("/org/bluez/hci0/dev_AA"));
            assert!(!valid_bluetooth_address("AA:BB:CC:DD:EE"));
            assert!(!valid_bluetooth_address("GG:BB:CC:DD:EE:FF"));
        }
        // Phase `handoff_pairing_requires_the_connection_to_complete`.
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
