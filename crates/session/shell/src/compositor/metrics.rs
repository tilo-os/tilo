//! Frame-loop instrumentation: per-tick phase timings, present pacing and input-to-present latency.
//!
//! One [`FrameSample`] per loop iteration: `period_us` (real cadence), per-phase durations,
//! `painted` (idle ticks do no paint work), `input_latency_us` (input-consumed to first painted
//! submit), and `input_retirement_latency_us` (input-consumed until the presenter retires that
//! exact submitted serial).
//!
//! Entirely env-var driven (`TILO_FRAME_TRACE`, `TILO_FRAME_SUMMARY`, `TILO_FRAME_WARMUP_MS`);
//! `TILO_FRAME_CPU_TRACE` adds a bounded numeric-only companion with phase CPU and clock anchors.
//! disabled, every [`FrameRecorder`] method is a `bool` branch. Production host sessions set a
//! private trace path so the final second before an abrupt reset remains measurable. [`summarize`]
//! is pure — the same function the live binary and unit tests call, so they can't drift.

pub(crate) mod cpu_trace;
pub(crate) mod profile;
mod trace_writer;
pub(crate) use cpu_trace::{CpuPhase, CpuSnapshot};
use trace_writer::TraceWriter;

use std::io::Write;
use std::time::{Duration, Instant};
use tilo_lock::MutexRecover as _;

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
    /// Microseconds this frame was submitted after the later of its target
    /// deadline and the moment it was first wanted ([`DemandOnset`]). The
    /// frame clock's deadline does not move while nothing is presented, so
    /// against the deadline alone the first frame after an idle park was
    /// "late" by the whole park.
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
    /// The longest named step of the input phase and how long it took, so a
    /// tick whose `input_us` stalls names what it stalled in. Empty when the
    /// phase had no steps.
    pub input_step: &'static str,
    pub input_step_us: u32,
    /// Every reason that held on a painted tick, as [`paint_reason`] bits;
    /// zero on a tick that did not paint. An idle desktop that keeps
    /// repainting is named here.
    pub paint_reasons: u16,
    /// The apps whose surfaces committed a new frame since the previous
    /// painted tick, `+`-joined; empty when none did or nothing painted.
    pub paint_clients: &'static str,
    /// The part of [`Self::cpu_us`] the loop thread spent answering the
    /// test-control socket. A harness's request lands in whichever tick is
    /// running when it arrives, so what a tick cost the session itself is
    /// `cpu_us` less this. `None` where `cpu_us` is.
    pub testctl_cpu_us: Option<u32>,
    /// On a painted tick that animated, who asked for the motion: chrome
    /// sources by name, content widgets by path, `transition` for a
    /// compositor transition's frame run, `resting-frame` for the one frame
    /// owed after motion ended; `+`-joined, empty when nothing animated. A
    /// desktop that keeps animating while idle is attributed here.
    pub paint_animations: &'static str,
}

/// [`FrameSample::paint_reasons`] bits: why the compositor decided a tick had
/// something new to show.
pub mod paint_reason {
    /// The session's first frame.
    pub const FORCE: u16 = 1 << 0;
    /// Backend (input or output) events were drained.
    pub const INPUT: u16 = 1 << 1;
    /// Input moved compositor-owned pixels (the cursor).
    pub const CURSOR: u16 = 1 << 2;
    pub const SESSION: u16 = 1 << 3;
    /// The chrome's revision moved.
    pub const REVISION: u16 = 1 << 4;
    /// A chrome or widget animation is running, or owes its resting frame.
    pub const ANIMATION: u16 = 1 << 5;
    /// A compositor transition asked for its run of frames (the overview's
    /// fade, a workspace switch).
    pub const TRANSITION: u16 = 1 << 6;
    /// A hosted app or a client surface changed what the widget tree shows.
    pub const APPS: u16 = 1 << 7;
    pub const PEERS: u16 = 1 << 8;
    pub const PRESENTATION: u16 = 1 << 9;
    pub const CAPTURE: u16 = 1 << 10;
    pub const KEEPALIVE: u16 = 1 << 11;
    pub const LOCK_CLOCK: u16 = 1 << 12;
    pub const LOCK_ANIMATION: u16 = 1 << 13;
    pub const LOCK_OSD: u16 = 1 << 14;
    /// A change an earlier tick counted still waited for its frame.
    pub const OWED: u16 = 1 << 15;

