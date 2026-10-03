//! Frame-loop instrumentation: per-tick phase timings, present pacing and input-to-present latency.
//!
//! One [`FrameSample`] per loop iteration: `period_us` (real cadence), per-phase durations,
//! `painted` (idle ticks do no paint work), `input_latency_us` (input-consumed to first painted
//! submit), and `input_retirement_latency_us` (input-consumed until the presenter retires that
//! exact submitted serial).
//!
//! Entirely env-var driven (`TILO_FRAME_TRACE`, `TILO_FRAME_SUMMARY`, `TILO_FRAME_WARMUP_MS`);
//! disabled, every [`FrameRecorder`] method is a `bool` branch. Production host sessions set a
//! private trace path so the final second before an abrupt reset remains measurable. [`summarize`]
//! is pure — the same function the live binary and unit tests call, so they can't drift.

use std::io::Write;
use std::time::{Duration, Instant};

/// One loop iteration's timings, in microseconds; a `u32` covers 71 minutes, far past where these numbers are interesting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameSample {
    /// Monotonic tick counter from loop start.
    pub seq: u64,
    /// Microseconds from the recorder's start to this tick's beginning.
    pub start_us: u64,
    /// Microseconds since the previous tick began — the loop's actual cadence. Zero for the first sample.
    pub period_us: u32,
    /// Draining backend events and dispatching them into the widget tree.
    pub input_us: u32,
    /// `pump_apps` plus the foreign-tab reconcile and focus sync.
    pub pump_us: u32,
    /// Taffy layout (`relayout`). Zero on a tick that did not paint.
    pub layout_us: u32,
    /// Building the `vello::Scene`. Zero on a tick that did not paint.
    pub scene_us: u32,
    /// Handing the scene to the backend; on a synchronous backend this includes the GPU wait, on a threaded presenter it's the enqueue.
    pub submit_us: u32,
    /// Test-control socket servicing.
    pub testctl_us: u32,
    /// `dispatch_clients` + `flush_clients`.
    pub clients_us: u32,
    /// Time parked in the event loop waiting for work / the next deadline.
    pub sleep_us: u32,
    pub painted: bool,
    /// Whether the submit handed off to another thread rather than completing inline.
    pub present_async: bool,
    /// Set on the first painted tick after an input was consumed: microseconds from consuming it to the submit returning. `None` on every other tick.
    pub input_latency_us: Option<u32>,
    /// Input-to-presenter-retirement for an asynchronously presented frame.
    /// Unlike `input_latency_us`, this cannot be satisfied merely by enqueueing.
    pub input_retirement_latency_us: Option<u32>,
    /// Presentation queue was full while this tick had paint work to do.
    pub backpressured: bool,
    /// Microseconds after the target deadline when this frame was submitted.
    pub deadline_late_us: u32,
    /// What ended the park that closed this tick: the name of the deadline
    /// that expired, or [`WAKE_IO`] when a registered fd became readable
    /// first. An idle desktop that keeps waking is named here.
    pub wake: &'static str,
    /// CPU time the loop thread consumed during this tick, park included (the
    /// park runs event callbacks; parked itself it consumes none). Unlike the
    /// wall-clock timings it leaves out time the thread was descheduled or
    /// blocked, so a host that steals a virtual CPU or a display that holds a
    /// buffer does not show up as compositor work. `None` where the platform
    /// has no per-thread CPU clock.
    pub cpu_us: Option<u32>,
}

/// [`FrameSample::wake`] for a park ended by a readable fd (input, a client, the control socket, a worker ping) rather than by a deadline.
pub const WAKE_IO: &str = "io";

impl FrameSample {
    /// Non-parked time in the interval this sample's `period_us` spans. `period_us` runs from the
    /// *previous* tick's start to this one's, so the park it contains is the previous tick's
    /// (`prev_sleep_us`); subtracting this sample's own sleep instead is an off-by-one that can go negative when this tick's park outlasts the previous one's.
    pub fn work_us(&self, prev_sleep_us: u32) -> u32 {
        self.period_us.saturating_sub(prev_sleep_us)
    }

    /// CSV row matching [`CSV_HEADER`].
    fn write_csv_row(&self, out: &mut impl Write) -> std::io::Result<()> {
        writeln!(
            out,
            "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
            self.seq,
            self.start_us,
            self.period_us,
            self.input_us,
            self.pump_us,
            self.layout_us,
            self.scene_us,
            self.submit_us,
            self.testctl_us,
            self.clients_us,
            self.sleep_us,
            u8::from(self.painted),
            u8::from(self.present_async),
            self.input_latency_us
                .map(|v| v.to_string())
                .unwrap_or_default(),
            self.input_retirement_latency_us
                .map(|v| v.to_string())
                .unwrap_or_default(),
            u8::from(self.backpressured),
            self.deadline_late_us,
            self.wake,
            self.cpu_us.map(|v| v.to_string()).unwrap_or_default(),
        )
    }
}

/// Header line for the `TILO_FRAME_TRACE` CSV, kept next to `FrameSample::write_csv_row` so the two cannot drift.
pub const CSV_HEADER: &str = "seq,start_us,period_us,input_us,pump_us,layout_us,scene_us,\
submit_us,testctl_us,clients_us,sleep_us,painted,present_async,input_latency_us,\
input_retirement_latency_us,backpressured,deadline_late_us,wake,cpu_us";

/// Percentile bundle for one measured quantity, in microseconds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct Percentiles {
    pub count: u64,
    pub min: u32,
    pub p50: u32,
    pub p95: u32,
    pub p99: u32,
    pub max: u32,
    pub mean: u32,
}

