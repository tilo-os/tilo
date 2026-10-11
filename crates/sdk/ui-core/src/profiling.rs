//! Frame profiler and headless performance-testing harness for `ui` apps.
//!
//! Measures each of the toolkit's five frame passes (`sync`, `build_layout`,
//! `apply_layout`, `draw`, `handle_event` — see [`crate::widget::Widget`]) so
//! frame cost can be **read off, not guessed**. Three surfaces share one type
//! ([`FrameTiming`]): an in-app HUD (`TILO_PROFILE=1`, hooked into
//! [`crate::app::App`]'s render loop with no per-app code), a machine-readable
//! dump (`TILO_PROFILE_OUT=<path>`, JSON-lines or CSV, plus a `tracing` event
//! under `tilo::profile` per frame), and a headless perf harness
//! ([`run_scenario`]) that drives a scripted [`PerfScenario`] through the
//! app's **real** widget passes with no GPU or display and returns a
//! [`PerfReport`] a test can assert on.
//!
//! When neither env var is set, [`FrameProfiler::from_env`] returns `None`
//! and the render-loop hooks are `Option::map` over that: no clock read, no
//! allocation, until profiling is explicitly requested.
//!
//! Live-frame diagnostics also attribute resource availability, recycled-buffer
//! waiting, and presence overlays. Waiting is wall time, not active CPU work;
//! none of these fields measure GPU completion or display presentation.

#[cfg(test)]
pub(crate) mod frame_probe;
pub(crate) mod resource_trace;

use crate::num::{f32_of_f64, f64_of_u64, f64_of_usize, usize_of_f64};
use crate::time::{Duration, Instant};
use std::collections::VecDeque;
use std::path::Path;

use serde::Serialize;
use vello::Scene;
use vello::kurbo::{Affine, Rect};
use vello::peniko::{Color, Fill};

use crate::text_render::TextRenderer;
use crate::theme::Theme;

/// Rounded 60 Hz CPU-work budget. Exceeding it is a CPU diagnostic, not
/// evidence of an actual dropped display frame.
pub const FRAME_BUDGET_US: u64 = 16_667;

/// Optional logical desktop position for live profiling with several apps.
/// Kept in the shared host so benchmarks never need per-app windowing code.
#[cfg(feature = "native")]
pub(crate) fn profile_window_position(value: &str) -> Option<(i32, i32)> {
    let (x, y) = value.split_once(',')?;
    Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
}

#[cfg(test)]
mod window_position_tests {
    #[test]
    fn parses_signed_coordinates_and_rejects_malformed_positions() {
        assert_eq!(super::profile_window_position(" -20, 40"), Some((-20, 40)));
        for value in ["", "20", "20,40,60", "NaN,40", "2147483648,0"] {
            assert_eq!(super::profile_window_position(value), None);
        }
    }
}

/// Attributes a measured duration to the right column of the current
/// frame's [`FrameTiming`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pass {
    Sync,
    /// Includes `compute_layout`.
    BuildLayout,
    ApplyLayout,
    Draw,
    HandleEvent,
    /// Resource discovery and control availability immediately before drawing.
    ResourceAvailability,
    /// CPU wait for recycled scene and damage buffers from the render worker.
    ScenePoolWait,
    /// Legacy cursors and remote semantic/action presence after widget drawing.
    PresenceOverlay,
}

/// This is the atom every surface shares: the HUD graphs it, the dump
/// serialises it, and [`PerfReport`] aggregates it.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct FrameTiming {
    pub frame: u64,
    /// A monotonic counter, not a wall clock, so it is immune to clock steps.
    pub t_us: u64,
    pub sync_us: u64,
    pub build_layout_us: u64,
    pub apply_layout_us: u64,
    pub draw_us: u64,
    pub handle_event_us: u64,
    pub resource_availability_us: u64,
    pub scene_pool_wait_us: u64,
    pub presence_overlay_us: u64,
    /// For the live app this is the measured wall time of the whole frame
    /// build (so it includes work *between* passes); for the headless
    /// harness, where passes run contiguously, it is their sum.
    pub total_us: u64,
}

