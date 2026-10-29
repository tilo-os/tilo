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
