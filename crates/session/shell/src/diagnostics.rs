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
        {
            let m = PeerSurfaceMetrics::new();
            let s = m.snapshot();
            assert_eq!(s, PeerSurfaceSnapshot::default());
        }
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
        {
            let m = PeerSurfaceMetrics::new();
            m.record_keyframe();
            m.record_keyframe();
            m.record_keyframe();
            let s = m.snapshot();
            assert_eq!(s.keyframes, 3);
        }
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
        {
            assert_eq!(percentile_50_99(&[]), (0, 0));
            assert_eq!(percentile_50_99(&[42]), (42, 42));
            assert_eq!(percentile_50_99(&[1, 2, 3, 4, 5]), (3, 5));
        }
    }
}