impl FrameTiming {
    /// Used as the total in the headless harness, where nothing runs
    /// between passes.
    fn pass_sum(&self) -> u64 {
        self.sync_us
            .saturating_add(self.build_layout_us)
            .saturating_add(self.apply_layout_us)
            .saturating_add(self.draw_us)
            .saturating_add(self.handle_event_us)
            .saturating_add(self.resource_availability_us)
            .saturating_add(self.scene_pool_wait_us)
            .saturating_add(self.presence_overlay_us)
    }

    pub fn over_budget(&self) -> bool {
        self.total_us > FRAME_BUDGET_US
    }
}

enum DumpFormat {
    Jsonl,
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
                eprintln!("TILO_PROFILE_OUT: cannot open {}: {e}", path.display());
                None
            }
        }
    }

    fn write(&mut self, t: &FrameTiming) {
        use std::io::Write;
        let _ = match self.format {
            DumpFormat::Jsonl => {
                writeln!(
                    self.file,
                    "{}",
                    serde_json::to_string(t).unwrap_or_default()
                )
            }
            DumpFormat::Csv => {
                if !self.header_written {
                    let _ = writeln!(
                        self.file,
                        "frame,t_us,sync_us,build_layout_us,apply_layout_us,draw_us,handle_event_us,total_us,resource_availability_us,scene_pool_wait_us,presence_overlay_us"
                    );
                    self.header_written = true;
                }
                writeln!(
                    self.file,
                    "{},{},{},{},{},{},{},{},{},{},{}",
                    t.frame,
                    t.t_us,
                    t.sync_us,
                    t.build_layout_us,
                    t.apply_layout_us,
                    t.draw_us,
                    t.handle_event_us,
                    t.total_us,
                    t.resource_availability_us,
                    t.scene_pool_wait_us,
                    t.presence_overlay_us
                )
            }
        };
    }
}

/// 4 s at 60 fps.
const HISTORY_CAP: usize = 240;

/// The [`crate::app::App`] holds one of these behind an `Option`; the
/// headless [`crate::headless::Driver`] holds another for the perf harness.
/// Both feed it the same way: [`record`](Self::record) a pass duration, then
/// [`commit_frame`](Self::commit_frame) once per frame.
pub struct FrameProfiler {
    origin: Instant,
    frame_index: u64,
    /// The in-progress frame — passes accumulate here until commit.
    cur: FrameTiming,
    /// In the live app several input events land between two redraws;
    /// their cost is summed here and attributed to the next frame.
    pending_handle_event_us: u64,
    history: VecDeque<FrameTiming>,
    hud: bool,
    dump: Option<DumpWriter>,
    /// When `Some`, every committed frame is also appended here — the
    /// headless harness reads this back as its [`PerfReport`] sample set.
    capture: Option<Vec<FrameTiming>>,
}

/// `Duration::as_micros` returns `u128`; frame passes and elapsed profiler
/// time never approach `u64::MAX` microseconds (~584,942 years), so this
/// saturates rather than truncates in practice.
fn saturating_u64(value: u128) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

/// Fixed geometry for one [`FrameProfiler::draw_hud`] call, computed once
/// and shared by the text and graph sections.
struct HudLayout {
    x0: f64,
    y0: f64,
    pad: f64,
    panel_w: f64,
    panel_h: f64,
    graph_h: f64,
    line_h: f64,
}

impl FrameProfiler {
    fn new(hud: bool, dump: Option<DumpWriter>) -> Self {
        Self {
            origin: crate::time::now(),
            frame_index: 0,
            cur: FrameTiming::default(),
            pending_handle_event_us: 0,
            history: VecDeque::with_capacity(HISTORY_CAP),
            hud,
            dump,
            capture: None,
        }
    }

    /// `TILO_PROFILE` (non-empty) turns on the in-app HUD;
    /// `TILO_PROFILE_OUT=<path>` appends a per-frame dump (CSV when the path
    /// ends in `.csv`, JSON-lines otherwise); either alone enables profiling.
    /// Deliberately **not** gated on `debug_assertions`: the point of a
    /// profiler is to measure release builds, where frame cost matters.
    pub fn from_env() -> Option<Self> {
        Self::from_diagnostics(&crate::host::environment().diagnostics)
    }

