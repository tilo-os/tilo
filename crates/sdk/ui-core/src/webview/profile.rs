//! Lightweight instrumentation for the `WebView` pipeline. Off by default;
//! enabled by setting `TILO_WEBVIEW_PROFILE` in the environment.
//!
//! What it measures, from the engine thread and the widget:
//!  * **frame throughput** — inter-frame intervals → frames/sec,
//!  * **input→next-frame interval** — time from an interactive command
//!    being sent to the next observed engine frame. This is uncorrelated:
//!    the frame may belong to another tab or unrelated animation, and
//!    arrival does not prove that the window displayed it,
//!  * **SHM swizzle cost** — the per-frame BGRA→RGBA copy time,
//!  * **geometry** — each frame's pixel size and each draw's rect, so a
//!    size/rect mismatch (a stretched or mispositioned frame) is visible.
//!
//! A summary prints to stderr every `SUMMARY_EVERY` frames and once
//! more when the process exits. With `TILO_WEBVIEW_PROFILE=trace` the
//! first `TRACE_FRAMES` frames and draws are logged individually — the
//! fast path to diagnosing a visual glitch.
//!
//! All timing uses `crate::time::Instant`; the whole module is a no-op
//! (one relaxed atomic load) when disabled.

use crate::time::{Duration, Instant};
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use tilo_lock::MutexRecover as _;

/// Frames between periodic stderr summaries.
const SUMMARY_EVERY: u64 = 120;
/// Individual frame/draw lines printed at the start in `trace` mode.
const TRACE_FRAMES: u64 = 48;
/// Ring-buffer capacity for the latency / interval samples.
const SAMPLES: usize = 512;

static ENABLED: AtomicBool = AtomicBool::new(false);
static TRACE: AtomicBool = AtomicBool::new(false);
static RESIZE_COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static STATE: OnceLock<Mutex<State>> = OnceLock::new();

/// Read the env once and latch the on/off + trace flags. Called from
/// every hook; the first call resolves it, the rest are a relaxed load.
#[inline]
pub fn enabled() -> bool {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| match std::env::var("TILO_WEBVIEW_PROFILE") {
        Ok(v) if !v.is_empty() && v != "0" => {
            ENABLED.store(true, Ordering::Relaxed);
            if v == "trace" {
                TRACE.store(true, Ordering::Relaxed);
            }
        }
        _ => {}
    });
    ENABLED.load(Ordering::Relaxed)
}

fn state() -> &'static Mutex<State> {
    STATE.get_or_init(|| Mutex::new(State::new()))
}

