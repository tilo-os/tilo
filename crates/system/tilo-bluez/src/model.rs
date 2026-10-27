//! Pure model types for the `BlueZ` layer.
//!
//! Everything here is bus-free so it can be unit-tested without a running `bluetoothd`:
//! object-path ↔ address conversion, the device shape the UI renders, and the pairing
//! question/answer pair the agent exchanges with whoever is drawing the prompt.

use std::collections::HashMap;

use zbus::zvariant::{OwnedValue, Value};

/// A Bluetooth device as `BlueZ` describes it on `org.bluez.Device1`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeviceInfo {
    /// The `BlueZ` object path, e.g. `/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF`. This is the handle
    /// every operation takes — addresses are not unique across adapters, paths are.
    pub path: String,
    /// MAC address, e.g. `AA:BB:CC:DD:EE:FF`.
    pub address: String,
    /// `Alias` if set, else `Name`, else the address. Never empty.
    pub name: String,
    /// Freedesktop icon name `BlueZ` reports (`audio-headset`, `input-mouse`, …). Empty when the
    /// device did not advertise a class.
    pub icon: String,
    pub paired: bool,
    pub trusted: bool,
    pub connected: bool,
    /// `org.bluez.Battery1.Percentage`, or `None` when the device exposes no battery. Distinct
    /// from `Some(0)`, a real flat battery.
    pub battery: Option<u8>,
    /// Received signal strength during discovery, in dBm. `None` for devices known but not
    /// currently seen.
    pub rssi: Option<i16>,
}

impl DeviceInfo {
    /// Devices sort by "most likely to be what the user wants": connected first, then paired,
    /// then by signal strength (strongest first), then by name for a stable order frame to frame.
    pub fn sort_key(&self) -> (u8, i16, String) {
        let rank = if self.connected {
            0
        } else if self.paired {
            1
        } else {
            2
        };
        // Negate so a stronger (less negative) RSSI sorts first; unknown strength sorts last.
        // `i16::MIN` has no positive counterpart, but real RSSI never gets near it — saturating
        // just means such a reading sorts alongside "unknown" instead of panicking.
        let rssi = self.rssi.map_or(i16::MAX, i16::saturating_neg);
        (rank, rssi, self.name.clone())
    }
}

/// Sort a device list in place into the order [`DeviceInfo::sort_key`] describes.
pub fn sort_devices(devices: &mut [DeviceInfo]) {
    devices.sort_by_key(DeviceInfo::sort_key);
}

/// The adapter's own state.
// Four independently-toggleable `BlueZ` properties (Powered, Discovering, Discoverable,
// Pairable), not a state machine with illegal combinations, so a bitflags-style redesign
// wouldn't earn its keep here — and this type is read by field across `shell`, `settings` and
// `os-actions`, so restructuring it ripples well outside this crate.
#[allow(
    clippy::struct_excessive_bools,
    reason = "four independent BlueZ adapter properties, not a state machine; consumers in \
              other crates read these by field"
)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AdapterInfo {
    pub path: String,
    pub address: String,
    pub name: String,
    pub powered: bool,
    pub discovering: bool,
    pub discoverable: bool,
    pub pairable: bool,
}

/// What `BlueZ` is asking the user during pairing. These map one-to-one onto the
/// `org.bluez.Agent1` methods, carrying exactly what the prompt needs and nothing more.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PairingPrompt {
    /// `RequestConfirmation` — both ends display the same 6-digit number and the user confirms
    /// they match. The most common modern flow.
    ConfirmPasskey { passkey: u32 },
    /// `DisplayPasskey` — we show a number for the user to type on the *other* device.
    /// `entered` is how many digits `BlueZ` has seen so far. Informational: nothing to answer.
    DisplayPasskey { passkey: u32, entered: u16 },
    /// `DisplayPinCode` — same, but a legacy alphanumeric PIN.
    DisplayPinCode { pin: String },
    /// `RequestPasskey` — the user reads a number off the other device and types it here.
    RequestPasskey,
    /// `RequestPinCode` — legacy: the user types a PIN, typically `0000`.
    RequestPinCode,
    /// `RequestAuthorization` — just-works pairing with no number to compare. The weakest flow;
    /// the prompt should say so.
    ConfirmPairing,
    /// `AuthorizeService` — an already-paired device wants to use a profile (A2DP, HID, …) it
    /// has not been authorised for.
    AuthorizeService { uuid: String },
}