    /// Bit and CSV name, in bit order.
    pub const NAMES: [(u16, &str); 16] = [
        (FORCE, "force"),
        (INPUT, "input"),
        (CURSOR, "cursor"),
        (SESSION, "session"),
        (REVISION, "revision"),
        (ANIMATION, "animation"),
        (TRANSITION, "transition"),
        (APPS, "apps"),
        (PEERS, "peers"),
        (PRESENTATION, "presentation"),
        (CAPTURE, "capture"),
        (KEEPALIVE, "keepalive"),
        (LOCK_CLOCK, "lock-clock"),
        (LOCK_ANIMATION, "lock-animation"),
        (LOCK_OSD, "lock-osd"),
        (OWED, "owed"),
    ];

    /// `+`-joined names of the set bits, in bit order.
    pub fn names(bits: u16) -> String {
        NAMES
            .iter()
            .filter(|(bit, _)| (bits & *bit) != 0)
            .map(|(_, name)| *name)
            .collect::<Vec<_>>()
            .join("+")
    }
}

/// When the frame the loop is about to paint was first wanted: the start of
/// the first tick that had something new to show after a tick that had
/// nothing. Lateness is measured from the later of this and the frame
/// clock's deadline, so a park with nothing to paint is not charged to the
/// frame that ends it, while a frame wanted on time and submitted late still
/// is. The clock itself is not touched.
#[derive(Clone, Copy, Debug, Default)]
pub struct DemandOnset {
    since: Option<Instant>,
}

impl DemandOnset {
    /// Record one tick: whether it had something new to show, and when it began.
    pub fn note_tick(&mut self, wanted: bool, at: Instant) {
        self.since = if wanted {
            Some(self.since.unwrap_or(at))
        } else {
            None
        };
    }

    /// How late a frame submitted at `submitted` is against `deadline`.
    pub fn lateness(&self, submitted: Instant, deadline: Instant) -> Duration {
        let from = self.since.map_or(deadline, |since| since.max(deadline));
        submitted.saturating_duration_since(from)
    }

    /// The wanted frame went out; the next one is wanted from its own tick.
    pub fn painted(&mut self) {
        self.since = None;
    }
}

/// Interned `+`-joined app ids for [`FrameSample::paint_clients`]. Bounded:
/// app ids come from clients, so past [`MAX_CLIENT_LABELS`] distinct labels
/// every new one reads as [`OTHER_CLIENTS`] instead of leaking more.
pub fn intern_clients(label: &str) -> &'static str {
    static LABELS: InternedLabels = InternedLabels::new();
    LABELS.intern(label)
}

/// Interned labels for [`FrameSample::paint_animations`], bounded the same
/// way as [`intern_clients`] and counted apart from it.
pub fn intern_animations(label: &str) -> &'static str {
    static LABELS: InternedLabels = InternedLabels::new();
    LABELS.intern(label)
}

/// A bounded set of leaked trace labels, so a row can carry a `&'static str`.
struct InternedLabels(
    std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<&'static str>>>,
);

impl InternedLabels {
    const fn new() -> Self {
        Self(std::sync::OnceLock::new())
    }

    fn intern(&self, label: &str) -> &'static str {
        if label.is_empty() {
            return "";
        }
        let mut labels = self.0.get_or_init(Default::default).lock_recover();
        if let Some(known) = labels.get(label).copied() {
            return known;
        }
        if labels.len() >= MAX_CLIENT_LABELS {
            return OTHER_CLIENTS;
        }
        let leaked: &'static str = Box::leak(label.to_owned().into_boxed_str());
        labels.insert(leaked);
        leaked
    }
}