/// Compute percentiles over an unsorted slice; empty input yields all zeroes with `count = 0`.
pub fn percentiles(values: &[u32]) -> Percentiles {
    if values.is_empty() {
        return Percentiles::default();
    }
    let mut v = values.to_vec();
    v.sort_unstable();
    // Nearest-rank: the p-th percentile is the ceil(p/100 * n)-th value, 1-indexed — chosen over interpolation since frame times are "a frame this long happened", not a fitted curve.
    let pick = |p: u64| -> u32 {
        let n = v.len() as u64;
        // n >= 1 (checked above), so clamping rank to [1, n] keeps rank - 1 in bounds.
        let rank = p.saturating_mul(n).div_ceil(100).clamp(1, n);
        let idx = usize::try_from(rank.saturating_sub(1)).unwrap_or(usize::MAX);
        v.get(idx).copied().unwrap_or(0)
    };
    let sum: u64 = v.iter().map(|&x| u64::from(x)).sum();
    let len = v.len() as u64;
    Percentiles {
        count: len,
        min: v.first().copied().unwrap_or(0),
        p50: pick(50),
        p95: pick(95),
        p99: pick(99),
        max: v.last().copied().unwrap_or(0),
        mean: u32::try_from(sum.checked_div(len).unwrap_or(0)).unwrap_or(u32::MAX),
    }
}

/// Aggregate view of a run. Serialised to `TILO_FRAME_SUMMARY`.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
pub struct FrameSummary {
    /// Ticks considered (post-warmup).
    pub ticks: u64,
    pub painted: u64,
    /// Wall time spanned by the considered samples, microseconds.
    pub span_us: u64,
    /// Painted frames per second over `span_us`; zero when the span is degenerate.
    pub fps: f64,
    /// Cadence of *every* tick.
    pub tick_period: Percentiles,
    /// Cadence of painted ticks only — the number a display sees.
    pub paint_period: Percentiles,
    /// Non-parked work per tick.
    pub work: Percentiles,
    pub input: Percentiles,
    pub pump: Percentiles,
    pub layout: Percentiles,
    pub scene: Percentiles,
    pub submit: Percentiles,
    pub testctl: Percentiles,
    pub clients: Percentiles,
    pub sleep: Percentiles,
    /// Input-to-submit-return; this is input-to-queue on threaded presenters.
    pub input_latency: Percentiles,
    pub input_retirement_latency: Percentiles,
    pub backpressured_ticks: u64,
    pub deadline_lateness: Percentiles,
    /// Fraction of painted frames whose period was within [`ON_CADENCE_TOLERANCE_US`] of `target_period_us`; meaningless (reported 0.0) when no target was supplied.
    pub on_cadence_ratio: f64,
    /// The refresh interval the loop was aiming at, microseconds.
    pub target_period_us: u32,
    /// Ticks per [`FrameSample::wake`] reason.
    pub wakes: std::collections::BTreeMap<&'static str, u64>,
}

/// How far a painted frame's period may stray from the target refresh interval and still count
/// as "on cadence". 2 ms is ~1/8 of a 60 Hz interval and ~1/3 of a 240 Hz one — tight enough that pacing off a fixed 16 ms timer against a 144 Hz panel fails, loose enough to survive scheduler jitter.
pub const ON_CADENCE_TOLERANCE_US: u32 = 2_000;

/// Reduce samples to a [`FrameSummary`]. Pure: no clock, no I/O, no env. `target_period_us` is the refresh interval the loop was pacing to; 0 disables the cadence ratio.
#[allow(
    clippy::too_many_lines,
    reason = "one pure reduction keeps every summary field derived from the same sample pass"
)]
pub fn summarize(samples: &[FrameSample], target_period_us: u32) -> FrameSummary {
    if samples.is_empty() {
        return FrameSummary::default();
    }
    // The first sample has no predecessor, so its `period_us` is 0 and would drag every percentile down — drop it from the cadence series (not the phase series, which are self-contained).
    let cadence: Vec<u32> = samples
        .iter()
        .filter(|s| s.period_us > 0)
        .map(|s| s.period_us)
        .collect();
    let paint_cadence: Vec<u32> = {
        // Interval between consecutive *painted* frames, not the tick period, since the loop idles between paints.
        let mut out = Vec::new();
        let mut prev: Option<u64> = None;
        for s in samples.iter().filter(|s| s.painted) {
            if let Some(p) = prev {
                out.push(
                    u32::try_from(s.start_us.saturating_sub(p).min(u64::from(u32::MAX)))
                        .unwrap_or(u32::MAX),
                );
            }
            prev = Some(s.start_us);
        }
        out
    };
    let painted = samples.iter().filter(|s| s.painted).count() as u64;
    let span_us = samples
        .last()
        .map_or(0, |s| s.start_us)
        .saturating_sub(samples.first().map_or(0, |s| s.start_us));
    #[allow(
        clippy::cast_precision_loss,
        reason = "display-only fps ratio: painted/span_us are bounded by MAX_RETAINED ticks, nowhere near f64's precision limit"
    )]
    let fps = if span_us > 0 {
        painted as f64 * 1_000_000.0 / span_us as f64
    } else {
        0.0
    };
    #[allow(
        clippy::cast_precision_loss,
        reason = "display-only ratio over a bounded sample count, nowhere near f64's precision limit"
    )]
    let on_cadence_ratio = if target_period_us > 0 && !paint_cadence.is_empty() {
        let hits = paint_cadence
            .iter()
            .filter(|&&p| p.abs_diff(target_period_us) <= ON_CADENCE_TOLERANCE_US)
            .count();
        hits as f64 / paint_cadence.len() as f64
    } else {
        0.0
    };
    let field = |f: fn(&FrameSample) -> u32| -> Percentiles {
        percentiles(&samples.iter().map(f).collect::<Vec<_>>())
    };
    // Phase series are only meaningful on ticks that ran that phase.
    let painted_field = |f: fn(&FrameSample) -> u32| -> Percentiles {
        percentiles(
            &samples
                .iter()
                .filter(|s| s.painted)
                .map(f)
                .collect::<Vec<_>>(),
        )
    };
    FrameSummary {
        ticks: samples.len() as u64,
        painted,
        span_us,
        fps,
        tick_period: percentiles(&cadence),
        paint_period: percentiles(&paint_cadence),
        work: percentiles(
            &samples
                .windows(2)
                .map(|w| match w {
                    [prev, cur] => cur.work_us(prev.sleep_us),
                    _ => 0,
                })
                .collect::<Vec<_>>(),
        ),
        input: field(|s| s.input_us),
        pump: field(|s| s.pump_us),
        layout: painted_field(|s| s.layout_us),
        scene: painted_field(|s| s.scene_us),
        submit: painted_field(|s| s.submit_us),
        testctl: field(|s| s.testctl_us),
        clients: field(|s| s.clients_us),
        sleep: field(|s| s.sleep_us),
        input_latency: percentiles(
            &samples
                .iter()
                .filter_map(|s| s.input_latency_us)
                .collect::<Vec<_>>(),
        ),
        input_retirement_latency: percentiles(
            &samples
                .iter()
                .filter_map(|s| s.input_retirement_latency_us)
                .collect::<Vec<_>>(),
        ),
        backpressured_ticks: samples.iter().filter(|s| s.backpressured).count() as u64,
        deadline_lateness: percentiles(
            &samples
                .iter()
                .filter(|s| s.painted)
                .map(|s| s.deadline_late_us)
                .collect::<Vec<_>>(),
        ),
        on_cadence_ratio,
        target_period_us,
        wakes: samples.iter().filter(|s| !s.wake.is_empty()).fold(
            std::collections::BTreeMap::new(),
            |mut wakes, s| {
                let n = wakes.entry(s.wake).or_insert(0_u64);
                *n = n.saturating_add(1);
                wakes
            },
        ),
    }
}