impl PairingPrompt {
    /// Whether the prompt takes an answer at all. `DisplayPasskey` and `DisplayPinCode` are
    /// one-way: `BlueZ` has already returned, and the dialog just shows the number until pairing
    /// completes or `Cancel` arrives.
    pub fn expects_answer(&self) -> bool {
        !matches!(
            self,
            PairingPrompt::DisplayPasskey { .. } | PairingPrompt::DisplayPinCode { .. }
        )
    }

    /// Whether answering requires typing something rather than pressing yes/no.
    pub fn expects_entry(&self) -> bool {
        matches!(
            self,
            PairingPrompt::RequestPasskey | PairingPrompt::RequestPinCode
        )
    }

    /// A 6-digit passkey rendered the way the Bluetooth spec displays it — zero-padded, since
    /// `12345` and `012345` are different keys and the other device shows the padded form.
    pub fn passkey_text(&self) -> Option<String> {
        match self {
            PairingPrompt::ConfirmPasskey { passkey }
            | PairingPrompt::DisplayPasskey { passkey, .. } => Some(format!("{passkey:06}")),
            PairingPrompt::DisplayPinCode { pin } => Some(pin.clone()),
            _ => None,
        }
    }
}

/// A pairing question addressed to the user, with the device it concerns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairingRequest {
    /// `BlueZ` object path of the device being paired.
    pub device_path: String,
    /// Best-effort display name for the device. Falls back to the address.
    pub device_name: String,
    pub prompt: PairingPrompt,
}

/// The user's answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PairingReply {
    /// Yes / confirmed.
    Accept,
    /// No — `BlueZ` is told the request was rejected, which aborts pairing.
    Reject,
    /// A typed passkey, for [`PairingPrompt::RequestPasskey`].
    Passkey(u32),
    /// A typed PIN, for [`PairingPrompt::RequestPinCode`].
    PinCode(String),
}

/// Whether a reply actually answers the question that was asked. The agent checks this so a
/// UI bug becomes a rejected pairing rather than a panic or nonsense handed to the daemon.
pub fn reply_answers(prompt: &PairingPrompt, reply: &PairingReply) -> bool {
    match prompt {
        PairingPrompt::RequestPasskey => {
            matches!(reply, PairingReply::Passkey(_) | PairingReply::Reject)
        }
        PairingPrompt::RequestPinCode => {
            matches!(reply, PairingReply::PinCode(pin) if !pin.is_empty() && pin.len() <= crate::agent::MAX_PIN_CODE_BYTES)
                || matches!(reply, PairingReply::Reject)
        }
        // Every other prompt is yes/no: an accept or a reject answers it, a typed
        // passkey or PIN does not.
        _ => matches!(reply, PairingReply::Accept | PairingReply::Reject),
    }
}

/// Extract a `DeviceInfo` from the property map `BlueZ` hands out via `ObjectManager` /
/// `PropertiesChanged`. `interfaces` is the `a{sa{sv}}` value for one object path: interface
/// name → property map. Returns `None` when the object is not a device.
pub fn device_from_interfaces<S: std::hash::BuildHasher>(
    path: &str,
    interfaces: &HashMap<String, HashMap<String, OwnedValue, S>, S>,
) -> Option<DeviceInfo> {
    let props = interfaces.get("org.bluez.Device1")?;
    let address = str_prop(props, "Address").unwrap_or_default();
    let name = str_prop(props, "Alias")
        .or_else(|| str_prop(props, "Name"))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            if address.is_empty() {
                address_from_path(path).unwrap_or_else(|| path.to_string())
            } else {
                address.clone()
            }
        });
    let battery = interfaces
        .get("org.bluez.Battery1")
        .and_then(|b| u8_prop(b, "Percentage"));
    Some(DeviceInfo {
        path: path.to_string(),
        address,
        name,
        icon: str_prop(props, "Icon").unwrap_or_default(),
        paired: bool_prop(props, "Paired"),
        trusted: bool_prop(props, "Trusted"),
        connected: bool_prop(props, "Connected"),
        battery,
        rssi: i16_prop(props, "RSSI"),
    })
}

