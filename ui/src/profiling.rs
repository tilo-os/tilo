//! Frame profiler and headless performance-testing harness for `ui` apps.
//!
//! Every app built on the toolkit runs the same five-pass frame — `sync`,
//! `build_layout`, `apply_layout`, `draw`, `handle_event` (see
//! [`crate::widget::Widget`]). This module measures each of those passes so an
//! app's frame cost can be **read off, not guessed**, and guards against a
//! regression by driving a scripted interaction headless and reporting
//! frame-time statistics.
//!
//! There are three surfaces, and they all share one type ([`FrameTiming`]):
//!
//! 1. **In-app HUD.** [`FrameProfiler`] hooks the [`crate::app::App`] render
//!    loop. Turn it on with `QOS_PROFILE=1`; every app gets a live per-pass
//!    readout and a rolling frame-time graph, with the 60 fps / 16.6 ms budget
//!    line drawn and over-budget frames flagged red. No per-app code — the hook
//!    lives in `App`.
//!
//! 2. **Machine-readable dump.** `QOS_PROFILE_OUT=<path>` appends one record
//!    per frame for offline analysis — JSON-lines by default, CSV when the path
//!    ends in `.csv`. Each frame also emits a `tracing` event under the
//!    `qos::profile` target, so an external tracer (`tracing-subscriber`,
//!    Perfetto via a layer, …) can consume the same numbers with no file.
//!
//! 3. **Headless perf harness.** [`run_scenario`] drives a [`crate::headless::Driver`]
//!    through a scripted [`PerfScenario`] — scroll K times, type a string, click
//!    a target — for N frames and returns a [`PerfReport`] (mean / p50 / p95 /
//!    max per pass, plus how many frames blew the budget). This is what lets a
//!    test measure a fix objectively and fail if a later change slows a pass
//!    down. It runs the app's **real** widget passes with no GPU or display.
//!
//! ## Zero cost when off
//!
//! When neither env var is set, [`FrameProfiler::from_env`] returns `None` and
//! `App` holds no profiler. The render-loop hooks are `Option::map` over that
//! `None`: no clock is read and no allocation happens on the hot path. Timing
//! only begins once profiling is explicitly requested.

use std::collections::VecDeque;
use std::path::Path;
use std::time::{Duration, Instant};

use serde::Serialize;
use vello::Scene;
use vello::kurbo::{Affine, Rect};
use vello::peniko::{Color, Fill};

use crate::text_render::TextRenderer;
use crate::theme::Theme;

/// The per-frame CPU budget for a sustained 60 fps, in microseconds
/// (`1_000_000 / 60`). A frame whose total exceeds this cannot hold 60 fps and
/// is counted as over-budget / dropped by [`PerfReport`].
pub const FRAME_BUDGET_US: u64 = 16_667;

/// One of the toolkit's five frame passes. Used to attribute a measured
/// duration to the right column of the current frame's [`FrameTiming`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pass {
    /// Pull visual state from the CRDT ([`crate::widget::Widget::sync`]).
    Sync,
    /// Build the Taffy tree and solve layout ([`crate::widget::Widget::build_layout`]
    /// plus `compute_layout`).
    BuildLayout,
    /// Push solved rects down into widgets ([`crate::widget::Widget::apply_layout`]).
    ApplyLayout,
    /// Encode the widget tree into a Vello scene ([`crate::widget::Widget::draw`]).
    Draw,
    /// Dispatch a winit event through the tree ([`crate::widget::Widget::handle_event`]).
    HandleEvent,
}

/// Per-pass timings for a single frame, in microseconds, plus the frame index
/// and a monotonic timestamp. This is the atom every surface shares: the HUD
/// graphs it, the dump serialises it, and [`PerfReport`] aggregates it.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct FrameTiming {
    /// Monotonically increasing frame counter, from the profiler's start.
    pub frame: u64,
    /// Microseconds since the profiler started — a monotonic counter, not a
    /// wall clock, so it is immune to clock steps.
    pub t_us: u64,
    pub sync_us: u64,
    pub build_layout_us: u64,
    pub apply_layout_us: u64,
    pub draw_us: u64,
    pub handle_event_us: u64,
    /// Total frame cost. For the live app this is the measured wall time of the
    /// whole frame build (so it includes the small amount of work *between*
    /// passes); for the headless harness, where the passes run contiguously, it
    /// is their sum.
    pub total_us: u64,
}