/// Live recorder wired into the compositor loop. Construct once with [`FrameRecorder::from_env`];
/// call [`FrameRecorder::begin_tick`] at the top of each iteration, the `mark_*` setters as
/// phases complete, and [`FrameRecorder::end_tick`] at the bottom — with no env var enabled, every call is a `bool` check.
pub struct FrameRecorder {
    enabled: bool,
    start: Instant,
    seq: u64,
    prev_start_us: Option<u64>,
    cur: FrameSample,
    /// Samples retained for the summary. Bounded: once [`MAX_RETAINED`] is reached the recorder
    /// stops retaining (the CSV trace, if enabled, keeps streaming) — a run that long has already produced a statistically settled summary.
    samples: Vec<FrameSample>,
    warmup_us: u64,
    trace: Option<std::io::BufWriter<std::fs::File>>,
    /// Flush cadence for crash-durable production traces.  Flushing every row
    /// perturbs the very loop being measured; flushing once per second bounds
    /// hard-reset loss while keeping the hot path buffered.
    next_trace_flush_us: u64,
    trace_path: Option<std::path::PathBuf>,
    trace_rows: usize,
    summary_path: Option<std::path::PathBuf>,
    target_period_us: u32,
    /// When an input was consumed and no painted frame has closed the loop on it yet.
    pending_input: Option<Instant>,
    /// Async frame which carries the earliest outstanding input. One input is
    /// tracked at a time so queue pressure cannot make a newer event hide an
    /// older response.
    pending_retirement: Option<(u64, Instant)>,
    completed_retirement_latency_us: Option<u32>,
    /// The loop thread's CPU clock when the current tick began.
    tick_cpu_start: Option<Duration>,
}

/// Cap on retained samples: 200k ticks is ~55 minutes at 60 Hz and ~7 MB of `FrameSample`.
const MAX_RETAINED: usize = 200_000;

const TRACE_FLUSH_INTERVAL_US: u64 = 1_000_000;
/// About 23 minutes at 144 ticks/s. Rewrites compactly at half this size, so
/// the trace remains a small rolling pre-crash window instead of growing for
/// the whole login. It must not exceed [`MAX_RETAINED`], whose in-memory tail
/// supplies the compacted file.
const MAX_TRACE_ROWS: usize = MAX_RETAINED;

/// Env var naming the per-tick CSV trace path.
pub const TRACE_ENV: &str = "TILO_FRAME_TRACE";
/// Env var naming the JSON summary path.
pub const SUMMARY_ENV: &str = "TILO_FRAME_SUMMARY";
/// Env var overriding the warmup window in milliseconds.
pub const WARMUP_ENV: &str = "TILO_FRAME_WARMUP_MS";
/// Samples younger than this are excluded from the summary.
pub const DEFAULT_WARMUP_MS: u64 = 1_500;

/// Env var asking the compositor to shut down cleanly after N seconds.
pub const RUN_SECONDS_ENV: &str = "TILO_RUN_SECONDS";

/// Parse a run duration from a raw env value. `None` means "run forever" (production default).
/// Pure so parsing is testable without poking the process environment: a mis-specified duration must read as "run forever" (killable) rather than "exit immediately" (a silent empty trace).
pub fn parse_run_seconds(raw: Option<&str>) -> Option<Duration> {
    let v: f64 = raw?.trim().parse().ok()?;
    if v.is_finite() && v > 0.0 {
        Some(Duration::from_secs_f64(v))
    } else {
        None
    }
}

