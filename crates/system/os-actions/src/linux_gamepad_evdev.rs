//! The evdev codes a forwarded gamepad travels as: read from a local
//! controller and written to the virtual uinput device, so the two directions
//! cannot disagree.

use crate::{GamepadAxis, GamepadButton};

pub(crate) const EV_SYN: u16 = 0x00;
pub(crate) const EV_KEY: u16 = 0x01;
pub(crate) const EV_ABS: u16 = 0x03;

pub(crate) const ABS_HAT0X: u16 = 0x10;
pub(crate) const ABS_HAT0Y: u16 = 0x11;

/// Every forwarded axis; the hat travels separately on [`ABS_HAT0X`] and
/// [`ABS_HAT0Y`].
pub(crate) const AXES: [GamepadAxis; 6] = [
    GamepadAxis::LeftX,
    GamepadAxis::LeftY,
    GamepadAxis::RightX,
    GamepadAxis::RightY,
    GamepadAxis::LeftTrigger,
    GamepadAxis::RightTrigger,
];

/// Every forwarded button.
pub(crate) const BUTTONS: [GamepadButton; 11] = [
    GamepadButton::South,
    GamepadButton::East,
    GamepadButton::West,
    GamepadButton::North,
    GamepadButton::LeftBumper,
    GamepadButton::RightBumper,
    GamepadButton::LeftStick,
    GamepadButton::RightStick,
    GamepadButton::Start,
    GamepadButton::Select,
    GamepadButton::Guide,
];

/// The absolute-axis code (`ABS_*`) an axis travels as.
pub(crate) const fn axis_code(axis: GamepadAxis) -> u16 {
    match axis {
        GamepadAxis::LeftX => 0x00,        // ABS_X
        GamepadAxis::LeftY => 0x01,        // ABS_Y
        GamepadAxis::LeftTrigger => 0x02,  // ABS_Z
        GamepadAxis::RightX => 0x03,       // ABS_RX
        GamepadAxis::RightY => 0x04,       // ABS_RY
        GamepadAxis::RightTrigger => 0x05, // ABS_RZ
    }
}

/// The key code (`BTN_*`) a button travels as.
pub(crate) const fn button_code(button: GamepadButton) -> u16 {
    match button {
        GamepadButton::South => 0x130,       // BTN_SOUTH
        GamepadButton::East => 0x131,        // BTN_EAST
        GamepadButton::North => 0x133,       // BTN_NORTH
        GamepadButton::West => 0x134,        // BTN_WEST
        GamepadButton::LeftBumper => 0x136,  // BTN_TL
        GamepadButton::RightBumper => 0x137, // BTN_TR
        GamepadButton::Select => 0x13a,      // BTN_SELECT
        GamepadButton::Start => 0x13b,       // BTN_START
        GamepadButton::Guide => 0x13c,       // BTN_MODE
        GamepadButton::LeftStick => 0x13d,   // BTN_THUMBL
        GamepadButton::RightStick => 0x13e,  // BTN_THUMBR
    }
}

/// The forwarded axis an absolute-axis code carries, if any.
pub(crate) fn axis(code: u16) -> Option<GamepadAxis> {
    AXES.into_iter().find(|&axis| axis_code(axis) == code)
}

/// The forwarded button a key code carries, if any.
pub(crate) fn button(code: u16) -> Option<GamepadButton> {
    BUTTONS
        .into_iter()
        .find(|&button| button_code(button) == code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kernel_codes_map_only_to_fixed_schema() {
        // BTN_SOUTH, BTN_MODE, ABS_RZ.
        assert_eq!(button(0x130), Some(GamepadButton::South));
        assert_eq!(button(0x13c), Some(GamepadButton::Guide));
        assert_eq!(axis(0x05), Some(GamepadAxis::RightTrigger));
        assert_eq!(button(30), None, "keyboard A must never escape");
        assert_eq!(button(0xffff), None);
        assert_eq!(axis(ABS_HAT0X), None, "the hat is not an axis");
    }
}