struct State {
    /// Wall clock the profiler started (first frame).
    started: Option<Instant>,
    /// Timestamp of the previous frame, for inter-frame intervals.
    last_frame: Option<Instant>,
    /// An interactive command was sent at this time and no frame has
    /// arrived since — the next frame closes the latency measurement.
    pending_input: Option<(Instant, &'static str)>,
    frame_count: u64,
    draw_count: u64,
    /// Most recent frame dimensions, to detect alternation (flicker).
    last_dims: Option<(u32, u32)>,
    dims_changes: u64,
    intervals_ms: Ring,
    latency_ms: Ring,
    swizzle_us: Ring,
}

impl State {
    fn new() -> Self {
        Self {
            started: None,
            last_frame: None,
            pending_input: None,
            frame_count: 0,
            draw_count: 0,
            last_dims: None,
            dims_changes: 0,
            intervals_ms: Ring::new(),
            latency_ms: Ring::new(),
            swizzle_us: Ring::new(),
        }
    }
}

/// `kind` is a short static label (`"scroll"`, `"mouse"`, `"key"`). Only
/// the first input after a paint opens a latency window, so a burst
/// reports the leading edge's latency to its first repaint.
#[inline]
pub fn record_input(kind: &'static str) {
    if !enabled() {
        return;
    }
    let mut s = state().lock_recover();
    if s.pending_input.is_none() {
        s.pending_input = Some((crate::time::now(), kind));
    }
}

/// Record a frame the engine just produced: its pixel dimensions and the
/// time spent converting it (SHM BGRA → RGBA).
pub fn record_frame(width: u32, height: u32, swizzle: Duration) {
    if !enabled() {
        return;
    }
    let now = crate::time::now();
    let mut s = state().lock_recover();
    s.started.get_or_insert(now);

    if let Some(prev) = s.last_frame {
        s.intervals_ms
            .push(now.duration_since(prev).as_secs_f64() * 1000.0);
    }
    s.last_frame = Some(now);
    s.swizzle_us.push(swizzle.as_secs_f64() * 1_000_000.0);

    if let Some((sent, kind)) = s.pending_input.take() {
        let ms = now.duration_since(sent).as_secs_f64() * 1000.0;
        s.latency_ms.push(ms);
        if TRACE.load(Ordering::Relaxed) && s.frame_count < TRACE_FRAMES {
            eprintln!("[webview-profile] input({kind})→next-frame (uncorrelated) {ms:.1}ms");
        }
    }

    match s.last_dims {
        Some(d) if d != (width, height) => s.dims_changes = s.dims_changes.saturating_add(1),
        _ => {}
    }
    s.last_dims = Some((width, height));

    s.frame_count = s.frame_count.saturating_add(1);
    let n = s.frame_count;
    if TRACE.load(Ordering::Relaxed) && n <= TRACE_FRAMES {
        eprintln!(
            "[webview-profile] frame #{n} {width}x{height} swizzle {:.0}us",
            swizzle.as_secs_f64() * 1e6
        );
    }
    if n.is_multiple_of(SUMMARY_EVERY) {
        s.print_summary();
    }
}

/// `frame_dims` is `None` for a placeholder (no frame yet). A rect whose
/// aspect ratio diverges from the frame's, or a placeholder appearing
/// after real frames, points at a layout/caching glitch.
pub fn record_draw(rect_w: f64, rect_h: f64, frame_dims: Option<(u32, u32)>) {
    if !enabled() {
        return;
    }
    // Scoped so the mutex guard is dropped before the (I/O) eprintln! below.
    let n = {
        let mut s = state().lock_recover();
        s.draw_count = s.draw_count.saturating_add(1);
        s.draw_count
    };
    if TRACE.load(Ordering::Relaxed) && n <= TRACE_FRAMES {
        match frame_dims {
            Some((w, h)) => {
                eprintln!("[webview-profile] draw #{n} rect {rect_w:.0}x{rect_h:.0} frame {w}x{h}");
            }
            None => eprintln!(
                "[webview-profile] draw #{n} rect {rect_w:.0}x{rect_h:.0} PLACEHOLDER (no frame)"
            ),
        }
    }
}

/// Record physical resize commands so startup traces can distinguish a stale
/// frame from a missing or downgraded device-scale request.
pub fn record_resize(width: u32, height: u32, scale_factor: f32) {
    if !enabled() || !TRACE.load(Ordering::Relaxed) {
        return;
    }
    if let Some(n) = next_resize_trace(&RESIZE_COUNT) {
        eprintln!("[webview-profile] resize #{n} {width}x{height} scale {scale_factor:.2}");
    }
}

fn next_resize_trace(counter: &std::sync::atomic::AtomicU64) -> Option<u64> {
    counter
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
            (n < TRACE_FRAMES).then(|| n.saturating_add(1))
        })
        .ok()
        .map(|n| n.saturating_add(1))
}

#[cfg(test)]
mod resize_trace_tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    #[test]
    fn trace_limit_never_wraps_or_reopens_logging() {
        let counter = AtomicU64::new(0);
        for expected in 1..=TRACE_FRAMES {
            assert_eq!(next_resize_trace(&counter), Some(expected));
        }
        for _ in 0..3 {
            assert_eq!(next_resize_trace(&counter), None);
            assert_eq!(counter.load(Ordering::Relaxed), TRACE_FRAMES);
        }
        let exhausted = AtomicU64::new(u64::MAX);
        assert_eq!(next_resize_trace(&exhausted), None);
        assert_eq!(exhausted.load(Ordering::Relaxed), u64::MAX);
    }
}