/// [`parse_run_seconds`] against the process environment.
pub fn run_seconds_from_env() -> Option<Duration> {
    parse_run_seconds(std::env::var(RUN_SECONDS_ENV).ok().as_deref())
}

impl FrameRecorder {
    /// A recorder that does nothing; used when no env var asked for instrumentation, and by tests that only exercise the summariser.
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            start: Instant::now(),
            seq: 0,
            prev_start_us: None,
            cur: FrameSample::default(),
            samples: Vec::new(),
            warmup_us: 0,
            trace: None,
            next_trace_flush_us: TRACE_FLUSH_INTERVAL_US,
            trace_path: None,
            trace_rows: 0,
            summary_path: None,
            target_period_us: 0,
            pending_input: None,
            pending_retirement: None,
            completed_retirement_latency_us: None,
            tick_cpu_start: None,
        }
    }

    /// Build from the process environment. Returns [`Self::disabled`] unless `TILO_FRAME_TRACE` or `TILO_FRAME_SUMMARY` is set.
    pub fn from_env() -> Self {
        let trace_path = std::env::var_os(TRACE_ENV);
        let summary_path = std::env::var_os(SUMMARY_ENV);
        if trace_path.is_none() && summary_path.is_none() {
            return Self::disabled();
        }
        let warmup_ms = std::env::var(WARMUP_ENV)
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(DEFAULT_WARMUP_MS);
        let mut opened_trace_path = None;
        let trace = trace_path.and_then(|p| match std::fs::File::create(&p) {
            Ok(mut f) => {
                if let Err(e) = writeln!(f, "{CSV_HEADER}") {
                    eprintln!(
                        "frame-metrics: writing trace header to {}: {e}",
                        p.display()
                    );
                    return None;
                }
                opened_trace_path = Some(std::path::PathBuf::from(p));
                Some(std::io::BufWriter::new(f))
            }
            Err(e) => {
                eprintln!("frame-metrics: cannot create trace {}: {e}", p.display());
                None
            }
        });
        let mut me = Self::disabled();
        me.enabled = true;
        me.warmup_us = warmup_ms.saturating_mul(1_000);
        me.trace = trace;
        me.trace_path = opened_trace_path;
        me.summary_path = summary_path.map(std::path::PathBuf::from);
        me.samples.reserve(4096);
        eprintln!(
            "frame-metrics: recording (warmup {warmup_ms} ms, trace={}, summary={})",
            me.trace.is_some(),
            me.summary_path.is_some()
        );
        me
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Record the refresh interval the loop is pacing to, so the summary can report how many frames landed on it.
    pub fn set_target_period(&mut self, period: Duration) {
        self.target_period_us =
            u32::try_from(period.as_micros().min(u128::from(u32::MAX))).unwrap_or(u32::MAX);
    }

    /// Start a new tick. `now` is the loop's own tick-start instant, so the recorder doesn't take a second clock reading.
    pub fn begin_tick(&mut self, now: Instant) {
        if !self.enabled {
            return;
        }
        let start_us =
            u64::try_from(now.duration_since(self.start).as_micros()).unwrap_or(u64::MAX);
        let period_us = self.prev_start_us.map_or(0, |p| {
            u32::try_from(start_us.saturating_sub(p).min(u64::from(u32::MAX))).unwrap_or(u32::MAX)
        });
        self.prev_start_us = Some(start_us);
        self.cur = FrameSample {
            seq: self.seq,
            start_us,
            period_us,
            ..Default::default()
        };
        self.cur.input_retirement_latency_us = self.completed_retirement_latency_us.take();
        self.tick_cpu_start = thread_cpu_time();
        self.seq = self.seq.saturating_add(1);
    }

    /// Note that an input event was consumed this tick; the first painted frame that follows closes
    /// the latency measurement. Repeated calls before that frame keep the *earliest* input (the pessimistic, honest reading).
    pub fn note_input(&mut self, at: Instant) {
        if !self.enabled {
            return;
        }
        if self.pending_input.is_none() {
            self.pending_input = Some(at);
        }
    }

    /// Age of the oldest input that has not yet reached a submitted frame.
    /// The async presenter attaches this clock to the exact frame serial it
    /// accepts, rather than mistaking queue hand-off for physical retirement.
    pub fn pending_input_age(&self, now: Instant) -> Option<Duration> {
        self.pending_input
            .map(|at| now.saturating_duration_since(at))
    }

    /// Cumulative phase marks: each takes the duration *of that phase*, not a cumulative offset, so callers can compute them however is cheapest at the call site.
    pub fn mark_input(&mut self, d: Duration) {
        self.cur.input_us = us(d);
    }
    pub fn mark_pump(&mut self, d: Duration) {
        self.cur.pump_us = us(d);
    }
    pub fn mark_layout(&mut self, d: Duration) {
        self.cur.layout_us = us(d);
    }
    pub fn mark_scene(&mut self, d: Duration) {
        self.cur.scene_us = us(d);
    }
    pub fn mark_testctl(&mut self, d: Duration) {
        self.cur.testctl_us = us(d);
    }
    pub fn mark_clients(&mut self, d: Duration) {
        self.cur.clients_us = us(d);
    }
    pub fn mark_sleep(&mut self, d: Duration) {
        self.cur.sleep_us = us(d);
    }
    pub fn mark_wake(&mut self, reason: &'static str) {
        self.cur.wake = reason;
    }

    pub fn mark_backpressured(&mut self) {
        if self.enabled {
            self.cur.backpressured = true;
        }
    }

    /// Record the submit phase and close any outstanding input-latency measurement. `async_present` records whether the submit merely queued the frame for another thread.
    pub fn mark_submit(
        &mut self,
        d: Duration,
        async_present: bool,
        done: Instant,
        submitted_serial: Option<u64>,
    ) {
        if !self.enabled {
            return;
        }
        self.cur.submit_us = us(d);
        self.cur.painted = true;
        self.cur.present_async = async_present;
        if let Some(input_at) = self.pending_input.take() {
            self.cur.input_latency_us = Some(
                u32::try_from(
                    done.saturating_duration_since(input_at)
                        .as_micros()
                        .min(u128::from(u32::MAX)),
                )
                .unwrap_or(u32::MAX),
            );
            if async_present {
                if let Some(serial) = submitted_serial {
                    self.pending_retirement = Some((serial, input_at));
                }
            } else {
                self.cur.input_retirement_latency_us = self.cur.input_latency_us;
            }
        }
    }

    pub fn mark_deadline_lateness(&mut self, d: Duration) {
        if self.enabled {
            self.cur.deadline_late_us = us(d);
        }
    }

    /// Close an async input measurement only when the presenter has retired
    /// the exact submitted serial (or a later FIFO serial).
    pub fn note_retirement(&mut self, retired_serial: u64, at: Instant) {
        if !self.enabled {
            return;
        }
        let Some((wanted, input_at)) = self.pending_retirement else {
            return;
        };
        if retired_serial < wanted {
            return;
        }
        self.pending_retirement = None;
        self.completed_retirement_latency_us = Some(
            u32::try_from(
                at.saturating_duration_since(input_at)
                    .as_micros()
                    .min(u128::from(u32::MAX)),
            )
            .unwrap_or(u32::MAX),
        );
    }

    /// Close the tick: stream it to the trace, periodically flush it for
    /// hard-reset diagnosis, and retain it for the optional clean-exit summary.
    pub fn end_tick(&mut self) {
        if !self.enabled {
            return;
        }
        self.cur.cpu_us = self
            .tick_cpu_start
            .zip(thread_cpu_time())
            .map(|(start, end)| us(end.saturating_sub(start)));
        let sample = self.cur;
        if let Some(t) = self.trace.as_mut()
            && let Err(e) = sample.write_csv_row(t)
        {
            eprintln!("frame-metrics: trace write failed ({e}); disabling trace");
            self.trace = None;
        }
        if sample.start_us >= self.next_trace_flush_us {
            if let Some(trace) = self.trace.as_mut()
                && let Err(error) = trace.flush()
            {
                eprintln!("frame-metrics: trace flush failed ({error}); disabling trace");
                self.trace = None;
            }
            self.next_trace_flush_us = sample.start_us.saturating_add(TRACE_FLUSH_INTERVAL_US);
        }
        if sample.start_us >= self.warmup_us && self.samples.len() < MAX_RETAINED {
            self.samples.push(sample);
        }
        self.trace_rows = self
            .trace_rows
            .saturating_add(usize::from(self.trace.is_some()));
        if self.trace_rows >= MAX_TRACE_ROWS {
            self.compact_trace();
        }
    }

    fn compact_trace(&mut self) {
        let (Some(path), Some(mut trace)) = (self.trace_path.clone(), self.trace.take()) else {
            return;
        };
        if let Err(error) = trace.flush() {
            eprintln!("frame-metrics: trace flush failed ({error}); disabling trace");
            return;
        }
        let keep = MAX_TRACE_ROWS / 2;
        let retained = self
            .samples
            .iter()
            .rev()
            .take(keep)
            .copied()
            .collect::<Vec<_>>();
        match std::fs::File::create(&path) {
            Ok(mut file) => {
                if writeln!(file, "{CSV_HEADER}").is_err()
                    || retained
                        .iter()
                        .rev()
                        .try_for_each(|sample| sample.write_csv_row(&mut file))
                        .is_err()
                {
                    eprintln!(
                        "frame-metrics: compacting {} failed; disabling trace",
                        path.display()
                    );
                    return;
                }
                self.trace_rows = retained.len();
                self.trace = Some(std::io::BufWriter::new(file));
            }
            Err(error) => eprintln!(
                "frame-metrics: reopening compacted trace {} failed ({error}); disabling trace",
                path.display()
            ),
        }
    }

    /// Post-warmup samples retained so far.
    pub fn samples(&self) -> &[FrameSample] {
        &self.samples
    }

    /// Summarise everything retained.
    pub fn summary(&self) -> FrameSummary {
        summarize(&self.samples, self.target_period_us)
    }

    /// Flush the trace and write the JSON summary. Called from `Drop`, and directly by the loop's shutdown path (which `process::exit`s past destructors).
    pub fn finish(&mut self) {
        if !self.enabled {
            return;
        }
        if let Some(t) = self.trace.as_mut() {
            let _ = t.flush();
        }
        let Some(path) = self.summary_path.clone() else {
            return;
        };
        let summary = self.summary();
        match serde_json::to_vec_pretty(&summary) {
            Ok(bytes) => {
                if let Err(e) = std::fs::write(&path, bytes) {
                    eprintln!(
                        "frame-metrics: cannot write summary {}: {e}",
                        path.display()
                    );
                } else {
                    eprintln!(
                        "frame-metrics: {} ticks, {} painted, {:.1} fps, \
                         paint-period p50={}us p99={}us, input-to-queue p50={}us p99={}us, \
                         input-to-retirement p50={}us p99={}us → {}",
                        summary.ticks,
                        summary.painted,
                        summary.fps,
                        summary.paint_period.p50,
                        summary.paint_period.p99,
                        summary.input_latency.p50,
                        summary.input_latency.p99,
                        summary.input_retirement_latency.p50,
                        summary.input_retirement_latency.p99,
                        path.display(),
                    );
                }
            }
            Err(e) => eprintln!("frame-metrics: cannot serialise summary: {e}"),
        }
    }
}