    /// The profiler `diagnostics` asks for, if any: its HUD flag and dump
    /// path correspond to `TILO_PROFILE` and `TILO_PROFILE_OUT`.
    pub fn from_diagnostics(diagnostics: &crate::host::RunnerDiagnostics) -> Option<Self> {
        let hud = diagnostics.profile_hud;
        let out = diagnostics.profile_out.as_deref();
        if !hud && out.is_none() {
            return None;
        }
        let dump = out.and_then(DumpWriter::open);
        Some(Self::new(hud, dump))
    }

    /// No HUD, no dump. Used by the headless harness.
    pub fn for_capture() -> Self {
        let mut p = Self::new(false, None);
        p.capture = Some(Vec::new());
        p
    }

    pub fn hud_enabled(&self) -> bool {
        self.hud
    }

    #[inline]
    pub fn record(&mut self, pass: Pass, dur: Duration) {
        let us = saturating_u64(dur.as_micros());
        match pass {
            Pass::Sync => self.cur.sync_us = self.cur.sync_us.saturating_add(us),
            Pass::BuildLayout => {
                self.cur.build_layout_us = self.cur.build_layout_us.saturating_add(us);
            }
            Pass::ApplyLayout => {
                self.cur.apply_layout_us = self.cur.apply_layout_us.saturating_add(us);
            }
            Pass::Draw => self.cur.draw_us = self.cur.draw_us.saturating_add(us),
            Pass::ResourceAvailability => {
                self.cur.resource_availability_us =
                    self.cur.resource_availability_us.saturating_add(us);
            }
            Pass::ScenePoolWait => {
                self.cur.scene_pool_wait_us = self.cur.scene_pool_wait_us.saturating_add(us);
            }
            Pass::PresenceOverlay => {
                self.cur.presence_overlay_us = self.cur.presence_overlay_us.saturating_add(us);
            }
            // Input can arrive between frames, so it is banked and folded
            // into the next committed frame.
            Pass::HandleEvent => {
                self.pending_handle_event_us = self.pending_handle_event_us.saturating_add(us);
            }
        }
    }

    /// Does nothing when `start` is `None` (profiling was off when the
    /// pass began). Keeps the call sites in `app.rs` / `headless.rs` to
    /// two lines each.
    #[inline]
    pub fn record_since(&mut self, pass: Pass, start: Option<Instant>) {
        if let Some(t) = start {
            self.record(pass, t.elapsed());
        }
    }

    /// `total_us` is the live app's measured wall time.
    pub fn commit_frame(&mut self, total_us: u64) {
        self.cur.handle_event_us = std::mem::take(&mut self.pending_handle_event_us);
        self.cur.frame = self.frame_index;
        self.cur.t_us = saturating_u64(self.origin.elapsed().as_micros());
        self.cur.total_us = total_us;
        self.frame_index = self.frame_index.saturating_add(1);

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
            target: "tilo::profile",
            frame = sample.frame,
            t_us = sample.t_us,
            sync_us = sample.sync_us,
            build_layout_us = sample.build_layout_us,
            apply_layout_us = sample.apply_layout_us,
            draw_us = sample.draw_us,
            handle_event_us = sample.handle_event_us,
            resource_availability_us = sample.resource_availability_us,
            scene_pool_wait_us = sample.scene_pool_wait_us,
            presence_overlay_us = sample.presence_overlay_us,
            total_us = sample.total_us,
            "frame"
        );

