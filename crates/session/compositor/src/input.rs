//! Input as the compositor routes it, whatever device produced it.
//!
//! Backends translate their devices (libinput, a host window, a test or a
//! remote seat) into these events; the compositor routes each to the hosted
//! surface or client it belongs to.

use smithay::backend::input::{
    AbsolutePositionEvent, Axis, AxisSource, ButtonState, InputBackend,
    InputEvent as DeviceEvent, KeyState, KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent,
    PointerMotionEvent,
};
use smithay::utils::{Logical, Size};

/// One input event, in the logical layout.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InputEvent {
    /// The pointer moved to an absolute logical position.
    PointerMotion { x: f64, y: f64 },
    /// The pointer moved by a logical offset.
    PointerMotionRelative { dx: f64, dy: f64 },
    /// A pointer button (evdev code, `BTN_LEFT` = `0x110`) changed state.
    PointerButton { button: u32, pressed: bool },
    /// Scrolling, in logical pixels.
    PointerAxis { horizontal: f64, vertical: f64 },
    /// A key (evdev code) changed state.
    Key { keycode: u32, pressed: bool },
}

impl InputEvent {
    /// A device event as the compositor routes it, whichever backend read
    /// it (libinput on a TTY, the host window when nested); absolute
    /// positions are mapped onto `extent`, the logical size they span.
    /// Events the compositor does not route yet give `None`.
    pub(crate) fn from_device<B: InputBackend>(
        event: DeviceEvent<B>,
        extent: Size<i32, Logical>,
    ) -> Option<Self> {
        match event {
            DeviceEvent::Keyboard { event } => Some(Self::Key {
                // xkb keycodes are evdev codes plus 8.
                keycode: event.key_code().raw().saturating_sub(8),
                pressed: event.state() == KeyState::Pressed,
            }),
            DeviceEvent::PointerMotion { event } => {
                let delta = event.delta();
                Some(Self::PointerMotionRelative {
                    dx: delta.x,
                    dy: delta.y,
                })
            }
            DeviceEvent::PointerMotionAbsolute { event } => {
                let at = event.position_transformed(extent);
                Some(Self::PointerMotion { x: at.x, y: at.y })
            }
            DeviceEvent::PointerButton { event } => Some(Self::PointerButton {
                button: event.button_code(),
                pressed: event.state() == ButtonState::Pressed,
            }),
            DeviceEvent::PointerAxis { event } => Some(Self::PointerAxis {
                horizontal: scroll_amount::<B>(&event, Axis::Horizontal),
                vertical: scroll_amount::<B>(&event, Axis::Vertical),
            }),
            _ => None,
        }
    }
}

/// Logical pixels scrolled on `axis`: a wheel notch (120 in v120 units) is
/// [`WHEEL_STEP`] pixels, as clients expect; fingers and continuous devices
/// report pixels.
fn scroll_amount<B: InputBackend>(event: &B::PointerAxisEvent, axis: Axis) -> f64 {
    match event.source() {
        AxisSource::Wheel | AxisSource::WheelTilt => event.amount_v120(axis).map_or_else(
            || event.amount(axis).unwrap_or(0.0),
            |v120| v120 * WHEEL_STEP / 120.0,
        ),
        _ => event.amount(axis).unwrap_or(0.0),
    }
}

/// Logical pixels one wheel notch scrolls.
const WHEEL_STEP: f64 = 15.0;

/// A key as a hosted surface receives it: the compositor's keymap already
/// applied, so hosts need no keyboard state of their own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostKey {
    /// The evdev code.
    pub keycode: u32,
    /// Pressed or released.
    pub pressed: bool,
    /// The keysym with the active modifiers and layout applied.
    pub keysym: u32,
    /// The keysym of the key's first level (no Shift), for shortcuts.
    pub unmodified_keysym: u32,
    /// The character the key types, if it types one.
    pub text: Option<char>,
    /// The modifiers held as the key changed.
    pub modifiers: KeyModifiers,
}

/// Modifier state accompanying a [`HostKey`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools, reason = "one flag per modifier key")]
pub struct KeyModifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub logo: bool,
    pub caps_lock: bool,
}
