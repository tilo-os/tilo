//! Per-share observability counters — the OSD overlay's data source.
//!
//! [`PeerSurfaceMetrics`] aggregates two kinds of signal for each peer
//! surface (the receive side of a `flatpak_share` session):
//!
//! 1. **Frame counters** — how many decoded frames landed, how many were
//!    dropped because the decoder / paint queue backed up. Backpressure
//!    on the receive side is drop-oldest: if the caller pushes a new
//!    frame before `mark_drawn` has cleared the previous one and more
//!    than `queue_ceiling` frames are pending, the oldest gets dropped
//!    and [`Self::frames_dropped`] increments. Dropped frames are the
//!    diagnostic the overlay surfaces when a link is over-subscribed.
//!
//! 2. **Latency samples** — a small ring of `(capture_ts_ns,
//!    present_ts_ns)` deltas so the overlay can render a rolling motion-
//!    to-photon histogram. The default ring holds the last 128 samples;
//!    at 60 fps that's ~2.1 s of history, enough to catch a burst-of-
//!    jitter without unbounded memory.
//!
//! Access is [`Clone`] + a single mutex-protected inner so multiple
//! subsystems (the actor's receive loop, the PeerView paint pass, the
//! OSD overlay) can share the same handle without lifetime gymnastics.

use std::sync::{Arc, Mutex};

/// Default ring capacity — 128 samples ≈ 2.1 s at 60 fps.
const LATENCY_RING_CAPACITY: usize = 128;

/// Shared observability counters for one receive-side peer surface.
///
/// Clone is cheap — the inner state sits behind an `Arc<Mutex<_>>` so
/// every clone points at the same numbers. Snapshots via
/// [`Self::snapshot`] copy the current values out for callers that just
/// want to render them (the OSD overlay) without holding the lock.
#[derive(Clone, Debug)]
pub struct PeerSurfaceMetrics {
    inner: Arc<Mutex<Inner>>,
}

#[derive(Debug)]
struct Inner {
    frames_received: u64,
    frames_dropped: u64,
    latency_ring: Vec<u64>,
    latency_head: usize,
    latency_capacity: usize,
    /// Sender→receiver monotonic clock offset, in signed nanoseconds.
    /// Computed once per session from the `SurfaceStreamStart` handshake:
    /// `offset = receiver_monotonic_ns_at_recv - sender_monotonic_ns_at_send`.
    /// A frame with wire timestamp `ts` was captured on the sender at
    /// receiver-monotonic time `ts + offset`; motion-to-photon latency
    /// at the receiver is `receiver_now_ns - (ts + offset)`.
    ///
    /// `None` means the handshake either hasn't landed yet or the peer
    /// is on an older wire format that shipped `ts=0` (see the
    /// `#[serde(default)]` note on `SurfaceStreamStart::sender_monotonic_ns_at_send`).
    /// Callers treat "no offset" as "skip this measurement" rather than
    /// stamping a wrong value.
    clock_offset_ns: Option<i128>,
}

/// Point-in-time copy of the counters for read-only consumers.
///
/// Small enough to pass by value; no `Arc` so callers can serialise
/// or diff without touching the mutex.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct PeerSurfaceSnapshot {
    /// Total frames the receiver successfully decoded and stashed.
    pub frames_received: u64,
    /// Total frames the receiver dropped due to backpressure. Rate =
    /// `frames_dropped / (frames_received + frames_dropped)`.
    pub frames_dropped: u64,
    /// Mean of the current latency ring in nanoseconds. `0` if the ring
    /// is empty.
    pub latency_ns_mean: u64,
    /// Peak (max) latency observed in the current ring, in nanoseconds.
    /// `0` if the ring is empty.
    pub latency_ns_max: u64,
    /// Number of latency samples currently in the ring.
    pub latency_samples: usize,
}

impl PeerSurfaceMetrics {
    /// Fresh counters — all zero, empty ring at the default capacity.
    pub fn new() -> Self {
        Self::with_capacity(LATENCY_RING_CAPACITY)
    }

    /// Fresh counters with a caller-chosen ring capacity. Tests use this
    /// to keep assertions deterministic on small sample counts; production
    /// callers should stick to [`Self::new`].
    pub fn with_capacity(latency_capacity: usize) -> Self {
        let capacity = latency_capacity.max(1);
        Self {
            inner: Arc::new(Mutex::new(Inner {
                frames_received: 0,
                frames_dropped: 0,
                latency_ring: Vec::with_capacity(capacity),
                latency_head: 0,
                latency_capacity: capacity,
                clock_offset_ns: None,
            })),
        }
    }

