//! Per-share observability counters — the OSD overlay's data source.
//!
//! [`PeerSurfaceMetrics`] aggregates, per peer surface (receive side of a
//! `flatpak_share` session): frame counters (received vs. dropped,
//! drop-oldest under backpressure); a rolling ring of motion-to-photon
//! latency samples (128 ≈ 2.1 s at 60 fps); a parallel ring of
//! sender-side encode wall-clock times (p50/p99); and the chosen encoder
//! + capture path, set once at session start.
//!
//! [`Clone`] + a single mutex-protected inner, so the actor's receive
//! loop, the `PeerView` paint pass, and the OSD overlay share one handle.

use std::sync::{Arc, Mutex};

use crate::collab::encoder::EncoderKind;

/// Default ring capacity — 128 samples ≈ 2.1 s at 60 fps.
const LATENCY_RING_CAPACITY: usize = 128;

/// Ring capacity for encode-wallclock samples — same size as latency, since they represent the same time window.
const ENCODE_WALLCLOCK_CAPACITY: usize = 128;

/// Which route a captured frame took from the client's `wl_buffer` to the encoder.
///
/// `ShmReadback` copies out of the client's SHM pool and swizzles
/// ARGB→RGBA into a Vec before handing it to the encoder: ~4 ms per 720p
/// frame, the default fallback (Xwayland is the common case).
/// `DmabufDirect` is zero-copy — the compositor hands the DMABUF fd +
/// strides straight to the encoder (VA-API's `Surface::from_dma_buf`,
/// NVENC's registered CUDA external memory), saving ~3.5 ms in the encode-wallclock ring.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum CapturePath {
    /// SHM pool readback + CPU-side swizzle.
    #[default]
    ShmReadback,
    /// Direct DMABUF fd handed to the encoder — no CPU copy.
    DmabufDirect,
}

impl CapturePath {
    /// Human-readable label for the OSD overlay.
    pub fn as_str(self) -> &'static str {
        match self {
            CapturePath::ShmReadback => "shm-readback",
            CapturePath::DmabufDirect => "dmabuf-direct",
        }
    }
}

/// Shared observability counters for one receive-side peer surface.
///
/// Clone is cheap — inner state sits behind an `Arc<Mutex<_>>`, so every
/// clone points at the same numbers. [`Self::snapshot`] copies out current values without holding the lock.
#[derive(Clone, Debug)]
pub struct PeerSurfaceMetrics {
    inner: Arc<Mutex<Inner>>,
}

#[derive(Debug)]
struct Inner {
    frames_received: u64,
    frames_dropped: u64,
    keyframes: u64,
    latency_ring: Vec<u64>,
    latency_head: usize,
    latency_capacity: usize,
    encode_ring: Vec<u64>,
    encode_head: usize,
    encode_capacity: usize,
    chosen_encoder: EncoderKind,
    capture_path: CapturePath,
    bitrate_bytes_per_sec_ewma: u64,
    /// Sender→receiver monotonic clock offset, in signed nanoseconds:
    /// `offset = receiver_monotonic_ns_at_recv - sender_monotonic_ns_at_send`,
    /// computed once per session from the `SurfaceStreamStart` handshake. A
    /// frame with wire timestamp `ts` was captured on the sender at
    /// receiver-monotonic time `ts + offset`.
    ///
    /// `None` means the handshake hasn't landed, or the peer ships `ts=0`
    /// (older wire format); callers treat "no offset" as "skip this measurement" rather than stamping a wrong value.
    clock_offset_ns: Option<i128>,
}

/// Point-in-time copy of the counters for read-only consumers — small
/// enough to pass by value, no `Arc`, so callers can serialise or diff without touching the mutex.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct PeerSurfaceSnapshot {
    /// Total frames the receiver successfully decoded and stashed.
    pub frames_received: u64,
    /// Total frames the receiver dropped due to backpressure. Rate = `frames_dropped / (frames_received + frames_dropped)`.
    pub frames_dropped: u64,
    /// Number of keyframes observed — proxy for GOP length + encoder health
    /// (a spike means the encoder is refreshing more aggressively than the target keyframe interval).
    pub keyframes: u64,
    /// Mean of the current latency ring in nanoseconds; `0` if empty.
    pub latency_ns_mean: u64,
    /// Peak (max) latency observed in the current ring, in nanoseconds; `0` if empty.
    pub latency_ns_max: u64,
    /// Number of latency samples currently in the ring.
    pub latency_samples: usize,
    /// Encode wall-clock p50 (median) in milliseconds.
    pub encode_wallclock_ms_p50: u64,
    /// Encode wall-clock p99 in milliseconds.
    pub encode_wallclock_ms_p99: u64,
    /// Which encoder the runtime probe selected, set once at session start via [`PeerSurfaceMetrics::set_chosen_encoder`].
    pub chosen_encoder: EncoderKind,
    /// Whether the capture path used direct DMABUF import or SHM readback for the most recently observed frame.
    pub capture_path: CapturePath,
    /// Exponentially-weighted moving average of the encoded bitrate (bytes/sec), set by [`PeerSurfaceMetrics::record_bitrate`].
    pub bitrate_bytes_per_sec_ewma: u64,
}