/// Extract an `AdapterInfo` from an `ObjectManager` entry, or `None` when the object is not
/// an adapter.
pub fn adapter_from_interfaces<S: std::hash::BuildHasher>(
    path: &str,
    interfaces: &HashMap<String, HashMap<String, OwnedValue, S>, S>,
) -> Option<AdapterInfo> {
    let props = interfaces.get("org.bluez.Adapter1")?;
    Some(AdapterInfo {
        path: path.to_string(),
        address: str_prop(props, "Address").unwrap_or_default(),
        name: str_prop(props, "Alias")
            .or_else(|| str_prop(props, "Name"))
            .unwrap_or_default(),
        powered: bool_prop(props, "Powered"),
        discovering: bool_prop(props, "Discovering"),
        discoverable: bool_prop(props, "Discoverable"),
        pairable: bool_prop(props, "Pairable"),
    })
}

// `OwnedValue` is not `Clone` in zvariant 4 (it can own a file descriptor), so these read
// through the borrowed `Value` rather than converting; matching explicitly also means an
// unexpected property type is ignored rather than panicking.

fn str_prop<S: std::hash::BuildHasher>(
    props: &HashMap<String, OwnedValue, S>,
    key: &str,
) -> Option<String> {
    match props.get(key).map(|v| &**v) {
        Some(Value::Str(s)) => Some(s.to_string()),
        _ => None,
    }
}

fn bool_prop<S: std::hash::BuildHasher>(props: &HashMap<String, OwnedValue, S>, key: &str) -> bool {
    matches!(props.get(key).map(|v| &**v), Some(Value::Bool(true)))
}

fn u8_prop<S: std::hash::BuildHasher>(
    props: &HashMap<String, OwnedValue, S>,
    key: &str,
) -> Option<u8> {
    match props.get(key).map(|v| &**v) {
        Some(Value::U8(n)) => Some(*n),
        _ => None,
    }
}

fn i16_prop<S: std::hash::BuildHasher>(
    props: &HashMap<String, OwnedValue, S>,
    key: &str,
) -> Option<i16> {
    match props.get(key).map(|v| &**v) {
        Some(Value::I16(n)) => Some(*n),
        _ => None,
    }
}

/// `/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF` → `AA:BB:CC:DD:EE:FF`. Used only for display
/// fallback; operations always carry the path.
pub fn address_from_path(path: &str) -> Option<String> {
    let last = path.rsplit('/').next()?;
    let hex = last.strip_prefix("dev_")?;
    let parts: Vec<&str> = hex.split('_').collect();
    if parts.len() != 6
        || !parts
            .iter()
            .all(|p| p.len() == 2 && p.chars().all(|c| c.is_ascii_hexdigit()))
    {
        return None;
    }
    Some(parts.join(":"))
}