impl FrameTiming {
    /// Sum of the five measured passes. Used as the total in the headless
    /// harness, where nothing runs between passes.
    fn pass_sum(&self) -> u64 {
        self.sync_us
            + self.build_layout_us
            + self.apply_layout_us
            + self.draw_us
            + self.handle_event_us
    }

    /// Whether this frame exceeded the 60 fps budget.
    pub fn over_budget(&self) -> bool {
        self.total_us > FRAME_BUDGET_US
    }
}

/// Where a machine-readable dump goes and in what shape.
enum DumpFormat {
    /// One JSON object per line.
    Jsonl,
    /// Comma-separated, with a header row.
    Csv,
}

struct DumpWriter {
    file: std::fs::File,
    format: DumpFormat,
    header_written: bool,
}

impl DumpWriter {
    fn open(path: &Path) -> Option<Self> {
        let format = if path.extension().and_then(|e| e.to_str()) == Some("csv") {
            DumpFormat::Csv
        } else {
            DumpFormat::Jsonl
        };
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            Ok(file) => Some(Self {
                file,
                format,
                header_written: false,
            }),
            Err(e) => {
                eprintln!("QOS_PROFILE_OUT: cannot open {}: {e}", path.display());
                None
            }
        }
    }

    fn write(&mut self, t: &FrameTiming) {
        use std::io::Write;
        let _ = match self.format {
            DumpFormat::Jsonl => {
                writeln!(self.file, "{}", serde_json::to_string(t).unwrap_or_default())
            }
            DumpFormat::Csv => {
                if !self.header_written {
                    let _ = writeln!(
                        self.file,
                        "frame,t_us,sync_us,build_layout_us,apply_layout_us,draw_us,handle_event_us,total_us"
                    );
                    self.header_written = true;
                }
                writeln!(
                    self.file,
                    "{},{},{},{},{},{},{},{}",
                    t.frame,
                    t.t_us,
                    t.sync_us,
                    t.build_layout_us,
                    t.apply_layout_us,
                    t.draw_us,
                    t.handle_event_us,
                    t.total_us
                )
            }
        };
    }
}

/// Number of recent frames the HUD keeps for its rolling graph and live stats
/// (4 s at 60 fps).
const HISTORY_CAP: usize = 240;

/// Accumulates per-pass timings for the frame currently being built, keeps a
/// rolling history for the HUD, and fans committed frames out to the dump, the
/// tracer, and (in the headless harness) a capture buffer.
///
/// The [`crate::app::App`] holds one of these behind an `Option`; the headless
/// [`crate::headless::Driver`] holds another for the perf harness. Both feed it
/// the same way: [`record`](Self::record) a pass duration, then
/// [`commit_frame`](Self::commit_frame) once per frame.
pub struct FrameProfiler {
    origin: Instant,
    frame_index: u64,
    /// The in-progress frame — passes accumulate here until commit.
    cur: FrameTiming,
    /// `handle_event` time seen since the last commit. In the live app several
    /// input events land between two redraws; their cost is summed here and
    /// attributed to the next frame.
    pending_handle_event_us: u64,
    history: VecDeque<FrameTiming>,
    hud: bool,
    dump: Option<DumpWriter>,
    /// When `Some`, every committed frame is also appended here — the headless
    /// harness reads this back as the sample set for its [`PerfReport`].
    capture: Option<Vec<FrameTiming>>,
}

impl FrameProfiler {
    fn new(hud: bool, dump: Option<DumpWriter>) -> Self {
        Self {
            origin: Instant::now(),
            frame_index: 0,
            cur: FrameTiming::default(),
            pending_handle_event_us: 0,
            history: VecDeque::with_capacity(HISTORY_CAP),
            hud,
            dump,
            capture: None,
        }
    }