impl Default for PeerSurfaceSnapshot {
    fn default() -> Self {
        Self {
            frames_received: 0,
            frames_dropped: 0,
            keyframes: 0,
            latency_ns_mean: 0,
            latency_ns_max: 0,
            latency_samples: 0,
            encode_wallclock_ms_p50: 0,
            encode_wallclock_ms_p99: 0,
            chosen_encoder: EncoderKind::Openh264,
            capture_path: CapturePath::default(),
            bitrate_bytes_per_sec_ewma: 0,
        }
    }
}

impl PeerSurfaceMetrics {
    /// Fresh counters — all zero, empty rings at the default capacity.
    pub fn new() -> Self {
        Self::with_capacity(LATENCY_RING_CAPACITY)
    }

    /// Fresh counters with a caller-chosen ring capacity — tests use this to
    /// keep assertions deterministic on small sample counts; production callers should stick to [`Self::new`].
    pub fn with_capacity(latency_capacity: usize) -> Self {
        let capacity = latency_capacity.max(1);
        Self {
            inner: Arc::new(Mutex::new(Inner {
                frames_received: 0,
                frames_dropped: 0,
                keyframes: 0,
                latency_ring: Vec::with_capacity(capacity),
                latency_head: 0,
                latency_capacity: capacity,
                encode_ring: Vec::with_capacity(ENCODE_WALLCLOCK_CAPACITY),
                encode_head: 0,
                encode_capacity: ENCODE_WALLCLOCK_CAPACITY,
                chosen_encoder: EncoderKind::Openh264,
                capture_path: CapturePath::default(),
                bitrate_bytes_per_sec_ewma: 0,
                clock_offset_ns: None,
            })),
        }
    }

    /// Record a successfully-decoded frame with its measured motion-to-photon latency (nanoseconds).
    pub fn record_received(&self, latency_ns: u64) {
        let mut inner = self.inner.lock().expect("metrics mutex");
        inner.frames_received = inner.frames_received.saturating_add(1);
        // Insert into the ring: grow up to capacity, then overwrite the oldest slot via `latency_head`.
        if inner.latency_ring.len() < inner.latency_capacity {
            inner.latency_ring.push(latency_ns);
        } else {
            let cap = inner.latency_capacity;
            let head = inner.latency_head;
            inner.latency_ring[head] = latency_ns;
            inner.latency_head = (head + 1) % cap;
        }
    }

    /// Record a dropped frame — counter increments by one; latency ring untouched (a dropped frame has no valid latency).
    pub fn record_dropped(&self) {
        let mut inner = self.inner.lock().expect("metrics mutex");
        inner.frames_dropped = inner.frames_dropped.saturating_add(1);
    }

    /// Record an encode wall-clock sample (nanoseconds); populates the p50/p99 fields the OSD renders.
    pub fn record_encode_wallclock(&self, ns: u64) {
        let mut inner = self.inner.lock().expect("metrics mutex");
        if inner.encode_ring.len() < inner.encode_capacity {
            inner.encode_ring.push(ns);
        } else {
            let cap = inner.encode_capacity;
            let head = inner.encode_head;
            inner.encode_ring[head] = ns;
            inner.encode_head = (head + 1) % cap;
        }
    }

    /// Record that the just-encoded frame was a keyframe (SPS/PPS or IDR refresh).
    pub fn record_keyframe(&self) {
        let mut inner = self.inner.lock().expect("metrics mutex");
        inner.keyframes = inner.keyframes.saturating_add(1);
    }

    /// Update the bitrate EWMA with a fresh sample (bytes/sec); alpha fixed
    /// at 0.2 so recent samples dominate but stay smooth across the ~5s window the OSD renders.
    pub fn record_bitrate(&self, bytes_per_sec: u64) {
        let mut inner = self.inner.lock().expect("metrics mutex");
        let prev = inner.bitrate_bytes_per_sec_ewma;
        let alpha = 20u64; // 20% new sample weight
        inner.bitrate_bytes_per_sec_ewma =
            ((prev.saturating_mul(100 - alpha)) + bytes_per_sec.saturating_mul(alpha)) / 100;
    }

