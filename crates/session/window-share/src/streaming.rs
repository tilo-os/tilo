//! Owner-side streaming + peer-side input-replay coordination.
//!
//! `tilo_callstream`'s `StreamingThrottle` throttles the owner's damage-aware encode loop to
//! 30fps. [`ShareRegistry`] tracks which surfaces are shared with which
//! peer and whether read-only, gating
//! [`neighbour_wire::NeighbourMsg::RemoteInput`] via
//! [`ShareRegistry::accept_remote_input`].
//!
//! `InputEvent` is the wire vocabulary peer→owner input travels in; the
//! owner's compositor asks [`ShareRegistry::accept_remote_input`] before a
//! remote event reaches its seat.

use neighbour_wire::{PeerId, SurfaceId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
    PointerEnter { surface: SurfaceIdWire },
    /// Peer's pointer left `surface`. This does not clear keyboard focus:
    /// moving a mouse out of a text field must not destroy its caret or IME.
    PointerLeave { surface: SurfaceIdWire },
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
    KeyboardFocus {
        surface: SurfaceIdWire,
        focused: bool,
    },
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

/// Wire-side surface id, duplicated to keep `InputEvent` free of a cross-module dependency; equal to [`neighbour_wire::SurfaceId`].
pub type SurfaceIdWire = neighbour_wire::SurfaceId;

/// A single decoded `org.freedesktop.impl.portal.RemoteDesktop` `Notify*` action.
///
/// Unlike [`InputEvent`] (peer-sync, keyed to a `(peer, surface)` share),
/// this is a *local* portal request: an app the compositor hosts asked
/// (via `RemoteDesktop`) to inject input into whatever surface holds seat
/// focus. The portal backend decodes each `Notify*` call into one of
/// these for the portal's `RemoteInputInjector`;
/// the compositor drains them via
/// the compositor state's `apply_remote_desktop_action`.
///
/// Coordinates mirror the `RemoteDesktop` spec: relative motion is a
/// `(dx, dy)` delta, absolute a point; button/keycode use Linux evdev codes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RemoteAction {
    /// Relative pointer motion (`NotifyPointerMotion`).
    PointerMotionRelative { dx: f64, dy: f64 },
    /// Absolute pointer motion (`NotifyPointerMotionAbsolute`); `stream` is the `PipeWire` node id the coordinate is relative to.
    PointerMotionAbsolute { stream: u32, x: f64, y: f64 },
    /// Pointer button (`NotifyPointerButton`); `button` is an evdev code (e.g. `BTN_LEFT` = 272).
    PointerButton { button: i32, pressed: bool },
    /// Smooth scroll (`NotifyPointerAxis`).
    PointerAxis { dx: f64, dy: f64 },
    /// Discrete scroll (`NotifyPointerAxisDiscrete`); `axis` is `0` (vertical) or `1` (horizontal) per spec.
    PointerAxisDiscrete { axis: u32, steps: i32 },
    /// Key press/release by evdev keycode (`NotifyKeyboardKeycode`).
    KeyboardKeycode { keycode: i32, pressed: bool },
    /// Key press/release by keysym (`NotifyKeyboardKeysym`).
    KeyboardKeysym { keysym: i32, pressed: bool },
}

/// Per-share metadata held by the owner; `read_only` defaults `false` — full control until the owner narrows it.
#[derive(Debug, Clone, PartialEq)]
pub struct StreamingShare {
    /// Whether this peer is allowed to inject input; `true` drops every incoming `RemoteInput`.
    pub read_only: bool,
}

impl StreamingShare {
    /// Build a fresh share — read/write by default.
    pub fn new() -> Self {
        Self { read_only: false }
    }
}

impl Default for StreamingShare {
    fn default() -> Self {
        Self::new()
    }
}

/// Owner-side per-share registry, keyed by `(peer, surface)` since a peer
/// may have multiple surfaces shared with them. Lookup is O(1).
#[derive(Debug, Default)]
pub struct ShareRegistry {
    shares: HashMap<(PeerId, SurfaceId), StreamingShare>,
}

impl ShareRegistry {
    /// Construct an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add or replace the share record for `(peer, surface)`.
    pub fn insert(&mut self, peer: PeerId, surface: SurfaceId, share: StreamingShare) {
        self.shares.insert((peer, surface), share);
    }

    /// Drop the share record for `(peer, surface)`.
    pub fn remove(&mut self, peer: PeerId, surface: SurfaceId) {
        self.shares.remove(&(peer, surface));
    }

    /// Borrow the share record, if any.
    pub fn get(&self, peer: PeerId, surface: SurfaceId) -> Option<&StreamingShare> {
        self.shares.get(&(peer, surface))
    }

    /// Snapshot the peers currently registered for one surface.
    pub fn peers_for(&self, surface: SurfaceId) -> Vec<PeerId> {
        self.shares
            .keys()
            .filter_map(|(peer, candidate)| (*candidate == surface).then_some(*peer))
            .collect()
    }

    /// Flip the `read_only` flag on an existing share; no-op if absent — callers needing creation-on-toggle should `insert` first.
    pub fn set_read_only(&mut self, peer: PeerId, surface: SurfaceId, read_only: bool) {
        if let Some(share) = self.shares.get_mut(&(peer, surface)) {
            share.read_only = read_only;
        }
    }

    /// Decide whether an inbound `RemoteInput` clears the *share* gate: a
    /// record exists and isn't marked read-only. Where/how fast it may land is the compositor's input firewall's job.
    pub fn accept_remote_input(&self, peer: PeerId, surface: SurfaceId) -> bool {
        match self.shares.get(&(peer, surface)) {
            Some(share) => !share.read_only,
            None => false,
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "exact streaming transform fixtures are the behavior under test"
)]
mod tests {
    use super::*;

    #[test]
    fn remote_input_is_gated_by_the_owners_grant() {
        let peer: PeerId = 7;
        let surface: SurfaceId = 42;
        let mut registry = ShareRegistry::new();
        registry.insert(peer, surface, StreamingShare::new());
        assert!(registry.accept_remote_input(peer, surface));

        registry.set_read_only(peer, surface, true);
        assert!(
            !registry.accept_remote_input(peer, surface),
            "a read-only share leaked input"
        );

        registry.set_read_only(peer, surface, false);
        assert!(registry.accept_remote_input(peer, surface));

        registry.remove(peer, surface);
        assert!(
            !registry.accept_remote_input(peer, surface),
            "a dropped share leaked input"
        );
    }

    #[test]
    fn input_events_round_trip_through_serde() {
        {
            let events = [
                InputEvent::PointerMotion { x: 1.5, y: -3.25 },
                InputEvent::PointerButton {
                    button: 272,
                    pressed: true,
                },
                InputEvent::Key {
                    keysym: 0xFF1B,
                    pressed: false,
                    modifiers: 4, /* Ctrl */
                },
                InputEvent::PointerEnter { surface: 42 },
                InputEvent::PointerLeave { surface: 42 },
                InputEvent::KeyboardFocus {
                    surface: 42,
                    focused: true,
                },
            ];
            for ev in events {
                let bytes = serde_json::to_vec(&ev).expect("serialize");
                let round: InputEvent = serde_json::from_slice(&bytes).expect("deserialize");
                assert_eq!(round, ev);
            }
        }
    }
}