    /// Build a profiler from the environment, or `None` when profiling is off.
    ///
    /// - `QOS_PROFILE` (non-empty) turns on the in-app HUD.
    /// - `QOS_PROFILE_OUT=<path>` appends a per-frame dump — CSV when the path
    ///   ends in `.csv`, JSON-lines otherwise.
    ///
    /// Either variable alone enables profiling. Deliberately **not** gated on
    /// `debug_assertions` (unlike the debug overlay): the point of a profiler is
    /// to measure release builds, which is where frame cost actually matters.
    pub fn from_env() -> Option<Self> {
        let hud = std::env::var_os("QOS_PROFILE").is_some_and(|v| !v.is_empty());
        let out = std::env::var_os("QOS_PROFILE_OUT").filter(|v| !v.is_empty());
        if !hud && out.is_none() {
            return None;
        }
        let dump = out.and_then(|p| DumpWriter::open(Path::new(&p)));
        Some(Self::new(hud, dump))
    }

    /// A profiler that only captures samples into a buffer — no HUD, no dump.
    /// Used by the headless harness.
    pub fn for_capture() -> Self {
        let mut p = Self::new(false, None);
        p.capture = Some(Vec::new());
        p
    }

    /// Whether the on-screen HUD should be drawn this frame.
    pub fn hud_enabled(&self) -> bool {
        self.hud
    }

    /// Attribute `dur` to `pass` of the frame currently being built.
    #[inline]
    pub fn record(&mut self, pass: Pass, dur: Duration) {
        let us = dur.as_micros() as u64;
        match pass {
            Pass::Sync => self.cur.sync_us += us,
            Pass::BuildLayout => self.cur.build_layout_us += us,
            Pass::ApplyLayout => self.cur.apply_layout_us += us,
            Pass::Draw => self.cur.draw_us += us,
            // Input can arrive between frames, so it is banked and folded into
            // the next committed frame.
            Pass::HandleEvent => self.pending_handle_event_us += us,
        }
    }

    /// Convenience for the render-loop hooks: record the elapsed time since
    /// `start`, doing nothing when `start` is `None` (profiling was off when the
    /// pass began). Keeps the call sites in `app.rs` / `headless.rs` to two
    /// lines each.
    #[inline]
    pub fn record_since(&mut self, pass: Pass, start: Option<Instant>) {
        if let Some(t) = start {
            self.record(pass, t.elapsed());
        }
    }

    /// Finalise the current frame with an explicit total (the live app's
    /// measured wall time), publish it, and reset for the next one.
    pub fn commit_frame(&mut self, total_us: u64) {
        self.cur.handle_event_us = std::mem::take(&mut self.pending_handle_event_us);
        self.cur.frame = self.frame_index;
        self.cur.t_us = self.origin.elapsed().as_micros() as u64;
        self.cur.total_us = total_us;
        self.frame_index += 1;

        let sample = self.cur;
        self.history.push_back(sample);
        while self.history.len() > HISTORY_CAP {
            self.history.pop_front();
        }
        if let Some(d) = self.dump.as_mut() {
            d.write(&sample);
        }
        if let Some(c) = self.capture.as_mut() {
            c.push(sample);
        }
        tracing::debug!(
            target: "qos::profile",
            frame = sample.frame,
            t_us = sample.t_us,
            sync_us = sample.sync_us,
            build_layout_us = sample.build_layout_us,
            apply_layout_us = sample.apply_layout_us,
            draw_us = sample.draw_us,
            handle_event_us = sample.handle_event_us,
            total_us = sample.total_us,
            "frame"
        );

        self.cur = FrameTiming::default();
    }

    /// Finalise the current frame with the total taken as the sum of its
    /// passes. Used by the headless harness, where passes run contiguously so
    /// their sum *is* the frame cost.
    pub fn commit_frame_summed(&mut self) {
        // Fold pending input in before summing, so it is counted.
        let total = self.cur.pass_sum() + self.pending_handle_event_us;
        self.commit_frame(total);
    }

    /// The frames captured so far, draining the buffer. Empty when not in
    /// capture mode.
    pub fn take_capture(&mut self) -> Vec<FrameTiming> {
        self.capture.as_mut().map(std::mem::take).unwrap_or_default()
    }

    /// Discard captured frames (e.g. warmup) without stopping capture.
    pub fn clear_capture(&mut self) {
        if let Some(c) = self.capture.as_mut() {
            c.clear();
        }
    }

    /// The rolling history the HUD draws — most recent frame last.
    pub fn history(&self) -> &VecDeque<FrameTiming> {
        &self.history
    }