impl State {
    fn print_summary(&self) {
        let elapsed = self.started.map_or(0.0, |s| s.elapsed().as_secs_f64());
        #[allow(
            clippy::cast_precision_loss,
            reason = "frame_count is a live-session counter; reaching 2^52 frames \
                      (~14,000 years at 10kHz) is not a real scenario, so the f64 \
                      conversion for an fps estimate stays exact in practice"
        )]
        let fps = if elapsed > 0.0 {
            self.frame_count as f64 / elapsed
        } else {
            0.0
        };
        eprintln!(
            "[webview-profile] === {frames} frames in {elapsed:.1}s ({fps:.1} fps), {draws} draws, {dims} size-changes ===",
            frames = self.frame_count,
            draws = self.draw_count,
            dims = self.dims_changes,
        );
        self.intervals_ms.report("frame interval (ms)");
        self.latency_ms
            .report("input→next-frame, uncorrelated (ms)");
        self.swizzle_us.report("SHM swizzle (us)");
    }
}

/// Fixed-capacity sample buffer with percentile reporting. Overwrites
/// oldest on overflow — the summary reflects a recent window, not the
/// whole run, which is what matters for a live feel.
struct Ring {
    buf: Vec<f64>,
    next: usize,
    len: usize,
}

impl Ring {
    fn new() -> Self {
        Self {
            buf: vec![0.0; SAMPLES],
            next: 0,
            len: 0,
        }
    }

    fn push(&mut self, v: f64) {
        if let Some(slot) = self.buf.get_mut(self.next) {
            *slot = v;
        }
        self.next = self.next.wrapping_add(1) % SAMPLES;
        self.len = self.len.saturating_add(1).min(SAMPLES);
    }

    fn report(&self, label: &str) {
        if self.len == 0 {
            return;
        }
        let mut sorted: Vec<f64> = self.buf.get(..self.len).unwrap_or(&[]).to_vec();
        sorted.sort_by(f64::total_cmp);
        #[allow(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "sorted.len() is at most SAMPLES (512), far inside f64's exact-integer \
                      range, and every call below passes a fixed non-negative percentile \
                      constant"
        )]
        let pct = |p: f64| {
            let idx = (sorted.len() as f64 * p) as usize;
            sorted
                .get(idx.min(sorted.len().saturating_sub(1)))
                .copied()
                .unwrap_or(0.0)
        };
        #[allow(
            clippy::cast_precision_loss,
            reason = "sorted.len() is at most SAMPLES (512), far inside f64's exact-integer range"
        )]
        let mean = sorted.iter().sum::<f64>() / sorted.len() as f64;
        eprintln!(
            "[webview-profile]   {label:<22} n={n} mean {mean:.1} p50 {p50:.1} p95 {p95:.1} p99 {p99:.1} max {max:.1}",
            n = sorted.len(),
            p50 = pct(0.50),
            p95 = pct(0.95),
            p99 = pct(0.99),
            max = sorted.last().copied().unwrap_or(0.0),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ring_wraps_keeps_recent_and_orders_percentiles() {
        // phase: ring_percentiles_are_ordered
        {
            let mut r = Ring::new();
            for i in 0..100 {
                r.push(f64::from(i));
            }
            let mut sorted: Vec<f64> = r.buf[..r.len].to_vec();
            sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
            // p50 of 0..100 sits mid-range, p95 near the top.
            let pct = |p: f64| {
                let len = u32::try_from(sorted.len()).unwrap_or(u32::MAX);
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "p is a fixed non-negative percentile constant, so the \
                          resulting index is always in range"
                )]
                let idx = (f64::from(len) * p) as usize;
                sorted[idx.min(sorted.len() - 1)]
            };
            assert!(pct(0.50) >= 45.0 && pct(0.50) <= 55.0);
            assert!(pct(0.95) >= 90.0);
        }
        // phase: ring_wraps_and_keeps_recent
        {
            let mut r = Ring::new();
            for i in 0..(SAMPLES + 10) {
                let value = u32::try_from(i).unwrap_or(u32::MAX);
                r.push(f64::from(value));
            }
            assert_eq!(r.len, SAMPLES);
            // Oldest ten were overwritten; the minimum retained is >= 10.
            let min = r.buf.iter().copied().fold(f64::INFINITY, f64::min);
            assert!(min >= 10.0);
        }
    }
}
