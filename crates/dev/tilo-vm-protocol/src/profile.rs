//! Device profiles: input devices described by what real hardware reports, so a
//! uinput copy of it gets the same hwdb, udev and libinput treatment.
//!
//! The default devices of a headful slot are `BUS_VIRTUAL`, vendor `0x7469`, named
//! `tilo-vm keyboard (seat)`: libinput and udev's `70-touchpad`/hwdb/quirk rules,
//! which match on bus, vendor, product and name, never see a laptop. A profile
//! carries the identity (bus, vendor, product, version, name) and the capability
//! bitmaps (keys, relative and absolute axes with ranges and resolution, device
//! properties) of one real device, so the guest kernel registers a device that
//! udev tags `ID_INPUT_TOUCHPAD` with the real size, that libinput classifies the
//! same way, and that quirk files keyed on the model apply to.
//!
//! The checked-in profiles are captured from the Lenovo Yoga Slim 7x (83ED) under
//! `os/hardware/profiles/lenovo-83ed/input/`: `/sys/class/input/eventN/device`
//! for ids and bitmaps, the HID report descriptor for axis ranges and units.
//!
//! What a profile cannot reproduce: the kernel driver. hwdb's `KEYBOARD_KEY_<scan>`
//! remaps are applied through the driver's keymap (`EVIOCSKEYCODE`), which a uinput
//! device does not have, so a profile's keys arrive as the keycodes sent.

use serde::{Deserialize, Serialize};

/// What a profile's device is, which decides which actions drive it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceClass {
    Keyboard,
    Touchpad,
    Mouse,
}

/// One absolute axis: its range and resolution (units per millimetre).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AbsAxis {
    pub code: u16,
    pub min: i32,
    pub max: i32,
    pub fuzz: i32,
    pub flat: i32,
    pub resolution: i32,
}

/// A device as the kernel reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceProfile {
    /// Where the capture came from; not used.
    #[serde(default)]
    pub description: String,
    pub class: DeviceClass,
    pub name: String,
    pub bus: u16,
    pub vendor: u16,
    pub product: u16,
    pub version: u16,
    /// `INPUT_PROP_*` bits.
    pub props: Vec<u16>,
    /// `KEY_*` and `BTN_*` codes the device can send.
    pub key: Vec<u16>,
    pub rel: Vec<u16>,
    pub abs: Vec<AbsAxis>,
    pub msc: Vec<u16>,
    pub led: Vec<u16>,
    /// Whether the device asks for key repeat (`EV_REP`).
    pub rep: bool,
}

/// `BTN_LEFT`, the clickpad's physical press.
const BTN_LEFT: u16 = 0x110;
const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;
const ABS_MT_SLOT: u16 = 0x2f;
const ABS_MT_POSITION_X: u16 = 0x35;
const ABS_MT_POSITION_Y: u16 = 0x36;
const ABS_MT_TRACKING_ID: u16 = 0x39;
const BTN_TOUCH: u16 = 0x14a;
const BTN_TOOL_FINGER: u16 = 0x145;
const BTN_TOOL_DOUBLETAP: u16 = 0x14d;
const BTN_TOOL_TRIPLETAP: u16 = 0x14e;
/// `KEY_A`, the proof a keyboard profile is typeable.
const KEY_A: u16 = 30;

/// Checked-in profiles by the name `tilo-vm input device plug` takes.
const PROFILES: &[(&str, &str)] = &[
    (
        "lenovo-83ed-keyboard",
        include_str!("../../../../os/hardware/profiles/lenovo-83ed/input/keyboard.json"),
    ),
    (
        "lenovo-83ed-touchpad",
        include_str!("../../../../os/hardware/profiles/lenovo-83ed/input/touchpad.json"),
    ),
    (
        "lenovo-83ed-mouse",
        include_str!("../../../../os/hardware/profiles/lenovo-83ed/input/mouse.json"),
    ),
];

/// The names `profile` accepts, for error messages.
pub fn profile_names() -> String {
    PROFILES
        .iter()
        .map(|(name, _)| *name)
        .collect::<Vec<_>>()
        .join(", ")
}

/// The canonical (static) spelling of a profile name, if there is one.
pub fn profile_key(name: &str) -> Option<&'static str> {
    PROFILES
        .iter()
        .find(|(known, _)| *known == name)
        .map(|(known, _)| *known)
}

/// The profile `name` names.
///
/// # Errors
/// An unknown name, or a checked-in file that does not describe a usable device.
pub fn profile(name: &str) -> Result<DeviceProfile, String> {
    let (_, text) = PROFILES
        .iter()
        .find(|(known, _)| *known == name)
        .ok_or_else(|| format!("'{name}' is no device profile (known: {})", profile_names()))?;
    let profile: DeviceProfile =
        serde_json::from_str(text).map_err(|e| format!("profile {name}: {e}"))?;
    profile
        .check()
        .map_err(|e| format!("profile {name}: {e}"))?;
    Ok(profile)
}

impl DeviceProfile {
    /// The axis `code`, if the device has it.
    pub fn axis(&self, code: u16) -> Option<&AbsAxis> {
        self.abs.iter().find(|axis| axis.code == code)
    }

    /// Whether the device can send key or button `code`.
    pub fn has_key(&self, code: u16) -> bool {
        self.key.contains(&code)
    }

    /// Millimetres to device units on `axis` (rounded, never below one unit for a
    /// non-zero distance).
    pub fn mm_to_units(&self, code: u16, mm: i32) -> i32 {
        let resolution = self.axis(code).map_or(1, |axis| axis.resolution.max(1));
        let units = mm.saturating_mul(resolution);
        if units == 0 && mm != 0 {
            mm.signum()
        } else {
            units
        }
    }

