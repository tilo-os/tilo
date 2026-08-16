//! Owner-side streaming + peer-side input-replay coordination — Phase 4
//! Tasks 4.2 and 4.4.
//!
//! Two responsibilities live in this module:
//!
//! 1. [`StreamingThrottle`] — a 30-fps throttle for the owner's
//!    damage-aware encode loop. The compositor calls
//!    [`StreamingThrottle::should_encode`] on every smithay surface
//!    commit; the throttle returns `true` at most once per ~33 ms so a
//!    flood of damage events doesn't burn CPU re-encoding the same
//!    pixels.
//! 2. [`ShareRegistry`] — owner-side bookkeeping for "which surfaces are
//!    shared with which peer, and is the share read-only?". The receive
//!    side of [`crate::network::NeighbourMsg::RemoteInput`] consults the
//!    registry through [`ShareRegistry::accept_remote_input`] to drop
//!    events when the owner has flipped the per-peer `read_only` flag.
//!
//! `InputEvent` is the wire-format vocabulary peer→owner input traffic
//! travels in. Its serde bytes ride inside `NeighbourMsg::RemoteInput`'s
//! `event` field; the owner deserialises and either drops it
//! (`read_only = true`) or hands it to a [`RemoteInputSink`] which is
//! the seat-injection trait the compositor's `State` implements.

use crate::network::encoder::{EncodeError, H264Encoder};
use crate::network::protocol::{PeerId, SurfaceId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Instant;

/// Damage-aware encode throttle.
///
/// Now lives in the standalone [`qos_callstream`] crate (its `throttle`
/// module) so the compositor's surface streaming and the chat call app
/// share one copy. Re-exported here under its historical path so
/// [`encode_tick`] and the flatpak-share session keep compiling
/// unchanged: one instance per shared surface, `should_encode(now)`
/// gates the 30-fps cap, `mark_encoded(now)` stamps a successful encode.
pub use qos_callstream::throttle::StreamingThrottle;

/// Wire-format input event a peer publishes to drive the owner's seat.
///
/// Phase 4 v1 carries pointer motion / button + a single keysym; richer
/// events (touch, axis, tablet) are a follow-up. The serde representation
/// rides inside [`crate::network::NeighbourMsg::RemoteInput::event`].
///
/// # Keyboard focus semantics
///
/// `PointerEnter` and `PointerLeave` are the focus-grab / -release signals
/// the owner uses to route subsequent [`InputEvent::Key`] events to the
/// right client. On `PointerEnter { surface }` the owner grabs keyboard
/// focus onto the peer's virtual seat for `surface`; on `PointerLeave` the
/// grab releases. Between them, `Key` events flow into the focused client
/// through the seat's `KeyboardHandle::input`.
///
/// # Multi-user typing
///
/// When two peers hold overlapping shares of the same surface and type
/// concurrently, their keystrokes interleave at the kernel-input level:
/// whichever `InputEvent::Key` arrives first at the owner wins the
/// serial-counter mint and is delivered to the focused client first. There
/// is no session-level lock — `KeyboardHandle::input` is atomic per event,
/// so a "half-composed" character is not observable, but the resulting
/// character stream is a race with the network order. Callers that need
/// deterministic multi-typist behaviour should serialise upstream.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum InputEvent {
    /// Pointer moved to logical `(x, y)`, **surface-local** to the shared
    /// surface's content rect: a peer sees only the surface it was given
    /// and has no way to know where on the owner's output that surface
    /// sits. [`crate::network::input_firewall`] checks the coordinate
    /// against that rect and translates it into the owner's output space
    /// before it reaches the seat.
    PointerMotion { x: f64, y: f64 },
    /// Mouse button transitioned to `pressed`. `button` mirrors smithay's
    /// libinput-style button codes (e.g. 272 = BTN_LEFT).
    PointerButton { button: u32, pressed: bool },
    /// Peer's pointer entered `surface`. The owner responds by grabbing
    /// keyboard focus onto the peer's virtual seat for `surface`, so
    /// subsequent [`InputEvent::Key`] events land in the right client.
    ///
    /// The remote driver emits this once when it acquires the share's
    /// focus (typically on hover / click into the `PeerView`).
    PointerEnter { surface: SurfaceIdWire },
    /// Peer's pointer left `surface`. The owner releases the keyboard
    /// focus grab for the peer's virtual seat. Subsequent `Key` events
    /// fall on the floor at the seat level until another `PointerEnter`.
    PointerLeave { surface: SurfaceIdWire },
    /// Key press / release. `keysym` is the raw u32 from
    /// `xkbcommon::xkb::Keysym`; the owner translates back to its local
    /// keymap before injection.
    ///
    /// `modifiers` is a bitfield in xkbcommon's `Mods` layout — Shift=1,
    /// Ctrl=4, Alt=8, Logo=64, etc. The owner sets the modifier state on
    /// the seat's keyboard *before* delivering the key, then unsets it on
    /// key-release, so held modifiers survive across the network. The
    /// field defaults to `0` (via serde) so the legacy two-field
    /// `{ keysym, pressed }` payload deserialises with no modifiers set,
    /// preserving compat with peers on older builds.
    Key {
        keysym: u32,
        pressed: bool,
        #[serde(default)]
        modifiers: u32,
    },
}