        self.cur = FrameTiming::default();
    }

    /// Used by the headless harness, where passes run contiguously so
    /// their sum *is* the frame cost.
    pub fn commit_frame_summed(&mut self) {
        // Fold pending input in before summing, so it is counted.
        let total = self
            .cur
            .pass_sum()
            .saturating_add(self.pending_handle_event_us);
        self.commit_frame(total);
    }

    /// Drains the buffer. Empty when not in capture mode.
    pub fn take_capture(&mut self) -> Vec<FrameTiming> {
        self.capture
            .as_mut()
            .map(std::mem::take)
            .unwrap_or_default()
    }

    /// Discard captured frames (e.g. warmup) without stopping capture.
    pub fn clear_capture(&mut self) {
        if let Some(c) = self.capture.as_mut() {
            c.clear();
        }
    }

    /// Most recent frame last.
    pub fn history(&self) -> &VecDeque<FrameTiming> {
        &self.history
    }

    /// Drawn in the same logical space and `transform` the app draws its
    /// widgets with, so it's crisp at any scale. `viewport` is the logical
    /// `(width, height)`. A top-right panel: a text readout above a rolling
    /// bar graph, with the 16.6ms budget line drawn across it and
    /// over-budget bars painted red.
    pub fn draw_hud(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        viewport: (f64, f64),
    ) {
        let Some(last) = self.history.back().copied() else {
            return;
        };
        let (vw, _vh) = viewport;

        // Logical px, pinned to the top-right corner.
        let pad = 8.0;
        let panel_w = 260.0_f64;
        let graph_h = 56.0_f64;
        let line_h = 14.0_f64;
        let text_rows = 6.0;
        let layout = HudLayout {
            x0: (vw - panel_w - 12.0).max(12.0),
            y0: 12.0_f64,
            pad,
            panel_w,
            panel_h: text_rows * line_h + graph_h + pad * 3.0,
            graph_h,
            line_h,
        };

        let panel = Rect::new(
            layout.x0,
            layout.y0,
            layout.x0 + layout.panel_w,
            layout.y0 + layout.panel_h,
        );
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

        self.draw_hud_text(scene, transform, text, theme, &layout, last);
        self.draw_hud_graph(scene, transform, theme, &layout);
    }

    /// The text readout above the bar graph: current frame, rolling
    /// mean/max/over-budget count, and a per-pass breakdown of the last
    /// frame.
    fn draw_hud_text(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        layout: &HudLayout,
        last: FrameTiming,
    ) {
        let n = self.history.len() as u64;
        let sum: u64 = self.history.iter().map(|f| f.total_us).sum();
        let mean = sum.checked_div(n.max(1)).unwrap_or(0);
        let max = self.history.iter().map(|f| f.total_us).max().unwrap_or(0);
        let over = self.history.iter().filter(|f| f.over_budget()).count();
        let fps = if last.total_us > 0 {
            1_000_000.0 / f64_of_u64(last.total_us)
        } else {
            0.0
        };

        let ms = |us: u64| f64_of_u64(us) / 1000.0;
        let mut ty = layout.y0 + layout.pad + 10.0;
        let tx = layout.x0 + layout.pad;
        // Passed in per call rather than captured, so this closure holds no
        // borrow across calls and the graph below can reuse `scene` freely.
        let draw_line = |scene: &mut Scene,
                         text: &mut TextRenderer,
                         s: &str,
                         color: Color,
                         ty: &mut f64| {
            #[allow(
                clippy::arithmetic_side_effects,
                reason = "Affine composition is a pure geometric transform with no overflow or side effects"
            )]
            let origin = transform * Affine::translate((tx, *ty));
            text.draw_text(
                scene,
                s,
                11.0,
                color,
                origin,
                Some(f32_of_f64(layout.panel_w - layout.pad * 2.0)),
            );
            *ty += layout.line_h;
        };

        let head_color = if last.over_budget() {
            Color::from_rgba8(255, 90, 90, 255)
        } else {
            theme.status_success
        };
        draw_line(
            scene,
            text,
            &format!(
                "frame {}  {:.1}ms  {:.0}fps",
                last.frame,
                ms(last.total_us),
                fps
            ),
            head_color,
            &mut ty,
        );
        draw_line(
            scene,
            text,
            &format!(
                "mean {:.1}ms  max {:.1}ms  over {}/{}",
                ms(mean),
                ms(max),
                over,
                n
            ),
            theme.text_primary,
            &mut ty,
        );
        draw_line(
            scene,
            text,
            &format!(
                "sync {:.2}  build {:.2}",
                ms(last.sync_us),
                ms(last.build_layout_us)
            ),
            theme.text_dim,
            &mut ty,
        );
        draw_line(
            scene,
            text,
            &format!(
                "apply {:.2}  draw {:.2}",
                ms(last.apply_layout_us),
                ms(last.draw_us)
            ),
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
    }

    /// The rolling bar graph: one bar per recent frame, a budget line, and
    /// over-budget bars painted red.
    fn draw_hud_graph(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        layout: &HudLayout,
    ) {
        let gx0 = layout.x0 + layout.pad;
        let gx1 = layout.x0 + layout.panel_w - layout.pad;
        let gy1 = layout.y0 + layout.panel_h - layout.pad;
        let gw = gx1 - gx0;

        // Scale to at least 2x budget (so the budget line sits mid-graph)
        // and the 95th percentile, NOT the raw max: a single cold-start or
        // GC spike would otherwise rescale the graph and flatten every
        // normal frame to an invisible sliver. Frames above the ceiling
        // clip to a full-height bar, already painted red as over-budget.
        let mut totals: Vec<u64> = self.history.iter().map(|f| f.total_us).collect();
        totals.sort_unstable();
        let p95 = percentile(&totals, 95.0);
        let scale_top = f64_of_u64((FRAME_BUDGET_US * 2).max(p95));
        let y_of = |us: u64| gy1 - (f64_of_u64(us) / scale_top).min(1.0) * layout.graph_h;

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

        let shown = self.history.len().min(HISTORY_CAP);
        let bar_w = (gw / f64_of_usize(shown)).max(1.0);
        for (i, f) in self.history.iter().rev().take(shown).enumerate() {
            let bx1 = gx1 - f64_of_usize(i) * bar_w;
            let bx0 = (bx1 - bar_w + 0.5).max(gx0);
            let by0 = y_of(f.total_us);
            let bar = Rect::new(bx0, by0, bx1, gy1);
            let color = if f.over_budget() {
                Color::from_rgba8(230, 70, 70, 255)
            } else {
                theme.tokens.colors.intent.inverted
            };
            scene.fill(Fill::NonZero, transform, color, None, &bar);
        }
    }
}

