//! Per-share observability counters used by the OSD overlay.

use std::sync::{Arc, Mutex};

use crate::collab::encoder::EncoderKind;
use tilo_lock::MutexRecover as _;

/// Default ring capacity — 128 samples ≈ 2.1 s at 60 fps.
const LATENCY_RING_CAPACITY: usize = 128;

const ENCODE_WALLCLOCK_CAPACITY: usize = 128;

/// Route from a client's `wl_buffer` to the encoder.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum CapturePath {
    #[default]
    ShmReadback,
    DmabufDirect,
}

impl CapturePath {
    pub fn as_str(self) -> &'static str {
        match self {
            CapturePath::ShmReadback => "shm-readback",
            CapturePath::DmabufDirect => "dmabuf-direct",
        }
    }
}

/// Shared observability counters for one receive-side peer surface.
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

/// Point-in-time copy of a peer surface's counters.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct PeerSurfaceSnapshot {
    pub frames_received: u64,
    pub frames_dropped: u64,
    pub keyframes: u64,
    pub latency_ns_mean: u64,
    pub latency_ns_max: u64,
    pub latency_samples: usize,
    pub encode_wallclock_ms_p50: u64,
    pub encode_wallclock_ms_p99: u64,
    pub chosen_encoder: EncoderKind,
    pub capture_path: CapturePath,
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
            chosen_encoder: EncoderKind::RoyaltyFreeSoftware,
            capture_path: CapturePath::default(),
            bitrate_bytes_per_sec_ewma: 0,
        }
    }
}

impl PeerSurfaceMetrics {
    pub fn new() -> Self {
        Self::with_capacity(LATENCY_RING_CAPACITY)
    }