impl Drop for FrameRecorder {
    fn drop(&mut self) {
        self.finish();
    }
}

/// CPU time the calling thread has consumed.
#[cfg(unix)]
fn thread_cpu_time() -> Option<Duration> {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `time` is a valid, writable timespec for the duration of the
    // call; an unsupported clock reports an error instead of writing.
    let status = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &raw mut time) };
    if status != 0 {
        return None;
    }
    Some(Duration::new(
        u64::try_from(time.tv_sec).ok()?,
        u32::try_from(time.tv_nsec).ok()?,
    ))
}

#[cfg(not(unix))]
fn thread_cpu_time() -> Option<Duration> {
    None
}

fn us(d: Duration) -> u32 {
    u32::try_from(d.as_micros().min(u128::from(u32::MAX))).unwrap_or(u32::MAX)
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "exact metric boundary fixtures are the behavior under test"
)]
mod tests {
    use super::*;

    fn sample(seq: u64, start_us: u64, period_us: u32, painted: bool) -> FrameSample {
        FrameSample {
            seq,
            start_us,
            period_us,
            painted,
            ..Default::default()
        }
    }

    #[test]
    fn percentiles_use_nearest_rank_and_handle_tiny_series() {
        // Phase `percentiles_of_empty_series_are_zero_with_zero_count`.
        {
            let p = percentiles(&[]);
            assert_eq!(p.count, 0, "empty series must report zero samples");
            assert_eq!(p.max, 0);
        }
        // Phase `percentiles_use_nearest_rank_not_interpolation`.
        {
            // 1..=100. Nearest-rank p50 is the 50th value = 50; p95 = 95; p99 = 99 — an interpolating implementation would give 50.5 etc., so this pins the definition.
            let v: Vec<u32> = (1..=100).collect();
            let p = percentiles(&v);
            assert_eq!(p.count, 100);
            assert_eq!(p.min, 1);
            assert_eq!(p.p50, 50, "p50 must be the 50th of 100 sorted values");
            assert_eq!(p.p95, 95);
            assert_eq!(p.p99, 99);
            assert_eq!(p.max, 100);
            assert_eq!(p.mean, 50, "integer mean of 1..=100 is 5050/100 = 50");
        }
        // Phase `percentiles_handle_single_sample`.
        {
            let p = percentiles(&[42]);
            assert_eq!(
                (p.count, p.min, p.p50, p.p99, p.max),
                (1, 42, 42, 42, 42),
                "a one-element series is its own every percentile"
            );
        }
    }