// ---------------------------------------------------------------------------
// Statistics & report
// ---------------------------------------------------------------------------

/// For one column of measurements (a single pass, or the total), in
/// microseconds.
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
    /// `values` is unsorted; this sorts a copy.
    pub fn of(values: &[u64]) -> Self {
        if values.is_empty() {
            return Self::default();
        }
        let mut v = values.to_vec();
        v.sort_unstable();
        let count = v.len();
        let sum: u128 = v.iter().map(|&x| u128::from(x)).sum();
        let mean_us = saturating_u64(sum.checked_div(count as u128).unwrap_or(0));
        Self {
            count,
            mean_us,
            p50_us: percentile(&v, 50.0),
            p95_us: percentile(&v, 95.0),
            min_us: v.first().copied().unwrap_or(0),
            max_us: v.last().copied().unwrap_or(0),
        }
    }
}

/// Nearest-rank (NIST method): the value at 1-indexed rank
/// `ceil(p/100 * n)`, clamped into range. `sorted` must already be sorted.
fn percentile(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let n = sorted.len();
    let rank = usize_of_f64(((p / 100.0) * f64_of_usize(n)).ceil());
    let idx = rank.saturating_sub(1).min(n.saturating_sub(1));
    sorted.get(idx).copied().unwrap_or(0)
}

/// Per-pass and total statistics over every frame a [`PerfScenario`]
/// produced, plus how many blew the frame budget.
#[derive(Debug, Clone, Serialize)]
pub struct PerfReport {
    pub scenario: String,
    pub frames: usize,
    pub budget_us: u64,
    /// CPU samples above the budget; does not count dropped display frames.
    pub over_budget: usize,
    pub over_budget_pct: f64,
    pub total: StatSummary,
    pub sync: StatSummary,
    pub build_layout: StatSummary,
    pub apply_layout: StatSummary,
    pub draw: StatSummary,
    pub handle_event: StatSummary,
    pub resource_availability: StatSummary,
    pub scene_pool_wait: StatSummary,
    pub presence_overlay: StatSummary,
    /// Skipped when serialising the report itself so a JSON summary stays
    /// small; dump separately if wanted.
    #[serde(skip)]
    pub samples: Vec<FrameTiming>,
}

impl PerfReport {
    pub fn from_samples(scenario: &str, samples: Vec<FrameTiming>, budget_us: u64) -> Self {
        let frames = samples.len();
        let col = |f: fn(&FrameTiming) -> u64| {
            StatSummary::of(&samples.iter().map(f).collect::<Vec<_>>())
        };
        let over_budget = samples.iter().filter(|f| f.total_us > budget_us).count();
        let over_budget_pct = if frames > 0 {
            f64_of_usize(over_budget) * 100.0 / f64_of_usize(frames)
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
            resource_availability: col(|f| f.resource_availability_us),
            scene_pool_wait: col(|f| f.scene_pool_wait_us),
            presence_overlay: col(|f| f.presence_overlay_us),
            samples,
        }
    }