/// Wire-side surface id. Duplicated in this module to keep `InputEvent`
/// free of a cross-module dependency; equal to
/// [`crate::network::protocol::SurfaceId`].
pub type SurfaceIdWire = crate::network::protocol::SurfaceId;

/// A single decoded `org.freedesktop.impl.portal.RemoteDesktop` `Notify*`
/// action.
///
/// Unlike [`InputEvent`] — which is the *peer-sync* wire format keyed to a
/// `(peer, surface)` share — a `RemoteAction` is a *local* portal request:
/// an app the compositor is hosting asked (via `RemoteDesktop`) to inject
/// input into whatever surface currently holds seat focus. The portal
/// backend decodes each `Notify*` D-Bus call into one of these and hands it
/// to a [`RemoteInputInjector`](crate::portal::RemoteInputInjector); the
/// compositor's event loop drains them and calls
/// [`crate::compositor::state::State::apply_remote_desktop_action`], which
/// drives the real smithay `Seat`.
///
/// Coordinate spaces mirror the RemoteDesktop spec: relative motion is a
/// `(dx, dy)` delta in the stream's logical space; absolute motion is a
/// point in the stream's logical space. Button / keycode use Linux evdev
/// codes; `state` is the freedesktop `0 = released, 1 = pressed` convention,
/// captured here as a `bool`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RemoteAction {
    /// Relative pointer motion (`NotifyPointerMotion`).
    PointerMotionRelative { dx: f64, dy: f64 },
    /// Absolute pointer motion (`NotifyPointerMotionAbsolute`). `stream` is
    /// the PipeWire node id the coordinate is relative to.
    PointerMotionAbsolute { stream: u32, x: f64, y: f64 },
    /// Pointer button (`NotifyPointerButton`). `button` is an evdev code
    /// (e.g. `BTN_LEFT` = 272).
    PointerButton { button: i32, pressed: bool },
    /// Smooth scroll (`NotifyPointerAxis`).
    PointerAxis { dx: f64, dy: f64 },
    /// Discrete scroll (`NotifyPointerAxisDiscrete`). `axis` is `0`
    /// (vertical) or `1` (horizontal) per the spec.
    PointerAxisDiscrete { axis: u32, steps: i32 },
    /// Key press/release by evdev keycode (`NotifyKeyboardKeycode`).
    KeyboardKeycode { keycode: i32, pressed: bool },
    /// Key press/release by keysym (`NotifyKeyboardKeysym`).
    KeyboardKeysym { keysym: i32, pressed: bool },
}