    /// The centre of the touch surface, in device units.
    pub fn centre(&self) -> (i32, i32) {
        let mid = |code| {
            self.axis(code)
                .map_or(0, |axis| axis.min.saturating_add(axis.max) / 2)
        };
        (mid(ABS_MT_POSITION_X), mid(ABS_MT_POSITION_Y))
    }

    /// A profile must be one a uinput device can be made from, and its class's
    /// actions must have what they drive.
    ///
    /// # Errors
    /// What is missing or out of range.
    pub fn check(&self) -> Result<(), String> {
        if self.name.is_empty() || self.name.len() > 79 {
            return Err("the name must be 1 to 79 bytes".into());
        }
        for axis in &self.abs {
            if axis.min > axis.max {
                return Err(format!("axis {} has min above max", axis.code));
            }
        }
        match self.class {
            DeviceClass::Keyboard if !self.has_key(KEY_A) => Err("a keyboard needs KEY_A".into()),
            DeviceClass::Touchpad => {
                for code in [
                    ABS_X,
                    ABS_Y,
                    ABS_MT_SLOT,
                    ABS_MT_POSITION_X,
                    ABS_MT_POSITION_Y,
                    ABS_MT_TRACKING_ID,
                ] {
                    if self.axis(code).is_none() {
                        return Err(format!("a touchpad needs axis {code:#x}"));
                    }
                }
                for code in [
                    BTN_LEFT,
                    BTN_TOUCH,
                    BTN_TOOL_FINGER,
                    BTN_TOOL_DOUBLETAP,
                    BTN_TOOL_TRIPLETAP,
                ] {
                    if !self.has_key(code) {
                        return Err(format!("a touchpad needs button {code:#x}"));
                    }
                }
                Ok(())
            }
            DeviceClass::Keyboard | DeviceClass::Mouse => Ok(()),
        }
    }

    /// Contacts the touch surface tracks at once.
    pub fn slots(&self) -> i32 {
        self.axis(ABS_MT_SLOT).map_or(1, |axis| {
            axis.max.saturating_sub(axis.min).saturating_add(1)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The checked-in captures load, match what the Yoga reports, and are usable.
    #[test]
    fn lenovo_83ed_profiles_match_the_captured_hardware() {
        let keyboard = profile("lenovo-83ed-keyboard").unwrap();
        assert_eq!(keyboard.class, DeviceClass::Keyboard);
        assert_eq!(keyboard.name, "hid-over-i2c 048D:8987");
        // Bus I2C, as `Bus=0018` in /proc/bus/input/devices.
        assert_eq!(
            (keyboard.bus, keyboard.vendor, keyboard.product),
            (0x18, 0x048d, 0x8987)
        );
        assert!(keyboard.rep);
        // The Fn row's and Copilot's keys sit above what the default keyboard advertises.
        assert!(keyboard.key.iter().any(|k| *k > crate::input::KEY_MAX_USED));

        let touchpad = profile("lenovo-83ed-touchpad").unwrap();
        assert_eq!(touchpad.class, DeviceClass::Touchpad);
        assert_eq!(touchpad.name, "hid-over-i2c 0488:1054 Touchpad");
        assert_eq!(
            (touchpad.bus, touchpad.vendor, touchpad.product),
            (0x18, 0x0488, 0x1054)
        );
        // INPUT_PROP_POINTER and INPUT_PROP_BUTTONPAD: a clickpad.
        assert_eq!(touchpad.props, [0, 2]);
        assert_eq!(touchpad.slots(), 5);
        // 130 x 76 mm, as udev's ID_INPUT_WIDTH_MM and ID_INPUT_HEIGHT_MM.
        let x = touchpad.axis(ABS_MT_POSITION_X).unwrap();
        let y = touchpad.axis(ABS_MT_POSITION_Y).unwrap();
        assert_eq!(x.max / x.resolution, 130);
        assert_eq!(y.max / y.resolution, 76);

        let mouse = profile("lenovo-83ed-mouse").unwrap();
        assert_eq!(mouse.class, DeviceClass::Mouse);
        assert_eq!(mouse.product, touchpad.product);
    }

    #[test]
    fn unknown_names_list_the_known_ones_and_units_convert() {
        let error = profile("nope").unwrap_err();
        assert!(error.contains("lenovo-83ed-touchpad"), "{error}");
        assert_eq!(profile_key("lenovo-83ed-mouse"), Some("lenovo-83ed-mouse"));
        assert_eq!(profile_key("nope"), None);
        let touchpad = profile("lenovo-83ed-touchpad").unwrap();
        assert_eq!(touchpad.mm_to_units(ABS_MT_POSITION_X, 10), 240);
        assert_eq!(touchpad.mm_to_units(ABS_MT_POSITION_X, -10), -240);
        assert_eq!(touchpad.mm_to_units(ABS_MT_POSITION_X, 0), 0);
        let (cx, cy) = touchpad.centre();
        assert_eq!((cx, cy), (1570, 921));
    }

    #[test]
    fn a_profile_that_cannot_be_driven_is_refused() {
        let mut touchpad = profile("lenovo-83ed-touchpad").unwrap();
        touchpad.abs.retain(|axis| axis.code != ABS_MT_SLOT);
        assert!(touchpad.check().unwrap_err().contains("0x2f"));
        let mut keyboard = profile("lenovo-83ed-keyboard").unwrap();
        keyboard.key.retain(|k| *k != KEY_A);
        assert!(keyboard.check().is_err());
        let mut skewed = profile("lenovo-83ed-touchpad").unwrap();
        skewed.abs[0].min = skewed.abs[0].max + 1;
        assert!(skewed.check().unwrap_err().contains("min above max"));
    }
}