    #[test]
    fn summary_cadence_counts_only_painted_frames() {
        // Phase `summarize_drops_the_first_sample_from_the_cadence_series`.
        {
            // The first tick has no predecessor, so its period is 0 — letting it into the series would report a bogus min of 0 and drag the p50 down.
            let s = vec![
                sample(0, 0, 0, true),
                sample(1, 16_000, 16_000, true),
                sample(2, 32_000, 16_000, true),
            ];
            let sum = summarize(&s, 16_667);
            assert_eq!(
                sum.tick_period.count, 2,
                "the zero-period first sample must not enter the cadence series"
            );
            assert_eq!(sum.tick_period.min, 16_000);
        }
        // Phase `summarize_paint_period_measures_gaps_between_painted_frames_only`.
        {
            // Ticks at 0/8/16/24 ms, painting only on the even ones. The paint period is 16 ms even though the tick period is 8 ms — the distinction redraw gating relies on.
            let s = vec![
                sample(0, 0, 0, true),
                sample(1, 8_000, 8_000, false),
                sample(2, 16_000, 8_000, true),
                sample(3, 24_000, 8_000, false),
                sample(4, 32_000, 8_000, true),
            ];
            let sum = summarize(&s, 0);
            assert_eq!(sum.painted, 3);
            assert_eq!(sum.tick_period.p50, 8_000);
            assert_eq!(
                sum.paint_period.count, 2,
                "three painted frames give two inter-paint intervals"
            );
            assert_eq!(
                sum.paint_period.p50, 16_000,
                "paint period must measure painted-to-painted, not tick-to-tick"
            );
        }
        // Phase `summarize_on_cadence_ratio_rejects_a_loop_pacing_to_the_wrong_rate`.
        {
            // A loop pinned at 60 Hz (16.6 ms) driving a 144 Hz panel (6.94 ms) must score 0.0: every frame is ~10 ms late.
            let mut s = Vec::new();
            for i in 0..50u64 {
                s.push(sample(i, i * 16_667, 16_667, true));
            }
            let sum144 = summarize(&s, 6_944);
            assert_eq!(
                sum144.on_cadence_ratio, 0.0,
                "a 60 Hz loop must not count as on-cadence for a 144 Hz target"
            );
            // Same samples against a 60 Hz target: every frame is on cadence.
            let sum60 = summarize(&s, 16_667);
            assert!(
                sum60.on_cadence_ratio > 0.99,
                "a 60 Hz loop against a 60 Hz target must be ~100% on cadence, got {}",
                sum60.on_cadence_ratio
            );
        }
        // Phase `summarize_fps_is_painted_frames_over_the_measured_span`.
        {
            // 61 painted ticks spanning exactly 1 s → 61 painted / 1 s. Span is last.start - first.start, so 61 samples 16.667 ms apart span 1.0 s.
            let mut s = Vec::new();
            for i in 0..61u64 {
                s.push(sample(i, i * 16_667, 16_667, true));
            }
            let sum = summarize(&s, 16_667);
            assert_eq!(sum.span_us, 60 * 16_667);
            assert!(
                (sum.fps - 61.0 * 1_000_000.0 / (60.0 * 16_667.0)).abs() < 0.01,
                "fps must be painted/span, got {}",
                sum.fps
            );
        }
        // Phase `summarize_empty_input_is_all_zero`.
        {
            let sum = summarize(&[], 16_667);
            assert_eq!(sum.ticks, 0);
            assert_eq!(sum.fps, 0.0);
        }
    }