    /// Set the encoder chosen by the runtime probe at session start.
    pub fn set_chosen_encoder(&self, kind: EncoderKind) {
        let mut inner = self.inner.lock().expect("metrics mutex");
        inner.chosen_encoder = kind;
    }

    /// Set the capture path for the most recent frame — the compositor
    /// commit hook calls this after routing the buffer to DMABUF direct or SHM readback.
    pub fn set_capture_path(&self, path: CapturePath) {
        let mut inner = self.inner.lock().expect("metrics mutex");
        inner.capture_path = path;
    }

    /// Learn the sender→receiver clock offset from a `SurfaceStreamStart`
    /// handshake, later consumed by [`Self::end_to_end_latency_ns`] to
    /// translate every per-frame wire `ts` into the receiver's clock domain.
    ///
    /// `sender_ns_at_send == 0` signals the sender didn't include the field
    /// (older peers, or `#[serde(default)]`); the offset is cleared rather
    /// than storing a bogus value, so downstream queries return `None` (OSD renders "—") instead of a nonsense number.
    pub fn set_clock_offset(&self, sender_ns_at_send: u64, receiver_ns_at_recv: u64) {
        let mut inner = self.inner.lock().expect("metrics mutex");
        if sender_ns_at_send == 0 {
            inner.clock_offset_ns = None;
            return;
        }
        let offset = i128::from(receiver_ns_at_recv) - i128::from(sender_ns_at_send);
        inner.clock_offset_ns = Some(offset);
    }

    /// End-to-end latency in **nanoseconds** for a frame whose wire `ts`
    /// (sender-side monotonic ns) is `wire_ts_ns`, observed at receiver-side monotonic ns `receiver_now_ns`.
    ///
    /// Returns `None` when no clock-offset has been learned yet, or when the
    /// arithmetic would underflow (clock skew against a frame predating the
    /// handshake) — callers skip the measurement rather than push a wrong one to the latency ring.
    pub fn end_to_end_latency_ns(&self, wire_ts_ns: u64, receiver_now_ns: u64) -> Option<u64> {
        let inner = self.inner.lock().expect("metrics mutex");
        let offset = inner.clock_offset_ns?;
        // The sender's `ts` on the receiver's clock is `wire_ts_ns + offset` —
        // the "moment of capture" in the receiver's monotonic domain; latency is now minus that.
        let capture_on_receiver = i128::from(wire_ts_ns) + offset;
        let latency = i128::from(receiver_now_ns) - capture_on_receiver;
        if latency < 0 {
            None
        } else {
            Some(latency as u64)
        }
    }

    /// Convenience: the same latency in **milliseconds**, wrapping
    /// [`Self::end_to_end_latency_ns`] and rounding to the nearest u64; `None` for the same reasons.
    pub fn end_to_end_latency_ms(&self, wire_ts_ns: u64, receiver_now_ns: u64) -> Option<u64> {
        self.end_to_end_latency_ns(wire_ts_ns, receiver_now_ns)
            .map(|ns| (ns + 500_000) / 1_000_000)
    }

    /// Current learned clock offset, or `None` before the handshake —
    /// exposed so the OSD can show a huge offset (drifted NTP, unsync'd hosts) alongside the latency histogram.
    pub fn clock_offset_ns(&self) -> Option<i128> {
        self.inner.lock().expect("metrics mutex").clock_offset_ns
    }

    /// Snapshot the counters for a read-only consumer.
    pub fn snapshot(&self) -> PeerSurfaceSnapshot {
        let inner = self.inner.lock().expect("metrics mutex");
        let latency_samples = inner.latency_ring.len();
        let (latency_ns_mean, latency_ns_max) = if latency_samples == 0 {
            (0, 0)
        } else {
            let sum: u128 = inner.latency_ring.iter().map(|s| u128::from(*s)).sum();
            let mean = (sum / latency_samples as u128) as u64;
            let max = inner.latency_ring.iter().copied().max().unwrap_or(0);
            (mean, max)
        };
        // Compute p50 / p99 from the encode ring.
        let (p50_ns, p99_ns) = percentile_50_99(&inner.encode_ring);
        PeerSurfaceSnapshot {
            frames_received: inner.frames_received,
            frames_dropped: inner.frames_dropped,
            keyframes: inner.keyframes,
            latency_ns_mean,
            latency_ns_max,
            latency_samples,
            encode_wallclock_ms_p50: p50_ns / 1_000_000,
            encode_wallclock_ms_p99: p99_ns / 1_000_000,
            chosen_encoder: inner.chosen_encoder,
            capture_path: inner.capture_path,
            bitrate_bytes_per_sec_ewma: inner.bitrate_bytes_per_sec_ewma,
        }
    }