    /// Record a successfully-decoded frame with its measured
    /// motion-to-photon latency (nanoseconds).
    pub fn record_received(&self, latency_ns: u64) {
        let mut inner = self.inner.lock().expect("metrics mutex");
        inner.frames_received = inner.frames_received.saturating_add(1);
        // Insert into the ring. Grow up to capacity, then overwrite the
        // oldest slot via `latency_head`.
        if inner.latency_ring.len() < inner.latency_capacity {
            inner.latency_ring.push(latency_ns);
        } else {
            let cap = inner.latency_capacity;
            let head = inner.latency_head;
            inner.latency_ring[head] = latency_ns;
            inner.latency_head = (head + 1) % cap;
        }
    }

    /// Record a dropped frame — the counter increments by one; latency
    /// ring untouched (a dropped frame has no valid latency).
    pub fn record_dropped(&self) {
        let mut inner = self.inner.lock().expect("metrics mutex");
        inner.frames_dropped = inner.frames_dropped.saturating_add(1);
    }

    /// Learn the sender→receiver clock offset from a `SurfaceStreamStart`
    /// handshake.
    ///
    /// Called once per session on the receiving side when the shell
    /// decodes a [`crate::network::NeighbourMsg::SurfaceStreamStart`]
    /// message. `sender_ns_at_send` is the sender's monotonic reading
    /// at message construction; `receiver_ns_at_recv` is the receiver's
    /// reading at message decode. The offset is stashed and later
    /// consumed by [`Self::end_to_end_latency_ns`] to translate every
    /// per-frame `ts` on the wire into the receiver's clock domain.
    ///
    /// Passing `sender_ns_at_send == 0` signals "the sender didn't
    /// include the field" (older peers, or the `#[serde(default)]`
    /// fallback). We clear the offset in that case rather than storing
    /// a bogus receiver-clock value, so downstream latency queries
    /// return `None` (which the OSD renders as "—") instead of a
    /// nonsense number.
    pub fn set_clock_offset(&self, sender_ns_at_send: u64, receiver_ns_at_recv: u64) {
        let mut inner = self.inner.lock().expect("metrics mutex");
        if sender_ns_at_send == 0 {
            inner.clock_offset_ns = None;
            return;
        }
        let offset = (receiver_ns_at_recv as i128) - (sender_ns_at_send as i128);
        inner.clock_offset_ns = Some(offset);
    }

    /// End-to-end latency in **nanoseconds** for a frame whose wire
    /// `ts` (sender-side monotonic ns) is `wire_ts_ns`, observed at
    /// receiver-side monotonic ns `receiver_now_ns`.
    ///
    /// Returns `None` when no clock-offset has been learned yet (no
    /// handshake) or when the arithmetic would underflow (clock skew
    /// against a frame that predates the handshake). Callers treat
    /// `None` as "skip this measurement" and don't push to the latency
    /// ring — a wrong measurement is worse than a missing one.
    pub fn end_to_end_latency_ns(&self, wire_ts_ns: u64, receiver_now_ns: u64) -> Option<u64> {
        let inner = self.inner.lock().expect("metrics mutex");
        let offset = inner.clock_offset_ns?;
        // The sender's `ts`, expressed on the receiver's clock, is
        // `wire_ts_ns + offset` — that's the "moment of capture" in the
        // receiver's monotonic domain. Latency is now − that.
        let capture_on_receiver = (wire_ts_ns as i128) + offset;
        let latency = (receiver_now_ns as i128) - capture_on_receiver;
        if latency < 0 {
            None
        } else {
            Some(latency as u64)
        }
    }

    /// Convenience: the same latency in **milliseconds**. Wraps
    /// [`Self::end_to_end_latency_ns`] and rounds to the nearest u64.
    /// Returns `None` for the same reasons the ns-precision method does.
    pub fn end_to_end_latency_ms(&self, wire_ts_ns: u64, receiver_now_ns: u64) -> Option<u64> {
        self.end_to_end_latency_ns(wire_ts_ns, receiver_now_ns)
            .map(|ns| (ns + 500_000) / 1_000_000)
    }

    /// Current learned clock offset, or `None` before the handshake.
    /// Exposed for diagnostics — the OSD overlay shows the offset
    /// alongside the latency histogram so a huge offset (drifted NTP,
    /// unsync'd hosts) is visible.
    pub fn clock_offset_ns(&self) -> Option<i128> {
        self.inner
            .lock()
            .expect("metrics mutex")
            .clock_offset_ns
    }

    /// Snapshot the counters for a read-only consumer.
    pub fn snapshot(&self) -> PeerSurfaceSnapshot {
        let inner = self.inner.lock().expect("metrics mutex");
        let latency_samples = inner.latency_ring.len();
        let (latency_ns_mean, latency_ns_max) = if latency_samples == 0 {
            (0, 0)
        } else {
            let sum: u128 = inner.latency_ring.iter().map(|s| *s as u128).sum();
            let mean = (sum / latency_samples as u128) as u64;
            let max = inner.latency_ring.iter().copied().max().unwrap_or(0);
            (mean, max)
        };
        PeerSurfaceSnapshot {
            frames_received: inner.frames_received,
            frames_dropped: inner.frames_dropped,
            latency_ns_mean,
            latency_ns_max,
            latency_samples,
        }
    }