/// Sink the owner's compositor exposes to receive remote-input events.
///
/// Implemented by `crate::compositor::state::State::apply_remote_input`
/// for the real Wayland path; tests use [`RecordingSink`] to assert
/// delivery without spinning up a smithay seat.
pub trait RemoteInputSink {
    /// Apply a remote-input event to the owner's seat, scoped to
    /// `surface` on behalf of `peer`. The implementation is responsible
    /// for any keymap translation and for ignoring events that target
    /// surfaces the owner no longer owns.
    fn apply(&mut self, peer: PeerId, surface: SurfaceId, event: InputEvent);
}

/// Per-share metadata held by the owner.
///
/// `read_only` is the flag flipped by the owner's UI when they want a
/// peer to observe but not drive a shared surface. The default at
/// construction is `false` — peer-sync mirrors smithay's seat semantics
/// where the inviter is the host and the invitee has full control until
/// the host narrows it.
#[derive(Debug, Clone, PartialEq)]
pub struct StreamingShare {
    /// Whether this peer is allowed to inject input. `true` drops every
    /// incoming `RemoteInput`.
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

/// Owner-side per-share registry.
///
/// Keyed by `(peer, surface)` — the same peer may have multiple surfaces
/// shared with them (one per foreign-client tab the peer has subscribed
/// to). Lookup is O(1).
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

    /// Flip the `read_only` flag on an existing share. No-op if the
    /// share doesn't exist; callers that need creation-on-toggle should
    /// `insert` first.
    pub fn set_read_only(&mut self, peer: PeerId, surface: SurfaceId, read_only: bool) {
        if let Some(share) = self.shares.get_mut(&(peer, surface)) {
            share.read_only = read_only;
        }
    }

    /// Decide whether an inbound `RemoteInput` clears the *share* gate:
    /// there is a record for this `(peer, surface)` pair and the owner has
    /// not marked it read-only. The drop path covers both "share doesn't
    /// exist" (peer is making things up) and "share is read-only" (owner
    /// revoked write access).
    ///
    /// This is the first two of the checks a remote event has to pass, not
    /// all of them — where the event may land, which chords it may carry
    /// and how fast it may arrive are
    /// [`crate::network::input_firewall`]'s. The whole sequence is
    /// [`crate::compositor::state::State::review_remote_input`].
    pub fn accept_remote_input(&self, peer: PeerId, surface: SurfaceId) -> bool {
        match self.shares.get(&(peer, surface)) {
            Some(share) => !share.read_only,
            None => false,
        }
    }

    /// Mark every share read-only. The registry side of the kill switch:
    /// peers keep seeing what they were shown, and stop being able to
    /// touch it.
    ///
    /// Returns how many shares actually lost write access, so the caller
    /// can report what the panic key did.
    pub fn revoke_all_input(&mut self) -> usize {
        let mut revoked = 0;
        for share in self.shares.values_mut() {
            if !share.read_only {
                share.read_only = true;
                revoked += 1;
            }
        }
        revoked
    }