    /// A custom capacity keeps small-sample tests deterministic.
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
                chosen_encoder: EncoderKind::RoyaltyFreeSoftware,
                capture_path: CapturePath::default(),
                bitrate_bytes_per_sec_ewma: 0,
                clock_offset_ns: None,
            })),
        }
    }

    pub fn record_received(&self, latency_ns: u64) {
        let mut inner = self.inner.lock_recover();
        inner.frames_received = inner.frames_received.saturating_add(1);
        if inner.latency_ring.len() < inner.latency_capacity {
            inner.latency_ring.push(latency_ns);
        } else {
            let cap = inner.latency_capacity;
            let head = inner.latency_head;
            if let Some(slot) = inner.latency_ring.get_mut(head) {
                *slot = latency_ns;
            }
            inner.latency_head = head.wrapping_add(1).checked_rem(cap).unwrap_or(0);
        }
    }

    pub fn record_dropped(&self) {
        let mut inner = self.inner.lock_recover();
        inner.frames_dropped = inner.frames_dropped.saturating_add(1);
    }

    pub fn record_encode_wallclock(&self, ns: u64) {
        let mut inner = self.inner.lock_recover();
        if inner.encode_ring.len() < inner.encode_capacity {
            inner.encode_ring.push(ns);
        } else {
            let cap = inner.encode_capacity;
            let head = inner.encode_head;
            if let Some(slot) = inner.encode_ring.get_mut(head) {
                *slot = ns;
            }
            inner.encode_head = head.wrapping_add(1).checked_rem(cap).unwrap_or(0);
        }
    }

    pub fn record_keyframe(&self) {
        let mut inner = self.inner.lock_recover();
        inner.keyframes = inner.keyframes.saturating_add(1);
    }

    /// Update the bitrate EWMA with a fresh sample (bytes/sec); alpha fixed
    /// at 0.2 so recent samples dominate but stay smooth across the ~5s window the OSD renders.
    pub fn record_bitrate(&self, bytes_per_sec: u64) {
        let mut inner = self.inner.lock_recover();
        let prev = inner.bitrate_bytes_per_sec_ewma;
        let alpha = 20u64; // 20% new sample weight
        inner.bitrate_bytes_per_sec_ewma = prev
            .saturating_mul(100u64.saturating_sub(alpha))
            .saturating_add(bytes_per_sec.saturating_mul(alpha))
            .saturating_div(100);
    }

    pub fn set_chosen_encoder(&self, kind: EncoderKind) {
        let mut inner = self.inner.lock_recover();
        inner.chosen_encoder = kind;
    }

    pub fn set_capture_path(&self, path: CapturePath) {
        let mut inner = self.inner.lock_recover();
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
        let mut inner = self.inner.lock_recover();
        if sender_ns_at_send == 0 {
            inner.clock_offset_ns = None;
            return;
        }
        let offset = i128::from(receiver_ns_at_recv).saturating_sub(i128::from(sender_ns_at_send));
        inner.clock_offset_ns = Some(offset);
    }

    /// End-to-end latency in **nanoseconds** for a frame whose wire `ts`
    /// (sender-side monotonic ns) is `wire_ts_ns`, observed at receiver-side monotonic ns `receiver_now_ns`.
    ///
    /// Returns `None` when no clock-offset has been learned yet, or when the
    /// arithmetic would underflow (clock skew against a frame predating the
    /// handshake) — callers skip the measurement rather than push a wrong one to the latency ring.
    pub fn end_to_end_latency_ns(&self, wire_ts_ns: u64, receiver_now_ns: u64) -> Option<u64> {
        let offset = {
            let inner = self.inner.lock_recover();
            inner.clock_offset_ns
        }?;
        // The sender's `ts` on the receiver's clock is `wire_ts_ns + offset` —
        // the "moment of capture" in the receiver's monotonic domain; latency is now minus that.
        // `wire_ts_ns` and `receiver_now_ns` are peer/wall-clock supplied, not bounded by this
        // function, so both steps saturate rather than risk overflowing i128.
        let capture_on_receiver = i128::from(wire_ts_ns).saturating_add(offset);
        let latency = i128::from(receiver_now_ns).saturating_sub(capture_on_receiver);
        if latency < 0 {
            None
        } else {
            // A pathological clock offset can in principle push `latency` above
            // `u64::MAX`; report the clamp rather than silently wrapping to a small,
            // wrong-looking number.
            Some(u64::try_from(latency).unwrap_or(u64::MAX))
        }
    }

    /// End-to-end latency rounded to the nearest millisecond.
    pub fn end_to_end_latency_ms(&self, wire_ts_ns: u64, receiver_now_ns: u64) -> Option<u64> {
        self.end_to_end_latency_ns(wire_ts_ns, receiver_now_ns)
            .map(|ns| ns.saturating_add(500_000).saturating_div(1_000_000))
    }

    /// Current learned clock offset, or `None` before the handshake —
    /// exposed so the OSD can show a huge offset (drifted NTP, unsync'd hosts) alongside the latency histogram.
    pub fn clock_offset_ns(&self) -> Option<i128> {
        self.inner.lock_recover().clock_offset_ns
    }

    pub fn snapshot(&self) -> PeerSurfaceSnapshot {
        let inner = self.inner.lock_recover();
        let latency_samples = inner.latency_ring.len();
        let (latency_ns_mean, latency_ns_max) = if latency_samples == 0 {
            (0, 0)
        } else {
            let sum: u128 = inner.latency_ring.iter().map(|s| u128::from(*s)).sum();
            let mean = u128::try_from(latency_samples)
                .ok()
                .and_then(|count| sum.checked_div(count))
                .and_then(|m| u64::try_from(m).ok())
                .unwrap_or(0);
            let max = inner.latency_ring.iter().copied().max().unwrap_or(0);
            (mean, max)
        };
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

    #[cfg(test)]
    /// # Panics
    ///
    /// Panics if the metrics mutex is poisoned.
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

fn percentile_50_99(ring: &[u64]) -> (u64, u64) {
    if ring.is_empty() {
        return (0, 0);
    }
    let mut sorted = ring.to_vec();
    sorted.sort_unstable();
    let p50_idx = sorted.len() / 2;
    let p99_idx = sorted
        .len()
        .saturating_mul(99)
        .saturating_div(100)
        .min(sorted.len().saturating_sub(1));
    (
        sorted.get(p50_idx).copied().unwrap_or(0),
        sorted.get(p99_idx).copied().unwrap_or(0),
    )
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
    fn metrics_record_arrivals_drops_and_a_wrapping_latency_ring() {
        // Phase `fresh_metrics_snapshot_is_zero`.
        {
            let m = PeerSurfaceMetrics::new();
            let s = m.snapshot();
            assert_eq!(s, PeerSurfaceSnapshot::default());
        }
        // Phase `record_received_increments_and_stores_latency`.
        {
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
        // Phase `record_dropped_does_not_touch_latency`.
        {
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
        // Phase `latency_ring_wraps_at_capacity`.
        {
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
        // Phase `clone_shares_inner_state`.
        {
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
        // Phase `keyframe_counter_increments`.
        {
            let m = PeerSurfaceMetrics::new();
            m.record_keyframe();
            m.record_keyframe();
            m.record_keyframe();
            let s = m.snapshot();
            assert_eq!(s.keyframes, 3);
        }
        // Phase `chosen_encoder_and_capture_path_round_trip`.
        {
            let m = PeerSurfaceMetrics::new();
            m.set_chosen_encoder(EncoderKind::VaApi);
            m.set_capture_path(CapturePath::DmabufDirect);
            let s = m.snapshot();
            assert_eq!(s.chosen_encoder, EncoderKind::VaApi);
            assert_eq!(s.capture_path, CapturePath::DmabufDirect);
        }
    }

    #[test]
    fn encode_and_bitrate_statistics_are_smoothed_percentiles() {
        // Phase `encode_wallclock_p50_p99_populate`.
        {
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
        // Phase `bitrate_ewma_smooths_samples`.
        {
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
        // Phase `percentile_helper_handles_edge_cases`.
        {
            assert_eq!(percentile_50_99(&[]), (0, 0));
            assert_eq!(percentile_50_99(&[42]), (42, 42));
            assert_eq!(percentile_50_99(&[1, 2, 3, 4, 5]), (3, 5));
        }
    }

    #[test]
    #[allow(
        clippy::items_after_statements,
        reason = "a phase with an early return keeps its original body as a nested fn"
    )]
    fn end_to_end_latency_uses_only_a_learned_nonnegative_offset() {
        // Phase `no_clock_offset_returns_none_for_latency`.
        {
            let m = PeerSurfaceMetrics::new();
            assert_eq!(m.end_to_end_latency_ns(1_000, 5_000), None);
            assert_eq!(m.end_to_end_latency_ms(1_000, 5_000), None);
            assert_eq!(m.clock_offset_ns(), None);
        }
        // Phase `clock_offset_zero_ts_clears`.
        {
            let m = PeerSurfaceMetrics::new();
            // First, learn a real offset.
            m.set_clock_offset(1_000, 5_000);
            assert_eq!(m.clock_offset_ns(), Some(4_000));
            // Then a "no field on the wire" event — offset is cleared.
            m.set_clock_offset(0, 5_000);
            assert_eq!(m.clock_offset_ns(), None);
        }
        // Phase `end_to_end_latency_uses_learned_offset`.
        {
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
        // Phase `end_to_end_latency_none_when_negative`.
        fn end_to_end_latency_none_when_negative() {
            let m = PeerSurfaceMetrics::new();
            m.set_clock_offset(1_000_000, 500_000);
            // Frame whose "capture on receiver clock" is in the future (would-be
            // negative latency) — return None instead of a wrong number.
            assert_eq!(m.end_to_end_latency_ns(2_000_000, 500_000), None);
        }
        end_to_end_latency_none_when_negative();
    }
}