    /// Reset every counter — used by tests reusing a metrics handle across cases; production has no reason to call this.
    #[cfg(test)]
    pub fn reset(&self) {
        let mut inner = self.inner.lock().expect("metrics mutex");
        inner.frames_received = 0;
        inner.frames_dropped = 0;
        inner.keyframes = 0;
        inner.latency_ring.clear();
        inner.latency_head = 0;
        inner.encode_ring.clear();
        inner.encode_head = 0;
        inner.bitrate_bytes_per_sec_ewma = 0;
    }
}

/// Return `(p50_ns, p99_ns)` for a ring of nanosecond samples — copies
/// into a scratch vec for sorting so the caller's ring stays in insertion order.
fn percentile_50_99(ring: &[u64]) -> (u64, u64) {
    if ring.is_empty() {
        return (0, 0);
    }
    let mut sorted = ring.to_vec();
    sorted.sort_unstable();
    let p50_idx = sorted.len() / 2;
    let p99_idx = ((sorted.len() as f64 * 0.99) as usize).min(sorted.len() - 1);
    (sorted[p50_idx], sorted[p99_idx])
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
    fn encode_wallclock_p50_p99_populate() {
        let m = PeerSurfaceMetrics::new();
        // 10 samples: 1ms .. 10ms.
        for i in 1..=10u64 {
            m.record_encode_wallclock(i * 1_000_000);
        }
        let s = m.snapshot();
        // Median of 10 sorted [1..10]ms: p50 index 5 (0-indexed) → 6ms.
        // p99 = ceil(0.99 * 10) = 9 → 10ms.
        assert_eq!(s.encode_wallclock_ms_p50, 6);
        assert_eq!(s.encode_wallclock_ms_p99, 10);
    }

    #[test]
    fn chosen_encoder_and_capture_path_round_trip() {
        let m = PeerSurfaceMetrics::new();
        m.set_chosen_encoder(EncoderKind::VaApi);
        m.set_capture_path(CapturePath::DmabufDirect);
        let s = m.snapshot();
        assert_eq!(s.chosen_encoder, EncoderKind::VaApi);
        assert_eq!(s.capture_path, CapturePath::DmabufDirect);
    }

    #[test]
    fn keyframe_counter_increments() {
        let m = PeerSurfaceMetrics::new();
        m.record_keyframe();
        m.record_keyframe();
        m.record_keyframe();
        let s = m.snapshot();
        assert_eq!(s.keyframes, 3);
    }

    #[test]
    fn bitrate_ewma_smooths_samples() {
        let m = PeerSurfaceMetrics::new();
        m.record_bitrate(1_000_000);
        m.record_bitrate(1_000_000);
        m.record_bitrate(1_000_000);
        let s = m.snapshot();
        // alpha=0.2 EWMA reaches ~488_000 after 3 samples of 1M (200k → 360k → 488k).
        assert!(
            s.bitrate_bytes_per_sec_ewma > 400_000 && s.bitrate_bytes_per_sec_ewma < 600_000,
            "EWMA converged to unexpected value: {}",
            s.bitrate_bytes_per_sec_ewma
        );
    }

    #[test]
    fn percentile_helper_handles_edge_cases() {
        assert_eq!(percentile_50_99(&[]), (0, 0));
        assert_eq!(percentile_50_99(&[42]), (42, 42));
        assert_eq!(percentile_50_99(&[1, 2, 3, 4, 5]), (3, 5));
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
        // Sender 1_000_000 ns ahead of receiver at handshake (sender clock 100_000_000, receiver 101_000_000 → offset = +1_000_000).
        m.set_clock_offset(100_000_000, 101_000_000);
        // wire ts=200_000_000, receiver now=201_500_000 → capture_on_receiver =
        // 200_000_000 + 1_000_000 = 201_000_000; latency = 500_000 ns.
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
        // Frame whose "capture on receiver clock" is in the future (would-be
        // negative latency) — return None instead of a wrong number.
        assert_eq!(m.end_to_end_latency_ns(2_000_000, 500_000), None);
    }
}
