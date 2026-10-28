//! Owner-side streaming + peer-side input-replay coordination.
//!
//! `tilo_callstream`'s `StreamingThrottle` throttles the owner's damage-aware encode loop to
//! 30fps. [`ShareRegistry`] tracks which surfaces are shared with which
//! peer and whether read-only, gating
//! [`neighbour_wire::NeighbourMsg::RemoteInput`] via
//! [`ShareRegistry::accept_remote_input`].
//!
//! `InputEvent` is the wire vocabulary peer→owner input travels in; the
//! owner drops it if read-only, else hands it to a [`RemoteInputSink`].

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

/// Sink the owner's compositor exposes to receive remote-input events;
/// `State::apply_remote_input` in production and `RecordingSink` in tests.
pub trait RemoteInputSink {
    /// Apply a remote-input event to the owner's seat, scoped to `surface` on
    /// behalf of `peer` — implementations own keymap translation and ignoring surfaces they no longer own.
    fn apply(&mut self, peer: PeerId, surface: SurfaceId, event: InputEvent);
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

    /// Stable disclosure rows for trusted chrome and semantic inspection.
    /// Sorted so a refresh changes only when the authority changes, never
    /// because `HashMap` iteration happened to move.
    pub fn disclosure(&self) -> Vec<(PeerId, SurfaceId, bool)> {
        let mut rows = self
            .shares
            .iter()
            .map(|(&(peer, surface), share)| (peer, surface, !share.read_only))
            .collect::<Vec<_>>();
        rows.sort_unstable();
        rows
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

    /// Number of live shares.
    pub fn len(&self) -> usize {
        self.shares.len()
    }

    /// Whether no share is live.
    pub fn is_empty(&self) -> bool {
        self.shares.is_empty()
    }

    /// Mark every share read-only — the registry side of the kill switch. Returns how many shares lost write access.
    pub fn revoke_all_input(&mut self) -> usize {
        let mut revoked: usize = 0;
        for share in self.shares.values_mut() {
            if !share.read_only {
                share.read_only = true;
                revoked = revoked.saturating_add(1);
            }
        }
        revoked
    }

    /// Remove every live share after an emergency stop. This is deliberately
    /// stronger than making rows read-only: a later firewall rebuild must not
    /// resurrect a session the user believed they had terminated.
    pub fn clear(&mut self) {
        self.shares.clear();
    }

    /// Route an inbound `InputEvent` to `sink` if the registry permits it.
    /// **Not the injection path** — only the share gate; not checked against
    /// the surface's rect, occlusion, chords, or rate budget. A seat sink
    /// must go through the compositor state's `review_remote_input` instead.
    pub fn route_to<S: RemoteInputSink>(
        &self,
        peer: PeerId,
        surface: SurfaceId,
        event: InputEvent,
        sink: &mut S,
    ) -> bool {
        if !self.accept_remote_input(peer, surface) {
            return false;
        }
        sink.apply(peer, surface, event);
        true
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    /// Test-only [`RemoteInputSink`] that records every applied event in arrival order.
    #[derive(Default, Debug)]
    pub struct RecordingSink {
        pub events: Vec<(PeerId, SurfaceId, InputEvent)>,
    }

    impl RemoteInputSink for RecordingSink {
        fn apply(&mut self, peer: PeerId, surface: SurfaceId, event: InputEvent) {
            self.events.push((peer, surface, event));
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
    use crate::streaming::test_support::RecordingSink;

    // `StreamingThrottle` unit tests moved into the `tilo_callstream` crate's `throttle` module.

    #[test]
    fn remote_input_authority_is_disclosed_and_enforced() {
        // Phase `disclosure_reports_view_and_control_authority_stably`.
        {
            let mut registry = ShareRegistry::new();
            registry.insert(9, 42, StreamingShare::new());
            registry.insert(3, 77, StreamingShare { read_only: true });

            assert_eq!(registry.disclosure(), vec![(3, 77, false), (9, 42, true)]);

            registry.remove(9, 42);
            assert_eq!(registry.disclosure(), vec![(3, 77, false)]);
        }
        // Phase `remote_input_blocked_when_read_only`.
        // A synthesised `RemoteInput` reaches the seat when read/write; dropped when the owner flips `read_only`.
        {
            let peer: PeerId = 7;
            let surface: SurfaceId = 42;
            let mut registry = ShareRegistry::new();
            registry.insert(peer, surface, StreamingShare::new());
            let mut sink = RecordingSink::default();

            // Read/write: event lands.
            let ev = InputEvent::PointerMotion { x: 10.0, y: 20.0 };
            assert!(registry.route_to(peer, surface, ev.clone(), &mut sink));
            assert_eq!(sink.events.len(), 1);
            assert_eq!(sink.events[0], (peer, surface, ev));

            // Flip to read-only: next event drops.
            registry.set_read_only(peer, surface, true);
            let ev2 = InputEvent::PointerButton {
                button: 272,
                pressed: true,
            };
            assert!(!registry.route_to(peer, surface, ev2, &mut sink));
            assert_eq!(sink.events.len(), 1, "read-only share leaked input");

            // Flip back: events flow again.
            registry.set_read_only(peer, surface, false);
            let ev3 = InputEvent::Key {
                keysym: 0xFF0D,
                pressed: true,
                modifiers: 0,
            };
            assert!(registry.route_to(peer, surface, ev3.clone(), &mut sink));
            assert_eq!(sink.events.len(), 2);
            assert_eq!(sink.events[1].2, ev3);
        }
        // Phase `remote_input_dropped_for_unknown_share`.
        {
            let mut sink = RecordingSink::default();
            let registry = ShareRegistry::new();
            let ev = InputEvent::PointerMotion { x: 1.0, y: 2.0 };
            // Peer never invited; event must drop.
            assert!(!registry.route_to(99, 99, ev, &mut sink));
            assert!(sink.events.is_empty());
        }
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