/// Cap on distinct labels of each interned trace column.
pub const MAX_CLIENT_LABELS: usize = 256;
/// The label past [`MAX_CLIENT_LABELS`].
pub const OTHER_CLIENTS: &str = "(other)";

/// The slowest of a phase's consecutive named steps. Each [`Self::step`]
/// closes the step that began at the previous one; two clock reads per step,
/// cheap enough to run on every tick.
#[derive(Clone, Copy, Debug)]
pub struct SlowestStep {
    last: Instant,
    name: &'static str,
    took: Duration,
}

impl SlowestStep {
    pub fn starting_at(at: Instant) -> Self {
        Self {
            last: at,
            name: "",
            took: Duration::ZERO,
        }
    }

    /// Close the step named `name`, which ran from the previous step (or the
    /// start) until now.
    pub fn step(&mut self, name: &'static str) {
        self.step_at(name, Instant::now());
    }

    fn step_at(&mut self, name: &'static str, now: Instant) {
        let took = now.saturating_duration_since(self.last);
        if took > self.took || self.name.is_empty() {
            self.name = name;
            self.took = took;
        }
        self.last = now;
    }

    pub fn slowest(&self) -> (&'static str, Duration) {
        (self.name, self.took)
    }
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
            "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
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
            self.input_step,
            self.input_step_us,
            paint_reason::names(self.paint_reasons),
            self.paint_clients,
            self.testctl_cpu_us
                .map(|v| v.to_string())
                .unwrap_or_default(),
            self.paint_animations,
        )
    }
}