    /// Microseconds shown as milliseconds.
    pub fn to_table(&self) -> String {
        use std::fmt::Write;
        let ms = |us: u64| f64_of_u64(us) / 1000.0;
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
        row(&mut s, "resources", &self.resource_availability);
        row(&mut s, "pool_wait", &self.scene_pool_wait);
        row(&mut s, "presence", &self.presence_overlay);
        row(&mut s, "TOTAL", &self.total);
        s
    }

    /// Without the raw samples.
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

/// Each step drives the headless driver and produces one or more measured
/// frames.
#[derive(Debug, Clone)]
pub enum PerfStep {
    /// No input — measures steady-state redraw cost.
    Frame,
    Frames(usize),
    /// Negative `dy` scrolls the content down. Each notch is one measured
    /// frame.
    Scroll {
        dy: f64,
        count: usize,
    },
    /// One key press/release pair per character.
    Type(String),
    /// `Enter`, `Tab`, arrows, or a character.
    Key(String),
    /// The widget addressed by `test_id` / path.
    ClickNamed(String),
    ClickAt(f64, f64),
}

/// Reusable across any app: build a [`crate::headless::Driver`] over the
/// app's real root + state, then call [`run_scenario`].
#[derive(Debug, Clone)]
pub struct PerfScenario {
    pub name: String,
    pub viewport: (u32, u32),
    /// Frames to run and discard before measuring, so first-frame
    /// allocation / shaping-cache warmup doesn't skew the stats.
    pub warmup: usize,
    pub steps: Vec<PerfStep>,
}

impl PerfScenario {
    /// The most common shape — what surfaces list/grid draw and layout cost.
    pub fn scroll(name: impl Into<String>, count: usize) -> Self {
        Self {
            name: name.into(),
            viewport: crate::headless::DEFAULT_VIEWPORT,
            warmup: 3,
            steps: vec![PerfStep::Scroll { dy: -3.0, count }],
        }
    }

    pub fn typing(name: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            viewport: crate::headless::DEFAULT_VIEWPORT,
            warmup: 3,
            steps: vec![PerfStep::Type(text.into())],
        }
    }

    #[must_use]
    pub fn with_viewport(mut self, w: u32, h: u32) -> Self {
        self.viewport = (w, h);
        self
    }
}

/// The driver runs the app's real five passes with no GPU or display, so
/// timings are of the production code path, not a mock.
///
/// # Panics
///
/// Panics if a named click target cannot be resolved or has an empty rectangle.
/// A failed workload must not return a successful timing report.
#[expect(
    clippy::panic,
    reason = "invalid benchmark workloads must not return timing reports"
)]
pub fn run_scenario(driver: &mut crate::headless::Driver, scenario: &PerfScenario) -> PerfReport {
    driver.begin_profiling();
    for _ in 0..scenario.warmup {
        driver.profile_frame();
    }
    driver.clear_profile_samples();

    for (step_index, step) in scenario.steps.iter().enumerate() {
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
                driver.click_named(name).unwrap_or_else(|error| {
                    panic!(
                        "performance scenario {:?}, step {step_index}: {error}",
                        scenario.name
                    )
                });
            }
            PerfStep::ClickAt(x, y) => driver.click_at(*x, *y),
        }
    }

    let samples = driver.take_profile_samples();
    PerfReport::from_samples(&scenario.name, samples, FRAME_BUDGET_US)
}

