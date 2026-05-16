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
}
