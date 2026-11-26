//! Lightweight instrumentation for the `WebView` pipeline. Off by default; enabled by
//! setting `TILO_WEBVIEW_PROFILE` in the environment. What it measures, from the engine
//! thread and the widget.

use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use tilo_clock::time::{Duration, Instant};
use tilo_lock::MutexRecover as _;
use tilo_sort::CompactSort as _;

const SUMMARY_EVERY: u64 = 120;
const TRACE_FRAMES: u64 = 48;
const SAMPLES: usize = 512;

static ENABLED: AtomicBool = AtomicBool::new(false);
static TRACE: AtomicBool = AtomicBool::new(false);
static RESIZE_COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static STATE: OnceLock<Mutex<State>> = OnceLock::new();

/// Read the env once and latch the on/off + trace flags.
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
    started: Option<Instant>,
    last_frame: Option<Instant>,
    pending_input: Option<(Instant, &'static str)>,
    frame_count: u64,
    draw_count: u64,
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

/// `kind` is a short static label (`"scroll"`, `"mouse"`, `"key"`).
#[inline]
pub fn record_input(kind: &'static str) {
    if !enabled() {
        return;
    }
    let mut s = state().lock_recover();
    if s.pending_input.is_none() {
        s.pending_input = Some((tilo_clock::now(), kind));
    }
}

/// Record a frame the engine just produced: its pixel dimensions and the time spent
/// converting it (SHM BGRA → RGBA).
pub fn record_frame(width: u32, height: u32, swizzle: Duration) {
    if !enabled() {
        return;
    }
    let now = tilo_clock::now();
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

/// `frame_dims` is `None` for a placeholder (no frame yet).
pub fn record_draw(rect_w: f64, rect_h: f64, frame_dims: Option<(u32, u32)>) {
    if !enabled() {
        return;
    }
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

/// Record physical resize commands so startup traces can distinguish a stale frame from a
/// missing or downgraded device-scale request.
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

/// Fixed-capacity sample buffer with percentile reporting.
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
        sorted.compact_sort_by(f64::total_cmp);
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
