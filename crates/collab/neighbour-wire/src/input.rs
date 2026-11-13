//! A viewer's input into another device's window: the events a person's
//! pointer and keys make on the window's picture, which the device hosting the
//! window replays on a seat of its own.

use serde::{Deserialize, Serialize};

use crate::SurfaceId;

/// Wire-format input event a peer publishes to drive the owner's seat:
/// pointer motion/button + a single keysym (richer events are a
/// follow-up), riding inside
/// `NeighbourMsg::RemoteInput::event`.
///
/// `PointerEnter`/`PointerLeave` scope the peer's virtual-seat bookkeeping;
/// keyboard/text-input focus remains on the active legacy tab so ordinary
/// clients see one stable seat. Two peers typing concurrently interleave at
/// event order, while held keys are reference-counted across peers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum InputEvent {
    /// Pointer moved to logical `(x, y)`, surface-local; the compositor's input firewall translates it into the owner's output space.
    PointerMotion { x: f64, y: f64 },
    /// Mouse button transitioned to `pressed`; `button` mirrors smithay's libinput-style codes (e.g. 272 = `BTN_LEFT`).
    PointerButton { button: u32, pressed: bool },
    /// Peer's pointer entered `surface`, emitted once on hover into the `PeerView`.
    PointerEnter { surface: SurfaceId },
    /// Peer's pointer left `surface`. This does not clear keyboard focus:
    /// moving a mouse out of a text field must not destroy its caret or IME.
    PointerLeave { surface: SurfaceId },
    /// Key press/release. The historical `keysym` field carries a Linux
    /// evdev keycode (for example 30 = A); it is translated to XKB space at
    /// the owner. `modifiers` is an untrusted xkb-style diagnostic bitfield;
    /// held modifiers are reconstructed from each peer's key transitions.
    Key {
        keysym: u32,
        pressed: bool,
        #[serde(default)]
        modifiers: u32,
    },
    /// Explicit keyboard-focus transition, driven by clicks rather than hover.
    /// Kept last for compatibility with the existing serde enum ordering.
    KeyboardFocus { surface: SurfaceId, focused: bool },
    /// The wheel or a trackpad scrolled by `(dx, dy)` surface pixels, at
    /// the peer's last pointer position.
    PointerAxis { dx: f64, dy: f64 },
    /// The peer pasted `text`: the session offers it as the clipboard, so
    /// the window's own paste (a key chord that follows) reads it.
    ClipboardText { text: String },
    /// The peer closed its view of the window: the window is asked to
    /// close, as a person's close of its tab does.
    Close,
    /// Text the peer's input method committed (a composed character, a
    /// dictated phrase, an emoji): delivered into the focused field the way
    /// an input method delivers it, not as key presses.
    Text { text: String },
}