    /// Convenience: route an inbound `InputEvent` to `sink` only if the
    /// registry permits it. Returns whether the event was applied. The
    /// owner's actor loop calls this from its `NeighbourMsg::RemoteInput`
    /// handler.
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

/// Outcome of a single `encode_tick` call — what the damage-aware loop
/// did (or didn't do) this iteration.
///
/// `Encoded` carries the freshly-encoded Annex-B NALU bytes; the
/// streaming task hands them straight to
/// [`crate::network::NeighbourMsg::SurfaceFrame`] for publication. The
/// other variants are reasons the tick produced no bytes, surfaced as
/// distinct enum variants so the task can report metrics (encode-skipped
/// vs. throttled) without poking at the throttle internals.
#[derive(Debug)]
pub enum EncodeTickOutcome {
    /// A frame was encoded; carries the NALU payload.
    Encoded(Vec<u8>),
    /// The throttle rejected this tick (last encode was less than the
    /// configured interval ago). No frame fetched.
    Throttled,
    /// The throttle was open but no new frame is staged — either the
    /// foreign surface hasn't committed since the previous take, or
    /// the readback failed and the tracker slot was emptied.
    NoFrame,
    /// The encoder rejected the frame (typically buffer-size mismatch
    /// after a dimension change before the encoder is reconfigured).
    EncodeFailed(EncodeError),
}

/// Drive one iteration of the owner-side damage-aware encode loop —
/// Peer-sync Phase 4 v2.
///
/// `fetch_frame` is the closure the streaming task supplies; in
/// production it bottoms out at
/// [`crate::compositor::surface_readback::SurfaceTracker::take_latest`],
/// in tests it can return a synthesised RGBA frame. The split lets the
/// encode loop stay decoupled from the compositor's `State` while
/// keeping the production wiring single-trait — no
/// `FrameSource`-style indirection.
///
/// The throttle is *not* mutated until an encode actually succeeds, so
/// callers that swallow [`EncodeTickOutcome::EncodeFailed`] don't
/// accidentally poison the timestamp.
///
/// Returns the [`EncodeTickOutcome`] for the caller to ship / drop /
/// retry. The function does not allocate beyond what
/// [`H264Encoder::encode`] does internally; the staged RGBA `Vec` from
/// `fetch_frame` is consumed by `encode`.
pub fn encode_tick<F>(
    encoder: &mut H264Encoder,
    throttle: &mut StreamingThrottle,
    now: Instant,
    fetch_frame: F,
) -> EncodeTickOutcome
where
    F: FnOnce() -> Option<Vec<u8>>,
{
    if !throttle.should_encode(now) {
        return EncodeTickOutcome::Throttled;
    }
    let rgba = match fetch_frame() {
        Some(bytes) => bytes,
        None => return EncodeTickOutcome::NoFrame,
    };
    match encoder.encode(&rgba) {
        Ok(nalus) => {
            throttle.mark_encoded(now);
            EncodeTickOutcome::Encoded(nalus)
        }
        Err(err) => EncodeTickOutcome::EncodeFailed(err),
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    /// Test-only [`RemoteInputSink`] that records every applied event in
    /// arrival order. Tests assert on `events.len()` and content rather
    /// than a real smithay seat.
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
mod tests {
    use super::*;
    use crate::network::streaming::test_support::RecordingSink;
    use std::time::Duration;

    // The `StreamingThrottle` unit tests (30-fps cap, first-call-accepts,
    // rejects-back-to-back) moved with the type into the `qos_callstream`
    // crate's `throttle` module.

    /// Phase 4 Task 4.4 — required test. A synthesised `RemoteInput`
    /// reaches the seat when the share is read/write; it's dropped when
    /// the owner flips `read_only`.
    #[test]
    fn remote_input_blocked_when_read_only() {
        let peer: PeerId = 7;
        let surface: SurfaceId = 42;
        let mut registry = ShareRegistry::new();
        registry.insert(peer, surface, StreamingShare::new());
        let mut sink = RecordingSink::default();

        // Read/write: event lands.
        let ev = InputEvent::PointerMotion { x: 10.0, y: 20.0 };
        assert!(registry.route_to(peer, surface, ev.clone(), &mut sink));
        assert_eq!(sink.events.len(), 1);
        assert_eq!(sink.events[0], (peer, surface, ev.clone()));

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

    #[test]
    fn remote_input_dropped_for_unknown_share() {
        let mut sink = RecordingSink::default();
        let registry = ShareRegistry::new();
        let ev = InputEvent::PointerMotion { x: 1.0, y: 2.0 };
        // Peer never invited; event must drop.
        assert!(!registry.route_to(99, 99, ev, &mut sink));
        assert!(sink.events.is_empty());
    }

    #[test]
    fn input_event_serde_round_trips() {
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
        ];
        for ev in events {
            let bytes = serde_json::to_vec(&ev).expect("serialize");
            let round: InputEvent = serde_json::from_slice(&bytes).expect("deserialize");
            assert_eq!(round, ev);
        }
    }

    /// Peer-sync Phase 4 v2 — required test. The synthetic-gradient
    /// round-trip in `encoder::tests::gradient_round_trip_psnr_at_least_35_db`
    /// proved the encoder works on a fabricated buffer. This test
    /// wires the real production egress path through a
    /// [`crate::compositor::surface_readback::SurfaceTracker`]: a
    /// foreign-surface RGBA frame is deposited into the tracker, the
    /// `encode_tick` loop drains it via `take_latest` (the same call
    /// the production streaming task uses), the resulting NALU bytes
    /// go through the H.264 decoder, and the decoded frame is
    /// PSNR-compared against the input. Floor is the same 35 dB the
    /// Phase 4 v1 gradient test asserts.
    ///
    /// The deposit-side helper
    /// (`SurfaceTracker::deposit_for_test`) is gated to `#[cfg(test)]`
    /// because in production the bytes arrive via
    /// `with_buffer_contents` on a real shm `wl_buffer`. Building a
    /// real shm buffer requires a live `wayland-server::Display` +
    /// `wl_shm` pool, which a unit test can't spin up without an
    /// external client process. The mock keeps the *egress* path
    /// (`take_latest`) on the same code as production, which is what
    /// the v2 spec calls out.
    #[test]
    fn shared_foreign_surface_round_trips_rgba() {
        use crate::compositor::surface_readback::SurfaceTracker;
        use crate::network::decoder::H264Decoder;

        const W: u32 = 64;
        const H: u32 = 64;
        const SURFACE: SurfaceId = 314;

        // Deterministic input — soft gradient so H.264 has a fair
        // shot at the 35 dB floor.
        let mut input = Vec::with_capacity((W * H * 4) as usize);
        for y in 0..H {
            for x in 0..W {
                input.push(((x * 255) / W) as u8);
                input.push(((y * 255) / H) as u8);
                input.push(0x80);
                input.push(0xFF);
            }
        }

        // Inject into the tracker the same way the compositor's
        // `commit` handler would — but without the wayland plumbing
        // (see the doc comment above).
        let mut tracker = SurfaceTracker::new();
        tracker.deposit_for_test(SURFACE, input.clone(), W, H);

        // Production egress path: a single `encode_tick` driven from
        // a closure that calls `tracker.take_latest(surface)`. The
        // streaming task in production wires exactly this shape — the
        // closure is the only knob it needs.
        let mut encoder = H264Encoder::new(W, H, 30).expect("encoder");
        let mut throttle = StreamingThrottle::new();
        let outcome = encode_tick(&mut encoder, &mut throttle, Instant::now(), || {
            tracker.take_latest(SURFACE).map(|frame| frame.rgba)
        });
        let nalus = match outcome {
            EncodeTickOutcome::Encoded(b) => b,
            other => panic!("expected Encoded, got {other:?}"),
        };
        assert!(!nalus.is_empty(), "encoder produced empty bitstream");

        // Tracker slot was drained — a second tick must report NoFrame
        // (no new commit since the take).
        let outcome2 = encode_tick(
            &mut encoder,
            &mut throttle,
            Instant::now() + Duration::from_millis(50),
            || tracker.take_latest(SURFACE).map(|frame| frame.rgba),
        );
        assert!(
            matches!(outcome2, EncodeTickOutcome::NoFrame),
            "expected NoFrame, got {outcome2:?}"
        );

        // Decode + PSNR check, mirroring the v1 gradient test floor.
        let mut decoder = H264Decoder::new();
        let frame = decoder.decode(&nalus).expect("decoder yields a frame");
        assert_eq!(frame.width, W);
        assert_eq!(frame.height, H);

        let strip_alpha = |buf: &[u8]| -> Vec<u8> {
            buf.chunks_exact(4)
                .flat_map(|c| [c[0], c[1], c[2]])
                .collect()
        };
        let orig_rgb = strip_alpha(&input);
        let rgba = match &frame.pixels {
            crate::network::decoder::DecodedPixels::Rgba8(b) => b,
            other => panic!("H.264 decoder returned non-RGBA pixels: {other:?}"),
        };
        let decoded_rgb = strip_alpha(rgba);
        // PSNR — inline so this test stays self-contained without
        // re-exporting `encoder::tests::psnr`.
        let mse: f64 = orig_rgb
            .iter()
            .zip(&decoded_rgb)
            .map(|(a, b)| (*a as f64 - *b as f64).powi(2))
            .sum::<f64>()
            / orig_rgb.len() as f64;
        let psnr = if mse == 0.0 {
            f64::INFINITY
        } else {
            20.0 * 255.0_f64.log10() - 10.0 * mse.log10()
        };
        assert!(
            psnr >= 35.0,
            "shared-surface PSNR {psnr:.2} dB below 35 dB floor (v2 readback regression)"
        );
        eprintln!("shared-surface PSNR = {psnr:.3} dB");
    }

    /// D-1 regression: `apply_remote_input` used to log the event and
    /// stamp `last_remote_input`, but never touched the smithay seat
    /// — Peer-sync Phase 4's input replay was a no-op. The boolean
    /// `true` return gave callers / earlier tests false confidence
    /// that the event had been "applied".
    ///
    /// Post-fix, `RemoteInputSink::apply` for `State` calls
    /// `PointerHandle::motion(...)` for `PointerMotion` events (and
    /// `PointerHandle::button` for buttons). This test asserts the
    /// pointer's logical location actually reflects the injected
    /// coordinates — observable through `seat.get_pointer().current_location()`.
    ///
    /// Keyboard injection is intentionally deferred (see the module
    /// comment in `compositor::state`); the audit explicitly allows
    /// scoping D-1 to pointer events + filing a follow-up.
    #[test]
    fn apply_remote_input_pointer_motion_reaches_seat() {
        use crate::compositor::state::State;
        use crate::network::streaming::{InputEvent, StreamingShare};
        use smithay::reexports::calloop::EventLoop;
        use smithay::reexports::wayland_server::Display;

        // Build a minimal `State` standalone — no listening socket,
        // no clients. This is the same plumbing `native::run` does at
        // startup, minus the winit/drm backend.
        let event_loop: EventLoop<'static, State> =
            EventLoop::try_new().expect("calloop event loop");
        let display: Display<State> = Display::new().expect("wayland display");
        let display_handle = display.handle();
        let mut state = State::new(event_loop.handle(), display_handle);

        // Stage the (peer, surface) share so the read-only gate lets
        // the event through. Without this `apply_remote_input` would
        // drop on the unknown-share path.
        let peer: PeerId = 42;
        let surface: SurfaceId = 7;
        state
            .share_registry
            .insert(peer, surface, StreamingShare::new());

        // Before injection: pointer is at the seat's default origin
        // (0, 0). `current_location` returns the last-known location;
        // a fresh seat starts at the origin.
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

        // Now drive a PointerMotion event through the real
        // production path.
        let routed = state.apply_remote_input(
            peer,
            surface,
            InputEvent::PointerMotion { x: 123.0, y: 456.0 },
        );
        assert!(routed, "share is read/write, motion must route");

        // The seat's pointer location must now reflect the injected
        // coordinates — proves the call actually reached the
        // production `motion()` path, not just stamped
        // `last_remote_input`.
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

        // A second motion shifts the pointer again — the seat is
        // mutating, not pinned at the first value.
        let _ =
            state.apply_remote_input(peer, surface, InputEvent::PointerMotion { x: 5.0, y: 6.0 });
        let after2 = state.seat.get_pointer().unwrap().current_location();
        assert_eq!((after2.x, after2.y), (5.0, 6.0));
    }

    /// `InputEvent::Key` must reach the smithay `KeyboardHandle::input`
    /// path. We can't easily observe a wayland client receiving the
    /// key without spinning up a real client + surface — instead we
    /// prove the delivery reaches the *seat*: after a Key event, the
    /// keyboard's `forwarded_pressed_keys` set includes the keycode.
    ///
    /// The compositor's `KeyboardHandle::input` guarded by the
    /// forwarded-pressed-keys bookkeeping is the same call site
    /// `network::wayland::WaylandServer::handle_input` uses in
    /// production — hitting it here proves the peer-input path is no
    /// longer a no-op.
    #[test]
    fn apply_remote_input_key_reaches_keyboard() {
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

        // Baseline: no keys pressed.
        {
            let kb = state.seat.get_keyboard().expect("keyboard on seat");
            assert!(
                kb.pressed_keys().is_empty(),
                "seat starts with no pressed keys",
            );
        }

        // Drive a Key event through the real production path.
        let keysym: u32 = 30; /* A on a US keymap */
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

        // The seat's keyboard now tracks the injected keycode as
        // pressed — smithay's `forwarded_pressed_keys` (exposed via
        // `KeyboardHandle::pressed_keys`) reflects the state after
        // `KeyboardHandle::input` runs.
        {
            let kb = state.seat.get_keyboard().expect("keyboard on seat");
            let pressed = kb.pressed_keys();
            assert!(
                pressed.iter().any(|k| u32::from(*k) == keysym),
                "keyboard did not observe key press; observed {pressed:?}",
            );
        }

        // Release event mirrors the press: after `Released`, the
        // pressed set no longer contains the keycode.
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
                !kb.pressed_keys().iter().any(|k| u32::from(*k) == keysym),
                "keyboard still holds the released keycode",
            );
        }

        // Origin tag: `last_remote_input` carries the peer id so the
        // future per-user "typing" overlay can find it in one place.
        let (last_peer, _, _) = state.last_remote_input.as_ref().expect("event stamped");
        assert_eq!(*last_peer, peer, "origin peer tag lost");
    }

    /// `PointerEnter` auto-grabs the peer's keyboard focus (bookkeeping
    /// on the peer's `RemoteSeat`). `PointerLeave` releases it. The
    /// physical seat's `set_focus` flip only fires when a
    /// `surface_resolver` is installed — headless tests set it to a
    /// stub, so we assert on the `RemoteSeats` bookkeeping directly.
    #[test]
    fn pointer_enter_grabs_and_leave_releases_remote_seat_focus() {
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

        // No seat entry yet — the peer hasn't emitted any input.
        assert!(state.remote_seats.seat(peer).is_none());

        // Enter grabs focus.
        let routed = state.apply_remote_input(peer, surface, InputEvent::PointerEnter { surface });
        assert!(routed);
        let seat = state
            .remote_seats
            .seat(peer)
            .expect("peer's remote seat exists after enter");
        assert_eq!(seat.focused_surface, Some(surface));

        // Leave releases.
        let _ = state.apply_remote_input(peer, surface, InputEvent::PointerLeave { surface });
        let seat = state
            .remote_seats
            .seat(peer)
            .expect("peer's remote seat still exists");
        assert_eq!(seat.focused_surface, None);
    }

    #[test]
    fn encode_tick_throttled_when_within_interval() {
        use crate::compositor::surface_readback::SurfaceTracker;

        const W: u32 = 32;
        const H: u32 = 32;
        const SURFACE: SurfaceId = 1;
        let frame = vec![0u8; (W * H * 4) as usize];

        let mut tracker = SurfaceTracker::new();
        tracker.deposit_for_test(SURFACE, frame.clone(), W, H);

        let mut encoder = H264Encoder::new(W, H, 30).expect("encoder");
        let mut throttle = StreamingThrottle::new();
        let t0 = Instant::now();

        // First tick encodes.
        let r1 = encode_tick(&mut encoder, &mut throttle, t0, || {
            tracker.take_latest(SURFACE).map(|f| f.rgba)
        });
        assert!(matches!(r1, EncodeTickOutcome::Encoded(_)));

        // Re-deposit immediately. A second tick at t0+1ms must
        // *not* drain the tracker — the throttle short-circuits
        // before `fetch_frame` runs.
        tracker.deposit_for_test(SURFACE, frame, W, H);
        let r2 = encode_tick(
            &mut encoder,
            &mut throttle,
            t0 + Duration::from_millis(1),
            || tracker.take_latest(SURFACE).map(|f| f.rgba),
        );
        assert!(matches!(r2, EncodeTickOutcome::Throttled));
        // Crucially: the tracker slot is still full because the
        // throttle blocked `take_latest` from running.
        assert!(!tracker.is_empty_for(SURFACE));
    }

    /// A RemoteDesktop `NotifyPointerMotionAbsolute` decoded into a
    /// [`RemoteAction`] must reach the real smithay seat — proven by the
    /// pointer's logical location, the same observable
    /// `apply_remote_input_pointer_motion_reaches_seat` uses. This is the
    /// portal `RemoteDesktop` counterpart of that peer-sync test.
    #[test]
    fn apply_remote_desktop_absolute_motion_reaches_seat() {
        use crate::compositor::state::State;
        use crate::network::streaming::RemoteAction;
        use smithay::reexports::calloop::EventLoop;
        use smithay::reexports::wayland_server::Display;

        let event_loop: EventLoop<'static, State> =
            EventLoop::try_new().expect("calloop event loop");
        let display: Display<State> = Display::new().expect("wayland display");
        let mut state = State::new(event_loop.handle(), display.handle());

        let applied = state.apply_remote_desktop_action(
            RemoteAction::PointerMotionAbsolute {
                stream: 1,
                x: 200.0,
                y: 150.0,
            },
        );
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

    /// A RemoteDesktop `NotifyKeyboardKeycode` reaches the seat keyboard —
    /// the keycode appears in `pressed_keys()` after a press and clears on
    /// release.
    #[test]
    fn apply_remote_desktop_keycode_reaches_keyboard() {
        use crate::compositor::state::State;
        use crate::network::streaming::RemoteAction;
        use smithay::reexports::calloop::EventLoop;
        use smithay::reexports::wayland_server::Display;

        let event_loop: EventLoop<'static, State> =
            EventLoop::try_new().expect("calloop event loop");
        let display: Display<State> = Display::new().expect("wayland display");
        let mut state = State::new(event_loop.handle(), display.handle());

        let kc: i32 = 30; // 'a' keycode
        assert!(state.apply_remote_desktop_action(RemoteAction::KeyboardKeycode {
            keycode: kc,
            pressed: true,
        }));
        {
            let kb = state.seat.get_keyboard().expect("keyboard on seat");
            assert!(
                kb.pressed_keys().iter().any(|k| u32::from(*k) == kc as u32),
                "keycode did not reach the seat keyboard",
            );
        }
        let _ = state.apply_remote_desktop_action(RemoteAction::KeyboardKeycode {
            keycode: kc,
            pressed: false,
        });
        let kb = state.seat.get_keyboard().expect("keyboard on seat");
        assert!(
            !kb.pressed_keys().iter().any(|k| u32::from(*k) == kc as u32),
            "release must clear the pressed key",
        );
    }

    /// End-to-end production path: a [`ChannelInjector`] carries a decoded
    /// action to the compositor loop, which drains it into
    /// `apply_remote_desktop_action` and drives the real seat. This is the
    /// wiring `serve()` uses for RemoteDesktop in the hosted compositor.
    #[test]
    fn remote_desktop_channel_injector_drives_the_seat() {
        use crate::compositor::state::State;
        use crate::network::streaming::RemoteAction;
        use crate::portal::{ChannelInjector, RemoteInputInjector};
        use smithay::reexports::calloop::EventLoop;
        use smithay::reexports::wayland_server::Display;

        let (tx, rx) = std::sync::mpsc::channel();
        let injector = ChannelInjector::new(tx);
        // The portal `Notify*` handler side: forward the decoded action.
        assert!(injector.inject(RemoteAction::PointerMotionAbsolute {
            stream: 1,
            x: 77.0,
            y: 88.0,
        }));

        // The compositor-loop side: drain + apply to the live seat.
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
