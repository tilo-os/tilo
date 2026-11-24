//! The input devices attached to this machine, as a view reads them: its keyboards, pointers,
//! touchpads and drawing tablets with their pads, each by the identity its settings are kept
//! under.

use serde::{Deserialize, Serialize};

/// What kind of device one is.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum InputKind {
    #[default]
    Other,
    Keyboard,
    Mouse,
    Touchpad,
    Touchscreen,
    Tablet,
    /// The buttons, rings and strips beside a drawing tablet's surface.
    TabletPad,
}

/// One input device attached now.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub struct InputDevice {
    /// The identity its settings are kept under, as `input-device` names it.
    pub key: String,
    /// What the device calls itself: `Wacom Intuos Pro M Pen`.
    pub name: String,
    pub kind: InputKind,
    /// How many buttons a tablet's pad has.
    pub pad_buttons: i64,
    /// A tablet pad's touch rings, in the order its driver lists them: how many modes each one's
    /// mode group has.
    pub pad_rings: Vec<i64>,
    /// A tablet pad's strips, like [`Self::pad_rings`].
    pub pad_strips: Vec<i64>,
    /// The pad's mode buttons, by number.
    pub pad_mode_buttons: Vec<i64>,
    /// The physical device this one is part of: a drawing tablet's pen and its pad share it, so
    /// they are one device to the person. Empty when the machine does not say.
    pub physical_device: String,
}

/// The input devices attached now, and what each one is.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub struct InputDevices {
    /// Every device attached, in the order the machine lists them.
    pub devices: Vec<InputDevice>,
    /// The keyboard last typed on, when a view shows which layout that is.
    pub active_keyboard: Option<String>,
}

/// The input devices as a view of another language reads them.
#[must_use]
pub fn surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<InputDevices, tilo_resource::NoEffect>(
        "input-devices",
        "Input devices",
    )
}