/// Header line for the `TILO_FRAME_TRACE` CSV, kept next to `FrameSample::write_csv_row` so the two cannot drift.
pub const CSV_HEADER: &str = "seq,start_us,period_us,input_us,pump_us,layout_us,scene_us,\
submit_us,testctl_us,clients_us,sleep_us,painted,present_async,input_latency_us,\
input_retirement_latency_us,backpressured,deadline_late_us,wake,cpu_us,input_step,input_step_us,\
paint_reasons,paint_clients,testctl_cpu_us,paint_animations";

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
    /// Recording this tick: asked for by the environment, or by a profile.
    enabled: bool,
    /// Asked for by the environment (`TILO_FRAME_TRACE` and friends).
    env_enabled: bool,
    /// A [`profile`] is recording this tick.
    profiling: bool,
    start: Instant,
    seq: u64,
    prev_start_us: Option<u64>,
    cur: FrameSample,
    /// Samples retained for the summary. Bounded: once [`MAX_RETAINED`] is reached the recorder
    /// stops retaining (the CSV trace, if enabled, keeps streaming) — a run that long has already produced a statistically settled summary.
    samples: Vec<FrameSample>,
    warmup_us: u64,
    trace: Option<TraceWriter>,
    /// Flush cadence for crash-durable production traces.  Flushing every row
    /// perturbs the very loop being measured; flushing once per second bounds
    /// hard-reset loss while keeping the hot path buffered.
    next_trace_flush_us: u64,
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
    cpu_trace: Option<cpu_trace::CpuTrace>,
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
            env_enabled: false,
            profiling: false,
            start: Instant::now(),
            seq: 0,
            prev_start_us: None,
            cur: FrameSample::default(),
            samples: Vec::new(),
            warmup_us: 0,
            trace: None,
            next_trace_flush_us: TRACE_FLUSH_INTERVAL_US,
            trace_rows: 0,
            summary_path: None,
            target_period_us: 0,
            pending_input: None,
            pending_retirement: None,
            completed_retirement_latency_us: None,
            tick_cpu_start: None,
            cpu_trace: None,
        }
    }

    /// Build from the process environment. Returns [`Self::disabled`] unless `TILO_FRAME_TRACE` or `TILO_FRAME_SUMMARY` is set.
    pub fn from_env() -> Self {
        let trace_path = std::env::var_os(TRACE_ENV);
        let summary_path = std::env::var_os(SUMMARY_ENV);
        let cpu_path = std::env::var_os(cpu_trace::ENV);
        if trace_path.is_none() && summary_path.is_none() && cpu_path.is_none() {
            return Self::disabled();
        }
        let warmup_ms = std::env::var(WARMUP_ENV)
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(DEFAULT_WARMUP_MS);
        let trace = trace_path.and_then(|p| match std::fs::File::create(&p) {
            Ok(mut f) => {
                if let Err(e) = writeln!(f, "{CSV_HEADER}") {
                    eprintln!(
                        "frame-metrics: writing trace header to {}: {e}",
                        p.display()
                    );
                    return None;
                }
                let path = std::path::PathBuf::from(p);
                Some(TraceWriter::to_file(f, path))
            }
            Err(e) => {
                eprintln!("frame-metrics: cannot create trace {}: {e}", p.display());
                None
            }
        });
        let mut me = Self::disabled();
        me.enabled = true;
        me.env_enabled = true;
        me.cpu_trace = cpu_path.and_then(|path| {
            match cpu_trace::CpuTrace::open(std::path::Path::new(&path)) {
                Ok(trace) => Some(trace),
                Err(error) => {
                    eprintln!("frame-metrics: cannot open CPU trace ({error})");
                    None
                }
            }
        });
        me.warmup_us = warmup_ms.saturating_mul(1_000);
        me.trace = trace;
        me.summary_path = summary_path.map(std::path::PathBuf::from);
        me.samples.reserve(4096);
        eprintln!(
            "frame-metrics: recording (warmup {warmup_ms} ms, trace={}, summary={}, cpu_trace={})",
            me.trace.is_some(),
            me.summary_path.is_some(),
            me.cpu_trace.is_some()
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
        // The admin can start a profile at any time; while none records and
        // the environment asked for nothing, this load is the whole cost.
        let profiling = profile::active();
        if profiling != self.profiling {
            self.profiling = profiling;
            if !self.env_enabled {
                // A profile's recording begins and ends without carrying an
                // input measurement across the gap.
                self.prev_start_us = None;
                self.pending_input = None;
                self.pending_retirement = None;
                self.completed_retirement_latency_us = None;
            }
        }
        self.enabled = self.env_enabled || profiling;
        if !self.enabled {
            return;
        }
        if profiling {
            profile::begin_tick(self.seq);
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
        if let Some(trace) = self.cpu_trace.as_mut() {
            trace.begin_tick(now);
        }
        self.seq = self.seq.saturating_add(1);
    }

    /// Snapshot phase CPU only for an explicitly requested CPU companion trace.
    pub(crate) fn cpu_snapshot(&self) -> Option<CpuSnapshot> {
        self.cpu_trace
            .as_ref()
            .and_then(cpu_trace::CpuTrace::snapshot)
    }

    pub(crate) fn mark_cpu(&mut self, phase: CpuPhase, start: Option<CpuSnapshot>) {
        if let Some(trace) = self.cpu_trace.as_mut() {
            trace.mark(phase, start);
        }
    }

    pub(crate) fn mark_paint_wall(&mut self, wall: Duration) {
        if let Some(trace) = self.cpu_trace.as_mut() {
            trace.mark_paint_wall(wall);
        }
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

    /// Close the pending input measurement without a frame: the loop is
    /// going idle having handled it, with nothing on screen to change (a key
    /// release, a press nothing reacts to). Its latency ends here; left
    /// pending, it would be charged to the next painted frame, however much
    /// later and whichever input that frame answers.
    pub fn settle_unpainted_input(&mut self) {
        self.pending_input = None;
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
    pub fn mark_input_step(&mut self, step: SlowestStep) {
        let (name, took) = step.slowest();
        self.cur.input_step = name;
        self.cur.input_step_us = us(took);
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
    /// The loop thread's CPU clock now, for [`Self::mark_testctl_cpu`];
    /// `None` while nothing is recorded.
    pub fn thread_cpu(&self) -> Option<Duration> {
        if !self.enabled {
            return None;
        }
        thread_cpu_time()
    }
    /// The thread CPU the test-control socket took since `start`, read
    /// with [`Self::thread_cpu`] before it was serviced.
    pub fn mark_testctl_cpu(&mut self, start: Option<Duration>) {
        self.cur.testctl_cpu_us = start
            .zip(thread_cpu_time())
            .map(|(start, end)| us(end.saturating_sub(start)));
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
    /// Why this tick paints and whose commits it carries; see
    /// [`FrameSample::paint_reasons`].
    pub fn mark_paint_reasons(&mut self, reasons: u16, clients: &'static str) {
        if self.enabled {
            self.cur.paint_reasons = reasons;
            self.cur.paint_clients = clients;
        }
    }

    /// Who asked for this painted tick's motion; see
    /// [`FrameSample::paint_animations`].
    pub fn mark_paint_animations(&mut self, owners: &'static str) {
        if self.enabled {
            self.cur.paint_animations = owners;
        }
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
        if let Some(trace) = self.cpu_trace.as_mut() {
            trace.end_tick(sample);
        }
        if let Some(t) = self.trace.as_mut()
            && let Err(e) = sample.write_csv_row(t)
        {
            eprintln!("frame-metrics: trace write failed ({e}); disabling trace");
            self.trace = None;
        }
        // An idle session may finish only its startup ticks, all before the
        // periodic deadline, and then park indefinitely. Publish the first
        // completed row while the recorder is alive rather than leaving only
        // its header until another input or clean shutdown arrives.
        if self.trace.is_some()
            && (self.trace_rows == 0 || sample.start_us >= self.next_trace_flush_us)
        {
            if let Some(trace) = self.trace.as_mut()
                && let Err(error) = trace.flush()
            {
                eprintln!("frame-metrics: trace flush failed ({error}); disabling trace");
                self.trace = None;
            }
            self.next_trace_flush_us = sample.start_us.saturating_add(TRACE_FLUSH_INTERVAL_US);
        }
        if self.profiling {
            profile::record_tick(&sample, self.target_period_us);
        }
        if self.env_enabled
            && sample.start_us >= self.warmup_us
            && self.samples.len() < MAX_RETAINED
        {
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
        let Some(mut trace) = self.trace.take() else {
            return;
        };
        let keep = MAX_TRACE_ROWS / 2;
        let retained = self
            .samples
            .iter()
            .rev()
            .take(keep)
            .copied()
            .collect::<Vec<_>>();
        let mut bytes = Vec::new();
        let formatted = writeln!(bytes, "{CSV_HEADER}").and_then(|()| {
            retained
                .iter()
                .rev()
                .try_for_each(|sample| sample.write_csv_row(&mut bytes))
        });
        // The writer thread recreates the file; this thread only formats.
        if formatted.is_err() || trace.replace(bytes).is_err() {
            eprintln!("frame-metrics: compacting the trace failed; disabling trace");
            return;
        }
        self.trace_rows = retained.len();
        self.trace = Some(trace);
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
        if self.profiling {
            profile::shutdown();
            self.profiling = false;
        }
        if let Some(trace) = self.cpu_trace.as_mut() {
            trace.finish();
        }
        if !self.env_enabled {
            return;
        }
        if let Some(t) = self.trace.as_mut() {
            let _ = t.close();
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

/// `CLOCK_MONOTONIC` now, in nanoseconds: the clock input events and boot
/// measurements are stamped with, so the session log can place a frame on it.
pub(crate) fn monotonic_time_ns() -> Option<u128> {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: the out pointer refers to a live timespec for the entire call.
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &raw mut now) } != 0 {
        return None;
    }
    u128::try_from(now.tv_sec)
        .ok()?
        .checked_mul(1_000_000_000)?
        .checked_add(u128::try_from(now.tv_nsec).ok()?)
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

    /// The trace file once it holds `lines` lines: the rows reach it from the
    /// writer thread, not from the call that ended the tick.
    pub(super) fn wait_for_lines(path: &std::path::Path, lines: usize) -> String {
        let mut text = String::new();
        tilo_test_env::wait_until("the trace writer to catch up", || {
            text = std::fs::read_to_string(path).unwrap_or_default();
            text.lines().count() >= lines
        });
        text
    }

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
    fn a_frame_after_an_idle_park_is_late_only_by_its_own_work() {
        let deadline = Instant::now();
        let mut onset = DemandOnset::default();
        // A widget deadline ended a 300 ms park with nothing to paint before it.
        onset.note_tick(false, deadline + Duration::from_millis(10));
        let woke = deadline + Duration::from_millis(300);
        onset.note_tick(true, woke);
        let submitted = woke + Duration::from_millis(4);
        assert_eq!(
            onset.lateness(submitted, deadline),
            Duration::from_millis(4),
            "the park before the frame was wanted is not the frame's lateness"
        );
        // The stale deadline alone would have charged the whole park.
        assert_eq!(
            submitted.saturating_duration_since(deadline),
            Duration::from_millis(304)
        );
    }

    #[test]
    fn a_frame_wanted_on_time_and_submitted_late_is_still_late() {
        let start = Instant::now();
        let mut onset = DemandOnset::default();
        onset.note_tick(true, start);
        // Held across ticks (off cadence, a full presenter): still the same frame.
        onset.note_tick(true, start + Duration::from_millis(8));
        let deadline = start + Duration::from_millis(16);
        let submitted = start + Duration::from_millis(60);
        assert_eq!(
            onset.lateness(submitted, deadline),
            Duration::from_millis(44),
            "lateness after the deadline is kept when the frame was wanted before it"
        );
        onset.painted();
        onset.note_tick(true, start + Duration::from_millis(100));
        assert_eq!(
            onset.lateness(start + Duration::from_millis(101), deadline),
            Duration::from_millis(1),
            "after a paint the next frame is wanted from its own tick"
        );
        // With no onset recorded the deadline alone is the reference.
        assert_eq!(
            DemandOnset::default().lateness(submitted, deadline),
            Duration::from_millis(44)
        );
    }

    #[test]
    fn paint_reasons_name_every_set_bit_and_clients_stay_bounded() {
        assert_eq!(paint_reason::names(0), "");
        assert_eq!(
            paint_reason::names(paint_reason::APPS | paint_reason::FORCE | paint_reason::LOCK_OSD),
            "force+apps+lock-osd"
        );
        let all = paint_reason::NAMES
            .iter()
            .fold(0, |bits, (bit, _)| bits | bit);
        assert_eq!(all.count_ones(), 16, "the 16 bits are distinct");
        assert_eq!(intern_clients(""), "");
        let first = intern_clients("so.tilo.notes");
        assert!(
            std::ptr::eq(first, intern_clients("so.tilo.notes")),
            "interned once"
        );
        for n in 0..MAX_CLIENT_LABELS {
            let _ = intern_clients(&format!("bounded.{n}"));
        }
        assert_eq!(intern_clients("one.more.than.allowed"), OTHER_CLIENTS);
        assert_eq!(intern_clients("so.tilo.notes"), "so.tilo.notes");
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
            r.env_enabled = true;
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
            r.env_enabled = true;
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
        // Phase `an_input_settled_without_a_frame_is_not_charged_to_the_next_one`.
        {
            // A key release paints nothing and the loop parks; seconds later
            // another key paints. That frame answers the later key only.
            let mut r = FrameRecorder::disabled();
            r.enabled = true;
            r.env_enabled = true;
            let base = Instant::now();
            r.begin_tick(base);
            r.note_input(base);
            r.settle_unpainted_input();
            r.end_tick();
            assert_eq!(r.pending_input_age(base + Duration::from_secs(7)), None);
            let later = base + Duration::from_millis(7_500);
            r.begin_tick(later);
            r.note_input(later);
            r.mark_submit(
                Duration::from_millis(1),
                false,
                later + Duration::from_micros(2_400),
                None,
            );
            r.end_tick();
            let s = r.samples();
            assert_eq!(s[0].input_latency_us, None);
            assert_eq!(
                s[1].input_latency_us,
                Some(2_400),
                "the frame's latency must run from its own input, not from one settled before it"
            );
        }
        // Phase `pending_input_age_exposes_the_oldest_unpresented_input`.
        {
            let mut r = FrameRecorder::disabled();
            r.enabled = true;
            r.env_enabled = true;
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
            r.env_enabled = true;
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
    fn the_input_phase_names_its_slowest_step() {
        let base = Instant::now();
        let mut step = SlowestStep::starting_at(base);
        step.step_at("poll", base + Duration::from_millis(2));
        step.step_at("persist", base + Duration::from_millis(90));
        step.step_at("flush", base + Duration::from_millis(91));
        assert_eq!(step.slowest(), ("persist", Duration::from_millis(88)));
        let mut recorder = FrameRecorder::disabled();
        recorder.enabled = true;
        recorder.env_enabled = true;
        recorder.begin_tick(base);
        recorder.mark_input_step(step);
        recorder.end_tick();
        let sample = recorder.samples()[0];
        assert_eq!(
            (sample.input_step, sample.input_step_us),
            ("persist", 88_000)
        );
    }

    #[test]
    fn csv_row_column_count_matches_the_header() {
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
            input_step: "events",
            input_step_us: 16,
            paint_reasons: paint_reason::APPS | paint_reason::REVISION,
            paint_clients: "so.tilo.file-explorer",
            testctl_cpu_us: Some(17),
            paint_animations: "notification+content/DesktopSurface/TileHost",
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
        // Phase `production_trace_flushes_periodically_without_waiting_for_clean_exit`.
        {
            let dir = tempfile::tempdir().expect("trace tempdir");
            let path = dir.path().join("frame.csv");
            let file = std::fs::File::create(&path).expect("create trace");
            let mut file = file;
            writeln!(file, "{CSV_HEADER}").expect("write trace header");
            let mut recorder = FrameRecorder::disabled();
            recorder.enabled = true;
            recorder.env_enabled = true;
            recorder.trace = Some(TraceWriter::to_file(file, path.clone()));
            let base = Instant::now();
            recorder.begin_tick(base + Duration::from_secs(1));
            recorder.end_tick();

            let text = tests::wait_for_lines(&path, 2);
            assert_eq!(
                text.lines().count(),
                2,
                "one-second trace must include its sample, not only its header, before Drop"
            );
        }
        // Phase `each_tick_records_the_cpu_its_thread_spent`.
        #[cfg(unix)]
        {
            let mut recorder = FrameRecorder::disabled();
            recorder.enabled = true;
            recorder.env_enabled = true;
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

#[cfg(test)]
mod startup_trace_tests {
    use super::{CSV_HEADER, FrameRecorder, TraceWriter};
    use std::io::Write;

    #[test]
    fn first_completed_trace_row_is_visible_without_later_activity() {
        let dir = tempfile::tempdir().expect("trace tempdir");
        let path = dir.path().join("frame.csv");
        let mut file = std::fs::File::create(&path).expect("create trace");
        writeln!(file, "{CSV_HEADER}").expect("write trace header");
        let mut recorder = FrameRecorder::disabled();
        recorder.enabled = true;
        recorder.env_enabled = true;
        recorder.trace = Some(TraceWriter::to_file(file, path.clone()));

        // Real end_tick path, before its one-second periodic flush deadline.
        recorder.begin_tick(recorder.start);
        recorder.mark_submit(
            std::time::Duration::from_micros(17),
            false,
            recorder.start,
            None,
        );
        recorder.end_tick();

        let text = super::tests::wait_for_lines(&path, 2);
        let lines = text.lines().collect::<Vec<_>>();
        assert_eq!(
            lines.len(),
            2,
            "startup sample must be readable before recorder Drop"
        );
        assert_eq!(lines[0], CSV_HEADER);
        let row = lines[1].split(',').collect::<Vec<_>>();
        assert_eq!(row.len(), CSV_HEADER.split(',').count());
        assert_eq!(row[0], "0");
        assert_eq!(row[1], "0");
        assert_eq!(row[7], "17");
        assert_eq!(row[11], "1");
    }
}
