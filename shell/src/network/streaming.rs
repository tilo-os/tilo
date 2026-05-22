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
use std::time::{Duration, Instant};

/// Minimum gap between two consecutive encodes on a shared surface.
///
/// 33 ms ≈ 30 fps cap. Phase 4 v1 caps at 30 because openh264 software
/// can't sustain higher rates on a typical laptop without burning the
/// peer-sync thread; hardware encode lifts this in a follow-up.
const ENCODE_INTERVAL_MS: u64 = 33;

/// Damage-aware encode throttle.
///
/// One instance per shared surface; the streaming task threads it through
/// the per-frame commit handler. `should_encode(now)` returns `true` when
/// the configured minimum interval has elapsed since the previous
/// accepted encode, `false` otherwise. The throttle does not enforce a
/// frame queue — by design, callers either encode synchronously and ship
/// the bytes or drop the frame.
#[derive(Debug)]
pub struct StreamingThrottle {
    last_encode: Option<Instant>,
    interval: Duration,
}

impl StreamingThrottle {
    /// Construct a throttle gated at 30 fps.
    pub fn new() -> Self {
        Self::with_interval(Duration::from_millis(ENCODE_INTERVAL_MS))
    }

    /// Construct a throttle with a caller-chosen minimum encode interval.
    /// Used by the test suite to feed a synthetic clock.
    pub fn with_interval(interval: Duration) -> Self {
        Self {
            last_encode: None,
            interval,
        }
    }

    /// Should the caller encode *now*? Returns `true` if the throttle's
    /// last-accepted timestamp is unset or strictly older than `interval`,
    /// `false` otherwise. The caller is responsible for then calling
    /// [`Self::mark_encoded`] when (and only when) it actually performs
    /// the encode — so callers that gate on `should_encode` for telemetry
    /// without actually encoding don't poison the throttle.
    pub fn should_encode(&self, now: Instant) -> bool {
        match self.last_encode {
            None => true,
            Some(prev) => now.duration_since(prev) >= self.interval,
        }
    }

    /// Mark a successful encode at `now`. Cementing this in a separate
    /// call (vs. `should_encode_and_mark`) lets the caller skip the
    /// stamp on encode failure without manual state surgery.
    pub fn mark_encoded(&mut self, now: Instant) {
        self.last_encode = Some(now);
    }
}

impl Default for StreamingThrottle {
    fn default() -> Self {
        Self::new()
    }
}

/// Wire-format input event a peer publishes to drive the owner's seat.
///
/// Phase 4 v1 carries pointer motion / button + a single keysym; richer
/// events (touch, axis, tablet) are a follow-up. The serde representation
/// rides inside [`crate::network::NeighbourMsg::RemoteInput::event`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum InputEvent {
    /// Pointer moved to logical `(x, y)` inside the surface's content rect.
    PointerMotion { x: f64, y: f64 },
    /// Mouse button transitioned to `pressed`. `button` mirrors smithay's
    /// libinput-style button codes (e.g. 272 = BTN_LEFT).
    PointerButton { button: u32, pressed: bool },
    /// Key press / release. `keysym` is the raw u32 from
    /// `xkbcommon::xkb::Keysym`; the owner translates back to its local
    /// keymap before injection.
    Key { keysym: u32, pressed: bool },
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

    /// Decide whether an inbound `RemoteInput` should be applied. Returns
    /// `true` to apply, `false` to drop. The drop path covers both
    /// "share doesn't exist" (peer is making things up) and "share is
    /// read-only" (owner revoked write access).
    pub fn accept_remote_input(&self, peer: PeerId, surface: SurfaceId) -> bool {
        match self.shares.get(&(peer, surface)) {
            Some(share) => !share.read_only,
            None => false,
        }
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

    /// Phase 4 Task 4.2 — required test. Drive 100 simulated damage
    /// commits across one wall-clock second through a fake clock; assert
    /// the throttle accepts at most 30. Hand-rolled clock so the test
    /// doesn't actually sleep.
    #[test]
    fn damage_loop_throttles_to_30fps() {
        let start = Instant::now();
        let mut throttle = StreamingThrottle::new();
        let mut encoded = 0u32;
        for i in 0..100 {
            // 100 commits evenly spaced across 1.0 s = one every 10 ms.
            let now = start + Duration::from_millis(i * 10);
            if throttle.should_encode(now) {
                throttle.mark_encoded(now);
                encoded += 1;
            }
        }
        // 33 ms interval over 990 ms of synthesized commits accepts
        // 30 frames (0, 33, 66, …, 957 = 1 + 957/33 = 30 events). The
        // upper bound is the contract; the lower bound is "did we even
        // encode anything" — guard against an off-by-one that mutes the
        // throttle entirely.
        assert!(
            encoded <= 30,
            "throttle accepted {encoded} encodes — must cap at 30 fps"
        );
        assert!(encoded > 0, "throttle muted every encode — interval bug");
    }

    #[test]
    fn throttle_first_call_always_accepts() {
        let now = Instant::now();
        let throttle = StreamingThrottle::new();
        assert!(throttle.should_encode(now));
    }

    #[test]
    fn throttle_rejects_back_to_back() {
        let now = Instant::now();
        let mut throttle = StreamingThrottle::new();
        assert!(throttle.should_encode(now));
        throttle.mark_encoded(now);
        // 1 ms later — well inside the 33 ms interval — must reject.
        assert!(!throttle.should_encode(now + Duration::from_millis(1)));
        // Past the interval, accepts again.
        assert!(throttle.should_encode(now + Duration::from_millis(40)));
    }

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
            },
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
        let decoded_rgb = strip_alpha(&frame.rgba);
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
}
