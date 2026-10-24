//! Owner-side streaming + peer-side input-replay coordination.
//!
//! [`StreamingThrottle`] throttles the owner's damage-aware encode loop to
//! 30fps. [`ShareRegistry`] tracks which surfaces are shared with which
//! peer and whether read-only, gating
//! [`crate::network::NeighbourMsg::RemoteInput`] via
//! [`ShareRegistry::accept_remote_input`].
//!
//! `InputEvent` is the wire vocabulary peer→owner input travels in; the
//! owner drops it if read-only, else hands it to a [`RemoteInputSink`].

use crate::network::protocol::{PeerId, SurfaceId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Damage-aware encode throttle, living in the standalone
/// [`tilo_callstream`] crate so surface streaming and the chat call app
/// share one copy: `should_encode(now)` gates the 30-fps cap,
/// `mark_encoded(now)` stamps a successful encode.
pub use tilo_callstream::throttle::StreamingThrottle;

/// Wire-format input event a peer publishes to drive the owner's seat:
/// pointer motion/button + a single keysym (richer events are a
/// follow-up), riding inside
/// [`crate::network::NeighbourMsg::RemoteInput::event`].
///
/// `PointerEnter`/`PointerLeave` scope the peer's virtual-seat bookkeeping;
/// keyboard/text-input focus remains on the active legacy tab so ordinary
/// clients see one stable seat. Two peers typing concurrently interleave at
/// event order, while held keys are reference-counted across peers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum InputEvent {
    /// Pointer moved to logical `(x, y)`, surface-local; [`crate::network::input_firewall`] translates it into the owner's output space.
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

/// Wire-side surface id, duplicated to keep `InputEvent` free of a cross-module dependency; equal to [`crate::network::protocol::SurfaceId`].
pub type SurfaceIdWire = crate::network::protocol::SurfaceId;

/// A single decoded `org.freedesktop.impl.portal.RemoteDesktop` `Notify*` action.
///
/// Unlike [`InputEvent`] (peer-sync, keyed to a `(peer, surface)` share),
/// this is a *local* portal request: an app the compositor hosts asked
/// (via `RemoteDesktop`) to inject input into whatever surface holds seat
/// focus. The portal backend decodes each `Notify*` call into one of
/// these for a [`RemoteInputInjector`](crate::portal::RemoteInputInjector);
/// the compositor drains them via
/// [`crate::compositor::state::State::apply_remote_desktop_action`].
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
    /// record exists and isn't marked read-only. Where/how fast it may land is [`crate::network::input_firewall`]'s job.
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
    /// must go through [`crate::compositor::state::State::review_remote_input`] instead.
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
    use crate::network::streaming::test_support::RecordingSink;

    /// Put `surface` on screen, focused, unobstructed, so the firewall's checks have something to answer from.
    #[cfg(target_os = "linux")]
    fn stage_shared_surface(state: &mut crate::compositor::state::State, surface: SurfaceId) {
        use crate::network::input_firewall::ShellView;
        use vello::kurbo::Rect;
        state.remote_input_firewall.publish_view(ShellView::new(
            vec![(surface, Rect::new(0.0, 0.0, 1920.0, 1080.0))],
            Some(surface),
            crate::shell::input_policy::InputCapture::NONE,
            false,
        ));
    }

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

    #[test]
    #[cfg(target_os = "linux")]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn remote_seat_input_reaches_the_focused_client() {
        // Phase `apply_remote_input_pointer_motion_reaches_seat`.
        // `RemoteInputSink::apply` for `State` calls `PointerHandle::motion` for `PointerMotion`; asserts the pointer's location reflects the injected coordinates.
        {
            use crate::compositor::state::State;
            use crate::network::streaming::{InputEvent, StreamingShare};
            use smithay::reexports::calloop::EventLoop;
            use smithay::reexports::wayland_server::Display;

            // Minimal standalone `State` — no listening socket, no clients; same plumbing `native::run` does at startup, minus the winit/drm backend.
            let event_loop: EventLoop<'static, State> =
                EventLoop::try_new().expect("calloop event loop");
            let display: Display<State> = Display::new().expect("wayland display");
            let display_handle = display.handle();
            let mut state = State::new(event_loop.handle(), display_handle);

            // Stage the (peer, surface) share, else `apply_remote_input` drops on the unknown-share path.
            let peer: PeerId = 42;
            let surface: SurfaceId = 7;
            state
                .share_registry
                .insert(peer, surface, StreamingShare::new());
            // The shared surface fills the output from its origin, so a surface-local coordinate matches its output-space translation.
            stage_shared_surface(&mut state, surface);

            // Fresh seat starts at the origin; current_location returns the last-known location.
            let initial = state
                .seat
                .get_pointer()
                .expect("seat has pointer device")
                .current_location();
            assert_eq!(
                initial.x, 0.0,
                "seat fresh start; pointer x should be 0, got {}",
                initial.x
            );
            assert_eq!(initial.y, 0.0);

            let routed = state.apply_remote_input(
                peer,
                surface,
                InputEvent::PointerMotion { x: 123.0, y: 456.0 },
            );
            assert!(routed, "share is read/write, motion must route");

            // Proves the call reached the production `motion()` path, not just stamped `last_remote_input`.
            let after = state
                .seat
                .get_pointer()
                .expect("seat still has pointer")
                .current_location();
            assert_eq!(
                (after.x, after.y),
                (123.0, 456.0),
                "D-1 regression: pointer location did not reach the seat \
                 after `apply_remote_input(PointerMotion)`; observed {:?}",
                (after.x, after.y)
            );

            // Seat is mutating, not pinned at the first value.
            let _ = state.apply_remote_input(
                peer,
                surface,
                InputEvent::PointerMotion { x: 5.0, y: 6.0 },
            );
            let after2 = state.seat.get_pointer().unwrap().current_location();
            assert_eq!((after2.x, after2.y), (5.0, 6.0));
        }
        // Phase `apply_remote_input_key_reaches_keyboard`.
        // `InputEvent::Key` must reach `KeyboardHandle::input` — proven via `forwarded_pressed_keys` after a Key event.
        {
            use crate::compositor::state::State;
            use crate::network::streaming::{InputEvent, StreamingShare};
            use smithay::reexports::calloop::EventLoop;
            use smithay::reexports::wayland_server::Display;

            let event_loop: EventLoop<'static, State> =
                EventLoop::try_new().expect("calloop event loop");
            let display: Display<State> = Display::new().expect("wayland display");
            let display_handle = display.handle();
            let mut state = State::new(event_loop.handle(), display_handle);

            let peer: PeerId = 7;
            let surface: SurfaceId = 42;
            state
                .share_registry
                .insert(peer, surface, StreamingShare::new());
            // The scrubber's focus guard mutes a guest's keystrokes unless the shared surface holds keyboard focus.
            stage_shared_surface(&mut state, surface);
            assert!(state.apply_remote_input(
                peer,
                surface,
                InputEvent::KeyboardFocus {
                    surface,
                    focused: true,
                },
            ));

            // Baseline: no keys pressed.
            {
                let kb = state.seat.get_keyboard().expect("keyboard on seat");
                assert!(
                    kb.pressed_keys().is_empty(),
                    "seat starts with no pressed keys",
                );
            }

            let keysym: u32 = 30; /* evdev A on a US keymap */
            let xkb_keycode = keysym + 8;
            let routed = state.apply_remote_input(
                peer,
                surface,
                InputEvent::Key {
                    keysym,
                    pressed: true,
                    modifiers: 0,
                },
            );
            assert!(routed, "read/write share, key event must route");

            // smithay's `forwarded_pressed_keys` reflects the state after `KeyboardHandle::input` runs.
            {
                let kb = state.seat.get_keyboard().expect("keyboard on seat");
                let pressed = kb.pressed_keys();
                assert!(
                    pressed.iter().any(|k| u32::from(*k) == xkb_keycode),
                    "keyboard did not observe key press; observed {pressed:?}",
                );
            }

            // After `Released`, the pressed set no longer contains the keycode.
            let _ = state.apply_remote_input(
                peer,
                surface,
                InputEvent::Key {
                    keysym,
                    pressed: false,
                    modifiers: 0,
                },
            );
            {
                let kb = state.seat.get_keyboard().expect("keyboard on seat");
                assert!(
                    !kb.pressed_keys()
                        .iter()
                        .any(|k| u32::from(*k) == xkb_keycode),
                    "keyboard still holds the released keycode",
                );
            }

            // `last_remote_input` carries the peer id so a future per-user "typing" overlay can find it in one place.
            let (last_peer, _, _) = state.last_remote_input.as_ref().expect("event stamped");
            assert_eq!(*last_peer, peer, "origin peer tag lost");
        }
        // Phase `pointer_leave_preserves_focus_until_explicit_blur`.
        // Hover and click focus are independent: pointer leave preserves the
        // caret, while an explicit blur releases keyboard focus.
        {
            use crate::compositor::state::State;
            use crate::network::streaming::{InputEvent, StreamingShare};
            use smithay::reexports::calloop::EventLoop;
            use smithay::reexports::wayland_server::Display;

            let event_loop: EventLoop<'static, State> =
                EventLoop::try_new().expect("calloop event loop");
            let display: Display<State> = Display::new().expect("wayland display");
            let display_handle = display.handle();
            let mut state = State::new(event_loop.handle(), display_handle);

            let peer: PeerId = 11;
            let surface: SurfaceId = 314;
            state
                .share_registry
                .insert(peer, surface, StreamingShare::new());
            stage_shared_surface(&mut state, surface);

            // No seat entry yet — the peer hasn't emitted any input.
            assert!(state.remote_seats.seat(peer).is_none());

            // PointerEnter remains a compatibility focus handshake for older
            // viewers; importantly, leaving no longer clears that focus.
            let routed =
                state.apply_remote_input(peer, surface, InputEvent::PointerEnter { surface });
            assert!(routed);
            let seat = state
                .remote_seats
                .seat(peer)
                .expect("peer's remote seat exists after enter");
            assert_eq!(seat.pointer_surface, Some(surface));
            assert_eq!(seat.focused_surface, Some(surface));

            assert!(state.apply_remote_input(
                peer,
                surface,
                InputEvent::KeyboardFocus {
                    surface,
                    focused: true,
                },
            ));

            // Leaving the tile preserves the app caret/focus.
            let _ = state.apply_remote_input(peer, surface, InputEvent::PointerLeave { surface });
            let seat = state
                .remote_seats
                .seat(peer)
                .expect("peer's remote seat still exists");
            assert_eq!(seat.pointer_surface, None);
            assert_eq!(seat.focused_surface, Some(surface));

            assert!(state.apply_remote_input(
                peer,
                surface,
                InputEvent::KeyboardFocus {
                    surface,
                    focused: false,
                },
            ));
            assert_eq!(state.remote_seats.seat(peer).unwrap().focused_surface, None);
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn remote_desktop_input_reaches_the_seat_until_the_kill_switch() {
        // Phase `apply_remote_desktop_absolute_motion_reaches_seat`.
        // A `RemoteDesktop` `NotifyPointerMotionAbsolute` must reach the real smithay seat, mirroring the peer-sync motion test above.
        {
            use crate::compositor::state::State;
            use crate::network::streaming::RemoteAction;
            use smithay::reexports::calloop::EventLoop;
            use smithay::reexports::wayland_server::Display;

            let event_loop: EventLoop<'static, State> =
                EventLoop::try_new().expect("calloop event loop");
            let display: Display<State> = Display::new().expect("wayland display");
            let mut state = State::new(event_loop.handle(), display.handle());

            let applied = state.apply_remote_desktop_action(RemoteAction::PointerMotionAbsolute {
                stream: 1,
                x: 200.0,
                y: 150.0,
            });
            assert!(applied, "absolute motion must reach the seat pointer");
            let loc = state.seat.get_pointer().unwrap().current_location();
            assert_eq!((loc.x, loc.y), (200.0, 150.0));

            // Relative motion accumulates on top of the current location.
            let _ = state.apply_remote_desktop_action(RemoteAction::PointerMotionRelative {
                dx: -50.0,
                dy: 25.0,
            });
            let loc2 = state.seat.get_pointer().unwrap().current_location();
            assert_eq!((loc2.x, loc2.y), (150.0, 175.0));
        }
        // Phase `personal_fabric_motion_goes_to_the_event_loop_but_yields_to_kill_switch`.
        {
            use crate::compositor::state::State;
            use crate::shell::device_fabric::FabricInputEvent;
            use smithay::reexports::calloop::EventLoop;
            use smithay::reexports::wayland_server::Display;

            let event_loop: EventLoop<'static, State> =
                EventLoop::try_new().expect("calloop event loop");
            let display: Display<State> = Display::new().expect("wayland display");
            let mut state = State::new(event_loop.handle(), display.handle());
            let controller =
                crate::shell::personal_continuity::DeviceId::new("controller").unwrap();
            assert!(state.apply_personal_fabric_input(
                &controller,
                FabricInputEvent::PointerMotion { x: 320, y: 180 }
            ));
            assert_eq!(
                state.take_personal_fabric_input(),
                vec![FabricInputEvent::PointerMotion { x: 320, y: 180 }],
                "admitted input is delivered as this device's own"
            );

            state.remote_input_firewall.engage_kill_switch();
            assert!(!state.apply_personal_fabric_input(
                &controller,
                FabricInputEvent::PointerMotion { x: 640, y: 360 }
            ));
            assert!(state.take_personal_fabric_input().is_empty());
        }
        // Phase `apply_remote_desktop_keycode_reaches_keyboard`.
        // A `RemoteDesktop` `NotifyKeyboardKeycode` reaches the seat keyboard, appearing in `pressed_keys()` after press and clearing on release.
        {
            use crate::compositor::state::State;
            use crate::network::streaming::RemoteAction;
            use smithay::reexports::calloop::EventLoop;
            use smithay::reexports::wayland_server::Display;

            let event_loop: EventLoop<'static, State> =
                EventLoop::try_new().expect("calloop event loop");
            let display: Display<State> = Display::new().expect("wayland display");
            let mut state = State::new(event_loop.handle(), display.handle());

            let kc: i32 = 30; // Linux evdev A maps to XKB keycode 38.
            assert!(
                state.apply_remote_desktop_action(RemoteAction::KeyboardKeycode {
                    keycode: kc,
                    pressed: true,
                })
            );
            {
                let kb = state.seat.get_keyboard().expect("keyboard on seat");
                assert!(
                    kb.pressed_keys().iter().any(|k| u32::from(*k) == 38),
                    "evdev A must reach the seat as XKB A",
                );
            }
            let _ = state.apply_remote_desktop_action(RemoteAction::KeyboardKeycode {
                keycode: kc,
                pressed: false,
            });
            let kb = state.seat.get_keyboard().expect("keyboard on seat");
            assert!(
                !kb.pressed_keys().iter().any(|k| u32::from(*k) == 38),
                "release must clear the pressed key",
            );
        }
        // Phase `remote_desktop_channel_injector_drives_the_seat`.
        // End-to-end production path: a [`ChannelInjector`] carries a decoded action into `apply_remote_desktop_action` against the real seat.
        {
            use crate::compositor::state::State;
            use crate::network::streaming::RemoteAction;
            use crate::portal::{ChannelInjector, RemoteInputInjector};
            use smithay::reexports::calloop::EventLoop;
            use smithay::reexports::wayland_server::Display;

            let (tx, rx) = std::sync::mpsc::channel();
            let injector = ChannelInjector::new(tx);
            // Portal `Notify*` handler side: forward the decoded action.
            assert!(injector.inject(RemoteAction::PointerMotionAbsolute {
                stream: 1,
                x: 77.0,
                y: 88.0,
            }));

            // Compositor-loop side: drain + apply to the live seat.
            let event_loop: EventLoop<'static, State> =
                EventLoop::try_new().expect("calloop event loop");
            let display: Display<State> = Display::new().expect("wayland display");
            let mut state = State::new(event_loop.handle(), display.handle());
            while let Ok(action) = rx.try_recv() {
                state.apply_remote_desktop_action(action);
            }
            let loc = state.seat.get_pointer().unwrap().current_location();
            assert_eq!(
                (loc.x, loc.y),
                (77.0, 88.0),
                "channel injector → loop drain → seat must move the pointer"
            );
        }
    }
}