    /// Draw the profiling HUD into `scene`, in the same logical space and under
    /// the same `transform` the app draws its widgets with, so it is crisp at
    /// any scale. `viewport` is the logical `(width, height)`.
    ///
    /// The HUD is a top-right panel: a text readout (current + windowed mean /
    /// max frame time, an fps estimate, and the per-pass breakdown) above a
    /// rolling bar graph of recent frame totals, with the 16.6 ms budget line
    /// drawn across it and over-budget bars painted red.
    pub fn draw_hud(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        viewport: (f64, f64),
    ) {
        if self.history.is_empty() {
            return;
        }
        let (vw, _vh) = viewport;

        // Panel geometry (logical px), pinned to the top-right corner.
        let pad = 8.0;
        let panel_w = 260.0_f64;
        let graph_h = 56.0_f64;
        let line_h = 14.0_f64;
        let text_rows = 6.0;
        let panel_h = text_rows * line_h + graph_h + pad * 3.0;
        let x0 = (vw - panel_w - 12.0).max(12.0);
        let y0 = 12.0_f64;

        let panel = Rect::new(x0, y0, x0 + panel_w, y0 + panel_h);
        scene.fill(
            Fill::NonZero,
            transform,
            Color::from_rgba8(10, 12, 18, 220),
            None,
            &panel,
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            theme.border_standard,
            None,
            &panel,
        );

        // Windowed stats over the history buffer.
        let n = self.history.len() as u64;
        let last = *self.history.back().unwrap();
        let sum: u64 = self.history.iter().map(|f| f.total_us).sum();
        let mean = sum / n.max(1);
        let max = self.history.iter().map(|f| f.total_us).max().unwrap_or(0);
        let over = self.history.iter().filter(|f| f.over_budget()).count();
        let fps = if last.total_us > 0 {
            1_000_000.0 / last.total_us as f64
        } else {
            0.0
        };

        let ms = |us: u64| us as f64 / 1000.0;
        let mut ty = y0 + pad + 10.0;
        let tx = x0 + pad;
        // `scene` and `text` are passed in per call rather than captured, so
        // this closure holds no borrow across calls and the graph below can
        // reuse `scene` freely.
        let draw_line =
            |scene: &mut Scene, text: &mut TextRenderer, s: &str, color: Color, ty: &mut f64| {
                text.draw_text(
                    scene,
                    s,
                    11.0,
                    color,
                    transform * Affine::translate((tx, *ty)),
                    Some((panel_w - pad * 2.0) as f32),
                );
                *ty += line_h;
            };

        let head_color = if last.over_budget() {
            Color::from_rgba8(255, 90, 90, 255)
        } else {
            theme.status_success
        };
        draw_line(
            scene,
            text,
            &format!("frame {}  {:.1}ms  {:.0}fps", last.frame, ms(last.total_us), fps),
            head_color,
            &mut ty,
        );
        draw_line(
            scene,
            text,
            &format!("mean {:.1}ms  max {:.1}ms  over {}/{}", ms(mean), ms(max), over, n),
            theme.text_primary,
            &mut ty,
        );
        draw_line(
            scene,
            text,
            &format!("sync {:.2}  build {:.2}", ms(last.sync_us), ms(last.build_layout_us)),
            theme.text_dim,
            &mut ty,
        );
        draw_line(
            scene,
            text,
            &format!("apply {:.2}  draw {:.2}", ms(last.apply_layout_us), ms(last.draw_us)),
            theme.text_dim,
            &mut ty,
        );
        draw_line(
            scene,
            text,
            &format!("event {:.2}ms  budget 16.6ms", ms(last.handle_event_us)),
            theme.text_dim,
            &mut ty,
        );

        // Rolling frame-time graph.
        let gx0 = x0 + pad;
        let gx1 = x0 + panel_w - pad;
        let gy1 = y0 + panel_h - pad;
        let gw = gx1 - gx0;

        // Vertical scale: at least 2x budget, so the budget line sits mid-graph
        // and a normal frame reads as a clearly-visible bar. Grow to the 95th
        // percentile of recent frames so a genuinely heavy app still fits — but
        // NOT to the raw `max`: a single cold-start or GC spike would otherwise
        // rescale the whole graph and flatten every normal frame to an invisible
        // sliver. The rare frames above the ceiling clip to a full-height bar,
        // which — being over budget — is already painted red, so nothing is lost.
        let mut totals: Vec<u64> = self.history.iter().map(|f| f.total_us).collect();
        totals.sort_unstable();
        let p95 = percentile(&totals, 95.0);
        let scale_top = (FRAME_BUDGET_US * 2).max(p95) as f64;
        let y_of = |us: u64| gy1 - (us as f64 / scale_top).min(1.0) * graph_h;

        // Budget line.
        let budget_y = y_of(FRAME_BUDGET_US);
        let mut bl = vello::kurbo::BezPath::new();
        bl.move_to((gx0, budget_y));
        bl.line_to((gx1, budget_y));
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            Color::from_rgba8(120, 130, 150, 200),
            None,
            &bl,
        );