/// The one-call entry point for an app author or a test: no driver
/// bookkeeping.
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
    fn stats_summaries_use_nearest_rank_and_handle_empty_input() {
        // Case `percentile_is_nearest_rank`.
        {
            let v: Vec<u64> = (1..=100).collect();
            assert_eq!(percentile(&v, 50.0), 50);
            assert_eq!(percentile(&v, 95.0), 95);
            assert_eq!(percentile(&v, 0.0), 1);
            assert_eq!(percentile(&v, 100.0), 100);
        }

        // Case `stat_summary_captures_spread`.
        {
            let s = StatSummary::of(&[10, 20, 30, 40, 50]);
            assert_eq!(s.count, 5);
            assert_eq!(s.mean_us, 30);
            assert_eq!(s.p50_us, 30);
            assert_eq!(s.min_us, 10);
            assert_eq!(s.max_us, 50);
        }

        // Case `empty_stats_are_zero_not_a_panic`.
        {
            let s = StatSummary::of(&[]);
            assert_eq!(s.count, 0);
            assert_eq!(s.max_us, 0);
        }
    }

    #[test]
    fn profiler_records_passes_commits_and_over_budget_frames() {
        // Case `profiler_records_passes_and_commits`.
        {
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

        // Case `diagnostic_passes_accumulate_serialize_and_reset`.
        {
            let mut profiler = FrameProfiler::for_capture();
            profiler.record(Pass::ResourceAvailability, Duration::from_micros(11));
            profiler.record(Pass::ResourceAvailability, Duration::from_micros(7));
            profiler.record(Pass::ScenePoolWait, Duration::from_micros(23));
            profiler.record(Pass::PresenceOverlay, Duration::from_micros(31));
            profiler.record(Pass::HandleEvent, Duration::from_micros(5));
            profiler.commit_frame_summed();
            profiler.commit_frame_summed();
            let samples = profiler.take_capture();
            assert_eq!(samples[0].total_us, 77);
            assert_eq!(samples[1].total_us, 0);
            let json = serde_json::to_value(samples[0]).expect("serialize frame");
            assert_eq!(json["resource_availability_us"], 18);
            assert_eq!(json["scene_pool_wait_us"], 23);
            assert_eq!(json["presence_overlay_us"], 31);
            let report = PerfReport::from_samples("diagnostic", samples.clone(), FRAME_BUDGET_US);
            assert_eq!(report.resource_availability.max_us, 18);
            assert_eq!(report.scene_pool_wait.max_us, 23);
            assert_eq!(report.presence_overlay.max_us, 31);

            let dir = tempfile::tempdir().expect("temporary profile directory");
            let path = dir.path().join("frames.csv");
            let mut writer = DumpWriter::open(&path).expect("open profile dump");
            writer.write(&samples[0]);
            writer.write(&samples[1]);
            drop(writer);
            let csv = std::fs::read_to_string(path).expect("read profile dump");
            let mut lines = csv.lines();
            let names: Vec<_> = lines.next().expect("header").split(',').collect();
            let values: Vec<_> = lines.next().expect("frame").split(',').collect();
            assert_eq!(names.len(), values.len());
            for (name, value) in names.iter().zip(values) {
                assert_eq!(
                    json[*name].as_u64(),
                    Some(value.parse().expect("CSV number"))
                );
            }
            assert_eq!(lines.count(), 1, "one header, two frames");
        }

        // Case `over_budget_frames_are_counted_in_the_report`.
        {
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

        // Case `scenario_named_click_reaches_the_real_control`.
        {
            let clicks = std::rc::Rc::new(std::cell::Cell::new(0));
            let observed = clicks.clone();
            let root = crate::widget::Button::new("activate", "Activate")
                .on_click(move || observed.set(observed.get() + 1));
            let mut driver =
                crate::headless::Driver::new(crate::State::new(), Box::new(root), (400, 300));
            let scenario = PerfScenario {
                name: "real-click".into(),
                viewport: (400, 300),
                warmup: 1,
                steps: vec![PerfStep::ClickNamed("activate".into()), PerfStep::Frames(2)],
            };
            let report = run_scenario(&mut driver, &scenario);
            assert_eq!(clicks.get(), 1);
            assert!(report.frames >= 2);
        }

        // Case `scenario_cannot_report_success_after_a_missing_click`: the run
        // panics naming the control it could not click (formerly
        // `#[should_panic(expected = "missing-control")]`).
        {
            let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut driver = crate::headless::Driver::new(
                    crate::State::new(),
                    Box::new(crate::widget::Text::new("Root")),
                    (400, 300),
                );
                let scenario = PerfScenario {
                    name: "missing-target".into(),
                    viewport: (400, 300),
                    warmup: 1,
                    steps: vec![
                        PerfStep::ClickNamed("missing-control".into()),
                        PerfStep::Frames(10),
                    ],
                };
                run_scenario(&mut driver, &scenario);
            }))
            .expect_err("a scenario with a missing click must not report success");
            let message = panic
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| panic.downcast_ref::<&str>().copied())
                .unwrap_or_default();
            assert!(
                message.contains("missing-control"),
                "panic message: {message}"
            );
        }
    }
}