    /// Reset every counter. Used by tests that reuse a metrics handle
    /// across cases; production code has no reason to call this.
    #[cfg(test)]
    pub fn reset(&self) {
        let mut inner = self.inner.lock().expect("metrics mutex");
        inner.frames_received = 0;
        inner.frames_dropped = 0;
        inner.latency_ring.clear();
        inner.latency_head = 0;
    }
}

impl Default for PeerSurfaceMetrics {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_metrics_snapshot_is_zero() {
        let m = PeerSurfaceMetrics::new();
        let s = m.snapshot();
        assert_eq!(s, PeerSurfaceSnapshot::default());
    }

    #[test]
    fn record_received_increments_and_stores_latency() {
        let m = PeerSurfaceMetrics::with_capacity(4);
        m.record_received(10_000_000);
        m.record_received(20_000_000);
        let s = m.snapshot();
        assert_eq!(s.frames_received, 2);
        assert_eq!(s.frames_dropped, 0);
        assert_eq!(s.latency_samples, 2);
        assert_eq!(s.latency_ns_mean, 15_000_000);
        assert_eq!(s.latency_ns_max, 20_000_000);
    }

    #[test]
    fn record_dropped_does_not_touch_latency() {
        let m = PeerSurfaceMetrics::new();
        m.record_received(1_000);
        m.record_dropped();
        m.record_dropped();
        let s = m.snapshot();
        assert_eq!(s.frames_received, 1);
        assert_eq!(s.frames_dropped, 2);
        assert_eq!(s.latency_samples, 1);
        assert_eq!(s.latency_ns_mean, 1_000);
    }

    #[test]
    fn latency_ring_wraps_at_capacity() {
        // Capacity 3 with 5 pushes: samples 3, 4, 5 remain (drop 1 + 2).
        let m = PeerSurfaceMetrics::with_capacity(3);
        for i in 1..=5u64 {
            m.record_received(i * 1_000_000);
        }
        let s = m.snapshot();
        assert_eq!(s.frames_received, 5);
        assert_eq!(s.latency_samples, 3);
        // (3 + 4 + 5) / 3 = 4 → 4_000_000 ns.
        assert_eq!(s.latency_ns_mean, 4_000_000);
        assert_eq!(s.latency_ns_max, 5_000_000);
    }

    #[test]
    fn clone_shares_inner_state() {
        let m1 = PeerSurfaceMetrics::new();
        let m2 = m1.clone();
        m1.record_received(500);
        m2.record_dropped();
        let s = m1.snapshot();
        assert_eq!(s.frames_received, 1);
        assert_eq!(s.frames_dropped, 1);
        // m2's view sees the same values.
        assert_eq!(m2.snapshot(), s);
    }

    #[test]
    fn no_clock_offset_returns_none_for_latency() {
        let m = PeerSurfaceMetrics::new();
        assert_eq!(m.end_to_end_latency_ns(1_000, 5_000), None);
        assert_eq!(m.end_to_end_latency_ms(1_000, 5_000), None);
        assert_eq!(m.clock_offset_ns(), None);
    }

    #[test]
    fn clock_offset_zero_ts_clears() {
        let m = PeerSurfaceMetrics::new();
        // First, learn a real offset.
        m.set_clock_offset(1_000, 5_000);
        assert_eq!(m.clock_offset_ns(), Some(4_000));
        // Then a "no field on the wire" event — offset is cleared.
        m.set_clock_offset(0, 5_000);
        assert_eq!(m.clock_offset_ns(), None);
    }

    #[test]
    fn end_to_end_latency_uses_learned_offset() {
        let m = PeerSurfaceMetrics::new();
        // Sender was 1_000_000 ns ahead of receiver at handshake.
        // (sender's clock: 100_000_000; receiver's clock: 101_000_000 → offset = +1_000_000)
        m.set_clock_offset(100_000_000, 101_000_000);
        // A frame with wire ts = 200_000_000 (sender monotonic ns), and
        // receiver now = 201_500_000, has a 500 000 ns latency:
        //   capture_on_receiver = 200_000_000 + 1_000_000 = 201_000_000
        //   latency = 201_500_000 - 201_000_000 = 500_000 ns
        assert_eq!(
            m.end_to_end_latency_ns(200_000_000, 201_500_000),
            Some(500_000)
        );
        // Rounded to ms: 500 000 ns rounds to 1 ms via the +500 000 offset.
        assert_eq!(m.end_to_end_latency_ms(200_000_000, 201_500_000), Some(1));
    }

    #[test]
    fn end_to_end_latency_none_when_negative() {
        let m = PeerSurfaceMetrics::new();
        m.set_clock_offset(1_000_000, 500_000);
        // Frame whose "capture on receiver clock" is in the future
        // (would-be latency negative) — return None instead of a wrong
        // number.
        assert_eq!(m.end_to_end_latency_ns(2_000_000, 500_000), None);
    }
}