        // One bar per recent frame, newest at the right edge.
        let shown = self.history.len().min(HISTORY_CAP);
        let bar_w = (gw / shown as f64).max(1.0);
        for (i, f) in self.history.iter().rev().take(shown).enumerate() {
            let bx1 = gx1 - i as f64 * bar_w;
            let bx0 = (bx1 - bar_w + 0.5).max(gx0);
            let by0 = y_of(f.total_us);
            let bar = Rect::new(bx0, by0, bx1, gy1);
            let color = if f.over_budget() {
                Color::from_rgba8(230, 70, 70, 255)
            } else {
                theme.accent_primary
            };
            scene.fill(Fill::NonZero, transform, color, None, &bar);
        }
    }
}

// ---------------------------------------------------------------------------
// Statistics & report
// ---------------------------------------------------------------------------

/// Summary statistics for one column of measurements (a single pass, or the
/// total), in microseconds.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct StatSummary {
    pub count: usize,
    pub mean_us: u64,
    pub p50_us: u64,
    pub p95_us: u64,
    pub min_us: u64,
    pub max_us: u64,
}

impl StatSummary {
    /// Compute the summary of `values` (unsorted; this sorts a copy).
    pub fn of(values: &[u64]) -> Self {
        if values.is_empty() {
            return Self::default();
        }
        let mut v = values.to_vec();
        v.sort_unstable();
        let count = v.len();
        let sum: u128 = v.iter().map(|&x| x as u128).sum();
        let mean_us = (sum / count as u128) as u64;
        Self {
            count,
            mean_us,
            p50_us: percentile(&v, 50.0),
            p95_us: percentile(&v, 95.0),
            min_us: v[0],
            max_us: v[count - 1],
        }
    }
}

/// Nearest-rank percentile over an already-sorted slice (NIST method): the
/// value at 1-indexed rank `ceil(p/100 * n)`, clamped into range.
fn percentile(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let n = sorted.len();
    let rank = ((p / 100.0) * n as f64).ceil() as usize;
    let idx = rank.saturating_sub(1).min(n - 1);
    sorted[idx]
}

/// The result of running a [`PerfScenario`]: per-pass and total statistics over
/// every frame the scenario produced, plus how many blew the frame budget.
#[derive(Debug, Clone, Serialize)]
pub struct PerfReport {
    pub scenario: String,
    pub frames: usize,
    pub budget_us: u64,
    /// Frames whose total exceeded the budget (i.e. would drop below 60 fps).
    pub over_budget: usize,
    pub over_budget_pct: f64,
    pub total: StatSummary,
    pub sync: StatSummary,
    pub build_layout: StatSummary,
    pub apply_layout: StatSummary,
    pub draw: StatSummary,
    pub handle_event: StatSummary,
    /// The raw per-frame samples, in order. Skipped when serialising the report
    /// itself so a JSON summary stays small; dump them separately if wanted.
    #[serde(skip)]
    pub samples: Vec<FrameTiming>,
}

impl PerfReport {
    /// Aggregate `samples` into a report against `budget_us`.
    pub fn from_samples(scenario: &str, samples: Vec<FrameTiming>, budget_us: u64) -> Self {
        let frames = samples.len();
        let col = |f: fn(&FrameTiming) -> u64| {
            StatSummary::of(&samples.iter().map(f).collect::<Vec<_>>())
        };
        let over_budget = samples.iter().filter(|f| f.total_us > budget_us).count();
        let over_budget_pct = if frames > 0 {
            over_budget as f64 * 100.0 / frames as f64
        } else {
            0.0
        };
        Self {
            scenario: scenario.to_string(),
            frames,
            budget_us,
            over_budget,
            over_budget_pct,
            total: col(|f| f.total_us),
            sync: col(|f| f.sync_us),
            build_layout: col(|f| f.build_layout_us),
            apply_layout: col(|f| f.apply_layout_us),
            draw: col(|f| f.draw_us),
            handle_event: col(|f| f.handle_event_us),
            samples,
        }
    }

