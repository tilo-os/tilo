//! Compatibility-shaped Bluetooth helpers over the shared BlueZ D-Bus client.
//! New code should prefer [`crate::bluez`] and object paths directly.

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

/// Toggle the Bluetooth controller via `bluetoothctl power on|off`.
pub fn set_bluetooth(enabled: bool) -> Result<(), String> {
    crate::bluez::set_powered(enabled)
}

/// Connect a paired device by MAC via `bluetoothctl connect <mac>`.
pub fn bt_connect(mac: &str) -> Result<(), String> {
    let device = crate::bluez::devices()?
        .into_iter()
        .find(|d| d.address.eq_ignore_ascii_case(mac))
        .ok_or_else(|| format!("Bluetooth device {mac} is not known to BlueZ"))?;
    crate::bluez::connect(&device.path)
}

/// Disconnect a device by MAC via `bluetoothctl disconnect <mac>`.
pub fn bt_disconnect(mac: &str) -> Result<(), String> {
    let device = crate::bluez::devices()?
        .into_iter()
        .find(|d| d.address.eq_ignore_ascii_case(mac))
        .ok_or_else(|| format!("Bluetooth device {mac} is not known to BlueZ"))?;
    crate::bluez::disconnect(&device.path)
}

/// Read the Bluetooth controller state by grepping `bluetoothctl show`
/// for `Powered:`.
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
                let after = &rest[open + 1..];
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
    fn parse_bluetoothctl_power_yes() {
        let s = "Controller AA:BB\n\tName: Foo\n\tPowered: yes\n\tDiscoverable: no";
        assert!(parse_bluetoothctl_power(s).unwrap());
    }

    #[test]
    fn parse_bluetoothctl_power_no() {
        let s = "\tPowered: no";
        assert!(!parse_bluetoothctl_power(s).unwrap());
    }

    #[test]
    fn parse_bluetoothctl_power_missing() {
        assert!(parse_bluetoothctl_power("no Powered line").is_err());
    }

    #[test]
    fn parse_bluetoothctl_devices_basic() {
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

    #[test]
    fn parse_bluetoothctl_info_extracts_battery_and_connected() {
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

    #[test]
    fn parse_bluetoothctl_info_no_battery_defaults_zero() {
        let s = "\tConnected: no\n";
        let (connected, battery) = parse_bluetoothctl_info(s);
        assert!(!connected);
        assert_eq!(battery, 0);
    }
}