    #[test]
    fn summary_work_and_backpressure_subtract_real_parks() {
        // Phase `summarize_reports_backpressure_and_painted_deadline_lateness`.
        {
            let mut idle = sample(0, 0, 0, false);
            idle.backpressured = true;
            idle.deadline_late_us = 99_000;
            let mut first = sample(1, 7_000, 7_000, true);
            first.deadline_late_us = 300;
            let mut second = sample(2, 14_000, 7_000, true);
            second.backpressured = true;
            second.deadline_late_us = 1_200;

            let summary = summarize(&[idle, first, second], 6_944);
            assert_eq!(summary.backpressured_ticks, 2);
            assert_eq!(summary.deadline_lateness.count, 2);
            assert_eq!(summary.deadline_lateness.p50, 300);
            assert_eq!(summary.deadline_lateness.max, 1_200);
        }
        // Phase `summarize_work_subtracts_the_park_that_actually_happened_in_the_period`.
        {
            // `period_us` spans the previous tick's start to this one's, so the park inside it belongs to
            // the *previous* sample: tick 0 works 4 ms then parks 12 ms, tick 1 starts 16 ms later and its period contains tick 0's park; subtracting the sample's *own* sleep instead goes negative when a tick parks longer than its predecessor.
            let mut a = sample(0, 0, 0, true);
            a.sleep_us = 12_000;
            let mut b = sample(1, 16_000, 16_000, true);
            b.sleep_us = 2_000;
            let sum = summarize(&[a, b], 0);
            assert_eq!(
                sum.work.count, 1,
                "work needs a predecessor, so N samples give N-1 work values"
            );
            assert_eq!(
                sum.work.max, 4_000,
                "work must subtract the park that happened inside the period \
                 (the previous tick's), not the park this tick went on to do"
            );
        }
        // Phase `work_never_goes_negative_when_a_tick_parks_longer_than_its_predecessor`.
        {
            // A tick parked 16 ms following one that parked 2 ms: `saturating_sub` on the wrong operand would report a nonsense-but-positive number.
            let mut a = sample(0, 0, 0, true);
            a.sleep_us = 2_000;
            let mut b = sample(1, 5_000, 5_000, true);
            b.sleep_us = 16_000;
            let sum = summarize(&[a, b], 0);
            assert_eq!(
                sum.work.max, 3_000,
                "5ms period containing a 2ms park is 3ms of work"
            );
        }
        // Phase `summarize_layout_and_scene_ignore_ticks_that_did_not_paint`.
        {
            // An idle tick contributes layout=0/scene=0; folding those into the percentiles would make a busy loop look cheap.
            let mut painted = sample(0, 0, 0, true);
            painted.layout_us = 3_000;
            painted.scene_us = 5_000;
            let idle = sample(1, 16_000, 16_000, false);
            let sum = summarize(&[painted, idle], 0);
            assert_eq!(sum.layout.count, 1, "only painted ticks ran layout");
            assert_eq!(sum.layout.p50, 3_000);
            assert_eq!(sum.scene.p50, 5_000);
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn recorder_measures_input_latency_to_the_exact_presented_frame() {
        // Phase `disabled_recorder_retains_nothing`.
        {
            let mut r = FrameRecorder::disabled();
            let t = Instant::now();
            r.begin_tick(t);
            r.mark_scene(Duration::from_millis(5));
            r.mark_submit(Duration::from_millis(3), false, Instant::now(), None);
            r.end_tick();
            assert!(
                r.samples().is_empty(),
                "a disabled recorder must not allocate or retain samples"
            );
            assert!(!r.is_enabled());
        }
        // Phase `recorder_measures_input_latency_from_input_to_submit_completion`.
        {
            // Drive the real recorder (not the summariser) so the note_input → mark_submit handshake is what's under test.
            let mut r = FrameRecorder::disabled();
            r.enabled = true;
            let base = Instant::now();
            r.begin_tick(base);
            r.note_input(base);
            // Submit finishes 7 ms after the input was consumed.
            r.mark_submit(
                Duration::from_millis(2),
                false,
                base + Duration::from_millis(7),
                None,
            );
            r.end_tick();
            let s = r.samples();
            assert_eq!(s.len(), 1);
            let latency = s[0]
                .input_latency_us
                .expect("a painted tick following an input must carry a latency sample");
            assert_eq!(
                latency, 7_000,
                "latency must span input-consumed to submit-returned"
            );
            assert!(
                !s[0].present_async,
                "a synchronous submit must not be flagged async"
            );
        }
        // Phase `recorder_keeps_the_earliest_pending_input_across_idle_ticks`.
        {
            // Input arrives, then two ticks pass without painting; latency must be measured from the *original* input, not the tick that happened to paint.
            let mut r = FrameRecorder::disabled();
            r.enabled = true;
            let base = Instant::now();
            r.begin_tick(base);
            r.note_input(base);
            r.end_tick();
            r.begin_tick(base + Duration::from_millis(10));
            r.note_input(base + Duration::from_millis(10));
            r.end_tick();
            r.begin_tick(base + Duration::from_millis(20));
            r.mark_submit(
                Duration::from_millis(1),
                true,
                base + Duration::from_millis(25),
                Some(7),
            );
            r.end_tick();
            let s = r.samples();
            assert_eq!(s.len(), 3);
            assert_eq!(
                s[0].input_latency_us, None,
                "unpainted ticks carry no latency"
            );
            assert_eq!(s[1].input_latency_us, None);
            assert_eq!(
                s[2].input_latency_us,
                Some(25_000),
                "latency must run from the earliest un-presented input, not the latest"
            );
            assert!(
                s[2].present_async,
                "a queued (threaded) submit must be flagged so readers don't mistake it for photons"
            );
        }
        // Phase `pending_input_age_exposes_the_oldest_unpresented_input`.
        {
            let mut r = FrameRecorder::disabled();
            r.enabled = true;
            let base = Instant::now();
            r.note_input(base);
            r.note_input(base + Duration::from_millis(4));
            assert_eq!(
                r.pending_input_age(base + Duration::from_millis(9)),
                Some(Duration::from_millis(9)),
                "a newer input must not hide the oldest outstanding latency"
            );
        }
        // Phase `async_input_latency_waits_for_the_exact_retired_serial`.
        {
            let mut r = FrameRecorder::disabled();
            r.enabled = true;
            let base = Instant::now();
            r.begin_tick(base);
            r.note_input(base);
            r.mark_submit(
                Duration::from_micros(20),
                true,
                base + Duration::from_millis(1),
                Some(42),
            );
            r.end_tick();

            r.begin_tick(base + Duration::from_millis(2));
            r.note_retirement(41, base + Duration::from_millis(2));
            r.end_tick();
            assert!(
                r.samples()
                    .iter()
                    .all(|s| s.input_retirement_latency_us.is_none()),
                "an older frame must not acknowledge the input-bearing frame"
            );

            r.begin_tick(base + Duration::from_millis(8));
            r.note_retirement(42, base + Duration::from_millis(8));
            r.end_tick();
            r.begin_tick(base + Duration::from_millis(9));
            r.end_tick();
            assert_eq!(
                r.samples()
                    .last()
                    .and_then(|s| s.input_retirement_latency_us),
                Some(8_000)
            );
        }
    }

    #[test]
    fn metrics_cli_and_trace_output_are_well_formed() {
        // Phase `parse_run_seconds_accepts_positive_values_only`.
        assert_eq!(
            super::parse_run_seconds(Some("2.5")),
            Some(Duration::from_millis(2500))
        );
        assert_eq!(
            super::parse_run_seconds(Some(" 10 ")),
            Some(Duration::from_secs(10))
        );
        // Anything that isn't a positive finite number means "run forever" — never "exit immediately", which would hand a harness an empty trace it could mistake for a real result.
        for bad in [
            None,
            Some(""),
            Some("0"),
            Some("-1"),
            Some("nan"),
            Some("inf"),
            Some("abc"),
        ] {
            assert_eq!(
                super::parse_run_seconds(bad),
                None,
                "{bad:?} must mean run-forever, not exit-immediately"
            );
        }
        // Phase `csv_row_column_count_matches_the_header`.
        {
            let mut buf = Vec::new();
            FrameSample {
                seq: 1,
                start_us: 2,
                period_us: 3,
                input_us: 4,
                pump_us: 5,
                layout_us: 6,
                scene_us: 7,
                submit_us: 8,
                testctl_us: 9,
                clients_us: 10,
                sleep_us: 11,
                painted: true,
                present_async: false,
                input_latency_us: Some(12),
                input_retirement_latency_us: Some(13),
                backpressured: true,
                deadline_late_us: 14,
                wake: "cadence",
                cpu_us: Some(15),
            }
            .write_csv_row(&mut buf)
            .expect("write csv row into a Vec");
            let row = String::from_utf8(buf).expect("csv row is utf8");
            assert_eq!(
                row.trim_end().split(',').count(),
                CSV_HEADER.split(',').count(),
                "trace rows must have exactly as many columns as the header:\n  header: {CSV_HEADER}\n  row:    {}",
                row.trim_end()
            );
        }
        // Phase `production_trace_flushes_periodically_without_waiting_for_clean_exit`.
        {
            let dir = tempfile::tempdir().expect("trace tempdir");
            let path = dir.path().join("frame.csv");
            let file = std::fs::File::create(&path).expect("create trace");
            let mut file = file;
            writeln!(file, "{CSV_HEADER}").expect("write trace header");
            let mut recorder = FrameRecorder::disabled();
            recorder.enabled = true;
            recorder.trace = Some(std::io::BufWriter::new(file));
            recorder.trace_path = Some(path.clone());
            let base = Instant::now();
            recorder.begin_tick(base + Duration::from_secs(1));
            recorder.end_tick();

            let bytes = std::fs::read(&path).expect("read flushed trace");
            assert!(
                !bytes.is_empty(),
                "one-second trace must reach disk before Drop"
            );
        }
        // Phase `each_tick_records_the_cpu_its_thread_spent`.
        #[cfg(unix)]
        {
            let mut recorder = FrameRecorder::disabled();
            recorder.enabled = true;
            recorder.begin_tick(Instant::now());
            let spin_from = thread_cpu_time().expect("unix has a per-thread CPU clock");
            let mut acc = 0_u64;
            while thread_cpu_time()
                .is_some_and(|now| now.saturating_sub(spin_from) < Duration::from_millis(5))
            {
                acc = std::hint::black_box(acc.wrapping_add(1));
            }
            std::thread::sleep(Duration::from_millis(30));
            recorder.end_tick();
            let cpu = recorder.samples()[0]
                .cpu_us
                .expect("unix has a per-thread CPU clock");
            assert!(
                (5_000..30_000).contains(&cpu),
                "5 ms of spinning counts as CPU, 30 ms asleep does not: {cpu} us"
            );
        }
    }
}