    /// A compact human-readable table, microseconds shown as milliseconds.
    pub fn to_table(&self) -> String {
        use std::fmt::Write;
        let ms = |us: u64| us as f64 / 1000.0;
        let mut s = String::new();
        let _ = writeln!(
            s,
            "perf scenario {:?}: {} frames, budget {:.1}ms, over-budget {} ({:.1}%)",
            self.scenario,
            self.frames,
            ms(self.budget_us),
            self.over_budget,
            self.over_budget_pct
        );
        let _ = writeln!(
            s,
            "  {:<13} {:>8} {:>8} {:>8} {:>8} {:>8}",
            "pass (ms)", "mean", "p50", "p95", "min", "max"
        );
        let row = |s: &mut String, name: &str, st: &StatSummary| {
            let _ = writeln!(
                s,
                "  {:<13} {:>8.3} {:>8.3} {:>8.3} {:>8.3} {:>8.3}",
                name,
                ms(st.mean_us),
                ms(st.p50_us),
                ms(st.p95_us),
                ms(st.min_us),
                ms(st.max_us)
            );
        };
        row(&mut s, "sync", &self.sync);
        row(&mut s, "build_layout", &self.build_layout);
        row(&mut s, "apply_layout", &self.apply_layout);
        row(&mut s, "draw", &self.draw);
        row(&mut s, "handle_event", &self.handle_event);
        row(&mut s, "TOTAL", &self.total);
        s
    }

    /// The report as pretty JSON (without the raw samples).
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }
}

impl std::fmt::Display for PerfReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_table())
    }
}

// ---------------------------------------------------------------------------
// Scenario harness
// ---------------------------------------------------------------------------

/// One step in a [`PerfScenario`]. Each step drives the headless driver and
/// produces one or more measured frames.
#[derive(Debug, Clone)]
pub enum PerfStep {
    /// Re-render one frame with no input — measures steady-state redraw cost.
    Frame,
    /// Re-render `n` frames with no input.
    Frames(usize),
    /// Send a vertical wheel scroll `count` times (`dy` per notch; negative
    /// scrolls the content down). Each notch is one measured frame.
    Scroll { dy: f64, count: usize },
    /// Type a string, one key press/release pair per character.
    Type(String),
    /// Press and release one named key (`Enter`, `Tab`, arrows, or a character).
    Key(String),
    /// Click the widget addressed by `test_id` / path.
    ClickNamed(String),
    /// Click at raw logical coordinates.
    ClickAt(f64, f64),
}

/// A named, scripted interaction to run headless and time. Reusable across any
/// app: build a [`crate::headless::Driver`] over the app's real root + state,
/// then call [`run_scenario`].
#[derive(Debug, Clone)]
pub struct PerfScenario {
    pub name: String,
    pub viewport: (u32, u32),
    /// Frames to run and discard before measuring, so first-frame allocation /
    /// shaping-cache warmup doesn't skew the stats.
    pub warmup: usize,
    pub steps: Vec<PerfStep>,
}

impl PerfScenario {
    /// A scroll scenario: warm up, then scroll `count` notches. The most common
    /// shape — it is what surfaces list/grid draw and layout cost.
    pub fn scroll(name: impl Into<String>, count: usize) -> Self {
        Self {
            name: name.into(),
            viewport: crate::headless::DEFAULT_VIEWPORT,
            warmup: 3,
            steps: vec![PerfStep::Scroll { dy: -3.0, count }],
        }
    }

    /// A typing scenario: warm up, then type `text`.
    pub fn typing(name: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            viewport: crate::headless::DEFAULT_VIEWPORT,
            warmup: 3,
            steps: vec![PerfStep::Type(text.into())],
        }
    }

    /// Override the viewport the scenario lays out at.
    pub fn with_viewport(mut self, w: u32, h: u32) -> Self {
        self.viewport = (w, h);
        self
    }
}