/// `AA:BB:CC:DD:EE:FF` on `/org/bluez/hci0` → the device object path.
pub fn path_for_address(adapter_path: &str, address: &str) -> String {
    format!("{adapter_path}/dev_{}", address.replace([':', '-'], "_"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_addresses_paths_and_passkeys_round_trip_and_render() {
        {
            let path = "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF";
            assert_eq!(
                address_from_path(path).as_deref(),
                Some("AA:BB:CC:DD:EE:FF")
            );
            assert_eq!(
                path_for_address("/org/bluez/hci0", "AA:BB:CC:DD:EE:FF"),
                path
            );
        }
        {
            assert_eq!(address_from_path("/org/bluez/hci0"), None);
            assert_eq!(address_from_path("/org/bluez"), None);
            // A service under a device is not itself a device.
            assert_eq!(
                address_from_path("/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF/service0001"),
                None
            );
            // Non-hex where hex is required.
            assert_eq!(
                address_from_path("/org/bluez/hci0/dev_ZZ_BB_CC_DD_EE_FF"),
                None
            );
        }
        {
            let p = PairingPrompt::ConfirmPasskey { passkey: 12345 };
            assert_eq!(p.passkey_text().as_deref(), Some("012345"));
            let p = PairingPrompt::DisplayPasskey {
                passkey: 0,
                entered: 0,
            };
            assert_eq!(p.passkey_text().as_deref(), Some("000000"));
            let p = PairingPrompt::ConfirmPasskey { passkey: 999_999 };
            assert_eq!(p.passkey_text().as_deref(), Some("999999"));
        }
        {
            assert!(!reply_answers(
                &PairingPrompt::RequestPinCode,
                &PairingReply::PinCode("x".repeat(crate::agent::MAX_PIN_CODE_BYTES + 1)),
            ));
        }
    }

    #[test]
    fn pairing_replies_must_answer_the_prompt_asked() {
        {
            assert!(
                !PairingPrompt::DisplayPasskey {
                    passkey: 1,
                    entered: 0
                }
                .expects_answer()
            );
            assert!(!PairingPrompt::DisplayPinCode { pin: "0000".into() }.expects_answer());
            assert!(PairingPrompt::ConfirmPasskey { passkey: 1 }.expects_answer());
            assert!(PairingPrompt::ConfirmPairing.expects_answer());
            assert!(PairingPrompt::RequestPasskey.expects_answer());
        }
        {
            assert!(PairingPrompt::RequestPasskey.expects_entry());
            assert!(PairingPrompt::RequestPinCode.expects_entry());
            assert!(!PairingPrompt::ConfirmPasskey { passkey: 1 }.expects_entry());
            assert!(!PairingPrompt::AuthorizeService { uuid: "x".into() }.expects_entry());
        }
        {
            // A yes/no answer to "type the passkey" is not an answer.
            assert!(!reply_answers(
                &PairingPrompt::RequestPasskey,
                &PairingReply::Accept
            ));
            assert!(reply_answers(
                &PairingPrompt::RequestPasskey,
                &PairingReply::Passkey(42)
            ));
            assert!(reply_answers(
                &PairingPrompt::RequestPasskey,
                &PairingReply::Reject
            ));
            // And a typed passkey is not an answer to "do these match?".
            assert!(!reply_answers(
                &PairingPrompt::ConfirmPasskey { passkey: 1 },
                &PairingReply::Passkey(1)
            ));
            assert!(reply_answers(
                &PairingPrompt::ConfirmPasskey { passkey: 1 },
                &PairingReply::Accept
            ));
            assert!(reply_answers(
                &PairingPrompt::RequestPinCode,
                &PairingReply::PinCode("0000".into())
            ));
            assert!(!reply_answers(
                &PairingPrompt::RequestPinCode,
                &PairingReply::Passkey(0)
            ));
        }
    }

    #[test]
    fn devices_sort_connected_paired_then_by_known_signal() {
        {
            let mut v = vec![
                DeviceInfo {
                    name: "far".into(),
                    rssi: Some(-90),
                    ..Default::default()
                },
                DeviceInfo {
                    name: "paired".into(),
                    paired: true,
                    ..Default::default()
                },
                DeviceInfo {
                    name: "near".into(),
                    rssi: Some(-40),
                    ..Default::default()
                },
                DeviceInfo {
                    name: "live".into(),
                    connected: true,
                    paired: true,
                    ..Default::default()
                },
            ];
            sort_devices(&mut v);
            let order: Vec<&str> = v.iter().map(|d| d.name.as_str()).collect();
            assert_eq!(order, ["live", "paired", "near", "far"]);
        }
        {
            let mut v = vec![
                DeviceInfo {
                    name: "silent".into(),
                    rssi: None,
                    ..Default::default()
                },
                DeviceInfo {
                    name: "weak".into(),
                    rssi: Some(-99),
                    ..Default::default()
                },
            ];
            sort_devices(&mut v);
            assert_eq!(v[0].name, "weak");
        }
    }
}
