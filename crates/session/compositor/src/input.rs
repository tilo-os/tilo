//! Input as the compositor routes it, whatever device produced it.
//!
//! Backends translate their devices (libinput, a host window, a test or a
//! remote seat) into these events; the compositor routes each to the hosted
//! surface or client it belongs to.

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