/// Run `scenario` against an already-built driver and return the report.
///
/// The driver runs the app's real five passes — `sync`, `build_layout`,
/// `apply_layout`, `draw`, `handle_event` — with no GPU or display, so the
/// timings are of the production code path, not a mock. Warmup frames are run
/// and discarded before measurement begins.
pub fn run_scenario(driver: &mut crate::headless::Driver, scenario: &PerfScenario) -> PerfReport {
    driver.begin_profiling();
    for _ in 0..scenario.warmup {
        driver.profile_frame();
    }
    driver.clear_profile_samples();

    for step in &scenario.steps {
        match step {
            PerfStep::Frame => driver.profile_frame(),
            PerfStep::Frames(n) => {
                for _ in 0..*n {
                    driver.profile_frame();
                }
            }
            PerfStep::Scroll { dy, count } => {
                for _ in 0..*count {
                    driver.scroll(0.0, *dy);
                }
            }
            PerfStep::Type(t) => driver.type_text(t),
            PerfStep::Key(k) => driver.key(k),
            PerfStep::ClickNamed(name) => {
                let _ = driver.click_named(name);
            }
            PerfStep::ClickAt(x, y) => driver.click_at(*x, *y),
        }
    }

    let samples = driver.take_profile_samples();
    PerfReport::from_samples(&scenario.name, samples, FRAME_BUDGET_US)
}

/// Build a headless driver over `state` + `root` and run `scenario` against it.
/// The one-call entry point for an app author or a test: no driver bookkeeping.
pub fn run_scenario_on(
    state: crate::state::State,
    root: Box<dyn crate::widget::Widget>,
    scenario: &PerfScenario,
) -> PerfReport {
    let mut driver = crate::headless::Driver::new(state, root, scenario.viewport);
    run_scenario(&mut driver, scenario)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_is_nearest_rank() {
        let v: Vec<u64> = (1..=100).collect();
        assert_eq!(percentile(&v, 50.0), 50);
        assert_eq!(percentile(&v, 95.0), 95);
        assert_eq!(percentile(&v, 0.0), 1);
        assert_eq!(percentile(&v, 100.0), 100);
    }

    #[test]
    fn stat_summary_captures_spread() {
        let s = StatSummary::of(&[10, 20, 30, 40, 50]);
        assert_eq!(s.count, 5);
        assert_eq!(s.mean_us, 30);
        assert_eq!(s.p50_us, 30);
        assert_eq!(s.min_us, 10);
        assert_eq!(s.max_us, 50);
    }

    #[test]
    fn empty_stats_are_zero_not_a_panic() {
        let s = StatSummary::of(&[]);
        assert_eq!(s.count, 0);
        assert_eq!(s.max_us, 0);
    }

    /// A profiler records each pass into the right column and commits it, and
    /// the over-budget flag tracks the budget.
    #[test]
    fn profiler_records_passes_and_commits() {
        let mut p = FrameProfiler::for_capture();
        p.record(Pass::Sync, Duration::from_micros(100));
        p.record(Pass::BuildLayout, Duration::from_micros(200));
        p.record(Pass::ApplyLayout, Duration::from_micros(50));
        p.record(Pass::Draw, Duration::from_micros(400));
        p.record(Pass::HandleEvent, Duration::from_micros(80));
        p.commit_frame_summed();

        let samples = p.take_capture();
        assert_eq!(samples.len(), 1);
        let f = samples[0];
        assert_eq!(f.sync_us, 100);
        assert_eq!(f.build_layout_us, 200);
        assert_eq!(f.apply_layout_us, 50);
        assert_eq!(f.draw_us, 400);
        assert_eq!(f.handle_event_us, 80);
        assert_eq!(f.total_us, 830);
        assert!(!f.over_budget());
    }

    #[test]
    fn over_budget_frames_are_counted_in_the_report() {
        let mk = |total: u64| FrameTiming {
            total_us: total,
            draw_us: total,
            ..Default::default()
        };
        let samples = vec![mk(5_000), mk(20_000), mk(8_000), mk(30_000)];
        let r = PerfReport::from_samples("t", samples, FRAME_BUDGET_US);
        assert_eq!(r.frames, 4);
        assert_eq!(r.over_budget, 2); // 20ms and 30ms exceed 16.6ms
        assert_eq!(r.draw.max_us, 30_000);
    }
}
